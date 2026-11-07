//! Search page: the idle/searching/failed/done states, the tabbed results (top hit, tracks,
//! albums, artists, playlists), the suggestion dropdown, and the shared track table the
//! collection, playlist, mix, album and artist pages also use.

use rune::{Color, El, ScrollSpec, Tx, col, el, icon, row, spacer, text};

use crate::app::{SearchState, SearchTab, Ui};
use crate::theme::{BODY, BUTTON, CAPS, H1, H2, ICON_SEARCH, SMALL, TRACK, Palette};
use crate::tidal::{SearchAll, TopHit, Track};
use crate::ui::cards::{
    album_card, album_carousel, artist_card, artist_carousel, playlist_card, playlist_carousel,
    round_art,
};
use crate::ui::tracks::{ARTIST_H, ALBUM_H, IndexList, PLAYLIST_H, TRACK_H, VIDEO_H, grid_page, tracks_page};
use crate::ui::video::{video_carousel, video_card};
use crate::widgets::{cover_art, play_button, primary_button, secondary_button, section_label, track_count};
use crate::ui::context_menu::{MenuState, TrackMenu, menu_button};

pub(crate) fn search_content(s: &Ui, p: Palette) -> El {
    let mut content = col()
        .key("search-content")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .px(32.0)
        .pt(20.0)
        .pb(24.0)
        .gap(18.0);

    match &s.search_state {
        SearchState::Idle => {
            content = content.child(
                col()
                    .w_full()
                    .grow(1.0)
                    .min_h(0.0)
                    .center()
                    .gap(12.0)
                    .child(icon(&ICON_SEARCH, 44.0).color(p.faint))
                    .child(text("Search TIDAL").font(H1).color(p.text))
                    .child(
                        text("Find tracks, albums, artists and playlists.")
                            .font(BODY)
                            .color(p.dim),
                    ),
            );
        }
        SearchState::Searching => {
            content = content
                .child(
                    text(format!("Searching for “{}”…", s.last_query))
                        .font(H2)
                        .color(p.text)
                        .truncate(),
                )
                .child(text("Waiting for TIDAL to answer.").font(BODY).color(p.dim));
        }
        SearchState::Failed(error) => {
            content = content
                .child(text("Search failed").font(H2).color(p.text))
                .child(text(error.clone()).font(BODY).color(p.danger).wrap_anywhere())
                .child(
                    row()
                        .gap(10.0)
                        .child(primary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                            s.submit_search()
                        }))
                        .child(secondary_button(p, "Sign in again", true, |s: &mut Ui, _: &mut Tx| {
                            s.start_pkce()
                        })),
                );
        }
        SearchState::Done => {
            if s.search_all.as_ref().is_none_or(search_all_is_empty) {
                content = content
                    .child(text("No results").font(H2).color(p.text))
                    .child(
                        text(format!(
                            "Nothing matched “{}”. Try another spelling, artist, or album.",
                            s.last_query
                        ))
                        .font(BODY)
                        .color(p.dim)
                        .wrap_anywhere(),
                    )
                    .child(secondary_button(p, "Clear search", true, |s: &mut Ui, _: &mut Tx| {
                        s.clear_search()
                    }));
            } else {
                content = content
                    .child(tabs_row(s, p))
                    .child(tab_body(s, p));
            }
        }
    }

    content
}

fn search_all_is_empty(all: &SearchAll) -> bool {
    all.tracks.is_empty()
        && all.albums.is_empty()
        && all.artists.is_empty()
        && all.playlists.is_empty()
        && all.videos.is_empty()
        && all.top.is_none()
}

fn tabs_row(s: &Ui, p: Palette) -> El {
    row()
        .key("search-tabs")
        .w_full()
        .gap(8.0)
        .flex_wrap()
        .child(tab_chip(s, p, SearchTab::Top, "Top results", "search-tab-top"))
        .child(tab_chip(s, p, SearchTab::Tracks, "Tracks", "search-tab-tracks"))
        .child(tab_chip(s, p, SearchTab::Albums, "Albums", "search-tab-albums"))
        .child(tab_chip(s, p, SearchTab::Artists, "Artists", "search-tab-artists"))
        .child(tab_chip(s, p, SearchTab::Playlists, "Playlists", "search-tab-playlists"))
        .child(tab_chip(s, p, SearchTab::Videos, "Videos", "search-tab-videos"))
}

