//! The Settings card for Discord Rich Presence.
//!
//! The card owns the whole feature's user-facing side: the Application ID field (Discord shows
//! "Listening to <that application's name>", so it recommends naming the application "TIDAL"),
//! the enable switch, the three display switches and the live connection status. Everything the
//! switches say is real: the switches are handed to the running [`Discord`](crate::discord)
//! client and change what it publishes, and the status line is the client thread's own state.

use rune::{Color, El, TextStyle, col, el, icon, row, text, text_input};

use crate::app::Ui;
use crate::discord::{self, Config, Status};
use crate::theme::{BODY, CAPS, ICON_CHECK, SMALL, TRACK, Palette};

/// The Discord Rich Presence card.
pub(crate) fn discord_card(s: &Ui, p: Palette) -> El {
    let enabled = s.settings.discord_enabled;
    let typed = s.discord_client_id.text.trim();
    let mut card = card(p)
        .child(section("Discord Rich Presence", p))
        .child(
            text(
                "Shows what you are listening to on your Discord profile. It needs your own \
                 Discord application: create one at discord.com/developers/applications, name it \
                 TIDAL, and paste its Application ID below.",
            )
            .font(SMALL)
            .color(p.faint)
            .wrap_anywhere(),
        )
        .child(field_row(s, p, enabled))
        .child(activity_name_row(s, p))
        .child(toggle_row(
            p,
            "discord-enabled",
            "Show what I'm listening to",
            "Publishes the current track as your Discord activity, through the application above.",
            enabled,
            |s: &mut Ui| s.set_discord_enabled(!s.settings.discord_enabled),
        ));
    if enabled {
        card = card.child(status_row(s, p));
    }
    if !typed.is_empty() && !discord::valid_client_id(typed) {
        card = card.child(
            text("An Application ID is 17 to 20 digits.")
                .font(SMALL)
                .color(p.danger)
                .wrap_anywhere(),
        );
    }
    card = card
        .child(toggle_row(
            p,
            "discord-show-art",
            "Show album art",
            "Sends the TIDAL cover as the activity's large image (Discord fetches it itself).",
            s.settings.discord_show_art,
            |s: &mut Ui| s.set_discord_show_art(!s.settings.discord_show_art),
        ))
        .child(toggle_row(
            p,
            "discord-show-time",
            "Show elapsed and remaining time",
            "Sends start and end times so Discord draws the progress bar; it follows seeking.",
            s.settings.discord_show_time,
            |s: &mut Ui| s.set_discord_show_time(!s.settings.discord_show_time),
        ))
        .child(toggle_row(
            p,
            "discord-button",
            "Show a Listen on TIDAL button",
            "Adds a button linking to the track's TIDAL page. Local files have no such page, so \
             they never get one. Other people see the button; Discord hides it from you.",
            s.settings.discord_show_button,
            |s: &mut Ui| s.set_discord_show_button(!s.settings.discord_show_button),
        ))
        .child(
            text(
                "Paused playback is sent without timestamps and with \"Paused\" in the state, \
                 because Discord has no paused flag. Your Discord friends see whatever tsun is \
                 playing (and, for TIDAL tracks, the track's link) whenever this is on.",
            )
            .font(SMALL)
            .color(p.faint)
            .wrap_anywhere(),
        );
    card
}

/// The Application ID field.
fn field_row(s: &Ui, p: Palette, enabled: bool) -> El {
    // The check means the ID is the one being published under, not just that it is typed.
    let in_use = enabled && discord::valid_client_id(s.discord_client_id.text.trim());
    let mut field = row()
        .w_full()
        .h(40.0)
        .px(12.0)
        .radius(8.0)
        .fill(p.bg)
        .border(1.0, p.border)
        .center_y()
        .child(
            text_input(&s.discord_client_id, TextStyle::new(13.0, 400.0))
                .key("discord-client-id")
                .single_line()
                .grow(1.0)
                .color(p.text)
                .placeholder("Application ID (17–20 digits)", p.faint)
                .on_edit(|s: &mut Ui| &mut s.discord_client_id)
                .on_submit(|s: &mut Ui, _: &mut rune::Tx| s.commit_discord_client_id()),
        );
    if in_use {
        // A hint that the field is what the connection follows, without another control.
        field = field.child(icon(&ICON_CHECK, 14.0).color(p.accent));
    }
    field
}

