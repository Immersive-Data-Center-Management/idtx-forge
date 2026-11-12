//! Node + connection painting for the graph canvas.
//!
//! Coordinate space:
//! ```text
//! screen = canvas_rect.min + view.pan + canvas_pos * view.zoom
//! ```

use std::collections::HashMap;

use egui::{
    Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2, epaint::CubicBezierShape,
};
use num_traits::ToPrimitive;

use super::graph::{Graph, Node, NodeId, NodeRole, SocketKind};
use super::state::CanvasView;
use super::theme::{
    AGGREGATE_HEADER_HEIGHT, AGGREGATE_WIDTH, HEADER_HEIGHT, NODE_CORNER, NODE_WIDTH,
    SOCKET_RADIUS, SOCKET_ROW_HEIGHT, VALUE_BOX_CORNER, VALUE_BOX_GAP, VALUE_BOX_HPAD,
    VALUE_BOX_VPAD, aggregate_height_for_socket_rows, node_body_fill, node_height_for_socket_rows,
    node_outline, socket_color, value_box_fill, value_box_text,
};

/// Minimal palette derived from egui's visual style.
#[derive(Clone, Copy, Debug)]
pub struct CanvasPalette {
    pub inspector_bg: Color32,
    pub accent: Color32,
    pub heading: Color32,
}

impl CanvasPalette {
    #[must_use]
    pub fn from_ui(ui: &Ui) -> Self {
        let vis = &ui.style().visuals;
        Self {
            inspector_bg: vis.panel_fill,
            accent: vis.selection.bg_fill,
            heading: vis.strong_text_color(),
        }
    }
}

#[must_use]
pub fn node_width(node: &Node) -> f32 {
    match node.role {
        NodeRole::Standard => NODE_WIDTH,
        NodeRole::SourceAggregate | NodeRole::SinkAggregate => AGGREGATE_WIDTH,
    }
}

#[must_use]
pub fn node_header_height(node: &Node) -> f32 {
    match node.role {
        NodeRole::Standard => HEADER_HEIGHT,
        NodeRole::SourceAggregate | NodeRole::SinkAggregate => AGGREGATE_HEADER_HEIGHT,
    }
}

#[must_use]
pub fn node_rect(node: &Node, origin: Vec2) -> Rect {
    let (_, h) = node_size(node);
    Rect::from_min_size(
        Pos2::new(origin.x, origin.y),
        Vec2::new(node_width(node), h),
    )
}

#[must_use]
pub fn node_size(node: &Node) -> (f32, f32) {
    match node.role {
        NodeRole::Standard => {
            let rows = node.inputs.len().max(node.outputs.len());
            (NODE_WIDTH, node_height_for_socket_rows(rows))
        }
        NodeRole::SourceAggregate => {
            let rows = node.inputs.len().max(node.outputs.len());
            (AGGREGATE_WIDTH, aggregate_height_for_socket_rows(rows))
        }
        NodeRole::SinkAggregate => {
            let rows = node.inputs.len();
            (AGGREGATE_WIDTH, aggregate_height_for_socket_rows(rows))
        }
    }
}

#[must_use]
pub fn socket_center(node: &Node, socket_idx: usize, kind: SocketKind, origin: Vec2) -> Vec2 {
    let w = node_width(node);
    let hdr = node_header_height(node);
    let row_y = origin.y
        + hdr
        + SOCKET_ROW_HEIGHT * (0.5 + f32::from(socket_idx.to_u16().unwrap_or(u16::MAX)));
    let x = match node.role {
        NodeRole::Standard | NodeRole::SourceAggregate => match kind {
            SocketKind::Input => origin.x,
            SocketKind::Output => origin.x + w,
        },
        NodeRole::SinkAggregate => origin.x,
    };
    Vec2::new(x, row_y)
}

