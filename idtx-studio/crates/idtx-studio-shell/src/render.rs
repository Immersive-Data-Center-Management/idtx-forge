//! The per-frame render + input path.
//!
//! [`ApplicationState::render`] runs the USD tick, GPU frame, and the entire
//! egui UI closure (menus, panels, viewport, gizmo overlay), collecting
//! [`ShellAction`]s that are applied afterwards. [`ApplicationState::handle_shortcut`]
//! processes global keyboard shortcuts feeding the same frame.

use std::sync::Arc;

use egui_wgpu::ScreenDescriptor;
#[cfg(all(feature = "remote", feature = "authoring"))]
use idtx_authoring::EditCommand;
use idtx_editor_model::EditorCommand;
use idtx_editor_model::LayerStackInfo;
#[cfg(feature = "authoring")]
use idtx_editor_model::ProductKind;
use idtx_editor_ui::compute_canvas::{self as cc, ComputeGraphSource};
use idtx_editor_ui::graph_canvas::GraphCanvasState;
use idtx_editor_ui::inspector::inspector_panel;
use idtx_editor_ui::layer_stack::{LayerStackAction, layer_stack_panel};
use idtx_editor_ui::material_canvas::{self as mc, CanvasGraphSource, MaterialCanvasState};
use idtx_editor_ui::remote_browser::AppConfigAction;
#[cfg(feature = "remote")]
use idtx_editor_ui::remote_browser::RemoteBrowserAction;
use idtx_editor_ui::scene_tree::{SceneTreeAction, SceneTreeIndex, scene_tree_panel};
#[cfg(feature = "authoring")]
use idtx_editor_ui::stage_panel::StageAction;
use idtx_editor_ui::stage_panel::{stage_behavior_panel, stage_panel};
#[cfg(feature = "authoring")]
use idtx_editor_ui::stage_panel::StageBehaviorAction;
use idtx_editor_ui::viewport_toolbar::{CameraViewLabel, RenderMode, viewport_toolbar};
use idtx_studio_session::{ProjectionMode, ViewPreset};
use num_traits::ToPrimitive as _;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::app::ApplicationState;
use crate::authoring_ops::InspectorEditSink;
#[cfg(feature = "authoring")]
use crate::gizmo::ActiveDrag;
use crate::gizmo::{self, GizmoState};
use crate::model::{CenterTab, InspectorTab, ShellAction, ShellError};
use crate::projection::{
    build_gizmo_data, build_tree_nodes, inspector_mode_for_profile, product_supports_authoring,
    ray_from_viewport, refresh_canvas_graph_source, refresh_compute_graph_source,
    refresh_inspector_cache, refresh_layer_stack_cache, refresh_stage_behavior_cache,
};
#[cfg(feature = "remote")]
use crate::remote::remote_download_url;
#[cfg(feature = "cooking")]
use crate::runtime_launch::PreviewProcess;
#[cfg(feature = "metrics")]
use crate::shell_metrics;
#[cfg(feature = "remote")]
use crate::view::sync_border_color;
use crate::view::{ActiveTabData, TabHeader};

