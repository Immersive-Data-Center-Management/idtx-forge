//! Session → UI projection helpers and product-capability mapping.
//!
//! Pure(ish) builders/refreshers that turn `StudioSession` state into the cached
//! tree/inspector/layer-stack/material/gizmo data the UI reads, plus the
//! authoring-capability predicates. Called from the render path when caches are
//! stale; never on the hot no-change path.

use std::sync::Arc;

#[cfg(feature = "authoring")]
use idtx_editor_model::ProductKind;
use idtx_editor_model::{InspectorMode, LayerStackInfo, PrimInfo, PrimVariantSet, ProductProfile};
use idtx_editor_ui::compute_canvas::ComputeGraphSource;
use idtx_editor_ui::material_canvas::CanvasGraphSource;
use idtx_editor_ui::scene_tree::{TreeNode, stable_prim_hash};
use idtx_studio_session::StudioSession;

use crate::gizmo;
use crate::model::{InspectorTab, SessionTab};
use crate::view::GizmoTransformData;

/// Whether this product may author prim edits in the current build.
///
/// This is the single source of truth for editability, consumed by both the
/// inspector-mode mapping and the transform-gizmo gate so the two never
/// disagree. It is `true` only when **both** hold:
/// 1. the `authoring` feature is compiled into this build, and
/// 2. the product is an authoring product (`Poser` or `Conductor`).
///
/// The read-only `Inspector` is always `false`. Without the `authoring`
/// feature every product is `false`, so editable-looking widgets never render
/// in a build whose write path is compiled out.
pub(crate) fn product_supports_authoring(profile: ProductProfile) -> bool {
    #[cfg(feature = "authoring")]
    {
        matches!(profile.kind(), ProductKind::Poser | ProductKind::Conductor)
    }
    #[cfg(not(feature = "authoring"))]
    {
        let _ = profile;
        false
    }
}

/// Whether this product may run cooking in the current build.
///
/// The read-only `Inspector` is always `false`. Without the `cooking`
/// feature every product is `false`, so editable-looking widgets never render
/// in a build whose write path is compiled out.
pub(crate) fn product_supports_cooking(profile: ProductProfile) -> bool {
    #[cfg(feature = "cooking")]
    {
        matches!(profile.kind(), ProductKind::Conductor)
    }
    #[cfg(not(feature = "cooking"))]
    {
        let _ = profile;
        false
    }
}

/// Map a [`ProductProfile`] to the [`InspectorMode`] it should use.
///
/// - `Conductor` → [`InspectorMode::Cooking`] when `cooking` is enabled (which
///   also enables `authoring`).
/// - Otherwise, authoring products (`Poser`, `Conductor`) → [`InspectorMode::Authoring`]
///   (full attribute editing enabled), but only when the `authoring` feature is
///   compiled — see [`product_supports_authoring`].
/// - Otherwise → [`InspectorMode::ReadOnlyWithVariants`] (variant combos are
///   interactive; attributes remain read-only)
pub(crate) fn inspector_mode_for_profile(profile: ProductProfile) -> InspectorMode {
    if product_supports_cooking(profile) {
        InspectorMode::Cooking
    } else if product_supports_authoring(profile) {
        InspectorMode::Authoring
    } else {
        InspectorMode::ReadOnlyWithVariants
    }
}

/// Build the flat `TreeNode` list from the current stage topology.
///
/// Pre-computes `path_hash` and `display_label` so the per-frame draw path
/// does no string allocations.  Called only when topology changes.
pub(crate) fn build_tree_nodes(session: &StudioSession) -> Vec<TreeNode> {
    let meta = session.metadata();
    let stage = session.stage();
    let root_id = &meta.root_identifier;
    stage
        .topology()
        .iter()
        // Shared instancing prototypes (`/__Prototype_N`) are stage-global,
        // parented under the pseudo-root, and only exist because of native
        // instancing. Every prim in a prototype subtree is flagged
        // `is_prototype` by the bridge; exclude them from the scene tree
        // entirely so they never appear as spurious root-level nodes.
        .filter(|r| !r.is_pseudo_root && !r.is_prototype)
        .map(|r| {
            let path = stage.path(r.path).unwrap_or("").to_owned();
            let parent_path = r
                .parent
                .and_then(|pid| stage.path_for(pid))
                .map(|s| s.to_owned());
            let name = r.name.clone();
            let type_name = r.type_name.clone();
            let has_packed_arc = r.packed_arc.is_some();
            let is_locally_authored = r
                .local_layer_identifier
                .as_deref()
                .map(|id| id == root_id.as_str())
                .unwrap_or(root_id.is_empty());
            let path_hash = stable_prim_hash(&path);
            let display_label = if type_name.is_empty() {
                name.clone()
            } else {
                format!("{name}  [{type_name}]")
            };
            let visible = session.prim_authored_visible(&path);
            let purpose = r.purpose;
            let is_root_over = matches!(r.local_specifier, Some(idtx_usd::PrimSpecifier::Over));
            TreeNode {
                path,
                parent_path,
                name,
                type_name,
                has_packed_arc,
                is_locally_authored,
                path_hash,
                display_label,
                visible,
                purpose,
                is_root_over,
            }
        })
        .collect()
}

