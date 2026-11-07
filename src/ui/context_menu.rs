//! Anchored context menus: the "…" popup on every track row and catalog header, and its
//! "Add to playlist" submenu.
//!
//! The actions are the official client's and every one of them does something real: they drive
//! the play queue, the favourites, navigation, TIDAL's track radio, the clipboard, the
//! playlist-editing endpoints and the track-info dialog. Rows also open the menu on a
//! right-click (`El::on_secondary_press`).
//!
//! The popup itself is drawn at the root of the view (see [`crate::ui::view`]) and anchored to
//! the key the press came from, exactly as the top bar's profile menu is, so it escapes the
//! page's clipping ancestors.

use std::rc::Rc;

use rune::{El, Icon, Tx, col, el, icon, row, text};

use crate::app::{Notice, Ui};
use crate::theme::{
    BODY, CAPS, ICON_ALBUM, ICON_DOWN, ICON_EDIT, ICON_HEART, ICON_HEART_FILLED, ICON_INFO,
    ICON_LINK, ICON_MORE, ICON_NOTE, ICON_PLAY, ICON_PLUS, ICON_PROFILE, ICON_QUEUE, ICON_RADIO,
    ICON_TRASH, ICON_UP, Palette, SMALL,
};
use crate::tidal::{Album, Artist, FavKind, Mix, Playlist, Track};

/// The open context menu: what it acts on and where it hangs.
#[derive(Clone)]
pub(crate) struct MenuState {
    pub(crate) target: MenuTarget,
    /// Key of the element the popup is anchored to.
    pub(crate) anchor: String,
    /// Whether the "Add to playlist" submenu is showing beside the menu.
    pub(crate) submenu: bool,
    /// Where the pointer was for a right-click: the popup opens there rather than under its
    /// anchor element, which then only names the row the menu belongs to.
    pub(crate) at: Option<(f32, f32)>,
}

/// What an open context menu acts on.
#[derive(Clone)]
pub(crate) enum MenuTarget {
    Track(TrackMenu),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
    Mix(Mix),
}

/// A track row's menu: the track, the table it sits in, and the playlist item it came from.
#[derive(Clone)]
pub(crate) struct TrackMenu {
    pub(crate) track: Track,
    /// The list the row was drawn in, so "Play now" starts that list at the row's own track.
    /// `None` where the row has no surrounding table (queue rows).
    pub(crate) list: Option<Rc<[Track]>>,
    /// The row's position when it was built. [`TrackMenu::resolve`] re-finds it by id when the
    /// menu opens, so a virtual row that outlives an insert or a reorder still acts on its own
    /// track.
    pub(crate) index: usize,
    /// `(playlist uuid, playlist item index)` when the row is on an owned playlist page, so
    /// the menu can offer "Remove from this playlist" and the move items.
    pub(crate) playlist: Option<(String, u64)>,
    /// True for a queue-panel row, so the menu can offer "Move up"/"Move down" into the queue.
    pub(crate) queue: bool,
}

impl TrackMenu {
    /// Re-finds the row's place in the list it belongs to, by track id, and the playlist item it
    /// came from. Called when the menu opens, so a stale index can never address the wrong track.
    pub(crate) fn resolve(&mut self, s: &Ui) {
        if self.queue {
            if let Some(index) = crate::ui::tracks::row_of(s.queue.tracks(), self.track.id, self.index) {
                self.index = index;
            }
        } else if let Some(list) = &self.list {
            if let Some(index) = crate::ui::tracks::row_of(list, self.track.id, self.index) {
                self.index = index;
            }
        }
        self.playlist = playlist_item_for(s, self.track.id, self.index);
    }
}

impl MenuState {
    /// A menu for a track row, anchored to `anchor`.
    pub(crate) fn track(anchor: impl Into<String>, menu: TrackMenu) -> Self {
        MenuState { target: MenuTarget::Track(menu), anchor: anchor.into(), submenu: false, at: None }
    }

    /// A menu for an album, anchored to `anchor`.
    pub(crate) fn album(anchor: impl Into<String>, album: Album) -> Self {
        MenuState { target: MenuTarget::Album(album), anchor: anchor.into(), submenu: false, at: None }
    }

