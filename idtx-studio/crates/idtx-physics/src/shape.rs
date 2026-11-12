//! Engine-facing collision shapes and their mapping to `parry` `SharedShape`.
//!
//! [`PhysicsShape`] is the **pure-Rust mirror** of
//! `idtx_scene::collision::ResolvedKind` (plus `Hull` / `Trimesh` / `Plane`).
//! Keeping it here — rather than depending on `idtx-scene` — is deliberate:
//! `idtx-scene` links `idtx-usd` (the `OpenUSD` bridge), and pulling that into the
//! physics engine would force every `idtx-physics` unit test to link `OpenUSD`.
//! The USD/ECS → physics adapter that converts `ResolvedKind` into a
//! [`PhysicsShape`] lives in the runtime layer that already links the bridge.
//!
//! `parry`'s cylinder / cone / capsule primitives are **+Y aligned**; the long
//! [`Axis`] is realised by composing [`Axis::local_y_rotation`] into the
//! collider's body-local pose (mirroring `idtx_scene::collision::
//! axis_local_y_rotation`), never by baking it into the shape.

use glam::{Quat, Vec3};
use rapier3d::prelude::*;

/// Long axis for the cylinder / cone / capsule primitives — the pure-Rust mirror
/// of `idtx_usd_extension::ShapeAxis` (default +Y, USD's default gprim axis).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Axis {
    X,
    #[default]
    Y,
    Z,
}

impl Axis {
    /// Unit vector of this axis.
    #[must_use]
    pub fn vec(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }

    /// Rotation mapping the shape-local +Y axis onto this axis, so a Y-aligned
    /// `parry` primitive serves any axis. Mirrors
    /// `idtx_scene::collision::axis_local_y_rotation`.
    #[must_use]
    pub fn local_y_rotation(self) -> Quat {
        use std::f32::consts::FRAC_PI_2;
        match self {
            Self::Y => Quat::IDENTITY,
            // +Y → +X : rotate −90° about Z.
            Self::X => Quat::from_rotation_z(-FRAC_PI_2),
            // +Y → +Z : rotate +90° about X.
            Self::Z => Quat::from_rotation_x(FRAC_PI_2),
        }
    }
}

/// A collision shape the engine can build a `parry` collider from.
///
/// Analytic variants mirror `ResolvedKind`; `Hull` / `Trimesh` carry pre-baked
/// geometry (never cooked at runtime); `Plane` is the new infinite static-ground
/// half-space the plan calls for (`UsdGeomPlane` + `CollisionAPI`).
#[derive(Clone, Debug, PartialEq)]
pub enum PhysicsShape {
    Sphere {
        radius: f32,
    },
    Cuboid {
        half_extents: Vec3,
    },
    Cylinder {
        radius: f32,
        /// Half the **total** extent along `axis` (matches `ResolvedKind`).
        half_height: f32,
        axis: Axis,
    },
    Cone {
        radius: f32,
        half_height: f32,
        axis: Axis,
    },
    Capsule {
        radius: f32,
        /// Half the **total** tip-to-tip extent along `axis`.
        half_height: f32,
        axis: Axis,
    },
    /// Pre-baked convex hull (local-space points). Dynamic-safe.
    Hull {
        points: Vec<Vec3>,
    },
    /// Pre-baked triangle mesh. **Static only** (no volume/inertia).
    Trimesh {
        vertices: Vec<Vec3>,
        indices: Vec<[u32; 3]>,
    },
    /// Infinite static ground half-space with an outward (solid-side) normal.
    Plane {
        normal: Vec3,
    },
}

/// Maximum vertices kept for a convex `Hull` collider. A high-vertex scanned
/// hull (the `UsdCookie`'s ~285-vtx hull) rocks on a flip-flopping contact
/// manifold and never sleeps — jitter that damping cannot cure (desktop +
/// Quest). A direction-sampled subset at this budget rests stably at every
/// orientation (see `tests/cookie_hull_rest.rs`) and is cheaper to collide.
const HULL_MAX_VERTICES: usize = 96;

