//! Artist page: the round portrait header (name, Play / Shuffle / Radio /
//! Follow), Top tracks, horizontal rows for Albums, EPs & singles, Appears on
//! and similar artists, and the biography when TIDAL has one.
//!
//! The page is one virtual list: the header, the "Top tracks" heading and both transport rows
//! are its leading blocks, the top tracks are its rows, and each section below (Albums, EPs &
//! singles, Appears on, Similar artists, About) is a trailing block that is only built once it
//! has scrolled into view — so an artist with hundreds of top tracks and dozens of albums costs
//! a window of rows per commit, not the whole page.

use rune::{El, Tx, col, el, icon, row, spacer, text};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, BUTTON, H2, HERO, ICON_HEART, ICON_HEART_FILLED, Palette};
use crate::tidal::{Artist, FavKind};
use crate::ui::cards::{album_carousel, artist_carousel, round_art};
use crate::ui::context_menu::{MenuState, header_menu_button};
use crate::ui::tracks::{TRACK_H, section, tracks_page, version};
use crate::ui::video::video_carousel;
use crate::widgets::{primary_button, secondary_button};

/// How many top tracks the artist page lists before "Show all".
const TOP_TRACKS: usize = 10;

pub(crate) fn artist_content(s: &Ui, p: Palette) -> El {
    if s.artist.is_none() && s.artist_page.is_none() {
        return el();
    }
    // The top tracks as shown: the ten-track preview, or every fetched track once "Show all"
    // ran. Built by the page's cache, so a commit clones a pointer.
    let list = s.artist_top.get(
        version((artist_id(s), artist_loaded(s), s.artist_top_all)),
        || match &s.artist_page {
            Some(page) => {
                let shown = if s.artist_top_all {
                    page.top_tracks.len()
                } else {
                    page.top_tracks.len().min(TOP_TRACKS)
                };
                page.top_tracks[..shown].to_vec()
            }
            None => Vec::new(),
        },
    );
    let mut page = tracks_page("artist-content", list.clone(), !list.rows.is_empty(), TRACK_H)
        .leading(section("header", header).after(31.0));
    if !list.rows.is_empty() {
        page = page.leading(section("top-heading", top_heading).after(13.0));
    }
    match &s.artist_state {
        SearchState::Idle | SearchState::Searching => {
            page = page.trailing(
                section("loading", |_, p| {
                    col().w_full().child(text("Loading artist…").font(BODY).color(p.dim))
                })
                .before(31.0),
            );
        }
        SearchState::Failed(error) => {
            let failure = error.clone();
            page = page.trailing(
                section("failed", move |_, p| {
                    col().w_full().gap(12.0)
                        .child(text(failure.clone()).font(BODY).color(p.dim).wrap_anywhere())
                        .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                            if let Some(id) = s.artist.as_ref().map(|artist| artist.id) {
                                s.open_artist(id);
                            }
                        }).key("artist-retry"))
                })
                .before(31.0),
            );
        }
        SearchState::Done => {}
    }
    // Each section is a block of its own, built only while it is near the viewport; the
    // carousels inside page their cards, so no section builds more than a screen of cards.
    if let Some(data) = &s.artist_page {
        if !data.albums.is_empty() {
            page = page.trailing(section("albums", albums).before(31.0));
        }
        if !data.videos.is_empty() {
            page = page.trailing(section("videos", videos).before(31.0));
        }
        if !data.singles.is_empty() {
            page = page.trailing(section("singles", singles).before(31.0));
        }
        if !data.appears_on.is_empty() {
            page = page.trailing(section("appears-on", appears_on).before(31.0));
        }
        if !data.similar.is_empty() {
            page = page.trailing(section("similar", similar).before(31.0));
        }
        if data.bio.is_some() {
            page = page.trailing(section("bio", bio).before(31.0));
        }
        if data.top_tracks.is_empty()
            && data.albums.is_empty()
            && data.singles.is_empty()
            && data.appears_on.is_empty()
            && data.videos.is_empty()
            && data.similar.is_empty()
            && data.bio.is_none()
        {
            page = page.trailing(
                section("empty", |_, p| {
                    col().w_full().child(text("Nothing to show for this artist yet.").font(BODY).color(p.dim))
                })
                .before(31.0),
            );
        }
    }
    page.render(s, p)
}

/// The open artist's id, from either half of the loaded state.
fn artist_id(s: &Ui) -> u64 {
    s.artist
        .as_ref()
        .map(|artist| artist.id)
        .or_else(|| s.artist_page.as_ref().map(|page| page.artist.id))
        .unwrap_or(0)
}

/// How many top tracks are loaded, so the cache rebuilds when a "Show all" page lands.
fn artist_loaded(s: &Ui) -> usize {
    s.artist_page.as_ref().map_or(0, |page| page.top_tracks.len())
}

/// The artist's portrait, name and transport.
fn header(s: &Ui, p: Palette) -> El {
    let artist = s
        .artist
        .as_ref()
        .or_else(|| s.artist_page.as_ref().map(|page| &page.artist));
    let Some(artist) = artist else { return el() };
    row()
        .w_full()
        .gap(28.0)
        .center_y()
        .child(round_art(s, p, artist.picture.as_deref(), 180.0))
        .child(
            col().grow(1.0).min_w(0.0).gap(18.0)
                .child(text(artist.name.clone()).font(HERO).color(p.text).truncate())
                .child(actions(s, artist, p)),
        )
}

