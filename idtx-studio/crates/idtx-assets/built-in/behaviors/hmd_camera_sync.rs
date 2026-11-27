// hmd_camera_sync — built-in HMD → camera pose sync for the XR player.
//
// Part of the built-in player's default gameplay set. Attached to
// /World/Player/Camera (a real hydrated prim). Copies the current HMD pose into
// the camera prim's local transform through the stage_edit (session-layer) seam
// so the camera tracks the headset. A missing pose this frame leaves the camera
// where it was. Capabilities: `xr_hmd` + `stage_edit`.
//
// Loaded as a normal Rust module by the generated build skeleton.

use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct HmdCameraSync;

#[behavior(kind = "runtime")]
impl HmdCameraSync {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if let Some(pose) = ctx.xr().get_pose(XrSpace::Hmd) {
            ctx.stage_edit().set_self_transform(&pose)?;
        }
        Ok(())
    }
}
