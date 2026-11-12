//! Viewport ray-picking for the ECS scene world.
//!
//! Two-phase selection:
//! 1. **Broad phase** — world-space AABB slab test using [`BoundsComp`].
//! 2. **Narrow phase** — Möller-Trumbore for [`MeshComp`]; analytic
//!    ray-sphere for [`SphereComp`]; AABB fallback for cubes and extended
//!    gprims (cylinder, cone, capsule).
//!
//! Only entities marked [`ViewportPickable`] are considered.

// Without `usd-extension`, the analytic collider helpers (`ray_obb` /
// `ray_cylinder` / `ray_cone` / `ray_capsule` / `ray_trisoup` + their
// `back_project` / `clamp_index` helpers) and the collision channel vocabulary
// are reachable only through the gated collision narrow-phase, so they are
// intentionally unused in that configuration. This scoped allow keeps the
// feature-off build warning-free without weakening lints in the default build.
#![cfg_attr(not(feature = "usd-extension"), allow(dead_code, unused_imports))]

use bevy_ecs::prelude::*;
use glam::{Mat4, Vec3};
use idtx_usd::RenderPurpose;

#[cfg(feature = "usd-extension")]
use crate::collision::{
    axis_local_y_rotation, collision_shape_geom, resolve_analytic, CollisionChannel,
    CollisionShape, ResolvedKind,
};
#[cfg(feature = "usd-extension")]
use crate::components::CollisionComp;
use crate::components::{
    BoundsComp, CubeComp, ExtendedGprimComp, MeshComp, PrimNode, SphereComp,
    ViewportPickable, VisibilityComp, WorldTransform,
};

// ── Scene-query vocabulary (purpose / channel masks + prim-type hash) ──────────
//
// This is the host side of the behaviour raycast wire contract. When behaviours
// are compiled in, a WASM raycast actually crosses the boundary, so picking must
// interpret `purpose_mask` / `channel_mask` / `type_filter_hash` with
// `idtx_behavior_abi`'s EXACT definitions — so we use them directly (never
// copies), making the host-side contract byte-identical, `type_hash` included,
// for all inputs.
#[cfg(feature = "behavior")]
use idtx_behavior_abi::{
    type_hash, CHANNEL_INTERACTION, CHANNEL_PHYSICS, PURPOSE_DEFAULT, PURPOSE_GUIDE, PURPOSE_PROXY,
    PURPOSE_RENDER,
};

// Behaviour-free (editor / off-screen) builds have no WASM boundary and never
// interpret a wire query, so equivalent local copies suffice for in-process
// viewport selection and cannot affect a contract that is not exercised here.
// They MUST mirror `idtx_behavior_abi`; the `wire_vocabulary_matches_contract`
// test pins both sources against the contract's hardcoded values.
#[cfg(not(feature = "behavior"))]
use wire::{
    type_hash, CHANNEL_INTERACTION, CHANNEL_PHYSICS, PURPOSE_DEFAULT, PURPOSE_GUIDE, PURPOSE_PROXY,
    PURPOSE_RENDER,
};
#[cfg(not(feature = "behavior"))]
mod wire {
    pub const PURPOSE_DEFAULT: u32 = 1 << 0;
    pub const PURPOSE_RENDER: u32 = 1 << 1;
    pub const PURPOSE_PROXY: u32 = 1 << 2;
    pub const PURPOSE_GUIDE: u32 = 1 << 3;
    pub const CHANNEL_PHYSICS: u32 = 1 << 0;
    pub const CHANNEL_INTERACTION: u32 = 1 << 1;

    /// 64-bit FNV-1a of a prim-type token — a verbatim copy of
    /// `idtx_behavior_abi::type_hash`. Used only in behaviour-free builds (no
    /// wire boundary); mirrors the ABI so a later behaviour build interprets the
    /// same bits.
    #[must_use]
    pub const fn type_hash(token: &str) -> u64 {
        let bytes = token.as_bytes();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        hash
    }
}

const EPSILON: f32 = 1e-6;

// ── Public API ────────────────────────────────────────────────────────────────

/// A successful viewport pick.
#[derive(Debug, Clone)]
pub struct RayHit {
    pub entity: Entity,
    /// World-space distance along `ray_dir` from `ray_origin` to the hit surface.
    pub t: f32,
}

/// Filter controlling which prims a [`pick_prim_filtered`] ray may hit.
///
/// Default is all-permissive (equivalent to [`pick_prim`]).
#[derive(Debug, Clone, Copy)]
pub struct RayFilter {
    /// Only hit prims flagged collidable via the collision API schema
    /// (`CollisionComp::enabled`); non-collidable prims pass through. Has no
    /// effect unless the `usd-extension` feature is enabled.
    pub require_collidable: bool,
    /// Skip prims whose [`VisibilityComp`] is invisible.
    pub respect_visibility: bool,
    /// 64-bit FNV-1a hash of the required/excluded prim type name
    /// (see [`type_name_hash`]); `0` means "any type" (no type filter).
    pub type_filter_hash: u64,
    /// Invert the type filter: when `true`, `type_filter_hash` *excludes* the
    /// named type instead of requiring it.
    pub type_exclude: bool,
    /// Maximum hit distance along the ray in world metres; `<= 0` is unbounded.
    pub max_distance: f32,
    /// [`RenderPurpose`] bitmask (see [`purpose_bit`]); `0` means "any purpose".
    pub purpose_mask: u32,
    /// Collision query-channel mask (mirrors the behaviour wire ABI's
    /// `CHANNEL_*`). `0` (`CHANNEL_ANY`) is unfiltered — the pre-channel
    /// behaviour. Otherwise a collider is a candidate only when its own
    /// channels (`idtx:collision:channel`) share a bit with this mask; prims
    /// without a `CollisionComp` carry an implicit all-channels tag and are
    /// never excluded by a channel filter. Has no effect unless the
    /// `usd-extension` feature is enabled.
    pub channel_mask: u32,
}

impl Default for RayFilter {
    fn default() -> Self {
        Self {
            require_collidable: false,
            respect_visibility: true,
            type_filter_hash: 0,
            type_exclude: false,
            max_distance: 0.0,
            purpose_mask: 0,
            channel_mask: 0,
        }
    }
}

/// The single [`RenderPurpose`] bit for a prim, matching the `PURPOSE_*` masks
/// of the behaviour wire ABI (so a wire `purpose_mask` filters identically).
#[must_use]
pub fn purpose_bit(purpose: RenderPurpose) -> u32 {
    match purpose {
        RenderPurpose::Default => PURPOSE_DEFAULT,
        RenderPurpose::Render => PURPOSE_RENDER,
        RenderPurpose::Proxy => PURPOSE_PROXY,
        RenderPurpose::Guide => PURPOSE_GUIDE,
    }
}

/// 64-bit FNV-1a of a prim-type token, matching the behaviour wire ABI's
/// `type_hash` so a wire `type_filter_hash` from a behavior matches
/// `PrimNode::type_name` here without marshalling the string across the boundary.
#[must_use]
pub fn type_name_hash(token: &str) -> u64 {
    type_hash(token)
}

/// Casts a ray through the ECS world and returns the closest pickable prim hit.
///
/// Only entities with [`ViewportPickable`] + [`BoundsComp`] are tested.
/// Invisible prims (via [`VisibilityComp`]) are skipped. Equivalent to
/// [`pick_prim_filtered`] with [`RayFilter::default`] (no collision-hull
/// narrow-phase, so viewport selection behaviour is unchanged).
///
/// `ray_dir` must be a unit vector.
pub fn pick_prim(world: &mut World, ray_origin: Vec3, ray_dir: Vec3) -> Option<RayHit> {
    pick_prim_impl(world, ray_origin, ray_dir, &RayFilter::default(), false)
}

