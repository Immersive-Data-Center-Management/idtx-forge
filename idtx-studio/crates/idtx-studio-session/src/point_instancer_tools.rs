//! Selected-instancer inspection / cost report and ordinary root-default edits.
//! The runtime player never calls this inspector. Reads are revision-cached and
//! copying into the shell is an Arc clone, not thousands of array clones/frame.
use super::*;
use idtx_usd::point_instancer::{
    MAX_POINT_INSTANCES, PointInstancerInspection, PointInstancerPlacement,
};

#[derive(Debug, Clone)]
pub struct PointInstancerToolState {
    pub inspection: PointInstancerInspection,
    pub revision: u64,
    pub read_only: bool,
    pub guides: bool,
    pub invalid_entries: std::collections::BTreeMap<usize, String>,
    pub prototype_gpu_bytes: usize,
    pub placement_gpu_bytes: usize,
    pub visible_triangles: usize,
    pub draw_batches: usize,
    pub geometry_cost_available: bool,
    /// Existing F2 native-Xform outputs; generation remains in F2's bake tool.
    pub baked_placements: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ToolCache {
    path: String,
    key: (u64, u64, u64, bool, bool),
    value: Arc<PointInstancerToolState>,
}

impl StudioSession {
    pub fn point_instancer_tool_state(&self, path: &str) -> Option<Arc<PointInstancerToolState>> {
        let entity = self.hydrator.entity_for_path(path)?;
        if self.hydrator.world().get::<PrimNode>(entity)?.type_name != "PointInstancer" {
            return None;
        }
        let read_only = self.play_state != PlayState::Authoring;
        let time =
            read_only.then(|| behavior::stage_time_code(self.hydrator.world(), self.pseudo_root));
        let revision = self.service.revision().get();
        let key = (
            revision,
            time.map_or(0, f64::to_bits),
            self.last_snapshot
                .as_ref()
                .map_or(0, |s| s.point_instancer_serial),
            read_only,
            self.point_instancer_guides,
        );
        if let Some(cache) = self
            .point_instancer_tool_cache
            .borrow()
            .as_ref()
            .filter(|c| c.path == path && c.key == key)
        {
            return Some(Arc::clone(&cache.value));
        }
        let inspection = self.service.inspect_point_instancer(path, time).ok()?;
        let mut state = PointInstancerToolState {
            inspection,
            revision,
            read_only,
            guides: self.point_instancer_guides,
            invalid_entries: Default::default(),
            prototype_gpu_bytes: 0,
            placement_gpu_bytes: 0,
            visible_triangles: 0,
            draw_batches: 0,
            geometry_cost_available: self.last_snapshot.is_some(),
            baked_placements: Vec::new(),
        };
        let a = &state.inspection.placement;
        let mut seen = std::collections::HashMap::new();
        let n = a
            .positions
            .len()
            .max(a.ids.len())
            .max(a.prototype_indices.len());
        for i in 0..n {
            let reason = if a
                .positions
                .get(i)
                .is_none_or(|v| v.iter().any(|v| !v.is_finite()))
            {
                Some("missing/non-finite position")
            } else if a
                .prototype_indices
                .get(i)
                .is_none_or(|p| *p < 0 || *p as usize >= state.inspection.prototypes.len())
            {
                Some("invalid prototype index")
            } else if self
                .service
                .prim_at_path(&state.inspection.prototypes[a.prototype_indices[i] as usize])
                .is_none()
            {
                Some("unresolved/unloaded prototype")
            } else if !a.scales.is_empty()
                && a.scales
                    .get(i)
                    .is_none_or(|v| v.iter().any(|v| !v.is_finite()))
            {
                Some("missing/non-finite scale")
            } else if !a.orientations_xyzw.is_empty()
                && a.orientations_xyzw.get(i).is_none_or(|q| {
                    let norm: f64 = q.iter().map(|v| f64::from(*v).powi(2)).sum();
                    !norm.is_finite() || (norm - 1.0).abs() > 0.002
                })
            {
                Some("invalid unit XYZW quaternion")
            } else {
                None
            };
            if let Some(reason) = reason {
                state.invalid_entries.insert(i, reason.into());
            }
            if let Some(&id) = a.ids.get(i) {
                if let Some(previous) = seen.insert(id, i) {
                    state
                        .invalid_entries
                        .insert(previous, "duplicate stable id".into());
                    state
                        .invalid_entries
                        .insert(i, "duplicate stable id".into());
                }
            } else {
                state.invalid_entries.insert(i, "missing stable id".into());
            }
        }
        if let Some(snapshot) = &self.last_snapshot {
            for (&index, meta) in snapshot
                .point_instancer_batches
                .iter()
                .filter(|(_, m)| m.owner_path == path)
            {
                let b = &snapshot.instanced_batches[index];
                state.draw_batches += 1;
                state.prototype_gpu_bytes += b.mesh.positions.len()
                    * std::mem::size_of::<idtx_render::Vertex>()
                    + b.mesh.indices.len() * 4;
                // Worst-case capacity after revealing the whole allocated pool.
                let allocated = self
                    .hydrator
                    .world()
                    .get::<idtx_scene::PointInstancerComp>(entity)
                    .map_or(0, |pi| {
                        pi.proto_indices
                            .iter()
                            .filter(|p| **p == meta.prototype_index)
                            .count()
                    });
                state.placement_gpu_bytes += allocated.max(1).next_power_of_two() * 64;
                state.visible_triangles += b.mesh.indices.len() / 3 * b.instance_transforms.len();
            }
        }
        state.baked_placements = self
            .service
            .topology()
            .iter()
            .filter(|p| p.type_name == "Scope")
            .filter_map(|p| self.service.path_for(p.prim))
            .filter(|p| p.ends_with("/Placements") && p.contains("/IDTX_CurveBake_"))
            .map(str::to_owned)
            .collect();
        let value = Arc::new(state);
        *self.point_instancer_tool_cache.borrow_mut() = Some(ToolCache {
            path: path.into(),
            key,
            value: Arc::clone(&value),
        });
        Some(value)
    }