fn tab_chip(s: &Ui, p: Palette, tab: SearchTab, label: &str, key: &str) -> El {
    let active = s.search_tab == tab;
    el()
        .key(key.to_owned())
        .role("button")
        .label(label.to_owned())
        .focusable(true)
        .h(36.0)
        .px(18.0)
        .center()
        .radius(18.0)
        .fill(if active { p.text } else { p.card })
        .hover(if active { p.text } else { p.card_hover })
        .on_press(move |s: &mut Ui| s.search_tab = tab)
        .child(text(label.to_owned()).font(BUTTON).color(if active { p.accent_ink } else { p.dim }))
}

fn tab_body(s: &Ui, p: Palette) -> El {
    let Some(all) = &s.search_all else { return el() };
    match s.search_tab {
        // The top-results tab is a short mixed column, so it scrolls as plain content.
        SearchTab::Top => col()
            .key("search-scroll")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec { stick_bottom: false })
            .scroll_edge_fade(24.0)
            .child(top_results(s, p, all)),
        SearchTab::Tracks if all.tracks.is_empty() => empty_note(p, "No tracks matched."),
        SearchTab::Tracks => {
            // The results list is already held as one shared allocation by `Tracks::shared`,
            // so the page clones a pointer.
            tracks_page("search-tracks", s.results.shared(), true, TRACK_H)
                .padding(0.0, 0.0, 24.0)
                .render(s, p)
        }
        SearchTab::Albums if all.albums.is_empty() => empty_note(p, "No albums matched."),
        SearchTab::Albums => {
            let list = IndexList::all(all.albums.len());
            grid_page(s, "search-albums", list, ALBUM_H, |s, p, index| {
                match s.search_all.as_ref().and_then(|all| all.albums.get(index)) {
                    Some(album) => album_card(s, p, album),
                    None => el(),
                }
            })
            .padding(0.0, 0.0, 24.0)
            .render(s, p)
        }
        SearchTab::Artists if all.artists.is_empty() => empty_note(p, "No artists matched."),
        SearchTab::Artists => {
            let list = IndexList::all(all.artists.len());
            grid_page(s, "search-artists", list, ARTIST_H, |s, p, index| {
                match s.search_all.as_ref().and_then(|all| all.artists.get(index)) {
                    Some(artist) => artist_card(s, p, artist),
                    None => el(),
                }
            })
            .padding(0.0, 0.0, 24.0)
            .render(s, p)
        }
        SearchTab::Playlists if all.playlists.is_empty() => empty_note(p, "No playlists matched."),
        SearchTab::Playlists => {
            let list = IndexList::all(all.playlists.len());
            grid_page(s, "search-playlists", list, PLAYLIST_H, |s, p, index| {
                match s.search_all.as_ref().and_then(|all| all.playlists.get(index)) {
                    Some(playlist) => playlist_card(s, p, playlist),
                    None => el(),
                }
            })
            .padding(0.0, 0.0, 24.0)
            .render(s, p)
        }
        SearchTab::Videos if all.videos.is_empty() => empty_note(p, "No videos matched."),
        SearchTab::Videos => {
            let list = IndexList::all(all.videos.len());
            grid_page(s, "search-videos", list, VIDEO_H, |s, p, index| {
                match s.search_all.as_ref().and_then(|all| all.videos.get(index)) {
                    Some(video) => video_card(s, p, video),
                    None => el(),
                }
            })
            .padding(0.0, 0.0, 24.0)
            .render(s, p)
        }
    }
}

fn empty_note(p: Palette, message: &str) -> El {
    col().w_full().py(24.0).child(text(message.to_owned()).font(BODY).color(p.dim))
}

