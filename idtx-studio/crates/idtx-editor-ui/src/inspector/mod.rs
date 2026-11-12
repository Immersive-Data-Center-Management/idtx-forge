//! Extensible prim property inspector for IDTX Studio.
//!
//! # Architecture
//!
//! The inspector is driven by a registry of [`InspectorContribution`] trait
//! objects, sorted by [`InspectorContribution::priority`].  Each contribution
//! decides whether it is relevant for the current prim via [`present`] and
//! renders its section via [`draw`].  A [`PropertyClaimSet`] prevents the
//! same USD attribute from appearing in two panels: specific panels claim the
//! properties they handle; the built-in [`FallbackPanel`] renders everything
//! that remains unclaimed.
//!
//! # Extending the inspector
//!
//! Register a custom contribution **at app startup**, before any stage opens:
//!
//! ```rust,ignore
//! registry.register(Arc::new(MyPanel));
//! ```
//!
//! `MyPanel` implements [`InspectorContribution`] and can gate on
//! `ctx.info.type_name`, `ctx.info.applied_schemas`, or `ctx.properties`.
//! Claiming properties in `draw()` prevents them from appearing in the
//! fallback panel.
//!
//! # Authoring
//!
//! When the shell passes [`InspectorMode::Authoring`] (Poser product), panels
//! that called [`PropertyClaimSet::claim_editable`] will see
//! [`InspectorContext::is_property_editable`] return `true` for those properties.
//! Combined with [`InspectorContext::is_prim_editable`] (which also checks that
//! the prim has a root-layer opinion so packed-stage prims stay read-only),
//! panels show interactive widgets.  Changes are dispatched via
//! [`InspectorContext::author_attribute`] / [`InspectorContext::author_transform`],
//! which forward through [`EditSink`] to the shell.

pub mod panels;
pub mod widgets;

use std::{collections::HashSet, sync::Arc};

use egui::Ui;
use idtx_editor_model::{AttributeValue, HullGenParams, InspectorMode, PrimInfo, TransformEdit};
use idtx_usd::{PropertyKind, PropertyRecord, SkeletonJointRecord};

// ── PropertyClaimSet ──────────────────────────────────────────────────────────

/// Tracks which USD property names have been "claimed" by a specific panel,
/// and which of those are also marked as editable in [`InspectorMode::Authoring`].
///
/// Panels call [`claim`] or [`claim_prefix`] during their `claim()` to mark
/// properties they handle.  Calling [`claim_editable`] / [`claim_prefix_editable`]
/// additionally marks the property as user-editable when the prim is editable.
/// The [`FallbackPanel`] then renders only the unclaimed authored properties so
/// nothing is silently dropped.
#[derive(Default, Debug)]
pub struct PropertyClaimSet {
    claimed: HashSet<String>,
    claimed_prefix: HashSet<String>,
    editable: HashSet<String>,
    editable_prefix: HashSet<String>,
}

impl PropertyClaimSet {
    fn new() -> Self {
        Self {
            claimed: HashSet::new(),
            claimed_prefix: HashSet::new(),
            editable: HashSet::new(),
            editable_prefix: HashSet::new(),
        }
    }

    /// Claim a single property by exact name (read-only display).
    pub fn claim(&mut self, name: &str) {
        self.claimed.insert(name.to_owned());
    }

    /// Claim every property whose name starts with `prefix` (read-only display).
    pub fn claim_prefix(&mut self, prefix: &str) {
        self.claimed_prefix.insert(prefix.to_owned());
    }

    /// Claim a single property by exact name **and** mark it as editable in
    /// [`InspectorMode::Authoring`].
    pub fn claim_editable(&mut self, name: &str) {
        self.claimed.insert(name.to_owned());
        self.editable.insert(name.to_owned());
    }

    /// Claim every property whose name starts with `prefix` **and** mark all
    /// of them as editable in [`InspectorMode::Authoring`].
    pub fn claim_prefix_editable(&mut self, prefix: &str) {
        self.claimed_prefix.insert(prefix.to_owned());
        self.editable_prefix.insert(prefix.to_owned());
    }

    /// Returns `true` when `name` has already been claimed.
    /// Either direct or based on the prefix
    #[must_use]
    pub fn is_claimed(&self, name: &str) -> bool {
        self.claimed.contains(name)
            || self
                .claimed_prefix
                .iter()
                .any(|p| name.starts_with(p.as_str()))
    }

    /// Returns `true` when `name` was claimed via [`claim_editable`] or
    /// [`claim_prefix_editable`].  Combine with
    /// [`InspectorContext::is_prim_editable`] to decide whether to show an
    /// interactive widget.
    #[must_use]
    pub fn is_property_editable(&self, name: &str) -> bool {
        self.editable.contains(name)
            || self
                .editable_prefix
                .iter()
                .any(|p| name.starts_with(p.as_str()))
    }

    /// All authored properties that have not yet been claimed, in original order.
    #[must_use]
    pub fn unclaimed_authored<'a>(&self, props: &'a [PropertyRecord]) -> Vec<&'a PropertyRecord> {
        props
            .iter()
            .filter(|p| p.authored && !self.is_claimed(&p.name))
            .collect()
    }
}

// ── InspectorContext ──────────────────────────────────────────────────────────