    pub fn set_point_instancer_guides(&mut self, enabled: bool) {
        self.point_instancer_guides = enabled;
    }

    #[cfg(feature = "authoring")]
    pub fn edit_point_instancer_defaults(
        &mut self,
        path: &str,
        placement: &PointInstancerPlacement,
    ) -> Result<(), SessionError> {
        if self.play_state != PlayState::Authoring {
            return Err(StageError::OpenUsd(
                "runtime-derived instancer values are read-only during play".into(),
            )
            .into());
        }
        self.service
            .author_point_instancer_defaults(path, placement)?;
        self.tick()?;
        Ok(())
    }

    /// Copies F2's *already baked* native Xform placements into native arrays.
    /// No second route generator or procedural particle schema. Defaults become
    /// ordinary authored data; running F2 again requires explicitly importing again.
    #[cfg(feature = "authoring")]
    pub fn import_point_instancer_bake(
        &mut self,
        target: &str,
        output: &str,
        prototype_index: i32,
    ) -> Result<(), SessionError> {
        if self.play_state != PlayState::Authoring {
            return Err(StageError::OpenUsd("stop play before importing placements".into()).into());
        }
        let owner = output
            .strip_suffix("/Placements")
            .ok_or_else(|| StageError::OpenUsd("select an F2 bake output".into()))?;
        if self.service.curve_bake_settings(owner)?.is_none() {
            return Err(StageError::OpenUsd("output is not an F2 bake".into()).into());
        }
        self.service.validate_curve_bakes()?;
        let prim = self
            .service
            .prim_at_path(target)
            .ok_or_else(|| StageError::UnknownPath(target.into()))?;
        let inverse =
            glam::DMat4::from_cols_array(&self.service.get_world_transform_f64(prim)?).inverse();
        let prefix = format!("{output}/Sample_");
        let mut samples: Vec<_> = self
            .service
            .topology()
            .iter()
            .filter(|p| p.type_name == "Xform")
            .filter_map(|p| {
                self.service
                    .path_for(p.prim)?
                    .strip_prefix(&prefix)?
                    .parse::<u32>()
                    .ok()
                    .map(|i| (i, p.prim))
            })
            .collect();
        samples.sort_by_key(|(i, _)| *i);
        if samples.is_empty() || samples.len() > MAX_POINT_INSTANCES {
            return Err(
                StageError::OpenUsd("F2 bake is empty or exceeds 4096 instances".into()).into(),
            );
        }
        let mut placement = PointInstancerPlacement::default();
        for (id, sample) in samples {
            let local = inverse
                * glam::DMat4::from_cols_array(&self.service.get_world_transform_f64(sample)?);
            if !local.is_finite() || local.determinant().abs() < 1e-12 {
                return Err(StageError::InvalidTransform(
                    "singular instancer-local bake transform".into(),
                )
                .into());
            }
            let (scale, rotation, position) = local.to_scale_rotation_translation();
            let rebuilt = glam::DMat4::from_scale_rotation_translation(scale, rotation, position);
            if !local.abs_diff_eq(rebuilt, 1e-7) {
                return Err(StageError::InvalidTransform(
                    "instancer-local bake contains shear; native arrays require TRS".into(),
                )
                .into());
            }
            placement.positions.push(position.as_vec3().to_array());
            placement
                .orientations_xyzw
                .push(rotation.as_quat().to_array());
            placement.scales.push(scale.as_vec3().to_array());
            placement.prototype_indices.push(prototype_index);
            placement.ids.push(i64::from(id));
        }
        self.edit_point_instancer_defaults(target, &placement)
    }

