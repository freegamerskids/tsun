//! The Settings page: a category navigation beside a content column of headed groups.
//!
//! Every control is backed by real behaviour — the quality preference drives what the client
//! requests, normalization applies the stream's reported ReplayGain to the audio output, and
//! the device list is the host's real enumeration (the chosen name is handed to `cpal`, so a
//! raw `hw:` device means an exclusive path). The page only changes how those controls are
//! presented: the shared controls in [`crate::ui::settings_widgets`] draw them.

use rune::{Color, El, Icon, ScrollSpec, col, el, icon, row, text};

use crate::app::Ui;
use crate::settings::StartupPage;
use crate::theme::{
    BODY, BUTTON, H1, ICON_CHECK, ICON_FOLDER, ICON_LINK, ICON_PLAY, ICON_PROFILE, ICON_SETTINGS,
    ICON_VOLUME_HIGH, SMALL, TRACK, Palette,
};
use crate::tidal::Quality;
use crate::ui::settings_widgets::{self as w, Tone};
use crate::widgets::secondary_button;

/// The categories the Settings page is split into. The page shows one at a time; the
/// navigation switches between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum SettingsSection {
    #[default]
    General,
    Playback,
    Audio,
    Library,
    Integrations,
    Account,
}

impl SettingsSection {
    /// Every category, in the order the navigation lists them.
    pub(crate) const ALL: [SettingsSection; 6] = [
        SettingsSection::General,
        SettingsSection::Playback,
        SettingsSection::Audio,
        SettingsSection::Library,
        SettingsSection::Integrations,
        SettingsSection::Account,
    ];

    /// The element-key slug of the category.
    pub(crate) fn key(self) -> &'static str {
        match self {
            SettingsSection::General => "general",
            SettingsSection::Playback => "playback",
            SettingsSection::Audio => "audio",
            SettingsSection::Library => "library",
            SettingsSection::Integrations => "integrations",
            SettingsSection::Account => "account",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Playback => "Playback",
            SettingsSection::Audio => "Audio output",
            SettingsSection::Library => "Library",
            SettingsSection::Integrations => "Integrations",
            SettingsSection::Account => "Account",
        }
    }

    fn icon(self) -> &'static Icon {
        match self {
            SettingsSection::General => &ICON_SETTINGS,
            SettingsSection::Playback => &ICON_PLAY,
            SettingsSection::Audio => &ICON_VOLUME_HIGH,
            SettingsSection::Library => &ICON_FOLDER,
            SettingsSection::Integrations => &ICON_LINK,
            SettingsSection::Account => &ICON_PROFILE,
        }
    }
}

/// The Settings page body: the navigation and the selected category beside it, or the
/// navigation as a strip above it when the window is too narrow for both.
pub(crate) fn settings_content(s: &Ui, p: Palette) -> El {
    if s.settings_nav_wide.get() {
        row()
            .key("settings-page")
            .w_full()
            .h_full()
            .min_h(0.0)
            .child(nav_column(s, p))
            .child(content_area(s, p))
    } else {
        col()
            .key("settings-page")
            .w_full()
            .h_full()
            .min_h(0.0)
            .child(
                row()
                    .w_full()
                    .px(28.0)
                    .pt(20.0)
                    .pb(12.0)
                    .child(text("Settings").font(H1).color(p.text)),
            )
            .child(nav_strip(s, p))
            .child(content_area(s, p))
    }
}

/// The left-hand category navigation, with the page title above it.
fn nav_column(s: &Ui, p: Palette) -> El {
    let current = s.settings_section.get();
    let mut nav = col()
        .key("settings-nav")
        .w(256.0)
        .h_full()
        .min_h(0.0)
        // A margin on the left as well, so the highlighted item does not sit against the app
        // sidebar's edge.
        .pl(20.0)
        .pr(12.0)
        .pb(24.0)
        .gap(2.0)
        .child(text("Settings").font(H1).color(p.text).px(12.0).pb(14.0));
    for section in SettingsSection::ALL {
        nav = nav.child(
            w::nav_item(
                p,
                &format!("settings-nav-{}", section.key()),
                section.label(),
                section.icon(),
                current == section,
            )
            .on_press(move |s: &mut Ui| open_section(s, section)),
        );
    }
    nav
}

