//! Shared collision-shape geometry helpers for the `IdtxCollisionAPI` schema.
//!
//! The analytic collision shapes (`sphere`/`cube`/`cylinder`/`capsule`) author
//! no explicit dimensions — they are sized from the prim's local bounds. This
//! module derives the local-space centre + half-extents once so both the
//! picking narrow-phase ([`crate::picking`]) and the editor viewport wireframe
//! (`idtx-render`) agree on the exact same shape.

use glam::{Mat4, Vec3};

pub use idtx_usd::GprimAxis;
pub use idtx_usd_extension::{Approximation, CollisionChannel, CollisionShape};

/// Local-space oriented box derived from a prim's local bounds: the centre and
/// the half-extents along local X/Y/Z. Combine with the prim's
/// [`WorldTransform`](crate::components::WorldTransform) to place the shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeGeom {
    /// Local-space centre of the bounds (bounds are not assumed origin-centred).
    pub center: Vec3,
    /// Half the extent along each local axis (always ≥ 0).
    pub half_extents: Vec3,
}

impl ShapeGeom {
    /// Radius for a bounding `sphere` collider — the largest half-extent, so a
    /// `UsdGeomSphere`'s cubic ±radius bounds yield exactly its radius.
    #[must_use]
    pub fn sphere_radius(&self) -> f32 {
        self.half_extents.max_element()
    }

    /// Radius + half-height for a `cylinder`/`capsule` collider aligned to local
    /// Y (USD's default gprim axis): radius spans the X/Z extent, half-height is
    /// the Y extent.
    #[must_use]
    pub fn cylinder_radius_half_height(&self) -> (f32, f32) {
        (
            self.half_extents.x.max(self.half_extents.z),
            self.half_extents.y,
        )
    }
}

/// Derive the [`ShapeGeom`] from a prim's local-space AABB
/// (`BoundsComp::local_minimum`/`local_maximum`).
#[must_use]
pub fn collision_shape_geom(local_min: [f32; 3], local_max: [f32; 3]) -> ShapeGeom {
    let min = Vec3::from(local_min);
    let max = Vec3::from(local_max);
    ShapeGeom {
        center: (min + max) * 0.5,
        half_extents: ((max - min) * 0.5).abs(),
    }
}

/// Resolve the narrow-phase [`CollisionShape`] from the authored OpenUSD-standard
/// collider description, per the physics-collision refactor precedence:
///
/// 1. a **mesh** collider's `physics:approximation` maps
///    `convexHull`→[`CollisionShape::Hull`], `boundingSphere`→
///    [`CollisionShape::Sphere`], `boundingCube`→[`CollisionShape::Cube`], and
///    `none`/`convexDecomposition`/`meshSimplification`→[`CollisionShape::Mesh`]
///    (the latter two have no dedicated IDTX narrow-phase yet, so they collide
///    against the mesh);
/// 2. otherwise the collider gprim's own type selects the analytic shape
///    (`Sphere`/`Cube`/`Cylinder`/`Cone`/`Capsule`), defaulting to
///    [`CollisionShape::Mesh`].
///
/// The caller enforces the "`convexHull` authored but no hull data" rule
/// (treat the prim as non-colliding, never regenerate a hull) — this fn only
/// maps the token to a shape.
#[must_use]
pub fn resolve_collision_shape(
    approximation: Approximation,
    has_mesh_collision: bool,
    gprim_type: &str,
) -> CollisionShape {
    if has_mesh_collision {
        match approximation {
            Approximation::ConvexHull => CollisionShape::Hull,
            Approximation::BoundingSphere => CollisionShape::Sphere,
            Approximation::BoundingCube => CollisionShape::Cube,
            Approximation::None
            | Approximation::ConvexDecomposition
            | Approximation::MeshSimplification => CollisionShape::Mesh,
        }
    } else {
        match gprim_type {
            "Sphere" => CollisionShape::Sphere,
            "Cube" => CollisionShape::Cube,
            "Cylinder" => CollisionShape::Cylinder,
            "Cone" => CollisionShape::Cone,
            "Capsule" => CollisionShape::Capsule,
            _ => CollisionShape::Mesh,
        }
    }
}

