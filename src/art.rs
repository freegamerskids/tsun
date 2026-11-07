//! Album artwork: fetch TIDAL cover images off the UI thread and decode them through Rune's
//! [`ImageCache`].
//!
//! [`Track::cover`](crate::tidal::Track) carries the raw TIDAL cover UUID. [`Artwork::request`]
//! turns it into a `resources.tidal.com` URL, a small pool of background threads downloads the
//! bytes, and every finished download wakes the app so [`Artwork::poll`] can hand the bytes to
//! [`ImageCache::load`]. Decoding and mip generation then happen on the cache's own workers,
//! which wake the app again through the same [`Waker`]. Nothing here ever blocks a frame, and
//! dropping the `Artwork` shuts the fetch threads down.
//!
//! The signed-in user's avatar is fetched the same way: [`Artwork::request_profile`] takes the
//! raw picture UUID from [`Profile::picture`](crate::tidal::Profile::picture), builds the
//! `resources.tidal.com` avatar URL, and shares the same cache, so the decoded image is read
//! back with [`Artwork::get`] using that picture UUID as the key.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rune::{Image, ImageCache, Waker};

/// Covers are served at fixed sizes; the sidebar and list rows use the 160px square.
const COVER_PX: u32 = 160;
/// The square sizes TIDAL stores for every image (covers, avatars, artist
/// pictures). [`Artwork::request_cover_sized`] rejects anything else.
const ALLOWED_PX: [u32; 6] = [80, 160, 320, 480, 640, 750];
/// Profile avatars are shown in the top-right menu. 320px stays crisp on HiDPI and is one of
/// the sizes TIDAL stores for user and artist pictures (160/320/480/750).
const AVATAR_PX: u32 = 320;
/// Fetch threads. Covers are tiny and a search page is bounded, so a few is plenty.
const FETCHERS: usize = 3;
/// Longest a single cover fetch may take, so a stalled request cannot pin a worker.
const TIMEOUT: Duration = Duration::from_secs(8);
/// Hard cap on one response body. A 160x160 JPEG is a few KB; anything near this is wrong.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Byte budget for decoded pixels held by the cache. A 160px cover decodes to ~100 KiB and a
/// 320px avatar or tile to ~400 KiB, so this keeps several hundred covers plus the pinned
/// images — comfortably more than a deep Collection page draws at once — without letting a
/// long session hold every cover it ever saw.
const CACHE_BYTES: usize = 96 * 1024 * 1024;
/// Hard ceiling on tracked ids. Entries that hold no pixels yet (in flight or failed) also
/// count against this, so a flood of bad ids cannot grow the map without bound.
const MAX_ENTRIES: usize = 2048;

const USER_AGENT: &str = "tsun/0.1 (TIDAL desktop client)";

/// What is known about one cover id.
enum Entry {
    /// Requested; the bytes are still being fetched.
    Pending,
    /// Bytes loaded into the cache and handed to its decoder.
    Decoding(Image),
    /// Decoded and ready to draw.
    Ready(Image),
    /// The fetch or decode failed. Never retried, so a dead cover cannot spam the network.
    Failed,
}

/// Where one cover's bytes come from.
enum JobSource {
    /// A `https://` TIDAL image URL.
    Url(String),
    /// A local thumbnail produced by the local-file scanner (`file:<path>` ids).
    File(std::path::PathBuf),
}

/// One cover to fetch, resolved on the UI thread.
struct Job {
    id: String,
    source: JobSource,
}

/// A finished download, delivered back to the UI thread by [`Artwork::poll`].
enum Msg {
    Bytes { id: String, bytes: Vec<u8> },
    Failed { id: String },
}

