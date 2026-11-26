//! Editor curve operations. Inspector uses only the read-only state accessor.
use crate::{SessionError, StudioSession};
use idtx_scene::CurveComp;
use idtx_usd::curve::placement::{PlacementPreview, PlacementSettings};

#[derive(Debug, Clone)]
pub struct CurveToolState {
    pub curve: CurveComp,
    pub settings: PlacementSettings,
    pub message: String,
}

#[derive(Debug)]
pub(crate) struct CurvePreviewState {
    source: String,
    settings: PlacementSettings,
    preview: PlacementPreview,
    revision: u64,
}

impl StudioSession {
    pub fn select_curve_index(&mut self, path: &str, index: u32) {
        self.curve_tool_index = Some((path.into(), index));
    }
    pub fn curve_tool_state(&self, path: &str) -> Option<CurveToolState> {
        let entity = self.hydrator.entity_for_path(path)?;
        let curve = self.hydrator.world().get::<CurveComp>(entity)?.clone();
        let index =
            self.curve_tool_index.as_ref().filter(|(p, _)| p == path).map_or(0, |(_, i)| *i);
        let owner = self
            .service
            .curve_bake_owner_for(path, index)
            .unwrap_or_else(|_| idtx_usd::curve_bake_owner(path, index));
        let settings =
            self.service.curve_bake_settings(&owner).ok().flatten().unwrap_or(PlacementSettings {
                curve_index: index,
                ..Default::default()
            });
        let mut state = CurveToolState {
            curve,
            settings,
            message: String::new(),
        };
        if self.service.curve_bake_is_stale(&owner, &state.curve.source).unwrap_or(true) {
            state.message =
                "Derived placements are stale. Bake placements along curve before preview/cook."
                    .into();
        }
        if let Some(preview) = &self.curve_preview {
            if preview.source == path && preview.settings.curve_index == index {
                state.settings = preview.settings.clone();
                state.message = if preview.revision != self.service.revision().get() {
                    "Scene changed: preview/clearance is stale. Preview or bake again.".into()
                } else {
                    format!(
                        "{} placements; {} clearance overlaps. {} {}",
                        preview.preview.transforms.len(),
                        preview.preview.collisions.len(),
                        preview
                            .preview
                            .collisions
                            .first()
                            .map(|(_, p)| p.as_str())
                            .unwrap_or("Loaded-scene clearance clear."),
                        preview.preview.diagnostics.join("; ")
                    )
                };
            }
        }
        Some(state)
    }

    pub fn preview_curve(
        &mut self,
        path: &str,
        settings: PlacementSettings,
    ) -> Result<(), SessionError> {
        let (source, mut preview) = self.service.preview_curve_placements(path, &settings)?;
        if self.service.curve_bake_is_stale(
            &self.service.curve_bake_owner_for(path, settings.curve_index)?,
            &source,
        )? {
            preview.diagnostics.push(
                "Derived output is stale: run Bake placements along curve before cook".into(),
            );
        }
        self.curve_preview = Some(CurvePreviewState {
            source: path.into(),
            settings,
            preview,
            revision: self.service.revision().get(),
        });
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn bake_curve(
        &mut self,
        path: &str,
        settings: &PlacementSettings,
    ) -> Result<(), SessionError> {
        let owner = self.service.curve_bake_owner_for(path, settings.curve_index)?;
        self.service.bake_curve_placements(path, &owner, settings)?;
        self.curve_preview = None;
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn edit_curve_points(
        &mut self,
        path: &str,
        points: Vec<[f32; 3]>,
        counts: Vec<i32>,
    ) -> Result<(), SessionError> {
        let prim = self
            .service
            .prim_at_path(path)
            .ok_or_else(|| idtx_usd::StageError::UnknownPath(path.into()))?;
        let projection = self.service.project_viewport_roots(&[prim])?;
        let mut record = projection
            .curves
            .into_iter()
            .find(|r| r.prim == prim)
            .ok_or_else(|| idtx_usd::StageError::UnknownPath(path.into()))?;
        record.points = points.clone();
        record.counts = counts.clone();
        record.validate().map_err(|e| idtx_usd::StageError::OpenUsd(e.to_string()))?;
        for (property, value) in [
            ("points", idtx_usd::PropertyValue::Point3Array(points)),
            ("curveVertexCounts", idtx_usd::PropertyValue::IntArray(counts)),
        ] {
            self.service.apply_structural_command(&idtx_usd::StructuralCommand::SetProperty {
                prim,
                property: property.into(),
                value,
            })?;
        }
        Ok(())
    }

    #[cfg(feature = "authoring")]
    pub fn edit_curve_route(
        &mut self,
        path: &str,
        tolerance: f64,
        eligible: bool,
        start_group: String,
        end_group: String,
    ) -> Result<(), SessionError> {
        if !tolerance.is_finite() || !(1e-9..=1.0).contains(&tolerance) {
            return Err(
                idtx_usd::StageError::OpenUsd("tolerance must be 1e-9..1 metre".into()).into()
            );
        }
        let prim = self
            .service
            .prim_at_path(path)
            .ok_or_else(|| idtx_usd::StageError::UnknownPath(path.into()))?;
        use idtx_usd::{PropertyValue as V, StructuralCommand};
        for (property, value) in [
            ("idtx:curve:toleranceMetres", V::F64(tolerance)),
            ("idtx:curve:eligible", V::Bool(eligible)),
            ("idtx:curve:startGroup", V::String(start_group)),
            ("idtx:curve:endGroup", V::String(end_group)),
        ] {
            self.service.apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: property.into(),
                value,
            })?;
        }
        Ok(())
    }

    #[cfg(feature = "editor-overlay")]
    pub(crate) fn curve_placement_lines(&self, out: &mut Vec<idtx_render::OverlayLine>) {
        let Some(preview) = &self.curve_preview else {
            return;
        };
        if !self.prim_matches_viewport_purpose(&preview.source) {
            return;
        }
        if preview.revision != self.service.revision().get() {
            return;
        }
        let scale = (1.0 / self.world_unit_scale().metres_per_unit()) as f32;
        let h = preview.settings.half_extents;
        for (i, matrix) in preview.preview.transforms.iter().enumerate() {
            let color = if preview.preview.collisions.iter().any(|(index, _)| *index == i) {
                [1.0, 0.2, 0.2, 1.0]
            } else {
                [0.25, 1.0, 0.5, 1.0]
            };
            let points =
                [[-h[0], 0.0, -h[2]], [h[0], 0.0, -h[2]], [h[0], 0.0, h[2]], [-h[0], 0.0, h[2]]]
                    .map(|p| {
                        (matrix.transform_point3(glam::DVec3::from_array(p)).as_vec3() * scale)
                            .to_array()
                    });
            for n in 0..4 {
                out.push(idtx_render::OverlayLine {
                    a: points[n],
                    b: points[(n + 1) % 4],
                    color,
                });
            }
        }
    }
}
