//! Playback events shared by the optional integrations that follow what is playing (Last.fm,
//! Discord).
//!
//! Everything is emitted from the *app-level* now-playing lifecycle in [`crate::app`] — where
//! `NowPlaying` is set or cleared, pause/resume, seek, the worker's own auto-advance (gapless or
//! not), a natural end, a stop and signing out — so a TIDAL track and a local file produce the
//! same events and no integration has to know where the audio came from. Each integration keeps
//! a channel to its own worker thread, so [`Ui::emit_media`] only ever hands the event over and
//! never blocks the frame.

use serde::{Deserialize, Serialize};

use crate::app::{Notice, NowPlaying, Ui};
use crate::media_hub::MediaNote;

/// One playable item, in the form the integrations need it. `id` is a stable identity: TIDAL's
/// numeric track id as text, or a local file's path hash. `cover_url` is an https TIDAL image
/// URL when one is known (local files usually have none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MediaTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: Option<u64>,
    pub cover_url: Option<String>,
    /// True for a local file rather than a TIDAL stream.
    pub local: bool,
}

/// A change in the playback lifecycle. Positions are the track's own elapsed seconds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MediaEvent {
    /// A track started (from the queue, a manual pick, the worker's own advance or a repeat).
    Started { track: MediaTrack, position_secs: f64 },
    Paused { position_secs: f64 },
    Resumed { position_secs: f64 },
    Seeked { position_secs: f64 },
    /// Playback stopped (the user stopped, signed out, or it failed).
    Stopped,
    /// The track reached its natural end.
    Finished,
}

impl MediaTrack {
    /// The track the app is playing, as an integration sees it. A local file is recognised by
    /// its id's top bit (see [`crate::local`]), so it is reported as `local` with its own
    /// identity; everything else is a TIDAL track with TIDAL's numeric id.
    pub(crate) fn from_now_playing(playing: &NowPlaying) -> Self {
        MediaTrack {
            id: playing.id.to_string(),
            title: playing.title.clone(),
            artist: playing.artist.clone(),
            album: playing.album.clone(),
            duration_secs: playing.duration,
            cover_url: playing.cover.clone(),
            local: crate::local::is_local_id(playing.id),
        }
    }

    /// The same, from the queue track the worker starts (the metadata the hub publishes never
    /// depends on the UI having applied anything).
    pub(crate) fn from_track(track: &crate::tidal::Track) -> Self {
        MediaTrack {
            id: track.id.to_string(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_secs: track.duration,
            cover_url: track.cover.clone(),
            local: crate::local::is_local_id(track.id),
        }
    }
}

impl Ui {
    /// The UI's side of the media fan-out, for a test that drives the hub itself: the app's own
    /// events come from the worker (see [`crate::worker`]), which is what makes them independent
    /// of the window being drawn.
    ///
    /// It installs the same sinks `setup` does and drains the notes `poll` would, so a test can
    /// check what an integration would see.
    #[cfg(test)]
    pub(crate) fn emit_media(&mut self, event: MediaEvent) {
        self.media.set_lastfm(self.lastfm.as_ref().map(crate::lastfm::LastFm::sink));
        self.media.set_discord(self.discord.as_ref().map(crate::discord::Discord::sink));
        self.media.dispatch_now(event);
        self.poll_media();
    }

    /// Applies anything the integrations want the user to see. Called from `poll`, so a note
    /// raised while no frame was running is shown as soon as the window is drawn again.
    pub(crate) fn poll_media(&mut self) -> bool {
        let mut changed = false;
        for note in self.media.take_notes() {
            match note {
                MediaNote::NoArtistTag { title } => {
                    // A track Last.fm cannot identify is skipped, and the user is told why
                    // instead of silently losing the scrobble.
                    self.notice = Some(Notice::info(format!(
                        "Not scrobbling “{title}”: it has no artist tag"
                    )));
                    changed = true;
                }
            }
        }
        changed
    }
}
