//! Recent searches, persisted as JSON beside the TIDAL session so the search box can offer
//! them when it is focused and empty.
//!
//! The file lives at `$XDG_CONFIG_HOME/tsun/recent_searches.json` (or
//! `~/.config/tsun/recent_searches.json` when `XDG_CONFIG_HOME` is unset), is written `0600`
//! inside a `0700` directory, holds at most [`MAX`] entries, and is de-duplicated
//! case-insensitively with the newest first.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How many recent searches are kept.
pub(crate) const MAX: usize = 10;

#[derive(Serialize, Deserialize, Default)]
struct Store {
    #[serde(default)]
    searches: Vec<String>,
}

/// The stored recent searches, newest first. Unreadable or malformed files read as empty.
pub(crate) fn load() -> Vec<String> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(bytes) = fs::read(&path) else { return Vec::new() };
    let Ok(store) = serde_json::from_slice::<Store>(&bytes) else { return Vec::new() };
    let mut searches: Vec<String> = Vec::new();
    for entry in store.searches {
        let entry = entry.trim().to_owned();
        if !entry.is_empty() && !searches.iter().any(|kept| kept.eq_ignore_ascii_case(&entry)) {
            searches.push(entry);
        }
    }
    searches.truncate(MAX);
    searches
}

/// Records `query` as the newest search and persists the list. Returns `true` when it changed.
pub(crate) fn remember(searches: &mut Vec<String>, query: &str) -> bool {
    if push_recent(searches, query) {
        save(searches);
        true
    } else {
        false
    }
}

/// Moves `query` to the front of `searches`, de-duplicating case-insensitively and keeping at
/// most [`MAX`] entries. Returns `true` when the list changed.
fn push_recent(searches: &mut Vec<String>, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return false;
    }
    let existing = searches.iter().position(|entry| entry.eq_ignore_ascii_case(query));
    if existing == Some(0) && searches[0] == query {
        return false;
    }
    if let Some(index) = existing {
        searches.remove(index);
    }
    searches.insert(0, query.to_owned());
    searches.truncate(MAX);
    true
}

/// Removes every recent search.
pub(crate) fn clear(searches: &mut Vec<String>) {
    if searches.is_empty() {
        return;
    }
    searches.clear();
    save(searches);
}

fn save(searches: &[String]) {
    let Some(path) = path() else { return };
    if let Some(directory) = path.parent() {
        if fs::create_dir_all(directory).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o700));
        }
    }
    let Ok(json) = serde_json::to_vec_pretty(&Store { searches: searches.to_vec() }) else { return };
    let temporary = path.with_extension("json.tmp");
    let _ = fs::remove_file(&temporary);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let Ok(mut file) = options.open(&temporary) else { return };
    if file.write_all(&json).is_err() || file.sync_all().is_err() {
        let _ = fs::remove_file(&temporary);
        return;
    }
    drop(file);
    let _ = fs::rename(&temporary, &path);
}

fn path() -> Option<PathBuf> {
    Some(crate::paths::config_dir()?.join("recent_searches.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_recent_dedupes_case_insensitively_and_keeps_newest_first() {
        let mut searches = vec!["Muse".to_owned(), "Radiohead".to_owned()];
        assert!(push_recent(&mut searches, "radiohead"));
        assert_eq!(searches, ["radiohead", "Muse"]);
        // Re-remembering the entry already at the front changes nothing.
        assert!(!push_recent(&mut searches, "radiohead"));
        assert_eq!(searches, ["radiohead", "Muse"]);
    }

    #[test]
    fn push_recent_caps_at_max() {
        let mut searches = Vec::new();
        for index in 0..(MAX + 5) {
            push_recent(&mut searches, &format!("query {index}"));
        }
        assert_eq!(searches.len(), MAX);
        assert_eq!(searches[0], format!("query {}", MAX + 4));
    }

    #[test]
    fn push_recent_ignores_blank_queries() {
        let mut searches = Vec::new();
        assert!(!push_recent(&mut searches, "   "));
        assert!(searches.is_empty());
    }
}
