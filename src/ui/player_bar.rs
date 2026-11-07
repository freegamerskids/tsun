//! Player bar: what's playing on the left (cover, title, artist, heart), the transport with
//! shuffle/repeat and a draggable progress bar in the middle, and volume, lyrics/queue toggles,
//! and the quality pill on the right. Pressing the bar opens the full-screen Now Playing view.

use rune::{Color, Drag, El, Icon, Phase, Tx, col, el, icon, oklch, row, text};

use crate::app::Ui;
use crate::theme::{
    H2, ICON_HEART, ICON_HEART_FILLED, ICON_LYRICS, ICON_NEXT, ICON_PAUSE,
    ICON_PLAY, ICON_PREVIOUS, ICON_QUEUE, ICON_REPEAT, ICON_REPEAT_ONE, ICON_SHUFFLE,
    ICON_VOLUME_HIGH, ICON_VOLUME_LOW, ICON_VOLUME_MUTE, PLAYER_H, SMALL, TRACK, Palette,
};
use crate::tidal::FavKind;
use crate::widgets::cover_art;

/// The height of the seek and volume slider tracks.
const BAR: f32 = 4.0;
/// The diameter of the round dot that marks a slider's value.
const KNOB: f32 = 10.0;
/// The diameter of the ring that fades in around the dot while its slider is hovered. A drag
/// holds the pointer on the slider, so a drag shows it too.
const KNOB_RING: f32 = 16.0;

/// How far to the left/right the transport buttons sit from the play button.
pub(crate) fn player_bar(s: &Ui, p: Palette) -> El {
    let playing = s.now_playing.as_ref().or(s.pending_play.as_ref());
    let left = now_playing_area(s, p, playing);
    let middle = transport_area(s, p, playing);
    let right = right_area(s, p, playing);
    // The whole bar opens the full-screen Now Playing view; the controls inside it take their
    // own presses, so only a press on the bare bar reaches this.
    row()
        .key("player")
        .role("button")
        .label("Open now playing view")
        .focusable(true)
        .on_press(|s: &mut Ui| s.toggle_now_playing())
        .w_full()
        .h(PLAYER_H)
        .px(20.0)
        .gap(16.0)
        .center_y()
        .fill(oklch(0.18, 0.0, 0.0))
        .border(1.0, p.border)
        .child(left)
        .child(middle)
        .child(right)
}

/// The now-playing block: cover, title, artist, album and a favourite heart. The title opens
/// its album and the artist name opens their page when the ids are known.
fn now_playing_area(s: &Ui, p: Palette, playing: Option<&crate::app::NowPlaying>) -> El {
    let cover = playing.and_then(|track| track.cover.as_deref());
    let mut cover_el = cover_art(s, p, cover, Some(52.0));
    if let Some(album_id) = playing.and_then(|track| track.album_id) {
        cover_el = el()
            .key("player-cover")
            .role("button")
            .label("Open album")
            .radius(3.0)
            .focusable(true)
            .on_press(move |s: &mut Ui| s.open_album(album_id))
            .child(cover_el);
    }

    let title = playing.map(|track| track.title.clone()).unwrap_or_else(|| "Nothing playing".to_owned());
    let mut title_el = text(title)
        .font(TRACK)
        .color(if playing.is_some() { p.text } else { p.dim })
        .truncate();
    if let Some(album_id) = playing.and_then(|track| track.album_id) {
        // `w_full`, not `grow`: this sits in a column, where growing stretches it vertically
        // and pushes the block off-centre against the cover.
        title_el = el()
            .w_full()
            .min_w(0.0)
            .role("button")
            .label("Open album")
            .focusable(true)
            .on_press(move |s: &mut Ui| s.open_album(album_id))
            .child(title_el);
    }
    let artist = playing.map(|track| track.artist.as_str()).unwrap_or("Choose a track");
    let mut artist_el = text(artist.to_owned()).font(SMALL).color(p.dim).truncate();
    if let Some(artist_id) = playing.and_then(|track| track.artist_id) {
        artist_el = el()
            .w_full()
            .min_w(0.0)
            .role("button")
            .label("Open artist")
            .focusable(true)
            .on_press(move |s: &mut Ui| s.open_artist(artist_id))
            .child(artist_el);
    }
    let album = playing.map(|track| track.album.clone()).unwrap_or_default();

    let info = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(3.0)
        .child(title_el)
        .child(artist_el)
        .child(text(album).font(SMALL).color(p.faint).truncate());

    let mut area = row().w(0.0).grow(1.0).min_w(300.0).gap(12.0).center_y().child(cover_el);
    area = area.child(info);
    // Local files are not TIDAL favourites, so they get no heart at all.
    if let Some(id) = playing.map(|track| track.id).filter(|id| !crate::local::is_local_id(*id)) {
        let id_text = id.to_string();
        let favorite = s.is_favorite(FavKind::Track, &id_text);
        area = area.child(
            el()
                .key("player-heart")
                .role("button")
                .label(if favorite { "Remove from favorites" } else { "Add to favorites" })
                .size(30.0, 30.0)
                .center()
                .radius(15.0)
                .hover(p.card_hover)
                .focusable(true)
                .on_press(move |s: &mut Ui| {
                    let id = id.to_string();
                    s.toggle_favorite(FavKind::Track, &id);
                })
                .child(icon(
                    if favorite { &ICON_HEART_FILLED } else { &ICON_HEART },
                    17.0,
                ).color(if favorite { p.accent } else { p.dim })),
        );
    }
    area
}

