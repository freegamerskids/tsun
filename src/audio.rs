//! Native Rust audio playback for the tsun TIDAL client.
//!
//! Playback is fully in-process: there is no `mpv`/`ffmpeg` subprocess. Decoding
//! and output use the [`symphonia`] and [`cpal`] crates.
//!
//! # Sources
//!
//! [`Source`] describes what to play:
//!
//! * [`Source::Url`] — a single HTTP(S) resource. TIDAL's BTS manifest and the
//!   `HI_RES_LOSSLESS` tier hand back a direct FLAC URL; `HIGH` may hand back a
//!   progressive AAC/MP4 resource.
//! * [`Source::Hls`] — an in-memory HLS playlist, exactly as produced by
//!   [`crate::tidal`]'s DASH→HLS conversion: `#EXT-X-MAP` carries the fMP4
//!   initialization segment (`ftyp`+`moov`) and each `#EXTINF` line is followed
//!   by an absolute segment URL. The segments are concatenated, in order, into
//!   one logical byte stream and fetched one at a time, so a track is never
//!   downloaded whole before it starts playing (but see *Seeking* below).
//! * [`Source::EncryptedUrl`] — a single resource wrapped in TIDAL's legacy
//!   AES-128-CTR stream cipher (the scheme used by `streamrip` and friends).
//!   The key/nonce are supplied by the caller; the keystream is applied to the
//!   HTTP bytes as they stream in.
//! * [`Source::File`] — a local audio file. It is read and seeked straight from
//!   disk (no fetch thread, no in-memory store) and is seekable from the start.
//!
//! # Buffering and seeking
//!
//! Every byte the fetcher receives is *retained* in an in-memory [`Store`] as
//! well as handed to the demuxer, so the whole track is buffered as it streams.
//! That costs the track's size in RAM (a few tens of MiB) and buys random
//! access: [`Player::seek`] repositions the demuxer so FLAC and AAC/MP4 sources
//! can be seeked.
//!
//! Seeking has two paths:
//!
//! * Seeking *forward* past the current position is done by the demuxer's own
//!   forwards-only resync, reading (and blocking for) network bytes as needed —
//!   it never has to download the rest of the track.
//! * Seeking *backwards* (or to any point when the container cannot seek
//!   forwards in place) waits until the track has finished downloading, then
//!   re-opens the container over a seekable view of the fully buffered bytes and
//!   asks the demuxer to seek. If the container's seek is unsupported it falls
//!   back to decoding from the start and discarding packets until the target is
//!   reached.
//!
//! # Volume
//!
//! Volume is owned by the application through a cloneable [`Volume`] handle
//! ([`Arc`]-backed) that is passed to [`Player::start`]. Because the handle
//! outlives a [`Player`], volume and mute survive track changes. The output
//! callback applies the gain with a short linear ramp (about 20 ms) so changes
//! are click-free; pausing and resuming use the same ramp.
//!
//! # Output
//!
//! The caller names the output device through [`OutputOptions`]: a specific device
//! ( [`output_devices`] lists what the host offers, including raw `hw:` ALSA plugs
//! for an exclusive-ish path) or the system default when `None`. The device is
//! opened at the stream's own sample rate and channel count when it advertises it,
//! so lossless output stays lossless; otherwise the closest supported rate and
//! channel layout are used and the decoded audio is resampled to them with a
//! windowed-sinc interpolator (no rate conversion happens at all when the rates
//! already match). At volume 100%, unmuted and with normalisation off, the output
//! callback copies samples through without applying any gain, leaving the
//! bit-perfect path untouched. If the named device has vanished, or no output
//! device exists, [`Player::start`] fails with an actionable message instead of
//! pretending to play.
//!
//! [`Player::set_output_device`] re-opens the device mid-track, restarting at the
//! position that was playing; a device that cannot be opened is reported as an
//! error through [`Player::poll`]. [`Player::output_info`] reports the format the
//! stream was actually opened with.
//!
//! # Errors
//!
//! [`Player::start`] only returns `Ok` once the container opened, the first
//! audio packet decoded, *and* the output stream started. Failures to open the
//! source, detect the format, decode audio, or open a device are returned as
//! `Err`. Errors that can only happen later (a mid-stream network failure, a
//! device error) are reported through [`Player::poll`].
//!
//! # Track transitions and gapless playback
//!
//! [`Player::poll`] distinguishes a natural end ([`Outcome::Finished`]) from an
//! explicit stop ([`Outcome::Stopped`]) so a queue can auto-advance. The previous
//! stream is torn down promptly: [`Player::stop`] (and [`Drop`]) only ever joins
//! the decoder thread, which observes the stop flag between reads, while the
//! network fetcher is detached and signalled to abort.
//!
//! For real gapless playback the application can hand the next track to
//! [`Player::preload`] ahead of time. That opens the source, probes the
//! container and decodes its first audio buffer on a background thread, and
//! stores the result. When the current decoder reaches end of stream the audio
//! thread swaps the preloaded session in *without tearing the output stream
//! down*, so consecutive tracks run into each other with no gap at all. When the
//! preloaded track's channel layout or sample rate differs, the device is
//! reopened at the new rate (the one case that cannot stay seamless). A preload
//! that fails to resolve, or one that was cancelled with
//! [`Player::cancel_preload`], simply falls back to the non-gapless path:
//! [`Outcome::Finished`] is reported as usual and the caller starts the next
//! track itself.
//!
//! # Threading
//!
//! A [`Player`] owns a fetcher thread that reads HTTP into the [`Store`] and a
//! decoder thread that demuxes, decodes and feeds a bounded sample queue driving
//! the `cpal` callback. `Player` holds only `Send` handles ([`Arc`]s and a
//! [`JoinHandle`]), so it may be moved between threads; the `cpal` stream itself
//! never leaves its owning thread.
//!
//! # Limitations
//!
//! * Widevine/PlayReady DRM cannot be played: that requires a CDM, which is out
//!   of scope. Legacy AES-128-CTR is supported.
//! * `Player::stop` joins the decoder thread. Decoder reads observe the stop
//!   flag between network reads, so stopping is normally immediate; a stalled
//!   socket bounds the join by the HTTP read timeout.

use std::collections::VecDeque;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use aes::cipher::{KeyIvInit, StreamCipher};
use aes::Aes128;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use ctr::Ctr128BE;
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

// ---------------------------------------------------------------------------------------------
// Constants

const USER_AGENT: &str = "tsun/0.1 (TIDAL desktop client)";

/// Ceiling for establishing a connection. Deliberately *not* a whole-stream
/// deadline: a track plays for minutes, and back-pressure throttles reads.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling for a single read from a socket. Bounds both stall detection and the
/// time [`Player::stop`] can be kept waiting by a dead connection.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// How long [`Player::start`] waits for the first decoded packet and the output
/// stream to come up.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Probe buffer for the demuxer. Must be a power of two larger than 32 KiB; the
/// probe rewinds within it, and one fMP4 init segment plus the first fragment
/// fit comfortably.
const PROBE_BUFFER: usize = 1 << 20;

/// Size of a single network read into the byte store.
const FEED_CHUNK: usize = 64 * 1024;

/// Preferred amount of decoded audio kept ahead of the output callback.
const AUDIO_BUFFER_MS: usize = 250;

/// Duration of the linear gain ramp applied to volume, mute, pause and resume
/// changes. Long enough to be inaudible, short enough to feel instant.
const VOLUME_RAMP_SECONDS: f32 = 0.02;

/// Half-width of the windowed-sinc interpolation kernel, in source frames. 24 taps per side is
/// ~48 dB of stop-band rejection with a Hann window, ample for rate conversion while staying
/// cheap (a few million multiply-adds per second of audio).
const RESAMPLE_HALF_TAPS: i32 = 24;

// ---------------------------------------------------------------------------------------------
// Public API

/// Where to send audio, and how.
///
/// `device` names a cpal output device exactly as [`output_devices`] reports it; `None` follows
/// the host's default. The name is resolved when the stream is (re)opened, so a device that has
/// disappeared since it was chosen produces an actionable error rather than silent playback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputOptions {
    /// The output device name, or `None` for the system default.
    pub device: Option<String>,
}

/// The format an output stream was actually opened with, as the device accepted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    /// The device's cpal name.
    pub device: String,
    /// Sample rate the device is running at, in Hz.
    pub rate: u32,
    /// Channel count the device is running at.
    pub channels: usize,
    /// The sample format name, e.g. `f32` or `i16`.
    pub sample_format: String,
}

impl OutputInfo {
    /// `"96 kHz · f32 · Built-in Audio Analog Stereo"`, for the UI.
    pub fn label(&self) -> String {
        format!(
            "{} kHz · {} · {}",
            format_khz(self.rate),
            self.sample_format,
            self.device
        )
    }
}

/// Formats a sample rate as kilohertz without a trailing `.0`: `44100` → `44.1`, `48000` → `48`.
pub fn format_khz(rate: u32) -> String {
    let whole = rate / 1000;
    let fraction = (rate % 1000) / 100;
    if fraction == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{fraction}")
    }
}

/// Enumerates the host's output devices as `(name, is_default)`, the default first.
///
/// This is the real device list the Settings page offers; it includes whatever PipeWire, Pulse
/// or ALSA exposes, including raw `hw:` plug names. An empty list means no output at all.
pub fn output_devices() -> Vec<(String, bool)> {
    let host = cpal::default_host();
    let default = host
        .default_output_device()
        .and_then(|device| device.name().ok());
    let mut devices = Vec::new();
    if let Ok(list) = host.output_devices() {
        for device in list {
            if let Ok(name) = device.name() {
                let is_default = default.as_deref() == Some(name.as_str());
                devices.push((name, is_default));
            }
        }
    }
    // The default can be missing from `output_devices` on some hosts; keep it visible.
    if let Some(default) = default {
        if !devices.iter().any(|(name, _)| *name == default) {
            devices.insert(0, (default, true));
        }
    }
    devices.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    devices
}

/// Shared, cloneable volume control.
///
/// The application owns one of these and passes a clone to every
/// [`Player::start`], so the volume outlives individual tracks. All operations
/// are lock-free and safe to call from any thread (the UI thread), including
/// between tracks.
#[derive(Clone, Debug)]
pub struct Volume {
    gain: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    /// Linear multiplier from the current stream's ReplayGain; 1.0 when normalization is off
    /// or the stream reports no gain.
    normalize: Arc<AtomicU32>,
}

impl Volume {
    /// A new handle at full volume, unmuted.
    pub fn new() -> Self {
        Volume {
            gain: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            muted: Arc::new(AtomicBool::new(false)),
            normalize: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        }
    }

    /// Sets the volume, clamped to `0.0..=1.0`. `NaN` is treated as full volume.
    ///
    /// Mute is independent: setting the volume while muted does not unmute.
    pub fn set_volume(&self, volume: f32) {
        let volume = if volume.is_nan() { 1.0 } else { volume.clamp(0.0, 1.0) };
        self.gain.store(volume.to_bits(), Ordering::Relaxed);
    }

    /// The configured volume in `0.0..=1.0` (ignores mute).
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// Mutes or unmutes without losing the configured volume.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Whether output is currently muted.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Sets the ReplayGain-style normalization multiplier applied on top of the volume. `1.0`
    /// disables it; values are clamped to a sane `0.0..=4.0` so a broken tag cannot blast the
    /// output. Takes effect on the next output callback, without restarting the track.
    pub fn set_normalization_gain(&self, gain: f32) {
        let gain = if gain.is_finite() { gain.clamp(0.0, 4.0) } else { 1.0 };
        self.normalize.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// The current normalization multiplier ([`Volume::set_normalization_gain`]).
    pub fn normalization_gain(&self) -> f32 {
        f32::from_bits(self.normalize.load(Ordering::Relaxed))
    }

    /// The linear gain the output callback ramps toward: the volume, or zero
    /// while muted, times the normalization multiplier.
    pub fn gain(&self) -> f32 {
        if self.is_muted() {
            0.0
        } else {
            self.volume() * self.normalization_gain()
        }
    }
}

impl Default for Volume {
    fn default() -> Self {
        Volume::new()
    }
}

/// A playback source.
#[derive(Debug, Clone)]
pub enum Source {
    /// A single HTTP(S) resource (FLAC, fMP4 or ADTS AAC).
    Url(String),
    /// An in-memory HLS playlist with absolute segment URLs, as produced by the
    /// DASH→HLS conversion in [`crate::tidal`].
    Hls(String),
    /// A single HTTP(S) resource encrypted with TIDAL's legacy AES-128-CTR.
    ///
    /// `key` is the 16-byte AES key; `nonce` is the 8-byte nonce prefix. The
    /// keystream is the standard `Ctr128BE` construction over a 16-byte IV made
    /// of `nonce` followed by a big-endian 64-bit block counter starting at 0.
    EncryptedUrl {
        url: String,
        key: [u8; 16],
        nonce: [u8; 8],
    },
    /// A local audio file, read and seeked directly from disk. No fetcher thread
    /// is involved; the container is seekable from the first packet.
    File(std::path::PathBuf),
}

/// Why a playback ended, reported by [`Player::poll`] as a [`Result`].
///
/// A natural end and an explicit stop are distinct so that a queue can
/// auto-advance only on [`Outcome::Finished`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The track reached its end; all decoded audio was delivered.
    Finished,
    /// Playback was stopped by [`Player::stop`] (or by dropping the [`Player`]).
    Stopped,
}

