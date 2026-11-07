//! The preload state machine behind gapless playback.
//!
//! The audio side ( [`crate::audio`] ) can hold one fully-opened *next* track ready and swap it
//! in when the current decoder hits end-of-stream, without stopping the output stream. Deciding
//! *when* to ask for that preload, and invalidating it whenever the queue changes underneath, is
//! plain bookkeeping with no I/O, so it lives here where it can be unit-tested.
//!
//! Rules encoded here:
//!
//! * A preload is requested once, when the current track has less than
//!   [`PRELOAD_LEAD_SECONDS`] left and a next track is known. Re-requesting the same id is a
//!   no-op, so the per-second progress tick cannot spam the worker.
//! * Any structural change to the queue (skip, jump, shuffle, repeat, play-next, removal)
//!   invalidates an outstanding preload: the audio thread's preloaded track would no longer be
//!   the one the queue says comes next.
//! * When the audio thread reports that it swapped in the preloaded track, the preload is
//!   consumed and the next one can be armed for the track after it.

use serde::{Deserialize, Serialize};

/// How much of the current track must remain before its successor is preloaded. Fifteen seconds
/// is far more than opening a decoder (~tens of milliseconds) and fetching the first bytes need,
/// while leaving room to re-arm if the queue changes.
pub(crate) const PRELOAD_LEAD_SECONDS: u64 = 15;

/// What the UI knows about the outstanding preload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GaplessState {
    /// The track id the audio side has been asked to preload, if any.
    armed: Option<u64>,
}

impl GaplessState {
    pub(crate) fn new() -> Self {
        GaplessState::default()
    }

    /// Decides whether to preload now.
    ///
    /// `remaining_seconds` is how much of the current track is left ( `None` when the duration
    /// is unknown), `next` is the id the queue would play next ( `None` at the end of the
    /// queue). Returns the id to ask the worker to preload, exactly once per id.
    pub(crate) fn next_preload(
        &mut self,
        remaining_seconds: Option<u64>,
        next: Option<u64>,
    ) -> Option<u64> {
        let remaining = remaining_seconds?;
        let next = next?;
        if remaining > PRELOAD_LEAD_SECONDS || self.armed == Some(next) {
            return None;
        }
        self.armed = Some(next);
        Some(next)
    }

    /// Records that the audio thread swapped the preloaded track in. Returns `true` when the id
    /// was the armed one (i.e. a real gapless hand-off), and clears the slot either way.
    pub(crate) fn switched(&mut self, id: u64) -> bool {
        let matched = self.armed == Some(id);
        self.armed = None;
        matched
    }

    /// Invalidates any outstanding preload, e.g. because the queue changed. Returns `true` when
    /// there was one (so the worker can be told to cancel it).
    pub(crate) fn invalidate(&mut self) -> bool {
        self.armed.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preloads_only_inside_the_lead_window() {
        let mut state = GaplessState::new();
        assert_eq!(state.next_preload(Some(60), Some(2)), None);
        assert_eq!(state.next_preload(Some(16), Some(2)), None);
        assert_eq!(state.next_preload(Some(15), Some(2)), Some(2));
        assert_eq!(state.armed, Some(2));
    }

    #[test]
    fn preloads_a_given_track_only_once() {
        let mut state = GaplessState::new();
        assert_eq!(state.next_preload(Some(10), Some(7)), Some(7));
        // The per-second progress tick repeats the same decision.
        assert_eq!(state.next_preload(Some(9), Some(7)), None);
        assert_eq!(state.next_preload(Some(8), Some(7)), None);
    }

    #[test]
    fn no_preload_without_a_known_next_or_duration() {
        let mut state = GaplessState::new();
        assert_eq!(state.next_preload(None, Some(3)), None);
        assert_eq!(state.next_preload(Some(5), None), None);
        assert_eq!(state.armed, None);
    }

    #[test]
    fn a_changed_queue_re_arms_for_the_new_next_track() {
        let mut state = GaplessState::new();
        assert_eq!(state.next_preload(Some(5), Some(3)), Some(3));
        // The user hit "play next": the queue now says 9 comes next.
        assert_eq!(state.next_preload(Some(4), Some(9)), Some(9));
        assert_eq!(state.armed, Some(9));
    }

    #[test]
    fn invalidate_reports_whether_anything_was_armed() {
        let mut state = GaplessState::new();
        assert!(!state.invalidate());
        state.next_preload(Some(3), Some(1));
        assert!(state.invalidate());
        assert_eq!(state.armed, None);
        assert!(!state.invalidate());
    }

    #[test]
    fn switch_consumes_the_armed_id_only_when_it_matches() {
        let mut state = GaplessState::new();
        state.next_preload(Some(3), Some(4));
        assert!(!state.switched(5));
        assert_eq!(state.armed, None);
        // A stale id cannot fire twice.
        state.next_preload(Some(3), Some(4));
        assert!(state.switched(4));
        assert_eq!(state.armed, None);
    }
}
