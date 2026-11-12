//! Stage **Stage** panel — the third inspector tab (beside Properties and Layer
//! Stack) that surfaces the open stage's root-layer metadata and hosts the
//! stage-level authoring controls.
//!
//! Today it renders read-only root metadata (default prim, up-axis,
//! meters-per-unit, time-code range, root identifier) and a single authoring
//! control: the four-state **shadow mode** (`Off`/`Hard`/`Smooth`/`Soft`),
//! authored back into the root layer's `customLayerData["idtx:shadowMode"]`
//! token. It is the authoring home for this and future stage-level properties.
//!
//! Like [`crate::layer_stack`] this is a pure view over UI-neutral data: it
//! takes the cached [`StageMetadata`] plus the session's live shadow mode (which
//! diverges from `meta.shadow_mode` after an author write — the session owns the
//! live copy) and returns an optional [`StageAction`] for the shell to apply.
//! The shadow selector is only interactive when `editable` (the product's
//! shadow capability — Conductor only); otherwise it renders disabled so the
//! metadata is still visible under Inspector/Poser.

use egui::{Color32, RichText, Ui};
use idtx_editor_model::{InspectorMode, PrimInfo};
use idtx_usd::{PropertyRecord, ShadowMode, StageBehaviorRunScope, StageMetadata};

use crate::icons;
use crate::inspector::panels::behavior::render_behavior_body;
use crate::inspector::{EditSink, InspectorContext};

/// Applied-schema name marking a prim as carrying a behavior (`IdtxBehaviorAPI`).
const BEHAVIOR_SCHEMA: &str = "IdtxBehaviorAPI";

/// Dim grey for secondary metadata text (mirrors the Layer Stack palette).
fn dim() -> Color32 {
    crate::theme::current().text_dim
}
/// Row icon size, matching the tree/inspector row-icon metrics.
const ICON_SIZE: f32 = icons::DEFAULT_ICON_SIZE;

/// An action the Stage panel wants the shell to perform against the live
/// session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StageAction {
    /// Author a new root-layer shadow mode (`idtx:shadowMode` token).
    SetShadowMode(ShadowMode),
    /// Activate runtime physics: author a `PhysicsScene` prim with gravity
    /// defaults as a child of the stage pseudo-root.
    ActivatePhysics,
    /// Deactivate runtime physics: delete the `PhysicsScene` prim.
    DeactivatePhysics,
    /// Edit `physics:gravityDirection`. `commit` is `false` while a drag streams
    /// live (authored, not journaled) and `true` on release / focus-loss (the
    /// single journaled before→after entry). `before` is the value captured at
    /// the start of the gesture.
    SetGravityDirection {
        before: Option<[f32; 3]>,
        value: [f32; 3],
        commit: bool,
    },
    /// Edit `physics:gravityMagnitude` — same `commit` / `before` semantics as
    /// [`Self::SetGravityDirection`].
    SetGravityMagnitude {
        before: Option<f32>,
        value: f32,
        commit: bool,
    },
}

/// UI-neutral snapshot of the stage's physics state for the Physics section.
/// `active` is `true` when a `PhysicsScene` prim is present; the gravity fields
/// hold its authored values (or the schema defaults when inactive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicsPanelState {
    pub active: bool,
    pub gravity_direction: [f32; 3],
    pub gravity_magnitude: f32,
}

impl Default for PhysicsPanelState {
    fn default() -> Self {
        Self {
            active: false,
            gravity_direction: [0.0, -1.0, 0.0],
            gravity_magnitude: 9.81,
        }
    }
}