/// An active playback. Dropping it (or calling [`Player::stop`]) stops audio.
///
/// `Player` is `Send`: it owns only handles; the audio device and the demuxer
/// live on worker threads.
pub struct Player {
    stop: Arc<AtomicBool>,
    store: Arc<Store>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Starts playback at the beginning and returns once audio is genuinely
    /// flowing, using `volume` for output gain.
    ///
    /// `Ok` is returned only after the source was opened, the first audio
    /// packet was decoded, and the output stream was started. Any failure up to
    /// that point — an unreachable source, an unsupported or encrypted
    /// container, a decode failure, or no output device — is returned as `Err`
    /// with an actionable message.
    pub fn start(source: Source, volume: Volume, output: OutputOptions) -> Result<Self, String> {
        Self::start_at(source, volume, 0.0, output)
    }

    /// Like [`Player::start`], but begins playback at `position_seconds`.
    ///
    /// The position is applied by seeking forward from the start, so it streams
    /// only as far as needed before audio begins.
    pub fn start_at(
        source: Source,
        volume: Volume,
        position_seconds: f64,
        output: OutputOptions,
    ) -> Result<Self, String> {
        let plan = Plan::new(source)?;

        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::new(stop.clone(), volume));
        let store = Arc::new(Store::new(stop.clone()));
        shared.set_store(store.clone());
        let (ready_tx, ready_rx) = sync_channel::<Result<(), String>>(1);

        let thread_stop = stop.clone();
        let thread_shared = shared.clone();
        let thread_store = store.clone();
        let position = if position_seconds.is_finite() { position_seconds.max(0.0) } else { 0.0 };
        let handle = thread::Builder::new()
            .name("tsun-audio".to_string())
            .spawn(move || {
                run(
                    plan,
                    thread_stop,
                    thread_store,
                    thread_shared,
                    ready_tx,
                    position,
                    output,
                )
            })
            .map_err(|error| format!("could not start the audio thread: {error}"))?;

        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(Player {
                stop,
                store,
                shared,
                thread: Some(handle),
            }),
            Ok(Err(message)) => {
                stop.store(true, Ordering::SeqCst);
                store.wake();
                let _ = handle.join();
                Err(message)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                store.wake();
                let _ = handle.join();
                Err("timed out while starting playback".to_string())
            }
        }
    }

    /// Stops playback and waits for the decoder thread to wind down.
    ///
    /// Idempotent. Safe to call from [`Drop`].
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.store.wake();
        if let Some(store) = self.shared.current_store() {
            store.wake();
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }

    /// Pausing keeps buffered audio for a later resume. The change is ramped so
    /// it does not click.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    /// Opens the next track ahead of time so it can be swapped in seamlessly when
    /// this one reaches end of stream.
    ///
    /// `id` is echoed back by [`Player::take_gapless_switch`] once the swap has
    /// happened; it is otherwise opaque to the audio layer. `normalize_gain` is the
    /// ReplayGain multiplier to apply from the first sample of `source`, so the
    /// hand-off does not swing in loudness. Any previously preloaded track is
    /// superseded, and a preload that fails to open simply never becomes available
    /// (the caller falls back to [`Outcome::Finished`]).
    pub fn preload(&self, id: u64, source: Source, normalize_gain: f32) -> Result<(), String> {
        let plan = Plan::new(source)?;
        let generation = self.shared.next_preload_generation();
        let store = Arc::new(Store::new(self.stop.clone()));
        let thread_store = store.clone();
        let thread_stop = self.stop.clone();
        let thread_shared = self.shared.clone();
        thread::Builder::new()
            .name("tsun-audio-preload".to_string())
            .spawn(move || {
                preload(plan, id, normalize_gain, thread_stop, thread_store, thread_shared, generation)
            })
            .map_err(|error| format!("could not start the preload thread: {error}"))?;
        Ok(())
    }

    /// Drops any outstanding preload, e.g. because the queue changed underneath it.
    ///
    /// Idempotent and safe to call on every queue edit: it only costs when a preload
    /// was actually pending.
    pub fn cancel_preload(&self) {
        self.shared.cancel_preload();
    }

    /// The preloaded track that has been swapped in and not yet reported, if any.
    ///
    /// [`Player::poll`] keeps returning `None` for such a swap (the same output
    /// stream is still playing), so the caller learns about the change here.
    pub fn take_gapless_switch(&self) -> Option<u64> {
        self.shared.take_switch()
    }

    /// Re-opens the output on `device`, restarting at the position that is playing.
    ///
    /// Asynchronous: the audio thread notices on its next iteration. A device that
    /// cannot be opened is reported through [`Player::poll`] as an error, which also
    /// ends playback rather than silently playing to the wrong device.
    pub fn set_output_device(&self, device: Option<String>) {
        if let Ok(mut slot) = self.shared.device_request.lock() {
            *slot = Some(device);
        }
        // The decode thread may be blocked feeding the sample queue; nudge it so the
        // change lands promptly.
        self.shared.signal.notify_all();
        if let Some(store) = self.shared.current_store() {
            store.wake();
        }
    }

    /// The output format the stream is currently open at.
    pub fn output_info(&self) -> Option<OutputInfo> {
        self.shared.output_info()
    }

    /// Requests playback to jump to `seconds` from the start of the track.
    ///
    /// The request is asynchronous: this returns immediately and the decode
    /// thread repositions while the position clock is updated right away (then
    /// corrected to the demuxer's actual position). See the module docs for how
    /// forward and backward seeks are performed.
    pub fn seek(&self, seconds: f64) {
        let seconds = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
        let rate = self.shared.out_rate.load(Ordering::Relaxed).max(1) as f64;
        let current = self.shared.position_frames() as f64 / rate;
        if let Ok(mut request) = self.shared.seek_request.lock() {
            *request = Some(SeekRequest {
                target: seconds,
                backward: seconds + 0.001 < current,
            });
        }
        // Reflect the target at once so the UI clock jumps without waiting for
        // the decode thread to reposition.
        self.shared.set_seek_position(seconds);
        self.shared.clear_queue();
    }

    /// Non-blocking status check.
    ///
    /// * `None` — still playing.
    /// * `Some(Ok(Outcome::Finished))` — the track reached its natural end.
    /// * `Some(Ok(Outcome::Stopped))` — playback was stopped.
    /// * `Some(Err(message))` — a late failure (network, decode, device).
    pub fn poll(&mut self) -> Option<Result<Outcome, String>> {
        match self.shared.outcome.lock() {
            Ok(slot) => slot.clone(),
            Err(_) => Some(Err("audio playback state was poisoned".to_string())),
        }
    }

    /// Output time in whole seconds based on samples actually delivered,
    /// excluding underruns. Correct after a [`Player::seek`] and while paused.
    pub fn position_seconds(&self) -> u64 {
        self.shared.position_seconds()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Playback plan

/// The ordered byte sources that make up one logical stream.
#[derive(Clone)]
enum Plan {
    /// One resource, optionally AES-128-CTR encrypted.
    Single {
        url: String,
        key: Option<([u8; 16], [u8; 8])>,
    },
    /// fMP4 initialization segment followed by media segments.
    Segments(Vec<String>),
    /// A local file, opened directly from disk.
    File(std::path::PathBuf),
}

impl Plan {
    fn new(source: Source) -> Result<Self, String> {
        match source {
            Source::Url(url) => {
                require_http_url(&url)?;
                Ok(Plan::Single { url, key: None })
            }
            Source::EncryptedUrl { url, key, nonce } => {
                require_http_url(&url)?;
                Ok(Plan::Single {
                    url,
                    key: Some((key, nonce)),
                })
            }
            Source::Hls(playlist) => Ok(Plan::Segments(parse_playlist(&playlist)?)),
            Source::File(path) => {
                if !path.is_file() {
                    return Err(format!("{} is not a readable audio file", path.display()));
                }
                Ok(Plan::File(path))
            }
        }
    }

    /// Whether this plan is a local file (no fetch thread, always seekable).
    fn is_file(&self) -> bool {
        matches!(self, Plan::File(_))
    }

    /// Opens the demuxer's view of the plan. Network plans go through the byte
    /// store (streaming when `seekable` is false); a local file is opened once
    /// and is always seekable.
    fn open_source(
        &self,
        store: &Arc<Store>,
        seekable: bool,
    ) -> Result<Box<dyn MediaSource>, String> {
        match self {
            Plan::File(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|error| format!("could not open {}: {error}", path.display()))?;
                Ok(Box::new(file))
            }
            _ => Ok(Box::new(StoreSource::new(store.clone(), seekable))),
        }
    }

    /// File extension handed to the prober to make format detection cheap and
    /// reliable. A wrong hint is harmless; the probe still inspects the bytes.
    fn hint_extension(&self) -> &'static str {
        match self {
            Plan::Segments(_) => "mp4",
            Plan::File(path) => match path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("flac") => "flac",
                Some("mp3") => "mp3",
                Some("aac" | "adts") => "aac",
                Some("m4a" | "mp4" | "mov" | "m4s") => "mp4",
                Some("ogg" | "oga") => "ogg",
                Some("wav") => "wav",
                Some("aiff" | "aif") => "aiff",
                _ => "mp4",
            },
            Plan::Single { url, .. } => match url_extension(url).as_str() {
                "flac" => "flac",
                "mp3" => "mp3",
                "aac" | "adts" => "aac",
                "m4a" | "mp4" | "mov" | "m4s" => "mp4",
                "ogg" | "oga" => "ogg",
                "wav" => "wav",
                "aiff" | "aif" => "aiff",
                _ => "mp4",
            },
        }
    }
}

fn require_http_url(url: &str) -> Result<(), String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err("audio source URL is not absolute HTTP(S)".to_string())
    }
}

fn url_extension(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((_, extension)) => extension.to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Extracts the ordered URL list from an in-memory HLS playlist.
///
/// The playlist is the output of the DASH→HLS conversion: an optional
/// `#EXT-X-MAP` initialization segment followed by absolute segment URLs, each
/// preceded by `#EXTINF`.
fn parse_playlist(playlist: &str) -> Result<Vec<String>, String> {
    let mut initialization: Option<String> = None;
    let mut segments: Vec<String> = Vec::new();

    for raw in playlist.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-KEY") || line.starts_with("#EXT-X-SESSION-KEY") {
            return Err("the audio stream advertises encryption; refusing to play it".to_string());
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = map_uri(rest)
                .ok_or_else(|| "HLS playlist has an EXT-X-MAP without a URI".to_string())?;
            initialization = Some(uri);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        segments.push(line.to_string());
    }

    if segments.is_empty() {
        return Err("HLS playlist contains no audio segments".to_string());
    }

    let mut urls = Vec::with_capacity(segments.len() + 1);
    if let Some(initialization) = initialization {
        if !initialization.starts_with("http://") && !initialization.starts_with("https://") {
            return Err("HLS initialization segment URL is not absolute".to_string());
        }
        urls.push(initialization);
    }
    for segment in segments {
        if !segment.starts_with("http://") && !segment.starts_with("https://") {
            return Err("HLS segment URL is not absolute".to_string());
        }
        urls.push(segment);
    }
    Ok(urls)
}

/// Reads the `URI="..."` attribute of an `#EXT-X-MAP` tag.
fn map_uri(attributes: &str) -> Option<String> {
    let rest = attributes.trim().strip_prefix("URI=")?.trim();
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or(""),
        None => rest.split(',').next().unwrap_or("").trim(),
    };
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// Byte store (HTTP fetcher thread -> demuxer, retained for seeking)

/// A growable, cancellable byte buffer shared by the fetcher thread and the
/// demuxer.
///
/// Unlike a plain pipe it never discards consumed bytes: everything the fetcher
/// received stays addressable so a later [`Player::seek`] can read backwards.
struct Store {
    /// Cancellation flag shared with the whole playback.
    stop: Arc<AtomicBool>,
    state: Mutex<StoreState>,
    signal: Condvar,
}

#[derive(Default)]
struct StoreState {
    data: Vec<u8>,
    eof: bool,
    error: Option<String>,
    stopped: bool,
}

impl Store {
    fn new(stop: Arc<AtomicBool>) -> Self {
        Store {
            stop,
            state: Mutex::new(StoreState::default()),
            signal: Condvar::new(),
        }
    }

