//! Interaction handling for the graph canvas.

use egui::{Rect, Ui, Vec2};

use super::graph::{Graph, NodeId, SocketKind};
use super::paint::{node_rect, socket_center};
use super::state::{CanvasView, GraphCanvasState};
use super::theme::{HEADER_HEIGHT, NODE_WIDTH, SOCKET_RADIUS, node_height_for_socket_rows};

pub fn process_input(
    ui: &mut Ui,
    canvas_rect: Rect,
    response: &egui::Response,
    state: &mut GraphCanvasState,
    graph: &Graph,
) {
    if response.hovered() {
        let (scroll, gesture_zoom, pointer) = ui.ctx().input(|i| {
            (
                i.smooth_scroll_delta(),
                i.zoom_delta(),
                i.pointer.hover_pos().map(egui::Pos2::to_vec2),
            )
        });
        apply_navigation_input(
            &mut state.view,
            canvas_rect.min.to_vec2(),
            pointer,
            scroll,
            gesture_zoom,
            cfg!(target_os = "macos"),
        );
    }

    if response.double_clicked() {
        let pointer_over_node = response
            .interact_pointer_pos()
            .and_then(|p| hit_test_node(graph, &state.layout, canvas_rect, &state.view, p))
            .is_some();
        if !pointer_over_node {
            fit_to_view(state, graph, canvas_rect);
        }
    }

    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(press) = response.interact_pointer_pos()
    {
        let canvas_origin = canvas_rect.min.to_vec2();
        let press_canvas = state
            .view
            .screen_to_canvas(canvas_origin, Vec2::new(press.x, press.y));
        if let Some(id) = hit_test_header(graph, &state.layout, press_canvas) {
            state.dragging_node = Some(id);
            let origin = state.layout.get(&id).copied().unwrap_or(Vec2::ZERO);
            state.drag_offset = press_canvas - origin;
            state.selected_node = Some(id);
        } else {
            state.dragging_node = None;
        }
    }

    if response.dragged_by(egui::PointerButton::Primary)
        && let Some(id) = state.dragging_node
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let canvas_origin = canvas_rect.min.to_vec2();
        let pointer_canvas = state
            .view
            .screen_to_canvas(canvas_origin, Vec2::new(pointer.x, pointer.y));
        let new_origin = pointer_canvas - state.drag_offset;
        state.layout.insert(id, new_origin);
    }

    if response.drag_stopped_by(egui::PointerButton::Primary) {
        state.dragging_node = None;
    }

    if state.dragging_node.is_none() && response.dragged_by(egui::PointerButton::Middle) {
        state.view.pan += response.drag_delta();
    }

    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let canvas_origin = canvas_rect.min.to_vec2();
        let p_canvas = state
            .view
            .screen_to_canvas(canvas_origin, Vec2::new(pos.x, pos.y));
        let hit = hit_test_node(graph, &state.layout, canvas_rect, &state.view, pos)
            .or_else(|| hit_test_body(graph, &state.layout, p_canvas));
        state.selected_node = hit;
    }
}

fn apply_navigation_input(
    view: &mut CanvasView,
    canvas_origin: Vec2,
    pointer: Option<Vec2>,
    scroll: Vec2,
    gesture_zoom: f32,
    scroll_pans: bool,
) {
    if scroll_pans {
        view.pan += scroll;
    }

    let wheel_zoom = if scroll_pans || scroll.y.abs() <= f32::EPSILON {
        1.0
    } else {
        (1.0 + scroll.y * 0.005).max(0.01)
    };
    let zoom_factor = gesture_zoom * wheel_zoom;

    if !zoom_factor.is_finite() || zoom_factor <= 0.0 || (zoom_factor - 1.0).abs() <= f32::EPSILON {
        return;
    }
    let Some(pointer) = pointer else { return };

    let old_zoom = view.zoom;
    let new_zoom = (old_zoom * zoom_factor).clamp(CanvasView::MIN_ZOOM, CanvasView::MAX_ZOOM);
    let pointer_canvas = (pointer - canvas_origin - view.pan) / old_zoom.max(0.0001);
    view.pan = pointer - canvas_origin - pointer_canvas * new_zoom;
    view.zoom = new_zoom;
}