/// Renders the Stage panel and returns an optional action for the shell.
///
/// `meta` is the cached root-layer metadata (read-only rows). `current_mode` is
/// the session's *live* shadow mode — pass `session.stage_shadow_mode()`, not
/// `meta.shadow_mode`, so the selector reflects in-session author writes. When
/// `!editable` the shadow selector is shown disabled (the product forbids
/// shadow authoring). `physics` is the live physics state
/// (`session.physics_scene_state()`); when `!physics_editable` the physics
/// controls render disabled (read-only products). Returns `Some(action)` only on
/// the frame the user changes something.
pub fn stage_panel(
    ui: &mut Ui,
    meta: &StageMetadata,
    current_mode: ShadowMode,
    editable: bool,
    physics: &PhysicsPanelState,
    physics_editable: bool,
) -> Option<StageAction> {
    let mut action = None;

    // ── Stage — read-only root-layer metadata (collapsible) ─────────────────
    section(ui, "stage_section_stage", icons::GLOBE, "Stage", |ui| {
        stage_metadata_body(ui, meta);
    });

    // ── Shadows — authoring (collapsible) ───────────────────────────────────
    section(ui, "stage_section_shadows", icons::SHADED, "Shadows", |ui| {
        if let Some(a) = shadow_section(ui, current_mode, editable) {
            action = Some(a);
        }
    });

    // ── Physics — authoring (collapsible) ───────────────────────────────────
    section(ui, "stage_section_physics", icons::GLOBE, "Physics", |ui| {
        if let Some(a) = physics_section(ui, physics, physics_editable) {
            action = Some(a);
        }
    });

    action
}

/// Renders one collapsible Stage-tab section: a default toggle triangle, the
/// section's row icon, and its bold title, with the given `body` as the
/// collapsible content. A stable persistent id (`id`) keeps each section's
/// open/closed state across frames; sections default to open.
fn section(
    ui: &mut Ui,
    id: &str,
    icon: egui::ImageSource<'static>,
    title: &str,
    body: impl FnOnce(&mut Ui),
) {
    let header_id = ui.make_persistent_id(id);
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), header_id, true)
        .show_header(ui, |ui| {
            ui.add(icons::icon_image(ui, icon, ICON_SIZE));
            ui.label(RichText::new(title).strong());
        })
        .body(body);
    ui.add_space(6.0);
}

/// Renders the read-only root-layer metadata grid (default prim, up-axis,
/// meters-per-unit, optional time range, root identifier).
fn stage_metadata_body(ui: &mut Ui, meta: &StageMetadata) {
    ui.label(
        RichText::new("open stage · root-layer metadata")
            .small()
            .color(dim()),
    );
    ui.add_space(4.0);

    egui::Grid::new("stage_metadata_grid")
        .num_columns(2)
        .striped(true)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            metadata_row(
                ui,
                "Default Prim",
                meta.default_prim.as_deref().unwrap_or("—"),
            );
            metadata_row(ui, "Up Axis", &meta.up_axis);
            metadata_row(ui, "Meters / Unit", &format!("{:.6}", meta.meters_per_unit));
            if meta.has_time_code_range {
                metadata_row(
                    ui,
                    "Time Range",
                    &format!("{} – {}", meta.start_time_code, meta.end_time_code),
                );
                metadata_row(
                    ui,
                    "Codes / Second",
                    &format!("{}", meta.time_codes_per_second),
                );
            } else {
                metadata_row(ui, "Time Range", "— (none authored)");
            }
            metadata_row_path(ui, "Root Layer", &meta.root_identifier);
        });
}

/// Renders the shadow-mode authoring body (four-state selector + description).
/// Interactive only when `editable`. Returns [`StageAction::SetShadowMode`] on a
/// change.
fn shadow_section(ui: &mut Ui, current_mode: ShadowMode, editable: bool) -> Option<StageAction> {
    let mut action = None;

    ui.label(
        RichText::new("root-layer opt-in · customLayerData[\"idtx:shadowMode\"]")
            .small()
            .color(dim()),
    );
    ui.add_space(4.0);

    if !editable {
        ui.label(
            RichText::new("shadow authoring is unavailable in this product")
                .small()
                .weak(),
        );
    }

    ui.add_enabled_ui(editable, |ui| {
        ui.horizontal(|ui| {
            for (mode, label) in [
                (ShadowMode::Off, "Off"),
                (ShadowMode::Hard, "Hard"),
                (ShadowMode::Smooth, "Smooth"),
                (ShadowMode::Soft, "Soft"),
            ] {
                let selected = current_mode == mode;
                if ui.selectable_label(selected, label).clicked() && !selected {
                    action = Some(StageAction::SetShadowMode(mode));
                }
            }
        });
    });

    ui.add_space(4.0);
    ui.label(
        RichText::new(match current_mode {
            ShadowMode::Off => "Off — no shadows rendered.",
            ShadowMode::Hard => "Hard — crisp single-tap edges, no blur or jitter.",
            ShadowMode::Smooth => "Smooth — softened 3×3 PCF contact edge.",
            ShadowMode::Soft => "Soft — variable-penumbra PCSS shadows.",
        })
        .small()
        .color(dim()),
    );

    action
}

