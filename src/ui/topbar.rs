//! Top bar: the search field (which collapses to an icon button when the bar is narrow) and the
//! signed-in account button with its menu.

use rune::{Color, El, Icon, Key, KeyEvent, TextStyle, Tx, col, el, icon, row, spacer, text, text_input};

use crate::app::{ArtKind, Login, Page, Ui};
use crate::theme::{BODY, BUTTON, ICON_BACK, ICON_CLOSE, ICON_DOWN, ICON_FORWARD, ICON_PROFILE, ICON_SEARCH, ICON_SETTINGS, ICON_WIN_MAXIMIZE, ICON_WIN_MINIMIZE, TOPBAR_H, Palette};

/// Diameter of one window-control button.
const CONTROL_SIZE: f32 = 32.0;
/// The top bar's own horizontal padding (`pl` + `pr`).
const BAR_PAD: f32 = 48.0;
/// The row's gap between children.
const BAR_GAP: f32 = 12.0;
/// The back/forward pair: two 32pt buttons with the row's 4pt gap.
const HISTORY_W: f32 = 68.0;
/// The account button: avatar, chevron and its padding.
const ACCOUNT_W: f32 = 46.0;
/// Search field widths: the full field, the narrowest it shrinks to, and its collapsed icon.
const SEARCH_MAX: f32 = 280.0;
const SEARCH_MIN: f32 = 150.0;
const SEARCH_ICON: f32 = 32.0;

/// How the top bar lays itself out at a given main-area width. Derived from the laid-out width of
/// `main` (see `ui::view`), so it is a breakpoint: the view only re-runs when the mode changes,
/// while the search field itself shrinks continuously through layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BarMode {
    /// The full search field (which shrinks, by layout, down to `SEARCH_MIN`).
    Field,
    /// The field has collapsed to an icon button; pressing it expands the field again.
    Icon,
    /// Like `Icon`, but the history buttons also give up their room so the window controls and
    /// the account button always fit.
    Compact,
}

impl BarMode {
    /// The width the bar needs for a `search`-wide search element, with the non-search parts it
    /// keeps: the padding, the narrowest `search` the mode allows, the account button (when
    /// signed in) and the window controls — plus the row's gaps. `history` counts the
    /// back/forward pair, which `Compact` gives up.
    fn needed(history: bool, search: f32, logged_in: bool, tray: bool) -> f32 {
        let mut children = 3usize; // spacer, search, window controls
        if history { children += 1; }
        if logged_in { children += 1; }
        // `window_controls`: 10pt leading margin, then 32pt buttons 4pt apart.
        let controls = 10.0 + if tray { CONTROL_SIZE * 3.0 + 8.0 } else { CONTROL_SIZE * 2.0 + 4.0 };
        BAR_PAD
            + if history { HISTORY_W } else { 0.0 }
            + search
            + if logged_in { ACCOUNT_W } else { 0.0 }
            + controls
            + (children - 1) as f32 * BAR_GAP
    }

    /// The mode for a `main_w`-wide main area. `logged_in` and `tray` decide the fixed parts the
    /// bar always keeps (the account button and the tray's extra window button). The search field
    /// shrinks by layout down to `SEARCH_MIN`, so `Field` covers both the full and the shrunken
    /// field; `Icon` is the collapse, and `Compact` is where the search has squeezed the history
    /// buttons out too. The window controls and the account button are never given up.
    pub(crate) fn for_width(main_w: f32, logged_in: bool, tray: bool) -> BarMode {
        if main_w >= Self::needed(true, SEARCH_MIN, logged_in, tray) {
            BarMode::Field
        } else if main_w >= Self::needed(true, SEARCH_ICON, logged_in, tray) {
            BarMode::Icon
        } else {
            BarMode::Compact
        }
    }

    /// Whether the bar shows the field or the collapsed icon.
    pub(crate) fn is_collapsed(self) -> bool {
        matches!(self, BarMode::Icon | BarMode::Compact)
    }

    /// Whether the back/forward buttons have room.
    fn shows_history(self) -> bool {
        !matches!(self, BarMode::Compact)
    }
}

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
    let logged_in = matches!(&s.login, Login::LoggedIn);
    let mode = s.bar_mode.get();
    // A collapsed bar expands its field on demand; while it is up, the field grows to fill the
    // bar and the spacer steps aside so it can.
    let expanded = mode.is_collapsed() && s.search_expanded;
    let mut bar = row()
        .key("topbar")
        .w_full()
        .h(TOPBAR_H)
        .pl(32.0)
        .pr(16.0)
        .center_y()
        .gap(12.0)
        .window_drag()
        .child(spacer().grow(if expanded { 0.0 } else { 1.0 }));
    if mode.shows_history() {
        bar = bar.child(history_buttons(s, p));
    }
    bar = bar.child(search_area(s, p, mode, expanded));
    if logged_in {
        bar = bar.child(account_button(s, p));
    }
    // The window's own buttons end the bar, in the row with everything else rather than
    // floating over it. Never hidden: the search gives up its room first.
    bar.child(window_controls(s, p))
}

/// The search: the field, or (when the bar is collapsed and not expanded) its icon button. Both
/// carry the `search` key, so the suggestions dropdown anchors to whatever is up.
fn search_area(s: &Ui, p: Palette, mode: BarMode, expanded: bool) -> El {
    if mode == BarMode::Field {
        search_bar(s, p).w(SEARCH_MAX).shrink().min_w(SEARCH_MIN)
    } else if expanded {
        search_bar(s, p).grow(1.0).min_w(0.0)
    } else {
        el()
            .key("search")
            .role("button")
            .label("Search")
            .size(SEARCH_ICON + 4.0, SEARCH_ICON + 4.0)
            .radius((SEARCH_ICON + 4.0) / 2.0)
            .center()
            .fill(p.card)
            .hover(p.card_hover)
            .press(p.accent.alpha(0.25))
            .focusable(true)
            .on_press(|s: &mut Ui| s.expand_search())
            .child(icon(&ICON_SEARCH, 16.0).color(p.text))
    }
}

