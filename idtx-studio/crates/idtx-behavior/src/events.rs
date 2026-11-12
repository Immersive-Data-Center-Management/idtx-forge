//! Named-event queue delivered to behaviors (dispatch loop step 6).
//!
//! A behavior with the `events` capability may `post` a named event
//! carrying a small byte payload; the host drains the queue at the end of
//! the dispatch loop and delivers each event to the targeted entity's
//! `on_event` hook.
//!
//! The payload is kept opaque (a `Vec<u8>`) and delivered to a single
//! target entity (or broadcast when the target is
//! [`idtx_behavior_abi::Entity::NULL`]).

use idtx_behavior_abi::{Entity, ErrorCode};
use crate::work::{BoundedQueue, QueueBudget, Queued, RetainedSize};

/// A queued named event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Target entity, or [`Entity::NULL`] for a broadcast.
    pub target: Entity,
    /// Event name (`"damage"`, `"pickup"`).
    pub name: String,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
}

impl RetainedSize for Event {
    fn retained_size(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.name.capacity()).saturating_add(self.payload.capacity())
    }
}

/// A simple FIFO queue of pending events.
///
/// Behaviors push through the `events` host bucket; the dispatch loop
/// drains it once per frame after `on_update` / `on_late_update`.
#[derive(Debug, Default)]
pub struct EventQueue {
    pending: BoundedQueue<Event>,
}

impl EventQueue {
    /// Enqueue an event, or return `InvalidArgument` on size/capacity overflow.
    pub fn post(&mut self, target: Entity, name: impl Into<String>, payload: Vec<u8>) -> Result<(), ErrorCode> {
        self.pending.push(Event {
            target,
            name: name.into(),
            payload,
        })
    }

    pub(crate) fn with_budget(budget: QueueBudget, max_payload: usize) -> Self {
        Self { pending: BoundedQueue::new(budget, max_payload) }
    }

    pub(crate) fn take_all(&mut self) -> Vec<Queued<Event>> { self.pending.take_all() }
    pub(crate) fn clear(&mut self) { self.pending.clear(); }

    /// `true` when no events are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of pending events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Drain every pending event, leaving the queue empty.
    #[must_use]
    pub fn drain(&mut self) -> Vec<Event> {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_drain_fifo() {
        let mut q = EventQueue::default();
        assert!(q.is_empty());
        q.post(Entity::NULL, "a", vec![1]).unwrap();
        q.post(Entity::from_bits(5), "b", vec![2, 3]).unwrap();
        assert_eq!(q.len(), 2);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].name, "a");
        assert_eq!(drained[1].name, "b");
        assert_eq!(drained[1].payload, vec![2, 3]);
        assert!(q.is_empty());
    }
}
