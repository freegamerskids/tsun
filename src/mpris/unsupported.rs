//! Platforms without a D-Bus session: there is no MPRIS service to publish.

use std::sync::mpsc::Sender;

use super::{MprisCommand, MprisMeta, MprisPlayback, MprisTransport};

/// Uninhabited: [`Mpris::start`] always fails here, so no handle ever exists and the methods
/// below can never run. They only keep the callers free of platform conditionals.
pub enum Mpris {}

impl Mpris {
    pub fn start(_command_tx: Sender<MprisCommand>) -> Result<Mpris, String> {
        Err("MPRIS needs a D-Bus session bus, which this platform does not have".to_owned())
    }

    pub fn set_transport_sink(&self, _tx: Sender<MprisTransport>) {
        match *self {}
    }

    pub fn set_playback(&self, _playback: MprisPlayback) {
        match *self {}
    }

    pub fn set_metadata(&self, _metadata: MprisMeta) {
        match *self {}
    }

    pub fn set_position(&self, _position_us: i64) {
        match *self {}
    }

    pub fn set_volume(&self, _volume: f64) {
        match *self {}
    }

    pub fn set_can_go(&self, _next: bool, _previous: bool) {
        match *self {}
    }

    pub fn emit_seeked(&self, _position_us: i64) {
        match *self {}
    }
}
