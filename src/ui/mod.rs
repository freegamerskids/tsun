//! The view layer: the root `view()` composes the sidebar, the main column and the player bar,
//! and dispatches the body to the page modules. Adding a page means a `Page` variant, a module
//! here and one arm in `content`.

use rune::{Align, Cx, El, Side, col, el, row, text};

use crate::app::{Login, Notice, Page, Ui};
use crate::theme::{BODY, Palette, palette};
use crate::widgets::secondary_button;

pub(crate) mod album;
pub(crate) mod artist;
pub(crate) mod cards;
pub(crate) mod collection;
pub(crate) mod context_menu;
pub(crate) mod dialogs;
pub(crate) mod explore;
pub(crate) mod home;
pub(crate) mod login;
pub(crate) mod lyrics;
pub(crate) mod mixes;
pub(crate) mod now_playing;
pub(crate) mod player_bar;
pub(crate) mod playlist;
pub(crate) mod queue;
pub(crate) mod search;
pub(crate) mod sections;
pub(crate) mod local;
pub(crate) mod settings;
pub(crate) mod settings_discord;
pub(crate) mod settings_lastfm;
pub(crate) mod settings_local;
pub(crate) mod sidebar;
pub(crate) mod topbar;
pub(crate) mod tracks;
pub(crate) mod video;

// ---------------------------------------------------------------------------------------------
// View

pub(crate) fn view(s: &Ui, cx: &mut Cx<Ui>) -> El {
    // Cards in carousels and grids follow the main area's laid-out width (read from the last
    // layout; the first frame keeps the previous value). The view re-runs only when the
    // column count changes, not on every resize.
    if let Some(columns) = cx.layout_with("main", |rect| cards::columns_for_width(rect[2])) {
        s.columns.set(columns);
    }
    // The top bar's shape follows the same laid-out width: the search shrinks, then collapses to
    // an icon. A breakpoint read, so a resize only re-runs the view as it crosses one.
    let (logged_in, tray) = (matches!(&s.login, Login::LoggedIn), s.tray_available);
    if let Some(mode) = cx.layout_with("main", move |rect| topbar::BarMode::for_width(rect[2], logged_in, tray)) {
        s.bar_mode.set(mode);
    }
    // Below this window width the sidebar, both side panels and a usable main area do not fit.
    if let Some(narrow) = cx.layout_with("app", |rect| rect[2] < 1280.0) {
        s.narrow_window.set(narrow);
    }
    let p = palette();
    let mut body = row()
        .key("body")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(sidebar::sidebar(s, p))
        .child(main_view(s, p));
    if s.queue_open {
        body = body.child(queue::queue_panel(s, p));
    }
    // The full-screen Now Playing view shows its own lyrics; a second viewport behind it would
    // share the `lyrics-scroll` key and win the auto-scroll.
    if s.lyrics_open && !s.now_playing_open {
        body = body.child(
            col()
                .key("lyrics-dock")
                .w(360.0)
                .h_full()
                .min_h(0.0)
                .px(20.0)
                .py(16.0)
                .border(1.0, p.border)
                .fill(p.bg)
                .child(lyrics::lyrics_panel(s, p)),
        );
    }
    let mut root = col()
        .key("app")
        .w_full()
        .h_full()
        .fill(p.bg)
        // Files dropped from another app play as a one-off queue, like `tsun song.flac`.
        .on_drop_files(|s: &mut Ui, files: &[std::path::PathBuf], _: &mut rune::Tx| {
            s.play_local_paths(files.to_vec())
        })
        .child(body);
    // The footer only exists once the user has chosen a track to play.
    if s.now_playing.is_some() || s.pending_play.is_some() {
        root = root.child(player_bar::player_bar(s, p));
    }
    if s.profile_menu_open && matches!(&s.login, Login::LoggedIn) {
        root = root
            .child(el().key("profile-menu-dismiss").absolute(0.0, 0.0).w_full().h_full()
                .on_press(|s: &mut Ui| s.profile_menu_open = false))
            .child(topbar::profile_menu(s, p).anchored("profile-button", Side::Bottom, Align::End, 8.0));
    }
    if s.search_dropdown_open() {
        root = root.child(
            search::suggestions_dropdown(s, p).anchored("search", Side::Bottom, Align::Start, 8.0),
        );
    }
    if s.collection_sort_open && s.page == Page::Collection {
        root = root
            .child(el().key("collection-sort-dismiss").absolute(0.0, 0.0).w_full().h_full()
                .on_press(|s: &mut Ui| s.collection_sort_open = false))
            .child(collection::sort_menu(s, p).anchored("collection-sort", Side::Bottom, Align::Start, 8.0));
    }
    // The context menu: a scrim that dismisses, the popup anchored to the row or header the
    // press came from, and its "Add to playlist" submenu beside the popup's own item.
    if let Some(menu) = &s.menu {
        root = root
            .child(el().key("context-menu-dismiss").absolute(0.0, 0.0).w_full().h_full()
                .on_press(|s: &mut Ui| s.close_menu()))
            .child(context_menu::menu(s, p).anchored_fit(menu.anchor.clone(), Side::Bottom, Align::End, 4.0));
        if menu.submenu {
            root = root.child(
                context_menu::playlist_picker(s, p)
                    .anchored_fit("menu-add-to-playlist", Side::Right, Align::Start, 2.0),
            );
        }
    }
    // A modal dialog: the scrim dismisses, and the centred card takes its own presses.
    if s.dialog.is_some() {
        root = root
            .child(el().key("dialog-dismiss").absolute(0.0, 0.0).w_full().h_full()
                .fill(rune::Color::BLACK.alpha(0.55))
                .on_press(|s: &mut Ui| s.close_dialog()))
            .child(
                el().key("dialog-layer").absolute(0.0, 0.0).w_full().h_full().center()
                    .child(dialogs::dialog(s, p)),
            );
    }
    // The video player is the topmost surface: it owns the output while up.
    if s.video_active() {
        root = root.child(video::video_overlay(s, p));
    }
    // The full-screen Now Playing view sits above every other surface: it belongs to no page.
    if s.now_playing_open {
        root = root.child(now_playing::now_playing_overlay(s, p));
    }
    // The window has no system title bar: the top bar (which every screen has) moves the window
    // and ends with its buttons. The full-screen views (Now Playing, video) cover the bar and
    // close with Escape or their own button.
    root
}

