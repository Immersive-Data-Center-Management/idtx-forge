//! ECS → render-world extraction: the [`RenderWorldSnapshot::extract`] bulk query
//! and the transform / joint fast-path patches.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy_ecs::prelude::*;
use glam::{Mat3, Mat4};
use idtx_usd::{
    DomeTextureFormat, ExtendedGprimKind, GprimAxis, LightKind, PoleAxis, PurposeFilter, RenderPurpose,
};

#[cfg(feature = "metrics")]
use crate::convert::nanos_u64;
use crate::convert::u32_sat;
use crate::material::{RenderMaterialDesc, fallback_material};
use crate::mesh::{
    GpuMeshDesc, SkinData, tessellate_capsule, tessellate_cone, tessellate_cube,
    tessellate_cylinder, tessellate_plane, tessellate_sphere,
};
use crate::vertex::SkinVertex;
use idtx_scene::hydrator::mat4_from_usd_row_major;
use idtx_scene::{
    CameraComp, CubeComp, ExtendedGprimComp, InternalRefArc, LightComp, MaterialComp, MeshComp,
    NativeInstanceComp, PlaneComp, PointInstancerComp, PrimNode, SkeletonPoseComp, SkinBindingComp,
    SphereComp, VisibilityComp, WidgetPanelComp, WorldTransform,
};
use idtx_usd::{SkeletonPoseRecord, SkinBindingRecord, SkinningExecution};

use super::camera::RenderCameraDesc;
use super::instancing::promote_topology_instances_from_groups;
use super::light::{RenderLightDesc, RenderLightKind, caster_shadow_fit, light_point_intensity};
use super::mesh_instance::{RenderInstancedBatch, RenderMeshInstance};
use super::shadow::{RenderShadowSettings, ShadowAtlasAllocator};
use super::snapshot::{ExtractionStats, RenderWorldSnapshot};

pub(super) static SNAPSHOT_SERIAL: AtomicU64 = AtomicU64::new(1);

pub(super) fn purpose_matches(filter: Option<PurposeFilter>, purpose: RenderPurpose) -> bool {
    filter.map_or(purpose == RenderPurpose::Default, |f| f.matches(purpose))
}

pub(super) fn render_visible(filter: Option<PurposeFilter>, vis: Option<&VisibilityComp>) -> bool {
    vis.is_none_or(|v| v.visible && purpose_matches(filter, v.purpose))
}

pub(super) fn instance_purpose(owner: Option<&VisibilityComp>, geometry: Option<&VisibilityComp>) -> RenderPurpose {
    // Geometry purpose is already computed by USD. In particular, an authored
    // default on a prototype must not be replaced with the instancer's purpose.
    geometry.or(owner).map_or(RenderPurpose::Default, |v| v.purpose)
}

fn native_placements(
    world: &World,
    placements: &[(Mat4, String, Entity)],
    geometry: Entity,
    local: Mat4,
    basis: Mat4,
    filter: Option<PurposeFilter>,
) -> HashMap<(RenderPurpose, bool), (Vec<Mat4>, Vec<String>)> {
    let mut groups: HashMap<(RenderPurpose, bool), (Vec<Mat4>, Vec<String>)> = HashMap::new();
    let geometry_visibility = world.get::<VisibilityComp>(geometry);
    if geometry_visibility.is_some_and(|v| !v.visible) { return groups; }
    let geometry_prim = world.get::<PrimNode>(geometry).map(|n| n.prim);
    for (matrix, path, entity) in placements {
        let owner = world.get::<VisibilityComp>(*entity);
        let purpose = world.get::<NativeInstanceComp>(*entity)
            .and_then(|instance| instance.purposes.iter().find(|(prim, _)| Some(*prim) == geometry_prim))
            .map_or_else(|| instance_purpose(owner, geometry_visibility), |(_, purpose)| *purpose);
        if !purpose_matches(filter, purpose) { continue; }
        let casts_shadow = owner.is_none_or(|v| v.casts_shadow) && geometry_visibility.is_none_or(|v| v.casts_shadow);
        let (transforms, paths) = groups.entry((purpose, casts_shadow)).or_default();
        transforms.push(basis * *matrix * local);
        paths.push(path.clone());
    }
    groups
}

/// If `material` is a UDIM material drawn by a static (non-GPU-skinned) mesh,
/// restore raw USD-space V on `mesh` and union the tiles it touches into
/// `udim_tiles` under the material's path. No-op otherwise, so the non-UDIM
/// path is untouched.
///
/// GPU-skinned UDIM is not supported (it needs a skinned UDIM shader
/// variant); CPU-skinned meshes bake positions into the static vertex buffer
/// and so render correctly through the static UDIM variant, hence only
/// `skin.is_some()` (GPU classic-linear) is excluded.
fn record_udim_mesh(
    udim_tiles: &mut HashMap<String, BTreeSet<u32>>,
    mesh: &mut GpuMeshDesc,
    material: &RenderMaterialDesc,
) -> u64 {
    if !material.is_udim() || mesh.skin.is_some() {
        return 0;
    }
    mesh.unflip_texcoords_v();
    #[cfg(feature = "metrics")]
    let t0 = std::time::Instant::now();
    let tiles = crate::udim::touched_tiles(&mesh.texcoords, &mesh.indices);
    udim_tiles
        .entry(material.material_path.clone())
        .or_default()
        .extend(tiles);
    #[cfg(feature = "metrics")]
    {
        nanos_u64(t0.elapsed().as_nanos())
    }
    #[cfg(not(feature = "metrics"))]
    {
        0
    }
}

/// Resolve a material by its optional USD path, falling back to the shared
/// fallback material when the path is absent or not in `materials`.
fn lookup_material(
    materials: &HashMap<String, RenderMaterialDesc>,
    path: Option<&String>,
) -> RenderMaterialDesc {
    path.and_then(|p| materials.get(p).cloned())
        .unwrap_or_else(|| fallback_material(""))
}

/// When a gprim/mesh has no bound material, tint the fallback `material` with
/// its authored `displayColor[0]` and recompute the fingerprint. No-op when a
/// material is bound. Mirrors the per-prim-type binding fallback used across the
/// direct and prototype extraction passes.
fn apply_display_color_fallback(
    material: &mut RenderMaterialDesc,
    material_path: Option<&String>,
    display_colors: &[[f32; 3]],
) {
    if material_path.is_none() {
        if let Some(&c) = display_colors.first() {
            material.base_color = c;
        }
        material.fingerprint = material.compute_fingerprint();
    }
}

/// Record UDIM tiles for `mesh` / `material` and, under the `metrics` feature,
/// accumulate the discovery time into `stats`. Bundles the per-call
/// [`record_udim_mesh`] + metrics bookkeeping repeated at every mesh / subset
/// emission site.
///
/// Also the single point where the per-draw `displayColor` vertex tint is finalised
/// against the resolved `material`: this runs at every mesh / subset / remainder /
/// instanced emission site, each with the material actually bound to that draw, so
/// [`GpuMeshDesc::neutralize_display_color_tint`] makes the keep-vs-drop decision with
/// full per-subset material knowledge.
#[cfg_attr(not(feature = "metrics"), allow(unused_variables))]
fn record_udim_and_time(
    udim_tiles: &mut HashMap<String, BTreeSet<u32>>,
    stats: &mut ExtractionStats,
    mesh: &mut GpuMeshDesc,
    material: &RenderMaterialDesc,
) {
    mesh.neutralize_display_color_tint(material);
    let udim_ns = record_udim_mesh(udim_tiles, mesh, material);
    #[cfg(feature = "metrics")]
    {
        stats.extract_udim_discovery_ns += udim_ns;
    }
}

/// Y-axis-oriented tessellation for a `UsdGeom` cylinder / cone / capsule.
fn tessellate_ext_gprim(comp: &ExtendedGprimComp) -> GpuMeshDesc {
    match comp.kind {
        ExtendedGprimKind::Cylinder => tessellate_cylinder(comp.radius, comp.height * 0.5, 32),
        ExtendedGprimKind::Cone => tessellate_cone(comp.radius, comp.height * 0.5, 32),
        // USD capsule height = midsection only; pass directly.
        ExtendedGprimKind::Capsule => tessellate_capsule(comp.radius, comp.height, 32, 8),
    }
}

/// Pre-rotation mapping the Y-oriented gprim tessellation onto the USD `axis`
/// (tessellated +Y → +X via `Rz(-π/2)`, → +Z via `Rx(+π/2)`).
fn ext_gprim_axis_rot(axis: GprimAxis) -> Mat4 {
    match axis {
        GprimAxis::Y => Mat4::IDENTITY,
        GprimAxis::X => Mat4::from_rotation_z(-std::f32::consts::FRAC_PI_2),
        GprimAxis::Z => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
    }
}

/// Emit one or more [`RenderInstancedBatch`]es for a single prototype mesh of a
/// `PointInstancer` / native-instance prototype, splitting by `GeomSubset`
/// materials exactly as the direct mesh path does.
///
/// `key_base` is the shared prototype-key stem (`"{prefix}proto_{slot}_{index}"`,
/// also used as the `GpuMeshDesc` label): the single-material / non-subset batch
/// uses it verbatim, subset batches append `_<material>`, and the uncovered
/// remainder appends `__rem`. `instance_prim_paths` is empty for
/// `PointInstancer`-derived batches (no per-instance prim identity exists there)
/// and carries one path per instance for native instances.
fn emit_instanced_mesh_batches(
    snapshot: &mut RenderWorldSnapshot,
    mesh_comp: &MeshComp,
    gpu_mesh_base: GpuMeshDesc,
    key_base: &str,
    instance_transforms: &[Mat4],
    instance_prim_paths: &[String],
    purpose: RenderPurpose,
    casts_shadow: bool,
) {
    if mesh_comp.material_subsets.is_empty() {
        let mat = lookup_material(&snapshot.materials, mesh_comp.material_path.as_ref());
        let mut gpu_mesh_base = gpu_mesh_base;
        record_udim_and_time(
            &mut snapshot.udim_tiles,
            &mut snapshot.stats,
            &mut gpu_mesh_base,
            &mat,
        );
        snapshot.instanced_batches.push(RenderInstancedBatch {
            prototype_key: key_base.to_owned(),
            mesh: Arc::new(gpu_mesh_base),
            instance_transforms: instance_transforms.to_vec(),
            instance_prim_paths: instance_prim_paths.to_vec(),
            material: mat,
            purpose,
            casts_shadow,
        });
    } else {
        let mut covered: HashSet<u32> = HashSet::new();
        for subset in &mesh_comp.material_subsets {
            if subset.face_indices.is_empty() {
                continue;
            }
            covered.extend(subset.face_indices.iter().copied());
            let mut sub_mesh = GpuMeshDesc::subset_by_faces(
                &gpu_mesh_base,
                &mesh_comp.face_vertex_counts,
                &subset.face_indices,
            );
            if sub_mesh.indices.is_empty() {
                continue;
            }
            let sub_mat = lookup_material(&snapshot.materials, subset.material_path.as_ref());
            record_udim_and_time(
                &mut snapshot.udim_tiles,
                &mut snapshot.stats,
                &mut sub_mesh,
                &sub_mat,
            );
            let key = format!(
                "{key_base}_{}",
                subset.material_path.as_deref().unwrap_or("default")
            );
            snapshot.instanced_batches.push(RenderInstancedBatch {
                prototype_key: key,
                mesh: Arc::new(sub_mesh),
                instance_transforms: instance_transforms.to_vec(),
                instance_prim_paths: instance_prim_paths.to_vec(),
                material: sub_mat,
                purpose,
                casts_shadow,
            });
        }
        let remainder: Vec<u32> = (0..u32_sat(mesh_comp.face_vertex_counts.len()))
            .filter(|f| !covered.contains(f))
            .collect();
        if !remainder.is_empty() {
            let mut rem_mesh =
                GpuMeshDesc::subset_by_faces(&gpu_mesh_base, &mesh_comp.face_vertex_counts, &remainder);
            if !rem_mesh.indices.is_empty() {
                let mat = lookup_material(&snapshot.materials, mesh_comp.material_path.as_ref());
                record_udim_and_time(
                    &mut snapshot.udim_tiles,
                    &mut snapshot.stats,
                    &mut rem_mesh,
                    &mat,
                );
                snapshot.instanced_batches.push(RenderInstancedBatch {
                    prototype_key: format!("{key_base}__rem"),
                    mesh: Arc::new(rem_mesh),
                    instance_transforms: instance_transforms.to_vec(),
                    instance_prim_paths: instance_prim_paths.to_vec(),
                    material: mat,
                    purpose,
                    casts_shadow,
                });
            }
        }
    }
}

