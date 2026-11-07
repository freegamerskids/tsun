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

    let mut area = row().w(300.0).min_w(0.0).gap(12.0).center_y().child(cover_el);
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
    let mut middle = col().grow(1.0).min_w(0.0).center().gap(2.0).child(controls);
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

/// The draggable progress bar: the fraction filled is the live position, or the scrubbed one.
fn progress_bar(p: Palette, fraction: f32) -> El {
    let fraction = fraction.clamp(0.0, 1.0);
    row()
        .key("seek")
        .role("slider")
        .label("Seek")
        .grow(1.0)
        .h(16.0)
        .center_y()
        .focusable(true)
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub(fraction, d.phase == Phase::Up);
        })
        .child(
            row()
                .w_full()
                .h(4.0)
                .radius(2.0)
                .fill(p.card_hover)
                .child(el().grow(fraction.max(0.0001)).h(4.0).radius(2.0).fill(p.text))
                .child(el().grow((1.0 - fraction).max(0.0001)).h(4.0)),
        )
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
    let volume = row()
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

    let mut right = row().w(380.0).center_y().justify_end().gap(10.0);
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

/// The volume slider: the filled part is the current volume, dragged to change it.
fn volume_slider(p: Palette, volume: f32) -> El {
    let volume = volume.clamp(0.0, 1.0);
    el()
        .key("volume")
        .role("slider")
        .label("Volume")
        .w(96.0)
        .h(16.0)
        .center_y()
        .focusable(true)
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub_volume(fraction, d.phase == Phase::Up);
        })
        .child(
            row()
                .w_full()
                .h(4.0)
                .radius(2.0)
                .fill(p.card_hover)
                .child(el().grow(volume.max(0.0001)).h(4.0).radius(2.0).fill(p.text))
                .child(el().grow((1.0 - volume).max(0.0001)).h(4.0)),
        )
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
}