/// Fetches and caches TIDAL cover art for one app run.
///
/// Create it once in `AppSpec::setup` with the [`Waker`] Rune supplies, call [`request`] with
/// every `Track::cover` you might draw, call [`poll`] from `AppSpec::poll` (adding its result to
/// your "changed" flag), and read decoded art with [`get`].
///
/// [`request`]: Artwork::request
/// [`poll`]: Artwork::poll
/// [`get`]: Artwork::get
pub struct Artwork {
    cache: ImageCache,
    /// Jobs to the fetch threads. Dropping it is what stops them.
    jobs: Sender<Job>,
    results: Receiver<Msg>,
    entries: HashMap<String, Entry>,
    /// Last-use tick per cached id, for LRU eviction. A [`Cell`] so [`Artwork::get`] can touch
    /// an entry through `&self`, as the draw path only has `&Ui`.
    used: HashMap<String, Cell<u64>>,
    /// Monotonic tick, bumped on every request or read.
    clock: Cell<u64>,
    /// The signed-in user's avatar id; never evicted.
    profile_pin: Option<String>,
    /// The now-playing cover id; never evicted.
    now_playing_pin: Option<String>,
    /// The ids the frame now on screen drew: the most recent committed view pass's reads through
    /// [`Artwork::get`]. A cover that finishes for an id nothing drew cannot change what is on
    /// screen, so [`Artwork::poll`] does not ask for a repaint for it; it is simply cached for a
    /// row that mounts later.
    shown: RefCell<HashSet<String>>,
    /// The ids the pass since the last poll drew, which becomes `shown` at the next poll.
    pass: RefCell<HashSet<String>>,
    /// Whether a view pass has run since the last poll (and so `pass` is worth taking).
    drew: Cell<bool>,
}

impl Artwork {
    /// Starts the fetch and decode threads. `waker` must be the one from `AppSpec::setup`.
    pub fn new(waker: rune::Waker) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build();
        let agent = ureq::Agent::new_with_config(config);

        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<Msg>();
        // Workers share the receiver; each blocks in `recv` only until the next job arrives,
        // so a batch of covers downloads side by side rather than one at a time.
        let job_rx = Arc::new(Mutex::new(job_rx));
        for index in 0..FETCHERS {
            let agent = agent.clone();
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            let waker = waker.clone();
            thread::Builder::new()
                .name(format!("tsun-artwork-{index}"))
                .spawn(move || fetch_loop(agent, job_rx, result_tx, waker))
                .expect("artwork worker");
        }

