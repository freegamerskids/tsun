//! Collection page: the signed-in user's TIDAL library, split into Playlists,
//! Albums, Artists, Tracks and Custom mixes tabs. The data comes from
//! `Client::library()`, loaded the first time the page is opened.
//!
//! Albums, Artists, Tracks and Playlists carry a filter box and a sort dropdown, exactly as the
//! official client's Collection does. Filtering and sorting are local, so they are instant and
//! never re-hit the network; "Date added" is the order TIDAL's favourites endpoints return.
//!
//! Every tab is one virtual page (see [`crate::ui::tracks`]): the title, the tab strip, the
//! toolbar and the tab's own actions are the list's leading blocks, and its rows follow. A
//! library of thousands of tracks or albums therefore costs a window of rows per commit, and the
//! filtered, sorted view behind it is rebuilt only when its inputs change.

use rune::{Color, El, TextStyle, Tx, col, el, row, spacer, text, text_input};

use crate::app::{CollectionTab, SearchState, SortKey, Ui};
use crate::theme::{BODY, BUTTON, H1, Palette, SMALL};
use crate::tidal::{Library, Track};
use crate::ui::cards::{album_card, artist_card, playlist_card};
use crate::ui::mixes::mix_card;
use crate::ui::tracks::{
    ALBUM_H, ARTIST_H, Derived, IndexList, MIX_H, Page, PLAYLIST_H, TRACK_H, TrackList, VIDEO_H,
    grid_page, section, tracks_page, version,
};
use crate::ui::video::video_card;
use crate::widgets::{primary_button, secondary_button};

/// Whether `tab` shows the filter and sort toolbar.
fn has_toolbar(tab: CollectionTab) -> bool {
    matches!(
        tab,
        CollectionTab::Playlists
            | CollectionTab::Albums
            | CollectionTab::Artists
            | CollectionTab::Tracks
            | CollectionTab::Videos
    )
}

pub(crate) fn collection_content(s: &Ui, p: Palette) -> El {
    let (mut body, note) = collection_page(s);
    // The blocks every tab shares, in front of the tab's own: the title, the tab strip and
    // (where it applies) the toolbar.
    let mut head = vec![
        section("title", |_, p| text("Collection").font(H1).color(p.text)).after(21.0),
        section("tabs", tabs).after(21.0),
    ];
    if has_toolbar(s.collection_tab) {
        head.push(section("toolbar", toolbar).after(21.0));
    }
    body = body.leading_first(head);
    match &s.library_state {
        SearchState::Idle | SearchState::Searching => {
            body = body.trailing(
                section("loading", |_, p| {
                    col().w_full().child(text("Loading your collection…").font(BODY).color(p.dim))
                })
                .before(21.0),
            );
        }
        SearchState::Failed(error) => {
            let failure = error.clone();
            body = body.trailing(
                section("failed", move |_, p| {
                    col().w_full().gap(12.0)
                        .child(text(failure.clone()).font(BODY).color(p.dim).wrap_anywhere())
                        .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                            s.load_library();
                        }).key("collection-retry"))
                })
                .before(21.0),
            );
        }
        SearchState::Done => {
            if let Some(message) = note {
                body = body.trailing(
                    section("empty", move |_, p| {
                        col().w_full().child(text(message.clone()).font(BODY).color(p.dim).wrap_anywhere())
                    })
                    .before(21.0),
                );
            }
        }
    }
    body.render(s, p)
}