/// Casts a filtered ray, honouring prim type / purpose / visibility and the
/// collision API schema (`require_collidable` + collision-hull narrow-phase).
///
/// `ray_dir` must be a unit vector. Returns the closest hit passing `filter`.
pub fn pick_prim_filtered(
    world: &mut World,
    ray_origin: Vec3,
    ray_dir: Vec3,
    filter: &RayFilter,
) -> Option<RayHit> {
    pick_prim_impl(world, ray_origin, ray_dir, filter, true)
}

fn pick_prim_impl(
    world: &mut World,
    ray_origin: Vec3,
    ray_dir: Vec3,
    filter: &RayFilter,
    use_hull: bool,
) -> Option<RayHit> {
    // Phase 1: AABB broad phase — collect candidates passing the filter.
    //
    // Two interchangeable enumerations feed the SAME component-filter predicate
    // ([`candidate_passes`]) and the SAME leaf slab test ([`ray_aabb`]), so the
    // candidate set is independent of which spatial enumeration ran:
    //   * [`crate::collision_bvh::CollisionBvh`] resource present (feature
    //     `collision-bvh`) → BVH ray traversal, or
    //   * otherwise → linear scan over every `ViewportPickable + BoundsComp`.
    let candidates: Vec<(Entity, f32)> = {
        #[cfg(feature = "collision-bvh")]
        let bvh_candidates: Option<Vec<(Entity, f32)>> = world
            .get_resource::<crate::collision_bvh::CollisionBvh>()
            .map(|bvh| bvh.query_ray(ray_origin, ray_dir, filter.max_distance));
        #[cfg(not(feature = "collision-bvh"))]
        let bvh_candidates: Option<Vec<(Entity, f32)>> = None;

        if let Some(spatial) = bvh_candidates {
            // BVH path: the slab test + max_distance was applied in `query_ray`;
            // apply the component filters here (immutable `world` reads only).
            spatial
                .into_iter()
                .filter(|(entity, _)| {
                    candidate_passes(
                        world.get::<VisibilityComp>(*entity),
                        world.get::<PrimNode>(*entity),
                        #[cfg(feature = "usd-extension")]
                        world.get::<CollisionComp>(*entity),
                        filter,
                    )
                })
                .collect()
        } else {
            // Linear path: the query requires &mut World to register access.
            #[cfg(feature = "usd-extension")]
            let mut q = world.query_filtered::<
                (
                    Entity,
                    &BoundsComp,
                    Option<&VisibilityComp>,
                    Option<&PrimNode>,
                    Option<&CollisionComp>,
                ),
                With<ViewportPickable>,
            >();
            #[cfg(feature = "usd-extension")]
            let candidates = q.iter(world)
                .filter(|(_, _, vis, node, col)| {
                    candidate_passes(*vis, *node, *col, filter)
                })
                .filter_map(|(entity, bounds, _, _, _)| {
                    let t = ray_aabb(
                        ray_origin,
                        ray_dir,
                        bounds.minimum.into(),
                        bounds.maximum.into(),
                    )?;
                    if filter.max_distance > 0.0 && t > filter.max_distance {
                        return None;
                    }
                    Some((entity, t))
                })
                .collect();
            #[cfg(not(feature = "usd-extension"))]
            let mut q = world.query_filtered::<
                (
                    Entity,
                    &BoundsComp,
                    Option<&VisibilityComp>,
                    Option<&PrimNode>,
                ),
                With<ViewportPickable>,
            >();
            #[cfg(not(feature = "usd-extension"))]
            let candidates = q.iter(world)
                .filter(|(_, _, vis, node)| {
                    candidate_passes(*vis, *node, filter)
                })
                .filter_map(|(entity, bounds, _, _)| {
                    let t = ray_aabb(
                        ray_origin,
                        ray_dir,
                        bounds.minimum.into(),
                        bounds.maximum.into(),
                    )?;
                    if filter.max_distance > 0.0 && t > filter.max_distance {
                        return None;
                    }
                    Some((entity, t))
                })
                .collect();
            candidates
        }
    };

    if candidates.is_empty() {
        return None;
    }

    // Sort by AABB entry t for early-exit on the best so-far bound.
    let mut candidates = candidates;
    candidates.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    // Phase 2: narrow-phase per candidate.
    let mut best: Option<RayHit> = None;
    for (entity, t_aabb) in candidates {
        if best.as_ref().is_some_and(|b| t_aabb >= b.t) {
            break; // All remaining have a farther AABB entry — done.
        }
        let t = narrow_phase_inner(world, entity, ray_origin, ray_dir, t_aabb, use_hull);
        if let Some(t) = t {
            if filter.max_distance > 0.0 && t > filter.max_distance {
                continue;
            }
            if best.as_ref().is_none_or(|b| t < b.t) {
                best = Some(RayHit { entity, t });
            }
        }
    }

    best
}

// ── Narrow phase ──────────────────────────────────────────────────────────────

/// The query-channel bits a collider answers, derived from its
/// `idtx:collision:channel` tags. An `All` tag (or no `CollisionComp` at all,
/// via the `None` default) answers every channel; an empty channel list is
/// treated as `All`. A prim without a collider therefore never fails a channel
/// filter — channels only *narrow among colliders*.
#[cfg(feature = "usd-extension")]
fn collider_channel_bits(col: Option<&CollisionComp>) -> u32 {
    let all = CHANNEL_PHYSICS | CHANNEL_INTERACTION;
    let Some(col) = col else { return all };
    if col.channels.is_empty() {
        return all;
    }
    let mut bits = 0;
    for ch in &col.channels {
        bits |= match ch {
            CollisionChannel::All => all,
            CollisionChannel::Physics => CHANNEL_PHYSICS,
            CollisionChannel::Interaction => CHANNEL_INTERACTION,
        };
    }
    bits
}

/// The component-level broad-phase predicate (visibility / purpose / type /
/// collidability / channel), shared by the linear scan and the BVH path so the
/// two enumerations always agree on which prims are candidates. Pure over the
/// optional components — no spatial test here. The collidability / channel
/// checks (and the `col` parameter) are present only with the `usd-extension`
/// feature.
pub(crate) fn candidate_passes(
    vis: Option<&VisibilityComp>,
    node: Option<&PrimNode>,
    #[cfg(feature = "usd-extension")] col: Option<&CollisionComp>,
    filter: &RayFilter,
) -> bool {
    if filter.respect_visibility && vis.is_some_and(|v| !v.visible) {
        return false;
    }
    if filter.purpose_mask != 0
        && vis.is_some_and(|v| purpose_bit(v.purpose) & filter.purpose_mask == 0)
    {
        return false;
    }
    if filter.type_filter_hash != 0
        && (node.is_some_and(|n| type_name_hash(&n.type_name) == filter.type_filter_hash)
            == filter.type_exclude)
    {
        return false;
    }
    #[cfg(feature = "usd-extension")]
    {
        if filter.require_collidable && !col.is_some_and(|c| c.enabled) {
            return false;
        }
        if filter.channel_mask != 0 && collider_channel_bits(col) & filter.channel_mask == 0 {
            return false;
        }
    }
    true
}

