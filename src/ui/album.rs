//! Album page: the header (200pt cover, ALBUM label, title, artist link, year,
//! track count, total duration, quality badge, Play/Shuffle and the favourite
//! heart) and the album's full track table.

use rune::{El, Tx, col, el, row, text};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, CAPS, H2, HERO, Palette, SMALL, SUBTITLE};
use crate::tidal::{Album, FavKind};
use crate::ui::cards::{album_card, cover_tile, heart_button, link_text, quality_badge};
use crate::ui::context_menu::{MenuState, header_menu_button};
use crate::ui::tracks::{TRACK_H, section, tracks_page};
use crate::util::{quality_badge as badge_label, total_duration};
use crate::widgets::{primary_button, secondary_button, track_count};

pub(crate) fn album_content(s: &Ui, p: Palette) -> El {
    if s.album.is_none() {
        return el();
    }
    let rows = s.album_tracks.shared();
    let mut page = tracks_page("album-content", rows.clone(), !rows.rows.is_empty(), TRACK_H)
        .leading(section("header", header).after(27.0));
    match &s.album_state {
        SearchState::Idle | SearchState::Searching => {
            page = page.trailing(
                section("loading", |_, p| {
                    col().w_full().child(text("Loading album…").font(BODY).color(p.dim))
                })
                .before(27.0),
            );
        }
        SearchState::Failed(error) => {
            let failure = error.clone();
            page = page.trailing(
                section("failed", move |_, p| {
                    col().w_full().gap(12.0)
                        .child(text(failure.clone()).font(BODY).color(p.dim).wrap_anywhere())
                        .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                            if let Some(id) = s.album.as_ref().map(|album| album.id) {
                                s.open_album(id);
                            }
                        }).key("album-retry"))
                })
                .before(27.0),
            );
        }
        SearchState::Done if rows.rows.is_empty() => {
            page = page.trailing(
                section("empty", |_, p| {
                    col().w_full().child(text("This album has no tracks.").font(BODY).color(p.dim))
                })
                .before(27.0),
            );
        }
        SearchState::Done => {}
    }
    page = page.trailing(section("extras", |s, p| extras(s, p)).before(27.0));
    page.render(s, p)
}

/// The album's extras below the track list: TIDAL's editorial review (with
/// credit to its source), the album credits, and TIDAL's related albums. Each
/// appears only when TIDAL actually returned it.
fn extras(s: &Ui, p: Palette) -> El {
    let Some(extras) = &s.album_extras else { return el() };
    let mut body = col().w_full().gap(32.0);
    if let Some(review) = &extras.review {
        let mut card = col().w_full().gap(8.0)
            .child(text("About this album").font(H2).color(p.text))
            .child(text(review.text.clone()).font(BODY).color(p.dim).wrap_anywhere());
        if let Some(source) = &review.source {
            card = card.child(text(format!("Source: {source}")).font(SMALL).color(p.faint));
        }
        body = body.child(card);
    }
    if !extras.credits.is_empty() {
        let mut credits = col().w_full().gap(6.0)
            .child(text("Credits").font(H2).color(p.text));
        for credit in &extras.credits {
            credits = credits.child(
                row().w_full().gap(10.0).items_start()
                    .child(text(credit.role.clone()).font(SMALL).color(p.faint).w(150.0))
                    .child(
                        text(credit.names.join(", "))
                            .font(SMALL)
                            .color(p.dim)
                            .grow(1.0)
                            .wrap_anywhere(),
                    ),
            );
        }
        body = body.child(credits);
    }
    if !extras.similar.is_empty() {
        let mut strip = row().w_full().gap(16.0).items_start().scroll_x().scroll_edge_fade(24.0);
        for album in &extras.similar {
            strip = strip.child(album_card(s, p, album));
        }
        body = body.child(
            col().w_full().gap(14.0)
                .child(text("Related albums").font(H2).color(p.text))
                .child(strip),
        );
    }
    body
}

/// The album page's header: its cover, title, artist, year, counts, quality and transport.
fn header(s: &Ui, p: Palette) -> El {
    let Some(album) = &s.album else { return el() };
    let row_meta = meta(s, album, p);
    row()
        .w_full()
        .gap(28.0)
        .items_end()
        .child(cover_tile(s, p, album.cover.as_deref(), 200.0))
        .child(
            col().grow(1.0).min_w(0.0).gap(12.0)
                .child(text("ALBUM").font(CAPS).color(p.dim))
                .child(text(album.title.clone()).font(HERO).color(p.text).truncate())
                .child(artist(album, p))
                .child(row_meta)
                .child(actions(s, album, p)),
        )
}

fn artist(album: &Album, p: Palette) -> El {
    match album.artist_id {
        Some(id) => {
            let artist = crate::tidal::Artist { id, name: album.artist.clone(), picture: None };
            link_text(p, album.artist.clone(), SUBTITLE, move |s: &mut Ui| s.open_artist_with(artist.clone()))
                .key("album-artist-link")
                .w_full()
        }
        None => text(album.artist.clone()).font(SUBTITLE).color(p.dim).truncate(),
    }
}

fn meta(s: &Ui, album: &Album, p: Palette) -> El {
    let mut parts = row().gap(8.0).center_y();
    if let Some(year) = &album.year {
        parts = parts
            .child(text(year.clone()).font(SMALL).color(p.dim))
            .child(text("·").font(SMALL).color(p.faint));
    }
    let count = album.tracks.map(u64::from).unwrap_or(s.album_tracks.len() as u64);
    parts = parts.child(text(track_count(count)).font(SMALL).color(p.dim));
    if let Some(duration) = album.duration.map(crate::util::format_duration).or_else(|| total_duration(s.album_tracks.items())) {
        parts = parts
            .child(text("·").font(SMALL).color(p.faint))
            .child(text(duration).font(SMALL).color(p.dim));
    }
    if let Some(label) = badge_label(album.quality.as_deref()) {
        parts = parts
            .child(text("·").font(SMALL).color(p.faint))
            .child(quality_badge(p, label));
    }
    parts
}

fn actions(s: &Ui, album: &Album, p: Palette) -> El {
    let rounds = s.album_tracks.len();
    row().gap(12.0).center_y().mt(4.0)
        .child(primary_button(p, "Play", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = s.album_tracks.to_vec();
            if !tracks.is_empty() {
                s.play_tracks(tracks, 0);
            }
        }))
        .child(secondary_button(p, "Shuffle", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = s.album_tracks.to_vec();
            s.play_tracks_shuffled(tracks);
        }).key("album-shuffle"))
        .child(heart_button(s, p, FavKind::Album, &album.id.to_string(), 40.0))
        .child(header_menu_button(
            p,
            "album-menu",
            "Album options",
            MenuState::album("album-menu", album.clone()),
        ))
}
