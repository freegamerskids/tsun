//! Home ("For you"): shortcuts into the collection, the custom-mix gallery when available and
//! the TIDAL recommendations below.

use rune::{Color, El, TextStyle, Tx, col, el, row, spacer, text};

use crate::app::{HomeTab, SearchState, Ui};
use crate::theme::{BODY, H1, H2, SMALL, TRACK, Palette};
use crate::tidal::{FavKind, Track};
use crate::ui::cards::{heart_button, track_card};
use crate::ui::context_menu::{MenuState, TrackMenu, menu_button};
use crate::ui::mixes::mix_gallery;
use crate::ui::sections::{catalogue_sections_view, sections_view};
use crate::widgets::{cover_art, primary_button, secondary_button};

// The home screen uses the user's actual saved tracks; artwork and titles come
// from TIDAL rather than placeholder recommendations.
pub(crate) fn home_content(s: &Ui, p: Palette) -> El {
    let mut body = col().w_full().px(48.0).pt(6.0).pb(48.0).gap(28.0)
        .child(home_tabs(s, p));
    match s.home_tab {
        HomeTab::ForYou => body = body.child(for_you(s, p)),
        HomeTab::NewReleases if s.new_releases.is_empty() => {
            body = body.child(text("Loading new releases…").font(BODY).color(p.dim));
        }
        HomeTab::NewReleases => {
            body = body.child(sections_view(s, p, "home-new-releases", &s.new_releases));
        }
    }
    col().key("home-content").w_full().grow(1.0).min_h(0.0)
        .scroll(rune::ScrollSpec { stick_bottom: false }).child(body)
}

fn home_tabs(s: &Ui, p: Palette) -> El {
    row().key("home-tabs").w_full().gap(18.0)
        .child(home_tab(s, p, HomeTab::ForYou, "For you", "home-tab-for-you"))
        .child(home_tab(s, p, HomeTab::NewReleases, "New releases", "home-tab-new-releases"))
}

/// The "Recently played" row: what this app has actually played, newest first, with a Clear
/// action. Only drawn when the local history has something in it.
fn recently_played(s: &Ui, p: Palette) -> El {
    // The same cached list the Collection's History tab uses, so a commit never rebuilds it.
    let list = s.history_rows.get(s.data_rev, || s.history_tracks()).rows;
    // One card per column, like every other card row, padded with empty slots: the cards grow
    // to share the row, so without the padding a single track would fill the whole width.
    // The Collection's History tab lists everything beyond what fits here.
    let columns = crate::ui::cards::columns(s);
    let mut cards = row().w_full().gap(16.0).items_start();
    let mut shown = 0;
    for (index, track) in list.iter().take(columns).enumerate() {
        let play_list = list.clone();
        cards = cards.child(track_card(s, p, track, move |s: &mut Ui| {
            s.play_tracks((*play_list).to_vec(), index);
        }));
        shown += 1;
    }
    for _ in shown..columns {
        cards = cards.child(el().grow(1.0).min_w(0.0));
    }
    col().w_full().gap(14.0)
        .child(
            row().w_full().center_y().gap(10.0)
                .child(text("Recently played").font(H2).color(p.text))
                .child(spacer())
                .child(
                    secondary_button(p, "Clear history", true, |s: &mut Ui, _: &mut Tx| {
                        s.clear_history();
                    })
                    .key("home-history-clear"),
                ),
        )
        .child(cards)
}

fn home_tab(s: &Ui, p: Palette, value: HomeTab, label: &str, key: &str) -> El {
    let active = s.home_tab == value;
    col().key(key.to_owned()).role("button").label(label.to_owned()).gap(8.0).focusable(true)
        .on_press(move |s: &mut Ui| s.home_tab = value)
        .child(text(label.to_owned()).font(TextStyle::new(13.0, 600.0)).color(if active { p.text } else { p.dim }))
        .child(el().w_full().h(2.0).fill(if active { p.text } else { Color::TRANSPARENT }))
}

