//! Transform gizmo overlay for the 3-D viewport.
//!
//! Drawn as flat 2-D shapes on top of the wgpu scene texture using the egui
//! `Painter` API.  Hit-testing and drag interaction happen in screen space so
//! no GPU picking is required.
//!
//! # Coordinate systems
//!
//! All world-space positions passed into this module must be in **render world
//! space** (post `projection_basis`, Y-up).  The `view_proj` from
//! `StudioSession::camera_view_proj()` is already in render world space.
//!
//! Transform authoring is always in **USD local space** (parent-relative).
//! `compute_dragged_matrix` returns a new local matrix that callers must
//! decompose to TRS and author via `StudioSession::author_transform`.

use egui::{Color32, Painter, Pos2, Rect, Stroke};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
use idtx_editor_ui::viewport_toolbar::GizmoSpace;

/// Light blue highlight for the actively-dragged axis or arc (as per spec).
pub const ACTIVE_COLOR: Color32 = Color32::from_rgb(100, 200, 255);

const AXIS_COLORS: [Color32; 3] = [
    Color32::from_rgb(230, 80, 80),  // X — red
    Color32::from_rgb(80, 210, 100), // Y — green
    Color32::from_rgb(80, 130, 240), // Z — blue
];

const CONE_PICK_R: f32 = 14.0;
const SPHERE_PICK_R: f32 = 14.0;

// ── Handle types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GizmoHandle {
    TranslateX,
    TranslateY,
    TranslateZ,
    RotateX,
    RotateY,
    RotateZ,
}

impl GizmoHandle {
    fn axis_index(self) -> usize {
        match self {
            Self::TranslateX | Self::RotateX => 0,
            Self::TranslateY | Self::RotateY => 1,
            Self::TranslateZ | Self::RotateZ => 2,
        }
    }
    fn is_translate(self) -> bool {
        matches!(self, Self::TranslateX | Self::TranslateY | Self::TranslateZ)
    }
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

/// Per-frame geometry snapshot used for hit-testing.
#[derive(Debug, Clone)]
pub struct GizmoSnapshot {
    pub selected_path: String,
    /// USD local matrix at snapshot time (column-major glam convention).
    pub current_local_mat: Mat4,
    /// Parent world matrix in render world space.
    pub parent_world_mat: Mat4,
    /// Selection origin in render world space.
    pub origin_world: Vec3,
    /// View-projection matrix (render world space).
    pub view_proj: Mat4,
    pub panel_rect: Rect,
    /// World-space length of one gizmo axis shaft.
    pub axis_len: f32,
    pub space: GizmoSpace,
    /// The three gizmo axes in render world space this frame.
    pub world_axes: [Vec3; 3],
    pub handles: [HandleHit; 6],
}

#[derive(Debug, Clone, Copy)]
pub struct HandleHit {
    pub kind: GizmoHandle,
    pub center: Pos2,
    pub radius: f32,
}

impl GizmoSnapshot {
    pub fn pick(&self, pos: Pos2) -> Option<GizmoHandle> {
        let mut best: Option<(f32, GizmoHandle)> = None;
        for h in &self.handles {
            let d = pos.distance(h.center);
            if d <= h.radius && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, h.kind));
            }
        }
        best.map(|(_, k)| k)
    }
}

// ── Drag state ────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct GizmoState {
    pub snapshot: Option<GizmoSnapshot>,
    pub active: Option<ActiveDrag>,
}

#[derive(Debug, Clone)]
pub struct ActiveDrag {
    pub path: String,
    pub handle: GizmoHandle,
    /// USD local matrix at drag-start (column-major glam convention).
    pub before: Mat4,
    pub snapshot: GizmoSnapshot,
    pub start_pos: Pos2,
}

// ── Drag → new local matrix ────────────────────────────────────────────────────

