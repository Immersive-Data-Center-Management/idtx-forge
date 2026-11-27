/// Scene tree panel — renders the USD prim hierarchy as a collapsible tree.
///
/// The pseudo-root prim (`/`) is never shown; its children become the top-level
/// nodes. Prims with a payload/reference arc (packed stages) show an inline
/// indicator and a toggle button that reveals/hides the composed subtree dimmed
/// as read-only.
///
/// Schema icons are drawn procedurally via egui's painter API so they can be
/// swapped for image/SVG assets later by replacing `draw_schema_icon`.
use std::collections::HashMap;
use std::collections::HashSet;

use egui::{Color32, Id, ImageSource, Layout, Ui};
use idtx_usd::{PurposeFilter, RenderPurpose};
use num_traits::ToPrimitive;

use crate::asset_browser::AssetDragPayload;
use crate::icons;

// ── Constants ─────────────────────────────────────────────────────────────────

const ICON_SIZE: f32 = 14.0;
const INDENT_PX: f32 = 14.0;
const SCROLLBAR_PAD: f32 = 16.0;

/// Label color for prims authored as `over` in the open root layer.
fn root_over_color() -> Color32 {
    crate::theme::current().accent_warm
}

// ── Schema icon type ──────────────────────────────────────────────────────────

/// Classification used to select which procedural icon to draw.
/// Variants are intentionally coarse — map many USD type names to a handful of
/// visual shapes. Replace `draw_schema_icon` with SVG/image dispatch when
/// assets are available.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SchemaIcon {
    Prim,
    Mesh,
    Camera,
    Light,
    Material,
    Shader,
}

#[must_use]
pub fn classify_schema_icon(type_name: &str) -> ImageSource<'static> {
    match type_name {
        "Cube" => icons::CUBE,
        "Capsule" => icons::CAPSULE,
        "Cone" => icons::CONE,
        "Cylinder" => icons::CYLINDER,
        "Sphere" => icons::SPHERE,
        "Plane" => icons::GRID,
        "Mesh" | "BasisCurves" | "Points" | "NurbsPatch" => icons::GRID,
        "Xform" => icons::XFORM,
        "Camera" => icons::CAMERA,
        "DomeLight" => icons::ENVIRONMENT,
        "DistantLight" | "SphereLight" | "RectLight" | "DiskLight" | "CylinderLight"
        | "PortalLight" | "GeometryLight" => icons::LIGHT,
        "Material" => icons::MATERIAL,
        "Shader" | "NodeGraph" => icons::SHADER,
        "Scope" => icons::SCOPE,
        "Skeleton" => icons::SKELETON,
        "SkelRoot" => icons::SKEL_ROOT,
        _ => icons::WIREFRAME,
    }
}

/// True when `type_name` names a concrete USD **gprim** (a schema deriving
/// from `UsdGeomGprim`): the intrinsic geometry primitives and the point-based
/// prims (meshes, curves, points).
///
/// USD forbids nesting gprims in namespace. Core features such as
/// activation and visibility apply hierarchically and are *pruning*: once an
/// ancestor gprim is deactivated or made invisible there is no way to make any
/// descendant active or visible again, so a child under a gprim can never be
/// independently addressed. `usdchecker` warns on such scenes. The scene tree
/// therefore refuses to author any child under a gprim (see `draw_node`).
///
/// The list mirrors the concrete `IsA<UsdGeom…>` gprim checks in the C++ shim
/// (`project_viewport` in `usd_shim.cpp`) plus the point-based and intrinsic
/// gprims not drawn in the viewport. The `_1` suffixed names are the typed
/// revisions introduced in recent USD releases.
#[must_use]
pub fn is_gprim(type_name: &str) -> bool {
    matches!(
        type_name,
        // Intrinsic geometry gprims
        "Cube"
            | "Cube_1"
            | "Sphere"
            | "Sphere_1"
            | "Cone"
            | "Cone_1"
            | "Cylinder"
            | "Cylinder_1"
            | "Capsule"
            | "Capsule_1"
            | "Plane"
            // Point-based gprims
            | "Mesh"
            | "Points"
            | "BasisCurves"
            | "NurbsCurves"
            | "NurbsPatch"
            | "HermiteCurves"
    )
}

// ── Prim render classification ──────────────────────────────────────────────

/// A prim is display-only ("referenced") when it lives inside a composed
/// (reference/payload) boundary and carries no spec in the open root layer.
///
/// This depends on the prim's **own** authorship and whether it sits inside a
/// packed boundary — never on how its parent happened to be drawn. Authoring an
/// `over` ("Create Opinion") on one prim flips only *that* prim (and the
/// ancestor chain `SdfCreatePrimInLayer` also authors) to locally-authored;
/// unauthored siblings must stay referenced so their Create Opinion item (and
/// grey label) survive.
#[must_use]
fn prim_is_referenced(inside_packed: bool, is_locally_authored: bool) -> bool {
    inside_packed && !is_locally_authored
}

/// Whether composed (unauthored) children are currently revealed at a node's
/// level. A packed-arc node gates its interior with its own reveal toggle;
/// otherwise the state is inherited from the enclosing boundary, so an authored
/// `over` chain (which strips the packed arc off ancestors) still flows the
/// enclosing boundary's reveal state through to deeper composed prims.
#[must_use]
fn composed_children_revealed(
    has_packed_arc: bool,
    is_packed_revealed: bool,
    parent_composed_revealed: bool,
) -> bool {
    if has_packed_arc {
        is_packed_revealed
    } else {
        parent_composed_revealed
    }
}

/// Whether a child row must be hidden from the tree.
///
/// Locally-authored overs/defs always show. A composed (unauthored) child is
/// hidden only while it sits **inside a packed boundary** (reference/payload)
/// that has not been revealed — that boundary owns the unveil toggle which
/// un-hides its interior.
///
/// A composed child that is *not* inside any packed boundary has no toggle that
/// could ever reveal it, so it must show outright. This covers two cases that
/// composition produces under an ordinary (non-packed) parent via a selected
/// variant:
/// - the variant brings in a plain prim subtree (no arc) — it simply appears;
/// - the variant brings in a prim that *is* a packed boundary (e.g. `PlayerRig`
///   referencing a rig stage) — it appears and carries its own unveil toggle,
///   with its referenced interior hidden until that toggle is used.
///
/// `child_inside_packed` is the parent's `inside_packed || has_packed_arc`: once
/// inside a boundary it stays set for the whole subtree, so a nested packed
/// boundary under an already-packed parent is correctly gated by the enclosing
/// toggle rather than slipping out on its own.
#[must_use]
fn composed_child_hidden(
    child_is_locally_authored: bool,
    child_composed_revealed: bool,
    child_inside_packed: bool,
) -> bool {
    !child_is_locally_authored && !child_composed_revealed && child_inside_packed
}

// ── Tree node data ────────────────────────────────────────────────────────────

