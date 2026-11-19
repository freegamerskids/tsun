//! Scrobbling to Last.fm and to Last.fm-compatible services.
//!
//! The protocol is Last.fm's 2.0 API: a signed `application/x-www-form-urlencoded` POST, its
//! JSON answers and its error codes. One engine speaks it for every [`Service`] tsun supports —
//! Last.fm itself, and [scrobble.now](https://scrobble.now), a Last.fm-compatible scrobbling
//! service — so a service is a descriptor (endpoints, files, credentials), not a second
//! implementation. Each service has its own [`Store`], its own offline queue, its own thread and
//! its own card in Settings, and both can run at the same time.
//!
//! The **scrobbling rules** are the official ones: a play is sent once when it has actually
//! played for at least half its length or four minutes (whichever comes first), the track is
//! longer than 30 seconds; real played time is counted, so pausing stops the clock and seeking
//! neither adds nor removes time. `track.updateNowPlaying` is sent when a track starts, and a
//! played-so-far track is scrobbled when it ends, is skipped or the transport stops.
//!
//! Everything that touches the network runs on a dedicated thread per service so the UI never
//! blocks: the UI sends [`Command`]s and drains [`Status`]es in `poll`. Failed submissions go to
//! that service's on-disk queue (`lastfm_queue.json`, `scrobblenow_queue.json`) and are retried
//! with backoff; submissions the service rejects as permanently invalid are dropped instead of
//! being retried forever.
//!
//! Credentials: Last.fm needs the user's own API key and shared secret (from
//! <https://www.last.fm/api/account/create>), stored in `lastfm.json` (0600) beside the rest of
//! tsun's config, never in `settings.json`. scrobble.now does not gate API keys, so tsun ships
//! one for it (see [`SCROBBLENOW`]) and its card is just Connect.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rune::{TextEdit, Waker};
use serde::{Deserialize, Serialize};

use crate::app::Ui;
use crate::media_events::{MediaEvent, MediaTrack};

/// `track.scrobble` accepts up to 50 submissions per request.
pub(crate) const BATCH: usize = 50;
/// The offline queue never grows past this many plays; the oldest fall off.
const QUEUE_MAX: usize = 1000;
/// A track shorter than this is never scrobbled.
const MIN_TRACK_SECS: u64 = 30;
/// A play must reach half the track or this many seconds, whichever comes first.
const MIN_PLAYED_SECS: f64 = 240.0;

/// Credentials tsun ships for a service that does not gate API keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub(crate) api_key: &'static str,
    pub(crate) secret: &'static str,
}

/// One Last.fm-compatible scrobbling service.
///
/// Everything that differs between the services tsun speaks lives here; the engine ([`Api`],
/// [`Store`], [`PendingQueue`], [`Scrobbler`]) takes it as a parameter.
#[derive(Debug)]
pub(crate) struct Service {
    /// The stable identity used in tests ("lastfm", "scrobblenow").
    pub(crate) id: &'static str,
    /// The name shown to the user ("Last.fm", "scrobble.now").
    pub(crate) name: &'static str,
    /// The Last.fm 2.0 API endpoint.
    pub(crate) api_base: &'static str,
    /// Where the user authorizes a token in their browser.
    pub(crate) auth_url: &'static str,
    /// The environment variable a test points at a mock server instead of `api_base`; the app
    /// never sets it.
    pub(crate) api_base_env: &'static str,
    /// `$XDG_CONFIG_HOME/tsun/<store_file>` (0600): credentials, session and the switches.
    pub(crate) store_file: &'static str,
    /// The on-disk offline queue beside it.
    pub(crate) queue_file: &'static str,
    /// Credentials tsun ships. `None` means the user registers an application and pastes its
    /// key and secret (Last.fm).
    pub(crate) builtin: Option<Credentials>,
    /// The card's one-line description of what the service does.
    pub(crate) description: &'static str,
    /// The card's hint while disconnected: where the credentials come from.
    pub(crate) connect_hint: &'static str,
    /// The card's hint under the credential fields (Last.fm only).
    pub(crate) credentials_help: &'static str,
}

/// Last.fm: the original, and still the one with the strongest rules.
pub(crate) const LASTFM: Service = Service {
    id: "lastfm",
    name: "Last.fm",
    api_base: "https://ws.audioscrobbler.com/2.0/",
    auth_url: "https://www.last.fm/api/auth/",
    api_base_env: "TSUN_LASTFM_API_BASE",
    store_file: "lastfm.json",
    queue_file: "lastfm_queue.json",
    builtin: None,
    description: "Send what you play to your Last.fm account.",
    connect_hint: "Add your own Last.fm API key and shared secret, then press Connect.",
    credentials_help: "tsun ships no API key: register an application at last.fm/api/account/create and paste its API key and shared secret here. They are stored in lastfm.json (0600), never in settings.json.",
};

/// scrobble.now (<https://scrobble.now>): a Last.fm-compatible scrobbling service. It answers
/// the Last.fm 2.0 API under `/2.0/` and authorizes through its own page at `/api/auth/`.
///
/// It does not gate API keys — `auth.getToken` answers any key — so tsun ships its own pair and
/// the card is just Connect. Neither value is a secret in the usual sense: the key is public,
/// and the signature secret is only a fixed string that keeps the protocol's MD5 signature
/// (every parameter plus the secret, sorted by name) intact. Nothing here can be locked out or
/// revoked, and nothing is shared with any other service.
pub(crate) const SCROBBLENOW: Service = Service {
    id: "scrobblenow",
    name: "scrobble.now",
    api_base: "https://scrobble.now/2.0/",
    auth_url: "https://scrobble.now/api/auth/",
    api_base_env: "TSUN_SCROBBLENOW_API_BASE",
    store_file: "scrobblenow.json",
    queue_file: "scrobblenow_queue.json",
    builtin: Some(Credentials { api_key: "tsun", secret: "0b7f4d2a9c6e1f3a8d5b7c9e2f4a6d81" }),
    description: "Send what you play to scrobble.now, a Last.fm-compatible scrobbling service.",
    connect_hint: "Press Connect and approve tsun in your browser — no API key is needed.",
    credentials_help: "tsun connects to scrobble.now under its own application, so there is nothing to paste here.",
};

/// Every service tsun scrobbles to, in the order Settings lists them.
pub(crate) const SERVICES: [&Service; 2] = [&LASTFM, &SCROBBLENOW];


