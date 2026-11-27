//! Light-blue collision-shape wireframes for the selected prim (editor only).
//!
//! When a prim with the `IdtxCollisionAPI` schema is selected, the session draws
//! a wireframe of its authored collision shape over the prim — the same geometry
//! the picking narrow-phase ([`idtx_scene::picking`]) tests against: analytic
//! `sphere`/`cube`/`cylinder`/`capsule` sized from the prim's local bounds, the
//! `hull` triangle soup, or the prim's own mesh for `shape = mesh`.
//!
//! All builders are pure [`OverlayLine`] producers (no wgpu), unit-tested
//! directly. They run once per frame for the single selected prim, so this is
//! not a hot path. **This module is compiled only with the `editor-overlay`
//! feature**, so the packaged `idtx-runtime` player never includes it.

use std::collections::HashSet;

use glam::{Mat4, Vec3};

use crate::convert::AsF32;
use crate::overlay::OverlayLine;
use idtx_scene::{axis_local_y_rotation, ResolvedKind, ResolvedShape};

/// Segment count for every circle drawn by the analytic collision glyphs.
const SEG: usize = 24;

/// The light-blue accent used for collision wireframes (matches the editor's
/// active/selection accent family).
pub const COLLISION_COLOR: [f32; 4] = [0.40, 0.65, 1.0, 1.0];

/// The 12 edges of a box, as index pairs into the 8-corner array built by
/// [`push_box`].
const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1), (1, 2), (2, 3), (3, 0), // back face
    (4, 5), (5, 6), (6, 7), (7, 4), // front face
    (0, 4), (1, 5), (2, 6), (3, 7), // connectors
];

/// Append the analytic collision glyph for a [`ResolvedShape`], placed by
/// `world_matrix` (full render-space transform, scale included so the wireframe
/// matches the collision test). The shape centre and the cylinder/cone/capsule
/// axis are folded into the matrix so the Y-aligned builders serve any axis,
/// exactly mirroring the picking narrow-phase ([`idtx_scene::picking`]).
///
/// Only the analytic shapes carry a [`ResolvedShape`]; `hull`/`mesh` geometry is
/// drawn by [`push_collision_trisoup`] / [`push_collision_polysoup`] from the
/// authored arrays, which the session has to hand.
pub fn push_collision_analytic(
    out: &mut Vec<OverlayLine>,
    resolved: ResolvedShape,
    world_matrix: Mat4,
    color: [f32; 4],
) {
    let centred = world_matrix * Mat4::from_translation(resolved.center);
    match resolved.kind {
        ResolvedKind::Sphere { radius } => {
            push_box_sphere(out, centred, Vec3::ZERO, radius, color);
        }
        ResolvedKind::Cube { half_extents } => {
            push_box(out, centred, Vec3::ZERO, half_extents, color);
        }
        ResolvedKind::Cylinder { radius, half_height, axis } => {
            push_cylinder(out, centred * axis_local_y_rotation(axis), Vec3::ZERO, radius, half_height, color);
        }
        ResolvedKind::Cone { radius, half_height, axis } => {
            push_cone(out, centred * axis_local_y_rotation(axis), Vec3::ZERO, radius, half_height, color);
        }
        ResolvedKind::Capsule { radius, half_height, axis } => {
            push_capsule(out, centred * axis_local_y_rotation(axis), Vec3::ZERO, radius, half_height, color);
        }
    }
}

