pub mod behavior;
pub mod animation;
pub mod camera;
pub mod collision;
pub mod common;
pub mod fallback;
pub mod light;
pub mod material;
pub mod mesh;
pub mod packed_stage;
pub mod prim_header;
pub mod relationships;
pub mod skeleton;
pub mod transform;
pub mod variant_sets;

// Primitive shape panels — one per UsdGeomGprim sub-type.
pub mod capsule;
pub mod cone;
pub mod cube;
pub mod cylinder;
pub mod plane;
pub mod sphere;

// ── Shared helpers for primitive-shape panels ─────────────────────────────────
//
// Private to this module and its children; each panel imports via `super::`.
use egui::Ui;
use idtx_editor_model::AttributeValue;
use idtx_usd::PropertyRecord;
use std::cell::Cell;

use crate::inspector::{InspectorContext, widgets::attribute_row_with_value_renderer};

/// Parse the bridge's `value_summary` string for a `double` attribute.
///
/// Handles plain numbers (`"2"`, `"0.5"`, `"1e-3"`) as emitted by the C++
/// bridge.  Returns `None` when the string is not a valid decimal float.
fn parse_f64_summary(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Parse the bridge's `value_summary` string for a `float` attribute.
///
/// Handles plain numbers (`"2"`, `"0.5"`, `"1e-3"`) as emitted by the C++
/// bridge.  Returns `None` when the string is not a valid decimal float.
fn parse_f32_summary(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok()
}

/// Render a non-negative `double` USD attribute as an editable `DragValue`.
///
/// Streams the live value to USD via [`InspectorContext::author_attribute`] on
/// every change (immediate viewport feedback), and records the confirmed
/// gesture once via [`InspectorContext::commit_attribute`] when a field loses
/// focus / Enter is pressed / a drag ends.
///
/// The working value and the pre-edit value are held in egui temp memory for
/// the duration of the gesture (keyed by prim path + attribute). Without this,
/// `new_value` would re-seed from the property `value_summary` every frame —
/// which is *not* refreshed mid-gesture — so a drag could never accumulate, and
/// the committed `before` would collapse onto the just-authored value, making
/// the journal entry a no-op that is silently dropped. This mirrors the
/// transform panel's hold-across-gesture handling.
///
/// `default` is the USD schema default used when the attribute has no authored
/// value yet.  `speed` is the pixels-per-unit drag sensitivity.
fn draw_f64_attr(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    prop: &PropertyRecord,
    attr_name: &str,
    default: f64,
    speed: f64,
) {
    let current = parse_f64_summary(&prop.value_summary).unwrap_or(default);

    // Temp-memory keys for the in-flight gesture, unique per prim + attribute.
    let path = ctx.info.path.clone();
    let work_id = egui::Id::new(("attr_edit_work", path.as_str(), attr_name));
    let before_id = egui::Id::new(("attr_edit_before", path.as_str(), attr_name));

    // Seed from the held working value while a gesture is in flight; otherwise
    // from the current authored value.
    let stored_work: Option<f64> = ui.data(|d| d.get_temp(work_id));
    let mut new_value = stored_work.unwrap_or(current);

    let did_change = Cell::new(false);
    let edit_started = Cell::new(false);
    let committed = Cell::new(false);

    attribute_row_with_value_renderer(ui, ctx, prop, |ui, _prop| {
        let resp = ui.add(
            egui::DragValue::new(&mut new_value)
                .speed(speed)
                .range(0.0..=f64::MAX),
        );
        did_change.set(resp.changed());
        if resp.gained_focus() || resp.drag_started() {
            edit_started.set(true);
        }
        if resp.lost_focus() || resp.drag_stopped() {
            committed.set(true);
        }
        resp
    });

    // Capture the pre-edit value once, at the first frame of the gesture, so the
    // committed before→after pair spans the whole gesture. `Option<f64>`
    // preserves the "was not previously authored" case (→ `before = None`).
    let stored_before: Option<Option<f64>> = ui.data(|d| d.get_temp(before_id));
    if stored_before.is_none() && (edit_started.get() || did_change.get()) {
        let pre = prop.authored.then_some(current);
        ui.data_mut(|d| d.insert_temp(before_id, pre));
    }
    let before = stored_before
        .unwrap_or_else(|| prop.authored.then_some(current))
        .map(AttributeValue::F64);

    // Persist the working value until the gesture confirms so a live drag
    // accumulates across frames; drop it once committed.
    if committed.get() {
        ui.data_mut(|d| d.remove::<f64>(work_id));
    } else if edit_started.get() || did_change.get() || stored_work.is_some() {
        ui.data_mut(|d| d.insert_temp(work_id, new_value));
    }

    // Stream the live value to USD each frame it changes; never touches journal.
    if did_change.get() {
        ctx.author_attribute(attr_name, before.clone(), AttributeValue::F64(new_value));
    }

    // On confirm, journal the whole gesture as a single before→after entry.
    if committed.get() {
        ctx.commit_attribute(attr_name, before, AttributeValue::F64(new_value));
        ui.data_mut(|d| d.remove::<Option<f64>>(before_id));
    }
}

/// Render a non-negative `float` USD attribute as an editable `DragValue`.
///
/// Streams the live value to USD via [`InspectorContext::author_attribute`] on
/// every change (immediate viewport feedback), and records the confirmed
/// gesture once via [`InspectorContext::commit_attribute`] when a field loses
/// focus / Enter is pressed / a drag ends.
///
/// The working value and the pre-edit value are held in egui temp memory for
/// the duration of the gesture (keyed by prim path + attribute). Without this,
/// `new_value` would re-seed from the property `value_summary` every frame —
/// which is *not* refreshed mid-gesture — so a drag could never accumulate, and
/// the committed `before` would collapse onto the just-authored value, making
/// the journal entry a no-op that is silently dropped. This mirrors the
/// transform panel's hold-across-gesture handling.
///
/// `default` is the USD schema default used when the attribute has no authored
/// value yet.  `speed` is the pixels-per-unit drag sensitivity.
fn draw_f32_attr(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    prop: &PropertyRecord,
    attr_name: &str,
    default: f32,
    speed: f32,
) {
    let current = parse_f32_summary(&prop.value_summary).unwrap_or(default);

    // Temp-memory keys for the in-flight gesture, unique per prim + attribute.
    let path = ctx.info.path.clone();
    let work_id = egui::Id::new(("attr_edit_work", path.as_str(), attr_name));
    let before_id = egui::Id::new(("attr_edit_before", path.as_str(), attr_name));

    // Seed from the held working value while a gesture is in flight; otherwise
    // from the current authored value.
    let stored_work: Option<f32> = ui.data(|d| d.get_temp(work_id));
    let mut new_value = stored_work.unwrap_or(current);

    let did_change = Cell::new(false);
    let edit_started = Cell::new(false);
    let committed = Cell::new(false);

    attribute_row_with_value_renderer(ui, ctx, prop, |ui, _prop| {
        let resp = ui.add(
            egui::DragValue::new(&mut new_value)
                .speed(speed)
                .range(0.0..=f32::MAX),
        );
        did_change.set(resp.changed());
        if resp.gained_focus() || resp.drag_started() {
            edit_started.set(true);
        }
        if resp.lost_focus() || resp.drag_stopped() {
            committed.set(true);
        }
        resp
    });

    // Capture the pre-edit value once, at the first frame of the gesture, so the
    // committed before→after pair spans the whole gesture. `Option<f64>`
    // preserves the "was not previously authored" case (→ `before = None`).
    let stored_before: Option<Option<f32>> = ui.data(|d| d.get_temp(before_id));
    if stored_before.is_none() && (edit_started.get() || did_change.get()) {
        let pre = prop.authored.then_some(current);
        ui.data_mut(|d| d.insert_temp(before_id, pre));
    }
    let before = stored_before
        .unwrap_or_else(|| prop.authored.then_some(current))
        .map(AttributeValue::F32);

    // Persist the working value until the gesture confirms so a live drag
    // accumulates across frames; drop it once committed.
    if committed.get() {
        ui.data_mut(|d| d.remove::<f32>(work_id));
    } else if edit_started.get() || did_change.get() || stored_work.is_some() {
        ui.data_mut(|d| d.insert_temp(work_id, new_value));
    }

    // Stream the live value to USD each frame it changes; never touches journal.
    if did_change.get() {
        ctx.author_attribute(attr_name, before.clone(), AttributeValue::F32(new_value));
    }

    // On confirm, journal the whole gesture as a single before→after entry.
    if committed.get() {
        ctx.commit_attribute(attr_name, before, AttributeValue::F32(new_value));
        ui.data_mut(|d| d.remove::<Option<f32>>(before_id));
    }
}

/// Render the `axis` token attribute as a `ComboBox` with X / Y / Z choices.
///
/// A selection change is a single discrete edit, so it is committed directly
/// via [`InspectorContext::commit_attribute`] (which authors *and* journals) —
/// there is no intermediate drag gesture to stream.
fn draw_axis_attr(ui: &mut Ui, ctx: &mut InspectorContext<'_>, prop: &PropertyRecord) {
    let before_token = prop.value_summary.trim().to_owned();
    let mut selected = before_token.clone();

    attribute_row_with_value_renderer(ui, ctx, prop, |ui, _prop| {
        egui::ComboBox::from_id_salt(("axis_combo", prop.name.as_str()))
            .selected_text(selected.as_str())
            .show_ui(ui, |ui| {
                for &ax in &["X", "Y", "Z"] {
                    ui.selectable_value(&mut selected, ax.to_owned(), ax);
                }
            })
            .response
    });

    if selected != before_token {
        let before = if prop.authored {
            Some(AttributeValue::Token(before_token))
        } else {
            None
        };
        ctx.commit_attribute("axis", before, AttributeValue::Token(selected));
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::parse_f64_summary;

    #[test]
    fn parses_integer_string() {
        assert_eq!(parse_f64_summary("2"), Some(2.0));
    }

    #[test]
    fn parses_decimal_string() {
        assert!((parse_f64_summary("0.5").unwrap() - 0.5_f64).abs() < 1e-9);
    }

    #[test]
    fn parses_whitespace_padded() {
        assert!((parse_f64_summary("  1.0  ").unwrap() - 1.0_f64).abs() < 1e-9);
    }

    #[test]
    fn parses_scientific_notation() {
        assert!((parse_f64_summary("1e0").unwrap() - 1.0_f64).abs() < 1e-9);
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(parse_f64_summary("not a number").is_none());
        assert!(parse_f64_summary("").is_none());
    }
}