/// Everything an [`InspectorContribution`] needs to decide `present()` and
/// render `draw()`.
///
/// Passed by `&mut` reference because `claims` is mutable; `info` and
/// `properties` are immutable borrows of shell-owned data.
#[allow(missing_debug_implementations)]
pub struct InspectorContext<'a> {
    /// Lightweight prim descriptor built by the shell from the ECS world.
    pub info: &'a PrimInfo,
    /// Full list of authored (and non-authored) property records from
    /// `inspect_prim()`.  Panels read this to display attribute values.
    pub properties: &'a [PropertyRecord],
    pub mode: InspectorMode,
    /// Write sink for prim authoring.  `None` in read-only contexts.
    pub sink: Option<&'a mut dyn EditSink>,
    /// Skeleton joint records when the selected prim is a `Skeleton` or `SkelRoot`.
    /// `None` for all other prim types.
    pub skeleton_joints: Option<&'a [SkeletonJointRecord]>,
    /// `true` when a collision convex-hull generation job is currently running
    /// for this prim (shell-owned transient state). The collision panel swaps
    /// its **Generate `CollisionHull`** button for an animated spinner while set.
    pub collision_hull_generating: bool,
}

impl InspectorContext<'_> {
    /// Returns `true` when variant-set combo boxes should be interactive.
    #[must_use]
    pub fn is_editable(&self) -> bool {
        matches!(
            self.mode,
            InspectorMode::ReadOnlyWithVariants | InspectorMode::Authoring | InspectorMode::Cooking
        )
    }

    /// Returns `true` when full attribute editing should be enabled for this
    /// prim.
    ///
    /// Two conditions must both hold:
    /// 1. The inspector mode is [`InspectorMode::Authoring`].
    /// 2. The prim has a root-layer opinion (`is_locally_authored`).  This
    ///    keeps packed-stage prims (pulled in via references) read-only unless
    ///    the user has already authored an override on the root layer.
    #[must_use]
    pub fn is_prim_editable(&self) -> bool {
        matches!(self.mode, InspectorMode::Authoring | InspectorMode::Cooking)
            && self.info.is_locally_authored
    }

    /// Returns `true` when cooking is supported.
    #[must_use]
    pub fn is_cookable(&self) -> bool {
        matches!(self.mode, InspectorMode::Cooking)
    }

    /// Invokes `f` with the prim path and the write sink when variant editing
    /// is available.  A no-op when mode is `ReadOnly` or no sink is provided.
    pub fn with_variant_selection(&mut self, f: impl FnOnce(&str, &mut dyn EditSink)) {
        // Extract independent fields before the mutable borrow of `self.sink`.
        // Variant combos are interactive in every mode that `is_editable`
        // reports — crucially including `Cooking` (Conductor), which this match
        // previously omitted, leaving variant switching a silent no-op there.
        let editable = self.is_editable();
        let path = self.info.path.as_str();
        if let Some(sink) = self.sink.as_deref_mut()
            && editable
        {
            f(path, sink);
        }
    }

    /// Author a typed attribute value on the root layer.
    ///
    /// `before` is the previous value at call time (for undo); pass `None`
    /// when the attribute was not previously authored.
    /// No-op when there is no sink or the prim is not editable.
    pub fn author_attribute(
        &mut self,
        property: &str,
        before: Option<AttributeValue>,
        value: AttributeValue,
    ) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.author_attribute(&path, property, before, value);
        }
    }

    pub fn commit_attribute(
        &mut self,
        property: &str,
        before: Option<AttributeValue>,
        value: AttributeValue,
    ) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.commit_attribute(&path, property, before, value);
        }
    }

    /// Author a full TRS transform edit on the root layer.
    ///
    /// The session implementation handles xformOp-aware routing (matrix vs.
    /// individual translate/rotate/scale ops).  No-op when there is no sink or
    /// the prim is not editable.
    pub fn author_transform(&mut self, edit: TransformEdit) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.author_transform(&path, edit);
        }
    }

    /// Commit a confirmed transform edit to the undo journal.
    ///
    /// Call this once when a transform field is *confirmed* (its focus is lost
    /// or Enter is pressed, or a drag ends) — after the live value has already
    /// been streamed to USD via [`author_transform`]. This produces a single
    /// journal entry for the whole edit gesture instead of one per intermediate
    /// value.
    ///
    /// `before` is the prim's local matrix captured when editing began (USD
    /// row-major `[[f32; 4]; 4]`); `after` is the confirmed TRS. `before_trs` is
    /// the prim's authored Euler TRS captured when editing began, `Some` only for
    /// `rotateXYZ`-op prims — it lets the journal restore the exact angles on undo
    /// instead of re-decomposing the matrix (which folds a middle-axis rotation
    /// like `Y = 180°` onto the other axes). No-op when the prim is not editable
    /// or there is no sink.
    pub fn commit_transform(
        &mut self,
        before: [[f32; 4]; 4],
        after: TransformEdit,
        before_trs: Option<TransformEdit>,
    ) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.commit_transform(&path, before, after, before_trs);
        }
    }

    /// Looks up a property by exact name.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&PropertyRecord> {
        self.properties.iter().find(|p| p.name == name)
    }

    /// All properties whose names start with `prefix`.
    #[must_use]
    pub fn properties_with_prefix(&self, prefix: &str) -> Vec<&PropertyRecord> {
        self.properties
            .iter()
            .filter(|p| p.name.starts_with(prefix))
            .collect()
    }

    /// Route a behavior action to the sink for the current prim, gated on
    /// [`Self::is_prim_editable`]. A no-op with no sink, outside authoring, or
    /// when the owning prim has no local opinion (so behaviors on a weaker-layer
    /// prim stay read-only until the author creates an override from the scene
    /// tree). The one carve-out is `overwrite_behavior`, which is only offered
    /// once an override exists — so it too is correctly gated here.
    fn with_behavior_sink(&mut self, f: impl FnOnce(&str, &mut dyn EditSink)) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            f(&path, sink);
        }
    }

    /// Request attaching a behavior to the selected prim.
    pub fn attach_behavior(&mut self) {
        self.with_behavior_sink(|path, sink| sink.attach_behavior(path));
    }

    /// Create a new source-authored behavior for the selected prim (Conductor
    /// "Create Behavior"): writes a template `.rs` + authors the source triple.
    /// The `.wasm` is compiled before preview / at packaging, not here.
    pub fn create_behavior(&mut self) {
        self.with_behavior_sink(|path, sink| sink.create_behavior(path));
    }

    /// Overwrite the selected prim's behavior into the current edit layer
    /// (Conductor "Overwrite Behavior"): carries the composed manifest config
    /// over and creates a fresh source + new uuid, re-applying `IdtxBehaviorAPI`
    /// into the local `over`. Offered when the behavior lives in a weaker layer
    /// (the `over` authors nothing for it) or the `over` has detached the API.
    pub fn overwrite_behavior(&mut self) {
        self.with_behavior_sink(|path, sink| sink.overwrite_behavior(path));
    }

    /// Detach the behavior from the selected prim.
    pub fn detach_behavior(&mut self) {
        self.with_behavior_sink(|path, sink| sink.detach_behavior(path));
    }

    /// Open the selected prim's behavior source in VS Code — edit only.
    pub fn edit_behavior_source(&mut self) {
        self.with_behavior_sink(|path, sink| sink.edit_behavior_source(path));
    }

    /// Compile-check the selected prim's behavior — never runs it.
    pub fn check_behavior(&mut self) {
        self.with_behavior_sink(|path, sink| sink.check_behavior(path));
    }

    /// Build + link-verify the selected prim's behavior against its authored
    /// capabilities, reporting required / missing / unused buckets — never runs it.
    pub fn verify_behavior(&mut self) {
        self.with_behavior_sink(|path, sink| sink.verify_behavior(path));
    }

    /// Enable/disable the selected prim's behavior.
    pub fn set_behavior_enabled(&mut self, enabled: bool) {
        self.with_behavior_sink(move |path, sink| sink.set_behavior_enabled(path, enabled));
    }

    /// Set the selected prim's behavior execution kind.
    ///
    /// `kind` is a token — `"runtime"` or `"editor"` — kept as a `&str` so this
    /// crate need not depend on `idtx-behavior-abi`.
    pub fn set_behavior_kind(&mut self, kind: &str) {
        let kind = kind.to_owned();
        self.with_behavior_sink(move |path, sink| sink.set_behavior_kind(path, &kind));
    }

    /// Set the selected prim's behavior scope.
    ///
    /// `scope` is a token — `"entity"` or `"stage"` — kept as a `&str` so this
    /// crate need not depend on `idtx-scene`.
    pub fn set_behavior_scope(&mut self, scope: &str) {
        let scope = scope.to_owned();
        self.with_behavior_sink(move |path, sink| sink.set_behavior_scope(path, &scope));
    }

    /// Set whether the selected prim's behavior is server-authoritative.
    pub fn set_behavior_server_authoritative(&mut self, server_authoritative: bool) {
        self.with_behavior_sink(move |path, sink| {
            sink.set_behavior_server_authoritative(path, server_authoritative);
        });
    }

    /// Author an arbitrary behavior-config attribute on the selected prim's
    /// behavior (capabilities/inputs token arrays, version, ship, memory cap).
    /// The generic channel behind the behavior-config controls.
    pub fn set_behavior_property(&mut self, property: &str, value: idtx_editor_model::AttributeValue) {
        let property = property.to_owned();
        self.with_behavior_sink(move |path, sink| sink.set_behavior_property(path, &property, value));
    }

    /// The USD schema type name of the selected prim (e.g. `"Mesh"`, `"Camera"`).
    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.info.type_name
    }

    /// Returns `true` when `schema` appears in the prim's applied schemas.
    #[must_use]
    pub fn has_applied_schema(&self, schema: &str) -> bool {
        self.info.applied_schemas.iter().any(|s| s == schema)
    }

    /// Returns `true` when any authored property has a `Relationship` kind.
    #[must_use]
    pub fn has_authored_relationships(&self) -> bool {
        self.properties
            .iter()
            .any(|p| p.authored && matches!(p.kind, PropertyKind::Relationship))
    }

    /// Apply a single-apply API schema (e.g. `"ShadowAPI"`) to the selected
    /// prim. Forwards to the sink; no-op when the prim is not editable or there
    /// is no sink. Mirrors [`Self::author_attribute`].
    pub fn apply_api_schema(&mut self, schema: &str) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.apply_api_schema(&path, schema);
        }
    }

    /// Remove an API schema (e.g. `"ShadowAPI"`) from the selected
    /// prim. Forwards to the sink; no-op when the prim is not editable or there
    /// is no sink. Mirrors [`Self::author_attribute`].
    pub fn remove_api_schema(&mut self, schema: &str) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.remove_api_schema(&path, schema);
        }
    }

    /// Apply the `IdtxCollisionAPI` schema to the selected prim (the collision
    /// inspector's **Add Collision** button). Conductor-only: gated on
    /// [`Self::is_cookable`] in addition to editability, since all collision
    /// authoring is restricted to `idtx-conductor`. No-op otherwise.
    pub fn apply_collision_api(&mut self) {
        if !self.is_cookable() || !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.apply_collision_api(&path);
        }
    }

    /// Remove the collider schemas from the selected prim and clear their
    /// authored opinions (the collision inspector's **Remove Collision** button,
    /// the counterpart of [`Self::apply_collision_api`]). Conductor-only: gated
    /// on [`Self::is_cookable`] in addition to editability. No-op otherwise.
    pub fn remove_collision_api(&mut self) {
        if !self.is_cookable() || !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.remove_collision_api(&path);
        }
    }

    /// Request collision convex-hull generation for the selected prim (the
    /// collision inspector's **Generate `CollisionHull`** button). `params`
    /// carries the fit knobs from the panel. Conductor-only; no-op otherwise.
    pub fn generate_collision_hull(&mut self, params: HullGenParams) {
        if !self.is_cookable() || !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.generate_collision_hull(&path, params);
        }
    }

    /// Remove the root-layer opinion of `property` on the selected prim.
    ///
    /// Forwards to the sink; no-op when the prim is not editable or there is no
    /// sink. Mirrors [`Self::author_attribute`]. The button that calls this is
    /// itself gated on `PropertyRecord::root_authored`, so only an existing
    /// root-layer opinion is ever removed.
    pub fn remove_property_opinion(&mut self, property: &str) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.remove_property_opinion(&path, property);
        }
    }

    /// Remove every root-layer transform opinion in `properties` on the selected
    /// prim ("Reset Transform"). Forwards to the sink; no-op when the prim is
    /// not editable or there is no sink.
    pub fn reset_transform(&mut self, properties: Vec<String>) {
        if !self.is_prim_editable() {
            return;
        }
        let path = self.info.path.clone();
        if let Some(sink) = self.sink.as_deref_mut() {
            sink.reset_transform(&path, properties);
        }
    }
}

