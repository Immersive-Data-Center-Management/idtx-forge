//! XR hand-joint tracker.
//!
//! [`HandTracker`] holds the most recently located poses for both hands from
//! `XR_EXT_hand_tracking`. Each hand is represented as 26 [`HandJoint`]
//! values following the fixed parent hierarchy defined by the extension spec.
//!
//! The tracker is a pure data store: the actual `xrLocateHandJointsEXT` call
//! lives in `idtx-openxr-sys` (pcvr path) and the located joints are fed here
//! via [`HandTracker::set_poses`] once per frame from `idtx-studio-session`.
//!
//! # Feature gating
//!
//! The `hand-tracking` feature in this crate (and `idtx-openxr-sys`) enables:
//! - Requesting `XR_EXT_hand_tracking` from the instance
//! - Per-frame joint location in `XrGraphicsSession::begin_frame()`
//! - [`HandTracker::mark_supported`] being callable
//!
//! Without the feature the tracker always reports `is_supported() == false`.
//! On the `mock-xr` path [`HandTracker::inject_poses`] can exercise the full
//! rendering pipeline without a headset.

use glam::{Quat, Vec3};

/// Number of joints per hand per the `XR_EXT_hand_tracking` spec.
pub const HAND_JOINT_COUNT: usize = 26;

/// Parent joint for each joint index (None = root).
///
/// Indices match the `XR_HAND_JOINT_*_EXT` ordinals (0–25). The array defines
/// the 25 bone segments between consecutive joints; the root (PALM, index 0)
/// has no parent. Iterating this table and skipping `None` entries yields all
/// drawable bone segments.
pub const HAND_JOINT_PARENTS: [Option<usize>; HAND_JOINT_COUNT] = [
    None,     // 0  PALM (root)
    Some(0),  // 1  WRIST
    Some(1),  // 2  THUMB_METACARPAL
    Some(2),  // 3  THUMB_PROXIMAL
    Some(3),  // 4  THUMB_DISTAL
    Some(4),  // 5  THUMB_TIP
    Some(1),  // 6  INDEX_METACARPAL
    Some(6),  // 7  INDEX_PROXIMAL
    Some(7),  // 8  INDEX_INTERMEDIATE
    Some(8),  // 9  INDEX_DISTAL
    Some(9),  // 10 INDEX_TIP
    Some(1),  // 11 MIDDLE_METACARPAL
    Some(11), // 12 MIDDLE_PROXIMAL
    Some(12), // 13 MIDDLE_INTERMEDIATE
    Some(13), // 14 MIDDLE_DISTAL
    Some(14), // 15 MIDDLE_TIP
    Some(1),  // 16 RING_METACARPAL
    Some(16), // 17 RING_PROXIMAL
    Some(17), // 18 RING_INTERMEDIATE
    Some(18), // 19 RING_DISTAL
    Some(19), // 20 RING_TIP
    Some(1),  // 21 LITTLE_METACARPAL
    Some(21), // 22 LITTLE_PROXIMAL
    Some(22), // 23 LITTLE_INTERMEDIATE
    Some(23), // 24 LITTLE_DISTAL
    Some(24), // 25 LITTLE_TIP
];

/// A single located hand joint.
///
/// Position is in STAGE (world) space. Orientation is a unit quaternion
/// aligning the joint's local frame to STAGE space. Radius is the runtime's
/// estimate of the joint's anatomical size in metres (e.g. ~3 mm for fingertip,
/// ~10 mm for wrist), used to scale the joint sphere in the renderer.
#[derive(Debug, Clone, Copy)]
pub struct HandJoint {
    /// Position in metres, STAGE space.
    pub position: Vec3,
    /// Orientation (unit quaternion), STAGE space.
    pub orientation: Quat,
    /// Runtime-provided anatomical radius in metres. Always > 0.
    pub radius: f32,
}

impl Default for HandJoint {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            radius: 0.005, // 5 mm fallback
        }
    }
}

/// A fully located 26-joint hand pose.
#[derive(Debug, Clone)]
pub struct HandPose {
    /// All 26 joints; indices match `XR_HAND_JOINT_*_EXT` ordinals.
    pub joints: [HandJoint; HAND_JOINT_COUNT],
    /// Predicted display timestamp (seconds) this pose was located at.
    pub timestamp: f64,
}

impl HandPose {
    /// Construct a pose where every joint sits at the origin with zero radius.
    /// Useful for tests that want to override specific joints.
    #[must_use]
    pub fn zeroed() -> Self {
        Self {
            joints: [HandJoint::default(); HAND_JOINT_COUNT],
            timestamp: 0.0,
        }
    }
}

