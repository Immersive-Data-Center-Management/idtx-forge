//! Runtime-only rigid-body physics subsystem — `UsdPhysics` → Rapier.
//!
//! This module is the USD/ECS adapter the pure-Rust [`idtx_physics`] engine
//! deliberately does **not** carry (that crate must stay `OpenUSD`-free so its
//! unit tests run on any host). It:
//!
//! 1. **detects** a `UsdPhysicsScene` prim in the hydrated stage — the sole gate
//!    that activates simulation (plan §3); no scene ⇒ this subsystem never
//!    builds and [`StudioSession::tick_physics`] is a no-op;
//! 2. **hydrates** authored `UsdPhysics*` schemas into [`BodyDesc`] /
//!    [`ColliderDesc`] values, reusing the already-resolved [`CollisionComp`]
//!    shapes and the ECS [`WorldTransform`] / [`BoundsComp`] (1d);
//! 3. steps the engine with a fixed timestep and **writes** the resulting dynamic
//!    poses back to the **session** layer as single `xformOp:transform` matrices
//!    via the bulk [`StageService::author_session_transforms`] shim (1f).
//!
//! It is compiled only under the `physics` feature, which is enabled solely by
//! `idtx-runtime-core` — the in-process editor viewport is physics-free (plan
//! §2/§3). Conductor *preview* launches the out-of-process runtime and therefore
//! *does* simulate, which is intended.
//!
//! # Scope
//!
//! Static + dynamic + kinematic single-collider bodies (the collider carries the
//! `RigidBodyAPI`, or is a bare static collider). A **compound** body whose
//! `RigidBodyAPI` sits on an ancestor of its colliders is detected and skipped
//! with a `warn!` — whole-asset grouping is not yet implemented.
//! Joints/articulations are not supported (they need relationship-read FFI).

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Vec3};
use idtx_physics::{
    classify, decompose, default_density_in_stage_units, resolve_gravity,
    transform::is_uniform_scale, world_to_local, Axis, BodyDesc, BodyKind, ColliderDesc,
    ColliderMass, PhysicsKey, PhysicsShape, PhysicsWorld, SceneParams, UpAxis,
};
use idtx_scene::{
    bevy_ecs::entity::Entity, collision_shape_geom, resolve_analytic, AnimatedAttribute, BoundsComp,
    CollisionComp, CollisionShape, PrimNode, ResolvedKind, SceneHydrator, GprimAxis, ShapeGeom,
    WorldTransform,
};
use idtx_usd::{PrimId, StageService};
use idtx_usd_extension::{
    ATTR_PHYSICS_ANGULAR_VELOCITY, ATTR_PHYSICS_DENSITY, ATTR_PHYSICS_DYNAMIC_FRICTION,
    ATTR_PHYSICS_GRAVITY_DIRECTION, ATTR_PHYSICS_GRAVITY_MAGNITUDE, ATTR_PHYSICS_KINEMATIC_ENABLED,
    ATTR_PHYSICS_MASS, ATTR_PHYSICS_RESTITUTION, ATTR_PHYSICS_RIGID_BODY_ENABLED,
    ATTR_PHYSICS_STARTS_ASLEEP, ATTR_PHYSICS_VELOCITY, PHYSICS_RIGID_BODY_SCHEMA, PHYSICS_SCENE_TYPE,
};

/// Evidence retained before topology/API hydration destroys the old membership.
/// Only new identities with no removals can take the physics-neutral spawn path.
pub(crate) struct TopologyChangeSummary {
    removed: HashSet<PrimId>,
    previously_physics: HashSet<PrimId>,
    additive: bool,
}

impl TopologyChangeSummary {
    pub(crate) fn capture(hydrator: &SceneHydrator, delta: &idtx_usd::TopologyDelta) -> Self {
        Self {
            removed: delta.removed.clone(),
            previously_physics: delta
                .upserts
                .iter()
                .map(|record| record.prim)
                .chain(delta.removed.iter().copied())
                .filter(|&prim| hydrator.prim_has_physics(prim))
                .collect(),
            additive: !delta.upserts.is_empty()
                && delta
                    .upserts
                    .iter()
                    .all(|record| hydrator.entity_for(record.prim).is_none()),
        }
    }

    pub(crate) fn is_physics_neutral_addition(&self) -> bool {
        self.additive && self.removed.is_empty() && self.previously_physics.is_empty()
    }
}

/// Degrees → radians, for the authored `UsdPhysics` angular quantities (the spec
/// authors `angularVelocity` and joint limits/drives in degrees — plan §4).
fn deg_to_rad(v: Vec3) -> Vec3 {
    v * std::f32::consts::PI / 180.0
}

// ── pure conversions (no USD) ────────────────────────────────────────────────

/// `idtx_scene::GprimAxis` → the physics engine's own [`Axis`]. A plain function
/// rather than a `From` impl: both types are foreign to this crate (orphan rule).
fn to_phys_axis(axis: GprimAxis) -> Axis {
    match axis {
        GprimAxis::X => Axis::X,
        GprimAxis::Y => Axis::Y,
        GprimAxis::Z => Axis::Z,
    }
}

/// A resolved analytic [`ResolvedKind`] (collider-local, unit scale) → a
/// [`PhysicsShape`] with the collider's uniform world scale `s` baked into its
/// dimensions (plan §5.5 — parry rigid shapes carry no scale, so it must live in
/// the shape). `half_height` is already the tip-to-tip/total extent that
/// [`PhysicsShape`] expects.
fn scaled_shape(kind: ResolvedKind, s: f32) -> PhysicsShape {
    match kind {
        ResolvedKind::Sphere { radius } => PhysicsShape::Sphere { radius: radius * s },
        ResolvedKind::Cube { half_extents } => PhysicsShape::Cuboid {
            half_extents: half_extents * s,
        },
        ResolvedKind::Cylinder { radius, half_height, axis } => PhysicsShape::Cylinder {
            radius: radius * s,
            half_height: half_height * s,
            axis: to_phys_axis(axis),
        },
        ResolvedKind::Cone { radius, half_height, axis } => PhysicsShape::Cone {
            radius: radius * s,
            half_height: half_height * s,
            axis: to_phys_axis(axis),
        },
        ResolvedKind::Capsule { radius, half_height, axis } => PhysicsShape::Capsule {
            radius: radius * s,
            half_height: half_height * s,
            axis: to_phys_axis(axis),
        },
    }
}

// ── attribute read helpers (parse the streamed `value_repr`) ─────────────────

fn repr(service: &StageService, prim: PrimId, attr: &str) -> Option<String> {
    service
        .eval_attribute(prim, attr, 0.0, true)
        .ok()
        .flatten()
        .map(|v| v.value_repr)
}

