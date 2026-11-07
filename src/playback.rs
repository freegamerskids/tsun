//! The play queue: the ordered list of tracks the transport walks, plus shuffle and repeat
//! behaviour. This is pure state with no UI or audio dependencies, so next/previous, shuffle,
//! repeat and removal are all unit-testable in isolation. The UI reads it to draw the queue
//! panel and the transport, and drives it when the user presses skip or a track ends.
//!
//! It also builds the *upcoming order* the worker keeps: the ordered [`Upcoming`] tracks a
//! natural end will move to, so the worker can advance (and preload for gapless playback) with
//! no UI frame at all — a covered window gets no frame callbacks, and playback must not stop
//! because of it. [`upcoming_ids`] is that order as plain ids and [`UpcomingQueue`] is the
//! worker's copy of it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tidal::Track;

/// What playback does when the current track reaches its natural end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Repeat {
    /// Stop at the end of the queue.
    #[default]
    Off,
    /// Wrap back to the first track.
    All,
    /// Replay the current track.
    One,
}

impl Repeat {
    /// The next mode in the button's cycle: Off → All → One → Off.
    pub(crate) fn cycle(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

/// How long a track must have played before the previous button restarts it instead of stepping
/// back to the track before it, matching the official client.
pub(crate) const PREVIOUS_RESTART_SECONDS: u64 = 3;

/// Whether a previous press should restart the current track (played longer than
/// [`PREVIOUS_RESTART_SECONDS`]) rather than move to the previous one.
pub(crate) fn previous_restarts(elapsed_seconds: u64) -> bool {
    elapsed_seconds > PREVIOUS_RESTART_SECONDS
}

/// The ordered play queue with a current position.
///
/// `order` is what actually plays; while shuffled it is a permutation of `original`, which
/// remembers the pre-shuffle order so un-shuffling restores it. Structural edits (append,
/// play-next, remove) keep the two in step so that invariant holds for the whole run.
pub(crate) struct Queue {
    order: Vec<Track>,
    original: Vec<Track>,
    index: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
    /// Bumped by every change to the tracks, their order or the current one, so the queue
    /// panel's list is rebuilt only when it changed (see [`Queue::revision`]).
    rev: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Queue::new()
    }
}

impl Queue {
    pub(crate) fn new() -> Self {
        Queue {
            order: Vec::new(),
            original: Vec::new(),
            index: None,
            shuffle: false,
            repeat: Repeat::Off,
            rev: 0,
        }
    }

    /// The play order, current track included, as the queue panel lists it.
    pub(crate) fn tracks(&self) -> &[Track] {
        &self.order
    }

    /// A counter bumped by every change to the queue (its tracks, their order, or the current
    /// one), so a view's cached copy of [`Queue::tracks`] is rebuilt only when it changed.
    pub(crate) fn revision(&self) -> u64 {
        self.rev
    }

