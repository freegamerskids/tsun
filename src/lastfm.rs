//! Last.fm scrobbling.
//!
//! The **scrobbling rules** are the official ones: a play is sent once when it has actually
//! played for at least half its length or four minutes (whichever comes first), the track is
//! longer than 30 seconds, and it is longer than 30 s; real played time is counted, so pausing
//! stops the clock and seeking neither adds nor removes time. `track.updateNowPlaying` is sent
//! when a track starts, and a played-so-far track is scrobbled when it ends, is skipped or the
//! transport stops.
//!
//! Everything that touches the network runs on a dedicated thread so the UI never blocks: the
//! UI sends [`Command`]s and drains [`LastFmStatus`]s in `poll`. Failed submissions go to an
//! on-disk queue (`lastfm_queue.json`) that is retried with backoff; submissions Last.fm rejects
//! as permanently invalid are dropped instead of being retried forever.
//!
//! The user's own API key and shared secret (from <https://www.last.fm/api/account/create>) and
//! the authorized session live in `lastfm.json` (0600) beside the rest of tsun's config, never
//! in `settings.json`. tsun ships no key of its own.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rune::{TextEdit, Waker};
use serde::{Deserialize, Serialize};

use crate::app::Ui;
use crate::media_events::{MediaEvent, MediaTrack};

/// Last.fm's API 2.0 endpoint. `Api::with_base` overrides it for the mock-server test.
pub(crate) const API_BASE: &str = "https://ws.audioscrobbler.com/2.0/";
/// Where the user authorizes a token in their browser.
pub(crate) const AUTH_URL: &str = "https://www.last.fm/api/auth/";
/// `track.scrobble` accepts up to 50 submissions per request.
pub(crate) const BATCH: usize = 50;
/// The offline queue never grows past this many plays; the oldest fall off.
const QUEUE_MAX: usize = 1000;
/// A track shorter than this is never scrobbled.
const MIN_TRACK_SECS: u64 = 30;
/// A play must reach half the track or this many seconds, whichever comes first.
const MIN_PLAYED_SECS: f64 = 240.0;
/// First retry delay for the offline queue, doubling up to [`MAX_BACKOFF_SECS`].
const BASE_BACKOFF_SECS: f64 = 30.0;
const MAX_BACKOFF_SECS: f64 = 900.0;

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------------------------
// Persisted state

/// What tsun keeps for Last.fm: the user's API credentials, the authorized session and the
/// feature toggles. Stored at `$XDG_CONFIG_HOME/tsun/lastfm.json` (0600).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Store {
    pub(crate) api_key: String,
    pub(crate) secret: String,
    pub(crate) session_key: String,
    pub(crate) username: String,
    #[serde(default = "default_true")]
    pub(crate) scrobble: bool,
    #[serde(default = "default_true")]
    pub(crate) now_playing: bool,
    pub(crate) love: bool,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            api_key: String::new(),
            secret: String::new(),
            session_key: String::new(),
            username: String::new(),
            scrobble: true,
            now_playing: true,
            love: false,
        }
    }
}

impl Store {
    /// Whether a session key is present (the user has authorized an account).
    pub(crate) fn connected(&self) -> bool {
        !self.session_key.is_empty()
    }

    /// The API key and secret are both present, so an authorization can be started.
    pub(crate) fn has_credentials(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.secret.trim().is_empty()
    }

    /// Loads the file, or defaults when it is missing or unreadable.
    pub(crate) fn load() -> Store {
        let Some(path) = path("lastfm.json") else { return Store::default() };
        let Ok(text) = fs::read_to_string(&path) else { return Store::default() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Writes the file back, best-effort.
    pub(crate) fn save(&self) {
        let Some(path) = path("lastfm.json") else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_vec_pretty(self) else { return };
        write_private(&path, &text);
    }
}

/// One play waiting to be scrobbled, with the timestamp it started at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Scrobble {
    pub(crate) track: MediaTrack,
    /// Unix seconds the play started at (Last.fm's `timestamp`).
    pub(crate) at: u64,
    pub(crate) played_secs: f64,
}

#[derive(Serialize, Deserialize, Default)]
struct QueueStore {
    #[serde(default)]
    entries: Vec<Scrobble>,
}

/// The on-disk offline queue, retried with backoff while the network or Last.fm is failing.
#[derive(Debug, Default)]
pub(crate) struct PendingQueue {
    entries: Vec<Scrobble>,
    /// Consecutive failed flushes, for the backoff.
    failures: u32,
    /// The earliest time a flush may be attempted again.
    next_attempt: Option<f64>,
}

impl PendingQueue {
    /// Reads `lastfm_queue.json`, or an empty queue.
    pub(crate) fn load() -> PendingQueue {
        let Some(path) = path("lastfm_queue.json") else { return PendingQueue::default() };
        let Ok(bytes) = fs::read(&path) else { return PendingQueue::default() };
        let Ok(store) = serde_json::from_slice::<QueueStore>(&bytes) else { return PendingQueue::default() };
        PendingQueue { entries: store.entries, failures: 0, next_attempt: None }
    }

    /// Appends a play, dropping the oldest when the queue is full, and persists.
    pub(crate) fn push(&mut self, scrobble: Scrobble) {
        self.entries.push(scrobble);
        if self.entries.len() > QUEUE_MAX {
            let overflow = self.entries.len() - QUEUE_MAX;
            self.entries.drain(..overflow);
        }
        self.save();
    }

    /// The number of plays still waiting.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the backoff allows another attempt now.
    pub(crate) fn due(&self, now: f64) -> bool {
        !self.entries.is_empty() && self.next_attempt.is_none_or(|at| now >= at)
    }

    /// Writes the queue back, best-effort.
    pub(crate) fn save(&self) {
        let Some(path) = path("lastfm_queue.json") else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_vec(&QueueStore { entries: self.entries.clone() }) else { return };
        write_private(&path, &text);
    }

