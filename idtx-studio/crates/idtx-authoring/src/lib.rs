//! Edit journal — the user-visible undo/redo history for IDTX Studio authoring.
//!
//! ## Design
//!
//! The journal stores a stack of [`EditCommand`]s representing reversible edits.
//! Authoring itself is done directly through `StudioSession`; the journal is a
//! separate accounting layer on top.
//!
//! For interactive drags (gizmo), only the **final** transform at drag-end is
//! recorded: the shell authors every intermediate value live (so the viewport
//! responds at interactive rates) but calls `push` only once when the drag
//! finishes.  This produces a "undo the drag in one step" history that matches
//! every professional DCC tool.
//!
//! Non-interactive commits (inspector text-field edits) call `push` immediately.
//!
//! ## Undo/Redo model
//!
//! - `push(cmd)` — record a completed edit; clears the redo stack.
//! - `try_undo(apply)` / `try_redo(apply)` — replay and move the entry only on success.
//! - `clear()` — wipe both stacks (stage reload / close).
//!
//! History retains at most 256 entries / 64 MiB by default. Live opinion captures
//! have a separate 16-gesture / 4 MiB admission limit.

use glam::Mat4;
use idtx_usd::PropertyValue;

/// Authored TRS (translate, `rotateXYZ` degrees, scale) captured alongside a
/// transform edit so undo/redo can restore the exact authored Euler values.
///
/// A matrix stores orientation but not which Euler triple produced it, so
/// re-decomposing on replay folds middle-axis rotations back onto ±90°. When a
/// prim is authored as `rotateXYZ` ops the shell captures this so the reverse/
/// forward direction re-authors the raw angles instead of decomposing the matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformTrs {
    pub translation: [f64; 3],
    pub rotation_xyz_degrees: [f64; 3],
    pub scale: [f64; 3],
}

/// A single reversible edit in the IDTX Studio editor.
#[derive(Debug, Clone)]
pub enum EditCommand {
    /// Replace the local transform of `path` from `before` → `after`.
    /// Local space (parent-relative).
    ///
    /// `before_trs`/`after_trs` carry the authored Euler TRS when the prim is a
    /// `rotateXYZ` op stack; replay prefers them so a `Y = 180°` edit is restored
    /// exactly. `None` (e.g. matrix ops, gizmo drags) falls back to decomposing
    /// the matrix.
    SetLocalTransform {
        path: String,
        before: Mat4,
        after: Mat4,
        before_trs: Option<TransformTrs>,
        after_trs: Option<TransformTrs>,
    },
    /// Set a typed attribute value on the root layer.
    ///
    /// `before` is `None` when the attribute was not previously authored (the
    /// undo direction will clear the attribute).  Each entry is one confirmed
    /// edit gesture: live drag deltas are streamed to USD without journaling,
    /// and only the committed before→after pair is pushed here (see the shell's
    /// `CommitAttributeEdit`), so consecutive edits are *not* merged.
    SetAttribute {
        path: String,
        property: String,
        before: Option<PropertyValue>,
        after: PropertyValue,
    },
    /// Exact authored ROOT-layer property opinions, including absence, samples
    /// and metadata. Inspector values may be composed fallbacks and cannot be
    /// used as the before opinion. Replay compares the expected opinion first.
    RootProperty {
        path: String,
        property: String,
        before: String,
        after: String,
    },
    /// Root-layer transform-only captures; other properties/children never
    /// become part of an interactive transform gesture's inverse.
    RootTransform { path: String, before: String, after: String },
    /// Change a variant selection on the session layer.
    SetVariantSelection {
        path: String,
        set_name: String,
        before: String,
        after: String,
    },
    SessionVariant { path: String, set_name: String, before: Option<String>, after: Option<String> },
    /// Author an empty root-layer `over` for a display-only prim at `path`
    /// ("Create Opinion"). Undo removes the empty override.
    CreateOverride { path: String },
    /// Remove an empty root-layer `over` at `path` ("Remove Opinion").
    /// Undo re-authors the empty override.
    RemoveOverride { path: String },
    /// A structural edit captured as two root-layer USDA snapshots. Replay
    /// derives their authored delta and merges it into the CURRENT layer after
    /// checking expected opinions. It never replaces unrelated peer opinions
    /// with the historical root snapshot.
    ///
    /// `label` is the human-readable menu text (e.g. `"Create Camera"`). Undo
    /// restores `before`; redo restores `after`.
    Structural {
        label: String,
        before: String,
        after: String,
    },
    /// Change the stage's root-layer shadow mode (`idtx:shadowMode` token).
    /// Stage-level metadata edit (no prim path); undo restores `before`.
    SetShadowMode {
        before: idtx_usd::ShadowMode,
        after: idtx_usd::ShadowMode,
    },
    /// Apply a single-apply API schema (e.g. `ShadowAPI`) to the prim at `path`
    /// ("Add Shadow Casting"). Undo removes the schema (`remove_api_schema`).
    ApplyApiSchema { path: String, schema: String },
    /// Remove an API schema (e.g. `ShadowAPI`) from the prim at `path`
    /// ("Remove Shadow Casting"). Undo adds the schema (`apply_api_schema`).
    RemoveApiSchema { path: String, schema: String },
    /// Remove the root-layer opinion of a single property on the prim at `path`
    /// ("Remove Opinion" in the attribute opinion stack).
    ///
    /// `snapshot` is the serialized root-layer spec captured *before* removal
    /// (see `StudioSession::snapshot_root_property`); undo re-authors it exactly,
    /// so any property type round-trips losslessly. Empty `snapshot` means there
    /// was no root opinion to remove and the command is a no-op.
    ClearProperty {
        path: String,
        property: String,
        snapshot: String,
    },
    /// Remove every root-layer transform opinion (`xformOp:*` and
    /// `xformOpOrder`) on the prim at `path` ("Reset Transform"), reverting the
    /// composed transform to whatever weaker layers / schema provide.
    ///
    /// `snapshots` holds one `(property, snapshot)` pair per removed spec; undo
    /// restores them all. Empty `snapshots` is a no-op.
    ResetTransform {
        path: String,
        snapshots: Vec<(String, String)>,
    },
}