/// An action the stage-level **Behavior** section wants the shell to perform.
/// The behavior's own config/edit/check/detach flow through the [`EditSink`]
/// (driven against `/StageBehavior` by the reused behavior-panel body); these
/// are only the stage-specific lifecycle steps the shell must orchestrate.
#[derive(Debug, Clone, PartialEq)]
pub enum StageBehaviorAction {
    /// Create a new source-authored stage behavior: define `/StageBehavior`,
    /// author the root-layer `idtx:stageBehavior` metadata, scaffold a `.rs`
    /// source, and set `idtx:behavior:scope = "stage"`.
    Create,
    /// Attach an existing `.rs` behavior source to the stage (same preamble as
    /// [`Self::Create`], picking a source file instead of scaffolding one).
    Attach,
    /// Author a new run-condition token for the stage behavior.
    SetRunScope(StageBehaviorRunScope),
}

/// Renders the stage-level **Behavior** section for the Stage inspector tab and
/// returns an optional [`StageBehaviorAction`].
///
/// When no stage behavior is assigned (`prim_info` is `None`, or the prim lacks
/// `IdtxBehaviorAPI`) it offers **Create / Attach** buttons (Conductor only —
/// `mode == Cooking`). When one is assigned it renders the run-condition
/// selector (`Standalone`/`Payload`/`Sublayer`/`Always`, with a note that
/// reference-composed layers never run) followed by the full prim behavior-panel
/// body ([`render_behavior_body`]) driven against the `/StageBehavior` prim —
/// so Edit Code / Cargo Check / enable/kind/server-auth / Manifest config /
/// Detach all work exactly as on a prim, flowing through `sink`.
pub fn stage_behavior_panel(
    ui: &mut Ui,
    prim_info: Option<&PrimInfo>,
    properties: &[PropertyRecord],
    run_scope: StageBehaviorRunScope,
    mode: InspectorMode,
    sink: Option<&mut dyn EditSink>,
) -> Option<StageBehaviorAction> {
    let mut action = None;
    section(ui, "stage_section_behavior", icons::GLOBE, "Behavior", |ui| {
        action = stage_behavior_body(ui, prim_info, properties, run_scope, mode, sink);
    });
    action
}

/// The collapsible body of [`stage_behavior_panel`] — everything below the
/// section header (the title is supplied by the enclosing collapsing header).
fn stage_behavior_body(
    ui: &mut Ui,
    prim_info: Option<&PrimInfo>,
    properties: &[PropertyRecord],
    run_scope: StageBehaviorRunScope,
    mode: InspectorMode,
    sink: Option<&mut dyn EditSink>,
) -> Option<StageBehaviorAction> {
    let mut action = None;

    ui.label(
        RichText::new("stage-wide behavior · /StageBehavior · customLayerData[\"idtx:stageBehavior\"]")
            .small()
            .color(dim()),
    );
    ui.add_space(4.0);

    let assigned = prim_info.is_some_and(|info| {
        info.applied_schemas.iter().any(|s| s == BEHAVIOR_SCHEMA)
    });
    let cookable = matches!(mode, InspectorMode::Cooking);

    if !assigned {
        ui.label(RichText::new("No stage behavior assigned.").small().weak());
        if cookable {
            ui.horizontal(|ui| {
                if ui
                    .button("✨ Create Stage Behavior…")
                    .on_hover_text(
                        "Create a /StageBehavior prim carrying IdtxBehaviorAPI and a new Rust \
                         source (compiled before preview / at packaging)",
                    )
                    .clicked()
                {
                    action = Some(StageBehaviorAction::Create);
                }
                if ui
                    .button("➕ Attach Stage Behavior…")
                    .on_hover_text("Attach an existing .rs behavior source as the stage behavior")
                    .clicked()
                {
                    action = Some(StageBehaviorAction::Attach);
                }
            });
        } else {
            ui.label(
                RichText::new("stage-behavior authoring is unavailable in this product")
                    .small()
                    .weak(),
            );
        }
        return action;
    }

    // Assigned: run-condition selector, then the reused behavior-panel body.
    if let Some(a) = run_scope_selector(ui, run_scope, cookable) {
        action = Some(a);
    }

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    if let (Some(info), Some(sink)) = (prim_info, sink) {
        let mut ctx = InspectorContext {
            info,
            properties,
            mode,
            sink: Some(sink),
            skeleton_joints: None,
            collision_hull_generating: false,
        };
        render_behavior_body(ui, &mut ctx);
    }

    action
}

