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
/// next/previous follow it; album, artist, playlist and mix cards navigate on their own.
pub(crate) fn carousel(s: &Ui, p: Palette, key: &str, title: &str, items: &[Card]) -> El {
    let tracks = Rc::new(section_tracks(items));
    cards::carousel_rows(s, p, key, title, items.len(), |index| {
        card_view(s, p, &items[index], &tracks, index)
    })
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
/// through the section list `tracks`.
fn card_view(s: &Ui, p: Palette, card: &Card, tracks: &Rc<Vec<Track>>, index: usize) -> El {
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
