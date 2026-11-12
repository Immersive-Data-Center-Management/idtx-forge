//! Apple Vision Pro XR backend (the pose/view half).
//!
//! [`CompositorServicesBackend`] is the [`XrBackend`](idtx_platform_xr::XrBackend)
//! implementor for Vision Pro. It is a **pure data store**: the Swift
//! `CompositorServices` + ARKit shell samples the runtime each frame and feeds the
//! results in through [`set_frame`](CompositorServicesBackend::set_frame) /
//! [`set_hand`](CompositorServicesBackend::set_hand) / the lifecycle setters;
//! the renderer, pawn, locomotion, and behavior layers then read the neutral
//! glam poses back through the trait, identically to the `OpenXR` path.
//!
//! It holds **no** Metal / `cp_drawable` handle — those are GPU resources that
//! never cross the [`XrBackend`] seam. The drawable-texture import + per-eye
//! render live in `idtx-studio-session::visionos_render`; the standalone runtime
//! (`apps/idtx-runtime`) owns the C-ABI that wires Swift ↔ this backend ↔ that
//! renderer. Because this crate is pure Rust it builds and unit-tests on any
//! host.
//!
//! # ARKit sources (device) and Simulator fallbacks
//!
//! * **Head pose** — ARKit `WorldTrackingProvider::queryDeviceAnchor`. The
//!   Simulator supplies a usable (simulated) device anchor, so the head pose is
//!   live there too.
//! * **Per-eye views** — the `cp_drawable` views' tangents + per-view transforms
//!   (relative to the device anchor), converted to [`StereoViews`].
//! * **Hands** — ARKit `HandTrackingProvider`. `HandTrackingProvider.isSupported`
//!   is **false** in the Simulator, so [`hand_pose`](CompositorServicesBackend::hand_pose)
//!   returns `None` there; on device the Swift shell feeds located joints via
//!   [`set_hand`](CompositorServicesBackend::set_hand).

// The docs are dense with Apple identifiers (MTLDevice, CompositorServices,
// ARKit providers, cp_drawable) that read fine unbackticked.
#![allow(clippy::doc_markdown)]

use glam::{Quat, Vec3};
use idtx_platform_xr::hand::HandPose;
use idtx_platform_xr::lifecycle::XrSessionState;
use idtx_platform_xr::view::{Fov, HmdPose, StereoViews, XrView};
use idtx_platform_xr::{Hand, XrBackend};

/// One eye's pose + field of view as fed from the Swift `cp_drawable` view.
///
/// `position`/`orientation` place the eye in STAGE space (the device anchor
/// composed with the per-view transform); the four `tan_*` are the view
/// frustum tangents `CompositorServices` reports (`drawable.views[i].tangents`),
/// matching the signs the renderer's off-axis projection expects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeViewInput {
    pub position: Vec3,
    pub orientation: Quat,
    pub tan_left: f32,
    pub tan_right: f32,
    pub tan_up: f32,
    pub tan_down: f32,
    /// Predicted presentation time (seconds) the view was computed for.
    pub timestamp: f64,
}

impl EyeViewInput {
    /// Convert to the neutral [`XrView`]. The [`Fov`] stores **angles** (radians)
    /// per the `OpenXR` convention; the tangents the Swift shell supplies are
    /// `tan(angle)`, so we take the arctangent back to angles here and the
    /// renderer re-tangents them. This keeps [`Fov`] identical to the `OpenXR`
    /// path (which also carries angles).
    #[must_use]
    pub fn to_view(self) -> XrView {
        XrView {
            pose: HmdPose {
                position: self.position,
                orientation: self.orientation,
                timestamp: self.timestamp,
            },
            fov: Fov {
                angle_left: self.tan_left.atan(),
                angle_right: self.tan_right.atan(),
                angle_up: self.tan_up.atan(),
                angle_down: self.tan_down.atan(),
            },
        }
    }
}

/// The Vision Pro [`XrBackend`] — a per-frame snapshot of head pose, stereo
/// views, and (device-only) hand poses, fed by the Swift `CompositorServices`
/// shell.
#[derive(Debug, Clone)]
pub struct CompositorServicesBackend {
    state: XrSessionState,
    head: HmdPose,
    views: StereoViews,
    hands: [Option<HandPose>; 2],
    exit_requested: bool,
}