impl RenderWorldSnapshot {
    /// Extracts all renderable items from the ECS world.
    ///
    /// Only prims with `purpose == Default` (or no [`VisibilityComp`]) and
    /// `visible == true` are included. Prototype-only prims are excluded.
    ///
    /// `viewport_aspect` is the render target width/height ratio, used to
    /// derive the projection matrix.  Takes `&mut World` so query states can
    /// be registered internally.
    ///
    /// `stage_basis` is the root coordinate-system correction matrix computed
    /// from the stage's `upAxis` and `metersPerUnit` metadata (see
    /// `idtx_scene::coordinate::pseudo_root_matrix`).  Pass [`Mat4::IDENTITY`]
    /// when no correction is needed.  Every USD world-space matrix is
    /// pre-multiplied by this matrix before being submitted to the GPU.
    pub fn extract(world: &mut World, viewport_aspect: f32, stage_basis: Mat4) -> Self {
        Self::extract_with_shadow(
            world,
            viewport_aspect,
            stage_basis,
            RenderShadowSettings::OFF,
        )
    }

    /// Like [`Self::extract`] but records the resolved [`RenderShadowSettings`]
    /// onto the snapshot so the renderer knows which shadow tier to run. Callers
    /// that honor the `idtx:softShadows` stage flag + product capability use this;
    /// the plain [`Self::extract`] wrapper defaults to shadows off.
    pub fn extract_with_shadow(
        world: &mut World,
        viewport_aspect: f32,
        stage_basis: Mat4,
        shadow: RenderShadowSettings,
    ) -> Self {
        Self::extract_impl(world, viewport_aspect, stage_basis, shadow, None)
    }

    /// Extract a viewport with the selected purpose union. Default-purpose prims
    /// always participate; an empty filter includes every purpose. Guides are
    /// retained separately for the collision wireframe overlay.
    pub fn extract_with_purpose(
        world: &mut World,
        viewport_aspect: f32,
        stage_basis: Mat4,
        shadow: RenderShadowSettings,
        filter: PurposeFilter,
    ) -> Self {
        Self::extract_impl(world, viewport_aspect, stage_basis, shadow, Some(filter))
    }

