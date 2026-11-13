//! Local music files: scanning the user's folders, tagging them, and the library index.
//!
//! A local track is identified by a stable hash of its absolute path with the top bit set, so
//! local and TIDAL ids can never collide (TIDAL's ids are far below `1 << 63`). The [`Library`]
//! holds every scanned track, grouped into albums and artists for the Local files page.
//!
//! Scanning runs on its own thread and reports progress through a channel; the caller wakes the
//! UI through Rune's [`Waker`]. The index is cached in `$XDG_CONFIG_HOME/tsun/local_index.json`
//! keyed by path + mtime + size, so a restart only has to stat the files. Covers are decoded on
//! the same thread into small JPEG thumbnails under `$XDG_CACHE_HOME/tsun/local-covers/`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::UNIX_EPOCH;

use lofty::file::{AudioFile, FileType as LoftyFileType, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use serde::{Deserialize, Serialize};

use rune::Waker;

use crate::tidal::Track;

/// The bit that marks a locally scanned track id. TIDAL's own ids are positive integers well
/// below it, so the two spaces never overlap.
pub(crate) const LOCAL_ID_BIT: u64 = 1 << 63;

/// The extensions the scanner accepts. Symphonia 0.5 has no pure-Rust Opus decoder, so `.opus`
/// is deliberately absent (see the README).
pub(crate) const EXTENSIONS: &[&str] =
    &["flac", "mp3", "m4a", "mp4", "aac", "ogg", "oga", "wav", "aiff", "aif"];

/// Longest edge of the cached cover thumbnails.
const THUMB_PX: u32 = 480;

/// The tabs of the Local files page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum LocalTab {
    #[default]
    Tracks,
    Albums,
    Artists,
    Folders,
}

/// How the Local files page orders its lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) enum LocalSort {
    #[default]
    Artist,
    Title,
    Album,
    Year,
}

impl LocalSort {
    pub(crate) fn label(self) -> &'static str {
        match self {
            LocalSort::Artist => "Artist",
            LocalSort::Title => "Title",
            LocalSort::Album => "Album",
            LocalSort::Year => "Year",
        }
    }

    pub(crate) fn next(self) -> LocalSort {
        match self {
            LocalSort::Artist => LocalSort::Title,
            LocalSort::Title => LocalSort::Album,
            LocalSort::Album => LocalSort::Year,
            LocalSort::Year => LocalSort::Artist,
        }
    }
}

/// One scanned file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalTrack {
    pub(crate) id: u64,
    pub(crate) path: PathBuf,
    pub(crate) title: String,
    /// The artist tag as written, which may be empty.
    pub(crate) artist: String,
    pub(crate) album_artist: String,
    pub(crate) album: String,
    pub(crate) track_number: Option<u32>,
    pub(crate) disc_number: Option<u32>,
    pub(crate) year: Option<String>,
    pub(crate) duration: Option<u64>,
    /// `FLAC`, `MP3`, `AAC`, `Vorbis`, `WAV`, `AIFF`, …
    pub(crate) codec: String,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) bit_depth: Option<u32>,
    pub(crate) bitrate_kbps: Option<u32>,
    /// Linear ReplayGain multiplier from the track-gain tag, when present.
    pub(crate) replaygain: Option<f32>,
    /// `file:<thumb path>` when artwork was embedded (or a folder image was found).
    pub(crate) cover: Option<String>,
    pub(crate) mtime: i64,
    pub(crate) size: u64,
}

impl LocalTrack {
    /// The artist shown and scrobbled: the artist tag, falling back to the album artist.
    pub(crate) fn display_artist(&self) -> &str {
        if !self.artist.trim().is_empty() {
            &self.artist
        } else if !self.album_artist.trim().is_empty() {
            &self.album_artist
        } else {
            ""
        }
    }

    /// The artist line, or `"Unknown artist"` when the file has neither tag.
    pub(crate) fn artist_or_unknown(&self) -> String {
        let artist = self.display_artist();
        if artist.is_empty() {
            "Unknown artist".to_owned()
        } else {
            artist.to_owned()
        }
    }

    pub(crate) fn album_or_unknown(&self) -> String {
        if self.album.trim().is_empty() {
            "Unknown album".to_owned()
        } else {
            self.album.clone()
        }
    }

