//! Physics-control command queue emitted by behaviors (dispatch loop drain).
//!
//! A behavior with the `physics` capability may suspend / resume runtime
//! simulation of a specific body — the grab primitive: a dynamic rigid body is
//! flipped to kinematic while held so the solver stops integrating gravity and
//! no longer authors its pose (letting the behavior's `set_transform` ride it
//! with the pointer), then flipped back to dynamic on release.
//!
//! The host has **no knowledge of physics** (it does not depend on
//! `idtx-physics` / `idtx-scene`): it only buffers `(entity, simulated)` here
//! and the session — which links both the host and the physics subsystem —
//! drains it after the update hooks and applies each command to its live
//! `PhysicsSubsystem`. This mirrors the [`PlaybackQueue`] / [`SessionEditQueue`]
//! guest→host channels exactly.
//!
//! [`PlaybackQueue`]: crate::playback::PlaybackQueue
//! [`SessionEditQueue`]: crate::session_edit::SessionEditQueue

use idtx_behavior_abi::{Entity, ErrorCode};
use crate::work::{BoundedQueue, QueueBudget, RetainedSize};

/// A queued physics-control command: set whether the target body is simulated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicsControlCommand {
    /// Entity whose physics body is addressed. [`Entity::NULL`] targets the
    /// caller's own entity (resolved by the drain site).
    pub target: Entity,
    /// `true` resumes dynamic simulation (grab release); `false` suspends it
    /// (grab — the body becomes kinematic while held).
    pub simulated: bool,
}

impl RetainedSize for PhysicsControlCommand {
    fn retained_size(&self) -> usize { std::mem::size_of::<Self>() }
}

/// A simple FIFO queue of pending physics-control commands.
///
/// Behaviors push through the `physics` host bucket; the session drains it once
/// per frame after the update hooks and applies each command to its physics
/// subsystem (before the physics tick, so a suspend takes effect the same frame).
#[derive(Debug, Default)]
pub struct PhysicsControlQueue {
    pending: BoundedQueue<PhysicsControlCommand>,
}

impl PhysicsControlQueue {
    /// Enqueue a command.
    pub fn post(&mut self, target: Entity, simulated: bool) -> Result<(), ErrorCode> {
        self.pending.push(PhysicsControlCommand { target, simulated })
    }

    pub(crate) fn with_budget(budget: QueueBudget, max_payload: usize) -> Self {
        Self { pending: BoundedQueue::new(budget, max_payload) }
    }

    pub(crate) fn clear(&mut self) { self.pending.clear(); }

    /// `true` when no commands are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of pending commands.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Drain every pending command, leaving the queue empty.
    #[must_use]
    pub fn drain(&mut self) -> Vec<PhysicsControlCommand> {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_drain_fifo() {
        let mut q = PhysicsControlQueue::default();
        assert!(q.is_empty());
        q.post(Entity::NULL, false).unwrap();
        q.post(Entity::from_bits(7), true).unwrap();
        assert_eq!(q.len(), 2);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].target, Entity::NULL);
        assert!(!drained[0].simulated);
        assert_eq!(drained[1].target, Entity::from_bits(7));
        assert!(drained[1].simulated);
        assert!(q.is_empty());
    }
}