pub(crate) fn main_view(s: &Ui, p: Palette) -> El {
    let mut main = col()
        .key("main")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .min_h(0.0)
        .child(topbar::top_bar(s, p));
    if let Some(notice) = &s.notice {
        main = main.child(notice_banner(notice, p));
    }
    main.child(content(s, p))
}

pub(crate) fn notice_banner(notice: &Notice, p: Palette) -> El {
    let accent = if notice.error { p.danger } else { p.accent };
    row()
        .w_full()
        .mx(32.0)
        .mt(16.0)
        .px(14.0)
        .py(10.0)
        .gap(12.0)
        .center_y()
        .radius(8.0)
        .fill(if notice.error { p.danger.alpha(0.10) } else { p.card })
        .border(1.0, accent.alpha(0.35))
        .child(
            text(notice.text.clone())
                .font(BODY)
                .color(if notice.error { p.danger } else { p.dim })
                .grow(1.0)
                .wrap_anywhere(),
        )
        .child(secondary_button(p, "Dismiss", true, |s: &mut Ui, _: &mut rune::Tx| {
            s.notice = None
        }))
}

pub(crate) fn content(s: &Ui, p: Palette) -> El {
    // The local library is on disk, so it renders without a TIDAL session too.
    match s.page {
        Page::Local => return local::local_content(s, p),
        Page::LocalAlbum => return local::local_album_content(s, p),
        Page::LocalArtist => return local::local_artist_content(s, p),
        _ => {}
    }
    if matches!(&s.login, Login::LoggedIn) {
        match s.page {
            Page::Home => home::home_content(s, p),
            Page::Search => search::search_content(s, p),
            Page::Explore => explore::explore_content(s, p),
            Page::Collection => collection::collection_content(s, p),
            Page::Playlist => playlist::playlist_content(s, p),
            Page::Mixes => mixes::mixes_content(s, p),
            Page::Mix => mixes::mix_content(s, p),
            Page::Album => album::album_content(s, p),
            Page::Artist => artist::artist_content(s, p),
            Page::Local => local::local_content(s, p),
            Page::LocalAlbum => local::local_album_content(s, p),
            Page::LocalArtist => local::local_artist_content(s, p),
            Page::Settings => settings::settings_content(s, p),
        }
    } else {
        login::sign_in_content(s, p)
    }
}