fn narrow_phase_inner(
    world: &World,
    entity: Entity,
    ray_origin: Vec3,
    ray_dir: Vec3,
    t_aabb: f32,
    use_hull: bool,
) -> Option<f32> {
    let xf = world
        .get::<WorldTransform>(entity)
        .map_or(Mat4::IDENTITY, |w| w.matrix);

    // Collision schema takes precedence when present and enabled — raycasts hit
    // the authored collision shape rather than the render geometry.
    #[cfg(feature = "usd-extension")]
    if use_hull
        && let Some(col) = world.get::<CollisionComp>(entity)
            && col.enabled {
                return collision_narrow_phase(world, entity, col, ray_origin, ray_dir, xf, t_aabb);
            }
            // Present but disabled: when `require_collidable` is set the candidate
            // was already filtered out; otherwise fall through to render geometry.
    #[cfg(not(feature = "usd-extension"))]
    let _ = use_hull;

    // Mesh: Möller-Trumbore on the polygon soup.
    if let Some(mesh) = world.get::<MeshComp>(entity)
        && !mesh.prototype_only {
            return ray_polysoup(
                ray_origin,
                ray_dir,
                &mesh.points,
                &mesh.face_vertex_counts,
                &mesh.face_vertex_indices,
                xf,
            );
        }

    // Sphere: analytic ray-sphere in local space.
    if let Some(sphere) = world.get::<SphereComp>(entity)
        && !sphere.prototype_only {
            return ray_sphere(ray_origin, ray_dir, sphere.radius, xf);
        }

    // Cube, ExtendedGprim, or any other geometry: accept the AABB hit.
    if world
        .get::<CubeComp>(entity)
        .is_some_and(|c| c.prototype_only)
        || world
            .get::<ExtendedGprimComp>(entity)
            .is_some_and(|e| e.prototype_only)
    {
        return None; // prototype-only — skip
    }

    Some(t_aabb)
}

/// Narrow-phase against the authored [`CollisionComp`] shape. The analytic
/// shapes resolve their dimensions from the authored [`CollisionComp::dims`],
/// falling back to the prim's local bounds ([`BoundsComp`]); `mesh` uses the
/// prim's own [`MeshComp`]; `hull` uses the collision hull triangle soup. Any
/// shape with no usable geometry (e.g. an analytic shape on a prim without
/// bounds and no authored dimensions, or an empty hull) falls back to the
/// broad-phase AABB entry `t`.
#[cfg(feature = "usd-extension")]
fn collision_narrow_phase(
    world: &World,
    entity: Entity,
    col: &CollisionComp,
    ray_origin: Vec3,
    ray_dir: Vec3,
    xf: Mat4,
    t_aabb: f32,
) -> Option<f32> {
    // Mesh / hull are triangle soups, not analytic shapes.
    match col.shape {
        CollisionShape::Mesh => {
            if let Some(mesh) = world.get::<MeshComp>(entity)
                && !mesh.prototype_only {
                    return ray_polysoup(
                        ray_origin,
                        ray_dir,
                        &mesh.points,
                        &mesh.face_vertex_counts,
                        &mesh.face_vertex_indices,
                        xf,
                    );
                }
            // shape = mesh but the prim is not a Mesh: fall back to any authored
            // hull, else the AABB.
            if !col.hull_points.is_empty() {
                return ray_trisoup(ray_origin, ray_dir, &col.hull_points, &col.hull_triangles, xf);
            }
            return Some(t_aabb);
        }
        CollisionShape::Hull => {
            return if col.hull_points.is_empty() {
                Some(t_aabb)
            } else {
                ray_trisoup(ray_origin, ray_dir, &col.hull_points, &col.hull_triangles, xf)
            };
        }
        _ => {}
    }

    // Analytic shapes: resolve dimensions (authored, else bounds-derived) and
    // test the shape-local frame, composing the axis orientation.
    let geom = world
        .get::<BoundsComp>(entity)
        .map(|b| collision_shape_geom(b.local_minimum, b.local_maximum));
    let Some(resolved) = resolve_analytic(col.shape, &col.dims, geom) else {
        return Some(t_aabb);
    };
    let centred = xf * Mat4::from_translation(resolved.center);
    match resolved.kind {
        ResolvedKind::Sphere { radius } => ray_sphere(ray_origin, ray_dir, radius, centred),
        ResolvedKind::Cube { half_extents } => ray_obb(ray_origin, ray_dir, half_extents, centred),
        ResolvedKind::Cylinder { radius, half_height, axis } => ray_cylinder(
            ray_origin,
            ray_dir,
            radius,
            half_height,
            centred * axis_local_y_rotation(axis),
        ),
        ResolvedKind::Cone { radius, half_height, axis } => ray_cone(
            ray_origin,
            ray_dir,
            radius,
            half_height,
            centred * axis_local_y_rotation(axis),
        ),
        ResolvedKind::Capsule { radius, half_height, axis } => ray_capsule(
            ray_origin,
            ray_dir,
            radius,
            half_height,
            centred * axis_local_y_rotation(axis),
        ),
    }
}

// ── AABB slab test ─────────────────────────────────────────────────────────────

/// World-space AABB slab intersection.
/// Returns the entry t ≥ 0 or `None` on miss. Returns 0.0 when origin is inside.
///
/// `pub(crate)` so the optional BVH broad-phase ([`crate::collision_bvh`]) can
/// run the *identical* leaf slab test — guaranteeing the accelerated candidate
/// set is bit-for-bit equal to the linear scan's.
pub(crate) fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = dir.recip();
    let t1 = (min - origin) * inv;
    let t2 = (max - origin) * inv;
    let t_enter = t1.min(t2).max_element();
    let t_exit = t1.max(t2).min_element();
    if t_exit < 0.0 || t_enter > t_exit + EPSILON {
        return None;
    }
    Some(t_enter.max(0.0))
}

// ── Ray-mesh (Möller-Trumbore) ─────────────────────────────────────────────────

/// Tests a ray against a polygon-soup (shared by [`MeshComp`] and the
/// collision hull) in local space. Transforms the world-space ray into local
/// space, fan-triangulates faces, then back-projects the nearest hit to world
/// space for the returned t.
fn ray_polysoup(
    ray_origin: Vec3,
    ray_dir: Vec3,
    points: &[[f32; 3]],
    face_vertex_counts: &[i32],
    face_vertex_indices: &[i32],
    world_xf: Mat4,
) -> Option<f32> {
    let inv = world_xf.inverse();
    let local_o = inv.transform_point3(ray_origin);
    let local_d = inv.transform_vector3(ray_dir); // not normalised — that's intentional

    let n_pts = points.len();
    if n_pts == 0 {
        return None;
    }

    let mut best_t_local: Option<f32> = None;
    let mut face_cursor: usize = 0;

    for &count in face_vertex_counts {
        let n = count.max(0).cast_unsigned() as usize;
        if n < 3 {
            face_cursor += n;
            continue;
        }
        let vi0 = vertex_index(face_vertex_indices, face_cursor, n_pts);
        let v0 = Vec3::from(points[vi0]);

        for k in 1..(n - 1) {
            let v1 = Vec3::from(points[vertex_index(face_vertex_indices, face_cursor + k, n_pts)]);
            let v2 =
                Vec3::from(points[vertex_index(face_vertex_indices, face_cursor + k + 1, n_pts)]);
            if let Some(t) = moller_trumbore(local_o, local_d, v0, v1, v2)
                && best_t_local.is_none_or(|b| t < b) {
                    best_t_local = Some(t);
                }
        }
        face_cursor += n;
    }

    let t_local = best_t_local?;
    let p_local = local_o + t_local * local_d;
    let p_world = world_xf.transform_point3(p_local);
    let t_world = (p_world - ray_origin).dot(ray_dir);
    (t_world > EPSILON).then_some(t_world)
}

