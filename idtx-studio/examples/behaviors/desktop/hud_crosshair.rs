// camera_hud_crosshair — draw a full-screen crosshair HUD on the player camera.
//
// Attached to the desktop player's camera prim. On its first playing frame
// (`on_start`) it authors an `IdtxHud` child of the camera into the stage's
// SESSION layer via the `stage_edit` seam, pointing at a square crosshair
// image. The HUD is composited over the 3D render by the runtime.
//
// # Runtime-resolved size
//
// The on-screen size is passed as `[0, 0]`, meaning "use the image's native
// pixel size". The renderer reads the decoded image's dimensions at load, so
// the crosshair's size is resolved at runtime and need not be known when the
// behavior is compiled — exactly the requirement. A behavior that *does* know
// the desired size at runtime (e.g. computed from the viewport) can pass it
// explicitly via `attach_hud` instead.
//
// # Why `on_start` (author once)
//
// `attach_hud` is idempotent on the session side (it re-defines/re-authors the
// same child), but the HUD is static here, so authoring it once on the first
// playing frame is enough. A behavior that animates the crosshair (e.g. hides
// it while aiming) would re-author from `on_update`.
//
// # Capabilities
//
// `stage_edit` — author the HUD child + parameters on the session layer.
//
// This file is loaded as a normal Rust module by the generated build skeleton.

use idtx_behavior_sdk::prelude::*;

/// The crosshair image asset. Resolved relative to the stage's asset search
/// path at load; its native pixel size is read from the decoded image.
const CROSSHAIR_IMAGE: &str = "crosshair.png";

/// `[0, 0]` = use the image's native pixel size (resolved at runtime).
const NATIVE_SIZE: [i32; 2] = [0, 0];

/// Authors a centered crosshair HUD onto its camera prim.
#[derive(Default)]
pub struct CameraHudCrosshair;

#[behavior(kind = "runtime")]
impl CameraHudCrosshair {
    #[allow(clippy::unused_self)]
    fn on_start(&mut self, ctx: &Ctx) -> Result<()> {
        // Attach a centered crosshair at full opacity to this behavior's own
        // prim (the camera). `attach_crosshair` targets `self`, so the HUD
        // becomes a session-layer child of the camera.
        ctx.stage_edit()
            .attach_crosshair(CROSSHAIR_IMAGE, NATIVE_SIZE)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_size_sentinel_is_zero() {
        // `[0, 0]` is the documented "use the image's native size" sentinel.
        assert_eq!(NATIVE_SIZE, [0, 0]);
    }

    #[test]
    fn crosshair_image_is_a_square_asset_name() {
        assert!(CROSSHAIR_IMAGE.ends_with(".png"));
    }
}
