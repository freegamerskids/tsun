//! Generic renderers for TIDAL card sections: [`Card`] lists laid out through the shared
//! carousel and grid builders in [`crate::ui::cards`]. Explore, the search result tabs and the
//! home feed all build from these, so a new TIDAL page module only has to hand over its parsed
//! [`Section`]s.

use std::rc::Rc;

use rune::{El, col};

use crate::app::Ui;
use crate::theme::Palette;
use crate::tidal::{Card, Section, Track};
use crate::ui::cards;

/// A titled carousel of mixed cards. Track cards play the section's whole track list, so
/// next/previous follow it; album, artist, playlist and mix cards navigate on their own. The
/// carousel's "View all" keeps the list and its tracks in the snapshot, so the grid's track
/// cards play through the same list.
pub(crate) fn carousel(s: &Ui, p: Palette, key: &str, title: &str, items: &[Card]) -> El {
    let tracks = Rc::new(section_tracks(items));
    let shown = tracks.clone();
    cards::carousel_rows(
        s,
        p,
        key,
        title,
        items.len(),
        move || cards::ViewAll::cards(title, items, shown),
        move |index| card_view(s, p, &items[index], &tracks, index),
    )
}

/// Every [`Card::Track`] of `items`, in order, so a track card can start playback of the whole
/// section with next/previous following it.
pub(crate) fn section_tracks(items: &[Card]) -> Vec<Track> {
    items.iter().filter_map(|card| match card {
        Card::Track(track) => Some(track.clone()),
        _ => None,
    }).collect()
}

/// One card. Albums, artists, playlists and mixes carry their own navigation; tracks play
/// through the section list `tracks`. The "View all" grid draws the same cards.
pub(crate) fn card_view(s: &Ui, p: Palette, card: &Card, tracks: &Rc<Vec<Track>>, index: usize) -> El {
    match card {
        Card::Album(album) => cards::album_card(s, p, album),
        Card::Artist(artist) => cards::artist_card(s, p, artist),
        Card::Playlist(playlist) => cards::playlist_card(s, p, playlist),
        Card::Mix(mix) => cards::mix_card(s, p, mix),
        Card::Track(track) => {
            let position = index.min(tracks.len().saturating_sub(1));
            let list = tracks.clone();
            cards::track_card(s, p, track, move |s: &mut Ui| {
                let list = (*list).clone();
                s.play_tracks(list, position);
            })
        }
    }
}

/// A row of [`Section`] carousels, keyed `{prefix}-{index}` so their paging offsets stay
/// independent. Empty sections are skipped.
pub(crate) fn sections_view(s: &Ui, p: Palette, prefix: &str, sections: &[Section]) -> El {
    sections_impl(s, p, prefix, sections, false)
}

/// Like [`sections_view`], but skips sections that carry only tracks: the home feed's album,
/// playlist and mix rows, which the track rows above them already cover.
pub(crate) fn catalogue_sections_view(s: &Ui, p: Palette, prefix: &str, sections: &[Section]) -> El {
    sections_impl(s, p, prefix, sections, true)
}

fn sections_impl(s: &Ui, p: Palette, prefix: &str, sections: &[Section], catalogue_only: bool) -> El {
    let mut body = col().w_full().gap(28.0);
    for (index, section) in sections.iter().enumerate() {
        if section.items.is_empty() {
            continue;
        }
        if catalogue_only && section.items.iter().all(|card| matches!(card, Card::Track(_))) {
            continue;
        }
        let key = format!("{prefix}-{index}");
        let title = if section.title.trim().is_empty() { "More to explore" } else { section.title.trim() };
        body = body.child(carousel(s, p, &key, title, &section.items));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Login, Page, SearchState, Ui, init};
    use crate::history::Route;
    use crate::tidal::{Album, Section, Track};

    /// A scratch `XDG_CONFIG_HOME`: these tests navigate, and every route change persists the
    /// settings file, which must never be the user's own.
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
                .join(format!("tsun-sections-{}-{name}-{unique}", std::process::id()));
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

    fn album(index: usize) -> Album {
        Album { id: 300 + index as u64, title: format!("Album {index}"), ..Album::default() }
    }

    fn track(index: usize) -> Track {
        Track {
            id: 700 + index as u64,
            title: format!("Track {index}"),
            artist: "Artist".to_owned(),
            album: "Album 0".to_owned(),
            ..Track::default()
        }
    }

    /// The Explore page with one mixed section: three albums, then the section's two tracks.
    fn explore_page() -> rune::Runtime<Ui> {
        let mut s = init();
        s.login = Login::LoggedIn;
        s.history.push(Route::Explore);
        s.page = Page::Explore;
        s.explore_state = SearchState::Done;
        s.explore = vec![Section {
            title: "Recommended".to_owned(),
            items: vec![
                Card::Album(album(0)),
                Card::Album(album(1)),
                Card::Album(album(2)),
                Card::Track(track(1)),
                Card::Track(track(2)),
            ],
        }];
        let mut rt = rune::Runtime::new(s, crate::ui::view, (1280.0, 800.0), rune::Env::new(true, 1.0));
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

    /// A generic section's "View all" opens its cards as a grid under the section's title, and a
    /// track card there plays the section's whole track list, so next/previous follow the
    /// section exactly as they do in its carousel.
    #[test]
    fn a_section_view_all_grid_plays_the_sections_track_list() {
        let _config = ConfigHome::new("section-grid");
        let mut rt = explore_page();
        assert!(rt.find("explore-0-all").is_some(), "the section is longer than a page");
        press(&mut rt, "explore-0-all");
        assert_eq!(rt.state.page, Page::ViewAll, "the section opens as a page");
        assert!(
            rt.elements()
                .into_iter()
                .any(|element| !element.exiting && element.text.as_deref() == Some("Recommended")),
            "the page is headed by the section's own title"
        );
        assert!(rt.find("album-300").is_some(), "the section's albums are there");

        press(&mut rt, "track-702");
        assert_eq!(rt.state.queue.current_id(), Some(702), "the pressed track plays");
        assert_eq!(
            rt.state.queue.tracks().len(),
            2,
            "and the queue is the section's own track list"
        );
    }

    /// A card in the grid keeps the behaviour it had in the carousel: an album card opens the
    /// album's page.
    #[test]
    fn a_card_in_the_view_all_grid_still_navigates() {
        let _config = ConfigHome::new("section-nav");
        let mut rt = explore_page();
        press(&mut rt, "explore-0-all");
        press(&mut rt, "album-301");
        assert_eq!(rt.state.page, Page::Album, "the album card opens its album");
        assert_eq!(
            rt.state.album.as_ref().map(|album| album.id),
            Some(301),
            "and names the album it drew"
        );
    }
}
