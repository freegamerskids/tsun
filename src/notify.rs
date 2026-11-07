//! Desktop notifications on track change, via the freedesktop
//! `org.freedesktop.Notifications` D-Bus service.
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

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::thread;

use zbus::zvariant::Value;

/// The service implementing the notification spec.
const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
/// What the desktop attributes the bubble to.
const APP_NAME: &str = "tsun";

/// How long a track-change notification stays up, in milliseconds.
const EXPIRE_MS: i32 = 4000;

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

/// Posts track-change notifications from a background thread.
pub(crate) struct Notifier {
    tx: Sender<Notification>,
}

impl Notifier {
    /// Connects to the session bus and starts the notification thread.
    ///
    /// Returns `Err` when no session bus is reachable, so the caller can run without desktop
    /// notifications.
    pub(crate) fn start() -> Result<Notifier, String> {
        let connection = zbus::blocking::Connection::session()
            .map_err(|error| format!("no session bus: {error}"))?;
        let proxy = zbus::blocking::Proxy::new(&connection, SERVICE, PATH, INTERFACE)
            .map_err(|error| format!("could not reach {SERVICE}: {error}"))?;
        let (tx, rx) = mpsc::channel::<Notification>();
        thread::Builder::new()
            .name("tsun-notify".to_owned())
            .spawn(move || {
                // The id of the notification currently on screen, so the next one replaces it.
                let mut replaces: u32 = 0;
                while let Ok(notification) = rx.recv() {
                    if let Some(id) = post(&proxy, &notification, replaces) {
                        replaces = id;
                    }
                }
                // The connection is released when the thread exits, i.e. when the `Notifier` is
                // dropped along with its sender.
            })
            .map_err(|error| format!("could not start the notification thread: {error}"))?;
        Ok(Notifier { tx })
    }

    /// Posts `notification`, replacing whatever the desktop is still showing. Best-effort: a
    /// failure is dropped, never surfaced to the frame.
    pub(crate) fn notify(&self, notification: Notification) {
        let _ = self.tx.send(notification);
    }
}

/// Sends one `Notify` call, returning the id the desktop assigned.
fn post(
    proxy: &zbus::blocking::Proxy<'_>,
    notification: &Notification,
    replaces_id: u32,
) -> Option<u32> {
    let image = notification
        .cover
        .as_deref()
        .and_then(crate::cover::cover_file);
    let hints = hints(image);
    proxy
        .call::<_, _, u32>(
            "Notify",
            &(
                APP_NAME,
                replaces_id,
                "", // application icon name; the app is not installed with one
                notification.title.as_str(),
                notification.body.as_str(),
                Vec::<&str>::new(), // no actions: clicking opens nothing
                hints,
                EXPIRE_MS,
            ),
        )
        .ok()
}

/// The `a{sv}` hints dictionary: the cover file (when there is one) and the desktop entry that
/// attributes the bubble to this application.
fn hints(image: Option<std::path::PathBuf>) -> HashMap<String, Value<'static>> {
    let mut hints: HashMap<String, Value<'static>> = HashMap::new();
    hints.insert("desktop-entry".to_owned(), Value::from(APP_NAME.to_owned()));
    if let Some(path) = image {
        if let Some(path) = path.to_str() {
            hints.insert("image-path".to_owned(), Value::from(path.to_owned()));
        }
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_always_carry_the_desktop_entry() {
        let hints = hints(None);
        assert_eq!(
            hints.get("desktop-entry").and_then(|value| value.downcast_ref::<&str>().ok()),
            Some(APP_NAME)
        );
        assert!(!hints.contains_key("image-path"));
    }

    #[test]
    fn hints_point_at_the_cover_when_there_is_one() {
        let hints = hints(Some(std::path::PathBuf::from("/tmp/cover.jpg")));
        assert_eq!(
            hints.get("image-path").and_then(|value| value.downcast_ref::<&str>().ok()),
            Some("/tmp/cover.jpg")
        );
    }
}
