//! `OpenXR` lifecycle, stereo-view, input, and frame-budget contracts.
//!
//! A safe, typed abstraction over the `OpenXR` session lifecycle and HMD
//! pose that the rest of the stack depends on without touching raw XR
//! types. Built on top of the [`idtx_openxr_sys`] loader shim.
//!
//! # Feature gating
//!
//! * `pcvr` — the real `OpenXR` path (the graphics-bound
//!   session + real pose location).
//! * `mock-xr` — a deterministic mock: [`hmd::HmdTracker::inject_pose`]
//!   feeds a programmable pose so the lifecycle + pose plumbing is
//!   testable headlessly.
//! * Without `pcvr`, [`hmd::HmdTracker::current_pose`] returns `None`
//!   (desktop-only mode).

pub mod backend;
pub mod frame;
pub mod hand;
#[cfg(feature = "gesture-recognition")]
pub mod gesture;
pub mod hmd;
pub mod input;
pub mod lifecycle;
pub mod locomotion;
pub mod pawn;
pub mod view;

pub use backend::{Hand, XrBackend};
pub use frame::{XrFrameState, XrTime};
pub use hand::{
    HandJoint, HandPose, HandTracker, HAND_JOINT_COUNT, HAND_JOINT_PARENTS,
};
// `gesture::Hand` mirrors `backend::Hand`; the backend one stays canonical at
// the crate root, so it is not re-exported here (reach it as `gesture::Hand`).
#[cfg(feature = "gesture-recognition")]
pub use gesture::{
    GestureConfig, GestureEvent, GestureKind, GestureLink, GesturePhase, GestureWorker,
    HandSnapshot, PointDiagnostics, SimpleGestureConfig, point_diagnostics, thumb_index_cos,
    thumb_index_gap,
};
pub use hmd::HmdTracker;
pub use input::{
    ControllerButtons, ControllerInputTracker, ControllerPose, ControllerTracker, XrInputSource,
};
pub use lifecycle::{XrPlatform, XrSessionState, XrStateError};
pub use locomotion::{Locomotor, SmoothMoveConfig, SnapTurnConfig, TurnDirection, teleport_arc};
pub use pawn::Pawn;
pub use view::{Fov, HmdPose, StereoViews, XrView};

/// Re-export the loader error surface for convenience.
pub use idtx_openxr_sys::XrError;