    /// A menu for an artist, anchored to `anchor`.
    pub(crate) fn artist(anchor: impl Into<String>, artist: Artist) -> Self {
        MenuState { target: MenuTarget::Artist(artist), anchor: anchor.into(), submenu: false, at: None }
    }

    /// A menu for a playlist, anchored to `anchor`.
    pub(crate) fn playlist(anchor: impl Into<String>, playlist: Playlist) -> Self {
        MenuState { target: MenuTarget::Playlist(playlist), anchor: anchor.into(), submenu: false, at: None }
    }

    /// A menu for a custom mix, anchored to `anchor`.
    pub(crate) fn mix(anchor: impl Into<String>, mix: Mix) -> Self {
        MenuState { target: MenuTarget::Mix(mix), anchor: anchor.into(), submenu: false, at: None }
    }

    /// Opens the menu at the pointer (a right-click) instead of under the element it is
    /// anchored to. `press` is in window points, as `on_secondary_press` gives it.
    pub(crate) fn at(mut self, press: rune::PointerPress) -> Self {
        self.at = Some((press.x, press.y));
        self
    }
}

/// The width of the popup, and of its submenu.
const MENU_W: f32 = 250.0;

/// The track row's "…" button: it opens `menu`'s popup anchored to itself.
pub(crate) fn menu_button(p: Palette, key: &str, label: &str, menu: TrackMenu) -> El {
    let anchor = key.to_owned();
    el()
        .key(key.to_owned())
        .role("button")
        .label(label.to_owned())
        .size(28.0, 28.0)
        .radius(14.0)
        .center()
        .fill(rune::Color::TRANSPARENT)
        .hover(p.card_hover)
        .focusable(true)
        .on_press(move |s: &mut Ui| {
            let mut menu = menu.clone();
            menu.resolve(s);
            s.open_menu(MenuState::track(anchor.clone(), menu));
        })
        .child(icon(&ICON_MORE, 16.0).color(p.dim))
}

/// The "…" button a catalog header uses (album, artist, playlist): it opens `menu`'s popup
/// anchored to itself.
pub(crate) fn header_menu_button(p: Palette, key: &str, label: &str, menu: MenuState) -> El {
    let template = menu;
    el()
        .key(key.to_owned())
        .role("button")
        .label(label.to_owned())
        .size(40.0, 40.0)
        .radius(20.0)
        .center()
        .fill(p.card)
        .hover(p.card_hover)
        .focusable(true)
        .on_press(move |s: &mut Ui| {
            let anchor = template.anchor.clone();
            s.open_menu(MenuState { target: template.target.clone(), anchor, submenu: false, at: None });
        })
        .child(icon(&ICON_MORE, 18.0).color(p.text))
}

/// `(playlist uuid, playlist item index)` when `id` is on one of the user's own playlist pages,
/// so its row's menu offers "Remove from this playlist" and the move items. The item index is
/// the track's place in the playlist, found by id, so a virtual row can never address another
/// item.
pub(crate) fn playlist_item_for(s: &Ui, id: u64, near: usize) -> Option<(String, u64)> {
    if s.page != crate::app::Page::Playlist {
        return None;
    }
    let playlist = s.playlist.as_ref().filter(|playlist| playlist.owned)?;
    let index = crate::ui::tracks::row_of(s.playlist_tracks.items(), id, near)?;
    Some((playlist.id.clone(), index as u64))
}

/// The track ids the open menu's "Add to playlist" would add: one for a row, the whole album or
/// playlist for a header menu.
pub(crate) fn menu_track_ids(s: &Ui) -> Vec<u64> {
    match s.menu.as_ref().map(|menu| &menu.target) {
        Some(MenuTarget::Track(menu)) if !crate::local::is_local_id(menu.track.id) => {
            vec![menu.track.id]
        }
        Some(MenuTarget::Track(_)) => Vec::new(),
        Some(MenuTarget::Album(album)) => album_tracks(s, album.id).iter().map(|track| track.id).collect(),
        Some(MenuTarget::Playlist(playlist)) => playlist_tracks(s, playlist.id.as_str())
            .iter()
            .map(|track| track.id)
            .collect(),
        Some(MenuTarget::Mix(mix)) => mix_tracks(s, mix.id.as_str()).iter().map(|track| track.id).collect(),
        _ => Vec::new(),
    }
}

