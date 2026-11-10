//! Synthetic preview camera that frames all scene geometry.
//!
//! When the USD stage has no camera prim, [`fit_camera_to_snapshot`] computes
//! an orbit camera positioned to see the full scene extent.

use glam::{Mat4, Vec3, Vec4};

use crate::scene_extract::{RenderCameraDesc, RenderWorldSnapshot};

/// The 8 corners of an axis-aligned box `(lo, hi)`.
#[inline]
fn aabb_corners(lo: Vec3, hi: Vec3) -> [Vec3; 8] {
    [
        Vec3::new(lo.x, lo.y, lo.z),
        Vec3::new(hi.x, lo.y, lo.z),
        Vec3::new(lo.x, hi.y, lo.z),
        Vec3::new(hi.x, hi.y, lo.z),
        Vec3::new(lo.x, lo.y, hi.z),
        Vec3::new(hi.x, lo.y, hi.z),
        Vec3::new(lo.x, hi.y, hi.z),
        Vec3::new(hi.x, hi.y, hi.z),
    ]
}

/// Computes the world-space axis-aligned bounding box of all geometry in the
/// snapshot by transforming each mesh's cached local AABB (8 corners) under its
/// world matrix, plus every instanced-batch instance (prototype AABB corners
/// under each instance transform). `O(meshes + instances)`, not `O(vertices)` —
/// the result is a conservative superset under rotation (a transformed box can
/// exceed the transformed hull), which is exactly correct for shadow-frustum and
/// preview-camera fitting. GPU-skinned meshes contribute their rest-pose bounds
/// (skinning is GPU-side); this is intentional — see [`scene_aabb_signature`].
///
/// Returns `None` when the snapshot has no geometry at all.
#[must_use]
pub fn compute_scene_aabb(
    snapshot: &RenderWorldSnapshot,
    casters_only: bool,
) -> Option<(Vec3, Vec3)> {
    let mut aabb_min = Vec3::splat(f32::MAX);
    let mut aabb_max = Vec3::splat(f32::MIN);
    let mut found = false;

    for inst in &snapshot.meshes {
        // `casters_only` (shadow fit): skip `idtx:castsShadows = 0` prims so a
        // visible-but-non-casting gizmo / pointer ray cannot extend the shadow
        // frustum. The preview-camera fit passes `false` to frame all geometry.
        if casters_only && !inst.casts_shadow {
            continue;
        }
        let Some((lo, hi)) = inst.mesh.local_aabb else {
            continue;
        };
        let m = inst.world_matrix;
        for c in aabb_corners(Vec3::from_array(lo), Vec3::from_array(hi)) {
            let wp = m.mul_vec4(Vec4::new(c.x, c.y, c.z, 1.0));
            let v = Vec3::new(wp.x / wp.w, wp.y / wp.w, wp.z / wp.w);
            aabb_min = aabb_min.min(v);
            aabb_max = aabb_max.max(v);
            found = true;
        }
    }

    // Hardware-instanced / native-instanced batches are not in `meshes`. Compute
    // the prototype's local AABB once, then transform its 8 corners per instance —
    // O(instances), not O(instances × vertices). Without this a scene whose
    // instanced casters extend beyond the static geometry would fit the shadow
    // frustum to the static bounds and clip the instanced casters' shadows.
    for batch in &snapshot.instanced_batches {
        if casters_only && !batch.casts_shadow {
            continue;
        }
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        let mut proto_found = false;
        for &p in &batch.mesh.positions {
            let v = Vec3::new(p[0], p[1], p[2]);
            lo = lo.min(v);
            hi = hi.max(v);
            proto_found = true;
        }
        if !proto_found {
            continue;
        }
        let corners = aabb_corners(lo, hi);
        for m in &batch.instance_transforms {
            for &c in &corners {
                let wp = m.mul_vec4(Vec4::new(c.x, c.y, c.z, 1.0));
                let v = Vec3::new(wp.x / wp.w, wp.y / wp.w, wp.z / wp.w);
                aabb_min = aabb_min.min(v);
                aabb_max = aabb_max.max(v);
                found = true;
            }
        }
    }

    found.then_some((aabb_min, aabb_max))
}