    /// Submits what it can, newest batches first. Entries Last.fm rejects with a
    /// permanently-invalid code are dropped; a transient failure stops the attempt and arms the
    /// backoff. Returns how many plays were accepted.
    pub(crate) fn flush(&mut self, api: &Api, store: &Store, now: f64) -> Result<usize, ApiError> {
        let mut accepted = 0;
        let mut failure = None;
        while !self.entries.is_empty() {
            let batch: Vec<Scrobble> = self.entries.iter().take(BATCH).cloned().collect();
            match api.scrobble(store, &batch) {
                Ok(count) => {
                    self.entries.drain(..batch.len());
                    accepted += count;
                }
                Err(error) if error.disposition() == ErrorDisposition::Retry => {
                    failure = Some(error);
                    break;
                }
                Err(_) if batch.len() == 1 => {
                    // A lone, permanently-invalid play is simply dropped.
                    self.entries.drain(..1);
                }
                Err(_) => {
                    // A permanently-invalid answer to the whole batch: retry one at a time so a
                    // single rejected play cannot take the rest of the batch with it.
                    let taken: Vec<Scrobble> = self.entries.drain(..batch.len()).collect();
                    let mut kept = Vec::new();
                    for entry in taken {
                        match api.scrobble(store, std::slice::from_ref(&entry)) {
                            Ok(count) => accepted += count,
                            Err(error) if error.disposition() == ErrorDisposition::Retry => {
                                kept.push(entry);
                                failure = Some(error);
                            }
                            Err(_) => {}
                        }
                    }
                    for (index, entry) in kept.into_iter().enumerate() {
                        self.entries.insert(index, entry);
                    }
                    break;
                }
            }
        }
        if failure.is_some() {
            self.failures = self.failures.saturating_add(1);
            self.next_attempt = Some(now + backoff_secs(self.failures));
        } else {
            self.failures = 0;
            self.next_attempt = None;
        }
        self.save();
        failure.map_or(Ok(accepted), Err)
    }
}

/// How long to wait before the next queue flush, doubling per consecutive failure.
pub(crate) fn backoff_secs(failures: u32) -> f64 {
    let shift = failures.saturating_sub(1).min(5);
    (BASE_BACKOFF_SECS * (1u32 << shift) as f64).min(MAX_BACKOFF_SECS)
}

/// `$XDG_CONFIG_HOME/tsun/<name>`, or `~/.config/tsun/<name>`.
fn path(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("tsun").join(name))
}

/// Writes `bytes` to `path` with `0600` permissions, best-effort.
fn write_private(path: &std::path::Path, bytes: &[u8]) {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o600);
    if let Ok(mut file) = options.open(path) {
        let _ = file.write_all(bytes);
    }
}

// ---------------------------------------------------------------------------------------------
// API signatures and requests

/// Last.fm's `api_sig`: the MD5 of every parameter's name and value concatenated in name order,
/// with the shared secret appended. `format` (and the unused `callback`) are excluded.
pub(crate) fn api_sig(params: &[(String, String)], secret: &str) -> String {
    let mut pairs: Vec<&(String, String)> = params
        .iter()
        .filter(|(name, _)| name != "format" && name != "callback" && name != "api_sig")
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut text = String::new();
    for (name, value) in pairs {
        text.push_str(name);
        text.push_str(value);
    }
    text.push_str(secret);
    format!("{:x}", md5::compute(text.as_bytes()))
}

/// Builds a signed POST body: the parameters, plus `api_key`, `method`, the signature and
/// `format=json`. The signature covers every parameter except `format`.
pub(crate) fn signed_form(
    method: &str,
    mut params: Vec<(String, String)>,
    api_key: &str,
    secret: &str,
) -> Vec<(String, String)> {
    params.push(("api_key".to_owned(), api_key.to_owned()));
    params.push(("method".to_owned(), method.to_owned()));
    let sig = api_sig(&params, secret);
    params.push(("api_sig".to_owned(), sig));
    params.push(("format".to_owned(), "json".to_owned()));
    params
}

/// What to do with a failed submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorDisposition {
    /// Keep it and try again later (network trouble, service offline, rate limit).
    Retry,
    /// Drop it: Last.fm will never accept it.
    Drop,
}

/// Last.fm error codes that mean the submission itself is permanently invalid: invalid service,
/// method, format, authentication or session, bad parameters, unsupported resource, already
/// acted on, the token was never authorized, and a suspended key.
const DROP_CODES: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 14, 17, 18, 26];

/// Classifies an API error code. Retryable codes are 11 (service offline), 16 (temporary error)
/// and 29 (rate limit); unknown codes retry rather than discard the user's plays.
pub(crate) fn classify(code: i64) -> ErrorDisposition {
    if DROP_CODES.contains(&code) {
        ErrorDisposition::Drop
    } else {
        ErrorDisposition::Retry
    }
}

/// A failed API call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApiError {
    /// The request never produced a usable answer (connection, timeout, 5xx).
    Network(String),
    /// Last.fm answered with an error code and message.
    Api { code: i64, message: String },
}

impl ApiError {
    pub(crate) fn disposition(&self) -> ErrorDisposition {
        match self {
            ApiError::Network(_) => ErrorDisposition::Retry,
            ApiError::Api { code, .. } => classify(*code),
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            ApiError::Network(message) => format!("Last.fm network error: {message}"),
            ApiError::Api { code, message } => format!("Last.fm error {code}: {message}"),
        }
    }
}

/// The blocking Last.fm client. Owned by the scrobble thread; `Api::with_base` points it at a
/// mock server for tests.
pub(crate) struct Api {
    agent: ureq::Agent,
    base: String,
}

impl Api {
    pub(crate) fn with_base(base: &str) -> Api {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .build();
        Api { agent: ureq::Agent::new_with_config(config), base: base.to_owned() }
    }

