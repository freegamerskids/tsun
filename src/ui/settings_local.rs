//! Settings → Local files: the music folders the scanner walks, a real folder picker, and the
//! scan status. Every control is wired: Add validates the path and rescans, Browse opens Rune's
//! native folder dialog (the XDG FileChooser portal on Linux), Remove drops a folder, and
//! Rescan re-walks the configured ones.

use rune::{El, FileDialogOptions, TextStyle, col, el, icon, row, text, text_input};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, ICON_FOLDER, ICON_PLUS, ICON_TRASH, SMALL, TRACK, Palette};
use crate::ui::settings_widgets::{self as w, Tone};
use crate::widgets::secondary_button;

/// The Settings page's Local files card.
pub(crate) fn local_card(s: &Ui, p: Palette) -> El {
    let (status, tone) = scan_status(s);
    let rescan = !s.settings.local_folders.is_empty();
    let mut card = w::card(p)
        .key("local-card")
        .child(
            row()
                .w_full()
                .padding(14.0)
                .gap(16.0)
                .center_y()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .shrink()
                        .gap(3.0)
                        .child(text("Music folders").font(TRACK).color(p.text))
                        .child(
                            text("Scanned for your own FLAC, MP3, M4A/AAC, Vorbis, WAV and AIFF files.")
                                .font(SMALL)
                                .color(p.dim)
                                .wrap(),
                        ),
                )
                .child(secondary_button(p, "Rescan", rescan, |s: &mut Ui, _: &mut rune::Tx| {
                    s.start_local_scan()
                })),
        )
        .child(w::divider(p));

    let mut rows = col().w_full();
    if s.settings.local_folders.is_empty() {
        rows = rows.child(
            row()
                .w_full()
                .padding(14.0)
                .child(
                    text("No folders yet — add one below or use Browse…")
                        .font(SMALL)
                        .color(p.dim)
                        .wrap(),
                ),
        );
    }
    for (index, folder) in s.settings.local_folders.iter().enumerate() {
        if index > 0 {
            rows = rows.child(w::divider(p));
        }
        let key = sanitise(folder);
        let remove = folder.clone();
        rows = rows.child(
            row()
                .key(format!("local-folder-{key}"))
                .w_full()
                .padding(14.0)
                .gap(12.0)
                .center_y()
                .radius(8.0)
                .hover(p.card_hover)
                .child(icon(&ICON_FOLDER, 16.0).color(p.dim))
                .child(
                    el()
                        .grow(1.0)
                        .min_w(0.0)
                        .shrink()
                        .child(text(folder.clone()).font(BODY).color(p.text).truncate_middle()),
                )
                .child(
                    el()
                        .key(format!("local-folder-remove-{key}"))
                        .role("button")
                        .label(format!("Remove {folder}"))
                        .h(28.0)
                        .px(10.0)
                        .center()
                        .radius(14.0)
                        .hover(p.card_hover)
                        .focusable(true)
                        .on_press(move |s: &mut Ui| s.remove_local_folder(&remove))
                        .child(
                            row()
                                .gap(6.0)
                                .center_y()
                                .child(icon(&ICON_TRASH, 13.0).color(p.dim))
                                .child(text("Remove").font(SMALL).color(p.dim)),
                        ),
                ),
        );
    }
    card = card.child(rows).child(w::divider(p));

    // Add: type or paste a path, or pick one in the native folder dialog.
    let add = row()
        .w_full()
        .padding(14.0)
        .gap(8.0)
        .center_y()
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .shrink()
                .h(40.0)
                .px(12.0)
                .center_y()
                .radius(8.0)
                .fill(w::recess(p))
                .border(1.0, p.border)
                .child(
                    text_input(&s.local_add_path, TextStyle::new(13.0, 400.0))
                        .key("local-folder-input")
                        .single_line()
                        .grow(1.0)
                        .color(p.text)
                        .placeholder("/path/to/music", p.faint)
                        .on_edit(|s: &mut Ui| &mut s.local_add_path),
                ),
        )
        .child(
            el()
                .key("local-folder-browse")
                .role("button")
                .label("Browse for a music folder")
                .h(36.0)
                .px(14.0)
                .center()
                .radius(18.0)
                .fill(p.card)
                .border(1.0, p.border)
                .hover(p.card_hover)
                .focusable(true)
                .on_press_tx(|_: &mut Ui, tx: &mut rune::Tx| {
                    let options = FileDialogOptions {
                        title: "Add a music folder".to_owned(),
                        ..Default::default()
                    };
                    tx.open_folder(options, |s: &mut Ui, result, _: &mut rune::Tx| match result {
                        Ok(Some(path)) => s.add_local_folder_path(path),
                        Ok(None) => {}
                        Err(error) => {
                            s.notice = Some(crate::app::Notice::error(format!(
                                "The folder chooser is unavailable: {error}"
                            )));
                        }
                    });
                })
                .child(text("Browse…").font(BODY).color(p.text)),
        )
        .child(
            el()
                .key("local-folder-add")
                .role("button")
                .label("Add folder")
                .h(36.0)
                .px(16.0)
                .center()
                .radius(18.0)
                .fill(p.accent)
                .hover(p.accent.mix(rune::Color::WHITE, 0.12))
                .press(p.accent.mix(rune::Color::BLACK, 0.15))
                .focusable(true)
                .on_press(|s: &mut Ui| s.add_local_folder())
                .child(
                    row()
                        .gap(6.0)
                        .center_y()
                        .child(icon(&ICON_PLUS, 13.0).color(p.accent_ink))
                        .child(text("Add").font(BODY).color(p.accent_ink)),
                ),
        );
    card = card
        .child(add)
        .child(w::divider(p))
        .child(
            row()
                .w_full()
                .padding(14.0)
                .child(w::note(w::tone_color(tone, p), &status)),
        );
    card
}

/// The scan's state as a line and the tone it draws in.
fn scan_status(s: &Ui) -> (String, Tone) {
    match &s.local_state {
        SearchState::Searching => (
            match s.local_progress {
                Some((done, total)) if total > 0 => format!("Scanning {done} of {total} files…"),
                _ => "Scanning…".to_owned(),
            },
            Tone::Warn,
        ),
        SearchState::Failed(error) => (format!("Scan failed: {error}"), Tone::Error),
        SearchState::Done | SearchState::Idle => {
            if s.settings.local_folders.is_empty() {
                ("No folders configured yet.".to_owned(), Tone::Off)
            } else if s.local.is_empty() {
                ("No music files found yet.".to_owned(), Tone::Off)
            } else {
                (
                    format!(
                        "{} tracks · {} albums · {} artists{}",
                        s.local.tracks.len(),
                        s.local.albums.len(),
                        s.local.artists.len(),
                        if s.local_skipped > 0 {
                            format!(" · {} unreadable", s.local_skipped)
                        } else {
                            String::new()
                        }
                    ),
                    Tone::Ok,
                )
            }
        }
    }
}

/// Turns a path into something usable as an element key.
fn sanitise(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
