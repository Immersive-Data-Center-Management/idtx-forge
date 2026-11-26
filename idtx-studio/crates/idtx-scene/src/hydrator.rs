//! [`SceneHydrator`] — incremental ECS projection from `ViewportProjection` deltas.
//!
//! Owns the ECS [`World`] and the entity-by-[`PrimId`] lookup table.
//! Callers (e.g. `idtx-studio-session`) call the two entry points:
//!
//! 1. [`SceneHydrator::apply_topology_delta`] — after every `project_topology_paths` call.
//! 2. [`SceneHydrator::apply_viewport_projection`] — after every `project_viewport` call.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use bevy_ecs::prelude::*;
use glam::{Mat4, Vec3};
#[cfg(feature = "behavior")]
use idtx_behavior_abi::BehaviorKind;
use idtx_usd::{PackedArcKind, PrimId, StageService, TopologyDelta, ViewportProjection};
use thiserror::Error;

#[derive(Component)]
struct AuthoredVisibility;
#[derive(Component)]
struct SampledVisibility(bool);

#[cfg(feature = "behavior")]
use crate::behavior::{attr as behavior_attr, BehaviorRef, BehaviorScope};
use crate::{
    components::{
        BoundsComp, CameraComp, CollisionComp, CubeComp, ExtendedGprimComp,
        InternalRefArc, LightComp, LocallyAuthoredChild, MaterialComp, MeshComp,
        NativeInstanceComp, PackedStageMetadata, PayloadState, PlaneComp, PointInstancerComp,
        PrimNode, SkeletonPoseComp, SkeletonTopologyComp, SkinBindingComp, SphereComp,
        ViewportPickable, VisibilityComp, VisibilityOverride,
        WorldTransform, Xformable,
    },
    metrics::{HydrationMetrics, MetricsSink},
    populator::PopulatorRegistry,
    schema_ext::{SchemaHydrationCtx, SchemaHydrator, SchemaHydratorRegistry},
};

#[cfg(feature = "usd-extension")]
pub(crate) mod collision;
#[cfg(feature = "usd-extension")]
pub(crate) mod widget;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum HydrationError {
    #[error("prim {0:?} referenced in viewport projection is not in the topology world")]
    UnknownPrim(PrimId),
    #[error("prim {0:?} path resolution failed: {1}")]
    PathResolution(PrimId, String),
}

// ── SceneHydrator ─────────────────────────────────────────────────────────────

/// Owns the ECS [`World`] and drives incremental hydration from USD projections.
///
/// # Invariants
///
/// - Every live prim in the topology has exactly one entity in `entities`.
/// - `entities_by_path` is the inverse of the `PrimNode.path` field.
/// - Components are inserted/replaced on `apply_viewport_projection` but
///   never eagerly removed between calls — stale data stays until a fresh
///   projection covers that prim or the entity is despawned.
pub struct SceneHydrator {
    world: World,
    registry: PopulatorRegistry,
    /// `PrimId` → ECS entity for O(1) lookup during projection.
    entities: HashMap<PrimId, Entity>,
    /// USD path string → ECS entity, for material-binding lookups.
    entities_by_path: BTreeMap<String, Entity>,
    /// Runtime-registered schema hydrators (built-in IDTX domains + any
    /// post-compile plugins). Run via [`Self::run_schema_hydrators`].
    schema_hydrators: SchemaHydratorRegistry,
    metrics: MetricsSink,
}

impl std::fmt::Debug for SceneHydrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneHydrator")
            .field("entities", &self.entities.len())
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

/// Returns `true` when `candidate` carries the same geometry as `existing` for
/// the purposes of the render-side mesh cache, so the stable `geometry_version`
/// (and hence the GPU buffer) can be reused.
///
/// Every vertex-buffer input is compared **by value**, not by length. Comparing
/// only array lengths let value-only edits that keep the counts constant slip
/// through — time-sampled / blend-shape point deformation, same-sized index
/// edits, animated UVs / display colors / tangents, time-sampled normals — so
/// the serial stayed put and the renderer kept a GPU buffer full of stale
/// positions and synthesized normals. The synthesized-normal inputs (authored
/// normals, winding, and the crease/corner sharp-feature sets) are compared for
/// the same reason. Transform-only animation changes none of these object-space
/// arrays (the transform lives on `WorldTransform`), so it still short-circuits
/// to the renderer's cheap transform-update path.
fn mesh_geometry_unchanged(existing: &MeshComp, candidate: &MeshComp) -> bool {
    existing.points == candidate.points
        && existing.face_vertex_indices == candidate.face_vertex_indices
        && existing.face_vertex_counts == candidate.face_vertex_counts
        && existing.normals == candidate.normals
        && existing.tangents == candidate.tangents
        && existing.texcoords == candidate.texcoords
        && existing.display_colors == candidate.display_colors
        && existing.left_handed == candidate.left_handed
        && existing.subdivision_scheme == candidate.subdivision_scheme
        && existing.crease_indices == candidate.crease_indices
        && existing.crease_lengths == candidate.crease_lengths
        && existing.crease_sharpnesses == candidate.crease_sharpnesses
        && existing.corner_indices == candidate.corner_indices
        && existing.corner_sharpnesses == candidate.corner_sharpnesses
}

impl SceneHydrator {
    /// Creates a hydrator with the supplied populator registry.
    ///
    /// The schema-hydrator registry is seeded with the built-in collision / HUD
    /// / widget passes when the `usd-extension` feature is on, and left empty
    /// otherwise (a core build awaiting runtime plugin registration via
    /// [`Self::register_schema_hydrator`]).
    #[must_use]
    pub fn new(registry: PopulatorRegistry) -> Self {
        #[cfg(feature = "usd-extension")]
        let schema_hydrators = SchemaHydratorRegistry::with_builtins();
        #[cfg(not(feature = "usd-extension"))]
        let schema_hydrators = SchemaHydratorRegistry::new();
        Self {
            world: World::new(),
            registry,
            entities: HashMap::new(),
            entities_by_path: BTreeMap::new(),
            schema_hydrators,
            metrics: MetricsSink::default(),
        }
    }

    // ── Public accessors ───────────────────────────────────────────────────

    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    #[must_use]
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    #[must_use]
    pub fn entity_for(&self, prim: PrimId) -> Option<Entity> {
        self.entities.get(&prim).copied()
    }

    #[must_use]
    pub fn entity_for_path(&self, path: &str) -> Option<Entity> {
        self.entities_by_path.get(path).copied()
    }

    /// True iff `prim` renders as a single parametric-gprim mesh entry that the
    /// value-only geometry fast path can re-tessellate in place: a Cube, Sphere,
    /// extended gprim (Cylinder / Cone / Capsule) or Plane that is **not**
    /// `prototype_only`. Returns false for a poly `MeshComp` (whose `points` /
    /// topology edits keep the full re-extract path), an instancer prototype
    /// (not a direct `snapshot.meshes` entry), and any prim not in the world.
    #[must_use]
    pub fn is_patchable_gprim(&self, prim: PrimId) -> bool {
        let Some(entity) = self.entity_for(prim) else {
            return false;
        };
        if let Some(c) = self.world.get::<CubeComp>(entity) {
            return !c.prototype_only;
        }
        if let Some(c) = self.world.get::<SphereComp>(entity) {
            return !c.prototype_only;
        }
        if let Some(c) = self.world.get::<ExtendedGprimComp>(entity) {
            return !c.prototype_only;
        }
        if let Some(c) = self.world.get::<PlaneComp>(entity) {
            return !c.prototype_only;
        }
        false
    }

    /// True iff `prim`'s entity carries a [`CollisionComp`] — i.e. the prim
    /// participates in physics as a collider (or a collider-bearing rigid body).
    /// Used to prim-scope physics invalidation: a value-only geometry edit on a
    /// non-collider prim must not trigger any physics work, and an edit on a
    /// collider should drive an incremental per-body reconcile rather than a
    /// whole-stage rebuild.
    #[must_use]
    pub fn prim_has_collision(&self, prim: PrimId) -> bool {
        self.entity_for(prim)
            .is_some_and(|e| self.world.get::<CollisionComp>(e).is_some())
    }

    /// Whether `prim` carries physics-relevant ECS data, including the scene
    /// that activates/configures simulation. Read before hydration as well as
    /// afterwards: removing an API can erase the only evidence of membership.
    #[must_use]
    pub fn prim_has_physics(&self, prim: PrimId) -> bool {
        self.entity_for(prim).is_some_and(|e| {
            self.world.get::<CollisionComp>(e).is_some()
                || self.world.get::<crate::components::CollisionBodyComp>(e).is_some()
                || self.world.get::<crate::components::RigidBodyComp>(e).is_some()
                || self
                    .world
                    .get::<PrimNode>(e)
                    .is_some_and(|node| node.type_name == "PhysicsScene")
        })
    }

    /// Whether any prim in `scope` carries physics-relevant ECS data. This
    /// describes only the current world, not whether the change was neutral;
    /// callers must also account for removals and pre-change membership.
    #[must_use]
    pub fn scope_has_physics(&self, scope: &HashSet<PrimId>) -> bool {
        scope.iter().any(|&prim| self.prim_has_physics(prim))
    }

    /// Entities for `root_path` and every descendant, found by path-prefix match
    /// (there is no bevy parent/child hierarchy in the hydrated world — namespace
    /// nesting is expressed only through the interned path strings). The root is
    /// included when present; the result is empty when `root_path` is absent.
    ///
    /// Callers that must read a whole subtree's geometry from outside this crate
    /// (e.g. the runtime XR controller-mesh sourcing, which merges the geometry
    /// beneath `/XR/Controllers/{Left,Right}`) use this instead of the private
    /// [`Self::descendants_of`], which needs an `Entity` and is `Children`-free.
    #[must_use]
    pub fn subtree_entities(&self, root_path: &str) -> Vec<Entity> {
        let mut out = Vec::new();
        if let Some(&entity) = self.entities_by_path.get(root_path) {
            out.push(entity);
        }
        let prefix = if root_path == "/" { "/".to_owned() } else { format!("{root_path}/") };
        out.extend(self.entities_by_path.range(prefix.clone()..)
            .take_while(|(path, _)| path.starts_with(&prefix))
            .filter(|(path, _)| path.as_str() != root_path).map(|(_, &entity)| entity));
        out
    }