/// The category strip the page shows instead of the side navigation when it is narrow.
fn nav_strip(s: &Ui, p: Palette) -> El {
    let current = s.settings_section.get();
    let mut strip = row().w_full().px(28.0).pb(8.0).gap(6.0).flex_wrap();
    for section in SettingsSection::ALL {
        strip = strip.child(
            w::nav_chip(
                p,
                &format!("settings-nav-{}", section.key()),
                section.label(),
                current == section,
            )
            .on_press(move |s: &mut Ui| open_section(s, section)),
        );
    }
    strip
}

/// Switches the shown category, closing any open output dropdown with it.
fn open_section(s: &mut Ui, section: SettingsSection) {
    if s.settings_section.get() == section {
        return;
    }
    s.settings_section.set(section);
    s.settings_output_open = false;
}

/// The scrolling content column: one category's groups, centred and never wider than a
/// comfortable reading width.
fn content_area(s: &Ui, p: Palette) -> El {
    let section = s.settings_section.get();
    col()
        .key(format!("settings-content-{}", section.key()))
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false })
        .child(
            row()
                .w_full()
                .justify_center()
                .px(32.0)
                .pt(6.0)
                .pb(40.0)
                .child(
                    col()
                        .w_full()
                        .max_w(720.0)
                        .min_w(0.0)
                        .shrink()
                        .gap(26.0)
                        .child(section_body(s, p))
                        .child(footer(p)),
                ),
        )
}

/// The blocks of the category the page is showing.
fn section_body(s: &Ui, p: Palette) -> El {
    match s.settings_section.get() {
        SettingsSection::General => general_body(s, p),
        SettingsSection::Playback => playback_body(s, p),
        SettingsSection::Audio => audio_body(s, p),
        SettingsSection::Library => col().w_full().gap(26.0).child(crate::ui::settings_local::local_card(s, p)),
        SettingsSection::Integrations => col()
            .w_full()
            .gap(14.0)
            .child(crate::ui::settings_discord::discord_card(s, p))
            .child(crate::ui::settings_lastfm::lastfm_card(s, p)),
        SettingsSection::Account => account_body(s, p),
    }
}

/// Where the settings live, under every category.
fn footer(p: Palette) -> El {
    row()
        .gap(8.0)
        .center_y()
        .pl(2.0)
        .child(icon(&ICON_SETTINGS, 14.0).color(p.faint))
        .child(
            text("Settings are saved to ~/.config/tsun/settings.json")
                .font(SMALL)
                .color(p.faint),
        )
}

// ---------------------------------------------------------------------------------------------
// General

fn general_body(s: &Ui, p: Palette) -> El {
    let mut card = w::card(p)
        .child(
            w::setting_row(
                p,
                "startup-page",
                "Startup page",
                Some("Which page tsun opens after sign-in."),
                startup_control(s, p),
            ),
        )
        .child(w::divider(p))
        .child(w::toggle_row(
            p,
            "desktop-notifications",
            "Desktop notifications",
            Some("Post a notification with the cover art when the track changes."),
            s.settings.notifications,
            |s: &mut Ui| s.set_notifications(!s.settings.notifications),
        ));
    if s.tray_available {
        card = card.child(w::divider(p)).child(w::toggle_row(
            p,
            "close-to-tray",
            "Close to tray",
            Some("Closing the window keeps tsun running (and playing) in the tray; the tray's Quit exits."),
            s.settings.close_to_tray,
            |s: &mut Ui| s.set_close_to_tray(!s.settings.close_to_tray),
        ));
    }
    col()
        .w_full()
        .gap(26.0)
        .child(w::block(p, "General", None, card))
}

/// The startup-page segmented control.
fn startup_control(s: &Ui, p: Palette) -> El {
    let current = s.settings.startup_page;
    let items = StartupPage::ALL.into_iter().map(|page| {
        w::segment_item(
            p,
            &format!("startup-{}", page.key()),
            page.label(),
            current == page,
        )
        .on_press(move |s: &mut Ui| s.set_startup_page(page))
    });
    w::segmented(p, items).w(330.0)
}

// ---------------------------------------------------------------------------------------------
// Playback