    /// The quality label the player bar shows, e.g. `FLAC 24-bit/96 kHz` or `MP3 320 kbps`.
    pub(crate) fn quality_label(&self) -> String {
        let lossless = matches!(self.codec.as_str(), "FLAC" | "WAV" | "AIFF" | "ALAC");
        let mut label = self.codec.clone();
        if lossless {
            if let Some(bits) = self.bit_depth {
                label.push_str(&format!(" {bits}-bit"));
            }
            if let Some(rate) = self.sample_rate {
                label.push('/');
                label.push_str(&crate::audio::format_khz(rate));
                label.push_str(" kHz");
            }
        } else if let Some(kbps) = self.bitrate_kbps {
            label.push_str(&format!(" {kbps} kbps"));
        } else if let Some(rate) = self.sample_rate {
            label.push_str(&format!("/{} kHz", crate::audio::format_khz(rate)));
        }
        label
    }

    /// The shared [`Track`] the queue, player bar and history use.
    pub(crate) fn track(&self) -> Track {
        Track {
            id: self.id,
            title: self.title.clone(),
            artist: self.artist_or_unknown_fallback(),
            album: self.album.clone(),
            cover: self.cover.clone(),
            duration: self.duration,
            album_id: None,
            artist_id: None,
            explicit: false,
            quality: None,
            track_number: self.track_number,
        }
    }

    /// The artist for the shared `Track`, empty when the file carries neither tag (Last.fm then
    /// skips it instead of inventing an artist name).
    fn artist_or_unknown_fallback(&self) -> String {
        self.display_artist().to_owned()
    }
}

/// One album in the local index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalAlbum {
    /// Stable key used in routes and page lookups.
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) year: Option<String>,
    pub(crate) cover: Option<String>,
    pub(crate) duration: Option<u64>,
    /// Indices into [`Library::tracks`], in disc/track order.
    pub(crate) tracks: Vec<usize>,
}

/// One artist in the local index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalArtist {
    pub(crate) name: String,
    /// Indices into [`Library::tracks`], in album/disc/track order.
    pub(crate) tracks: Vec<usize>,
    /// Keys into [`Library::albums`].
    pub(crate) albums: Vec<String>,
}

/// The scanned library.
#[derive(Debug, Clone, Default)]
pub(crate) struct Library {
    pub(crate) tracks: Vec<LocalTrack>,
    pub(crate) albums: Vec<LocalAlbum>,
    pub(crate) artists: Vec<LocalArtist>,
    /// The folders that were scanned.
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) by_id: HashMap<u64, usize>,
}

impl Library {
    pub(crate) fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub(crate) fn track(&self, id: u64) -> Option<&LocalTrack> {
        self.by_id.get(&id).and_then(|index| self.tracks.get(*index))
    }

    pub(crate) fn album(&self, key: &str) -> Option<&LocalAlbum> {
        self.albums.iter().find(|album| album.key == key)
    }

    pub(crate) fn artist(&self, name: &str) -> Option<&LocalArtist> {
        self.artists.iter().find(|artist| artist.name == name)
    }

    /// The shared tracks for `indices`.
    pub(crate) fn track_list(&self, indices: &[usize]) -> Vec<Track> {
        indices
            .iter()
            .filter_map(|index| self.tracks.get(*index))
            .map(LocalTrack::track)
            .collect()
    }

    pub(crate) fn album_tracks(&self, key: &str) -> Vec<Track> {
        self.album(key).map(|album| self.track_list(&album.tracks)).unwrap_or_default()
    }

    pub(crate) fn artist_tracks(&self, name: &str) -> Vec<Track> {
        self.artist(name).map(|artist| self.track_list(&artist.tracks)).unwrap_or_default()
    }

