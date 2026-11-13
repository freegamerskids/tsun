//! Where tsun keeps its files, and how it writes the private ones.
//!
//! `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` win on every platform when they name an absolute path.
//! Without them the config lives in `~/.config/tsun` (Linux, macOS, the BSDs) or
//! `%APPDATA%\tsun` (Windows), and the cache in `~/.cache/tsun` or `%LOCALAPPDATA%\tsun`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// tsun's configuration directory, or `None` when the environment names no home at all.
pub(crate) fn config_dir() -> Option<PathBuf> {
    Some(env_dir("XDG_CONFIG_HOME").or_else(default_config_base)?.join("tsun"))
}

/// tsun's cache directory, or `None` when the environment names no home at all.
pub(crate) fn cache_dir() -> Option<PathBuf> {
    Some(env_dir("XDG_CACHE_HOME").or_else(default_cache_base)?.join("tsun"))
}

/// Creates or truncates `path` and writes `bytes`, readable by the owner only where the
/// platform has file modes (`0600`); elsewhere the profile directory's own ACL protects it.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

/// An environment variable holding an absolute directory; anything else counts as unset.
fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

/// The unit tests get a scratch directory instead, so a test that navigates (which remembers
/// the route) can never rewrite the developer's settings.
#[cfg(test)]
fn default_config_base() -> Option<PathBuf> {
    Some(std::env::temp_dir().join("tsun-unit-test-config"))
}

#[cfg(all(not(test), not(windows)))]
fn default_config_base() -> Option<PathBuf> {
    env_dir("HOME").map(|home| home.join(".config"))
}

#[cfg(all(not(test), windows))]
fn default_config_base() -> Option<PathBuf> {
    env_dir("APPDATA")
}

#[cfg(not(windows))]
fn default_cache_base() -> Option<PathBuf> {
    env_dir("HOME").map(|home| home.join(".cache"))
}

#[cfg(windows)]
fn default_cache_base() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA")
}