        Artwork {
            cache: ImageCache::new(waker),
            jobs,
            results: result_rx,
            entries: HashMap::new(),
            used: HashMap::new(),
            clock: Cell::new(0),
            profile_pin: None,
            now_playing_pin: None,
            shown: RefCell::new(HashSet::new()),
            pass: RefCell::new(HashSet::new()),
            drew: Cell::new(false),
        }
    }

    /// Asks for the cover with TIDAL UUID `id`. Cheap and idempotent: an id already requested
    /// (or known bad) is ignored, and a malformed UUID issues no request at all.
    pub fn request(&mut self, id: &str) {
        // Local covers are `file:<absolute thumbnail path>`; anything else must be a TIDAL
        // image UUID so no arbitrary host or path can be requested through this entry point.
        if let Some(path) = id.strip_prefix("file:") {
            let path = std::path::PathBuf::from(path);
            if path.is_absolute() {
                self.enqueue(id, JobSource::File(path));
            }
            return;
        }
        let Some(url) = cover_url(id) else { return };
        self.enqueue(id, JobSource::Url(url));
    }

    /// Requests a TIDAL-provided mix image without allowing arbitrary hosts.
    pub fn request_mix(&mut self, url: &str) {
        if !url.starts_with("https://images.tidal.com/") { return; }
        self.enqueue(url, JobSource::Url(url.to_owned()));
    }

    /// Asks for an artist picture from the raw picture UUID in
    /// [`Artist::picture`](crate::tidal::Artist::picture), at the same 320px
    /// square used for avatars. Read the decoded image back with
    /// [`Artwork::get`] passing that same UUID.
    pub fn request_artist(&mut self, id: &str) {
        let Some(url) = image_url(id, AVATAR_PX) else { return };
        self.enqueue(id, JobSource::Url(url));
    }

    /// Asks for an album cover art UUID at an explicit square size. `px` must be
    /// one of the sizes TIDAL stores (80, 160, 320, 480, 640, 750); any other
    /// value issues no request.
    ///
    /// The cache is keyed by UUID alone, so requesting the same cover at several
    /// sizes returns whichever size arrived first — pick one size per cover.
    pub fn request_cover_sized(&mut self, id: &str, px: u32) {
        if !ALLOWED_PX.contains(&px) { return; }
        let Some(url) = image_url(id, px) else { return };
        self.enqueue(id, JobSource::Url(url));
    }

    /// Asks for a video's 16:9 thumbnail from the raw image UUID in
    /// [`Video::image`](crate::tidal::Video::image). Read it back with
    /// [`Artwork::get`] using that same UUID. Cheap and idempotent.
    pub fn request_video(&mut self, id: &str) {
        let Some(url) = video_art_url(id) else { return };
        self.enqueue(id, JobSource::Url(url));
    }

    /// Asks for the signed-in user's avatar from the raw picture UUID in
    /// [`Profile::picture`](crate::tidal::Profile::picture).
    ///
    /// Uses the same cache key as [`Artwork::get`]: after [`Artwork::poll`] reports the image
    /// ready, read it back with `get(picture)`, passing that same UUID. Cheap and idempotent;
    /// a malformed UUID issues no request. When `Profile::picture` is `None` there is nothing
    /// to request and the caller draws its own fallback.
    pub fn request_profile(&mut self, id: &str) {
        let Some(url) = profile_url(id) else { return };
        self.enqueue(id, JobSource::Url(url));
    }

    fn enqueue(&mut self, id: &str, source: JobSource) {
        if self.entries.contains_key(id) {
            self.touch(id);
            return;
        }
        self.make_room();
        self.entries.insert(id.to_owned(), Entry::Pending);
        let tick = self.tick();
        self.used.insert(id.to_owned(), Cell::new(tick));
        let _ = self.jobs.send(Job { id: id.to_owned(), source });
    }

    /// Drains finished downloads and decode results. Returns `true` when some cover a surface
    /// drew became drawable since the last call, so the caller can redraw. A cover nothing has
    /// drawn lately (the rows of a long list that are not on screen, say) only lands in the
    /// cache: repainting for it would run the view for a frame nothing changed in.
    pub fn poll(&mut self) -> bool {
        // A view pass since the last poll drew the frame now on screen: its reads are what a
        // finished cover is checked against. While the window is idle no pass runs, and the ids
        // of the screen that is still up stay the ones considered.
        if self.drew.replace(false) {
            let pass = std::mem::take(&mut *self.pass.borrow_mut());
            *self.shown.borrow_mut() = pass;
        }
        let mut became: Vec<String> = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(Msg::Bytes { id, bytes }) => {
                    if let Some(entry @ Entry::Pending) = self.entries.get_mut(&id) {
                        // `load` hashes and queues on the cache's decoder; it never reads
                        // the network or decodes here.
                        *entry = Entry::Decoding(self.cache.load(bytes));
                    }
                }
                Ok(Msg::Failed { id }) => {
                    // A failed cover draws its fallback whether poll noticed or not, so it
                    // never needs a repaint.
                    if let Some(entry @ Entry::Pending) = self.entries.get_mut(&id) {
                        *entry = Entry::Failed;
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        for (id, entry) in self.entries.iter_mut() {
            let next = match entry {
                Entry::Decoding(image) if image.error().is_some() => Some(Entry::Failed),
                Entry::Decoding(image) if image.is_ready() => Some(Entry::Ready(image.clone())),
                _ => None,
            };
            if let Some(next) = next {
                *entry = next;
                became.push(id.clone());
            }
        }
        let shown = self.shown.borrow();
        became.into_iter().any(|id| shown.contains(&id))
    }

    /// Whether `id` has been requested already — in flight, decoded, or failed. The draw path
    /// uses this to note a want only for covers the cache has never seen, so a frame that draws
    /// a row it has drawn before does no work and raises no repaint.
    pub fn knows(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    /// The decoded cover for `id`, or `None` while it is still loading, if it failed, or if it
    /// was never requested. Drawing a placeholder while this is `None` is the fallback.
    pub fn get(&self, id: &str) -> Option<&Image> {
        // The draw path is what decides which covers matter: `poll` repaints for a cover only
        // while the screen that drew it is still the one up.
        if !self.pass.borrow().contains(id) {
            self.pass.borrow_mut().insert(id.to_owned());
        }
        self.drew.set(true);
        match self.entries.get(id) {
            Some(Entry::Ready(image)) => {
                self.touch(id);
                Some(image)
            }
            _ => None,
        }
    }

    /// Marks `id` as the signed-in user's avatar, which eviction never drops. `None` clears it.
    /// Cheap and idempotent, so the app can call it every frame.
    pub fn set_profile(&mut self, id: Option<&str>) {
        if self.profile_pin.as_deref() == id {
            return;
        }
        self.profile_pin = id.map(str::to_owned);
    }

    /// Marks `id` as the now-playing cover, which eviction never drops. `None` clears it.
    pub fn set_now_playing(&mut self, id: Option<&str>) {
        if self.now_playing_pin.as_deref() == id {
            return;
        }
        self.now_playing_pin = id.map(str::to_owned);
    }

    /// Whether `id` is protected from eviction.
    fn is_pinned(&self, id: &str) -> bool {
        self.profile_pin.as_deref() == Some(id) || self.now_playing_pin.as_deref() == Some(id)
    }

    /// Advances the use clock and returns the new tick.
    fn tick(&self) -> u64 {
        let tick = self.clock.get().wrapping_add(1);
        self.clock.set(tick);
        tick
    }

    /// Records a read of `id` for the LRU order.
    fn touch(&self, id: &str) {
        if let Some(used) = self.used.get(id) {
            used.set(self.tick());
        }
    }

    /// Evicts least-recently-used images until a new one fits, both by the pixel budget
    /// ([`CACHE_BYTES`]) and the id ceiling ([`MAX_ENTRIES`]). The pinned avatar and now-playing
    /// cover are skipped; if everything left is pinned, eviction stops rather than looping.
    fn make_room(&mut self) {
        while self.entries.len() >= MAX_ENTRIES || self.decoded_bytes() > CACHE_BYTES {
            let Some(victim) = self.lru_victim() else { break };
            self.entries.remove(&victim);
            self.used.remove(&victim);
        }
    }

    /// The least-recently-used unpinned id, or `None` when only pinned ids remain.
    fn lru_victim(&self) -> Option<String> {
        self.entries
            .keys()
            .filter(|id| !self.is_pinned(id.as_str()))
            .min_by_key(|id| self.used.get(id.as_str()).map_or(0, |used| used.get()))
            .cloned()
    }

    /// The decoded size of every held image. Undecoded and failed entries contribute nothing;
    /// the id ceiling keeps those bounded instead.
    fn decoded_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|entry| match entry {
                Entry::Decoding(image) | Entry::Ready(image) => image.size(),
                Entry::Pending | Entry::Failed => None,
            })
            .map(|[width, height]| width as usize * height as usize * 4)
            .sum()
    }
}

