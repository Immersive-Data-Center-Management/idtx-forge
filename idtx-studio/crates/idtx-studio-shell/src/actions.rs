//! Applies the [`ShellAction`]s collected during the egui pass and launches the
//! out-of-process runtime preview.
//!
//! [`ApplicationState::apply_actions`] is the single mutation dispatcher run
//! after the UI closure returns; `launch_runtime_preview` spawns `idtx-runtime`.

#[cfg(feature = "authoring")]
use idtx_authoring::EditCommand;
use idtx_editor_ui::remote_browser::AppConfigState;
use idtx_editor_ui::viewport_toolbar::RenderMode;

use crate::app::ApplicationState;
#[cfg(feature = "authoring")]
use crate::authoring_ops::{apply_edit_command_forward, apply_edit_command_reverse};
#[cfg(feature = "authoring")]
use crate::gizmo;
#[cfg(all(feature = "remote", feature = "authoring"))]
use crate::model::PendingCollabInfo;
use crate::model::{ShellAction, ShellError};
#[cfg(feature = "cooking")]
use crate::preview_assembly;
#[cfg(all(feature = "remote", feature = "authoring"))]
use crate::remote::remote_download_url;
#[cfg(feature = "cooking")]
use crate::runtime_launch::{self, PreviewProcess};