/// Whether the local AABB `local` transformed by `m` stays fully inside `bounds`
/// (inclusive). Used by the shadow-refit fast-path: a caster whose transformed
/// bounds remain inside the cached scene AABB cannot enlarge it, so the (now
/// O(meshes) but still non-free) recompute can be skipped entirely.
#[must_use]
pub fn transformed_aabb_within(local: ([f32; 3], [f32; 3]), m: Mat4, bounds: (Vec3, Vec3)) -> bool {
    let (bmin, bmax) = bounds;
    for c in aabb_corners(Vec3::from_array(local.0), Vec3::from_array(local.1)) {
        let wp = m.mul_vec4(Vec4::new(c.x, c.y, c.z, 1.0));
        let v = Vec3::new(wp.x / wp.w, wp.y / wp.w, wp.z / wp.w);
        if v.x < bmin.x
            || v.y < bmin.y
            || v.z < bmin.z
            || v.x > bmax.x
            || v.y > bmax.y
            || v.z > bmax.z
        {
            return false;
        }
    }
    true
}

/// Scene AABB for the synthetic *preview camera*: [`compute_scene_aabb`]'s
/// geometry bounds plus every widget panel's transformed quad corners.
///
/// Widget panels are world-space geometry but are not in `meshes`/instanced
/// batches, so without this a widget-only stage (or a panel outside the mesh
/// bounds) would be framed with the mesh-only / unit-cube camera and sit
/// off-screen. This is deliberately *separate* from [`compute_scene_aabb`]:
/// panels neither cast nor receive shadows, so the shadow-fit path must keep
/// using the geometry-only bounds and not waste shadow-map resolution on them.
#[must_use]
pub fn compute_camera_aabb(snapshot: &RenderWorldSnapshot) -> Option<(Vec3, Vec3)> {
    let geom = compute_scene_aabb(snapshot, false);
    if snapshot.widget_panels.is_empty() {
        return geom;
    }
    let (mut lo, mut hi) = geom.unwrap_or((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)));
    let mut found = geom.is_some();
    for panel in &snapshot.widget_panels {
        let m = panel.world_matrix;
        let center = m.w_axis.truncate();
        // Authored (non-billboard) quad: center ± right ± up with the full world
        // transform applied to the half-extent edge vectors (matches the renderer's
        // `WidgetBillboard::None` resolution).
        let r = m.transform_vector3(Vec3::X) * (panel.size[0] * 0.5);
        let u = m.transform_vector3(Vec3::Y) * (panel.size[1] * 0.5);
        for sx in [-1.0_f32, 1.0] {
            for sy in [-1.0_f32, 1.0] {
                let v = center + r * sx + u * sy;
                lo = lo.min(v);
                hi = hi.max(v);
                found = true;
            }
        }
    }
    found.then_some((lo, hi))
}

/// Cheap FNV-1a change-signal for the inputs of [`compute_scene_aabb`]: the mesh
/// count plus, for each mesh, its geometry serial and world matrix. `O(meshes)`,
/// not `O(vertices)`, so it is cheap to recompute every frame as a cache key.
///
/// It must change on all three domains the AABB depends on:
/// - **TOPOLOGY** — the mesh count (and, via the per-mesh mix order, membership).
/// - **GEOMETRY** — `mesh.serial`, bumped by `extract()` when vertex data changes.
/// - **TRANSFORM** — `world_matrix`, including in-place `patch_world_matrices`
///   transform patches that deliberately do *not* bump `snapshot.serial`
///   (see `renderer::merge_group_xform_sig`, whose shape this mirrors). Keying an
///   AABB cache on `snapshot.serial` alone would therefore frame stale geometry
///   after a rigid transform-only tick; this signature does not.
///
/// GPU-skinned deformation is intentionally *not* captured: `compute_scene_aabb`
/// reads rest-pose positions (skinning is GPU-side), so joint-matrix patches do
/// not move the AABB inputs and must not invalidate the cache.
#[must_use]
pub fn scene_aabb_signature(snapshot: &RenderWorldSnapshot) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bits: u64| {
        h ^= bits;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(snapshot.meshes.len() as u64);
    for inst in &snapshot.meshes {
        mix(inst.mesh.serial);
        for f in inst.world_matrix.to_cols_array() {
            mix(u64::from(f.to_bits()));
        }
    }
    // Instanced batches contribute to the AABB, so their geometry serial and each
    // instance transform must feed the change-signal too.
    mix(snapshot.instanced_batches.len() as u64);
    for batch in &snapshot.instanced_batches {
        mix(batch.mesh.serial);
        mix(batch.instance_transforms.len() as u64);
        for m in &batch.instance_transforms {
            for f in m.to_cols_array() {
                mix(u64::from(f.to_bits()));
            }
        }
    }
    // Widget panels are folded into the preview-camera AABB (`compute_camera_aabb`),
    // so their transform + size must feed this camera-cache change-signal too.
    // (This signature keys only the synthetic-camera AABB cache, not the shadow
    // fit, so including panels here does not disturb shadow bounds.)
    mix(snapshot.widget_panels.len() as u64);
    for panel in &snapshot.widget_panels {
        for f in panel.world_matrix.to_cols_array() {
            mix(u64::from(f.to_bits()));
        }
        mix(u64::from(panel.size[0].to_bits()));
        mix(u64::from(panel.size[1].to_bits()));
    }
    h
}

