// right_controller_sync — built-in right grip-pose sync for the XR player.
//
// Part of the built-in player's default gameplay set. Attached to the real
// hydrated prim /World/Player/Controllers/Right. Reads the right grip pose
// (XrSpace::RightHand, already in stage space) and authors it as the prim's
// local transform through the stage_edit (session-layer) seam, so the
// controller model tracks the hand. Capabilities: `xr_hmd` + `stage_edit`.
//
// include!'d into the build skeleton → only `//` line comments.

use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct RightControllerSync;

#[behavior(kind = "runtime")]
impl RightControllerSync {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if let Some(pose) = ctx.xr().get_pose(XrSpace::RightHand) {
            ctx.stage_edit().set_self_transform(&pose)?;
        }
        Ok(())
    }
}