impl EditCommand {
    fn retained_bytes(&self) -> usize {
        let value_bytes = |value: &PropertyValue| match value {
            PropertyValue::String(text) | PropertyValue::Token(text) | PropertyValue::Asset(text) => text.capacity(),
            PropertyValue::TokenArray(values) => values.capacity() * std::mem::size_of::<String>() + values.iter().map(String::capacity).sum::<usize>(),
            PropertyValue::Point3Array(values) => values.capacity() * std::mem::size_of::<[f32; 3]>(),
            PropertyValue::IntArray(values) => values.capacity() * std::mem::size_of::<i32>(),
            _ => 0,
        };
        std::mem::size_of::<Self>() + match self {
            Self::SetLocalTransform { path, .. } | Self::CreateOverride { path } | Self::RemoveOverride { path } => path.capacity(),
            Self::SetAttribute { path, property, before, after } => path.capacity() + property.capacity() + before.as_ref().map_or(0, &value_bytes) + value_bytes(after),
            Self::RootProperty { path, property, before, after } => path.capacity() + property.capacity() + before.capacity() + after.capacity(),
            Self::RootTransform { path, before, after } => path.capacity() + before.capacity() + after.capacity(),
            Self::SetVariantSelection { path, set_name, before, after } => path.capacity() + set_name.capacity() + before.capacity() + after.capacity(),
            Self::SessionVariant { path, set_name, before, after } => path.capacity() + set_name.capacity() + before.as_ref().map_or(0, String::capacity) + after.as_ref().map_or(0, String::capacity),
            Self::Structural { label, before, after } => label.capacity() + before.capacity() + after.capacity(),
            Self::SetShadowMode { .. } => 0,
            Self::ApplyApiSchema { path, schema } | Self::RemoveApiSchema { path, schema } => path.capacity() + schema.capacity(),
            Self::ClearProperty { path, property, snapshot } => path.capacity() + property.capacity() + snapshot.capacity(),
            Self::ResetTransform { path, snapshots } => path.capacity() + snapshots.capacity() * std::mem::size_of::<(String, String)>()
                + snapshots.iter().map(|(property, text)| property.capacity() + text.capacity()).sum::<usize>(),
        }
    }