/// The run-condition selector: a 4-way segmented choice plus a note that a
/// layer composed as a **reference** never runs its stage behavior. Interactive
/// only when `editable` (Conductor). Returns [`StageBehaviorAction::SetRunScope`]
/// on a change.
fn run_scope_selector(
    ui: &mut Ui,
    current: StageBehaviorRunScope,
    editable: bool,
) -> Option<StageBehaviorAction> {
    let mut action = None;

    ui.label(RichText::new("Run condition").small().color(dim()));
    ui.add_enabled_ui(editable, |ui| {
        ui.horizontal(|ui| {
            for (scope, label) in [
                (StageBehaviorRunScope::Standalone, "Standalone"),
                (StageBehaviorRunScope::Payload, "Payload"),
                (StageBehaviorRunScope::Sublayer, "Sublayer"),
                (StageBehaviorRunScope::Always, "Always"),
            ] {
                let selected = current == scope;
                if ui.selectable_label(selected, label).clicked() && !selected {
                    action = Some(StageBehaviorAction::SetRunScope(scope));
                }
            }
        });
    });
    ui.label(
        RichText::new(match current {
            StageBehaviorRunScope::Standalone => {
                "Standalone — runs only when this layer is opened directly as the stage root."
            }
            StageBehaviorRunScope::Payload => {
                "Payload — runs only when this layer is pulled in via a payload arc."
            }
            StageBehaviorRunScope::Sublayer => {
                "Sublayer — runs only when this layer is used as a sublayer."
            }
            StageBehaviorRunScope::Always => {
                "Always — runs when standalone, payloaded, or sublayered."
            }
        })
        .small()
        .color(dim()),
    );
    ui.label(
        RichText::new("A layer composed as a reference never runs its stage behavior.")
            .small()
            .weak(),
    );

    action
}

/// Renders the Physics section body: an Activate button when no `PhysicsScene`
/// prim exists, or a Deactivate button plus editable gravity when one does.
/// The section title is supplied by the enclosing collapsing header. Returns
/// the physics [`StageAction`] the user triggered this frame, if any.
fn physics_section(
    ui: &mut Ui,
    physics: &PhysicsPanelState,
    editable: bool,
) -> Option<StageAction> {
    let mut action = None;

    ui.label(
        RichText::new("runtime rigid-body sim · UsdPhysicsScene")
            .small()
            .color(dim()),
    );
    ui.add_space(4.0);

    if !editable {
        ui.label(
            RichText::new("physics authoring is unavailable in this product")
                .small()
                .weak(),
        );
    }

    ui.add_enabled_ui(editable, |ui| {
        if physics.active {
            if ui.button("Deactivate Physics").clicked() {
                action = Some(StageAction::DeactivatePhysics);
            }
            ui.add_space(6.0);

            if let Some(a) = gravity_direction_editor(ui, physics.gravity_direction) {
                action = Some(a);
            }
            ui.add_space(4.0);
            if let Some(a) = gravity_magnitude_editor(ui, physics.gravity_magnitude) {
                action = Some(a);
            }
        } else if ui.button("Activate Physics").clicked() {
            action = Some(StageAction::ActivatePhysics);
        }
    });

    ui.add_space(4.0);
    ui.label(
        RichText::new(if physics.active {
            "Active — a UsdPhysicsScene prim drives the runtime simulation."
        } else {
            "Inactive — activate to author a UsdPhysicsScene and enable simulation."
        })
        .small()
        .color(dim()),
    );

    action
}