pub fn paint_graph<H: ::std::hash::BuildHasher>(
    ui: &mut Ui,
    canvas_rect: Rect,
    view: &CanvasView,
    graph: &Graph,
    layout: &HashMap<NodeId, Vec2, H>,
    palette: CanvasPalette,
    selected: Option<NodeId>,
) {
    let canvas_origin = canvas_rect.min.to_vec2();
    let painter = ui.painter_at(canvas_rect);

    painter.rect_filled(canvas_rect, CornerRadius::ZERO, canvas_background(palette));
    paint_grid(&painter, canvas_rect, view);
    paint_connections(&painter, canvas_rect, canvas_origin, view, graph, layout);

    for node in &graph.nodes {
        let Some(&origin) = layout.get(&node.id) else {
            continue;
        };
        paint_node(
            &painter,
            canvas_origin,
            view,
            node,
            origin,
            palette,
            selected == Some(node.id),
        );
    }

    for node in &graph.nodes {
        let Some(&origin) = layout.get(&node.id) else {
            continue;
        };
        paint_value_boxes(&painter, canvas_origin, view, node, origin);
    }
}

fn canvas_background(palette: CanvasPalette) -> Color32 {
    let [r, g, b, _] = palette.inspector_bg.to_array();
    let scale = 0.85;
    Color32::from_rgb(
        (f32::from(r) * scale).to_u8().unwrap_or(u8::MAX),
        (f32::from(g) * scale).to_u8().unwrap_or(u8::MAX),
        (f32::from(b) * scale).to_u8().unwrap_or(u8::MAX),
    )
}

fn paint_grid(painter: &egui::Painter, canvas_rect: Rect, view: &CanvasView) {
    const CANVAS_GRID: f32 = 40.0;
    let step = CANVAS_GRID * view.zoom.max(0.25);
    if step < 6.0 {
        return;
    }
    let dot_color = Color32::from_gray(80).linear_multiply(0.4);
    let radius = (0.75_f32).max(view.zoom * 0.5);
    let start_x = canvas_rect.left() + ((view.pan.x % step) + step).rem_euclid_f32(step) - step;
    let start_y = canvas_rect.top() + ((view.pan.y % step) + step).rem_euclid_f32(step) - step;
    let mut y = start_y;
    while y < canvas_rect.bottom() {
        let mut x = start_x;
        while x < canvas_rect.right() {
            painter.circle_filled(Pos2::new(x, y), radius, dot_color);
            x += step;
        }
        y += step;
    }
}

trait RemEuclidF32 {
    fn rem_euclid_f32(self, rhs: f32) -> f32;
}

impl RemEuclidF32 for f32 {
    fn rem_euclid_f32(self, rhs: f32) -> f32 {
        let r = self % rhs;
        if r < 0.0 { r + rhs } else { r }
    }
}

fn paint_connections<H: ::std::hash::BuildHasher>(
    painter: &egui::Painter,
    canvas_rect: Rect,
    canvas_origin: Vec2,
    view: &CanvasView,
    graph: &Graph,
    layout: &HashMap<NodeId, Vec2, H>,
) {
    for conn in &graph.connections {
        let (src_id, src_idx) = conn.from;
        let (dst_id, dst_idx) = conn.to;
        let (Some(src_node), Some(dst_node)) = (graph.node(src_id), graph.node(dst_id)) else {
            continue;
        };
        let (Some(&src_pos), Some(&dst_pos)) = (layout.get(&src_id), layout.get(&dst_id)) else {
            continue;
        };
        let src_socket_center = if src_node.outputs.get(src_idx).is_some() {
            socket_center(src_node, src_idx, SocketKind::Output, src_pos)
        } else if src_node.inputs.get(src_idx).is_some() {
            socket_center(src_node, src_idx, SocketKind::Input, src_pos)
        } else {
            continue;
        };
        let dst_socket_center = if dst_node.inputs.get(dst_idx).is_some() {
            socket_center(dst_node, dst_idx, SocketKind::Input, dst_pos)
        } else if dst_node.outputs.get(dst_idx).is_some() {
            socket_center(dst_node, dst_idx, SocketKind::Output, dst_pos)
        } else {
            continue;
        };

        let a = view.canvas_to_screen(canvas_origin, src_socket_center);
        let b = view.canvas_to_screen(canvas_origin, dst_socket_center);
        let a_pos = Pos2::new(a.x, a.y);
        let b_pos = Pos2::new(b.x, b.y);

        let bb = Rect::from_two_pos(a_pos, b_pos).expand(20.0);
        if !bb.intersects(canvas_rect) {
            continue;
        }

        let handle_len = ((b.x - a.x).abs() * 0.5).clamp(30.0, 120.0);
        let dir = if b.x >= a.x { 1.0 } else { -1.0 };
        let c1 = Pos2::new(a.x + handle_len * dir, a.y);
        let c2 = Pos2::new(b.x - handle_len * dir, b.y);

        let color = wire_color(src_node, src_idx);
        let stroke = Stroke::new((1.5 * view.zoom).max(1.0), color);
        painter.add(CubicBezierShape {
            points: [a_pos, c1, c2, b_pos],
            closed: false,
            fill: Color32::TRANSPARENT,
            stroke: stroke.into(),
        });
    }
}

