//! DCC-style selection glyphs for USD light prims.
//!
//! Lights carry no `BoundsComp`, so the corner-tick selection overlay (see
//! [`crate::overlay`]) never draws anything for them. Instead, when a light is
//! selected the session builds a light-type-specific wireframe glyph — the kind
//! of representation common across DCC tools (Blender/Maya): a wireframe sphere
//! for a sphere light, a cone for a spotlight, a rectangle/disk with a normal for
//! area lights, a sun disk with parallel rays for a distant light, an end-capped
//! cylinder, and a large enclosing sphere for a dome.
//!
//! All builders are pure [`OverlayLine`] producers (no wgpu), so they are unit
//! tested directly. They run once per frame for the single selected light
//! (≤ ~72 lines), so this is not a hot path and carries no `metrics` timer.

use crate::convert::AsF32;
use crate::overlay::OverlayLine;
use crate::scene_extract::RenderLightKind;
use glam::{Mat4, Vec3};

/// Segment count for every circle/great-circle drawn by these glyphs.
const SEG: usize = 24;

/// Inputs for [`push_light_glyph`], gathered by the session from a light's
/// [`crate::scene_extract::RenderLightDesc`].
#[derive(Debug, Clone, Copy)]
pub struct LightGlyphParams {
    /// Light geometry variant (dimensions already in render units).
    pub kind: RenderLightKind,
    /// Render-space transform of the light prim (translation in column 3).
    pub world_matrix: Mat4,
    /// `Some(half_angle_degrees)` when a `UsdLuxShapingAPI` cone is authored —
    /// turns a sphere/disk light into a spotlight glyph.
    pub cone_angle: Option<f32>,
    /// Shaping cone softness in `[0, 1]`; shrinks the inner cone circle.
    pub cone_softness: f32,
    /// Scene bounding-sphere radius (render units), used to size the otherwise
    /// dimensionless distant and dome glyphs. `0.0` falls back to unit sizing.
    pub scene_radius: f32,
    /// RGBA glyph colour.
    pub color: [f32; 4],
}

/// Appends the selection glyph for `p`'s light into `out`.
pub fn push_light_glyph(out: &mut Vec<OverlayLine>, p: &LightGlyphParams) {
    let (x, y, z) = basis(&p.world_matrix);
    let origin = p.world_matrix.w_axis.truncate();
    let color = p.color;

    match p.kind {
        RenderLightKind::Distant { .. } => {
            push_distant(out, origin, x, y, z, p.scene_radius, color);
        }
        RenderLightKind::Sphere { radius } => {
            if let Some(cone) = p.cone_angle {
                push_spot(
                    out,
                    origin,
                    x,
                    y,
                    z,
                    cone,
                    p.cone_softness,
                    p.scene_radius,
                    color,
                );
            } else if radius > 1e-4 {
                push_sphere(out, origin, x, y, z, radius, color);
            } else {
                push_point_star(
                    out,
                    origin,
                    x,
                    y,
                    z,
                    glyph_unit(p.scene_radius) * 0.15,
                    color,
                );
            }
        }
        RenderLightKind::Disk { radius } => {
            if let Some(cone) = p.cone_angle {
                push_spot(
                    out,
                    origin,
                    x,
                    y,
                    z,
                    cone,
                    p.cone_softness,
                    p.scene_radius,
                    color,
                );
            } else {
                push_disk(out, origin, x, y, z, radius, color);
            }
        }
        RenderLightKind::Rect { width, height } => {
            push_rect(out, origin, x, y, z, width, height, color);
        }
        RenderLightKind::Cylinder { radius, length } => {
            push_cylinder(out, origin, x, y, z, radius, length, color);
        }
        RenderLightKind::Dome | RenderLightKind::DomeProcedural => {
            // The dome is world-enclosing; ignore the light translation and centre
            // the glyph on the world origin at the scene radius.
            let r = glyph_unit(p.scene_radius);
            push_sphere(out, Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z, r, color);
        }
    }
}