/// Compute the updated **USD local** matrix for `active` given cursor `pos`.
///
/// Returns a column-major glam `Mat4` in USD local space (parent-relative).
/// Callers must decompose this to TRS and author via `session.author_transform`.
pub fn compute_dragged_matrix(active: &ActiveDrag, pos: Pos2) -> Mat4 {
    let snap = &active.snapshot;
    let axis_idx = active.handle.axis_index();
    let world_axis = snap.world_axes[axis_idx].normalize_or_zero();
    if world_axis.length_squared() < 1.0e-6 {
        return active.before;
    }

    let parent_inv = snap.parent_world_mat.inverse();
    let (scale, rot, trans) = active.before.to_scale_rotation_translation();

    if active.handle.is_translate() {
        let o = project_world(&snap.view_proj, snap.panel_rect, snap.origin_world);
        let t = project_world(
            &snap.view_proj,
            snap.panel_rect,
            snap.origin_world + world_axis * snap.axis_len,
        );
        let (Some(o), Some(t)) = (o, t) else {
            return active.before;
        };
        let axis_screen = Vec2::new(t.x - o.x, t.y - o.y);
        let axis_len_screen_sq = axis_screen.length_squared();
        if axis_len_screen_sq < 1.0 {
            return active.before;
        }
        let start_v = Vec2::new(active.start_pos.x - o.x, active.start_pos.y - o.y);
        let now_v = Vec2::new(pos.x - o.x, pos.y - o.y);
        let start_t = start_v.dot(axis_screen) / axis_len_screen_sq;
        let now_t = now_v.dot(axis_screen) / axis_len_screen_sq;
        let dt_world_units = (now_t - start_t) * snap.axis_len;
        let world_delta = world_axis * dt_world_units;
        // Transform the render-world delta into USD local space via parent inverse.
        let local_delta = parent_inv.transform_vector3(world_delta);
        let new_trans = trans + local_delta;
        Mat4::from_scale_rotation_translation(scale, rot, new_trans)
    } else {
        let Some(o) = project_world(&snap.view_proj, snap.panel_rect, snap.origin_world) else {
            return active.before;
        };
        let a0 = (active.start_pos.y - o.y).atan2(active.start_pos.x - o.x);
        let a1 = (pos.y - o.y).atan2(pos.x - o.x);
        let mut d_angle = a1 - a0;
        let axis_clip = snap.view_proj * Vec4::new(world_axis.x, world_axis.y, world_axis.z, 0.0);
        if axis_clip.z < 0.0 {
            d_angle = -d_angle;
        }
        let new_rot = match snap.space {
            GizmoSpace::Local => {
                let unit = match axis_idx {
                    0 => Vec3::X,
                    1 => Vec3::Y,
                    _ => Vec3::Z,
                };
                rot * Quat::from_axis_angle(unit, d_angle)
            }
            GizmoSpace::World => {
                let (_ps, parent_rot, _pt) = snap.parent_world_mat.to_scale_rotation_translation();
                let local_axis = parent_rot
                    .inverse()
                    .mul_vec3(world_axis)
                    .normalize_or_zero();
                if local_axis.length_squared() < 1.0e-6 {
                    return active.before;
                }
                Quat::from_axis_angle(local_axis, d_angle) * rot
            }
        };
        Mat4::from_scale_rotation_translation(scale, new_rot, trans)
    }
}

// ── Drawing ────────────────────────────────────────────────────────────────────

/// The three gizmo axes in render world space for the given [`GizmoSpace`].
///
/// - [`GizmoSpace::World`] → the world basis `[X, Y, Z]`.
/// - [`GizmoSpace::Local`] → the object's normalized 3×3 columns (falling back
///   to the world basis for any degenerate column).
///
/// Shared by the interactive gizmo ([`draw_combined_gizmo`]) and the read-only
/// axis gizmo ([`draw_axes_only`]) so both draw identically oriented axes.
fn gizmo_world_axes(world_transform: Mat4, space: GizmoSpace) -> [Vec3; 3] {
    match space {
        GizmoSpace::World => [Vec3::X, Vec3::Y, Vec3::Z],
        GizmoSpace::Local => {
            let m3 = Mat3::from_mat4(world_transform);
            let a = m3.x_axis.try_normalize().unwrap_or(Vec3::X);
            let b = m3.y_axis.try_normalize().unwrap_or(Vec3::Y);
            let c = m3.z_axis.try_normalize().unwrap_or(Vec3::Z);
            [a, b, c]
        }
    }
}

