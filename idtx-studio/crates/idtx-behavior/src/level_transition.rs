//! Deferred guest→host level-transition request queue.
//!
//! A behavior granted the `level_control` capability may ask the runtime to
//! travel to another level. Guests may **not** swap stages on the hot path (a
//! transition opens a fresh session on a worker thread and is the runtime
//! driver's job): instead each request is **deferred** into this per-frame
//! queue, drained by the session after the update hooks, and surfaced to the
//! runtime driver which performs the actual swap. Nothing in this slice
//! performs a transition — it only buffers + surfaces the request.
//!
//! This mirrors the [`SessionEditQueue`] / [`PlaybackQueue`] guest→host channels
//! exactly: the host knows nothing of levels or sessions, it only buffers the
//! `(target, spawn)` the guest posted.
//!
//! [`SessionEditQueue`]: crate::session_edit::SessionEditQueue
//! [`PlaybackQueue`]: crate::playback::PlaybackQueue

use idtx_behavior_abi::ErrorCode;
use crate::work::{BoundedQueue, QueueBudget, RetainedSize};

/// A single deferred level-transition request posted by a `level_control`
/// behavior.
///
/// `target` is a level **id** (e.g. `"cave"`) that the runtime resolves to an
/// assembly path via its level table; `spawn` is an optional named spawn
/// point in the target level (`None` = the target's default spawn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelTransitionRequest {
    /// Target level id the behavior asked to travel to.
    pub target: String,
    /// Optional named spawn point in the target level (`None` = default).
    pub spawn: Option<String>,
}

impl RetainedSize for LevelTransitionRequest {
    fn retained_size(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.target.capacity())
            .saturating_add(self.spawn.as_ref().map_or(0, String::capacity))
    }
}

/// A simple FIFO queue of pending level-transition requests.
///
/// Behaviors push through the `level_control` host bucket; the session drains
/// it once per frame after the update hooks and surfaces the most recent
/// request for the runtime driver to poll. The host only buffers — it has
/// no knowledge of levels or sessions.
#[derive(Debug, Default)]
pub struct LevelTransitionQueue {
    pending: BoundedQueue<LevelTransitionRequest>,
}

impl LevelTransitionQueue {
    /// Enqueue a level-transition request.
    pub fn post(&mut self, target: String, spawn: Option<String>) -> Result<(), ErrorCode> {
        self.pending.push(LevelTransitionRequest { target, spawn })
    }

    pub(crate) fn with_budget(budget: QueueBudget, max_payload: usize) -> Self {
        Self { pending: BoundedQueue::new(budget, max_payload) }
    }

    pub(crate) fn clear(&mut self) { self.pending.clear(); }

    /// `true` when no requests are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of pending requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Drain every pending request, leaving the queue empty.
    #[must_use]
    pub fn drain(&mut self) -> Vec<LevelTransitionRequest> {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_drain_fifo() {
        let mut q = LevelTransitionQueue::default();
        assert!(q.is_empty());
        q.post("cave".to_owned(), None).unwrap();
        q.post("summit".to_owned(), Some("north".to_owned())).unwrap();
        assert_eq!(q.len(), 2);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].target, "cave");
        assert_eq!(drained[0].spawn, None);
        assert_eq!(drained[1].target, "summit");
        assert_eq!(drained[1].spawn.as_deref(), Some("north"));
        assert!(q.is_empty());

        // Draining again yields nothing — the queue was emptied.
        assert!(q.drain().is_empty());
    }
}