#[inline]
fn vertex_index(face_vertex_indices: &[i32], face_cursor: usize, n_pts: usize) -> usize {
    let raw = face_vertex_indices
        .get(face_cursor)
        .copied()
        .unwrap_or(0)
        .max(0)
        .cast_unsigned() as usize;
    raw.min(n_pts - 1)
}

/// Double-sided Möller-Trumbore triangle intersection.
fn moller_trumbore(origin: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h = dir.cross(e2);
    let a = e1.dot(h);
    // Parallel-ray test, scale-RELATIVE rather than a fixed absolute epsilon.
    //
    // `a` is the scalar triple product `e1·(dir×e2)`; its magnitude scales with
    // `|e1|·|e2|·|dir|`, so a fixed absolute threshold silently rejects whole
    // meshes whose triangles are small. 3D-scanned / high-poly assets authored at
    // metre scale routinely have sub-millimetre edges (~2e-4 m): there `a` is on
    // the order of 1e-8, well under an absolute `1e-6`, so EVERY triangle looks
    // "parallel" and the mesh becomes un-pickable. Comparing `a²` against
    // `(1e-6)²·|e1|²·|e2|²·|dir|²` tests the sine of the ray/plane angle instead
    // of an absolute determinant, so it is invariant to triangle size and to the
    // (intentionally unnormalised) local-space ray scale from `ray_polysoup`.
    let a_eps_sq = 1e-12 * e1.length_squared() * e2.length_squared() * dir.length_squared();
    if a * a <= a_eps_sq {
        return None; // Ray (near-)parallel to triangle
    }
    let f = 1.0 / a;
    let s = origin - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = f * dir.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * e2.dot(q);
    (t > EPSILON).then_some(t)
}

// ── Analytic ray-sphere ───────────────────────────────────────────────────────

/// Tests a ray against a sphere of `radius` centred at the local-space origin.
/// The world transform handles translation/scale/rotation.
fn ray_sphere(ray_origin: Vec3, ray_dir: Vec3, radius: f32, world_xf: Mat4) -> Option<f32> {
    let inv = world_xf.inverse();
    let lo = inv.transform_point3(ray_origin);
    let ld = inv.transform_vector3(ray_dir);

    let a = ld.dot(ld);
    let b = 2.0 * lo.dot(ld);
    let c = lo.dot(lo) - radius * radius;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sqrt_disc = disc.sqrt();
    let t0 = (-b - sqrt_disc) / (2.0 * a);
    let t1 = (-b + sqrt_disc) / (2.0 * a);
    let t_local = if t0 > EPSILON {
        t0
    } else if t1 > EPSILON {
        t1
    } else {
        return None;
    };

    let p_local = lo + t_local * ld;
    let p_world = world_xf.transform_point3(p_local);
    let t_world = (p_world - ray_origin).dot(ray_dir);
    (t_world > EPSILON).then_some(t_world)
}

// ── Collision-shape intersections ───────────────────────────────────────────────

/// Ray against a flat triangle-index soup (the `IdtxCollisionAPI` hull:
/// `hull_points` + `hull_triangles`, 3 indices per triangle). Mirrors
/// [`ray_polysoup`] but reads a pre-triangulated index list directly.
fn ray_trisoup(
    ray_origin: Vec3,
    ray_dir: Vec3,
    points: &[[f32; 3]],
    triangles: &[i32],
    world_xf: Mat4,
) -> Option<f32> {
    let n_pts = points.len();
    if n_pts == 0 {
        return None;
    }
    let inv = world_xf.inverse();
    let local_o = inv.transform_point3(ray_origin);
    let local_d = inv.transform_vector3(ray_dir);

    let mut best_t_local: Option<f32> = None;
    for tri in triangles.chunks_exact(3) {
        let v0 = Vec3::from(points[clamp_index(tri[0], n_pts)]);
        let v1 = Vec3::from(points[clamp_index(tri[1], n_pts)]);
        let v2 = Vec3::from(points[clamp_index(tri[2], n_pts)]);
        if let Some(t) = moller_trumbore(local_o, local_d, v0, v1, v2)
            && best_t_local.is_none_or(|b| t < b) {
                best_t_local = Some(t);
            }
    }
    back_project(local_o, local_d, best_t_local?, world_xf, ray_origin, ray_dir)
}

#[inline]
fn clamp_index(raw: i32, n_pts: usize) -> usize {
    usize::try_from(raw).unwrap_or(0).min(n_pts - 1)
}

/// Project a local-space hit `t` back to a world-space distance along `ray_dir`,
/// filtering hits behind the origin. Shared by the analytic shape tests, whose
/// local ray direction is unnormalised (it carries the inverse-transform scale).
#[inline]
fn back_project(
    local_o: Vec3,
    local_d: Vec3,
    t_local: f32,
    world_xf: Mat4,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> Option<f32> {
    let p_world = world_xf.transform_point3(local_o + t_local * local_d);
    let t_world = (p_world - ray_origin).dot(ray_dir);
    (t_world > EPSILON).then_some(t_world)
}

/// Ray against an oriented box of the given local half-extents, centred at the
/// `shape_xf` origin. Reuses the AABB slab test in the shape-local frame.
fn ray_obb(ray_origin: Vec3, ray_dir: Vec3, half: Vec3, shape_xf: Mat4) -> Option<f32> {
    let inv = shape_xf.inverse();
    let local_o = inv.transform_point3(ray_origin);
    let local_d = inv.transform_vector3(ray_dir);
    let t_local = ray_aabb(local_o, local_d, -half, half)?;
    back_project(local_o, local_d, t_local, shape_xf, ray_origin, ray_dir)
}

/// Ray against a finite, capped cylinder of `radius` and `half_height` aligned
/// to the shape-local Y axis (USD's default gprim axis).
fn ray_cylinder(
    ray_origin: Vec3,
    ray_dir: Vec3,
    radius: f32,
    half_height: f32,
    shape_xf: Mat4,
) -> Option<f32> {
    let inv = shape_xf.inverse();
    let lo = inv.transform_point3(ray_origin);
    let ld = inv.transform_vector3(ray_dir);
    let mut best: Option<f32> = None;

    // Infinite-cylinder side (x² + z² = r²), limited to |y| ≤ half_height.
    let a = ld.x * ld.x + ld.z * ld.z;
    if a > EPSILON {
        let b = 2.0 * (lo.x * ld.x + lo.z * ld.z);
        let c = lo.x * lo.x + lo.z * lo.z - radius * radius;
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            for t in [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)] {
                if t > EPSILON && (lo.y + t * ld.y).abs() <= half_height {
                    best = Some(best.map_or(t, |m: f32| m.min(t)));
                }
            }
        }
    }
    // End caps (y = ±half_height disks of `radius`).
    if ld.y.abs() > EPSILON {
        for cap_y in [half_height, -half_height] {
            let t = (cap_y - lo.y) / ld.y;
            if t > EPSILON {
                let x = lo.x + t * ld.x;
                let z = lo.z + t * ld.z;
                if x * x + z * z <= radius * radius {
                    best = Some(best.map_or(t, |m: f32| m.min(t)));
                }
            }
        }
    }
    back_project(lo, ld, best?, shape_xf, ray_origin, ray_dir)
}

