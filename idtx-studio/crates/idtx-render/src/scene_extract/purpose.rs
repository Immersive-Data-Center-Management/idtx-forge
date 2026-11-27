//! Viewport purpose routing. Guide geometry never reaches beauty/shadow draws.

use idtx_usd::RenderPurpose;

use super::RenderWorldSnapshot;

impl RenderWorldSnapshot {
    pub(super) fn separate_guide_geometry(&mut self) {
        if self.purpose_filter.is_none() {
            return;
        }
        let meshes = std::mem::take(&mut self.meshes);
        for mut mesh in meshes {
            if mesh.purpose == RenderPurpose::Guide {
                mesh.casts_shadow = false;
                self.guide_meshes.push(mesh);
            } else {
                self.meshes.push(mesh);
            }
        }
        let batches = std::mem::take(&mut self.instanced_batches);
        let mut metadata = std::mem::take(&mut self.point_instancer_batches);
        for (old_index, mut batch) in batches.into_iter().enumerate() {
            if batch.purpose == RenderPurpose::Guide {
                batch.casts_shadow = false;
                if let Some(meta) = metadata.remove(&old_index) {
                    self.guide_point_instancer_owners
                        .insert(meta.owner_path.clone());
                    self.guide_point_instancer_batches
                        .insert(self.guide_instanced_batches.len(), meta);
                }
                self.guide_instanced_batches.push(batch);
            } else {
                if let Some(meta) = metadata.remove(&old_index) {
                    self.point_instancer_batches
                        .insert(self.instanced_batches.len(), meta);
                }
                self.instanced_batches.push(batch);
            }
        }
    }

    /// Wireframes for all visible guides, using the collision triangle overlay
    /// builder. This is independent of the viewport Shaded/Wireframe control.
    #[cfg(feature = "editor-overlay")]
    pub fn guide_wireframe_lines(&self) -> Vec<crate::OverlayLine> {
        let mut lines = Vec::new();
        for instance in &self.guide_meshes {
            push_mesh_lines(&mut lines, &instance.mesh, instance.world_matrix);
        }
        for batch in &self.guide_instanced_batches {
            for &matrix in &batch.instance_transforms {
                push_mesh_lines(&mut lines, &batch.mesh, matrix);
            }
        }
        lines
    }
}

#[cfg(feature = "editor-overlay")]
fn push_mesh_lines(
    out: &mut Vec<crate::OverlayLine>,
    mesh: &crate::GpuMeshDesc,
    matrix: glam::Mat4,
) {
    let points = world_positions(mesh, matrix);
    let indices: Vec<i32> = mesh
        .indices
        .iter()
        .map(|&i| i32::try_from(i).unwrap_or(i32::MAX))
        .collect();
    crate::push_collision_trisoup(
        out,
        glam::Mat4::IDENTITY,
        &points,
        &indices,
        crate::COLLISION_COLOR,
    );
}

pub(super) fn world_positions(mesh: &crate::GpuMeshDesc, matrix: glam::Mat4) -> Vec<[f32; 3]> {
    // GPU-skinned draw positions have already composed stage/skel/bind space
    // into their joint matrices. Apply that same deformation for the CPU overlay.
    if let Some(skin) = &mesh.skin {
        mesh.positions
            .iter()
            .enumerate()
            .map(|(i, position)| {
                let Some(influence) = skin.influences.get(i) else {
                    return *position;
                };
                let mut point = glam::Vec3::ZERO;
                for (&joint, &weight) in influence.joints.iter().zip(&influence.weights) {
                    if let Some(transform) = skin.joint_matrices.get(joint as usize) {
                        point += glam::Mat4::from_cols_array(transform)
                            .transform_point3((*position).into())
                            * weight;
                    }
                }
                point.to_array()
            })
            .collect()
    } else {
        mesh.positions
            .iter()
            .map(|p| matrix.transform_point3((*p).into()).to_array())
            .collect()
    }
}
