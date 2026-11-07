//! The Settings page: streaming quality, volume normalization, the output device, desktop
//! notifications and signing out. Every control is backed by real behaviour — the quality
//! preference drives what the client requests, normalization applies the stream's reported
//! ReplayGain to the audio output, and the device list is the host's real enumeration (the
//! chosen name is handed to `cpal`, so a raw `hw:` device means an exclusive path).

use rune::{Color, El, ScrollSpec, col, el, icon, oklch, row, text};

use crate::app::Ui;
use crate::settings::StartupPage;
use crate::theme::{BODY, CAPS, H1, ICON_SETTINGS, SMALL, TRACK, Palette};
use crate::tidal::Quality;
use crate::widgets::secondary_button;

/// The Settings page body.
pub(crate) fn settings_content(s: &Ui, p: Palette) -> El {
    let mut column = col().w_full().gap(18.0).pb(32.0)
        .child(text("Settings").font(H1).color(p.text))
        .child(
            card(p)
                .child(section("Streaming quality", p))
                .child(text("The requested tier; TIDAL may serve a lower one.").font(SMALL).color(p.faint))
                .child(
                    col().w_full().gap(2.0).pt(6.0)
                        .child(quality_row(s, p, Quality::Low, "Low"))
                        .child(quality_row(s, p, Quality::High, "High"))
                        .child(quality_row(s, p, Quality::Lossless, "Lossless"))
                        .child(quality_row(s, p, Quality::HiResLossless, "Max (Hi-Res)")),
                ),
        )
        .child(
            card(p)
                .child(section("Playback", p))
                .child(toggle_row(
                    p,
                    "volume-normalization",
                    "Volume normalization",
                    "Apply each track's ReplayGain so albums keep a consistent loudness.",
                    s.settings.normalize,
                    |s: &mut Ui| s.set_normalize(!s.settings.normalize),
                ))
                .child(toggle_row(
                    p,
                    "volume-scroll",
                    "Adjust volume with the mouse wheel",
                    "Scroll over the volume control to change the volume in 5% steps (1% with Shift). Off, scrolling there never changes it.",
                    s.settings.volume_scroll,
                    |s: &mut Ui| s.set_volume_scroll(!s.settings.volume_scroll),
                ))
                .child(toggle_row(
                    p,
                    "desktop-notifications",
                    "Desktop notifications",
                    "Post a notification with the cover art when the track changes.",
                    s.settings.notifications,
                    |s: &mut Ui| s.set_notifications(!s.settings.notifications),
                )),
        )
        .child(crate::ui::settings_local::local_card(s, p))
        .child(startup_card(s, p))
        .child(crate::ui::settings_discord::discord_card(s, p))
        .child(crate::ui::settings_lastfm::lastfm_card(s, p))
        .child(output_card(s, p))
        .child(
            card(p)
                .child(section("Account", p))
                .child(
                    el().key("settings-sign-out").role("button").label("Sign out")
                        .w_full().h(44.0).px(14.0).center_y().radius(6.0)
                        .fill(Color::TRANSPARENT).hover(p.card_hover).focusable(true)
                        .on_press(|s: &mut Ui| s.sign_out())
                        .child(text("Sign out").font(BODY).color(p.danger)),
                ),
        );
    if s.tray_available {
        column = column.child(tray_card(s, p));
    }
    column = column.child(
        row().gap(8.0).center_y()
            .child(icon(&ICON_SETTINGS, 14.0).color(p.faint))
            .child(text("Settings are saved to ~/.config/tsun/settings.json").font(SMALL).color(p.faint)),
    );
    col().w_full().h_full().px(32.0).pt(24.0).min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(column)
}

/// The startup-page card: which page the app opens after sign-in.
fn startup_card(s: &Ui, p: Palette) -> El {
    let mut rows = col().w_full().gap(2.0).pt(6.0);
    for page in StartupPage::ALL {
        rows = rows.child(radio_row(
            p,
            &format!("startup-{}", page.key()),
            page.label(),
            s.settings.startup_page == page,
            move |s: &mut Ui| s.set_startup_page(page),
        ));
    }
    card(p)
        .child(section("Startup page", p))
        .child(text("Which page opens when tsun starts.").font(SMALL).color(p.faint))
        .child(rows)
}

/// The tray card, shown only when a status-notifier tray could be created.
fn tray_card(s: &Ui, p: Palette) -> El {
    card(p)
        .child(section("Tray", p))
        .child(toggle_row(
            p,
            "close-to-tray",
            "Close to tray",
            "Closing the window keeps tsun running (and playing) in the tray; the tray's Quit exits.",
            s.settings.close_to_tray,
            |s: &mut Ui| s.set_close_to_tray(!s.settings.close_to_tray),
        ))
}

pub(crate) fn card(p: Palette) -> El {
    col().w_full().max_w(620.0).padding(16.0).gap(6.0).radius(10.0)
        .fill(p.card).border(1.0, p.border)
}