/// The transport: shuffle, previous, play/pause, next, repeat over a progress bar.
fn transport_area(s: &Ui, p: Palette, playing: Option<&crate::app::NowPlaying>) -> El {
    let shuffle = glyph_button(
        p,
        "shuffle",
        &ICON_SHUFFLE,
        if s.queue.shuffle() { "Shuffle on" } else { "Shuffle off" },
        s.queue.shuffle(),
        playing.is_some() || !s.queue.is_empty(),
        |s: &mut Ui| s.toggle_shuffle(),
    );
    let previous = glyph_button(
        p,
        "transport-previous",
        &ICON_PREVIOUS,
        "Previous track",
        false,
        s.can_skip_previous(),
        |s: &mut Ui| s.skip_previous(),
    );
    let next = glyph_button(
        p,
        "transport-next",
        &ICON_NEXT,
        "Next track",
        false,
        s.can_skip_next(),
        |s: &mut Ui| s.skip_next(),
    );
    let (repeat_icon, repeat_label, repeat_active) = match s.queue.repeat() {
        crate::playback::Repeat::Off => (&ICON_REPEAT, "Repeat off", false),
        crate::playback::Repeat::All => (&ICON_REPEAT, "Repeat all", true),
        crate::playback::Repeat::One => (&ICON_REPEAT_ONE, "Repeat one", true),
    };
    let repeat = glyph_button(
        p,
        "repeat",
        repeat_icon,
        repeat_label,
        repeat_active,
        playing.is_some() || !s.queue.is_empty(),
        |s: &mut Ui| s.cycle_repeat(),
    );

    let mut transport = el().key("transport").role("button").size(40.0, 40.0).center().radius(20.0).hover(p.card_hover);
    if s.now_playing.is_some() {
        transport = transport
            .label(if s.paused { "Resume playback" } else { "Pause playback" })
            .focusable(!s.stopping && !s.pause_pending)
            .disabled(s.stopping || s.pause_pending)
            .child(icon(if s.paused { &ICON_PLAY } else { &ICON_PAUSE }, 22.0).color(p.text))
            .on_press(|s: &mut Ui| s.toggle_pause());
    } else if s.pending_play.is_some() {
        transport = transport.label("Starting playback").disabled(true).child(text("…").font(H2).color(p.dim));
    } else if s.queue.current().is_some() || s.selected.is_some() {
        transport = transport
            .label("Play")
            .focusable(true)
            .child(icon(&ICON_PLAY, 22.0).color(p.text))
            .on_press(|s: &mut Ui| s.resume_or_play());
    } else {
        transport = transport.label("No track selected").disabled(true).child(icon(&ICON_PLAY, 22.0).color(p.faint));
    }

    let controls = row()
        .center()
        .gap(10.0)
        .child(shuffle)
        .child(previous)
        .child(transport)
        .child(next)
        .child(repeat);
    let mut middle = col().w(480.0).min_w(0.0).center().gap(2.0).child(controls);
    if let Some(track) = playing {
        if let Some(duration) = track.duration.filter(|duration| *duration > 0) {
            let fraction = s.scrub.unwrap_or_else(|| track.elapsed.min(duration) as f32 / duration as f32);
            let elapsed = s
                .scrub
                .map(|fraction| (fraction * duration as f32) as u64)
                .unwrap_or(track.elapsed.min(duration));
            middle = middle.child(
                row()
                    .w_full()
                    .max_w(440.0)
                    .gap(8.0)
                    .center_y()
                    .child(text(format_time(elapsed)).font(SMALL).color(p.faint))
                    .child(progress_bar(p, fraction))
                    .child(text(format_time(duration)).font(SMALL).color(p.faint)),
            );
        }
    }
    middle
}

