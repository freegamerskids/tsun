//! Modal dialogs: creating and editing a playlist, confirming its deletion, and a track's
//! info and credits.
//!
//! A dialog is a centred card over a scrim. The scrim is drawn as a sibling by
//! [`crate::ui::view`] (as the profile menu's is), so a press outside the card dismisses and a
//! press inside it stays with the card.

use rune::{El, ScrollSpec, TextStyle, Tx, col, el, icon, row, text, text_input};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, CAPS, H2, ICON_CLOSE, Palette, SMALL, SUBTITLE};
use crate::tidal::{Credit, Track};
use crate::util::{format_duration, quality_label};
use crate::widgets::secondary_button;

/// An open dialog.
pub(crate) enum Dialog {
    /// The create form. `tracks` are added to the new playlist once it exists, so the
    /// "New playlist…" item of a context menu creates and fills in one action.
    CreatePlaylist { tracks: Vec<u64> },
    /// The rename/re-describe form for one of the user's own playlists.
    EditPlaylist { id: String },
    /// The delete confirmation for one of the user's own playlists.
    ConfirmDelete { id: String, title: String },
    /// A track's metadata and credits.
    TrackInfo(TrackInfo),
}

/// A track-info dialog's data: the track, and its credits as they load.
pub(crate) struct TrackInfo {
    pub(crate) track: Track,
    pub(crate) state: SearchState,
    pub(crate) credits: Vec<Credit>,
}

/// The centred card for the open dialog.
pub(crate) fn dialog(s: &Ui, p: Palette) -> El {
    let Some(dialog) = &s.dialog else { return el() };
    match dialog {
        Dialog::CreatePlaylist { .. } => playlist_form(s, p, "New playlist", "Create", "playlist-dialog-submit"),
        Dialog::EditPlaylist { .. } => playlist_form(s, p, "Edit playlist", "Save", "playlist-dialog-submit"),
        Dialog::ConfirmDelete { title, .. } => confirm_delete(p, title),
        Dialog::TrackInfo(info) => track_info(p, info),
    }
}

/// The shared card shell: a title, the body and a footer row of buttons.
fn card(p: Palette, key: &str, title: &str, body: El, footer: El) -> El {
    col()
        .key(key.to_owned())
        .w(440.0)
        .gap(16.0)
        .padding(24.0)
        .radius(16.0)
        .fill(p.card)
        .border(1.0, p.border)
        .shadow(0.0, 40.0, rune::Color::BLACK.alpha(0.45))
        .on_press(|_: &mut Ui| {})
        .child(
            row().w_full().gap(12.0).center_y()
                .child(text(title.to_owned()).font(H2).color(p.text).grow(1.0))
                .child(
                    el().key(format!("{key}-close")).role("button").label("Close")
                        .size(28.0, 28.0).center().radius(14.0).hover(p.card_hover).focusable(true)
                        .on_press(|s: &mut Ui| s.close_dialog())
                        .child(icon(&ICON_CLOSE, 14.0).color(p.dim)),
                ),
        )
        .child(body)
        .child(footer)
}

/// The create/rename form: title and description fields.
fn playlist_form(s: &Ui, p: Palette, title: &str, submit: &str, submit_key: &str) -> El {
    let can_submit = !s.playlist_form_title.text.trim().is_empty();
    let body = col().w_full().gap(12.0)
        .child(field_label("Title", p))
        .child(
            row().w_full().h(40.0).px(12.0).radius(8.0).fill(p.bg).border(1.0, p.border).center_y()
                .child(
                    text_input(&s.playlist_form_title, TextStyle::new(14.0, 400.0))
                        .key("playlist-dialog-title")
                        .single_line()
                        .grow(1.0)
                        .color(p.text)
                        .placeholder("Playlist name", p.faint)
                        .on_edit(|s: &mut Ui| &mut s.playlist_form_title),
                ),
        )
        .child(field_label("Description", p))
        .child(
            row().w_full().h(40.0).px(12.0).radius(8.0).fill(p.bg).border(1.0, p.border).center_y()
                .child(
                    text_input(&s.playlist_form_desc, TextStyle::new(14.0, 400.0))
                        .key("playlist-dialog-description")
                        .single_line()
                        .grow(1.0)
                        .color(p.text)
                        .placeholder("Optional", p.faint)
                        .on_edit(|s: &mut Ui| &mut s.playlist_form_desc),
                ),
        );
    let footer = row().w_full().gap(12.0).items_end()
        .child(el().grow(1.0))
        .child(secondary_button(p, "Cancel", true, |s: &mut Ui, _: &mut Tx| s.close_dialog()))
        .child(primary(p, submit, can_submit, submit_key, |s: &mut Ui| s.submit_playlist_form()));
    card(p, "playlist-dialog", title, body, footer)
}

