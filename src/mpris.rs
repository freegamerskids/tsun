//! MPRIS2 (`org.mpris.MediaPlayer2`) D-Bus service for the tsun TIDAL client.
//!
//! This lets desktop media keys, panels and widgets (GNOME/KDE media controls,
//! `playerctl`, …) see the player and drive it. It exports two interfaces at
//! `/org/mpris/MediaPlayer2`:
//!
//! * `org.mpris.MediaPlayer2` — identity and quit/raise,
//! * `org.mpris.MediaPlayer2.Player` — playback status, metadata, position,
//!   volume, capabilities, transport methods and `PropertiesChanged`.
//!
//! # Usage
//!
//! The application creates a channel, hands the [`Sender`] to [`Mpris::start`],
//! and keeps the returned [`Mpris`] handle for the lifetime of the process:
//!
//! ```text
//! use std::sync::mpsc::channel;
//! use tsun::mpris::{Mpris, MprisCommand};
//!
//! let (tx, rx) = channel::<MprisCommand>();
//! let mpris = match Mpris::start(tx) {
//!     Ok(mpris) => mpris,
//!     Err(error) => { eprintln!("MPRIS disabled: {error}"); return; }
//! };
//! mpris.set_can_go(true, true);
//! // …then, from wherever playback state changes:
//! mpris.set_metadata(Default::default());
//! // …and pump `rx` from the UI loop to act on media-key commands.
//! ```
//!
//! [`Mpris::start`] runs the D-Bus plumbing on its own thread (zbus's internal
//! executor); the handle's setters may be called from any thread. When no
//! session bus is available `start` returns `Err` and the application should
//! simply carry on without MPRIS. Dropping the handle releases the bus name.
//!
//! # The play/pause fast path
//!
//! By default every method call (play, pause, next, seek, …) is delivered as an
//! [`MprisCommand`] on the channel passed to [`Mpris::start`], so the
//! application applies it when its loop next runs. That loop can be stalled for
//! a long time — a covered or hidden Wayland window gets no frame callbacks —
//! so [`Mpris::set_transport_sink`] lets play/pause bypass it and reach the
//! player directly. Next/previous still need the play queue, and stop, seek and
//! volume still need the application's own state, so those keep using the
//! channel.
//!
//! # Position
//!
//! MPRIS defines `Position` as read-only and excludes it from
//! `PropertiesChanged`, so [`Mpris::set_position`] only updates the value
//! clients read; use [`Mpris::emit_seeked`] after a deliberate jump so clients
//! can resynchronise.

/// A request from the desktop, delivered on the channel passed to
/// [`Mpris::start`]. Off Linux only the tray sends any, so most variants go unbuilt there.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum MprisCommand {
    /// Start or resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Toggle between playing and paused.
    PlayPause,
    /// Stop playback.
    Stop,
    /// Advance to the next track in the queue.
    Next,
    /// Return to the previous track in the queue.
    Previous,
    /// Seek to an absolute position, in microseconds from the track start.
    SeekTo(i64),
    /// Seek by a relative offset in microseconds (negative seeks backwards).
    SeekBy(i64),
    /// The desktop asked for this volume, in `0.0..=1.0`.
    SetVolume(f64),
    /// Quit the application.
    Quit,
}

/// Playback status as MPRIS (and the desktop) understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MprisPlayback {
    Playing,
    Paused,
    #[default]
    Stopped,
}

/// A play/pause request that can go straight to the player thread, without waiting for the
/// application's UI loop to run a frame: a window that is hidden or covered by a Wayland
/// compositor gets no frame callbacks, and meta keys must still work then.
///
/// Everything that needs the play queue (next/previous) and everything that changes the UI's
/// own state (seek, volume, stop, quit) still goes through [`MprisCommand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum MprisTransport {
    Play,
    Pause,
    PlayPause,
}

/// Track metadata reported to the desktop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MprisMeta {
    /// Track title (`xesam:title`).
    pub title: String,
    /// Primary artist (`xesam:artist`, reported as an array of one).
    pub artist: String,
    /// Album title (`xesam:album`).
    pub album: String,
    /// Cover art URL (`mpris:artUrl`).
    pub art_url: Option<String>,
    /// Track length in microseconds (`mpris:length`); zero omits it.
    pub length_us: u64,
    /// Stable track identity (`mpris:trackid`); sanitised into an object path.
    pub track_id: String,
}

#[cfg(target_os = "linux")]
mod dbus;
#[cfg(not(target_os = "linux"))]
mod unsupported;

#[cfg(target_os = "linux")]
pub use dbus::Mpris;
#[cfg(not(target_os = "linux"))]
pub use unsupported::Mpris;
