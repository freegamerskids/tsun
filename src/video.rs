//! Music video playback: TIDAL HLS → FFmpeg decoders → Rune pixels + cpal audio.
//!
//! TIDAL's video playback endpoint hands back an HLS master playlist whose
//! variant streams are plain MPEG-TS (H.264 High + AAC LC, no `EXT-X-KEY`).
//! [`Engine::start`] picks the best variant up to 1080p, opens it with
//! FFmpeg's HLS demuxer, and runs one media thread that:
//!
//! * feeds video packets to the FFmpeg H.264 decoder and converts each decoded
//!   frame to straight-alpha RGBA,
//! * feeds audio packets to the FFmpeg AAC decoder and resamples them to the
//!   output device's rate and channel layout,
//! * writes both into their own pools so a seek only drops buffered work.
//!
//! The UI reads the current position from atomics and receives frames through
//! [`rune::Pixels::set`], the same streaming pixel surface the rest of Rune
//! uses; the decode thread never runs in a view.
//!
//! Audio goes to cpal directly so volume and mute are the shared
//! [`Volume`](crate::audio::Volume) handle the music player uses: changing the
//! volume or pressing mute affects the video exactly as it affects a track.
//! The output clock (`position`) drives A/V sync: a video frame is shown once
//! the audio device has delivered the samples up to its presentation time, and
//! video-only playback falls back to a wall clock. Pause stops consuming audio
//! and freezes the clock; seek flushes both pipelines and repositions the HLS
//! demuxer.
//!
//! # Why FFmpeg and not a pure-Rust decoder
//!
//! Cisco's openh264 decodes this catalogue's streams for a few frames and then
//! fails (`dsNoParamSets`): TIDAL encodes High profile with B-frames, which
//! openh264 does not support. FFmpeg's libavcodec handles the real bitstreams,
//! including their reordering, and its HLS demuxer does the segment fetching and
//! look-ahead. The build therefore needs the FFmpeg 7/8 development libraries
//! (`libavcodec`, `libavformat`, `libavutil`, `libswresample`, `libswscale`),
//! the same runtime the system ffmpeg package ships.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use ffmpeg_next as ffmpeg;
use ffmpeg::format::Pixel;
use ffmpeg::media::Type;
use ffmpeg::software::scaling::context::Context as Scaler;
use ffmpeg::software::scaling::flag::Flags;
use ffmpeg::ChannelLayout;
use rune::Pixels;

use crate::audio::Volume;

const USER_AGENT: &str = "tsun/0.1 (TIDAL desktop client)";
/// The audio pipeline targets this rate; the output device follows when it can.
const SOURCE_RATE: u32 = 48_000;
/// How many decoded video frames to keep ahead of the audio clock.
const VIDEO_AHEAD: usize = 14;
/// How much audio to keep ahead of the output callback, in seconds.
const AUDIO_AHEAD: f64 = 1.0;
/// A pause/stop check granularity for the media thread.
const TICK: Duration = Duration::from_millis(5);
/// Coarse sleep once the prefetch pools are full.
const IDLE: Duration = Duration::from_millis(4);
/// Volume ramp length, matching the music player so changes do not click.
const RAMP_SECONDS: f32 = 0.02;
/// The largest variant we open. TIDAL serves up to 1080p; anything larger
/// would cost memory without changing what the window can show.
const MAX_HEIGHT: u32 = 1080;

/// What the media thread is doing, for the player view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Opening the playlist / first packets.
    Loading,
    Playing,
    Paused,
    /// The stream ended (or the user freed it): the last frame stays up.
    Ended,
    Failed(String),
}

