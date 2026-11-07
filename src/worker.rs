//! The TIDAL worker thread: it owns `Client` for the whole run, so every blocking call (device
//! auth polling, search, playback control) happens here and never on the frame.
//!
//! It also owns the *upcoming order* the UI hands it (`Cmd::SetUpcoming`), which is what makes
//! auto-advance independent of the window: a natural end starts the next track right here, and
//! the track after it is preloaded for a gapless hand-off, without waiting for the UI to run a
//! frame. That matters because a covered or hidden Wayland window gets no frame callbacks, so
//! Rune's `poll` — where every queue decision used to live — may not run for minutes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rune::Waker;

use crate::app::{Cmd, Ev};
use crate::audio::{Outcome, OutputOptions, Player, Source, Volume};
use crate::local;
use crate::mpris::MprisTransport;
use crate::playback::{Upcoming, UpcomingQueue, UpcomingSource};
use crate::tidal::{Client, Track, VideoStream};

/// How long the UI may go without running `poll` while a track is playing before the worker
/// says so on stderr (only with `TSUN_TRACE_POLL` set).
const POLL_STALL_MS: u64 = 2000;

/// How many upcoming tracks the worker will try before giving up on auto-advance. A dead
/// network or a library of missing files must not make it work through the whole queue.
const MAX_FAILED_STARTS: usize = 5;

/// A beacon `poll` marks on every frame, so the worker can tell when the window stopped being
/// drawn at all: a covered or hidden Wayland window gets no frame callbacks, and the UI thread
/// then blocks in the compositor's presentation path.
///
/// Created only when `TSUN_TRACE_POLL` is set, so the check costs nothing by default.
pub(crate) struct PollTrace {
    base: Instant,
    last_ms: AtomicU64,
}

impl PollTrace {
    /// `Some` when `TSUN_TRACE_POLL` is set in the environment.
    pub(crate) fn from_env() -> Option<Arc<Self>> {
        std::env::var_os("TSUN_TRACE_POLL")?;
        Some(Arc::new(PollTrace { base: Instant::now(), last_ms: AtomicU64::new(0) }))
    }

