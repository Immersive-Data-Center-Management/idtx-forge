//! `XrSnapshot` — the most recently polled tracked-space poses the host
//! exposes to behaviors through the `xr_hmd` capability.
//!
//! A `Copy` value holding one [`XrPoseResult`] per [`XrSpace`] plus optional
//! hand-joint arrays. The host carries the current snapshot in
//! each behavior store's [`HostState`] and refreshes it before every tick
//! via [`crate::BehaviorHost::set_xr_snapshot`]. Guest imports read directly
//! from it — a plain memory copy, no per-frame FFI into the XR platform.

use idtx_behavior_abi::{
    XR_HAND_JOINT_COUNT, Xform, XrHandJointResult, XrInputSource, XrPoseResult, XrSide, XrSpace,
};

/// A snapshot of every tracked XR space's pose and optional hand joints for
/// the current frame.
///
/// Defaults to all-untracked (`valid == 0`), so a behavior polling before
/// the platform has produced a pose simply sees `None`.
#[derive(Debug, Clone, Default)]
pub struct XrSnapshot {
    hmd: XrPoseResult,
    left_hand: XrPoseResult,
    right_hand: XrPoseResult,
    /// Left-hand **aim** pose (pointer ray origin+direction).
    left_aim: XrPoseResult,
    /// Right-hand **aim** pose.
    right_aim: XrPoseResult,
    /// 26-joint array for the left hand, or `None` when not tracked.
    left_joints: Option<Box<[XrHandJointResult; XR_HAND_JOINT_COUNT]>>,
    /// 26-joint array for the right hand, or `None` when not tracked.
    right_joints: Option<Box<[XrHandJointResult; XR_HAND_JOINT_COUNT]>>,
    /// Which source(s) drive the left side this frame (controller / hand).
    left_source: XrInputSource,
    /// Which source(s) drive the right side this frame (controller / hand).
    right_source: XrInputSource,
}

impl XrSnapshot {
    /// An all-untracked snapshot (the default).
    #[must_use]
    pub fn untracked() -> Self {
        Self::default()
    }

    /// Convenience: a snapshot with only the HMD tracked at `xform`.
    #[must_use]
    pub fn with_hmd(xform: Xform) -> Self {
        let mut s = Self::default();
        s.set(XrSpace::Hmd, XrPoseResult::tracked(xform));
        s
    }

    /// The pose for `space` (untracked sentinel when never set).
    #[must_use]
    pub fn get(&self, space: XrSpace) -> XrPoseResult {
        match space {
            XrSpace::Hmd => self.hmd,
            XrSpace::LeftHand => self.left_hand,
            XrSpace::RightHand => self.right_hand,
            XrSpace::LeftAim => self.left_aim,
            XrSpace::RightAim => self.right_aim,
        }
    }

    /// Set the pose for `space`.
    pub fn set(&mut self, space: XrSpace, pose: XrPoseResult) {
        match space {
            XrSpace::Hmd => self.hmd = pose,
            XrSpace::LeftHand => self.left_hand = pose,
            XrSpace::RightHand => self.right_hand = pose,
            XrSpace::LeftAim => self.left_aim = pose,
            XrSpace::RightAim => self.right_aim = pose,
        }
    }

    /// Store 26 joint results for `side`. Pass `None` to clear.
    pub fn set_hand_joints(
        &mut self,
        side: XrSide,
        joints: Option<Box<[XrHandJointResult; XR_HAND_JOINT_COUNT]>>,
    ) {
        match side {
            XrSide::Left => self.left_joints = joints,
            XrSide::Right => self.right_joints = joints,
        }
    }

    /// Retrieve the 26-joint array for `side`, or `None` when not tracked.
    #[must_use]
    pub fn hand_joints(&self, side: XrSide) -> Option<&[XrHandJointResult; XR_HAND_JOINT_COUNT]> {
        match side {
            XrSide::Left => self.left_joints.as_deref(),
            XrSide::Right => self.right_joints.as_deref(),
        }
    }

    /// Set which source(s) drive `side` this frame (controller / hand / both).
    pub fn set_input_source(&mut self, side: XrSide, source: XrInputSource) {
        match side {
            XrSide::Left => self.left_source = source,
            XrSide::Right => self.right_source = source,
        }
    }

    /// Which source(s) drive `side` this frame. [`XrInputSource::NONE`] when the
    /// side is not tracked (the default). Read by the `idtx_xr_get_tracking_mode`
    /// import so a behavior can tell a controller from hand tracking even when
    /// the grip/aim pose spaces are synthesised from the hand.
    #[must_use]
    pub fn input_source(&self, side: XrSide) -> XrInputSource {
        match side {
            XrSide::Left => self.left_source,
            XrSide::Right => self.right_source,
        }
    }
}

