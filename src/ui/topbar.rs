//! Top bar: the search field and the signed-in account button with its menu.

use rune::{El, Icon, Key, KeyEvent, TextStyle, Tx, col, el, icon, row, spacer, text, text_input};

use crate::app::{Login, Page, Ui};
use crate::theme::{BODY, BUTTON, ICON_BACK, ICON_CLOSE, ICON_FORWARD, ICON_PROFILE, ICON_SEARCH, ICON_SETTINGS, ICON_WIN_MAXIMIZE, ICON_WIN_MINIMIZE, TOPBAR_H, Palette};

/// Diameter of one window-control button.
const CONTROL_SIZE: f32 = 32.0;

/// The window is drawn without the system's title bar, so the app supplies what that bar did:
/// the top bar moves the window (`window_drag`), and these buttons maximize/restore, close and,
/// when the tray icon exists to bring the window back, hide it there. Rune does not minimize a
/// borderless window, so there is no plain "minimize" button.
pub(crate) fn window_controls(s: &Ui, p: Palette) -> El {
    let mut controls = row().key("window-controls").gap(4.0).center_y().ml(10.0);
    if s.tray_available {
        controls = controls.child(control_button(p, "window-minimize", "Minimize to tray", &ICON_WIN_MINIMIZE, false,
            |s: &mut Ui, _: &mut Tx| s.hide_to_tray()));
    }
    controls
        .child(control_button(p, "window-maximize", "Maximize or restore", &ICON_WIN_MAXIMIZE, false,
            |_: &mut Ui, tx: &mut Tx| tx.toggle_maximized()))
        .child(control_button(p, "window-close", "Close", &ICON_CLOSE, true,
            |_: &mut Ui, tx: &mut Tx| tx.close_window()))
}

fn control_button(
    p: Palette,
    key: &str,
    label: &str,
    glyph: &'static Icon,
    danger: bool,
    on: impl Fn(&mut Ui, &mut Tx) + 'static,
) -> El {
    // Round like the back/forward buttons beside them.
    el().key(key.to_owned()).role("button").label(label.to_owned()).focusable(true)
        .size(CONTROL_SIZE, CONTROL_SIZE).radius(CONTROL_SIZE / 2.0).center()
        .hover(if danger { p.danger } else { p.card_hover })
        .on_press_tx(on)
        .child(icon(glyph, 14.0).color(p.text))
}

impl Ui {
    /// Hides the window; the tray icon's Show brings it back (the app keeps playing).
    pub(crate) fn hide_to_tray(&mut self) {
        if let Some(handle) = &self.app_handle {
            handle.hide();
        }
    }
}

pub(crate) fn top_bar(s: &Ui, p: Palette) -> El {
    let mut bar = row()
        .key("topbar")
        .w_full()
        .h(TOPBAR_H)
        .pl(32.0)
        .pr(16.0)
        .center_y()
        .gap(12.0)
        .window_drag()
        .child(spacer())
        .child(history_buttons(s, p))
        .child(search_bar(s, p));
    if matches!(&s.login, Login::LoggedIn) {
        let mut avatar = el().key("profile-button").role("button").label("Account menu")
            .expanded(s.profile_menu_open).focusable(true).size(30.0, 30.0)
            .radius(15.0).clip_rounded(15.0).center().fill(p.card_hover)
            .on_press(|s: &mut Ui| s.profile_menu_open = !s.profile_menu_open);
        if let Some(image) = s.profile.as_ref().and_then(|profile| profile.picture.as_deref())
            .and_then(|picture| s.artwork.as_ref()?.get(picture))
        {
            avatar = avatar.pixels_fit(image.pixels(), rune::SurfaceFit::Fill);
        } else {
            avatar = avatar.child(icon(&ICON_PROFILE, 18.0).color(p.text));
        }
        bar = bar.child(avatar);
    }
    // The window's own buttons end the bar, in the row with everything else rather than
    // floating over it.
    bar.child(window_controls(s, p))
}

fn history_buttons(s: &Ui, p: Palette) -> El {
    row()
        .key("history")
        .gap(4.0)
        .child(history_button(
            p,
            "back",
            &ICON_BACK,
            "Back",
            s.history.can_back(),
            |s: &mut Ui| s.go_back(),
        ))
        .child(history_button(
            p,
            "forward",
            &ICON_FORWARD,
            "Forward",
            s.history.can_forward(),
            |s: &mut Ui| s.go_forward(),
        ))
}