    fn extract_impl(
        world: &mut World,
        viewport_aspect: f32,
        stage_basis: Mat4,
        shadow: RenderShadowSettings,
        purpose_filter: Option<PurposeFilter>,
    ) -> Self {
        let mut snapshot = Self::default();
        snapshot.purpose_filter = purpose_filter;
        snapshot.serial = SNAPSHOT_SERIAL.fetch_add(1, Ordering::Relaxed);
        // A fresh extract invalidates both structural and instance caches; keep
        // them in lock-step at extract time. Only the warm XR hand injection bumps
        // `instance_serial` on its own afterwards.
        snapshot.instance_serial = snapshot.serial;
        snapshot.point_instancer_serial = snapshot.serial;
        snapshot.shadow = shadow;

        // ── Materials ─────────────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        let t_materials = std::time::Instant::now();
        let mut mat_query = world.query::<(&PrimNode, &MaterialComp)>();
        for (node, mat) in mat_query.iter(world) {
            snapshot.materials.insert(
                node.path.clone(),
                RenderMaterialDesc::from_material_record(&mat.record),
            );
        }

        #[cfg(feature = "metrics")]
        {
            snapshot.stats.extract_materials_ns = nanos_u64(t_materials.elapsed().as_nanos());
        }
        #[cfg(feature = "metrics")]
        let t_meshes = std::time::Instant::now();

        // ── Skeleton pose map ─────────────────────────────────────────────
        // Build a lookup from skeleton PrimId → pose record so the mesh loop
        // can find the correct joint matrices without a second world query.
        let mut pose_map: HashMap<idtx_scene::PrimId, SkeletonPoseRecord> = HashMap::new();
        {
            let mut q = world.query::<&SkeletonPoseComp>();
            for pose_comp in q.iter(world) {
                pose_map.insert(pose_comp.record.skeleton, pose_comp.record.clone());
            }
        }

        // ── Polygon meshes ────────────────────────────────────────────────
        let mut mesh_query = world.query::<(
            &PrimNode,
            &MeshComp,
            &WorldTransform,
            Option<&VisibilityComp>,
            Option<&SkinBindingComp>,
        )>();
        for (node, mesh, xform, vis, skin_binding) in mesh_query.iter(world) {
            if mesh.prototype_only {
                snapshot.stats.skipped_prototype_only += 1;
                continue;
            }
            if let Some(vis) = vis {
                if !vis.visible {
                    snapshot.stats.skipped_invisible += 1;
                    continue;
                }
                if !purpose_matches(purpose_filter, vis.purpose) {
                    snapshot.stats.skipped_invisible += 1;
                    continue;
                }
            }
            let mut gpu_mesh = GpuMeshDesc::from_mesh_comp(&node.path, mesh);
            gpu_mesh.material_path.clone_from(&mesh.material_path);

            // ── Skinning dispatch ──────────────────────────────────────────
            // Compute the effective world matrix and, for GPU skinning, fill
            // in gpu_mesh.skin.  All USD bridge matrices are row-major and must
            // go through mat4_from_usd_row_major before any glam arithmetic.
            let world_matrix = if let Some(skin_comp) = skin_binding {
                let skin_rec = &skin_comp.record;
                match skin_rec.execution {
                    SkinningExecution::GpuClassicLinear => {
                        let pose = pose_map.get(&skin_rec.skeleton);
                        if let Some(pose) = pose.filter(|p| p.skinning_transforms.len() <= 256) {
                            let skel_world =
                                mat4_from_usd_row_major(&pose.skeleton_world_transform);
                            let geom_bind = mat4_from_usd_row_major(&skin_rec.geom_bind_transform);

                            let joint_matrices: Vec<[f32; 16]> = pose
                                .skinning_transforms
                                .iter()
                                .map(|st| {
                                    (stage_basis
                                        * skel_world
                                        * mat4_from_usd_row_major(st)
                                        * geom_bind)
                                        .to_cols_array()
                                })
                                .collect();

                            // Expand per-point influences to per-flat-vertex order.
                            let influences: Vec<SkinVertex> = mesh
                                .face_vertex_indices
                                .iter()
                                .map(|&pt| {
                                    // USD face-vertex indices are non-negative; a stray
                                    // negative degrades to slot 0 rather than wrapping huge.
                                    let vi = skin_rec
                                        .influences
                                        .get(usize::try_from(pt).unwrap_or(0))
                                        .copied();
                                    match vi {
                                        Some(v) => SkinVertex {
                                            joints: [
                                                u32::from(v.joints[0]),
                                                u32::from(v.joints[1]),
                                                u32::from(v.joints[2]),
                                                u32::from(v.joints[3]),
                                                u32::from(v.joints[4]),
                                                u32::from(v.joints[5]),
                                                u32::from(v.joints[6]),
                                                u32::from(v.joints[7]),
                                            ],
                                            weights: v.weights,
                                        },
                                        None => SkinVertex {
                                            joints: [0; 8],
                                            weights: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                                        },
                                    }
                                })
                                .collect();

                            gpu_mesh.skin = Some(SkinData {
                                influences,
                                joint_matrices,
                            });
                            // Joint matrices already encode full world space; mesh uniform is ignored.
                            glam::Mat4::IDENTITY
                        } else {
                            // Too many joints (>256) or no pose found — rest pose fallback.
                            if pose_map
                                .get(&skin_rec.skeleton)
                                .is_some_and(|p| p.skinning_transforms.len() > 256)
                            {
                                eprintln!(
                                    "idtx-render: skinned mesh {} has >256 joints — falling back to rest pose",
                                    node.path
                                );
                            }
                            stage_basis * xform.matrix
                        }
                    }
                    SkinningExecution::CpuOpenUsd => {
                        let pose = pose_map.get(&skin_rec.skeleton);
                        if let Some(pose) = pose {
                            // Deform positions AND normals in the SAME face order
                            // `triangulate_mesh` / `compute_smooth_normals_with_creases`
                            // use — skipping faces with fewer than three corners — so
                            // positions, normals and the triangulated indices from
                            // `from_mesh_comp` stay aligned. Emitting a vertex for every
                            // raw face-corner (as before) desynced the index→vertex
                            // mapping on meshes containing a degenerate face, which the
                            // triangulated indices skip.
                            //
                            // mesh.normals and face_vertex_indices share face-corner
                            // order (len == face_vertex_indices.len), so authored normals
                            // are indexed by the original corner cursor.
                            let authored_normals_present = !mesh.normals.is_empty()
                                && mesh.normals.len() == mesh.face_vertex_indices.len();
                            // Per-joint linear parts (skinningXform · geomBind, the same
                            // matrices that deform the points — see the skel test in
                            // idtx-usd), reused for every skinned corner below.
                            let joint_normal_mats: Vec<glam::Mat3> = if authored_normals_present
                            {
                                let geom_bind =
                                    mat4_from_usd_row_major(&skin_rec.geom_bind_transform);
                                pose.skinning_transforms
                                    .iter()
                                    .map(|st| {
                                        glam::Mat3::from_mat4(
                                            mat4_from_usd_row_major(st) * geom_bind,
                                        )
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };

                            let mut flat_positions: Vec<[f32; 3]> =
                                Vec::with_capacity(mesh.face_vertex_indices.len());
                            let mut authored_skinned_normals: Vec<[f32; 3]> = Vec::new();
                            if authored_normals_present {
                                authored_skinned_normals.reserve(mesh.face_vertex_indices.len());
                            }
                            let mut cursor = 0_usize;
                            for &count in &mesh.face_vertex_counts {
                                let n = usize::try_from(count).unwrap_or(0);
                                if n < 3 {
                                    cursor += n; // skip, matching triangulate_mesh
                                    continue;
                                }
                                for k in 0..n {
                                    let corner = cursor + k;
                                    let pt = usize::try_from(
                                        mesh.face_vertex_indices.get(corner).copied().unwrap_or(0),
                                    )
                                    .unwrap_or(0);
                                    flat_positions.push(
                                        skin_rec
                                            .cpu_skinned_points
                                            .get(pt)
                                            .copied()
                                            .unwrap_or([0.0; 3]),
                                    );
                                    if !authored_normals_present {
                                        continue;
                                    }
                                    let n_authored = glam::Vec3::from(mesh.normals[corner]);
                                    let deformed = match skin_rec.influences.get(pt) {
                                        Some(inf) => {
                                            let mut blended = glam::Mat3::ZERO;
                                            // A zero-weight influence contributes a zero
                                            // matrix, so no need to branch on the weight.
                                            for (&w, joint) in inf.weights.iter().zip(inf.joints)
                                            {
                                                if let Some(nm) =
                                                    joint_normal_mats.get(usize::from(joint))
                                                {
                                                    blended += *nm * w;
                                                }
                                            }
                                            // Inverse-transpose keeps the normal
                                            // perpendicular to the skinned surface under
                                            // non-uniform scale / shear (a plain linear
                                            // transform does not); a singular blend falls
                                            // back to the authored normal below.
                                            if blended.determinant().abs() > f32::EPSILON {
                                                (blended.inverse().transpose() * n_authored)
                                                    .normalize_or_zero()
                                            } else {
                                                glam::Vec3::ZERO
                                            }
                                        }
                                        None => n_authored.normalize_or_zero(),
                                    };
                                    let d = if deformed == glam::Vec3::ZERO {
                                        n_authored.normalize_or_zero()
                                    } else {
                                        deformed
                                    };
                                    authored_skinned_normals.push([d.x, d.y, d.z]);
                                }
                                cursor += n;
                            }

                            let skinned_normals: Vec<[f32; 3]> = if authored_normals_present {
                                authored_skinned_normals
                            } else {
                                // No authored normals: synthesize smooth normals from the
                                // deformed points, honouring the same authored
                                // crease/corner sharp features the static path uses so
                                // hard edges survive animation. compute_smooth_normals_
                                // with_creases skips the same <3-corner faces, so its
                                // output aligns 1:1 with flat_positions above.
                                let hard_edges = crate::mesh::hard_edges_from_creases(
                                    &mesh.crease_indices,
                                    &mesh.crease_lengths,
                                    &mesh.crease_sharpnesses,
                                    crate::mesh::DEFAULT_CREASE_SHARPNESS_CUTOFF,
                                );
                                let corner_points = crate::mesh::corner_point_set(
                                    &mesh.corner_indices,
                                    &mesh.corner_sharpnesses,
                                    crate::mesh::DEFAULT_CREASE_SHARPNESS_CUTOFF,
                                );
                                crate::mesh::compute_smooth_normals_with_creases(
                                    &skin_rec.cpu_skinned_points,
                                    &mesh.face_vertex_counts,
                                    &mesh.face_vertex_indices,
                                    crate::mesh::DEFAULT_CREASE_ANGLE,
                                    mesh.left_handed,
                                    &hard_edges,
                                    &corner_points,
                                )
                            };
                            gpu_mesh.positions = flat_positions;
                            gpu_mesh.normals = skinned_normals;
                            // The CPU-deformed positions change every animated frame, but the
                            // vertex/topology counts do not, so `MeshComp::geometry_version`
                            // (and hence the serial copied by `from_mesh_comp`) stays constant.
                            // Bump the serial here so the renderer's per-instance mesh cache
                            // detects the change and re-uploads the deformed vertex buffer;
                            // otherwise the GPU keeps the first frame's positions and the mesh
                            // appears frozen while still paying the CPU deformation cost.
                            gpu_mesh.serial = crate::mesh::next_mesh_serial();
                            // cpu_skinned_points are skeleton-local; apply skeleton world transform.
                            stage_basis * mat4_from_usd_row_major(&pose.skeleton_world_transform)
                        } else {
                            stage_basis * xform.matrix
                        }
                    }
                    SkinningExecution::RestPose => stage_basis * xform.matrix,
                }
            } else {
                stage_basis * xform.matrix
            };
            let mut material = lookup_material(&snapshot.materials, mesh.material_path.as_ref());
            // When there's no material binding, use displayColor[0] as base tint.
            apply_display_color_fallback(
                &mut material,
                mesh.material_path.as_ref(),
                &mesh.display_colors,
            );
            let purpose = vis.map_or(RenderPurpose::Default, |v| v.purpose);
            let casts_shadow = vis.map_or(true, |v| v.casts_shadow);

            // Split into per-subset draw calls when GeomSubsets are present.
            if !mesh.material_subsets.is_empty() {
                let mut covered: HashSet<u32> = HashSet::new();
                for subset in &mesh.material_subsets {
                    if subset.face_indices.is_empty() {
                        continue;
                    }
                    covered.extend(subset.face_indices.iter().copied());
                    let mut sub_mesh = GpuMeshDesc::subset_by_faces(
                        &gpu_mesh,
                        &mesh.face_vertex_counts,
                        &subset.face_indices,
                    );
                    if sub_mesh.indices.is_empty() {
                        continue;
                    }
                    let sub_mat = lookup_material(&snapshot.materials, subset.material_path.as_ref());
                    record_udim_and_time(
                        &mut snapshot.udim_tiles,
                        &mut snapshot.stats,
                        &mut sub_mesh,
                        &sub_mat,
                    );
                    let label = format!(
                        "{}/subset_{}",
                        node.path,
                        subset.material_path.as_deref().unwrap_or("default"),
                    );
                    snapshot.meshes.push(RenderMeshInstance {
                        prim_path: label,
                        mesh: Arc::new(sub_mesh),
                        world_matrix,
                        material: sub_mat,
                        purpose,
                        casts_shadow,
                    });
                    snapshot.stats.mesh_count += 1;
                }
                // Faces not covered by any subset get the mesh's own material.
                let remainder: Vec<u32> = (0..u32_sat(mesh.face_vertex_counts.len()))
                    .filter(|f| !covered.contains(f))
                    .collect();
                if !remainder.is_empty() {
                    let mut rem_mesh = GpuMeshDesc::subset_by_faces(
                        &gpu_mesh,
                        &mesh.face_vertex_counts,
                        &remainder,
                    );
                    if !rem_mesh.indices.is_empty() {
                        record_udim_and_time(
                            &mut snapshot.udim_tiles,
                            &mut snapshot.stats,
                            &mut rem_mesh,
                            &material,
                        );
                        snapshot.meshes.push(RenderMeshInstance {
                            prim_path: format!("{}/remainder", node.path),
                            mesh: Arc::new(rem_mesh),
                            world_matrix,
                            material,
                            purpose,
                            casts_shadow,
                        });
                        snapshot.stats.mesh_count += 1;
                    }
                }
                continue;
            }

            record_udim_and_time(
                &mut snapshot.udim_tiles,
                &mut snapshot.stats,
                &mut gpu_mesh,
                &material,
            );
            snapshot.meshes.push(RenderMeshInstance {
                prim_path: node.path.clone(),
                mesh: Arc::new(gpu_mesh),
                world_matrix,
                material,
                purpose,
                casts_shadow,
            });
            snapshot.stats.mesh_count += 1;
        }

        // ── Cubes ─────────────────────────────────────────────────────────
        let mut cube_query = world.query::<(
            &PrimNode,
            &CubeComp,
            &WorldTransform,
            Option<&VisibilityComp>,
        )>();
        for (node, cube, xform, vis) in cube_query.iter(world) {
            if cube.prototype_only {
                snapshot.stats.skipped_prototype_only += 1;
                continue;
            }
            if !render_visible(purpose_filter, vis) {
                snapshot.stats.skipped_invisible += 1;
                continue;
            }
            let mut gpu_mesh = tessellate_cube(cube.size * 0.5);
            gpu_mesh.prim_path.clone_from(&node.path);
            gpu_mesh.double_sided = cube.double_sided;
            gpu_mesh.material_path.clone_from(&cube.material_path);
            let mut material = lookup_material(&snapshot.materials, cube.material_path.as_ref());
            apply_display_color_fallback(
                &mut material,
                cube.material_path.as_ref(),
                &cube.display_colors,
            );
            let purpose = vis.map_or(RenderPurpose::Default, |v| v.purpose);
            let casts_shadow = vis.map_or(true, |v| v.casts_shadow);
            snapshot.meshes.push(RenderMeshInstance {
                prim_path: node.path.clone(),
                mesh: Arc::new(gpu_mesh),
                world_matrix: stage_basis * xform.matrix,
                material,
                purpose,
                casts_shadow,
            });
            snapshot.stats.mesh_count += 1;
        }

        // ── Widget panels (2D widgets in 3D space) ─────────────────────────
        // World-space quads; the renderer rasterizes each panel to a texture
        // (dirty-gated by content_hash) and draws it depth-tested in the beauty
        // pass. Populated here (not session-injected like hud_elements) because
        // a panel is world geometry carrying a WorldTransform.
        let mut widget_query = world.query::<(
            &PrimNode,
            &WidgetPanelComp,
            &WorldTransform,
            Option<&VisibilityComp>,
        )>();
        for (node, panel, xform, vis) in widget_query.iter(world) {
            if !render_visible(purpose_filter, vis) {
                snapshot.stats.skipped_invisible += 1;
                continue;
            }
            if purpose_filter.is_some() && vis.is_some_and(|v| v.purpose == RenderPurpose::Guide) {
                let mut mesh = tessellate_plane(panel.size[0], panel.size[1], GprimAxis::Z);
                mesh.prim_path.clone_from(&node.path);
                snapshot.guide_meshes.push(RenderMeshInstance {
                    prim_path: node.path.clone(), mesh: Arc::new(mesh),
                    world_matrix: stage_basis * xform.matrix, material: fallback_material(""),
                    purpose: RenderPurpose::Guide, casts_shadow: false,
                });
                continue;
            }
            snapshot.widget_panels.push(super::WidgetPanel {
                highlight: None,
                surface: panel.surface.clone(),
                prim_path: node.path.clone(),
                world_matrix: stage_basis * xform.matrix,
                size: panel.size,
                resolution: panel.resolution,
                billboard: panel.billboard,
                double_sided: panel.double_sided,
                opacity: panel.opacity,
                space: panel.space,
                elements: panel.elements.clone(),
                content_hash: panel.content_hash,
            });
        }

        // ── Spheres ───────────────────────────────────────────────────────
        let mut sphere_query = world.query::<(
            &PrimNode,
            &SphereComp,
            &WorldTransform,
            Option<&VisibilityComp>,
        )>();
        for (node, sphere, xform, vis) in sphere_query.iter(world) {
            if sphere.prototype_only {
                snapshot.stats.skipped_prototype_only += 1;
                continue;
            }
            if !render_visible(purpose_filter, vis) {
                snapshot.stats.skipped_invisible += 1;
                continue;
            }
            let mut gpu_mesh = tessellate_sphere(sphere.radius, 24, 36);
            gpu_mesh.prim_path.clone_from(&node.path);
            gpu_mesh.double_sided = sphere.double_sided;
            gpu_mesh.material_path.clone_from(&sphere.material_path);
            let mut material = lookup_material(&snapshot.materials, sphere.material_path.as_ref());
            apply_display_color_fallback(
                &mut material,
                sphere.material_path.as_ref(),
                &sphere.display_colors,
            );
            let purpose = vis.map_or(RenderPurpose::Default, |v| v.purpose);
            let casts_shadow = vis.map_or(true, |v| v.casts_shadow);
            snapshot.meshes.push(RenderMeshInstance {
                prim_path: node.path.clone(),
                mesh: Arc::new(gpu_mesh),
                world_matrix: stage_basis * xform.matrix,
                material,
                purpose,
                casts_shadow,
            });
            snapshot.stats.mesh_count += 1;
        }

        // ── Extended gprims (Cylinder / Cone / Capsule) ───────────────────
        // All tessellations are Y-axis oriented by default.  The USD `axis`
        // attribute is baked into the world matrix as a pre-rotation so the
        // normal matrix stays correct for any world transform.
        //
        // Rotation mapping  (tessellated Y-axis → USD axis):
        //   Y → identity
        //   X → Rz(-π/2)  maps +Y → +X
        //   Z → Rx(+π/2)  maps +Y → +Z
        let mut ext_query = world.query::<(
            &PrimNode,
            &ExtendedGprimComp,
            &WorldTransform,
            Option<&VisibilityComp>,
        )>();
        for (node, comp, xform, vis) in ext_query.iter(world) {
            if comp.prototype_only {
                snapshot.stats.skipped_prototype_only += 1;
                continue;
            }
            if !render_visible(purpose_filter, vis) {
                snapshot.stats.skipped_invisible += 1;
                continue;
            }

            let axis_rot = ext_gprim_axis_rot(comp.axis);
            let world_matrix = stage_basis * xform.matrix * axis_rot;

            let mut gpu_mesh = tessellate_ext_gprim(comp);
            gpu_mesh.prim_path.clone_from(&node.path);
            gpu_mesh.double_sided = comp.double_sided;
            gpu_mesh.material_path.clone_from(&comp.material_path);

            let mut material = lookup_material(&snapshot.materials, comp.material_path.as_ref());
            apply_display_color_fallback(
                &mut material,
                comp.material_path.as_ref(),
                &comp.display_colors,
            );
            let purpose = vis.map_or(RenderPurpose::Default, |v| v.purpose);
            let casts_shadow = vis.map_or(true, |v| v.casts_shadow);
            snapshot.meshes.push(RenderMeshInstance {
                prim_path: node.path.clone(),
                mesh: Arc::new(gpu_mesh),
                world_matrix,
                material,
                purpose,
                casts_shadow,
            });
            snapshot.stats.mesh_count += 1;
        }

        // ── Planes ────────────────────────────────────────────────────────
        // UsdGeomPlane: finite double-sided rectangle. The axis / width / length
        // mapping is baked into the quad geometry by `tessellate_plane`, so the
        // world matrix needs no axis pre-rotation (unlike the extended gprims).
        let mut plane_query = world.query::<(
            &PrimNode,
            &PlaneComp,
            &WorldTransform,
            Option<&VisibilityComp>,
        )>();
        for (node, plane, xform, vis) in plane_query.iter(world) {
            if plane.prototype_only {
                snapshot.stats.skipped_prototype_only += 1;
                continue;
            }
            if !render_visible(purpose_filter, vis) {
                snapshot.stats.skipped_invisible += 1;
                continue;
            }
            let mut gpu_mesh = tessellate_plane(plane.width, plane.length, plane.axis);
            gpu_mesh.prim_path.clone_from(&node.path);
            gpu_mesh.double_sided = plane.double_sided;
            gpu_mesh.material_path.clone_from(&plane.material_path);
            let mut material = lookup_material(&snapshot.materials, plane.material_path.as_ref());
            apply_display_color_fallback(
                &mut material,
                plane.material_path.as_ref(),
                &plane.display_colors,
            );
            let purpose = vis.map_or(RenderPurpose::Default, |v| v.purpose);
            let casts_shadow = vis.map_or(true, |v| v.casts_shadow);
            snapshot.meshes.push(RenderMeshInstance {
                prim_path: node.path.clone(),
                mesh: Arc::new(gpu_mesh),
                world_matrix: stage_basis * xform.matrix,
                material,
                purpose,
                casts_shadow,
            });
            snapshot.stats.mesh_count += 1;
        }

        // ── PointInstancers ───────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        {
            snapshot.stats.extract_meshes_ns = nanos_u64(t_meshes.elapsed().as_nanos());
        }
        #[cfg(feature = "metrics")]
        let t_instancers = std::time::Instant::now();
        // Build PrimId → entity and path → entity lookups.
        let prim_to_entity: HashMap<idtx_scene::PrimId, Entity> = {
            let mut q = world.query::<(&PrimNode, Entity)>();
            q.iter(world).map(|(n, e)| (n.prim, e)).collect()
        };

        // Collect (entity, path) for ALL mesh entities including prototype-only ones.
        struct ProtoMeshInfo {
            entity: Entity,
            path: String,
        }
        let all_mesh_entities: Vec<ProtoMeshInfo> = {
            let mut q = world.query::<(&PrimNode, Entity, &MeshComp)>();
            q.iter(world)
                .map(|(n, e, _)| ProtoMeshInfo {
                    entity: e,
                    path: n.path.clone(),
                })
                .collect()
        };

        // Collect instancer raw data (entity included; no world lookups inside the query).
        struct InstancerRaw {
            entity: Entity,
            prototypes: Vec<idtx_scene::PrimId>,
            proto_indices: Vec<u32>,
            transforms: Vec<[f32; 16]>,
            ids: Vec<i64>,
            mask: Vec<u8>,
        }
        let instancer_raws: Vec<InstancerRaw> = {
            let mut q = world.query::<(Entity, &PointInstancerComp)>();
            q.iter(world)
                .map(|(e, c)| InstancerRaw {
                    entity: e,
                    prototypes: c.prototypes.clone(),
                    proto_indices: c.proto_indices.clone(),
                    transforms: c.transforms.clone(),
                    ids: c.ids.clone(),
                    mask: c.mask.clone(),
                })
                .collect()
        };

        // Resolve prototype paths from prim_to_entity (no active query borrow).
        struct InstancerData {
            entity: Entity,
            prototypes: Vec<(idtx_scene::PrimId, String)>,
            proto_indices: Vec<u32>,
            transforms: Vec<[f32; 16]>,
            ids: Vec<i64>,
            mask: Vec<u8>,
        }
        let instancers: Vec<InstancerData> = instancer_raws
            .into_iter()
            .map(|raw| {
                let prototypes = raw
                    .prototypes
                    .iter()
                    .map(|&p| {
                        let path = prim_to_entity
                            .get(&p)
                            .and_then(|&e| world.get::<PrimNode>(e))
                            .map(|n| n.path.clone())
                            .unwrap_or_default();
                        (p, path)
                    })
                    .collect();
                InstancerData {
                    entity: raw.entity,
                    prototypes,
                    proto_indices: raw.proto_indices,
                    transforms: raw.transforms,
                    ids: raw.ids,
                    mask: raw.mask,
                }
            })
            .collect();

        for inst in &instancers {
            // ComputeInstanceTransformsAtTime returns transforms in instancer-LOCAL space.
            // Compose with the instancer's own world transform to get world-space placements.
            let instancer_world = world
                .get::<WorldTransform>(inst.entity)
                .map_or(Mat4::IDENTITY, |x| x.matrix);
            let visibility = world.get::<VisibilityComp>(inst.entity);
            let visible = visibility.is_none_or(|v| v.visible);
            let owner_path = world.get::<PrimNode>(inst.entity).map_or_else(String::new, |n| n.path.clone());

            for (proto_idx, (proto_prim, proto_path)) in inst.prototypes.iter().enumerate() {
                let Some(&proto_entity) = prim_to_entity.get(proto_prim) else {
                    continue;
                };

                // Collect all world-space instance transforms for this prototype index.
                let proto_world = world
                    .get::<WorldTransform>(proto_entity)
                    .map_or(Mat4::IDENTITY, |x| x.matrix);
                let proto_world_inv = proto_world.inverse();

                // Walk all mesh descendants of this prototype root once.
                let prefix = format!("{proto_path}/");
                let descendants: Vec<(Entity, Mat4)> = all_mesh_entities
                    .iter()
                    .filter(|m| m.path == *proto_path || m.path.starts_with(&prefix))
                    .filter_map(|m| {
                        let mesh_world = world.get::<WorldTransform>(m.entity)?.matrix;
                        Some((m.entity, mesh_world))
                    })
                    .collect();

                // Build per-prototype-mesh batches: one `RenderInstancedBatch` per
                // (proto_mesh, material/subset) combination, accumulating all instance transforms.
                for (mesh_entity, mesh_world) in &descendants {
                    let Some(mesh_comp) = world.get::<MeshComp>(*mesh_entity) else {
                        continue;
                    };
                    let geometry_visibility = world.get::<VisibilityComp>(*mesh_entity);
                    let purpose = instance_purpose(visibility, geometry_visibility);
                    let visible = visible && geometry_visibility.is_none_or(|v| v.visible) && purpose_matches(purpose_filter, purpose);
                    let mesh_world = *mesh_world;

                    // proto_local encodes the mesh's position within prototype space.
                    let proto_local = proto_world_inv * mesh_world;

                    // Collect instance transforms: for each authored instance whose proto_index
                    // matches, compose its world placement with proto_local.
                    let instance_transforms: Vec<Mat4> = inst
                        .proto_indices
                        .iter()
                        .zip(inst.transforms.iter())
                        .enumerate()
                        .filter(|&(i, (&idx, _))| visible && idx as usize == proto_idx && (inst.mask.is_empty() || inst.mask.get(i) == Some(&1)))
                        .map(|(_, (_, &xform_raw))| {
                            let instance_local = mat4_from_usd_row_major(&xform_raw);
                            stage_basis * instancer_world * instance_local * proto_local
                        })
                        .collect();

                    let key_base =
                        format!("instproto_{}_{}_{}", inst.entity.to_bits(), proto_prim.slot(), mesh_entity.index());
                    let gpu_mesh_base = GpuMeshDesc::from_mesh_comp(key_base.clone(), mesh_comp);
                    let first_batch = snapshot.instanced_batches.len();
                    emit_instanced_mesh_batches(
                        &mut snapshot,
                        mesh_comp,
                        gpu_mesh_base,
                        &key_base,
                        &instance_transforms,
                        &[],
                        purpose,
                        visibility.is_none_or(|v| v.casts_shadow) && geometry_visibility.is_none_or(|v| v.casts_shadow),
                    );
                    let ids: Vec<_> = inst.proto_indices.iter().enumerate()
                        .filter(|&(i, &p)| visible && p as usize == proto_idx && (inst.mask.is_empty() || inst.mask.get(i) == Some(&1)))
                        .map(|(i,_)| inst.ids[i]).collect();
                    for batch in first_batch..snapshot.instanced_batches.len() {
                        snapshot.point_instancer_batches.insert(batch, super::PointInstancerBatch::new(
                            inst.entity, owner_path.clone(), proto_entity, *mesh_entity,
                            proto_idx as u32, ids.clone(), snapshot.serial, Mat4::IDENTITY));
                    }
                }
            }
        }

        // ── Native scenegraph instances (`instanceable = true`) ───────────
        // Instances collapsed to leaf prims by the bridge; each carries its own
        // world transform. Group by shared prototype, walk the prototype's mesh
        // descendants once, and emit one hardware-instanced batch per
        // (proto-mesh, material/subset) — mirroring the PointInstancer pass above.
        let native_entities: Vec<(Entity, idtx_scene::PrimId)> = {
            let mut q = world.query::<(Entity, &NativeInstanceComp)>();
            q.iter(world).map(|(e, c)| (e, c.prototype)).collect()
        };
        // (world transform, instance prim path) grouped by prototype PrimId.
        let mut native_by_prototype: HashMap<idtx_scene::PrimId, Vec<(Mat4, String, Entity)>> =
            HashMap::new();
        for (entity, prototype) in native_entities {
            if world.get::<VisibilityComp>(entity).is_some_and(|v| !v.visible) { continue; }
            let instance_world = world
                .get::<WorldTransform>(entity)
                .map_or(Mat4::IDENTITY, |x| x.matrix);
            let path = world
                .get::<PrimNode>(entity)
                .map(|n| n.path.clone())
                .unwrap_or_default();
            native_by_prototype
                .entry(prototype)
                .or_default()
                .push((instance_world, path, entity));
        }
        snapshot.stats.native_instance_count +=
            native_by_prototype.values().map(Vec::len).sum::<usize>();

        // Prototype gprim geometry (Cube / Sphere / Cylinder / Cone / Capsule).
        // Unlike Meshes (queried via `all_mesh_entities` / `MeshComp`), gprims carry
        // an implicit surface, so tessellate each prototype-only gprim once here and
        // hardware-instance it below — this is what gives the plan's "Meshes + gprims"
        // scope its gprim half. `geom_world` bakes the axis pre-rotation (identity for
        // Cube/Sphere) so `proto_local` is computed consistently with the direct path.
        struct ProtoGprimInfo {
            entity: Entity,
            axis_transform: Mat4,
            path: String,
            base_mesh: GpuMeshDesc,
            material: RenderMaterialDesc,
            geom_world: Mat4,
        }
        let mut proto_gprims: Vec<ProtoGprimInfo> = Vec::new();
        {
            let mut q = world.query::<(Entity, &PrimNode, &CubeComp, &WorldTransform)>();
            for (entity, node, cube, xform) in q.iter(world) {
                if !cube.prototype_only {
                    continue;
                }
                let mut m = tessellate_cube(cube.size * 0.5);
                m.prim_path.clone_from(&node.path);
                m.double_sided = cube.double_sided;
                m.material_path.clone_from(&cube.material_path);
                let mut material = lookup_material(&snapshot.materials, cube.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    cube.material_path.as_ref(),
                    &cube.display_colors,
                );
                proto_gprims.push(ProtoGprimInfo {
                    entity,
                    axis_transform: Mat4::IDENTITY,
                    path: node.path.clone(),
                    base_mesh: m,
                    material,
                    geom_world: xform.matrix,
                });
            }
        }
        {
            let mut q = world.query::<(Entity, &PrimNode, &SphereComp, &WorldTransform)>();
            for (entity, node, sphere, xform) in q.iter(world) {
                if !sphere.prototype_only {
                    continue;
                }
                let mut m = tessellate_sphere(sphere.radius, 24, 36);
                m.prim_path.clone_from(&node.path);
                m.double_sided = sphere.double_sided;
                m.material_path.clone_from(&sphere.material_path);
                let mut material =
                    lookup_material(&snapshot.materials, sphere.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    sphere.material_path.as_ref(),
                    &sphere.display_colors,
                );
                proto_gprims.push(ProtoGprimInfo {
                    entity,
                    axis_transform: Mat4::IDENTITY,
                    path: node.path.clone(),
                    base_mesh: m,
                    material,
                    geom_world: xform.matrix,
                });
            }
        }
        {
            let mut q = world.query::<(Entity, &PrimNode, &ExtendedGprimComp, &WorldTransform)>();
            for (entity, node, comp, xform) in q.iter(world) {
                if !comp.prototype_only {
                    continue;
                }
                let axis_rot = ext_gprim_axis_rot(comp.axis);
                let mut m = tessellate_ext_gprim(comp);
                m.prim_path.clone_from(&node.path);
                m.double_sided = comp.double_sided;
                m.material_path.clone_from(&comp.material_path);
                let mut material = lookup_material(&snapshot.materials, comp.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    comp.material_path.as_ref(),
                    &comp.display_colors,
                );
                proto_gprims.push(ProtoGprimInfo {
                    entity,
                    axis_transform: axis_rot,
                    path: node.path.clone(),
                    base_mesh: m,
                    material,
                    geom_world: xform.matrix * axis_rot,
                });
            }
        }
        {
            let mut q = world.query::<(Entity, &PrimNode, &PlaneComp, &WorldTransform)>();
            for (entity, node, plane, xform) in q.iter(world) {
                if !plane.prototype_only {
                    continue;
                }
                let mut m = tessellate_plane(plane.width, plane.length, plane.axis);
                m.prim_path.clone_from(&node.path);
                m.double_sided = plane.double_sided;
                m.material_path.clone_from(&plane.material_path);
                let mut material = lookup_material(&snapshot.materials, plane.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    plane.material_path.as_ref(),
                    &plane.display_colors,
                );
                proto_gprims.push(ProtoGprimInfo {
                    entity,
                    axis_transform: Mat4::IDENTITY,
                    path: node.path.clone(),
                    base_mesh: m,
                    material,
                    geom_world: xform.matrix,
                });
            }
        }

        // Native PointInstancers can reference parametric geometry as well as
        // polygon meshes. Reuse the same tessellated prototype/material records.
        for inst in &instancers {
            let owner_path = world.get::<PrimNode>(inst.entity).map_or_else(String::new, |n| n.path.clone());
            let owner_world = world.get::<WorldTransform>(inst.entity).map_or(Mat4::IDENTITY, |x| x.matrix);
            let visibility = world.get::<VisibilityComp>(inst.entity);
            let visible = visibility.is_none_or(|v| v.visible);
            for (proto_index, (prim, path)) in inst.prototypes.iter().enumerate() {
                let Some(&prototype) = prim_to_entity.get(prim) else { continue; };
                let proto_inv = world.get::<WorldTransform>(prototype).map_or(Mat4::IDENTITY, |x| x.matrix).inverse();
                let prefix = format!("{path}/");
                for gp in proto_gprims.iter().filter(|gp| gp.path == *path || gp.path.starts_with(&prefix)) {
                    let geometry_visibility = world.get::<VisibilityComp>(gp.entity);
                    let purpose = instance_purpose(visibility, geometry_visibility);
                    let visible = visible && geometry_visibility.is_none_or(|v| v.visible) && purpose_matches(purpose_filter, purpose);
                    let local = proto_inv * gp.geom_world;
                    let slots: Vec<_> = inst.proto_indices.iter().enumerate()
                        .filter(|&(i,&p)| visible && p as usize == proto_index && (inst.mask.is_empty() || inst.mask.get(i) == Some(&1)))
                        .map(|(i,_)| i).collect();
                    let transforms = slots.iter().map(|&i| stage_basis * owner_world * mat4_from_usd_row_major(&inst.transforms[i]) * local).collect();
                    let ids = slots.iter().map(|&i| inst.ids[i]).collect();
                    let mut mesh = gp.base_mesh.clone();
                    record_udim_and_time(&mut snapshot.udim_tiles, &mut snapshot.stats, &mut mesh, &gp.material);
                    let index = snapshot.instanced_batches.len();
                    snapshot.instanced_batches.push(RenderInstancedBatch {
                        prototype_key: format!("instproto_gprim_{}_{}_{}", inst.entity.to_bits(), prim.slot(), gp.path),
                        mesh: Arc::new(mesh), instance_transforms: transforms, instance_prim_paths: Vec::new(),
                        material: gp.material.clone(), purpose,
                        casts_shadow: visibility.is_none_or(|v| v.casts_shadow) && geometry_visibility.is_none_or(|v| v.casts_shadow),
                    });
                    snapshot.point_instancer_batches.insert(index, super::PointInstancerBatch::new(
                        inst.entity, owner_path.clone(), prototype, gp.entity, proto_index as u32,
                        ids, snapshot.serial, gp.axis_transform));
                }
            }
        }

        for (proto_prim, placements) in &native_by_prototype {
            let Some(&proto_entity) = prim_to_entity.get(proto_prim) else {
                continue;
            };
            let Some(proto_path) = world.get::<PrimNode>(proto_entity).map(|n| n.path.clone())
            else {
                continue;
            };

            // Prototype-local placement of each mesh, relative to the prototype root.
            let proto_world = world
                .get::<WorldTransform>(proto_entity)
                .map_or(Mat4::IDENTITY, |x| x.matrix);
            let proto_world_inv = proto_world.inverse();

            let prefix = format!("{proto_path}/");
            let descendants: Vec<(Entity, Mat4)> = all_mesh_entities
                .iter()
                .filter(|m| m.path == proto_path || m.path.starts_with(&prefix))
                .filter_map(|m| {
                    let mesh_world = world.get::<WorldTransform>(m.entity)?.matrix;
                    Some((m.entity, mesh_world))
                })
                .collect();

            for (mesh_entity, mesh_world) in &descendants {
                let Some(mesh_comp) = world.get::<MeshComp>(*mesh_entity) else {
                    continue;
                };
                let proto_local = proto_world_inv * *mesh_world;

                for ((purpose, casts_shadow), (instance_transforms, instance_prim_paths)) in
                    native_placements(world, placements, *mesh_entity, proto_local, stage_basis, purpose_filter)
                {
                    let key_base = format!("nativeproto_{}_{}_{purpose:?}_{casts_shadow}", proto_prim.slot(), mesh_entity.index());
                    let gpu_mesh_base = GpuMeshDesc::from_mesh_comp(key_base.clone(), mesh_comp);
                    emit_instanced_mesh_batches(
                        &mut snapshot, mesh_comp, gpu_mesh_base, &key_base,
                        &instance_transforms, &instance_prim_paths, purpose, casts_shadow,
                    );
                }
            }

            // Prototype gprims (Cube/Sphere/Cylinder/Cone/Capsule). Single material,
            // no GeomSubsets — one batch each, sharing the instance placements above.
            for gp in proto_gprims
                .iter()
                .filter(|g| g.path == proto_path || g.path.starts_with(&prefix))
            {
                let proto_local = proto_world_inv * gp.geom_world;
                for ((purpose, casts_shadow), (instance_transforms, instance_prim_paths)) in
                    native_placements(world, placements, gp.entity, proto_local, stage_basis, purpose_filter)
                {
                    let mut gp_mesh = gp.base_mesh.clone();
                    record_udim_and_time(
                        &mut snapshot.udim_tiles,
                        &mut snapshot.stats,
                        &mut gp_mesh,
                        &gp.material,
                    );
                    snapshot.instanced_batches.push(RenderInstancedBatch {
                        prototype_key: format!("nativeproto_gprim_{}_{}_{purpose:?}_{casts_shadow}", proto_prim.slot(), gp.path),
                        mesh: Arc::new(gp_mesh),
                        instance_transforms,
                        instance_prim_paths,
                        material: gp.material.clone(),
                        purpose,
                        casts_shadow,
                    });
                }
            }
        }

        for meta in snapshot.point_instancer_batches.values_mut() {
            meta.dynamic = world.get::<PointInstancerComp>(meta.owner).is_some_and(|c| c.runtime_dynamic);
        }
        // Populate reusable unmasked placements and authored per-instance coverage
        // once. The per-frame opacity overlay never reads USD or recomputes TRS.
        let owners: Vec<_> = snapshot.point_instancer_batches.values()
            .map(|m| (m.owner_path.clone(), false)).collect();
        snapshot.patch_point_instancers(world, &owners, stage_basis);

        // ── Update instanced batch stats ──────────────────────────────────
        snapshot.stats.instanced_batch_count += snapshot.instanced_batches.len();
        snapshot.stats.instanced_draw_count = snapshot.instanced_batches.iter().map(|b| b.instance_transforms.len()).sum();

        // ── Topology-aware instance promotion ────────────────────────────
        #[cfg(feature = "metrics")]
        {
            snapshot.stats.extract_instancers_ns = nanos_u64(t_instancers.elapsed().as_nanos());
        }
        #[cfg(feature = "metrics")]
        let t_topo = std::time::Instant::now();
        Self::promote_topology_instances(&mut snapshot, world);
        #[cfg(feature = "metrics")]
        {
            snapshot.stats.extract_topo_promote_ns = nanos_u64(t_topo.elapsed().as_nanos());
        }

        // ── Pseudo-instance deduplication ─────────────────────────────────
        //Self::promote_pseudo_instances(&mut snapshot);

        // ── Lights ────────────────────────────────────────────────────────
        snapshot.separate_guide_geometry();

        // Uniform stage→render unit scale (== metersPerUnit for the default
        // WorldUnitScale). Light dimensions are authored in stage units and must
        // be scaled into render units so the area/attenuation terms match the
        // unit-scaled world positions in `world_matrix`.
        let unit_scale = stage_basis.x_axis.length();
        // Shadow fit: compute the world-space scene AABB once, only when shadows
        // are active, so each Distant caster's light-space matrix frames the whole
        // scene. `None` when there is no geometry or shadows are off. (Fits to
        // `compute_scene_aabb`, which covers per-mesh geometry and
        // instanced-batch caster bounds.)
        let shadow_aabb = if snapshot.shadow.enabled() {
            crate::camera_fit::compute_scene_aabb(&snapshot, true)
        } else {
            None
        };
        // Retain the fit bounds on the snapshot so the transform-fast-path
        // (`patch_world_matrices`) can refit a moved light's shadow frustum
        // without re-walking every vertex.
        snapshot.shadow_aabb = shadow_aabb;
        // Cache the AABB input signature so `patch_world_matrices` can skip the
        // recompute on a frame whose caster transforms did not change the bounds.
        if snapshot.shadow.enabled() {
            snapshot.shadow_aabb_sig = crate::camera_fit::scene_aabb_signature(&snapshot);
        }
        let mut light_query = world.query::<(&PrimNode, &LightComp, Option<&WorldTransform>, Option<&VisibilityComp>)>();
        for (node, light_comp, xform, vis) in light_query.iter(world) {
            if purpose_filter.is_some() && !render_visible(purpose_filter, vis) { continue; }
            let world_matrix = stage_basis * xform.map_or(Mat4::IDENTITY, |x| x.matrix);
            let rec = &light_comp.record;
            let kind = match rec.kind {
                LightKind::Distant { angle } => RenderLightKind::Distant {
                    angle_radians: angle.to_radians(),
                },
                LightKind::Sphere { radius } => RenderLightKind::Sphere {
                    radius: radius * unit_scale,
                },
                LightKind::Rect { width, height } => RenderLightKind::Rect {
                    width: width * unit_scale,
                    height: height * unit_scale,
                },
                LightKind::Disk { radius } => RenderLightKind::Disk {
                    radius: radius * unit_scale,
                },
                LightKind::Cylinder { radius, length } => RenderLightKind::Cylinder {
                    radius: radius * unit_scale,
                    length: length * unit_scale,
                },
                LightKind::Dome => RenderLightKind::Dome,
                LightKind::DomeProcedural => RenderLightKind::DomeProcedural,
            };
            // The C++ bridge already folds the blackbody `colorTemperature`
            // multiplier into `rec.color` (via `UsdLuxBlackbodyTemperatureAsRgb`),
            // so the render color is used as-is; `rec.temperature` is retained
            // upstream purely for inspector/ambient reporting.
            let color = rec.color;
            // Effective point-source intensity: intensity × 2^exposure (EV stops
            // from USD LightAPI), with the emitter-area / `normalize` term folded
            // in so the shader can use a plain inverse-square falloff.
            let base = rec.intensity * (2.0_f32).powf(rec.exposure);
            let power = light_point_intensity(&kind, base, rec.normalize);
            // Shadow caster fit: only Distant + shadow-enabled lights cast in v1.
            // Build the light-space ortho VP from the scene AABB; the allocator
            // (below) decides which casters win a scarce atlas slot. Shared with
            // the fast-path refit in `patch_world_matrices` so both agree.
            let (shadow_view_proj, shadow_near, shadow_far, light_size) =
                caster_shadow_fit(&kind, rec.shadow_enabled, world_matrix, shadow_aabb);
            // Dome-specific fields: texture, projection, orientation, ambient.
            let is_dome = matches!(
                kind,
                RenderLightKind::Dome | RenderLightKind::DomeProcedural
            );
            let (dome_texture_path, dome_texture_format, env_rotation, ambient) = if is_dome {
                let pole = match rec.dome_pole_axis {
                    PoleAxis::Z => crate::env_ibl::EnvPoleAxis::Z,
                    PoleAxis::Scene | PoleAxis::Y => crate::env_ibl::EnvPoleAxis::Y,
                };
                (
                    rec.dome_texture_path.clone(),
                    rec.dome_texture_format,
                    crate::env_ibl::env_rotation(world_matrix, pole),
                    [color[0] * power, color[1] * power, color[2] * power],
                )
            } else {
                (None, DomeTextureFormat::default(), Mat3::IDENTITY, [0.0; 3])
            };
            // Effective point-source intensity: intensity × 2^exposure (EV stops
            // from USD LightAPI), with the emitter-area / `normalize` term folded
            // in so the shader can use a plain inverse-square falloff.
            let base = rec.intensity * (2.0_f32).powf(rec.exposure);
            let power = light_point_intensity(&kind, base, rec.normalize);
            snapshot.lights.push(RenderLightDesc {
                prim_path: node.path.clone(),
                kind,
                world_matrix,
                color,
                power,
                diffuse_contribution: rec.diffuse,
                specular_contribution: rec.specular,
                shadow_enabled: rec.shadow_enabled,
                cone_angle: rec.has_shaping.then_some(rec.cone_angle),
                cone_softness: rec.cone_softness,
                shadow_view_proj,
                shadow_near,
                shadow_far,
                light_size,
                shadow_slot: None,
                dome_texture_path,
                dome_texture_format,
                env_rotation,
                ambient,
            });
            snapshot.stats.light_count += 1;
        }

        // ── Shadow atlas allocation ───────────────────────────────────────
        // Rank the Distant casters by importance and assign the ≤ SHADOW_BUDGET
        // atlas layers (mutates each winner's `shadow_slot`). No-op when shadows
        // are off (no light carries a `shadow_view_proj`).
        if snapshot.shadow.enabled() {
            snapshot.shadow_slots = ShadowAtlasAllocator::allocate(&mut snapshot.lights);
        }

        // ── Camera ────────────────────────────────────────────────────────
        let mut cam_query = world.query::<(&PrimNode, &CameraComp, &WorldTransform)>();
        if let Some((node, cam_comp, xform)) = cam_query.iter(world).next() {
            snapshot.camera = Some(RenderCameraDesc::from_scene_camera(
                node.path.clone(),
                cam_comp,
                xform.matrix,
                stage_basis,
                viewport_aspect,
            ));
        }

        snapshot
    }

    /// Promotes pseudo-instances detected via USD internal reference arcs to hardware-instanced
    /// batches.
    ///
    /// Looks for ECS entities with [`InternalRefArc`] (prims that have `prepend references =
    /// </SomePrim>` pointing to a prim in the same layer). Groups with ≥2 referencing prims are
    /// candidate pseudo-instances. For each candidate group, sub-meshes at the same relative path
    /// within the prototype subtree are promoted to a batch if their geometry and material
    /// fingerprints all match (validating the "only transforms differ" constraint).
    fn promote_topology_instances(snapshot: &mut RenderWorldSnapshot, world: &mut World) {
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut q = world.query::<(&PrimNode, &InternalRefArc)>();
            for (node, arc) in q.iter(world) {
                groups
                    .entry(arc.target_path.clone())
                    .or_default()
                    .push(node.path.clone());
            }
        }
        promote_topology_instances_from_groups(snapshot, &groups);
    }