/// The draggable progress bar: the fraction filled is the live position, or the scrubbed one,
/// with the value knob on the fill's end.
fn progress_bar(p: Palette, fraction: f32) -> El {
    row()
        .key("seek")
        .role("slider")
        .label("Seek")
        .grow(1.0)
        .h(16.0)
        .center_y()
        .focusable(true)
        // The knob's ring follows this element's hover, so a drag (which holds the pointer
        // here) and a hover read the same.
        .hover_group()
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub(fraction, d.phase == Phase::Up);
        })
        .child(slider_track(p, fraction, BAR, "seek"))
}

/// A slider track, shared by the bar's seek and volume sliders and the full-screen seek bar: a
/// bar `bar` points high, filled to `fraction` in the bright text colour over a dimmer rest,
/// with the value knob hanging off the fill's right end — where the value lands at any width,
/// so a bar that grows tracks it too. `key` names the knob for the tests.
pub(crate) fn slider_track(p: Palette, fraction: f32, bar: f32, key: &str) -> El {
    let fraction = fraction.clamp(0.0, 1.0);
    row()
        .w_full()
        .h(bar)
        .radius(bar / 2.0)
        .fill(p.card_hover)
        .child(
            el()
                .grow(fraction.max(0.0001))
                .h(bar)
                .radius(bar / 2.0)
                .fill(p.text)
                .children(knob(p, bar, key)),
        )
        .child(el().grow((1.0 - fraction).max(0.0001)).h(bar))
}

/// The value knob and its hover ring: a round dot pinned to the right edge of the track's fill,
/// with a translucent ring around it that fades in while the slider is hovered. Both are out of
/// flow, so neither disturbs the track's layout; `bar` centres them on the track's own height.
fn knob(p: Palette, bar: f32, key: &str) -> [El; 2] {
    let ring = el()
        .key(format!("{key}-ring"))
        .pin(None, Some((bar - KNOB_RING) / 2.0), Some(-KNOB_RING / 2.0), None)
        .size(KNOB_RING, KNOB_RING)
        .radius(KNOB_RING / 2.0)
        .fill(p.text.alpha(0.25))
        .group_opacity(0.0, 1.0);
    let dot = el()
        .key(format!("{key}-knob"))
        .pin(None, Some((bar - KNOB) / 2.0), Some(-KNOB / 2.0), None)
        .size(KNOB, KNOB)
        .radius(KNOB / 2.0)
        .fill(p.text);
    [ring, dot]
}

