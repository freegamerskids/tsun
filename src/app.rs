//! Application state: the worker protocol (`Cmd`/`Ev`), the UI state machine, and the
//! `AppSpec` callbacks (`init`, `setup`, `poll`, `on_key`) that drive it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use rune::{AppHandle, Key, KeyEvent, TextEdit, Tray, TrayEvent, TrayIcon, TrayItem, TraySpec, Tx, Waker};

use crate::art;
use crate::audio::Volume;
use crate::cover::CoverArt;
use crate::gapless::GaplessState;
use crate::history::{History, Route};
use crate::history_log::{self, PlayEntry};
use crate::lastfm::LastFm;
use crate::local;
use crate::mpris::{Mpris, MprisCommand, MprisMeta, MprisPlayback};
use crate::notify::{Notification, Notifier};
use crate::playback::{self, Queue};
use crate::recent;
use crate::settings::{Settings, StartupPage};
use crate::tidal::{
    Album, AlbumExtras, Artist, ArtistPage, Credit, FavKind, FavoriteIds, HomeSection, Library,
    LyricsData, Mix, Playlist, Profile, Quality, SearchAll, Section, Track, Video, VideoStream,
};
use crate::ui::context_menu::MenuState;
use crate::ui::dialogs::{Dialog, TrackInfo};
use crate::ui::tracks::{Derived, DerivedLibrary, DerivedTracks, Tracks};
use crate::video;
use crate::worker::worker;

/// What kind of image an element draws, so the fetch for it uses the right TIDAL variant (and
/// is only made for art a surface actually shows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArtKind {
    /// A cover drawn small: a track row or a card at the 160px square.
    Cover,
    /// A cover drawn large: an album tile or header, at the 320px square.
    CoverLarge,
    /// An artist or user picture: the 320px square.
    Picture,
    /// A video's 16:9 thumbnail.
    Video,
}

// ---------------------------------------------------------------------------------------------
// Worker protocol

pub(crate) enum Cmd {
    Init,
    Login,
    StartPkce,
    FinishPkce { url: String },
    SearchAll { seq: u64, query: String },
    Suggestions { seq: u64, query: String },
    Explore,
    NewReleases,
    HomeSections,
    Lyrics { track_id: u64 },
    Profile,
    Home,
    Playlists,
    Mixes,
    Library,
    FavoriteIds,
    SetFavorite { kind: FavKind, id: String, on: bool },
    OpenAlbum(u64),
    OpenArtist(u64),
    ArtistRadio(u64),
    OpenMix(String),
    OpenPlaylist(String),
    /// Loads one playlist's metadata (title, description, creator, public flag).
    PlaylistMeta(String),
    /// Creates a playlist, then adds `tracks` to it when it already exists.
    CreatePlaylist { title: String, description: String, tracks: Vec<u64> },
    AddToPlaylist { id: String, track_ids: Vec<u64> },
    RemoveFromPlaylist { id: String, index: u64 },
    /// Moves a playlist item from one index to another (own playlists).
    MovePlaylistItem { id: String, from: u64, to: u64 },
    DeletePlaylist(String),
    RenamePlaylist { id: String, title: String, description: String },
    /// TIDAL's track radio: the seed track's id, and the tracks it answers with play as a queue.
    TrackRadio(u64),
    /// The track credits for the track-info dialog.
    TrackCredits(u64),
    /// The album-page extras (review, credits, related albums).
    AlbumExtras(u64),
    /// The next page of an artist's top tracks, for the "Show all" button.
    ArtistTopTracks { id: u64, offset: u32 },
    /// Resolves a video's HLS master playlist from TIDAL.
    OpenVideo { seq: u64, id: u64 },
    Play { seq: u64, id: u64 },
    /// Plays a local file directly through the shared audio player.
    PlayLocal {
        seq: u64,
        id: u64,
        path: PathBuf,
        quality: String,
        replaygain: Option<f32>,
    },
    SetPaused { seq: u64, paused: bool },
    Seek { seq: u64, seconds: f64 },
    SetVolume(f32),
    SetMuted(bool),
    SetQuality(Quality),
    SetNormalize(bool),
    SetOutputDevice(Option<String>),
    /// Opens the next queue track ahead of time so it can start gaplessly.
    Preload { id: u64 },
    /// Opens the next local queue track ahead of time for the same gapless hand-off.
    PreloadLocal { id: u64, path: PathBuf, replaygain: Option<f32> },
    /// Drops any outstanding preload, e.g. because the queue changed.
    CancelPreload,
    Stop,
    Logout,
}

pub(crate) enum Ev {
    Ready { logged_in: bool },
    InitFailed(String),
    LoginUrl(String),
    PkceUrl(String),
    LoginDone,
    LoginFailed(String),
    PkceFailed(String),
    SearchAllDone { seq: u64, result: Result<SearchAll, String> },
    Suggestions { seq: u64, result: Result<Vec<String>, String> },
    ExploreResult(Result<Vec<Section>, String>),
    NewReleases(Result<Vec<Section>, String>),
    HomeSections(Result<Vec<Section>, String>),
    Lyrics { track_id: u64, result: Result<Option<LyricsData>, String> },
    Profile(Result<Profile, String>),
    HomeResult(Result<Vec<Track>, String>),
    HomeRecommendations(Result<Vec<HomeSection>, String>),
    Mixes(Result<Vec<Mix>, String>),
    MixTracks { id: String, result: Result<Vec<Track>, String> },
    Playlists(Result<Vec<Playlist>, String>),
    Library(Result<Library, String>),
    Favorites(Result<FavoriteIds, String>),
    FavoriteChanged { kind: FavKind, id: String, on: bool, error: Option<String> },
    Album { id: u64, result: Result<(Album, Vec<Track>), String> },
    Artist { id: u64, result: Result<ArtistPage, String> },
    Radio { id: u64, result: Result<Vec<Track>, String> },
    PlaylistTracks { id: String, result: Result<Vec<Track>, String> },
    /// One playlist's metadata, as the playlist page asked for on open.
    PlaylistMeta { id: String, result: Result<Playlist, String> },
    /// A created playlist, plus the result of adding the tracks that were part of the request
    /// (`None` when the request carried none).
    PlaylistCreated { result: Result<Playlist, String>, added: Option<Result<usize, String>> },
    PlaylistAdded { id: String, count: usize, result: Result<(), String> },
    PlaylistRemoved { id: String, index: u64, result: Result<(), String> },
    /// A playlist item moved from `from` to `to`; on failure the UI rolls the order back.
    PlaylistMoved { id: String, from: u64, to: u64, result: Result<(), String> },
    PlaylistDeleted { id: String, result: Result<(), String> },
    PlaylistRenamed { id: String, title: String, description: String, result: Result<(), String> },
    TrackRadio(Result<Vec<Track>, String>),
    TrackCredits { track_id: u64, result: Result<Vec<Credit>, String> },
    /// The album-page extras (review, credits, related albums).
    AlbumExtras { id: u64, result: Result<AlbumExtras, String> },
    /// A video's HLS master playlist, or TIDAL's reason it cannot be played.
    /// `volume` is the same shared output handle the music player uses.
    VideoReady { seq: u64, id: u64, result: Result<VideoStream, String>, volume: Volume },
    /// The next page of an artist's top tracks, plus TIDAL's total for the artist.
    ArtistTopTracks { id: u64, result: Result<(Vec<Track>, Option<u64>), String> },
    SearchFailed { seq: u64, error: String },
    Playing { seq: u64, id: u64, quality: String, output: Option<String> },
    PlayFailed { seq: u64, error: String },
    PauseChanged { seq: u64, paused: bool, error: Option<String> },
    PlaybackProgress { seq: u64, elapsed: u64 },
    PlaybackEnded { seq: u64, error: Option<String> },
    /// The current track reached its natural end; the queue may auto-advance.
    TrackFinished { seq: u64 },
    /// The preloaded next track was swapped in without interrupting the output: the queue has
    /// advanced, but the same stream is still playing. `output` is the format it is running at
    /// (it can change when the hand-off had to re-open the device).
    GaplessAdvanced { seq: u64, id: u64, output: Option<String> },
    Stopped,
}

// ---------------------------------------------------------------------------------------------
// UI state

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SearchState {
    Idle,
    Searching,
    Done,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Home,
    Search,
    Collection,
    Playlist,
    Mixes,
    Mix,
    Explore,
    Album,
    Artist,
    /// The Local files library (and its album/artist drill-downs).
    Local,
    LocalAlbum,
    LocalArtist,
    Settings,
}

/// The tabs of the search results page.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SearchTab {
    #[default]
    Top,
    Tracks,
    Albums,
    Artists,
    Playlists,
    Videos,
}

/// The tabs of the home page.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HomeTab {
    #[default]
    ForYou,
    NewReleases,
}

/// The tabs of the Collection page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollectionTab {
    Playlists,
    Albums,
    Artists,
    Tracks,
    Videos,
    Mixes,
    /// What this app has played, recorded locally (TIDAL's own history endpoints 404).
    History,
}

impl CollectionTab {
    /// Every tab, in the order the strip draws them.
    pub(crate) const ALL: [CollectionTab; 7] = [
        CollectionTab::Playlists,
        CollectionTab::Albums,
        CollectionTab::Artists,
        CollectionTab::Tracks,
        CollectionTab::Videos,
        CollectionTab::Mixes,
        CollectionTab::History,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            CollectionTab::Playlists => "Playlists",
            CollectionTab::Albums => "Albums",
            CollectionTab::Artists => "Artists",
            CollectionTab::Tracks => "Tracks",
            CollectionTab::Videos => "Videos",
            CollectionTab::Mixes => "Mixes",
            CollectionTab::History => "History",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            CollectionTab::Playlists => "playlists",
            CollectionTab::Albums => "albums",
            CollectionTab::Artists => "artists",
            CollectionTab::Tracks => "tracks",
            CollectionTab::Videos => "videos",
            CollectionTab::Mixes => "mixes",
            CollectionTab::History => "history",
        }
    }
}

/// The Collection sort options, mirroring the official client's dropdown.
///
/// Sorting is done locally so switching order costs no round-trip; "Date added" is the order
/// TIDAL's favourites endpoints return (newest first, which [`crate::tidal::Client`] preserves).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) enum SortKey {
    /// Newest favourites first, the order TIDAL returns them.
    #[default]
    DateAdded,
    /// Title / name, A–Z.
    Name,
    /// Artist name, A–Z.
    Artist,
    /// Release year, newest first (albums only).
    ReleaseDate,
}

impl SortKey {
    /// Every option, in the order the dropdown lists them.
    pub(crate) const ALL: [SortKey; 4] =
        [SortKey::DateAdded, SortKey::Name, SortKey::Artist, SortKey::ReleaseDate];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SortKey::DateAdded => "Date added",
            SortKey::Name => "Name",
            SortKey::Artist => "Artist",
            SortKey::ReleaseDate => "Release date",
        }
    }

    /// A stable slug for element keys and labels.
    pub(crate) fn key(self) -> &'static str {
        match self {
            SortKey::DateAdded => "date-added",
            SortKey::Name => "name",
            SortKey::Artist => "artist",
            SortKey::ReleaseDate => "release-date",
        }
    }

    /// Whether the option applies to `tab`: artists have no artist field to sort by and only
    /// albums carry a release date.
    pub(crate) fn applies_to(self, tab: CollectionTab) -> bool {
        match self {
            SortKey::DateAdded | SortKey::Name => true,
            SortKey::Artist => matches!(
                tab,
                CollectionTab::Albums | CollectionTab::Tracks | CollectionTab::Videos
            ),
            SortKey::ReleaseDate => tab == CollectionTab::Albums,
        }
    }
}

pub(crate) enum Login {
    Checking,
    LoggedOut,
    DeviceStarting,
    DeviceAwaiting { url: String },
    PkceStarting,
    PkceAwaiting { url: String },
    LoggedIn,
    LoginFailed(String),
    InitFailed(String),
}

pub(crate) struct NowPlaying {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) cover: Option<String>,
    pub(crate) album_id: Option<u64>,
    pub(crate) artist_id: Option<u64>,
    pub(crate) elapsed: u64,
    pub(crate) duration: Option<u64>,
    pub(crate) quality: Option<String>,
    /// The real output format label ("96 kHz · f32 · device"), once the stream is open.
    pub(crate) output: Option<String>,
}

/// One launched video: TIDAL's catalogue entry, the decode/playback engine,
/// and what a replay needs to start a fresh engine.
pub(crate) struct VideoSession {
    pub(crate) video: Video,
    pub(crate) engine: video::Engine,
    pub(crate) master_url: String,
    pub(crate) volume: Volume,
    pub(crate) device: Option<String>,
    pub(crate) duration_ms: u64,
}

pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) error: bool,
}

impl Notice {
    pub(crate) fn error(text: impl Into<String>) -> Self {
        Notice { text: text.into(), error: true }
    }

    pub(crate) fn info(text: impl Into<String>) -> Self {
        Notice { text: text.into(), error: false }
    }
}