/// Pre-processed prim snapshot passed to `scene_tree_panel`.
///
/// Built once when topology changes; all derived fields are computed at
/// construction time so the per-frame draw path does no string allocations.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct TreeNode {
    /// Full USD path, e.g. `/World/Mesh`.
    pub path: String,
    /// Resolved path of parent prim, or `None` for top-level prims.
    pub parent_path: Option<String>,
    pub name: String,
    pub type_name: String,
    /// True when this prim has a reference or payload arc (packed stage boundary).
    pub has_packed_arc: bool,
    /// True when the composed prim has IdtxBehaviorAPI applied.
    pub has_behavior: bool,
    /// True when this prim has a local spec in the root stage layer.
    /// Used to split children of packed prims into editable vs. composed-only.
    pub is_locally_authored: bool,
    /// Deterministic hash of `path`, pre-computed at construction time.
    /// Used as egui widget IDs and for `SceneTreeState` set lookups.
    pub path_hash: u64,
    /// Display label: `"name  [TypeName]"` or just `"name"` when `type_name`
    /// is empty.  Pre-computed to avoid per-frame `format!` allocations.
    pub display_label: String,
    /// Current computed visibility from the ECS world.  Updated whenever the
    /// visibility generation changes, not just on topology changes.
    pub visible: bool,
    /// Actual purpose computed by USD, including imageable inheritance. Updated
    /// with visibility so authored purpose changes also refresh the row tint.
    pub purpose: RenderPurpose,
    /// True when this prim's spec in the open root layer uses the `over`
    /// specifier (as opposed to `def`/`class`, or having no root-layer spec).
    /// Such prims are drawn with an orange label to flag that the root layer
    /// only overrides an existing prim rather than defining it.
    pub is_root_over: bool,
}

// ── Stable ID helper ──────────────────────────────────────────────────────────

/// Deterministic hash of a USD prim path for use as an egui ID.
/// Mirrors `idtx_scene::stable_prim_hash` — same algorithm, same values.
#[must_use]
pub fn stable_prim_hash(path: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash as _, Hasher as _};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

// ── Scene tree index ──────────────────────────────────────────────────────────

/// Pre-built tree index derived from a `Vec<TreeNode>`.
///
/// Computed once after topology changes and reused every frame.  Holds the
/// parent→children mapping and sorted root list so `scene_tree_panel` does no
/// allocations or sorting on the hot path.
#[derive(Clone, Debug, Default)]
pub struct SceneTreeIndex {
    /// Indices into the flat node slice for root-level prims, sorted by path.
    pub roots: Vec<usize>,
    /// Maps parent path → sorted child indices.
    children_of: HashMap<String, Vec<usize>>,
}

impl SceneTreeIndex {
    /// Build the index from a flat node slice.  O(N log N) due to sorting;
    /// called only when topology changes, not every frame.
    #[must_use]
    pub fn build(nodes: &[TreeNode]) -> Self {
        let mut children_of: HashMap<String, Vec<usize>> = HashMap::new();
        let mut roots: Vec<usize> = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            match node.parent_path.as_deref() {
                None | Some("/") => roots.push(i),
                Some(p) => children_of.entry(p.to_owned()).or_default().push(i),
            }
        }
        roots.sort_by(|&a, &b| nodes[a].path.cmp(&nodes[b].path));
        for children in children_of.values_mut() {
            children.sort_by(|&a, &b| nodes[a].path.cmp(&nodes[b].path));
        }
        Self { roots, children_of }
    }

    /// Returns the sorted child indices for `path`, or an empty slice.
    pub fn children(&self, path: &str) -> &[usize] {
        self.children_of.get(path).map_or(&[], Vec::as_slice)
    }
}

// ── Scene tree state ──────────────────────────────────────────────────────────

/// Per-session UI state for the scene tree panel.
#[derive(Clone, Debug, Default)]
pub struct SceneTreeState {
    /// Colors excluded prims without removing them or pruning their children.
    pub purpose_filter: PurposeFilter,
    /// Set of path hashes whose subtree is collapsed (default: expanded).
    pub collapsed: HashSet<u64>,
    /// Set of path hashes whose composed (referenced) subtree is revealed.
    pub packed_revealed: HashSet<u64>,
    /// Selected prim path, if any.
    pub selected: Option<String>,
    /// When `true`, the next rendered frame will call `scroll_to_me` on the
    /// selected row so it enters the visible viewport of the scroll area.
    pub scroll_to_selected: bool,
}

impl SceneTreeState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_collapsed(&self, hash: u64) -> bool {
        self.collapsed.contains(&hash)
    }

    #[must_use]
    pub fn is_packed_revealed(&self, hash: u64) -> bool {
        self.packed_revealed.contains(&hash)
    }

    pub fn toggle_collapsed(&mut self, hash: u64) {
        if !self.collapsed.remove(&hash) {
            self.collapsed.insert(hash);
        }
    }

    pub fn toggle_packed(&mut self, hash: u64) {
        if !self.packed_revealed.remove(&hash) {
            self.packed_revealed.insert(hash);
            // Auto-expand the node so the revealed children are immediately visible.
            self.collapsed.remove(&hash);
        }
    }

    /// Expand all ancestors, revealing composed contents at every package boundary.
    pub fn reveal_ancestors(&mut self, path: &str, nodes: &[TreeNode]) {
        let Some(node) = nodes.iter().find(|n| n.path == path) else {
            return;
        };
        if let Some(parent) = &node.parent_path
            && parent != "/"
        {
            let hash = stable_prim_hash(parent);
            self.collapsed.remove(&hash);
            if nodes.iter().any(|n| n.path == *parent && n.has_packed_arc) {
                self.packed_revealed.insert(hash);
            }
            self.reveal_ancestors(parent, nodes);
        }
    }

    /// Collapse all nodes. Called once after a stage finishes loading so the
    /// tree starts fully collapsed and egui only paints the root-level rows.
    pub fn collapse_all(&mut self, nodes: &[TreeNode]) {
        self.collapsed.clear();
        for node in nodes {
            self.collapsed.insert(node.path_hash);
        }
    }

    /// Apply a batch of actions produced by `scene_tree_panel`.
    pub fn apply(&mut self, actions: Vec<SceneTreeAction>, nodes: &[TreeNode]) {
        for action in actions {
            match action {
                SceneTreeAction::ToggleCollapsed(hash) => self.toggle_collapsed(hash),
                SceneTreeAction::TogglePacked(hash) => self.toggle_packed(hash),
                SceneTreeAction::Select(path) => {
                    if let Some(ref p) = path {
                        self.reveal_ancestors(p, nodes);
                    }
                    self.scroll_to_selected = path.is_some();
                    self.selected = path;
                }
                SceneTreeAction::ToggleVisibility(_path) => {
                    // Handled by the caller (shell) which authors the visibility
                    // on the session layer; nothing for state to do here.
                }
                // Viewport-only actions are handled by the shell.
                SceneTreeAction::FrameSelected(_) | SceneTreeAction::UseCamera(_) => {}
                // Authored by the shell (session write + undo record); no
                // tree-state mutation required.
                SceneTreeAction::CreateOpinion(_)
                | SceneTreeAction::RemoveOpinion(_)
                // Structural authoring actions are serviced by the shell against
                // the session; they carry no scene-tree UI state to update here.
                | SceneTreeAction::DefineChild { .. }
                | SceneTreeAction::CreateAssetChild { .. }
                | SceneTreeAction::DefineChildFromTemplate { .. }
                | SceneTreeAction::DeletePrim(_)
                | SceneTreeAction::RequestRename(_)
                | SceneTreeAction::AddReference(_)
                | SceneTreeAction::AddPayload(_)
                | SceneTreeAction::AttachBehavior(_)
                | SceneTreeAction::DropAssetReference { .. }
                | SceneTreeAction::DropAssetPayload { .. } => {}
                SceneTreeAction::AckScroll => {
                    self.scroll_to_selected = false;
                }
            }
        }
    }
}

