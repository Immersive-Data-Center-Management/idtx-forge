//! Shell data model: errors, UI enums, per-tab session state, caches, and the
//! [`ShellAction`] command vocabulary collected during the egui pass.
//!
//! These are UI-neutral data types shared across the shell modules. Feature-gated
//! variants and fields (`authoring`, `remote`) keep their `#[cfg]` guards intact.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};

#[cfg(feature = "authoring")]
use idtx_authoring::{EditJournal, TransformTrs};
use idtx_editor_model::{LayerStackInfo, PrimInfo};
use idtx_editor_ui::compute_canvas::ComputeGraphSource;
use idtx_editor_ui::graph_canvas::GraphCanvasState;
use idtx_editor_ui::layer_stack::LayerStackUiState;
use idtx_editor_ui::material_canvas::{CanvasGraphSource, MaterialCanvasState};
use idtx_editor_ui::scene_tree::{SceneTreeIndex, SceneTreeState, TreeNode};
use idtx_editor_ui::viewport_toolbar::{GizmoSpace, RenderMode};
use idtx_platform_desktop::DesktopGpuError;
use idtx_project::IdtxProject;
use idtx_studio_session::{SessionError, StudioSession, ViewPreset};
use thiserror::Error;

use crate::gizmo::GizmoState;

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("failed to create the desktop event loop: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    #[error("failed to create the desktop window: {0}")]
    Window(#[from] winit::error::OsError),
    #[error(transparent)]
    Gpu(#[from] DesktopGpuError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CenterTab {
    #[default]
    Viewport,
    MaterialCanvas,
    ComputeGraph,
}

/// Which view the right-hand inspector panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InspectorTab {
    /// The per-prim property inspector (all inspector panels).
    #[default]
    Properties,
    /// The stage-wide layer stack visualization.
    LayerStack,
    /// The open stage's root-layer metadata + stage-level authoring (shadow mode).
    Stage,
}

/// Per-tab cache for the Layer Stack panel.
///
/// The layer stack is fetched across the FFI bridge **only** when the user has
/// the Layer Stack tab open, and re-fetched only when the stage topology
/// changes — never on the per-frame path. `topo_gen == u64::MAX` marks an
/// unpopulated cache so the first activation always fetches.
pub(crate) struct LayerStackCache {
    pub(crate) topo_gen: u64,
    pub(crate) info: Option<Arc<LayerStackInfo>>,
}

impl Default for LayerStackCache {
    fn default() -> Self {
        Self {
            topo_gen: u64::MAX,
            info: None,
        }
    }
}

/// Per-project cache for the level browser panel.
///
/// [`idtx_usd::read_stage_kind`] opens each level's root layer to classify it —
/// file I/O we must not repeat every frame. The cache is keyed on the project
/// manifest's content hash so it rebuilds only when the level registry (or any
/// other manifest field) changes; the per-frame `is_open` flag is applied
/// separately when the browser snapshot is built.
#[derive(Default)]
pub(crate) struct LevelRegistryCache {
    /// Manifest content hash the cached rows were built from.
    pub(crate) manifest_hash: String,
    /// Cached per-level data (without the per-frame `is_open` flag).
    pub(crate) rows: Vec<CachedLevelRow>,
}

/// A cached level row — everything but the per-frame `is_open` flag.
pub(crate) struct CachedLevelRow {
    /// Display name: the stage's `displayName` tag, else its file stem.
    pub(crate) display_name: String,
    /// Project-relative path.
    pub(crate) rel_path: String,
    /// Raw stage-kind token (`"level"`/`"asset"`/…/`""`).
    pub(crate) kind_token: String,
    /// Resolved absolute path inside the sandbox, if it resolves.
    pub(crate) abs_path: Option<PathBuf>,
    /// `true` when the registered stage file is missing on disk.
    pub(crate) missing: bool,
}

/// Per-tab cache for the inspector panel.  Refreshed whenever the selected
/// path or change generation changes so `inspect_prim()` is not called
/// every frame.
#[derive(Default)]
pub(crate) struct InspectorCache {
    pub(crate) prim_path: Option<String>,
    pub(crate) topo_gen: u64,
    pub(crate) change_gen: u64,
    pub(crate) prim_info: Option<PrimInfo>,
    pub(crate) properties: Arc<Vec<idtx_usd::PropertyRecord>>,
    pub(crate) skeleton_joints: Option<Arc<Vec<idtx_usd::SkeletonJointRecord>>>,
}

/// Cache of the `/StageBehavior` prim's inspector data, driving the Stage tab's
/// behavior section. Unlike [`InspectorCache`] it is keyed only on the USD
/// change/topology generation (the stage behavior prim path is fixed), so it is
/// refreshed independently of the selection. `key` is `None` until first built.
#[derive(Default)]
pub(crate) struct StageBehaviorCache {
    pub(crate) key: Option<(u64, u64)>,
    pub(crate) prim_info: Option<PrimInfo>,
    pub(crate) properties: Arc<Vec<idtx_usd::PropertyRecord>>,
}

pub(crate) struct SessionTab {
    pub(crate) session: StudioSession,
    /// egui texture ID for the rendered viewport image.
    pub(crate) texture_id: egui::TextureId,
    pub(crate) center_tab: CenterTab,
    pub(crate) render_mode: RenderMode,
    /// Coordinate space for the transform gizmo.
    pub(crate) gizmo_space: GizmoSpace,
    /// Viewport pixels from the previous frame; used to detect resize.
    pub(crate) last_viewport_pixels: [u32; 2],
    pub(crate) scene_tree_state: SceneTreeState,
    /// False until the first frame with topology data, at which point the tree
    /// is collapsed so egui only paints root-level rows by default.
    pub(crate) tree_initialized: bool,
    /// Cached flat prim list; rebuilt only when `session.topology_generation()`
    /// changes, not every frame.
    pub(crate) cached_nodes: Arc<Vec<TreeNode>>,
    /// Pre-built tree index (parent→children map + sorted roots) derived from
    /// `cached_nodes`.  Rebuilt together with `cached_nodes`.
    pub(crate) cached_index: Arc<SceneTreeIndex>,
    /// Value of `session.topology_generation()` when the cache was last built.
    pub(crate) cached_topology_gen: u64,
    /// Value of `session.visibility_generation()` when the cache was last built.
    pub(crate) cached_visibility_gen: u64,
    /// Inspector property cache; refreshed when selection or topology changes.
    pub(crate) inspector_cache: InspectorCache,
    /// Stage-behavior (`/StageBehavior`) inspector cache for the Stage tab.
    pub(crate) stage_behavior_cache: StageBehaviorCache,
    /// Which inspector view (properties / layer stack) the right panel shows.
    pub(crate) inspector_tab: InspectorTab,
    /// Layer Stack panel cache; populated on demand when the tab is opened.
    pub(crate) layer_stack_cache: LayerStackCache,
    /// Layer Stack panel row-selection state (which layer's detail card shows).
    pub(crate) layer_stack_ui: LayerStackUiState,
    /// Material canvas egui state (pan/zoom/selection/layout cache).
    pub(crate) material_canvas: MaterialCanvasState,
    /// Pre-computed graph source passed to `material_canvas_panel`; `None` when
    /// no material-bearing prim is selected.
    pub(crate) canvas_graph_source: Option<Arc<CanvasGraphSource>>,
    /// The `(selected prim path, topology generation)` that last drove
    /// `refresh_canvas_graph_source`. Used as the refresh cache key so the
    /// `material_graph`/`inspect_prim` FFI runs only when the selection or
    /// topology actually changes. The resolved material path can differ from
    /// the selected prim (a bound geometry prim resolves to a material
    /// elsewhere), so `canvas_graph_source.material_path` cannot serve as the
    /// key; likewise a prim with no binding leaves the source `None` but must
    /// still be recorded so it is not re-resolved every frame. Outer `None`
    /// means "not yet computed".
    pub(crate) canvas_source_key: Option<(Option<String>, u64)>,
    /// Compute Graph canvas egui state (pan/zoom/selection/layout cache).
    pub(crate) compute_canvas: GraphCanvasState,
    /// Pre-computed graph source passed to `compute_canvas_panel`; `None` when
    /// the selected prim has no connected compute attribute.
    pub(crate) compute_graph_source: Option<Arc<ComputeGraphSource>>,
    /// The `(selected prim path, topology generation)` that last drove
    /// `refresh_compute_graph_source` — the refresh cache key (see
    /// `canvas_source_key` for the rationale). Outer `None` = not yet computed.
    /// The tuple is `(selected path, topology generation, change generation)`:
    /// the compute canvas renders composed constant values (`jsonPath`,
    /// `jsonValueType`, …) that change on value-only edits without bumping
    /// topology, so it must also invalidate on `change_generation`.
    pub(crate) compute_source_key: Option<(Option<String>, u64, u64)>,
    /// Transform gizmo overlay state: snapshot for hit-testing and active drag.
    pub(crate) gizmo: GizmoState,
    /// Per-tab edit history (undo/redo stack). Only present when the authoring
    /// feature is enabled; cleared on stage reload.
    #[cfg(feature = "authoring")]
    pub(crate) journal: EditJournal,
    /// Last-known collaboration connection state; used to detect drops and
    /// show a one-time status notification.  `false` when no collab client is
    /// attached to this tab.
    #[cfg(feature = "remote")]
    pub(crate) collab_was_connected: bool,
}

/// State carried while the "overwrite?" confirmation dialog is open.
#[cfg(feature = "authoring")]
#[derive(Debug, Clone)]
pub(crate) struct SaveConfirmInfo {
    pub(crate) tab_id: u64,
    /// The current on-disk path that would be overwritten.
    pub(crate) path: PathBuf,
}

/// Why a save-on-close prompt is running: closing a single tab, or exiting the
/// whole app (where several dirty tabs may be resolved one prompt at a time).
#[cfg(feature = "authoring")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseIntent {
    /// Closing one tab (the tab `×` button or Ctrl+W).
    JustTab,
    /// Closing the window — exit the app once the queue drains.
    ExitApp,
}

