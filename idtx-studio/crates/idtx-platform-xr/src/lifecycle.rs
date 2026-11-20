//! Session lifecycle state machine + the owning [`XrPlatform`].
//!
//! Models the `OpenXR` session-state progression the runtime drives:
//! `Idle → Ready → Synchronized → Visible → Focused`, then
//! `→ Stopping → Idle` on exit. Exposes explicit transition methods; the
//! shape is validated headlessly.

use crate::hand::{HandPose, HandTracker};
use crate::hmd::HmdTracker;
use crate::input::{ControllerButtons, ControllerInputTracker, ControllerPose, ControllerTracker};
use crate::pawn::Pawn;
use crate::view::HmdPose;

/// `OpenXR` session lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrSessionState {
    /// No session / session ended.
    Idle,
    /// Session created, ready to begin.
    Ready,
    /// Frames are synchronising but not yet visible.
    Synchronized,
    /// Composited output is visible to the user.
    Visible,
    /// The app has input focus (poses + input are live).
    Focused,
    /// Exit requested; winding down.
    Stopping,
}

/// An invalid lifecycle transition was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid XR session transition: {from:?} → {to:?}")]
pub struct XrStateError {
    /// The state the platform was in.
    pub from: XrSessionState,
    /// The state that was requested.
    pub to: XrSessionState,
}

/// Owns the XR session lifetime + head tracker + controller tracker + hand
/// tracker.
///
/// Completely idle until [`Self::begin_session`]. Without the `pcvr`
/// feature the head tracker only surfaces mock-injected poses.
#[derive(Debug)]
pub struct XrPlatform {
    state: XrSessionState,
    tracker: HmdTracker,
    controllers: ControllerTracker,
    /// Aim-pose tracker — the "where the controller points" pose that drives
    /// the pointer ray. Same STAGE space as [`Self::controllers`], but a
    /// distinct, ergonomically-tilted pose per the `OpenXR` aim/grip split.
    aims: ControllerTracker,
    /// Hand-joint tracker. Populated from `xrLocateHandJointsEXT` on the pcvr
    /// path (requires `hand-tracking` feature + device support), or from
    /// [`Self::inject_hand_poses`] on the `mock-xr` path.
    hands: HandTracker,
    /// Controller button / axis state (thumbsticks, triggers, grips, buttons),
    /// polled once per frame from `OpenXR` action states (real path) or injected
    /// (mock-xr). The input twin of [`Self::controllers`].
    controller_input: ControllerInputTracker,
    /// The player pawn: a single `stage_to_world` transform applied uniformly
    /// to the HMD pose and both controller/aim poses as they leave this
    /// platform. Locomotion / snap-turn move the whole rig by
    /// writing this one matrix; the default [`Pawn::IDENTITY`] passes poses
    /// through unchanged (today's "pawn at the stage origin" behaviour).
    pawn: Pawn,
}

