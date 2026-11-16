//! Application bootstrap and window lifecycle.
//!
//! Holds [`ApplicationState`] (the whole shell's mutable state) and
//! [`StudioApplication`] (the winit [`ApplicationHandler`]). Owns construction
//! (`ApplicationState::new`) and event dispatch; per-frame work lives in the
//! sibling `render`/`tabs`/`actions` modules.

use std::path::PathBuf;
use std::sync::Arc;

use idtx_editor_model::ProductProfile;
use idtx_editor_ui::inspector::InspectorRegistry;
use idtx_editor_ui::remote_browser::AppConfigState;
#[cfg(feature = "remote")]
use idtx_editor_ui::remote_browser::RemoteBrowserState;
use idtx_platform_desktop::DesktopGpu;
use idtx_project::IdtxProject;
use idtx_studio_session::RenderWorldUnitScale;
use num_traits::ToPrimitive as _;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

#[cfg(feature = "authoring")]
use crate::model::RenameDialog;
#[cfg(feature = "authoring")]
use crate::model::SaveConfirmInfo;
use crate::model::{LoadingTab, SessionTab, ShellError, StartupProjectDialog};
#[cfg(feature = "cooking")]
use crate::runtime_launch::PreviewProcess;
#[cfg(feature = "metrics")]
use crate::shell_metrics;
use crate::utils;

pub(crate) struct StudioApplication {
    pub(crate) profile: ProductProfile,
    pub(crate) initial_stage: Option<PathBuf>,
    /// A pre-loaded project (Conductor `--project`); opened on window creation.
    pub(crate) project: Option<IdtxProject>,
    /// Show the startup project dialog when no stage/project is supplied.
    pub(crate) prompt_for_project: bool,
    pub(crate) state: Option<ApplicationState>,
}

impl ApplicationHandler for StudioApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.profile.title())
            .with_inner_size(LogicalSize::new(1440.0_f64, 900.0_f64))
            .with_min_inner_size(LogicalSize::new(900.0_f64, 600.0_f64));
        let attrs = match utils::decode_window_icon(self.profile.window_icon) {
            Some(icon) => attrs.with_window_icon(Some(icon)),
            None => attrs,
        };
        let result = event_loop
            .create_window(attrs)
            .map_err(ShellError::from)
            .and_then(|window| {
                pollster::block_on(ApplicationState::new(
                    Arc::new(window),
                    self.profile,
                    self.initial_stage.take(),
                    self.project.take(),
                    self.prompt_for_project,
                ))
            });
        match result {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(err) => {
                eprintln!("idtx-studio: {err}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if window_id != state.window.id() {
            return;
        }
        let egui_response = state.egui_state.on_window_event(&state.window, &event);
        let _egui_consumed = egui_response.consumed;
        match event {
            WindowEvent::CloseRequested => {
                // Authoring builds intercept the close to prompt for unsaved
                // root-layer edits; a clean workspace (or non-authoring
                // Inspector) exits immediately.
                #[cfg(feature = "authoring")]
                {
                    if state.begin_app_close() {
                        event_loop.exit();
                    } else {
                        state.window.request_redraw();
                    }
                }
                #[cfg(not(feature = "authoring"))]
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                state.gpu.resize(size.width, size.height);
                state.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => state.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                // Global app shortcuts (⌘Z, ⌘O, ⌘W, ⌘S …) must fire even when
                // egui has consumed the event for its own text-editing handling.
                state.handle_shortcut(&event);
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Err(err) = state.render() {
                    state.status_message = Some(err.to_string());
                }
                // A save-on-close flow that resolved every dirty stage this
                // frame requests the actual window close here.
                if state.should_exit {
                    event_loop.exit();
                }
            }
            _ if egui_response.repaint => state.window.request_redraw(),
            _ => {}
        }
    }
}