    /// Every file under `dir` (recursively) as track indices, used by the Folders tab.
    pub(crate) fn tracks_under(&self, dir: &Path) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.path.starts_with(dir))
            .map(|(index, _)| index)
            .collect()
    }

    /// Every file directly inside `dir`.
    pub(crate) fn tracks_in(&self, dir: &Path) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.path.parent() == Some(dir))
            .map(|(index, _)| index)
            .collect()
    }

    /// The immediate child folders of `dir` that contain at least one scanned file.
    pub(crate) fn subfolders(&self, dir: &Path) -> Vec<PathBuf> {
        let mut folders: Vec<PathBuf> = self
            .tracks
            .iter()
            .filter_map(|track| track.path.parent())
            .filter(|parent| parent != &dir && parent.starts_with(dir))
            .filter_map(|parent| {
                let rest = parent.strip_prefix(dir).ok()?;
                let first = rest.components().next()?;
                Some(dir.join(first.as_os_str()))
            })
            .collect();
        folders.sort();
        folders.dedup();
        folders
    }

    /// Builds the index from scanned tracks and the configured roots.
    fn build(tracks: Vec<LocalTrack>, mut roots: Vec<PathBuf>) -> Library {
        let mut tracks = tracks;
        tracks.sort_by(|a, b| {
            let left = (
                a.display_artist().to_lowercase(),
                a.album.to_lowercase(),
                a.disc_number.unwrap_or(0),
                a.track_number.unwrap_or(0),
                a.title.to_lowercase(),
            );
            let right = (
                b.display_artist().to_lowercase(),
                b.album.to_lowercase(),
                b.disc_number.unwrap_or(0),
                b.track_number.unwrap_or(0),
                b.title.to_lowercase(),
            );
            left.cmp(&right)
        });

        let mut by_id = HashMap::with_capacity(tracks.len());
        for (index, track) in tracks.iter().enumerate() {
            by_id.entry(track.id).or_insert(index);
        }

        // Albums, in encounter order of the (already sorted) tracks.
        let mut album_order: Vec<String> = Vec::new();
        let mut albums: HashMap<String, LocalAlbum> = HashMap::new();
        for (index, track) in tracks.iter().enumerate() {
            let artist = track.artist_or_unknown();
            let key = album_key(&artist, &track.album_or_unknown());
            let album = albums.entry(key.clone()).or_insert_with(|| {
                album_order.push(key.clone());
                LocalAlbum {
                    key: key.clone(),
                    title: track.album_or_unknown(),
                    artist: artist.clone(),
                    year: track.year.clone(),
                    cover: track.cover.clone(),
                    duration: None,
                    tracks: Vec::new(),
                }
            });
            album.tracks.push(index);
            album.duration = Some(album.duration.unwrap_or(0) + track.duration.unwrap_or(0));
            if album.cover.is_none() {
                album.cover = track.cover.clone();
            }
        }
        let albums: Vec<LocalAlbum> = album_order
            .into_iter()
            .filter_map(|key| albums.remove(&key))
            .collect();

        // Artists.
        let mut artist_order: Vec<String> = Vec::new();
        let mut artists: HashMap<String, LocalArtist> = HashMap::new();
        for (index, track) in tracks.iter().enumerate() {
            let name = track.artist_or_unknown();
            let artist = artists.entry(name.clone()).or_insert_with(|| {
                artist_order.push(name.clone());
                LocalArtist { name: name.clone(), tracks: Vec::new(), albums: Vec::new() }
            });
            artist.tracks.push(index);
            let key = album_key(&name, &track.album_or_unknown());
            if !artist.albums.contains(&key) {
                artist.albums.push(key);
            }
        }
        let artists: Vec<LocalArtist> = artist_order
            .into_iter()
            .filter_map(|name| artists.remove(&name))
            .collect();

        roots.sort();
        roots.dedup();
        Library { tracks, albums, artists, roots, by_id }
    }
}

/// The key an album is grouped and routed by.
pub(crate) fn album_key(artist: &str, album: &str) -> String {
    format!("{artist}\u{1}{album}")
}

/// Whether `id` names a locally scanned track.
pub(crate) fn is_local_id(id: u64) -> bool {
    id & LOCAL_ID_BIT != 0
}

/// The stable id of the file at `path`: FNV-1a over the absolute path, with the local bit set.
pub(crate) fn local_id(path: &Path) -> u64 {
    let absolute = absolute_path(path);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in absolute.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | LOCAL_ID_BIT
}

/// `path` made absolute (without requiring it to exist).
pub(crate) fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map(|dir| dir.join(path)).unwrap_or_else(|_| path.to_path_buf())
}

/// What a scan reports back to the UI.
#[derive(Debug)]
pub(crate) enum ScanMsg {
    /// Files found and how many have been indexed so far.
    Progress { done: usize, total: usize },
    /// The finished index. `skipped` counts files whose tags could not be read.
    Finished { library: Box<Library>, skipped: usize },
}

