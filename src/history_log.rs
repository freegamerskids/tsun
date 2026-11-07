//! Local listening history.
//!
//! TIDAL's own history endpoints are 404 for this client's credentials, so this records what
//! *this app* plays: up to the [`MAX`] most recent distinct plays, newest first, persisted as
//! JSON at `$XDG_CONFIG_HOME/tsun/play_history.json` (or `~/.config/tsun/play_history.json`)
//! with `0600` permissions. A play is recorded once its track has actually been listened to —
//! past [`PLAY_THRESHOLD_SECS`], or when it reached its natural end — never when a track is
//! merely selected.
//!
//! Everything that decides *what* is recorded is pure and unit-tested here; the caller owns the
//! list and asks this module to load, record and persist it.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How many distinct plays are kept; older ones fall off the end.
pub(crate) const MAX: usize = 200;

/// How long a track must have played (seconds) before even a skip counts as a play.
pub(crate) const PLAY_THRESHOLD_SECS: u64 = 30;

/// One recorded play. Everything the History list and the Home row need to draw and replay it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlayEntry {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    #[serde(default)]
    pub(crate) cover: Option<String>,
    #[serde(default)]
    pub(crate) duration: Option<u64>,
    #[serde(default)]
    pub(crate) album_id: Option<u64>,
    #[serde(default)]
    pub(crate) artist_id: Option<u64>,
    /// Unix seconds the play was recorded at, used to keep the newest first.
    pub(crate) ts: u64,
}

impl PlayEntry {
    /// The track this play can be replayed as, for the shared track table and the Home row.
    pub(crate) fn track(&self) -> crate::tidal::Track {
        crate::tidal::Track {
            id: self.id,
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            cover: self.cover.clone(),
            duration: self.duration,
            album_id: self.album_id,
            artist_id: self.artist_id,
            explicit: false,
            quality: None,
            track_number: None,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Store {
    #[serde(default)]
    entries: Vec<PlayEntry>,
}

/// Whether a track that has played `elapsed` seconds counts as a play. `finished` is true when
/// the track reached its natural end, which always counts (a short track can be under the
/// threshold and still be listened to in full).
pub(crate) fn counts_as_played(elapsed: u64, finished: bool) -> bool {
    finished || elapsed > PLAY_THRESHOLD_SECS
}

/// Records `entry` as the newest play: it moves to the front, replacing any earlier play of the
/// same track, and the list is capped at [`MAX`]. Returns `true` when the stored history changed.
pub(crate) fn record(entries: &mut Vec<PlayEntry>, entry: PlayEntry) -> bool {
    let front = entries.first().map(|existing| (existing.id, existing.ts));
    entries.retain(|existing| existing.id != entry.id);
    let changed = front != Some((entry.id, entry.ts));
    entries.insert(0, entry);
    let truncated = entries.len() > MAX;
    entries.truncate(MAX);
    changed || truncated
}

/// Removes every play from the list.
pub(crate) fn clear(entries: &mut Vec<PlayEntry>) {
    entries.clear();
}

/// The stored history, newest first. Unreadable or malformed files read as empty.
pub(crate) fn load() -> Vec<PlayEntry> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(bytes) = fs::read(&path) else { return Vec::new() };
    let Ok(store) = serde_json::from_slice::<Store>(&bytes) else { return Vec::new() };
    let mut entries: Vec<PlayEntry> = store
        .entries
        .into_iter()
        .filter(|entry| entry.id != 0)
        .collect();
    entries.truncate(MAX);
    entries
}

/// Persists the history, best-effort: a failed write must never disrupt playback.
pub(crate) fn save(entries: &[PlayEntry]) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let Ok(text) = serde_json::to_vec_pretty(&Store { entries: entries.to_vec() }) else { return };
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(0o600);
    if let Ok(mut file) = options.open(&path) {
        let _ = file.write_all(&text);
    }
}

/// Unix seconds now, for a new entry's timestamp.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// `$XDG_CONFIG_HOME/tsun/play_history.json`, or `~/.config/tsun/play_history.json`.
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("tsun").join("play_history.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, ts: u64) -> PlayEntry {
        PlayEntry {
            id,
            title: format!("Track {id}"),
            artist: "Artist".into(),
            album: "Album".into(),
            cover: None,
            duration: Some(200),
            album_id: Some(1),
            artist_id: Some(2),
            ts,
        }
    }

    #[test]
    fn a_play_counts_past_the_threshold_or_when_finished() {
        assert!(!counts_as_played(0, false));
        assert!(!counts_as_played(PLAY_THRESHOLD_SECS, false), "at the threshold, not past it");
        assert!(counts_as_played(PLAY_THRESHOLD_SECS + 1, false));
        assert!(counts_as_played(0, true), "a finished short track counts");
    }

    #[test]
    fn recording_dedupes_by_track_and_moves_it_to_the_front() {
        let mut entries = Vec::new();
        assert!(record(&mut entries, entry(1, 100)));
        assert!(record(&mut entries, entry(2, 200)));
        assert!(!record(&mut entries, entry(2, 200)), "the same play twice changes nothing");
        assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), vec![2, 1]);
        // Replaying track 1 moves it back to the front instead of duplicating it.
        assert!(record(&mut entries, entry(1, 300)));
        assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn history_is_capped_at_max_entries() {
        let mut entries = Vec::new();
        for id in 1..=(MAX as u64 + 10) {
            record(&mut entries, entry(id, id));
        }
        assert_eq!(entries.len(), MAX);
        assert_eq!(entries.first().unwrap().id, MAX as u64 + 10);
        assert_eq!(entries.last().unwrap().id, 11, "the oldest entries fell off");
    }

    #[test]
    fn clearing_empties_the_history() {
        let mut entries = vec![entry(1, 1), entry(2, 2)];
        clear(&mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn entries_round_trip_through_json() {
        let entries = vec![entry(7, 1_700_000_000)];
        let text = serde_json::to_string(&Store { entries: entries.clone() }).unwrap();
        let back: Store = serde_json::from_str(&text).unwrap();
        assert_eq!(back.entries, entries);
    }

    #[test]
    fn an_entry_maps_back_to_a_playable_track() {
        let track = entry(9, 5).track();
        assert_eq!(track.id, 9);
        assert_eq!(track.title, "Track 9");
        assert_eq!(track.duration, Some(200));
        assert_eq!(track.album_id, Some(1));
        assert_eq!(track.artist_id, Some(2));
    }
}
