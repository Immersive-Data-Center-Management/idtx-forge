//! Deferred session-layer authoring queue emitted by behaviors.
//!
//! A behavior with the `stage_edit` capability may author edits against the
//! **session layer** — the non-destructive overlay that runtime toggles write
//! into, never dirtying the saved stage. Guests may *not* mutate USD on the hot
//! path (USD is the single source of truth, and a write must be followed by a
//! re-projection so the ECS/render worlds reflect it): instead each edit is
//! **deferred** into this per-frame queue and drained by the session after the
//! update hooks, applied to the session layer, and re-projected the same frame.
//!
//! This mirrors the [`PlaybackQueue`] / [`EventQueue`] guest→host channels
//! exactly. The concrete guest-callable imports (ABI value marshalling +
//! capability-gated write imports in [`crate::linker`]) and the session-side
//! drain/apply/re-projection are layered on top of this buffer later; this
//! slice is the buffer + its capability gate.
//!
//! [`PlaybackQueue`]: crate::playback::PlaybackQueue
//! [`EventQueue`]: crate::events::EventQueue

use idtx_behavior_abi::{Entity, ErrorCode, Xform};
use crate::work::{BoundedQueue, QueueBudget, RetainedSize};

/// A single deferred session-layer authoring operation.
///
/// The value ops (`SetTransform` / `SetVisibility`) have an existing session
/// apply path (`author_session_transform` / `author_session_visibility`, both
/// used by the level-transition sequence). The structural ops
/// (`DefinePrim` / `RemovePrim`) route through the session's
/// **TOPOLOGY** tick path (`define_prim` / `delete_prim` → `project_topology` +
/// `topology_generation` bump) so a freshly injected or removed prim is
/// re-hydrated the same frame. Richer attribute marshalling (typed scalar /
/// string / asset attributes) extends this enum additively as it lands.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEditOp {
    /// Author the target prim's local transform into the session layer
    /// (`author_session_transform`).
    SetTransform(Xform),
    /// Author the target prim's visibility into the session layer
    /// (`author_session_visibility`): `true` = `inherited`, `false` =
    /// `invisible`.
    SetVisibility(bool),
    /// Define a new child prim under the **target** prim (the target is the
    /// parent): a leaf `name` of USD type `type_name` (empty = typeless).
    /// Applied via the session's structural define path, re-projecting topology.
    DefinePrim {
        /// USD prim type (e.g. `"Xform"`, `"Sphere"`; empty = typeless).
        type_name: String,
        /// Leaf name of the child prim to create under the target.
        name: String,
    },
    /// Remove the **target** prim (and its subtree) from the session's edit
    /// target. Applied via the structural delete path, re-projecting topology.
    RemovePrim,
    /// Attach a screen-space HUD element (e.g. a center crosshair) to the
    /// **target** prim (typically the player camera): atomically define an
    /// `IdtxHud` child named `name` into the **session** layer and author its
    /// `idtx:hud:*` parameters there. Applied as define-child + author in one
    /// shot (the apply site knows the child path `<target>/<name>`), so a
    /// behavior does not need the new prim's entity id — which the SDK cannot
    /// hand back within the same frame. `size_px` is the on-screen size in
    /// pixels (`[0, 0]` = use the image's native size); `opacity` is in
    /// `[0, 1]`. The anchor is `center` for now.
    ///
    /// `idtx-behavior` carries only primitives here — the mapping to the
    /// `IdtxHud` schema vocabulary lives in the session's apply site.
    AttachHud {
        /// Leaf name of the HUD child prim created under the target.
        name: String,
        /// Image asset path (e.g. a crosshair PNG). Resolved + uploaded at
        /// runtime; native pixel size is read from the decoded image.
        image: String,
        /// On-screen size in pixels; `[0, 0]` means use the image's native size.
        size_px: [i32; 2],
        /// Opacity multiplier in `[0, 1]`.
        opacity: f32,
    },
    /// Author a string `value` into `idtx:widget:value` of the widget-field
    /// child `name` (panel-relative, e.g. `"Value"` / `"Row/Value"`) of the
    /// **target** `IdtxWidgetPanel`. The apply site joins `<target>/<name>` and
    /// authors the string into the session layer via `author_session_property`;
    /// because the property is in the `idtx:widget:` namespace, the session
    /// re-hydrates the panel and re-rasterizes it the same frame. Carries only
    /// primitives here — the `idtx:widget:value` attribute name lives in the
    /// session's apply site.
    SetWidgetValue {
        /// Panel-relative prim path of the `IdtxWidgetField*` child.
        name: String,
        /// String value to author into the child's `idtx:widget:value`.
        value: String,
    },
}

/// A queued session-layer edit addressed to an entity.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEditCommand {
    /// Entity whose prim the edit targets. [`Entity::NULL`] targets the
    /// caller's own entity (resolved by the drain site).
    pub target: Entity,
    /// The authoring operation to apply.
    pub op: SessionEditOp,
}