/// Scans `folders` on a background thread, honouring the index cache.
///
/// The returned receiver yields progress and then exactly one [`ScanMsg::Finished`]. A scan with
/// no folders finishes immediately with an empty library.
pub(crate) fn start_scan(folders: Vec<PathBuf>, waker: Waker) -> Receiver<ScanMsg> {
    let (tx, rx) = mpsc::channel();
    let roots: Vec<PathBuf> = folders.into_iter().map(|path| absolute_path(&path)).collect();
    thread::Builder::new()
        .name("tsun-local-scan".to_owned())
        .spawn(move || {
            let cache = load_cache();
            let covers = cover_dir();
            let (library, skipped, cache) = scan(&roots, cache, &covers, &tx, &waker);
            save_cache(&cache);
            let _ = tx.send(ScanMsg::Finished { library: Box::new(library), skipped });
            waker.as_ref()();
        })
        .expect("local scanner thread");
    rx
}

/// Scans `roots` with `cache` as the incremental starting point and returns the index, the
/// number of unreadable files, and the refreshed cache. Split from [`start_scan`] so tests can
/// run it without touching `$XDG_CONFIG_HOME`.
fn scan(
    roots: &[PathBuf],
    mut cache: HashMap<String, CachedTrack>,
    covers: &Path,
    tx: &mpsc::Sender<ScanMsg>,
    waker: &Waker,
) -> (Library, usize, HashMap<String, CachedTrack>) {
    let mut files: Vec<PathBuf> = Vec::new();
    for root in roots {
        walk(root, &mut files);
    }
    files.sort();
    files.dedup();
    let total = files.len();
    let mut tracks: Vec<LocalTrack> = Vec::with_capacity(total);
    let mut skipped = 0usize;
    let mut done = 0usize;
    for path in &files {
        done += 1;
        match read_track(path, &cache, covers) {
            Some(track) => {
                cache.insert(track.path.to_string_lossy().into_owned(), CachedTrack::from(&track));
                tracks.push(track);
            }
            None => skipped += 1,
        }
        if done % 25 == 0 || done == total {
            let _ = tx.send(ScanMsg::Progress { done, total });
            waker.as_ref()();
        }
    }
    (Library::build(tracks, roots.to_vec()), skipped, cache)
}

/// Recursively collects playable files, skipping hidden directories and symlinks.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let hidden = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'));
            if !hidden {
                walk(&path, files);
            }
        } else if file_type.is_file() && is_music_file(&path) {
            files.push(path);
        }
    }
}

pub(crate) fn is_music_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Reads one file, reusing the cached tags when path, mtime and size all match.
fn read_track(path: &Path, cache: &HashMap<String, CachedTrack>, covers: &Path) -> Option<LocalTrack> {
    let key = path.to_string_lossy().into_owned();
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mtime = mtime_of(&metadata);
    let size = metadata.len();
    if let Some(cached) = cache.get(&key) {
        if cached.mtime == mtime && cached.size == size {
            if let Some(track) = cached.to_track(path) {
                return Some(track);
            }
        }
    }
    parse_file(path, mtime, size, covers)
}

fn mtime_of(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Reads a file's tags, audio properties and artwork. `None` when lofty cannot parse it (the
/// scanner then reports it as skipped rather than failing the whole scan).
fn parse_file(path: &Path, mtime: i64, size: u64, covers: &Path) -> Option<LocalTrack> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let props = tagged.properties();
    let duration = props.duration().as_secs();
    let sample_rate = props.sample_rate().filter(|rate| *rate > 0);
    let bit_depth = props.bit_depth().map(u32::from);
    let bitrate_kbps = props.audio_bitrate();

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let title = tag
        .and_then(Accessor::title)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .to_owned()
        });
    let artist = tag.and_then(Accessor::artist).map(|value| value.trim().to_owned()).unwrap_or_default();
    let album_artist = tag
        .and_then(|tag| tag.get_string(&ItemKey::AlbumArtist))
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    let album = tag.and_then(Accessor::album).map(|value| value.trim().to_owned()).unwrap_or_default();
    let track_number = tag.and_then(Accessor::track);
    let disc_number = tag.and_then(Accessor::disk);
    let year = tag.and_then(Accessor::year).map(|year| year.to_string());
    let replaygain = tag
        .and_then(|tag| tag.get_string(&ItemKey::ReplayGainTrackGain))
        .and_then(parse_replaygain);

    let id = local_id(path);
    let codec = codec_label(tagged.file_type(), path);
    let embedded = tag
        .and_then(|tag| tag.pictures().first())
        .map(|picture| picture.data().to_vec());
    let cover = cover_thumb(id, embedded, path, covers);

    Some(LocalTrack {
        id,
        path: absolute_path(path),
        title,
        artist,
        album_artist,
        album,
        track_number,
        disc_number,
        year,
        duration: (duration > 0).then_some(duration),
        codec,
        sample_rate,
        bit_depth,
        bitrate_kbps,
        replaygain,
        cover,
        mtime,
        size,
    })
}

