//! Cover pixels read back on the CPU: the large cover the full-screen Now Playing view draws,
//! the average colour it tints its gradient with, and a small on-disk cover cache for desktop
//! notifications.
//!
//! Rune's [`ImageCache`] decodes covers for the renderer but hands back only an opaque handle, so
//! the average colour is computed from the real pixels of a 480px copy, downloaded once per cover
//! and decoded on a background thread (never on the frame). Those same bytes are handed to a
//! private [`ImageCache`] so the overlay can draw the cover at full size without competing for
//! the shared cache's entries. The notification icon needs a *file* on disk, so
//! [`cover_file`] downloads the cover once into `$XDG_CACHE_HOME/tsun/covers/` and reuses it.
//! Nothing here blocks a frame.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use rune::{Image, ImageCache, Waker};

use crate::art::cover_art_url;

/// The cover sizes tried for the overlay, best (crispest) first.
///
/// TIDAL stores a ladder per image and answers 403 for a size it does not have — 480 is missing
/// for many covers — so each size is tried in turn and the first that responds is used.
const OVERLAY_SIZES: [u32; 2] = [640, 320];
/// The sizes tried for the notification icon; smaller keeps the cached file (and the bubble) tidy.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const NOTIFY_SIZES: [u32; 2] = [320, 160];
/// Bounds one cover download so a stalled request cannot pin the worker.
const TIMEOUT: Duration = Duration::from_secs(8);
/// Hard cap on one response body.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Hard cap on retained covers; the overlay only ever shows the current one.
const MAX_ENTRIES: usize = 32;

const USER_AGENT: &str = "tsun/0.1 (TIDAL desktop client)";

// ---------------------------------------------------------------------------------------------
// Overlay cover art and tint (UI side)

/// One cover whose pixels have been asked for.
struct Job {
    id: String,
    /// `Some` for a local thumbnail (`file:<path>`), `None` to download a TIDAL cover.
    local: Option<PathBuf>,
}

/// A finished cover: the average colour, and the bytes for the renderer to decode.
struct Msg {
    id: String,
    colour: Option<[f32; 4]>,
    bytes: Vec<u8>,
}

/// Downloads and decodes the covers the full-screen view needs.
///
/// Create it once in `AppSpec::setup`, [`request`](CoverArt::request) every cover whose pixels or
/// tint are needed (idempotent and cheap), drain with [`poll`](CoverArt::poll) alongside the
/// other background work, and read with [`image`](CoverArt::image) and
/// [`tint`](CoverArt::tint).
pub(crate) struct CoverArt {
    /// A private cache for these covers, so the overlay's large art never evicts the shared one.
    cache: ImageCache,
    jobs: Sender<Job>,
    results: Receiver<Msg>,
    /// Cover ids already asked for, so requests are idempotent.
    known: HashSet<String>,
    images: HashMap<String, Image>,
    tints: HashMap<String, [f32; 4]>,
    /// Insertion order, oldest first, for the bound in [`MAX_ENTRIES`].
    order: VecDeque<String>,
}

