//! WASM behavior host (wasmtime).
//!
//! Implements the wasmtime host ([`BehaviorHost`]) with a uuid-keyed
//! module registry, per-(entity, uuid) [`wasmtime::Store`]s, a
//! capability-gated [`linker`], epoch + memory [`limits`], the
//! named-input [`input`] router, the [`events`] queue, the [`xr_snapshot`]
//! XR-pose surface, and the [`stage_access`] bridge to the live ECS
//! `World`.
//!
//! # ECS bridge
//!
//! The host reads/writes a small, decomposed transform + hierarchy + path
//! component set defined in [`ecs`] — deliberately *independent* of
//! `idtx-scene` (which depends on `idtx-usd` and would require the `OpenUSD`
//! link) so the host is testable headlessly. A session is responsible for
//! keeping those bridge components in sync with the USD projection.

pub mod analyze;
pub mod ecs;
pub mod events;
pub mod host;
pub mod input;
pub mod level_transition;
pub mod limits;
pub mod linker;
pub mod manifest;
pub mod mapping_context;
pub mod module;
pub mod overlay;
pub mod physics_control;
pub mod playback;
pub mod scene_backend;
pub mod session_edit;
pub mod spawn;
pub mod stage_access;
pub mod widget;
mod work;
pub mod xr_snapshot;
pub mod xr_sources;

/// The behavior ABI version this host speaks. A guest module's
/// `_idtx_abi_version` export must equal this for the host to instantiate
/// it.
pub use idtx_behavior_abi::ABI_VERSION;

/// Re-export the frozen wire ABI so consumers name types through
/// `idtx_behavior::abi::*`.
pub use idtx_behavior_abi as abi;

/// Re-export the stage-level lifecycle event enum so a session can
/// drive [`BehaviorHost::dispatch_stage_event`] without a direct ABI dep.
pub use idtx_behavior_abi::StageEvent;

pub use idtx_behavior_abi::PlaybackCommandKind;

pub use idtx_behavior_abi::OverlayCommandKind;

pub use events::{Event, EventQueue};
pub use host::{BehaviorHost, InstanceKey, PlayState, entity_to_wire};
pub mod experience;
pub use analyze::{AnalyzeError, LinkReport, idtx_imports, required_capabilities, verify_link};
pub use ecs::PrimBehaviorData;
pub use experience::ExperienceState;
pub use idtx_behavior_abi::{SpawnResult, SpawnStatus, SpawnTicket};
pub use input::{
    ActionSpec, Binding, FrameInput, InputMap, InputRouter, InputSnapshot, Modifier, RawInput,
    Trigger, apply_modifiers, apply_radial_deadzone,
};
pub use level_transition::{LevelTransitionQueue, LevelTransitionRequest};
pub use limits::{Limits, WorkLimits};
pub use manifest::{
    BehaviorManifest, Capabilities, InputDecl, InputKind, KNOWN_CAPABILITIES, ManifestError,
    ParamDecl, capability_label,
};
pub use mapping_context::{MappingContext, MappingContextStack};
pub use module::{CompiledModule, ExportedHooks, ModuleError};
pub use overlay::{OverlayCommand, OverlayOp, OverlayQueue};
pub use physics_control::{PhysicsControlCommand, PhysicsControlQueue};
pub use playback::{PlaybackCommand, PlaybackQueue};
pub use scene_backend::SceneBackend;
pub use scene_backend::WidgetHitInfo;
pub mod input_capture;
pub use session_edit::{AttributeConnection, SessionEditCommand, SessionEditOp, SessionEditQueue};
pub use stage_access::{HostState, WorldScope, resolve_live, to_wire};
pub use widget::{CurrentWidgetEvent, PendingWidgetEvent, WidgetInteraction, WidgetMarker};
#[cfg(feature = "metrics")]
pub use work::DispatchMetrics;
pub use xr_snapshot::XrSnapshot;
pub use xr_sources::{
    XrControllerSnapshot, XrHandInput, apply_xr_controllers, canonicalize_source,
};

/// The host-side error surface.
pub use module::ModuleError as BehaviorError;