/// The "Top tracks" heading, with the Show all button while TIDAL holds more.
fn top_heading(s: &Ui, p: Palette) -> El {
    let Some(page) = &s.artist_page else { return el() };
    let loaded = page.top_tracks.len();
    // The button appears while TIDAL still holds tracks we have not fetched, or while some of
    // what we have fetched is still hidden behind the ten-track preview.
    let more_on_tidal = page.top_tracks_total.is_some_and(|total| total > loaded as u64);
    let hidden_locally = !s.artist_top_all && loaded > TOP_TRACKS;
    let has_more = more_on_tidal || hidden_locally;
    let id = page.artist.id;
    let mut heading = row().w_full().center_y().gap(10.0)
        .child(text("Top tracks").font(H2).color(p.text));
    if has_more {
        let label = if s.artist_top_loading { "Loading…" } else { "Show all" };
        heading = heading.child(spacer()).child(
            secondary_button(p, label, !s.artist_top_loading, move |s: &mut Ui, _: &mut Tx| {
                s.show_all_top_tracks(id);
            })
            .key("artist-top-show-all"),
        );
    }
    heading
}

fn actions(s: &Ui, artist: &Artist, p: Palette) -> El {
    let id = artist.id;
    let has_tracks = s.artist_page.as_ref().is_some_and(|page| !page.top_tracks.is_empty());
    row().gap(12.0).center_y()
        .child(primary_button(p, "Play", has_tracks, move |s: &mut Ui, _: &mut Tx| {
            let tracks = s.artist_page.as_ref().map(|page| page.top_tracks.clone()).unwrap_or_default();
            if !tracks.is_empty() {
                s.play_tracks(tracks, 0);
            }
        }))
        .child(secondary_button(p, "Shuffle", has_tracks, move |s: &mut Ui, _: &mut Tx| {
            let tracks = s
                .artist_page
                .as_ref()
                .map(|page| page.top_tracks.clone())
                .unwrap_or_default();
            s.play_tracks_shuffled(tracks);
        }).key("artist-shuffle"))
        .child(secondary_button(p, "Radio", true, move |s: &mut Ui, _: &mut Tx| {
            s.play_artist_radio(id);
        }))
        .child(follow_button(s, artist, p))
        .child(header_menu_button(
            p,
            "artist-menu",
            "Artist options",
            MenuState::artist("artist-menu", artist.clone()),
        ))
}

fn follow_button(s: &Ui, artist: &Artist, p: Palette) -> El {
    let id = artist.id.to_string();
    let on = s.is_favorite(FavKind::Artist, &id);
    el()
        .key("follow-artist")
        .role("button")
        .label(if on { "Unfollow artist" } else { "Follow artist" })
        .h(36.0)
        .px(16.0)
        .center()
        .radius(18.0)
        .fill(if on { p.card_hover } else { p.card })
        .border(1.0, p.border)
        .hover(p.card_hover)
        .press(p.accent.alpha(0.2))
        .focusable(true)
        .on_press(move |s: &mut Ui| s.toggle_favorite(FavKind::Artist, &id))
        .child(
            row().gap(8.0).center_y()
                .child(
                    icon(if on { &ICON_HEART_FILLED } else { &ICON_HEART }, 15.0)
                        .color(if on { p.text } else { p.dim }),
                )
                .child(text(if on { "Following" } else { "Follow" }).font(BUTTON).color(if on { p.text } else { p.dim })),
        )
}

/// The artist's albums, paged `columns` cards at a time.
fn albums(s: &Ui, p: Palette) -> El {
    let Some(page) = &s.artist_page else { return el() };
    album_carousel(s, p, "artist-albums", "Albums", &page.albums)
}

fn videos(s: &Ui, p: Palette) -> El {
    let Some(page) = &s.artist_page else { return el() };
    video_carousel(s, p, "artist-videos", "Videos", &page.videos)
}

fn singles(s: &Ui, p: Palette) -> El {
    let Some(page) = &s.artist_page else { return el() };
    album_carousel(s, p, "artist-singles", "EPs & singles", &page.singles)
}

fn appears_on(s: &Ui, p: Palette) -> El {
    let Some(page) = &s.artist_page else { return el() };
    album_carousel(s, p, "artist-appears-on", "Appears on", &page.appears_on)
}

fn similar(s: &Ui, p: Palette) -> El {
    let Some(page) = &s.artist_page else { return el() };
    artist_carousel(s, p, "artist-similar", "Similar artists", &page.similar)
}

fn bio(s: &Ui, p: Palette) -> El {
    let Some(bio) = s.artist_page.as_ref().and_then(|page| page.bio.as_deref()) else {
        return el();
    };
    col().w_full().gap(14.0)
        .child(text("About").font(H2).color(p.text))
        .child(text(bio.to_owned()).font(BODY).color(p.dim).wrap_anywhere())
}