/// State of an in-progress save-on-close flow. Dirty tabs are resolved from the
/// front of `queue`: each prompt saves or discards the head tab, closes it, and
/// pops it. When `queue` empties, the app exits if `intent == ExitApp`.
#[cfg(feature = "authoring")]
#[derive(Debug, Clone)]
pub(crate) struct PendingClose {
    /// Ids of dirty tabs still awaiting a save/discard decision, head first.
    pub(crate) queue: Vec<u64>,
    /// Whether draining the queue should also exit the app.
    pub(crate) intent: CloseIntent,
    /// `true` while an in-place save for `queue[0]` is deferred in flight
    /// (pushed to `pending_saves`); `do_save` advances the queue on success.
    pub(crate) saving: bool,
}

/// State carried while the scene-tree "Rename…" dialog is open.
#[cfg(feature = "authoring")]
#[derive(Debug, Clone)]
pub(crate) struct RenameDialog {
    /// Tab whose stage owns the prim being renamed.
    pub(crate) tab_id: u64,
    /// Absolute path of the prim being renamed.
    pub(crate) path: String,
    /// Editable buffer, seeded with the current leaf name.
    pub(crate) buffer: String,
}

/// State carried while the scene-tree drop target picker is open. Shown when a
/// dropped USD stage has **no** `defaultPrim` and more than one root `def` prim,
/// so the user must choose which prim the reference/payload arc targets.
#[cfg(feature = "authoring")]
#[derive(Debug, Clone)]
pub(crate) struct DropTargetPickerDialog {
    /// Tab whose stage receives the new arc-bearing child.
    pub(crate) tab_id: u64,
    /// Drop-target prim path; the new child is nested under it.
    pub(crate) parent_prim: String,
    /// Project-relative (`/`-separated) path of the dropped USD stage.
    pub(crate) asset_rel: String,
    /// `true` authors a reference arc, `false` a payload arc.
    pub(crate) as_reference: bool,
    /// Root `def` prim names offered as the arc target.
    pub(crate) candidates: Vec<String>,
}