    /*
    /// Promotes groups of identical singleton meshes to hardware-instanced batches.
    ///
    /// Detects DCC-exported pseudo-instances: prims with the same geometry fingerprint
    /// (vertex/index count + sampled position data) and the same material fingerprint.
    /// Groups of ≥2 are converted to a single `RenderInstancedBatch`; the originals are
    /// removed from `self.meshes`.  Skinned meshes and non-default-purpose prims are skipped.
    pub fn promote_pseudo_instances(snapshot: &mut RenderWorldSnapshot) {
        let mut groups: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
        for (i, inst) in snapshot.meshes.iter().enumerate() {
            if inst.purpose != RenderPurpose::Default { continue; }
            if inst.mesh.skin.is_some() { continue; }
            let key = (geometry_fingerprint(&inst.mesh), inst.material.fingerprint);
            groups.entry(key).or_default().push(i);
        }

        let mut to_remove: HashSet<usize> = HashSet::new();
        for (_, indices) in &groups {
            if indices.len() < 2 { continue; }
            let proto_idx = indices[0];
            let proto_path = snapshot.meshes[proto_idx].prim_path.clone();
            let prototype_key = format!("pseudo_{proto_path}");
            let mesh = snapshot.meshes[proto_idx].mesh.clone();
            let material = snapshot.meshes[proto_idx].material.clone();
            let purpose = snapshot.meshes[proto_idx].purpose;
            let instance_transforms: Vec<Mat4> = indices
                .iter()
                .map(|&i| snapshot.meshes[i].world_matrix)
                .collect();
            let instance_prim_paths: Vec<String> = indices
                .iter()
                .map(|&i| snapshot.meshes[i].prim_path.clone())
                .collect();
            snapshot.instanced_batches.push(RenderInstancedBatch {
                prototype_key,
                mesh,
                instance_transforms,
                instance_prim_paths,
                material,
                purpose,
                casts_shadow,
            });
            to_remove.extend(indices.iter().copied());
        }

        if !to_remove.is_empty() {
            snapshot.stats.pseudo_instance_promoted += to_remove.len();
            snapshot.stats.instanced_batch_count = snapshot.instanced_batches.len();
            let mut remove_sorted: Vec<usize> = to_remove.into_iter().collect();
            remove_sorted.sort_unstable_by(|a, b| b.cmp(a));
            for i in remove_sorted {
                snapshot.meshes.swap_remove(i);
            }
            snapshot.stats.instanced_draw_count = snapshot.instanced_batches
                .iter()
                .map(|b| b.instance_transforms.len())
                .sum();
        }
    }*/