/// Refreshes the Layer Stack cache **only** while its tab is active.
///
/// This is the "FFI on client request" guard: `layer_stack()` crosses the
/// bridge exclusively when the user has the Layer Stack tab open, and only when
/// the stage topology generation differs from the cached one (a new stage, a
/// sublayer add/remove, a reload). When the Properties tab is active the cache
/// is left untouched, so opening a stage and never visiting this tab costs zero
/// layer-stack FFI calls.
pub(crate) fn refresh_layer_stack_cache(tab: &mut SessionTab) {
    if tab.inspector_tab != InspectorTab::LayerStack {
        return;
    }
    let topo_gen = tab.session.topology_generation();
    if tab.layer_stack_cache.info.is_some() && tab.layer_stack_cache.topo_gen == topo_gen {
        return;
    }
    let layers = tab
        .session
        .layer_stack()
        .map(|records| {
            let infos: Vec<_> = records.into_iter().map(layer_record_to_info).collect();
            build_layer_tree(infos)
        })
        .unwrap_or_default();
    tab.layer_stack_cache.info = Some(Arc::new(LayerStackInfo { layers }));
    tab.layer_stack_cache.topo_gen = topo_gen;
}

/// Maps a safe-wrapper [`idtx_usd::LayerRecord`] to the UI-neutral
/// [`idtx_editor_model::LayerInfo`] the panel renders.
fn layer_record_to_info(r: idtx_usd::LayerRecord) -> idtx_editor_model::LayerInfo {
    use idtx_editor_model::LayerArc;
    let arc = match r.arc {
        idtx_usd::LayerArcKind::Session => LayerArc::Session,
        idtx_usd::LayerArcKind::Root => LayerArc::Root,
        idtx_usd::LayerArcKind::SubLayer => LayerArc::SubLayer,
        idtx_usd::LayerArcKind::Reference => LayerArc::Reference,
        idtx_usd::LayerArcKind::Payload => LayerArc::Payload,
    };
    idtx_editor_model::LayerInfo {
        identifier: r.identifier,
        display_name: r.display_name,
        real_path: r.real_path,
        arc,
        arc_source_path: r.arc_source_path,
        arc_target_path: r.arc_target_path,
        has_specs: r.has_specs,
        is_loaded: r.is_loaded,
        depth: r.depth,
        offset: r.offset,
        scale: r.scale,
        delta_count: r.delta_count,
        is_anonymous: r.is_anonymous,
        is_dirty: r.is_dirty,
        is_muted: r.is_muted,
        is_edit_target: r.is_edit_target,
        parent_identifier: r.parent_identifier,
    }
}

/// Reorders a flat list of layer records (from the bridge) into DFS pre-order
/// reflecting the actual parent-child hierarchy, and recomputes `depth` from
/// the tree structure so the UI indentation is correct.
///
/// Records with an empty `parent_identifier` (session and root) are treated as
/// roots.  All other records are children of the record whose `identifier`
/// matches their `parent_identifier`.  Orphaned records (whose parent is not
/// present in the list, which can happen if a parent was filtered out) are
/// appended at the end with `depth = 1`.
fn build_layer_tree(
    records: Vec<idtx_editor_model::LayerInfo>,
) -> Vec<idtx_editor_model::LayerInfo> {
    // Build identifier → vec-of-children index.
    let mut children: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (i, r) in records.iter().enumerate() {
        if r.parent_identifier.is_empty() {
            roots.push(i);
        } else {
            children
                .entry(r.parent_identifier.clone())
                .or_default()
                .push(i);
        }
    }

    let mut out: Vec<idtx_editor_model::LayerInfo> = Vec::with_capacity(records.len());
    let mut visited = vec![false; records.len()];

    // DFS from each root.
    fn dfs(
        idx: usize,
        depth: u32,
        records: &[idtx_editor_model::LayerInfo],
        children: &std::collections::HashMap<String, Vec<usize>>,
        visited: &mut Vec<bool>,
        out: &mut Vec<idtx_editor_model::LayerInfo>,
    ) {
        if visited[idx] {
            return;
        }
        visited[idx] = true;
        let mut node = records[idx].clone();
        node.depth = depth;
        let id = node.identifier.clone();
        out.push(node);
        if let Some(ch) = children.get(&id) {
            for &ci in ch {
                dfs(ci, depth + 1, records, children, visited, out);
            }
        }
    }

    for root in roots {
        dfs(root, 0, &records, &children, &mut visited, &mut out);
    }

    // Append any orphaned records (parent not found in the list).
    for (i, r) in records.iter().enumerate() {
        if !visited[i] {
            let mut node = r.clone();
            node.depth = 1;
            out.push(node);
        }
    }

    out
}

/// Refreshes the per-tab inspector cache when the selection or any USD change has occurred.
///
/// Called once per frame, before the egui closure.  The cache stores the
/// `(prim_path, change_gen)` key so that `inspect_prim()` is only called on a
/// miss, not every frame.  `change_gen` increments on every USD change batch
/// (topology *and* value-only), so editing an attribute refreshes the cache.
pub(crate) fn refresh_inspector_cache(tab: &mut SessionTab) {
    let path = tab.session.selected_path().map(str::to_owned);
    let topo_gen = tab.session.topology_generation();
    let change_gen = tab.session.change_generation();

    if tab.inspector_cache.prim_path == path
        && tab.inspector_cache.topo_gen == topo_gen
        && tab.inspector_cache.change_gen == change_gen
    {
        return;
    }

    tab.inspector_cache.prim_path = path.clone();
    tab.inspector_cache.topo_gen = topo_gen;
    tab.inspector_cache.change_gen = change_gen;

    match path {
        None => {
            tab.inspector_cache.prim_info = None;
            tab.inspector_cache.properties = Arc::new(Vec::new());
            tab.inspector_cache.skeleton_joints = None;
        }
        Some(ref p) => {
            let properties = tab.session.inspect_prim(p).unwrap_or_default();
            let mut prim_info = build_prim_info(p, &mut tab.session);
            if let Some(info) = prim_info.as_mut() {
                info.applied_schemas = applied_schemas_from_properties(&properties);
            }
            let skeleton_joints = tab.session.skeleton_joints_for_path(p).map(Arc::new);
            tab.inspector_cache.properties = Arc::new(properties);
            tab.inspector_cache.prim_info = prim_info;
            tab.inspector_cache.skeleton_joints = skeleton_joints;
        }
    }
}