impl Default for XrPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl XrPlatform {
    /// A fresh, idle platform.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: XrSessionState::Idle,
            tracker: HmdTracker::new(),
            controllers: ControllerTracker::new(),
            aims: ControllerTracker::new(),
            hands: HandTracker::new(),
            controller_input: ControllerInputTracker::new(),
            pawn: Pawn::IDENTITY,
        }
    }

    /// The current lifecycle state.
    #[must_use]
    pub fn state(&self) -> XrSessionState {
        self.state
    }

    /// Borrow the head tracker.
    #[must_use]
    pub fn tracker(&self) -> &HmdTracker {
        &self.tracker
    }

    /// Mutably borrow the head tracker (e.g. to register a callback).
    pub fn tracker_mut(&mut self) -> &mut HmdTracker {
        &mut self.tracker
    }

    /// The latest located head pose in **world space** (the pawn's
    /// `stage_to_world` is applied), or `None`.
    #[must_use]
    pub fn current_pose(&self) -> Option<HmdPose> {
        self.tracker
            .current_pose()
            .map(|pose| self.pawn.apply_hmd(&pose))
    }

    /// The current player [`Pawn`] (the stage→world seam applied to all poses).
    #[must_use]
    pub fn pawn(&self) -> Pawn {
        self.pawn
    }

    /// Replace the player pawn. Every subsequent pose read from this platform
    /// ([`Self::current_pose`], [`Self::controller_poses`], [`Self::aim_poses`])
    /// is mapped through the new `stage_to_world` — the same matrix for the HMD
    /// and both controllers, so the rig stays rigid. This is the seam
    /// locomotion / snap-turn behaviours drive.
    pub fn set_pawn(&mut self, pawn: Pawn) {
        self.pawn = pawn;
    }

    /// Convenience: place the pawn at `origin` (world metres) with a `yaw`
    /// snap-turn (radians about world +Y). See [`Pawn::from_origin_yaw`].
    pub fn set_pawn_origin_yaw(&mut self, origin: glam::Vec3, yaw: f32) {
        self.pawn = Pawn::from_origin_yaw(origin, yaw);
    }

    fn transition(
        &mut self,
        allowed_from: &[XrSessionState],
        to: XrSessionState,
    ) -> Result<(), XrStateError> {
        if allowed_from.contains(&self.state) {
            self.state = to;
            Ok(())
        } else {
            Err(XrStateError {
                from: self.state,
                to,
            })
        }
    }

    /// `Idle → Ready`: create the session.
    pub fn begin_session(&mut self) -> Result<(), XrStateError> {
        self.transition(&[XrSessionState::Idle], XrSessionState::Ready)
    }

    /// `Ready → Synchronized`.
    pub fn synchronize(&mut self) -> Result<(), XrStateError> {
        self.transition(&[XrSessionState::Ready], XrSessionState::Synchronized)
    }

    /// `Synchronized → Visible`.
    pub fn show(&mut self) -> Result<(), XrStateError> {
        self.transition(&[XrSessionState::Synchronized], XrSessionState::Visible)
    }

    /// `Visible → Focused`.
    pub fn focus(&mut self) -> Result<(), XrStateError> {
        self.transition(&[XrSessionState::Visible], XrSessionState::Focused)
    }

    /// Request exit: any active state `→ Stopping`.
    pub fn request_exit(&mut self) -> Result<(), XrStateError> {
        self.transition(
            &[
                XrSessionState::Ready,
                XrSessionState::Synchronized,
                XrSessionState::Visible,
                XrSessionState::Focused,
            ],
            XrSessionState::Stopping,
        )
    }

    /// `Stopping → Idle` (also idempotent from `Idle`): tear the session
    /// down.
    pub fn end_session(&mut self) -> Result<(), XrStateError> {
        self.transition(
            &[XrSessionState::Stopping, XrSessionState::Idle],
            XrSessionState::Idle,
        )
    }

    /// Set the latest located HMD pose (real `pcvr` path). The graphics
    /// session locates the head each frame and pushes it here so
    /// [`Self::current_pose`] reflects the live headset.
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    pub fn set_current_pose(&mut self, pose: HmdPose) {
        self.tracker.set_pose(pose);
    }

    /// Inject a mock head pose (tests / headless CI).
    #[cfg(feature = "mock-xr")]
    pub fn inject_pose(&mut self, pose: HmdPose) {
        self.tracker.inject_pose(pose);
    }

    /// The latest located controller poses in **world space** (the pawn's
    /// `stage_to_world` is applied to each), or `None` per hand when untracked.
    ///
    /// Both poses share the HMD's world frame (same [`Self::pawn`]) — see
    /// [`crate::input`] docs for snap-turn notes.
    #[must_use]
    pub fn controller_poses(&self) -> (Option<ControllerPose>, Option<ControllerPose>) {
        (
            self.controllers
                .left()
                .map(|p| self.pawn.apply_controller(p)),
            self.controllers
                .right()
                .map(|p| self.pawn.apply_controller(p)),
        )
    }

    /// Push located controller poses from the graphics session (real `pcvr` path).
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    pub fn set_controller_poses(
        &mut self,
        left: Option<ControllerPose>,
        right: Option<ControllerPose>,
    ) {
        self.controllers.set_poses(left, right);
    }

    /// Inject mock controller poses (tests / headless CI).
    #[cfg(feature = "mock-xr")]
    pub fn inject_controller_poses(
        &mut self,
        left: Option<ControllerPose>,
        right: Option<ControllerPose>,
    ) {
        self.controllers.set_poses(left, right);
    }

    /// The latest located aim poses in **world space** (the pawn's
    /// `stage_to_world` is applied to each), or `None` per hand when untracked.
    ///
    /// Both poses share the HMD's world frame (same [`Self::pawn`]), the same
    /// frame as [`Self::controller_poses`]. The aim pose is the pointer-ray
    /// source.
    #[must_use]
    pub fn aim_poses(&self) -> (Option<ControllerPose>, Option<ControllerPose>) {
        (
            self.aims.left().map(|p| self.pawn.apply_controller(p)),
            self.aims.right().map(|p| self.pawn.apply_controller(p)),
        )
    }

    /// Push located aim poses from the graphics session (real `pcvr` path).
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    pub fn set_aim_poses(&mut self, left: Option<ControllerPose>, right: Option<ControllerPose>) {
        self.aims.set_poses(left, right);
    }

    /// Inject mock aim poses (tests / headless CI).
    #[cfg(feature = "mock-xr")]
    pub fn inject_aim_poses(
        &mut self,
        left: Option<ControllerPose>,
        right: Option<ControllerPose>,
    ) {
        self.aims.set_poses(left, right);
    }

    // ── Controller button / axis input ──────────────────────────────────────

    /// The latest polled controller button / axis state `(left, right)`.
    ///
    /// Unlike poses, input state carries no pawn mapping — buttons and stick
    /// deflections are frame-local scalars, not world-space transforms.
    #[must_use]
    pub fn controller_input(&self) -> (&ControllerButtons, &ControllerButtons) {
        (self.controller_input.left(), self.controller_input.right())
    }

    /// Push polled controller input from the graphics session (real `pcvr` /
    /// `quest` path), read from the per-frame `OpenXR` action states.
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    pub fn set_controller_input(&mut self, left: ControllerButtons, right: ControllerButtons) {
        self.controller_input.set(left, right);
    }

    /// Inject mock controller input (tests / headless CI).
    #[cfg(feature = "mock-xr")]
    pub fn inject_controller_input(&mut self, left: ControllerButtons, right: ControllerButtons) {
        self.controller_input.set(left, right);
    }

    // ── Hand tracking ─────────────────────────────────────────────────────────

    /// Whether the connected device supports `XR_EXT_hand_tracking`.
    ///
    /// Returns `false` until `mark_hand_tracking_supported()` is called (which
    /// happens from the pcvr path after a successful system property probe).
    #[must_use]
    pub fn hand_tracking_supported(&self) -> bool {
        self.hands.is_supported()
    }

    /// Signal that the device confirmed `XR_EXT_hand_tracking` support.
    ///
    /// Called once from the pcvr frame-init path after `xrGetSystemProperties`
    /// returns `supportsHandTracking == XR_TRUE`.
    pub fn mark_hand_tracking_supported(&mut self) {
        self.hands.mark_supported();
    }

    /// The latest located hand poses as `(left, right)`.
    ///
    /// `None` per hand when untracked or when hand tracking is not supported.
    #[must_use]
    pub fn hand_poses(
        &self,
    ) -> (
        Option<&crate::hand::HandPose>,
        Option<&crate::hand::HandPose>,
    ) {
        self.hands.poses()
    }

    /// Push located hand poses from the graphics session (real `pcvr` path).
    ///
    /// Called once per frame from `render_xr()` after `xrLocateHandJointsEXT`
    /// completes. Pass `None` for any hand that was not tracked this frame.
    pub fn set_hand_poses(&mut self, left: Option<HandPose>, right: Option<HandPose>) {
        self.hands.set_poses(left, right);
    }

    /// Inject mock hand poses (tests / headless CI).
    #[cfg(feature = "mock-xr")]
    pub fn inject_hand_poses(&mut self, left: Option<HandPose>, right: Option<HandPose>) {
        self.hands.inject_poses(left, right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_lifecycle_progression() {
        let mut xr = XrPlatform::new();
        assert_eq!(xr.state(), XrSessionState::Idle);
        xr.begin_session().unwrap();
        assert_eq!(xr.state(), XrSessionState::Ready);
        xr.synchronize().unwrap();
        xr.show().unwrap();
        xr.focus().unwrap();
        assert_eq!(xr.state(), XrSessionState::Focused);
        xr.request_exit().unwrap();
        assert_eq!(xr.state(), XrSessionState::Stopping);
        xr.end_session().unwrap();
        assert_eq!(xr.state(), XrSessionState::Idle);
    }

    #[test]
    fn invalid_transition_is_rejected() {
        let mut xr = XrPlatform::new();
        // Can't focus straight from Idle.
        let err = xr.focus().unwrap_err();
        assert_eq!(err.from, XrSessionState::Idle);
        assert_eq!(err.to, XrSessionState::Focused);
        // State is unchanged after a rejected transition.
        assert_eq!(xr.state(), XrSessionState::Idle);
    }

    #[test]
    fn no_pose_before_tracking() {
        let xr = XrPlatform::new();
        assert!(xr.current_pose().is_none());
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn injected_pose_is_returned_across_transitions() {
        use glam::{Quat, Vec3};

        let mut xr = XrPlatform::new();
        xr.begin_session().unwrap();
        xr.synchronize().unwrap();
        xr.show().unwrap();
        xr.focus().unwrap();

        let pose = HmdPose {
            position: Vec3::new(0.1, 1.7, -0.3),
            orientation: Quat::from_rotation_y(0.5),
            timestamp: 12.5,
        };
        xr.inject_pose(pose);

        let got = xr.current_pose().expect("pose after injection");
        assert!((got.position - pose.position).length() < 1e-6);
        assert!(got.orientation.angle_between(pose.orientation) < 1e-6);
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn identity_pawn_leaves_poses_unchanged() {
        use crate::input::ControllerPose;
        use glam::{Quat, Vec3};

        let mut xr = XrPlatform::new();
        assert_eq!(xr.pawn(), Pawn::IDENTITY);

        let hmd = HmdPose {
            position: Vec3::new(0.1, 1.7, -0.3),
            orientation: Quat::from_rotation_y(0.5),
            timestamp: 1.0,
        };
        xr.inject_pose(hmd);
        let left = ControllerPose {
            position: Vec3::new(0.3, 1.2, -0.4),
            orientation: Quat::IDENTITY,
            timestamp: 1.0,
        };
        xr.inject_controller_poses(Some(left), None);

        let got_h = xr.current_pose().expect("hmd");
        assert!((got_h.position - hmd.position).length() < 1e-6);
        let (got_l, got_r) = xr.controller_poses();
        assert!((got_l.expect("left").position - left.position).length() < 1e-6);
        assert!(got_r.is_none());
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn pawn_maps_hmd_and_both_controllers_into_one_rigid_world_frame() {
        use crate::input::ControllerPose;
        use glam::{Quat, Vec3};
        use std::f32::consts::FRAC_PI_2;

        let mut xr = XrPlatform::new();

        // Stage-space poses: HMD ahead, controllers symmetric about it.
        let hmd = HmdPose {
            position: Vec3::new(0.0, 1.6, 0.0),
            orientation: Quat::IDENTITY,
            timestamp: 2.0,
        };
        let left = ControllerPose {
            position: Vec3::new(-0.3, 1.2, -0.4),
            orientation: Quat::IDENTITY,
            timestamp: 2.0,
        };
        let right = ControllerPose {
            position: Vec3::new(0.3, 1.2, -0.4),
            orientation: Quat::IDENTITY,
            timestamp: 2.0,
        };
        xr.inject_pose(hmd);
        xr.inject_controller_poses(Some(left), Some(right));
        xr.inject_aim_poses(Some(left), Some(right));

        // Move the whole rig: +10 on X, then a +90° yaw about world up.
        let origin = Vec3::new(10.0, 0.0, 0.0);
        xr.set_pawn_origin_yaw(origin, FRAC_PI_2);
        let pawn = Pawn::from_origin_yaw(origin, FRAC_PI_2);

        let got_h = xr.current_pose().expect("hmd");
        let (got_l, got_r) = xr.controller_poses();
        let got_l = got_l.expect("left");
        let got_r = got_r.expect("right");

        // Each pose equals the same pawn applied independently.
        assert!((got_h.position - pawn.apply_hmd(&hmd).position).length() < 1e-5);
        assert!((got_l.position - pawn.apply_controller(&left).position).length() < 1e-5);
        assert!((got_r.position - pawn.apply_controller(&right).position).length() < 1e-5);

        // Rigidity: the left→right separation is length-preserving under the
        // shared pawn (the rig is not sheared, and no controller is HMD-parented).
        let sep_stage = right.position - left.position;
        let sep_world = got_r.position - got_l.position;
        assert!((sep_world.length() - sep_stage.length()).abs() < 1e-5);

        // Aim poses ride the same frame.
        let (aim_l, _aim_r) = xr.aim_poses();
        assert!((aim_l.expect("aim left").position - got_l.position).length() < 1e-5);
    }
}