fn playback_body(s: &Ui, p: Palette) -> El {
    let tiers = [
        (Quality::Low, "Low", "96 kbps AAC"),
        (Quality::High, "High", "320 kbps AAC"),
        (Quality::Lossless, "Lossless", "FLAC · 16-bit/44.1 kHz"),
        (Quality::HiResLossless, "Max (Hi-Res)", "FLAC · up to 24-bit/192 kHz"),
    ];
    let mut tiles = col().w_full().gap(10.0);
    for pair in tiers.chunks(2) {
        let mut line = row().w_full().gap(10.0);
        for (quality, name, detail) in pair {
            line = line.child(quality_tile(s, p, *quality, name, detail));
        }
        tiles = tiles.child(line);
    }
    let quality_card = w::card(p)
        .child(
            col()
                .w_full()
                .padding(10.0)
                .child(tiles),
        );
    let switches = w::card(p)
        .child(w::toggle_row(
            p,
            "volume-normalization",
            "Volume normalization",
            Some("Apply each track's ReplayGain so albums keep a consistent loudness."),
            s.settings.normalize,
            |s: &mut Ui| s.set_normalize(!s.settings.normalize),
        ))
        .child(w::divider(p))
        .child(w::toggle_row(
            p,
            "volume-scroll",
            "Adjust volume with the mouse wheel",
            Some("Scroll over the volume control to change the volume in 5% steps (1% with Shift). Off, scrolling there never changes it."),
            s.settings.volume_scroll,
            |s: &mut Ui| s.set_volume_scroll(!s.settings.volume_scroll),
        ));
    col()
        .w_full()
        .gap(26.0)
        .child(w::block(
            p,
            "Streaming quality",
            Some("The requested tier; TIDAL may serve a lower one."),
            quality_card,
        ))
        .child(w::block(p, "Playback", None, switches))
}

/// One streaming-quality option: its name and what it delivers, with the chosen one ringed in
/// the accent.
fn quality_tile(s: &Ui, p: Palette, quality: Quality, name: &str, detail: &str) -> El {
    let selected = s.settings.quality == quality;
    col()
        .key(format!("quality-{}", quality.as_str().to_lowercase()))
        .role("button")
        .label(format!("{name} — {detail}"))
        .grow(1.0)
        .min_w(0.0)
        .shrink()
        .padding(14.0)
        .gap(3.0)
        .radius(10.0)
        .fill(if selected { p.card_hover } else { w::recess(p) })
        .border(1.0, if selected { p.accent } else { p.border })
        .hover(p.card_hover)
        .focusable(true)
        .on_press(move |s: &mut Ui| s.set_quality(quality))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(text(name.to_owned()).font(TRACK).color(p.text).grow(1.0))
                .child(
                    icon(&ICON_CHECK, 14.0)
                        .color(if selected { p.accent } else { Color::TRANSPARENT }),
                ),
        )
        .child(
            text(detail.to_owned())
                .font(SMALL)
                .color(if selected { p.dim } else { p.faint })
                .wrap(),
        )
}

// ---------------------------------------------------------------------------------------------
// Audio output

fn audio_body(s: &Ui, p: Palette) -> El {
    let selected = s.settings.output_device.as_deref();
    let mut card = w::card(p).child(
        w::setting_row(
            p,
            "settings-output-row",
            "Playback device",
            Some("Applied immediately, at the position that was playing. A raw ALSA hw: device gives an exclusive path."),
            output_trigger(s, p),
        ),
    );
    if let Some(output) = s.now_playing.as_ref().and_then(|track| track.output.as_deref()) {
        card = card.child(w::divider(p)).child(
            row()
                .key("settings-output-live")
                .w_full()
                .padding(14.0)
                .child(w::note(w::tone_color(Tone::Ok, p), &format!("Output: {output}"))),
        );
    }
    if let Some(selected) = selected {
        if !device_is_listed(s, selected) {
            card = card.child(w::divider(p)).child(
                row()
                    .key("output-missing")
                    .w_full()
                    .padding(14.0)
                    .gap(10.0)
                    .center_y()
                    .child(
                        col()
                            .grow(1.0)
                            .min_w(0.0)
                            .shrink()
                            .gap(3.0)
                            .child(text(format!("{selected} is unavailable")).font(TRACK).color(p.danger).truncate())
                            .child(
                                text("This device is no longer present; choose another one.")
                                    .font(SMALL)
                                    .color(p.dim)
                                    .wrap(),
                            ),
                    ),
            );
        }
    }
    card = card.child(w::divider(p)).child(
        row()
            .w_full()
            .padding(14.0)
            .gap(12.0)
            .center_y()
            .child(w::note(
                p.dim,
                match s.output_devices.as_deref() {
                    Some([]) => "No output device is available.".to_owned(),
                    Some(devices) => format!("{} device(s) found.", devices.len()),
                    None => String::new(),
                }
                .as_str(),
            ))
            .child(
                secondary_button(p, "Refresh devices", true, |s: &mut Ui, _: &mut rune::Tx| {
                    s.refresh_output_devices()
                })
                .key("settings-output-refresh"),
            ),
    );
    col()
        .w_full()
        .gap(26.0)
        .child(w::block(p, "Output device", None, card))
}

