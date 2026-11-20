//! Scene queries a behavior issues against the live world.
//!
//! `raycast` / `arc_cast` / `find_by_type` require the `scene_query`
//! capability (the host imports are unlinked without it, so a module that uses
//! them fails to load). On wasm they marshal POD param/result structs through
//! the host imports; natively they return misses so author logic stays
//! unit-testable without a host.
//!
//! Rays and arcs are expressed in **stage space** — the frame
//! [`crate::xr::XrHmd`] aim/HMD poses are reported in. See the session's
//! `scene_backend_impl` for the stage↔USD mapping.

use crate::abi::{
    self, ArcCastParams, ArcHitResult, RAYCAST_BLOCK_WIDGETS, RAYCAST_REQUIRE_COLLIDABLE,
    RAYCAST_RESPECT_VISIBILITY, RAYCAST_TYPE_EXCLUDE, RAYHIT_WIDGET, RayHitResult, RaycastParams,
};
use crate::entity::Entity;
use glam::Vec3;

/// Maximum prim-path length a scene query returns. Paths longer than this are
/// truncated (detectable via [`PrimPath::truncated`]).
pub const MAX_PATH_LEN: usize = 256;

/// A fixed-capacity, allocation-free prim path returned by a scene query.
#[derive(Clone, Copy)]
pub struct PrimPath {
    buf: [u8; MAX_PATH_LEN],
    /// Bytes written to `buf`.
    len: usize,
    /// True length the host reported (may exceed [`MAX_PATH_LEN`]).
    true_len: usize,
}

impl PrimPath {
    const fn empty() -> Self {
        Self {
            buf: [0; MAX_PATH_LEN],
            len: 0,
            true_len: 0,
        }
    }

    /// The path as a string slice (empty when the query missed).
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// `true` when the path was truncated to fit [`MAX_PATH_LEN`].
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.true_len > self.len
    }

    /// `true` when no path was written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl core::fmt::Debug for PrimPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Builder for the raycast/arc-cast prim filter.
///
/// Defaults: respect visibility, **only hit collidable prims** (prims with the
/// `IdtxCollisionAPI` applied and `idtx:collision:enabled`), hit any prim type,
/// any purpose, unbounded. Use [`RayOpts::include_noncollidable`] to also hit
/// prims without a collider.
#[derive(Clone, Copy, Debug)]
pub struct RayOpts {
    flags: u32,
    type_hash: u64,
    purpose_mask: u32,
    channel_mask: u32,
    max_distance: f32,
}

impl Default for RayOpts {
    fn default() -> Self {
        Self {
            // Default collision checks filter to enabled colliders; the viewport
            // editor pick path (host-side `pick_prim`) stays permissive.
            flags: RAYCAST_RESPECT_VISIBILITY | RAYCAST_REQUIRE_COLLIDABLE,
            type_hash: abi::TYPE_HASH_ANY,
            purpose_mask: 0,
            // `CHANNEL_ANY` (0) — unfiltered, matching every collider regardless
            // of its `idtx:collision:channel`. Opt into a channel below.
            channel_mask: abi::CHANNEL_ANY,
            max_distance: 0.0,
        }
    }
}

impl RayOpts {
    /// A fresh default filter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Only hit prims of USD type `token` (e.g. `"Mesh"`, `"Cube"`).
    #[must_use]
    pub fn of_type(mut self, token: &str) -> Self {
        self.type_hash = abi::type_hash(token);
        self.flags &= !RAYCAST_TYPE_EXCLUDE;
        self
    }

    /// Hit everything *except* prims of USD type `token`.
    #[must_use]
    pub fn exclude_type(mut self, token: &str) -> Self {
        self.type_hash = abi::type_hash(token);
        self.flags |= RAYCAST_TYPE_EXCLUDE;
        self
    }

    /// Only hit prims flagged collidable (`idtx:collision:enabled`). This is the
    /// **default**; call [`RayOpts::include_noncollidable`] to disable it.
    #[must_use]
    pub fn require_collidable(mut self) -> Self {
        self.flags |= RAYCAST_REQUIRE_COLLIDABLE;
        self
    }

    /// Also hit prims that have no (enabled) collider — the opposite of the
    /// default collidable-only filter.
    #[must_use]
    pub fn include_noncollidable(mut self) -> Self {
        self.flags &= !RAYCAST_REQUIRE_COLLIDABLE;
        self
    }

    /// Allow the ray to hit invisible prims (default: skip them).
    #[must_use]
    pub fn include_hidden(mut self) -> Self {
        self.flags &= !RAYCAST_RESPECT_VISIBILITY;
        self
    }

    /// Restrict to the given [`abi::PURPOSE_*`](crate::abi) mask.
    #[must_use]
    pub fn purpose(mut self, mask: u32) -> Self {
        self.purpose_mask = mask;
        self
    }

