//! The crate a behavior author *depends on*.
//!
//! Re-exports the [`macro@behavior`] attribute macro, the safe guest
//! wrappers (`Ctx`, `Entity`, `input`, `log`, `xr`, …), and a `glam`
//! surface pinned to the workspace version so authored math crosses the
//! host boundary bit-compatibly.
//!
//! # Targets
//!
//! The SDK compiles both natively (for host-side tests, manifest
//! round-trips, and the build tooling) and to `wasm32-unknown-unknown`
//! (the real guest target). The import table is `extern "C"` on wasm and
//! a set of inert stubs natively — a natively-compiled behavior links and
//! its wrappers return defaults, so unit tests can exercise author logic
//! without a wasm host.

#![no_std]

/// The frozen wire ABI. Re-exported so generated code and authors can name
/// `idtx_behavior_sdk::abi::ABI_VERSION` etc. without a direct dep.
pub use idtx_behavior_abi as abi;

/// The `#[behavior]` attribute macro (from the proc-macro companion).
pub use idtx_behavior_macros::behavior;

pub use base64;
pub use base64::engine::general_purpose::{URL_SAFE, STANDARD};
pub use base64::Engine;
pub mod context;
pub mod ctx;
/// Contract derives and traits available to single-file behavior authors.
pub use serde;
pub use serde::{Deserialize, Serialize};
pub mod aas;
pub mod auth;
pub mod entity;
pub mod events;
pub mod playback;
pub mod input;
pub mod level;
pub mod log;
pub mod math;
pub mod net;
pub mod physics;
pub mod result;
pub mod scene;
pub mod stage_edit;
pub mod xr;

#[doc(hidden)]
pub mod __rt;

/// Everything an author needs with a single glob import.
///
/// ```ignore
/// use idtx_behavior_sdk::prelude::*;
/// ```
pub mod prelude {
    pub use crate::aas;
    pub use crate::abi::{
        BehaviorKind, ErrorCode, InputActionId, InputPhase, StageEvent, WidgetEventKind,
        XrInputSource, XrSide, XrSpace,
    };
    pub use crate::abi::{SpawnResult, SpawnStatus, SpawnTicket};
    pub use crate::behavior;
    pub use crate::context::ContextString;
    pub use crate::ctx::{CameraRay, CameraView, Ctx};
    pub use crate::entity::{Entity, Transform};
    pub use crate::level::Level;
    pub use crate::net::HttpResponse;
    pub use crate::physics::Physics;
    pub use crate::result::{BehaviorError, Result};
    pub use crate::scene::{self, ArcHit, ArcLaunch, Draw, Highlight, RayHit, RayOpts, Scene};
    pub use crate::scene::{Widget, WidgetClick, Widgets};
    pub use crate::stage_edit::AttributeConnection;
    pub use crate::stage_edit::StageEdit;
    pub use crate::xr::{
        HAND_JOINT_INDEX_PROXIMAL, HAND_JOINT_INDEX_TIP, HAND_JOINT_PALM, HAND_JOINT_THUMB_TIP,
        HAND_JOINT_WRIST, HandJoints, XrHmd,
    };
    pub use crate::{Deserialize, Serialize};
    pub use crate::{auth, events, input, log, math, net, playback}; // glam, pinned to the workspace version so wire types are invisible.
    pub use glam::{Mat4, Quat, Vec3, Vec4};
}

// Re-export the pinned glam surface directly too.
pub use glam::{Mat4, Quat, Vec3, Vec4};
pub use arrayvec;
