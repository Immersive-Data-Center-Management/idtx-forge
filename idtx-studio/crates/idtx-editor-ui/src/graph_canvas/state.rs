//! Per-tab state for the shared graph canvas (material + compute).

use std::collections::HashMap;

use egui::Vec2;

use super::graph::{Graph, NodeId};

/// Pan + zoom of the canvas viewport.
///
/// ```text
/// screen = canvas_rect.min + view.pan + canvas_pos * view.zoom
/// ```
#[derive(Debug, Clone, Copy)]
pub struct CanvasView {
    pub pan: Vec2,
    pub zoom: f32,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            pan: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl CanvasView {
    pub const MIN_ZOOM: f32 = 0.25;
    pub const MAX_ZOOM: f32 = 3.0;

    #[must_use]
    pub fn canvas_to_screen(&self, canvas_origin: Vec2, p: Vec2) -> Vec2 {
        canvas_origin + self.pan + p * self.zoom
    }

    #[must_use]
    pub fn screen_to_canvas(&self, canvas_origin: Vec2, p: Vec2) -> Vec2 {
        (p - canvas_origin - self.pan) / self.zoom.max(0.0001)
    }
}

/// The previously-extracted graph, kept so we can skip re-extraction on
/// selection-noop frames.
#[derive(Debug, Clone)]
pub struct CachedGraph {
    /// Stable identity of the cached graph (material path / compute root path).
    pub graph_key: String,
    pub stage_revision: u64,
    pub graph: Graph,
}

/// Persistent graph-canvas state carried per stage tab.
#[derive(Debug, Clone, Default)]
pub struct GraphCanvasState {
    pub cache: Option<CachedGraph>,
    pub view: CanvasView,
    pub layout: HashMap<NodeId, Vec2>,
    pub dragging_node: Option<NodeId>,
    pub drag_offset: Vec2,
    pub selected_node: Option<NodeId>,
    /// Set to `true` the first time a graph is displayed so the paint pass
    /// can fit-to-view before rendering.
    pub needs_fit_to_view: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_to_screen_round_trips_with_offset_origin() {
        let view = CanvasView {
            pan: Vec2::new(15.0, -7.0),
            zoom: 1.75,
        };
        let origin = Vec2::new(320.0, 260.0);
        let canvas_point = Vec2::new(50.0, 30.0);

        let screen = view.canvas_to_screen(origin, canvas_point);
        assert!(screen.x > 320.0);
        assert!(screen.y > 260.0);

        let back = view.screen_to_canvas(origin, screen);
        assert!((back.x - canvas_point.x).abs() < 1e-3);
        assert!((back.y - canvas_point.y).abs() < 1e-3);
    }

    #[test]
    fn zero_pan_zero_zoom_pins_to_canvas_origin() {
        let view = CanvasView {
            pan: Vec2::ZERO,
            zoom: 1.0,
        };
        let origin = Vec2::new(500.0, 400.0);
        let mapped = view.canvas_to_screen(origin, Vec2::ZERO);
        assert_eq!(mapped, origin);
    }
}