// ── Scene tree actions ────────────────────────────────────────────────────────

/// Mutations collected by `scene_tree_panel` and applied by the caller after
/// the egui frame.
#[derive(Debug)]
pub enum SceneTreeAction {
    /// Frame this prim and its descendants with the viewport orbit camera.
    FrameSelected(String),
    /// Render the viewport through this authored camera, without editing it.
    UseCamera(String),
    ToggleCollapsed(u64),
    TogglePacked(u64),
    /// `None` = deselect.
    Select(Option<String>),
    ToggleVisibility(String),
    /// Author an empty root-layer `over` for a display-only prim ("Create
    /// Opinion"), making it editable. Caller authors + records undo.
    CreateOpinion(String),
    /// Remove an empty root-layer `over` for a prim ("Remove Opinion").
    RemoveOpinion(String),
    /// Author a new typed child prim under the given parent path. The caller
    /// picks a unique leaf name derived from `type_name`.
    DefineChild {
        parent_path: String,
        type_name: String,
    },
    /// Request the shell open the remote-asset picker to create a typeless
    /// reference/payload child under the given parent path. Only emitted on a
    /// live remote-sync stage (the sync server authors the prim). The shell
    /// reuses the "Open Remote…" dialog, excludes the current stage, and on
    /// selection sends a `CreatePrim` with `type = Ref`, `ref_type = Payload`.
    CreateAssetChild {
        parent_path: String,
    },
    /// Author a new typed child prim *with schema-sensible default attributes*
    /// (camera / light / environment). `template_key` is mapped to an
    /// `idtx_usd::PrimTemplate` by the shell (which owns the `authoring`
    /// feature), so `idtx-editor-ui` need not enable it.
    DefineChildFromTemplate {
        parent_path: String,
        template_key: String,
    },
    /// Delete the prim (and its subtree) at the given path.
    DeletePrim(String),
    /// Request the shell open its rename dialog for the prim at this path.
    RequestRename(String),
    /// Request the shell prompt for an asset and add a reference arc to it.
    AddReference(String),
    /// Request the shell prompt for an asset and add a payload arc to it.
    AddPayload(String),
    /// Request the shell prompt for a Rust `.rs` behavior source and attach it to
    /// the prim at this path — the shell copies it into the project's
    /// `behaviors/` and authors the `IdtxBehaviorAPI` manifest scaffold.
    AttachBehavior(String),
    /// An asset dragged from the Asset Browser was dropped on this prim:
    /// author a new child prim under it carrying a **reference** to the given
    /// project-relative asset path. Emitted when Shift is held on drop.
    DropAssetReference {
        /// Drop-target prim path; the new arc-bearing child is nested under it.
        prim: String,
        /// Project-relative (`/`-separated) path of the dropped USD stage.
        asset_rel: String,
    },
    /// As [`Self::DropAssetReference`] but authors a **payload** arc on the new
    /// child — the default drop (no modifier).
    DropAssetPayload {
        /// Drop-target prim path; the new arc-bearing child is nested under it.
        prim: String,
        /// Project-relative (`/`-separated) path of the dropped USD stage.
        asset_rel: String,
    },
    /// Acknowledges that the scroll-to-selected action has been serviced.
    AckScroll,
}

/// Prim types offered by the scene-tree "Create Child" submenu. Typed
/// camera / light / environment authoring gets dedicated helpers; this
/// is the generic structural set. `(menu label, USD prim type)`.
const CREATE_CHILD_PRIM_TYPES: &[(&str, &str)] = &[
    ("Xform (group)", "Xform"),
    ("Scope", "Scope"),
    ("Cube", "Cube"),
    ("Sphere", "Sphere"),
    ("Cylinder", "Cylinder"),
    ("Cone", "Cone"),
    ("Capsule", "Capsule"),
    ("Plane", "Plane"),
];

/// Typed prim templates offered by the scene-tree "Create Camera/Light" submenu
/// Each authors the prim type *and* schema-sensible default attributes.
/// `(menu label, template key)`; the key is mapped to an `idtx_usd::PrimTemplate`
/// by the shell so `idtx-editor-ui` need not enable `idtx-usd/authoring`. Scope
/// is omitted here — it carries no defaults and is already in the generic
/// "Create Child" set above.
const CREATE_TYPED_PRIM_TEMPLATES: &[(&str, &str)] = &[
    ("Bezier Curve", "bezier-curve"),
    ("Linear Curve", "linear-curve"),
    ("Camera", "camera"),
    ("Distant Light (sun)", "distant-light"),
    ("Rect Light", "rect-light"),
    ("Dome Light (environment)", "dome-light"),
];

/// USD prim types the collaboration sync server can author (mirrors
/// `idtxcore.PrimType`). On a live remote stage every other create entry is
/// shown disabled, because the stage is server-authoritative and the server
/// only knows these built-ins. Kept as a local literal so `idtx-editor-ui` need
/// not depend on `idtx-collab`.
const REMOTE_CREATABLE_PRIM_TYPES: &[&str] =
    &["Xform", "Cube", "Sphere", "Cone", "Cylinder", "Capsule", "Plane"];

/// Hover explanation shown on create entries disabled because the stage is a
/// live collaboration session.
const REMOTE_UNSUPPORTED_WHY: &str =
    "Only built-in shapes (Xform, Cube, Sphere, Cone, Cylinder, Capsule, Plane) can be \
     created in a collaboration session — the sync server authors the prim.";

/// Hover explanation for the remote-only "Asset…" create entry.
const REMOTE_ASSET_CHILD_WHY: &str =
    "Create a child that references another asset from the server — the sync \
     server authors a payload to the chosen stage.";

// ── Panel entry point ─────────────────────────────────────────────────────────