/// The output device card: the real device list, the format the stream is running at, and a
/// Refresh button for devices that came or went since the page was opened.
fn output_card(s: &Ui, p: Palette) -> El {
    let selected = s.settings.output_device.as_deref();
    let mut rows = col().w_full().gap(2.0).pt(6.0);
    rows = rows.child(device_row(
        p,
        "output-system-default",
        "System default",
        selected.is_none(),
        None,
    ));
    let mut listed = false;
    for (name, is_default) in s.output_devices.as_deref().unwrap_or_default() {
        let label = if *is_default {
            format!("{name} (system default)")
        } else {
            name.clone()
        };
        rows = rows.child(device_row(
            p,
            &format!("output-device-{}", sanitise(name)),
            &label,
            selected == Some(name.as_str()),
            Some(name.clone()),
        ));
        listed |= selected == Some(name.as_str());
    }
    // The chosen device disappeared: say so instead of silently falling back.
    if let Some(selected) = selected {
        if !listed {
            rows = rows.child(
                row()
                    .key("output-missing")
                    .w_full()
                    .h(44.0)
                    .px(10.0)
                    .gap(12.0)
                    .center_y()
                    .radius(6.0)
                    .border(1.0, p.danger.alpha(0.4))
                    .child(
                        col().grow(1.0).min_w(0.0).gap(2.0)
                            .child(text(format!("{selected} (unavailable)")).font(BODY).color(p.danger))
                            .child(text("This device is no longer present; choose another one.").font(SMALL).color(p.dim)),
                    ),
            );
        }
    }

    let mut card_el = card(p)
        .child(section("Output device", p))
        .child(
            text("Opened at the track's own sample rate when the device supports it, so lossless audio stays lossless. A raw ALSA hw: device here gives an exclusive path.")
                .font(SMALL)
                .color(p.faint),
        )
        .child(rows);
    if let Some(output) = s.now_playing.as_ref().and_then(|track| track.output.as_deref()) {
        card_el = card_el.child(
            text(format!("Output: {output}"))
                .font(SMALL)
                .color(oklch(0.78, 0.13, 170.0))
                .wrap_anywhere(),
        );
    }
    card_el.child(
        row().pt(4.0).gap(8.0).center_y()
            .child(secondary_button(p, "Refresh devices", true, |s: &mut Ui, _: &mut rune::Tx| {
                s.refresh_output_devices()
            }))
            .child(
                text(match s.output_devices.as_deref() {
                    Some([]) => "No output device is available.".to_owned(),
                    Some(devices) => format!("{} device(s) found.", devices.len()),
                    None => String::new(),
                })
                .font(SMALL)
                .color(p.faint),
            ),
    )
}

/// One output-device radio row. `name` is `None` for the system default.
fn device_row(p: Palette, key: &str, title: &str, selected: bool, name: Option<String>) -> El {
    radio_row(p, key, title, selected, move |s: &mut Ui| {
        s.set_output_device(name.clone());
    })
}

/// Turns a device name into something usable as an element key.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
        .collect()
}

fn section(title: &str, p: Palette) -> El {
    text(title).font(CAPS).color(p.faint)
}

/// One radio row for the streaming-quality preference.
fn quality_row(s: &Ui, p: Palette, quality: Quality, label: &str) -> El {
    radio_row(p, &format!("quality-{}", quality.as_str().to_lowercase()), label, s.settings.quality == quality, move |s: &mut Ui| {
        s.set_quality(quality);
    })
}

/// A boolean row with a check that flips when the row is pressed.
fn toggle_row(
    p: Palette,
    key: &str,
    title: &str,
    detail: &str,
    on: bool,
    on_press: impl Fn(&mut Ui) + 'static,
) -> El {
    let row = row()
        .key(key)
        .role("button")
        .label(title)
        .w_full()
        .padding(10.0)
        .gap(12.0)
        .center_y()
        .radius(6.0)
        .fill(Color::TRANSPARENT)
        .hover(p.card_hover)
        .focusable(true)
        .on_press(on_press)
        .child(
            col().grow(1.0).min_w(0.0).gap(2.0)
                .child(text(title).font(TRACK).color(p.text))
                .child(text(detail).font(SMALL).color(p.dim).wrap_anywhere()),
        )
        .child(check(p, on));
    row
}

/// A radio row: a title plus a check drawn when selected.
fn radio_row(
    p: Palette,
    key: &str,
    title: &str,
    selected: bool,
    on_press: impl Fn(&mut Ui) + 'static,
) -> El {
    row()
        .key(key)
        .role("button")
        .label(title)
        .w_full()
        .h(44.0)
        .px(10.0)
        .gap(12.0)
        .center_y()
        .radius(6.0)
        .fill(if selected { p.card_hover } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .on_press(on_press)
        .child(text(title).font(BODY).color(if selected { p.text } else { p.dim }).grow(1.0))
        .child(check(p, selected))
}

/// A check mark, bright when `on`.
fn check(p: Palette, on: bool) -> El {
    el().size(20.0, 20.0).center().radius(10.0)
        .fill(if on { p.accent } else { Color::TRANSPARENT })
        .border(1.0, if on { p.accent } else { p.border })
        .child(icon(&crate::theme::ICON_CHECK, 12.0).color(if on { p.accent_ink } else { p.faint }))
}