/// Volume, lyrics and queue toggles and the quality pill.
fn right_area(s: &Ui, p: Palette, playing: Option<&crate::app::NowPlaying>) -> El {
    let quality = playing
        .and_then(|track| track.quality.as_deref())
        .unwrap_or(if playing.is_some() { "Starting…" } else { "" });

    let volume_icon = if s.settings.muted {
        &ICON_VOLUME_MUTE
    } else if s.settings.volume < 0.5 {
        &ICON_VOLUME_LOW
    } else {
        &ICON_VOLUME_HIGH
    };
    let mut volume = row()
        .gap(8.0)
        .center_y()
        .child(
            el()
                .key("volume-mute")
                .role("button")
                .label(if s.settings.muted { "Unmute" } else { "Mute" })
                .size(28.0, 28.0)
                .center()
                .radius(14.0)
                .hover(p.card_hover)
                .focusable(true)
                .on_press(|s: &mut Ui| s.toggle_mute())
                .child(icon(volume_icon, 16.0).color(if s.settings.muted { p.faint } else { p.text })),
        )
        .child(volume_slider(p, if s.settings.muted { 0.0 } else { s.settings.volume }));
    // With the option off no handler is registered at all, which is what leaves the wheel over
    // this control to the page: Rune only routes a delta to `on_wheel` where one exists, and
    // scrolls otherwise.
    if s.settings.volume_scroll {
        volume = volume.on_wheel(|s: &mut Ui, ev: rune::WheelEvent, _tx: &mut Tx| {
            // Linux and Windows turn a Shift+wheel on its side before the app sees it, so read
            // whichever axis carries the notch; Shift is also what asks for the fine step.
            let dy = if ev.dy != 0.0 { ev.dy } else if ev.mods.shift { ev.dx } else { 0.0 };
            if dy != 0.0 {
                s.wheel_volume(dy, ev.mods.shift);
            }
            // The volume control owns the wheel over itself, so a scroll here never pages.
            true
        });
    }

    // Equal flexible share with the left block (the transport sits between them), but never
    // narrower than its own controls need.
    let mut right = row().w(0.0).grow(1.0).min_w(380.0).center_y().justify_end().gap(10.0);
    if !quality.is_empty() {
        // The tooltip carries the format the device actually accepted, not just the tier.
        let tooltip = match playing.and_then(|track| track.output.as_deref()) {
            Some(output) => format!("Output: {output}"),
            None => quality.to_owned(),
        };
        right = right.child(
            el()
                .key("quality-pill")
                .label(tooltip)
                .h(30.0)
                .px(14.0)
                .radius(15.0)
                .center()
                .fill(p.sidebar)
                .border(1.0, p.border)
                .child(text(quality_label(quality)).font(SMALL).color(oklch(0.78, 0.13, 170.0)).truncate()),
        );
    }
    right = right
        .child(glyph_button(
            p,
            "lyrics-toggle",
            &ICON_LYRICS,
            if s.lyrics_open { "Hide lyrics" } else { "Show lyrics" },
            s.lyrics_open,
            playing.is_some(),
            |s: &mut Ui| s.toggle_lyrics(),
        ))
        .child(glyph_button(
            p,
            "queue-toggle",
            &ICON_QUEUE,
            if s.queue_open { "Hide queue" } else { "Show queue" },
            s.queue_open,
            true,
            |s: &mut Ui| s.toggle_queue(),
        ))
        .child(volume);
    right
}

/// The volume slider: the filled part is the current volume, dragged to change it, with the
/// value knob on the fill's end.
fn volume_slider(p: Palette, volume: f32) -> El {
    el()
        .key("volume")
        .role("slider")
        .label("Volume")
        .w(96.0)
        .h(16.0)
        .center_y()
        .focusable(true)
        // The knob's ring follows this element's hover, so a drag (which holds the pointer
        // here) and a hover read the same.
        .hover_group()
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub_volume(fraction, d.phase == Phase::Up);
        })
        .child(slider_track(p, volume, BAR, "volume"))
}