/// Parses a ReplayGain gain string (`-7.23 dB`, `+3.5 dB`, or a bare number) into a linear
/// multiplier.
pub(crate) fn parse_replaygain(value: &str) -> Option<f32> {
    let number = value.split_whitespace().next()?.parse::<f32>().ok()?;
    let linear = 10f32.powf(number / 20.0);
    linear.is_finite().then_some(linear.clamp(0.0, 4.0))
}

fn codec_label(file_type: LoftyFileType, path: &Path) -> String {
    match file_type {
        LoftyFileType::Flac => "FLAC".to_owned(),
        LoftyFileType::Mpeg => "MP3".to_owned(),
        LoftyFileType::Mp4 => "M4A".to_owned(),
        LoftyFileType::Aac => "AAC".to_owned(),
        LoftyFileType::Vorbis => "Vorbis".to_owned(),
        LoftyFileType::Wav => "WAV".to_owned(),
        LoftyFileType::Aiff => "AIFF".to_owned(),
        LoftyFileType::Opus => "Opus".to_owned(),
        _ => {
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_ascii_uppercase();
            if extension.is_empty() { "Audio".to_owned() } else { extension }
        }
    }
}

/// Writes (or reuses) the 480px cover thumbnail for `id`, from the embedded picture or a
/// `folder.jpg`/`cover.jpg`/`front.jpg` beside the file. `None` when there is no artwork.
fn cover_thumb(id: u64, embedded: Option<Vec<u8>>, path: &Path, covers: &Path) -> Option<String> {
    let destination = covers.join(format!("{id}.jpg"));
    if fs::metadata(&destination).is_ok_and(|metadata| metadata.len() > 0) {
        return Some(format!("file:{}", destination.display()));
    }
    let bytes = match embedded.filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => bytes,
        None => folder_image(path)?,
    };
    write_thumb(&bytes, &destination)?;
    Some(format!("file:{}", destination.display()))
}

/// Decodes `bytes` and writes a JPEG thumbnail no larger than [`THUMB_PX`] stored at
/// `destination`.
fn write_thumb(bytes: &[u8], destination: &Path) -> Option<()> {
    let image = image::load_from_memory(bytes).ok()?;
    let thumb = image.thumbnail(THUMB_PX, THUMB_PX);
    let mut encoded = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut encoded);
    thumb.write_to(&mut cursor, image::ImageFormat::Jpeg).ok()?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    fs::write(destination, &encoded).ok()?;
    Some(())
}

fn folder_image(path: &Path) -> Option<Vec<u8>> {
    let dir = path.parent()?;
    for name in ["folder.jpg", "cover.jpg", "front.jpg", "Folder.jpg", "Cover.jpg"] {
        let candidate = dir.join(name);
        if let Ok(bytes) = fs::read(&candidate) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
    }
    None
}

/// `local-covers` in [`crate::paths::cache_dir`]; a temp directory when the environment names
/// no cache home.
pub(crate) fn cover_dir() -> PathBuf {
    crate::paths::cache_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("tsun"))
        .join("local-covers")
}

// ---------------------------------------------------------------------------------------------
// Index cache

#[derive(Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<CachedTrack>,
}

const CACHE_VERSION: u32 = 1;

/// One cached tag set. The cover path is recomputed from the id, so a cache entry does not pin
/// a stale thumbnail path.
#[derive(Clone, Serialize, Deserialize)]
struct CachedTrack {
    path: String,
    mtime: i64,
    size: u64,
    title: String,
    artist: String,
    album_artist: String,
    album: String,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    year: Option<String>,
    duration: Option<u64>,
    codec: String,
    sample_rate: Option<u32>,
    bit_depth: Option<u32>,
    bitrate_kbps: Option<u32>,
    replaygain: Option<f32>,
    cover: Option<String>,
}