/// Stores the most recently located hand poses for both hands.
///
/// Updated once per frame from `idtx-studio-session` (pcvr path) or from a
/// test injector (`mock-xr` path). Either or both poses may be `None` when
/// the corresponding hand is untracked or when hand tracking is not supported
/// by the connected device.
#[derive(Debug, Default)]
pub struct HandTracker {
    /// `true` once the runtime confirmed `supportsHandTracking == XR_TRUE`.
    supported: bool,
    left: Option<HandPose>,
    right: Option<HandPose>,
}

impl HandTracker {
    /// Create an empty, unsupported tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the tracker as supported (called after a successful
    /// `xrGetSystemProperties` probe on the pcvr path).
    ///
    /// Without this call `is_supported()` returns `false` and the renderer
    /// suppresses hand skeleton injection.
    pub fn mark_supported(&mut self) {
        self.supported = true;
    }

    /// Whether the connected device confirmed `XR_EXT_hand_tracking` support.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.supported
    }

    /// Replace both hand poses atomically (called once per frame from the
    /// graphics session after `xrLocateHandJointsEXT`).
    pub fn set_poses(&mut self, left: Option<HandPose>, right: Option<HandPose>) {
        self.left = left;
        self.right = right;
    }

    /// The latest located left-hand pose, or `None` when untracked.
    #[must_use]
    pub fn left(&self) -> Option<&HandPose> {
        self.left.as_ref()
    }

    /// The latest located right-hand pose, or `None` when untracked.
    #[must_use]
    pub fn right(&self) -> Option<&HandPose> {
        self.right.as_ref()
    }

    /// Both hand poses as `(left, right)`. Mirrors [`XrPlatform::hand_poses`].
    #[must_use]
    pub fn poses(&self) -> (Option<&HandPose>, Option<&HandPose>) {
        (self.left.as_ref(), self.right.as_ref())
    }
}

/// Inject mock hand poses (tests / headless CI).
#[cfg(feature = "mock-xr")]
impl HandTracker {
    /// Inject both hand poses for the next frame. Sets `supported = true`
    /// so the render path actually picks them up.
    pub fn inject_poses(&mut self, left: Option<HandPose>, right: Option<HandPose>) {
        self.supported = true;
        self.set_poses(left, right);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint_parent_table_has_25_edges() {
        let edge_count = HAND_JOINT_PARENTS.iter().filter(|p| p.is_some()).count();
        assert_eq!(
            edge_count, 25,
            "all joints except PALM should have a parent (got {edge_count})"
        );
    }

    #[test]
    fn all_parents_are_valid_indices() {
        for (i, &parent) in HAND_JOINT_PARENTS.iter().enumerate() {
            if let Some(p) = parent {
                assert!(
                    p < HAND_JOINT_COUNT,
                    "joint {i} has out-of-range parent {p}"
                );
                assert!(
                    p < i,
                    "joint {i} references a later joint {p} (would form a cycle)"
                );
            }
        }
    }

    #[test]
    fn tracker_starts_unsupported_and_untracked() {
        let t = HandTracker::new();
        assert!(!t.is_supported());
        assert!(t.left().is_none());
        assert!(t.right().is_none());
    }

    #[test]
    fn mark_supported_flips_flag() {
        let mut t = HandTracker::new();
        t.mark_supported();
        assert!(t.is_supported());
    }

    #[test]
    fn set_poses_round_trips() {
        let mut t = HandTracker::new();
        let mut pose = HandPose::zeroed();
        pose.joints[1].position = Vec3::new(0.0, 0.05, 0.0); // WRIST
        pose.timestamp = 1.5;
        t.set_poses(Some(pose), None);
        let got = t.left().expect("left pose after set");
        assert!((got.joints[1].position.y - 0.05).abs() < 1e-6);
        assert!(t.right().is_none());
    }

    #[test]
    fn set_poses_none_clears() {
        let mut t = HandTracker::new();
        t.set_poses(Some(HandPose::zeroed()), Some(HandPose::zeroed()));
        assert!(t.left().is_some());
        t.set_poses(None, None);
        assert!(t.left().is_none());
        assert!(t.right().is_none());
    }

    #[cfg(feature = "mock-xr")]
    #[test]
    fn inject_poses_marks_supported() {
        let mut t = HandTracker::new();
        assert!(!t.is_supported());
        t.inject_poses(Some(HandPose::zeroed()), None);
        assert!(t.is_supported());
        assert!(t.left().is_some());
        assert!(t.right().is_none());
    }
}