    #[cfg(feature = "editor-overlay")]
    pub(crate) fn point_instancer_placement_lines(&self, out: &mut Vec<idtx_render::OverlayLine>) {
        if !self.point_instancer_guides {
            return;
        }
        let Some(path) = &self.selected_path else {
            return;
        };
        let Some(state) = self.point_instancer_tool_state(path) else {
            return;
        };
        let Some(entity) = self.hydrator.entity_for_path(path) else {
            return;
        };
        let transform = self.projection_basis
            * self
                .hydrator
                .world()
                .get::<idtx_scene::WorldTransform>(entity)
                .map_or(Mat4::IDENTITY, |x| x.matrix);
        let palette = [
            [0.2, 1.0, 0.4, 1.0],
            [0.3, 0.6, 1.0, 1.0],
            [1.0, 0.7, 0.2, 1.0],
            [0.9, 0.3, 1.0, 1.0],
        ];
        for (i, p) in state
            .inspection
            .placement
            .positions
            .iter()
            .enumerate()
            .take(MAX_POINT_INSTANCES)
        {
            let valid = !state.invalid_entries.contains_key(&i);
            let p = if p.iter().all(|v| v.is_finite()) {
                Vec3::from_array(*p)
            } else {
                Vec3::ZERO
            };
            let center = transform.transform_point3(p);
            let visible =
                state.inspection.mask.is_empty() || state.inspection.mask.get(i) == Some(&1);
            let proto = state
                .inspection
                .placement
                .prototype_indices
                .get(i)
                .copied()
                .unwrap_or(0)
                .max(0) as usize;
            let color = if !valid {
                [1.0, 0.1, 0.1, 1.0]
            } else if !visible {
                [0.45, 0.45, 0.45, 1.0]
            } else {
                palette[proto % palette.len()]
            };
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                let delta = axis * 0.04;
                out.push(idtx_render::OverlayLine {
                    a: (center - delta).to_array(),
                    b: (center + delta).to_array(),
                    color,
                });
            }
        }
    }
}
