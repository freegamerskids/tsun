//! Album page: the header (ALBUM label, title, artist link, year, track count, total
//! duration, quality badge, the review TIDAL wrote with its More/Less toggle, and the
//! Play/Shuffle and favourite controls) and the album's full track table.
//!
//! The header draws no cover of its own: the title block starts at the page's left edge, so
//! the review TIDAL wrote for the album sits right under the title, artist and meta line where
//! Apple Music puts it. The Credits and Related albums that come with it stay below the track
//! list.

use rune::{El, Tx, col, el, row, text};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, CAPS, H2, HERO, Palette, SMALL, SUBTITLE};
use crate::tidal::{Album, FavKind};
use crate::ui::cards::{album_card, heart_button, link_text, quality_badge};
use crate::ui::context_menu::{MenuState, header_menu_button};
use crate::ui::tracks::{TRACK_H, section, tracks_page};
use crate::util::{quality_badge as badge_label, total_duration};
use crate::widgets::{primary_button, secondary_button, track_count};

/// The lines the album's review shows before its "More" button, as Apple Music clamps it.
const REVIEW_LINES: usize = 3;
/// The width the review wraps at: a paragraph, not a banner across a wide window.
const REVIEW_W: f32 = 640.0;
/// How many characters a review may hold and still fit three lines at [`REVIEW_W`]. The count is
/// deliberately above what three lines really hold (roughly 90 characters each at this width and
/// text style), so a "More" button never promises text that is already visible — and a review
/// between the two lengths simply draws its own lines, with no clamp and no button.
const REVIEW_LONG: usize = 320;

/// Whether a review is long enough to clamp to [`REVIEW_LINES`] with a "More" button.
pub(crate) fn review_is_long(text: &str) -> bool {
    text.chars().count() > REVIEW_LONG
}

pub(crate) fn album_content(s: &Ui, p: Palette) -> El {
    if s.album.is_none() {
        return el();
    }
    let rows = s.album_tracks.shared();
    let mut page = tracks_page("album-content", rows.clone(), !rows.rows.is_empty(), TRACK_H)
        .leading(section("header", header).after(27.0));
    match &s.album_state {
        SearchState::Idle | SearchState::Searching => {
            page = page.trailing(
                section("loading", |_, p| {
                    col().w_full().child(text("Loading album…").font(BODY).color(p.dim))
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
                            if let Some(id) = s.album.as_ref().map(|album| album.id) {
                                s.open_album(id);
                            }
                        }).key("album-retry"))
                })
                .before(27.0),
            );
        }
        SearchState::Done if rows.rows.is_empty() => {
            page = page.trailing(
                section("empty", |_, p| {
                    col().w_full().child(text("This album has no tracks.").font(BODY).color(p.dim))
                })
                .before(27.0),
            );
        }
        SearchState::Done => {}
    }
    page = page.trailing(section("extras", |s, p| extras(s, p)).before(27.0));
    page.render(s, p)
}

/// The album's extras below the track list: the album credits and TIDAL's related albums. Each
/// appears only when TIDAL actually returned it. The review is not among them: it belongs in
/// the header, above the transport.
fn extras(s: &Ui, p: Palette) -> El {
    let Some(extras) = &s.album_extras else { return el() };
    let mut body = col().w_full().gap(32.0);
    if !extras.credits.is_empty() {
        let mut credits = col().w_full().gap(6.0)
            .child(text("Credits").font(H2).color(p.text));
        for credit in &extras.credits {
            credits = credits.child(
                row().w_full().gap(10.0).items_start()
                    .child(text(credit.role.clone()).font(SMALL).color(p.faint).w(150.0))
                    .child(
                        text(credit.names.join(", "))
                            .font(SMALL)
                            .color(p.dim)
                            .grow(1.0)
                            .wrap_anywhere(),
                    ),
            );
        }
        body = body.child(credits);
    }
    if !extras.similar.is_empty() {
        let mut strip = row().w_full().gap(16.0).items_start().scroll_x().scroll_edge_fade(24.0);
        for album in &extras.similar {
            strip = strip.child(album_card(s, p, album));
        }
        body = body.child(
            col().w_full().gap(14.0)
                .child(text("Related albums").font(H2).color(p.text))
                .child(strip),
        );
    }
    body
}

/// The album page's header: title, artist, year, counts, quality, the review TIDAL wrote and
/// the transport. No cover of its own, so the title block starts at the page's left edge.
fn header(s: &Ui, p: Palette) -> El {
    let Some(album) = &s.album else { return el() };
    col()
        .w_full()
        .min_w(0.0)
        .gap(12.0)
        .child(text("ALBUM").font(CAPS).color(p.dim))
        .child(text(album.title.clone()).font(HERO).color(p.text).truncate())
        .child(artist(album, p))
        .child(meta(s, album, p))
        .child(review(s, p))
        .child(actions(s, album, p))
}