    /// Update only the world matrices in this snapshot without re-extracting
    /// geometry or materials.
    ///
    /// Used by the animation transform-only fast path: when USD prims with
    /// exclusively xformOp tracks advance to a new time code, only their world
    /// matrices change.  Calling this instead of a full `extract()` saves ~2 ms
    /// per animated frame for typical scenes.
    ///
    /// `patches` is a slice of `(prim_path, usd_row_major_matrix)` pairs
    /// returned by `SceneHydrator::apply_transform_samples()`.  `stage_basis`
    /// must be the same coordinate-system correction matrix used during the
    /// last full `extract()` call.
    ///
    /// Lights are patched in place as well: a moved light (or a moved ancestor
    /// of one) changes only its world matrix, and the lights uniform buffer is
    /// rebuilt from `self.lights` every frame regardless of [`serial`](Self::serial),
    /// so repositioning a light needs no whole-stage re-extract — just this
    /// matrix rewrite. `RenderLightDesc::world_matrix` is derived exactly as the
    /// mesh matrices are (`stage_basis * world`), so the same patch value applies.
    ///
    /// Widget panels (`IdtxWidgetPanel`) are world-space quads whose placement
    /// is driven by `WidgetPanel::world_matrix`. They are Xformable (Gprim-derived),
    /// so a transform-only change must patch them here just like meshes and lights.
    ///
    /// Entries in `patches` that do not match any mesh, instanced-batch, light,
    /// or widget panel prim path are silently ignored (cameras, invisible prims, etc.).
    pub fn patch_world_matrices(&mut self, patches: &[(String, [f32; 16])], stage_basis: Mat4) {
        if patches.is_empty() {
            return;
        }
        let map: HashMap<&str, Mat4> = patches
            .iter()
            .map(|(p, m)| (p.as_str(), stage_basis * mat4_from_usd_row_major(m)))
            .collect();
        // Track whether any *caster* (mesh or instanced geometry) actually moved
        // (diff-guarded: an identical matrix is not a move), and — for the shadow
        // fast-path — whether any mover's transformed bounds left the cached scene
        // AABB. A mover that stayed inside cannot enlarge the AABB, so the refit
        // can reuse the cached bounds without the O(meshes) recompute.
        let mut casters_moved = false;
        let mut casters_left_bounds = false;
        let cached_shadow_aabb = self.shadow_aabb;
        let mut note_mover = |local: Option<([f32; 3], [f32; 3])>, mat: Mat4| {
            casters_moved = true;
            if !casters_left_bounds {
                match (local, cached_shadow_aabb) {
                    (Some(l), Some(bounds)) => {
                        if !crate::camera_fit::transformed_aabb_within(l, mat, bounds) {
                            casters_left_bounds = true;
                        }
                    }
                    // No local bounds or no cached AABB → cannot prove containment;
                    // force a recompute to stay conservative/correct.
                    _ => casters_left_bounds = true,
                }
            }
        };
        for inst in self.meshes.iter_mut().chain(self.guide_meshes.iter_mut()) {
            // Key on the OWNING USD prim carried inside the mesh, not the
            // synthetic outer `inst.prim_path`: a mesh split by GeomSubset
            // materials emits instances with `/subset_<mat>` / `/remainder`
            // suffixes that never equal the authored prim path in `patches`,
            // so matching on `inst.prim_path` silently skips every subset piece
            // (e.g. the material-split XR controller) and freezes it in place.
            // `GpuMeshDesc::prim_path` is preserved verbatim by
            // `subset_by_faces`, so it always equals the authored prim path.
            if let Some(&mat) = map.get(inst.mesh.prim_path.as_str()) {
                if inst.world_matrix != mat {
                    // Only shadow casters can perturb the shadow AABB; a moving
                    // non-casting gizmo (`idtx:castsShadows = 0`) updates its pose
                    // for the beauty pass but must not trigger a shadow refit.
                    if inst.casts_shadow {
                        note_mover(inst.mesh.local_aabb, mat);
                    }
                    inst.world_matrix = mat;
                }
            }
        }
        let mut instance_values_changed = false;
        for batch in self.instanced_batches.iter_mut().chain(self.guide_instanced_batches.iter_mut()) {
            let proto_local = batch.mesh.local_aabb;
            let batch_casts = batch.casts_shadow;
            for (i, path) in batch.instance_prim_paths.iter().enumerate() {
                if let Some(&mat) = map.get(path.as_str()) {
                    if batch.instance_transforms[i] != mat {
                        if batch_casts {
                            note_mover(proto_local, mat);
                        }
                        batch.instance_transforms[i] = mat;
                        instance_values_changed = true;
                    }
                }
            }
        }
        // A mask pulse can now share a warm frame with native/pseudo-instance
        // motion. That motion must independently invalidate its instance bytes;
        // the structural serial (and all prototype geometry) stays stable.
        if instance_values_changed {
            self.instance_serial = self.instance_serial.wrapping_add(1);
        }
        drop(note_mover);
        // A moved light (or moved ancestor of one) only changes placement — the
        // per-frame lights uniform reads `world_matrix` straight from here, so an
        // in-place rewrite moves the illumination without a re-extract or serial
        // bump. Emission/kind/color depend on light attributes, not the transform,
        // so they are left untouched.
        for light in &mut self.lights {
            if let Some(&mat) = map.get(light.prim_path.as_str()) {
                light.world_matrix = mat;
            }
        }
        // Widget panels are Xformable (Gprim-derived) and their world_matrix
        // drives quad placement in the widget pass. A transform-only change
        // must update the panel here for the same reason as meshes and lights.
        for panel in &mut self.widget_panels {
            if let Some(&mat) = map.get(panel.prim_path.as_str()) {
                panel.world_matrix = mat;
            }
        }

        // ── Shadow refit ──────────────────────────────────────────────────
        // A Distant light's shadow is rendered from `shadow_slots[*].view_proj`
        // (fit to the light direction + scene AABB), NOT from `world_matrix`. The
        // renderer's shadow-atlas cache is keyed on `light_matrix_hash(view_proj)`
        // (see `renderer::casters_signature` / the per-slot cache), so unless we
        // refresh the slot's `view_proj` here a fast-path light move leaves it
        // stale, the cache key never changes, the depth pre-pass is skipped, and
        // the shadow freezes at the light's pre-move orientation — the very bug a
        // moved caster does NOT hit, because its world matrix perturbs
        // `casters_signature`. Recompute each caster's frustum from its patched
        // world matrix (and, if a caster moved, a refreshed AABB) and re-run the
        // allocator. `serial` is deliberately left untouched: the changed
        // `view_proj` invalidates only the affected atlas layer, while the warm
        // mesh/instance GPU caches survive.
        if self.shadow.enabled() {
            #[cfg(feature = "metrics")]
            let t_shadow_refit = std::time::Instant::now();
            // Recompute the scene AABB the shadow frusta fit to only when a caster
            // actually moved (diff-guarded) AND its transformed bounds left the
            // cached AABB (bounds fast-path) AND the O(meshes) input signature
            // changed (signature-gate). The common case — the head-tracked player
            // rig gizmos moving every frame but staying inside the room — keeps the
            // cached bounds and skips the recompute entirely. `compute_scene_aabb`
            // is itself O(meshes) now, so even when it does run it is cheap.
            if casters_moved && casters_left_bounds {
                let sig = crate::camera_fit::scene_aabb_signature(self);
                if sig != self.shadow_aabb_sig {
                    self.shadow_aabb = crate::camera_fit::compute_scene_aabb(self, true);
                    self.shadow_aabb_sig = sig;
                }
            }
            let aabb = self.shadow_aabb;
            for light in &mut self.lights {
                // `kind` / `shadow_enabled` cannot change on a transform-only
                // tick, so this preserves exactly which lights are casters.
                let (vp, near, far, size) =
                    caster_shadow_fit(&light.kind, light.shadow_enabled, light.world_matrix, aabb);
                light.shadow_view_proj = vp;
                light.shadow_near = near;
                light.shadow_far = far;
                light.light_size = size;
                // Cleared before re-allocation so an overflow caster that lost its
                // slot does not keep a stale index.
                light.shadow_slot = None;
            }
            self.shadow_slots = ShadowAtlasAllocator::allocate(&mut self.lights);
            #[cfg(feature = "metrics")]
            {
                self.stats.patch_shadow_refit_ns = nanos_u64(t_shadow_refit.elapsed().as_nanos());
            }
        }
    }

