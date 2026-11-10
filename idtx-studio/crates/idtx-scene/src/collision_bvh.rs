//! Persistent world-space AABB BVH broad-phase for ray picking.
//!
//! The default broad phase ([`crate::picking::pick_prim_filtered`]) is an
//! `O(N)` linear scan over every `ViewportPickable + BoundsComp` entity, run
//! once per cast and amplified by `arc_cast` (one pick per arc segment) and by
//! XR issuing several casts per frame. This module replaces that scan with a
//! bounding-volume hierarchy traversal when the [`CollisionBvh`] resource is
//! present in the [`World`].
//!
//! # Single source of truth
//!
//! The BVH is a **derived projection** of the revisioned [`BoundsComp`] world
//! AABBs — a *cache*, never independent state. The `UsdStage` remains the sole
//! source of truth (per `CLAUDE.md`): [`CollisionBvh::build`] reads the live
//! `BoundsComp`s, and [`CollisionBvh::refit`] re-reads them for the entities a
//! domain-classified change touched. A `TOPOLOGY` notice rebuilds; a
//! `TRANSFORM`/`GEOMETRY` notice refits the affected leaves and propagates the
//! enlarged AABBs to the root.
//!
//! # Equivalence guarantee
//!
//! Leaves run the identical [`crate::picking::ray_aabb`] slab test the linear
//! scan uses, over the identical `ViewportPickable + BoundsComp` entity set.
//! Internal nodes are culled only by a conservative slab test (a parent AABB
//! always contains its children), so [`query_ray`](CollisionBvh::query_ray)
//! returns exactly the `(entity, t)` pairs the linear scan would — the BVH
//! changes *how fast* candidates are found, never *which*. The
//! `bvh_matches_linear_scan` test gates this.
//!
//! Enabled by the `collision-bvh` feature; timing/visit counters by
//! `collision-metrics`.

#![cfg(feature = "collision-bvh")]

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use glam::Vec3;

use crate::components::{BoundsComp, ViewportPickable};
use crate::picking::ray_aabb;

/// Sentinel for "no node" in a [`BvhNode`] index slot (parent of the root,
/// children of a leaf).
const NIL: u32 = u32::MAX;

/// A world-space axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Aabb {
    min: Vec3,
    max: Vec3,
}

