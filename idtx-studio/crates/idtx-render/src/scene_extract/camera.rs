//! Renderer-facing camera descriptor with derived view / projection matrices.

use glam::Mat4;
use idtx_scene::CameraComp;

use crate::convert::AsF32;

/// Renderer-facing camera descriptor with derived view / projection matrices.
#[derive(Debug, Clone)]
pub struct RenderCameraDesc {
    /// Source USD prim path.
    pub prim_path: String,
    /// Column-major view matrix (inverse of the camera's world transform).
    pub view: Mat4,
    /// Column-major perspective projection matrix.
    pub projection: Mat4,
    /// Near clip plane distance (metres).
    pub clip_near: f32,
    /// Far clip plane distance (metres).
    pub clip_far: f32,
    /// Horizontal field of view in radians.
    pub fov_horizontal: f32,
    /// EV100 exposure value from physical camera parameters.
    /// `0.0` means "no physical camera authored" → linear exposure = 1.0.
    pub exposure_ev: f32,
}

impl RenderCameraDesc {
    /// Build a descriptor from a hydrated [`CameraComp`] and its world transform.
    ///
    /// `world_matrix` is the camera's [`idtx_scene::WorldTransform`] matrix *before* the
    /// `stage_basis` coordinate-system correction; the view matrix is
    /// `(stage_basis * world_matrix).inverse()`. Shared by the bulk extract
    /// ([`super::RenderWorldSnapshot::extract_with_shadow`]) and the per-frame runtime
    /// path that renders through a chosen scene camera, so the USD
    /// aperture→FOV conversion, clip-range fallback, and EV100 exposure model
    /// live in one place.
    #[must_use]
    pub fn from_scene_camera(
        prim_path: String,
        cam: &CameraComp,
        world_matrix: Mat4,
        stage_basis: Mat4,
        viewport_aspect: f32,
    ) -> Self {
        let rec = &cam.record;
        let view = (stage_basis * world_matrix).inverse();
        let near = rec.clipping[0].as_f32();
        let far = rec.clipping[1].as_f32();
        let (near, far) = if near > 0.0 && far > near {
            (near, far)
        } else {
            (0.1, 10_000.0)
        };
        // USD horizontal aperture / focalLength gives horizontal FOV; convert to vertical.
        let fov_h =
            2.0 * ((rec.horizontal_aperture.as_f32() / 2.0) / rec.focal_length.as_f32()).atan();
        let fov_v = 2.0 * ((fov_h * 0.5).tan() / viewport_aspect).atan();
        let projection = glam::camera::rh::proj::directx::perspective(fov_v, viewport_aspect, near, far);
        // Physical camera EV100 from CameraPhysics schema (non-zero f_stop + time → physical).
        let exposure_ev = if rec.exposure_f_stop > 0.0 && rec.exposure_time > 0.0 {
            let n = rec.exposure_f_stop.as_f32();
            let t = rec.exposure_time.as_f32();
            let iso = if rec.exposure_iso > 0.0 {
                rec.exposure_iso.as_f32()
            } else {
                100.0
            };
            (n * n * iso / (100.0 * t)).log2()
        } else if rec.exposure.abs() > f64::EPSILON {
            // Simple exposure bias in f-stops from UsdGeomCamera.exposure
            rec.exposure.as_f32()
        } else {
            0.0 // not authored → linear exposure 1.0 in shader
        };
        Self {
            prim_path,
            view,
            projection,
            clip_near: near,
            clip_far: far,
            fov_horizontal: fov_h,
            exposure_ev,
        }
    }
}
