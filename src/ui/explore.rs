//! Explore page: TIDAL's explore feed (albums, playlists, artists, mixes, tracks) rendered as
//! generic card carousels, with the new-releases row under it.

use rune::{El, ScrollSpec, Tx, col, text};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, H1, H2, Palette};
use crate::ui::sections::sections_view;
use crate::widgets::secondary_button;

pub(crate) fn explore_content(s: &Ui, p: Palette) -> El {
    let mut body = col().w_full().px(48.0).pt(12.0).pb(48.0).gap(28.0)
        .child(text("Explore").font(H1).color(p.text));
    match &s.explore_state {
        SearchState::Idle | SearchState::Searching if s.explore.is_empty() && s.new_releases.is_empty() => {
            body = body.child(text("Loading Explore…").font(BODY).color(p.dim));
        }
        SearchState::Failed(error) if s.explore.is_empty() && s.new_releases.is_empty() => {
            body = body.child(text("Could not load Explore").font(H2).color(p.text))
                .child(text(error.clone()).font(BODY).color(p.dim).wrap_anywhere())
                .child(secondary_button(p, "Try again", true, |s: &mut Ui, _: &mut Tx| s.open_explore()));
        }
        _ => {
            if s.explore.is_empty() {
                let message = match &s.explore_state {
                    SearchState::Failed(error) => format!("The Explore feed could not be loaded: {error}"),
                    _ => "Nothing to explore right now.".to_owned(),
                };
                body = body.child(text(message).font(BODY).color(p.dim).wrap_anywhere());
            } else {
                body = body.child(sections_view(s, p, "explore", &s.explore));
            }
            if !s.new_releases.is_empty() {
                body = body.child(text("New releases").font(H2).color(p.text))
                    .child(sections_view(s, p, "new-releases", &s.new_releases));
            }
        }
    }
    col().key("explore-content").w_full().grow(1.0).min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false }).child(body)
}