impl Aabb {
    const EMPTY: Self = Self {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    fn from_bounds(b: &BoundsComp) -> Self {
        Self {
            min: Vec3::from(b.minimum),
            max: Vec3::from(b.maximum),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    fn centroid(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }
}

/// One node of the hierarchy. Leaves carry an [`Entity`]; internal nodes carry
/// two child indices. `parent` is stored on every node to drive bottom-up
/// refit propagation ([`CollisionBvh::refit`]).
#[derive(Debug, Clone, Copy)]
struct BvhNode {
    aabb: Aabb,
    parent: u32,
    /// `NIL` on a leaf; otherwise the left child index.
    left: u32,
    /// `NIL` on a leaf; otherwise the right child index.
    right: u32,
    /// Valid only when `left == NIL` (leaf).
    entity: Entity,
}

impl BvhNode {
    fn is_leaf(&self) -> bool {
        self.left == NIL
    }
}

/// Timing / traversal counters, maintained only under `collision-metrics`.
///
/// Counters are atomic so [`CollisionBvh::query_ray`] can tally visits through
/// a shared `&CollisionBvh` (the picker holds the BVH as an immutable resource
/// borrow) while the resource stays `Send + Sync`.
#[cfg(feature = "collision-metrics")]
#[derive(Debug, Default)]
pub struct BvhMetrics {
    /// Number of full [`CollisionBvh::build`] calls.
    pub builds: std::sync::atomic::AtomicU64,
    /// Number of [`CollisionBvh::refit`] calls.
    pub refits: std::sync::atomic::AtomicU64,
    /// Wall-clock microseconds of the most recent build.
    pub last_build_us: std::sync::atomic::AtomicU64,
    /// Wall-clock microseconds of the most recent refit.
    pub last_refit_us: std::sync::atomic::AtomicU64,
    /// Number of [`CollisionBvh::query_ray`] calls.
    pub queries: std::sync::atomic::AtomicU64,
    /// Total internal + leaf nodes visited across all queries.
    pub nodes_visited: std::sync::atomic::AtomicU64,
    /// Total leaves that passed the slab test (candidates returned).
    pub candidates: std::sync::atomic::AtomicU64,
}

#[cfg(feature = "collision-metrics")]
impl BvhMetrics {
    /// A snapshot of the live counters, cheap to format into the XR-metrics
    /// JSON dump.
    #[must_use]
    pub fn snapshot(&self) -> BvhMetricsSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        BvhMetricsSnapshot {
            builds: self.builds.load(Relaxed),
            refits: self.refits.load(Relaxed),
            last_build_us: self.last_build_us.load(Relaxed),
            last_refit_us: self.last_refit_us.load(Relaxed),
            queries: self.queries.load(Relaxed),
            nodes_visited: self.nodes_visited.load(Relaxed),
            candidates: self.candidates.load(Relaxed),
        }
    }
}

/// A plain-data copy of [`BvhMetrics`] for serialisation.
#[cfg(feature = "collision-metrics")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BvhMetricsSnapshot {
    pub builds: u64,
    pub refits: u64,
    pub last_build_us: u64,
    pub last_refit_us: u64,
    pub queries: u64,
    pub nodes_visited: u64,
    pub candidates: u64,
}

/// A world-space AABB bounding-volume hierarchy over the pickable colliders,
/// stored as a [`World`] resource so [`crate::picking`] can consult it.
#[derive(Resource)]
pub struct CollisionBvh {
    nodes: Vec<BvhNode>,
    /// Index of the root node, or [`NIL`] when the hierarchy is empty.
    root: u32,
    /// `entity → leaf node index`, for `O(1)` refit of a changed prim.
    entity_leaf: HashMap<Entity, u32>,
    #[cfg(feature = "collision-metrics")]
    metrics: BvhMetrics,
}

impl CollisionBvh {
    /// Builds a fresh hierarchy over every `ViewportPickable + BoundsComp`
    /// entity. Rebuild on a `TOPOLOGY` notice (prims added/removed); use
    /// [`refit`](Self::refit) for value-only `TRANSFORM`/`GEOMETRY` notices.
    #[must_use]
    pub fn build(world: &mut World) -> Self {
        #[cfg(feature = "collision-metrics")]
        let start = std::time::Instant::now();

        let mut leaves: Vec<(Entity, Aabb)> = Vec::new();
        {
            let mut q = world.query_filtered::<(Entity, &BoundsComp), With<ViewportPickable>>();
            for (entity, bounds) in q.iter(world) {
                leaves.push((entity, Aabb::from_bounds(bounds)));
            }
        }

        let mut bvh = Self {
            nodes: Vec::with_capacity(leaves.len().saturating_mul(2)),
            root: NIL,
            entity_leaf: HashMap::with_capacity(leaves.len()),
            #[cfg(feature = "collision-metrics")]
            metrics: BvhMetrics::default(),
        };

        if !leaves.is_empty() {
            bvh.root = bvh.build_recursive(&mut leaves, NIL);
        }

        #[cfg(feature = "collision-metrics")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            bvh.metrics.builds.fetch_add(1, Relaxed);
            bvh.metrics.last_build_us.store(
                u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
                Relaxed,
            );
        }
        bvh
    }

