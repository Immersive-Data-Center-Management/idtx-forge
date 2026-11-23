use super::StageService;
use crate::{
    StageError,
    curve::{self, BasisCurvesRecord, placement::*},
};
use glam::DVec3;
use idtx_usd_sys::bridge::ffi;
use sha2::{Digest, Sha256};

fn error(e: impl std::fmt::Display) -> StageError {
    StageError::OpenUsd(e.to_string())
}

pub fn curve_bake_owner(source: &str, index: u32) -> String {
    let digest: String =
        Sha256::digest(source.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let parent = source.rsplit_once('/').map_or("", |(parent, _)| parent);
    format!("{parent}/IDTX_CurveBake_{}_{}", &digest[..16], index)
}

fn settings(raw: &ffi::CurveBakeRaw) -> PlacementSettings {
    PlacementSettings {
        curve_index: raw.curve_index,
        spacing: raw.spacing,
        start: raw.start,
        window: raw.window,
        lateral: [raw.lateral[0], raw.lateral[1]],
        floor_normal: [raw.floor_normal[0], raw.floor_normal[1], raw.floor_normal[2]],
        half_extents: [raw.half_extents[0], raw.half_extents[1], raw.half_extents[2]],
        orient: raw.orient,
        seed: raw.seed,
    }
}

impl StageService {
    /// Finds ownership after reference namespace remapping, so a copy rebakes
    /// its existing owner instead of leaving a stale inherited bake behind.
    pub fn curve_bake_owner_for(&self, source: &str, index: u32) -> Result<String, StageError> {
        for record in self.topology().iter().filter(|r| r.type_name == "Scope") {
            let Some(path) = self.path(record.path) else {
                continue;
            };
            if !path.rsplit('/').next().is_some_and(|name| name.starts_with("IDTX_CurveBake_")) {
                continue;
            }
            let raw = ffi::read_curve_bake(self.handle_ref()?, path)?;
            if raw.source == source && raw.curve_index == index {
                return Ok(path.into());
            }
        }
        Ok(curve_bake_owner(source, index))
    }
    pub fn curve_bake_is_stale(
        &self,
        owner: &str,
        source: &BasisCurvesRecord,
    ) -> Result<bool, StageError> {
        let raw = ffi::read_curve_bake(self.handle_ref()?, owner)?;
        Ok(!raw.owner.is_empty()
            && (raw.source_digest != source_digest(source, None)
                || raw.settings_digest != settings(&raw).digest()
                || raw.generator != curve::GENERATOR_VERSION))
    }
    pub fn curve_scene_complete(&self) -> Result<bool, StageError> {
        Ok(ffi::curve_scene_complete(self.handle_ref()?)?)
    }

    pub fn curve_bake_settings(
        &self,
        owner: &str,
    ) -> Result<Option<PlacementSettings>, StageError> {
        let raw = ffi::read_curve_bake(self.handle_ref()?, owner)?;
        Ok((!raw.owner.is_empty()).then(|| settings(&raw)))
    }

    /// Default-time bake preview. Fails explicitly for unloaded/muted/unresolved
    /// geometry instead of interpreting absent remote racks as free space.
    pub fn preview_curve_placements(
        &mut self,
        source: &str,
        settings: &PlacementSettings,
    ) -> Result<(BasisCurvesRecord, PlacementPreview), StageError> {
        if !self.curve_scene_complete()? {
            return Err(error(
                "clearance inconclusive: load all payloads, unmute layers and resolve composition errors before baking",
            ));
        }
        self.project_topology()?;
        let prim = self.prim_at_path(source).ok_or_else(|| error("curve source removed"))?;
        let projection = self.project_viewport_roots(&[prim])?;
        let record = projection
            .curves
            .iter()
            .find(|r| r.prim == prim)
            .ok_or_else(|| error("source is not a supported native BasisCurves"))?
            .clone();
        let basis = curve::normalized_stage_matrix(record.metres_per_unit, record.z_up);
        let mut obstacles = Vec::new();
        for bounds in ffi::curve_obstacles(self.handle_ref()?)? {
            let path = bounds.path;
            let mut min = DVec3::splat(f64::INFINITY);
            let mut max = -min;
            for x in [bounds.minimum[0], bounds.maximum[0]] {
                for y in [bounds.minimum[1], bounds.maximum[1]] {
                    for z in [bounds.minimum[2], bounds.maximum[2]] {
                        let p = basis.transform_point3(DVec3::new(x.into(), y.into(), z.into()));
                        min = min.min(p);
                        max = max.max(p);
                    }
                }
            }
            obstacles.push(ClearanceObstacle {
                path: path.into(),
                minimum: min,
                maximum: max,
            });
        }
        let mut preview = preview(&record, None, settings, &obstacles).map_err(error)?;
        if !record.start_group.is_empty() || !record.end_group.is_empty() {
            let roots: Vec<_> = self
                .topology()
                .iter()
                .filter(|r| r.type_name == "BasisCurves")
                .map(|r| r.prim)
                .collect();
            let routes = self.project_viewport_roots(&roots)?;
            let selected = curve::CurveEvaluator::new(&record, settings.curve_index as usize)
                .map_err(error)?;
            for (label, group, distance) in
                [("start", &record.start_group, 0.0), ("end", &record.end_group, selected.length())]
            {
                if group.is_empty() {
                    continue;
                }
                let point = selected
                    .sample_distance(distance, curve::CurveSpace::StageWorldMetres)
                    .map_err(error)?
                    .point;
                let mut matches = 0;
                for route in &routes.curves {
                    for index in 0..route.counts.len() {
                        if route.prim == record.prim && index == settings.curve_index as usize {
                            continue;
                        }
                        let Ok(table) = curve::CurveEvaluator::new(route, index) else {
                            continue;
                        };
                        for (other_group, distance) in
                            [(&route.start_group, 0.0), (&route.end_group, table.length())]
                        {
                            if other_group != group {
                                continue;
                            }
                            matches += 1;
                            let other = table
                                .sample_distance(distance, curve::CurveSpace::StageWorldMetres)
                                .map_err(error)?
                                .point;
                            if other.distance(point) > record.tolerance.max(route.tolerance) {
                                preview.diagnostics.push(format!(
                                    "{label} group '{group}' endpoints do not meet within tolerance"
                                ));
                            }
                        }
                    }
                }
                if matches == 0 {
                    preview
                        .diagnostics
                        .push(format!("{label} group '{group}' has no loaded peer endpoint"));
                }
            }
        }
        Ok((record, preview))
    }

    /// Authors native Xforms and provenance as one isolated-layer commit. The
    /// returned USD layer is exportable; the editor journals the root delta.
    #[cfg(feature = "authoring")]
    pub fn bake_curve_placements(
        &mut self,
        source: &str,
        owner: &str,
        settings: &PlacementSettings,
    ) -> Result<String, StageError> {
        let (record, preview) = self.preview_curve_placements(source, settings)?;
        if let Some((index, path)) = preview.collisions.first() {
            return Err(error(format!(
                "placement {index} footprint overlaps {path}; adjust the route, offsets or clearance"
            )));
        }
        let inverse = curve::normalized_stage_matrix(record.metres_per_unit, record.z_up).inverse();
        let raw = ffi::CurveBakeRaw {
            owner: owner.into(),
            source: source.into(),
            source_digest: preview.source_digest,
            settings_digest: settings.digest(),
            generator: curve::GENERATOR_VERSION.into(),
            curve_index: settings.curve_index,
            spacing: settings.spacing,
            start: settings.start,
            window: settings.window,
            lateral: settings.lateral.to_vec(),
            floor_normal: settings.floor_normal.to_vec(),
            half_extents: settings.half_extents.to_vec(),
            orient: settings.orient,
            seed: settings.seed,
            tolerance: record.tolerance,
            source_revision: self.revision.get(),
            matrices: preview
                .transforms
                .iter()
                .flat_map(|m| (inverse * *m).to_cols_array())
                .collect(),
        };
        Ok(ffi::bake_curve_placements(self.handle.pin_mut(), &raw)?)
    }

    /// Cook/preview gate. Returns owners requiring the documented explicit
    /// rebake path; opening/inspecting a stage never mutates its derived data.
    pub fn stale_curve_bakes(&mut self) -> Result<Vec<String>, StageError> {
        let owners: Vec<_> = self
            .topology()
            .iter()
            .filter(|r| r.type_name == "Scope")
            .filter_map(|r| self.path(r.path).map(str::to_owned))
            .collect();
        let mut stale = Vec::new();
        for owner in owners {
            let raw = ffi::read_curve_bake(self.handle_ref()?, &owner)?;
            if raw.owner.is_empty() {
                continue;
            }
            let Some(prim) = self.prim_at_path(&raw.source) else {
                stale.push(owner);
                continue;
            };
            let projection = self.project_viewport_roots(&[prim])?;
            let fresh = projection.curves.iter().find(|r| r.prim == prim).is_some_and(|r| {
                curve::placement::source_digest(r, None) == raw.source_digest
                    && r.tolerance == raw.tolerance
                    && settings(&raw).digest() == raw.settings_digest
                    && raw.generator == curve::GENERATOR_VERSION
            });
            if !fresh {
                stale.push(owner);
            }
        }
        Ok(stale)
    }

    pub fn validate_curve_bakes(&mut self) -> Result<(), StageError> {
        let stale = self.stale_curve_bakes()?;
        if !stale.is_empty() {
            return Err(error(format!(
                "stale curve placements: {}. Select each source in Conductor and run Bake placements along curve, then save and cook again",
                stale.join(", ")
            )));
        }
        let owners: Vec<_> = self
            .topology()
            .iter()
            .filter(|r| r.type_name == "Scope")
            .filter_map(|r| self.path(r.path).map(str::to_owned))
            .collect();
        for owner in owners {
            let raw = ffi::read_curve_bake(self.handle_ref()?, &owner)?;
            if raw.owner.is_empty() {
                continue;
            }
            let (_, preview) = self.preview_curve_placements(&raw.source, &settings(&raw))?;
            if let Some((i, obstacle)) = preview.collisions.first() {
                return Err(error(format!(
                    "{owner}: placement {i} now overlaps {obstacle}; preview and rebake before cooking"
                )));
            }
        }
        Ok(())
    }
}
