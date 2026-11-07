//! The full-screen Now Playing view: a `Page`-independent overlay with large cover art, the
//! track's links, a seekable progress bar, the full transport and — alongside — the synced
//! lyrics.
//!
//! It is deliberately not a `Page`: it renders over whatever page is showing and is opened by the
//! player bar's maximize button (or `Ui::toggle_now_playing`), closed by the minimize button or
//! Escape. Everything it draws is real state: the cover comes from [`crate::cover::CoverArt`]
//! (the same pixels the gradient's tint is averaged from), and every control calls the very same
//! [`Ui`] methods the player bar does.

use rune::{Color, Drag, El, Phase, ScrollSpec, col, el, icon, oklch, row, text};

use crate::app::Ui;
use crate::theme::{
    BODY, CAPS, H1, H2, ICON_CLOSE, ICON_HEART, ICON_HEART_FILLED, ICON_LYRICS, ICON_NEXT,
    ICON_NOTE, ICON_PAUSE, ICON_PLAY, ICON_PREVIOUS, ICON_REPEAT, ICON_REPEAT_ONE, ICON_SHUFFLE,
    Palette, SMALL, SUBTITLE, TRACK,
};
use crate::tidal::FavKind;
use crate::ui::{lyrics, player_bar};

/// The cover size in the full-screen view: the maximum the layout allows, shrunk a little when
/// the lyrics need the space on the right.
const COVER_MAX: f32 = 480.0;
const COVER_WITH_LYRICS: f32 = 400.0;