fn read_bool(service: &StageService, prim: PrimId, attr: &str) -> Option<bool> {
    match repr(service, prim, attr)?.trim() {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn read_f32(service: &StageService, prim: PrimId, attr: &str) -> Option<f32> {
    repr(service, prim, attr)?.trim().parse::<f32>().ok()
}

fn read_token(service: &StageService, prim: PrimId, attr: &str) -> Option<String> {
    let s = repr(service, prim, attr)?;
    let t = s.trim().trim_matches('"').trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// Parse a `vector3f`/`float3`/`double3` `value_repr` of the form `"(x, y, z)"`.
fn read_vec3(service: &StageService, prim: PrimId, attr: &str) -> Option<Vec3> {
    let s = repr(service, prim, attr)?;
    let inner = s.trim().trim_matches(|c| c == '(' || c == ')');
    let mut it = inner.split(',').map(|p| p.trim().parse::<f32>());
    let x = it.next()?.ok()?;
    let y = it.next()?.ok()?;
    let z = it.next()?.ok()?;
    Some(Vec3::new(x, y, z))
}

fn up_axis_from_token(token: &str) -> UpAxis {
    match token {
        "X" | "x" => UpAxis::X,
        "Z" | "z" => UpAxis::Z,
        _ => UpAxis::Y,
    }
}

/// The outward (solid-side) half-space normal for a `UsdGeomPlane` collider,
/// from its `axis` token (default `Y`). A plane with `axis = Y` is the XZ plane
/// with a `+Y` normal.
fn plane_normal(service: &StageService, prim: PrimId) -> Vec3 {
    match read_token(service, prim, "axis").as_deref() {
        Some("X") => Vec3::X,
        Some("Z") => Vec3::Z,
        _ => Vec3::Y,
    }
}

// ── per-body bookkeeping ─────────────────────────────────────────────────────

/// Session-layer transform write-back entries for a frame: `(prim_path,
/// column-major local matrix)` — the bulk `author_session_transforms` payload.
type TransformWrites = Vec<(String, [f64; 16])>;

/// `physics:startsAsleep` write-backs for a frame: `(prim_path, asleep)`, emitted
/// only on a body's sleep/wake transition to keep USD in sync with the live sim.
type SleepFlagWrites = Vec<(String, bool)>;

/// One tracked physics body: its engine key, the USD prim path to author the
/// pose back onto, the ECS entity (for the live kinematic goal + parent
/// transform), and its classification.
struct BodyEntry {
    key: PhysicsKey,
    path: String,
    entity: Entity,
    kind: BodyKind,
    /// `Some(original_kind)` while a behavior has suspended this body's native
    /// simulation for a grab — the kind to restore on release. A grab flips the
    /// body to [`BodyKind::Kinematic`] (via [`PhysicsSubsystem::set_body_simulated`])
    /// so it tracks the grabber's authored pose and still pushes dynamics; on
    /// release it returns to this stored kind:
    ///
    /// - a **dynamic** prim resumes dynamic, carrying the grab motion as a throw;
    /// - a **static** collider-only prim (its collision shape must follow the
    ///   grab so the sim reacts to the move) resumes static at the released pose.
    ///
    /// `None` when the body is simulating natively. Guards resume so a body that
    /// is *natively* kinematic (animated / `kinematicEnabled`) is never disturbed.
    grab_restore: Option<BodyKind>,
    /// The body's sleep state as of the last [`PhysicsSubsystem::collect_authoring`]
    /// pass. The write-back is gated on this: a dynamic body that is asleep (and
    /// was already asleep last frame) authors nothing — a settled scene costs zero
    /// USD writes / reprojections (plan Phase 2A). A sleep/wake transition authors
    /// one final/first pose plus the `physics:startsAsleep` flag. Seeded from the
    /// authored `physics:startsAsleep` so a body built asleep does not spuriously
    /// author on frame 1.
    last_sleeping: bool,
    configuration: BodyDesc,
    colliders: Vec<ColliderDesc>,
    /// The source collider prim(s) this body was built from — the entries
    /// `snapshot_colliders` grouped under it. Retained so an incremental
    /// value-only reconcile ([`reconcile_dirty`]) can re-snapshot just this
    /// body's colliders without a whole-stage ECS scan.
    collider_prims: Vec<PrimId>,
    /// Whether this body's root prim carries `PhysicsRigidBodyAPI` (vs. a bare
    /// static collider). Stored so [`reconcile_dirty`] can recompute `kind`
    /// without re-walking the ancestor chain.
    has_rb: bool,
}

/// The runtime physics subsystem: the owned Rapier [`PhysicsWorld`] plus the
/// per-body bookkeeping needed to feed kinematic goals and author dynamic poses
/// back. Built lazily by [`detect_and_build`] when a `UsdPhysicsScene` is found;
/// reconciled on topology/configuration changes while retaining unchanged bodies
/// and transferring live motion when an existing body's colliders change.
pub(crate) struct PhysicsSubsystem {
    world: PhysicsWorld,
    bodies: Vec<BodyEntry>,
}

impl std::fmt::Debug for PhysicsSubsystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsSubsystem")
            .field("bodies", &self.bodies.len())
            .finish_non_exhaustive()
    }
}

/// A minimal ECS snapshot of one prim, gathered immutably before any `&mut`
/// `StageService` reads so the two borrows never overlap.
#[derive(Clone)]
struct Snap {
    prim: PrimId,
    parent: Option<PrimId>,
    path: String,
    type_name: String,
    world: Mat4,
    local_bounds: Option<([f32; 3], [f32; 3])>,
    collision: Option<CollisionComp>,
}

impl PhysicsSubsystem {
    #[cfg(feature = "physics-metrics")]
    pub(crate) fn metrics(&self) -> idtx_physics::PhysicsMetrics {
        self.world.metrics()
    }

    /// `(body_count, collider_count)` for tests: a compound body contributes one
    /// body and more than one collider.
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.bodies.len(), self.world.collider_count())
    }

    /// Whether the body backing `entity` is currently asleep in the engine, or
    /// `None` when `entity` is not a tracked body. Test seam for the Phase 2A
    /// sleep gate.
    pub(crate) fn is_entity_sleeping(&self, entity: Entity) -> Option<bool> {
        let entry = self.bodies.iter().find(|b| b.entity == entity)?;
        self.world.is_body_sleeping(entry.key)
    }

    pub(crate) fn entity_velocity(&self, entity: Entity) -> Option<(Vec3, Vec3)> {
        let entry = self.bodies.iter().find(|b| b.entity == entity)?;
        self.world.body_velocity(entry.key)
    }

    /// `(awake, asleep)` over the dynamic bodies — a diagnostic for the
    /// "never comes to rest / bodies accumulate" symptom.
    pub(crate) fn dynamic_sleep_counts(&self) -> (usize, usize) {
        self.world.dynamic_sleep_counts()
    }

    /// Feed every **kinematic** body its live per-frame world pose from the ECS
    /// (set by animation / behaviors this frame) as the next kinematic goal, so
    /// pushed dynamics respond to the pose delta (plan §6.3).
    pub(crate) fn feed_kinematic(&mut self, hydrator: &SceneHydrator) {
        for entry in &self.bodies {
            if entry.kind != BodyKind::Kinematic {
                continue;
            }
            if let Some(w) = hydrator.world().get::<WorldTransform>(entry.entity) {
                self.world.set_kinematic_target(entry.key, w.matrix);
            }
        }
    }

    /// Advance the simulation by a rendered-frame delta (fixed-timestep
    /// accumulator with the spiral-of-death substep clamp).
    pub(crate) fn advance(&mut self, dt: f32) {
        self.world.advance(dt);
    }

    /// Suspend or resume simulation of the body backing `entity` — the hook a
    /// grab behavior drives through the `physics` capability so a grabbed prim can
    /// be moved by the grabber without the solver fighting (or ignoring) it.
    ///
    /// - `simulated == false` (grab): a tracked **dynamic** *or* **static** body
    ///   is flipped to kinematic. It stops integrating gravity and leaves
    ///   [`PhysicsSubsystem::collect_authoring`]'s dynamic set (physics no longer
    ///   authors it; the grabber's `set_transform` wins), yet still collides with
    ///   and pushes dynamics as its fed pose moves (via [`Self::feed_kinematic`]).
    ///   Flipping a *static* collider matters: without it a grabbed collider-only
    ///   prim's shape would stay frozen at its original pose and the sim would not
    ///   react to the move. A natively-kinematic (animated) body is left alone.
    /// - `simulated == true` (release): only a body **this method** suspended is
    ///   restored to its pre-grab kind — dynamic (carrying the grab motion as a
    ///   throw) or static (fixed at the released pose). A natively-kinematic body
    ///   is never disturbed.
    ///
    /// Returns `true` when a body changed state. A no-op (returns `false`) when
    /// `entity` is not a tracked body, or is not in the state the request needs.
    pub(crate) fn set_body_simulated(&mut self, entity: Entity, simulated: bool) -> bool {
        let key = PhysicsKey(entity.to_bits());
        let Some(entry) = self.bodies.iter_mut().find(|b| b.entity == entity) else {
            return false;
        };
        if simulated {
            // Release: restore the pre-grab kind, only if this method suspended it.
            let Some(restore) = entry.grab_restore else {
                return false;
            };
            if self.world.set_body_kind(key, restore) {
                entry.kind = restore;
                entry.grab_restore = None;
                return true;
            }
            false
        } else {
            // Grab: suspend a body that is simulating natively (not already
            // grab-suspended) and is dynamic or static. A natively-kinematic
            // (animated / kinematicEnabled) body is left to its own driver.
            if entry.grab_restore.is_some() || entry.kind == BodyKind::Kinematic {
                return false;
            }
            let original = entry.kind;
            if self.world.set_body_kind(key, BodyKind::Kinematic) {
                entry.kind = BodyKind::Kinematic;
                entry.grab_restore = Some(original);
                return true;
            }
            false
        }
    }

    /// Collect this frame's session-layer write-back for every **dynamic** body:
    /// the transform entries `(prim_path, local_matrix)` (where `local =
    /// parent_world⁻¹ · body_world` — plan §7.2) **plus** the `physics:startsAsleep`
    /// flag writes `(prim_path, asleep)` for bodies that changed sleep state this
    /// frame.
    ///
    /// Gating (plan Phase 2A): a dynamic body authors a transform only while it is
    /// awake, or on the single frame it transitions sleep state (which records the
    /// exact rest / first-motion pose once). A body asleep for a second
    /// consecutive frame authors **nothing** — so a fully settled scene produces
    /// an empty result and [`StudioSession::tick_physics`] skips the write + the
    /// re-projection entirely. Static bodies never move; kinematic bodies are
    /// authored by their driver, not physics. A sleep/wake transition also mirrors
    /// the state into USD so the authored flag tracks the live sim.
    pub(crate) fn collect_authoring(
        &mut self,
        hydrator: &SceneHydrator,
    ) -> (TransformWrites, SleepFlagWrites) {
        let mut transforms = Vec::new();
        let mut sleep_flags = Vec::new();
        for entry in &mut self.bodies {
            if entry.kind != BodyKind::Dynamic {
                continue;
            }
            let sleeping = self.world.is_body_sleeping(entry.key).unwrap_or(false);
            let edge = sleeping != entry.last_sleeping;
            if (!sleeping || edge)
                && let Some(body_world) = self.world.body_world(entry.key)
            {
                let parent_world = parent_world_of(hydrator, entry.entity);
                let local = world_to_local(parent_world, body_world);
                transforms.push((entry.path.clone(), local.to_cols_array().map(f64::from)));
            }
            if edge {
                sleep_flags.push((entry.path.clone(), sleeping));
                entry.last_sleeping = sleeping;
            }
        }
        (transforms, sleep_flags)
    }
}

