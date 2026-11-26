//! Document/tab lifecycle: opening, background loading, reload/redownload,
//! closing, active-tab lookup, and the authoring save/undo family.
//!
//! Methods on [`ApplicationState`] that manage the set of open [`SessionTab`]s.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use idtx_editor_model::ProductKind;
use idtx_editor_ui::layer_stack::LayerStackUiState;
use idtx_editor_ui::material_canvas::MaterialCanvasState;
use idtx_editor_ui::scene_tree::{SceneTreeIndex, SceneTreeState};
use idtx_editor_ui::viewport_toolbar::{GizmoSpace, RenderMode};
use idtx_project::{IdtxProject, ProjectManifest};
use idtx_studio_session::{StudioSession, TextureDecodeConfig};
use num_traits::ToPrimitive as _;

#[cfg(feature = "authoring")]
use idtx_authoring::EditJournal;

use crate::app::ApplicationState;
#[cfg(feature = "authoring")]
use crate::authoring_ops::{apply_edit_command_forward, apply_edit_command_reverse};
use crate::gizmo::GizmoState;
#[cfg(feature = "authoring")]
use crate::model::SaveConfirmInfo;
use crate::model::{
    CenterTab, InspectorCache, InspectorTab, LayerStackCache, LoadingTab, SessionTab,
    StageBehaviorCache,
};