    /// Restrict to colliders answering the given [`abi::CHANNEL_*`](crate::abi)
    /// query-channel mask. [`abi::CHANNEL_ANY`] (`0`, the default) is
    /// unfiltered. A collider answers when it shares a channel bit with `mask`
    /// (an `All` collider answers every channel).
    #[must_use]
    pub fn collision_channel(mut self, mask: u32) -> Self {
        self.channel_mask = mask;
        self
    }

    /// Only hit colliders on the **interaction** channel (gaze / highlight /
    /// grab) — colliders tagged `Interaction` or `All`. Shorthand for
    /// [`collision_channel`](Self::collision_channel)`(abi::CHANNEL_INTERACTION)`.
    #[must_use]
    pub fn interaction_only(self) -> Self {
        self.collision_channel(abi::CHANNEL_INTERACTION)
    }

    /// Only hit colliders on the **physics** channel — colliders tagged
    /// `Physics` or `All`.
    #[must_use]
    pub fn physics_only(self) -> Self {
        self.collision_channel(abi::CHANNEL_PHYSICS)
    }

    /// Bound the hit distance (stage units); `<= 0` is unbounded.
    #[must_use]
    pub fn max_distance(mut self, d: f32) -> Self {
        self.max_distance = d;
        self
    }

    /// Let `IdtxWidgetPanel` quads participate in this cast so a widget occludes
    /// like any geometry (nearest hit wins). Off by default: a cast passes
    /// *through* widgets unless it opts in. A widget-nearest hit sets
    /// [`RayHit::is_widget`]; hand it to [`crate::ctx::Ctx::widgets`]'
    /// `handle` to run the built-in click processing. Only visible, in-world,
    /// non-transparent panels are ever tested.
    #[must_use]
    pub fn block_widgets(mut self) -> Self {
        self.flags |= RAYCAST_BLOCK_WIDGETS;
        self
    }
}

/// A successful raycast hit.
#[derive(Clone, Copy, Debug)]
pub struct RayHit {
    /// The hit prim's entity handle.
    pub entity: Entity,
    /// World-space hit position (stage units).
    pub position: Vec3,
    /// Distance from the ray origin to the hit (stage units).
    pub distance: f32,
    /// The hit prim's USD path.
    pub path: PrimPath,
    /// `true` when the nearest hit is an `IdtxWidgetPanel` (only possible when
    /// the cast used [`RayOpts::block_widgets`]).
    pub widget: bool,
}

impl RayHit {
    /// `true` when this hit is on a widget panel — branch to
    /// [`crate::ctx::Ctx::widgets`]'s `handle` to run widget interaction.
    #[must_use]
    pub const fn is_widget(&self) -> bool {
        self.widget
    }
}

/// Cast a ray into the scene; `None` on a miss.
#[must_use]
pub fn raycast(origin: Vec3, dir: Vec3, opts: &RayOpts) -> Option<RayHit> {
    let params = RaycastParams {
        origin: origin.into(),
        dir: dir.into(),
        type_filter_hash: opts.type_hash,
        max_distance: opts.max_distance,
        flags: opts.flags,
        purpose_mask: opts.purpose_mask,
        channel_mask: opts.channel_mask,
    };
    let mut out = [0u8; core::mem::size_of::<RayHitResult>()];
    let mut path = PrimPath::empty();
    #[cfg(target_arch = "wasm32")]
    let true_len = unsafe {
        crate::__rt::idtx_raycast(
            bytemuck::bytes_of(&params).as_ptr(),
            out.as_mut_ptr(),
            path.buf.as_mut_ptr(),
            MAX_PATH_LEN as u32,
        )
    };
    #[cfg(not(target_arch = "wasm32"))]
    let true_len = {
        let _ = &params;
        out.copy_from_slice(bytemuck::bytes_of(&RayHitResult::default()));
        0u32
    };
    let res: RayHitResult = bytemuck::pod_read_unaligned(&out);
    if res.hit == 0 {
        return None;
    }
    path.true_len = true_len as usize;
    path.len = (true_len as usize).min(MAX_PATH_LEN);
    Some(RayHit {
        entity: Entity::from_wire(res.entity),
        position: res.position.into(),
        distance: res.distance,
        path,
        widget: res.flags & RAYHIT_WIDGET != 0,
    })
}

/// Launch parameters for a ballistic arc.
#[derive(Clone, Copy, Debug)]
pub struct ArcLaunch {
    /// Launch origin (stage units).
    pub origin: Vec3,
    /// Launch direction (need not be unit length).
    pub dir: Vec3,
    /// Launch speed (stage units / second).
    pub speed: f32,
    /// Downward acceleration on world +Y (e.g. `9.81`).
    pub gravity: f32,
    /// Maximum flight time to sample (seconds).
    pub max_time: f32,
}

/// Result of an arc cast. The sampled polyline is written into the caller's
/// `out_points` buffer (stage units); `point_count` is how many were written.
#[derive(Clone, Copy, Debug)]
pub struct ArcHit {
    /// Whether the arc struck a prim.
    pub hit: bool,
    /// The hit prim's entity handle (null when `hit` is false).
    pub entity: Entity,
    /// World-space hit position (stage units).
    pub position: Vec3,
    /// Arc length from origin to the hit (stage units).
    pub distance: f32,
    /// Number of arc points written to `out_points`.
    pub point_count: usize,
    /// True number of points the host produced (may exceed the buffer).
    pub true_point_count: usize,
    /// The hit prim's USD path (empty when `hit` is false).
    pub path: PrimPath,
}

