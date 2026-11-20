// desktop_player_move — WASD + yaw mouse-look for a desktop player pawn.
//
// Attached to `/World/Player`. Every `on_update` it reads the logical input
// actions (`move_forward` / `move_right` scalar axes, `look` 2D axis) the
// stage's `input_map.toml` binds to physical keys / the mouse, integrates a
// yaw angle from the mouse X delta, moves the pawn along its yaw-relative
// forward / right vectors, and authors the result through the `stage_edit`
// (session-layer) seam so the change composes down the hierarchy (the child
// camera follows) and re-projects to the renderer.
//
// This is the *player* half of the textbook FPS split: yaw + translation live
// here; pitch lives on the camera (`desktop_camera_pitch`) so the two never
// fight over the same transform.
//
// # State
//
// Cross-frame yaw and position live on the behavior instance (`self.yaw` /
// `self.pos`): the host constructs the behavior once per attachment and reuses
// it across every hook, so plain `&mut self` fields accumulate frame to frame.
//
// # Capabilities
//
// `input` (read the actions) + `stage_edit` (author the pawn transform). The
// manifest grants exactly those two.
//
// This file is `include!`d into the build skeleton, so it uses only `//` line
// comments (no inner `//!` attributes).

use idtx_behavior_sdk::prelude::*;

// --- Tunables --------------------------------------------------------------
// Sign / magnitude are intentionally simple; the input map's `scale` /
// `invert` modifiers can retune or flip an axis without a rebuild.

/// Radians of yaw per pixel of mouse-X delta.
const MOUSE_SENS: f32 = 0.0025;
/// Metres per second at full stick / key deflection.
const MOVE_SPEED: f32 = 3.0;

/// Rotating its prim around Y and translating it along the yaw-relative
/// ground plane in response to WASD + mouse-look.
#[derive(Default)]
pub struct DesktopPlayerMove {
    /// Accumulated yaw in radians.
    yaw: f32,
    /// Accumulated world position. Starts at the origin: the guest withholds the
    /// scene-read capability (the textbook split — see the crate docs' caveat),
    /// so it cannot recover an authored rig offset. The template rig seats the
    /// pawn at the origin to match; a rig that offsets the pawn would begin at
    /// the origin on the first tick instead.
    pos: Vec3,
}

#[behavior(kind = "runtime")]
impl DesktopPlayerMove {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        let (mouse_dx, _) = input::axis2("look");
        self.yaw = integrate_yaw(self.yaw, mouse_dx, MOUSE_SENS);

        let forward = input::axis("move_forward");
        let strafe = input::axis("move_right");
        self.pos += move_delta(self.yaw, forward, strafe, MOVE_SPEED, dt);

        ctx.stage_edit().set_self_transform(&Transform {
            translation: self.pos,
            rotation: Quat::from_rotation_y(self.yaw),
            scale: Vec3::ONE,
        })?;
        Ok(())
    }
}

// --- Pure math (unit-testable natively) ------------------------------------

/// Integrate a yaw angle from a mouse-X delta. Mouse-right (+dx) turns the
/// pawn clockwise (yaw decreases about +Y), matching the usual screen mapping.
fn integrate_yaw(yaw: f32, mouse_dx: f32, sens: f32) -> f32 {
    yaw - mouse_dx * sens
}

/// The per-frame world-space translation for the given yaw and stick inputs.
///
/// `forward`/`strafe` are `[-1, 1]` axis values; the pawn moves along its
/// yaw-relative `-Z` (forward) and `+X` (right) vectors.
fn move_delta(yaw: f32, forward: f32, strafe: f32, speed: f32, dt: f32) -> Vec3 {
    let rot = Quat::from_rotation_y(yaw);
    let fwd = rot * Vec3::NEG_Z;
    let right = rot * Vec3::X;
    (fwd * forward + right * strafe) * (speed * dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    #[test]
    fn yaw_decreases_with_rightward_mouse() {
        // +dx (mouse moved right) turns the pawn clockwise → yaw decreases.
        assert!(integrate_yaw(0.0, 10.0, 0.01) < 0.0);
        // -dx turns the other way.
        assert!(integrate_yaw(0.0, -10.0, 0.01) > 0.0);
        // Zero delta leaves yaw untouched.
        assert!((integrate_yaw(1.23, 0.0, 0.01) - 1.23).abs() < EPS);
    }

    #[test]
    fn forward_at_zero_yaw_is_negative_z() {
        let d = move_delta(0.0, 1.0, 0.0, 1.0, 1.0);
        assert!(d.x.abs() < EPS, "x = {}", d.x);
        assert!(d.y.abs() < EPS, "y = {}", d.y);
        assert!((d.z + 1.0).abs() < EPS, "z = {}", d.z);
    }

    #[test]
    fn yaw_90_rotates_forward_to_negative_x() {
        let d = move_delta(core::f32::consts::FRAC_PI_2, 1.0, 0.0, 1.0, 1.0);
        assert!((d.x + 1.0).abs() < 1e-4, "x = {}", d.x);
        assert!(d.z.abs() < 1e-4, "z = {}", d.z);
    }

    #[test]
    fn strafe_at_zero_yaw_is_positive_x() {
        let d = move_delta(0.0, 0.0, 1.0, 1.0, 1.0);
        assert!((d.x - 1.0).abs() < EPS, "x = {}", d.x);
        assert!(d.z.abs() < EPS, "z = {}", d.z);
    }

    #[test]
    fn speed_and_dt_scale_linearly() {
        let d = move_delta(0.0, 1.0, 0.0, 3.0, 0.5);
        // forward -Z, magnitude speed*dt = 1.5.
        assert!((d.z + 1.5).abs() < EPS, "z = {}", d.z);
    }
}
