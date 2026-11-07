//! Settings → Local files: the music folders the scanner walks, a real folder picker, and the
//! scan status. Every control is wired: Add validates the path and rescans, Browse opens Rune's
//! native folder dialog (the XDG FileChooser portal on Linux), Remove drops a folder, and
//! Rescan re-walks the configured ones.

use rune::{El, FileDialogOptions, TextStyle, col, el, row, text, text_input};

use crate::app::{SearchState, Ui};
use crate::theme::{BODY, CAPS, SMALL, Palette};
use crate::ui::settings::card;
use crate::widgets::secondary_button;

/// The Settings page's Local files card.
pub(crate) fn local_card(s: &Ui, p: Palette) -> El {
    let mut card = card(p)
        .child(text("Local files").font(CAPS).color(p.faint))
        .child(
            text("Music folders scanned for your own FLAC, MP3, M4A/AAC, Vorbis, WAV and AIFF files.")
                .font(SMALL)
                .color(p.faint),
        );

    let mut folders = col().w_full().gap(2.0).pt(6.0);
    if s.settings.local_folders.is_empty() {
        folders = folders.child(
            text("No folders yet — add one below or use Browse…").font(SMALL).color(p.dim).py(4.0),
        );
    }
    for folder in &s.settings.local_folders {
        let key = sanitise(folder);
        let remove = folder.clone();
        folders = folders.child(
            row().key(format!("local-folder-{key}")).w_full().h(36.0).px(10.0).gap(10.0)
                .center_y().radius(6.0).hover(p.card_hover)
                .child(el().grow(1.0).min_w(0.0).child(
                    text(folder.clone()).font(BODY).color(p.text).truncate(),
                ))
                .child(
                    el().key(format!("local-folder-remove-{key}")).role("button")
                        .label(format!("Remove {folder}"))
                        .h(26.0).px(10.0).center().radius(13.0).hover(p.card_hover)
                        .focusable(true)
                        .on_press(move |s: &mut Ui| s.remove_local_folder(&remove))
                        .child(text("Remove").font(SMALL).color(p.dim)),
                ),
        );
    }
    card = card.child(folders);

    // Add: type or paste a path, or pick one in the native folder dialog.
    let add = row().w_full().gap(8.0).center_y().pt(6.0)
        .child(
            row().grow(1.0).min_w(0.0).h(34.0).px(12.0).center_y().radius(17.0).fill(p.sidebar)
                .child(text_input(&s.local_add_path, TextStyle::new(13.0, 400.0))
                    .key("local-folder-input")
                    .single_line()
                    .grow(1.0)
                    .color(p.text)
                    .placeholder("/path/to/music", p.faint)
                    .on_edit(|s: &mut Ui| &mut s.local_add_path)),
        )
        .child(
            el().key("local-folder-add").role("button").label("Add folder")
                .h(34.0).px(14.0).center().radius(17.0).fill(p.card_hover)
                .hover(p.card).focusable(true)
                .on_press(|s: &mut Ui| s.add_local_folder())
                .child(text("Add").font(BODY).color(p.text)),
        )
        .child(
            el().key("local-folder-browse").role("button").label("Browse for a music folder")
                .h(34.0).px(14.0).center().radius(17.0).fill(p.card_hover)
                .hover(p.card).focusable(true)
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
        );
    card = card.child(add);

    // Status and Rescan.
    let status = match &s.local_state {
        SearchState::Searching => match s.local_progress {
            Some((done, total)) if total > 0 => format!("Scanning {done} of {total} files…"),
            _ => "Scanning…".to_owned(),
        },
        SearchState::Failed(error) => format!("Scan failed: {error}"),
        SearchState::Done | SearchState::Idle => {
            if s.settings.local_folders.is_empty() {
                "No folders configured.".to_owned()
            } else if s.local.is_empty() {
                "No music files found yet.".to_owned()
            } else {
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
                )
            }
        }
    };
    let rescan = !s.settings.local_folders.is_empty();
    card = card.child(
        row().w_full().gap(10.0).center_y().pt(6.0)
            .child(text(status).font(SMALL).color(p.dim).grow(1.0).wrap_anywhere())
            .child(secondary_button(p, "Rescan", rescan, |s: &mut Ui, _: &mut rune::Tx| {
                s.start_local_scan()
            })),
    );
    card
}

/// Turns a path into something usable as an element key.
fn sanitise(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