    /// Recomputes the GPU classic-linear joint palette for skinned meshes in
    /// place, from the skeleton poses currently in the ECS `world` (refreshed by
    /// [`SceneHydrator::apply_viewport_projection`] during a value-only
    /// reprojection).
    ///
    /// This is the skinned-mesh counterpart to [`patch_world_matrices`]: it lets
    /// a per-frame skeleton-pose change — e.g. the rigidly re-posed *skinned* XR
    /// controller prefab — refresh only the joint matrices (which the renderer
    /// re-uploads every frame regardless of the mesh cache) without a whole-stage
    /// re-extract and, crucially, WITHOUT bumping [`serial`](Self::serial), so the
    /// cross-frame GPU buffer cache and the stereo eye0/eye1 instanced/merge
    /// buffers survive rather than being rebuilt every frame.
    ///
    /// Mirrors the joint-matrix computation in [`extract`](Self::extract). Only
    /// meshes extracted as `GpuClassicLinear` with an active palette
    /// (`skin = Some(..)`) are patched; meshes that fell back to the rest pose at
    /// extract time (>256 joints or no pose → `skin = None`) are left untouched,
    /// preserving that fallback. `stage_basis` must be the same coordinate-system
    /// correction matrix used by the last full `extract`.
    pub fn patch_joint_matrices(&mut self, world: &mut World, stage_basis: Mat4) {
        // Skeleton pose lookup (skeleton PrimId → posed joint set). The ECS
        // `SkeletonPoseComp` values were refreshed by the value-only reprojection
        // that preceded this patch, so this is the single source of truth.
        let mut pose_map: HashMap<idtx_scene::PrimId, SkeletonPoseRecord> = HashMap::new();
        {
            let mut q = world.query::<&SkeletonPoseComp>();
            for pose_comp in q.iter(world) {
                pose_map.insert(pose_comp.record.skeleton, pose_comp.record.clone());
            }
        }
        if pose_map.is_empty() {
            return;
        }
        // Skinned-mesh prim path → its skin binding (skeleton + geom-bind).
        let mut skin_map: HashMap<String, SkinBindingRecord> = HashMap::new();
        {
            let mut q = world.query::<(&PrimNode, &SkinBindingComp)>();
            for (node, skin) in q.iter(world) {
                skin_map.insert(node.path.clone(), skin.record.clone());
            }
        }
        for inst in self.meshes.iter_mut().chain(self.guide_meshes.iter_mut()) {
            // Match on the owning USD prim carried inside the mesh, not the
            // synthetic outer `inst.prim_path`. `skin_map` is keyed by the ECS
            // `PrimNode.path`; a mesh split by GeomSubset materials carries
            // `/subset_<mat>` / `/remainder` suffixes in `inst.prim_path` that
            // never match that key, so matching on `inst.prim_path` skips every
            // subset piece and leaves the joint palette frozen at extract-time
            // values (the material-split skinned XR controller). `subset_by_faces`
            // preserves `GpuMeshDesc::prim_path`, so it always equals `node.path`.
            let Some(skin_rec) = skin_map.get(&inst.mesh.prim_path) else {
                continue;
            };
            if skin_rec.execution != SkinningExecution::GpuClassicLinear {
                continue;
            }
            let Some(pose) = pose_map
                .get(&skin_rec.skeleton)
                .filter(|p| p.skinning_transforms.len() <= 256)
            else {
                continue;
            };
            let skel_world = mat4_from_usd_row_major(&pose.skeleton_world_transform);
            let geom_bind = mat4_from_usd_row_major(&skin_rec.geom_bind_transform);
            let joint_matrices: Vec<[f32; 16]> = pose
                .skinning_transforms
                .iter()
                .map(|st| {
                    (stage_basis * skel_world * mat4_from_usd_row_major(st) * geom_bind)
                        .to_cols_array()
                })
                .collect();
            // Mutate the palette in place; `serial` is deliberately preserved so
            // the mesh cache treats the geometry as unchanged.
            if let Some(skin) = Arc::make_mut(&mut inst.mesh).skin.as_mut() {
                skin.joint_matrices = joint_matrices;
            }
        }
    }