fn hit_test_header(
    graph: &Graph,
    layout: &std::collections::HashMap<NodeId, Vec2>,
    p_canvas: Vec2,
) -> Option<NodeId> {
    for node in &graph.nodes {
        let Some(&origin) = layout.get(&node.id) else {
            continue;
        };
        let header = egui::Rect::from_min_size(
            egui::Pos2::new(origin.x, origin.y),
            Vec2::new(NODE_WIDTH, HEADER_HEIGHT),
        );
        if header.contains(egui::Pos2::new(p_canvas.x, p_canvas.y)) {
            return Some(node.id);
        }
    }
    None
}

fn hit_test_body(
    graph: &Graph,
    layout: &std::collections::HashMap<NodeId, Vec2>,
    p_canvas: Vec2,
) -> Option<NodeId> {
    for node in &graph.nodes {
        let Some(&origin) = layout.get(&node.id) else {
            continue;
        };
        let r = node_rect(node, origin);
        if r.contains(egui::Pos2::new(p_canvas.x, p_canvas.y)) {
            return Some(node.id);
        }
    }
    None
}

fn hit_test_node(
    graph: &Graph,
    layout: &std::collections::HashMap<NodeId, Vec2>,
    canvas_rect: Rect,
    view: &CanvasView,
    p_screen: egui::Pos2,
) -> Option<NodeId> {
    let canvas_origin = canvas_rect.min.to_vec2();
    let p_canvas = view.screen_to_canvas(canvas_origin, Vec2::new(p_screen.x, p_screen.y));
    for node in &graph.nodes {
        let Some(&origin) = layout.get(&node.id) else {
            continue;
        };
        let expanded = node_rect(node, origin).expand(SOCKET_RADIUS + 1.0);
        if expanded.contains(egui::Pos2::new(p_canvas.x, p_canvas.y)) {
            return Some(node.id);
        }
        let hit_r_sq = (SOCKET_RADIUS * 2.0).powi(2);
        for i in 0..node.inputs.len() {
            let c = socket_center(node, i, SocketKind::Input, origin);
            if (c - p_canvas).length_sq() <= hit_r_sq {
                return Some(node.id);
            }
        }
        for i in 0..node.outputs.len() {
            let c = socket_center(node, i, SocketKind::Output, origin);
            if (c - p_canvas).length_sq() <= hit_r_sq {
                return Some(node.id);
            }
        }
    }
    None
}

pub fn fit_to_view(state: &mut GraphCanvasState, graph: &Graph, canvas_rect: Rect) {
    let mut min = Vec2::new(f32::INFINITY, f32::INFINITY);
    let mut max = Vec2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut any = false;
    for node in &graph.nodes {
        let Some(&origin) = state.layout.get(&node.id) else {
            continue;
        };
        any = true;
        let rows = node.inputs.len().max(node.outputs.len());
        let h = node_height_for_socket_rows(rows);
        min.x = min.x.min(origin.x - SOCKET_RADIUS);
        min.y = min.y.min(origin.y);
        max.x = max.x.max(origin.x + NODE_WIDTH + SOCKET_RADIUS);
        max.y = max.y.max(origin.y + h);
    }
    if !any {
        state.view = CanvasView::default();
        return;
    }
    let content_size = max - min;
    if content_size.x <= 0.0 || content_size.y <= 0.0 {
        return;
    }
    let margin = 24.0_f32;
    let avail = canvas_rect.size() - Vec2::splat(margin * 2.0);
    if avail.x <= 0.0 || avail.y <= 0.0 {
        return;
    }
    let zoom_x = avail.x / content_size.x;
    let zoom_y = avail.y / content_size.y;
    let zoom = zoom_x
        .min(zoom_y)
        .clamp(CanvasView::MIN_ZOOM, CanvasView::MAX_ZOOM);

    let content_centre = (min + max) * 0.5;
    let panel_half = canvas_rect.size() * 0.5;
    state.view.zoom = zoom;
    state.view.pan = panel_half - content_centre * zoom;
}

