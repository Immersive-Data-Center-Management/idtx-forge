//! Convex-hull generation for the `IdtxCollisionAPI` `shape = hull` collider.
//!
//! Pure geometry: takes a point cloud (already in the collision prim's local
//! space — the session gathers and transforms it, see
//! `idtx_studio_session`) and returns a convex hull as a vertex list plus a
//! flat triangle-index soup, matching the layout the
//! `idtx:collision:hull:points` / `idtx:collision:hull:triangles` attributes
//! and the wireframe renderer (`idtx_render::push_collision_trisoup`) expect.
//!
//! This crate is a pure-geometry leaf — no `bevy_ecs`, no `idtx_usd`. It is
//! deliberately separate from the `idtx-scene` USD→ECS projection facade: hull
//! generation is editor-only authoring, not scene hydration, and keeping it out
//! of `idtx-scene` means the read-only inspector and the runtime player never
//! compile or link the hull machinery (or `parry3d`).
//!
//! Backed by `parry3d`'s quickhull (`transformation::try_convex_hull`), the
//! same robust hull implementation rapier uses. Compiled only with the
//! **`hull-gen`** feature so the runtime player never links it. (It replaced the
//! `chull` crate, whose quickhull was `HashSet`-order nondeterministic and
//! intermittently failed — `Degenerated` / `RoundOffError` — on dense real
//! meshes such as `UsdCookie`; parry hulls that same 118 k-point cloud 20/20.)
//!
//! The one performance-heavy step is the quickhull call itself; it is timed and
//! logged behind the **`hull-metrics`** feature (see [`generate_convex_hull`]).

use std::collections::HashMap;

use thiserror::Error;

/// A 3D convex hull needs at least a (non-degenerate) tetrahedron.
const MIN_HULL_POINTS: usize = 4;

/// Default cap on the generated hull's vertex count (see
/// [`HullParams::max_hull_vertices`]). Matches the runtime safety cap in
/// `idtx-physics` (`shape::HULL_MAX_VERTICES`) so an inspector-generated hull is
/// used by the physics simulation **verbatim** — never further reduced at
/// runtime — keeping "what you generate and see" identical to what simulates.
pub const DEFAULT_MAX_HULL_VERTICES: usize = 48;

/// Tunable parameters for [`generate_convex_hull`], surfaced in the collision
/// inspector panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HullParams {
    /// Input clouds larger than this are voxel-decimated before hulling, to bound
    /// quickhull compute time on dense meshes.
    pub max_input_points: usize,
    /// Cap on the **output** hull's vertex count. A high-vertex scanned hull
    /// (hundreds of near-coplanar rim verts) produces an unstable contact
    /// manifold that keeps a dynamic body jittering and never sleeping; the
    /// generator reduces the hull to at most this many well-distributed
    /// (direction-sampled) vertices so the resting contact is stable. The user
    /// "collision detail" knob; clamped to at least [`MIN_HULL_POINTS`].
    pub max_hull_vertices: usize,
}

impl Default for HullParams {
    fn default() -> Self {
        Self {
            max_input_points: 20_000,
            max_hull_vertices: DEFAULT_MAX_HULL_VERTICES,
        }
    }
}

/// A generated convex hull: boundary vertices plus a flat triangle-index soup
/// (each consecutive 3 indices form one triangle into `points`, CCW winding),
/// ready to author into `idtx:collision:hull:points` / `:triangles`.
#[derive(Debug, Clone, PartialEq)]
pub struct HullResult {
    pub points: Vec<[f32; 3]>,
    pub triangles: Vec<i32>,
}

/// Why [`generate_convex_hull`] could not produce a hull. Carried to the shell
/// so the GUI user sees a status message rather than a silent no-op or a panic.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HullError {
    /// Fewer than four input points — no volume to enclose.
    #[error("convex hull needs at least {MIN_HULL_POINTS} points, got {0}")]
    TooFewPoints(usize),
    /// parry's quickhull rejected the input (typically all points
    /// coplanar/collinear — no enclosed volume).
    #[error("convex hull generation failed (points may be degenerate): {0}")]
    Compute(String),
}