    fn changed(&mut self) {
        self.rev = self.rev.wrapping_add(1);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub(crate) fn index(&self) -> Option<usize> {
        self.index
    }

    pub(crate) fn current(&self) -> Option<&Track> {
        self.index.and_then(|index| self.order.get(index))
    }

    pub(crate) fn current_id(&self) -> Option<u64> {
        self.current().map(|track| track.id)
    }

    pub(crate) fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub(crate) fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub(crate) fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Replaces the queue with `tracks` and selects `start` as the current track.
    ///
    /// Under shuffle the selected track keeps its place and the tracks that follow it are
    /// shuffled, so the user still hears what they clicked first.
    pub(crate) fn set(&mut self, tracks: Vec<Track>, start: usize) {
        self.original = tracks;
        self.order = self.original.clone();
        self.index = if self.order.is_empty() {
            None
        } else {
            Some(start.min(self.order.len() - 1))
        };
        if self.shuffle {
            self.reshuffle_after_current();
        }
        self.changed();
    }

    /// Appends a track to the very end of the queue.
    pub(crate) fn push_back(&mut self, track: Track) {
        self.original.push(track.clone());
        self.order.push(track);
        self.changed();
    }

    /// Appends several tracks to the very end of the queue, in order ("Add to queue" for a
    /// whole album or playlist).
    pub(crate) fn push_back_many(&mut self, tracks: Vec<Track>) {
        for track in tracks {
            self.push_back(track);
        }
    }

    /// Inserts a track to play immediately after the current one ("Play next").
    pub(crate) fn insert_next(&mut self, track: Track) {
        match self.index {
            Some(index) => {
                self.order.insert(index + 1, track.clone());
                let current_id = self.order[index].id;
                let position = self
                    .original
                    .iter()
                    .position(|existing| existing.id == current_id)
                    .map(|position| position + 1)
                    .unwrap_or(self.original.len());
                self.original.insert(position.min(self.original.len()), track);
            }
            None => {
                self.original.push(track.clone());
                self.order.push(track);
            }
        }
        self.changed();
    }

    /// Inserts several tracks to play immediately after the current one, keeping their order
    /// ("Play next" for a whole album or playlist).
    pub(crate) fn insert_next_many(&mut self, tracks: Vec<Track>) {
        // With nothing playing there is no anchor to insert after, so the tracks simply land at
        // the end — in the order given, unlike the reverse insertion below.
        if self.index.is_none() {
            self.push_back_many(tracks);
            return;
        }
        for track in tracks.into_iter().rev() {
            self.insert_next(track);
        }
    }

    /// Removes the track at `index`, keeping the current track selected where possible.
    pub(crate) fn remove(&mut self, index: usize) -> bool {
        if index >= self.order.len() {
            return false;
        }
        let removed = self.order.remove(index);
        if let Some(position) = self
            .original
            .iter()
            .position(|track| track.id == removed.id)
        {
            self.original.remove(position);
        }
        match self.index {
            Some(current) if current > index => self.index = Some(current - 1),
            Some(_) if self.order.is_empty() => self.index = None,
            Some(current) if current == index => self.index = Some(index.min(self.order.len() - 1)),
            _ => {}
        }
        self.changed();
        true
    }

    /// Moves the queue row at `from` to position `to` (a user reorder in the queue panel),
    /// keeping the same track selected.
    ///
    /// The visible play order moves. With shuffle off the remembered pre-shuffle order moves the
    /// same way; with shuffle on the moved track is re-anchored in that order just before
    /// whatever now follows it, so turning shuffle off keeps the user's edit. Returns `false`,
    /// changing nothing, for an out-of-range index or a no-op move.
    pub(crate) fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.order.len() || to >= self.order.len() {
            return false;
        }
        let current = self.current_id();
        let track = self.order.remove(from);
        self.order.insert(to, track.clone());
        if self.shuffle {
            if let Some(position) = self.original.iter().position(|existing| existing.id == track.id) {
                self.original.remove(position);
            }
            let anchor = self.order.get(to + 1).map(|existing| existing.id);
            let insert_at = anchor
                .and_then(|id| self.original.iter().position(|existing| existing.id == id))
                .unwrap_or(self.original.len());
            self.original.insert(insert_at, track);
        } else {
            let track = self.original.remove(from);
            self.original.insert(to, track);
        }
        self.index = current.and_then(|id| self.order.iter().position(|existing| existing.id == id));
        self.changed();
        true
    }

    /// Empties the queue.
    pub(crate) fn clear(&mut self) {
        self.order.clear();
        self.original.clear();
        self.index = None;
        self.changed();
    }

    /// Makes `index` current and returns the track, for a click in the queue panel.
    pub(crate) fn jump(&mut self, index: usize) -> Option<Track> {
        let track = self.order.get(index)?.clone();
        self.index = Some(index);
        self.changed();
        Some(track)
    }

    /// Moves to the next track for a manual skip: wraps only under repeat-all.
    pub(crate) fn skip_next(&mut self) -> Option<Track> {
        let current = self.index?;
        let next = if current + 1 < self.order.len() {
            current + 1
        } else if self.repeat == Repeat::All && !self.order.is_empty() {
            0
        } else {
            return None;
        };
        self.index = Some(next);
        self.changed();
        self.order.get(next).cloned()
    }