    /// Human-readable label for undo/redo menu items.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::SetLocalTransform { path, .. } | Self::RootTransform { path, .. } => format!("Transform {path}"),
            Self::SetAttribute { property, .. } | Self::RootProperty { property, .. } => format!("Set {property}"),
            Self::SetVariantSelection {
                set_name, after, ..
            } => {
                format!("Variant {set_name}={after}")
            }
            Self::SessionVariant { set_name, after, .. } => format!("Variant {set_name}={}", after.as_deref().unwrap_or("<inherited>")),
            Self::CreateOverride { path } => format!("Create Opinion {path}"),
            Self::RemoveOverride { path } => format!("Remove Opinion {path}"),
            Self::Structural { label, .. } => label.clone(),
            Self::SetShadowMode { after, .. } => format!("Shadow Mode {after:?}"),
            Self::ApplyApiSchema { schema, .. } => format!("Add {schema}"),
            Self::RemoveApiSchema { schema, .. } => format!("Remove {schema}"),
            Self::ClearProperty { property, .. } => format!("Remove {property}"),
            Self::ResetTransform { .. } => "Reset Transform".to_owned(),
        }
    }

    /// `true` when the command makes no observable change and can be dropped.
    #[allow(clippy::float_cmp)]
    fn is_noop(&self) -> bool {
        match self {
            Self::SetLocalTransform { before, after, .. } => {
                before.to_cols_array() == after.to_cols_array()
            }
            Self::SetAttribute { before, after, .. } => before
                .as_ref()
                .is_some_and(|b| property_values_eq(b, after)),
            Self::SetVariantSelection { before, after, .. } | Self::RootProperty { before, after, .. } | Self::RootTransform { before, after, .. }
            // A structural edit is a no-op only when it changed nothing in the
            // root layer's serialised text.
            | Self::Structural { before, after, .. } => before == after,
            Self::SetShadowMode { before, after } => before == after,
            Self::SessionVariant { before, after, .. } => before == after,
            // Structural create/remove of an override , or applying an API schemais always observable.
            Self::CreateOverride { .. }
            | Self::RemoveOverride { .. }
            | Self::ApplyApiSchema { .. }
            | Self::RemoveApiSchema { .. } => false,
            // Removing a root opinion is observable only when one existed; an
            // empty snapshot / list means there was nothing to remove.
            Self::ClearProperty { snapshot, .. } => snapshot.is_empty(),
            Self::ResetTransform { snapshots, .. } => snapshots.is_empty(),
        }
    }

    #[must_use]
    /// The path this command targets.
    pub fn target_path(&self) -> &str {
        match self {
            Self::SetLocalTransform { path, .. }
            | Self::SetAttribute { path, .. }
            | Self::RootProperty { path, .. }
            | Self::RootTransform { path, .. }
            | Self::SetVariantSelection { path, .. }
            | Self::SessionVariant { path, .. }
            | Self::CreateOverride { path }
            | Self::RemoveOverride { path }
            | Self::ApplyApiSchema { path, .. }
            | Self::RemoveApiSchema { path, .. }
            | Self::ClearProperty { path, .. }
            | Self::ResetTransform { path, .. } => path,
            // A structural edit spans the whole root layer, not a single path.
            Self::Structural { .. } => "",
            // Stage-level metadata edit — targets the pseudo-root.
            Self::SetShadowMode { .. } => "/",
        }
    }
}

/// Shallow equality for `PropertyValue` — sufficient to detect no-op edits.
fn property_values_eq(a: &PropertyValue, b: &PropertyValue) -> bool {
    use PropertyValue as PV;
    match (a, b) {
        (PV::Bool(a), PV::Bool(b)) => a == b,
        (PV::F32(a32), PV::F32(b32)) => a32.to_bits() == b32.to_bits(),
        (PV::F64(a64), PV::F64(b64)) => a64.to_bits() == b64.to_bits(),
        (PV::TimeCode(a), PV::TimeCode(b)) => a.to_bits() == b.to_bits(),
        (PV::String(sa), PV::String(sb)) => sa == sb,
        (PV::Token(ta), PV::Token(tb)) => ta == tb,
        (PV::Color3f(ca), PV::Color3f(cb)) => ca == cb,
        (PV::Asset(aa), PV::Asset(ab)) => aa == ab,
        _ => false,
    }
}

/// Undo/redo history for the active stage.
///
/// Backed by a `done` stack and a `redo_stack`.  New edits push onto `done`
/// and clear `redo_stack`. Successful transactional replay moves an entry
/// between the stacks; failed replay leaves both unchanged.
///
/// The journal does **not** author edits itself — it returns commands to the
/// caller (the shell) which applies them to the `StudioSession`.
#[derive(Debug)]
pub struct EditJournal {
    done: Vec<EditCommand>,
    redo_stack: Vec<EditCommand>,
    max_entries: usize,
    max_bytes: usize,
    pending_properties: std::collections::HashMap<(String, String), String>,
}

