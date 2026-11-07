//! The fan-out every integration that follows playback listens to.
//!
//! The thread that owns the audio (the worker) reports what is playing here, and this hub runs
//! its own relay thread that drives Last.fm, Discord, MPRIS and desktop notifications from it.
//! That is what keeps them in step when the window is not being drawn at all: a covered or
//! hidden Wayland window gets no frame callbacks, so Rune's `poll` does not run, and nothing
//! driven from a frame can update — the presence would keep showing the previous track until
//! the window came back.
//!
//! The UI still *configures* the integrations (it owns the Discord client, the Last.fm card and
//! the notification switch) and hands their event sinks over with
//! [`MediaHub::set_discord`]/[`MediaHub::set_lastfm`]/[`MediaHub::set_mpris`]/
//! [`MediaHub::set_notifier`]; it also pushes the few things only it sees (the user's volume, a
//! video taking the transport over). Anything the integrations want the *user* to see comes back
//! through [`MediaHub::take_notes`], which `poll` drains.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use rune::Waker;

use crate::art;
use crate::discord::DiscordSink;
use crate::lastfm::LastFmSink;
use crate::media_events::{MediaEvent, MediaTrack};
use crate::mpris::{Mpris, MprisMeta, MprisPlayback};
use crate::notify::{Notification, Notifier};

/// Whether the presence follows the desktop, one line per track started: the shape of a
/// notification body ("artist — album"), or whichever half is known.
fn notification_body(track: &MediaTrack) -> String {
    match (track.artist.is_empty(), track.album.is_empty()) {
        (false, false) => format!("{} — {}", track.artist, track.album),
        (false, true) => track.artist.clone(),
        (true, false) => track.album.clone(),
        (true, true) => String::new(),
    }
}

/// The MPRIS metadata for a track: the same fields the UI used to publish before the hub owned
/// this, including the artwork the desktop reads.
fn metadata_of(track: &MediaTrack) -> MprisMeta {
    MprisMeta {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        art_url: track.cover_url.as_deref().and_then(|cover| art::art_url(cover, 320)),
        length_us: track.duration_secs.unwrap_or(0) * 1_000_000,
        track_id: track.id.clone(),
    }
}

/// A line the hub wants the UI to show the user. The UI drains these in `poll`, so a notice
/// raised while the window was frozen appears as soon as it is drawn again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaNote {
    /// A track Last.fm cannot scrobble was skipped: it has no artist tag.
    NoArtistTag { title: String },
}

/// What the hub publishes to MPRIS.
///
/// A trait rather than [`Mpris`] directly so a test can record the calls without a session bus.
pub(crate) trait MprisSink: Send + Sync {
    fn set_metadata(&self, metadata: MprisMeta);
    fn set_playback(&self, playback: MprisPlayback);
    fn set_can_go(&self, next: bool, previous: bool);
    fn set_volume(&self, volume: f64);
    fn set_position(&self, position_us: i64);
    fn emit_seeked(&self, position_us: i64);
}

impl MprisSink for Mpris {
    fn set_metadata(&self, metadata: MprisMeta) {
        Mpris::set_metadata(self, metadata);
    }

    fn set_playback(&self, playback: MprisPlayback) {
        Mpris::set_playback(self, playback);
    }

    fn set_can_go(&self, next: bool, previous: bool) {
        Mpris::set_can_go(self, next, previous);
    }

    fn set_volume(&self, volume: f64) {
        Mpris::set_volume(self, volume);
    }

    fn set_position(&self, position_us: i64) {
        Mpris::set_position(self, position_us);
    }

    fn emit_seeked(&self, position_us: i64) {
        Mpris::emit_seeked(self, position_us);
    }
}

/// What the worker (and the UI, for the few things only it sees) sends the hub.
enum Message {
    /// A playback lifecycle change, fanned out to every integration.
    Media(MediaEvent),
    /// Where the playhead is now, for MPRIS's `Position` (read on demand by the desktop).
    Position(f64),
}

/// What the hub published last, so it only sends what changed: MPRIS metadata (a D-Bus signal)
/// and volume are not repeated for every progress tick.
#[derive(Default)]
struct Published {
    /// The track the metadata describes, as `(id, cover)`.
    track: Option<(String, Option<String>)>,
    volume: Option<f64>,
    /// Whether a video owns the transport: music events then leave MPRIS alone, because the UI
    /// publishes the video's own state through the same service.
    video: bool,
}

