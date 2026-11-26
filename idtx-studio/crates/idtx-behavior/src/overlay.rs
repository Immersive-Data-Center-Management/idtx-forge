//! Overlay / render-command queue emitted by behaviors.
//!
//! A behavior with the `overlay` capability may draw transient overlay
//! geometry (lines / arcs), highlight prims, override a prim's material, or
//! drive a named post-process parameter. Unlike [`SessionEditQueue`], overlay
//! commands **never author into USD** — they describe *one frame's* runtime
//! overlay and are consumed by the render world, not the stage. The host defers
//! each command into this per-frame queue and drains it after the update hooks,
//! feeding the overlay geometry builder + render pass and the
//! post-process chain.
//!
//! This mirrors the [`PlaybackQueue`] / [`SessionEditQueue`] / [`EventQueue`]
//! guest→host channels exactly. The concrete guest-callable import (ABI value
//! marshalling in [`crate::linker`]) and the render-side drain/build/draw are
//! layered on top of this buffer by the overlay and post-process passes; this slice is the buffer, the
//! op vocabulary, and its capability gate.
//!
//! [`PlaybackQueue`]: crate::playback::PlaybackQueue
//! [`SessionEditQueue`]: crate::session_edit::SessionEditQueue
//! [`EventQueue`]: crate::events::EventQueue

use glam::{Vec3, Vec4};
use idtx_behavior_abi::{Entity, ErrorCode};
use crate::work::{BoundedQueue, QueueBudget, RetainedSize};

/// A single transient overlay / render command.
///
/// The variants map 1:1 onto [`idtx_behavior_abi::OverlayCommandKind`]; the
/// host unpacks the wire arguments into these typed ops at the import boundary.
/// Colours are linear RGBA. Line widths are in world metres.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayOp {
    /// Frame-local native-id coverage overrides; absent entries use authored defaults.
    PointInstanceOpacities { values: Vec<idtx_behavior_abi::PointInstanceOpacity> },
    /// A world-space line segment from `start` to `end`.
    Line {
        /// Segment start (world metres).
        start: Vec3,
        /// Segment end (world metres).
        end: Vec3,
        /// Linear RGBA colour.
        color: Vec4,
        /// Line width in world metres.
        width: f32,
    },
    /// A ballistic arc from `start` along `dir` at initial `speed`, sampled to
    /// a polyline by the geometry builder (the teleport-pointer arc).
    Arc {
        /// Arc origin (world metres).
        start: Vec3,
        /// Launch direction (need not be normalized).
        dir: Vec3,
        /// Initial speed (m/s).
        speed: f32,
        /// Downward pull on world +Y (m/s²).
        gravity: f32,
        /// Linear RGBA colour.
        color: Vec4,
        /// Line width in world metres.
        width: f32,
    },
    /// Highlight the target prim with a coloured wireframe box in `color` (the
    /// low-risk fallback cue; see also [`OverlayOp::HighlightTint`] and
    /// [`OverlayOp::HighlightOutline`]).
    Highlight {
        /// Linear RGBA box colour.
        color: Vec4,
    },
    /// Clear any highlight on the target prim.
    ClearHighlight,
    /// Tint the target prim's surface with `color` — linear RGBA where the
    /// alpha channel is the tint strength (0 = untouched, 1 = fully `color`).
    HighlightTint {
        /// Linear RGBA tint colour; alpha = strength.
        color: Vec4,
    },
    /// Draw a silhouette outline around the target prim in `color` (JFA
    /// outline pass). `thickness_px` is the band width in pixels (`0.0` = the
    /// renderer default); softness is derived from it.
    HighlightOutline {
        /// Linear RGBA outline colour.
        color: Vec4,
        /// Outline band width in pixels; `0.0` means "use the renderer default".
        thickness_px: f32,
    },
    /// Override the target prim's material with the named runtime material.
    SetMaterialOverride {
        /// Name of the override material.
        material: String,
    },
    /// Clear the target prim's material override.
    ClearMaterialOverride,
    /// Set a named post-process parameter to a scalar value (chain).
    SetPostprocessParam {
        /// Parameter name.
        name: String,
        /// Scalar value.
        value: f32,
    },
}

/// A queued overlay command addressed to an entity.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayCommand {
    /// Entity the command targets. [`Entity::NULL`] means "not prim-scoped"
    /// (world-space `Line`/`Arc`, or a global post-process parameter);
    /// prim-scoped ops (`Highlight`, material override) resolve it at the drain
    /// site, falling back to the caller's own entity.
    pub target: Entity,
    /// The overlay operation to apply this frame.
    pub op: OverlayOp,
}

impl RetainedSize for OverlayCommand {
    fn retained_size(&self) -> usize {
        let bytes = match &self.op {
            OverlayOp::PointInstanceOpacities { values } => values.capacity().saturating_mul(16),
            OverlayOp::SetMaterialOverride { material } => material.capacity(),
            OverlayOp::SetPostprocessParam { name, .. } => name.capacity(),
            _ => 0,
        };
        std::mem::size_of::<Self>().saturating_add(bytes)
    }
}

/// A simple FIFO queue of pending overlay commands.
///
/// Behaviors push through the `overlay` host bucket; the render side drains it
/// once per frame after the update hooks. The queue is transient — it is fully
/// drained every frame, so nothing persists across frames unless the guest
/// re-posts it.
#[derive(Debug, Default)]
pub struct OverlayQueue {
    pending: BoundedQueue<OverlayCommand>,
}

impl OverlayQueue {
    /// Enqueue a command.
    pub fn post(&mut self, target: Entity, op: OverlayOp) -> Result<(), ErrorCode> {
        self.pending.push(OverlayCommand { target, op })
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
    pub fn drain(&mut self) -> Vec<OverlayCommand> {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_drain_fifo() {
        let mut q = OverlayQueue::default();
        assert!(q.is_empty());
        q.post(
            Entity::NULL,
            OverlayOp::Line {
                start: Vec3::ZERO,
                end: Vec3::X,
                color: Vec4::ONE,
                width: 0.01,
            },
        ).unwrap();
        q.post(
            Entity::from_bits(7),
            OverlayOp::Highlight {
                color: Vec4::new(1.0, 0.5, 0.0, 1.0),
            },
        ).unwrap();
        q.post(
            Entity::NULL,
            OverlayOp::SetPostprocessParam {
                name: "bloom".to_owned(),
                value: 0.75,
            },
        ).unwrap();
        assert_eq!(q.len(), 3);

        let drained = q.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].target, Entity::NULL);
        assert!(matches!(drained[0].op, OverlayOp::Line { .. }));
        assert_eq!(drained[1].target, Entity::from_bits(7));
        assert!(matches!(drained[1].op, OverlayOp::Highlight { .. }));
        assert!(matches!(
            &drained[2].op,
            OverlayOp::SetPostprocessParam { name, value }
                if name == "bloom" && (*value - 0.75).abs() < 1e-6
        ));
        assert!(q.is_empty());
    }

    #[test]
    fn drain_leaves_queue_empty_across_frames() {
        let mut q = OverlayQueue::default();
        q.post(Entity::NULL, OverlayOp::ClearHighlight).unwrap();
        assert_eq!(q.drain().len(), 1);
        // A transient overlay: nothing carries into the next frame's drain.
        assert!(q.drain().is_empty());
    }
}