pub(crate) struct ApplicationState {
    pub(crate) profile: ProductProfile,
    pub(crate) window: Arc<Window>,
    pub(crate) gpu: DesktopGpu,
    pub(crate) egui_ctx: egui::Context,
    pub(crate) egui_state: egui_winit::State,
    pub(crate) egui_renderer: egui_wgpu::Renderer,
    pub(crate) tabs: Vec<SessionTab>,
    /// In-flight background loads; polled each frame.
    pub(crate) loading_tabs: Vec<LoadingTab>,
    pub(crate) active_tab_id: Option<u64>,
    pub(crate) last_open_dir: Option<PathBuf>,
    /// Directory of the most recent successful "Save As…" — used as the default
    /// location the next time Save As opens, and never the remote cache dir.
    pub(crate) last_save_dir: Option<PathBuf>,
    /// Saves confirmed via the overwrite dialog but not yet executed. A save is
    /// a multi-second, UI-thread-blocking `OpenUSD` operation; running it inside
    /// the egui closure (while a swapchain image is held between
    /// `get_current_texture` and `present`) starves the compositor and wedges
    /// the next frame's surface acquire. Deferring to the top of the next
    /// frame — before any surface texture is acquired — avoids that.
    #[cfg(feature = "authoring")]
    pub(crate) pending_saves: Vec<(u64, PathBuf)>,
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) collab_service_cursor: usize,
    #[cfg(feature = "remote")]
    pub(crate) remote_job: Option<std::sync::mpsc::Receiver<crate::remote::RemoteCompletion>>,
    #[cfg(feature = "remote")]
    pub(crate) remote_generation: u64,
    /// Most-recently-opened **local** stage paths (newest first, max
    /// [`crate::recent::MAX_RECENT`]), backing the File → Open Recent… submenu.
    /// Loaded at startup and persisted to `~/.idtx-studio/recent.toml`. Remote
    /// stages are never recorded here.
    pub(crate) recent_files: Vec<PathBuf>,
    /// Most-recently-activated project-manifest (`project.toml`) paths (newest
    /// first, max [`crate::recent::MAX_RECENT`]), backing the Conductor startup
    /// dialog's recent-projects list. Loaded at startup and persisted alongside
    /// `recent_files` in `~/.idtx-studio/recent.toml`.
    pub(crate) recent_projects: Vec<PathBuf>,
    pub(crate) status_message: Option<String>,
    pub(crate) modifiers: ModifiersState,
    /// Whether the viewport panel is hovered (used for scroll passthrough).
    pub(crate) viewport_hovered: bool,
    /// Render world unit scale applied to all newly opened stages.
    /// One renderer unit = `world_unit_scale.metres_per_unit()` real metres.
    /// Default: [`RenderWorldUnitScale::METRE`] (1 unit = 1 metre).
    pub(crate) world_unit_scale: RenderWorldUnitScale,
    /// The running out-of-process runtime-preview player, if any. Holds the
    /// child process and (when the stage was dirty) a temp-file guard that is
    /// deleted after the process exits. `None` when no preview is running.
    /// Runtime preview is a `cooking`-only surface (Conductor).
    #[cfg(feature = "cooking")]
    pub(crate) preview_process: Option<PreviewProcess>,
    /// Inspector panel contribution registry; shared across all tabs.
    pub(crate) inspector_registry: Arc<InspectorRegistry>,
    /// Per-frame metrics collector; only present when the `metrics` feature is enabled.
    #[cfg(feature = "metrics")]
    pub(crate) shell_metrics: Option<shell_metrics::ShellMetrics>,
    /// State of the "overwrite?" save confirmation dialog. `Some` while the
    /// dialog is visible; `None` otherwise.
    #[cfg(feature = "authoring")]
    pub(crate) pending_save_confirm: Option<SaveConfirmInfo>,
    /// In-progress save-on-close flow. `Some` while a close request (tab close
    /// or app exit) is blocked on one or more dirty stages; drives the
    /// save-on-close modal and the deferred exit. `None` when nothing is closing.
    #[cfg(feature = "authoring")]
    pub(crate) pending_close: Option<crate::model::PendingClose>,
    /// Set once a save-on-close flow for a whole-app exit has resolved every
    /// dirty stage; the winit handler calls `event_loop.exit()` after the next
    /// frame. Unused (always `false`) in non-authoring builds.
    pub(crate) should_exit: bool,
    /// State of the scene-tree "Rename…" dialog. `Some` while visible.
    #[cfg(feature = "authoring")]
    pub(crate) rename_dialog: Option<RenameDialog>,
    /// State of the scene-tree drop target picker. `Some` while visible.
    #[cfg(feature = "authoring")]
    pub(crate) drop_target_picker: Option<crate::model::DropTargetPickerDialog>,
    /// Servers loaded at startup from `~/.idtx-studio/config.toml`. Maintained
    /// as a list so the UI can target any of them per remote action.
    #[cfg(feature = "remote")]
    pub(crate) servers: Vec<idtx_remote::ServerConfig>,
    /// Index into [`Self::servers`] of the server the remote dialogs/browsers
    /// currently target. Set when the user picks a server from a remote menu
    /// entry. `None` when no server is configured.
    #[cfg(feature = "remote")]
    pub(crate) active_server: Option<usize>,
    /// Bearer tokens obtained by successful logins, keyed by server origin.
    /// In-memory only.
    #[cfg(feature = "remote")]
    pub(crate) bearer_tokens: std::collections::HashMap<String, String>,
    /// App configuration dialog state; `Some` while the dialog is open.
    pub(crate) configure_dialog: Option<AppConfigState>,
    /// Project Settings dialog state; `Some` while the "Project ▸ Settings…"
    /// modal is open. Edits are discarded on Cancel and applied to the active
    /// project's manifest on Save.
    pub(crate) project_settings_dialog:
        Option<idtx_editor_ui::project_settings::ProjectSettingsState>,
    /// Remote file browser state; `Some` while the browser window is visible.
    #[cfg(feature = "remote")]
    pub(crate) remote_browser: Option<RemoteBrowserState>,
    /// `true` while the remote browser was opened via "Open Remote with Sync…".
    /// Cleared when the browser is dismissed or a normal open is dispatched.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) remote_browser_sync_mode: bool,
    /// Session mode to request when `remote_browser_sync_mode` is `true`.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) remote_session_mode: idtx_collab::session::SessionMode,
    /// Session browser state; `Some` while the "Connect Remote with Sync…"
    /// window is visible.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) session_browser: Option<idtx_editor_ui::remote_browser::SessionBrowserState>,
    /// Set while the remote file browser is open as the "Create Child → Asset…"
    /// picker (vs. a plain Open Remote): carries the target tab + parent prim and
    /// the current stage's server path, so a picked file becomes a `CreatePrim`
    /// reference request instead of opening a new stage. Cleared when the request
    /// is dispatched or the browser is dismissed.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) asset_create_target: Option<AssetCreateContext>,
    /// Timestamp of the previous frame — used to compute `dt` for animation
    /// clock advancement.  `None` until the second frame.
    pub(crate) last_frame_time: Option<std::time::Instant>,
    /// The active Conductor project, if one is loaded. Carries the project root
    /// (for project-relative resolution) and manifest. `None` for the
    /// single-stage products and for a Conductor with no project chosen yet.
    pub(crate) active_project: Option<IdtxProject>,
    /// Cached `(action, kind)` pairs parsed from the active project's
    /// `input_map.toml`, offered in the behavior inspector's input-actions "add"
    /// combo (each action addable once). Rebuilt when the project root changes;
    /// empty with no project or no map.
    pub(crate) behavior_input_actions: Vec<(String, String)>,
    /// Cache key the [`Self::behavior_input_actions`] list was built for: the
    /// project root string paired with the `input_map.toml` file signature
    /// (length + modified time). Tracking the file signature — not just the
    /// root — means creating or editing the map while the project stays open
    /// refreshes the list instead of serving a stale one. `None` with no
    /// project; the inner signature is `None` when the map file is absent.
    #[allow(clippy::type_complexity)]
    pub(crate) behavior_input_actions_key:
        Option<(String, Option<(u64, Option<std::time::SystemTime>)>)>,
    /// Startup "open a project" dialog state; `Some` while the modal is visible.
    pub(crate) startup_dialog: Option<StartupProjectDialog>,
    /// Cached level-browser rows for the active project, rebuilt when the
    /// manifest changes.
    pub(crate) level_cache: crate::model::LevelRegistryCache,
    /// Project-relative directory the asset browser is viewing (`""` = root),
    /// with `/` separators.
    pub(crate) asset_browser_subdir: String,
    /// Asset-browser panel selection state (which row is highlighted).
    pub(crate) asset_browser_state: idtx_editor_ui::asset_browser::AssetBrowserState,
    /// In-flight background cook + package started from the "Build" menu; `None`
    /// when no build is running. Polled once per frame.
    #[cfg(feature = "build-tools")]
    pub(crate) build_job: Option<crate::build_job::BuildJob>,
    /// Whether a Vision Pro is currently paired/connected (refreshed by a
    /// background `devicectl` poller). Gates the Build menu's "Build and deploy"
    /// entry. Always `false` off macOS (no `devicectl`).
    #[cfg(feature = "build-tools")]
    pub(crate) visionos_device_connected: Arc<std::sync::atomic::AtomicBool>,
    /// In-flight Conductor "Import" packaging job (spec case 3); `None` when no
    /// import is packaging. Polled once per frame, drives the import modal.
    #[cfg(feature = "authoring")]
    pub(crate) import_job: Option<crate::import::ImportJob>,
    /// In-flight collision convex-hull generation (collision inspector's
    /// "Generate CollisionHull"); `None` when idle. Polled once per frame; the
    /// inspector shows a spinner for the owning prim while it runs.
    #[cfg(feature = "authoring")]
    pub(crate) hull_job: Option<crate::hull_job::HullJob>,
}