/// Reduce `points` to at most `max` well-distributed hull vertices: keep the
/// extreme vertex along each of `max` evenly-spread (Fibonacci-sphere)
/// directions. Returns the input unchanged when already within budget. The
/// result is a subset of the input (an inscribed hull), monotonic in `max`, and
/// free of the jagged artifacts grid quantization produces (which could make the
/// jitter *worse*).
#[allow(clippy::cast_precision_loss)] // `i`/`max` are tiny vertex-budget counts.
fn reduce_hull_vertices(points: &[Vec3], max: usize) -> Vec<Vec3> {
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

/// A small border radius for a convex `Hull`, as a fraction of its smallest
/// bounding-box extent (clamped to a sane absolute band). Rounding the hull by
/// this much further stabilises the resting contact manifold without visibly
/// inflating it.
fn hull_border_radius(points: &[Vec3]) -> f32 {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for &p in points {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let min_extent = (hi - lo).min_element().max(0.0);
    (min_extent * 0.02).clamp(0.001, 0.02)
}

impl PhysicsShape {
    /// Build the `parry` [`SharedShape`].
    ///
    /// Returns `None` when the shape is degenerate — an empty/too-small `Hull`
    /// (convex hull needs ≥ 4 non-coplanar points), an empty `Trimesh`, or a
    /// `Plane` with a zero normal — so the caller can drop the collider and warn
    /// rather than panic.
    ///
    /// Cylinder / cone / capsule are returned **+Y aligned**; the caller applies
    /// [`Axis::local_y_rotation`] via [`Self::collider_local_rotation`] in the
    /// collider's body-local pose.
    #[must_use]
    pub fn to_shared_shape(&self) -> Option<SharedShape> {
        match self {
            Self::Sphere { radius } => Some(SharedShape::ball(*radius)),
            Self::Cuboid { half_extents } => Some(SharedShape::cuboid(
                half_extents.x,
                half_extents.y,
                half_extents.z,
            )),
            Self::Cylinder { radius, half_height, .. } => {
                Some(SharedShape::cylinder(*half_height, *radius))
            }
            Self::Cone { radius, half_height, .. } => {
                Some(SharedShape::cone(*half_height, *radius))
            }
            Self::Capsule { radius, half_height, .. } => {
                // parry's `capsule_y` half-height is the segment half-length
                // (between the two cap centres); USD's total extent includes the
                // two radius caps.
                let segment_half = (*half_height - *radius).max(0.0);
                Some(SharedShape::capsule_y(segment_half, *radius))
            }
            Self::Hull { points } => {
                // Reduce a high-vertex scanned hull to a direction-sampled subset
                // so the resting contact manifold is stable — a ~285-vtx hull
                // rocks forever and never sleeps (jitter), even under damping
                // (confirmed desktop + Quest; see tests/cookie_hull_rest.rs). A
                // small border radius then rounds the result for extra contact
                // stability. Fall back to a sharp hull if rounding fails
                // (degenerate / too few non-coplanar points).
                let reduced = reduce_hull_vertices(points, HULL_MAX_VERTICES);
                let r = hull_border_radius(&reduced);
                SharedShape::round_convex_hull(&reduced, r)
                    .or_else(|| SharedShape::convex_hull(&reduced))
            }
            Self::Trimesh { vertices, indices } => {
                if vertices.is_empty() || indices.is_empty() {
                    return None;
                }
                SharedShape::trimesh(vertices.clone(), indices.clone()).ok()
            }
            Self::Plane { normal } => {
                let n = normal.normalize_or_zero();
                (n != Vec3::ZERO).then(|| SharedShape::halfspace(n))
            }
        }
    }

    /// The rotation to compose into the collider's body-local pose: the long-axis
    /// rotation for cylinder / cone / capsule, identity otherwise.
    #[must_use]
    pub fn collider_local_rotation(&self) -> Quat {
        match self {
            Self::Cylinder { axis, .. } | Self::Cone { axis, .. } | Self::Capsule { axis, .. } => {
                axis.local_y_rotation()
            }
            _ => Quat::IDENTITY,
        }
    }

    /// Whether this shape can back a **dynamic** rigid body. `parry` cannot derive
    /// mass properties for a `Trimesh` or a `Plane`, so a dynamic body built on
    /// one must fall back to a hull (if authored) or to static — see the plan
    /// §5.4. Primitives and hulls are dynamic-safe.
    #[must_use]
    pub fn dynamic_safe(&self) -> bool {
        !matches!(self, Self::Trimesh { .. } | Self::Plane { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_rotation_maps_local_y_onto_axis() {
        let eps = 1e-6;
        assert!((Axis::Y.local_y_rotation() * Vec3::Y - Vec3::Y).length() < eps);
        assert!((Axis::X.local_y_rotation() * Vec3::Y - Vec3::X).length() < eps);
        assert!((Axis::Z.local_y_rotation() * Vec3::Y - Vec3::Z).length() < eps);
    }

    #[test]
    fn analytic_shapes_build() {
        assert!(PhysicsShape::Sphere { radius: 0.5 }.to_shared_shape().is_some());
        assert!(
            PhysicsShape::Cuboid { half_extents: Vec3::splat(1.0) }
                .to_shared_shape()
                .is_some()
        );
        assert!(
            PhysicsShape::Cylinder { radius: 0.5, half_height: 1.0, axis: Axis::Z }
                .to_shared_shape()
                .is_some()
        );
        assert!(
            PhysicsShape::Capsule { radius: 0.5, half_height: 1.0, axis: Axis::Y }
                .to_shared_shape()
                .is_some()
        );
    }

    #[test]
    fn hull_needs_enough_points() {
        // A single point cannot form a hull.
        assert!(PhysicsShape::Hull { points: vec![Vec3::ZERO] }.to_shared_shape().is_none());
        // A unit tetrahedron does.
        let tet = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        assert!(PhysicsShape::Hull { points: tet }.to_shared_shape().is_some());
    }

    #[test]
    fn plane_requires_nonzero_normal() {
        assert!(PhysicsShape::Plane { normal: Vec3::ZERO }.to_shared_shape().is_none());
        assert!(PhysicsShape::Plane { normal: Vec3::Y }.to_shared_shape().is_some());
    }

    #[test]
    fn trimesh_and_plane_are_not_dynamic_safe() {
        assert!(!PhysicsShape::Plane { normal: Vec3::Y }.dynamic_safe());
        assert!(
            !PhysicsShape::Trimesh { vertices: vec![], indices: vec![] }.dynamic_safe()
        );
        assert!(PhysicsShape::Sphere { radius: 1.0 }.dynamic_safe());
        assert!(PhysicsShape::Hull { points: vec![] }.dynamic_safe());
    }

    #[test]
    fn empty_trimesh_is_none() {
        assert!(
            PhysicsShape::Trimesh { vertices: vec![], indices: vec![] }
                .to_shared_shape()
                .is_none()
        );
    }
}