/// When a play counts as a scrobble: the track must be longer than `min_track_secs` and must
/// have played for half its length or `max_played_secs`, whichever comes first.
///
/// [`Thresholds::from_env`] exists for the tests alone: a generated test file is a second long
/// and a null output device plays it faster than real time, so the official rule could never be
/// reached there. The app never sets those variables, so it always scrobbles by the real rule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Thresholds {
    pub(crate) min_track_secs: u64,
    pub(crate) max_played_secs: f64,
}

impl Thresholds {
    /// The rule Last.fm documents.
    pub(crate) const OFFICIAL: Thresholds =
        Thresholds { min_track_secs: MIN_TRACK_SECS, max_played_secs: MIN_PLAYED_SECS };

    /// [`Thresholds::OFFICIAL`], overridable per variable for a test (the app never sets them).
    pub(crate) fn from_env() -> Thresholds {
        let number = |name: &str| std::env::var(name).ok().and_then(|value| value.trim().parse::<f64>().ok());
        Thresholds {
            min_track_secs: number("TSUN_SCROBBLE_MIN_TRACK_SECS")
                .map(|secs| secs.max(0.0) as u64)
                .unwrap_or(MIN_TRACK_SECS),
            max_played_secs: number("TSUN_SCROBBLE_MIN_PLAYED_SECS")
                .map(|secs| secs.max(0.0))
                .unwrap_or(MIN_PLAYED_SECS),
        }
    }
}
/// First retry delay for the offline queue, doubling up to [`MAX_BACKOFF_SECS`].
const BASE_BACKOFF_SECS: f64 = 30.0;
const MAX_BACKOFF_SECS: f64 = 900.0;

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------------------------
// Persisted state

/// What tsun keeps for one scrobbling service: the API credentials, the authorized session and
/// the feature toggles. Stored at `$XDG_CONFIG_HOME/tsun/<service.store_file>` (0600), and one
/// file per service, so connecting both keeps them independent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Store {
    /// The user's own API key (Last.fm). A service with [`Service::builtin`] ignores it.
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

    /// The key and secret to sign with: the service's built-in pair when it ships one, the
    /// user's own otherwise.
    pub(crate) fn credentials<'a>(&'a self, service: &'static Service) -> (&'a str, &'a str) {
        match &service.builtin {
            Some(builtin) => (builtin.api_key, builtin.secret),
            None => (self.api_key.trim(), self.secret.trim()),
        }
    }

    /// Whether the service can be authorized: a built-in pair always can, otherwise the user's
    /// key and secret must both be present.
    pub(crate) fn has_credentials(&self, service: &'static Service) -> bool {
        let (api_key, secret) = self.credentials(service);
        !api_key.is_empty() && !secret.is_empty()
    }

    /// Loads the service's file, or defaults when it is missing or unreadable.
    pub(crate) fn load(service: &'static Service) -> Store {
        let Some(path) = path(service.store_file) else { return Store::default() };
        let Ok(text) = fs::read_to_string(&path) else { return Store::default() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Writes the service's file back, best-effort.
    pub(crate) fn save(&self, service: &'static Service) {
        let Some(path) = path(service.store_file) else { return };
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

/// The on-disk offline queue, retried with backoff while the network or the service is failing.
/// One per service: `lastfm_queue.json`, `scrobblenow_queue.json`.
#[derive(Debug, Default)]
pub(crate) struct PendingQueue {
    entries: Vec<Scrobble>,
    /// Consecutive failed flushes, for the backoff.
    failures: u32,
    /// The earliest time a flush may be attempted again.
    next_attempt: Option<f64>,
}

impl PendingQueue {
    /// Reads the service's queue file, or an empty queue.
    pub(crate) fn load(service: &'static Service) -> PendingQueue {
        let Some(path) = path(service.queue_file) else { return PendingQueue::default() };
        let Ok(bytes) = fs::read(&path) else { return PendingQueue::default() };
        let Ok(store) = serde_json::from_slice::<QueueStore>(&bytes) else { return PendingQueue::default() };
        PendingQueue { entries: store.entries, failures: 0, next_attempt: None }
    }

    /// Appends a play, dropping the oldest when the queue is full, and persists.
    pub(crate) fn push(&mut self, scrobble: Scrobble, service: &'static Service) {
        self.entries.push(scrobble);
        if self.entries.len() > QUEUE_MAX {
            let overflow = self.entries.len() - QUEUE_MAX;
            self.entries.drain(..overflow);
        }
        self.save(service);
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

    /// Writes the service's queue file back, best-effort.
    pub(crate) fn save(&self, service: &'static Service) {
        let Some(path) = path(service.queue_file) else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_vec(&QueueStore { entries: self.entries.clone() }) else { return };
        write_private(&path, &text);
    }

    /// Submits what it can, newest batches first. Entries the service rejects with a
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
        self.save(api.service);
        failure.map_or(Ok(accepted), Err)
    }
}

/// How long to wait before the next queue flush, doubling per consecutive failure.
pub(crate) fn backoff_secs(failures: u32) -> f64 {
    let shift = failures.saturating_sub(1).min(5);
    (BASE_BACKOFF_SECS * (1u32 << shift) as f64).min(MAX_BACKOFF_SECS)
}

/// `<name>` in [`crate::paths::config_dir`].
fn path(name: &str) -> Option<PathBuf> {
    Some(crate::paths::config_dir()?.join(name))
}

/// Writes `bytes` to `path` with `0600` permissions, best-effort.
fn write_private(path: &std::path::Path, bytes: &[u8]) {
    let _ = crate::paths::write_private(path, bytes);
}

// ---------------------------------------------------------------------------------------------
// API signatures and requests

/// The Last.fm protocol's `api_sig`: the MD5 of every parameter's name and value concatenated in
/// name order, with the shared secret appended. `format` (and the unused `callback`) are excluded.
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
    /// Drop it: the service will never accept it.
    Drop,
}

/// The protocol's error codes that mean the submission itself is permanently invalid: invalid service,
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
    /// The service answered with an error code and message.
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
            ApiError::Network(message) => message.clone(),
            ApiError::Api { code, message } => format!("error {code}: {message}"),
        }
    }
}

/// The blocking Last.fm-protocol client for one service. Owned by that service's scrobble
/// thread; `Api::with_base` points it at a mock server for tests.
pub(crate) struct Api {
    agent: ureq::Agent,
    base: String,
    pub(crate) service: &'static Service,
}

impl Api {
    /// The client for `service`, at its real endpoint — unless a test named another one in
    /// `service.api_base_env`.
    pub(crate) fn for_service(service: &'static Service) -> Api {
        let base = std::env::var(service.api_base_env).unwrap_or_else(|_| service.api_base.to_owned());
        Api::with_base(service, &base)
    }

