use super::light::caster_shadow_fit;
use super::{RenderWorldSnapshot, ShadowAtlasAllocator};
use bevy_ecs::prelude::*;
use glam::Mat4;
use idtx_scene::hydrator::mat4_from_usd_row_major;
use idtx_scene::{PointInstancerComp, PrimNode, VisibilityComp, WorldTransform};
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Placement {
    slot: usize,
    id: i64,
    world: Mat4,
    visible: bool,
    opacity: f32,
}

/// Stable native identity is `(owner_path, id)`, not a USD prim/entity per instance.
/// Prototype geometry/materials live in the ordinary batch cache. Hidden entries
/// retain their placement here; a pulse only compacts visible instance matrices.
#[derive(Debug, Clone)]
pub struct PointInstancerBatch {
    pub owner_path: String,
    pub owner: Entity,
    pub prototype: Entity,
    pub geometry: Entity,
    pub prototype_index: u32,
    pub axis_transform: Mat4,
    /// Native ids parallel to the rendered (visible) instance transforms.
    pub ids: Vec<i64>,
    /// Visible coverage values, parallel to ids/transforms, never material data.
    pub opacities: Vec<f32>,
    pub version: u64,
    /// Runtime-updated singletons stay out of static draw merging. Static
    /// PointInstancers retain the existing single-instance merge optimization.
    pub dynamic: bool,
    placements: Vec<Placement>,
}

impl PointInstancerBatch {
    pub(super) fn new(
        owner: Entity,
        owner_path: String,
        prototype: Entity,
        geometry: Entity,
        prototype_index: u32,
        ids: Vec<i64>,
        version: u64,
        axis_transform: Mat4,
    ) -> Self {
        Self {
            owner,
            owner_path,
            prototype,
            geometry,
            prototype_index,
            ids,
            opacities: Vec::new(),
            version,
            axis_transform,
            dynamic: false,
            placements: Vec::new(),
        }
    }
}

