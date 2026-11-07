//! Playlist page: the header (cover, PLAYLIST label, title, track count and
//! total duration, Play/Shuffle and the favourite heart) and the track table.

use rune::{El, Tx, col, el, row, text};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, CAPS, HERO, Palette, SMALL};
use crate::tidal::{FavKind, Playlist};
use crate::ui::cards::{cover_tile, heart_button};
use crate::ui::context_menu::{MenuState, header_menu_button};
use crate::ui::tracks::{TRACK_H, section, tracks_page};
use crate::util::total_duration;
use crate::widgets::{primary_button, secondary_button, track_count};

pub(crate) fn playlist_content(s: &Ui, p: Palette) -> El {
    if s.playlist.is_none() {
        return el();
    }
    let rows = s.playlist_tracks.shared();
    let mut page = tracks_page("playlist-content", rows.clone(), !rows.rows.is_empty(), TRACK_H)
        .leading(section("header", header).after(27.0));
    match &s.playlist_state {
        SearchState::Idle | SearchState::Searching => {
            page = page.trailing(
                section("loading", |_, p| {
                    col().w_full().child(text("Loading playlist…").font(BODY).color(p.dim))
                })
                .before(27.0),
            );
        }
        SearchState::Failed(error) => {
            let failure = error.clone();
            page = page.trailing(
                section("failed", move |_, p| {
                    col().w_full().gap(12.0)
                        .child(text(failure.clone()).font(BODY).color(p.dim).wrap_anywhere())
                        .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| {
                            let Some(playlist) = s.playlist.clone() else { return };
                            s.open_playlist(playlist);
                        }).key("playlist-retry"))
                })
                .before(27.0),
            );
        }
        SearchState::Done if rows.rows.is_empty() => {
            page = page.trailing(
                section("empty", |_, p| {
                    col().w_full().child(text("This playlist has no tracks.").font(BODY).color(p.dim))
                })
                .before(27.0),
            );
        }
        SearchState::Done => {}
    }
    page.render(s, p)
}

/// The playlist page's header: its cover, title, description, counts and transport.
fn header(s: &Ui, p: Palette) -> El {
    let Some(playlist) = &s.playlist else { return el() };
    let count = if s.playlist_tracks.is_empty() { playlist.count } else { s.playlist_tracks.len() as u64 };
    let mut meta = row().gap(8.0).center_y()
        .child(text(track_count(count)).font(SMALL).color(p.dim));
    if let Some(duration) = total_duration(s.playlist_tracks.items()) {
        meta = meta
            .child(text("·").font(SMALL).color(p.faint))
            .child(text(duration).font(SMALL).color(p.dim));
    }
    // TIDAL reports the public flag; showing it makes an accidental private/public playlist
    // visible at a glance, as the official client does.
    if playlist.owned {
        if let Some(public) = playlist.public {
            meta = meta
                .child(text("·").font(SMALL).color(p.faint))
                .child(
                    text(if public { "PUBLIC" } else { "PRIVATE" })
                        .font(CAPS)
                        .color(p.faint),
                );
        }
    }
    let mut info = col().grow(1.0).min_w(0.0).gap(12.0)
        .child(text("PLAYLIST").font(CAPS).color(p.dim))
        .child(text(playlist.title.clone()).font(HERO).color(p.text).truncate());
    if let Some(description) = playlist.description.clone().filter(|text| !text.is_empty()) {
        info = info.child(text(description).font(BODY).color(p.dim).wrap_anywhere());
    }
    info = info.child(meta).child(actions(s, playlist, p));
    row()
        .w_full()
        .gap(28.0)
        .items_end()
        .child(cover_tile(s, p, playlist.cover.as_deref(), 200.0))
        .child(info)
}

fn actions(s: &Ui, playlist: &Playlist, p: Palette) -> El {
    let rounds = s.playlist_tracks.len();
    row().gap(12.0).center_y().mt(4.0)
        .child(primary_button(p, "Play", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = s.playlist_tracks.to_vec();
            if !tracks.is_empty() {
                s.play_tracks(tracks, 0);
            }
        }))
        .child(secondary_button(p, "Shuffle", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = s.playlist_tracks.to_vec();
            s.play_tracks_shuffled(tracks);
        }).key("playlist-shuffle"))
        .child(heart_button(s, p, FavKind::Playlist, &playlist.id, 40.0))
        .child(header_menu_button(
            p,
            "playlist-menu",
            "Playlist options",
            MenuState::playlist("playlist-menu", playlist.clone()),
        ))
}