    /// Records that the UI thread just ran a frame.
    pub(crate) fn mark(&self) {
        self.last_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    /// Milliseconds since the UI thread last ran `poll`.
    fn stalled_ms(&self) -> u64 {
        self.now_ms().saturating_sub(self.last_ms.load(Ordering::Relaxed))
    }

    /// Whether the UI has gone long enough without a frame to be worth reporting.
    fn stalled(&self) -> bool {
        self.stalled_ms() > POLL_STALL_MS
    }

    fn now_ms(&self) -> u64 {
        self.base.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

/// The audio side the worker owns: the TIDAL client (which holds its own player), the local
/// file player, and the settings that decide how a track is opened.
struct Backend {
    client: Option<Client>,
    local: Option<Player>,
    /// Volume handle for local playback. TIDAL's client keeps its own; `Cmd::SetVolume` drives
    /// both from the UI's single volume control.
    volume: Volume,
    output_device: Option<String>,
    normalize: bool,
}

impl Backend {
    fn new(volume: Volume) -> Self {
        Backend { client: None, local: None, volume, output_device: None, normalize: false }
    }

    /// Stops whatever is playing on either side; a new track supersedes both.
    fn stop_all(&mut self) {
        self.local = None;
        if let Some(client) = self.client.as_mut() {
            client.stop();
        }
    }

    /// Resolves and starts `id` through TIDAL, the way `Cmd::Play` does. Returns the quality
    /// label TIDAL served and the format the device accepted.
    fn play_tidal(&mut self, id: u64) -> Result<(String, Option<String>), String> {
        self.stop_all();
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| "The TIDAL client is not ready.".to_owned())?;
        let quality = client.play(id)?;
        let output = client.output_info().map(|info| info.label());
        Ok((quality, output))
    }

    /// Starts a local file, the way `Cmd::PlayLocal` does. Returns its quality label and the
    /// format the device accepted.
    fn play_file(
        &mut self,
        path: std::path::PathBuf,
        replaygain: Option<f32>,
        quality: String,
    ) -> Result<(String, Option<String>), String> {
        self.stop_all();
        let gain = if self.normalize { replaygain.unwrap_or(1.0) } else { 1.0 };
        self.volume.set_normalization_gain(gain);
        let player = Player::start(
            Source::File(path),
            self.volume.clone(),
            OutputOptions { device: self.output_device.clone() },
        )?;
        let output = player.output_info().map(|info| info.label());
        self.local = Some(player);
        Ok((quality, output))
    }

    /// Starts one upcoming track on the side that owns it.
    fn start(&mut self, entry: &Upcoming) -> Result<(String, Option<String>), String> {
        match &entry.source {
            UpcomingSource::Tidal => self.play_tidal(entry.id()),
            UpcomingSource::Local { path, quality, replaygain } => {
                self.play_file(path.clone(), *replaygain, quality.clone())
            }
        }
    }

    /// Opens `entry` ahead of time so the audio layer can hand over to it without a gap. Best
    /// effort: a preload that fails just means the track starts the ordinary way.
    fn preload(&mut self, entry: &Upcoming) {
        match &entry.source {
            UpcomingSource::Tidal => {
                if let Some(client) = self.client.as_mut() {
                    let _ = client.preload(entry.id());
                }
            }
            UpcomingSource::Local { path, replaygain, .. } => {
                if let Some(player) = self.local.as_ref() {
                    let gain = if self.normalize { replaygain.unwrap_or(1.0) } else { 1.0 };
                    let _ = player.preload(entry.id(), Source::File(path.clone()), gain);
                }
            }
        }
    }

    /// Drops any outstanding preload on either side.
    fn cancel_preloads(&mut self) {
        if let Some(player) = self.local.as_ref() {
            player.cancel_preload();
        }
        if let Some(client) = self.client.as_mut() {
            client.cancel_preload();
        }
    }

    /// A preloaded track that has been swapped in and not yet reported.
    fn take_gapless_switch(&mut self) -> Option<u64> {
        if let Some(player) = self.local.as_ref() {
            return player.take_gapless_switch();
        }
        self.client.as_mut()?.take_gapless_switch()
    }

    /// Reports playback completion or a late failure once.
    fn poll(&mut self) -> Option<Result<Outcome, String>> {
        if self.local.is_some() {
            let result = self.local.as_mut().and_then(Player::poll);
            if result.is_some() {
                self.local = None;
            }
            return result;
        }
        self.client.as_mut()?.poll_playback()
    }

    /// The output time of the track that is playing, in whole seconds.
    fn position_seconds(&self) -> Option<u64> {
        if let Some(player) = self.local.as_ref() {
            return Some(player.position_seconds());
        }
        self.client.as_ref()?.playback_position()
    }

    /// The format the current output stream really runs at, or `None` when nothing reports one.
    fn output_label(&self) -> Option<String> {
        if let Some(player) = self.local.as_ref() {
            return player.output_info().map(|info| info.label());
        }
        self.client.as_ref()?.output_info().map(|info| info.label())
    }

    fn set_paused(&self, paused: bool) -> Result<(), String> {
        if let Some(player) = self.local.as_ref() {
            player.set_paused(paused);
            return Ok(());
        }
        self.client
            .as_ref()
            .ok_or_else(|| "The TIDAL client is not ready.".to_owned())
            .and_then(|client| client.set_paused(paused))
    }

    fn video_stream(&mut self, id: u64) -> Result<VideoStream, String> {
        match self.client.as_mut() {
            Some(client) => client.video_stream(id),
            None => Err("The TIDAL client is not ready.".to_owned()),
        }
    }

    /// The shared volume handle a video should use: TIDAL's when there is a client, a fresh one
    /// otherwise (so it still gets an independent control rather than none).
    fn volume_handle(&self) -> Volume {
        self.client
            .as_ref()
            .map(Client::volume)
            .unwrap_or_else(Volume::new)
    }
}

/// Whether the worker should print its stall report now: once per stall, only while a track is
/// playing, and only when the UI really has stopped running frames.
fn should_report_stall(trace: &PollTrace, playing: bool, already_reported: bool) -> bool {
    playing && trace.stalled() && !already_reported
}

/// The stall report itself, naming the observed stall and the likely cause.
fn stall_message(trace: &PollTrace) -> String {
    format!(
        "tsun: UI poll stalled for {}s; window frames are not being produced (compositor throttling?)",
        trace.stalled_ms() / 1000
    )
}

/// The worker's own state: the audio side, the order it plays on by itself, and the preload
/// armed for a gapless hand-off.
struct Worker {
    backend: Backend,
    /// The tracks a natural end moves to, as the UI last described the queue.
    upcoming: UpcomingQueue,
    /// Whether `upcoming` repeats for ever (repeat-all/one): the worker then starts its own list
    /// over at the end instead of reporting the end of the queue.
    cyclic: bool,
    /// The entry whose preload is armed on the audio layer, if any.
    preloaded: Option<Upcoming>,
    /// The sequence of the track the UI is following; kept across auto-advances, so a late
    /// event for a play the user replaced can still be told apart.
    playing_seq: Option<u64>,
    /// The track actually playing, which auto-advance bookkeeping is anchored on.
    current_id: Option<u64>,
    paused: bool,
    last_position: Option<u64>,
    ev_tx: Sender<Ev>,
    waker: Waker,
    trace: Option<Arc<PollTrace>>,
    /// Whether the current frame-less stretch has already been reported.
    stalled_reported: bool,
}

/// Owns the backend for the whole run. Every blocking call happens here.
pub(crate) fn worker(
    cmd_rx: Receiver<Cmd>,
    ev_tx: Sender<Ev>,
    waker: Waker,
    trace: Option<Arc<PollTrace>>,
) {
    let mut worker = Worker {
        backend: Backend::new(Volume::new()),
        upcoming: UpcomingQueue::default(),
        cyclic: false,
        preloaded: None,
        playing_seq: None,
        current_id: None,
        paused: false,
        last_position: None,
        ev_tx,
        waker,
        trace,
        stalled_reported: false,
    };
    let logged_in = match Client::new() {
        Ok(client) => {
            let logged_in = client.is_logged_in();
            worker.backend.client = Some(client);
            Ok(logged_in)
        }
        Err(error) => Err(error),
    };
    match logged_in {
        Ok(logged_in) => worker.emit(Ev::Ready { logged_in }),
        Err(error) => worker.emit(Ev::InitFailed(error)),
    }
    worker.run(cmd_rx);
}

impl Worker {
    fn emit(&self, ev: Ev) {
        let _ = self.ev_tx.send(ev);
        self.waker.as_ref()();
    }

    /// Hands the worker's upcoming order to the audio layer: opens the track after the current
    /// one ahead of time so it can be swapped in gaplessly, and drops a preload that is no
    /// longer the right track.
    ///
    /// The hand-off happens inside one player, so it only works when both tracks come from the
    /// same side (TIDAL resolves TIDAL ids; a local file needs a local player). Across the
    /// boundary the next track simply starts the ordinary way when its turn comes.
    fn arm_preload(&mut self) {
        let wanted = match (self.current_id, self.upcoming.head()) {
            (Some(current), Some(entry)) if local::is_local_id(current) == entry.is_local() => {
                Some(entry)
            }
            _ => None,
        };
        if wanted.map(Upcoming::id) == self.preloaded.as_ref().map(Upcoming::id) {
            return;
        }
        self.backend.cancel_preloads();
        self.preloaded = None;
        let Some(entry) = wanted else { return };
        self.backend.preload(entry);
        self.preloaded = Some(entry.clone());
    }

    /// Starts the next track on a natural end: the head of the upcoming order, right here, with
    /// no UI frame involved.
    ///
    /// With nothing left to play the finish is reported as before, so the UI advances its own
    /// queue (or stops). Under repeat-all/one the worker's own list starts over instead, so a
    /// window that is never drawn cannot run the queue out. A start that fails is reported and
    /// the following entry is tried, a bounded number of times; when every attempt fails the
    /// error ends playback rather than walking the whole queue.
    fn advance(&mut self, seq: u64, emit: &dyn Fn(Ev)) {
        let mut failures = 0;
        let mut last_error = None;
        while failures < MAX_FAILED_STARTS {
            let Some(entry) = self.next_entry() else {
                self.current_id = None;
                self.paused = false;
                emit(Ev::TrackFinished { seq });
                return;
            };
            match self.backend.start(&entry) {
                Ok((quality, output)) => {
                    self.playing_seq = Some(seq);
                    self.current_id = Some(entry.id());
                    self.paused = false;
                    self.last_position = None;
                    self.preloaded = None;
                    emit(Ev::AutoAdvanced {
                        seq,
                        track: entry.track,
                        quality: Some(quality),
                        output,
                    });
                    self.arm_preload();
                    return;
                }
                Err(error) => {
                    failures += 1;
                    last_error = Some(error.clone());
                    emit(Ev::PlayFailed { seq, error });
                }
            }
        }
        self.playing_seq = None;
        self.current_id = None;
        self.paused = false;
        self.last_position = None;
        self.preloaded = None;
        emit(Ev::PlaybackEnded { seq, error: last_error });
    }

    /// The next track to start: the head of the upcoming list, or — under repeat-all/one — the
    /// same list from the top again, so playback continues without the UI refilling it.
    fn next_entry(&mut self) -> Option<Upcoming> {
        if let Some(entry) = self.upcoming.pop() {
            return Some(entry);
        }
        if !self.cyclic {
            return None;
        }
        self.upcoming.restart();
        self.upcoming.pop()
    }

    /// Follows a gapless hand-off: the audio thread already started the preloaded track on the
    /// same output stream, so only the worker's bookkeeping moves.
    fn hand_over(&mut self, seq: u64, id: u64, emit: &dyn Fn(Ev)) {
        let output = self.backend.output_label();
        // The preloaded entry carries the metadata the UI draws. When the queue changed under
        // it the id alone has to do: the UI prefers its own copy of the queue anyway.
        let track = match self.preloaded.take() {
            Some(entry) if entry.id() == id => entry.track,
            _ => Track { id, ..Track::default() },
        };
        self.current_id = Some(id);
        self.paused = false;
        self.last_position = None;
        self.upcoming.consume(id);
        emit(Ev::AutoAdvanced { seq, track, quality: None, output });
        self.arm_preload();
    }

    /// Says so on stderr when the UI has stopped running frames while a track is playing: that
    /// is a compositor withholding frame callbacks, and it used to stall every queue decision
    /// with them. Only armed by `TSUN_TRACE_POLL`.
    fn report_stall(&mut self) {
        let Some(trace) = self.trace.as_deref() else { return };
        let playing = self.playing_seq.is_some();
        if should_report_stall(trace, playing, self.stalled_reported) {
            self.stalled_reported = true;
            eprintln!("{}", stall_message(trace));
        } else if !playing || !trace.stalled() {
            // The UI is running again (or nothing is playing): the next stall is reported anew.
            self.stalled_reported = false;
        }
    }

    fn run(&mut self, cmd_rx: Receiver<Cmd>) {
        let emit = {
            let ev_tx = self.ev_tx.clone();
            let waker = self.waker.clone();
            move |ev: Ev| {
                let _ = ev_tx.send(ev);
                waker.as_ref()();
            }
        };
        loop {
            let cmd = match cmd_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    self.report_stall();
                    let Some(seq) = self.playing_seq else { continue };
                    // A gapless hand-off means the current player is still playing: report the
                    // new track without tearing anything down.
                    if let Some(id) = self.backend.take_gapless_switch() {
                        self.hand_over(seq, id, &emit);
                        continue;
                    }
                    if let Some(result) = self.backend.poll() {
                        self.playing_seq = None;
                        self.last_position = None;
                        match result {
                            Ok(Outcome::Finished) => self.advance(seq, &emit),
                            Ok(Outcome::Stopped) => emit(Ev::Stopped),
                            Err(error) => {
                                self.current_id = None;
                                self.paused = false;
                                emit(Ev::PlaybackEnded { seq, error: Some(error) });
                            }
                        }
                        continue;
                    }
                    if let Some(elapsed) = self.backend.position_seconds() {
                        if self.last_position != Some(elapsed) {
                            self.last_position = Some(elapsed);
                            emit(Ev::PlaybackProgress { seq, elapsed });
                        }
                    }
                    continue;
                }
            };
            match cmd {
                Cmd::Init => match Client::new() {
                    Ok(new_client) => {
                        let logged_in = new_client.is_logged_in();
                        self.backend.client = Some(new_client);
                        emit(Ev::Ready { logged_in });
                    }
                    Err(error) => emit(Ev::InitFailed(error)),
                },
                Cmd::Login => {
                    let Some(client) = self.backend.client.as_mut() else {
                        emit(Ev::LoginFailed("The TIDAL client is not ready.".to_owned()));
                        continue;
                    };
                    match client.login() {
                        Ok(url) => {
                            emit(Ev::LoginUrl(url));
                            match client.complete_login() {
                                Ok(()) => emit(Ev::LoginDone),
                                Err(error) => emit(Ev::LoginFailed(error)),
                            }
                        }
                        Err(error) => emit(Ev::LoginFailed(error)),
                    }
                }
                Cmd::StartPkce => {
                    let Some(client) = self.backend.client.as_mut() else {
                        emit(Ev::PkceFailed("The TIDAL client is not ready.".to_owned()));
                        continue;
                    };
                    match client.start_pkce() {
                        Ok(url) => emit(Ev::PkceUrl(url)),
                        Err(error) => emit(Ev::PkceFailed(error)),
                    }
                }
                Cmd::FinishPkce { url } => {
                    let Some(client) = self.backend.client.as_mut() else {
                        emit(Ev::PkceFailed("The TIDAL client is not ready.".to_owned()));
                        continue;
                    };
                    match client.finish_pkce(&url) {
                        Ok(()) => emit(Ev::LoginDone),
                        Err(error) => emit(Ev::PkceFailed(error)),
                    }
                }
                Cmd::SearchAll { seq, query } => {
                    let Some(client) = self.backend.client.as_mut() else {
                        emit(Ev::SearchFailed { seq, error: "The TIDAL client is not ready.".to_owned() });
                        continue;
                    };
                    let result = client.search_all(&query);
                    emit(Ev::SearchAllDone { seq, result });
                }
                Cmd::Suggestions { seq, query } => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.search_suggestions(&query),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Suggestions { seq, result });
                }
                Cmd::Explore => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.explore(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::ExploreResult(result));
                }
                Cmd::NewReleases => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.new_releases(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::NewReleases(result));
                }
                Cmd::HomeSections => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.home_sections(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::HomeSections(result));
                }
                Cmd::Lyrics { track_id } => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.lyrics(track_id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Lyrics { track_id, result });
                }
                Cmd::Profile => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.profile(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Profile(result));
                }
                Cmd::Home => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.home_tracks(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::HomeResult(result));
                    let recommendations = match self.backend.client.as_mut() {
                        Some(client) => client.home_recommendations(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::HomeRecommendations(recommendations));
                }
                Cmd::Playlists => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.playlists(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Playlists(result));
                }
                Cmd::Mixes => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.mixes(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Mixes(result));
                }
                Cmd::Library => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.library(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Library(result));
                }
                Cmd::FavoriteIds => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.favorite_ids(),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Favorites(result));
                }
                Cmd::SetFavorite { kind, id, on } => {
                    let error = match self.backend.client.as_mut() {
                        Some(client) => client.set_favorite(kind, &id, on).err(),
                        None => Some("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::FavoriteChanged { kind, id, on, error });
                }
                Cmd::OpenAlbum(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.album(id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Album { id, result });
                }
                Cmd::OpenArtist(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.artist_page(id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Artist { id, result });
                }
                Cmd::ArtistRadio(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.artist_radio(id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::Radio { id, result });
                }
                Cmd::OpenMix(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.mix_tracks(&id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::MixTracks { id, result });
                }
                Cmd::OpenPlaylist(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.playlist_tracks(&id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistTracks { id, result });
                }
                Cmd::PlaylistMeta(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.playlist(&id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistMeta { id, result });
                }
                Cmd::CreatePlaylist { title, description, tracks } => {
                    let Some(client) = self.backend.client.as_mut() else {
                        emit(Ev::PlaylistCreated {
                            result: Err("The TIDAL client is not ready.".to_owned()),
                            added: None,
                        });
                        continue;
                    };
                    match client.create_playlist(&title, &description) {
                        Ok(playlist) => {
                            // A create that came from a menu's "New playlist…" also fills the new
                            // playlist, so the user gets what they asked for in one step.
                            let added = (!tracks.is_empty()).then(|| {
                                client
                                    .add_to_playlist(&playlist.id, &tracks)
                                    .map(|()| tracks.len())
                            });
                            emit(Ev::PlaylistCreated { result: Ok(playlist), added });
                        }
                        Err(error) => emit(Ev::PlaylistCreated { result: Err(error), added: None }),
                    }
                }
                Cmd::AddToPlaylist { id, track_ids } => {
                    let count = track_ids.len();
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.add_to_playlist(&id, &track_ids),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistAdded { id, count, result });
                }
                Cmd::RemoveFromPlaylist { id, index } => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.remove_from_playlist(&id, index),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistRemoved { id, index, result });
                }
                Cmd::MovePlaylistItem { id, from, to } => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.move_playlist_item(&id, from, to),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistMoved { id, from, to, result });
                }
                Cmd::DeletePlaylist(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.delete_playlist(&id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistDeleted { id, result });
                }
                Cmd::RenamePlaylist { id, title, description } => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.rename_playlist(&id, &title, &description),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::PlaylistRenamed { id, title, description, result });
                }
                Cmd::TrackRadio(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.track_radio(id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::TrackRadio(result));
                }
                Cmd::TrackCredits(track_id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => client.track_credits(track_id),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::TrackCredits { track_id, result });
                }
                Cmd::AlbumExtras(id) => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => Ok(client.album_extras(id)),
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::AlbumExtras { id, result });
                }
                Cmd::ArtistTopTracks { id, offset } => {
                    let result = match self.backend.client.as_mut() {
                        Some(client) => {
                            client.artist_top_tracks_all(id, offset, crate::tidal::MAX_TOP_TRACKS)
                        }
                        None => Err("The TIDAL client is not ready.".to_owned()),
                    };
                    emit(Ev::ArtistTopTracks { id, result });
                }
                Cmd::OpenVideo { seq, id } => {
                    let volume = self.backend.volume_handle();
                    let result = self.backend.video_stream(id);
                    emit(Ev::VideoReady { seq, id, result, volume });
                }
                Cmd::PlayLocal { seq, id, path, quality, replaygain } => {
                    // A local play supersedes anything either player was doing, and its tail
                    // comes from the UI (see `Cmd::SetUpcoming`).
                    self.playing_seq = None;
                    self.last_position = None;
                    self.current_id = None;
                    self.paused = false;
                    self.preloaded = None;
                    match self.backend.play_file(path, replaygain, quality) {
                        Ok((quality, output)) => {
                            self.playing_seq = Some(seq);
                            self.current_id = Some(id);
                            self.last_position = None;
                            emit(Ev::Playing { seq, id, quality, output });
                        }
                        Err(error) => emit(Ev::PlayFailed { seq, error }),
                    }
                    self.arm_preload();
                }
                Cmd::Play { seq, id } => {
                    // A local id is never a TIDAL track id; it must never reach the TIDAL play
                    // endpoint (the UI routes local files through `PlayLocal` instead).
                    if local::is_local_id(id) {
                        emit(Ev::PlayFailed {
                            seq,
                            error: "This is a local file, not a TIDAL track.".to_owned(),
                        });
                        continue;
                    }
                    self.playing_seq = None;
                    self.last_position = None;
                    self.current_id = None;
                    self.paused = false;
                    self.preloaded = None;
                    match self.backend.play_tidal(id) {
                        Ok((quality, output)) => {
                            self.playing_seq = Some(seq);
                            self.current_id = Some(id);
                            self.last_position = None;
                            emit(Ev::Playing { seq, id, quality, output });
                        }
                        Err(error) => emit(Ev::PlayFailed { seq, error }),
                    }
                    self.arm_preload();
                }
                Cmd::SetPaused { seq, paused } => {
                    let result = if self.playing_seq == Some(seq) {
                        self.backend.set_paused(paused)
                    } else {
                        Err("The track is no longer playing.".to_owned())
                    };
                    if result.is_ok() {
                        self.paused = paused;
                    }
                    emit(Ev::PauseChanged { seq, paused, error: result.err() });
                }
                Cmd::Seek { seq, seconds } => {
                    if self.playing_seq == Some(seq) {
                        if let Some(player) = self.backend.local.as_ref() {
                            player.seek(seconds);
                        } else if let Some(client) = self.backend.client.as_ref() {
                            client.seek(seconds);
                        }
                    }
                }
                Cmd::Transport(command) => {
                    // A desktop play/pause with no track of its own. Applied to whatever is
                    // playing, so a media key or the tray works while the window is hidden and
                    // no frame is being drawn.
                    let target = match (self.playing_seq, command) {
                        (Some(seq), MprisTransport::Play) => Some((seq, false)),
                        (Some(seq), MprisTransport::Pause) => Some((seq, true)),
                        (Some(seq), MprisTransport::PlayPause) => Some((seq, !self.paused)),
                        (None, _) => None,
                    };
                    let Some((seq, paused)) = target else {
                        // Nothing is playing: starting playback needs the queue, which only
                        // the UI owns.
                        emit(Ev::TransportRequest(command));
                        continue;
                    };
                    let result = self.backend.set_paused(paused);
                    if result.is_ok() {
                        self.paused = paused;
                    }
                    emit(Ev::PauseChanged { seq, paused, error: result.err() });
                }
                Cmd::SetVolume(volume) => {
                    self.backend.volume.set_volume(volume);
                    if let Some(client) = self.backend.client.as_ref() {
                        client.volume().set_volume(volume);
                    }
                }
                Cmd::SetMuted(muted) => {
                    self.backend.volume.set_muted(muted);
                    if let Some(client) = self.backend.client.as_ref() {
                        client.volume().set_muted(muted);
                    }
                }
                Cmd::SetQuality(quality) => {
                    if let Some(client) = self.backend.client.as_mut() {
                        client.set_quality_preference(quality);
                    }
                }
                Cmd::SetNormalize(flag) => {
                    self.backend.normalize = flag;
                    if let Some(client) = self.backend.client.as_mut() {
                        client.set_normalize(flag);
                    }
                }
                Cmd::SetOutputDevice(device) => {
                    self.backend.output_device = device.clone();
                    if let Some(player) = self.backend.local.as_ref() {
                        player.set_output_device(device.clone());
                    }
                    if let Some(client) = self.backend.client.as_mut() {
                        client.set_output_device(device);
                    }
                }
                Cmd::SetUpcoming { after, tracks, cyclic } => {
                    // The order the UI believes follows `after`, which is not necessarily where
                    // the worker is: a stalled UI that is only now catching up resends a list
                    // whose first entries have already played.
                    self.upcoming.set(after, tracks, self.current_id);
                    self.cyclic = cyclic;
                    // `arm_preload` keeps a preload whose track is still the one that comes
                    // next, so an edit further down the queue does not restart it.
                    self.arm_preload();
                }
                Cmd::Stop => {
                    self.playing_seq = None;
                    self.current_id = None;
                    self.paused = false;
                    self.last_position = None;
                    self.preloaded = None;
                    self.cyclic = false;
                    self.upcoming.clear();
                    self.backend.stop_all();
                    emit(Ev::Stopped);
                }
                Cmd::Logout => {
                    self.playing_seq = None;
                    self.current_id = None;
                    self.paused = false;
                    self.last_position = None;
                    self.preloaded = None;
                    self.cyclic = false;
                    self.upcoming.clear();
                    self.backend.stop_all();
                    if let Some(client) = self.backend.client.as_mut() {
                        client.logout();
                    }
                    emit(Ev::Ready { logged_in: false });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::Repeat;
    use crate::tidal::Track;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;

    // ---- the poll-stall diagnostic ---------------------------------------------------------

    #[test]
    fn the_poll_trace_only_reports_a_real_stall() {
        let fresh = PollTrace { base: Instant::now(), last_ms: AtomicU64::new(0) };
        assert!(fresh.stalled_ms() < 1000, "a fresh beacon has not stalled");
        assert!(!fresh.stalled());
        fresh.mark();
        assert!(fresh.stalled_ms() < 1000);

        // A beacon whose base is older than the stall window, with no frame marked on it, reads
        // as stalled — which is what the worker reports while a track is playing.
        let stale =
            PollTrace { base: Instant::now() - Duration::from_secs(5), last_ms: AtomicU64::new(0) };
        assert!(stale.stalled_ms() >= 5_000);
        assert!(stale.stalled());
        assert!(should_report_stall(&stale, true, false), "a stalled UI with audio playing");
        assert!(!should_report_stall(&stale, false, false), "nothing is playing");
        assert!(!should_report_stall(&stale, true, true), "the stall was already reported");
        assert!(!should_report_stall(&fresh, true, false), "the UI is running frames");
        assert!(stall_message(&stale).contains("UI poll stalled for 5s"));
        assert!(stall_message(&stale).contains("compositor"));
        // Running a frame again clears it.
        stale.mark();
        assert!(!stale.stalled());
        assert!(!should_report_stall(&stale, true, false));
    }

    // ---- auto-advance with no UI at all ----------------------------------------------------

    /// A minimal 44.1 kHz 16-bit mono PCM WAV with `seconds` of silence.
    fn write_wav(path: &Path, seconds: u32) {
        let rate: u32 = 44_100;
        let data_len = rate * seconds * 2;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, data_len as usize));
        std::fs::write(path, bytes).unwrap();
    }

    fn track(id: u64, title: &str) -> Track {
        Track {
            id,
            title: title.to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            duration: Some(1),
            ..Track::default()
        }
    }

    fn local_entry(track: Track, path: PathBuf) -> Upcoming {
        Upcoming {
            track,
            source: UpcomingSource::Local {
                path,
                quality: "WAV 16-bit/44.1 kHz".to_owned(),
                replaygain: None,
            },
        }
    }

    /// `XDG_CONFIG_HOME` is process-wide and the worker's client reads the session file from it,
    /// so this test takes the shared turn the settings tests use and points it at a scratch
    /// directory: the user's own session and settings are never read.
    struct ConfigHome {
        previous: Option<std::ffi::OsString>,
        dir: PathBuf,
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigHome {
        fn new(name: &str) -> ConfigHome {
            let turn = crate::settings::lock_config_env();
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("tsun-workercfg-{}-{name}-{unique}", std::process::id()));
            std::fs::create_dir_all(dir.join("tsun")).unwrap();
            std::fs::write(
                dir.join("tsun").join("settings.json"),
                br#"{"volume":0.0,"muted":true,"discord_enabled":false}"#,
            )
            .unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: this test holds the config lock, so no other test reads the variable.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
            ConfigHome { previous, dir, _turn: turn }
        }
    }

    impl Drop for ConfigHome {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The whole point of the design: with **no UI polling at all** — nobody drains events, no
    /// frame runs — the worker plays a, then the upcoming b, then c, each to its natural end,
    /// and reports each advance. Then the UI state machine fed those events afterwards lands on
    /// the right track with the history written once per play.
    ///
    /// Requires `TSUN_AUDIO_TEST=1` and `ALSA_CONFIG_PATH` pointing at a null-device config, so
    /// it stays silent and opt-in like the audio layer's own playback tests:
    ///
    /// ```text
    /// printf 'pcm.!default { type null }\nctl.!default { type null }\n' > /tmp/tsun-alsa.conf
    /// ALSA_CONFIG_PATH=/tmp/tsun-alsa.conf TSUN_AUDIO_TEST=1 \
    ///     cargo test --bin tsun worker::tests::auto_advance -- --nocapture
    /// ```
    #[test]
    fn auto_advance_plays_the_upcoming_queue_with_no_ui_at_all() {
        if std::env::var_os("TSUN_AUDIO_TEST").is_none() {
            eprintln!("skipping worker auto-advance test (set TSUN_AUDIO_TEST=1 with a null ALSA device)");
            return;
        }
        let _config = ConfigHome::new("autoadvance");
        let dir = std::env::temp_dir().join(format!(
            "tsun-worker-autoadvance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let a_path = dir.join("a.wav");
        let b_path = dir.join("b.wav");
        let c_path = dir.join("c.wav");
        for path in [&a_path, &b_path, &c_path] {
            write_wav(path, 1);
        }
        let a = track(local::local_id(&a_path), "A");
        let b = track(local::local_id(&b_path), "B");
        let c = track(local::local_id(&c_path), "C");

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (ev_tx, ev_rx) = mpsc::channel::<Ev>();
        let waker: Waker = Arc::new(|| {});
        let handle = std::thread::spawn(move || worker(cmd_rx, ev_tx, waker, None));
        // Silence is a second guard beside the null device.
        cmd_tx.send(Cmd::SetVolume(0.0)).unwrap();
        cmd_tx.send(Cmd::SetMuted(true)).unwrap();
        cmd_tx
            .send(Cmd::PlayLocal {
                seq: 1,
                id: a.id,
                path: a_path.clone(),
                quality: "WAV 16-bit/44.1 kHz".to_owned(),
                replaygain: None,
            })
            .unwrap();
        cmd_tx
            .send(Cmd::SetUpcoming {
                after: Some(a.id),
                tracks: vec![
                    local_entry(b.clone(), b_path.clone()),
                    local_entry(c.clone(), c_path.clone()),
                ],
                cyclic: false,
            })
            .unwrap();

        // Nobody drains the events for the worker's sake: they pile up in the channel exactly as
        // a frozen UI would leave them, and the queue must still play through.
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut events: Vec<Ev> = Vec::new();
        let finished = |events: &[Ev]| {
            matches!(events.last(), Some(Ev::TrackFinished { .. } | Ev::PlaybackEnded { .. }))
        };
        while !finished(&events) && Instant::now() < deadline {
            while let Ok(ev) = ev_rx.try_recv() {
                events.push(ev);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cmd_tx.send(Cmd::Stop).unwrap();
        // Dropping the sender is what ends the worker's loop; without it `join` waits for ever.
        drop(cmd_tx);
        let _ = handle.join();

        let advanced: Vec<u64> = events
            .iter()
            .filter_map(|ev| match ev {
                Ev::AutoAdvanced { track, .. } => Some(track.id),
                _ => None,
            })
            .collect();
        let names: Vec<&str> = events.iter().map(event_name).collect();
        assert_eq!(advanced, [b.id, c.id], "the worker played b and c by itself: {names:?}");
        assert!(
            matches!(events.first(), Some(Ev::Ready { logged_in: false })),
            "the worker started without a session: {names:?}"
        );
        assert!(
            events.iter().any(|ev| matches!(ev, Ev::Playing { id, .. } if *id == a.id)),
            "the first track was reported as playing: {names:?}"
        );
        assert!(
            !events.iter().any(|ev| matches!(ev, Ev::PlayFailed { .. } | Ev::PlaybackEnded { .. })),
            "no track failed: {names:?}"
        );
        assert!(
            matches!(events.last(), Some(Ev::TrackFinished { seq: 1 })),
            "the end of the queue is reported once the last entry has played: {names:?}"
        );

        // Now the UI catches up on the events it missed, exactly as its `poll` would have.
        let mut ui = crate::app::init();
        // The UI knows the files (as the scanner would), and had started `a` before the window
        // stopped being drawn.
        for path in [&a_path, &b_path, &c_path] {
            let described = local::describe(path).expect("the generated file is readable");
            ui.local_extra.insert(described.id, described);
        }
        ui.play_tracks(vec![a.clone(), b.clone(), c.clone()], 0);
        assert_eq!(ui.pending_play.as_ref().map(|playing| playing.id), Some(a.id));
        for ev in events {
            // The worker has no session in the scratch config, so its `Ready` says logged out.
            // A UI that is playing a queue has necessarily applied a *logged-in* Ready long
            // before (as the real app does at startup); feeding this one would bump the UI's
            // play sequence and — correctly — make every event below stale.
            if !matches!(ev, Ev::Ready { .. }) {
                ui.apply(ev);
            }
        }
        assert_eq!(ui.queue.current_id(), Some(c.id), "the queue followed the worker to c");
        assert_eq!(ui.queue.index(), Some(2), "and its index is c's");
        for id in [a.id, b.id, c.id] {
            let count = ui.history_log.iter().filter(|entry| entry.id == id).count();
            assert_eq!(count, 1, "every play is recorded exactly once");
        }
        assert!(ui.now_playing.is_none(), "the end of the queue stopped playback");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn event_name(ev: &Ev) -> &'static str {
        match ev {
            Ev::Ready { .. } => "Ready",
            Ev::InitFailed(_) => "InitFailed",
            Ev::Playing { .. } => "Playing",
            Ev::PlayFailed { .. } => "PlayFailed",
            Ev::PauseChanged { .. } => "PauseChanged",
            Ev::PlaybackProgress { .. } => "PlaybackProgress",
            Ev::PlaybackEnded { .. } => "PlaybackEnded",
            Ev::TrackFinished { .. } => "TrackFinished",
            Ev::AutoAdvanced { .. } => "AutoAdvanced",
            Ev::TransportRequest(_) => "TransportRequest",
            Ev::Stopped => "Stopped",
            _ => "Other",
        }
    }

    /// The upcoming list is what decides which track plays next, so the worker must take the
    /// UI's list when they agree and drop what it has already played when the UI is behind.
    #[test]
    fn the_upcoming_order_is_reconciled_with_what_already_played() {
        let mut upcoming = UpcomingQueue::default();
        let entry = |id: u64| Upcoming { track: track(id, "t"), source: UpcomingSource::Tidal };
        upcoming.set(Some(1), vec![entry(2), entry(3)], Some(1));
        assert_eq!(upcoming.head().map(Upcoming::id), Some(2), "the UI is where the worker is");
        // The UI is one behind: it still sends 2 first, which has already started.
        upcoming.set(Some(1), vec![entry(2), entry(3), entry(4)], Some(2));
        assert_eq!(upcoming.head().map(Upcoming::id), Some(3));
        upcoming.clear();
        assert!(upcoming.head().is_none());
    }

    /// Repeat modes reach the worker as a plain list, so all the worker has to do is walk it.
    #[test]
    fn repeat_modes_arrive_as_a_plain_list() {
        let mut queue = crate::playback::Queue::new();
        queue.set(vec![track(1, "one"), track(2, "two")], 1);
        assert_eq!(crate::playback::upcoming_ids(&queue, 4), Vec::<u64>::new());
        queue.set_repeat(Repeat::All);
        assert_eq!(crate::playback::upcoming_ids(&queue, 4), [1, 2, 1, 2]);
        queue.set_repeat(Repeat::One);
        assert_eq!(crate::playback::upcoming_ids(&queue, 2), [2, 2]);
    }

    fn idle_worker() -> Worker {
        let (ev_tx, _ev_rx) = mpsc::channel();
        let waker: Waker = Arc::new(|| {});
        Worker {
            backend: Backend::new(Volume::new()),
            upcoming: UpcomingQueue::default(),
            cyclic: false,
            preloaded: None,
            playing_seq: None,
            current_id: None,
            paused: false,
            last_position: None,
            ev_tx,
            waker,
            trace: None,
            stalled_reported: false,
        }
    }

    /// Repeat-all/one keep playing without the UI refilling anything, and repeat-off really
    /// ends at the end of the queue.
    #[test]
    fn a_repeat_mode_repeats_the_workers_own_list() {
        let mut worker = idle_worker();
        let entry = |id: u64| Upcoming { track: track(id, "t"), source: UpcomingSource::Tidal };
        worker.upcoming.set(Some(1), vec![entry(2), entry(3)], Some(1));
        assert_eq!(worker.next_entry().map(|entry| entry.id()), Some(2));
        assert_eq!(worker.next_entry().map(|entry| entry.id()), Some(3));
        assert_eq!(worker.next_entry().map(|entry| entry.id()), None, "repeat off stops");
        worker.cyclic = true;
        assert_eq!(worker.next_entry().map(|entry| entry.id()), Some(2), "the list starts over");
        worker.cyclic = false;
        worker.upcoming.clear();
        worker.cyclic = true;
        assert_eq!(
            worker.next_entry().map(|entry| entry.id()),
            None,
            "an empty list cannot repeat for ever"
        );
    }

    /// Dropping the UI's command channel ends the worker, so it never lingers after a quit.
    #[test]
    fn a_disconnected_command_channel_ends_the_worker() {
        // The worker builds a `Client` on start, which reads the session from the config
        // directory: keep it off the user's own file.
        let _config = ConfigHome::new("disconnect");
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (ev_tx, _ev_rx) = mpsc::channel::<Ev>();
        let waker: Waker = Arc::new(|| {});
        let handle = std::thread::spawn(move || worker(cmd_rx, ev_tx, waker, None));
        drop(cmd_tx);
        handle.join().expect("the worker exits when the UI is gone");
    }
}