/// The "For you" tab: the user's saved tracks, the custom-mix gallery, the TIDAL
/// recommendations, and the album and playlist rows of the home feed.
fn for_you(s: &Ui, p: Palette) -> El {
    let mut body = col().w_full().gap(32.0);
    if !s.history_log.is_empty() {
        body = body.child(recently_played(s, p));
    }
    match &s.home_state {
        SearchState::Idle | SearchState::Searching => {
            body = body.child(text("Loading your music…").font(BODY).color(p.dim));
        }
        SearchState::Failed(error) => {
            body = body.child(text("Could not load your collection").font(H2).color(p.text))
                .child(text(error.clone()).font(BODY).color(p.dim).wrap_anywhere())
                .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| s.load_home()));
        }
        SearchState::Done if s.home_tracks.is_empty() => {
            body = body.child(text("Your music, all in one place").font(H1).color(p.text))
                .child(text("Favorite tracks on TIDAL to see them here, or search for something to play.")
                    .font(BODY).color(p.dim).wrap_anywhere())
                .child(primary_button(p, "Explore music", true, |s: &mut Ui, _: &mut Tx| {
                    s.navigate(crate::history::Route::Search);
                    s.query.focused = true;
                }));
        }
        SearchState::Done => {
            let mut shortcuts = col().w_full().gap(8.0);
            for group in s.home_tracks.iter().take(6).collect::<Vec<_>>().chunks(3) {
                let mut line = row().w_full().gap(8.0);
                for track in group {
                    line = line.child(shortcut_card(s, p, track));
                }
                for _ in group.len()..3 { line = line.child(el().grow(1.0)); }
                shortcuts = shortcuts.child(line);
            }
            body = body.child(shortcuts);
            let after_gallery = if s.mixes.is_empty() {
                let picks: Vec<_> = s.home_tracks.iter().skip(6).take(4).collect();
                if !picks.is_empty() {
                    let mut cards = row().w_full().gap(16.0);
                    for track in &picks { cards = cards.child(album_card(s, p, track)); }
                    for _ in picks.len()..4 { cards = cards.child(el().grow(1.0)); }
                    body = body.child(col().w_full().gap(14.0)
                        .child(text("Your favorite tracks").font(H2).color(p.text)).child(cards));
                }
                10
            } else {
                body = body.child(mix_gallery(s, p));
                6
            };
            if s.home_tracks.len() > after_gallery {
                let mut list = col().w_full().gap(6.0);
                let source: std::rc::Rc<[Track]> = s.home_tracks.as_slice().into();
                let tracks: Vec<_> = s.home_tracks.iter().skip(after_gallery).take(9).collect();
                for (chunk, group) in tracks.chunks(3).enumerate() {
                    let mut line = row().w_full().gap(24.0);
                    for (slot, track) in group.iter().enumerate() {
                        let index = after_gallery + chunk * 3 + slot;
                        line = line.child(compact_track(s, p, track, &source, index, "home"));
                    }
                    for _ in group.len()..3 { line = line.child(el().grow(1.0)); }
                    list = list.child(line);
                }
                body = body.child(col().w_full().gap(14.0)
                    .child(text("More from your collection").font(H2).color(p.text))
                    .child(list));
            }
        }
    }
    if s.home_tracks.is_empty() && !s.mixes.is_empty() {
        body = body.child(mix_gallery(s, p));
    }
    for (section_index, section) in s.recommendations.iter().enumerate() {
        let mut list = col().w_full().gap(6.0);
        let source: std::rc::Rc<[Track]> = section.tracks.as_slice().into();
        let prefix = format!("rec-{section_index}");
        for (chunk, group) in source.chunks(3).enumerate() {
            let mut line = row().w_full().gap(24.0);
            for (slot, track) in group.iter().enumerate() {
                line = line.child(compact_track(s, p, track, &source, chunk * 3 + slot, &prefix));
            }
            for _ in group.len()..3 { line = line.child(el().grow(1.0)); }
            list = list.child(line);
        }
        body = body.child(col().w_full().gap(14.0)
            .child(text(section.title.clone()).font(H2).color(p.text)).child(list));
    }
    body.child(catalogue_sections_view(s, p, "home-sections", &s.home_sections))
}

fn shortcut_card(s: &Ui, p: Palette, track: &Track) -> El {
    let id = track.id;
    row().key(format!("shortcut-{id}")).role("button").label(format!("Play {}", track.title))
        .grow(1.0).min_w(0.0).h(60.0).px(8.0).gap(10.0).center_y()
        .radius(4.0).fill(p.card).hover(p.card_hover).focusable(true)
        .on_press(move |s: &mut Ui| s.play_id(id))
        .child(cover_art(s, p, track.cover.as_deref(), Some(44.0)))
        .child(col().grow(1.0).min_w(0.0).gap(3.0)
            .child(text(track.title.clone()).font(TRACK).color(p.text).truncate())
            .child(text(track.artist.clone()).font(SMALL).color(p.dim).truncate()))
}

fn album_card(s: &Ui, p: Palette, track: &Track) -> El {
    let id = track.id;
    col().key(format!("album-card-{id}")).role("button").label(format!("Play {}", track.title))
        .grow(1.0).min_w(0.0).gap(8.0).focusable(true)
        .on_press(move |s: &mut Ui| s.play_id(id))
        .child(cover_art(s, p, track.cover.as_deref(), None))
        .child(text(track.title.clone()).font(TRACK).color(p.text).truncate())
        .child(text(track.artist.clone()).font(SMALL).color(p.dim).truncate())
}

fn compact_track(
    s: &Ui,
    p: Palette,
    track: &Track,
    source: &std::rc::Rc<[Track]>,
    index: usize,
    prefix: &str,
) -> El {
    let id = track.id;
    let menu_key = format!("{prefix}-track-menu-{index}");
    let menu = TrackMenu {
        track: track.clone(),
        list: Some(source.clone()),
        index,
        playlist: None,
        queue: false,
    };
    let menu_for_click = menu.clone();
    let menu_key_for_click = menu_key.clone();
    // A local file has no TIDAL favourite, so its row (including History entries) has no heart.
    let favourite = if crate::local::is_local_id(id) {
        el().w(32.0)
    } else {
        heart_button(s, p, FavKind::Track, &id.to_string(), 32.0)
    };
    row().role("button").label(format!("Play {}", track.title))
        .grow(1.0).min_w(0.0).h(62.0).px(7.0).gap(12.0).center_y()
        .radius(4.0).hover(p.card).focusable(true)
        .on_press(move |s: &mut Ui| s.play_id(id))
        .on_secondary_press(move |s: &mut Ui, press: rune::PointerPress, _: &mut rune::Tx| {
            s.open_menu(MenuState::track(menu_key_for_click.clone(), menu_for_click.clone()).at(press))
        })
        .child(cover_art(s, p, track.cover.as_deref(), Some(48.0)))
        .child(col().grow(1.0).min_w(0.0).gap(4.0)
            .child(text(track.title.clone()).font(TRACK).color(p.text).truncate())
            .child(text(track.artist.clone()).font(SMALL).color(p.dim).truncate()))
        .child(favourite)
        .child(menu_button(p, &menu_key, "Track options", menu))
}