/// The delete confirmation.
fn confirm_delete(p: Palette, title: &str) -> El {
    let body = col().w_full().gap(12.0)
        .child(
            text(format!("Delete “{title}”?"))
                .font(SUBTITLE)
                .color(p.text)
                .wrap_anywhere(),
        )
        .child(
            text("This removes it from your TIDAL account. It cannot be undone.")
                .font(BODY)
                .color(p.dim)
                .wrap_anywhere(),
        );
    let footer = row().w_full().gap(12.0).items_end()
        .child(el().grow(1.0))
        .child(secondary_button(p, "Cancel", true, |s: &mut Ui, _: &mut Tx| s.close_dialog()))
        .child(danger_button(p, "Delete", true, "delete-dialog-confirm", |s: &mut Ui| s.confirm_delete_playlist()));
    card(p, "delete-dialog", "Delete playlist", body, footer)
}

/// A track's metadata and, below it, its credits.
fn track_info(p: Palette, info: &TrackInfo) -> El {
    let track = &info.track;
    let dash = "—".to_owned();
    let body = col().w_full().gap(8.0)
        .child(text(track.title.clone()).font(SUBTITLE).color(p.text).w_full().wrap_anywhere())
        .child(field(p, "Artist", if track.artist.is_empty() { dash.clone() } else { track.artist.clone() }))
        .child(field(p, "Album", if track.album.is_empty() { dash.clone() } else { track.album.clone() }))
        .child(field(
            p,
            "Track",
            track.track_number.map(|number| number.to_string()).unwrap_or_else(|| dash.clone()),
        ))
        .child(field(
            p,
            "Duration",
            track.duration.map(format_duration).unwrap_or_else(|| dash.clone()),
        ))
        .child(field(
            p,
            "Quality",
            quality_label(track.quality.as_deref()).unwrap_or_else(|| dash.clone()),
        ))
        .child(el().w_full().h(1.0).my(6.0).fill(p.border))
        .child(text("Credits").font(CAPS).color(p.faint));
    let credits = match &info.state {
        SearchState::Idle | SearchState::Searching => {
            col().w_full().child(text("Loading credits…").font(SMALL).color(p.dim))
        }
        SearchState::Failed(_) => {
            col().w_full().child(text("TIDAL credits are unavailable for this track.").font(SMALL).color(p.dim).wrap_anywhere())
        }
        SearchState::Done if info.credits.is_empty() => {
            col().w_full().child(text("TIDAL lists no credits for this track.").font(SMALL).color(p.dim).wrap_anywhere())
        }
        SearchState::Done => {
            let mut list = col().w_full().gap(10.0);
            for credit in &info.credits {
                list = list.child(
                    col().w_full().gap(2.0)
                        .child(text(credit.role.clone()).font(SMALL).color(p.faint))
                        .child(text(credit.names.join(", ")).font(BODY).color(p.text).wrap_anywhere()),
                );
            }
            col().w_full().max_h(220.0).shrink().min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(list)
        }
    };
    card(
        p,
        "track-info-dialog",
        "Track info",
        body.child(credits),
        row().w_full().items_end()
            .child(el().grow(1.0))
            .child(secondary_button(p, "Close", true, |s: &mut Ui, _: &mut Tx| s.close_dialog())),
    )
}

/// A `Label    value` row.
fn field(p: Palette, label: &str, value: String) -> El {
    row().w_full().gap(12.0).items_start()
        .child(text(label.to_owned()).font(SMALL).color(p.faint).w(78.0))
        .child(text(value).font(BODY).color(p.text).grow(1.0).min_w(0.0).wrap_anywhere())
}

/// The small caps label above a form field.
fn field_label(label: &str, p: Palette) -> El {
    text(label.to_owned()).font(CAPS).color(p.faint).w_full()
}

/// A filled button; `danger` tints it red for destructive confirmations.
fn primary(
    p: Palette,
    label: &str,
    enabled: bool,
    key: &str,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    let mut button = el()
        .key(key.to_owned())
        .role("button")
        .label(label.to_owned())
        .h(40.0)
        .px(22.0)
        .center()
        .radius(20.0)
        .fill(if enabled { p.accent } else { p.card })
        .hover(if enabled { p.accent.mix(rune::Color::WHITE, 0.12) } else { p.card })
        .press(if enabled { p.accent.mix(rune::Color::BLACK, 0.15) } else { p.card })
        .focusable(enabled)
        .disabled(!enabled)
        .child(text(label.to_owned()).font(crate::theme::BUTTON).color(if enabled { p.accent_ink } else { p.faint }));
    if enabled {
        button = button.on_press(on);
    }
    button
}

/// The red, destructive counterpart of [`primary`], used for "Delete".
fn danger_button(
    p: Palette,
    label: &str,
    enabled: bool,
    key: &str,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    let mut button = el()
        .key(key.to_owned())
        .role("button")
        .label(label.to_owned())
        .h(40.0)
        .px(22.0)
        .center()
        .radius(20.0)
        .fill(if enabled { p.danger } else { p.card })
        .hover(if enabled { p.danger.mix(rune::Color::WHITE, 0.12) } else { p.card })
        .press(if enabled { p.danger.mix(rune::Color::BLACK, 0.15) } else { p.card })
        .focusable(enabled)
        .disabled(!enabled)
        .child(text(label.to_owned()).font(crate::theme::BUTTON).color(if enabled { p.accent_ink } else { p.faint }));
    if enabled {
        button = button.on_press(on);
    }
    button
}