// ── EditSink ──────────────────────────────────────────────────────────────────

/// Write sink for prim authoring.
///
/// Passed via [`InspectorContext::sink`] to panels that need to author USD.
/// Provide a concrete implementation in the shell; the default no-op impls
/// make it safe to call from any panel regardless of the active mode.
pub trait EditSink {
    /// Author a variant selection on the session layer.
    ///
    /// `path` is the absolute `SdfPath` of the prim.  `set_name` identifies the
    /// variant set; `before` is the previous selection (for undo), `after` is
    /// the new selection.
    fn set_variant_selection(&mut self, path: &str, set_name: &str, before: &str, after: &str) {
        let _ = (path, set_name, before, after);
    }

    /// Author a typed attribute value on the root layer for the prim at `path`.
    ///
    /// `before` captures the previous value at call time (for undo); pass `None`
    /// when the attribute was not previously authored.  The shell dispatches this
    /// to the session's `author_attribute` method, which calls
    /// `apply_structural_command(SetProperty { … })`.  The resulting USD change
    /// notification flows through `tick()` → ECS update → render invalidation.
    fn author_attribute(
        &mut self,
        path: &str,
        property: &str,
        before: Option<AttributeValue>,
        value: AttributeValue,
    ) {
        let _ = (path, property, before, value);
    }

