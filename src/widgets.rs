//! Shared controls reused across the view areas: the two button styles, section labels, cover
//! art, the per-track play/skip buttons and small text helpers.

use rune::{Color, El, TextStyle, el, icon, text};

use crate::app::Ui;
use crate::theme::{BUTTON, CAPS, ICON_NOTE, ICON_PAUSE, ICON_PLAY, Palette};

pub(crate) fn primary_button(
    p: Palette,
    label: &str,
    enabled: bool,
    on: impl Fn(&mut Ui, &mut rune::Tx) + 'static,
) -> El {
    let mut button = el()
        .role("button")
        .label(label.to_owned())
        .h(40.0)
        .px(22.0)
        .center()
        .radius(20.0)
        .fill(if enabled { p.accent } else { p.card })
        .hover(if enabled { p.accent.mix(Color::WHITE, 0.12) } else { p.card })
        .press(if enabled { p.accent.mix(Color::BLACK, 0.15) } else { p.card })
        .focusable(enabled)
        .disabled(!enabled)
        .child(
            text(label)
                .font(BUTTON)
                .color(if enabled { p.accent_ink } else { p.faint }),
        );
    if enabled {
        button = button.on_press_tx(on);
    }
    button
}

pub(crate) fn secondary_button(
    p: Palette,
    label: &str,
    enabled: bool,
    on: impl Fn(&mut Ui, &mut rune::Tx) + 'static,
) -> El {
    let mut button = el()
        .role("button")
        .label(label.to_owned())
        .h(36.0)
        .px(16.0)
        .center()
        .radius(18.0)
        .fill(if enabled { p.card } else { Color::TRANSPARENT })
        .border(1.0, if enabled { p.border } else { p.border.alpha(0.5) })
        .hover(if enabled { p.card_hover } else { Color::TRANSPARENT })
        .press(if enabled { p.accent.alpha(0.2) } else { Color::TRANSPARENT })
        .focusable(enabled)
        .disabled(!enabled)
        .child(text(label).font(BUTTON).color(if enabled { p.text } else { p.faint }));
    if enabled {
        button = button.on_press_tx(on);
    }
    button
}

pub(crate) fn section_label(label: &str, p: Palette) -> El {
    text(label).font(CAPS).color(p.faint)
}

pub(crate) fn cover_art(s: &Ui, p: Palette, cover: Option<&str>, size: Option<f32>) -> El {
    let mut tile = el().radius(3.0).fill(p.card);
    tile = match size {
        Some(size) => tile.size(size, size),
        None => tile.w_full().aspect_ratio(1.0),
    };
    // Through the shared want scheme, like every other cover: reading the cache alone would
    // never ask for a cover the first draw missed, leaving rows, thumbnails and mix tiles blank.
    match s.wanted_art(crate::app::ArtKind::Cover, cover) {
        Some(image) => tile.pixels_fit(image.pixels(), rune::SurfaceFit::Fill),
        None => tile.center().child(icon(&ICON_NOTE, 18.0).color(p.faint)),
    }
}

pub(crate) fn play_button(s: &Ui, p: Palette, id: u64, title: &str, current: bool, pending: bool) -> El {
    let busy = pending || (current && s.stopping);
    let mut button = el()
        .key(format!("play-{id}"))
        .role("button")
        .label(format!("Play {title}"))
        .size(32.0, 32.0)
        .radius(16.0)
        .center()
        .fill(if current { p.accent } else { Color::TRANSPARENT })
        .hover(if current { p.accent.mix(Color::WHITE, 0.12) } else { p.card_hover })
        .press(p.accent.alpha(0.25))
        .focusable(!busy)
        .disabled(busy);

    if busy {
        button = button.child(text("…").font(TextStyle::new(15.0, 600.0)).color(p.dim));
    } else if current {
        // The playing row toggles pause/resume; the app has no stop control.
        button = button
            .label(if s.paused { "Resume playback" } else { "Pause playback" })
            .child(icon(if s.paused { &ICON_PLAY } else { &ICON_PAUSE }, 13.0).color(p.accent_ink))
            .on_press_tx(|s: &mut Ui, _: &mut rune::Tx| s.toggle_pause());
    } else {
        button = button
            .child(icon(&ICON_PLAY, 13.0).color(p.text))
            .on_press_tx(move |s: &mut Ui, _: &mut rune::Tx| s.play_id(id));
    }
    button
}

pub(crate) fn track_count(count: u64) -> String {
    format!("{count} track{}", if count == 1 { "" } else { "s" })
}