    /// True once playback has been stopped.
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Appends bytes. Returns `false` once playback has stopped or failed.
    fn push(&self, data: &[u8]) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        if state.stopped || state.error.is_some() || self.cancelled() {
            return false;
        }
        state.data.extend_from_slice(data);
        drop(state);
        self.signal.notify_all();
        true
    }

    /// Marks the end of the stream, with an error if the fetch failed.
    fn finish(&self, error: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.eof = true;
            if state.error.is_none() {
                state.error = error;
            }
        }
        self.signal.notify_all();
    }

    fn error(&self) -> Option<String> {
        self.state.lock().ok().and_then(|state| state.error.clone())
    }

    /// Marks the stream as aborted (playback stopped).
    fn abort(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.stopped = true;
        }
        self.signal.notify_all();
    }

    /// Wakes any thread blocked in [`Store::read_at`] or [`Store::wait_complete`].
    fn wake(&self) {
        self.signal.notify_all();
    }

    /// Bytes buffered so far.
    fn len(&self) -> usize {
        self.state.lock().map(|state| state.data.len()).unwrap_or(0)
    }

    /// True once the fetch finished cleanly.
    fn is_complete(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.eof && state.error.is_none() && !state.stopped)
            .unwrap_or(false)
    }

    /// Reads at most `buffer.len()` bytes at `cursor`, advancing it. Blocks
    /// while the cursor is at the end of a still-running fetch. Returns `Ok(0)`
    /// at end of stream.
    fn read_at(&self, cursor: &mut usize, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("audio stream state was poisoned"))?;
        loop {
            if state.stopped || self.cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "playback stopped",
                ));
            }
            let available = state.data.len();
            if *cursor < available {
                let count = buffer.len().min(available - *cursor);
                buffer[..count].copy_from_slice(&state.data[*cursor..*cursor + count]);
                *cursor += count;
                drop(state);
                self.signal.notify_all();
                return Ok(count);
            }
            if let Some(error) = &state.error {
                return Err(io::Error::other(error.clone()));
            }
            if state.eof {
                return Ok(0);
            }
            state = match self.signal.wait_timeout(state, Duration::from_millis(100)) {
                Ok((state, _)) => state,
                Err(_) => return Err(io::Error::other("audio stream state was poisoned")),
            };
        }
    }

    /// Blocks until the fetch has finished. Returns `true` on a clean end,
    /// `false` if the fetch failed or playback stopped.
    fn wait_complete(&self) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        loop {
            if state.stopped || self.cancelled() {
                return false;
            }
            if state.eof {
                return state.error.is_none();
            }
            state = match self.signal.wait_timeout(state, Duration::from_millis(100)) {
                Ok((state, _)) => state,
                Err(_) => return false,
            };
        }
    }
}

/// The demuxer's view of the byte store.
///
/// With `seekable = false` it presents a progressive network stream: no random
/// access, no known length. With `seekable = true` it presents the fully
/// buffered track for random access; it is only ever used once the fetch has
/// finished, so its length is final and reads never block.
struct StoreSource {
    store: Arc<Store>,
    cursor: usize,
    seekable: bool,
}

impl StoreSource {
    fn new(store: Arc<Store>, seekable: bool) -> Self {
        StoreSource {
            store,
            cursor: 0,
            seekable,
        }
    }
}

impl Read for StoreSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.store.read_at(&mut self.cursor, buffer)
    }
}

impl Seek for StoreSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if !self.seekable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "audio stream is not seekable",
            ));
        }
        let len = self.store.len() as i64;
        let complete = self.store.is_complete();
        let target = match position {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => self.cursor as i64 + offset,
            SeekFrom::End(offset) => {
                if !complete {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "the end of the audio stream is not known yet",
                    ));
                }
                len + offset
            }
        };
        if target < 0 || (complete && target > len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek past the end of the audio stream",
            ));
        }
        self.cursor = target as usize;
        Ok(target as u64)
    }
}

impl MediaSource for StoreSource {
    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn byte_len(&self) -> Option<u64> {
        if self.seekable {
            Some(self.store.len() as u64)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------------------------
// HTTP fetching and decryption

fn build_agent() -> ureq::Agent {
    // No `timeout_global`: it would be a whole-stream deadline and abort a
    // long track once back-pressure throttles reads. Connections are bounded at
    // setup, and each individual read is bounded, which is enough to detect a
    // dead peer without capping playback length.
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_per_read(Some(READ_TIMEOUT))
        .build();
    ureq::Agent::new_with_config(config)
}

/// Fetches every resource of the plan, in order, into the store.
fn fetch(plan: &Plan, agent: &ureq::Agent, store: &Store, stop: &AtomicBool) -> Result<(), String> {
    match plan {
        Plan::Single { url, key } => {
            let mut reader = open_body(agent, url)?;
            match key {
                Some((key, nonce)) => {
                    let mut reader = DecryptReader::new(reader, *key, *nonce);
                    pump(&mut reader, store, stop)
                }
                None => pump(&mut reader, store, stop),
            }
        }
        Plan::Segments(urls) => {
            for (index, url) in urls.iter().enumerate() {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let mut reader = open_body(agent, url)
                    .map_err(|error| format!("{error} (segment {} of {})", index + 1, urls.len()))?;
                pump(&mut reader, store, stop)?;
            }
            Ok(())
        }
        // Local files never go through the fetcher.
        Plan::File(_) => Ok(()),
    }
}

fn open_body(agent: &ureq::Agent, url: &str) -> Result<Box<dyn Read + Send>, String> {
    let response = agent
        .get(url)
        .call()
        .map_err(|error| format!("could not reach the audio server: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("audio server returned HTTP {status}"));
    }
    Ok(Box::new(response.into_body().into_reader()))
}

/// Drains a reader into the store until end of stream, failure, or stop.
fn pump<R: Read>(reader: &mut R, store: &Store, stop: &AtomicBool) -> Result<(), String> {
    let mut buffer = vec![0u8; FEED_CHUNK];
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if !store.push(&buffer[..count]) {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("error while streaming audio: {error}")),
        }
    }
}

/// Applies TIDAL's legacy AES-128-CTR stream cipher to a forward-only reader.
///
/// The keystream is the standard big-endian 128-bit counter mode: the IV is the
/// 8-byte nonce followed by a 64-bit block counter starting at zero, so the
/// keystream for byte `n` depends only on `n / 16`.
struct DecryptReader<R> {
    inner: R,
    cipher: Ctr128BE<Aes128>,
}

impl<R: Read> DecryptReader<R> {
    fn new(inner: R, key: [u8; 16], nonce: [u8; 8]) -> Self {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&nonce);
        DecryptReader {
            inner,
            cipher: Ctr128BE::<Aes128>::new(&key.into(), &iv.into()),
        }
    }
}

impl<R: Read> Read for DecryptReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.cipher.apply_keystream(&mut buffer[..count]);
        Ok(count)
    }
}

// ---------------------------------------------------------------------------------------------
// Shared playback state

/// A pending seek: where to go and whether that means going backwards (which
/// needs the fully buffered track rather than an in-place resync).
#[derive(Clone, Copy, Debug)]
struct SeekRequest {
    target: f64,
    backward: bool,
}

struct Shared {
    stop: Arc<AtomicBool>,
    /// Application-owned gain handle.
    volume: Volume,
    paused: AtomicBool,
    /// Decoded, output-layout samples waiting for the audio callback.
    samples: Mutex<VecDeque<f32>>,
    /// Wake-up signal for the producer when the callback drains the queue.
    signal: Condvar,
    /// Preferred number of queued samples; set once the output format is known.
    target: AtomicUsize,
    /// Output samples actually delivered since the last seek.
    samples_played: AtomicUsize,
    /// Output sample rate and channel count.
    out_rate: AtomicUsize,
    out_channels: AtomicUsize,
    /// Output frames to add to the played count (moves the clock after a seek).
    seek_offset_frames: AtomicUsize,
    /// Pending seek, consumed by the decode thread.
    seek_request: Mutex<Option<SeekRequest>>,
    /// Gain the output callback has ramped to (f32 bits; callback-owned).
    current_gain: AtomicU32,
    /// Gain change per output frame for the linear volume ramp (f32 bits).
    ramp_step: AtomicU32,
    /// Track duration in microseconds, or zero if unknown.
    duration_us: AtomicU64,
    /// Error reported by the audio device callback, if any.
    device_error: Mutex<Option<String>>,
    /// Final outcome, set once when the decoder thread returns.
    outcome: Mutex<Option<Result<Outcome, String>>>,
    /// The byte store the decode thread is reading from right now. It changes on a gapless
    /// hand-off, so commands like `stop` must wake whichever store is current.
    store: Mutex<Option<Arc<Store>>>,
    /// The format the output is open at, as reported to the UI.
    output: Mutex<Option<OutputInfo>>,
    /// A device change requested by [`Player::set_output_device`], consumed by the decode thread.
    device_request: Mutex<Option<Option<String>>>,
    /// The preloaded next track, if one is ready and not yet consumed, paired with its open
    /// demuxer session.
    preload: PreloadSlot<(Prepared, Session)>,
    /// The track id whose preload was swapped in and not yet reported.
    switched: Mutex<Option<u64>>,
}

impl Shared {
    fn new(stop: Arc<AtomicBool>, volume: Volume) -> Self {
        Shared {
            stop,
            volume,
            paused: AtomicBool::new(false),
            samples: Mutex::new(VecDeque::new()),
            signal: Condvar::new(),
            target: AtomicUsize::new(4096),
            samples_played: AtomicUsize::new(0),
            out_rate: AtomicUsize::new(0),
            out_channels: AtomicUsize::new(1),
            seek_offset_frames: AtomicUsize::new(0),
            seek_request: Mutex::new(None),
            current_gain: AtomicU32::new(1.0f32.to_bits()),
            ramp_step: AtomicU32::new(1.0f32.to_bits()),
            duration_us: AtomicU64::new(0),
            device_error: Mutex::new(None),
            outcome: Mutex::new(None),
            store: Mutex::new(None),
            output: Mutex::new(None),
            device_request: Mutex::new(None),
            preload: PreloadSlot::new(),
            switched: Mutex::new(None),
        }
    }

    /// Records the store the decode thread currently reads from.
    fn set_store(&self, store: Arc<Store>) {
        if let Ok(mut slot) = self.store.lock() {
            *slot = Some(store);
        }
    }

    /// The store the decode thread currently reads from, if playback has started.
    fn current_store(&self) -> Option<Arc<Store>> {
        self.store.lock().ok().and_then(|slot| slot.clone())
    }

    fn set_output(&self, info: OutputInfo) {
        if let Ok(mut slot) = self.output.lock() {
            *slot = Some(info);
        }
    }

    /// The format the output stream is open at.
    fn output_info(&self) -> Option<OutputInfo> {
        self.output.lock().ok().and_then(|slot| slot.clone())
    }