/// The tracks of the mix page being viewed, or an empty list when it is not the open page.
fn mix_tracks(s: &Ui, id: &str) -> Vec<Track> {
    match &s.mix {
        Some(mix) if mix.id == id => s.mix_tracks.to_vec(),
        _ => Vec::new(),
    }
}

/// The tracks of the album page being viewed, or an empty list when it is not the open page
/// (menus only ever open from the page that holds the data).
fn album_tracks(s: &Ui, id: u64) -> Vec<Track> {
    match &s.album {
        Some(album) if album.id == id => s.album_tracks.to_vec(),
        _ => Vec::new(),
    }
}

/// The tracks of the playlist being viewed, or an empty list when it is not the open page.
fn playlist_tracks(s: &Ui, id: &str) -> Vec<Track> {
    match &s.playlist {
        Some(playlist) if playlist.id == id => s.playlist_tracks.to_vec(),
        _ => Vec::new(),
    }
}

/// The open context menu, ready to be `.anchored(..)` at the root of the view.
pub(crate) fn menu(s: &Ui, p: Palette) -> El {
    let Some(state) = &s.menu else { return el() };
    let list = match &state.target {
        MenuTarget::Track(menu) => track_items(s, p, items(), menu),
        MenuTarget::Album(album) => album_items(s, p, items(), album),
        MenuTarget::Artist(artist) => artist_items(p, items(), artist),
        MenuTarget::Playlist(playlist) => playlist_items(s, p, items(), playlist),
        MenuTarget::Mix(mix) => mix_items(s, p, items(), mix),
    };
    shell(p, "context-menu").child(scroller(list))
}

/// The "Add to playlist" submenu: the user's own, editable playlists plus "New playlist…".
pub(crate) fn playlist_picker(s: &Ui, p: Palette) -> El {
    let ids = menu_track_ids(s);
    let mut list = col().key("picker-list").w_full().gap(1.0);
    let owned: Vec<&Playlist> = s.playlists.iter().filter(|playlist| playlist.owned).collect();
    if owned.is_empty() {
        list = list.child(
            text("You have no playlists yet.").font(SMALL).color(p.faint).px(10.0).py(6.0).w_full(),
        );
    }
    for playlist in owned {
        let id = playlist.id.clone();
        let target = ids.clone();
        list = list.child(item(
            p,
            &format!("picker-playlist-{}", playlist.id),
            &playlist.title,
            Some(&ICON_NOTE),
            true,
            move |s: &mut Ui, _: &mut Tx| s.add_to_playlist(id.clone(), target.clone()),
        ));
    }
    shell(p, "playlist-picker")
        .child(text("Add to playlist").font(CAPS).color(p.faint).px(10.0).pt(6.0).pb(2.0).w_full())
        .child(item(p, "menu-new-playlist", "New playlist…", Some(&ICON_PLUS), true, move |s: &mut Ui, _| {
            let ids = menu_track_ids(s);
            s.open_create_playlist(ids);
        }))
        .child(el().w_full().h(1.0).my(3.0).fill(p.border))
        .child(scroller(list))
}

/// The popup body: the visible surface that absorbs presses on its own padding, so the scrim
/// under it does not see them.
fn shell(p: Palette, key: &str) -> El {
    col()
        .key(key.to_owned())
        .role("menu")
        .w(MENU_W)
        .padding(6.0)
        .gap(1.0)
        .radius(8.0)
        .fill(p.card)
        .border(1.0, p.border)
        .on_press(|_: &mut Ui| {})
}

/// A shrinking scroll viewport around a popup's items. `El::anchored_fit` clamps the popup to
/// the room the window has for it, and this is what gives way when there is not enough: a long
/// menu scrolls instead of running off the bottom of the window.
fn scroller(child: El) -> El {
    col()
        .w_full()
        .shrink()
        .min_h(0.0)
        .scroll(rune::ScrollSpec { stick_bottom: false })
        .child(child)
}