impl RenderWorldSnapshot {
    /// Patch native instances only. `true` means the batch structure is reusable;
    /// `false` asks the caller for normal extraction (e.g. a prototype changed).
    /// Each update is `(instancer path, mask-only)`. Geometry Arcs, materials and
    /// the structural serial remain untouched, including while ALL ids are hidden.
    pub fn patch_point_instancers(
        &mut self,
        world: &World,
        updates: &[(String, bool)],
        basis: Mat4,
    ) -> bool {
        if updates.is_empty() {
            return true;
        }
        if updates.iter().any(|(path, _)| self.guide_point_instancer_owners.contains(path)) {
            return false;
        }
        // Validate the entire patch before touching the snapshot.
        for (&index, meta) in self
            .point_instancer_batches
            .iter()
            .filter(|(_, m)| updates.iter().any(|(p, _)| p == &m.owner_path))
        {
            if self.instanced_batches[index].purpose != super::extract::instance_purpose(
                world.get::<VisibilityComp>(meta.owner), world.get::<VisibilityComp>(meta.geometry),
            ) {
                return false;
            }
            let Some(pi) = world.get::<PointInstancerComp>(meta.owner) else {
                return false;
            };
            if pi.ids.len() != pi.transforms.len()
                || pi.ids.len() != pi.proto_indices.len()
                || (!pi.mask.is_empty() && pi.mask.len() != pi.ids.len())
                || (!pi.opacities.is_empty() && pi.opacities.len() != pi.ids.len())
                || world.get::<PrimNode>(meta.prototype).map(|n| n.prim)
                    != pi.prototypes.get(meta.prototype_index as usize).copied()
            {
                return false;
            }
        }
        let version =
            super::extract::SNAPSHOT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut changed = false;
        for (&index, meta) in &mut self.point_instancer_batches {
            let Some((_, mask_only)) = updates.iter().find(|(p, _)| p == &meta.owner_path) else {
                continue;
            };
            let pi = world
                .get::<PointInstancerComp>(meta.owner)
                .expect("validated owner");
            if !mask_only || meta.placements.is_empty() {
                let owner = world
                    .get::<WorldTransform>(meta.owner)
                    .map_or(Mat4::IDENTITY, |x| x.matrix);
                let proto = world
                    .get::<WorldTransform>(meta.prototype)
                    .map_or(Mat4::IDENTITY, |x| x.matrix);
                let geometry = world
                    .get::<WorldTransform>(meta.geometry)
                    .map_or(Mat4::IDENTITY, |x| x.matrix);
                let local = proto.inverse() * geometry * meta.axis_transform;
                meta.placements.clear();
                meta.placements.extend(
                    pi.proto_indices
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| **p == meta.prototype_index)
                        .map(|(slot, _)| Placement {
                            slot,
                            id: pi.ids[slot],
                            world: basis
                                * owner
                                * mat4_from_usd_row_major(&pi.transforms[slot])
                                * local,
                            visible: false,
                            opacity: 1.0,
                        }),
                );
            }
            let batch = &mut self.instanced_batches[index];
            let owner_vis = world.get::<VisibilityComp>(meta.owner);
            let geometry_vis = world.get::<VisibilityComp>(meta.geometry);
            batch.purpose = super::extract::instance_purpose(owner_vis, geometry_vis);
            let visible = owner_vis.is_none_or(|v| v.visible) && geometry_vis.is_none_or(|v| v.visible)
                && super::extract::purpose_matches(self.purpose_filter, batch.purpose);
            batch.instance_transforms.clear();
            meta.ids.clear();
            meta.opacities.clear();
            if !pi.opacities.is_empty() { enable_opacity(&mut batch.material); }
            for p in &mut meta.placements {
                p.visible = visible && (pi.mask.is_empty() || pi.mask.get(p.slot) == Some(&1));
                p.opacity = pi.opacities.get(p.slot).copied().unwrap_or(1.0);
                if p.visible && p.opacity > 0.0 {
                    batch.instance_transforms.push(p.world);
                    meta.ids.push(p.id);
                    meta.opacities.push(p.opacity);
                }
            }
            meta.dynamic = pi.runtime_dynamic || !pi.opacities.is_empty();
            meta.version = version;
            changed = true;
        }
        if changed {
            self.point_instancer_serial = version;
            self.stats.instanced_draw_count = self
                .instanced_batches
                .iter()
                .map(|b| b.instance_transforms.len())
                .sum();
            // Native mask/placement changes also invalidate shadow caster bounds.
            if self.shadow.enabled() {
                self.shadow_aabb = crate::camera_fit::compute_scene_aabb(self, true);
                self.shadow_aabb_sig = crate::camera_fit::scene_aabb_signature(self);
                for light in &mut self.lights {
                    let (vp, near, far, size) = caster_shadow_fit(
                        &light.kind,
                        light.shadow_enabled,
                        light.world_matrix,
                        self.shadow_aabb,
                    );
                    light.shadow_view_proj = vp;
                    light.shadow_near = near;
                    light.shadow_far = far;
                    light.light_size = size;
                    light.shadow_slot = None;
                }
                self.shadow_slots = ShadowAtlasAllocator::allocate(&mut self.lights);
            }
        }
        true
    }

    /// Frame-local overrides after F5's placement/mask patch. Reuses cached
    /// placements, geometry and material bindings; zero coverage emits no draw
    /// instances. An absent command restores authored coverage, including zero.
    pub fn apply_point_instance_opacities(&mut self, overrides: &HashMap<String, HashMap<i64, f32>>) {
        let mut changed = false;
        for (batches, metadata) in [
            (&mut self.instanced_batches, &mut self.point_instancer_batches),
            (&mut self.guide_instanced_batches, &mut self.guide_point_instancer_batches),
        ] {
        for (&index, meta) in metadata {
            let values = overrides.get(&meta.owner_path);
            let batch = &mut batches[index];
            let material_changed = values.is_some() && enable_opacity(&mut batch.material);
            if values.is_some() {
                meta.dynamic = true;
            }
            let desired = || meta.placements.iter().filter_map(|p| {
                let a = values.and_then(|v| v.get(&p.id)).copied().unwrap_or(p.opacity);
                (p.visible && a > 0.0).then_some((p, a))
            });
            let same = desired().enumerate().all(|(i, (p, a))|
                meta.ids.get(i) == Some(&p.id) && meta.opacities.get(i) == Some(&a)
                    && batch.instance_transforms.get(i) == Some(&p.world))
                && desired().count() == meta.ids.len();
            if same && !material_changed { continue; }
            batch.instance_transforms.clear();
            meta.ids.clear();
            meta.opacities.clear();
            for p in &meta.placements {
                let a = values.and_then(|v| v.get(&p.id)).copied().unwrap_or(p.opacity);
                if p.visible && a > 0.0 {
                    batch.instance_transforms.push(p.world);
                    meta.ids.push(p.id);
                    meta.opacities.push(a);
                }
            }
            meta.version = super::extract::SNAPSHOT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.point_instancer_serial = meta.version;
            changed = true;
        }
        }
        if changed {
            self.stats.instanced_draw_count = self.instanced_batches.iter().map(|b| b.instance_transforms.len()).sum();
        }
    }
}

fn enable_opacity(material: &mut crate::material::RenderMaterialDesc) -> bool {
    // A stable presence/blend pipeline, also at alpha 0 or 1. Only an explicit
    // authored/runtime instance-coverage opt-in changes the batch material.
    if material.alpha_policy != idtx_usd::AlphaPolicy::Blend || material.opacity_mode != idtx_usd::OpacityMode::Presence {
        material.alpha_policy = idtx_usd::AlphaPolicy::Blend;
        material.opacity_mode = idtx_usd::OpacityMode::Presence;
        material.fingerprint = material.compute_fingerprint();
        return true;
    }
    false
}