    /// Consumes a pending device change requested from the UI thread.
    fn take_device_request(&self) -> Option<Option<String>> {
        self.device_request.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Reserves the next preload generation. Any older preload becomes stale.
    fn next_preload_generation(&self) -> u64 {
        self.preload.next_generation()
    }

    /// Stores a finished preload, unless it has since been superseded.
    fn store_preload(&self, generation: u64, prepared: Prepared, session: Session) {
        self.preload.put(generation, (prepared, session));
    }

    /// Takes the ready preload, if any, for a gapless hand-off.
    fn take_preload(&self) -> Option<(Prepared, Session)> {
        self.preload.take()
    }

    /// Invalidates any outstanding preload and stops its fetcher.
    fn cancel_preload(&self) {
        self.preload.cancel();
    }

    /// Reports that the preloaded track is now playing.
    fn record_switch(&self, id: u64) {
        if let Ok(mut slot) = self.switched.lock() {
            *slot = Some(id);
        }
    }

    /// The preloaded track id that was swapped in, once.
    fn take_switch(&self) -> Option<u64> {
        self.switched.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Sizes the sample queue for about [`AUDIO_BUFFER_MS`] of output and
    /// configures the position clock, volume ramp and initial gain.
    fn set_target(&self, rate: u32, channels: usize) {
        let channels = channels.max(1);
        let samples = (rate as usize)
            .saturating_mul(channels)
            .saturating_mul(AUDIO_BUFFER_MS)
            / 1000;
        self.target.store(samples.max(1024), Ordering::Relaxed);
        self.out_rate.store(rate as usize, Ordering::Relaxed);
        self.out_channels.store(channels, Ordering::Relaxed);
        let step = if rate == 0 {
            1.0
        } else {
            1.0 / (rate as f32 * VOLUME_RAMP_SECONDS)
        };
        self.ramp_step
            .store(step.clamp(f32::MIN_POSITIVE, 1.0).to_bits(), Ordering::Relaxed);
        self.current_gain
            .store(self.volume.gain().to_bits(), Ordering::Relaxed);
    }

    fn set_duration(&self, micros: u64) {
        self.duration_us.store(micros, Ordering::Relaxed);
    }

    /// Output frames played, including the offset applied by the last seek.
    fn position_frames(&self) -> u64 {
        let channels = self.out_channels.load(Ordering::Relaxed).max(1) as u64;
        self.seek_offset_frames.load(Ordering::Relaxed) as u64
            + self.samples_played.load(Ordering::Relaxed) as u64 / channels
    }

    fn position_seconds(&self) -> u64 {
        let rate = self.out_rate.load(Ordering::Relaxed) as u64;
        if rate == 0 {
            return 0;
        }
        self.position_frames() / rate
    }

    fn position_micros(&self) -> u64 {
        let rate = self.out_rate.load(Ordering::Relaxed) as u64;
        if rate == 0 {
            return 0;
        }
        self.position_frames().saturating_mul(1_000_000) / rate
    }

    /// Moves the position clock to `seconds` and restarts the played count.
    fn set_seek_position(&self, seconds: f64) {
        let rate = self.out_rate.load(Ordering::Relaxed) as f64;
        let frames = if rate > 0.0 {
            (seconds.max(0.0) * rate).round() as usize
        } else {
            0
        };
        self.seek_offset_frames.store(frames, Ordering::Relaxed);
        self.samples_played.store(0, Ordering::Relaxed);
    }

    fn take_seek_request(&self) -> Option<SeekRequest> {
        self.seek_request.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Drops buffered audio, e.g. across a seek discontinuity.
    fn clear_queue(&self) {
        if let Ok(mut queue) = self.samples.lock() {
            queue.clear();
        }
        self.signal.notify_all();
    }

    fn queued(&self) -> usize {
        self.samples.lock().map(|queue| queue.len()).unwrap_or(0)
    }

    fn device_error(&self) -> Option<String> {
        self.device_error.lock().ok().and_then(|slot| slot.clone())
    }

    fn record_outcome(&self, outcome: Result<Outcome, String>) {
        if let Ok(mut slot) = self.outcome.lock() {
            if slot.is_none() {
                *slot = Some(outcome);
            }
        }
    }

    /// Appends samples, blocking while the queue is full so decoding cannot run
    /// arbitrarily far ahead of real-time playback.
    fn push_samples(&self, samples: &[f32]) {
        let mut queue = match self.samples.lock() {
            Ok(queue) => queue,
            Err(_) => return,
        };
        let mut written = 0usize;
        while written < samples.len() {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            if self.device_error().is_some() {
                return;
            }
            if queue.len() >= self.target.load(Ordering::Relaxed) && written == 0 {
                queue = match self.signal.wait_timeout(queue, Duration::from_millis(20)) {
                    Ok((queue, _)) => queue,
                    Err(_) => return,
                };
                continue;
            }
            queue.extend(samples[written..].iter().copied());
            written = samples.len();
        }
        drop(queue);
        self.signal.notify_all();
    }
}

// ---------------------------------------------------------------------------------------------
// Demuxer session

/// An open container plus a decoder, i.e. everything that has to be replaced
/// when the stream is re-opened for a backward seek.
struct Session {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    rate: u32,
    channels: usize,
    duration_us: u64,
}

impl Session {
    /// Opens `source`, probes the container, and prepares a decoder for the
    /// default track.
    fn open(plan: &Plan, source: Box<dyn MediaSource>, store: &Store) -> Result<Session, String> {
        let stream = MediaSourceStream::new(
            source,
            MediaSourceStreamOptions {
                buffer_len: PROBE_BUFFER,
            },
        );
        let mut hint = Hint::new();
        hint.with_extension(plan.hint_extension());
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|error| {
                store
                    .error()
                    .unwrap_or_else(|| format!("could not open the audio stream: {error}"))
            })?;

        let format = probed.format;
        let (track_id, codec_params) = {
            let track = format
                .default_track()
                .ok_or_else(|| "the audio stream contains no audio track".to_string())?;
            (track.id, track.codec_params.clone())
        };
        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|error| format!("unsupported audio codec: {error}"))?;

        let rate = codec_params.sample_rate.unwrap_or(0);
        let channels = codec_params.channels.map(|channels| channels.count()).unwrap_or(0);
        let duration_us = match (codec_params.n_frames, codec_params.sample_rate) {
            (Some(frames), Some(rate)) if rate > 0 => frames.saturating_mul(1_000_000) / u64::from(rate),
            _ => 0,
        };

        Ok(Session {
            format,
            decoder,
            track_id,
            rate,
            channels,
            duration_us,
        })
    }

    /// The number of seconds at timestamp `ts` in this container's time base.
    fn seconds_of(&self, ts: u64) -> f64 {
        let time_base = self
            .format
            .default_track()
            .and_then(|track| track.codec_params.time_base);
        match time_base {
            Some(time_base) => {
                let time = time_base.calc_time(ts);
                time.seconds as f64 + time.frac
            }
            None if self.rate > 0 => ts as f64 / f64::from(self.rate),
            None => 0.0,
        }
    }

    /// Repositions the container to `seconds`. Returns the actual position the
    /// demuxer landed on.
    fn seek(&mut self, seconds: f64) -> Result<f64, String> {
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::from(seconds.max(0.0)),
                    track_id: None,
                },
            )
            .map_err(|error| format!("could not seek: {error}"))?;
        self.decoder.reset();
        Ok(self.seconds_of(seeked.actual_ts))
    }

    /// Last-resort seek: decode from the start and discard packets until the
    /// target time is reached. Returns the (approximate) position reached.
    fn fast_forward(&mut self, seconds: f64) -> Result<f64, String> {
        let rate = self.rate.max(1);
        let target_ts = TimeBase::new(1, rate).calc_timestamp(Time::from(seconds.max(0.0)));
        let mut last_ts = 0u64;
        loop {
            match self.format.next_packet() {
                Ok(packet) => {
                    if packet.track_id() != self.track_id {
                        continue;
                    }
                    last_ts = packet.ts();
                    if last_ts >= target_ts {
                        break;
                    }
                }
                Err(SymphoniaError::IoError(error))
                    if error.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(SymphoniaError::ResetRequired) => continue,
                Err(error) => return Err(format!("could not seek: {error}")),
            }
        }
        self.decoder.reset();
        let reached = last_ts.min(target_ts);
        Ok(reached as f64 / f64::from(rate))
    }
}

// ---------------------------------------------------------------------------------------------
// Next-track preload (gapless hand-off)

/// Something that must be released when it is dropped without being consumed.
trait Abandon {
    fn abandon(&self);
}

impl Abandon for (Prepared, Session) {
    fn abandon(&self) {
        // Aborting the store stops the abandoned track's fetch thread.
        self.0.store.abort();
    }
}

/// A single-slot mailbox where every value is tagged with the generation that produced it.
///
/// Gapless playback needs exactly one preloaded track at a time, and that track must be thrown
/// away the moment it stops being the right one (the queue changed, or a newer preload landed).
/// Storing the generation alongside the value makes that a single atomic comparison: a `put` from
/// a stale generation, a `put` that displaces a live value, and an explicit `cancel` all abandon
/// what they replace, so nothing leaks and no obsolete track can ever be swapped in.
struct PreloadSlot<T: Abandon> {
    slot: Mutex<Option<T>>,
    generation: AtomicU64,
}

impl<T: Abandon> PreloadSlot<T> {
    fn new() -> Self {
        PreloadSlot {
            slot: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }

    /// Reserves a generation for a new producer to work under.
    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Publishes `payload` produced under `generation`.
    ///
    /// Returns `false` (abandoning the payload) when a newer generation has since been reserved,
    /// i.e. the work was invalidated while it ran.
    fn put(&self, generation: u64, payload: T) -> bool {
        if generation != self.generation.load(Ordering::SeqCst) {
            payload.abandon();
            return false;
        }
        let Ok(mut slot) = self.slot.lock() else {
            payload.abandon();
            return false;
        };
        if let Some(previous) = slot.replace(payload) {
            previous.abandon();
        }
        true
    }

    /// Takes the stored value, if any.
    fn take(&self) -> Option<T> {
        self.slot.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Invalidates anything stored or in flight, abandoning whatever was there.
    fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(payload) = self.take() {
            payload.abandon();
        }
    }
}

/// A fully-opened next track, ready to be swapped in at end of stream.
///
/// Everything expensive — opening the source, probing the container, decoding the first audio
/// buffer and opening the demuxer — has already happened, so the swap itself costs nothing but a
/// few pointer moves. `store` is kept so a cancelled or superseded preload can be aborted.
struct Prepared {
    /// The track id this preload belongs to, echoed back once it is playing.
    id: u64,
    plan: Plan,
    store: Arc<Store>,
    /// The first decoded audio buffer, already imported to interleaved `f32`.
    first: Vec<f32>,
    /// Sample rate and channel count of that buffer.
    rate: u32,
    channels: usize,
    duration_us: u64,
    /// The ReplayGain multiplier the next track wants, applied at the hand-off.
    normalize_gain: f32,
}

/// Opens `plan` for streaming and decodes far enough to have real audio.
///
/// Spawns the source's fetch thread (which is detached and bounded by `stop`), opens the
/// container, and decodes packets until the first non-empty audio buffer, returning the session,
/// that buffer, and its format. Shared by [`playback`] and the preload thread so both see
/// exactly the same failure modes.
fn prepare(
    plan: &Plan,
    stop: &Arc<AtomicBool>,
    store: &Arc<Store>,
) -> Result<(Session, Vec<f32>, u32, usize), String> {
    // A local file needs no fetcher: it is read (and seeked) straight from disk.
    if !plan.is_file() {
        let fetch_agent = build_agent();
        let fetch_store = store.clone();
        let fetch_stop = stop.clone();
        let fetch_plan = plan.clone();
        let fetcher = thread::Builder::new()
            .name("tsun-audio-fetch".to_string())
            .spawn(move || {
                let result = fetch(&fetch_plan, &fetch_agent, &fetch_store, &fetch_stop);
                fetch_store.finish(result.err());
            })
            .map_err(|error| format!("could not start the audio fetch thread: {error}"))?;
        // From here on the fetcher must not outlive its caller: it is signalled via `stop` and
        // `store.abort`, and detached rather than joined so a stalled socket cannot delay
        // shutdown.
        drop(fetcher);
    }

    // Start with a forward-only (streaming) view so audio begins before the whole track has
    // downloaded.
    let mut session = Session::open(plan, plan.open_source(store, plan.is_file())?, store)?;

    // Decode until the first real audio buffer so that `start` reports decode failures and so
    // the output device can be opened at the right rate.
    let mut scratch: Option<SampleBuffer<f32>> = None;
    let mut first_samples: Vec<f32> = Vec::new();
    let (source_rate, source_channels) = loop {
        if stop.load(Ordering::SeqCst) {
            return Err("playback was stopped before it started".to_string());
        }
        let packet_result = session.format.next_packet();
        let packet = match packet_result {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == io::ErrorKind::UnexpectedEof =>
            {
                return Err("the audio stream ended before any audio was decoded".to_string());
            }
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::Interrupted => {
                return Err("playback was stopped before it started".to_string());
            }
            Err(SymphoniaError::ResetRequired) => {
                continue;
            }
            Err(error) => return Err(format!("could not read the audio stream: {error}")),
        };
        if packet.track_id() != session.track_id {
            continue;
        }
        match session.decoder.decode(&packet) {
            Ok(audio) => {
                let spec = *audio.spec();
                let rate = spec.rate;
                let channels = spec.channels.count();
                if channels == 0 {
                    return Err("the audio stream has no channels".to_string());
                }
                import(audio, &mut scratch, &mut first_samples);
                if first_samples.is_empty() {
                    continue;
                }
                session.rate = rate;
                session.channels = channels;
                break (rate, channels);
            }
            // Recoverable: skip the damaged packet and keep going.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(error))
                if error.kind() == io::ErrorKind::UnexpectedEof =>
            {
                return Err("the audio stream ended before any audio was decoded".to_string());
            }
            Err(error) => return Err(format!("could not decode the audio stream: {error}")),
        }
    };

    Ok((session, first_samples, source_rate, source_channels))
}

/// Prepares the next track and publishes it for a gapless hand-off. Runs on its own thread; a
/// failure (a broken source, a cancelled preload, a stop) publishes nothing and the normal
/// end-of-track path takes over.
fn preload(
    plan: Plan,
    id: u64,
    normalize_gain: f32,
    stop: Arc<AtomicBool>,
    store: Arc<Store>,
    shared: Arc<Shared>,
    generation: u64,
) {
    let Ok((session, first, rate, channels)) = prepare(&plan, &stop, &store) else {
        store.abort();
        return;
    };
    let duration_us = session.duration_us;
    shared.store_preload(
        generation,
        Prepared {
            id,
            plan,
            store,
            first,
            rate,
            channels,
            duration_us,
            normalize_gain,
        },
        session,
    );
}

// ---------------------------------------------------------------------------------------------
// Playback thread

fn run(
    plan: Plan,
    stop: Arc<AtomicBool>,
    store: Arc<Store>,
    shared: Arc<Shared>,
    ready: SyncSender<Result<(), String>>,
    start_position: f64,
    output: OutputOptions,
) {
    let result = playback(plan, &stop, store, &shared, &ready, start_position, &output);
    // If `playback` failed before signalling readiness, `start` is still
    // waiting; hand it the error. If it already signalled `Ok`, this send either
    // lands in the (now unused) channel or fails, and the caller reads the
    // outcome from `shared` instead.
    if let Err(message) = &result {
        let _ = ready.try_send(Err(message.clone()));
    }
    shared.record_outcome(result);
    if let Some(store) = shared.current_store() {
        store.abort();
    }
}

/// Opens the output device named by `device` (or the host default) at `rate`/`channels`,
/// publishes the resulting format, and starts it. The previous output must already be closed so
/// the device is free to be re-opened.
fn open_output(
    device: &Option<String>,
    rate: u32,
    channels: usize,
    shared: &Arc<Shared>,
) -> Result<(AudioOutput, u32, usize), String> {
    let output = AudioOutput::open(rate, channels, shared, device.as_deref())?;
    let (out_rate, out_channels) = (output.rate, output.channels);
    shared.set_target(out_rate, out_channels);
    output.play()?;
    Ok((output, out_rate, out_channels))
}

fn playback(
    mut plan: Plan,
    stop: &Arc<AtomicBool>,
    mut store: Arc<Store>,
    shared: &Arc<Shared>,
    ready: &SyncSender<Result<(), String>>,
    start_position: f64,
    output_options: &OutputOptions,
) -> Result<Outcome, String> {
    // The device the output is open on; updated when the user picks another one.
    let mut device = output_options.device.clone();

    // Fetch thread: owns the network reader so no non-`Send` handle crosses a
    // thread boundary. Opening the container and decoding the first buffer is shared
    // with the preload path.
    let (mut session, first_samples, mut source_rate, mut source_channels) =
        prepare(&plan, stop, &store)?;
    shared.set_duration(session.duration_us);

    let (mut output, mut out_rate, mut out_channels) =
        open_output(&device, source_rate, source_channels, shared)?;

    let mut converter = Converter::new(source_rate, source_channels, out_rate, out_channels);
    let mut converted: Vec<f32> = Vec::new();
    converter.push(&first_samples, &mut converted);
    shared.push_samples(&converted);

    if ready.try_send(Ok(())).is_err() {
        // `start` gave up waiting; abandon playback.
        stop.store(true, Ordering::SeqCst);
        return Ok(Outcome::Stopped);
    }

    // Honour a `start_at` position by seeking before the main loop.
    if start_position > 0.0 {
        shared.clear_queue();
        let request = SeekRequest {
            target: start_position,
            backward: false,
        };
        match apply_seek(&plan, &store, stop, shared, &mut session, request) {
            Ok(()) => converter = Converter::new(session.rate, session.channels, out_rate, out_channels),
            Err(error) => {
                if stop.load(Ordering::SeqCst) {
                    return Ok(Outcome::Stopped);
                }
                return Err(error);
            }
        }
    }

    // Main decode loop.
    let mut stopped_early = false;
    let mut scratch: Option<SampleBuffer<f32>> = None;
    let mut decoded: Vec<f32> = Vec::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            stopped_early = true;
            break;
        }
        if let Some(message) = shared.device_error() {
            return Err(message);
        }
        // The user picked another output device: re-open it and carry on at the
        // position that was playing. A device that has vanished ends playback with an
        // error rather than pretending to play.
        if let Some(request) = shared.take_device_request() {
            let position = shared.position_micros() as f64 / 1_000_000.0;
            output.close();
            match open_output(&request, session.rate, session.channels, shared) {
                Ok((fresh, rate, channels)) => {
                    output = fresh;
                    out_rate = rate;
                    out_channels = channels;
                    device = request;
                    converter = Converter::new(session.rate, session.channels, out_rate, out_channels);
                    shared.clear_queue();
                    shared.set_seek_position(position);
                }
                Err(error) => {
                    if stop.load(Ordering::SeqCst) {
                        stopped_early = true;
                        break;
                    }
                    return Err(error);
                }
            }
            continue;
        }
        if let Some(request) = shared.take_seek_request() {
            shared.clear_queue();
            match apply_seek(&plan, &store, stop, shared, &mut session, request) {
                Ok(()) => {
                    converter = Converter::new(session.rate, session.channels, out_rate, out_channels);
                    continue;
                }
                Err(error) => {
                    if stop.load(Ordering::SeqCst) {
                        stopped_early = true;
                        break;
                    }
                    return Err(error);
                }
            }
        }

