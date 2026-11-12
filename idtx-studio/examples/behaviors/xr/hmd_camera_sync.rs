/// Built-in HMD → camera sync behavior.
///
/// Authored on `over "Camera"` of the XR player rig (a real hydrated USD
/// prim, `/World/Player/Camera`). Every `on_update` it reads the most
/// recently polled HMD pose (through the `xr_hmd` capability) and authors
/// it as the camera prim's local transform through the `stage_edit`
/// (session-layer) seam, so the camera tracks the headset.
///
/// # Why `stage_edit`, not `entity().set_xform()`
///
/// Like the controller/aim sync behaviors, this drives a *hydrated* USD
/// prim which carries only the scene projection components (`PrimNode` /
/// `WorldTransform`) and never a bridge `Transform`. `entity().set_xform()`
/// would fail with `NotFound`. Authoring through the session layer
/// re-projects the camera prim's `WorldTransform` — the exact path the
/// session's hydrated-pose writer uses — and the render camera resolves
/// from that prim (`RenderCameraSource::ScenePath`).
///
/// # Capabilities
///
/// Requires `xr_hmd` (read the pose) and `stage_edit` (author the camera
/// prim). The manifest (`idtx.toml`) grants exactly those two.
///
/// # Compilation
///
/// This is a *single-file* behavior compiled to `wasm32-unknown-unknown`
/// by `idtx-behavior-build` / `cargo xtask build-behavior`. The emitted
/// `hmd_camera_sync.wasm` is what the runtime registers with the host.
/// Author logic is unit-testable natively: the SDK's native import stubs
/// make `ctx.xr().get_pose(...)` return `None`, so `on_update` is a no-op
/// off-device.

use idtx_behavior_sdk::prelude::*;

/// Zero-sized behavior state — the camera pose lives entirely in the ECS
/// transform this behavior drives, so the behavior itself holds nothing.
#[derive(Default)]
pub struct HmdCameraSync;

#[behavior(kind = "runtime")]
impl HmdCameraSync {
    /// Copy the current HMD pose into the attached camera prim's local
    /// transform. A missing pose (device not tracking this frame) leaves
    /// the camera where it was rather than snapping it to the origin.
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        if let Some(pose) = ctx.xr().get_pose(XrSpace::Hmd) {
            ctx.stage_edit().set_self_transform(&pose)?;
        }
        Ok(())
    }
}