/// Whether the chosen device is still in the host's list.
fn device_is_listed(s: &Ui, name: &str) -> bool {
    s.output_devices
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|(device, _)| device == name)
}

/// The dropdown's trigger: the device in use, with a chevron. Pressing it opens the menu.
fn output_trigger(s: &Ui, p: Palette) -> El {
    let label = match s.settings.output_device.as_deref() {
        None => "System default".to_owned(),
        Some(name) => friendly_device(name),
    };
    row()
        .key("settings-output-select")
        .role("button")
        .label("Choose an output device")
        .h(38.0)
        .px(14.0)
        .gap(8.0)
        .center_y()
        .radius(19.0)
        .fill(p.card_hover)
        .border(1.0, p.border)
        .hover(p.card)
        .focusable(true)
        .expanded(s.settings_output_open)
        .on_press(|s: &mut Ui| s.settings_output_open = !s.settings_output_open)
        .child(el().w(190.0).min_w(0.0).child(text(label).font(BODY).color(p.text).truncate()))
        .child(text("▾").font(SMALL).color(p.dim))
}

/// The output-device dropdown, drawn as an overlay anchored under its trigger (see `ui::view`).
pub(crate) fn output_menu(s: &Ui, p: Palette) -> El {
    let selected = s.settings.output_device.as_deref();
    let mut menu = col()
        .key("settings-output-menu")
        .role("menu")
        .w(440.0)
        .max_h(420.0)
        .min_h(0.0)
        .shrink()
        .padding(6.0)
        .gap(1.0)
        .radius(12.0)
        .fill(p.card)
        .border(1.0, p.border)
        .shadow(12.0, 32.0, Color::BLACK.alpha(0.55))
        .scroll(ScrollSpec { stick_bottom: false })
        .scroll_edge_fade(18.0)
        .child(device_option(
            p,
            "output-system-default",
            "System default",
            Some("Let the system pick the device"),
            selected.is_none(),
            None,
        ))
        .child(w::divider(p));
    for (name, is_default) in s.output_devices.as_deref().unwrap_or_default() {
        let friendly = friendly_device(name);
        let title = if *is_default && !friendly.to_ascii_lowercase().contains("default") {
            format!("{friendly} — system default")
        } else {
            friendly.clone()
        };
        let subtitle = (friendly != *name).then(|| name.clone());
        menu = menu.child(device_option(
            p,
            &format!("output-device-{}", sanitise(name)),
            &title,
            subtitle.as_deref(),
            selected == Some(name.as_str()),
            Some(name.clone()),
        ));
    }
    if matches!(s.output_devices.as_deref(), Some([])) {
        menu = menu.child(
            text("No output device is available.")
                .font(SMALL)
                .color(p.faint)
                .px(12.0)
                .py(10.0),
        );
    }
    if let Some(selected) = selected {
        if !device_is_listed(s, selected) {
            menu = menu.child(
                col()
                    .key("output-missing")
                    .w_full()
                    .gap(2.0)
                    .padding(12.0)
                    .radius(8.0)
                    .fill(p.danger.alpha(0.10))
                    .child(text(format!("{selected} is unavailable")).font(TRACK).color(p.danger))
                    .child(
                        text("This device is no longer present; choose another one.")
                            .font(SMALL)
                            .color(p.dim)
                            .wrap(),
                    ),
            );
        }
    }
    menu.child(
        row()
            .w_full()
            .padding(8.0)
            .child(
                secondary_button(p, "Refresh devices", true, |s: &mut Ui, _: &mut rune::Tx| {
                    s.refresh_output_devices()
                })
                .key("settings-output-refresh"),
            ),
    )
}