impl Default for EditJournal {
    fn default() -> Self { Self::with_limits(256, 64 * 1024 * 1024) }
}

impl EditJournal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self { done: Vec::new(), redo_stack: Vec::new(), max_entries, max_bytes, pending_properties: std::collections::HashMap::new() }
    }

    #[must_use]
    pub fn has_property_capture(&self, path: &str, property: &str) -> bool {
        self.pending_properties.contains_key(&(path.into(), property.into()))
    }

    /// Capture once at the start of a live gesture, before its first USD write.
    pub fn capture_property(&mut self, path: &str, property: &str, text: String) -> bool {
        if self.has_property_capture(path, property) { return true; }
        let bytes: usize = self.pending_properties.iter().map(|((path, property), text)| path.capacity() + property.capacity() + text.capacity()).sum();
        if self.pending_properties.len() >= 16 || bytes.saturating_add(text.capacity()).saturating_add(path.len()).saturating_add(property.len()) > 4 * 1024 * 1024 { return false; }
        self.pending_properties.insert((path.into(), property.into()), text);
        true
    }

    pub fn finish_property(&mut self, path: &str, property: &str, after: String) {
        if let Some(before) = self.pending_properties.remove(&(path.into(), property.into())) {
            self.push(EditCommand::RootProperty { path: path.into(), property: property.into(), before, after });
        }
    }

    pub fn finish_transform(&mut self, path: &str, after: String) {
        if let Some(before) = self.pending_properties.remove(&(path.into(), String::new())) {
            self.push(EditCommand::RootTransform { path: path.into(), before, after });
        }
    }

    /// Replay before moving history. An error leaves both stacks untouched.
    pub fn try_undo<E>(&mut self, apply: impl FnOnce(&EditCommand) -> Result<(), E>) -> Result<bool, E> {
        let Some(command) = self.done.last() else { return Ok(false); };
        apply(command)?;
        if let Some(command) = self.done.pop() { self.redo_stack.push(command); }
        Ok(true)
    }

    pub fn try_redo<E>(&mut self, apply: impl FnOnce(&EditCommand) -> Result<(), E>) -> Result<bool, E> {
        let Some(command) = self.redo_stack.last() else { return Ok(false); };
        apply(command)?;
        if let Some(command) = self.redo_stack.pop() { self.done.push(command); }
        Ok(true)
    }

    #[must_use]
    /// Number of entries available to undo.
    pub fn undo_depth(&self) -> usize {
        self.done.len()
    }

    #[must_use]
    /// Number of entries available to redo.
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Label for the next entry to undo, for menu display.
    pub fn next_undo_label(&self) -> Option<String> {
        self.done.last().map(EditCommand::label)
    }

    /// Label for the next entry to redo, for menu display.
    pub fn next_redo_label(&self) -> Option<String> {
        self.redo_stack.last().map(EditCommand::label)
    }

    /// Record a completed edit.  Clears the redo stack.  No-op commands
    /// (before == after) are silently dropped.
    ///
    /// Each call records one confirmed edit as its own undo step. Continuous
    /// live edits (color-picker drags, numeric field typing) are streamed to
    /// USD off-journal and only their confirmed result is pushed here, so there
    /// is nothing to coalesce — three distinct value changes to the same
    /// attribute produce three undo steps, each restoring the prior value.
    pub fn push(&mut self, cmd: EditCommand) {
        if cmd.is_noop() {
            return;
        }
        self.done.push(cmd);
        self.redo_stack.clear();
        let mut bytes: usize = self.done.iter().map(EditCommand::retained_bytes).sum();
        while self.done.len() > self.max_entries || bytes > self.max_bytes {
            bytes = bytes.saturating_sub(self.done.remove(0).retained_bytes());
        }
    }

    /// Pop the top undo entry and move it to the redo stack.
    ///
    /// Returns the command to reverse: the caller must author `before` onto the
    /// stage. Returns `None` when there is nothing to undo.
    #[cfg(test)]
    fn undo(&mut self) -> Option<EditCommand> {
        let cmd = self.done.pop()?;
        self.redo_stack.push(cmd.clone());
        Some(cmd)
    }

    /// Pop the top redo entry and move it back to the done stack.
    ///
    /// Returns the command to re-apply: the caller must author `after` onto the
    /// stage. Returns `None` when there is nothing to redo.
    #[cfg(test)]
    fn redo(&mut self) -> Option<EditCommand> {
        let cmd = self.redo_stack.pop()?;
        self.done.push(cmd.clone());
        Some(cmd)
    }

    /// Wipe both stacks — call on stage reload or close.
    pub fn clear(&mut self) {
        self.done.clear();
        self.redo_stack.clear();
        self.pending_properties.clear();
    }
}

