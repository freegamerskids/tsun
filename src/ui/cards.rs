//! Reusable catalog surfaces: album, artist and playlist cards, the grids and
//! carousels that hold them, the quality badge and the favorite heart.
//!
//! Everything TIDAL-shaped that more than one page draws lives here, so the
//! album, artist, collection and search views stay consistent.

use rune::{Align, Color, El, Icon, TextStyle, col, el, icon, row, spacer, text};

use std::rc::Rc;

use crate::app::{ArtKind, Ui};
use crate::theme::{CAPS, H2, ICON_HEART, ICON_HEART_FILLED, ICON_NOTE, ICON_PROFILE, SMALL, TRACK, Palette};
use crate::tidal::{Album, Artist, FavKind, Playlist, Track};
use crate::widgets::{cover_art, track_count};

pub(crate) use crate::ui::mixes::mix_card;

/// A stable name for `kind`, used in element keys and labels.
fn kind_key(kind: FavKind) -> &'static str {
    match kind {
        FavKind::Track => "track",
        FavKind::Album => "album",
        FavKind::Artist => "artist",
        FavKind::Playlist => "playlist",
        FavKind::Video => "video",
    }
}

/// A square cover at `size`, with `radius` corners, falling back to a note icon. The 160px
/// square, for rows and small tiles.
pub(crate) fn cover_tile(s: &Ui, p: Palette, cover: Option<&str>, size: f32) -> El {
    picture(s, p, ArtKind::Cover, cover, size, 4.0, false, &ICON_NOTE)
}

/// A square cover drawn large (an album tile or header): the 320px square, so it stays crisp on
/// a HiDPI screen.
pub(crate) fn cover_tile_large(s: &Ui, p: Palette, cover: Option<&str>, size: f32) -> El {
    picture(s, p, ArtKind::CoverLarge, cover, size, 4.0, false, &ICON_NOTE)
}

/// A circular picture at `size`, falling back to a person icon (artists, users).
pub(crate) fn round_art(s: &Ui, p: Palette, image: Option<&str>, size: f32) -> El {
    picture(s, p, ArtKind::Picture, image, size, size / 2.0, true, &ICON_PROFILE)
}

fn picture(
    s: &Ui,
    p: Palette,
    kind: ArtKind,
    image_id: Option<&str>,
    size: f32,
    radius: f32,
    round: bool,
    fallback: &'static Icon,
) -> El {
    let mut tile = el().size(size, size).radius(radius).fill(p.card);
    if round {
        tile = tile.clip_rounded(radius);
    }
    match s.wanted_art(kind, image_id) {
        Some(image) => tile.pixels_fit(image.pixels(), rune::SurfaceFit::Fill),
        None => tile.center().child(icon(fallback, size * 0.32).color(p.faint)),
    }
}

/// The outlined `MAX`/`HI-RES` quality badge.
pub(crate) fn quality_badge(p: Palette, label: &str) -> El {
    el().h(18.0).px(6.0).center().radius(3.0)
        .border(1.0, p.dim.alpha(0.55))
        .child(text(label).font(CAPS).color(p.dim))
}

/// The heart toggle: filled and bright when `kind`/`id` is a favourite, an
/// outline otherwise. Pressing it flips the favourite optimistically.
pub(crate) fn heart_button(s: &Ui, p: Palette, kind: FavKind, id: &str, size: f32) -> El {
    let on = s.is_favorite(kind, id);
    let owned = id.to_owned();
    el()
        .key(format!("heart-{}-{id}", kind_key(kind)))
        .role("button")
        .label(if on { "Remove from favorites" } else { "Add to favorites" })
        .size(size, size)
        .radius(size / 2.0)
        .center()
        .fill(Color::TRANSPARENT)
        .hover(p.card_hover)
        .press(p.accent.alpha(0.25))
        .focusable(true)
        .on_press(move |s: &mut Ui| s.toggle_favorite(kind, &owned))
        .child(
            icon(if on { &ICON_HEART_FILLED } else { &ICON_HEART }, size * 0.5)
                .color(if on { p.text } else { p.dim }),
        )
}

/// Text that navigates, styled like TIDAL's grey artist and album links.
pub(crate) fn link_text(p: Palette, label: String, style: TextStyle, on: impl Fn(&mut Ui) + 'static) -> El {
    text(label)
        .role("link")
        .font(style)
        .color(p.dim)
        .truncate()
        .radius(3.0)
        .hover(p.card_hover)
        .on_press(on)
}

/// One album tile: cover, title, artist (and year when TIDAL reports one).
pub(crate) fn album_card(s: &Ui, p: Palette, album: &Album) -> El {
    let open = album.clone();
    let subtitle = match &album.year {
        Some(year) if !album.artist.is_empty() => format!("{} · {year}", album.artist),
        Some(year) => year.clone(),
        None => album.artist.clone(),
    };
    col().key(format!("album-{}", album.id)).role("button")
        .label(format!("Open {}", album.title))
        .w(168.0).gap(8.0).focusable(true)
        .on_press(move |s: &mut Ui| s.open_album_with(open.clone()))
        .child(cover_tile_large(s, p, album.cover.as_deref(), 168.0))
        .child(text(album.title.clone()).font(TRACK).color(p.text).w_full().truncate())
        .child(text(subtitle).font(SMALL).color(p.dim).w_full().truncate())
}

/// One artist tile: a round picture with the name under it.
pub(crate) fn artist_card(s: &Ui, p: Palette, artist: &Artist) -> El {
    let open = artist.clone();
    col().key(format!("artist-{}", artist.id)).role("button")
        .label(format!("Open {}", artist.name))
        .w(148.0).items_center().gap(8.0).focusable(true)
        .on_press(move |s: &mut Ui| s.open_artist_with(open.clone()))
        .child(round_art(s, p, artist.picture.as_deref(), 128.0))
        .child(text(artist.name.clone()).font(TRACK).color(p.text).w_full().align(Align::Center).truncate())
}

