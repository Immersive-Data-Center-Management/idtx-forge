//! Widget-interaction host state (ABI 3 — `widget` capability).
//!
//! Shared across behavior stores (like the installed `SceneBackend`), this holds
//! the three pieces the widget-click path needs the host to coordinate:
//!
//! - **`registry`** — which `(panel, element path, kind)` triples have a handler,
//!   so a pointer behavior's `handle` import can report `consumed` synchronously
//!   and the host only dispatches events that will be handled. A panel behavior
//!   populates it from `on_start` via `idtx_widget_register_handler`.
//! - **`markers`** — hit-point cues to draw this frame. The `handle` import pushes
//!   one whenever a ray hits a panel (regardless of click); the session drains
//!   them after `run_frame` into the overlay line set, so the cue is drawn by the
//!   host independent of any behavior's `overlay` capability.
//! - **`pending`** — clicks to dispatch this frame. `handle` pushes one when a
//!   `widgets.*` action edge fired on an element with a registered handler;
//!   `run_frame` drains them and delivers each to the panel behavior's
//!   `_idtx_on_event`.
//!
//! The whole struct is wrapped in `Arc<Mutex<_>>` and cloned into each store's
//! `HostState`, mirroring `scene_backend`.

use std::collections::HashMap;

use idtx_behavior_abi::{Entity as WireEntity, ErrorCode, WidgetEventKind};
use crate::limits::WorkLimits;
use crate::work::{BoundedQueue, QueueBudget, QueueOwner, Queued, Reservation, RetainedSize};

/// A hit-point cue to render this frame (stage space).
#[derive(Clone, Copy, Debug)]
pub struct WidgetMarker {
    /// World-space hit position on the panel.
    pub position: glam::Vec3,
    /// Panel normal at the hit (unit), to orient the cue ring.
    pub normal: glam::Vec3,
}

impl RetainedSize for WidgetMarker {
    fn retained_size(&self) -> usize { std::mem::size_of::<Self>() }
}

/// A click to deliver to a panel behavior's `_idtx_on_event` this frame.
#[derive(Clone, Debug)]
pub struct PendingWidgetEvent {
    pub source: idtx_behavior_abi::PointerSource,
    /// The panel entity the event targets (its behavior receives it).
    pub panel: WireEntity,
    /// Panel-relative element path (`"Footer/QuitButton"`).
    pub element: String,
    /// Panel-local UV of the hit.
    pub uv: [f32; 2],
    /// The interaction kind.
    pub kind: WidgetEventKind,
}

impl RetainedSize for PendingWidgetEvent {
    fn retained_size(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.element.capacity())
    }
}

/// The widget event currently being delivered to `_idtx_on_event`, seeded into
/// the receiving store so the always-linked `idtx_widget_event_*` reads can
/// recover it (mirrors `HostState::current_event`). `None` outside a widget
/// dispatch — the reads then report [`WidgetEventKind`]-none.
#[derive(Clone, Debug)]
pub struct CurrentWidgetEvent {
    pub source: idtx_behavior_abi::PointerSource,
    /// Panel-relative element path.
    pub element: String,
    /// Panel-local UV.
    pub uv: [f32; 2],
    /// The interaction kind.
    pub kind: WidgetEventKind,
}

/// Shared widget-interaction coordination state.
pub struct WidgetInteraction {
    highlights: HashMap<WireEntity, (String, QueueOwner)>,
    used_clicks: std::collections::HashSet<idtx_behavior_abi::PointerSource>,
    /// `(panel, element path, kind)` triples that have a registered handler.
    registry: HashMap<(u64, String, u32), Vec<(QueueOwner, Reservation)>>,
    /// Hit-point cues accumulated this frame (drained by the session).
    markers: BoundedQueue<WidgetMarker>,
    /// Clicks to dispatch this frame (drained by `run_frame`).
    pending: BoundedQueue<PendingWidgetEvent>,
    budget: QueueBudget,
    registry_budget: QueueBudget,
    argument_bytes: usize,
}