/// Draw the combined translate+rotate gizmo.
///
/// `world_transform` and `parent_world_transform` must be in **render world space**.
/// `local_mat` is the USD local matrix (column-major glam convention).
/// Returns a [`GizmoSnapshot`] for hit-testing, or `None` if the origin is
/// behind the camera.
#[allow(clippy::too_many_arguments)]
pub fn draw_combined_gizmo(
    painter: &Painter,
    panel_rect: Rect,
    view_proj: Mat4,
    camera_radius: f32,
    world_transform: Mat4,
    parent_world_transform: Mat4,
    local_mat: Mat4,
    selected_path: &str,
    active_handle: Option<GizmoHandle>,
    space: GizmoSpace,
) -> Option<GizmoSnapshot> {
    let origin = world_transform.transform_point3(Vec3::ZERO);
    let o2 = project_world(&view_proj, panel_rect, origin)?;
    let axis_len = camera_radius * 0.10;

    let world_axes = gizmo_world_axes(world_transform, space);

    let mut handles: [HandleHit; 6] = [HandleHit {
        kind: GizmoHandle::TranslateX,
        center: o2,
        radius: 0.0,
    }; 6];

    let translate_kinds = [
        GizmoHandle::TranslateX,
        GizmoHandle::TranslateY,
        GizmoHandle::TranslateZ,
    ];
    let rotate_kinds = [
        GizmoHandle::RotateX,
        GizmoHandle::RotateY,
        GizmoHandle::RotateZ,
    ];

    for i in 0..3 {
        let is_active_translate = active_handle == Some(translate_kinds[i]);
        let is_active_rotate = active_handle == Some(rotate_kinds[i]);
        let base_color = AXIS_COLORS[i];
        let axis_color = if is_active_translate || is_active_rotate {
            ACTIVE_COLOR
        } else {
            base_color
        };
        let arc_color = if is_active_rotate {
            ACTIVE_COLOR
        } else {
            base_color
        };

        let axis = world_axes[i];

        // Translation shaft + cone head.
        let tip_world = origin + axis * axis_len;
        if let Some(tip2) = project_world(&view_proj, panel_rect, tip_world) {
            painter.line_segment([o2, tip2], Stroke::new(2.5, axis_color));
            draw_cone_head(painter, o2, tip2, axis_color);
            handles[i] = HandleHit {
                kind: translate_kinds[i],
                center: tip2,
                radius: CONE_PICK_R,
            };
        }

        // 90° rotation arc + sphere grabber.
        let (a, b) = match i {
            0 => (world_axes[1], world_axes[2]),
            1 => (world_axes[2], world_axes[0]),
            _ => (world_axes[0], world_axes[1]),
        };
        let arc_radius = axis_len * 0.75;
        let n = 24;
        let mut prev: Option<Pos2> = None;
        for k in 0..=n {
            let t = (k as f32 / n as f32) * std::f32::consts::FRAC_PI_2;
            let world = origin + (a * t.cos() + b * t.sin()) * arc_radius;
            if let Some(p) = project_world(&view_proj, panel_rect, world) {
                if let Some(pp) = prev {
                    painter.line_segment([pp, p], Stroke::new(1.75, arc_color));
                }
                prev = Some(p);
            }
        }
        let mid_t = std::f32::consts::FRAC_PI_4;
        let grab_world = origin + (a * mid_t.cos() + b * mid_t.sin()) * arc_radius;
        if let Some(g2) = project_world(&view_proj, panel_rect, grab_world) {
            painter.circle_filled(g2, 8.0, arc_color);
            painter.circle_stroke(g2, 8.0, Stroke::new(1.0, Color32::BLACK));
            handles[i + 3] = HandleHit {
                kind: rotate_kinds[i],
                center: g2,
                radius: SPHERE_PICK_R,
            };
        }
    }

    // Central pivot dot.
    painter.circle_filled(o2, 4.0, Color32::from_rgb(240, 240, 240));
    painter.circle_stroke(o2, 4.0, Stroke::new(1.0, Color32::BLACK));

    Some(GizmoSnapshot {
        selected_path: selected_path.to_owned(),
        current_local_mat: local_mat,
        parent_world_mat: parent_world_transform,
        origin_world: origin,
        view_proj,
        panel_rect,
        axis_len,
        space,
        world_axes,
        handles,
    })
}