/// Ray against a finite, base-capped cone of base `radius` and `half_height`
/// aligned to the shape-local Y axis: apex at `+half_height·Y`, base disk of
/// `radius` at `-half_height·Y`. The cross-section radius tapers linearly from
/// `radius` at the base to `0` at the apex.
fn ray_cone(
    ray_origin: Vec3,
    ray_dir: Vec3,
    radius: f32,
    half_height: f32,
    shape_xf: Mat4,
) -> Option<f32> {
    let inv = shape_xf.inverse();
    let lo = inv.transform_point3(ray_origin);
    let ld = inv.transform_vector3(ray_dir);
    let h = half_height;
    if h <= EPSILON || radius <= EPSILON {
        return None;
    }
    // Cross-section radius r(y) = k·(h − y), with k the base-radius-over-height
    // slope. Lateral surface: x² + z² = k²·(h − y)², limited to y ∈ [−h, h].
    let k = radius / (2.0 * h);
    let k2 = k * k;
    let mut best: Option<f32> = None;

    let w0 = h - lo.y; // h − y at t = 0
    let wd = -ld.y; // d/dt of (h − y)
    let a = ld.x * ld.x + ld.z * ld.z - k2 * wd * wd;
    let b = 2.0 * (lo.x * ld.x + lo.z * ld.z) - 2.0 * k2 * w0 * wd;
    let c = lo.x * lo.x + lo.z * lo.z - k2 * w0 * w0;
    let consider = |t: f32, best: &mut Option<f32>| {
        if t > EPSILON {
            let y = lo.y + t * ld.y;
            if (-h..=h).contains(&y) {
                *best = Some(best.map_or(t, |m: f32| m.min(t)));
            }
        }
    };
    if a.abs() > EPSILON {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            consider((-b - sq) / (2.0 * a), &mut best);
            consider((-b + sq) / (2.0 * a), &mut best);
        }
    } else if b.abs() > EPSILON {
        // Ray parallel to a generator line: a single linear root.
        consider(-c / b, &mut best);
    }
    // Base cap: disk of `radius` at y = −h.
    if ld.y.abs() > EPSILON {
        let t = (-h - lo.y) / ld.y;
        if t > EPSILON {
            let x = lo.x + t * ld.x;
            let z = lo.z + t * ld.z;
            if x * x + z * z <= radius * radius {
                best = Some(best.map_or(t, |m: f32| m.min(t)));
            }
        }
    }
    back_project(lo, ld, best?, shape_xf, ray_origin, ray_dir)
}

/// Ray against a capsule of `radius` and total `half_height` aligned to the
/// shape-local Y axis: a cylinder segment of half-length `half_height - radius`
/// capped by two hemispheres of `radius`.
fn ray_capsule(
    ray_origin: Vec3,
    ray_dir: Vec3,
    radius: f32,
    half_height: f32,
    shape_xf: Mat4,
) -> Option<f32> {
    let inv = shape_xf.inverse();
    let lo = inv.transform_point3(ray_origin);
    let ld = inv.transform_vector3(ray_dir);
    let seg = (half_height - radius).max(0.0);
    let mut best: Option<f32> = None;

    // Cylindrical side, limited to the segment |y| ≤ seg.
    let a = ld.x * ld.x + ld.z * ld.z;
    if a > EPSILON {
        let b = 2.0 * (lo.x * ld.x + lo.z * ld.z);
        let c = lo.x * lo.x + lo.z * lo.z - radius * radius;
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            for t in [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)] {
                if t > EPSILON && (lo.y + t * ld.y).abs() <= seg {
                    best = Some(best.map_or(t, |m: f32| m.min(t)));
                }
            }
        }
    }
    // Two hemisphere caps, centred at y = ±seg. Only count the hit beyond the
    // segment on the matching side so the spherical parts don't double up with
    // the cylinder region.
    let qa = ld.dot(ld);
    if qa > EPSILON {
        for cap_y in [seg, -seg] {
            let center = Vec3::new(0.0, cap_y, 0.0);
            let oc = lo - center;
            let qb = 2.0 * oc.dot(ld);
            let qc = oc.dot(oc) - radius * radius;
            let disc = qb * qb - 4.0 * qa * qc;
            if disc < 0.0 {
                continue;
            }
            let sq = disc.sqrt();
            for t in [(-qb - sq) / (2.0 * qa), (-qb + sq) / (2.0 * qa)] {
                if t <= EPSILON {
                    continue;
                }
                let y = lo.y + t * ld.y;
                let beyond = if cap_y >= 0.0 { y >= seg } else { y <= -seg };
                if beyond {
                    best = Some(best.map_or(t, |m: f32| m.min(t)));
                }
            }
        }
    }
    back_project(lo, ld, best?, shape_xf, ray_origin, ray_dir)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

// The picking tests share collision-aware helpers (`spawn_pickable`'s `hull`
// param, `collider`, the `CollisionComp` imports), so the whole module is gated
// on `usd-extension`; the default build (feature on) runs every test.
#[cfg(all(test, feature = "usd-extension"))]
mod tests {
    use super::*;

    /// Pins the scene-query vocabulary against the behaviour wire contract. The
    /// imported names resolve to `idtx_behavior_abi` in behaviour builds and to
    /// the local `wire` copies otherwise, so this one test proves BOTH sources
    /// agree with the contract's hardcoded bit values and FNV-1a hashes. If
    /// either side (the ABI, or the editor-only `wire` copy) drifts, its build's
    /// run of this test fails — a deliberate, coordinated change is then required.
    #[test]
    fn wire_vocabulary_matches_contract() {
        assert_eq!(PURPOSE_DEFAULT, 1 << 0);
        assert_eq!(PURPOSE_RENDER, 1 << 1);
        assert_eq!(PURPOSE_PROXY, 1 << 2);
        assert_eq!(PURPOSE_GUIDE, 1 << 3);
        assert_eq!(CHANNEL_PHYSICS, 1 << 0);
        assert_eq!(CHANNEL_INTERACTION, 1 << 1);
        // FNV-1a/64 of representative prim-type tokens (offset 0xcbf2…2325,
        // prime 0x100000001b3) — must match idtx_behavior_abi::type_hash.
        assert_eq!(type_hash("Mesh"), 0x6db2_09ae_e4a1_8a9c);
        assert_eq!(type_hash("DomeLight"), 0x11b9_c8a6_5f09_469e);
        assert_eq!(type_hash("Xform"), 0xe582_a722_a7b3_0acd);
    }

    #[test]
    fn aabb_hit_from_outside() {
        let origin = Vec3::new(0.0, 0.0, -5.0);
        let dir = Vec3::Z;
        let t = ray_aabb(
            origin,
            dir,
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        );
        assert!(t.is_some());
        let t = t.unwrap();
        assert!((t - 4.0).abs() < 0.01, "expected ~4.0, got {t}");
    }

    #[test]
    fn aabb_miss() {
        let origin = Vec3::new(5.0, 5.0, -5.0);
        let dir = Vec3::Z;
        let t = ray_aabb(
            origin,
            dir,
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        );
        assert!(t.is_none());
    }