    fn call(&self, form: &[(String, String)]) -> Result<serde_json::Value, ApiError> {
        let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut response = self
            .agent
            .post(&self.base)
            .send_form(pairs)
            .map_err(|error| ApiError::Network(error.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| ApiError::Network(error.to_string()))?;
        if status == 429 {
            return Err(ApiError::Api { code: 29, message: "rate limit exceeded".to_owned() });
        }
        if status >= 500 {
            return Err(ApiError::Network(format!("Last.fm returned HTTP {status}")));
        }
        if !(200..300).contains(&status) {
            return Err(ApiError::Api { code: -1, message: format!("Last.fm returned HTTP {status}") });
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| ApiError::Network(format!("unreadable Last.fm response: {error}")))?;
        if let Some(code) = value.get("error").and_then(serde_json::Value::as_i64) {
            let message = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error")
                .to_owned();
            return Err(ApiError::Api { code, message });
        }
        Ok(value)
    }

    /// `auth.getToken`: the token the user authorizes in their browser.
    pub(crate) fn get_token(&self, api_key: &str, secret: &str) -> Result<String, ApiError> {
        let form = signed_form("auth.getToken", Vec::new(), api_key, secret);
        let value = self.call(&form)?;
        value
            .get("token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Network("Last.fm did not return a token".to_owned()))
    }

    /// `auth.getSession`: exchanges an authorized token for the session key and username.
    pub(crate) fn get_session(
        &self,
        api_key: &str,
        secret: &str,
        token: &str,
    ) -> Result<(String, String), ApiError> {
        let form = signed_form("auth.getSession", vec![("token".to_owned(), token.to_owned())], api_key, secret);
        let value = self.call(&form)?;
        let session = value
            .get("session")
            .ok_or_else(|| ApiError::Network("Last.fm did not return a session".to_owned()))?;
        let key = session.get("key").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let name = session.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        if key.is_empty() {
            return Err(ApiError::Network("Last.fm returned an empty session key".to_owned()));
        }
        Ok((key, name))
    }

    /// `track.updateNowPlaying` for the track that just started.
    pub(crate) fn update_now_playing(&self, store: &Store, track: &MediaTrack) -> Result<(), ApiError> {
        let mut form = signed_form("track.updateNowPlaying", track_params(track, None), &store.api_key, &store.secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        // Re-sign now that `sk` is present: Last.fm requires every parameter signed.
        resign(&mut form, &store.secret);
        self.call(&form).map(|_| ())
    }

    /// `track.scrobble` for up to [`BATCH`] plays; returns how many Last.fm accepted.
    pub(crate) fn scrobble(&self, store: &Store, batch: &[Scrobble]) -> Result<usize, ApiError> {
        let mut params = Vec::new();
        for (index, entry) in batch.iter().enumerate() {
            for (name, value) in track_params(&entry.track, Some(entry.at)) {
                params.push((format!("{name}[{index}]"), value));
            }
        }
        let mut form = signed_form("track.scrobble", params, &store.api_key, &store.secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        resign(&mut form, &store.secret);
        let value = self.call(&form)?;
        let accepted = value
            .pointer("/scrobbles/@attr/accepted")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(batch.len() as u64) as usize;
        Ok(accepted)
    }

    /// `track.love` / `track.unlove`.
    pub(crate) fn love(&self, store: &Store, title: &str, artist: &str, on: bool) -> Result<(), ApiError> {
        let method = if on { "track.love" } else { "track.unlove" };
        let params = vec![
            ("artist".to_owned(), artist.to_owned()),
            ("track".to_owned(), title.to_owned()),
        ];
        let mut form = signed_form(method, params, &store.api_key, &store.secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        resign(&mut form, &store.secret);
        self.call(&form).map(|_| ())
    }
}

/// The `track`/`artist`/`album`/`duration`/`timestamp` parameters for one play.
fn track_params(track: &MediaTrack, at: Option<u64>) -> Vec<(String, String)> {
    let mut params = vec![
        ("artist".to_owned(), track.artist.clone()),
        ("track".to_owned(), track.title.clone()),
    ];
    if !track.album.trim().is_empty() {
        params.push(("album".to_owned(), track.album.clone()));
    }
    if let Some(duration) = track.duration_secs.filter(|duration| *duration > 0) {
        params.push(("duration".to_owned(), duration.to_string()));
    }
    if let Some(at) = at {
        params.push(("timestamp".to_owned(), at.to_string()));
    }
    params
}

/// Re-computes `api_sig` in place after a parameter (the session key) was added.
fn resign(form: &mut Vec<(String, String)>, secret: &str) {
    form.retain(|(name, _)| name != "api_sig");
    let sig = api_sig(form, secret);
    form.push(("api_sig".to_owned(), sig));
}

// ---------------------------------------------------------------------------------------------
// The scrobble state machine

/// A play that has passed the scrobble threshold at finalize time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Completed {
    pub track: MediaTrack,
    pub at: u64,
    pub played_secs: f64,
}

/// What the tracker wants the thread to do after an event.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Actions {
    /// A `track.updateNowPlaying` to send.
    pub now_playing: Option<MediaTrack>,
    /// A finished play to scrobble.
    pub scrobble: Option<Completed>,
}

#[derive(Debug, Clone)]
struct Playing {
    track: MediaTrack,
    played: f64,
    started_at: u64,
}

/// Counts real played time from [`MediaEvent`]s. Pure: every event carries the current time in
/// seconds, so the thresholds are unit-tested against synthetic events and clock values.
#[derive(Debug, Default)]
pub(crate) struct Scrobbler {
    current: Option<Playing>,
    playing: bool,
    last_update: f64,
}

impl Scrobbler {
    pub(crate) fn new() -> Scrobbler {
        Scrobbler::default()
    }

    /// Folds one event in at wall-clock `now` (unix seconds) and returns what to send.
    pub(crate) fn on_event(&mut self, event: &MediaEvent, now: f64) -> Actions {
        let mut actions = Actions::default();
        // Real playing time up to this instant; a pause and a seek both land here so neither
        // adds to the count.
        if self.playing {
            if let Some(current) = &mut self.current {
                current.played += (now - self.last_update).max(0.0);
            }
        }
        match event {
            MediaEvent::Started { track, .. } => {
                actions.scrobble = self.finalize();
                self.current = Some(Playing { track: track.clone(), played: 0.0, started_at: now.max(0.0) as u64 });
                self.playing = true;
                if identifiable(track) {
                    actions.now_playing = Some(track.clone());
                }
            }
            MediaEvent::Paused { .. } => self.playing = false,
            MediaEvent::Resumed { .. } => self.playing = true,
            MediaEvent::Seeked { .. } => {}
            MediaEvent::Stopped | MediaEvent::Finished => {
                actions.scrobble = self.finalize();
                self.playing = false;
            }
        }
        self.last_update = now;
        actions
    }

    /// The play in progress, if it passed the threshold, taken out of the tracker.
    fn finalize(&mut self) -> Option<Completed> {
        let current = self.current.take()?;
        if !identifiable(&current.track) || !qualifies(current.track.duration_secs, current.played) {
            return None;
        }
        Some(Completed { track: current.track, at: current.started_at, played_secs: current.played })
    }
}

/// Whether a track carries what Last.fm needs (a title and an artist). A local file with no
/// artist tag is skipped rather than mis-scrobbled.
pub(crate) fn identifiable(track: &MediaTrack) -> bool {
    !track.title.trim().is_empty() && !track.artist.trim().is_empty()
}

/// The official threshold: longer than 30 s, and played for half its length or four minutes,
/// whichever comes first. An unknown length falls back to the four-minute rule.
pub(crate) fn qualifies(duration_secs: Option<u64>, played_secs: f64) -> bool {
    match duration_secs {
        Some(duration) if duration <= MIN_TRACK_SECS => false,
        Some(duration) => played_secs >= (duration as f64 * 0.5).min(MIN_PLAYED_SECS),
        None => played_secs >= MIN_PLAYED_SECS,
    }
}

// ---------------------------------------------------------------------------------------------
// Worker thread

/// Commands the UI sends to the scrobble thread.
pub(crate) enum Command {
    /// A playback event to fold into the scrobble state machine.
    Media(MediaEvent),
    /// Start authorization: fetch a token and report the URL to open.
    Connect,
    /// The user authorized in their browser: exchange the token for a session.
    ConfirmAuth,
    /// Forget the session key (the API key and secret are kept).
    Disconnect,
    /// Store the API key and shared secret the user pasted.
    SetCredentials { api_key: String, secret: String },
    /// Update the three toggles.
    SetPrefs { scrobble: bool, now_playing: bool, love: bool },
    /// Love or unlove a track on Last.fm.
    Love { title: String, artist: String, on: bool },
}

/// What the thread reports back to the UI.
#[derive(Debug)]
pub(crate) enum LastFmStatus {
    Disconnected,
    /// A token was fetched; the user must open this URL (which embeds the token) and authorize.
    Authorizing { url: String },
    Connected { username: String },
    /// Something went wrong; shown in the card.
    Error(String),
    /// An informational line (scrobbled, retrying, skipped).
    Notice(String),
}

/// The UI side of Last.fm: the channel to the scrobble thread, the form fields and the state the
/// card draws.
pub(crate) struct LastFm {
    cmd_tx: Sender<Command>,
    rx: Receiver<LastFmStatus>,
    /// The connection state shown in the card.
    pub(crate) conn: Connection,
    /// The pasted API key.
    pub(crate) key: TextEdit,
    /// The pasted shared secret (masked).
    pub(crate) secret: TextEdit,
    pub(crate) scrobble: bool,
    pub(crate) now_playing: bool,
    pub(crate) love: bool,
    /// The last status line, if any, and whether it is an error.
    pub(crate) message: Option<String>,
    pub(crate) error: bool,
}

/// The card's view of the connection.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Connection {
    Disconnected,
    /// Waiting for the user to authorize the token in their browser.
    Authorizing { url: String },
    Connected { username: String },
}

impl LastFm {
    /// Starts the scrobble thread, seeding the form from the saved store.
    pub(crate) fn start(waker: Waker) -> LastFm {
        let store = Store::load();
        let conn = if store.connected() {
            Connection::Connected { username: store.username.clone() }
        } else {
            Connection::Disconnected
        };
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let base = std::env::var("TSUN_LASTFM_API_BASE").unwrap_or_else(|_| API_BASE.to_owned());
        let prefs = (store.scrobble, store.now_playing, store.love);
        // The form starts from what was saved: the key and secret prefill, and the toggles show
        // their persisted state.
        let (key, secret) = (store.api_key.clone(), store.secret.clone());
        thread::spawn(move || run(store, base, cmd_rx, status_tx, waker));
        LastFm {
            cmd_tx,
            rx: status_rx,
            conn,
            key: TextEdit::new(key),
            secret: TextEdit::new(secret),
            scrobble: prefs.0,
            now_playing: prefs.1,
            love: prefs.2,
            message: None,
            error: false,
        }
    }

    /// Forwards one playback event to the thread. Never blocks.
    pub(crate) fn handle(&self, event: MediaEvent) {
        let _ = self.cmd_tx.send(Command::Media(event));
    }

    /// Whether anything would actually be sent: a session exists and a toggle is on.
    pub(crate) fn enabled(&self) -> bool {
        matches!(self.conn, Connection::Connected { .. }) && (self.scrobble || self.now_playing)
    }

    /// A client for UI tests: the same state, but no thread, no filesystem and no network.
    #[cfg(test)]
    pub(crate) fn for_test(store: &Store) -> LastFm {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_status_tx, status_rx) = mpsc::channel();
        let conn = if store.connected() {
            Connection::Connected { username: store.username.clone() }
        } else {
            Connection::Disconnected
        };
        LastFm {
            cmd_tx,
            rx: status_rx,
            conn,
            key: TextEdit::new(store.api_key.clone()),
            secret: TextEdit::new(store.secret.clone()),
            scrobble: store.scrobble,
            now_playing: store.now_playing,
            love: store.love,
            message: None,
            error: false,
        }
    }

    /// Applies one status from the thread to the card's state. Returns whether it changed.
    fn apply(&mut self, status: LastFmStatus) -> bool {
        match status {
            LastFmStatus::Disconnected => {
                self.conn = Connection::Disconnected;
                self.message = Some("Disconnected from Last.fm".to_owned());
                self.error = false;
            }
            LastFmStatus::Authorizing { url } => {
                self.conn = Connection::Authorizing { url };
                self.message = Some("Authorize tsun in your browser, then confirm.".to_owned());
                self.error = false;
            }
            LastFmStatus::Connected { username } => {
                self.conn = Connection::Connected { username };
                self.message = Some("Connected to Last.fm".to_owned());
                self.error = false;
            }
            LastFmStatus::Error(message) => {
                self.message = Some(message);
                self.error = true;
            }
            LastFmStatus::Notice(message) => {
                self.message = Some(message);
                self.error = false;
            }
        }
        true
    }
}

/// The scrobble thread: owns the store, the tracker, the offline queue and the HTTP client.
fn run(store: Store, base: String, cmd_rx: Receiver<Command>, tx: Sender<LastFmStatus>, waker: Waker) {
    let api = Api::with_base(&base);
    let mut store = store;
    let mut tracker = Scrobbler::new();
    let mut queue = PendingQueue::load();
    let mut pending_token: Option<String> = None;
    let mut warned_retry = false;
    let report = |status: LastFmStatus| {
        let _ = tx.send(status);
        waker.as_ref()();
    };
    // Announce the restored state so the card matches the file.
    if store.connected() {
        report(LastFmStatus::Connected { username: store.username.clone() });
    }
    // A queue left from an earlier run is retried as soon as the network allows.
    if store.connected() && !queue.is_empty() {
        drain_queue(&api, &mut store, &mut queue, &report, &mut warned_retry);
    }
    loop {
        match cmd_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(command) => match command {
                Command::Media(event) => {
                    let now = unix_secs();
                    let actions = tracker.on_event(&event, now);
                    if let Some(track) = actions.now_playing {
                        if store.now_playing && store.connected() {
                            if let Err(error) = api.update_now_playing(&store, &track) {
                                if error.disposition() == ErrorDisposition::Drop {
                                    report(LastFmStatus::Error(error.message()));
                                }
                            }
                        }
                    }
                    if let Some(completed) = actions.scrobble {
                        if store.scrobble {
                            queue.push(Scrobble {
                                track: completed.track,
                                at: completed.at,
                                played_secs: completed.played_secs,
                            });
                        }
                    }
                    if store.connected() && queue.due(now) {
                        drain_queue(&api, &mut store, &mut queue, &report, &mut warned_retry);
                    }
                }
                Command::Connect => {
                    if !store.has_credentials() {
                        report(LastFmStatus::Error("Enter your Last.fm API key and shared secret first".to_owned()));
                        continue;
                    }
                    match api.get_token(&store.api_key, &store.secret) {
                        Ok(token) => {
                            let url = format!(
                                "{AUTH_URL}?api_key={}&token={token}",
                                urlencode(&store.api_key)
                            );
                            pending_token = Some(token);
                            report(LastFmStatus::Authorizing { url });
                        }
                        Err(error) => report(LastFmStatus::Error(error.message())),
                    }
                }
                Command::ConfirmAuth => {
                    let Some(token) = pending_token.clone() else {
                        report(LastFmStatus::Error("Press Connect first".to_owned()));
                        continue;
                    };
                    match api.get_session(&store.api_key, &store.secret, &token) {
                        Ok((key, name)) => {
                            store.session_key = key;
                            store.username = name.clone();
                            store.save();
                            pending_token = None;
                            report(LastFmStatus::Connected { username: name });
                            let now = unix_secs();
                            if queue.due(now) {
                                drain_queue(&api, &mut store, &mut queue, &report, &mut warned_retry);
                            }
                        }
                        Err(error) => report(LastFmStatus::Error(error.message())),
                    }
                }
                Command::Disconnect => {
                    store.session_key.clear();
                    store.username.clear();
                    store.save();
                    pending_token = None;
                    report(LastFmStatus::Disconnected);
                }
                Command::SetCredentials { api_key, secret } => {
                    store.api_key = api_key;
                    store.secret = secret;
                    store.save();
                }
                Command::SetPrefs { scrobble, now_playing, love } => {
                    store.scrobble = scrobble;
                    store.now_playing = now_playing;
                    store.love = love;
                    store.save();
                }
                Command::Love { title, artist, on } => {
                    if store.love && store.connected() {
                        if let Err(error) = api.love(&store, &title, &artist, on) {
                            if error.disposition() == ErrorDisposition::Drop {
                                report(LastFmStatus::Error(error.message()));
                            }
                        }
                    }
                }
            },
            Err(RecvTimeoutError::Timeout) => {
                let now = unix_secs();
                if store.connected() && queue.due(now) {
                    drain_queue(&api, &mut store, &mut queue, &report, &mut warned_retry);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Flushes the queue and reports the outcome once per failure streak.
fn drain_queue(
    api: &Api,
    store: &mut Store,
    queue: &mut PendingQueue,
    report: &impl Fn(LastFmStatus),
    warned_retry: &mut bool,
) {
    let now = unix_secs();
    match queue.flush(api, store, now) {
        Ok(count) if count > 0 => {
            *warned_retry = false;
            report(LastFmStatus::Notice(format!("Scrobbled {count} play(s) to Last.fm")));
        }
        Ok(_) => {}
        Err(_) => {
            if !*warned_retry {
                *warned_retry = true;
                report(LastFmStatus::Notice(format!(
                    "{} play(s) will be scrobbled when Last.fm is reachable again",
                    queue.len()
                )));
            }
        }
    }
}

/// Percent-encodes a value for a query string (the API key is hex in practice, but this keeps
/// the URL valid for any key).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Unix seconds as a float, the tracker's clock.
fn unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------------------------
// UI integration

impl Ui {
    /// Drains the scrobble thread's statuses into the card's state. Called from `poll`.
    pub(crate) fn poll_lastfm(&mut self) -> bool {
        let Some(lastfm) = &mut self.lastfm else { return false };
        let mut changed = false;
        loop {
            match lastfm.rx.try_recv() {
                Ok(status) => {
                    changed |= lastfm.apply(status);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// Sends the pasted API key and secret to the thread.
    pub(crate) fn lastfm_save_credentials(&mut self) {
        let Some(lastfm) = &self.lastfm else { return };
        let _ = lastfm.cmd_tx.send(Command::SetCredentials {
            api_key: lastfm.key.text.trim().to_owned(),
            secret: lastfm.secret.text.trim().to_owned(),
        });
    }

    /// Starts the authorization flow (fetch a token, then let the user open the URL).
    pub(crate) fn lastfm_connect(&mut self) {
        self.lastfm_save_credentials();
        if let Some(lastfm) = &mut self.lastfm {
            lastfm.message = Some("Contacting Last.fm…".to_owned());
            lastfm.error = false;
            let _ = lastfm.cmd_tx.send(Command::Connect);
        }
    }

    /// Completes the authorization after the user approved the token.
    pub(crate) fn lastfm_confirm(&mut self) {
        if let Some(lastfm) = &mut self.lastfm {
            lastfm.message = Some("Finishing sign-in…".to_owned());
            lastfm.error = false;
            let _ = lastfm.cmd_tx.send(Command::ConfirmAuth);
        }
    }

    /// Forgets the session key while keeping the API key and secret.
    pub(crate) fn lastfm_disconnect(&mut self) {
        if let Some(lastfm) = &self.lastfm {
            let _ = lastfm.cmd_tx.send(Command::Disconnect);
        }
    }

    /// Flips one of the three Last.fm toggles and pushes the change to the thread.
    pub(crate) fn lastfm_set_pref(&mut self, which: LastFmPref) {
        let Some(lastfm) = &mut self.lastfm else { return };
        match which {
            LastFmPref::Scrobble => lastfm.scrobble = !lastfm.scrobble,
            LastFmPref::NowPlaying => lastfm.now_playing = !lastfm.now_playing,
            LastFmPref::Love => lastfm.love = !lastfm.love,
        }
        let command = Command::SetPrefs {
            scrobble: lastfm.scrobble,
            now_playing: lastfm.now_playing,
            love: lastfm.love,
        };
        let _ = lastfm.cmd_tx.send(command);
    }

    /// Mirrors a TIDAL favourite onto Last.fm (love/unlove), when the toggle is on. Only called
    /// for TIDAL tracks: a local file is never a TIDAL favourite and carries no Last.fm love.
    pub(crate) fn lastfm_love(&mut self, track_id: u64, on: bool) {
        // Local files are never TIDAL favourites and have no Last.fm love of their own.
        if crate::local::is_local_id(track_id) {
            return;
        }
        let Some(lastfm) = &self.lastfm else { return };
        if !lastfm.love || !matches!(lastfm.conn, Connection::Connected { .. }) {
            return;
        }
        let Some((title, artist)) = self.track_artist_title(track_id) else { return };
        let _ = lastfm.cmd_tx.send(Command::Love { title, artist, on });
    }

    /// The title and artist of a TIDAL track by id, from whatever list holds it.
    fn track_artist_title(&self, id: u64) -> Option<(String, String)> {
        if let Some(playing) = &self.now_playing {
            if playing.id == id && !crate::local::is_local_id(playing.id) {
                return Some((playing.title.clone(), playing.artist.clone()));
            }
        }
        let mut candidates: Vec<&crate::tidal::Track> = Vec::new();
        candidates.extend(self.queue.tracks().iter());
        candidates.extend(self.results.items());
        candidates.extend(self.home_tracks.iter());
        candidates.extend(self.playlist_tracks.items());
        candidates.extend(self.mix_tracks.items());
        candidates.extend(self.album_tracks.items());
        if let Some(library) = &self.library {
            candidates.extend(library.tracks.iter());
        }
        if let Some(page) = &self.artist_page {
            candidates.extend(page.top_tracks.iter());
        }
        candidates
            .into_iter()
            .find(|track| track.id == id)
            .map(|track| (track.title.clone(), track.artist.clone()))
    }
}

/// One of the Last.fm toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LastFmPref {
    Scrobble,
    NowPlaying,
    Love,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn track(title: &str, artist: &str, duration: Option<u64>) -> MediaTrack {
        MediaTrack {
            id: "42".to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            album: "Album".to_owned(),
            duration_secs: duration,
            cover_url: None,
            local: false,
        }
    }

    fn started(track: MediaTrack) -> MediaEvent {
        MediaEvent::Started { track, position_secs: 0.0 }
    }

    // -----------------------------------------------------------------------------------------
    // Signature, request building, error classification

    #[test]
    fn api_sig_matches_the_documented_example() {
        // The string Last.fm's signature documentation works through, computed independently
        // with `printf 'api_keyabcdefmethodauth.getTokens3cr3t' | md5sum`.
        assert_eq!(
            "391f9bb38b019a4f7bc08baa15697390",
            api_sig(
                &[
                    ("method".to_owned(), "auth.getToken".to_owned()),
                    ("api_key".to_owned(), "abcdef".to_owned()),
                ],
                "s3cr3t",
            ),
        );
    }

    #[test]
    fn api_sig_sorts_by_name_and_excludes_format() {
        let with_format = api_sig(
            &[
                ("method".to_owned(), "auth.getToken".to_owned()),
                ("format".to_owned(), "json".to_owned()),
                ("api_key".to_owned(), "abcdef".to_owned()),
            ],
            "s3cr3t",
        );
        let without = api_sig(
            &[
                ("api_key".to_owned(), "abcdef".to_owned()),
                ("method".to_owned(), "auth.getToken".to_owned()),
            ],
            "s3cr3t",
        );
        assert_eq!(with_format, without);
    }

    #[test]
    fn signed_form_carries_key_method_signature_and_json() {
        let form = signed_form("track.love", vec![("artist".into(), "A".into()), ("track".into(), "T".into())], "key", "sec");
        let get = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        assert_eq!(get("api_key"), Some("key"));
        assert_eq!(get("method"), Some("track.love"));
        assert_eq!(get("format"), Some("json"));
        // The signature covers artist, track, api_key and method, plus the secret.
        let expected = api_sig(
            &[
                ("artist".to_owned(), "A".to_owned()),
                ("track".to_owned(), "T".to_owned()),
                ("api_key".to_owned(), "key".to_owned()),
                ("method".to_owned(), "track.love".to_owned()),
            ],
            "sec",
        );
        assert_eq!(get("api_sig"), Some(expected.as_str()));
    }

    #[test]
    fn a_missing_session_key_is_signed_in() {
        let store = Store { api_key: "k".into(), secret: "s".into(), session_key: "sk".into(), ..Store::default() };
        let mut form = signed_form("track.updateNowPlaying", vec![("artist".into(), "A".into())], &store.api_key, &store.secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        resign(&mut form, &store.secret);
        let sig = form.iter().find(|(k, _)| k == "api_sig").unwrap().1.clone();
        let expected = api_sig(
            &[
                ("artist".to_owned(), "A".to_owned()),
                ("api_key".to_owned(), "k".to_owned()),
                ("method".to_owned(), "track.updateNowPlaying".to_owned()),
                ("sk".to_owned(), "sk".to_owned()),
            ],
            "s",
        );
        assert_eq!(sig, expected);
    }

    #[test]
    fn error_codes_classify_per_the_docs() {
        for code in [4, 6, 7, 13, 14, 26] {
            assert_eq!(classify(code), ErrorDisposition::Drop, "code {code}");
        }
        for code in [11, 16, 29] {
            assert_eq!(classify(code), ErrorDisposition::Retry, "code {code}");
        }
        // An unknown code keeps the play rather than losing it.
        assert_eq!(classify(99), ErrorDisposition::Retry);
        assert_eq!(ApiError::Network("x".into()).disposition(), ErrorDisposition::Retry);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff_secs(1), 30.0);
        assert_eq!(backoff_secs(2), 60.0);
        assert!(backoff_secs(20) <= MAX_BACKOFF_SECS);
    }

    // -----------------------------------------------------------------------------------------
    // Scrobble threshold state machine

    #[test]
    fn a_31_second_track_scrobbles_at_its_halfway_point() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(31))), 100.0);
        // 15 s in: below 50% of 31 s.
        let early = tracker.on_event(&MediaEvent::Stopped, 115.0);
        assert!(early.scrobble.is_none());
    }

    #[test]
    fn a_31_second_track_scrobbled_when_it_passed_half() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(31))), 100.0);
        let done = tracker.on_event(&MediaEvent::Finished, 116.0);
        let completed = done.scrobble.expect("past 50% of 31 s");
        assert_eq!(completed.track.title, "T");
        assert_eq!(completed.at, 100);
        assert!((completed.played_secs - 16.0).abs() < 0.001);
    }