        let packet_result = session.format.next_packet();
        let packet = match packet_result {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == io::ErrorKind::UnexpectedEof =>
            {
                // End of this track. If the next one was preloaded, swap it in
                // without touching the output stream: that is the whole point of
                // gapless playback.
                if stop.load(Ordering::SeqCst) {
                    stopped_early = true;
                    break;
                }
                let Some((next, next_session)) = shared.take_preload() else { break };
                shared.volume.set_normalization_gain(next.normalize_gain);
                if next.rate != out_rate || next.channels != out_channels {
                    // The one case that cannot stay seamless: the device has to be
                    // re-opened at the new track's rate.
                    output.close();
                    let (fresh, rate, channels) =
                        open_output(&device, next.rate, next.channels, shared)?;
                    output = fresh;
                    out_rate = rate;
                    out_channels = channels;
                }
                plan = next.plan;
                store = next.store;
                session = next_session;
                source_rate = next.rate;
                source_channels = next.channels;
                shared.set_store(store.clone());
                shared.set_duration(next.duration_us);
                converter = Converter::new(source_rate, source_channels, out_rate, out_channels);
                // Buffered audio from the previous track keeps playing; the clock
                // restarts for the new one.
                shared.set_seek_position(0.0);
                converted.clear();
                converter.push(&next.first, &mut converted);
                shared.push_samples(&converted);
                shared.record_switch(next.id);
                continue;
            }
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::Interrupted => {
                stopped_early = true;
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                if let Some(track) = session.format.default_track() {
                    if let Ok(fresh) = symphonia::default::get_codecs()
                        .make(&track.codec_params, &DecoderOptions::default())
                    {
                        session.decoder = fresh;
                    }
                }
                continue;
            }
            Err(error) => return Err(format!("could not read the audio stream: {error}")),
        };
        if packet.track_id() != session.track_id {
            continue;
        }

        match session.decoder.decode(&packet) {
            Ok(audio) => {
                let spec = *audio.spec();
                let rate = spec.rate;
                let channels = spec.channels.count().max(1);
                decoded.clear();
                import(audio, &mut scratch, &mut decoded);
                if decoded.is_empty() {
                    continue;
                }
                session.rate = rate;
                session.channels = channels;
                if rate != source_rate || channels != source_channels {
                    converter = Converter::new(rate, channels, out_rate, out_channels);
                }
                converted.clear();
                converter.push(&decoded, &mut converted);
                shared.push_samples(&converted);
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(error))
                if error.kind() == io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => return Err(format!("could not decode the audio stream: {error}")),
        }
    }
    if !stop.load(Ordering::SeqCst) {
        if let Some(error) = store.error() {
            return Err(error);
        }
    }

    // Natural end: let the queued audio drain so the tail is not cut off.
    if !stopped_early && !stop.load(Ordering::SeqCst) && shared.device_error().is_none() {
        while shared.queued() > 0 {
            if stop.load(Ordering::SeqCst) || shared.device_error().is_some() {
                stopped_early = true;
                break;
            }
            thread::sleep(Duration::from_millis(15));
        }
    }
    stop.store(true, Ordering::SeqCst);

    if let Some(message) = shared.device_error() {
        return Err(message);
    }
    if stopped_early {
        return Ok(Outcome::Stopped);
    }
    Ok(Outcome::Finished)
}

/// Repositions playback to `request`, updating the position clock.
///
/// Forward seeks use the demuxer's in-place resync (streaming as needed);
/// backward seeks wait for the whole track to buffer and re-open the container
/// with random access.
fn apply_seek(
    plan: &Plan,
    store: &Arc<Store>,
    stop: &Arc<AtomicBool>,
    shared: &Arc<Shared>,
    session: &mut Session,
    request: SeekRequest,
) -> Result<(), String> {
    let target = request.target;

    // A local file is seekable in place from the start; if its container refuses, re-open the
    // file and try again (or decode forward as the last resort).
    if plan.is_file() {
        match session.seek(target) {
            Ok(actual) => {
                shared.set_seek_position(actual);
                return Ok(());
            }
            Err(_) => {
                let mut fresh = Session::open(plan, plan.open_source(store, true)?, store)?;
                let actual = match fresh.seek(target) {
                    Ok(actual) => actual,
                    Err(_) => fresh.fast_forward(target)?,
                };
                *session = fresh;
                shared.set_seek_position(actual);
                return Ok(());
            }
        }
    }

    if request.backward {
        // Backward seek: needs the buffered track.
        if !store.wait_complete() {
            if stop.load(Ordering::SeqCst) {
                return Err("playback stopped".to_string());
            }
            return Err(store.error().unwrap_or_else(|| {
                "the audio stream ended before the seek could complete".to_string()
            }));
        }
        let mut fresh = Session::open(plan, Box::new(StoreSource::new(store.clone(), true)), store)?;
        let actual = match fresh.seek(target) {
            Ok(actual) => actual,
            Err(_) => fresh.fast_forward(target)?,
        };
        *session = fresh;
        shared.set_seek_position(actual);
        return Ok(());
    }

    // Forward (or negligible) seek: resync in place.
    match session.seek(target) {
        Ok(actual) => {
            shared.set_seek_position(actual);
            Ok(())
        }
        Err(_) => {
            // The container cannot seek forward in place. Buffer the track and
            // re-open with random access.
            if !store.wait_complete() {
                if stop.load(Ordering::SeqCst) {
                    return Err("playback stopped".to_string());
                }
                return Err(store.error().unwrap_or_else(|| {
                    "the audio stream ended before the seek could complete".to_string()
                }));
            }
            let mut fresh = Session::open(plan, Box::new(StoreSource::new(store.clone(), true)), store)?;
            let actual = match fresh.seek(target) {
                Ok(actual) => actual,
                Err(_) => fresh.fast_forward(target)?,
            };
            *session = fresh;
            shared.set_seek_position(actual);
            Ok(())
        }
    }
}

/// Copies one decoded buffer into `samples` as interleaved `f32`.
fn import(audio: AudioBufferRef, scratch: &mut Option<SampleBuffer<f32>>, samples: &mut Vec<f32>) {
    let spec = *audio.spec();
    let frames = audio.frames();
    let needed = frames * spec.channels.count();
    let recreate = match scratch {
        Some(buffer) => buffer.capacity() < needed,
        None => true,
    };
    if recreate {
        *scratch = Some(SampleBuffer::<f32>::new(frames.max(1) as u64, spec));
    }
    if let Some(buffer) = scratch.as_mut() {
        buffer.copy_interleaved_ref(audio);
        samples.extend_from_slice(buffer.samples());
    }
}

// ---------------------------------------------------------------------------------------------
// Output device

/// An open output stream and the format it runs at.
struct AudioOutput {
    /// `None` once [`AudioOutput::close`] has released the device for a re-open.
    stream: Option<cpal::Stream>,
    rate: u32,
    channels: usize,
}

impl AudioOutput {
    /// Opens `device_name` (or the host default when `None`) at the stream's own format when the
    /// device advertises it, otherwise at the closest advertised format (with resampling).
    ///
    /// A raw ALSA `hw:` device is just another name here: choosing it is what gives an
    /// exclusive-ish, one-client path when that is what the user wants.
    fn open(
        source_rate: u32,
        source_channels: usize,
        shared: &Arc<Shared>,
        device_name: Option<&str>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .output_devices()
                .ok()
                .and_then(|mut devices| {
                    devices.find(|device| device.name().ok().as_deref() == Some(name))
                })
                .ok_or_else(|| {
                    format!(
                        "the audio output device “{name}” is not available; pick another one in Settings"
                    )
                })?,
            None => host.default_output_device().ok_or_else(|| {
                if cfg!(target_os = "linux") {
                    "no audio output device is available; check that PipeWire/PulseAudio (or ALSA) is running and a default sink exists".to_string()
                } else {
                    "no audio output device is available; check the system's sound settings".to_string()
                }
            })?,
        };
        let label = device.name().unwrap_or_else(|_| "audio output".to_string());

        // Everything the device advertises that we know how to build. Devices can advertise
        // configurations that then fail to open, so a failure falls through to the next
        // candidate and, ultimately, to the device default.
        let mut configs: Vec<(usize, u32, u32, SampleFormat)> = Vec::new();
        if let Ok(supported) = device.supported_output_configs() {
            for config in supported {
                if is_supported_format(config.sample_format()) {
                    configs.push((
                        config.channels() as usize,
                        config.min_sample_rate().0,
                        config.max_sample_rate().0,
                        config.sample_format(),
                    ));
                }
            }
        }
        let advertised: Vec<(usize, u32, u32)> =
            configs.iter().map(|&(channels, min, max, _)| (channels, min, max)).collect();

        let mut closest_error: Option<String> = None;
        if let Some((channels, rate)) = choose_config(&advertised, source_rate, source_channels) {
            for &(config_channels, _, _, format) in configs.iter().filter(|entry| entry.0 == channels)
            {
                let stream_config = cpal::StreamConfig {
                    channels: config_channels as u16,
                    sample_rate: cpal::SampleRate(rate),
                    buffer_size: cpal::BufferSize::Default,
                };
                match build_stream(&device, &stream_config, format, shared) {
                    Ok(stream) => {
                        return Ok(AudioOutput::new(stream, rate, channels, label, format, shared))
                    }
                    Err(error) => closest_error = Some(error),
                }
            }
        }

        let default = match device.default_output_config() {
            Ok(default) => default,
            Err(error) => {
                return Err(match closest_error {
                    Some(closest) => format!(
                        "no usable audio output configuration is available for “{label}”: {error} (and opening the closest advertised format failed: {closest})"
                    ),
                    None => format!(
                        "no usable audio output configuration is available for “{label}”: {error}"
                    ),
                });
            }
        };
        let format = default.sample_format();
        if !is_supported_format(format) {
            return Err(format!(
                "the audio output “{label}” uses an unsupported sample format ({format})"
            ));
        }
        let stream_config: cpal::StreamConfig = default.config();
        let rate = stream_config.sample_rate.0;
        let channels = stream_config.channels as usize;
        match build_stream(&device, &stream_config, format, shared) {
            Ok(stream) => Ok(AudioOutput::new(stream, rate, channels, label, format, shared)),
            Err(error) => Err(match closest_error {
                Some(closest) => format!(
                    "{error} (and opening the closest advertised format failed: {closest})"
                ),
                None => error,
            }),
        }
    }

    /// Wraps `stream` and publishes the format so the UI can show what is really playing.
    fn new(
        stream: cpal::Stream,
        rate: u32,
        channels: usize,
        device: String,
        format: SampleFormat,
        shared: &Arc<Shared>,
    ) -> Self {
        shared.set_output(OutputInfo {
            device,
            rate,
            channels,
            sample_format: format.to_string(),
        });
        AudioOutput {
            stream: Some(stream),
            rate,
            channels,
        }
    }

    fn play(&self) -> Result<(), String> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| "the audio output stream was closed".to_string())?;
        stream
            .play()
            .map_err(|error| format!("could not start audio output: {error}"))
    }

    /// Releases the device, so a new stream can be opened (on this device or another). Dropping
    /// a `cpal` stream is what closes it.
    fn close(&mut self) {
        self.stream = None;
    }
}

