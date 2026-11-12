//! XR controller visualisation meshes.
//!
//! [`ControllerMeshCache`] provides per-hand slices of transient
//! [`RenderMeshInstance`]s injected into the render snapshot each frame when an
//! XR preview is active.
//!
//! Two mesh sources, in priority order:
//!
//! 1. **Real controller geometry**, sourced once from the hydrated ECS world.
//!    "Preview in Runtime" mounts a generated XR sublayer that references the
//!    controller assets under `/XR/Controllers/{Left,Right}/Model`, with the
//!    proxy root authored `visibility = "invisible"` (see
//!    `idtx-studio-shell::preview_assembly`). The inner `Model` node carries a
//!    per-asset metre / Y-up correction (`xformOp:scale` / `xformOp:rotateX`)
//!    derived from the controller asset's *own* `upAxis` / `metersPerUnit`.
//!    [`source_geometry`] walks each proxy subtree, groups geometry by bound
//!    USD material path, and produces one [`ControllerMeshPart`] per group. The
//!    geometry is baked into the proxy root's local frame.
//!
//!    Materials are filled separately by [`fill_materials`], which looks up
//!    each part's recorded USD material path in the snapshot's pre-resolved
//!    [`RenderMaterialDesc`] map. This two-phase design decouples geometry
//!    extraction (which only needs the ECS world) from material resolution
//!    (which needs the snapshot) so that both can run at the right time without
//!    a race between [`render_xr`] and [`render_one_frame`].
//!
//! 2. **Fallback capsule** (radius 2 cm, 6 cm mid-cylinder) when no proxy
//!    resolves (desktop/world-only assembly, or an unresolved asset) — the
//!    original Phase-8 behaviour.
//!
//! [`source_geometry`]: ControllerMeshCache::source_geometry
//! [`fill_materials`]: ControllerMeshCache::fill_materials
//! [`left_instances`]: ControllerMeshCache::left_instances
//! [`right_instances`]: ControllerMeshCache::right_instances
//!
//! # Cache key stability
//!
//! Each part carries a stable `prim_path` of the form `{cache_key}/{i}` where
//! `i` is the 0-based index of the material group, ordered deterministically
//! by USD material path string ([`BTreeMap`]). The capsule fallback is always
//! index `0`. The renderer's GPU-resource cache is keyed on
//! `prim_path + material_path`, so a capsule→real-mesh flip re-uploads once
//! (fresh [`next_mesh_serial`]) and steady-state frames never reallocate.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use glam::{Mat4, Vec3};
use idtx_render::GpuMeshDesc;
use idtx_render::material::{RenderMaterialDesc, fallback_material};
use idtx_render::mesh::{
    aabb_of_positions, next_mesh_serial, tessellate_cone, tessellate_cube, tessellate_cylinder,
    tessellate_sphere,
};
use idtx_render::scene_extract::RenderMeshInstance;
use idtx_scene::{
    CubeComp, ExtendedGprimComp, MeshComp, RenderPurpose, SceneHydrator, SphereComp, WorldTransform,
};
use idtx_usd::{ExtendedGprimKind, GprimAxis, MaterialSubsetRecord};

/// Stable cache-key path prefixes for the synthetic controller mesh instances.
pub const LEFT_CTRL_PATH: &str = "__xr_left_ctrl__";
pub const RIGHT_CTRL_PATH: &str = "__xr_right_ctrl__";

/// USD prim paths of the invisible controller proxies authored by the
/// conductor's XR sublayer (`idtx-studio-shell::preview_assembly::xr_layer_text`).
/// Kept in lock-step with that authoring — this is a documented coupling.
pub const LEFT_PROXY_PATH: &str = "/XR/Controllers/Left";
pub const RIGHT_PROXY_PATH: &str = "/XR/Controllers/Right";

/// Controller dimensions — 2 cm radius, 6 cm cylinder mid-section.
const CTRL_RADIUS: f32 = 0.02;
const CTRL_MID_HEIGHT: f32 = 0.06;
const CTRL_SLICES: u32 = 16;
const CTRL_HEMI_STACKS: u32 = 4;

