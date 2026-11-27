//! Shared graph-canvas core used by both the Material Canvas and the Compute
//! Graph panels.
//!
//! The view-model ([`graph`]), per-tab state ([`state`]), auto-layout
//! ([`layout`]), interaction ([`interact`]), painting ([`paint`]) and theme
//! ([`theme`]) are graph-kind agnostic. Each concrete panel supplies only its
//! own `extract` step (building a [`Graph`]) and calls [`render_graph_canvas`].

use egui::Sense;

use paint::CanvasPalette;

pub mod graph;
pub mod interact;
pub mod layout;
pub mod paint;
pub mod state;
pub mod theme;

pub use graph::{
    ColumnPin, Connection, Graph, Node, NodeId, NodeRole, Socket, SocketKind, SocketType,
    aggregate_node_id, classify_type_name, node_id_for_path, split_connection_target, strip_ns,
};
pub use state::{CachedGraph, CanvasView, GraphCanvasState};

/// Empty-state hints shown by [`render_graph_canvas`] for the three
/// no-content situations (no selection, selection without a graph, graph with
/// no nodes).
#[derive(Debug, Clone, Copy)]
pub struct EmptyStateMessages {
    /// `graph_key` was `None` — nothing (relevant) is selected.
    pub no_selection: &'static str,
    /// A graph was requested but the cache could not be built.
    pub no_graph: &'static str,
    /// A graph was built but it has no nodes.
    pub no_nodes: &'static str,
}

/// Drive one frame of a graph canvas.
///
/// `graph_key` is the stable identity of the graph to show (`None` → the
/// `no_selection` empty state). `build_graph` is invoked **only on a cache
/// miss** (first show, or `graph_key`/`topology_generation` changed) — so the
/// caller may lazily hold whatever source data it needs. User node positions
/// survive a topology-only bump for the same `graph_key`.
pub fn render_graph_canvas(
    ui: &mut egui::Ui,
    state: &mut GraphCanvasState,
    graph_key: Option<&str>,
    topology_generation: u64,
    msgs: &EmptyStateMessages,
    build_graph: impl FnOnce() -> Graph,
) {
    let palette = CanvasPalette::from_ui(ui);

    let Some(graph_key) = graph_key else {
        show_empty_state(ui, msgs.no_selection);
        return;
    };

    // Rebuild the canvas graph on cache miss.
    let needs_rebuild = match &state.cache {
        None => true,
        Some(c) => c.graph_key != graph_key || c.stage_revision != topology_generation,
    };
    if needs_rebuild {
        let new_graph = build_graph();

        let key_changed = state
            .cache
            .as_ref()
            .is_none_or(|c| c.graph_key != graph_key);
        if key_changed {
            state.layout.clear();
            state.view = CanvasView::default();
            state.selected_node = None;
            state.needs_fit_to_view = true;
        }

        state.cache = Some(CachedGraph {
            graph_key: graph_key.to_owned(),
            stage_revision: topology_generation,
            graph: new_graph,
        });
    }

    let Some(cache) = &state.cache else {
        show_empty_state(ui, msgs.no_graph);
        return;
    };

    if cache.graph.nodes.is_empty() {
        show_empty_state(ui, msgs.no_nodes);
        return;
    }

    // Fill missing layout positions.
    layout::auto_layout(&cache.graph, &mut state.layout);

    let canvas_rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(canvas_rect, Sense::click_and_drag());

    // Take the cache out so `interact` and `paint` can access it while
    // `state` is mutably borrowed for pan/zoom/drag updates.
    let Some(owned_cache) = state.cache.take() else {
        tracing::error!("Unable to take the current cache.");
        return;
    };

    if state.needs_fit_to_view {
        interact::fit_to_view(state, &owned_cache.graph, canvas_rect);
        state.needs_fit_to_view = false;
    }

    interact::process_input(ui, canvas_rect, &response, state, &owned_cache.graph);

    paint::paint_graph(
        ui,
        canvas_rect,
        &state.view,
        &owned_cache.graph,
        &state.layout,
        palette,
        state.selected_node,
    );

    state.cache = Some(owned_cache);
}

/// Centred weak-text hint for an empty canvas.
pub fn show_empty_state(ui: &mut egui::Ui, msg: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(egui::RichText::new(msg).weak());
    });
}