/// The album's review in the header, under the title: TIDAL's prose, the source it credits, and
/// the "More"/"Less" toggle a long one gets. Nothing at all while TIDAL has no review for the
/// album (or has not returned one yet), and no toggle for a review that fits in [`REVIEW_LINES`]
/// lines.
fn review(s: &Ui, p: Palette) -> El {
    let Some(id) = s.album.as_ref().map(|album| album.id) else { return el() };
    let Some(review) = s.album_extras.as_ref().and_then(|extras| extras.review.as_ref()) else {
        return el();
    };
    let long = review_is_long(&review.text);
    let expanded = long && s.album_review_open == Some(id);
    let mut prose = text(review.text.clone())
        .key("album-review")
        .font(BODY)
        .color(p.dim)
        .wrap_anywhere()
        .max_w(REVIEW_W);
    if long && !expanded {
        prose = prose.max_lines(REVIEW_LINES);
    }
    let mut block = col().w_full().min_w(0.0).gap(6.0).child(prose);
    if let Some(source) = &review.source {
        block = block.child(text(format!("Source: {source}")).font(SMALL).color(p.faint));
    }
    if long {
        block = block.child(
            link_text(p, if expanded { "Less" } else { "More" }.to_owned(), SMALL, move |s: &mut Ui| {
                s.album_review_open = if expanded { None } else { Some(id) };
            })
            .key("album-review-more"),
        );
    }
    block
}

fn artist(album: &Album, p: Palette) -> El {
    match album.artist_id {
        Some(id) => {
            let artist = crate::tidal::Artist { id, name: album.artist.clone(), picture: None };
            link_text(p, album.artist.clone(), SUBTITLE, move |s: &mut Ui| s.open_artist_with(artist.clone()))
                .key("album-artist-link")
                .w_full()
        }
        None => text(album.artist.clone()).font(SUBTITLE).color(p.dim).truncate(),
    }
}

fn meta(s: &Ui, album: &Album, p: Palette) -> El {
    let mut parts = row().gap(8.0).center_y();
    if let Some(year) = &album.year {
        parts = parts
            .child(text(year.clone()).font(SMALL).color(p.dim))
            .child(text("·").font(SMALL).color(p.faint));
    }
    let count = album.tracks.map(u64::from).unwrap_or(s.album_tracks.len() as u64);
    parts = parts.child(text(track_count(count)).font(SMALL).color(p.dim));
    if let Some(duration) = album.duration.map(crate::util::format_duration).or_else(|| total_duration(s.album_tracks.items())) {
        parts = parts
            .child(text("·").font(SMALL).color(p.faint))
            .child(text(duration).font(SMALL).color(p.dim));
    }
    if let Some(label) = badge_label(album.quality.as_deref()) {
        parts = parts
            .child(text("·").font(SMALL).color(p.faint))
            .child(quality_badge(p, label));
    }
    parts
}