/// Builds the `resources.tidal.com` URL for an image UUID at a square size, or `None` if the id
/// is not a canonical hyphenated UUID. TIDAL addresses every image (covers, avatars) by
/// replacing the UUID's hyphens with `/`.
fn image_url(id: &str, px: u32) -> Option<String> {
    if !is_uuid(id) {
        return None;
    }
    let path = id.replace('-', "/");
    Some(format!("https://resources.tidal.com/images/{path}/{px}x{px}.jpg"))
}

/// `<size>x<size>` URL for an album cover UUID.
fn cover_url(id: &str) -> Option<String> {
    image_url(id, COVER_PX)
}

/// The `resources.tidal.com` URL for an album cover UUID at an explicit square size, for
/// callers outside this module (desktop metadata needs a 320px cover). `None` when the id is
/// not a canonical UUID or the size is not one TIDAL stores.
pub(crate) fn cover_art_url(id: &str, px: u32) -> Option<String> {
    if !ALLOWED_PX.contains(&px) {
        return None;
    }
    image_url(id, px)
}

/// The URL desktop metadata reports for a cover id: a `file://` URI for a local thumbnail and
/// the usual `resources.tidal.com` URL for a TIDAL UUID. Other ids produce no URL.
pub(crate) fn art_url(id: &str, px: u32) -> Option<String> {
    if let Some(uri) = crate::local::cover_file_uri(id) {
        return Some(uri);
    }
    cover_art_url(id, px)
}