    #[test]
    fn aabb_origin_inside() {
        let origin = Vec3::ZERO;
        let dir = Vec3::Z;
        let t = ray_aabb(origin, dir, Vec3::splat(-1.0), Vec3::splat(1.0));
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn moller_trumbore_hit() {
        let origin = Vec3::new(0.1, 0.1, -5.0);
        let dir = Vec3::Z;
        let v0 = Vec3::new(-1.0, -1.0, 0.0);
        let v1 = Vec3::new(1.0, -1.0, 0.0);
        let v2 = Vec3::new(0.0, 1.0, 0.0);
        let t = moller_trumbore(origin, dir, v0, v1, v2);
        assert!(t.is_some(), "expected hit");
        assert!((t.unwrap() - 5.0).abs() < 0.01);
    }

    #[test]
    fn moller_trumbore_miss() {
        let origin = Vec3::new(5.0, 5.0, -5.0);
        let dir = Vec3::Z;
        let v0 = Vec3::new(-1.0, -1.0, 0.0);
        let v1 = Vec3::new(1.0, -1.0, 0.0);
        let v2 = Vec3::new(0.0, 1.0, 0.0);
        assert!(moller_trumbore(origin, dir, v0, v1, v2).is_none());
    }

    #[test]
    fn moller_trumbore_hits_sub_millimetre_triangle() {
        // Regression: a fixed absolute parallel-ray epsilon rejected whole meshes
        // whose triangles are small, because the determinant `a` scales with the
        // triangle size. A ~2e-4 m triangle (typical of a 3D-scanned asset at
        // metre scale) gives `a` ~ 1e-8 — below an absolute 1e-6 — so every
        // triangle was skipped and the mesh became un-pickable. The scale-relative
        // test must still hit here. `s` is well below the old 1e-6 threshold.
        let s = 2.0e-4_f32;
        let origin = Vec3::new(0.1 * s, 0.1 * s, -5.0);
        let dir = Vec3::Z;
        let v0 = Vec3::new(-s, -s, 0.0);
        let v1 = Vec3::new(s, -s, 0.0);
        let v2 = Vec3::new(0.0, s, 0.0);
        let t = moller_trumbore(origin, dir, v0, v1, v2);
        assert!(t.is_some(), "sub-mm triangle must still be hit, got {t:?}");
        assert!((t.unwrap() - 5.0).abs() < 0.01);
        // A genuinely parallel ray (in the triangle's z=0 plane) must still miss.
        assert!(moller_trumbore(Vec3::new(0.0, 0.0, 0.0), Vec3::X, v0, v1, v2).is_none());
    }

    #[test]
    fn polysoup_hits_finely_tessellated_mesh() {
        // World-level regression mirroring the scanned-asset failure: a mesh whose
        // triangles are sub-millimetre must be hit by `ray_polysoup` (and so by
        // `pick_prim`), not dropped by the parallel-ray guard.
        let s = 2.0e-4_f32;
        // A ±s quad at z=0 as a USD polygon soup (one quad face, fan-triangulated).
        let points = vec![[-s, -s, 0.0], [s, -s, 0.0], [s, s, 0.0], [-s, s, 0.0]];
        let t = ray_polysoup(
            Vec3::new(0.1 * s, 0.1 * s, -5.0),
            Vec3::Z,
            &points,
            &[4],
            &[0, 1, 2, 3],
            Mat4::IDENTITY,
        );
        assert!(t.is_some(), "sub-mm polysoup must be hit, got {t:?}");
        assert!((t.unwrap() - 5.0).abs() < 0.01);
    }

    #[test]
    fn sphere_hit() {
        let origin = Vec3::new(0.0, 0.0, -5.0);
        let dir = Vec3::Z;
        let t = ray_sphere(origin, dir, 1.0, Mat4::IDENTITY);
        assert!(t.is_some());
        assert!((t.unwrap() - 4.0).abs() < 0.01);
    }

    #[test]
    fn sphere_miss() {
        let origin = Vec3::new(2.0, 0.0, -5.0);
        let dir = Vec3::Z;
        assert!(ray_sphere(origin, dir, 1.0, Mat4::IDENTITY).is_none());
    }

    // ── Collision-shape intersections ──────────────────────────────────────

    #[test]
    fn obb_hit_and_miss() {
        let dir = Vec3::Z;
        // Unit box centred at origin: ray down +Z through centre hits front face.
        let t = ray_obb(
            Vec3::new(0.0, 0.0, -5.0),
            dir,
            Vec3::splat(1.0),
            Mat4::IDENTITY,
        );
        assert!((t.unwrap() - 4.0).abs() < 0.01, "expected ~4.0, got {t:?}");
        // Outside the box in x → miss.
        assert!(ray_obb(Vec3::new(3.0, 0.0, -5.0), dir, Vec3::splat(1.0), Mat4::IDENTITY).is_none());
        // A translated shape frame moves the box with it.
        let xf = Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
        assert!(ray_obb(Vec3::new(5.0, 0.0, -5.0), dir, Vec3::splat(1.0), xf).is_some());
        assert!(ray_obb(Vec3::new(0.0, 0.0, -5.0), dir, Vec3::splat(1.0), xf).is_none());
    }

    #[test]
    fn cylinder_side_cap_and_height() {
        let dir = Vec3::Z;
        // Y-aligned cylinder r=1, half-height=2. Ray through the side at y=0.
        let t = ray_cylinder(Vec3::new(0.0, 0.0, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY);
        assert!((t.unwrap() - 4.0).abs() < 0.01);
        // Ray above the finite height (y=3 > 2) → miss (no infinite cylinder).
        assert!(ray_cylinder(Vec3::new(0.0, 3.0, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY).is_none());
        // Ray straight down the axis hits the top cap at y=2.
        let t_cap = ray_cylinder(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 1.0, 2.0, Mat4::IDENTITY);
        assert!((t_cap.unwrap() - 3.0).abs() < 0.01);
    }

    #[test]
    fn capsule_hits_hemisphere_cap_beyond_segment() {
        let dir = Vec3::Z;
        // r=1, half-height=2 → cylinder segment half-length seg = 1, caps at ±1.
        // A ray at y=2.5 misses the segment but hits the top hemisphere (top of
        // the capsule reaches y = seg + r = 2).
        assert!(ray_capsule(Vec3::new(0.0, 2.5, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY).is_none());
        // At y=1.5 the top hemisphere (centre y=1, r=1) is present.
        assert!(ray_capsule(Vec3::new(0.0, 1.5, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY).is_some());
        // Through the cylindrical middle at y=0.
        let t = ray_capsule(Vec3::new(0.0, 0.0, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY);
        assert!((t.unwrap() - 4.0).abs() < 0.01);
    }

    #[test]
    fn cone_side_apex_and_base_cap() {
        let dir = Vec3::Z;
        // Y-aligned cone, base radius 1, half_height 2 → apex at y=+2, base disk
        // (r=1) at y=-2. The widest cross-section is the base.
        // Ray through the base-cap centre straight up hits the base disk at y=-2.
        let t_base = ray_cone(Vec3::new(0.0, -5.0, 0.0), Vec3::Y, 1.0, 2.0, Mat4::IDENTITY);
        assert!((t_base.unwrap() - 3.0).abs() < 0.01, "base cap at y=-2, got {t_base:?}");
        // Ray at y=-1.5 (near the base, cross-section radius ~0.875) hits the side.
        assert!(ray_cone(Vec3::new(0.0, -1.5, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY).is_some());
        // Ray above the apex (y=2.5) misses entirely.
        assert!(ray_cone(Vec3::new(0.0, 2.5, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY).is_none());
        // Ray well outside the base radius at base height misses.
        assert!(ray_cone(Vec3::new(3.0, -1.9, -5.0), dir, 1.0, 2.0, Mat4::IDENTITY).is_none());
    }

    #[test]
    fn trisoup_hits_triangle_list() {
        // Two triangles fanning a ±1 quad at z=0.
        let points = vec![
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ];
        let tris = vec![0, 1, 2, 0, 2, 3];
        let t = ray_trisoup(
            Vec3::new(0.5, 0.5, -5.0),
            Vec3::Z,
            &points,
            &tris,
            Mat4::IDENTITY,
        );
        assert!((t.unwrap() - 5.0).abs() < 0.01);
        // Outside the quad → miss.
        assert!(ray_trisoup(Vec3::new(2.0, 2.0, -5.0), Vec3::Z, &points, &tris, Mat4::IDENTITY)
            .is_none());
    }

    // ── Filtered world-level picking ───────────────────────────────────────

    use crate::collision::{CollisionChannel, CollisionShape};
    use crate::components::{
        BoundsComp, CollisionComp, MeshComp, PrimNode, VisibilityComp, ViewportPickable,
        WorldTransform,
    };
    use crate::collision::CollisionDims;
    use idtx_usd::{PrimId, PrimState, RenderPurpose, StageGeneration};

    /// A single z=0 quad spanning `[x0,x1] × [y0,y1]` as a USD polygon soup.
    fn quad(x0: f32, x1: f32, y0: f32, y1: f32) -> (Vec<[f32; 3]>, Vec<i32>, Vec<i32>) {
        (
            vec![
                [x0, y0, 0.0],
                [x1, y0, 0.0],
                [x1, y1, 0.0],
                [x0, y1, 0.0],
            ],
            vec![4],
            vec![0, 1, 2, 3],
        )
    }

    fn prim_node(slot: u32, path: &str, type_name: &str) -> PrimNode {
        PrimNode {
            prim: PrimId::from_raw_parts(slot, StageGeneration::from_raw(1)),
            path: path.to_owned(),
            parent: None,
            name: path.rsplit('/').next().unwrap_or_default().to_owned(),
            type_name: type_name.to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        }
    }

    fn mesh_comp(points: Vec<[f32; 3]>, counts: Vec<i32>, indices: Vec<i32>) -> MeshComp {
        MeshComp {
            geometry_version: 0,
            points,
            normals: Vec::new(),
            tangents: Vec::new(),
            texcoords: Vec::new(),
            face_vertex_counts: counts,
            face_vertex_indices: indices,
            display_colors: Vec::new(),
            material_path: None,
            material_subsets: Vec::new(),
            left_handed: false,
            double_sided: true,
            prototype_only: false,
            subdivision_scheme: String::new(),
            crease_indices: Vec::new(),
            crease_lengths: Vec::new(),
            crease_sharpnesses: Vec::new(),
            corner_indices: Vec::new(),
            corner_sharpnesses: Vec::new(),
        }
    }

    /// Spawn a pickable unit-ish box: a `[-1,1]²` quad at z=0 with a matching
    /// AABB, plus `PrimNode`/`VisibilityComp` and optional collision hull.
    fn spawn_pickable(
        world: &mut World,
        slot: u32,
        path: &str,
        type_name: &str,
        visible: bool,
        purpose: RenderPurpose,
        hull: Option<CollisionComp>,
    ) -> Entity {
        let (p, c, i) = quad(-1.0, 1.0, -1.0, 1.0);
        let mut e = world.spawn((
            prim_node(slot, path, type_name),
            WorldTransform {
                matrix: Mat4::IDENTITY,
            },
            BoundsComp {
                minimum: [-1.0, -1.0, -0.01],
                maximum: [1.0, 1.0, 0.01],
                local_minimum: [-1.0, -1.0, -0.01],
                local_maximum: [1.0, 1.0, 0.01],
            },
            mesh_comp(p, c, i),
            VisibilityComp { visible, purpose, casts_shadow: true },
            ViewportPickable,
        ));
        if let Some(h) = hull {
            e.insert(h);
        }
        e.id()
    }

    // A ray from z=-5 toward +Z through the given x (y = 0).
    fn ray_at_x(x: f32) -> (Vec3, Vec3) {
        (Vec3::new(x, 0.0, -5.0), Vec3::Z)
    }

    // A plain enabled mesh collider answering the given query channels.
    fn collider(channels: Vec<CollisionChannel>) -> CollisionComp {
        CollisionComp {
            enabled: true,
            shape: CollisionShape::Mesh,
            approximation: idtx_usd_extension::Approximation::default(),
            dims: CollisionDims::default(),
            hull_points: Vec::new(),
            hull_triangles: Vec::new(),
            channels,
        }
    }

    // Reposition a pickable so its ±1 box sits centred at world x.
    fn move_to_x(world: &mut World, e: Entity, x: f32) {
        world.get_mut::<WorldTransform>(e).unwrap().matrix =
            Mat4::from_translation(Vec3::new(x, 0.0, 0.0));
        let mut b = world.get_mut::<BoundsComp>(e).unwrap();
        b.minimum = [x - 1.0, -1.0, -0.01];
        b.maximum = [x + 1.0, 1.0, 0.01];
    }

    #[test]
    fn type_filter_includes_and_excludes() {
        let mut world = World::new();
        let cube = spawn_pickable(
            &mut world, 1, "/Cube", "Cube", true, RenderPurpose::Default, None,
        );
        let sphere = spawn_pickable(
            &mut world, 2, "/Sphere", "Sphere", true, RenderPurpose::Default, None,
        );
        // Separate them along x so a given ray hits exactly one.
        world.get_mut::<WorldTransform>(sphere).unwrap().matrix =
            Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
        let mut b = world.get_mut::<BoundsComp>(sphere).unwrap();
        b.minimum = [4.0, -1.0, -0.01];
        b.maximum = [6.0, 1.0, 0.01];

        // Include "Cube": ray at x=0 hits the cube, ray at x=5 hits nothing.
        let f = RayFilter {
            type_filter_hash: type_name_hash("Cube"),
            ..RayFilter::default()
        };
        let (o, d) = ray_at_x(0.0);
        assert_eq!(pick_prim_filtered(&mut world, o, d, &f).unwrap().entity, cube);
        let (o, d) = ray_at_x(5.0);
        assert!(pick_prim_filtered(&mut world, o, d, &f).is_none());

        // Exclude "Cube": ray at x=5 now hits the sphere; x=0 hits nothing.
        let f = RayFilter {
            type_filter_hash: type_name_hash("Cube"),
            type_exclude: true,
            ..RayFilter::default()
        };
        let (o, d) = ray_at_x(5.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &f).unwrap().entity,
            sphere
        );
        let (o, d) = ray_at_x(0.0);
        assert!(pick_prim_filtered(&mut world, o, d, &f).is_none());
    }

    #[test]
    fn require_collidable_skips_noncollidable() {
        let mut world = World::new();
        spawn_pickable(
            &mut world, 1, "/Plain", "Mesh", true, RenderPurpose::Default, None,
        );
        let collider = spawn_pickable(
            &mut world,
            2,
            "/Collider",
            "Mesh",
            true,
            RenderPurpose::Default,
            Some(CollisionComp {
                enabled: true,
                shape: CollisionShape::Mesh,
                approximation: idtx_usd_extension::Approximation::default(),
                dims: CollisionDims::default(),
                hull_points: Vec::new(),
                hull_triangles: Vec::new(),
                channels: vec![CollisionChannel::All],
            }),
        );
        world.get_mut::<WorldTransform>(collider).unwrap().matrix =
            Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0));
        let mut b = world.get_mut::<BoundsComp>(collider).unwrap();
        b.minimum = [4.0, -1.0, -0.01];
        b.maximum = [6.0, 1.0, 0.01];

        let f = RayFilter {
            require_collidable: true,
            ..RayFilter::default()
        };
        // x=0 (the plain prim) is passed through; x=5 (collider) is hit.
        let (o, d) = ray_at_x(0.0);
        assert!(pick_prim_filtered(&mut world, o, d, &f).is_none());
        let (o, d) = ray_at_x(5.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &f).unwrap().entity,
            collider
        );
    }

    #[test]
    fn channel_mask_filters_colliders_by_query_domain() {
        // Three enabled colliders on distinct channels, spread along x so a ray
        // at that x hits exactly one.
        let mut world = World::new();
        let interaction = spawn_pickable(
            &mut world, 1, "/I", "Mesh", true, RenderPurpose::Default,
            Some(collider(vec![CollisionChannel::Interaction])),
        );
        let physics = spawn_pickable(
            &mut world, 2, "/P", "Mesh", true, RenderPurpose::Default,
            Some(collider(vec![CollisionChannel::Physics])),
        );
        let both = spawn_pickable(
            &mut world, 3, "/A", "Mesh", true, RenderPurpose::Default,
            Some(collider(vec![CollisionChannel::All])),
        );
        move_to_x(&mut world, interaction, 0.0);
        move_to_x(&mut world, physics, 5.0);
        move_to_x(&mut world, both, -5.0);

        let interaction_mask = RayFilter {
            require_collidable: true,
            channel_mask: CHANNEL_INTERACTION,
            ..RayFilter::default()
        };
        // Interaction query: hits the Interaction collider and the `All`
        // collider, skips the Physics-only one.
        let (o, d) = ray_at_x(0.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &interaction_mask).unwrap().entity,
            interaction
        );
        let (o, d) = ray_at_x(5.0);
        assert!(pick_prim_filtered(&mut world, o, d, &interaction_mask).is_none());
        let (o, d) = ray_at_x(-5.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &interaction_mask).unwrap().entity,
            both
        );

        let physics_mask = RayFilter {
            require_collidable: true,
            channel_mask: CHANNEL_PHYSICS,
            ..RayFilter::default()
        };
        let (o, d) = ray_at_x(5.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &physics_mask).unwrap().entity,
            physics
        );
        let (o, d) = ray_at_x(0.0);
        assert!(pick_prim_filtered(&mut world, o, d, &physics_mask).is_none());

        // Zero mask (CHANNEL_ANY) is unfiltered — pre-channel behaviour: every
        // collider is reachable regardless of its channel.
        let unfiltered = RayFilter {
            require_collidable: true,
            ..RayFilter::default()
        };
        assert_eq!(unfiltered.channel_mask, 0);
        for (x, want) in [(0.0, interaction), (5.0, physics), (-5.0, both)] {
            let (o, d) = ray_at_x(x);
            assert_eq!(
                pick_prim_filtered(&mut world, o, d, &unfiltered).unwrap().entity,
                want
            );
        }
    }

    #[test]
    fn invisible_prims_are_skipped() {
        let mut world = World::new();
        spawn_pickable(
            &mut world, 1, "/Hidden", "Mesh", false, RenderPurpose::Default, None,
        );
        let (o, d) = ray_at_x(0.0);
        assert!(pick_prim_filtered(&mut world, o, d, &RayFilter::default()).is_none());
        // ...but a filter that ignores visibility still hits it.
        let f = RayFilter {
            respect_visibility: false,
            ..RayFilter::default()
        };
        assert!(pick_prim_filtered(&mut world, o, d, &f).is_some());
    }

    #[test]
    fn collision_hull_overrides_mesh_narrow_phase() {
        // Render mesh spans ±1; the collision hull is a tiny ±0.1 quad authored
        // as a flat triangle soup (two triangles fan-triangulating the quad).
        let mut world = World::new();
        let (hp, _hc, _hi) = quad(-0.1, 0.1, -0.1, 0.1);
        let e = spawn_pickable(
            &mut world,
            1,
            "/Box",
            "Mesh",
            true,
            RenderPurpose::Default,
            Some(CollisionComp {
                enabled: true,
                shape: CollisionShape::Hull,
                approximation: idtx_usd_extension::Approximation::default(),
                dims: CollisionDims::default(),
                hull_points: hp,
                hull_triangles: vec![0, 1, 2, 0, 2, 3],
                channels: vec![CollisionChannel::All],
            }),
        );

        // A ray at x=0.5 is inside the render mesh (±1) but outside the hull.
        let (o, d) = ray_at_x(0.5);
        // Unfiltered pick uses the render mesh → hit.
        assert_eq!(pick_prim(&mut world, o, d).unwrap().entity, e);
        // Filtered pick uses the collision hull → miss at x=0.5.
        assert!(pick_prim_filtered(&mut world, o, d, &RayFilter::default()).is_none());
        // ...and a hit at x=0 where the hull does cover.
        let (o, d) = ray_at_x(0.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &RayFilter::default())
                .unwrap()
                .entity,
            e
        );
    }