    /// Moves to the previous track for a manual skip: wraps to the end only under repeat-all.
    pub(crate) fn skip_previous(&mut self) -> Option<Track> {
        let current = self.index?;
        let previous = if current > 0 {
            current - 1
        } else if self.repeat == Repeat::All && !self.order.is_empty() {
            self.order.len() - 1
        } else {
            return None;
        };
        self.index = Some(previous);
        self.changed();
        self.order.get(previous).cloned()
    }

    /// Advances for a natural end: repeat-one replays the current track, otherwise it is a
    /// manual skip (so repeat-all wraps and repeat-off stops).
    pub(crate) fn advance(&mut self) -> Option<Track> {
        if self.repeat == Repeat::One {
            let current = self.index?;
            return self.order.get(current).cloned();
        }
        self.skip_next()
    }

    pub(crate) fn has_next(&self) -> bool {
        match self.index {
            Some(current) => {
                current + 1 < self.order.len()
                    || (self.repeat == Repeat::All && !self.order.is_empty())
            }
            None => false,
        }
    }

    /// Turns shuffle on (reshuffling what follows the current track) or off (restoring the
    /// original order and re-selecting the current track in it).
    pub(crate) fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        let current = self.current_id();
        self.shuffle = on;
        if on {
            self.reshuffle_after_current();
        } else {
            self.order = self.original.clone();
            self.index = current.and_then(|id| self.order.iter().position(|track| track.id == id));
        }
        self.changed();
    }

    fn reshuffle_after_current(&mut self) {
        self.reshuffle_after_current_with(random_seed());
    }

    fn reshuffle_after_current_with(&mut self, seed: u64) {
        let Some(index) = self.index else { return };
        let tail = &mut self.order[index + 1..];
        if tail.len() < 2 {
            return;
        }
        shuffle_slice(tail, seed);
    }

}

// ---------------------------------------------------------------------------------------------
// The upcoming order the worker advances through by itself

/// The shortest upcoming list the UI hands the worker. Enough to cover a queue of that length
/// without a refill (a covered window gets no frames at all, so nothing refills it), and used as
/// the minimum for the repeat modes, whose lists the worker repeats itself.
pub(crate) const UPCOMING_LIMIT: usize = 25;

/// Where the worker gets one upcoming track's audio from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UpcomingSource {
    /// A TIDAL track: resolved and started exactly as `Cmd::Play` does.
    Tidal,
    /// A local file, opened directly exactly as `Cmd::PlayLocal` does.
    Local {
        path: PathBuf,
        /// The quality label the player bar shows for the file.
        quality: String,
        /// Its ReplayGain multiplier, applied when normalization is on.
        replaygain: Option<f32>,
    },
}

/// One track the worker can start by itself when the current one ends, so a natural end does
/// not need the UI thread to run a frame. The [`Track`] carries everything the UI needs to draw
/// the track once it catches up; [`UpcomingSource`] carries what the audio side needs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Upcoming {
    pub(crate) track: Track,
    pub(crate) source: UpcomingSource,
}

impl Upcoming {
    pub(crate) fn id(&self) -> u64 {
        self.track.id
    }

    /// Whether this track is a local file (the gapless hand-off only works within one side).
    pub(crate) fn is_local(&self) -> bool {
        matches!(self.source, UpcomingSource::Local { .. })
    }
}

/// The ids a natural end would move to, in order.
///
/// The worker must be able to keep playing on its own for as long as the window is not being
/// drawn, so the list is not a short window but the whole future the queue knows:
///
/// * repeat-off: every track after the current one, so nothing is left out,
/// * repeat-one: the current track `limit` times (the worker repeats the list),
/// * repeat-all: whole cycles of the queue, at least `limit` entries — a list that stopped
///   mid-cycle would restart rotated and play a track twice at the seam.
///
/// Empty when playback would stop, which is how the worker knows it has nothing to advance to.
pub(crate) fn upcoming_ids(queue: &Queue, limit: usize) -> Vec<u64> {
    let Some(index) = queue.index else { return Vec::new() };
    if queue.order.is_empty() || index >= queue.order.len() {
        return Vec::new();
    }
    match queue.repeat {
        Repeat::One => vec![queue.order[index].id; limit.max(1)],
        Repeat::Off => queue.order[index + 1..].iter().map(|track| track.id).collect(),
        Repeat::All => {
            let period = queue.order.len();
            let count = period * limit.max(1).div_ceil(period);
            let mut ids = Vec::with_capacity(count);
            let mut position = index + 1;
            while ids.len() < count {
                ids.push(queue.order[position % period].id);
                position += 1;
            }
            ids
        }
    }
}

