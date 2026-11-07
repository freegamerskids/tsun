//! Lyrics view: the now-playing track's words, with the current synced line highlighted and
//! kept in view, or plain text when TIDAL only sent unsynchronised lyrics.
//!
//! The panel is mounted by the playback surface; the toggle button lives there too. Everything
//! it draws comes from `Ui::lyrics` (the `(track_id, Option<LyricsData>)` the worker filled) and
//! `Ui::now_playing.elapsed`, which the worker updates once a second.

use rune::{El, ScrollSpec, TextStyle, col, el, row, spacer, text};

use crate::app::{SearchState, Ui};
use crate::theme::{CAPS, H2, Palette, SMALL, TRACK};
use crate::tidal::LyricsData;

/// The index of the synced line in force at `elapsed_ms`: the last line whose timestamp is at
/// or before it. `None` before the first line or when there are no synced lines.
pub(crate) fn highlighted_line(data: &LyricsData, elapsed_ms: u64) -> Option<usize> {
    if data.synced.is_empty() {
        return None;
    }
    let mut current = None;
    for (index, (time, _)) in data.synced.iter().enumerate() {
        if *time <= elapsed_ms {
            current = Some(index);
        } else {
            break;
        }
    }
    current
}

/// The lyrics of the current track, or a state message. `s.lyrics` holds
/// `(track_id, Option<LyricsData>)`; a `None` payload with a finished load means TIDAL had no
/// lyrics for the track.
pub(crate) fn lyrics_panel(s: &Ui, p: Palette) -> El {
    let mut panel = col().key("lyrics-panel").w_full().h_full().min_h(0.0).gap(14.0);
    let Some(playing) = &s.now_playing else {
        return panel
            .child(text("Lyrics").font(H2).color(p.text))
            .child(text("Nothing is playing.").font(SMALL).color(p.dim));
    };
    panel = panel
        .child(text("Lyrics").font(CAPS).color(p.faint))
        .child(col().w_full().min_w(0.0).gap(3.0)
            .child(text(playing.title.clone()).font(H2).color(p.text).truncate())
            .child(text(playing.artist.clone()).font(SMALL).color(p.dim).truncate()));

    let data = s.lyrics.as_ref().filter(|(id, _)| *id == playing.id).and_then(|(_, data)| data.as_ref());
    let loaded = s.lyrics.as_ref().is_some_and(|(id, _)| *id == playing.id);
    match &s.lyrics_state {
        SearchState::Searching | SearchState::Idle if !loaded => {
            panel.child(text("Loading lyrics…").font(SMALL).color(p.dim))
        }
        SearchState::Failed(error) => panel
            .child(text("Lyrics unavailable").font(SMALL).color(p.dim))
            .child(text(error.clone()).font(SMALL).color(p.dim).wrap_anywhere()),
        _ => match data {
            Some(data) if !data.synced.is_empty() => panel.child(synced_view(p, data, playing.elapsed)),
            Some(data) if !data.text.trim().is_empty() => panel.child(plain_view(p, data)),
            _ => panel.child(text("No lyrics available for this track.").font(SMALL).color(p.dim)),
        },
    }
}

fn synced_view(p: Palette, data: &LyricsData, elapsed: u64) -> El {
    let current = highlighted_line(data, elapsed.saturating_mul(1000));
    let mut lines = col().w_full().gap(10.0).py(8.0);
    for (index, (_, line)) in data.synced.iter().enumerate() {
        let highlighted = current == Some(index);
        lines = lines.child(
            text(line.clone())
                .key(format!("lyric-{index}"))
                .font(if highlighted { TRACK } else { SOLID })
                .color(if highlighted { p.text } else { p.dim })
                .wrap_anywhere(),
        );
    }
    scroll_with_provider(p, data, lines)
}

fn plain_view(p: Palette, data: &LyricsData) -> El {
    let mut lines = col().w_full().gap(10.0).py(8.0);
    for line in data.text.lines() {
        if line.trim().is_empty() {
            lines = lines.child(el().h(6.0));
        } else {
            lines = lines.child(text(line.to_owned()).font(SOLID).color(p.text).wrap_anywhere());
        }
    }
    scroll_with_provider(p, data, lines)
}

/// The scrolling lyrics viewport keyed `lyrics-scroll`, with the attribution line pinned under
/// it so [`crate::app::poll`] can keep the highlighted line centred inside the viewport.
fn scroll_with_provider(p: Palette, data: &LyricsData, lines: El) -> El {
    let viewport = col().key("lyrics-scroll").w_full().grow(1.0).min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(20.0).child(lines);
    let mut panel = col().w_full().grow(1.0).min_h(0.0).child(viewport);
    if let Some(provider) = &data.provider {
        panel = panel.child(
            row().w_full().pt(10.0)
                .child(spacer())
                .child(text(format!("Lyrics by {provider}")).font(SMALL).color(p.faint)),
        );
    }
    panel
}

