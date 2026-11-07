//! TIDAL backend for the tsun desktop client.
//!
//! The module implements two OAuth 2.0 flows against TIDAL:
//!
//! * **Device authorization** ([`Client::login`] / [`Client::complete_login`]).
//!   The user opens a short verification URL on any device. This is the flow the
//!   UI starts by default.
//! * **Authorization code + PKCE** ([`Client::start_pkce`] /
//!   [`Client::finish_pkce`]). The user logs in through a browser and pastes the
//!   redirected URL back into the app. TIDAL currently caps device-flow tokens
//!   at `HIGH` (320 kbps AAC) for many accounts; the PKCE client is the one that
//!   can unlock `LOSSLESS` and `HI_RES_LOSSLESS` FLAC.
//!
//! Access and refresh tokens are persisted in
//! `$XDG_CONFIG_HOME/tsun/session.json` (or `$HOME/.config/tsun/session.json`).
//! On Unix the directory is created `0700` and the file `0600` before any token
//! is written; writes are atomic (temporary file + rename). The file records
//! which OAuth client issued the tokens so a later refresh uses the matching
//! client id/secret.
//!
//! Search uses TIDAL's v1 catalogue API. Playback converts segmented DASH
//! manifests into fMP4 HLS playlists for the in-process audio player.
//! The displayed quality is the quality TIDAL actually reports.
//!
//! The home screen is filled from the signed-in user's real TIDAL collection
//! via `GET /users/{userId}/favorites/tracks`; see [`Client::home_tracks`].
//! The profile menu's name and avatar come from the v2 listener profile,
//! `GET /v2/profiles/me`; see [`Client::profile`].
//!
//! All network and playback entry points are blocking. The UI owns the client
//! on a worker thread; `Client` is `Send`.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::audio::{Outcome, OutputInfo, OutputOptions, Player, Source, Volume};

// ---------------------------------------------------------------------------------------------
// Constants

const AUTH_BASE: &str = "https://auth.tidal.com/v1/oauth2";
const API_BASE: &str = "https://api.tidal.com/v1";
/// User-facing v2 API. The account's listener profile — and the only copy of its
/// avatar — lives at `/v2/profiles/me`, not on the v1 user object.
const API_V2_BASE: &str = "https://api.tidal.com/v2";
const AUTHORIZE_URL: &str = "https://login.tidal.com/authorize";

// Public client credentials used by the open-source python-tidal project for
// TIDAL's limited-input device flow. They identify the application, not the user.
const DEVICE_CLIENT_ID: &str = "fX2JxdmntZWK0ixT";
const DEVICE_CLIENT_SECRET: &str = "1Nn9AfDAjxrgJFJbKNWLeAyKGVGmINuXPPLHVXAvxAg=";
const DEVICE_SCOPE: &str = "r_usr w_usr";

// Public PKCE client credentials from python-tidal. This client is entitled to
// LOSSLESS / HI_RES_LOSSLESS streams. The authorization-code exchange sends no
// secret; refreshes use the secret.
const PKCE_CLIENT_ID: &str = "6BDSRdpK9hqEBTgU";
const PKCE_CLIENT_SECRET: &str = "xeuPmY7nbpZ9IIbLAcQ93shka1VNheUAqN6IcszjTG8=";
const PKCE_REDIRECT_URI: &str = "https://tidal.com/android/login/auth";
const PKCE_REDIRECT_URI_ENCODED: &str = "https%3A%2F%2Ftidal.com%2Fandroid%2Flogin%2Fauth";
const PKCE_SCOPE: &str = "r_usr w_usr w_sub";

// w_sub is not granted to every client and is not needed to refresh; asking for
// it can produce a 400 "scopes not allowed".
const REFRESH_SCOPE: &str = "r_usr w_usr";

const USER_AGENT: &str = "tsun/0.1 (TIDAL desktop client)";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const TOKEN_REFRESH_MARGIN_SECS: u64 = 60;
/// How many results of each kind `search_all` asks for per request.
const SEARCH_ALL_LIMIT: u32 = 30;
/// How many cards a page module contributes; TIDAL's pages default to ~20.
const PAGE_LIMIT: u32 = 30;
/// How many favourite entries are fetched per pagination request. TIDAL caps
/// the collection endpoints at 100 items per page.
const FAVORITES_PAGE_LIMIT: u32 = 100;
/// How many favourite tracks the home screen loads in one request. TIDAL caps
/// this collection endpoint at 100 items per page.
const HOME_FAVORITES_LIMIT: u32 = 50;
/// Safety bound so a server that never reports a total cannot loop forever.
const MAX_PAGINATED_ITEMS: u64 = 5_000;
/// How many top tracks the artist page fetches in one request, and what "Show
/// all" tops out at. TIDAL's `toptracks` endpoint caps a page at 100.
pub(crate) const TOP_TRACKS_LIMIT: u32 = 50;
/// The most top tracks one "Show all" fetch will pull in, so a single click stays bounded even
/// for an artist TIDAL gives thousands of tracks.
pub(crate) const MAX_TOP_TRACKS: u32 = 200;

// ---------------------------------------------------------------------------------------------
// Public types

/// A track as displayed by the UI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Raw TIDAL album cover UUID, used to build artwork URLs. `None` when the
    /// search result carries no album/cover.
    pub cover: Option<String>,
    /// Track length in whole seconds. `None` when TIDAL omits it.
    pub duration: Option<u64>,
    /// Numeric id of the album the track belongs to. `None` when TIDAL omits
    /// the album object.
    pub album_id: Option<u64>,
    /// Numeric id of the primary artist. `None` when TIDAL omits the artist.
    pub artist_id: Option<u64>,
    /// Whether TIDAL flags the track as explicit.
    pub explicit: bool,
    /// The `audioQuality` tag TIDAL reports for this track (e.g. `LOSSLESS`,
    /// `HI_RES_LOSSLESS`, `HIGH`). `None` when the object omits it.
    pub quality: Option<String>,
    /// 1-based position on its album, when TIDAL reports it.
    pub track_number: Option<u32>,
}

/// An album as shown on artist pages, search results and the collection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Album {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub artist_id: Option<u64>,
    /// Raw TIDAL cover UUID (see [`Track::cover`]).
    pub cover: Option<String>,
    /// Four-digit release year, when TIDAL reports a release date.
    pub year: Option<String>,
    /// Number of tracks on the album.
    pub tracks: Option<u32>,
    /// Total playing time in seconds.
    pub duration: Option<u64>,
    pub explicit: bool,
    /// `audioQuality` tag, e.g. `LOSSLESS` or `HI_RES_LOSSLESS`.
    pub quality: Option<String>,
}

/// An artist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Artist {
    pub id: u64,
    pub name: String,
    /// Raw TIDAL picture UUID (see [`Profile::picture`]).
    pub picture: Option<String>,
}

/// A music video, as shown in search results, artist pages and the collection.
///
/// TIDAL's video streams are plain HLS: an `mpegurl` master playlist whose
/// variants carry H.264 + AAC MPEG-TS segments with no `EXT-X-KEY` (no DRM).
/// See [`Client::video_stream`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Video {
    pub id: u64,
    pub title: String,
    /// The primary artist's name, or the first artist TIDAL lists.
    pub artist: String,
    /// The primary artist's id, when TIDAL reports one.
    pub artist_id: Option<u64>,
    /// Raw TIDAL image UUID (16:9 thumbnail; see [`Track::cover`] for the form).
    pub image: Option<String>,
    /// Video length in whole seconds.
    pub duration: Option<u64>,
    pub explicit: bool,
    /// `quality` tag TIDAL reports, e.g. `MP4_1080P`.
    pub quality: Option<String>,
    /// Whether TIDAL flags the video as streamable at all.
    pub stream_ready: bool,
}

/// A playable video stream: the HLS master playlist URL TIDAL's playback
/// endpoint handed back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VideoStream {
    /// The `application/vnd.apple.mpegurl` master playlist, with its token in
    /// the query string. Variant media playlists list plain MPEG-TS segments.
    pub master_url: String,
}

/// The strongest search result TIDAL returned in its `topHit` field, when it
/// sends one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopHit {
    Track(Track),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

/// The combined result of a multi-type catalogue search (`GET /search`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchAll {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<Playlist>,
    pub videos: Vec<Video>,
    pub top: Option<TopHit>,
}

/// One card on a TIDAL page (explore, genre, mood…). The UI renders whichever
/// kind it drew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    Album(Album),
    Playlist(Playlist),
    Artist(Artist),
    Mix(Mix),
    Track(Track),
}

/// A titled row of cards parsed from a TIDAL page module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    pub title: String,
    pub items: Vec<Card>,
}

/// An artist's page: everything the official client's artist view needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArtistPage {
    pub artist: Artist,
    pub bio: Option<String>,
    pub top_tracks: Vec<Track>,
    /// How many top tracks TIDAL says the artist has, when the endpoint reports
    /// a total. `None` when it does not, or the request failed.
    pub top_tracks_total: Option<u64>,
    pub albums: Vec<Album>,
    /// EPs and singles (`filter=EPSANDSINGLES`).
    pub singles: Vec<Album>,
    /// Albums by other artists this artist appears on.
    pub appears_on: Vec<Album>,
    /// The artist's music videos (`GET /artists/{id}/videos`).
    pub videos: Vec<Video>,
    pub similar: Vec<Artist>,
}

/// A track's lyrics: plain text plus optional time-synced lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LyricsData {
    /// Unsynchronised lyrics text, or the text parts of the synced lines.
    pub text: String,
    /// `(milliseconds, text)` lines parsed from the LRC `subtitles`, sorted by
    /// time.
    pub synced: Vec<(u64, String)>,
    /// Where TIDAL sourced the lyrics, when it reports a provider.
    pub provider: Option<String>,
}

/// One role's contributors from a track's credits (`GET /tracks/{id}/credits`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Credit {
    /// The role the contributors filled, e.g. `Producer`, `Composer` or `Bass guitar`.
    pub role: String,
    /// The contributor names TIDAL lists for that role, in its order.
    pub names: Vec<String>,
}

/// An album's editorial review (`GET /albums/{id}/review`): the text and the
/// source TIDAL credits it to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlbumReview {
    /// The text TIDAL wrote about the album, with its `[wimpLink …]`/`<br/>`
    /// markup flattened to plain prose.
    pub text: String,
    /// Who TIDAL credits the text to (`TiVo`, `TIDAL`, …), when it names one.
    pub source: Option<String>,
}

/// The extra album-page data that loads beside the album itself: the review,
/// the credits, and TIDAL's related albums. Any part TIDAL does not return is
/// left empty rather than failing the page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlbumExtras {
    pub review: Option<AlbumReview>,
    pub credits: Vec<Credit>,
    pub similar: Vec<Album>,
}

/// The signed-in user's TIDAL collection, assembled from the favourites
/// endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Library {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<Playlist>,
    pub videos: Vec<Video>,
}

/// The user's favourites as plain ids, for marking heart icons without loading
/// the full objects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FavoriteIds {
    pub tracks: HashSet<u64>,
    pub albums: HashSet<u64>,
    pub artists: HashSet<u64>,
    pub playlists: HashSet<String>,
    pub videos: HashSet<u64>,
}

/// Which favourites collection a [`Client::set_favorite`] call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavKind {
    Track,
    Album,
    Artist,
    Playlist,
    Video,
}

/// Preferred playback quality. Requests start at this tier and fall back to the
/// lower ones; it is persisted in the session file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Quality {
    Low,
    High,
    Lossless,
    #[default]
    HiResLossless,
}

impl Quality {
    /// The `audioquality` value TIDAL's playback endpoint expects.
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Low => "LOW",
            Quality::High => "HIGH",
            Quality::Lossless => "LOSSLESS",
            Quality::HiResLossless => "HI_RES_LOSSLESS",
        }
    }
}

/// Tiers to try, best-representing first: the preference itself, then each lower
/// tier as a fallback.
fn quality_chain(preference: Quality) -> Vec<&'static str> {
    let mut chain = Vec::new();
    for tier in [
        Quality::Low,
        Quality::High,
        Quality::Lossless,
        Quality::HiResLossless,
    ] {
        if tier <= preference {
            chain.push(tier.as_str());
        }
    }
    chain.reverse();
    chain
}

/// Converts TIDAL's ReplayGain (dB) to the linear multiplier the audio output applies.
///
/// TIDAL reports a negative gain for loud tracks and a positive one for quiet ones; `-6 dB`
/// becomes `0.5`. A non-finite value falls back to unity so a bad tag cannot silence a track.
fn replay_gain_linear(db: f64) -> f32 {
    if !db.is_finite() {
        return 1.0;
    }
    (10f64.powf(db / 20.0)) as f32
}

pub struct HomeSection {
    pub title: String,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub cover: Option<String>,
    pub count: u64,
    /// The playlist's description, as TIDAL reports it. Blank descriptions become `None`.
    pub description: Option<String>,
    /// The numeric TIDAL id of the user who created the playlist, when TIDAL reports it.
    /// [`Playlist::owned`] is derived from it.
    pub creator: Option<u64>,
    /// TIDAL's `publicPlaylist` flag: `Some(true)` for a public playlist, `Some(false)` for a
    /// private one, `None` when TIDAL does not say.
    pub public: Option<bool>,
    /// Whether the signed-in user owns the playlist, and so may rename, re-describe, reorder
    /// or delete it and remove its tracks. Only the owner may edit.
    pub owned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mix {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub image: Option<String>,
}

/// The signed-in user's listener profile, as shown in the top-right menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    /// Display name TIDAL reports for the profile: its `name`, else its `handle`,
    /// else (name fallback) the user's `profileName`/`firstName lastName`/`username`.
    /// `None` only when TIDAL reports none of them.
    pub name: Option<String>,
    /// The account's avatar, as the raw image UUID from `picture.url` of
    /// `GET /v2/profiles/me`, or `None` when the profile has no picture.
    ///
    /// The value is the same kind of hyphenated UUID as [`Track::cover`] (a bare
    /// id, not a URL). Pass it to
    /// [`Artwork::request_profile`](crate::art::Artwork::request_profile) and
    /// read the decoded image back with
    /// [`Artwork::get`](crate::art::Artwork::get) using this exact string as the
    /// key. `None` means the UI should draw its own fallback.
    pub picture: Option<String>,
}

/// Which OAuth client issued the persisted tokens. Recorded so refreshes use the
/// same client that created them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum AuthClient {
    #[default]
    Device,
    Pkce,
}

struct DeviceLogin {
    device_code: String,
    expires_at: u64,
    interval: u64,
}

struct PkcePending {
    code_verifier: String,
    client_unique_key: String,
}

/// Blocking TIDAL client. Own it on one worker thread; it is `Send`.
pub struct Client {
    agent: ureq::Agent,
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: u64,
    country_code: Option<String>,
    user_id: Option<u64>,
    auth_client: AuthClient,
    session_path: Option<PathBuf>,
    device: Option<DeviceLogin>,
    pkce: Option<PkcePending>,
    player: Option<Player>,
    /// Shared output-volume handle, cloned into every [`Player::start`] so the UI
    /// can change volume or mute between tracks and during playback.
    volume: Volume,
    /// Preferred playback quality; drives which `audioquality` tiers `play`
    /// requests and is persisted in the session file.
    quality: Quality,
    /// Whether ReplayGain-style normalization is applied to each stream's reported gain.
    normalize: bool,
    /// The current stream's replay gain as a linear multiplier, so a normalization toggle can
    /// be applied without restarting the track.
    replay_gain: Option<f32>,
    /// The output device every stream is opened on, by its cpal name; `None` is the system
    /// default. Chosen in Settings and persisted there.
    output_device: Option<String>,
}

// The UI moves the client into a worker thread.
#[allow(dead_code)]
fn _assert_client_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Client>();
}