/// Three `DragValue`s authoring `physics:gravityDirection` (`vector3f`).
///
/// `current` is the live value (re-read from the session each frame, so a drag
/// accumulates because the live-author path below keeps the session current).
/// Streams `SetGravityDirection { commit: false }` while dragging and emits one
/// `{ commit: true }` on release / focus-loss, carrying the gesture's pre-edit
/// `before` (held in egui temp memory) so the journal entry spans the gesture.
fn gravity_direction_editor(ui: &mut Ui, current: [f32; 3]) -> Option<StageAction> {
    let before_id = egui::Id::new("stage_physics_gravity_dir_before");
    let mut value = current;
    let mut changed = false;
    let mut committed = false;
    let mut started = false;

    ui.label(RichText::new("Gravity Direction").small().color(dim()));
    ui.horizontal(|ui| {
        for (i, label) in ["X", "Y", "Z"].iter().enumerate() {
            ui.label(RichText::new(*label).small().color(dim()));
            let resp = ui.add(egui::DragValue::new(&mut value[i]).speed(0.01));
            changed |= resp.changed();
            started |= resp.drag_started() || resp.gained_focus();
            committed |= resp.drag_stopped() || resp.lost_focus();
        }
    });

    gravity_action(
        ui,
        before_id,
        current,
        value,
        changed,
        started,
        committed,
        |before, value, commit| StageAction::SetGravityDirection {
            before,
            value,
            commit,
        },
    )
}

/// A single `DragValue` authoring `physics:gravityMagnitude` (`float`), with the
/// same live-author / commit-journal gesture handling as the direction editor.
fn gravity_magnitude_editor(ui: &mut Ui, current: f32) -> Option<StageAction> {
    let before_id = egui::Id::new("stage_physics_gravity_mag_before");
    let mut value = current;
    let mut changed = false;
    let mut committed = false;
    let mut started = false;

    ui.horizontal(|ui| {
        ui.label(RichText::new("Gravity Magnitude").small().color(dim()));
        let resp = ui.add(
            egui::DragValue::new(&mut value)
                .speed(0.05)
                .range(0.0..=f32::MAX),
        );
        changed = resp.changed();
        started = resp.drag_started() || resp.gained_focus();
        committed = resp.drag_stopped() || resp.lost_focus();
    });

    gravity_action(
        ui,
        before_id,
        current,
        value,
        changed,
        started,
        committed,
        |before, value, commit| StageAction::SetGravityMagnitude {
            before,
            value,
            commit,
        },
    )
}

/// Shared gesture bookkeeping for a gravity field: capture the pre-edit `before`
/// in egui temp memory at the start of a gesture, emit a live (non-committing)
/// action while the value changes, and emit the single committing action on
/// release / focus-loss. Generic over the value type `T` and the `StageAction`
/// constructor so the direction (`[f32; 3]`) and magnitude (`f32`) editors share
/// one implementation.
#[allow(clippy::too_many_arguments)]
fn gravity_action<T: Clone + Send + Sync + 'static>(
    ui: &mut Ui,
    before_id: egui::Id,
    current: T,
    value: T,
    changed: bool,
    started: bool,
    committed: bool,
    make: impl Fn(Option<T>, T, bool) -> StageAction,
) -> Option<StageAction> {
    // Capture `before` once, at the first frame of the gesture.
    let stored_before: Option<T> = ui.data(|d| d.get_temp(before_id));
    if stored_before.is_none() && (started || changed) {
        ui.data_mut(|d| d.insert_temp(before_id, current.clone()));
    }
    let before: Option<T> = ui
        .data(|d| d.get_temp(before_id))
        .or(Some(current));

    if committed {
        ui.data_mut(|d| d.remove::<T>(before_id));
        Some(make(before, value, true))
    } else if changed {
        Some(make(before, value, false))
    } else {
        None
    }
}