/// The column a header menu's items are appended to.
fn items() -> El {
    col().w_full().gap(1.0)
}

/// One menu row.
fn item(
    p: Palette,
    key: &str,
    label: &str,
    glyph: Option<&'static Icon>,
    enabled: bool,
    on: impl Fn(&mut Ui, &mut Tx) + 'static,
) -> El {
    let mut row = row()
        .key(key.to_owned())
        .role("menuitem")
        .label(label.to_owned())
        .w_full()
        .h(28.0)
        .px(10.0)
        .gap(10.0)
        .center_y()
        .radius(4.0)
        .hover(if enabled { p.card_hover } else { p.card })
        .disabled(!enabled);
    if enabled {
        row = row.focusable(true).on_press_tx(on);
    }
    if let Some(glyph) = glyph {
        row = row.child(icon(glyph, 16.0).color(if enabled { p.dim } else { p.faint }));
    }
    row.child(
        text(label.to_owned())
            .font(BODY)
            .color(if enabled { p.text } else { p.faint })
            .grow(1.0)
            .truncate(),
    )
}

/// A local file's menu: queue actions, reveal in the file manager and copy the path. TIDAL-only
/// actions (favourite, radio, go to artist/album, TIDAL link, playlists, credits) are absent.
fn local_track_items(p: Palette, popup: El, menu: &TrackMenu) -> El {
    let id = menu.track.id;
    let play = menu.track.clone();
    let mut popup = popup;
    match &menu.list {
        Some(list) => {
            let list = list.clone();
            let index = menu.index;
            popup = popup.child(item(p, "menu-play-now", "Play now", Some(&ICON_PLAY), true, move |s: &mut Ui, _: &mut Tx| {
                s.close_menu();
                s.play_tracks((*list).to_vec(), index);
            }));
        }
        None => {
            popup = popup.child(item(p, "menu-play-now", "Play now", Some(&ICON_PLAY), true, move |s: &mut Ui, _| {
                s.close_menu();
                s.play_tracks(vec![play.clone()], 0);
            }));
        }
    }
    let next = menu.track.clone();
    let queue = menu.track.clone();
    popup
        .child(item(p, "menu-play-next", "Play next", Some(&ICON_QUEUE), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.queue_next(next.clone());
        }))
        .child(item(p, "menu-add-queue", "Add to queue", Some(&ICON_QUEUE), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.queue_add(queue.clone());
        }))
        .child(el().w_full().h(1.0).my(3.0).fill(p.border))
        .child(item(p, "menu-show-in-files", "Show in file manager", Some(&ICON_ALBUM), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.show_local_in_file_manager(id);
        }))
        .child(item(p, "menu-copy-path", "Copy file path", Some(&ICON_LINK), true, move |s: &mut Ui, tx: &mut Tx| {
            if let Some(track) = s.local_track(id) {
                tx.copy(track.path.display().to_string());
                s.notice = Some(Notice::info("File path copied to the clipboard"));
            }
            s.close_menu();
        }))
}