pub(crate) struct Ui {
    pub(crate) cmd_tx: Sender<Cmd>,
    pub(crate) cmd_rx: Option<Receiver<Cmd>>,
    pub(crate) ev_tx: Option<Sender<Ev>>,
    pub(crate) ev_rx: Receiver<Ev>,
    pub(crate) worker_down: bool,
    pub(crate) login: Login,
    pub(crate) page: Page,
    pub(crate) home_state: SearchState,
    pub(crate) home_tracks: Vec<Track>,
    pub(crate) recommendations: Vec<HomeSection>,
    pub(crate) mixes: Vec<Mix>,
    pub(crate) mix_offset: usize,
    /// Cards across a carousel or grid, derived from the main area's width by `ui::view` each
    /// frame so cards keep a steady size as the window grows.
    pub(crate) columns: std::cell::Cell<usize>,
    /// Whether the window is too narrow for the queue and lyrics panels side by side (set by
    /// `ui::view` from the laid-out window width).
    pub(crate) narrow_window: std::cell::Cell<bool>,
    /// How the top bar lays itself out, derived from the main area's laid-out width by
    /// `ui::view` (a breakpoint read, so a resize only re-runs the view when the mode changes).
    pub(crate) bar_mode: std::cell::Cell<crate::ui::topbar::BarMode>,
    /// Whether the search bar is expanded from its collapsed icon button. Only meaningful while
    /// `bar_mode` is collapsed; Escape or a blur with an empty query collapses it again.
    pub(crate) search_expanded: bool,
    pub(crate) mix: Option<Mix>,
    pub(crate) mix_tracks: Tracks,
    pub(crate) mix_state: SearchState,
    pub(crate) playlists: Vec<Playlist>,
    pub(crate) playlist: Option<Playlist>,
    pub(crate) playlist_tracks: Tracks,
    pub(crate) playlist_state: SearchState,
    pub(crate) album: Option<Album>,
    pub(crate) album_tracks: Tracks,
    pub(crate) album_state: SearchState,
    /// The open album's review, credits and related albums, once loaded.
    pub(crate) album_extras: Option<AlbumExtras>,
    /// The album whose header review is expanded by its "More" button. Keyed by album id, so
    /// navigating to another album shows its own review collapsed.
    pub(crate) album_review_open: Option<u64>,
    pub(crate) artist: Option<Artist>,
    pub(crate) artist_page: Option<ArtistPage>,
    pub(crate) artist_state: SearchState,
    /// Whether the artist page is already showing every top track TIDAL has.
    pub(crate) artist_top_all: bool,
    /// Whether a "Show all" page of top tracks is in flight.
    pub(crate) artist_top_loading: bool,
    pub(crate) library: Option<Library>,
    pub(crate) library_state: SearchState,
    pub(crate) collection_tab: CollectionTab,
    /// Substring the Collection tab filters its list by (name/artist). Cleared only by the user.
    pub(crate) collection_filter: TextEdit,
    /// How the Collection tabs order their list; kept for the session.
    pub(crate) collection_sort: SortKey,
    /// Whether the Collection sort dropdown is open.
    pub(crate) collection_sort_open: bool,
    pub(crate) favorite_tracks: HashSet<u64>,
    pub(crate) favorite_albums: HashSet<u64>,
    pub(crate) favorite_artists: HashSet<u64>,
    pub(crate) favorite_playlists: HashSet<String>,
    pub(crate) favorite_videos: HashSet<u64>,
    /// Visited routes behind the Back and Forward buttons.
    pub(crate) history: History,
    pub(crate) artwork: Option<art::Artwork>,
    pub(crate) profile: Option<Profile>,
    pub(crate) profile_menu_open: bool,
    pub(crate) query: TextEdit,
    pub(crate) pkce_redirect: TextEdit,
    pub(crate) pkce_submitting: bool,
    pub(crate) search_state: SearchState,
    pub(crate) last_query: String,
    pub(crate) results: Tracks,
    pub(crate) selected: Option<u64>,
    /// The row under the pointer, so a track row can show its play button: the id of the track
    /// whose row the pointer is inside, cleared when it leaves. Only one row is hovered at a
    /// time, so the id is enough.
    pub(crate) row_hover: Option<u64>,
    /// The combined multi-type results of the current search.
    pub(crate) search_all: Option<SearchAll>,
    /// Which results tab the search page shows.
    pub(crate) search_tab: SearchTab,
    /// Search-box suggestions from TIDAL, and the keyboard selection into them.
    pub(crate) suggestions: Vec<String>,
    pub(crate) suggest_seq: u64,
    pub(crate) suggest_selected: Option<usize>,
    /// Set when Escape hides the dropdown; typing clears it again.
    pub(crate) suggest_dismissed: bool,
    /// The box text `poll` last saw, and when it last changed, for the debounce.
    pub(crate) suggest_seen: String,
    pub(crate) suggest_edited_at: Option<f64>,
    /// The query the last suggestion request carried, so identical ones are skipped.
    pub(crate) suggest_query: String,
    /// Recently submitted searches, newest first (persisted).
    pub(crate) recent_searches: Vec<String>,
    /// Explore feed and new releases, both generic card sections.
    pub(crate) explore_state: SearchState,
    pub(crate) explore: Vec<Section>,
    pub(crate) new_releases: Vec<Section>,
    /// The extra home-feed sections (`/pages/home` album and playlist rows).
    pub(crate) home_sections: Vec<Section>,
    pub(crate) home_tab: HomeTab,
    /// Paging offsets of the card carousels, keyed by their element key.
    pub(crate) carousel_offsets: HashMap<String, usize>,
    /// The current track's lyrics: `(track id, lyrics)`, where a `None` payload means TIDAL
    /// has none for the track.
    pub(crate) lyrics: Option<(u64, Option<LyricsData>)>,
    pub(crate) lyrics_state: SearchState,
    /// The last highlighted lyrics line, so `poll` scrolls only when it changes.
    pub(crate) lyrics_line: Option<usize>,
    /// Whether the lyrics panel is shown; the player bar's toggle flips it.
    pub(crate) lyrics_open: bool,
    /// Whether the full-screen Now Playing overlay is up.
    pub(crate) now_playing_open: bool,
    /// The preload state machine behind gapless playback.
    pub(crate) preload: GaplessState,
    /// Average cover colours for the overlay's gradient, computed off the frame.
    pub(crate) covers: Option<CoverArt>,
    /// Desktop notifications, when the user has them on and a session bus is reachable.
    pub(crate) notifier: Option<Notifier>,
    /// The host's output devices as `(name, is_default)`, enumerated when the Settings page is
    /// first shown (or when its Refresh button is pressed) rather than once per frame.
    pub(crate) output_devices: Option<Vec<(String, bool)>>,
    pub(crate) search_seq: u64,
    pub(crate) play_seq: u64,
    /// The play queue: play order, current track, shuffle and repeat.
    pub(crate) queue: Queue,
    /// Whether the right-hand queue panel is open.
    pub(crate) queue_open: bool,
    /// While the progress bar is being dragged: the previewed fraction `0..=1`.
    pub(crate) scrub: Option<f32>,
    /// Wheel points over the volume control that have not added up to a notch yet, so a
    /// trackpad's run of small deltas steps the volume at the same rate as a notched wheel.
    pub(crate) volume_wheel: f32,
    pub(crate) pending_play: Option<NowPlaying>,
    pub(crate) now_playing: Option<NowPlaying>,
    /// The music video playing in the full-screen video view, once TIDAL resolved
    /// its stream, and the one being resolved (so the overlay can show at once).
    pub(crate) video: Option<VideoSession>,
    pub(crate) video_pending: Option<Video>,
    /// Bumped per launched video so a late `VideoReady` for a closed one is ignored.
    pub(crate) video_seq: u64,
    /// The whole second of the video clock the last frame drew, so `poll` redraws
    /// the time labels only when they change.
    pub(crate) video_clock: Option<u64>,
    /// The engine's frame counter at the last poll, to notice a new picture.
    pub(crate) video_frames: u64,
    pub(crate) paused: bool,
    pub(crate) pause_pending: bool,
    pub(crate) stopping: bool,
    /// Persisted preferences: volume, mute, shuffle, repeat, quality and normalization.
    pub(crate) settings: Settings,
    /// The locally-recorded play history, newest first (TIDAL's own history 404s).
    pub(crate) history_log: Vec<PlayEntry>,
    /// The track id of the current play that has already been recorded, so one play is logged
    /// once however many progress ticks arrive.
    pub(crate) play_recorded: Option<u64>,
    /// The status-notifier tray icon, when one could be created (Linux/Wayland/X11 with a
    /// StatusNotifierWatcher). Dropping it removes the icon.
    pub(crate) tray: Option<Tray>,
    /// Whether a tray was created; the "Close to tray" setting only appears when it was.
    pub(crate) tray_available: bool,
    /// The play/pause state the tray menu was last built for, so it is rebuilt only on a change.
    pub(crate) tray_playing: Option<bool>,
    /// A handle to the event loop, so the tray can show or quit the window from its own thread.
    pub(crate) app_handle: Option<AppHandle<Ui>>,
    /// The playlist reorder currently awaiting confirmation, so a failed move can be rolled back
    /// (and a stale answer for a superseded move ignored).
    pub(crate) playlist_move: Option<(String, u64, u64)>,
    /// The last MPRIS metadata key `(track id, cover)`, so a signal goes out only on a change.
    pub(crate) mpris_track: Option<(u64, Option<String>)>,
    /// The last volume pushed to MPRIS, so `set_volume` only signals on a change.
    pub(crate) mpris_volume: Option<f64>,
    pub(crate) mpris: Option<Mpris>,
    pub(crate) mpris_rx: Option<Receiver<MprisCommand>>,
    /// Last.fm scrobbling, when the thread could be started. The card reads its state; playback
    /// events reach it through [`Ui::emit_media`].
    pub(crate) lastfm: Option<LastFm>,
    /// The scanned local-file library and the state of its background scan.
    pub(crate) local: local::Library,
    /// One-off files from the command line or a drop, which are not in any scanned folder but
    /// must still play (and keep their path for the queue and the file manager).
    pub(crate) local_extra: HashMap<u64, local::LocalTrack>,
    pub(crate) local_state: SearchState,
    pub(crate) local_scan: Option<Receiver<local::ScanMsg>>,
    pub(crate) local_progress: Option<(usize, usize)>,
    pub(crate) local_skipped: usize,
    pub(crate) local_tab: local::LocalTab,
    pub(crate) local_filter: TextEdit,
    pub(crate) local_sort: local::LocalSort,
    /// The open local album/artist key, and the folder the Folders tab is browsing.
    pub(crate) local_album: Option<String>,
    pub(crate) local_artist: Option<String>,
    pub(crate) local_folder: Option<PathBuf>,
    /// The Settings card's "folder path" field.
    pub(crate) local_add_path: TextEdit,
    /// Rune's waker, kept so the local scanner can wake the UI.
    pub(crate) waker: Option<Waker>,
    pub(crate) notice: Option<Notice>,
    /// The last OS window title pushed with `Tx::set_title`, so `poll` signals only on a change.
    pub(crate) window_title: Option<String>,
    /// The open context menu: the popup a row's or header's "…" button opened, anchored to the
    /// element it came from.
    pub(crate) menu: Option<MenuState>,
    /// The open modal dialog (create/edit/delete a playlist, or a track's info and credits).
    pub(crate) dialog: Option<Dialog>,
    /// The title and description fields the create and edit dialogs share.
    pub(crate) playlist_form_title: TextEdit,
    pub(crate) playlist_form_desc: TextEdit,
    /// The Discord Rich Presence client, once enabled with a valid Application ID.
    pub(crate) discord: Option<crate::discord::Discord>,
    /// The config the current client was started with, so a switch change retunes it instead
    /// of reconnecting, and an Application ID change does reconnect.
    pub(crate) discord_config: Option<crate::discord::Config>,
    /// The Discord status the last frame drew, so `poll` repaints only on a change.
    pub(crate) discord_status: Option<crate::discord::Status>,
    /// The Application ID field of the Settings card.
    pub(crate) discord_client_id: TextEdit,
    /// The "Activity name" field of the Settings card.
    pub(crate) discord_activity_name: TextEdit,
    /// Bumped whenever a loaded list changes (the library, the local index, the history, the
    /// search results, an artist page), so the pages' derived lists rebuild only then — and a
    /// commit that changes nothing but the clock rebuilds nothing.
    pub(crate) data_rev: u64,
    /// The Collection's filtered, sorted views, rebuilt only when their inputs change.
    pub(crate) collection_tracks: DerivedTracks,
    pub(crate) collection_albums: Derived,
    pub(crate) collection_artists: Derived,
    pub(crate) collection_playlists: Derived,
    /// The recorded history as playable tracks.
    pub(crate) history_rows: DerivedTracks,
    /// The local library's filtered, sorted tracks, albums and artists.
    pub(crate) local_tracks: DerivedLibrary,
    pub(crate) local_albums: Derived,
    pub(crate) local_artists: Derived,
    /// The tracks of the local album, artist or folder page that is open.
    pub(crate) local_page_tracks: DerivedTracks,
    /// The open artist's top tracks as shown (the ten-track preview, or all of them).
    pub(crate) artist_top: DerivedTracks,
    /// The play queue's tracks, for the queue panel's rows.
    pub(crate) queue_tracks: DerivedTracks,
    /// Art the last frame drew that the cache does not hold yet: `poll` asks the fetch threads
    /// for it, so only what a surface shows is ever fetched or decoded.
    pub(crate) art_wanted: std::cell::RefCell<Vec<(ArtKind, String)>>,
}

/// A fresh history, already on Home so Back has somewhere to return to.
fn initial_history() -> History {
    let mut history = History::default();
    history.reset(Route::Home);
    history
}

/// Moves the item at `from` to `to` within `items`, as the playlist and queue reorders do.
/// A no-op when either index is out of range.
fn swap_items<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from == to || from >= items.len() || to >= items.len() {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
}

/// Inserts or removes the numeric `id` in a favourite-id set.
fn set_id(set: &mut HashSet<u64>, id: &str, on: bool) {
    let Ok(id) = id.parse::<u64>() else { return };
    if on {
        set.insert(id);
    } else {
        set.remove(&id);
    }
}

/// The second line of a track-change notification: `"artist — album"`, or whichever half is
/// known. Empty when neither is.
fn notification_body(playing: &NowPlaying) -> String {
    match (playing.artist.is_empty(), playing.album.is_empty()) {
        (false, false) => format!("{} — {}", playing.artist, playing.album),
        (false, true) => playing.artist.clone(),
        (true, false) => playing.album.clone(),
        (true, true) => String::new(),
    }
}

/// Builds the initial state, remembering the event-loop handle so the tray can reach it.
pub(crate) fn init_with_handle(handle: AppHandle<Ui>) -> Ui {
    let mut ui = init();
    ui.app_handle = Some(handle);
    ui
}

pub(crate) fn init() -> Ui {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (ev_tx, ev_rx) = mpsc::channel();
    let settings = Settings::load();
    // The Discord card's fields start on what was saved; `settings` itself moves into the UI.
    let saved_discord_client_id = settings.discord_client_id.clone();
    let saved_discord_activity_name = settings.discord_activity_name.clone();
    let mut queue = Queue::new();
    queue.set_shuffle(settings.shuffle);
    queue.set_repeat(settings.repeat);
    let ui = Ui {
        cmd_tx,
        cmd_rx: Some(cmd_rx),
        ev_tx: Some(ev_tx),
        ev_rx,
        worker_down: false,
        login: Login::Checking,
        page: Page::Home,
        home_state: SearchState::Idle,
        home_tracks: Vec::new(),
        recommendations: Vec::new(),
        mixes: Vec::new(),
        mix_offset: 0,
        columns: std::cell::Cell::new(4),
        narrow_window: std::cell::Cell::new(false),
        bar_mode: std::cell::Cell::new(crate::ui::topbar::BarMode::Field),
        search_expanded: false,
        mix: None,
        mix_tracks: Tracks::default(),
        mix_state: SearchState::Idle,
        playlists: Vec::new(),
        playlist: None,
        playlist_tracks: Tracks::default(),
        playlist_state: SearchState::Idle,
        album: None,
        album_tracks: Tracks::default(),
        album_state: SearchState::Idle,
        album_extras: None,
        album_review_open: None,
        artist: None,
        artist_page: None,
        artist_state: SearchState::Idle,
        artist_top_all: false,
        artist_top_loading: false,
        library: None,
        library_state: SearchState::Idle,
        collection_tab: CollectionTab::Playlists,
        collection_filter: TextEdit::new(""),
        collection_sort: SortKey::default(),
        collection_sort_open: false,
        favorite_tracks: HashSet::new(),
        favorite_albums: HashSet::new(),
        favorite_artists: HashSet::new(),
        favorite_playlists: HashSet::new(),
        favorite_videos: HashSet::new(),
        history: initial_history(),
        artwork: None,
        profile: None,
        profile_menu_open: false,
        query: TextEdit::new(""),
        pkce_redirect: TextEdit::new(""),
        pkce_submitting: false,
        search_state: SearchState::Idle,
        last_query: String::new(),
        results: Tracks::default(),
        selected: None,
        row_hover: None,
        search_all: None,
        search_tab: SearchTab::Top,
        suggestions: Vec::new(),
        suggest_seq: 0,
        suggest_selected: None,
        suggest_dismissed: false,
        suggest_seen: String::new(),
        suggest_edited_at: None,
        suggest_query: String::new(),
        recent_searches: recent::load(),
        explore_state: SearchState::Idle,
        explore: Vec::new(),
        new_releases: Vec::new(),
        home_sections: Vec::new(),
        home_tab: HomeTab::ForYou,
        carousel_offsets: HashMap::new(),
        lyrics: None,
        lyrics_state: SearchState::Idle,
        lyrics_line: None,
        lyrics_open: false,
        now_playing_open: false,
        preload: GaplessState::new(),
        covers: None,
        notifier: None,
        output_devices: None,
        search_seq: 0,
        play_seq: 0,
        queue,
        queue_open: false,
        scrub: None,
        volume_wheel: 0.0,
        pending_play: None,
        now_playing: None,
        video: None,
        video_pending: None,
        video_seq: 0,
        video_clock: None,
        video_frames: 0,
        paused: false,
        pause_pending: false,
        stopping: false,
        settings,
        history_log: history_log::load(),
        play_recorded: None,
        tray: None,
        tray_available: false,
        tray_playing: None,
        app_handle: None,
        playlist_move: None,
        mpris_track: None,
        mpris_volume: None,
        mpris: None,
        mpris_rx: None,
        lastfm: None,
        local: local::Library::default(),
        local_extra: HashMap::new(),
        local_state: SearchState::Idle,
        local_scan: None,
        local_progress: None,
        local_skipped: 0,
        local_tab: local::LocalTab::default(),
        local_filter: TextEdit::new(""),
        local_sort: local::LocalSort::default(),
        local_album: None,
        local_artist: None,
        local_folder: None,
        local_add_path: TextEdit::new(""),
        waker: None,
        notice: None,
        window_title: None,
        menu: None,
        dialog: None,
        playlist_form_title: TextEdit::new(""),
        playlist_form_desc: TextEdit::new(""),
        discord: None,
        discord_config: None,
        discord_status: None,
        discord_client_id: TextEdit::new(saved_discord_client_id),
        discord_activity_name: TextEdit::new(saved_discord_activity_name),
        data_rev: 0,
        collection_tracks: DerivedTracks::default(),
        collection_albums: Derived::default(),
        collection_artists: Derived::default(),
        collection_playlists: Derived::default(),
        history_rows: DerivedTracks::default(),
        local_tracks: DerivedLibrary::default(),
        local_albums: Derived::default(),
        local_artists: Derived::default(),
        local_page_tracks: DerivedTracks::default(),
        artist_top: DerivedTracks::default(),
        queue_tracks: DerivedTracks::default(),
        art_wanted: std::cell::RefCell::new(Vec::new()),
    };
    // Push the loaded preferences to the worker; the channel buffers them until the worker
    // starts draining in `setup`.
    let _ = ui.cmd_tx.send(Cmd::SetVolume(ui.settings.volume));
    let _ = ui.cmd_tx.send(Cmd::SetMuted(ui.settings.muted));
    let _ = ui.cmd_tx.send(Cmd::SetQuality(ui.settings.quality));
    let _ = ui.cmd_tx.send(Cmd::SetNormalize(ui.settings.normalize));
    let _ = ui.cmd_tx.send(Cmd::SetOutputDevice(ui.settings.output_device.clone()));
    ui
}

pub(crate) fn setup(s: &mut Ui, waker: Waker) {
    let Some(cmd_rx) = s.cmd_rx.take() else { return };
    let Some(ev_tx) = s.ev_tx.take() else { return };
    s.artwork = Some(art::Artwork::new(waker.clone()));
    // MPRIS: the desktop's commands arrive on a channel drained in `poll`. A relay thread
    // wakes the UI when one lands, exactly as the worker does, so a media key acts on the
    // next frame even while the app is idle. If the session bus is unavailable we simply run
    // without desktop integration and the relay thread exits when its sender is dropped.
    let (mpris_tx, mpris_raw) = mpsc::channel();
    let (forward_tx, forward_rx) = mpsc::channel();
    let wake = waker.clone();
    thread::spawn(move || {
        while let Ok(command) = mpris_raw.recv() {
            if forward_tx.send(command).is_err() {
                break;
            }
            wake.as_ref()();
        }
    });
    s.mpris = Mpris::start(mpris_tx.clone()).ok();
    s.mpris_rx = Some(forward_rx);
    s.covers = Some(CoverArt::new(waker.clone()));
    // Last.fm scrobbling on its own thread, so its network work never touches a frame.
    s.lastfm = Some(LastFm::start(waker.clone()));
    // Local files: show the cached index at once and refresh it in the background.
    s.waker = Some(waker.clone());
    s.load_local_library();
    // A status-notifier tray, when the session has one. Its transport items ride the same
    // channel as the desktop media keys, so there is one code path for both.
    install_tray(s, waker.clone(), mpris_tx);
    // Desktop notifications are opt-in; a missing session bus just leaves them off.
    if s.settings.notifications {
        s.notifier = Notifier::start().ok();
    }
    thread::spawn(move || worker(cmd_rx, ev_tx, waker));
    // `tsun song.flac` starts that file as a one-off queue, beside the configured library.
    let files = local::cli_audio_files();
    if !files.is_empty() {
        s.play_local_paths(files);
    }
}

