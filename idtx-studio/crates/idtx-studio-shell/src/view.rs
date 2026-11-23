//! Presentation snapshots handed into the egui closure.
//!
//! [`TabHeader`] and [`ActiveTabData`] are per-frame value snapshots so the UI
//! closure never touches `StudioSession` mutably; [`GizmoTransformData`] carries
//! the pre-fetched transform the gizmo overlay draws from. [`sync_border_color`]
//! maps a remote-sync status to the viewport frame color.

use std::sync::Arc;

use idtx_editor_model::{LayerStackInfo, PrimInfo};
use idtx_editor_ui::layer_stack::LayerStackUiState;
use idtx_editor_ui::scene_tree::{SceneTreeIndex, SceneTreeState, TreeNode};
use idtx_editor_ui::viewport_toolbar::{CameraViewLabel, GizmoSpace, RenderMode};

use crate::model::{CenterTab, InspectorTab};

#[derive(Debug, Clone)]
pub(crate) struct TabHeader {
    pub(crate) id: u64,
    pub(crate) label: String,
    pub(crate) active: bool,
    /// `true` when the stage has unsaved root-layer edits; drives the tab's
    /// unsaved-changes marker. Always `false` in non-authoring (Inspector) builds.
    pub(crate) dirty: bool,
}

#[derive(Debug)]
pub(crate) struct ActiveTabData {
    pub(crate) id: u64,
    pub(crate) texture_id: egui::TextureId,
    pub(crate) viewport_pixels: [u32; 2],
    pub(crate) center_tab: CenterTab,
    pub(crate) render_mode: RenderMode,
    pub(crate) gizmo_space: GizmoSpace,
    /// Current camera view (numpad preset or free-orbit perspective), shown
    /// in the viewport toolbar's camera-view dropdown.
    pub(crate) camera_view: CameraViewLabel,
    /// `true` when the viewport camera currently uses an orthographic
    /// projection; drives the toolbar's projection toggle state.
    pub(crate) camera_ortho: bool,
    pub(crate) metadata_line: String,
    pub(crate) source_path: String,
    /// `true` when the active stage was opened from a remote server; gates the
    /// in-place "Save" affordance (remote stages are Save-As-only).
    pub(crate) is_remote: bool,
    /// `true` when a live collaboration client is attached: the scene-tree
    /// create menu then offers only server-creatable built-in prim types.
    pub(crate) collab_restricted: bool,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) start_tc: f64,
    pub(crate) end_tc: f64,
    pub(crate) tcps: f64,
    /// True when the stage has an animated range.
    pub(crate) has_animation: bool,
    /// True when the animation clock is currently playing.
    pub(crate) playing: bool,
    /// Current time code (for the scrub slider).
    pub(crate) current_tc: f64,
    /// Whether animation loops.
    pub(crate) looping: bool,
    pub(crate) scene_tree_nodes: Arc<Vec<TreeNode>>,
    pub(crate) scene_tree_index: Arc<SceneTreeIndex>,
    pub(crate) scene_tree_state: SceneTreeState,
    pub(crate) inspector_prim_info: Option<PrimInfo>,
    pub(crate) curve_tool: Option<idtx_studio_session::CurveToolState>,
    pub(crate) inspector_properties: Arc<Vec<idtx_usd::PropertyRecord>>,
    pub(crate) inspector_skeleton_joints: Option<Arc<Vec<idtx_usd::SkeletonJointRecord>>>,
    /// Which inspector view the right panel shows (properties / layer stack).
    pub(crate) inspector_tab: InspectorTab,
    /// Root-layer metadata snapshot for the Stage tab (cheap clone, once/frame).
    pub(crate) stage_metadata: idtx_usd::StageMetadata,
    /// The session's *live* shadow mode (diverges from `stage_metadata.shadow_mode`
    /// after an in-session author write — the session owns the live copy).
    pub(crate) stage_shadow_mode: idtx_usd::ShadowMode,
    /// The `/StageBehavior` prim's inspector info for the Stage tab's behavior
    /// section (`None` when no stage behavior is assigned).
    pub(crate) stage_behavior_prim_info: Option<PrimInfo>,
    /// The `/StageBehavior` prim's property records (empty when unassigned).
    pub(crate) stage_behavior_properties: Arc<Vec<idtx_usd::PropertyRecord>>,
    /// The session's *live* stage-behavior run-condition (diverges from
    /// `stage_metadata` after an in-session author write — the session owns it).
    pub(crate) stage_behavior_run_scope: idtx_usd::StageBehaviorRunScope,
    /// The stage's live physics state (`PhysicsScene` presence + gravity),
    /// driving the Stage tab's Physics section. Default (inactive) when
    /// authoring is not compiled in.
    pub(crate) physics_state: idtx_editor_ui::stage_panel::PhysicsPanelState,
    /// Cached layer stack for the Layer Stack tab; `None` until first opened.
    pub(crate) layer_stack_info: Option<Arc<LayerStackInfo>>,
    /// Layer Stack panel selection state (snapshot; changes flow back via action).
    pub(crate) layer_stack_ui: LayerStackUiState,
    /// Pre-fetched gizmo transform data for the selected prim, if any.
    pub(crate) gizmo_data: Option<GizmoTransformData>,
    /// Snapshot of the journal state for the Edit menu.
    #[cfg(feature = "authoring")]
    pub(crate) journal_can_undo: bool,
    #[cfg(feature = "authoring")]
    pub(crate) journal_can_redo: bool,
    #[cfg(feature = "authoring")]
    pub(crate) journal_undo_label: Option<String>,
    #[cfg(feature = "authoring")]
    pub(crate) journal_redo_label: Option<String>,
    /// `true` when a collab client is attached but its background thread has
    /// exited (server-side disconnect, idle timeout, network error).
    #[cfg(feature = "remote")]
    pub(crate) collab_disconnected: bool,
    /// Live remote-sync status, or `None` when the stage has no active sync.
    /// Drives the viewport's 4px sync border (BLUE peer / GREEN host / RED lost).
    #[cfg(feature = "remote")]
    pub(crate) collab_sync_status: Option<idtx_studio_session::CollabSyncStatus>,
}