/// Render the scene tree panel.
///
/// `nodes` is the flat list of prim snapshots (pseudo-root excluded).
/// `index` is the pre-built tree index; rebuilt only when topology changes.
/// `state` is a read-only snapshot of the current UI state.
/// `authoring_allowed` gates the right-click "Create/Remove Opinion" items.
/// `behavior_authoring_allowed` additionally gates the "Attach Behavior…" item
/// (a narrower capability — only products that permit behavior authoring).
/// `remote_restricted` is true when the stage is a live collaboration session:
/// prim types the sync server cannot create (Scope, Camera, lights) are shown
/// disabled with an explanation, leaving only the server-creatable built-ins.
/// Mutations are pushed into `actions`; apply them with `SceneTreeState::apply`.
pub fn scene_tree_panel(
    ui: &mut Ui,
    nodes: &[TreeNode],
    index: &SceneTreeIndex,
    state: &SceneTreeState,
    authoring_allowed: bool,
    behavior_authoring_allowed: bool,
    remote_restricted: bool,
    actions: &mut Vec<SceneTreeAction>,
) {
    // Panel header
    ui.horizontal(|ui| {
        ui.heading("Scene");
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            if state.selected.is_some()
                && ui
                    .small_button("×")
                    .on_hover_text("Clear selection")
                    .clicked()
            {
                actions.push(SceneTreeAction::Select(None));
            }
        });
    });
    ui.separator();

    if nodes.is_empty() {
        ui.label(
            egui::RichText::new("No stage open")
                .small()
                .italics()
                .color(Color32::DARK_GRAY),
        );
        return;
    }

    // Let long names extend to their natural width instead of truncating, so a
    // name wider than the panel pushes the row past the viewport and the
    // enclosing `ScrollArea::both` surfaces a horizontal scrollbar.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    egui::ScrollArea::both()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Row-layout metrics, captured once per frame in content space.
            // `origin_x` is the content's left edge and `viewport_w` the visible
            // width; both translate together with horizontal scroll, so the
            // right-pinned row controls stay put rather than feeding back into
            // the measured content width.
            let origin_x = ui.cursor().left();
            let viewport_w = ui.clip_rect().width();
            for &root_idx in &index.roots {
                // Roots live in the open root stage: never inside a packed
                // boundary, so never referenced; no composed content revealed yet.
                draw_node(
                    ui,
                    nodes,
                    index,
                    root_idx,
                    0,
                    state,
                    authoring_allowed,
                    behavior_authoring_allowed,
                    remote_restricted,
                    actions,
                    false,
                    false,
                    viewport_w,
                    origin_x,
                );
            }
        });
}

// ── Recursive node drawing ────────────────────────────────────────────────────