///------------------------------------------------------------------------------
/// Unit Tests
/// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    #[test]
    fn replay_failure_preserves_both_history_positions() {
        let mut journal = EditJournal::new();
        journal.push(xf("/Cube", Vec3::ZERO, Vec3::X));
        assert_eq!(journal.try_undo(|_| Err("conflict")), Err("conflict"));
        assert_eq!((journal.undo_depth(), journal.redo_depth()), (1, 0));
        assert_eq!(journal.try_undo::<()>(|_| Ok(())), Ok(true));
        assert_eq!(journal.try_redo(|_| Err("missing target")), Err("missing target"));
        assert_eq!((journal.undo_depth(), journal.redo_depth()), (0, 1));
    }

    #[test]
    fn history_bounds_account_for_snapshots_and_retain_the_recent_suffix() {
        let mut journal = EditJournal::with_limits(2, 4096);
        for index in 0..3 { journal.push(EditCommand::Structural { label: index.to_string(), before: "a".repeat(500), after: "b".repeat(500) }); }
        assert_eq!(journal.undo_depth(), 2);
        assert_eq!(journal.next_undo_label().as_deref(), Some("2"));
        journal.push(EditCommand::Structural { label: "too large".into(), before: "a".repeat(4096), after: "b".into() });
        assert_eq!(journal.undo_depth(), 0);
        assert!(!journal.can_redo());
    }

    fn xf(path: &str, before: Vec3, after: Vec3) -> EditCommand {
        EditCommand::SetLocalTransform {
            path: path.to_owned(),
            before: Mat4::from_translation(before),
            after: Mat4::from_translation(after),
            before_trs: None,
            after_trs: None,
        }
    }

    fn attr(path: &str, before: Option<[f32; 3]>, after: [f32; 3]) -> EditCommand {
        EditCommand::SetAttribute {
            path: path.to_owned(),
            property: "primvars:displayColor".to_owned(),
            before: before.map(PropertyValue::Color3f),
            after: PropertyValue::Color3f(after),
        }
    }

    fn variant(path: &str, before: &str, after: &str) -> EditCommand {
        EditCommand::SetVariantSelection {
            path: path.to_owned(),
            set_name: "shadingVariant".to_owned(),
            before: before.to_owned(),
            after: after.to_owned(),
        }
    }

    #[test]
    fn push_undo_redo_roundtrip() {
        let mut j = EditJournal::new();
        let p = "/World/Cube";

        j.push(xf(p, Vec3::ZERO, Vec3::X));
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.redo_depth(), 0);

        let cmd = j.undo().expect("one entry to undo");
        assert!(matches!(cmd, EditCommand::SetLocalTransform { .. }));
        assert_eq!(j.undo_depth(), 0);
        assert_eq!(j.redo_depth(), 1);

        let cmd = j.redo().expect("one entry to redo");
        assert!(matches!(cmd, EditCommand::SetLocalTransform { .. }));
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.redo_depth(), 0);
    }

    #[test]
    fn noop_push_produces_no_entry() {
        let mut j = EditJournal::new();
        j.push(xf("/Foo", Vec3::ZERO, Vec3::ZERO));
        assert_eq!(j.undo_depth(), 0);
    }

    #[test]
    fn push_after_undo_clears_redo_stack() {
        let mut j = EditJournal::new();
        let p = "/World/Cube";

        j.push(xf(p, Vec3::ZERO, Vec3::X));
        j.undo();
        assert_eq!(j.redo_depth(), 1);

        // New edit clears redo stack.
        j.push(xf(p, Vec3::ZERO, Vec3::Y));
        assert_eq!(j.redo_depth(), 0);
        assert_eq!(j.undo_depth(), 1);
    }

    #[test]
    fn undo_empty_returns_none() {
        let mut j = EditJournal::new();
        assert!(j.undo().is_none());
    }

    #[test]
    fn redo_empty_returns_none() {
        let mut j = EditJournal::new();
        assert!(j.redo().is_none());
    }

    #[test]
    fn labels_reflect_top_of_stack() {
        let mut j = EditJournal::new();
        assert!(j.next_undo_label().is_none());
        j.push(xf("/World/Box", Vec3::ZERO, Vec3::X));
        assert_eq!(j.next_undo_label().as_deref(), Some("Transform /World/Box"));
        j.undo();
        assert_eq!(j.next_redo_label().as_deref(), Some("Transform /World/Box"));
    }

    #[test]
    fn clear_wipes_both_stacks() {
        let mut j = EditJournal::new();
        j.push(xf("/A", Vec3::ZERO, Vec3::X));
        j.push(xf("/B", Vec3::ZERO, Vec3::Y));
        j.undo();
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.redo_depth(), 1);
        j.clear();
        assert_eq!(j.undo_depth(), 0);
        assert_eq!(j.redo_depth(), 0);
    }

    #[test]
    fn multiple_undo_redo_sequence() {
        let mut j = EditJournal::new();
        j.push(xf("/A", Vec3::ZERO, Vec3::X));
        j.push(xf("/B", Vec3::ZERO, Vec3::Y));
        j.push(xf("/C", Vec3::ZERO, Vec3::Z));

        assert_eq!(j.undo_depth(), 3);
        j.undo();
        j.undo();
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.redo_depth(), 2);

        j.redo();
        assert_eq!(j.undo_depth(), 2);
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn set_attribute_roundtrip() {
        let mut j = EditJournal::new();
        j.push(attr(
            "/World/Mesh",
            Some([0.18, 0.18, 0.18]),
            [1.0, 0.0, 0.0],
        ));
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(
            j.next_undo_label().as_deref(),
            Some("Set primvars:displayColor")
        );
        j.undo();
        assert_eq!(j.undo_depth(), 0);
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn set_attribute_edits_are_distinct_undo_steps() {
        // Each confirmed edit is journaled once (live deltas are streamed
        // off-journal), so consecutive changes to the same attribute must NOT
        // coalesce: undo restores the immediately-preceding value, not the
        // original. Regression: 50 → 100 → 80 → 90 must undo to 80, not 50.
        let mut j = EditJournal::new();
        j.push(attr("/Mesh", Some([0.18, 0.18, 0.18]), [0.5, 0.0, 0.0]));
        j.push(attr("/Mesh", Some([0.5, 0.0, 0.0]), [0.8, 0.0, 0.0]));
        j.push(attr("/Mesh", Some([0.8, 0.0, 0.0]), [1.0, 0.0, 0.0]));
        assert_eq!(
            j.undo_depth(),
            3,
            "three distinct confirmed edits must each be an undo step"
        );
        let cmd = j.undo().unwrap();
        if let EditCommand::SetAttribute {
            after: PropertyValue::Color3f(c),
            before: Some(PropertyValue::Color3f(b)),
            ..
        } = cmd
        {
            assert!((c[0] - 1.0).abs() < 1e-6, "after is the last value");
            assert!(
                (b[0] - 0.8).abs() < 1e-6,
                "before is the immediately-preceding value, not the original"
            );
        } else {
            panic!("expected SetAttribute");
        }
    }

    #[test]
    fn set_attribute_no_coalesce_different_property() {
        let mut j = EditJournal::new();
        j.push(EditCommand::SetAttribute {
            path: "/Mesh".to_owned(),
            property: "primvars:displayColor".to_owned(),
            before: None,
            after: PropertyValue::Color3f([1.0, 0.0, 0.0]),
        });
        j.push(EditCommand::SetAttribute {
            path: "/Mesh".to_owned(),
            property: "primvars:displayOpacity".to_owned(),
            before: None,
            after: PropertyValue::F32(0.5),
        });
        assert_eq!(j.undo_depth(), 2, "different properties must not coalesce");
    }

    #[test]
    fn set_variant_selection_roundtrip() {
        let mut j = EditJournal::new();
        j.push(variant("/World/Model", "modelingVariant", "HighRes"));
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(
            j.next_undo_label().as_deref(),
            Some("Variant shadingVariant=HighRes")
        );
        let cmd = j.undo().unwrap();
        assert!(matches!(cmd, EditCommand::SetVariantSelection { .. }));
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn set_variant_selection_noop_dropped() {
        let mut j = EditJournal::new();
        j.push(variant("/World/Model", "lod", "lod"));
        assert_eq!(j.undo_depth(), 0);
    }

    #[test]
    fn create_override_undo_redo_roundtrip() {
        let mut j = EditJournal::new();
        let p = "/World/Widget/Body";
        j.push(EditCommand::CreateOverride { path: p.to_owned() });
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(
            j.next_undo_label().as_deref(),
            Some("Create Opinion /World/Widget/Body")
        );

        let cmd = j.undo().expect("one entry to undo");
        assert!(matches!(cmd, EditCommand::CreateOverride { .. }));
        assert_eq!(j.redo_depth(), 1);

        let cmd = j.redo().expect("one entry to redo");
        assert!(matches!(cmd, EditCommand::CreateOverride { .. }));
        assert_eq!(j.undo_depth(), 1);
    }

    #[test]
    fn remove_override_undo_redo_roundtrip() {
        let mut j = EditJournal::new();
        let p = "/World/Widget/Body";
        j.push(EditCommand::RemoveOverride { path: p.to_owned() });
        assert_eq!(
            j.next_undo_label().as_deref(),
            Some("Remove Opinion /World/Widget/Body")
        );
        let cmd = j.undo().expect("one entry to undo");
        assert!(matches!(cmd, EditCommand::RemoveOverride { .. }));
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn override_commands_are_never_noop() {
        assert!(
            !EditCommand::CreateOverride {
                path: "/A".to_owned()
            }
            .is_noop()
        );
        assert!(
            !EditCommand::RemoveOverride {
                path: "/A".to_owned()
            }
            .is_noop()
        );
    }

    #[test]
    fn shadow_mode_command_journals_and_reverses() {
        use idtx_usd::ShadowMode;
        let mut j = EditJournal::new();
        j.push(EditCommand::SetShadowMode {
            before: ShadowMode::Off,
            after: ShadowMode::Soft,
        });
        assert_eq!(j.next_undo_label().as_deref(), Some("Shadow Mode Soft"));
        let cmd = j.undo().expect("one entry to undo");
        match cmd {
            EditCommand::SetShadowMode { before, after } => {
                assert_eq!(before, ShadowMode::Off);
                assert_eq!(after, ShadowMode::Soft);
            }
            other => panic!("expected SetShadowMode, got {other:?}"),
        }
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn shadow_mode_noop_when_unchanged() {
        use idtx_usd::ShadowMode;
        // A no-op mode change (before == after) is dropped by the journal.
        let mut j = EditJournal::new();
        j.push(EditCommand::SetShadowMode {
            before: ShadowMode::Hard,
            after: ShadowMode::Hard,
        });
        assert_eq!(j.undo_depth(), 0, "unchanged shadow mode must not journal");
        // Its target path is the pseudo-root (stage-level metadata edit).
        assert_eq!(
            EditCommand::SetShadowMode {
                before: ShadowMode::Off,
                after: ShadowMode::Soft
            }
            .target_path(),
            "/"
        );
    }

    #[test]
    fn override_commands_do_not_coalesce_with_attribute_edits() {
        let mut j = EditJournal::new();
        j.push(attr("/Mesh", None, [1.0, 0.0, 0.0]));
        j.push(EditCommand::CreateOverride {
            path: "/Mesh".to_owned(),
        });
        j.push(attr("/Mesh", None, [0.0, 1.0, 0.0]));
        assert_eq!(
            j.undo_depth(),
            3,
            "structural override edits must not merge into attribute runs",
        );
    }

    fn structural(label: &str, before: &str, after: &str) -> EditCommand {
        EditCommand::Structural {
            label: label.to_owned(),
            before: before.to_owned(),
            after: after.to_owned(),
        }
    }

    #[test]
    fn structural_undo_redo_roundtrip() {
        let mut j = EditJournal::new();
        j.push(structural(
            "Create Camera",
            "#usda 1.0\n",
            "#usda 1.0\ndef Camera \"Cam\" {}\n",
        ));
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.next_undo_label().as_deref(), Some("Create Camera"));

        let cmd = j.undo().expect("one entry to undo");
        assert!(matches!(cmd, EditCommand::Structural { .. }));
        assert_eq!(j.redo_depth(), 1);

        let cmd = j.redo().expect("one entry to redo");
        assert!(matches!(cmd, EditCommand::Structural { .. }));
        assert_eq!(j.undo_depth(), 1);
    }

    #[test]
    fn structural_noop_when_snapshots_match_is_dropped() {
        let mut j = EditJournal::new();
        j.push(structural("No change", "#usda 1.0\n", "#usda 1.0\n"));
        assert_eq!(j.undo_depth(), 0, "identical before/after is a no-op");
    }

    #[test]
    fn structural_edits_do_not_coalesce_with_each_other() {
        let mut j = EditJournal::new();
        j.push(structural("Define A", "a", "b"));
        j.push(structural("Define B", "b", "c"));
        assert_eq!(
            j.undo_depth(),
            2,
            "each structural edit is its own undo step",
        );
    }

    #[test]
    fn structural_target_path_is_empty() {
        assert_eq!(structural("x", "a", "b").target_path(), "");
    }

    #[test]
    fn apply_api_schema_command_journals_and_targets_prim() {
        let mut j = EditJournal::new();
        j.push(EditCommand::ApplyApiSchema {
            path: "/World/Sun".to_owned(),
            schema: "ShadowAPI".to_owned(),
        });
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.next_undo_label().as_deref(), Some("Add ShadowAPI"));
        let cmd = j.undo().expect("one entry to undo");
        match &cmd {
            EditCommand::ApplyApiSchema { path, schema } => {
                assert_eq!(path, "/World/Sun");
                assert_eq!(schema, "ShadowAPI");
                assert_eq!(cmd.target_path(), "/World/Sun");
            }
            other => panic!("expected ApplyApiSchema, got {other:?}"),
        }
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn apply_api_schema_is_never_noop() {
        assert!(
            !EditCommand::ApplyApiSchema {
                path: "/A".to_owned(),
                schema: "ShadowAPI".to_owned(),
            }
            .is_noop()
        );
    }

    #[test]
    fn clear_property_command_journals_labels_and_targets_prim() {
        let mut j = EditJournal::new();
        j.push(EditCommand::ClearProperty {
            path: "/World/Sphere".to_owned(),
            property: "radius".to_owned(),
            snapshot: "double radius = 5\n".to_owned(),
        });
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.next_undo_label().as_deref(), Some("Remove radius"));
        let cmd = j.undo().expect("one entry to undo");
        match &cmd {
            EditCommand::ClearProperty {
                path,
                property,
                snapshot,
            } => {
                assert_eq!(path, "/World/Sphere");
                assert_eq!(property, "radius");
                assert_eq!(snapshot, "double radius = 5\n");
                assert_eq!(cmd.target_path(), "/World/Sphere");
            }
            other => panic!("expected ClearProperty, got {other:?}"),
        }
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn clear_property_noop_when_snapshot_empty() {
        // An empty snapshot means nothing was authored in the root layer — the
        // journal drops the entry so undo can never "restore" a non-existent op.
        let mut j = EditJournal::new();
        j.push(EditCommand::ClearProperty {
            path: "/World/Sphere".to_owned(),
            property: "radius".to_owned(),
            snapshot: String::new(),
        });
        assert_eq!(j.undo_depth(), 0);
    }

    #[test]
    fn reset_transform_command_journals_labels_and_targets_prim() {
        let mut j = EditJournal::new();
        j.push(EditCommand::ResetTransform {
            path: "/World/Pawn".to_owned(),
            snapshots: vec![
                (
                    "xformOp:translate".to_owned(),
                    "double3 xformOp:translate = (1, 2, 3)\n".to_owned(),
                ),
                (
                    "xformOpOrder".to_owned(),
                    "uniform token[] xformOpOrder = [\"xformOp:translate\"]\n".to_owned(),
                ),
            ],
        });
        assert_eq!(j.undo_depth(), 1);
        assert_eq!(j.next_undo_label().as_deref(), Some("Reset Transform"));
        let cmd = j.undo().expect("one entry to undo");
        match &cmd {
            EditCommand::ResetTransform { path, snapshots } => {
                assert_eq!(path, "/World/Pawn");
                assert_eq!(snapshots.len(), 2);
                assert_eq!(snapshots[0].0, "xformOp:translate");
                assert_eq!(cmd.target_path(), "/World/Pawn");
            }
            other => panic!("expected ResetTransform, got {other:?}"),
        }
        assert_eq!(j.redo_depth(), 1);
    }

    #[test]
    fn reset_transform_noop_when_no_snapshots() {
        let mut j = EditJournal::new();
        j.push(EditCommand::ResetTransform {
            path: "/World/Pawn".to_owned(),
            snapshots: Vec::new(),
        });
        assert_eq!(j.undo_depth(), 0, "an empty reset must not journal");
    }
}
