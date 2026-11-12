//! XR controller pose tracker.
//!
//! [`ControllerTracker`] holds the most recently located grip poses for both
//! controllers. Poses are in the **STAGE reference space** — the same global
//! world-space origin as the HMD pose from [`crate::view::HmdPose`].
//!
//! # Space convention and snap-turns
//!
//! Both HMD and controller poses are reported in absolute **STAGE space**
//! (`OpenXR` `XR_REFERENCE_SPACE_TYPE_STAGE`). They are **not** relative to
//! each other. A future "Pawn / Player Controller" applying a yaw snap-turn
//! must apply the same rotation offset to both the HMD camera and both
//! controllers. The recommended pattern is a single `stage_to_world: Mat4`
//! on the preview session (identity now); multiplying every STAGE-space pose
//! by it before writing the ECS `Transform` keeps all XR objects in sync.
//!
//! Because controllers are in global STAGE space — not HMD-relative — they
//! should **not** be parented to the HMD camera entity. The pawn's snap-turn
//! offset is applied to controller world matrices independently
//! ([`Pawn::apply_hmd`](crate::pawn::Pawn::apply_hmd) /
//! [`apply_controller`](crate::pawn::Pawn::apply_controller)).

use glam::{Quat, Vec3};

/// A located controller grip pose in STAGE (world) space.
///
/// Position is in metres from the STAGE origin (roomscale floor level).
/// Orientation is a unit quaternion in the same STAGE frame.
///
/// Both fields are in the same coordinate frame as [`crate::view::HmdPose`].
#[derive(Debug, Clone, Copy)]
pub struct ControllerPose {
    /// Position in metres, STAGE space (world space).
    pub position: Vec3,
    /// Orientation (unit quaternion), STAGE space (world space).
    pub orientation: Quat,
    /// Predicted display timestamp (seconds) this pose was located at.
    pub timestamp: f64,
}

/// Holds the most recently located grip poses for both hands.
///
/// Updated atomically once per frame from the graphics session or a test
/// injector. Either or both poses may be `None` when the corresponding
/// controller is untracked (occluded, off, or no pose yet received).
#[derive(Debug, Default)]
pub struct ControllerTracker {
    left: Option<ControllerPose>,
    right: Option<ControllerPose>,
}

impl ControllerTracker {
    /// Create an empty tracker (no controllers tracked).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace both controller poses atomically (called once per frame from the
    /// graphics session or a test injector).
    pub fn set_poses(&mut self, left: Option<ControllerPose>, right: Option<ControllerPose>) {
        self.left = left;
        self.right = right;
    }

    /// The latest located left-controller grip pose, or `None` when untracked.
    #[must_use]
    pub fn left(&self) -> Option<&ControllerPose> {
        self.left.as_ref()
    }

    /// The latest located right-controller grip pose, or `None` when untracked.
    #[must_use]
    pub fn right(&self) -> Option<&ControllerPose> {
        self.right.as_ref()
    }
}

/// A physical XR input source an action binding can reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XrInputSource {
    /// The left-hand controller.
    LeftController,
    /// The right-hand controller.
    RightController,
    /// Eye/head gaze.
    Gaze,
}

/// One controller's per-frame button / axis state (device-neutral plain data).
///
/// Populated once per frame from the graphics session's `OpenXR` action states
/// (`xrGetActionState*`) on the real path, or from a test injector. `primary` /
/// `secondary` are the hand's lower / upper face buttons — `X`/`Y` on the left
/// controller, `A`/`B` on the right — so one struct serves both hands; the
/// consumer (the session) routes them to the correct logical source names.
///
/// This mirrors the shape the behavior-layer input feed
/// (`idtx_behavior::XrHandInput`) expects, but is kept free of any dependency on
/// that crate — platform-xr stays a leaf.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
// Each bool is an independent physical button; they are not a state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct ControllerButtons {
    /// Thumbstick deflection, `[x, y]` each in `[-1, 1]` (x right+, y up+).
    pub thumbstick: [f32; 2],
    /// Trigger pull, `[0, 1]`.
    pub trigger: f32,
    /// Grip / squeeze, `[0, 1]`.
    pub grip: f32,
    /// Thumbstick pressed in (click).
    pub thumbstick_click: bool,
    /// Lower face button — `X` (left) / `A` (right).
    pub primary: bool,
    /// Upper face button — `Y` (left) / `B` (right).
    pub secondary: bool,
    /// Menu button (left controller only).
    pub menu: bool,
}

/// Holds the most recently polled button / axis state for both controllers.
///
/// Updated once per frame from the graphics session's `OpenXR` action states or a
/// test injector — the input twin of [`ControllerTracker`] (which holds poses).
#[derive(Debug, Default, Clone, Copy)]
pub struct ControllerInputTracker {
    left: ControllerButtons,
    right: ControllerButtons,
}

impl ControllerInputTracker {
    /// A fresh tracker with both controllers at rest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace both controllers' input state atomically (once per frame).
    pub fn set(&mut self, left: ControllerButtons, right: ControllerButtons) {
        self.left = left;
        self.right = right;
    }

    /// The latest left-controller input state.
    #[must_use]
    pub fn left(&self) -> &ControllerButtons {
        &self.left
    }

    /// The latest right-controller input state.
    #[must_use]
    pub fn right(&self) -> &ControllerButtons {
        &self.right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_starts_untracked() {
        let t = ControllerTracker::new();
        assert!(t.left().is_none());
        assert!(t.right().is_none());
    }

    #[test]
    fn set_poses_round_trips() {
        let mut t = ControllerTracker::new();
        let left = ControllerPose {
            position: Vec3::new(0.3, 1.2, -0.5),
            orientation: Quat::IDENTITY,
            timestamp: 1.0,
        };
        let right = ControllerPose {
            position: Vec3::new(-0.3, 1.2, -0.5),
            orientation: Quat::from_rotation_y(0.1),
            timestamp: 1.0,
        };
        t.set_poses(Some(left), Some(right));
        assert!((t.left().unwrap().position - left.position).length() < 1e-6);
        assert!((t.right().unwrap().position - right.position).length() < 1e-6);
    }

    #[test]
    fn set_poses_none_clears() {
        let mut t = ControllerTracker::new();
        t.set_poses(
            Some(ControllerPose {
                position: Vec3::ONE,
                orientation: Quat::IDENTITY,
                timestamp: 0.0,
            }),
            None,
        );
        assert!(t.left().is_some());
        assert!(t.right().is_none());
        t.set_poses(None, None);
        assert!(t.left().is_none());
    }

    #[test]
    fn input_tracker_starts_at_rest() {
        let t = ControllerInputTracker::new();
        assert_eq!(*t.left(), ControllerButtons::default());
        assert_eq!(*t.right(), ControllerButtons::default());
    }

    #[test]
    fn input_tracker_round_trips() {
        let mut t = ControllerInputTracker::new();
        let left = ControllerButtons {
            thumbstick: [0.4, -0.9],
            trigger: 0.75,
            primary: true,
            ..ControllerButtons::default()
        };
        let right = ControllerButtons {
            grip: 1.0,
            secondary: true,
            ..ControllerButtons::default()
        };
        t.set(left, right);
        assert_eq!(*t.left(), left);
        assert_eq!(*t.right(), right);
    }
}
