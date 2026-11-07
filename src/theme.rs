//! Visual language: the palette, the layout metrics every area shares, the text styles and the
//! icon set. Nothing here depends on `Ui` state, so any view module can pull from it.

use rune::{Color, Icon, TextStyle, oklch, oklcha};

pub(crate) const SIDEBAR_W: f32 = 236.0;
pub(crate) const TOPBAR_H: f32 = 76.0;
pub(crate) const PLAYER_H: f32 = 84.0;

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) bg: Color,
    pub(crate) sidebar: Color,
    pub(crate) card: Color,
    pub(crate) card_hover: Color,
    pub(crate) border: Color,
    pub(crate) text: Color,
    pub(crate) dim: Color,
    pub(crate) faint: Color,
    pub(crate) accent: Color,
    pub(crate) accent_ink: Color,
    pub(crate) danger: Color,
}

pub(crate) fn palette() -> Palette {
    Palette {
        bg: oklch(0.0, 0.0, 0.0),
        sidebar: oklch(0.0, 0.0, 0.0),
        card: oklch(0.19, 0.0, 0.0),
        card_hover: oklch(0.28, 0.0, 0.0),
        border: oklcha(1.0, 0.0, 0.0, 0.10),
        text: oklch(0.97, 0.0, 0.0),
        dim: oklch(0.68, 0.0, 0.0),
        faint: oklch(0.48, 0.0, 0.0),
        accent: oklch(0.97, 0.0, 0.0),
        accent_ink: oklch(0.12, 0.0, 0.0),
        danger: oklch(0.68, 0.17, 25.0),
    }
}

pub(crate) const H1: TextStyle = TextStyle::new(28.0, 600.0);
pub(crate) const HERO: TextStyle = TextStyle::new(38.0, 700.0);
pub(crate) const SUBTITLE: TextStyle = TextStyle::new(16.0, 500.0);
pub(crate) const H2: TextStyle = TextStyle::new(18.0, 600.0);
pub(crate) const TRACK: TextStyle = TextStyle::new(14.0, 500.0);
pub(crate) const BODY: TextStyle = TextStyle::new(13.0, 400.0);
pub(crate) const SMALL: TextStyle = TextStyle::new(12.0, 400.0);
pub(crate) const CAPS: TextStyle = TextStyle::new(10.5, 600.0).tracking(1.2);
pub(crate) const BUTTON: TextStyle = TextStyle::new(13.0, 600.0);

