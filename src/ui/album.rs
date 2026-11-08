//! Album page: the header (200pt cover, ALBUM label, title, artist link, year, track count,
//! total duration, quality badge, the review TIDAL wrote with its "More" button, and the
//! Play/Shuffle and favourite controls) and the album's full track table.
//!
//! The header carries the album's cover, so the table's rows do not: every row of an album
//! shows the same picture, and dropping the cell puts the title column right after the number
//! whose play button the rows grow on hover (`Page::covers`). The review TIDAL wrote sits under
//! the title, artist and meta line where Apple Music puts it: a three-line preview, and — when
//! the text is longer — a "More" button that opens the whole review in a modal. The Credits and
//! Related albums that come with it stay below the track list.

use rune::{El, Tx, col, el, row, text};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, CAPS, H2, HERO, Palette, SMALL, SUBTITLE};
use crate::tidal::{Album, FavKind};
use crate::ui::cards::{album_card, cover_tile, heart_button, link_text, quality_badge, title_with_badge};
use crate::ui::context_menu::{MenuState, header_menu_button};
use crate::ui::tracks::{TRACK_H, section, tracks_page};
use crate::util::{quality_badge as badge_label, total_duration};
use crate::widgets::{primary_button, secondary_button, track_count};

/// The lines the album's review shows in the header before its "More" button, as Apple Music
/// clamps it. The whole text lives in the modal the button opens.
const REVIEW_LINES: usize = 3;
/// The width the review wraps at: a paragraph, not a banner across a wide window.
const REVIEW_W: f32 = 640.0;
/// How many characters a review may hold and still fit three lines at [`REVIEW_W`]. The count is
/// deliberately above what three lines really hold (roughly 90 characters each at this width and
/// text style), so a "More" button never promises text that the preview already shows — and a
/// review between the two lengths simply draws its own lines, with no clamp and no button.
const REVIEW_LONG: usize = 320;

/// Whether a review is long enough to clamp to [`REVIEW_LINES`] behind a "More" button.
pub(crate) fn review_is_long(text: &str) -> bool {
    text.chars().count() > REVIEW_LONG
}

