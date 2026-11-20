//! Stereo-view + pose value types.

use glam::{Quat, Vec3};

/// A tracked pose in the STAGE reference space.
///
/// Position + orientation drive the active camera; `timestamp` is the
/// predicted-display-time (seconds) the pose was located for, so consumers
/// can reason about latency.
///
/// `Default` is the origin with an identity orientation at time zero
/// (glam's `Vec3`/`Quat` defaults are `ZERO`/`IDENTITY`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HmdPose {
    /// World-space position (STAGE space), metres.
    pub position: Vec3,
    /// World-space orientation.
    pub orientation: Quat,
    /// Predicted display time this pose was located for, in seconds.
    pub timestamp: f64,
}

/// A symmetric-or-asymmetric projection field of view, in radians.
///
/// Signs follow the `OpenXR` convention: `angle_left` / `angle_down` are
/// typically negative, `angle_right` / `angle_up` positive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Fov {
    pub angle_left: f32,
    pub angle_right: f32,
    pub angle_up: f32,
    pub angle_down: f32,
}

/// One eye's view: its pose + projection FOV.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct XrView {
    /// The eye's pose.
    pub pose: HmdPose,
    /// The eye's projection FOV.
    pub fov: Fov,
}

/// The pair of eye views for a stereo frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StereoViews {
    /// Left-eye view.
    pub left: XrView,
    /// Right-eye view.
    pub right: XrView,
}