/// The world transform of `entity`'s parent prim (identity for a root-level
/// body), read live from the ECS so a moved parent is honoured.
fn parent_world_of(hydrator: &SceneHydrator, entity: Entity) -> Mat4 {
    let Some(node) = hydrator.world().get::<PrimNode>(entity) else {
        return Mat4::IDENTITY;
    };
    node.parent
        .and_then(|p| hydrator.entity_for(p))
        .and_then(|pe| hydrator.world().get::<WorldTransform>(pe).map(|w| w.matrix))
        .unwrap_or(Mat4::IDENTITY)
}

/// Scan the hydrated stage for a `UsdPhysicsScene` prim and, if present, build
/// the physics world from the authored `UsdPhysics*` schemas. Returns `None`
/// when there is no scene prim (simulation stays off).
///
/// `service` is taken `&mut` for the `inspect_prim` physics-material binding
/// read; all other reads are immutable.
pub(crate) fn detect_and_build(
    service: &mut StageService,
    hydrator: &SceneHydrator,
    previous: Option<PhysicsSubsystem>,
) -> Option<PhysicsSubsystem> {
    // ── 1. Detect the scene prim (first wins; warn on multiples). ────────────
    let mut scene_prims: Vec<PrimId> = Vec::new();
    for eref in hydrator.world().iter_entities() {
        if let Some(node) = eref.get::<PrimNode>()
            && node.type_name == PHYSICS_SCENE_TYPE
            && node.state.active && node.state.loaded
        {
            scene_prims.push(node.prim);
        }
    }
    let scene_prim = *scene_prims.first()?;
    if scene_prims.len() > 1 {
        tracing::warn!(
            count = scene_prims.len(),
            "multiple UsdPhysicsScene prims; simulating the first (multi-scene is Phase 4)"
        );
    }

    // ── 2. Resolve scene params (gravity + units). ───────────────────────────
    let meta = service.metadata();
    let up_axis = up_axis_from_token(&meta.up_axis);
    // A stage scale truncated to f32 is semantically fine (small positive).
    #[allow(clippy::cast_possible_truncation)]
    let mpu = if meta.meters_per_unit > 0.0 {
        meta.meters_per_unit as f32
    } else {
        1.0
    };
    let gravity_dir = read_vec3(service, scene_prim, ATTR_PHYSICS_GRAVITY_DIRECTION)
        .unwrap_or(Vec3::ZERO);
    // The spec sentinel for "unset" magnitude is negative; absent ⇒ earth default.
    let gravity_mag = read_f32(service, scene_prim, ATTR_PHYSICS_GRAVITY_MAGNITUDE).unwrap_or(-1.0);
    let gravity = resolve_gravity(gravity_dir, gravity_mag, up_axis, mpu);
    let params = SceneParams {
        gravity,
        meters_per_unit: mpu,
        ..SceneParams::default()
    };

    // ── 3. Snapshot the ECS (immutable) before any &mut service reads. ───────
    let snaps = snapshot_colliders(hydrator);
    if snaps.is_empty() {
        tracing::debug!("UsdPhysicsScene present but no colliders hydrated yet");
    }

    // ── 4. Group colliders by the rigid body that owns them. ─────────────────
    // Each collider resolves to the nearest `RigidBodyAPI` prim (its body root),
    // or — with none — to itself as a standalone static body. Colliders sharing a
    // body form one compound body (plan §5.1). `order` preserves first-seen order
    // for deterministic logging (bodies are otherwise independent).
    let mut order: Vec<PrimId> = Vec::new();
    let mut groups: HashMap<PrimId, (bool, Vec<usize>)> = HashMap::new();
    for (i, snap) in snaps.iter().enumerate() {
        let (body_prim, has_rb) = resolve_body_prim(service, hydrator, snap);
        groups
            .entry(body_prim)
            .or_insert_with(|| {
                order.push(body_prim);
                (has_rb, Vec::new())
            })
            .1
            .push(i);
    }

    // The UsdPhysics default density (water, 1000 kg/m³) in stage units. The
    // stage's kilogramsPerUnit is not exposed by StageMetadata yet, so it is
    // assumed 1 (true for every current fixture); metersPerUnit is honoured.
    let default_density = default_density_in_stage_units(mpu, 1.0);

    // ── 5. Build one body per group. ─────────────────────────────────────────
    let mut subsystem = previous.unwrap_or_else(|| PhysicsSubsystem { world: PhysicsWorld::new(params), bodies: Vec::new() });
    subsystem.world.set_gravity(gravity);
    let mut old: HashMap<Entity, BodyEntry> = subsystem.bodies.drain(..).map(|entry| (entry.entity, entry)).collect();
    let mut world = subsystem.world;
    let mut bodies = Vec::new();
    for body_prim in order {
        let (has_rb, idxs) = &groups[&body_prim];
        let Some(body_entity) = hydrator.entity_for(body_prim) else {
            continue;
        };
        // Gather this body's collider snaps (clones, so the `&snaps` borrow is
        // released before the `&mut world` writes inside `build_body_entry`).
        let collider_snaps: Vec<Snap> = idxs.iter().map(|&i| snaps[i].clone()).collect();
        let old_entry = old.remove(&body_entity);
        if let Some(entry) = build_body_entry(
            service,
            hydrator,
            &mut world,
            body_prim,
            body_entity,
            *has_rb,
            &collider_snaps,
            default_density,
            old_entry,
        ) {
            bodies.push(entry);
        }
    }
    for entry in old.into_values() { world.remove_body(entry.key); }

    tracing::info!(bodies = bodies.len(), "built runtime physics world from UsdPhysicsScene");
    Some(PhysicsSubsystem { world, bodies })
}