#[cfg(test)]
mod tests {
    use super::super::graph::{
        ColumnPin, Connection, Graph, Node, NodeId, NodeRole, Socket, SocketKind, SocketType,
        node_id_for_path,
    };
    use super::*;
    use egui::Pos2;
    use std::collections::HashMap;

    fn mk_node(path: &str) -> Node {
        Node {
            id: node_id_for_path(path),
            prim_path: path.to_owned(),
            display_name: path.rsplit('/').next().unwrap_or("").to_owned(),
            role: NodeRole::Standard,
            pin: ColumnPin::None,
            accent_rgb: None,
            subtitle: None,
            inputs: vec![Socket {
                name: "in".to_owned(),
                full_name: "inputs:in".to_owned(),
                kind: SocketKind::Input,
                ty: SocketType::Float,
                authored_value: None,
            }],
            outputs: vec![Socket {
                name: "out".to_owned(),
                full_name: "outputs:out".to_owned(),
                kind: SocketKind::Output,
                ty: SocketType::Float,
                authored_value: None,
            }],
        }
    }

    #[test]
    fn fit_to_view_centres_content_in_canvas_rect() {
        let a = mk_node("/M/A");
        let b = mk_node("/M/B");
        let a_id = a.id;
        let b_id = b.id;
        let graph = Graph {
            graph_key: "/M".to_owned(),
            nodes: vec![a, b],
            connections: vec![Connection {
                from: (a_id, 0),
                to: (b_id, 0),
            }],
        };

        let mut state = GraphCanvasState::default();
        state.layout.insert(a_id, Vec2::new(0.0, 0.0));
        state.layout.insert(b_id, Vec2::new(300.0, 200.0));

        let canvas_rect = Rect::from_min_size(Pos2::new(400.0, 300.0), egui::vec2(600.0, 400.0));

        fit_to_view(&mut state, &graph, canvas_rect);

        assert!(state.view.zoom >= CanvasView::MIN_ZOOM);
        assert!(state.view.zoom <= CanvasView::MAX_ZOOM);

        let canvas_origin = canvas_rect.min.to_vec2();
        let rows = 1_usize;
        let h = node_height_for_socket_rows(rows);
        let cmin = Vec2::new(0.0 - SOCKET_RADIUS, 0.0);
        let cmax = Vec2::new(300.0 + NODE_WIDTH + SOCKET_RADIUS, 200.0 + h);
        let centre = (cmin + cmax) * 0.5;

        let screen_centre = state.view.canvas_to_screen(canvas_origin, centre);
        let target = canvas_rect.center();
        assert!((screen_centre.x - target.x).abs() < 1e-3);
        assert!((screen_centre.y - target.y).abs() < 1e-3);
    }

    #[test]
    fn fit_to_view_empty_graph_resets_view() {
        let mut state = GraphCanvasState::default();
        state.view = CanvasView {
            pan: Vec2::new(99.0, 99.0),
            zoom: 2.5,
        };
        let graph = Graph {
            graph_key: "/M".to_owned(),
            nodes: vec![],
            connections: vec![],
        };
        let canvas_rect = Rect::from_min_size(Pos2::new(0.0, 0.0), egui::vec2(400.0, 300.0));
        fit_to_view(&mut state, &graph, canvas_rect);
        assert_eq!(state.view.pan, Vec2::ZERO);
        assert!((state.view.zoom - 1.0).abs() < 1e-6);
    }
}