    /// Commit a confirmed attribute edit (`before` → `after`) to the undo
    /// journal for the prim at `path`.
    fn commit_attribute(
        &mut self,
        path: &str,
        property: &str,
        before: Option<AttributeValue>,
        value: AttributeValue,
    ) {
        let _ = (path, property, before, value);
    }

    /// Author a full TRS transform edit on the root layer for the prim at `path`.
    ///
    /// The session inspects the prim's existing xformOps (matrix vs.
    /// individual translate/rotate/scale ops) and authors the appropriate
    /// property, then fires a USD change notification that flows through
    /// `tick()` → ECS update → render invalidation automatically.
    fn author_transform(&mut self, path: &str, edit: TransformEdit) {
        let _ = (path, edit);
    }

    /// Commit a confirmed transform edit (`before` → `after`) to the undo
    /// journal for the prim at `path`.
    ///
    /// Called once when a transform edit is confirmed (field focus lost, Enter,
    /// or drag release), so a drag or multi-keystroke edit yields a single
    /// journal entry rather than one per intermediate value. `before` is the
    /// pre-edit local matrix (USD row-major); `after` is the confirmed TRS.
    /// `before_trs` is the pre-edit authored Euler TRS, `Some` only for
    /// `rotateXYZ`-op prims, so undo restores the exact angles instead of
    /// decomposing the matrix. The shell re-authors `after` (idempotent) and
    /// records the pair for undo. Default no-op.
    fn commit_transform(
        &mut self,
        path: &str,
        before: [[f32; 4]; 4],
        after: TransformEdit,
        before_trs: Option<TransformEdit>,
    ) {
        let _ = (path, before, after, before_trs);
    }

    // ── Behavior authoring ──────────────────────────────────────────
    //
    // The behavior inspector panel routes its actions through these methods.
    // All default to no-ops so a read-only shell (or a product without the
    // `behavior_authoring` capability) can ignore them. None of them run a
    // behavior — playback/run stays owned by the transport/session, never the
    // inspector.

    /// Request attaching a behavior to the prim at `path`. The shell lets the
    /// author pick a Rust `.rs` source, copies it into the project's `behaviors/`
    /// folder, and authors the `IdtxBehaviorAPI` manifest scaffold
    /// (schema + source/uuid/kind + name + version) via
    /// `StudioSession::attach_behavior_source`. The compiled `.wasm` is produced
    /// before preview / at packaging, not here.
    fn attach_behavior(&mut self, path: &str) {
        let _ = path;
    }

    /// Create a new **source-authored** behavior for the prim at `path`: the
    /// shell writes a template `.rs` (+ manifest) into the project's `behaviors/`
    /// folder and applies `IdtxBehaviorAPI` + authors the source triple
    /// (`idtx:behavior:source`/`:uuid`/`:kind`)
    /// via `StudioSession::create_behavior_source`. The `.wasm` is compiled
    /// before preview / at packaging, not here.
    fn create_behavior(&mut self, path: &str) {
        let _ = path;
    }

    /// Overwrite the behavior on the prim at `path` into the current edit layer:
    /// carry its composed manifest config over and create a fresh source + new
    /// uuid, re-applying `IdtxBehaviorAPI` into the `over`
    /// (`behavior_tools::overwrite_behavior`). Offered when the behavior lives in
    /// a weaker layer (the `over` authors nothing for it) or the `over` has
    /// detached the API and only lingering weaker-layer attributes remain.
    fn overwrite_behavior(&mut self, path: &str) {
        let _ = path;
    }

    /// Detach the behavior from the prim at `path`
    /// (`StudioSession::detach_behavior`).
    fn detach_behavior(&mut self, path: &str) {
        let _ = path;
    }