    /// Despawn the entity at `root_path` and every descendant (path-prefix
    /// match, the same reachability as [`Self::subtree_entities`]), clearing
    /// both lookup maps. Returns the number of entities despawned.
    ///
    /// This is the explicit removal path for a structural *delete*. The normal
    /// despawn route is a `removed` set in [`Self::apply_topology_delta`], but
    /// that set is produced by diffing the *previous* `service.topology()`
    /// against a fresh subtree projection — and the session's delete helper
    /// eagerly reprojects the whole topology (to refresh the scene tree) before
    /// the change-drain runs, which drops the deleted subtree from that
    /// baseline. With no baseline the diff can no longer see the removal, so the
    /// entities would otherwise linger in the ECS world and keep feeding stale
    /// `LightComp` / `MeshComp` data into the render extract. The caller invokes
    /// this directly to despawn them.
    pub fn despawn_subtree(&mut self, root_path: &str) -> usize {
        let doomed: HashSet<Entity> = self.subtree_entities(root_path).into_iter().collect();
        if doomed.is_empty() {
            return 0;
        }
        self.entities.retain(|_, entity| !doomed.contains(entity));
        self.entities_by_path
            .retain(|_, entity| !doomed.contains(entity));
        for &entity in &doomed {
            crate::playback::release_player(&mut self.world, entity);
            self.world.despawn(entity);
        }
        self.metrics.note_despawns(u64::try_from(doomed.len()).unwrap_or(u64::MAX));
        doomed.len()
    }

    /// Returns the ECS entity for the USD pseudo-root (`"/"` path).
    #[must_use]
    pub fn pseudo_root_entity(&self) -> Option<Entity> {
        for &entity in self.entities.values() {
            if let Some(node) = self.world.get::<crate::components::PrimNode>(entity)
                && node.is_pseudo_root {
                    return Some(entity);
                }
        }
        None
    }

    #[must_use]
    pub fn metrics(&self) -> &HydrationMetrics {
        self.metrics.get()
    }

    // ── Schema-hydrator registry ─────────────────────────────────────────────

    /// Registers an additional [`SchemaHydrator`] to run each
    /// [`Self::run_schema_hydrators`] pass. Appended after the built-ins; this is
    /// the entry point a runtime-loaded USD extension uses to supply its
    /// Rust-side ECS projection without recompiling `idtx-scene`.
    pub fn register_schema_hydrator(&mut self, hydrator: Arc<dyn SchemaHydrator>) {
        self.schema_hydrators.register(hydrator);
    }

    /// Read-only view of the registered schema hydrators.
    #[must_use]
    pub fn schema_hydrators(&self) -> &SchemaHydratorRegistry {
        &self.schema_hydrators
    }

    /// Runs every registered [`SchemaHydrator`] in registration order, each a
    /// full additive+reconciling pass against `service`. Returns the total
    /// number of components added, refreshed, or removed. A no-op (returns 0) on
    /// a core build with no hydrators registered.
    pub fn run_schema_hydrators(&mut self, service: &StageService) -> usize {
        // Clone the Arc list so the per-hydrator loop can borrow `self.world` /
        // `self.entities` mutably without also holding a borrow on
        // `self.schema_hydrators`.
        let hydrators: Vec<Arc<dyn SchemaHydrator>> =
            self.schema_hydrators.iter().cloned().collect();
        let mut total = 0;
        for hydrator in &hydrators {
            let mut ctx = SchemaHydrationCtx::new(&mut self.world, &self.entities);
            total += hydrator.hydrate(&mut ctx, service);
        }
        total
    }