/// The full-screen overlay, drawn over everything else.
pub(crate) fn now_playing_overlay(s: &Ui, p: Palette) -> El {
    let playing = s.now_playing.as_ref().or(s.pending_play.as_ref());
    let tint = playing
        .and_then(|track| track.cover.as_deref())
        .and_then(|cover| s.covers.as_ref()?.tint(cover));
    let (top, bottom) = gradient(tint);

    let mut root = col()
        .key("now-playing")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .gradient(top, bottom)
        .padding(28.0);
    if playing.is_none() {
        return root.center().child(
            col().gap(10.0).center()
                .child(text("Nothing playing").font(H1).color(p.text))
                .child(text("Pick a track and this view fills in.").font(BODY).color(p.dim))
                .child(minimize_button(p)),
        );
    }

    let cover = cover_block(s, p, playing.and_then(|track| track.cover.as_deref()));
    let details = details_block(s, p);
    let transport = transport_block(s, p);
    let left = col()
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .min_h(0.0)
        .center()
        .gap(18.0)
        .child(cover)
        .child(details)
        .child(transport);

    let mut body = row().w_full().grow(1.0).min_h(0.0).gap(28.0).child(left);
    if s.lyrics_open {
        body = body.child(
            col()
                .w(380.0)
                .h_full()
                .min_h(0.0)
                .px(4.0)
                .gap(12.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(lyrics::lyrics_panel(s, p)),
        );
    }

    root = root
        .child(
            row().w_full().center_y().gap(12.0)
                .child(text("Now playing").font(CAPS).color(p.dim).grow(1.0))
                .child(lyrics_toggle(s, p))
                .child(minimize_button(p)),
        )
        .child(body);
    root
}

/// The dark gradient: the cover's own average colour, held to a low OKLab lightness so white text
/// stays readable, fading to near-black.
fn gradient(tint: Option<[f32; 4]>) -> (Color, Color) {
    let Some([r, g, b, a]) = tint else {
        return (oklch(0.14, 0.0, 0.0), oklch(0.03, 0.0, 0.0));
    };
    let base = Color { r, g, b, a };
    let [lightness, axis_a, axis_b, _] = base.to_oklab();
    let top = Color::from_oklab([lightness.clamp(0.14, 0.40), axis_a * 0.85, axis_b * 0.85, 1.0]);
    let bottom = Color::from_oklab([0.04, axis_a * 0.35, axis_b * 0.35, 1.0]);
    (top, bottom)
}

/// The large cover, rounded, with a placeholder until its pixels arrive.
fn cover_block(s: &Ui, p: Palette, cover: Option<&str>) -> El {
    let size = if s.lyrics_open { COVER_WITH_LYRICS } else { COVER_MAX };
    let mut tile = el().key("now-playing-cover").size(size, size).radius(10.0).fill(p.card);
    match cover.and_then(|id| s.covers.as_ref()?.image(id)) {
        Some(image) => tile = tile.pixels_fit(image.pixels(), rune::SurfaceFit::Fill),
        None => tile = tile.center().child(icon(&ICON_NOTE, 56.0).color(p.faint)),
    }
    tile
}

/// Title, artist, album, the favourite heart and the real output format.
fn details_block(s: &Ui, p: Palette) -> El {
    let playing = s.now_playing.as_ref().or(s.pending_play.as_ref());
    let title = playing.map(|track| track.title.clone()).unwrap_or_default();
    let artist = playing.map(|track| track.artist.clone()).unwrap_or_default();
    let album = playing.map(|track| track.album.clone()).unwrap_or_default();

    let mut title_el = text(title).font(H1).color(p.text).truncate();
    if let Some(album_id) = playing.and_then(|track| track.album_id) {
        title_el = el().role("button").label("Open album").focusable(true)
            .on_press(move |s: &mut Ui| s.open_album(album_id)).child(title_el);
    }
    let mut artist_el = text(artist).font(SUBTITLE).color(p.dim).truncate();
    if let Some(artist_id) = playing.and_then(|track| track.artist_id) {
        artist_el = el().role("button").label("Open artist").focusable(true)
            .on_press(move |s: &mut Ui| s.open_artist(artist_id)).child(artist_el);
    }

    let mut favourites = row().gap(6.0).center_y();
    // Local files have no TIDAL favourite, so they show no heart.
    if let Some(id) = playing
        .map(|track| track.id)
        .filter(|id| !crate::local::is_local_id(*id))
    {
        let id_text = id.to_string();
        let favorite = s.is_favorite(FavKind::Track, &id_text);
        favourites = favourites.child(
            el()
                .key("now-playing-heart")
                .role("button")
                .label(if favorite { "Remove from favorites" } else { "Add to favorites" })
                .size(34.0, 34.0)
                .center()
                .radius(17.0)
                .hover(p.card_hover)
                .focusable(true)
                .on_press(move |s: &mut Ui| {
                    let id = id.to_string();
                    s.toggle_favorite(FavKind::Track, &id);
                })
                .child(icon(
                    if favorite { &ICON_HEART_FILLED } else { &ICON_HEART },
                    18.0,
                ).color(if favorite { p.accent } else { p.dim })),
        );
    }
    // The format the device really accepted, not the tier we asked for.
    let output = playing.and_then(|track| track.output.clone());
    if let Some(output) = output {
        favourites = favourites.child(
            text(format!("Output: {output}"))
                .font(SMALL)
                .color(oklch(0.78, 0.13, 170.0))
                .truncate(),
        );
    }

    col()
        .w_full()
        .max_w(620.0)
        .min_w(0.0)
        .gap(8.0)
        .child(title_el)
        .child(artist_el)
        .child(if album.is_empty() {
            el()
        } else {
            text(album).font(SMALL).color(p.faint).truncate()
        })
        .child(favourites)
}

/// Progress bar and transport, all wired to the same [`Ui`] actions as the player bar.
fn transport_block(s: &Ui, p: Palette) -> El {
    let playing = s.now_playing.as_ref().or(s.pending_play.as_ref());
    let mut progress = col().key("now-playing-progress").w_full().max_w(620.0).gap(6.0);
    if let Some(track) = playing {
        if let Some(duration) = track.duration.filter(|duration| *duration > 0) {
            let fraction = s
                .scrub
                .unwrap_or_else(|| track.elapsed.min(duration) as f32 / duration as f32);
            let elapsed = s
                .scrub
                .map(|fraction| (fraction * duration as f32) as u64)
                .unwrap_or(track.elapsed.min(duration));
            progress = progress.child(
                row()
                    .w_full()
                    .gap(10.0)
                    .center_y()
                    .child(text(player_bar::format_time(elapsed)).font(SMALL).color(p.dim))
                    .child(seek_bar(p, fraction))
                    .child(text(player_bar::format_time(duration)).font(SMALL).color(p.dim)),
            );
        }
    }

    let transport = if s.now_playing.is_some() {
        let mut button = el()
            .key("now-playing-pause")
            .role("button")
            .label(if s.paused { "Resume playback" } else { "Pause playback" })
            .size(56.0, 56.0)
            .center()
            .radius(28.0)
            .fill(p.text)
            .hover(p.text.mix(Color::WHITE, 0.2))
            .focusable(!s.stopping && !s.pause_pending)
            .disabled(s.stopping || s.pause_pending)
            .child(icon(if s.paused { &ICON_PLAY } else { &ICON_PAUSE }, 26.0).color(p.bg));
        if !s.stopping && !s.pause_pending {
            button = button.on_press(|s: &mut Ui| s.toggle_pause());
        }
        button
    } else {
        el().key("now-playing-pause").role("button").label("Starting playback").size(56.0, 56.0)
            .center().radius(28.0).fill(p.card).disabled(true)
            .child(text("…").font(H2).color(p.dim))
    };

    let controls = row()
        .gap(14.0)
        .center_y()
        .child(big_button(
            p,
            "now-playing-shuffle",
            &ICON_SHUFFLE,
            if s.queue.shuffle() { "Shuffle on" } else { "Shuffle off" },
            s.queue.shuffle(),
            playing.is_some() || !s.queue.is_empty(),
            |s: &mut Ui| s.toggle_shuffle(),
        ))
        .child(big_button(
            p,
            "now-playing-previous",
            &ICON_PREVIOUS,
            "Previous track",
            false,
            s.can_skip_previous(),
            |s: &mut Ui| s.skip_previous(),
        ))
        .child(transport)
        .child(big_button(
            p,
            "now-playing-next",
            &ICON_NEXT,
            "Next track",
            false,
            s.can_skip_next(),
            |s: &mut Ui| s.skip_next(),
        ))
        .child(big_button(
            p,
            "now-playing-repeat",
            match s.queue.repeat() {
                crate::playback::Repeat::Off => &ICON_REPEAT,
                crate::playback::Repeat::All => &ICON_REPEAT,
                crate::playback::Repeat::One => &ICON_REPEAT_ONE,
            },
            match s.queue.repeat() {
                crate::playback::Repeat::Off => "Repeat off",
                crate::playback::Repeat::All => "Repeat all",
                crate::playback::Repeat::One => "Repeat one",
            },
            s.queue.repeat() != crate::playback::Repeat::Off,
            playing.is_some() || !s.queue.is_empty(),
            |s: &mut Ui| s.cycle_repeat(),
        ));

    col().w_full().max_w(620.0).gap(14.0).child(progress).child(controls.center())
}

/// The draggable seek bar; scrolling the whole overlay is never needed, so it may be generous.
/// Its knob is the player bar's, so both views mark the position the same way.
fn seek_bar(p: Palette, fraction: f32) -> El {
    row()
        .key("now-playing-seek")
        .role("slider")
        .label("Seek")
        .grow(1.0)
        .h(20.0)
        .center_y()
        .focusable(true)
        // The knob's ring follows this element's hover, so a drag (which holds the pointer
        // here) and a hover read the same.
        .hover_group()
        .on_drag(|s: &mut Ui, d: &mut Drag, _tx: &mut rune::Tx| {
            let fraction = if d.w > 0.0 { d.x / d.w } else { 0.0 };
            s.scrub(fraction, d.phase == Phase::Up);
        })
        .child(player_bar::slider_track(p, fraction, 5.0, "now-playing-seek"))
}

/// A round icon button sized for the full-screen transport.
fn big_button(
    p: Palette,
    key: &str,
    glyph: &'static rune::Icon,
    label: &str,
    active: bool,
    enabled: bool,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    let mut button = el()
        .key(key)
        .role("button")
        .label(label)
        .size(44.0, 44.0)
        .center()
        .radius(22.0)
        .fill(if active { p.card } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(enabled)
        .disabled(!enabled)
        .child(icon(glyph, 20.0).color(if !enabled {
            p.faint
        } else if active {
            p.accent
        } else {
            p.text
        }));
    if enabled {
        button = button.on_press(on);
    }
    button
}

/// The minimize button: the same view toggles closed on Escape.
fn minimize_button(p: Palette) -> El {
    el()
        .key("now-playing-minimize")
        .role("button")
        .label("Close now playing")
        .size(38.0, 38.0)
        .center()
        .radius(19.0)
        .hover(p.card_hover)
        .focusable(true)
        .on_press(|s: &mut Ui| s.now_playing_open = false)
        .child(icon(&ICON_CLOSE, 20.0).color(p.text))
}

/// The lyrics toggle, so the words can be brought alongside without closing the view.
fn lyrics_toggle(s: &Ui, p: Palette) -> El {
    let mut button = el()
        .key("now-playing-lyrics")
        .role("button")
        .label(if s.lyrics_open { "Hide lyrics" } else { "Show lyrics" })
        .h(32.0)
        .px(14.0)
        .center()
        .radius(16.0)
        .fill(if s.lyrics_open { p.card } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .on_press(|s: &mut Ui| s.toggle_lyrics())
        .child(
            row().gap(8.0).center_y()
                .child(icon(&ICON_LYRICS, 15.0).color(if s.lyrics_open { p.accent } else { p.text }))
                .child(text("Lyrics").font(TRACK).color(p.text)),
        );
    if !s.lyrics_open {
        button = button.fill(Color::TRANSPARENT);
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{NowPlaying, init};
    use crate::theme::palette;

    fn overlay_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        now_playing_overlay(s, palette())
    }

    /// Main's rule: the full-screen view has no heart for a local file.
    #[test]
    fn now_playing_has_no_heart_for_a_local_file() {
        let mut s = init();
        s.now_playing_open = true;
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
        let mut rt = rune::Runtime::new(s, overlay_view, (1200.0, 800.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        let keys: Vec<String> =
            rt.elements().into_iter().filter_map(|element| element.key).collect();
        assert!(!keys.iter().any(|key| key == "now-playing-heart"), "{keys:?}");
    }

    /// The full-screen seek bar marks the played position with the same knob the player bar
    /// draws, so the two views read the position the same way.
    #[test]
    fn the_seek_bar_marks_the_played_position_with_a_knob() {
        let mut s = init();
        s.now_playing_open = true;
        s.now_playing = Some(NowPlaying {
            id: 7,
            title: "Track".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 90,
            duration: Some(180),
            quality: None,
            output: None,
        });
        let mut rt = rune::Runtime::new(s, overlay_view, (1200.0, 800.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        let rect = |key: &str| {
            rt.elements()
                .into_iter()
                .find(|element| element.key.as_deref() == Some(key))
                .map(|element| element.rect)
                .unwrap_or_else(|| panic!("no element keyed {key}"))
        };
        let bar = rect("now-playing-seek");
        let knob = rect("now-playing-seek-knob");
        assert!(bar[2] > 300.0, "the seek bar spans the middle of the overlay: {}", bar[2]);
        let centre = knob[0] + knob[2] / 2.0;
        assert!(
            (centre - (bar[0] + bar[2] / 2.0)).abs() < 1.5,
            "90 of 180 seconds is the middle of the bar: the knob is at {centre}, the bar at {bar:?}"
        );
        assert!(
            rt.elements().into_iter().any(|element| element.key.as_deref() == Some("now-playing-seek-ring")),
            "the hover ring comes with the knob"
        );
    }
}
