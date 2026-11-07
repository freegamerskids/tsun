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

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{ObjectPath, Value};

/// The well-known bus name this service owns.
pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.tsun";
/// The MPRIS object path shared by both interfaces.
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

/// A request from the desktop, delivered on the channel passed to
/// [`Mpris::start`].
#[derive(Debug, Clone, PartialEq)]
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
pub enum MprisTransport {
    Play,
    Pause,
    PlayPause,
}

/// The fast-path request a command maps to, if any.
fn transport_for(command: &MprisCommand) -> Option<MprisTransport> {
    match command {
        MprisCommand::Play => Some(MprisTransport::Play),
        MprisCommand::Pause => Some(MprisTransport::Pause),
        MprisCommand::PlayPause => Some(MprisTransport::PlayPause),
        _ => None,
    }
}

impl MprisPlayback {
    fn as_str(self) -> &'static str {
        match self {
            MprisPlayback::Playing => "Playing",
            MprisPlayback::Paused => "Paused",
            MprisPlayback::Stopped => "Stopped",
        }
    }
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

/// The current, shared state of the exported interfaces.
#[derive(Default)]
struct Props {
    playback: MprisPlayback,
    metadata: MprisMeta,
    position_us: i64,
    volume: f64,
    can_go_next: bool,
    can_go_previous: bool,
}

/// State shared between the exported interfaces and the [`Mpris`] handle.
struct Shared {
    props: Mutex<Props>,
    /// Wrapped in a `Mutex` so the handle (and `Shared`) can be `Sync`.
    command_tx: Mutex<Sender<MprisCommand>>,
    /// When set, play/pause is delivered here instead of through `command_tx`, so a media key
    /// acts on the player even while the application's UI loop is stalled with its window.
    transport_tx: Mutex<Option<Sender<MprisTransport>>>,
}

impl Shared {
    fn new(command_tx: Sender<MprisCommand>) -> Self {
        let mut props = Props::default();
        props.volume = 1.0;
        Shared {
            props: Mutex::new(props),
            command_tx: Mutex::new(command_tx),
            transport_tx: Mutex::new(None),
        }
    }

    fn send(&self, command: MprisCommand) {
        if let Some(transport) = transport_for(&command) {
            if let Ok(mut slot) = self.transport_tx.lock() {
                if let Some(tx) = slot.as_mut() {
                    let _ = tx.send(transport);
                    return;
                }
            }
        }
        if let Ok(tx) = self.command_tx.lock() {
            let _ = tx.send(command);
        }
    }

    /// Installs the fast path play/pause takes; without one every command rides `command_tx`.
    fn set_transport(&self, tx: Sender<MprisTransport>) {
        if let Ok(mut slot) = self.transport_tx.lock() {
            *slot = Some(tx);
        }
    }

    fn with_props<R>(&self, action: impl FnOnce(&mut Props) -> R) -> Option<R> {
        self.props.lock().ok().map(|mut props| action(&mut props))
    }
}

/// Turns a track id into a valid MPRIS object path.
fn track_path(track_id: &str) -> String {
    let mut sanitized: String = track_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized.chars().all(|character| character == '_') {
        sanitized = "unknown".to_string();
    }
    format!("{OBJECT_PATH}/Track/{sanitized}")
}

/// Builds the `a{sv}` metadata dictionary MPRIS clients read.
fn metadata_map(meta: &MprisMeta) -> HashMap<String, Value<'static>> {
    let mut map = HashMap::new();
    if let Ok(path) = ObjectPath::try_from(track_path(&meta.track_id)) {
        map.insert("mpris:trackid".to_string(), Value::from(path));
    }
    if meta.length_us > 0 {
        map.insert(
            "mpris:length".to_string(),
            Value::from(meta.length_us.min(i64::MAX as u64) as i64),
        );
    }
    if let Some(art_url) = &meta.art_url {
        if !art_url.is_empty() {
            map.insert("mpris:artUrl".to_string(), Value::from(art_url.clone()));
        }
    }
    if !meta.title.is_empty() {
        map.insert("xesam:title".to_string(), Value::from(meta.title.clone()));
    }
    if !meta.artist.is_empty() {
        map.insert(
            "xesam:artist".to_string(),
            Value::from(vec![meta.artist.clone()]),
        );
    }
    if !meta.album.is_empty() {
        map.insert("xesam:album".to_string(), Value::from(meta.album.clone()));
    }
    map
}

