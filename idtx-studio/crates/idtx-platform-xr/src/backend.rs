//! The [`XrBackend`] abstraction.
//!
//! A session-level seam over the two XR runtimes IDTX Studio targets:
//!
//! * **`OpenXR`** — Quest standalone / PC-VR (the existing path in
//!   `idtx-openxr-sys` + the feature-gated session in `idtx-studio-session`).
//! * **Apple `CompositorServices` + ARKit** — Vision Pro (the new path in
//!   `idtx-platform-visionos`).
//!
//! The glam value types this crate already defines — [`HmdPose`],
//! [`StereoViews`], [`XrView`](crate::view::XrView), [`Fov`](crate::view::Fov),
//! [`HandPose`], and the lifecycle [`XrSessionState`] — are the **shared
//! surface above** this trait, unchanged. A backend produces those neutral
//! types; it never leaks a runtime handle (an `XrSession`, a `cp_drawable`) past
//! this boundary. The renderer, pawn, locomotion, and behavior layers consume
//! the trait, so they are identical across runtimes.
//!
//! The trait is deliberately minimal: lifecycle state, the per-frame head pose
//! and stereo views the renderer needs, and the optional hand poses the input
//! layer feeds to behaviors. Graphics-API specifics (importing a drawable
//! texture, submitting a frame) stay in the GPU layer and never cross this
//! trait — a backend only has to *describe* the frame, not render it.

// Docs reference OpenXR / CompositorServices / cp_drawable identifiers that read
// fine unbackticked.
#![allow(clippy::doc_markdown)]

use crate::hand::HandPose;
use crate::lifecycle::XrSessionState;
use crate::view::{HmdPose, StereoViews};

/// Which hand a [`HandPose`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    /// The user's left hand.
    Left,
    /// The user's right hand.
    Right,
}

/// A session-level XR backend: the source of lifecycle transitions, per-frame
/// stereo views, and head / hand poses, independent of the underlying runtime.
///
/// Implementors: `idtx_platform_visionos::CompositorServicesBackend` (Vision
/// Pro). The Quest / PC-VR path is reached through its own feature-gated types.
pub trait XrBackend {
    /// The current session lifecycle state.
    fn session_state(&self) -> XrSessionState;

    /// Whether the session is actively presenting frames (synchronised,
    /// visible, or focused). Default-derived from [`Self::session_state`].
    fn is_running(&self) -> bool {
        matches!(
            self.session_state(),
            XrSessionState::Synchronized | XrSessionState::Visible | XrSessionState::Focused
        )
    }

    /// Request the session wind down (the compositor stops presenting and the
    /// state machine transitions toward [`XrSessionState::Idle`]).
    fn request_exit(&mut self);

    /// The predicted head pose for the frame currently being rendered, in STAGE
    /// space.
    fn head_pose(&self) -> HmdPose;

    /// The per-eye poses + projection FOV for the frame currently being
    /// rendered. The renderer builds one off-axis camera per eye from these.
    fn stereo_views(&self) -> StereoViews;

    /// The latest located pose for `hand`, or `None` when hand tracking is
    /// unsupported or unavailable for this frame — e.g. the visionOS Simulator,
    /// which exposes no ARKit `HandTrackingProvider`.
    fn hand_pose(&self, hand: Hand) -> Option<HandPose>;
}
