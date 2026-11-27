//! Material Canvas — a read-only visualisation of the shader graph of
//! the currently-selected `UsdShade.Material` prim (or the material
//! bound to the selected prim).
//!
//! This is a thin wrapper over the shared [`crate::graph_canvas`] core: it
//! supplies only the material-specific [`extract`] step and the empty-state
//! copy. All rendering, layout, interaction and state live in `graph_canvas`.
//!
//! Public entry point: [`material_canvas_panel`].

pub mod extract;

pub use extract::CanvasGraphSource;

// Re-export the shared view-model so existing `material_canvas::{..}` paths and
// the historical type names keep working.
pub use crate::graph_canvas::{
    CachedGraph, CanvasView, Connection, Node, NodeId, Socket, SocketKind, SocketType,
};
pub use crate::graph_canvas::{EmptyStateMessages, render_graph_canvas};

/// Historical alias — the per-tab state is the shared [`crate::graph_canvas::GraphCanvasState`].
pub type MaterialCanvasState = crate::graph_canvas::GraphCanvasState;
/// Historical alias — the extracted view is the shared [`crate::graph_canvas::Graph`].
pub type MaterialGraph = crate::graph_canvas::Graph;

const MATERIAL_MSGS: EmptyStateMessages = EmptyStateMessages {
    no_selection: "Select a Material or material-bound prim in the scene tree",
    no_graph: "Material selected but no shader graph found",
    no_nodes: "No shader nodes found in this material",
};

/// Draw the Material Canvas inside the parent `ui`.
///
/// `source` is pre-computed by the shell's cache-refresh logic (see
/// `idtx-studio-shell`). When `None` is passed, the panel renders an
/// appropriate empty-state hint.
///
/// Cache invalidation: the canvas rebuilds its internal graph whenever
/// `source.material_path` or `source.topology_generation` differs from
/// the state's cached key. User node positions survive a topology-only
/// bump for the same material.
pub fn material_canvas_panel(
    ui: &mut egui::Ui,
    state: &mut MaterialCanvasState,
    source: Option<&CanvasGraphSource>,
) {
    let (graph_key, topo_gen) = match source {
        Some(s) => (Some(s.material_path.as_str()), s.topology_generation),
        None => (None, 0),
    };
    render_graph_canvas(
        ui,
        state,
        graph_key,
        topo_gen,
        &MATERIAL_MSGS,
        // Reached only on a cache miss, which implies `source` is `Some`; the
        // `None` fallback is unreachable but keeps the closure panic-free.
        || source.map(extract::extract_canvas_graph).unwrap_or_default(),
    );
}