/// The tray's menu: transport, then show/quit. The play/pause item follows the real state.
fn tray_menu(playing: bool) -> Vec<TrayItem> {
    vec![
        TrayItem::action("play-pause", if playing { "Pause" } else { "Play" }),
        TrayItem::action("previous", "Previous"),
        TrayItem::action("next", "Next"),
        TrayItem::Separator,
        TrayItem::action("show", "Show tsun"),
        TrayItem::action("quit", "Quit"),
    ]
}

/// Whether the transport is playing right now, for the tray's play/pause label.
fn transport_is_playing(s: &Ui) -> bool {
    s.now_playing.is_some() && !s.paused
}

/// Builds the tray icon: a filled disc with a play triangle cut out, legible on
/// both light and dark panels. Straight (not premultiplied) RGBA.
fn tray_icon() -> TrayIcon {
    const SIZE: u32 = 32;
    let centre = (SIZE as f32 - 1.0) / 2.0;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    // A point inside the triangle with vertices `a`, `b`, `c`.
    let in_triangle = |px: f32, py: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
        let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
            (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
        };
        let d1 = sign((px, py), a, b);
        let d2 = sign((px, py), b, c);
        let d3 = sign((px, py), c, a);
        let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(has_negative && has_positive)
    };
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - centre;
            let dy = y as f32 - centre;
            if (dx * dx + dy * dy).sqrt() > 15.5 {
                continue;
            }
            let index = ((y * SIZE + x) * 4) as usize;
            if in_triangle(dx, dy, (-4.5, -7.0), (-4.5, 7.0), (7.0, 0.0)) {
                // The triangle is cut out (transparent), so the panel shows through.
                continue;
            }
            rgba[index] = 0x3b;
            rgba[index + 1] = 0xd0;
            rgba[index + 2] = 0xd0;
            rgba[index + 3] = 0xff;
        }
    }
    TrayIcon::rgba(SIZE, SIZE, rgba)
}

/// Creates the status-notifier tray, if this session has one and we are not headless.
///
/// Transport picks are sent back as the same commands the desktop media keys use; show and quit
/// go straight to the event loop through the app handle, so the tray works while the window is
/// hidden. A session that cannot host a tray simply runs without one.
fn install_tray(s: &mut Ui, waker: Waker, commands: Sender<MprisCommand>) {
    if rune::is_headless() {
        return;
    }
    let Some(handle) = s.app_handle.clone() else { return };
    let spec = TraySpec::new("tsun", "tsun", tray_icon())
        .tooltip("tsun — TIDAL")
        .menu(tray_menu(transport_is_playing(s)));
    let ok = Tray::new(spec, move |event| match event {
        TrayEvent::Activate { token } => match token {
            Some(token) => handle.activate(token),
            None => handle.open(),
        },
        TrayEvent::Menu(id) => match id.as_str() {
            "play-pause" => {
                let _ = commands.send(MprisCommand::PlayPause);
                waker.as_ref()();
            }
            "previous" => {
                let _ = commands.send(MprisCommand::Previous);
                waker.as_ref()();
            }
            "next" => {
                let _ = commands.send(MprisCommand::Next);
                waker.as_ref()();
            }
            "show" => handle.open(),
            "quit" => handle.quit(),
            _ => {}
        },
        _ => {}
    });
    match ok {
        Ok(tray) => {
            s.tray = Some(tray);
            s.tray_available = true;
            s.tray_playing = Some(transport_is_playing(s));
        }
        Err(_) => {
            s.tray = None;
            s.tray_available = false;
        }
    }
}

/// Runs when the window's close button (or ⇧⌘W / the macOS menu, which reach the same hook)
/// closes it. With "Close to tray" on the window is hidden and the app keeps running, so the
/// tray can bring it back; otherwise the app quits, as a close normally would.
pub(crate) fn on_close(s: &mut Ui, tx: &mut Tx) {
    if !s.settings.close_to_tray {
        tx.quit();
    }
}

pub(crate) fn poll(s: &mut Ui, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = drain_events(s);
    changed |= drain_mpris(s, tx);
    // Last.fm's scrobble thread reports connection and scrobble status back here.
    changed |= s.poll_lastfm();
    // The Discord Rich Presence follows the settings and the field, and a status change (Discord
    // starting or stopping) repaints the card.
    s.sync_discord();
    if let Some(discord) = &s.discord {
        let status = discord.status();
        if s.discord_status.as_ref() != Some(&status) {
            s.discord_status = Some(status);
            changed = true;
        }
    } else if s.discord_status.take().is_some() {
        changed = true;
    }
    // Keep the tray's play/pause label honest without rebuilding the menu every frame.
    let playing = transport_is_playing(s);
    if s.tray.is_some() && s.tray_playing != Some(playing) {
        s.tray_playing = Some(playing);
        if let Some(tray) = &s.tray {
            tray.set_menu(tray_menu(playing));
        }
    }
    update_window_title(s, tx);
    if let Some(artwork) = &mut s.artwork {
        // The profile avatar and the now-playing cover must survive cache eviction.
        let playing = s.now_playing.as_ref().or(s.pending_play.as_ref());
        artwork.set_now_playing(playing.and_then(|track| track.cover.as_deref()));
        artwork.set_profile(s.profile.as_ref().and_then(|profile| profile.picture.as_deref()));
        changed |= artwork.poll();
        // Art the last frame drew: only what a surface shows is fetched, and a cover that
        // arrives for a row nobody draws does not ask for a repaint (see `Artwork::poll`).
        for (kind, id) in std::mem::take(&mut *s.art_wanted.borrow_mut()) {
            // Mix images are full `images.tidal.com` URLs, not the bare UUIDs of every other
            // cover; `request_mix` re-checks the host, so no arbitrary URL can be fetched.
            if id.starts_with("https://") {
                artwork.request_mix(&id);
                continue;
            }
            match kind {
                ArtKind::Cover => artwork.request(&id),
                ArtKind::CoverLarge => artwork.request_cover_sized(&id, 320),
                ArtKind::Picture => artwork.request_artist(&id),
                ArtKind::Video => artwork.request_video(&id),
            }
        }
    } else {
        s.art_wanted.borrow_mut().clear();
    }
    // A collapsed search that lost focus while empty folds back to its icon, so the bar returns
    // to its compact shape as soon as the user clicks away. While the bar is wide enough for the
    // field, the collapsed flag means nothing and is dropped.
    if !s.search_collapsed() {
        s.search_expanded = false;
    } else if s.search_expanded && !s.query.focused && s.query.text.is_empty() {
        s.collapse_search();
        changed = true;
    }
    changed |= s.tick_local();
    if let Some(covers) = &mut s.covers {
        // The full-screen view's big cover and its gradient tint come from the same download;
        // only fetch while that view is actually up.
        if s.now_playing_open {
            let playing = s.now_playing.as_ref().or(s.pending_play.as_ref());
            if let Some(cover) = playing.and_then(|track| track.cover.as_deref()) {
                covers.request(cover);
            }
        }
        changed |= covers.poll();
    }
    changed |= tick_search_suggestions(s, clock, tx);
    if s.video.is_some() {
        // The pixel surface's own wake does not reliably repaint the window (frames only
        // showed up when something else changed), so the poll watches the frame counter and
        // asks for a commit whenever a new picture was shown, and ticks at the display rate
        // while pictures flow. A slow timer covers loading and paused states.
        let mut needs_tick = s.video_pending.is_some();
        let mut playing = false;
        if let Some(session) = &s.video {
            match session.engine.status() {
                video::Status::Loading | video::Status::Paused => needs_tick = true,
                video::Status::Playing => playing = true,
                _ => {}
            }
            let frames = session.engine.frames_shown();
            if s.video_frames != frames {
                s.video_frames = frames;
                changed = true;
            }
            let second = session.engine.position_secs() as u64;
            if s.video_clock != Some(second) {
                s.video_clock = Some(second);
                changed = true;
            }
        }
        if playing {
            tx.wake_after(Duration::from_millis(16));
        } else if needs_tick {
            tx.wake_after(Duration::from_millis(250));
        }
    }
    if s.page == Page::Settings && s.output_devices.is_none() {
        // Real enumeration, once per visit to the page rather than once per frame.
        s.output_devices = Some(crate::audio::output_devices());
    }
    if s.lyrics_open {
        tick_lyrics_scroll(s, tx);
    }
    if changed {
        s.publish_mpris();
    }
    changed
}

/// Keeps the OS window title on `Artist – Title` while a track is playing and on `TIDAL`
/// otherwise. `Tx::set_title` is a request, so it is only sent when the title actually changes;
/// headless `--tree` output exposes it as `"title"`.
fn update_window_title(s: &mut Ui, tx: &mut Tx) {
    let wanted = if let Some(video) = s.video_meta() {
        match (video.artist.is_empty(), video.title.is_empty()) {
            (false, false) => format!("{} – {}", video.artist, video.title),
            (false, true) => video.artist.clone(),
            (true, false) => video.title.clone(),
            (true, true) => "TIDAL".to_owned(),
        }
    } else {
        match s.now_playing.as_ref().or(s.pending_play.as_ref()) {
            Some(track) if !track.title.is_empty() && !track.artist.is_empty() => {
                format!("{} – {}", track.artist, track.title)
            }
            Some(track) if !track.title.is_empty() => track.title.clone(),
            _ => "TIDAL".to_owned(),
        }
    };
    if s.window_title.as_deref() != Some(wanted.as_str()) {
        s.window_title = Some(wanted.clone());
        tx.set_title(wanted);
    }
}

/// How long the search box must stay unchanged before its suggestions are fetched.
const SUGGEST_DEBOUNCE: f64 = 0.25;

/// Fetches search suggestions once the box has settled; a sequence number drops stale answers,
/// and the dropdown only ever shows suggestions the box still matches.
fn tick_search_suggestions(s: &mut Ui, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if s.query.text != s.suggest_seen {
        s.suggest_seen = s.query.text.clone();
        s.suggest_edited_at = Some(clock);
        s.suggest_selected = None;
        s.suggest_dismissed = false;
        tx.wake_after(Duration::from_millis(250));
    }
    let query = s.query.text.trim().to_owned();
    if query.is_empty() && !s.suggestions.is_empty() {
        s.suggestions.clear();
        s.suggest_selected = None;
        changed = true;
    }
    if let Some(edited_at) = s.suggest_edited_at {
        if clock - edited_at >= SUGGEST_DEBOUNCE {
            s.suggest_edited_at = None;
            if s.query.focused && !query.is_empty() && query != s.suggest_query {
                s.suggest_query = query.clone();
                s.suggest_seq += 1;
                let _ = s.cmd_tx.send(Cmd::Suggestions { seq: s.suggest_seq, query });
            }
        }
    }
    changed
}

/// Keeps the current synced lyrics line centred while the lyrics panel is mounted.
fn tick_lyrics_scroll(s: &mut Ui, tx: &mut Tx) {
    let Some(playing) = &s.now_playing else { return };
    let Some(data) = s.lyrics.as_ref().filter(|(id, _)| *id == playing.id).and_then(|(_, data)| data.as_ref())
    else {
        return;
    };
    let Some(line) = crate::ui::lyrics::highlighted_line(data, playing.elapsed.saturating_mul(1000))
    else {
        return;
    };
    if s.lyrics_line != Some(line) {
        s.lyrics_line = Some(line);
        tx.scroll_center_in("lyrics-scroll", format!("lyric-{line}"));
    }
}

/// Drains every worker event delivered since the last frame.
fn drain_events(s: &mut Ui) -> bool {
    let mut changed = false;
    loop {
        match s.ev_rx.try_recv() {
            Ok(ev) => {
                s.apply(ev);
                changed = true;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                if !s.worker_down {
                    s.worker_down = true;
                    s.notice = Some(Notice::error(
                        "The TIDAL worker stopped unexpectedly. Restart the app to reconnect.",
                    ));
                    changed = true;
                }
                break;
            }
        }
    }
    changed
}

/// Drains desktop (MPRIS) commands and applies them to the state machine.
fn drain_mpris(s: &mut Ui, tx: &mut Tx) -> bool {
    let mut changed = false;
    loop {
        let Some(rx) = &s.mpris_rx else { break };
        match rx.try_recv() {
            Ok(command) => {
                s.mpris_command(command, tx);
                changed = true;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                s.mpris_rx = None;
                break;
            }
        }
    }
    changed
}

