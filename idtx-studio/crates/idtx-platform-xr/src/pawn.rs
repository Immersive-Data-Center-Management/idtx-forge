//! Pawn / player-controller origin seam.
//!
//! XR poses arrive in **STAGE reference space** — the HMD
//! ([`crate::view::HmdPose`]) and both controllers ([`crate::input::
//! ControllerPose`]) are all located in that single global frame, never
//! relative to each other (see the space convention in [`crate::input`]).
//!
//! A [`Pawn`] carries one `stage_to_world: Mat4`. Multiplying every
//! STAGE-space pose by it before it reaches the ECS `Transform` is the
//! documented pattern for locomotion / snap-turns: because the SAME matrix
//! is applied to the HMD camera and to both controllers independently, they
//! all stay rigidly in sync — no controller is ever parented to the HMD.
//!
//! The default pawn is [`Pawn::IDENTITY`] (poses pass through unchanged),
//! matching today's "pawn at the stage origin" behaviour.

use glam::{Mat4, Quat, Vec3};

use crate::input::ControllerPose;
use crate::view::HmdPose;

/// A player pawn: a single `stage_to_world` transform applied uniformly to
/// the HMD pose and both controller poses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pawn {
    stage_to_world: Mat4,
}

impl Default for Pawn {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Pawn {
    /// A pawn at the stage origin: `stage_to_world` is the identity, so every
    /// pose passes through unchanged. This is the current default.
    pub const IDENTITY: Self = Self {
        stage_to_world: Mat4::IDENTITY,
    };

    /// A pawn at the stage origin (alias of [`Pawn::IDENTITY`]).
    #[must_use]
    pub const fn new() -> Self {
        Self::IDENTITY
    }

    /// A pawn from an arbitrary stage-to-world transform.
    #[must_use]
    pub const fn from_stage_to_world(stage_to_world: Mat4) -> Self {
        Self { stage_to_world }
    }

