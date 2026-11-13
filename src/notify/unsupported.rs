//! Platforms without the freedesktop notification service.

use super::Notification;

/// Uninhabited: [`Notifier::start`] always fails here, so no handle ever exists. It only keeps
/// the callers free of platform conditionals.
#[derive(Clone)]
pub(crate) enum Notifier {}

impl Notifier {
    pub(crate) fn start() -> Result<Notifier, String> {
        Err("track notifications are only implemented for Linux desktops".to_owned())
    }

    pub(crate) fn notify(&self, _notification: Notification) {
        match *self {}
    }
}