/// Build (or reuse) one physics body from its resolved collider snapshot. Shared
/// by the full [`detect_and_build`] and the incremental [`reconcile_dirty`] so
/// both produce byte-identical bodies. `old_entry` is the previous [`BodyEntry`]
/// for this body (if any), used to preserve live motion/sleep state through a
/// collider/material change and to short-circuit when nothing the solver cares
/// about changed.
///
/// Returns `None` — after removing any `old_entry` body from `world` — when the
/// body has no usable collider or the engine insert fails.
#[allow(clippy::too_many_arguments)]
fn build_body_entry(
    service: &mut StageService,
    hydrator: &SceneHydrator,
    world: &mut PhysicsWorld,
    body_prim: PrimId,
    body_entity: Entity,
    has_rb: bool,
    collider_snaps: &[Snap],
    default_density: f32,
    old_entry: Option<BodyEntry>,
) -> Option<BodyEntry> {
    let old_key = old_entry.as_ref().map(|e| e.key);
    let (body_world, body_path, body_animated) = {
        let w = hydrator.world();
        (
            w.get::<WorldTransform>(body_entity).map_or(Mat4::IDENTITY, |t| t.matrix),
            w.get::<PrimNode>(body_entity).map(|n| n.path.clone()).unwrap_or_default(),
            w.get::<AnimatedAttribute>(body_entity).is_some(),
        )
    };

    let kind = if has_rb {
        let kinematic =
            read_bool(service, body_prim, ATTR_PHYSICS_KINEMATIC_ENABLED).unwrap_or(false);
        classify(rigid_body_state(service, body_prim), kinematic, body_animated)
    } else {
        // A standalone collider with no RigidBodyAPI is a static body (its
        // animation state does not make it dynamic).
        BodyKind::Static
    };

    // The body's rigid pose (scale factored out) — collider body-local poses
    // are relative to this so the body's own scale (re-composed into the
    // authored write-back) is not double-applied.
    let body_rigid_inv = {
        let d = decompose(body_world);
        Mat4::from_rotation_translation(d.pose.rotation, d.pose.translation).inverse()
    };

    // Build each child collider in the body's rigid-local frame.
    let mut colliders = Vec::new();
    for snap in collider_snaps {
        let Some((shape, center)) = resolve_shape(service, snap) else {
            tracing::warn!(path = %snap.path, "collider shape could not be resolved; skipped");
            continue;
        };
        // A dynamic body cannot carry a trimesh/plane collider (no mass or
        // inertia — plan §5.4); skip it with a warn rather than mis-simulate.
        if kind == BodyKind::Dynamic && !shape.dynamic_safe() {
            tracing::warn!(path = %snap.path, "dynamic body: non-dynamic-safe collider (trimesh/plane) skipped");
            continue;
        }

        // Collider world → body rigid-local: translation + rotation place the
        // collider within the body; its scale is baked into the shape dims.
        let rel = decompose(body_rigid_inv * snap.world);
        let (rel_scale, rel_rot, rel_trans) =
            (rel.scale, rel.pose.rotation, rel.pose.translation);
        if !scale_is_representable(&shape, rel_scale) {
            tracing::warn!(
                path = %snap.path,
                scale = ?rel_scale,
                "non-uniform scale this collider shape cannot represent; approximating (plan §5.5)"
            );
        }
        let shape = bake_scale(shape, rel_scale);
        // Shape centre is a collider-local offset: scale it, rotate into the
        // body frame, then add the collider's body-local translation.
        let local_center = rel_trans + rel_rot * (center * rel_scale);

        let (friction, restitution, material_density) =
            resolve_material(service, hydrator, snap.prim);
        let cmass = ColliderMass::resolve(
            read_f32(service, snap.prim, ATTR_PHYSICS_MASS),
            inherited_mass_density(service, hydrator, snap.prim, body_prim),
            material_density,
            default_density,
        );
        let channel_mask = snap.collision.as_ref().map_or(0, channel_mask_of);

        colliders.push(ColliderDesc {
            shape,
            local_center,
            local_rotation: rel_rot,
            density: cmass.density,
            explicit_mass: cmass.explicit_mass,
            friction,
            restitution,
            channel_mask,
        });
    }

    if colliders.is_empty() {
        // The body lost all usable colliders — drop it (and tear down any live
        // engine body), matching the full build's orphaned-old-entry removal.
        if let Some(k) = old_key {
            world.remove_body(k);
        }
        return None;
    }

    let (linvel, angvel) = if kind == BodyKind::Dynamic {
        (
            read_vec3(service, body_prim, ATTR_PHYSICS_VELOCITY).unwrap_or(Vec3::ZERO),
            deg_to_rad(
                read_vec3(service, body_prim, ATTR_PHYSICS_ANGULAR_VELOCITY).unwrap_or(Vec3::ZERO),
            ),
        )
    } else {
        (Vec3::ZERO, Vec3::ZERO)
    };

    // `physics:startsAsleep` (dynamic only) seeds the body asleep so it holds
    // its authored rest pose until a contact or grab release wakes it, and so
    // the write-back gate treats it as settled from frame 1 (plan Phase 2A).
    let starts_asleep = kind == BodyKind::Dynamic
        && read_bool(service, body_prim, ATTR_PHYSICS_STARTS_ASLEEP).unwrap_or(false);

    // Body-level `MassAPI` overrides, read on the rigid-body prim: explicit
    // mass overrides the computed total; centre-of-mass overrides the computed
    // centre. `UsdPhysicsMassAPI` leaves `centerOfMass` at a `(-inf,-inf,-inf)`
    // sentinel when unset (and `mass`/`density` at 0), so a non-finite COM is
    // dropped — honouring it would freeze the body. The full inertia-tensor
    // override (diagonalInertia/principalAxes) is a follow-up.
    let mut body = BodyDesc {
        kind,
        world: body_world,
        explicit_mass: read_f32(service, body_prim, ATTR_PHYSICS_MASS),
        explicit_com: read_vec3(service, body_prim, "physics:centerOfMass")
            .filter(|c| c.is_finite()),
        explicit_inertia: None,
        linvel,
        angvel,
        ccd: false,
        starts_asleep,
    };

    let key = PhysicsKey(body_entity.to_bits());
    let mut configuration = body.clone();
    configuration.world = Mat4::IDENTITY;
    // Sleep state is mirrored back to USD by the live sim, not a rebuild trigger.
    configuration.starts_asleep = false;
    let collider_prims: Vec<PrimId> = collider_snaps.iter().map(|s| s.prim).collect();
    let mut grab_restore = None;
    // Whether an *existing* body's collider geometry changed (vs. a brand-new body
    // or a config-only edit) — the trigger to wake resting neighbours below.
    let mut shape_changed = false;
    if let Some(mut existing) = old_entry {
        if existing.configuration == configuration && existing.colliders == colliders {
            // Nothing the solver cares about changed — keep the live body as-is,
            // only refreshing the authored path and source-collider bookkeeping.
            existing.path = body_path;
            existing.collider_prims = collider_prims;
            existing.has_rb = has_rb;
            return Some(existing);
        }
        shape_changed = existing.colliders != colliders;
        // Preserve motion through collider/material/topology updates. Explicit
        // authored velocity or kind changes still take effect.
        if existing.configuration.kind == configuration.kind {
            if let Some(pose) = world.body_world(key) { body.world = pose; }
            if let Some((linear, angular)) = world.body_velocity(key) {
                if existing.configuration.linvel == configuration.linvel { body.linvel = linear; }
                if existing.configuration.angvel == configuration.angvel { body.angvel = angular; }
            }
            body.starts_asleep = world.is_body_sleeping(key).unwrap_or(starts_asleep);
            body.kind = existing.kind;
            grab_restore = existing.grab_restore;
        }
    }
    if world.insert_body(key, &body, &colliders) {
        if shape_changed {
            // A collider's shape changed on an existing body (e.g. an `OpenExec`-driven
            // cube `size`). rapier does NOT wake a sleeping body when the collider it
            // rests on is replaced, so a body resting on this one would hang frozen in
            // mid-air. Wake the OTHER dynamic bodies so any resting neighbour
            // re-evaluates its support and settles onto — or falls off — the new
            // shape. `key` itself is excluded: waking the resized body would make the
            // solver resolve the penetration its own grown collider has with the
            // ground and kick it upward each interval until it floats.
            world.wake_dynamic_except(&[key]);
        }
        Some(BodyEntry {
            key,
            path: body_path,
            entity: body_entity,
            kind: body.kind,
            grab_restore,
            last_sleeping: body.starts_asleep,
            configuration,
            colliders,
            collider_prims,
            has_rb,
        })
    } else {
        None
    }
}

