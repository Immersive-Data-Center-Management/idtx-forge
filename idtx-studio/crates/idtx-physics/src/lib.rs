//! Runtime rigid-body physics for IDTX — `UsdPhysics` → Rapier.
//!
//! A **runtime-only** rigid-body simulation activated by the presence of a
//! `UsdPhysicsScene` prim in the experienced stage. This crate owns the engine
//! (the [`PhysicsWorld`]) and the pure-Rust read/write math; the USD/ECS adapter
//! that hydrates authored `UsdPhysics*` schemas into [`BodyDesc`]/[`ColliderDesc`]
//! values, and writes solver poses back to the session layer, lives in the
//! runtime layer that links the `OpenUSD` bridge. See
//! `docs/physics-simulation-runtime-plan.md` and `docs/physics/`.
//!
//! # Why the engine is dependency-light
//!
//! `idtx-physics` depends only on `glam` + `rapier3d` (not `idtx-scene`, which
//! links the `OpenUSD` bridge). Rapier 0.36 / parry 0.31 are **glam-native via
//! glamx**, so the project's own [`glam`] types *are* the engine's math types —
//! seeding a body and reading its pose back are conversion-free — and every unit
//! test here runs with no `OpenUSD` link, on any host (validated native + Quest +
//! Vision Pro — see `docs/physics/phase0-validation.md`).
//!
//! # Runtime-only by construction
//!
//! Depended on only by the runtime path; never by `idtx-inspector` /
//! `idtx-poser` / the `idtx-conductor` editor viewport. Conductor *preview*
//! launches the out-of-process runtime and therefore *does* simulate — intended.

pub mod mass;
pub mod scene;
pub mod shape;
pub mod transform;

use std::collections::HashMap;

use glam::{Mat4, Quat, Vec3};
use rapier3d::prelude::*;

pub use mass::{default_density_in_stage_units, ColliderMass, MassSpec};
pub use scene::{SceneParams, UpAxis, resolve_gravity};
pub use shape::{Axis, PhysicsShape};
pub use transform::{compound_part_pose, decompose, world_to_local};

/// Stable key for a physics body, mirroring the caller's `PrimId`/`Entity`
/// (store `Entity::to_bits()` here). The engine never interprets it — identity,
/// selection, and write-back targeting are the caller's concern (the
/// stable-identity invariant: keyed by id, never by path string).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhysicsKey(pub u64);

/// Static / kinematic / dynamic classification (plan §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    /// Collider with infinite mass (floors / walls / props). No `RigidBodyAPI`.
    Static,
    /// Externally driven (animation / behavior / grab): pushes dynamics but is
    /// not integrated. `kinematicEnabled`, or a prim also animated/behavior-moved.
    Kinematic,
    /// Integrated dynamic body. `rigidBodyEnabled && !kinematic && !animated`.
    Dynamic,
}

/// Classify a prim from its authored `UsdPhysics` state (plan §5.2).
///
/// `rigid_body` is `None` when the prim carries **no** `UsdPhysicsRigidBodyAPI`
/// (⇒ a static collider), or `Some(rigid_body_enabled)` when it does. An
/// animated or behavior-driven prim is **always** kinematic — never dynamic —
/// so it is not authored twice per frame (its driver authors the pose; physics
/// reads it).
#[must_use]
pub fn classify(
    rigid_body: Option<bool>,
    kinematic_enabled: bool,
    animated_or_driven: bool,
) -> BodyKind {
    match rigid_body {
        Some(_) if kinematic_enabled || animated_or_driven => BodyKind::Kinematic,
        Some(true) => BodyKind::Dynamic,
        None | Some(false) => BodyKind::Static,
    }
}

/// A rigid body to insert, resolved from the authored schemas. `world` carries
/// the body root's world transform (rigid pose + already-baked uniform scale);
/// the engine takes the rigid pose and expects collider dimensions to carry any
/// scale (per the uniform-scale-bake rule, plan §5.5).
///
/// Mass properties for a **dynamic** body are computed from the attached
/// colliders (each collider's `density` × `parry` shape volume, combined by the
/// parallel-axis theorem — compound/whole-asset bodies, plan §5.1). The
/// `explicit_*` fields are the `UsdPhysicsMassAPI` body-level overrides applied
/// on top (`explicit_mass` wins over the computed total; `explicit_com` /
/// `explicit_inertia` replace the computed centre / inertia). Static and
/// kinematic bodies ignore all of these.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyDesc {
    pub kind: BodyKind,
    pub world: Mat4,
    /// `UsdPhysicsMassAPI` `physics:mass` on the body prim — overrides the mass
    /// computed from the colliders. `None` ⇒ use the computed (or default) mass.
    pub explicit_mass: Option<f32>,
    /// `UsdPhysicsMassAPI` `physics:centerOfMass` (body-local) override.
    pub explicit_com: Option<Vec3>,
    /// `UsdPhysicsMassAPI` `physics:diagonalInertia` + `physics:principalAxes`
    /// override, as `(diagonal_inertia, principal_axes)`.
    pub explicit_inertia: Option<(Vec3, Quat)>,
    /// Initial linear velocity (dynamic only), stage units/s.
    pub linvel: Vec3,
    /// Initial angular velocity (dynamic only), rad/s.
    pub angvel: Vec3,
    /// Continuous collision detection (fast/thin bodies).
    pub ccd: bool,
    /// Start the body asleep (dynamic only) — the `UsdPhysicsRigidBodyAPI`
    /// `physics:startsAsleep` initial condition. A body seeded asleep does not
    /// integrate gravity until woken by a contact or a grab release, keeping USD
    /// rest state and the sim in sync (plan Phase 2A).
    pub starts_asleep: bool,
}

impl Default for BodyDesc {
    fn default() -> Self {
        Self {
            kind: BodyKind::Static,
            world: Mat4::IDENTITY,
            explicit_mass: None,
            explicit_com: None,
            explicit_inertia: None,
            linvel: Vec3::ZERO,
            angvel: Vec3::ZERO,
            ccd: false,
            starts_asleep: false,
        }
    }
}

