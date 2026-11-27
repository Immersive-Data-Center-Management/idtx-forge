//! Presentation-independent snapshot, resource and overlay preparation.
use super::*;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SnapshotMaint {
    pub extracted: bool,
    #[cfg(feature = "metrics")]
    pub full_extract_ns: u64,
    #[cfg(feature = "metrics")]
    pub patch_transform_ns: u64,
    #[cfg(feature = "metrics")]
    pub patch_skeleton_ns: u64,
    #[cfg(feature = "metrics")]
    pub patch_widget_ns: u64,
    #[cfg(feature = "metrics")]
    pub patch_geometry_ns: u64,
}

impl StudioSession {
    pub(crate) fn prepare_presentation(&mut self, aspect: f32) -> SnapshotMaint {
        let mut maintenance = SnapshotMaint::default();
        // Scoped runtime-spawn splice: when a localized topology change queued one
        // and a warm snapshot exists, insert just the spawned subtree in place
        // (keeping `snapshot.serial` stable). If the splice bails (skinning /
        // instancing / a light in the subtree), force a full re-extract below.
        if let Some(upserts) = self.snapshot_topology_splice.take() {
            let upserts: Vec<_> = upserts.into_iter().collect();
            if !self.snapshot_dirty && self.last_snapshot.is_some() {
                let basis = self.projection_basis;
                let spliced = self
                    .last_snapshot
                    .as_mut()
                    .expect("snapshot exists")
                    .splice_subtree(self.hydrator.world_mut(), basis, &upserts, &[]);
                if spliced {
                    // New subtree may introduce late materials / textures.
                    let _ = self
                        .renderer
                        .prefetch_textures(self.last_snapshot.as_ref().expect("snapshot exists"));
                } else {
                    // Subtree needs the whole-stage passes — force a full extract.
                    self.snapshot_dirty = true;
                }
            }
        }
        let instancer_updates = std::mem::take(&mut self.snapshot_instancer_patches);
        if !instancer_updates.is_empty() && !self.snapshot_dirty {
            if let Some(snapshot) = &mut self.last_snapshot {
                if !snapshot.patch_point_instancers(self.hydrator.world(), &instancer_updates, self.projection_basis) {
                    self.snapshot_dirty = true;
                }
            }
        }
        if self.snapshot_dirty || self.last_snapshot.is_none() {
            #[cfg(feature = "metrics")]
            let started = std::time::Instant::now();
            let shadow = self.resolved_shadow_settings();
            let purpose_filter = self.viewport_purpose_filter();
            let snapshot = if self.viewport_purpose.is_some() {
                RenderWorldSnapshot::extract_with_purpose(self.hydrator.world_mut(), aspect, self.projection_basis, shadow, purpose_filter)
            } else {
                RenderWorldSnapshot::extract_with_shadow(self.hydrator.world_mut(), aspect, self.projection_basis, shadow)
            };
            // Late materials (including UDIM arrays) need the same resource
            // discovery as startup. Cached requests are skipped by the renderer.
            let _ = self.renderer.prefetch_textures(&snapshot);
            self.last_snapshot = Some(snapshot);
            self.snapshot_dirty = false;
            self.snapshot_transform_patches = None;
            self.snapshot_instancer_patches.clear();
            self.snapshot_skeleton_dirty = false;
            self.snapshot_widget_dirty = false;
            self.snapshot_geometry_dirty = false;
            self.snapshot_geometry_paths.clear();
            maintenance.extracted = true;
            #[cfg(feature = "metrics")]
            { maintenance.full_extract_ns = started.elapsed().as_nanos() as u64; }
        } else {
            if let Some(patches) = self.snapshot_transform_patches.take() {
                #[cfg(feature = "metrics")]
                let started = std::time::Instant::now();
                self.last_snapshot.as_mut().expect("snapshot exists").patch_world_matrices(&patches, self.projection_basis);
                #[cfg(feature = "metrics")]
                { maintenance.patch_transform_ns = started.elapsed().as_nanos() as u64; }
            }
            if self.snapshot_skeleton_dirty {
                #[cfg(feature = "metrics")]
                let started = std::time::Instant::now();
                self.last_snapshot.as_mut().expect("snapshot exists").patch_joint_matrices(self.hydrator.world_mut(), self.projection_basis);
                self.snapshot_skeleton_dirty = false;
                #[cfg(feature = "metrics")]
                { maintenance.patch_skeleton_ns = started.elapsed().as_nanos() as u64; }
            }
            if self.snapshot_widget_dirty {
                #[cfg(feature = "metrics")]
                let started = std::time::Instant::now();
                self.last_snapshot.as_mut().expect("snapshot exists").patch_widget_panels(self.hydrator.world_mut(), self.projection_basis);
                self.snapshot_widget_dirty = false;
                #[cfg(feature = "metrics")]
                { maintenance.patch_widget_ns = started.elapsed().as_nanos() as u64; }
            }
            if self.snapshot_geometry_dirty {
                #[cfg(feature = "metrics")]
                let started = std::time::Instant::now();
                let paths = std::mem::take(&mut self.snapshot_geometry_paths);
                self.last_snapshot.as_mut().expect("snapshot exists").patch_mesh_geometry(self.hydrator.world_mut(), self.projection_basis, &paths);
                self.snapshot_geometry_dirty = false;
                #[cfg(feature = "metrics")]
                { maintenance.patch_geometry_ns = started.elapsed().as_nanos() as u64; }
            }
        }

        self.refresh_instancer_bounds(&instancer_updates);
        let (boxes, tints, outlines) = partition_highlights(&self.behavior_highlights);
        self.presentation_overlay_lines.clone_from(&self.behavior_overlay_lines);
        for (path, color) in boxes {
            if let Some(corners) = selection_obb_for_path(&self.hydrator, &path, self.projection_basis) {
                push_obb_box_lines(&mut self.presentation_overlay_lines, &corners, color);
            }
        }
        let (_, widget_camera) = self.view_camera_pose();
        let widget_camera = if self.play_state == idtx_behavior::PlayState::Playing {
            self.hydrator.world().get_resource::<idtx_scene::widget_layout::WidgetInteractionCamera>()
                .map_or(widget_camera, |camera| camera.0)
        } else { widget_camera };
        let widget_highlights: std::collections::HashMap<_, _> = self.behaviors.widget_highlights().into_iter()
            .filter_map(|(entity, path)| self.hydrator.world().get::<idtx_scene::PrimNode>(bevy_ecs::entity::Entity::from_bits(entity.to_bits()))
                .map(|node| (node.path.clone(), path))).collect();
        let snapshot = self.last_snapshot.as_mut().expect("snapshot prepared");
        snapshot.apply_point_instance_opacities(&self.behavior_instance_opacities);
        snapshot.widget_camera = Some(widget_camera);
        for panel in &mut snapshot.widget_panels { panel.highlight = widget_highlights.get(&panel.prim_path).cloned(); }
        snapshot.highlight_tints = tints;
        snapshot.highlight_outlines = outlines;
        snapshot.hud_elements = collect_hud_elements(self.hydrator.world());
        maintenance
    }
}
