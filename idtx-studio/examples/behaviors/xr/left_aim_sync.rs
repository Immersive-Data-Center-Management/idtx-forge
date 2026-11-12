/// Built-in left-aim (pointer) sync behavior.
///
/// Mirrors `left_controller_sync` but reads `XrSpace::LeftAim` — the pointer
/// ray origin+orientation, distinct from the grip pose. Attached to the real
/// hydrated prim `/World/Player/Pointers/Left`; it authors the left-controller
/// aim pose as that prim's local transform each frame through the `stage_edit`
/// (session-layer) seam, so the pointer ray tracks where the device is pointing
/// rather than where it is held.
///
/// See `left_controller_sync` for why this authors through `stage_edit` rather
/// than `entity().set_xform()` (a hydrated prim has no bridge `Transform`).
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
