//! Persisted user preferences, so the client reopens the way the user left it.
//!
//! Stored as JSON at `$XDG_CONFIG_HOME/tsun/settings.json` (or `~/.config/tsun/settings.json`)
//! with `0600` permissions, separately from the TIDAL session so signing out never drops the
//! user's volume, shuffle or quality choices. Every field has a serde default: an old or
//! partial file loads cleanly, and a corrupt one falls back to [`Settings::default`].

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::playback::Repeat;
use crate::tidal::Quality;

fn default_volume() -> f32 {
    1.0
}

/// Where the app opens after sign-in. Chosen in Settings, persisted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StartupPage {
    /// The Home ("For you") page.
    #[default]
    Home,
    Collection,
    /// The page that was open when the app last closed.
    LastVisited,
    /// An unrecognised value from a hand-edited file, treated as [`StartupPage::Home`] so one bad
    /// string never resets the whole settings file.
    Unknown,
}

impl StartupPage {
    /// Every option, in the order Settings lists them.
    pub(crate) const ALL: [StartupPage; 3] =
        [StartupPage::Home, StartupPage::Collection, StartupPage::LastVisited];

    pub(crate) fn label(self) -> &'static str {
        match self {
            StartupPage::Home => "Home",
            StartupPage::Collection => "Collection",
            StartupPage::LastVisited => "Last visited",
            StartupPage::Unknown => "Home",
        }
    }

    /// The stable slug persisted in settings.json (kebab-case).
    pub(crate) fn key(self) -> &'static str {
        match self {
            StartupPage::Home | StartupPage::Unknown => "home",
            StartupPage::Collection => "collection",
            StartupPage::LastVisited => "last-visited",
        }
    }

    fn parse(value: &str) -> StartupPage {
        match value {
            "home" => StartupPage::Home,
            "collection" => StartupPage::Collection,
            "last-visited" => StartupPage::LastVisited,
            _ => StartupPage::Unknown,
        }
    }
}

impl Serialize for StartupPage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for StartupPage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(StartupPage::parse(&value))
    }
}

/// The persisted preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    /// Output volume in `0.0..=1.0` (mute is tracked separately, so unmuting restores it).
    pub(crate) volume: f32,
    pub(crate) muted: bool,
    /// Whether the mouse wheel over the volume control adjusts the volume. Off by default, so
    /// scrolling a page with the pointer near the volume bar never changes it.
    pub(crate) volume_scroll: bool,
    pub(crate) shuffle: bool,
    pub(crate) repeat: Repeat,
    /// Preferred streaming quality; the client requests this tier first.
    pub(crate) quality: Quality,
    /// Whether ReplayGain-style volume normalisation is applied to the stream's reported gain.
    pub(crate) normalize: bool,
    /// The output device to open, by its cpal name; `None` follows the system default.
    /// A device that is no longer present is reported as an error notice when playback starts.
    pub(crate) output_device: Option<String>,
    /// Whether a freedesktop desktop notification is posted when the track changes.
    pub(crate) notifications: bool,
    /// Where the app opens after sign-in.
    pub(crate) startup_page: StartupPage,
    /// The route that was open when the app last closed, for [`StartupPage::LastVisited`]. A
    /// slug plus an optional id (see [`crate::history::Route::persist`]).
    pub(crate) last_route: Option<String>,
    /// Whether closing the window hides it to the tray instead of quitting. Off by default; the
    /// tray must exist for the setting to be offered.
    pub(crate) close_to_tray: bool,
    /// Whether the Discord Rich Presence is published from the user's own Discord application.
    /// Off by default.
    pub(crate) discord_enabled: bool,
    /// An optional override for the Discord Application ID the presence is published under
    /// (17–20 digits). Empty — the default — means tsun's own built-in application, so the
    /// presence works without any setup; a typed id uses that application's name and assets.
    pub(crate) discord_client_id: String,
    /// What Discord shows after "Listening to", replacing the application's own name. Empty (the
    /// default) means the application's name.
    pub(crate) discord_activity_name: String,
    /// Whether the album art (the TIDAL cover URL) is the activity's large image.
    #[serde(default = "crate::discord::default_true")]
    pub(crate) discord_show_art: bool,
    /// Whether start/end timestamps are sent, so Discord draws the elapsed bar.
    #[serde(default = "crate::discord::default_true")]
    pub(crate) discord_show_time: bool,
    /// Whether a "Listen on TIDAL" button is offered for TIDAL tracks. Local files never have
    /// one, because they have no TIDAL page.
    #[serde(default = "crate::discord::default_true")]
    pub(crate) discord_show_button: bool,
    /// The music folders the local-file scanner walks. Empty by default.
    pub(crate) local_folders: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            volume: default_volume(),
            muted: false,
            volume_scroll: false,
            shuffle: false,
            repeat: Repeat::Off,
            quality: Quality::default(),
            normalize: false,
            output_device: None,
            notifications: false,
            startup_page: StartupPage::default(),
            last_route: None,
            close_to_tray: false,
            discord_enabled: false,
            discord_client_id: String::new(),
            discord_activity_name: "{title} – {artist}".to_owned(),
            discord_show_art: true,
            discord_show_time: true,
            discord_show_button: true,
            local_folders: Vec::new(),
        }
    }
}

