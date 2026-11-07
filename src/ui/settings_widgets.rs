//! The controls every Settings card is built from: the grouped-list row (a title with a
//! one-line description on the left, its control on the right), the switch, the section
//! heading and its card, the hairline divider, the segmented control and the status badge.
//!
//! Keeping them here is what makes the page consistent: every card shares the same paddings,
//! corner radii, hover highlight and accent treatment, and a change to the visual language
//! lands everywhere at once.

use rune::{Color, El, Icon, TextStyle, col, el, icon, oklch, row, text};

use crate::app::Ui;
use crate::theme::{SMALL, TRACK, Palette};

/// A group heading above its card: sentence case and semibold, not a tiny letter-spaced label.
pub(crate) const GROUP: TextStyle = TextStyle::new(13.0, 600.0);

/// A card title inside a card that owns its own header (the integrations).
pub(crate) const CARD_TITLE: TextStyle = TextStyle::new(15.0, 600.0);

/// The green the app already uses for live audio state ("Output: 96 kHz · f32").
fn ok_tone() -> Color {
    oklch(0.78, 0.13, 170.0)
}

/// A warm amber for "waiting for you" states.
fn warn_tone() -> Color {
    oklch(0.80, 0.11, 80.0)
}

/// How a status badge reads.
#[derive(Clone, Copy)]
pub(crate) enum Tone {
    /// Live and working.
    Ok,
    /// Waiting on something.
    Warn,
    /// Simply off.
    Off,
    /// Failed, with the reason in the label.
    Error,
}

/// The colour a tone draws in.
pub(crate) fn tone_color(tone: Tone, p: Palette) -> Color {
    match tone {
        Tone::Ok => ok_tone(),
        Tone::Warn => warn_tone(),
        Tone::Off => p.dim,
        Tone::Error => p.danger,
    }
}

/// A section heading (with an optional one-line description under it) that sits above a card.
pub(crate) fn section(p: Palette, title: &str, detail: Option<&str>) -> El {
    let mut heading = col().w_full().gap(3.0).pl(2.0)
        .child(text(title.to_owned()).font(GROUP).color(p.dim));
    if let Some(detail) = detail {
        heading = heading.child(text(detail.to_owned()).font(SMALL).color(p.faint).wrap());
    }
    heading
}

/// A headed group: the section heading above the card, spaced as one block.
pub(crate) fn block(p: Palette, title: &str, detail: Option<&str>, card: El) -> El {
    col().w_full().gap(8.0)
        .child(section(p, title, detail))
        .child(card)
}

/// The empty card a group's rows go in.
pub(crate) fn card(p: Palette) -> El {
    col().w_full().padding(4.0).radius(12.0).fill(p.card).border(1.0, p.border)
}

/// A hairline between rows of a card, inset to the rows' text.
pub(crate) fn divider(p: Palette) -> El {
    el().w_full().h(1.0).fill(p.border).mx(14.0)
}

/// One row of a group: a title (and an optional one-line description) on the left, `control`
/// on the right. The caller wires the control's own behaviour.
pub(crate) fn setting_row(
    p: Palette,
    key: &str,
    title: &str,
    detail: Option<&str>,
    control: El,
) -> El {
    let mut left = col().grow(1.0).min_w(0.0).shrink().gap(3.0)
        .child(text(title.to_owned()).font(TRACK).color(p.text));
    if let Some(detail) = detail {
        left = left.child(text(detail.to_owned()).font(SMALL).color(p.dim).wrap());
    }
    row()
        .key(key)
        .w_full()
        .padding(14.0)
        .gap(16.0)
        .center_y()
        .radius(8.0)
        .child(left)
        .child(control)
}

/// A boolean row: pressing anywhere on it flips the setting, and the switch on the right shows
/// the state.
pub(crate) fn toggle_row(
    p: Palette,
    key: &str,
    title: &str,
    detail: Option<&str>,
    on: bool,
    on_press: impl Fn(&mut Ui) + 'static,
) -> El {
    setting_row(p, key, title, detail, switch(p, on))
        .role("button")
        .label(title.to_owned())
        .hover(p.card_hover)
        .focusable(true)
        .on_press(on_press)
}

/// A real switch: a track with a knob that slides to the right when it is on.
pub(crate) fn switch(p: Palette, on: bool) -> El {
    let mut track = row()
        .size(44.0, 24.0)
        .px(3.0)
        .radius(12.0)
        .center_y()
        .fill(if on { p.accent } else { p.card_hover })
        .border(1.0, if on { p.accent } else { p.border });
    if on {
        track = track.justify_end();
    }
    track.child(
        el()
            .size(18.0, 18.0)
            .radius(9.0)
            .animate_layout()
            .fill(if on { p.accent_ink } else { p.text }),
    )
}

/// A recessed surface inside a card: darker than the card, without the stark black of the page
/// behind it. Used for the segmented track and for text fields.
pub(crate) fn recess(p: Palette) -> Color {
    p.bg.mix(p.card, 0.4)
}

/// A segmented control: the track `items` (built with [`segment_item`]) sit in.
pub(crate) fn segmented(p: Palette, items: impl IntoIterator<Item = El>) -> El {
    row()
        .padding(4.0)
        .gap(4.0)
        .radius(11.0)
        .fill(recess(p))
        .border(1.0, p.border)
        .children(items)
}