/// Build an [`XrSnapshot`] from a real `OpenXR` HMD pose. Gated behind the
/// `xr` feature so the host stays XR-platform-agnostic by default.
#[cfg(feature = "xr")]
impl From<&idtx_platform_xr::HmdPose> for XrSnapshot {
    fn from(pose: &idtx_platform_xr::HmdPose) -> Self {
        let xform = Xform::from_trs(pose.position, pose.orientation, glam::Vec3::ONE);
        Self::with_hmd(xform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_untracked() {
        let s = XrSnapshot::untracked();
        assert!(!s.get(XrSpace::Hmd).is_valid());
        assert!(!s.get(XrSpace::LeftHand).is_valid());
        assert!(!s.get(XrSpace::RightHand).is_valid());
        assert!(s.hand_joints(XrSide::Left).is_none());
        assert!(s.hand_joints(XrSide::Right).is_none());
        assert_eq!(s.input_source(XrSide::Left), XrInputSource::NONE);
        assert_eq!(s.input_source(XrSide::Right), XrInputSource::NONE);
    }

    #[test]
    fn input_source_round_trips_per_side() {
        let mut s = XrSnapshot::default();
        s.set_input_source(XrSide::Left, XrInputSource::CONTROLLER);
        s.set_input_source(
            XrSide::Right,
            XrInputSource::NONE.with_controller(true).with_hand(true),
        );
        // Left: controller only.
        assert!(s.input_source(XrSide::Left).has_controller());
        assert!(!s.input_source(XrSide::Left).has_hand());
        // Right: both sources present simultaneously.
        assert!(s.input_source(XrSide::Right).has_controller());
        assert!(s.input_source(XrSide::Right).has_hand());
    }

    #[test]
    fn set_get_round_trips_per_space() {
        let mut s = XrSnapshot::default();
        let x = Xform::from_trs(
            glam::Vec3::new(1.0, 2.0, 3.0),
            glam::Quat::IDENTITY,
            glam::Vec3::ONE,
        );
        s.set(XrSpace::Hmd, XrPoseResult::tracked(x));
        assert!(s.get(XrSpace::Hmd).is_valid());
        assert_eq!(s.get(XrSpace::Hmd).xform.translation, x.translation);
        // Other spaces stay untracked.
        assert!(!s.get(XrSpace::RightHand).is_valid());
    }

    #[test]
    fn aim_spaces_are_independent_from_grip() {
        let mut s = XrSnapshot::default();
        let grip = Xform::from_trs(
            glam::Vec3::new(1.0, 0.0, 0.0),
            glam::Quat::IDENTITY,
            glam::Vec3::ONE,
        );
        let aim = Xform::from_trs(
            glam::Vec3::new(0.0, 5.0, 0.0),
            glam::Quat::IDENTITY,
            glam::Vec3::ONE,
        );
        s.set(XrSpace::LeftHand, XrPoseResult::tracked(grip));
        s.set(XrSpace::LeftAim, XrPoseResult::tracked(aim));
        // Grip and aim on the same hand are distinct channels.
        assert_eq!(s.get(XrSpace::LeftHand).xform.translation, grip.translation);
        assert_eq!(s.get(XrSpace::LeftAim).xform.translation, aim.translation);
        // Right-hand aim untouched.
        assert!(!s.get(XrSpace::RightAim).is_valid());
    }

    #[test]
    fn hand_joints_round_trip() {
        let mut s = XrSnapshot::default();
        let mut joints = Box::new(
            [XrHandJointResult {
                position: [0.0; 3],
                radius: 0.0,
                orientation: [0.0, 0.0, 0.0, 1.0],
                valid: 0,
                _pad: 0,
            }; XR_HAND_JOINT_COUNT],
        );
        joints[0].valid = 1;
        joints[0].radius = 0.01;
        s.set_hand_joints(XrSide::Left, Some(joints));
        let got = s.hand_joints(XrSide::Left).unwrap();
        assert_eq!(got[0].valid, 1);
        assert!((got[0].radius - 0.01).abs() < 1e-6);
        assert!(s.hand_joints(XrSide::Right).is_none());
    }

    #[cfg(feature = "xr")]
    #[test]
    fn from_hmd_pose_maps_position_and_orientation() {
        let pose = idtx_platform_xr::HmdPose {
            position: glam::Vec3::new(0.5, 1.6, -2.0),
            orientation: glam::Quat::from_rotation_y(0.3),
            timestamp: 1.0,
        };
        let snap = XrSnapshot::from(&pose);
        let hmd = snap.get(XrSpace::Hmd);
        assert!(hmd.is_valid());
        let t: glam::Vec3 = hmd.xform.translation.into();
        assert!((t - pose.position).length() < 1e-6);
        let r: glam::Quat = hmd.xform.rotation.into();
        assert!(r.angle_between(pose.orientation) < 1e-6);
    }
}
