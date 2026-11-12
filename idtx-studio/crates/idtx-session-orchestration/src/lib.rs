#![recursion_limit = "512"]
//! UI-neutral multi-session orchestration for the per-level-assembly level
//! transition (`docs/level-transition-runtime-plan.md`).
//!
//! Extracted from `idtx-runtime-core` so the winit-free visionOS runtime can
//! reuse the exact same transition state machine (that crate pulls `winit` via
//! `idtx-platform-desktop`, which has no visionOS backend). This crate depends
//! only on [`idtx_studio_session`], so it links on every runtime target.
//!
//! - [`SessionSet`] — the live level plus an optional resident transfer/mask
//!   session, and which one the frame loop ticks + renders.
//! - [`StageLoader`] — a non-blocking background open of the next level's
//!   assembly on a worker thread.
//! - [`TransitionDriver`] — the state machine that services a behavior-posted
//!   transition: cut to the mask, load the target, place the player, swap. It
//!   is independent of any `RunConfig`; the embedding runtime supplies a
//!   [`SessionConfigure`] closure to set up each swapped-in session. Desktop /
//!   Quest build one via `idtx_runtime_core::build_transition_driver`; visionOS
//!   wires its own.
//! - [`GlobalState`] — experience state carried across a swap (the driver owns
//!   it, being the one object that spans a `set_current`).

pub mod global_state;
#[cfg(feature = "metrics")]
pub mod frame_metrics;
pub mod runtime;
pub mod session_set;
pub mod stage_loader;
pub mod transition;

pub use global_state::GlobalState;
pub use runtime::BundleContext;
pub use session_set::{ActiveSession, SessionSet};
pub use stage_loader::{StageLoadError, StageLoader, LoadPhase, LoadPreparation, PrepareSession};
pub use transition::{LevelTable, SessionConfigure, TransitionDriver, TransitionStatus};

/// Surfaceless wgpu device/queue for the headless tests in this crate's modules
/// (they open `StudioSession`s and warm a frame). Mirrors
/// `idtx-runtime-core`'s own `headless_device`; kept here so the moved tests
/// stay self-contained. `None` when no adapter is available.
#[cfg(test)]
pub(crate) fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-orchestration-headless"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .ok()?;
        Some((device, queue))
    })
}