    /// A pawn placed at `origin` (metres, world space) with a `yaw` snap-turn
    /// (radians about the +Y world-up axis) — the common locomotion case:
    /// translate the play space, then rotate it about world up.
    #[must_use]
    pub fn from_origin_yaw(origin: Vec3, yaw: f32) -> Self {
        Self {
            stage_to_world: Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), origin),
        }
    }

    /// The underlying stage-to-world matrix.
    #[must_use]
    pub const fn stage_to_world(&self) -> Mat4 {
        self.stage_to_world
    }

    /// Map an HMD pose from STAGE space into world space. `timestamp` is
    /// carried through unchanged (it describes *when*, not *where*).
    #[must_use]
    pub fn apply_hmd(&self, pose: &HmdPose) -> HmdPose {
        let (position, orientation) = self.apply_pose(pose.position, pose.orientation);
        HmdPose {
            position,
            orientation,
            timestamp: pose.timestamp,
        }
    }

    /// Map a controller pose from STAGE space into world space (applied
    /// independently of the HMD — controllers are not HMD-relative).
    #[must_use]
    pub fn apply_controller(&self, pose: &ControllerPose) -> ControllerPose {
        let (position, orientation) = self.apply_pose(pose.position, pose.orientation);
        ControllerPose {
            position,
            orientation,
            timestamp: pose.timestamp,
        }
    }

    /// Transform a (position, orientation) pair by `stage_to_world`: the
    /// position is a full affine point transform; the orientation is
    /// pre-rotated by the transform's rotation component.
    fn apply_pose(&self, position: Vec3, orientation: Quat) -> (Vec3, Quat) {
        let world_position = self.stage_to_world.transform_point3(position);
        let (_scale, rotation, _translation) = self.stage_to_world.to_scale_rotation_translation();
        (world_position, rotation * orientation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn hmd(position: Vec3, orientation: Quat) -> HmdPose {
        HmdPose {
            position,
            orientation,
            timestamp: 3.0,
        }
    }

    fn controller(position: Vec3, orientation: Quat) -> ControllerPose {
        ControllerPose {
            position,
            orientation,
            timestamp: 3.0,
        }
    }

    #[test]
    fn identity_pawn_passes_poses_through() {
        let pawn = Pawn::IDENTITY;
        assert_eq!(pawn, Pawn::default());

        let h = hmd(Vec3::new(0.3, 1.6, -0.5), Quat::from_rotation_y(0.4));
        let out = pawn.apply_hmd(&h);
        assert!((out.position - h.position).length() < 1e-6);
        assert!((out.orientation.to_axis_angle().1 - h.orientation.to_axis_angle().1).abs() < 1e-6);

        let c = controller(Vec3::new(-0.3, 1.2, -0.5), Quat::IDENTITY);
        let out = pawn.apply_controller(&c);
        assert!((out.position - c.position).length() < 1e-6);
    }

    #[test]
    fn translation_offsets_position_but_not_orientation() {
        let pawn = Pawn::from_origin_yaw(Vec3::new(10.0, 0.0, -5.0), 0.0);
        let orient = Quat::from_rotation_y(0.7);
        let h = hmd(Vec3::new(1.0, 1.6, 2.0), orient);
        let out = pawn.apply_hmd(&h);
        assert!((out.position - Vec3::new(11.0, 1.6, -3.0)).length() < 1e-5);
        // Pure translation leaves orientation untouched.
        assert!(out.orientation.angle_between(orient) < 1e-5);
    }

    #[test]
    fn yaw_snap_turn_rotates_hmd_and_both_controllers_uniformly() {
        // +90° about Y sends +X toward -Z (glam's right-handed convention).
        let pawn = Pawn::from_origin_yaw(Vec3::ZERO, FRAC_PI_2);

        let head = hmd(Vec3::X, Quat::IDENTITY);
        let left = controller(Vec3::new(0.3, 1.2, 0.0), Quat::IDENTITY);
        let right = controller(Vec3::new(-0.3, 1.2, 0.0), Quat::IDENTITY);

        let head_w = pawn.apply_hmd(&head);
        let left_w = pawn.apply_controller(&left);
        let right_w = pawn.apply_controller(&right);

        assert!((head_w.position - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5);

        // The SAME yaw rotates every pose's orientation identically. Compare
        // by the image of a probe vector (unambiguous under the q/-q double
        // cover, unlike comparing quaternion components directly).
        let expected = Quat::from_rotation_y(FRAC_PI_2) * Vec3::X;
        assert!((head_w.orientation * Vec3::X - expected).length() < 1e-5);
        assert!((left_w.orientation * Vec3::X - expected).length() < 1e-5);
        assert!((right_w.orientation * Vec3::X - expected).length() < 1e-5);

        // The left→right separation vector rotates rigidly (its length is
        // preserved), proving the controllers stay locked to the play space.
        let sep_stage = right.position - left.position;
        let sep_world = right_w.position - left_w.position;
        assert!((sep_world.length() - sep_stage.length()).abs() < 1e-5);
        assert!(sep_world.y.abs() < 1e-5); // yaw is about Y: height unchanged
    }

    #[test]
    fn timestamp_is_preserved() {
        let pawn = Pawn::from_origin_yaw(Vec3::new(1.0, 2.0, 3.0), 0.5);
        let h = pawn.apply_hmd(&hmd(Vec3::ZERO, Quat::IDENTITY));
        assert!((h.timestamp - 3.0).abs() < 1e-9);
        let c = pawn.apply_controller(&controller(Vec3::ZERO, Quat::IDENTITY));
        assert!((c.timestamp - 3.0).abs() < 1e-9);
    }

    #[test]
    fn stage_to_world_round_trips_the_matrix() {
        let m =
            Mat4::from_rotation_translation(Quat::from_rotation_y(0.25), Vec3::new(4.0, 0.0, 1.0));
        assert_eq!(Pawn::from_stage_to_world(m).stage_to_world(), m);
    }
}
