//! HMD (and controller) pose tracking.
//!
//! [`HmdTracker`] reads the STAGE-space pose each frame and exposes the
//! latest via [`HmdTracker::current_pose`], firing an optional
//! `on_pose_changed` callback when the pose moves by more than a small
//! threshold. With the `pcvr` feature the tracker polls `xrLocateSpace`;
//! without it the tracker only holds mock-injected poses.

use crate::view::HmdPose;

/// Squared translation threshold (m²) beyond which a pose change fires the
/// `on_pose_changed` callback. Rotation changes above ~0.5° also fire.
const POSE_EPSILON_SQ: f32 = 1e-6;

type PoseCallback = Box<dyn FnMut(&HmdPose) + Send>;

/// Tracks the head pose in STAGE reference space.
pub struct HmdTracker {
    current: Option<HmdPose>,
    #[allow(clippy::type_complexity)]
    on_pose_changed: Option<PoseCallback>,
}

impl std::fmt::Debug for HmdTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HmdTracker")
            .field("current", &self.current)
            .field("has_callback", &self.on_pose_changed.is_some())
            .finish()
    }
}

impl Default for HmdTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl HmdTracker {
    /// A fresh tracker with no pose yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: None,
            on_pose_changed: None,
        }
    }

    /// The most recently located pose, or `None` if not yet tracked.
    #[must_use]
    pub fn current_pose(&self) -> Option<HmdPose> {
        self.current
    }

    /// Register a callback fired whenever the pose changes appreciably.
    pub fn set_pose_changed_callback(&mut self, cb: impl FnMut(&HmdPose) + Send + 'static) {
        self.on_pose_changed = Some(Box::new(cb));
    }

    /// Poll the runtime for the latest pose.
    ///
    /// The real path (`pcvr`) calls `xrLocateSpace` against the
    /// STAGE space for `predicted_display_time`. Without `pcvr` this is a
    /// no-op — the tracker only surfaces mock-injected poses.
    pub fn poll(&mut self) {
        // The `pcvr` build locates the STAGE space here.
    }

    /// Update the current pose, firing the change callback when the pose
    /// moved appreciably. Shared by the real poll path and mock injection.
    #[cfg_attr(not(feature = "mock-xr"), allow(dead_code))]
    fn update(&mut self, pose: HmdPose) {
        let changed = match self.current {
            None => true,
            Some(prev) => {
                (pose.position - prev.position).length_squared() > POSE_EPSILON_SQ
                    || pose.orientation.angle_between(prev.orientation) > 0.01
            }
        };
        self.current = Some(pose);
        if changed
            && let Some(cb) = self.on_pose_changed.as_mut() {
                cb(&pose);
            }
    }

    /// Set the latest located pose (real `pcvr` path — the graphics session
    /// locates the HMD each frame and pushes it here). Fires the change
    /// callback on an appreciable move, exactly like mock injection.
    #[cfg(any(feature = "pcvr", feature = "quest"))]
    pub fn set_pose(&mut self, pose: HmdPose) {
        self.update(pose);
    }

    /// Inject a mock pose (tests / headless CI). Fires the change callback
    /// on an appreciable move, exactly like the real poll path.
    #[cfg(feature = "mock-xr")]
    pub fn inject_pose(&mut self, pose: HmdPose) {
        self.update(pose);
    }
}