/// Percent-decode the path portion of a server download URI (`%XX` → byte),
/// leaving everything else intact, so a canonical `usd_uri` the server sends
/// matches the raw layer identifier USD opened the layer with. Malformed
/// escapes are left verbatim. Only used by [`ApplicationState::apply_collab_layer_reload`].
#[cfg(all(feature = "remote", feature = "authoring"))]
fn percent_decode_path(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let hex = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl ApplicationState {
    /// Begin loading a stage in the background.
    ///
    /// `remote_url` is `Some` when `path` is a remote HTTP(S) URL (the URL is
    /// also used as the pseudo-path); the loaded session is then tagged with a
    /// `Remote` [`SessionSource`]. Pass `None` for a local-file open.
    pub(crate) fn open_path(
        &mut self,
        path: &Path,
        remote_url: Option<String>,
        insert_at: Option<usize>,
        force_refresh: bool,
    ) {
        let scale = self.window.scale_factor().to_f32().unwrap_or(1.0);
        let size = self.window.inner_size();
        let vp_w = ((size.width as f32 * 0.6 * scale).round() as u32).max(1);
        let vp_h = (size.height.saturating_sub(120)).max(1);
        let vp = [vp_w, vp_h];

        let path_buf = path.to_path_buf();
        let device = self.gpu.device.clone();
        let queue = self.gpu.queue.clone();
        let texture_config = match self.profile.kind() {
            ProductKind::Inspector => TextureDecodeConfig::INSPECTOR,
            ProductKind::Poser => TextureDecodeConfig::AUTHORING,
            ProductKind::Conductor => TextureDecodeConfig::FULL_RESOLUTION,
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let path_label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("…")
            .to_owned();

        let world_unit_scale = self.world_unit_scale;
        // Product-level shadow capability (off for Inspector/Poser). Captured
        // before the loader thread so the session can be clamped on open.
        let shadows_enabled = self.profile.shadows_enabled();
        std::thread::Builder::new()
            .name("idtx-open".to_owned())
            .spawn(move || {
                // Force-refresh is armed for the whole open so the root layer,
                // its sublayers/references, and textures are all re-fetched
                // (overwriting the cache) rather than served stale. Disarmed
                // unconditionally afterwards, including on error, so it never
                // leaks into subsequent normal opens.
                #[cfg(feature = "usd-resolver-http")]
                if force_refresh {
                    idtx_usd_resolver_http::set_force_refresh(true);
                }
                let result = StudioSession::open_with_gpu(
                    &path_buf,
                    device,
                    queue,
                    vp,
                    texture_config,
                    world_unit_scale,
                )
                .map(|mut s| {
                    s.set_shadow_capability(shadows_enabled);
                    s.set_viewport_purpose(idtx_usd::RenderPurpose::Default);
                    #[cfg(feature = "metrics")]
                    let t_render = std::time::Instant::now();
                    s.render_one_frame(false);
                    #[cfg(feature = "metrics")]
                    s.finalize_open_metrics(t_render.elapsed());
                    s
                });
                #[cfg(feature = "usd-resolver-http")]
                if force_refresh {
                    idtx_usd_resolver_http::set_force_refresh(false);
                }
                #[cfg(not(feature = "usd-resolver-http"))]
                let _ = force_refresh;
                tx.send(result).ok();
            })
            .expect("failed to spawn loader thread");

        // Only local opens seed the file-dialog start directory; a remote URL's
        // "parent" is a meaningless partial URL.
        if remote_url.is_none() {
            self.last_open_dir = path.parent().map(Path::to_path_buf);
            crate::recent::push_recent(&mut self.recent_files, path);
            crate::recent::save_recent(&self.recent_files);
        }
        self.status_message = Some(format!("Opening {}…", path_label));
        self.loading_tabs.push(LoadingTab {
            vp,
            path_label,
            source_path: Some(path.to_path_buf()),
            rx,
            remote_url,
            insert_at,
            #[cfg(all(feature = "remote", feature = "authoring"))]
            pending_collab: None,
        });
        self.window.request_redraw();
    }

    pub(crate) fn poll_loading_tabs(&mut self) {
        let mut i = 0;
        while i < self.loading_tabs.len() {
            match self.loading_tabs[i].rx.try_recv() {
                Ok(Ok(session)) => {
                    let lt = self.loading_tabs.swap_remove(i);
                    let mut session = session;
                    session.set_viewport_purpose(idtx_usd::RenderPurpose::Default);
                    // Tag remote-loaded sessions so Poser offers only "Save As…".
                    if let Some(url) = lt.remote_url {
                        session.set_source(idtx_studio_session::SessionSource::Remote { url });
                    }
                    // Attach live collab client if this open was initiated by
                    // "Open Remote with Sync…".
                    #[cfg(all(feature = "remote", feature = "authoring"))]
                    if let Some(info) = lt.pending_collab {
                        let ws_url = idtx_collab::session::ws_url_from_http(
                            &info.base_url,
                            &info.ws_url_path,
                        );
                        let connect_result = idtx_collab::client::CollabClient::connect_shared(
                            &ws_url, &info.session_id, info.owns_session,
                        );
                        match connect_result {
                            Ok(client) => session.attach_collab(client),
                            Err(e) => {
                                self.status_message = Some(format!("Collab connect failed: {e}"));
                            }
                        }
                    }
                    let texture_id = self.egui_renderer.register_native_texture(
                        &self.gpu.device,
                        // Non-sRGB view: egui samples the stored gamma bytes verbatim,
                        // matching the runtime player's brightness (see display_view_unorm).
                        session.display_view_unorm(),
                        wgpu::FilterMode::Linear,
                    );
                    let id = session.id().get();
                    let tab = SessionTab {
                        last_viewport_pixels: lt.vp,
                        session,
                        texture_id,
                        center_tab: CenterTab::Viewport,
                        render_mode: RenderMode::default(),
                        gizmo_space: GizmoSpace::default(),
                        scene_tree_state: SceneTreeState::new(),
                        tree_initialized: false,
                        cached_nodes: Arc::new(Vec::new()),
                        cached_index: Arc::new(SceneTreeIndex::default()),
                        cached_topology_gen: u64::MAX,
                        cached_visibility_gen: u64::MAX,
                        inspector_cache: InspectorCache::default(),
                        stage_behavior_cache: StageBehaviorCache::default(),
                        inspector_tab: InspectorTab::default(),
                        layer_stack_cache: LayerStackCache::default(),
                        layer_stack_ui: LayerStackUiState::default(),
                        material_canvas: MaterialCanvasState::default(),
                        canvas_graph_source: None,
                        canvas_source_key: None,
                        compute_canvas: Default::default(),
                        compute_graph_source: None,
                        compute_source_key: None,
                        gizmo: GizmoState::default(),
                        #[cfg(feature = "authoring")]
                        journal: EditJournal::new(),
                        #[cfg(feature = "remote")]
                        collab_was_connected: false,
                    };
                    // A reload reinserts at the original slot so the tab keeps
                    // its position; a fresh open appends. The index is clamped
                    // in case tabs were closed while the reload was in flight.
                    match lt.insert_at {
                        Some(at) => self.tabs.insert(at.min(self.tabs.len()), tab),
                        None => self.tabs.push(tab),
                    }
                    self.active_tab_id = Some(id);
                    // Clear a pending "Opening…" status if this load caused it.
                    if self
                        .status_message
                        .as_deref()
                        .map(|s| s.starts_with("Opening"))
                        .unwrap_or(false)
                    {
                        self.status_message = None;
                    }
                    if let Some(idx) = self.active_tab_index() {
                        self.tabs[idx].session.initialize_viewport_camera();
                    }
                    self.window.request_redraw();
                }
                Ok(Err(e)) => {
                    self.loading_tabs.swap_remove(i);
                    self.status_message = Some(e.to_string());
                    self.window.request_redraw();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    i += 1;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let lt = self.loading_tabs.swap_remove(i);
                    self.status_message = Some(format!(
                        "Failed to open {} (loader thread error)",
                        lt.path_label
                    ));
                    self.window.request_redraw();
                }
            }
        }
    }

    pub(crate) fn open_dialog(&mut self) {
        let mut dialog =
            rfd::FileDialog::new().add_filter("OpenUSD stage", &["usd", "usda", "usdc", "usdz"]);
        if let Some(dir) = &self.last_open_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_path(&path, None, None, false);
        }
    }

    /// Conductor File → Import: pick a USD file and bring it into the active
    /// project's `assets/` folder before opening it (U-import).
    ///
    /// Three cases (see [`crate::import::classify_import`]): a file already under
    /// `assets/` opens in place; an external `.usdz` is copied in; an external
    /// loose stage is packaged (with all referenced assets) into a `.usdz` on a
    /// worker thread behind the import progress modal.
    #[cfg(feature = "authoring")]
    pub(crate) fn import_dialog(&mut self) {
        let Some(project) = self.active_project.as_ref() else {
            self.status_message = Some("Open a project before importing.".to_owned());
            return;
        };
        if self.import_job.is_some() {
            self.status_message = Some("An import is already in progress.".to_owned());
            return;
        }
        let root = project.project_root();
        if let Err(err) = root.create_dir_all(Path::new("assets")) {
            self.status_message = Some(format!("Couldn't create the assets folder: {err}"));
            return;
        }

        let mut dialog =
            rfd::FileDialog::new().add_filter("OpenUSD stage", &["usd", "usda", "usdc", "usdz"]);
        if let Some(dir) = &self.last_open_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(picked) = dialog.pick_file() else {
            return;
        };

        // Revalidate after the modal: a project ancestor may have changed while
        // the user selected a source. Actual writes also use the root capability.
        let (picked_canon, assets_canon) = match std::fs::canonicalize(&picked)
            .and_then(|picked| root.resolve_relative_checked(Path::new("assets")).map(|assets| (picked, assets))) {
            Ok(paths) => paths,
            Err(error) => {
                self.status_message = Some(format!("Couldn't resolve import paths: {error}"));
                return;
            }
        };

        match crate::import::classify_import(&picked_canon, &assets_canon) {
            crate::import::ImportPlan::OpenInPlace => {
                self.open_path(&picked, None, None, false);
            }
            crate::import::ImportPlan::CopyUsdz => {
                self.import_copy_usdz(&picked, &root);
            }
            crate::import::ImportPlan::PackageLoose => {
                self.spawn_import_packaging(&picked, &root);
            }
        }
    }

    /// Copy an external `.usdz` into `assets/` (non-clobbering name) and open it.
    #[cfg(feature = "authoring")]
    fn import_copy_usdz(&mut self, source: &Path, root: &idtx_project::ProjectRoot) {
        match crate::import::copy_into_project(root, source) {
            Ok(dest) => {
                if let Some(name) = dest.file_name().and_then(|n| n.to_str()) {
                    self.status_message = Some(format!("Imported {name}"));
                }
                self.open_path(&dest, None, None, false);
            }
            Err(err) => {
                self.status_message = Some(format!("Couldn't copy into the project: {err}"));
                self.window.request_redraw();
            }
        }
    }

    /// Spawn the background packaging of an external loose stage into a `.usdz`
    /// in private scratch storage. Acceptance copies through the captured project
    /// file capability; [`Self::poll_import_job`] drives it to completion.
    #[cfg(feature = "authoring")]
    fn spawn_import_packaging(&mut self, source: &Path, root: &idtx_project::ProjectRoot) {
        let prepared = crate::import::prepare_import(root, source)
            .and_then(|target| crate::import::import_scratch().map(|scratch| (target, scratch)));
        let (target, scratch) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.status_message = Some(format!("Couldn't prepare import: {error}"));
                self.window.request_redraw();
                return;
            }
        };
        let temp_dest = scratch.path().join("stage.usdz");
        let source_label = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("stage")
            .to_owned();

        let (tx, rx) = std::sync::mpsc::channel();
        let source_owned = source.to_path_buf();
        let temp_for_worker = temp_dest.clone();
        let scratch_for_worker = scratch.clone();
        std::thread::Builder::new()
            .name("idtx-import-pkg".to_owned())
            .spawn(move || {
                let _scratch = scratch_for_worker;
                #[cfg(feature = "metrics")]
                let started = std::time::Instant::now();
                let result = idtx_usd::package_stage(
                    &source_owned,
                    &temp_for_worker,
                    idtx_usd::PackagingPurpose::PreserveAuthoring,
                )
                .map_err(|e| e.to_string());
                #[cfg(feature = "metrics")]
                eprintln!(
                    "[import-metrics] package_stage took {:?}",
                    started.elapsed()
                );
                // The receiver may be gone if the app is shutting down; ignore.
                let _ = tx.send(result);
            })
            .expect("failed to spawn import packaging thread");

        self.status_message = Some(format!("Packaging {source_label}…"));
        self.import_job = Some(crate::import::ImportJob {
            rx,
            source: source.to_path_buf(),
            scratch,
            temp_dest,
            target,
            source_label,
            phase: crate::import::ImportPhase::Packaging,
            abandoned: false,
        });
        self.window.request_redraw();
    }

    /// Re-spawn packaging for the "Import anyway" decision: clear the
    /// unresolved references from in-memory copies of the stage's dependency
    /// layers, then package. The source files on disk are never modified
    /// (`OpenUSD` packages the in-memory copies). Reuses the current job's
    /// scratch/destination capability so retries have the same confined boundary.
    #[cfg(feature = "authoring")]
    pub(crate) fn spawn_import_strip_packaging(&mut self) {
        let Some(job) = self.import_job.as_ref() else {
            return;
        };
        let source = job.source.clone();
        let temp_dest = job.temp_dest.clone();
        let target = job.target.clone();
        let scratch = job.scratch.clone();
        let source_label = job.source_label.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        let source_owned = source.clone();
        let temp_for_worker = temp_dest.clone();
        let scratch_for_worker = scratch.clone();
        std::thread::Builder::new()
            .name("idtx-import-pkg".to_owned())
            .spawn(move || {
                let _scratch = scratch_for_worker;
                #[cfg(feature = "metrics")]
                let started = std::time::Instant::now();
                let result = idtx_usd::package_stage_dropping_unresolved(
                    &source_owned,
                    &temp_for_worker,
                    idtx_usd::PackagingPurpose::PreserveAuthoring,
                )
                .map_err(|e| e.to_string());
                #[cfg(feature = "metrics")]
                eprintln!(
                    "[import-metrics] package_stage_dropping_unresolved took {:?}",
                    started.elapsed()
                );
                // The receiver may be gone if the app is shutting down; ignore.
                let _ = tx.send(result);
            })
            .expect("failed to spawn import strip-packaging thread");

        self.status_message =
            Some(format!("Repackaging {source_label} without broken references…"));
        self.import_job = Some(crate::import::ImportJob {
            rx,
            source,
            scratch,
            temp_dest,
            target,
            source_label,
            phase: crate::import::ImportPhase::Packaging,
            abandoned: false,
        });
        self.window.request_redraw();
    }

    /// Poll the in-flight packaging import once per frame. Only acts while the
    /// job is still `Packaging`; the warnings/failed phases wait on the modal.
    #[cfg(feature = "authoring")]
    pub(crate) fn poll_import_job(&mut self) {
        use crate::import::ImportPhase;
        use std::sync::mpsc::TryRecvError;

        match self.import_job.as_ref() {
            Some(job) if matches!(job.phase, ImportPhase::Packaging) => {}
            _ => return,
        }
        let received = self
            .import_job
            .as_ref()
            .expect("checked above")
            .rx
            .try_recv();
        match received {
            Ok(Ok(result)) => {
                let abandoned = self.import_job.as_ref().expect("checked above").abandoned;
                if abandoned {
                    let temp = self.import_job.take().expect("checked above").temp_dest;
                    let _ = std::fs::remove_file(&temp);
                } else if result.destination.as_os_str().is_empty() {
                    // No package was written. If that is because references
                    // couldn't be resolved, offer the user a choice: Cancel
                    // (default) or "Import anyway", which re-packages with the
                    // broken references stripped out (spawn_import_strip_packaging).
                    let job = self.import_job.as_mut().expect("checked above");
                    // No temp file exists in this case, but clean up defensively.
                    let _ = std::fs::remove_file(&job.temp_dest);
                    if result.unresolved.is_empty() {
                        job.phase = ImportPhase::Failed(format!(
                            "Couldn't package {}: OpenUSD produced no package.",
                            job.source_label
                        ));
                    } else {
                        job.phase = ImportPhase::AwaitingUnresolved(result.unresolved);
                    }
                    self.window.request_redraw();
                } else if result.warnings.is_empty() {
                    // Clean package (or a strip that dropped refs but lost no
                    // whole assets) → move into place and open; modal closes.
                    self.finalize_import();
                } else {
                    // Packaging succeeded but with warnings to surface before the
                    // import completes: either OpenUSD diagnostics (clean path) or
                    // the "Package + warn on loss" list of assets that could not
                    // be packaged (strip path). Let the user review and accept.
                    self.import_job.as_mut().expect("checked above").phase =
                        ImportPhase::AwaitingWarnings(result.warnings);
                    self.window.request_redraw();
                }
            }
            Ok(Err(msg)) => {
                let job = self.import_job.as_mut().expect("checked above");
                let _ = std::fs::remove_file(&job.temp_dest);
                if job.abandoned {
                    self.import_job = None;
                } else {
                    job.phase = ImportPhase::Failed(msg);
                }
                self.window.request_redraw();
            }
            Err(TryRecvError::Empty) => {
                // Still packaging — keep repainting so the poll fires next frame.
                self.window.request_redraw();
            }
            Err(TryRecvError::Disconnected) => {
                let job = self.import_job.as_mut().expect("checked above");
                let _ = std::fs::remove_file(&job.temp_dest);
                if job.abandoned {
                    self.import_job = None;
                } else {
                    job.phase = ImportPhase::Failed(
                        "import packaging thread ended unexpectedly".to_owned(),
                    );
                }
                self.window.request_redraw();
            }
        }
    }

    /// Copy the completed package to its confined `assets/` name and open it —
    /// for both a clean package and an accepted-despite-warnings import.
    #[cfg(feature = "authoring")]
    pub(crate) fn finalize_import(&mut self) {
        let Some(job) = self.import_job.take() else {
            return;
        };
        if let Err(err) = job.target.copy_new_from(&job.temp_dest) {
            self.status_message = Some(format!("Couldn't finalize the import: {err}"));
            self.window.request_redraw();
            return;
        }
        if let Some(name) = job.target.path().file_name().and_then(|n| n.to_str()) {
            self.status_message = Some(format!("Imported {name}"));
        }
        self.open_path(job.target.path(), None, None, false);
    }

    /// File → New: pick a save target, create a blank stage with a default
    /// `/World` Xform root (up-axis Y, the active project's metres-per-unit when
    /// a project is open), then open it in a new tab.
    #[cfg(feature = "authoring")]
    pub(crate) fn new_stage_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("OpenUSD stage", &["usda", "usd", "usdc"])
            .set_file_name("untitled.usda");
        // Default into the project's stage directory when a project is open, else
        // the last-used open directory.
        if let Some(project) = &self.active_project {
            let stages = project.root().join("stages");
            dialog = dialog.set_directory(if stages.is_dir() {
                stages
            } else {
                project.root().to_path_buf()
            });
        } else if let Some(dir) = &self.last_open_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        let meters_per_unit = self
            .active_project
            .as_ref()
            .map_or(1.0, |p| p.manifest().unit_scale.meters_per_unit());
        // Create + save the blank stage to disk, then drop the handle; open_path
        // reopens it from disk through the normal background loader.
        match idtx_usd::StageService::create_new(&path, "Y", meters_per_unit) {
            Ok(_) => self.open_path(&path, None, None, false),
            Err(err) => {
                self.status_message = Some(format!("Couldn't create stage: {err}"));
                self.window.request_redraw();
            }
        }
    }

    /// Make `project` the active project: apply its unit scale to subsequently
    /// opened stages, open its default stage (when it resolves inside the
    /// project sandbox), and store the project.
    pub(crate) fn activate_project(&mut self, project: IdtxProject) {
        #[cfg(feature = "usd-resolver-http")]
        crate::remote::configure_project_cache(&project.name());
        #[cfg(feature = "usd-resolver-http")]
        crate::remote::configure_asset_network(&project.manifest().network);
        // Record the manifest in the recent-projects list (newest first) so the
        // startup dialog can offer it on the next launch. Covers both the
        // open-existing and create-new paths, which both funnel through here.
        crate::recent::push_recent_project(
            &mut self.recent_projects,
            project.manifest_path(),
        );
        crate::recent::save_recent_projects(&self.recent_projects);
        // `open_path` snapshots `self.world_unit_scale` for the loader thread,
        // so set it before opening the default stage.
        self.world_unit_scale = idtx_studio_session::RenderWorldUnitScale::from_metres(
            project.manifest().unit_scale.meters_per_unit(),
        );
        // Anchor the USD default-resolver search path at the project's asset
        // roots so bare/project-relative references (`@prop.usda@`) resolve
        // before the default stage opens. Append the built-in asset root so
        // `@built-in/...@` references (e.g. the default player) resolve too.
        // Non-fatal — surface but continue.
        let mut search_dirs = project.asset_search_dirs_abs();
        search_dirs.push(idtx_assets::builtin_root());
        if let Err(err) = idtx_usd::set_default_asset_search_path(&search_dirs) {
            self.status_message = Some(format!("Couldn't anchor asset resolver: {err}"));
        }
        if let Some(stage) = project.default_stage_abs() {
            self.open_path(&stage, None, None, false);
        } else {
            self.status_message = Some(format!(
                "Opened project '{}' — it has no default stage set.",
                project.name()
            ));
        }
        self.active_project = Some(project);
        self.window.request_redraw();
    }

    /// Open the native project-manifest picker (from the startup dialog or the
    /// File menu) and load the chosen `project.toml`.
    pub(crate) fn open_project_picker(&mut self) {
        let dialog = rfd::FileDialog::new().add_filter("IDTX project", &["toml"]);
        if let Some(path) = dialog.pick_file() {
            self.open_project_file(&path);
        }
    }

    /// Load a `project.toml` and activate it, dismissing the startup dialog on
    /// success or surfacing the error (in the dialog when open, else the status
    /// bar) on failure.
    pub(crate) fn open_project_file(&mut self, path: &Path) {
        match IdtxProject::load(path) {
            Ok(project) => {
                self.startup_dialog = None;
                self.activate_project(project);
            }
            Err(err) => {
                let message = format!("Couldn't open project: {err}");
                if let Some(dialog) = self.startup_dialog.as_mut() {
                    dialog.message = Some(message);
                } else {
                    self.status_message = Some(message);
                }
                self.window.request_redraw();
            }
        }
    }

    /// Create a brand-new project at `<parent_dir>/<name>/` and activate it.
    /// Delegates the on-disk scaffold to [`Self::scaffold_new_project`]; errors
    /// are surfaced in the startup dialog so the user can correct and retry
    /// without losing the sub-form.
    pub(crate) fn create_project(&mut self, parent_dir: &Path, name: &str) {
        match Self::scaffold_new_project(parent_dir, name) {
            Ok(project) => {
                self.startup_dialog = None;
                self.activate_project(project);
            }
            Err(message) => self.set_new_project_error(message),
        }
    }

    /// Validate a proposed project (folder) name, returning a user-facing
    /// reason string when it can't be used as a subfolder on the common desktop
    /// platforms, or `None` when it's acceptable. Shared by the startup dialog's
    /// Create-button gate and [`Self::scaffold_new_project`] so the interactive
    /// validation and the on-disk guard never drift apart.
    pub(crate) fn project_name_error(name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return Some("Enter a project name.".to_string());
        }
        // `.`/`..` would resolve `parent.join(name)` to the parent or grandparent
        // and scaffold outside the chosen folder.
        if name == "." || name == ".." {
            return Some("Choose a real project name, not \".\" or \"..\".".to_string());
        }
        if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Some("A project name can't contain any of / \\ : * ? \" < > |".to_string());
        }
        // Windows silently strips a trailing space/dot, so the created folder
        // wouldn't match the typed name — reject up front instead.
        if name.ends_with('.') || name.ends_with(' ') {
            return Some("A project name can't end with a space or a dot.".to_string());
        }
        // Windows reserved device names (also reserved with any extension, e.g.
        // `NUL.txt`); creating such a folder fails or produces a broken project.
        const RESERVED: &[&str] = &[
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        let stem = name.split('.').next().unwrap_or(name);
        if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
            return Some(format!(
                "\"{name}\" is a reserved name on Windows. Choose another."
            ));
        }
        None
    }

    /// Build the on-disk project scaffold at `<parent_dir>/<name>/`: the project
    /// subfolder, `project.toml` (asset root = `assets/`), the standard
    /// `stages/` + `assets/` + `behaviors/` subdirectories that the stage/asset
    /// dialogs default into, a default `input_map.toml` so the built-in player is
    /// interactive immediately, and a default `xr_controllers.toml` so an XR cook
    /// resolves device-accurate controller meshes. Pure I/O (no `ApplicationState`)
    /// so it is unit-testable; the user-facing message is returned as the `Err`
    /// string.
    pub(crate) fn scaffold_new_project(
        parent_dir: &Path,
        name: &str,
    ) -> Result<IdtxProject, String> {
        if let Some(reason) = Self::project_name_error(name) {
            return Err(reason);
        }
        let name = name.trim();
        let root = parent_dir.join(name);
        // Refuse to create into a path that already exists — whether it holds a
        // project or unrelated files — so we never silently merge the scaffold
        // into (or clobber) existing content. `project_name_error` has already
        // rejected `.`/`..` and separators, so `join` cannot escape the parent.
        if std::fs::symlink_metadata(&root).is_ok() {
            return Err(format!(
                "A folder named \"{name}\" already exists here. Choose another name."
            ));
        }
        let mut manifest = ProjectManifest::new(name);
        manifest.asset_roots = vec![PathBuf::from("assets")];
        let project = IdtxProject::create_child(parent_dir, Path::new(name), manifest)
            .map_err(|err| format!("Couldn't create project: {err}"))?;
        for sub in ["stages", "assets", "behaviors"] {
            project.project_root().create_dir_all(Path::new(sub))
                .map_err(|err| format!("Couldn't create project folders: {err}"))?;
        }
        // Write a default `input_map.toml` so the built-in player (used until the
        // project authors its own `player_stage`) is interactive out of the box:
        // its behaviors read `move_forward`/`move_right`/`look` (desktop) and
        // `move`/`turn` (XR) by name, which this map binds. Sourced from the
        // canonical baseline template (single source of truth).
        const DEFAULT_INPUT_MAP: &str =
            include_str!("../../idtx-assets/built-in/projects/input_map.toml");
        project.project_root().prepare_file(Path::new("input_map.toml"))
            .and_then(|file| file.write_new(DEFAULT_INPUT_MAP))
            .map_err(|err| format!("Couldn't write default input_map.toml: {err}"))?;
        // Write the default `xr_controllers.toml` sidecar (write-if-absent) so an
        // XR cook has the per-device entries (`questpro`/`quest2` + `generic`
        // fallback) and resolves device-accurate controller meshes — the meshes
        // themselves ship with the toolbox and are folded into the payload at cook
        // (see `idtx-build::cook`). Without the sidecar the cook falls back to the
        // `generic` mesh for every device.
        project
            .write_default_xr_controllers()
            .map_err(|err| format!("Couldn't write default xr_controllers.toml: {err}"))?;
        Ok(project)
    }

    /// Surface a new-project failure in the startup dialog (keeping the sub-form
    /// open for a retry), or the status bar if the dialog has since closed.
    fn set_new_project_error(&mut self, message: String) {
        if let Some(dialog) = self.startup_dialog.as_mut() {
            dialog.message = Some(message);
        } else {
            self.status_message = Some(message);
        }
        self.window.request_redraw();
    }

    // ── Level registry ────────────────────────────────────────────
    /// Rebuild the behavior inspector's available-input-actions cache from the
    /// active project's `input_map.toml`. Keyed on the project root *and* the
    /// map file's signature (length + modified time), so creating or editing
    /// the map while the project stays open refreshes the list rather than
    /// serving a stale one. Runs every frame, so a cache hit costs one `stat`.
    /// Missing/unparseable map ⇒ empty list (non-fatal), matching the runtime's
    /// `load_input_map`.
    pub(crate) fn refresh_behavior_input_actions(&mut self) {
        let root = self
            .active_project
            .as_ref()
            .map(|p| p.root().to_string_lossy().into_owned());
        let map_path = root
            .as_deref()
            .map(|dir| std::path::Path::new(dir).join("input_map.toml"));
        // File signature: length + modified time. `None` when the map is absent
        // (or its metadata is unreadable) — which itself changes the key the
        // moment the file appears, triggering a rebuild.
        let signature = map_path.as_ref().and_then(|path| {
            let meta = std::fs::metadata(path).ok()?;
            Some((meta.len(), meta.modified().ok()))
        });
        let key = root.map(|r| (r, signature));
        if key == self.behavior_input_actions_key {
            return;
        }
        self.behavior_input_actions_key = key;
        self.behavior_input_actions = map_path
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| idtx_studio_session::InputMap::parse(&text).ok())
            .map(|map| {
                let mut actions: Vec<(String, String)> = map
                    .actions()
                    .map(|spec| (spec.name.clone(), spec.kind.as_token().to_owned()))
                    .collect();
                actions.sort_by(|a, b| a.0.cmp(&b.0));
                actions
            })
            .unwrap_or_default();
    }

    /// Rebuild the level-browser cache when the active project's manifest
    /// changed. Reads each level's stage-kind tag (file I/O) only on a cache
    /// miss, keyed on the manifest content hash.
    pub(crate) fn refresh_level_cache(&mut self) {
        let Some(project) = self.active_project.as_ref() else {
            if !self.level_cache.rows.is_empty() || !self.level_cache.manifest_hash.is_empty() {
                self.level_cache = crate::model::LevelRegistryCache::default();
            }
            return;
        };
        let hash = project.manifest().content_hash();
        if !hash.is_empty() && hash == self.level_cache.manifest_hash {
            return;
        }
        let root = project.project_root();
        let rows = project
            .levels()
            .iter()
            .map(|rel| {
                let rel_path = rel.to_string_lossy().into_owned();
                let stem = rel
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| rel_path.clone());
                let abs = root.resolve_relative(rel);
                let exists = abs.as_ref().is_some_and(|p| p.is_file());
                let (kind_token, display_name) = match &abs {
                    Some(p) if exists => match idtx_usd::read_stage_kind(p) {
                        Ok(tag) => {
                            let name = if tag.display_name.is_empty() {
                                stem.clone()
                            } else {
                                tag.display_name
                            };
                            (tag.stage_kind, name)
                        }
                        Err(_) => (String::new(), stem.clone()),
                    },
                    _ => (String::new(), stem.clone()),
                };
                crate::model::CachedLevelRow {
                    display_name,
                    rel_path,
                    kind_token,
                    abs_path: abs,
                    missing: !exists,
                }
            })
            .collect();
        self.level_cache = crate::model::LevelRegistryCache {
            manifest_hash: hash,
            rows,
        };
    }

    /// Build the per-frame level-browser snapshot from the cache, marking the
    /// row whose stage is the active tab's open stage.
    pub(crate) fn level_browser_rows(&self) -> Vec<idtx_editor_ui::level_browser::LevelRow> {
        let active_canon = self
            .active_tab_id
            .and_then(|id| self.tabs.iter().find(|t| t.session.id().get() == id))
            .map(|t| {
                let p = t.session.source_path();
                std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
            });
        self.level_cache
            .rows
            .iter()
            .map(|r| {
                let is_open = match (&r.abs_path, &active_canon) {
                    (Some(a), Some(b)) => {
                        &std::fs::canonicalize(a).unwrap_or_else(|_| a.clone()) == b
                    }
                    _ => false,
                };
                idtx_editor_ui::level_browser::LevelRow {
                    display_name: r.display_name.clone(),
                    rel_path: r.rel_path.clone(),
                    kind_token: r.kind_token.clone(),
                    missing: r.missing,
                    is_open,
                }
            })
            .collect()
    }

    /// Snapshot the current asset-browser directory into UI-neutral rows the
    /// panel renders. Reads the active project's listing of
    /// [`Self`]`::asset_browser_subdir` fresh each frame (a single sandbox-
    /// confined `read_dir`); an out-of-sandbox or missing dir yields no rows.
    pub(crate) fn asset_browser_rows(&self) -> Vec<idtx_editor_ui::asset_browser::AssetRow> {
        use idtx_editor_ui::asset_browser::{AssetGlyph, AssetRow};
        use idtx_project::AssetKind;
        let Some(project) = self.active_project.as_ref() else {
            return Vec::new();
        };
        project
            .list_assets(&self.asset_browser_subdir)
            .into_iter()
            .map(|e| {
                let glyph = match e.kind {
                    AssetKind::Folder => AssetGlyph::Folder,
                    AssetKind::Stage => AssetGlyph::Usd,
                    AssetKind::Texture => AssetGlyph::Image,
                    AssetKind::Material => AssetGlyph::Material,
                    AssetKind::Model => AssetGlyph::Mesh,
                    AssetKind::Audio => AssetGlyph::Audio,
                    AssetKind::Video => AssetGlyph::Video,
                    AssetKind::Data => AssetGlyph::Data,
                    AssetKind::Other => AssetGlyph::File,
                };
                AssetRow {
                    name: e.name,
                    // Normalise to `/` so the panel's breadcrumb/parent logic is
                    // platform-independent; the shell parses it back through the
                    // sandbox on navigation.
                    rel_path: e.rel_path.to_string_lossy().replace('\\', "/"),
                    glyph,
                    is_dir: e.is_dir,
                    is_stage: e.kind.is_stage(),
                }
            })
            .collect()
    }

    /// Navigate the asset browser to `subdir` (`/`-separated project-relative,
    /// `""` = root). Rejected when it escapes the sandbox or is not a directory,
    /// so the browser never shows a path outside the project.
    pub(crate) fn set_asset_dir(&mut self, subdir: String) {
        let ok = subdir.is_empty()
            || self
                .active_project
                .as_ref()
                .and_then(|p| {
                    p.project_root()
                        .resolve_relative(std::path::Path::new(&subdir))
                })
                .is_some_and(|abs| abs.is_dir());
        if ok {
            self.asset_browser_subdir = subdir;
            self.asset_browser_state.selected = None;
        }
        self.window.request_redraw();
    }

    /// Author a reference (`payload = false`) or payload (`payload = true`) of
    /// the project-relative `asset_rel_path` onto the active tab's currently
    /// selected prim. No-ops (with a status hint) when nothing is selected.
    ///
    /// The asset path is authored verbatim as a project-relative arc, resolved
    /// at composition time via the project-root search path.
    #[cfg(feature = "authoring")]
    pub(crate) fn author_asset_arc(&mut self, asset_rel_path: String, payload: bool) {
        let Some(idx) = self.active_tab_index() else {
            return;
        };
        let Some(prim) = self.tabs[idx].session.selected_path().map(str::to_owned) else {
            self.status_message =
                Some("Select a prim in the scene tree to attach the asset to.".to_owned());
            self.window.request_redraw();
            return;
        };
        let label = if payload {
            "Add Payload"
        } else {
            "Add Reference"
        };
        let result = self.journaled_structural(idx, label, |s| {
            if payload {
                s.add_payload(&prim, &asset_rel_path, None)
            } else {
                s.add_reference(&prim, &asset_rel_path, None)
            }
        });
        match result {
            Ok(()) => {
                self.status_message = Some(format!("{label}: {asset_rel_path} → {prim}"));
                self.window.request_redraw();
            }
            Err(e) => {
                tracing::warn!(error = %e, "asset-browser arc authoring failed");
                self.status_message = Some(format!("Couldn't {}: {e}", label.to_lowercase()));
                self.window.request_redraw();
            }
        }
    }

    /// Resolve which prim a USD-stage asset dropped onto the scene tree should
    /// compose, then author the arc-bearing child. The dropped stage's
    /// root layer drives the choice:
    /// - a `defaultPrim` → the arc targets it (`@asset@</DefaultPrim>`);
    /// - no `defaultPrim`, one root `def` prim → auto-target that prim;
    /// - no `defaultPrim`, several root `def` prims → open the drop-target
    ///   picker so the user chooses;
    /// - nothing composable (or an unreadable root layer) → author an
    ///   untargeted arc (the prior behavior), which composes the asset's
    ///   `defaultPrim` if it has one.
    ///
    /// Reading the root layer is cheap ([`idtx_usd::read_stage_root_prims`]
    /// opens just the `SdfLayer`, no stage composition).
    #[cfg(feature = "authoring")]
    pub(crate) fn resolve_drop_target(
        &mut self,
        idx: usize,
        parent: String,
        asset_rel: String,
        as_reference: bool,
    ) {
        // Resolve the project-relative asset to an absolute path so its root
        // layer can be read. No active project (or a path escaping the sandbox)
        // leaves `targets` empty → untargeted-arc fallback below.
        let targets = self
            .active_project
            .as_ref()
            .and_then(|p| p.project_root().resolve_relative(Path::new(&asset_rel)))
            .and_then(|abs| match idtx_usd::read_stage_root_prims(&abs) {
                Ok(targets) => Some(targets),
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't read dropped stage's root prims");
                    None
                }
            });

        let target = match targets.as_ref() {
            // Authored defaultPrim: compose it explicitly.
            Some(t) if t.default_prim.is_some() => {
                t.default_prim.as_ref().map(|name| format!("/{name}"))
            }
            // No defaultPrim but a single composable root: auto-pick it.
            Some(t) if t.root_def_prims.len() == 1 => Some(format!("/{}", t.root_def_prims[0])),
            // No defaultPrim and several roots: let the user choose.
            Some(t) if t.root_def_prims.len() >= 2 => {
                self.drop_target_picker = Some(crate::model::DropTargetPickerDialog {
                    tab_id: self.tabs[idx].session.id().get(),
                    parent_prim: parent,
                    asset_rel,
                    as_reference,
                    candidates: t.root_def_prims.clone(),
                });
                self.window.request_redraw();
                return;
            }
            // Nothing composable / unreadable: fall back to an untargeted arc.
            _ => None,
        };

        self.author_dropped_asset(idx, &parent, &asset_rel, as_reference, target.as_deref());
    }

    /// Author the arc-bearing child for a dropped asset — a payload, or a
    /// reference when `as_reference` — targeting `target` (an absolute prim
    /// path, or `None` for the asset's `defaultPrim`). Journaled as one undo
    /// unit; selects the new child on success. Shared by the immediate drop
    /// path and the drop-target picker's commit.
    #[cfg(feature = "authoring")]
    pub(crate) fn author_dropped_asset(
        &mut self,
        idx: usize,
        parent: &str,
        asset_rel: &str,
        as_reference: bool,
        target: Option<&str>,
    ) {
        let (label, noun) = if as_reference {
            ("Reference Asset as Child", "Reference Asset")
        } else {
            ("Payload Asset as Child", "Payload Asset")
        };
        match self.journaled_structural(idx, label, |s| {
            s.add_asset_child_arc(parent, asset_rel, as_reference, target)
        }) {
            Ok(Some(new_path)) => {
                self.tabs[idx].session.set_selected(Some(new_path.clone()));
                self.tabs[idx].scene_tree_state.selected = Some(new_path);
                self.window.request_redraw();
            }
            Ok(None) => {
                self.status_message = Some(format!("{noun}: prim {parent} no longer exists"));
            }
            Err(e) => {
                tracing::warn!("drop asset-as-child failed: {e}");
                self.status_message = Some(format!("{noun} failed: {e}"));
            }
        }
    }

    pub(crate) fn open_level(&mut self, index: usize) {
        let Some(project) = self.active_project.as_ref() else {
            return;
        };
        let Some(rel) = project.levels().get(index) else {
            return;
        };
        let Some(abs) = project.project_root().resolve_relative(rel) else {
            self.status_message = Some("That level path is outside the project.".to_owned());
            self.window.request_redraw();
            return;
        };
        if !abs.is_file() {
            self.status_message = Some(format!(
                "Level file not found: {}",
                idtx_project::normalize_display_path(&abs)
            ));
            self.window.request_redraw();
            return;
        }
        // Don't open a second tab for a level that's already open (or currently
        // loading) — activate the existing tab instead.
        if let Some(id) = self
            .tabs
            .iter()
            .find(|t| paths_equal(t.session.source_path(), &abs))
            .map(|t| t.session.id().get())
        {
            self.active_tab_id = Some(id);
            self.window.request_redraw();
            return;
        }
        if self.loading_tabs.iter().any(|lt| {
            lt.source_path
                .as_deref()
                .is_some_and(|p| paths_equal(p, &abs))
        }) {
            self.window.request_redraw();
            return;
        }
        self.open_path(&abs, None, None, false);
    }

    /// Open the project-relative USD stage `asset_rel_path` (from the asset
    /// browser) in a tab. Resolves the path through the project sandbox and,
    /// like [`Self::open_level`], activates an existing tab rather than opening
    /// a second one when the stage is already open or loading.
    pub(crate) fn open_asset_stage(&mut self, asset_rel_path: &str) {
        let Some(project) = self.active_project.as_ref() else {
            return;
        };
        let Some(abs) = project
            .project_root()
            .resolve_relative(std::path::Path::new(asset_rel_path))
        else {
            self.status_message = Some("That asset path is outside the project.".to_owned());
            self.window.request_redraw();
            return;
        };
        if !abs.is_file() {
            self.status_message = Some(format!(
                "Asset file not found: {}",
                idtx_project::normalize_display_path(&abs)
            ));
            self.window.request_redraw();
            return;
        }
        // Don't open a second tab for a stage that's already open (or currently
        // loading) — activate the existing tab instead.
        if let Some(id) = self
            .tabs
            .iter()
            .find(|t| paths_equal(t.session.source_path(), &abs))
            .map(|t| t.session.id().get())
        {
            self.active_tab_id = Some(id);
            self.window.request_redraw();
            return;
        }
        if self.loading_tabs.iter().any(|lt| {
            lt.source_path
                .as_deref()
                .is_some_and(|p| paths_equal(p, &abs))
        }) {
            self.window.request_redraw();
            return;
        }
        self.open_path(&abs, None, None, false);
    }

    /// Remove the registry level at `index` (the stage file is kept on disk).
    pub(crate) fn remove_level(&mut self, index: usize) {
        let Some(project) = self.active_project.as_mut() else {
            return;
        };
        let Some(rel) = project.levels().get(index).cloned() else {
            return;
        };
        if project.remove_level(&rel) {
            self.persist_project_manifest();
        }
    }

    /// Reorder a registry level from one slot to another and persist.
    pub(crate) fn move_level(&mut self, from: usize, to: usize) {
        let Some(project) = self.active_project.as_mut() else {
            return;
        };
        if project.move_level(from, to) {
            self.persist_project_manifest();
        }
    }

    /// Register an existing in-project stage as a level via an open dialog.
    pub(crate) fn add_existing_level_dialog(&mut self) {
        let Some(root) = self.active_project.as_ref().map(|p| p.root().to_path_buf()) else {
            return;
        };
        let stages = root.join("stages");
        let path = rfd::FileDialog::new()
            .add_filter("OpenUSD stage", &["usda", "usd", "usdc"])
            .set_directory(if stages.is_dir() { stages } else { root })
            .pick_file();
        let Some(path) = path else { return };
        let Some(project) = self.active_project.as_mut() else {
            return;
        };
        if project.add_level(&path) {
            self.persist_project_manifest();
        } else {
            self.status_message =
                Some("That stage is already a level or lies outside the project root.".to_owned());
            self.window.request_redraw();
        }
    }

    /// Create a brand-new level stage (save dialog), tag it `StageKind::Level`,
    /// register it in the project, and open it.
    #[cfg(feature = "authoring")]
    pub(crate) fn new_level_dialog(&mut self) {
        let Some(root) = self.active_project.as_ref().map(|p| p.root().to_path_buf()) else {
            return;
        };
        let stages = root.join("stages");
        let path = rfd::FileDialog::new()
            .add_filter("OpenUSD stage", &["usda", "usd", "usdc"])
            .set_file_name("level.usda")
            .set_directory(if stages.is_dir() { stages } else { root })
            .save_file();
        let Some(path) = path else { return };
        let meters_per_unit = self
            .active_project
            .as_ref()
            .map_or(1.0, |p| p.manifest().unit_scale.meters_per_unit());
        let Some(project) = self.active_project.as_ref() else { return; };
        let path = match create_level_file(&project.project_root(), &path, meters_per_unit) {
            Ok(path) => path,
            Err(err) => {
                self.status_message = Some(format!("Couldn't create level: {err}"));
                self.window.request_redraw();
                return;
            }
        };
        if let Some(project) = self.active_project.as_mut() {
            project.add_level(&path);
        }
        self.persist_project_manifest();
        self.open_path(&path, None, None, false);
    }

    /// Author a new player rig + local overlay for `kind` into the active
    /// project, register the `[[players]]` entry, and open the
    /// local overlay for editing.
    ///
    /// Unlike [`new_level_dialog`](Self::new_level_dialog) there is no save
    /// dialog: the two files land at canonical project locations
    /// (`assets/<slug>_player_rig.usda` + `stages/<slug>_player.usda`) and the
    /// factory persists the manifest itself. Re-running the same `kind` refreshes
    /// the existing files in place rather than creating a duplicate entry.
    #[cfg(feature = "authoring")]
    pub(crate) fn create_player_stage(&mut self) {
        let Some(project) = self.active_project.as_mut() else {
            self.status_message = Some("Open a project first to create a player stage.".to_owned());
            self.window.request_redraw();
            return;
        };
        let artifacts = match crate::player_template::create_player_stage(project) {
            Ok(artifacts) => artifacts,
            Err(err) => {
                self.status_message = Some(format!("Couldn't create player stage: {err}"));
                self.window.request_redraw();
                return;
            }
        };
        // The factory saved the manifest; refresh so the browser reflects the new
        // content hash, then open the player stage for editing.
        self.refresh_level_cache();
        self.status_message = Some(format!(
            "Created player stage ({}).",
            idtx_project::normalize_display_path(&artifacts.overlay_path)
        ));
        self.open_path(&artifacts.overlay_path, None, None, false);
    }

    /// Persist the active project's manifest, surfacing any write error.
    pub(crate) fn persist_project_manifest(&mut self) {
        if let Some(project) = self.active_project.as_ref() {
            if let Err(err) = project.save() {
                self.status_message = Some(format!("Couldn't save project: {err}"));
            }
        }
        self.window.request_redraw();
    }

    /// Reloads the active stage, evicting its in-memory stage/layer state.
    ///
    /// A **local** stage reopens from the file system. A **remote** stage
    /// re-fetches itself — root layer plus all referenced sublayers,
    /// references, and textures — from its server with force-refresh on,
    /// overwriting the cached copies, then reopens.
    ///
    /// A remote stage that is part of a live **single/collab session** is a
    /// special case: the session is *kept* rather than torn down. Dropping and
    /// reopening the tab would create a brand-new [`StudioSession`] the collab
    /// client is no longer bound to, so instead the attached client is
    /// reconnected — [`CollabClient::reconnect`][idtx_collab::client::CollabClient::reconnect]
    /// re-gates on a fresh server snapshot, re-syncing the stage to the latest
    /// authoritative state on the server. (The collab channel is the
    /// authoritative source of live state here, so no cache eviction / stage
    /// reopen is needed on this path.)
    ///
    /// For a plain remote stage (no live session), the HTTP resolver serves the
    /// cache on a normal open, so a remote reload arms the resolver's
    /// force-refresh (via the `force_refresh` arg to
    /// [`open_path`][Self::open_path]) so every asset is re-fetched and
    /// overwritten in place during the reopen. The current session is dropped
    /// first so its cache-file handles are released before the overwrite.
    pub(crate) fn reload_active(&mut self) {
        let Some(id) = self.active_tab_id else { return };
        let Some(idx) = self.tabs.iter().position(|t| t.session.id().get() == id) else {
            return;
        };
        // Live single/collab session: keep it and re-sync to the server's
        // latest state instead of reopening the stage.
        #[cfg(feature = "remote")]
        if self.tabs[idx].session.has_collab() {
            match self.tabs[idx].session.reconnect_collab() {
                Ok(()) => {
                    self.tabs[idx].collab_was_connected = true;
                    self.status_message = Some(
                        "Reload: awaiting authoritative replay; local opinions retained in recovery."
                            .to_owned(),
                    );
                }
                Err(e) => {
                    self.status_message = Some(format!("Reload (session re-sync) failed: {e}"));
                }
            }
            self.window.request_redraw();
            return;
        }
        let remote_url = match self.tabs[idx].session.source() {
            idtx_studio_session::SessionSource::Remote { url } => Some(url.clone()),
            idtx_studio_session::SessionSource::Local(_) => None,
        };
        // Only a remote stage force-refreshes, and only when the resolver that
        // serves the cache is compiled in.
        let force_refresh = cfg!(feature = "remote") && remote_url.is_some();
        #[cfg(feature = "remote")]
        if force_refresh {
            // Re-arm the auth token in case it was cleared; the resolver's
            // fetch callback reads the shared `idtx_auth` bucket on every download.
            if let Err(error) = self.arm_server_token() {
                self.status_message = Some(error);
                return;
            }
        }
        let path = self.tabs[idx].session.source_path().to_path_buf();
        // Drop the current session first so USD releases any cache-file handles
        // before a force-refresh overwrites them, then reopen.
        self.egui_renderer.free_texture(&self.tabs[idx].texture_id);
        self.tabs.remove(idx);
        self.active_tab_id = self.tabs.last().map(|t| t.session.id().get());
        self.open_path(&path, remote_url, Some(idx), force_refresh);
        self.window.request_redraw();
    }

    /// Handle a server `LayerReloaded` for the collab session in tab `tab_id`:
    /// a file the stage uses was rewritten server-side (another session's
    /// commit, or a replacing upload). Re-fetch that one file with the HTTP
    /// resolver's force-refresh armed (so the stale cached copy is overwritten),
    /// reload it into the stage in place, then re-gate the collab client on a
    /// fresh snapshot so the session's overrides are re-applied on top of the
    /// new content. The collab client and tab are kept (no reopen), so the
    /// session binding survives.
    ///
    /// The reloaded layer's identifier is rebuilt from the stage's remote root
    /// URL origin plus the server's (percent-encoded, canonical) download URI;
    /// `usd_uri` is decoded so it matches the raw identifier USD opened the
    /// layer with. If the opened path was not already canonical the identifier
    /// will not match and `reload_layer` reports it — surfaced in the status bar.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) fn apply_collab_layer_reload(
        &mut self,
        tab_id: u64,
        reload: &idtx_studio_session::PendingLayerReload,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.session.id().get() == tab_id) else {
            return;
        };
        let root_url = match self.tabs[idx].session.source() {
            idtx_studio_session::SessionSource::Remote { url } => url.clone(),
            idtx_studio_session::SessionSource::Local(_) => return,
        };
        // origin = everything before "/api/v1/download/" in the root layer URL.
        let Some(origin) = root_url.split("/api/v1/download/").next() else {
            return;
        };
        let identifier = format!("{origin}{}", percent_decode_path(&reload.usd_uri));

        // The resolver's fetch callback reads the shared auth bucket on every
        // download; re-arm in case it was cleared.
        if let Err(error) = self.arm_server_token() {
            self.status_message = Some(error);
            return;
        }
        #[cfg(feature = "usd-resolver-http")]
        idtx_usd_resolver_http::set_force_refresh(true);
        let reloaded = self.tabs[idx].session.reload_layer(&identifier);
        #[cfg(feature = "usd-resolver-http")]
        idtx_usd_resolver_http::set_force_refresh(false);

        match reloaded {
            Ok(()) => match self.tabs[idx].session.reconnect_collab() {
                Ok(()) => {
                    self.tabs[idx].collab_was_connected = true;
                    self.status_message = Some(format!(
                        "Server reloaded {}; re-syncing session state…",
                        reload.usd_path
                    ));
                }
                Err(e) => {
                    self.status_message =
                        Some(format!("Layer reload re-sync failed: {e}"));
                }
            },
            Err(e) => {
                self.status_message =
                    Some(format!("Failed to reload {}: {e}", reload.usd_path));
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn close_tab(&mut self, id: u64) {        if let Some(idx) = self.tabs.iter().position(|t| t.session.id().get() == id) {
            self.egui_renderer.free_texture(&self.tabs[idx].texture_id);
            // Tear down any live collab session before dropping the session.
            #[cfg(feature = "remote")]
            if let Some(client) = self.tabs[idx].session.detach_collab() {
                client.close();
            }
            self.tabs.remove(idx);
            if self.active_tab_id == Some(id) {
                self.active_tab_id = self.tabs.last().map(|t| t.session.id().get());
            }
            self.window.request_redraw();
        }
    }

    pub(crate) fn active_tab_index(&self) -> Option<usize> {
        let id = self.active_tab_id?;
        self.tabs.iter().position(|t| t.session.id().get() == id)
    }

    /// Begin closing the whole app. Returns `true` when it is safe to exit
    /// immediately (no stage has unsaved changes). Otherwise queues every dirty
    /// stage into the save-on-close flow and returns `false` — the caller must
    /// not exit yet; the modal resolves each stage and sets `should_exit`.
    #[cfg(feature = "authoring")]
    pub(crate) fn begin_app_close(&mut self) -> bool {
        // A close flow is already in progress (a queued save may still be
        // in-flight). Reinitializing `pending_close` here would reset `saving`
        // and rebuild the queue, so the completing save would no longer advance
        // it — leaving the app wedged in the close prompt. Keep the existing
        // flow and just nudge a repaint of the modal.
        if self.pending_close.is_some() {
            self.window.request_redraw();
            return false;
        }
        let dirty: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| t.session.is_stage_dirty())
            .map(|t| t.session.id().get())
            .collect();
        if dirty.is_empty() {
            return true;
        }
        self.pending_close = Some(crate::model::PendingClose {
            queue: dirty,
            intent: crate::model::CloseIntent::ExitApp,
            saving: false,
        });
        self.window.request_redraw();
        false
    }

    /// Request to close a single tab. In authoring builds a clean stage closes
    /// immediately while a dirty stage opens the save-on-close modal; the
    /// read-only Inspector always closes immediately.
    pub(crate) fn request_close_tab(&mut self, id: u64) {
        #[cfg(feature = "authoring")]
        {
            // A close flow is already in progress (e.g. an app-exit queue with a
            // queued save in-flight). Overwriting `pending_close` here would drop
            // the remaining queue and its `CloseIntent` — the same hazard
            // `begin_app_close` guards against — so leave it intact and just nudge
            // a repaint of the active modal.
            if self.pending_close.is_some() {
                self.window.request_redraw();
                return;
            }
            let dirty = self
                .tabs
                .iter()
                .find(|t| t.session.id().get() == id)
                .is_some_and(|t| t.session.is_stage_dirty());
            if self.pending_close.is_some() {
                self.window.request_redraw();
                return;
            }
            if dirty {
                self.pending_close = Some(crate::model::PendingClose {
                    queue: vec![id],
                    intent: crate::model::CloseIntent::JustTab,
                    saving: false,
                });
                self.window.request_redraw();
                return;
            }
        }
        self.close_tab(id);
    }

    /// Resolve the head tab of the save-on-close queue: optionally close it, pop
    /// it, and advance. When the queue drains, exit the app if the request was a
    /// whole-app close.
    #[cfg(feature = "authoring")]
    pub(crate) fn finish_close_head(&mut self, close: bool) {
        let (head, exit) = {
            let Some(pc) = self.pending_close.as_mut() else {
                return;
            };
            if pc.queue.is_empty() {
                self.pending_close = None;
                return;
            }
            let head = pc.queue.remove(0);
            pc.saving = false;
            let empty = pc.queue.is_empty();
            let exit = empty && pc.intent == crate::model::CloseIntent::ExitApp;
            (head, exit)
        };
        if self
            .pending_close
            .as_ref()
            .is_some_and(|pc| pc.queue.is_empty())
        {
            self.pending_close = None;
        }
        if close {
            self.close_tab(head);
        }
        if exit {
            self.should_exit = true;
        }
        self.window.request_redraw();
    }

    /// Save the head tab of the save-on-close queue, then (on success) close it
    /// and advance. Local writable stages save in-place via the deferred
    /// `pending_saves` path; remote / unsaved stages route through the native
    /// "Save As" dialog. `do_save` advances the queue once the write succeeds.
    #[cfg(feature = "authoring")]
    pub(crate) fn save_and_close(&mut self) {
        let Some(head) = self
            .pending_close
            .as_ref()
            .and_then(|pc| pc.queue.first().copied())
        else {
            return;
        };
        let Some((is_remote, source)) = self
            .tabs
            .iter()
            .find(|t| t.session.id().get() == head)
            .map(|t| (t.session.is_remote(), t.session.source_path().to_path_buf()))
        else {
            // The tab vanished; drop it from the queue and move on.
            self.finish_close_head(false);
            return;
        };
        if let Some(pc) = self.pending_close.as_mut() {
            pc.saving = true;
        }
        if is_remote {
            // Native dialog is synchronous; its `do_save` advances the queue.
            // If the user cancels the dialog the save simply doesn't happen and
            // the modal stays up.
            self.save_as_dialog(head);
        } else {
            // Defer the in-place save to the top of the next frame (a save is a
            // multi-second, UI-thread-blocking OpenUSD call); `do_save` advances
            // the close queue when it completes.
            self.pending_saves.push((head, source));
            self.window.request_redraw();
        }
    }

    /// `true` when the active tab's stage was opened from a remote server.
    #[cfg(feature = "authoring")]
    pub(crate) fn active_is_remote(&self) -> bool {
        self.active_tab_index()
            .map(|i| self.tabs[i].session.is_remote())
            .unwrap_or(false)
    }

    /// Commit the head tab of the save-on-close queue to its remote server
    /// instead of writing a local file, then (on success) clear its dirty flag,
    /// close it, and advance the queue. The committed server state becomes the
    /// authoritative saved copy, so no local overwrite prompt is involved. On
    /// failure the modal stays up (its in-flight flag cleared) so the user can
    /// retry, save a local copy, discard, or cancel. Only meaningful for a
    /// remote stage with a live collab session; the close dialog gates the
    /// button on exactly that.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) fn commit_remote_and_close(&mut self) {
        let Some(head) = self
            .pending_close
            .as_ref()
            .and_then(|pc| pc.queue.first().copied())
        else {
            return;
        };
        // Commit while the tab borrow is scoped, then act on the result after it
        // ends so `finish_close_head` can re-borrow `self`.
        let result = {
            let Some(t) = self
                .tabs
                .iter_mut()
                .find(|t| t.session.id().get() == head)
            else {
                // The tab vanished; drop it from the queue and move on.
                self.finish_close_head(false);
                return;
            };
            t.session.commit_collab()
        };
        match result {
            Ok(()) => {
                self.status_message =
                    Some("Waiting for collaboration acknowledgements and commit…".to_owned());
                if let Some(pc) = self.pending_close.as_mut() { pc.saving = true; }
            }
            Err(e) => {
                self.status_message = Some(format!("Commit failed: {e}"));
                if let Some(pc) = self.pending_close.as_mut() {
                    pc.saving = false;
                }
                self.window.request_redraw();
            }
        }
    }

    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) fn poll_remote_commits(&mut self) {
        let results: Vec<_> = self.tabs.iter_mut().filter_map(|tab|
            tab.session.poll_collab_commit().map(|result| (tab.session.id().get(), result))).collect();
        for (id, result) in results {
            let closing = self.pending_close.as_ref().is_some_and(|pc| pc.saving && pc.queue.first() == Some(&id));
            match result {
                Ok(true) => {
                    self.status_message = Some("Remote session committed successfully.".into());
                    if closing { self.finish_close_head(true); }
                }
                Ok(false) => {
                    self.status_message = Some("Synchronized edits committed; local-only or newer changes remain unsaved. Save As preserves a local copy.".into());
                    if closing && let Some(pc) = &mut self.pending_close { pc.saving = false; }
                }
                Err(error) => {
                    self.status_message = Some(format!("Commit failed: {error}"));
                    if closing && let Some(pc) = &mut self.pending_close { pc.saving = false; }
                }
            }
        }
    }

    #[cfg(feature = "authoring")]
    pub(crate) fn undo_active(&mut self) {
        let Some(idx) = self.active_tab_index() else {
            return;
        };
        let tab = &mut self.tabs[idx];
        if let Err(error) = tab.journal.try_undo(|cmd| apply_edit_command_reverse(cmd, &mut tab.session)) {
            self.status_message = Some(format!("Undo failed: {error}"));
        }
        self.window.request_redraw();
    }

    #[cfg(feature = "authoring")]
    pub(crate) fn redo_active(&mut self) {
        let Some(idx) = self.active_tab_index() else {
            return;
        };
        let tab = &mut self.tabs[idx];
        if let Err(error) = tab.journal.try_redo(|cmd| apply_edit_command_forward(cmd, &mut tab.session)) {
            self.status_message = Some(format!("Redo failed: {error}"));
        }
        self.window.request_redraw();
    }

    /// Open the overwrite-confirmation dialog for the active tab's source path.
    #[cfg(feature = "authoring")]
    pub(crate) fn begin_save(&mut self, tab_id: u64) {
        let Some(tab) = self.tabs.iter().find(|t| t.session.id().get() == tab_id) else {
            return;
        };
        let path = tab.session.source_path().to_path_buf();
        self.pending_save_confirm = Some(SaveConfirmInfo { tab_id, path });
        self.window.request_redraw();
    }

    /// Default directory for the "Save As…" dialog when the source path has no
    /// usable parent (e.g. a remote stage, whose "path" is an HTTP URL).
    ///
    /// Preference order: last successful save location → the running
    /// executable's directory → none. Deliberately never returns the remote
    /// project cache directory (`~/.idtx-studio/<project>`), which holds transient
    /// downloads that must not become an implicit save target.
    #[cfg(feature = "authoring")]
    fn save_as_start_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = &self.last_save_dir {
            return Some(dir.clone());
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return Some(dir.to_owned());
            }
        }
        None
    }

    /// Open a native "Save As" file-chooser and write the stage root layer to
    /// the chosen path. The allowed extensions depend on the source format:
    ///   usda / usdc → usda, usdc, usdz
    ///   usdz        → usdz only
    #[cfg(feature = "authoring")]
    pub(crate) fn save_as_dialog(&mut self, tab_id: u64) {
        let (source, is_remote) = {
            let Some(tab) = self.tabs.iter().find(|t| t.session.id().get() == tab_id) else {
                return;
            };
            (
                tab.session.source_path().to_path_buf(),
                tab.session.is_remote(),
            )
        };
        let source_ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        // A remote stage's "path" is an HTTP URL with no local parent; fall back
        // to a sensible local directory instead of a partial URL.
        let start_dir = if is_remote {
            self.save_as_start_dir()
                .unwrap_or_else(|| PathBuf::from("."))
        } else {
            source
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        };

        let mut dialog = rfd::FileDialog::new().set_directory(&start_dir);
        if source_ext == "usdz" {
            dialog = dialog.add_filter("USDZ Package", &["usdz"]);
        } else {
            dialog = dialog
                .add_filter("OpenUSD Layer (usda, usdc)", &["usda", "usdc"])
                .add_filter("USDZ Package", &["usdz"])
                .add_filter("All USD", &["usda", "usdc", "usdz"]);
        }
        if let Some(path) = dialog.save_file() {
            self.do_save(tab_id, path);
        }
    }

    /// Execute the save/export to `path` for tab `tab_id`.
    ///
    /// Routing:
    /// - `.usdz` destination → [`StudioSession::save_as_usdz`] (handles both
    ///   usdz→usdz repack and loose→usdz packaging via `UsdUtilsCreateNewUsdzPackage`)
    /// - Same path as source (non-usdz) → [`StudioSession::save_stage`] (in-place save)
    /// - Different path (non-usdz) → [`StudioSession::export_root_layer`] (copy to new path)
    #[cfg(feature = "authoring")]
    pub(crate) fn do_save(&mut self, tab_id: u64, path: PathBuf) {
        let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|t| t.session.id().get() == tab_id)
        else {
            return;
        };
        let source = tab.session.source_path().to_path_buf();

        let dest_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let result = if dest_ext == "usdz" {
            tab.session.save_as_usdz(&path)
        } else if path == source {
            tab.session.save_stage(&path)
        } else {
            tab.session.export_root_layer(&path)
        };

        let mut saved_ok = false;
        match result {
            Ok(()) => {
                saved_ok = true;
                // The stage now lives at `path` on the local filesystem: record
                // the directory for the next Save As, and flip the session to a
                // Local source so in-place "Save" is (re-)enabled — including
                // for stages that were originally opened from a remote server.
                self.last_save_dir = path.parent().map(std::path::Path::to_path_buf);
                tab.session.mark_saved_local(path.clone());
                // The saved document is now clean — clear the unsaved-changes
                // flag so the tab's dirty marker disappears.
                tab.session.mark_stage_saved();
                self.status_message = Some(format!("Saved: {}", path.display()));
            }
            Err(e) => {
                self.status_message = Some(format!("Save failed: {e}"));
                tracing::error!("Save failed for {}: {e}", path.display());
            }
        }

        // If a save-on-close flow is waiting on this tab, advance it: close the
        // tab and move to the next on success, or re-open the prompt (clearing
        // the in-flight flag) so the user can retry or cancel on failure.
        let awaiting_close = self
            .pending_close
            .as_ref()
            .is_some_and(|pc| pc.saving && pc.queue.first() == Some(&tab_id));
        if awaiting_close {
            if saved_ok {
                self.finish_close_head(true);
            } else if let Some(pc) = self.pending_close.as_mut() {
                pc.saving = false;
            }
        }
        self.window.request_redraw();
    }
}