// ── Per-kind builders ──────────────────────────────────────────────────────────

/// Sun disk (circle perpendicular to the emit axis) + parallel rays along the
/// emission direction (`-Z`). Pushes `SEG + 4` lines.
fn push_distant(
    out: &mut Vec<OverlayLine>,
    o: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    scene_r: f32,
    color: [f32; 4],
) {
    let unit = glyph_unit(scene_r);
    let disk_r = unit * 0.12;
    let ray_len = unit * 0.4;
    // Emission travels along -Z (the shader treats +Z as *toward* the light).
    let dir = -z;
    push_circle(out, o, x, y, disk_r, color);
    // Four parallel rays starting on the disk rim, all sharing `dir`.
    for (u, v) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let start = o + (x * u + y * v) * disk_r;
        out.push(OverlayLine {
            a: start.to_array(),
            b: (start + dir * ray_len).to_array(),
            color,
        });
    }
}

/// Three great circles of `radius` about `center`. Pushes `3 * SEG` lines.
fn push_sphere(
    out: &mut Vec<OverlayLine>,
    c: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    radius: f32,
    color: [f32; 4],
) {
    push_circle(out, c, x, y, radius, color);
    push_circle(out, c, y, z, radius, color);
    push_circle(out, c, z, x, radius, color);
}

/// Small three-axis star for a treat-as-point sphere light. Pushes 3 lines.
fn push_point_star(
    out: &mut Vec<OverlayLine>,
    c: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    r: f32,
    color: [f32; 4],
) {
    for axis in [x, y, z] {
        out.push(OverlayLine {
            a: (c - axis * r).to_array(),
            b: (c + axis * r).to_array(),
            color,
        });
    }
}

/// Spotlight cone opening along `-Z`: 4 apex→rim lines + outer base circle +
/// inner softness circle. Pushes `4 + 2 * SEG` lines.
fn push_spot(
    out: &mut Vec<OverlayLine>,
    apex: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    cone_angle_deg: f32,
    softness: f32,
    scene_r: f32,
    color: [f32; 4],
) {
    let dir = -z;
    let length = glyph_unit(scene_r) * 0.5;
    let half = cone_angle_deg.clamp(0.0, 89.9).to_radians();
    let outer_r = length * half.tan();
    let base = apex + dir * length;

    // Apex → rim lines at 4 cardinal points on the outer circle.
    for (u, v) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let rim = base + (x * u + y * v) * outer_r;
        out.push(OverlayLine {
            a: apex.to_array(),
            b: rim.to_array(),
            color,
        });
    }
    // Outer base circle.
    push_circle(out, base, x, y, outer_r, color);
    // Inner softness circle (softness shrinks the effective cone edge).
    let inner_half = (half * (1.0 - softness.clamp(0.0, 1.0))).max(0.0);
    let inner_r = length * inner_half.tan();
    push_circle(out, base, x, y, inner_r, color);
}

/// Rectangle in the local XY plane (`width`×`height`) + a `-Z` normal line.
/// Pushes 5 lines.
fn push_rect(
    out: &mut Vec<OverlayLine>,
    o: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    width: f32,
    height: f32,
    color: [f32; 4],
) {
    let hw = x * (width * 0.5);
    let hh = y * (height * 0.5);
    let p = [o - hw - hh, o + hw - hh, o + hw + hh, o - hw + hh];
    for i in 0..4 {
        out.push(OverlayLine {
            a: p[i].to_array(),
            b: p[(i + 1) % 4].to_array(),
            color,
        });
    }
    let normal_len = (width.max(height)) * 0.5;
    out.push(OverlayLine {
        a: o.to_array(),
        b: (o - z * normal_len).to_array(),
        color,
    });
}

