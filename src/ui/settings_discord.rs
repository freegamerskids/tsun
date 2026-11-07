//! The Settings card for Discord Rich Presence.
//!
//! The card owns the whole feature's user-facing side: the Application ID field (Discord shows
//! "Listening to <that application's name>", so it recommends naming the application "TIDAL"),
//! the enable switch, the three display switches and the live connection status. Everything the
//! switches say is real: the switches are handed to the running [`Discord`](crate::discord)
//! client and change what it publishes, and the status line is the client thread's own state.

use rune::{El, TextStyle, col, icon, row, text, text_input};

use crate::app::Ui;
use crate::discord::{self, Config, Status, client_id_is_usable};
use crate::theme::{ICON_CHECK, SMALL, Palette};
use crate::ui::settings_widgets::{self as w, Tone};

/// The Discord Rich Presence card.
pub(crate) fn discord_card(s: &Ui, p: Palette) -> El {
    let enabled = s.settings.discord_enabled;
    let status = enabled.then(|| s.discord_status.clone().unwrap_or(Status::NotRunning));
    let badge = status
        .as_ref()
        .map(|status| w::status_badge(p, status_tone(status), &status.label()));
    let mut card = w::card(p)
        .key("discord-card")
        .child(
            row().w_full().padding(14.0).child(w::card_header(
                p,
                "Discord Rich Presence",
                "Tell your Discord profile what you are listening to.",
                badge,
            )),
        )
        .child(w::toggle_row(
            p,
            "discord-enabled",
            "Show what I'm listening to",
            Some("Publishes the current track as your Discord activity, through the built-in application (or the one you name below)."),
            enabled,
            |s: &mut Ui| s.set_discord_enabled(!s.settings.discord_enabled),
        ));
    if let Some(status) = &status {
        card = card.child(
            row()
                .key("discord-status")
                .w_full()
                .px(14.0)
                .pb(12.0)
                .child(w::note(
                    w::tone_color(status_tone(status), p),
                    status_hint(status),
                )),
        );
    }
    card = card.child(w::divider(p)).child(
        col()
            .w_full()
            .padding(14.0)
            .gap(16.0)
            .child(field_row(s, p, enabled))
            .child(activity_name_row(s, p)),
    );
    card = card
        .child(w::divider(p))
        .child(w::toggle_row(
            p,
            "discord-show-art",
            "Show album art",
            Some("Sends the TIDAL cover as the activity's large image (Discord fetches it itself)."),
            s.settings.discord_show_art,
            |s: &mut Ui| s.set_discord_show_art(!s.settings.discord_show_art),
        ))
        .child(w::divider(p))
        .child(w::toggle_row(
            p,
            "discord-show-time",
            "Show elapsed and remaining time",
            Some("Sends start and end times so Discord draws the progress bar; it follows seeking."),
            s.settings.discord_show_time,
            |s: &mut Ui| s.set_discord_show_time(!s.settings.discord_show_time),
        ))
        .child(w::divider(p))
        .child(w::toggle_row(
            p,
            "discord-button",
            "Show a Listen on TIDAL button",
            Some("Adds a button linking to the track's TIDAL page. Other people see the button; Discord hides it from you, and local files have no TIDAL page."),
            s.settings.discord_show_button,
            |s: &mut Ui| s.set_discord_show_button(!s.settings.discord_show_button),
        ))
        .child(
            text("Paused playback is sent without timestamps and with \"Paused\" in the state, because Discord has no paused flag.")
                .font(SMALL)
                .color(p.faint)
                .px(14.0)
                .py(10.0)
                .wrap(),
        );
    card
}

/// The tone the status badge and its note draw in.
fn status_tone(status: &Status) -> Tone {
    if status.is_connected() {
        Tone::Ok
    } else if matches!(status, Status::Connecting) {
        Tone::Warn
    } else if matches!(status, Status::Error(_)) {
        Tone::Error
    } else {
        Tone::Off
    }
}

/// What to do about the current status.
fn status_hint(status: &Status) -> &'static str {
    match status {
        Status::Connected(_) => "Your Discord friends see the current track.",
        Status::Connecting => "Looking for the Discord client on this machine.",
        Status::NotRunning => "Start the Discord desktop app and tsun will connect.",
        Status::Error(_) => "Check the Application ID, then Discord itself.",
    }
}

/// The Application ID field: an optional override for the built-in application.
fn field_row(s: &Ui, p: Palette, enabled: bool) -> El {
    let typed = s.discord_client_id.text.trim();
    // The check means the presence is being published under a valid application: the built-in
    // one when the field is empty, or the typed override.
    let in_use = enabled && client_id_is_usable(typed);
    let ok = client_id_is_usable(typed);
    let mut field = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            text_input(&s.discord_client_id, TextStyle::new(13.0, 400.0))
                .key("discord-client-id")
                .single_line()
                .grow(1.0)
                .color(p.text)
                .placeholder("Built-in application (leave empty)", p.faint)
                .on_edit(|s: &mut Ui| &mut s.discord_client_id)
                .on_submit(|s: &mut Ui, _: &mut rune::Tx| s.commit_discord_client_id()),
        );
    if in_use {
        field = field.child(icon(&ICON_CHECK, 14.0).color(p.accent));
    }
    let hint = if ok {
        format!(
            "Empty uses tsun's built-in application ({}). Set your own Application ID only to use \
             your own application's name, icon or art assets.",
            crate::discord::effective_client_id("")
        )
    } else {
        "An Application ID is 17 to 20 digits, or empty for the built-in application.".to_owned()
    };
    let colour = if ok { p.faint } else { p.danger };
    w::field_row_with(
        p,
        "Application ID",
        w::field_frame(field, p),
        Some((&hint, colour)),
    )
}