impl CoverArt {
    /// Starts the single download/decode worker. Dropping the `CoverArt` closes the job channel
    /// and stops it.
    pub(crate) fn new(waker: Waker) -> Self {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .user_agent(USER_AGENT)
                .timeout_global(Some(TIMEOUT))
                .build(),
        );
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (result_tx, result_rx) = mpsc::channel::<Msg>();
        let cache = ImageCache::new(waker.clone());
        thread::Builder::new()
            .name("tsun-cover-art".to_owned())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let bytes = match &job.local {
                        Some(path) => std::fs::read(path).ok(),
                        None => download_cover(&agent, &job.id, &OVERLAY_SIZES),
                    };
                    let Some(bytes) = bytes else { continue };
                    let colour = image::load_from_memory(&bytes)
                        .ok()
                        .map(|image| average_rgba(&image.to_rgba8()));
                    if result_tx.send(Msg { id: job.id, colour, bytes }).is_err() {
                        break;
                    }
                    waker.as_ref()();
                }
            })
            .expect("cover art worker");
        CoverArt {
            cache,
            jobs: job_tx,
            results: result_rx,
            known: HashSet::new(),
            images: HashMap::new(),
            tints: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Asks for the cover `id` at full size. No-op when the id is malformed or already requested.
    pub(crate) fn request(&mut self, id: &str) {
        // Local thumbnails are read from disk; anything else must be a TIDAL cover UUID (the
        // URL builder is the single source of that rule).
        let local = if let Some(path) = id.strip_prefix("file:") {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return;
            }
            Some(path)
        } else {
            if cover_art_url(id, OVERLAY_SIZES[0]).is_none() {
                return;
            }
            None
        };
        if !self.known.insert(id.to_owned()) {
            return;
        }
        let _ = self.jobs.send(Job { id: id.to_owned(), local });
    }

    /// Drains finished covers. Returns `true` when one became available since the last call, so
    /// the caller can redraw.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.results.try_recv() {
                Ok(msg) => {
                    // `load` hashes and queues the bytes on the cache's decoder; it never
                    // decodes here.
                    self.images.insert(msg.id.clone(), self.cache.load(msg.bytes));
                    if let Some(colour) = msg.colour {
                        self.tints.insert(msg.id.clone(), colour);
                    }
                    self.order.push_back(msg.id);
                    while self.order.len() > MAX_ENTRIES {
                        if let Some(oldest) = self.order.pop_front() {
                            self.images.remove(&oldest);
                            self.tints.remove(&oldest);
                        }
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// The decoded cover for `id`, once it is ready to draw.
    pub(crate) fn image(&self, id: &str) -> Option<&Image> {
        self.images.get(id).filter(|image| image.is_ready())
    }

    /// The average linear-sRGB colour of `id`'s cover, or `None` while it is unknown.
    pub(crate) fn tint(&self, id: &str) -> Option<[f32; 4]> {
        self.tints.get(id).copied()
    }
}

/// Averages a straight-alpha sRGB RGBA buffer, skipping fully transparent pixels. Returns linear
/// sRGB with alpha 1, or black when nothing was readable.
pub(crate) fn average_rgba(rgba: &[u8]) -> [f32; 4] {
    let mut sums = [0.0f64; 3];
    let mut weight = 0.0f64;
    for pixel in rgba.chunks_exact(4) {
        let alpha = pixel[3] as f64 / 255.0;
        if alpha <= 0.0 {
            continue;
        }
        for channel in 0..3 {
            sums[channel] += (pixel[channel] as f64 / 255.0) * alpha;
        }
        weight += alpha;
    }
    if weight <= 0.0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    [
        srgb_to_linear((sums[0] / weight) as f32),
        srgb_to_linear((sums[1] / weight) as f32),
        srgb_to_linear((sums[2] / weight) as f32),
        1.0,
    ]
}

/// The sRGB transfer function's inverse, matching rune's linear-light colour space.
pub(crate) fn srgb_to_linear(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

// ---------------------------------------------------------------------------------------------
// Notification cover file

/// The path of the cached cover for `id`, downloading it once if needed.
///
/// Blocking (network + file I/O): call it from the notification thread, never the UI frame. Any
/// malformed id, failed download or cache-write failure yields `None`, and the notification is
/// posted without an icon. Only the Linux notification backend calls it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn cover_file(id: &str) -> Option<PathBuf> {
    // A local track's artwork is already a JPEG thumbnail on disk; the notification uses it
    // directly instead of downloading anything.
    if let Some(path) = id.strip_prefix("file:") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let path = cache_path(id)?;
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 0) {
        return Some(path);
    }
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .timeout_global(Some(TIMEOUT))
            .build(),
    );
    let bytes = download_cover(&agent, id, &NOTIFY_SIZES)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// `covers/<uuid>.jpg` in [`crate::paths::cache_dir`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cache_path(id: &str) -> Option<PathBuf> {
    Some(crate::paths::cache_dir()?.join("covers").join(format!("{id}.jpg")))
}

// ---------------------------------------------------------------------------------------------
// Downloading