/// The `org.mpris.MediaPlayer2` interface.
struct RootInterface {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl RootInterface {
    #[zbus(property)]
    fn identity(&self) -> String {
        "tsun".to_string()
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        "tsun".to_string()
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn fullscreen(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_set_fullscreen(&self) -> bool {
        false
    }

    fn quit(&self) {
        self.shared.send(MprisCommand::Quit);
    }
}

/// The `org.mpris.MediaPlayer2.Player` interface.
struct PlayerInterface {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerInterface {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.shared
            .with_props(|props| props.playback.as_str().to_string())
            .unwrap_or_else(|| MprisPlayback::Stopped.as_str().to_string())
    }

    #[zbus(property)]
    fn loop_status(&self) -> String {
        "None".to_string()
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, Value<'static>> {
        self.shared
            .with_props(|props| metadata_map(&props.metadata))
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.shared.with_props(|props| props.volume).unwrap_or(1.0)
    }

    #[zbus(property)]
    fn set_volume(&self, volume: f64) {
        let volume = volume.clamp(0.0, 1.0);
        self.shared.with_props(|props| props.volume = volume);
        self.shared.send(MprisCommand::SetVolume(volume));
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        self.shared.with_props(|props| props.position_us).unwrap_or(0)
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.shared.with_props(|props| props.can_go_next).unwrap_or(false)
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.shared
            .with_props(|props| props.can_go_previous)
            .unwrap_or(false)
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }

    fn play(&self) {
        self.shared.send(MprisCommand::Play);
    }

    fn pause(&self) {
        self.shared.send(MprisCommand::Pause);
    }

    fn play_pause(&self) {
        self.shared.send(MprisCommand::PlayPause);
    }

    fn stop(&self) {
        self.shared.send(MprisCommand::Stop);
    }

    fn next(&self) {
        self.shared.send(MprisCommand::Next);
    }

    fn previous(&self) {
        self.shared.send(MprisCommand::Previous);
    }

    fn seek(&self, offset: i64) {
        self.shared.send(MprisCommand::SeekBy(offset));
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        // The caller supplies the track id it believes is playing; MPRIS says a
        // mismatch must be ignored, but clients routinely pass a stale id, so we
        // accept the position and leave validation to the application.
        let _ = track_id;
        self.shared
            .with_props(|props| props.position_us = position.max(0));
        self.shared.send(MprisCommand::SeekTo(position.max(0)));
    }
}

/// A running MPRIS service.
///
/// Setters are cheap, non-blocking and callable from any thread. Dropping the
/// handle releases [`BUS_NAME`]; the D-Bus plumbing shuts down with it.
pub struct Mpris {
    shared: Arc<Shared>,
    connection: zbus::blocking::Connection,
}

impl Mpris {
    /// Connects to the session bus, exports both interfaces and owns
    /// [`BUS_NAME`].
    ///
    /// Returns `Err` (with an explanation) when there is no session bus or the
    /// name is already taken; callers should treat that as "MPRIS unavailable"
    /// and continue without it.
    pub fn start(command_tx: Sender<MprisCommand>) -> Result<Mpris, String> {
        let shared = Arc::new(Shared::new(command_tx));
        let connection = zbus::blocking::connection::Builder::session()
            .map_err(|error| format!("no D-Bus session bus is available: {error}"))?
            .name(BUS_NAME)
            .map_err(|error| format!("invalid MPRIS bus name {BUS_NAME}: {error}"))?
            .serve_at(
                OBJECT_PATH,
                RootInterface {
                    shared: shared.clone(),
                },
            )
            .map_err(|error| format!("could not export {ROOT_INTERFACE}: {error}"))?
            .serve_at(
                OBJECT_PATH,
                PlayerInterface {
                    shared: shared.clone(),
                },
            )
            .map_err(|error| format!("could not export {PLAYER_INTERFACE}: {error}"))?
            .build()
            .map_err(|error| format!("could not register {BUS_NAME} on the session bus: {error}"))?;
        Ok(Mpris { shared, connection })
    }

    /// Routes play/pause straight to the player thread instead of the command channel, so a
    /// media key works even when the application's window is not being drawn (and its poll loop
    /// is therefore not running). Without it every command rides the channel passed to
    /// [`Mpris::start`].
    pub fn set_transport_sink(&self, tx: Sender<MprisTransport>) {
        self.shared.set_transport(tx);
    }

    /// Reports a new playback status and signals the change.
    pub fn set_playback(&self, playback: MprisPlayback) {
        let changed = self.shared.with_props(|props| {
            if props.playback == playback {
                return false;
            }
            props.playback = playback;
            true
        });
        if changed != Some(true) {
            return;
        }
        let mut properties = HashMap::new();
        properties.insert(
            "PlaybackStatus".to_string(),
            Value::from(playback.as_str()),
        );
        self.emit_player_properties(properties);
    }

    /// Reports new track metadata and signals the change.
    pub fn set_metadata(&self, metadata: MprisMeta) {
        let map = metadata_map(&metadata);
        self.shared.with_props(|props| props.metadata = metadata);
        let mut properties = HashMap::new();
        properties.insert("Metadata".to_string(), Value::from(map));
        self.emit_player_properties(properties);
    }

    /// Updates the reported position, in microseconds.
    ///
    /// `Position` is excluded from `PropertiesChanged` by MPRIS, so no signal is
    /// emitted; see [`Mpris::emit_seeked`].
    pub fn set_position(&self, position_us: i64) {
        self.shared
            .with_props(|props| props.position_us = position_us.max(0));
    }

    /// Reports the player's own volume (`0.0..=1.0`) and signals the change.
    pub fn set_volume(&self, volume: f64) {
        let volume = volume.clamp(0.0, 1.0);
        self.shared.with_props(|props| props.volume = volume);
        let mut properties = HashMap::new();
        properties.insert("Volume".to_string(), Value::from(volume));
        self.emit_player_properties(properties);
    }

    /// Reports whether next/previous are available and signals the change.
    pub fn set_can_go(&self, next: bool, previous: bool) {
        let changed = self.shared.with_props(|props| {
            if props.can_go_next == next && props.can_go_previous == previous {
                return false;
            }
            props.can_go_next = next;
            props.can_go_previous = previous;
            true
        });
        if changed != Some(true) {
            return;
        }
        let mut properties = HashMap::new();
        properties.insert("CanGoNext".to_string(), Value::from(next));
        properties.insert("CanGoPrevious".to_string(), Value::from(previous));
        self.emit_player_properties(properties);
    }

    /// Emits the `Seeked` signal after a discontinuous position change.
    ///
    /// Clients use it to resynchronise their own position estimate; also call
    /// [`Mpris::set_position`] so `Position` reads back the new value.
    pub fn emit_seeked(&self, position_us: i64) {
        let _ = self.connection.emit_signal(
            None::<()>,
            OBJECT_PATH,
            PLAYER_INTERFACE,
            "Seeked",
            &(position_us.max(0),),
        );
    }

    fn emit_player_properties(&self, changed: HashMap<String, Value<'static>>) {
        // A client disconnect or a closed bus must not take the app down; the
        // player simply carries on.
        let _ = self.connection.emit_signal(
            None::<()>,
            OBJECT_PATH,
            PROPERTIES_INTERFACE,
            "PropertiesChanged",
            &(PLAYER_INTERFACE, changed, Vec::<String>::new()),
        );
    }
}

impl Drop for Mpris {
    fn drop(&mut self) {
        // Release the well-known name so another instance can take over, then
        // let the connection close.
        let _ = self.connection.release_name(BUS_NAME);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_dictionary_matches_the_mpris_schema() {
        let meta = MprisMeta {
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            art_url: Some("https://example/art.jpg".to_string()),
            length_us: 213_000_000,
            track_id: "42".to_string(),
        };
        let map = metadata_map(&meta);

        let track_id = map.get("mpris:trackid").expect("track id");
        assert!(matches!(track_id, Value::ObjectPath(_)));
        assert_eq!(
            track_id.downcast_ref::<ObjectPath>().unwrap().as_str(),
            "/org/mpris/MediaPlayer2/Track/42"
        );

        assert_eq!(
            map.get("mpris:length").and_then(|value| value.downcast_ref::<i64>().ok()),
            Some(213_000_000)
        );
        assert_eq!(
            map.get("mpris:artUrl").and_then(|value| value.downcast_ref::<&str>().ok()),
            Some("https://example/art.jpg")
        );
        assert_eq!(
            map.get("xesam:title").and_then(|value| value.downcast_ref::<&str>().ok()),
            Some("Track")
        );
        assert_eq!(
            map.get("xesam:album").and_then(|value| value.downcast_ref::<&str>().ok()),
            Some("Album")
        );
        // `xesam:artist` is an array of strings per the MPRIS schema.
        let artists = map.get("xesam:artist").expect("artist");
        assert_eq!(artists.value_signature().to_string(), "as");
        match artists {
            Value::Array(array) => {
                let names: Vec<&str> = array
                    .iter()
                    .map(|value| value.downcast_ref::<&str>().unwrap())
                    .collect();
                assert_eq!(names, ["Artist"]);
            }
            other => panic!("xesam:artist is not an array: {other:?}"),
        }
    }

    #[test]
    fn metadata_omits_unknown_fields() {
        let map = metadata_map(&MprisMeta {
            track_id: "  7  ".to_string(),
            ..Default::default()
        });
        assert!(!map.contains_key("mpris:length"));
        assert!(!map.contains_key("mpris:artUrl"));
        assert!(!map.contains_key("xesam:title"));
        assert!(!map.contains_key("xesam:artist"));
        assert!(!map.contains_key("xesam:album"));
        // A track id with awkward characters still yields a valid object path.
        assert!(map.contains_key("mpris:trackid"));
    }

    #[test]
    fn track_path_sanitises_and_never_is_empty() {
        assert_eq!(track_path("1234"), "/org/mpris/MediaPlayer2/Track/1234");
        assert_eq!(
            track_path("a/b c"),
            "/org/mpris/MediaPlayer2/Track/a_b_c"
        );
        assert_eq!(track_path(""), "/org/mpris/MediaPlayer2/Track/unknown");
        assert!(ObjectPath::try_from(track_path("")).is_ok());
        assert!(ObjectPath::try_from(track_path("a/b c")).is_ok());
    }

    #[test]
    fn handle_and_commands_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Mpris>();
        assert_send_sync::<MprisCommand>();
        assert_send_sync::<MprisMeta>();
        assert_send_sync::<MprisPlayback>();
    }

    #[test]
    fn state_updates_without_a_bus() {
        // The shared state and command channel work independently of D-Bus, so
        // the interface accessors can be exercised without a session bus.
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared::new(tx));
        let player = PlayerInterface {
            shared: shared.clone(),
        };
        assert_eq!(player.playback_status(), "Stopped");
        assert_eq!(player.volume(), 1.0);
        let metadata = player.metadata();
        assert_eq!(metadata.len(), 1);
        assert!(metadata.contains_key("mpris:trackid"));
        assert!(!player.can_go_next());

        player.play_pause();
        player.seek(-1_000_000);
        player.set_position(ObjectPath::try_from(track_path("9")).unwrap(), 5_000_000);
        player.set_volume(0.4);
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            MprisCommand::PlayPause
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            MprisCommand::SeekBy(-1_000_000)
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            MprisCommand::SeekTo(5_000_000)
        );
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            MprisCommand::SetVolume(0.4)
        );
        assert_eq!(player.volume(), 0.4);
        assert_eq!(player.position(), 5_000_000);