/// Compute the convex hull of `points` (local space) under `params`.
///
/// Decimates the cloud to at most `params.max_input_points` first (both to bound
/// the compute and to keep the resulting collider lean), then runs parry3d's
/// quickhull. Returns a [`HullError`] (never panics) for empty, too-small, or
/// degenerate input.
///
/// # Errors
/// [`HullError::TooFewPoints`] when fewer than four points remain, or
/// [`HullError::Compute`] when the hull is degenerate (e.g. a fully coplanar
/// cloud).
#[cfg(feature = "hull-gen")]
pub fn generate_convex_hull(
    points: &[[f32; 3]],
    params: &HullParams,
) -> Result<HullResult, HullError> {
    use glam::Vec3;

    if points.len() < MIN_HULL_POINTS {
        return Err(HullError::TooFewPoints(points.len()));
    }

    #[cfg(feature = "hull-metrics")]
    let started = std::time::Instant::now();

    let decimated = decimate(points, params.max_input_points);
    if decimated.len() < MIN_HULL_POINTS {
        return Err(HullError::TooFewPoints(decimated.len()));
    }

    // parry3d 0.31 is glam-native, so the cloud maps straight to `Vec3`.
    // `try_convex_hull` is deterministic and robust (same input ⇒ same hull);
    // it returns an error rather than panicking on degenerate input.
    let cloud: Vec<Vec3> = decimated.iter().map(|p| Vec3::from(*p)).collect();
    let (verts, tris) = parry3d::transformation::try_convex_hull(&cloud)
        .map_err(|e| HullError::Compute(format!("{e:?}")))?;

    // Cap the OUTPUT vertex count: a high-vertex scanned hull (the UsdCookie's
    // ~285-vtx hull) has clustered near-coplanar rim verts that make the resting
    // contact manifold flip-flop → a dynamic body jitters and never sleeps. Keep
    // a direction-sampled subset (even distribution → stable manifold) and
    // re-hull it so points + triangles stay consistent. Authored at this budget,
    // the hull is used by the physics sim verbatim (no runtime re-reduction).
    let max_hull = params.max_hull_vertices.max(MIN_HULL_POINTS);
    let (verts, tris) = if verts.len() > max_hull {
        let reduced = reduce_hull_vertices(&verts, max_hull);
        parry3d::transformation::try_convex_hull(&reduced)
            .map_err(|e| HullError::Compute(format!("{e:?}")))?
    } else {
        (verts, tris)
    };

    // A degenerate result is a failure, not a collider. parry does not error on
    // a flat (coplanar) cloud — it returns a zero-thickness sheet with ≥ 4
    // double-sided triangles — so the triangle count can't catch it. Reject by
    // enclosed volume instead: sum the signed tetrahedron volumes of the hull
    // faces and require it to be non-trivial relative to the hull's size (so a
    // genuinely thin-but-solid slab still passes, but a flat sheet does not).
    let char_len = {
        let mut lo = verts[0];
        let mut hi = verts[0];
        for v in &verts {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        (hi - lo).max_element()
    };
    let volume = tris
        .iter()
        .map(|t| {
            let a = verts[t[0] as usize];
            let b = verts[t[1] as usize];
            let c = verts[t[2] as usize];
            a.dot(b.cross(c))
        })
        .sum::<f32>()
        .abs()
        / 6.0;
    if verts.len() < MIN_HULL_POINTS || char_len <= 0.0 || volume < 1.0e-6 * char_len.powi(3) {
        return Err(HullError::Compute(format!(
            "degenerate hull: {} verts / {} tris / volume {volume:.3e}",
            verts.len(),
            tris.len(),
        )));
    }

    let out_points: Vec<[f32; 3]> = verts.iter().map(|v| [v.x, v.y, v.z]).collect();
    let out_triangles: Vec<i32> = tris
        .iter()
        .flat_map(|t| t.iter().map(|&i| i32::try_from(i).unwrap_or(0)))
        .collect();

    #[cfg(feature = "hull-metrics")]
    tracing::info!(
        target: "idtx::collision::hull",
        input_points = points.len(),
        decimated_points = decimated.len(),
        output_vertices = out_points.len(),
        output_triangles = out_triangles.len() / 3,
        elapsed_ms = started.elapsed().as_secs_f64() * 1.0e3,
        "generated convex collision hull",
    );

    Ok(HullResult {
        points: out_points,
        triangles: out_triangles,
    })
}

/// Reduce a convex hull's vertices to at most `max` well-distributed points:
/// keep the extreme vertex along each of `max` evenly-spread (Fibonacci-sphere)
/// directions. Returns the input unchanged when already within budget. Monotonic
/// and artifact-free (unlike grid quantization, which can make the contact
/// manifold *worse*), and the result is a subset of the input, so re-hulling it
/// yields a tight inscribed hull. Mirrors `idtx_physics::shape`'s runtime
/// fallback so a generated hull equals the physics collider.
#[cfg(feature = "hull-gen")]
#[allow(clippy::cast_precision_loss)] // `i`/`max` are tiny vertex-budget counts.
fn reduce_hull_vertices(points: &[glam::Vec3], max: usize) -> Vec<glam::Vec3> {
    use glam::Vec3;
    use std::f32::consts::PI;
    if points.len() <= max {
        return points.to_vec();
    }
    let golden = PI * (3.0 - 5.0_f32.sqrt());
    let mut kept = std::collections::HashSet::new();
    let mut out = Vec::new();
    for i in 0..max {
        let y = 1.0 - 2.0 * (i as f32 + 0.5) / max as f32;
        let r = (1.0 - y * y).max(0.0).sqrt();
        let theta = golden * i as f32;
        let dir = Vec3::new(r * theta.cos(), y, r * theta.sin());
        let mut best = 0usize;
        let mut best_dot = f32::NEG_INFINITY;
        for (j, &p) in points.iter().enumerate() {
            let d = p.dot(dir);
            if d > best_dot {
                best_dot = d;
                best = j;
            }
        }
        if kept.insert(best) {
            out.push(points[best]);
        }
    }
    out
}

/// Voxel-grid downsample: keep, per occupied cell, the point farthest from the
/// cloud centroid so the hull's extreme points (corners/edges) survive while
/// interior density is thinned.
///
/// Returns the cloud unchanged when it already fits in `max_points` (or when
/// `max_points` is zero / the cloud is empty). The grid resolution is derived
/// from the AABB so cells are roughly cubic and the occupied-cell count tracks
/// `max_points`; because a convex hull depends only on extreme points, keeping
/// the most-extreme point per cell keeps the hull tight.
#[must_use]
pub fn decimate(points: &[[f32; 3]], max_points: usize) -> Vec<[f32; 3]> {
    if points.len() <= max_points || max_points == 0 {
        return points.to_vec();
    }

    // Axis-aligned bounds and centroid of the cloud.
    let mut min = points[0];
    let mut max = points[0];
    let mut sum = [0.0f64; 3];
    for p in points {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
            sum[k] += f64::from(p[k]);
        }
    }
    let count = usize_to_f64(points.len());
    let centroid = [sum[0] / count, sum[1] / count, sum[2] / count];
    let extent = [
        (max[0] - min[0]).max(f32::MIN_POSITIVE),
        (max[1] - min[1]).max(f32::MIN_POSITIVE),
        (max[2] - min[2]).max(f32::MIN_POSITIVE),
    ];

    // Target cells per axis so total cells ≈ max_points, weighted by extent so
    // cells stay roughly cubic. res_k = round(cbrt(N) * ext_k / geomean(ext)).
    let n = usize_to_f64(max_points).cbrt().max(1.0);
    let geo_mean = (f64::from(extent[0]) * f64::from(extent[1]) * f64::from(extent[2]))
        .cbrt()
        .max(f64::MIN_POSITIVE);
    let res = |k: usize| -> f64 { (n * f64::from(extent[k]) / geo_mean).max(1.0) };
    let (rx, ry, rz) = (res(0), res(1), res(2));

    let cell = |p: &[f32; 3]| -> (i64, i64, i64) {
        let coord = |v: f32, lo: f32, ext: f32, r: f64| -> i64 {
            // Normalised 0..1 position within the AABB → cell index 0..r-1.
            let t = f64::from((v - lo) / ext).clamp(0.0, 1.0);
            f64_floor_to_i64((t * r).min(r - 1.0))
        };
        (
            coord(p[0], min[0], extent[0], rx),
            coord(p[1], min[1], extent[1], ry),
            coord(p[2], min[2], extent[2], rz),
        )
    };
    let dist2 = |p: &[f32; 3]| -> f64 {
        (0..3)
            .map(|k| {
                let d = f64::from(p[k]) - centroid[k];
                d * d
            })
            .sum()
    };

    // Per occupied cell, keep the point farthest from the centroid (the most
    // likely hull vertex); ties keep the first seen for determinism.
    let mut seen: HashMap<(i64, i64, i64), ([f32; 3], f64)> = HashMap::new();
    for p in points {
        let d = dist2(p);
        seen.entry(cell(p))
            .and_modify(|best| {
                if d > best.1 {
                    *best = (*p, d);
                }
            })
            .or_insert((*p, d));
    }
    // Deterministic output order. `HashMap::into_values` iterates in a
    // per-instance-random order; because the collision-hull worker runs on a
    // fresh thread each click, that fed `chull` a different point order every
    // invocation and made its order-dependent `RoundOffError("concave")`
    // intermittent (the flaky collision-inspector failure). Sorting makes the
    // decimated cloud — and thus the generated hull — reproducible run to run.
    let mut out: Vec<[f32; 3]> = seen.into_values().map(|(p, _)| p).collect();
    out.sort_unstable_by(|a, b| {
        a[0].total_cmp(&b[0])
            .then(a[1].total_cmp(&b[1]))
            .then(a[2].total_cmp(&b[2]))
    });
    out
}

