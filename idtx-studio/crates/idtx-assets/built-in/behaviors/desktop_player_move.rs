// desktop_player_move — built-in WASD + yaw mouse-look for the desktop player.
//
// Part of the built-in player's default gameplay set (ships inside the toolbox
// at crates/idtx-assets/built-in/behaviors). Attached to /World/Player via the
// built-in Desktop rig's IdtxBehaviorAPI binding. Reads the logical `look` 2D
// axis (mouse) and `move_forward` / `move_right` scalar axes the project's
// input_map.toml binds, integrates a yaw angle from the mouse-X delta, slides
// the pawn along its yaw-relative ground plane, and authors the result through
// the stage_edit (session-layer) seam so the child camera follows.
//
// Player half of the textbook FPS split: yaw + translation here, pitch on the
// camera (desktop_camera_pitch). Capabilities: `input` + `stage_edit`.
//
// include!'d into the build skeleton → only `//` line comments.

use idtx_behavior_sdk::prelude::*;

/// Radians of yaw per pixel of mouse-X delta.
const MOUSE_SENS: f32 = 0.0025;
/// Metres per second at full key/stick deflection.
const MOVE_SPEED: f32 = 3.0;

/// Rotates its prim about Y and translates it along the yaw-relative ground
/// plane in response to WASD + mouse-look.
#[derive(Default)]
pub struct DesktopPlayerMove {
    yaw: f32,
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

/// Integrate yaw from a mouse-X delta (mouse-right turns clockwise → yaw down).
fn integrate_yaw(yaw: f32, mouse_dx: f32, sens: f32) -> f32 {
    yaw - mouse_dx * sens
}

/// Per-frame world-space translation for the given yaw and axis inputs.
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
        assert!(integrate_yaw(0.0, 10.0, 0.01) < 0.0);
        assert!(integrate_yaw(0.0, -10.0, 0.01) > 0.0);
        assert!((integrate_yaw(1.23, 0.0, 0.01) - 1.23).abs() < EPS);
    }

    #[test]
    fn forward_at_zero_yaw_is_negative_z() {
        let d = move_delta(0.0, 1.0, 0.0, 1.0, 1.0);
        assert!(d.x.abs() < EPS && d.y.abs() < EPS && (d.z + 1.0).abs() < EPS);
    }

    #[test]
    fn strafe_at_zero_yaw_is_positive_x() {
        let d = move_delta(0.0, 0.0, 1.0, 1.0, 1.0);
        assert!((d.x - 1.0).abs() < EPS && d.z.abs() < EPS);
    }
}