/// The hub itself. Clone the [`Arc`] into the worker; the UI keeps its own for configuration.
pub(crate) struct MediaHub {
    tx: Sender<Message>,
    /// Taken by [`MediaHub::start`], which owns the receiving end from then on.
    rx: Mutex<Option<Receiver<Message>>>,
    waker: Mutex<Option<Waker>>,
    notes: Mutex<Vec<MediaNote>>,
    discord: Mutex<Option<DiscordSink>>,
    lastfm: Mutex<Option<LastFmSink>>,
    mpris: Mutex<Option<Arc<dyn MprisSink>>>,
    notifier: Mutex<Option<Notifier>>,
    published: Mutex<Published>,
}

impl MediaHub {
    /// A hub with no relay thread yet: the app builds one in `init`, the worker's test in its
    /// own setup, and both call [`MediaHub::start`] to give it the thread that drains what the
    /// audio side sends.
    pub(crate) fn new() -> Arc<MediaHub> {
        let (tx, rx) = mpsc::channel();
        Arc::new(MediaHub {
            tx,
            rx: Mutex::new(Some(rx)),
            waker: Mutex::new(None),
            notes: Mutex::new(Vec::new()),
            discord: Mutex::new(None),
            lastfm: Mutex::new(None),
            mpris: Mutex::new(None),
            notifier: Mutex::new(None),
            published: Mutex::new(Published::default()),
        })
    }

