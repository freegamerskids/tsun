//! Discord Rich Presence over Discord's local IPC socket.
//!
//! A running Discord desktop client listens on a Unix socket — `$XDG_RUNTIME_DIR/discord-ipc-N`,
//! under `$XDG_RUNTIME_DIR/app/com.discordapp.Discord/` in the Flatpak build, under
//! `$XDG_RUNTIME_DIR/snap.discord/` in the Snap, with `/tmp/discord-ipc-N` as the historic
//! fallback — and takes frames of `u32` little-endian opcode, `u32` little-endian payload
//! length and a JSON payload. After a handshake (op `0`,
//! `{"v":1,"client_id":"…"}`) it answers `READY` and accepts `SET_ACTIVITY` frames (op `1`) that
//! set or clear this process's presence.
//!
//! tsun cannot ship a Discord application of its own, so the user creates one at
//! <https://discord.com/developers/applications> and pastes its Application ID into Settings.
//! Discord shows "Listening to <the application's name>", which is why the settings card
//! recommends naming it "TIDAL".
//!
//! Everything runs on one thread of its own so a missing, slow or restarting Discord never
//! blocks a frame: the UI sends [`Command`]s over a channel and reads the shared [`Status`]. A
//! broken pipe reconnects with a backoff and republishes the current presence; editions closer
//! together than [`MIN_INTERVAL`] are coalesced to the latest (Discord allows about five
//! `SET_ACTIVITY` frames per twenty seconds); dropping the client clears the presence.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::media_events::{MediaEvent, MediaTrack};

/// Frame opcodes of the Discord IPC protocol.
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

/// Discord's `ActivityType::Listening`.
const ACTIVITY_LISTENING: u64 = 2;

/// Discord refuses text fields longer than this, and anything shorter than two characters.
const MAX_TEXT_CHARS: usize = 128;
/// The same limit, for the activity name field the Settings card caps its input at.
pub(crate) const MAX_ACTIVITY_NAME_CHARS: usize = MAX_TEXT_CHARS;
/// A Rich Presence button's URL may be at most this long (and must be `https`).
const MAX_BUTTON_URL_CHARS: usize = 512;
/// A zero-width space pads a one-character field to Discord's two-character minimum without
/// showing anything.
const PAD: char = '\u{200b}';

/// How long one `read` on the socket may block before the loop looks at its command channel.
const READ_TIMEOUT: Duration = Duration::from_millis(20);
/// How long the loop waits for a command when there is nothing to send. Short, so dropping the
/// client (which joins this thread) is prompt.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// How long Discord has to answer the handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// The first reconnect delay; it doubles up to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// The distance kept between two `SET_ACTIVITY` frames (Discord allows ~5 per 20 s).
pub(crate) const MIN_INTERVAL: Duration = Duration::from_secs(4);

/// A socket frame's payload may not exceed this; anything larger is a broken stream.
const MAX_FRAME: usize = 1 << 20;

/// Woken when the status changes, so the Settings card repaints without polling. This is
/// Rune's `Waker` (`Arc<dyn Fn() + Send + Sync>`).
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// The user's Discord application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    /// The Application ID from the Discord developer portal (17–20 digits).
    pub(crate) client_id: String,
    /// What to show after "Listening to", overriding the application's own name; empty uses the
    /// application's name, which is what Discord shows by default.
    pub(crate) name: String,
    /// Whether the album art (the TIDAL cover URL) is shown as the activity's large image.
    pub(crate) show_art: bool,
    /// Whether start/end timestamps are sent, so Discord draws the elapsed bar.
    pub(crate) show_time: bool,
    /// Whether a "Listen on TIDAL" button is offered for TIDAL tracks.
    pub(crate) show_button: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            client_id: String::new(),
            name: String::new(),
            show_art: true,
            show_time: true,
            show_button: true,
        }
    }
}

/// Discord Application IDs are 17 to 20 digits.
pub(crate) fn valid_client_id(id: &str) -> bool {
    let id = id.trim();
    (17..=20).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// The `serde` default for the on-by-default Discord display switches.
pub(crate) fn default_true() -> bool {
    true
}

/// What the Settings card shows about the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// No Discord socket could be found: Discord is closed, or not installed.
    NotRunning,
    /// A socket was found and the handshake is in flight.
    Connecting,
    /// Handshaken; Discord reported this account.
    Connected(String),
    /// The socket exists but refused us (bad Application ID, Discord shutting down, …).
    Error(String),
}

impl Status {
    /// The one-line label the Settings card shows.
    pub(crate) fn label(&self) -> String {
        match self {
            Status::NotRunning => "Not running".to_owned(),
            Status::Connecting => "Connecting…".to_owned(),
            Status::Connected(user) => format!("Connected as {user}"),
            Status::Error(message) => format!("Error: {message}"),
        }
    }

    /// Whether a presence can be published right now.
    pub(crate) fn is_connected(&self) -> bool {
        matches!(self, Status::Connected(_))
    }
}

/// Exactly what to publish: the track's metadata and where playback is in it. Paused playbacks
/// carry no timestamps, because Discord has no paused flag — see [`activity_json`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Presence {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    /// A public `https` cover URL (Discord fetches it for the activity's large image).
    pub(crate) cover_url: Option<String>,
    /// The "Listen on TIDAL" target; `None` for local files.
    pub(crate) url: Option<String>,
    pub(crate) duration_secs: Option<u64>,
    pub(crate) position_secs: f64,
    pub(crate) paused: bool,
}

impl Presence {
    /// The same playback moved to `position_secs`, paused or not.
    fn at(&self, position_secs: f64, paused: bool) -> Presence {
        Presence { position_secs, paused, ..self.clone() }
    }
}

enum Command {
    /// Publish this presence, or clear it with `None`.
    Set(Option<Presence>),
    /// The user changed one of the display switches.
    Configure(Config),
    Shutdown,
}

type SharedStatus = Arc<Mutex<Status>>;