impl Default for WidgetInteraction {
    fn default() -> Self {
        let limits = WorkLimits::default();
        Self::with_budget(QueueBudget::new(limits.host_queue_items, limits.host_queue_bytes), limits)
    }
}

impl WidgetInteraction {
    pub(crate) fn highlight(&mut self, panel: WireEntity, path: &str, owner: QueueOwner) {
        if path.is_empty() { self.highlights.remove(&panel); }
        else if self.highlights.len() < 128 || self.highlights.contains_key(&panel) {
            self.highlights.insert(panel, (path.into(), owner));
        }
    }

    pub fn highlights(&self) -> Vec<(WireEntity, String)> {
        self.highlights.iter().filter(|(_, (_, owner))| owner.is_active()).map(|(&e, (p, _))| (e, p.clone())).collect()
    }
    pub(crate) fn begin_frame(&mut self) { self.used_clicks.clear(); }

    /// One physical confirmation cannot activate two surfaces during dispatch.
    pub(crate) fn take_click(&mut self, source: idtx_behavior_abi::PointerSource) -> bool {
        use idtx_behavior_abi::PointerSource;
        if self.used_clicks.contains(&PointerSource::Unspecified)
            || (source == PointerSource::Unspecified && !self.used_clicks.is_empty()) { return false; }
        self.used_clicks.insert(source)
    }

    pub(crate) fn with_budget(budget: QueueBudget, limits: WorkLimits) -> Self {
        Self {
            used_clicks: Default::default(),
            highlights: HashMap::new(),
            registry: HashMap::new(),
            markers: BoundedQueue::new(budget.clone(), limits.argument_bytes),
            pending: BoundedQueue::new(budget.clone(), limits.argument_bytes),
            registry_budget: budget.child(limits.widget_handlers, limits.widget_handler_bytes),
            budget,
            argument_bytes: limits.argument_bytes,
        }
    }

    /// Record that `panel`'s `element` handles `kind` events.
    /// Duplicate registrations by the same owner consume no additional quota.
    pub fn register(&mut self, panel: WireEntity, element: &str, kind: WidgetEventKind) -> Result<(), ErrorCode> {
        self.register_from(panel, element, kind, &self.budget.clone())
    }

    pub(crate) fn register_from(&mut self, panel: WireEntity, element: &str, kind: WidgetEventKind, source: &QueueBudget) -> Result<(), ErrorCode> {
        self.prune_expired();
        if element.len() > self.argument_bytes || !source.owner().is_active() {
            return Err(ErrorCode::InvalidArgument);
        }
        let key = (panel.to_bits(), element.to_owned(), kind.as_u32());
        let owner = source.owner();
        if self.registry.get(&key).is_some_and(|owners| owners.iter().any(|(o, _)| o.same(&owner))) {
            return Ok(());
        }
        let bytes = std::mem::size_of_val(&key).saturating_add(element.len())
            .saturating_add(std::mem::size_of::<(QueueOwner, Reservation)>());
        let reservation = self.registry_budget.reserve(bytes)?;
        self.registry.entry(key).or_default().push((owner, reservation));
        Ok(())
    }

    /// `true` when a handler is registered for `(panel, element, kind)`.
    #[must_use]
    pub fn has_handler(&self, panel: WireEntity, element: &str, kind: WidgetEventKind) -> bool {
        self.registry
            .get(&(panel.to_bits(), element.to_owned(), kind.as_u32()))
            .is_some_and(|owners| owners.iter().any(|(owner, _)| owner.is_active()))
    }

    /// Queue a hit-point cue for this frame.
    pub fn push_marker(&mut self, marker: WidgetMarker) -> Result<(), ErrorCode> {
        self.markers.push(marker)
    }

    pub(crate) fn push_marker_from(&mut self, marker: WidgetMarker, source: &QueueBudget) -> Result<(), ErrorCode> {
        self.markers.push_from(marker, source)
    }