/// Border color for the viewport's remote-sync frame.
///
/// AMBER while syncing (snapshot not yet received), BLUE when connected to
/// another peer's session, GREEN when hosting a single/multi-edit session, RED
/// when the connection has been lost.
#[cfg(feature = "remote")]
pub(crate) fn sync_border_color(status: idtx_studio_session::CollabSyncStatus) -> egui::Color32 {
    use idtx_studio_session::CollabSyncStatus;
    let th = idtx_editor_ui::theme::current();
    match status {
        CollabSyncStatus::Syncing => th.status_warn,
        CollabSyncStatus::ConnectedToPeer => th.accent,
        CollabSyncStatus::Hosting => th.status_ok,
        CollabSyncStatus::Disconnected => th.status_err,
    }
}

/// Per-frame data fetched from the session before the egui closure so the gizmo
/// can draw without accessing `StudioSession` mutably inside the closure.
#[derive(Debug, Clone)]
pub(crate) struct GizmoTransformData {
    pub(crate) selected_path: String,
    /// USD local matrix in column-major glam convention.
    pub(crate) local_mat: glam::Mat4,
    /// World transform in render world space.
    pub(crate) world_mat_render: glam::Mat4,
    /// Parent world transform in render world space.
    pub(crate) parent_world_mat_render: glam::Mat4,
    /// View-projection matrix (render world space).
    pub(crate) view_proj: glam::Mat4,
    pub(crate) camera_radius: f32,
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "remote")]
    #[test]
    fn sync_border_color_matches_connection_status() {
        use super::sync_border_color;
        use idtx_studio_session::CollabSyncStatus;
        // Each connection state maps to a semantic theme token: warn while
        // syncing, accent for a joined peer, ok for a hosted session, err when
        // lost. Guards the status→token mapping, not specific RGB values.
        let th = idtx_editor_ui::theme::current();
        assert_eq!(
            sync_border_color(CollabSyncStatus::Syncing),
            th.status_warn
        );
        assert_eq!(
            sync_border_color(CollabSyncStatus::ConnectedToPeer),
            th.accent
        );
        assert_eq!(sync_border_color(CollabSyncStatus::Hosting), th.status_ok);
        assert_eq!(
            sync_border_color(CollabSyncStatus::Disconnected),
            th.status_err
        );
    }
}
