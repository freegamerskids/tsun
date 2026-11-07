//! The queue panel: a right-hand column listing what is playing now and what comes next, with
//! jump-to-row, remove and clear. It reads the same `Queue` the transport walks, so it always
//! agrees with the next/previous buttons.
//!
//! The upcoming rows are one virtual list, so a queue of thousands costs a window of rows to
//! build, not every row. Row handlers find their track's place in the queue by id at press
//! time, so a row kept while the queue is edited still acts on its own track.

use rune::{Color, El, col, el, icon, row, text};

use crate::app::Ui;
use crate::theme::{BODY, H2, ICON_CLOSE, ICON_QUEUE, SMALL, TRACK, Palette};
use crate::ui::context_menu::{MenuState, TrackMenu, menu_button};
use crate::ui::tracks::{section, track_rows_page, version};
use crate::widgets::cover_art;

use super::player_bar::format_time;

/// Width of the queue panel.
const QUEUE_W: f32 = 320.0;

/// The row height the panel's unvisited rows are estimated at.
const ROW_H: f32 = 52.0;

/// The right-hand queue panel, shown while [`Ui::queue_open`] is set.
pub(crate) fn queue_panel(s: &Ui, p: Palette) -> El {
    let current = s.queue.index();
    let current_id = s.queue.current_id();

    // The header, and the now-playing block above the list, stay put: only what comes next
    // scrolls.
    let mut header = row().w_full().h(44.0).px(4.0).gap(8.0).center_y()
        .child(text("Queue").font(H2).color(p.text).grow(1.0));
    if !s.queue.is_empty() {
        header = header.child(
            el().key("queue-clear").role("button").label("Clear queue")
                .h(28.0).px(10.0).center().radius(14.0).hover(p.card_hover).focusable(true)
                .on_press(|s: &mut Ui| s.clear_queue())
                .child(text("Clear").font(SMALL).color(p.dim)),
        );
    }
    header = header.child(
        el().key("queue-close").role("button").label("Close queue panel")
            .size(28.0, 28.0).center().radius(14.0).hover(p.card_hover).focusable(true)
            .on_press(|s: &mut Ui| s.toggle_queue())
            .child(icon(&ICON_CLOSE, 14.0).color(p.dim)),
    );

    let mut body = col().w_full().grow(1.0).min_h(0.0).gap(6.0);
    if let Some(track) = s.queue.current() {
        body = body
            .child(text("Now playing").font(SMALL).color(p.faint))
            .child(queue_row(s, p, track, true));
    }
    if s.queue.is_empty() {
        body = body.child(
            col().w_full().py(40.0).center().gap(6.0)
                .child(text("The queue is empty.").font(BODY).color(p.dim))
                .child(text("Play a track to fill it.").font(SMALL).color(p.faint)),
        );
    } else {
        // Every track but the current one, in queue order.
        let list = s.queue_tracks.get(version((s.queue.revision(), current_id)), || {
            s.queue
                .tracks()
                .iter()
                .filter(|track| Some(track.id) != current_id)
                .cloned()
                .collect()
        });
        let mut page = track_rows_page("queue-list", list, ROW_H, |s, p, _, track| {
            queue_row(s, p, track, false)
        })
        .padding(0.0, 0.0, 0.0);
        if current.is_some() {
            page = page.leading(
                section("next-up", |_, p| {
                    row().w_full().pt(10.0).gap(6.0).center_y()
                        .child(icon(&ICON_QUEUE, 13.0).color(p.faint))
                        .child(text("Next up").font(SMALL).color(p.faint))
                })
                .after(2.0),
            );
        }
        body = body.child(page.render(s, p));
    }

    col()
        .key("queue-panel")
        .w(QUEUE_W)
        .h_full()
        .min_h(0.0)
        .px(14.0)
        .py(12.0)
        .gap(4.0)
        .fill(p.bg)
        .border(1.0, p.border)
        .child(header)
        .child(body)
}

/// The queue index of the track with `id`, found when a row's handler runs so a virtual row
/// never acts on an index the queue has moved past.
fn queue_index(s: &Ui, id: u64) -> usize {
    s.queue.tracks().iter().position(|track| track.id == id).unwrap_or(0)
}