/// The account button: the avatar (or a placeholder icon) inside a subtle outlined pill with a
/// chevron, so it reads as the menu that opens the profile. Pressing it keeps toggling the menu.
fn account_button(s: &Ui, p: Palette) -> El {
    let picture = s.profile.as_ref().and_then(|profile| profile.picture.as_deref());
    let mut circle = el().size(26.0, 26.0).radius(13.0).clip_rounded(13.0).center().fill(p.card_hover);
    match s.wanted_art(ArtKind::Picture, picture) {
        Some(image) => circle = circle.pixels_fit(image.pixels(), rune::SurfaceFit::Fill),
        None => circle = circle.child(icon(&ICON_PROFILE, 15.0).color(p.dim)),
    }
    el()
        .key("profile-button")
        .role("button")
        .label("Account")
        .expanded(s.profile_menu_open)
        .focusable(true)
        .h(32.0)
        .px(3.0)
        .gap(4.0)
        .center_y()
        .radius(16.0)
        .fill(Color::TRANSPARENT)
        .border(1.0, p.border)
        .hover(p.card_hover)
        .press(p.accent.alpha(0.20))
        .on_press(|s: &mut Ui| s.profile_menu_open = !s.profile_menu_open)
        .child(circle)
        .child(icon(&ICON_DOWN, 10.0).color(p.dim))
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
    // Width comes from `search_area`: the full field, or the expanded field filling the bar.
    let mut bar = row()
        .key("search")
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
                    .min_w(0.0)
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
                            // Escape on an empty expanded field folds it back to its icon; with
                            // text in it, Escape still dismisses the dropdown or clears it.
                            Key::Escape if s.search_collapsed() && s.query.text.is_empty() => {
                                s.collapse_search();
                                true
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The width a mode keeps for itself: the parts it never gives up plus its search element.
    fn keeps(mode: BarMode, logged_in: bool, tray: bool) -> f32 {
        match mode {
            BarMode::Field => BarMode::needed(true, SEARCH_MIN, logged_in, tray),
            BarMode::Icon => BarMode::needed(true, SEARCH_ICON, logged_in, tray),
            BarMode::Compact => BarMode::needed(false, SEARCH_ICON, logged_in, tray),
        }
    }

    /// How generous a mode is, so a shrinking width can only move down it.
    fn rank(mode: BarMode) -> u8 {
        match mode {
            BarMode::Field => 2,
            BarMode::Icon => 1,
            BarMode::Compact => 0,
        }
    }

    /// The bar gives up the search before the window controls or the account button: the mode it
    /// picks fits what that mode keeps, it only narrows as the width shrinks, and `Field` is
    /// picked exactly when the full field fits.
    #[test]
    fn the_search_collapses_before_the_controls_ever_do() {
        for (logged_in, tray) in [(true, false), (true, true), (false, false)] {
            // Walking the widths upward, the bar can only ever get more generous.
            let mut previous = BarMode::Compact;
            for width in (0..1600).map(|w| w as f32) {
                let mode = BarMode::for_width(width, logged_in, tray);
                if mode != BarMode::Compact {
                    assert!(
                        keeps(mode, logged_in, tray) <= width,
                        "{mode:?} overflows at {width} (logged_in={logged_in}, tray={tray})"
                    );
                }
                assert!(rank(mode) >= rank(previous), "the bar narrowed as the window grew at {width}");
                assert_eq!(
                    mode == BarMode::Field,
                    width >= keeps(BarMode::Field, logged_in, tray),
                    "Field at {width} (logged_in={logged_in}, tray={tray})"
                );
                previous = mode;
            }
        }
    }

    /// `Icon` is chosen exactly while the icon fits but the field would not.
    #[test]
    fn icon_mode_is_the_window_where_the_field_no_longer_fits() {
        for (logged_in, tray) in [(true, false), (true, true), (false, false)] {
            let field = keeps(BarMode::Field, logged_in, tray);
            let icon = keeps(BarMode::Icon, logged_in, tray);
            assert!(field > icon, "the collapsed bar is narrower than the full one");
            assert_eq!(BarMode::for_width(field, logged_in, tray), BarMode::Field);
            assert_eq!(BarMode::for_width(field - 1.0, logged_in, tray), BarMode::Icon);
            assert_eq!(BarMode::for_width(icon, logged_in, tray), BarMode::Icon);
            assert_eq!(BarMode::for_width(icon - 1.0, logged_in, tray), BarMode::Compact);
        }
    }

    /// The shapes the friend reported: with a side panel open at the minimum window the search
    /// collapses (and the history buttons go with it), while a wide window keeps the full field.
    #[test]
    fn the_reported_shapes_collapse_the_search_and_keep_the_controls() {
        // 860pt window: 236pt sidebar + 320pt queue leaves 304pt of main.
        assert_eq!(BarMode::for_width(304.0, true, false), BarMode::Compact);
        // 860pt window: sidebar and 360pt lyrics leave 264pt.
        assert_eq!(BarMode::for_width(264.0, true, false), BarMode::Compact);
        // 1100pt window with the queue open: 544pt of main, the field shrunken but up.
        assert_eq!(BarMode::for_width(544.0, true, false), BarMode::Field);
        // A full-width window, and the same signed out.
        assert_eq!(BarMode::for_width(1044.0, true, false), BarMode::Field);
        assert_eq!(BarMode::for_width(1044.0, false, false), BarMode::Field);
    }
}