/// Screen-space endpoints of the three gizmo axis lines.
///
/// Returns `[X, Y, Z]`; each entry is `Some([origin_screen, tip_screen])`, or
/// `None` when that axis endpoint (or the shared origin) projects behind the
/// camera. Pure geometry — shared drawing input for the read-only gizmo and
/// directly unit-testable.
#[cfg(not(feature = "authoring"))]
fn axis_line_segments(
    view_proj: Mat4,
    panel_rect: Rect,
    camera_radius: f32,
    world_transform: Mat4,
    space: GizmoSpace,
) -> [Option<[Pos2; 2]>; 3] {
    let mut out: [Option<[Pos2; 2]>; 3] = [None, None, None];
    let origin = world_transform.transform_point3(Vec3::ZERO);
    let Some(o2) = project_world(&view_proj, panel_rect, origin) else {
        return out;
    };
    let axis_len = camera_radius * 0.10;
    let world_axes = gizmo_world_axes(world_transform, space);
    for i in 0..3 {
        let tip_world = origin + world_axes[i] * axis_len;
        if let Some(tip2) = project_world(&view_proj, panel_rect, tip_world) {
            out[i] = Some([o2, tip2]);
        }
    }
    out
}

/// Draw a **display-only** axis gizmo: just the three coloured axis lines
/// (X red, Y green, Z blue) through the selection origin.
///
/// No arrow heads, no rotation arcs, no grabbers, and no hit-testing. Read-only
/// builds (compiled without the `authoring` feature) use this so the selected
/// prim's position and orientation are visible without offering any drag
/// interaction. Axes match those drawn by [`draw_combined_gizmo`].
///
/// `world_transform` must be in **render world space** (see module docs).
#[cfg(not(feature = "authoring"))]
pub fn draw_axes_only(
    painter: &Painter,
    panel_rect: Rect,
    view_proj: Mat4,
    camera_radius: f32,
    world_transform: Mat4,
    space: GizmoSpace,
) {
    let segments = axis_line_segments(view_proj, panel_rect, camera_radius, world_transform, space);
    for (i, seg) in segments.iter().enumerate() {
        if let Some([o2, tip2]) = *seg {
            painter.line_segment([o2, tip2], Stroke::new(2.5, AXIS_COLORS[i]));
        }
    }
}

fn draw_cone_head(painter: &Painter, base: Pos2, tip: Pos2, color: Color32) {
    let dir = tip - base;
    let len = dir.length();
    if len < 1.0 {
        return;
    }
    let d = dir / len;
    let n = egui::vec2(-d.y, d.x);
    let cone_len = 14.0;
    let cone_half_w = 6.0;
    let base_of_cone = tip - d * cone_len;
    let p1 = base_of_cone + n * cone_half_w;
    let p2 = base_of_cone - n * cone_half_w;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, p1, p2],
        color,
        Stroke::new(1.0, Color32::BLACK),
    ));
}