/// A collider attached to a body, in the body's local space.
#[derive(Clone, Debug, PartialEq)]
pub struct ColliderDesc {
    pub shape: PhysicsShape,
    /// Body-local centre offset of the shape (the resolved bounds midpoint,
    /// already rotated into the body frame for a compound child).
    pub local_center: Vec3,
    /// Body-local rotation of the collider (identity for a single-collider body;
    /// a compound child's rotation relative to the body root). Composed with the
    /// shape's intrinsic axis rotation at build time.
    pub local_rotation: Quat,
    /// Effective density (stage mass/length³) for this collider, already resolved
    /// by the caller through the `UsdPhysics` precedence (see [`mass`]). Used only
    /// for a **dynamic** body's mass computation.
    pub density: f32,
    /// `UsdPhysicsMassAPI` `physics:mass` authored on this collider prim — scales
    /// this collider's contribution to that mass. `None` ⇒ density × volume.
    pub explicit_mass: Option<f32>,
    /// Dynamic friction coefficient.
    pub friction: f32,
    /// Restitution (bounciness) coefficient.
    pub restitution: f32,
    /// Collision query-channel mask (reuses the raycast channel bits). `0` =
    /// member of all groups (unfiltered).
    pub channel_mask: u32,
}

impl Default for ColliderDesc {
    fn default() -> Self {
        Self {
            shape: PhysicsShape::Sphere { radius: 0.5 },
            local_center: Vec3::ZERO,
            local_rotation: Quat::IDENTITY,
            density: 1.0,
            explicit_mass: None,
            friction: 0.5,
            restitution: 0.0,
            channel_mask: 0,
        }
    }
}

/// Combine the per-collider [`MassProperties`] of a **dynamic** body (already
/// placed at each collider's body-local pose) into the body's final mass
/// properties, applying the `UsdPhysicsMassAPI` body-level overrides.
///
/// `parry`'s `Sum` combines the colliders with the parallel-axis theorem. Then,
/// per the `UsdPhysics` precedence: an explicit body `physics:mass` overrides the
/// computed total (scaling the inertia with it); if there is no explicit mass and
/// the colliders enclose no usable volume, the body falls back to the spec
/// default mass of `1.0`. Finally an explicit centre-of-mass / inertia replaces
/// the computed values.
fn combine_body_mass(props: Vec<MassProperties>, desc: &BodyDesc) -> MassProperties {
    let mut combined: MassProperties = props.into_iter().sum();

    if let Some(m) = desc.explicit_mass.filter(|m| m.is_finite() && *m > 0.0) {
        combined.set_mass(m, true);
    } else if !(combined.mass().is_finite() && combined.mass() > 0.0) {
        // No explicit mass and no collidable volume ⇒ UsdPhysics default mass 1.0.
        combined.set_mass(1.0, true);
    }

    // Only honour FINITE overrides: UsdPhysicsMassAPI leaves `centerOfMass` /
    // `diagonalInertia` at a `(-inf,-inf,-inf)` sentinel when unset, which would
    // otherwise corrupt the body (an infinite COM freezes the solver).
    if let Some(com) = desc.explicit_com.filter(|c| c.is_finite()) {
        combined.local_com = com;
    }
    if let Some((diagonal, axes)) = desc
        .explicit_inertia
        .filter(|(d, a)| d.is_finite() && a.is_finite())
    {
        combined =
            MassProperties::with_principal_inertia_frame(combined.local_com, combined.mass(), diagonal, axes);
    }
    combined
}

/// Per-body bookkeeping inside the [`PhysicsWorld`]: the rapier handle, the
/// current [`BodyKind`], and the body's authored (visual) scale.
///
/// Rapier poses are rigid isometries (no scale) — the authored scale is baked
/// into the collider dimensions at insert time, never into the pose. But the
/// prim's `xformOp:transform` the caller authors back from a solver pose must
/// still carry that scale, or the first physics write would visibly resize a
/// scaled prim to unit scale. So we retain it here and re-compose it into the
/// matrices [`PhysicsWorld::body_world`] / [`PhysicsWorld::dynamic_world_poses`]
/// return, keeping the solver pose itself rigid.
#[derive(Clone, Copy, Debug)]
struct BodyRecord {
    handle: RigidBodyHandle,
    kind: BodyKind,
    scale: Vec3,
}

/// Per-frame physics metrics (behind the `metrics` feature, mirroring
/// `collision-metrics`).
#[cfg(feature = "metrics")]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhysicsMetrics {
    /// Sub-steps run on the most recent [`PhysicsWorld::advance`] call.
    pub substeps_last_frame: u32,
    /// Total fixed steps run over the world's lifetime.
    pub total_steps: u64,
    /// Wall-clock microseconds spent in the most recent `advance`.
    pub advance_micros: u128,
    /// Rigid bodies currently tracked.
    pub body_count: usize,
}

/// The owned Rapier simulation plus the key→handle map and the fixed-timestep
/// accumulator.
pub struct PhysicsWorld {
    params: SceneParams,
    accumulator: f32,
    bodies: HashMap<PhysicsKey, BodyRecord>,

    pipeline: PhysicsPipeline,
    islands: IslandManager,
    broad_phase: BroadPhaseBvh,
    narrow_phase: NarrowPhase,
    rigid_bodies: RigidBodySet,
    colliders: ColliderSet,
    impulse_joints: ImpulseJointSet,
    multibody_joints: MultibodyJointSet,
    soft_bodies: SoftBodySet,
    ccd_solver: CCDSolver,

    #[cfg(feature = "metrics")]
    metrics: PhysicsMetrics,
}

impl PhysicsWorld {
    /// Create an empty world with the resolved per-stage [`SceneParams`].
    #[must_use]
    pub fn new(params: SceneParams) -> Self {
        Self {
            params,
            accumulator: 0.0,
            bodies: HashMap::new(),
            pipeline: PhysicsPipeline::new(),
            islands: IslandManager::new(),
            broad_phase: BroadPhaseBvh::new(),
            narrow_phase: NarrowPhase::new(),
            rigid_bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            impulse_joints: ImpulseJointSet::new(),
            multibody_joints: MultibodyJointSet::new(),
            soft_bodies: SoftBodySet::new(),
            ccd_solver: CCDSolver::new(),
            #[cfg(feature = "metrics")]
            metrics: PhysicsMetrics::default(),
        }
    }

    /// Number of tracked bodies.
    #[must_use]
    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// Update authored scene gravity without resetting bodies or solver time.
    pub fn set_gravity(&mut self, gravity: Vec3) {
        if gravity.is_finite() { self.params.gravity = gravity; }
    }

    /// Total number of colliders across all bodies (a compound body contributes
    /// more than one).
    #[must_use]
    pub fn collider_count(&self) -> usize {
        self.colliders.len()
    }

    /// Latest per-frame metrics.
    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn metrics(&self) -> PhysicsMetrics {
        PhysicsMetrics {
            body_count: self.bodies.len(),
            ..self.metrics
        }
    }