/// State for the Conductor's startup "open a project" dialog. Held in
/// `Some` on [`crate::app::ApplicationState`] while the modal is visible.
#[derive(Debug, Clone, Default)]
pub(crate) struct StartupProjectDialog {
    /// A project found by walking up from the launch directory, offered as a
    /// one-click open. `None` when the launch dir is not inside a project.
    pub(crate) discovered: Option<DiscoveredProject>,
    /// Transient status/error line (a failed open, or a create error).
    pub(crate) message: Option<String>,
    /// When `Some`, the "New Project" sub-form is active: the dialog collects a
    /// project name and a parent folder before creating the project.
    pub(crate) creating: Option<NewProjectForm>,
}

/// Editable state for the startup dialog's "New Project" sub-form. The project
/// is created at `<parent_dir>/<name>/` once both a valid name and a folder are
/// chosen.
#[derive(Debug, Clone, Default)]
pub(crate) struct NewProjectForm {
    /// Editable project-name buffer; also names the created subfolder.
    pub(crate) name: String,
    /// Parent folder chosen via the native picker. `None` until one is picked.
    pub(crate) parent_dir: Option<PathBuf>,
}

/// A project surfaced by [`IdtxProject::discover`] for the startup dialog.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredProject {
    /// Display name of the discovered project.
    pub(crate) name: String,
    /// Absolute path to its `project.toml`.
    pub(crate) manifest_path: PathBuf,
}

impl StartupProjectDialog {
    /// Build the dialog, running project discovery from the current directory
    /// so a launch from inside a project offers a one-click open.
    pub(crate) fn discovering() -> Self {
        let discovered = std::env::current_dir()
            .ok()
            .and_then(|cwd| IdtxProject::discover(cwd).ok().flatten())
            .map(|project| DiscoveredProject {
                name: project.name(),
                manifest_path: project.manifest_path().to_path_buf(),
            });
        Self {
            discovered,
            message: None,
            creating: None,
        }
    }
}

/// In-flight stage open: the loader thread sends its completed [`StudioSession`]
/// (or an error) through this channel.
pub(crate) struct LoadingTab {
    pub(crate) vp: [u32; 2],
    pub(crate) path_label: String,
    /// Local filesystem path being loaded (`None` for remote-only opens). Used to
    /// detect and suppress duplicate open requests for a stage that is already
    /// in flight, before it has landed in `tabs`.
    pub(crate) source_path: Option<std::path::PathBuf>,
    pub(crate) rx: mpsc::Receiver<Result<StudioSession, SessionError>>,
    /// When `Some`, the stage is being fetched from this remote URL; the loaded
    /// session's [`SessionSource`] is flipped to `Remote` on completion. `None`
    /// for local-file opens.
    pub(crate) remote_url: Option<String>,
    /// Target position in the tab bar once loading completes. `Some(idx)` for a
    /// reload, which reinserts the fresh session at the reloaded tab's original
    /// slot so the tab keeps its place; `None` for a new open, which appends.
    pub(crate) insert_at: Option<usize>,
    /// When `Some`, the session was opened in live-collaboration mode; the
    /// collab client is connected and attached once loading succeeds.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    pub(crate) pending_collab: Option<PendingCollabInfo>,
}