impl CachedTrack {
    fn from(track: &LocalTrack) -> CachedTrack {
        CachedTrack {
            path: track.path.to_string_lossy().into_owned(),
            mtime: track.mtime,
            size: track.size,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album_artist: track.album_artist.clone(),
            album: track.album.clone(),
            track_number: track.track_number,
            disc_number: track.disc_number,
            year: track.year.clone(),
            duration: track.duration,
            codec: track.codec.clone(),
            sample_rate: track.sample_rate,
            bit_depth: track.bit_depth,
            bitrate_kbps: track.bitrate_kbps,
            replaygain: track.replaygain,
            cover: track.cover.clone(),
        }
    }

    /// Rebuilds a track, dropping a cached cover whose thumbnail no longer exists.
    fn to_track(&self, path: &Path) -> Option<LocalTrack> {
        let id = local_id(path);
        let cover = match &self.cover {
            Some(cover) => {
                let file = cover.strip_prefix("file:")?;
                fs::metadata(file).ok().filter(|metadata| metadata.len() > 0)?;
                Some(cover.clone())
            }
            None => None,
        };
        Some(LocalTrack {
            id,
            path: absolute_path(path),
            title: self.title.clone(),
            artist: self.artist.clone(),
            album_artist: self.album_artist.clone(),
            album: self.album.clone(),
            track_number: self.track_number,
            disc_number: self.disc_number,
            year: self.year.clone(),
            duration: self.duration,
            codec: self.codec.clone(),
            sample_rate: self.sample_rate,
            bit_depth: self.bit_depth,
            bitrate_kbps: self.bitrate_kbps,
            replaygain: self.replaygain,
            cover,
            mtime: self.mtime,
            size: self.size,
        })
    }
}

fn index_path() -> Option<PathBuf> {
    Some(crate::paths::config_dir()?.join("local_index.json"))
}

fn load_cache() -> HashMap<String, CachedTrack> {
    let Some(path) = index_path() else { return HashMap::new() };
    let Ok(text) = fs::read_to_string(path) else { return HashMap::new() };
    let Ok(file) = serde_json::from_str::<CacheFile>(&text) else { return HashMap::new() };
    if file.version != CACHE_VERSION {
        return HashMap::new();
    }
    file.entries.into_iter().map(|entry| (entry.path.clone(), entry)).collect()
}

fn save_cache(entries: &HashMap<String, CachedTrack>) {
    let Some(path) = index_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let mut list: Vec<&CachedTrack> = entries.values().collect();
    list.sort_by(|a, b| a.path.cmp(&b.path));
    let file = CacheFile {
        version: CACHE_VERSION,
        entries: list.into_iter().cloned().collect(),
    };
    let Ok(text) = serde_json::to_vec(&file) else { return };
    let _ = crate::paths::write_private(&path, &text);
}

/// Loads the cached index written by the last scan, without touching the filesystem beyond the
/// cache file itself. Used at startup so the page is populated before the refresh scan finishes.
pub(crate) fn load_index(roots: Vec<PathBuf>) -> Library {
    let cache = load_cache();
    let tracks: Vec<LocalTrack> = cache
        .values()
        .filter_map(|entry| entry.to_track(Path::new(&entry.path)))
        .collect();
    Library::build(tracks, roots)
}

/// Whether `folder` is an existing directory (the Settings path box's validation).
pub(crate) fn is_folder(folder: &str) -> bool {
    let trimmed = folder.trim();
    !trimmed.is_empty() && Path::new(trimmed).is_dir()
}

/// The playable files named on the command line (`tsun song.flac`).
pub(crate) fn cli_audio_files() -> Vec<PathBuf> {
    std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|path| path.is_file() && is_music_file(path))
        .collect()
}