/// Context for the "Create Child → Asset…" flow: captured when the user picks
/// the menu entry and consumed when they pick a file in the remote browser. See
/// [`ApplicationState::asset_create_target`].
#[cfg(all(feature = "remote", feature = "authoring"))]
#[derive(Debug, Clone)]
pub(crate) struct AssetCreateContext {
    /// Tab whose session should receive the reference-create request.
    pub(crate) tab_id: u64,
    /// Parent prim the new reference child is created under.
    pub(crate) parent_path: String,
    /// The current stage's server path (e.g. `demo/scene.usda`), used to make
    /// the picked asset path relative, and to exclude the stage from the picker.
    pub(crate) current_stage_filepath: String,
}

impl ApplicationState {
    async fn new(
        window: Arc<Window>,
        profile: ProductProfile,
        initial_stage: Option<PathBuf>,
        project: Option<IdtxProject>,
        prompt_for_project: bool,
    ) -> Result<Self, ShellError> {
        // OpenXR now runs in the out-of-process `idtx-runtime` player (spawned
        // for "Preview in Runtime"), so the editor no longer probes for a
        // runtime, owns an XR instance, or pins the GPU adapter by LUID.
        let preferred_luid: Option<(u32, i32)> = None;

        let gpu = DesktopGpu::new(window.clone(), preferred_luid).await?;
        let egui_ctx = egui::Context::default();
        egui_extras::install_image_loaders(&egui_ctx);
        // Install the editor-wide theme: fonts, sizes, colours and widget
        // geometry all flow from `idtx_editor_ui::theme::Theme`. Edit that one
        // definition to restyle the whole UI.
        idtx_editor_ui::theme::install(idtx_editor_ui::theme::Theme::dark(), &egui_ctx);
        // Request redraws on animation/repaint requests from egui widgets.
        let repaint_window = window.clone();
        egui_ctx.set_request_repaint_callback(move |info| {
            if info.delay.is_zero() {
                repaint_window.request_redraw();
            }
        });
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui_ctx.viewport_id(),
            window.as_ref(),
            Some(window.scale_factor().to_f32().unwrap_or(1.0)),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.configuration.format,
            egui_wgpu::RendererOptions::default(),
        );
        #[cfg(feature = "remote")]
        let servers = idtx_remote::load_configs().unwrap_or_default();
        #[cfg(feature = "remote")]
        let active_server = (!servers.is_empty()).then_some(0);
        let mut state = Self {
            profile,
            window,
            gpu,
            egui_ctx,
            egui_state,
            egui_renderer,
            tabs: Vec::new(),
            loading_tabs: Vec::new(),
            active_tab_id: None,
            last_open_dir: None,
            last_save_dir: None,
            #[cfg(feature = "authoring")]
            pending_saves: Vec::new(),
            #[cfg(all(feature = "remote", feature = "authoring"))]
            collab_service_cursor: 0,
            #[cfg(feature = "remote")]
            remote_job: None,
            #[cfg(feature = "remote")]
            remote_generation: 0,
            recent_files: crate::recent::load_recent(),
            recent_projects: crate::recent::load_recent_projects(),
            status_message: None,
            modifiers: ModifiersState::default(),
            viewport_hovered: false,
            world_unit_scale: RenderWorldUnitScale::METRE,
            #[cfg(feature = "cooking")]
            preview_process: None,
            inspector_registry: Arc::new(InspectorRegistry::with_builtins()),
            #[cfg(feature = "metrics")]
            shell_metrics: None,
            #[cfg(feature = "authoring")]
            pending_save_confirm: None,
            #[cfg(feature = "authoring")]
            pending_close: None,
            should_exit: false,
            #[cfg(feature = "authoring")]
            rename_dialog: None,
            #[cfg(feature = "authoring")]
            drop_target_picker: None,
            #[cfg(feature = "remote")]
            servers,
            #[cfg(feature = "remote")]
            active_server,
            #[cfg(feature = "remote")]
            bearer_tokens: std::collections::HashMap::new(),
            configure_dialog: None,
            project_settings_dialog: None,
            #[cfg(feature = "remote")]
            remote_browser: None,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            remote_browser_sync_mode: false,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            remote_session_mode: idtx_collab::session::SessionMode::SingleEdit,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            session_browser: None,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            asset_create_target: None,
            last_frame_time: None,
            active_project: None,
            behavior_input_actions: Vec::new(),
            behavior_input_actions_key: None,
            startup_dialog: None,
            level_cache: crate::model::LevelRegistryCache::default(),
            asset_browser_subdir: String::new(),
            asset_browser_state: idtx_editor_ui::asset_browser::AssetBrowserState::default(),
            #[cfg(feature = "build-tools")]
            build_job: None,
            #[cfg(feature = "build-tools")]
            visionos_device_connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(feature = "authoring")]
            import_job: None,
            #[cfg(feature = "authoring")]
            hull_job: None,
        };
        // Arm the remote bearer token up front (best-effort) so a stage — or a
        // *local* project level that references `http(s)://` payloads — opens
        // with authenticated fetches, without the user first opening a remote
        // stage explicitly. Mirrors the standalone player's pre-open `prepare()`
        // (`idtx-runtime-core/src/remote.rs`). Must run before the startup
        // priority block below, which may open a project's default stage. A
        // missing/failed login just leaves the token unarmed and is logged.
        #[cfg(feature = "remote")]
        if let Err(e) = state.ensure_bearer_token() {
            tracing::warn!(
                "remote bearer token not armed at startup ({e}); authenticated \
                 remote fetches will fail until the server is reconfigured"
            );
        }

        // Startup priority: an explicit project (Conductor `--project`) wins,
        // then a bare stage file, then — if requested and nothing else was
        // given — the startup project dialog. Otherwise the window comes up
        // empty.
        if let Some(project) = project {
            state.activate_project(project);
        } else if let Some(path) = initial_stage {
            state.open_path(&path, None, None, false);
        } else if prompt_for_project {
            state.startup_dialog = Some(StartupProjectDialog::discovering());
        }

        // Background poller: refresh the "Vision Pro connected" flag every few
        // seconds via `devicectl` so the Build menu's "Build and deploy" entry
        // enables/disables itself (read when the user opens the menu). macOS only
        // (no `devicectl` elsewhere); the thread is a daemon tied to process life.
        #[cfg(all(feature = "build-tools", target_os = "macos"))]
        {
            let flag = state.visionos_device_connected.clone();
            std::thread::spawn(move || {
                loop {
                    let connected = match idtx_build::list_paired_devices() {
                        Ok(devices) => idtx_build::select_visionos_device(&devices).is_some(),
                        Err(_) => false,
                    };
                    flag.store(connected, std::sync::atomic::Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_secs(8));
                }
            });
        }

        Ok(state)
    }
}
