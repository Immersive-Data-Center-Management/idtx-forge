//! Shared capability-driven desktop shell for IDTX Studio products.
//!
//! Each product binary calls [`run`] with a [`ProductProfile`] constant:
//! - `idtx-inspector` → [`ProductProfile::INSPECTOR`]
//! - `idtx-poser`     → [`ProductProfile::POSER`]
//! - `idtx-conductor` → [`ProductProfile::CONDUCTOR`]

#![forbid(unsafe_code)]

mod actions;
mod app;
mod assembly;
mod authoring_ops;
// Behavior authoring + preview/cook overlay: both depend on `idtx-behavior-build`
// (the `.rs`→`.wasm` compiler), which only the `cooking` feature pulls in. Poser
// (authoring, no cooking) therefore compiles neither — its lean build has no
// behavior-build toolchain.
#[cfg(feature = "cooking")]
mod behavior_prepare;
#[cfg(feature = "cooking")]
mod behavior_tools;
#[cfg(feature = "build-tools")]
mod build_job;
#[cfg(feature = "authoring")]
mod hull_job;
mod gizmo;
#[cfg(feature = "authoring")]
mod import;
mod model;
mod player_template;
// Runtime-preview composition + process launch: both pull the cooking-only
// behavior-build overlay (and the assembly composer re-export), so the whole
// preview surface is `cooking`-gated. Poser (authoring, no cooking) has no
// Preview menu and compiles neither.
#[cfg(feature = "cooking")]
mod preview_assembly;
mod project_settings;
mod projection;
mod recent;
mod recent_projects_ui;
mod remote;
mod render;
#[cfg(feature = "cooking")]
mod runtime_launch;
#[cfg(feature = "metrics")]
mod shell_metrics;
mod tabs;
mod utils;
mod view;

pub use model::ShellError;

use std::path::PathBuf;

use idtx_editor_model::ProductProfile;
use idtx_project::IdtxProject;
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::StudioApplication;

/// How the shell should come up: which stage or project to open, and whether to
/// prompt for a project when neither is given.
///
/// The single-stage products (`idtx-inspector`, `idtx-poser`) use [`run`] with a
/// bare `initial_stage`; the Conductor uses [`run_startup`] to pass a loaded
/// [`IdtxProject`] (from its `--project` argument) or to request the startup
/// project dialog when launched with no argument.
#[derive(Default)]
pub struct Startup {
    /// An explicit stage file to open (non-project opens).
    pub initial_stage: Option<PathBuf>,
    /// A pre-loaded project whose default stage the shell should open.
    pub project: Option<IdtxProject>,
    /// When no stage or project is supplied, show the startup project dialog
    /// instead of opening an empty window. Ignored by non-project products.
    pub prompt_for_project: bool,
}

/// Launches the IDTX Studio shell with the given product profile and an optional
/// initial stage.
///
/// Blocks until the window is closed.
pub fn run(profile: ProductProfile, initial_stage: Option<PathBuf>) -> Result<(), ShellError> {
    run_startup(
        profile,
        Startup {
            initial_stage,
            ..Startup::default()
        },
    )
}

/// Launches the IDTX Studio shell with an explicit [`Startup`] configuration.
///
/// Blocks until the window is closed.
pub fn run_startup(profile: ProductProfile, startup: Startup) -> Result<(), ShellError> {
    #[cfg(feature = "usd-resolver-http")]
    remote::configure_asset_network(&startup.project.as_ref()
        .map(|p| p.manifest().network.clone()).unwrap_or_default());
    // Install the HTTP(S) USD asset resolver before any OpenUSD type is touched
    // (PXR_PLUGINPATH_NAME is read once when TfPluginRegistry initialises). This
    // is centralised here rather than duplicated across each app's `main()`
    // because every product binary funnels through `run()`. Gated behind the
    // `usd-resolver-http` feature so default builds don't require the C++ plugin.
    #[cfg(feature = "usd-resolver-http")]
    match idtx_remote::project_cache_dir(&startup.project.as_ref().map(|p| p.name()).unwrap_or_default()) {
        Ok(cache_dir) => {
            if let Err(e) = idtx_usd_resolver_http::install(&cache_dir) {
                tracing::warn!("HTTP USD resolver install failed: {e}");
            }
        }
        Err(e) => tracing::warn!("could not determine USD cache dir: {e}"),
    }

    // Register the IDTX custom OpenUSD schemas (e.g. `IdtxCollisionAPI`) before
    // any stage is opened. Like the resolver above, this must use runtime
    // `PlugRegistry::RegisterPlugins` (via `install()`), not `PXR_PLUGINPATH_NAME`,
    // and is centralised here so every product binary picks it up. Without it,
    // applying/reading the collision API fails with "unknown API schema type".
    if let Err(e) = idtx_usd_extension::install() {
        tracing::warn!("IDTX USD schema registration failed: {e}");
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = StudioApplication {
        profile,
        initial_stage: startup.initial_stage,
        project: startup.project,
        prompt_for_project: startup.prompt_for_project,
        state: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