/// Build a [`Snap`] for a single collider prim from the ECS, or `None` when the
/// prim is no longer a live, collision-enabled collider. The single-prim analogue
/// of [`snapshot_colliders`], used by [`reconcile_dirty`] to re-read only a dirty
/// body's colliders without a whole-stage scan.
fn snap_prim(hydrator: &SceneHydrator, prim: PrimId) -> Option<Snap> {
    let entity = hydrator.entity_for(prim)?;
    let world = hydrator.world();
    let node = world.get::<PrimNode>(entity)?;
    if !node.state.active || !node.state.loaded {
        return None;
    }
    let collision = world.get::<CollisionComp>(entity);
    if collision.is_some_and(|c| !c.enabled) {
        return None;
    }
    if collision.is_none() {
        return None;
    }
    let wt = world.get::<WorldTransform>(entity)?;
    Some(Snap {
        prim: node.prim,
        parent: node.parent,
        path: node.path.clone(),
        type_name: node.type_name.clone(),
        world: wt.matrix,
        local_bounds: world
            .get::<BoundsComp>(entity)
            .map(|b| (b.local_minimum, b.local_maximum)),
        collision: collision.cloned(),
    })
}

/// Incrementally reconcile the physics world after a value-only geometry edit on
/// one or more collider prims (e.g. an `OpenExec`-driven cube `size`), re-baking
/// ONLY the affected bodies instead of the whole-stage [`detect_and_build`]. This
/// is the per-interval hot path: it touches neither the untouched bodies nor the
/// whole-stage ECS scan, so a size tween on one collider costs one body rebuild.
///
/// Returns `(subsystem, needs_full_rebuild)`. `needs_full_rebuild` is `true` when
/// a dirty prim maps to no tracked body (the body set changed underneath us) —
/// the caller must then fall back to [`detect_and_build`].
pub(crate) fn reconcile_dirty(
    service: &mut StageService,
    hydrator: &SceneHydrator,
    subsystem: PhysicsSubsystem,
    dirty_prims: &[PrimId],
) -> (PhysicsSubsystem, bool) {
    // Map each dirty prim to the body that owns it (its own body, or a compound
    // body whose child collider it is). A dirty prim owned by no existing body
    // means the tracked set changed → defer to a full rebuild.
    let mut dirty_bodies: HashSet<Entity> = HashSet::new();
    for &p in dirty_prims {
        let pe = hydrator.entity_for(p);
        let owner = subsystem
            .bodies
            .iter()
            .find(|b| pe == Some(b.entity) || b.collider_prims.contains(&p));
        match owner {
            Some(b) => {
                dirty_bodies.insert(b.entity);
            }
            None => return (subsystem, true),
        }
    }
    if dirty_bodies.is_empty() {
        return (subsystem, false);
    }

    // `meters_per_unit` for the default density — same source as `detect_and_build`.
    let mpu = {
        let m = service.metadata();
        if m.meters_per_unit > 0.0 { m.meters_per_unit as f32 } else { 1.0 }
    };
    let default_density = default_density_in_stage_units(mpu, 1.0);

    let mut world = subsystem.world;
    let mut new_bodies = Vec::with_capacity(subsystem.bodies.len());
    for entry in subsystem.bodies {
        if !dirty_bodies.contains(&entry.entity) {
            new_bodies.push(entry);
            continue;
        }
        let body_entity = entry.entity;
        let Some(body_prim) = hydrator.world().get::<PrimNode>(body_entity).map(|n| n.prim) else {
            // The body prim vanished from the ECS — drop it from the sim.
            world.remove_body(entry.key);
            continue;
        };
        let has_rb = entry.has_rb;
        let collider_snaps: Vec<Snap> = entry
            .collider_prims
            .iter()
            .filter_map(|&p| snap_prim(hydrator, p))
            .collect();
        if let Some(rebuilt) = build_body_entry(
            service,
            hydrator,
            &mut world,
            body_prim,
            body_entity,
            has_rb,
            &collider_snaps,
            default_density,
            Some(entry),
        ) {
            new_bodies.push(rebuilt);
        }
    }

    #[cfg(feature = "physics-metrics")]
    tracing::debug!(
        bodies = new_bodies.len(),
        dirty = dirty_bodies.len(),
        "reconciled dirty physics bodies (incremental)"
    );

    (PhysicsSubsystem { world, bodies: new_bodies }, false)
}