/// The page for the open tab, and the message it shows when it has nothing to list.
fn collection_page(s: &Ui) -> (Page, Option<String>) {
    let library = s.library.as_ref();
    match s.collection_tab {
        CollectionTab::Playlists => {
            let list = derived(&s.collection_playlists, s, playlists_view);
            let note = note_for(
                library.is_none_or(|library| library.playlists.is_empty()),
                list.rows.is_empty(),
                "Playlists you make or favourite on TIDAL appear here.",
                "No playlists match your filter.",
            );
            let page = grid_page(s, "collection-content", list, PLAYLIST_H, |s, p, index| {
                match s.library.as_ref().and_then(|library| library.playlists.get(index)) {
                    Some(playlist) => playlist_card(s, p, playlist),
                    None => el(),
                }
            });
            (page, note)
        }
        CollectionTab::Albums => {
            let list = derived(&s.collection_albums, s, albums_view);
            let note = note_for(
                library.is_none_or(|library| library.albums.is_empty()),
                list.rows.is_empty(),
                "Albums you favourite on TIDAL appear here.",
                "No albums match your filter.",
            );
            let page = grid_page(s, "collection-content", list, ALBUM_H, |s, p, index| {
                match s.library.as_ref().and_then(|library| library.albums.get(index)) {
                    Some(album) => album_card(s, p, album),
                    None => el(),
                }
            });
            (page, note)
        }
        CollectionTab::Artists => {
            let list = derived(&s.collection_artists, s, artists_view);
            let note = note_for(
                library.is_none_or(|library| library.artists.is_empty()),
                list.rows.is_empty(),
                "Artists you follow on TIDAL appear here.",
                "No artists match your filter.",
            );
            let page = grid_page(s, "collection-content", list, ARTIST_H, |s, p, index| {
                match s.library.as_ref().and_then(|library| library.artists.get(index)) {
                    Some(artist) => artist_card(s, p, artist),
                    None => el(),
                }
            });
            (page, note)
        }
        CollectionTab::Tracks => {
            let list = tracks_for(s);
            let note = note_for(
                library.is_none_or(|library| library.tracks.is_empty()),
                list.rows.is_empty(),
                "Tracks you favourite on TIDAL appear here.",
                "No tracks match your filter.",
            );
            let mut page = tracks_page("collection-content", list.clone(), !list.rows.is_empty(), TRACK_H);
            if !list.rows.is_empty() {
                page = page.leading(section("actions", track_actions).after(13.0));
            }
            (page, note)
        }
        CollectionTab::Videos => {
            let list = derived(&s.collection_playlists, s, videos_view);
            let note = note_for(
                library.is_none_or(|library| library.videos.is_empty()),
                list.rows.is_empty(),
                "Videos you favourite on TIDAL appear here.",
                "No videos match your filter.",
            );
            let page = grid_page(s, "collection-content", list, VIDEO_H, |s, p, index| {
                match s.library.as_ref().and_then(|library| library.videos.get(index)) {
                    Some(video) => video_card(s, p, video),
                    None => el(),
                }
            });
            (page, note)
        }
        CollectionTab::Mixes => {
            let list = IndexList::all(s.mixes.len());
            let page = grid_page(s, "collection-content", list, MIX_H, |s, p, index| {
                let Some(mix) = s.mixes.get(index) else { return el() };
                mix_card(s, p, mix)
            });
            let note = match s.mixes.is_empty() {
                true => Some("Your custom mixes will appear here.".to_owned()),
                false => None,
            };
            (page, note)
        }
        CollectionTab::History => {
            // TIDAL's own history endpoints 404, so this is what tsun itself has played.
            let list = s.history_rows.get(s.data_rev, || s.history_tracks());
            let empty = s.history_log.is_empty();
            let mut page = tracks_page("collection-content", list.clone(), !list.rows.is_empty(), TRACK_H);
            if !empty {
                page = page.leading(section("actions", |_, p| history_actions(p)).after(13.0));
            }
            let note = match empty {
                true => Some(
                    "Tracks you listen to in tsun appear here. Nothing has been played yet.".to_owned(),
                ),
                false => None,
            };
            (page, note)
        }
    }
}

/// The message a tab shows: nothing, the tab's own "empty library" note, or its filter note.
fn note_for(library_empty: bool, view_empty: bool, library_note: &str, filter_note: &str) -> Option<String> {
    match (library_empty, view_empty) {
        (true, _) => Some(library_note.to_owned()),
        (false, true) => Some(filter_note.to_owned()),
        (false, false) => None,
    }
}

/// A derived index list for the open filter and sort, rebuilt only when those change.
fn derived(
    cache: &Derived,
    s: &Ui,
    build: impl FnOnce(&Ui, &Library) -> Vec<usize>,
) -> IndexList {
    cache.get(version((s.data_rev, needle(s), s.collection_sort)), || {
        s.library.as_ref().map_or_else(Vec::new, |library| build(s, library))
    })
}

/// The favourite tracks the Tracks tab shows, from the page's cache.
fn tracks_for(s: &Ui) -> TrackList {
    s.collection_tracks
        .get(version((s.data_rev, needle(s), s.collection_sort)), || {
            s.library.as_ref().map_or_else(Vec::new, |library| tracks_view(s, library))
        })
}