/// Reads one file's tags for a one-off play (a command-line argument or a drop), without going
/// through the full library scan.
pub(crate) fn describe(path: &Path) -> Option<LocalTrack> {
    if !is_music_file(path) {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    parse_file(path, mtime_of(&metadata), metadata.len(), &cover_dir())
}

/// Reveals `path` in the desktop's file manager: Finder, Explorer, or
/// `org.freedesktop.FileManager1.ShowItems` with `xdg-open` on the parent folder as the
/// fallback. Runs on its own thread so a slow bus or launcher never stalls the frame.
pub(crate) fn show_in_file_manager(path: PathBuf) {
    thread::Builder::new()
        .name("tsun-file-manager".to_owned())
        .spawn(move || {
            #[cfg(target_os = "macos")]
            let _ = std::process::Command::new("open").arg("-R").arg(&path).spawn();
            // Explorer wants `/select,"<path>"` verbatim, not the whole argument quoted.
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let mut select = std::ffi::OsString::from("/select,\"");
                select.push(&path);
                select.push("\"");
                let _ = std::process::Command::new("explorer").raw_arg(select).spawn();
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            {
                #[cfg(target_os = "linux")]
                if reveal_in_file_manager(&path).is_ok() {
                    return;
                }
                let folder = if path.is_dir() {
                    path
                } else {
                    path.parent().map(Path::to_path_buf).unwrap_or(path)
                };
                let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
            }
        })
        .ok();
}

#[cfg(target_os = "linux")]
fn reveal_in_file_manager(path: &Path) -> Result<(), zbus::Error> {
    let connection = zbus::blocking::Connection::session()?;
    let uri = file_uri(path);
    connection.call_method(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        Some("org.freedesktop.FileManager1"),
        "ShowItems",
        &(vec![uri], ""),
    )?;
    Ok(())
}

/// An absolute `file://` URI, percent-encoding everything outside the URI path safe set.
pub(crate) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in absolute_path(path).to_string_lossy().bytes() {
        let safe = byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b'+' | b'@');
        if safe {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// `file:///path` for a `file:<path>` cover id, or `None` for any other id.
pub(crate) fn cover_file_uri(id: &str) -> Option<String> {
    id.strip_prefix("file:").map(|path| file_uri(Path::new(path)))
}

/// Writes tags on a generated test file through the same lofty path the scanner reads.
#[cfg(test)]
pub(crate) fn write_test_tags(path: &Path, title: &str, artist: &str, album: &str) -> bool {
    use lofty::config::WriteOptions;
    use lofty::tag::{Tag, TagType};
    let Ok(mut tagged) = Probe::open(path).and_then(|probe| probe.read()) else { return false };
    if tagged.primary_tag_mut().is_none() {
        tagged.insert_tag(Tag::new(TagType::Id3v2));
    }
    let Some(tag) = tagged.primary_tag_mut() else { return false };
    tag.set_title(title.to_owned());
    tag.set_artist(artist.to_owned());
    tag.set_album(album.to_owned());
    tagged.save_to_path(path, WriteOptions::default()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tsun-local-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal valid 16-bit mono 8 kHz PCM WAV with `seconds` of silence.
    fn write_wav(path: &Path, seconds: u32) {
        let rate: u32 = 8000;
        let samples = rate * seconds;
        let data_len = samples * 2;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, data_len as usize));
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn local_ids_are_stable_and_never_collide_with_tidal_ids() {
        let a = local_id(Path::new("/music/a.flac"));
        let b = local_id(Path::new("/music/a.flac"));
        let c = local_id(Path::new("/music/b.flac"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(is_local_id(a));
        assert!(!is_local_id(12345));
        assert!(!is_local_id(0));
        // A hash whose low bits are all zero still sets the top bit.
        assert_eq!(local_id(Path::new("/")) & LOCAL_ID_BIT, LOCAL_ID_BIT);
    }

    #[test]
    fn replaygain_parses_db_and_gain() {
        assert!((parse_replaygain("-6.02 dB").unwrap() - 0.5).abs() < 0.001);
        assert!((parse_replaygain("+6.0206 dB").unwrap() - 2.0).abs() < 0.01);
        assert_eq!(parse_replaygain("nonsense"), None);
    }

    #[test]
    fn scanner_reads_generated_wav_and_groups_albums() {
        let dir = temp_dir("scan");
        let album = dir.join("Album");
        fs::create_dir_all(&album).unwrap();
        let one = album.join("01 - One.wav");
        let two = album.join("02 - Two.wav");
        write_wav(&one, 1);
        write_wav(&two, 2);
        assert!(write_test_tags(&one, "One", "The Artist", "First Album"));
        assert!(write_test_tags(&two, "Two", "The Artist", "First Album"));

        let (rx, _tx) = mpsc::channel();
        let waker: Waker = std::sync::Arc::new(|| {});
        let library =
            scan(std::slice::from_ref(&dir), HashMap::new(), &dir.join("covers"), &rx, &waker).0;
        assert_eq!(library.tracks.len(), 2);
        assert_eq!(library.albums.len(), 1);
        assert_eq!(library.artists.len(), 1);
        assert_eq!(library.albums[0].title, "First Album");
        assert_eq!(library.albums[0].artist, "The Artist");
        assert_eq!(library.albums[0].tracks.len(), 2);
        let first = &library.tracks[0];
        assert!(library.track(first.id).is_some());
        assert!(library.track(42).is_none());
        assert_eq!(first.title, "One");
        assert_eq!(first.display_artist(), "The Artist");
        assert_eq!(first.codec, "WAV");
        assert!(first.duration.is_some());
        let shared = first.track();
        assert!(is_local_id(shared.id));
        assert_eq!(shared.artist, "The Artist");

        fs::remove_dir_all(&dir).ok();
    }

    /// A FLAC generated with ffmpeg (when it is installed) and re-tagged through lofty is read
    /// with its duration, rate, depth and codec.
    #[test]
    fn scanner_reads_a_generated_flac() {
        let available = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !available {
            eprintln!("skipping the FLAC scanner test: ffmpeg is not installed");
            return;
        }
        let dir = temp_dir("flac");
        let file = dir.join("Song.flac");
        let status = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=1")
            .args(["-ar", "44100", "-ac", "2"])
            .arg(&file)
            .status()
            .expect("run ffmpeg");
        assert!(status.success(), "ffmpeg generated the FLAC");
        assert!(write_test_tags(&file, "Lofty Title", "Lofty Artist", "Lofty Album"));
        let track = parse_file(&file, 1, 2, &dir.join("covers")).unwrap();
        assert_eq!(track.codec, "FLAC");
        assert_eq!(track.title, "Lofty Title");
        assert_eq!(track.artist, "Lofty Artist");
        assert_eq!(track.album, "Lofty Album");
        assert_eq!(track.sample_rate, Some(44100));
        assert!(track.duration.is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hidden_directories_are_skipped() {
        let dir = temp_dir("hidden");
        let hidden = dir.join(".git");
        fs::create_dir_all(&hidden).unwrap();
        write_wav(&dir.join("kept.wav"), 1);
        write_wav(&hidden.join("ignored.wav"), 1);
        let (rx, _tx) = mpsc::channel();
        let waker: Waker = std::sync::Arc::new(|| {});
        let library =
            scan(std::slice::from_ref(&dir), HashMap::new(), &dir.join("covers"), &rx, &waker).0;
        assert_eq!(library.tracks.len(), 1);
        assert_eq!(library.tracks[0].path.file_name().unwrap(), "kept.wav");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_art_becomes_a_jpeg_thumbnail() {
        let dir = temp_dir("thumb");
        let bytes = {
            let image = image::RgbImage::from_pixel(600, 300, image::Rgb([200, 30, 10]));
            let mut encoded = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
                .unwrap();
            encoded
        };
        fs::write(dir.join("folder.jpg"), &bytes).unwrap();
        let file = dir.join("track.wav");
        write_wav(&file, 1);
        let destination = dir.join("thumb.jpg");
        write_thumb(&bytes, &destination).unwrap();
        let decoded = image::open(&destination).unwrap();
        assert!(decoded.width() <= THUMB_PX && decoded.height() <= THUMB_PX);
        let track = parse_file(&file, 1, 2, &dir.join("covers")).unwrap();
        let cover = track.cover.expect("folder.jpg becomes the cover");
        let path = PathBuf::from(cover.strip_prefix("file:").unwrap());
        assert!(path.is_file());
        assert!(image::open(path).is_ok());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_round_trips_through_json() {
        let dir = temp_dir("cache");
        let file = dir.join("x.wav");
        write_wav(&file, 1);
        let track = parse_file(&file, 1, 2, &dir.join("covers")).unwrap();
        let cached = CachedTrack::from(&track);
        let text = serde_json::to_string(&cached).unwrap();
        let decoded: CachedTrack = serde_json::from_str(&text).unwrap();
        let again = decoded.to_track(&file).unwrap();
        assert_eq!(again.id, track.id);
        assert_eq!(again.title, track.title);
        assert_eq!(again.duration, track.duration);
        fs::remove_dir_all(&dir).ok();
    }
}