    /// Insert a body with its colliders, keyed by `key`. Degenerate colliders
    /// (empty hull / mesh, zero-normal plane) are skipped. Replaces any existing
    /// body under `key`. Returns `true` if the body was created with at least one
    /// collider.
    pub fn insert_body(
        &mut self,
        key: PhysicsKey,
        desc: &BodyDesc,
        colliders: &[ColliderDesc],
    ) -> bool {
        self.remove_body(key);

        let decomposed = decompose(desc.world);
        let pose = decomposed.pose;

        // First pass: resolve each collider's shape + body-local pose, dropping
        // any that cannot build a shape, and accumulate per-collider mass
        // properties (density × shape volume, placed at the collider's body-local
        // pose) so the combined body mass can be set on the builder before the
        // body is created. Mass is computed for **every** kind — a kinematic or
        // static body may later be flipped to dynamic by a grab release
        // (`set_body_kind`), which recomputes mass from the colliders (now
        // density-0) plus this `additional_mass_properties`.
        let mut built: Vec<(SharedShape, Pose, &ColliderDesc)> = Vec::new();
        let mut mass_props: Vec<MassProperties> = Vec::new();
        for cd in colliders {
            let Some(shape) = cd.shape.to_shared_shape() else {
                continue;
            };
            let local = Pose::from_parts(
                cd.local_center,
                cd.local_rotation * cd.shape.collider_local_rotation(),
            );
            // Trimesh / Plane yield degenerate parry mass properties, so they
            // never contribute to a body's mass (the caller is expected to keep
            // such shapes off dynamic bodies — plan §5.4).
            if cd.shape.dynamic_safe() {
                let mut mp = shape.mass_properties(cd.density).transform_by(&local);
                if let Some(m) = cd.explicit_mass.filter(|m| m.is_finite() && *m > 0.0) {
                    mp.set_mass(m, true);
                }
                mass_props.push(mp);
            }
            built.push((shape, local, cd));
        }

        if built.is_empty() {
            // No usable collider — drop the body rather than keep a massless ghost.
            return false;
        }

        let mut builder = match desc.kind {
            BodyKind::Static => RigidBodyBuilder::fixed(),
            BodyKind::Kinematic => RigidBodyBuilder::kinematic_position_based(),
            BodyKind::Dynamic => RigidBodyBuilder::dynamic(),
        }
        .pose(pose)
        .additional_mass_properties(combine_body_mass(mass_props, desc));

        if desc.kind == BodyKind::Dynamic {
            builder = builder
                .linvel(desc.linvel)
                .angvel(desc.angvel)
                .ccd_enabled(desc.ccd)
                .linear_damping(self.params.linear_damping)
                .angular_damping(self.params.angular_damping)
                .sleeping(desc.starts_asleep);
        }

        let handle = self.rigid_bodies.insert(builder.build());

        // Colliders carry **zero density**: a dynamic body's mass is the explicit
        // `additional_mass_properties` set above (computed from the colliders),
        // and static/kinematic bodies ignore mass entirely. This keeps the body
        // mass from being double-counted.
        for (shape, local, cd) in built {
            let groups = InteractionGroups::new(
                channel_membership(cd.channel_mask),
                Group::ALL,
                InteractionTestMode::And,
            );
            let collider = ColliderBuilder::new(shape)
                .position(local)
                .friction(cd.friction)
                .restitution(cd.restitution)
                .density(0.0)
                .collision_groups(groups)
                .build();
            self.colliders
                .insert_with_parent(collider, handle, &mut self.rigid_bodies);
        }

        self.bodies.insert(
            key,
            BodyRecord { handle, kind: desc.kind, scale: decomposed.scale },
        );
        true
    }

    /// Remove the body under `key` (and its colliders), if present.
    pub fn remove_body(&mut self, key: PhysicsKey) {
        if let Some(BodyRecord { handle, .. }) = self.bodies.remove(&key) {
            self.rigid_bodies.remove(
                handle,
                &mut self.islands,
                &mut self.colliders,
                &mut self.impulse_joints,
                &mut self.multibody_joints,
                &mut self.soft_bodies,
                true,
            );
        }
    }

    /// Wake every dynamic rigid body **except** those in `exclude`.
    ///
    /// Called when a collider's shape changes (e.g. an animated cube `size`).
    /// rapier does **not** wake a sleeping body when the collider it rests on is
    /// replaced — the trailing flag on `RigidBodySet::remove` is
    /// `remove_attached_colliders`, not a wake — so without this a body resting on
    /// the resized one would hang frozen in mid-air instead of settling onto, or
    /// falling off, the new shape.
    ///
    /// The resized body itself is excluded: waking it would make the solver
    /// resolve the penetration its own grown collider now has with whatever it
    /// rests on (e.g. the ground), kicking it upward each interval until it
    /// floats. A sleeping resized body keeps its stable rest pose while its
    /// collider is swapped under it; only the *neighbours* resting on it need to
    /// re-evaluate. Waking is cheap — a body still at rest re-sleeps within a
    /// frame or two, so this does not defeat the sleep optimisation.
    pub fn wake_dynamic_except(&mut self, exclude: &[PhysicsKey]) {
        let handles: Vec<RigidBodyHandle> = self
            .bodies
            .iter()
            .filter(|(key, record)| {
                record.kind == BodyKind::Dynamic && !exclude.contains(key)
            })
            .map(|(_, record)| record.handle)
            .collect();
        for handle in handles {
            if let Some(body) = self.rigid_bodies.get_mut(handle) {
                body.wake_up(true);
            }
        }
    }

    /// Switch a tracked body between dynamic and kinematic (or fixed) at runtime
    /// — the engine side of grab suspend/resume. Returns
    /// `false` when `key` is absent or already of `kind` (no change).
    ///
    /// "Suspend a grab" flips a [`BodyKind::Dynamic`] body to
    /// [`BodyKind::Kinematic`]: it stops integrating gravity and is no longer
    /// authored by physics (it leaves [`Self::dynamic_world_poses`]), yet still
    /// collides with and pushes dynamics as its driver feeds it poses through
    /// [`Self::set_kinematic_target`]. "Resume" flips it back to
    /// [`BodyKind::Dynamic`], **carrying the velocity** the kinematic motion
    /// built up (rapier zeroes velocity on a type change, so we read it before
    /// the switch and re-apply it after) — a flung object throws, a gently-placed
    /// one settles.
    pub fn set_body_kind(&mut self, key: PhysicsKey, kind: BodyKind) -> bool {
        let Some(&BodyRecord { handle, kind: old_kind, scale }) = self.bodies.get(&key) else {
            return false;
        };
        if old_kind == kind {
            return false;
        }
        let Some(body) = self.rigid_bodies.get_mut(handle) else {
            return false;
        };
        // Preserve the current velocity across the switch so a kinematic→dynamic
        // release carries the grab motion into a throw.
        let (linvel, angvel) = (body.linvel(), body.angvel());
        let status = match kind {
            BodyKind::Static => RigidBodyType::Fixed,
            BodyKind::Kinematic => RigidBodyType::KinematicPositionBased,
            BodyKind::Dynamic => RigidBodyType::Dynamic,
        };
        body.set_body_type(status, true);
        if kind == BodyKind::Dynamic {
            body.set_linvel(linvel, true);
            body.set_angvel(angvel, true);
            // Match the damping a born-dynamic body gets in `insert_body`, so a
            // body flipped to dynamic by a grab release (which may have been built
            // static/kinematic) also settles and sleeps instead of jittering.
            body.set_linear_damping(self.params.linear_damping);
            body.set_angular_damping(self.params.angular_damping);
            // A released (grab→dynamic) body must simulate immediately, never
            // resume into a stale sleeping island — explicitly wake it.
            body.wake_up(true);
        }
        self.bodies.insert(key, BodyRecord { handle, kind, scale });
        true
    }