/// The tab label, with the album and artist counts the official client shows.
fn tab_label(s: &Ui, tab: CollectionTab) -> String {
    match (tab, s.library.as_ref()) {
        (CollectionTab::Albums, Some(library)) => format!("Albums ({})", library.albums.len()),
        (CollectionTab::Artists, Some(library)) => format!("Artists ({})", library.artists.len()),
        (CollectionTab::Videos, Some(library)) => format!("Videos ({})", library.videos.len()),
        _ => tab.label().to_owned(),
    }
}

fn tabs(s: &Ui, p: Palette) -> El {
    let mut row = row().w_full().gap(8.0).flex_wrap();
    for tab in CollectionTab::ALL {
        let active = s.collection_tab == tab;
        let label = tab_label(s, tab);
        row = row.child(
            el().key(format!("collection-tab-{}", tab.key()))
                .role("tab")
                .label(label.clone())
                .h(34.0)
                .px(16.0)
                .center()
                .radius(17.0)
                .fill(if active { p.text } else { Color::TRANSPARENT })
                .hover(if active { p.text } else { p.card_hover })
                .focusable(true)
                .on_press(move |s: &mut Ui| s.set_collection_tab(tab))
                .child(text(label).font(BUTTON).color(if active { p.accent_ink } else { p.dim })),
        );
    }
    row
}

/// The filter box and sort dropdown.
fn toolbar(s: &Ui, p: Palette) -> El {
    row().w_full().gap(12.0).center_y()
        .child(filter_box(s, p))
        .child(sort_button(s, p))
        .child(spacer())
}

fn filter_box(s: &Ui, p: Palette) -> El {
    let mut bar = row().w(240.0).h(34.0).px(12.0).gap(6.0).center_y().radius(17.0).fill(p.card)
        .child(text_input(&s.collection_filter, TextStyle::new(13.0, 400.0))
            .key("collection-filter-input")
            .single_line()
            .grow(1.0)
            .color(p.text)
            .placeholder("Filter", p.faint)
            .on_edit(|s: &mut Ui| &mut s.collection_filter));
    if !s.collection_filter.text.is_empty() {
        bar = bar.child(
            el().key("collection-filter-clear").role("button").label("Clear filter")
                .size(20.0, 20.0).radius(10.0).center().hover(p.card_hover).focusable(true)
                .on_press(|s: &mut Ui| s.clear_collection_filter())
                .child(text("×").font(TextStyle::new(14.0, 500.0)).color(p.dim)),
        );
    }
    bar
}

fn sort_button(s: &Ui, p: Palette) -> El {
    row().key("collection-sort").role("button").label("Sort")
        .h(34.0).px(14.0).gap(8.0).center_y().radius(17.0)
        .fill(p.card).hover(p.card_hover).focusable(true)
        .expanded(s.collection_sort_open)
        .on_press(|s: &mut Ui| s.collection_sort_open = !s.collection_sort_open)
        .child(text(format!("Sort: {}", s.collection_sort.label())).font(BUTTON).color(p.text))
        .child(text("▾").font(SMALL).color(p.dim))
}

/// The sort dropdown, drawn as an overlay anchored under the sort button (see `ui::view`).
pub(crate) fn sort_menu(s: &Ui, p: Palette) -> El {
    let mut menu = col().key("collection-sort-menu").role("menu").w(184.0).padding(6.0)
        .radius(8.0).fill(p.card).border(1.0, p.border);
    for key in SortKey::ALL {
        if !key.applies_to(s.collection_tab) {
            continue;
        }
        let active = s.collection_sort == key;
        menu = menu.child(
            row().key(format!("collection-sort-{}", key.key())).role("menuitem").label(key.label())
                .w_full().h(34.0).px(12.0).center_y().gap(8.0).radius(4.0)
                .hover(p.card_hover).focusable(true)
                .on_press(move |s: &mut Ui| s.set_collection_sort(key))
                .child(text(key.label().to_owned()).font(BODY).color(if active { p.text } else { p.dim }))
                .child(spacer())
                .child(text(if active { "✓" } else { "" }).font(SMALL).color(p.accent)),
        );
    }
    menu
}

/// The History tab's toolbar: a Clear action beside a note that the list is local.
fn history_actions(p: Palette) -> El {
    row().w_full().gap(12.0).center_y()
        .child(secondary_button(p, "Clear history", true, |s: &mut Ui, _: &mut Tx| {
            s.clear_history();
        }).key("history-clear"))
        .child(text("Recorded locally by tsun; TIDAL has no listening-history API.").font(SMALL).color(p.faint))
}