/// Bake a prim's per-axis world `scale` into a (possibly already axis-rotated)
/// unit-scale [`PhysicsShape`].
///
/// A `Cuboid` represents an axis-aligned non-uniform scale **exactly** — the
/// whole point of this path: a flattened ground cube becomes a thin slab, not a
/// mean-collapsed cube. `Hull` / `Trimesh` carry explicit object-space geometry,
/// so each point/vertex is scaled component-wise (exact for any axis-aligned
/// scale). Radially-symmetric primitives cannot carry a non-uniform scale in a
/// parry rigid shape, so they take the best per-dimension factor (cross-section
/// radius from the two axes perpendicular to the long axis, length from the long
/// axis itself); [`scale_is_representable`] gates the warning when that is only an
/// approximation. `Plane` is an infinite half-space with no extent — left as-is.
fn bake_scale(shape: PhysicsShape, scale: Vec3) -> PhysicsShape {
    let s = scale.abs();
    match shape {
        PhysicsShape::Cuboid { half_extents } => PhysicsShape::Cuboid {
            half_extents: half_extents * s,
        },
        PhysicsShape::Sphere { radius } => PhysicsShape::Sphere {
            radius: radius * uniform_factor(s),
        },
        PhysicsShape::Cylinder { radius, half_height, axis } => {
            let (r, h) = radial_scale(s, axis);
            PhysicsShape::Cylinder { radius: radius * r, half_height: half_height * h, axis }
        }
        PhysicsShape::Cone { radius, half_height, axis } => {
            let (r, h) = radial_scale(s, axis);
            PhysicsShape::Cone { radius: radius * r, half_height: half_height * h, axis }
        }
        PhysicsShape::Capsule { radius, half_height, axis } => {
            let (r, h) = radial_scale(s, axis);
            PhysicsShape::Capsule { radius: radius * r, half_height: half_height * h, axis }
        }
        // Object-space geometry: scale every point/vertex component-wise so the
        // collider matches the scaled visual mesh (exact for any axis-aligned
        // scale), rather than leaving it at authored unit size.
        PhysicsShape::Hull { points } => PhysicsShape::Hull {
            points: points.into_iter().map(|p| p * s).collect(),
        },
        PhysicsShape::Trimesh { vertices, indices } => PhysicsShape::Trimesh {
            vertices: vertices.into_iter().map(|v| v * s).collect(),
            indices,
        },
        PhysicsShape::Plane { .. } => shape,
    }
}

/// The uniform factor for a radially-symmetric shape under `scale` (already
/// `abs`): the mean of the three axes.
fn uniform_factor(scale: Vec3) -> f32 {
    (scale.x + scale.y + scale.z) / 3.0
}

/// `(cross_section_factor, length_factor)` for a cylinder/cone/capsule under
/// `scale` (already `abs`): the radius scales by the mean of the two axes
/// perpendicular to `axis`, the half-height by the `axis` component. Exact when
/// the cross-section stays circular (the two perpendicular axes are equal).
fn radial_scale(scale: Vec3, axis: Axis) -> (f32, f32) {
    match axis {
        Axis::X => ((scale.y + scale.z) * 0.5, scale.x),
        Axis::Y => ((scale.x + scale.z) * 0.5, scale.y),
        Axis::Z => ((scale.x + scale.y) * 0.5, scale.z),
    }
}

/// Whether `shape` can carry `scale` with no loss. A cuboid (and the geometry-
/// carrying `Hull` / `Trimesh` / `Plane`) represents any axis-aligned scale; a
/// sphere needs a uniform scale; a cylinder/cone/capsule needs its circular
/// cross-section preserved (the two axes perpendicular to its long axis equal).
fn scale_is_representable(shape: &PhysicsShape, scale: Vec3) -> bool {
    let s = scale.abs();
    match shape {
        PhysicsShape::Cuboid { .. }
        | PhysicsShape::Hull { .. }
        | PhysicsShape::Trimesh { .. }
        | PhysicsShape::Plane { .. } => true,
        PhysicsShape::Sphere { .. } => is_uniform_scale(s, 1e-3),
        PhysicsShape::Cylinder { axis, .. }
        | PhysicsShape::Cone { axis, .. }
        | PhysicsShape::Capsule { axis, .. } => cross_section_uniform(s, *axis),
    }
}

/// Whether the two axes perpendicular to `axis` are (near-)equal in `scale`
/// (already `abs`), i.e. a cylinder/cone/capsule keeps a circular cross-section.
fn cross_section_uniform(scale: Vec3, axis: Axis) -> bool {
    let (a, b) = match axis {
        Axis::X => (scale.y, scale.z),
        Axis::Y => (scale.x, scale.z),
        Axis::Z => (scale.x, scale.y),
    };
    let max = a.max(b);
    max <= f32::EPSILON || (a - b).abs() / max <= 1e-3
}

/// `Some(rigid_body_enabled)` when `prim` carries `UsdPhysicsRigidBodyAPI`, else
/// `None` (⇒ a static collider).
fn rigid_body_state(service: &StageService, prim: PrimId) -> Option<bool> {
    service
        .prim_has_api(prim, PHYSICS_RIGID_BODY_SCHEMA)
        .unwrap_or(false)
        .then(|| read_bool(service, prim, ATTR_PHYSICS_RIGID_BODY_ENABLED).unwrap_or(true))
}

/// The body prim that owns the collider `snap`: a self-first walk to the nearest
/// prim carrying `UsdPhysicsRigidBodyAPI`. Returns `(body_prim, has_rigid_body)`.
/// When no ancestor (nor the collider itself) carries the schema, the collider is
/// its **own standalone static body** and `has_rigid_body` is `false` (ground
/// planes / static props). All colliders that resolve to
/// the same body prim form one compound rigid body.
///
/// `UsdPhysics` forbids a rigid body nested under another, so the nearest
/// ancestor is unambiguously the body root.
fn resolve_body_prim(service: &StageService, hydrator: &SceneHydrator, snap: &Snap) -> (PrimId, bool) {
    if service
        .prim_has_api(snap.prim, PHYSICS_RIGID_BODY_SCHEMA)
        .unwrap_or(false)
    {
        return (snap.prim, true);
    }
    let mut cursor = snap.parent;
    for _ in 0..64 {
        let Some(pid) = cursor else { break };
        if service
            .prim_has_api(pid, PHYSICS_RIGID_BODY_SCHEMA)
            .unwrap_or(false)
        {
            return (pid, true);
        }
        cursor = hydrator
            .entity_for(pid)
            .and_then(|e| hydrator.world().get::<PrimNode>(e))
            .and_then(|n| n.parent);
    }
    (snap.prim, false)
}

/// The nearest authored `MassAPI` `physics:density` on the `collider` prim or an
/// ancestor up to (and including) the `body` prim — the inherited density for the
/// collider (plan §5.3: a density on a child collider overrides one on a parent,
/// and an inherited density overrides a bound material's). `None` ⇒ no `MassAPI`
/// density in that chain (fall back to the material density, then the default).
fn inherited_mass_density(
    service: &StageService,
    hydrator: &SceneHydrator,
    collider: PrimId,
    body: PrimId,
) -> Option<f32> {
    let mut cursor = Some(collider);
    for _ in 0..64 {
        let pid = cursor?;
        if let Some(d) = read_f32(service, pid, ATTR_PHYSICS_DENSITY).filter(|d| d.is_finite() && *d > 0.0)
        {
            return Some(d);
        }
        if pid == body {
            break;
        }
        cursor = hydrator
            .entity_for(pid)
            .and_then(|e| hydrator.world().get::<PrimNode>(e))
            .and_then(|n| n.parent);
    }
    None
}

/// Resolve a collider's `(dynamicFriction, restitution, material_density)`:
/// friction/restitution are authored inline on the collider prim, else from the
/// bound physics material (`material:binding:physics`), else the engine defaults.
/// `material_density` is the bound material's `physics:density` (the
/// lowest-precedence density source for the mass computation — below any
/// `MassAPI` density). The binding is read through the existing
/// [`StageService::inspect_prim`] relationship-targets — no new relationship FFI.
fn resolve_material(
    service: &mut StageService,
    hydrator: &SceneHydrator,
    prim: PrimId,
) -> (f32, f32, Option<f32>) {
    let defaults = ColliderDesc::default();
    let mut friction = defaults.friction;
    let mut restitution = defaults.restitution;
    let mut material_density = None;

    // Resolve the bound physics material first (friction/restitution + density),
    // then let an inline friction/restitution opinion on the collider win.
    let bound = physics_material_binding(service, prim)
        .and_then(|path| hydrator.entity_for_path(&path))
        .and_then(|e| hydrator.world().get::<PrimNode>(e).map(|n| n.prim));
    if let Some(mat_prim) = bound {
        if let Some(f) = read_f32(service, mat_prim, ATTR_PHYSICS_DYNAMIC_FRICTION) {
            friction = f;
        }
        if let Some(r) = read_f32(service, mat_prim, ATTR_PHYSICS_RESTITUTION) {
            restitution = r;
        }
        material_density = read_f32(service, mat_prim, ATTR_PHYSICS_DENSITY);
    }
    if let Some(f) = read_f32(service, prim, ATTR_PHYSICS_DYNAMIC_FRICTION) {
        friction = f;
    }
    if let Some(r) = read_f32(service, prim, ATTR_PHYSICS_RESTITUTION) {
        restitution = r;
    }
    (friction, restitution, material_density)
}

