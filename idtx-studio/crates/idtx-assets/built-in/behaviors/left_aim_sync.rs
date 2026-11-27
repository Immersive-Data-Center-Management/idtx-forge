// left_aim_sync — built-in left aim (pointer) pose sync for the XR player.
//
// Part of the built-in player's default gameplay set. Mirrors
// left_controller_sync but reads XrSpace::LeftAim — the pointer ray
// origin+orientation, distinct from the grip pose. Attached to the real
// hydrated prim /World/Player/Pointers/Left; authors the aim pose as that
// prim's local transform through the stage_edit (session-layer) seam so the
// pointer ray tracks where the device points. Capabilities: `xr_hmd` + `stage_edit`.
//
// Loaded as a normal Rust module by the generated build skeleton.

use idtx_behavior_sdk::prelude::*;

#[derive(Default)]
pub struct LeftAimSync;

#[behavior(kind = "runtime")]
impl LeftAimSync {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if let Some(pose) = ctx.xr().get_pose(XrSpace::LeftAim) {
            ctx.stage_edit().set_self_transform(&pose)?;
        }
        Ok(())
    }
}
