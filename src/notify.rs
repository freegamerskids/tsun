//! Desktop notifications on track change, via the freedesktop
//! `org.freedesktop.Notifications` D-Bus service (Linux only; elsewhere [`Notifier::start`]
//! reports that notifications are unavailable).
//!
//! One background thread owns a blocking session-bus connection, so posting a notification never
//! touches a UI frame; a notification *replaces* the previous one (`replaces_id`), so a run of
//! tracks leaves a single bubble rather than a stack. The cover art is handed over as an
//! `image-path` hint pointing at the cover cached by [`crate::cover::cover_file`]; when the cover
//! is unavailable or the download fails, the notification is posted without an icon rather than
//! holding up the track change.
//!
//! The feature is off unless the user enables it in Settings, and a missing session bus simply
//! disables it: [`Notifier::start`] returns `Err` and the caller carries on without desktop
//! notifications.

#[cfg(target_os = "linux")]
mod dbus;
#[cfg(not(target_os = "linux"))]
mod unsupported;

#[cfg(target_os = "linux")]
pub(crate) use dbus::Notifier;
#[cfg(not(target_os = "linux"))]
pub(crate) use unsupported::Notifier;

/// One track-change notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notification {
    /// The track title, shown in bold as the summary line.
    pub(crate) title: String,
    /// `"artist — album"`, or just the one that is known.
    pub(crate) body: String,
    /// The raw TIDAL cover UUID, or `None` with no cover art.
    pub(crate) cover: Option<String>,
}
