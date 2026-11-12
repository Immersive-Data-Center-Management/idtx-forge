//! Graph canvas theme constants + socket-type colour palette.
//!
//! Layout metrics (node sizes, gaps, radii) live here as plain constants.
//! Colours are read from the central [`crate::theme`] (`canvas` sub-palette)
//! so the editor keeps a single colour-definition point.

use egui::Color32;
use num_traits::ToPrimitive;

use super::graph::SocketType;

// ── Node metrics ──────────────────────────────────────────────────────────────

pub const NODE_WIDTH: f32 = 190.0;
pub const HEADER_HEIGHT: f32 = 24.0;
pub const SOCKET_ROW_HEIGHT: f32 = 18.0;
pub const SOCKET_RADIUS: f32 = 5.0;
pub const NODE_CORNER: f32 = 6.0;
pub const COLUMN_GAP: f32 = 60.0;
pub const ROW_GAP: f32 = 24.0;
pub const BOTTOM_PADDING: f32 = 8.0;

#[must_use]
pub fn node_height_for_socket_rows(rows: usize) -> f32 {
    let rows = rows.max(1).to_f32().unwrap_or(f32::MAX);
    HEADER_HEIGHT + rows * SOCKET_ROW_HEIGHT + BOTTOM_PADDING
}

// ── Socket colour palette (central theme) ───────────────────────────────────────

#[must_use]
pub fn socket_color(ty: SocketType) -> Color32 {
    let c = &crate::theme::current().canvas;
    match ty {
        SocketType::String => c.socket_string,
        SocketType::Float => c.socket_float,
        SocketType::Float3OrColor => c.socket_float3,
        SocketType::Path => c.socket_path,
        SocketType::Other => c.socket_other,
    }
}

#[must_use]
pub fn node_body_fill() -> Color32 {
    crate::theme::current().canvas.node_body_fill
}
#[must_use]
pub fn node_outline() -> Color32 {
    crate::theme::current().canvas.node_outline
}

// ── Authored value-box metrics ────────────────────────────────────────────────

pub const VALUE_BOX_GAP: f32 = 32.0;
pub const VALUE_BOX_CORNER: f32 = 5.0;
pub const VALUE_BOX_VPAD: f32 = 3.0;
pub const VALUE_BOX_HPAD: f32 = 6.0;

#[must_use]
pub fn value_box_fill() -> Color32 {
    crate::theme::current().canvas.value_box_fill
}
#[must_use]
pub fn value_box_text() -> Color32 {
    crate::theme::current().canvas.value_box_text
}

// ── Aggregate (narrow source/sink) rectangle metrics ──────────────────────────

pub const AGGREGATE_WIDTH: f32 = 100.0;
pub const AGGREGATE_HEADER_HEIGHT: f32 = 32.0;
pub const AGGREGATE_BOTTOM_PADDING: f32 = 12.0;

#[must_use]
pub fn aggregate_height_for_socket_rows(rows: usize) -> f32 {
    // its inlikely that this value will ever reach f32 max, but to be safe here
    let rows = rows.max(1).to_f32().unwrap_or(f32::MAX);
    AGGREGATE_HEADER_HEIGHT + rows * SOCKET_ROW_HEIGHT + AGGREGATE_BOTTOM_PADDING
}