pub(crate) fn album_content(s: &Ui, p: Palette) -> El {
    if s.album.is_none() {
        return el();
    }
    let rows = s.album_tracks.shared();
    let mut page = tracks_page("album-content", rows.clone(), !rows.rows.is_empty(), TRACK_H)
        // The cover is in the header: the rows would repeat it.
        .covers(false)
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

/// The album page's header: its cover, then the title, artist, year, counts, quality, the
/// review TIDAL wrote and the transport.
fn header(s: &Ui, p: Palette) -> El {
    let Some(album) = &s.album else { return el() };
    let details = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(12.0)
        .child(text("ALBUM").font(CAPS).color(p.dim))
        .child(title_with_badge(p, text(album.title.clone()).font(HERO).color(p.text).truncate(), album.explicit))
        .child(artist(album, p))
        .child(meta(s, album, p))
        .child(review(s, p))
        .child(actions(s, album, p));
    // Top-aligned, like the details beside it start at the top: bottom alignment left the
    // cover floating low whenever the review made the details column tall.
    row()
        .w_full()
        .gap(32.0)
        .items_start()
        .child(cover_tile(s, p, album.cover.as_deref(), COVER))
        .child(details)
}

/// The header cover's size.
const COVER: f32 = 240.0;

/// The album's review in the header, under the title: TIDAL's prose as a three-line preview, the
/// source it credits, and — when there is more to read than the preview shows — the "More"
/// button that opens the whole review in a modal. Nothing at all while TIDAL has no review for
/// the album (or has not returned one yet), and no button for a review that fits the preview.
fn review(s: &Ui, p: Palette) -> El {
    let Some(review) = s.album_extras.as_ref().and_then(|extras| extras.review.as_ref()) else {
        return el();
    };
    let long = review_is_long(&review.text);
    let mut prose = text(review.text.clone())
        .key("album-review")
        .font(BODY)
        .color(p.dim)
        .wrap_anywhere()
        .max_w(REVIEW_W);
    if long {
        prose = prose.max_lines(REVIEW_LINES);
    }
    let mut block = col().w_full().min_w(0.0).gap(6.0).child(prose);
    if let Some(source) = &review.source {
        block = block.child(text(format!("Source: {source}")).font(SMALL).color(p.faint));
    }
    if long {
        block = block.child(
            link_text(p, "More".to_owned(), SMALL, |s: &mut Ui| s.open_album_review())
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
    use crate::app::{Login, Page, init};
    use crate::theme::palette;
    use crate::tidal::{AlbumExtras, AlbumReview, Track};
    use crate::ui::dialogs::{self, Dialog};

    /// The album id the test pages open.
    const ALBUM: u64 = 42;

    /// The threshold: only a review longer than the three lines the preview shows gets the
    /// "More" button, counted in characters rather than bytes.
    #[test]
    fn only_a_long_review_gets_the_more_button() {
        assert!(!review_is_long(""), "no text, no button");
        assert!(!review_is_long(&"a".repeat(REVIEW_LONG)), "a review that fits keeps no button");
        assert!(review_is_long(&"a".repeat(REVIEW_LONG + 1)), "one character over it gets one");
        let accented = "é".repeat(REVIEW_LONG);
        assert!(accented.len() > REVIEW_LONG, "the text is longer in bytes than in characters");
        assert!(!review_is_long(&accented), "the count is characters, so accented prose measures the same");
    }

    /// The album page under a full-height root, with the window's dialog layer under it — the
    /// page plus whatever modal is open, as the window composes them.
    fn album_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        col()
            .w_full()
            .h_full()
            .child(album_content(s, palette()))
            .child(dialogs::dialog(s, palette()))
    }

    /// The app's state on an album whose extras carry `review` as TIDAL's editorial text.
    fn seeded(review: &str) -> Ui {
        let mut s = init();
        s.album = Some(Album {
            id: ALBUM,
            title: "Album".to_owned(),
            artist: "Artist".to_owned(),
            ..Album::default()
        });
        s.album_state = SearchState::Done;
        // The window's own view draws the page the state names, so the modal test shows the
        // album header behind it — and that page draws only for a signed-in session.
        s.login = Login::LoggedIn;
        s.page = Page::Album;
        s.album_tracks.set(vec![Track { id: 7, title: "Song".to_owned(), ..Track::default() }]);
        s.album_extras = Some(AlbumExtras {
            review: Some(AlbumReview { text: review.to_owned(), source: Some("TiVo".to_owned()) }),
            ..AlbumExtras::default()
        });
        s
    }

    /// An album page whose extras carry `review` as TIDAL's editorial text.
    fn album_page(review: &str) -> rune::Runtime<Ui> {
        let mut rt =
            rune::Runtime::new(seeded(review), album_view, (900.0, 700.0), rune::Env::new(true, 1.0));
        for _ in 0..4 {
            rt.frame(0.0);
        }
        rt
    }

    /// The window's own view with the review modal open, so the scrim, the card and Escape all
    /// behave as they do in the app.
    fn review_modal(review: &str) -> rune::Runtime<Ui> {
        let mut s = seeded(review);
        s.open_album_review();
        let mut rt =
            rune::Runtime::new(s, crate::ui::view, (1120.0, 720.0), rune::Env::new(true, 1.0));
        rt.on_key = Some(crate::app::on_key);
        for _ in 0..3 {
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

    /// The header keeps a three-line preview and never grows: pressing "More" opens the whole
    /// review in the modal instead, and a review that fits the preview has no button at all.
    #[test]
    fn a_long_review_previews_three_lines_and_more_opens_the_modal() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let long = "One two three four five six seven eight. ".repeat(40);
        let mut rt = album_page(&long);
        let preview = element(&rt, "album-review").expect("the preview is drawn").0;
        let (_, label) = element(&rt, "album-review-more").expect("a long review gets its More button");
        assert_eq!(label.as_deref(), Some("More"), "the button reads More");

        // Pressing More opens the modal; the header's preview is untouched.
        let (x, y) = rt.find("album-review-more").expect("the button is drawn");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        let Some(Dialog::AlbumReview(dialog)) = &rt.state.dialog else {
            panic!("More opens the review modal");
        };
        assert_eq!(dialog.text, long, "the modal holds the whole review");
        assert_eq!(dialog.title, "Album", "and names the album it belongs to");
        assert_eq!(dialog.artist, "Artist");
        assert_eq!(dialog.source.as_deref(), Some("TiVo"), "and keeps the source credit");
        assert_eq!(
            element(&rt, "album-review").map(|(height, _)| height),
            Some(preview),
            "the header preview is unchanged: nothing expands in place"
        );

        // A review that fits: drawn, with no button.
        let short = album_page("A short note about the album.");
        assert!(element(&short, "album-review-more").is_none(), "a short review shows no More");
        assert!(element(&short, "album-review").is_some(), "but it is drawn");
    }

    /// The threshold promise: a review the length of [`REVIEW_LONG`] plus a character really
    /// does run past the three lines the preview shows, and the modal then has more text than
    /// the preview did. This is the check on the characters-per-line estimate behind
    /// [`REVIEW_LONG`]; were the estimate wrong, the button would appear over text the reader
    /// can already see. `line` is the height the page gives a one-line review.
    #[test]
    fn a_review_over_the_threshold_is_really_cut() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let line = element(&album_page("A short note about the album."), "album-review")
            .expect("the review is drawn")
            .0;
        let prose = "The record opens on a slow, patient figure that the band never hurries, \
                     and by the time the second side arrives the whole thing has turned over into \
                     something far stranger than its first minutes suggest. ";
        let review: String = prose.repeat(8).chars().take(REVIEW_LONG + 1).collect();
        assert!(review_is_long(&review), "the text is one character past the threshold");
        let mut rt = album_page(&review);
        let preview = element(&rt, "album-review").expect("the preview is drawn").0;
        assert!(
            preview > 2.5 * line && preview < 3.6 * line,
            "the preview is three lines: {preview} against a line of {line}"
        );
        rt.state.open_album_review();
        rt.invalidate();
        for _ in 0..3 {
            rt.frame(1.0 / 120.0);
        }
        let full = element(&rt, "album-review-text").expect("the modal draws the review").0;
        assert!(full > preview, "the modal shows more than the preview: {preview} -> {full}");
    }

    /// The modal as the window draws it — the card, the whole review, its credit and a Close
    /// button — and the three ways out of it: its own Close button, the scrim, and Escape.
    #[test]
    fn the_review_modal_shows_the_whole_text_and_closes_three_ways() {
        // `init` reads the settings file, whose directory the environment names.
        let _env = crate::settings::lock_config_env();
        let text = "One two three four five six seven eight. ".repeat(20);
        let mut rt = review_modal(&text);
        assert!(rt.find("album-review-dialog").is_some(), "the card is up");
        assert!(rt.find("album-review-dialog-close").is_some(), "with its own close button");
        assert!(
            rt.elements().into_iter().any(|element| element.text.as_deref() == Some(text.as_str())),
            "the card draws the whole review"
        );
        assert!(rt.find("album-review-more").is_some(), "the header's More stays behind it");

        // Escape closes it.
        rt.key(rune::KeyEvent { key: rune::Key::Escape, mods: rune::Mods::default() });
        for _ in 0..2 {
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.state.dialog.is_none(), "Escape closes the modal");
        assert!(rt.find("dialog-dismiss").is_none(), "and takes the scrim with it");

        // The scrim dismisses it: a press beside the card, on the window's own edge.
        reopen(&mut rt);
        rt.pointer_down(20.0, 700.0);
        rt.pointer_up(20.0, 700.0);
        for _ in 0..2 {
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.state.dialog.is_none(), "a press on the scrim closes the modal");

        // And so does its own close button.
        reopen(&mut rt);
        let (x, y) = rt.find("album-review-dialog-close").expect("the close button is up");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..2 {
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.state.dialog.is_none(), "the card's close button closes the modal");
    }

    /// Opens the album's review modal again on a runtime that has closed it.
    fn reopen(rt: &mut rune::Runtime<Ui>) {
        rt.state.open_album_review();
        rt.invalidate();
        for _ in 0..2 {
            rt.frame(1.0 / 120.0);
        }
        assert!(rt.find("album-review-dialog").is_some(), "the modal is up again");
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
        assert!(element(&rt, "album-review-more").is_none(), "and no More button");
    }
}