/// A track row's items.
fn track_items(s: &Ui, p: Palette, popup: El, menu: &TrackMenu) -> El {
    let track = &menu.track;
    let id = track.id;
    // Local files get their own, smaller menu: no favourites, radio, TIDAL links or playlist
    // endpoints (none of those can act on a file), but the queue actions and the desktop reveal.
    if crate::local::is_local_id(id) {
        return local_track_items(p, popup, menu);
    }
    let mut popup = popup;

    // "Play now" starts the table the row was clicked in, so next/previous walk it; a row with
    // no table of its own (the queue panel) just plays the one track.
    let play = track.clone();
    match &menu.list {
        Some(list) => {
            let list = list.clone();
            let index = menu.index;
            popup = popup.child(item(p, "menu-play-now", "Play now", Some(&ICON_PLAY), true, move |s: &mut Ui, _: &mut Tx| {
                s.close_menu();
                s.play_tracks((*list).to_vec(), index);
            }));
        }
        None => {
            popup = popup.child(item(p, "menu-play-now", "Play now", Some(&ICON_PLAY), true, move |s: &mut Ui, _| {
                s.close_menu();
                s.play_tracks(vec![play.clone()], 0);
            }));
        }
    }

    let next = track.clone();
    let queue = track.clone();
    popup = popup
        .child(item(p, "menu-play-next", "Play next", Some(&ICON_QUEUE), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.queue_next(next.clone());
        }))
        .child(item(p, "menu-add-queue", "Add to queue", Some(&ICON_QUEUE), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.queue_add(queue.clone());
        }))
        .child(item(p, "menu-add-to-playlist", "Add to playlist", Some(&ICON_PLUS), true, |s: &mut Ui, _: &mut Tx| {
            s.open_playlist_submenu();
        }));

    let favourite = s.is_favorite(FavKind::Track, &id.to_string());
    let fav_id = id.to_string();
    popup = popup.child(item(
        p,
        "menu-favourite",
        if favourite { "Remove from My Collection" } else { "Add to My Collection" },
        Some(if favourite { &ICON_HEART_FILLED } else { &ICON_HEART }),
        true,
        move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.toggle_favorite(FavKind::Track, &fav_id);
        },
    ));

    let go_artist = track.artist_id;
    popup = popup.child(item(
        p,
        "menu-go-artist",
        "Go to artist",
        Some(&ICON_PROFILE),
        go_artist.is_some(),
        move |s: &mut Ui, _: &mut Tx| {
            if let Some(artist_id) = go_artist {
                s.close_menu();
                s.open_artist(artist_id);
            }
        },
    ));
    let go_album = track.album_id;
    popup = popup.child(item(
        p,
        "menu-go-album",
        "Go to album",
        Some(&ICON_ALBUM),
        go_album.is_some(),
        move |s: &mut Ui, _: &mut Tx| {
            if let Some(album_id) = go_album {
                s.close_menu();
                s.open_album(album_id);
            }
        },
    ));

    popup = popup
        .child(el().w_full().h(1.0).my(3.0).fill(p.border))
        .child(item(p, "menu-track-radio", "Track radio", Some(&ICON_RADIO), true, move |s: &mut Ui, _: &mut Tx| {
            s.play_track_radio(id);
        }))
        .child(item(p, "menu-copy-link", "Copy track link", Some(&ICON_LINK), true, move |s: &mut Ui, tx: &mut Tx| {
            tx.copy(track_url(id));
            s.notice = Some(Notice::info("Track link copied to the clipboard"));
            s.close_menu();
        }));
    let info = track.clone();
    popup = popup.child(item(p, "menu-track-info", "Track info", Some(&ICON_INFO), true, move |s: &mut Ui, _: &mut Tx| {
        s.open_track_info(info.clone());
    }));

    if let Some((playlist_id, item_index)) = &menu.playlist {
        let playlist_id = playlist_id.clone();
        let item_index = *item_index;
        // Reordering is only offered on the user's own playlists, which are the only ones TIDAL
        // lets them edit, and only where there is somewhere to move to.
        let count = playlist_tracks(s, &playlist_id).len() as u64;
        let remove_id = playlist_id.clone();
        popup = popup
            .child(el().w_full().h(1.0).my(3.0).fill(p.border))
            .child(item(p, "menu-remove-track", "Remove from this playlist", Some(&ICON_TRASH), true, move |s: &mut Ui, _: &mut Tx| {
                s.remove_from_playlist(remove_id.clone(), item_index);
            }));
        if count > 1 {
            let id_up = playlist_id.clone();
            let id_down = playlist_id;
            popup = popup
                .child(item(p, "menu-move-up", "Move up", Some(&ICON_UP), item_index > 0, move |s: &mut Ui, _: &mut Tx| {
                    s.move_playlist_item(id_up.clone(), item_index, item_index - 1);
                }))
                .child(item(p, "menu-move-down", "Move down", Some(&ICON_DOWN), item_index + 1 < count, move |s: &mut Ui, _: &mut Tx| {
                    s.move_playlist_item(id_down.clone(), item_index, item_index + 1);
                }));
        }
    }
    // Queue rows reorder the play queue itself, purely locally.
    if menu.queue {
        let count = s.queue.tracks().len();
        let from = menu.index;
        popup = popup
            .child(el().w_full().h(1.0).my(3.0).fill(p.border))
            .child(item(p, "menu-move-up", "Move up", Some(&ICON_UP), from > 0, move |s: &mut Ui, _: &mut Tx| {
                s.move_queue_item(from, from - 1);
            }))
            .child(item(p, "menu-move-down", "Move down", Some(&ICON_DOWN), from + 1 < count, move |s: &mut Ui, _: &mut Tx| {
                s.move_queue_item(from, from + 1);
            }));
    }
    popup
}