    /// Re-reads the [`idtx_scene::WidgetPanelComp`] of panels already present in
    /// this snapshot and updates their rasterization inputs (elements, size,
    /// resolution, opacity, billboard, world matrix, `content_hash`) in place,
    /// from the ECS `world`. Used by the value-only widget fast path (e.g. an
    /// `OpenExec` compute writeback into a panel's `idtx:widget:text`): the session
    /// re-stamps `WidgetPanelComp` via `hydrate_widgets`, then this splices the
    /// fresh values into the warm snapshot without a whole-stage re-extract.
    ///
    /// `serial` is deliberately preserved so the renderer's structural / mesh GPU
    /// caches survive untouched; the widget pass is `content_hash`-gated, so only
    /// panels whose content actually changed re-rasterize. Panels are matched by
    /// the stable USD prim path (the same key the widget texture cache uses).
    ///
    /// Only panels whose prim path already exists in the snapshot are updated:
    /// a value-only edit never adds or removes a panel (a new/removed widget prim
    /// is a resync that takes the whole-stage topology re-hydrate path instead),
    /// and a panel that became invisible carries a `VISIBILITY` change that routes
    /// to a full re-extract, not here — so this never needs to add/drop entries.
    pub fn patch_widget_panels(&mut self, world: &mut World, stage_basis: Mat4) {
        if self.widget_panels.is_empty() && self.guide_meshes.is_empty() {
            return;
        }
        let mut current: HashMap<String, super::WidgetPanel> = HashMap::new();
        let mut q = world.query::<(
            &PrimNode,
            &WidgetPanelComp,
            &WorldTransform,
            Option<&VisibilityComp>,
        )>();
        for (node, panel, xform, vis) in q.iter(world) {
            if vis.is_some_and(|v| !v.visible) {
                continue;
            }
            if let Some(guide) = self.guide_meshes.iter_mut().find(|mesh| mesh.prim_path == node.path) {
                let mut mesh = tessellate_plane(panel.size[0], panel.size[1], GprimAxis::Z);
                mesh.prim_path.clone_from(&node.path);
                guide.mesh = Arc::new(mesh);
                guide.world_matrix = stage_basis * xform.matrix;
            }
            current.insert(
                node.path.clone(),
                super::WidgetPanel {
                    highlight: None,
                    surface: panel.surface.clone(),
                    prim_path: node.path.clone(),
                    world_matrix: stage_basis * xform.matrix,
                    size: panel.size,
                    resolution: panel.resolution,
                    billboard: panel.billboard,
                    double_sided: panel.double_sided,
                    opacity: panel.opacity,
                    space: panel.space,
                    elements: panel.elements.clone(),
                    content_hash: panel.content_hash,
                },
            );
        }
        for existing in &mut self.widget_panels {
            if let Some(fresh) = current.get(&existing.prim_path) {
                existing.clone_from(fresh);
            }
        }
    }

    /// Re-tessellates the meshes of the given parametric gprims (Cube / Sphere /
    /// Cylinder-Cone-Capsule / Plane) in place from the ECS, without a whole-stage
    /// re-extract and WITHOUT bumping the snapshot `serial`.
    ///
    /// Used by the value-only geometry fast path (e.g. an `OpenExec` writeback onto
    /// a cube `size`): the session re-hydrated the affected `CubeComp` /
    /// `SphereComp` / `ExtendedGprimComp` / `PlaneComp`, then this swaps each
    /// matching [`RenderMeshInstance::mesh`] for a freshly tessellated
    /// [`GpuMeshDesc`] (and re-resolves its material so a `primvars:displayColor`
    /// edit is honored). The new per-mesh `serial` (content-hashed from the new
    /// params) moves, so the renderer re-uploads just that mesh and re-bakes just
    /// its merge group, while every unrelated GPU / merge / material / AABB cache
    /// survives.
    ///
    /// INVARIANT: entries are mutated strictly in place by vector index — never
    /// pushed, removed, or reordered — so the merge planner's `MergeSource::Mesh(i)`
    /// indices stay valid. A value-only geometry edit never adds or removes a mesh
    /// (that would be a resync routed to the topology re-hydrate), so matching by
    /// prim path and updating in place is always sufficient; a `paths` entry with
    /// no matching snapshot mesh (e.g. a currently-invisible prim) is simply
    /// skipped. Poly `MeshComp` prims are deliberately NOT handled here — their
    /// `points` / topology edits keep the full re-extract path.
    pub fn patch_mesh_geometry(&mut self, world: &mut World, stage_basis: Mat4, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let want: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();

        // Phase 1 — rebuild each affected gprim's GpuMeshDesc + material exactly as
        // the full extract does, keyed by prim path. Borrows `world` and
        // `&self.materials` only; collects owned results so the query borrows end
        // before the in-place write in phase 2.
        let mut updates: Vec<(String, GpuMeshDesc, RenderMaterialDesc, Mat4)> = Vec::new();

        {
            let mut q = world
                .query::<(&PrimNode, &CubeComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, cube, xform, vis) in q.iter(world) {
                if cube.prototype_only || !want.contains(node.path.as_str()) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible) {
                    continue;
                }
                let mut gpu_mesh = tessellate_cube(cube.size * 0.5);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = cube.double_sided;
                gpu_mesh.material_path.clone_from(&cube.material_path);
                let mut material = lookup_material(&self.materials, cube.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    cube.material_path.as_ref(),
                    &cube.display_colors,
                );
                updates.push((node.path.clone(), gpu_mesh, material, stage_basis * xform.matrix));
            }
        }