/// Picks the channel/rate to open from a device's advertised configurations.
///
/// `supported` is `(channels, min_rate, max_rate)` per advertised configuration. The choice
/// prefers the source's channel count (so nothing is remixed) and then the sample rate closest to
/// the source's (so a 96 kHz track stays 96 kHz when the device can do it, and otherwise lands on
/// the nearest rate that will be resampled). Ties break toward the higher rate, which sounds
/// better than truncating the band. `None` when the device advertises nothing usable.
fn choose_config(
    supported: &[(usize, u32, u32)],
    source_rate: u32,
    source_channels: usize,
) -> Option<(usize, u32)> {
    if supported.is_empty() {
        return None;
    }
    let source_channels = source_channels.max(1);
    let source_rate = source_rate.max(1);
    let mut candidates: Vec<(usize, u32)> = supported
        .iter()
        .map(|&(channels, min, max)| {
            let low = min.max(1);
            let high = max.max(low);
            (channels.max(1), source_rate.clamp(low, high))
        })
        .collect();
    candidates.sort_by_key(|&(channels, rate)| {
        (
            (channels as i64 - source_channels as i64).unsigned_abs(),
            (rate as i64 - source_rate as i64).unsigned_abs(),
            std::cmp::Reverse(rate),
        )
    });
    candidates.first().copied()
}

fn is_supported_format(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::U16
            | SampleFormat::U32
    )
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, String> {
    let result = match format {
        SampleFormat::F32 => build_typed::<f32>(device, config, shared),
        SampleFormat::F64 => build_typed::<f64>(device, config, shared),
        SampleFormat::I16 => build_typed::<i16>(device, config, shared),
        SampleFormat::I32 => build_typed::<i32>(device, config, shared),
        SampleFormat::U16 => build_typed::<u16>(device, config, shared),
        SampleFormat::U32 => build_typed::<u32>(device, config, shared),
        other => return Err(format!("unsupported output sample format ({other})")),
    };
    result.map_err(|error| format!("could not open the audio output stream: {error}"))
}

fn build_typed<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let data_shared = shared.clone();
    let error_shared = shared.clone();
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| fill::<T>(data, &data_shared),
        move |error: cpal::StreamError| {
            if let Ok(mut slot) = error_shared.device_error.lock() {
                if slot.is_none() {
                    *slot = Some(format!("audio output device error: {error}"));
                }
            }
        },
        None,
    )
}

/// Audio callback: copies queued samples out with the volume ramp applied,
/// outputting silence on underrun, while paused, or after a stop.
///
/// At volume 100%, unmuted and with normalisation off the gain is exactly 1.0, and the sample is
/// copied through untouched: no multiply, no dither, nothing between the decoder and the device.
fn fill<T: Sample + FromSample<f32>>(data: &mut [T], shared: &Shared) {
    let channels = shared.out_channels.load(Ordering::Relaxed).max(1);
    let active = !shared.stop.load(Ordering::SeqCst) && !shared.paused.load(Ordering::SeqCst);
    let target = if active { shared.volume.gain() } else { 0.0 };
    let step = f32::from_bits(shared.ramp_step.load(Ordering::Relaxed));
    let mut gain = f32::from_bits(shared.current_gain.load(Ordering::Relaxed));

    let mut queue = match shared.samples.lock() {
        Ok(queue) => queue,
        Err(_) => {
            for sample in data.iter_mut() {
                *sample = T::from_sample(0.0f32);
            }
            return;
        }
    };

    let mut consumed = 0usize;
    for (index, sample) in data.iter_mut().enumerate() {
        // Advance the ramp once per output frame so a gain change is spread
        // over `VOLUME_RAMP_SECONDS` instead of stepping at a buffer boundary.
        if index % channels == 0 {
            gain = ramp_toward(gain, target, step);
        }
        // Once a pause/mute has fully faded out, stop consuming audio so the
        // position clock does not advance and nothing is lost on resume.
        if !active && gain <= 0.0 {
            *sample = T::from_sample(0.0f32);
            continue;
        }
        match queue.pop_front() {
            // Unity gain: the bit-perfect pass-through.
            Some(value) if gain == 1.0 => {
                *sample = T::from_sample(value);
                consumed += 1;
            }
            Some(value) => {
                *sample = T::from_sample(value * gain);
                consumed += 1;
            }
            None => *sample = T::from_sample(0.0f32),
        }
    }

    shared.samples_played.fetch_add(consumed, Ordering::Relaxed);
    drop(queue);
    shared.current_gain.store(gain.to_bits(), Ordering::Relaxed);
    if consumed > 0 {
        shared.signal.notify_all();
    }
}

/// Moves `current` toward `target` by at most `step`, giving a linear,
/// click-free gain ramp that never overshoots.
fn ramp_toward(current: f32, target: f32, step: f32) -> f32 {
    if current < target {
        (current + step).min(target)
    } else if current > target {
        (current - step).max(target)
    } else {
        current
    }
}

// ---------------------------------------------------------------------------------------------
// Sample conversion

/// Maps decoded audio onto the output device's rate and channel layout.
///
/// When the source and the device already run at the same rate there is nothing to do: samples
/// are only mapped frame by frame, so the signal is untouched. Otherwise a
/// [`Resampler`] converts the rate first and the channel map follows.
struct Converter {
    source_channels: usize,
    output_channels: usize,
    resampler: Option<Resampler>,
    /// Output-rate samples awaiting the channel map (resampling path only).
    scratch: Vec<f32>,
}

impl Converter {
    fn new(
        source_rate: u32,
        source_channels: usize,
        output_rate: u32,
        output_channels: usize,
    ) -> Self {
        let source_channels = source_channels.max(1);
        let output_channels = output_channels.max(1);
        let resampler = (source_rate != output_rate && source_rate > 0 && output_rate > 0)
            .then(|| Resampler::new(source_rate, output_rate, source_channels));
        Converter {
            source_channels,
            output_channels,
            resampler,
            scratch: Vec::new(),
        }
    }

    /// Converts one buffer of interleaved source samples into output samples.
    fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let Converter {
            source_channels,
            output_channels,
            resampler,
            scratch,
        } = self;
        let source = match resampler {
            Some(resampler) => {
                scratch.clear();
                resampler.push(input, scratch);
                &scratch[..]
            }
            None => input,
        };
        for frame in source.chunks_exact(*source_channels) {
            map_frame(*source_channels, *output_channels, frame, output);
        }
    }
}

/// Maps one source frame onto the output channel layout: unchanged when the counts match, a
/// mean when collapsing to mono, otherwise the first channels repeated for the last one.
fn map_frame(
    source_channels: usize,
    output_channels: usize,
    frame: &[f32],
    output: &mut Vec<f32>,
) {
    let frame = &frame[..frame.len().min(source_channels)];
    if frame.is_empty() {
        return;
    }
    if output_channels == source_channels {
        output.extend_from_slice(frame);
    } else if output_channels == 1 {
        let mut sum = 0.0f32;
        for value in frame {
            sum += *value;
        }
        output.push(sum / frame.len() as f32);
    } else {
        for channel in 0..output_channels {
            let index = if output_channels > frame.len() {
                channel.min(frame.len() - 1)
            } else {
                channel
            };
            output.push(frame[index]);
        }
    }
}

/// A windowed-sinc (band-limited) sample-rate converter over interleaved `f32` frames.
///
/// Each output sample is the convolution of the source with a Hann-windowed sinc low-pass,
/// re-evaluated at the exact fractional position, and normalised by the sum of its taps so a
/// constant maps to the same constant (perfect DC), regardless of phase. Upsampling keeps the
/// full band (`cutoff = 1`); downsampling places the cutoff at the new Nyquist so nothing aliases
/// back. The interpolation is causal, so the output lags the input by [`RESAMPLE_HALF_TAPS`]
/// frames (half a millisecond at 48 kHz).
struct Resampler {
    channels: usize,
    /// Source frames advanced per output frame.
    step: f64,
    /// Low-pass cutoff as a fraction of the source Nyquist.
    cutoff: f64,
    /// Context needed on each side of the interpolation point, in source frames.
    half: i32,
    /// Interleaved source frames, preceded by `half` zero frames of lead-in. `position` is the
    /// next output sample's position within it, in frames.
    history: Vec<f32>,
    position: f64,
    /// Whether the lead-in has been added.
    seeded: bool,
    /// Reused kernel weights, one per tap.
    taps: Vec<f32>,
}

impl Resampler {
    fn new(source_rate: u32, output_rate: u32, channels: usize) -> Self {
        let ratio = output_rate as f64 / source_rate.max(1) as f64;
        Resampler {
            channels: channels.max(1),
            step: 1.0 / ratio,
            cutoff: ratio.min(1.0),
            half: RESAMPLE_HALF_TAPS,
            history: Vec::new(),
            position: 0.0,
            seeded: false,
            taps: vec![0.0; (2 * RESAMPLE_HALF_TAPS + 1) as usize],
        }
    }

    /// Converts `input` (interleaved at the source rate) into output-rate samples.
    ///
    /// The tail lagging the last complete kernel stays buffered for the next call, so the stream
    /// can be fed in arbitrarily small chunks.
    fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let channels = self.channels;
        if !self.seeded {
            // Zero lead-in so the very first output sample has a full kernel to its left.
            self.history
                .extend(std::iter::repeat_n(0.0f32, self.half as usize * channels));
            self.position = self.half as f64;
            self.seeded = true;
        }
        self.history.extend_from_slice(input);

        let frames = self.history.len() / channels;
        let half = self.half as i64;
        loop {
            let index = self.position.floor() as i64;
            // Wait until the kernel's right-hand side is inside the buffered history.
            if index + half >= frames as i64 {
                break;
            }
            let fraction = (self.position - index as f64) as f64;
            let start = (index - half) as usize;
            // Kernel weights for offsets `-half..=half` around `index`, plus their sum for
            // normalisation.
            let mut sum = 0.0f32;
            for (tap, weight) in self.taps.iter_mut().enumerate() {
                let offset = tap as i64 - half;
                *weight = kernel(offset as f64 - fraction, self.cutoff, self.half);
                sum += *weight;
            }
            let gain = if sum.abs() > f32::EPSILON { 1.0 / sum } else { 1.0 };
            for channel in 0..channels {
                let mut accumulator = 0.0f32;
                let mut cursor = start * channels + channel;
                for weight in &self.taps {
                    accumulator += self.history[cursor] * *weight;
                    cursor += channels;
                }
                output.push(accumulator * gain);
            }
            self.position += self.step;
        }

        // Drop history the next output sample can no longer reach.
        let keep_from = (self.position.floor() as i64 - half).max(0) as usize;
        if keep_from > 0 {
            let drop = (keep_from * channels).min(self.history.len());
            self.history.copy_within(drop.., 0);
            self.history.truncate(self.history.len() - drop);
            self.position -= keep_from as f64;
        }
    }
}