/// The worker's copy of the upcoming order: what it plays next on its own, and with which
/// [`UpcomingSource`]. The list is kept whole with a cursor into it, so [`UpcomingQueue::restart`]
/// can repeat it for ever under repeat-all/one without copying it again.
///
/// The UI sends a fresh list whenever the order can change. The list is derived from the queue,
/// so it must be reconciled rather than replaced blindly: while a stalled UI is catching up it
/// resends a tail that already contains tracks the worker has played, and replaying those would
/// repeat tracks.
#[derive(Default)]
pub(crate) struct UpcomingQueue {
    list: Vec<Upcoming>,
    next: usize,
}

impl UpcomingQueue {
    /// Takes a list the UI built after `after` (the track the UI believes is playing) while the
    /// worker is on `current`.
    ///
    /// When they agree the list is the future and replaces the old one. When they do not — the
    /// UI has not applied every auto-advance yet — everything up to and including what the
    /// worker is already playing is dropped, so the tracks it has already started are not
    /// played a second time. A list that does not contain `current` at all is taken as it is:
    /// the UI knows better than a list the worker has already walked past.
    pub(crate) fn set(&mut self, after: Option<u64>, incoming: Vec<Upcoming>, current: Option<u64>) {
        self.list = if after.is_some() && after == current {
            incoming
        } else if let Some(current) = current {
            match incoming.iter().position(|entry| entry.id() == current) {
                Some(position) => incoming[position + 1..].to_vec(),
                None => incoming,
            }
        } else {
            incoming
        };
        self.next = 0;
    }

    pub(crate) fn head(&self) -> Option<&Upcoming> {
        self.list.get(self.next)
    }

    pub(crate) fn pop(&mut self) -> Option<Upcoming> {
        let entry = self.list.get(self.next)?.clone();
        self.next += 1;
        Some(entry)
    }

    /// Drops the head when it is `id`, for a gapless hand-off that already started it.
    pub(crate) fn consume(&mut self, id: u64) {
        if self.head().is_some_and(|entry| entry.id() == id) {
            self.next += 1;
        }
    }

    /// Starts the list over, so a repeat mode can keep playing without the UI refilling it.
    pub(crate) fn restart(&mut self) {
        self.next = 0;
    }

    pub(crate) fn clear(&mut self) {
        self.list.clear();
        self.next = 0;
    }
}

/// A seed for the shuffle, from the OS when available and from a fixed constant otherwise (a
/// deterministic order beats a panic).
fn random_seed() -> u64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return 0x9E37_79B9_7F4A_7C15;
    }
    u64::from_le_bytes(bytes)
}