// ── Centralised lossy conversions (mirror idtx-render/src/convert.rs) ─────────

/// Lossless-for-small-values widen of a `usize` to `f64` (exact below 2^53;
/// point budgets never approach that).
#[inline]
#[allow(clippy::cast_precision_loss)]
fn usize_to_f64(x: usize) -> f64 {
    x as f64
}

/// Floor a finite non-negative `f64` to `i64` (grid indices, always ≥ 0 and far
/// below `i64::MAX`); saturates like `as`.
#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f64_floor_to_i64(x: f64) -> i64 {
    x.floor() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 8 corners of a unit cube, plus an interior point that must not appear
    /// on the hull.
    fn cube_cloud() -> Vec<[f32; 3]> {
        vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0], // interior
        ]
    }

    #[cfg(feature = "hull-gen")]
    #[test]
    fn cube_hull_has_eight_vertices_and_twelve_triangles() {
        let hull = generate_convex_hull(&cube_cloud(), &HullParams::default()).unwrap();
        // 8 corners survive, the interior point is dropped; a box triangulates
        // to 6 faces × 2 = 12 triangles ⇒ 36 indices.
        assert_eq!(hull.points.len(), 8);
        assert_eq!(hull.triangles.len(), 12 * 3);
        // Every index addresses a real vertex.
        assert!(hull.triangles.iter().all(|&i| usize::try_from(i).is_ok_and(|u| u < hull.points.len())));
    }

    #[cfg(feature = "hull-gen")]
    #[test]
    fn too_few_points_is_an_error() {
        assert_eq!(
            generate_convex_hull(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], &HullParams::default()),
            Err(HullError::TooFewPoints(2))
        );
    }

    #[cfg(feature = "hull-gen")]
    #[test]
    fn coplanar_cloud_fails_gracefully() {
        // Four points all on z = 0: no enclosed volume ⇒ parry returns an error
        // (or a sub-tetrahedral result), surfaced as `Compute`, not a panic.
        let flat = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        assert!(matches!(
            generate_convex_hull(&flat, &HullParams::default()),
            Err(HullError::Compute(_))
        ));
    }

    #[cfg(feature = "hull-gen")]
    #[test]
    fn thin_disc_hulls_successfully() {
        // Cookie proxy: a dense, near-planar disc of points. parry hulls it
        // reliably and deterministically (same input ⇒ same hull, every call).
        let mut pts = Vec::new();
        let segments = 128u32;
        for i in 0..segments {
            let a = f64::from(i) / f64::from(segments) * std::f64::consts::TAU;
            #[allow(clippy::cast_possible_truncation)]
            let (c, s) = (a.cos() as f32, a.sin() as f32);
            for r in [0.25f32, 0.5, 0.75, 1.0] {
                pts.push([r * c, 0.3, r * s]);
                pts.push([r * c, -0.3, r * s]);
            }
        }
        let hull = generate_convex_hull(&pts, &HullParams::default())
            .expect("a dense disc must hull");
        assert!(hull.points.len() >= 4);
        assert_eq!(hull.triangles.len() % 3, 0);
        assert!(hull.triangles.iter().all(|&i| usize::try_from(i).is_ok_and(|u| u < hull.points.len())));

        let again =
            generate_convex_hull(&pts, &HullParams::default()).expect("second hull of same cloud");
        assert_eq!(hull, again, "parry hull generation is deterministic run to run");
    }

    #[cfg(feature = "hull-gen")]
    #[test]
    fn max_hull_vertices_caps_output() {
        // A dense disc hulls to hundreds of rim vertices (the UsdCookie class).
        // The output cap must reduce it to ≤ the budget (and the default ≤48),
        // with a consistent re-triangulated hull.
        let mut pts = Vec::new();
        let segments = 256u32;
        for i in 0..segments {
            let a = f64::from(i) / f64::from(segments) * std::f64::consts::TAU;
            #[allow(clippy::cast_possible_truncation)]
            let (c, s) = (a.cos() as f32, a.sin() as f32);
            for r in [0.5f32, 1.0] {
                pts.push([r * c, 0.2, r * s]);
                pts.push([r * c, -0.2, r * s]);
            }
        }

        // A high budget leaves the dense hull large…
        let big = generate_convex_hull(
            &pts,
            &HullParams { max_input_points: 20_000, max_hull_vertices: 10_000 },
        )
        .expect("dense disc hulls");
        // …a small budget reduces it, re-hulled consistently.
        let capped = generate_convex_hull(
            &pts,
            &HullParams { max_input_points: 20_000, max_hull_vertices: 16 },
        )
        .expect("capped hull");
        assert!(big.points.len() > 48, "dense disc should exceed the default cap");
        assert!(capped.points.len() <= 16, "cap not honoured: {}", capped.points.len());
        assert!(capped.points.len() >= MIN_HULL_POINTS);
        assert!(capped.points.len() < big.points.len(), "cap must reduce vs high budget");
        assert_eq!(capped.triangles.len() % 3, 0);
        assert!(capped.triangles.iter().all(|&i| usize::try_from(i).is_ok_and(|u| u < capped.points.len())));

        // The default cap holds too.
        let def = generate_convex_hull(&pts, &HullParams::default()).expect("default hull");
        assert!(
            def.points.len() <= DEFAULT_MAX_HULL_VERTICES,
            "default cap not honoured: {}",
            def.points.len()
        );
    }

    #[test]
    fn decimation_shrinks_a_dense_grid_below_budget() {
        // A 20×20×20 grid = 8000 points; budget 500 → fewer than 8000 remain,
        // and the 8 extreme corners are preserved (one per corner cell).
        let mut dense = Vec::new();
        for x in 0u8..20 {
            for y in 0u8..20 {
                for z in 0u8..20 {
                    dense.push([f32::from(x), f32::from(y), f32::from(z)]);
                }
            }
        }
        let out = decimate(&dense, 500);
        assert!(out.len() < dense.len(), "expected decimation, got {}", out.len());
        for corner in [[0.0, 0.0, 0.0], [19.0, 19.0, 19.0]] {
            assert!(out.contains(&corner), "corner {corner:?} must survive decimation");
        }
    }

    #[test]
    fn decimation_is_a_noop_below_budget() {
        let small = cube_cloud();
        assert_eq!(decimate(&small, 1_000), small);
    }

    #[test]
    fn decimation_output_is_deterministic() {
        // Two decimations of the same dense cloud must be byte-identical. Before
        // the output sort, `HashMap::into_values` made the order (and so the
        // hull fed from it on a fresh worker thread) nondeterministic across
        // calls — the root cause of the flaky `RoundOffError("concave")` in the
        // collision inspector. Without the sort this assertion fails: two
        // `HashMap`s iterate their entries in different orders.
        let mut dense = Vec::new();
        for x in 0u8..20 {
            for y in 0u8..20 {
                for z in 0u8..20 {
                    dense.push([f32::from(x), f32::from(y), f32::from(z)]);
                }
            }
        }
        assert_eq!(
            decimate(&dense, 500),
            decimate(&dense, 500),
            "decimate must be deterministic across calls"
        );
    }
}