// ── Authored per-shape dimensions ───────────────────────────────────────────

/// The authored dimensions of a `cylinder`/`cone`/`capsule` collider: the long
/// `axis`, the total `height` along it, and the `radius`. Each is `Option`;
/// `None` means "unauthored — derive from the prim's local bounds".
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AxialDims {
    pub axis: Option<GprimAxis>,
    /// Total extent along the axis (tip-to-tip), not the cylinder-segment length.
    pub height: Option<f32>,
    pub radius: Option<f32>,
}

/// The resolved analytic-collider dimensions, sourced at hydration from the
/// collider **gprim's own geometry** (`UsdGeomSphere.radius`, `UsdGeomCube.size`,
/// and the `radius`/`height`/`axis` of `UsdGeomCylinder`/`Cone`/`Capsule`).
/// Every field is `Option`: an absent dimension falls back to the prim's local
/// bounds in [`resolve_analytic`], so a `boundingSphere`/`boundingCube` mesh
/// collider (which has no analytic gprim geometry) is sized from `BoundsComp`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CollisionDims {
    /// `UsdGeomSphere.radius` of a typed Sphere collider.
    pub sphere_radius: Option<f32>,
    /// `UsdGeomCube.size` — edge length of a typed Cube collider.
    pub cube_size: Option<f32>,
    pub cylinder: AxialDims,
    pub cone: AxialDims,
    pub capsule: AxialDims,
}

/// A collision shape resolved to concrete local-space parameters, shared by the
/// picking narrow-phase ([`crate::picking`]) and the editor wireframe
/// (`idtx-render`) so both place and size the shape identically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedShape {
    /// Local-space centre of the shape (the bounds midpoint).
    pub center: Vec3,
    pub kind: ResolvedKind,
}

/// The concrete analytic shape + parameters of a [`ResolvedShape`]. Cylinder/
/// cone/capsule carry the long [`GprimAxis`]; `half_height` is half the total
/// extent along that axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResolvedKind {
    Sphere { radius: f32 },
    Cube { half_extents: Vec3 },
    Cylinder { radius: f32, half_height: f32, axis: GprimAxis },
    Cone { radius: f32, half_height: f32, axis: GprimAxis },
    Capsule { radius: f32, half_height: f32, axis: GprimAxis },
}

/// Rotation that maps the shape-local +Y axis onto `axis`. Composed into the
/// shape transform so the Y-aligned analytic tests/wireframe builders can serve
/// any axis: `shape_xf = world_xf * translate(center) * axis_local_y_rotation(axis)`.
#[must_use]
pub fn axis_local_y_rotation(axis: GprimAxis) -> Mat4 {
    use std::f32::consts::FRAC_PI_2;
    match axis {
        GprimAxis::Y => Mat4::IDENTITY,
        // +Y → +X : rotate −90° about Z.
        GprimAxis::X => Mat4::from_rotation_z(-FRAC_PI_2),
        // +Y → +Z : rotate +90° about X.
        GprimAxis::Z => Mat4::from_rotation_x(FRAC_PI_2),
    }
}