/// One playlist tile: cover, title, track count.
pub(crate) fn playlist_card(s: &Ui, p: Palette, playlist: &Playlist) -> El {
    let open = playlist.clone();
    col().key(format!("playlist-card-{}", playlist.id)).role("button")
        .label(format!("Open {}", playlist.title))
        .w(168.0).gap(8.0).focusable(true)
        .on_press(move |s: &mut Ui| s.open_playlist(open.clone()))
        .child(cover_tile(s, p, playlist.cover.as_deref(), 168.0))
        .child(text(playlist.title.clone()).font(TRACK).color(p.text).w_full().truncate())
        .child(text(track_count(playlist.count)).font(SMALL).color(p.dim).w_full().truncate())
}

/// Narrowest a card in a paged carousel or grid is allowed to get, in points; the number of
/// columns follows the width of the main area so cards stay about this size however large the
/// window is made, as in the official client, instead of growing with it.
const CARD_MIN_WIDTH: f32 = 180.0;
const CARD_GAP: f32 = 16.0;
/// Side padding of the pages that hold the carousels and grids.
const PAGE_PADDING: f32 = 96.0;

/// How many cards fit across a main area `main_width` points wide.
pub(crate) fn columns_for_width(main_width: f32) -> usize {
    let room = (main_width - PAGE_PADDING).max(0.0);
    (((room + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as usize).clamp(2, 10)
}

/// Cards shown per paged carousel page and per grid row at the current window width.
pub(crate) fn columns(s: &Ui) -> usize {
    s.columns.get()
}

/// A titled paged carousel of albums: `columns` visible at a time, with the
/// `{key}-prev`/`{key}-next` chevrons paging `Ui::carousel_offsets`.
pub(crate) fn album_carousel(s: &Ui, p: Palette, key: &str, title: &str, albums: &[Album]) -> El {
    carousel_rows(s, p, key, title, albums.len(), |index| album_card(s, p, &albums[index]))
}

/// A paged carousel of artists.
pub(crate) fn artist_carousel(s: &Ui, p: Palette, key: &str, title: &str, artists: &[Artist]) -> El {
    carousel_rows(s, p, key, title, artists.len(), |index| artist_card(s, p, &artists[index]))
}

/// A paged carousel of playlists.
pub(crate) fn playlist_carousel(s: &Ui, p: Palette, key: &str, title: &str, playlists: &[Playlist]) -> El {
    carousel_rows(s, p, key, title, playlists.len(), |index| playlist_card(s, p, &playlists[index]))
}

/// The shared paged-carousel frame: `cell` draws one card, `columns(s)` are visible at a time and
/// the chevrons page the offset kept in `Ui::carousel_offsets` under `key`.
pub(crate) fn carousel_rows(
    s: &Ui,
    p: Palette,
    key: &str,
    title: &str,
    len: usize,
    mut cell: impl FnMut(usize) -> El,
) -> El {
    let columns = columns(s);
    let at = s.carousel_offsets.get(key).copied().unwrap_or(0).min(len);
    let mut line = row().w_full().gap(16.0);
    let mut shown = 0usize;
    for index in at..(at + columns).min(len) {
        line = line.child(cell(index));
        shown += 1;
    }
    for _ in shown..columns {
        line = line.child(el().grow(1.0).min_w(0.0));
    }
    let previous = at > 0;
    let next = at + columns < len;
    let previous_key = format!("{key}-prev");
    let next_key = format!("{key}-next");
    let previous_press = previous_key.clone();
    let next_press = next_key.clone();
    let step = columns;
    col().w_full().gap(14.0)
        .child(row().w_full().center_y().gap(10.0)
            .child(text(title.to_owned()).font(H2).color(p.text))
            .child(spacer())
            .child(page_button(p, &previous_key, "‹", "Previous", previous, move |s: &mut Ui| {
                let entry = s.carousel_offsets.entry(previous_press.clone()).or_insert(0);
                *entry = entry.saturating_sub(step);
            }))
            .child(page_button(p, &next_key, "›", "Next", next, move |s: &mut Ui| {
                let entry = s.carousel_offsets.entry(next_press.clone()).or_insert(0);
                *entry += step;
            })))
        .child(line)
}

fn page_button(
    p: Palette,
    key: &str,
    glyph: &str,
    label: &str,
    enabled: bool,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    let mut button = el().key(key.to_owned()).role("button").label(label.to_owned()).focusable(true)
        .disabled(!enabled).w(24.0).h(24.0).radius(12.0).fill(p.card).center()
        .child(text(glyph.to_owned()).font(TextStyle::new(18.0, 600.0))
            .color(if enabled { p.text } else { p.dim }));
    if enabled {
        button = button.hover(p.card_hover).on_press(on);
    }
    button
}

/// A square track card that starts `play` on a press (the generic-section track card).
pub(crate) fn track_card(s: &Ui, p: Palette, track: &Track, play: impl Fn(&mut Ui) + 'static) -> El {
    let id = track.id;
    let shown = Rc::new(play);
    let pressed = shown.clone();
    col().key(format!("track-{id}")).role("button").label(format!("Play {}", track.title))
        .focusable(true).grow(1.0).min_w(0.0).gap(8.0)
        .on_press(move |s: &mut Ui| pressed(s))
        .child(cover_art(s, p, track.cover.as_deref(), None))
        .child(text(track.title.clone()).font(TRACK).color(p.text).truncate())
        .child(text(track.artist.clone()).font(SMALL).color(p.dim).truncate())
}
