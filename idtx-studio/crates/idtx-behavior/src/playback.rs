//! Playback-command queue emitted by behaviors (dispatch loop drain).
//!
//! A behavior with the `playback` capability may issue transport commands
//! (`play`/`pause`/`stop`/`seek`/`set_speed`/`set_looping`) against an
//! `AnimClipPlayer` playhead. The host defers each command into this per-frame
//! queue and drains it after the hooks run, applying the commands to the
//! addressed entity's clip playhead — exactly mirroring the [`EventQueue`]
//! guest→host channel.
//!
//! [`EventQueue`]: crate::events::EventQueue

use idtx_behavior_abi::{Entity, ErrorCode, PlaybackCommandKind};
use crate::work::{BoundedQueue, QueueBudget, RetainedSize};

/// A queued playback-transport command.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackCommand {
    /// Entity whose [`AnimClipPlayer`] is addressed. [`Entity::NULL`] targets
    /// the caller's own entity (resolved by the drain site).
    pub target: Entity,
    /// The clip label naming the playhead within that player.
    pub clip: String,
    /// Which transport action to apply.
    pub kind: PlaybackCommandKind,
    /// Command argument (seek time code, speed multiplier, or looping flag);
    /// interpreted per [`PlaybackCommandKind`], ignored where not applicable.
    pub value: f32,
}

impl RetainedSize for PlaybackCommand {
    fn retained_size(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.clip.capacity())
    }
}

/// A simple FIFO queue of pending playback commands.
///
/// Behaviors push through the `playback` host bucket; the dispatch loop drains
/// it once per frame after the update hooks.
#[derive(Debug, Default)]
pub struct PlaybackQueue {
    pending: BoundedQueue<PlaybackCommand>,
}

impl PlaybackQueue {
    /// Enqueue a command.
    pub fn post(
        &mut self,
        target: Entity,
        clip: impl Into<String>,
        kind: PlaybackCommandKind,
        value: f32,
    ) -> Result<(), ErrorCode> {
        self.pending.push(PlaybackCommand {
            target,
            clip: clip.into(),
            kind,
            value,
        })
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
    pub fn drain(&mut self) -> Vec<PlaybackCommand> {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_drain_fifo() {
        let mut q = PlaybackQueue::default();
        assert!(q.is_empty());
        q.post(Entity::NULL, "walk", PlaybackCommandKind::Play, 0.0).unwrap();
        q.post(Entity::from_bits(5), "run", PlaybackCommandKind::Seek, 12.0).unwrap();
        assert_eq!(q.len(), 2);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].clip, "walk");
        assert_eq!(drained[0].kind, PlaybackCommandKind::Play);
        assert_eq!(drained[1].clip, "run");
        assert_eq!(drained[1].kind, PlaybackCommandKind::Seek);
        assert!((drained[1].value - 12.0).abs() < 1e-6);
        assert!(q.is_empty());
    }
}