/// Play and Shuffle for the (filtered, sorted) favourite-track list. The list is read from the
/// page's cache at press time, so it is whatever the filter and sort show then.
fn track_actions(s: &Ui, p: Palette) -> El {
    let rounds = tracks_for(s).rows.len();
    row().gap(12.0).center_y()
        .child(primary_button(p, "Play", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = tracks_now(s);
            if !tracks.is_empty() {
                s.play_tracks(tracks, 0);
            }
        }).key("collection-play"))
        .child(secondary_button(p, "Shuffle", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = tracks_now(s);
            s.play_tracks_shuffled(tracks);
        }).key("collection-shuffle"))
}

/// The Tracks tab's list as it stands, for the transport buttons.
fn tracks_now(s: &Ui) -> Vec<Track> {
    tracks_for(s).rows.to_vec()
}

// ---------------------------------------------------------------------------------------------
// Filtering and sorting

fn needle(s: &Ui) -> String {
    s.collection_filter.text.trim().to_lowercase()
}

fn contains(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(needle)
}

/// The indices of the playlists the tab shows, filtered and sorted.
fn playlists_view(s: &Ui, library: &Library) -> Vec<usize> {
    let needle = needle(s);
    let mut items: Vec<usize> = (0..library.playlists.len())
        .filter(|&index| contains(&library.playlists[index].title, &needle))
        .collect();
    if s.collection_sort == SortKey::Name {
        items.sort_by_cached_key(|&index| library.playlists[index].title.to_lowercase());
    }
    items
}

/// The indices of the albums the tab shows, filtered and sorted.
fn albums_view(s: &Ui, library: &Library) -> Vec<usize> {
    let needle = needle(s);
    let mut items: Vec<usize> = (0..library.albums.len())
        .filter(|&index| {
            let album = &library.albums[index];
            contains(&album.title, &needle) || contains(&album.artist, &needle)
        })
        .collect();
    match s.collection_sort {
        SortKey::DateAdded => {}
        SortKey::Name => items.sort_by_cached_key(|&index| library.albums[index].title.to_lowercase()),
        SortKey::Artist => items.sort_by_cached_key(|&index| {
            let album = &library.albums[index];
            (album.artist.to_lowercase(), album.title.to_lowercase())
        }),
        SortKey::ReleaseDate => items.sort_by_cached_key(|&index| {
            let album = &library.albums[index];
            (std::cmp::Reverse(album.year.clone()), album.title.to_lowercase())
        }),
    }
    items
}

/// The indices of the artists the tab shows, filtered and sorted.
fn artists_view(s: &Ui, library: &Library) -> Vec<usize> {
    let needle = needle(s);
    let mut items: Vec<usize> = (0..library.artists.len())
        .filter(|&index| contains(&library.artists[index].name, &needle))
        .collect();
    if s.collection_sort == SortKey::Name {
        items.sort_by_cached_key(|&index| library.artists[index].name.to_lowercase());
    }
    items
}

/// The favourite tracks the Tracks tab shows: filtered, then sorted. Rebuilt only when the
/// filter, the sort or the library changes, so a commit clones a pointer.
fn tracks_view(s: &Ui, library: &Library) -> Vec<Track> {
    let needle = needle(s);
    let mut items: Vec<Track> = library
        .tracks
        .iter()
        .filter(|track| {
            contains(&track.title, &needle)
                || contains(&track.artist, &needle)
                || contains(&track.album, &needle)
        })
        .cloned()
        .collect();
    match s.collection_sort {
        SortKey::DateAdded | SortKey::ReleaseDate => {}
        SortKey::Name => items.sort_by_cached_key(|track| track.title.to_lowercase()),
        SortKey::Artist => items.sort_by_cached_key(|track| {
            (track.artist.to_lowercase(), track.title.to_lowercase())
        }),
    }
    items
}

/// The indices of the favourite videos the Videos tab shows, filtered and sorted.
fn videos_view(s: &Ui, library: &Library) -> Vec<usize> {
    let needle = needle(s);
    let mut items: Vec<usize> = (0..library.videos.len())
        .filter(|&index| {
            let video = &library.videos[index];
            contains(&video.title, &needle) || contains(&video.artist, &needle)
        })
        .collect();
    match s.collection_sort {
        SortKey::DateAdded | SortKey::ReleaseDate => {}
        SortKey::Name => items.sort_by_cached_key(|&index| library.videos[index].title.to_lowercase()),
        SortKey::Artist => items.sort_by_cached_key(|&index| {
            let video = &library.videos[index];
            (video.artist.to_lowercase(), video.title.to_lowercase())
        }),
    }
    items
}