/// The first `material:binding:physics` relationship target on `prim`, read via
/// the existing property inspection (which exposes `relationship_targets`).
fn physics_material_binding(service: &mut StageService, prim: PrimId) -> Option<String> {
    let props = service.inspect_prim(prim).ok()?;
    props
        .into_iter()
        .find(|p| p.name == "material:binding:physics")
        .and_then(|p| p.relationship_targets.into_iter().next())
}

/// The engine channel mask from the collider's `IdtxCollisionAPI` channels
/// (`All` ⇒ `0` = member of every group / unfiltered; otherwise the OR of the
/// specific channel bits). Mirrors the picking-raycast channel convention.
fn channel_mask_of(comp: &CollisionComp) -> u32 {
    use idtx_scene::CollisionChannel;
    if comp.channels.is_empty() || comp.channels.contains(&CollisionChannel::All) {
        return 0;
    }
    let mut mask = 0u32;
    for ch in &comp.channels {
        mask |= match ch {
            CollisionChannel::All => 0,
            CollisionChannel::Physics => 1 << 0,
            CollisionChannel::Interaction => 1 << 1,
        };
    }
    mask
}

/// Resolve a collider prim to a [`PhysicsShape`] + body-local centre (collider
/// local, unit scale — the caller bakes the body scale). `None` when the shape
/// cannot be built (an unresolvable analytic form, or a Phase-1-unsupported mesh
/// collider).
fn resolve_shape(service: &StageService, snap: &Snap) -> Option<(PhysicsShape, Vec3)> {
    // A `UsdGeomPlane` collider is an infinite static ground half-space (plan
    // §8.1) — resolved from the prim type, independent of the `CollisionComp`
    // shape (which has no Plane variant and resolves such a prim to `Mesh`).
    if snap.type_name == "Plane" {
        return Some((
            PhysicsShape::Plane {
                normal: plane_normal(service, snap.prim),
            },
            Vec3::ZERO,
        ));
    }

    let comp = snap.collision.as_ref()?;
    match comp.shape {
        CollisionShape::Hull => {
            let points = comp
                .hull_points
                .iter()
                .map(|p| Vec3::from(*p))
                .collect::<Vec<_>>();
            Some((PhysicsShape::Hull { points }, Vec3::ZERO))
        }
        CollisionShape::Mesh => {
            // Mesh (trimesh) colliders are static-only and need the prim's own
            // triangulated geometry; wiring MeshComp → Trimesh is a Phase-1
            // follow-up (no fixture exercises it — the ground is a Plane).
            tracing::warn!(path = %snap.path, "mesh (trimesh) collider not yet supported in the physics sim; skipped");
            None
        }
        _ => {
            let geom: Option<ShapeGeom> = snap
                .local_bounds
                .map(|(min, max)| collision_shape_geom(min, max));
            let resolved = resolve_analytic(comp.shape, &comp.dims, geom)?;
            Some((scaled_shape(resolved.kind, 1.0), resolved.center))
        }
    }
}