/// The "Activity name" field: what Discord shows after "Listening to".
fn activity_name_row(s: &Ui, p: Palette) -> El {
    let field = row()
        .w_full()
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
        );
    w::field_row(
        p,
        "Activity name",
        w::field_frame(field, p),
        Some("Shown after \"Listening to\". {title}, {artist} and {album} come from the playing track; empty uses the application's own name."),
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
        if !discord::client_id_is_usable(&self.settings.discord_client_id) {
            // A non-empty id that is not 17–20 digits: nothing to connect under, and the card
            // says so.
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

    /// The Application ID field's own configuration. A blank field means the built-in
    /// application, so the client never sees an empty id.
    pub(crate) fn discord_config(&self) -> Config {
        Config {
            client_id: discord::effective_client_id(&self.settings.discord_client_id).to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Ui, init};
    use crate::settings::Settings;
    use crate::theme::palette;
    use rune::TextEdit;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
        assert!(
            shown.iter().any(|text| text.contains("tsun's built-in application")),
            "the card says it works with no setup: {shown:?}"
        );
        assert!(
            shown.iter().any(|text| text.contains(crate::discord::DEFAULT_CLIENT_ID)),
            "the built-in application id is named: {shown:?}"
        );
        assert!(
            !shown.iter().any(|text| text.contains("create one at discord.com")),
            "no setup is required any more: {shown:?}"
        );
        assert!(rt.find("discord-client-id").is_some(), "the override field is there");
        assert!(rt.find("discord-activity-name").is_some(), "the activity-name field is there");
        assert!(shown.iter().any(|text| text == "Activity name"), "{shown:?}");
        assert!(
            shown.iter().any(|text| text.contains("Other people see the button; Discord hides it from you")),
            "the button switch says who sees it: {shown:?}"
        );
        assert!(rt.find("discord-show-art").is_some());
        assert!(rt.find("discord-show-time").is_some());
        assert!(rt.find("discord-button").is_some());

        // A malformed override is called out; an empty field is not (it means the built-in
        // application), and the error state is shown verbatim.
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
        let rt = runtime({
            let mut state = init();
            state.settings.discord_enabled = true;
            state
        });
        assert!(
            !texts(&rt).iter().any(|text| text.contains("17 to 20 digits")),
            "an empty field is the built-in application, not an error"
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
    fn an_empty_id_uses_the_built_in_application_and_never_a_real_discord() {
        let _config = ConfigHome::new("built-in");
        let mut state = init();
        // Enabled with an empty field: the built-in application, exactly what a fresh install
        // does. The client it starts is confined to the private test path (see
        // `test_runtime_dir`), so this can never reach the Discord on this machine: it finds no
        // socket and reports "Not running".
        state.settings.discord_enabled = true;
        state.settings.discord_client_id = String::new();
        state.discord_client_id = TextEdit::new("");
        state.sync_discord();
        assert_eq!(state.discord_config().client_id, crate::discord::DEFAULT_CLIENT_ID);
        assert!(state.discord.is_some(), "the built-in application needs no setup");
        // The thread works down to "Not running": it looked for a socket in the private test
        // path only, which holds none.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut status = state.discord.as_ref().unwrap().status();
        while status != Status::NotRunning && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            status = state.discord.as_ref().unwrap().status();
        }
        assert_eq!(
            status,
            Status::NotRunning,
            "the private test path holds no Discord socket, and the real one is never searched"
        );
        // A malformed override stops it again: nothing to publish under. (The card's field is
        // what the settings follow, so the field is what changes here.)
        state.discord_client_id = TextEdit::new("not-a-number");
        state.sync_discord();
        assert!(state.settings.discord_client_id == "not-a-number", "the malformed text is kept");
        assert!(state.discord.is_none(), "a malformed override connects to nothing");
        assert!(!crate::discord::client_id_is_usable("not-a-number"));
        // Disabling leaves everything stopped.
        state.set_discord_enabled(false);
        assert!(state.discord.is_none());
    }

    #[test]
    fn a_typed_id_overrides_the_built_in_application() {
        let _config = ConfigHome::new("override");
        let mut state = init();
        state.settings.discord_enabled = true;
        state.discord_client_id = TextEdit::new("12345678901234567");
        state.settings.discord_client_id = "12345678901234567".to_owned();
        assert_eq!(state.discord_config().client_id, "12345678901234567");
        // Clearing the field goes back to the built-in application, without touching the network
        // (the client is confined to the private test path).
        state.discord_client_id = TextEdit::new(String::new());
        state.sync_discord();
        assert_eq!(state.settings.discord_client_id, "");
        assert_eq!(state.discord_config().client_id, crate::discord::DEFAULT_CLIENT_ID);
        state.stop_discord();
        assert!(state.discord.is_none());
    }
}