    /// Drive a kinematic body toward a new world pose this frame. Rapier derives
    /// the kinematic velocity from the pose delta so pushed dynamics respond
    /// (plan §6.3). No-op if `key` is absent or not kinematic.
    pub fn set_kinematic_target(&mut self, key: PhysicsKey, world: Mat4) {
        let Some(&BodyRecord { handle, kind, .. }) = self.bodies.get(&key) else {
            return;
        };
        if kind != BodyKind::Kinematic {
            return;
        }
        if let Some(body) = self.rigid_bodies.get_mut(handle) {
            body.set_next_kinematic_position(decompose(world).pose);
        }
    }

    /// Advance by a rendered-frame delta using a fixed-timestep accumulator,
    /// clamped to `max_substeps` to prevent the spiral of death on a frame hitch
    /// (plan §6.2). Returns the number of fixed sub-steps actually run.
    pub fn advance(&mut self, frame_dt: f32) -> u32 {
        #[cfg(feature = "metrics")]
        let t0 = std::time::Instant::now();

        self.accumulator += frame_dt.max(0.0);
        let dt = self.params.dt;
        let mut substeps = 0u32;
        while self.accumulator >= dt && substeps < self.params.max_substeps {
            self.step_once();
            self.accumulator -= dt;
            substeps += 1;
        }
        // The substep clamp fired: this frame wanted more fixed steps than
        // `max_substeps` allows. Carry a *bounded* remainder (≤ 2·dt) rather than
        // zeroing it — the old `accumulator = 0.0` silently discarded all backlog,
        // so a sustained sub-`max_substeps·dt` frame rate advanced the sim slower
        // than wall-clock and never recovered, reading as slow-motion rather than
        // stutter. The `min(2·dt)` cap still prevents a one-off hitch from queuing
        // unbounded catch-up (the spiral-of-death guard the drop was meant to be).
        // An always-on `warn!` (not metrics-gated) makes on-device clamp firing
        // visible in `adb logcat` — a frame too slow to step in real time.
        if substeps == self.params.max_substeps && self.accumulator > dt {
            self.accumulator = self.accumulator.min(2.0 * dt);
            tracing::warn!(
                substeps,
                max_substeps = self.params.max_substeps,
                frame_dt,
                dt,
                "physics substep clamp hit: frame too slow to step in real time (sim time may dilate — possible slow-motion)"
            );
        }

        #[cfg(feature = "metrics")]
        {
            self.metrics.substeps_last_frame = substeps;
            self.metrics.total_steps += u64::from(substeps);
            self.metrics.advance_micros = t0.elapsed().as_micros();
        }
        substeps
    }

    /// Run exactly one fixed step (`params.dt`). Useful for deterministic tests.
    pub fn step_once(&mut self) {
        let hooks = ();
        let events = ();
        self.pipeline.step(
            self.params.gravity,
            &IntegrationParameters {
                dt: self.params.dt,
                ..IntegrationParameters::default()
            },
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.rigid_bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.soft_bodies,
            &mut self.ccd_solver,
            &hooks,
            &events,
        );
    }

    /// The world transform of the body under `key`, if present, with the body's
    /// authored scale re-composed onto the solver's rigid pose. Compose with the
    /// parent world to author the local `xformOp:transform` via [`world_to_local`].
    #[must_use]
    pub fn body_world(&self, key: PhysicsKey) -> Option<Mat4> {
        let rec = self.bodies.get(&key)?;
        let pose = self.rigid_bodies.get(rec.handle)?.position();
        Some(transform::pose_to_matrix(*pose, rec.scale))
    }

    /// The current linear + angular velocity of the body under `key`, for seeding
    /// a throw when a grabbed kinematic body is released (plan §6.3).
    #[must_use]
    pub fn body_velocity(&self, key: PhysicsKey) -> Option<(Vec3, Vec3)> {
        let rec = self.bodies.get(&key)?;
        let b = self.rigid_bodies.get(rec.handle)?;
        Some((b.linvel(), b.angvel()))
    }

    /// Whether the body under `key` is currently asleep (its rapier island has
    /// come to rest). `None` if `key` is absent. The write-back gate uses this to
    /// skip authoring a settled body every frame (plan Phase 2A); a body wakes
    /// automatically on a new contact, and on a grab release via
    /// [`Self::set_body_kind`].
    #[must_use]
    pub fn is_body_sleeping(&self, key: PhysicsKey) -> Option<bool> {
        let rec = self.bodies.get(&key)?;
        Some(self.rigid_bodies.get(rec.handle)?.is_sleeping())
    }

    /// `(awake, asleep)` counts over the **dynamic** bodies — a diagnostic for the
    /// "a settled scene never sleeps" symptom: `asleep == 0` with a stable scene
    /// means the island never reaches rest (e.g. a jittery complex-hull contact),
    /// and a rising `awake` over time means bodies accumulate without settling.
    #[must_use]
    pub fn dynamic_sleep_counts(&self) -> (usize, usize) {
        let mut awake = 0;
        let mut asleep = 0;
        for rec in self.bodies.values() {
            if rec.kind != BodyKind::Dynamic {
                continue;
            }
            match self.rigid_bodies.get(rec.handle) {
                Some(b) if b.is_sleeping() => asleep += 1,
                Some(_) => awake += 1,
                None => {}
            }
        }
        (awake, asleep)
    }

