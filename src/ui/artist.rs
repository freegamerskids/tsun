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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Login, Page, init};
    use crate::history::Route;
    use crate::tidal::{Album, ArtistPage, Video};

    /// A scratch `XDG_CONFIG_HOME` for the tests that navigate: every route change persists the
    /// settings file, which must never be the user's own. It also holds the environment lock, so
    /// no other test reads the settings while this directory is named.
    struct ConfigHome {
        previous: Option<std::ffi::OsString>,
        dir: std::path::PathBuf,
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigHome {
        fn new(name: &str) -> ConfigHome {
            let turn = crate::settings::lock_config_env();
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("tsun-artist-{}-{name}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: the lock is held for as long as the environment names this directory, and
            // the value is restored on drop.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
            ConfigHome { previous, dir, _turn: turn }
        }
    }

    impl Drop for ConfigHome {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The first element (in tree order) whose key starts with `prefix`: the leftmost card of a
    /// carousel row, since the row's children are its cards.
    fn first_key(rt: &rune::Runtime<Ui>, prefix: &str) -> Option<String> {
        rt.elements()
            .into_iter()
            .find(|element| !element.exiting && element.key.as_deref().is_some_and(|key| key.starts_with(prefix)))
            .and_then(|element| element.key)
    }

    fn album(index: usize) -> Album {
        Album {
            id: 100 + index as u64,
            title: format!("Album {index}"),
            artist: "Artist".to_owned(),
            ..Album::default()
        }
    }

    fn video(index: usize) -> Video {
        Video {
            id: 900 + index as u64,
            title: format!("Video {index}"),
            artist: "Artist".to_owned(),
            ..Video::default()
        }
    }

    /// The artist page as the window draws it: `albums` albums in its first carousel, `videos`
    /// videos in the second, and no top tracks — so both sections are the first thing below the
    /// header and are mounted. The route is in the history, so Back has the artist page to
    /// return to. Callers hold a [`ConfigHome`], since navigating persists the settings.
    fn artist_page(albums: usize, videos: usize) -> rune::Runtime<Ui> {
        let mut s = init();
        s.login = Login::LoggedIn;
        // Reached the way the app reaches it (`navigate` would reload the page's data).
        s.history.push(Route::Artist(5));
        s.page = Page::Artist;
        s.artist_state = SearchState::Done;
        let artist = Artist { id: 5, name: "Artist".to_owned(), picture: None };
        s.artist = Some(artist.clone());
        s.artist_page = Some(ArtistPage {
            artist,
            bio: None,
            top_tracks: Vec::new(),
            top_tracks_total: None,
            albums: (0..albums).map(album).collect(),
            singles: Vec::new(),
            appears_on: Vec::new(),
            videos: (0..videos).map(video).collect(),
            similar: Vec::new(),
        });
        let mut rt = rune::Runtime::new(
            s,
            crate::ui::view,
            (1280.0, 800.0),
            rune::Env::new(true, 1.0),
        );
        for _ in 0..4 {
            rt.frame(0.0);
        }
        rt
    }

    /// Presses the element keyed `key`, then lets the frame land.
    fn press(rt: &mut rune::Runtime<Ui>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element keyed {key}"));
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
    }

    /// The carousel chevrons page the cards: the row a carousel sits in is cached by the page's
    /// version, which carries `carousel_stamp`, so an offset change must rebuild it.
    #[test]
    fn the_album_carousel_chevrons_page_the_cards() {
        // `init` reads the settings file, whose directory the environment names.
        let _config = ConfigHome::new("albums-page");
        let mut rt = artist_page(30, 0);
        let before = first_key(&rt, "album-").expect("the first carousel draws its cards");
        press(&mut rt, "artist-albums-next");
        assert!(
            rt.state.carousel_offsets.get("artist-albums").copied().unwrap_or(0) > 0,
            "the chevron paged the carousel"
        );
        assert_ne!(
            first_key(&rt, "album-"),
            Some(before.clone()),
            "the visible cards changed with the offset"
        );
        press(&mut rt, "artist-albums-prev");
        assert_eq!(first_key(&rt, "album-"), Some(before), "paging back shows the first page again");
    }

    /// The same for the video carousel, which pages the same frame.
    #[test]
    fn the_video_carousel_chevrons_page_the_cards() {
        // `init` reads the settings file, whose directory the environment names.
        let _config = ConfigHome::new("videos-page");
        let mut rt = artist_page(0, 12);
        for _ in 0..6 {
            rt.wheel(600.0, 400.0, -200.0);
            rt.frame(1.0 / 120.0);
        }
        let before = first_key(&rt, "video-thumb-").expect("the carousel draws its videos");
        press(&mut rt, "artist-videos-next");
        assert_ne!(
            first_key(&rt, "video-thumb-"),
            Some(before.clone()),
            "the visible videos changed with the offset"
        );
        press(&mut rt, "artist-videos-prev");
        assert_eq!(first_key(&rt, "video-thumb-"), Some(before), "paging back shows the first page");
    }

    /// "View all" opens the carousel's whole list on its own page: a virtualized grid of the same
    /// cards under the carousel's title, with no chevrons, and Back returns to the artist page.
    #[test]
    fn view_all_opens_the_whole_carousel_as_a_page() {
        // `init` reads the settings file, whose directory the environment names.
        let _config = ConfigHome::new("view-all");
        let mut rt = artist_page(300, 0);
        assert!(rt.find("artist-albums-all").is_some(), "a carousel longer than a page offers View all");
        press(&mut rt, "artist-albums-all");
        assert_eq!(rt.state.page, Page::ViewAll, "the button opens its own page");
        assert!(rt.find("album-100").is_some(), "the first album's card is up");
        assert!(
            rt.find("album-399").is_none(),
            "the last one is not built before it is scrolled to"
        );
        assert!(rt.find("artist-albums-next").is_none(), "the page holds no carousel chevrons");
        // Scrolling reaches the last album of all three hundred: the grid is virtualized, not
        // capped at what a carousel page holds.
        for _ in 0..80 {
            rt.wheel(600.0, 400.0, -900.0);
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.find("album-399").is_some(), "the last album is reachable");
        // Back returns to the artist page (which reloads its data, as the app's Back does)...
        rt.update(|s, _| s.go_back());
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(rt.state.page, Page::Artist, "Back returns to the artist page");
        assert!(
            rt.elements().into_iter().any(|element| !element.exiting && element.text.as_deref() == Some("Artist")),
            "the artist's own header is up"
        );
        // ...and Forward finds the snapshot again: the page is kept in the map for exactly this,
        // and opens at the top.
        rt.update(|s, _| s.go_forward());
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(rt.state.page, Page::ViewAll, "Forward returns to the View all page");
        assert!(
            rt.find("album-100").is_some(),
            "with the snapshot back, opened at the top again"
        );
    }

    /// A carousel whose whole list already fits on one page has nothing more to show, so it
    /// draws no "View all" button.
    #[test]
    fn a_carousel_that_fits_on_one_page_has_no_view_all() {
        // `init` reads the settings file, whose directory the environment names.
        let _config = ConfigHome::new("fits");
        let rt = artist_page(2, 0);
        assert!(rt.find("album-100").is_some(), "the albums are drawn");
        assert!(rt.find("artist-albums-all").is_none(), "with nothing more to show");
    }
}