/// The official top-results tab: the strongest hit as a large card beside a short list of
/// tracks, then the album, artist and playlist rows.
fn top_results(s: &Ui, p: Palette, all: &SearchAll) -> El {
    let mut body = col().w_full().gap(28.0);
    let short: Vec<&Track> = all.tracks.iter().take(5).collect();
    if all.top.is_some() || !short.is_empty() {
        let mut line = row().w_full().gap(28.0).items_start();
        if let Some(hit) = &all.top {
            line = line.child(top_hit(s, p, hit));
        }
        if !short.is_empty() {
            let mut list = col().grow(1.0).min_w(0.0).gap(10.0).child(section_label("TRACKS", p));
            let mut rows = col().w_full().gap(2.0);
            for (index, track) in short.iter().enumerate() {
                rows = rows.child(short_row(s, p, track, index));
            }
            list = list.child(rows);
            line = line.child(list);
        }
        body = body.child(line);
    }
    if !all.albums.is_empty() {
        body = body.child(album_carousel(s, p, "search-albums", "Albums", &all.albums));
    }
    if !all.artists.is_empty() {
        body = body.child(artist_carousel(s, p, "search-artists", "Artists", &all.artists));
    }
    if !all.playlists.is_empty() {
        body = body.child(playlist_carousel(s, p, "search-playlists", "Playlists", &all.playlists));
    }
    if !all.videos.is_empty() {
        body = body.child(video_carousel(s, p, "search-videos", "Videos", &all.videos));
    }
    body
}

/// The top hit as a large card: cover, kind label, title and subtitle. Albums, artists and
/// playlists navigate to their pages; a track starts playing.
fn top_hit(s: &Ui, p: Palette, hit: &TopHit) -> El {
    match hit {
        TopHit::Album(album) => {
            let id = album.id;
            col()
                .key("top-hit")
                .role("button")
                .label(format!("Open {}", album.title))
                .focusable(true)
                .w(240.0)
                .gap(10.0)
                .on_press(move |s: &mut Ui| s.open_album(id))
                .child(cover_art(s, p, album.cover.as_deref(), Some(228.0)))
                .child(section_label("ALBUM", p))
                .child(text(album.title.clone()).font(H2).color(p.text).truncate())
                .child(text(album.artist.clone()).font(BODY).color(p.dim).truncate())
        }
        TopHit::Artist(artist) => {
            let id = artist.id;
            col()
                .key("top-hit")
                .role("button")
                .label(format!("Open {}", artist.name))
                .focusable(true)
                .w(240.0)
                .gap(10.0)
                .on_press(move |s: &mut Ui| s.open_artist(id))
                .child(round_art(s, p, artist.picture.as_deref(), 228.0))
                .child(section_label("ARTIST", p))
                .child(text(artist.name.clone()).font(H2).color(p.text).truncate())
        }
        TopHit::Playlist(playlist) => {
            let open = playlist.clone();
            let count = track_count(playlist.count);
            col()
                .key("top-hit")
                .role("button")
                .label(format!("Open {}", playlist.title))
                .focusable(true)
                .w(240.0)
                .gap(10.0)
                .on_press(move |s: &mut Ui| s.open_playlist(open.clone()))
                .child(cover_art(s, p, playlist.cover.as_deref(), Some(228.0)))
                .child(section_label("PLAYLIST", p))
                .child(text(playlist.title.clone()).font(H2).color(p.text).truncate())
                .child(text(count).font(BODY).color(p.dim).truncate())
        }
        TopHit::Track(track) => {
            let id = track.id;
            col()
                .key("top-hit")
                .role("button")
                .label(format!("Play {}", track.title))
                .focusable(true)
                .w(240.0)
                .gap(10.0)
                .hover(p.card_hover)
                .on_press(move |s: &mut Ui| {
                    let list = s.results.to_vec();
                    let position = crate::ui::tracks::row_of(&list, id, 0).unwrap_or(0);
                    s.play_tracks(list, position);
                })
                .child(cover_art(s, p, track.cover.as_deref(), Some(228.0)))
                .child(section_label("TRACK", p))
                .child(text(track.title.clone()).font(H2).color(p.text).truncate())
                .child(text(track.artist.clone()).font(BODY).color(p.dim).truncate())
        }
    }
}