impl Default for CompositorServicesBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositorServicesBackend {
    /// A backend in the [`XrSessionState::Ready`] state with no poses yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: XrSessionState::Ready,
            head: HmdPose::default(),
            views: StereoViews::default(),
            hands: [None, None],
            exit_requested: false,
        }
    }

    /// Advance the lifecycle state (driven by the immersive-space open/close +
    /// `cp_layer_renderer` state the Swift shell reports).
    pub fn set_state(&mut self, state: XrSessionState) {
        self.state = state;
    }

    /// Feed the per-frame head pose + both eye views for the frame about to be
    /// rendered. Called once per frame from the Swift shell via the C-ABI,
    /// before the render call. Transitions a `Ready` session to `Focused` on
    /// the first frame (the Vision Pro has no separate sync/visible handshake
    /// the way `OpenXR` does — a presenting immersive space is focused).
    pub fn set_frame(&mut self, head: HmdPose, left: EyeViewInput, right: EyeViewInput) {
        self.head = head;
        self.views = StereoViews {
            left: left.to_view(),
            right: right.to_view(),
        };
        if self.state == XrSessionState::Ready {
            self.state = XrSessionState::Focused;
        }
    }

    /// Set (or clear) the located pose for one hand. The Simulator never calls
    /// this with `Some` (no `HandTrackingProvider`), so hands stay `None` there.
    pub fn set_hand(&mut self, hand: Hand, pose: Option<HandPose>) {
        self.hands[hand_index(hand)] = pose;
    }
}

/// Stable index for a hand (`Left == 0`, `Right == 1`).
#[must_use]
fn hand_index(hand: Hand) -> usize {
    match hand {
        Hand::Left => 0,
        Hand::Right => 1,
    }
}

impl XrBackend for CompositorServicesBackend {
    fn session_state(&self) -> XrSessionState {
        self.state
    }

    fn request_exit(&mut self) {
        self.exit_requested = true;
        self.state = XrSessionState::Stopping;
    }

    fn head_pose(&self) -> HmdPose {
        self.head
    }

    fn stereo_views(&self) -> StereoViews {
        self.views
    }

    fn hand_pose(&self, hand: Hand) -> Option<HandPose> {
        self.hands[hand_index(hand)].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eye(x: f32) -> EyeViewInput {
        EyeViewInput {
            position: Vec3::new(x, 1.6, 0.0),
            orientation: Quat::IDENTITY,
            // A symmetric 90°-ish frustum: tan(±45°) = ±1, tan(45°)=1.
            tan_left: -1.0,
            tan_right: 1.0,
            tan_up: 1.0,
            tan_down: -1.0,
            timestamp: 0.5,
        }
    }

    #[test]
    fn set_frame_promotes_ready_to_focused_and_stores_views() {
        let mut be = CompositorServicesBackend::new();
        assert_eq!(be.session_state(), XrSessionState::Ready);
        assert!(!be.is_running());

        let head = HmdPose {
            position: Vec3::new(0.0, 1.6, 0.0),
            orientation: Quat::IDENTITY,
            timestamp: 0.5,
        };
        be.set_frame(head, eye(-0.03), eye(0.03));

        assert_eq!(be.session_state(), XrSessionState::Focused);
        assert!(be.is_running(), "a presenting immersive space is focused");
        assert_eq!(be.head_pose().position, Vec3::new(0.0, 1.6, 0.0));
        let views = be.stereo_views();
        assert!((views.left.pose.position.x - -0.03).abs() < 1e-6);
        assert!((views.right.pose.position.x - 0.03).abs() < 1e-6);
        // tan→angle round trip: tan 1.0 → 45° → π/4.
        assert!((views.left.fov.angle_right - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    }

    #[test]
    fn hands_default_none_and_round_trip() {
        let mut be = CompositorServicesBackend::new();
        assert!(be.hand_pose(Hand::Left).is_none());
        assert!(be.hand_pose(Hand::Right).is_none());

        be.set_hand(Hand::Right, Some(HandPose::zeroed()));
        assert!(be.hand_pose(Hand::Right).is_some());
        assert!(be.hand_pose(Hand::Left).is_none(), "left untouched");
    }

    #[test]
    fn request_exit_moves_to_stopping() {
        let mut be = CompositorServicesBackend::new();
        be.set_frame(HmdPose::default(), eye(-0.03), eye(0.03));
        assert!(be.is_running());
        be.request_exit();
        assert_eq!(be.session_state(), XrSessionState::Stopping);
        assert!(!be.is_running());
    }
}
