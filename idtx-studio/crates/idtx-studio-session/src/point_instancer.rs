//! Native array commits at the existing deferred edit boundary, and narrow
//! projection/patching that never remarshal prototype geometry for a pulse.
use super::*;
use idtx_behavior_abi::ErrorCode;
use idtx_scene::PointInstancerComp;
use std::collections::{BTreeMap, HashSet};

#[cfg(all(test, feature = "authoring"))]
mod tests;

impl StudioSession {
    #[cfg(feature = "authoring")]
    pub(super) fn apply_instancer_edit(
        &mut self,
        path: &str,
        op: &SessionEditOp,
    ) -> Result<(), SessionError> {
        let SessionEditOp::UpdatePointInstancer {
            ticket,
            generation,
            instances,
            invisible_ids,
        } = op
        else {
            return Ok(());
        };
        if *generation != self.runtime_spawn_generation()
            || !self.behaviors.has_pending_stage_edit(*ticket)
        {
            self.behaviors
                .finish_stage_edit(*ticket, ErrorCode::StaleHandle);
            return Ok(());
        }
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            self.behaviors
                .finish_stage_edit(*ticket, ErrorCode::StaleHandle);
            return Ok(());
        };
        let node = self
            .hydrator
            .world()
            .get::<PrimNode>(entity)
            .expect("resolved prim");
        if !node.state.active
            || !node.state.loaded
            || self.service.prim_at_path(path) != Some(node.prim)
        {
            self.behaviors
                .finish_stage_edit(*ticket, ErrorCode::StaleHandle);
            return Ok(());
        }
        if node.type_name != "PointInstancer" {
            self.behaviors
                .finish_stage_edit(*ticket, ErrorCode::TypeMismatch);
            return Ok(());
        }
        let result = if let Some(entries) = instances {
            let placement = idtx_usd::point_instancer::PointInstancerPlacement {
                positions: entries.iter().map(|i| i.position).collect(),
                orientations_xyzw: entries.iter().map(|i| i.orientation_xyzw).collect(),
                scales: entries.iter().map(|i| i.scale).collect(),
                prototype_indices: entries.iter().map(|i| i.prototype_index as i32).collect(),
                ids: entries.iter().map(|i| i.id).collect(),
                invisible_ids: invisible_ids.clone(),
            };
            self.service.update_point_instancer(path, &placement)
        } else {
            self.service
                .update_point_instancer_mask(path, invisible_ids)
        };
        if result.is_ok()
            && let Some(mut comp) = self
                .hydrator
                .world_mut()
                .get_mut::<PointInstancerComp>(entity)
        {
            comp.runtime_dynamic = true;
        }
        self.behaviors.finish_stage_edit(
            *ticket,
            if result.is_ok() {
                ErrorCode::Ok
            } else {
                ErrorCode::InvalidArgument
            },
        );
        result.map_err(SessionError::from)
    }

    pub(super) fn is_instancer_value_change(c: &idtx_usd::ChangedPrim) -> bool {
        const ALLOWED: u32 = DirtyDomains::INSTANCER.bits() | DirtyDomains::INSPECTION.bits();
        !c.resync && c.domains.contains(DirtyDomains::INSTANCER) && c.domains.bits() & !ALLOWED == 0
    }

    fn queue_instancer_patch(&mut self, path: String, mask_only: bool) {
        if let Some((_, only)) = self
            .snapshot_instancer_patches
            .iter_mut()
            .find(|(p, _)| p == &path)
        {
            *only &= mask_only;
        } else {
            self.snapshot_instancer_patches.push((path, mask_only));
        }
    }

    fn sample_instancer_values(
        &mut self,
        paths: &[String],
        mask_only: bool,
    ) -> Result<(), SessionError> {
        if paths.is_empty() {
            return Ok(());
        }
        let (records, diagnostics) = self.service.sample_point_instancers(paths, mask_only)?;
        self.diagnostics.extend(diagnostics);
        let mut retry = Vec::new();
        for record in records {
            let Some(path) = self.service.path_for(record.prim).map(str::to_owned) else {
                continue;
            };
            let Some(entity) = self.hydrator.entity_for_path(&path) else {
                continue;
            };
            let mut comp = self
                .hydrator
                .world_mut()
                .get_mut::<PointInstancerComp>(entity)
                .ok_or_else(|| idtx_usd::StageError::UnknownPath(path.clone()))?;
            if mask_only {
                // External/reallocation changes can invalidate an old placement
                // cache. Never apply a mask to a differently ordered id array.
                if comp.ids != record.ids
                    || comp.proto_indices != record.proto_indices
                    || comp.prototypes != record.prototypes
                {
                    retry.push(path);
                    continue;
                }
                comp.mask = record.mask;
            } else {
                comp.prototypes = record.prototypes;
                comp.proto_indices = record.proto_indices;
                comp.transforms = record.transforms;
                comp.ids = record.ids;
                comp.mask = record.mask;
            }
            comp.opacities = record.opacities;
            comp.runtime_dynamic = true;
            self.queue_instancer_patch(path, mask_only);
        }
        if !retry.is_empty() {
            self.sample_instancer_values(&retry, false)?;
        }
        Ok(())
    }

    pub(super) fn project_instancer_changes(
        &mut self,
        changes: &[idtx_usd::ChangedPrim],
    ) -> Result<HashSet<idtx_usd::PrimId>, SessionError> {
        let mut changed = std::collections::HashMap::new();
        for c in changes
            .iter()
            .filter(|c| c.domains.contains(DirtyDomains::INSTANCER))
        {
            if let Some(entity) = self.hydrator.entity_for(c.prim)
                && let Some(mut comp) = self
                    .hydrator
                    .world_mut()
                    .get_mut::<PointInstancerComp>(entity)
            {
                // The initial default override may also remove animation tracks
                // and take normal projection. It is already a dynamic pool.
                comp.runtime_dynamic = true;
                if !Self::is_instancer_value_change(c) {
                    continue;
                }
                let only = c.property_name.as_deref() == Some("invisibleIds");
                changed
                    .entry(c.prim)
                    .and_modify(|v| *v &= only)
                    .or_insert(only);
            }
        }
        for mask_only in [false, true] {
            let paths: Vec<_> = changed
                .iter()
                .filter(|(_, v)| **v == mask_only)
                .filter_map(|(p, _)| self.service.path_for(*p).map(str::to_owned))
                .collect();
            self.sample_instancer_values(&paths, mask_only)?;
        }
        Ok(changed.into_keys().collect())
    }

    /// Transform-only paths already sample all descendant world matrices. Native
    /// instancer parents need a placement-buffer patch; prototype roots also need
    /// native ComputeInstanceTransforms (which includes that root transform).
    pub(super) fn refresh_instancers_for_transforms(&mut self, patches: &[(String, [f32; 16])]) {
        let Some(snapshot) = &self.last_snapshot else {
            return;
        };
        if snapshot.point_instancer_batches.is_empty() {
            return;
        }
        let mut owners = BTreeMap::<String, bool>::new();
        for meta in snapshot.point_instancer_batches.values() {
            let proto_path = self
                .hydrator
                .world()
                .get::<PrimNode>(meta.prototype)
                .map(|n| n.path.as_str());
            let geometry_path = self
                .hydrator
                .world()
                .get::<PrimNode>(meta.geometry)
                .map(|n| n.path.as_str());
            let proto_changed = patches.iter().any(|(p, _)| Some(p.as_str()) == proto_path);
            if proto_changed
                || patches
                    .iter()
                    .any(|(p, _)| p == &meta.owner_path || Some(p.as_str()) == geometry_path)
            {
                owners
                    .entry(meta.owner_path.clone())
                    .and_modify(|v| *v |= proto_changed)
                    .or_insert(proto_changed);
            }
        }
        let native: Vec<_> = owners
            .iter()
            .filter(|(_, v)| **v)
            .map(|(p, _)| p.clone())
            .collect();
        if let Err(error) = self.sample_instancer_values(&native, false) {
            tracing::warn!(%error, "PointInstancer transform refresh failed");
            self.snapshot_dirty = true;
        }
        for (path, _) in owners {
            self.queue_instancer_patch(path, false);
        }
    }

    /// Keep prim-level framing/selection bounds current using cached prototype
    /// AABBs. This reads only changed instancers' visible matrices, never USD
    /// meshes or a stage-wide bounds traversal. Individual instances remain
    /// non-entities; these are bounds of the native instancer prim.
    pub(super) fn refresh_instancer_bounds(&mut self, updates: &[(String, bool)]) {
        if updates.is_empty() {
            return;
        }
        let Some(snapshot) = &self.last_snapshot else {
            return;
        };
        let mut bounds = std::collections::HashMap::new();
        for (&index, meta) in snapshot
            .point_instancer_batches
            .iter()
            .filter(|(_, m)| updates.iter().any(|(p, _)| p == &m.owner_path))
        {
            let owner = self
                .hydrator
                .world()
                .get::<idtx_scene::WorldTransform>(meta.owner)
                .map_or(Mat4::IDENTITY, |t| t.matrix);
            let entry = bounds.entry(meta.owner).or_insert_with(|| {
                (
                    owner,
                    (self.projection_basis * owner).inverse(),
                    Vec3::splat(f32::INFINITY),
                    Vec3::splat(f32::NEG_INFINITY),
                )
            });
            let batch = &snapshot.instanced_batches[index];
            let Some((lo, hi)) = batch.mesh.local_aabb else {
                continue;
            };
            for transform in &batch.instance_transforms {
                let (lo, hi) = transform_bounds(lo.into(), hi.into(), entry.1 * *transform);
                entry.2 = entry.2.min(lo);
                entry.3 = entry.3.max(hi);
            }
        }
        for (entity, (world, _, lo, hi)) in bounds {
            if lo.is_finite() && hi.is_finite() && lo.cmple(hi).all() {
                let (minimum, maximum) = transform_bounds(lo, hi, world);
                self.hydrator
                    .world_mut()
                    .entity_mut(entity)
                    .insert(idtx_scene::BoundsComp {
                        minimum: minimum.to_array(),
                        maximum: maximum.to_array(),
                        local_minimum: lo.to_array(),
                        local_maximum: hi.to_array(),
                    });
            } else {
                self.hydrator
                    .world_mut()
                    .entity_mut(entity)
                    .remove::<idtx_scene::BoundsComp>();
            }
        }
    }
}

fn transform_bounds(lo: Vec3, hi: Vec3, matrix: Mat4) -> (Vec3, Vec3) {
    let center = matrix.transform_point3((lo + hi) * 0.5);
    let half = (hi - lo) * 0.5;
    let extent = matrix.x_axis.truncate().abs() * half.x
        + matrix.y_axis.truncate().abs() * half.y
        + matrix.z_axis.truncate().abs() * half.z;
    (center - extent, center + extent)
}