impl Settings {
    /// Loads the settings, falling back to defaults when the file is missing or unreadable.
    pub(crate) fn load() -> Settings {
        let Some(path) = path() else {
            return Settings::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    /// Writes the settings back, best-effort: a failed save must never disrupt playback.
    pub(crate) fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let Ok(text) = serde_json::to_vec_pretty(self) else { return };
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        if let Ok(mut file) = options.open(&path) {
            let _ = file.write_all(&text);
        }
    }
}

/// `$XDG_CONFIG_HOME/tsun/settings.json`, or `~/.config/tsun/settings.json`.
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("tsun").join("settings.json"))
}

/// `XDG_CONFIG_HOME` is process-wide, so tests that *write* settings (through
/// `ConfigHome` in the Discord card's tests) take turns on this lock, and tests that merely read
/// the settings through [`Settings::load`] — every test that builds a `Ui` — take it too, so a
/// read can never race the environment being repointed at a scratch directory.
#[cfg(test)]
pub(crate) fn lock_config_env() -> std::sync::MutexGuard<'static, ()> {
    static CONFIG_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
    CONFIG_ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let settings = Settings::default();
        assert_eq!(settings.volume, 1.0);
        assert!(!settings.muted);
        assert!(!settings.shuffle);
        assert_eq!(settings.repeat, Repeat::Off);
        assert_eq!(settings.quality, Quality::HiResLossless);
        assert!(!settings.normalize);
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"volume":0.25,"shuffle":true}"#).unwrap();
        assert_eq!(settings.volume, 0.25);
        assert!(settings.shuffle);
        assert!(!settings.muted);
        assert_eq!(settings.repeat, Repeat::Off);
        assert_eq!(settings.quality, Quality::HiResLossless);
    }

    #[test]
    fn round_trips_through_json() {
        let settings = Settings {
            volume: 0.4,
            muted: true,
            volume_scroll: true,
            shuffle: true,
            repeat: Repeat::One,
            quality: Quality::Lossless,
            normalize: true,
            output_device: Some("Built-in Audio Analog Stereo".to_owned()),
            notifications: true,
            startup_page: StartupPage::Collection,
            last_route: Some("playlist:abc".to_owned()),
            close_to_tray: true,
            discord_enabled: true,
            discord_client_id: "12345678901234567".to_owned(),
            discord_activity_name: "TIDAL".to_owned(),
            discord_show_art: false,
            discord_show_time: false,
            discord_show_button: false,
            local_folders: vec!["/music".to_owned()],
        };
        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(settings, back);
    }

    #[test]
    fn old_files_without_new_fields_load_with_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"volume":0.5}"#).unwrap();
        assert_eq!(settings.output_device, None);
        assert!(!settings.notifications);
        assert!(!settings.volume_scroll, "the mouse wheel stays off unless the file turns it on");
        assert_eq!(settings.startup_page, StartupPage::Home);
        assert_eq!(settings.last_route, None);
        assert!(!settings.close_to_tray);
        assert!(!settings.discord_enabled, "the Discord presence is opt-in");
        assert_eq!(settings.discord_client_id, "");
        assert_eq!(settings.discord_activity_name, "{title} – {artist}", "title and artist by default");
        assert!(settings.discord_show_art && settings.discord_show_time && settings.discord_show_button);
        assert_eq!(
            serde_json::from_str::<Settings>(r#"{"discord_enabled":true}"#).unwrap().discord_show_art,
            true,
            "the display switches default on even in a partial file"
        );
    }

    #[test]
    fn the_volume_wheel_option_defaults_off_and_round_trips() {
        assert!(!Settings::default().volume_scroll, "the mouse wheel never touches the volume unless asked to");
        // A hand-written file that names it off, or one written before the field existed, both
        // keep the wheel out of the volume.
        assert!(!serde_json::from_str::<Settings>(r#"{"volume_scroll":false}"#).unwrap().volume_scroll);
        assert!(!serde_json::from_str::<Settings>(r#"{"volume":0.25}"#).unwrap().volume_scroll);
        let settings = Settings { volume_scroll: true, ..Settings::default() };
        let text = serde_json::to_string(&settings).unwrap();
        assert!(text.contains("\"volume_scroll\":true"), "{text}");
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert!(back.volume_scroll, "the choice comes back on the next launch");
    }

    #[test]
    fn startup_page_round_trips_administered_slugs() {
        for page in StartupPage::ALL {
            let settings = Settings { startup_page: page, ..Settings::default() };
            let text = serde_json::to_string(&settings).unwrap();
            let back: Settings = serde_json::from_str(&text).unwrap();
            assert_eq!(back.startup_page, page, "{text}");
        }
        assert!(serde_json::to_string(&Settings { startup_page: StartupPage::LastVisited, ..Settings::default() })
            .unwrap()
            .contains("\"last-visited\""));
    }

    #[test]
    fn an_unknown_startup_page_does_not_reset_the_rest_of_the_file() {
        let settings: Settings =
            serde_json::from_str(r#"{"volume":0.3,"startup_page":"nonsense","close_to_tray":true}"#)
                .unwrap();
        assert_eq!(settings.startup_page, StartupPage::Unknown);
        assert_eq!(settings.volume, 0.3, "the rest of the file is kept");
        assert!(settings.close_to_tray);
        assert_eq!(settings.startup_page.label(), "Home", "unknown behaves as Home");
    }

    #[test]
    fn last_route_and_close_to_tray_round_trip() {
        let settings = Settings {
            last_route: Some("album:1550545".to_owned()),
            close_to_tray: true,
            ..Settings::default()
        };
        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.last_route.as_deref(), Some("album:1550545"));
        assert!(back.close_to_tray);
    }
}