/// An album header's items.
fn album_items(s: &Ui, p: Palette, mut popup: El, album: &Album) -> El {
    let id = album.id;
    let tracks = album_tracks(s, id);
    let has_tracks = !tracks.is_empty();
    let next = tracks.clone();
    let add = tracks.clone();
    let artist = album.artist_id.map(|artist_id| Artist { id: artist_id, name: album.artist.clone(), picture: None });

    popup = popup
        .child(item(p, "menu-play-next", "Play next", Some(&ICON_QUEUE), has_tracks, move |s: &mut Ui, _: &mut Tx| {
            s.queue_next_list(next.clone());
        }))
        .child(item(p, "menu-add-queue", "Add to queue", Some(&ICON_QUEUE), has_tracks, move |s: &mut Ui, _: &mut Tx| {
            s.queue_add_list(add.clone());
        }))
        .child(item(p, "menu-add-to-playlist", "Add to playlist", Some(&ICON_PLUS), has_tracks, |s: &mut Ui, _: &mut Tx| {
            s.open_playlist_submenu();
        }))
        .child(el().w_full().h(1.0).my(3.0).fill(p.border))
        .child(item(p, "menu-copy-link", "Copy album link", Some(&ICON_LINK), true, move |s: &mut Ui, tx: &mut Tx| {
            tx.copy(format!("https://tidal.com/browse/album/{id}"));
            s.notice = Some(Notice::info("Album link copied to the clipboard"));
            s.close_menu();
        }));
    match artist {
        Some(artist) => popup.child(item(p, "menu-go-artist", "Go to artist", Some(&ICON_PROFILE), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.open_artist_with(artist.clone());
        })),
        None => popup,
    }
}

/// An artist header's items.
fn artist_items(p: Palette, popup: El, artist: &Artist) -> El {
    let id = artist.id;
    popup
        .child(item(p, "menu-copy-link", "Copy artist link", Some(&ICON_LINK), true, move |s: &mut Ui, tx: &mut Tx| {
            tx.copy(format!("https://tidal.com/browse/artist/{id}"));
            s.notice = Some(Notice::info("Artist link copied to the clipboard"));
            s.close_menu();
        }))
        .child(item(p, "menu-artist-radio", "Radio", Some(&ICON_RADIO), true, move |s: &mut Ui, _: &mut Tx| {
            s.close_menu();
            s.play_artist_radio(id);
        }))
}