/// One device in the dropdown. `name` is `None` for the system default; pressing the row
/// chooses it and closes the menu.
fn device_option(
    p: Palette,
    key: &str,
    title: &str,
    subtitle: Option<&str>,
    selected: bool,
    name: Option<String>,
) -> El {
    let mut text_col = col()
        .grow(1.0)
        .min_w(0.0)
        .shrink()
        .gap(2.0)
        .child(
            text(title.to_owned())
                .font(TRACK)
                .color(if selected { p.text } else { p.dim })
                .truncate(),
        );
    if let Some(subtitle) = subtitle {
        text_col = text_col.child(text(subtitle.to_owned()).font(SMALL).color(p.faint).truncate());
    }
    row()
        .key(key)
        .role("menuitem")
        .label(title.to_owned())
        .w_full()
        .padding(10.0)
        .gap(10.0)
        .center_y()
        .radius(8.0)
        .fill(if selected { p.card_hover } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .on_press(move |s: &mut Ui| {
            s.set_output_device(name.clone());
            s.settings_output_open = false;
        })
        .child(text_col)
        .child(
            icon(&ICON_CHECK, 14.0)
                .color(if selected { p.accent } else { Color::TRANSPARENT }),
        )
}

/// A friendlier reading of the raw device names the host reports: `hw:CARD=PCH,DEV=0` reads
/// "PCH · hw 0", anything unrecognised stays as it is.
fn friendly_device(name: &str) -> String {
    if name == "default" {
        return "System default device".to_owned();
    }
    let Some((plugin, rest)) = name.split_once(':') else {
        return name.to_owned();
    };
    let Some(card) = rest.split(',').find_map(|part| part.strip_prefix("CARD=")) else {
        return name.to_owned();
    };
    let card = card.replace('_', " ");
    match rest.split(',').find_map(|part| part.strip_prefix("DEV=")) {
        Some(device) => format!("{card} · {plugin} {device}"),
        None => format!("{card} · {plugin}"),
    }
}

/// Turns a device name into something usable as an element key.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Account

fn account_body(s: &Ui, p: Palette) -> El {
    let who = s
        .profile
        .as_ref()
        .and_then(|profile| profile.name.clone())
        .unwrap_or_else(|| "your TIDAL account".to_owned());
    let card = w::card(p)
        .child(
            row()
                .w_full()
                .padding(14.0)
                .gap(14.0)
                .center_y()
                .child(
                    el()
                        .size(40.0, 40.0)
                        .radius(20.0)
                        .center()
                        .fill(p.card_hover)
                        .child(icon(&ICON_PROFILE, 18.0).color(p.text)),
                )
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .shrink()
                        .gap(3.0)
                        .child(text(format!("Signed in as {who}")).font(TRACK).color(p.text).truncate())
                        .child(
                            text("Your TIDAL session is kept on this machine.")
                                .font(SMALL)
                                .color(p.dim)
                                .wrap(),
                        ),
                ),
        )
        .child(w::divider(p))
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
                        .child(text("Sign out").font(TRACK).color(p.text))
                        .child(
                            text("Ends this session on this machine; playback stops.")
                                .font(SMALL)
                                .color(p.dim)
                                .wrap(),
                        ),
                )
                .child(
                    el()
                        .key("settings-sign-out")
                        .role("button")
                        .label("Sign out")
                        .h(36.0)
                        .px(18.0)
                        .center()
                        .radius(18.0)
                        .fill(p.danger.alpha(0.14))
                        .border(1.0, p.danger.alpha(0.45))
                        .hover(p.danger.alpha(0.24))
                        .press(p.danger.alpha(0.32))
                        .focusable(true)
                        .on_press(|s: &mut Ui| s.sign_out())
                        .child(text("Sign out").font(BUTTON).color(p.danger)),
                ),
        );
    col()
        .w_full()
        .gap(26.0)
        .child(w::block(p, "Account", None, card))
}