impl ApplicationState {
    pub(crate) fn apply_actions(&mut self, actions: Vec<ShellAction>) -> Result<(), ShellError> {
        const ORBIT_SPEED: f32 = 0.007;
        const SCROLL_DOLLY: f32 = 0.05;

        for action in actions {
            match action {
                ShellAction::OpenDialog => self.open_dialog(),
                #[cfg(feature = "authoring")]
                ShellAction::ImportDialog => self.import_dialog(),
                #[cfg(feature = "authoring")]
                ShellAction::CancelImport => {
                    if let Some(job) = self.import_job.as_mut() {
                        if matches!(job.phase, crate::import::ImportPhase::Packaging) {
                            // The OpenUSD packaging call can't be interrupted;
                            // abandon the result. poll_import_job deletes the
                            // temp and clears the job once the worker returns.
                            job.abandoned = true;
                        } else {
                            // Decision phase: the worker is done, the temp
                            // exists, remove it and clear now.
                            let temp = job.temp_dest.clone();
                            self.import_job = None;
                            let _ = std::fs::remove_file(&temp);
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::AcceptImportWarnings => self.finalize_import(),
                #[cfg(feature = "authoring")]
                ShellAction::ImportStrippingUnresolved => self.spawn_import_strip_packaging(),
                #[cfg(feature = "authoring")]
                ShellAction::DismissImportError => {
                    if let Some(job) = self.import_job.as_ref() {
                        let _ = std::fs::remove_file(&job.temp_dest);
                    }
                    self.import_job = None;
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::NewStage => self.new_stage_dialog(),
                ShellAction::OpenProjectPicker => self.open_project_picker(),
                ShellAction::OpenProjectFile(path) => self.open_project_file(&path),
                ShellAction::BeginNewProject => {
                    // Reachable from the startup modal and from File → New
                    // Project…; create the dialog if it was already dismissed so
                    // the sub-form always has a host.
                    let dialog = self
                        .startup_dialog
                        .get_or_insert_with(crate::model::StartupProjectDialog::default);
                    dialog.message = None;
                    dialog.creating = Some(crate::model::NewProjectForm::default());
                    self.window.request_redraw();
                }
                ShellAction::CancelNewProject => {
                    if let Some(dialog) = self.startup_dialog.as_mut() {
                        dialog.creating = None;
                        dialog.message = None;
                    }
                    self.window.request_redraw();
                }
                ShellAction::PickNewProjectFolder => {
                    // Native folder picker runs here (post-egui pass), matching
                    // `open_project_picker`; the choice is written back into the
                    // dialog's sub-form for the next frame.
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        if let Some(form) = self
                            .startup_dialog
                            .as_mut()
                            .and_then(|d| d.creating.as_mut())
                        {
                            form.parent_dir = Some(dir);
                        }
                    }
                    self.window.request_redraw();
                }
                ShellAction::CreateProject { parent_dir, name } => {
                    self.create_project(&parent_dir, &name);
                }
                ShellAction::CancelStartup => {
                    // No project chosen → nothing to edit; quit the app. The
                    // winit loop exits on the next redraw once `should_exit` is
                    // set (see `app::ApplicationHandler`).
                    self.should_exit = true;
                    self.window.request_redraw();
                }
                ShellAction::RemoveRecentProject(path) => {
                    self.recent_projects.retain(|p| p != &path);
                    crate::recent::save_recent_projects(&self.recent_projects);
                    self.window.request_redraw();
                }
                ShellAction::OpenRecent(path) => {
                    if path.exists() {
                        self.open_path(&path, None, None, false);
                    } else {
                        // Drop the dead entry and persist the pruned list.
                        self.recent_files.retain(|p| p != &path);
                        crate::recent::save_recent(&self.recent_files);
                        self.status_message = Some(format!(
                            "File not found: {}",
                            idtx_project::normalize_display_path(&path)
                        ));
                    }
                }
                ShellAction::Reload => self.reload_active(),
                ShellAction::ActivateTab(id) => {
                    if self.tabs.iter().any(|t| t.session.id().get() == id) {
                        self.active_tab_id = Some(id);
                        self.window.request_redraw();
                    }
                }
                ShellAction::RequestCloseTab(id) => self.request_close_tab(id),
                #[cfg(feature = "authoring")]
                ShellAction::SaveAndClose => self.save_and_close(),
                #[cfg(feature = "authoring")]
                ShellAction::DiscardAndClose => self.finish_close_head(true),
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::CommitRemoteAndClose => self.commit_remote_and_close(),
                #[cfg(feature = "authoring")]
                ShellAction::CancelClose => {
                    self.pending_close = None;
                    self.window.request_redraw();
                }
                ShellAction::SetCenterTab { tab_id, tab } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        t.center_tab = tab;
                    }
                }
                ShellAction::SetInspectorTab { tab_id, tab } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        t.inspector_tab = tab;
                    }
                }
                ShellAction::SetLayerStackUi { tab_id, ui_state } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        t.layer_stack_ui = ui_state;
                    }
                }
                ShellAction::ReloadLayer { tab_id, identifier } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = t.session.reload_layer(&identifier) {
                            self.status_message = Some(format!("Reload failed: {e}"));
                        } else {
                            // Force the layer-stack cache to refetch on the next
                            // frame; a reload can flip dirty/delta/topology state.
                            t.layer_stack_cache.info = None;
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetEditTarget { tab_id, identifier } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = t.session.set_edit_target(&identifier) {
                            self.status_message = Some(format!("Set edit target failed: {e}"));
                        } else {
                            t.layer_stack_cache.info = None;
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::MuteLayer { tab_id, identifier } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = t.session.mute_layer(&identifier) {
                            self.status_message = Some(format!("Mute failed: {e}"));
                        } else {
                            t.layer_stack_cache.info = None;
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::UnmuteLayer { tab_id, identifier } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = t.session.unmute_layer(&identifier) {
                            self.status_message = Some(format!("Un-mute failed: {e}"));
                        } else {
                            t.layer_stack_cache.info = None;
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::MoveSublayer {
                    tab_id,
                    identifier,
                    parent_identifier,
                    stronger,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) =
                            t.session
                                .move_sublayer(&identifier, &parent_identifier, stronger)
                        {
                            self.status_message = Some(format!("Reorder failed: {e}"));
                        } else {
                            t.layer_stack_cache.info = None;
                        }
                    }
                    self.window.request_redraw();
                }
                ShellAction::SetRenderMode { tab_id, mode } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        t.render_mode = mode;
                        t.session.render_one_frame(mode == RenderMode::Wireframe);
                    }
                    self.window.request_redraw();
                }
                ShellAction::SetGizmoSpace { tab_id, space } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        t.gizmo_space = space;
                    }
                }
                ShellAction::OrbitCamera { delta_x, delta_y } => {
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx]
                            .session
                            .orbit_camera(delta_x * ORBIT_SPEED, delta_y * ORBIT_SPEED);
                        self.window.request_redraw();
                    }
                }
                ShellAction::PanCamera { delta_x, delta_y } => {
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx].session.pan_camera(-delta_x, delta_y);
                        self.window.request_redraw();
                    }
                }
                ShellAction::DollyCamera { delta } => {
                    if let Some(idx) = self.active_tab_index() {
                        let factor = if delta > 0.0 {
                            1.0 - SCROLL_DOLLY
                        } else {
                            1.0 + SCROLL_DOLLY
                        };
                        self.tabs[idx].session.dolly_camera(factor);
                        self.window.request_redraw();
                    }
                }
                ShellAction::DollyFactor { factor } => {
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx].session.dolly_camera(factor);
                        self.window.request_redraw();
                    }
                }
                ShellAction::FrameAll => {
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx].session.frame_all();
                        self.window.request_redraw();
                    }
                }
                ShellAction::SetViewPreset { preset } => {
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx].session.set_view_preset(preset);
                        self.window.request_redraw();
                    }
                }
                ShellAction::ToggleProjection => {
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx].session.toggle_projection();
                        self.window.request_redraw();
                    }
                }
                ShellAction::ResizeViewport { pixels } => {
                    if let Some(idx) = self.active_tab_index() {
                        if self.tabs[idx].last_viewport_pixels != pixels {
                            self.tabs[idx].last_viewport_pixels = pixels;
                            self.tabs[idx].session.resize_viewport(pixels);
                            // Re-render with new size before updating the egui
                            // texture pointer so there's no single-frame blank.
                            let wireframe = self.tabs[idx].render_mode == RenderMode::Wireframe;
                            self.tabs[idx].session.render_one_frame(wireframe);
                            self.egui_renderer.update_egui_texture_from_wgpu_texture(
                                &self.gpu.device,
                                self.tabs[idx].session.display_view(),
                                wgpu::FilterMode::Linear,
                                self.tabs[idx].texture_id,
                            );
                        }
                    }
                }
                ShellAction::PickViewport { .. } => {
                    // Handled before apply_actions is called (see render()).
                }
                ShellAction::LaunchRuntimePreview => {
                    #[cfg(feature = "cooking")]
                    self.launch_runtime_preview(preview_assembly::PreviewMode::Xr);
                }
                ShellAction::LaunchRuntimePreviewDesktop => {
                    #[cfg(feature = "cooking")]
                    self.launch_runtime_preview(preview_assembly::PreviewMode::Desktop);
                }
                ShellAction::StopRuntimePreview => {
                    #[cfg(feature = "cooking")]
                    if let Some(mut proc) = self.preview_process.take() {
                        proc.stop();
                        self.status_message = Some("Runtime preview stopped.".to_owned());
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "build-tools")]
                ShellAction::BuildExperience { target, deploy } => {
                    self.start_build(target, deploy);
                }
                ShellAction::SetVariantSelection {
                    tab_id,
                    path,
                    set_name,
                    before: _,
                    selection,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        #[cfg(feature = "authoring")]
                        let before_opinion = match t.session.session_variant_opinion(&path, &set_name) {
                            Ok(value) => value,
                            Err(error) => { self.status_message = Some(error.to_string()); continue; }
                        };
                        if let Err(e) = t.session.set_session_variant(&path, &set_name, &selection)
                        {
                            tracing::warn!("set_session_variant failed: {e}");
                            self.status_message = Some(e.to_string());
                            continue;
                        }
                        #[cfg(feature = "authoring")]
                        t.journal
                            .push(idtx_authoring::EditCommand::SessionVariant {
                                path,
                                set_name,
                                before: before_opinion,
                                after: Some(selection),
                            });
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::AuthorAttribute {
                    tab_id,
                    path,
                    property,
                    before: _,
                    value,
                    target,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = crate::authoring_ops::capture_property_edit(&mut t.journal, &t.session, &path, &property)
                            .and_then(|()| t.session.author_attribute_targeted(&path, &property, value.clone(), &target))
                        {
                            tracing::warn!("author_attribute failed: {e}");
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::CommitAttributeEdit {
                    tab_id,
                    path,
                    property,
                    before,
                    value,
                    target,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if !t.journal.has_property_capture(&path, &property) && before.as_ref() == Some(&value) { continue; }
                        if let Err(e) = crate::authoring_ops::capture_property_edit(&mut t.journal, &t.session, &path, &property)
                            .and_then(|()| t.session.author_attribute_targeted(&path, &property, value.clone(), &target))
                            .and_then(|()| crate::authoring_ops::finish_property_edit(&mut t.journal, &t.session, &path, &property))
                        {
                            tracing::warn!("CommitAttributeEdit author_attribute failed: {e}");
                            self.status_message = Some(format!("Attribute edit failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::AuthorTransform {
                    tab_id,
                    path,
                    translation,
                    rotation_xyz_degrees,
                    scale,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = crate::authoring_ops::capture_transform_edit(&mut t.journal, &t.session, &path)
                            .and_then(|()| t.session.author_transform(
                            &path,
                            translation,
                            rotation_xyz_degrees,
                            scale,
                        )) {
                            tracing::warn!("author_transform failed: {e}");
                            self.status_message = Some(format!("Transform edit failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetShadowMode {
                    tab_id,
                    before: _,
                    mode,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = crate::authoring_ops::journal_root_edit(&mut t.journal, &mut t.session,
                            format!("Shadow Mode {mode:?}"), |session| session.set_stage_shadow_mode(mode)) {
                            tracing::warn!("set_stage_shadow_mode failed: {e}");
                            self.status_message = Some(format!("Shadow mode edit failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::ActivatePhysics { tab_id } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        match self.journaled_structural(idx, "Activate Physics", |s| {
                            s.activate_physics()
                        }) {
                            Ok(()) => self.window.request_redraw(),
                            Err(e) => {
                                tracing::warn!("activate_physics failed: {e}");
                                self.status_message =
                                    Some(format!("Activate Physics failed: {e}"));
                            }
                        }
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::DeactivatePhysics { tab_id } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        match self.journaled_structural(idx, "Deactivate Physics", |s| {
                            s.deactivate_physics()
                        }) {
                            Ok(()) => self.window.request_redraw(),
                            Err(e) => {
                                tracing::warn!("deactivate_physics failed: {e}");
                                self.status_message =
                                    Some(format!("Deactivate Physics failed: {e}"));
                            }
                        }
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetGravityDirection {
                    tab_id,
                    before: _,
                    value,
                    commit,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        // Stream the live value every frame (cheap FFI, no
                        // journal); journal a single before→after entry on commit.
                        let path = t.session.physics_scene_path();
                        let property = idtx_usd_extension::ATTR_PHYSICS_GRAVITY_DIRECTION;
                        let capture = path.as_ref().map_or(Ok(()), |path| crate::authoring_ops::capture_property_edit(&mut t.journal, &t.session, path, property));
                        if let Err(e) = capture.and_then(|()| t.session.set_physics_gravity_direction(value)) {
                            tracing::warn!("set_physics_gravity_direction failed: {e}");
                            self.status_message =
                                Some(format!("Set Gravity Direction failed: {e}"));
                        } else if commit && let Some(path) = path
                        {
                            if let Err(error) = crate::authoring_ops::finish_property_edit(&mut t.journal, &t.session, &path, property) {
                                self.status_message = Some(error.to_string());
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetGravityMagnitude {
                    tab_id,
                    before: _,
                    value,
                    commit,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        let path = t.session.physics_scene_path();
                        let property = idtx_usd_extension::ATTR_PHYSICS_GRAVITY_MAGNITUDE;
                        let capture = path.as_ref().map_or(Ok(()), |path| crate::authoring_ops::capture_property_edit(&mut t.journal, &t.session, path, property));
                        if let Err(e) = capture.and_then(|()| t.session.set_physics_gravity_magnitude(value)) {
                            tracing::warn!("set_physics_gravity_magnitude failed: {e}");
                            self.status_message =
                                Some(format!("Set Gravity Magnitude failed: {e}"));
                        } else if commit && let Some(path) = path
                        {
                            if let Err(error) = crate::authoring_ops::finish_property_edit(&mut t.journal, &t.session, &path, property) {
                                self.status_message = Some(error.to_string());
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::ApplyApiSchema {
                    tab_id,
                    path,
                    schema,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = crate::authoring_ops::journal_root_edit(&mut t.journal, &mut t.session,
                            format!("Add {schema}"), |session| session.apply_api_schema(&path, &schema)) {
                            tracing::warn!("apply_api_schema failed: {e}");
                            self.status_message = Some(format!("Add {schema} failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::RemoveApiSchema {
                    tab_id,
                    path,
                    schema,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = crate::authoring_ops::journal_root_edit(&mut t.journal, &mut t.session,
                            format!("Remove {schema}"), |session| session.remove_api_schema(&path, &schema)) {
                            tracing::warn!("remove_api_schema failed: {e}");
                            self.status_message = Some(format!("Remove {schema} failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::AddCollisionApi { tab_id, path } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        // Snapshot the root layer around the apply so the applied
                        // schemas AND the opinions they author (physics:* +
                        // idtx:collision:*) undo/redo together as one entry — the
                        // symmetric counterpart of Remove Collision. A no-op apply
                        // (target absent) leaves the snapshots equal, so
                        // `journaled_structural` drops the entry automatically.
                        match self
                            .journaled_structural(idx, "Add Collision", |s| {
                                s.apply_collision_api(&path)
                            }) {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("apply_collision_api failed: {e}");
                                self.status_message = Some(format!("Add Collision failed: {e}"));
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::RemoveCollisionApi { tab_id, path } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        // Snapshot the root layer around the removal so the schema
                        // deletions and the cleared opinions (channel + the bulky
                        // hull arrays) undo/redo together as one entry. A no-op
                        // removal (nothing applied) leaves the snapshots equal, so
                        // `journaled_structural` drops the entry automatically.
                        match self
                            .journaled_structural(idx, "Remove Collision", |s| {
                                s.remove_collision_api(&path)
                            }) {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("remove_collision_api failed: {e}");
                                self.status_message =
                                    Some(format!("Remove Collision failed: {e}"));
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::GenerateCollisionHull {
                    tab_id,
                    path,
                    params,
                } => {
                    if self.hull_job.is_some() {
                        self.status_message =
                            Some("Collision hull generation already in progress".to_owned());
                    } else {
                        // Gather the mesh point cloud on the main thread (the ECS
                        // world is not `Send`); only the owned points cross to the
                        // worker. Ending the `self.tabs` borrow before touching
                        // `self.hull_job` keeps the borrow checker happy.
                        let points = self
                            .tabs
                            .iter()
                            .find(|t| t.session.id().get() == tab_id)
                            .map(|t| t.session.gather_collision_mesh_points(&path));
                        match points {
                            // Tab gone, or too little geometry to enclose.
                            None => {}
                            Some(points) if points.len() < 4 => {
                                self.status_message = Some(format!(
                                    "Generate CollisionHull: {path} has no mesh geometry to hull"
                                ));
                            }
                            Some(points) => {
                                let hull_params = idtx_hull_gen::HullParams {
                                    max_input_points: usize::try_from(params.max_input_points)
                                        .unwrap_or(usize::MAX),
                                    max_hull_vertices: usize::try_from(params.max_hull_vertices)
                                        .unwrap_or(idtx_hull_gen::DEFAULT_MAX_HULL_VERTICES),
                                };
                                self.status_message =
                                    Some(format!("Generating collision hull for {path}…"));
                                self.hull_job = Some(crate::hull_job::spawn(
                                    points, hull_params, tab_id, path,
                                ));
                            }
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::RemovePropertyOpinion {
                    tab_id,
                    path,
                    property,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        // Snapshot the root-layer opinion for a lossless undo,
                        // then remove it. An empty snapshot means nothing was
                        // authored in the root layer — skip both the edit and
                        // the journal entry.
                        let snapshot = t
                            .session
                            .snapshot_root_property(&path, &property)
                            .unwrap_or_else(|e| {
                                tracing::warn!("snapshot_root_property failed: {e}");
                                String::new()
                            });
                        if !snapshot.is_empty() {
                            if let Err(e) = t.session.clear_property_opinion(&path, &property) {
                                tracing::warn!("clear_property_opinion failed: {e}");
                                self.status_message = Some(format!("Remove opinion failed: {e}"));
                            } else {
                                t.journal.push(EditCommand::ClearProperty {
                                    path,
                                    property,
                                    snapshot,
                                });
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::ResetTransform {
                    tab_id,
                    path,
                    properties,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        // Snapshot each root-layer transform opinion, then clear
                        // it. Only properties with a non-empty snapshot are
                        // actually authored in the root layer.
                        let mut snapshots: Vec<(String, String)> = Vec::new();
                        for property in properties {
                            let snapshot = t
                                .session
                                .snapshot_root_property(&path, &property)
                                .unwrap_or_else(|e| {
                                    tracing::warn!("snapshot_root_property failed: {e}");
                                    String::new()
                                });
                            if snapshot.is_empty() {
                                continue;
                            }
                            if let Err(e) = t.session.clear_property_opinion(&path, &property) {
                                tracing::warn!("clear_property_opinion failed: {e}");
                                self.status_message = Some(format!("Reset transform failed: {e}"));
                            } else {
                                snapshots.push((property, snapshot));
                            }
                        }
                        if !snapshots.is_empty() {
                            t.journal
                                .push(EditCommand::ResetTransform { path, snapshots });
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::CommitGizmoDrag {
                    tab_id,
                    path,
                    before: _,
                    after,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        // Author the final position (ensures correctness even if the last
                        // dragged_by frame didn't author it at exactly this position).
                        let usd_mat = gizmo::mat_to_usd(after);
                        let (trans, rot, scale) = gizmo::decompose_usd_trs(&usd_mat);
                        if let Err(e) = crate::authoring_ops::capture_transform_edit(&mut t.journal, &t.session, &path)
                            .and_then(|()| t.session.author_transform(&path, trans, rot, scale))
                            .and_then(|()| crate::authoring_ops::finish_transform_edit(&mut t.journal, &t.session, &path)) {
                            tracing::warn!("CommitGizmoDrag author_transform failed: {e}");
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::CommitTransformEdit {
                    tab_id,
                    path,
                    before: _,
                    translation,
                    rotation_xyz_degrees,
                    scale,
                    before_trs: _,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        // Re-author the confirmed TRS so the journaled "after"
                        // matches the stage exactly, independent of action ordering.
                        if let Err(e) = crate::authoring_ops::capture_transform_edit(&mut t.journal, &t.session, &path)
                            .and_then(|()| t.session.author_transform(
                            &path,
                            translation,
                            rotation_xyz_degrees,
                            scale,
                        ))
                            .and_then(|()| crate::authoring_ops::finish_transform_edit(&mut t.journal, &t.session, &path)) {
                            tracing::warn!("CommitTransformEdit author_transform failed: {e}");
                            self.status_message = Some(format!("Transform edit failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::Undo { tab_id } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(error) = t.journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut t.session)) {
                            self.status_message = Some(format!("Undo failed: {error}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::Redo { tab_id } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(error) = t.journal.try_redo(|cmd| apply_edit_command_forward(cmd, &mut t.session)) {
                            self.status_message = Some(format!("Redo failed: {error}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::Save { tab_id } => self.begin_save(tab_id),
                #[cfg(feature = "authoring")]
                ShellAction::SaveAs { tab_id } => self.save_as_dialog(tab_id),
                #[cfg(feature = "authoring")]
                ShellAction::DoSave { tab_id, path } => {
                    // Defer the actual (multi-second, blocking) save to the top
                    // of the next frame, where no swapchain image is held. See
                    // `pending_saves`. Running it here would hold the acquired
                    // surface texture across the save and wedge the next acquire.
                    self.pending_saves.push((tab_id, path));
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::DismissSaveConfirm => {
                    self.pending_save_confirm = None;
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::CommitRename {
                    tab_id,
                    path,
                    new_name,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        let before = t.session.root_layer_snapshot().ok();
                        match t.session.rename_prim(&path, &new_name) {
                            Ok(()) => {
                                // Rename is structural (re-authors the root layer
                                // subtree under a new name); record it as a
                                // snapshot-based undo entry like the other
                                // structural edits.
                                if let (Some(before), Ok(after)) =
                                    (before, t.session.root_layer_snapshot())
                                {
                                    t.journal.push(idtx_authoring::EditCommand::Structural {
                                        label: format!("Rename {path}"),
                                        before,
                                        after,
                                    });
                                }
                                self.window.request_redraw();
                            }
                            Err(e) => {
                                tracing::warn!("rename_prim failed: {e}");
                                self.status_message = Some(format!("Rename failed: {e}"));
                            }
                        }
                    }
                    self.rename_dialog = None;
                }
                #[cfg(feature = "authoring")]
                ShellAction::DismissRenameDialog => {
                    self.rename_dialog = None;
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::CommitDropTarget { tab_id, prim_name } => {
                    // Author the pending drop's arc targeting the chosen prim.
                    // Take the picker first so the stored drop params survive the
                    // `&mut self` authoring call.
                    let picked = self.drop_target_picker.take();
                    let idx = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id);
                    if let (Some(dlg), Some(idx)) = (picked, idx) {
                        let target = format!("/{prim_name}");
                        self.author_dropped_asset(
                            idx,
                            &dlg.parent_prim,
                            &dlg.asset_rel,
                            dlg.as_reference,
                            Some(&target),
                        );
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "authoring")]
                ShellAction::DismissDropTargetPicker => {
                    self.drop_target_picker = None;
                    self.window.request_redraw();
                }
                // ── Behavior attachment ──────────────────────────────
                // Authoring + tooling only; none of these play/run a behavior.
                #[cfg(feature = "cooking")]
                ShellAction::AttachBehavior { tab_id, path } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        self.perform_attach_behavior(idx, &path);
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::CreateBehavior { tab_id, path } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        // Snapshot the root layer so the authored source triple is
                        // undoable. The template `.rs`/manifest files written to
                        // `behaviors/` are not part of the layer and stay on disk on
                        // undo (they are inert until referenced).
                        let before = self.tabs[idx].session.root_layer_snapshot().ok();
                        match crate::behavior_tools::create_behavior_source(
                            &mut self.tabs[idx].session,
                            &path,
                        ) {
                            Ok(_source_asset) => {
                                if let (Some(before), Ok(after)) =
                                    (before, self.tabs[idx].session.root_layer_snapshot())
                                {
                                    self.tabs[idx].journal.push(
                                        idtx_authoring::EditCommand::Structural {
                                            label: "Create Behavior".to_owned(),
                                            before,
                                            after,
                                        },
                                    );
                                }
                                // Kick a non-blocking cargo check of the fresh source;
                                // the `.wasm` itself is only built at preview/package.
                                let ctx = self.egui_ctx.clone();
                                crate::behavior_tools::start_check(
                                    &ctx,
                                    &mut self.tabs[idx].session,
                                    &path,
                                );
                                self.window.request_redraw();
                            }
                            Err(e) => {
                                tracing::warn!("create_behavior failed: {e}");
                                self.status_message = Some(format!("Create Behavior failed: {e}"));
                            }
                        }
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::OverwriteBehavior { tab_id, path } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        // Snapshot the root layer so the carried-over config + the
                        // fresh source triple are undoable in one step. The template
                        // `.rs` written to `behaviors/` is inert until referenced and
                        // stays on disk on undo (mirrors Create Behavior).
                        let before = self.tabs[idx].session.root_layer_snapshot().ok();
                        match crate::behavior_tools::overwrite_behavior(
                            &mut self.tabs[idx].session,
                            &path,
                        ) {
                            Ok(_source_asset) => {
                                if let (Some(before), Ok(after)) =
                                    (before, self.tabs[idx].session.root_layer_snapshot())
                                {
                                    self.tabs[idx].journal.push(
                                        idtx_authoring::EditCommand::Structural {
                                            label: "Overwrite Behavior".to_owned(),
                                            before,
                                            after,
                                        },
                                    );
                                }
                                // Kick a non-blocking cargo check of the fresh source;
                                // the `.wasm` itself is only built at preview/package.
                                let ctx = self.egui_ctx.clone();
                                crate::behavior_tools::start_check(
                                    &ctx,
                                    &mut self.tabs[idx].session,
                                    &path,
                                );
                                self.window.request_redraw();
                            }
                            Err(e) => {
                                tracing::warn!("overwrite_behavior failed: {e}");
                                self.status_message =
                                    Some(format!("Overwrite Behavior failed: {e}"));
                            }
                        }
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::DetachBehavior { tab_id, path } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        // Detaching the stage behavior removes the whole slot
                        // (prim + root-layer metadata), not just the attachment.
                        let is_stage = path == idtx_studio_session::STAGE_BEHAVIOR_PRIM_PATH;
                        let (label, result) = if is_stage {
                            (
                                "Remove Stage Behavior",
                                self.journaled_structural(idx, "Remove Stage Behavior", |s| {
                                    s.remove_stage_behavior()
                                }),
                            )
                        } else {
                            (
                                "Detach Behavior",
                                self.journaled_structural(idx, "Detach Behavior", |s| {
                                    s.detach_behavior(&path)
                                }),
                            )
                        };
                        match result {
                            Ok(()) => self.window.request_redraw(),
                            Err(e) => {
                                tracing::warn!("{label} failed: {e}");
                                self.status_message = Some(format!("{label} failed: {e}"));
                            }
                        }
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::CreateStageBehavior { tab_id } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        self.perform_create_stage_behavior(idx);
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::AttachStageBehavior { tab_id } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        self.perform_attach_stage_behavior(idx);
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetStageBehaviorRunScope { tab_id, scope } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        match self.journaled_structural(idx, "Stage Behavior Run Condition", |s| {
                            s.set_stage_behavior_run_scope(scope)
                        }) {
                            Ok(()) => self.window.request_redraw(),
                            Err(e) => {
                                tracing::warn!("set_stage_behavior_run_scope failed: {e}");
                                self.status_message =
                                    Some(format!("Run condition change failed: {e}"));
                            }
                        }
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::EditBehaviorSource { tab_id, path } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) =
                            crate::behavior_tools::edit_behavior_source(&mut t.session, &path)
                        {
                            tracing::warn!("edit_behavior_source failed: {e}");
                            self.status_message = Some(format!("Edit Code failed: {e}"));
                        }
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::CheckBehavior { tab_id, path } => {
                    let ctx = self.egui_ctx.clone();
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        crate::behavior_tools::start_check(&ctx, &mut t.session, &path);
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "cooking")]
                ShellAction::VerifyBehavior { tab_id, path } => {
                    let ctx = self.egui_ctx.clone();
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        crate::behavior_tools::start_verify(&ctx, &mut t.session, &path);
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetBehaviorEnabled {
                    tab_id,
                    path,
                    enabled,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = t.session.set_behavior_enabled(&path, enabled) {
                            tracing::warn!("set_behavior_enabled failed: {e}");
                            self.status_message = Some(format!("Set behavior enabled failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetBehaviorKind { tab_id, path, kind } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        let kind = idtx_studio_session::BehaviorKind::from_token(&kind);
                        if let Err(e) = t.session.set_behavior_kind(&path, kind) {
                            tracing::warn!("set_behavior_kind failed: {e}");
                            self.status_message = Some(format!("Set behavior kind failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetBehaviorScope {
                    tab_id,
                    path,
                    scope,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        let scope = idtx_studio_session::BehaviorScope::from_token(&scope);
                        if let Err(e) = t.session.set_behavior_scope(&path, scope) {
                            tracing::warn!("set_behavior_scope failed: {e}");
                            self.status_message = Some(format!("Set behavior scope failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetBehaviorServerAuthoritative {
                    tab_id,
                    path,
                    value,
                } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        if let Err(e) = t.session.set_behavior_server_authoritative(&path, value) {
                            tracing::warn!("set_behavior_server_authoritative failed: {e}");
                            self.status_message =
                                Some(format!("Set server-authoritative failed: {e}"));
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "authoring")]
                ShellAction::SetBehaviorProperty {
                    tab_id,
                    path,
                    property,
                    value,
                } => {
                    if let Some(idx) = self
                        .tabs
                        .iter()
                        .position(|t| t.session.id().get() == tab_id)
                    {
                        match self.journaled_structural(idx, "Edit Behavior Config", |s| {
                            s.set_behavior_attribute(&path, &property, value.clone())
                        }) {
                            Ok(()) => self.window.request_redraw(),
                            Err(e) => {
                                tracing::warn!("set_behavior_attribute failed: {e}");
                                self.status_message =
                                    Some(format!("Edit behavior config failed: {e}"));
                            }
                        }
                    }
                }
                ShellAction::OpenConfigureDialog => {
                    let mut dlg = AppConfigState::default();
                    #[cfg(feature = "remote")]
                    {
                        dlg.rows = self
                            .servers
                            .iter()
                            .map(|c| idtx_editor_ui::remote_browser::ServerConfigDraft {
                                description: c.description.clone(),
                                url: c.url.clone(),
                                username: c.username.clone(),
                                // Prefill from the keychain so the user isn't
                                // forced to retype the password on every edit.
                                // Falls back to empty if the secret can't be
                                // resolved.
                                password: idtx_remote::resolve_password(&c.password_hint)
                                    .unwrap_or_default(),
                                status: None,
                                status_ok: false,
                            })
                            .collect();
                    }
                    self.configure_dialog = Some(dlg);
                    self.window.request_redraw();
                }
                ShellAction::OpenProjectSettings => {
                    if let Some(project) = self.active_project.as_ref() {
                        self.project_settings_dialog =
                            Some(crate::project_settings::state_from_project(project));
                    } else {
                        self.status_message =
                            Some("Open a project before editing its settings.".to_owned());
                    }
                    self.window.request_redraw();
                }
                ShellAction::SaveProjectSettings => {
                    // Validate + apply onto a clone first, so a rejected save
                    // leaves the live manifest untouched.
                    let Some(state) = self.project_settings_dialog.clone() else {
                        continue;
                    };
                    let Some(project) = self.active_project.as_mut() else {
                        self.project_settings_dialog = None;
                        continue;
                    };
                    let mut manifest = project.manifest().clone();
                    match crate::project_settings::apply_state_to_manifest(&state, &mut manifest) {
                        Ok(()) => {
                            *project.manifest_mut() = manifest;
                            #[cfg(feature = "usd-resolver-http")]
                            crate::remote::configure_project_cache(&project.name());
                            #[cfg(feature = "usd-resolver-http")]
                            crate::remote::configure_asset_network(&project.manifest().network);
                            self.persist_project_manifest();
                            self.project_settings_dialog = None;
                            self.status_message = Some("Project settings saved.".to_owned());
                        }
                        Err(errors) => {
                            if let Some(dlg) = self.project_settings_dialog.as_mut() {
                                dlg.validation_errors = errors;
                            }
                        }
                    }
                    self.window.request_redraw();
                }
                ShellAction::CloseProjectSettings => {
                    self.project_settings_dialog = None;
                    self.window.request_redraw();
                }
                ShellAction::PickProjectSettingsPath { field } => {
                    use idtx_editor_ui::project_settings::PathField;
                    let picked = match field {
                        PathField::ProvisioningProfile => rfd::FileDialog::new()
                            .add_filter("Provisioning profile", &["mobileprovision"])
                            .pick_file(),
                        PathField::Jdk | PathField::Sdk | PathField::Ndk => {
                            rfd::FileDialog::new().pick_folder()
                        }
                    };
                    if let (Some(path), Some(dlg)) =
                        (picked, self.project_settings_dialog.as_mut())
                    {
                        let value = path.to_string_lossy().replace('\\', "/");
                        match field {
                            PathField::Jdk => dlg.android_jdk = value,
                            PathField::Sdk => dlg.android_sdk = value,
                            PathField::Ndk => dlg.android_ndk = value,
                            PathField::ProvisioningProfile => {
                                dlg.visionos_provisioning_profile = value;
                            }
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "remote")]
                ShellAction::SaveServerConfigs { servers } => {
                    // Validate every origin first; a bad URL aborts the whole
                    // save so the on-disk table never holds an unusable entry.
                    let mut cfgs = Vec::with_capacity(servers.len());
                    let mut passwords = Vec::with_capacity(servers.len());
                    let mut bad = None;
                    for row in &servers {
                        if let Err(error) = idtx_auth::Origin::from_url(&row.url) {
                            bad = Some(format!(
                                "Invalid server origin for \"{}\": {error}",
                                row.description
                            ));
                            break;
                        }
                        cfgs.push(idtx_remote::ServerConfig {
                            description: row.description.clone(),
                            url: row.url.clone(),
                            username: row.username.clone(),
                            password_hint: String::new(), // filled by save_configs
                        });
                        passwords.push(row.password.clone());
                    }
                    if let Some(msg) = bad {
                        self.status_message = Some(msg);
                        if let Some(dlg) = self.configure_dialog.as_mut()
                            && let Some(r) = dlg.rows.iter_mut().find(|r| {
                                idtx_auth::Origin::from_url(&r.url).is_err()
                            })
                        {
                            r.status = Some("Invalid server URL".to_owned());
                            r.status_ok = false;
                        }
                        continue;
                    }
                    let password_refs: Vec<&str> =
                        passwords.iter().map(String::as_str).collect();
                    match idtx_remote::save_configs(&cfgs, &password_refs) {
                        Ok(()) => {
                            // Drop every previously-armed origin + cached token;
                            // a reconfigured server must not keep a stale token.
                            for old in &self.servers {
                                let _ = idtx_auth::clear_origin(&old.url);
                            }
                            self.remote_generation = self.remote_generation.wrapping_add(1);
                            self.bearer_tokens.clear();
                            // Reload the canonical form (hints now resolve to the
                            // keychain).
                            self.servers = idtx_remote::load_configs().unwrap_or_default();
                            self.active_server = (!self.servers.is_empty()).then_some(0);
                            // Best-effort: log in + arm each server up front so a
                            // local level referencing remote payloads (or a later
                            // Open Remote…) authenticates without a further step.
                            let creds: Vec<(String, String, String)> = cfgs
                                .iter()
                                .zip(&passwords)
                                .map(|(c, p)| {
                                    (c.url.clone(), c.username.clone(), p.clone())
                                })
                                .collect();
                            self.start_remote_job(move || {
                                let mut tokens = Vec::new();
                                let mut first_err = None;
                                for (url, username, password) in creds {
                                    let outcome = idtx_auth::Origin::from_url(&url)
                                        .map_err(|e| e.to_string())
                                        .and_then(|origin| {
                                            idtx_remote::login(&url, &username, &password)
                                                .map_err(|e| e.to_string())
                                                .map(|t| (origin.to_string(), t))
                                        });
                                    match outcome {
                                        Ok(pair) => tokens.push(pair),
                                        Err(e) => {
                                            first_err.get_or_insert(e);
                                        }
                                    }
                                }
                                Box::new(move |app| {
                                    for (origin, token) in tokens {
                                        app.bearer_tokens.insert(origin, token);
                                    }
                                    if let Err(error) = app.arm_server_token() {
                                        app.status_message = Some(error);
                                        return;
                                    }
                                    app.status_message = Some(match first_err {
                                        Some(e) => {
                                            format!("Saved config; some logins failed: {e}")
                                        }
                                        None => "Servers configured and connected.".to_owned(),
                                    });
                                })
                            });
                        }
                        Err(e) => {
                            self.status_message = Some(format!("Failed to save config: {e}"));
                        }
                    }
                    self.window.request_redraw();
                }
                #[cfg(feature = "remote")]
                ShellAction::TestServerConnection {
                    url,
                    username,
                    password,
                } => {
                    self.start_remote_job(move || {
                    let result = idtx_remote::health_check(&url)
                        .and_then(|_| idtx_remote::login(&url, &username, &password).map(|_| ()));
                    let (ok, msg) = match result {
                        Ok(()) => (true, "Connected ✓".to_owned()),
                        Err(e) => (false, format!("Error: {e}")),
                    };
                    // Route the status back by row identity (url + username), not
                    // a positional index: the table may have been edited while
                    // the test was in flight.
                    Box::new(move |app| { if let Some(dlg) = app.configure_dialog.as_mut()
                        && let Some(row) = dlg.rows.iter_mut().find(|r| r.url == url && r.username == username) {
                        row.status = Some(msg);
                        row.status_ok = ok;
                    } })
                    });
                    self.window.request_redraw();
                }
                #[cfg(feature = "remote")]
                ShellAction::OpenRemoteDialog { server } => {
                    self.active_server = Some(server);
                    #[cfg(all(feature = "remote", feature = "authoring"))]
                    {
                        self.remote_browser_sync_mode = false;
                        // A plain Open-Remote supersedes any pending asset-picker
                        // context, so a later file pick opens a stage (not a ref).
                        self.asset_create_target = None;
                    }
                    if !self.ensure_remote_browser_listing() {
                        continue;
                    }
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::OpenAssetPickerDialog => {
                    // The "Create Child → Asset…" picker: same listing as
                    // OpenRemoteDialog but preserves `asset_create_target` (set by
                    // the scene-tree action) so a picked file becomes a reference.
                    self.remote_browser_sync_mode = false;
                    if !self.ensure_remote_browser_listing() {
                        continue;
                    }
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::OpenRemoteWithSyncDialog { server, mode } => {
                    self.active_server = Some(server);
                    self.remote_browser_sync_mode = true;
                    self.remote_session_mode = mode;
                    // A plain Open-Remote-with-Sync supersedes any pending
                    // asset-picker context.
                    self.asset_create_target = None;
                    if !self.ensure_remote_browser_listing() {
                        continue;
                    }
                }
                #[cfg(feature = "remote")]
                ShellAction::OpenRemoteStage(url) => {
                    // Arm the HTTP resolver token, then open via the same
                    // path as local files.  For now treat as URL-typed path.
                    if let Err(error) = self.arm_server_token() {
                        self.status_message = Some(error);
                        continue;
                    }
                    // Derive a display label from the URL.
                    let label = url.rsplit('/').next().unwrap_or("remote.usd").to_owned();
                    self.status_message = Some(format!("Opening {label}…"));
                    self.open_path(std::path::Path::new(&url), Some(url.clone()), None, false);
                    self.window.request_redraw();
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::OpenRemoteSyncStage {
                    base_url,
                    filepath,
                    mode,
                } => {
                    // 1. Arm the HTTP resolver token for the download.
                    if let Err(error) = self.arm_server_token() {
                        self.status_message = Some(error);
                        continue;
                    }
                    // 2. Create the collab session on the server.
                    let token = match self.active_server_token() {
                        Some(t) => t,
                        None => {
                            self.status_message = Some("Not logged in.".to_owned());
                            self.window.request_redraw();
                            continue;
                        }
                    };
                    self.start_remote_job(move || {
                    let result = idtx_collab::session::create_session(
                        &base_url, &token, &filepath, mode,
                    );
                    Box::new(move |app| {
                    let session_info = match result {
                        Ok(info) => info,
                        Err(e) => {
                            app.status_message =
                                Some(format!("Failed to create collab session: {e}"));
                            app.window.request_redraw();
                            return;
                        }
                    };
                    // 3. Begin loading the stage.
                    let download_url = remote_download_url(&base_url, &filepath);
                    let label = filepath
                        .rsplit('/')
                        .next()
                        .unwrap_or("remote.usd")
                        .to_owned();
                    app.status_message = Some(format!("Opening {label} (sync)…"));
                    app.open_path(
                        std::path::Path::new(&download_url),
                        Some(download_url.clone()),
                        None,
                        true, // for remote stages always force fresh download of the file to match current server state
                    );
                    // 4. Attach collab metadata so poll_loading_tabs can connect once done.
                    if let Some(lt) = app.loading_tabs.last_mut() {
                        lt.pending_collab = Some(PendingCollabInfo {
                            session_id: session_info.session_id,
                            ws_url_path: session_info.ws_url,
                            base_url,
                            token,
                            owns_session: true,
                        });
                    }
                    app.window.request_redraw();
                    })
                    });
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::OpenSessionBrowserDialog { server } => {
                    self.active_server = Some(server);
                    if !self.ensure_session_browser_listing() {
                        continue;
                    }
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::JoinRemoteSession {
                    base_url,
                    session_id,
                    usd_file,
                    ws_url_path,
                } => {
                    // 1. Arm the HTTP resolver token for the stage download.
                    if let Err(error) = self.arm_server_token() {
                        self.status_message = Some(error);
                        continue;
                    }
                    let token = match self.active_server_token() {
                        Some(t) => t,
                        None => {
                            self.status_message = Some("Not logged in.".to_owned());
                            self.window.request_redraw();
                            continue;
                        }
                    };
                    // 2. Derive the stage download URL and begin loading.
                    let download_url = remote_download_url(&base_url, &usd_file);
                    let label = usd_file
                        .rsplit('/')
                        .next()
                        .unwrap_or("remote.usd")
                        .to_owned();
                    self.status_message = Some(format!("Joining session for {label}…"));
                    self.open_path(
                        std::path::Path::new(&download_url),
                        Some(download_url.clone()),
                        None,
                        true, // for remote stages always force fresh download of the file to match current server state
                    );
                    // 3. Attach collab metadata; mark as NOT owning the session
                    //    so the server-side session is not deleted when this tab
                    //    is closed or the WebSocket is dropped.
                    if let Some(lt) = self.loading_tabs.last_mut() {
                        lt.pending_collab = Some(PendingCollabInfo {
                            session_id,
                            ws_url_path,
                            base_url,
                            token,
                            owns_session: false,
                        });
                    }
                    self.window.request_redraw();
                }
                ShellAction::AnimPlayback { tab_id, req } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        t.session.apply_playback_request(req);
                        self.window.request_redraw();
                    }
                }
                #[cfg(feature = "remote")]
                ShellAction::ReconnectCollab { tab_id } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        match t.session.reconnect_collab() {
                            Ok(()) => {
                                self.status_message =
                                    Some("Reconnecting collaboration session…".to_owned());
                                t.collab_was_connected = true;
                            }
                            Err(e) => {
                                self.status_message = Some(format!("Collab reconnect failed: {e}"));
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::ExportCollabRecovery { tab_id } => {
                    if let Some(tab) = self.tabs.iter().find(|tab| tab.session.id().get() == tab_id) {
                        match tab.session.collab_recovery_report() {
                            Ok(report) => { self.start_remote_job(move || {
                                let result = rfd::FileDialog::new().set_file_name("collaboration-recovery.txt").save_file()
                                    .map(|path| std::fs::write(path, report).map_err(|error| error.to_string()));
                                Box::new(move |app| { app.status_message = Some(match result {
                                    Some(Ok(())) => "Collaboration recovery exported. Retained intent still requires reconciliation before commit.".into(),
                                    Some(Err(error)) => format!("Recovery export failed: {error}"),
                                    None => "Recovery export cancelled.".into(),
                                }); })
                            }); }
                            Err(error) => self.status_message = Some(error.to_string()),
                        }
                    }
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::CommitRemote { tab_id } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.session.id().get() == tab_id)
                    {
                        match t.session.commit_collab() {
                            Ok(()) => {
                                self.status_message =
                                    Some("Waiting for collaboration acknowledgements and commit…".to_owned());
                            }
                            Err(e) => {
                                self.status_message = Some(format!("Commit failed: {e}"));
                            }
                        }
                        self.window.request_redraw();
                    }
                }
                #[cfg(all(feature = "remote", feature = "authoring"))]
                ShellAction::CreateRemoteAssetChild {
                    tab_id,
                    parent_path,
                    reference_path,
                } => {
                    // Server-authoritative, like the built-in remote create: send
                    // the request and let the server's `CreatePrimBroadcast`
                    // author the typeless payload prim (which this client also
                    // receives). Nothing is authored or journaled locally.
                    if let Some(t) = self.tabs.iter().find(|t| t.session.id().get() == tab_id) {
                        if t.session.request_remote_asset_prim(&parent_path, &reference_path) {
                            self.status_message = Some(format!(
                                "Requested asset reference ({reference_path}) from the collaboration server…"
                            ));
                        } else {
                            self.status_message = Some(
                                "Asset reference cannot be created in this session".to_owned(),
                            );
                        }
                        self.window.request_redraw();
                    }
                }
                ShellAction::OpenLevel { index } => self.open_level(index),
                ShellAction::RemoveLevel { index } => self.remove_level(index),
                ShellAction::MoveLevel { from, to } => self.move_level(from, to),
                #[cfg(feature = "authoring")]
                ShellAction::NewLevel => self.new_level_dialog(),
                ShellAction::AddExistingLevel => self.add_existing_level_dialog(),
                #[cfg(feature = "authoring")]
                ShellAction::CreatePlayerStage => self.create_player_stage(),
                ShellAction::SetAssetDir(subdir) => self.set_asset_dir(subdir),
                ShellAction::OpenAssetStage { asset_rel_path } => {
                    self.open_asset_stage(&asset_rel_path);
                }
                #[cfg(feature = "authoring")]
                ShellAction::AddReferenceToSelected { asset_rel_path } => {
                    self.author_asset_arc(asset_rel_path, false);
                }
                #[cfg(feature = "authoring")]
                ShellAction::AddPayloadToSelected { asset_rel_path } => {
                    self.author_asset_arc(asset_rel_path, true);
                }
            }
        }
        Ok(())
    }

    /// Kick off a background cook + package for the active project.
    ///
    /// Runs `idtx-build`'s cook pipeline (flatten + localize the entry stage,
    /// collect + validate behaviors, stamp `run.toml`) and, when a sibling
    /// `idtx-runtime` binary is present, the per-target packager — all on a
    /// worker thread so the UI stays responsive. The job is polled once per frame
    /// (see `render`); the outcome lands in the status bar. Refuses to start a
    /// second build while one is in flight, or when no project is open.
    #[cfg(feature = "build-tools")]
    fn start_build(&mut self, target: idtx_editor_model::BuildTarget, deploy: bool) {
        if self.build_job.is_some() {
            self.status_message = Some("A build is already running.".to_owned());
            return;
        }
        let Some(project) = self.active_project.clone() else {
            self.status_message = Some("Open a project before building.".to_owned());
            return;
        };
        let verb = if deploy { "Building + deploying" } else { "Building" };
        self.status_message = Some(format!("{verb} {} experience…", target.label()));
        self.build_job = Some(crate::build_job::spawn(project, target, deploy));
        self.window.request_redraw();
    }

    ///
    /// Resolves the stage path (flattening a temp beside the source when the
    /// tab has unsaved authoring edits so relative references still resolve),
    /// spawns `idtx-runtime --stage <path> --mode <mode>`, and tracks the child
    /// so the temp is deleted after the preview process exits. Surfaces a status
    /// message on any failure.
    ///
    /// `mode` selects the presentation: [`PreviewMode::Xr`] mounts the XR
    /// controller-defaults layer over the on-the-fly assembly and runs an
    /// `OpenXR` session (falling back to desktop in the runtime when no headset is
    /// present); [`PreviewMode::Desktop`] previews the same assembled
    /// stage in a plain window with the orbit camera and no XR — the XR
    /// controller probe/layer are skipped entirely.
    #[cfg(feature = "cooking")]
    fn launch_runtime_preview(&mut self, mode: preview_assembly::PreviewMode) {
        if self
            .preview_process
            .as_ref()
            .is_some_and(PreviewProcess::is_running)
        {
            self.status_message = Some("A runtime preview is already running.".to_owned());
            return;
        }
        let Some(idx) = self.active_tab_index() else {
            self.status_message = Some("Open a USD stage before launching a preview.".to_owned());
            return;
        };

        // Dirty when the tab's authoring journal has any history (edits were
        // applied this session); then flatten a temp beside the source so
        // relative references/textures resolve.
        #[cfg(feature = "authoring")]
        let dirty = {
            let j = &self.tabs[idx].journal;
            j.undo_depth() > 0 || j.redo_depth() > 0
        };
        #[cfg(not(feature = "authoring"))]
        let dirty = false;

        // Discover the runtime manifest (env → exe-adjacent run.toml → default);
        // it locates the XR controller mesh assets mounted over the world in XR
        // mode. In desktop mode no controller layer is mounted, so no probe is
        // needed and the "generic" fallback is passed straight through.
        let manifest = idtx_runtime_manifest::RuntimeManifest::discover();

        // Probe the connected XR device once to select the right controller
        // meshes for the assembly layer — only in XR mode. No-op (→ "generic"
        // fallback) when pcvr is not compiled in, no HMD is reachable, or the
        // preview is desktop.
        #[cfg(feature = "pcvr")]
        let controller_device: Option<String> = if mode == preview_assembly::PreviewMode::Xr {
            use idtx_openxr_sys::XrLoader;
            match XrLoader::try_load().and_then(|l| l.detect_controller_kind()) {
                Ok(kind) => {
                    eprintln!(
                        "[preview] detected controller kind: {:?} (manifest device = \"{}\")",
                        kind,
                        kind.manifest_device_id()
                    );
                    Some(kind.manifest_device_id().to_owned())
                }
                Err(e) => {
                    eprintln!(
                        "[preview] controller kind detection failed ({e}); using generic fallback"
                    );
                    None
                }
            }
        } else {
            None
        };
        #[cfg(not(feature = "pcvr"))]
        let controller_device: Option<String> = None;

        // Behavior compile context: the open project's root (where `behaviors/`
        // lives) and the staged guest-ABI SDK. Only present in authoring builds
        // with a project open; the preview compiles any authored
        // `idtx:behavior:source` into a linked overlay before launch.
        #[cfg(feature = "authoring")]
        let behavior_ctx =
            self.active_project
                .as_ref()
                .map(|project| preview_assembly::BehaviorPrepareCtx {
                    project_root: project.root().to_path_buf(),
                    sdk_path: crate::behavior_tools::discover_sdk_path(),
                });
        #[cfg(not(feature = "authoring"))]
        let behavior_ctx: Option<preview_assembly::BehaviorPrepareCtx> = None;

        // Project-preview composition: compose the project's level(s) plus
        // the configured player, composed as a reference at `/World/Player`. One
        // player stage carries both desktop + XR rigs via a `playerKind`
        // variantSet; the preview selects the variant from the target mode. `None`
        // when no project is open → the legacy open-tab composition applies
        // (loose-file preview).
        let player_comp = self.active_project.as_ref().map(|project| {
            let player = match project.player_stage() {
                idtx_project::PlayerStage::Project(abs) => crate::assembly::PlayerRef::Project(abs),
                idtx_project::PlayerStage::BuiltinDefault => crate::assembly::PlayerRef::Builtin(
                    idtx_assets::BUILTIN_PLAYER_STAGE_REF.to_owned(),
                ),
            };
            preview_assembly::PlayerComposition {
                player: Some(player),
                levels: project.levels_abs(),
            }
        });

        // The open project's asset roots (absolute), so the spawned runtime
        // installs the same USD default-resolver search path the editor uses and
        // a level's project-relative references (`@assets/foo.usdz@`) resolve
        // out-of-process. Empty for a loose-file preview with no project. The
        // built-in asset root is appended so `@built-in/...@` references (the
        // default player) resolve out-of-process too.
        let mut asset_search_dirs = self
            .active_project
            .as_ref()
            .map(|project| project.asset_search_dirs_abs())
            .unwrap_or_default();
        asset_search_dirs.push(idtx_assets::builtin_root());

        // Convert the project's network config (idtx_project type) into the
        // runtime-manifest type so spawn_preview can forward it as CLI flags.
        let preview_network: Option<idtx_runtime_manifest::NetworkConfig> =
            self.active_project.as_ref().and_then(|p| {
                let n = &p.manifest().network;
                if n.is_empty() {
                    None
                } else {
                    Some(idtx_runtime_manifest::NetworkConfig {
                        allow: n.allow,
                        allowed_hosts: n.allowed_hosts.clone(),
                        private_hosts: n.private_hosts.clone(),
                        auth_origins: n.auth_origins.clone(),
                        max_response_mb: n.max_response_mb,
                        max_asset_mb: n.max_asset_mb,
                        ..Default::default()
                    })
                }
            });

        match runtime_launch::spawn_preview(
            &self.tabs[idx].session,
            dirty,
            mode,
            &manifest,
            controller_device.as_deref(),
            player_comp.as_ref(),
            behavior_ctx.as_ref(),
            &asset_search_dirs,
            preview_network.as_ref(),
        ) {
            Ok(proc) => {
                self.status_message = Some(format!(
                    "Runtime preview launched ({} mode).",
                    mode.as_arg()
                ));
                self.preview_process = Some(proc);
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to launch runtime preview: {e}"));
            }
        }
        self.window.request_redraw();
    }
}