// `authoring_allowed` and `behavior_authoring_allowed` are distinct capability
// gates threaded verbatim through the recursion; bundling them into a struct
// would only obscure the pass-through, so the bool count is intentional.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn draw_node(
    ui: &mut Ui,
    nodes: &[TreeNode],
    index: &SceneTreeIndex,
    node_idx: usize,
    depth: u32,
    state: &SceneTreeState,
    authoring_allowed: bool,
    behavior_authoring_allowed: bool,
    remote_restricted: bool,
    actions: &mut Vec<SceneTreeAction>,
    inside_packed: bool,
    composed_revealed: bool,
    viewport_w: f32,
    origin_x: f32,
) {
    let node = &nodes[node_idx];
    let hash = node.path_hash;
    // OpenUSD forbids nesting gprims in namespace, so no child may be authored
    // under a gprim — this gates both the drag-drop nested-child path and the
    // "Create Child"/"Create Camera/Light" context-menu items below.
    let node_is_gprim = is_gprim(&node.type_name);
    let is_collapsed = state.is_collapsed(hash);
    let is_selected = state.selected.as_deref() == Some(node.path.as_str());
    let is_packed_revealed = state.is_packed_revealed(hash);
    // A prim is "referenced" (grey, display-only, offers Create Opinion) purely
    // from its own authorship + whether it sits inside a packed boundary — not
    // from how its parent was drawn. This is what keeps unauthored siblings of a
    // freshly-authored `over` referenced instead of flipping them to local defs.
    let is_referenced = prim_is_referenced(inside_packed, node.is_locally_authored);

    let all_children = index.children(node.path.as_str());

    // Determine child visibility/expandability without allocating. Composed
    // (unauthored) children are gated by the reveal state flowing to this level;
    // locally-authored children (overs/defs) always show.
    let has_packed_arc = node.has_packed_arc;
    let child_inside_packed = inside_packed || has_packed_arc;
    let child_composed_revealed =
        composed_children_revealed(has_packed_arc, is_packed_revealed, composed_revealed);
    let has_composed_children = all_children.iter().any(|&i| !nodes[i].is_locally_authored);
    // A child is visible exactly when the draw gate below would not skip it, so
    // the expander chevron and the actual rows never disagree (a parent with a
    // visible variant-composed packed boundary still gets its chevron).
    let has_visible_children = all_children.iter().any(|&i| {
        !composed_child_hidden(
            nodes[i].is_locally_authored,
            child_composed_revealed,
            child_inside_packed,
        )
    });
    // A packed-arc node stays expandable even with its interior hidden so the
    // chevron can reveal the composed subtree once the packed toggle is used.
    let has_expandable = has_visible_children || (has_packed_arc && has_composed_children);

    ui.push_id(Id::new(hash), |ui| {
        let row_resp = draw_row(
            ui,
            node,
            depth,
            is_selected,
            is_collapsed,
            is_packed_revealed,
            has_expandable,
            is_referenced,
            !state.purpose_filter.matches(node.purpose),
            hash,
            viewport_w,
            origin_x,
            actions,
        );

        // Scroll the selected row into view when requested from external selection
        // (e.g. a viewport pick).  The flag is cleared via AckScroll after one frame.
        if is_selected && state.scroll_to_selected {
            row_resp.scroll_to_me(Some(egui::Align::Center));
            actions.push(SceneTreeAction::AckScroll);
        }

        if row_resp.clicked() {
            actions.push(SceneTreeAction::Select(Some(node.path.clone())));
        }
        if row_resp.double_clicked() && node.type_name == "Camera" {
            actions.push(SceneTreeAction::UseCamera(node.path.clone()));
        }

        // Accept a USD-stage asset dragged from the Asset Browser as a
        // new nested child carrying a reference/payload arc. Only locally-authored
        // prims can host a new child (mirrors the "Add Reference/Payload…"
        // context-menu gating). The default drop authors a **payload** child;
        // holding Shift on release authors a **reference** child instead.
        if authoring_allowed && node.is_locally_authored && !node_is_gprim {
            let dragging_asset =
                egui::DragAndDrop::has_payload_of_type::<AssetDragPayload>(ui.ctx());
            if dragging_asset && row_resp.contains_pointer() {
                let accent = ui.visuals().selection.bg_fill;
                ui.painter().rect_stroke(
                    row_resp.rect,
                    2.0,
                    egui::Stroke::new(1.5, accent),
                    egui::StrokeKind::Inside,
                );
            }
            if let Some(payload) = row_resp.dnd_release_payload::<AssetDragPayload>() {
                let as_reference = ui.input(|i| i.modifiers.shift);
                actions.push(if as_reference {
                    SceneTreeAction::DropAssetReference {
                        prim: node.path.clone(),
                        asset_rel: payload.rel_path.clone(),
                    }
                } else {
                    SceneTreeAction::DropAssetPayload {
                        prim: node.path.clone(),
                        asset_rel: payload.rel_path.clone(),
                    }
                });
            }
        }

        // Right-click context menu. "Create Opinion" is offered on display-only
        // prims (composed from a reference/payload with no root opinion, drawn
        // gray). "Remove Opinion" is offered on a root override that sits inside
        // a packed boundary. Locally-authored prims additionally get the
        // structural authoring items (create child / rename / add arc / delete).
        // Navigation and copying are available in every product.
        let show_opinion = is_referenced || (node.is_locally_authored && inside_packed);
        let show_structural = node.is_locally_authored;
        row_resp.context_menu(|ui| {
            if ui.button("Copy Path").clicked() {
                ui.ctx().copy_text(node.path.clone());
                ui.close();
            }
            if ui
                .add(egui::Button::new("Frame Selected").shortcut_text("Numpad ."))
                .clicked()
            {
                actions.push(SceneTreeAction::Select(Some(node.path.clone())));
                actions.push(SceneTreeAction::FrameSelected(node.path.clone()));
                ui.close();
            }
            if authoring_allowed && (show_opinion || show_structural) {
                ui.separator();
                if is_referenced {
                    if ui.button("Create Opinion").clicked() {
                        actions.push(SceneTreeAction::CreateOpinion(node.path.clone()));
                        ui.close();
                    }
                } else if node.is_locally_authored
                    && inside_packed
                    && ui.button("Remove Opinion").clicked()
                {
                    actions.push(SceneTreeAction::RemoveOpinion(node.path.clone()));
                    ui.close();
                }

                if show_structural {
                    if show_opinion {
                        ui.separator();
                    }
                    // A gprim can't host children (OpenUSD forbids nesting
                    // gprims in namespace). Surface the child-authoring entries
                    // disabled with an explanation rather than hiding them, so
                    // the restriction is discoverable.
                    if node_is_gprim {
                        let why = "Gprims can't contain child prims: OpenUSD \
                                   forbids nesting gprims in namespace \
                                   (visibility and activation are pruning).";
                        ui.add_enabled(false, egui::Button::new("Create Child"))
                            .on_disabled_hover_text(why);
                        ui.add_enabled(false, egui::Button::new("Create Camera/Light"))
                            .on_disabled_hover_text(why);
                    } else {
                        ui.menu_button("Create Child", |ui| {
                            for (label, type_name) in CREATE_CHILD_PRIM_TYPES {
                                // On a remote stage, only server-creatable
                                // built-ins are enabled; the rest are shown
                                // disabled so the restriction is discoverable.
                                let allowed = !remote_restricted
                                    || REMOTE_CREATABLE_PRIM_TYPES.contains(type_name);
                                if allowed {
                                    if ui.button(*label).clicked() {
                                        actions.push(SceneTreeAction::DefineChild {
                                            parent_path: node.path.clone(),
                                            type_name: (*type_name).to_owned(),
                                        });
                                        ui.close();
                                    }
                                } else {
                                    ui.add_enabled(false, egui::Button::new(*label))
                                        .on_disabled_hover_text(REMOTE_UNSUPPORTED_WHY);
                                }
                            }
                            // On a live remote-sync stage, offer creating a child
                            // that references/payloads another server asset. Shown
                            // only when remote (the sync server authors the arc)
                            // and enabled under the same rule as the built-in
                            // shapes above (always, on a non-gprim parent).
                            if remote_restricted {
                                ui.separator();
                                if ui
                                    .button("Asset…")
                                    .on_hover_text(REMOTE_ASSET_CHILD_WHY)
                                    .clicked()
                                {
                                    actions.push(SceneTreeAction::CreateAssetChild {
                                        parent_path: node.path.clone(),
                                    });
                                    ui.close();
                                }
                            }
                        });
                        // The sync server cannot create cameras or lights, so the
                        // whole template submenu is disabled on a remote stage.
                        if remote_restricted {
                            ui.add_enabled(false, egui::Button::new("Create Camera/Light"))
                                .on_disabled_hover_text(REMOTE_UNSUPPORTED_WHY);
                        } else {
                            ui.menu_button("Create Camera/Light", |ui| {
                                for (label, template_key) in CREATE_TYPED_PRIM_TEMPLATES {
                                    if ui.button(*label).clicked() {
                                        actions.push(SceneTreeAction::DefineChildFromTemplate {
                                            parent_path: node.path.clone(),
                                            template_key: (*template_key).to_owned(),
                                        });
                                        ui.close();
                                    }
                                }
                            });
                        }
                    }
                    if ui.button("Rename…").clicked() {
                        actions.push(SceneTreeAction::RequestRename(node.path.clone()));
                        ui.close();
                    }
                    // Reference/payload arcs compose the referenced subtree as
                    // children of this prim, so they are suppressed on gprims
                    // for the same no-nested-gprims reason as "Create Child".
                    if !node_is_gprim {
                        if ui.button("Add Reference…").clicked() {
                            actions.push(SceneTreeAction::AddReference(node.path.clone()));
                            ui.close();
                        }
                        if ui.button("Add Payload…").clicked() {
                            actions.push(SceneTreeAction::AddPayload(node.path.clone()));
                            ui.close();
                        }
                    }
                    if behavior_authoring_allowed && ui.button("Attach Behavior…").clicked() {
                        actions.push(SceneTreeAction::AttachBehavior(node.path.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Delete").clicked() {
                        actions.push(SceneTreeAction::DeletePrim(node.path.clone()));
                        ui.close();
                    }
                }
            }
        });

        if !is_collapsed {
            // One pass over every child. "Referenced" is derived inside the
            // recursive call from `child_inside_packed` + the child's own
            // authorship, so the authored `over` chain, its unauthored siblings,
            // and nested packed boundaries all classify correctly on their own
            // merits rather than inheriting the parent's branch.
            for &child_idx in all_children {
                // Composed (unauthored) children appear only when the enclosing
                // boundary is revealed; locally-authored overs/defs always show,
                // so the authored opinion chain stays visible even while the
                // referenced contents are hidden. The exception: a composed
                // packed boundary under a non-packed parent (a prim def'd inside
                // a variant that references a stage) stays visible so its own
                // unveil toggle is reachable.
                if composed_child_hidden(
                    nodes[child_idx].is_locally_authored,
                    child_composed_revealed,
                    child_inside_packed,
                ) {
                    continue;
                }
                draw_node(
                    ui,
                    nodes,
                    index,
                    child_idx,
                    depth + 1,
                    state,
                    authoring_allowed,
                    behavior_authoring_allowed,
                    remote_restricted,
                    actions,
                    child_inside_packed,
                    child_composed_revealed,
                    viewport_w,
                    origin_x,
                );
            }
        }
    });
}

// ── Row rendering ─────────────────────────────────────────────────────────────
#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
fn draw_row(
    ui: &mut Ui,
    node: &TreeNode,
    depth: u32,
    is_selected: bool,
    is_collapsed: bool,
    is_packed_revealed: bool,
    has_expandable: bool,
    is_referenced: bool,
    filtered_out: bool,
    hash: u64,
    viewport_w: f32,
    origin_x: f32,
    actions: &mut Vec<SceneTreeAction>,
) -> egui::Response {
    let accent = ui.visuals().selection.bg_fill;
    let row_height = ICON_SIZE + 4.0;

    ui.horizontal(|ui| {
        ui.set_min_height(row_height);
        // Lay the row out strictly left→right with hand-managed gaps so the
        // right-pinned controls can be positioned by absolute x. A natural-width
        // (non-truncated) label then pushes the row past the viewport and the
        // enclosing `ScrollArea::both` shows a horizontal scrollbar.
        let gap = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = 0.0;

        // Indent
        ui.add_space(depth.to_f32().unwrap_or(f32::MAX) * INDENT_PX);

        // Expander
        if has_expandable {
            let (ch_rect, ch_resp) =
                ui.allocate_exact_size(egui::vec2(ICON_SIZE, row_height), egui::Sense::click());
            if ui.is_rect_visible(ch_rect) {
                let col = if ch_resp.hovered() {
                    ui.style().visuals.text_color()
                } else {
                    ui.style().visuals.weak_text_color()
                };
                let img = egui::Image::new(icons::chevron(!is_collapsed))
                    .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE))
                    .tint(col);
                let icon_rect =
                    egui::Rect::from_center_size(ch_rect.center(), egui::Vec2::splat(ICON_SIZE));
                img.paint_at(ui, icon_rect);
            }
            if ch_resp.clicked() {
                actions.push(SceneTreeAction::ToggleCollapsed(hash));
            }
        } else {
            ui.add_space(ICON_SIZE);
        }

        ui.add_space(gap);

        // Schema icon (procedural, swappable)
        let (icon_rect, _) =
            ui.allocate_exact_size(egui::vec2(ICON_SIZE, ICON_SIZE), egui::Sense::hover());
        if ui.is_rect_visible(icon_rect) {
            let mut img = egui::Image::new(classify_schema_icon(&node.type_name))
                .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE));
            if filtered_out {
                img = img.tint(crate::theme::current().filtered_out);
            }
            img.paint_at(ui, icon_rect);
        }
        ui.add_space(gap);

        // Label — extends to its natural width (no truncation).
        let rich = if filtered_out {
            egui::RichText::new(&node.display_label).color(crate::theme::current().filtered_out)
        } else if is_referenced {
            egui::RichText::new(&node.display_label).weak()
        } else if node.is_root_over {
            // Prims the root layer only overrides (`over`) are flagged orange to
            // distinguish them from root-layer definitions.
            egui::RichText::new(&node.display_label).color(root_over_color())
        } else {
            egui::RichText::new(&node.display_label)
        };
        let label_resp = ui.selectable_label(is_selected, rich);

        // Right-pinned controls (behavior, packed toggle, visibility eye). Pin
        // them to the viewport's right edge while the row fits; once the label overflows,
        // the padding collapses to a single gap and the row scrolls horizontally.
        // `SCROLLBAR_PAD` keeps them clear of the vertical scrollbar gutter.
        let eye_w = ICON_SIZE;
        let packed_w = if node.has_packed_arc { ICON_SIZE + gap } else { 0.0 };
        let behavior_w = if node.has_behavior { ICON_SIZE + gap } else { 0.0 };
        let controls_w = behavior_w + packed_w + eye_w;
        let right_limit = origin_x + viewport_w - SCROLLBAR_PAD;
        let target_x = right_limit - controls_w;
        let pad = (target_x - ui.min_rect().right()).max(gap);
        ui.add_space(pad);

        if node.has_behavior {
            ui.add(icons::icon_image(ui, icons::CODE, ICON_SIZE))
                .on_hover_text("Behavior attached");
            ui.add_space(gap);
        }

        // Packed toggle: reveal/hide composed subtree
        if node.has_packed_arc {
            let base_col = ui.visuals().weak_text_color();
            let icon_color = if is_packed_revealed { accent } else { base_col };
            let tooltip = if is_packed_revealed {
                "Referenced contents shown — click to hide"
            } else {
                "Show referenced contents"
            };
            let (pk_rect, pk_resp) =
                ui.allocate_exact_size(egui::vec2(ICON_SIZE, row_height), egui::Sense::click());
            if ui.is_rect_visible(pk_rect) {
                let col = if pk_resp.hovered() {
                    ui.visuals().text_color()
                } else {
                    icon_color
                };
                let img = egui::Image::new(icons::PACKED)
                    .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE))
                    .tint(col);
                let icon_rect =
                    egui::Rect::from_center_size(pk_rect.center(), egui::Vec2::splat(ICON_SIZE));
                img.paint_at(ui, icon_rect);
            }
            if pk_resp.on_hover_text(tooltip).clicked() {
                actions.push(SceneTreeAction::TogglePacked(hash));
            }
            ui.add_space(gap);
        }

        // Eye / visibility toggle stub
        let (eye_rect, eye_resp) =
            ui.allocate_exact_size(egui::vec2(ICON_SIZE, row_height), egui::Sense::click());
        if ui.is_rect_visible(eye_rect) {
            let col = if eye_resp.hovered() {
                ui.style().visuals.text_color()
            } else {
                ui.style().visuals.weak_text_color()
            };
            let img = egui::Image::new(icons::eye(node.visible))
                .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE))
                .tint(col);
            let icon_rect =
                egui::Rect::from_center_size(eye_rect.center(), egui::Vec2::splat(ICON_SIZE));
            img.paint_at(ui, icon_rect);
        }
        if eye_resp
            .on_hover_text(if node.visible {
                "Hide prim"
            } else {
                "Show prim"
            })
            .clicked()
        {
            actions.push(SceneTreeAction::ToggleVisibility(node.path.clone()));
        }

        label_resp
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    fn navigation_node(path: &str, packed: bool) -> TreeNode {
        let (parent, name) = path.rsplit_once('/').unwrap();
        TreeNode {
            path: path.into(),
            parent_path: (!parent.is_empty()).then(|| parent.to_owned()),
            name: name.into(),
            type_name: "Xform".into(),
            has_packed_arc: packed,
            has_behavior: false,
            is_locally_authored: parent.is_empty(),
            path_hash: stable_prim_hash(path),
            display_label: name.into(),
            visible: true,
            purpose: RenderPurpose::Default,
            is_root_over: false,
        }
    }

    #[test]
    fn selecting_nested_reference_reveals_and_scrolls_to_target() {
        let nodes = vec![
            navigation_node("/Package", true),
            navigation_node("/Package/Nested", true),
            navigation_node("/Package/Nested/Target", false),
            navigation_node("/Unrelated", true),
        ];
        let mut state = SceneTreeState::new();
        state.collapse_all(&nodes);
        state.apply(
            vec![SceneTreeAction::Select(Some(nodes[2].path.clone()))],
            &nodes,
        );
        assert_eq!(state.selected.as_deref(), Some("/Package/Nested/Target"));
        assert!(state.scroll_to_selected);
        for ancestor in &nodes[..2] {
            assert!(!state.is_collapsed(ancestor.path_hash));
            assert!(state.is_packed_revealed(ancestor.path_hash));
        }
        assert!(state.is_collapsed(nodes[3].path_hash));
        assert!(!state.is_packed_revealed(nodes[3].path_hash));

        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let mut actions = vec![];
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            scene_tree_panel(
                ui, &nodes, &SceneTreeIndex::build(&nodes), &state,
                false, false, false, &mut actions,
            );
        });
        assert!(output.shapes.iter().any(|shape| matches!(
            &shape.shape, egui::Shape::Text(text) if text.galley.text() == "Target"
        )));
        assert!(actions.iter().any(|action| matches!(action, SceneTreeAction::AckScroll)));
    }

    #[test]
    fn double_clicking_camera_in_readonly_tree_requests_camera_view() {
        let mut node = navigation_node("/Camera", false);
        node.type_name = "Camera".into();
        let nodes = vec![node];
        let index = SceneTreeIndex::build(&nodes);
        let state = SceneTreeState::new();
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        let frame = |time, events| {
            let mut actions = vec![];
            let output = ctx.run_ui(
                egui::RawInput { time: Some(time), events, ..Default::default() },
                |ui| {
                    scene_tree_panel(
                        ui, &nodes, &index, &state, false, false, false, &mut actions,
                    );
                },
            );
            (output, actions)
        };
        let (output, _) = frame(0.0, vec![]);
        let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Camera" => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
            }
            _ => None,
        }).expect("camera row");
        let pointer = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(0.1, vec![egui::Event::PointerMoved(pos), pointer(true)]);
        frame(0.15, vec![pointer(false)]);
        frame(0.2, vec![pointer(true)]);
        let (_, actions) = frame(0.25, vec![pointer(false)]);
        assert!(actions.iter().any(|action| matches!(
            action, SceneTreeAction::UseCamera(path) if path == "/Camera"
        )));
        assert!(actions.iter().any(|action| matches!(
            action, SceneTreeAction::Select(Some(path)) if path == "/Camera"
        )));
    }

    #[test]
    fn purpose_filters_keep_all_tree_rows_and_tint_only_excluded_prims() {
        let nodes: Vec<_> = [
            ("/Guide", None, RenderPurpose::Guide),
            ("/Guide/Proxy", Some("/Guide"), RenderPurpose::Proxy),
            ("/Guide/Render", Some("/Guide"), RenderPurpose::Render),
            ("/Guide/Default", Some("/Guide"), RenderPurpose::Default),
        ]
        .into_iter()
        .map(|(path, parent, purpose)| TreeNode {
            path: path.to_owned(),
            parent_path: parent.map(str::to_owned),
            name: path.to_owned(),
            type_name: "Xform".to_owned(),
            has_packed_arc: parent.is_none(),
            has_behavior: false,
            is_locally_authored: parent.is_none(),
            path_hash: stable_prim_hash(path),
            display_label: path.to_owned(),
            visible: true,
            purpose,
            is_root_over: parent.is_none(),
        })
        .collect();
        let index = SceneTreeIndex::build(&nodes);
        for mask in 0..8 {
            // Selected labels must retain the filter tint too. The root is an
            // authored over and its children are composed, testing both normal
            // color priorities beneath the purpose exclusion.
            for selected in &nodes {
                let ctx = egui::Context::default();
                crate::theme::Theme::dark().apply_style(&ctx);
                egui_extras::install_image_loaders(&ctx);
                let state = SceneTreeState {
                    purpose_filter: PurposeFilter {
                        guide: mask & 1 != 0,
                        proxy: mask & 2 != 0,
                        render: mask & 4 != 0,
                    },
                    selected: Some(selected.path.clone()),
                    packed_revealed: HashSet::from([nodes[0].path_hash]),
                    ..Default::default()
                };
                let output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(700.0, 500.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        scene_tree_panel(
                            ui, &nodes, &index, &state, false, false, false, &mut vec![],
                        );
                    },
                );
                for node in &nodes {
                    let text = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text)
                                if text.galley.text() == node.display_label =>
                            {
                                Some(text)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("Filter {mask} removed row {}", node.path));
                    let color = text.override_text_color.unwrap_or_else(|| {
                        let color = text.galley.job.sections[0].format.color;
                        if color == Color32::PLACEHOLDER {
                            text.fallback_color
                        } else {
                            color
                        }
                    });
                    let purpose_bit = match node.purpose {
                        RenderPurpose::Default => 0,
                        RenderPurpose::Guide => 1,
                        RenderPurpose::Proxy => 2,
                        RenderPurpose::Render => 4,
                    };
                    let excluded = mask != 0 && purpose_bit != 0 && mask & purpose_bit == 0;
                    assert_eq!(
                        color == crate::theme::current().filtered_out,
                        excluded,
                        "Filter {mask}, row {}, selected {}",
                        node.path,
                        selected.path,
                    );
                }
            }
        }
    }

    // ── Render classification (the Create Opinion sibling bug) ──────────────────

    // "Referenced" (display-only, grey, offers Create Opinion) is decided from a
    // prim's own authorship + whether it sits inside a packed boundary — never
    // from how its parent was drawn.
    #[test]
    fn prim_is_referenced_depends_only_on_self() {
        // Inside a composed boundary with no local spec → referenced.
        assert!(prim_is_referenced(true, false));
        // Inside a composed boundary but locally authored (an `over`/`def`) → not.
        assert!(!prim_is_referenced(true, true));
        // Outside any packed boundary (root-stage prim) → never referenced,
        // regardless of authorship.
        assert!(!prim_is_referenced(false, false));
        assert!(!prim_is_referenced(false, true));
    }

    // Composed children follow the enclosing packed boundary's reveal state: a
    // packed-arc node gates on its own toggle, every other node inherits.
    #[test]
    fn composed_children_reveal_gating() {
        // Packed-arc node: its own toggle governs the interior.
        assert!(composed_children_revealed(true, true, false));
        assert!(!composed_children_revealed(true, false, true));
        // Non-arc node (e.g. an authored `over` ancestor that lost its arc)
        // inherits the enclosing boundary's reveal state, so deeper composed
        // prims flow through.
        assert!(composed_children_revealed(false, false, true));
        assert!(!composed_children_revealed(false, true, false));
    }

    // The reported bug, reproduced over the classification layer: `/Asset`
    // references a subtree `Geom/{Legs,Top}` each holding two cubes. After
    // "Create Opinion" on `Legs/pCube1` — which authors an `over` on it *and its
    // ancestor chain* `Asset→Geom→Legs` — the unauthored siblings must stay
    // referenced (Create Opinion eligible), while the authored chain must not.
    #[test]
    fn create_opinion_leaves_siblings_referenced() {
        // Walk the fixed hierarchy exactly as `draw_node` would, tracking
        // `inside_packed` and asking the pure classifier for each prim.
        //
        // (path, is_locally_authored, has_packed_arc)
        let prims: &[(&str, bool, bool)] = &[
            ("/Asset", true, true),            // reference arc, authored in root
            ("/Asset/Geom", true, false),      // ancestor over from Create Opinion
            ("/Asset/Geom/Legs", true, false), // ancestor over
            ("/Asset/Geom/Legs/pCube1", true, false), // the Create Opinion target
            ("/Asset/Geom/Legs/pCube2", false, false), // sibling — unauthored
            ("/Asset/Geom/Top", false, false), // sibling branch — unauthored
            ("/Asset/Geom/Top/pCube1", false, false),
            ("/Asset/Geom/Top/pCube2", false, false),
        ];

        // `inside_packed` is true for everything strictly below `/Asset` (the
        // packed-arc boundary), false for `/Asset` itself.
        let inside_packed = |path: &str| path != "/Asset" && path.starts_with("/Asset");

        for &(path, authored, _arc) in prims {
            let referenced = prim_is_referenced(inside_packed(path), authored);
            match path {
                // Authored chain: not referenced (drawn as local/over, editable
                // directly — Create Opinion is *not* offered, Remove Opinion is).
                "/Asset"
                | "/Asset/Geom"
                | "/Asset/Geom/Legs"
                | "/Asset/Geom/Legs/pCube1" => {
                    assert!(!referenced, "{path} should not be referenced");
                }
                // Everyone else is an unauthored composed sibling: still
                // referenced, so Create Opinion stays available (the bug made
                // these flip to white local defs with no Create Opinion).
                _ => assert!(referenced, "{path} should remain referenced"),
            }
        }
    }

    // A reference arc introduced *through a selected variant* (e.g. `PlayerRig`
    // def'd inside the `playerKind` variant of `Player`, referencing a rig stage)
    // composes a prim that is not locally authored yet carries the packed arc.
    // Its parent (`Player`) is an ordinary authored Xform with no arc, so the
    // child is not inside any packed boundary — it must show so its own unveil
    // toggle renders. Its interior stays hidden until that toggle is used.
    #[test]
    fn variant_composed_packed_boundary_stays_visible() {
        // `PlayerRig`: composed (unauthored), not inside a packed boundary
        // (parent `Player` is not packed) → MUST show.
        assert!(!composed_child_hidden(false, false, false));
        // Its interior (`Head`, `Capsule`): composed, now inside `PlayerRig`'s
        // packed boundary, not yet revealed → hidden until the toggle flips.
        assert!(composed_child_hidden(false, false, true));
        // …and shown once `PlayerRig`'s unveil toggle makes them revealed.
        assert!(!composed_child_hidden(false, true, true));
    }

    // A variant may also compose in a *plain* prim subtree (no reference/payload)
    // under an ordinary parent. With nothing to "unveil", it must simply appear
    // in the tree rather than being hidden behind a toggle that does not exist.
    #[test]
    fn variant_composed_plain_subtree_is_visible() {
        // Composed, no packed boundary anywhere above it (child_inside_packed is
        // false for the whole subtree) → visible at every depth.
        assert!(!composed_child_hidden(false, false, false));
    }

    // The exception is narrow: anything *inside* a packed boundary is still
    // gated. Authored and already-revealed children always show.
    #[test]
    fn composed_child_hidden_covers_the_ordinary_cases() {
        // Authored child → always visible, whatever the gating.
        assert!(!composed_child_hidden(true, false, false));
        assert!(!composed_child_hidden(true, false, true));
        // Already revealed → visible.
        assert!(!composed_child_hidden(false, true, true));
        // Composed and inside an unrevealed boundary → hidden (covers both a
        // plain leaf and a nested packed boundary under a packed parent).
        assert!(composed_child_hidden(false, false, true));
    }

    // The drag-and-drop authoring path can't be exercised without an
    // egui runtime, so the interaction itself is not unit-tested. What we *can*
    // pin down is the state contract: a dropped-asset action is serviced by the
    // shell (session write + undo record) and must leave `SceneTreeState`
    // untouched, exactly like the other structural actions.
    #[test]
    fn dropped_asset_actions_are_state_no_ops() {
        let mut state = SceneTreeState::new();
        state.selected = Some("/World/Group".to_owned());
        state.collapsed.insert(42);
        let before = (
            state.selected.clone(),
            state.collapsed.clone(),
            state.scroll_to_selected,
        );

        state.apply(
            vec![
                SceneTreeAction::DropAssetReference {
                    prim: "/World/Group".to_owned(),
                    asset_rel: "assets/prop.usda".to_owned(),
                },
                SceneTreeAction::DropAssetPayload {
                    prim: "/World/Group".to_owned(),
                    asset_rel: "assets/prop.usda".to_owned(),
                },
            ],
            &[],
        );

        assert_eq!(state.selected, before.0);
        assert_eq!(state.collapsed, before.1);
        assert_eq!(state.scroll_to_selected, before.2);
    }

    // "Create Child → Asset…" (remote-sync only) is likewise a shell-serviced
    // structural action — the menu item that emits it needs an egui runtime, but
    // the state contract we can pin down is that applying the action mutates no
    // `SceneTreeState` (the prim only appears once the server broadcast arrives).
    #[test]
    fn create_asset_child_action_is_state_no_op() {
        let mut state = SceneTreeState::new();
        state.selected = Some("/World/Group".to_owned());
        state.collapsed.insert(11);
        let before = (
            state.selected.clone(),
            state.collapsed.clone(),
            state.scroll_to_selected,
        );

        state.apply(
            vec![SceneTreeAction::CreateAssetChild {
                parent_path: "/World/Group".to_owned(),
            }],
            &[],
        );

        assert_eq!(state.selected, before.0);
        assert_eq!(state.collapsed, before.1);
        assert_eq!(state.scroll_to_selected, before.2);
    }
    // shell-serviced structural action: the context-menu item that emits it is
    // gated behind `behavior_authoring_allowed` inside `draw_node` (only
    // exercisable under an egui runtime), but the state contract we *can* pin
    // down is that applying the action mutates no `SceneTreeState`.
    #[test]
    fn attach_behavior_action_is_state_no_op() {
        let mut state = SceneTreeState::new();
        state.selected = Some("/World/Robot".to_owned());
        state.collapsed.insert(7);
        let before = (
            state.selected.clone(),
            state.collapsed.clone(),
            state.scroll_to_selected,
        );

        state.apply(
            vec![SceneTreeAction::AttachBehavior("/World/Robot".to_owned())],
            &[],
        );

        assert_eq!(state.selected, before.0);
        assert_eq!(state.collapsed, before.1);
        assert_eq!(state.scroll_to_selected, before.2);
    }

    // The context-menu gating itself needs an egui runtime, but the gprim
    // classification that drives it (`node_is_gprim` in `draw_node`) is pure
    // logic we can pin down here. OpenUSD forbids nesting gprims in namespace,
    // so every concrete gprim schema must be rejected as a child host while the
    // structural grouping prims (and non-gprim typed prims) are accepted.
    #[test]
    fn gprim_classification_blocks_child_authoring() {
        // Concrete gprims — children must never be authored under these.
        for ty in [
            "Cube",
            "Cube_1",
            "Sphere",
            "Cone",
            "Cylinder",
            "Capsule",
            "Capsule_1",
            "Plane",
            "Mesh",
            "Points",
            "BasisCurves",
            "NurbsCurves",
            "NurbsPatch",
            "HermiteCurves",
        ] {
            assert!(is_gprim(ty), "{ty} should be classified as a gprim");
        }

        // Grouping / non-gprim prims may host children.
        for ty in [
            "Xform",
            "Scope",
            "Camera",
            "DomeLight",
            "DistantLight",
            "RectLight",
            "Material",
            "Shader",
            "Skeleton",
            "SkelRoot",
            "", // typeless prim
        ] {
            assert!(!is_gprim(ty), "{ty} should not be classified as a gprim");
        }
    }
}
