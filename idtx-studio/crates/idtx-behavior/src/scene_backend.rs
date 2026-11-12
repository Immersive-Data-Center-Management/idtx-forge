//! Abstract scene-query backend (scene interaction).
//!
//! `idtx-behavior` is deliberately USD-free — it carries its own decomposed
//! bridge components ([`crate::ecs`]) and must build + unit-test headlessly,
//! without `idtx-scene` or `OpenUSD`. But the scene-interaction imports a behavior
//! wants — raycast, ballistic-arc cast, and prim-type query — need the real
//! USD-projected scene components (`PrimNode`, `MeshComp`, `BoundsComp`,
//! `WorldTransform`, …) which live in `idtx-scene`.
//!
//! So those operations are expressed here as a trait over a plain
//! `&mut bevy_ecs::World` plus [`idtx_behavior_abi`] POD, and the concrete
//! implementation lives in `idtx-studio-session` (which depends on both crates
//! and calls `idtx_scene::picking`). The session installs an
//! `Arc<dyn SceneBackend>` on the [`BehaviorHost`](crate::BehaviorHost) via
//! `set_scene_backend`; it is cloned into every behavior store's
//! [`HostState`](crate::HostState) at attach time, and the scene-query imports
//! reach it through the `WorldScope`-guarded world borrow.
//!
//! When no backend is installed (headless tests, or a host wired without a
//! session), the scene-query imports return [`ErrorCode::NotFound`] /
//! `hit == 0` rather than trapping.
//!
//! ## Coordinate space
//!
//! Rays and arcs are expressed in **USD/stage world space** — the same space
//! a behavior reads/writes transforms in. The session implementation calls
//! `pick_prim_filtered` directly on the hydrator world with no
//! `projection_basis` premultiply (unlike the viewport picker, which converts
//! from *render* space).
//!
//! [`ErrorCode::NotFound`]: idtx_behavior_abi::ErrorCode::NotFound

use bevy_ecs::entity::Entity as EcsEntity;
use bevy_ecs::world::World;
use idtx_behavior_abi::{ArcCastParams, ArcHitResult, RayHitResult, RaycastParams, Xform};

/// The result of resolving a widget raycast hit to a panel element (ABI 3).
/// Returned by [`SceneBackend::widget_hit`]; the host uses it to draw the
/// hit-point cue and route a click to the panel behavior.
#[derive(Clone, Debug, Default)]
pub struct WidgetHitInfo {
    /// Panel-relative namespace path of the element under the hit (e.g.
    /// `"Footer/QuitButton"`), or `None` when the UV lands on bare panel.
    pub element: Option<String>,
    /// Panel-local UV of the hit (top-left origin, `(0,0)..(1,1)`).
    pub uv: [f32; 2],
    /// Stage-space hit position (for the hit-point cue marker).
    pub position: glam::Vec3,
    /// Stage-space panel normal (to orient the cue), unit length.
    pub normal: glam::Vec3,
}

/// Scene interrogation a behavior host delegates to the session.
///
/// All methods take the live world the host already holds for the current hook.
/// Implementations must be side-effect free w.r.t. authored USD state — these
/// are read-only queries.
pub trait SceneBackend: Send + Sync {
    /// Cast a ray (`params`) and return the closest filtered hit. On a miss the
    /// result carries `hit == 0` and [`Entity::NULL`](idtx_behavior_abi::Entity::NULL).
    fn raycast(&self, world: &mut World, params: &RaycastParams) -> RayHitResult;

    /// Cast a ballistic arc (`params`), writing up to `out_points.len()` sampled
    /// polyline points (world space) into `out_points` and returning the first
    /// filtered hit. The result's `sample_count` is the *true* number of points
    /// produced, so a caller with a short buffer can detect truncation.
    fn arc_cast(
        &self,
        world: &mut World,
        params: &ArcCastParams,
        out_points: &mut [glam::Vec3],
    ) -> ArcHitResult;

    /// Collect every entity whose prim type matches `type_hash`
    /// ([`idtx_behavior_abi::type_hash`]) into `out`. `out` is cleared first.
    fn find_by_type(&self, world: &mut World, type_hash: u64, out: &mut Vec<EcsEntity>);

    /// The USD path of `entity`, or `None` if it has no projected prim.
    fn path_of(&self, world: &World, entity: EcsEntity) -> Option<String>;

    /// The world-space transform of `entity`, in **stage space** (the frame XR
    /// poses and raycasts use — `projection_basis * worldMatrix`), or `None`
    /// when the prim has no projected world transform.
    ///
    /// This backs the `idtx_world_xform` import's fallback for hydrated USD
    /// prims, which carry `idtx-scene`'s `WorldTransform` rather than the
    /// behavior-bridge `GlobalTransform`. Default `None` so headless/test
    /// backends need not implement it.
    fn world_xform(&self, world: &World, entity: EcsEntity) -> Option<Xform> {
        let _ = (world, entity);
        None
    }

    /// Resolve a USD prim path (`/World/Player`) to its entity, or `None` when
    /// no projected prim has that path. Backs the `idtx_entity_find` fallback
    /// for hydrated prims. Default `None`.
    fn find_by_path(&self, world: &mut World, path: &str) -> Option<EcsEntity> {
        let _ = (world, path);
        None
    }

    /// The parent entity of `entity`, or `None` at the root / when unresolved.
    /// Backs the `idtx_entity_parent` fallback for hydrated prims. Default
    /// `None`.
    fn parent(&self, world: &mut World, entity: EcsEntity) -> Option<EcsEntity> {
        let _ = (world, entity);
        None
    }

    /// The nearest strict-ancestor collision body (`IdtxCollisionBodyAPI`) of
    /// `entity`, or `None` when it is inside no body. Backs the
    /// `idtx_scene_collision_body` import so a behavior can resolve a hit gprim
    /// up to the whole-asset interaction unit. Default `None` so headless/test
    /// backends need not implement it.
    fn collision_body(&self, world: &World, entity: EcsEntity) -> Option<EcsEntity> {
        let _ = (world, entity);
        None
    }

    /// The nearest collision/rigid **physics** body (`UsdPhysicsRigidBodyAPI`) of
    /// `entity` (self or ancestor), or `None` when it is inside no rigid body.
    /// Backs the `idtx_scene_physics_body` import (physics capability) so a grab
    /// behavior resolves a hit child collider up to its simulated body root.
    /// Default `None` so headless/test backends need not implement it.
    fn physics_body(&self, world: &World, entity: EcsEntity) -> Option<EcsEntity> {
        let _ = (world, entity);
        None
    }

    /// Resolve a widget raycast hit on panel `entity` at stage-space world
    /// `position` to the element under it (ABI 3). Returns the panel-relative
    /// element path (if any), the panel-local UV, and the stage-space hit
    /// position + panel normal for the host's hit-point cue. `None` when
    /// `entity` is not a visible widget panel. Default `None` so headless/test
    /// backends need not implement widget interaction.
    fn widget_hit(
        &self,
        world: &World,
        entity: EcsEntity,
        position: glam::Vec3,
    ) -> Option<WidgetHitInfo> {
        let _ = (world, entity, position);
        None
    }
}