pub(crate) static ICON_SEARCH: Icon = Icon {
    name: "search",
    view: 24.0,
    body: r#"<circle cx="10.5" cy="10.5" r="6.5" stroke="currentColor" stroke-width="2"/><line x1="15.5" y1="15.5" x2="21" y2="21" stroke="currentColor" stroke-width="2"/>"#,
};
pub(crate) static ICON_PLAY: Icon = Icon { name: "play", view: 24.0, body: r#"<path d="M8 5v14l11-7z"/>"# };
pub(crate) static ICON_PAUSE: Icon = Icon { name: "pause", view: 24.0, body: r#"<path d="M6 4h4v16H6zM14 4h4v16h-4z"/>"# };
pub(crate) static ICON_PREVIOUS: Icon = Icon {
    name: "previous", view: 24.0,
    body: r#"<path d="M6 5h2v14H6zM18 5v14L8 12z"/>"#,
};
pub(crate) static ICON_NEXT: Icon = Icon {
    name: "next", view: 24.0,
    body: r#"<path d="M16 5h2v14h-2zM6 5v14l10-7z"/>"#,
};
pub(crate) static ICON_NOTE: Icon = Icon {
    name: "note",
    view: 24.0,
    body: r#"<path d="M9 18V5l12-2v13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><circle cx="6" cy="18" r="3" fill="none" stroke="currentColor" stroke-width="2"/><circle cx="18" cy="16" r="3" fill="none" stroke="currentColor" stroke-width="2"/>"#,
};
pub(crate) static ICON_PROFILE: Icon = Icon {
    name: "profile",
    view: 24.0,
    body: r#"<circle cx="12" cy="8" r="4" fill="none" stroke="currentColor" stroke-width="2"/><path d="M4 21a8 8 0 0 1 16 0" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_TIDAL: Icon = Icon {
    name: "tidal-mark",
    view: 24.0,
    body: r#"<path d="M12.012 3.992L8.008 7.996 4.004 3.992 0 7.996 4.004 12l4.004-4.004L12.012 12l-4.004 4.004 4.004 4.004 4.004-4.004L12.012 12l4.004-4.004-4.004-4.004zM16.042 7.996l3.979-3.979L24 7.996l-3.979 3.979z"/>"#,
};
pub(crate) static ICON_EXPLORE: Icon = Icon {
    name: "explore",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="8" stroke="currentColor" stroke-width="1.8"/><path d="M4 12h16M12 4c-4 4-4 12 0 16M12 4c4 4 4 12 0 16" stroke="currentColor" stroke-width="1.6" fill="none"/>"#,
};
pub(crate) static ICON_COLLECTION: Icon = Icon {
    name: "collection",
    view: 24.0,
    body: r#"<path d="M6 5v14M10 5v14M14 5v14M18 5v14" stroke="currentColor" stroke-width="1.8"/>"#,
};
pub(crate) static ICON_FOLDER: Icon = Icon {
    name: "folder",
    view: 24.0,
    body: r#"<path d="M4 7a2 2 0 0 1 2-2h3.5l2 2H18a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_HEART: Icon = Icon {
    name: "heart",
    view: 24.0,
    body: r#"<path d="M12 20.3 5 13.3a4.5 4.5 0 0 1 6.4-6.4l.6.6.6-.6A4.5 4.5 0 0 1 19 13.3z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_HEART_FILLED: Icon = Icon {
    name: "heart-filled",
    view: 24.0,
    body: r#"<path d="M12 20.3 5 13.3a4.5 4.5 0 0 1 6.4-6.4l.6.6.6-.6A4.5 4.5 0 0 1 19 13.3z"/>"#,
};
pub(crate) static ICON_BACK: Icon = Icon {
    name: "back",
    view: 24.0,
    body: r#"<path d="M15 5 8 12l7 7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_FORWARD: Icon = Icon {
    name: "forward",
    view: 24.0,
    body: r#"<path d="M9 5l7 7-7 7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_SHUFFLE: Icon = Icon {
    name: "shuffle",
    view: 24.0,
    body: r#"<path d="M16 3h5v5M4 20 21 3M21 16v5h-5M15 15l6 6M4 4l5 5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_REPEAT: Icon = Icon {
    name: "repeat",
    view: 24.0,
    body: r#"<path d="M17 2l4 4-4 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M3 11V9a4 4 0 0 1 4-4h14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M7 22l-4-4 4-4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M21 13v2a4 4 0 0 1-4 4H3" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_REPEAT_ONE: Icon = Icon {
    name: "repeat-one",
    view: 24.0,
    body: r#"<path d="M17 2l4 4-4 4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M3 11V9a4 4 0 0 1 4-4h14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M7 22l-4-4 4-4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M21 13v2a4 4 0 0 1-4 4H3" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M11 10.5l1.6-1.5V15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_VOLUME_HIGH: Icon = Icon {
    name: "volume-high",
    view: 24.0,
    body: r#"<path d="M11 5 6 9H2v6h4l5 4z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M15.5 8.5a5 5 0 0 1 0 7M18.7 5.3a9 9 0 0 1 0 13.4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_VOLUME_LOW: Icon = Icon {
    name: "volume-low",
    view: 24.0,
    body: r#"<path d="M11 5 6 9H2v6h4l5 4z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M15.5 8.5a5 5 0 0 1 0 7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_VOLUME_MUTE: Icon = Icon {
    name: "volume-mute",
    view: 24.0,
    body: r#"<path d="M11 5 6 9H2v6h4l5 4z" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"/><path d="M22 9l-6 6M16 9l6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_QUEUE: Icon = Icon {
    name: "queue",
    view: 24.0,
    body: r#"<path d="M3 6h13M3 12h13M3 18h8M17 12v9l5-4.5z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_LYRICS: Icon = Icon {
    name: "lyrics",
    view: 24.0,
    body: r#"<path d="M4 5h16M4 10h16M4 15h11M4 20h7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_SETTINGS: Icon = Icon {
    name: "settings",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" stroke-width="2"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_CLOSE: Icon = Icon {
    name: "close",
    view: 24.0,
    body: r#"<path d="M6 6l12 12M18 6L6 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_CHECK: Icon = Icon {
    name: "check",
    view: 24.0,
    body: r#"<path d="M4 12.5l5 5L20 6.5" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_WIN_MINIMIZE: Icon = Icon {
    name: "win-minimize",
    view: 24.0,
    body: r#"<path d="M5 17h14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_WIN_MAXIMIZE: Icon = Icon {
    name: "win-maximize",
    view: 24.0,
    body: r#"<rect x="5" y="5" width="14" height="14" rx="2" fill="none" stroke="currentColor" stroke-width="2"/>"#,
};
/// The "…" that opens a row's or header's context menu.
pub(crate) static ICON_MORE: Icon = Icon {
    name: "more",
    view: 24.0,
    body: r#"<circle cx="5" cy="12" r="1.9"/><circle cx="12" cy="12" r="1.9"/><circle cx="19" cy="12" r="1.9"/>"#,
};
pub(crate) static ICON_PLUS: Icon = Icon {
    name: "plus",
    view: 24.0,
    body: r#"<path d="M12 5v14M5 12h14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_TRASH: Icon = Icon {
    name: "trash",
    view: 24.0,
    body: r#"<path d="M4 7h16M9.5 7V4.5h5V7M6.5 7l1 13h9l1-13M10 11v6M14 11v6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_LINK: Icon = Icon {
    name: "link",
    view: 24.0,
    body: r#"<path d="M10.5 13.5a3.5 3.5 0 0 1 0-5l2-2a3.5 3.5 0 0 1 5 5l-1 1M13.5 10.5a3.5 3.5 0 0 1 0 5l-2 2a3.5 3.5 0 0 1-5-5l1-1" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(crate) static ICON_INFO: Icon = Icon {
    name: "info",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M12 11v6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"/><circle cx="12" cy="7.6" r="1.2"/>"#,
};
pub(crate) static ICON_RADIO: Icon = Icon {
    name: "radio",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="2.4"/><path d="M7.8 16.2a6 6 0 0 1 0-8.4M16.2 7.8a6 6 0 0 1 0 8.4M4.9 19.1a10 10 0 0 1 0-14.2M19.1 4.9a10 10 0 0 1 0 14.2" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>"#,
};
pub(crate) static ICON_ALBUM: Icon = Icon {
    name: "album",
    view: 24.0,
    body: r#"<circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="12" cy="12" r="2.6" fill="none" stroke="currentColor" stroke-width="1.8"/>"#,
};
pub(crate) static ICON_EDIT: Icon = Icon {
    name: "edit",
    view: 24.0,
    body: r#"<path d="M4 20h4L20 8l-4-4L4 16z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M14 6l4 4" fill="none" stroke="currentColor" stroke-width="1.8"/>"#,
};
/// A chevron up, for "Move up" in reorder menus.
pub(crate) static ICON_UP: Icon = Icon {
    name: "up",
    view: 24.0,
    body: r#"<path d="M6 15l6-6 6 6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
/// A chevron down, for "Move down" in reorder menus.
pub(crate) static ICON_DOWN: Icon = Icon {
    name: "down",
    view: 24.0,
    body: r#"<path d="M6 9l6 6 6-6" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