    /// Queue a click for dispatch this frame.
    pub fn push_pending(&mut self, ev: PendingWidgetEvent) -> Result<(), ErrorCode> {
        self.pending.push(ev)
    }

    pub(crate) fn push_pending_from(&mut self, ev: PendingWidgetEvent, source: &QueueBudget) -> Result<(), ErrorCode> {
        self.pending.push_from(ev, source)
    }

    pub(crate) fn take_pending(&mut self) -> Vec<Queued<PendingWidgetEvent>> {
        self.pending.take_all()
    }

    /// Take the frame's accumulated cues (leaves the buffer empty).
    #[must_use]
    pub fn drain_markers(&mut self) -> Vec<WidgetMarker> {
        self.prune_expired();
        self.markers.drain()
    }

    /// Take the frame's pending clicks (leaves the buffer empty).
    #[must_use]
    pub fn drain_pending(&mut self) -> Vec<PendingWidgetEvent> {
        self.pending.drain()
    }

    /// Drop every handler registered for `panel` (on detach / destroy).
    pub fn clear_panel(&mut self, panel: WireEntity) {
        self.highlights.remove(&panel);
        let bits = panel.to_bits();
        self.registry.retain(|(p, _, _), _| *p != bits);
        self.pending.retain(|ev| ev.panel != panel);
    }

    pub(crate) fn prune_expired(&mut self) {
        self.highlights.retain(|_, (_, owner)| owner.is_active());
        self.registry.retain(|_, owners| {
            owners.retain(|(owner, _)| owner.is_active());
            !owners.is_empty()
        });
        self.markers.retain(|_| true);
        self.pending.retain(|_| true);
    }

    pub(crate) fn clear(&mut self) {
        self.highlights.clear();
        self.registry.clear();
        self.markers.clear();
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(bits: u64) -> WireEntity {
        WireEntity::from_bits(bits)
    }

    #[test]
    fn registry_tracks_panel_element_kind() {
        let mut w = WidgetInteraction::default();
        let panel = e(7);
        w.register(panel, "Controls/Increment", WidgetEventKind::Click).unwrap();
        assert!(w.has_handler(panel, "Controls/Increment", WidgetEventKind::Click));
        // Different element / kind / panel → not registered.
        assert!(!w.has_handler(panel, "Controls/Decrement", WidgetEventKind::Click));
        assert!(!w.has_handler(panel, "Controls/Increment", WidgetEventKind::Scroll));
        assert!(!w.has_handler(e(8), "Controls/Increment", WidgetEventKind::Click));
    }

    #[test]
    fn markers_and_pending_drain_once() {
        let mut w = WidgetInteraction::default();
        w.push_marker(WidgetMarker {
            position: glam::Vec3::ZERO,
            normal: glam::Vec3::Z,
        }).unwrap();
        w.push_pending(PendingWidgetEvent {
            source: idtx_behavior_abi::PointerSource::Unspecified,
            panel: e(1),
            element: "Btn".to_owned(),
            uv: [0.5, 0.5],
            kind: WidgetEventKind::Click,
        }).unwrap();
        assert_eq!(w.drain_markers().len(), 1);
        assert!(w.drain_markers().is_empty(), "drain leaves the buffer empty");
        assert_eq!(w.drain_pending().len(), 1);
        assert!(w.drain_pending().is_empty());
    }

    #[test]
    fn clear_panel_drops_only_that_panel() {
        let mut w = WidgetInteraction::default();
        w.register(e(1), "A", WidgetEventKind::Click).unwrap();
        w.register(e(2), "B", WidgetEventKind::Click).unwrap();
        w.clear_panel(e(1));
        assert!(!w.has_handler(e(1), "A", WidgetEventKind::Click));
        assert!(w.has_handler(e(2), "B", WidgetEventKind::Click));
    }
}