/// `<size>x<size>` URL for a user avatar, from the picture UUID in
/// [`Profile::picture`](crate::tidal::Profile::picture).
fn profile_url(id: &str) -> Option<String> {
    image_url(id, AVATAR_PX)
}

/// The `resources.tidal.com` URL for a video's 16:9 thumbnail UUID. TIDAL stores
/// video images at 16:9 sizes (`320x180`, `640x360`, `1280x720`); 640x360 keeps a
/// tile crisp on HiDPI without decoding a full HD JPEG per card.
pub(crate) fn video_art_url(id: &str) -> Option<String> {
    if !is_uuid(id) {
        return None;
    }
    let path = id.replace('-', "/");
    Some(format!("https://resources.tidal.com/images/{path}/640x360.jpg"))
}

/// `8-4-4-4-12` lower- or upper-case hex, nothing else.
fn is_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

/// One fetch thread: wait for a job, download it, hand the bytes (or the failure) back, wake
/// the app. Returns when the `Artwork` is dropped and the job channel closes.
fn fetch_loop(agent: ureq::Agent, jobs: Arc<Mutex<Receiver<Job>>>, results: Sender<Msg>, waker: Waker) {
    loop {
        // The lock guards only `recv`, so other workers can take jobs while this one fetches.
        let job = match jobs.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).recv() {
            Ok(job) => job,
            Err(_) => return,
        };
        let bytes = match &job.source {
            JobSource::Url(url) => fetch(&agent, url),
            JobSource::File(path) => {
                // The scanner writes small thumbnails; cap the read like a download so a
                // pathologically large file cannot balloon the cache.
                std::fs::metadata(path)
                    .ok()
                    .filter(|metadata| metadata.len() <= MAX_BYTES)
                    .and_then(|_| std::fs::read(path).ok())
            }
        };
        let msg = match bytes {
            Some(bytes) if !bytes.is_empty() => Msg::Bytes { id: job.id, bytes },
            _ => Msg::Failed { id: job.id },
        };
        if results.send(msg).is_err() {
            return;
        }
        waker.as_ref()();
    }
}