    /// Rebuilds in place over the current world (equivalent to replacing with
    /// [`build`](Self::build) but preserving the metrics counters).
    pub fn rebuild(&mut self, world: &mut World) {
        let fresh = Self::build(world);
        self.nodes = fresh.nodes;
        self.root = fresh.root;
        self.entity_leaf = fresh.entity_leaf;
        #[cfg(feature = "collision-metrics")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            // Fold the one build `fresh` counted into our running totals.
            self.metrics.builds.fetch_add(1, Relaxed);
            self.metrics
                .last_build_us
                .store(fresh.metrics.last_build_us.load(Relaxed), Relaxed);
        }
    }

    /// Refits the leaves of `entities` from their current [`BoundsComp`] and
    /// propagates the (possibly enlarged) AABBs up to the root.
    ///
    /// A bare leaf refit is insufficient: a parent must grow to contain a moved
    /// child or the parent slab test can wrongly cull a ray that now enters the
    /// child — so every touched leaf walks its `parent` chain unioning the two
    /// children at each step. Entities absent from the hierarchy (added since
    /// the last build) are skipped; the caller must rebuild on topology change.
    pub fn refit(&mut self, world: &World, entities: &[Entity]) {
        #[cfg(feature = "collision-metrics")]
        let start = std::time::Instant::now();

        for &entity in entities {
            let Some(&leaf) = self.entity_leaf.get(&entity) else {
                continue;
            };
            let Some(bounds) = world.get::<BoundsComp>(entity) else {
                continue;
            };
            self.nodes[leaf as usize].aabb = Aabb::from_bounds(bounds);
            self.propagate(leaf);
        }

        #[cfg(feature = "collision-metrics")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            self.metrics.refits.fetch_add(1, Relaxed);
            self.metrics.last_refit_us.store(
                u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
                Relaxed,
            );
        }
    }

    /// Walks from `node`'s parent to the root, re-unioning each internal node's
    /// AABB from its two children.
    fn propagate(&mut self, node: u32) {
        let mut p = self.nodes[node as usize].parent;
        while p != NIL {
            let (l, r) = (self.nodes[p as usize].left, self.nodes[p as usize].right);
            let merged = self.nodes[l as usize].aabb.union(self.nodes[r as usize].aabb);
            self.nodes[p as usize].aabb = merged;
            p = self.nodes[p as usize].parent;
        }
    }

    /// Returns the candidate `(entity, t_entry)` pairs whose world AABB the ray
    /// enters within `max_distance` (`<= 0` is unbounded), bit-for-bit matching
    /// the linear scan's spatial pre-filter. The caller applies the component
    /// filters afterward.
    #[must_use]
    pub fn query_ray(&self, origin: Vec3, dir: Vec3, max_distance: f32) -> Vec<(Entity, f32)> {
        let mut out = Vec::new();
        #[cfg(feature = "collision-metrics")]
        let mut visited: u64 = 0;

        if self.root != NIL {
            // Explicit stack — the hierarchy can be deep and we forbid unsafe /
            // avoid recursion on the hot path.
            let mut stack: Vec<u32> = vec![self.root];
            while let Some(idx) = stack.pop() {
                #[cfg(feature = "collision-metrics")]
                {
                    visited += 1;
                }
                let node = self.nodes[idx as usize];
                // Conservative internal/leaf slab cull: a child AABB is contained
                // in its parent, so a parent whose entry is beyond `max_distance`
                // (or missed entirely) can drop its whole subtree safely.
                let Some(t) = ray_aabb(origin, dir, node.aabb.min, node.aabb.max) else {
                    continue;
                };
                if max_distance > 0.0 && t > max_distance {
                    continue;
                }
                if node.is_leaf() {
                    // The leaf repeats the exact `ray_aabb` the linear scan runs
                    // on `BoundsComp` directly, so the returned `t` is identical.
                    out.push((node.entity, t));
                } else {
                    stack.push(node.left);
                    stack.push(node.right);
                }
            }
        }

        #[cfg(feature = "collision-metrics")]
        {
            use std::sync::atomic::Ordering::Relaxed;
            self.metrics.queries.fetch_add(1, Relaxed);
            self.metrics.nodes_visited.fetch_add(visited, Relaxed);
            self.metrics
                .candidates
                .fetch_add(out.len() as u64, Relaxed);
        }
        out
    }

    /// Number of leaves (pickable colliders) indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entity_leaf.len()
    }

    /// Whether the hierarchy indexes no colliders.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root == NIL
    }

    /// A snapshot of the traversal / timing counters (metrics builds only).
    #[cfg(feature = "collision-metrics")]
    #[must_use]
    pub fn metrics(&self) -> BvhMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Recursively splits `leaves` on the longest centroid axis at the median,
    /// pushing nodes into `self.nodes` and returning the index of the subtree
    /// root. Reserves its own slot *before* recursing so children get higher
    /// indices and the parent is backfilled once both subtrees exist.
    fn build_recursive(&mut self, leaves: &mut [(Entity, Aabb)], parent: u32) -> u32 {
        let idx = u32::try_from(self.nodes.len()).expect("BVH node count exceeds u32");
        // Reserve the slot (backfilled below).
        self.nodes.push(BvhNode {
            aabb: Aabb::EMPTY,
            parent,
            left: NIL,
            right: NIL,
            entity: Entity::PLACEHOLDER,
        });

        if leaves.len() == 1 {
            let (entity, aabb) = leaves[0];
            self.nodes[idx as usize].aabb = aabb;
            self.nodes[idx as usize].entity = entity;
            self.entity_leaf.insert(entity, idx);
            return idx;
        }

        // Split on the axis of greatest centroid spread, at the median.
        let mut cbounds = Aabb::EMPTY;
        for (_, aabb) in leaves.iter() {
            let c = aabb.centroid();
            cbounds.min = cbounds.min.min(c);
            cbounds.max = cbounds.max.max(c);
        }
        let extent = cbounds.max - cbounds.min;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        leaves.sort_unstable_by(|a, b| {
            let ca = a.1.centroid()[axis];
            let cb = b.1.centroid()[axis];
            ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
        });

        let mid = leaves.len() / 2;
        let (left_slice, right_slice) = leaves.split_at_mut(mid);
        let left = self.build_recursive(left_slice, idx);
        let right = self.build_recursive(right_slice, idx);

        let aabb = self.nodes[left as usize]
            .aabb
            .union(self.nodes[right as usize].aabb);
        let node = &mut self.nodes[idx as usize];
        node.aabb = aabb;
        node.left = left;
        node.right = right;
        idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{PrimNode, VisibilityComp};
    use crate::picking::{candidate_passes, pick_prim_filtered, RayFilter};
    use idtx_usd::{PrimId, PrimState, StageGeneration};

    fn prim_id(slot: u32) -> PrimId {
        PrimId::from_raw_parts(slot, StageGeneration::from_raw(0))
    }

    fn prim_state() -> PrimState {
        PrimState {
            active: true,
            defined: true,
            loaded: true,
        }
    }

    /// Spawns a pickable entity with a world-space AABB `[min, max]`.
    fn spawn_box(world: &mut World, min: [f32; 3], max: [f32; 3]) -> Entity {
        world
            .spawn((
                BoundsComp {
                    minimum: min,
                    maximum: max,
                    local_minimum: min,
                    local_maximum: max,
                },
                ViewportPickable,
            ))
            .id()
    }

    /// Linear-scan baseline over the raw `BoundsComp`s (no component filters),
    /// mirroring `query_ray`'s contract so the two can be compared directly.
    fn linear_candidates(
        world: &mut World,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
    ) -> Vec<(Entity, f32)> {
        let mut q = world.query_filtered::<(Entity, &BoundsComp), With<ViewportPickable>>();
        q.iter(world)
            .filter_map(|(e, b)| {
                let t = ray_aabb(origin, dir, b.minimum.into(), b.maximum.into())?;
                if max_distance > 0.0 && t > max_distance {
                    return None;
                }
                Some((e, t))
            })
            .collect()
    }

    fn sorted(mut v: Vec<(Entity, f32)>) -> Vec<(Entity, f32)> {
        v.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        v
    }

    /// Deterministic LCG so the equivalence sweep is reproducible.
    struct Lcg(u64);
    impl Lcg {
        #[allow(clippy::cast_precision_loss)] // test-only pseudo-random source
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((self.0 >> 40) as f32) / (1u32 << 24) as f32
        }
        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next_f32()
        }
    }

    #[test]
    fn empty_world_builds_empty_bvh() {
        let mut world = World::new();
        let bvh = CollisionBvh::build(&mut world);
        assert!(bvh.is_empty());
        assert_eq!(bvh.len(), 0);
        assert!(bvh.query_ray(Vec3::ZERO, Vec3::X, 0.0).is_empty());
    }

    #[test]
    fn single_box_hit_and_miss() {
        let mut world = World::new();
        let e = spawn_box(&mut world, [-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]);
        let bvh = CollisionBvh::build(&mut world);
        assert_eq!(bvh.len(), 1);

        let hit = bvh.query_ray(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 0.0);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].0, e);
        assert!((hit[0].1 - 4.0).abs() < 1e-4);

        let miss = bvh.query_ray(Vec3::new(-5.0, 5.0, 0.0), Vec3::X, 0.0);
        assert!(miss.is_empty());
    }

    #[test]
    fn bvh_matches_linear_scan_over_random_scene() {
        let mut world = World::new();
        let mut rng = Lcg(0x1234_5678_9abc_def0);
        for _ in 0..200 {
            let cx = rng.range(-20.0, 20.0);
            let cy = rng.range(-20.0, 20.0);
            let cz = rng.range(-20.0, 20.0);
            let h = rng.range(0.2, 2.0);
            spawn_box(
                &mut world,
                [cx - h, cy - h, cz - h],
                [cx + h, cy + h, cz + h],
            );
        }
        let bvh = CollisionBvh::build(&mut world);

        for _ in 0..400 {
            let origin = Vec3::new(
                rng.range(-30.0, 30.0),
                rng.range(-30.0, 30.0),
                rng.range(-30.0, 30.0),
            );
            let dir = Vec3::new(
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
            )
            .normalize_or_zero();
            if dir == Vec3::ZERO {
                continue;
            }
            for max_d in [0.0_f32, 15.0] {
                let got = sorted(bvh.query_ray(origin, dir, max_d));
                let want = sorted(linear_candidates(&mut world, origin, dir, max_d));
                assert_eq!(got.len(), want.len(), "candidate count differs");
                for (a, b) in got.iter().zip(want.iter()) {
                    assert_eq!(a.0, b.0, "entity mismatch");
                    assert!((a.1 - b.1).abs() < 1e-4, "t mismatch {} vs {}", a.1, b.1);
                }
            }
        }
    }

    #[test]
    fn refit_propagates_enlarged_child_to_root() {
        // Two small far-apart boxes; a ray that initially misses the first box.
        let mut world = World::new();
        let a = spawn_box(&mut world, [-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]);
        let _b = spawn_box(&mut world, [50.0, 50.0, 50.0], [52.0, 52.0, 52.0]);
        let mut bvh = CollisionBvh::build(&mut world);

        // Ray along +X at y=10 — misses box `a` entirely.
        let origin = Vec3::new(-10.0, 10.0, 0.0);
        assert!(bvh.query_ray(origin, Vec3::X, 0.0).is_empty());

        // Grow box `a` upward so the ray now enters it; refit must enlarge the
        // ancestor AABBs or the parent slab test would still cull it.
        world.get_mut::<BoundsComp>(a).unwrap().maximum = [1.0, 12.0, 1.0];
        bvh.refit(&world, &[a]);

        let hit = bvh.query_ray(origin, Vec3::X, 0.0);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].0, a);
    }

    #[test]
    fn pick_prim_filtered_identical_with_and_without_bvh() {
        // Full pick path (incl. component filters + narrow phase) must agree
        // whether or not the BVH resource is installed.
        let mut world = World::new();
        let mut rng = Lcg(0xdead_beef_0bad_f00d);
        for ids in 0..120u32 {
            let cx = rng.range(-15.0, 15.0);
            let cy = rng.range(-15.0, 15.0);
            let cz = rng.range(-15.0, 15.0);
            let h = rng.range(0.3, 1.5);
            let e = spawn_box(
                &mut world,
                [cx - h, cy - h, cz - h],
                [cx + h, cy + h, cz + h],
            );
            // Half the boxes are invisible, to exercise the respect-visibility
            // filter through both enumerations.
            let visible = rng.next_f32() > 0.5;
            world.entity_mut(e).insert((
                VisibilityComp {
                    visible,
                    purpose: idtx_usd::RenderPurpose::Default,
                    casts_shadow: true,
                },
                PrimNode {
                    prim: prim_id(ids),
                    path: format!("/B{ids}"),
                    parent: None,
                    name: format!("B{ids}"),
                    type_name: "Cube".to_owned(),
                    state: prim_state(),
                    is_pseudo_root: false,
                    local_layer_identifier: None,
                },
            ));
        }

        let filter = RayFilter {
            respect_visibility: true,
            ..RayFilter::default()
        };

        // Snapshot the per-component data so `candidate_passes` isn't the thing
        // under test — only the spatial enumeration differs between runs.
        let _ = candidate_passes(None, None, None, &filter);

        for _ in 0..300 {
            let origin = Vec3::new(
                rng.range(-25.0, 25.0),
                rng.range(-25.0, 25.0),
                rng.range(-25.0, 25.0),
            );
            let dir = Vec3::new(
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
            )
            .normalize_or_zero();
            if dir == Vec3::ZERO {
                continue;
            }

            // Without BVH (resource absent).
            world.remove_resource::<CollisionBvh>();
            let without = pick_prim_filtered(&mut world, origin, dir, &filter);

            // With BVH.
            let bvh = CollisionBvh::build(&mut world);
            world.insert_resource(bvh);
            let with = pick_prim_filtered(&mut world, origin, dir, &filter);

            match (without, with) {
                (None, None) => {}
                (Some(x), Some(y)) => {
                    assert_eq!(x.entity, y.entity, "hit entity differs");
                    assert!((x.t - y.t).abs() < 1e-4, "hit t differs {} vs {}", x.t, y.t);
                }
                (a, b) => panic!("hit presence differs: {a:?} vs {b:?}"),
            }
        }
    }
}
