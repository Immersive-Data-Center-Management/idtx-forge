//! Compute Graph — a read-only visualisation of the `OpenExec` compute-node
//! chain feeding the currently-selected prim's connected attribute.
//!
//! A thin wrapper over the shared [`crate::graph_canvas`] core (the same canvas
//! that renders the Material Canvas): it supplies only the compute-specific
//! [`extract`] step and empty-state copy.
//!
//! Public entry point: [`compute_canvas_panel`].

pub mod extract;
pub mod theme;

pub use extract::ComputeGraphSource;

use crate::graph_canvas::{EmptyStateMessages, GraphCanvasState, render_graph_canvas};

const COMPUTE_MSGS: EmptyStateMessages = EmptyStateMessages {
    no_selection: "Select a prim with a connected compute attribute",
    no_graph: "No compute graph for this prim",
    no_nodes: "No compute nodes found",
};

/// Draw the Compute Graph inside the parent `ui`.
///
/// `source` is pre-computed by the shell's cache-refresh logic. When `None`
/// is passed, the panel renders the empty-state hint. Cache invalidation keys
/// on `source.root_path` and `source.topology_generation`; user node positions
/// survive a topology-only bump for the same root prim.
pub fn compute_canvas_panel(
    ui: &mut egui::Ui,
    state: &mut GraphCanvasState,
    source: Option<&ComputeGraphSource>,
) {
    let (graph_key, topo_gen) = match source {
        Some(s) => (Some(s.root_path.as_str()), s.topology_generation),
        None => (None, 0),
    };
    render_graph_canvas(
        ui,
        state,
        graph_key,
        topo_gen,
        &COMPUTE_MSGS,
        // Reached only on a cache miss, which implies `source` is `Some`; the
        // `None` fallback is unreachable but keeps the closure panic-free.
        || source.map(extract::extract_compute_graph).unwrap_or_default(),
    );
}