/// Downloads one cover, bounded in both time (the agent's timeout) and size. `None` means the
/// caller draws its fallback; nothing is logged and nothing is retried.
fn fetch(agent: &ureq::Agent, url: &str) -> Option<Vec<u8>> {
    let mut response = agent.get(url).header("User-Agent", USER_AGENT).call().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    if let Some(length) = header(&response, "content-length").and_then(|value| value.parse::<u64>().ok()) {
        if length > MAX_BYTES {
            return None;
        }
    }
    if let Some(mime) = header(&response, "content-type") {
        if !mime.starts_with("image/") {
            return None;
        }
    }
    let bytes = response.body_mut().with_config().limit(MAX_BYTES).read_to_vec().ok()?;
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// The UTF-8 value of one response header, if present.
fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_url_requires_a_canonical_uuid() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            image_url(id, 160).unwrap(),
            "https://resources.tidal.com/images/01234567/89ab/cdef/0123/456789abcdef/160x160.jpg"
        );
        assert!(image_url("not-a-uuid", 160).is_none());
        assert!(image_url("0123456789abcdef0123456789abcdef", 160).is_none());
    }

    #[test]
    fn covers_and_artists_use_their_sizes() {
        let id = "01234567-89ab-cdef-0123-456789abcdef";
        assert!(cover_url(id).unwrap().ends_with("/160x160.jpg"));
        assert!(profile_url(id).unwrap().ends_with("/320x320.jpg"));
        assert!(ALLOWED_PX.contains(&160) && ALLOWED_PX.contains(&750));
    }

    /// A cache with no jobs sent, so the fetch threads stay idle and the map is driven directly.
    fn empty_cache() -> Artwork {
        let waker: Waker = Arc::new(|| {});
        Artwork::new(waker)
    }

    #[test]
    fn eviction_drops_the_least_recently_used_cover() {
        let mut art = empty_cache();
        for i in 0..MAX_ENTRIES {
            art.entries.insert(format!("id-{i}"), Entry::Failed);
            art.used.insert(format!("id-{i}"), Cell::new(i as u64));
        }
        // Every id is as old as its index, so `id-0` is the LRU and goes first.
        art.make_room();
        assert!(!art.entries.contains_key("id-0"), "the oldest entry is evicted");
        assert!(art.entries.contains_key("id-1"), "a fresher entry survives");
        assert!(art.entries.len() < MAX_ENTRIES, "the map came back under the ceiling");
    }

    #[test]
    fn the_avatar_and_now_playing_cover_are_never_evicted() {
        let mut art = empty_cache();
        for i in 0..MAX_ENTRIES {
            art.entries.insert(format!("id-{i}"), Entry::Failed);
            art.used.insert(format!("id-{i}"), Cell::new(i as u64));
        }
        // The profile avatar is the oldest entry and the now-playing cover the newest: eviction
        // must skip both and take the oldest *unpinned* cover instead.
        art.set_profile(Some("id-0"));
        let playing = format!("id-{}", MAX_ENTRIES - 1);
        art.set_now_playing(Some(playing.as_str()));
        art.make_room();
        assert!(art.entries.contains_key("id-0"), "the avatar survives");
        assert!(art.entries.contains_key(&playing), "the now-playing cover survives");
        assert!(!art.entries.contains_key("id-1"), "the LRU unpinned cover is evicted");
    }

    #[test]
    fn a_touched_entry_outlives_older_ones() {
        let mut art = empty_cache();
        for i in 0..MAX_ENTRIES {
            art.entries.insert(format!("id-{i}"), Entry::Failed);
            art.used.insert(format!("id-{i}"), Cell::new(i as u64));
        }
        // Reading `id-0` again makes it the freshest (beyond every index), so `id-1` becomes the
        // single least-recently-used entry and is the victim.
        let used = art.used.get("id-0").unwrap();
        used.set(MAX_ENTRIES as u64 + 1);
        art.make_room();
        assert!(art.entries.contains_key("id-0"), "a just-read cover survives");
        assert!(!art.entries.contains_key("id-1"), "the next-oldest is evicted instead");
    }

    #[test]
    fn eviction_skips_pinned_entries_and_still_trims() {
        let mut art = empty_cache();
        for i in 0..MAX_ENTRIES {
            art.entries.insert(format!("id-{i}"), Entry::Failed);
            art.used.insert(format!("id-{i}"), Cell::new(i as u64));
        }
        // Only `id-0` is pinned; eviction takes unpinned entries and stops once under the
        // ceiling rather than looping forever on the pinned one.
        art.set_profile(Some("id-0"));
        art.make_room();
        assert!(art.entries.contains_key("id-0"), "the pinned entry stays");
        assert!(art.entries.len() < MAX_ENTRIES, "the map still trims to the ceiling");
    }

    /// A 2x2 PNG the paint test fetches from disk, so the test needs no network.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72, 0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00,
        0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x84, 0x19, 0x60, 0x0c, 0x00,
        0x47, 0xca, 0x07, 0xf9, 0x67, 0x59, 0x6e, 0xb7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
        0x60, 0x82,
    ];

    /// The `file:` cover id the paint test's view draws. A view is a `fn`, so it cannot capture,
    /// and the initializer also writes the tiny PNG the fetch reads — keeping the whole fixture
    /// (path and bytes) in one place.
    static TEST_COVER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let dir = std::env::temp_dir().join("tsun-art-first-paint");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("cover.png");
        std::fs::write(&path, TINY_PNG).expect("write cover");
        format!("file:{}", path.display())
    });

    /// Draws one cover through the shared widget, exactly as the rows, tiles and thumbnails do.
    fn paint_view(s: &crate::app::Ui, _: &mut rune::Cx<crate::app::Ui>) -> rune::El {
        rune::col().child(
            rune::el()
                .key("cover")
                .child(crate::widgets::cover_art(s, crate::theme::palette(), Some(TEST_COVER.as_str()), Some(40.0))),
        )
    }

    /// Whether the `cover` wrapper still draws its fallback note icon (it is gone once the image
    /// paints).
    fn cover_placeholder_up(rt: &rune::Runtime<crate::app::Ui>) -> bool {
        let elements = rt.elements();
        let Some(cover) = elements.iter().find(|e| e.key.as_deref() == Some("cover")) else {
            return false;
        };
        let prefix = format!("{}/", cover.path);
        elements.iter().any(|e| e.path.starts_with(&prefix) && e.role == "icon")
    }

    /// A cold start draws a cover the cache has never seen: the draw notes the want, wakes the
    /// app for the poll that turns the want into a fetch, and the cover paints once the fetch and
    /// its decode land — with no other event (no tap, resize or navigation).
    #[test]
    fn a_cover_drawn_on_the_first_frame_is_fetched_and_painted_without_any_other_event() {
        // Touch the fixture so the tiny PNG exists before the runtime draws the cover.
        let _ = TEST_COVER.as_str();
        let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = woken.clone();
        let waker: Waker = Arc::new(move || flag.store(true, std::sync::atomic::Ordering::SeqCst));

        // The app's own state and poll: `init` builds the `Ui`, and the artwork cache and waker
        // are what `setup` would install (without the worker thread a test does not want).
        let mut s = crate::app::init();
        s.settings.discord_enabled = false;
        s.settings.notifications = false;
        s.artwork = Some(Artwork::new(waker.clone()));
        s.waker = Some(waker);
        let mut rt = rune::Runtime::new(s, paint_view, (240.0, 120.0), rune::Env::new(true, 1.0));
        rt.poll = Some(crate::app::poll);

        // The first frame: `poll` runs before the view, so this frame's draw is one the cache has
        // never seen. The placeholder stands in, and noting the want woke the app to fetch it.
        rt.frame(0.0);
        assert!(cover_placeholder_up(&rt), "the placeholder stands in until the cover arrives");
        assert!(
            woken.load(std::sync::atomic::Ordering::SeqCst),
            "drawing a cover the cache has not seen wakes the app for the poll that fetches it"
        );

        // From here nothing else happens: the fetch and its decode are the only things that wake
        // the app, exactly as on a cold start.
        let mut painted = false;
        for _ in 0..600 {
            rt.frame(1.0 / 60.0);
            if !cover_placeholder_up(&rt) {
                painted = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(painted, "the cover the first frame asked for paints with no other event");
    }
}