    /// Open the behavior's Rust **source** for the prim at `path` in an external
    /// editor (VS Code). Editing only — never compiles or runs.
    fn edit_behavior_source(&mut self, path: &str) {
        let _ = path;
    }

    /// Compile-check the behavior attached to the prim at `path`
    /// (`cargo check --target wasm32-…`), surfacing errors. Does not run it.
    fn check_behavior(&mut self, path: &str) {
        let _ = path;
    }

    /// Build the behavior at `path` and verify it would link against its
    /// authored capabilities, reporting required / missing / unused buckets
    /// (`idtx_behavior::verify_link`). Does not run it.
    fn verify_behavior(&mut self, path: &str) {
        let _ = path;
    }

    /// Enable or disable the behavior attached to the prim at `path`
    /// (`StudioSession::set_behavior_enabled`). Disabling drops it from the tick
    /// loop without detaching it.
    fn set_behavior_enabled(&mut self, path: &str, enabled: bool) {
        let _ = (path, enabled);
    }

    /// Set the execution kind of the behavior attached to the prim at `path`
    /// (`StudioSession::set_behavior_kind`). `kind` is a token — `"runtime"` or
    /// `"editor"` — passed as a `&str` so `idtx-editor-ui` need not depend on
    /// `idtx-behavior-abi`. Runtime behaviors tick in the packaged player;
    /// editor behaviors tick only in the authoring session.
    fn set_behavior_kind(&mut self, path: &str, kind: &str) {
        let _ = (path, kind);
    }

    /// Set the scope of the behavior attached to the prim at `path`
    /// (`StudioSession::set_behavior_scope`). `scope` is a token — `"entity"` or
    /// `"stage"` — passed as a `&str` so `idtx-editor-ui` need not depend on
    /// `idtx-scene`.
    fn set_behavior_scope(&mut self, path: &str, scope: &str) {
        let _ = (path, scope);
    }

    /// Set whether the behavior attached to the prim at `path` is
    /// server-authoritative (`StudioSession::set_behavior_server_authoritative`).
    /// A server-authoritative behavior ticks only on the authoritative server
    /// role; the authority gate skips it on a local client that is not the
    /// server.
    fn set_behavior_server_authoritative(&mut self, path: &str, server_authoritative: bool) {
        let _ = (path, server_authoritative);
    }

    /// Author an arbitrary `IdtxBehaviorAPI` config attribute on the behavior at
    /// `path` — the generic channel behind the behavior-config area
    /// (`idtx:behavior:capabilities` / `:inputs` token arrays, `:version`,
    /// `:ship`, `:limits:memoryMb`). Routes through the session-layer behavior
    /// override path (no root-layer opinion required). Default no-op.
    fn set_behavior_property(
        &mut self,
        path: &str,
        property: &str,
        value: idtx_editor_model::AttributeValue,
    ) {
        let _ = (path, property, value);
    }

    /// Apply a single-apply API schema (e.g. `"ShadowAPI"`) to the prim at
    /// `path`, authoring it into the root layer.
    ///
    /// Used by the light inspector's **Add Shadow Casting** button. The shell
    /// dispatches to the session's `apply_api_schema` and journals a reversible
    /// edit (undo removes the schema). Default no-op.
    fn apply_api_schema(&mut self, path: &str, schema: &str) {
        let _ = (path, schema);
    }

    /// Remove an API schema (e.g. `"ShadowAPI"`) from the prim at
    /// `path`, authoring it into the root layer.
    ///
    /// Used by the light inspector's **Remove Shadow Casting** button. The shell
    /// dispatches to the session's `remove_api_schema` and journals a reversible
    /// edit (undo removes the schema). Default no-op.
    fn remove_api_schema(&mut self, path: &str, schema: &str) {
        let _ = (path, schema);
    }

    /// Make the gprim at `path` collidable with the `OpenUSD`-standard schemas:
    /// apply `UsdPhysicsCollisionAPI` (+ `UsdPhysicsMeshCollisionAPI` with
    /// `physics:approximation = none` when the prim is a Mesh) and the reduced
    /// `IdtxCollisionAPI` addendum, as one failure-atomic batch. Used by the
    /// collision inspector's **Add Collision** button (conductor only). The shell
    /// journals a reversible edit (undo removes exactly the applied schema set).
    /// Default no-op.
    fn apply_collision_api(&mut self, path: &str) {
        let _ = path;
    }

    /// Remove the collider schemas from the gprim at `path` and clear the
    /// collision-owned authored opinions (the two standard collider attrs and
    /// every `idtx:collision:*` attribute, including the pre-baked hull arrays).
    /// Used by the collision inspector's **Remove Collision** button (conductor
    /// only), the counterpart of [`Self::apply_collision_api`]. The shell journals
    /// a reversible edit (undo re-applies the schemas and restores the opinions).
    /// Default no-op.
    fn remove_collision_api(&mut self, path: &str) {
        let _ = path;
    }

    /// Generate a collision convex hull for the prim at `path` (the collision
    /// inspector's **Generate `CollisionHull`** button, conductor only).
    ///
    /// `params` carries the fit knobs (tolerance, iteration cap, input-point
    /// budget). The shell gathers the prim's mesh geometry, computes the hull
    /// off-thread, and authors `idtx:collision:hull:points` / `:triangles`.
    /// Default no-op.
    fn generate_collision_hull(&mut self, path: &str, params: HullGenParams) {
        let _ = (path, params);
    }

    /// Remove the root-layer opinion of `property` on the prim at `path`
    /// ("Remove Opinion" in the attribute opinion stack).
    ///
    /// The shell snapshots the opinion, clears it (reverting the composed value
    /// to weaker layers / schema), and journals a reversible edit whose undo
    /// restores the snapshot. Default no-op.
    fn remove_property_opinion(&mut self, path: &str, property: &str) {
        let _ = (path, property);
    }

