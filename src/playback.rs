//! The play queue: the ordered list of tracks the transport walks, plus shuffle and repeat
//! behaviour. This is pure state with no UI or audio dependencies, so next/previous, shuffle,
//! repeat and removal are all unit-testable in isolation. The UI reads it to draw the queue
//! panel and the transport, and drives it when the user presses skip or a track ends.

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

    /// The track a natural end would move to, without moving. `None` when playback would stop.
    /// Used to preload the next track for a gapless hand-off.
    pub(crate) fn peek_advance(&self) -> Option<&Track> {
        if self.repeat == Repeat::One {
            self.current()
        } else {
            self.peek_next()
        }
    }

    /// The track a manual skip would move to, without moving.
    pub(crate) fn peek_next(&self) -> Option<&Track> {
        match self.index {
            Some(current) if current + 1 < self.order.len() => self.order.get(current + 1),
            Some(_) if self.repeat == Repeat::All => self.order.first(),
            _ => None,
        }
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
}