/// Wireframe edges of a flat triangle-index soup (the `hull:points` +
/// `hull:triangles` collision hull), deduplicated so shared edges draw once.
pub fn push_collision_trisoup(
    out: &mut Vec<OverlayLine>,
    world_matrix: Mat4,
    points: &[[f32; 3]],
    triangles: &[i32],
    color: [f32; 4],
) {
    if points.is_empty() {
        return;
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for tri in triangles.chunks_exact(3) {
        let i = [clamp_index(tri[0], points.len()), clamp_index(tri[1], points.len()), clamp_index(tri[2], points.len())];
        for e in [(i[0], i[1]), (i[1], i[2]), (i[2], i[0])] {
            push_unique_edge(out, world_matrix, points, e, &mut seen, color);
        }
    }
}

/// Wireframe boundary edges of a USD polygon soup (`face_vertex_counts` +
/// `face_vertex_indices`) — used for `shape = mesh`, drawing the prim's own mesh
/// outline rather than a triangulation. Deduplicated.
pub fn push_collision_polysoup(
    out: &mut Vec<OverlayLine>,
    world_matrix: Mat4,
    points: &[[f32; 3]],
    face_vertex_counts: &[i32],
    face_vertex_indices: &[i32],
    color: [f32; 4],
) {
    if points.is_empty() {
        return;
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut cursor = 0usize;
    for &count in face_vertex_counts {
        let n = usize::try_from(count).unwrap_or(0);
        if n >= 2 {
            for k in 0..n {
                let a = face_vertex_indices.get(cursor + k).copied().unwrap_or(0);
                let b = face_vertex_indices.get(cursor + (k + 1) % n).copied().unwrap_or(0);
                let edge = (clamp_index(a, points.len()), clamp_index(b, points.len()));
                push_unique_edge(out, world_matrix, points, edge, &mut seen, color);
            }
        }
        cursor += n;
    }
}

// ── Analytic builders (local space → transformed by world_matrix) ───────────────

/// Three great circles (XY/YZ/ZX planes) of `radius` about local `center`.
fn push_box_sphere(out: &mut Vec<OverlayLine>, xf: Mat4, center: Vec3, radius: f32, color: [f32; 4]) {
    push_circle(out, xf, center, Vec3::X, Vec3::Y, radius, color);
    push_circle(out, xf, center, Vec3::Y, Vec3::Z, radius, color);
    push_circle(out, xf, center, Vec3::Z, Vec3::X, radius, color);
}

/// 12 edges of the oriented box of `half` extents about local `center`.
fn push_box(out: &mut Vec<OverlayLine>, xf: Mat4, center: Vec3, half: Vec3, color: [f32; 4]) {
    let corner = |sx: f32, sy: f32, sz: f32| {
        center + Vec3::new(sx * half.x, sy * half.y, sz * half.z)
    };
    // 8 corners indexed by (x,y,z) sign bits.
    let c = [
        corner(-1.0, -1.0, -1.0),
        corner(1.0, -1.0, -1.0),
        corner(1.0, 1.0, -1.0),
        corner(-1.0, 1.0, -1.0),
        corner(-1.0, -1.0, 1.0),
        corner(1.0, -1.0, 1.0),
        corner(1.0, 1.0, 1.0),
        corner(-1.0, 1.0, 1.0),
    ];
    for (a, b) in BOX_EDGES {
        push_seg(out, xf, c[a], c[b], color);
    }
}

/// Y-aligned capped cylinder: two end circles at `center ± half_height·Y` plus
/// four vertical connectors.
fn push_cylinder(
    out: &mut Vec<OverlayLine>,
    xf: Mat4,
    center: Vec3,
    radius: f32,
    half_height: f32,
    color: [f32; 4],
) {
    let top = center + Vec3::Y * half_height;
    let bot = center - Vec3::Y * half_height;
    push_circle(out, xf, top, Vec3::X, Vec3::Z, radius, color);
    push_circle(out, xf, bot, Vec3::X, Vec3::Z, radius, color);
    for (ux, uz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let off = Vec3::new(ux * radius, 0.0, uz * radius);
        push_seg(out, xf, bot + off, top + off, color);
    }
}

/// Y-aligned capped cone: a base circle at `center − half_height·Y` and four
/// connectors rising to the apex at `center + half_height·Y`.
fn push_cone(
    out: &mut Vec<OverlayLine>,
    xf: Mat4,
    center: Vec3,
    radius: f32,
    half_height: f32,
    color: [f32; 4],
) {
    let apex = center + Vec3::Y * half_height;
    let base = center - Vec3::Y * half_height;
    push_circle(out, xf, base, Vec3::X, Vec3::Z, radius, color);
    for (ux, uz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let off = Vec3::new(ux * radius, 0.0, uz * radius);
        push_seg(out, xf, base + off, apex, color);
    }
}

/// Y-aligned capsule: a cylinder segment (half-length `half_height - radius`)
/// plus two hemispherical caps drawn as cross circles at the cap centres.
fn push_capsule(
    out: &mut Vec<OverlayLine>,
    xf: Mat4,
    center: Vec3,
    radius: f32,
    half_height: f32,
    color: [f32; 4],
) {
    let seg = (half_height - radius).max(0.0);
    let top = center + Vec3::Y * seg;
    let bot = center - Vec3::Y * seg;
    // Body: two circles + 4 connectors over the straight section.
    push_circle(out, xf, top, Vec3::X, Vec3::Z, radius, color);
    push_circle(out, xf, bot, Vec3::X, Vec3::Z, radius, color);
    for (ux, uz) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let off = Vec3::new(ux * radius, 0.0, uz * radius);
        push_seg(out, xf, bot + off, top + off, color);
    }
    // Caps: cross half-circles in the XY and ZY planes at each hemisphere centre
    // read as rounded ends. Only the outward-facing hemisphere is drawn — the top
    // cap arcs over +Y, the bottom cap under −Y — so the caps look like domes, not
    // full cross circles poking back into the body.
    use std::f32::consts::PI;
    push_arc(out, xf, top, Vec3::X, Vec3::Y, radius, 0.0, PI, SEG / 2, color);
    push_arc(out, xf, top, Vec3::Z, Vec3::Y, radius, 0.0, PI, SEG / 2, color);
    push_arc(out, xf, bot, Vec3::X, Vec3::Y, radius, PI, 2.0 * PI, SEG / 2, color);
    push_arc(out, xf, bot, Vec3::Z, Vec3::Y, radius, PI, 2.0 * PI, SEG / 2, color);
}

// ── Low-level helpers ───────────────────────────────────────────────────────────

/// Push one world-space segment, transforming both local endpoints by `xf`.
fn push_seg(out: &mut Vec<OverlayLine>, xf: Mat4, a: Vec3, b: Vec3, color: [f32; 4]) {
    out.push(OverlayLine {
        a: xf.transform_point3(a).to_array(),
        b: xf.transform_point3(b).to_array(),
        color,
    });
}

/// Push a closed circle of `radius` about local `center` in the `u_axis`/`v_axis`
/// plane, each vertex transformed by `xf`. Emits exactly [`SEG`] segments.
fn push_circle(
    out: &mut Vec<OverlayLine>,
    xf: Mat4,
    center: Vec3,
    u_axis: Vec3,
    v_axis: Vec3,
    radius: f32,
    color: [f32; 4],
) {
    use std::f32::consts::TAU;
    push_arc(out, xf, center, u_axis, v_axis, radius, 0.0, TAU, SEG, color);
}

/// Push an open arc of `radius` about local `center` in the `u_axis`/`v_axis`
/// plane, sweeping from angle `t0` to `t1` (radians) in `segs` segments, each
/// vertex transformed by `xf`. A half-turn cap passes `SEG / 2` to match
/// [`push_circle`]'s per-radian density.
#[allow(clippy::too_many_arguments)]
fn push_arc(
    out: &mut Vec<OverlayLine>,
    xf: Mat4,
    center: Vec3,
    u_axis: Vec3,
    v_axis: Vec3,
    radius: f32,
    t0: f32,
    t1: f32,
    segs: usize,
    color: [f32; 4],
) {
    let span = t1 - t0;
    let segs = segs.max(1);
    let mut prev = center + (u_axis * t0.cos() + v_axis * t0.sin()) * radius;
    for i in 1..=segs {
        let t = t0 + span * (i.as_f32() / segs.as_f32());
        let p = center + (u_axis * t.cos() + v_axis * t.sin()) * radius;
        push_seg(out, xf, prev, p, color);
        prev = p;
    }
}

/// Push an edge once (keyed by its unordered index pair).
fn push_unique_edge(
    out: &mut Vec<OverlayLine>,
    xf: Mat4,
    points: &[[f32; 3]],
    (a, b): (usize, usize),
    seen: &mut HashSet<(usize, usize)>,
    color: [f32; 4],
) {
    if a == b {
        return;
    }
    let key = (a.min(b), a.max(b));
    if seen.insert(key) {
        push_seg(out, xf, Vec3::from(points[a]), Vec3::from(points[b]), color);
    }
}

#[inline]
fn clamp_index(raw: i32, n: usize) -> usize {
    usize::try_from(raw).unwrap_or(0).min(n - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_has_twelve_edges() {
        let mut out = Vec::new();
        push_box(&mut out, Mat4::IDENTITY, Vec3::ZERO, Vec3::splat(1.0), COLLISION_COLOR);
        assert_eq!(out.len(), 12);
    }

    #[test]
    fn sphere_has_three_great_circles() {
        let mut out = Vec::new();
        push_collision_analytic(
            &mut out,
            ResolvedShape {
                center: Vec3::ZERO,
                kind: ResolvedKind::Sphere { radius: 1.0 },
            },
            Mat4::IDENTITY,
            COLLISION_COLOR,
        );
        assert_eq!(out.len(), 3 * SEG);
    }

    #[test]
    fn cylinder_counts() {
        let mut out = Vec::new();
        push_cylinder(&mut out, Mat4::IDENTITY, Vec3::ZERO, 1.0, 2.0, COLLISION_COLOR);
        // Two end circles + four connectors.
        assert_eq!(out.len(), 2 * SEG + 4);
    }

    #[test]
    fn cone_has_base_circle_and_four_connectors() {
        let mut out = Vec::new();
        push_cone(&mut out, Mat4::IDENTITY, Vec3::ZERO, 1.0, 2.0, COLLISION_COLOR);
        assert_eq!(out.len(), SEG + 4);
    }

    #[test]
    fn capsule_caps_are_half_circles() {
        // Body: two full circles (2·SEG) + 4 connectors. Caps: four half-circles,
        // SEG/2 segments each (2·SEG) — not four full cross circles (4·SEG).
        let mut out = Vec::new();
        push_capsule(&mut out, Mat4::IDENTITY, Vec3::ZERO, 1.0, 2.0, COLLISION_COLOR);
        assert_eq!(out.len(), 2 * SEG + 4 + 4 * (SEG / 2));
    }

    #[test]
    fn capsule_cap_arc_stays_on_the_outward_hemisphere() {
        // A top-cap half-arc (t ∈ [0, π]) in the XY plane must keep every vertex on
        // or above the ring plane (y ≥ 0) — a full circle would dip to y = −radius,
        // poking back through the straight section instead of doming outward.
        use std::f32::consts::PI;
        let radius = 1.0_f32;
        let mut out = Vec::new();
        push_arc(&mut out, Mat4::IDENTITY, Vec3::ZERO, Vec3::X, Vec3::Y, radius, 0.0, PI, SEG / 2, COLLISION_COLOR);
        for line in &out {
            for p in [line.a, line.b] {
                assert!(p[1] >= -1e-4, "cap arc vertex y={} dipped below the ring plane", p[1]);
            }
        }
        // And the arc reaches the pole (y = +radius at its apex).
        let apex_y = out.iter().flat_map(|l| [l.a[1], l.b[1]]).fold(f32::MIN, f32::max);
        assert!((apex_y - radius).abs() < 1e-3, "cap arc apex y={apex_y} should reach +radius");
    }

    #[test]
    fn analytic_hull_and_mesh_have_no_resolved_shape() {
        // Hull / Mesh never produce a ResolvedShape, so the session routes them to
        // the trisoup / polysoup builders instead — push_collision_analytic is only
        // ever called with an analytic ResolvedKind. This test documents that the
        // analytic builder has no hull/mesh arm (it is a total match on ResolvedKind).
        let mut out = Vec::new();
        push_collision_analytic(
            &mut out,
            ResolvedShape {
                center: Vec3::ZERO,
                kind: ResolvedKind::Cube { half_extents: Vec3::ONE },
            },
            Mat4::IDENTITY,
            COLLISION_COLOR,
        );
        assert_eq!(out.len(), 12);
    }

    #[test]
    fn trisoup_dedups_shared_edges() {
        // Two triangles sharing the 0-2 diagonal of a quad → 5 unique edges.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let tris = vec![0, 1, 2, 0, 2, 3];
        let mut out = Vec::new();
        push_collision_trisoup(&mut out, Mat4::IDENTITY, &points, &tris, COLLISION_COLOR);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn polysoup_draws_face_boundary() {
        // A single quad face → 4 boundary edges (no diagonal).
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let mut out = Vec::new();
        push_collision_polysoup(
            &mut out,
            Mat4::IDENTITY,
            &points,
            &[4],
            &[0, 1, 2, 3],
            COLLISION_COLOR,
        );
        assert_eq!(out.len(), 4);
    }
}