        let root = RootInterface {
            shared: shared.clone(),
        };
        root.quit();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            MprisCommand::Quit
        );
    }

    // -----------------------------------------------------------------------------------------
    // Session-bus smoke test (ignored by default; needs a live `--user` bus).

    #[test]
    #[ignore = "requires a D-Bus session bus"]
    fn service_round_trips_over_the_session_bus() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mpris = Mpris::start(tx).expect("a session bus");
        mpris.set_playback(MprisPlayback::Playing);
        mpris.set_metadata(MprisMeta {
            title: "Smoke".to_string(),
            artist: "Tester".to_string(),
            album: "Suite".to_string(),
            art_url: Some("https://example/art.jpg".to_string()),
            length_us: 180_000_000,
            track_id: "123".to_string(),
        });
        mpris.set_can_go(true, true);
        mpris.set_position(7_000_000);

        let connection = zbus::blocking::Connection::session().expect("a session bus");
        let player = zbus::blocking::Proxy::new(
            &connection,
            BUS_NAME,
            OBJECT_PATH,
            PLAYER_INTERFACE,
        )
        .expect("proxy");

        let status: String = player.get_property("PlaybackStatus").unwrap();
        assert_eq!(status, "Playing");
        let volume: f64 = player.get_property("Volume").unwrap();
        assert_eq!(volume, 1.0);
        let position: i64 = player.get_property("Position").unwrap();
        assert_eq!(position, 7_000_000);
        assert!(player.get_property::<bool>("CanGoNext").unwrap());
        assert!(player.get_property::<bool>("CanGoPrevious").unwrap());

        let metadata: HashMap<String, Value<'static>> =
            player.get_property("Metadata").unwrap();
        assert_eq!(
            metadata
                .get("xesam:title")
                .and_then(|value| value.downcast_ref::<&str>().ok()),
            Some("Smoke")
        );
        assert_eq!(
            metadata
                .get("mpris:length")
                .and_then(|value| value.downcast_ref::<i64>().ok()),
            Some(180_000_000)
        );

        player.call::<_, _, ()>("PlayPause", &()).unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            MprisCommand::PlayPause
        );
        player.call::<_, _, ()>("Next", &()).unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            MprisCommand::Next
        );
        player
            .call::<_, _, ()>(
                "SetPosition",
                &(
                    ObjectPath::try_from(track_path("123")).unwrap(),
                    9_000_000i64,
                ),
            )
            .unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            MprisCommand::SeekTo(9_000_000)
        );
        player.set_property("Volume", 0.25f64).unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            MprisCommand::SetVolume(0.25)
        );

        drop(mpris);
    }
}