/// Builds a synthetic right-handed perspective camera that frames the entire
/// scene.  The camera is positioned at a 3/4 orbit (45° elevation, 45°
/// azimuth) around the scene's bounding-box centre.
///
/// If the snapshot has no geometry the camera defaults to a unit-cube view.
#[must_use]
pub fn fit_camera_to_snapshot(
    snapshot: &RenderWorldSnapshot,
    viewport_aspect: f32,
) -> RenderCameraDesc {
    //let (aabb_min, aabb_max) = compute_scene_aabb(snapshot, false)
    let (aabb_min, aabb_max) = compute_camera_aabb(snapshot)
        .unwrap_or((Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0)));
    fit_camera_to_aabb(aabb_min, aabb_max, viewport_aspect)
}

/// Builds the synthetic 3/4-orbit camera from an already-computed world-space
/// AABB. Split from [`fit_camera_to_snapshot`] so a caller that caches the AABB
/// (the expensive per-vertex pass) can still rebuild the cheap camera projection
/// every frame — e.g. to honour a window-resize aspect change without redoing the
/// AABB. The camera geometry (elevation/azimuth/distance) is unchanged.
#[must_use]
pub fn fit_camera_to_aabb(
    aabb_min: Vec3,
    aabb_max: Vec3,
    viewport_aspect: f32,
) -> RenderCameraDesc {
    let center = (aabb_min + aabb_max) * 0.5;
    let diagonal = (aabb_max - aabb_min).length().max(0.001);

    // 3/4 orbit: 45° elevation, 45° azimuth (top-right), matching the editor's
    // `OrbitCamera` synthetic framing.
    let elevation = 45_f32.to_radians();
    let azimuth = 45_f32.to_radians();
    let distance = diagonal * 1.8;

    let eye = center
        + Vec3::new(
            distance * azimuth.cos() * elevation.cos(),
            distance * elevation.sin(),
            distance * azimuth.sin() * elevation.cos(),
        );

    let view = glam::camera::rh::view::look_at_mat4(eye, center, Vec3::Y);

    let fov_v = 45_f32.to_radians();
    // Keep near plane away from geometry to avoid clipping at the front face.
    let near = (distance - diagonal * 0.6).max(diagonal * 0.001).max(0.01);
    let far = distance + diagonal * 2.5;
    let projection = glam::camera::rh::proj::directx::perspective(fov_v, viewport_aspect, near, far);
    let fov_h = 2.0 * ((fov_v * 0.5).tan() * viewport_aspect).atan();

    RenderCameraDesc {
        prim_path: "__synthetic__".to_string(),
        view,
        projection,
        clip_near: near,
        clip_far: far,
        fov_horizontal: fov_h,
        exposure_ev: 0.0,
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact-value assertions on known constants
mod tests {
    use super::*;
    use crate::mesh::GpuMeshDesc;
    use crate::scene_extract::{RenderMeshInstance, RenderWorldSnapshot};
    use idtx_usd::RenderPurpose;
    use std::sync::Arc;

    #[test]
    fn empty_snapshot_returns_unit_cube_camera() {
        let snap = RenderWorldSnapshot::default();
        let cam = fit_camera_to_snapshot(&snap, 16.0 / 9.0);
        assert!(!cam.prim_path.is_empty());
        assert!(cam.clip_near > 0.0);
        assert!(cam.clip_far > cam.clip_near);
    }

    /// Minimal single-triangle mesh with a controllable geometry `serial`.
    fn triangle(serial: u64) -> GpuMeshDesc {
        GpuMeshDesc {
            prim_path: "/tri".to_string(),
            material_path: None,
            local_aabb: Some(([0.0, 0.0, 0.0], [1.0, 1.0, 0.0])),
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.5, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            texcoords: vec![[0.0, 0.0]; 3],
            vertex_colors: vec![],
            tangents: vec![],
            indices: vec![0, 1, 2],
            double_sided: false,
            skin: None,
            serial,
        }
    }

    fn snapshot_with(mesh: GpuMeshDesc, world: glam::Mat4) -> RenderWorldSnapshot {
        let mut snap = RenderWorldSnapshot::default();
        snap.meshes.push(RenderMeshInstance {
            prim_path: "/tri".to_string(),
            mesh: Arc::new(mesh),
            world_matrix: world,
            material: crate::material::fallback_material("/tri"),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        });
        snap
    }

    #[test]
    fn casters_only_excludes_non_shadow_casting_meshes() {
        // A caster at the origin + a non-casting mesh translated far along +X.
        // `casters_only = true` (shadow fit) must exclude the non-caster so it
        // cannot blow up the shadow frustum; `false` (camera fit) includes it.
        let mut snap = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        snap.meshes.push(RenderMeshInstance {
            prim_path: "/ray".to_string(),
            mesh: Arc::new(triangle(2)),
            world_matrix: glam::Mat4::from_translation(glam::Vec3::new(100.0, 0.0, 0.0)),
            material: crate::material::fallback_material("/ray"),
            purpose: RenderPurpose::Default,
            casts_shadow: false,
        });
        let all = compute_scene_aabb(&snap, false).expect("geometry");
        let casters = compute_scene_aabb(&snap, true).expect("geometry");
        assert!(all.1.x > 50.0, "camera fit includes the far non-caster");
        assert!(
            casters.1.x < 2.0,
            "shadow fit (casters_only) excludes the non-caster: {casters:?}"
        );
    }

    #[test]
    fn signature_is_stable_when_nothing_changes() {
        let a = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        let b = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        assert_eq!(
            scene_aabb_signature(&a),
            scene_aabb_signature(&b),
            "identical scenes must hash identically (cache hit → no recompute)"
        );
    }

    #[test]
    fn transformed_aabb_within_detects_in_and_out_of_bounds() {
        let bounds = (Vec3::splat(-10.0), Vec3::splat(10.0));
        let local = ([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        // A small translation keeps all 8 corners inside → within.
        assert!(transformed_aabb_within(
            local,
            glam::Mat4::from_translation(Vec3::new(2.0, 2.0, 2.0)),
            bounds,
        ));
        // A large translation pushes corners past +10 → not within.
        assert!(!transformed_aabb_within(
            local,
            glam::Mat4::from_translation(Vec3::new(20.0, 0.0, 0.0)),
            bounds,
        ));
    }

    #[test]
    fn compute_scene_aabb_uses_local_aabb_corners() {
        // With a cached local AABB the result is the 8-corner transform (O(meshes));
        // an axis-aligned translate is exact.
        let snap = snapshot_with(
            triangle(1),
            glam::Mat4::from_translation(glam::Vec3::new(5.0, 0.0, 0.0)),
        );
        let (lo, hi) = compute_scene_aabb(&snap, false).expect("has geometry");
        // triangle local_aabb = ([0,0,0],[1,1,0]) → translated +5 on x.
        assert!((lo.x - 5.0).abs() < 1e-5 && (hi.x - 6.0).abs() < 1e-5);
        assert!((lo.y - 0.0).abs() < 1e-5 && (hi.y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn signature_changes_on_transform_only_edit() {
        // This is the case a pure `snapshot.serial` gate would miss:
        // `patch_world_matrices` mutates `world_matrix` in place without bumping
        // serial, so the signature — not serial — is what keeps the AABB fresh.
        let base = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        let moved = snapshot_with(
            triangle(1),
            glam::Mat4::from_translation(glam::Vec3::new(10.0, 0.0, 0.0)),
        );
        assert_ne!(
            scene_aabb_signature(&base),
            scene_aabb_signature(&moved),
            "a world-matrix change must invalidate the cached AABB"
        );
    }

    #[test]
    fn signature_changes_on_geometry_serial() {
        let a = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        let b = snapshot_with(triangle(2), glam::Mat4::IDENTITY);
        assert_ne!(
            scene_aabb_signature(&a),
            scene_aabb_signature(&b),
            "a geometry serial bump must invalidate the cached AABB"
        );
    }

    #[test]
    fn signature_changes_on_topology_count() {
        let one = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        let mut two = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        two.meshes.push(two.meshes[0].clone());
        assert_ne!(
            scene_aabb_signature(&one),
            scene_aabb_signature(&two),
            "adding/removing a mesh (topology) must invalidate the cached AABB"
        );
    }

    #[test]
    fn cached_aabb_reproduces_uncached_camera() {
        // The cache path (fit_camera_to_aabb over a stored AABB) must produce the
        // identical camera the uncached path (fit_camera_to_snapshot) would, so a
        // cache hit is behaviourally invisible.
        let snap = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        let (min, max) = compute_scene_aabb(&snap, false).unwrap();
        let cached = fit_camera_to_aabb(min, max, 16.0 / 9.0);
        let direct = fit_camera_to_snapshot(&snap, 16.0 / 9.0);
        assert_eq!(cached.view, direct.view);
        assert_eq!(cached.projection, direct.projection);
        assert_eq!(cached.clip_near, direct.clip_near);
        assert_eq!(cached.clip_far, direct.clip_far);
    }

    fn instanced_batch(
        serial: u64,
        transforms: Vec<glam::Mat4>,
    ) -> crate::scene_extract::RenderInstancedBatch {
        crate::scene_extract::RenderInstancedBatch {
            prototype_key: "proto".to_string(),
            mesh: Arc::new(triangle(serial)),
            instance_transforms: transforms,
            instance_prim_paths: vec![],
            material: crate::material::fallback_material("/tri"),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        }
    }

    #[test]
    fn instanced_batch_extends_aabb_beyond_static_meshes() {
        // A lone static triangle sits at the origin; an instanced batch is stamped
        // 100 units away. The AABB must grow to enclose the far instance — else the
        // shadow frustum, fit to the static bounds, would clip the instanced caster.
        let mut snap = snapshot_with(triangle(1), glam::Mat4::IDENTITY);
        let (_, max_static) = compute_scene_aabb(&snap, false).unwrap();
        assert!(
            max_static.x < 50.0,
            "sanity: static geometry is near the origin"
        );

        snap.instanced_batches.push(instanced_batch(
            1,
            vec![glam::Mat4::from_translation(glam::Vec3::new(
                100.0, 0.0, 0.0,
            ))],
        ));
        let (_, max_all) = compute_scene_aabb(&snap, false).unwrap();
        assert!(
            max_all.x >= 100.0,
            "instanced batch at x=100 must extend the AABB (got max.x={})",
            max_all.x
        );
    }

    #[test]
    fn signature_changes_on_instance_transform_edit() {
        let mut a = RenderWorldSnapshot::default();
        a.instanced_batches
            .push(instanced_batch(1, vec![glam::Mat4::IDENTITY]));
        let mut b = RenderWorldSnapshot::default();
        b.instanced_batches.push(instanced_batch(
            1,
            vec![glam::Mat4::from_translation(glam::Vec3::new(3.0, 0.0, 0.0))],
        ));
        assert_ne!(
            scene_aabb_signature(&a),
            scene_aabb_signature(&b),
            "a per-instance transform change must invalidate the cached AABB"
        );
    }
}