    /// Builds a [`SchemaHydrationCtx`] over this hydrator's world + entity index,
    /// used by the thin inherent `hydrate_*` wrappers that delegate to a single
    /// built-in [`SchemaHydrator`].
    #[cfg(feature = "usd-extension")]
    pub(crate) fn schema_ctx(&mut self) -> SchemaHydrationCtx<'_> {
        SchemaHydrationCtx::new(&mut self.world, &self.entities)
    }

    /// Subtree-scoped counterpart to [`Self::schema_ctx`]: the built-in
    /// `hydrate_*_scoped` wrappers use this so a localized topology delta only
    /// re-probes the spawned subtree (pass the delta's upsert PrimId set).
    #[cfg(feature = "usd-extension")]
    pub(crate) fn schema_ctx_scoped<'a>(
        &'a mut self,
        scope: &'a HashSet<PrimId>,
    ) -> SchemaHydrationCtx<'a> {
        SchemaHydrationCtx::new_scoped(&mut self.world, &self.entities, scope)
    }

    // ── Topology ───────────────────────────────────────────────────────────

    /// Applies a topology delta produced by `StageService::project_topology_paths`.
    ///
    /// - Spawns an entity for every new prim in `delta.upserts`.
    /// - Updates [`PrimNode`] and packed-arc components on existing entities.
    /// - Despawns entities for every prim in `delta.removed`.
    /// - Detects [`LocallyAuthoredChild`] for prims with root-layer opinions
    ///   inside packed arc boundaries.
    pub fn apply_topology_delta(&mut self, delta: &TopologyDelta, service: &StageService) {
        let timer = MetricsSink::scope();
        let mut despawns: u64 = 0;
        let mut spawns: u64 = 0;

        // Despawn removed entities.
        for &prim in &delta.removed {
            if let Some(entity) = self.entities.remove(&prim) {
                // Also remove from path lookup.
                let path = self.world.get::<PrimNode>(entity).map(|n| n.path.clone());
                if let Some(path) = path {
                    self.entities_by_path.remove(&path);
                }
                crate::playback::release_player(&mut self.world, entity);
                self.world.despawn(entity);
                despawns += 1;
            }
        }

        // Build a set of packed boundary PrimIds from the upsert list so we can
        // detect LocallyAuthoredChild in a single forward pass.
        let packed_boundaries: HashSet<PrimId> = delta
            .upserts
            .iter()
            .filter(|r| r.packed_arc.is_some())
            .map(|r| r.prim)
            .collect();

        // Build parent → prim map for ancestry walk.
        let parent_of: HashMap<PrimId, PrimId> = delta
            .upserts
            .iter()
            .filter_map(|r| r.parent.map(|p| (r.prim, p)))
            .collect();

        for record in &delta.upserts {
            let prim_path = service.path(record.path).unwrap_or("").to_owned();

            let entity = if let Some(e) = self.entities.get(&record.prim).copied() { e } else {
                let e = self.world.spawn_empty().id();
                self.entities.insert(record.prim, e);
                spawns += 1;
                e
            };

            // Update path→entity index (path may change on rename).
            // Remove the stale entry first.
            if let Some(old_path) = self
                .world
                .get::<PrimNode>(entity)
                .map(|n| n.path.clone())
                .filter(|p| p != &prim_path)
            {
                self.entities_by_path.remove(&old_path);
            }
            self.entities_by_path.insert(prim_path.clone(), entity);

            let mut emut = self.world.entity_mut(entity);
            if record.type_name != "BasisCurves" || !record.state.active || !record.state.loaded {
                emut.remove::<crate::CurveComp>();
            }
            emut.insert(PrimNode {
                prim: record.prim,
                path: prim_path,
                parent: record.parent,
                name: record.name.clone(),
                type_name: record.type_name.clone(),
                state: record.state,
                is_pseudo_root: record.is_pseudo_root,
                local_layer_identifier: record.local_layer_identifier.clone(),
            });

            // Xformable: mirrors `prim.IsA<UsdGeomXformable>()` from the C++ shim.
            // Explicit remove handles prims that become non-xformable after a type change.
            if record.is_xformable {
                emut.insert(Xformable);
            } else {
                emut.remove::<Xformable>();
            }

            // PackedStageMetadata for arc boundary prims.
            if let Some(arc) = &record.packed_arc {
                emut.insert(PackedStageMetadata {
                    arc_kind: arc.kind,
                    asset_path: arc.asset_path.clone(),
                    nested_meters_per_unit: None,
                    nested_up_axis: None,
                    nested_prim_count: None,
                });
            }

            // PayloadState for payload arc roots.
            if let Some(arc) = &record.packed_arc
                && arc.kind == PackedArcKind::Payload {
                    emut.insert(PayloadState {
                        loaded: record.state.loaded,
                    });
                }

            // LocallyAuthoredChild: root-layer opinions inside a packed boundary.
            if record.local_layer_identifier.is_some() {
                let has_packed_ancestor =
                    has_packed_ancestor(record.prim, &parent_of, &packed_boundaries);
                if has_packed_ancestor {
                    emut.insert(LocallyAuthoredChild);
                } else {
                    emut.remove::<LocallyAuthoredChild>();
                }
            }

            // InternalRefArc: same-layer reference arc (pseudo-instance pattern).
            match &record.internal_ref_target {
                Some(target) => {
                    emut.insert(InternalRefArc {
                        target_path: target.clone(),
                    });
                }
                None => {
                    emut.remove::<InternalRefArc>();
                }
            }
        }

        self.metrics.note_topology(&timer, spawns, despawns);
    }

    // ── Viewport projection ─────────────────────────────────────────────────

    /// Applies world-transform-only samples from
    /// `StageService::sample_transforms_only()`.  Updates the entity's
    /// `WorldTransform` and, for entities that carry a [`BoundsComp`],
    /// recomputes the world-space AABB (`minimum`/`maximum`) from the
    /// transform-invariant local extents.  All other components are left
    /// untouched.  Silently skips prims that are not (yet) in the topology world.
    ///
    /// The world-space AABB is derived from the world transform, so it goes
    /// stale if only `WorldTransform` is patched — leaving the selection-outline
    /// fallback (for container prims), ray-pick hit tests, and camera framing
    /// pointing at the prim's *old* placement. Recomputing it here keeps the fast
    /// path visually and interactively identical to a full re-projection.
    ///
    /// Returns `(prim_path, raw_usd_row_major_matrix)` pairs for every prim
    /// that was successfully updated, for use by the render snapshot patch path
    /// (`RenderWorldSnapshot::patch_world_matrices`).
    pub fn apply_transform_samples(
        &mut self,
        samples: &[idtx_usd::TransformRecord],
    ) -> Result<Vec<(String, [f32; 16])>, HydrationError> {
        let mut patches = Vec::with_capacity(samples.len());
        for rec in samples {
            let Ok(entity) = self.require_entity(rec.prim) else {
                continue;
            };
            let matrix = mat4_from_usd_row_major(&rec.world_matrix);

            // Read path and existing local extents before the mutable inserts.
            let path = self.world.get::<PrimNode>(entity).map(|n| n.path.clone());
            // Recompute the world-space AABB EXACTLY as the full projection does
            // in the C++ shim (`ComputeExtentFromPlugins` local extent → the 8
            // corners transformed by the local-to-world matrix → component-wise
            // min/max). `local_minimum`/`local_maximum` are transform-invariant
            // and left as-is (they feed the leaf-prim oriented outline directly
            // together with the freshly patched `WorldTransform`).
            let updated_bounds = self.world.get::<BoundsComp>(entity).map(|b| {
                let (minimum, maximum) =
                    world_aabb_from_local(b.local_minimum, b.local_maximum, matrix);
                BoundsComp {
                    minimum,
                    maximum,
                    local_minimum: b.local_minimum,
                    local_maximum: b.local_maximum,
                }
            });

            let mut ent = self.world.entity_mut(entity);
            if let Some(curve) = ent.get::<crate::CurveComp>() {
                let matrix = rec.world_matrix.map(f64::from);
                if curve.source.world_matrix != matrix {
                    let mut source = curve.source.clone();
                    source.world_matrix = matrix;
                    let next = crate::CurveComp::new(source, curve.time_code, curve.geometric_revision + 1);
                    ent.insert(next);
                }
            }
            ent.insert(WorldTransform { matrix });
            if let Some(bounds) = updated_bounds {
                ent.insert(bounds);
            }
            if let Some(p) = path {
                patches.push((p, rec.world_matrix));
            }
        }
        Ok(patches)
    }

    /// Returns `true` if the world currently contains any skinned prim (a
    /// [`SkeletonPoseComp`] on any entity).
    ///
    /// The authoring transform fast path samples world matrices only and does
    /// **not** refresh joint palettes, so a moved ancestor of a skinned subtree
    /// must instead take the projection path (which re-samples poses). Scenes
    /// with no skeletons — the common case — skip that entirely.
    #[must_use]
    pub fn has_skeletons(&mut self) -> bool {
        self.world
            .query::<&SkeletonPoseComp>()
            .iter(&self.world)
            .next()
            .is_some()
    }

    /// Curve arc tables require the native double-precision composed transform.
    pub fn has_curve_under(&self, root: PrimId) -> bool {
        let Some(entity) = self.entities.get(&root) else { return false; };
        let Some(node) = self.world.get::<PrimNode>(*entity) else { return false; };
        let prefix = format!("{}/", node.path.trim_end_matches('/'));
        self.world.iter_entities().any(|e| e.get::<crate::CurveComp>().is_some()
            && e.get::<PrimNode>().is_some_and(|n| n.path == node.path || n.path.starts_with(&prefix)))
    }

    /// Paths of every prim that participates in skinning — a skeleton (carrying a
    /// [`SkeletonPoseComp`]) or a skinned mesh (carrying a [`SkinBindingComp`]).
    ///
    /// The transform-only fast path samples world matrices only and does **not**
    /// resample joint palettes, so a changed root that is an ancestor/descendant
    /// of one of these paths must take the projection path instead. A plain rigid
    /// root (e.g. a physics body) that touches none of these can take the cheap
    /// matrices-only sample even while a skeleton exists elsewhere in the scene —
    /// the per-root scoping the global [`Self::has_skeletons`] gate cannot express.
    #[must_use]
    pub fn skinned_prim_paths(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut q = self
            .world
            .query::<(&PrimNode, Option<&SkeletonPoseComp>, Option<&SkinBindingComp>)>();
        for (node, pose, skin) in q.iter(&self.world) {
            if pose.is_some() || skin.is_some() {
                out.push(node.path.clone());
            }
        }
        out
    }

    /// Applies a skeleton-pose-only projection produced by
    /// [`StageService::sample_skeleton_poses`].
    ///
    /// Updates only [`SkeletonPoseComp`] (the per-frame joint palette) for the
    /// projected skeletons, leaving all mesh geometry, materials, and skin
    /// bindings untouched. Use this for skeletal animation frames where the mesh
    /// topology is static and only the joint transforms change — the GPU
    /// classic-linear path reads the refreshed palette on the next extract.
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::UnknownPrim`] if the projection references a
    /// skeleton `PrimId` not currently in the topology world.
    pub fn apply_skeleton_poses(
        &mut self,
        projection: &ViewportProjection,
    ) -> Result<(), HydrationError> {
        for rec in &projection.skeleton_poses {
            let entity = self.require_entity(rec.skeleton)?;
            self.world.entity_mut(entity).insert(SkeletonPoseComp {
                record: rec.clone(),
            });
        }
        Ok(())
    }

    /// Applies a full or partial viewport projection produced by
    /// `StageService::project_viewport*`.
    ///
    /// Domain-specific components are inserted or replaced. Components not
    /// present in this projection are left unchanged on entities that are not
    /// covered by it (partial reprojection semantics).
    ///
    /// # Errors
    ///
    /// Returns [`HydrationError::UnknownPrim`] if the projection references a
    /// `PrimId` not currently in the topology world — this indicates a bug in
    /// the caller's change-notification handling (topology should always be
    /// refreshed before viewport).
    pub fn apply_viewport_projection(
        &mut self,
        projection: &ViewportProjection,
    ) -> Result<(), HydrationError> {
        let owned = crate::collect_independent_clip_target_prims(&mut self.world);
        if owned.is_empty() { return self.apply_projection_inner(projection); }
        let mut bounded = projection.clone();
        bounded.exclude_prims(&owned);
        self.apply_projection_inner(&bounded)
    }

    /// A controller-owned, already bounded animation sample. Ordinary stage
    /// reprojection goes through `apply_viewport_projection` and cannot overwrite
    /// even paused clip poses. Animation samples also refresh sampled visibility.
    pub fn apply_animation_projection(&mut self, projection: &ViewportProjection) -> Result<(), HydrationError> {
        self.apply_projection_inner(projection)?;
        self.apply_animation_visibility(projection)?;
        Ok(())
    }

    /// Refresh sampled visibility on matrix/palette-only animation paths too.
    /// The caller rebuilds its render snapshot only when effective visibility changed.
    pub fn apply_animation_visibility(&mut self, projection: &ViewportProjection) -> Result<bool, HydrationError> {
        let mut changed = false;
        for record in &projection.visibility {
            let entity = self.require_entity(record.prim)?;
            self.world.entity_mut(entity).insert(SampledVisibility(record.visible));
            let visible = self.compute_effective_visibility(entity);
            if let Some(mut component) = self.world.get_mut::<VisibilityComp>(entity) {
                changed |= component.visible != visible;
                component.visible = visible;
            }
        }
        Ok(changed)
    }

    fn apply_projection_inner(&mut self, projection: &ViewportProjection) -> Result<(), HydrationError> {
        let timer = MetricsSink::scope();
        let mut upserts: u64 = 0;

        // ── Transforms ────────────────────────────────────────────────────
        for rec in &projection.transforms {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(WorldTransform {
                matrix: mat4_from_usd_row_major(&rec.world_matrix),
            });
            upserts += 1;
        }

        // ── Visibility ────────────────────────────────────────────────────
        // `VisibilityComp.visible` is now managed by the ECS override chain
        // (see `initialize_visibility_overrides` and `toggle_prim_visibility`).
        // This section only ensures that:
        //   a) `VisibilityComp` exists so the render layer has something to read,
        //      using the USD computed value as a temporary default until
        //      `initialize_visibility_overrides` is called;
        //   b) `purpose` is kept up-to-date from USD regardless of user overrides.
        //
        // It deliberately does **not** seed a `VisibilityOverride` here. The C++
        // projection emits a record for *every* `UsdGeomImageable` with no
        // visibility filter, so pre-seeding `VisibilityOverride::default()`
        // (== `Inherited`) for all of them made `initialize_visibility_overrides`
        // *skip* every imageable (it only seeds entities that still lack an
        // override) — authored-`invisible` prims then resolved to
        // `Inherited`→visible and rendered anyway. Leaving the override unset lets
        // `initialize_visibility_overrides` derive it from the authored USD state.
        // See docs/xr-controller-hydration-plan.md §4 (Phase-0 finding).
        for rec in &projection.visibility {
            let entity = self.require_entity(rec.prim)?;

            let has_vis_comp = self.world.get::<VisibilityComp>(entity).is_some();

            if has_vis_comp {
                if let Some(mut comp) = self.world.get_mut::<VisibilityComp>(entity) {
                    comp.purpose = rec.purpose;
                    comp.casts_shadow = rec.casts_shadow;
                }
            } else {
                self.world.entity_mut(entity).insert(VisibilityComp {
                    visible: rec.visible,
                    purpose: rec.purpose,
                    casts_shadow: rec.casts_shadow,
                });
            }
            upserts += 1;
        }

        // ── Bounds ────────────────────────────────────────────────────────
        for rec in &projection.bounds {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(BoundsComp {
                minimum: rec.minimum,
                maximum: rec.maximum,
                local_minimum: rec.local_minimum,
                local_maximum: rec.local_maximum,
            });
            upserts += 1;
        }

        for rec in &projection.curves {
            let entity = self.require_entity(rec.prim)?;
            let existing = self.world.get::<crate::CurveComp>(entity);
            if existing.is_none_or(|old| !old.source.geometry_eq(rec) || old.time_code != projection.time_code) {
                let revision = existing.map_or(1, |old| old.geometric_revision + 1);
                self.world.entity_mut(entity).insert(crate::CurveComp::new(rec.clone(), projection.time_code, revision));
            } else if let Some(mut existing) = self.world.get_mut::<crate::CurveComp>(entity) {
                existing.source.eligible = rec.eligible;
                existing.source.start_group.clone_from(&rec.start_group);
                existing.source.end_group.clone_from(&rec.end_group);
            }
            if !rec.prototype_only { self.world.entity_mut(entity).insert(ViewportPickable); }
            upserts += 1;
        }

        // ── Meshes ────────────────────────────────────────────────────────
        for rec in &projection.meshes {
            let entity = self.require_entity(rec.prim)?;
            // Build the candidate component first, then decide its
            // `geometry_version` by comparing against any existing one. Reusing
            // the version lets the render cache take the cheap transform-update
            // path (no GPU rebuild of all vertex/index/uniform buffers) for
            // transform-only animation, where nothing the mesh cache keys on
            // changes. Any change `mesh_geometry_unchanged` flags (counts,
            // normals, winding, crease/corner data) bumps the version so the
            // renderer re-uploads instead of keeping stale synthesized normals.
            let mut comp = MeshComp {
                geometry_version: 0,
                points: rec.points.clone(),
                normals: rec.normals.clone(),
                tangents: rec.tangents.clone(),
                texcoords: rec.texcoords.clone(),
                face_vertex_counts: rec.face_vertex_counts.clone(),
                face_vertex_indices: rec.face_vertex_indices.clone(),
                display_colors: rec.display_colors.clone(),
                material_path: rec.material_path.clone(),
                material_subsets: rec.material_subsets.clone(),
                left_handed: rec.left_handed,
                double_sided: rec.double_sided,
                prototype_only: rec.prototype_only,
                subdivision_scheme: rec.subdivision_scheme.clone(),
                crease_indices: rec.crease_indices.clone(),
                crease_lengths: rec.crease_lengths.clone(),
                crease_sharpnesses: rec.crease_sharpnesses.clone(),
                corner_indices: rec.corner_indices.clone(),
                corner_sharpnesses: rec.corner_sharpnesses.clone(),
            };
            comp.geometry_version = match self.world.get::<MeshComp>(entity) {
                Some(existing) if mesh_geometry_unchanged(existing, &comp) => {
                    existing.geometry_version
                }
                Some(existing) => existing.geometry_version + 1,
                None => 1,
            };
            self.world.entity_mut(entity).insert(comp);
            if !rec.prototype_only {
                self.world.entity_mut(entity).insert(ViewportPickable);
            }
            upserts += 1;
        }

        // ── Cubes ─────────────────────────────────────────────────────────
        for rec in &projection.cubes {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(CubeComp {
                size: rec.size,
                display_colors: rec.display_colors.clone(),
                material_path: rec.material_path.clone(),
                double_sided: rec.double_sided,
                prototype_only: rec.prototype_only,
            });
            if !rec.prototype_only {
                self.world.entity_mut(entity).insert(ViewportPickable);
            }
            upserts += 1;
        }

        // ── Spheres ───────────────────────────────────────────────────────
        for rec in &projection.spheres {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(SphereComp {
                radius: rec.radius,
                display_colors: rec.display_colors.clone(),
                material_path: rec.material_path.clone(),
                double_sided: rec.double_sided,
                prototype_only: rec.prototype_only,
            });
            if !rec.prototype_only {
                self.world.entity_mut(entity).insert(ViewportPickable);
            }
            upserts += 1;
        }

        // ── Extended gprims ───────────────────────────────────────────────
        for rec in &projection.extended_gprims {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(ExtendedGprimComp {
                kind: rec.kind,
                radius: rec.radius,
                height: rec.height,
                axis: rec.axis,
                display_colors: rec.display_colors.clone(),
                material_path: rec.material_path.clone(),
                double_sided: rec.double_sided,
                prototype_only: rec.prototype_only,
            });
            if !rec.prototype_only {
                self.world.entity_mut(entity).insert(ViewportPickable);
            }
            upserts += 1;
        }

        // ── Planes ────────────────────────────────────────────────────────
        for rec in &projection.planes {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(PlaneComp {
                width: rec.width,
                length: rec.length,
                axis: rec.axis,
                display_colors: rec.display_colors.clone(),
                material_path: rec.material_path.clone(),
                double_sided: rec.double_sided,
                prototype_only: rec.prototype_only,
            });
            if !rec.prototype_only {
                self.world.entity_mut(entity).insert(ViewportPickable);
            }
            upserts += 1;
        }

        // ── Materials (keyed by path string) ──────────────────────────────
        for rec in &projection.materials {
            if let Some(&entity) = self.entities_by_path.get(&rec.path) {
                self.world.entity_mut(entity).insert(MaterialComp {
                    record: rec.clone(),
                });
                upserts += 1;
            }
            // Materials not in the topology (anonymous bindings) are silently
            // skipped — the render crate resolves material paths at draw time.
        }

        // ── Cameras ───────────────────────────────────────────────────────
        for rec in &projection.cameras {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(CameraComp {
                record: rec.clone(),
            });
            upserts += 1;
        }

        // ── Lights ────────────────────────────────────────────────────────
        for rec in &projection.lights {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(LightComp {
                record: rec.clone(),
            });
            upserts += 1;
        }

        // ── Skeleton topologies ───────────────────────────────────────────
        for rec in &projection.skeleton_topologies {
            let entity = self.require_entity(rec.skeleton)?;
            self.world.entity_mut(entity).insert(SkeletonTopologyComp {
                record: rec.clone(),
            });
            upserts += 1;
        }

        // ── Skeleton poses ────────────────────────────────────────────────
        for rec in &projection.skeleton_poses {
            let entity = self.require_entity(rec.skeleton)?;
            self.world.entity_mut(entity).insert(SkeletonPoseComp {
                record: rec.clone(),
            });
            upserts += 1;
        }

        // ── Skin bindings ─────────────────────────────────────────────────
        for rec in &projection.skin_bindings {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(SkinBindingComp {
                record: rec.clone(),
            });
            upserts += 1;
        }

        // ── PointInstancers ───────────────────────────────────────────────
        for rec in &projection.point_instancers {
            let entity = self.require_entity(rec.prim)?;
            let runtime_dynamic = self.world.get::<PointInstancerComp>(entity).is_some_and(|c| c.runtime_dynamic);
            self.world.entity_mut(entity).insert(PointInstancerComp {
                prototypes: rec.prototypes.clone(),
                proto_indices: rec.proto_indices.clone(),
                transforms: rec.transforms.clone(),
                ids: rec.ids.clone(),
                mask: rec.mask.clone(),
                opacities: rec.opacities.clone(),
                runtime_dynamic,
            });
            upserts += 1;
        }

        // ── Native scenegraph instances ───────────────────────────────────
        for rec in &projection.native_instances {
            let entity = self.require_entity(rec.prim)?;
            self.world.entity_mut(entity).insert(NativeInstanceComp {
                prototype: rec.prototype,
                purposes: rec.purposes.clone(),
            });
            upserts += 1;
        }

        // ── Custom schema populators ───────────────────────────────────────
        // Call registered populators only for prims covered by this (possibly
        // partial) projection. Scoping to the projection's prim set avoids an
        // O(total entities) scan on every partial reprojection — when custom
        // populators are registered, only the prims that actually changed need
        // re-populating. Use entity map (not query) to avoid borrow conflicts.
        if !self.registry.is_empty() {
            // Build the covered set from all typed records in this projection.
            // Materials are path-keyed and carry no PrimId, so they are skipped.
            let covered: HashSet<PrimId> = projection
                .transforms
                .iter()
                .map(|r| r.prim)
                .chain(projection.visibility.iter().map(|r| r.prim))
                .chain(projection.bounds.iter().map(|r| r.prim))
                .chain(projection.meshes.iter().map(|r| r.prim))
                .chain(projection.cubes.iter().map(|r| r.prim))
                .chain(projection.spheres.iter().map(|r| r.prim))
                .chain(projection.extended_gprims.iter().map(|r| r.prim))
                .chain(projection.cameras.iter().map(|r| r.prim))
                .chain(projection.lights.iter().map(|r| r.prim))
                .chain(projection.skeleton_topologies.iter().map(|r| r.skeleton))
                .chain(projection.skeleton_poses.iter().map(|r| r.skeleton))
                .chain(projection.skin_bindings.iter().map(|r| r.prim))
                .chain(projection.point_instancers.iter().map(|r| r.prim))
                .collect();

            let custom_targets: Vec<(Entity, PrimId, String)> = covered
                .iter()
                .filter_map(|&prim| {
                    let entity = self.entities.get(&prim).copied()?;
                    let type_name = self.world.get::<PrimNode>(entity)?.type_name.clone();
                    self.registry
                        .get(&type_name)
                        .is_some()
                        .then_some((entity, prim, type_name))
                })
                .collect();
            for (entity, prim, type_name) in custom_targets {
                if let Some(populator) = self.registry.get(&type_name).cloned() {
                    let mut emut = self.world.entity_mut(entity);
                    populator.populate(&mut emut, prim, projection);
                    upserts += 1;
                }
            }
        }

        self.metrics.note_viewport(&timer, upserts);

        Ok(())
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    fn require_entity(&self, prim: PrimId) -> Result<Entity, HydrationError> {
        self.entities
            .get(&prim)
            .copied()
            .ok_or(HydrationError::UnknownPrim(prim))
    }

    // ── ECS-managed visibility ─────────────────────────────────────────────

    /// Initialises [`VisibilityOverride`] for every entity that does not yet
    /// have one, reading the per-prim authored visibility from `service`.
    ///
    /// Call once after the initial full hydration and again after each
    /// topology delta that may have spawned new entities.  Existing overrides
    /// (e.g. set by [`toggle_prim_visibility`]) are intentionally preserved.
    ///
    /// After setting overrides, recomputes [`VisibilityComp::visible`] for
    /// every entity via the full parent-chain override walk.
    pub fn initialize_visibility_overrides(&mut self, service: &StageService) {
        let prims: Vec<(Entity, PrimId)> = self
            .entities
            .iter()
            .map(|(&prim, &entity)| (entity, prim))
            .collect();

        let mut any_new = false;
        for (entity, prim_id) in &prims {
            if self.world.get::<VisibilityOverride>(*entity).is_some() {
                continue; // preserve user-set overrides
            }
            let authored_visible = service.prim_authored_visible(*prim_id).unwrap_or(true);
            let override_val = if authored_visible {
                VisibilityOverride::Inherited
            } else {
                VisibilityOverride::Invisible
            };
            self.world.entity_mut(*entity).insert((override_val, AuthoredVisibility));
            any_new = true;
        }
        if any_new {
            self.recompute_all_effective_visibility();
        }
    }

    /// Hydrates a [`BehaviorRef`] onto every entity carrying an authored
    /// compiled-module attribute (the [`behavior_attr::MODULE`] `Asset` of the
    /// `IdtxBehaviorAPI` schema), reading the module `.wasm` asset path per-prim
    /// from `service`.
    ///
    /// This is the seam that activates the authored-behavior runtime path:
    /// the session's `attach_authored_behaviors` loop only sees behaviors that
    /// carry a [`BehaviorRef`], and the [`crate::behavior::BehaviorPopulator`]
    /// cannot hydrate them (the `Populator` interface has no per-attribute
    /// read). Call once after the initial full hydration and again after each
    /// topology delta that may have spawned new entities.
    ///
    /// Entities that already carry a [`BehaviorRef`] are left untouched (so a
    /// re-scan neither clobbers a live attachment nor double-inserts). The
    /// hydrated ref uses the module path as its uuid placeholder and inherits
    /// [`BehaviorRef::new`]'s runtime defaults (enabled, entity scope) — the
    /// host attach loop keys on the manifest uuid it resolves from the module,
    /// not on this field. Returns the number of behaviors hydrated.
    ///
    /// Per-prim FFI reads are acceptable here: hydration is not the hot
    /// per-frame path, and the scan issues exactly one `eval_attribute` call
    /// per entity (the richer per-attachment override attributes are only read
    /// when an attachment is present — none today).
    #[cfg(feature = "behavior")]
    pub fn hydrate_authored_behaviors(&mut self, service: &StageService) -> usize {
        let prims: Vec<(Entity, PrimId)> = self
            .entities
            .iter()
            .map(|(&prim, &entity)| (entity, prim))
            .collect();

        let mut count = 0;
        for (entity, prim_id) in &prims {
            if self.world.get::<BehaviorRef>(*entity).is_some() {
                continue; // preserve an already-hydrated / live attachment
            }
            self.metrics.note_behavior_probe();
            let Some(module) = read_asset_attribute(service, *prim_id, behavior_attr::MODULE)
            else {
                continue;
            };
            self.world
                .entity_mut(*entity)
                .insert(build_authored_behavior_ref(service, *prim_id, module));
            count += 1;
        }
        count
    }

    /// Reconcile live [`BehaviorRef`] components against the currently-authored
    /// `idtx:behavior:*` attributes after a wholesale root-layer swap (undo/redo).
    ///
    /// Unlike [`Self::hydrate_authored_behaviors`] — which is *additive* and
    /// deliberately preserves any component already present — this is the
    /// *authoritative* resync used when the root layer text is replaced out
    /// from under the ECS world (structural undo/redo restores a snapshot, so
    /// an authored attachment can appear or vanish without a topology change,
    /// and the value-only drain path never re-hydrates behaviors). It:
    ///
    /// * **ADDS / REFRESHES** an authored ref (rebuilt with its current
    ///   enabled/kind/scope overrides) wherever `idtx:behavior:module` is authored;
    /// * **REMOVES** an authored ref (`module_asset == Some`) wherever the
    ///   attribute is now gone;
    /// * **NEVER touches** a host-embedded ref (`module_asset == None`, e.g.
    ///   the XR-preview HMD→camera sync behavior) — it has no authored USD
    ///   backing to reconcile against.
    ///
    /// Returns the number of components added, removed, or refreshed.
    #[cfg(feature = "behavior")]
    pub fn reconcile_authored_behaviors(&mut self, service: &StageService) -> usize {
        self.reconcile_authored_behaviors_scoped(service, None)
    }

    #[cfg(feature = "behavior")]
    pub fn reconcile_authored_behaviors_scoped(&mut self, service: &StageService, scope: Option<&HashSet<PrimId>>) -> usize {
        let prims: Vec<(Entity, PrimId)> = match scope {
            Some(scope) => scope.iter().filter_map(|&prim| self.entity_for(prim).map(|entity| (entity, prim))).collect(),
            None => self.entities.iter().map(|(&prim, &entity)| (entity, prim)).collect(),
        };

        let mut changed = 0;
        for (entity, prim_id) in &prims {
            self.metrics.note_behavior_probe();
            let module = read_asset_attribute(service, *prim_id, behavior_attr::MODULE);
            // `None` = no ref; `Some(true)` = authored ref; `Some(false)` = embedded.
            let existing_authored = self
                .world
                .get::<BehaviorRef>(*entity)
                .map(|b| b.module_asset.is_some());
            match (module, existing_authored) {
                // Attribute authored: (re)build the authored ref, but never
                // clobber a host-embedded ref that shadows the same entity.
                (Some(module), None | Some(true)) => {
                    self.world
                        .entity_mut(*entity)
                        .insert(build_authored_behavior_ref(service, *prim_id, module));
                    changed += 1;
                }
                // Authored ref whose backing attribute is gone (undo of attach).
                (None, Some(true)) => {
                    self.world.entity_mut(*entity).remove::<BehaviorRef>();
                    changed += 1;
                }
                // Embedded ref present, or nothing to do.
                (Some(_), Some(false)) | (None, Some(false) | None) => {}
            }
        }
        changed
    }

    /// Hydrate [`PlayerRole`] components from the authored `idtx:playerRole`
    /// (+ optional `idtx:playerId`) attributes.
    ///
    /// Mirrors [`Self::hydrate_authored_behaviors`]: the [`Populator`] interface
    /// has no per-attribute read, so this per-prim FFI-scan is where the work
    /// happens. One `eval_attribute` call per entity for the role token; the id
    /// is only read when a role is actually authored, so unowned prims (the vast
    /// majority) cost exactly one scan each.
    ///
    /// Additive and idempotent-ish: it (re)stamps `PlayerRole` wherever a role
    /// is authored and **removes** a stale `PlayerRole` where the attribute has
    /// since disappeared, so it doubles as the reconcile path after a root-layer
    /// swap. Returns the number of components added, refreshed, or removed.
    ///
    /// [`Populator`]: crate::populator::Populator
    pub fn hydrate_player_roles(&mut self, service: &StageService) -> usize {
        self.hydrate_player_roles_impl(service, None)
    }

    /// Subtree-scoped variant of [`Self::hydrate_player_roles`]: probes only the
    /// prims in `scope` (the newly-spawned / retopologised set from a localized
    /// topology delta). Reconcile semantics are preserved because `scope` must be
    /// the topology-delta upsert set, which includes changed-in-place prims — not
    /// only brand-new ones.
    pub fn hydrate_player_roles_scoped(
        &mut self,
        service: &StageService,
        scope: &HashSet<PrimId>,
    ) -> usize {
        self.hydrate_player_roles_impl(service, Some(scope))
    }

    fn hydrate_player_roles_impl(
        &mut self,
        service: &StageService,
        scope: Option<&HashSet<PrimId>>,
    ) -> usize {
        let prims: Vec<(Entity, PrimId)> = self
            .entities
            .iter()
            .filter(|(prim, _)| scope.is_none_or(|s| s.contains(prim)))
            .map(|(&prim, &entity)| (entity, prim))
            .collect();

        let mut changed = 0;
        for (entity, prim_id) in &prims {
            let authored_role =
                read_token_attribute(service, *prim_id, crate::player::PLAYER_ROLE_ATTR);
            match authored_role {
                Some(role_token) => {
                    let role = crate::player::NetRole::from_token(&role_token);
                    let player_id =
                        read_token_attribute(service, *prim_id, crate::player::PLAYER_ID_ATTR)
                            .unwrap_or_default();
                    let next = crate::player::PlayerRole { role, player_id };
                    // Skip the write when nothing changed (avoids spurious
                    // change-detection churn on repeated hydration passes).
                    if self.world.get::<crate::player::PlayerRole>(*entity) != Some(&next) {
                        self.world.entity_mut(*entity).insert(next);
                        changed += 1;
                    }
                }
                None => {
                    if self
                        .world
                        .get::<crate::player::PlayerRole>(*entity)
                        .is_some()
                    {
                        self.world
                            .entity_mut(*entity)
                            .remove::<crate::player::PlayerRole>();
                        changed += 1;
                    }
                }
            }
        }
        changed
    }

    

    ///    back to `Inherited` so a future invisible on this prim hides them too.
    ///
    /// Always recomputes [`VisibilityComp::visible`] for all entities after
    /// applying the change.
    pub fn toggle_prim_visibility(&mut self, entity: Entity) {
        let effective = self.compute_effective_visibility(entity);
        self.world.entity_mut(entity).remove::<AuthoredVisibility>();

        if effective {
            // Visible → make invisible.
            self.world
                .entity_mut(entity)
                .insert(VisibilityOverride::Invisible);
        } else {
            // Invisible → make visible.
            let parent_effective = self.parent_effective_visibility(entity);
            let new_override = if parent_effective && self.world.get::<SampledVisibility>(entity).is_none_or(|v| v.0) {
                // Parent chain is visible — Inherited suffices.
                VisibilityOverride::Inherited
            } else {
                // Parent is invisible — need an explicit Visible to override it.
                VisibilityOverride::Visible
            };
            self.world.entity_mut(entity).insert(new_override);

            // Rule 3: clear descendant explicit-Visible overrides so that a
            // future invisible on this ancestor will hide them again.
            self.reset_visible_descendants(entity);
        }

        self.recompute_all_effective_visibility();
    }

    /// Forces `entity`'s [`VisibilityOverride`] to an **absolute** state —
    /// `Visible` (explicit, wins even under an invisible ancestor) or
    /// `Invisible` — then recomputes effective visibility for all entities.
    ///
    /// Unlike [`toggle_prim_visibility`][Self::toggle_prim_visibility], this is
    /// idempotent and does not read the current [`VisibilityComp`], so a caller
    /// driving a prim to a known visibility does not depend on that value being
    /// correctly seeded. It authors an **explicit** `Visible` (which wins even
    /// under an invisible ancestor), so it is the right call for forcing a prim
    /// visible against inherited state — e.g. flipping an XR controller prim
    /// visible from the session layer.
    pub fn set_prim_visibility_override(&mut self, entity: Entity, visible: bool) {
        self.world.entity_mut(entity).remove::<AuthoredVisibility>();
        let override_val = if visible {
            VisibilityOverride::Visible
        } else {
            VisibilityOverride::Invisible
        };
        self.world.entity_mut(entity).insert(override_val);
        self.recompute_all_effective_visibility();
    }

    /// Recomputes [`VisibilityComp::visible`] for every entity by walking the
    /// parent chain to determine the effective override.
    fn recompute_all_effective_visibility(&mut self) {
        let entity_vis: Vec<(Entity, bool)> = self
            .entities
            .values()
            .map(|&entity| (entity, self.compute_effective_visibility(entity)))
            .collect();
        for (entity, vis) in entity_vis {
            if let Some(mut comp) = self.world.get_mut::<VisibilityComp>(entity) {
                comp.visible = vis;
            }
        }
    }

    /// Walks from `entity` up through `PrimNode.parent` links, returning the
    /// first non-`Inherited` override decision (`true` = visible, `false` =
    /// invisible).  Defaults to `true` at the root.
    fn compute_effective_visibility(&self, entity: Entity) -> bool {
        let sampled = self.world.get::<SampledVisibility>(entity).map(|v| v.0);
        let mut current = entity;
        for _ in 0..64_u32 {
            let Some(node) = self.world.get::<PrimNode>(current) else {
                return sampled.unwrap_or(true);
            };
            match if sampled.is_some() && self.world.get::<AuthoredVisibility>(current).is_some() {
                VisibilityOverride::Inherited
            } else { self.world.get::<VisibilityOverride>(current).copied().unwrap_or_default() }
            {
                VisibilityOverride::Visible => return true,
                VisibilityOverride::Invisible => return false,
                VisibilityOverride::Inherited => {
                    match node.parent.and_then(|p| self.entities.get(&p).copied()) {
                        Some(parent_entity) => current = parent_entity,
                        None => return sampled.unwrap_or(true),
                    }
                }
            }
        }
        sampled.unwrap_or(true)
    }

    /// Returns the effective visibility of `entity`'s parent, or `true` if
    /// the entity has no parent (top-level prim).
    fn parent_effective_visibility(&self, entity: Entity) -> bool {
        let Some(node) = self.world.get::<PrimNode>(entity) else {
            return true;
        };
        match node.parent.and_then(|p| self.entities.get(&p).copied()) {
            Some(parent_entity) => self.compute_effective_visibility(parent_entity),
            None => true,
        }
    }

    /// Collects all entities whose prim path is a descendant of `entity`'s path.
    fn descendants_of(&self, entity: Entity) -> Vec<Entity> {
        let Some(root_node) = self.world.get::<PrimNode>(entity) else {
            return Vec::new();
        };
        let prefix = format!("{}/", root_node.path);
        self.entities_by_path
            .range(prefix.clone()..)
            .take_while(|(path, _)| path.starts_with(&prefix))
            .map(|(_, &e)| e)
            .collect()
    }

    /// Resets all descendant entities with [`VisibilityOverride::Visible`] back
    /// to [`VisibilityOverride::Inherited`] (Rule 3).
    fn reset_visible_descendants(&mut self, entity: Entity) {
        let descendants = self.descendants_of(entity);
        for desc in descendants {
            if self.world.get::<VisibilityOverride>(desc).copied()
                == Some(VisibilityOverride::Visible)
            {
                self.world
                    .entity_mut(desc)
                    .insert(VisibilityOverride::Inherited);
            }
        }
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Reads an `Asset`-typed attribute's authored path from `service`, stripping
/// the USD `@…@` value-repr delimiters.
///
/// [`StageService::eval_attribute`] streams the `VtValue` through USD's
/// `operator<<`, which prints an `SdfAssetPath` as `@<path>@`. We recover the
/// authored path as the text between the first and last `@`. Returns `None`
/// when the attribute is absent, carries no authored value, or resolves to an
/// empty path.
#[must_use]
pub fn read_asset_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<String> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    let repr = value.value_repr.trim();
    let path = match (repr.find('@'), repr.rfind('@')) {
        (Some(first), Some(last)) if last > first => &repr[first + 1..last],
        _ => repr,
    }
    .trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_owned())
    }
}

/// Reads a `Bool`-typed attribute from `service`. USD streams a `bool`
/// `VtValue` as `0`/`1`; the Rust authoring side (`PropertyValue::Bool`) uses
/// `true`/`false`. Both spellings are accepted. Returns `None` when the
/// attribute is absent or unparseable.
#[must_use]
pub fn read_bool_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<bool> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    match value.value_repr.trim() {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Reads a `Token`- (or `String`-) typed attribute from `service`, trimming any
/// surrounding quotes USD's `operator<<` may print. Returns `None` when the
/// attribute is absent or resolves to an empty token.
#[must_use]
pub fn read_token_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<String> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    let token = value.value_repr.trim().trim_matches('"').trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// Reads a `float`-typed attribute from `service`. USD streams float `VtValue`
/// values as decimal strings (e.g. `"0.8"`, `"1"`). Returns `None` when the
/// attribute is absent or the text cannot be parsed.
#[must_use]
pub fn read_float_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<f32> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    value.value_repr.trim().parse::<f32>().ok()
}

/// Reads an `int`-typed attribute from `service` as `i32` (e.g. a field's
/// `idtx:widget:precision`). Returns `None` when the attribute has no authored or
/// fallback value, or the text cannot be parsed.
#[must_use]
pub fn read_int_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<i32> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    value.value_repr.trim().parse::<i32>().ok()
}

/// Reads a `double`-typed attribute from `service` as `f32`. USD streams a
/// double `VtValue` as a decimal string; the collision-shape dimensions
/// (`idtx:collision:<shape>:*`) are doubles but the collider math is `f32`.
/// Returns `None` when the attribute has no authored or fallback value, or the
/// text cannot be parsed — so an unauthored dimension with no schema default
/// (e.g. `sphere:radius`) naturally yields `None`.
#[must_use]
pub fn read_double_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<f32> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    value.value_repr.trim().parse::<f64>().ok().map(|v| v as f32)
}

/// Reads a `UsdGeomCylinder`/`Cone`/`Capsule` collider gprim's own geometry
/// (`radius` / `height` / `axis`) into an [`AxialDims`](crate::collision::AxialDims).
///
/// `UsdGeomCapsule.height` is the cylinder-segment length, but the collider math
/// (and `ray_capsule`) want the **tip-to-tip** half-extent, so when `is_capsule`
/// the height is converted to `segment + 2·radius`. Cylinder/cone heights are the
/// full axis extent and pass through unchanged.
#[cfg(feature = "usd-extension")]
#[must_use]
pub fn read_axial(
    service: &StageService,
    prim: PrimId,
    is_capsule: bool,
) -> crate::collision::AxialDims {
    use idtx_usd::GprimAxis;
    let radius = read_double_attribute(service, prim, "radius");
    let height = read_double_attribute(service, prim, "height");
    let axis = read_token_attribute(service, prim, "axis").and_then(|t| GprimAxis::from_token(&t));
    let height = if is_capsule {
        match (height, radius) {
            (Some(h), Some(r)) => Some(h + 2.0 * r),
            (h, _) => h,
        }
    } else {
        height
    };
    crate::collision::AxialDims {
        axis,
        height,
        radius,
    }
}

/// Reads an `int2`-typed attribute from `service`. USD streams `GfVec2i` as
/// `"(x, y)"`. Returns `None` when the attribute is absent or the text does
/// not parse as two integers separated by a comma.
#[must_use]
pub fn read_int2_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<[i32; 2]> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    let s = value
        .value_repr
        .trim()
        .trim_matches(|c: char| c == '(' || c == ')');
    let mut parts = s.split(',').map(|p| p.trim().parse::<i32>());
    let x = parts.next()?.ok()?;
    let y = parts.next()?.ok()?;
    Some([x, y])
}

/// Reads a `float2`-typed attribute from `service` (e.g. `idtx:widget:size`).
/// USD streams `GfVec2f` as `"(x, y)"`. Returns `None` when absent or the text
/// does not parse as two floats.
#[must_use]
pub fn read_float2_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<[f32; 2]> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    let s = value
        .value_repr
        .trim()
        .trim_matches(|c: char| c == '(' || c == ')');
    let mut parts = s.split(',').map(|p| p.trim().parse::<f32>());
    let x = parts.next()?.ok()?;
    let y = parts.next()?.ok()?;
    Some([x, y])
}

/// Reads a `color3f`/`float3`-typed attribute from `service` (e.g.
/// `idtx:widget:color`). USD streams `GfVec3f` as `"(r, g, b)"`. Returns `None`
/// when absent or the text does not parse as three floats.
#[must_use]
pub fn read_color3f_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<[f32; 3]> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    let s = value
        .value_repr
        .trim()
        .trim_matches(|c: char| c == '(' || c == ')');
    let mut parts = s.split(',').map(|p| p.trim().parse::<f32>());
    let r = parts.next()?.ok()?;
    let g = parts.next()?.ok()?;
    let b = parts.next()?.ok()?;
    Some([r, g, b])
}

/// Reads a `string`-typed attribute from `service`, trimming USD's surrounding
/// quotes. Unlike [`read_token_attribute`] this returns `Some("")` for an
/// authored-empty string rather than `None`, so an empty label round-trips.
#[must_use]
pub fn read_string_attribute(service: &StageService, prim: PrimId, attr_name: &str) -> Option<String> {
    let value = service.eval_attribute(prim, attr_name, 0.0, true).ok()??;
    Some(value.value_repr.trim().trim_matches('"').to_owned())
}



/// Builds an authored [`BehaviorRef`] for `module`, layering on any authored
/// per-attachment overrides (`idtx:behavior:enabled` / `:kind` / `:scope`).
///
/// The uuid keys off the module path (the host resolves the manifest uuid from
/// the module bytes). `enabled` defaults to `true`, `kind` to
/// [`BehaviorKind::Runtime`], and `scope` to [`BehaviorScope::Entity`] when the
/// corresponding override attribute is absent. Per-parameter overrides
/// (`idtx:behavior:param:<name>`) are not read here; they need
/// authored-property enumeration, not a single-attribute read.
#[cfg(feature = "behavior")]
fn build_authored_behavior_ref(
    service: &StageService,
    prim: PrimId,
    module: String,
) -> BehaviorRef {
    let mut bref = BehaviorRef::new(module.clone()).with_module_asset(module);
    if let Some(enabled) = read_bool_attribute(service, prim, behavior_attr::ENABLED) {
        bref.enabled = enabled;
    }
    if let Some(kind) = read_token_attribute(service, prim, behavior_attr::KIND) {
        bref = bref.with_kind(BehaviorKind::from_token(&kind));
    }
    if let Some(scope) = read_token_attribute(service, prim, behavior_attr::SCOPE) {
        bref = bref.with_scope(BehaviorScope::from_token(&scope));
    }
    if let Some(server_authoritative) =
        read_bool_attribute(service, prim, behavior_attr::SERVER_AUTHORITATIVE)
    {
        bref = bref.with_server_authoritative(server_authoritative);
    }
    // Carry the authoring source (`.rs`) alongside the compiled module when one
    // is authored, so tooling can round-trip source ↔ module without a filename
    // convention. Informational only — the tick loop keys off `module_asset`.
    if let Some(source) = read_asset_attribute(service, prim, behavior_attr::SOURCE) {
        bref = bref.with_source_asset(source);
    }
    bref
}

/// Converts an `OpenUSD` row-major flat matrix (16 floats) to a glam column-major
/// `Mat4`. `OpenUSD` `GfMatrix4d` stores `m[row][col]` in row-major order; glam
/// uses column-major storage.
#[must_use]
/// World-space axis-aligned bounding box of a local-space AABB transformed by
/// `matrix`.
///
/// Transforms the 8 corners of the local box through `matrix` and takes the
/// component-wise min/max. This mirrors the world-bound derivation in the C++
/// projection shim (`usd_shim.cpp`: `ComputeExtentFromPlugins` local extent →
/// local-to-world corner transform → min/max), so a transform-fast-path
/// recompute produces the same world AABB a full re-projection would — correct
/// even for rotated/scaled prims (an axis-aligned box in world space that
/// tightly contains the transformed local box).
fn world_aabb_from_local(
    local_min: [f32; 3],
    local_max: [f32; 3],
    matrix: Mat4,
) -> ([f32; 3], [f32; 3]) {
    let lo = local_min;
    let hi = local_max;
    let corners = [
        [lo[0], lo[1], lo[2]],
        [hi[0], lo[1], lo[2]],
        [lo[0], hi[1], lo[2]],
        [hi[0], hi[1], lo[2]],
        [lo[0], lo[1], hi[2]],
        [hi[0], lo[1], hi[2]],
        [lo[0], hi[1], hi[2]],
        [hi[0], hi[1], hi[2]],
    ];
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for c in &corners {
        let p = matrix.transform_point3(Vec3::from(*c));
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    (min, max)
}

#[must_use]
pub fn mat4_from_usd_row_major(m: &[f32; 16]) -> Mat4 {
    // USD uses row-vector convention (v' = v * M) with translation in the last row.
    // glam uses column-vector convention (v' = M * v) with translation in the last column.
    // The two are transposes of each other: treat the USD row-major flat array as
    // column-major for glam so that USD row i becomes glam column i.
    // Result: glam w_axis = USD row 3 = [tx, ty, tz, 1]. Translation is preserved. ✓
    Mat4::from_cols_array(m)
}

/// Triangulates a USD polygon mesh using fan triangulation.
///
/// Returns `(positions, tex_coords, indices)` suitable for a GPU triangle-list.
/// Faces with fewer than 3 vertices are skipped. The winding order matches the
/// source data; callers should apply `left_handed` orientation separately.
///
/// # Panics
///
/// Does not panic — malformed index references are clamped to zero so the
/// output is always valid geometry (if possibly incorrect).
#[must_use]
pub fn triangulate_mesh(
    points: &[[f32; 3]],
    texcoords: &[[f32; 2]],
    face_vertex_counts: &[i32],
    face_vertex_indices: &[i32],
) -> (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>) {
    let point_count = points.len();
    let uv_count = texcoords.len();
    let total_indices: usize = face_vertex_counts
        .iter()
        .map(|&n|
            // this cast is fine as max() ensures its either positive or 0 and never < 0            
            n.max(0).cast_unsigned() as usize)
        .sum();
    let mut out_pos: Vec<[f32; 3]> = Vec::with_capacity(total_indices);
    let mut out_uv: Vec<[f32; 2]> = Vec::with_capacity(total_indices);
    let mut out_idx: Vec<u32> = Vec::with_capacity(total_indices.saturating_sub(2) * 3);

    let mut face_cursor: usize = 0;
    for &count in face_vertex_counts {
        // this cast is fine as max() ensures its either positive or 0 and never < 0
        let n = count.max(0).cast_unsigned() as usize;
        if n < 3 {
            face_cursor += n;
            continue;
        }
        let base_vertex = out_pos.len().try_into().unwrap_or_default();
        // Emit flat vertex data for this face.
        for k in 0..n {
            let vi = face_vertex_indices
                .get(face_cursor + k)
                .copied()
                .unwrap_or(0)
                .max(0)
                .cast_unsigned() as usize;
            let vi = vi.min(point_count.saturating_sub(1));
            out_pos.push(points[vi]);
            let uv = if uv_count > 0 {
                texcoords[(face_cursor + k).min(uv_count.saturating_sub(1))]
            } else {
                [0.0, 0.0]
            };
            out_uv.push(uv);
        }
        // Fan triangulation: (0,1,2), (0,2,3), …
        for i in 1..((n.try_into().unwrap_or(u32::MAX)) - 1) {
            out_idx.push(base_vertex);
            out_idx.push(base_vertex + i);
            out_idx.push(base_vertex + i + 1);
        }
        face_cursor += n;
    }
    (out_pos, out_uv, out_idx)
}

/// Returns `true` if any ancestor of `prim` in `parent_of` is in
/// `packed_boundaries`. Terminates after at most 64 hops to avoid cycles.
fn has_packed_ancestor(
    prim: PrimId,
    parent_of: &HashMap<PrimId, PrimId>,
    packed_boundaries: &HashSet<PrimId>,
) -> bool {
    let mut current = prim;
    for _ in 0..64_u32 {
        match parent_of.get(&current) {
            Some(&parent) => {
                if packed_boundaries.contains(&parent) {
                    return true;
                }
                current = parent;
            }
            None => return false,
        }
    }
    false
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a bare prim entity at `path` and register it in both lookup maps,
    /// mirroring what `apply_topology_delta` does on an upsert. `slot` gives the
    /// entity a distinct `PrimId`.
    fn spawn_prim(h: &mut SceneHydrator, slot: u32, path: &str) -> (PrimId, Entity) {
        use idtx_usd::StageGeneration;
        let prim = PrimId::from_raw_parts(slot, StageGeneration::from_raw(1));
        let entity = h
            .world
            .spawn(PrimNode {
                prim,
                path: path.to_owned(),
                parent: None,
                name: path.rsplit('/').next().unwrap_or(path).to_owned(),
                type_name: "Xform".to_owned(),
                state: idtx_usd::PrimState {
                    active: true,
                    defined: true,
                    loaded: true,
                },
                is_pseudo_root: false,
                local_layer_identifier: None,
            })
            .id();
        h.entities.insert(prim, entity);
        h.entities_by_path.insert(path.to_owned(), entity);
        (prim, entity)
    }

    #[test]
    fn physics_scope_includes_scene_and_pre_hydration_membership() {
        let mut h = SceneHydrator::new(PopulatorRegistry::default());
        let (prim, entity) = spawn_prim(&mut h, 1, "/World/Body");
        let scope = HashSet::from([prim]);
        assert!(!h.scope_has_physics(&scope));
        h.world.entity_mut(entity).insert(crate::components::RigidBodyComp);
        assert!(h.prim_has_physics(prim));
        h.world.entity_mut(entity).remove::<crate::components::RigidBodyComp>();
        assert!(!h.scope_has_physics(&scope));
        h.world.get_mut::<PrimNode>(entity).unwrap().type_name = "PhysicsScene".into();
        assert!(
            h.scope_has_physics(&scope),
            "a scene is relevant without any collider/body components"
        );
        h.despawn_subtree("/World/Body");
        assert!(!h.prim_has_physics(prim));
    }

    #[test]
    fn despawn_subtree_removes_root_and_descendants() {
        let mut h = SceneHydrator::new(PopulatorRegistry::default());
        let (root_prim, root) = spawn_prim(&mut h, 1, "/World/Light");
        let (child_prim, child) = spawn_prim(&mut h, 2, "/World/Light/Shaping");
        // A sibling whose path merely shares a prefix *string* but is not under
        // "/World/Light" must survive (no false "/World/Light2" match).
        let (sibling_prim, sibling) = spawn_prim(&mut h, 3, "/World/Light2");

        let removed = h.despawn_subtree("/World/Light");

        assert_eq!(removed, 2, "root + one descendant despawned");
        assert!(h.entity_for(root_prim).is_none());
        assert!(h.entity_for(child_prim).is_none());
        assert!(h.entity_for_path("/World/Light").is_none());
        assert!(h.entity_for_path("/World/Light/Shaping").is_none());
        assert!(!h.world.entities().contains(root));
        assert!(!h.world.entities().contains(child));
        // Prefix-only sibling untouched.
        assert_eq!(h.entity_for(sibling_prim), Some(sibling));
        assert_eq!(h.entity_for_path("/World/Light2"), Some(sibling));
    }

    #[test]
    fn despawn_subtree_absent_root_is_noop() {
        let mut h = SceneHydrator::new(PopulatorRegistry::default());
        let (prim, entity) = spawn_prim(&mut h, 1, "/World/Mesh");
        assert_eq!(h.despawn_subtree("/World/Nope"), 0);
        assert_eq!(h.entity_for(prim), Some(entity));
    }

    #[test]
    fn triangulate_quad() {
        let points = [
            [0.0_f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let uvs = [[0.0_f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let (pos, uv, idx) = triangulate_mesh(&points, &uvs, &[4], &[0, 1, 2, 3]);
        assert_eq!(pos.len(), 4); // 4 unique verts for one quad
        assert_eq!(idx.len(), 6); // 2 triangles = 6 indices
        assert_eq!(idx, [0, 1, 2, 0, 2, 3]);
        assert_eq!(uv.len(), 4);
    }

    #[test]
    fn triangulate_two_tris() {
        let points = [
            [0.0_f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.5, 1.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [2.5, 1.0, 0.0],
        ];
        let (pos, _uv, idx) = triangulate_mesh(&points, &[], &[3, 3], &[0, 1, 2, 3, 4, 5]);
        assert_eq!(pos.len(), 6);
        assert_eq!(idx.len(), 6);
    }

    #[test]
    fn triangulate_pentagon() {
        let points: Vec<[f32; 3]> = (0..5_u8)
            .map(|i| {
                let angle = std::f32::consts::TAU * f32::from(i) / 5.0;
                [angle.cos(), angle.sin(), 0.0]
            })
            .collect();
        let indices: Vec<i32> = (0..5_i32).collect();
        let (pos, _uv, idx) = triangulate_mesh(&points, &[], &[5], &indices);
        assert_eq!(pos.len(), 5);
        assert_eq!(idx.len(), 9); // (5-2) * 3
    }

    #[test]
    fn geometry_version_change_detection_compares_vertex_inputs_by_value() {
        fn base() -> MeshComp {
            MeshComp {
                geometry_version: 7,
                points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                normals: vec![[0.0, 0.0, 1.0]; 3],
                tangents: Vec::new(),
                texcoords: Vec::new(),
                face_vertex_counts: vec![3],
                face_vertex_indices: vec![0, 1, 2],
                display_colors: Vec::new(),
                material_path: None,
                material_subsets: Vec::new(),
                left_handed: false,
                double_sided: false,
                prototype_only: false,
                subdivision_scheme: String::new(),
                crease_indices: Vec::new(),
                crease_lengths: Vec::new(),
                crease_sharpnesses: Vec::new(),
                corner_indices: Vec::new(),
                corner_sharpnesses: Vec::new(),
            }
        }
        let existing = base();
        // Identical content → reuse the serial (cheap transform-update path).
        assert!(mesh_geometry_unchanged(&existing, &base()));

        // Normals change while every count stays constant → geometry change, so
        // the renderer re-uploads instead of keeping the stale GPU normals.
        let mut changed_normals = base();
        changed_normals.normals = vec![[0.0, 1.0, 0.0]; 3];
        assert!(!mesh_geometry_unchanged(&existing, &changed_normals));

        // Authored crease edit with identical counts → geometry change (it feeds
        // the synthesized-normal path).
        let mut changed_crease = base();
        changed_crease.crease_indices = vec![0, 1];
        changed_crease.crease_lengths = vec![2];
        changed_crease.crease_sharpnesses = vec![10.0];
        assert!(!mesh_geometry_unchanged(&existing, &changed_crease));

        // Winding flip with identical counts → geometry change (flips synthesized
        // normal direction).
        let mut flipped = base();
        flipped.left_handed = true;
        assert!(!mesh_geometry_unchanged(&existing, &flipped));

        // Point VALUES change while the point count stays constant (time-sampled
        // / blend-shape deformation) → geometry change, or the renderer keeps a
        // GPU buffer full of stale positions and synthesized normals.
        let mut moved_point = base();
        moved_point.points[1] = [2.0, 0.0, 0.0];
        assert!(!mesh_geometry_unchanged(&existing, &moved_point));

        // Same-sized index edit (re-topologized with the same count) → change.
        let mut reindexed = base();
        reindexed.face_vertex_indices = vec![0, 2, 1];
        assert!(!mesh_geometry_unchanged(&existing, &reindexed));

        // Animated UVs / display colors / tangents of the same length → change.
        let mut uv_anim = base();
        uv_anim.texcoords = vec![[0.5, 0.5]; 3];
        assert!(!mesh_geometry_unchanged(&existing, &uv_anim));

        // Subdivision scheme change → change (affects downstream normal handling).
        let mut subdiv = base();
        subdiv.subdivision_scheme = "catmullClark".to_owned();
        assert!(!mesh_geometry_unchanged(&existing, &subdiv));
    }

    #[test]
    fn mat4_identity_roundtrip() {
        let flat: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let mat = mat4_from_usd_row_major(&flat);
        assert_eq!(mat, Mat4::IDENTITY);
    }

    #[test]
    fn mat4_translation_row_major() {
        // OpenUSD row-major translation matrix: translation in row 3, cols 0-2.
        let flat: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 3.0, 5.0, 7.0, 1.0,
        ];
        let mat = mat4_from_usd_row_major(&flat);
        let expected = Mat4::from_translation(glam::Vec3::new(3.0, 5.0, 7.0));
        assert_eq!(mat, expected);
    }

    #[test]
    fn world_aabb_from_local_translates_and_scales() {
        // Unit box scaled 2x then translated (+10, 0, 0): world AABB is the
        // scaled box recentred on the new origin.
        let m =
            Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)) * Mat4::from_scale(Vec3::splat(2.0));
        let (min, max) = world_aabb_from_local([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0], m);
        assert!((min[0] - 8.0).abs() < 1e-5, "min.x {}", min[0]);
        assert!((max[0] - 12.0).abs() < 1e-5, "max.x {}", max[0]);
        assert!((min[1] + 2.0).abs() < 1e-5, "min.y {}", min[1]);
        assert!((max[1] - 2.0).abs() < 1e-5, "max.y {}", max[1]);
    }

    #[test]
    fn world_aabb_from_local_grows_under_rotation() {
        // A 45° rotation about Z of a unit box enlarges its axis-aligned world
        // extent to ±√2 (the corner-transform derivation the shim uses).
        let m = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_4);
        let (min, max) = world_aabb_from_local([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0], m);
        let sqrt2 = std::f32::consts::SQRT_2;
        assert!((max[0] - sqrt2).abs() < 1e-4, "max.x {} vs {sqrt2}", max[0]);
        assert!((min[0] + sqrt2).abs() < 1e-4, "min.x {}", min[0]);
    }

    /// The transform fast path must refresh the world-space AABB, not just the
    /// world matrix — otherwise picking, camera framing, and the container-prim
    /// selection outline keep pointing at the prim's old placement (the exact
    /// symptom: outline stuck at the origin after a gizmo/inspector move).
    #[test]
    fn apply_transform_samples_recomputes_world_aabb() {
        use crate::components::{BoundsComp, WorldTransform};

        let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
        let prim = idtx_usd::PrimId::from_raw_parts(1, idtx_usd::StageGeneration::from_raw(1));

        // Minimal entity: a unit box at the origin with matching local/world
        // extents. `apply_transform_samples` needs only the `PrimId → Entity`
        // registration and the `BoundsComp` local extents.
        let entity = hydrator
            .world
            .spawn((
                WorldTransform {
                    matrix: Mat4::IDENTITY,
                },
                BoundsComp {
                    minimum: [-1.0, -1.0, -1.0],
                    maximum: [1.0, 1.0, 1.0],
                    local_minimum: [-1.0, -1.0, -1.0],
                    local_maximum: [1.0, 1.0, 1.0],
                },
            ))
            .id();
        hydrator.entities.insert(prim, entity);

        // Move +10 in X (USD row-major: translation in row 3).
        let world_matrix = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 0.0, 0.0, 1.0,
        ];
        let sample = idtx_usd::TransformRecord { prim, world_matrix };
        hydrator
            .apply_transform_samples(&[sample])
            .expect("apply_transform_samples");

        let bounds = hydrator
            .world
            .get::<BoundsComp>(entity)
            .expect("bounds present");
        // World AABB shifted with the prim.
        assert!(
            (bounds.minimum[0] - 9.0).abs() < 1e-5,
            "min.x {}",
            bounds.minimum[0]
        );
        assert!(
            (bounds.maximum[0] - 11.0).abs() < 1e-5,
            "max.x {}",
            bounds.maximum[0]
        );
        // Non-moved axes unchanged.
        assert!(
            (bounds.minimum[1] + 1.0).abs() < 1e-5,
            "min.y {}",
            bounds.minimum[1]
        );
        assert!(
            (bounds.maximum[1] - 1.0).abs() < 1e-5,
            "max.y {}",
            bounds.maximum[1]
        );
        // Local extents are transform-invariant.
        assert_eq!(bounds.local_minimum, [-1.0, -1.0, -1.0]);
        assert_eq!(bounds.local_maximum, [1.0, 1.0, 1.0]);

        // World transform was also patched.
        let wt = hydrator.world.get::<WorldTransform>(entity).expect("wt");
        assert_eq!(wt.matrix, mat4_from_usd_row_major(&world_matrix));
    }
}
