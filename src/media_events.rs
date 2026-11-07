//! Playback events shared by the optional integrations that follow what is playing (Last.fm,
//! Discord).
//!
//! Everything is emitted from the *app-level* now-playing lifecycle in [`crate::app`] — where
//! `NowPlaying` is set or cleared, pause/resume, seek, the gapless hand-off, a natural end, a
//! stop and signing out — so a TIDAL track and a local file produce the same events and no
//! integration has to know where the audio came from. Each integration keeps a channel to its
//! own worker thread, so [`Ui::emit_media`] only ever hands the event over and never blocks the
//! frame.

use serde::{Deserialize, Serialize};

use crate::app::{Notice, NowPlaying, Ui};

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
    /// A track started (from the queue, a manual pick, a gapless hand-off or a repeat).
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
}

impl Ui {
    /// Fans one playback event out to every integration that is running. Cheap and
    /// non-blocking: each integration forwards it to its own thread.
    pub(crate) fn emit_media(&mut self, event: MediaEvent) {
        // A track Last.fm cannot identify (a local file with no artist tag) is skipped, and the
        // user is told why instead of silently losing the scrobble.
        let unlabelled = match (&self.lastfm, &event) {
            (Some(lastfm), MediaEvent::Started { track, .. })
                if lastfm.enabled() && track.artist.trim().is_empty() =>
            {
                Some(track.title.clone())
            }
            _ => None,
        };
        if let Some(title) = unlabelled {
            self.notice = Some(Notice::info(format!(
                "Not scrobbling “{title}”: it has no artist tag"
            )));
        }
        if let Some(lastfm) = &self.lastfm {
            lastfm.handle(event.clone());
        }
        // Discord: the presence follows the same events, on its own thread.
        if let Some(discord) = &self.discord {
            discord.media_event(&event);
        }
    }

    /// Emits [`MediaEvent::Started`] for the track that just began playing.
    pub(crate) fn media_started(&mut self, position_secs: f64) {
        let Some(playing) = self.now_playing.as_ref() else { return };
        let track = MediaTrack::from_now_playing(playing);
        self.emit_media(MediaEvent::Started { track, position_secs });
    }

    /// Emits `Paused`/`Resumed` for a transport state change.
    pub(crate) fn media_pause_changed(&mut self, paused: bool, position_secs: f64) {
        if paused {
            self.emit_media(MediaEvent::Paused { position_secs });
        } else {
            self.emit_media(MediaEvent::Resumed { position_secs });
        }
    }

    /// Emits [`MediaEvent::Seeked`] at `position_secs`.
    pub(crate) fn media_seeked(&mut self, position_secs: f64) {
        self.emit_media(MediaEvent::Seeked { position_secs });
    }

    /// Emits [`MediaEvent::Stopped`].
    pub(crate) fn media_stopped(&mut self) {
        self.emit_media(MediaEvent::Stopped);
    }

    /// Emits [`MediaEvent::Finished`] (the track reached its natural end).
    pub(crate) fn media_finished(&mut self) {
        self.emit_media(MediaEvent::Finished);
    }
}