/// Refreshes the `/StageBehavior` inspector cache driving the Stage tab's
/// behavior section. Keyed only on `(topology_generation, change_generation)` —
/// the stage-behavior prim path is fixed, so this is independent of the current
/// selection. A no-op when the key is unchanged.
pub(crate) fn refresh_stage_behavior_cache(tab: &mut SessionTab) {
    let topo_gen = tab.session.topology_generation();
    let change_gen = tab.session.change_generation();
    if tab.stage_behavior_cache.key == Some((topo_gen, change_gen)) {
        return;
    }
    tab.stage_behavior_cache.key = Some((topo_gen, change_gen));

    if tab.session.has_stage_behavior() {
        let path = idtx_studio_session::STAGE_BEHAVIOR_PRIM_PATH;
        let properties = tab.session.inspect_prim(path).unwrap_or_default();
        let mut prim_info = build_prim_info(path, &mut tab.session);
        if let Some(info) = prim_info.as_mut() {
            info.applied_schemas = applied_schemas_from_properties(&properties);
        }
        tab.stage_behavior_cache.properties = Arc::new(properties);
        tab.stage_behavior_cache.prim_info = prim_info;
    } else {
        tab.stage_behavior_cache.prim_info = None;
        tab.stage_behavior_cache.properties = Arc::new(Vec::new());
    }
}