/// Body-sized lyrics text, one step down from [`TRACK`] so a highlighted line stands out.
const SOLID: TextStyle = TextStyle::new(14.0, 400.0);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{NowPlaying, init};
    use crate::theme::palette;

    fn timed() -> LyricsData {
        LyricsData {
            text: "a\nb\nc".to_owned(),
            synced: vec![(0, "a".to_owned()), (5_000, "b".to_owned()), (10_000, "c".to_owned())],
            provider: Some("TIDAL".to_owned()),
        }
    }

    #[test]
    fn highlighted_line_tracks_the_position() {
        let data = timed();
        assert_eq!(highlighted_line(&data, 0), Some(0));
        assert_eq!(highlighted_line(&data, 4_999), Some(0));
        assert_eq!(highlighted_line(&data, 5_000), Some(1));
        assert_eq!(highlighted_line(&data, 600_000), Some(2));
        let plain = LyricsData { text: "x".to_owned(), synced: Vec::new(), provider: None };
        assert_eq!(highlighted_line(&plain, 10_000), None);
    }

    #[test]
    fn the_panel_renders_every_state() {
        let p = palette();
        let mut s = init();
        // Nothing playing.
        let _ = lyrics_panel(&s, p);
        s.now_playing = Some(NowPlaying {
            id: 7,
            title: "Track".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 6,
            duration: Some(180),
            quality: None,
            output: None,
        });
        // Still loading.
        s.lyrics = Some((7, None));
        s.lyrics_state = SearchState::Searching;
        let _ = lyrics_panel(&s, p);
        // Loaded, TIDAL has none.
        s.lyrics_state = SearchState::Done;
        let _ = lyrics_panel(&s, p);
        // Synced lyrics (6s in: the second line is current).
        s.lyrics = Some((7, Some(timed())));
        let _ = lyrics_panel(&s, p);
        // Plain lyrics.
        s.lyrics = Some((7, Some(LyricsData {
            text: "one\ntwo".to_owned(),
            synced: Vec::new(),
            provider: None,
        })));
        let _ = lyrics_panel(&s, p);
        // Fetch failure.
        s.lyrics_state = SearchState::Failed("no lyrics service".to_owned());
        let _ = lyrics_panel(&s, p);
    }

    fn lyrics_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        lyrics_panel(s, palette())
    }

    /// Renders the panel through Rune's headless runtime with a hand-injected
    /// now-playing track and synced lyrics: the viewport and every line are in
    /// the committed tree, so the panel is mountable and its lines are keyed for
    /// the auto-scroll in `app::poll`.
    #[test]
    fn the_panel_renders_the_synced_lines_in_a_scrolling_viewport() {
        let mut s = init();
        s.now_playing = Some(NowPlaying {
            id: 7,
            title: "Track".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            cover: None,
            album_id: None,
            artist_id: None,
            elapsed: 6,
            duration: Some(180),
            quality: None,
            output: None,
        });
        s.lyrics = Some((7, Some(timed())));
        s.lyrics_state = SearchState::Done;
        let mut rt = rune::Runtime::new(s, lyrics_view, (360.0, 600.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        let keys: Vec<String> = rt
            .elements()
            .into_iter()
            .filter_map(|element| element.key)
            .collect();
        assert!(keys.iter().any(|key| key == "lyrics-panel"), "panel: {keys:?}");
        assert!(keys.iter().any(|key| key == "lyrics-scroll"), "viewport: {keys:?}");
        for line in 0..3 {
            assert!(keys.iter().any(|key| key == &format!("lyric-{line}")), "line {line}: {keys:?}");
        }
        let elapsed_ms = rt.state.now_playing.as_ref()
            .map_or(0, |playing| playing.elapsed.saturating_mul(1000));
        assert_eq!(highlighted_line(&timed(), elapsed_ms), Some(1));
    }

    /// With the full-screen view up, the lyrics dock behind it must not be built: two viewports
    /// share the `lyrics-scroll` key and the auto-scroll would drive the hidden one.
    #[test]
    fn the_full_screen_view_has_the_only_lyrics_viewport() {
        let viewports = |full_screen: bool| {
            let mut s = init();
            s.now_playing = Some(NowPlaying {
                id: 7,
                title: "Track".to_owned(),
                artist: "Artist".to_owned(),
                album: "Album".to_owned(),
                cover: None,
                album_id: None,
                artist_id: None,
                elapsed: 6,
                duration: Some(180),
                quality: None,
                output: None,
            });
            s.lyrics = Some((7, Some(timed())));
            s.lyrics_state = SearchState::Done;
            s.lyrics_open = true;
            s.now_playing_open = full_screen;
            let mut rt = rune::Runtime::new(s, crate::ui::view, (1400.0, 800.0), rune::Env::new(true, 1.0));
            rt.frame(0.0);
            let keys: Vec<String> = rt.elements().into_iter().filter_map(|element| element.key).collect();
            keys.iter().filter(|key| *key == "lyrics-scroll").count()
        };
        assert_eq!(viewports(false), 1, "the dock, with the full-screen view closed");
        assert_eq!(viewports(true), 1, "only the full-screen view's viewport while it is up");
    }
}