    /// The client for `service` at an explicit base, for tests.
    pub(crate) fn with_base(service: &'static Service, base: &str) -> Api {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .build();
        Api { agent: ureq::Agent::new_with_config(config), base: base.to_owned(), service }
    }

    fn call(&self, form: &[(String, String)]) -> Result<serde_json::Value, ApiError> {
        let name = self.service.name;
        let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut response = self
            .agent
            .post(&self.base)
            .send_form(pairs)
            .map_err(|error| ApiError::Network(format!("{name} network error: {error}")))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| ApiError::Network(format!("{name} network error: {error}")))?;
        if status == 429 {
            return Err(ApiError::Api { code: 29, message: "rate limit exceeded".to_owned() });
        }
        if status >= 500 {
            return Err(ApiError::Network(format!("{name} returned HTTP {status}")));
        }
        if !(200..300).contains(&status) {
            return Err(ApiError::Api { code: -1, message: format!("{name} returned HTTP {status}") });
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|error| ApiError::Network(format!("unreadable {name} response: {error}")))?;
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
    pub(crate) fn get_token(&self, store: &Store) -> Result<String, ApiError> {
        let (api_key, secret) = store.credentials(self.service);
        let form = signed_form("auth.getToken", Vec::new(), api_key, secret);
        let value = self.call(&form)?;
        value
            .get("token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Network(format!("{} did not return a token", self.service.name)))
    }

    /// `auth.getSession`: exchanges an authorized token for the session key and username.
    pub(crate) fn get_session(&self, store: &Store, token: &str) -> Result<(String, String), ApiError> {
        let (api_key, secret) = store.credentials(self.service);
        let form = signed_form("auth.getSession", vec![("token".to_owned(), token.to_owned())], api_key, secret);
        let value = self.call(&form)?;
        let name = self.service.name;
        let session = value
            .get("session")
            .ok_or_else(|| ApiError::Network(format!("{name} did not return a session")))?;
        let key = session.get("key").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        let username = session.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
        if key.is_empty() {
            return Err(ApiError::Network(format!("{name} returned an empty session key")));
        }
        Ok((key, username))
    }

    /// `track.updateNowPlaying` for the track that just started.
    pub(crate) fn update_now_playing(&self, store: &Store, track: &MediaTrack) -> Result<(), ApiError> {
        let (api_key, secret) = store.credentials(self.service);
        let mut form = signed_form("track.updateNowPlaying", track_params(track, None), api_key, secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        // Re-sign now that `sk` is present: the protocol requires every parameter signed.
        resign(&mut form, secret);
        self.call(&form).map(|_| ())
    }

    /// `track.scrobble` for up to [`BATCH`] plays; returns how many the service accepted.
    pub(crate) fn scrobble(&self, store: &Store, batch: &[Scrobble]) -> Result<usize, ApiError> {
        let (api_key, secret) = store.credentials(self.service);
        let mut params = Vec::new();
        for (index, entry) in batch.iter().enumerate() {
            for (name, value) in track_params(&entry.track, Some(entry.at)) {
                params.push((format!("{name}[{index}]"), value));
            }
        }
        let mut form = signed_form("track.scrobble", params, api_key, secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        resign(&mut form, secret);
        let value = self.call(&form)?;
        let accepted = value
            .pointer("/scrobbles/@attr/accepted")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(batch.len() as u64) as usize;
        Ok(accepted)
    }

    /// `track.love` / `track.unlove`.
    pub(crate) fn love(&self, store: &Store, title: &str, artist: &str, on: bool) -> Result<(), ApiError> {
        let (api_key, secret) = store.credentials(self.service);
        let method = if on { "track.love" } else { "track.unlove" };
        let params = vec![
            ("artist".to_owned(), artist.to_owned()),
            ("track".to_owned(), title.to_owned()),
        ];
        let mut form = signed_form(method, params, api_key, secret);
        form.push(("sk".to_owned(), store.session_key.clone()));
        resign(&mut form, secret);
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
#[derive(Debug)]
pub(crate) struct Scrobbler {
    current: Option<Playing>,
    playing: bool,
    last_update: f64,
    thresholds: Thresholds,
}

impl Default for Scrobbler {
    fn default() -> Self {
        Scrobbler { current: None, playing: false, last_update: 0.0, thresholds: Thresholds::OFFICIAL }
    }
}

impl Scrobbler {
    /// A tracker counting against `thresholds` instead of the official rule.
    pub(crate) fn with_thresholds(thresholds: Thresholds) -> Scrobbler {
        Scrobbler { thresholds, ..Scrobbler::default() }
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
        if !identifiable(&current.track)
            || !qualifies_with(current.track.duration_secs, current.played, self.thresholds)
        {
            return None;
        }
        Some(Completed { track: current.track, at: current.started_at, played_secs: current.played })
    }
}

/// Whether a track carries what a scrobbling service needs (a title and an artist). A local file
/// with no artist tag is skipped rather than mis-scrobbled.
pub(crate) fn identifiable(track: &MediaTrack) -> bool {
    !track.title.trim().is_empty() && !track.artist.trim().is_empty()
}

/// When a play counts as a scrobble, against an explicit rule (see [`Thresholds`]).
pub(crate) fn qualifies_with(
    duration_secs: Option<u64>,
    played_secs: f64,
    thresholds: Thresholds,
) -> bool {
    match duration_secs {
        Some(duration) if duration <= thresholds.min_track_secs => false,
        Some(duration) => played_secs >= (duration as f64 * 0.5).min(thresholds.max_played_secs),
        None => played_secs >= thresholds.max_played_secs,
    }
}

// ---------------------------------------------------------------------------------------------
// Worker thread

/// Commands the UI sends to one service's scrobble thread.
pub(crate) enum Command {
    /// A playback event to fold into the scrobble state machine.
    Media(MediaEvent),
    /// Start authorization: fetch a token and report the URL to open.
    Connect,
    /// The user authorized in their browser: exchange the token for a session.
    ConfirmAuth,
    /// Forget the session key (whatever credentials the service uses are kept).
    Disconnect,
    /// Store the API key and shared secret the user pasted (services without built-in ones).
    SetCredentials { api_key: String, secret: String },
    /// Update the three toggles.
    SetPrefs { scrobble: bool, now_playing: bool, love: bool },
    /// Love or unlove a track on the service.
    Love { title: String, artist: String, on: bool },
}

/// What the thread reports back to the UI.
#[derive(Debug)]
pub(crate) enum Status {
    Disconnected,
    /// A token was fetched; the user must open this URL (which embeds the token) and authorize.
    Authorizing { url: String },
    Connected { username: String },
    /// Something went wrong; shown in the card.
    Error(String),
    /// An informational line (scrobbled, retrying, skipped).
    Notice(String),
}

/// The UI side of one scrobbling service: the channel to its scrobble thread, the form fields
/// and the state its card draws. One per [`SERVICES`] entry.
pub(crate) struct Client {
    /// Which service this client speaks to (endpoints, name, files).
    pub(crate) service: &'static Service,
    cmd_tx: Sender<Command>,
    rx: Receiver<Status>,
    /// Whether anything would actually be sent (a session exists and a toggle is on), kept in
    /// step with `conn` and the toggles so the media hub can ask without reading UI state.
    enabled: Arc<AtomicBool>,
    /// The connection state shown in the card.
    pub(crate) conn: Connection,
    /// The pasted API key (Last.fm; hidden for a service with built-in credentials).
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

/// The handle another thread (the media hub) uses to follow playback with one service: it
/// forwards events to that service's scrobble thread and answers whether anything would be sent.
#[derive(Clone)]
pub(crate) struct Sink {
    tx: Sender<Command>,
    enabled: Arc<AtomicBool>,
}

impl Sink {
    /// Forwards one playback event to the scrobble thread. Never blocks.
    pub(crate) fn handle(&self, event: MediaEvent) {
        let _ = self.tx.send(Command::Media(event));
    }

    /// Whether anything would actually be sent: a session exists and a toggle is on.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

/// The card's view of the connection.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Connection {
    Disconnected,
    /// Waiting for the user to authorize the token in their browser.
    Authorizing { url: String },
    Connected { username: String },
}

impl Client {
    /// Starts the service's scrobble thread, seeding the form from its saved store.
    pub(crate) fn start(service: &'static Service, waker: Waker) -> Client {
        let store = Store::load(service);
        let conn = if store.connected() {
            Connection::Connected { username: store.username.clone() }
        } else {
            Connection::Disconnected
        };
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let prefs = (store.scrobble, store.now_playing, store.love);
        // The form starts from what was saved: the key and secret prefill, and the toggles show
        // their persisted state.
        let (key, secret) = (store.api_key.clone(), store.secret.clone());
        thread::spawn(move || run(service, store, cmd_rx, status_tx, waker));
        let client = Client {
            service,
            cmd_tx,
            rx: status_rx,
            enabled: Arc::new(AtomicBool::new(false)),
            conn,
            key: TextEdit::new(key),
            secret: TextEdit::new(secret),
            scrobble: prefs.0,
            now_playing: prefs.1,
            love: prefs.2,
            message: None,
            error: false,
        };
        client.mark_enabled();
        client
    }

    /// The handle the media hub sends playback events through.
    pub(crate) fn sink(&self) -> Sink {
        Sink { tx: self.cmd_tx.clone(), enabled: Arc::clone(&self.enabled) }
    }

    /// Recomputes [`Sink::enabled`] after the connection state or a toggle changed.
    fn mark_enabled(&self) {
        let on = matches!(self.conn, Connection::Connected { .. }) && (self.scrobble || self.now_playing);
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// A client for UI tests: the same state, but no thread, no filesystem and no network.
    #[cfg(test)]
    pub(crate) fn for_test(service: &'static Service, store: &Store) -> Client {
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_status_tx, status_rx) = mpsc::channel();
        let conn = if store.connected() {
            Connection::Connected { username: store.username.clone() }
        } else {
            Connection::Disconnected
        };
        let client = Client {
            service,
            cmd_tx,
            rx: status_rx,
            enabled: Arc::new(AtomicBool::new(false)),
            conn,
            key: TextEdit::new(store.api_key.clone()),
            secret: TextEdit::new(store.secret.clone()),
            scrobble: store.scrobble,
            now_playing: store.now_playing,
            love: store.love,
            message: None,
            error: false,
        };
        client.mark_enabled();
        client
    }

    /// Applies one status from the thread to the card's state. Returns whether it changed.
    fn apply(&mut self, status: Status) -> bool {
        let name = self.service.name;
        match status {
            Status::Disconnected => {
                self.conn = Connection::Disconnected;
                self.message = Some(format!("Disconnected from {name}"));
                self.error = false;
            }
            Status::Authorizing { url } => {
                self.conn = Connection::Authorizing { url };
                self.message = Some("Authorize tsun in your browser, then confirm.".to_owned());
                self.error = false;
            }
            Status::Connected { username } => {
                self.conn = Connection::Connected { username };
                self.message = Some(format!("Connected to {name}"));
                self.error = false;
            }
            Status::Error(message) => {
                self.message = Some(message);
                self.error = true;
            }
            Status::Notice(message) => {
                self.message = Some(message);
                self.error = false;
            }
        }
        self.mark_enabled();
        true
    }
}

/// The scrobble thread: owns the service's store, tracker, offline queue and HTTP client.
fn run(
    service: &'static Service,
    store: Store,
    cmd_rx: Receiver<Command>,
    tx: Sender<Status>,
    waker: Waker,
) {
    let api = Api::for_service(service);
    let mut store = store;
    // The official scrobble rule, unless a test asked for another one (see `Thresholds`).
    let mut tracker = Scrobbler::with_thresholds(Thresholds::from_env());
    let mut queue = PendingQueue::load(service);
    let mut pending_token: Option<String> = None;
    let mut warned_retry = false;
    let report = |status: Status| {
        let _ = tx.send(status);
        waker.as_ref()();
    };
    // Announce the restored state so the card matches the file.
    if store.connected() {
        report(Status::Connected { username: store.username.clone() });
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
                                    report(Status::Error(error.message()));
                                }
                            }
                        }
                    }
                    if let Some(completed) = actions.scrobble {
                        if store.scrobble {
                            queue.push(
                                Scrobble {
                                    track: completed.track,
                                    at: completed.at,
                                    played_secs: completed.played_secs,
                                },
                                service,
                            );
                        }
                    }
                    if store.connected() && queue.due(now) {
                        drain_queue(&api, &mut store, &mut queue, &report, &mut warned_retry);
                    }
                }
                Command::Connect => {
                    if !store.has_credentials(service) {
                        report(Status::Error(format!(
                            "Enter your {} API key and shared secret first",
                            service.name
                        )));
                        continue;
                    }
                    let (api_key, _) = store.credentials(service);
                    match api.get_token(&store) {
                        Ok(token) => {
                            let url = format!("{}?api_key={}&token={token}", service.auth_url, urlencode(api_key));
                            pending_token = Some(token);
                            report(Status::Authorizing { url });
                        }
                        Err(error) => report(Status::Error(error.message())),
                    }
                }
                Command::ConfirmAuth => {
                    let Some(token) = pending_token.clone() else {
                        report(Status::Error("Press Connect first".to_owned()));
                        continue;
                    };
                    match api.get_session(&store, &token) {
                        Ok((key, name)) => {
                            store.session_key = key;
                            store.username = name.clone();
                            store.save(service);
                            pending_token = None;
                            report(Status::Connected { username: name });
                            let now = unix_secs();
                            if queue.due(now) {
                                drain_queue(&api, &mut store, &mut queue, &report, &mut warned_retry);
                            }
                        }
                        Err(error) => report(Status::Error(error.message())),
                    }
                }
                Command::Disconnect => {
                    store.session_key.clear();
                    store.username.clear();
                    store.save(service);
                    pending_token = None;
                    report(Status::Disconnected);
                }
                Command::SetCredentials { api_key, secret } => {
                    store.api_key = api_key;
                    store.secret = secret;
                    store.save(service);
                }
                Command::SetPrefs { scrobble, now_playing, love } => {
                    store.scrobble = scrobble;
                    store.now_playing = now_playing;
                    store.love = love;
                    store.save(service);
                }
                Command::Love { title, artist, on } => {
                    if store.love && store.connected() {
                        if let Err(error) = api.love(&store, &title, &artist, on) {
                            if error.disposition() == ErrorDisposition::Drop {
                                report(Status::Error(error.message()));
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
    report: &impl Fn(Status),
    warned_retry: &mut bool,
) {
    let name = api.service.name;
    let now = unix_secs();
    match queue.flush(api, store, now) {
        Ok(count) if count > 0 => {
            *warned_retry = false;
            report(Status::Notice(format!("Scrobbled {count} play(s) to {name}")));
        }
        Ok(_) => {}
        Err(_) => {
            if !*warned_retry {
                *warned_retry = true;
                report(Status::Notice(format!(
                    "{} play(s) will be scrobbled when {name} is reachable again",
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
    /// The client for `service`, when it is running.
    pub(crate) fn scrobble_client(&self, service: &'static Service) -> Option<&Client> {
        self.scrobbling.iter().find(|client| client.service.id == service.id)
    }

    /// The mutable client for `service`, when it is running.
    pub(crate) fn scrobble_client_mut(&mut self, service: &'static Service) -> Option<&mut Client> {
        self.scrobbling.iter_mut().find(|client| client.service.id == service.id)
    }

    /// Drains every scrobble thread's statuses into its card's state. Called from `poll`.
    pub(crate) fn poll_scrobbling(&mut self) -> bool {
        let mut changed = false;
        for client in &mut self.scrobbling {
            loop {
                match client.rx.try_recv() {
                    Ok(status) => changed |= client.apply(status),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
        }
        changed
    }

    /// Sends the pasted API key and secret to the service's thread.
    pub(crate) fn scrobble_save_credentials(&mut self, service: &'static Service) {
        let Some(client) = self.scrobble_client(service) else { return };
        let api_key = client.key.text.trim().to_owned();
        let secret = client.secret.text.trim().to_owned();
        let _ = client.cmd_tx.send(Command::SetCredentials { api_key, secret });
    }

    /// Starts the authorization flow (fetch a token, then let the user open the URL).
    pub(crate) fn scrobble_connect(&mut self, service: &'static Service) {
        self.scrobble_save_credentials(service);
        if let Some(client) = self.scrobble_client_mut(service) {
            client.message = Some(format!("Contacting {}…", service.name));
            client.error = false;
            let _ = client.cmd_tx.send(Command::Connect);
        }
    }

    /// Completes the authorization after the user approved the token.
    pub(crate) fn scrobble_confirm(&mut self, service: &'static Service) {
        if let Some(client) = self.scrobble_client_mut(service) {
            client.message = Some("Finishing sign-in…".to_owned());
            client.error = false;
            let _ = client.cmd_tx.send(Command::ConfirmAuth);
        }
    }

    /// Forgets the session key while keeping whatever credentials the service uses.
    pub(crate) fn scrobble_disconnect(&mut self, service: &'static Service) {
        if let Some(client) = self.scrobble_client(service) {
            let _ = client.cmd_tx.send(Command::Disconnect);
        }
    }

    /// Flips one of a service's three toggles and pushes the change to its thread.
    pub(crate) fn scrobble_set_pref(&mut self, service: &'static Service, which: Pref) {
        let Some(client) = self.scrobble_client_mut(service) else { return };
        match which {
            Pref::Scrobble => client.scrobble = !client.scrobble,
            Pref::NowPlaying => client.now_playing = !client.now_playing,
            Pref::Love => client.love = !client.love,
        }
        client.mark_enabled();
        let command = Command::SetPrefs {
            scrobble: client.scrobble,
            now_playing: client.now_playing,
            love: client.love,
        };
        let _ = client.cmd_tx.send(command);
    }

    /// Mirrors a TIDAL favourite onto every connected service that has the love switch on. Only
    /// called for TIDAL tracks: a local file is never a TIDAL favourite and has no love of its
    /// own.
    pub(crate) fn scrobble_love(&mut self, track_id: u64, on: bool) {
        // Local files are never TIDAL favourites and have no love of their own.
        if crate::local::is_local_id(track_id) {
            return;
        }
        let Some((title, artist)) = self.track_artist_title(track_id) else { return };
        for client in &self.scrobbling {
            if !client.love || !matches!(client.conn, Connection::Connected { .. }) {
                continue;
            }
            let _ = client.cmd_tx.send(Command::Love {
                title: title.clone(),
                artist: artist.clone(),
                on,
            });
        }
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

/// One of a service's three toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pref {
    Scrobble,
    NowPlaying,
    Love,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The official scrobble rule, for the threshold tests.
    fn official(duration_secs: Option<u64>, played_secs: f64) -> bool {
        qualifies_with(duration_secs, played_secs, Thresholds::OFFICIAL)
    }

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
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(31))), 100.0);
        // 15 s in: below 50% of 31 s.
        let early = tracker.on_event(&MediaEvent::Stopped, 115.0);
        assert!(early.scrobble.is_none());
    }

    #[test]
    fn a_31_second_track_scrobbled_when_it_passed_half() {
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(31))), 100.0);
        let done = tracker.on_event(&MediaEvent::Finished, 116.0);
        let completed = done.scrobble.expect("past 50% of 31 s");
        assert_eq!(completed.track.title, "T");
        assert_eq!(completed.at, 100);
        assert!((completed.played_secs - 16.0).abs() < 0.001);
    }

    #[test]
    fn a_29_second_track_never_scrobbles() {
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(29))), 100.0);
        let done = tracker.on_event(&MediaEvent::Finished, 200.0);
        assert!(done.scrobble.is_none());
    }

    #[test]
    fn a_long_track_needs_four_minutes_even_at_ten_percent() {
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(3600))), 0.0);
        // 50% would be 30 minutes; four minutes is sooner.
        let early = tracker.on_event(&MediaEvent::Stopped, 239.0);
        assert!(early.scrobble.is_none());
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(3600))), 0.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 240.0);
        assert!(done.scrobble.is_some());
    }

    #[test]
    fn paused_time_does_not_count() {
        // A 200 s track needs 100 s of real playing.
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(200))), 0.0);
        tracker.on_event(&MediaEvent::Paused { position_secs: 10.0 }, 10.0);
        // 5 minutes paused, then 40 s more of real playing: 50 s total, below half.
        tracker.on_event(&MediaEvent::Resumed { position_secs: 10.0 }, 310.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 350.0);
        assert!(done.scrobble.is_none(), "only 50 s of the 200 s track played");
        // Real playing now adds up to 110 s, past the halfway point.
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(200))), 0.0);
        tracker.on_event(&MediaEvent::Paused { position_secs: 10.0 }, 10.0);
        tracker.on_event(&MediaEvent::Resumed { position_secs: 10.0 }, 310.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 410.0);
        assert!(done.scrobble.is_some());
    }

    #[test]
    fn seeking_does_not_add_played_time() {
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("T", "A", Some(300))), 0.0);
        // Jump to the last 10 s after only 20 s of real listening.
        tracker.on_event(&MediaEvent::Seeked { position_secs: 290.0 }, 20.0);
        let done = tracker.on_event(&MediaEvent::Stopped, 30.0);
        assert!(done.scrobble.is_none(), "only 30 s of real playing");
    }

    #[test]
    fn a_new_track_finalizes_the_previous_one() {
        let mut tracker = Scrobbler::default();
        tracker.on_event(&started(track("One", "A", Some(60))), 0.0);
        // 40 s of a 60 s track is past half: starting the next one scrobbles it.
        let actions = tracker.on_event(&started(track("Two", "B", Some(60))), 40.0);
        assert_eq!(actions.scrobble.expect("40 s past half of 60").track.title, "One");
        assert_eq!(actions.now_playing.unwrap().title, "Two");
    }

    #[test]
    fn repeat_one_scrobbles_again() {
        let mut tracker = Scrobbler::default();
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
        let mut tracker = Scrobbler::default();
        let actions = tracker.on_event(&started(track("T", "", Some(60))), 0.0);
        assert!(actions.now_playing.is_none());
        let done = tracker.on_event(&MediaEvent::Finished, 120.0);
        assert!(done.scrobble.is_none());
    }

    #[test]
    fn an_unknown_length_uses_the_four_minute_rule() {
        assert!(!official(None, 239.0));
        assert!(official(None, 240.0));
        assert!(!official(Some(29), 1000.0));
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
    ///
    /// `pub(crate)` so the worker's integration test can point a real Last.fm client at it; the
    /// requests it recorded are how a test checks what was actually sent.
    pub(crate) struct Mock {
        pub(crate) base: String,
        seen: Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
        requests: Arc<AtomicUsize>,
    }

    impl Mock {
        /// Answers `count` requests with the same successful body (a test that only cares about
        /// which methods were called does not need to script each one).
        pub(crate) fn repeating(count: usize, body: String) -> Mock {
            Mock::start(vec![(200, body); count])
        }

        /// Every request's form parameters, in the order they arrived.
        pub(crate) fn requests(&self) -> Vec<Vec<(String, String)>> {
            self.seen.lock().unwrap().clone()
        }

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
        let api = Api::with_base(&LASTFM, &mock.base);
        let store = Store { api_key: "key".into(), secret: "sec".into(), ..Store::default() };
        let token = api.get_token(&store).unwrap();
        assert_eq!(token, "TOK");
        let (session, name) = api.get_session(&store, &token).unwrap();
        assert_eq!((session.as_str(), name.as_str()), ("SESS", "tester"));
        let store = Store { session_key: session.clone(), username: name.clone(), ..store };
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
        let api = Api::with_base(&LASTFM, &mock.base);
        let store = Store { api_key: "k".into(), secret: "s".into(), session_key: "sk".into(), ..Store::default() };
        let mut queue = PendingQueue { entries: Vec::new(), failures: 0, next_attempt: None };
        queue.push(Scrobble { track: track("T", "A", Some(120)), at: 100, played_secs: 90.0 }, &LASTFM);

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
        let api = Api::with_base(&LASTFM, &mock.base);
        let store = Store { api_key: "k".into(), secret: "s".into(), session_key: "sk".into(), ..Store::default() };
        let mut queue = PendingQueue { entries: Vec::new(), failures: 0, next_attempt: None };
        queue.push(Scrobble { track: track("T", "A", Some(120)), at: 100, played_secs: 90.0 }, &LASTFM);
        // The single entry is retried on its own and, still invalid, dropped.
        assert!(queue.flush(&api, &store, 0.0).is_ok());
        assert!(queue.is_empty());
    }

    // -----------------------------------------------------------------------------------------
    // Two services side by side

    /// A scratch `XDG_CONFIG_HOME` plus the service environment, for tests that start real
    /// clients. The config lock is held for as long as the variables are set (they are
    /// process-wide), and everything is restored and removed on drop.
    struct ScratchEnv {
        previous: Option<std::ffi::OsString>,
        dir: PathBuf,
        vars: Vec<(&'static str, Option<std::ffi::OsString>)>,
        /// Declared last so the lock is held until the variables are restored (fields drop in
        /// declaration order, and `Drop` runs before them).
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl ScratchEnv {
        fn new(name: &str, vars: &[(&'static str, String)]) -> ScratchEnv {
            let turn = crate::settings::lock_config_env();
            let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "tsun-scrobble-{}-{name}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(dir.join("tsun")).unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: the config lock is held, so no other test reads these variables.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
            let mut saved = Vec::new();
            for (name, value) in vars {
                saved.push((*name, std::env::var_os(name)));
                unsafe { std::env::set_var(name, value) };
            }
            ScratchEnv { previous, dir, vars: saved, _turn: turn }
        }

        fn write(&self, file: &str, contents: &str) {
            fs::write(self.dir.join("tsun").join(file), contents).unwrap();
        }

        fn config_file(&self, file: &str) -> PathBuf {
            self.dir.join("tsun").join(file)
        }

        fn exists(&self, file: &str) -> bool {
            self.config_file(file).exists()
        }
    }

    impl Drop for ScratchEnv {
        fn drop(&mut self) {
            // SAFETY: the config lock is still held (see the field order).
            unsafe {
                for (name, value) in &self.vars {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
                match &self.previous {
                    Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// The `method` of every request a mock received, in order.
    fn methods(mock: &Mock) -> Vec<String> {
        mock.requests()
            .iter()
            .filter_map(|params| param_of(params, "method"))
            .collect()
    }

    fn param_of(params: &[(String, String)], name: &str) -> Option<String> {
        params.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
    }

    /// Waits for `condition`, polling: the scrobble threads work asynchronously.
    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        condition()
    }

    fn test_waker() -> Waker {
        Arc::new(|| {})
    }

    #[test]
    fn each_service_keeps_its_own_files_and_old_files_load_with_defaults() {
        let env = ScratchEnv::new("files", &[]);
        // The two services never share a file, so connecting one cannot disturb the other.
        assert_ne!(LASTFM.store_file, SCROBBLENOW.store_file);
        assert_ne!(LASTFM.queue_file, SCROBBLENOW.queue_file);
        assert_ne!(LASTFM.api_base, SCROBBLENOW.api_base);
        assert_ne!(LASTFM.api_base_env, SCROBBLENOW.api_base_env);
        assert!(LASTFM.builtin.is_none(), "Last.fm asks the user for a key");
        assert!(SCROBBLENOW.builtin.is_some(), "scrobble.now ships one");

        // A Last.fm file written before the switches existed keeps loading exactly as it did:
        // the credentials and session it carries, the same defaults for the rest.
        env.write(
            LASTFM.store_file,
            r#"{"api_key":"oldkey","secret":"oldsecret","session_key":"OLDSESS","username":"oldlistener"}"#,
        );
        let legacy = Store::load(&LASTFM);
        assert_eq!(
            (legacy.api_key.as_str(), legacy.session_key.as_str(), legacy.username.as_str()),
            ("oldkey", "OLDSESS", "oldlistener"),
            "the credentials an old file carries are kept"
        );
        assert!(legacy.scrobble && legacy.now_playing && !legacy.love, "and the defaults fill the rest");
        assert!(legacy.has_credentials(&LASTFM));
        assert_eq!(legacy.credentials(&LASTFM), ("oldkey", "oldsecret"));

        // A scrobble.now file written before the switches existed loads the same way.
        env.write(SCROBBLENOW.store_file, r#"{"session_key":"S","username":"late"}"#);
        let store = Store::load(&SCROBBLENOW);
        assert_eq!(store.username, "late");
        assert!(store.scrobble && store.now_playing, "the defaults send both");
        assert!(!store.love, "loving is off by default");

        // The credentials are the service's own: scrobble.now signs with tsun's built-in pair
        // whatever an old file may hold, Last.fm uses what the user pasted.
        assert_eq!(store.credentials(&SCROBBLENOW), ("tsun", SCROBBLENOW.builtin.unwrap().secret));
        assert!(store.has_credentials(&SCROBBLENOW), "no key to paste for scrobble.now");

        // `settings.json` is not involved at all, so any file from an earlier tsun loads: the
        // scrobbling switches live beside each service's session, never in it.
        assert!(!fs::read_to_string(env.config_file("settings.json")).unwrap_or_default().contains("scrobble"));

        // Saving one service writes its own file only.
        let lastfm_before = fs::read_to_string(env.config_file(LASTFM.store_file)).unwrap();
        Store { username: "saved".to_owned(), ..store }.save(&SCROBBLENOW);
        assert!(env.exists(SCROBBLENOW.store_file));
        assert_eq!(
            fs::read_to_string(env.config_file(LASTFM.store_file)).unwrap(),
            lastfm_before,
            "Last.fm's file is untouched"
        );
        assert_eq!(Store::load(&SCROBBLENOW).username, "saved");

        // The offline queue is per service too.
        let mut queue = PendingQueue::default();
        queue.push(
            Scrobble { track: track("T", "A", Some(120)), at: 100, played_secs: 90.0 },
            &SCROBBLENOW,
        );
        assert!(env.exists(SCROBBLENOW.queue_file));
        assert!(!env.exists(LASTFM.queue_file), "Last.fm's queue is untouched");
        let text = fs::read_to_string(env.config_file(SCROBBLENOW.queue_file)).unwrap();
        assert!(text.contains("\"T\""), "{text}");
    }

    /// The scrobble.now flow end to end against a mock: getToken, getSession (username), then
    /// now-playing and a scrobble — all at the scrobble.now base, with tsun's own key, and with
    /// the session landing in `scrobblenow.json` and the queue in `scrobblenow_queue.json`.
    #[test]
    fn a_scrobblenow_client_authorizes_and_scrobbles_through_its_own_base() {
        let mock = Mock::start(vec![
            (200, r#"{"token":"TOKNOW"}"#.to_owned()),
            (200, r#"{"session":{"name":"nowlistener","key":"SESSNOW","subscriber":"1"}}"#.to_owned()),
            (200, r#"{"nowplaying":{}}"#.to_owned()),
            (200, r#"{"scrobbles":{"@attr":{"accepted":1,"ignored":0}}}"#.to_owned()),
        ]);
        let env = ScratchEnv::new(
            "scrobblenow",
            &[
                (SCROBBLENOW.api_base_env, mock.base.clone()),
                ("TSUN_SCROBBLE_MIN_TRACK_SECS", "0".to_owned()),
                ("TSUN_SCROBBLE_MIN_PLAYED_SECS", "0".to_owned()),
            ],
        );
        assert!(SCROBBLENOW.api_base.starts_with("https://scrobble.now/2.0/"));
        let client = Client::start(&SCROBBLENOW, test_waker());
        assert!(matches!(client.conn, Connection::Disconnected));

        // Connect: no credentials are asked for, and the token comes back from the mock.
        let _ = client.cmd_tx.send(Command::Connect);
        let status = client.rx.recv_timeout(Duration::from_secs(10)).expect("an authorization URL");
        let Status::Authorizing { url } = status else { panic!("expected authorization, got {status:?}") };
        assert_eq!(url, "https://scrobble.now/api/auth/?api_key=tsun&token=TOKNOW");

        // Confirm: the session's username is parsed and the store file is scrobble.now's.
        let _ = client.cmd_tx.send(Command::ConfirmAuth);
        let connected = wait_for_status(&client, |status| {
            matches!(status, Status::Connected { username } if username == "nowlistener")
        });
        assert!(connected, "the session was not accepted");
        assert!(env.exists(SCROBBLENOW.store_file), "the session is saved in scrobblenow.json");
        assert!(!env.exists(LASTFM.store_file), "Last.fm's file is untouched");
        let saved = fs::read_to_string(env.config_file(SCROBBLENOW.store_file)).unwrap();
        assert!(saved.contains("SESSNOW") && saved.contains("nowlistener"), "{saved}");

        // Playback: one now-playing, then one scrobble, both at the scrobble.now base.
        let media = MediaEvent::Started { track: track("T", "A", Some(120)), position_secs: 0.0 };
        let _ = client.cmd_tx.send(Command::Media(media));
        let _ = client.cmd_tx.send(Command::Media(MediaEvent::Finished));
        let sent = || methods(&mock);
        assert!(
            wait_until(|| {
                let seen = sent();
                seen.contains(&"track.updateNowPlaying".to_owned())
                    && seen.contains(&"track.scrobble".to_owned())
            }),
            "the mock received {:?}",
            sent()
        );
        let requests = mock.requests();
        assert_eq!(
            methods(&mock),
            [
                "auth.getToken",
                "auth.getSession",
                "track.updateNowPlaying",
                "track.scrobble"
            ],
            "the requests went to the mock, in the Last.fm protocol's order"
        );
        // tsun's own key signs every request, and the session key is carried once connected.
        assert_eq!(param_of(&requests[0], "api_key").as_deref(), Some("tsun"));
        assert_eq!(
            param_of(&requests[0], "api_sig"),
            Some(api_sig(&requests[0], SCROBBLENOW.builtin.unwrap().secret)),
            "the token request is signed with tsun's own secret"
        );
        assert_eq!(param_of(&requests[3], "sk").as_deref(), Some("SESSNOW"));
        assert_eq!(param_of(&requests[3], "artist[0]").as_deref(), Some("A"));
    }

    /// Both services follow one track through the media hub: each gets one now-playing and one
    /// scrobble, a favourite reaches both, and turning one service's switches off stops only it.
    #[test]
    fn both_services_follow_playback_and_only_the_one_switched_off_stops() {
        let lastfm_mock = Mock::repeating(10, r#"{"nowplaying":{}}"#.to_owned());
        let now_mock =
            Mock::repeating(12, r#"{"scrobbles":{"@attr":{"accepted":1,"ignored":0}}}"#.to_owned());
        let env = ScratchEnv::new(
            "both",
            &[
                (LASTFM.api_base_env, lastfm_mock.base.clone()),
                (SCROBBLENOW.api_base_env, now_mock.base.clone()),
                ("TSUN_SCROBBLE_MIN_TRACK_SECS", "0".to_owned()),
                ("TSUN_SCROBBLE_MIN_PLAYED_SECS", "0".to_owned()),
            ],
        );
        // Both stores connected, with the love switch on, so a heart mirrors to both.
        for service in [&LASTFM, &SCROBBLENOW] {
            env.write(
                service.store_file,
                r#"{"api_key":"key","secret":"secret","session_key":"SESS","username":"listener","scrobble":true,"now_playing":true,"love":true}"#,
            );
        }
        let mut ui = crate::app::init();
        ui.scrobbling = vec![
            Client::start(&LASTFM, test_waker()),
            Client::start(&SCROBBLENOW, test_waker()),
        ];
        let hub = crate::media_hub::MediaHub::new();
        hub.set_scrobbling(ui.scrobbling.iter().map(Client::sink).collect());

        // One track, played to its end: one now-playing and one scrobble to each service.
        let first = track("T", "A", Some(120));
        ui.now_playing = Some(now_playing(42, &first));
        hub.dispatch_now(MediaEvent::Started { track: first.clone(), position_secs: 0.0 });
        hub.dispatch_now(MediaEvent::Finished);
        assert!(
            wait_until(|| {
                methods(&lastfm_mock).len() >= 2 && methods(&now_mock).len() >= 2
            }),
            "Last.fm: {:?}, scrobble.now: {:?}",
            methods(&lastfm_mock),
            methods(&now_mock)
        );
        for name in [&lastfm_mock, &now_mock] {
            let seen = methods(name);
            assert_eq!(
                seen.iter().filter(|method| *method == "track.updateNowPlaying").count(),
                1,
                "exactly one now-playing: {seen:?}"
            );
            assert_eq!(
                seen.iter().filter(|method| *method == "track.scrobble").count(),
                1,
                "exactly one scrobble: {seen:?}"
            );
        }

        // A TIDAL favourite mirrors to both services.
        ui.scrobble_love(42, true);
        assert!(
            wait_until(|| {
                methods(&lastfm_mock).contains(&"track.love".to_owned())
                    && methods(&now_mock).contains(&"track.love".to_owned())
            }),
            "Last.fm: {:?}, scrobble.now: {:?}",
            methods(&lastfm_mock),
            methods(&now_mock)
        );

        // Switch Last.fm's sending off: its thread stops, scrobble.now's carries on.
        let lastfm_before = methods(&lastfm_mock).len();
        ui.scrobble_set_pref(&LASTFM, Pref::Scrobble);
        ui.scrobble_set_pref(&LASTFM, Pref::Love);
        ui.scrobble_set_pref(&LASTFM, Pref::NowPlaying);
        let second = track("T2", "B", Some(180));
        ui.now_playing = Some(now_playing(43, &second));
        hub.dispatch_now(MediaEvent::Started { track: second.clone(), position_secs: 0.0 });
        hub.dispatch_now(MediaEvent::Finished);
        ui.scrobble_love(43, true);
        assert!(
            wait_until(|| {
                let seen = methods(&now_mock);
                seen.iter().filter(|method| *method == "track.scrobble").count() >= 2
                    && seen.iter().filter(|method| *method == "track.love").count() >= 2
            }),
            "scrobble.now: {:?}",
            methods(&now_mock)
        );
        // A moment for anything the other thread was still going to send.
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            methods(&lastfm_mock).len(),
            lastfm_before,
            "Last.fm sent nothing after its switches were turned off: {:?}",
            methods(&lastfm_mock)
        );
        // The stores record each service's own switches, so the choice survives a restart.
        let lastfm_store = Store::load(&LASTFM);
        let now_store = Store::load(&SCROBBLENOW);
        assert!(!lastfm_store.scrobble && !lastfm_store.now_playing && !lastfm_store.love);
        assert!(now_store.scrobble && now_store.now_playing && now_store.love);
    }

    fn now_playing(id: u64, track: &MediaTrack) -> crate::app::NowPlaying {
        crate::app::NowPlaying {
            id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 0,
            duration: track.duration_secs,
            quality: None,
            output: None,
        }
    }

    /// Drains `client`'s statuses until `wanted` matches (or the timeout passes).
    fn wait_for_status(client: &Client, wanted: impl Fn(&Status) -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match client.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(status) => {
                    if wanted(&status) {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
        false
    }
}