/// A running Discord client. Dropping it stops the thread and clears the presence.
pub(crate) struct Discord {
    tx: Sender<Command>,
    status: SharedStatus,
    /// The presence last handed to [`Discord::show`] / [`Discord::update`], so a paused or
    /// seeked event can be folded into it.
    current: Mutex<Option<Presence>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Discord {
    /// Connects through the environment's runtime dirs, as Discord itself does.
    pub(crate) fn start(config: Config, wake: Option<Wake>) -> Discord {
        Discord::start_with(config, wake, None, MIN_INTERVAL)
    }

    /// `runtime` overrides the socket search entirely (one directory, as a test's fake Discord
    /// needs); `min_interval` is the `SET_ACTIVITY` spacing.
    pub(crate) fn start_with(
        config: Config,
        wake: Option<Wake>,
        runtime: Option<PathBuf>,
        min_interval: Duration,
    ) -> Discord {
        let (tx, rx) = mpsc::channel();
        let status: SharedStatus = Arc::new(Mutex::new(Status::Connecting));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread = {
            let status = Arc::clone(&status);
            let shutdown = Arc::clone(&shutdown);
            thread::Builder::new()
                .name("discord-presence".to_owned())
                .spawn(move || run(config, rx, status, wake, runtime, min_interval, shutdown))
                .ok()
        };
        Discord { tx, status, current: Mutex::new(None), shutdown, thread }
    }

    /// Publishes `presence` (a new track, or the same one after a change).
    pub(crate) fn show(&self, presence: Presence) {
        *self.current.lock().unwrap() = Some(presence.clone());
        self.send(Command::Set(Some(presence)));
    }

    /// Moves the current presence: a seek, a pause or a resume. Ignored while nothing plays.
    pub(crate) fn update(&self, position_secs: f64, paused: bool) {
        let mut current = self.current.lock().unwrap();
        let Some(next) = current.as_ref().map(|presence| presence.at(position_secs, paused)) else {
            return;
        };
        *current = Some(next.clone());
        drop(current);
        self.send(Command::Set(Some(next)));
    }

    /// Clears the presence (stopped, finished, signed out).
    pub(crate) fn clear(&self) {
        *self.current.lock().unwrap() = None;
        self.send(Command::Set(None));
    }

    /// Moves the current presence after a seek, keeping however the transport was.
    pub(crate) fn seek(&self, position_secs: f64) {
        let paused = self.current.lock().unwrap().as_ref().is_some_and(|presence| presence.paused);
        self.update(position_secs, paused);
    }

    /// Republishes the presence for the track playing right now. A client started with a
    /// different Application ID is a new Discord session, and Discord dropped what the old one
    /// had published, so the current track has to go out again.
    pub(crate) fn republish(&self, track: &MediaTrack, position_secs: f64, paused: bool) {
        self.show(presence_of(track, position_secs, paused));
    }

    /// Follows one playback event from the app's now-playing lifecycle (see
    /// [`Ui::emit_media`](crate::app::Ui::emit_media)). Cheap: it folds the event into the
    /// presence and hands it to the thread, which coalesces and rate-limits the frames.
    pub(crate) fn media_event(&self, event: &MediaEvent) {
        match event {
            MediaEvent::Started { track, position_secs } => {
                self.show(presence_of(track, *position_secs, false));
            }
            MediaEvent::Paused { position_secs } => self.update(*position_secs, true),
            MediaEvent::Resumed { position_secs } => self.update(*position_secs, false),
            // A seek keeps the transport state: a seek while paused must not restart the bar.
            MediaEvent::Seeked { position_secs } => self.seek(*position_secs),
            MediaEvent::Stopped | MediaEvent::Finished => self.clear(),
        }
    }

    /// Applies changed display switches without reconnecting.
    pub(crate) fn configure(&self, config: Config) {
        self.send(Command::Configure(config));
    }

    /// The connection state, for the Settings card.
    pub(crate) fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

impl Drop for Discord {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// -------------------------------------------------------------------------------------------
// Frames

/// `u32` opcode, `u32` payload length, payload — little-endian, as Discord frames are.
fn encode_frame(op: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// What one non-blocking look at a socket turned up.
enum FramePoll {
    /// A complete `(opcode, payload)` frame.
    Frame(u32, Vec<u8>),
    /// Nothing complete yet; the read timed out.
    Empty,
    /// The peer closed the connection.
    Closed,
}

/// One IPC connection: the socket and whatever bytes of a frame have arrived so far. Reading
/// into a buffer (rather than `read_exact` on each part) keeps a frame that a timeout cut in
/// half from being lost.
struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
    user: String,
}

impl Conn {
    /// Finds Discord's socket, connects and handshakes, returning the account it reported.
    fn open(
        config: &Config,
        runtime: Option<&Path>,
        status: &SharedStatus,
        shutdown: &AtomicBool,
    ) -> Result<Conn, ConnectError> {
        set_status(status, Status::Connecting);
        let (path, stream) = connect_socket(runtime)?;
        stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
        let mut conn = Conn { stream, buf: Vec::new(), user: String::new() };
        conn.send(OP_HANDSHAKE, &json!({ "v": 1, "client_id": config.client_id }))
            .map_err(|error| ConnectError::Failed(format!("{}: {error}", path.display())))?;
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return Err(ConnectError::Failed("cancelled".to_owned()));
            }
            if Instant::now() >= deadline {
                return Err(ConnectError::Failed(format!(
                    "{} did not answer the handshake",
                    path.display()
                )));
            }
            match conn.poll() {
                Ok(FramePoll::Frame(OP_FRAME, payload)) => {
                    let value: Value = match serde_json::from_slice(&payload) {
                        Ok(value) => value,
                        Err(error) => return Err(ConnectError::Failed(error.to_string())),
                    };
                    match value["evt"].as_str() {
                        Some("READY") => {
                            conn.user = user_name(&value["data"]["user"]);
                            set_status(status, Status::Connected(conn.user.clone()));
                            return Ok(conn);
                        }
                        Some("ERROR") => {
                            let code = value["data"]["code"].as_u64().unwrap_or(0);
                            let message = value["data"]["message"].as_str().unwrap_or("refused");
                            return Err(ConnectError::Failed(format!("Discord refused: {message} ({code})")));
                        }
                        // Any other DISPATCH is not ours to act on.
                        _ => {}
                    }
                }
                Ok(FramePoll::Frame(OP_PING, payload)) => {
                    let _ = conn.send(OP_PONG, &payload_value(&payload));
                }
                Ok(FramePoll::Frame(OP_CLOSE, payload)) => {
                    return Err(ConnectError::Failed(close_reason(&payload)));
                }
                Ok(FramePoll::Frame(..) | FramePoll::Empty) => {}
                Ok(FramePoll::Closed) => {
                    return Err(ConnectError::Failed("Discord closed the connection".to_owned()));
                }
                Err(error) => return Err(ConnectError::Failed(error.to_string())),
            }
        }
    }

    /// One buffered look at the socket.
    fn poll(&mut self) -> io::Result<FramePoll> {
        loop {
            if self.buf.len() >= 8 {
                let op = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
                let len = u32::from_le_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;
                if len > MAX_FRAME {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Discord frame too large"));
                }
                if self.buf.len() >= 8 + len {
                    let payload = self.buf[8..8 + len].to_vec();
                    self.buf.drain(..8 + len);
                    return Ok(FramePoll::Frame(op, payload));
                }
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(FramePoll::Closed),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Ok(FramePoll::Empty);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn send(&mut self, op: u32, value: &Value) -> io::Result<()> {
        let payload = serde_json::to_vec(value)?;
        self.stream.write_all(&encode_frame(op, &payload))?;
        self.stream.flush()
    }

    /// Answers a `PING` and reports a `CLOSE` (or a broken pipe) as an error so the caller
    /// reconnects.
    fn serve(&mut self) -> io::Result<()> {
        match self.poll()? {
            FramePoll::Frame(OP_PING, payload) => self.send(OP_PONG, &payload_value(&payload)),
            FramePoll::Frame(OP_CLOSE, payload) => {
                Err(io::Error::new(io::ErrorKind::ConnectionAborted, close_reason(&payload)))
            }
            FramePoll::Frame(..) | FramePoll::Empty => Ok(()),
            FramePoll::Closed => Err(io::Error::new(io::ErrorKind::ConnectionReset, "Discord closed the connection")),
        }
    }

    /// Sets (`Some`) or clears (`None`) this process's presence on Discord.
    fn set_activity(&mut self, config: &Config, presence: Option<&Presence>, now: f64) -> io::Result<()> {
        let mut args = Map::new();
        args.insert("pid".to_owned(), json!(std::process::id()));
        if let Some(presence) = presence {
            args.insert("activity".to_owned(), activity_json(config, presence, now));
        }
        // Omitted `activity` is what clears a presence; `nonce` makes the reply matchable.
        let frame = json!({
            "cmd": "SET_ACTIVITY",
            "args": Value::Object(args),
            "nonce": format!("{}-{}", std::process::id(), now),
        });
        self.send(OP_FRAME, &frame)
    }
}

fn payload_value(payload: &[u8]) -> Value {
    serde_json::from_slice(payload).unwrap_or(Value::Null)
}

/// The reason a `CLOSE` frame carried, or a generic one.
fn close_reason(payload: &[u8]) -> String {
    let value = payload_value(payload);
    let code = value["code"].as_u64().unwrap_or(0);
    let message = value["message"].as_str().unwrap_or("Discord closed the connection");
    format!("{message} ({code})")
}

/// Discord's `user` object: the display name when it has one, the account name otherwise.
fn user_name(user: &Value) -> String {
    user["global_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .or_else(|| user["username"].as_str())
        .unwrap_or("Discord")
        .to_owned()
}

/// Why connecting failed: whether Discord is there at all matters to the Settings card.
enum ConnectError {
    NotRunning,
    Failed(String),
}

/// Connects to the first candidate socket that accepts, skipping the stale ones.
fn connect_socket(runtime: Option<&Path>) -> Result<(PathBuf, UnixStream), ConnectError> {
    let mut last_error = None;
    for path in socket_candidates(runtime) {
        match UnixStream::connect(&path) {
            Ok(stream) => return Ok((path, stream)),
            Err(error) if is_missing(&error) => {}
            Err(error) => last_error = Some(format!("{}: {error}", path.display())),
        }
    }
    match last_error {
        Some(message) => Err(ConnectError::Failed(message)),
        None => Err(ConnectError::NotRunning),
    }
}

/// A candidate that is not there (or is a socket file whose Discord has gone) is simply the
/// next candidate's problem.
fn is_missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Every path a Discord client may listen on, most likely first. Discord's own client uses the
/// runtime dir, the Flatpak and Snap builds nest one below it, and older builds used `/tmp`.
fn socket_candidates(runtime: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = runtime {
        for sub in ["", "app/com.discordapp.Discord", "snap.discord"] {
            for index in 0..10 {
                paths.push(dir.join(sub).join(format!("discord-ipc-{index}")));
            }
        }
    } else {
        if let Some(dir) = env_path("XDG_RUNTIME_DIR") {
            for sub in ["", "app/com.discordapp.Discord", "snap.discord"] {
                for index in 0..10 {
                    paths.push(dir.join(sub).join(format!("discord-ipc-{index}")));
                }
            }
        }
        let tmp = env_path("TMPDIR")
            .or_else(|| env_path("TMP"))
            .or_else(|| env_path("TEMP"))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        for index in 0..10 {
            paths.push(tmp.join(format!("discord-ipc-{index}")));
        }
    }
    paths
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// A [`Presence`] for a track from the app's playback lifecycle.
fn presence_of(track: &MediaTrack, position_secs: f64, paused: bool) -> Presence {
    Presence {
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        cover_url: tidal_cover_url(track.cover_url.as_deref()),
        url: tidal_track_url(track),
        duration_secs: track.duration_secs,
        position_secs,
        paused,
    }
}

/// The track's TIDAL page, for the "Listen on TIDAL" button. Local files have no TIDAL page
/// (and their ids are not TIDAL track ids), so they never get a button.
fn tidal_track_url(track: &MediaTrack) -> Option<String> {
    if track.local || track.id.is_empty() || !track.id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(format!("https://tidal.com/browse/track/{}", track.id))
}

/// An https artwork URL Discord can fetch. The shared lifecycle hands over TIDAL's dashed image
/// id (the app's own cover identity); a value that is already a URL is used as it is.
fn tidal_cover_url(cover: Option<&str>) -> Option<String> {
    let cover = cover?.trim();
    if cover.starts_with("https://") {
        return Some(cover.to_owned());
    }
    if !is_tidal_uuid(cover) {
        return None;
    }
    Some(format!(
        "https://resources.tidal.com/images/{}/640x640.jpg",
        cover.replace('-', "/")
    ))
}

/// TIDAL's image ids are canonical dashed UUIDs (as in [`crate::art`]).
fn is_tidal_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Fills `{title}`, `{artist}` and `{album}` in an activity-name template and tidies what is
/// left when one of them is empty (a dangling separator at either end).
fn expand_name(template: &str, presence: &Presence) -> String {
    let filled = template
        .replace("{title}", presence.title.trim())
        .replace("{artist}", presence.artist.trim())
        .replace("{album}", presence.album.trim());
    filled
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—' | '·' | '|'))
        .to_owned()
}

/// The activity Discord should show for `presence`, shaped like Discord's documented activity
/// object: `type`, optional `name`, `details`, `state`, `timestamps`, `assets` (a nested object,
/// not top-level fields), and `buttons`.
///
/// Paused playback has no timestamps and says so in `state` (`"Paused"`, or `"Paused · artist"`
/// when the artist is known), because the protocol has no paused flag: an activity with
/// timestamps would keep counting up while the music sits still.
pub(crate) fn activity_json(config: &Config, presence: &Presence, now: f64) -> Value {
    let mut activity = Map::new();
    activity.insert("type".to_owned(), json!(ACTIVITY_LISTENING));
    // Without a `name` Discord shows the *application's* name after "Listening to"; a
    // non-empty one replaces it for this activity. It is a template: `{title}`, `{artist}` and
    // `{album}` are filled in from the playing track.
    if let Some(name) = clamp_text(&expand_name(&config.name, presence)) {
        activity.insert("name".to_owned(), json!(name));
    }
    if let Some(details) = clamp_text(&presence.title) {
        activity.insert("details".to_owned(), json!(details));
    }
    let state = if presence.paused {
        if presence.artist.trim().is_empty() {
            "Paused".to_owned()
        } else {
            format!("Paused · {}", presence.artist)
        }
    } else {
        presence.artist.clone()
    };
    if let Some(state) = clamp_text(&state) {
        activity.insert("state".to_owned(), json!(state));
    }
    if config.show_art {
        if let Some(image) = presence.cover_url.as_deref().filter(|url| url.starts_with("https://")) {
            // Discord's activity object keeps the images in `assets`; a bare `large_image` at
            // the top level is simply ignored (which is how a cover can go missing silently).
            // An external image is sent as its own URL — Discord proxies it, and only the `mp:`
            // form comes back over the gateway.
            let mut assets = Map::new();
            assets.insert("large_image".to_owned(), json!(image));
            if let Some(album) = clamp_text(&presence.album) {
                assets.insert("large_text".to_owned(), json!(album));
            }
            activity.insert("assets".to_owned(), Value::Object(assets));
        }
    }
    if config.show_time && !presence.paused {
        if let Some(timestamps) = timestamps(presence, now) {
            activity.insert("timestamps".to_owned(), timestamps);
        }
    }
    if config.show_button {
        // `buttons` is a top-level activity field (max two), each `{label ≤ 32, url: https}`;
        // Discord's own docs note that only *other* users see the buttons, never the sender.
        if let Some(url) = presence
            .url
            .as_deref()
            .filter(|url| url.starts_with("https://") && url.len() <= MAX_BUTTON_URL_CHARS)
        {
            activity.insert(
                "buttons".to_owned(),
                json!([{ "label": "Listen on TIDAL", "url": url }]),
            );
        }
    }
    Value::Object(activity)
}

/// `start` (now minus how far in we are) and `end` (plus the duration) as Unix seconds, which
/// is what Discord turns into the elapsed bar. The RPC docs' own SET_ACTIVITY example and the
/// working music clients (tidal-hifi, and the `discord-rpc` bindings they use) send seconds;
/// the millisecond wording in the gateway's activity object describes what the gateway *returns*.
fn timestamps(presence: &Presence, now: f64) -> Option<Value> {
    let duration = presence.duration_secs.filter(|duration| *duration > 0);
    if duration.is_none() && presence.position_secs <= 0.0 {
        return None;
    }
    let position = match duration {
        Some(duration) => presence.position_secs.clamp(0.0, duration as f64),
        None => presence.position_secs.max(0.0),
    };
    let start = (now - position).floor() as i64;
    Some(match duration {
        Some(duration) => json!({ "start": start, "end": start + duration as i64 }),
        // No length known: only the elapsed side is honest.
        None => json!({ "start": start }),
    })
}

/// Discord's text rules: a blank field is dropped, a field over 128 characters is cut, and a
/// single-character field (which Discord rejects) is padded with a zero-width space.
fn clamp_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut clamped: String = text.chars().take(MAX_TEXT_CHARS).collect();
    if clamped.chars().count() == 1 {
        clamped.push(PAD);
    }
    Some(clamped)
}

fn set_status(status: &SharedStatus, next: Status) {
    *status.lock().unwrap() = next;
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or(0.0)
}

// -------------------------------------------------------------------------------------------
// The worker thread

/// What has been published and when, for the rate limit.
#[derive(Default)]
struct Outbox {
    /// The presence the UI last wanted.
    desired: Option<Presence>,
    /// The presence Discord last confirmed (a failed send leaves this alone so it is retried).
    sent: Option<Presence>,
    /// When the last frame went out, for the spacing.
    last_sent: Option<Instant>,
}

impl Outbox {
    /// How long until the next frame may go out.
    fn wait(&self, min_interval: Duration) -> Duration {
        match self.last_sent {
            Some(last) => min_interval.saturating_sub(last.elapsed()),
            None => Duration::ZERO,
        }
    }

    fn take_command(&mut self, command: Command, config: &mut Config) -> Flow {
        match command {
            Command::Set(presence) => {
                self.desired = presence;
                Flow::Continue
            }
            Command::Configure(next) => {
                *config = next;
                // The switches changed: republish even if the track did not.
                self.sent = None;
                Flow::Continue
            }
            Command::Shutdown => Flow::Stop,
        }
    }
}

enum Flow {
    Continue,
    Stop,
}

fn run(
    mut config: Config,
    rx: Receiver<Command>,
    status: SharedStatus,
    wake: Option<Wake>,
    runtime: Option<PathBuf>,
    min_interval: Duration,
    shutdown: Arc<AtomicBool>,
) {
    let mut outbox = Outbox::default();
    let mut backoff = INITIAL_BACKOFF;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        // Connect, waking only to accept commands, until Discord answers.
        let mut conn = loop {
            match Conn::open(&config, runtime.as_deref(), &status, &shutdown) {
                Ok(conn) => {
                    backoff = INITIAL_BACKOFF;
                    break conn;
                }
                Err(ConnectError::NotRunning) => set_status(&status, Status::NotRunning),
                Err(ConnectError::Failed(message)) => set_status(&status, Status::Error(message)),
            }
            notify(&wake);
            match rx.recv_timeout(backoff) {
                Ok(command) => {
                    if let Flow::Stop = outbox.take_command(command, &mut config) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        };
        notify(&wake);
        // A fresh connection has no presence to keep in step with, and no rate-limit history.
        outbox.sent = None;
        outbox.last_sent = None;
        // Serve this connection until it breaks or the client goes away.
        loop {
            if shutdown.load(Ordering::Relaxed) {
                let _ = conn.set_activity(&config, None, now_secs());
                return;
            }
            if outbox.desired != outbox.sent {
                let wait = outbox.wait(min_interval);
                if wait.is_zero() {
                    match conn.set_activity(&config, outbox.desired.as_ref(), now_secs()) {
                        Ok(()) => {
                            outbox.sent = outbox.desired.clone();
                            outbox.last_sent = Some(Instant::now());
                        }
                        Err(_) => break,
                    }
                }
            }
            // Wait for the next command, but never longer than a pending send is due.
            let timeout = if outbox.desired != outbox.sent {
                outbox.wait(min_interval).min(IDLE_WAIT)
            } else {
                IDLE_WAIT
            };
            match rx.recv_timeout(timeout) {
                Ok(command) => {
                    if let Flow::Stop = outbox.take_command(command, &mut config) {
                        let _ = conn.set_activity(&config, None, now_secs());
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = conn.set_activity(&config, None, now_secs());
                    return;
                }
            }
            // `PING`s are answered and a `CLOSE` reconnects.
            if conn.serve().is_err() {
                break;
            }
        }
    }
}

fn notify(wake: &Option<Wake>) {
    if let Some(wake) = wake {
        wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::fs;
    use std::os::unix::net::UnixListener;

    fn temp_dir(name: &str) -> TempDir {
        TempDir::new(name)
    }

    /// A directory that removes itself, for fake-socket files.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("tsun-discord-{}-{name}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn config(client_id: &str) -> Config {
        Config { client_id: client_id.to_owned(), ..Config::default() }
    }

    fn presence(title: &str) -> Presence {
        Presence {
            title: title.to_owned(),
            artist: "An Artist".to_owned(),
            album: "An Album".to_owned(),
            cover_url: Some("https://resources.tidal.com/images/aa/bb/cc/640x640.jpg".to_owned()),
            url: Some("https://tidal.com/browse/track/12345".to_owned()),
            duration_secs: Some(200),
            position_secs: 0.0,
            paused: false,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Frames and activity JSON

    #[test]
    fn frames_round_trip_including_two_back_to_back_and_a_cut_one() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let first = encode_frame(OP_HANDSHAKE, br#"{"v":1}"#);
        let second = encode_frame(OP_FRAME, br#"{"cmd":"SET_ACTIVITY"}"#);
        writer.write_all(&first).unwrap();
        writer.write_all(&second).unwrap();
        // Only the head of a third frame has arrived.
        let third = encode_frame(OP_PING, b"hello");
        writer.write_all(&third[..6]).unwrap();

        let mut conn = Conn { stream: reader.try_clone().unwrap(), buf: Vec::new(), user: String::new() };
        reader.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        // The first frame is exactly the header plus payload, little-endian.
        assert_eq!(first.len(), 8 + 7);
        assert_eq!(u32::from_le_bytes([first[0], first[1], first[2], first[3]]), OP_HANDSHAKE);
        assert_eq!(u32::from_le_bytes([first[4], first[5], first[6], first[7]]), 7);
        assert!(matches!(conn.poll().unwrap(), FramePoll::Frame(OP_HANDSHAKE, payload) if payload == br#"{"v":1}"#));
        assert!(
            matches!(conn.poll().unwrap(), FramePoll::Frame(OP_FRAME, payload) if payload == br#"{"cmd":"SET_ACTIVITY"}"#)
        );
        // The third frame is incomplete: nothing is reported and nothing is lost.
        assert!(matches!(conn.poll().unwrap(), FramePoll::Empty));
        writer.write_all(&third[6..]).unwrap();
        assert!(matches!(conn.poll().unwrap(), FramePoll::Frame(OP_PING, payload) if payload == b"hello"));
    }

    #[test]
    fn activity_carries_details_state_art_album_and_button() {
        let mut presence = presence("A Title");
        presence.position_secs = 30.0;
        let activity = activity_json(&config("12345678901234567"), &presence, 1000.0);
        assert_eq!(activity["type"], json!(2), "listening");
        assert_eq!(activity["details"], json!("A Title"));
        assert_eq!(activity["state"], json!("An Artist"), "Discord adds the 'by' itself");
        // The images live in `assets`: Discord ignores a top-level `large_image` silently, which
        // is exactly how a cover goes missing without an error.
        assert!(activity["large_image"].is_null(), "no flat image field exists");
        assert_eq!(activity["assets"]["large_image"], json!(presence.cover_url.clone().unwrap()));
        assert_eq!(activity["assets"]["large_text"], json!("An Album"));
        assert_eq!(activity["timestamps"], json!({ "start": 970, "end": 1170 }));
        assert_eq!(activity["buttons"][0]["label"], json!("Listen on TIDAL"));
        assert_eq!(activity["buttons"][0]["url"], json!("https://tidal.com/browse/track/12345"));
        assert!(activity["name"].is_null(), "no activity name means the application's own");
        assert_eq!(activity.as_object().unwrap().len(), 6, "{activity}");
        // Exactly the documented activity fields, and nothing else.
        let mut keys: Vec<&str> = activity.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["assets", "buttons", "details", "state", "timestamps", "type"],
            "{activity}"
        );
    }

    #[test]
    fn an_activity_name_replaces_the_application_name_when_set() {
        let presence = presence("A Title");
        let mut config = config("12345678901234567");
        config.name = "  TIDAL  ".to_owned();
        let activity = activity_json(&config, &presence, 1000.0);
        assert_eq!(activity["name"], json!("TIDAL"), "trimmed");
        config.name = "A".to_owned();
        assert_eq!(activity_json(&config, &presence, 1000.0)["name"], json!("A\u{200b}"));
        config.name = "   ".to_owned();
        assert!(activity_json(&config, &presence, 1000.0)["name"].is_null(), "blank means unset");
        config.name = "N".repeat(300);
        assert_eq!(
            activity_json(&config, &presence, 1000.0)["name"].as_str().unwrap().chars().count(),
            MAX_TEXT_CHARS
        );
    }

    /// The name is a template: the status line after "Listening to" can show the song and the
    /// artist, and an empty field does not leave a dangling separator behind.
    #[test]
    fn the_activity_name_template_fills_in_title_artist_and_album() {
        let mut presence = presence("Around the World");
        presence.artist = "Daft Punk".to_owned();
        presence.album = "Homework".to_owned();
        let mut config = config("12345678901234567");
        config.name = "{title} – {artist}".to_owned();
        assert_eq!(activity_json(&config, &presence, 1000.0)["name"], json!("Around the World – Daft Punk"));
        config.name = "{artist} · {album}".to_owned();
        assert_eq!(activity_json(&config, &presence, 1000.0)["name"], json!("Daft Punk · Homework"));
        presence.artist = String::new();
        config.name = "{title} – {artist}".to_owned();
        assert_eq!(activity_json(&config, &presence, 1000.0)["name"], json!("Around the World"), "no dangling dash");
        config.name = String::new();
        assert!(activity_json(&config, &presence, 1000.0)["name"].is_null(), "empty uses the application's name");
    }

    #[test]
    fn the_button_is_shaped_the_way_discord_documents_it() {
        // Up to two `{label ≤ 32, url: https ≤ 512}` objects; a non-https or over-long URL makes
        // Discord drop the whole activity, so only https links are ever sent.
        let mut presence = presence("A Title");
        presence.url = Some("http://insecure.example/track/1".to_owned());
        assert!(activity_json(&Config::default(), &presence, 0.0)["buttons"].is_null(), "plain http is not sent");
        presence.url = Some(format!("https://tidal.com/browse/track/{}", "1".repeat(600)));
        let activity = activity_json(&Config::default(), &presence, 0.0);
        assert!(activity["buttons"].is_null(), "an over-long URL is dropped rather than breaking the activity");
        presence.url = Some("https://tidal.com/browse/track/42".to_owned());
        let activity = activity_json(&Config::default(), &presence, 0.0);
        let buttons = activity["buttons"].as_array().unwrap();
        assert_eq!(buttons.len(), 1, "one button, well inside the two Discord allows");
        assert!(buttons[0]["label"].as_str().unwrap().chars().count() <= 32);
        assert!(buttons[0]["url"].as_str().unwrap().starts_with("https://"));
        assert!(buttons[0]["url"].as_str().unwrap().len() <= 512);
    }

    #[test]
    fn timestamps_follow_the_position_and_need_a_duration_for_the_end() {
        let mut presence = presence("T");
        presence.position_secs = 12.0;
        presence.duration_secs = Some(60);
        assert_eq!(
            activity_json(&Config::default(), &presence, 500.0)["timestamps"],
            json!({ "start": 488, "end": 548 })
        );
        presence.duration_secs = None;
        assert_eq!(
            activity_json(&Config::default(), &presence, 500.0)["timestamps"],
            json!({ "start": 488 }),
            "an unknown length shows only elapsed"
        );
        presence.position_secs = 0.0;
        assert!(activity_json(&Config::default(), &presence, 500.0)["timestamps"].is_null(), "nothing to say yet");
        // A position past the end does not make the bar run backwards.
        presence.duration_secs = Some(60);
        presence.position_secs = 90.0;
        assert_eq!(
            activity_json(&Config::default(), &presence, 500.0)["timestamps"],
            json!({ "start": 440, "end": 500 })
        );
    }

    #[test]
    fn paused_playback_has_no_timestamps_and_says_so() {
        let mut presence = presence("A Title");
        presence.position_secs = 30.0;
        presence.paused = true;
        let activity = activity_json(&Config::default(), &presence, 1000.0);
        assert!(activity["timestamps"].is_null(), "a paused bar must not keep counting");
        assert_eq!(activity["state"], json!("Paused · An Artist"));
        presence.artist = "  ".to_owned();
        assert_eq!(activity_json(&Config::default(), &presence, 1000.0)["state"], json!("Paused"));
    }

    #[test]
    fn long_text_is_cut_and_a_single_character_is_padded() {
        assert_eq!(clamp_text(""), None, "a blank field is dropped");
        assert_eq!(clamp_text("   "), None);
        assert_eq!(clamp_text("A"), Some("A\u{200b}".to_owned()), "Discord wants at least two characters");
        let long = "x".repeat(200);
        let clamped = clamp_text(&long).unwrap();
        assert_eq!(clamped.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(clamp_text("  A Title  "), Some("A Title".to_owned()));

        let mut presence = presence("A");
        presence.artist = "B".repeat(300);
        presence.album = "C".repeat(300);
        let activity = activity_json(&Config::default(), &presence, 0.0);
        assert_eq!(activity["details"].as_str().unwrap().chars().count(), 2);
        assert_eq!(activity["state"].as_str().unwrap().chars().count(), 128);
        assert_eq!(activity["assets"]["large_text"].as_str().unwrap().chars().count(), 128);
    }

    #[test]
    fn the_switches_drop_art_time_and_button() {
        let mut presence = presence("A Title");
        presence.position_secs = 5.0;
        let muted = Config { show_art: false, show_time: false, show_button: false, ..Config::default() };
        let activity = activity_json(&muted, &presence, 1000.0);
        assert!(activity["assets"].is_null(), "no art means no assets object at all");
        assert!(activity["timestamps"].is_null());
        assert!(activity["buttons"].is_null());
        assert_eq!(activity["details"], json!("A Title"), "the text stays");
        // A local file has no TIDAL link and no https cover.
        let local = Presence { url: None, cover_url: None, ..presence };
        let activity = activity_json(&Config::default(), &local, 1000.0);
        assert!(activity["buttons"].is_null());
        assert!(activity["assets"].is_null());
        // An empty artist leaves no state field rather than a blank one.
        let nameless = Presence { artist: String::new(), ..local };
        assert!(activity_json(&Config::default(), &nameless, 1000.0)["state"].is_null());
    }

    #[test]
    fn no_presence_is_cleared_with_an_activity_less_frame() {
        let (writer, reader) = UnixStream::pair().unwrap();
        reader.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut conn = Conn { stream: reader, buf: Vec::new(), user: String::new() };
        conn.set_activity(&config("1"), None, 0.0).unwrap();
        let frame = read_from(&writer, OP_FRAME);
        assert_eq!(frame["cmd"], json!("SET_ACTIVITY"));
        assert!(frame["args"]["activity"].is_null(), "omitting the activity clears the presence");
        assert!(frame["args"]["pid"].as_u64().unwrap() > 0);
    }

    /// Reads one frame of `op` from `peer`, through the shared decoder: what the other end of
    /// the connection wrote.
    fn read_from(peer: &UnixStream, op: u32) -> Value {
        let reader = peer.try_clone().unwrap();
        reader.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut conn = Conn { stream: reader, buf: Vec::new(), user: String::new() };
        loop {
            match conn.poll().unwrap() {
                FramePoll::Frame(actual, payload) => {
                    assert_eq!(actual, op, "unexpected opcode");
                    return payload_value(&payload);
                }
                FramePoll::Empty => {}
                FramePoll::Closed => panic!("the peer closed"),
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Socket discovery

    #[test]
    fn socket_candidates_cover_the_runtime_dir_flatpak_snap_and_tmp() {
        let runtime = PathBuf::from("/run/user/1000");
        let tmp = PathBuf::from("/tmp");
        let candidates = socket_candidates(Some(&runtime));
        assert!(candidates.contains(&runtime.join("discord-ipc-0")));
        assert!(candidates.contains(&runtime.join("discord-ipc-9")));
        assert!(candidates.contains(&runtime.join("app/com.discordapp.Discord/discord-ipc-0")));
        assert!(candidates.contains(&runtime.join("snap.discord/discord-ipc-3")));
        assert!(
            !candidates.iter().any(|path| path.starts_with(&tmp)),
            "an injected runtime dir must not fall back to /tmp: {candidates:?}"
        );
        assert_eq!(candidates[0], runtime.join("discord-ipc-0"), "the most common path is tried first");
        // The historic `/tmp` fallback belongs to environment-driven discovery.
        let expected_tmp = env_path("TMPDIR")
            .or_else(|| env_path("TMP"))
            .or_else(|| env_path("TEMP"))
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let environment = socket_candidates(None);
        assert!(environment.iter().any(|path| path.starts_with(&expected_tmp)), "{environment:?}");
    }

    #[test]
    fn injection_keeps_discovery_inside_the_given_directory() {
        let dir = temp_dir("discovery");
        let candidates = socket_candidates(Some(dir.path()));
        assert!(candidates.iter().all(|path| path.starts_with(dir.path())), "{candidates:?}");
        assert_eq!(candidates.len(), 30, "three layouts of ten slots");
    }

    // ---------------------------------------------------------------------------------------
    // The fake Discord

    /// A fake Discord IPC server: hands out raw frames and collects what the client sent.
    struct FakeDiscord {
        dir: TempDir,
        received: Receiver<(u32, Value)>,
        handshakes: Receiver<Value>,
    }

    impl FakeDiscord {
        /// Accepts `connections` connections, answers each handshake with `READY` for
        /// `user`, and forwards every later frame. `close_after_first` drops the first
        /// connection once its first `SET_ACTIVITY` has arrived, which is how a reconnect
        /// (Discord restarting) is provoked.
        fn start(name: &str, connections: usize, user: &str, close_after_first: bool) -> FakeDiscord {
            let dir = temp_dir(name);
            let path = dir.path().join("discord-ipc-0");
            let listener = UnixListener::bind(&path).unwrap();
            let (received_tx, received) = mpsc::channel();
            let (handshake_tx, handshakes) = mpsc::channel();
            let user = user.to_owned();
            thread::spawn(move || {
                for connection in 0..connections {
                    let Ok((stream, _)) = listener.accept() else { return };
                    let mut conn = Conn { stream, buf: Vec::new(), user: String::new() };
                    // The client's handshake first.
                    if let Some(payload) = read_value(&mut conn, OP_HANDSHAKE) {
                        if handshake_tx.send(payload).is_err() {
                            return;
                        }
                    } else {
                        return;
                    }
                    let ready = json!({
                        "cmd": "DISPATCH",
                        "evt": "READY",
                        "data": { "v": 1, "user": { "id": "1", "username": user, "global_name": null } },
                    });
                    if conn.send(OP_FRAME, &ready).is_err() {
                        return;
                    }
                    let mut seen = 0;
                    while let Some((op, payload)) = read_frame(&mut conn) {
                        if received_tx.send((op, payload)).is_err() {
                            return;
                        }
                        seen += 1;
                        if close_after_first && connection == 0 && seen >= 1 {
                            // Discord went away: close this connection.
                            break;
                        }
                    }
                }
            });
            FakeDiscord { dir, received, handshakes }
        }

        /// The next frame the client sent, within `timeout`.
        fn next_frame(&self, timeout: Duration) -> Option<(u32, Value)> {
            self.received.recv_timeout(timeout).ok()
        }

        fn next_handshake(&self, timeout: Duration) -> Option<Value> {
            self.handshakes.recv_timeout(timeout).ok()
        }

        fn dir(&self) -> &Path {
            self.dir.path()
        }
    }

    /// Reads one frame of any opcode with a blocking read; `None` at end of stream.
    fn read_frame(conn: &mut Conn) -> Option<(u32, Value)> {
        conn.stream.set_read_timeout(None).ok();
        loop {
            match conn.poll() {
                Ok(FramePoll::Frame(op, payload)) => return Some((op, payload_value(&payload))),
                Ok(FramePoll::Empty) => {}
                Ok(FramePoll::Closed) | Err(_) => return None,
            }
        }
    }

    fn read_value(conn: &mut Conn, op: u32) -> Option<Value> {
        let (actual, value) = read_frame(conn)?;
        assert_eq!(actual, op, "expected opcode {op}, got {actual}: {value}");
        Some(value)
    }

    /// Waits for `predicate` to hold of the status, at most `timeout`.
    fn wait_status(discord: &Discord, timeout: Duration, predicate: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + timeout;
        loop {
            let status = discord.status();
            if predicate(&status) || Instant::now() >= deadline {
                return status;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    // ---------------------------------------------------------------------------------------
    // The client against the fake Discord

    #[test]
    fn handshake_publishes_the_presence_and_dropping_the_client_clears_it() {
        let fake = FakeDiscord::start("full", 1, "tester", false);
        let discord = Discord::start_with(
            config("12345678901234567"),
            None,
            Some(fake.dir().to_path_buf()),
            Duration::ZERO,
        );
        let handshake = fake.next_handshake(Duration::from_secs(5)).expect("a handshake");
        assert_eq!(handshake["v"], json!(1));
        assert_eq!(handshake["client_id"], json!("12345678901234567"));
        let status = wait_status(&discord, Duration::from_secs(5), Status::is_connected);
        assert_eq!(status, Status::Connected("tester".to_owned()));

        let mut current = presence("A Title");
        current.position_secs = 30.0;
        discord.show(current);
        let (op, frame) = fake.next_frame(Duration::from_secs(5)).expect("a SET_ACTIVITY frame");
        assert_eq!(op, OP_FRAME);
        assert_eq!(frame["cmd"], json!("SET_ACTIVITY"));
        assert_eq!(frame["args"]["activity"]["details"], json!("A Title"));
        assert_eq!(frame["args"]["activity"]["state"], json!("An Artist"));
        assert!(frame["nonce"].is_string());

        // Pausing republishes without timestamps, resuming puts them back.
        discord.update(30.0, true);
        let (_, paused) = fake.next_frame(Duration::from_secs(5)).expect("a pause frame");
        assert!(paused["args"]["activity"]["timestamps"].is_null());
        assert_eq!(paused["args"]["activity"]["state"], json!("Paused · An Artist"));

        // Dropping the client clears the presence and stops its thread.
        drop(discord);
        let (_, cleared) = fake.next_frame(Duration::from_secs(5)).expect("a clear frame");
        assert!(cleared["args"]["activity"].is_null(), "{cleared}");
    }

    #[test]
    fn a_closed_socket_reconnects_and_republishes_the_current_presence() {
        let fake = FakeDiscord::start("reconnect", 2, "tester", true);
        let discord = Discord::start_with(
            config("12345678901234567"),
            None,
            Some(fake.dir().to_path_buf()),
            Duration::ZERO,
        );
        assert!(fake.next_handshake(Duration::from_secs(5)).is_some());
        assert!(wait_status(&discord, Duration::from_secs(5), Status::is_connected).is_connected());
        discord.show(presence("A Title"));
        let (_, first) = fake.next_frame(Duration::from_secs(5)).expect("the first publication");
        assert_eq!(first["args"]["activity"]["details"], json!("A Title"));
        // The fake closes the connection, so the client reconnects and republishes by itself.
        assert!(fake.next_handshake(Duration::from_secs(10)).is_some(), "a second handshake");
        let (_, second) = fake.next_frame(Duration::from_secs(10)).expect("the republication");
        assert_eq!(second["args"]["activity"]["details"], json!("A Title"), "{second}");
        assert!(wait_status(&discord, Duration::from_secs(5), Status::is_connected).is_connected());
    }

    #[test]
    fn rapid_updates_coalesce_to_the_latest_within_the_interval() {
        let fake = FakeDiscord::start("ratelimit", 1, "tester", false);
        let interval = Duration::from_millis(400);
        let discord =
            Discord::start_with(config("12345678901234567"), None, Some(fake.dir().to_path_buf()), interval);
        assert!(fake.next_handshake(Duration::from_secs(5)).is_some());
        assert!(wait_status(&discord, Duration::from_secs(5), Status::is_connected).is_connected());

        let mut first = presence("Skipped 1");
        first.position_secs = 0.0;
        discord.show(first);
        let (_, sent) = fake.next_frame(Duration::from_secs(5)).expect("the first update");
        assert_eq!(sent["args"]["activity"]["details"], json!("Skipped 1"));

        // Three more changes in a row: only the last one may reach Discord.
        discord.show(presence("Skipped 2"));
        discord.update(5.0, false);
        discord.show(presence("Kept"));
        let (_, coalesced) = fake.next_frame(Duration::from_secs(5)).expect("the coalesced update");
        assert_eq!(coalesced["args"]["activity"]["details"], json!("Kept"), "{coalesced}");
        assert!(coalesced["args"]["activity"]["timestamps"].is_object(), "{coalesced}");
        assert!(
            fake.next_frame(Duration::from_millis(600)).is_none(),
            "nothing else may go out: the two skipped updates were dropped"
        );
    }

    #[test]
    fn a_missing_socket_is_reported_as_not_running_and_shuts_down_at_once() {
        let dir = temp_dir("absent");
        let bare = Config { client_id: "12345678901234567".to_owned(), ..Config::default() };
        let discord = Discord::start_with(bare, None, Some(dir.path().to_path_buf()), Duration::ZERO);
        assert_eq!(wait_status(&discord, Duration::from_secs(5), |status| *status == Status::NotRunning), Status::NotRunning);
        assert_eq!(Status::NotRunning.label(), "Not running");
        let started = Instant::now();
        drop(discord);
        assert!(started.elapsed() < Duration::from_secs(2), "dropping must not wait on a backoff");
    }

    #[test]
    fn a_refused_application_id_is_an_error_with_discords_own_words() {
        let dir = temp_dir("refused");
        let listener = UnixListener::bind(dir.path().join("discord-ipc-0")).unwrap();
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut conn = Conn { stream, buf: Vec::new(), user: String::new() };
                if read_value(&mut conn, OP_HANDSHAKE).is_some() {
                    let refused = json!({
                        "cmd": "SET_ACTIVITY",
                        "evt": "ERROR",
                        "data": { "code": 4000, "message": "Invalid Client ID" },
                    });
                    let _ = conn.send(OP_FRAME, &refused);
                }
            }
        });
        let discord =
            Discord::start_with(config("12345678901234567"), None, Some(dir.path().to_path_buf()), Duration::ZERO);
        let status = wait_status(&discord, Duration::from_secs(5), |status| matches!(status, Status::Error(_)));
        match status {
            Status::Error(message) => assert!(message.contains("Invalid Client ID"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn pings_are_answered_with_pongs_and_closes_are_failures() {
        let (client, mut server) = UnixStream::pair().unwrap();
        client.set_read_timeout(Some(Duration::from_millis(50))).ok();
        let mut conn = Conn { stream: client, buf: Vec::new(), user: String::new() };
        server.write_all(&encode_frame(OP_PING, br#"{"n":7}"#)).unwrap();
        // `serve` answers the ping without treating it as a broken connection.
        assert!(conn.serve().is_ok(), "a PING is not a failure");
        // The answer is the PONG the peer received, carrying the ping's payload back.
        assert_eq!(read_from(&server, OP_PONG), json!({ "n": 7 }));

        // A CLOSE is reported so the caller reconnects, with Discord's own words.
        let close = json!({ "code": 4000, "message": "Discord is restarting" });
        server.write_all(&encode_frame(OP_CLOSE, &serde_json::to_vec(&close).unwrap())).unwrap();
        let mut error = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Err(served) = conn.serve() {
                error = Some(served);
                break;
            }
        }
        let error = error.expect("a CLOSE must be reported");
        assert!(error.to_string().contains("Discord is restarting"), "{error}");
        // And a plain end of stream too.
        drop(server);
        assert!(conn.serve().is_err());
    }

    #[test]
    fn media_events_become_presence_and_local_files_lose_the_button() {
        let media = MediaTrack {
            id: "987654".to_owned(),
            title: "A Title".to_owned(),
            artist: "An Artist".to_owned(),
            album: "An Album".to_owned(),
            duration_secs: Some(210),
            cover_url: Some("01234567-89ab-cdef-0123-456789abcdef".to_owned()),
            local: false,
        };
        let presence = presence_of(&media, 20.0, false);
        assert_eq!(presence.url.as_deref(), Some("https://tidal.com/browse/track/987654"));
        assert_eq!(
            presence.cover_url.as_deref(),
            Some("https://resources.tidal.com/images/01234567/89ab/cdef/0123/456789abcdef/640x640.jpg")
        );
        assert_eq!(presence.duration_secs, Some(210));
        let activity = activity_json(&Config::default(), &presence, 1000.0);
        assert_eq!(activity["timestamps"], json!({ "start": 980, "end": 1190 }));

        // A local file: no TIDAL page, so no button, whatever the display switches say.
        let local = MediaTrack { id: "9223372036854775809".to_owned(), local: true, ..media.clone() };
        let presence = presence_of(&local, 0.0, false);
        assert_eq!(presence.url, None);
        assert!(activity_json(&Config::default(), &presence, 1000.0)["buttons"].is_null());
        // A cover that is already a URL is used as it is; a broken one is dropped.
        assert_eq!(
            tidal_cover_url(Some("https://example.com/cover.png")).as_deref(),
            Some("https://example.com/cover.png")
        );
        assert_eq!(tidal_cover_url(Some("not-a-uuid")), None);
        assert_eq!(tidal_cover_url(Some("0123456789abcdef0123456789abcdef")), None);
        assert_eq!(tidal_cover_url(None), None);
        // Non-numeric TIDAL ids (a defensive case) still get no button.
        let odd = MediaTrack { id: "abc".to_owned(), ..media };
        assert_eq!(tidal_track_url(&odd), None);
    }

    #[test]
    fn media_events_drive_the_published_activity() {
        let fake = FakeDiscord::start("events", 1, "tester", false);
        let discord = Discord::start_with(
            config("12345678901234567"),
            None,
            Some(fake.dir().to_path_buf()),
            Duration::ZERO,
        );
        assert!(fake.next_handshake(Duration::from_secs(5)).is_some());
        assert!(wait_status(&discord, Duration::from_secs(5), Status::is_connected).is_connected());

        let media = MediaTrack {
            id: "42".to_owned(),
            title: "Song".to_owned(),
            artist: "Band".to_owned(),
            album: "Disc".to_owned(),
            duration_secs: Some(120),
            cover_url: None,
            local: false,
        };
        discord.media_event(&MediaEvent::Started { track: media.clone(), position_secs: 0.0 });
        let (_, started) = fake.next_frame(Duration::from_secs(5)).expect("the started frame");
        assert_eq!(started["args"]["activity"]["details"], json!("Song"));
        assert_eq!(started["args"]["activity"]["state"], json!("Band"));
        assert_eq!(
            started["args"]["activity"]["buttons"][0]["url"],
            json!("https://tidal.com/browse/track/42")
        );

        discord.media_event(&MediaEvent::Paused { position_secs: 30.0 });
        let (_, paused) = fake.next_frame(Duration::from_secs(5)).expect("the pause frame");
        assert!(paused["args"]["activity"]["timestamps"].is_null());
        assert_eq!(paused["args"]["activity"]["state"], json!("Paused · Band"));

        discord.media_event(&MediaEvent::Stopped);
        let (_, cleared) = fake.next_frame(Duration::from_secs(5)).expect("the clear frame");
        assert!(cleared["args"]["activity"].is_null(), "{cleared}");
    }

    #[test]
    fn the_ui_fan_out_reaches_the_wire_for_tidal_tracks_and_local_files() {
        use crate::app::{NowPlaying, init};

        let fake = FakeDiscord::start("fanout", 1, "tester", false);
        let mut ui = init();
        ui.discord = Some(Discord::start_with(
            config("12345678901234567"),
            None,
            Some(fake.dir().to_path_buf()),
            Duration::ZERO,
        ));
        assert!(fake.next_handshake(Duration::from_secs(5)).is_some());

        // A TIDAL track starting: the app's own lifecycle emits the event, which must arrive at
        // Discord as a Listening activity with the cover, the bar and the TIDAL button.
        ui.now_playing = Some(NowPlaying {
            id: 42,
            title: "Song".to_owned(),
            artist: "Band".to_owned(),
            album: "Disc".to_owned(),
            cover: Some("01234567-89ab-cdef-0123-456789abcdef".to_owned()),
            album_id: None,
            artist_id: None,
            elapsed: 0,
            duration: Some(120),
            quality: None,
            output: None,
        });
        ui.media_started(0.0);
        let (_, started) = fake.next_frame(Duration::from_secs(5)).expect("the started frame");
        let activity = &started["args"]["activity"];
        assert_eq!(activity["details"], json!("Song"));
        assert_eq!(activity["state"], json!("Band"));
        assert_eq!(activity["assets"]["large_text"], json!("Disc"));
        assert_eq!(
            activity["assets"]["large_image"],
            json!("https://resources.tidal.com/images/01234567/89ab/cdef/0123/456789abcdef/640x640.jpg"),
            "the cover goes out inside `assets`, as Discord's activity object requires"
        );
        assert_eq!(activity["buttons"][0]["url"], json!("https://tidal.com/browse/track/42"));

        // A seek, then a pause: the bar follows the position and a pause says so.
        ui.now_playing.as_mut().unwrap().elapsed = 30;
        ui.media_seeked(30.0);
        let (_, seeked) = fake.next_frame(Duration::from_secs(5)).expect("the seeked frame");
        let timestamps = &seeked["args"]["activity"]["timestamps"];
        let start = timestamps["start"].as_i64().expect("a start");
        assert_eq!(timestamps["end"].as_i64(), Some(start + 120));
        ui.media_pause_changed(true, 30.0);
        let (_, paused) = fake.next_frame(Duration::from_secs(5)).expect("the paused frame");
        assert!(paused["args"]["activity"]["timestamps"].is_null());
        assert_eq!(paused["args"]["activity"]["state"], json!("Paused · Band"));

        // A local file: same presence, no TIDAL button (local ids have no TIDAL page).
        let local_id = crate::local::local_id(std::path::Path::new("/music/song.flac"));
        ui.now_playing = Some(NowPlaying {
            id: local_id,
            title: "Local Song".to_owned(),
            artist: "Local Band".to_owned(),
            album: "Local Disc".to_owned(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 0,
            duration: Some(200),
            quality: None,
            output: None,
        });
        ui.media_started(0.0);
        let (_, local) = fake.next_frame(Duration::from_secs(5)).expect("the local frame");
        let activity = &local["args"]["activity"];
        assert_eq!(activity["details"], json!("Local Song"), "{activity}");
        assert!(activity["buttons"].is_null(), "a local file must not get a TIDAL button");
        assert!(activity["assets"].is_null(), "a local file has no TIDAL cover");

        // Stopping clears it.
        ui.media_stopped();
        let (_, cleared) = fake.next_frame(Duration::from_secs(5)).expect("the cleared frame");
        assert!(cleared["args"]["activity"].is_null());
    }

    #[test]
    fn the_activity_name_and_assets_go_over_the_wire_as_discord_expects() {
        let fake = FakeDiscord::start("named", 1, "tester", false);
        let mut settings = config("12345678901234567");
        settings.name = "TIDAL".to_owned();
        let discord = Discord::start_with(
            settings.clone(),
            None,
            Some(fake.dir().to_path_buf()),
            Duration::ZERO,
        );
        assert!(fake.next_handshake(Duration::from_secs(5)).is_some());
        let mut current = presence("A Title");
        current.position_secs = 5.0;
        discord.show(current);
        let (_, frame) = fake.next_frame(Duration::from_secs(5)).expect("the activity frame");
        let activity = &frame["args"]["activity"];
        assert_eq!(activity["name"], json!("TIDAL"), "the name overrides the application name");
        assert_eq!(activity["type"], json!(2));
        assert_eq!(activity["details"], json!("A Title"));
        assert_eq!(activity["state"], json!("An Artist"));
        assert!(activity["timestamps"]["start"].as_i64().is_some());
        assert!(activity["assets"]["large_image"].as_str().unwrap().starts_with("https://resources.tidal.com/images/"));
        assert_eq!(activity["assets"]["large_text"], json!("An Album"));
        assert_eq!(activity["buttons"][0]["label"], json!("Listen on TIDAL"));
        assert!(activity["large_image"].is_null(), "never at the top level");
        assert!(activity["large_text"].is_null(), "never at the top level");

        // Turning the name off (an empty one) retunes the client and republishes without it.
        discord.configure(config("12345678901234567"));
        let (_, renamed) = fake.next_frame(Duration::from_secs(5)).expect("the retuned frame");
        assert!(renamed["args"]["activity"]["name"].is_null(), "{renamed}");
        assert_eq!(renamed["args"]["activity"]["details"], json!("A Title"), "the track is still there");
    }

    #[test]
    fn a_restarted_client_republishes_the_current_track() {
        let fake = FakeDiscord::start("republish", 1, "tester", false);
        let discord = Discord::start_with(
            config("12345678901234567"),
            None,
            Some(fake.dir().to_path_buf()),
            Duration::ZERO,
        );
        assert!(fake.next_handshake(Duration::from_secs(5)).is_some());
        let media = MediaTrack {
            id: "7".to_owned(),
            title: "Song".to_owned(),
            artist: "Band".to_owned(),
            album: "Disc".to_owned(),
            duration_secs: Some(100),
            cover_url: None,
            local: false,
        };
        // A paused track republishes as paused, then as playing at the same position.
        discord.republish(&media, 12.0, true);
        let (_, paused) = fake.next_frame(Duration::from_secs(5)).expect("the republished frame");
        assert_eq!(paused["args"]["activity"]["details"], json!("Song"));
        assert_eq!(paused["args"]["activity"]["state"], json!("Paused · Band"));
        assert!(paused["args"]["activity"]["timestamps"].is_null());
        discord.republish(&media, 12.0, false);
        let (_, playing) = fake.next_frame(Duration::from_secs(5)).expect("the republished frame");
        let timestamps = &playing["args"]["activity"]["timestamps"];
        let start = timestamps["start"].as_i64().expect("a start");
        assert_eq!(timestamps["end"].as_i64(), Some(start + 100), "the position is honoured");
    }

    #[test]
    fn status_labels_are_the_ones_the_card_shows() {
        assert_eq!(Status::Connecting.label(), "Connecting…");
        assert_eq!(Status::Connected("ana".to_owned()).label(), "Connected as ana");
        assert_eq!(Status::Error("boom".to_owned()).label(), "Error: boom");
        assert!(Status::Connected("ana".to_owned()).is_connected());
        assert!(!Status::NotRunning.is_connected());
    }

    #[test]
    fn client_ids_are_validated_as_digits_with_a_plausible_length() {
        assert!(valid_client_id("12345678901234567"));
        assert!(valid_client_id("12345678901234567890"));
        assert!(!valid_client_id("1234567890123456"), "16 digits is too short");
        assert!(!valid_client_id("123456789012345678901"), "21 digits is too long");
        assert!(!valid_client_id("1234567890123456a"));
        assert!(!valid_client_id(""));
        assert!(valid_client_id(" 12345678901234567 "), "surrounding spaces are trimmed");
    }

    #[test]
    fn outbox_waits_between_frames_and_forgets_the_rate_limit_on_reconnect() {
        let mut outbox = Outbox::default();
        let interval = Duration::from_millis(300);
        assert!(outbox.wait(interval).is_zero(), "the first frame is never delayed");
        outbox.last_sent = Some(Instant::now());
        assert!(!outbox.wait(interval).is_zero());
        outbox.last_sent = Some(Instant::now() - interval * 2);
        assert!(outbox.wait(interval).is_zero());
    }

    #[test]
    fn configure_republishes_and_shutdown_stops() {
        let mut outbox = Outbox::default();
        let mut current = config("1");
        outbox.sent = Some(presence("Title"));
        outbox.desired = Some(presence("Title"));
        assert!(matches!(outbox.take_command(Command::Configure(Config::default()), &mut current), Flow::Continue));
        assert_eq!(outbox.sent, None, "a switch change forces a republish");
        assert!(matches!(outbox.take_command(Command::Shutdown, &mut current), Flow::Stop));
    }

    #[test]
    fn requests_queue_up_while_discord_is_missing_and_apply_on_connect() {
        // A dir where the socket appears only later: the client retries with a backoff and then
        // publishes what the UI asked for while it was away.
        let dir = temp_dir("late");
        let path = dir.path().join("discord-ipc-0");
        let discord = Discord::start_with(
            config("12345678901234567"),
            None,
            Some(dir.path().to_path_buf()),
            Duration::ZERO,
        );
        discord.show(presence("Queued"));
        assert_eq!(wait_status(&discord, Duration::from_secs(5), |s| *s == Status::NotRunning), Status::NotRunning);
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut conn = Conn { stream, buf: Vec::new(), user: String::new() };
                if read_value(&mut conn, OP_HANDSHAKE).is_some() {
                    let ready = json!({
                        "cmd": "DISPATCH",
                        "evt": "READY",
                        "data": { "user": { "username": "late" } },
                    });
                    if conn.send(OP_FRAME, &ready).is_ok() {
                        if let Some((_, payload)) = read_frame(&mut conn) {
                            let _ = tx.send(payload);
                        }
                    }
                }
            }
        });
        let published = rx.recv_timeout(Duration::from_secs(10)).expect("the queued presence");
        assert_eq!(published["args"]["activity"]["details"], json!("Queued"));
    }

    #[test]
    fn play_presence_folds_position_and_pause_into_the_same_track() {
        let mut current = presence("A Title");
        current.position_secs = 10.0;
        let moved = current.at(42.0, true);
        assert_eq!(moved.title, "A Title");
        assert_eq!(moved.position_secs, 42.0);
        assert!(moved.paused);
        assert_eq!(current.position_secs, 10.0, "the original is untouched");
    }

    #[test]
    fn a_queue_of_commands_is_drained_in_order() {
        let (tx, rx) = mpsc::channel();
        tx.send(Command::Set(Some(presence("First")))).unwrap();
        tx.send(Command::Configure(config("2"))).unwrap();
        tx.send(Command::Set(None)).unwrap();
        drop(tx);
        let mut outbox = Outbox::default();
        let mut current = config("1");
        let mut seen = VecDeque::new();
        while let Ok(command) = rx.recv() {
            seen.push_back(matches!(outbox.take_command(command, &mut current), Flow::Continue));
        }
        assert!(seen.into_iter().all(|continued| continued));
        assert_eq!(outbox.desired, None);
        assert_eq!(current.client_id, "2");
    }
}