/// In-place Fisher–Yates driven by a small xorshift64* generator, so shuffling needs no extra
/// crate and the same seed always yields the same order (which the tests rely on).
fn shuffle_slice<T>(slice: &mut [T], seed: u64) {
    let mut state = seed | 1;
    for i in (1..slice.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        slice.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64) -> Track {
        Track {
            id,
            title: format!("Track {id}"),
            artist: "Artist".to_owned(),
            ..Default::default()
        }
    }

    fn ids(queue: &Queue) -> Vec<u64> {
        queue.tracks().iter().map(|track| track.id).collect()
    }

    fn build(ids_in: &[u64], start: usize) -> Queue {
        let mut queue = Queue::new();
        queue.set(ids_in.iter().copied().map(track).collect(), start);
        queue
    }

    #[test]
    fn set_selects_start_and_reports_current() {
        let queue = build(&[1, 2, 3], 1);
        assert_eq!(queue.index(), Some(1));
        assert_eq!(queue.current_id(), Some(2));
        assert!(queue.has_next());
    }

    #[test]
    fn next_and_previous_walk_the_queue_without_wrapping() {
        let mut queue = build(&[1, 2, 3], 0);
        assert_eq!(queue.skip_previous(), None, "nothing before the first track");
        assert_eq!(queue.skip_next().map(|t| t.id), Some(2));
        assert_eq!(queue.skip_next().map(|t| t.id), Some(3));
        assert_eq!(queue.skip_next(), None, "nothing after the last track");
        assert_eq!(queue.skip_previous().map(|t| t.id), Some(2));
    }

    #[test]
    fn advance_stops_at_the_end_with_repeat_off() {
        let mut queue = build(&[1, 2], 1);
        assert_eq!(queue.advance().map(|t| t.id), None);
    }

    #[test]
    fn push_back_many_appends_in_order() {
        let mut queue = build(&[1], 0);
        queue.push_back_many(vec![track(2), track(3)]);
        assert_eq!(ids(&queue), [1, 2, 3]);
        assert_eq!(queue.current_id(), Some(1), "the current track does not move");
    }

    #[test]
    fn insert_next_many_keeps_the_given_order_right_after_the_current_track() {
        let mut queue = build(&[1, 2, 3], 1);
        queue.insert_next_many(vec![track(8), track(9)]);
        assert_eq!(ids(&queue), [1, 2, 8, 9, 3]);
        assert_eq!(queue.current_id(), Some(2));
        assert_eq!(queue.skip_next().map(|t| t.id), Some(8));
        assert_eq!(queue.skip_next().map(|t| t.id), Some(9));

        // With nothing playing yet the tracks simply land at the end, in order.
        let mut empty = Queue::new();
        empty.insert_next_many(vec![track(4), track(5)]);
        assert_eq!(ids(&empty), [4, 5]);
        assert_eq!(empty.index(), None, "inserting into an unstarted queue plays nothing");
    }

    #[test]
    fn repeat_all_wraps_on_advance_and_skip() {
        let mut queue = build(&[1, 2, 3], 2);
        queue.set_repeat(Repeat::All);
        assert_eq!(queue.advance().map(|t| t.id), Some(1));
        assert_eq!(queue.skip_previous().map(|t| t.id), Some(3));
        assert!(queue.has_next());
    }

    #[test]
    fn repeat_one_repeats_on_advance_but_skips_manually() {
        let mut queue = build(&[1, 2, 3], 1);
        queue.set_repeat(Repeat::One);
        assert_eq!(queue.advance().map(|t| t.id), Some(2), "natural end replays");
        assert_eq!(queue.skip_next().map(|t| t.id), Some(3), "manual skip moves on");
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        assert_eq!(Repeat::Off.cycle(), Repeat::All);
        assert_eq!(Repeat::All.cycle(), Repeat::One);
        assert_eq!(Repeat::One.cycle(), Repeat::Off);
    }

    #[test]
    fn previous_restarts_after_the_three_second_window() {
        assert!(!previous_restarts(0));
        assert!(!previous_restarts(3));
        assert!(previous_restarts(4));
    }

    #[test]
    fn shuffle_keeps_the_current_track_and_permutes_the_rest() {
        let mut queue = build(&[1, 2, 3, 4, 5, 6], 2);
        queue.set_shuffle(true);
        assert!(queue.shuffle());
        assert_eq!(queue.current_id(), Some(3), "the played track stays current");
        let mut after: Vec<u64> = ids(&queue)[3..].to_vec();
        let mut expected = vec![4, 5, 6];
        after.sort_unstable();
        expected.sort_unstable();
        assert_eq!(after, expected, "the tail is a permutation, nothing lost");
        assert_eq!(ids(&queue)[..3], [1, 2, 3], "everything up to the current track is kept");
    }

    #[test]
    fn unshuffle_restores_the_original_order_and_follows_the_current_track() {
        let mut queue = build(&[1, 2, 3, 4, 5], 1);
        queue.set_shuffle(true);
        queue.skip_next();
        let playing = queue.current_id().unwrap();
        queue.set_shuffle(false);
        assert_eq!(ids(&queue), vec![1, 2, 3, 4, 5]);
        assert_eq!(queue.current_id(), Some(playing), "the same track stays selected");
    }

    #[test]
    fn remove_adjusts_the_current_index() {
        let mut queue = build(&[1, 2, 3, 4], 2);
        assert!(queue.remove(0), "removing before the current track");
        assert_eq!(ids(&queue), vec![2, 3, 4]);
        assert_eq!(queue.current_id(), Some(3), "current track unchanged");

        let mut queue = build(&[1, 2, 3, 4], 1);
        assert!(queue.remove(2), "removing after the current track");
        assert_eq!(ids(&queue), vec![1, 2, 4]);
        assert_eq!(queue.current_id(), Some(2));
        assert!(!queue.remove(9), "out of range is a no-op");
    }

    #[test]
    fn removing_the_current_track_selects_the_next_one() {
        let mut queue = build(&[1, 2, 3], 1);
        queue.remove(1);
        assert_eq!(ids(&queue), vec![1, 3]);
        assert_eq!(queue.index(), Some(1), "the following track takes its place");
        assert_eq!(queue.current_id(), Some(3));
    }

    #[test]
    fn play_next_inserts_right_after_the_current_track() {
        let mut queue = build(&[1, 2, 3], 1);
        queue.insert_next(track(9));
        assert_eq!(ids(&queue), vec![1, 2, 9, 3]);
        assert_eq!(queue.skip_next().map(|t| t.id), Some(9));

        // Un-shuffling must still show the inserted track in a sensible place.
        queue.set_shuffle(true);
        queue.set_shuffle(false);
        assert!(ids(&queue).contains(&9));
    }

    #[test]
    fn add_to_queue_appends_at_the_end() {
        let mut queue = build(&[1, 2], 0);
        queue.push_back(track(8));
        assert_eq!(ids(&queue), vec![1, 2, 8]);
        assert_eq!(queue.index(), Some(0), "appending does not move the current track");
    }

    #[test]
    fn jump_selects_a_row() {
        let mut queue = build(&[1, 2, 3], 0);
        assert_eq!(queue.jump(2).map(|t| t.id), Some(3));
        assert_eq!(queue.current_id(), Some(3));
    }

    #[test]
    fn clear_empties_the_queue() {
        let mut queue = build(&[1, 2, 3], 1);
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.current_id(), None);
        assert!(!queue.has_next());
    }

    #[test]
    fn empty_queue_reports_nothing() {
        let mut queue = Queue::new();
        assert_eq!(queue.index(), None);
        assert_eq!(queue.skip_next(), None);
        assert_eq!(queue.advance(), None);
        assert!(!queue.has_next());
    }

    #[test]
    fn move_item_reorders_and_follows_the_current_track() {
        let mut queue = build(&[1, 2, 3, 4], 1);
        assert!(queue.move_item(3, 0), "move the last track to the front");
        assert_eq!(ids(&queue), vec![4, 1, 2, 3]);
        assert_eq!(queue.current_id(), Some(2), "the playing track stays selected");
        assert_eq!(queue.index(), Some(2));
        assert!(queue.move_item(0, 2));
        assert_eq!(ids(&queue), vec![1, 2, 4, 3]);
        assert_eq!(queue.current_id(), Some(2));
    }

    #[test]
    fn move_item_rejects_out_of_range_and_no_op_moves() {
        let mut queue = build(&[1, 2, 3], 0);
        assert!(!queue.move_item(1, 1));
        assert!(!queue.move_item(3, 0));
        assert!(!queue.move_item(0, 3));
        assert!(!Queue::new().move_item(0, 1), "an empty queue has nothing to move");
        assert_eq!(ids(&queue), vec![1, 2, 3]);
    }

    #[test]
    fn move_item_then_unshuffle_keeps_the_edit_and_the_head() {
        let mut queue = build(&[1, 2, 3, 4, 5], 0);
        queue.set_shuffle(true);
        // Whatever the shuffled tail is, move its first entry to the end.
        queue.move_item(1, 4);
        let playing = queue.current_id().unwrap();
        queue.set_shuffle(false);
        assert_eq!(ids(&queue).len(), 5);
        assert_eq!(queue.current_id(), Some(playing), "the playing track survives the edit");
        assert_eq!(ids(&queue)[0], 1, "the pre-shuffle head is intact");
        assert_eq!(queue.tracks().len(), queue.original.len(), "no track was lost");
    }

    #[test]
    fn seeded_shuffle_is_deterministic_and_keeps_the_head() {
        let mut queue = build(&[1, 2, 3, 4, 5, 6, 7, 8], 0);
        queue.reshuffle_after_current_with(42);
        let first = ids(&queue);
        let mut again = build(&[1, 2, 3, 4, 5, 6, 7, 8], 0);
        again.reshuffle_after_current_with(42);
        assert_eq!(first, ids(&again));
        assert_eq!(first[0], 1);
        let mut tail = first[1..].to_vec();
        tail.sort_unstable();
        assert_eq!(tail, vec![2, 3, 4, 5, 6, 7, 8]);
    }

    // ---- the upcoming order the worker advances through on its own ------------------------

    fn upcoming(ids_in: &[u64], start: usize, limit: usize) -> Vec<u64> {
        upcoming_ids(&build(ids_in, start), limit)
    }

    #[test]
    fn upcoming_off_is_the_tail_and_ends_with_the_queue() {
        assert_eq!(upcoming(&[1, 2, 3], 0, 25), [2, 3]);
        assert_eq!(upcoming(&[1, 2, 3], 1, 25), [3]);
        assert_eq!(upcoming(&[1, 2, 3], 2, 25), Vec::<u64>::new(), "nothing after the last track");
        assert_eq!(upcoming(&[1], 0, 25), Vec::<u64>::new());
    }

    #[test]
    fn upcoming_off_is_never_truncated() {
        // A frozen UI cannot refill the list, so the whole rest of the queue goes with it: a
        // window would stop playback at the end of the window instead of the end of the queue.
        let ids_in: Vec<u64> = (1..=30).collect();
        let expected: Vec<u64> = (2..=30).collect();
        assert_eq!(upcoming(&ids_in, 0, 2), expected);
    }

    #[test]
    fn upcoming_all_wraps_in_whole_cycles() {
        // Three cycles of a three-track queue: a whole number of passes, so the worker can
        // repeat the list for ever without the seam playing a track twice.
        let mut queue = build(&[1, 2, 3], 2);
        queue.set_repeat(Repeat::All);
        assert_eq!(
            upcoming_ids(&queue, 9),
            [1, 2, 3, 1, 2, 3, 1, 2, 3]
        );
        assert_eq!(upcoming_ids(&queue, 1), [1, 2, 3], "at least one whole cycle");
        assert_eq!(upcoming_ids(&queue, 4), [1, 2, 3, 1, 2, 3], "two cycles cover four");

        let mut single = build(&[7], 0);
        single.set_repeat(Repeat::All);
        assert_eq!(upcoming_ids(&single, 3), [7, 7, 7], "one track repeats for ever");
    }

    #[test]
    fn upcoming_one_repeats_the_current_id() {
        let mut queue = build(&[1, 2, 3], 1);
        queue.set_repeat(Repeat::One);
        assert_eq!(upcoming_ids(&queue, 3), [2, 2, 2]);
        assert_eq!(upcoming_ids(&queue, 0), [2], "at least one entry, so the list can repeat");
    }

    #[test]
    fn upcoming_is_empty_with_nothing_selected() {
        let mut queue = build(&[1, 2, 3], 0);
        queue.clear();
        assert_eq!(upcoming_ids(&queue, 25), Vec::<u64>::new());

        let mut unstarted = Queue::new();
        unstarted.push_back_many(vec![track(1), track(2)]);
        assert_eq!(upcoming_ids(&unstarted, 25), Vec::<u64>::new(), "nothing is playing yet");
    }

    #[test]
    fn upcoming_follows_the_shuffled_order() {
        let mut queue = build(&[1, 2, 3, 4, 5, 6, 7, 8], 0);
        queue.reshuffle_after_current_with(42);
        // Whatever the shuffle produced, the upcoming order is the queue's own tail.
        assert_eq!(upcoming_ids(&queue, 25), ids(&queue)[1..].to_vec());
    }

    #[test]
    fn upcoming_tracks_queue_edits() {
        // "Play next" lands right after the current track.
        let mut queue = build(&[1, 2, 3], 1);
        queue.insert_next(track(9));
        assert_eq!(upcoming_ids(&queue, 25), [9, 3]);

        // "Add to queue" lands at the end.
        queue.push_back(track(4));
        assert_eq!(upcoming_ids(&queue, 25), [9, 3, 4]);

        // A queue reorder moves what comes next.
        assert!(queue.move_item(4, 2), "the appended track moves to come next");
        assert_eq!(ids(&queue), [1, 2, 4, 9, 3]);
        assert_eq!(upcoming_ids(&queue, 25), [4, 9, 3]);

        // Turning shuffle off restores the remembered order.
        queue.set_shuffle(true);
        queue.set_shuffle(false);
        assert_eq!(upcoming_ids(&queue, 25), [4, 9, 3]);
    }

    fn entry(id: u64) -> Upcoming {
        Upcoming { track: track(id), source: UpcomingSource::Tidal }
    }

    fn upcoming_list(queue: &UpcomingQueue) -> Vec<u64> {
        queue.list[queue.next..].iter().map(Upcoming::id).collect()
    }

    #[test]
    fn the_worker_takes_a_fresh_list_when_the_ui_is_where_it_is() {
        let mut list = UpcomingQueue::default();
        list.set(Some(1), vec![entry(2), entry(3)], Some(1));
        assert_eq!(upcoming_list(&list), [2, 3]);
        assert_eq!(list.head().map(Upcoming::id), Some(2));
        assert_eq!(list.pop().map(|entry| entry.id()), Some(2));
        assert_eq!(upcoming_list(&list), [3]);
        assert_eq!(list.head().map(Upcoming::id), Some(3), "one entry is left");
    }

    #[test]
    fn a_lagging_ui_does_not_replay_what_the_worker_has_played() {
        // The UI is on track 1 while the worker has already advanced to 3: the resend (whose
        // list starts at 2, as the UI built it) must not put 2 and 3 back in the queue.
        let mut list = UpcomingQueue::default();
        list.set(Some(1), vec![entry(2), entry(3), entry(4)], Some(3));
        assert_eq!(upcoming_list(&list), [4]);
    }

    #[test]
    fn a_list_the_worker_is_not_in_is_taken_as_it_is() {
        // The worker's current track is not in the UI's window (it was edited out, or the
        // window wrapped): the UI's list wins.
        let mut list = UpcomingQueue::default();
        list.set(Some(1), vec![entry(5), entry(6)], Some(9));
        assert_eq!(upcoming_list(&list), [5, 6]);

        // …and with nothing playing at all the whole list is kept.
        list.set(None, vec![entry(7), entry(8)], None);
        assert_eq!(upcoming_list(&list), [7, 8]);
        list.clear();
        assert!(list.head().is_none());
        assert_eq!(list.pop().map(|entry| entry.id()), None);
    }

    #[test]
    fn a_gapless_hand_off_consumes_its_head() {
        let mut list = UpcomingQueue::default();
        list.set(Some(1), vec![entry(2), entry(3)], Some(1));
        list.consume(2);
        assert_eq!(upcoming_list(&list), [3]);
        // A stale hand-off for something else leaves the list alone.
        list.consume(9);
        assert_eq!(upcoming_list(&list), [3]);
    }

    #[test]
    fn the_worker_can_repeat_the_very_same_list() {
        // What repeat-all and repeat-one need: a drained list starts over, unchanged.
        let mut list = UpcomingQueue::default();
        list.set(Some(1), vec![entry(2), entry(3), entry(2)], Some(1));
        for _ in 0..2 {
            assert_eq!(list.pop().map(|entry| entry.id()), Some(2));
            assert_eq!(list.pop().map(|entry| entry.id()), Some(3));
            assert_eq!(list.pop().map(|entry| entry.id()), Some(2));
            assert_eq!(list.pop().map(|entry| entry.id()), None, "drained");
            list.restart();
        }
        assert_eq!(upcoming_list(&list), [2, 3, 2]);
        // Clearing leaves nothing to repeat, so a repeat mode cannot spin on an empty list.
        list.clear();
        list.restart();
        assert_eq!(list.pop().map(|entry| entry.id()), None);
    }
}