/// Snapshot every prim carrying a [`CollisionComp`] (plus the `Plane` ground
/// prims, which carry one too) into plain [`Snap`] structs — read entirely from
/// the ECS so no `StageService` borrow is held during the later `&mut` reads.
fn snapshot_colliders(hydrator: &SceneHydrator) -> Vec<Snap> {
    let world = hydrator.world();
    let mut out = Vec::new();
    for eref in world.iter_entities() {
        let Some(node) = eref.get::<PrimNode>() else {
            continue;
        };
        if !node.state.active || !node.state.loaded { continue; }
        let collision = eref.get::<CollisionComp>();
        // A collider authored with `physics:collisionEnabled = false` must not
        // block or contact rigid bodies — skip it (the flag rides CollisionComp).
        if collision.is_some_and(|c| !c.enabled) {
            continue;
        }
        // Shape alone grants no collision. In particular, removing a Plane's
        // collision API must not leave an implicit infinite collider behind.
        if collision.is_none() {
            continue;
        }
        let Some(wt) = eref.get::<WorldTransform>() else {
            continue;
        };
        out.push(Snap {
            prim: node.prim,
            parent: node.parent,
            path: node.path.clone(),
            type_name: node.type_name.clone(),
            world: wt.matrix,
            local_bounds: eref
                .get::<BoundsComp>()
                .map(|b| (b.local_minimum, b.local_maximum)),
            collision: collision.cloned(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_conversion_matches() {
        assert_eq!(to_phys_axis(GprimAxis::X), Axis::X);
        assert_eq!(to_phys_axis(GprimAxis::Y), Axis::Y);
        assert_eq!(to_phys_axis(GprimAxis::Z), Axis::Z);
    }

    #[test]
    fn scaled_shape_bakes_uniform_scale() {
        let s = scaled_shape(ResolvedKind::Sphere { radius: 2.0 }, 3.0);
        assert_eq!(s, PhysicsShape::Sphere { radius: 6.0 });
        let c = scaled_shape(
            ResolvedKind::Cube { half_extents: Vec3::splat(0.5) },
            2.0,
        );
        assert_eq!(c, PhysicsShape::Cuboid { half_extents: Vec3::splat(1.0) });
        let cyl = scaled_shape(
            ResolvedKind::Cylinder { radius: 1.0, half_height: 2.0, axis: GprimAxis::Z },
            2.0,
        );
        assert_eq!(
            cyl,
            PhysicsShape::Cylinder { radius: 2.0, half_height: 4.0, axis: Axis::Z }
        );
    }

    #[test]
    fn deg_to_rad_converts_angular() {
        let r = deg_to_rad(Vec3::new(180.0, 0.0, 90.0));
        assert!((r.x - std::f32::consts::PI).abs() < 1e-5);
        assert!((r.z - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    #[test]
    fn up_axis_token_parses() {
        assert_eq!(up_axis_from_token("Y"), UpAxis::Y);
        assert_eq!(up_axis_from_token("Z"), UpAxis::Z);
        assert_eq!(up_axis_from_token("whatever"), UpAxis::Y);
    }

    #[test]
    fn bake_scale_cuboid_is_anisotropic() {
        // A flattened ground cube keeps its thin Y extent exactly — the
        // regression target: no collapse to a mean-scaled cube.
        let shape = PhysicsShape::Cuboid { half_extents: Vec3::splat(5.0) };
        let PhysicsShape::Cuboid { half_extents } = bake_scale(shape, Vec3::new(1.0, 0.01, 1.0))
        else {
            panic!("cuboid stays a cuboid");
        };
        assert!(
            (half_extents - Vec3::new(5.0, 0.05, 5.0)).length() < 1e-5,
            "{half_extents:?}"
        );
    }

    #[test]
    fn bake_scale_sphere_uses_mean() {
        let s = bake_scale(PhysicsShape::Sphere { radius: 1.0 }, Vec3::new(2.0, 2.0, 2.0));
        assert_eq!(s, PhysicsShape::Sphere { radius: 2.0 });
        // Non-uniform → mean (the shape cannot carry it exactly).
        let s = bake_scale(PhysicsShape::Sphere { radius: 3.0 }, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(s, PhysicsShape::Sphere { radius: 6.0 });
    }

    #[test]
    fn bake_scale_hull_scales_points_componentwise() {
        // A hull carries object-space geometry: each point scales per-axis so the
        // collider matches the scaled visual (exact for any axis-aligned scale),
        // rather than staying at authored unit size.
        let shape = PhysicsShape::Hull {
            points: vec![Vec3::new(1.0, 2.0, 3.0), Vec3::new(-1.0, 0.0, 1.0)],
        };
        let PhysicsShape::Hull { points } = bake_scale(shape, Vec3::new(2.0, 3.0, 0.5)) else {
            panic!("hull stays a hull");
        };
        assert_eq!(points[0], Vec3::new(2.0, 6.0, 1.5));
        assert_eq!(points[1], Vec3::new(-2.0, 0.0, 0.5));
    }

    #[test]
    fn bake_scale_cylinder_splits_radius_and_length() {
        // Y-axis cylinder: radius from the X/Z mean, half-height from Y.
        let s = bake_scale(
            PhysicsShape::Cylinder { radius: 1.0, half_height: 2.0, axis: Axis::Y },
            Vec3::new(3.0, 4.0, 3.0),
        );
        assert_eq!(
            s,
            PhysicsShape::Cylinder { radius: 3.0, half_height: 8.0, axis: Axis::Y }
        );
    }

    #[test]
    fn scale_representability_rules() {
        let cuboid = PhysicsShape::Cuboid { half_extents: Vec3::ONE };
        // A cuboid carries any axis-aligned scale exactly → never warns.
        assert!(scale_is_representable(&cuboid, Vec3::new(1.0, 0.01, 1.0)));
        // A sphere needs a uniform scale.
        let sphere = PhysicsShape::Sphere { radius: 1.0 };
        assert!(scale_is_representable(&sphere, Vec3::splat(2.0)));
        assert!(!scale_is_representable(&sphere, Vec3::new(1.0, 2.0, 1.0)));
        // A Y cylinder needs its X/Z cross-section circular; a Y-only stretch is fine.
        let cyl = PhysicsShape::Cylinder { radius: 1.0, half_height: 1.0, axis: Axis::Y };
        assert!(scale_is_representable(&cyl, Vec3::new(2.0, 5.0, 2.0)));
        assert!(!scale_is_representable(&cyl, Vec3::new(2.0, 5.0, 3.0)));
    }

    /// Build a one-body subsystem around a real engine body keyed by `entity`,
    /// so the grab suspend/resume transitions can be exercised without USD.
    fn subsystem_with_dynamic_body(entity: Entity) -> PhysicsSubsystem {
        use idtx_physics::{BodyDesc, ColliderDesc};
        let mut world = PhysicsWorld::new(SceneParams::default());
        let key = PhysicsKey(entity.to_bits());
        let body = BodyDesc { kind: BodyKind::Dynamic, ..BodyDesc::default() };
        let collider = ColliderDesc {
            shape: PhysicsShape::Sphere { radius: 0.5 },
            ..ColliderDesc::default()
        };
        assert!(world.insert_body(key, &body, std::slice::from_ref(&collider)));
        PhysicsSubsystem {
            world,
            bodies: vec![BodyEntry {
                key,
                path: "/World/Ball".to_owned(),
                entity,
                kind: BodyKind::Dynamic,
                grab_restore: None,
                last_sleeping: false,
                configuration: body,
                colliders: vec![collider],
                collider_prims: Vec::new(),
                has_rb: true,
            }],
        }
    }

    #[test]
    fn grab_suspend_resume_flips_body_kind() {
        use idtx_scene::bevy_ecs::world::World as EcsWorld;
        let mut ecs = EcsWorld::new();
        let entity = ecs.spawn_empty().id();
        let other = ecs.spawn_empty().id();
        let mut sub = subsystem_with_dynamic_body(entity);

        // Suspend: dynamic → kinematic, grab_restore records Dynamic, leaves the set.
        assert!(sub.set_body_simulated(entity, false));
        assert_eq!(sub.bodies[0].kind, BodyKind::Kinematic);
        assert_eq!(sub.bodies[0].grab_restore, Some(BodyKind::Dynamic));
        assert!(sub.world.dynamic_world_poses().is_empty());
        // Idempotent: suspending an already-suspended body does nothing.
        assert!(!sub.set_body_simulated(entity, false));

        // Resume: kinematic → dynamic, flag cleared, back in the authored set.
        assert!(sub.set_body_simulated(entity, true));
        assert_eq!(sub.bodies[0].kind, BodyKind::Dynamic);
        assert_eq!(sub.bodies[0].grab_restore, None);
        assert_eq!(sub.world.dynamic_world_poses().len(), 1);
        // Resuming a body that was never suspended is a no-op.
        assert!(!sub.set_body_simulated(entity, true));

        // An untracked entity is always a no-op.
        assert!(!sub.set_body_simulated(other, false));
        assert!(!sub.set_body_simulated(other, true));
    }

    #[test]
    fn resume_never_disturbs_a_natively_kinematic_body() {
        use idtx_scene::bevy_ecs::world::World as EcsWorld;
        let mut ecs = EcsWorld::new();
        let entity = ecs.spawn_empty().id();
        let mut sub = subsystem_with_dynamic_body(entity);
        // Simulate a body that is kinematic by nature (animated), not by a grab.
        sub.bodies[0].kind = BodyKind::Kinematic;
        sub.bodies[0].grab_restore = None;
        // A stray resume must not flip it to dynamic (grab_restore guards it).
        assert!(!sub.set_body_simulated(entity, true));
        assert_eq!(sub.bodies[0].kind, BodyKind::Kinematic);
        // And a suspend on a natively-kinematic body is likewise a no-op.
        assert!(!sub.set_body_simulated(entity, false));
        assert_eq!(sub.bodies[0].kind, BodyKind::Kinematic);
    }

    #[test]
    fn grab_suspend_resume_drives_a_static_collider() {
        // Issue: a collider-only (static) prim that is grabbed must have its
        // collision shape follow the grab so the sim reacts to the move — the
        // grab flips it to kinematic (fed by the grabber each frame) and release
        // restores it to static at the let-go pose, not to dynamic.
        use idtx_scene::bevy_ecs::world::World as EcsWorld;
        let mut ecs = EcsWorld::new();
        let entity = ecs.spawn_empty().id();
        let mut sub = subsystem_with_dynamic_body(entity);
        // Reclassify the engine body + entry as a static collider.
        assert!(sub.world.set_body_kind(PhysicsKey(entity.to_bits()), BodyKind::Static));
        sub.bodies[0].kind = BodyKind::Static;

        // Grab: static → kinematic, grab_restore records Static.
        assert!(sub.set_body_simulated(entity, false));
        assert_eq!(sub.bodies[0].kind, BodyKind::Kinematic);
        assert_eq!(sub.bodies[0].grab_restore, Some(BodyKind::Static));
        // Still not authored by physics (only dynamic bodies are).
        assert!(sub.world.dynamic_world_poses().is_empty());

        // Release: kinematic → static (NOT dynamic), flag cleared.
        assert!(sub.set_body_simulated(entity, true));
        assert_eq!(sub.bodies[0].kind, BodyKind::Static);
        assert_eq!(sub.bodies[0].grab_restore, None);
        assert!(sub.world.dynamic_world_poses().is_empty());
    }
}