    #[test]
    fn a_29_second_track_never_scrobbles() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(29))), 100.0);
        let done = tracker.on_event(&MediaEvent::Finished, 200.0);
        assert!(done.scrobble.is_none());
    }

    #[test]
    fn a_long_track_needs_four_minutes_even_at_ten_percent() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(3600))), 0.0);
        // 50% would be 30 minutes; four minutes is sooner.
        let early = tracker.on_event(&MediaEvent::Stopped, 239.0);
        assert!(early.scrobble.is_none());
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(3600))), 0.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 240.0);
        assert!(done.scrobble.is_some());
    }

    #[test]
    fn paused_time_does_not_count() {
        // A 200 s track needs 100 s of real playing.
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(200))), 0.0);
        tracker.on_event(&MediaEvent::Paused { position_secs: 10.0 }, 10.0);
        // 5 minutes paused, then 40 s more of real playing: 50 s total, below half.
        tracker.on_event(&MediaEvent::Resumed { position_secs: 10.0 }, 310.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 350.0);
        assert!(done.scrobble.is_none(), "only 50 s of the 200 s track played");
        // Real playing now adds up to 110 s, past the halfway point.
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(200))), 0.0);
        tracker.on_event(&MediaEvent::Paused { position_secs: 10.0 }, 10.0);
        tracker.on_event(&MediaEvent::Resumed { position_secs: 10.0 }, 310.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 410.0);
        assert!(done.scrobble.is_some());
    }

    #[test]
    fn seeking_does_not_add_played_time() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(300))), 0.0);
        // Jump to the last 10 s after only 20 s of real listening.
        tracker.on_event(&MediaEvent::Seeked { position_secs: 290.0 }, 20.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 30.0);
        assert!(done.scrobble.is_none(), "only 30 s of real playing");
    }

    #[test]
    fn a_new_track_finalizes_the_previous_one() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("One", "A", Some(60))), 0.0);
        // 40 s of a 60 s track is past half: starting the next one scrobbles it.
        let actions = tracker.on_event(&started(track("Two", "B", Some(60))), 40.0);
        assert_eq!(actions.scrobble.expect("40 s past half of 60").track.title, "One");
        assert_eq!(actions.now_playing.unwrap().title, "Two");
    }

    #[test]
    fn repeat_one_scrobbles_again() {
        let mut tracker = Scrobbler::new();
        tracker.on_event(&started(track("T", "A", Some(40))), 0.0);
        let first = tracker.on_event(&MediaEvent::Finished, 40.0);
        assert!(first.scrobble.is_some());
        tracker.on_event(&started(track("T", "A", Some(40))), 41.0);
        let second = tracker.on_event(&MediaEvent::Finished, 81.0);
        let replay = second.scrobble.expect("a repeat counts as another play");
        assert_eq!(replay.at, 41);
    }

    #[test]
    fn a_track_without_an_artist_is_not_scrobbled_or_announced() {
        let mut tracker = Scrobbler::new();
        let actions = tracker.on_event(&started(track("T", "", Some(60))), 0.0);
        assert!(actions.now_playing.is_none());
        let done = tracker.on_event(&MediaEvent::Finished, 120.0);
        assert!(done.scrobble.is_none());
    }

    #[test]
    fn an_unknown_length_uses_the_four_minute_rule() {
        assert!(!qualifies(None, 239.0));
        assert!(qualifies(None, 240.0));
        assert!(!qualifies(Some(29), 1000.0));
    }

    // -----------------------------------------------------------------------------------------
    // Queue persistence and retry

    #[test]
    fn queue_round_trips_through_json() {
        let text = serde_json::to_string(&QueueStore {
            entries: vec![Scrobble { track: track("T", "A", Some(60)), at: 1000, played_secs: 45.0 }],
        })
        .unwrap();
        let back: QueueStore = serde_json::from_str(&text).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].at, 1000);
        assert_eq!(back.entries[0].track.artist, "A");
    }

    // -----------------------------------------------------------------------------------------
    // Mock HTTP server

    /// Percent-decodes an `application/x-www-form-urlencoded` field.
    fn urldecode(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'+' => {
                    out.push(b' ');
                    index += 1;
                }
                b'%' if index + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                        .ok()
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                    match hex {
                        Some(byte) => {
                            out.push(byte);
                            index += 3;
                        }
                        None => {
                            out.push(bytes[index]);
                            index += 1;
                        }
                    }
                }
                byte => {
                    out.push(byte);
                    index += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A tiny server answering one scripted response per request, then closing.
    struct Mock {
        base: String,
        seen: Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
        requests: Arc<AtomicUsize>,
    }

    impl Mock {
        fn start(responses: Vec<(u16, String)>) -> Mock {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let requests = Arc::new(AtomicUsize::new(0));
            let seen_thread = seen.clone();
            let count_thread = requests.clone();
            thread::spawn(move || {
                for (status, body) in responses {
                    let Ok((mut stream, _)) = listener.accept() else { break };
                    count_thread.fetch_add(1, Ordering::SeqCst);
                    // Read the request line and headers, then the body by Content-Length.
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut params = Vec::new();
                    let mut length = 0usize;
                    let mut line = String::new();
                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                        let trimmed = line.trim_end().to_owned();
                        if let Some((name, value)) = trimmed.split_once(':')
                            && name.eq_ignore_ascii_case("Content-Length")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                        if trimmed.is_empty() {
                            break;
                        }
                        line.clear();
                    }
                    if length > 0 {
                        let mut body = vec![0u8; length];
                        if reader.read_exact(&mut body).is_ok() {
                            let text = String::from_utf8_lossy(&body);
                            for pair in text.split('&') {
                                if let Some((key, value)) = pair.split_once('=') {
                                    params.push((urldecode(key), urldecode(value)));
                                }
                            }
                        }
                    }
                    seen_thread.lock().unwrap().push(params);
                    let reason = if status == 200 { "OK" } else { "Error" };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });
            Mock { base: format!("http://{addr}/"), seen, requests }
        }

        fn param(&self, request: usize, name: &str) -> Option<String> {
            let seen = self.seen.lock().unwrap();
            seen.get(request)?.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn full_flow_against_a_mock_server() {
        let mock = Mock::start(vec![
            (200, r#"{"token":"TOK"}"#.to_owned()),
            (200, r#"{"session":{"name":"tester","key":"SESS","subscriber":"0"}}"#.to_owned()),
            (200, r##"{"nowplaying":{"artist":{"#text":"A"}}}"##.to_owned()),
            (200, r#"{"scrobbles":{"@attr":{"accepted":2,"ignored":0}}}"#.to_owned()),
        ]);
        let api = Api::with_base(&mock.base);
        let token = api.get_token("key", "sec").unwrap();
        assert_eq!(token, "TOK");
        let (session, name) = api.get_session("key", "sec", &token).unwrap();
        assert_eq!((session.as_str(), name.as_str()), ("SESS", "tester"));
        let store = Store {
            api_key: "key".into(),
            secret: "sec".into(),
            session_key: session.clone(),
            username: name.clone(),
            ..Store::default()
        };
        api.update_now_playing(&store, &track("T", "A", Some(120))).unwrap();
        let batch = vec![
            Scrobble { track: track("T1", "A", Some(120)), at: 100, played_secs: 90.0 },
            Scrobble { track: track("T2", "B", Some(180)), at: 300, played_secs: 120.0 },
        ];
        assert_eq!(api.scrobble(&store, &batch).unwrap(), 2);

        // The scrobble request carried the indexed parameters and a valid signature.
        assert_eq!(mock.param(3, "artist[1]").as_deref(), Some("B"));
        assert_eq!(mock.param(3, "timestamp[0]").as_deref(), Some("100"));
        assert!(mock.param(3, "api_sig").is_some());
        assert_eq!(mock.param(2, "sk").as_deref(), Some("SESS"));
        assert_eq!(mock.requests.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn the_queue_retries_after_a_server_error() {
        let mock = Mock::start(vec![
            (500, "server error".to_owned()),
            (200, r#"{"scrobbles":{"@attr":{"accepted":1,"ignored":0}}}"#.to_owned()),
        ]);
        let api = Api::with_base(&mock.base);
        let store = Store { api_key: "k".into(), secret: "s".into(), session_key: "sk".into(), ..Store::default() };
        let mut queue = PendingQueue { entries: Vec::new(), failures: 0, next_attempt: None };
        queue.push(Scrobble { track: track("T", "A", Some(120)), at: 100, played_secs: 90.0 });

        // First attempt: the mock answers 500, so the play stays and the backoff is armed.
        let error = queue.flush(&api, &store, 10.0).unwrap_err();
        assert_eq!(error.disposition(), ErrorDisposition::Retry);
        assert_eq!(queue.len(), 1);
        assert!(!queue.due(11.0), "the backoff holds it back");
        assert!(queue.due(10.0 + backoff_secs(1)));

        // Once the backoff elapses the play goes through and the queue empties.
        assert_eq!(queue.flush(&api, &store, 100.0).unwrap(), 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn the_queue_drops_a_permanently_invalid_play() {
        let mock = Mock::start(vec![(200, r#"{"error":6,"message":"Invalid parameters"}"#.to_owned())]);
        let api = Api::with_base(&mock.base);
        let store = Store { api_key: "k".into(), secret: "s".into(), session_key: "sk".into(), ..Store::default() };
        let mut queue = PendingQueue { entries: Vec::new(), failures: 0, next_attempt: None };
        queue.push(Scrobble { track: track("T", "A", Some(120)), at: 100, played_secs: 90.0 });
        // The single entry is retried on its own and, still invalid, dropped.
        assert!(queue.flush(&api, &store, 0.0).is_ok());
        assert!(queue.is_empty());
    }
}