/// One option of a segmented control, sized to fill its share of the track.
pub(crate) fn segment_item(p: Palette, key: &str, label: &str, selected: bool) -> El {
    row()
        .key(key)
        .role("button")
        .label(label.to_owned())
        .grow(1.0)
        .min_w(0.0)
        .shrink()
        .h(32.0)
        .px(10.0)
        .radius(8.0)
        .center()
        .fill(if selected { p.card_hover } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .child(
            text(label.to_owned())
                .font(TRACK)
                .color(if selected { p.text } else { p.dim })
                .truncate(),
        )
}

/// A left-hand navigation item, highlighting the section the page is showing.
pub(crate) fn nav_item(p: Palette, key: &str, label: &str, mark: &'static Icon, selected: bool) -> El {
    row()
        .key(key)
        .role("button")
        .label(label.to_owned())
        .w_full()
        .h(40.0)
        .px(12.0)
        .gap(12.0)
        .center_y()
        .radius(8.0)
        .fill(if selected { p.card_hover } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .child(icon(mark, 16.0).color(if selected { p.text } else { p.dim }))
        .child(
            text(label.to_owned())
                .font(TRACK)
                .color(if selected { p.text } else { p.dim }),
        )
}

/// The compact form of a navigation item, for the strip the page shows when the window is too
/// narrow for the side navigation.
pub(crate) fn nav_chip(p: Palette, key: &str, label: &str, selected: bool) -> El {
    row()
        .key(key)
        .role("button")
        .label(label.to_owned())
        .h(32.0)
        .px(12.0)
        .radius(8.0)
        .center()
        .fill(if selected { p.card_hover } else { p.card })
        .border(1.0, if selected { p.border } else { Color::TRANSPARENT })
        .hover(p.card_hover)
        .focusable(true)
        .child(
            text(label.to_owned())
                .font(TRACK)
                .color(if selected { p.text } else { p.dim }),
        )
}

/// A pill that says how an integration is doing, with a dot in the tone's colour.
pub(crate) fn status_badge(p: Palette, tone: Tone, label: &str) -> El {
    let colour = tone_color(tone, p);
    row()
        .h(24.0)
        .px(10.0)
        .gap(6.0)
        .center_y()
        .radius(12.0)
        .fill(colour.alpha(0.14))
        .border(1.0, colour.alpha(0.32))
        .child(el().size(6.0, 6.0).radius(3.0).fill(colour))
        .child(text(label.to_owned()).font(SMALL).color(colour))
}

/// A small "dot + line" note, for live values like the current output format.
pub(crate) fn note(colour: Color, body: &str) -> El {
    row()
        .grow(1.0)
        .min_w(0.0)
        .shrink()
        .gap(8.0)
        .center_y()
        .child(el().size(6.0, 6.0).radius(3.0).fill(colour))
        .child(
            text(body.to_owned())
                .font(SMALL)
                .color(colour)
                .grow(1.0)
                .min_w(0.0)
                .shrink()
                .truncate(),
        )
}

/// The card header an integration card owns: its title (and a one-line summary) with its
/// status badge at the far right.
pub(crate) fn card_header(p: Palette, title: &str, detail: &str, badge: Option<El>) -> El {
    let mut head = row().w_full().gap(12.0).center_y();
    head = head.child(
        col().grow(1.0).min_w(0.0).shrink().gap(3.0)
            .child(text(title.to_owned()).font(CARD_TITLE).color(p.text))
            .child(text(detail.to_owned()).font(SMALL).color(p.dim).wrap()),
    );
    if let Some(badge) = badge {
        head = head.child(badge);
    }
    head
}

/// A labelled text field: the label in a fixed left column, the field beside it, an optional
/// hint under the field. The field keeps its own key and behaviour; only its frame is drawn
/// here, so every input on the page lines up.
pub(crate) fn field_row(p: Palette, label: &str, field: El, hint: Option<&str>) -> El {
    field_row_with(p, label, field, hint.map(|hint| (hint, p.faint)))
}

/// [`field_row`] with a colour for the hint, for a hint that is calling out a problem.
pub(crate) fn field_row_with(
    p: Palette,
    label: &str,
    field: El,
    hint: Option<(&str, Color)>,
) -> El {
    let mut block = col().w_full().gap(6.0).child(
        row()
            .w_full()
            .gap(16.0)
            .center_y()
            .child(el().w(150.0).child(text(label.to_owned()).font(SMALL).color(p.dim)))
            .child(field.grow(1.0).min_w(0.0).shrink()),
    );
    if let Some((hint, colour)) = hint {
        block = block.child(
            row()
                .w_full()
                .gap(16.0)
                .child(el().w(150.0))
                .child(
                    text(hint.to_owned())
                        .font(SMALL)
                        .color(colour)
                        .grow(1.0)
                        .min_w(0.0)
                        .shrink()
                        .wrap(),
                ),
        );
    }
    block
}

/// The frame a text field sits in, shared so every input on the page lines up.
pub(crate) fn field_frame(input: El, p: Palette) -> El {
    row()
        .w_full()
        .h(40.0)
        .px(12.0)
        .radius(8.0)
        .fill(recess(p))
        .border(1.0, p.border)
        .center_y()
        .child(input)
}