fn wire_color(src_node: &Node, src_idx: usize) -> Color32 {
    if let Some(s) = src_node.outputs.get(src_idx) {
        return socket_color(s.ty);
    }
    if let Some(s) = src_node.inputs.get(src_idx) {
        return socket_color(s.ty);
    }
    Color32::from_gray(180)
}

fn paint_node(
    painter: &egui::Painter,
    canvas_origin: Vec2,
    view: &CanvasView,
    node: &Node,
    origin_canvas: Vec2,
    palette: CanvasPalette,
    is_selected: bool,
) {
    let node_rect_canvas = node_rect(node, origin_canvas);
    let min = view.canvas_to_screen(
        canvas_origin,
        Vec2::new(node_rect_canvas.min.x, node_rect_canvas.min.y),
    );
    let max = view.canvas_to_screen(
        canvas_origin,
        Vec2::new(node_rect_canvas.max.x, node_rect_canvas.max.y),
    );
    let screen_rect = Rect::from_min_max(Pos2::new(min.x, min.y), Pos2::new(max.x, max.y));
    let corner = CornerRadius::same((NODE_CORNER * view.zoom).max(2.0).to_u8().unwrap_or(2));

    painter.rect_filled(screen_rect, corner, node_body_fill());
    let outline = if is_selected {
        Stroke::new((2.0 * view.zoom).max(1.5), palette.accent)
    } else {
        Stroke::new((1.0 * view.zoom).max(1.0), node_outline())
    };
    painter.rect_stroke(screen_rect, corner, outline, StrokeKind::Middle);

    let header_h = node_header_height(node) * view.zoom;
    let header_rect =
        Rect::from_min_size(screen_rect.min, Vec2::new(screen_rect.width(), header_h));
    painter.rect_filled(header_rect, corner, header_color(node, palette));
    let separator_y = header_rect.bottom();
    painter.line_segment(
        [
            Pos2::new(screen_rect.left(), separator_y),
            Pos2::new(screen_rect.right(), separator_y),
        ],
        Stroke::new(1.0, node_outline()),
    );

    if view.zoom >= 0.5 {
        let label_pos = header_rect.center();
        painter.text(
            label_pos,
            egui::Align2::CENTER_CENTER,
            &node.display_name,
            egui::FontId::proportional((12.0 * view.zoom).clamp(9.0, 20.0)),
            palette.heading,
        );
    }

    paint_sockets(
        painter,
        canvas_origin,
        view,
        node,
        origin_canvas,
        SocketKind::Input,
    );
    paint_sockets(
        painter,
        canvas_origin,
        view,
        node,
        origin_canvas,
        SocketKind::Output,
    );
}

fn header_color(node: &Node, palette: CanvasPalette) -> Color32 {
    if let Some([r, g, b]) = node.accent_rgb {
        return Color32::from_rgb(r, g, b);
    }
    let [r, g, b, _] = palette.accent.to_array();
    Color32::from_rgb(
        (f32::from(r) * 0.6).to_u8().unwrap_or(u8::MAX),
        (f32::from(g) * 0.6).to_u8().unwrap_or(u8::MAX),
        (f32::from(b) * 0.6).to_u8().unwrap_or(u8::MAX),
    )
}