impl Client {
    /// Creates a client and restores a persisted session, if one exists.
    ///
    /// No network request is made here. An expired access token is refreshed
    /// lazily by [`Client::search_all`] or [`Client::play`].
    pub fn new() -> Result<Self, String> {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(HTTP_TIMEOUT))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let session_path = session_file_path();
        let mut client = Client {
            agent,
            access_token: None,
            refresh_token: None,
            expires_at: 0,
            country_code: None,
            user_id: None,
            auth_client: AuthClient::Device,
            session_path,
            device: None,
            pkce: None,
            player: None,
            volume: Volume::new(),
            quality: Quality::default(),
            normalize: false,
            replay_gain: None,
            output_device: None,
        };
        if let Some(path) = client.session_path.clone() {
            client.load_session(&path);
        }
        Ok(client)
    }

    /// True when a saved access token is still valid or a refresh token exists.
    ///
    /// This does not contact TIDAL. A revoked refresh token is only discovered
    /// when the next request tries to refresh, at which point the session is
    /// cleared and the error asks the user to sign in again.
    pub fn is_logged_in(&self) -> bool {
        self.refresh_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
            || (self.access_token.is_some() && self.expires_at > now_secs())
    }

    // -----------------------------------------------------------------------------------------
    // Device authorization

    /// Starts TIDAL's device authorization flow and returns the verification URL.
    ///
    /// The URL already contains the user code. The caller should show it to the
    /// user and then call [`Client::complete_login`], which blocks until the
    /// user authorizes the device, the code expires, or a fatal error occurs.
    pub fn login(&mut self) -> Result<String, String> {
        let mut response = self
            .agent
            .post(&format!("{AUTH_BASE}/device_authorization"))
            .header("User-Agent", USER_AGENT)
            .send_form([("client_id", DEVICE_CLIENT_ID), ("scope", DEVICE_SCOPE)])
            .map_err(|error| format!("could not start TIDAL device login: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("could not read TIDAL login response: {error}"))?;
        if status != 200 {
            return Err(api_error_message(status, &body));
        }
        let authorization: DeviceAuthorizationResponse = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL login response: {error}"))?;
        let url = authorization
            .verification_uri_complete
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .or_else(|| {
                authorization
                    .verification_uri
                    .as_deref()
                    .filter(|url| !url.trim().is_empty())
            })
            .map(str::to_string)
            .ok_or_else(|| "TIDAL did not return a login URL".to_string())?;
        let url = normalize_url(&url);
        self.device = Some(DeviceLogin {
            device_code: authorization.device_code,
            expires_at: now_secs().saturating_add(authorization.expires_in.max(1)),
            interval: authorization.interval.clamp(1, 30),
        });
        Ok(url)
    }

    /// Polls TIDAL until the device login completes. Blocks for up to the code's
    /// lifetime (typically five minutes).
    pub fn complete_login(&mut self) -> Result<(), String> {
        let (device_code, expires_at) = match self.device.as_ref() {
            Some(device) => (device.device_code.clone(), device.expires_at),
            None => return Err("No pending TIDAL login. Call login() first.".to_string()),
        };
        let mut interval = self
            .device
            .as_ref()
            .map(|device| device.interval)
            .unwrap_or(5)
            .clamp(1, 30);
        let mut consecutive_errors = 0u32;

        loop {
            if now_secs() >= expires_at {
                self.device = None;
                return Err("TIDAL login code expired. Call login() to get a new link.".to_string());
            }
            std::thread::sleep(Duration::from_secs(interval));
            if now_secs() >= expires_at {
                self.device = None;
                return Err("TIDAL login code expired. Call login() to get a new link.".to_string());
            }

            match self.poll_device_token(&device_code) {
                Ok(token) => {
                    self.apply_token(token, AuthClient::Device)?;
                    self.device = None;
                    self.pkce = None;
                    if let Err(error) = self.fetch_session() {
                        eprintln!("tsun: could not read TIDAL session details: {error}");
                    }
                    self.save_session();
                    return Ok(());
                }
                Err(PollError::Pending) => {
                    consecutive_errors = 0;
                }
                Err(PollError::SlowDown) => {
                    interval = interval.saturating_add(5).min(30);
                    consecutive_errors = 0;
                }
                Err(PollError::Fatal(message)) => {
                    self.device = None;
                    return Err(message);
                }
                Err(PollError::Transient(message)) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    if consecutive_errors >= 5 {
                        return Err(format!("TIDAL login failed: {message}"));
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Authorization code + PKCE (needed for hi-res FLAC)

    /// Starts the PKCE flow and returns the TIDAL login URL.
    ///
    /// The user logs in through a browser. TIDAL redirects to an "Oops" page on
    /// `tidal.com`; the user copies that full URL and passes it to
    /// [`Client::finish_pkce`].
    pub fn start_pkce(&mut self) -> Result<String, String> {
        let mut verifier_bytes = [0u8; 32];
        getrandom::fill(&mut verifier_bytes)
            .map_err(|error| format!("could not generate PKCE verifier: {error}"))?;
        let code_verifier =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

        let code_challenge = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(code_verifier.as_bytes());
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(hasher.finalize().as_slice())
        };

        let mut key_bytes = [0u8; 8];
        getrandom::fill(&mut key_bytes)
            .map_err(|error| format!("could not generate PKCE client key: {error}"))?;
        let client_unique_key: String = key_bytes
            .iter()
            .map(|byte| format!("{:02x}", *byte))
            .collect();

        let url = format!(
            "{AUTHORIZE_URL}?response_type=code&redirect_uri={PKCE_REDIRECT_URI_ENCODED}\
             &client_id={PKCE_CLIENT_ID}&lang=EN&appMode=android\
             &client_unique_key={client_unique_key}&code_challenge={code_challenge}\
             &code_challenge_method=S256&restrict_signup=true"
        );
        self.pkce = Some(PkcePending {
            code_verifier,
            client_unique_key,
        });
        Ok(url)
    }

    /// Completes the PKCE flow from the URL TIDAL redirected the browser to.
    ///
    /// Accepts either the full redirected URL (with `?code=...`) or the bare
    /// authorization code.
    pub fn finish_pkce(&mut self, redirect_url: &str) -> Result<(), String> {
        let (code_verifier, client_unique_key) = match self.pkce.as_ref() {
            Some(pending) => (
                pending.code_verifier.clone(),
                pending.client_unique_key.clone(),
            ),
            None => return Err("No pending PKCE login. Call start_pkce() first.".to_string()),
        };
        let code = extract_authorization_code(redirect_url)
            .ok_or_else(|| "The pasted URL does not contain a TIDAL authorization code.".to_string())?;

        let mut response = self
            .agent
            .post(&format!("{AUTH_BASE}/token"))
            .header("User-Agent", USER_AGENT)
            .send_form([
                ("code", code.as_str()),
                ("client_id", PKCE_CLIENT_ID),
                ("grant_type", "authorization_code"),
                ("redirect_uri", PKCE_REDIRECT_URI),
                ("scope", PKCE_SCOPE),
                ("code_verifier", code_verifier.as_str()),
                ("client_unique_key", client_unique_key.as_str()),
            ])
            .map_err(|error| format!("could not complete TIDAL PKCE login: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("could not read TIDAL PKCE response: {error}"))?;
        if status != 200 {
            return Err(api_error_message(status, &body));
        }
        let token: TokenResponse = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL PKCE token response: {error}"))?;
        self.apply_token(token, AuthClient::Pkce)?;
        self.pkce = None;
        self.device = None;
        if let Err(error) = self.fetch_session() {
            eprintln!("tsun: could not read TIDAL session details: {error}");
        }
        self.save_session();
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Home

    /// Loads the signed-in user's favourite tracks to fill the home screen.
    ///
    /// Uses TIDAL's authenticated v1 collection endpoint
    /// `GET /users/{userId}/favorites/tracks`, the source the official client's
    /// "My Tracks" collection is built from. Returned tracks are playable
    /// through [`Client::play`].
    ///
    /// An empty collection is returned as an empty list.
    pub fn home_tracks(&mut self) -> Result<Vec<Track>, String> {
        let has_session = self.access_token.is_some()
            || self
                .refresh_token
                .as_deref()
                .is_some_and(|token| !token.is_empty());
        if !has_session {
            return Err("Not signed in to TIDAL; sign in to load your home tracks".to_string());
        }
        // Refresh lazily, then make sure we know which user's favourites to read.
        self.ensure_access_token()?;
        if self.user_id.is_none() {
            self.fetch_session()?;
        }
        let user_id = self.user_id.ok_or_else(|| {
            "TIDAL session has no user id; sign out and sign in again to load home tracks"
                .to_string()
        })?;
        let mut params: Vec<(&str, String)> = vec![
            ("limit", HOME_FAVORITES_LIMIT.to_string()),
            ("offset", "0".to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let path = format!("/users/{user_id}/favorites/tracks");
        let body = self.api_get(&path, &params)?;
        let parsed: FavoriteTracksResponse = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL favorites response: {error}"))?;
        let mut tracks = Vec::with_capacity(parsed.items.len());
        for entry in parsed.items {
            if let Some(item) = entry.item {
                if let Some(track) = parse_track(item) {
                    tracks.push(track);
                }
            }
        }
        Ok(tracks)
    }

    /// Loads track sections from TIDAL's home feed.
    pub fn home_recommendations(&mut self) -> Result<Vec<HomeSection>, String> {
        let mut params = vec![("deviceType", "BROWSER".to_owned())];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let body = self.api_get("/pages/home", &params)?;
        let page: Value = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL home response: {error}"))?;
        Ok(parse_home_sections(&page))
    }

    /// The home feed as generic card sections (albums, playlists, tracks…).
    ///
    /// Complements [`Client::home_recommendations`], which returns only the track
    /// rows. Built on the generic [`Client::page`] parser, so it also surfaces the
    /// album and playlist modules of `/pages/home`.
    pub fn home_sections(&mut self) -> Result<Vec<Section>, String> {
        self.page("/pages/home")
    }

    /// Lists the account's personalized mixes.
    pub fn mixes(&mut self) -> Result<Vec<Mix>, String> {
        let mut params = vec![("deviceType", "BROWSER".to_owned())];
        if let Some(country) = self.country_code.clone() { params.push(("countryCode", country)); }
        let body = self.api_get("/pages/my_collection_my_mixes", &params)?;
        let page: Value = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL mixes response: {error}"))?;
        Ok(parse_mixes(&page))
    }

    /// Loads the first page of playable tracks in a personalized mix.
    pub fn mix_tracks(&mut self, id: &str) -> Result<Vec<Track>, String> {
        if id.is_empty() || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("Invalid mix ID".to_string());
        }
        let mut params = vec![("limit", "100".to_owned())];
        if let Some(country) = self.country_code.clone() { params.push(("countryCode", country)); }
        let body = self.api_get(&format!("/mixes/{id}/items"), &params)?;
        let page: Value = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL mix response: {error}"))?;
        let items = page.get("items").and_then(Value::as_array)
            .ok_or_else(|| "TIDAL returned an invalid mix".to_string())?;
        Ok(items.iter().filter(|entry| entry.get("type").and_then(Value::as_str) == Some("track")
                || entry.get("type").and_then(Value::as_str) == Some("TRACK"))
            .filter_map(|entry| entry.get("item"))
            .filter_map(|item| serde_json::from_value::<SearchTrack>(item.clone()).ok())
            .filter_map(parse_track).collect())
    }

    /// Lists the signed-in user's playlists.
    pub fn playlists(&mut self) -> Result<Vec<Playlist>, String> {
        self.ensure_access_token()?;
        if self.user_id.is_none() { self.fetch_session()?; }
        let user_id = self.user_id.ok_or_else(|| "TIDAL session has no user id".to_string())?;
        let mut params = vec![("limit", "50".to_owned())];
        if let Some(country) = self.country_code.clone() { params.push(("countryCode", country)); }
        let body = self.api_get(&format!("/users/{user_id}/playlists"), &params)?;
        let data: PlaylistList = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL playlists response: {error}"))?;
        Ok(data.items.into_iter().map(|item| {
            let creator = item.creator.and_then(|creator| creator.id);
            Playlist {
                id: item.uuid,
                title: item.title,
                cover: item.square_image.or(item.image),
                count: item.number_of_tracks,
                description: non_empty(item.description),
                creator,
                public: item.public_playlist,
                owned: creator.is_none_or(|creator| creator == user_id),
            }
        }).collect())
    }

    /// Loads the first page of tracks in a playlist.
    pub fn playlist_tracks(&mut self, id: &str) -> Result<Vec<Track>, String> {
        if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err("Invalid playlist ID".to_string());
        }
        let mut params = vec![("limit", "100".to_owned())];
        if let Some(country) = self.country_code.clone() { params.push(("countryCode", country)); }
        let body = self.api_get(&format!("/playlists/{id}/tracks"), &params)?;
        let data: SearchTracks = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL playlist response: {error}"))?;
        Ok(data.items.into_iter().filter_map(parse_track).collect())
    }

    /// Loads one playlist's metadata: title, description, creator and public flag.
    ///
    /// `GET /playlists/{uuid}`. The page's own request, so description and ownership are
    /// authoritative even when the playlist was reached from a search result that carried
    /// only a title.
    pub fn playlist(&mut self, id: &str) -> Result<Playlist, String> {
        validate_playlist_id(id)?;
        let user_id = self.require_user_id().ok();
        let value = self.api_json(&format!("/playlists/{id}"), &self.country_params())?;
        parse_playlist_json(&value, user_id).ok_or_else(|| "unexpected TIDAL playlist response".to_string())
    }

    // -----------------------------------------------------------------------------------------
    // Rich search

    /// Searches TIDAL's catalogue across tracks, albums, artists and playlists.
    ///
    /// `GET /search?types=TRACKS,ALBUMS,ARTISTS,PLAYLISTS`. When TIDAL returns a
    /// `topHit`, it is exposed as [`SearchAll::top`]. An empty query returns an
    /// empty result without contacting TIDAL.
    pub fn search_all(&mut self, query: &str) -> Result<SearchAll, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchAll::default());
        }
        let mut params: Vec<(&str, String)> = vec![
            ("query", query.to_string()),
            ("types", "TRACKS,ALBUMS,ARTISTS,PLAYLISTS,VIDEOS".to_string()),
            ("limit", SEARCH_ALL_LIMIT.to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let value = self.api_json("/search", &params)?;
        Ok(parse_search_all(&value))
    }

    /// Best-effort search-box suggestions for `query`.
    ///
    /// Tries TIDAL's v2 suggestion endpoint (`/search/suggestions`, falling back
    /// to `/suggestions`); when neither is available — the endpoint is not
    /// exposed to every client — it derives suggestions from [`Client::search_all`]
    /// top names (the top hit, then the leading tracks, albums, artists and
    /// playlists), de-duplicated and capped at 10.
    pub fn search_suggestions(&mut self, query: &str) -> Result<Vec<String>, String> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut params: Vec<(&str, String)> = vec![("query", query.to_string())];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        for candidate in ["/search/suggestions", "/suggestions"] {
            if let Ok(value) = self.api_json_at(API_V2_BASE, candidate, &params) {
                let suggestions = parse_suggestions(&value);
                if !suggestions.is_empty() {
                    return Ok(suggestions);
                }
            }
        }
        let all = self.search_all(query)?;
        Ok(suggestion_names(&all))
    }

    // -----------------------------------------------------------------------------------------
    // Favourites

    /// The signed-in user's favourite tracks, newest first, across every page.
    pub fn favorite_tracks(&mut self) -> Result<Vec<Track>, String> {
        let user_id = self.require_user_id()?;
        let entries = self.fetch_pages(&format!("/users/{user_id}/favorites/tracks"))?;
        let mut tracks: Vec<(String, Track)> = Vec::new();
        for entry in &entries {
            let created = entry.get("created").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(track) = track_from_value(wrapped_item(entry)) {
                tracks.push((created, track));
            }
        }
        tracks.sort_by(|a, b| b.0.cmp(&a.0));
        // TIDAL pads this list with repeated entries (a different set each request: a library
        // of 306 distinct tracks came back as 331 entries); each track appears once.
        Ok(dedupe_by(tracks.into_iter().map(|(_, track)| track).collect(), |track| track.id))
    }

    /// The signed-in user's favourite albums, newest first, across every page.
    pub fn favorite_albums(&mut self) -> Result<Vec<Album>, String> {
        let user_id = self.require_user_id()?;
        let entries = self.fetch_pages(&format!("/users/{user_id}/favorites/albums"))?;
        let mut albums: Vec<(String, Album)> = Vec::new();
        for entry in &entries {
            let created = entry.get("created").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(album) = parse_album_json(wrapped_item(entry)) {
                albums.push((created, album));
            }
        }
        albums.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(dedupe_by(albums.into_iter().map(|(_, album)| album).collect(), |album| album.id))
    }

    /// The signed-in user's favourite music videos, newest first, across every page.
    pub fn favorite_videos(&mut self) -> Result<Vec<Video>, String> {
        let user_id = self.require_user_id()?;
        let entries = self.fetch_pages(&format!("/users/{user_id}/favorites/videos"))?;
        let mut videos: Vec<(String, Video)> = Vec::new();
        for entry in &entries {
            let created = entry.get("created").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(video) = parse_video_json(wrapped_item(entry)) {
                videos.push((created, video));
            }
        }
        videos.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(dedupe_by(videos.into_iter().map(|(_, video)| video).collect(), |video| video.id))
    }

    /// The signed-in user's favourite artists, newest first, across every page.
    pub fn favorite_artists(&mut self) -> Result<Vec<Artist>, String> {
        let user_id = self.require_user_id()?;
        let entries = self.fetch_pages(&format!("/users/{user_id}/favorites/artists"))?;
        let mut artists: Vec<(String, Artist)> = Vec::new();
        for entry in &entries {
            let created = entry.get("created").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(artist) = parse_artist_json(wrapped_item(entry)) {
                artists.push((created, artist));
            }
        }
        artists.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(dedupe_by(artists.into_iter().map(|(_, artist)| artist).collect(), |artist| artist.id))
    }

    /// The user's playlists: the ones they own plus the ones they favourited,
    /// de-duplicated by id. Either source may be empty; a failure of one source
    /// is tolerated as long as the user is signed in.
    pub fn favorite_playlists(&mut self) -> Result<Vec<Playlist>, String> {
        self.ensure_access_token()?;
        let mut playlists: Vec<Playlist> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        if let Ok(owned) = self.playlists() {
            for playlist in owned {
                if seen.insert(playlist.id.clone()) {
                    playlists.push(playlist);
                }
            }
        }
        if let Ok(user_id) = self.require_user_id() {
            if let Ok(entries) =
                self.fetch_pages(&format!("/users/{user_id}/favorites/playlists"))
            {
                for entry in &entries {
                    if let Some(playlist) = parse_playlist_json(wrapped_item(entry), Some(user_id)) {
                        if seen.insert(playlist.id.clone()) {
                            playlists.push(playlist);
                        }
                    }
                }
            }
        }
        Ok(playlists)
    }

    /// The user's favourites as plain ids, for heart icons.
    ///
    /// TIDAL's `/users/{id}/favorites/ids` keys the collections in upper case
    /// (`TRACK`/`ALBUM`/`ARTIST`/`PLAYLIST`).
    pub fn favorite_ids(&mut self) -> Result<FavoriteIds, String> {
        let user_id = self.require_user_id()?;
        let value = self.api_json(
            &format!("/users/{user_id}/favorites/ids"),
            &self.country_params(),
        )?;
        Ok(FavoriteIds {
            tracks: numeric_id_set(&value, "TRACK"),
            albums: numeric_id_set(&value, "ALBUM"),
            artists: numeric_id_set(&value, "ARTIST"),
            playlists: string_id_set(&value, "PLAYLIST"),
            videos: numeric_id_set(&value, "VIDEO"),
        })
    }

    /// Adds (`on = true`) or removes (`on = false`) a favourite.
    ///
    /// Adding posts a form to `/users/{id}/favorites/{kind}`; removing issues
    /// `DELETE /users/{id}/favorites/{kind}/{id}`.
    pub fn set_favorite(&mut self, kind: FavKind, id: &str, on: bool) -> Result<(), String> {
        let user_id = self.require_user_id()?;
        let segment = match kind {
            FavKind::Track => "tracks",
            FavKind::Album => "albums",
            FavKind::Artist => "artists",
            FavKind::Playlist => "playlists",
            FavKind::Video => "videos",
        };
        match kind {
            FavKind::Playlist => {
                if id.len() != 36 || !id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
                {
                    return Err("Invalid playlist ID".to_string());
                }
            }
            _ => {
                if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("Invalid favourite id".to_string());
                }
            }
        }
        if on {
            let field = match kind {
                FavKind::Track => "trackId",
                FavKind::Album => "albumId",
                FavKind::Artist => "artistId",
                FavKind::Playlist => "uuids",
                FavKind::Video => "videoId",
            };
            let form = vec![(field, id.to_string())];
            let outcome = self.api_outcome_post_form_query(
                &format!("/users/{user_id}/favorites/{segment}"),
                &self.country_params(),
                &form,
            )?;
            expect_success(&outcome)?;
        } else {
            let outcome = self.api_outcome_delete_query(
                &format!("/users/{user_id}/favorites/{segment}/{id}"),
                &self.country_params(),
                &[],
            )?;
            expect_success(&outcome)?;
        }
        Ok(())
    }

    /// The signed-in user's whole collection in one call.
    pub fn library(&mut self) -> Result<Library, String> {
        Ok(Library {
            tracks: self.favorite_tracks()?,
            albums: self.favorite_albums()?,
            artists: self.favorite_artists()?,
            playlists: self.favorite_playlists()?,
            // A catalogue with no video favourites endpoint still has a working
            // collection; the tab simply stays empty.
            videos: self.favorite_videos().unwrap_or_default(),
        })
    }

    // -----------------------------------------------------------------------------------------
    // Albums and artists

    /// Loads an album and its full track list.
    ///
    /// `GET /albums/{id}` for the metadata plus every page of
    /// `GET /albums/{id}/items`; the item entries wrap each track in an `item`
    /// field.
    pub fn album(&mut self, id: u64) -> Result<(Album, Vec<Track>), String> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let value = self.api_json(&format!("/albums/{id}"), &params)?;
        let album = parse_album_json(&value)
            .ok_or_else(|| "TIDAL returned an invalid album".to_string())?;
        let entries = self.fetch_pages(&format!("/albums/{id}/items"))?;
        let tracks = entries
            .iter()
            .filter_map(|entry| track_from_value(wrapped_item(entry)))
            .collect();
        Ok((album, tracks))
    }

    /// Loads an artist page: bio, top tracks, albums, EPs/singles, compilations
    /// the artist appears on, and similar artists.
    ///
    /// The artist document must resolve; every other part tolerates an
    /// individual endpoint failure by returning an empty list.
    pub fn artist_page(&mut self, id: u64) -> Result<ArtistPage, String> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let artist_value = self.api_json(&format!("/artists/{id}"), &params)?;
        let artist = parse_artist_json(&artist_value).unwrap_or(Artist { id, ..Artist::default() });
        let bio = self
            .api_json(&format!("/artists/{id}/bio"), &params)
            .ok()
            .and_then(|value| {
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
            });
        let (top_tracks, top_tracks_total) = self
            .artist_top_tracks(id, TOP_TRACKS_LIMIT, 0)
            .unwrap_or_default();
        let albums = self.artist_albums(id, "ALBUMS", &params);
        let singles = self.artist_albums(id, "EPSANDSINGLES", &params);
        let appears_on = self.artist_albums(id, "COMPILATIONS", &params);
        let videos = {
            let mut video_params = params.clone();
            video_params.push(("limit", PAGE_LIMIT.to_string()));
            self.wrapped_items(&format!("/artists/{id}/videos"), &video_params)
                .iter()
                .filter_map(parse_video_json)
                .collect()
        };
        let similar = self
            .wrapped_items(&format!("/artists/{id}/similar"), &params)
            .iter()
            .filter_map(|item| parse_artist_json(item))
            .collect();
        Ok(ArtistPage { artist, bio, top_tracks, top_tracks_total, albums, singles, appears_on, videos, similar })
    }

    /// One page of an artist's top tracks, plus the total TIDAL reports for the
    /// artist (so the page can offer "Show all" only when there is more).
    ///
    /// `GET /artists/{id}/toptracks`. An endpoint failure is an error here; the
    /// artist page itself treats it as "no top tracks".
    pub fn artist_top_tracks(
        &mut self,
        id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Track>, Option<u64>), String> {
        let mut params: Vec<(&str, String)> = vec![
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
            ("deviceType", "BROWSER".to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let value = self.api_json(&format!("/artists/{id}/toptracks"), &params)?;
        let tracks = value_items(&value)
            .iter()
            .filter_map(|item| track_from_value(wrapped_item(item)))
            .collect();
        let total = value.get("totalNumberOfItems").and_then(Value::as_u64);
        Ok((tracks, total))
    }

    /// Every remaining top track of an artist, from `offset`, up to `max` more of them.
    ///
    /// `/artists/{id}/toptracks` pages at [`TOP_TRACKS_LIMIT`]; this walks the pages until
    /// TIDAL's reported total is reached or `max` tracks have been fetched, so one "Show all"
    /// covers all but the most prolific artists. A failure after the first page is not fatal:
    /// whatever was fetched comes back.
    pub fn artist_top_tracks_all(
        &mut self,
        id: u64,
        offset: u32,
        max: u32,
    ) -> Result<(Vec<Track>, Option<u64>), String> {
        let mut tracks: Vec<Track> = Vec::new();
        let mut total: Option<u64> = None;
        let mut offset = offset;
        let mut error = None;
        while (tracks.len() as u32) < max {
            let want = (max - tracks.len() as u32).min(TOP_TRACKS_LIMIT);
            match self.artist_top_tracks(id, want, offset) {
                Ok((page, page_total)) => {
                    total = page_total.or(total);
                    let short = (page.len() as u32) < want;
                    offset = offset.saturating_add(page.len() as u32);
                    let reached_total = total.is_some_and(|total| u64::from(offset) >= total);
                    tracks.extend(page);
                    if short || reached_total || tracks.is_empty() {
                        break;
                    }
                }
                Err(failure) => {
                    error = Some(failure);
                    break;
                }
            }
        }
        match error {
            Some(failure) if tracks.is_empty() => Err(failure),
            _ => Ok((tracks, total)),
        }
    }

    /// Loads an album's editorial review, plus its credits and TIDAL's related
    /// albums.
    ///
    /// `GET /albums/{id}/review`, `/credits` and `/similar`. Each is optional
    /// and independent: a 404 or a parse failure leaves that part empty, so a
    /// missing review never stops the extras from loading.
    pub fn album_extras(&mut self, id: u64) -> AlbumExtras {
        let review = self.album_review(id).ok().flatten();
        let credits = self
            .api_json(&format!("/albums/{id}/credits"), &self.country_params())
            .map(|value| parse_credits(&value))
            .unwrap_or_default();
        let similar = self
            .api_json(&format!("/albums/{id}/similar"), &self.country_params())
            .map(|value| {
                value_items(&value)
                    .iter()
                    .filter_map(|item| parse_album_json(wrapped_item(item)))
                    .collect()
            })
            .unwrap_or_default();
        AlbumExtras { review, credits, similar }
    }

    /// `GET /albums/{id}/review`. `Ok(None)` when TIDAL has no review for the
    /// album (HTTP 404); the text is flattened to plain prose.
    pub fn album_review(&mut self, id: u64) -> Result<Option<AlbumReview>, String> {
        let outcome = self.api_outcome_get_at(
            API_BASE,
            &format!("/albums/{id}/review"),
            &self.country_params(),
        )?;
        if outcome.status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&outcome.status) {
            return Err(api_error_message(outcome.status, &outcome.body));
        }
        let value: Value = serde_json::from_str(&outcome.body)
            .map_err(|error| format!("unexpected TIDAL review response: {error}"))?;
        Ok(parse_album_review(&value))
    }

    /// Tracks TIDAL recommends after `track_id` (the "track radio").
    pub fn track_radio(&mut self, track_id: u64) -> Result<Vec<Track>, String> {
        let mut params: Vec<(&str, String)> = vec![("limit", PAGE_LIMIT.to_string())];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let value = self.api_json(&format!("/tracks/{track_id}/radio"), &params)?;
        Ok(value_items(&value)
            .iter()
            .filter_map(|item| track_from_value(wrapped_item(item)))
            .collect())
    }

    /// Tracks TIDAL recommends from an artist (the "artist radio").
    pub fn artist_radio(&mut self, artist_id: u64) -> Result<Vec<Track>, String> {
        let mut params: Vec<(&str, String)> = vec![("limit", PAGE_LIMIT.to_string())];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let value = self.api_json(&format!("/artists/{artist_id}/radio"), &params)?;
        Ok(value_items(&value)
            .iter()
            .filter_map(|item| track_from_value(wrapped_item(item)))
            .collect())
    }

    /// Loads a track's credits, grouped by role.
    ///
    /// `GET /tracks/{id}/credits`. TIDAL answers with `[{type, contributors: [{name, id?}]}]`;
    /// roles with no named contributors are dropped. A track TIDAL has no credits for returns
    /// an empty list rather than an error.
    pub fn track_credits(&mut self, track_id: u64) -> Result<Vec<Credit>, String> {
        let value = self.api_json(&format!("/tracks/{track_id}/credits"), &self.country_params())?;
        Ok(parse_credits(&value))
    }

    /// Loads a track's lyrics.
    ///
    /// `GET /tracks/{id}/lyrics` with `deviceType=BROWSER` and `locale=en_US`.
    /// TIDAL's `subtitles` field is LRC, parsed into [`LyricsData::synced`].
    /// Returns `Ok(None)` when the track has no lyrics (HTTP 404).
    pub fn lyrics(&mut self, track_id: u64) -> Result<Option<LyricsData>, String> {
        let mut params: Vec<(&str, String)> = vec![
            ("deviceType", "BROWSER".to_string()),
            ("locale", "en_US".to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let outcome = self.api_outcome_get_at(
            API_BASE,
            &format!("/tracks/{track_id}/lyrics"),
            &params,
        )?;
        if outcome.status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&outcome.status) {
            return Err(api_error_message(outcome.status, &outcome.body));
        }
        let value: Value = serde_json::from_str(&outcome.body)
            .map_err(|error| format!("unexpected TIDAL lyrics response: {error}"))?;
        Ok(parse_lyrics(&value))
    }

    // -----------------------------------------------------------------------------------------
    // Playlist editing

    /// Creates a playlist and returns it.
    ///
    /// Tries v1 `POST /users/{id}/playlists` first, then the v2
    /// `/my-collection/playlists/folders/create-playlist` endpoint.
    pub fn create_playlist(&mut self, title: &str, description: &str) -> Result<Playlist, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("A playlist needs a title".to_string());
        }
        let user_id = self.require_user_id()?;
        // The user just created the playlist, so it is theirs to edit whatever the response says.
        let finalize = |mut playlist: Playlist| {
            playlist.owned = true;
            playlist.creator = playlist.creator.or(Some(user_id));
            playlist
        };
        let form = vec![("title", title.to_string()), ("description", description.to_string())];
        if let Ok(outcome) =
            self.api_outcome_post_form(&format!("/users/{user_id}/playlists"), &form)
        {
            if (200..300).contains(&outcome.status) {
                if let Ok(value) = serde_json::from_str::<Value>(&outcome.body) {
                    if let Some(playlist) = parse_playlist_json(&value, Some(user_id)) {
                        return Ok(finalize(playlist));
                    }
                }
            }
        }
        let form = vec![
            ("title", title.to_string()),
            ("description", description.to_string()),
            ("folderId", "root".to_string()),
        ];
        let outcome = self.api_outcome_post_form_at(
            API_V2_BASE,
            "/my-collection/playlists/folders/create-playlist",
            &form,
        )?;
        expect_success(&outcome)?;
        let value: Value = serde_json::from_str(&outcome.body)
            .map_err(|error| format!("unexpected TIDAL create-playlist response: {error}"))?;
        parse_playlist_json(&value, Some(user_id))
            .map(finalize)
            .ok_or_else(|| "TIDAL did not return the new playlist".to_string())
    }

    /// Appends tracks to a playlist, skipping duplicates.
    ///
    /// `POST /playlists/{uuid}/items` with `onDupes=SKIP`. The playlist's current
    /// ETag is fetched first and sent as `If-None-Match`, which TIDAL requires to
    /// version-guard the edit.
    pub fn add_to_playlist(&mut self, playlist_uuid: &str, track_ids: &[u64]) -> Result<(), String> {
        validate_playlist_id(playlist_uuid)?;
        if track_ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = track_ids.iter().map(u64::to_string).collect();
        let form = vec![
            ("trackIds", ids.join(",")),
            ("onDupes", "SKIP".to_string()),
        ];
        let headers = self.conditional_headers(playlist_uuid)?;
        let outcome = self.api_outcome_post_form_headers(
            &format!("/playlists/{playlist_uuid}/items"),
            &form,
            &headers,
        )?;
        expect_success(&outcome)
    }

    /// Removes the playlist item at `index` (0-based).
    pub fn remove_from_playlist(&mut self, playlist_uuid: &str, index: u64) -> Result<(), String> {
        validate_playlist_id(playlist_uuid)?;
        let headers = self.conditional_headers(playlist_uuid)?;
        let outcome = self.api_outcome_delete(
            &format!("/playlists/{playlist_uuid}/items/{index}"),
            &headers,
        )?;
        expect_success(&outcome)
    }

    /// Moves the playlist item at `from` (0-based) to position `to`.
    ///
    /// `POST /playlists/{uuid}/items/{from}` with a form body of `toIndex={to}`
    /// and the playlist's current ETag as `If-None-Match`. TIDAL shifts the
    /// items between the two positions, so `to` is the item's final index in
    /// the resulting order. Verified against the live v1 API.
    pub fn move_playlist_item(
        &mut self,
        playlist_uuid: &str,
        from: u64,
        to: u64,
    ) -> Result<(), String> {
        validate_playlist_id(playlist_uuid)?;
        if from == to {
            return Ok(());
        }
        let form = vec![("toIndex", to.to_string())];
        let headers = self.conditional_headers(playlist_uuid)?;
        let outcome = self.api_outcome_post_form_headers(
            &format!("/playlists/{playlist_uuid}/items/{from}"),
            &form,
            &headers,
        )?;
        expect_success(&outcome)
    }

    /// Deletes a playlist the user owns.
    pub fn delete_playlist(&mut self, playlist_uuid: &str) -> Result<(), String> {
        validate_playlist_id(playlist_uuid)?;
        let outcome = self.api_outcome_delete(&format!("/playlists/{playlist_uuid}"), &[])?;
        expect_success(&outcome)
    }

    /// Renames (and re-describes) a playlist.
    pub fn rename_playlist(
        &mut self,
        playlist_uuid: &str,
        title: &str,
        description: &str,
    ) -> Result<(), String> {
        validate_playlist_id(playlist_uuid)?;
        let form = vec![
            ("title", title.trim().to_string()),
            ("description", description.to_string()),
        ];
        let headers = self.conditional_headers(playlist_uuid)?;
        let outcome = self
            .api_outcome_post_form_headers(&format!("/playlists/{playlist_uuid}"), &form, &headers)?;
        expect_success(&outcome)
    }

    // -----------------------------------------------------------------------------------------
    // Pages (explore, genres, moods)

    /// Parses any TIDAL page into titled card sections.
    ///
    /// Works for `/pages/explore`, `/pages/genre...`, `/pages/moods...`, etc.
    /// Recognises `ALBUM_LIST`, `PLAYLIST_LIST`, `ARTIST_LIST`, `TRACK_LIST` and
    /// `MIX_LIST` modules; other modules (including `HIGHLIGHT_MODULE`) are
    /// skipped.
    pub fn page(&mut self, api_path: &str) -> Result<Vec<Section>, String> {
        if !api_path.starts_with('/') {
            return Err("A TIDAL page path must start with '/'".to_string());
        }
        let mut params: Vec<(&str, String)> = vec![
            ("deviceType", "BROWSER".to_string()),
            ("limit", PAGE_LIMIT.to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let value = self.api_json(api_path, &params)?;
        Ok(parse_sections(&value))
    }

    /// The explore page, as generic sections. Tries the known explore paths in
    /// order and returns the first that yields content.
    pub fn explore(&mut self) -> Result<Vec<Section>, String> {
        let mut last_error = None;
        for candidate in ["/pages/explore", "/pages/explore_new_music"] {
            match self.page(candidate) {
                Ok(sections) if !sections.is_empty() => return Ok(sections),
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        match last_error {
            Some(error) => Err(error),
            None => Ok(Vec::new()),
        }
    }

    /// The new-releases page, as generic sections.
    ///
    /// TIDAL has no standalone v1 new-releases page; its "new music" page
    /// (`/pages/explore_new_music`) carries the new-release module. This returns
    /// that page's album sections, falling back to the whole page when no album
    /// module is present.
    pub fn new_releases(&mut self) -> Result<Vec<Section>, String> {
        let sections = self.page("/pages/explore_new_music")?;
        let albums: Vec<Section> = sections
            .iter()
            .filter(|section| section.items.iter().any(|card| matches!(card, Card::Album(_))))
            .cloned()
            .collect();
        if albums.is_empty() {
            Ok(sections)
        } else {
            Ok(albums)
        }
    }

    // -----------------------------------------------------------------------------------------
    // Playback quality preference

    /// Sets the preferred playback quality and persists it.
    pub fn set_quality_preference(&mut self, quality: Quality) {
        if self.quality != quality {
            self.quality = quality;
            self.save_session();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Profile

    /// Loads the signed-in user's listener profile: display name and avatar.
    ///
    /// The avatar comes from TIDAL's v2 listener profile, `GET /profiles/me`:
    /// its `picture.url` is the raw image UUID [`Profile::picture`] carries.
    ///
    /// The access token is refreshed lazily; when signed out this returns the
    /// token error rather than a profile.
    pub fn profile(&mut self) -> Result<Profile, String> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let body = self.api_get_at(API_V2_BASE, "/profiles/me", &params)?;
        let value: Value = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL profile response: {error}"))?;
        Ok(parse_profile(&value))
    }

    // -----------------------------------------------------------------------------------------
    // Playback

    /// Resolves a video's HLS stream.
    ///
    /// TIDAL's endpoint is a **GET** (`POST` answers 405); it returns an
    /// `application/vnd.tidal.emu` document whose base64 manifest wraps one
    /// `application/vnd.apple.mpegurl` URL — the master playlist. That playlist
    /// lists H.264+AAC MPEG-TS variants (320x180 through 1920x1080) with no
    /// `EXT-X-KEY`: the segments are plain, not encrypted, so they play without a
    /// CDM. A manifest that is not a plain HLS URL is reported as unsupported.
    pub fn video_stream(&mut self, id: u64) -> Result<VideoStream, String> {
        let mut params: Vec<(&str, String)> = vec![
            ("videoquality", "HIGH".to_string()),
            ("assetpresentation", "FULL".to_string()),
            ("playbackmode", "STREAM".to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let path = format!("/videos/{id}/playbackinfopostpaywall");
        let body = self.api_get(&path, &params)?;
        let info: VideoPlaybackInfo = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL video playback response: {error}"))?;
        let manifest = decode_video_manifest(&info.manifest, info.manifest_mime_type.as_deref())?;
        Ok(VideoStream { master_url: manifest })
    }

    /// Resolves a track and starts in-process audio playback.
    ///
    /// Requests the user's preferred tier (see [`Client::set_quality_preference`]),
    /// then falls back through the lower tiers. If TIDAL answers a request with a
    /// lower quality than requested, the remaining tiers are probed and the best
    /// quality actually served is used. The returned string is that quality plus
    /// the codec/resolution TIDAL reported; if TIDAL omits `audioQuality`, the
    /// label is `UNKNOWN` rather than the requested tier.
    ///
    /// The call blocks while resolving and starting the stream.
    pub fn play(&mut self, id: u64) -> Result<String, String> {
        self.stop();
        let stream = self.resolve_best_stream(id)?;
        self.start_stream(stream)
    }

    /// Resolves `id` to the best stream TIDAL will serve for the current quality preference.
    ///
    /// Lossless preferences go to the v2 `trackManifests` endpoint first: the v1
    /// `playbackinfopostpaywall` endpoint answers AAC (`HIGH`) at every tier for many tracks
    /// that do have FLAC, which showed up as "sometimes a lower quality than the track has".
    /// When that fails the v1 tiers are probed, best first.
    fn resolve_best_stream(&mut self, id: u64) -> Result<ResolvedStream, String> {
        let mut errors = Vec::new();
        if let Some(formats) = manifest_formats(self.quality) {
            let mut attempt = 0;
            loop {
                match self.resolve_stream_v2(id, formats) {
                    Ok(stream) => return Ok(stream),
                    Err(error) if is_transient_failure(&error) && attempt < RETRY_BACKOFF_MS.len() => {
                        std::thread::sleep(Duration::from_millis(RETRY_BACKOFF_MS[attempt]));
                        attempt += 1;
                    }
                    Err(error) => {
                        errors.push(format!("manifests: {error}"));
                        break;
                    }
                }
            }
        }
        let mut best: Option<(u8, ResolvedStream)> = None;
        for requested in quality_chain(self.quality) {
            // A tier that cannot beat what is already in hand is not worth a request: each
            // one counts against TIDAL's rate limit, and a rate-limited request is exactly
            // what used to push playback down to a lower tier.
            if !worth_probing(best.as_ref().map(|(rank, _)| *rank), requested) {
                continue;
            }
            match self.resolve_stream_retrying(id, requested) {
                Ok(stream) => {
                    let matches_requested = stream
                        .quality
                        .as_deref()
                        .is_some_and(|quality| quality.eq_ignore_ascii_case(requested));
                    let rank = quality_rank(stream.quality.as_deref());
                    let better = best
                        .as_ref()
                        .map_or(true, |(best_rank, _)| rank > *best_rank);
                    if better {
                        best = Some((rank, stream));
                    }
                    if matches_requested {
                        break;
                    }
                }
                Err(error) => errors.push(format!("{requested}: {error}")),
            }
        }
        match best {
            Some((_, stream)) => Ok(stream),
            None => Err(format!(
                "TIDAL could not start playback: {}",
                errors.join("; ")
            )),
        }
    }

    /// [`Client::resolve_stream`], retrying a transient failure (TIDAL's rate limit, a network
    /// hiccup) with a short backoff before giving up on the tier. Falling straight to the next
    /// tier on such an error would quietly play a lower quality than the track has.
    fn resolve_stream_retrying(&mut self, id: u64, requested: &str) -> Result<ResolvedStream, String> {
        let mut attempt = 0;
        loop {
            match self.resolve_stream(id, requested) {
                Err(error) if is_transient_failure(&error) && attempt < RETRY_BACKOFF_MS.len() => {
                    std::thread::sleep(Duration::from_millis(RETRY_BACKOFF_MS[attempt]));
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    /// Stops playback.
    pub fn stop(&mut self) {
        if let Some(mut player) = self.player.take() {
            player.stop();
        }
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), String> {
        let player = self.player.as_ref().ok_or_else(|| "No track is playing".to_string())?;
        player.set_paused(paused);
        Ok(())
    }

    /// Seeks the current player to `seconds` from the start of the track. A no-op when nothing
    /// is playing; the caller has already checked the playing sequence.
    pub fn seek(&self, seconds: f64) {
        if let Some(player) = self.player.as_ref() {
            player.seek(seconds);
        }
    }

    /// Reports playback completion or a late audio/network failure once.
    ///
    /// `Ok(Outcome::Finished)` is a natural end; `Ok(Outcome::Stopped)` means
    /// [`Client::stop`] (or a drop) ended it.
    pub fn poll_playback(&mut self) -> Option<Result<Outcome, String>> {
        let outcome = self.player.as_mut()?.poll()?;
        self.player.take();
        Some(outcome)
    }

    pub fn playback_position(&self) -> Option<u64> {
        self.player.as_ref().map(Player::position_seconds)
    }

    /// Chooses the output device for playback.
    ///
    /// Applied immediately to the track that is playing (the output is re-opened and resumes at
    /// the position that was playing) and used for every later one. A device name that no longer
    /// exists is reported as a playback error rather than silently ignored.
    pub fn set_output_device(&mut self, device: Option<String>) {
        self.output_device = device.clone();
        if let Some(player) = self.player.as_ref() {
            player.set_output_device(device);
        }
    }

    /// The format the current output stream is really running at, for the UI.
    pub fn output_info(&self) -> Option<OutputInfo> {
        self.player.as_ref()?.output_info()
    }

    /// Prepares `id` so it can start seamlessly when the current track ends.
    ///
    /// Resolves the stream on the worker thread and hands it to the audio layer, which opens and
    /// decodes it ahead of time. Nothing about the current playback changes; a failure simply
    /// means the track starts the normal way when its turn comes.
    pub fn preload(&mut self, id: u64) -> Result<(), String> {
        let stream = self.resolve_best_stream(id)?;
        let gain = match (self.normalize, stream.replay_gain_db) {
            (true, Some(db)) => replay_gain_linear(db),
            _ => 1.0,
        };
        let player = self
            .player
            .as_ref()
            .ok_or_else(|| "nothing is playing".to_string())?;
        player.preload(id, stream.source, gain)
    }

    /// Drops any track prepared by [`Client::preload`], e.g. because the queue changed.
    pub fn cancel_preload(&mut self) {
        if let Some(player) = self.player.as_ref() {
            player.cancel_preload();
        }
    }

    /// The preloaded track the audio thread has swapped in, reported once.
    ///
    /// Playback never stopped for it, so this is how the caller learns the queue has advanced
    /// without a fresh `play`.
    pub fn take_gapless_switch(&mut self) -> Option<u64> {
        self.player.as_ref()?.take_gapless_switch()
    }

    /// The shared output-volume handle. Clone it and call `set_volume`/`set_muted`
    /// on it from any thread; it outlives individual tracks.
    pub fn volume(&self) -> Volume {
        self.volume.clone()
    }

    /// Stops playback and forgets the persisted TIDAL session.
    pub fn logout(&mut self) {
        self.stop();
        self.clear_session();
        self.device = None;
        self.pkce = None;
        self.auth_client = AuthClient::Device;
    }

    // -----------------------------------------------------------------------------------------
    // Internal: playback resolution

    fn resolve_stream(&mut self, id: u64, requested: &str) -> Result<ResolvedStream, String> {
        let mut params: Vec<(&str, String)> = vec![
            ("audioquality", requested.to_string()),
            ("playbackmode", "STREAM".to_string()),
            ("assetpresentation", "FULL".to_string()),
        ];
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let path = format!("/tracks/{id}/playbackinfopostpaywall");
        let body = self.api_get(&path, &params)?;
        let info: PlaybackInfo = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL playback response: {error}"))?;
        let quality = info
            .audio_quality
            .clone()
            .filter(|quality| !quality.trim().is_empty());
        let manifest = decode_manifest(&info.manifest, info.manifest_mime_type.as_deref())?;
        let (source, codec) = match manifest {
            PlayableManifest::Url { url, codec } => (Source::Url(url), codec),
            PlayableManifest::EncryptedUrl { url, codec, key, nonce } =>
                (Source::EncryptedUrl { url, key, nonce }, codec),
            PlayableManifest::Hls { playlist, codec } => (Source::Hls(playlist), codec),
        };
        Ok(ResolvedStream {
            quality,
            codec,
            bit_depth: info.bit_depth,
            sample_rate: info.sample_rate,
            replay_gain_db: info.replay_gain.or(info.album_replay_gain),
            source,
        })
    }

    /// Resolves `id` through the v2 `trackManifests` endpoint, which names the formats it may
    /// serve (`FLAC_HIRES`, `FLAC`, `AACLC`…) and answers with the best of them as an
    /// unencrypted DASH manifest; a DRM-protected one is refused like everywhere else.
    fn resolve_stream_v2(&mut self, id: u64, formats: &[&str]) -> Result<ResolvedStream, String> {
        let mut params: Vec<(&str, String)> = vec![
            ("manifestType", "MPEG_DASH".to_string()),
            ("uriScheme", "HTTPS".to_string()),
            ("usage", "PLAYBACK".to_string()),
            ("adaptive", "false".to_string()),
        ];
        for format in formats {
            params.push(("formats", (*format).to_string()));
        }
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let body = self.api_get_at(OPENAPI_BASE, &format!("/trackManifests/{id}"), &params)?;
        let value: Value = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL track manifest response: {error}"))?;
        let manifest = parse_track_manifest(&value)?;
        let mpd = self.fetch_manifest_text(&manifest.uri)?;
        let (playlist, codec) = match parse_dash(&mpd)? {
            PlayableManifest::Hls { playlist, codec } => (playlist, codec),
            _ => return Err("TIDAL returned an unexpected manifest kind".to_string()),
        };
        let (sample_rate, bit_depth) = opening_tag(&mpd, "Representation")
            .and_then(|tag| xml_attr(tag, "id"))
            .map(|id| parse_representation_id(&id))
            .unwrap_or((None, None));
        Ok(ResolvedStream {
            quality: manifest.format.as_deref().and_then(manifest_quality).map(str::to_owned),
            codec,
            bit_depth,
            sample_rate,
            replay_gain_db: manifest.replay_gain,
            source: Source::Hls(playlist),
        })
    }

    /// Downloads a manifest document from a TIDAL host.
    fn fetch_manifest_text(&self, url: &str) -> Result<String, String> {
        let host_ok = url
            .strip_prefix("https://")
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|host| host == "tidal.com" || host.ends_with(".tidal.com"));
        if !host_ok {
            return Err("TIDAL returned a manifest URL outside tidal.com".to_string());
        }
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|error| format!("TIDAL network error: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("TIDAL network error: could not read the manifest: {error}"))?;
        if (200..300).contains(&status) {
            Ok(body)
        } else if status == 429 {
            Err("TIDAL rate limit reached; try again in a moment".to_string())
        } else {
            Err(api_error_message(status, &body))
        }
    }

    fn start_stream(&mut self, stream: ResolvedStream) -> Result<String, String> {
        self.replay_gain = stream.replay_gain_db.map(replay_gain_linear);
        self.apply_normalization();
        let output = OutputOptions { device: self.output_device.clone() };
        self.player = Some(Player::start(stream.source, self.volume.clone(), output)?);
        Ok(quality_text(
            stream.quality.as_deref(),
            &stream.codec,
            stream.bit_depth,
            stream.sample_rate,
        ))
    }

    /// Turns ReplayGain-style normalization on or off for the current and future streams.
    pub fn set_normalize(&mut self, normalize: bool) {
        self.normalize = normalize;
        self.apply_normalization();
    }

    /// Pushes the effective normalization multiplier onto the shared volume handle: the current
    /// stream's gain while normalization is on, otherwise unity.
    fn apply_normalization(&self) {
        let gain = if self.normalize {
            self.replay_gain.unwrap_or(1.0)
        } else {
            1.0
        };
        self.volume.set_normalization_gain(gain);
    }


    // -----------------------------------------------------------------------------------------
    // Internal: HTTP and authentication

    fn api_get(&mut self, path: &str, query: &[(&str, String)]) -> Result<String, String> {
        self.api_get_at(API_BASE, path, query)
    }

    /// `api_get` against an explicit API base, so v2 resources can be fetched with
    /// the same lazy-refresh behaviour.
    fn api_get_at(
        &mut self,
        base: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String, String> {
        let token = self.ensure_access_token()?;
        match self.api_get_once_at(&token, base, path, query) {
            Ok(body) => Ok(body),
            Err(ApiFailure::Unauthorized) => {
                self.refresh_access_token()?;
                let token = self
                    .access_token
                    .clone()
                    .ok_or_else(|| "TIDAL did not return an access token".to_string())?;
                self.api_get_once_at(&token, base, path, query)
                    .map_err(ApiFailure::message)
            }
            Err(failure) => Err(failure.message()),
        }
    }

    fn api_get_once(
        &self,
        token: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String, ApiFailure> {
        self.api_get_once_at(token, API_BASE, path, query)
    }

    fn api_get_once_at(
        &self,
        token: &str,
        base: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<String, ApiFailure> {
        let url = format!("{base}{path}");
        let authorization = format!("Bearer {token}");
        let mut request = self
            .agent
            .get(url.as_str())
            .header("Authorization", authorization.as_str())
            .header("User-Agent", USER_AGENT);
        for (key, value) in query {
            request = request.query(*key, value.as_str());
        }
        let mut response = request
            .call()
            .map_err(|error| ApiFailure::Other(format!("TIDAL network error: {error}")))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| ApiFailure::Other(format!("could not read TIDAL response: {error}")))?;
        if (200..300).contains(&status) {
            Ok(body)
        } else if status == 401 {
            Err(ApiFailure::Unauthorized)
        } else if status == 429 {
            Err(ApiFailure::Other(
                "TIDAL rate limit reached; try again in a moment".to_string(),
            ))
        } else {
            Err(ApiFailure::Other(api_error_message(status, &body)))
        }
    }

    // -----------------------------------------------------------------------------------------
    // Internal: generic HTTP used by the richer endpoints

    /// Runs `call` with the current access token, refreshing once and retrying
    /// when TIDAL answers 401. Any other failure is turned into a message.
    fn request_with_refresh<T>(
        &mut self,
        call: impl Fn(&Client, &str) -> Result<T, ApiFailure>,
    ) -> Result<T, String> {
        let token = self.ensure_access_token()?;
        match call(self, &token) {
            Ok(value) => Ok(value),
            Err(ApiFailure::Unauthorized) => {
                self.refresh_access_token()?;
                let token = self
                    .access_token
                    .clone()
                    .ok_or_else(|| "TIDAL did not return an access token".to_string())?;
                call(self, &token).map_err(ApiFailure::message)
            }
            Err(failure) => Err(failure.message()),
        }
    }

    fn http_get_once(
        &self,
        token: &str,
        base: &str,
        path: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
    ) -> Result<HttpOutcome, ApiFailure> {
        let request = apply_request(self.agent.get(format!("{base}{path}")), token, query, headers);
        let response = request
            .call()
            .map_err(|error| ApiFailure::Other(format!("TIDAL network error: {error}")))?;
        read_outcome(response)
    }

    fn http_post_form_once(
        &self,
        token: &str,
        base: &str,
        path: &str,
        query: &[(&str, String)],
        form: &[(&str, String)],
        headers: &[(&str, String)],
    ) -> Result<HttpOutcome, ApiFailure> {
        let request = apply_request(self.agent.post(format!("{base}{path}")), token, query, headers);
        let response = request
            .send_form(form.iter().map(|(key, value)| (*key, value.as_str())))
            .map_err(|error| ApiFailure::Other(format!("TIDAL network error: {error}")))?;
        read_outcome(response)
    }

    fn http_delete_once(
        &self,
        token: &str,
        base: &str,
        path: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
    ) -> Result<HttpOutcome, ApiFailure> {
        let request = apply_request(self.agent.delete(format!("{base}{path}")), token, query, headers);
        let response = request
            .call()
            .map_err(|error| ApiFailure::Other(format!("TIDAL network error: {error}")))?;
        read_outcome(response)
    }

    fn api_outcome_get_at(
        &mut self,
        base: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.request_with_refresh(|client, token| client.http_get_once(token, base, path, query, &[]))
    }

    fn api_outcome_post_form(
        &mut self,
        path: &str,
        form: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.api_outcome_post_form_headers(path, form, &[])
    }

    fn api_outcome_post_form_headers(
        &mut self,
        path: &str,
        form: &[(&str, String)],
        headers: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.request_with_refresh(|client, token| {
            client.http_post_form_once(token, API_BASE, path, &[], form, headers)
        })
    }

    fn api_outcome_post_form_at(
        &mut self,
        base: &str,
        path: &str,
        form: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.request_with_refresh(|client, token| {
            client.http_post_form_once(token, base, path, &[], form, &[])
        })
    }

    fn api_outcome_post_form_query(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        form: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.request_with_refresh(|client, token| {
            client.http_post_form_once(token, API_BASE, path, query, form, &[])
        })
    }

    fn api_outcome_delete_query(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        headers: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.request_with_refresh(|client, token| {
            client.http_delete_once(token, API_BASE, path, query, headers)
        })
    }

    fn api_outcome_delete(
        &mut self,
        path: &str,
        headers: &[(&str, String)],
    ) -> Result<HttpOutcome, String> {
        self.request_with_refresh(|client, token| {
            client.http_delete_once(token, API_BASE, path, &[], headers)
        })
    }

    /// GETs a v1 resource and parses its JSON body, turning a non-2xx status
    /// into an error message.
    fn api_json(&mut self, path: &str, query: &[(&str, String)]) -> Result<Value, String> {
        self.api_json_at(API_BASE, path, query)
    }

    fn api_json_at(
        &mut self,
        base: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, String> {
        let outcome = self.api_outcome_get_at(base, path, query)?;
        expect_success(&outcome)?;
        serde_json::from_str(&outcome.body)
            .map_err(|error| format!("unexpected TIDAL response from {path}: {error}"))
    }

    /// The `countryCode` query parameter TIDAL expects, when the session knows
    /// the country. Some mutation endpoints reject requests without it.
    fn country_params(&self) -> Vec<(&'static str, String)> {
        match self.country_code.clone() {
            Some(country) => vec![("countryCode", country)],
            None => Vec::new(),
        }
    }

    /// The signed-in user's id, refreshing and fetching the session when needed.
    fn require_user_id(&mut self) -> Result<u64, String> {
        self.ensure_access_token()?;
        if self.user_id.is_none() {
            self.fetch_session()?;
        }
        self.user_id
            .ok_or_else(|| "TIDAL session has no user id; sign out and sign in again".to_string())
    }

    /// Every page of a paginated v1 collection, newly fetched from the top. The
    /// response's `totalNumberOfItems` bounds the walk; `MAX_PAGINATED_ITEMS`
    /// guards against a server that reports no total.
    fn fetch_pages(&mut self, path: &str) -> Result<Vec<Value>, String> {
        let mut entries: Vec<Value> = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let mut params: Vec<(&str, String)> = vec![
                ("limit", FAVORITES_PAGE_LIMIT.to_string()),
                ("offset", offset.to_string()),
            ];
            if let Some(country) = self.country_code.clone() {
                params.push(("countryCode", country));
            }
            let value = self.api_json(path, &params)?;
            let page = value
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if page.is_empty() {
                break;
            }
            offset += page.len() as u64;
            entries.extend(page);
            if entries.len() as u64 >= MAX_PAGINATED_ITEMS {
                break;
            }
            if let Some(total) = value.get("totalNumberOfItems").and_then(Value::as_u64) {
                if offset >= total {
                    break;
                }
            }
        }
        Ok(entries)
    }

    /// Fetches `{items:[...]}` from a v1 endpoint, unwrapping each `item`, and
    /// returns the raw item values. Tolerates a missing or failed endpoint by
    /// returning an empty list.
    fn wrapped_items(&mut self, path: &str, params: &[(&str, String)]) -> Vec<Value> {
        match self.api_json(path, params) {
            Ok(value) => value_items(&value)
                .into_iter()
                .map(wrapped_item)
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// One `filter=…` page of an artist's albums. Tolerates failure with an
    /// empty list.
    fn artist_albums(&mut self, id: u64, filter: &str, params: &[(&str, String)]) -> Vec<Album> {
        let mut params = params.to_vec();
        params.push(("limit", "50".to_string()));
        params.push(("filter", filter.to_string()));
        self.wrapped_items(&format!("/artists/{id}/albums"), &params)
            .iter()
            .filter_map(|item| parse_album_json(item))
            .collect()
    }

    /// `If-None-Match` header with the playlist's current ETag, when TIDAL sends
    /// one. Required to version-guard playlist mutations.
    fn conditional_headers(&mut self, playlist_uuid: &str) -> Result<Vec<(&'static str, String)>, String> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(country) = self.country_code.clone() {
            params.push(("countryCode", country));
        }
        let outcome = self.api_outcome_get_at(
            API_BASE,
            &format!("/playlists/{playlist_uuid}"),
            &params,
        )?;
        Ok(match outcome.etag {
            Some(etag) => vec![("If-None-Match", etag)],
            None => Vec::new(),
        })
    }

    fn ensure_access_token(&mut self) -> Result<String, String> {
        if let Some(token) = self.access_token.as_ref() {
            if self.expires_at > now_secs().saturating_add(TOKEN_REFRESH_MARGIN_SECS) {
                return Ok(token.clone());
            }
        }
        if self
            .refresh_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
        {
            self.refresh_access_token()?;
            return self
                .access_token
                .clone()
                .ok_or_else(|| "TIDAL did not return an access token".to_string());
        }
        Err("Not signed in to TIDAL. Call login()/complete_login() or start_pkce()/finish_pkce() first."
            .to_string())
    }

    fn refresh_access_token(&mut self) -> Result<(), String> {
        let refresh_token = self
            .refresh_token
            .clone()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "TIDAL session expired; sign in again".to_string())?;
        let (client_id, client_secret) = self.auth_credentials();
        let authorization = basic_auth(client_id, client_secret);
        let mut response = self
            .agent
            .post(&format!("{AUTH_BASE}/token"))
            .header("Authorization", authorization.as_str())
            .header("User-Agent", USER_AGENT)
            .send_form([
                ("client_id", client_id),
                ("refresh_token", refresh_token.as_str()),
                ("grant_type", "refresh_token"),
                ("scope", REFRESH_SCOPE),
            ])
            .map_err(|error| format!("could not refresh TIDAL session: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("could not read TIDAL refresh response: {error}"))?;
        if status != 200 {
            let message = api_error_message(status, &body);
            if body.contains("invalid_grant") || body.contains("invalid_token") {
                self.clear_session();
            }
            return Err(format!("TIDAL session refresh failed: {message}"));
        }
        let token: TokenResponse = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL refresh response: {error}"))?;
        let auth_client = self.auth_client;
        self.apply_token(token, auth_client)?;
        self.save_session();
        Ok(())
    }

    fn poll_device_token(&self, device_code: &str) -> Result<TokenResponse, PollError> {
        let authorization = basic_auth(DEVICE_CLIENT_ID, DEVICE_CLIENT_SECRET);
        let mut response = self
            .agent
            .post(&format!("{AUTH_BASE}/token"))
            .header("Authorization", authorization.as_str())
            .header("User-Agent", USER_AGENT)
            .send_form([
                ("client_id", DEVICE_CLIENT_ID),
                ("device_code", device_code),
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code",
                ),
                ("scope", DEVICE_SCOPE),
            ])
            .map_err(|error| PollError::Transient(format!("network error: {error}")))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| PollError::Transient(format!("could not read response: {error}")))?;
        if status == 200 {
            return serde_json::from_str(&body).map_err(|error| {
                PollError::Fatal(format!("unexpected TIDAL token response: {error}"))
            });
        }
        let parsed: Option<Value> = serde_json::from_str(&body).ok();
        let code = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let sub_status = parsed
            .as_ref()
            .and_then(|value| value.get("sub_status"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        match code {
            "authorization_pending" => Err(PollError::Pending),
            "slow_down" => Err(PollError::SlowDown),
            "expired_token" => Err(PollError::Fatal(
                "TIDAL login code expired. Call login() to get a new link.".to_string(),
            )),
            "access_denied" => Err(PollError::Fatal("TIDAL login was denied.".to_string())),
            _ if code.is_empty() && sub_status == 1002 => Err(PollError::Pending),
            _ => {
                let message = api_error_message(status, &body);
                if status == 429 || status >= 500 {
                    Err(PollError::Transient(message))
                } else {
                    Err(PollError::Fatal(message))
                }
            }
        }
    }

    fn fetch_session(&mut self) -> Result<(), String> {
        let token = self
            .access_token
            .clone()
            .ok_or_else(|| "not signed in".to_string())?;
        let body = self
            .api_get_once(&token, "/sessions", &[])
            .map_err(ApiFailure::message)?;
        let value: Value = serde_json::from_str(&body)
            .map_err(|error| format!("unexpected TIDAL session response: {error}"))?;
        if let Some(country) = value.get("countryCode").and_then(Value::as_str) {
            if !country.is_empty() {
                self.country_code = Some(country.to_string());
            }
        }
        if let Some(user_id) = value.get("userId").and_then(Value::as_u64) {
            self.user_id = Some(user_id);
        }
        self.save_session();
        Ok(())
    }

    fn apply_token(&mut self, token: TokenResponse, auth_client: AuthClient) -> Result<(), String> {
        if token.access_token.trim().is_empty() {
            return Err("TIDAL returned an empty access token".to_string());
        }
        self.access_token = Some(token.access_token);
        if let Some(refresh_token) = token.refresh_token {
            if !refresh_token.trim().is_empty() {
                self.refresh_token = Some(refresh_token);
            }
        }
        self.expires_at = now_secs().saturating_add(token.expires_in.max(1));
        if let Some(user) = token.user {
            if let Some(user_id) = user.user_id {
                self.user_id = Some(user_id);
            }
            if let Some(country) = user.country_code {
                if !country.trim().is_empty() {
                    self.country_code = Some(country);
                }
            }
        }
        if let Some(user_id) = token.user_id {
            self.user_id = Some(user_id);
        }
        self.auth_client = auth_client;
        Ok(())
    }

    fn auth_credentials(&self) -> (&'static str, &'static str) {
        match self.auth_client {
            AuthClient::Device => (DEVICE_CLIENT_ID, DEVICE_CLIENT_SECRET),
            AuthClient::Pkce => (PKCE_CLIENT_ID, PKCE_CLIENT_SECRET),
        }
    }

    fn clear_session(&mut self) {
        self.access_token = None;
        self.refresh_token = None;
        self.expires_at = 0;
        self.country_code = None;
        self.user_id = None;
        self.auth_client = AuthClient::Device;
        self.delete_session_file();
    }

    // -----------------------------------------------------------------------------------------
    // Internal: persistence

    fn load_session(&mut self, path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let Ok(data) = serde_json::from_str::<PersistedSession>(&text) else {
            return;
        };
        if data.access_token.is_empty() {
            return;
        }
        self.access_token = Some(data.access_token);
        if !data.refresh_token.is_empty() {
            self.refresh_token = Some(data.refresh_token);
        }
        self.expires_at = data.expires_at;
        if !data.country_code.is_empty() {
            self.country_code = Some(data.country_code);
        }
        self.user_id = data.user_id;
        self.auth_client = data.auth_client;
        self.quality = data.quality;
    }

    fn save_session(&self) {
        if let Err(error) = self.try_save_session() {
            eprintln!("tsun: could not save TIDAL session: {error}");
        }
    }

    fn try_save_session(&self) -> Result<(), String> {
        let Some(path) = self.session_path.as_deref() else {
            return Ok(());
        };
        let Some(access_token) = self.access_token.as_ref() else {
            return Ok(());
        };
        let data = PersistedSession {
            access_token: access_token.clone(),
            refresh_token: self.refresh_token.clone().unwrap_or_default(),
            expires_at: self.expires_at,
            country_code: self.country_code.clone().unwrap_or_default(),
            user_id: self.user_id,
            auth_client: self.auth_client,
            quality: self.quality,
        };
        write_session_file(path, &data)
    }

    fn delete_session_file(&self) {
        if let Some(path) = self.session_path.as_deref() {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Internal types

struct ResolvedStream {
    quality: Option<String>,
    codec: String,
    bit_depth: Option<u32>,
    sample_rate: Option<u32>,
    /// The stream's ReplayGain in dB (track gain, else album gain), for normalization.
    replay_gain_db: Option<f64>,
    source: Source,
}


enum PlayableManifest {
    Url { url: String, codec: String },
    EncryptedUrl { url: String, codec: String, key: [u8; 16], nonce: [u8; 8] },
    Hls { playlist: String, codec: String },
}

enum ApiFailure {
    Unauthorized,
    Other(String),
}

impl ApiFailure {
    fn message(self) -> String {
        match self {
            ApiFailure::Unauthorized => "TIDAL session expired; sign in again".to_string(),
            ApiFailure::Other(message) => message,
        }
    }
}

/// A completed HTTP response: its status, `ETag` (when TIDAL sent one) and body.
struct HttpOutcome {
    status: u16,
    etag: Option<String>,
    body: String,
}

/// Adds the bearer token, user agent, query parameters and any extra headers to
/// a request. Generic over the body typestate so GET/POST/DELETE share it.
fn apply_request<B>(
    mut request: ureq::RequestBuilder<B>,
    token: &str,
    query: &[(&str, String)],
    headers: &[(&str, String)],
) -> ureq::RequestBuilder<B> {
    let authorization = format!("Bearer {token}");
    request = request
        .header("Authorization", authorization.as_str())
        .header("User-Agent", USER_AGENT);
    for (key, value) in query {
        request = request.query(*key, value.as_str());
    }
    for (key, value) in headers {
        request = request.header(*key, value.as_str());
    }
    request
}

/// Reads status, ETag and body from a response. A 401 becomes
/// [`ApiFailure::Unauthorized`] so the caller can refresh and retry; every other
/// status is returned to the caller to interpret.
fn read_outcome(mut response: ureq::http::Response<ureq::Body>) -> Result<HttpOutcome, ApiFailure> {
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| ApiFailure::Other(format!("could not read TIDAL response: {error}")))?;
    if status == 401 {
        return Err(ApiFailure::Unauthorized);
    }
    Ok(HttpOutcome { status, etag, body })
}

/// Turns a non-2xx outcome into a message.
fn expect_success(outcome: &HttpOutcome) -> Result<(), String> {
    if (200..300).contains(&outcome.status) {
        Ok(())
    } else if outcome.status == 429 {
        Err("TIDAL rate limit reached; try again in a moment".to_string())
    } else {
        Err(api_error_message(outcome.status, &outcome.body))
    }
}

/// Validates a playlist UUID (`8-4-4-4-12` hex).
fn validate_playlist_id(id: &str) -> Result<(), String> {
    if id.len() == 36 && id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-') {
        Ok(())
    } else {
        Err("Invalid playlist ID".to_string())
    }
}

enum PollError {
    Pending,
    SlowDown,
    Fatal(String),
    Transient(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceAuthorizationResponse {
    device_code: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    #[serde(default)]
    user: Option<TokenUser>,
    #[serde(default, alias = "userId")]
    user_id: Option<u64>,
}

#[derive(Deserialize)]
struct TokenUser {
    #[serde(default, rename = "userId", alias = "user_id")]
    user_id: Option<u64>,
    #[serde(default, rename = "countryCode", alias = "country_code")]
    country_code: Option<String>,
}

#[derive(Deserialize)]
struct SearchTracks {
    #[serde(default)]
    items: Vec<SearchTrack>,
}

#[derive(Deserialize)]
struct PlaylistList {
    #[serde(default)]
    items: Vec<PlaylistItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaylistItem {
    uuid: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    square_image: Option<String>,
    #[serde(default)]
    number_of_tracks: u64,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    creator: Option<PlaylistCreator>,
    #[serde(default)]
    public_playlist: Option<bool>,
}

#[derive(Deserialize)]
struct PlaylistCreator {
    #[serde(default)]
    id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchTrack {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: Option<SearchArtist>,
    #[serde(default)]
    artists: Vec<SearchArtist>,
    #[serde(default)]
    album: Option<SearchAlbum>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    track_number: Option<u32>,
    #[serde(default)]
    explicit: Option<bool>,
    #[serde(default)]
    audio_quality: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchArtist {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchAlbum {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    cover: Option<String>,
}

/// Response of `GET /users/{userId}/favorites/tracks`. Each entry wraps the
/// actual catalogue track in an `item` field alongside the date it was added.
#[derive(Deserialize)]
struct FavoriteTracksResponse {
    #[serde(default)]
    items: Vec<FavoriteTrackEntry>,
}

#[derive(Deserialize)]
struct FavoriteTrackEntry {
    #[serde(default)]
    item: Option<SearchTrack>,
}


#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackInfo {
    #[serde(default)]
    audio_quality: Option<String>,
    #[serde(default)]
    manifest_mime_type: Option<String>,
    #[serde(default)]
    manifest: String,
    #[serde(default)]
    bit_depth: Option<u32>,
    #[serde(default)]
    sample_rate: Option<u32>,
    /// Track ReplayGain in dB, when TIDAL reports it.
    #[serde(default)]
    replay_gain: Option<f64>,
    /// Album ReplayGain in dB, the fallback when the track has none.
    #[serde(default)]
    album_replay_gain: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoPlaybackInfo {
    #[serde(default)]
    manifest_mime_type: Option<String>,
    #[serde(default)]
    manifest: String,
}

/// The JSON TIDAL wraps inside a video playback manifest: the stream's real
/// MIME type and its URL list.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoManifest {
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    urls: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BtsManifest {
    #[serde(default)]
    codecs: String,
    #[serde(default)]
    encryption_type: String,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    key_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct PersistedSession {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: u64,
    #[serde(default)]
    country_code: String,
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    auth_client: AuthClient,
    #[serde(default)]
    quality: Quality,
}

fn default_expires_in() -> u64 {
    3600
}

fn default_interval() -> u64 {
    5
}

// ---------------------------------------------------------------------------------------------
// Manifest handling

/// Decodes a video playback manifest into its HLS master playlist URL.
///
/// The value is base64 JSON (`application/vnd.tidal.emu`); the wrapped
/// `mimeType` must be `application/vnd.apple.mpegurl`, and the first URL is the
/// master playlist. Anything else — notably a Widevine/PlayReady manifest — is
/// reported rather than guessed at, so the UI can show TIDAL's real reason.
fn decode_video_manifest(encoded: &str, mime: Option<&str>) -> Result<String, String> {
    let compact: String = encoded.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.is_empty() {
        return Err("TIDAL returned an empty video manifest".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&compact)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&compact))
        .map_err(|error| format!("could not decode the TIDAL video manifest: {error}"))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("the TIDAL video manifest is not UTF-8: {error}"))?;
    let compressed = serde_json::from_str::<VideoManifest>(&text)
        .map_err(|error| format!("unexpected TIDAL video manifest: {error}"))?;
    let inner_mime = compressed.mime_type.trim().to_ascii_lowercase();
    if inner_mime.contains("widevine") || inner_mime.contains("playready") {
        return Err(format!(
            "TIDAL returned a DRM-protected video manifest ({inner_mime}); a CDM would be required"
        ));
    }
    if !inner_mime.is_empty() && !inner_mime.contains("mpegurl") {
        return Err(format!(
            "unsupported TIDAL video stream manifest type: {} (outer {})",
            compressed.mime_type,
            mime.unwrap_or("unknown")
        ));
    }
    let url = compressed
        .urls
        .into_iter()
        .find(|url| !url.trim().is_empty())
        .ok_or_else(|| "TIDAL's video manifest contained no playlist URL".to_string())?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("TIDAL's video manifest contained a non-HTTP playlist URL".to_string());
    }
    Ok(url)
}

fn decode_manifest(encoded: &str, mime: Option<&str>) -> Result<PlayableManifest, String> {
    if mime.is_some_and(|mime| mime.eq_ignore_ascii_case("application/vnd.tidal.emu")) {
        return Err("TIDAL returned a DRM-protected stream requiring a licensed CDM".to_string());
    }
    let compact: String = encoded.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.is_empty() {
        return Err("TIDAL returned an empty stream manifest".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&compact)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&compact))
        .map_err(|error| format!("could not decode TIDAL stream manifest: {error}"))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("TIDAL stream manifest is not UTF-8: {error}"))?;
    let mime = mime.unwrap_or("").to_ascii_lowercase();
    let trimmed = text.trim_start();
    if mime.contains("bts") || trimmed.starts_with('{') {
        parse_bts(&text)
    } else if mime.contains("dash") || trimmed.starts_with("<?xml") || text.contains("<MPD") {
        parse_dash(&text)
    } else if mime.contains("mpegurl") || trimmed.starts_with("#EXTM3U") {
        parse_hls(&text)
    } else {
        Err(format!("unsupported TIDAL stream manifest type: {mime}"))
    }
}

fn parse_bts(text: &str) -> Result<PlayableManifest, String> {
    let manifest: BtsManifest = serde_json::from_str(text)
        .map_err(|error| format!("unexpected TIDAL BTS manifest: {error}"))?;
    let encryption = manifest.encryption_type.trim();
    let url = manifest
        .urls
        .into_iter()
        .find(|url| !url.trim().is_empty())
        .ok_or_else(|| "TIDAL BTS manifest contained no stream URL".to_string())?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("TIDAL BTS manifest contained a non-HTTP stream URL".to_string());
    }
    if encryption.eq_ignore_ascii_case("OLD_AES") {
        let token = manifest.key_id.as_deref().filter(|key| !key.is_empty())
            .ok_or_else(|| "Encrypted TIDAL stream is missing its key".to_string())?;
        let (key, nonce) = legacy_stream_key(token)?;
        return Ok(PlayableManifest::EncryptedUrl { url, codec: manifest.codecs, key, nonce });
    }
    if (!encryption.is_empty() && !encryption.eq_ignore_ascii_case("NONE"))
        || manifest.key_id.as_deref().is_some_and(|key| !key.trim().is_empty())
    {
        return Err(format!("Unsupported TIDAL stream encryption: {encryption}"));
    }
    Ok(PlayableManifest::Url {
        url,
        codec: manifest.codecs,
    })
}

// Legacy TIDAL BTS wraps a per-track AES-CTR key and nonce in AES-256-CBC.
fn legacy_stream_key(token: &str) -> Result<([u8; 16], [u8; 8]), String> {
    use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
    const MASTER: &str = "UIlTTEMmmLfGowo/UC60x2H45W6MdGgTRfo/umg4754=";
    let master = base64::engine::general_purpose::STANDARD.decode(MASTER)
        .map_err(|e| format!("Invalid TIDAL key: {e}"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(token)
        .map_err(|e| format!("Invalid TIDAL stream key: {e}"))?;
    if bytes.len() < 48 || (bytes.len() - 16) % 16 != 0 {
        return Err("Invalid TIDAL stream key length".to_string());
    }
    let mut payload = bytes[16..].to_vec();
    let decrypted = cbc::Decryptor::<aes::Aes256>::new_from_slices(&master, &bytes[..16])
        .map_err(|_| "Invalid TIDAL key parameters".to_string())?
        .decrypt_padded_mut::<NoPadding>(&mut payload)
        .map_err(|_| "Invalid encrypted TIDAL stream key".to_string())?;
    let mut key = [0; 16];
    let mut nonce = [0; 8];
    key.copy_from_slice(&decrypted[..16]);
    nonce.copy_from_slice(&decrypted[16..24]);
    Ok((key, nonce))
}

fn parse_dash(xml: &str) -> Result<PlayableManifest, String> {
    let lower = xml.to_ascii_lowercase();
    if lower.contains("<contentprotection")
        || lower.contains("default_kid")
        || lower.contains("widevine")
        || lower.contains("playready")
    {
        return Err("TIDAL returned a DRM-protected stream; refusing to bypass DRM".to_string());
    }
    let (playlist, codec) = mpd_to_hls(xml)?;
    Ok(PlayableManifest::Hls { playlist, codec })
}

fn parse_hls(text: &str) -> Result<PlayableManifest, String> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("#ext-x-key") || lower.contains("#ext-x-session-key") {
        return Err("TIDAL returned an encrypted HLS stream; refusing to bypass DRM".to_string());
    }
    let has_absolute_url = text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("http://") || line.starts_with("https://") || line.contains("URI=\"http")
    });
    if !has_absolute_url {
        return Err("unsupported HLS manifest: segment URLs are not absolute".to_string());
    }
    Ok(PlayableManifest::Hls {
        playlist: text.to_string(),
        codec: String::new(),
    })
}

/// Converts TIDAL's segmented DASH manifest into an fMP4 HLS playlist.
///
/// The initialization segment is identified by `#EXT-X-MAP`, separately
/// from the media segments.
fn mpd_to_hls(xml: &str) -> Result<(String, String), String> {
    let representation = opening_tag(xml, "Representation")
        .ok_or_else(|| "DASH manifest has no Representation element".to_string())?;
    let codec = xml_attr(representation, "codecs").unwrap_or_default();
    let representation_id = xml_attr(representation, "id").unwrap_or_default();
    let base_url = xml_element_text(xml, "BaseURL").unwrap_or_default();

    if let Some(template) = opening_tag(xml, "SegmentTemplate") {
        let media = xml_attr(template, "media")
            .ok_or_else(|| "DASH SegmentTemplate has no media template".to_string())?;
        let initialization = xml_attr(template, "initialization").unwrap_or_default();
        let timescale = xml_attr(template, "timescale")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1);
        let start_number = xml_attr(template, "startNumber")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1);

        let mut segments: Vec<(f64, String)> = Vec::new();
        if let Some(timeline) = opening_tag(xml, "SegmentTimeline") {
            let timeline_start = xml.find(timeline).unwrap_or(0) + timeline.len();
            let timeline_rest = &xml[timeline_start..];
            let timeline_end = timeline_rest
                .find("</SegmentTimeline>")
                .unwrap_or(timeline_rest.len());
            let timeline_xml = &timeline_rest[..timeline_end];
            let segment_tags = opening_tags(timeline_xml, "S");
            if segment_tags.is_empty() {
                return Err("DASH SegmentTimeline contains no segments".to_string());
            }
            let mut number = start_number;
            let mut time = 0u64;
            for segment in segment_tags {
                let duration = xml_attr(segment, "d")
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or_else(|| "DASH segment has no duration".to_string())?;
                if let Some(start) = xml_attr(segment, "t").and_then(|value| value.parse::<u64>().ok())
                {
                    time = start;
                }
                let repeat = xml_attr(segment, "r")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(0);
                if repeat < 0 {
                    return Err("DASH manifest uses an unbounded segment repeat".to_string());
                }
                let count = (repeat as u64).saturating_add(1);
                for _ in 0..count {
                    let expanded =
                        expand_segment_template(&media, number, time, &representation_id)?;
                    let url = resolve_url(&base_url, &expanded);
                    segments.push((duration as f64 / timescale as f64, url));
                    number = number.saturating_add(1);
                    time = time.saturating_add(duration);
                }
            }
        } else if let Some(segment_duration) = xml_attr(template, "duration")
            .and_then(|value| value.parse::<u64>().ok())
        {
            let total = xml_attr(xml, "mediaPresentationDuration")
                .and_then(|value| parse_iso8601_duration(&value))
                .ok_or_else(|| "DASH manifest has no mediaPresentationDuration".to_string())?;
            let seconds = segment_duration as f64 / timescale as f64;
            if !(seconds.is_finite() && seconds > 0.0) {
                return Err("DASH manifest has an invalid segment duration".to_string());
            }
            let count = (total / seconds).ceil() as u64;
            if count == 0 {
                return Err("DASH manifest has no segments".to_string());
            }
            let mut number = start_number;
            let mut time = 0u64;
            for _ in 0..count {
                let expanded = expand_segment_template(&media, number, time, &representation_id)?;
                let url = resolve_url(&base_url, &expanded);
                segments.push((seconds, url));
                number = number.saturating_add(1);
                time = time.saturating_add(segment_duration);
            }
        } else {
            return Err("DASH SegmentTemplate has no SegmentTimeline or duration".to_string());
        }

        if segments.is_empty() {
            return Err("DASH manifest produced no audio segments".to_string());
        }
        let initialization = resolve_url(&base_url, &initialization);
        if initialization.is_empty() {
            return Err("DASH manifest has no initialization segment".to_string());
        }
        if !is_http_url(initialization.as_str()) {
            return Err("unsupported DASH manifest: initialization URL is not absolute".to_string());
        }
        if segments
            .iter()
            .any(|(_, url)| !is_http_url(url.as_str()))
        {
            return Err("unsupported DASH manifest: segment URLs are not absolute".to_string());
        }
        return Ok((build_hls(&initialization, &segments), codec));
    }

    if opening_tag(xml, "SegmentList").is_some() {
        let initialization = opening_tag(xml, "Initialization")
            .and_then(|tag| xml_attr(tag, "sourceURL"))
            .unwrap_or_default();
        let mut segments = Vec::new();
        for segment in opening_tags(xml, "SegmentURL") {
            if let Some(media) = xml_attr(segment, "media") {
                segments.push((0.0, resolve_url(&base_url, &media)));
            }
        }
        if segments.is_empty() {
            return Err("DASH SegmentList contains no segments".to_string());
        }
        let initialization = resolve_url(&base_url, &initialization);
        if initialization.is_empty() {
            return Err("DASH manifest has no initialization segment".to_string());
        }
        if !is_http_url(initialization.as_str()) {
            return Err("unsupported DASH manifest: initialization URL is not absolute".to_string());
        }
        if segments
            .iter()
            .any(|(_, url)| !is_http_url(url.as_str()))
        {
            return Err("unsupported DASH manifest: segment URLs are not absolute".to_string());
        }
        return Ok((build_hls(&initialization, &segments), codec));
    }

    Err("unsupported DASH manifest: no SegmentTemplate or SegmentList".to_string())
}

fn expand_segment_template(
    template: &str,
    number: u64,
    time: u64,
    representation_id: &str,
) -> Result<String, String> {
    if template.contains("$Number%") {
        return Err("DASH formatted segment numbers are not supported".to_string());
    }
    let mut expanded = template.replace("$RepresentationID$", representation_id);
    expanded = expanded.replace("$Number$", &number.to_string());
    expanded = expanded.replace("$Time$", &time.to_string());
    if expanded.contains('$') {
        return Err("DASH manifest uses an unsupported segment template variable".to_string());
    }
    Ok(expanded)
}

fn build_hls(initialization: &str, segments: &[(f64, String)]) -> String {
    let max_duration = segments
        .iter()
        .map(|(duration, _)| *duration)
        .fold(0.0f64, f64::max);
    let target_duration = if max_duration.is_finite() && max_duration > 0.0 {
        max_duration.ceil() as u64
    } else {
        10
    };

    let mut playlist = String::new();
    playlist.push_str("#EXTM3U\n");
    playlist.push_str("#EXT-X-VERSION:7\n");
    playlist.push_str(&format!("#EXT-X-TARGETDURATION:{target_duration}\n"));
    playlist.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
    playlist.push_str(&format!("#EXT-X-MAP:URI=\"{initialization}\"\n"));
    for (duration, url) in segments {
        let duration = if duration.is_finite() && *duration > 0.0 {
            *duration
        } else {
            0.0
        };
        playlist.push_str(&format!("#EXTINF:{duration:.3},\n{url}\n"));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    playlist
}

// ---------------------------------------------------------------------------------------------
// Small XML helpers (TIDAL's MPDs are simple; a full XML parser is unnecessary)

fn opening_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("<{name}");
    let mut search = 0usize;
    while let Some(relative) = xml[search..].find(needle.as_str()) {
        let start = search + relative;
        let after = start + needle.len();
        let next = xml[after..].chars().next();
        if matches!(next, Some(ch) if ch.is_whitespace() || ch == '>' || ch == '/') {
            let rest = &xml[start..];
            let end = rest.find('>')? + 1;
            return Some(&rest[..end]);
        }
        search = after;
    }
    None
}

fn opening_tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let needle = format!("<{name}");
    let mut tags = Vec::new();
    let mut search = 0usize;
    while let Some(relative) = xml[search..].find(needle.as_str()) {
        let start = search + relative;
        let after = start + needle.len();
        let next = xml[after..].chars().next();
        if matches!(next, Some(ch) if ch.is_whitespace() || ch == '>' || ch == '/') {
            let rest = &xml[start..];
            if let Some(end) = rest.find('>') {
                tags.push(&rest[..end + 1]);
                search = start + end + 1;
                continue;
            }
        }
        search = after;
    }
    tags
}

fn xml_attr(tag: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{name}={quote}");
        let mut search = 0usize;
        while let Some(relative) = tag[search..].find(needle.as_str()) {
            let position = search + relative;
            let preceded_by_space =
                position == 0 || tag.as_bytes()[position - 1].is_ascii_whitespace();
            if preceded_by_space {
                let value_start = position + needle.len();
                if let Some(end) = tag[value_start..].find(quote) {
                    return Some(xml_unescape(&tag[value_start..value_start + end]));
                }
            }
            search = position + needle.len();
        }
    }
    None
}

fn xml_element_text(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(open.as_str())? + open.len();
    let end = xml[start..].find(close.as_str())? + start;
    Some(xml_unescape(xml[start..end].trim()))
}

fn xml_unescape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        let (replacement, consumed) = if rest.starts_with("&amp;") {
            ("&", 5)
        } else if rest.starts_with("&lt;") {
            ("<", 4)
        } else if rest.starts_with("&gt;") {
            (">", 4)
        } else if rest.starts_with("&quot;") {
            ("\"", 6)
        } else if rest.starts_with("&apos;") {
            ("'", 6)
        } else {
            ("&", 1)
        };
        output.push_str(replacement);
        rest = &rest[consumed..];
    }
    output.push_str(rest);
    output
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn resolve_url(base: &str, url: &str) -> String {
    let url = url.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    let base = base.trim();
    if base.is_empty() {
        return url.to_string();
    }
    if url.starts_with('/') {
        if let Some(scheme_end) = base.find("://") {
            let after_scheme = &base[scheme_end + 3..];
            if let Some(path_start) = after_scheme.find('/') {
                return format!("{}{}", &base[..scheme_end + 3 + path_start], url);
            }
        }
        return format!("{}{}", base.trim_end_matches('/'), url);
    }
    format!("{}/{}", base.trim_end_matches('/'), url)
}

fn parse_iso8601_duration(value: &str) -> Option<f64> {
    let value = value.trim().strip_prefix("PT")?;
    let mut total = 0.0;
    let mut number = String::new();
    for character in value.chars() {
        if character.is_ascii_digit() || character == '.' {
            number.push(character);
        } else {
            let amount: f64 = number.parse().ok()?;
            match character {
                'H' => total += amount * 3600.0,
                'M' => total += amount * 60.0,
                'S' => total += amount,
                _ => return None,
            }
            number.clear();
        }
    }
    Some(total)
}

// ---------------------------------------------------------------------------------------------
// Small utility helpers

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn normalize_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else if url.starts_with("//") {
        format!("https:{url}")
    } else {
        format!("https://{url}")
    }
}

fn basic_auth(client_id: &str, client_secret: &str) -> String {
    let raw = format!("{client_id}:{client_secret}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

fn api_error_message(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            for key in ["userMessage", "error_description", "message", "error"] {
                if let Some(message) = value.get(key).and_then(Value::as_str) {
                    if !message.is_empty() {
                        return Some(message.to_string());
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                String::new()
            } else {
                trimmed.chars().take(200).collect()
            }
        });
    if detail.is_empty() {
        format!("TIDAL request failed with HTTP {status}")
    } else {
        format!("TIDAL request failed with HTTP {status}: {detail}")
    }
}

fn extract_authorization_code(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let code = if let Some(position) = input.find("code=") {
        let rest = &input[position + "code=".len()..];
        let end = rest
            .find(|character| character == '&' || character == '#')
            .unwrap_or(rest.len());
        &rest[..end]
    } else if !input.contains("://") && !input.contains('?') && !input.contains(' ') {
        input
    } else {
        return None;
    };
    let code = percent_decode(code);
    let code = code.trim();
    if code.is_empty() {
        None
    } else {
        Some(code.to_string())
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                output.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn quality_text(
    quality: Option<&str>,
    codec: &str,
    bit_depth: Option<u32>,
    sample_rate: Option<u32>,
) -> String {
    let quality_label = quality
        .map(str::trim)
        .filter(|quality| !quality.is_empty())
        .unwrap_or("UNKNOWN");
    let mut details = Vec::new();
    let codec = codec_name(codec);
    if !codec.is_empty() {
        details.push(codec);
    }
    // TIDAL reports the decoded depth for lossy streams too (16-bit AAC), which
    // would misleadingly look like a lossless resolution. Only show resolution
    // for lossless tiers / FLAC.
    let lossless = quality_label.eq_ignore_ascii_case("LOSSLESS")
        || quality_label.eq_ignore_ascii_case("HI_RES_LOSSLESS")
        || details.iter().any(|detail| detail.as_str() == "FLAC");
    if lossless {
        match (bit_depth, sample_rate) {
            (Some(bits), Some(rate)) => details.push(format!("{bits}-bit/{}", kilohertz(rate))),
            (Some(bits), None) => details.push(format!("{bits}-bit")),
            (None, Some(rate)) => details.push(kilohertz(rate)),
            (None, None) => {}
        }
    }
    if details.is_empty() {
        quality_label.to_string()
    } else {
        format!("{quality_label} ({})", details.join(" "))
    }
}

fn quality_rank(quality: Option<&str>) -> u8 {
    match quality {
        Some(quality) if quality.eq_ignore_ascii_case("HI_RES_LOSSLESS") => 3,
        Some(quality) if quality.eq_ignore_ascii_case("LOSSLESS") => 2,
        Some(quality) if quality.eq_ignore_ascii_case("HIGH") => 1,
        _ => 0,
    }
}

/// How long to wait before each retry of a tier whose request failed transiently.
const RETRY_BACKOFF_MS: [u64; 3] = [700, 1_800, 3_500];

/// Whether a failure is worth retrying on the same tier: TIDAL's rate limit or a network
/// error. Anything else (a DRM manifest, a 4xx, a parse failure) says the tier itself is not
/// available for this track, so the next tier is tried.
fn is_transient_failure(message: &str) -> bool {
    message.contains("rate limit") || message.starts_with("TIDAL network error")
}

/// Whether requesting `requested` could still improve on the best stream found so far
/// (`best_rank`, from [`quality_rank`]).
fn worth_probing(best_rank: Option<u8>, requested: &str) -> bool {
    best_rank.map_or(true, |best| quality_rank(Some(requested)) > best)
}

/// Drops every item whose key was already seen, keeping the first of each in order. TIDAL pads
/// its favourites lists with repeated entries (and which ones repeat changes from request to
/// request), so each track, album, artist or video is kept once.
fn dedupe_by<T, K: std::hash::Hash + Eq>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    items.into_iter().filter(|item| seen.insert(key(item))).collect()
}

/// The v2 resource that serves track manifests listing the formats a track can be played in.
const OPENAPI_BASE: &str = "https://openapi.tidal.com/v2";

/// Formats to request from `trackManifests` for a quality preference, best first. `None` for
/// the AAC tiers, which the v1 endpoint serves correctly.
fn manifest_formats(preference: Quality) -> Option<&'static [&'static str]> {
    match preference {
        Quality::HiResLossless => Some(&["FLAC_HIRES", "FLAC", "AACLC"]),
        Quality::Lossless => Some(&["FLAC", "AACLC"]),
        Quality::High | Quality::Low => None,
    }
}

/// The `audioQuality` tier a manifest `formats` entry stands for.
fn manifest_quality(format: &str) -> Option<&'static str> {
    match format {
        "FLAC_HIRES" => Some("HI_RES_LOSSLESS"),
        "FLAC" => Some("LOSSLESS"),
        "AACLC" => Some("HIGH"),
        "HEAACV1" => Some("LOW"),
        _ => None,
    }
}

/// What `GET /trackManifests/{id}` says about the manifest it hands out.
struct TrackManifest {
    uri: String,
    /// The format actually served (the first of `formats`).
    format: Option<String>,
    replay_gain: Option<f64>,
}

fn parse_track_manifest(value: &Value) -> Result<TrackManifest, String> {
    let attributes = value
        .pointer("/data/attributes")
        .ok_or_else(|| "TIDAL's track manifest response has no attributes".to_string())?;
    let uri = attributes
        .get("uri")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
        .ok_or_else(|| "TIDAL's track manifest response has no manifest URL".to_string())?
        .to_owned();
    let format = attributes
        .pointer("/formats/0")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let gain = |key: &str| attributes.pointer(&format!("/{key}/replayGain")).and_then(Value::as_f64);
    Ok(TrackManifest {
        uri,
        format,
        replay_gain: gain("trackAudioNormalizationData").or_else(|| gain("albumAudioNormalizationData")),
    })
}

/// Splits a DASH representation id such as `FLAC_HIRES,44100,24` into its sample rate and
/// bit depth.
fn parse_representation_id(id: &str) -> (Option<u32>, Option<u32>) {
    let mut parts = id.split(',').skip(1).map(|part| part.trim().parse::<u32>().ok());
    (parts.next().flatten(), parts.next().flatten())
}

fn codec_name(codec: &str) -> String {
    let codec = codec.trim();
    let lower = codec.to_ascii_lowercase();
    if lower.is_empty() {
        String::new()
    } else if lower.contains("flac") {
        "FLAC".to_string()
    } else if lower.contains("mp4a") || lower.contains("aac") {
        "AAC".to_string()
    } else if lower.contains("eac3") {
        "EAC3".to_string()
    } else if lower.contains("ac4") {
        "AC4".to_string()
    } else {
        codec.to_string()
    }
}

fn kilohertz(rate: u32) -> String {
    if rate % 1000 == 0 {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f64 / 1000.0)
    }
}


/// Converts one TIDAL catalogue track object — a search hit or the `item` of a
/// favourite entry — into a [`Track`]. Returns `None` only when TIDAL omits the
/// track id, which makes the track unplayable. Artist falls back from the
/// `artists` array to the singular `artist` field; a blank album cover is
/// normalised to `None`.
fn parse_track(item: SearchTrack) -> Option<Track> {
    let id = item.id?;
    let mut artist = String::new();
    let mut artist_id = None;
    for candidate in &item.artists {
        if !candidate.name.trim().is_empty() {
            artist = candidate.name.clone();
            artist_id = candidate.id;
            break;
        }
    }
    if artist.is_empty() {
        if let Some(candidate) = &item.artist {
            if !candidate.name.trim().is_empty() {
                artist = candidate.name.clone();
            }
        }
    }
    if artist_id.is_none() {
        artist_id = item
            .artist
            .as_ref()
            .and_then(|candidate| candidate.id)
            .or_else(|| item.artists.iter().find_map(|candidate| candidate.id));
    }
    let (album, cover, album_id) = match item.album {
        Some(album) => (
            album.title,
            album.cover.filter(|cover| !cover.trim().is_empty()),
            album.id,
        ),
        None => (String::new(), None, None),
    };
    Some(Track {
        id,
        title: item.title,
        artist,
        album,
        cover,
        duration: item.duration,
        album_id,
        artist_id,
        explicit: item.explicit.unwrap_or(false),
        quality: item
            .audio_quality
            .map(|quality| quality.trim().to_string())
            .filter(|quality| !quality.is_empty()),
        track_number: item.track_number,
    })
}

/// Reads a track object out of any JSON value, tolerating missing optional
/// fields. Returns `None` when it cannot be parsed or carries no id.
fn track_from_value(value: &Value) -> Option<Track> {
    serde_json::from_value::<SearchTrack>(value.clone())
        .ok()
        .and_then(parse_track)
}

/// Reads a video object out of any JSON value, tolerating missing optional
/// fields. Returns `None` when it carries no id. The thumbnail UUID is
/// `imageId` on v1 objects; the singular `artist` is preferred over
/// `artists`, matching the search-result shape.
fn parse_video_json(value: &Value) -> Option<Video> {
    let id = value.get("id").and_then(Value::as_u64)?;
    let artist_value = value
        .get("artist")
        .or_else(|| value.get("artists").and_then(Value::as_array).and_then(|items| items.first()));
    let artist = artist_value
        .and_then(|artist| artist.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let artist_id = artist_value.and_then(|artist| artist.get("id")).and_then(Value::as_u64);
    let image = ["imageId", "image"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .map(str::to_owned);
    Some(Video {
        id,
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned(),
        artist,
        artist_id,
        image,
        duration: value.get("duration").and_then(Value::as_u64),
        explicit: value.get("explicit").and_then(Value::as_bool).unwrap_or(false),
        quality: value
            .get("quality")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|quality| !quality.is_empty())
            .map(str::to_owned),
        stream_ready: value.get("streamReady").and_then(Value::as_bool).unwrap_or(true),
    })
}

/// The `{items:[…]}` array of a v1 collection response, or an empty slice.
fn value_items(value: &Value) -> Vec<&Value> {
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

/// A collection entry that wraps its payload in an `item` field: returns the
/// inner value, otherwise the entry itself.
fn wrapped_item(entry: &Value) -> &Value {
    match entry.get("item") {
        Some(item) if item.is_object() => item,
        _ => entry,
    }
}

/// The trimmed string at `key`, when non-empty.
fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The trimmed, non-empty form of an already-extracted optional string, so a blank
/// description from TIDAL reads as "no description" rather than an empty line.
fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// Parses `/tracks/{id}/credits`: an array of `{type, contributors:[{name}]}` entries, one per
/// role. Entries with no role or no named contributors are skipped.
fn parse_credits(value: &Value) -> Vec<Credit> {
    let Some(entries) = value.as_array() else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| {
            let role = entry.get("type").and_then(Value::as_str)?.trim();
            if role.is_empty() {
                return None;
            }
            let names: Vec<String> = entry
                .get("contributors")
                .and_then(Value::as_array)
                .map(|contributors| {
                    contributors
                        .iter()
                        .filter_map(|contributor| contributor.get("name").and_then(Value::as_str))
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                return None;
            }
            Some(Credit { role: role.to_owned(), names })
        })
        .collect()
}

/// Parses `/albums/{id}/review`: `{text, source, lastUpdated}`. Returns `None`
/// when the object carries no usable text.
fn parse_album_review(value: &Value) -> Option<AlbumReview> {
    let raw = value.get("text").and_then(Value::as_str)?;
    let text = strip_review_markup(raw);
    if text.is_empty() {
        return None;
    }
    Some(AlbumReview { text, source: non_empty_string(value, "source") })
}

/// Flattens TIDAL's review markup to plain prose: `[wimpLink id="…"]…[/wimpLink]`
/// becomes its inner text, `<br>`/`<br/>`/`<p>` become line breaks, and the
/// handful of HTML entities TIDAL emits are unescaped.
fn strip_review_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        // `[wimpLink …]` drops its tag, anything else bracketed is kept verbatim.
        if let Some(close) = rest.find(']') {
            let tag = &rest[..=close];
            if tag.starts_with("[wimpLink") {
                rest = &rest[close + 1..];
                continue;
            }
        }
        out.push('[');
        rest = &rest[1..];
    }
    out.push_str(rest);
    let out = out.replace("[/wimpLink]", "");
    let mut flat = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(open) = rest.find('<') {
        flat.push_str(&rest[..open]);
        match rest[open..].find('>') {
            Some(close) => {
                let tag = &rest[open + 1..open + close];
                let name = tag.trim_start_matches('/').trim_end_matches('/').trim();
                if matches!(name, "br" | "p") {
                    flat.push('\n');
                }
                rest = &rest[open + close + 1..];
            }
            None => {
                flat.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    flat.push_str(rest);
    let decoded = flat
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // Collapse the runs of blank lines the tag removal can leave behind.
    let mut lines: Vec<&str> = Vec::new();
    for line in decoded.split('\n') {
        let line = line.trim();
        if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_owned()
}

/// The four-digit year inside a TIDAL `releaseDate`/`year`, when present.
fn year_text(value: &Value) -> Option<String> {
    let raw = value
        .get("year")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("releaseDate")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| value.get("year").and_then(Value::as_u64).map(|year| year.to_string()))?;
    let year = raw.trim();
    if year.len() >= 4 && year.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
        Some(year[..4].to_string())
    } else if !year.is_empty() {
        Some(year.to_string())
    } else {
        None
    }
}

/// Parses one album object (from `/albums`, a page card or a favourite entry).
fn parse_album_json(value: &Value) -> Option<Album> {
    let data = match value.get("data") {
        Some(data) if data.get("id").is_some() => data,
        _ => value,
    };
    let id = data.get("id").and_then(Value::as_u64)?;
    let artist = data
        .get("artists")
        .and_then(Value::as_array)
        .and_then(|artists| artists.first())
        .or_else(|| data.get("artist"));
    Some(Album {
        id,
        title: data.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        artist: artist
            .and_then(|artist| artist.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        artist_id: artist.and_then(|artist| artist.get("id")).and_then(Value::as_u64),
        cover: non_empty_string(data, "cover").or_else(|| non_empty_string(data, "image")),
        year: year_text(data),
        tracks: data
            .get("numberOfTracks")
            .and_then(Value::as_u64)
            .map(|count| count as u32),
        duration: data.get("duration").and_then(Value::as_u64),
        explicit: data.get("explicit").and_then(Value::as_bool).unwrap_or(false),
        quality: non_empty_string(data, "audioQuality"),
    })
}

/// Parses one artist object.
fn parse_artist_json(value: &Value) -> Option<Artist> {
    let data = match value.get("data") {
        Some(data) if data.get("id").is_some() => data,
        _ => value,
    };
    let id = data.get("id").and_then(Value::as_u64)?;
    Some(Artist {
        id,
        name: data.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        picture: non_empty_string(data, "picture")
            .or_else(|| non_empty_string(data, "selectedAlbumCoverFallback")),
    })
}

/// Parses one playlist object (owned or favourited).
///
/// `user_id` is the signed-in user's id, used to mark the playlist as [`Playlist::owned`];
/// pass `None` where the caller has no session (search results, page sections), and the
/// playlist page's own metadata request will settle ownership when it opens.
fn parse_playlist_json(value: &Value, user_id: Option<u64>) -> Option<Playlist> {
    let data = match value.get("data") {
        Some(data) if data.get("uuid").is_some() => data,
        _ => value,
    };
    let id = data
        .get("uuid")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            data.get("uuid")
                .and_then(Value::as_u64)
                .map(|uuid| uuid.to_string())
        })?;
    let creator = data
        .get("creator")
        .and_then(|creator| creator.get("id"))
        .and_then(Value::as_u64)
        .or_else(|| data.get("creatorId").and_then(Value::as_u64));
    let owned = match (creator, user_id) {
        (Some(creator), Some(user_id)) => creator == user_id,
        // No user id: without a session to compare against, assume not editable.
        (_, None) => false,
        // No creator reported: TIDAL always reports it for real playlists, so a missing one
        // means the payload is trimmed; treat it as not editable rather than risk a failed edit.
        (None, Some(_)) => false,
    };
    Some(Playlist {
        id,
        title: data.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        cover: non_empty_string(data, "squareImage").or_else(|| non_empty_string(data, "image")),
        count: data
            .get("numberOfTracks")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        description: non_empty_string(data, "description"),
        creator,
        public: data.get("publicPlaylist").and_then(Value::as_bool),
        owned,
    })
}

/// Parses the combined `/search` document into [`SearchAll`].
fn parse_search_all(value: &Value) -> SearchAll {
    let tracks = value
        .get("tracks")
        .map(|tracks| {
            value_items(tracks)
                .iter()
                .filter_map(|item| track_from_value(wrapped_item(item)))
                .collect()
        })
        .unwrap_or_default();
    let albums = value
        .get("albums")
        .map(|albums| {
            value_items(albums)
                .iter()
                .filter_map(|item| parse_album_json(wrapped_item(item)))
                .collect()
        })
        .unwrap_or_default();
    let artists = value
        .get("artists")
        .map(|artists| {
            value_items(artists)
                .iter()
                .filter_map(|item| parse_artist_json(wrapped_item(item)))
                .collect()
        })
        .unwrap_or_default();
    let playlists = value
        .get("playlists")
        .map(|playlists| {
            value_items(playlists)
                .iter()
                .filter_map(|item| parse_playlist_json(wrapped_item(item), None))
                .collect()
        })
        .unwrap_or_default();
    let videos = value
        .get("videos")
        .map(|videos| {
            value_items(videos)
                .iter()
                .filter_map(|item| parse_video_json(wrapped_item(item)))
                .collect()
        })
        .unwrap_or_default();
    let top = parse_top_hit(value.get("topHit"));
    SearchAll { tracks, albums, artists, playlists, videos, top }
}

/// Parses TIDAL's `topHit` (`{type, value}`), tolerating a wrapped value.
fn parse_top_hit(value: Option<&Value>) -> Option<TopHit> {
    let hit = value?;
    let kind = hit
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_uppercase();
    let inner = wrapped_item(hit.get("value").unwrap_or(hit));
    match kind.as_str() {
        "TRACKS" | "TRACK" => track_from_value(inner).map(TopHit::Track),
        "ALBUMS" | "ALBUM" => parse_album_json(inner).map(TopHit::Album),
        "ARTISTS" | "ARTIST" => parse_artist_json(inner).map(TopHit::Artist),
        "PLAYLISTS" | "PLAYLIST" => parse_playlist_json(inner, None).map(TopHit::Playlist),
        _ => None,
    }
}

/// Names from a v2 suggestions response: a bare array, or `{suggestions:[…]}` of
/// strings or `{name}`/`{title}` objects.
fn parse_suggestions(value: &Value) -> Vec<String> {
    let list = if let Some(array) = value.as_array() {
        array.clone()
    } else if let Some(array) = value.get("suggestions").and_then(Value::as_array) {
        array.clone()
    } else if let Some(array) = value.get("items").and_then(Value::as_array) {
        array.clone()
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    for entry in list {
        let text = entry
            .as_str()
            .map(str::to_owned)
            .or_else(|| non_empty_string(&entry, "name"))
            .or_else(|| non_empty_string(&entry, "title"))
            .or_else(|| non_empty_string(&entry, "query"));
        if let Some(text) = text {
            if !out.contains(&text) {
                out.push(text);
            }
        }
        if out.len() >= 10 {
            break;
        }
    }
    out
}

/// Fallback suggestions from a [`SearchAll`]: the top hit, then the leading
/// results of each kind, de-duplicated.
fn suggestion_names(all: &SearchAll) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |text: &str| {
        let text = text.trim();
        if !text.is_empty() && !out.iter().any(|existing| existing == text) {
            out.push(text.to_string());
        }
    };
    match &all.top {
        Some(TopHit::Track(track)) => push(&track.title),
        Some(TopHit::Album(album)) => push(&album.title),
        Some(TopHit::Artist(artist)) => push(&artist.name),
        Some(TopHit::Playlist(playlist)) => push(&playlist.title),
        None => {}
    }
    for track in &all.tracks {
        push(&track.title);
    }
    for album in &all.albums {
        push(&album.title);
    }
    for artist in &all.artists {
        push(&artist.name);
    }
    for playlist in &all.playlists {
        push(&playlist.title);
    }
    out.truncate(10);
    out
}

/// A `HashSet<u64>` from `key:[…]` numbers (or numeric strings).
fn numeric_id_set(value: &Value, key: &str) -> HashSet<u64> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_u64().or_else(|| item.as_str().and_then(|text| text.parse().ok()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `HashSet<String>` from `key:[…]` strings (or numbers).
fn string_id_set(value: &Value, key: &str) -> HashSet<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .or_else(|| item.as_u64().map(|id| id.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parses a v1 page document (`{rows:[{modules:[…]}]}`) into titled card
/// sections. Recognises album/playlist/artist/track/mix list modules; other
/// module types are skipped.
fn parse_sections(page: &Value) -> Vec<Section> {
    let mut sections = Vec::new();
    let Some(rows) = page.get("rows").and_then(Value::as_array) else {
        return sections;
    };
    for row in rows {
        let Some(modules) = row.get("modules").and_then(Value::as_array) else {
            continue;
        };
        for module in modules {
            let kind = module.get("type").and_then(Value::as_str).unwrap_or("");
            let Some(items) = module.pointer("/pagedList/items").and_then(Value::as_array) else {
                continue;
            };
            let mut cards: Vec<Card> = Vec::new();
            for item in items {
                let inner = wrapped_item(item);
                let card = match kind {
                    "ALBUM_LIST" => parse_album_json(inner).map(Card::Album),
                    "PLAYLIST_LIST" => parse_playlist_json(inner, None).map(Card::Playlist),
                    "ARTIST_LIST" => parse_artist_json(inner).map(Card::Artist),
                    "TRACK_LIST" => track_from_value(inner).map(Card::Track),
                    "MIX_LIST" => parse_mix_json(inner).map(Card::Mix),
                    _ => None,
                };
                if let Some(card) = card {
                    cards.push(card);
                }
            }
            if cards.is_empty() {
                continue;
            }
            let title = module
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or("")
                .to_string();
            sections.push(Section { title, items: cards });
        }
    }
    sections
}

/// Parses one mix object (`{id,title,subTitle,mixType,images}`).
fn parse_mix_json(value: &Value) -> Option<Mix> {
    let data = match value.get("data") {
        Some(data) if data.get("id").is_some() => data,
        _ => value,
    };
    let id = data.get("id").and_then(Value::as_str)?;
    let title = data.get("title").and_then(Value::as_str)?;
    let subtitle = data
        .get("subTitle")
        .or_else(|| data.get("subtitle"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let image = non_empty_string(data, "image")
        .or_else(|| {
            data.pointer("/images/SMALL/url")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Some(Mix { id: id.to_string(), title: title.to_string(), subtitle, image })
}

/// Parses a lyrics document into [`LyricsData`]. Returns `None` when TIDAL sent
/// neither text nor synced lines.
fn parse_lyrics(value: &Value) -> Option<LyricsData> {
    let lyrics = non_empty_string(value, "lyrics").unwrap_or_default();
    let subtitles = non_empty_string(value, "subtitles").unwrap_or_default();
    let synced = parse_lrc(&subtitles);
    let text = if !lyrics.is_empty() {
        lyrics
    } else {
        synced
            .iter()
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    if text.is_empty() && synced.is_empty() {
        return None;
    }
    let provider = non_empty_string(value, "provider")
        .or_else(|| non_empty_string(value, "lyricsProvider"));
    Some(LyricsData { text, synced, provider })
}

/// Parses an LRC document into `(milliseconds, text)` lines, sorted by time.
/// Handles `[mm:ss.xx]`, `[mm:ss.xxx]` and `[mm:ss:xx]` timestamps, multiple
/// timestamps on one line, and skips metadata (`[ar:…]`) and empty lines.
fn parse_lrc(text: &str) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        if rest.is_empty() {
            continue;
        }
        let mut stamps: Vec<u64> = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let Some(millis) = parse_lrc_timestamp(&rest[1..end]) else { break };
            stamps.push(millis);
            rest = rest[end + 1..].trim_start();
        }
        if stamps.is_empty() {
            continue;
        }
        let content = rest.trim().to_string();
        if content.is_empty() {
            continue;
        }
        for millis in stamps {
            out.push((millis, content.clone()));
        }
    }
    out.sort_by_key(|(millis, _)| *millis);
    out
}

/// Parses one LRC timestamp body (`mm:ss.xx`, `mm:ss.xxx`, `mm:ss:xx`,
/// `mm:ss`) into whole milliseconds.
fn parse_lrc_timestamp(body: &str) -> Option<u64> {
    let body = body.trim();
    let (minutes, rest) = body.split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.find(['.', ':']) {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, ""),
    };
    let seconds: u64 = seconds.trim().parse().ok()?;
    let fraction = fraction.trim();
    let millis = if fraction.is_empty() {
        0
    } else {
        // LRC uses centiseconds (2 digits) or milliseconds (3 digits).
        let digits = fraction.chars().take(3).collect::<String>();
        let value: u64 = digits.parse().ok()?;
        match digits.len() {
            1 => value * 100,
            2 => value * 10,
            _ => value,
        }
    };
    Some(minutes * 60_000 + seconds * 1000 + millis)
}

/// Reads a v2 listener-profile document (`GET /profiles/me`) into [`Profile`].
///
/// `name` prefers the profile `name`, then its `handle`. `picture` is the raw
/// image UUID at `picture.url` (a bare hyphenated UUID, not a URL), absent when
/// the account has no avatar.
fn parse_profile(value: &Value) -> Profile {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let picture = value
        .get("picture")
        .and_then(|picture| picture.get("url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned);
    Profile { name: text("name").or_else(|| text("handle")), picture }
}

fn parse_mixes(page: &Value) -> Vec<Mix> {
    let mut mixes = Vec::new();
    let Some(rows) = page.get("rows").and_then(Value::as_array) else { return mixes };
    for row in rows {
        let Some(modules) = row.get("modules").and_then(Value::as_array) else { continue };
        for module in modules {
            if module.get("type").and_then(Value::as_str) != Some("MIX_LIST") { continue; }
            let Some(items) = module.pointer("/pagedList/items").and_then(Value::as_array) else { continue };
            for item in items {
                let kind = item.get("mixType").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "DAILY_MIX" | "DISCOVERY_MIX" | "NEW_RELEASE_MIX") { continue; }
                let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
                let Some(title) = item.get("title").and_then(Value::as_str) else { continue };
                let subtitle = item.get("subTitle").and_then(Value::as_str).unwrap_or("");
                let image = item.pointer("/images/SMALL/url").and_then(Value::as_str).map(str::to_owned);
                mixes.push(Mix { id: id.to_owned(), title: title.to_owned(), subtitle: subtitle.to_owned(), image });
            }
        }
    }
    mixes
}

fn parse_home_sections(page: &Value) -> Vec<HomeSection> {
    let mut sections = Vec::new();
    let Some(rows) = page.get("rows").and_then(Value::as_array) else { return sections };
    for row in rows {
        let Some(modules) = row.get("modules").and_then(Value::as_array) else { continue };
        for module in modules {
            if module.get("type").and_then(Value::as_str) != Some("TRACK_LIST") { continue; }
            let Some(items) = module.pointer("/pagedList/items").and_then(Value::as_array) else { continue };
            let tracks: Vec<Track> = items.iter().filter_map(|item|
                serde_json::from_value::<SearchTrack>(item.clone()).ok().and_then(parse_track)
            ).take(9).collect();
            if tracks.is_empty() { continue; }
            let title = module.get("title").and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty()).unwrap_or("Recommended tracks");
            sections.push(HomeSection { title: title.to_owned(), tracks });
            if sections.len() == 2 { return sections; }
        }
    }
    sections
}

// ---------------------------------------------------------------------------------------------
// Persistence

fn session_file_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
        })
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(base.join("tsun").join("session.json"))
}

fn write_session_file(path: &Path, data: &PersistedSession) -> Result<(), String> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)
            .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
        }
    }

    let json = serde_json::to_vec_pretty(data)
        .map_err(|error| format!("could not serialize TIDAL session: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    let _ = fs::remove_file(&temporary);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("could not create {}: {error}", temporary.display()))?;
    if let Err(error) = file.write_all(&json) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("could not write TIDAL session: {error}"));
    }
    if let Err(error) = file.sync_all() {
        let _ = fs::remove_file(&temporary);
        return Err(format!("could not flush TIDAL session: {error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    }
    drop(file);

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("could not replace TIDAL session file: {error}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live proof of playlist reordering, on a throwaway playlist that is deleted again.
    ///
    /// Ignored by default: it needs the network and a signed-in session, and it touches the
    /// account (it creates one scratch playlist and deletes it). Run it with
    /// `cargo test -- --ignored live_move_playlist_item`.
    #[test]
    #[ignore]
    fn live_move_playlist_item_reorders_a_scratch_playlist() {
        let mut client = Client::new().expect("a client");
        if !client.is_logged_in() {
            eprintln!("skipping: no TIDAL session");
            return;
        }
        let playlist = client
            .create_playlist("tsun reorder check (safe to delete)", "scratch")
            .expect("create the scratch playlist");
        let ids = [1_550_546u64, 161_017, 20_115_561];
        let mut outcome: Result<(), String> = Ok(());
        let order = |client: &mut Client| -> Result<Vec<u64>, String> {
            Ok(client
                .playlist_tracks(&playlist.id)?
                .iter()
                .map(|track| track.id)
                .collect())
        };
        if let Err(error) = client.add_to_playlist(&playlist.id, &ids) {
            outcome = Err(format!("add: {error}"));
        }
        if outcome.is_ok() {
            match order(&mut client) {
                Ok(before) if before == ids => {}
                Ok(before) => outcome = Err(format!("tracks landed in the wrong order: {before:?}")),
                Err(error) => outcome = Err(format!("read before: {error}")),
            }
        }
        if outcome.is_ok() {
            if let Err(error) = client.move_playlist_item(&playlist.id, 0, 2) {
                outcome = Err(format!("move: {error}"));
            }
        }
        if outcome.is_ok() {
            match order(&mut client) {
                Ok(after) if after == [ids[1], ids[2], ids[0]] => {}
                Ok(after) => outcome = Err(format!("move did not reorder: {after:?}")),
                Err(error) => outcome = Err(format!("read after: {error}")),
            }
        }
        if outcome.is_ok() {
            if let Err(error) = client.move_playlist_item(&playlist.id, 2, 0) {
                outcome = Err(format!("move back: {error}"));
            }
        }
        if outcome.is_ok() {
            match order(&mut client) {
                Ok(restored) if restored == ids => {}
                Ok(restored) => outcome = Err(format!("move back did not restore: {restored:?}")),
                Err(error) => outcome = Err(format!("read restored: {error}")),
            }
        }
        // The scratch playlist is removed whatever happened, so the account is left as found.
        let _ = client.delete_playlist(&playlist.id);
        outcome.expect("the live reorder round trip");
    }

    #[test]
    fn album_review_flattens_wimp_links_and_credits_the_source() {
        let review = parse_album_review(&serde_json::json!({
            "source": "TiVo",
            "text": "After [wimpLink albumId=\"161010\"]Homework[/wimpLink], [wimpLink artistId=\"8847\"]Daft Punk[/wimpLink] returned.<br/>Second line. ~ Sean Cooper"
        }))
        .expect("a review");
        assert_eq!(review.source.as_deref(), Some("TiVo"));
        assert_eq!(review.text, "After Homework, Daft Punk returned.\nSecond line. ~ Sean Cooper");
    }

    #[test]
    fn album_review_without_text_is_none_and_plain_text_is_kept() {
        assert!(parse_album_review(&serde_json::json!({"text": "   "})).is_none());
        assert!(parse_album_review(&serde_json::json!({"source": "TIDAL"})).is_none());
        let plain = parse_album_review(&serde_json::json!({"text": "Simple &amp; short."})).unwrap();
        assert_eq!(plain.text, "Simple & short.");
        assert_eq!(plain.source, None);
        // A stray bracket that is not a tag survives intact.
        let stray = parse_album_review(&serde_json::json!({"text": "See [1] and [2]"})).unwrap();
        assert_eq!(stray.text, "See [1] and [2]");
    }

    #[test]
    fn credits_parse_for_albums_and_tracks() {
        let value = serde_json::json!([
            {"type": "Producer", "contributors": [{"name": "Daft Punk", "id": 8847}, {"name": "  "}]},
            {"type": "Bass guitar", "contributors": [{"name": "Nathan East"}]},
            {"type": "", "contributors": [{"name": "Nobody"}]},
            {"type": "Empty", "contributors": []}
        ]);
        assert_eq!(
            parse_credits(&value),
            vec![
                Credit { role: "Producer".into(), names: vec!["Daft Punk".into()] },
                Credit { role: "Bass guitar".into(), names: vec!["Nathan East".into()] },
            ]
        );
    }

    #[test]
    fn profile_uses_listener_avatar_and_handles_missing_picture() {
        let profile = parse_profile(&serde_json::json!({
            "name": "Listener",
            "handle": "listener",
            "picture": {"url": "01234567-89ab-cdef-0123-456789abcdef"}
        }));
        assert_eq!(profile.name.as_deref(), Some("Listener"));
        assert_eq!(profile.picture.as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
        let without_picture = parse_profile(&serde_json::json!({"name": "", "handle": "listener", "picture": null}));
        assert_eq!(without_picture.name.as_deref(), Some("listener"));
        assert_eq!(without_picture.picture, None);
    }

    #[test]
    fn home_feed_preserves_playable_tracks_and_artwork() {
        let page = serde_json::json!({"rows": [
            {"modules": [{"type": "PLAYLIST_LIST", "pagedList": {"items": [{"id": 9}]}}]},
            {"modules": [{"type": "TRACK_LIST", "title": "For you", "pagedList": {"items": [
                {"id": 42, "title": "Song", "artists": [{"name": "Artist"}],
                 "duration": 185, "album": {"title": "Album", "cover": "a-b"}},
                {"title": "Unavailable"}
            ]}}]}
        ]});
        let sections = parse_home_sections(&page);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title, "For you");
        assert_eq!(sections[0].tracks, vec![Track {
            id: 42, title: "Song".into(), artist: "Artist".into(), album: "Album".into(),
            cover: Some("a-b".into()), duration: Some(185),
            album_id: None, artist_id: None, explicit: false, quality: None, track_number: None,
        }]);
    }

    #[test]
    fn personalized_mixes_keep_artwork_and_exclude_unrelated_modules() {
        let page = serde_json::json!({"rows": [{"modules": [
            {"type": "TRACK_LIST", "pagedList": {"items": [
                {"id": "wrong-module", "title": "Not a mix", "mixType": "DAILY_MIX"}
            ]}},
            {"type": "MIX_LIST", "pagedList": {"items": [
                {"id": "discovery", "title": "My Daily Discovery", "subTitle": "Artists for you",
                 "mixType": "DISCOVERY_MIX", "images": {"SMALL": {"url": "https://images.tidal.com/mix.jpg"}}},
                {"id": "other", "title": "Other mix", "mixType": "OTHER"},
                {"id": "daily", "title": "My Mix 1", "mixType": "DAILY_MIX"},
                {"title": "Missing id", "mixType": "DAILY_MIX"}
            ]}}
        ]}]});
        let mixes = parse_mixes(&page);
        assert_eq!(mixes.len(), 2);
        assert_eq!((&*mixes[0].id, &*mixes[0].title, &*mixes[0].subtitle),
                   ("discovery", "My Daily Discovery", "Artists for you"));
        assert_eq!(mixes[0].image.as_deref(), Some("https://images.tidal.com/mix.jpg"));
        assert_eq!(&mixes[1].id, "daily");
        assert_eq!(mixes[1].image, None);
    }

    #[test]
    fn dash_flac_initialization_and_timeline_become_playable_hls() {
        let mpd = r#"<MPD><Period><AdaptationSet>
            <Representation id="0" codecs="flac">
              <SegmentTemplate timescale="1000" initialization="https://cdn.example/init.mp4"
                media="https://cdn.example/$Number$.mp4" startNumber="1">
                <SegmentTimeline><S d="5000" r="1"/><S d="2500"/></SegmentTimeline>
              </SegmentTemplate>
            </Representation>
        </AdaptationSet></Period></MPD>"#;
        let (playlist, codec) = mpd_to_hls(mpd).unwrap();
        assert_eq!(codec, "flac");
        assert!(playlist.contains("#EXT-X-MAP:URI=\"https://cdn.example/init.mp4\""));
        assert!(playlist.contains("#EXTINF:5.000,\nhttps://cdn.example/1.mp4"));
        assert!(playlist.contains("#EXTINF:5.000,\nhttps://cdn.example/2.mp4"));
        assert!(playlist.contains("#EXTINF:2.500,\nhttps://cdn.example/3.mp4"));
    }

    #[test]
    fn legacy_bts_manifest_unwraps_stream_key() {
        let manifest = r#"{"codecs":"flac","encryptionType":"OLD_AES","keyId":"AAECAwQFBgcICQoLDA0ODwDArlH3qgOnIOkznG4ENzMDej4itrHpn1TEpUizlY6j","urls":["https://cdn.example/audio"]}"#;
        match parse_bts(manifest).unwrap() {
            PlayableManifest::EncryptedUrl { key, nonce, .. } => {
                assert_eq!(key, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
                assert_eq!(nonce, [16, 17, 18, 19, 20, 21, 22, 23]);
            }
            _ => panic!("legacy encrypted stream was not recognized"),
        }
    }

    #[test]
    fn malformed_legacy_key_is_rejected() {
        let manifest = r#"{"encryptionType":"OLD_AES","keyId":"AAAA","urls":["https://cdn.example/audio"]}"#;
        assert!(parse_bts(manifest).is_err());
    }

    #[test]
    fn widevine_manifest_reports_missing_drm_support() {
        let mpd = "<MPD><ContentProtection schemeIdUri=\"widevine\"/></MPD>";
        assert!(parse_dash(mpd).err().unwrap().contains("DRM"));
    }

    #[test]
    fn emu_manifest_reports_license_requirement() {
        assert!(decode_manifest("", Some("application/vnd.tidal.emu"))
            .err().unwrap().contains("licensed CDM"));
    }

    #[test]
    fn video_manifest_unwraps_the_plain_hls_master_playlist() {
        let runtime = serde_json::json!({
            "mimeType": "application/vnd.apple.mpegurl",
            "urls": ["https://im-fa.manifest.tidal.com/1/manifests/abc.m3u8?token=secret"]
        });
        let encoded = base64::engine::general_purpose::STANDARD.encode(runtime.to_string());
        assert_eq!(
            decode_video_manifest(&encoded, Some("application/vnd.tidal.emu")).unwrap(),
            "https://im-fa.manifest.tidal.com/1/manifests/abc.m3u8?token=secret"
        );
    }

    #[test]
    fn video_manifest_reports_drm_instead_of_guessing() {
        let runtime = serde_json::json!({
            "mimeType": "application/vnd.widevine",
            "urls": ["https://cdn.example/encrypted"]
        });
        let encoded = base64::engine::general_purpose::STANDARD.encode(runtime.to_string());
        let error = decode_video_manifest(&encoded, Some("application/vnd.tidal.emu")).unwrap_err();
        assert!(error.contains("DRM"), "{error}");
    }

    #[test]
    fn video_parser_reads_the_catalogue_shape() {
        let value = serde_json::json!({
            "id": 44187439, "title": "Around the World",
            "artists": [{"id": 8847, "name": "Daft Punk"}],
            "imageId": "30bfadb8-e012-4baa-a712-1a62ab404f7d",
            "duration": 242, "quality": "MP4_1080P", "streamReady": true, "explicit": false
        });
        let video = parse_video_json(&value).unwrap();
        assert_eq!(video.id, 44187439);
        assert_eq!(video.artist, "Daft Punk");
        assert_eq!(video.artist_id, Some(8847));
        assert_eq!(video.duration, Some(242));
        assert_eq!(video.quality.as_deref(), Some("MP4_1080P"));
        assert_eq!(video.image.as_deref(), Some("30bfadb8-e012-4baa-a712-1a62ab404f7d"));
        assert!(video.stream_ready);
    }

    #[test]
    fn favourite_ids_include_videos() {
        let value = serde_json::json!({
            "TRACK": [1, 2], "ALBUM": [3], "ARTIST": [4],
            "PLAYLIST": ["11111111-1111-1111-1111-111111111111"], "VIDEO": [44187439]
        });
        assert!(numeric_id_set(&value, "VIDEO").contains(&44187439));
    }

    #[test]
    fn track_parser_fills_extended_metadata() {
        let value = serde_json::json!({
            "id": 5, "title": "Song", "artist": {"id": 9, "name": "Artist"},
            "album": {"id": 3, "title": "Album", "cover": "cover-uuid"},
            "duration": 100, "trackNumber": 4, "explicit": true, "audioQuality": "HI_RES_LOSSLESS"
        });
        let track = track_from_value(&value).unwrap();
        assert_eq!(track.id, 5);
        assert_eq!(track.album_id, Some(3));
        assert_eq!(track.artist_id, Some(9));
        assert!(track.explicit);
        assert_eq!(track.quality.as_deref(), Some("HI_RES_LOSSLESS"));
        assert_eq!(track.track_number, Some(4));
    }

    #[test]
    fn track_parser_prefers_leading_non_empty_artist_and_blank_cover_is_none() {
        let value = serde_json::json!({
            "id": 1, "title": "T",
            "artists": [{"id": 0, "name": ""}, {"id": 11, "name": "Real"}],
            "album": {"title": "A", "cover": "  "}
        });
        let track = track_from_value(&value).unwrap();
        assert_eq!(track.artist, "Real");
        assert_eq!(track.artist_id, Some(11));
        assert_eq!(track.cover, None);
        assert!(!track.explicit);
        assert_eq!(track.quality, None);
    }

    #[test]
    fn search_all_parses_all_kinds_and_top_hit() {
        let doc = serde_json::json!({
            "tracks": {"items": [
                {"id": 1, "title": "T", "artists": [{"id": 7, "name": "A"}],
                 "audioQuality": "LOSSLESS", "explicit": false}
            ]},
            "albums": {"items": [
                {"id": 2, "title": "Al", "artists": [{"id": 7, "name": "A"}],
                 "numberOfTracks": 10, "releaseDate": "2019-05-01", "duration": 2400}
            ]},
            "artists": {"items": [{"id": 7, "name": "A", "picture": "abcdefab-1234-5678-9abc-def012345678"}]},
            "playlists": {"items": [
                {"uuid": "11111111-1111-1111-1111-111111111111", "title": "P", "numberOfTracks": 3}
            ]},
            "videos": {"items": [
                {"id": 9, "title": "Video", "artists": [{"id": 7, "name": "A"}],
                 "imageId": "abcdefab-1234-5678-9abc-def012345678", "duration": 242,
                 "quality": "MP4_1080P", "streamReady": true, "explicit": true}
            ]},
            "topHit": {"type": "TRACKS", "value": {"id": 1, "title": "T"}}
        });
        let all = parse_search_all(&doc);
        assert_eq!(all.tracks.len(), 1);
        assert_eq!(all.tracks[0].artist_id, Some(7));
        assert_eq!(all.tracks[0].quality.as_deref(), Some("LOSSLESS"));
        assert_eq!(all.albums.len(), 1);
        assert_eq!(all.albums[0].year.as_deref(), Some("2019"));
        assert_eq!(all.albums[0].tracks, Some(10));
        assert_eq!(all.albums[0].duration, Some(2400));
        assert_eq!(all.artists.len(), 1);
        assert_eq!(all.artists[0].picture.as_deref(), Some("abcdefab-1234-5678-9abc-def012345678"));
        assert_eq!(all.playlists.len(), 1);
        assert_eq!(all.playlists[0].count, 3);
        assert_eq!(all.videos.len(), 1);
        assert_eq!(all.videos[0].id, 9);
        assert_eq!(all.videos[0].artist_id, Some(7));
        assert_eq!(all.videos[0].duration, Some(242));
        assert_eq!(all.videos[0].image.as_deref(), Some("abcdefab-1234-5678-9abc-def012345678"));
        assert!(all.videos[0].stream_ready);
        assert!(all.videos[0].explicit);
        match all.top {
            Some(TopHit::Track(track)) => assert_eq!(track.id, 1),
            other => panic!("unexpected top hit: {other:?}"),
        }
    }

    #[test]
    fn album_artist_and_playlist_parsers_accept_nested_documents() {
        let album = parse_album_json(&serde_json::json!({
            "id": 4, "title": "Record", "artists": [{"id": 8, "name": "Band"}],
            "cover": "cover", "year": 2020, "numberOfTracks": 7, "explicit": true,
            "audioQuality": "HI_RES_LOSSLESS"
        }))
        .unwrap();
        assert_eq!(album.artist_id, Some(8));
        assert_eq!(album.year.as_deref(), Some("2020"));
        assert_eq!(album.quality.as_deref(), Some("HI_RES_LOSSLESS"));
        assert!(album.explicit);

        let artist = parse_artist_json(&serde_json::json!({"id": 8, "name": "Band", "picture": "p"})).unwrap();
        assert_eq!(artist.name, "Band");
        assert_eq!(artist.picture.as_deref(), Some("p"));

        // A create-playlist v2 response nests the object under `data`.
        let playlist = parse_playlist_json(
            &serde_json::json!({
                "data": {"uuid": "22222222-2222-2222-2222-222222222222", "title": "New"}
            }),
            Some(7),
        )
        .unwrap();
        assert_eq!(playlist.id, "22222222-2222-2222-2222-222222222222");
        assert_eq!(playlist.title, "New");
        assert_eq!(playlist.count, 0);
        assert!(!playlist.owned, "a payload with no creator is not editable");
    }

    #[test]
    fn playlist_parser_reads_description_creator_and_ownership() {
        let owned = parse_playlist_json(
            &serde_json::json!({
                "uuid": "11111111-1111-1111-1111-111111111111",
                "title": "Mine",
                "description": "  Made by me  ",
                "numberOfTracks": 12,
                "publicPlaylist": false,
                "creator": {"id": 7, "name": "me"}
            }),
            Some(7),
        )
        .unwrap();
        assert_eq!(owned.description.as_deref(), Some("Made by me"));
        assert_eq!(owned.creator, Some(7));
        assert_eq!(owned.public, Some(false));
        assert!(owned.owned);
        assert_eq!(owned.count, 12);

        let followed = parse_playlist_json(
            &serde_json::json!({
                "uuid": "33333333-3333-3333-3333-333333333333",
                "title": "Theirs",
                "description": "",
                "creator": {"id": 9}
            }),
            Some(7),
        )
        .unwrap();
        assert_eq!(followed.description, None, "a blank description is no description");
        assert!(!followed.owned);
        assert_eq!(followed.public, None);

        // Without a session there is nothing to compare the creator against, so nothing is
        // treated as editable.
        let anonymous = parse_playlist_json(
            &serde_json::json!({
                "uuid": "44444444-4444-4444-4444-444444444444", "creator": {"id": 7}
            }),
            None,
        )
        .unwrap();
        assert!(!anonymous.owned);
    }

    #[test]
    fn credits_parser_keeps_named_roles_only() {
        let credits = parse_credits(&serde_json::json!([
            {"type": "Producer", "contributors": [{"name": "Greg Kurstin", "id": 3}]},
            {"type": "Composer", "contributors": [{"name": " Adele Adkins "}, {"name": ""}]},
            {"type": "Engineer", "contributors": []},
            {"type": " ", "contributors": [{"name": "Nobody"}]},
            {"type": "Misc"}
        ]));
        assert_eq!(credits.len(), 2);
        assert_eq!(credits[0].role, "Producer");
        assert_eq!(credits[0].names, vec!["Greg Kurstin".to_string()]);
        assert_eq!(credits[1].role, "Composer");
        assert_eq!(credits[1].names, vec!["Adele Adkins".to_string()]);
        assert!(parse_credits(&serde_json::json!({"status": 400})).is_empty());
    }

    #[test]
    fn wrapped_items_unwrap_the_item_field() {
        let entry = serde_json::json!({"item": {"id": 1, "title": "A"}, "type": "ALBUM"});
        assert_eq!(wrapped_item(&entry)["id"], 1);
        let bare = serde_json::json!({"id": 2});
        assert_eq!(wrapped_item(&bare)["id"], 2);
    }

    #[test]
    fn favorite_ids_parses_numeric_and_string_sets() {
        let value = serde_json::json!({
            "TRACK": [1, 2], "ALBUM": [], "ARTIST": ["5"], "PLAYLIST": ["uuid-1", 7], "VIDEO": []
        });
        let ids = FavoriteIds {
            tracks: numeric_id_set(&value, "TRACK"),
            albums: numeric_id_set(&value, "ALBUM"),
            artists: numeric_id_set(&value, "ARTIST"),
            playlists: string_id_set(&value, "PLAYLIST"),
            videos: numeric_id_set(&value, "VIDEO"),
        };
        assert!(ids.tracks.contains(&1) && ids.tracks.contains(&2));
        assert!(ids.albums.is_empty());
        assert!(ids.artists.contains(&5));
        assert!(ids.playlists.contains("uuid-1") && ids.playlists.contains("7"));
    }

    #[test]
    fn lrc_parsing_handles_timestamps_metadata_and_sorting() {
        let lines = parse_lrc("[ar:Artist]\n[00:12.34] Hello\n[00:15.00][00:20.00] Again\n[01:02.5] End\n");
        assert_eq!(lines, vec![
            (12_340, "Hello".to_string()),
            (15_000, "Again".to_string()),
            (20_000, "Again".to_string()),
            (62_500, "End".to_string()),
        ]);
        assert!(parse_lrc("not lrc at all").is_empty());
    }

    #[test]
    fn lyrics_prefers_plain_text_and_parses_subtitles() {
        let both = parse_lyrics(&serde_json::json!({
            "lyrics": "Plain", "subtitles": "[00:01.00] One\n", "provider": "Sourced"
        }))
        .unwrap();
        assert_eq!(both.text, "Plain");
        assert_eq!(both.synced, vec![(1_000, "One".to_string())]);
        assert_eq!(both.provider.as_deref(), Some("Sourced"));

        let synced_only = parse_lyrics(&serde_json::json!({"subtitles": "[00:02.00] Two"})).unwrap();
        assert_eq!(synced_only.text, "Two");
        assert_eq!(synced_only.provider, None);

        assert!(parse_lyrics(&serde_json::json!({"lyrics": "", "subtitles": ""})).is_none());
    }

    #[test]
    fn page_sections_map_modules_to_cards_and_skip_others() {
        let page = serde_json::json!({"rows": [{"modules": [
            {"type": "HIGHLIGHT_MODULE", "pagedList": {"items": [{"id": 1, "title": "hero"}]}},
            {"type": "ALBUM_LIST", "title": "Albums", "pagedList": {"items": [
                {"id": 10, "title": "A", "artists": [{"id": 1, "name": "Ar"}]}
            ]}},
            {"type": "PLAYLIST_LIST", "title": "Playlists", "pagedList": {"items": [
                {"uuid": "33333333-3333-3333-3333-333333333333", "title": "P"}
            ]}},
            {"type": "ARTIST_LIST", "title": "Artists", "pagedList": {"items": [{"id": 2, "name": "N"}]}},
            {"type": "TRACK_LIST", "title": "Tracks", "pagedList": {"items": [{"id": 3, "title": "T"}]}},
            {"type": "MIX_LIST", "title": "Mixes", "pagedList": {"items": [{"id": "mix", "title": "M", "subTitle": "s"}]}},
            {"type": "ALBUM_LIST", "title": "Empty", "pagedList": {"items": [{"title": "no id"}]}}
        ]}]});
        let sections = parse_sections(&page);
        assert_eq!(sections.len(), 5);
        assert_eq!(sections[0].title, "Albums");
        assert!(matches!(sections[0].items[0], Card::Album(_)));
        assert!(matches!(sections[1].items[0], Card::Playlist(_)));
        assert!(matches!(sections[2].items[0], Card::Artist(_)));
        assert!(matches!(sections[3].items[0], Card::Track(_)));
        match &sections[4].items[0] {
            Card::Mix(mix) => assert_eq!(mix.id, "mix"),
            other => panic!("unexpected card: {other:?}"),
        }
    }

    #[test]
    fn suggestions_parse_arrays_and_objects() {
        let value = serde_json::json!({"suggestions": ["alpha", {"name": "beta"}, "alpha", {"title": ""}]});
        assert_eq!(parse_suggestions(&value), vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn suggestion_fallback_uses_search_names() {
        let track = track_from_value(&serde_json::json!({"id": 1, "title": "Song"})).unwrap();
        let album = parse_album_json(&serde_json::json!({"id": 2, "title": "Album"})).unwrap();
        let artist = parse_artist_json(&serde_json::json!({"id": 3, "name": "Artist"})).unwrap();
        let all = SearchAll { tracks: vec![track], albums: vec![album], artists: vec![artist], ..Default::default() };
        assert_eq!(suggestion_names(&all), vec!["Song".to_string(), "Album".to_string(), "Artist".to_string()]);
    }

    #[test]
    fn quality_chain_starts_at_preference_and_falls_back() {
        assert_eq!(quality_chain(Quality::HiResLossless), vec!["HI_RES_LOSSLESS", "LOSSLESS", "HIGH", "LOW"]);
        assert_eq!(quality_chain(Quality::Lossless), vec!["LOSSLESS", "HIGH", "LOW"]);
        assert_eq!(quality_chain(Quality::High), vec!["HIGH", "LOW"]);
        assert_eq!(quality_chain(Quality::Low), vec!["LOW"]);
    }

    /// A rate-limited or flaky request must be retried on the same tier rather than quietly
    /// playing a lower one; a tier TIDAL simply refuses (DRM, 4xx) moves on to the next.
    #[test]
    fn only_rate_limits_and_network_errors_are_retried_on_the_same_tier() {
        assert!(is_transient_failure("TIDAL rate limit reached; try again in a moment"));
        assert!(is_transient_failure("TIDAL network error: timed out"));
        assert!(!is_transient_failure("TIDAL returned a DRM-protected stream requiring a licensed CDM"));
        assert!(!is_transient_failure("TIDAL request failed (404): not found"));
    }

    /// Once a stream is in hand, only strictly better tiers are worth another request.
    #[test]
    fn probing_stops_at_tiers_that_cannot_improve_the_best_stream() {
        assert!(worth_probing(None, "HIGH"));
        // Asked for hi-res, TIDAL answered HIGH: LOSSLESS could still be better, HIGH/LOW not.
        assert!(worth_probing(Some(quality_rank(Some("HIGH"))), "LOSSLESS"));
        assert!(!worth_probing(Some(quality_rank(Some("HIGH"))), "HIGH"));
        assert!(!worth_probing(Some(quality_rank(Some("HIGH"))), "LOW"));
        assert!(!worth_probing(Some(quality_rank(Some("LOSSLESS"))), "HIGH"));
    }

    #[test]
    fn playlist_id_validation_rejects_arbitrary_input() {
        assert!(validate_playlist_id("11111111-1111-1111-1111-111111111111").is_ok());
        assert!(validate_playlist_id("not a uuid").is_err());
        assert!(validate_playlist_id("").is_err());
    }

    /// `GET /trackManifests/{id}` as TIDAL returns it (URL and hash shortened).
    #[test]
    fn track_manifest_response_gives_the_served_format_and_gain() {
        let value = serde_json::json!({"data": {"id": "61721286", "type": "trackManifests", "attributes": {
            "trackPresentation": "FULL",
            "uri": "https://im-fa.manifest.tidal.com/1/manifests/abc.mpd?token=t",
            "formats": ["FLAC"],
            "albumAudioNormalizationData": {"replayGain": -8.64, "peakAmplitude": 0.98},
            "trackAudioNormalizationData": {"replayGain": -8.56, "peakAmplitude": 0.98}
        }}});
        let manifest = parse_track_manifest(&value).unwrap();
        assert_eq!(manifest.uri, "https://im-fa.manifest.tidal.com/1/manifests/abc.mpd?token=t");
        assert_eq!(manifest.format.as_deref(), Some("FLAC"));
        assert_eq!(manifest.replay_gain, Some(-8.56));
        assert_eq!(manifest_quality("FLAC"), Some("LOSSLESS"));
        assert_eq!(manifest_quality("FLAC_HIRES"), Some("HI_RES_LOSSLESS"));
        assert_eq!(manifest_quality("AACLC"), Some("HIGH"));

        assert!(parse_track_manifest(&serde_json::json!({"data": {"attributes": {"formats": ["FLAC"]}}})).is_err());
        assert!(parse_track_manifest(&serde_json::json!({})).is_err());
    }

    #[test]
    fn representation_id_carries_sample_rate_and_bit_depth() {
        assert_eq!(parse_representation_id("FLAC_HIRES,44100,24"), (Some(44_100), Some(24)));
        assert_eq!(parse_representation_id("FLAC,44100,16"), (Some(44_100), Some(16)));
        assert_eq!(parse_representation_id("AACLC"), (None, None));
    }

    /// Lossless preferences use the manifests endpoint; the AAC tiers stay on v1.
    #[test]
    fn only_lossless_preferences_ask_for_flac_manifests() {
        assert_eq!(manifest_formats(Quality::HiResLossless), Some(&["FLAC_HIRES", "FLAC", "AACLC"][..]));
        assert_eq!(manifest_formats(Quality::Lossless), Some(&["FLAC", "AACLC"][..]));
        assert_eq!(manifest_formats(Quality::High), None);
        assert_eq!(manifest_formats(Quality::Low), None);
    }

    /// TIDAL's favourites list repeats entries (a different set each request); each id must
    /// appear once, at its first (newest) position.
    #[test]
    fn favourites_keep_each_id_once_at_its_first_position() {
        let list = vec![(3u64, "c"), (1, "a"), (3, "c again"), (2, "b"), (1, "a again"), (2, "b again")];
        let unique = dedupe_by(list, |item| item.0);
        assert_eq!(unique, vec![(3, "c"), (1, "a"), (2, "b")]);
        assert!(dedupe_by(Vec::<(u64, &str)>::new(), |item| item.0).is_empty());
    }
}