    /// Remove every root-layer transform opinion (`properties` — the prim's
    /// root-authored `xformOp:*` / `xformOpOrder`) on the prim at `path`
    /// ("Reset Transform").
    ///
    /// The shell snapshots each, clears them all, and journals a single
    /// reversible edit whose undo restores every snapshot. Default no-op.
    fn reset_transform(&mut self, path: &str, properties: Vec<String>) {
        let _ = (path, properties);
    }
}

// ── InspectorContribution ─────────────────────────────────────────────────────

/// A single collapsible section in the prim inspector.
///
/// Implement this trait and register an instance with [`InspectorRegistry::register`]
/// to add a new section.  The registry calls panels in [`priority`] order; lower
/// numbers render first.
pub trait InspectorContribution: Send + Sync {
    /// Title shown in the collapsing header.
    fn label(&self) -> &'static str;

    /// Whether the section starts open (`true`) or collapsed (`false`).
    fn default_open(&self) -> bool {
        true
    }

    /// When `false` the section body is inlined without a collapsing header.
    fn collapsible(&self) -> bool {
        true
    }

    /// Rendering order: lower numbers appear first.  Built-ins use -1000…0.
    /// Plugins should use values ≥ 500 to appear after built-in sections.
    fn priority(&self) -> i32 {
        0
    }

    /// Call `claims.claim(name)` for every property the panel handles so
    /// the fallback panel does not duplicate them.
    fn claim(&self, claims: &mut PropertyClaimSet);

    /// Return `true` when this panel should be shown for the current prim.
    /// Called every frame; must be cheap (no FFI, no allocations).
    fn present(&self, ctx: &InspectorContext<'_>, claims: &PropertyClaimSet) -> bool;

    /// Render the section body.
    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, claims: &PropertyClaimSet);
}

// ── InspectorRegistry ─────────────────────────────────────────────────────────

/// Ordered collection of [`InspectorContribution`]s.
///
/// Built at app startup; read-only after that.  Thread-safe via `Arc`.
#[allow(missing_debug_implementations)]
pub struct InspectorRegistry {
    /// Sorted by `contribution.priority()` ascending at insertion time.
    contributions: Vec<Arc<dyn InspectorContribution>>,
    /// claim set; panels call `claims.claim(name)` during `claim()`
    /// to prevent the fallback panel from duplicating the same attribute.
    claims: PropertyClaimSet,
}

impl InspectorRegistry {
    /// Empty registry.  Useful for tests.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contributions: Vec::new(),
            claims: PropertyClaimSet::new(),
        }
    }

    /// Registry pre-populated with all built-in panels in canonical order.
    #[must_use]
    pub fn with_builtins() -> Self {
        use panels::{
            behavior::BehaviorInspectorPanel, camera::CameraPanel, capsule::CapsulePanel,
            collision::CollisionPanel, common::CommonPanel, cone::ConePanel, cube::CubePanel,
            cylinder::CylinderPanel, fallback::FallbackPanel, light::LightPanel,
            material::MaterialPanel, mesh::MeshPanel, packed_stage::PackedStagePanel,
            plane::PlanePanel, prim_header::PrimHeaderPanel, relationships::RelationshipsPanel,
            skeleton::SkeletonPanel, sphere::SpherePanel, transform::TransformPanel,
            variant_sets::VariantSetsPanel,
        };

        let mut r = Self::new();
        r.register(Arc::new(PrimHeaderPanel));
        r.register(Arc::new(CommonPanel));
        r.register(Arc::new(TransformPanel));
        r.register(Arc::new(SkeletonPanel));
        r.register(Arc::new(MaterialPanel));
        r.register(Arc::new(MeshPanel));
        // Primitive-shape panels (priority 5) appear after MeshPanel (priority 0).
        r.register(Arc::new(CubePanel));
        r.register(Arc::new(SpherePanel));
        r.register(Arc::new(CylinderPanel));
        r.register(Arc::new(ConePanel));
        r.register(Arc::new(CapsulePanel));
        r.register(Arc::new(PlanePanel));
        r.register(Arc::new(CameraPanel));
        r.register(Arc::new(LightPanel));
        r.register(Arc::new(CollisionPanel));
        r.register(Arc::new(VariantSetsPanel));
        r.register(Arc::new(PackedStagePanel));
        r.register(Arc::new(BehaviorInspectorPanel));
        r.register(Arc::new(RelationshipsPanel));
        r.register(Arc::new(FallbackPanel));
        r
    }

    /// Register a contribution.  Inserted at the position that maintains
    /// ascending priority order.
    pub fn register(&mut self, contribution: Arc<dyn InspectorContribution>) {
        let p = contribution.priority();
        contribution.claim(&mut self.claims);

        let pos = self.contributions.partition_point(|c| c.priority() <= p);

        self.contributions.insert(pos, contribution);
    }

    /// Draw all present contributions into `ui`.
    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
        for contribution in &self.contributions {
            if !contribution.present(ctx, &self.claims) {
                continue;
            }
            if contribution.collapsible() {
                /*let id = ui.make_persistent_id(contribution.label());
                egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, contribution.default_open())
                    .show_header(ui, |ui| ui.label(contribution.label()))
                    .body(|ui| contribution.draw(ui, ctx));
                */
                egui::CollapsingHeader::new(contribution.label())
                    .default_open(contribution.default_open())
                    .show(ui, |ui| contribution.draw(ui, ctx, &self.claims));
            } else {
                contribution.draw(ui, ctx, &self.claims);
            }
            ui.add_space(2.0);
        }
    }
}