/// Downloads `id`'s cover at the first size in `sizes` the server actually has.
///
/// TIDAL answers 403 for a ladder size an image does not have, so the sizes are tried in turn.
/// `None` when the id is malformed or every size failed.
fn download_cover(agent: &ureq::Agent, id: &str, sizes: &[u32]) -> Option<Vec<u8>> {
    for size in sizes {
        let Some(url) = cover_art_url(id, *size) else { return None };
        if let Some(bytes) = fetch(agent, &url) {
            return Some(bytes);
        }
    }
    None
}

/// Downloads one resource, bounded in time and size.
fn fetch(agent: &ureq::Agent, url: &str) -> Option<Vec<u8>> {
    let response = agent.get(url).call().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut reader = response.into_body().into_reader().take(MAX_BYTES);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() { None } else { Some(bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn average_of_a_solid_image_is_that_colour_linearised() {
        // Two opaque mid-grey sRGB pixels (128/255 = 0.502 sRGB).
        let rgba = [128u8, 128, 128, 255, 128, 128, 128, 255];
        let [r, g, b, a] = average_rgba(&rgba);
        let expected = srgb_to_linear(128.0 / 255.0);
        assert!((r - expected).abs() < 1e-6);
        assert!((g - expected).abs() < 1e-6);
        assert!((b - expected).abs() < 1e-6);
        assert_eq!(a, 1.0);
    }

    #[test]
    fn average_ignores_fully_transparent_pixels() {
        // One opaque black pixel and one fully transparent white one.
        let rgba = [0u8, 0, 0, 255, 255, 255, 255, 0];
        let [r, g, b, _] = average_rgba(&rgba);
        assert_eq!([r, g, b], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn average_of_nothing_is_black() {
        assert_eq!(average_rgba(&[]), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn srgb_transfer_matches_the_spec_at_the_knee() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(srgb_to_linear(1.0), 1.0);
        assert!((srgb_to_linear(0.5) - 0.2140).abs() < 1e-3);
        // Below the knee the curve is linear.
        assert!((srgb_to_linear(0.04) - 0.04 / 12.92).abs() < 1e-9);
    }

    #[test]
    fn a_malformed_cover_id_is_rejected() {
        assert!(cover_file("not-a-uuid").is_none());
    }

    /// Downloads a real TIDAL cover through the same path the notification icon uses, and checks
    /// the pixels decode to a non-black colour, exactly as the overlay's tint needs.
    #[test]
    #[ignore = "needs a signed-in session and the network"]
    fn a_real_tidal_cover_caches_decodes_and_tints() {
        let mut client = crate::tidal::Client::new().expect("a TIDAL client");
        let found = client.search_all("the weeknd").expect("search TIDAL");
        let id = found
            .tracks
            .iter()
            .find_map(|track| track.cover.clone())
            .expect("a track with cover art");
        let path = cover_file(&id).unwrap_or_else(|| panic!("could not download the cover for {id}"));
        let bytes = std::fs::read(&path).expect("read the cached cover");
        let decoded = image::load_from_memory(&bytes).expect("a real JPEG");
        let [r, g, b, alpha] = average_rgba(&decoded.to_rgba8());
        assert_eq!(alpha, 1.0);
        assert!(r + g + b > 0.01, "the cover tint came out black: {r} {g} {b}");
    }

    /// A JPEG encoded in-process decodes to its own colour, so the decode half of the pipeline is
    /// covered without a network round trip.
    #[test]
    fn a_jpeg_decodes_to_its_own_colour() {
        let mut source = image::RgbImage::new(8, 8);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgb([200, 40, 40]);
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&source)
            .expect("encode a JPEG");
        let decoded = image::load_from_memory(&jpeg).expect("decode the JPEG just encoded");
        let [r, g, b, a] = average_rgba(&decoded.to_rgba8());
        assert!(r > g && r > b, "the tint keeps the cover's hue: {r} {g} {b}");
        assert_eq!(a, 1.0);
        // 200/255 in sRGB is ~0.578 in the linear space the palette uses (JPEG is lossy).
        assert!((r - srgb_to_linear(200.0 / 255.0)).abs() < 0.05, "r = {r}");
    }
}