/// Circle of `radius` in the local XY plane + a `-Z` normal line. Pushes
/// `SEG + 1` lines.
fn push_disk(
    out: &mut Vec<OverlayLine>,
    o: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    radius: f32,
    color: [f32; 4],
) {
    push_circle(out, o, x, y, radius, color);
    out.push(OverlayLine {
        a: o.to_array(),
        b: (o - z * radius).to_array(),
        color,
    });
}

/// Two end circles of `radius` at `±length/2` along the local **X** axis (the
/// `UsdLuxCylinderLight` axis) + 4 connecting lines. Pushes `2 * SEG + 4` lines.
fn push_cylinder(
    out: &mut Vec<OverlayLine>,
    o: Vec3,
    x: Vec3,
    y: Vec3,
    z: Vec3,
    radius: f32,
    length: f32,
    color: [f32; 4],
) {
    let cap_a = o - x * (length * 0.5);
    let cap_b = o + x * (length * 0.5);
    push_circle(out, cap_a, y, z, radius, color);
    push_circle(out, cap_b, y, z, radius, color);
    for (u, v) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let off = (y * u + z * v) * radius;
        out.push(OverlayLine {
            a: (cap_a + off).to_array(),
            b: (cap_b + off).to_array(),
            color,
        });
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────────

/// Pushes a closed circle of `r` centred at `c` in the plane spanned by unit
/// vectors `u`/`v`. Emits exactly [`SEG`] line segments.
fn push_circle(out: &mut Vec<OverlayLine>, c: Vec3, u: Vec3, v: Vec3, r: f32, color: [f32; 4]) {
    use std::f32::consts::TAU;
    let mut prev = c + u * r;
    for i in 1..=SEG {
        let t = i.as_f32() / SEG.as_f32() * TAU;
        let p = c + (u * t.cos() + v * t.sin()) * r;
        out.push(OverlayLine {
            a: prev.to_array(),
            b: p.to_array(),
            color,
        });
        prev = p;
    }
}

/// Normalized rotation basis from a world matrix, falling back to world axes for
/// degenerate (zero-scale) columns. Never bakes matrix scale into the glyph — the
/// explicit radius/size supplied per kind already carries the emitter dimensions.
fn basis(m: &Mat4) -> (Vec3, Vec3, Vec3) {
    let pick = |v: Vec3, fallback: Vec3| {
        if v.length_squared() > 1e-12 {
            v.normalize()
        } else {
            fallback
        }
    };
    (
        pick(m.x_axis.truncate(), Vec3::X),
        pick(m.y_axis.truncate(), Vec3::Y),
        pick(m.z_axis.truncate(), Vec3::Z),
    )
}

/// Characteristic glyph size for dimensionless lights (distant/dome) and cone
/// depth: half the scene radius, clamped to a sane minimum.
fn glyph_unit(scene_r: f32) -> f32 {
    if scene_r > 1e-4 { scene_r } else { 1.0 }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod glyph_tests {
    use super::*;

    fn params(kind: RenderLightKind) -> LightGlyphParams {
        LightGlyphParams {
            kind,
            world_matrix: Mat4::IDENTITY,
            cone_angle: None,
            cone_softness: 0.0,
            scene_radius: 4.0,
            color: [1.0, 0.9, 0.2, 1.0],
        }
    }

    fn len(l: &OverlayLine) -> f32 {
        let d = [l.b[0] - l.a[0], l.b[1] - l.a[1], l.b[2] - l.a[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    fn build(p: &LightGlyphParams) -> Vec<OverlayLine> {
        let mut out = Vec::new();
        push_light_glyph(&mut out, p);
        out
    }

    #[test]
    fn line_counts_per_kind() {
        assert_eq!(
            build(&params(RenderLightKind::Distant { angle_radians: 0.1 })).len(),
            SEG + 4
        );
        assert_eq!(
            build(&params(RenderLightKind::Sphere { radius: 1.0 })).len(),
            3 * SEG
        );
        assert_eq!(
            build(&params(RenderLightKind::Disk { radius: 1.0 })).len(),
            SEG + 1
        );
        assert_eq!(
            build(&params(RenderLightKind::Rect {
                width: 2.0,
                height: 1.0
            }))
            .len(),
            5
        );
        assert_eq!(
            build(&params(RenderLightKind::Cylinder {
                radius: 0.5,
                length: 3.0
            }))
            .len(),
            2 * SEG + 4
        );
        assert_eq!(build(&params(RenderLightKind::Dome)).len(), 3 * SEG);
        // Treat-as-point sphere → 3-axis star.
        assert_eq!(
            build(&params(RenderLightKind::Sphere { radius: 0.0 })).len(),
            3
        );
        // Spot (sphere + cone).
        let mut spot = params(RenderLightKind::Sphere { radius: 1.0 });
        spot.cone_angle = Some(30.0);
        assert_eq!(build(&spot).len(), 4 + 2 * SEG);
    }

    #[test]
    fn sphere_vertices_within_radius() {
        let r = 2.5;
        let p = params(RenderLightKind::Sphere { radius: r });
        for l in build(&p) {
            for v in [l.a, l.b] {
                let d = Vec3::from(v).length();
                assert!(d <= r + 1e-3, "vertex {d} outside sphere radius {r}");
            }
        }
    }

    #[test]
    fn distant_rays_are_parallel_to_emit_axis() {
        // Identity matrix ⇒ emit direction is -Z.
        let out = build(&params(RenderLightKind::Distant { angle_radians: 0.1 }));
        let rays: Vec<_> = out
            .iter()
            .filter(|l| {
                let d = (Vec3::from(l.b) - Vec3::from(l.a)).normalize_or_zero();
                d.dot(-Vec3::Z) > 0.999
            })
            .collect();
        assert_eq!(rays.len(), 4, "four parallel rays along -Z");
    }

    #[test]
    fn spot_rim_lies_on_cone_half_angle() {
        let mut p = params(RenderLightKind::Sphere { radius: 1.0 });
        let half_deg = 25.0_f32;
        p.cone_angle = Some(half_deg);
        let out = build(&p);
        // Apex→rim lines start at the apex (origin) and open at `half_deg` off -Z.
        let apex = Vec3::ZERO;
        let mut checked = 0;
        for l in &out {
            if Vec3::from(l.a).abs_diff_eq(apex, 1e-5) {
                let dir = (Vec3::from(l.b) - apex).normalize();
                let angle = dir.dot(-Vec3::Z).clamp(-1.0, 1.0).acos().to_degrees();
                assert!(
                    (angle - half_deg).abs() < 0.5,
                    "rim angle {angle} != {half_deg}"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 4, "four apex→rim lines");
    }

    #[test]
    fn glyph_radius_ignores_matrix_scale() {
        // A non-uniform scale in the world matrix must NOT change the sphere radius:
        // the glyph uses normalized basis vectors + the explicit radius.
        let r = 1.0;
        let mut p = params(RenderLightKind::Sphere { radius: r });
        p.world_matrix = Mat4::from_scale(Vec3::new(5.0, 0.2, 3.0));
        let max = build(&p)
            .iter()
            .flat_map(|l| [l.a, l.b])
            .map(|v| Vec3::from(v).length())
            .fold(0.0_f32, f32::max);
        assert!(
            (max - r).abs() < 1e-3,
            "max vertex distance {max} should equal radius {r}"
        );
    }

    #[test]
    fn all_lines_finite() {
        for kind in [
            RenderLightKind::Distant { angle_radians: 0.1 },
            RenderLightKind::Sphere { radius: 1.0 },
            RenderLightKind::Disk { radius: 1.0 },
            RenderLightKind::Rect {
                width: 2.0,
                height: 1.0,
            },
            RenderLightKind::Cylinder {
                radius: 0.5,
                length: 3.0,
            },
            RenderLightKind::Dome,
        ] {
            for l in build(&params(kind)) {
                assert!(len(&l).is_finite());
            }
        }
    }
}