/// Extracts the prim's applied API schemas from the `apiSchemas` metadata record.
/// `inspect_prim` emits the composed applied-schema list as a single
/// `PropertyRecord` named `"apiSchemas"` whose `value_summary` is the short USD
/// names joined by `", "` (e.g. `"ShadowAPI, MaterialBindingAPI"`, from
/// `UsdPrim::GetAppliedSchemas`). Split it back into individual short names —
/// no extra FFI, the record was already fetched for the property grid. Returns
/// empty when no schemas are applied (the record is omitted entirely).
fn applied_schemas_from_properties(properties: &[idtx_usd::PropertyRecord]) -> Vec<String> {
    properties
        .iter()
        .find(|record| record.name == "apiSchemas")
        .map(|record| {
            record
                .value_summary
                .split(", ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Builds a [`PrimInfo`] DTO from the session's topology record for `path`.
///
/// True when the prim at `path` carries a root-layer opinion, i.e. its local
/// layer identifier matches the stage root identifier.
///
/// Prims pulled in via a packed/referenced stage without a root-layer override
/// return `false` — they are not directly editable until an override exists.
/// This is the same predicate the inspector's [`InspectorContext::is_prim_editable`]
/// combines with the mode; both the inspector fields and the transform gizmo
/// consult it so they stay consistent.
fn prim_is_locally_authored(session: &mut StudioSession, path: &str) -> bool {
    let stage = session.stage();
    let meta = session.metadata();
    let Some(prim_id) = stage.prim_at_path(path) else {
        return false;
    };
    let Some(record) = stage.topology().iter().find(|r| r.prim == prim_id) else {
        return false;
    };
    let root_id = &meta.root_identifier;
    record
        .local_layer_identifier
        .as_deref()
        .map(|id| id == root_id.as_str())
        .unwrap_or(root_id.is_empty())
}

/// Returns `None` when the path is not present in the current stage (e.g.
/// after a topology change where the previously-selected prim was removed).
fn build_prim_info(path: &str, session: &mut StudioSession) -> Option<PrimInfo> {
    let (type_name, is_packed_boundary, is_locally_authored, has_payload, payload_loaded) = {
        let stage = session.stage();
        let prim_id = stage.prim_at_path(path)?;
        let record = stage.topology().iter().find(|r| r.prim == prim_id)?;
        let has_payload = record
            .packed_arc
            .as_ref()
            .map(|a| matches!(a.kind, idtx_usd::PackedArcKind::Payload))
            .unwrap_or(false);
        (
            record.type_name.clone(),
            record.packed_arc.is_some(),
            record.local_layer_identifier.clone(),
            has_payload,
            record.state.loaded,
        )
    };
    let is_locally_authored = {
        let root_id = &session.metadata().root_identifier;
        is_locally_authored
            .as_deref()
            .map(|id| id == root_id.as_str())
            .unwrap_or(root_id.is_empty())
    };
    let is_visible = session.prim_authored_visible(path);
    let variant_sets = session
        .variant_sets(path)
        .unwrap_or_default()
        .into_iter()
        .map(|vs| PrimVariantSet {
            name: vs.name,
            selection: vs.selection,
            choices: vs.choices,
            provenance: vs.provenance,
            defined_on_local_layer: vs.defined_on_local_layer,
        })
        .collect();
    let local_transform = session.get_local_transform(path);
    let local_bounds = session.collision_local_bounds(path);
    #[cfg(feature = "authoring")]
    let authored_transform = session
        .authored_xform_stack(path)
        .map(|stack| classify_authored_transform(&stack.ops, stack.diagnostic.as_deref()));
    #[cfg(not(feature = "authoring"))]
    let authored_transform = None;
    let resolved_player_role = session
        .resolved_player_role(path)
        .map(|role| role.as_token().to_owned());
    Some(PrimInfo {
        path: path.to_owned(),
        type_name,
        kind: String::new(), // not surfaced by the topology projection
        applied_schemas: Vec::new(), // not surfaced by the topology projection
        is_visible,
        has_payload,
        payload_loaded,
        is_packed_boundary,
        is_locally_authored,
        variant_sets,
        local_transform,
        local_bounds,
        authored_transform,
        resolved_player_role,
    })
}

/// Classify an authored `xformOpOrder` stack into the inspector's edit model.
///
/// The reported bug: the inspector used to derive Euler angles by decomposing the
/// composed local matrix, and a matrix→XYZ-Euler decomposition can never place a
/// middle-axis (Y) rotation outside ±90° (`Ry=180°` and `Rx=180°,Rz=180°` are the
/// same matrix). So `Y=180` folded to `Y=0, X=180, Z=180`. Reading the authored
/// `rotateXYZ` op value directly avoids the round-trip entirely.
///
/// - **TrsOps**: empty stack or only translate/`rotateXYZ`/scale ops → raw op
///   values are exact (identity defaults for absent ops).
/// - **Matrix**: a lone `xformOp:transform` → the inspector decomposes/recomposes.
/// - **Unsupported**: inverse ops, pivots, `orient`, non-`rotateXYZ` rotates, or a
///   matrix mixed with other ops → the transform rows are read-only.
#[cfg(feature = "authoring")]
fn classify_authored_transform(
    ops: &[idtx_usd::AuthoredXformOp],
    diagnostic: Option<&str>,
) -> idtx_editor_model::AuthoredTransform {
    use idtx_editor_model::{AuthoredTransform, XformEditKind};
    use idtx_usd::XformOpType;

    let mut translation = [0.0_f64; 3];
    let mut rotation_xyz_degrees = [0.0_f64; 3];
    let mut scale = [1.0_f64; 3];

    let mut has_inverse_or_pivot = false;
    let mut has_matrix = false;
    let mut has_other_unsupported = false;
    let mut has_animated = false;
    for op in ops {
        if op.inverse || op.name.contains(":pivot") {
            has_inverse_or_pivot = true;
        }
        if !op.has_default {
            // Animated-only ops have no default value; `apply_transform_edit`
            // refuses them (needs an explicit time-edit policy).
            has_animated = true;
        }
        match op.op_type {
            XformOpType::Translate => translation = op.value,
            XformOpType::RotateXyz => rotation_xyz_degrees = op.value,
            XformOpType::Scale => scale = op.value,
            XformOpType::Unsupported => {
                if op.op_type_name == "transform" {
                    has_matrix = true;
                } else {
                    has_other_unsupported = true;
                }
            }
        }
    }

    let lone_matrix = has_matrix && ops.len() == 1 && !has_inverse_or_pivot && !has_animated;
    let kind = if lone_matrix {
        XformEditKind::Matrix
    } else if has_inverse_or_pivot || has_other_unsupported || has_matrix || has_animated {
        // `has_matrix` here means a transform op mixed with other ops.
        XformEditKind::Unsupported
    } else {
        XformEditKind::TrsOps
    };

    let diagnostic = (kind == XformEditKind::Unsupported).then(|| {
        diagnostic
            .map(str::to_owned)
            .unwrap_or_else(|| "transform op stack is not editable".to_owned())
    });

    AuthoredTransform {
        kind,
        translation,
        rotation_xyz_degrees,
        scale,
        diagnostic,
    }
}

/// Refreshes the material canvas graph source when the selection or topology changes.
///
/// Only fetches FFI data on a cache miss. When the selected prim has no material
/// binding (or no prim is selected), `tab.canvas_graph_source` is set to `None`.
pub(crate) fn refresh_canvas_graph_source(tab: &mut SessionTab) {
    let path = tab.session.selected_path().map(str::to_owned);
    let topo_gen = tab.session.topology_generation();

    // Cache hit: the same selection + topology was already processed. Keyed on
    // the *selected* prim path, not the resolved material path — those differ
    // whenever a material-bound geometry prim is selected, and comparing
    // against the material path would miss every frame and re-run the binding
    // resolution FFI (flooding the console with MaterialBindingAPI warnings).
    if tab.canvas_source_key.as_ref() == Some(&(path.clone(), topo_gen)) {
        return;
    }
    tab.canvas_source_key = Some((path.clone(), topo_gen));

    let Some(ref p) = path else {
        tab.canvas_graph_source = None;
        return;
    };

    let usd_graph = match tab.session.material_graph_for_path(p) {
        Ok(g) => g,
        Err(_) => {
            tab.canvas_graph_source = None;
            return;
        }
    };

    // If the bridge returned no material path the prim has no material binding.
    let Some(ref mat_path) = usd_graph.material_path else {
        tab.canvas_graph_source = None;
        return;
    };
    let mat_path = mat_path.clone();

    let material_properties = tab.session.inspect_prim(&mat_path).unwrap_or_default();

    let mut node_properties = std::collections::HashMap::new();
    for node in &usd_graph.nodes {
        if let Ok(props) = tab.session.inspect_prim(&node.path) {
            node_properties.insert(node.path.clone(), props);
        }
    }

    tab.canvas_graph_source = Some(Arc::new(CanvasGraphSource {
        material_path: mat_path,
        topology_generation: topo_gen,
        usd_graph,
        material_properties,
        node_properties,
    }));
}

/// Refreshes the compute graph source when the selection or topology changes.
///
/// Only fetches FFI data on a cache miss. When the selected prim has no
/// connected compute attribute (or no prim is selected), the bridge returns an
/// empty graph (`root_path == None`) and `tab.compute_graph_source` is `None`.
/// Keyed on the *selected* prim path (see [`refresh_canvas_graph_source`]).
pub(crate) fn refresh_compute_graph_source(tab: &mut SessionTab) {
    let path = tab.session.selected_path().map(str::to_owned);
    let topo_gen = tab.session.topology_generation();
    // The compute canvas shows composed constant inputs (`jsonPath`,
    // `jsonValueType`, …). Editing those bumps `change_generation` but not
    // topology, so key on both or the canvas keeps showing stale values.
    let change_gen = tab.session.change_generation();

    if tab.compute_source_key.as_ref() == Some(&(path.clone(), topo_gen, change_gen)) {
        return;
    }
    tab.compute_source_key = Some((path.clone(), topo_gen, change_gen));

    let Some(ref p) = path else {
        tab.compute_graph_source = None;
        return;
    };

    let usd_graph = match tab.session.compute_graph_for_path(p) {
        Ok(g) => g,
        Err(_) => {
            tab.compute_graph_source = None;
            return;
        }
    };

    // No connected attribute on the selected prim → no compute graph to show.
    let Some(ref root_path) = usd_graph.root_path else {
        tab.compute_graph_source = None;
        return;
    };
    let root_path = root_path.clone();

    tab.compute_graph_source = Some(Arc::new(ComputeGraphSource {
        root_path,
        topology_generation: topo_gen,
        usd_graph,
    }));
}

/// Fetch the per-frame gizmo transform data for the selected prim, if any.
///
/// Returns `None` when no prim is selected, authoring is disabled, the prim is
/// not editable, or the selected prim has no transform. All matrices are in
/// render world space (post `projection_basis`).
///
/// `authoring_editable` must mirror the inspector's editability gate (see
/// [`product_supports_authoring`]): the gizmo is only offered when the product
/// may author **and** the selected prim carries a root-layer opinion, so the
/// gizmo and the inspector transform fields appear together, never one without
/// the other.
pub(crate) fn build_gizmo_data(
    tab: &mut SessionTab,
    authoring_editable: bool,
) -> Option<GizmoTransformData> {
    // Interactive (authoring) builds only offer the gizmo for editable prims:
    // the product must support authoring and the prim must carry a root-layer
    // opinion, so the gizmo and the inspector transform fields appear together.
    //
    // Read-only builds (no `authoring` feature) show a display-only axis gizmo
    // for any selected xformable prim — no editability gate, since nothing can
    // be dragged or authored (see `gizmo::draw_axes_only`).
    #[cfg(feature = "authoring")]
    if !authoring_editable {
        return None;
    }
    #[cfg(not(feature = "authoring"))]
    let _ = authoring_editable;

    let path = tab.session.selected_path()?.to_owned();
    if !tab.session.prim_matches_viewport_purpose(&path) {
        return None;
    }

    // Only show the gizmo for xformable prims.  The Xformable ECS marker is set
    // at topology hydration time from prim.IsA<UsdGeomXformable>(), so this
    // covers all built-in and custom schema types without per-frame FFI.
    if !tab.session.is_prim_xformable(&path) {
        return None;
    }
    // Match the inspector: only editable (locally-authored) prims get an
    // interactive gizmo. The read-only display gizmo shows for any xformable.
    #[cfg(feature = "authoring")]
    if !prim_is_locally_authored(&mut tab.session, &path) {
        return None;
    }
    let basis = tab.session.projection_basis();
    let local_raw = tab.session.get_local_transform(&path)?;
    let world_raw = tab.session.get_world_transform(&path)?;
    let parent_world_raw = tab.session.get_parent_world_transform(&path).unwrap_or([
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ]);
    let (view_proj, _eye) = tab.session.camera_view_proj();
    let camera_radius = tab.session.camera_radius();

    let local_mat = gizmo::mat_from_usd(local_raw);
    let world_mat_usd = gizmo::mat_from_usd(world_raw);
    let parent_world_mat_usd = gizmo::mat_from_usd(parent_world_raw);
    let world_mat_render = basis * world_mat_usd;
    let parent_world_mat_render = basis * parent_world_mat_usd;

    Some(GizmoTransformData {
        selected_path: path,
        local_mat,
        world_mat_render,
        parent_world_mat_render,
        view_proj,
        camera_radius,
    })
}

///
/// `view_proj` is the camera's `projection * view` matrix (right-handed, wgpu depth [0,1]).
/// Returns the primary pick ray as `(origin, direction)` in world space, where
/// `origin` is the unprojected point on the near plane for this pixel and
/// `direction` is a unit vector pointing into the scene. On a degenerate matrix
/// the direction falls back to `Vec3::Z`.
///
/// Deriving the origin per-pixel from the near plane (rather than reusing the
/// single camera eye) is what makes picking correct under **orthographic**
/// projection — the orthographic axis views (Top/Front/Left/…) cast parallel
/// rays whose origin varies across the image plane. For perspective projection
/// the near point lies on the eye-ray, so the result is unchanged.
pub(crate) fn ray_from_viewport(
    pointer: egui::Pos2,
    rect: egui::Rect,
    view_proj: glam::Mat4,
) -> (glam::Vec3, glam::Vec3) {
    // Normalised position within the rect: [0,1] with top-left origin.
    let rel_x = ((pointer.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
    let rel_y = ((pointer.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);

    // NDC: x in [-1,1], y flipped (egui Y-down → NDC Y-up)
    let ndc_x = rel_x * 2.0 - 1.0;
    let ndc_y = 1.0 - rel_y * 2.0;

    let inv = view_proj.inverse();

    // Unproject near-plane point (z=0 in wgpu NDC = near plane)
    let near_h = inv * glam::Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
    // Unproject far-plane point (z=1)
    let far_h = inv * glam::Vec4::new(ndc_x, ndc_y, 1.0, 1.0);

    let near_w = near_h.truncate() / near_h.w;
    let far_w = far_h.truncate() / far_h.w;

    (near_w, (far_w - near_w).normalize_or_zero())
}

#[cfg(test)]
mod tests {
    use super::{inspector_mode_for_profile, product_supports_authoring};
    use idtx_editor_model::{InspectorMode, ProductProfile};

    mod applied_schemas {
        use super::super::applied_schemas_from_properties;
        use idtx_usd::{PropertyKind, PropertyRecord};

        fn api_record(summary: &str) -> PropertyRecord {
            PropertyRecord {
                name: "apiSchemas".to_owned(),
                kind: PropertyKind::Metadata,
                type_name: "token[]".to_owned(),
                value_summary: summary.to_owned(),
                authored: true,
                value_source: "composed prim metadata".to_owned(),
                time_sample_count: 0,
                provenance: Vec::new(),
                provenance_values: Vec::new(),
                relationship_targets: Vec::new(),
                root_authored: false,
            }
        }

        #[test]
        fn splits_comma_separated_summary_into_short_names() {
            let props = vec![api_record("ShadowAPI, MaterialBindingAPI")];
            assert_eq!(
                applied_schemas_from_properties(&props),
                vec!["ShadowAPI".to_owned(), "MaterialBindingAPI".to_owned()]
            );
        }

        #[test]
        fn single_schema_yields_one_entry() {
            let props = vec![api_record("ShadowAPI")];
            assert_eq!(
                applied_schemas_from_properties(&props),
                vec!["ShadowAPI".to_owned()]
            );
        }

        #[test]
        fn absent_record_yields_empty() {
            // No apiSchemas record → no applied schemas.
            assert!(applied_schemas_from_properties(&[]).is_empty());
        }

        #[test]
        fn blank_summary_yields_empty() {
            let props = vec![api_record("")];
            assert!(applied_schemas_from_properties(&props).is_empty());
        }
    }

    #[cfg(feature = "authoring")]
    mod classify {
        use super::super::classify_authored_transform;
        use idtx_editor_model::{AuthoredTransform, XformEditKind};
        use idtx_usd::{AuthoredXformOp, XformOpType, XformPrecision};

        fn op(name: &str, ty: XformOpType, type_name: &str, value: [f64; 3]) -> AuthoredXformOp {
            AuthoredXformOp {
                name: name.to_owned(),
                op_type: ty,
                op_type_name: type_name.to_owned(),
                precision: XformPrecision::Double,
                inverse: false,
                time_sample_count: 0,
                has_default: true,
                value,
            }
        }

        /// Classify a raw op list (the `AuthoredXformStack` wrapper needs a
        /// `PrimId`, which has no public constructor — the classifier takes the
        /// ops + diagnostic directly, so tests do too).
        fn classify(ops: Vec<AuthoredXformOp>, diagnostic: Option<&str>) -> AuthoredTransform {
            classify_authored_transform(&ops, diagnostic)
        }

        #[test]
        fn full_trs_stack_is_trsops_and_reads_rotate() {
            let a = classify(
                vec![
                    op(
                        "xformOp:translate",
                        XformOpType::Translate,
                        "translate",
                        [1.0, 2.0, 3.0],
                    ),
                    op(
                        "xformOp:rotateXYZ",
                        XformOpType::RotateXyz,
                        "rotateXYZ",
                        [0.0, 180.0, 0.0],
                    ),
                    op(
                        "xformOp:scale",
                        XformOpType::Scale,
                        "scale",
                        [1.0, 1.0, 1.0],
                    ),
                ],
                None,
            );
            assert_eq!(a.kind, XformEditKind::TrsOps);
            assert_eq!(a.rotation_xyz_degrees, [0.0, 180.0, 0.0]);
            assert_eq!(a.translation, [1.0, 2.0, 3.0]);
            assert!(a.diagnostic.is_none());
        }

        #[test]
        fn partial_rotate_only_stack_is_trsops() {
            // `apply_transform_edit` Strategy C creates missing ops, so a lone
            // rotateXYZ is still editable — and reads back exactly.
            let a = classify(
                vec![op(
                    "xformOp:rotateXYZ",
                    XformOpType::RotateXyz,
                    "rotateXYZ",
                    [0.0, 180.0, 0.0],
                )],
                Some("editable stacks require exactly one translate, rotateXYZ, and scale op"),
            );
            assert_eq!(a.kind, XformEditKind::TrsOps);
            assert_eq!(a.rotation_xyz_degrees, [0.0, 180.0, 0.0]);
        }

        #[test]
        fn empty_stack_is_trsops() {
            let a = classify(vec![], None);
            assert_eq!(a.kind, XformEditKind::TrsOps);
        }

        #[test]
        fn lone_matrix_is_matrix() {
            // The shim always fails the exact-TRS-order check for a matrix and so
            // sends the "authored order" diagnostic; a lone matrix with a default
            // must still classify as editable Matrix and drop that diagnostic.
            let a = classify(
                vec![op(
                    "xformOp:transform",
                    XformOpType::Unsupported,
                    "transform",
                    [0.0; 3],
                )],
                Some("editable stacks require translate, rotateXYZ, scale authored order"),
            );
            assert_eq!(a.kind, XformEditKind::Matrix);
            // Matrix kind carries no diagnostic — the inspector decomposes it.
            assert!(a.diagnostic.is_none());
        }

        #[test]
        fn animated_only_matrix_is_unsupported() {
            // A matrix op with no default (time-samples only) has `has_default =
            // false`; unlike a defaulted matrix it must stay read-only. This guards
            // the shim fix that probes GfMatrix4d — the has_default signal is what
            // separates an editable matrix from a genuinely animated one.
            let mut m = op(
                "xformOp:transform",
                XformOpType::Unsupported,
                "transform",
                [0.0; 3],
            );
            m.has_default = false;
            let a = classify(
                vec![m],
                Some("animated-only transform ops require an explicit time-edit policy"),
            );
            assert_eq!(a.kind, XformEditKind::Unsupported);
            assert!(a.diagnostic.is_some());
        }

        #[test]
        fn pivot_stack_is_unsupported_with_diagnostic() {
            let mut inv = op(
                "xformOp:translate:pivot",
                XformOpType::Translate,
                "translate",
                [0.0; 3],
            );
            inv.name = "xformOp:translate:pivot".to_owned();
            let a = classify(
                vec![
                    op(
                        "xformOp:translate:pivot",
                        XformOpType::Translate,
                        "translate",
                        [1.0; 3],
                    ),
                    op(
                        "xformOp:rotateXYZ",
                        XformOpType::RotateXyz,
                        "rotateXYZ",
                        [0.0; 3],
                    ),
                    inv,
                ],
                Some("pivot transform ops are not editable"),
            );
            assert_eq!(a.kind, XformEditKind::Unsupported);
            assert!(a.diagnostic.is_some());
        }

        #[test]
        fn orient_op_is_unsupported() {
            let a = classify(
                vec![op(
                    "xformOp:orient",
                    XformOpType::Unsupported,
                    "orient",
                    [0.0; 3],
                )],
                Some("matrix, pivot, orient, and non-rotateXYZ ops are not editable"),
            );
            assert_eq!(a.kind, XformEditKind::Unsupported);
        }

        #[test]
        fn inverse_op_is_unsupported() {
            let mut inv = op(
                "xformOp:translate",
                XformOpType::Translate,
                "translate",
                [1.0; 3],
            );
            inv.inverse = true;
            let a = classify(vec![inv], Some("inverse transform ops are not editable"));
            assert_eq!(a.kind, XformEditKind::Unsupported);
        }

        #[test]
        fn animated_op_is_unsupported() {
            let mut anim = op(
                "xformOp:rotateXYZ",
                XformOpType::RotateXyz,
                "rotateXYZ",
                [0.0; 3],
            );
            anim.has_default = false;
            anim.time_sample_count = 10;
            let a = classify(
                vec![anim],
                Some("animated-only transform ops require an explicit time-edit policy"),
            );
            assert_eq!(a.kind, XformEditKind::Unsupported);
        }

        #[test]
        fn matrix_mixed_with_ops_is_unsupported() {
            let a = classify(
                vec![
                    op(
                        "xformOp:transform",
                        XformOpType::Unsupported,
                        "transform",
                        [0.0; 3],
                    ),
                    op("xformOp:scale", XformOpType::Scale, "scale", [1.0; 3]),
                ],
                Some("matrix, pivot, orient, and non-rotateXYZ ops are not editable"),
            );
            assert_eq!(a.kind, XformEditKind::Unsupported);
        }
    }

    // The read-only Inspector never authors, regardless of build features.
    #[test]
    fn inspector_is_never_authoring() {
        assert!(!product_supports_authoring(ProductProfile::INSPECTOR));
        assert_eq!(
            inspector_mode_for_profile(ProductProfile::INSPECTOR),
            InspectorMode::ReadOnlyWithVariants
        );
    }

    // With `authoring`, both Poser and Conductor are editable. Only Conductor
    // uses Cooking mode, and only when the `cooking` feature is also enabled.
    #[cfg(feature = "authoring")]
    #[test]
    fn authoring_products_are_editable_with_feature() {
        for profile in [ProductProfile::POSER, ProductProfile::CONDUCTOR] {
            assert!(product_supports_authoring(profile));
        }

        assert_eq!(
            inspector_mode_for_profile(ProductProfile::POSER),
            InspectorMode::Authoring
        );

        assert_eq!(
            inspector_mode_for_profile(ProductProfile::CONDUCTOR),
            if cfg!(feature = "cooking") {
                InspectorMode::Cooking
            } else {
                InspectorMode::Authoring
            }
        );
    }

    // Without the `authoring` feature no product is editable, so no field
    // renders as an interactive (but dead) widget.
    #[cfg(not(feature = "authoring"))]
    #[test]
    fn no_product_is_editable_without_feature() {
        for profile in [
            ProductProfile::INSPECTOR,
            ProductProfile::POSER,
            ProductProfile::CONDUCTOR,
        ] {
            assert!(!product_supports_authoring(profile));
            assert_eq!(
                inspector_mode_for_profile(profile),
                InspectorMode::ReadOnlyWithVariants
            );
        }
    }

    // ── build_layer_tree unit tests ─────────────────────────────────────────────

    use super::build_layer_tree;
    use idtx_editor_model::{LayerArc, LayerInfo};

    fn make_layer(id: &str, parent: &str, arc: LayerArc) -> LayerInfo {
        LayerInfo {
            identifier: id.to_owned(),
            display_name: id.to_owned(),
            real_path: None,
            arc,
            arc_source_path: None,
            arc_target_path: None,
            has_specs: true,
            is_loaded: true,
            depth: 0,
            offset: 0.0,
            scale: 1.0,
            delta_count: 1,
            is_anonymous: false,
            is_dirty: false,
            is_muted: false,
            is_edit_target: false,
            parent_identifier: parent.to_owned(),
        }
    }

    #[test]
    fn build_layer_tree_assigns_depth_from_parent_chain() {
        // root → sub → leaf
        let layers = vec![
            make_layer("session", "", LayerArc::Session),
            make_layer("root", "", LayerArc::Root),
            make_layer("sub", "root", LayerArc::SubLayer),
            make_layer("leaf", "sub", LayerArc::SubLayer),
        ];
        let out = build_layer_tree(layers);
        let depth_of = |id: &str| out.iter().find(|l| l.identifier == id).unwrap().depth;
        assert_eq!(depth_of("session"), 0);
        assert_eq!(depth_of("root"), 0);
        assert_eq!(depth_of("sub"), 1);
        assert_eq!(depth_of("leaf"), 2);
    }

    #[test]
    fn build_layer_tree_pre_order_parent_before_children() {
        let layers = vec![
            make_layer("root", "", LayerArc::Root),
            make_layer("child_b", "root", LayerArc::Payload),
            make_layer("child_a", "root", LayerArc::Payload),
        ];
        let out = build_layer_tree(layers);
        let pos = |id: &str| out.iter().position(|l| l.identifier == id).unwrap();
        assert!(pos("root") < pos("child_a"), "root must precede child_a");
        assert!(pos("root") < pos("child_b"), "root must precede child_b");
    }

    #[test]
    fn build_layer_tree_nested_payload_under_outer_not_root() {
        // Mirrors the layerstack_hier fixture hierarchy:
        //   root → outer (payload) → inner (payload)
        // inner's depth must be 2, not 1.
        let layers = vec![
            make_layer("root", "", LayerArc::Root),
            make_layer("outer", "root", LayerArc::Payload),
            make_layer("inner", "outer", LayerArc::Payload),
        ];
        let out = build_layer_tree(layers);
        let depth_of = |id: &str| out.iter().find(|l| l.identifier == id).unwrap().depth;
        assert_eq!(depth_of("root"), 0);
        assert_eq!(depth_of("outer"), 1);
        assert_eq!(depth_of("inner"), 2);

        let pos = |id: &str| out.iter().position(|l| l.identifier == id).unwrap();
        assert!(pos("outer") < pos("inner"), "outer must precede inner");
    }

    #[test]
    fn build_layer_tree_same_layer_appears_under_two_parents() {
        // A shared layer referenced by two different parents should appear twice.
        let layers = vec![
            make_layer("root", "", LayerArc::Root),
            make_layer("parent_a", "root", LayerArc::Payload),
            make_layer("parent_b", "root", LayerArc::Payload),
            make_layer("shared", "parent_a", LayerArc::Payload),
            make_layer("shared", "parent_b", LayerArc::Payload),
        ];
        let out = build_layer_tree(layers);
        let count = out.iter().filter(|l| l.identifier == "shared").count();
        // build_layer_tree preserves the input; duplicate (parent_a,shared) and
        // (parent_b,shared) records both appear exactly once.
        assert_eq!(count, 2, "shared layer should appear under both parents");
    }

    #[test]
    fn build_layer_tree_orphan_appended_at_depth_one() {
        // A record whose parent is not in the list is an orphan and appended at depth 1.
        let layers = vec![
            make_layer("root", "", LayerArc::Root),
            make_layer("orphan", "missing_parent", LayerArc::Payload),
        ];
        let out = build_layer_tree(layers);
        let orphan = out.iter().find(|l| l.identifier == "orphan").unwrap();
        assert_eq!(orphan.depth, 1, "orphan depth should be 1");
    }

    mod ray_from_viewport {
        use super::super::ray_from_viewport;
        use glam::{Mat4, Vec3};

        const EYE: Vec3 = Vec3::new(0.0, 0.0, 5.0);
        const TARGET: Vec3 = Vec3::ZERO;

        fn rect() -> egui::Rect {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0))
        }

        fn view() -> Mat4 {
            Mat4::look_at_rh(EYE, TARGET, Vec3::Y)
        }

        // wgpu depth [0,1] right-handed, matching OrbitCamera::to_camera_desc.
        fn perspective() -> Mat4 {
            Mat4::perspective_rh(45f32.to_radians(), 800.0 / 600.0, 0.1, 100.0) * view()
        }

        fn orthographic() -> Mat4 {
            let half_h = 2.0;
            let half_w = half_h * (800.0 / 600.0);
            Mat4::orthographic_rh(-half_w, half_w, -half_h, half_h, -100.0, 100.0) * view()
        }

        #[test]
        fn perspective_origin_lies_on_eye_ray() {
            // For perspective, every primary ray passes through the eye: the
            // near-plane origin plus the direction extrapolated back must hit the
            // eye point regardless of pixel.
            for px in [egui::pos2(100.0, 150.0), egui::pos2(700.0, 500.0)] {
                let (origin, dir) = ray_from_viewport(px, rect(), perspective());
                // Distance from origin back to eye along -dir.
                let to_eye = EYE - origin;
                let t = to_eye.dot(dir);
                let closest = origin + dir * t;
                assert!(
                    (closest - EYE).length() < 1e-3,
                    "perspective ray should pass through the eye; off by {}",
                    (closest - EYE).length()
                );
            }
        }

        #[test]
        fn orthographic_origin_varies_per_pixel_direction_constant() {
            // The regression: in an orthographic axis view the primary rays are
            // parallel (constant direction) but each originates at a distinct
            // point on the image plane.  A fixed eye origin would make every click
            // cast the same ray.
            let (origin_a, dir_a) = ray_from_viewport(egui::pos2(100.0, 150.0), rect(), orthographic());
            let (origin_b, dir_b) = ray_from_viewport(egui::pos2(700.0, 500.0), rect(), orthographic());

            assert!(
                (dir_a - dir_b).length() < 1e-4,
                "orthographic rays must be parallel"
            );
            assert!(
                (origin_a - origin_b).length() > 1.0,
                "orthographic ray origins must differ across the image plane; got {} and {}",
                origin_a,
                origin_b
            );
        }

        #[test]
        fn orthographic_center_ray_hits_target() {
            // A click at the viewport centre in a front ortho view should produce a
            // ray that passes through the pivot/target.
            let center = egui::pos2(400.0, 300.0);
            let (origin, dir) = ray_from_viewport(center, rect(), orthographic());
            let to_target = TARGET - origin;
            let t = to_target.dot(dir);
            let closest = origin + dir * t;
            assert!(
                (closest - TARGET).length() < 1e-3,
                "centre ortho ray should pass through the target; off by {}",
                (closest - TARGET).length()
            );
        }
    }
}