/// The "Activity name" field: what Discord shows after "Listening to".
fn activity_name_row(s: &Ui, p: Palette) -> El {
    col()
        .w_full()
        .gap(4.0)
        .child(
            text("Activity name")
                .font(SMALL)
                .color(p.dim),
        )
        .child(
            row()
                .w_full()
                .h(40.0)
                .px(12.0)
                .radius(8.0)
                .fill(p.bg)
                .border(1.0, p.border)
                .center_y()
                .child(
                    text_input(&s.discord_activity_name, TextStyle::new(13.0, 400.0))
                        .key("discord-activity-name")
                        .single_line()
                        .grow(1.0)
                        .color(p.text)
                        .placeholder("{title} – {artist}", p.faint)
                        .on_edit(|s: &mut Ui| &mut s.discord_activity_name)
                        .on_submit(|s: &mut Ui, _: &mut rune::Tx| s.commit_discord_activity_name()),
                ),
        )
        .child(
            text("Shown after \"Listening to\". {title}, {artist} and {album} are filled in from the playing track. Leave empty to use the application's own name.")
                .font(SMALL)
                .color(p.faint)
                .wrap_anywhere(),
        )
}

/// The live status line: the client thread's own state, or the reason there is none.
fn status_row(s: &Ui, p: Palette) -> El {
    let status = s.discord_status.clone().unwrap_or(Status::NotRunning);
    let colour = if status.is_connected() {
        p.accent
    } else if matches!(status, Status::Error(_)) {
        p.danger
    } else {
        p.dim
    };
    let hint = match &status {
        Status::Connected(_) => "Your Discord friends see the current track.",
        Status::Connecting => "Looking for the Discord client on this machine.",
        Status::NotRunning => "Start the Discord desktop app and tsun will connect.",
        Status::Error(_) => "Check the Application ID, then Discord itself.",
    };
    row()
        .key("discord-status")
        .label(status.label())
        .w_full()
        .pt(6.0)
        .gap(8.0)
        .center_y()
        .child(el().size(6.0, 6.0).radius(3.0).fill(colour))
        .child(
            col().grow(1.0).min_w(0.0).gap(2.0)
                .child(text(status.label()).font(BODY).color(colour))
                .child(text(hint).font(SMALL).color(p.dim).wrap_anywhere()),
        )
}

impl Ui {
    /// Keeps the Discord client in step with the settings and the Application ID field. Called
    /// from `poll`, so typing an ID starts the connection once the field settles and a switch
    /// retunes the running client instead of reconnecting it.
    pub(crate) fn sync_discord(&mut self) {
        let typed = self.discord_client_id.text.trim().to_owned();
        if typed != self.settings.discord_client_id {
            self.settings.discord_client_id = typed;
            self.settings.save();
        }
        // The activity name changes what is *published*, never the connection, so it is applied
        // as it is typed (trimmed, and cut to Discord's 128-character limit).
        let name: String = self
            .discord_activity_name
            .text
            .trim()
            .chars()
            .take(crate::discord::MAX_ACTIVITY_NAME_CHARS)
            .collect();
        if name != self.settings.discord_activity_name {
            self.settings.discord_activity_name = name;
            self.settings.save();
        }
        if !self.settings.discord_enabled {
            self.stop_discord();
            return;
        }
        if self.discord_client_id.focused {
            // Still being typed in: wait for the field to settle (`commit` or a blur) before
            // opening a connection with a half-typed ID.
            return;
        }
        let wanted = self.discord_config();
        if !discord::valid_client_id(&wanted.client_id) {
            self.stop_discord();
            return;
        }
        let id_changed = self
            .discord_config
            .as_ref()
            .is_some_and(|current| current.client_id != wanted.client_id);
        if id_changed {
            // A different application means a different Discord session.
            self.discord = None;
            self.discord_config = None;
        }
        match &self.discord {
            None => {
                self.discord = Some(discord::Discord::start(wanted.clone(), self.waker.clone()));
                self.discord_config = Some(wanted);
                // A fresh session starts with no presence: put the track that is playing back
                // on it, so an Application ID change does not blank the profile until the next
                // track change.
                if let (Some(discord), Some(playing)) = (&self.discord, self.now_playing.as_ref()) {
                    let track = crate::media_events::MediaTrack::from_now_playing(playing);
                    discord.republish(&track, playing.elapsed as f64, self.paused);
                }
            }
            Some(discord) => {
                if self.discord_config.as_ref() != Some(&wanted) {
                    discord.configure(wanted.clone());
                    self.discord_config = Some(wanted);
                }
            }
        }
    }

    /// The Application ID field's own configuration.
    pub(crate) fn discord_config(&self) -> Config {
        Config {
            client_id: self.settings.discord_client_id.trim().to_owned(),
            name: self.settings.discord_activity_name.trim().to_owned(),
            show_art: self.settings.discord_show_art,
            show_time: self.settings.discord_show_time,
            show_button: self.settings.discord_show_button,
        }
    }

    /// The user committed the Activity name field (Enter, or a press elsewhere): apply it.
    pub(crate) fn commit_discord_activity_name(&mut self) {
        self.discord_activity_name.focused = false;
        self.sync_discord();
    }