/// A running video, owned by the UI (it is `Send` but not `Sync`).
pub(crate) struct Engine {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    /// Starts decoding `master_url` on a background thread.
    ///
    /// `duration_ms` is TIDAL's catalogue duration; it fills the seek bar until
    /// the container reports its own. `start_secs` opens the stream at that
    /// position. The call returns immediately, with [`Status::Loading`].
    pub(crate) fn start(
        master_url: String,
        volume: Volume,
        device: Option<String>,
        duration_ms: u64,
        start_secs: f64,
    ) -> Engine {
        let pixels = Pixels::new();
        let audio = Arc::new(AudioShared::new(volume));
        let shared = Arc::new(Shared {
            pixels: pixels.clone(),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(duration_ms),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            started: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            seek: Mutex::new(None),
            device: Mutex::new(None),
            status: Mutex::new(Status::Loading),
            audio,
            queue: Mutex::new(VecDeque::new()),
            wall: Mutex::new((0.0, Instant::now())),
            immediate: AtomicBool::new(true),
            epoch: AtomicU64::new(0),
            presenter_done: AtomicBool::new(false),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let spawn_shared = shared.clone();
        let spawned = thread::Builder::new()
            .name("tsun-video".to_string())
            .spawn(move || {
                let start = if start_secs.is_finite() { start_secs.max(0.0) } else { 0.0 };
                if let Err(error) = run(master_url, thread_shared.clone(), thread_stop.clone(), device, start)
                {
                    if !thread_stop.load(Ordering::Relaxed) {
                        thread_shared.fail(error);
                    }
                }
            });
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(error) => {
                spawn_shared.fail(format!("could not start the video thread: {error}"));
                None
            }
        };
        Engine { shared, stop, thread }
    }

    /// The streaming pixel surface the view draws. Clone it into the element.
    pub(crate) fn pixels(&self) -> &Pixels {
        &self.shared.pixels
    }

    /// Presentation time in seconds, driven by the audio device's clock.
    pub(crate) fn position_secs(&self) -> f64 {
        self.shared.position_ms.load(Ordering::Relaxed) as f64 / 1000.0
    }

    pub(crate) fn duration_secs(&self) -> u64 {
        self.shared.duration_ms.load(Ordering::Relaxed) / 1000
    }

    /// How many pictures have been shown so far.
    pub(crate) fn frames_shown(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    /// The decoded surface size, once a frame has arrived.
    pub(crate) fn resolution(&self) -> (u32, u32) {
        (
            self.shared.width.load(Ordering::Relaxed),
            self.shared.height.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn status(&self) -> Status {
        let status = self.shared.status();
        match status {
            Status::Loading | Status::Playing | Status::Paused => {
                if self.shared.paused.load(Ordering::Relaxed) {
                    Status::Paused
                } else if self.shared.started.load(Ordering::Relaxed) {
                    Status::Playing
                } else {
                    Status::Loading
                }
            }
            other => other,
        }
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Freezes/resumes decoding, audio consumption and the clock.
    pub(crate) fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
        self.shared.audio.paused.store(paused, Ordering::SeqCst);
    }

    /// Seeks to `seconds` from the video's start. Cheap; the media thread does
    /// the demuxer seek and flushes both pipelines.
    pub(crate) fn seek(&self, seconds: f64) {
        let seconds = if seconds.is_finite() { seconds.max(0.0) } else { 0.0 };
        if let Ok(mut slot) = self.shared.seek.lock() {
            *slot = Some(seconds);
        }
        self.shared.position_ms.store((seconds * 1000.0) as u64, Ordering::Relaxed);
    }

    /// Re-opens the audio output on `device` at the position that is playing,
    /// as the music player does. A device that cannot be opened surfaces as a
    /// playback error through [`Engine::status`].
    pub(crate) fn set_output_device(&self, device: Option<String>) {
        if let Ok(mut slot) = self.shared.device.lock() {
            *slot = Some(device);
        }
    }

    /// Stops the media thread and waits for it. Idempotent.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Shared state

struct Shared {
    pixels: Pixels,
    position_ms: AtomicU64,
    duration_ms: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    /// Whether a picture or audio sample has actually been produced yet.
    started: AtomicBool,
    /// Pictures handed to the pixel surface so far; the UI polls it to know a new one is up.
    frames: AtomicU64,
    paused: AtomicBool,
    seek: Mutex<Option<f64>>,
    /// An output-device change from Settings, applied at the current position.
    device: Mutex<Option<Option<String>>>,
    status: Mutex<Status>,
    audio: Arc<AudioShared>,
    /// Decoded pictures waiting for the presenter thread, oldest first.
    queue: Mutex<VecDeque<Decoded>>,
    /// The wall-clock fallback: content seconds at the instant it was set.
    wall: Mutex<(f64, Instant)>,
    /// The next picture is shown at once (the first after opening or a seek).
    immediate: AtomicBool,
    /// Bumped when a seek or device change moves the clock, so the presenter snaps to it.
    epoch: AtomicU64,
    /// Set when the media thread is done, to stop the presenter.
    presenter_done: AtomicBool,
}

impl Shared {
    fn status(&self) -> Status {
        self.status.lock().map(|status| status.clone()).unwrap_or_else(|_| {
            Status::Failed("the video playback state was poisoned".to_string())
        })
    }

    fn set_status(&self, status: Status) {
        if let Ok(mut slot) = self.status.lock() {
            *slot = status;
        }
    }

    fn fail(&self, error: String) {
        self.set_status(Status::Failed(error));
    }
}

/// The PCM path between the media thread and the cpal callback.
struct AudioShared {
    /// Interleaved `dev_channels`-sample frames, ready for the callback.
    ring: Mutex<VecDeque<f32>>,
    /// Whether audio is being produced at all; when false the clock is wall-driven.
    started: AtomicBool,
    /// Samples actually delivered to the device (interleaved count).
    played: AtomicU64,
    /// Content seconds corresponding to `played == 0`.
    base_ms: AtomicU64,
    rate: AtomicU32,
    channels: AtomicU32,
    paused: AtomicBool,
    /// The callback's current ramp gain, so mute/volume changes glide.
    gain_bits: AtomicU32,
    volume: Volume,
}

impl AudioShared {
    fn new(volume: Volume) -> AudioShared {
        AudioShared {
            ring: Mutex::new(VecDeque::new()),
            started: AtomicBool::new(false),
            played: AtomicU64::new(0),
            base_ms: AtomicU64::new(0),
            rate: AtomicU32::new(SOURCE_RATE),
            channels: AtomicU32::new(2),
            paused: AtomicBool::new(false),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            volume,
        }
    }

    fn channels(&self) -> usize {
        self.channels.load(Ordering::Relaxed).max(1) as usize
    }

    fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed).max(1)
    }

    fn capacity(&self) -> usize {
        let seconds = (AUDIO_AHEAD + 0.5) as usize;
        self.rate() as usize * self.channels() * seconds
    }

    /// The playback clock in seconds, or `None` until the first sample played.
    fn clock(&self) -> Option<f64> {
        if !self.started.load(Ordering::Relaxed) {
            return None;
        }
        let played = self.played.load(Ordering::Relaxed) as f64;
        let frames = played / self.channels() as f64;
        Some(self.base_ms.load(Ordering::Relaxed) as f64 / 1000.0 + frames / self.rate() as f64)
    }

    fn buffered_secs(&self) -> f64 {
        let samples = self.ring.lock().map(|ring| ring.len()).unwrap_or(0);
        samples as f64 / (self.channels() as f64 * self.rate() as f64)
    }

    fn clear(&self) {
        if let Ok(mut ring) = self.ring.lock() {
            ring.clear();
        }
        self.played.store(0, Ordering::Relaxed);
        self.started.store(false, Ordering::Relaxed);
    }

    fn set_base(&self, seconds: f64) {
        let ms = (seconds.max(0.0) * 1000.0) as u64;
        self.base_ms.store(ms, Ordering::Relaxed);
        self.played.store(0, Ordering::Relaxed);
    }

    /// Blocks while the ring is full (bounded by `stop` and pause), then appends `samples`.
    fn push(&self, samples: &[f32], stop: &AtomicBool) {
        let mut offset = 0;
        while offset < samples.len() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if self.paused.load(Ordering::SeqCst) {
                thread::sleep(TICK);
                continue;
            }
            if let Ok(mut ring) = self.ring.lock() {
                let room = self.capacity().saturating_sub(ring.len());
                if room > 0 {
                    let end = (offset + room).min(samples.len());
                    ring.extend(samples[offset..end].iter().copied());
                    offset = end;
                }
            }
            if offset < samples.len() {
                thread::sleep(TICK);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Presenter thread

/// Shows each decoded picture when the playback clock reaches it.
///
/// This runs apart from the media thread, which spends its time in network reads and decoding
/// (a segment fetch can block it for a few hundred milliseconds); a picture that is due is
/// shown on time regardless. Dropping it stops and joins the thread.
struct Presenter {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Presenter {
    fn spawn(shared: Arc<Shared>, stop: Arc<AtomicBool>, has_audio: bool) -> Presenter {
        let thread_shared = shared.clone();
        let handle = thread::Builder::new()
            .name("tsun-video-present".to_string())
            .spawn(move || present_loop(&thread_shared, &stop, has_audio))
            .ok();
        Presenter { shared, handle }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.shared.presenter_done.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn present_loop(shared: &Shared, stop: &AtomicBool, has_audio: bool) {
    let mut smoother = ClockSmoother::new();
    let mut epoch = shared.epoch.load(Ordering::SeqCst);
    let mut paused_since: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) && !shared.presenter_done.load(Ordering::SeqCst) {
        let current = shared.epoch.load(Ordering::SeqCst);
        if current != epoch {
            // A seek or device change moved the clock: snap to it.
            epoch = current;
            smoother.reset();
        }

        if shared.paused.load(Ordering::SeqCst) {
            // The audio clock already freezes (the callback consumes nothing); keep the
            // wall-clock fallback from counting the pause too. A seek made while paused
            // still shows its first picture, so the scrub is visible.
            smoother.reset();
            if paused_since.is_none() {
                paused_since = Some(Instant::now());
            }
            if shared.immediate.load(Ordering::SeqCst) {
                show_due(shared, f64::INFINITY);
            }
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        if let Some(since) = paused_since.take() {
            if let Ok(mut wall) = shared.wall.lock() {
                wall.1 += since.elapsed();
            }
        }

        let wall_clock = shared
            .wall
            .lock()
            .map(|wall| wall.0 + wall.1.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let clock = if has_audio && shared.audio.started.load(Ordering::Relaxed) {
            match shared.audio.clock() {
                Some(device) => smoother.tick(device, Instant::now()),
                None => wall_clock,
            }
        } else {
            smoother.reset();
            wall_clock
        };
        show_due(shared, clock);
        shared.position_ms.store((clock.max(0.0) * 1000.0) as u64, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(2));
    }
}

/// Pops every queued picture whose time has come (or the next one, right after a seek) and
/// puts the newest of them on the surface.
fn show_due(shared: &Shared, clock: f64) {
    let mut newest: Option<Decoded> = None;
    if let Ok(mut queue) = shared.queue.lock() {
        while let Some(front) = queue.front() {
            if front.pts <= clock + 0.010 || shared.immediate.load(Ordering::SeqCst) {
                newest = queue.pop_front();
                shared.immediate.store(false, Ordering::SeqCst);
            } else {
                break;
            }
        }
    }
    if let Some(frame) = newest {
        shared.width.store(frame.width, Ordering::Relaxed);
        shared.height.store(frame.height, Ordering::Relaxed);
        shared.pixels.set(frame.width, frame.height, frame.rgba);
        shared.frames.fetch_add(1, Ordering::Relaxed);
        shared.started.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------------------------
// Media thread

/// Fetches and decodes one video. `Ok(())` means it stopped because of the
/// stop flag; errors are reported through the shared status.
fn run(
    master_url: String,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    device: Option<String>,
    start_secs: f64,
) -> Result<(), String> {
    ffmpeg::init().map_err(|error| format!("could not initialise FFmpeg: {error}"))?;
    // Decode warnings from the odd corrupt segment would otherwise print over
    // the app's stderr; real failures still reach the player as errors.
    ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
    let master = fetch_text(&master_url)?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let variant = select_variant(&master)?;

    let mut input = ffmpeg::format::input_with_interrupt(std::path::Path::new(&variant.url), {
        let stop = stop.clone();
        move || stop.load(Ordering::Relaxed)
    })
    .map_err(|error| format!("TIDAL's video stream could not be opened: {error}"))?;

    let (video_index, video_params, video_time_base) = {
        let stream = input
            .streams()
            .best(Type::Video)
            .ok_or_else(|| "TIDAL's video stream has no video track".to_string())?;
        (stream.index(), stream.parameters(), stream.time_base())
    };
    let audio_track = input.streams().best(Type::Audio).map(|stream| {
        (stream.index(), stream.parameters(), stream.time_base())
    });

    let origin = stream_origin(&input, video_index, audio_track.as_ref().map(|track| track.0));
    let content_end = {
        let duration = input.duration();
        if duration > 0 { duration as f64 / 1_000_000.0 } else { f64::INFINITY }
    };
    if let Some(duration) = stream_duration(&input) {
        if duration > 0 {
            shared.duration_ms.store(duration, Ordering::Relaxed);
        }
    }

    let mut video = ffmpeg::codec::context::Context::from_parameters(video_params)
        .map_err(|error| format!("the video decoder could not be created: {error}"))?
        .decoder()
        .video()
        .map_err(|error| format!("the video decoder could not be opened: {error}"))?;

    let mut audio_track_state = match audio_track {
        Some((index, params, time_base)) => Some(AudioTrack::new(index, params, time_base)?),
        None => None,
    };

    // The output device is opened before decoding starts so its rate and
    // layout decide the resampler's target. A device that cannot be opened is a
    // real failure, as it is for music.
    let output = open_output(device.as_deref(), &shared.audio)?;
    let mut out_channels = output.channels;
    let mut out_rate = output.rate;
    shared.audio.rate.store(out_rate, Ordering::Relaxed);
    shared.audio.channels.store(out_channels as u32, Ordering::Relaxed);
    let mut stream = Some(output);
    if let Some(output) = &stream {
        output.play()?;
    }

    // Open at the requested position. TIDAL's media timeline starts near 0;
    // `origin` makes a seek target relative to the video's content.
    if start_secs > 0.01 {
        seek_to(&mut input, origin + start_secs)?;
        shared.audio.set_base(start_secs);
    } else {
        shared.audio.set_base(0.0);
    }

    let mut target_rate = out_rate;
    let has_audio = audio_track_state.is_some();
    let mut scaler: Option<Scaler> = None;
    let mut frames: VecDeque<Decoded> = VecDeque::new();
    let mut eof = false;
    let mut skip_audio_until = start_secs;
    let mut skip_video_until = start_secs;
    let mut shown_error = false;
    if let Ok(mut wall) = shared.wall.lock() {
        *wall = (start_secs, Instant::now());
    }
    let _presenter = Presenter::spawn(shared.clone(), stop.clone(), has_audio);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // A seek requested by the UI is applied between packets so the demuxer
        // is never touched while a packet is outstanding.
        let requested = shared.seek.lock().ok().and_then(|mut slot| slot.take());
        if let Some(target) = requested {
            // Never ask past the playlist's end: the HLS demuxer answers EPERM
            // for a timestamp after the last segment, and a drag to the far
            // right would otherwise look like a broken video.
            let target = target.max(0.0).min((content_end - 0.5).max(0.0));
            if let Err(error) = seek_to(&mut input, origin + target) {
                // A seek that cannot be honoured leaves playback where it was;
                // the next clock tick corrects the scrubbed position.
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = error;
            } else {
                video.flush();
                if let Some(audio) = &mut audio_track_state {
                    audio.decoder.flush();
                    audio.resampler = None;
                }
                frames.clear();
                shared.audio.clear();
                shared.audio.set_base(target);
                skip_audio_until = target;
                skip_video_until = target;
                if let Ok(mut queue) = shared.queue.lock() {
                    queue.clear();
                }
                if let Ok(mut wall) = shared.wall.lock() {
                    *wall = (target, Instant::now());
                }
                shared.immediate.store(true, Ordering::SeqCst);
                shared.epoch.fetch_add(1, Ordering::SeqCst);
                eof = false;
                shared.position_ms.store((target * 1000.0) as u64, Ordering::Relaxed);
            }
        }

        // Settings' output-device change: drop the old stream and reopen at the
        // position that is playing, like the music player does.
        let requested_device = shared.device.lock().ok().and_then(|mut slot| slot.take());
        if let Some(device) = requested_device {
            let target = shared
                .audio
                .clock()
                .unwrap_or(shared.position_ms.load(Ordering::Relaxed) as f64 / 1000.0);
            shared.audio.clear();
            stream = None;
            match open_output(device.as_deref(), &shared.audio) {
                Ok(output) => {
                    out_channels = output.channels;
                    out_rate = output.rate;
                    target_rate = out_rate;
                    shared.audio.rate.store(out_rate, Ordering::Relaxed);
                    shared.audio.channels.store(out_channels as u32, Ordering::Relaxed);
                    if let Err(error) = output.play() {
                        shared.fail(error);
                        break;
                    }
                    stream = Some(output);
                    if let Some(audio) = &mut audio_track_state {
                        audio.resampler = None;
                    }
                    shared.audio.set_base(target);
                    skip_audio_until = target;
                    if let Ok(mut wall) = shared.wall.lock() {
                        *wall = (target, Instant::now());
                    }
                    shared.epoch.fetch_add(1, Ordering::SeqCst);
                    shared.position_ms.store((target * 1000.0) as u64, Ordering::Relaxed);
                }
                Err(error) => {
                    shared.fail(error);
                    break;
                }
            }
        }

        if shared.paused.load(Ordering::SeqCst) {
            // The presenter thread holds the picture and the clocks; nothing is decoded meanwhile.
            thread::sleep(TICK);
            continue;
        }

        // Hand finished pictures to the presenter thread, which shows each when the clock
        // reaches it. Showing happens there so a slow network read or decode here (a segment
        // fetch can block for a few hundred milliseconds) never delays a picture.
        if !frames.is_empty() {
            if let Ok(mut queue) = shared.queue.lock() {
                queue.extend(frames.drain(..));
            }
        }

        if eof {
            let audio_busy = has_audio && shared.audio.buffered_secs() > 0.0;
            let queued = shared.queue.lock().map(|queue| queue.len()).unwrap_or(0);
            if queued == 0 && frames.is_empty() && !audio_busy && !stop.load(Ordering::Relaxed) {
                shared.audio.started.store(false, Ordering::Relaxed);
                shared.set_status(Status::Ended);
                break;
            }
            thread::sleep(IDLE);
            continue;
        }

        // Read ahead: keep the picture pool and the audio ring full enough that
        // a network hiccup does not stall the clock.
        let audio_full = !has_audio || shared.audio.buffered_secs() >= AUDIO_AHEAD;
        let mut did_work = false;
        // A few packets per turn, so a picture whose time has come is shown within a few
        // milliseconds instead of waiting for a long batch of decoding.
        let mut budget = 3;
        let pending = |staged: usize| shared.queue.lock().map(|queue| queue.len()).unwrap_or(0) + staged;
        while budget > 0 && (pending(frames.len()) < VIDEO_AHEAD || !audio_full) {
            budget -= 1;
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut input) {
                Ok(()) => {
                    did_work = true;
                    let index = packet.stream();
                    if index == video_index {
                        decode_video(&mut video, &packet, &mut scaler, &mut frames, video_time_base, origin, skip_video_until)?;
                    } else if let Some(audio) = &mut audio_track_state {
                        if index == audio.index {
                            audio.decode(
                                &packet,
                                &shared.audio,
                                out_channels,
                                target_rate,
                                &mut skip_audio_until,
                                origin,
                                &stop,
                            )?;
                        }
                    }
                }
                Err(ffmpeg::Error::Eof) => {
                    eof = true;
                    flush_decoders(&mut video, &mut audio_track_state, &mut scaler, &mut frames, video_time_base, origin, &shared.audio, out_channels, target_rate, &mut skip_audio_until, skip_video_until, &stop)?;
                    break;
                }
                Err(error) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if !shown_error {
                        shown_error = true;
                        shared.fail(format!("the TIDAL video stream failed: {error}"));
                    }
                    eof = true;
                    break;
                }
            }
            if shared.seek.lock().map(|slot| slot.is_some()).unwrap_or(false) {
                break;
            }
        }
        let queued = shared.queue.lock().map(|queue| queue.len()).unwrap_or(0);
        if queued + frames.len() >= VIDEO_AHEAD && audio_full && !did_work {
            thread::sleep(IDLE);
        }
    }

    if let Some(mut stream) = stream.take() {
        let _ = stream.stop();
    }
    Ok(())
}

/// The audio decoder and the resampler into the output format.
struct AudioTrack {
    index: usize,
    time_base: ffmpeg::Rational,
    decoder: ffmpeg::decoder::Audio,
    resampler: Option<ffmpeg::software::resampling::Context>,
}

impl AudioTrack {
    fn new(index: usize, params: ffmpeg::codec::Parameters, time_base: ffmpeg::Rational) -> Result<Self, String> {
        let decoder = ffmpeg::codec::context::Context::from_parameters(params)
            .map_err(|error| format!("the video's audio decoder could not be created: {error}"))?
            .decoder()
            .audio()
            .map_err(|error| format!("the video's audio decoder could not be opened: {error}"))?;
        Ok(AudioTrack { index, time_base, decoder, resampler: None })
    }

    /// Sends one packet and hands every decoded frame to the resampler.
    #[allow(clippy::too_many_arguments)]
    fn decode(
        &mut self,
        packet: &ffmpeg::Packet,
        audio: &AudioShared,
        out_channels: usize,
        out_rate: u32,
        skip_until: &mut f64,
        origin: f64,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|error| format!("the video's audio packet was rejected: {error}"))?;
        let mut frame = ffmpeg::frame::Audio::empty();
        while self.decoder.receive_frame(&mut frame).is_ok() {
            self.push_frame(&frame, audio, out_channels, out_rate, skip_until, origin, stop)?;
            frame = ffmpeg::frame::Audio::empty();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_frame(
        &mut self,
        frame: &ffmpeg::frame::Audio,
        audio: &AudioShared,
        out_channels: usize,
        out_rate: u32,
        skip_until: &mut f64,
        origin: f64,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let pts = frame
            .pts()
            .map(|pts| pts as f64 * f64::from(self.time_base) - origin)
            .unwrap_or_else(|| if skip_until.is_finite() { *skip_until } else { 0.0 });
        let duration = if frame.rate() > 0 {
            frame.samples() as f64 / frame.rate() as f64
        } else {
            0.0
        };
        if pts + duration <= *skip_until {
            return Ok(());
        }
        let source_rate = frame.rate().max(1);
        let layout = {
            let layout = frame.channel_layout();
            if layout.is_empty() { self.decoder.channel_layout() } else { layout }
        };
        let target_layout = if out_channels == 1 { ChannelLayout::MONO } else { ChannelLayout::STEREO };
        if self.resampler.is_none() {
            self.resampler = Some(
                ffmpeg::software::resampling::Context::get(
                    frame.format(),
                    layout,
                    source_rate,
                    ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                    target_layout,
                    out_rate,
                )
                .map_err(|error| format!("the video's audio could not be resampled: {error}"))?,
            );
        }
        let mut resampled = ffmpeg::frame::Audio::empty();
        let resampler = self.resampler.as_mut().expect("the resampler was just created");
        resampler
            .run(frame, &mut resampled)
            .map_err(|error| format!("the video's audio could not be resampled: {error}"))?;
        if resampled.samples() == 0 {
            return Ok(());
        }
        let frames = resampled.samples();
        let bytes = resampled.data(0);
        let needed = frames * 2 * std::mem::size_of::<f32>();
        if bytes.len() < needed {
            return Ok(());
        }
        // SAFETY: FFmpeg allocated this packed f32 plane with at least `needed`
        // bytes; the slice is read-only and lives for this call.
        let stereo = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const f32, frames * 2) };
        let mut samples = Vec::with_capacity(frames * out_channels);
        for pair in stereo.chunks_exact(2) {
            match out_channels {
                1 => samples.push((pair[0] + pair[1]) * 0.5),
                2 => {
                    samples.push(pair[0]);
                    samples.push(pair[1]);
                }
                _ => {
                    samples.push(pair[0]);
                    samples.push(pair[1]);
                    samples.extend(std::iter::repeat_n(0.0, out_channels - 2));
                }
            }
        }
        audio.push(&samples, stop);
        if !audio.started.swap(true, Ordering::Relaxed) {
            let start = (*skip_until).max(pts);
            audio.set_base(start);
        }
        *skip_until = f64::NEG_INFINITY;
        Ok(())
    }
}

/// Decodes one video packet into the frame pool, as RGBA.
fn decode_video(
    decoder: &mut ffmpeg::decoder::Video,
    packet: &ffmpeg::Packet,
    scaler: &mut Option<Scaler>,
    frames: &mut VecDeque<Decoded>,
    time_base: ffmpeg::Rational,
    origin: f64,
    skip_before: f64,
) -> Result<(), String> {
    decoder
        .send_packet(packet)
        .map_err(|error| format!("the video packet was rejected: {error}"))?;
    let mut frame = ffmpeg::frame::Video::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        push_video_frame(&frame, scaler, frames, time_base, origin, skip_before)?;
        frame = ffmpeg::frame::Video::empty();
    }
    Ok(())
}

/// Drains both decoders after end of stream.
#[allow(clippy::too_many_arguments)]
fn flush_decoders(
    video: &mut ffmpeg::decoder::Video,
    audio: &mut Option<AudioTrack>,
    scaler: &mut Option<Scaler>,
    frames: &mut VecDeque<Decoded>,
    time_base: ffmpeg::Rational,
    origin: f64,
    audio_shared: &AudioShared,
    out_channels: usize,
    out_rate: u32,
    skip_until: &mut f64,
    skip_video_before: f64,
    stop: &AtomicBool,
) -> Result<(), String> {
    let _ = video.send_eof();
    let mut frame = ffmpeg::frame::Video::empty();
    while video.receive_frame(&mut frame).is_ok() {
        push_video_frame(&frame, scaler, frames, time_base, origin, skip_video_before)?;
        frame = ffmpeg::frame::Video::empty();
    }
    if let Some(audio) = audio {
        let _ = audio.decoder.send_eof();
        let mut frame = ffmpeg::frame::Audio::empty();
        while audio.decoder.receive_frame(&mut frame).is_ok() {
            audio.push_frame(&frame, audio_shared, out_channels, out_rate, skip_until, origin, stop)?;
            frame = ffmpeg::frame::Audio::empty();
        }
    }
    Ok(())
}

/// One decoded picture ready for the pixel surface.
struct Decoded {
    pts: f64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn push_video_frame(
    frame: &ffmpeg::frame::Video,
    scaler: &mut Option<Scaler>,
    frames: &mut VecDeque<Decoded>,
    time_base: ffmpeg::Rational,
    origin: f64,
    skip_before: f64,
) -> Result<(), String> {
    let pts = frame
        .pts()
        .map(|pts| pts as f64 * f64::from(time_base) - origin)
        .unwrap_or(0.0)
        .max(0.0);
    // A seek lands on the keyframe before the target; those pictures belong
    // before the requested position and must not overwrite the new first frame.
    if pts + 0.10 < skip_before {
        return Ok(());
    }
    let width = frame.width();
    let height = frame.height();
    if width == 0 || height == 0 {
        return Ok(());
    }
    if scaler.is_none() {
        *scaler = Some(
            Scaler::get(
                frame.format(),
                width,
                height,
                Pixel::RGBA,
                width,
                height,
                Flags::BILINEAR,
            )
            .map_err(|error| format!("the video frame could not be converted: {error}"))?,
        );
    }
    let mut rgb = ffmpeg::frame::Video::empty();
    scaler
        .as_mut()
        .expect("the scaler was just created")
        .run(frame, &mut rgb)
        .map_err(|error| format!("the video frame could not be converted: {error}"))?;
    let stride = rgb.stride(0);
    let row = width as usize * 4;
    let source = rgb.data(0);
    let mut rgba = vec![0u8; row * height as usize];
    for y in 0..height as usize {
        let start = y * stride;
        if start + row > source.len() {
            break;
        }
        rgba[y * row..(y + 1) * row].copy_from_slice(&source[start..start + row]);
    }
    frames.push_back(Decoded { pts, width, height, rgba });
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Stream discovery

/// The earliest stream start time, in seconds: the content's zero. Some HLS
/// streams start at a ten-second segment offset; every frame and seek is made
/// relative to this. `0.0` when the demuxer does not say.
fn stream_origin(input: &ffmpeg::format::context::Input, video: usize, audio: Option<usize>) -> f64 {
    let mut origin: Option<f64> = None;
    for index in std::iter::once(video).chain(audio) {
        let Some(stream) = input.streams().nth(index) else { continue };
        let start = stream.start_time();
        if start == i64::MIN {
            continue;
        }
        let seconds = start as f64 * f64::from(stream.time_base());
        if seconds.is_finite() && (origin.is_none() || seconds < origin.unwrap_or(f64::INFINITY)) {
            origin = Some(seconds);
        }
    }
    origin.filter(|value| *value > 0.0).unwrap_or(0.0)
}

fn stream_duration(input: &ffmpeg::format::context::Input) -> Option<u64> {
    let duration = input.duration();
    (duration > 0).then_some((duration / 1_000_000) as u64 * 1000)
}

fn seek_to(input: &mut ffmpeg::format::context::Input, seconds: f64) -> Result<(), String> {
    let timestamp = (seconds.max(0.0) * 1_000_000.0) as i64;
    input
        .seek(timestamp, ..)
        .map_err(|error| format!("the video could not seek: {error}"))
}

// ---------------------------------------------------------------------------------------------
// HLS master playlist

struct Variant {
    url: String,
    width: u32,
    height: u32,
}

/// Fetches a small text resource (the HLS master playlist).
fn fetch_text(url: &str) -> Result<String, String> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(8)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("could not fetch TIDAL's video playlist: {error}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "TIDAL's video playlist answered HTTP {}",
            response.status().as_u16()
        ));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(256 * 1024)
        .read_to_string()
        .map_err(|error| format!("could not read TIDAL's video playlist: {error}"))?;
    if !body.contains("#EXTM3U") {
        return Err("TIDAL returned a video playlist without an #EXTM3U header".to_string());
    }
    Ok(body)
}

/// Picks the best variant up to 1080p: highest resolution, then bandwidth.
fn select_variant(master: &str) -> Result<Variant, String> {
    let lines: Vec<&str> = master.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let mut variants: Vec<(Variant, u64)> = Vec::new();
    let mut index = 0;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bandwidth = attribute(attributes, "BANDWIDTH").and_then(|value| value.parse::<u64>().ok());
            let resolution = attribute(attributes, "RESOLUTION");
            let (width, height) = match resolution.as_deref().and_then(|value| value.split_once('x')) {
                Some((width, height)) => (
                    width.trim().parse::<u32>().unwrap_or(0),
                    height.trim().parse::<u32>().unwrap_or(0),
                ),
                None => (0, 0),
            };
            let url = lines[index + 1].to_string();
            if url.starts_with("http://") || url.starts_with("https://") {
                variants.push((Variant { url, width, height }, bandwidth.unwrap_or(0)));
            }
            index += 1;
        }
        index += 1;
    }
    let mut eligible: Vec<&(Variant, u64)> = variants
        .iter()
        .filter(|(variant, _)| variant.height == 0 || variant.height <= MAX_HEIGHT)
        .collect();
    if eligible.is_empty() {
        eligible = variants.iter().collect();
    }
    eligible
        .into_iter()
        .max_by_key(|(variant, bandwidth)| (variant.height, *bandwidth))
        .map(|(variant, _)| Variant {
            url: variant.url.clone(),
            width: variant.width,
            height: variant.height,
        })
        .ok_or_else(|| "TIDAL's video playlist listed no playable variants".to_string())
}

/// The value of `name=` in an HLS attribute list (quoted or not).
fn attribute(list: &str, name: &str) -> Option<String> {
    for part in list.split(',') {
        let (key, value) = part.split_once('=')?;
        if key.trim() != name {
            continue;
        }
        let value = value.trim().trim_matches('"');
        return Some(value.to_string());
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Audio output

/// An open cpal stream and the format it runs at.
struct Output {
    stream: cpal::Stream,
    rate: u32,
    channels: usize,
}

impl Output {
    fn play(&self) -> Result<(), String> {
        self.stream
            .play()
            .map_err(|error| format!("could not start the video's audio output: {error}"))
    }

    fn stop(&mut self) {
        let _ = self.stream.pause();
    }
}

fn is_supported(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
            | SampleFormat::U64
            | SampleFormat::F32
            | SampleFormat::F64
    )
}

/// Opens the chosen device at `SOURCE_RATE`/stereo when it advertises that, and
/// at its default format otherwise. The ring and resampler follow the actual
/// rate and channel count.
fn open_output(device_name: Option<&str>, audio: &Arc<AudioShared>) -> Result<Output, String> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .output_devices()
            .ok()
            .and_then(|mut devices| devices.find(|device| device.name().ok().as_deref() == Some(name)))
            .ok_or_else(|| {
                format!("the audio output device “{name}” is not available; pick another one in Settings")
            })?,
        None => host.default_output_device().ok_or_else(|| {
            "no audio output device is available; check that PipeWire/PulseAudio (or ALSA) is running".to_string()
        })?,
    };

    let mut preferred: Option<cpal::SupportedStreamConfig> = None;
    if let Ok(configs) = device.supported_output_configs() {
        for range in configs {
            if !is_supported(range.sample_format()) {
                continue;
            }
            if range.min_sample_rate().0 > SOURCE_RATE || range.max_sample_rate().0 < SOURCE_RATE {
                continue;
            }
            let channels = range.channels();
            let score = (
                channels == 2,
                range.sample_format() == SampleFormat::F32,
                std::cmp::Reverse(channels),
            );
            let replace = preferred
                .as_ref()
                .is_none_or(|current| score > score_of(current));
            if replace {
                preferred = Some(range.with_sample_rate(cpal::SampleRate(SOURCE_RATE)));
            }
        }
    }
    let selected = match preferred {
        Some(config) => config,
        None => device
            .default_output_config()
            .map_err(|error| format!("the audio output has no usable format: {error}"))?,
    };
    let format = selected.sample_format();
    if !is_supported(format) {
        return Err(format!("the audio output uses an unsupported sample format ({format})"));
    }
    let config = selected.config();
    let rate = config.sample_rate.0;
    let channels = config.channels as usize;

    let stream = match format {
        SampleFormat::I8 => build_typed::<i8>(&device, &config, audio),
        SampleFormat::I16 => build_typed::<i16>(&device, &config, audio),
        SampleFormat::I32 => build_typed::<i32>(&device, &config, audio),
        SampleFormat::I64 => build_typed::<i64>(&device, &config, audio),
        SampleFormat::U8 => build_typed::<u8>(&device, &config, audio),
        SampleFormat::U16 => build_typed::<u16>(&device, &config, audio),
        SampleFormat::U32 => build_typed::<u32>(&device, &config, audio),
        SampleFormat::U64 => build_typed::<u64>(&device, &config, audio),
        SampleFormat::F32 => build_typed::<f32>(&device, &config, audio),
        SampleFormat::F64 => build_typed::<f64>(&device, &config, audio),
        other => return Err(format!("the audio output uses an unsupported sample format ({other})")),
    }
    .map_err(|error| format!("could not open the video's audio output: {error}"))?;
    Ok(Output { stream, rate, channels })
}

fn score_of(config: &cpal::SupportedStreamConfig) -> (bool, bool, std::cmp::Reverse<u16>) {
    (
        config.channels() == 2,
        config.sample_format() == SampleFormat::F32,
        std::cmp::Reverse(config.channels()),
    )
}

fn build_typed<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    audio: &Arc<AudioShared>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let audio = audio.clone();
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| fill::<T>(data, &audio),
        move |_error: cpal::StreamError| {},
        None,
    )
}

/// Fills `data` from the ring, applying the shared volume/mute gain.
///
/// Kept out of line so the typed builders only differ in sample conversion.
fn fill<T: Sample + FromSample<f32>>(data: &mut [T], audio: &AudioShared) {
    let channels = audio.channels();
    let paused = audio.paused.load(Ordering::SeqCst);
    let target = if paused { 0.0 } else { audio.volume.gain() };
    let mut gain = f32::from_bits(audio.gain_bits.load(Ordering::Relaxed));
    let step = 1.0 / (RAMP_SECONDS * audio.rate() as f32);
    let mut queue = match audio.ring.try_lock() {
        Ok(queue) => queue,
        Err(_) => {
            for sample in data.iter_mut() {
                *sample = T::from_sample(0.0f32);
            }
            return;
        }
    };
    let frames = data.len() / channels;
    let mut played = 0usize;
    for frame in 0..frames {
        gain = ramp(gain, target, step);
        if queue.len() < channels || (paused && gain <= 0.0) {
            for channel in 0..channels {
                data[frame * channels + channel] = T::from_sample(0.0f32);
            }
            continue;
        }
        for channel in 0..channels {
            let sample = queue.pop_front().unwrap_or(0.0);
            data[frame * channels + channel] = T::from_sample(sample * gain);
        }
        played += channels;
    }
    audio.played.fetch_add(played as u64, Ordering::Relaxed);
    audio.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
}

fn ramp(current: f32, target: f32, step: f32) -> f32 {
    if current < target {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

/// Turns the audio device's clock into a smooth one.
///
/// The device clock counts samples handed to the callback, which arrives in chunks (a
/// Bluetooth or PipeWire buffer can be a couple of hundred milliseconds), so presenting by it
/// alone shows pictures in bursts. This free-runs on the wall clock between updates and pulls
/// the estimate toward the device clock with a half-second time constant, so it never drifts
/// from the audio but advances evenly. A jump larger than [`SMOOTH_JUMP`] (a seek, a device
/// change) snaps to the device clock.
struct ClockSmoother {
    estimate: f64,
    at: Option<Instant>,
}

/// How far the estimate may trail or lead the device clock before it snaps to it, in seconds.
const SMOOTH_JUMP: f64 = 0.75;
/// Time constant of the pull toward the device clock, in seconds.
const SMOOTH_TAU: f64 = 0.5;

impl ClockSmoother {
    fn new() -> ClockSmoother {
        ClockSmoother { estimate: 0.0, at: None }
    }

    /// Forget the estimate: the next reading snaps to the device clock.
    fn reset(&mut self) {
        self.at = None;
    }

    fn tick(&mut self, device: f64, now: Instant) -> f64 {
        match self.at {
            Some(at) if (device - self.estimate).abs() <= SMOOTH_JUMP => {
                let dt = now.saturating_duration_since(at).as_secs_f64();
                let free_run = self.estimate + dt;
                let pull = (dt / SMOOTH_TAU).min(1.0);
                // Never step back: a picture already shown stays shown.
                self.estimate = (free_run + (device - free_run) * pull).max(self.estimate);
            }
            _ => self.estimate = device,
        }
        self.at = Some(now);
        self.estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A device clock that advances in 200 ms chunks, as a big output buffer makes it do:
    /// the smoothed clock must advance every 16 ms tick, stay within a chunk of the device
    /// clock, and never go backwards.
    #[test]
    fn chunky_device_clock_becomes_a_smooth_one() {
        let begin = Instant::now();
        let mut smoother = ClockSmoother::new();
        let mut last = f64::MIN;
        let mut stalls = 0;
        for tick in 0..300u64 {
            let elapsed = tick as f64 * 0.016;
            let device = (elapsed / 0.2).floor() * 0.2;
            let smooth = smoother.tick(device, begin + Duration::from_millis(tick * 16));
            assert!(smooth >= last, "went backwards at tick {tick}: {smooth} < {last}");
            if tick > 0 && smooth - last < 0.001 {
                stalls += 1;
            }
            assert!((smooth - device).abs() < 0.45, "tick {tick}: {smooth} vs device {device}");
            last = smooth;
        }
        assert_eq!(stalls, 0, "the clock advanced on every tick");
    }

    #[test]
    fn a_seek_or_reset_snaps_to_the_device_clock() {
        let begin = Instant::now();
        let mut smoother = ClockSmoother::new();
        smoother.tick(10.0, begin);
        // A big jump snaps...
        assert_eq!(smoother.tick(60.0, begin + Duration::from_millis(16)), 60.0);
        // ...and so does a reset, even to an earlier time.
        smoother.reset();
        assert_eq!(smoother.tick(5.0, begin + Duration::from_millis(32)), 5.0);
    }
}