/// A playlist header's items. Rename and delete only appear for the user's own playlists, which
/// are the only ones TIDAL lets them edit.
fn playlist_items(s: &Ui, p: Palette, mut popup: El, playlist: &Playlist) -> El {
    let tracks = playlist_tracks(s, &playlist.id);
    let has_tracks = !tracks.is_empty();
    let next = tracks.clone();
    let add = tracks.clone();
    let id = playlist.id.clone();

    popup = popup
        .child(item(p, "menu-play-next", "Play next", Some(&ICON_QUEUE), has_tracks, move |s: &mut Ui, _: &mut Tx| {
            s.queue_next_list(next.clone());
        }))
        .child(item(p, "menu-add-queue", "Add to queue", Some(&ICON_QUEUE), has_tracks, move |s: &mut Ui, _: &mut Tx| {
            s.queue_add_list(add.clone());
        }))
        .child(item(p, "menu-add-to-playlist", "Add to playlist", Some(&ICON_PLUS), has_tracks, |s: &mut Ui, _: &mut Tx| {
            s.open_playlist_submenu();
        }))
        .child(el().w_full().h(1.0).my(3.0).fill(p.border))
        .child(item(p, "menu-copy-link", "Copy playlist link", Some(&ICON_LINK), true, move |s: &mut Ui, tx: &mut Tx| {
            tx.copy(format!("https://tidal.com/browse/playlist/{id}"));
            s.notice = Some(Notice::info("Playlist link copied to the clipboard"));
            s.close_menu();
        }));

    if playlist.owned {
        let edit = playlist.clone();
        let delete = playlist.clone();
        popup = popup
            .child(item(p, "menu-edit-playlist", "Edit playlist", Some(&ICON_EDIT), true, move |s: &mut Ui, _: &mut Tx| {
                s.open_edit_playlist(edit.clone());
            }))
            .child(item(p, "menu-delete-playlist", "Delete playlist", Some(&ICON_TRASH), true, move |s: &mut Ui, _: &mut Tx| {
                s.open_delete_confirm(delete.clone());
            }));
    }
    popup
}

/// A mix header's items: a mix is TIDAL's own, so it can be queued and added to a playlist but
/// not edited or linked.
fn mix_items(s: &Ui, p: Palette, mut popup: El, mix: &Mix) -> El {
    let tracks = mix_tracks(s, &mix.id);
    let has_tracks = !tracks.is_empty();
    let next = tracks.clone();
    let add = tracks;
    popup = popup
        .child(item(p, "menu-play-next", "Play next", Some(&ICON_QUEUE), has_tracks, move |s: &mut Ui, _: &mut Tx| {
            s.queue_next_list(next.clone());
        }))
        .child(item(p, "menu-add-queue", "Add to queue", Some(&ICON_QUEUE), has_tracks, move |s: &mut Ui, _: &mut Tx| {
            s.queue_add_list(add.clone());
        }))
        .child(item(p, "menu-add-to-playlist", "Add to playlist", Some(&ICON_PLUS), has_tracks, |s: &mut Ui, _: &mut Tx| {
            s.open_playlist_submenu();
        }));
    popup
}

/// The public TIDAL URL for a track.
fn track_url(id: u64) -> String {
    format!("https://tidal.com/browse/track/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Login, init};
    use crate::tidal::Track;

    #[test]
    fn track_links_point_at_tidals_web_player() {
        assert_eq!(track_url(62300893), "https://tidal.com/browse/track/62300893");
    }

    /// A right-click menu opens at the pointer; a "…" button's menu stays under the button.
    #[test]
    fn a_right_click_menu_opens_at_the_pointer() {
        let _env = crate::settings::lock_config_env();
        let track = Track { id: 7, title: "Song".to_owned(), ..Track::default() };
        let menu = TrackMenu { track, list: None, index: 0, playlist: None, queue: false };
        let popup_origin = |at: Option<(f32, f32)>| {
            let mut s = init();
            s.login = Login::LoggedIn;
            let mut state = MenuState::track("search", menu.clone());
            if let Some((x, y)) = at {
                state = state.at(rune::PointerPress { x, y });
            }
            s.menu = Some(state);
            let mut rt = rune::Runtime::new(s, crate::ui::view, (1280.0, 800.0), rune::Env::new(true, 1.0));
            rt.frame(0.0);
            rt.frame(0.0);
            let rect = rt
                .elements()
                .into_iter()
                .find(|element| element.key.as_deref() == Some("context-menu"))
                .expect("the popup is up")
                .rect;
            (rect[0], rect[1])
        };
        let (x, y) = popup_origin(Some((520.0, 300.0)));
        assert!((x - 520.0).abs() < 8.0 && (y - 304.0).abs() < 8.0, "popup at ({x}, {y}), pointer at (520, 300)");
        // Without a point it hangs under the anchor element instead (here the search box, near the top).
        let (_, anchored_y) = popup_origin(None);
        assert!(anchored_y < 200.0, "an anchored popup stays under its element: y = {anchored_y}");
    }
}
