// desktop_camera_pitch — built-in clamped pitch mouse-look for the desktop camera.
//
// Part of the built-in player's default gameplay set. Attached to
// /World/Player/Head/Camera via the built-in Desktop rig's IdtxBehaviorAPI
// binding. Reads the `look` 2D axis (mouse), integrates a pitch angle from the
// mouse-Y delta, clamps it to ±40°, and authors the camera's local rotation
// through the stage_edit (session-layer) seam.
//
// Camera half of the textbook FPS split: only pitch here; yaw + movement on the
// player (desktop_player_move). Capabilities: `input` + `stage_edit`.
//
// include!'d into the build skeleton → only `//` line comments.

use idtx_behavior_sdk::prelude::*;

/// Radians of pitch per pixel of mouse-Y delta.
const MOUSE_SENS: f32 = 0.0025;
/// Look up/down limit: 40° from straight-forward, in radians.
const MAX_PITCH_RAD: f32 = 40.0 * core::f32::consts::PI / 180.0;

/// Tilts its camera prim up/down from the mouse-Y delta, clamped to ±40°.
#[derive(Default)]
pub struct DesktopCameraPitch {
    pitch: f32,
}

#[behavior(kind = "runtime")]
impl DesktopCameraPitch {
    fn on_update(&mut self, ctx: &Ctx, _dt: f32) -> Result<()> {
        let (_, mouse_dy) = input::axis2("look");
        self.pitch = apply_pitch(self.pitch, mouse_dy, MOUSE_SENS, MAX_PITCH_RAD);
        ctx.stage_edit().set_self_transform(&Transform {
            translation: Vec3::ZERO,
            rotation: Quat::from_rotation_x(self.pitch),
            scale: Vec3::ONE,
        })?;
        Ok(())
    }
}

/// Integrate pitch from a mouse-Y delta and clamp to `±max`.
fn apply_pitch(pitch: f32, mouse_dy: f32, sens: f32, max: f32) -> f32 {
    (pitch - mouse_dy * sens).clamp(-max, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    const EPS: f32 = 1e-6;

    #[test]
    fn clamps_to_forty_degrees() {
        let up = apply_pitch(0.0, -100_000.0, MOUSE_SENS, MAX_PITCH_RAD);
        assert!((up - MAX_PITCH_RAD).abs() < EPS);
        let down = apply_pitch(0.0, 100_000.0, MOUSE_SENS, MAX_PITCH_RAD);
        assert!((down + MAX_PITCH_RAD).abs() < EPS);
    }

    #[test]
    fn forty_degrees_is_the_documented_limit() {
        assert!((MAX_PITCH_RAD.to_degrees() - 40.0).abs() < 1e-3);
    }
}