/// One geometry+material unit of a controller mesh.
///
/// A real controller asset (USDZ) typically contains several sub-meshes bound
/// to distinct materials. Each such group becomes one `ControllerMeshPart`.
/// The `usd_material_path` retains the original USD material prim path so that
/// [`fill_materials`] can patch the material from the snapshot once it is
/// available, without re-baking geometry.
#[derive(Clone)]
struct ControllerMeshPart {
    mesh: Arc<GpuMeshDesc>,
    material: RenderMaterialDesc,
    /// The USD material prim path this part was bound to, or `None` for
    /// unbound geometry. Used by [`fill_materials`] to look up the real
    /// [`RenderMaterialDesc`] from the snapshot's material map.
    usd_material_path: Option<String>,
}

impl std::fmt::Debug for ControllerMeshPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerMeshPart")
            .field("prim_path", &self.mesh.prim_path)
            .field("usd_material_path", &self.usd_material_path)
            .field(
                "material_resolved",
                &self.material.material_path.starts_with('/'),
            )
            .finish()
    }
}

/// Left and right controller meshes for an XR preview session.
#[derive(Debug)]
pub struct ControllerMeshCache {
    left_capsule: Vec<ControllerMeshPart>,
    right_capsule: Vec<ControllerMeshPart>,
    /// Real geometry for the left controller, or `None` → capsule fallback.
    left_real: Option<Vec<ControllerMeshPart>>,
    /// Real geometry for the right controller.
    right_real: Option<Vec<ControllerMeshPart>>,
    /// True once geometry has been baked from ECS (expensive; source-once).
    geometry_sourced: bool,
    /// True once all parts that have a `usd_material_path` have been patched
    /// with a real USD [`RenderMaterialDesc`] from the snapshot. Once true
    /// [`fill_materials`] becomes a no-op.
    materials_ready: bool,
    /// Snapshot serial of the most recent [`fill_materials`] attempt.
    /// Prevents redundant re-runs when the snapshot has not changed.
    last_mat_serial: u64,
}

impl ControllerMeshCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            left_capsule: make_capsule_parts(LEFT_CTRL_PATH),
            right_capsule: make_capsule_parts(RIGHT_CTRL_PATH),
            left_real: None,
            right_real: None,
            geometry_sourced: false,
            materials_ready: false,
            last_mat_serial: 0,
        }
    }

    /// **Geometry**: Bake real controller geometry from the hydrated
    /// ECS proxies. Idempotent after the first call. Materials are left at the
    /// fallback placeholder until [`fill_materials`] resolves them.
    ///
    /// Call this eagerly when the XR preview starts (ECS is ready, snapshot may
    /// not be). Geometry never changes after the stage is assembled.
    pub fn source_geometry(&mut self, hydrator: &SceneHydrator) {
        if self.geometry_sourced {
            return;
        }
        self.left_real = gather_controller_geometry(hydrator, LEFT_PROXY_PATH, LEFT_CTRL_PATH);
        self.right_real = gather_controller_geometry(hydrator, RIGHT_PROXY_PATH, RIGHT_CTRL_PATH);
        self.geometry_sourced = true;
        tracing::info!(
            left_parts = self.left_real.as_ref().map(|v| v.len()),
            right_parts = self.right_real.as_ref().map(|v| v.len()),
            "xr_ctrl source_geometry: left={:?} right={:?}",
            self.left_real.as_ref().map(|v| v
                .iter()
                .map(|p| p.usd_material_path.as_deref().unwrap_or("<none>"))
                .collect::<Vec<_>>()),
            self.right_real.as_ref().map(|v| v
                .iter()
                .map(|p| p.usd_material_path.as_deref().unwrap_or("<none>"))
                .collect::<Vec<_>>()),
        );
    }

    /// **Materials**: Patch each part's material from `materials`
    /// (the snapshot's pre-resolved USD material map). Called every frame from
    /// [`inject_xr_controller_meshes`] until all bound materials are resolved.
    ///
    /// `snapshot_serial` prevents redundant work: if the snapshot hasn't
    /// changed since the last call this is a no-op.  Once all parts with a
    /// `usd_material_path` have been patched this becomes a permanent no-op
    /// (`materials_ready = true`).
    ///
    /// Snapshot [`materials`] is populated by the normal ECS extraction pipeline
    /// without visibility filtering, so controller material prims ARE present
    /// even though their mesh siblings are invisible.
    pub fn fill_materials(
        &mut self,
        materials: &HashMap<String, RenderMaterialDesc>,
        snapshot_serial: u64,
    ) {
        if self.materials_ready {
            return;
        }
        if self.last_mat_serial == snapshot_serial {
            return;
        }
        self.last_mat_serial = snapshot_serial;

        // Diagnostic: log snapshot.materials keys near the controller paths.
        {
            let ctrl_keys: Vec<&str> = materials
                .keys()
                .filter(|k| {
                    k.contains("/XR/") || k.contains("Controller") || k.contains("controller")
                })
                .map(String::as_str)
                .take(20)
                .collect();
            tracing::info!(
                serial = snapshot_serial,
                total_materials = materials.len(),
                ctrl_material_keys = ?ctrl_keys,
                "xr_ctrl fill_materials: snapshot has {} materials, {} match XR/controller",
                materials.len(),
                ctrl_keys.len(),
            );
        }

        let left_found = patch_part_materials(&mut self.left_real, materials);
        let right_found = patch_part_materials(&mut self.right_real, materials);

        tracing::info!(
            left = ?left_found,
            right = ?right_found,
            "xr_ctrl fill_materials: patch result left={:?} right={:?}",
            left_found,
            right_found,
        );

        // Mark complete when:
        // (a) Both proxies had no geometry → nothing to resolve.
        // (b) All parts that have a usd_material_path were successfully patched.
        if self.left_real.is_none() && self.right_real.is_none() {
            self.materials_ready = true;
        } else if left_found != PatchResult::Pending && right_found != PatchResult::Pending {
            self.materials_ready = true;
        }
    }

    fn left_parts(&self) -> &[ControllerMeshPart] {
        self.left_real.as_deref().unwrap_or(&self.left_capsule)
    }

    fn right_parts(&self) -> &[ControllerMeshPart] {
        self.right_real.as_deref().unwrap_or(&self.right_capsule)
    }

    /// Build transient [`RenderMeshInstance`]s for the left controller at
    /// `world_matrix`. Returns an empty `Vec` when `world_matrix` is `None`.
    #[must_use]
    pub fn left_instances(&self, world_matrix: Option<Mat4>) -> Vec<RenderMeshInstance> {
        world_matrix.map_or_else(Vec::new, |m| mesh_instances(self.left_parts(), m))
    }

    /// Build transient [`RenderMeshInstance`]s for the right controller at
    /// `world_matrix`. Returns an empty `Vec` when `world_matrix` is `None`.
    #[must_use]
    pub fn right_instances(&self, world_matrix: Option<Mat4>) -> Vec<RenderMeshInstance> {
        world_matrix.map_or_else(Vec::new, |m| mesh_instances(self.right_parts(), m))
    }
}