/// Compare two filesystem paths for identity, canonicalising both when possible
/// so `.`/`..` segments and symlinks don't produce spurious mismatches. Falls
/// back to a literal comparison when a path can't be canonicalised (e.g. it no
/// longer exists on disk).
fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The path-only USD writer gets private scratch storage; publication into the
/// selected project is through a confined file capability.
#[cfg(feature = "authoring")]
fn create_level_file(root: &idtx_project::ProjectRoot, path: &Path, meters_per_unit: f64) -> Result<PathBuf, String> {
    let relative = root.relative_path(path).map_err(|e| e.to_string())?;
    let target = root.prepare_file(&relative).map_err(|e| e.to_string())?;
    let scratch = tempfile::Builder::new().prefix("idtx-new-level-").tempdir().map_err(|e| e.to_string())?;
    let staged = scratch.path().join(target.path().file_name().ok_or("missing level file name")?);
    let display = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    {
        let mut stage = idtx_usd::StageService::create_new(&staged, "Y", meters_per_unit).map_err(|e| e.to_string())?;
        stage.set_stage_kind("level", &display).map_err(|e| e.to_string())?;
        stage.save_stage(&staged).map_err(|e| e.to_string())?;
    }
    target.copy_from(&staged, false).map_err(|e| e.to_string())?;
    root.resolve_relative_checked(&relative).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::ApplicationState;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[cfg(feature = "authoring")]
    #[test]
    fn new_level_uses_confined_publication_and_preserves_usd_format() {
        let dir = tempfile::tempdir().unwrap();
        let root = idtx_project::ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        for name in ["nested/level.usda", "nested/level.usdc"] {
            let path = super::create_level_file(&root, &root.as_path().join(name), 0.01).unwrap();
            let tag = idtx_usd::read_stage_kind(&path).unwrap();
            assert_eq!(tag.stage_kind, "level");
            assert_eq!(tag.display_name, "level");
        }
    }

    #[cfg(all(feature = "authoring", unix))]
    #[test]
    fn new_level_rejects_an_external_selection_and_an_escaping_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = idtx_project::ProjectRoot::from_canonical(std::fs::canonicalize(dir.path()).unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.as_path().join("escape")).unwrap();
        assert!(super::create_level_file(&root, &root.as_path().join("escape/new.usda"), 1.0).is_err());
        assert!(super::create_level_file(&root, &outside.path().join("new.usda"), 1.0).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn paths_equal_ignores_dot_segments_and_matches_literal_when_missing() {
        use super::paths_equal;
        // A file that exists: `.` segments canonicalise away to the same path.
        let dir = scratch("paths_equal");
        let file = dir.join("main.usda");
        std::fs::write(&file, b"#usda 1.0\n").unwrap();
        let dotted = dir.join(".").join("main.usda");
        assert!(paths_equal(&file, &dotted));
        // Non-existent paths fall back to a literal comparison.
        let missing = dir.join("does-not-exist.usda");
        assert!(paths_equal(&missing, &missing));
        assert!(!paths_equal(&missing, &file));
    }

    /// Unique scratch dir under the system temp dir (no `tempfile` dep) — mirrors
    /// the helper in `idtx-project`'s tests.
    fn scratch(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "idtx-shell-newproj-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scaffold_creates_subfolder_manifest_and_structure() {
        let parent = scratch("ok");
        let project = ApplicationState::scaffold_new_project(&parent, "MyGame").unwrap();

        // Project rooted at <parent>/MyGame with a written manifest.
        assert!(project.root().ends_with("MyGame"));
        assert!(project.manifest_path().is_file());
        assert_eq!(project.name(), "MyGame");
        // Standard folder structure + wired asset root.
        assert!(project.root().join("stages").is_dir());
        assert!(project.root().join("assets").is_dir());
        assert_eq!(
            project.manifest().asset_roots,
            vec![PathBuf::from("assets")]
        );
        // A default input_map.toml accompanies the built-in player, binding the
        // actions its behaviors read (move_forward / look / move / turn).
        let input_map = project.root().join("input_map.toml");
        assert!(input_map.is_file(), "scaffold must write a default input_map.toml");
        let text = std::fs::read_to_string(&input_map).unwrap();
        for action in ["move_forward", "move_right", "look", "move", "turn"] {
            assert!(
                text.contains(&format!("[actions.{action}]")),
                "default input_map.toml missing [actions.{action}]"
            );
        }
        // A default xr_controllers.toml sidecar so an XR cook resolves
        // device-accurate controller meshes (questpro/quest2 + generic fallback).
        let xr_controllers = project.root().join("xr_controllers.toml");
        assert!(
            xr_controllers.is_file(),
            "scaffold must write a default xr_controllers.toml"
        );
        let xr_text = std::fs::read_to_string(&xr_controllers).unwrap();
        for device in ["generic", "questpro", "quest2"] {
            assert!(
                xr_text.contains(&format!("[controllers.{device}]")),
                "default xr_controllers.toml missing [controllers.{device}]"
            );
        }
        // Built-in player by default (no project-local player stage).
        assert!(!project.manifest().has_player_stage());
    }

    #[test]
    fn scaffold_trims_name_and_uses_it_as_the_subfolder() {
        let parent = scratch("trim");
        let project = ApplicationState::scaffold_new_project(&parent, "  Spaced  ").unwrap();
        assert!(project.root().ends_with("Spaced"));
        assert_eq!(project.name(), "Spaced");
    }

    #[test]
    fn scaffold_rejects_empty_name() {
        let parent = scratch("empty");
        let err = ApplicationState::scaffold_new_project(&parent, "   ").unwrap_err();
        assert!(err.contains("name"), "unexpected error: {err}");
    }

    #[test]
    fn scaffold_refuses_to_clobber_an_existing_project() {
        let parent = scratch("dup");
        ApplicationState::scaffold_new_project(&parent, "Dup").unwrap();
        let err = ApplicationState::scaffold_new_project(&parent, "Dup").unwrap_err();
        assert!(err.contains("already exists"), "unexpected error: {err}");
    }

    #[test]
    fn scaffold_rejects_dot_and_dotdot_names() {
        let parent = scratch("dots");
        for name in [".", "..", " .. "] {
            let err = ApplicationState::scaffold_new_project(&parent, name).unwrap_err();
            assert!(
                err.contains("\".\"") || err.contains(".."),
                "expected a dot-name rejection for {name:?}, got: {err}"
            );
        }
        // Nothing must have been written into the parent or its ancestors.
        assert!(!parent.join("project.toml").exists());
        assert!(
            !parent.join("..").join("project.toml").exists(),
            "must never scaffold into the grandparent"
        );
    }

    #[test]
    fn scaffold_rejects_illegal_characters_and_trailing_dot() {
        let parent = scratch("chars");
        for name in ["a/b", "a\\b", "c:d", "q?", "trailing."] {
            assert!(
                ApplicationState::scaffold_new_project(&parent, name).is_err(),
                "expected {name:?} to be rejected"
            );
        }
    }

    #[test]
    fn scaffold_rejects_windows_reserved_names() {
        let parent = scratch("reserved");
        for name in ["NUL", "con", "Com1", "LPT9", "nul.txt"] {
            let err = ApplicationState::scaffold_new_project(&parent, name).unwrap_err();
            assert!(
                err.contains("reserved"),
                "unexpected error for {name:?}: {err}"
            );
        }
    }

    #[test]
    fn scaffold_refuses_to_merge_into_an_existing_non_project_dir() {
        let parent = scratch("occupied");
        // A pre-existing, non-project directory with unrelated content.
        let occupied = parent.join("MyGame");
        std::fs::create_dir_all(&occupied).unwrap();
        std::fs::write(occupied.join("notes.txt"), b"keep me").unwrap();

        let err = ApplicationState::scaffold_new_project(&parent, "MyGame").unwrap_err();
        assert!(err.contains("already exists"), "unexpected error: {err}");
        // The user's file is untouched and no manifest was overlaid.
        assert!(occupied.join("notes.txt").is_file());
        assert!(!occupied.join("project.toml").exists());
    }

    #[test]
    fn project_name_error_accepts_ordinary_names() {
        assert!(ApplicationState::project_name_error("MyGame").is_none());
        assert!(ApplicationState::project_name_error("level-01").is_none());
        assert!(ApplicationState::project_name_error("Scene.v2").is_none());
    }
}