/// Project a render-world-space point through `vp` into the given screen [`Rect`].
/// Returns `None` when the point is behind the camera.
fn project_world(vp: &Mat4, rect: Rect, world: Vec3) -> Option<Pos2> {
    let clip = *vp * Vec4::new(world.x, world.y, world.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip / clip.w;
    let x = rect.left() + (ndc.x * 0.5 + 0.5) * rect.width();
    let y = rect.top() + (1.0 - (ndc.y * 0.5 + 0.5)) * rect.height();
    Some(Pos2::new(x, y))
}

// ── USD matrix conversion helpers ─────────────────────────────────────────────

/// Convert a USD row-major `[[f32;4];4]` to a column-major glam `Mat4`.
///
/// USD row-vector convention (`p' = p · M`) and glam column-vector convention
/// (`p' = M · p`) are **transposes** of each other.  The transpose maps as:
/// **glam column j = USD row j**.  In particular:
/// - `w_axis` (glam col 3) = USD row 3 = `[tx, ty, tz, 1]`  ← translation lives here
pub fn mat_from_usd(m: [[f32; 4]; 4]) -> Mat4 {
    Mat4::from_cols(
        Vec4::from_array(m[0]), // glam col 0 (x_axis) = USD row 0
        Vec4::from_array(m[1]), // glam col 1 (y_axis) = USD row 1
        Vec4::from_array(m[2]), // glam col 2 (z_axis) = USD row 2
        Vec4::from_array(m[3]), // glam col 3 (w_axis) = USD row 3 = [tx, ty, tz, 1]
    )
}

/// Decompose a USD row-major `[[f32;4];4]` into `(translation, rotation_xyz_deg, scale)`.
///
/// Matches the convention expected by `StudioSession::author_transform` and
/// `idtx_usd::TransformValues`.
pub fn decompose_usd_trs(m: &[[f32; 4]; 4]) -> ([f64; 3], [f64; 3], [f64; 3]) {
    // Translation — last row.
    let t = [m[3][0] as f64, m[3][1] as f64, m[3][2] as f64];

    // Scale — magnitude of each row of the upper-left 3×3.
    let row_len =
        |i: usize| -> f32 { (m[i][0] * m[i][0] + m[i][1] * m[i][1] + m[i][2] * m[i][2]).sqrt() };
    let sx = row_len(0).max(f32::EPSILON);
    let sy = row_len(1).max(f32::EPSILON);
    let sz = row_len(2).max(f32::EPSILON);
    let s = [sx as f64, sy as f64, sz as f64];

    // Rotation — normalize rows, extract XYZ Euler degrees.
    let r = [
        [m[0][0] / sx, m[0][1] / sx, m[0][2] / sx],
        [m[1][0] / sy, m[1][1] / sy, m[1][2] / sy],
        [m[2][0] / sz, m[2][1] / sz, m[2][2] / sz],
    ];
    let ry_rad = (-r[0][2]).clamp(-1., 1.).asin();
    let cos_ry = ry_rad.cos();
    let (rx_rad, rz_rad) = if cos_ry.abs() > 1e-6 {
        let rx = (r[1][2] / cos_ry).atan2(r[2][2] / cos_ry);
        let rz = (r[0][1] / cos_ry).atan2(r[0][0] / cos_ry);
        (rx, rz)
    } else {
        let rx = r[1][0].atan2(r[1][1]);
        (rx, 0.0_f32)
    };
    let rot = [
        rx_rad.to_degrees() as f64,
        ry_rad.to_degrees() as f64,
        rz_rad.to_degrees() as f64,
    ];
    (t, rot, s)
}

/// Convert a column-major glam `Mat4` back to a USD row-major `[[f32;4];4]`.
///
/// Inverse of [`mat_from_usd`]: USD row j = glam column j.
/// In particular USD row 3 = `w_axis = [tx, ty, tz, 1]`.
pub fn mat_to_usd(m: Mat4) -> [[f32; 4]; 4] {
    [
        m.x_axis.to_array(), // USD row 0 = glam col 0 (x_axis)
        m.y_axis.to_array(), // USD row 1 = glam col 1 (y_axis)
        m.z_axis.to_array(), // USD row 2 = glam col 2 (z_axis)
        m.w_axis.to_array(), // USD row 3 = glam col 3 (w_axis) = [tx, ty, tz, 1]
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    /// A USD translation-only matrix: identity 3×3, [tx,ty,tz] in last row.
    fn usd_translation(tx: f32, ty: f32, tz: f32) -> [[f32; 4]; 4] {
        [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [tx, ty, tz, 1.],
        ]
    }

    #[test]
    fn mat_from_usd_preserves_translation() {
        let m = mat_from_usd(usd_translation(1., 2., 3.));
        // transform_point3(ZERO) must give the translation vector.
        let p = m.transform_point3(Vec3::ZERO);
        assert!((p.x - 1.).abs() < 1e-5, "x: {}", p.x);
        assert!((p.y - 2.).abs() < 1e-5, "y: {}", p.y);
        assert!((p.z - 3.).abs() < 1e-5, "z: {}", p.z);
    }

    #[test]
    fn mat_round_trip() {
        // Any USD matrix survives mat_from_usd → mat_to_usd unchanged.
        let usd = usd_translation(4., 5., 6.);
        let recovered = mat_to_usd(mat_from_usd(usd));
        for i in 0..4 {
            for j in 0..4 {
                assert!(
                    (recovered[i][j] - usd[i][j]).abs() < 1e-6,
                    "mismatch at [{i}][{j}]: {} vs {}",
                    recovered[i][j],
                    usd[i][j]
                );
            }
        }
    }

    #[test]
    fn decompose_after_roundtrip() {
        // Build a glam TRS, convert to USD, decompose — must recover TRS.
        let scale = Vec3::new(2., 3., 4.);
        let rot = glam::Quat::from_rotation_y(45_f32.to_radians());
        let trans = Vec3::new(10., 20., 30.);
        let glam_mat = Mat4::from_scale_rotation_translation(scale, rot, trans);
        let usd_mat = mat_to_usd(glam_mat);
        let (t, r, s) = decompose_usd_trs(&usd_mat);
        assert!((t[0] - 10.).abs() < 1e-4, "tx: {}", t[0]);
        assert!((t[1] - 20.).abs() < 1e-4, "ty: {}", t[1]);
        assert!((t[2] - 30.).abs() < 1e-4, "tz: {}", t[2]);
        assert!((s[0] - 2.).abs() < 1e-4, "sx: {}", s[0]);
        assert!((s[1] - 3.).abs() < 1e-4, "sy: {}", s[1]);
        assert!((s[2] - 4.).abs() < 1e-4, "sz: {}", s[2]);
        assert!((r[1] - 45.).abs() < 1e-3, "ry: {}", r[1]);
    }

    #[test]
    fn world_axes_world_space_ignores_transform() {
        // World space always yields the world basis, regardless of the object's
        // own rotation/scale.
        let m = Mat4::from_scale_rotation_translation(
            Vec3::new(2., 3., 4.),
            glam::Quat::from_rotation_z(1.1),
            Vec3::new(5., 6., 7.),
        );
        let axes = gizmo_world_axes(m, GizmoSpace::World);
        assert!((axes[0] - Vec3::X).length() < 1e-6);
        assert!((axes[1] - Vec3::Y).length() < 1e-6);
        assert!((axes[2] - Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn world_axes_local_space_uses_normalized_columns() {
        // Local space follows the object's basis: a +90° rotation about Z maps
        // local X→world Y and local Y→world -X. Non-uniform scale must not leak
        // in — the returned axes are unit length.
        let m = Mat4::from_scale_rotation_translation(
            Vec3::new(2., 5., 1.),
            glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            Vec3::new(9., -3., 0.),
        );
        let axes = gizmo_world_axes(m, GizmoSpace::Local);
        assert!((axes[0] - Vec3::Y).length() < 1e-5, "x-axis: {:?}", axes[0]);
        assert!((axes[1] + Vec3::X).length() < 1e-5, "y-axis: {:?}", axes[1]);
        assert!((axes[2] - Vec3::Z).length() < 1e-5, "z-axis: {:?}", axes[2]);
        for a in axes {
            assert!((a.length() - 1.0).abs() < 1e-5, "not unit length: {a:?}");
        }
    }

    #[cfg(not(feature = "authoring"))]
    #[test]
    fn axis_segments_share_origin_and_have_distinct_tips() {
        // The read-only gizmo draws three lines from a common projected origin.
        // With the object in front of the camera all three project, share the
        // origin endpoint, and fan out to three distinct tips.
        let proj = Mat4::perspective_rh(60_f32.to_radians(), 4.0 / 3.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::new(3., 3., 3.), Vec3::ZERO, Vec3::Y);
        let vp = proj * view;
        let rect = Rect::from_min_size(Pos2::new(0., 0.), egui::vec2(800., 600.));

        let segs = axis_line_segments(vp, rect, 10.0, Mat4::IDENTITY, GizmoSpace::World);
        let origins: Vec<Pos2> = segs.iter().map(|s| s.expect("axis in front")[0]).collect();
        let tips: Vec<Pos2> = segs.iter().map(|s| s.unwrap()[1]).collect();

        // Shared origin.
        assert!(origins[0].distance(origins[1]) < 1e-3);
        assert!(origins[0].distance(origins[2]) < 1e-3);
        // Distinct tips, each offset from the origin.
        for tip in &tips {
            assert!(tip.distance(origins[0]) > 1.0, "tip on origin: {tip:?}");
        }
        assert!(tips[0].distance(tips[1]) > 1.0);
        assert!(tips[0].distance(tips[2]) > 1.0);
        assert!(tips[1].distance(tips[2]) > 1.0);
    }

    #[cfg(not(feature = "authoring"))]
    #[test]
    fn axis_segments_none_when_behind_camera() {
        // Origin behind the camera → nothing projects, all three are None.
        let proj = Mat4::perspective_rh(60_f32.to_radians(), 1.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::new(0., 0., 5.), Vec3::new(0., 0., 6.), Vec3::Y);
        let vp = proj * view;
        let rect = Rect::from_min_size(Pos2::new(0., 0.), egui::vec2(800., 600.));

        let segs = axis_line_segments(vp, rect, 10.0, Mat4::IDENTITY, GizmoSpace::World);
        assert!(segs.iter().all(|s| s.is_none()));
    }
}