impl RetainedSize for SessionEditCommand {
    fn retained_size(&self) -> usize {
        let bytes = match &self.op {
            SessionEditOp::DefinePrim { type_name, name } => type_name.capacity().saturating_add(name.capacity()),
            SessionEditOp::AttachHud { name, image, .. } => name.capacity().saturating_add(image.capacity()),
            SessionEditOp::SetWidgetValue { name, value } => name.capacity().saturating_add(value.capacity()),
            _ => 0,
        };
        std::mem::size_of::<Self>().saturating_add(bytes)
    }
}

/// A simple FIFO queue of pending session-layer edits.
///
/// Behaviors push through the `stage_edit` host bucket; the session drains it
/// once per frame after the update hooks, applies each edit to the session
/// layer, and re-projects so the ECS/render worlds reflect it the same frame.
#[derive(Debug, Default)]
pub struct SessionEditQueue {
    pending: BoundedQueue<SessionEditCommand>,
}

impl SessionEditQueue {
    /// Enqueue an edit, or return `InvalidArgument` on size/capacity overflow.
    pub fn post(&mut self, target: Entity, op: SessionEditOp) -> Result<(), ErrorCode> {
        self.pending.push(SessionEditCommand { target, op })
    }

    pub(crate) fn with_budget(budget: QueueBudget, max_payload: usize) -> Self {
        Self { pending: BoundedQueue::new(budget, max_payload) }
    }

    pub(crate) fn clear(&mut self) { self.pending.clear(); }

    /// `true` when no edits are pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of pending edits.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Drain every pending edit, leaving the queue empty.
    #[must_use]
    pub fn drain(&mut self) -> Vec<SessionEditCommand> {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_drain_fifo() {
        let mut q = SessionEditQueue::default();
        assert!(q.is_empty());
        q.post(Entity::NULL, SessionEditOp::SetVisibility(false)).unwrap();
        q.post(
            Entity::from_bits(7),
            SessionEditOp::SetTransform(Xform::default()),
        ).unwrap();
        assert_eq!(q.len(), 2);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].target, Entity::NULL);
        assert_eq!(drained[0].op, SessionEditOp::SetVisibility(false));
        assert_eq!(drained[1].target, Entity::from_bits(7));
        assert!(matches!(drained[1].op, SessionEditOp::SetTransform(_)));
        assert!(q.is_empty());
    }

    #[test]
    fn structural_ops_round_trip_through_queue() {
        let mut q = SessionEditQueue::default();
        // DefinePrim targets the parent entity; RemovePrim targets the prim.
        q.post(
            Entity::from_bits(4),
            SessionEditOp::DefinePrim {
                type_name: "Xform".to_owned(),
                name: "Spawned".to_owned(),
            },
        ).unwrap();
        q.post(Entity::from_bits(9), SessionEditOp::RemovePrim).unwrap();
        assert_eq!(q.len(), 2);

        let drained = q.drain();
        match &drained[0].op {
            SessionEditOp::DefinePrim { type_name, name } => {
                assert_eq!(drained[0].target, Entity::from_bits(4));
                assert_eq!(type_name, "Xform");
                assert_eq!(name, "Spawned");
            }
            other => panic!("expected DefinePrim, got {other:?}"),
        }
        assert_eq!(drained[1].target, Entity::from_bits(9));
        assert_eq!(drained[1].op, SessionEditOp::RemovePrim);
        assert!(q.is_empty());
    }

    #[test]
    fn attach_hud_round_trips_through_queue() {
        let mut q = SessionEditQueue::default();
        // AttachHud targets the parent (camera) entity; the child is created
        // under it with the given name.
        q.post(
            Entity::from_bits(3),
            SessionEditOp::AttachHud {
                name: "Crosshair".to_owned(),
                image: "crosshair.png".to_owned(),
                size_px: [64, 64],
                opacity: 1.0,
            },
        ).unwrap();
        let drained = q.drain();
        match &drained[0].op {
            SessionEditOp::AttachHud {
                name,
                image,
                size_px,
                opacity,
            } => {
                assert_eq!(drained[0].target, Entity::from_bits(3));
                assert_eq!(name, "Crosshair");
                assert_eq!(image, "crosshair.png");
                assert_eq!(*size_px, [64, 64]);
                assert!((*opacity - 1.0).abs() < f32::EPSILON);
            }
            other => panic!("expected AttachHud, got {other:?}"),
        }
        assert!(q.is_empty());
    }

    #[test]
    fn set_widget_value_round_trips_through_queue() {
        let mut q = SessionEditQueue::default();
        // NULL target resolves to the attached panel at drain; the child is
        // addressed panel-relative by name.
        q.post(
            Entity::NULL,
            SessionEditOp::SetWidgetValue {
                name: "Row/Value".to_owned(),
                value: "42.0".to_owned(),
            },
        ).unwrap();
        let drained = q.drain();
        match &drained[0].op {
            SessionEditOp::SetWidgetValue { name, value } => {
                assert_eq!(drained[0].target, Entity::NULL);
                assert_eq!(name, "Row/Value");
                assert_eq!(value, "42.0");
            }
            other => panic!("expected SetWidgetValue, got {other:?}"),
        }
        assert!(q.is_empty());
    }
}