        {
            let mut q = world
                .query::<(&PrimNode, &SphereComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, sphere, xform, vis) in q.iter(world) {
                if sphere.prototype_only || !want.contains(node.path.as_str()) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible) {
                    continue;
                }
                let mut gpu_mesh = tessellate_sphere(sphere.radius, 24, 36);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = sphere.double_sided;
                gpu_mesh.material_path.clone_from(&sphere.material_path);
                let mut material = lookup_material(&self.materials, sphere.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    sphere.material_path.as_ref(),
                    &sphere.display_colors,
                );
                updates.push((node.path.clone(), gpu_mesh, material, stage_basis * xform.matrix));
            }
        }

        {
            let mut q = world
                .query::<(&PrimNode, &ExtendedGprimComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, comp, xform, vis) in q.iter(world) {
                if comp.prototype_only || !want.contains(node.path.as_str()) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible) {
                    continue;
                }
                // `axis` is itself a geometry edit; fold it into the world matrix
                // exactly as the full extract does (the tessellation is Y-up).
                let world_matrix = stage_basis * xform.matrix * ext_gprim_axis_rot(comp.axis);
                let mut gpu_mesh = tessellate_ext_gprim(comp);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = comp.double_sided;
                gpu_mesh.material_path.clone_from(&comp.material_path);
                let mut material = lookup_material(&self.materials, comp.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    comp.material_path.as_ref(),
                    &comp.display_colors,
                );
                updates.push((node.path.clone(), gpu_mesh, material, world_matrix));
            }
        }

        {
            let mut q = world
                .query::<(&PrimNode, &PlaneComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, plane, xform, vis) in q.iter(world) {
                if plane.prototype_only || !want.contains(node.path.as_str()) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible) {
                    continue;
                }
                let mut gpu_mesh = tessellate_plane(plane.width, plane.length, plane.axis);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = plane.double_sided;
                gpu_mesh.material_path.clone_from(&plane.material_path);
                let mut material = lookup_material(&self.materials, plane.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    plane.material_path.as_ref(),
                    &plane.display_colors,
                );
                updates.push((node.path.clone(), gpu_mesh, material, stage_basis * xform.matrix));
            }
        }

        // Phase 2 — splice the fresh geometry into the warm snapshot in place.
        for (path, gpu_mesh, material, world_matrix) in updates {
            if let Some(inst) = self.meshes.iter_mut().chain(self.guide_meshes.iter_mut()).find(|m| m.prim_path == path) {
                inst.mesh = Arc::new(gpu_mesh);
                inst.material = material;
                inst.world_matrix = world_matrix;
            }
        }
    }

    /// Scoped runtime-spawn fast path: splice the newly-spawned / retopologised
    /// subtree `upserts` (and drop `removed_paths`) into this warm snapshot
    /// **without** bumping [`serial`](Self::serial) / [`instance_serial`](Self::instance_serial),
    /// so the renderer's static draw-merge, instance and scene-AABB caches survive
    /// and only the spliced prims incur GPU work (a per-prim `mesh_gpu_cache` miss).
    ///
    /// Handles the common static spawn: polygon meshes (including GeomSubsets),
    /// parametric gprims (Cube / Sphere / Cylinder / Cone / Capsule / Plane),
    /// widget panels, and their materials. Spliced meshes are recorded in
    /// [`standalone_paths`](Self::standalone_paths) so the merge planner leaves
    /// them out of every static merge group (keeping existing groups' membership —
    /// and thus the renderer's `structure_matches` cache — intact).
    ///
    /// Returns `false` — signalling the caller to fall back to a full
    /// [`extract_with_shadow`](Self::extract_with_shadow) — when the subtree
    /// contains anything the splice cannot faithfully reproduce in isolation:
    /// skinning ([`SkinBindingComp`]), native / point instancing
    /// ([`NativeInstanceComp`] / [`PointInstancerComp`]), or a light
    /// ([`LightComp`], which would need the whole-scene shadow-AABB refit). The
    /// full path is always correct, so the fallback never under-renders.
    pub fn splice_subtree(
        &mut self,
        world: &mut World,
        stage_basis: Mat4,
        upserts: &[idtx_scene::PrimId],
        removed_paths: &[String],
    ) -> bool {
        if self.purpose_filter.is_some() {
            return false;
        }
        let upsert_set: HashSet<idtx_scene::PrimId> = upserts.iter().copied().collect();
        if upsert_set.is_empty() && removed_paths.is_empty() {
            return true;
        }

        // Resolve the scoped prims' paths and reject exotic content the splice
        // cannot reproduce in isolation (fall back to a full extract).
        let mut want: HashSet<String> = HashSet::new();
        let mut want_materials: HashSet<String> = HashSet::new();
        {
            let mut q = world.query::<(
                &PrimNode,
                Option<&SkinBindingComp>,
                Option<&NativeInstanceComp>,
                Option<&PointInstancerComp>,
                Option<&LightComp>,
                Option<&MaterialComp>,
            )>();
            for (node, skin, native, point, light, mat) in q.iter(world) {
                if !upsert_set.contains(&node.prim) {
                    continue;
                }
                if skin.is_some() || native.is_some() || point.is_some() || light.is_some() {
                    return false;
                }
                want.insert(node.path.clone());
                if mat.is_some() {
                    want_materials.insert(node.path.clone());
                }
            }
        }

        // Remove stale entries for the scoped + removed prims. An entry is scoped
        // when its prim_path equals one of those paths or is a sub-label of it
        // (`<path>/subset_x`, `<path>/remainder`), compared on the `/`-delimited
        // namespace so `/World/Cube` never matches `/World/CubeJar`.
        let under = |path: &str, base: &str| -> bool {
            path == base
                || (path.len() > base.len()
                    && path.as_bytes()[base.len()] == b'/'
                    && path.starts_with(base))
        };
        let is_scoped = |path: &str| -> bool {
            want.iter().any(|b| under(path, b)) || removed_paths.iter().any(|b| under(path, b))
        };
        self.meshes.retain(|m| !is_scoped(&m.prim_path));
        self.widget_panels.retain(|w| !is_scoped(&w.prim_path));
        self.standalone_paths.retain(|p| !is_scoped(p));

        // Materials for the scoped subtree (so bound-material lookups resolve;
        // external pre-existing materials already live in `self.materials`).
        {
            let mut q = world.query::<(&PrimNode, &MaterialComp)>();
            for (node, mat) in q.iter(world) {
                if want_materials.contains(&node.path) {
                    self.materials.insert(
                        node.path.clone(),
                        RenderMaterialDesc::from_material_record(&mat.record),
                    );
                }
            }
        }

        // ── Polygon meshes (non-skinned — skinned subtrees rejected above) ──
        {
            let mut q = world.query::<(
                &PrimNode,
                &MeshComp,
                &WorldTransform,
                Option<&VisibilityComp>,
            )>();
            let mut pending: Vec<RenderMeshInstance> = Vec::new();
            for (node, mesh, xform, vis) in q.iter(world) {
                if mesh.prototype_only || !want.contains(&node.path) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible || v.purpose != RenderPurpose::Default) {
                    continue;
                }
                let world_matrix = stage_basis * xform.matrix;
                let mut gpu_mesh = GpuMeshDesc::from_mesh_comp(&node.path, mesh);
                gpu_mesh.material_path.clone_from(&mesh.material_path);
                let mut material = lookup_material(&self.materials, mesh.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    mesh.material_path.as_ref(),
                    &mesh.display_colors,
                );
                let purpose = vis.map_or(RenderPurpose::Default, |v| v.purpose);
                let casts_shadow = vis.map_or(true, |v| v.casts_shadow);

                if !mesh.material_subsets.is_empty() {
                    let mut covered: HashSet<u32> = HashSet::new();
                    for subset in &mesh.material_subsets {
                        if subset.face_indices.is_empty() {
                            continue;
                        }
                        covered.extend(subset.face_indices.iter().copied());
                        let mut sub_mesh = GpuMeshDesc::subset_by_faces(
                            &gpu_mesh,
                            &mesh.face_vertex_counts,
                            &subset.face_indices,
                        );
                        if sub_mesh.indices.is_empty() {
                            continue;
                        }
                        let sub_mat =
                            lookup_material(&self.materials, subset.material_path.as_ref());
                        record_udim_and_time(
                            &mut self.udim_tiles,
                            &mut self.stats,
                            &mut sub_mesh,
                            &sub_mat,
                        );
                        pending.push(RenderMeshInstance {
                            prim_path: format!(
                                "{}/subset_{}",
                                node.path,
                                subset.material_path.as_deref().unwrap_or("default"),
                            ),
                            mesh: Arc::new(sub_mesh),
                            world_matrix,
                            material: sub_mat,
                            purpose,
                            casts_shadow,
                        });
                    }
                    let remainder: Vec<u32> = (0..u32_sat(mesh.face_vertex_counts.len()))
                        .filter(|f| !covered.contains(f))
                        .collect();
                    if !remainder.is_empty() {
                        let mut rem_mesh = GpuMeshDesc::subset_by_faces(
                            &gpu_mesh,
                            &mesh.face_vertex_counts,
                            &remainder,
                        );
                        if !rem_mesh.indices.is_empty() {
                            record_udim_and_time(
                                &mut self.udim_tiles,
                                &mut self.stats,
                                &mut rem_mesh,
                                &material,
                            );
                            pending.push(RenderMeshInstance {
                                prim_path: format!("{}/remainder", node.path),
                                mesh: Arc::new(rem_mesh),
                                world_matrix,
                                material,
                                purpose,
                                casts_shadow,
                            });
                        }
                    }
                    continue;
                }

                record_udim_and_time(
                    &mut self.udim_tiles,
                    &mut self.stats,
                    &mut gpu_mesh,
                    &material,
                );
                pending.push(RenderMeshInstance {
                    prim_path: node.path.clone(),
                    mesh: Arc::new(gpu_mesh),
                    world_matrix,
                    material,
                    purpose,
                    casts_shadow,
                });
            }
            for inst in pending {
                self.standalone_paths.insert(inst.prim_path.clone());
                self.meshes.push(inst);
                self.stats.mesh_count += 1;
            }
        }

        // ── Parametric gprims (reuse the value-only patch path's query shape) ──
        // Build fresh gprim meshes filtered to the scoped subtree and push them as
        // standalone instances.
        let mut gprims: Vec<RenderMeshInstance> = Vec::new();
        {
            let mut q = world
                .query::<(&PrimNode, &CubeComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, cube, xform, vis) in q.iter(world) {
                if cube.prototype_only || !want.contains(&node.path) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible || v.purpose != RenderPurpose::Default) {
                    continue;
                }
                let mut gpu_mesh = tessellate_cube(cube.size * 0.5);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = cube.double_sided;
                gpu_mesh.material_path.clone_from(&cube.material_path);
                let mut material = lookup_material(&self.materials, cube.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    cube.material_path.as_ref(),
                    &cube.display_colors,
                );
                gprims.push(RenderMeshInstance {
                    prim_path: node.path.clone(),
                    mesh: Arc::new(gpu_mesh),
                    world_matrix: stage_basis * xform.matrix,
                    material,
                    purpose: vis.map_or(RenderPurpose::Default, |v| v.purpose),
                    casts_shadow: vis.map_or(true, |v| v.casts_shadow),
                });
            }
        }
        {
            let mut q = world
                .query::<(&PrimNode, &SphereComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, sphere, xform, vis) in q.iter(world) {
                if sphere.prototype_only || !want.contains(&node.path) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible || v.purpose != RenderPurpose::Default) {
                    continue;
                }
                let mut gpu_mesh = tessellate_sphere(sphere.radius, 24, 36);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = sphere.double_sided;
                gpu_mesh.material_path.clone_from(&sphere.material_path);
                let mut material = lookup_material(&self.materials, sphere.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    sphere.material_path.as_ref(),
                    &sphere.display_colors,
                );
                gprims.push(RenderMeshInstance {
                    prim_path: node.path.clone(),
                    mesh: Arc::new(gpu_mesh),
                    world_matrix: stage_basis * xform.matrix,
                    material,
                    purpose: vis.map_or(RenderPurpose::Default, |v| v.purpose),
                    casts_shadow: vis.map_or(true, |v| v.casts_shadow),
                });
            }
        }
        {
            let mut q = world.query::<(
                &PrimNode,
                &ExtendedGprimComp,
                &WorldTransform,
                Option<&VisibilityComp>,
            )>();
            for (node, comp, xform, vis) in q.iter(world) {
                if comp.prototype_only || !want.contains(&node.path) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible || v.purpose != RenderPurpose::Default) {
                    continue;
                }
                let world_matrix = stage_basis * xform.matrix * ext_gprim_axis_rot(comp.axis);
                let mut gpu_mesh = tessellate_ext_gprim(comp);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = comp.double_sided;
                gpu_mesh.material_path.clone_from(&comp.material_path);
                let mut material = lookup_material(&self.materials, comp.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    comp.material_path.as_ref(),
                    &comp.display_colors,
                );
                gprims.push(RenderMeshInstance {
                    prim_path: node.path.clone(),
                    mesh: Arc::new(gpu_mesh),
                    world_matrix,
                    material,
                    purpose: vis.map_or(RenderPurpose::Default, |v| v.purpose),
                    casts_shadow: vis.map_or(true, |v| v.casts_shadow),
                });
            }
        }
        {
            let mut q = world
                .query::<(&PrimNode, &PlaneComp, &WorldTransform, Option<&VisibilityComp>)>();
            for (node, plane, xform, vis) in q.iter(world) {
                if plane.prototype_only || !want.contains(&node.path) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible || v.purpose != RenderPurpose::Default) {
                    continue;
                }
                let mut gpu_mesh = tessellate_plane(plane.width, plane.length, plane.axis);
                gpu_mesh.prim_path.clone_from(&node.path);
                gpu_mesh.double_sided = plane.double_sided;
                gpu_mesh.material_path.clone_from(&plane.material_path);
                let mut material = lookup_material(&self.materials, plane.material_path.as_ref());
                apply_display_color_fallback(
                    &mut material,
                    plane.material_path.as_ref(),
                    &plane.display_colors,
                );
                gprims.push(RenderMeshInstance {
                    prim_path: node.path.clone(),
                    mesh: Arc::new(gpu_mesh),
                    world_matrix: stage_basis * xform.matrix,
                    material,
                    purpose: vis.map_or(RenderPurpose::Default, |v| v.purpose),
                    casts_shadow: vis.map_or(true, |v| v.casts_shadow),
                });
            }
        }
        for inst in gprims {
            self.standalone_paths.insert(inst.prim_path.clone());
            self.meshes.push(inst);
            self.stats.mesh_count += 1;
        }

        // ── Widget panels (world-space quads; common spawn target) ─────────
        {
            let mut q = world.query::<(
                &PrimNode,
                &WidgetPanelComp,
                &WorldTransform,
                Option<&VisibilityComp>,
            )>();
            let mut panels: Vec<super::WidgetPanel> = Vec::new();
            for (node, panel, xform, vis) in q.iter(world) {
                if !want.contains(&node.path) {
                    continue;
                }
                if vis.is_some_and(|v| !v.visible) {
                    continue;
                }
                panels.push(super::WidgetPanel {
                    highlight: None,
                    surface: panel.surface.clone(),
                    prim_path: node.path.clone(),
                    world_matrix: stage_basis * xform.matrix,
                    size: panel.size,
                    resolution: panel.resolution,
                    billboard: panel.billboard,
                    double_sided: panel.double_sided,
                    opacity: panel.opacity,
                    space: panel.space,
                    elements: panel.elements.clone(),
                    content_hash: panel.content_hash,
                });
            }
            self.widget_panels.extend(panels);
        }

        true
    }
}