impl Default for InspectorRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the prim property inspector panel.
///
/// Call this inside a right-side panel each frame.  The function wraps
/// everything in a `ScrollArea`.
///
/// `prim` is `None` when nothing is selected; a hint is shown instead.
/// `properties` should come from a cached `session.inspect_prim()` call
/// refreshed whenever `selected_path` or `topology_generation` changes.
/// `mode` controls whether widgets are interactive; pass
/// [`InspectorMode::Authoring`] for Poser, [`InspectorMode::ReadOnlyWithVariants`]
/// for Inspector.
/// `sink` is `None` in read-only contexts; pass `Some(&mut impl EditSink)` to
/// enable variant selection and attribute authoring.
#[allow(clippy::too_many_arguments)] // cohesive per-frame inspector inputs; a struct would only indirect them
pub fn inspector_panel<'a>(
    ui: &mut Ui,
    registry: &InspectorRegistry,
    prim: Option<&'a PrimInfo>,
    properties: &'a [PropertyRecord],
    skeleton_joints: Option<&'a [SkeletonJointRecord]>,
    mode: InspectorMode,
    sink: Option<&'a mut dyn EditSink>,
    collision_hull_generating: bool,
) {
    let Some(prim) = prim else {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Select a prim to inspect")
                    .small()
                    .weak(),
            );
        });
        return;
    };

    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut ctx = InspectorContext {
                info: prim,
                properties,
                mode,
                sink,
                skeleton_joints,
                collision_hull_generating,
            };
            registry.draw(ui, &mut ctx);
        });
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use idtx_editor_model::{InspectorMode, PrimInfo};

    use super::*;

    // ── PropertyClaimSet ──────────────────────────────────────────────────────

    #[test]
    fn claim_marks_property_as_claimed_not_editable() {
        let mut claims = PropertyClaimSet::new();
        claims.claim("xformOp:translate");
        assert!(claims.is_claimed("xformOp:translate"));
        assert!(!claims.is_property_editable("xformOp:translate"));
    }

    #[test]
    fn claim_editable_marks_both_claimed_and_editable() {
        let mut claims = PropertyClaimSet::new();
        claims.claim_editable("primvars:displayColor");
        assert!(claims.is_claimed("primvars:displayColor"));
        assert!(claims.is_property_editable("primvars:displayColor"));
    }

    #[test]
    fn claim_prefix_editable_matches_by_prefix() {
        let mut claims = PropertyClaimSet::new();
        claims.claim_prefix_editable("xformOp:");
        assert!(claims.is_claimed("xformOp:translate"));
        assert!(claims.is_claimed("xformOp:rotateXYZ"));
        assert!(claims.is_property_editable("xformOp:translate"));
        assert!(claims.is_property_editable("xformOp:scale"));
        assert!(!claims.is_property_editable("primvars:displayColor"));
    }

    #[test]
    fn unclaimed_authored_excludes_claimed_properties() {
        let mut claims = PropertyClaimSet::new();
        claims.claim_editable("primvars:displayColor");

        let props = vec![
            PropertyRecord {
                name: "primvars:displayColor".to_owned(),
                kind: idtx_usd::PropertyKind::Attribute,
                type_name: "color3f[]".to_owned(),
                value_summary: "(0.18, 0.18, 0.18)".to_owned(),
                authored: true,
                value_source: "authored".to_owned(),
                time_sample_count: 0,
                provenance: vec![],
                provenance_values: vec![],
                relationship_targets: vec![],
                root_authored: false,
            },
            PropertyRecord {
                name: "doubleSided".to_owned(),
                kind: idtx_usd::PropertyKind::Attribute,
                type_name: "bool".to_owned(),
                value_summary: "false".to_owned(),
                authored: true,
                value_source: "authored".to_owned(),
                time_sample_count: 0,
                provenance: vec![],
                provenance_values: vec![],
                relationship_targets: vec![],
                root_authored: false,
            },
        ];

        let unclaimed = claims.unclaimed_authored(&props);
        assert_eq!(unclaimed.len(), 1);
        assert_eq!(unclaimed[0].name, "doubleSided");
    }

    // ── InspectorContext::is_prim_editable ────────────────────────────────────

    fn make_info(locally_authored: bool) -> PrimInfo {
        PrimInfo {
            path: "/World/Box".to_owned(),
            is_locally_authored: locally_authored,
            ..Default::default()
        }
    }

    #[test]
    fn is_prim_editable_false_in_readonly_mode() {
        let info = make_info(true);
        let ctx = InspectorContext {
            info: &info,
            properties: &[],
            mode: InspectorMode::ReadOnly,
            sink: None,
            skeleton_joints: None,
            collision_hull_generating: false,
        };
        assert!(!ctx.is_prim_editable());
    }

    #[test]
    fn is_prim_editable_false_in_variant_mode() {
        let info = make_info(true);
        let ctx = InspectorContext {
            info: &info,
            properties: &[],
            mode: InspectorMode::ReadOnlyWithVariants,
            sink: None,
            skeleton_joints: None,
            collision_hull_generating: false,
        };
        assert!(!ctx.is_prim_editable());
    }

    #[test]
    fn is_prim_editable_true_when_authoring_and_locally_authored() {
        let info = make_info(true);
        let ctx = InspectorContext {
            info: &info,
            properties: &[],
            mode: InspectorMode::Authoring,
            sink: None,
            skeleton_joints: None,
            collision_hull_generating: false,
        };
        assert!(ctx.is_prim_editable());
    }

    #[test]
    fn is_prim_editable_false_when_authoring_but_not_locally_authored() {
        // Packed-stage prims with no root-layer opinion stay read-only.
        let info = make_info(false);
        let ctx = InspectorContext {
            info: &info,
            properties: &[],
            mode: InspectorMode::Authoring,
            sink: None,
            skeleton_joints: None,
            collision_hull_generating: false,
        };
        assert!(!ctx.is_prim_editable());
    }

    // ── InspectorContext::commit_transform ────────────────────────────────────

    #[derive(Default)]
    struct RecordingSink {
        commits: Vec<(String, [[f32; 4]; 4], TransformEdit, Option<TransformEdit>)>,
        applied_apis: Vec<(String, String)>,
        variant_selections: Vec<(String, String, String, String)>,
    }
    impl EditSink for RecordingSink {
        fn set_variant_selection(&mut self, path: &str, set_name: &str, before: &str, after: &str) {
            self.variant_selections.push((
                path.to_owned(),
                set_name.to_owned(),
                before.to_owned(),
                after.to_owned(),
            ));
        }

        fn commit_transform(
            &mut self,
            path: &str,
            before: [[f32; 4]; 4],
            after: TransformEdit,
            before_trs: Option<TransformEdit>,
        ) {
            self.commits
                .push((path.to_owned(), before, after, before_trs));
        }

        fn apply_api_schema(&mut self, path: &str, schema: &str) {
            self.applied_apis.push((path.to_owned(), schema.to_owned()));
        }
    }

    // ── InspectorContext::with_variant_selection ──────────────────────────────

    // Variant switching must author through the sink in *every* interactive
    // mode. The Conductor runs the inspector in `Cooking` mode, which was
    // previously omitted from `with_variant_selection`, silently dropping every
    // variant change (the combo snapped back to its old value). Poser
    // (`Authoring`) and Inspector (`ReadOnlyWithVariants`) were unaffected.
    #[test]
    fn with_variant_selection_authors_in_every_interactive_mode() {
        for mode in [
            InspectorMode::ReadOnlyWithVariants,
            InspectorMode::Authoring,
            InspectorMode::Cooking,
        ] {
            let info = make_info(false);
            let mut sink = RecordingSink::default();
            {
                let mut ctx = InspectorContext {
                    info: &info,
                    properties: &[],
                    mode,
                    sink: Some(&mut sink),
                    skeleton_joints: None,
                    collision_hull_generating: false,
                };
                ctx.with_variant_selection(|path, sink| {
                    sink.set_variant_selection(path, "playerKind", "desktop", "xr");
                });
            }
            assert_eq!(
                sink.variant_selections,
                vec![(
                    "/World/Box".to_owned(),
                    "playerKind".to_owned(),
                    "desktop".to_owned(),
                    "xr".to_owned()
                )],
                "variant selection should author in {mode:?}"
            );
        }
    }

    // Read-only mode has no variant interaction: the closure must never fire.
    #[test]
    fn with_variant_selection_is_a_no_op_in_readonly_mode() {
        let info = make_info(false);
        let mut sink = RecordingSink::default();
        {
            let mut ctx = InspectorContext {
                info: &info,
                properties: &[],
                mode: InspectorMode::ReadOnly,
                sink: Some(&mut sink),
                skeleton_joints: None,
                collision_hull_generating: false,
            };
            ctx.with_variant_selection(|path, sink| {
                sink.set_variant_selection(path, "playerKind", "desktop", "xr");
            });
        }
        assert!(sink.variant_selections.is_empty());
    }

    fn sample_edit() -> ([[f32; 4]; 4], TransformEdit) {
        let before = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let after = TransformEdit {
            translation: [1.0, 2.0, 3.0],
            rotation_xyz_degrees: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
        };
        (before, after)
    }

    #[test]
    fn commit_transform_routes_to_sink_when_editable() {
        let info = make_info(true);
        let mut sink = RecordingSink::default();
        let (before, after) = sample_edit();
        {
            let mut ctx = InspectorContext {
                info: &info,
                properties: &[],
                mode: InspectorMode::Authoring,
                sink: Some(&mut sink),
                skeleton_joints: None,
                collision_hull_generating: false,
            };
            ctx.commit_transform(before, after, None);
        }
        assert_eq!(sink.commits.len(), 1);
        assert_eq!(sink.commits[0].0, "/World/Box");
        assert_eq!(sink.commits[0].2, after);
    }

    #[test]
    fn commit_transform_suppressed_when_not_editable() {
        // Authoring mode but no root-layer opinion → the commit must not reach
        // the journal sink (mirrors is_prim_editable).
        let info = make_info(false);
        let mut sink = RecordingSink::default();
        let (before, after) = sample_edit();
        {
            let mut ctx = InspectorContext {
                info: &info,
                properties: &[],
                mode: InspectorMode::Authoring,
                sink: Some(&mut sink),
                skeleton_joints: None,
                collision_hull_generating: false,
            };
            ctx.commit_transform(before, after, None);
        }
        assert!(sink.commits.is_empty());
    }

    #[test]
    fn apply_api_schema_routes_to_sink_when_editable() {
        let info = make_info(true);
        let mut sink = RecordingSink::default();
        {
            let mut ctx = InspectorContext {
                info: &info,
                properties: &[],
                mode: InspectorMode::Authoring,
                sink: Some(&mut sink),
                skeleton_joints: None,
                collision_hull_generating: false,
            };
            ctx.apply_api_schema("ShadowAPI");
        }
        assert_eq!(sink.applied_apis.len(), 1);
        assert_eq!(
            sink.applied_apis[0],
            ("/World/Box".to_owned(), "ShadowAPI".to_owned())
        );
    }

    #[test]
    fn apply_api_schema_suppressed_when_not_editable() {
        // Authoring mode but no root-layer opinion → apply must not reach the sink.
        let info = make_info(false);
        let mut sink = RecordingSink::default();
        {
            let mut ctx = InspectorContext {
                info: &info,
                properties: &[],
                mode: InspectorMode::Authoring,
                sink: Some(&mut sink),
                skeleton_joints: None,
                collision_hull_generating: false,
            };
            ctx.apply_api_schema("ShadowAPI");
        }
        assert!(sink.applied_apis.is_empty());
    }
}
