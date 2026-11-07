//! Left navigation: the TIDAL mark, the Music/Explore/Collection nav items and the playlist
//! list, which shows for signed-in users only.

use rune::{Color, El, Icon, ScrollSpec, TextStyle, Tx, col, el, icon, row, text};

use crate::app::{Login, Notice, Page, Ui};
use crate::history::Route;
use crate::theme::{
    ICON_COLLECTION, ICON_EXPLORE, ICON_FOLDER, ICON_NOTE, ICON_PLUS, ICON_TIDAL, SIDEBAR_W,
    SMALL, TRACK, Palette,
};
use crate::widgets::{cover_art, track_count};

pub(crate) fn sidebar(s: &Ui, p: Palette) -> El {
    let mut sidebar = col()
        .key("sidebar")
        .w(SIDEBAR_W)
        .h_full()
        .px(16.0)
        .pt(20.0)
        .pb(18.0)
        .gap(14.0)
        .fill(p.sidebar)
        .border(1.0, p.border)
        .child(el().h(24.0).px(8.0).child(icon(&ICON_TIDAL, 24.0).color(p.text)))
        .child(
            col().w_full().gap(2.0)
                .child(nav_item(p, "Music", &ICON_NOTE, Route::Home, matches!(s.page, Page::Home | Page::Mixes | Page::Mix)))
                .child(nav_item(p, "Explore", &ICON_EXPLORE, Route::Explore, s.page == Page::Explore))
                .child(nav_item(p, "Collection", &ICON_COLLECTION, Route::Collection, s.page == Page::Collection))
                .child(local_item(p, matches!(s.page, Page::Local | Page::LocalAlbum | Page::LocalArtist))),
        );
    if matches!(&s.login, Login::LoggedIn) {
        let mut playlists = col().w_full().gap(4.0);
        for playlist in &s.playlists {
            let selected = s.page == Page::Playlist
                && s.playlist.as_ref().is_some_and(|open| open.id == playlist.id);
            let open = playlist.clone();
            playlists = playlists.child(
                row().key(format!("playlist-{}", playlist.id)).role("button")
                    .label(format!("Open {}", playlist.title)).w_full().h(52.0).px(8.0).gap(10.0)
                    .center_y().radius(4.0)
                    .fill(if selected { p.card } else { Color::TRANSPARENT })
                    .hover(p.card).focusable(true)
                    .on_press(move |s: &mut Ui| s.open_playlist(open.clone()))
                    .child(cover_art(s, p, playlist.cover.as_deref(), Some(36.0)))
                    .child(col().grow(1.0).min_w(0.0).gap(2.0)
                        .child(text(playlist.title.clone()).font(TRACK).color(p.text).truncate())
                        .child(text(track_count(playlist.count)).font(SMALL).color(p.dim))),
            );
        }
        sidebar = sidebar
            .child(el().h(1.0).w_full().fill(p.border))
            .child(nav_item(p, "Favorite tracks", &ICON_NOTE, Route::Collection, false))
            .child(
                row().key("playlists-header").w_full().h(28.0).px(8.0).gap(8.0).center_y()
                    .child(text("All playlists").font(SMALL).color(p.faint).grow(1.0))
                    .child(
                        el().key("create-playlist").role("button").label("Create playlist")
                            .size(22.0, 22.0).center().radius(11.0).hover(p.card_hover).focusable(true)
                            .on_press(|s: &mut Ui| s.open_create_playlist(Vec::new()))
                            .child(icon(&ICON_PLUS, 14.0).color(p.dim)),
                    ),
            )
            .child(col().w_full().grow(1.0).min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false }).child(playlists));
    }
    sidebar
}

/// The "Local files" entry. Files are scanned from disk, so it works signed out too.
fn local_item(p: Palette, active: bool) -> El {
    row()
        .key("nav-Local files")
        .role("button")
        .label("Local files")
        .w_full()
        .h(40.0)
        .px(12.0)
        .gap(12.0)
        .center_y()
        .radius(8.0)
        .fill(if active { p.card_hover } else { Color::TRANSPARENT })
        .hover(p.card)
        .focusable(true)
        .on_press(|s: &mut Ui| s.open_local())
        .child(icon(&ICON_FOLDER, 18.0).color(if active { p.text } else { p.dim }))
        .child(text("Local files").font(TextStyle::new(13.0, 600.0)).color(if active { p.text } else { p.dim }))
}

fn nav_item(p: Palette, label: &str, glyph: &'static Icon, route: Route, active: bool) -> El {
    row()
        .key(format!("nav-{label}"))
        .role("button")
        .label(label.to_owned())
        .w_full()
        .h(40.0)
        .px(12.0)
        .gap(12.0)
        .center_y()
        .radius(8.0)
        .fill(if active { p.card_hover } else { Color::TRANSPARENT })
        .hover(p.card)
        .focusable(true)
        .on_press_tx(move |s: &mut Ui, _: &mut Tx| {
            if matches!(&s.login, Login::LoggedIn) {
                let search = matches!(route, Route::Search);
                s.navigate(route.clone());
                if search {
                    s.query.focused = true;
                }
            } else {
                s.notice = Some(Notice::info("Sign in to explore TIDAL."));
            }
        })
        .child(icon(glyph, 18.0).color(if active { p.text } else { p.dim }))
        .child(text(label).font(TextStyle::new(13.0, 600.0)).color(if active { p.text } else { p.dim }))
}