/// Cast a ballistic arc, filling `out_points` with the sampled polyline (its
/// length is the requested density) and returning the first hit.
pub fn arc_cast(launch: &ArcLaunch, opts: &RayOpts, out_points: &mut [Vec3]) -> ArcHit {
    let params = ArcCastParams {
        origin: launch.origin.into(),
        dir: launch.dir.into(),
        type_filter_hash: opts.type_hash,
        speed: launch.speed,
        gravity: launch.gravity,
        max_time: launch.max_time,
        sample_count: u32::try_from(out_points.len()).unwrap_or(u32::MAX),
        flags: opts.flags,
        purpose_mask: opts.purpose_mask,
    };
    let mut out = [0u8; core::mem::size_of::<ArcHitResult>()];
    let mut path = PrimPath::empty();
    #[cfg(target_arch = "wasm32")]
    let true_len = unsafe {
        crate::__rt::idtx_arc_cast(
            bytemuck::bytes_of(&params).as_ptr(),
            out_points.as_mut_ptr().cast::<u8>(),
            u32::try_from(out_points.len()).unwrap_or(u32::MAX),
            out.as_mut_ptr(),
            path.buf.as_mut_ptr(),
            MAX_PATH_LEN as u32,
        )
    };
    #[cfg(not(target_arch = "wasm32"))]
    let true_len = {
        let _ = (&params, &mut *out_points);
        out.copy_from_slice(bytemuck::bytes_of(&ArcHitResult::default()));
        0u32
    };
    let res: ArcHitResult = bytemuck::pod_read_unaligned(&out);
    path.true_len = true_len as usize;
    path.len = (true_len as usize).min(MAX_PATH_LEN);
    ArcHit {
        hit: res.hit != 0,
        entity: Entity::from_wire(res.hit_entity),
        position: res.hit_position.into(),
        distance: res.hit_distance,
        point_count: (res.sample_count as usize).min(out_points.len()),
        true_point_count: res.sample_count as usize,
        path,
    }
}

/// Invoke `f(path, entity)` for every prim of USD type `token` (`"Cube"`,
/// `"DomeLight"`, …). Returns the number of matches. Allocation-free.
pub fn find_by_type<F: FnMut(&str, Entity)>(token: &str, mut f: F) -> u32 {
    let hash = abi::type_hash(token);
    #[cfg(target_arch = "wasm32")]
    {
        let count = unsafe { crate::__rt::idtx_find_by_type(hash) };
        for i in 0..count {
            let mut ent = [0u8; core::mem::size_of::<crate::abi::Entity>()];
            let mut path = PrimPath::empty();
            let true_len = unsafe {
                crate::__rt::idtx_type_query_get(
                    i,
                    ent.as_mut_ptr(),
                    path.buf.as_mut_ptr(),
                    MAX_PATH_LEN as u32,
                )
            };
            if true_len == u32::MAX {
                continue; // index out of range
            }
            path.true_len = true_len as usize;
            path.len = (true_len as usize).min(MAX_PATH_LEN);
            let wire: crate::abi::Entity = bytemuck::pod_read_unaligned(&ent);
            f(path.as_str(), Entity::from_wire(wire));
        }
        count
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (hash, &mut f);
        0
    }
}

/// Resolve a hit prim up to its whole-asset collision body
/// (`IdtxCollisionBodyAPI`), or `None` when it is inside no body.
///
/// A raycast returns the exact gprim hit; call this to roll that up to the
/// interaction unit (so gaze / highlight / grab act on the whole asset). The
/// idiom is resolve-then-highlight, falling back to the hit when there is no
/// body:
///
/// ```ignore
/// if let Some(hit) = ctx.scene().raycast(o, d, &RayOpts::new().max_distance(20.0)) {
///     let target = ctx.scene().collision_body(hit.entity).unwrap_or(hit.entity);
///     ctx.highlight().highlight(target, color)?;
/// }
/// ```
///
/// Requires the `scene_query` capability; natively (off-wasm) always `None`.
#[must_use]
pub fn collision_body(entity: Entity) -> Option<Entity> {
    #[cfg(target_arch = "wasm32")]
    {
        // Zero-init so a non-Ok host return (capability denied, stale entity, no
        // backend) — which does not write the slot — decodes to `Entity::NULL`.
        let mut out = [0u8; core::mem::size_of::<crate::abi::Entity>()];
        unsafe {
            crate::__rt::idtx_scene_collision_body(entity.wire().to_bits(), out.as_mut_ptr());
        }
        let wire: crate::abi::Entity = bytemuck::pod_read_unaligned(&out);
        if wire.is_null() {
            None
        } else {
            Some(Entity::from_wire(wire))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = entity;
        None
    }
}