    /// Spawns the relay thread (once; a second call is a no-op) and remembers `waker`, so a
    /// notice raised while no frame is running wakes the app to show it.
    pub(crate) fn start(self: &Arc<Self>, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
        let Some(rx) = self.rx.lock().unwrap().take() else { return };
        let hub = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("tsun-media-hub".to_owned())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    hub.dispatch(message);
                }
            });
    }

    /// Reports a playback lifecycle change. Never blocks and never touches D-Bus here, so the
    /// worker can call it from the thread that owns the audio.
    pub(crate) fn media(&self, event: MediaEvent) {
        let _ = self.tx.send(Message::Media(event));
    }

    /// Folds one event in on the calling thread.
    ///
    /// What the relay thread does with what [`MediaHub::media`] queued; a test with no relay
    /// thread calls it directly so the fan-out is deterministic.
    #[cfg(test)]
    pub(crate) fn dispatch_now(&self, event: MediaEvent) {
        self.dispatch(Message::Media(event));
    }

    /// Reports where the playhead is, for MPRIS's `Position`.
    pub(crate) fn position(&self, seconds: f64) {
        let _ = self.tx.send(Message::Position(seconds));
    }

    /// Installs the Discord client's event sink, or retires it when the presence is switched off.
    pub(crate) fn set_discord(&self, sink: Option<DiscordSink>) {
        *self.discord.lock().unwrap() = sink;
    }

    /// The sink the Last.fm client's playback events go to.
    pub(crate) fn set_lastfm(&self, sink: Option<LastFmSink>) {
        *self.lastfm.lock().unwrap() = sink;
    }

    /// The MPRIS service the hub publishes to.
    pub(crate) fn set_mpris(&self, sink: Option<Arc<dyn MprisSink>>) {
        *self.mpris.lock().unwrap() = sink;
    }

    /// The notifier track changes go to, when desktop notifications are on.
    pub(crate) fn set_notifier(&self, notifier: Option<Notifier>) {
        *self.notifier.lock().unwrap() = notifier;
    }

    /// The user's volume, as MPRIS reports it (zero while muted). Sent only on a change.
    pub(crate) fn set_volume(&self, volume: f64) {
        let mut published = self.published.lock().unwrap();
        if published.volume == Some(volume) {
            return;
        }
        published.volume = Some(volume);
        drop(published);
        if let Some(sink) = self.mpris.lock().unwrap().as_ref() {
            sink.set_volume(volume);
        }
    }

    /// Whether the transport can move, for MPRIS's `CanGoNext`/`CanGoPrevious`.
    pub(crate) fn set_can_go(&self, next: bool, previous: bool) {
        if let Some(sink) = self.mpris.lock().unwrap().as_ref() {
            sink.set_can_go(next, previous);
        }
    }

    /// A video took the transport over (`true`), or gave it back (`false`): the UI publishes the
    /// video's own MPRIS state in the meantime.
    pub(crate) fn set_video(&self, video: bool) {
        {
            let mut published = self.published.lock().unwrap();
            published.video = video;
            if video {
                // The metadata the video publishes is not this hub's to compare against.
                published.track = None;
            }
        }
        if !video {
            // Handing the transport back to music: nothing is playing yet, so report that.
            self.publish_stopped();
        }
    }

    /// Anything the integrations want the user to see, newest last. `poll` drains this.
    pub(crate) fn take_notes(&self) -> Vec<MediaNote> {
        std::mem::take(&mut *self.notes.lock().unwrap())
    }

    fn dispatch(&self, message: Message) {
        let event = match message {
            Message::Media(event) => event,
            Message::Position(seconds) => {
                if let Some(sink) = self.mpris.lock().unwrap().as_ref() {
                    sink.set_position((seconds * 1_000_000.0) as i64);
                }
                return;
            }
        };
        self.publish_mpris(&event);
        self.note(&event);
        if let Some(discord) = self.discord.lock().unwrap().as_ref() {
            discord.media_event(&event);
        }
        if let Some(lastfm) = self.lastfm.lock().unwrap().as_ref() {
            lastfm.handle(event.clone());
        }
        if let MediaEvent::Started { track, .. } = &event {
            self.notify(track);
        }
    }

    /// Keeps MPRIS's metadata, status and position in step with playback. A video's own state
    /// is published by the UI through the same service, so music leaves it alone while one is up.
    fn publish_mpris(&self, event: &MediaEvent) {
        let mut published = self.published.lock().unwrap();
        if published.video {
            return;
        }
        let Some(sink) = self.mpris.lock().unwrap().clone() else { return };
        match event {
            MediaEvent::Started { track, position_secs } => {
                let key = Some((track.id.clone(), track.cover_url.clone()));
                if published.track != key {
                    published.track = key;
                    sink.set_metadata(metadata_of(track));
                }
                sink.set_playback(MprisPlayback::Playing);
                sink.set_position((position_secs * 1_000_000.0) as i64);
            }
            MediaEvent::Paused { position_secs } => {
                sink.set_playback(MprisPlayback::Paused);
                sink.set_position((position_secs * 1_000_000.0) as i64);
            }
            MediaEvent::Resumed { position_secs } => {
                sink.set_playback(MprisPlayback::Playing);
                sink.set_position((position_secs * 1_000_000.0) as i64);
            }
            MediaEvent::Seeked { position_secs } => {
                let position_us = (position_secs * 1_000_000.0) as i64;
                sink.set_position(position_us);
                sink.emit_seeked(position_us);
            }
            MediaEvent::Stopped | MediaEvent::Finished => {
                if published.track.take().is_some() {
                    sink.set_metadata(MprisMeta::default());
                }
                sink.set_playback(MprisPlayback::Stopped);
                sink.set_position(0);
            }
        }
    }

    /// The "nothing is playing" MPRIS state, for handing the transport back from a video.
    fn publish_stopped(&self) {
        let mut published = self.published.lock().unwrap();
        let Some(sink) = self.mpris.lock().unwrap().clone() else { return };
        if published.track.take().is_some() {
            sink.set_metadata(MprisMeta::default());
        }
        sink.set_playback(MprisPlayback::Stopped);
        sink.set_position(0);
    }

    /// Raises the note the UI shows for a track Last.fm cannot scrobble.
    fn note(&self, event: &MediaEvent) {
        let MediaEvent::Started { track, .. } = event else { return };
        if !track.artist.trim().is_empty() {
            return;
        }
        let lastfm_enabled = self
            .lastfm
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(LastFmSink::enabled);
        if !lastfm_enabled {
            return;
        }
        self.notes.lock().unwrap().push(MediaNote::NoArtistTag { title: track.title.clone() });
        if let Some(waker) = self.waker.lock().unwrap().as_ref() {
            waker();
        }
    }

    /// Posts the desktop notification for a track that just started.
    fn notify(&self, track: &MediaTrack) {
        let notifier = self.notifier.lock().unwrap();
        let Some(notifier) = notifier.as_ref() else { return };
        notifier.notify(Notification {
            title: track.title.clone(),
            body: notification_body(track),
            cover: track.cover_url.clone(),
        });
    }
}