/// Render a remote menu entry that targets a configured server.
///
/// With exactly one configured server the entry is a direct button (returns
/// `Some(0)` when clicked — today's immediate behaviour). With several it
/// expands to a submenu with one entry per server, labelled by description, and
/// returns the chosen server's index. With none it is a disabled hint. The
/// caller maps the returned index onto the appropriate `ShellAction` and closes
/// the parent menu.
#[cfg(feature = "remote")]
fn server_choice(
    ui: &mut egui::Ui,
    label: &str,
    hover: &str,
    servers: &[(String, String)],
) -> Option<usize> {
    let mut chosen = None;
    match servers.len() {
        0 => {
            ui.add_enabled(false, egui::Button::new(label))
                .on_disabled_hover_text("Configure a server first via Configuration…");
        }
        1 => {
            if ui.button(label).on_hover_text(hover).clicked() {
                chosen = Some(0);
            }
        }
        _ => {
            ui.menu_button(label, |ui| {
                for (idx, (description, _url)) in servers.iter().enumerate() {
                    if ui.button(description.as_str()).clicked() {
                        chosen = Some(idx);
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text(hover);
        }
    }
    chosen
}

impl ApplicationState {
    /// Run a structural edit closure against tab `idx`'s session, recording a
    /// reversible [`EditCommand::Structural`] journal entry around it.
    ///
    /// Structural edits (define, delete, add reference/payload) author into the
    /// root layer and have no cheap semantic inverse, so undo/redo work by
    /// snapshotting the root-layer USDA text `before` and `after` the edit. The
    /// journal drops the entry automatically when the snapshots are identical
    /// (e.g. the edit no-oped because its target no longer exists), so callers
    /// can journal unconditionally on success.
    #[cfg(feature = "authoring")]
    pub(crate) fn journaled_structural<T>(
        &mut self,
        idx: usize,
        label: impl Into<String>,
        f: impl FnOnce(
            &mut idtx_studio_session::StudioSession,
        ) -> Result<T, idtx_studio_session::SessionError>,
    ) -> Result<T, idtx_studio_session::SessionError> {
        let tab = &mut self.tabs[idx];
        crate::authoring_ops::journal_root_edit(&mut tab.journal, &mut tab.session, label, f)
    }

    /// Drive the Conductor "Attach Behavior…" flow for the prim at `prim_path` on
    /// tab `idx`: pick a Rust `.rs` source, copy it into the project's
    /// `behaviors/`, and author the `IdtxBehaviorAPI` manifest scaffold onto the
    /// prim (via [`crate::behavior_tools::attach_behavior_source`]). The authored
    /// source triple + identity fields are journaled as one structural edit; the
    /// copied `.rs` is inert until referenced and stays on disk on undo (mirrors
    /// the "Create Behavior" handler). Kicks a non-blocking cargo check of the
    /// freshly-attached source. No `.wasm` is produced — the module is compiled
    /// before preview / at packaging.
    #[cfg(feature = "cooking")]
    pub(crate) fn perform_attach_behavior(&mut self, idx: usize, prim_path: &str) {
        let Some(picked) = rfd::FileDialog::new()
            .add_filter("Rust behavior source", &["rs"])
            .pick_file()
        else {
            return;
        };

        // Snapshot the root layer so the authored scaffold is undoable in one
        // step. The copied `.rs` is not part of the layer and stays on disk on
        // undo (it is inert until referenced).
        let before = self.tabs[idx].session.root_layer_snapshot().ok();
        match crate::behavior_tools::attach_behavior_source(
            &mut self.tabs[idx].session,
            prim_path,
            &picked,
        ) {
            Ok(_source_asset) => {
                if let (Some(before), Ok(after)) =
                    (before, self.tabs[idx].session.root_layer_snapshot())
                {
                    self.tabs[idx]
                        .journal
                        .push(idtx_authoring::EditCommand::Structural {
                            label: "Attach Behavior".to_owned(),
                            before,
                            after,
                        });
                }
                // Kick a non-blocking cargo check of the attached source; the
                // `.wasm` itself is only built at preview/package.
                let ctx = self.egui_ctx.clone();
                crate::behavior_tools::start_check(&ctx, &mut self.tabs[idx].session, prim_path);
                self.window.request_redraw();
            }
            Err(e) => {
                tracing::warn!("attach_behavior failed: {e}");
                self.status_message = Some(format!("Attach Behavior failed: {e}"));
            }
        }
    }

    /// Create a source-authored **stage** behavior on tab `idx`: ensure the
    /// `/StageBehavior` prim + root-layer metadata, scaffold a `.rs` source, and
    /// set the behavior scope to `stage`. The whole batch is journaled as one
    /// structural edit and a non-blocking cargo check is kicked. Mirrors the
    /// per-prim "Create Behavior" handler.
    #[cfg(feature = "cooking")]
    pub(crate) fn perform_create_stage_behavior(&mut self, idx: usize) {
        let path = idtx_studio_session::STAGE_BEHAVIOR_PRIM_PATH;
        let before = self.tabs[idx].session.root_layer_snapshot().ok();
        if let Err(e) = self.tabs[idx].session.ensure_stage_behavior_prim() {
            tracing::warn!("ensure_stage_behavior_prim failed: {e}");
            self.status_message = Some(format!("Create Stage Behavior failed: {e}"));
            return;
        }
        match crate::behavior_tools::create_behavior_source(&mut self.tabs[idx].session, path) {
            Ok(_source_asset) => {
                self.finish_stage_behavior_author(idx, before, "Create Stage Behavior");
            }
            Err(e) => {
                tracing::warn!("create stage behavior failed: {e}");
                self.status_message = Some(format!("Create Stage Behavior failed: {e}"));
                // `ensure_stage_behavior_prim` already authored the `/StageBehavior`
                // prim + root metadata (dirtying the root layer). With the source
                // step failed there is nothing to journal, so roll the root layer
                // back to `before` rather than leaving a half-authored,
                // non-undoable stage behavior committed.
                self.rollback_stage_behavior_author(idx, before);
            }
        }
    }

    /// Attach an existing `.rs` source as the **stage** behavior on tab `idx`
    /// (file picker + same preamble/journaling as
    /// [`Self::perform_create_stage_behavior`]).
    #[cfg(feature = "cooking")]
    pub(crate) fn perform_attach_stage_behavior(&mut self, idx: usize) {
        let Some(picked) = rfd::FileDialog::new()
            .add_filter("Rust behavior source", &["rs"])
            .pick_file()
        else {
            return;
        };
        let path = idtx_studio_session::STAGE_BEHAVIOR_PRIM_PATH;
        let before = self.tabs[idx].session.root_layer_snapshot().ok();
        if let Err(e) = self.tabs[idx].session.ensure_stage_behavior_prim() {
            tracing::warn!("ensure_stage_behavior_prim failed: {e}");
            self.status_message = Some(format!("Attach Stage Behavior failed: {e}"));
            return;
        }
        match crate::behavior_tools::attach_behavior_source(
            &mut self.tabs[idx].session,
            path,
            &picked,
        ) {
            Ok(_source_asset) => {
                self.finish_stage_behavior_author(idx, before, "Attach Stage Behavior");
            }
            Err(e) => {
                tracing::warn!("attach stage behavior failed: {e}");
                self.status_message = Some(format!("Attach Stage Behavior failed: {e}"));
                // See `perform_create_stage_behavior`: undo the `ensure_*` writes
                // rather than commit a half-authored stage behavior.
                self.rollback_stage_behavior_author(idx, before);
            }
        }
    }

    /// Roll the root layer back to a captured `before` snapshot after a
    /// stage-behavior authoring step failed partway through (the `ensure_*` prim +
    /// metadata writes already landed, but the source step did not). No-op when no
    /// snapshot was captured.
    #[cfg(feature = "cooking")]
    fn rollback_stage_behavior_author(&mut self, idx: usize, before: Option<String>) {
        if let Some(before) = before {
            if let Err(re) = self.tabs[idx].session.restore_root_layer_snapshot(&before) {
                tracing::warn!("rollback of partial stage-behavior authoring failed: {re}");
            }
        }
        self.window.request_redraw();
    }

    /// Shared tail of the stage-behavior create/attach flow: set `scope = stage`,
    /// journal the root-layer `before → after` as one structural edit, and kick a
    /// non-blocking cargo check.
    #[cfg(feature = "cooking")]
    fn finish_stage_behavior_author(
        &mut self,
        idx: usize,
        before: Option<String>,
        label: &str,
    ) {
        let path = idtx_studio_session::STAGE_BEHAVIOR_PRIM_PATH;
        if let Err(e) = self
            .tabs[idx]
            .session
            .set_behavior_scope(path, idtx_studio_session::BehaviorScope::Stage)
        {
            // Scope authoring failed: the prim would remain entity-scoped (bound to
            // `/StageBehavior` rather than `Entity::NULL`) and never receive stage
            // lifecycle events. Do NOT journal or kick a check for a half-configured
            // behavior — roll the root layer back to the captured `before` snapshot
            // so nothing partial is committed.
            tracing::warn!("set stage behavior scope failed: {e}");
            self.status_message = Some(format!("{label} failed: {e}"));
            if let Some(before) = before {
                if let Err(re) = self.tabs[idx].session.restore_root_layer_snapshot(&before) {
                    tracing::warn!("rollback after scope failure failed: {re}");
                }
            }
            self.window.request_redraw();
            return;
        }
        if let (Some(before), Ok(after)) =
            (before, self.tabs[idx].session.root_layer_snapshot())
        {
            self.tabs[idx]
                .journal
                .push(idtx_authoring::EditCommand::Structural {
                    label: label.to_owned(),
                    before,
                    after,
                });
        }
        let ctx = self.egui_ctx.clone();
        crate::behavior_tools::start_check(&ctx, &mut self.tabs[idx].session, path);
        self.window.request_redraw();
    }

    pub(crate) fn handle_shortcut(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let ctrl = self.modifiers.control_key() || self.modifiers.super_key();
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };

        // ── Blender-style numpad viewport navigation ───────────────────────────
        // Key-repeat is allowed for orbit/dolly steps (2/4/6/8/+/-).
        // 15° per step matches Blender's default numpad rotation increment.
        const ORBIT_STEP: f32 = 15.0 * std::f32::consts::PI / 180.0;
        const DOLLY_STEP: f32 = 0.1;
        match code {
            // Named orthographic views
            KeyCode::Numpad1 => {
                if let Some(idx) = self.active_tab_index() {
                    let preset = if ctrl {
                        ViewPreset::Back
                    } else {
                        ViewPreset::Front
                    };
                    self.tabs[idx].session.set_view_preset(preset);
                }
                return;
            }
            KeyCode::Numpad3 => {
                if let Some(idx) = self.active_tab_index() {
                    let preset = if ctrl {
                        ViewPreset::Left
                    } else {
                        ViewPreset::Right
                    };
                    self.tabs[idx].session.set_view_preset(preset);
                }
                return;
            }
            KeyCode::Numpad7 => {
                if let Some(idx) = self.active_tab_index() {
                    let preset = if ctrl {
                        ViewPreset::Bottom
                    } else {
                        ViewPreset::Top
                    };
                    self.tabs[idx].session.set_view_preset(preset);
                }
                return;
            }
            // Toggle perspective / orthographic
            KeyCode::Numpad5 if !event.repeat => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.toggle_projection();
                }
                return;
            }
            // Orbit steps (support key-repeat for continuous rotation)
            KeyCode::Numpad4 => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.orbit_camera(-ORBIT_STEP, 0.0);
                }
                return;
            }
            KeyCode::Numpad6 => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.orbit_camera(ORBIT_STEP, 0.0);
                }
                return;
            }
            KeyCode::Numpad8 => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.orbit_camera(0.0, ORBIT_STEP);
                }
                return;
            }
            KeyCode::Numpad2 => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.orbit_camera(0.0, -ORBIT_STEP);
                }
                return;
            }
            // Zoom in / zoom out
            KeyCode::NumpadAdd => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.dolly_camera(1.0 - DOLLY_STEP);
                }
                return;
            }
            KeyCode::NumpadSubtract => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.dolly_camera(1.0 + DOLLY_STEP);
                }
                return;
            }
            // Numpad . — frame all (same as F in the viewport)
            KeyCode::NumpadDecimal if !event.repeat => {
                if let Some(idx) = self.active_tab_index() {
                    self.tabs[idx].session.frame_all();
                }
                return;
            }
            _ => {}
        }

        // All other shortcuts — skip key-repeats.
        if event.repeat {
            return;
        }
        let shift = self.modifiers.shift_key();
        if ctrl {
            match code {
                KeyCode::KeyO => {
                    // Conductor imports into the active project instead of opening
                    // in place; there is no plain Open. Requires an open project.
                    #[cfg(feature = "authoring")]
                    if self.profile.project_management() {
                        if self.active_project.is_some() {
                            self.import_dialog();
                        }
                    } else {
                        self.open_dialog();
                    }
                    #[cfg(not(feature = "authoring"))]
                    self.open_dialog();
                }
                KeyCode::KeyR => self.reload_active(),
                KeyCode::KeyW => {
                    if let Some(id) = self.active_tab_id {
                        // Prompts to save unsaved changes first in authoring
                        // builds; closes immediately in the Inspector.
                        self.request_close_tab(id);
                    }
                }
                #[cfg(feature = "authoring")]
                KeyCode::KeyZ if !shift => self.undo_active(),
                #[cfg(feature = "authoring")]
                KeyCode::KeyZ if shift => self.redo_active(),
                #[cfg(feature = "authoring")]
                KeyCode::KeyS if !shift => {
                    if let Some(id) = self.active_tab_id {
                        // Remote stages have no writable local root layer — Ctrl+S
                        // routes to "Save As…" (matching the disabled menu item).
                        if self.active_is_remote() {
                            self.save_as_dialog(id);
                        } else {
                            self.begin_save(id);
                        }
                    }
                }
                #[cfg(feature = "authoring")]
                KeyCode::KeyS if shift => {
                    if let Some(id) = self.active_tab_id {
                        self.save_as_dialog(id);
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn render(&mut self) -> Result<(), ShellError> {
        #[cfg(feature = "metrics")]
        let t_frame_total = std::time::Instant::now();
        // Timing locals (only live under the metrics feature).
        #[cfg(feature = "metrics")]
        let (
            mut m_tick_ns,
            mut m_render_one_frame_ns,
            mut m_extract_ns,
            mut m_gpu_render_ns,
            mut m_anim_sample_ns,
        ): (u64, u64, u64, u64, u64) = (0, 0, 0, 0, 0);
        #[cfg(feature = "metrics")]
        let mut m_render_phases = idtx_studio_session::RenderFrameTimings::default();
        #[cfg(feature = "metrics")]
        let (
            mut m_extract_materials_ns,
            mut m_extract_meshes_ns,
            mut m_extract_instancers_ns,
            mut m_extract_topo_promote_ns,
        ): (u64, u64, u64, u64) = (0, 0, 0, 0);

        // ── Collect completed background loads ────────────────────────────────
        self.poll_loading_tabs();
        #[cfg(feature = "remote")]
        self.poll_remote_job();
        // Service a bounded prefix for inactive tabs as well. Socket queues use
        // backpressure; an inactive tab must still advance its snapshot/commit.
        #[cfg(all(feature = "remote", feature = "authoring"))]
        {
            let active = self.active_tab_index();
            let started = std::time::Instant::now();
            // Server layer reloads (LayerReloaded) collected while ticking; applied
            // after the loop so the per-tab mutable borrow is released first.
            #[cfg(all(feature = "remote", feature = "authoring"))]
            let mut layer_reloads: Vec<(u64, idtx_studio_session::PendingLayerReload)> = Vec::new();
            for _ in 0..self.tabs.len() {
                let index = self.collab_service_cursor % self.tabs.len();
                self.collab_service_cursor = (index + 1) % self.tabs.len();
                let tab = &mut self.tabs[index];
                if Some(index) != active && tab.session.has_collab() {
                    // Tick also drains USD notices and projects changes; merely
                    // applying edits would grow an inactive observer's backlog.
                    let _ = tab.session.tick();
                    let notices = tab.session.take_collab_notices();
                    if !notices.is_empty() { self.status_message = Some(notices.join("; ")); }
                    #[cfg(all(feature = "remote", feature = "authoring"))]
                    {
                        let id = tab.session.id().get();
                        for reload in tab.session.take_pending_layer_reloads() {
                            layer_reloads.push((id, reload));
                        }
                    }
                    if started.elapsed() >= std::time::Duration::from_millis(2) { break; }
                }
            }
            #[cfg(all(feature = "remote", feature = "authoring"))]
            for (tab_id, reload) in layer_reloads {
                self.apply_collab_layer_reload(tab_id, &reload);
            }
        }
        #[cfg(all(feature = "remote", feature = "authoring"))]
        self.poll_remote_commits();

        // ── Drive an in-flight Conductor import packaging job ─────────────────
        #[cfg(feature = "authoring")]
        self.poll_import_job();

        // ── Publish finished behavior compile-checks ───────────────────
        #[cfg(feature = "cooking")]
        crate::behavior_tools::poll_checks(&self.egui_ctx);
        #[cfg(feature = "cooking")]
        crate::behavior_tools::poll_verifies(&self.egui_ctx);

        // ── Execute deferred saves before acquiring a swapchain image ─────────
        // A save is a multi-second, UI-thread-blocking OpenUSD operation. Run
        // it here — at the top of the frame, before `get_current_texture` — so
        // no surface texture is held across the block. Afterwards reconfigure
        // the surface so the upcoming acquire starts from a fresh swapchain and
        // cannot wedge on state left over from the stall.
        #[cfg(feature = "authoring")]
        if !self.pending_saves.is_empty() {
            for (tab_id, path) in std::mem::take(&mut self.pending_saves) {
                self.do_save(tab_id, path);
            }
            if let Some(surface) = self.gpu.surface.as_ref() {
                surface.configure(&self.gpu.device, &self.gpu.configuration);
            }
        }

        // ── USD tick + GPU render for active tab ──────────────────────────────
        if let Some(idx) = self.active_tab_index() {
            #[cfg(feature = "metrics")]
            let t_tick = std::time::Instant::now();
            let _ = self.tabs[idx].session.tick();
            // Surface any collaboration notices (e.g. a server-rejected prim
            // create) raised during this tick in the status line. A single tick
            // can raise several (one per rejection/error), so join them all —
            // `.pop()` would silently discard every notice but the last.
            #[cfg(feature = "remote")]
            {
                let notices = self.tabs[idx].session.take_collab_notices();
                if !notices.is_empty() {
                    self.status_message = Some(notices.join("; "));
                    self.window.request_redraw();
                }
            }
            // Drive any server layer reloads for the active tab (re-fetch the
            // reloaded file, reload it in place, re-gate on a fresh snapshot).
            #[cfg(all(feature = "remote", feature = "authoring"))]
            {
                let tab_id = self.tabs[idx].session.id().get();
                let reloads = self.tabs[idx].session.take_pending_layer_reloads();
                for reload in reloads {
                    self.apply_collab_layer_reload(tab_id, &reload);
                }
            }
            #[cfg(feature = "metrics")]
            {
                m_tick_ns = t_tick.elapsed().as_nanos() as u64;
            }

            // ── Animation clock advance ───────────────────────────────────
            {
                let now = std::time::Instant::now();
                let dt = self
                    .last_frame_time
                    .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
                self.last_frame_time = Some(now);
                let _ = self.tabs[idx].session.tick_animation(dt);
                // Keep repainting while animation is playing.
                if self.tabs[idx]
                    .session
                    .animation_state()
                    .1
                    .map_or(false, |c| c.playing)
                {
                    self.window.request_redraw();
                }
                #[cfg(feature = "metrics")]
                {
                    m_anim_sample_ns = self.tabs[idx].session.last_anim_sample_ns();
                }
            }

            // OpenXR preview now runs in the out-of-process `idtx-runtime`
            // player; the editor renders only the desktop scene here.
            {
                #[cfg(feature = "metrics")]
                let t_rof = std::time::Instant::now();
                let wireframe = self.tabs[idx].render_mode == RenderMode::Wireframe;
                self.tabs[idx].session.render_one_frame(wireframe);
                #[cfg(feature = "metrics")]
                {
                    m_render_one_frame_ns = t_rof.elapsed().as_nanos() as u64;
                    let (_, extract_ns, gpu_ns, phases) =
                        self.tabs[idx].session.last_frame_timings();
                    m_extract_ns = extract_ns;
                    m_gpu_render_ns = gpu_ns;
                    m_render_phases = phases.clone();
                    if let Some(stats) = self.tabs[idx].session.last_extract_stats() {
                        m_extract_materials_ns = stats.extract_materials_ns;
                        m_extract_meshes_ns = stats.extract_meshes_ns;
                        m_extract_instancers_ns = stats.extract_instancers_ns;
                        m_extract_topo_promote_ns = stats.extract_topo_promote_ns;
                    }
                }
            }
        }

        // ── Acquire surface ───────────────────────────────────────────────────
        let Some(surface) = self.gpu.surface.as_ref() else {
            // The editor always has a window surface; surfaceless DesktopGpu is
            // the Quest-only runtime path, never the shell.
            return Ok(());
        };
        let surface_texture = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(&self.gpu.device, &self.gpu.configuration);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };
        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // ── Pre-extract display data (avoids borrow conflicts in the closure) ─
        let tabs_meta: Vec<TabHeader> = self
            .tabs
            .iter()
            .map(|t| TabHeader {
                id: t.session.id().get(),
                label: t.session.label().to_owned(),
                active: Some(t.session.id().get()) == self.active_tab_id,
                #[cfg(feature = "authoring")]
                dirty: t.session.is_stage_dirty(),
                #[cfg(not(feature = "authoring"))]
                dirty: false,
            })
            .collect();

        // Labels for in-flight loads shown as spinner tabs.
        let loading_tabs_meta: Vec<String> = self
            .loading_tabs
            .iter()
            .map(|lt| lt.path_label.clone())
            .collect();

        // Rebuild the cached node list and tree index when topology changes.
        // On the very first frame with non-empty topology the tree is also
        // fully collapsed so egui starts by painting only root-level rows.
        if let Some(i) = self.active_tab_index() {
            let current_topo_gen = self.tabs[i].session.topology_generation();
            let current_vis_gen = self.tabs[i].session.visibility_generation();
            if !self.tabs[i].tree_initialized
                || current_topo_gen != self.tabs[i].cached_topology_gen
                || current_vis_gen != self.tabs[i].cached_visibility_gen
            {
                let nodes = build_tree_nodes(&self.tabs[i].session);
                if !nodes.is_empty() {
                    let index = SceneTreeIndex::build(&nodes);
                    if !self.tabs[i].tree_initialized {
                        self.tabs[i].scene_tree_state.collapse_all(&nodes);
                        self.tabs[i].tree_initialized = true;
                    }
                    self.tabs[i].cached_nodes = Arc::new(nodes);
                    self.tabs[i].cached_index = Arc::new(index);
                    self.tabs[i].cached_topology_gen = current_topo_gen;
                    self.tabs[i].cached_visibility_gen = current_vis_gen;
                }
            }

            // Refresh the inspector cache when the selection or topology changes.
            refresh_inspector_cache(&mut self.tabs[i]);

            // Refresh the stage-behavior (/StageBehavior) cache for the Stage tab.
            refresh_stage_behavior_cache(&mut self.tabs[i]);

            // Refresh the layer stack cache (no-op unless its tab is active).
            refresh_layer_stack_cache(&mut self.tabs[i]);

            // Refresh the canvas graph source.
            refresh_canvas_graph_source(&mut self.tabs[i]);

            // Refresh the compute graph source.
            refresh_compute_graph_source(&mut self.tabs[i]);
        }

        // `profile` is `Copy`; bind it before the borrow of `self.tabs` below so
        // both the gizmo gate and the later egui closure can use it by value.
        let profile = self.profile;

        // Level-registry cache refresh. `refresh_level_cache` re-reads
        // each level's stage-kind tag only when the manifest changed; the
        // Project Settings dialog's per-frame `level_rows` snapshot (built below)
        // then adds `is_open`. Captured before the `&mut self.tabs` borrow below.
        self.refresh_level_cache();
        // Behavior inspector: the project's input actions for the inputs "add"
        // combo (cheap no-op unless the project root changed). Captured before
        // the `&mut self.tabs` borrow below.
        self.refresh_behavior_input_actions();
        let behavior_input_actions = self.behavior_input_actions.clone();
        // Asset-browser snapshot (Conductor only), captured before the borrow.
        let show_asset_browser = profile.show_asset_browser() && self.active_project.is_some();
        // Build/Package availability: the `build-tools` feature is
        // Conductor-only, so compiling this already implies a build-capable
        // product; just require a project open and no build already running.
        #[cfg(feature = "build-tools")]
        let can_build = self.active_project.is_some() && self.build_job.is_none();
        // Captured for the Build > "Build Vision OS" submenu: gates "Build and
        // deploy" on a currently-paired Vision Pro (refreshed by the devicectl
        // poller in `app.rs`).
        #[cfg(feature = "build-tools")]
        let vision_pro_connected = self
            .visionos_device_connected
            .load(std::sync::atomic::Ordering::Relaxed);
        let asset_browser_subdir = self.asset_browser_subdir.clone();
        let asset_rows = self.asset_browser_rows();
        let mut asset_browser_state = self.asset_browser_state.clone();
        let asset_authoring = cfg!(feature = "authoring") && profile.level_authoring();
        let asset_selected_prim = self
            .active_tab_index()
            .and_then(|i| self.tabs[i].session.selected_path().map(str::to_owned));

        let active_data: Option<ActiveTabData> = self.active_tab_index().map(|i| {
            let tab = &mut self.tabs[i];

            // Fetch gizmo transform data first (needs &mut session) then metadata.
            // The gizmo is gated by the same editability rule as the inspector's
            // transform fields so the two always appear together.
            let gizmo_data = build_gizmo_data(tab, product_supports_authoring(profile));
            let meta = tab.session.metadata();
            let (anim_clip, anim_clock) = tab.session.animation_state();

            ActiveTabData {
                id: tab.session.id().get(),
                texture_id: tab.texture_id,
                viewport_pixels: tab.session.viewport_pixels(),
                center_tab: tab.center_tab,
                render_mode: tab.render_mode,
                gizmo_space: tab.gizmo_space,
                camera_view: match tab.session.current_view_preset() {
                    None => CameraViewLabel::Perspective,
                    Some(ViewPreset::Front) => CameraViewLabel::Front,
                    Some(ViewPreset::Back) => CameraViewLabel::Back,
                    Some(ViewPreset::Left) => CameraViewLabel::Left,
                    Some(ViewPreset::Right) => CameraViewLabel::Right,
                    Some(ViewPreset::Top) => CameraViewLabel::Top,
                    Some(ViewPreset::Bottom) => CameraViewLabel::Bottom,
                },
                camera_ortho: matches!(
                    tab.session.projection_mode(),
                    ProjectionMode::Orthographic
                ),
                metadata_line: format!("MPU: {}  Up: {}", meta.meters_per_unit, meta.up_axis),
                source_path: idtx_project::normalize_display_path(tab.session.source_path()),
                is_remote: tab.session.is_remote(),
                collab_restricted: {
                    #[cfg(feature = "remote")]
                    {
                        tab.session.has_collab()
                    }
                    #[cfg(not(feature = "remote"))]
                    {
                        false
                    }
                },
                diagnostics: tab.session.asset_diagnostics().to_vec(),
                start_tc: anim_clip
                    .as_ref()
                    .map_or(meta.start_time_code, |c| c.start_time_code),
                end_tc: anim_clip
                    .as_ref()
                    .map_or(meta.end_time_code, |c| c.end_time_code),
                tcps: meta.time_codes_per_second,
                has_animation: anim_clip.as_ref().map_or(false, |c| c.has_animation),
                playing: anim_clock.map_or(false, |c| c.playing),
                current_tc: anim_clock
                    .map_or(anim_clip.as_ref().map_or(0.0, |c| c.start_time_code), |c| {
                        c.current_time_code
                    }),
                looping: anim_clock.map_or(true, |c| c.looping),
                scene_tree_nodes: Arc::clone(&tab.cached_nodes),
                scene_tree_index: Arc::clone(&tab.cached_index),
                scene_tree_state: tab.scene_tree_state.clone(),
                inspector_prim_info: tab.inspector_cache.prim_info.clone(),
                animation_inspector: idtx_editor_ui::inspector::panels::animation::AnimationInspector {
                    clips: tab.inspector_cache.prim_info.as_ref().map(|info| tab.session.clip_player_state(&info.path))
                        .unwrap_or_default().into_iter().map(|h| idtx_editor_ui::inspector::panels::animation::ClipView {
                            effective_source: tab.inspector_cache.prim_info.as_ref().map(|info|
                                tab.session.clip_effective_source(&info.path, &h.clip)).unwrap_or_default(),
                            label: h.clip, declaration: h.declaration, current: h.current_time_code,
                            last_command_error: h.last_command_error,
                            playing: h.playing, independent: h.independent, completed: h.completed, error: h.error,
                        }).collect(),
                    candidates: tab.cached_nodes.iter().map(|n| n.path.clone()).collect(),
                },
                behavior_bindings: tab.inspector_cache.prim_info.as_ref().map(|info| {
                    idtx_editor_ui::inspector::panels::behavior::AuthoredBindings {
                        context: tab.session.behavior_context(&info.path).map_err(|e| e.to_string()),
                        relationships: tab.session.behavior_relationships(&info.path).to_vec(),
                        candidates: tab.cached_nodes.iter().map(|node| node.path.clone()).collect(),
                    }
                }),
                curve_tool: tab.inspector_cache.prim_info.as_ref().and_then(|info| tab.session.curve_tool_state(&info.path)),
                point_instancer_tool: tab.inspector_cache.prim_info.as_ref().and_then(|info| tab.session.point_instancer_tool_state(&info.path)),
                inspector_properties: Arc::clone(&tab.inspector_cache.properties),
                inspector_skeleton_joints: tab.inspector_cache.skeleton_joints.clone(),
                inspector_tab: tab.inspector_tab,
                stage_metadata: meta.clone(),
                stage_shadow_mode: tab.session.stage_shadow_mode(),
                stage_behavior_prim_info: tab.stage_behavior_cache.prim_info.clone(),
                stage_behavior_bindings: tab.stage_behavior_cache.prim_info.as_ref().map(|info| {
                    idtx_editor_ui::inspector::panels::behavior::AuthoredBindings {
                        context: tab.session.behavior_context(&info.path).map_err(|e| e.to_string()),
                        relationships: tab.session.behavior_relationships(&info.path).to_vec(),
                        candidates: tab.cached_nodes.iter().map(|node| node.path.clone()).collect(),
                    }
                }),
                stage_behavior_properties: Arc::clone(&tab.stage_behavior_cache.properties),
                // Show the run scope the attach/tick gate actually enforces — the
                // owning (possibly payloaded) layer's value — falling back to the
                // session-cached root copy for a directly-opened stage. Keeps the
                // Stage panel's display (and re-authoring) consistent with the
                // resolved scope instead of the root-only cached one.
                stage_behavior_run_scope: tab
                    .session
                    .stage_behavior_run_scope_resolved()
                    .unwrap_or_else(|| tab.session.stage_behavior_run_scope()),
                physics_state: {
                    #[cfg(feature = "authoring")]
                    {
                        let s = tab.session.physics_scene_state();
                        idtx_editor_ui::stage_panel::PhysicsPanelState {
                            active: s.active,
                            gravity_direction: s.gravity_direction,
                            gravity_magnitude: s.gravity_magnitude,
                        }
                    }
                    #[cfg(not(feature = "authoring"))]
                    {
                        idtx_editor_ui::stage_panel::PhysicsPanelState::default()
                    }
                },
                layer_stack_info: tab.layer_stack_cache.info.clone(),
                layer_stack_ui: tab.layer_stack_ui.clone(),
                gizmo_data,
                #[cfg(feature = "authoring")]
                journal_can_undo: tab.journal.can_undo(),
                #[cfg(feature = "authoring")]
                journal_can_redo: tab.journal.can_redo(),
                #[cfg(feature = "authoring")]
                journal_undo_label: tab.journal.next_undo_label(),
                #[cfg(feature = "authoring")]
                journal_redo_label: tab.journal.next_redo_label(),
                #[cfg(feature = "remote")]
                collab_disconnected: tab.session.has_collab() && !tab.session.collab_is_connected(),
                #[cfg(feature = "remote")]
                collab_sync_status: tab.session.collab_sync_status(),
            }
        });

        let status_msg = self.status_message.clone();

        // Detect collaboration disconnect: if a tab had a live collab client
        // last frame but the background thread has now exited, surface a
        // one-time status message pointing the user to reconnect.
        #[cfg(feature = "remote")]
        if let Some(idx) = self.active_tab_index() {
            let now_connected = self.tabs[idx].session.collab_is_connected();
            let was_connected = self.tabs[idx].collab_was_connected;
            if was_connected && !now_connected && self.tabs[idx].session.has_collab() {
                self.status_message = Some(
                    "Collaboration session disconnected. \
                     Use File > Reconnect Collaboration to reconnect."
                        .to_owned(),
                );
                self.window.request_redraw();
            }
            self.tabs[idx].collab_was_connected = now_connected;
        }

        // Reap a self-exited preview process (user closed the runtime window),
        // so the menu flips back to "Preview" and any temp stage is deleted.
        #[cfg(feature = "cooking")]
        if let Some(proc) = self.preview_process.as_mut() {
            if !proc.poll() {
                // Surface *why* it ended. A normal exit reads plainly; a load
                // failure (no output, e.g. a missing runtime DLL) gets flagged
                // so an immediate silent exit is no longer "without notice".
                let report = proc.exit_report();
                self.preview_process = None;
                match report {
                    Some(detail) => {
                        eprintln!("[preview] runtime preview ended: {detail}");
                        self.status_message = Some(format!("Runtime preview ended — {detail}"));
                    }
                    None => {
                        self.status_message = Some("Runtime preview ended.".to_owned());
                    }
                }
            }
        }

        // Poll a background build. When the worker finishes, report the
        // cooked payload / bundle path (or the failure) in the status bar and
        // clear the job so the Build menu re-enables.
        #[cfg(feature = "build-tools")]
        if let Some(job) = self.build_job.as_ref() {
            if let Some(outcome) = job.poll() {
                let target_label = job.target().label();
                self.build_job = None;
                match outcome {
                    crate::build_job::BuildOutcome::Done {
                        payload,
                        bundle,
                        note,
                    } => {
                        let where_ = bundle.as_ref().unwrap_or(&payload);
                        let base =
                            format!("{target_label} build finished → {}", where_.display());
                        self.status_message = Some(match note {
                            Some(note) => format!("{base} — {note}"),
                            None => base,
                        });
                    }
                    crate::build_job::BuildOutcome::Failed(msg) => {
                        eprintln!("[build] {target_label} build failed: {msg}");
                        self.status_message = Some(format!("{target_label} build failed — {msg}"));
                    }
                }
                self.window.request_redraw();
            } else {
                // Keep repainting so the poll runs each frame until the job ends.
                self.window.request_redraw();
            }
        }
        // Poll a background collision-hull generation. On completion, author the
        // hull onto the owning prim (journaled for undo) and clear the job;
        // while it runs, keep repainting so the inspector's spinner animates.
        #[cfg(feature = "authoring")]
        if let Some(job) = self.hull_job.as_ref() {
            if let Some(outcome) = job.poll() {
                let (tab_id, path) = (job.tab_id, job.path.clone());
                self.hull_job = None;
                match outcome {
                    crate::hull_job::HullOutcome::Done(result) => {
                        if let Some(idx) =
                            self.tabs.iter().position(|t| t.session.id().get() == tab_id)
                        {
                            let n_pts = result.points.len();
                            let n_tris = result.triangles.len() / 3;
                            let res = self.journaled_structural(
                                idx,
                                "Generate CollisionHull",
                                |s| {
                                    s.author_collision_hull(
                                        &path,
                                        &result.points,
                                        &result.triangles,
                                    )
                                },
                            );
                            self.status_message = Some(match res {
                                Ok(()) => format!(
                                    "Collision hull generated — {n_pts} verts / {n_tris} tris"
                                ),
                                Err(e) => format!("Generate CollisionHull failed — {e}"),
                            });
                        }
                    }
                    crate::hull_job::HullOutcome::Failed(msg) => {
                        self.status_message = Some(format!("Generate CollisionHull failed — {msg}"));
                    }
                }
                self.window.request_redraw();
            } else {
                // Keep repainting so the poll runs each frame + the spinner spins.
                self.window.request_redraw();
            }
        }
        // Snapshot the runtime-preview gating state for the menu (Copy, so the
        // egui closure can capture it by value alongside `actions`). The Preview
        // menu is a `cooking`-only surface, so this snapshot is compiled only
        // then; `enabled_for_product` is always true at that point (Conductor).
        #[cfg(feature = "cooking")]
        let runtime_preview_ctrl = idtx_editor_ui::xr_controls::RuntimePreviewControl {
            enabled_for_product: true,
            stage_open: active_data.is_some(),
            preview_running: self
                .preview_process
                .as_ref()
                .is_some_and(PreviewProcess::is_running),
        };
        #[cfg(feature = "authoring")]
        let save_confirm = self.pending_save_confirm.clone();
        // Save-on-close prompt snapshot: the head dirty tab's id + label and how
        // many dirty tabs remain in the queue, so the egui closure can render
        // the confirmation modal without borrowing `self`.
        #[cfg(feature = "authoring")]
        let close_confirm: Option<(u64, String, usize, bool)> =
            self.pending_close.as_ref().and_then(|pc| {
                pc.queue.first().map(|&head| {
                    let tab = self.tabs.iter().find(|t| t.session.id().get() == head);
                    let label = tab
                        .map(|t| t.session.label().to_owned())
                        .unwrap_or_else(|| "this stage".to_owned());
                    // A remote stage with a live collab session can be committed
                    // to the server (its authoritative origin) instead of saved
                    // to a local file. Mirrors the "Commit Remote" menu gate.
                    let committable = {
                        #[cfg(feature = "remote")]
                        {
                            tab.map(|t| {
                                t.session.collab_sync_status().is_some()
                                    && t.session.collab_is_connected()
                            })
                            .unwrap_or(false)
                        }
                        #[cfg(not(feature = "remote"))]
                        {
                            false
                        }
                    };
                    (head, label, pc.queue.len(), committable)
                })
            });
        // The in-flight collision-hull job (owning tab + prim), snapshot so the
        // egui closure can show the inspector spinner without borrowing `self`.
        #[cfg(feature = "authoring")]
        let hull_generating: Option<(u64, String)> =
            self.hull_job.as_ref().map(|j| (j.tab_id, j.path.clone()));
        #[cfg(not(feature = "authoring"))]
        let hull_generating: Option<(u64, String)> = None;
        #[cfg(feature = "authoring")]
        let mut rename_dialog_state = self.rename_dialog.clone();
        // Read-only snapshot for rendering the drop-target picker modal inside
        // the egui closure. Unlike the rename dialog it is never mutated in the
        // closure (clicks push actions), so it must NOT be written back — the
        // scene-tree action loop sets `self.drop_target_picker` directly after
        // the closure, and a write-back would clobber that with this stale None.
        #[cfg(feature = "authoring")]
        let drop_target_picker_state = self.drop_target_picker.clone();
        // Snapshot the active project's presence (gates the Conductor "Import…"
        // menu item) and any in-flight import (drives the import modal), so the
        // egui closure can capture them without borrowing `self`.
        #[cfg(feature = "authoring")]
        let has_active_project = self.active_project.is_some();
        #[cfg(feature = "authoring")]
        let import_view: Option<(String, crate::import::ImportPhase)> = self
            .import_job
            .as_ref()
            .filter(|job| !job.abandoned)
            .map(|job| (job.source_label.clone(), job.phase.clone()));
        let scale = self.window.scale_factor().to_f32().unwrap_or(1.0);
        let inspector_registry = Arc::clone(&self.inspector_registry);
        let mut actions: Vec<ShellAction> = Vec::new();
        let mut tree_actions: Vec<SceneTreeAction> = Vec::new();
        let mut new_viewport_hovered = false;

        // Remote UI snapshots captured before the closure to avoid borrowing `self`.
        // The active server's URL drives the browser/session flows; the full
        // (description, url) list drives the per-server menu entries.
        #[cfg(feature = "remote")]
        let server_url_for_browser = self
            .active_server_config()
            .map(|c| c.url.clone())
            .unwrap_or_default();
        #[cfg(feature = "remote")]
        let server_menu: Vec<(String, String)> = self
            .servers
            .iter()
            .map(|c| (c.description.clone(), c.url.clone()))
            .collect();
        // The targeted server index for browser/session refreshes (re-list the
        // currently-active server).
        #[cfg(feature = "remote")]
        let active_server_idx = self.active_server.unwrap_or(0);
        let mut configure_dialog_state = self.configure_dialog.clone();
        let mut project_settings_state = self.project_settings_dialog.clone();
        // Refresh the dialog's Levels section each frame while it is open: the
        // registry rows (and `is_open`) track the active tab, and the authoring
        // flag gates create/add/remove/reorder. Selection persists in the dialog
        // state itself (written back after the pass).
        if let Some(dlg) = project_settings_state.as_mut() {
            dlg.level_rows = self.level_browser_rows();
            dlg.level_authoring = asset_authoring;
            // Refresh the runtime-prototype dropdowns from the project each frame:
            // the asset list (project USD files) and, per row, the selected
            // asset's root-level prims. `read_stage_root_prims` reads only the
            // root layer, so a handful of rows is cheap even per frame; picking a
            // new asset re-populates its prim list on the next frame with no
            // explicit refresh intent.
            if let Some(project) = self.active_project.as_ref() {
                dlg.asset_options = crate::project_settings::project_usd_files(project);
                for row in &mut dlg.runtime_prototypes {
                    row.prim_options =
                        crate::project_settings::root_prim_options(project, &row.asset);
                }
            }
        }
        // Mutable snapshot for the closure (the "New Project" sub-form edits its
        // name buffer in place); written back after the pass and *before*
        // `apply_actions`, which may replace it (dismiss / create / begin).
        let mut startup_dialog = self.startup_dialog.clone();
        let recent_files = self.recent_files.clone();
        let recent_projects = self.recent_projects.clone();
        #[cfg(feature = "remote")]
        let mut remote_browser_state = self.remote_browser.clone();
        #[cfg(all(feature = "remote", feature = "authoring"))]
        let remote_browser_sync_mode_cap = self.remote_browser_sync_mode;
        #[cfg(all(feature = "remote", feature = "authoring"))]
        let remote_session_mode_cap = self.remote_session_mode;
        #[cfg(all(feature = "remote", feature = "authoring"))]
        let mut session_browser_state = self.session_browser.clone();
        // The "Create Child → Asset…" picker context (frame-start snapshot). When
        // `Some`, the remote browser acts as an asset picker (excluding the
        // current stage) rather than an Open-Remote dialog. `clear_*` is flipped
        // inside the closure when the picker is resolved/dismissed so the context
        // is dropped after the closure (the closure can't touch `self`).
        #[cfg(all(feature = "remote", feature = "authoring"))]
        let asset_create_target_cap = self.asset_create_target.clone();
        #[cfg(all(feature = "remote", feature = "authoring"))]
        let mut clear_asset_create_target = false;

        // Extract mutable canvas state so it can be mutated inside the egui
        // closure without conflicting with the immutable `active_data` borrow.
        let mut canvas_state: MaterialCanvasState = self
            .active_tab_index()
            .map(|i| self.tabs[i].material_canvas.clone())
            .unwrap_or_default();
        let canvas_source: Option<Arc<CanvasGraphSource>> = self
            .active_tab_index()
            .and_then(|i| self.tabs[i].canvas_graph_source.clone());

        // Same for the Compute Graph canvas state + source.
        let mut compute_state: GraphCanvasState = self
            .active_tab_index()
            .map(|i| self.tabs[i].compute_canvas.clone())
            .unwrap_or_default();
        let compute_source: Option<Arc<ComputeGraphSource>> = self
            .active_tab_index()
            .and_then(|i| self.tabs[i].compute_graph_source.clone());

        // Extract mutable gizmo state; restored after the egui closure.
        let mut gizmo_state: GizmoState = self
            .active_tab_index()
            .map(|i| std::mem::take(&mut self.tabs[i].gizmo))
            .unwrap_or_default();

        // ── egui UI pass ──────────────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        let t_egui = std::time::Instant::now();
        #[cfg(feature = "metrics")]
        let scene_tree_cell = std::cell::Cell::new(0u64);
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let output = self.egui_ctx.run_ui(raw_input, |ui| {
            let ctx = ui.ctx().clone();

            // ── Global keyboard shortcuts ─────────────────────────────────────
            // Checked via egui so macOS NSTextInputClient intercept is bypassed.
            #[cfg(feature = "authoring")]
            if profile.kind != ProductKind::Inspector {
                use egui::{Key, KeyboardShortcut, Modifiers};
                let new_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
                let undo_sc = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
                let redo_sc = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
                if ctx.input_mut(|i| i.consume_shortcut(&new_sc)) {
                    actions.push(ShellAction::NewStage);
                }
                // Check redo first: its pattern (⌘⇧Z) is more specific than undo (⌘Z).
                // egui uses "contains" modifier matching, so ⌘⇧Z would also match ⌘Z
                // if undo were checked first.
                if ctx.input_mut(|i| i.consume_shortcut(&redo_sc)) {
                    if let Some(data) = &active_data {
                        actions.push(ShellAction::Redo { tab_id: data.id });
                    }
                } else if ctx.input_mut(|i| i.consume_shortcut(&undo_sc)) {
                    if let Some(data) = &active_data {
                        actions.push(ShellAction::Undo { tab_id: data.id });
                    }
                }
            }

            // ── Menu bar ──────────────────────────────────────────────────────
            egui::Panel::top("menu_bar").show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    // App-name menu (always first)
                    ui.menu_button(profile.title(), |ui| {
                        if ui.button("Configure…").clicked() {
                            actions.push(ShellAction::OpenConfigureDialog);
                            ui.close();
                        }
                    });

                    // Project menu — Conductor-only (project management). Hosts
                    // the project-wide Settings modal (edits `project.toml`).
                    // `authoring`-gated: project management implies the Conductor,
                    // which always compiles with authoring (`has_active_project`).
                    #[cfg(feature = "authoring")]
                    if profile.project_management() {
                        ui.menu_button("Project", |ui| {                            
                            if ui.button("New Project…").clicked() {
                                actions.push(ShellAction::BeginNewProject);
                                ui.close();
                            }
                            if ui.button("Open Project…").clicked() {
                                actions.push(ShellAction::OpenProjectPicker);
                                ui.close();
                            }
                            // Conductor imports files into the active project's
                            // assets/ folder rather than opening them in place —
                            // there is no plain "Open…". Disabled until a project
                            // is open.
                            let import = ui
                                .add_enabled(
                                    has_active_project,
                                    egui::Button::new("Import…    Ctrl+O"),
                                )
                                .on_disabled_hover_text("Open a project first");
                            if import.clicked() {
                                actions.push(ShellAction::ImportDialog);
                                ui.close();
                            }

                            // Create Player Stage: authors a standalone player
                            // stage referencing the toolbox's built-in player and
                            // records it as the project's player_stage. Disabled
                            // until a project is open.
                            #[cfg(feature = "authoring")]
                            {
                                let create = ui
                                    .add_enabled(
                                        has_active_project,
                                        egui::Button::new("Create Player Stage"),
                                    )
                                    .on_disabled_hover_text("Open a project first");
                                if create.clicked() {
                                    actions.push(ShellAction::CreatePlayerStage);
                                    ui.close();
                                }
                            }

                            let settings = ui
                                .add_enabled(
                                    has_active_project,
                                    egui::Button::new("Settings…"),
                                )
                                .on_disabled_hover_text("Open a project first");
                            if settings.clicked() {
                                actions.push(ShellAction::OpenProjectSettings);
                                ui.close();
                            }
                        });
                    }

                    // File menu
                    ui.menu_button("File", |ui| {
                        #[cfg(feature = "authoring")]
                        if profile.kind != ProductKind::Inspector {
                            if ui.button("New…    Ctrl+N").clicked() {
                                actions.push(ShellAction::NewStage);
                                ui.close();
                            }
                        }

                        // Project management is a Conductor-only capability.
                        if !profile.project_management() {
                            if ui.button("Open…    Ctrl+O").clicked() {
                                actions.push(ShellAction::OpenDialog);
                            }

                            ui.menu_button("Open Recent…", |ui| {
                                if recent_files.is_empty() {
                                    ui.add_enabled(false, egui::Button::new("(no recent files)"));
                                } else {
                                    for path in &recent_files {
                                        let label = path
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("<unnamed>");
                                        if ui
                                            .button(label)
                                            .on_hover_text(path.display().to_string())
                                            .clicked()
                                        {
                                            actions.push(ShellAction::OpenRecent(path.clone()));
                                            ui.close();
                                        }
                                    }
                                }
                            });
                        
                            #[cfg(feature = "remote")]
                            {
                                ui.separator();
                                ui.menu_button("Open Remote…", |ui| {
                                    let local_label = if !cfg!(feature = "authoring") {
                                        "View"
                                    } else {
                                        "Local Edit"
                                    };
                                    if let Some(server) = server_choice(
                                        ui,
                                        local_label,
                                        "Open a remote stage for read/local authoring only. Can be saved as local copy",
                                        &server_menu,
                                    ) {
                                        actions.push(ShellAction::OpenRemoteDialog { server });
                                        ui.close();
                                    }
                                    #[cfg(feature = "authoring")]
                                    if product_supports_authoring(profile) {
                                        if let Some(server) = server_choice(
                                            ui,
                                            "Single Edit",
                                            "Open a remote stage for read or single-user authoring.",
                                            &server_menu,
                                        ) {
                                            actions.push(ShellAction::OpenRemoteWithSyncDialog {
                                                server,
                                                mode: idtx_collab::session::SessionMode::SingleEdit,
                                            });
                                            ui.close();
                                        }

                                        if let Some(server) = server_choice(
                                            ui,
                                            "Collaborative Edit",
                                            "Open a server stage for remote edits. \
                                            Root-layer edits are broadcast to all peers in multi-user scenario.",
                                            &server_menu,
                                        ) {
                                            actions.push(ShellAction::OpenRemoteWithSyncDialog {
                                                server,
                                                mode: idtx_collab::session::SessionMode::CollaborativeEdit,
                                            });
                                            ui.close();
                                        }
                                    }
                                });
                                let has_live_session = active_data
                                    .as_ref()
                                    .map(|d| d.collab_sync_status.is_some() && !d.collab_disconnected)
                                    .unwrap_or(false);
                                #[cfg(feature = "authoring")]
                                if product_supports_authoring(profile) {
                                    if let Some(server) = server_choice(
                                        ui,
                                        "Connect Remote with Sync…",
                                        "Browse active collaboration sessions on the server \
                                        and join one; transforms are synced bidirectionally \
                                        with all session peers.",
                                        &server_menu,
                                    ) {
                                        actions.push(ShellAction::OpenSessionBrowserDialog { server });
                                        ui.close();
                                    }
                                    // Reconnect is only enabled when a collab client exists
                                    // but its background thread has exited (server disconnect).
                                    let collab_disconnected = active_data
                                        .as_ref()
                                        .map(|d| d.collab_disconnected || matches!(d.collab_sync_status, Some(idtx_studio_session::CollabSyncStatus::Syncing)))
                                        .unwrap_or(false);
                                    ui.add_enabled_ui(collab_disconnected, |ui| {
                                        if ui
                                            .button("Reconnect Collaboration")
                                            .on_hover_text(
                                                "Re-establish the WebSocket connection to the \
                                                collaboration server and replay authoritative state. Local opinions are retained in recovery.",
                                            )
                                            .clicked()
                                        {
                                            if let Some(data) = &active_data {
                                                actions.push(ShellAction::ReconnectCollab {
                                                    tab_id: data.id,
                                                });
                                            }
                                            ui.close();
                                        }
                                    });
                                    ui.add_enabled_ui(active_data.as_ref().is_some_and(|data| data.collab_sync_status.is_some()), |ui| {
                                        if ui.button("Export Collaboration Recovery…").clicked() {
                                            if let Some(data) = &active_data { actions.push(ShellAction::ExportCollabRecovery { tab_id: data.id }); }
                                            ui.close();
                                        }
                                    });
                                    ui.add_enabled_ui(has_live_session, |ui| {
                                        if ui
                                            .button("Commit Remote")
                                            .on_hover_text(
                                                "Merge the current session overrides back into the \
                                                server's origin USD file.",
                                            )
                                            .clicked()
                                        {
                                            if let Some(data) = &active_data {
                                                actions.push(ShellAction::CommitRemote {
                                                    tab_id: data.id,
                                                });
                                            }
                                            ui.close();
                                        }
                                    });
                                }
                            }
                        }

                        ui.add_enabled_ui(active_data.is_some(), |ui| {
                            let active_is_remote =
                                active_data.as_ref().map(|d| d.is_remote).unwrap_or(false);
                            let reload_hint = if active_is_remote {
                                "Re-fetch this stage from the server, overwriting the \
                                cached copy, and reopen it."
                            } else {
                                "Reopen this stage from disk, discarding the in-memory \
                                stage/layer state."
                            };
                            if ui
                                .button("Reload     Ctrl+R")
                                .on_hover_text(reload_hint)
                                .clicked()
                            {
                                actions.push(ShellAction::Reload);
                            }
                        });

                        #[cfg(feature = "authoring")]
                        if profile.kind != ProductKind::Inspector {
                            ui.separator();
                            let is_remote =
                                active_data.as_ref().map(|d| d.is_remote).unwrap_or(false);
                            ui.add_enabled_ui(active_data.is_some(), |ui| {
                                // In-place "Save" is disabled for remote stages —
                                // they have no writable local root layer. Use
                                // "Save As…" to write a local copy.
                                let save_btn = ui.add_enabled(
                                    !is_remote,
                                    egui::Button::new("Save    Ctrl+S"),
                                );
                                if save_btn.clicked() {
                                    if let Some(data) = &active_data {
                                        actions.push(ShellAction::Save { tab_id: data.id });
                                    }
                                    ui.close();
                                }
                                if is_remote {
                                    save_btn.on_hover_text(
                                        "Use Save As… for stages opened from a remote server",
                                    );
                                }
                                if ui.button("Save As…    Ctrl+Shift+S").clicked() {
                                    if let Some(data) = &active_data {
                                        actions.push(ShellAction::SaveAs { tab_id: data.id });
                                    }
                                    ui.close();
                                }
                            });
                        }
                    });

                    // Edit menu — only for authoring-capable products with an open stage.
                    #[cfg(feature = "authoring")]
                    if profile.kind != ProductKind::Inspector && active_data.is_some() {
                        ui.menu_button("Edit", |ui| {
                            let (can_undo, undo_label) = active_data
                                .as_ref()
                                .map(|d| (d.journal_can_undo, d.journal_undo_label.clone()))
                                .unwrap_or((false, None));
                            let (can_redo, redo_label) = active_data
                                .as_ref()
                                .map(|d| (d.journal_can_redo, d.journal_redo_label.clone()))
                                .unwrap_or((false, None));
                            let truncate = |s: String| -> String {
                                const MAX: usize = 22;
                                if s.chars().count() > MAX {
                                    format!("{}…", s.chars().take(MAX).collect::<String>())
                                } else {
                                    s
                                }
                            };
                            let undo_text = undo_label
                                .map(|l| format!("Undo {}", truncate(l)))
                                .unwrap_or_else(|| "Undo".to_owned());
                            let redo_text = redo_label
                                .map(|l| format!("Redo {}", truncate(l)))
                                .unwrap_or_else(|| "Redo".to_owned());
                            if let Some(data) = &active_data {
                                ui.add_enabled_ui(can_undo, |ui| {
                                    if ui
                                        .add(egui::Button::new(&undo_text).shortcut_text("⌘Z"))
                                        .clicked()
                                    {
                                        actions.push(ShellAction::Undo { tab_id: data.id });
                                        ui.close();
                                    }
                                });
                                ui.add_enabled_ui(can_redo, |ui| {
                                    if ui
                                        .add(egui::Button::new(&redo_text).shortcut_text("⌘⇧Z"))
                                        .clicked()
                                    {
                                        actions.push(ShellAction::Redo { tab_id: data.id });
                                        ui.close();
                                    }
                                });
                            }
                        });
                    }

                    // ── Preview menu ("Preview in Runtime"): a `cooking`-only
                    // surface (Conductor). `enabled_for_product` is always true
                    // here; the compile gate is what keeps it out of Poser.
                    #[cfg(feature = "cooking")]
                    if runtime_preview_ctrl.enabled_for_product {
                        ui.menu_button("Preview", |ui| {
                            if let Some(cmd) = idtx_editor_ui::xr_controls::runtime_preview_menu_item(
                                ui,
                                runtime_preview_ctrl,
                            ) {
                                match cmd {
                                    EditorCommand::LaunchRuntimePreview => {
                                        actions.push(ShellAction::LaunchRuntimePreview);
                                    }
                                    EditorCommand::LaunchRuntimePreviewDesktop => {
                                        actions.push(ShellAction::LaunchRuntimePreviewDesktop);
                                    }
                                    EditorCommand::StopRuntimePreview => {
                                        actions.push(ShellAction::StopRuntimePreview);
                                    }
                                    // Not emitted by the runtime-preview widget;
                                    // the Build menu below dispatches builds.
                                    EditorCommand::BuildExperience(_) => {}
                                }
                            }
                        });
                    }

                    // ── Build menu: cook + package the active project for a
                    // desktop or PCVR target. The `build-tools` feature is
                    // Conductor-only, so compiling this block already means a
                    // build-capable product; `can_build` disables it while a
                    // build is running or no project is open.
                    #[cfg(feature = "build-tools")]
                    {
                        ui.menu_button("Build", |ui| {
                            ui.add_enabled_ui(can_build, |ui| {
                                if ui
                                    .button("Build Desktop Experience")
                                    .on_hover_text(
                                        "Cook the project (flatten stage, collect \
                                        behaviors, stamp run.toml) and package a \
                                        desktop bundle when a runtime binary is present.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ShellAction::BuildExperience {
                                        target: idtx_editor_model::BuildTarget::Desktop,
                                        deploy: false,
                                    });
                                    ui.close();
                                }
                                if ui
                                    .button("Build PCVR Experience")
                                    .on_hover_text(
                                        "Cook + package with the pcvr feature for an \
                                        OpenXR runtime target.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ShellAction::BuildExperience {
                                        target: idtx_editor_model::BuildTarget::Pcvr,
                                        deploy: false,
                                    });
                                    ui.close();
                                }
                                if ui
                                    .button("Build Quest Experience")
                                    .on_hover_text(
                                        "Cook the portable payload for a standalone \
                                        Quest headset (aarch64-linux-android). The \
                                        .apk is assembled by xtask/CI — the editor \
                                        produces the payload only.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ShellAction::BuildExperience {
                                        target: idtx_editor_model::BuildTarget::Quest,
                                        deploy: false,
                                    });
                                    ui.close();
                                }
                                ui.menu_button("Build Vision OS", |ui| {
                                    if ui
                                        .button("Build only")
                                        .on_hover_text(
                                            "Cook + package + sign a Vision Pro .app \
                                            (aarch64-apple-visionos) on disk. Does not \
                                            install to a device.",
                                        )
                                        .clicked()
                                    {
                                        actions.push(ShellAction::BuildExperience {
                                            target: idtx_editor_model::BuildTarget::VisionPro,
                                            deploy: false,
                                        });
                                        ui.close();
                                    }
                                    ui.add_enabled_ui(vision_pro_connected, |ui| {
                                        let btn = ui.button("Build and deploy").on_hover_text(
                                            if vision_pro_connected {
                                                "Cook + package + sign, then install + \
                                                launch on the connected Vision Pro via \
                                                devicectl."
                                            } else {
                                                "Connect + pair a Vision Pro (Developer \
                                                Mode on) to enable on-device deploy."
                                            },
                                        );
                                        if btn.clicked() {
                                            actions.push(ShellAction::BuildExperience {
                                                target: idtx_editor_model::BuildTarget::VisionPro,
                                                deploy: true,
                                            });
                                            ui.close();
                                        }
                                    });
                                });
                            });
                        });
                    }
                }); // end MenuBar
            });

            // ── Save overwrite confirmation modal ─────────────────────────────
            #[cfg(feature = "authoring")]
            if let Some(ref confirm) = save_confirm {
                let path_str = confirm.path.display().to_string();
                let tab_id = confirm.tab_id;
                let path = confirm.path.clone();
                let modal_resp = egui::Modal::new(egui::Id::new("save_confirm_modal"))
                    .show(&ctx, |ui| {
                        ui.set_min_width(360.0);
                        ui.heading("Overwrite existing file?");
                        ui.add_space(8.0);
                        ui.label(format!("Save to:\n{path_str}"));
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                actions.push(ShellAction::DismissSaveConfirm);
                            }
                            if ui.button("Save As…").clicked() {
                                actions.push(ShellAction::DismissSaveConfirm);
                                actions.push(ShellAction::SaveAs { tab_id });
                            }
                            if ui
                                .button(egui::RichText::new("Overwrite").strong())
                                .clicked()
                            {
                                actions.push(ShellAction::DoSave { tab_id, path });
                                actions.push(ShellAction::DismissSaveConfirm);
                            }
                        });
                    });
                if modal_resp.should_close() {
                    actions.push(ShellAction::DismissSaveConfirm);
                }
            }

            // ── Save-on-close confirmation modal ──────────────────────────────
            // Shown while a close request is blocked on a dirty stage. Resolves
            // the head tab of the queue; for a whole-app exit with several dirty
            // stages the modal reappears once per remaining stage.
            #[cfg(feature = "authoring")]
            if let Some((_head, label, remaining, committable)) = &close_confirm {
                let modal_resp = egui::Modal::new(egui::Id::new("close_confirm_modal"))
                    .show(&ctx, |ui| {
                        ui.set_min_width(380.0);
                        ui.heading("Unsaved changes");
                        ui.add_space(8.0);
                        ui.label(format!("“{label}” has unsaved changes."));
                        if *remaining > 1 {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} stages with unsaved changes remaining.",
                                    remaining
                                ))
                                .weak(),
                            );
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                actions.push(ShellAction::CancelClose);
                            }
                            if ui.button("Don't Save").clicked() {
                                actions.push(ShellAction::DiscardAndClose);
                            }
                            // For a remote stage with a live collab session the
                            // primary action is committing to the server's origin
                            // (its authoritative copy) rather than saving a local
                            // file, so offer "Commit Remote" in place of "Save".
                            // `committable` is only ever true in a `remote` build
                            // (the snapshot forces it false otherwise), so the
                            // commit action is gated to that feature.
                            if *committable {
                                #[cfg(feature = "remote")]
                                if ui
                                    .button(egui::RichText::new("Commit Remote").strong())
                                    .on_hover_text(
                                        "Merge this stage's session overrides into the \
                                         server's origin USD file, then close the tab.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ShellAction::CommitRemoteAndClose);
                                }
                            } else if ui
                                .button(egui::RichText::new("Save").strong())
                                .clicked()
                            {
                                actions.push(ShellAction::SaveAndClose);
                            }
                        });
                    });
                if modal_resp.should_close() {
                    actions.push(ShellAction::CancelClose);
                }
            }
            // Driven by the `import_view` snapshot of the live `import_job`
            // (abandoned jobs are filtered out, so cancelling hides the modal
            // on the next frame even though the un-interruptible worker keeps
            // running). Three phases: packaging spinner, a diagnostics
            // accept/reject prompt, and a terminal failure message.
            #[cfg(feature = "authoring")]
            if let Some((label, phase)) = &import_view {
                use crate::import::ImportPhase;
                let modal_resp = egui::Modal::new(egui::Id::new("import_modal"))
                    .show(&ctx, |ui| {
                        ui.set_min_width(560.0);
                        match phase {
                            ImportPhase::Packaging => {
                                ui.heading("Importing…");
                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(24.0));
                                    ui.add_space(6.0);
                                    ui.label(format!(
                                        "Packaging “{label}” into the project…"
                                    ));
                                });
                                ui.add_space(14.0);
                                if ui.button("Cancel").clicked() {
                                    actions.push(ShellAction::CancelImport);
                                }
                            }
                            ImportPhase::AwaitingUnresolved(unresolved) => {
                                ui.heading("Some references can't be resolved");
                                ui.add_space(8.0);
                                ui.label(format!(
                                    "“{label}” has {} reference(s) that couldn't be \
                                     resolved. Import it anyway with the broken \
                                     reference(s) removed? The original file is left \
                                     unchanged.",
                                    unresolved.len()
                                ));
                                ui.add_space(8.0);
                                egui::ScrollArea::vertical()
                                    .max_height(220.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        for asset in unresolved {
                                            ui.label(
                                                egui::RichText::new(asset)
                                                    .monospace()
                                                    .small(),
                                            );
                                            ui.separator();
                                        }
                                    });
                                ui.add_space(14.0);
                                ui.horizontal(|ui| {
                                    // Cancel is the default/primary action.
                                    if ui
                                        .button(egui::RichText::new("Cancel").strong())
                                        .clicked()
                                    {
                                        actions.push(ShellAction::CancelImport);
                                    }
                                    if ui
                                        .button(format!(
                                            "Import anyway (remove {} reference(s))",
                                            unresolved.len()
                                        ))
                                        .clicked()
                                    {
                                        actions.push(
                                            ShellAction::ImportStrippingUnresolved,
                                        );
                                    }
                                });
                            }
                            ImportPhase::AwaitingWarnings(warnings) => {
                                ui.heading("Imported with some content missing");
                                ui.add_space(8.0);
                                ui.label(format!(
                                    "Packaging “{label}” could not include the item(s) \
                                     below — they reference assets OpenUSD cannot \
                                     resolve or package, so they will be missing from \
                                     the import. Everything else is kept. Import it \
                                     into the project anyway?"
                                ));
                                ui.add_space(8.0);
                                egui::ScrollArea::vertical()
                                    .max_height(220.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        for w in warnings {
                                            ui.label(
                                                egui::RichText::new(w).monospace().small(),
                                            );
                                            ui.separator();
                                        }
                                    });
                                ui.add_space(14.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Cancel").clicked() {
                                        actions.push(ShellAction::CancelImport);
                                    }
                                    if ui
                                        .button(
                                            egui::RichText::new("Import anyway").strong(),
                                        )
                                        .clicked()
                                    {
                                        actions.push(ShellAction::AcceptImportWarnings);
                                    }
                                });
                            }
                            ImportPhase::Failed(msg) => {
                                ui.heading("Import failed");
                                ui.add_space(8.0);
                                egui::ScrollArea::vertical()
                                    .max_height(260.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        ui.label(msg);
                                    });
                                ui.add_space(14.0);
                                if ui.button("Close").clicked() {
                                    actions.push(ShellAction::DismissImportError);
                                }
                            }
                        }
                    });
                if modal_resp.should_close() {
                    // Escape / click-away: a failure is merely dismissed; an
                    // in-flight or awaiting job is cancelled (abandon/cleanup).
                    if matches!(phase, ImportPhase::Failed(_)) {
                        actions.push(ShellAction::DismissImportError);
                    } else {
                        actions.push(ShellAction::CancelImport);
                    }
                }
            }

            // ── Prim rename dialog (scene-tree "Rename…") ─────────────────────
            #[cfg(feature = "authoring")]
            if let Some(ref mut dlg) = rename_dialog_state {
                let tab_id = dlg.tab_id;
                let path = dlg.path.clone();
                let modal_resp = egui::Modal::new(egui::Id::new("prim_rename_modal"))
                    .show(&ctx, |ui| {
                        ui.set_min_width(320.0);
                        ui.heading("Rename prim");
                        ui.add_space(8.0);
                        ui.label(&path);
                        ui.add_space(8.0);
                        let edit = ui.text_edit_singleline(&mut dlg.buffer);
                        edit.request_focus();
                        let submitted = edit.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let name = dlg.buffer.trim().to_owned();
                        let valid = !name.is_empty()
                            && !name.contains('/')
                            && !name.contains(' ');
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                actions.push(ShellAction::DismissRenameDialog);
                            }
                            let commit = ui
                                .add_enabled(
                                    valid,
                                    egui::Button::new(egui::RichText::new("Rename").strong()),
                                )
                                .clicked();
                            if (commit || (submitted && valid)) && valid {
                                actions.push(ShellAction::CommitRename {
                                    tab_id,
                                    path: path.clone(),
                                    new_name: name.clone(),
                                });
                            }
                        });
                    });
                if modal_resp.should_close() {
                    actions.push(ShellAction::DismissRenameDialog);
                }
            }

            // ── Drop-target picker (scene-tree asset drop, no defaultPrim) ────
            #[cfg(feature = "authoring")]
            if let Some(ref dlg) = drop_target_picker_state {
                let tab_id = dlg.tab_id;
                let arc_noun = if dlg.as_reference { "reference" } else { "payload" };
                let modal_resp = egui::Modal::new(egui::Id::new("drop_target_picker_modal"))
                    .show(&ctx, |ui| {
                        ui.set_min_width(340.0);
                        ui.heading("Choose prim to compose");
                        ui.add_space(6.0);
                        ui.label(format!(
                            "“{}” has no defaultPrim. Pick the root prim the {arc_noun} \
                             should point at:",
                            dlg.asset_rel
                        ));
                        ui.add_space(10.0);
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for name in &dlg.candidates {
                                if ui
                                    .add(egui::Button::new(format!("/{name}")).frame(true))
                                    .clicked()
                                {
                                    actions.push(ShellAction::CommitDropTarget {
                                        tab_id,
                                        prim_name: name.clone(),
                                    });
                                }
                            }
                        });
                        ui.add_space(12.0);
                        if ui.button("Cancel").clicked() {
                            actions.push(ShellAction::DismissDropTargetPicker);
                        }
                    });
                if modal_resp.should_close() {
                    actions.push(ShellAction::DismissDropTargetPicker);
                }
            }

            // ── Startup project dialog (Conductor) ────────────────────────────
            if let Some(ref mut dlg) = startup_dialog {
                let modal_resp =
                    egui::Modal::new(egui::Id::new("startup_project_modal")).show(&ctx, |ui| {
                        ui.set_min_width(400.0);
                        if let Some(form) = dlg.creating.as_mut() {
                            // ── New Project sub-form (name + parent folder) ──────
                            ui.heading("New Project");
                            ui.add_space(6.0);
                            ui.label("Choose a name and a location for the new project.");
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.label("Name:");
                                ui.text_edit_singleline(&mut form.name);
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.button("Choose Folder…").clicked() {
                                    actions.push(ShellAction::PickNewProjectFolder);
                                }
                                match &form.parent_dir {
                                    Some(dir) => {
                                        ui.label(dir.display().to_string());
                                    }
                                    None => {
                                        ui.weak("No folder chosen");
                                    }
                                }
                            });
                            let name = form.name.trim().to_owned();
                            // Single source of truth for name validity, shared
                            // with `scaffold_new_project` so the button gate and
                            // the on-disk guard can't drift.
                            let name_error =
                                Self::project_name_error(&name);
                            let name_valid = name_error.is_none();
                            let parent = form.parent_dir.clone();
                            if let (true, Some(dir)) = (name_valid, &parent) {
                                ui.add_space(6.0);
                                ui.weak(format!("Will create: {}", dir.join(&name).display()));
                            } else if let Some(reason) = &name_error {
                                if !form.name.trim().is_empty() {
                                    ui.add_space(6.0);
                                    ui.weak(reason);
                                }
                            }
                            let ready = name_valid && parent.is_some();
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                if ui.button("Back").clicked() {
                                    actions.push(ShellAction::CancelNewProject);
                                }
                                let create = ui
                                    .add_enabled(
                                        ready,
                                        egui::Button::new(
                                            egui::RichText::new("Create Project").strong(),
                                        ),
                                    )
                                    .clicked();
                                if create && ready {
                                    if let Some(dir) = parent {
                                        actions.push(ShellAction::CreateProject {
                                            parent_dir: dir,
                                            name: name.clone(),
                                        });
                                    }
                                }
                            });
                        } else {
                            // ── Open / choose an existing project ────────────────
                            ui.set_width((ctx.content_rect().width() - 64.0).clamp(400.0, 760.0));
                            ui.heading("Open a Project");
                            ui.add_space(6.0);
                            ui.label("Choose an IDTX project to work on.");
                            ui.add_space(12.0);
                            if let Some(disc) = &dlg.discovered {
                                if ui
                                    .button(format!("↩ Open discovered project: {}", disc.name))
                                    .on_hover_text(disc.manifest_path.display().to_string())
                                    .clicked()
                                {
                                    actions.push(ShellAction::OpenProjectFile(
                                        disc.manifest_path.clone(),
                                    ));
                                }
                                ui.add_space(8.0);
                            }
                            // ── Recent projects (double-click to open) ───────────
                            if !recent_projects.is_empty() {
                                ui.label(
                                    egui::RichText::new("Recent projects").strong(),
                                );
                                ui.weak("Select a project; double-click to open.");
                                ui.add_space(8.0);
                                egui::ScrollArea::vertical()
                                    .max_height(
                                        (ctx.content_rect().height() - 260.0).clamp(72.0, 360.0),
                                    )
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        for manifest in &recent_projects {
                                            let row = crate::recent_projects_ui::project_row(
                                                ui,
                                                manifest,
                                                dlg.selected_project.as_ref() == Some(manifest),
                                            );
                                            if row.remove {
                                                if dlg.selected_project.as_ref() == Some(manifest)
                                                {
                                                    dlg.selected_project = None;
                                                }
                                                actions.push(ShellAction::RemoveRecentProject(
                                                    manifest.clone(),
                                                ));
                                            } else if row.open {
                                                actions.push(ShellAction::OpenProjectFile(
                                                    manifest.clone(),
                                                ));
                                            } else if row.selected {
                                                dlg.selected_project = Some(manifest.clone());
                                            }
                                        }
                                    });
                                ui.add_space(16.0);
                            }
                            ui.horizontal(|ui| {
                                if ui.button("Open Project…").clicked() {
                                    actions.push(ShellAction::OpenProjectPicker);
                                }
                                if ui.button("New Project…").clicked() {
                                    actions.push(ShellAction::BeginNewProject);
                                }
                                if ui.button("Cancel").clicked() {
                                    actions.push(ShellAction::CancelStartup);
                                }
                            });
                        }
                        if let Some(message) = &dlg.message {
                            ui.add_space(10.0);
                            ui.colored_label(idtx_editor_ui::theme::current().status_warn, message);
                        }
                    });
                // Intentionally ignore `modal_resp.should_close()`: the startup
                // dialog is strictly modal. Clicking the backdrop or pressing
                // Escape must NOT dismiss it — the Conductor only proceeds once a
                // project is opened/created (which clears `startup_dialog`) or the
                // user explicitly presses Cancel (which quits the app). The modal
                // is re-shown every frame while `startup_dialog` is `Some`, so
                // doing nothing here keeps it open.
                let _ = modal_resp;
            }

            // ── Configuration dialog ──────────────────────────────────────────
            if let Some(ref mut dlg) = configure_dialog_state {
                if let Some(act) =
                    idtx_editor_ui::remote_browser::app_config_window(&ctx, dlg)
                {
                    match act {
                        #[cfg(feature = "remote")]
                        AppConfigAction::Save { servers } => {
                            actions.push(ShellAction::SaveServerConfigs { servers });
                        }
                        #[cfg(feature = "remote")]
                        AppConfigAction::Test { url, username, password } => {
                            actions.push(ShellAction::TestServerConnection { url, username, password });
                        }
                        AppConfigAction::Close => {
                            configure_dialog_state = None;
                        }
                    }
                }
            }

            // ── Project Settings dialog ───────────────────────────────────────
            if let Some(ref mut dlg) = project_settings_state {
                use idtx_editor_ui::project_settings::ProjectSettingsAction;
                if let Some(act) =
                    idtx_editor_ui::project_settings::project_settings_window(&ctx, dlg)
                {
                    match act {
                        ProjectSettingsAction::Save => {
                            actions.push(ShellAction::SaveProjectSettings);
                        }
                        ProjectSettingsAction::Cancel => {
                            actions.push(ShellAction::CloseProjectSettings);
                            project_settings_state = None;
                        }
                        ProjectSettingsAction::PickPath { field } => {
                            actions.push(ShellAction::PickProjectSettingsPath { field });
                        }
                        // Levels section: map onto the shell's level command bus.
                        // None of these clear `project_settings_state`, so opening
                        // or activating a level tab leaves the dialog open.
                        ProjectSettingsAction::Level(la) => {
                            use idtx_editor_ui::level_browser::LevelBrowserAction as La;
                            match la {
                                La::Open(index) => {
                                    actions.push(ShellAction::OpenLevel { index });
                                }
                                La::Remove(index) => {
                                    actions.push(ShellAction::RemoveLevel { index });
                                }
                                La::MoveUp(index) => actions.push(ShellAction::MoveLevel {
                                    from: index,
                                    to: index.saturating_sub(1),
                                }),
                                La::MoveDown(index) => actions.push(ShellAction::MoveLevel {
                                    from: index,
                                    to: index + 1,
                                }),
                                La::NewLevel => {
                                    #[cfg(feature = "authoring")]
                                    actions.push(ShellAction::NewLevel);
                                }
                                La::AddExisting => {
                                    actions.push(ShellAction::AddExistingLevel);
                                }
                            }
                        }
                    }
                }
            }

            // ── Remote file browser ───────────────────────────────────────────
            #[cfg(feature = "remote")]
            if let Some(ref mut browser) = remote_browser_state {
                // When opened as the asset picker, exclude the current stage from
                // selection (you cannot reference a stage into itself).
                let disabled_filepath = {
                    #[cfg(all(feature = "remote", feature = "authoring"))]
                    {
                        asset_create_target_cap
                            .as_ref()
                            .map(|c| c.current_stage_filepath.as_str())
                    }
                    #[cfg(not(all(feature = "remote", feature = "authoring")))]
                    {
                        None
                    }
                };
                if let Some(act) = idtx_editor_ui::remote_browser::render_remote_browser(
                    &ctx,
                    browser,
                    &server_url_for_browser,
                    disabled_filepath,
                ) {
                    match act {
                        RemoteBrowserAction::Open(filepath) => {
                            // Asset-picker mode: turn the selection into a
                            // reference-create request instead of opening a stage.
                            let mut handled_as_asset = false;
                            #[cfg(all(feature = "remote", feature = "authoring"))]
                            if let Some(ctx) = asset_create_target_cap.as_ref() {
                                let reference_path = crate::remote::server_relative_path(
                                    &ctx.current_stage_filepath,
                                    &filepath,
                                );
                                actions.push(ShellAction::CreateRemoteAssetChild {
                                    tab_id: ctx.tab_id,
                                    parent_path: ctx.parent_path.clone(),
                                    reference_path,
                                });
                                clear_asset_create_target = true;
                                handled_as_asset = true;
                            }
                            if !handled_as_asset {
                                let dispatch_sync = {
                                    #[cfg(all(feature = "remote", feature = "authoring"))]
                                    { remote_browser_sync_mode_cap }
                                    #[cfg(not(all(feature = "remote", feature = "authoring")))]
                                    { false }
                                };
                                if dispatch_sync {
                                    #[cfg(all(feature = "remote", feature = "authoring"))]
                                    actions.push(ShellAction::OpenRemoteSyncStage {
                                        base_url: server_url_for_browser.clone(),
                                        filepath,
                                        mode: remote_session_mode_cap,
                                    });
                                } else {
                                    let url =
                                        remote_download_url(&server_url_for_browser, &filepath);
                                    actions.push(ShellAction::OpenRemoteStage(url));
                                }
                            }
                            remote_browser_state = None;
                        }
                        RemoteBrowserAction::Close => {
                            remote_browser_state = None;
                            #[cfg(all(feature = "remote", feature = "authoring"))]
                            {
                                clear_asset_create_target = true;
                            }
                        }
                        RemoteBrowserAction::RequestListing => {
                            // Keep the asset-picker in asset mode on refresh; a
                            // plain OpenRemoteDialog here would clear the context.
                            #[cfg(all(feature = "remote", feature = "authoring"))]
                            if asset_create_target_cap.is_some() {
                                actions.push(ShellAction::OpenAssetPickerDialog);
                            } else {
                                actions.push(ShellAction::OpenRemoteDialog { server: active_server_idx });
                            }
                            #[cfg(not(all(feature = "remote", feature = "authoring")))]
                            actions.push(ShellAction::OpenRemoteDialog { server: active_server_idx });
                        }
                    }
                }
            }

            // ── Session browser ("Connect Remote with Sync…") ─────────────────
            #[cfg(all(feature = "remote", feature = "authoring"))]
            if let Some(ref mut browser) = session_browser_state {
                use idtx_editor_ui::remote_browser::{SessionBrowserAction, render_session_browser};
                if let Some(act) = render_session_browser(&ctx, browser, &server_url_for_browser) {
                    match act {
                        SessionBrowserAction::Join(entry) => {
                            actions.push(ShellAction::JoinRemoteSession {
                                base_url: server_url_for_browser.clone(),
                                session_id: entry.session_id,
                                usd_file: entry.usd_file,
                                ws_url_path: entry.ws_url,
                            });
                            session_browser_state = None;
                        }
                        SessionBrowserAction::Close => {
                            session_browser_state = None;
                        }
                        SessionBrowserAction::RequestListing => {
                            actions.push(ShellAction::OpenSessionBrowserDialog { server: active_server_idx });
                        }
                    }
                }
            }

            // ── Tab bar ───────────────────────────────────────────────────────
            egui::Panel::top("tab_bar")
                .frame(idtx_editor_ui::theme::current().tab_bar_frame())
                .show(ui, |ui| {
                    use idtx_editor_ui::tab_bar::{StageTab, TabEvent, stage_tabs};
                    let stage_tab_data: Vec<StageTab> = tabs_meta
                        .iter()
                        .map(|t| StageTab {
                            id: t.id,
                            label: t.label.clone(),
                            active: t.active,
                            dirty: t.dirty,
                        })
                        .collect();
                    ui.horizontal(|ui| {
                        if let Some(event) = stage_tabs(ui, &stage_tab_data) {
                            match event {
                                TabEvent::Activate(id) => {
                                    actions.push(ShellAction::ActivateTab(id));
                                }
                                TabEvent::Close(id) => {
                                    actions.push(ShellAction::RequestCloseTab(id));
                                }
                            }
                        }
                        // Show in-flight loads as dimmed spinner tabs.
                        for lt in &loading_tabs_meta {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(
                                egui::RichText::new(lt.as_str())
                                    .color(idtx_editor_ui::theme::current().text_dim)
                                    .italics(),
                            );
                            ui.separator();
                        }
                    });
                });

            // ── Status bar ────────────────────────────────────────────────────
            egui::Panel::bottom("status_bar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(data) = &active_data {
                        ui.label(
                            egui::RichText::new(&data.source_path)
                                .monospace()
                                .small(),
                        );
                        if !data.diagnostics.is_empty() {
                            ui.separator();
                            ui.colored_label(
                                idtx_editor_ui::theme::current().status_warn,
                                format!("⚠ {} issue(s)", data.diagnostics.len()),
                            )
                            .on_hover_ui(|ui| {
                                for d in &data.diagnostics {
                                    ui.label(d);
                                }
                            });
                        }
                    }
                    if let Some(msg) = &status_msg {
                        ui.separator();
                        ui.label(egui::RichText::new(msg).small().italics());
                    }
                });
            });

            // ── Asset browser (Conductor only) ────────────────────────────────
            if show_asset_browser {
                egui::Panel::bottom("asset_browser")
                    .default_size(200.0)
                    .min_size(180.0)
                    .resizable(true)
                    .show(ui, |ui| {
                        let asset_actions = idtx_editor_ui::asset_browser::asset_browser_panel(
                            ui,
                            &asset_browser_subdir,
                            &asset_rows,
                            asset_selected_prim.as_deref(),
                            asset_authoring,
                            &mut asset_browser_state,
                        );
                        for action in asset_actions {
                            use idtx_editor_ui::asset_browser::AssetBrowserAction as Aa;
                            match action {
                                Aa::Navigate(subdir) => {
                                    actions.push(ShellAction::SetAssetDir(subdir));
                                }
                                Aa::Select(_) => {}
                                Aa::OpenStage(asset_rel_path) => {
                                    actions.push(ShellAction::OpenAssetStage { asset_rel_path });
                                }
                                Aa::AddReference(_asset_rel_path) => {
                                    #[cfg(feature = "authoring")]
                                    actions.push(ShellAction::AddReferenceToSelected {
                                        asset_rel_path: _asset_rel_path,
                                    });
                                }
                                Aa::AddPayload(_asset_rel_path) => {
                                    #[cfg(feature = "authoring")]
                                    actions.push(ShellAction::AddPayloadToSelected {
                                        asset_rel_path: _asset_rel_path,
                                    });
                                }
                            }
                        }
                    });
            }

            // ── Central content ───────────────────────────────────────────────
            egui::CentralPanel::default().show(ui, |ui| {
                if let Some(data) = &active_data {
                    // 3-column layout ──────────────────────────────────────────
                    egui::Panel::left("scene_tree_panel")
                        .resizable(true)
                        .default_size(220.0)
                        .min_size(80.0)
                        .show(ui, |ui| {
                            #[cfg(feature = "metrics")]
                            let t_tree = std::time::Instant::now();
                            scene_tree_panel(
                                ui,
                                &data.scene_tree_nodes,
                                &data.scene_tree_index,
                                &data.scene_tree_state,
                                product_supports_authoring(profile),
                                cfg!(feature = "authoring") && profile.behavior_authoring(),
                                data.collab_restricted,
                                &mut tree_actions,
                            );
                            #[cfg(feature = "metrics")]
                            scene_tree_cell.set(t_tree.elapsed().as_nanos() as u64);
                        });

                    egui::Panel::right("inspector_panel")
                        .resizable(true)
                        .default_size(280.0)
                        .min_size(100.0)
                        .show(ui, |ui| {
                            if let Some(data) = &active_data {
                                // Tab bar: Properties | Layer Stack | Stage.
                                if let Some(sel) = idtx_editor_ui::tab_bar::sub_tabs(
                                    ui,
                                    &["Properties", "Layer Stack", "Stage"],
                                    match data.inspector_tab {
                                        InspectorTab::Properties => 0,
                                        InspectorTab::LayerStack => 1,
                                        InspectorTab::Stage => 2,
                                    },
                                ) {
                                    let tab = match sel {
                                        0 => InspectorTab::Properties,
                                        1 => InspectorTab::LayerStack,
                                        _ => InspectorTab::Stage,
                                    };
                                    actions.push(ShellAction::SetInspectorTab {
                                        tab_id: data.id,
                                        tab,
                                    });
                                }
                                ui.separator();

                                match data.inspector_tab {
                                    InspectorTab::Properties => {
                                        if let (Some(instancer), Some(info)) = (&data.point_instancer_tool, &data.inspector_prim_info) {
                                            crate::point_instancer_tools::panel(ui, data.id, &info.path, instancer,
                                                product_supports_authoring(profile) && !data.collab_restricted
                                                    && matches!(inspector_mode_for_profile(profile), idtx_editor_model::InspectorMode::Cooking),
                                                &mut actions);
                                        }
                                        if let (Some(curve), Some(info)) = (&data.curve_tool, &data.inspector_prim_info) {
                                            crate::curve_tools::panel(ui, data.id, info, curve,
                                                product_supports_authoring(profile) && !data.collab_restricted,
                                                matches!(inspector_mode_for_profile(profile), idtx_editor_model::InspectorMode::Cooking),
                                                &mut actions);
                                        }
                                        let mut sink = InspectorEditSink {
                                            tab_id: data.id,
                                            actions: &mut actions,
                                        };
                                        let mode = if data.point_instancer_tool.as_ref().is_some_and(|s| s.read_only) {
                                            idtx_editor_model::InspectorMode::ReadOnly
                                        } else { inspector_mode_for_profile(profile) };
                                        let collision_hull_generating =
                                            hull_generating.as_ref().is_some_and(|(tid, p)| {
                                                *tid == data.id
                                                    && data
                                                        .inspector_prim_info
                                                        .as_ref()
                                                        .is_some_and(|info| info.path == *p)
                                            });
                                        idtx_editor_ui::inspector::panels::behavior::set_available_input_actions(
                                            ui.ctx(),
                                            behavior_input_actions.clone(),
                                        );
                                        // Publish the capability vocabulary from
                                        // the single source of truth so the
                                        // inspector's capabilities table is
                                        // agnostic to the capability set.
                                        #[cfg(feature = "cooking")]
                                        idtx_editor_ui::inspector::panels::behavior::set_available_capabilities(
                                            ui.ctx(),
                                            idtx_behavior::KNOWN_CAPABILITIES
                                                .iter()
                                                .map(|&t| {
                                                    (t.to_owned(), idtx_behavior::capability_label(t).to_owned())
                                                })
                                                .collect(),
                                        );
                                        if let (Some(info), Some(bindings)) = (&data.inspector_prim_info, &data.behavior_bindings) {
                                            idtx_editor_ui::inspector::panels::behavior::set_authored_bindings(ui.ctx(), &info.path, bindings.clone());
                                        }
                                        if let Some(info) = &data.inspector_prim_info {
                                            idtx_editor_ui::inspector::panels::animation::set_snapshot(ui.ctx(), &info.path, data.animation_inspector.clone());
                                        }
                                        inspector_panel(
                                            ui,
                                            &inspector_registry,
                                            data.inspector_prim_info.as_ref(),
                                            &data.inspector_properties,                                            data.inspector_skeleton_joints
                                                .as_ref()
                                                .map(|a| a.as_slice()),
                                            mode,
                                            Some(&mut sink),
                                            collision_hull_generating,
                                        );
                                    }
                                    InspectorTab::LayerStack => {
                                        let empty = LayerStackInfo::default();
                                        let info =
                                            data.layer_stack_info.as_deref().unwrap_or(&empty);
                                        let mut ui_state = data.layer_stack_ui.clone();
                                        let act = layer_stack_panel(
                                            ui,
                                            info,
                                            &mut ui_state,
                                            product_supports_authoring(profile),
                                        );
                                        match act {
                                            Some(LayerStackAction::Reload(identifier)) => {
                                                actions.push(ShellAction::ReloadLayer {
                                                    tab_id: data.id,
                                                    identifier,
                                                });
                                            }
                                            #[cfg(feature = "authoring")]
                                            Some(LayerStackAction::SetEditTarget(identifier)) => {
                                                actions.push(ShellAction::SetEditTarget {
                                                    tab_id: data.id,
                                                    identifier,
                                                });
                                            }
                                            #[cfg(feature = "authoring")]
                                            Some(LayerStackAction::Mute(identifier)) => {
                                                actions.push(ShellAction::MuteLayer {
                                                    tab_id: data.id,
                                                    identifier,
                                                });
                                            }
                                            #[cfg(feature = "authoring")]
                                            Some(LayerStackAction::Unmute(identifier)) => {
                                                actions.push(ShellAction::UnmuteLayer {
                                                    tab_id: data.id,
                                                    identifier,
                                                });
                                            }
                                            #[cfg(feature = "authoring")]
                                            Some(LayerStackAction::MoveSublayer {
                                                identifier,
                                                parent_identifier,
                                                stronger,
                                            }) => {
                                                actions.push(ShellAction::MoveSublayer {
                                                    tab_id: data.id,
                                                    identifier,
                                                    parent_identifier,
                                                    stronger,
                                                });
                                            }
                                            _ => {}
                                        }
                                        if ui_state != data.layer_stack_ui {
                                            actions.push(ShellAction::SetLayerStackUi {
                                                tab_id: data.id,
                                                ui_state,
                                            });
                                        }
                                    }
                                    InspectorTab::Stage => {
                                        if let (Some(info), Some(bindings)) = (&data.stage_behavior_prim_info, &data.stage_behavior_bindings) {
                                            idtx_editor_ui::inspector::panels::behavior::set_authored_bindings(ui.ctx(), &info.path, bindings.clone());
                                        }
                                        // Scrollable so the stacked (and now
                                        // individually collapsible) Stage / Shadows
                                        // / Physics / Behavior sections are never
                                        // capped at the panel's bottom edge.
                                        // `both()` so stacked sections are never
                                        // capped at the bottom edge and any wide
                                        // row can be reached by scrolling right.
                                        egui::ScrollArea::both()
                                            .auto_shrink([false, false])
                                            .show(ui, |ui| {
                                        // The shadow selector is editable only in
                                        // a shadow-capable product (Conductor) and
                                        // only when authoring is compiled in; the
                                        // read-only metadata shows everywhere.
                                        #[cfg(feature = "authoring")]
                                        let editable = profile.shadows_enabled();
                                        #[cfg(not(feature = "authoring"))]
                                        let editable = false;
                                        // Physics authoring is gated only on the
                                        // authoring feature — there is no product
                                        // capability to clear it (unlike shadows).
                                        #[cfg(feature = "authoring")]
                                        let physics_editable = true;
                                        #[cfg(not(feature = "authoring"))]
                                        let physics_editable = false;
                                        let act = stage_panel(
                                            ui,
                                            &data.stage_metadata,
                                            data.stage_shadow_mode,
                                            editable,
                                            &data.physics_state,
                                            physics_editable,
                                        );
                                        #[cfg(feature = "authoring")]
                                        match act {
                                            Some(StageAction::SetShadowMode(mode)) => {
                                                actions.push(ShellAction::SetShadowMode {
                                                    tab_id: data.id,
                                                    before: data.stage_shadow_mode,
                                                    mode,
                                                });
                                            }
                                            Some(StageAction::ActivatePhysics) => {
                                                actions.push(ShellAction::ActivatePhysics {
                                                    tab_id: data.id,
                                                });
                                            }
                                            Some(StageAction::DeactivatePhysics) => {
                                                actions.push(ShellAction::DeactivatePhysics {
                                                    tab_id: data.id,
                                                });
                                            }
                                            Some(StageAction::SetGravityDirection {
                                                before,
                                                value,
                                                commit,
                                            }) => {
                                                actions.push(ShellAction::SetGravityDirection {
                                                    tab_id: data.id,
                                                    before,
                                                    value,
                                                    commit,
                                                });
                                            }
                                            Some(StageAction::SetGravityMagnitude {
                                                before,
                                                value,
                                                commit,
                                            }) => {
                                                actions.push(ShellAction::SetGravityMagnitude {
                                                    tab_id: data.id,
                                                    before,
                                                    value,
                                                    commit,
                                                });
                                            }
                                            None => {}
                                        }
                                        #[cfg(not(feature = "authoring"))]
                                        let _ = act;

                                        // ── Stage-level behavior section ──────
                                        let sb_mode = inspector_mode_for_profile(profile);
                                        let sb_act = {
                                            let mut sb_sink = InspectorEditSink {
                                                tab_id: data.id,
                                                actions: &mut actions,
                                            };
                                            stage_behavior_panel(
                                                ui,
                                                data.stage_behavior_prim_info.as_ref(),
                                                &data.stage_behavior_properties,
                                                data.stage_behavior_run_scope,
                                                sb_mode,
                                                Some(&mut sb_sink),
                                            )
                                        };
                                        #[cfg(feature = "authoring")]
                                        match sb_act {
                                            #[cfg(feature = "cooking")]
                                            Some(StageBehaviorAction::Create) => {
                                                actions.push(ShellAction::CreateStageBehavior {
                                                    tab_id: data.id,
                                                });
                                            }
                                            #[cfg(feature = "cooking")]
                                            Some(StageBehaviorAction::Attach) => {
                                                actions.push(ShellAction::AttachStageBehavior {
                                                    tab_id: data.id,
                                                });
                                            }
                                            Some(StageBehaviorAction::SetRunScope(scope)) => {
                                                actions.push(
                                                    ShellAction::SetStageBehaviorRunScope {
                                                        tab_id: data.id,
                                                        scope,
                                                    },
                                                );
                                            }
                                            _ => {}
                                        }
                                        #[cfg(not(feature = "authoring"))]
                                        let _ = sb_act;
                                            });
                                    }
                                }
                            } else {
                                ui.add_space(16.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        egui::RichText::new("Open a USD file to inspect")
                                            .small()
                                            .weak(),
                                    );
                                });
                            }
                        });

                    // Center column: viewport + canvas ────────────────────────
                    egui::CentralPanel::default().show(ui, |ui| {
                        // Sub-tab bar: 3D View | Material Canvas | Compute Graph
                        if let Some(sel) = idtx_editor_ui::tab_bar::sub_tabs(
                            ui,
                            &["3D View", "Material Canvas", "Compute Graph"],
                            match data.center_tab {
                                CenterTab::Viewport => 0,
                                CenterTab::MaterialCanvas => 1,
                                CenterTab::ComputeGraph => 2,
                            },
                        ) {
                            let tab = match sel {
                                0 => CenterTab::Viewport,
                                1 => CenterTab::MaterialCanvas,
                                _ => CenterTab::ComputeGraph,
                            };
                            actions.push(ShellAction::SetCenterTab {
                                tab_id: data.id,
                                tab,
                            });
                        }
                        ui.separator();

                        match data.center_tab {
                            CenterTab::Viewport => {
                                // Viewport header: animation bar + root-layer metadata ─
                                egui::Panel::top("viewport_header")
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            // Animation transport bar
                                            if data.has_animation {
                                                // Stop
                                                if ui.button("⏹").on_hover_text("Stop").clicked() {
                                                    actions.push(ShellAction::AnimPlayback {
                                                        tab_id: data.id,
                                                        req: idtx_studio_session::PlaybackRequest::Stop,
                                                    });
                                                }
                                                // Play / Pause toggle
                                                let play_label = if data.playing { "⏸" } else { "▶" };
                                                let play_tip = if data.playing { "Pause" } else { "Play" };
                                                if ui.button(play_label).on_hover_text(play_tip).clicked() {
                                                    actions.push(ShellAction::AnimPlayback {
                                                        tab_id: data.id,
                                                        req: idtx_studio_session::PlaybackRequest::TogglePlay,
                                                    });
                                                }
                                                // Loop toggle
                                                let loop_vis = if data.looping {
                                                    egui::RichText::new("🔁").strong()
                                                } else {
                                                    egui::RichText::new("🔁").weak()
                                                };
                                                if ui.button(loop_vis).on_hover_text("Toggle loop").clicked() {
                                                    actions.push(ShellAction::AnimPlayback {
                                                        tab_id: data.id,
                                                        req: idtx_studio_session::PlaybackRequest::SetLooping(!data.looping),
                                                    });
                                                }
                                                // Time code label
                                                ui.label(
                                                    egui::RichText::new(format!("{:.1}", data.current_tc))
                                                        .monospace()
                                                        .small(),
                                                );
                                                // Scrub slider
                                                if data.start_tc < data.end_tc {
                                                    let mut tc = data.current_tc;
                                                    let slider = egui::Slider::new(
                                                        &mut tc,
                                                        data.start_tc..=data.end_tc,
                                                    )
                                                    .show_value(false)
                                                    .clamping(egui::SliderClamping::Always);
                                                    if ui.add(slider).changed() {
                                                        actions.push(ShellAction::AnimPlayback {
                                                            tab_id: data.id,
                                                            req: idtx_studio_session::PlaybackRequest::Scrub(tc),
                                                        });
                                                    }
                                                }
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{:.0}–{:.0}  {:.0}fps",
                                                        data.start_tc, data.end_tc, data.tcps
                                                    ))
                                                    .small()
                                                    .monospace(),
                                                );
                                            } else {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "  {:.0} – {:.0} tc  ({:.0} fps)",
                                                        data.start_tc, data.end_tc, data.tcps
                                                    ))
                                                    .small(),
                                                );
                                            }
                                            ui.separator();
                                            // Root-layer metadata
                                            ui.label(
                                                egui::RichText::new(&data.metadata_line)
                                                    .small()
                                                    .monospace(),
                                            );
                                        });
                                    });

                                // 3D render viewport ──────────────────────────
                                egui::CentralPanel::default().show(ui, |ui| {
                                    let available = ui.available_size();
                                    let rect = egui::Rect::from_min_size(
                                        ui.cursor().min,
                                        available,
                                    );

                                    // Interact first (camera navigation).
                                    let response = ui.allocate_rect(
                                        rect,
                                        egui::Sense::click_and_drag(),
                                    );

                                    // Paint the rendered texture.
                                    let mut mesh = egui::Mesh::with_texture(data.texture_id);
                                    let uv = egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    );
                                    mesh.add_rect_with_uv(rect, uv, egui::Color32::WHITE);
                                    ui.painter().add(mesh);

                                    // Remote-sync border: a 4px frame drawn only
                                    // when the stage has an active sync session.
                                    // BLUE = connected to another session,
                                    // GREEN = single/multi-edit session created,
                                    // RED = connection lost.
                                    #[cfg(feature = "remote")]
                                    if let Some(status) = data.collab_sync_status {
                                        ui.painter().rect_stroke(
                                            rect,
                                            egui::CornerRadius::ZERO,
                                            egui::Stroke::new(2.0, sync_border_color(status)),
                                            egui::StrokeKind::Inside,
                                        );
                                    }

                                    // Floating viewport toolbar (overlay).
                                    let toolbar = viewport_toolbar(
                                        &ctx, rect, data.render_mode, data.gizmo_space,
                                        data.camera_view, data.camera_ortho,
                                    );
                                    if let Some(new_mode) = toolbar.render_mode {
                                        actions.push(ShellAction::SetRenderMode {
                                            tab_id: data.id,
                                            mode: new_mode,
                                        });
                                    }
                                    if let Some(new_space) = toolbar.gizmo_space {
                                        actions.push(ShellAction::SetGizmoSpace {
                                            tab_id: data.id,
                                            space: new_space,
                                        });
                                    }
                                    if let Some(preset) =
                                        toolbar.view_preset.and_then(view_preset_from_label)
                                    {
                                        actions.push(ShellAction::SetViewPreset { preset });
                                    }
                                    if toolbar.toggle_projection {
                                        actions.push(ShellAction::ToggleProjection);
                                    }

                                    // Detect viewport hovered.
                                    new_viewport_hovered = response.hovered();

                                    // ── Transform gizmo overlay ───────────────
                                    if data.center_tab == CenterTab::Viewport {
                                        if let Some(ref gd) = data.gizmo_data {
                                            // Authoring builds: full interactive
                                            // gizmo (axes + arrows + rotation arcs)
                                            // with hit-test snapshot for dragging.
                                            #[cfg(feature = "authoring")]
                                            {
                                                let active_handle = gizmo_state
                                                    .active
                                                    .as_ref()
                                                    .map(|a| a.handle);
                                                if let Some(snap) = gizmo::draw_combined_gizmo(
                                                    ui.painter(),
                                                    rect,
                                                    gd.view_proj,
                                                    gd.camera_radius,
                                                    gd.world_mat_render,
                                                    gd.parent_world_mat_render,
                                                    gd.local_mat,
                                                    &gd.selected_path,
                                                    active_handle,
                                                    data.gizmo_space,
                                                ) {
                                                    gizmo_state.snapshot = Some(snap);
                                                }
                                            }
                                            // Read-only builds: display-only axis
                                            // lines, no arrows/arcs and no drag.
                                            #[cfg(not(feature = "authoring"))]
                                            gizmo::draw_axes_only(
                                                ui.painter(),
                                                rect,
                                                gd.view_proj,
                                                gd.camera_radius,
                                                gd.world_mat_render,
                                                data.gizmo_space,
                                            );
                                        }
                                    }

                                    // ── Blender-style camera navigation ──────
                                    //
                                    // Matches idtx-poc gesture mapping:
                                    //   MMB drag            → orbit
                                    //   Shift + MMB drag    → pan
                                    //   Ctrl/Cmd + MMB drag → dolly
                                    //   Alt + LMB drag      → orbit (trackpad fallback)
                                    //
                                    // macOS trackpad (two-finger):
                                    //   pinch               → zoom
                                    //   plain swipe         → orbit
                                    //   Shift/Alt + swipe   → pan
                                    //
                                    // Windows/Linux scroll wheel:
                                    //   plain               → zoom
                                    //   Ctrl/Cmd            → horizontal pan
                                    //   Shift               → vertical pan

                                    let (mods, scroll, zoom_delta, mmb_down, ptr_delta) =
                                        ctx.input(|i| {
                                            let mods = i.modifiers;
                                            let scroll = i.smooth_scroll_delta();
                                            let zoom_delta = i.zoom_delta();
                                            let mmb_down = i.pointer
                                                .button_down(egui::PointerButton::Middle);
                                            let ptr_delta = i.pointer.delta();
                                            (mods, scroll, zoom_delta, mmb_down, ptr_delta)
                                        });

                                    let hovered = response.hovered()
                                        || response.is_pointer_button_down_on()
                                        || response.dragged();

                                    // ── MMB drag ──────────────────────────────
                                    if mmb_down && hovered {
                                        let dx = ptr_delta.x;
                                        let dy = ptr_delta.y;
                                        if mods.shift {
                                            actions.push(ShellAction::PanCamera {
                                                delta_x: dx,
                                                delta_y: dy,
                                            });
                                        } else if mods.ctrl || mods.command {
                                            actions.push(ShellAction::DollyCamera { delta: dy });
                                        } else {
                                            actions.push(ShellAction::OrbitCamera {
                                                delta_x: dx,
                                                delta_y: dy,
                                            });
                                        }
                                    }

                                    // ── Gizmo drag input ──────────────────────
                                    #[cfg(feature = "authoring")]
                                    {
                                        let ptr_pos = ctx.input(|i| i.pointer.hover_pos());

                                        // Start: try to grab a gizmo handle on LMB press.
                                        if response.drag_started_by(egui::PointerButton::Primary)
                                            && !mods.alt
                                        {
                                            if let (Some(pos), Some(snap)) =
                                                (ptr_pos, gizmo_state.snapshot.as_ref())
                                            {
                                                if let (Some(handle), Some(ref gd)) =
                                                    (snap.pick(pos), data.gizmo_data.as_ref())
                                                {
                                                    gizmo_state.active = Some(ActiveDrag {
                                                        path: gd.selected_path.clone(),
                                                        handle,
                                                        before: gd.local_mat,
                                                        snapshot: snap.clone(),
                                                        start_pos: pos,
                                                    });
                                                }
                                            }
                                        }

                                        // Continue: author transform every frame while dragging.
                                        if response.dragged_by(egui::PointerButton::Primary) {
                                            if let (Some(active), Some(pos)) =
                                                (gizmo_state.active.as_ref(), ptr_pos)
                                            {
                                                let new_mat = gizmo::compute_dragged_matrix(active, pos);
                                                let usd_mat = gizmo::mat_to_usd(new_mat);
                                                let (t, r, s) = gizmo::decompose_usd_trs(&usd_mat);
                                                actions.push(ShellAction::AuthorTransform {
                                                    tab_id: data.id,
                                                    path: active.path.clone(),
                                                    translation: t,
                                                    rotation_xyz_degrees: r,
                                                    scale: s,
                                                });
                                            }
                                        }

                                        // End: commit the drag to the journal and clear.
                                        if response.drag_stopped_by(egui::PointerButton::Primary) {
                                            if let Some(active) = gizmo_state.active.take() {
                                                let final_pos =
                                                    ptr_pos.unwrap_or(active.start_pos);
                                                let after =
                                                    gizmo::compute_dragged_matrix(&active, final_pos);
                                                actions.push(ShellAction::CommitGizmoDrag {
                                                    tab_id: data.id,
                                                    path: active.path.clone(),
                                                    before: active.before,
                                                    after,
                                                });
                                            }
                                        }
                                    }

                                    // ── Alt + LMB drag (trackpad fallback) ───
                                    // Skip camera orbit when gizmo drag is active.
                                    let gizmo_consuming_drag = {
                                        #[cfg(feature = "authoring")]
                                        { gizmo_state.active.is_some() }
                                        #[cfg(not(feature = "authoring"))]
                                        { false }
                                    };
                                    if response.dragged_by(egui::PointerButton::Primary)
                                        && mods.alt
                                        && !gizmo_consuming_drag
                                    {
                                        let d = response.drag_delta();
                                        if mods.shift {
                                            actions.push(ShellAction::PanCamera {
                                                delta_x: d.x,
                                                delta_y: d.y,
                                            });
                                        } else {
                                            actions.push(ShellAction::OrbitCamera {
                                                delta_x: d.x,
                                                delta_y: d.y,
                                            });
                                        }
                                    }

                                    // ── LMB click → viewport pick ─────────────
                                    if response.clicked_by(egui::PointerButton::Primary)
                                        && !mods.alt
                                    {
                                        if let Some(ptr) = response.interact_pointer_pos() {
                                            actions.push(ShellAction::PickViewport {
                                                pointer: ptr,
                                                rect,
                                            });
                                        }
                                    }

                                    // ── Scroll / trackpad gestures ────────────
                                    #[cfg(target_os = "macos")]
                                    {
                                        // Pinch-to-zoom: egui reports macOS
                                        // two-finger pinch (and Ctrl+scroll
                                        // magnify) as zoom_delta. 1.0 = no
                                        // change; >1 = fingers spreading apart.
                                        // Invert so spreading fingers zooms in
                                        // (decreases distance).
                                        if hovered && (zoom_delta - 1.0).abs() > f32::EPSILON {
                                            // Map so >1.0 (spread) = zoom in
                                            // (factor < 1.0 means smaller radius).
                                            let factor = 1.0 / zoom_delta;
                                            actions.push(ShellAction::DollyFactor { factor });
                                        }

                                        let scrolled =
                                            scroll.x.abs() > 0.0 || scroll.y.abs() > 0.0;
                                        if hovered && scrolled {
                                            // macOS holds Shift while a vertical
                                            // swipe arrives on scroll_x (axis swap).
                                            // Alt leaves scroll_y clean.
                                            // Shift/Alt → pan; plain swipe → orbit.
                                            if mods.alt || mods.shift {
                                                actions.push(ShellAction::PanCamera {
                                                    delta_x: scroll.x,
                                                    delta_y: scroll.y,
                                                });
                                            } else {
                                                actions.push(ShellAction::OrbitCamera {
                                                    delta_x: scroll.x,
                                                    delta_y: scroll.y,
                                                });
                                            }
                                        }
                                    }
                                    #[cfg(not(target_os = "macos"))]
                                    {
                                        let _ = zoom_delta;
                                        let scrolled = scroll.y.abs() > 0.1;
                                        if hovered && scrolled {
                                            if mods.ctrl || mods.command {
                                                // Ctrl+scroll → horizontal pan.
                                                actions.push(ShellAction::PanCamera {
                                                    delta_x: scroll.y * 20.0,
                                                    delta_y: 0.0,
                                                });
                                            } else if mods.shift {
                                                // Shift+scroll → vertical pan.
                                                actions.push(ShellAction::PanCamera {
                                                    delta_x: 0.0,
                                                    delta_y: scroll.y * 20.0,
                                                });
                                            } else {
                                                actions.push(ShellAction::DollyCamera {
                                                    delta: scroll.y,
                                                });
                                            }
                                        }
                                    }

                                    // F key = frame all.
                                    if response.hovered()
                                        && ctx.input(|i| i.key_pressed(egui::Key::F))
                                    {
                                        actions.push(ShellAction::FrameAll);
                                    }

                                    // Detect viewport size change.
                                    let new_pixels = [
                                        (available.x * scale).round().to_u32().unwrap_or(1).max(1),
                                        (available.y * scale).round().to_u32().unwrap_or(1).max(1),
                                    ];
                                    if new_pixels != data.viewport_pixels {
                                        actions.push(ShellAction::ResizeViewport {
                                            pixels: new_pixels,
                                        });
                                    }
                                });
                            }
                            CenterTab::MaterialCanvas => {
                                egui::CentralPanel::default().show(ui, |ui| {
                                    mc::material_canvas_panel(
                                        ui,
                                        &mut canvas_state,
                                        canvas_source.as_deref(),
                                    );
                                });
                            }
                            CenterTab::ComputeGraph => {
                                egui::CentralPanel::default().show(ui, |ui| {
                                    cc::compute_canvas_panel(
                                        ui,
                                        &mut compute_state,
                                        compute_source.as_deref(),
                                    );
                                });
                            }
                        }
                    });
                } else {
                    // Empty state hint ─────────────────────────────────────────
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.35);
                            ui.label(
                                egui::RichText::new("No stage open")
                                    .heading()
                                    .color(idtx_editor_ui::theme::current().text_dim),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(
                                    "File → Open  or  Ctrl+O\nSupported: .usd  .usda  .usdc  .usdz",
                                )
                                .small()
                                .color(idtx_editor_ui::theme::current().text_dim),
                            );
                        });
                    });
                }
            });
        });

        self.viewport_hovered = new_viewport_hovered;

        // ── Viewport picks → may produce tree Select actions ──────────────────
        // `actions` is pushed to below only under remote+authoring (the
        // "Create Child → Asset…" flow); keep the binding immutable otherwise.
        #[cfg_attr(
            not(all(feature = "remote", feature = "authoring")),
            allow(unused_mut)
        )]
        let (pick_actions, mut actions): (Vec<ShellAction>, Vec<ShellAction>) = actions
            .into_iter()
            .partition(|a| matches!(a, ShellAction::PickViewport { .. }));

        for action in pick_actions {
            if let ShellAction::PickViewport { pointer, rect } = action {
                if let Some(idx) = self.active_tab_index() {
                    let packed_revealed = self.tabs[idx].scene_tree_state.packed_revealed.clone();
                    let (view_proj, _eye) = self.tabs[idx].session.camera_view_proj();
                    // Use the per-pixel near-plane point as the ray origin rather
                    // than the single camera eye.  For perspective this lies on the
                    // eye-ray (unchanged result); for the orthographic axis views the
                    // primary rays are parallel and must originate across the image
                    // plane, so a fixed eye origin would pick the same prim for every
                    // click.
                    let (ray_origin, ray_dir) = ray_from_viewport(pointer, rect, view_proj);
                    let path =
                        self.tabs[idx]
                            .session
                            .pick_and_resolve(ray_origin, ray_dir, &packed_revealed);
                    self.tabs[idx].session.set_selected(path.clone());
                    tree_actions.push(SceneTreeAction::Select(path));
                }
            }
        }

        // ── Apply scene tree actions ───────────────────────────────────────────
        if !tree_actions.is_empty() {
            if let Some(idx) = self.active_tab_index() {
                // Mirror any selection change to the session so the overlay
                // renders for tree-originated picks too.
                for action in &tree_actions {
                    match action {
                        SceneTreeAction::Select(path) => {
                            self.tabs[idx].session.set_selected(path.clone());
                        }
                        SceneTreeAction::ToggleVisibility(path) => {
                            self.tabs[idx].session.toggle_visibility(path);
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::CreateOpinion(path) => {
                            match self.journaled_structural(idx, format!("Create Opinion {path}"), |session| session.create_opinion(path)) {
                                Ok(()) => {
                                    self.window.request_redraw();
                                }
                                Err(e) => {
                                    tracing::warn!("create_opinion failed: {e}");
                                    self.status_message =
                                        Some(format!("Create Opinion failed: {e}"));
                                }
                            }
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::RemoveOpinion(path) => {
                            match self.journaled_structural(idx, format!("Remove Opinion {path}"), |session| session.remove_opinion(path)) {
                                Ok(()) => {
                                    self.window.request_redraw();
                                }
                                Err(e) => {
                                    tracing::warn!("remove_opinion failed: {e}");
                                    self.status_message =
                                        Some(format!("Remove Opinion failed: {e}"));
                                }
                            }
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::DefineChild {
                            parent_path,
                            type_name,
                        } => {
                            let type_name = type_name.clone();
                            let parent_path = parent_path.clone();
                            // On a live collab stage, prim creation is server-
                            // authoritative: send a request and let the server's
                            // broadcast author the prim (which this client also
                            // receives). Nothing is authored or journaled locally.
                            #[cfg(feature = "remote")]
                            if self.tabs[idx].session.has_collab() {
                                if self
                                    .tabs[idx]
                                    .session
                                    .request_remote_prim(&parent_path, &type_name)
                                {
                                    self.status_message = Some(format!(
                                        "Requested {type_name} from the collaboration server…"
                                    ));
                                } else {
                                    self.status_message = Some(format!(
                                        "{type_name} cannot be created in a collaboration session"
                                    ));
                                }
                                self.window.request_redraw();
                                continue;
                            }
                            match self.journaled_structural(idx, "Create Child", |s| {
                                s.define_child(&parent_path, &type_name)
                            }) {
                                Ok(Some(_new_path)) => {
                                    self.tabs[idx].session.set_selected(Some(_new_path.clone()));
                                    self.tabs[idx].scene_tree_state.selected =
                                        Some(_new_path.clone());
                                    self.window.request_redraw();
                                }
                                Ok(None) => {
                                    self.status_message = Some(format!(
                                        "Create Child: parent {parent_path} no longer exists"
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!("define_child failed: {e}");
                                    self.status_message = Some(format!("Create Child failed: {e}"));
                                }
                            }
                        }
                        #[cfg(all(feature = "remote", feature = "authoring"))]
                        SceneTreeAction::CreateAssetChild { parent_path } => {
                            let parent_path = parent_path.clone();
                            // Only meaningful on a live remote-sync stage (the
                            // menu entry is shown only there). Open the remote
                            // browser as an asset picker: capture the target and
                            // the current stage's server path, then reuse the
                            // Open-Remote listing dialog.
                            if self.tabs[idx].session.has_collab() {
                                let server_url = self
                                    .active_server_config()
                                    .map(|c| c.url.clone())
                                    .unwrap_or_default();
                                let current_stage_filepath = match self.tabs[idx].session.source() {
                                    idtx_studio_session::SessionSource::Remote { url } => {
                                        crate::remote::server_filepath_from_download_url(
                                            &server_url,
                                            url,
                                        )
                                        .unwrap_or_default()
                                    }
                                    idtx_studio_session::SessionSource::Local(_) => String::new(),
                                };
                                self.asset_create_target = Some(crate::app::AssetCreateContext {
                                    tab_id: self.tabs[idx].session.id().get(),
                                    parent_path,
                                    current_stage_filepath,
                                });
                                actions.push(ShellAction::OpenAssetPickerDialog);
                                self.window.request_redraw();
                            }
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::DefineChildFromTemplate {
                            parent_path,
                            template_key,
                        } => {
                            let Some(template) = template_from_key(template_key) else {
                                tracing::warn!("unknown prim template key: {template_key}");
                                continue;
                            };
                            let parent_path = parent_path.clone();
                            let label = format!("Create {}", template.label());
                            // Camera/light templates are not server-creatable, so
                            // on a collab stage this is always a "not supported"
                            // notice (the scene-tree menu also greys them out).
                            #[cfg(feature = "remote")]
                            if self.tabs[idx].session.has_collab() {
                                self.status_message = Some(format!(
                                    "{} cannot be created in a collaboration session",
                                    template.label()
                                ));
                                self.window.request_redraw();
                                continue;
                            }
                            match self.journaled_structural(idx, label, |s| {
                                s.define_child_from_template(&parent_path, template)
                            }) {
                                Ok(Some(_new_path)) => {
                                    self.tabs[idx].session.set_selected(Some(_new_path.clone()));
                                    self.tabs[idx].scene_tree_state.selected =
                                        Some(_new_path.clone());
                                    self.window.request_redraw();
                                }
                                Ok(None) => {
                                    self.status_message = Some(format!(
                                        "Create {}: parent {parent_path} no longer exists",
                                        template.label()
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!("define_child_from_template failed: {e}");
                                    self.status_message =
                                        Some(format!("Create {} failed: {e}", template.label()));
                                }
                            }
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::DeletePrim(path) => {
                            let path = path.clone();
                            // On a live collab stage, deletion is server-
                            // authoritative (a soft delete): send a request and
                            // let the server's broadcast author `active=false`
                            // (which this client also receives). Nothing is
                            // deleted or journaled locally — mirrors Create Child.
                            #[cfg(feature = "remote")]
                            if self.tabs[idx].session.has_collab() {
                                if self.tabs[idx].session.request_remote_prim_deletion(&path) {
                                    self.status_message = Some(format!(
                                        "Requested deletion of {path} from the collaboration server…"
                                    ));
                                } else {
                                    self.status_message =
                                        Some(format!("Could not request deletion of {path}"));
                                }
                                // Selection is intentionally *not* cleared here: the
                                // deletion is only a request. If the server rejects it
                                // the prim stays, so the selection must stay too. The
                                // selection is dropped when (and only when) the
                                // server's `active=false` broadcast actually prunes the
                                // selected subtree — see `author_remote_set_active`.
                                self.window.request_redraw();
                                continue;
                            }
                            let label = format!("Delete {path}");
                            match self.journaled_structural(idx, label, |s| s.delete_prim(&path)) {
                                Ok(()) => {
                                    // Drop selection if the removed prim was selected.
                                    if self.tabs[idx].session.selected_path() == Some(path.as_str())
                                    {
                                        self.tabs[idx].session.set_selected(None);
                                    }
                                    self.window.request_redraw();
                                }
                                Err(e) => {
                                    tracing::warn!("delete_prim failed: {e}");
                                    self.status_message = Some(format!("Delete failed: {e}"));
                                }
                            }
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::RequestRename(path) => {
                            let leaf = path.rsplit('/').next().unwrap_or(path).to_owned();
                            self.rename_dialog = Some(crate::model::RenameDialog {
                                tab_id: self.tabs[idx].session.id().get(),
                                path: path.clone(),
                                buffer: leaf,
                            });
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::AddReference(path) => {
                            if let Some(asset) = rfd::FileDialog::new()
                                .add_filter("OpenUSD stage", &["usd", "usda", "usdc", "usdz"])
                                .pick_file()
                            {
                                let asset_str = asset.to_string_lossy().into_owned();
                                let path = path.clone();
                                match self.journaled_structural(idx, "Add Reference", |s| {
                                    s.add_reference(&path, &asset_str, None)
                                }) {
                                    Ok(()) => self.window.request_redraw(),
                                    Err(e) => {
                                        tracing::warn!("add_reference failed: {e}");
                                        self.status_message =
                                            Some(format!("Add Reference failed: {e}"));
                                    }
                                }
                            }
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::AddPayload(path) => {
                            if let Some(asset) = rfd::FileDialog::new()
                                .add_filter("OpenUSD stage", &["usd", "usda", "usdc", "usdz"])
                                .pick_file()
                            {
                                let asset_str = asset.to_string_lossy().into_owned();
                                let path = path.clone();
                                match self.journaled_structural(idx, "Add Payload", |s| {
                                    s.add_payload(&path, &asset_str, None)
                                }) {
                                    Ok(()) => self.window.request_redraw(),
                                    Err(e) => {
                                        tracing::warn!("add_payload failed: {e}");
                                        self.status_message =
                                            Some(format!("Add Payload failed: {e}"));
                                    }
                                }
                            }
                        }
                        #[cfg(feature = "cooking")]
                        SceneTreeAction::AttachBehavior(path) => {
                            let path = path.clone();
                            self.perform_attach_behavior(idx, &path);
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::DropAssetReference { prim, asset_rel } => {
                            self.resolve_drop_target(idx, prim.clone(), asset_rel.clone(), true);
                        }
                        #[cfg(feature = "authoring")]
                        SceneTreeAction::DropAssetPayload { prim, asset_rel } => {
                            self.resolve_drop_target(idx, prim.clone(), asset_rel.clone(), false);
                        }
                        _ => {}
                    }
                }
                let nodes = active_data
                    .as_ref()
                    .map(|d| d.scene_tree_nodes.as_slice())
                    .unwrap_or(&[]);
                self.tabs[idx].scene_tree_state.apply(tree_actions, nodes);
            }
        }

        // ── Apply actions ─────────────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        let m_egui_run_ui_ns = t_egui.elapsed().as_nanos() as u64;
        #[cfg(feature = "metrics")]
        let m_scene_tree_ns = scene_tree_cell.get();
        // Write back remote dialog states mutated inside the egui closure
        // *before* applying actions. Actions such as `OpenConfigureDialog`
        // set `self.configure_dialog`; if the write-back ran afterwards it
        // would clobber those mutations with the stale frame-start clone,
        // opening and immediately closing the dialog in the same frame.
        self.configure_dialog = configure_dialog_state;
        self.project_settings_dialog = project_settings_state;
        // The startup dialog's "New Project" sub-form edits its name buffer in
        // the cloned local; write it back before `apply_actions` (which may
        // replace the dialog entirely on begin/dismiss/create).
        self.startup_dialog = startup_dialog;
        self.asset_browser_state = asset_browser_state;
        #[cfg(feature = "authoring")]
        {
            self.rename_dialog = rename_dialog_state;
            // NOTE: `self.drop_target_picker` is intentionally NOT written back
            // here — `resolve_drop_target` sets it in the scene-tree action loop
            // above, and this write-back would clobber that with the stale
            // frame-start snapshot. The picker modal only reads its snapshot.
        }
        #[cfg(feature = "remote")]
        {
            self.remote_browser = remote_browser_state;
        }
        #[cfg(all(feature = "remote", feature = "authoring"))]
        {
            self.session_browser = session_browser_state;
            // Drop the asset-picker context once the picker was resolved or
            // dismissed inside the closure. Safe against a fresh "Create Child →
            // Asset…" (which sets the context in the scene-tree action loop
            // *above*, earlier this frame): the resolve/dismiss clear flag and a
            // new pick come from distinct pointer events in different frames, so
            // a set and this clear never collide within one frame.
            if clear_asset_create_target {
                self.asset_create_target = None;
            }
        }

        self.apply_actions(actions)?;

        // Write back mutable egui state to the active tab.
        if let Some(idx) = self.active_tab_index() {
            self.tabs[idx].material_canvas = canvas_state;
            self.tabs[idx].compute_canvas = compute_state;
            self.tabs[idx].gizmo = gizmo_state;
        }

        // ── Render egui to surface ─────────────────────────────────────────────
        self.egui_state
            .handle_platform_output(&self.window, output.platform_output);
        let ppp = output.pixels_per_point;
        #[cfg(feature = "metrics")]
        let t_tessellate = std::time::Instant::now();
        let paint_jobs = self.egui_ctx.tessellate(output.shapes, ppp);
        #[cfg(feature = "metrics")]
        let m_tessellate_ns = t_tessellate.elapsed().as_nanos() as u64;
        let screen = ScreenDescriptor {
            size_in_pixels: [self.gpu.configuration.width, self.gpu.configuration.height],
            pixels_per_point: ppp,
        };
        for (id, delta) in &output.textures_delta.set {
            self.egui_renderer
                .update_texture(&self.gpu.device, &self.gpu.queue, *id, delta);
        }
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("IDTX Studio egui encoder"),
            });
        self.egui_renderer.update_buffers(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("IDTX Studio egui pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.015,
                                g: 0.018,
                                b: 0.025,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, &paint_jobs, &screen);
        }
        for id in &output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        #[cfg(feature = "metrics")]
        let t_submit = std::time::Instant::now();
        self.gpu.queue.submit([encoder.finish()]);
        surface_texture.present();
        #[cfg(feature = "metrics")]
        let m_submit_ns = t_submit.elapsed().as_nanos() as u64;

        // ── Record frame metrics ──────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        {
            let frame_total_ns = t_frame_total.elapsed().as_nanos() as u64;
            let ns2ms = |n: u64| n as f64 * 1e-6;
            if let Some(idx) = self.active_tab_index() {
                // Lazily create a ShellMetrics writer when a scene is loaded.
                let scene_name = self.tabs[idx].session.label().to_owned();
                let needs_new = self
                    .shell_metrics
                    .as_ref()
                    .map_or(true, |m| m.scene_name != scene_name);
                if needs_new {
                    let path = std::env::temp_dir()
                        .join(format!("idtx-frame-metrics-{}.json", scene_name));
                    eprintln!(
                        "idtx-metrics: writing per-frame metrics to {}",
                        path.display()
                    );
                    self.shell_metrics = Some(shell_metrics::ShellMetrics::new(
                        scene_name, path, 300, // flush every 300 frames (~5 s at 60 fps)
                    ));
                }
                if let Some(ref mut m) = self.shell_metrics {
                    let frame_idx = m.next_frame_index();
                    m.push(shell_metrics::ShellFrameSample {
                        frame: frame_idx,
                        frame_total_ms: ns2ms(frame_total_ns),
                        tick_ms: ns2ms(m_tick_ns),
                        tick_animation_sample_ms: ns2ms(m_anim_sample_ns),
                        render_one_frame_ms: ns2ms(m_render_one_frame_ns),
                        extract_ms: ns2ms(m_extract_ns),
                        gpu_render_ms: ns2ms(m_gpu_render_ns),
                        egui_run_ui_ms: ns2ms(m_egui_run_ui_ns),
                        scene_tree_ms: ns2ms(m_scene_tree_ns),
                        egui_tessellate_ms: ns2ms(m_tessellate_ns),
                        egui_submit_ms: ns2ms(m_submit_ns),
                        extract_materials_ms: ns2ms(m_extract_materials_ns),
                        extract_meshes_ms: ns2ms(m_extract_meshes_ns),
                        extract_instancers_ms: ns2ms(m_extract_instancers_ns),
                        extract_topo_promote_ms: ns2ms(m_extract_topo_promote_ns),
                        render_total_ms: ns2ms(m_render_phases.total_ns),
                        render_camera_lights_ms: ns2ms(m_render_phases.camera_lights_ns),
                        render_mesh_classify_ms: ns2ms(m_render_phases.mesh_classify_ns),
                        render_transform_update_ms: ns2ms(m_render_phases.transform_update_ns),
                        render_material_rebuild_ms: ns2ms(m_render_phases.material_rebuild_ns),
                        render_cache_miss_ms: ns2ms(m_render_phases.cache_miss_build_ns),
                        render_evict_ms: ns2ms(m_render_phases.evict_ns),
                        render_instance_cache_ms: ns2ms(m_render_phases.instance_cache_ns),
                        render_draw_list_ms: ns2ms(m_render_phases.draw_list_build_ns),
                        render_gpu_passes_ms: ns2ms(m_render_phases.gpu_passes_ns),
                        mesh_count: m_render_phases.mesh_count,
                        instance_count: m_render_phases.instance_count,
                        cache_hits: m_render_phases.cache_hits,
                        cache_misses: m_render_phases.cache_misses,
                        cache_transform_updates: m_render_phases.cache_transform_updates,
                        cache_material_rebuilds: m_render_phases.cache_material_rebuilds,
                        evictions: m_render_phases.evictions,
                    });
                }
            }
        }

        // Keep repainting if tabs are open (animation / USD ticks) or loads are in flight.
        if !self.tabs.is_empty() || !self.loading_tabs.is_empty() {
            self.window.request_redraw();
        }
        Ok(())
    }
}

/// Map a UI-neutral [`CameraViewLabel`] from the toolbar back to a session
/// [`ViewPreset`]. The inverse of the preset→label mapping used when building
/// `ActiveTabData`. `Perspective` has no axis preset (it is reached via the
/// projection toggle), so it returns `None`.
fn view_preset_from_label(label: CameraViewLabel) -> Option<ViewPreset> {
    match label {
        CameraViewLabel::Perspective => None,
        CameraViewLabel::Front => Some(ViewPreset::Front),
        CameraViewLabel::Back => Some(ViewPreset::Back),
        CameraViewLabel::Left => Some(ViewPreset::Left),
        CameraViewLabel::Right => Some(ViewPreset::Right),
        CameraViewLabel::Top => Some(ViewPreset::Top),
        CameraViewLabel::Bottom => Some(ViewPreset::Bottom),
    }
}

/// Map a scene-tree template key (from
/// [`idtx_editor_ui::scene_tree::SceneTreeAction::DefineChildFromTemplate`]) to
/// an [`idtx_usd::PrimTemplate`]. Keys are authored in `scene_tree.rs`'s
/// `CREATE_TYPED_PRIM_TEMPLATES` table; an unknown key returns `None` so the
/// shell can log and skip rather than author a wrong prim.
#[cfg(feature = "authoring")]
fn template_from_key(key: &str) -> Option<idtx_usd::PrimTemplate> {
    match key {
        "camera" => Some(idtx_usd::PrimTemplate::Camera),
        "distant-light" => Some(idtx_usd::PrimTemplate::DistantLight),
        "rect-light" => Some(idtx_usd::PrimTemplate::RectLight),
        "dome-light" => Some(idtx_usd::PrimTemplate::DomeLight),
        "bezier-curve" => Some(idtx_usd::PrimTemplate::BezierCurve),
        "linear-curve" => Some(idtx_usd::PrimTemplate::LinearCurve),
        "scope" => Some(idtx_usd::PrimTemplate::Scope),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::view_preset_from_label;
    use idtx_editor_ui::viewport_toolbar::CameraViewLabel;
    use idtx_studio_session::ViewPreset;

    #[test]
    fn axis_labels_map_to_matching_presets() {
        let cases = [
            (CameraViewLabel::Front, ViewPreset::Front),
            (CameraViewLabel::Back, ViewPreset::Back),
            (CameraViewLabel::Left, ViewPreset::Left),
            (CameraViewLabel::Right, ViewPreset::Right),
            (CameraViewLabel::Top, ViewPreset::Top),
            (CameraViewLabel::Bottom, ViewPreset::Bottom),
        ];
        for (label, expected) in cases {
            assert_eq!(view_preset_from_label(label), Some(expected));
        }
    }

    #[test]
    fn perspective_has_no_axis_preset() {
        // Perspective is reached via the projection toggle, not as an axis view,
        // so it must not resolve to a SetViewPreset action.
        assert_eq!(view_preset_from_label(CameraViewLabel::Perspective), None);
    }
}