    /// The user committed the Application ID field (Enter, or a press elsewhere): apply it.
    pub(crate) fn commit_discord_client_id(&mut self) {
        self.discord_client_id.focused = false;
        self.sync_discord();
    }

    /// The enable switch. Dropping the client clears the presence on Discord and stops the
    /// thread, so turning this off really stops the presence.
    pub(crate) fn set_discord_enabled(&mut self, on: bool) {
        self.settings.discord_enabled = on;
        self.settings.save();
        if on {
            self.discord_client_id.focused = false;
            self.sync_discord();
        } else {
            self.stop_discord();
        }
    }

    pub(crate) fn set_discord_show_art(&mut self, on: bool) {
        self.settings.discord_show_art = on;
        self.settings.save();
        self.sync_discord();
    }

    pub(crate) fn set_discord_show_time(&mut self, on: bool) {
        self.settings.discord_show_time = on;
        self.settings.save();
        self.sync_discord();
    }

    pub(crate) fn set_discord_show_button(&mut self, on: bool) {
        self.settings.discord_show_button = on;
        self.settings.save();
        self.sync_discord();
    }

    /// Stops the client and forgets its status; the card falls back to "Not running".
    pub(crate) fn stop_discord(&mut self) {
        if self.discord.take().is_some() {
            self.discord_status = None;
        }
        self.discord_config = None;
    }
}

fn card(p: Palette) -> El {
    col().w_full().max_w(620.0).padding(16.0).gap(6.0).radius(10.0)
        .fill(p.card).border(1.0, p.border)
}

fn section(title: &str, p: Palette) -> El {
    text(title).font(CAPS).color(p.faint)
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
    row()
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
        .child(check(p, on))
}

