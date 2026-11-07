//! Custom mixes: the gallery shown on home and the "all mixes" page, plus a single mix's track
//! list.

use rune::{El, ScrollSpec, Tx, col, el, row, spacer, text};

use crate::app::{Page, SearchState, Ui};
use crate::theme::{BODY, H1, H2, SMALL, TRACK, Palette};
use crate::tidal::Mix;
use crate::ui::context_menu::{MenuState, header_menu_button};
use crate::ui::tracks::{TRACK_H, section, tracks_page};
use crate::ui::cards;
use crate::widgets::{cover_art, primary_button, secondary_button, section_label};

pub(crate) fn mix_card(s: &Ui, p: Palette, mix: &Mix) -> El {
    let selected = mix.clone();
    col().key(format!("mix-{}", mix.id)).role("button").focusable(true)
        .label(format!("Open {}", mix.title)).grow(1.0).min_w(0.0).gap(8.0)
        .on_press(move |s: &mut Ui| s.open_mix(selected.clone()))
        .child(cover_art(s, p, mix.image.as_deref(), None))
        .child(text(mix.title.clone()).font(TRACK).color(p.text).truncate())
        .child(text(mix.subtitle.clone()).font(SMALL).color(p.dim).max_lines(2).wrap_anywhere())
}

pub(crate) fn mix_gallery(s: &Ui, p: Palette) -> El {
    let columns = cards::columns(s);
    let mixes: Vec<_> = s.mixes.iter().skip(s.mix_offset).take(columns).collect();
    let mut cards = row().w_full().gap(16.0);
    for mix in &mixes { cards = cards.child(mix_card(s, p, mix)); }
    for _ in mixes.len()..columns { cards = cards.child(el().grow(1.0)); }
    let previous = s.mix_offset > 0;
    let next = s.mix_offset + columns < s.mixes.len();
    col().w_full().gap(14.0)
        .child(row().w_full().center_y().gap(10.0)
            .child(text("Custom mixes").font(H2).color(p.text))
            .child(spacer())
            .child(el().key("mixes-prev").role("button").label("Previous mixes")
                .focusable(true).disabled(!previous).w(24.0).h(24.0).radius(12.0)
                .fill(p.card).center()
                .on_press(move |s: &mut Ui| s.mix_offset = s.mix_offset.saturating_sub(columns))
                .child(text("‹").font(H2).color(if previous { p.text } else { p.dim })))
            .child(el().key("mixes-next").role("button").label("Next mixes")
                .focusable(true).disabled(!next).w(24.0).h(24.0).radius(12.0)
                .fill(p.card).center()
                .on_press(move |s: &mut Ui| {
                    if s.mix_offset + columns < s.mixes.len() { s.mix_offset += columns; }
                })
                .child(text("›").font(H2).color(if next { p.text } else { p.dim })))
            .child(el().key("mixes-all").role("button").label("View all custom mixes")
                .focusable(true)
                .on_press(|s: &mut Ui| s.page = Page::Mixes)
                .child(text("View all").font(SMALL).color(p.dim))))
        .child(cards)
}

pub(crate) fn mixes_content(s: &Ui, p: Palette) -> El {
    let mut body = col().w_full().px(48.0).pt(12.0).pb(48.0).gap(24.0)
        .child(text("Custom mixes").font(H1).color(p.text));
    let columns = cards::columns(s);
    for group in s.mixes.chunks(columns) {
        let mut cards = row().w_full().gap(16.0);
        for mix in group { cards = cards.child(mix_card(s, p, mix)); }
        for _ in group.len()..columns { cards = cards.child(el().grow(1.0)); }
        body = body.child(cards);
    }
    col().key("mixes-content").w_full().grow(1.0).min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false }).child(body)
}

pub(crate) fn mix_content(s: &Ui, p: Palette) -> El {
    if s.mix.is_none() {
        return el();
    }
    let rows = s.mix_tracks.shared();
    let mut page = tracks_page("mix-content", rows.clone(), !rows.rows.is_empty(), TRACK_H)
        .padding(48.0, 12.0, 48.0)
        .leading(section("header", header).after(23.0));
    match &s.mix_state {
        SearchState::Idle | SearchState::Searching => {
            page = page.trailing(
                section("loading", |_, p| {
                    col().w_full().child(text("Loading mix…").font(BODY).color(p.dim))
                })
                .before(23.0),
            );
        }
        SearchState::Failed(error) => {
            let failure = error.clone();
            page = page.trailing(
                section("failed", move |_, p| {
                    col().w_full().gap(12.0)
                        .child(text(failure.clone()).font(BODY).color(p.dim).wrap_anywhere())
                        .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                            let Some(mix) = s.mix.clone() else { return };
                            s.open_mix(mix);
                        }).key("mix-retry"))
                })
                .before(23.0),
            );
        }
        SearchState::Done if rows.rows.is_empty() => {
            page = page.trailing(
                section("empty", |_, p| {
                    col().w_full().child(text("This mix has no tracks.").font(BODY).color(p.dim))
                })
                .before(23.0),
            );
        }
        SearchState::Done => {}
    }
    page.render(s, p)
}

/// The mix page's header: its cover, title, subtitle, transport and menu.
fn header(s: &Ui, p: Palette) -> El {
    let Some(mix) = &s.mix else { return el() };
    row().w_full().gap(24.0).center_y()
        .child(cover_art(s, p, mix.image.as_deref(), Some(150.0)))
        .child(col().grow(1.0).min_w(0.0).gap(10.0)
            .child(section_label("MIX", p))
            .child(text(mix.title.clone()).font(H1).color(p.text).truncate())
            .child(text(mix.subtitle.clone()).font(BODY).color(p.dim).wrap_anywhere())
            .child(row().gap(12.0).center_y().mt(4.0)
                .child(primary_button(p, "Play", !s.mix_tracks.is_empty(), |s: &mut Ui, _: &mut Tx| {
                    let tracks = s.mix_tracks.to_vec();
                    s.play_tracks(tracks, 0);
                }).key("mix-play"))
                .child(secondary_button(p, "Shuffle", !s.mix_tracks.is_empty(), |s: &mut Ui, _: &mut Tx| {
                    let tracks = s.mix_tracks.to_vec();
                    s.play_tracks_shuffled(tracks);
                }).key("mix-shuffle"))
                .child(header_menu_button(
                    p,
                    "mix-menu",
                    "Mix options",
                    MenuState::mix("mix-menu", mix.clone()),
                ))))
}