/// A single `label : value` metadata row inside the grid.
fn metadata_row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).small().color(dim()));
    ui.label(RichText::new(value).small().monospace());
    ui.end_row();
}

/// Like [`metadata_row`], but for a long single-line value such as a filesystem
/// path: the value renders at full width (revealing the full text on hover) and,
/// when wider than the panel, is reachable via the Stage tab's horizontal
/// scrollbar rather than being clipped.
fn metadata_row_path(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).small().color(dim()));
    ui.add(egui::Label::new(RichText::new(value).small().monospace()))
        .on_hover_text(value);
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> StageMetadata {
        StageMetadata {
            root_identifier: "root.usda".to_string(),
            default_prim: Some("/World".to_string()),
            up_axis: "Y".to_string(),
            meters_per_unit: 1.0,
            start_time_code: 0.0,
            end_time_code: 0.0,
            time_codes_per_second: 24.0,
            has_time_code_range: false,
            shadow_mode: ShadowMode::Off,
            stage_behavior_name: None,
            stage_behavior_run_scope: None,
        }
    }

    /// Drives the panel headlessly and returns the action it emitted, if any.
    ///
    /// egui `selectable_label` clicks need pointer-input plumbing the headless
    /// context does not replay cleanly, so these tests cover the pure return
    /// shape (no interaction → `None`, disabled → no panic/action); the
    /// click→`SetShadowMode` mapping is exercised manually in Conductor and by
    /// the shell round-trip test.
    fn run(meta: &StageMetadata, current: ShadowMode, editable: bool) -> Option<StageAction> {
        let mut captured = None;
        let physics = PhysicsPanelState::default();
        egui::__run_test_ui(|ui| {
            captured = stage_panel(ui, meta, current, editable, &physics, editable);
        });
        captured
    }

    #[test]
    fn returns_none_without_interaction() {
        let meta = sample_meta();
        assert_eq!(run(&meta, ShadowMode::Off, true), None);
        assert_eq!(run(&meta, ShadowMode::Soft, false), None);
    }

    #[test]
    fn disabled_panel_still_renders() {
        // A non-editable panel must not panic and must emit no action.
        let meta = sample_meta();
        assert_eq!(run(&meta, ShadowMode::Hard, false), None);
    }

    #[test]
    fn active_physics_panel_renders_without_action() {
        // An active physics panel (deactivate button + gravity editors) renders
        // headlessly without panicking and emits no action absent interaction.
        let meta = sample_meta();
        let physics = PhysicsPanelState {
            active: true,
            gravity_direction: [0.0, -1.0, 0.0],
            gravity_magnitude: 9.81,
        };
        let mut captured = None;
        egui::__run_test_ui(|ui| {
            captured = stage_panel(ui, &meta, ShadowMode::Off, true, &physics, true);
        });
        assert_eq!(captured, None);
    }

    /// The stage-behavior section renders headlessly in both the unassigned
    /// (Create/Attach) and assigned (run-condition + reused body) states without
    /// panicking, and emits no action absent interaction.
    #[test]
    fn stage_behavior_panel_renders_both_states_without_action() {
        // Unassigned: no prim → Create/Attach buttons (Cooking mode).
        let mut captured = Some(StageBehaviorAction::Create);
        egui::__run_test_ui(|ui| {
            captured = stage_behavior_panel(
                ui,
                None,
                &[],
                StageBehaviorRunScope::Standalone,
                InspectorMode::Cooking,
                None,
            );
        });
        assert_eq!(captured, None);

        // Assigned: a /StageBehavior prim carrying IdtxBehaviorAPI → run-condition
        // selector + reused behavior body.
        let info = PrimInfo {
            path: "/StageBehavior".to_owned(),
            applied_schemas: vec![BEHAVIOR_SCHEMA.to_owned()],
            is_locally_authored: true,
            ..Default::default()
        };
        let mut captured = Some(StageBehaviorAction::Create);
        egui::__run_test_ui(|ui| {
            captured = stage_behavior_panel(
                ui,
                Some(&info),
                &[],
                StageBehaviorRunScope::Always,
                InspectorMode::Cooking,
                None,
            );
        });
        assert_eq!(captured, None);
    }
}
