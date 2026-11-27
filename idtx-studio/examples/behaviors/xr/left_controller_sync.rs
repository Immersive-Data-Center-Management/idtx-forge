/// Built-in left-controller sync behavior.
///
/// Attached to `/World/Player/Controllers/Left` — a real hydrated USD prim
/// (the skinned Quest controller model), not a synthetic bridge entity. It
/// reads the left-controller grip pose (`XrSpace::LeftHand`, already in stage
/// space) and authors it as the prim's local transform through the
/// `stage_edit` (session-layer) seam.
///
/// # Why `stage_edit`, not `entity().set_xform()`
///
/// Unlike `hmd_camera_sync` (which drives the preview *camera* entity — a
/// synthetic entity carrying `idtx_behavior::ecs::Transform` and swept into the
/// view matrix by the XR preview), a hydrated controller prim carries only the
/// scene projection components (`PrimNode` / `WorldTransform`) and never a
/// bridge `Transform`. `entity().set_xform()` would fail with `NotFound`, and
/// nothing sweeps a behavior-owned real prim into the render world anyway.
/// Authoring through the session layer re-projects the skinned mesh (the exact
/// path the session's hydrated-pose writer uses), so the model follows.
use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct LeftControllerSync;

#[behavior(kind = "runtime")]
impl LeftControllerSync {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if let Some(pose) = ctx.xr().get_pose(XrSpace::LeftHand) {
            ctx.stage_edit().set_self_transform(&pose)?;
        }
        Ok(())
    }
}