/// A small square icon button; `active` gives it the accent highlight, as the official client
/// lights shuffle/repeat when they are on.
fn glyph_button(
    p: Palette,
    key: &str,
    glyph: &'static Icon,
    label: &str,
    active: bool,
    enabled: bool,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    let mut button = el()
        .key(key)
        .role("button")
        .label(label)
        .size(30.0, 30.0)
        .center()
        .radius(15.0)
        .fill(if active { p.card } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(enabled)
        .disabled(!enabled)
        // Off is a quiet grey, on is the bright cyan the official client lights shuffle and
        // repeat with, so the two states read apart at a glance.
        .child(icon(glyph, 16.0).color(if !enabled {
            p.faint
        } else if active {
            oklch(0.86, 0.13, 195.0)
        } else {
            p.dim
        }));
    if enabled {
        button = button.on_press(on);
    }
    button
}

pub(crate) fn format_time(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(crate) fn quality_label(quality: &str) -> &str {
    quality.split_once('(')
        .and_then(|(_, details)| details.strip_suffix(')'))
        .map(|details| details.strip_prefix("FLAC ").unwrap_or(details))
        .unwrap_or(quality)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{NowPlaying, init};
    use crate::theme::palette;

    fn bar_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        player_bar(s, palette())
    }

    fn runtime() -> rune::Runtime<Ui> {
        let mut s = init();
        s.now_playing = Some(NowPlaying {
            id: 7,
            title: "Track".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 6,
            duration: Some(180),
            quality: None,
            output: None,
        });
        let mut rt = rune::Runtime::new(s, bar_view, (1100.0, 84.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        rt
    }

    fn click(rt: &mut rune::Runtime<Ui>, x: f32, y: f32) {
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// Points `XDG_CONFIG_HOME` at a scratch directory, so a test that turns the wheel option
    /// on — which persists it — never touches the user's own settings file.
    struct ConfigHome {
        previous: Option<std::ffi::OsString>,
        dir: std::path::PathBuf,
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigHome {
        fn new(name: &str) -> ConfigHome {
            let turn = crate::settings::lock_config_env();
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("tsun-player-bar-{}-{name}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: `lock_config_env` is held for as long as the environment names this
            // directory, and the value is restored on drop.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
            ConfigHome { previous, dir, _turn: turn }
        }

        /// The settings file this test writes, inside its own directory.
        fn settings_path(&self) -> std::path::PathBuf {
            self.dir.join("tsun").join("settings.json")
        }
    }

    impl Drop for ConfigHome {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The bar, with the volume set the way a user's settings file would have left it.
    fn runtime_at_volume(volume: f32) -> rune::Runtime<Ui> {
        let mut rt = runtime();
        // `update` re-runs the view, and `instant` lands the fill's new width in that frame
        // instead of springing to it over the frames a `frame(0.0)` would not advance.
        rt.update(|s, tx| {
            tx.instant();
            s.settings.volume = volume;
        });
        rt.frame(0.0);
        rt
    }

    /// The drawn rect `[x, y, w, h]` and effective opacity of the element keyed `key`.
    fn element(rt: &rune::Runtime<Ui>, key: &str) -> ([f32; 4], f32) {
        let found = rt.elements().into_iter().find(|element| element.key.as_deref() == Some(key));
        let found = found.unwrap_or_else(|| panic!("no element keyed {key}"));
        (found.rect, found.opacity)
    }

    fn opacity(rt: &rune::Runtime<Ui>, key: &str) -> f32 {
        element(rt, key).1
    }

    /// The bar has no stop or expand button; pressing the bare bar opens Now Playing, while a
    /// press on a control inside it does only that control's job.
    #[test]
    fn pressing_the_bar_opens_now_playing_but_controls_keep_their_own_presses() {
        let mut rt = runtime();
        let keys: Vec<String> =
            rt.elements().into_iter().filter_map(|element| element.key).collect();
        assert!(!keys.iter().any(|key| key == "stop-playback" || key == "now-playing-open"), "{keys:?}");

        let (x, y) = rt.find("queue-toggle").expect("queue toggle");
        click(&mut rt, x, y);
        assert!(rt.state.queue_open, "the queue toggle works");
        assert!(!rt.state.now_playing_open, "a control press must not open Now Playing");

        // Between the now-playing block and the transport there is only bar.
        click(&mut rt, 336.0, 42.0);
        assert!(rt.state.now_playing_open, "a press on the bare bar opens Now Playing");
    }

    /// Main's rule: the player bar shows no heart for a local file.
    #[test]
    fn the_player_bar_has_no_heart_for_a_local_file() {
        let mut s = init();
        s.now_playing = Some(NowPlaying {
            id: crate::local::local_id(std::path::Path::new("/music/song.flac")),
            title: "Song".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 6,
            duration: Some(180),
            quality: Some("FLAC 16-bit/44.1 kHz".to_owned()),
            output: None,
        });
        let mut rt = rune::Runtime::new(s, bar_view, (1100.0, 84.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        let keys: Vec<String> =
            rt.elements().into_iter().filter_map(|element| element.key).collect();
        assert!(!keys.iter().any(|key| key == "player-heart"), "{keys:?}");
        assert!(keys.iter().any(|key| key == "quality-pill"), "the quality pill still shows the file format");
    }

    /// The volume dot marks the value: at the left end of the track for silence, half way
    /// across for 50% and at the right end for 100%.
    #[test]
    fn the_volume_dot_sits_at_the_current_volume() {
        let _config = ConfigHome::new("dot");
        let centre = |volume: f32| {
            let rt = runtime_at_volume(volume);
            let (slider, _) = element(&rt, "volume");
            let (knob, knob_opacity) = element(&rt, "volume-knob");
            assert_eq!(knob_opacity, 1.0, "the dot is always there to read the volume off");
            assert!(slider[2] > 90.0, "the volume slider is 96 points wide: {}", slider[2]);
            (knob[0] + knob[2] / 2.0) - slider[0]
        };
        assert!(centre(0.0).abs() < 1.0, "silence is the left end: {}", centre(0.0));
        assert!((centre(0.5) - 48.0).abs() < 1.5, "50% is half way: {}", centre(0.5));
        assert!((centre(1.0) - 96.0).abs() < 1.5, "100% is the right end: {}", centre(1.0));
    }

    /// The seek bar's dot marks the played position the same way, at its own width.
    #[test]
    fn the_seek_dot_sits_at_the_played_position() {
        let _config = ConfigHome::new("seek-dot");
        let rt = runtime_at_volume(0.5);
        let (seek, _) = element(&rt, "seek");
        let (knob, knob_opacity) = element(&rt, "seek-knob");
        assert_eq!(knob_opacity, 1.0, "the dot is always there to read the position off");
        assert!(seek[2] > 100.0, "the seek bar spans the middle: {}", seek[2]);
        // `runtime` plays 6 seconds of a 180-second track.
        let expected = seek[0] + seek[2] * (6.0 / 180.0);
        let centre = knob[0] + knob[2] / 2.0;
        assert!((centre - expected).abs() < 1.5, "the dot is at {centre}, the position at {expected}");
    }

    /// The ring says the slider is live: it stays out at rest, fades in while the pointer is on
    /// the slider (a drag holds the pointer there too), and belongs to that slider alone.
    #[test]
    fn each_ring_shows_only_while_its_own_slider_is_hovered() {
        let _config = ConfigHome::new("ring");
        let mut rt = runtime_at_volume(0.5);
        assert_eq!(opacity(&rt, "volume-ring"), 0.0, "nothing is hovered yet");
        assert_eq!(opacity(&rt, "seek-ring"), 0.0, "nothing is hovered yet");

        let (x, y) = rt.find("volume").expect("volume slider");
        rt.pointer_move(x, y);
        for _ in 0..120 {
            rt.frame(1.0 / 120.0);
        }
        assert!(opacity(&rt, "volume-ring") > 0.99, "the volume ring is out: {}", opacity(&rt, "volume-ring"));
        assert_eq!(opacity(&rt, "seek-ring"), 0.0, "the seek ring ignores the volume slider's hover");

        let (x, y) = rt.find("seek").expect("seek bar");
        rt.pointer_move(x, y);
        for _ in 0..120 {
            rt.frame(1.0 / 120.0);
        }
        assert!(opacity(&rt, "seek-ring") > 0.99, "the seek ring is out: {}", opacity(&rt, "seek-ring"));
        assert_eq!(opacity(&rt, "volume-ring"), 0.0, "and the volume ring is back in");
    }

    /// Off by default, the wheel over the volume control is left to the page: the volume must
    /// not move, however the wheel turns.
    #[test]
    fn the_volume_control_ignores_the_wheel_by_default() {
        let _config = ConfigHome::new("wheel-off");
        let mut rt = runtime_at_volume(1.0);
        assert!(!rt.state.settings.volume_scroll, "the option is off by default");
        let (x, y) = rt.find("volume").expect("volume slider");
        for dy in [40.0, 40.0, -120.0] {
            rt.wheel_event(rune::WheelEvent::new(0.0, dy).at(x, y));
            rt.frame(1.0 / 60.0);
        }
        assert_eq!(rt.state.settings.volume, 1.0, "a scroll near the volume bar leaves the volume alone");
    }

    /// With the option on, a notch over the whole control (mute button and slider) steps the
    /// volume by 5%, Shift asks for 1%, and both ends clamp.
    #[test]
    fn the_volume_wheel_option_steps_the_volume_and_clamps() {
        let config = ConfigHome::new("wheel-on");
        let mut rt = runtime_at_volume(0.5);
        rt.update(|s, _| s.set_volume_scroll(true));
        rt.frame(0.0);
        assert!(rt.state.settings.volume_scroll, "the option is on");
        let saved = std::fs::read_to_string(config.settings_path()).expect("the settings file");
        assert!(saved.contains("\"volume_scroll\": true"), "the choice lands in settings.json: {saved}");
        let (vx, vy) = rt.find("volume").expect("volume slider");
        let notch = |rt: &mut rune::Runtime<Ui>, at: (f32, f32), dy: f32| {
            rt.wheel_event(rune::WheelEvent::new(0.0, dy).at(at.0, at.1));
            rt.frame(1.0 / 60.0);
        };

        notch(&mut rt, (vx, vy), 40.0);
        assert!((rt.state.settings.volume - 0.55).abs() < 1e-6, "one notch up is 5%: {}", rt.state.settings.volume);
        notch(&mut rt, (vx, vy), -40.0);
        notch(&mut rt, (vx, vy), -40.0);
        assert!((rt.state.settings.volume - 0.45).abs() < 1e-6, "two notches down is 10%: {}", rt.state.settings.volume);

        // Shift is the fine step, and on Linux it arrives as a horizontal delta.
        rt.wheel_event(rune::WheelEvent::new(40.0, 0.0).at(vx, vy).mods(rune::Mods { shift: true, ..Default::default() }));
        rt.frame(1.0 / 60.0);
        assert!((rt.state.settings.volume - 0.46).abs() < 1e-6, "Shift is a 1% step: {}", rt.state.settings.volume);

        // The mute button is part of the control, so the wheel works over it too.
        let (mx, my) = rt.find("volume-mute").expect("mute button");
        notch(&mut rt, (mx, my), 40.0);
        assert!((rt.state.settings.volume - 0.51).abs() < 1e-6, "the mute button adjusts too: {}", rt.state.settings.volume);

        rt.update(|s, _| s.set_volume(0.98));
        rt.frame(0.0);
        notch(&mut rt, (vx, vy), 40.0);
        assert_eq!(rt.state.settings.volume, 1.0, "the volume clamps at the top");
        rt.update(|s, _| s.set_volume(0.02));
        rt.frame(0.0);
        notch(&mut rt, (vx, vy), -40.0);
        assert_eq!(rt.state.settings.volume, 0.0, "and at the bottom");
    }
}

#[cfg(test)]
mod centring_tests {
    use super::*;
    use crate::app::{NowPlaying, init};
    use crate::theme::palette;

    fn bar_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        player_bar(s, palette())
    }

    /// The play/pause button sits in the middle of the window, whatever the widths of the
    /// now-playing block on the left and the volume block on the right (they share the rest
    /// equally; the transport used to be pushed left because the right block was wider).
    #[test]
    fn the_transport_is_centred_in_the_window() {
        for width in [1400.0f32, 1568.0, 1920.0] {
            let mut s = init();
            s.now_playing = Some(NowPlaying {
                id: 7,
                title: "Track".to_owned(),
                artist: "Artist".to_owned(),
                album: "Album".to_owned(),
                cover: None,
                album_id: None,
                artist_id: None,
                elapsed: 6,
                duration: Some(180),
                quality: None,
                output: None,
            });
            let mut rt = rune::Runtime::new(s, bar_view, (width, 84.0), rune::Env::new(true, 1.0));
            rt.frame(0.0);
            let (x, _) = rt.find("transport").expect("play/pause button");
            assert!((x - width / 2.0).abs() < 1.5, "width {width}: play/pause at {x}, window centre {}", width / 2.0);
        }
    }
}