fn actions(s: &Ui, album: &Album, p: Palette) -> El {
    let rounds = s.album_tracks.len();
    row().gap(12.0).center_y().mt(4.0)
        .child(primary_button(p, "Play", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = s.album_tracks.to_vec();
            if !tracks.is_empty() {
                s.play_tracks(tracks, 0);
            }
        }))
        .child(secondary_button(p, "Shuffle", rounds > 0, |s: &mut Ui, _: &mut Tx| {
            let tracks = s.album_tracks.to_vec();
            s.play_tracks_shuffled(tracks);
        }).key("album-shuffle"))
        .child(heart_button(s, p, FavKind::Album, &album.id.to_string(), 40.0))
        .child(header_menu_button(
            p,
            "album-menu",
            "Album options",
            MenuState::album("album-menu", album.clone()),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::init;
    use crate::theme::palette;
    use crate::tidal::{AlbumExtras, AlbumReview, Track};

    /// The album id the test pages open.
    const ALBUM: u64 = 42;

    /// The More/Less decision: only a review longer than the three lines the clamp shows gets
    /// the toggle, counted in characters rather than bytes.
    #[test]
    fn only_a_long_review_gets_the_more_button() {
        assert!(!review_is_long(""), "no text, no toggle");
        assert!(!review_is_long(&"a".repeat(REVIEW_LONG)), "a review that fits keeps no button");
        assert!(review_is_long(&"a".repeat(REVIEW_LONG + 1)), "one character over it gets one");
        let accented = "é".repeat(REVIEW_LONG);
        assert!(accented.len() > REVIEW_LONG, "the text is longer in bytes than in characters");
        assert!(!review_is_long(&accented), "the count is characters, so accented prose measures the same");
    }

    /// The album page under a full-height root, as the window provides it.
    fn album_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        col().w_full().h_full().child(album_content(s, palette()))
    }

    /// An album page whose extras carry `review` as TIDAL's editorial text.
    fn album_page(review: &str) -> rune::Runtime<Ui> {
        let mut s = init();
        s.album = Some(Album {
            id: ALBUM,
            title: "Album".to_owned(),
            artist: "Artist".to_owned(),
            ..Album::default()
        });
        s.album_state = SearchState::Done;
        s.album_tracks.set(vec![Track { id: 7, title: "Song".to_owned(), ..Track::default() }]);
        s.album_extras = Some(AlbumExtras {
            review: Some(AlbumReview { text: review.to_owned(), source: Some("TiVo".to_owned()) }),
            ..AlbumExtras::default()
        });
        let mut rt = rune::Runtime::new(s, album_view, (900.0, 700.0), rune::Env::new(true, 1.0));
        for _ in 0..4 {
            rt.frame(0.0);
        }
        rt
    }

    /// The height the element keyed `key` lays out at, and its text.
    fn element(rt: &rune::Runtime<Ui>, key: &str) -> Option<(f32, Option<String>)> {
        rt.elements()
            .into_iter()
            .find(|element| !element.exiting && element.key.as_deref() == Some(key))
            .map(|element| (element.rect[3], element.text))
    }

    /// A long review is clamped to three lines behind a "More" button that expands it in place
    /// and becomes "Less"; a short review has neither.
    #[test]
    fn a_long_review_clamps_behind_more_and_a_short_one_does_not() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let long = "One two three four five six seven eight. ".repeat(40);
        let mut rt = album_page(&long);
        let clamped = element(&rt, "album-review").expect("the review is drawn").0;
        let (_, label) = element(&rt, "album-review-more").expect("a long review gets its More button");
        assert_eq!(label.as_deref(), Some("More"), "the toggle reads More while collapsed");

        // Pressing More expands the same block where it stands.
        let (x, y) = rt.find("album-review-more").expect("the toggle is drawn");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(rt.state.album_review_open, Some(ALBUM), "the header remembers the album it expanded");
        let expanded = element(&rt, "album-review").expect("the review is still drawn").0;
        assert!(expanded > clamped, "expanding shows the rest of the review: {clamped} -> {expanded}");
        let (_, label) = element(&rt, "album-review-more").expect("the toggle stays");
        assert_eq!(label.as_deref(), Some("Less"), "the toggle reads Less while expanded");

        // Pressing Less clamps it back.
        let (x, y) = rt.find("album-review-more").expect("the toggle is still drawn");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        assert_eq!(rt.state.album_review_open, None, "Less forgets the expanded album");
        assert_eq!(element(&rt, "album-review").map(|(height, _)| height), Some(clamped), "the review is clamped again");

    // A review that fits: drawn, with no toggle and no clamp.
        let short = album_page("A short note about the album.");
        assert!(element(&short, "album-review-more").is_none(), "a short review shows no More");
        assert!(element(&short, "album-review").is_some(), "but it is drawn");
    }

    /// The threshold promise: a review the length of [`REVIEW_LONG`] plus a character really
    /// does run past the three lines the clamp shows, so its "More" button reveals text. This is
    /// the check on the characters-per-line estimate behind [`REVIEW_LONG`]; were the estimate
    /// wrong, the button would appear over text the reader can already see.
    #[test]
    fn a_review_over_the_threshold_is_really_cut() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let prose = "The record opens on a slow, patient figure that the band never hurries, \
                     and by the time the second side arrives the whole thing has turned over into \
                     something far stranger than its first minutes suggest. ";
        let mut review = prose.repeat(8);
        review = review.chars().take(REVIEW_LONG + 1).collect();
        assert!(review_is_long(&review), "the text is one character past the threshold");
        let mut rt = album_page(&review);
        let clamped = element(&rt, "album-review").expect("the review is drawn").0;
        rt.state.album_review_open = Some(ALBUM);
        rt.invalidate();
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        let expanded = element(&rt, "album-review").expect("the review is drawn").0;
        assert!(expanded > clamped, "the clamped review is really cut: {clamped} -> {expanded}");
    }

    /// An album with no review draws no description block at all.
    #[test]
    fn an_album_without_a_review_has_no_description() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let mut rt = album_page("");
        rt.state.album_extras = None;
        rt.invalidate();
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        assert!(element(&rt, "album-review").is_none(), "nothing to describe");
        assert!(element(&rt, "album-review-more").is_none(), "and no toggle");
    }

    /// The expanded state belongs to one album: opening another starts it collapsed again.
    #[test]
    fn the_expanded_review_does_not_follow_to_another_album() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let mut s = init();
        s.album = Some(Album { id: 1, ..Album::default() });
        s.album_review_open = Some(1);
        s.open_album(2);
        assert_eq!(s.album_review_open, None, "another album's review starts collapsed");
        assert_eq!(s.album.as_ref().map(|album| album.id), Some(2), "the page moved to the album");
        s.album_review_open = Some(2);
        s.open_album(2);
        assert_eq!(s.album_review_open, Some(2), "reopening an album keeps the state the user left");
    }
}