/// The Hann-windowed sinc kernel: `cutoff * sinc(cutoff * t)` tapered to zero at `±half` frames.
///
/// `t` is measured in source frames from the interpolation point; `cutoff` is a fraction of the
/// source Nyquist.
fn kernel(t: f64, cutoff: f64, half: i32) -> f32 {
    let half_frames = half as f64;
    if t.abs() >= half_frames || half <= 0 {
        return 0.0;
    }
    let phase = std::f64::consts::PI * t;
    let sinc = if t == 0.0 { cutoff } else { (phase * cutoff).sin() / phase };
    let window = 0.5 * (1.0 + (std::f64::consts::PI * t / half_frames).cos());
    (sinc * window) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_with(stop: &Arc<AtomicBool>) -> Arc<Shared> {
        Arc::new(Shared::new(stop.clone(), Volume::new()))
    }

    #[test]
    fn pause_preserves_audio_and_position_until_resume() {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = shared_with(&stop);
        shared.set_target(2, 2);
        shared.samples.lock().unwrap().extend([0.1, 0.2, 0.3, 0.4]);
        let player = Player {
            stop,
            store: Arc::new(Store::new(Arc::new(AtomicBool::new(false)))),
            shared: shared.clone(),
            thread: None,
        };
        let mut output = [1.0f32; 4];
        player.set_paused(true);
        fill(&mut output, &shared);
        assert_eq!(output, [0.0; 4]);
        assert_eq!(player.position_seconds(), 0);
        player.set_paused(false);
        fill(&mut output, &shared);
        assert_eq!(output, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(player.position_seconds(), 1);
    }

    #[test]
    fn playback_clock_excludes_underrun_silence() {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = shared_with(&stop);
        shared.set_target(2, 2);
        shared.samples.lock().unwrap().extend([0.1, 0.2, 0.3, 0.4]);
        let player = Player {
            stop,
            store: Arc::new(Store::new(Arc::new(AtomicBool::new(false)))),
            shared: shared.clone(),
            thread: None,
        };
        let mut output = [0.0f32; 8];
        fill(&mut output, &shared);
        assert_eq!(player.position_seconds(), 1);
        fill(&mut output, &shared);
        assert_eq!(player.position_seconds(), 1);
    }

    #[test]
    fn position_clock_follows_seek_and_pause() {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = shared_with(&stop);
        shared.set_target(100, 2);
        let player = Player {
            stop,
            store: Arc::new(Store::new(Arc::new(AtomicBool::new(false)))),
            shared: shared.clone(),
            thread: None,
        };
        player.seek(42.5);
        // The clock jumps to the requested position right away.
        assert_eq!(player.position_seconds(), 42);
        // A pause freezes it.
        player.set_paused(true);
        assert_eq!(player.position_seconds(), 42);
    }

    #[test]
    fn volume_ramp_is_linear_and_never_overshoots() {
        let mut gain = 1.0f32;
        let mut steps = 0;
        while gain > 0.0 {
            gain = ramp_toward(gain, 0.0, 0.1);
            steps += 1;
        }
        assert_eq!(steps, 10);
        assert_eq!(ramp_toward(0.05, 0.0, 0.1), 0.0);
        assert_eq!(ramp_toward(0.95, 1.0, 0.1), 1.0);
        assert_eq!(ramp_toward(0.5, 0.5, 0.1), 0.5);
    }

    #[test]
    fn volume_scales_output() {
        let stop = Arc::new(AtomicBool::new(false));
        let volume = Volume::new();
        volume.set_volume(0.5);
        let shared = Arc::new(Shared::new(stop.clone(), volume.clone()));
        shared.set_target(8, 1);
        // The ramp starts at the configured gain, so there is no fade-in.
        shared.samples.lock().unwrap().extend([1.0, 1.0, 1.0, 1.0]);
        let mut output = [0.0f32; 4];
        fill(&mut output, &shared);
        assert_eq!(output, [0.5, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn mute_ramps_down_without_clicks() {
        let stop = Arc::new(AtomicBool::new(false));
        let volume = Volume::new();
        let shared = Arc::new(Shared::new(stop.clone(), volume.clone()));
        shared.set_target(48000, 1);
        let frames = 4800;
        shared
            .samples
            .lock()
            .unwrap()
            .extend(std::iter::repeat(1.0f32).take(frames));
        volume.set_muted(true);
        let mut output = vec![0.0f32; frames];
        fill(&mut output, &shared);
        // No instantaneous step: the first output sample is still near unity.
        assert!(output[0] > 0.99, "first sample {} clicked", output[0]);
        // Monotone fade-out, reaching silence.
        for window in output.windows(2) {
            assert!(window[1] <= window[0] + 1e-6);
        }
        assert_eq!(*output.last().unwrap(), 0.0);
        // Silence arrives after about 20 ms = 960 frames at 48 kHz.
        let silent_at = output.iter().position(|value| *value == 0.0).unwrap();
        assert!((silent_at as i64 - 960).abs() <= 2, "silent at {silent_at}");
    }

    #[test]
    fn muted_output_is_silent_but_keeps_gain() {
        let stop = Arc::new(AtomicBool::new(false));
        let volume = Volume::new();
        volume.set_volume(0.7);
        volume.set_muted(true);
        assert_eq!(volume.volume(), 0.7);
        assert_eq!(volume.gain(), 0.0);
        let shared = Arc::new(Shared::new(stop.clone(), volume.clone()));
        shared.set_target(48000, 1);
        shared
            .samples
            .lock()
            .unwrap()
            .extend(std::iter::repeat(1.0f32).take(960));
        let mut output = vec![1.0f32; 960];
        fill(&mut output, &shared);
        assert!(output.iter().all(|value| *value == 0.0));
        volume.set_muted(false);
        assert_eq!(volume.gain(), 0.7);
    }

    #[test]
    fn store_source_seeks_over_buffered_bytes() {
        let stop = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Store::new(stop));
        store.push(b"hello world");
        store.finish(None);
        let mut reader = StoreSource::new(store.clone(), true);
        assert!(reader.is_seekable());
        assert_eq!(reader.byte_len(), Some(11));

        let mut buffer = [0u8; 5];
        assert_eq!(reader.read(&mut buffer).unwrap(), 5);
        assert_eq!(&buffer, b"hello");

        reader.seek(SeekFrom::Start(6)).unwrap();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"world");

        // Backwards seek over retained bytes.
        reader.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(reader.read(&mut buffer).unwrap(), 5);
        assert_eq!(&buffer, b"hello");

        // Applying the store's end is exact for a complete stream.
        assert_eq!(reader.seek(SeekFrom::End(-5)).unwrap(), 6);
    }

    #[test]
    fn non_seekable_source_rejects_seeks_and_hides_length() {
        let stop = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Store::new(stop));
        store.push(b"abcd");
        store.finish(None);
        let mut reader = StoreSource::new(store, false);
        assert!(!reader.is_seekable());
        assert_eq!(reader.byte_len(), None);
        assert!(reader.seek(SeekFrom::Start(1)).is_err());
    }

    #[test]
    fn store_source_blocks_for_missing_bytes_until_eof() {
        let stop = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Store::new(stop));
        store.push(b"ab");

        let reader_store = store.clone();
        let reader = std::thread::spawn(move || {
            let mut reader = StoreSource::new(reader_store, true);
            let mut all = Vec::new();
            reader.read_to_end(&mut all).unwrap();
            all
        });

        std::thread::sleep(Duration::from_millis(50));
        store.push(b"cd");
        store.finish(None);
        assert_eq!(reader.join().unwrap(), b"abcd");
    }

    #[test]
    fn store_wait_complete_reports_errors() {
        let stop = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Store::new(stop));
        store.push(b"x");
        store.finish(Some("boom".to_string()));
        assert!(!store.wait_complete());
        assert_eq!(store.error().as_deref(), Some("boom"));
        assert!(!store.is_complete());
    }

    #[test]
    fn legacy_audio_decrypts_across_read_boundaries() {
        let ciphertext = [
            0xc6, 0x88, 0xb7, 0xb7, 0xb3, 0x7b, 0xad, 0xed, 0xbb, 0x2f,
            0x55, 0xea, 0x90, 0x99, 0x4b, 0x1c, 0xf1, 0x40, 0xa6, 0xd8,
            0x9d, 0x18, 0x61, 0xd0, 0xcd, 0x8d, 0x31, 0xbe, 0x04, 0x70,
            0x2b, 0x46, 0x71, 0x55, 0xff, 0xd7, 0xac, 0x62, 0xb8, 0x9c, 0xd2,
        ];
        let mut reader = DecryptReader::new(
            &ciphertext[..],
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            [16, 17, 18, 19, 20, 21, 22, 23],
        );
        let mut plaintext = Vec::new();
        let mut chunk = [0; 7];
        loop {
            let count = reader.read(&mut chunk).unwrap();
            if count == 0 { break; }
            plaintext.extend_from_slice(&chunk[..count]);
        }
        assert_eq!(plaintext, b"Known audio plaintext for streaming test!");
    }

    // -----------------------------------------------------------------------------------------
    // Resampling

    #[test]
    fn equal_rates_do_not_resample_at_all() {
        let mut converter = Converter::new(44100, 2, 44100, 2);
        assert!(converter.resampler.is_none());
        let input: Vec<f32> = (0..64).map(|index| index as f32 / 64.0).collect();
        let mut output = Vec::new();
        converter.push(&input, &mut output);
        // Bit-identical: the same samples, in the same order.
        assert_eq!(output, input);
    }

    #[test]
    fn upsampling_produces_about_ratio_times_as_many_frames() {
        let mut resampler = Resampler::new(44100, 88200, 1);
        let input: Vec<f32> = (0..4410).map(|index| index as f32 / 4410.0).collect();
        let mut output = Vec::new();
        resampler.push(&input, &mut output);
        // The kernel lags by half a window plus whatever the loop has not completed.
        let expected = 2.0 * (4410.0 - RESAMPLE_HALF_TAPS as f64);
        assert!(
            (output.len() as f64 - expected).abs() <= 2.0 * RESAMPLE_HALF_TAPS as f64,
            "unexpected output length {}",
            output.len()
        );
    }

    #[test]
    fn a_constant_stays_a_constant_through_resampling() {
        for (source, target) in [(44100, 48000), (48000, 44100), (96000, 48000), (44100, 192000)] {
            let mut resampler = Resampler::new(source, target, 1);
            let input = vec![0.25f32; 2000];
            let mut output = Vec::new();
            resampler.push(&input, &mut output);
            // Skip the samples whose kernel still reaches into the zero lead-in: that many output
            // frames covers `half` input frames at this ratio.
            let skip = (RESAMPLE_HALF_TAPS as f64 * target as f64 / source as f64).ceil() as usize + 8;
            for (index, value) in output.iter().enumerate().skip(skip) {
                assert!(
                    (value - 0.25).abs() < 1e-3,
                    "{source}->{target} sample {index} = {value}"
                );
            }
        }
    }

    #[test]
    fn downsampling_keeps_a_low_tone_at_its_amplitude() {
        let mut resampler = Resampler::new(96000, 48000, 1);
        // 1 kHz, well below the new 24 kHz Nyquist.
        let input: Vec<f32> = (0..9600)
            .map(|index| (2.0 * std::f32::consts::PI * 1000.0 * index as f32 / 96000.0).sin())
            .collect();
        let mut output = Vec::new();
        resampler.push(&input, &mut output);
        let settled = &output[RESAMPLE_HALF_TAPS as usize * 2..];
        let peak = settled.iter().fold(0.0f32, |peak, value| peak.max(value.abs()));
        assert!((peak - 1.0).abs() < 0.05, "peak {peak} after downsampling");
    }

    #[test]
    fn the_kernel_is_symmetric_and_vanishes_outside_the_window() {
        for t in [0.0f64, 0.25, 1.0, 3.5] {
            assert!((kernel(t, 0.5, 32) - kernel(-t, 0.5, 32)).abs() < 1e-9);
        }
        assert_eq!(kernel(32.0, 1.0, 32), 0.0);
        assert_eq!(kernel(-40.0, 1.0, 32), 0.0);
        assert!((kernel(0.0, 1.0, 32) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn streaming_a_signal_in_chunks_matches_one_go() {
        let signal: Vec<f32> = (0..600).map(|index| ((index as f32) * 0.1).sin()).collect();
        let mut whole = Resampler::new(44100, 48000, 1);
        let mut whole_out = Vec::new();
        whole.push(&signal, &mut whole_out);

        let mut chunked = Resampler::new(44100, 48000, 1);
        let mut chunked_out = Vec::new();
        for chunk in signal.chunks(37) {
            chunked.push(chunk, &mut chunked_out);
        }
        assert_eq!(whole_out.len(), chunked_out.len());
        for (a, b) in whole_out.iter().zip(&chunked_out) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn channel_mapping_duplicates_and_collapses() {
        let mut mono_to_stereo = Converter::new(44100, 1, 44100, 2);
        let mut output = Vec::new();
        mono_to_stereo.push(&[0.5, 0.25], &mut output);
        assert_eq!(output, [0.5, 0.5, 0.25, 0.25]);

        let mut stereo_to_mono = Converter::new(44100, 2, 44100, 1);
        output.clear();
        stereo_to_mono.push(&[0.5, 0.1], &mut output);
        assert_eq!(output, [0.3]);
    }

    // -----------------------------------------------------------------------------------------
    // Output configuration choice

    #[test]
    fn an_exact_advertised_rate_is_preferred() {
        let advertised = [(2, 44100, 192000)];
        assert_eq!(choose_config(&advertised, 96000, 2), Some((2, 96000)));
        assert_eq!(choose_config(&advertised, 44100, 2), Some((2, 44100)));
    }

    #[test]
    fn the_closest_advertised_rate_wins_when_the_exact_one_is_missing() {
        // A device that only does 48 kHz: a 44.1 kHz track lands there and is resampled.
        let advertised = [(2, 48000, 48000)];
        assert_eq!(choose_config(&advertised, 44100, 2), Some((2, 48000)));
        // Out of range on both sides: whichever bound is nearer.
        let advertised = [(2, 44100, 48000), (2, 88200, 96000)];
        assert_eq!(choose_config(&advertised, 50000, 2), Some((2, 48000)));
        assert_eq!(choose_config(&advertised, 96000, 2), Some((2, 96000)));
    }

    #[test]
    fn the_source_channel_count_is_preferred_over_the_rate() {
        // Both rates are equally far from 44.1 kHz (44.1 is in range of neither as an exact
        // match); the stereo layout wins even though it forces a resample.
        let advertised = [(1, 44100, 44100), (2, 48000, 48000)];
        assert_eq!(choose_config(&advertised, 44100, 2), Some((2, 48000)));
        // With no stereo configuration at all, mono is chosen.
        let advertised = [(1, 48000, 48000)];
        assert_eq!(choose_config(&advertised, 44100, 2), Some((1, 48000)));
    }

    #[test]
    fn no_advertised_configuration_yields_nothing() {
        assert_eq!(choose_config(&[], 44100, 2), None);
    }

    #[test]
    fn output_rate_labels_read_like_a_dac_display() {
        assert_eq!(format_khz(44100), "44.1");
        assert_eq!(format_khz(48000), "48");
        assert_eq!(format_khz(88200), "88.2");
        assert_eq!(format_khz(96000), "96");
        assert_eq!(format_khz(176400), "176.4");
        assert_eq!(format_khz(192000), "192");
        let info = OutputInfo {
            device: "Built-in Audio".to_owned(),
            rate: 96000,
            channels: 2,
            sample_format: "f32".to_owned(),
        };
        assert_eq!(info.label(), "96 kHz · f32 · Built-in Audio");
    }

    // -----------------------------------------------------------------------------------------
    // Preload hand-off

    /// A test payload that records whether it was abandoned.
    struct Tracked(Arc<AtomicUsize>, u64);

    impl Abandon for Tracked {
        fn abandon(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_preload_lands_once_and_is_consumed_by_the_hand_off() {
        let slot = PreloadSlot::<Tracked>::new();
        let abandoned = Arc::new(AtomicUsize::new(0));
        let generation = slot.next_generation();
        assert!(slot.put(generation, Tracked(abandoned.clone(), 7)));
        assert_eq!(slot.take().unwrap().1, 7);
        assert!(slot.take().is_none(), "the hand-off consumes it");
        assert_eq!(abandoned.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_superseded_preload_is_abandoned_and_never_lands() {
        let slot = PreloadSlot::<Tracked>::new();
        let abandoned = Arc::new(AtomicUsize::new(0));
        let stale = slot.next_generation();
        // The queue changed while the preload was resolving.
        slot.cancel();
        assert!(!slot.put(stale, Tracked(abandoned.clone(), 1)));
        assert!(slot.take().is_none());
        assert_eq!(abandoned.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_newer_preload_abandons_the_one_it_displaces() {
        let slot = PreloadSlot::<Tracked>::new();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let generation = slot.next_generation();
        assert!(slot.put(generation, Tracked(first.clone(), 1)));
        let generation = slot.next_generation();
        assert!(slot.put(generation, Tracked(second.clone(), 2)));
        assert_eq!(slot.take().unwrap().1, 2);
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancelling_abandons_a_stored_preload() {
        let slot = PreloadSlot::<Tracked>::new();
        let abandoned = Arc::new(AtomicUsize::new(0));
        let generation = slot.next_generation();
        assert!(slot.put(generation, Tracked(abandoned.clone(), 3)));
        slot.cancel();
        assert!(slot.take().is_none());
        assert_eq!(abandoned.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_switch_is_reported_once() {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::new(stop, Volume::new()));
        assert_eq!(shared.take_switch(), None);
        shared.record_switch(42);
        assert_eq!(shared.take_switch(), Some(42));
        assert_eq!(shared.take_switch(), None);
    }

    #[test]
    fn the_current_store_follows_a_gapless_hand_off() {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::new(stop.clone(), Volume::new()));
        assert!(shared.current_store().is_none());
        let first = Arc::new(Store::new(stop.clone()));
        shared.set_store(first.clone());
        assert_eq!(shared.current_store().unwrap().len(), 0);
        let second = Arc::new(Store::new(stop));
        second.push(b"next track");
        shared.set_store(second.clone());
        assert_eq!(shared.current_store().unwrap().len(), 10);
    }

    #[test]
    fn an_aborted_store_refuses_more_bytes() {
        // A cancelled preload's store is aborted, which is what makes its fetcher thread stop
        // instead of downloading a track nobody will play.
        let store = Store::new(Arc::new(AtomicBool::new(false)));
        assert!(store.push(b"chunk"));
        store.abort();
        assert!(!store.push(b"more"));
        assert!(!store.is_complete());
    }

    #[test]
    fn unity_gain_is_the_bit_perfect_pass_through() {
        let stop = Arc::new(AtomicBool::new(false));
        let volume = Volume::new();
        assert_eq!(volume.gain(), 1.0);
        let shared = Arc::new(Shared::new(stop.clone(), volume.clone()));
        shared.set_target(48000, 1);
        let samples: Vec<f32> = (0..64).map(|index| index as f32 / 1000.0).collect();
        shared.samples.lock().unwrap().extend(samples.iter().copied());
        let mut output = vec![0.0f32; samples.len()];
        fill(&mut output, &shared);
        assert_eq!(output, samples);

        // Any gain other than unity (here: normalisation) leaves the pass-through.
        volume.set_normalization_gain(0.5);
        let mut output = vec![0.0f32; samples.len()];
        fill(&mut output, &shared);
        assert!(output.iter().all(|value| *value == 0.0), "queue was drained above");
    }

    // -----------------------------------------------------------------------------------------
    // Device smoke tests
    //
    // Ignored by default: they need an audio output device and `ffmpeg`. To keep
    // audio off the speakers, route the default output to a null sink first —
    // e.g. `pactl load-module module-null-sink sink_name=tsun_smoke` then
    // `pactl set-default-sink tsun_smoke` (restore afterwards) — and run
    // `cargo test -- --ignored`. They prove start, seek (forward *and*
    // backward), volume and the natural end on real generated sources served
    // over a tiny in-process HTTP server: a plain FLAC and a fragmented MP4
    // carrying AAC (the shape TIDAL's DASH→HLS conversion produces).

    #[test]
    #[ignore = "needs an audio output device and ffmpeg"]
    fn local_flac_seek_and_volume_through_output_device() {
        let dir = TempDir::new();
        let path = dir.path.join("tone.flac");
        synthesize(
            &[
                "-f", "lavfi", "-i", "sine=frequency=440:duration=6", "-ar", "44100", "-ac", "2",
                "-f", "flac",
            ],
            &path,
        );
        smoke_seek_and_volume(&path, "tone.flac");
    }

    #[test]
    #[ignore = "needs an audio output device and ffmpeg"]
    fn local_mp4_aac_seek_and_volume_through_output_device() {
        let dir = TempDir::new();
        let path = dir.path.join("tone.m4a");
        synthesize(
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6",
                "-ar",
                "44100",
                "-ac",
                "2",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-movflags",
                "+frag_keyframe+empty_moov+default_base_moof",
                "-f",
                "mp4",
            ],
            &path,
        );
        smoke_seek_and_volume(&path, "tone.m4a");
    }

    /// Runs ffmpeg with `arguments`, then `-y <path>`.
    fn synthesize(arguments: &[&str], path: &std::path::Path) {
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error"])
            .args(arguments)
            .arg("-y")
            .arg(path)
            .status()
            .expect("ffmpeg must be installed");
        assert!(status.success(), "ffmpeg failed to synthesize {path:?}");
    }

    /// Plays `path` over the default output device, exercising the position
    /// clock across a forward and a backward seek, a volume change and a mute,
    /// and the natural end.
    fn smoke_seek_and_volume(path: &std::path::Path, file_name: &str) {
        let bytes = std::fs::read(path).unwrap();
        let server = serve_bytes(bytes);
        let url = format!("http://127.0.0.1:{}/{file_name}", server.port);

        let volume = Volume::new();
        volume.set_volume(0.25);
        let mut player = Player::start(Source::Url(url), volume.clone(), OutputOptions::default())
            .expect("start playback");

        std::thread::sleep(Duration::from_millis(700));
        assert!(player.position_seconds() < 6);

        // Forward seek: handled by the demuxer's in-place resync.
        player.seek(4.0);
        std::thread::sleep(Duration::from_millis(900));
        let after_seek = player.position_seconds();
        assert!(after_seek >= 4, "forward seek did not advance the clock: {after_seek}");

        // Backward seek: needs the buffered track (re-opened for random access).
        player.seek(1.0);
        std::thread::sleep(Duration::from_millis(900));
        let after_back = player.position_seconds();
        assert!(
            (1..4).contains(&after_back),
            "backward seek did not move the clock back: {after_back}"
        );

        volume.set_volume(0.9);
        volume.set_muted(true);
        std::thread::sleep(Duration::from_millis(100));
        volume.set_muted(false);

        // Drain to the natural end.
        let mut outcome = None;
        for _ in 0..200 {
            if let Some(result) = player.poll() {
                outcome = Some(result);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(outcome, Some(Ok(Outcome::Finished)));
        player.stop();
    }

    /// Plays `first` and hands over to `second` through a preload, proving the switch happens
    /// while the same output stream stays open.
    #[test]
    #[ignore = "needs an audio output device and ffmpeg"]
    fn gapless_hand_off_switches_tracks_without_reopening_the_output() {
        let dir = TempDir::new();
        let first = dir.path.join("first.flac");
        let second = dir.path.join("second.flac");
        synthesize(
            &[
                "-f", "lavfi", "-i", "sine=frequency=440:duration=8", "-ar", "44100", "-ac", "2",
                "-f", "flac",
            ],
            &first,
        );
        synthesize(
            &[
                "-f", "lavfi", "-i", "sine=frequency=660:duration=8", "-ar", "44100", "-ac", "2",
                "-f", "flac",
            ],
            &second,
        );
        let server = serve_paths(vec![
            ("/first.flac".to_owned(), std::fs::read(&first).unwrap()),
            ("/second.flac".to_owned(), std::fs::read(&second).unwrap()),
        ]);

        let volume = Volume::new();
        volume.set_volume(0.25);
        let mut player = Player::start(
            Source::Url(format!("http://127.0.0.1:{}/first.flac", server.port)),
            volume,
            OutputOptions::default(),
        )
        .expect("start playback");
        let output_before = player.output_info().expect("the output format");
        // A real 16/24-bit FLAC at its own rate: the device must be at 44.1 kHz too.
        assert_eq!(output_before.rate, 44100);

        const SECOND_ID: u64 = 4242;
        player
            .preload(
                SECOND_ID,
                Source::Url(format!("http://127.0.0.1:{}/second.flac", server.port)),
                1.0,
            )
            .expect("preload");

        // Jump near the end of the first track and wait for the hand-off.
        std::thread::sleep(Duration::from_millis(400));
        player.seek(6.0);
        let mut switched = None;
        for _ in 0..240 {
            if let Some(id) = player.take_gapless_switch() {
                switched = Some(id);
                break;
            }
            assert!(
                player.poll().is_none(),
                "playback ended instead of handing over: {:?}",
                player.poll()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(switched, Some(SECOND_ID), "the preloaded track never started");

        // The second track is playing from its beginning, on the very same stream.
        std::thread::sleep(Duration::from_millis(600));
        let position = player.position_seconds();
        assert!(position < 4, "the second track should have restarted: {position}");
        assert!(player.poll().is_none(), "the hand-off ended playback");
        assert_eq!(
            player.output_info(),
            Some(output_before),
            "the output was re-opened across the hand-off"
        );
        player.stop();
    }

    /// A generated local WAV plays through [`Source::File`] to its natural end.
    ///
    /// Requires `TSUN_AUDIO_TEST=1` and `ALSA_CONFIG_PATH` pointing at a null-device config
    /// (see the repo README's testing notes): it opens the real output device, so it is opt-in
    /// to keep `cargo test` silent. Volume is forced to zero as a second guard.
    #[test]
    fn local_file_plays_to_its_natural_end() {
        if std::env::var_os("TSUN_AUDIO_TEST").is_none() {
            eprintln!("skipping audio playback test (set TSUN_AUDIO_TEST=1 with a null ALSA device)");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "tsun-audio-local-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("local.wav");
        write_test_wav(&path, 2);
        let volume = Volume::new();
        volume.set_volume(0.0);
        let mut player = Player::start(
            Source::File(path),
            volume,
            OutputOptions::default(),
        )
        .expect("start local playback");
        assert!(player.output_info().is_some(), "the device opened");
        let mut advanced = false;
        let mut outcome = None;
        for _ in 0..300 {
            std::thread::sleep(Duration::from_millis(50));
            if player.position_seconds() > 0 {
                advanced = true;
            }
            if let Some(result) = player.poll() {
                outcome = Some(result);
                break;
            }
        }
        assert!(advanced, "the position clock advanced");
        assert_eq!(outcome, Some(Ok(Outcome::Finished)), "the file reached its natural end");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A minimal 44.1 kHz 16-bit mono PCM WAV with `seconds` of silence.
    fn write_test_wav(path: &std::path::Path, seconds: u32) {
        let rate: u32 = 44_100;
        let data_len = rate * seconds * 2;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, data_len as usize));
        std::fs::write(path, bytes).unwrap();
    }

    /// Removes its directory even if the test panics.
    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("tsun-audio-smoke-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&path);
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct TinyServer {
        port: u16,
    }

    /// Serves exactly one GET per connection from a fixed byte buffer.
    fn serve_bytes(bytes: Vec<u8>) -> TinyServer {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&bytes);
                let _ = stream.flush();
            }
        });
        TinyServer { port }
    }

    /// Serves each `(path, bytes)` pair, matching on the request's first line. Used by the
    /// gapless smoke test, which needs two tracks at once.
    fn serve_paths(resources: Vec<(String, Vec<u8>)>) -> TinyServer {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0u8; 4096];
                let count = stream.read(&mut request).unwrap_or(0);
                let line = String::from_utf8_lossy(&request[..count]);
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let body = resources
                    .iter()
                    .find(|(name, _)| *name == path)
                    .map(|(_, bytes)| bytes.clone());
                let (status, body) = match body {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", Vec::new()),
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: audio/flac\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        TinyServer { port }
    }
}