impl Ui {
    fn apply(&mut self, ev: Ev) {
        match ev {
            Ev::Ready { logged_in } => {
                self.notice = None;
                if logged_in {
                    self.login = Login::LoggedIn;
                    self.history.reset(Route::Home);
                    self.load_home();
                    self.load_favorites();
                    self.open_launch_page();
                } else {
                    self.login = Login::LoggedOut;
                    if self.now_playing.is_some() || self.pending_play.is_some() {
                        self.media_stopped();
                    }
                    self.profile = None;
                    self.profile_menu_open = false;
                    self.menu = None;
                    self.dialog = None;
                    self.search_seq += 1;
                    self.now_playing = None;
                    self.pending_play = None;
                    self.play_seq += 1;
                    self.paused = false;
                    self.pause_pending = false;
                    self.stopping = false;
                    self.results.clear();
                    self.home_tracks.clear();
                    self.recommendations.clear();
                    self.mixes.clear();
                    self.mix = None;
                    self.mix_tracks.clear();
                    self.mix_offset = 0;
                    self.mix_state = SearchState::Idle;
                    self.playlists.clear();
                    self.playlist = None;
                    self.playlist_tracks.clear();
                    self.playlist_state = SearchState::Idle;
                    self.home_state = SearchState::Idle;
                    self.page = Page::Home;
                    self.search_state = SearchState::Idle;
                    self.album = None;
                    self.album_tracks.clear();
                    self.album_state = SearchState::Idle;
                    self.album_extras = None;
                    self.artist = None;
                    self.artist_page = None;
                    self.artist_state = SearchState::Idle;
                    self.artist_top_all = false;
                    self.artist_top_loading = false;
                    self.playlist_move = None;
                    self.library = None;
                    self.library_state = SearchState::Idle;
                    self.collection_tab = CollectionTab::Playlists;
                    self.favorite_tracks.clear();
                    self.favorite_albums.clear();
                    self.favorite_artists.clear();
                    self.favorite_playlists.clear();
                    self.favorite_videos.clear();
                    self.close_video();
                    self.search_all = None;
                    self.search_tab = SearchTab::Top;
                    self.suggest_seq += 1;
                    self.suggest_query.clear();
                    self.suggest_edited_at = None;
                    self.close_suggestions();
                    self.explore.clear();
                    self.explore_state = SearchState::Idle;
                    self.new_releases.clear();
                    self.home_sections.clear();
                    self.carousel_offsets.clear();
                    self.clear_lyrics();
                    self.history.reset(Route::Home);
                }
            }
            Ev::InitFailed(error) => {
                self.login = Login::InitFailed(error);
            }
            Ev::LoginUrl(url) => {
                self.login = Login::DeviceAwaiting { url };
            }
            Ev::PkceUrl(url) => {
                self.pkce_submitting = false;
                self.pkce_redirect.focused = true;
                self.login = Login::PkceAwaiting { url };
            }
            Ev::LoginDone => {
                self.login = Login::LoggedIn;
                self.notice = None;
                self.pkce_submitting = false;
                self.pkce_redirect.focused = false;
                self.page = Page::Home;
                self.history.reset(Route::Home);
                self.load_home();
                self.load_favorites();
                self.open_launch_page();
            }
            Ev::LoginFailed(error) => {
                self.pkce_submitting = false;
                self.login = Login::LoginFailed(error);
            }
            Ev::PkceFailed(error) => {
                self.pkce_submitting = false;
                self.login = Login::LoginFailed(error);
            }
            Ev::SearchAllDone { seq, result } => {
                if seq == self.search_seq {
                    match result {
                        Ok(all) => {
                            // No art is fetched here: the results' cards and rows request the
                            // covers they draw (see `Ui::want_art`), so a search only pays for
                            // what the open tab shows.
                            self.results.set(all.tracks.clone());
                            self.search_all = Some(all);
                            self.search_state = SearchState::Done;
                            self.search_tab = SearchTab::Top;
                            self.selected = None;
                            self.data_changed();
                        }
                        Err(error) => self.search_state = SearchState::Failed(error),
                    }
                }
            }
            Ev::Suggestions { seq, result } => {
                if seq == self.suggest_seq {
                    if let Ok(items) = result {
                        self.suggestions = items;
                        if self.suggest_selected.is_some_and(|index| index >= self.suggestions.len()) {
                            self.suggest_selected = None;
                        }
                    }
                }
            }
            Ev::ExploreResult(result) => match result {
                Ok(sections) => {
                    self.explore = sections;
                    self.explore_state = SearchState::Done;
                }
                Err(error) => self.explore_state = SearchState::Failed(error),
            },
            Ev::NewReleases(result) => match result {
                Ok(sections) => {
                    self.new_releases = sections;
                    if self.explore_state == SearchState::Searching {
                        self.explore_state = SearchState::Done;
                    }
                }
                Err(error) => {
                    if self.explore.is_empty() && self.explore_state == SearchState::Searching {
                        self.explore_state = SearchState::Failed(error);
                    }
                }
            },
            Ev::HomeSections(result) => {
                if let Ok(sections) = result {
                    self.home_sections = sections;
                }
            }
            Ev::Lyrics { track_id, result } => {
                if self.lyrics.as_ref().is_some_and(|(id, _)| *id == track_id) {
                    self.lyrics_line = None;
                    match result {
                        Ok(data) => {
                            self.lyrics = Some((track_id, data));
                            self.lyrics_state = SearchState::Done;
                        }
                        Err(error) => self.lyrics_state = SearchState::Failed(error),
                    }
                }
            }
            Ev::Profile(result) => match result {
                Ok(profile) => {
                    if let (Some(artwork), Some(picture)) = (&mut self.artwork, &profile.picture) {
                        artwork.request_profile(picture);
                    }
                    self.profile = Some(profile);
                }
                Err(error) => self.notice = Some(Notice::error(format!("Could not load profile: {error}"))),
            },
            Ev::HomeResult(result) => match result {
                Ok(tracks) => {
                    self.home_tracks = tracks;
                    self.home_state = SearchState::Done;
                }
                Err(error) => self.home_state = SearchState::Failed(error),
            },
            Ev::HomeRecommendations(result) => {
                match result {
                    Ok(sections) => {
                        self.recommendations = sections;
                    }
                    Err(error) => {
                        self.notice = Some(Notice::error(format!("Could not load TIDAL recommendations: {error}")));
                    }
                }
            }
            Ev::Mixes(result) => match result {
                Ok(mixes) => {
                    self.mixes = mixes;
                    self.mix_offset = 0;
                }
                Err(error) => self.notice = Some(Notice::error(format!("Could not load custom mixes: {error}"))),
            },
            Ev::MixTracks { id, result } => {
                if self.mix.as_ref().is_some_and(|mix| mix.id == id) {
                    match result {
                        Ok(tracks) => {
                            self.mix_tracks.set(tracks);
                            self.mix_state = SearchState::Done;
                            self.data_changed();
                        }
                        Err(error) => self.mix_state = SearchState::Failed(error),
                    }
                }
            }
            Ev::Playlists(result) => match result {
                Ok(playlists) => {
                    self.playlists = playlists;
                }
                Err(error) => self.notice = Some(Notice::error(format!("Could not load playlists: {error}"))),
            },
            Ev::PlaylistTracks { id, result } => {
                if self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
                    match result {
                        Ok(tracks) => {
                            self.playlist_tracks.set(tracks);
                            self.playlist_state = SearchState::Done;
                            self.data_changed();
                        }
                        Err(error) => self.playlist_state = SearchState::Failed(error),
                    }
                }
            }
            Ev::PlaylistMeta { id, result } => {
                if let Ok(meta) = result {
                    self.apply_playlist_meta(meta, &id);
                }
            }
            Ev::PlaylistCreated { result, added } => match result {
                Ok(playlist) => {
                    self.playlists.retain(|existing| existing.id != playlist.id);
                    self.playlists.insert(0, playlist.clone());
                    let title = playlist.title.clone();
                    self.notice = Some(match added {
                        Some(Ok(0)) => Notice::error(format!("Created “{title}”, but TIDAL added no tracks to it")),
                        Some(Ok(count)) => Notice::info(format!(
                            "Created “{title}” and added {count} track{}",
                            if count == 1 { "" } else { "s" }
                        )),
                        Some(Err(error)) => Notice::error(format!("Created “{title}”, but adding its tracks failed: {error}")),
                        None => Notice::info(format!("Created playlist “{title}”")),
                    });
                    self.open_playlist(playlist);
                    let _ = self.cmd_tx.send(Cmd::Playlists);
                }
                Err(error) => {
                    self.notice = Some(Notice::error(format!("Could not create the playlist: {error}")));
                }
            },
            Ev::PlaylistAdded { id, count, result } => {
                let title = self.playlist_title(&id);
                match result {
                    Ok(()) => {
                        self.notice = Some(Notice::info(format!(
                            "Added {count} track{} to “{title}”",
                            if count == 1 { "" } else { "s" }
                        )));
                        let _ = self.cmd_tx.send(Cmd::Playlists);
                        self.reload_open_playlist(&id);
                    }
                    Err(error) => {
                        self.notice = Some(Notice::error(format!("Could not add to “{title}”: {error}")));
                    }
                }
            }
            Ev::PlaylistRemoved { id, index, result } => {
                let title = self.playlist_title(&id);
                match result {
                    Ok(()) => {
                        if self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
                            let index = index as usize;
                            if index < self.playlist_tracks.len() {
                                self.playlist_tracks.edit().remove(index);
                            }
                            if let Some(playlist) = &mut self.playlist {
                                playlist.count = self.playlist_tracks.len() as u64;
                            }
                        }
                        if let Some(entry) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
                            entry.count = entry.count.saturating_sub(1);
                        }
                        self.notice = Some(Notice::info(format!("Removed a track from “{title}”")));
                        let _ = self.cmd_tx.send(Cmd::Playlists);
                    }
                    Err(error) => {
                        self.notice = Some(Notice::error(format!("Could not remove from “{title}”: {error}")));
                    }
                }
            }
            // The move already happened locally, so a failure rolls the order back to where it
            // was and says why; a stale answer for a superseded move is ignored.
            Ev::PlaylistMoved { id, from, to, result } => {
                if self.playlist_move.as_ref() == Some(&(id.clone(), from, to)) {
                    self.playlist_move = None;
                    if let Err(error) = result {
                        let title = self.playlist_title(&id);
                        if self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
                            swap_items(self.playlist_tracks.edit(), to as usize, from as usize);
                        }
                        self.notice = Some(Notice::error(format!(
                            "Could not reorder “{title}”: {error}"
                        )));
                    }
                }
            }
            Ev::PlaylistDeleted { id, result } => {
                let title = self.playlist_title(&id);
                match result {
                    Ok(()) => {
                        self.playlists.retain(|playlist| playlist.id != id);
                        self.favorite_playlists.remove(&id);
                        if self.page == Page::Playlist
                            && self.playlist.as_ref().is_some_and(|playlist| playlist.id == id)
                        {
                            self.playlist = None;
                            self.playlist_tracks.clear();
                            self.playlist_state = SearchState::Idle;
                            self.navigate(Route::Home);
                        }
                        self.notice = Some(Notice::info(format!("Deleted “{title}”")));
                    }
                    Err(error) => {
                        self.notice = Some(Notice::error(format!("Could not delete “{title}”: {error}")));
                    }
                }
            }
            Ev::PlaylistRenamed { id, title, description, result } => {
                match result {
                    Ok(()) => {
                        let description = description.trim().to_owned();
                        let description = (!description.is_empty()).then_some(description);
                        if let Some(entry) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
                            entry.title = title.clone();
                            entry.description = description.clone();
                        }
                        if let Some(playlist) = self.playlist.as_mut().filter(|playlist| playlist.id == id) {
                            playlist.title = title.clone();
                            playlist.description = description;
                        }
                        self.notice = Some(Notice::info(format!("Updated “{title}”")));
                    }
                    Err(error) => {
                        self.notice = Some(Notice::error(format!("Could not update the playlist: {error}")));
                    }
                }
            }
            Ev::TrackRadio(result) => match result {
                Ok(tracks) if !tracks.is_empty() => {
                    self.play_tracks(tracks, 0);
                }
                Ok(_) => {
                    self.notice = Some(Notice::error("TIDAL returned no radio for this track"));
                }
                Err(error) => {
                    self.notice = Some(Notice::error(format!("Could not start the track radio: {error}")));
                }
            },
            Ev::TrackCredits { track_id, result } => {
                if let Some(Dialog::TrackInfo(info)) = &mut self.dialog
                    && info.track.id == track_id
                {
                    match result {
                        Ok(credits) => {
                            info.credits = credits;
                            info.state = SearchState::Done;
                        }
                        Err(_) => info.state = SearchState::Failed(String::new()),
                    }
                }
            }
            Ev::Library(result) => match result {
                Ok(library) => {
                    // The pages' cards and rows request the covers they draw, so opening the
                    // Collection fetches only the art of the rows near the viewport.
                    self.library = Some(library);
                    self.library_state = SearchState::Done;
                    self.data_changed();
                }
                Err(error) => {
                    self.library_state = SearchState::Failed(error);
                }
            },
            Ev::Favorites(result) => match result {
                Ok(ids) => {
                    self.favorite_tracks = ids.tracks;
                    self.favorite_albums = ids.albums;
                    self.favorite_artists = ids.artists;
                    self.favorite_playlists = ids.playlists;
                    self.favorite_videos = ids.videos;
                }
                Err(error) => {
                    self.notice = Some(Notice::error(format!("Could not load favorites: {error}")));
                }
            },
            Ev::FavoriteChanged { kind, id, on, error } => {
                if let Some(error) = error {
                    self.set_favorite_local(kind, &id, !on);
                    self.notice = Some(Notice::error(format!("Could not update favorites: {error}")));
                }
            }
            Ev::Album { id, result } => {
                if self.album.as_ref().is_some_and(|album| album.id == id) {
                    match result {
                        Ok((album, tracks)) => {
                            self.album = Some(album);
                            self.album_tracks.set(tracks);
                            self.album_state = SearchState::Done;
                            self.data_changed();
                        }
                        Err(error) => self.album_state = SearchState::Failed(error),
                    }
                }
            }
            Ev::AlbumExtras { id, result } => {
                if self.album.as_ref().is_some_and(|album| album.id == id) {
                    match result {
                        Ok(extras) => {
                            self.album_extras = Some(extras);
                        }
                        Err(_) => self.album_extras = Some(AlbumExtras::default()),
                    }
                }
            }
            // "Show all" answered: append the page, and stop offering the button once TIDAL has
            // nothing further (an empty page, or the total reached).
            Ev::ArtistTopTracks { id, result } => {
                if self.artist.as_ref().is_some_and(|artist| artist.id == id) {
                    self.artist_top_loading = false;
                    match result {
                        Ok((tracks, total)) => {
                            if let Some(page) = &mut self.artist_page {
                                for track in tracks {
                                    if !page.top_tracks.iter().any(|existing| existing.id == track.id) {
                                        page.top_tracks.push(track);
                                    }
                                }
                                if let Some(total) = total {
                                    page.top_tracks_total = Some(total);
                                }
                            }
                            // Everything fetched becomes visible; the button only returns while
                            // TIDAL still reports more tracks than we hold.
                            self.artist_top_all = true;
                            self.data_changed();
                        }
                        Err(error) => {
                            self.notice = Some(Notice::error(format!("Could not load the top tracks: {error}")));
                        }
                    }
                }
            }
            Ev::VideoReady { seq, id, result, volume } => {
                if seq == self.video_seq {
                    match result {
                        Ok(stream) => {
                            if let Some(video) = self.video_pending.take().filter(|video| video.id == id) {
                                let duration_ms = video.duration.unwrap_or(0) * 1000;
                                let device = self.settings.output_device.clone();
                                let engine = video::Engine::start(
                                    stream.master_url.clone(),
                                    volume.clone(),
                                    device.clone(),
                                    duration_ms,
                                    0.0,
                                );
                                self.video = Some(VideoSession {
                                    video,
                                    engine,
                                    master_url: stream.master_url,
                                    volume,
                                    device,
                                    duration_ms,
                                });
                                self.video_clock = None;
                                self.publish_mpris();
                            }
                        }
                        Err(error) => {
                            self.video_pending = None;
                            self.video = None;
                            self.notice = Some(Notice::error(format!("Could not play the video: {error}")));
                        }
                    }
                }
            }
            Ev::Artist { id, result } => {
                if self.artist.as_ref().is_some_and(|artist| artist.id == id) {
                    match result {
                        Ok(page) => {
                            self.artist = Some(page.artist.clone());
                            self.artist_page = Some(page);
                            self.artist_state = SearchState::Done;
                            self.data_changed();
                        }
                        Err(error) => self.artist_state = SearchState::Failed(error),
                    }
                }
            }
            Ev::Radio { id, result } => {
                if self.artist.as_ref().is_some_and(|artist| artist.id == id) {
                    match result {
                        Ok(tracks) => {
                            if !tracks.is_empty() {
                                self.play_tracks(tracks, 0);
                            }
                        }
                        Err(error) => {
                            self.notice = Some(Notice::error(format!("Could not start the radio: {error}")));
                        }
                    }
                }
            }
            Ev::SearchFailed { seq, error } => {
                if seq == self.search_seq {
                    self.search_state = SearchState::Failed(error);
                }
            }
            Ev::Playing { seq, id, quality, output } => {
                if seq == self.play_seq && !self.stopping {
                    if let Some(mut playing) = self.pending_play.take() {
                        if playing.id == id {
                            playing.quality = Some(quality);
                            playing.output = output;
                            self.now_playing = Some(playing);
                            self.load_lyrics(id);
                            self.notify_now_playing();
                            self.media_started(0.0);
                        }
                    }
                }
            }
            Ev::PauseChanged { seq, paused, error } => {
                if seq == self.play_seq {
                    self.pause_pending = false;
                    if let Some(error) = error {
                        self.notice = Some(Notice::error(format!("Playback failed: {error}")));
                    } else {
                        self.paused = paused;
                        let position = self.current_elapsed() as f64;
                        self.media_pause_changed(paused, position);
                    }
                }
            }
            Ev::PlaybackProgress { seq, elapsed } => {
                if seq == self.play_seq {
                    let mut remaining = None;
                    if let Some(playing) = &mut self.now_playing {
                        playing.elapsed = elapsed;
                        remaining = playing
                            .duration
                            .map(|duration| duration.saturating_sub(elapsed));
                    }
                    self.record_play_if_listened(elapsed, false);
                    self.tick_preload(remaining);
                }
            }
            Ev::TrackFinished { seq } => {
                if seq == self.play_seq && !self.stopping {
                    let elapsed = self.current_elapsed();
                    self.record_play_if_listened(elapsed, true);
                    self.media_finished();
                    self.advance_queue();
                }
            }
            Ev::GaplessAdvanced { seq, id, output } => {
                if seq == self.play_seq && !self.stopping {
                    self.preload.switched(id);
                    self.advance_to_id(id, output);
                }
            }
            Ev::PlayFailed { seq, error } => {
                if seq == self.play_seq {
                    self.pending_play = None;
                    self.stopping = false;
                    // The track that was playing has stopped being replaced; close its play out
                    // so its real listening time is what counts.
                    self.media_stopped();
                    self.notice = Some(Notice::error(format!("Playback failed: {error}")));
                }
            }
            Ev::PlaybackEnded { seq, error } => {
                if seq == self.play_seq {
                    self.media_stopped();
                    self.now_playing = None;
                    self.pending_play = None;
                    self.paused = false;
                    self.pause_pending = false;
                    self.stopping = false;
                    self.preload = GaplessState::new();
                    self.now_playing_open = false;
                    self.clear_lyrics();
                    if let Some(error) = error {
                        self.notice = Some(Notice::error(format!("Playback failed: {error}")));
                    }
                }
            }
            Ev::Stopped => {
                self.media_stopped();
                self.pending_play = None;
                self.now_playing = None;
                self.paused = false;
                self.pause_pending = false;
                self.stopping = false;
                self.preload = GaplessState::new();
                self.now_playing_open = false;
                self.clear_lyrics();
            }
        }
    }

    pub(crate) fn load_home(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Profile);
        self.home_state = SearchState::Searching;
        if self.cmd_tx.send(Cmd::Home).is_err() {
            self.home_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
        let _ = self.cmd_tx.send(Cmd::Mixes);
        let _ = self.cmd_tx.send(Cmd::Playlists);
        let _ = self.cmd_tx.send(Cmd::HomeSections);
        let _ = self.cmd_tx.send(Cmd::NewReleases);
    }

    pub(crate) fn open_mix(&mut self, mix: Mix) {
        let id = mix.id.clone();
        self.mix = Some(mix);
        self.navigate(Route::Mix(id));
    }

    pub(crate) fn open_playlist(&mut self, playlist: Playlist) {
        let id = playlist.id.clone();
        self.playlist = Some(playlist);
        self.navigate(Route::Playlist(id));
    }

    /// Opens an album, seeding the header from an [`Album`] the caller already has.
    pub(crate) fn open_album_with(&mut self, album: Album) {
        let id = album.id;
        if !self.album.as_ref().is_some_and(|open| open.id == id) {
            // Another album's header starts collapsed: the review's toggle is per album.
            self.album_review_open = None;
        }
        self.album = Some(album);
        self.navigate(Route::Album(id));
    }

    /// Opens an album by id alone; the header fills in as the page loads.
    pub(crate) fn open_album(&mut self, id: u64) {
        self.navigate(Route::Album(id));
    }

    /// Opens an artist, seeding the header from an [`Artist`] the caller already has.
    pub(crate) fn open_artist_with(&mut self, artist: Artist) {
        let id = artist.id;
        self.artist = Some(artist);
        self.navigate(Route::Artist(id));
    }

    /// Opens an artist by id alone; the header fills in as the page loads.
    pub(crate) fn open_artist(&mut self, id: u64) {
        self.navigate(Route::Artist(id));
    }

    /// Selects one of the Collection tabs.
    pub(crate) fn set_collection_tab(&mut self, tab: CollectionTab) {
        self.collection_tab = tab;
        self.collection_sort_open = false;
        // A sort that does not apply to the new tab falls back to the default.
        if !self.collection_sort.applies_to(tab) {
            self.collection_sort = SortKey::default();
        }
    }

    /// Sets the Collection sort order and closes the dropdown.
    pub(crate) fn set_collection_sort(&mut self, sort: SortKey) {
        self.collection_sort = sort;
        self.collection_sort_open = false;
    }

    /// Clears the Collection filter box.
    pub(crate) fn clear_collection_filter(&mut self) {
        self.collection_filter.clear();
    }

    // -----------------------------------------------------------------------------------------
    // Local files

    /// A local track by id, from the scanned library or the one-off list.
    pub(crate) fn local_track(&self, id: u64) -> Option<&local::LocalTrack> {
        self.local.track(id).or_else(|| self.local_extra.get(&id))
    }

    /// The configured music folders as paths.
    pub(crate) fn local_folders(&self) -> Vec<PathBuf> {
        self.settings.local_folders.iter().map(PathBuf::from).collect()
    }

    /// Notes that a loaded list changed (the library, the local index, the history, the search
    /// results, an artist page), so the pages' derived views rebuild on the next commit and only
    /// then. A commit that changes nothing but the clock rebuilds nothing.
    fn data_changed(&mut self) {
        self.data_rev = self.data_rev.wrapping_add(1);
    }

    /// Populates the local index from the on-disk cache and asks the scanner to refresh it.
    /// Called once in `setup`: the cache makes a restart instant, the scan catches changes.
    fn load_local_library(&mut self) {
        let roots = self.local_folders();
        self.local = local::load_index(roots);
        self.data_changed();
        self.start_local_scan();
    }

    /// Starts (or restarts) the background scan of the configured folders.
    pub(crate) fn start_local_scan(&mut self) {
        let roots = self.local_folders();
        self.local_skipped = 0;
        if roots.is_empty() {
            self.local_scan = None;
            self.local_progress = None;
            self.local_state = SearchState::Idle;
            self.local = local::Library::default();
            self.data_changed();
            return;
        }
        let Some(waker) = self.waker.clone() else { return };
        self.local_state = SearchState::Searching;
        self.local_progress = Some((0, 0));
        self.local_scan = Some(local::start_scan(roots, waker));
    }

    /// Drains the scanner. Returns true when the page should redraw.
    fn tick_local(&mut self) -> bool {
        let mut changed = false;
        loop {
            let message = self.local_scan.as_ref().and_then(|rx| rx.try_recv().ok());
            match message {
                Some(local::ScanMsg::Progress { done, total }) => {
                    self.local_progress = Some((done, total));
                    // The count is only drawn by the Settings card: repainting the window for
                    // every batch of scanned files would run the view for a frame that changed
                    // nothing the user can see.
                    changed |= self.page == Page::Settings;
                }
                Some(local::ScanMsg::Finished { library, skipped }) => {
                    self.local = *library;
                    self.local_skipped = skipped;
                    self.local_state = SearchState::Done;
                    self.local_scan = None;
                    self.local_progress = None;
                    // The open album/artist may have vanished with a re-scan.
                    if self.local_album.as_ref().is_some_and(|key| self.local.album(key).is_none()) {
                        self.local_album = None;
                        if self.page == Page::LocalAlbum {
                            self.page = Page::Local;
                        }
                    }
                    if self.local_artist.as_ref().is_some_and(|name| self.local.artist(name).is_none()) {
                        self.local_artist = None;
                        if self.page == Page::LocalArtist {
                            self.page = Page::Local;
                        }
                    }
                    self.data_changed();
                    changed = true;
                }
                None => break,
            }
        }
        changed
    }

    /// Adds `path` as a music folder, if it is an existing directory, then rescans.
    pub(crate) fn add_local_folder_path(&mut self, path: PathBuf) {
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        if !canonical.is_dir() {
            self.notice = Some(Notice::error(format!(
                "{} is not a folder.",
                canonical.display()
            )));
            return;
        }
        let text = canonical.to_string_lossy().into_owned();
        if self.settings.local_folders.iter().any(|folder| folder == &text) {
            self.notice = Some(Notice::info("That folder is already in the local library."));
            return;
        }
        self.settings.local_folders.push(text);
        self.settings.save();
        self.local_add_path.clear();
        self.notice = None;
        self.start_local_scan();
    }

    /// Adds the path typed into the Settings card's field.
    pub(crate) fn add_local_folder(&mut self) {
        let text = self.local_add_path.text.trim().to_owned();
        if !local::is_folder(&text) {
            self.notice = Some(Notice::error(format!("{text} is not an existing folder.")));
            return;
        }
        self.add_local_folder_path(PathBuf::from(text));
    }

    /// Removes `folder` from the library sources and rescans.
    pub(crate) fn remove_local_folder(&mut self, folder: &str) {
        self.settings.local_folders.retain(|entry| entry != folder);
        self.settings.save();
        let removed = PathBuf::from(folder);
        if self.local_folder.as_ref().is_some_and(|open| open.starts_with(&removed)) {
            self.local_folder = None;
        }
        self.start_local_scan();
    }

    /// Selects a Local files tab.
    pub(crate) fn set_local_tab(&mut self, tab: local::LocalTab) {
        self.local_tab = tab;
    }

    /// Cycles the Local files sort order.
    pub(crate) fn cycle_local_sort(&mut self) {
        self.local_sort = self.local_sort.next();
    }

    pub(crate) fn clear_local_filter(&mut self) {
        self.local_filter.clear();
    }

    /// Opens a local album's page.
    pub(crate) fn open_local_album(&mut self, key: String) {
        self.local_album = Some(key.clone());
        self.local_artist = None;
        self.navigate(Route::LocalAlbum(key));
    }

    /// Opens a local artist's page.
    pub(crate) fn open_local_artist(&mut self, name: String) {
        self.local_artist = Some(name.clone());
        self.local_album = None;
        self.navigate(Route::LocalArtist(name));
    }

    /// The Local files page itself.
    pub(crate) fn open_local(&mut self) {
        self.navigate(Route::Local);
    }

    /// Opens a folder in the Folders tab.
    pub(crate) fn open_local_folder(&mut self, folder: PathBuf) {
        self.local_folder = Some(folder);
    }

    /// Steps the Folders tab to the parent folder, stopping at the configured sources.
    pub(crate) fn local_folder_up(&mut self) {
        let Some(current) = self.local_folder.clone() else { return };
        let roots = self.local_folders();
        if roots.iter().any(|root| root == &current) {
            self.local_folder = None;
            return;
        }
        match current.parent() {
            Some(parent) if roots.iter().any(|root| current.starts_with(root)) => {
                self.local_folder = Some(parent.to_path_buf());
            }
            _ => self.local_folder = None,
        }
    }

    pub(crate) fn play_local_album(&mut self, key: &str) {
        let tracks = self.local.album_tracks(key);
        if !tracks.is_empty() {
            self.play_tracks(tracks, 0);
        }
    }

    pub(crate) fn shuffle_local_album(&mut self, key: &str) {
        let tracks = self.local.album_tracks(key);
        if !tracks.is_empty() {
            self.play_tracks_shuffled(tracks);
        }
    }

    pub(crate) fn play_local_artist(&mut self, name: &str) {
        let tracks = self.local.artist_tracks(name);
        if !tracks.is_empty() {
            self.play_tracks(tracks, 0);
        }
    }

    pub(crate) fn shuffle_local_artist(&mut self, name: &str) {
        let tracks = self.local.artist_tracks(name);
        if !tracks.is_empty() {
            self.play_tracks_shuffled(tracks);
        }
    }

    /// Plays the given local track indices as a queue.
    pub(crate) fn play_local_indices(&mut self, indices: &[usize]) {
        let tracks = self.local.track_list(indices);
        if !tracks.is_empty() {
            self.play_tracks(tracks, 0);
        }
    }

    pub(crate) fn shuffle_local_indices(&mut self, indices: &[usize]) {
        let tracks = self.local.track_list(indices);
        if !tracks.is_empty() {
            self.play_tracks_shuffled(tracks);
        }
    }

    /// Opens a dropped or command-line list of files as a one-off queue.
    pub(crate) fn play_local_paths(&mut self, paths: Vec<PathBuf>) {
        let mut tracks: Vec<Track> = Vec::new();
        for path in paths {
            if let Some(track) = local::describe(&path) {
                self.local_extra.insert(track.id, track.clone());
                tracks.push(track.track());
            }
        }
        self.data_changed();
        if tracks.is_empty() {
            self.notice = Some(Notice::error("No playable audio files were given."));
            return;
        }
        self.play_tracks(tracks, 0);
    }

    /// Reveals a local file in the desktop's file manager.
    pub(crate) fn show_local_in_file_manager(&mut self, id: u64) {
        if let Some(track) = self.local_track(id) {
            local::show_in_file_manager(track.path.clone());
        }
    }

    /// Re-fetches the data of the page the user is on (F5 / Ctrl+R).
    pub(crate) fn refresh_page(&mut self) {
        match self.page {
            Page::Home => {
                self.home_state = SearchState::Idle;
                self.load_home();
            }
            Page::Search => self.submit_search(),
            Page::Explore => self.load_explore(),
            Page::Collection => self.load_library(),
            Page::Mixes => {
                let _ = self.cmd_tx.send(Cmd::Mixes);
            }
            Page::Playlist => {
                if let Some(id) = self.playlist.as_ref().map(|playlist| playlist.id.clone()) {
                    self.load_playlist(&id);
                }
            }
            Page::Mix => {
                if let Some(id) = self.mix.as_ref().map(|mix| mix.id.clone()) {
                    self.load_mix(&id);
                }
            }
            Page::Album => {
                if let Some(id) = self.album.as_ref().map(|album| album.id) {
                    self.load_album(id);
                }
            }
            Page::Artist => {
                if let Some(id) = self.artist.as_ref().map(|artist| artist.id) {
                    self.load_artist(id);
                }
            }
            Page::Local | Page::LocalAlbum | Page::LocalArtist => {
                self.start_local_scan();
            }
            Page::Settings => {}
        }
    }

    /// Records `route` in the history and shows it. Re-opening the route the
    /// user is already on does nothing.
    pub(crate) fn navigate(&mut self, route: Route) {
        if self.history.push(route.clone()) {
            self.show(route);
        }
    }

    /// The Back button: steps to the previous route, if there is one.
    pub(crate) fn go_back(&mut self) {
        if let Some(route) = self.history.back() {
            self.show(route);
        }
    }

    /// The Forward button: steps to the next route, if there is one.
    pub(crate) fn go_forward(&mut self) {
        if let Some(route) = self.history.forward() {
            self.show(route);
        }
    }

    /// Shows `route` without touching the history. Restoring a page reloads its
    /// data, keeping whatever the caller already had for its header.
    fn show(&mut self, route: Route) {
        self.page = route.page();
        self.remember_route(&route);
        match route {
            Route::Home => {
                if matches!(self.home_state, SearchState::Idle) {
                    self.load_home();
                }
            }
            Route::Search => {}
            Route::Explore => self.load_explore(),
            Route::Collection => self.load_library(),
            Route::Album(id) => self.load_album(id),
            Route::Artist(id) => self.load_artist(id),
            Route::Playlist(id) => self.load_playlist(&id),
            Route::Mix(id) => self.load_mix(&id),
            Route::Local => {}
            Route::LocalAlbum(key) => self.local_album = Some(key),
            Route::LocalArtist(name) => self.local_artist = Some(name),
        }
    }

    fn load_album(&mut self, id: u64) {
        if !self.album.as_ref().is_some_and(|album| album.id == id) {
            self.album = Some(Album { id, ..Album::default() });
            // Another album's header starts collapsed: the toggle is per album.
            self.album_review_open = None;
        }
        self.album_tracks.clear();
        self.album_extras = None;
        self.album_state = SearchState::Searching;
        if self.cmd_tx.send(Cmd::OpenAlbum(id)).is_err() {
            self.album_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        } else {
            // The review, credits and related albums load beside the album, so a missing one
            // never holds up the track list.
            let _ = self.cmd_tx.send(Cmd::AlbumExtras(id));
        }
    }

    fn load_artist(&mut self, id: u64) {
        if !self.artist.as_ref().is_some_and(|artist| artist.id == id) {
            self.artist = Some(Artist { id, ..Artist::default() });
        }
        self.artist_page = None;
        self.artist_top_all = false;
        self.artist_top_loading = false;
        self.artist_state = SearchState::Searching;
        if self.cmd_tx.send(Cmd::OpenArtist(id)).is_err() {
            self.artist_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    /// Reopens a playlist from its uuid, taking the metadata from whatever list
    /// already holds it (the sidebar, the collection, or the page itself).
    fn load_playlist(&mut self, id: &str) {
        if !self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
            self.playlist = Some(self.playlist_meta(id));
        }
        self.playlist_tracks.clear();
        self.playlist_state = SearchState::Searching;
        // The page's own metadata request settles description and ownership even when the
        // playlist was reached from a list that carried only a title.
        let _ = self.cmd_tx.send(Cmd::PlaylistMeta(id.to_owned()));
        if self.cmd_tx.send(Cmd::OpenPlaylist(id.to_owned())).is_err() {
            self.playlist_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    fn playlist_meta(&self, id: &str) -> Playlist {
        self.playlists
            .iter()
            .chain(self.library.iter().flat_map(|library| &library.playlists))
            .find(|playlist| playlist.id == id)
            .cloned()
            .unwrap_or(Playlist { id: id.to_owned(), ..Playlist::default() })
    }

    fn load_mix(&mut self, id: &str) {
        if !self.mix.as_ref().is_some_and(|mix| mix.id == id) {
            self.mix = Some(self.mix_meta(id));
        }
        self.mix_tracks.clear();
        self.mix_state = SearchState::Searching;
        if self.cmd_tx.send(Cmd::OpenMix(id.to_owned())).is_err() {
            self.mix_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    fn mix_meta(&self, id: &str) -> Mix {
        self.mixes
            .iter()
            .find(|mix| mix.id == id)
            .cloned()
            .unwrap_or(Mix { id: id.to_owned(), ..Mix::default() })
    }

    /// Loads the user's whole TIDAL collection for the Collection page. The mix
    /// gallery comes along for the Mixes tab.
    pub(crate) fn load_library(&mut self) {
        self.library_state = SearchState::Searching;
        let _ = self.cmd_tx.send(Cmd::Mixes);
        if self.cmd_tx.send(Cmd::Library).is_err() {
            self.library_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    /// Asks for the favourite ids once after login, so hearts start correct.
    pub(crate) fn load_favorites(&mut self) {
        let _ = self.cmd_tx.send(Cmd::FavoriteIds);
    }

    /// Whether `kind`/`id` is currently a favourite.
    pub(crate) fn is_favorite(&self, kind: FavKind, id: &str) -> bool {
        if kind == FavKind::Track && id.parse::<u64>().is_ok_and(local::is_local_id) {
            return false;
        }
        match kind {
            FavKind::Track => id.parse::<u64>().is_ok_and(|id| self.favorite_tracks.contains(&id)),
            FavKind::Album => id.parse::<u64>().is_ok_and(|id| self.favorite_albums.contains(&id)),
            FavKind::Artist => id.parse::<u64>().is_ok_and(|id| self.favorite_artists.contains(&id)),
            FavKind::Playlist => self.favorite_playlists.contains(id),
            FavKind::Video => id.parse::<u64>().is_ok_and(|id| self.favorite_videos.contains(&id)),
        }
    }

    fn set_favorite_local(&mut self, kind: FavKind, id: &str, on: bool) {
        match kind {
            FavKind::Track => set_id(&mut self.favorite_tracks, id, on),
            FavKind::Album => set_id(&mut self.favorite_albums, id, on),
            FavKind::Artist => set_id(&mut self.favorite_artists, id, on),
            FavKind::Video => set_id(&mut self.favorite_videos, id, on),
            FavKind::Playlist => {
                if on {
                    self.favorite_playlists.insert(id.to_owned());
                } else {
                    self.favorite_playlists.remove(id);
                }
            }
        }
    }

    /// Flips the favourite for `kind`/`id` immediately and tells the worker, so
    /// the heart responds on the next frame. A failure rolls it back and leaves
    /// a notice, as the official client does.
    pub(crate) fn toggle_favorite(&mut self, kind: FavKind, id: &str) {
        // Local files never touch TIDAL favourites (a local id must not be confused with, or
        // collide with, a TIDAL track id).
        if kind == FavKind::Track && id.parse::<u64>().is_ok_and(local::is_local_id) {
            return;
        }
        let on = !self.is_favorite(kind, id);
        self.set_favorite_local(kind, id, on);
        if self.cmd_tx.send(Cmd::SetFavorite { kind, id: id.to_owned(), on }).is_err() {
            self.set_favorite_local(kind, id, !on);
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
        // Mirror the heart on Last.fm. Only TIDAL tracks get here: a local id returned above.
        if kind == FavKind::Track
            && let Ok(track_id) = id.parse::<u64>()
        {
            self.lastfm_love(track_id, on);
        }
    }

    /// Starts TIDAL's artist radio; the tracks arrive as `Ev::Radio` and play.
    pub(crate) fn play_artist_radio(&mut self, id: u64) {
        if self.cmd_tx.send(Cmd::ArtistRadio(id)).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    pub(crate) fn start_login(&mut self) {
        self.notice = None;
        self.pkce_submitting = false;
        self.pkce_redirect.focused = false;
        self.login = Login::DeviceStarting;
        if self.cmd_tx.send(Cmd::Login).is_err() {
            self.login = Login::InitFailed("The TIDAL worker is not running.".to_owned());
        }
    }

    pub(crate) fn start_pkce(&mut self) {
        self.notice = None;
        self.pkce_submitting = false;
        self.pkce_redirect.clear();
        self.pkce_redirect.focused = false;
        self.login = Login::PkceStarting;
        if self.cmd_tx.send(Cmd::StartPkce).is_err() {
            self.login = Login::InitFailed("The TIDAL worker is not running.".to_owned());
        }
    }

    pub(crate) fn finish_pkce(&mut self) {
        let url = self.pkce_redirect.text.trim().to_owned();
        if url.is_empty() {
            self.notice = Some(Notice::info("Paste the redirected URL first."));
            return;
        }
        self.pkce_submitting = true;
        if self.cmd_tx.send(Cmd::FinishPkce { url }).is_err() {
            self.pkce_submitting = false;
            self.login = Login::InitFailed("The TIDAL worker is not running.".to_owned());
        }
    }

    pub(crate) fn retry_init(&mut self) {
        self.notice = None;
        self.login = Login::Checking;
        if self.cmd_tx.send(Cmd::Init).is_err() {
            self.login = Login::InitFailed("The TIDAL worker is not running.".to_owned());
        }
    }

    pub(crate) fn submit_search(&mut self) {
        if !matches!(&self.login, Login::LoggedIn) {
            return;
        }
        let query = self.query.text.trim().to_owned();
        if query.is_empty() {
            self.notice = Some(Notice::info("Type something to search for."));
            return;
        }
        let _ = recent::remember(&mut self.recent_searches, &query);
        self.navigate(Route::Search);
        self.search_seq += 1;
        // Drop any suggestion answer still in flight: the results page replaces the dropdown.
        self.suggest_seq += 1;
        self.last_query = query.clone();
        self.search_state = SearchState::Searching;
        self.search_all = None;
        self.search_tab = SearchTab::Top;
        self.results.clear();
        self.selected = None;
        self.close_suggestions();
        if self.cmd_tx.send(Cmd::SearchAll { seq: self.search_seq, query }).is_err() {
            self.search_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    pub(crate) fn clear_search(&mut self) {
        self.search_seq += 1;
        self.suggest_seq += 1;
        self.query.clear();
        self.query.focused = true;
        self.last_query.clear();
        self.results.clear();
        self.selected = None;
        self.search_all = None;
        self.close_suggestions();
        self.search_state = SearchState::Idle;
    }

    /// The suggestions the dropdown shows: recent searches while the box is empty, TIDAL's
    /// suggestions otherwise.
    pub(crate) fn search_suggest_list(&self) -> &[String] {
        if self.query.text.trim().is_empty() {
            &self.recent_searches
        } else {
            &self.suggestions
        }
    }

    /// Whether the suggestion dropdown belongs under the search box.
    pub(crate) fn search_dropdown_open(&self) -> bool {
        self.query.focused && !self.suggest_dismissed && !self.search_suggest_list().is_empty()
    }

    /// Clears the suggestion list (a search ran, or the box was emptied).
    pub(crate) fn close_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggest_selected = None;
    }

    /// Hides the dropdown until the box is typed in again.
    pub(crate) fn dismiss_suggestions(&mut self) {
        self.suggest_dismissed = true;
        self.close_suggestions();
    }

    /// Takes a suggestion: fills the box, runs the search, and closes the dropdown.
    pub(crate) fn accept_suggestion(&mut self, text: String) {
        self.query.replace_text(text);
        self.suggest_seen = self.query.text.clone();
        self.suggest_edited_at = None;
        self.suggest_dismissed = true;
        self.close_suggestions();
        self.submit_search();
    }

    /// Forgets the recent-search list.
    pub(crate) fn clear_recent_searches(&mut self) {
        recent::clear(&mut self.recent_searches);
        self.suggest_selected = None;
    }

    /// Opens Explore and (re)loads its feed.
    pub(crate) fn open_explore(&mut self) {
        self.page = Page::Explore;
        self.history.push(Route::Explore);
        self.load_explore();
    }

    /// Whether the top bar has collapsed the search field into its icon button, so the search is
    /// only up while [`Ui::search_expanded`] is set.
    pub(crate) fn search_collapsed(&self) -> bool {
        self.bar_mode.get().is_collapsed()
    }

    /// Expands the collapsed search icon into the field and focuses it (the top bar's icon
    /// button). A no-op unless the bar is actually collapsed.
    pub(crate) fn expand_search(&mut self) {
        self.search_expanded = true;
        self.suggest_dismissed = false;
        self.query.focused = true;
    }

    /// Puts the collapsed search field back to its icon, clearing focus. The query is kept, so
    /// reopening restores it.
    pub(crate) fn collapse_search(&mut self) {
        self.search_expanded = false;
        self.query.focused = false;
        self.suggest_dismissed = true;
        self.close_suggestions();
    }

    fn load_explore(&mut self) {
        self.explore_state = SearchState::Searching;
        let _ = self.cmd_tx.send(Cmd::Explore);
        if self.cmd_tx.send(Cmd::NewReleases).is_err() {
            self.explore_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    /// Loads the lyrics of `track_id`, unless they are already loaded or loading. Local files
    /// have no TIDAL lyrics endpoint, so they clear the panel instead of asking.
    fn load_lyrics(&mut self, track_id: u64) {
        if local::is_local_id(track_id) {
            self.clear_lyrics();
            return;
        }
        if self.lyrics.as_ref().is_some_and(|(id, _)| *id == track_id) {
            return;
        }
        self.lyrics = Some((track_id, None));
        self.lyrics_state = SearchState::Searching;
        self.lyrics_line = None;
        if self.cmd_tx.send(Cmd::Lyrics { track_id }).is_err() {
            self.lyrics_state = SearchState::Failed("The TIDAL worker is not running.".to_owned());
        }
    }

    fn clear_lyrics(&mut self) {
        self.lyrics = None;
        self.lyrics_state = SearchState::Idle;
        self.lyrics_line = None;
    }

    /// Replaces the play queue with `tracks` and starts `tracks[start]`.
    /// Every page that lists tracks should start playback through this so
    /// next/previous and auto-advance follow the list the user clicked in.
    pub(crate) fn play_tracks(&mut self, tracks: Vec<Track>, start: usize) {
        let Some(track) = tracks.get(start).cloned() else { return };
        self.queue.set(tracks, start);
        self.start_track(track);
    }

    /// Replaces the play queue with `tracks`, turns shuffle mode on, and starts the first
    /// track. The clicked track stays first and the rest are shuffled behind it, exactly as
    /// the transport's shuffle button does when it is pressed on a fresh queue. The player
    /// bar reflects the mode because it reads [`Queue::shuffle`].
    pub(crate) fn play_tracks_shuffled(&mut self, tracks: Vec<Track>) {
        let Some(track) = tracks.first().cloned() else { return };
        if !self.queue.shuffle() {
            self.queue.set_shuffle(true);
        }
        self.queue.set(tracks, 0);
        self.settings.shuffle = true;
        self.settings.save();
        self.start_track(track);
    }

    /// Starts `id` from whichever page list holds it, so it becomes the queue. A local id that
    /// is not in any list (a history row, say) plays as a one-track queue from the library.
    pub(crate) fn play_id(&mut self, id: u64) {
        if local::is_local_id(id) {
            if let Some(track) = self.local_track(id).map(local::LocalTrack::track) {
                self.play_tracks(vec![track], 0);
            } else {
                self.notice = Some(Notice::error("That local file is no longer in the library."));
            }
            return;
        }
        if let Some((tracks, index)) = self.list_containing(id) {
            self.play_tracks(tracks, index);
        }
    }

    /// The page list (and index) that holds `id`, preferring the list the user is looking at so
    /// `play_id` starts a queue in the page it was clicked in.
    fn list_containing(&self, id: u64) -> Option<(Vec<Track>, usize)> {
        let preferred: Option<&[Track]> = match self.page {
            Page::Search => Some(self.results.items()),
            Page::Playlist => Some(self.playlist_tracks.items()),
            Page::Mix => Some(self.mix_tracks.items()),
            Page::Album => Some(self.album_tracks.items()),
            Page::Artist => self.artist_page.as_ref().map(|page| page.top_tracks.as_slice()),
            Page::Collection => self.library.as_ref().map(|library| library.tracks.as_slice()),
            _ => None,
        };
        let mut candidates: Vec<&[Track]> = Vec::new();
        if let Some(list) = preferred {
            candidates.push(list);
        }
        candidates.extend([
            self.results.items(),
            self.home_tracks.as_slice(),
            self.playlist_tracks.items(),
            self.mix_tracks.items(),
            self.album_tracks.items(),
        ]);
        if let Some(page) = &self.artist_page {
            candidates.push(page.top_tracks.as_slice());
        }
        if let Some(library) = &self.library {
            candidates.push(library.tracks.as_slice());
        }
        for section in &self.recommendations {
            candidates.push(section.tracks.as_slice());
        }
        for list in candidates {
            if let Some(index) = list.iter().position(|track| track.id == id) {
                return Some((list.to_vec(), index));
            }
        }
        None
    }

    fn start_track(&mut self, track: Track) {
        let id = track.id;
        // A local id must name a file the scanner still knows about; otherwise nothing can be
        // opened (and the id must never be sent to TIDAL's play endpoint).
        if local::is_local_id(id) && self.local_track(id).is_none() {
            self.notice = Some(Notice::error("That local file is no longer in the library."));
            self.now_playing = None;
            self.pending_play = None;
            return;
        }
        // A fresh track supersedes anything preloaded for the old one.
        self.cancel_preload();
        self.selected = Some(id);
        self.play_seq += 1;
        self.paused = false;
        self.pause_pending = false;
        self.stopping = false;
        self.now_playing = None;
        self.scrub = None;
        // A new play has not been listened to yet, so it is not yet in the history.
        self.play_recorded = None;
        self.pending_play = Some(NowPlaying {
            id, title: track.title, artist: track.artist, album: track.album,
            cover: track.cover, album_id: track.album_id, artist_id: track.artist_id,
            quality: None, elapsed: 0, duration: track.duration, output: None,
        });
        let sent = match self.local_track(id) {
            Some(local_track) => self.cmd_tx.send(Cmd::PlayLocal {
                seq: self.play_seq,
                id,
                path: local_track.path.clone(),
                quality: local_track.quality_label(),
                replaygain: local_track.replaygain,
            }),
            None => self.cmd_tx.send(Cmd::Play { seq: self.play_seq, id }),
        };
        if sent.is_err() {
            self.pending_play = None;
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    /// Adopts `id` as the current track after a gapless hand-off: the audio thread has already
    /// started it, so the queue and the metadata move but nothing is re-issued to the worker.
    fn advance_to_id(&mut self, id: u64, output: Option<String>) {
        let Some(index) = self.queue.tracks().iter().position(|track| track.id == id) else {
            return;
        };
        let Some(track) = self.queue.jump(index) else { return };
        // The output format is whatever the hand-off is really running at; fall back to the
        // previous label when the worker could not read it.
        let output = output.or_else(|| self.now_playing.as_ref().and_then(|playing| playing.output.clone()));
        self.selected = Some(id);
        self.paused = false;
        self.pause_pending = false;
        self.stopping = false;
        self.pending_play = None;
        self.scrub = None;
        self.play_recorded = None;
        self.now_playing = Some(NowPlaying {
            id,
            title: track.title,
            artist: track.artist,
            album: track.album,
            cover: track.cover,
            album_id: track.album_id,
            artist_id: track.artist_id,
            quality: None,
            elapsed: 0,
            duration: track.duration,
            output,
        });
        self.load_lyrics(id);
        self.notify_now_playing();
        self.media_started(0.0);
    }

    /// Asks the worker to open the next queue track ahead of time once this one is nearly over,
    /// so the audio thread can hand over without a gap. Cheap to call on every progress tick.
    fn tick_preload(&mut self, remaining: Option<u64>) {
        if self.now_playing.is_none() {
            return;
        }
        let next = self.queue.peek_advance().map(|track| track.id);
        if let Some(id) = self.preload.next_preload(remaining, next) {
            let current_local = self
                .now_playing
                .as_ref()
                .is_some_and(|playing| local::is_local_id(playing.id));
            // The gapless hand-off happens inside one player, so it only works when both tracks
            // come from the same side: TIDAL resolves TIDAL ids, and the local file path comes
            // from the library index. Across the boundary the next track simply starts the
            // ordinary way.
            match (self.local_track(id), current_local) {
                (Some(track), true) => {
                    let _ = self.cmd_tx.send(Cmd::PreloadLocal {
                        id,
                        path: track.path.clone(),
                        replaygain: track.replaygain,
                    });
                }
                (None, false) => {
                    let _ = self.cmd_tx.send(Cmd::Preload { id });
                }
                _ => {}
            }
        }
    }

    /// Invalidates any outstanding preload because the queue changed. Only costs when one is
    /// actually armed.
    fn cancel_preload(&mut self) {
        if self.preload.invalidate() {
            let _ = self.cmd_tx.send(Cmd::CancelPreload);
        }
    }

    /// Posts a desktop notification for the track that just started, when the user has them on.
    fn notify_now_playing(&self) {
        let Some(notifier) = &self.notifier else { return };
        let Some(playing) = &self.now_playing else { return };
        notifier.notify(Notification {
            title: playing.title.clone(),
            body: notification_body(playing),
            cover: playing.cover.clone(),
        });
    }

    /// Whether the transport's next button should be enabled.
    pub(crate) fn can_skip_next(&self) -> bool {
        self.queue.has_next()
    }

    /// Whether the previous button should be enabled (there is a track to restart or step
    /// back from).
    pub(crate) fn can_skip_previous(&self) -> bool {
        self.queue.current().is_some()
    }

    /// Next track for a manual skip: stops at the end unless repeat-all is on.
    pub(crate) fn skip_next(&mut self) {
        if let Some(track) = self.queue.skip_next() {
            self.start_track(track);
        }
    }

    /// Previous track: restarts the current track if it has played past the window, otherwise
    /// steps back, matching the official client.
    pub(crate) fn skip_previous(&mut self) {
        if self.now_playing.is_some() && playback::previous_restarts(self.current_elapsed()) {
            self.seek_seconds(0.0);
            return;
        }
        if let Some(track) = self.queue.skip_previous() {
            self.start_track(track);
        }
    }

    /// Auto-advance on a natural end; stops (leaving the queue intact) when there is nowhere
    /// to go.
    fn advance_queue(&mut self) {
        match self.queue.advance() {
            Some(track) => self.start_track(track),
            None => {
                self.now_playing = None;
                self.pending_play = None;
                self.paused = false;
                self.pause_pending = false;
                self.stopping = false;
            }
        }
    }

    /// The elapsed seconds of the current track (zero when nothing is playing).
    fn current_elapsed(&self) -> u64 {
        self.now_playing.as_ref().map(|playing| playing.elapsed).unwrap_or(0)
    }

    /// Toggles the right-hand queue panel. On a narrow window the lyrics panel makes way for
    /// it: the sidebar plus both panels would leave the main area no width at all.
    pub(crate) fn toggle_queue(&mut self) {
        self.queue_open = !self.queue_open;
        if self.queue_open && self.narrow_window.get() {
            self.lyrics_open = false;
        }
    }

    /// Toggles the lyrics panel (and, on a narrow window, closes the queue panel).
    pub(crate) fn toggle_lyrics(&mut self) {
        self.lyrics_open = !self.lyrics_open;
        if self.lyrics_open && self.narrow_window.get() {
            self.queue_open = false;
        }
    }

    /// Plays the queue row at `index` (a click in the queue panel).
    pub(crate) fn play_queue_index(&mut self, index: usize) {
        if let Some(track) = self.queue.jump(index) {
            self.start_track(track);
        }
    }

    /// Removes the queue row at `index` (the row's x button).
    pub(crate) fn remove_from_queue(&mut self, index: usize) {
        self.queue.remove(index);
        self.cancel_preload();
    }

    /// Clears the queue, keeping whatever is currently playing.
    pub(crate) fn clear_queue(&mut self) {
        match self.queue.current().cloned() {
            Some(track) => self.queue.set(vec![track], 0),
            None => self.queue.clear(),
        }
        self.cancel_preload();
    }

    /// Plays `track` immediately after the current one ("Play next"). Context menus call this;
    /// with nothing queued it starts playing right away.
    pub(crate) fn queue_next(&mut self, track: Track) {
        self.request_cover(&track);
        if self.queue_idle() {
            self.queue.set(vec![track.clone()], 0);
            self.start_track(track);
        } else {
            self.queue.insert_next(track);
            // The preloaded track is no longer what comes next.
            self.cancel_preload();
        }
    }

    /// Appends `track` to the end of the queue ("Add to queue"). Context menus call this; with
    /// nothing queued it starts playing right away.
    pub(crate) fn queue_add(&mut self, track: Track) {
        self.request_cover(&track);
        if self.queue_idle() {
            self.queue.set(vec![track.clone()], 0);
            self.start_track(track);
        } else {
            self.queue.push_back(track);
        }
    }

    /// Whether there is nothing playing and nothing queued.
    fn queue_idle(&self) -> bool {
        self.now_playing.is_none() && self.pending_play.is_none() && self.queue.is_empty()
    }

    fn request_cover(&mut self, track: &Track) {
        if let (Some(artwork), Some(cover)) = (&mut self.artwork, &track.cover) {
            artwork.request(cover);
        }
    }

    /// Records that this frame draws `id` as `kind`, so `poll` fetches it once the frame is
    /// committed. Cheap and idempotent: an id the cache has ever been asked for (in flight,
    /// decoded or failed) is not queued again, and an id already queued this frame is not
    /// queued twice. Only art a surface actually draws is ever requested, however long the list
    /// behind it is.
    ///
    /// The first want of a frame also wakes the app. `poll` runs before the view, so a want a
    /// view notes is only turned into a fetch by the *next* frame's poll; on a cold start with
    /// nothing else in flight that frame would never come and the covers would stay blank until
    /// something else moved. Waking here guarantees the poll that drains the want.
    pub(crate) fn want_art(&self, kind: ArtKind, id: Option<&str>) {
        let Some(id) = id else { return };
        if id.is_empty() || self.artwork.as_ref().is_none_or(|artwork| artwork.knows(id)) {
            return;
        }
        let mut wanted = self.art_wanted.borrow_mut();
        if wanted.iter().any(|(_, queued)| queued == id) {
            return;
        }
        let first = wanted.is_empty();
        wanted.push((kind, id.to_owned()));
        drop(wanted);
        if first {
            if let Some(wake) = &self.waker {
                wake();
            }
        }
    }

    /// The decoded art for `id`, or `None` while it is still loading. Notes the want, so `poll`
    /// fetches — and the cache decodes — only art a surface actually shows.
    pub(crate) fn wanted_art(&self, kind: ArtKind, id: Option<&str>) -> Option<rune::Image> {
        let ready = id
            .and_then(|id| self.artwork.as_ref()?.get(id))
            .filter(|image| image.is_ready())
            .cloned();
        if ready.is_none() {
            self.want_art(kind, id);
        }
        ready
    }

    // -----------------------------------------------------------------------------------------
    // Volume

    /// Sets the output volume (`0..=1`) and persists it.
    pub(crate) fn set_volume(&mut self, volume: f32) {
        let volume = if volume.is_nan() { 1.0 } else { volume.clamp(0.0, 1.0) };
        if (volume - self.settings.volume).abs() < f32::EPSILON {
            return;
        }
        self.settings.volume = volume;
        let _ = self.cmd_tx.send(Cmd::SetVolume(volume));
        self.settings.save();
        self.publish_mpris();
    }

    /// Steps the volume by `delta` (keyboard and wheel).
    pub(crate) fn adjust_volume(&mut self, delta: f32) {
        self.set_volume(self.settings.volume + delta);
    }

    /// A wheel or trackpad delta over the volume control, when the "Adjust volume with the
    /// mouse wheel" option is on. `dy` is the delta in points — positive is a roll up, which
    /// turns the volume up — or the horizontal delta where the platform turned a Shift+wheel
    /// on its side (see the volume control in `ui::player_bar`). Shift asks for a fine step.
    ///
    /// A notched mouse wheel delivers one 40-point line per notch; smaller deltas accumulate,
    /// so a trackpad's run of many small ones steps at the same rate instead of one step per
    /// event. The volume clamps at 0 and 1 like any other step.
    pub(crate) fn wheel_volume(&mut self, dy: f32, fine: bool) {
        const NOTCH: f32 = 40.0;
        let step = if fine { 0.01 } else { 0.05 };
        let mut steps = 0.0;
        // Turning the wheel back first spends the leftover that way, as the physical wheel does.
        self.volume_wheel += dy;
        while self.volume_wheel >= NOTCH {
            self.volume_wheel -= NOTCH;
            steps += 1.0;
        }
        while self.volume_wheel <= -NOTCH {
            self.volume_wheel += NOTCH;
            steps -= 1.0;
        }
        if steps != 0.0 {
            self.adjust_volume(steps * step);
        }
    }

    /// Sets whether the mouse wheel over the volume control adjusts the volume, and persists it.
    /// Off by default: a scroll near the volume bar must leave the volume alone.
    pub(crate) fn set_volume_scroll(&mut self, on: bool) {
        if self.settings.volume_scroll == on {
            return;
        }
        self.settings.volume_scroll = on;
        self.settings.save();
    }

    /// Volume while dragging the slider: applied and sent live, unmuting, but written to disk
    /// only on release so a drag does not hammer the settings file.
    pub(crate) fn scrub_volume(&mut self, fraction: f32, release: bool) {
        let volume = if fraction.is_nan() { 1.0 } else { fraction.clamp(0.0, 1.0) };
        self.settings.volume = volume;
        let _ = self.cmd_tx.send(Cmd::SetVolume(volume));
        if self.settings.muted {
            self.settings.muted = false;
            let _ = self.cmd_tx.send(Cmd::SetMuted(false));
        }
        if release {
            self.settings.save();
        }
        self.publish_mpris();
    }

    pub(crate) fn toggle_mute(&mut self) {
        self.set_muted(!self.settings.muted);
    }

    /// Mutes or unmutes; the configured volume is kept either way.
    pub(crate) fn set_muted(&mut self, muted: bool) {
        if self.settings.muted == muted {
            return;
        }
        self.settings.muted = muted;
        let _ = self.cmd_tx.send(Cmd::SetMuted(muted));
        self.settings.save();
        self.publish_mpris();
    }

    // -----------------------------------------------------------------------------------------
    // Shuffle, repeat, quality, normalization

    /// Toggles shuffle, reshuffling what follows the current track, and persists it.
    pub(crate) fn toggle_shuffle(&mut self) {
        let on = !self.queue.shuffle();
        self.queue.set_shuffle(on);
        self.settings.shuffle = on;
        self.settings.save();
        // Shuffling changes what comes next, so any preload is now the wrong track.
        self.cancel_preload();
    }

    /// Cycles repeat off → all → one and persists it.
    pub(crate) fn cycle_repeat(&mut self) {
        let repeat = self.queue.repeat().cycle();
        self.queue.set_repeat(repeat);
        self.settings.repeat = repeat;
        self.settings.save();
        self.cancel_preload();
    }

    /// Sets the streaming-quality preference and persists it.
    pub(crate) fn set_quality(&mut self, quality: Quality) {
        if self.settings.quality == quality {
            return;
        }
        self.settings.quality = quality;
        let _ = self.cmd_tx.send(Cmd::SetQuality(quality));
        self.settings.save();
    }

    /// Turns ReplayGain-style normalization on or off and persists it.
    pub(crate) fn set_normalize(&mut self, normalize: bool) {
        if self.settings.normalize == normalize {
            return;
        }
        self.settings.normalize = normalize;
        let _ = self.cmd_tx.send(Cmd::SetNormalize(normalize));
        self.settings.save();
    }

    /// Chooses the output device and persists it.
    ///
    /// `None` means the system default. When a track is playing the worker re-opens the device
    /// at the position that was playing; a device that has since disappeared shows up as a
    /// playback error notice.
    pub(crate) fn set_output_device(&mut self, device: Option<String>) {
        if self.settings.output_device == device {
            return;
        }
        self.settings.output_device = device.clone();
        let _ = self.cmd_tx.send(Cmd::SetOutputDevice(device.clone()));
        if let Some(session) = &mut self.video {
            session.engine.set_output_device(device.clone());
            session.device = device;
        }
        self.settings.save();
    }

    /// Re-enumerates the host's output devices, e.g. after plugging a DAC in.
    pub(crate) fn refresh_output_devices(&mut self) {
        self.output_devices = Some(crate::audio::output_devices());
    }

    /// Turns desktop notifications on or off and persists it.
    ///
    /// Turning them on connects to the session bus; if that is unavailable the preference stays
    /// off and a notice explains why, so the toggle never lies about what will happen.
    pub(crate) fn set_notifications(&mut self, on: bool) {
        if self.settings.notifications == on {
            return;
        }
        if on {
            match Notifier::start() {
                Ok(notifier) => {
                    self.notifier = Some(notifier);
                    self.settings.notifications = true;
                }
                Err(error) => {
                    self.notifier = None;
                    self.settings.notifications = false;
                    self.notice = Some(Notice::error(format!(
                        "Desktop notifications are unavailable: {error}"
                    )));
                }
            }
        } else {
            self.notifier = None;
            self.settings.notifications = false;
        }
        self.settings.save();
    }

    /// Opens or closes the full-screen Now Playing view.
    pub(crate) fn toggle_now_playing(&mut self) {
        self.now_playing_open = !self.now_playing_open;
    }

    /// Signs out: closes the menus and asks the worker to forget the session. The UI leaves
    /// the playback state to the `Ready { logged_in: false }` event that follows.
    pub(crate) fn sign_out(&mut self) {
        self.profile_menu_open = false;
        if self.cmd_tx.send(Cmd::Logout).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    // -----------------------------------------------------------------------------------------
    // Seeking

    /// Seeks to an absolute position within the current track.
    pub(crate) fn seek_seconds(&mut self, seconds: f64) {
        let Some(playing) = &mut self.now_playing else { return };
        let duration = playing.duration.unwrap_or(0) as f64;
        let seconds = if duration > 0.0 {
            seconds.clamp(0.0, duration)
        } else {
            seconds.max(0.0)
        };
        playing.elapsed = seconds as u64;
        self.scrub = None;
        let position_us = (seconds * 1_000_000.0) as i64;
        if let Some(mpris) = &self.mpris {
            mpris.set_position(position_us);
            mpris.emit_seeked(position_us);
        }
        let seq = self.play_seq;
        if self.cmd_tx.send(Cmd::Seek { seq, seconds }).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
        self.media_seeked(seconds);
    }

    /// Seeks to a fraction of the current track (a click or drag on the progress bar).
    pub(crate) fn seek_fraction(&mut self, fraction: f32) {
        let Some(duration) = self.now_playing.as_ref().and_then(|playing| playing.duration).filter(|duration| *duration > 0) else {
            return;
        };
        self.seek_seconds(fraction.clamp(0.0, 1.0) as f64 * duration as f64);
    }

    /// Seeks by a relative offset in seconds (desktop `SeekBy`).
    pub(crate) fn seek_relative(&mut self, delta: f64) {
        let current = self.current_elapsed() as f64;
        self.seek_seconds(current + delta);
    }

    /// Progress-bar drag: show a preview while dragging, commit the seek on release.
    pub(crate) fn scrub(&mut self, fraction: f32, release: bool) {
        if release {
            self.scrub = None;
            self.seek_fraction(fraction.clamp(0.0, 1.0));
        } else {
            self.scrub = Some(fraction.clamp(0.0, 1.0));
        }
    }

    // -----------------------------------------------------------------------------------------
    // MPRIS (desktop media keys and panel widgets)

    fn mpris_command(&mut self, command: MprisCommand, tx: &mut Tx) {
        if self.video_active() {
            // The video view owns the transport while it is up, exactly as it
            // owns the output device.
            match command {
                MprisCommand::Play => {
                    if let Some(session) = &self.video {
                        session.engine.set_paused(false);
                        self.publish_mpris();
                    }
                }
                MprisCommand::Pause => {
                    if let Some(session) = &self.video {
                        session.engine.set_paused(true);
                        self.publish_mpris();
                    }
                }
                MprisCommand::PlayPause => self.toggle_video_pause(),
                MprisCommand::Stop => self.close_video(),
                MprisCommand::SeekTo(micros) => {
                    if let Some(session) = &self.video {
                        session.engine.seek(micros as f64 / 1_000_000.0);
                    }
                }
                MprisCommand::SeekBy(micros) => {
                    if let Some(session) = &self.video {
                        let target = session.engine.position_secs() + micros as f64 / 1_000_000.0;
                        session.engine.seek(target);
                    }
                }
                MprisCommand::SetVolume(volume) => {
                    self.set_muted(false);
                    self.set_volume(volume as f32);
                }
                MprisCommand::Quit => tx.quit(),
                MprisCommand::Next | MprisCommand::Previous => {}
            }
            return;
        }
        match command {
            MprisCommand::Play => self.resume_or_play(),
            MprisCommand::Pause => {
                if self.now_playing.is_some() && !self.paused {
                    self.toggle_pause();
                }
            }
            MprisCommand::PlayPause => {
                if self.now_playing.is_some() {
                    self.toggle_pause();
                } else {
                    self.play_from_queue();
                }
            }
            MprisCommand::Stop => self.stop(),
            MprisCommand::Next => self.skip_next(),
            MprisCommand::Previous => self.skip_previous(),
            MprisCommand::SeekTo(micros) => self.seek_seconds(micros as f64 / 1_000_000.0),
            MprisCommand::SeekBy(micros) => self.seek_relative(micros as f64 / 1_000_000.0),
            MprisCommand::SetVolume(volume) => {
                self.set_muted(false);
                self.set_volume(volume as f32);
            }
            MprisCommand::Quit => tx.quit(),
        }
    }

    /// Resumes if paused, and starts the current queue track if nothing is playing.
    pub(crate) fn resume_or_play(&mut self) {
        if self.now_playing.is_some() {
            if self.paused {
                self.toggle_pause();
            }
        } else if self.pending_play.is_none() {
            self.play_from_queue();
        }
    }

    /// Starts the queue's current track, or the first one when none is selected.
    fn play_from_queue(&mut self) {
        if let Some(track) = self.queue.current().or_else(|| self.queue.tracks().first()).cloned() {
            self.start_track(track);
        } else if let Some(id) = self.selected {
            self.play_id(id);
        }
    }

    /// Pushes the current playback state to MPRIS, if the service is running.
    fn publish_mpris(&mut self) {
        if self.mpris.is_none() {
            return;
        }
        let volume = if self.settings.muted { 0.0 } else { self.settings.volume } as f64;
        if let Some(session) = self.video.as_ref() {
            let video = &session.video;
            let key = (video.id, Some("video".to_owned()));
            let metadata = MprisMeta {
                title: video.title.clone(),
                artist: video.artist.clone(),
                album: String::new(),
                art_url: video.image.as_deref().and_then(art::video_art_url),
                length_us: session.engine.duration_secs() * 1_000_000,
                track_id: format!("video-{}", video.id),
            };
            let status = match session.engine.status() {
                video::Status::Paused => MprisPlayback::Paused,
                video::Status::Ended | video::Status::Failed(_) => MprisPlayback::Stopped,
                _ => MprisPlayback::Playing,
            };
            let position_us = (session.engine.position_secs() * 1_000_000.0) as i64;
            let track_changed = Some(key.clone()) != self.mpris_track;
            let volume_changed = self.mpris_volume != Some(volume);
            self.mpris_track = Some(key);
            self.mpris_volume = Some(volume);
            let Some(mpris) = &self.mpris else { return };
            if track_changed {
                mpris.set_metadata(metadata);
            }
            mpris.set_playback(status);
            mpris.set_can_go(false, false);
            if volume_changed {
                mpris.set_volume(volume);
            }
            mpris.set_position(position_us);
            return;
        }
        let playing = self.now_playing.as_ref().or(self.pending_play.as_ref());
        let key = playing.map(|track| (track.id, track.cover.clone()));
        let metadata = playing.map(|track| MprisMeta {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            art_url: track.cover.as_deref().and_then(|cover| art::art_url(cover, 320)),
            length_us: track.duration.unwrap_or(0) * 1_000_000,
            track_id: track.id.to_string(),
        });
        let status = if self.now_playing.is_some() && !self.paused {
            MprisPlayback::Playing
        } else if self.now_playing.is_some() {
            MprisPlayback::Paused
        } else {
            MprisPlayback::Stopped
        };
        let position_us = (self.current_elapsed() * 1_000_000) as i64;
        let can_next = self.queue.has_next();
        let can_previous = self.queue.current().is_some();
        let track_changed = key != self.mpris_track;
        let volume_changed = self.mpris_volume != Some(volume);
        self.mpris_track = key;
        self.mpris_volume = Some(volume);
        let Some(mpris) = &self.mpris else { return };
        if track_changed {
            mpris.set_metadata(metadata.unwrap_or_default());
        }
        mpris.set_playback(status);
        mpris.set_can_go(can_next, can_previous);
        if volume_changed {
            mpris.set_volume(volume);
        }
        mpris.set_position(position_us);
    }

    pub(crate) fn toggle_pause(&mut self) {
        if self.now_playing.is_none() || self.pause_pending { return; }
        self.pause_pending = true;
        if self.cmd_tx.send(Cmd::SetPaused { seq: self.play_seq, paused: !self.paused }).is_err() {
            self.pause_pending = false;
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    pub(crate) fn stop(&mut self) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        if self.cmd_tx.send(Cmd::Stop).is_err() {
            self.stopping = false;
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    // -----------------------------------------------------------------------------------------
    // Videos

    /// Opens the video player for `video`: stops the music queue, shows the
    /// overlay at once and asks the worker for the stream.
    pub(crate) fn play_video(&mut self, video: Video) {
        if !video.stream_ready {
            self.notice = Some(Notice::error("TIDAL does not offer this video for streaming"));
            return;
        }
        if self.now_playing.is_some() || self.pending_play.is_some() || !self.queue.is_empty() {
            // The music player and a video must never share the output.
            let _ = self.cmd_tx.send(Cmd::Stop);
        }
        self.now_playing = None;
        self.pending_play = None;
        self.paused = false;
        self.pause_pending = false;
        self.stopping = false;
        self.preload = GaplessState::new();
        self.now_playing_open = false;
        self.scrub = None;
        self.clear_lyrics();
        self.video = None;
        self.video_clock = None;
        self.video_pending = Some(video.clone());
        self.video_seq += 1;
        let seq = self.video_seq;
        if self.cmd_tx.send(Cmd::OpenVideo { seq, id: video.id }).is_err() {
            self.video_pending = None;
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    /// Leaves the video view and stops its decode/audio threads.
    pub(crate) fn close_video(&mut self) {
        self.video = None;
        self.video_pending = None;
        self.video_seq += 1;
        self.video_clock = None;
        if self.mpris.is_some() {
            self.publish_mpris();
        }
    }

    /// Restarts a video that ended (or failed) from the beginning, using the HLS
    /// master URL TIDAL handed back. The old engine is dropped, which stops its
    /// threads, before the new one starts.
    pub(crate) fn replay_video(&mut self) {
        let Some(session) = self.video.as_ref() else { return };
        let engine = video::Engine::start(
            session.master_url.clone(),
            session.volume.clone(),
            session.device.clone(),
            session.duration_ms,
            0.0,
        );
        if let Some(session) = self.video.as_mut() {
            session.engine = engine;
        }
        self.video_clock = None;
        self.publish_mpris();
    }

    /// Whether the video overlay is up (resolving or playing).
    pub(crate) fn video_active(&self) -> bool {
        self.video.is_some() || self.video_pending.is_some()
    }

    /// The video being shown, once TIDAL resolved its stream.
    pub(crate) fn video_session(&self) -> Option<&VideoSession> {
        self.video.as_ref()
    }

    /// The catalogue entry for the video on screen (resolving or playing).
    pub(crate) fn video_meta(&self) -> Option<&Video> {
        self.video
            .as_ref()
            .map(|session| &session.video)
            .or(self.video_pending.as_ref())
    }

    pub(crate) fn toggle_video_pause(&mut self) {
        if let Some(session) = &self.video {
            session.engine.set_paused(!session.engine.is_paused());
            self.publish_mpris();
        }
    }

    pub(crate) fn video_seek_fraction(&mut self, fraction: f32) {
        let Some(session) = &self.video else { return };
        let duration = session
            .engine
            .duration_secs()
            .max(session.video.duration.unwrap_or(0))
            .max(1);
        let target = fraction.clamp(0.0, 1.0) as f64 * duration as f64;
        session.engine.seek(target);
        self.video_clock = None;
        self.scrub = None;
    }

    /// Video seek-bar drag: preview while moving, commit on release.
    pub(crate) fn scrub_video(&mut self, fraction: f32, release: bool) {
        if release {
            self.video_seek_fraction(fraction);
        } else {
            self.scrub = Some(fraction.clamp(0.0, 1.0));
        }
    }

    pub(crate) fn video_position_secs(&self) -> u64 {
        self.video
            .as_ref()
            .map(|session| session.engine.position_secs() as u64)
            .unwrap_or(0)
    }

    pub(crate) fn video_duration_secs(&self) -> u64 {
        self.video
            .as_ref()
            .map(|session| {
                session
                    .engine
                    .duration_secs()
                    .max(session.video.duration.unwrap_or(0))
            })
            .or_else(|| self.video_pending.as_ref().and_then(|video| video.duration))
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------------------------------
    // Context menus

    /// Opens a context menu anchored to the element the press came from.
    pub(crate) fn open_menu(&mut self, mut menu: MenuState) {
        // A track row's menu re-finds its place in the list it came from, by id: the row may
        // have been built at another index (a virtual row the list has since moved past).
        if let crate::ui::context_menu::MenuTarget::Track(track) = &mut menu.target {
            let s: &Ui = self;
            track.resolve(s);
        }
        self.dialog = None;
        self.menu = Some(menu);
    }

    /// Closes the open context menu (its items, the scrim, or Escape).
    pub(crate) fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Opens the "Add to playlist" submenu of the open context menu.
    pub(crate) fn open_playlist_submenu(&mut self) {
        if let Some(menu) = &mut self.menu {
            menu.submenu = true;
        }
    }

    /// A menu's "Play next" for a whole list: each track goes in after the current one, in order.
    pub(crate) fn queue_next_list(&mut self, tracks: Vec<Track>) {
        self.menu = None;
        if tracks.is_empty() {
            return;
        }
        for track in &tracks {
            self.request_cover(track);
        }
        if self.queue_idle() {
            self.play_tracks(tracks, 0);
        } else {
            self.queue.insert_next_many(tracks);
        }
    }

    /// A menu's "Add to queue" for a whole list: the tracks land at the end, in order.
    pub(crate) fn queue_add_list(&mut self, tracks: Vec<Track>) {
        self.menu = None;
        if tracks.is_empty() {
            return;
        }
        for track in &tracks {
            self.request_cover(track);
        }
        if self.queue_idle() {
            self.play_tracks(tracks, 0);
        } else {
            self.queue.push_back_many(tracks);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Playlist editing

    /// "Add to playlist": appends the menu's tracks to `id` and reports the outcome as a notice.
    pub(crate) fn add_to_playlist(&mut self, id: String, track_ids: Vec<u64>) {
        self.menu = None;
        if track_ids.is_empty() {
            return;
        }
        let count = track_ids.len();
        if self.cmd_tx.send(Cmd::AddToPlaylist { id, track_ids }).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        } else {
            self.notice = Some(Notice::info(format!(
                "Adding {count} track{}…",
                if count == 1 { "" } else { "s" }
            )));
        }
    }

    /// "Remove from this playlist": drops the playlist item at `index`.
    pub(crate) fn remove_from_playlist(&mut self, id: String, index: u64) {
        self.menu = None;
        if self.cmd_tx.send(Cmd::RemoveFromPlaylist { id, index }).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    /// Moves one of the user's own playlist tracks from row `from` to row `to`.
    ///
    /// The order changes on the spot (so the page shows the move this frame) and the worker is
    /// asked to match it; a failure rolls the rows back and leaves a notice. Only own playlists
    /// have a move endpoint, which the menu already enforces.
    pub(crate) fn move_playlist_item(&mut self, id: String, from: u64, to: u64) {
        self.menu = None;
        if from == to {
            return;
        }
        if self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
            swap_items(self.playlist_tracks.edit(), from as usize, to as usize);
        }
        if self.cmd_tx.send(Cmd::MovePlaylistItem { id: id.clone(), from, to }).is_err() {
            if self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
                swap_items(self.playlist_tracks.edit(), to as usize, from as usize);
            }
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
            return;
        }
        self.playlist_move = Some((id, from, to));
    }

    /// Moves the queue row at `from` to `to` (the queue panel's "Move up"/"Move down").
    pub(crate) fn move_queue_item(&mut self, from: usize, to: usize) {
        self.menu = None;
        if self.queue.move_item(from, to) {
            // The track that comes next may have changed, so any preload is now the wrong one.
            self.cancel_preload();
        }
    }

    /// Asks for the artist's remaining top tracks ("Show all"): everything already loaded is
    /// revealed at once and the next pages are fetched behind it.
    pub(crate) fn show_all_top_tracks(&mut self, id: u64) {
        self.artist_top_all = true;
        if self.artist_top_loading {
            return;
        }
        let offset = self
            .artist_page
            .as_ref()
            .map(|page| page.top_tracks.len().min(u32::MAX as usize) as u32)
            .unwrap_or(0);
        if self.cmd_tx.send(Cmd::ArtistTopTracks { id, offset }).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
            return;
        }
        self.artist_top_loading = true;
    }

    /// The recorded history as playable tracks, newest first.
    pub(crate) fn history_tracks(&self) -> Vec<Track> {
        self.history_log.iter().map(PlayEntry::track).collect()
    }

    /// Forgets the local listening history, on disk included.
    pub(crate) fn clear_history(&mut self) {
        history_log::clear(&mut self.history_log);
        history_log::save(&self.history_log);
        self.data_changed();
        self.notice = Some(Notice::info("Cleared the listening history"));
    }

    /// Records the current track in the local history once it has actually been listened to:
    /// past the module's threshold, or when it reached its natural end. Idempotent per play.
    fn record_play_if_listened(&mut self, elapsed: u64, finished: bool) {
        if !history_log::counts_as_played(elapsed, finished) {
            return;
        }
        let Some(playing) = &self.now_playing else { return };
        if self.play_recorded == Some(playing.id) {
            return;
        }
        let entry = PlayEntry {
            id: playing.id,
            title: playing.title.clone(),
            artist: playing.artist.clone(),
            album: playing.album.clone(),
            cover: playing.cover.clone(),
            duration: playing.duration,
            album_id: playing.album_id,
            artist_id: playing.artist_id,
            ts: history_log::now_secs(),
        };
        self.play_recorded = Some(playing.id);
        if history_log::record(&mut self.history_log, entry) {
            history_log::save(&self.history_log);
            self.data_changed();
        }
    }

    /// Remembers `route` as the last visited place, so "Last visited" can restore it.
    fn remember_route(&mut self, route: &Route) {
        let persisted = route.persist();
        if self.settings.last_route.as_deref() != Some(persisted.as_str()) {
            self.settings.last_route = Some(persisted);
            self.settings.save();
        }
    }

    /// Opens the page the user chose to start on (Settings → Startup page).
    pub(crate) fn open_launch_page(&mut self) {
        match self.settings.startup_page {
            StartupPage::Home => {}
            StartupPage::Collection => self.navigate(Route::Collection),
            StartupPage::LastVisited | StartupPage::Unknown => {
                if let Some(route) = self.settings.last_route.as_deref().and_then(Route::restore) {
                    if route != Route::Home {
                        self.navigate(route);
                    }
                }
            }
        }
    }

    /// Sets the startup page and persists it.
    pub(crate) fn set_startup_page(&mut self, page: StartupPage) {
        if self.settings.startup_page == page {
            return;
        }
        self.settings.startup_page = page;
        self.settings.save();
    }

    /// Turns close-to-tray on or off and persists it. Only offered while a tray exists.
    pub(crate) fn set_close_to_tray(&mut self, on: bool) {
        if self.settings.close_to_tray == on {
            return;
        }
        self.settings.close_to_tray = on;
        self.settings.save();
    }

    /// Opens the create-playlist dialog, remembering `tracks` to add once it exists.
    pub(crate) fn open_create_playlist(&mut self, tracks: Vec<u64>) {
        self.menu = None;
        self.playlist_form_title = TextEdit::new("");
        self.playlist_form_desc = TextEdit::new("");
        self.playlist_form_title.focused = true;
        self.dialog = Some(Dialog::CreatePlaylist { tracks });
    }

    /// Opens the rename/re-describe dialog for an owned playlist.
    pub(crate) fn open_edit_playlist(&mut self, playlist: Playlist) {
        self.menu = None;
        self.playlist_form_title = TextEdit::new(playlist.title);
        self.playlist_form_title.focused = true;
        self.playlist_form_desc = TextEdit::new(playlist.description.unwrap_or_default());
        self.dialog = Some(Dialog::EditPlaylist { id: playlist.id });
    }

    /// Opens the delete confirmation for an owned playlist.
    pub(crate) fn open_delete_confirm(&mut self, playlist: Playlist) {
        self.menu = None;
        self.dialog = Some(Dialog::ConfirmDelete { id: playlist.id, title: playlist.title });
    }

    /// Closes whatever dialog is open.
    pub(crate) fn close_dialog(&mut self) {
        self.dialog = None;
    }

    /// The create/edit form's submit button: creates or renames, then closes the dialog.
    pub(crate) fn submit_playlist_form(&mut self) {
        let title = self.playlist_form_title.text.trim().to_owned();
        if title.is_empty() {
            self.notice = Some(Notice::error("A playlist needs a title"));
            return;
        }
        let description = self.playlist_form_desc.text.trim().to_owned();
        let dialog = self.dialog.take();
        match dialog {
            Some(Dialog::EditPlaylist { id }) => {
                if self.cmd_tx.send(Cmd::RenamePlaylist { id, title, description }).is_err() {
                    self.notice = Some(Notice::error("The TIDAL worker is not running."));
                }
            }
            Some(Dialog::CreatePlaylist { tracks }) => {
                let command = Cmd::CreatePlaylist { title, description, tracks };
                if self.cmd_tx.send(command).is_err() {
                    self.notice = Some(Notice::error("The TIDAL worker is not running."));
                }
            }
            _ => {}
        }
    }

    /// The delete confirmation's button.
    pub(crate) fn confirm_delete_playlist(&mut self) {
        let Some(Dialog::ConfirmDelete { id, .. }) = &self.dialog else { return };
        let id = id.clone();
        if self.cmd_tx.send(Cmd::DeletePlaylist(id)).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
            return;
        }
        self.dialog = None;
    }

    // -----------------------------------------------------------------------------------------
    // Track info and radio

    /// Opens the track-info dialog and asks TIDAL for the track's credits.
    pub(crate) fn open_track_info(&mut self, track: Track) {
        self.menu = None;
        let track_id = track.id;
        self.dialog = Some(Dialog::TrackInfo(TrackInfo {
            track,
            state: SearchState::Searching,
            credits: Vec::new(),
        }));
        if self.cmd_tx.send(Cmd::TrackCredits(track_id)).is_err() {
            if let Some(Dialog::TrackInfo(info)) = &mut self.dialog {
                info.state = SearchState::Failed(String::new());
            }
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    /// Starts TIDAL's track radio; the tracks arrive as `Ev::TrackRadio` and play.
    pub(crate) fn play_track_radio(&mut self, id: u64) {
        self.menu = None;
        if self.cmd_tx.send(Cmd::TrackRadio(id)).is_err() {
            self.notice = Some(Notice::error("The TIDAL worker is not running."));
        }
    }

    /// Applies one playlist's metadata to the open page and the sidebar list.
    fn apply_playlist_meta(&mut self, meta: Playlist, id: &str) {
        if let Some(entry) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
            // The list endpoint already has the authoritative track count; keep it.
            let count = entry.count;
            *entry = Playlist { count, ..meta.clone() };
        }
        if let Some(playlist) = self.playlist.as_mut().filter(|playlist| playlist.id == id) {
            let count = if self.playlist_tracks.is_empty() { meta.count } else { self.playlist_tracks.len() as u64 };
            *playlist = Playlist { count, ..meta };
        }
    }

    /// The best title we know for a playlist id, for notices.
    fn playlist_title(&self, id: &str) -> String {
        self.playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.title.clone())
            .or_else(|| self.playlist.as_ref().filter(|playlist| playlist.id == id).map(|playlist| playlist.title.clone()))
            .unwrap_or_else(|| "the playlist".to_owned())
    }

    /// Reloads the open playlist page when its id matches, so a change shows at once.
    fn reload_open_playlist(&mut self, id: &str) {
        if self.page == Page::Playlist && self.playlist.as_ref().is_some_and(|playlist| playlist.id == id) {
            self.load_playlist(id);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// App

pub(crate) fn on_key(s: &mut Ui, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.key == Key::Escape {
        if s.dialog.is_some() {
            s.close_dialog();
            return true;
        }
        if s.menu.is_some() {
            s.close_menu();
            return true;
        }
        if s.video_active() {
            s.close_video();
            return true;
        }
        if s.now_playing_open {
            s.now_playing_open = false;
            return true;
        }
        if s.profile_menu_open {
            s.profile_menu_open = false;
            return true;
        }
        if s.collection_sort_open {
            s.collection_sort_open = false;
            return true;
        }
        if s.queue_open {
            s.queue_open = false;
            return true;
        }
        if s.page == Page::Settings {
            s.page = Page::Home;
            return true;
        }
    }
    if ev.key == Key::Command('q') {
        tx.quit();
        return true;
    }

    let logged_in = matches!(&s.login, Login::LoggedIn);
    // File and menu shortcuts carry a modifier, so they apply even while a field has focus.
    match ev.key {
        Key::Command('f') if logged_in => {
            s.query.focused = true;
            return true;
        }
        Key::Command('r') if logged_in => {
            s.refresh_page();
            return true;
        }
        Key::Command(',') if logged_in => {
            s.profile_menu_open = false;
            s.page = Page::Settings;
            return true;
        }
        Key::Function(5) if logged_in => {
            s.refresh_page();
            return true;
        }
        _ => {}
    }

    // The transport and plain-key shortcuts only apply while nothing is being typed.
    if !logged_in || s.query.focused || s.pkce_redirect.focused || s.collection_filter.focused {
        return false;
    }
    let primary = ev.mods.ctrl || ev.mods.cmd;
    if primary {
        match ev.key {
            Key::Right => {
                s.skip_next();
                return true;
            }
            Key::Left => {
                s.skip_previous();
                return true;
            }
            Key::Up => {
                s.adjust_volume(0.05);
                return true;
            }
            Key::Down => {
                s.adjust_volume(-0.05);
                return true;
            }
            _ => {}
        }
        return false;
    }
    // Alt+Left/Right walk the navigation history, as in a browser. Ctrl+Left/Right stay on
    // previous/next track (the transport's own shortcut).
    if ev.mods.alt {
        match ev.key {
            Key::Left => {
                s.go_back();
                return true;
            }
            Key::Right => {
                s.go_forward();
                return true;
            }
            _ => {}
        }
        return false;
    }
    match &ev.key {
        Key::Text(text) => match text.as_str() {
            "/" if !ev.mods.shift => {
                s.query.focused = true;
                true
            }
            " " if !ev.mods.shift => {
                if s.video_active() {
                    s.toggle_video_pause();
                } else {
                    s.resume_or_play();
                }
                true
            }
            "m" if !ev.mods.shift => {
                s.toggle_mute();
                true
            }
            "s" if !ev.mods.shift => {
                s.toggle_shuffle();
                true
            }
            "r" if !ev.mods.shift => {
                s.cycle_repeat();
                true
            }
            "q" if !ev.mods.shift => {
                s.toggle_queue();
                true
            }
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tidal::Track;

    fn drain(s: &Ui) {
        if let Some(rx) = &s.cmd_rx {
            while rx.try_recv().is_ok() {}
        }
    }

    /// Main's rule: a local file is never a TIDAL favourite, and its id never reaches the
    /// favourites API.
    #[test]
    fn local_track_ids_are_never_favourites() {
        let mut s = init();
        let id = local::local_id(std::path::Path::new("/music/album/song.flac"));
        let text = id.to_string();
        assert!(!s.is_favorite(FavKind::Track, &text));
        s.toggle_favorite(FavKind::Track, &text);
        assert!(!s.is_favorite(FavKind::Track, &text));
        assert!(s.favorite_tracks.is_empty(), "the TIDAL favourite set stayed untouched");
        // A TIDAL track still toggles normally.
        s.toggle_favorite(FavKind::Track, "42");
        assert!(s.is_favorite(FavKind::Track, "42"));
    }

    /// A local id the scanner does not know must never be sent to TIDAL's play endpoint.
    #[test]
    fn an_unknown_local_track_never_reaches_the_tidal_play_command() {
        let mut s = init();
        drain(&s);
        let id = local::local_id(std::path::Path::new("/music/missing.flac"));
        s.start_track(Track {
            id,
            title: "Missing".to_owned(),
            ..Track::default()
        });
        assert!(s.pending_play.is_none());
        assert!(s.notice.is_some(), "the user is told the file is gone");
        if let Some(rx) = &s.cmd_rx {
            assert!(rx.try_recv().is_err(), "no command was issued");
        }
    }

    /// The sidebar plus the queue and lyrics panels leave a narrow window's main area no width,
    /// so there the two panels swap; a wide window keeps both.
    #[test]
    fn the_queue_and_lyrics_panels_swap_on_a_narrow_window_only() {
        let mut s = init();
        s.narrow_window.set(true);
        s.toggle_queue();
        s.toggle_lyrics();
        assert!(s.lyrics_open && !s.queue_open, "lyrics took the place of the queue");
        s.toggle_queue();
        assert!(s.queue_open && !s.lyrics_open, "and the queue took it back");

        let mut wide = init();
        wide.narrow_window.set(false);
        wide.toggle_queue();
        wide.toggle_lyrics();
        assert!(wide.queue_open && wide.lyrics_open, "a wide window shows both");
    }
}