    /// World poses of every **dynamic** body, for session-layer write-back, each
    /// with the body's authored scale re-composed onto the solver's rigid pose
    /// (so a scaled prim keeps its visual size rather than snapping to unit scale
    /// on the first write). Only dynamic bodies are authored by physics (kinematic
    /// bodies are authored by their driver; static bodies never move) — plan
    /// §6.3 / §7.
    #[must_use]
    pub fn dynamic_world_poses(&self) -> Vec<(PhysicsKey, Mat4)> {
        self.bodies
            .iter()
            .filter(|(_, rec)| rec.kind == BodyKind::Dynamic)
            .filter_map(|(key, rec)| {
                let pose = self.rigid_bodies.get(rec.handle)?.position();
                Some((*key, transform::pose_to_matrix(*pose, rec.scale)))
            })
            .collect()
    }
}

/// A `0` channel mask (`CHANNEL_ANY`) means "member of all groups" (unfiltered);
/// any other mask restricts membership to the set bits.
fn channel_membership(mask: u32) -> Group {
    if mask == 0 {
        Group::ALL
    } else {
        Group::from_bits_truncate(mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> SceneParams {
        SceneParams::default()
    }

    fn ground() -> (BodyDesc, Vec<ColliderDesc>) {
        (
            BodyDesc { kind: BodyKind::Static, ..BodyDesc::default() },
            vec![ColliderDesc {
                shape: PhysicsShape::Plane { normal: Vec3::Y },
                ..ColliderDesc::default()
            }],
        )
    }

    fn falling_ball(height: f32) -> (BodyDesc, Vec<ColliderDesc>) {
        (
            BodyDesc {
                kind: BodyKind::Dynamic,
                world: Mat4::from_translation(Vec3::new(0.0, height, 0.0)),
                ..BodyDesc::default()
            },
            vec![ColliderDesc {
                shape: PhysicsShape::Sphere { radius: 0.5 },
                restitution: 0.0,
                ..ColliderDesc::default()
            }],
        )
    }

    #[test]
    fn classify_rules() {
        // No RigidBodyAPI → static.
        assert_eq!(classify(None, false, false), BodyKind::Static);
        // Plain rigid body → dynamic.
        assert_eq!(classify(Some(true), false, false), BodyKind::Dynamic);
        // kinematicEnabled → kinematic.
        assert_eq!(classify(Some(true), true, false), BodyKind::Kinematic);
        // Animated/driven is kinematic even if rigidBodyEnabled (never both).
        assert_eq!(classify(Some(true), false, true), BodyKind::Kinematic);
        // rigidBodyEnabled=false → static.
        assert_eq!(classify(Some(false), false, false), BodyKind::Static);
    }

    #[test]
    fn ball_falls_and_rests_on_plane() {
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        assert!(w.insert_body(PhysicsKey(1), &gd, &gc));
        let (bd, bc) = falling_ball(10.0);
        assert!(w.insert_body(PhysicsKey(2), &bd, &bc));

        let start = w.body_world(PhysicsKey(2)).unwrap().w_axis.y;
        for _ in 0..240 {
            w.step_once();
        }
        let end = w.body_world(PhysicsKey(2)).unwrap().w_axis.y;
        assert!(end < start, "ball should fall: {start} -> {end}");
        // Sphere radius 0.5 resting on the y=0 plane settles near y≈0.5.
        assert!((end - 0.5).abs() < 0.1, "ball should rest near 0.5, got {end}");
    }

    #[test]
    fn compound_body_two_colliders_rests_level() {
        // A dynamic body with two cuboid colliders offset ±1 in X (half-extent
        // 0.5 each) — a compound/whole-asset body. It must fall and rest level on
        // the plane (combined COM centred ⇒ no net torque), proving both
        // colliders attach to the one body and the mass combines symmetrically.
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        w.insert_body(PhysicsKey(1), &gd, &gc);

        let bd = BodyDesc {
            kind: BodyKind::Dynamic,
            world: Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0)),
            ..BodyDesc::default()
        };
        let cube = |x: f32| ColliderDesc {
            shape: PhysicsShape::Cuboid { half_extents: Vec3::splat(0.5) },
            local_center: Vec3::new(x, 0.0, 0.0),
            ..ColliderDesc::default()
        };
        assert!(w.insert_body(PhysicsKey(2), &bd, &[cube(-1.0), cube(1.0)]));
        assert_eq!(w.body_count(), 2, "ground + one compound body");

        for _ in 0..300 {
            w.step_once();
        }
        let m = w.body_world(PhysicsKey(2)).unwrap();
        let (_, rot, t) = m.to_scale_rotation_translation();
        assert!((t.y - 0.5).abs() < 0.1, "compound body should rest at y≈0.5, got {}", t.y);
        assert!(t.x.abs() < 0.2, "centred COM ⇒ no X drift, got {}", t.x);
        assert!(
            rot.angle_between(Quat::IDENTITY) < 0.2,
            "symmetric compound should rest level, got {rot:?}"
        );
    }

    #[test]
    fn local_rotation_orients_collider() {
        // A single tall cuboid (half-extents 0.5,2,0.5) given a 90° local rotation
        // about Z becomes long in X / short (0.5) in Y, so it rests at y≈0.5 —
        // not y≈2.0 as it would unrotated. Proves ColliderDesc.local_rotation is
        // composed into the collider pose.
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        w.insert_body(PhysicsKey(1), &gd, &gc);
        let bd = BodyDesc {
            kind: BodyKind::Dynamic,
            world: Mat4::from_translation(Vec3::new(0.0, 6.0, 0.0)),
            ..BodyDesc::default()
        };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Cuboid { half_extents: Vec3::new(0.5, 2.0, 0.5) },
            local_rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            ..ColliderDesc::default()
        }];
        assert!(w.insert_body(PhysicsKey(2), &bd, &bc));
        for _ in 0..300 {
            w.step_once();
        }
        let y = w.body_world(PhysicsKey(2)).unwrap().w_axis.y;
        assert!((y - 0.5).abs() < 0.25, "rotated tall box rests on its 0.5 side at y≈0.5, got {y}");
    }

    #[test]
    fn combine_body_mass_sums_and_centers() {
        // Two unit point-masses at ±1 X ⇒ total mass 2, COM at the origin.
        let a = MassProperties::new(Vec3::new(-1.0, 0.0, 0.0), 1.0, Vec3::splat(0.1));
        let b = MassProperties::new(Vec3::new(1.0, 0.0, 0.0), 1.0, Vec3::splat(0.1));
        let desc = BodyDesc { kind: BodyKind::Dynamic, ..BodyDesc::default() };
        let c = combine_body_mass(vec![a, b], &desc);
        assert!((c.mass() - 2.0).abs() < 1e-4, "mass {}", c.mass());
        assert!(c.local_com.length() < 1e-4, "COM centred, got {:?}", c.local_com);
    }

    #[test]
    fn combine_body_mass_applies_overrides() {
        let a = MassProperties::new(Vec3::ZERO, 3.0, Vec3::splat(0.5));
        // Explicit body mass overrides the summed total.
        let desc =
            BodyDesc { kind: BodyKind::Dynamic, explicit_mass: Some(10.0), ..BodyDesc::default() };
        assert!((combine_body_mass(vec![a], &desc).mass() - 10.0).abs() < 1e-4);
        // Explicit COM replaces the computed centre.
        let desc = BodyDesc {
            kind: BodyKind::Dynamic,
            explicit_com: Some(Vec3::new(0.0, 1.0, 0.0)),
            ..BodyDesc::default()
        };
        assert!((combine_body_mass(vec![a], &desc).local_com - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-4);
        // No colliders / no volume ⇒ UsdPhysics default mass 1.0.
        let desc = BodyDesc { kind: BodyKind::Dynamic, ..BodyDesc::default() };
        assert!((combine_body_mass(vec![], &desc).mass() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn combine_body_mass_ignores_nonfinite_overrides() {
        // Regression: `UsdPhysicsMassAPI` leaves `centerOfMass`/`diagonalInertia`
        // at a `(-inf,-inf,-inf)` sentinel when unset. Honouring it would give the
        // body an infinite COM and freeze the solver — a dynamic body with
        // `physics:mass` authored but no COM must keep a finite, computed COM.
        let a = MassProperties::new(Vec3::ZERO, 2.0, Vec3::splat(0.1));
        let desc = BodyDesc {
            kind: BodyKind::Dynamic,
            explicit_mass: Some(1.0),
            explicit_com: Some(Vec3::splat(f32::NEG_INFINITY)),
            explicit_inertia: Some((Vec3::splat(f32::NEG_INFINITY), Quat::IDENTITY)),
            ..BodyDesc::default()
        };
        let c = combine_body_mass(vec![a], &desc);
        assert!(c.local_com.is_finite(), "sentinel COM must be ignored, got {:?}", c.local_com);
        assert!((c.mass() - 1.0).abs() < 1e-4, "explicit mass still applies: {}", c.mass());
    }

    #[test]
    fn restitution_makes_it_bounce() {
        let mut w = PhysicsWorld::new(params());
        let (gd, mut gc) = ground();
        gc[0].restitution = 0.9;
        w.insert_body(PhysicsKey(1), &gd, &gc);
        let (bd, mut bc) = falling_ball(5.0);
        bc[0].restitution = 0.9;
        w.insert_body(PhysicsKey(2), &bd, &bc);

        // Let it hit the ground and rebound; a bouncy ball should still carry
        // upward velocity at some point after first contact.
        let mut max_up = f32::MIN;
        for _ in 0..240 {
            w.step_once();
            let vy = w.body_velocity(PhysicsKey(2)).unwrap().0.y;
            max_up = max_up.max(vy);
        }
        assert!(max_up > 1.0, "a restitution-0.9 ball should rebound upward: {max_up}");
    }

    #[test]
    fn stack_of_boxes_is_stable() {
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        w.insert_body(PhysicsKey(0), &gd, &gc);
        // Five stacked unit cubes (half-extent 0.5), centres at 0.5,1.5,...
        for i in 0u8..5 {
            let y = 0.5 + f32::from(i);
            let bd = BodyDesc {
                kind: BodyKind::Dynamic,
                world: Mat4::from_translation(Vec3::new(0.0, y, 0.0)),
                ..BodyDesc::default()
            };
            let bc = vec![ColliderDesc {
                shape: PhysicsShape::Cuboid { half_extents: Vec3::splat(0.5) },
                friction: 0.8,
                ..ColliderDesc::default()
            }];
            w.insert_body(PhysicsKey(100 + u64::from(i)), &bd, &bc);
        }
        for _ in 0..300 {
            w.step_once();
        }
        // No box should have exploded sideways or sunk through the floor.
        for i in 0u8..5 {
            let p = w.body_world(PhysicsKey(100 + u64::from(i))).unwrap().w_axis;
            assert!(p.x.abs() < 0.5 && p.z.abs() < 0.5, "box {i} drifted: {p:?}");
            assert!(p.y > 0.0, "box {i} sank: {}", p.y);
        }
    }

    #[test]
    fn kinematic_body_tracks_its_target() {
        let mut w = PhysicsWorld::new(params());
        let bd = BodyDesc { kind: BodyKind::Kinematic, ..BodyDesc::default() };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Cuboid { half_extents: Vec3::splat(0.5) },
            ..ColliderDesc::default()
        }];
        w.insert_body(PhysicsKey(1), &bd, &bc);
        w.set_kinematic_target(PhysicsKey(1), Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0)));
        w.step_once();
        let p = w.body_world(PhysicsKey(1)).unwrap().w_axis;
        assert!((p.x - 3.0).abs() < 1e-3, "kinematic body should reach its target: {p:?}");
        // Kinematic bodies are not authored by physics (not in the dynamic set).
        assert!(w.dynamic_world_poses().is_empty());
    }

    #[test]
    fn advance_clamps_substeps() {
        let mut w = PhysicsWorld::new(SceneParams { max_substeps: 4, ..params() });
        // A huge frame delta would want hundreds of steps; the clamp caps it.
        let ran = w.advance(10.0);
        assert_eq!(ran, 4);
    }

    #[test]
    fn advance_carries_bounded_backlog_after_clamp() {
        // Regression (slow-motion fix): after the substep clamp fires, a bounded
        // remainder is carried instead of zeroing the accumulator. The old code
        // dropped all backlog, so the frame *after* a clamp started empty and
        // under-stepped — sustained slow frames advanced the sim slower than
        // wall-clock forever. With the carry, the next frame is still saturated.
        let mut w = PhysicsWorld::new(SceneParams { max_substeps: 4, ..params() });
        let dt = w.params.dt;
        // A frame far longer than max_substeps·dt clamps; backlog is capped at 2·dt.
        assert_eq!(w.advance(10.0), 4);
        // The next ordinary frame still has carried backlog, so it steps more than
        // the lone step the old drop-to-zero behaviour would have produced.
        assert!(w.advance(dt) >= 2, "carried backlog should keep stepping");
    }

    #[test]
    fn only_dynamic_bodies_are_authored() {
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        w.insert_body(PhysicsKey(1), &gd, &gc); // static
        let (bd, bc) = falling_ball(5.0);
        w.insert_body(PhysicsKey(2), &bd, &bc); // dynamic
        let poses = w.dynamic_world_poses();
        assert_eq!(poses.len(), 1);
        assert_eq!(poses[0].0, PhysicsKey(2));
    }

    #[test]
    fn grabbed_body_stops_falling_when_kinematic() {
        // A falling dynamic body flipped to kinematic (grab suspend) stops
        // integrating gravity and tracks its fed target instead.
        let mut w = PhysicsWorld::new(params());
        let (bd, bc) = falling_ball(10.0);
        w.insert_body(PhysicsKey(1), &bd, &bc);
        for _ in 0..30 {
            w.step_once();
        }
        let y_before = w.body_world(PhysicsKey(1)).unwrap().w_axis.y;
        assert!(y_before < 10.0, "ball should have fallen first: {y_before}");

        // Suspend: now kinematic, no longer authored by physics.
        assert!(w.set_body_kind(PhysicsKey(1), BodyKind::Kinematic));
        assert!(w.dynamic_world_poses().is_empty());
        // Hold it at a fixed pose (what a grab behavior would author each frame).
        let held = Mat4::from_translation(Vec3::new(2.0, 5.0, 0.0));
        for _ in 0..30 {
            w.set_kinematic_target(PhysicsKey(1), held);
            w.step_once();
        }
        let p = w.body_world(PhysicsKey(1)).unwrap().w_axis;
        assert!(
            (p.x - 2.0).abs() < 1e-2 && (p.y - 5.0).abs() < 1e-2,
            "grabbed body should hold its fed pose, not fall: {p:?}"
        );
    }

    #[test]
    fn released_body_resumes_dynamic_and_carries_velocity() {
        // A kinematic (grabbed) body driven sideways, then released to dynamic,
        // keeps the lateral velocity its motion built up (throw) and falls again.
        let mut w = PhysicsWorld::new(params());
        let bd = BodyDesc {
            kind: BodyKind::Kinematic,
            world: Mat4::from_translation(Vec3::new(0.0, 10.0, 0.0)),
            ..BodyDesc::default()
        };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Sphere { radius: 0.5 },
            ..ColliderDesc::default()
        }];
        w.insert_body(PhysicsKey(1), &bd, &bc);

        // Drive it +X each step so rapier derives a +X kinematic velocity.
        let dt = params().dt;
        for i in 1u8..=10 {
            let x = f32::from(i) * dt * 3.0; // 3 units/s along +X
            w.set_kinematic_target(PhysicsKey(1), Mat4::from_translation(Vec3::new(x, 10.0, 0.0)));
            w.step_once();
        }
        let (lin, _) = w.body_velocity(PhysicsKey(1)).unwrap();
        assert!(lin.x > 1.0, "kinematic body should have +X velocity: {lin:?}");

        // Release: resume dynamic, carrying the velocity.
        assert!(w.set_body_kind(PhysicsKey(1), BodyKind::Dynamic));
        let (lin_after, _) = w.body_velocity(PhysicsKey(1)).unwrap();
        assert!(
            (lin_after.x - lin.x).abs() < 1e-3,
            "released body should carry its +X throw velocity: {lin_after:?} vs {lin:?}"
        );
        let x0 = w.body_world(PhysicsKey(1)).unwrap().w_axis.x;
        for _ in 0..20 {
            w.step_once();
        }
        let p = w.body_world(PhysicsKey(1)).unwrap().w_axis;
        assert!(p.x > x0, "thrown body should keep moving +X: {} -> {}", x0, p.x);
        assert!(p.y < 10.0, "released body should fall under gravity: {}", p.y);
        // Dynamic again → authored by physics.
        assert_eq!(w.dynamic_world_poses().len(), 1);
    }

    #[test]
    fn set_body_kind_noops_on_absent_or_unchanged() {
        let mut w = PhysicsWorld::new(params());
        assert!(!w.set_body_kind(PhysicsKey(42), BodyKind::Kinematic), "absent key");
        let (bd, bc) = falling_ball(5.0);
        w.insert_body(PhysicsKey(1), &bd, &bc);
        assert!(!w.set_body_kind(PhysicsKey(1), BodyKind::Dynamic), "already dynamic");
    }

    #[test]
    fn ball_rests_on_a_static_cone() {
        // Regression for "ball/cube falls through the cone": a static cone (apex
        // up at y=+0.5, base disc at y=-0.5, base radius 1) with a ball dropped
        // straight down the axis. Cone contact must catch it — the ball settles on
        // the apex (centre near y≈1.0 = apex 0.5 + radius 0.5), not sink through.
        let mut w = PhysicsWorld::new(params());
        let cone = (
            BodyDesc { kind: BodyKind::Static, ..BodyDesc::default() },
            vec![ColliderDesc {
                shape: PhysicsShape::Cone { radius: 1.0, half_height: 0.5, axis: Axis::Y },
                ..ColliderDesc::default()
            }],
        );
        assert!(w.insert_body(PhysicsKey(1), &cone.0, &cone.1));
        let (bd, bc) = falling_ball(5.0);
        assert!(w.insert_body(PhysicsKey(2), &bd, &bc));

        for _ in 0..300 {
            w.step_once();
        }
        let y = w.body_world(PhysicsKey(2)).unwrap().w_axis.y;
        assert!((y - 1.0).abs() < 0.15, "ball should rest on the cone apex (~1.0), got {y}");
    }

    #[test]
    fn write_back_preserves_authored_scale() {
        // Regression (PR #16 review): the collider bakes the body's scale, but the
        // write-back pose must still carry that scale or the first physics write
        // snaps a scaled prim to unit size. A dynamic body seeded with a non-unit
        // scale must report that scale from body_world / dynamic_world_poses.
        let mut w = PhysicsWorld::new(params());
        let scale = Vec3::new(2.0, 0.5, 3.0);
        let bd = BodyDesc {
            kind: BodyKind::Dynamic,
            world: Mat4::from_scale_rotation_translation(
                scale,
                glam::Quat::IDENTITY,
                Vec3::new(0.0, 10.0, 0.0),
            ),
            ..BodyDesc::default()
        };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Cuboid { half_extents: Vec3::splat(0.5) },
            ..ColliderDesc::default()
        }];
        w.insert_body(PhysicsKey(1), &bd, &bc);

        let (s, _, _) = w.body_world(PhysicsKey(1)).unwrap().to_scale_rotation_translation();
        assert!((s - scale).length() < 1e-4, "body_world should keep the authored scale: {s:?}");

        w.step_once();
        let (ps, _, _) = w.dynamic_world_poses()[0].1.to_scale_rotation_translation();
        assert!(
            (ps - scale).length() < 1e-4,
            "dynamic_world_poses should keep the authored scale: {ps:?}"
        );
    }

    #[test]
    fn body_without_usable_collider_is_dropped() {
        let mut w = PhysicsWorld::new(params());
        let bd = BodyDesc { kind: BodyKind::Dynamic, ..BodyDesc::default() };
        // Empty-hull collider resolves to no shape.
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Hull { points: vec![] },
            ..ColliderDesc::default()
        }];
        assert!(!w.insert_body(PhysicsKey(1), &bd, &bc));
        assert_eq!(w.body_count(), 0);
    }

    #[test]
    fn seeded_asleep_body_does_not_fall() {
        // A dynamic body seeded `starts_asleep` (USD physics:startsAsleep) must not
        // integrate gravity until woken, so USD rest state and the sim stay in
        // sync (plan Phase 2A).
        let mut w = PhysicsWorld::new(params());
        let bd = BodyDesc {
            kind: BodyKind::Dynamic,
            world: Mat4::from_translation(Vec3::new(0.0, 10.0, 0.0)),
            starts_asleep: true,
            ..BodyDesc::default()
        };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Sphere { radius: 0.5 },
            ..ColliderDesc::default()
        }];
        assert!(w.insert_body(PhysicsKey(1), &bd, &bc));
        assert_eq!(w.is_body_sleeping(PhysicsKey(1)), Some(true));
        let y0 = w.body_world(PhysicsKey(1)).unwrap().w_axis.y;
        for _ in 0..120 {
            w.step_once();
        }
        let y1 = w.body_world(PhysicsKey(1)).unwrap().w_axis.y;
        assert!((y1 - y0).abs() < 1e-4, "asleep body should not fall: {y0} -> {y1}");
        assert_eq!(w.is_body_sleeping(PhysicsKey(1)), Some(true));
        assert_eq!(w.is_body_sleeping(PhysicsKey(99)), None, "absent key");
    }

    #[test]
    fn is_body_sleeping_reflects_rest() {
        // A falling ball is awake; once it settles on the plane rapier's island
        // solver puts it to sleep — the signal the write-back gate reads.
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        w.insert_body(PhysicsKey(1), &gd, &gc);
        let (bd, bc) = falling_ball(5.0);
        w.insert_body(PhysicsKey(2), &bd, &bc);
        w.step_once();
        assert_eq!(w.is_body_sleeping(PhysicsKey(2)), Some(false), "a falling ball is awake");
        for _ in 0..600 {
            w.step_once();
        }
        assert_eq!(w.is_body_sleeping(PhysicsKey(2)), Some(true), "a rested ball should sleep");
    }

    #[test]
    fn tilted_convex_body_settles_and_sleeps() {
        // Regression (cookie jitters on the ground and never rests → grab breaks):
        // a convex body that lands on a slightly-uneven face rocks on residual
        // angular velocity. With global angular/linear damping (SceneParams) that
        // residual bleeds off so the body reaches rest and rapier sleeps it. A
        // cuboid dropped with a small tilt stands in for the cookie's hull.
        let mut w = PhysicsWorld::new(params());
        let (gd, gc) = ground();
        w.insert_body(PhysicsKey(1), &gd, &gc);
        let bd = BodyDesc {
            kind: BodyKind::Dynamic,
            // Small tilt about Z so it lands on an edge and must rock to flat.
            world: Mat4::from_rotation_translation(
                Quat::from_rotation_z(0.15),
                Vec3::new(0.0, 2.0, 0.0),
            ),
            ..BodyDesc::default()
        };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Cuboid { half_extents: Vec3::new(0.5, 0.1, 0.5) },
            ..ColliderDesc::default()
        }];
        assert!(w.insert_body(PhysicsKey(2), &bd, &bc));
        // Generous horizon: it must settle AND sleep within a few seconds of sim.
        for _ in 0..1200 {
            w.step_once();
        }
        assert_eq!(
            w.is_body_sleeping(PhysicsKey(2)),
            Some(true),
            "a tilted body must settle and sleep (damping bleeds off the rocking)"
        );
    }

    #[test]
    fn released_body_is_awake() {
        // set_body_kind(..Dynamic) must leave the body awake (wake_up on release),
        // so a grabbed-then-released body simulates immediately rather than
        // resuming into a stale sleeping island.
        let mut w = PhysicsWorld::new(params());
        let bd = BodyDesc { kind: BodyKind::Kinematic, ..BodyDesc::default() };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Sphere { radius: 0.5 },
            ..ColliderDesc::default()
        }];
        w.insert_body(PhysicsKey(1), &bd, &bc);
        assert!(w.set_body_kind(PhysicsKey(1), BodyKind::Dynamic));
        assert_eq!(
            w.is_body_sleeping(PhysicsKey(1)),
            Some(false),
            "a released body must be awake"
        );
    }

    #[test]
    fn held_kinematic_body_keeps_following_after_resting_still() {
        // REPRO (grab-stops-following after holding / multiple grab cycles): a body
        // suspended for grab becomes KinematicPositionBased and is driven each
        // frame via `set_kinematic_target`. If it is held at a fixed target long
        // enough, rapier's island solver puts the kinematic body to sleep — and a
        // sleeping kinematic body ignores `set_next_kinematic_position`, so when the
        // camera moves again the grabbed prim stops following. The driver must keep
        // it awake.
        let mut w = PhysicsWorld::new(params());
        let bd = BodyDesc {
            kind: BodyKind::Dynamic,
            world: Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0)),
            ..BodyDesc::default()
        };
        let bc = vec![ColliderDesc {
            shape: PhysicsShape::Sphere { radius: 0.5 },
            ..ColliderDesc::default()
        }];
        w.insert_body(PhysicsKey(1), &bd, &bc);
        // Grab: flip to kinematic and hold at a fixed pose for a long time.
        assert!(w.set_body_kind(PhysicsKey(1), BodyKind::Kinematic));
        let held = Mat4::from_translation(Vec3::new(0.0, 2.0, 0.0));
        for _ in 0..600 {
            w.set_kinematic_target(PhysicsKey(1), held);
            w.step_once();
        }
        // Now "move the camera": feed a new target and step. The body must track it.
        let moved = Mat4::from_translation(Vec3::new(3.0, 2.0, 0.0));
        for _ in 0..10 {
            w.set_kinematic_target(PhysicsKey(1), moved);
            w.step_once();
        }
        let x = w.body_world(PhysicsKey(1)).unwrap().w_axis.x;
        assert!(
            (x - 3.0).abs() < 1.0e-3,
            "a held-then-moved kinematic (grabbed) body must follow its target; got x = {x}"
        );
    }
}