fn paint_value_boxes(
    painter: &egui::Painter,
    canvas_origin: Vec2,
    view: &CanvasView,
    node: &Node,
    origin_canvas: Vec2,
) {
    let font_size = (11.0 * view.zoom).clamp(8.0, 16.0);
    let font = egui::FontId::proportional(font_size);
    let corner = CornerRadius::same((VALUE_BOX_CORNER * view.zoom).max(2.0).to_u8().unwrap_or(2));
    let value_dot_r = (SOCKET_RADIUS * view.zoom).max(3.5);

    for (i, sock) in node.inputs.iter().enumerate() {
        let Some(text) = sock.authored_value.as_deref() else {
            continue;
        };

        let socket_center_canvas = socket_center(node, i, SocketKind::Input, origin_canvas);
        let socket_center_screen = view.canvas_to_screen(canvas_origin, socket_center_canvas);
        let socket_pos = Pos2::new(socket_center_screen.x, socket_center_screen.y);

        let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), value_box_text());
        let text_size = galley.size();

        let hpad = VALUE_BOX_HPAD * view.zoom;
        let vpad = VALUE_BOX_VPAD * view.zoom;
        let box_w = text_size.x + 2.0 * hpad;
        let box_h = text_size.y + 2.0 * vpad;

        let gap = VALUE_BOX_GAP * view.zoom;
        let box_right = socket_pos.x - gap;
        let box_min = Pos2::new(box_right - box_w, socket_pos.y - box_h * 0.5);
        let box_max = Pos2::new(box_right, socket_pos.y + box_h * 0.5);
        let box_rect = Rect::from_min_max(box_min, box_max);

        painter.rect_filled(box_rect, corner, value_box_fill());
        painter.rect_stroke(
            box_rect,
            corner,
            Stroke::new(1.0, node_outline()),
            StrokeKind::Middle,
        );
        painter.galley(
            Pos2::new(
                box_rect.center().x - text_size.x * 0.5,
                box_rect.center().y - text_size.y * 0.5,
            ),
            galley,
            value_box_text(),
        );

        let dot_center = Pos2::new(box_right, socket_pos.y);
        let dot_color = socket_color(sock.ty);
        painter.circle_filled(dot_center, value_dot_r, dot_color);
        painter.circle_stroke(dot_center, value_dot_r, Stroke::new(1.0, node_outline()));

        let handle = ((socket_pos.x - dot_center.x).abs() * 0.4).clamp(8.0, 40.0);
        let c1 = Pos2::new(dot_center.x + handle, dot_center.y);
        let c2 = Pos2::new(socket_pos.x - handle, socket_pos.y);
        painter.add(CubicBezierShape {
            points: [dot_center, c1, c2, socket_pos],
            closed: false,
            fill: Color32::TRANSPARENT,
            stroke: Stroke::new((1.5 * view.zoom).max(1.0), dot_color).into(),
        });
    }
}

fn paint_sockets(
    painter: &egui::Painter,
    canvas_origin: Vec2,
    view: &CanvasView,
    node: &Node,
    origin_canvas: Vec2,
    kind: SocketKind,
) {
    let sockets = match kind {
        SocketKind::Input => &node.inputs,
        SocketKind::Output => &node.outputs,
    };
    for (i, sock) in sockets.iter().enumerate() {
        let center_canvas = socket_center(node, i, kind, origin_canvas);
        let center_screen = view.canvas_to_screen(canvas_origin, center_canvas);
        let center = Pos2::new(center_screen.x, center_screen.y);
        let r = (SOCKET_RADIUS * view.zoom).max(3.5);
        painter.circle_filled(center, r, socket_color(sock.ty));
        painter.circle_stroke(center, r, Stroke::new(1.0, node_outline()));

        if view.zoom >= 0.65 {
            let font = egui::FontId::proportional((11.0 * view.zoom).clamp(8.0, 16.0));
            let label = match node.role {
                NodeRole::Standard => match kind {
                    SocketKind::Input => Some((
                        Pos2::new(center.x + r + 4.0, center.y),
                        egui::Align2::LEFT_CENTER,
                    )),
                    SocketKind::Output => Some((
                        Pos2::new(center.x - r - 4.0, center.y),
                        egui::Align2::RIGHT_CENTER,
                    )),
                },
                NodeRole::SourceAggregate => {
                    if kind == SocketKind::Input {
                        Some((
                            Pos2::new(center.x + r + 4.0, center.y),
                            egui::Align2::LEFT_CENTER,
                        ))
                    } else {
                        None
                    }
                }
                NodeRole::SinkAggregate => Some((
                    Pos2::new(center.x + r + 4.0, center.y),
                    egui::Align2::LEFT_CENTER,
                )),
            };
            if let Some((label_pos, align)) = label {
                painter.text(label_pos, align, &sock.name, font, Color32::from_gray(220));
            }
        }
    }
}