/// Resolve an analytic collision `shape` to concrete local-space parameters,
/// preferring authored [`CollisionDims`] and falling back to the bounds-derived
/// `geom` for any unauthored dimension.
///
/// Returns `None` for [`CollisionShape::Hull`] / [`CollisionShape::Mesh`] (which
/// carry no analytic form) and when a required dimension can be resolved from
/// neither the authored value nor `geom` — the caller then uses its own fallback
/// (the broad-phase AABB for picking; nothing drawn for the wireframe).
#[must_use]
pub fn resolve_analytic(
    shape: CollisionShape,
    dims: &CollisionDims,
    geom: Option<ShapeGeom>,
) -> Option<ResolvedShape> {
    let center = geom.map_or(Vec3::ZERO, |g| g.center);
    let axial = |d: &AxialDims| -> Option<(f32, f32, GprimAxis)> {
        let (gr, ghh) = geom.map(|g| g.cylinder_radius_half_height()).unzip();
        let radius = d.radius.or(gr)?;
        let half_height = d.height.map(|h| h * 0.5).or(ghh)?;
        Some((radius, half_height, d.axis.unwrap_or_default()))
    };
    let kind = match shape {
        CollisionShape::Sphere => {
            let radius = dims.sphere_radius.or_else(|| geom.map(|g| g.sphere_radius()))?;
            ResolvedKind::Sphere { radius }
        }
        CollisionShape::Cube => {
            let half_extents = dims
                .cube_size
                .map(|s| Vec3::splat(s * 0.5))
                .or_else(|| geom.map(|g| g.half_extents))?;
            ResolvedKind::Cube { half_extents }
        }
        CollisionShape::Cylinder => {
            let (radius, half_height, axis) = axial(&dims.cylinder)?;
            ResolvedKind::Cylinder { radius, half_height, axis }
        }
        CollisionShape::Cone => {
            let (radius, half_height, axis) = axial(&dims.cone)?;
            ResolvedKind::Cone { radius, half_height, axis }
        }
        CollisionShape::Capsule => {
            let (radius, half_height, axis) = axial(&dims.capsule)?;
            ResolvedKind::Capsule { radius, half_height, axis }
        }
        CollisionShape::Hull | CollisionShape::Mesh => return None,
    };
    Some(ResolvedShape { center, kind })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geom_from_symmetric_bounds() {
        let g = collision_shape_geom([-2.0, -1.0, -0.5], [2.0, 1.0, 0.5]);
        assert_eq!(g.center, Vec3::ZERO);
        assert_eq!(g.half_extents, Vec3::new(2.0, 1.0, 0.5));
        assert!((g.sphere_radius() - 2.0).abs() < 1e-6);
        let (r, hh) = g.cylinder_radius_half_height();
        assert!((r - 2.0).abs() < 1e-6 && (hh - 1.0).abs() < 1e-6);
    }

    #[test]
    fn geom_from_offset_bounds() {
        // Bounds not centred at the origin: centre tracks the midpoint.
        let g = collision_shape_geom([0.0, 0.0, 0.0], [2.0, 4.0, 2.0]);
        assert_eq!(g.center, Vec3::new(1.0, 2.0, 1.0));
        assert_eq!(g.half_extents, Vec3::new(1.0, 2.0, 1.0));
        assert!((g.sphere_radius() - 2.0).abs() < 1e-6);
        let (r, hh) = g.cylinder_radius_half_height();
        assert!((r - 1.0).abs() < 1e-6 && (hh - 2.0).abs() < 1e-6);
    }

    #[test]
    fn resolve_prefers_authored_over_bounds() {
        let geom = ShapeGeom {
            center: Vec3::new(1.0, 2.0, 3.0),
            half_extents: Vec3::new(1.0, 2.0, 1.0),
        };
        // Authored sphere radius wins over the bounds-derived 2.0.
        let dims = CollisionDims {
            sphere_radius: Some(0.25),
            ..CollisionDims::default()
        };
        let r = resolve_analytic(CollisionShape::Sphere, &dims, Some(geom));
        assert_eq!(
            r,
            Some(ResolvedShape {
                center: Vec3::new(1.0, 2.0, 3.0),
                kind: ResolvedKind::Sphere { radius: 0.25 },
            })
        );
        // Authored cylinder: height is total extent → half_height = height/2.
        let dims = CollisionDims {
            cylinder: AxialDims {
                axis: Some(GprimAxis::X),
                height: Some(10.0),
                radius: Some(3.0),
            },
            ..CollisionDims::default()
        };
        assert_eq!(
            resolve_analytic(CollisionShape::Cylinder, &dims, Some(geom)).unwrap().kind,
            ResolvedKind::Cylinder { radius: 3.0, half_height: 5.0, axis: GprimAxis::X }
        );
    }

    #[test]
    fn resolve_falls_back_to_bounds_when_unauthored() {
        let geom = ShapeGeom {
            center: Vec3::ZERO,
            half_extents: Vec3::new(2.0, 1.0, 2.0),
        };
        let dims = CollisionDims::default();
        // Sphere radius = max half-extent; cylinder from bounds, default axis Y.
        assert_eq!(
            resolve_analytic(CollisionShape::Sphere, &dims, Some(geom)).unwrap().kind,
            ResolvedKind::Sphere { radius: 2.0 }
        );
        assert_eq!(
            resolve_analytic(CollisionShape::Cone, &dims, Some(geom)).unwrap().kind,
            ResolvedKind::Cone { radius: 2.0, half_height: 1.0, axis: GprimAxis::Y }
        );
        // Hull / Mesh never resolve to an analytic form.
        assert_eq!(resolve_analytic(CollisionShape::Hull, &dims, Some(geom)), None);
        assert_eq!(resolve_analytic(CollisionShape::Mesh, &dims, Some(geom)), None);
        // No bounds and no authored dims → unresolvable.
        assert_eq!(resolve_analytic(CollisionShape::Sphere, &dims, None), None);
    }

    #[test]
    fn resolve_cube_uniform_from_authored_size() {
        let dims = CollisionDims {
            cube_size: Some(4.0),
            ..CollisionDims::default()
        };
        assert_eq!(
            resolve_analytic(CollisionShape::Cube, &dims, None).unwrap().kind,
            ResolvedKind::Cube { half_extents: Vec3::splat(2.0) }
        );
    }

    #[test]
    fn approximation_and_gprim_type_map_to_shape() {
        use CollisionShape as S;
        // Mesh collider: the approximation token drives the shape.
        assert_eq!(resolve_collision_shape(Approximation::ConvexHull, true, "Mesh"), S::Hull);
        assert_eq!(resolve_collision_shape(Approximation::BoundingSphere, true, "Mesh"), S::Sphere);
        assert_eq!(resolve_collision_shape(Approximation::BoundingCube, true, "Mesh"), S::Cube);
        assert_eq!(resolve_collision_shape(Approximation::None, true, "Mesh"), S::Mesh);
        // No IDTX narrow-phase for these → collide against the mesh.
        assert_eq!(resolve_collision_shape(Approximation::ConvexDecomposition, true, "Mesh"), S::Mesh);
        assert_eq!(resolve_collision_shape(Approximation::MeshSimplification, true, "Mesh"), S::Mesh);
        // No mesh-collision API: the gprim type selects the analytic shape.
        assert_eq!(resolve_collision_shape(Approximation::None, false, "Sphere"), S::Sphere);
        assert_eq!(resolve_collision_shape(Approximation::None, false, "Cube"), S::Cube);
        assert_eq!(resolve_collision_shape(Approximation::None, false, "Cylinder"), S::Cylinder);
        assert_eq!(resolve_collision_shape(Approximation::None, false, "Cone"), S::Cone);
        assert_eq!(resolve_collision_shape(Approximation::None, false, "Capsule"), S::Capsule);
        // Unknown / untyped gprim with no mesh-collision API → Mesh fallback.
        assert_eq!(resolve_collision_shape(Approximation::None, false, "Xform"), S::Mesh);
    }

    #[test]
    fn axis_rotation_maps_local_y_onto_axis() {
        let eps = 1e-6;
        // +Y maps to itself.
        assert!((axis_local_y_rotation(GprimAxis::Y).transform_vector3(Vec3::Y) - Vec3::Y).length() < eps);
        // +Y maps to +X / +Z.
        assert!((axis_local_y_rotation(GprimAxis::X).transform_vector3(Vec3::Y) - Vec3::X).length() < eps);
        assert!((axis_local_y_rotation(GprimAxis::Z).transform_vector3(Vec3::Y) - Vec3::Z).length() < eps);
    }
}