/// Collab session metadata carried through a background open so
/// [`ApplicationState::poll_loading_tabs`] can attach a `CollabClient` once
/// the stage has fully loaded.
#[cfg(all(feature = "remote", feature = "authoring"))]
#[derive(Debug)]
pub(crate) struct PendingCollabInfo {
    pub(crate) session_id: String,
    /// Relative WebSocket path returned by the session server (e.g. `/ws?sid=…`).
    pub(crate) ws_url_path: String,
    /// HTTP base URL of the server (used to derive the full `ws://`/`wss://` URL).
    pub(crate) base_url: String,
    pub(crate) token: String,
    /// When `false` this client is joining an existing session (not creating it).
    /// The server-side session is NOT deleted when the client disconnects.
    pub(crate) owns_session: bool,
}

#[derive(Debug)]
pub(crate) enum ShellAction {
    OpenDialog,
    /// Conductor-only: pick a USD file and import it into the active project's
    /// `assets/` folder (copy `.usdz`, package loose formats), then open it.
    #[cfg(feature = "authoring")]
    ImportDialog,
    /// Abandon the in-flight import (cancel while packaging, or reject the
    /// diagnostics): hide the modal and discard/delete the packaged output.
    #[cfg(feature = "authoring")]
    CancelImport,
    /// Accept an import despite the reported packaging diagnostics: move the
    /// package into `assets/` and open it.
    #[cfg(feature = "authoring")]
    AcceptImportWarnings,
    /// "Import anyway" despite unresolved references: re-package the stage with
    /// the broken references stripped out (sources left untouched), then open it.
    #[cfg(feature = "authoring")]
    ImportStrippingUnresolved,
    /// Dismiss a failed-import message.
    #[cfg(feature = "authoring")]
    DismissImportError,
    /// Open the native project-manifest picker (startup dialog / File menu).
    OpenProjectPicker,
    /// Load a specific `project.toml` — a discovered project or a picker result.
    OpenProjectFile(PathBuf),
    /// Flip the startup dialog into the "New Project" sub-form (collect a name
    /// and a parent folder before creating the project).
    BeginNewProject,
    /// Leave the "New Project" sub-form, returning to the open/choose view.
    CancelNewProject,
    /// Open the native folder picker for the new project's parent directory and
    /// store the choice in the startup dialog's [`NewProjectForm`].
    PickNewProjectFolder,
    /// Create `<parent_dir>/<name>/` with `project.toml` + the standard folder
    /// structure, then activate the new project.
    CreateProject {
        parent_dir: PathBuf,
        name: String,
    },
    /// Create a brand-new USD stage (default `/World` Xform root) via a save
    /// dialog, then open it in a fresh tab. Authoring products only.
    #[cfg(feature = "authoring")]
    NewStage,
    /// Cancel the Conductor startup dialog: with no project chosen there is
    /// nothing to edit, so this quits the application.
    CancelStartup,
    /// Drop a stale recent-project entry (its `project.toml` no longer exists)
    /// from the recent-projects list and persist the pruned list.
    RemoveRecentProject(PathBuf),
    /// Open a previously-opened local stage from the Open Recent… submenu.
    OpenRecent(std::path::PathBuf),
    Reload,
    ActivateTab(u64),
    /// Request to close a tab. In authoring builds this checks first whether the
    /// stage has unsaved root-layer edits — clean stages close immediately, a
    /// dirty stage opens the save-on-close modal. In the read-only Inspector it
    /// closes immediately. The tab `×` button and Ctrl+W both route through this.
    RequestCloseTab(u64),
    /// Save-on-close modal: save the head tab of the close queue, then close it
    /// and advance to the next (or exit the app when the queue drains).
    #[cfg(feature = "authoring")]
    SaveAndClose,
    /// Save-on-close modal: discard the head tab's unsaved edits, close it, and
    /// advance the close queue.
    #[cfg(feature = "authoring")]
    DiscardAndClose,
    /// Save-on-close modal (remote stages with a live collab session only):
    /// commit the head tab's session overrides to the server's origin file
    /// instead of saving a local copy, then close it and advance the queue.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    CommitRemoteAndClose,
    /// Save-on-close modal: abort the whole close request, leaving every tab
    /// open with its unsaved edits intact.
    #[cfg(feature = "authoring")]
    CancelClose,
    SetCenterTab {
        tab_id: u64,
        tab: CenterTab,
    },
    /// Switch the right-hand inspector between Properties and Layer Stack.
    SetInspectorTab {
        tab_id: u64,
        tab: InspectorTab,
    },
    /// Persist the Layer Stack panel's UI state (row selection + collapsed
    /// subtrees) back to the tab.
    SetLayerStackUi {
        tab_id: u64,
        ui_state: LayerStackUiState,
    },
    /// Reload a single layer from disk (`SdfLayer::Reload`) by identifier.
    ReloadLayer {
        tab_id: u64,
        identifier: String,
    },
    /// Switch the stage's edit target to the layer with this identifier.
    #[cfg(feature = "authoring")]
    SetEditTarget {
        tab_id: u64,
        identifier: String,
    },
    /// Mute the layer with this identifier (drop it from composition).
    #[cfg(feature = "authoring")]
    MuteLayer {
        tab_id: u64,
        identifier: String,
    },
    /// Un-mute the layer with this identifier (restore its contribution).
    #[cfg(feature = "authoring")]
    UnmuteLayer {
        tab_id: u64,
        identifier: String,
    },
    /// Reorder a sublayer within its parent's sublayer list.
    #[cfg(feature = "authoring")]
    MoveSublayer {
        tab_id: u64,
        identifier: String,
        parent_identifier: String,
        stronger: bool,
    },
    SetRenderMode {
        tab_id: u64,
        mode: RenderMode,
    },
    SetGizmoSpace {
        tab_id: u64,
        space: GizmoSpace,
    },
    OrbitCamera {
        delta_x: f32,
        delta_y: f32,
    },
    PanCamera {
        delta_x: f32,
        delta_y: f32,
    },
    DollyCamera {
        delta: f32,
    },
    /// Pinch-to-zoom: pass the multiplicative factor directly to `dolly_camera`.
    DollyFactor {
        factor: f32,
    },
    FrameAll,
    /// Snap the camera to a canonical orthographic named view (Blender numpad).
    SetViewPreset {
        preset: ViewPreset,
    },
    /// Toggle between perspective and orthographic projection.
    ToggleProjection,
    ResizeViewport {
        pixels: [u32; 2],
    },
    /// LMB click in the 3-D viewport: unproject and pick.
    PickViewport {
        pointer: egui::Pos2,
        rect: egui::Rect,
    },
    /// Launch the active stage in the out-of-process `idtx-runtime` player.
    LaunchRuntimePreview,
    /// Launch the active stage out-of-process in desktop (no-HMD) mode.
    LaunchRuntimePreviewDesktop,
    /// Stop the running runtime-preview process for the active stage.
    StopRuntimePreview,
    /// Cook + package the active project for the given target, off the UI thread.
    /// `deploy` additionally installs + launches on a connected device (Vision Pro
    /// via `devicectl`); `false` builds the signed bundle only.
    #[cfg(feature = "build-tools")]
    BuildExperience {
        target: idtx_editor_model::BuildTarget,
        deploy: bool,
    },
    /// Variant selection authored by the inspector panel.
    SetVariantSelection {
        tab_id: u64,
        path: String,
        set_name: String,
        before: String,
        selection: String,
    },
    /// Generic attribute value authored by the inspector panel.
    #[cfg(feature = "authoring")]
    AuthorAttribute {
        tab_id: u64,
        path: String,
        property: String,
        before: Option<idtx_usd::PropertyValue>,
        value: idtx_usd::PropertyValue,
    },
    /// Inspector attribute edit confirmed (field focus lost / drag end):
    /// record the before→after value in the journal as a single entry.
    #[cfg(feature = "authoring")]
    CommitAttributeEdit {
        tab_id: u64,
        path: String,
        property: String,
        before: Option<idtx_usd::PropertyValue>,
        value: idtx_usd::PropertyValue,
    },
    /// Stage-level shadow mode authored by the Stage tab (`idtx:shadowMode`
    /// token). `before` is the live mode captured at dispatch so undo can
    /// restore it symmetrically.
    #[cfg(feature = "authoring")]
    SetShadowMode {
        tab_id: u64,
        before: idtx_usd::ShadowMode,
        mode: idtx_usd::ShadowMode,
    },
    /// Activate runtime physics from the Stage tab: author a `PhysicsScene` prim
    /// with gravity defaults as a child of the stage pseudo-root. Journaled as a
    /// root-layer snapshot; undo removes it.
    #[cfg(feature = "authoring")]
    ActivatePhysics { tab_id: u64 },
    /// Deactivate runtime physics from the Stage tab: delete the `PhysicsScene`
    /// prim. Journaled as a root-layer snapshot; undo restores it.
    #[cfg(feature = "authoring")]
    DeactivatePhysics { tab_id: u64 },
    /// Edit `physics:gravityDirection` on the `PhysicsScene` prim. `commit`
    /// distinguishes a live drag frame (authored, not journaled) from the
    /// confirmed gesture (authored + one before→after journal entry).
    #[cfg(feature = "authoring")]
    SetGravityDirection {
        tab_id: u64,
        before: Option<[f32; 3]>,
        value: [f32; 3],
        commit: bool,
    },
    /// Edit `physics:gravityMagnitude` on the `PhysicsScene` prim — same
    /// `commit` / `before` semantics as [`Self::SetGravityDirection`].
    #[cfg(feature = "authoring")]
    SetGravityMagnitude {
        tab_id: u64,
        before: Option<f32>,
        value: f32,
        commit: bool,
    },
    /// Apply a single-apply API schema to a prim (light inspector's "Add Shadow
    /// Casting"). `schema` is the short USD alias (`"ShadowAPI"`); undo removes it.
    #[cfg(feature = "authoring")]
    ApplyApiSchema {
        tab_id: u64,
        path: String,
        schema: String,
    },
    /// Remove an API schema from a prim (light inspector's "Remove Shadow
    /// Casting"). `schema` is the short USD alias (`"ShadowAPI"`); undo adds it.
    #[cfg(feature = "authoring")]
    RemoveApiSchema {
        tab_id: u64,
        path: String,
        schema: String,
    },
    /// Apply the collider schemas to a prim (collision inspector's "Add
    /// Collision"). Journaled as a root-layer snapshot; undo removes the applied
    /// schemas and the opinions they author together.
    #[cfg(feature = "authoring")]
    AddCollisionApi {
        tab_id: u64,
        path: String,
    },
    /// Remove the collider schemas from a prim (collision inspector's "Remove
    /// Collision") and clear their authored opinions (channel + the pre-baked
    /// hull arrays). Journaled as a root-layer snapshot; undo restores them.
    #[cfg(feature = "authoring")]
    RemoveCollisionApi {
        tab_id: u64,
        path: String,
    },
    /// Generate a collision convex hull for a prim (collision inspector's
    /// "Generate CollisionHull"). The shell gathers the prim's mesh geometry,
    /// computes the hull off-thread under `params`, and authors the result.
    #[cfg(feature = "authoring")]
    GenerateCollisionHull {
        tab_id: u64,
        path: String,
        params: idtx_editor_model::HullGenParams,
    },
    /// Remove the root-layer opinion of a single property ("Remove Opinion" in
    /// the attribute opinion stack). The shell snapshots the opinion for undo,
    /// clears it, and journals a reversible edit.
    #[cfg(feature = "authoring")]
    RemovePropertyOpinion {
        tab_id: u64,
        path: String,
        property: String,
    },
    /// Reset a prim's transform by removing every root-layer transform opinion
    /// (`properties` = its root-authored `xformOp:*` / `xformOpOrder`). The
    /// shell snapshots each for undo, clears them all, and journals one edit.
    #[cfg(feature = "authoring")]
    ResetTransform {
        tab_id: u64,
        path: String,
        properties: Vec<String>,
    },
    /// Transform edit authored by the inspector panel.
    #[cfg(feature = "authoring")]
    AuthorTransform {
        tab_id: u64,
        path: String,
        translation: [f64; 3],
        rotation_xyz_degrees: [f64; 3],
        scale: [f64; 3],
    },
    /// Gizmo drag finished: record the final before→after transform in the journal.
    #[cfg(feature = "authoring")]
    CommitGizmoDrag {
        tab_id: u64,
        path: String,
        before: glam::Mat4,
        after: glam::Mat4,
    },
    /// Inspector transform edit confirmed (field focus lost / Enter / drag end):
    /// record the before→after transform in the journal as a single entry.
    #[cfg(feature = "authoring")]
    CommitTransformEdit {
        tab_id: u64,
        path: String,
        /// Pre-edit local matrix (USD row-major), captured when editing began.
        before: [[f32; 4]; 4],
        /// Confirmed TRS (re-authored idempotently before journaling).
        translation: [f64; 3],
        rotation_xyz_degrees: [f64; 3],
        scale: [f64; 3],
        /// Pre-edit authored Euler TRS, `Some` only for `rotateXYZ`-op prims.
        /// Carried into the journal so undo/redo restores the exact authored
        /// angles instead of re-decomposing the matrix (which folds a middle-axis
        /// rotation like `Y = 180°` back onto the other axes).
        before_trs: Option<TransformTrs>,
    },
    /// Undo the most recent journal entry for the active tab.
    #[cfg(feature = "authoring")]
    Undo {
        tab_id: u64,
    },
    /// Redo the most recently undone journal entry for the active tab.
    #[cfg(feature = "authoring")]
    Redo {
        tab_id: u64,
    },
    /// Save the active tab's stage to its current source path (shows overwrite confirmation).
    #[cfg(feature = "authoring")]
    Save {
        tab_id: u64,
    },
    /// Open a "Save As" file dialog to save the active tab's stage to a new path.
    #[cfg(feature = "authoring")]
    SaveAs {
        tab_id: u64,
    },
    /// Internal: execute the actual save/export to `path` (after user confirmed).
    #[cfg(feature = "authoring")]
    DoSave {
        tab_id: u64,
        path: PathBuf,
    },
    /// Dismiss the overwrite-confirmation dialog without saving.
    #[cfg(feature = "authoring")]
    DismissSaveConfirm,
    /// Commit the scene-tree rename dialog: rename `path`'s prim to `new_name`.
    #[cfg(feature = "authoring")]
    CommitRename {
        tab_id: u64,
        path: String,
        new_name: String,
    },
    /// Dismiss the rename dialog without renaming.
    #[cfg(feature = "authoring")]
    DismissRenameDialog,
    /// Commit the scene-tree drop target picker: author the pending reference/
    /// payload child arc targeting the chosen root prim (`prim_name`).
    #[cfg(feature = "authoring")]
    CommitDropTarget {
        tab_id: u64,
        prim_name: String,
    },
    /// Dismiss the drop target picker without authoring an arc.
    #[cfg(feature = "authoring")]
    DismissDropTargetPicker,
    /// Open a file dialog, pick a Rust `.rs` behavior source, copy it into the
    /// project's `behaviors/` folder, and author the `IdtxBehaviorAPI` manifest
    /// scaffold (schema + source/uuid/kind + name + version) onto `path`.
    #[cfg(feature = "cooking")]
    AttachBehavior {
        tab_id: u64,
        path: String,
    },
    /// Create a new source-authored behavior for `path`: write a template `.rs`
    /// into the project's `behaviors/` folder + author the source triple, then
    /// kick a non-blocking cargo check. The `.wasm` is built at preview/package.
    /// Needs the behavior-build toolchain (`behavior_tools`), so this is
    /// `cooking`-gated, not merely `authoring`.
    #[cfg(feature = "cooking")]
    CreateBehavior {
        tab_id: u64,
        path: String,
    },
    /// Overwrite the behavior on `path` into the current edit layer: carry its
    /// composed manifest config over, then create a fresh source + new uuid
    /// (re-applying `IdtxBehaviorAPI` into the `over`). Offered when a behavior
    /// lives in a weaker layer or the `over` detached the API. Uses
    /// `behavior_tools`, so `cooking`-gated like `CreateBehavior`.
    #[cfg(feature = "cooking")]
    OverwriteBehavior {
        tab_id: u64,
        path: String,
    },
    /// Detach the behavior attachment from `path`.
    #[cfg(feature = "authoring")]
    DetachBehavior {
        tab_id: u64,
        path: String,
    },
    /// Create a new source-authored **stage** behavior: ensure the
    /// `/StageBehavior` prim + root-layer `idtx:stageBehavior` metadata, scaffold
    /// a `.rs` source, and set `idtx:behavior:scope = "stage"`. Uses
    /// `behavior_tools`, so `cooking`-gated like [`Self::CreateBehavior`].
    #[cfg(feature = "cooking")]
    CreateStageBehavior {
        tab_id: u64,
    },
    /// Attach an existing `.rs` source as the stage behavior (same preamble as
    /// [`Self::CreateStageBehavior`], via a file picker).
    #[cfg(feature = "cooking")]
    AttachStageBehavior {
        tab_id: u64,
    },
    /// Author a new run-condition token for the stage behavior
    /// (`customLayerData["idtx:stageBehaviorRunScope"]`).
    #[cfg(feature = "authoring")]
    SetStageBehaviorRunScope {
        tab_id: u64,
        scope: idtx_usd::StageBehaviorRunScope,
    },
    /// Open the behavior's Rust source for `path` in VS Code. Uses
    /// `behavior_tools`, so `cooking`-gated.
    #[cfg(feature = "cooking")]
    EditBehaviorSource {
        tab_id: u64,
        path: String,
    },
    /// Compile-check (`cargo check --target wasm32-…`) the behavior on `path`
    /// Uses `behavior_tools`, so `cooking`-gated.
    #[cfg(feature = "cooking")]
    CheckBehavior {
        tab_id: u64,
        path: String,
    },
    /// Build + link-verify the behavior on `path` against its authored
    /// capabilities, reporting required / missing / unused buckets. Uses
    /// `behavior_tools`, so `cooking`-gated.
    #[cfg(feature = "cooking")]
    VerifyBehavior {
        tab_id: u64,
        path: String,
    },
    /// Enable/disable the behavior attachment on `path` without detaching.
    #[cfg(feature = "authoring")]
    SetBehaviorEnabled {
        tab_id: u64,
        path: String,
        enabled: bool,
    },
    /// Set the behavior execution kind on `path`. `kind` is a token —
    /// `"runtime"` or `"editor"`.
    #[cfg(feature = "authoring")]
    SetBehaviorKind {
        tab_id: u64,
        path: String,
        kind: String,
    },
    /// Set the behavior scope on `path`. `scope` is a token — `"entity"` or
    /// `"stage"`.
    #[cfg(feature = "authoring")]
    SetBehaviorScope {
        tab_id: u64,
        path: String,
        scope: String,
    },
    /// Set whether the behavior on `path` is server-authoritative.
    #[cfg(feature = "authoring")]
    SetBehaviorServerAuthoritative {
        tab_id: u64,
        path: String,
        value: bool,
    },
    /// Author an arbitrary `IdtxBehaviorAPI` config attribute on `path` — the
    /// generic channel behind the inspector's behavior-config controls
    /// (capabilities / inputs token arrays, version, ship, memory cap). Routed
    /// through the behavior-sink path (session-layer, no root-layer opinion
    /// required), journaled as a structural snapshot.
    #[cfg(feature = "authoring")]
    SetBehaviorProperty {
        tab_id: u64,
        path: String,
        property: String,
        value: idtx_usd::PropertyValue,
    },
    /// Open the general app configuration dialog.
    OpenConfigureDialog,
    /// Open the "Project ▸ Settings…" modal, seeded from the active project.
    OpenProjectSettings,
    /// Apply the Project Settings dialog to the manifest and persist it. Blocked
    /// (dialog kept open with inline errors) when a mandatory field is missing.
    SaveProjectSettings,
    /// Dismiss the Project Settings dialog, discarding its edits.
    CloseProjectSettings,
    /// Run a native picker for a Project Settings path field, writing the result
    /// back into the live dialog state.
    PickProjectSettingsPath {
        field: idtx_editor_ui::project_settings::PathField,
    },
    /// Save the server configuration table + store each password in the
    /// keychain.
    #[cfg(feature = "remote")]
    SaveServerConfigs {
        servers: Vec<idtx_editor_ui::remote_browser::ServerConfigDraft>,
    },
    /// Test the connection for a configuration row (health-check + login). The
    /// row is identified by its url + username so the async result lands on the
    /// right row even if the table is edited meanwhile.
    #[cfg(feature = "remote")]
    TestServerConnection {
        url: String,
        username: String,
        password: String,
    },
    /// Open the remote file browser window, targeting configured server `server`.
    #[cfg(feature = "remote")]
    OpenRemoteDialog {
        server: usize,
    },
    /// User picked a remote USD file to open.
    #[cfg(feature = "remote")]
    OpenRemoteStage(String),
    /// Open the remote file browser for a live-collaboration session (authoring
    /// only), targeting configured server `server`.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    OpenRemoteWithSyncDialog {
        server: usize,
        mode: idtx_collab::session::SessionMode,
    },
    /// User picked a remote USD file to open with live sync.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    OpenRemoteSyncStage {
        base_url: String,
        filepath: String,
        mode: idtx_collab::session::SessionMode,
    },
    /// Open the session browser to join an existing collaboration session on
    /// configured server `server`.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    OpenSessionBrowserDialog {
        server: usize,
    },
    /// User selected an existing session to join (Connect Remote with Sync).
    #[cfg(all(feature = "remote", feature = "authoring"))]
    JoinRemoteSession {
        base_url: String,
        session_id: String,
        usd_file: String,
        ws_url_path: String,
    },
    /// Reconnect the active tab's collaboration WebSocket after a disconnect.
    #[cfg(feature = "remote")]
    ReconnectCollab {
        tab_id: u64,
    },
    #[cfg(all(feature = "remote", feature = "authoring"))]
    ExportCollabRecovery { tab_id: u64 },
    /// Commit the active collab session's overrides to the server's origin file.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    CommitRemote {
        tab_id: u64,
    },
    /// User picked a server asset in the "Create Child → Asset…" picker: ask the
    /// sync server to author a typeless payload-reference child under
    /// `parent_path` pointing at `reference_path` (already made relative to the
    /// current stage). The prim is authored locally only when the server
    /// broadcast arrives, exactly like the built-in remote create path.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    CreateRemoteAssetChild {
        tab_id: u64,
        parent_path: String,
        reference_path: String,
    },
    /// Open (or refresh) the remote file browser as the "Create Child → Asset…"
    /// picker. Distinct from [`OpenRemoteDialog`](ShellAction::OpenRemoteDialog)
    /// so it does NOT clear `asset_create_target` — the plain Open-Remote opens
    /// clear it, keeping a stale picker context from misrouting a later plain
    /// open into a reference-create.
    #[cfg(all(feature = "remote", feature = "authoring"))]
    OpenAssetPickerDialog,
    /// Animation transport command (play/pause/stop/scrub).
    AnimPlayback {
        tab_id: u64,
        req: idtx_studio_session::PlaybackRequest,
    },
    /// Open the registry level at this index in a tab (project level browser).
    OpenLevel {
        index: usize,
    },
    /// Remove the registry level at this index (keeps the stage file on disk).
    RemoveLevel {
        index: usize,
    },
    /// Reorder a registry level from one slot to another.
    MoveLevel {
        from: usize,
        to: usize,
    },
    /// Create a brand-new level stage via a save dialog, tag it `StageKind::Level`,
    /// register it in the project, and open it. Authoring products only.
    #[cfg(feature = "authoring")]
    NewLevel,
    /// Register an existing in-project stage as a level via an open dialog.
    AddExistingLevel,
    /// Author a new standalone player stage (`stages/player.usda`) into the
    /// active project — a `def Xform "Player"` that references the toolbox's
    /// built-in player — and record it as the project's `player_stage`. Placed
    /// at a canonical project location (like [`NewLevel`](ShellAction::NewLevel)),
    /// so no save dialog is needed. Authoring products only.
    #[cfg(feature = "authoring")]
    CreatePlayerStage,
    /// Navigate the asset browser to a project-relative subdir (`""` = root).
    SetAssetDir(String),
    /// Open a project-relative USD stage from the asset browser in a tab
    /// (double-click). Reuses the open tab when the stage is already loaded.
    OpenAssetStage {
        asset_rel_path: String,
    },
    /// Author a reference to a project-relative asset onto the active tab's
    /// currently-selected prim (asset browser). Authoring products only.
    #[cfg(feature = "authoring")]
    AddReferenceToSelected {
        asset_rel_path: String,
    },
    /// Author a payload of a project-relative asset onto the active tab's
    /// currently-selected prim (asset browser). Authoring products only.
    #[cfg(feature = "authoring")]
    AddPayloadToSelected {
        asset_rel_path: String,
    },
}