    #[test]
    fn collision_shape_sphere_narrow_phase_differs_from_render_mesh() {
        // A prim whose render geometry is a ±1 quad, but whose collider is an
        // analytic sphere sized from the local bounds (radius 1). A corner ray
        // passes through the quad but outside the sphere.
        let mut world = World::new();
        let e = spawn_pickable(
            &mut world,
            1,
            "/Ball",
            "Mesh",
            true,
            RenderPurpose::Default,
            Some(CollisionComp {
                enabled: true,
                shape: CollisionShape::Sphere,
                approximation: idtx_usd_extension::Approximation::default(),
                dims: CollisionDims::default(),
                hull_points: Vec::new(),
                hull_triangles: Vec::new(),
                channels: vec![CollisionChannel::All],
            }),
        );

        // Corner ray at (0.9, 0.9): inside the ±1 quad, outside the unit sphere.
        let (o, d) = (Vec3::new(0.9, 0.9, -5.0), Vec3::Z);
        // Unfiltered (render mesh) → hit.
        assert_eq!(pick_prim(&mut world, o, d).unwrap().entity, e);
        // Filtered (analytic collision sphere) → miss at the corner.
        assert!(pick_prim_filtered(&mut world, o, d, &RayFilter::default()).is_none());
        // Centre ray hits the sphere.
        let (o, d) = ray_at_x(0.0);
        assert_eq!(
            pick_prim_filtered(&mut world, o, d, &RayFilter::default())
                .unwrap()
                .entity,
            e
        );
    }