fn history_button(
    p: Palette,
    name: &str,
    glyph: &'static Icon,
    label: &str,
    enabled: bool,
    on: impl Fn(&mut Ui) + 'static,
) -> El {
    let mut button = el()
        .key(format!("history-{name}"))
        .role("button")
        .label(label)
        .size(32.0, 32.0)
        .radius(16.0)
        .center()
        .fill(p.card)
        .hover(if enabled { p.card_hover } else { p.card })
        .focusable(enabled)
        .disabled(!enabled)
        .child(icon(glyph, 16.0).color(if enabled { p.text } else { p.faint }));
    if enabled {
        button = button.on_press(on);
    }
    button
}

pub(crate) fn profile_menu(s: &Ui, p: Palette) -> El {
    let mut menu = col().key("profile-menu").role("menu").w(192.0).padding(6.0)
        .radius(8.0).fill(p.card).border(1.0, p.border);
    if let Some(name) = s.profile.as_ref().and_then(|profile| profile.name.as_deref()) {
        menu = menu.child(el().w_full().px(12.0).py(10.0)
            .child(text(name.to_owned()).font(BUTTON).color(p.text).truncate()))
            .child(el().w_full().h(1.0).fill(p.border));
    }
    menu = menu
        .child(row().key("profile-settings").role("menuitem").label("Settings")
            .w_full().h(40.0).px(12.0).gap(10.0).center_y().radius(4.0)
            .hover(p.card_hover).focusable(true)
            .on_press(|s: &mut Ui| {
                s.profile_menu_open = false;
                s.page = Page::Settings;
            })
            .child(icon(&ICON_SETTINGS, 15.0).color(p.dim))
            .child(text("Settings").font(BODY).color(p.text)))
        .child(row().key("profile-sign-out").role("menuitem").label("Sign out")
            .w_full().h(40.0).px(12.0).center_y().radius(4.0)
            .hover(p.card_hover).focusable(true)
            .on_press(|s: &mut Ui| s.sign_out())
            .child(text("Sign out").font(BODY).color(p.text)));
    menu
}

fn search_bar(s: &Ui, p: Palette) -> El {
    let logged_in = matches!(&s.login, Login::LoggedIn);
    let mut bar = row()
        .key("search")
        .w(280.0)
        .h(36.0)
        .px(14.0)
        .gap(10.0)
        .center_y()
        .radius(18.0)
        .fill(p.card)
        .child(el().key("submit-search").role("button").label("Search")
            .size(18.0, 18.0).center().focusable(logged_in).disabled(!logged_in)
            .on_press(|s: &mut Ui| s.submit_search())
            .child(icon(&ICON_SEARCH, 16.0).color(p.faint)));

    if logged_in {
        bar = bar
            .child(
                text_input(&s.query, TextStyle::new(14.0, 400.0))
                    .key("search-input")
                    .single_line()
                    .grow(1.0)
                    .color(p.text)
                    .placeholder("Search", p.faint)
                    .on_edit(|s: &mut Ui| &mut s.query)
                    .on_submit(|s: &mut Ui, _: &mut Tx| s.submit_search())
                    .on_input_key(|s: &mut Ui, ev: &KeyEvent, _: &mut Tx| {
                        let count = s.search_suggest_list().len();
                        match ev.key {
                            Key::Down if count > 0 => {
                                let next = s
                                    .suggest_selected
                                    .map_or(0, |index| (index + 1).min(count - 1));
                                s.suggest_selected = Some(next);
                                true
                            }
                            Key::Up if count > 0 => {
                                let next = s
                                    .suggest_selected
                                    .map_or(count - 1, |index| index.saturating_sub(1));
                                s.suggest_selected = Some(next);
                                true
                            }
                            Key::Enter => {
                                let picked = s
                                    .suggest_selected
                                    .and_then(|index| s.search_suggest_list().get(index).cloned());
                                match picked {
                                    Some(text) => {
                                        s.accept_suggestion(text);
                                        true
                                    }
                                    None => false,
                                }
                            }
                            Key::Escape => {
                                if s.search_dropdown_open() {
                                    s.dismiss_suggestions();
                                } else {
                                    s.clear_search();
                                }
                                true
                            }
                            _ => false,
                        }
                    }),
            );
        if !s.query.text.is_empty() {
            bar = bar.child(clear_button(p));
        }
    } else {
        bar = bar.child(
            text("Sign in to search TIDAL")
                .font(TextStyle::new(14.0, 400.0))
                .color(p.faint)
                .grow(1.0),
        );
    }
    bar
}

fn clear_button(p: Palette) -> El {
    el()
        .key("clear-search")
        .role("button")
        .label("Clear search")
        .size(24.0, 24.0)
        .radius(12.0)
        .center()
        .hover(p.card_hover)
        .focusable(true)
        .on_press(|s: &mut Ui| s.clear_search())
        .child(text("×").font(TextStyle::new(16.0, 500.0)).color(p.dim))
}