/// A top-results track row: cover, title, artist, the play button and the "…" menu. Double-
/// clicking plays the whole result list from here; a right-click opens the menu too.
fn short_row(s: &Ui, p: Palette, track: &Track, index: usize) -> El {
    let id = track.id;
    let current = s.now_playing.as_ref().is_some_and(|playing| playing.id == id);
    let pending = s.pending_play.as_ref().is_some_and(|playing| playing.id == id);
    let menu_key = format!("top-track-menu-{id}");
    let row_menu = TrackMenu {
        track: track.clone(),
        list: Some(s.results.shared().rows),
        index,
        playlist: None,
        queue: false,
    };
    let click_menu = row_menu.clone();
    let click_key = menu_key.clone();
    row()
        .key(format!("top-track-{id}"))
        .w_full()
        .h(56.0)
        .px(8.0)
        .gap(12.0)
        .center_y()
        .radius(4.0)
        .fill(if s.selected == Some(id) { p.card } else { Color::TRANSPARENT })
        .hover(p.card)
        .on_press(move |s: &mut Ui| s.selected = Some(id))
        .on_double_press(move |s: &mut Ui| {
            let list = s.results.to_vec();
            let start = crate::ui::tracks::row_of(&list, id, index).unwrap_or(index);
            s.play_tracks(list, start);
        })
        .on_secondary_press(move |s: &mut Ui, press: rune::PointerPress, _: &mut rune::Tx| {
            s.open_menu(MenuState::track(click_key.clone(), click_menu.clone()).at(press))
        })
        .child(cover_art(s, p, track.cover.as_deref(), Some(48.0)))
        .child(
            col().grow(1.0).min_w(0.0).gap(3.0)
                .child(text(track.title.clone()).font(TRACK).color(p.text).truncate())
                .child(text(track.artist.clone()).font(SMALL).color(p.dim).truncate()),
        )
        .child(play_button(s, p, id, &track.title, current, pending))
        .child(menu_button(p, &menu_key, "Track options", row_menu))
}

/// The suggestion list shown under the search box while it is focused: recent searches when the
/// box is empty, TIDAL suggestions otherwise.
pub(crate) fn suggestions_dropdown(s: &Ui, p: Palette) -> El {
    let recents = s.query.text.trim().is_empty();
    let items = s.search_suggest_list();
    let mut menu = col()
        .key("search-suggestions")
        .role("menu")
        .w(300.0)
        .padding(6.0)
        .radius(10.0)
        .fill(p.card)
        .border(1.0, p.border)
        .on_hover_change(|s: &mut Ui, over: bool, _: &mut rune::Tx| s.suggest_hover = over);
    menu = menu.child(
        row()
            .w_full()
            .h(26.0)
            .px(12.0)
            .gap(8.0)
            .center_y()
            .child(
                text(if recents { "Recent searches" } else { "Suggestions" })
                    .font(CAPS)
                    .color(p.faint),
            )
            .child(spacer())
            .child(if recents {
                el()
                    .key("recent-clear")
                    .role("button")
                    .label("Clear recent searches")
                    .focusable(true)
                    .on_press(|s: &mut Ui| s.clear_recent_searches())
                    .child(text("Clear").font(SMALL).color(p.dim))
            } else {
                el()
            }),
    );
    for (index, value) in items.iter().enumerate() {
        let selected = s.suggest_selected == Some(index);
        let owned = value.clone();
        menu = menu.child(
            row()
                .key(format!("suggestion-{index}"))
                .role("menuitem")
                .label(owned.clone())
                .w_full()
                .h(34.0)
                .px(12.0)
                .gap(10.0)
                .center_y()
                .radius(4.0)
                .fill(if selected { p.card_hover } else { Color::TRANSPARENT })
                .hover(p.card_hover)
                .focusable(true)
                .on_press(move |s: &mut Ui| s.accept_suggestion(owned.clone()))
                .child(icon(&ICON_SEARCH, 14.0).color(p.faint))
                .child(text(value.clone()).font(BODY).color(p.text).grow(1.0).truncate()),
        );
    }
    menu
}



#[cfg(test)]
mod tests {
    use crate::app::{Login, init};

    /// Pressing a row of the recent-searches list moves the focus onto the row, which blurs the
    /// search box; the list must stay up until the press finishes so the click searches again.
    #[test]
    fn clicking_a_recent_search_runs_it_again() {
        let mut s = init();
        s.login = Login::LoggedIn;
        s.recent_searches = vec!["daft punk".to_owned(), "yoasobi".to_owned()];
        s.query.focused = true;
        let mut rt = rune::Runtime::new(s, crate::ui::view, (1280.0, 800.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        let (x, y) = rt.find("suggestion-1").expect("the recent-search rows are shown");
        rt.pointer_move(x, y);
        rt.frame(0.0);
        rt.pointer_down(x, y);
        rt.frame(0.0);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert_eq!(rt.state.query.text, "yoasobi", "the clicked recent search is now the query");
        assert!(rt.state.last_query == "yoasobi" || rt.state.search_state != crate::app::SearchState::Idle,
            "and the search was started");
    }
}