    #[test]
    fn disabled_collider_is_skipped_when_require_collidable() {
        let mut world = World::new();
        spawn_pickable(
            &mut world,
            1,
            "/Off",
            "Mesh",
            true,
            RenderPurpose::Default,
            Some(CollisionComp {
                enabled: false,
                shape: CollisionShape::Mesh,
                approximation: idtx_usd_extension::Approximation::default(),
                dims: CollisionDims::default(),
                hull_points: Vec::new(),
                hull_triangles: Vec::new(),
                channels: vec![CollisionChannel::All],
            }),
        );
        let f = RayFilter {
            require_collidable: true,
            ..RayFilter::default()
        };
        let (o, d) = ray_at_x(0.0);
        assert!(pick_prim_filtered(&mut world, o, d, &f).is_none());
    }

    #[test]
    fn max_distance_bounds_the_hit() {
        let mut world = World::new();
        spawn_pickable(
            &mut world, 1, "/Box", "Mesh", true, RenderPurpose::Default, None,
        );
        // Box is at z≈0, ray origin z=-5 → hit distance ≈ 5.
        let (o, d) = ray_at_x(0.0);
        let near = RayFilter {
            max_distance: 3.0,
            ..RayFilter::default()
        };
        assert!(pick_prim_filtered(&mut world, o, d, &near).is_none());
        let far = RayFilter {
            max_distance: 10.0,
            ..RayFilter::default()
        };
        assert!(pick_prim_filtered(&mut world, o, d, &far).is_some());
    }
}