/// A check mark, bright when `on`.
fn check(p: Palette, on: bool) -> El {
    el().size(20.0, 20.0).center().radius(10.0)
        .fill(if on { p.accent } else { Color::TRANSPARENT })
        .border(1.0, if on { p.accent } else { p.border })
        .child(icon(&ICON_CHECK, 12.0).color(if on { p.accent_ink } else { p.faint }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Ui, init};
    use crate::settings::Settings;
    use crate::theme::palette;
    use rune::TextEdit;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn card_view(s: &Ui, _: &mut rune::Cx<Ui>) -> El {
        discord_card(s, palette())
    }

    fn runtime(state: Ui) -> rune::Runtime<Ui> {
        // Tall enough that every row of the card is laid out inside the window.
        let mut rt = rune::Runtime::new(state, card_view, (620.0, 1100.0), rune::Env::new(true, 1.0));
        rt.frame(0.0);
        rt
    }

    /// Every visible text of the frame.
    fn texts(rt: &rune::Runtime<Ui>) -> Vec<String> {
        rt.elements().into_iter().filter_map(|element| element.text).collect()
    }

    fn click(rt: &mut rune::Runtime<Ui>, x: f32, y: f32) {
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// `XDG_CONFIG_HOME` is process-wide, so the tests that write settings take turns and each
    /// points it at a directory of its own: otherwise one test would load another's settings (or
    /// the user's own file), and an enabled card with a valid Application ID would even look for
    /// a real Discord socket. The lock lives in [`crate::settings`], so tests elsewhere that
    /// merely *read* the settings wait their turn too.
    use crate::settings::lock_config_env;

    /// Points `XDG_CONFIG_HOME` at a temporary directory, so the settings a press writes never
    /// touch the user's own file.
    struct ConfigHome {
        previous: Option<OsString>,
        dir: PathBuf,
        _turn: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigHome {
        fn new(name: &str) -> ConfigHome {
            let turn = lock_config_env();
            let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("tsun-discord-cfg-{}-{name}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: every test that reads or writes settings through these paths holds
            // `CONFIG_HOME`, and the value is restored on drop.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
            ConfigHome { previous, dir, _turn: turn }
        }

        /// The settings file this test writes, inside its own directory.
        fn settings_path(&self) -> PathBuf {
            self.dir.join("tsun").join("settings.json")
        }
    }

    impl Drop for ConfigHome {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_card_shows_the_live_status_and_the_application_id_rule() {
        let _config = ConfigHome::new("card");
        let mut state = init();
        state.discord_status = Some(Status::Connected("ana".to_owned()));
        state.settings.discord_enabled = true;
        let rt = runtime(state);
        let shown = texts(&rt);
        assert!(shown.iter().any(|text| text == "Connected as ana"), "{shown:?}");
        assert!(shown.iter().any(|text| text.contains("Your Discord friends")), "{shown:?}");
        assert!(rt.find("discord-client-id").is_some(), "the field is there");
        assert!(rt.find("discord-activity-name").is_some(), "the activity-name field is there");
        assert!(shown.iter().any(|text| text == "Activity name"), "{shown:?}");
        assert!(
            shown.iter().any(|text| text.contains("Other people see the button; Discord hides it from you")),
            "the button switch says who sees it: {shown:?}"
        );
        assert!(rt.find("discord-show-art").is_some());
        assert!(rt.find("discord-show-time").is_some());
        assert!(rt.find("discord-button").is_some());

        // A too-short Application ID is called out, and the error state is shown verbatim.
        let mut state = init();
        state.settings.discord_enabled = true;
        state.discord_client_id = TextEdit::new("12345");
        state.settings.discord_client_id = "12345".to_owned();
        state.discord_status = Some(Status::Error("Invalid Client ID (4000)".to_owned()));
        let rt = runtime(state);
        let shown = texts(&rt);
        assert!(shown.iter().any(|text| text.contains("17 to 20 digits")), "{shown:?}");
        assert!(
            shown.iter().any(|text| text == "Error: Invalid Client ID (4000)"),
            "{shown:?}"
        );

        // While the feature is off there is no status line at all.
        let rt = runtime(init());
        assert!(!texts(&rt).iter().any(|text| text == "Not running"), "no status line while off");
    }

    #[test]
    fn pressing_the_enable_switch_flips_and_persists_the_setting() {
        let _config = ConfigHome::new("toggle");
        let mut rt = runtime(init());
        assert!(!rt.state.settings.discord_enabled);
        assert!(!texts(&rt).iter().any(|text| text == "Not running"), "off means no status");
        let (x, y) = rt.find("discord-enabled").expect("the enable row");
        click(&mut rt, x, y);
        assert!(rt.state.settings.discord_enabled, "the switch flips the setting");
        let texts = texts(&rt);
        assert!(texts.iter().any(|text| text == "Not running"), "{texts:?}");
        // It is written out, so it survives a restart. (No client is started here: the empty
        // Application ID is invalid, so nothing ever looks for a Discord socket.)
        assert!(Settings::load().discord_enabled);
        let (x, y) = rt.find("discord-enabled").expect("the enable row");
        click(&mut rt, x, y);
        assert!(!rt.state.settings.discord_enabled);
        assert!(!Settings::load().discord_enabled);
    }

    #[test]
    fn the_activity_name_field_persists_and_lands_in_the_config() {
        let config = ConfigHome::new("name");
        let mut state = init();
        state.settings.discord_enabled = true;
        state.discord_client_id = TextEdit::new("12345678901234567");
        state.settings.discord_client_id = "12345678901234567".to_owned();
        // The Application ID field is still focused, so `sync_discord` waits before connecting:
        // this test must never look for a Discord socket (`Discord::start` searches the real
        // runtime dirs).
        state.discord_client_id.focused = true;
        // Typed text is trimmed, capped at Discord's 128 characters and persisted as it goes.
        state.discord_activity_name = TextEdit::new(format!("  {}  ", "N".repeat(200)));
        state.sync_discord();
        assert_eq!(state.settings.discord_activity_name.chars().count(), crate::discord::MAX_ACTIVITY_NAME_CHARS);
        assert_eq!(state.discord_config().name.chars().count(), crate::discord::MAX_ACTIVITY_NAME_CHARS);
        assert_eq!(Settings::load().discord_activity_name, state.settings.discord_activity_name);
        // The file it wrote is valid JSON carrying both the name and the ID.
        let text = std::fs::read_to_string(config.settings_path()).expect("the settings file");
        let saved: Settings = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(saved.discord_activity_name, state.settings.discord_activity_name);
        assert_eq!(saved.discord_client_id, "12345678901234567");
        // Clearing it means "use the application's own name".
        state.discord_activity_name = TextEdit::new("   ".to_owned());
        state.sync_discord();
        assert_eq!(state.settings.discord_activity_name, "");
        assert_eq!(state.discord_config().name, "", "empty, so no `name` field is sent");
        assert!(state.discord.is_none(), "nothing connects while the ID field is being edited");
    }

    #[test]
    fn start_and_stop_follow_the_settings_without_a_valid_application_id() {
        let _config = ConfigHome::new("start-stop");
        let mut state = init();
        // Enabled but no Application ID: no client, and no socket is ever touched.
        state.settings.discord_enabled = true;
        state.settings.discord_client_id = String::new();
        state.discord_client_id = TextEdit::new("");
        state.sync_discord();
        assert!(state.discord.is_none(), "an empty ID cannot start a client");
        assert!(state.discord_status.is_none());
        state.settings.discord_client_id = "not-a-number".to_owned();
        state.sync_discord();
        assert!(state.discord.is_none());
        // Disabling leaves everything stopped.
        state.set_discord_enabled(false);
        assert!(state.discord.is_none());
    }
}