impl Default for ControllerMeshCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Replace the controller mesh instances in `out` with the current pose.
///
/// Strips any previous controller instances (stable `starts_with` prefix check)
/// then pushes all material parts for each visible, tracked hand.
pub fn apply_controller_instances(
    cache: &ControllerMeshCache,
    left_pose: Option<Mat4>,
    right_pose: Option<Mat4>,
    show_left: bool,
    show_right: bool,
    out: &mut Vec<RenderMeshInstance>,
) {
    out.retain(|m| {
        !m.prim_path.starts_with(LEFT_CTRL_PATH) && !m.prim_path.starts_with(RIGHT_CTRL_PATH)
    });
    if show_left {
        out.extend(cache.left_instances(left_pose));
    }
    if show_right {
        out.extend(cache.right_instances(right_pose));
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_capsule_parts(cache_key: &str) -> Vec<ControllerMeshPart> {
    let part_key = format!("{}/0", cache_key);
    let mut mesh = idtx_render::mesh::tessellate_capsule(
        CTRL_RADIUS,
        CTRL_MID_HEIGHT,
        CTRL_SLICES,
        CTRL_HEMI_STACKS,
    );
    mesh.prim_path = part_key.clone();
    vec![ControllerMeshPart {
        mesh: Arc::new(mesh),
        material: fallback_material(&part_key),
        usd_material_path: None,
    }]
}

fn mesh_instances(parts: &[ControllerMeshPart], world_matrix: Mat4) -> Vec<RenderMeshInstance> {
    parts
        .iter()
        .map(|p| RenderMeshInstance {
            prim_path: p.mesh.prim_path.clone(),
            mesh: Arc::clone(&p.mesh),
            world_matrix,
            material: p.material.clone(),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        })
        .collect()
}

/// Result of [`patch_part_materials`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchResult {
    /// No proxy found (None parts) — nothing to patch.
    NoProxy,
    /// Proxy found; all bound USD material paths were successfully resolved.
    Resolved,
    /// Proxy found; at least one bound USD material path is still missing from
    /// the snapshot — retry next frame.
    Pending,
}

/// Patch real [`RenderMaterialDesc`] values from `materials` into each part's
/// material field, using the part's stored `usd_material_path`.
///
/// Returns [`PatchResult::Resolved`] when every part that has a
/// `usd_material_path` found its material in `materials`.
/// Returns [`PatchResult::Pending`] when at least one material is still absent
/// (snapshot hasn't caught up with the XR assembly stage yet).
/// Returns [`PatchResult::NoProxy`] when `parts` is `None`.
fn patch_part_materials(
    parts: &mut Option<Vec<ControllerMeshPart>>,
    materials: &HashMap<String, RenderMaterialDesc>,
) -> PatchResult {
    let Some(parts) = parts.as_mut() else {
        return PatchResult::NoProxy;
    };
    let mut all_resolved = true;
    for part in parts.iter_mut() {
        let Some(ref usd_path) = part.usd_material_path else {
            tracing::info!(
                "  part {} → usd_material_path=None (unbound)",
                part.mesh.prim_path
            );
            continue;
        };
        if let Some(real_mat) = materials.get(usd_path) {
            tracing::info!(
                "  part {} → {} FOUND (mat={})",
                part.mesh.prim_path,
                usd_path,
                real_mat.material_path
            );
            part.material = real_mat.clone();
            // Bump the mesh serial to guarantee a renderer cache miss on the next
            // frame. The fallback and real materials both may produce fingerprint=0
            // (first ^ second = 0 in the bridge IR hasher), so dirty_material would
            // never fire. A serial change forces dirty_geometry = true → full
            // GPU-cache rebuild → real bind group. One-time cost per session.
            let mut new_mesh = (*part.mesh).clone();
            new_mesh.serial = next_mesh_serial();
            part.mesh = Arc::new(new_mesh);
        } else {
            tracing::info!(
                "  part {} → {} NOT FOUND in snapshot.materials",
                part.mesh.prim_path,
                usd_path
            );
            all_resolved = false;
        }
    }
    if all_resolved {
        PatchResult::Resolved
    } else {
        PatchResult::Pending
    }
}

/// Split a mesh into `(material_key, sub-geometry)` groups, matching how
/// [`RenderSnapshot::extract`] resolves materials for the normal render path.
///
/// When the mesh has no `material_subsets`, this yields a single group keyed by
/// the whole-mesh `material_path` (empty string when unbound). When GeomSubsets
/// are present, each subset becomes its own group (keyed by the subset's bound
/// material path) and any faces not covered by a subset fall back to the mesh's
/// own `material_path` — exactly the split `extract` performs. Without this,
/// controllers whose materials are bound only via GeomSubsets (the typical
/// imported-USDZ layout) render with the fallback (white) material.
///
/// Pure over `GpuMeshDesc` + record slices — no ECS/USD linkage — so it is
/// CI-testable without an `OpenUSD` build (matches `bake_into`).
///
/// [`RenderSnapshot::extract`]: idtx_render::scene_extract::RenderSnapshot::extract
fn mesh_material_parts(
    gpu_mesh: &GpuMeshDesc,
    face_vertex_counts: &[i32],
    material_path: Option<&str>,
    subsets: &[MaterialSubsetRecord],
) -> Vec<(String, GpuMeshDesc)> {
    if subsets.is_empty() {
        return vec![(material_path.unwrap_or("").to_owned(), gpu_mesh.clone())];
    }

    let mut out: Vec<(String, GpuMeshDesc)> = Vec::new();
    let mut covered: HashSet<u32> = HashSet::new();
    for subset in subsets {
        if subset.face_indices.is_empty() {
            continue;
        }
        covered.extend(subset.face_indices.iter().copied());
        let sub = GpuMeshDesc::subset_by_faces(gpu_mesh, face_vertex_counts, &subset.face_indices);
        if sub.indices.is_empty() {
            continue;
        }
        out.push((
            subset.material_path.as_deref().unwrap_or("").to_owned(),
            sub,
        ));
    }

    // Faces not covered by any subset get the mesh's own (whole-mesh) material.
    let remainder: Vec<u32> = (0..face_vertex_counts.len() as u32)
        .filter(|f| !covered.contains(f))
        .collect();
    if !remainder.is_empty() {
        let rem = GpuMeshDesc::subset_by_faces(gpu_mesh, face_vertex_counts, &remainder);
        if !rem.indices.is_empty() {
            out.push((material_path.unwrap_or("").to_owned(), rem));
        }
    }
    out
}

/// Bake geometry from the proxy subtree at `root_path` into one
/// [`ControllerMeshPart`] per material group. Materials are left at the
/// fallback placeholder; call [`patch_part_materials`] with the snapshot's
/// material map to fill them in.
///
/// Groups are keyed by `MeshComp::material_path` string (empty for unbound
/// gprims) and ordered by [`BTreeMap`] for stable part indices.
fn gather_controller_geometry(
    hydrator: &SceneHydrator,
    root_path: &str,
    cache_key: &str,
) -> Option<Vec<ControllerMeshPart>> {
    let world = hydrator.world();
    let root_entity = hydrator.entity_for_path(root_path)?;
    let root_world = world.get::<WorldTransform>(root_entity)?.matrix;
    let root_inv = root_world.inverse();

    // BTreeMap gives deterministic iteration order so part indices are stable.
    let mut groups: BTreeMap<String, MergedMesh> = BTreeMap::new();

    for entity in hydrator.subtree_entities(root_path) {
        let Some(child_world) = world.get::<WorldTransform>(entity).map(|w| w.matrix) else {
            continue;
        };
        let rel = root_inv * child_world;

        if let Some(mesh) = world.get::<MeshComp>(entity) {
            if mesh.prototype_only {
                continue;
            }
            // Mirror `RenderSnapshot::extract`: a controller mesh (imported USDZ)
            // usually binds several materials via GeomSubsets, and often has no
            // whole-mesh binding at all. Splitting per subset here is what gives
            // the hand-tracked copies their materials — reading `material_path`
            // alone leaves subset-bound faces unbound (→ fallback/white mesh).
            let gpu_mesh = GpuMeshDesc::from_mesh_comp("", mesh);
            for (mat_key, sub) in mesh_material_parts(
                &gpu_mesh,
                &mesh.face_vertex_counts,
                mesh.material_path.as_deref(),
                &mesh.material_subsets,
            ) {
                groups.entry(mat_key).or_default().bake(&sub, rel);
            }
        } else if let Some(cube) = world.get::<CubeComp>(entity) {
            if cube.prototype_only {
                continue;
            }
            let key = cube.material_path.as_deref().unwrap_or("").to_owned();
            groups
                .entry(key)
                .or_default()
                .bake(&tessellate_cube(cube.size * 0.5), rel);
        } else if let Some(sphere) = world.get::<SphereComp>(entity) {
            if sphere.prototype_only {
                continue;
            }
            let key = sphere.material_path.as_deref().unwrap_or("").to_owned();
            groups
                .entry(key)
                .or_default()
                .bake(&tessellate_sphere(sphere.radius, 24, 36), rel);
        } else if let Some(ext) = world.get::<ExtendedGprimComp>(entity) {
            if ext.prototype_only {
                continue;
            }
            let axis_rot = match ext.axis {
                GprimAxis::Y => Mat4::IDENTITY,
                GprimAxis::X => Mat4::from_rotation_z(-std::f32::consts::FRAC_PI_2),
                GprimAxis::Z => Mat4::from_rotation_x(std::f32::consts::FRAC_PI_2),
            };
            let g = match ext.kind {
                ExtendedGprimKind::Cylinder => {
                    tessellate_cylinder(ext.radius, ext.height * 0.5, 32)
                }
                ExtendedGprimKind::Cone => tessellate_cone(ext.radius, ext.height * 0.5, 32),
                ExtendedGprimKind::Capsule => {
                    idtx_render::mesh::tessellate_capsule(ext.radius, ext.height, 32, 8)
                }
            };
            let key = ext.material_path.as_deref().unwrap_or("").to_owned();
            groups.entry(key).or_default().bake(&g, rel * axis_rot);
        }
    }

    if groups.is_empty() {
        return None;
    }

    let parts: Vec<ControllerMeshPart> = groups
        .into_iter()
        .enumerate()
        .map(|(i, (usd_mat_path, merged))| {
            let part_key = format!("{}/{}", cache_key, i);
            let usd_material_path = if usd_mat_path.is_empty() {
                None
            } else {
                Some(usd_mat_path)
            };
            ControllerMeshPart {
                mesh: Arc::new(merged.into_gpu_mesh_desc(&part_key)),
                material: fallback_material(&part_key),
                usd_material_path,
            }
        })
        .collect();

    Some(parts)
}

/// Accumulator that merges several transformed source meshes into one.
#[derive(Default)]
struct MergedMesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    texcoords: Vec<[f32; 2]>,
    vertex_colors: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl MergedMesh {
    fn bake(&mut self, src: &GpuMeshDesc, rel: Mat4) {
        bake_into(
            self,
            &src.positions,
            &src.normals,
            &src.texcoords,
            &src.vertex_colors,
            &src.indices,
            rel,
        );
    }

    fn into_gpu_mesh_desc(self, prim_path: &str) -> GpuMeshDesc {
        GpuMeshDesc {
            prim_path: prim_path.to_owned(),
            local_aabb: aabb_of_positions(&self.positions),
            positions: self.positions,
            normals: self.normals,
            tangents: Vec::new(),
            texcoords: self.texcoords,
            vertex_colors: self.vertex_colors,
            indices: self.indices,
            double_sided: false,
            material_path: None,
            skin: None,
            serial: next_mesh_serial(),
        }
    }
}

/// Transform source geometry by `rel` and append it to `dst`, offsetting
/// indices by the running vertex count. Normals are transformed by the
/// inverse-transpose and renormalised; missing UVs / colours default to
/// `[0,0]` / white.
///
/// Pure over `glam` + slices — no USD/ECS types — CI-testable without USD linkage.
fn bake_into(
    dst: &mut MergedMesh,
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    texcoords: &[[f32; 2]],
    vertex_colors: &[[f32; 3]],
    indices: &[u32],
    rel: Mat4,
) {
    let base = u32::try_from(dst.positions.len()).unwrap_or(u32::MAX);
    let normal_mat = rel.inverse().transpose();
    for (i, p) in positions.iter().enumerate() {
        dst.positions
            .push(rel.transform_point3(Vec3::from(*p)).to_array());
        let n = normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
        let tn = normal_mat
            .transform_vector3(Vec3::from(n))
            .normalize_or_zero();
        dst.normals.push(tn.to_array());
        dst.texcoords
            .push(texcoords.get(i).copied().unwrap_or([0.0, 0.0]));
        dst.vertex_colors
            .push(vertex_colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0]));
    }
    dst.indices.extend(indices.iter().map(|idx| idx + base));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_builds_without_panic() {
        let cache = ControllerMeshCache::new();
        assert!(
            !cache.left_capsule[0].mesh.positions.is_empty(),
            "left capsule has vertices"
        );
        assert!(
            !cache.right_capsule[0].mesh.positions.is_empty(),
            "right capsule has vertices"
        );
        assert_eq!(cache.left_capsule[0].mesh.prim_path, "__xr_left_ctrl__/0");
        assert_eq!(cache.right_capsule[0].mesh.prim_path, "__xr_right_ctrl__/0");
        assert!(!cache.geometry_sourced);
        assert!(!cache.materials_ready);
    }

    #[test]
    fn none_world_matrix_returns_empty_instances() {
        let cache = ControllerMeshCache::new();
        assert!(cache.left_instances(None).is_empty());
        assert!(cache.right_instances(None).is_empty());
    }

    #[test]
    fn some_world_matrix_returns_correct_instance() {
        let cache = ControllerMeshCache::new();
        let mat = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let insts = cache.left_instances(Some(mat));
        assert_eq!(insts.len(), 1, "capsule is one part");
        assert_eq!(insts[0].prim_path, "__xr_left_ctrl__/0");
        assert_eq!(insts[0].world_matrix, mat);
        assert_eq!(insts[0].purpose, RenderPurpose::Default);
    }

    #[test]
    fn real_mesh_preferred_over_capsule() {
        let mut cache = ControllerMeshCache::new();
        let real_mesh = {
            let mut m = idtx_render::mesh::tessellate_capsule(0.02, 0.06, 16, 4);
            m.prim_path = "__xr_left_ctrl__/0".to_owned();
            Arc::new(m)
        };
        let real_part = ControllerMeshPart {
            mesh: Arc::clone(&real_mesh),
            material: fallback_material("__xr_left_ctrl__/0"),
            usd_material_path: None,
        };
        cache.left_real = Some(vec![real_part]);
        let insts = cache.left_instances(Some(Mat4::IDENTITY));
        assert_eq!(insts.len(), 1);
        assert!(
            Arc::ptr_eq(&insts[0].mesh, &real_mesh),
            "real mesh must win over the capsule"
        );
        let right = cache.right_instances(Some(Mat4::IDENTITY));
        assert!(Arc::ptr_eq(&right[0].mesh, &cache.right_capsule[0].mesh));
    }

    #[test]
    fn fill_materials_patches_parts_from_snapshot_map() {
        let mut cache = ControllerMeshCache::new();
        // Simulate a real controller with one USD-bound part
        let part_key = "__xr_left_ctrl__/0";
        let usd_mat_path = "/XR/Controllers/Left/Model/Looks/Shell_Mat";
        let mut mesh = idtx_render::mesh::tessellate_capsule(0.02, 0.06, 16, 4);
        mesh.prim_path = part_key.to_owned();
        cache.left_real = Some(vec![ControllerMeshPart {
            mesh: Arc::new(mesh),
            material: fallback_material(part_key),
            usd_material_path: Some(usd_mat_path.to_owned()),
        }]);

        // First call: snapshot doesn't have the material yet (stale snapshot)
        let empty_materials = HashMap::new();
        cache.fill_materials(&empty_materials, 1);
        assert!(
            !cache.materials_ready,
            "should not commit with missing materials"
        );
        // Fallback material path should still be the pseudo-path
        assert!(
            !cache.left_real.as_ref().unwrap()[0]
                .material
                .material_path
                .starts_with('/')
        );

        // Second call: snapshot updated (serial=2), now has the real material
        let real_mat = idtx_render::material::fallback_material(usd_mat_path); // any RenderMaterialDesc
        let mut real_materials = HashMap::new();
        real_materials.insert(usd_mat_path.to_owned(), real_mat.clone());
        cache.fill_materials(&real_materials, 2);
        assert!(
            cache.materials_ready,
            "should commit once materials are resolved"
        );
        // The part now uses the real material descriptor
        assert_eq!(
            cache.left_real.as_ref().unwrap()[0].material.material_path,
            real_mat.material_path,
        );

        // Third call: serial stays at 2 → no-op
        let mut other_materials = HashMap::new();
        other_materials.insert(usd_mat_path.to_owned(), fallback_material("different"));
        cache.fill_materials(&other_materials, 2);
        // material_path unchanged because same serial (no-op)
        assert_eq!(
            cache.left_real.as_ref().unwrap()[0].material.material_path,
            real_mat.material_path,
        );
    }

    #[test]
    fn apply_controller_instances_is_idempotent() {
        let cache = ControllerMeshCache::new();
        let mut out: Vec<RenderMeshInstance> = Vec::new();
        let pose = Some(Mat4::IDENTITY);

        apply_controller_instances(&cache, pose, pose, true, true, &mut out);
        assert_eq!(out.len(), 2, "one instance per hand");

        apply_controller_instances(&cache, pose, pose, true, true, &mut out);
        assert_eq!(out.len(), 2, "no accumulation across frames");

        apply_controller_instances(&cache, None, pose, false, true, &mut out);
        assert_eq!(out.len(), 1, "left hidden+untracked, right shown");
        assert_eq!(out[0].prim_path, "__xr_right_ctrl__/0");
    }

    #[test]
    fn apply_controller_instances_strips_multi_part_real_mesh() {
        let mut cache = ControllerMeshCache::new();
        let make_part = |key: &str| {
            let mut mesh = idtx_render::mesh::tessellate_capsule(0.01, 0.01, 8, 2);
            mesh.prim_path = key.to_owned();
            ControllerMeshPart {
                mesh: Arc::new(mesh),
                material: fallback_material(key),
                usd_material_path: None,
            }
        };
        cache.left_real = Some(vec![
            make_part("__xr_left_ctrl__/0"),
            make_part("__xr_left_ctrl__/1"),
        ]);
        let mut out: Vec<RenderMeshInstance> = Vec::new();
        let pose = Some(Mat4::IDENTITY);

        apply_controller_instances(&cache, pose, pose, true, true, &mut out);
        assert_eq!(out.len(), 3, "left=2 parts, right=1 capsule");

        apply_controller_instances(&cache, pose, pose, true, true, &mut out);
        assert_eq!(out.len(), 3, "no accumulation");
    }

    #[test]
    fn bake_into_transforms_points_and_offsets_indices() {
        let mut m = MergedMesh::default();
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let normals = [[0.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let uvs = [[0.0, 0.0], [1.0, 0.0]];
        let colors = [[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]];
        let indices = [0_u32, 1, 0];
        let rel = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));

        bake_into(&mut m, &positions, &normals, &uvs, &colors, &indices, rel);
        assert_eq!(m.positions[0], [10.0, 0.0, 0.0]);
        assert_eq!(m.positions[1], [11.0, 0.0, 0.0]);
        assert_eq!(m.indices, vec![0, 1, 0]);

        bake_into(&mut m, &positions, &normals, &uvs, &colors, &indices, rel);
        assert_eq!(m.indices, vec![0, 1, 0, 2, 3, 2]);
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.normals.len(), 4);
        assert_eq!(m.texcoords.len(), 4);
        assert_eq!(m.vertex_colors.len(), 4);
        for n in &m.normals {
            assert!((Vec3::from(*n).length() - 1.0).abs() < 1e-5, "normal {n:?}");
        }
    }

    #[test]
    fn bake_into_rotates_normals() {
        let mut m = MergedMesh::default();
        let rel = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
        bake_into(
            &mut m,
            &[[0.0, 0.0, 0.0]],
            &[[1.0, 0.0, 0.0]],
            &[[0.0, 0.0]],
            &[[1.0, 1.0, 1.0]],
            &[0],
            rel,
        );
        let n = Vec3::from(m.normals[0]);
        assert!((n - Vec3::Y).length() < 1e-5, "normal was {n:?}");
    }

    /// A flat two-triangle mesh in face-corner order (6 corner vertices, faces
    /// `[0, 1]`), used to exercise the GeomSubset split.
    fn two_face_mesh() -> GpuMeshDesc {
        GpuMeshDesc {
            prim_path: String::new(),
            local_aabb: Some(([0.0, 0.0, 0.0], [1.0, 1.0, 0.0])),
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 6],
            tangents: Vec::new(),
            texcoords: vec![[0.0, 0.0]; 6],
            vertex_colors: vec![[1.0, 1.0, 1.0]; 6],
            indices: vec![0, 1, 2, 3, 4, 5],
            double_sided: false,
            material_path: None,
            skin: None,
            serial: 0,
        }
    }

    #[test]
    fn mesh_material_parts_no_subsets_single_group() {
        let mesh = two_face_mesh();
        let parts = mesh_material_parts(&mesh, &[3, 3], Some("/World/Looks/Body"), &[]);
        assert_eq!(parts.len(), 1, "unsplit mesh is one group");
        assert_eq!(parts[0].0, "/World/Looks/Body");
        assert_eq!(parts[0].1.indices.len(), 6, "both faces present");
    }

    #[test]
    fn mesh_material_parts_splits_by_geom_subset() {
        // Two subsets, one face each — the layout that leaves the hand-tracked
        // controllers white when only `material_path` is read.
        let mesh = two_face_mesh();
        let subsets = vec![
            MaterialSubsetRecord {
                material_path: Some("/Ctrl/Looks/Shell".to_owned()),
                face_indices: vec![0],
            },
            MaterialSubsetRecord {
                material_path: Some("/Ctrl/Looks/Battery".to_owned()),
                face_indices: vec![1],
            },
        ];
        let parts = mesh_material_parts(&mesh, &[3, 3], None, &subsets);
        assert_eq!(parts.len(), 2, "one group per subset");
        let keys: Vec<&str> = parts.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"/Ctrl/Looks/Shell"), "keys: {keys:?}");
        assert!(keys.contains(&"/Ctrl/Looks/Battery"), "keys: {keys:?}");
        for (_, sub) in &parts {
            assert_eq!(sub.indices.len(), 3, "each subset covers one triangle");
        }
    }

    #[test]
    fn mesh_material_parts_remainder_uses_mesh_material() {
        // Only face 0 is bound by a subset; face 1 falls back to the whole-mesh
        // material path.
        let mesh = two_face_mesh();
        let subsets = vec![MaterialSubsetRecord {
            material_path: Some("/Ctrl/Looks/Shell".to_owned()),
            face_indices: vec![0],
        }];
        let parts = mesh_material_parts(&mesh, &[3, 3], Some("/Ctrl/Looks/Rest"), &subsets);
        assert_eq!(parts.len(), 2, "subset group + remainder group");
        let keys: Vec<&str> = parts.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"/Ctrl/Looks/Shell"), "keys: {keys:?}");
        assert!(
            keys.contains(&"/Ctrl/Looks/Rest"),
            "remainder keyed by mesh material: {keys:?}"
        );
    }
}