/// One queue row: cover, title/artist, duration and a remove button.
fn queue_row(s: &Ui, p: Palette, track: &crate::tidal::Track, current: bool) -> El {
    let id = track.id;
    let menu_key = format!("queue-track-menu-{id}");
    let menu = TrackMenu { track: track.clone(), list: None, index: 0, playlist: None, queue: true };
    let anchor = menu_key.clone();
    let for_press = menu.clone();
    let mut row = row()
        .key(format!("queue-row-{id}"))
        .role("button")
        .label(format!("Play {}", track.title))
        .w_full()
        .h(ROW_H)
        .px(8.0)
        .gap(10.0)
        .center_y()
        .radius(6.0)
        .fill(if current { p.card } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .on_press(move |s: &mut Ui| s.play_queue_index(queue_index(s, id)))
        .on_secondary_press(move |s: &mut Ui, _: rune::PointerPress, _: &mut rune::Tx| {
            s.open_menu(MenuState::track(anchor.clone(), for_press.clone()))
        });
    let mut text_col = col().grow(1.0).min_w(0.0).gap(2.0)
        .child(text(track.title.clone()).font(TRACK).color(if current { p.accent } else { p.text }).truncate())
        .child(text(track.artist.clone()).font(SMALL).color(p.dim).truncate());
    if current {
        text_col = text_col.child(text(s.now_playing.as_ref().and_then(|playing| playing.quality.as_deref()).unwrap_or("Playing")).font(SMALL).color(p.faint).truncate());
    }
    row = row.child(cover_art(s, p, track.cover.as_deref(), Some(40.0))).child(text_col);
    if let Some(duration) = track.duration.filter(|duration| *duration > 0) {
        row = row.child(text(format_time(duration)).font(SMALL).color(p.faint));
    }
    if !current {
        row = row.child(
            el().key(format!("queue-remove-{id}")).role("button").label("Remove from queue")
                .size(26.0, 26.0).center().radius(13.0).hover(p.card_hover).focusable(true)
                .on_press(move |s: &mut Ui| s.remove_from_queue(queue_index(s, id)))
                .child(icon(&ICON_CLOSE, 13.0).color(p.faint)),
        );
    }
    row.child(menu_button(p, &menu_key, "Track options", menu))
}

#[cfg(test)]
mod tests {
    use crate::app::init;
    use crate::tidal::Track;

    /// The queue panel mounts a window of rows, not the queue: a 2,000-track queue costs a
    /// screenful of rows per commit, and its rows keep their own layout (cover, remove, menu).
    #[test]
    fn a_long_queue_mounts_a_window_of_rows() {
        let _env = crate::settings::lock_config_env();
        let mut s = init();
        s.queue_open = true;
        s.queue.set(
            (0..2_000)
                .map(|index| Track {
                    id: index as u64 + 1,
                    title: format!("Track {index}"),
                    artist: format!("Artist {index}"),
                    duration: Some(180),
                    ..Track::default()
                })
                .collect(),
            0,
        );
        let mut rt = rune::Runtime::new(s, crate::ui::view, (1280.0, 800.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        for _ in 0..8 {
            rt.frame(1.0 / 120.0);
        }
        let keys: Vec<String> = rt.elements().into_iter().filter_map(|element| element.key).collect();
        assert!(keys.iter().any(|key| key == "queue-list-rows"), "{keys:?}");
        let rows = keys.iter().filter(|key| key.starts_with("queue-row-")).count();
        assert_eq!(rows, 1, "the now-playing row above the list is the only fixed one");
        let upcoming = keys.iter().filter(|key| key.starts_with("track-") && !key.starts_with("track-menu-")).count();
        assert!(upcoming > 1 && upcoming < 60, "{upcoming} rows of a 2,000-track queue");
        // A queue row keeps its own actions: remove and the "…" menu.
        assert!(keys.iter().any(|key| key.starts_with("queue-remove-")), "{keys:?}");
        assert!(keys.iter().any(|key| key.starts_with("queue-track-menu-")), "{keys:?}");
    }
}
