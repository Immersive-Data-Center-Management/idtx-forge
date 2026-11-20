//! Reusable egui widgets for displaying and editing USD attribute values.
//!
//! Most widgets render a value read-only; editable variants such as
//! [`attribute_row_with_colorpicker`] stream the edited value back to USD and
//! journal a single before→after entry per gesture.
//!
//! # Opinion provenance (USD layer visualization)
//!
//! Every attribute rendered through [`attribute_row`] gets *opinion provenance*
//! for free: a compact chip at the end of the value showing the winning layer,
//! and — for attributes that actually have authored opinions — a click-to-expand
//! **opinion stack** listing every contributing layer strongest-first (the
//! winner highlighted, weaker opinions struck through) plus the resolved
//! `value_source`.  The stack renders as its own full-width band beneath the
//! attribute line, not squeezed into the value column.
//!
//! Attributes whose value comes only from the schema fallback (nothing authored)
//! have no stack to reveal — see [`has_opinion_stack`] — so they get no expand
//! affordance and no leading stack icon.
//!
//! This is intentionally **not opt-in**: any inspector panel that renders its
//! attributes with the shared [`attribute_row`] widget gets the provenance chip
//! and the expandable opinion stack automatically, with no extra code.  Panels
//! that build bespoke rows can still reuse the same behaviour by calling
//! [`provenance_chip`] and [`opinion_stack`] directly.

use std::cell::Cell;

use egui::{
    Align, Color32, CornerRadius, Frame, Label, Layout, Margin, Response, RichText, Sense, Stroke,
    Ui, Vec2,
};
use idtx_editor_model::AttributeValue;
use idtx_usd::PropertyRecord;

use crate::{icons, inspector::InspectorContext};

/// Colour for the *winning* opinion / provenance chip.  A medium green chosen
/// to read clearly against both the light and dark egui themes.
fn winner_green() -> Color32 {
    crate::theme::current().status_ok
}

/// Fixed column widths (logical px) for the opinion-stack rows.  Keeping the
/// marker and layer columns a fixed width is what makes the columns line up
/// vertically across every row of the stack; the path/value columns fill the
/// remaining width.
const STACK_MARKER_W: f32 = 30.0;
const STACK_LAYER_W: f32 = 140.0;
/// On-screen size of the small inline SVG icons used inside the stack / chips.
const STACK_ICON: f32 = 12.0;
/// Nominal row height used to size the fixed columns.
const STACK_ROW_H: f32 = 16.0;

/// Hard cap on the number of characters rendered for any attribute value.
///
/// Value labels render at their full natural width (no per-widget truncation)
/// so the enclosing scroll area can surface a horizontal scrollbar for a value
/// wider than the panel. This cap keeps that width *bounded*: inside an
/// `egui::Grid` a label's measured width locks the column, so an uncapped array
/// dump (thousands of `points` / face indices) would widen the column to an
/// enormous, barely-scrollable extent. Array types already collapse to their
/// element count (`[N]`); this cap guards every other value so the widest a
/// column can grow is [`MAX_VALUE_CHARS`] characters.
const MAX_VALUE_CHARS: usize = 96;

/// Elide an attribute value summary to [`MAX_VALUE_CHARS`], appending `…` when
/// truncated.  Counts by `char` (not bytes) so multi-byte UTF-8 is never split
/// mid-codepoint.  Returns the input unchanged when it already fits.
pub(crate) fn elide_value(s: &str) -> std::borrow::Cow<'_, str> {
    // Fast path: cheap byte-length pre-check avoids scanning short strings.
    if s.len() <= MAX_VALUE_CHARS {
        return std::borrow::Cow::Borrowed(s);
    }
    match s.char_indices().nth(MAX_VALUE_CHARS) {
        Some((byte_idx, _)) => std::borrow::Cow::Owned(format!("{}…", &s[..byte_idx])),
        None => std::borrow::Cow::Borrowed(s),
    }
}

// ── Top-level dispatch ────────────────────────────────────────────────────────

/// Render a single property value inside a grid cell, applying the value-column
/// rules:
///
/// * **Array types** (`float3[]`, `token[]`, …) collapse to their entry count,
///   e.g. `[300]` — never the full element dump.
/// * **Matrix types** (`matrix4d`, …) render but truncate at the column
///   boundary via the widget's `.truncate()`.
/// * **Simple types** (numbers, `float3`, `color3f`, `double3`, …) render their
///   value directly.  `bool`, `token` and `asset` keep their bespoke widgets.
pub fn value_label(
    ui: &mut Ui,
    prop: &PropertyRecord,
    render_value: Option<impl FnOnce(&mut Ui, &PropertyRecord) -> Response>,
) -> Response {
    if let Some(value_renderer) = render_value {
        return value_renderer(ui, prop);
    }

    let type_name = prop.type_name.as_str();

    // Array types render only their entry count.
    if type_name.ends_with("[]") {
        return array_count_label(ui, &prop.value_summary);
    }

    let elided = elide_value(prop.value_summary.as_str());
    let s = elided.as_ref();

    // Matrices render but truncate at the column boundary.
    if is_matrix_type(type_name) {
        return matrix_label(ui, s);
    }

    match type_name {
        "bool" => bool_label(ui, s),
        "token" => token_chip(ui, s),
        "asset" => asset_chip(ui, s),
        "string" | "path" => string_label(ui, s),
        // numbers, vecN and colorN all render as truncating monospace.
        _ => monospace_label(ui, s),
    }
}

/// Return the display string for a property value, applying the array/matrix
/// value-column rules (see [`value_label`]).  Array types collapse to their
/// entry count (`[300]`); everything else is elided to [`MAX_VALUE_CHARS`], with
/// matrices left for the rendering widget to truncate at the column boundary.
pub(crate) fn display_value<'a>(type_name: &str, summary: &'a str) -> std::borrow::Cow<'a, str> {
    if type_name.ends_with("[]") {
        let label = match array_count(summary) {
            Some(n) => format!("[{n}]"),
            None => "[…]".to_owned(),
        };
        return std::borrow::Cow::Owned(label);
    }
    elide_value(summary)
}

/// Parse the leading `"[N values]"` prefix the bridge prepends to array value
/// summaries, returning `N`.  Returns `None` for non-array summaries.
fn array_count(summary: &str) -> Option<u64> {
    let rest = summary.strip_prefix('[')?;
    let end = rest.find(" values]")?;
    rest[..end].trim().parse().ok()
}

/// Whether `type_name` is a USD matrix/frame type (rendered but truncated).
fn is_matrix_type(type_name: &str) -> bool {
    matches!(type_name, "matrix2d" | "matrix3d" | "matrix4d" | "frame4d")
}

/// On-screen size of the leading stack icon / disclosure toggle in front of the
/// attribute name.  Matches the tree/inspector row icon metrics.
const NAME_ICON: f32 = icons::DEFAULT_ICON_SIZE;

/// Whether an attribute has an authored opinion stack worth revealing.
///
/// Attributes whose value comes only from the schema fallback (nothing authored
/// on any layer) have no stack — so they get no expand affordance and no leading
/// stack icon.
pub(crate) fn has_opinion_stack(prop: &PropertyRecord) -> bool {
    prop.authored && !prop.provenance.is_empty()
}

/// Render a self-contained attribute row: the attribute line (leading stack
/// toggle + bold name + type tag on the left, value + provenance chip on the
/// right) and — when expanded — the full-width [`opinion_stack`] band beneath it.
///
/// The row is *not* laid out in an `egui::Grid`; it manages its own line via
/// [`egui::Sides`] so the opinion stack can span the whole width beneath it.
/// The leading icon is a disclosure toggle only for attributes that actually
/// have an authored opinion stack (see [`has_opinion_stack`]); expansion state
/// is remembered per attribute name in egui's temporary memory.
pub fn attribute_row(ui: &mut Ui, ctx: &mut InspectorContext<'_>, prop: &PropertyRecord) {
    attribute_block(
        ui,
        ctx,
        prop,
        None::<fn(&mut Ui, &PropertyRecord) -> Response>,
    );
}

pub fn attribute_row_with_value_renderer(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    prop: &PropertyRecord,
    renderer: impl FnOnce(&mut Ui, &PropertyRecord) -> Response,
) {
    attribute_block(ui, ctx, prop, Some(renderer));
}

pub fn attribute_row_with_colorpicker(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    prop: &PropertyRecord,
    editable: bool,
) {
    if editable {
        // Hold the working colour and the pre-edit value in
        // egui temp memory for the whole gesture. The colour
        // popup emits a burst of `changed()` events while a
        // slider is dragged; without holding state the button
        // would re-seed from the (stale, not refreshed
        // mid-gesture) summary each frame. We stream the live
        // colour to USD on every change and journal a single
        // before→after entry when the pointer is released.
        let path = ctx.info.path.clone();
        let work_id = egui::Id::new(("color_edit_work", path.as_str(), prop.name.as_str()));
        let before_id = egui::Id::new(("color_edit_before", path.as_str(), prop.name.as_str()));

        let summary_rgb = parse_color_summary(&prop.value_summary);
        let stored_work: Option<[f32; 3]> = ui.data(|d| d.get_temp(work_id));
        let mut rgb = stored_work.or(summary_rgb).unwrap_or([0.18, 0.18, 0.18]);
        let changed = Cell::new(false);

        attribute_row_with_value_renderer(ui, ctx, prop, |ui: &mut Ui, _prop| {
            let resp = ui.color_edit_button_rgb(&mut rgb);
            changed.set(resp.changed());
            resp
        });

        // Pre-edit value, captured once at gesture start.
        // `Option<[f32; 3]>` preserves the not-yet-authored
        // case (→ `before = None`).
        let stored_before: Option<Option<[f32; 3]>> = ui.data(|d| d.get_temp(before_id));
        let before = stored_before.unwrap_or(summary_rgb);

        if changed.get() {
            if stored_before.is_none() {
                ui.data_mut(|d| d.insert_temp(before_id, summary_rgb));
            }
            ui.data_mut(|d| d.insert_temp(work_id, rgb));
            ctx.author_attribute(
                prop.name.as_str(),
                before.map(AttributeValue::Color3f),
                AttributeValue::Color3f(rgb),
            );
        }

        // Commit once the picker interaction ends: a gesture
        // is in flight (before captured) and the pointer has
        // been released this frame with no further change.
        // Mirrors the drag-end commit used for scalar fields.
        let pointer_down = ui.input(|i| i.pointer.any_down());
        if stored_before.is_some() && !changed.get() && !pointer_down {
            ctx.commit_attribute(
                prop.name.as_str(),
                before.map(AttributeValue::Color3f),
                AttributeValue::Color3f(rgb),
            );
            ui.data_mut(|d| d.remove::<Option<[f32; 3]>>(before_id));
            ui.data_mut(|d| d.remove::<[f32; 3]>(work_id));
        }
    } else {
        // Display mode also renders the color button with color picker, but does not handle any
        // response/color changes
        attribute_row_with_value_renderer(ui, ctx, prop, |ui: &mut Ui, prop: &PropertyRecord| {
            let mut rgb = parse_color_summary(&prop.value_summary).unwrap_or([0.18, 0.18, 0.18]);
            ui.color_edit_button_rgb(&mut rgb)
        });
    }
    ui.end_row();
}

/// Shared body of [`attribute_row`] / [`attribute_row_with_value_renderer`].
fn attribute_block(
    ui: &mut Ui,
    ctx: &mut InspectorContext<'_>,
    prop: &PropertyRecord,
    render_value: Option<impl FnOnce(&mut Ui, &PropertyRecord) -> Response>,
) {
    let can_expand = has_opinion_stack(prop);
    let id = ui.make_persistent_id(("attr_opinion", prop.name.as_str()));
    // Only attributes with a real stack can be expanded — a schema-fallback
    // attribute never reveals anything even if a stale `true` lingers in memory.
    let mut expanded = can_expand && ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));

    // ── The attribute line: name/type then value/chip, left→right ──────────
    // Laid out left-to-right (rather than name-left / value-right justified) so
    // a long value extends past the panel edge and the enclosing scroll area
    // offers a horizontal scrollbar, instead of the value being clipped.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // Leading stack icon — a disclosure toggle when there is a stack to
        // reveal, otherwise a same-width spacer so names stay left-aligned.
        if can_expand {
            let tint = if expanded {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().text_color()
            };
            let resp = icons::icon_button_tinted(ui, icons::LAYER_ROOT, NAME_ICON, tint)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Show opinion stack (which layers contribute this value)");
            if resp.clicked() {
                expanded = !expanded;
                ui.data_mut(|d| d.insert_temp(id, expanded));
            }
        } else {
            ui.add_space(NAME_ICON);
        }
        ui.add(Label::new(RichText::new(&prop.name).strong()));
        type_tag(ui, &prop.type_name);
        value_label(ui, prop, render_value);
        provenance_chip(ui, prop);
        if prop.time_sample_count > 0 {
            time_sampled_badge(ui, prop.time_sample_count);
        }
    });

    // ── The opinion stack as a full-width band beneath the line ─────────────
    if expanded {
        opinion_stack(ui, ctx, prop);
    }
}

/// Render a relationship target row: bold name | "→ target" in accent colour.
pub fn relationship_row(ui: &mut Ui, prop: &PropertyRecord) {
    ui.label(RichText::new(&prop.name).strong().monospace().small());
    ui.label(RichText::new("relationship").weak().small().italics());
    ui.vertical(|ui| {
        for target in &prop.relationship_targets {
            ui.label(
                RichText::new(format!("→ {target}"))
                    .monospace()
                    .small()
                    .color(ui.visuals().hyperlink_color),
            );
        }
        if prop.relationship_targets.is_empty() {
            ui.label(RichText::new("(empty)").small().weak());
        }
    });
    ui.end_row();
}

// ── Opinion provenance (USD layer visualization) ─────────────────────

/// Compact, always-visible **pill** summarising where an attribute's value
/// comes from — appended after the value widget by [`attribute_row`].
///
/// * Authored with a known winning layer → green [`icons::PROVENANCE_WINNER`]
///   pill labelled with the winning layer's basename (full `layer:path` on
///   hover).
/// * Otherwise (schema fallback / no authored opinion) → a muted
///   [`icons::SCHEMA_FALLBACK`] pill labelled `schema fallback`.
pub fn provenance_chip(ui: &mut Ui, prop: &PropertyRecord) -> Response {
    let winner = if prop.authored {
        prop.provenance.first()
    } else {
        None
    };
    let (icon, text, color, hover) = match winner {
        Some(entry) => {
            let (layer_id, path) = split_provenance(entry);
            // A variant-scoped opinion's spec path carries a `{set=sel}` selector
            // (e.g. `/World/Cube{shading=red}.displayColor`). Surface it so the
            // value is visibly sourced from a variant, not a bare prim opinion.
            let variant = variant_selector_of(path);
            let text = match &variant {
                Some(sel) => format!("{} · {sel}", layer_basename(layer_id)),
                None => layer_basename(layer_id).to_owned(),
            };
            let hover = match &variant {
                Some(sel) => format!("{layer_id}\n{path}\nvia variant {sel}"),
                None => format!("{layer_id}\n{path}"),
            };
            (icons::PROVENANCE_WINNER, text, winner_green(), hover)
        }
        None => (
            icons::SCHEMA_FALLBACK,
            "schema fallback".to_owned(),
            dim_color(ui),
            value_source_detail(&prop.value_source),
        ),
    };

    let resp = Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add(icons::icon_image_tinted(icon, STACK_ICON, color));
                ui.add(Label::new(RichText::new(text).small().color(color)));
            });
        })
        .response;
    resp.interact(Sense::hover()).on_hover_text(hover)
}

/// The expandable **opinion stack**: every layer contributing an opinion for
/// this attribute, strongest-first, laid out as column-aligned boxed rows.  The
/// winner is a green-outlined box (green `✔` + value); weaker opinions are
/// muted, struck-through boxes.  A footer shows the resolved `value_source` and
/// the edit target.
///
/// Rendered beneath the value inside the same grid cell by [`attribute_row`];
/// reusable directly from bespoke rows.
pub fn opinion_stack(ui: &mut Ui, ctx: &mut InspectorContext<'_>, prop: &PropertyRecord) {
    ui.add_space(3.0);
    Frame::new()
        .fill(stack_band_fill(ui))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if prop.provenance.is_empty() {
                ui.label(
                    RichText::new("No authored opinions — value from schema fallback.")
                        .small()
                        .weak()
                        .italics(),
                );
            } else {
                ui.label(
                    RichText::new("OPINION STACK · strongest first")
                        .small()
                        .weak(),
                );
                ui.add_space(2.0);
                // The winner's sdf path anchors the "across a composition arc"
                // detection for the weaker rows.
                let winner_path = prop
                    .provenance
                    .first()
                    .map_or("", |entry| split_provenance(entry).1);
                for (rank, entry) in prop.provenance.iter().enumerate() {
                    let value = prop.provenance_values.get(rank).map_or("", String::as_str);
                    opinion_row(ui, rank, entry, value, &prop.type_name, winner_path);
                }
            }
            opinion_footer(ui, ctx, prop);
        });
}

/// One boxed, column-aligned row of the opinion stack.
///
/// Layout (fixed-width columns keep everything aligned across rows):
/// `[ ✔ rank ][ layer basename ][ sdf path … ][ value ]`.  The winner
/// (`rank == 0`) is green-outlined with a green value; weaker opinions are
/// muted and struck through.  `value` is the per-opinion authored value from
/// `PropertyRecord::provenance_values` (empty → nothing rendered).
fn opinion_row(
    ui: &mut Ui,
    rank: usize,
    entry: &str,
    value: &str,
    type_name: &str,
    winner_path: &str,
) {
    let (layer_id, path) = split_provenance(entry);
    let label = layer_basename(layer_id);
    let is_winner = rank == 0;
    // A weaker opinion whose spec lives at a different path than the winner is
    // reaching this prim across a composition arc (reference/payload/inherit).
    let via_arc = !is_winner && !path.is_empty() && !winner_path.is_empty() && path != winner_path;

    let (fill, stroke) = if is_winner {
        (
            Color32::from_rgba_unmultiplied(
                winner_green().r(),
                winner_green().g(),
                winner_green().b(),
                38,
            ),
            Stroke::new(1.0, winner_green()),
        )
    } else {
        (
            Color32::from_rgba_unmultiplied(128, 128, 128, 18),
            Stroke::NONE,
        )
    };

    Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;

                // Column A — status marker + rank.
                fixed_col(ui, STACK_MARKER_W, |ui| {
                    if is_winner {
                        ui.add(icons::icon_image_tinted(
                            icons::OK,
                            STACK_ICON,
                            winner_green(),
                        ));
                    }
                    let rank_text = RichText::new(format!("{}", rank + 1)).small();
                    ui.label(if is_winner {
                        rank_text.color(winner_green())
                    } else {
                        rank_text.weak()
                    });
                });

                // Column B — layer basename (+ composition-arc marker).
                fixed_col(ui, STACK_LAYER_W, |ui| {
                    if via_arc {
                        let tint = dim_color(ui);
                        ui.add(icons::icon_image_tinted(
                            icons::REFERENCE_ARC,
                            STACK_ICON,
                            tint,
                        ))
                        .on_hover_text("contributes across a composition arc");
                    }
                    let text = if is_winner {
                        RichText::new(label).small().strong().color(winner_green())
                    } else {
                        RichText::new(label).small().weak().strikethrough()
                    };
                    ui.add(Label::new(text).truncate())
                        .on_hover_text(layer_id.to_owned());
                });

                // Columns C + value — sdf path (left, shrinks) and the
                // per-opinion value (right-aligned).
                egui::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        if !path.is_empty() {
                            ui.add(
                                Label::new(RichText::new(path).small().weak().monospace())
                                    .truncate(),
                            );
                        }
                    },
                    |ui| {
                        if !value.is_empty() {
                            let shown = display_value(type_name, value);
                            let text = RichText::new(shown.as_ref()).small().monospace();
                            let text = if is_winner {
                                text.color(winner_green())
                            } else {
                                text.weak().strikethrough()
                            };
                            ui.add(Label::new(text).truncate());
                        }
                    },
                );
            });
        });
    ui.add_space(2.0);
}

/// Footer beneath the opinion rows: the resolved `value_source` plus, when the
/// value is authored, the edit-target hint with a [`icons::PENCIL`] glyph. In
/// authoring mode, when the attribute carries an opinion in the open stage's
/// **root layer** (`root_authored`), a right-aligned "Remove opinion" button
/// deletes that root-layer opinion (reverting the composed value to the
/// next-strongest layer / schema fallback), recorded as one journal entry.
fn opinion_footer(ui: &mut Ui, ctx: &mut InspectorContext<'_>, prop: &PropertyRecord) {
    ui.add_space(3.0);
    // Only offer removal for an opinion actually authored in the root layer of
    // an editable prim — matches the FFI `RemoveProperty` target.
    let can_remove = prop.root_authored && ctx.is_prim_editable();
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                RichText::new(format!("value source: {}", prop.value_source))
                    .small()
                    .weak()
                    .monospace(),
            );
            if prop.authored && !prop.provenance.is_empty() {
                ui.label(RichText::new("·").small().weak());
                let tint = dim_color(ui);
                ui.add(icons::icon_image_tinted(icons::PENCIL, STACK_ICON, tint));
                ui.label(RichText::new("edit target").small().weak());
            }
        },
        |ui| {
            if can_remove {
                let resp = ui
                    .button(RichText::new("Remove opinion").small())
                    .on_hover_text(
                        "Delete this attribute's opinion authored in the open stage's \
                         root layer, reverting to the next-strongest layer (undoable)",
                    );
                if resp.clicked() {
                    ctx.remove_property_opinion(&prop.name);
                }
            }
        },
    );
}

/// Allocate a fixed-width column and lay its content out left-to-right,
/// vertically centred.  Fixed widths are what keep the opinion-stack columns
/// aligned across every row.
fn fixed_col(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, STACK_ROW_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.spacing_mut().item_spacing.x = 3.0;
            add(ui);
        },
    );
}

/// A dimmed variant of the current theme's text colour, used for muted icons
/// and the fallback chip.
fn dim_color(ui: &Ui) -> Color32 {
    ui.visuals().text_color().gamma_multiply(0.55)
}

/// Background fill for the full-width opinion-stack band — the surrounding
/// panel colour lightened a few percent toward white so the band reads as a
/// distinct section beneath the attribute line in both light and dark themes.
fn stack_band_fill(ui: &Ui) -> Color32 {
    ui.visuals().panel_fill.lerp_to_gamma(Color32::WHITE, 0.06)
}

/// Split a `provenance` entry (`"<layerIdentifier>:<sdfPath>"`, as produced by
/// the bridge) into `(layer_identifier, sdf_path)`.
///
/// The `sdfPath` always begins with `/`, so we split on the last `":/"` — this
/// is robust to layer identifiers that themselves contain colons (Windows
/// drive letters `C:\…`, URIs `omniverse://…`, anonymous layer ids
/// `anon:0x…:name`).  Falls back to the whole string as the layer id when no
/// path separator is present.
pub(crate) fn split_provenance(entry: &str) -> (&str, &str) {
    if let Some(idx) = entry.rfind(":/") {
        (&entry[..idx], &entry[idx + 1..])
    } else if let Some(idx) = entry.rfind(':') {
        (&entry[..idx], &entry[idx + 1..])
    } else {
        (entry, "")
    }
}

/// Human-readable short label for a layer identifier: the file basename, or a
/// friendly placeholder for anonymous/session layers that have no path.
pub(crate) fn layer_basename(layer_id: &str) -> &str {
    if layer_id.is_empty() {
        return "‹unknown›";
    }
    if layer_id.starts_with("anon:") {
        return "‹anonymous›";
    }
    layer_id.rsplit(['/', '\\']).next().unwrap_or(layer_id)
}

/// Extract the first variant selector segment (`{set=sel}`) from a USD spec
/// path, e.g. `/World/Cube{shading=red}.displayColor` → `{shading=red}`.
/// Returns `None` when the opinion is not variant-scoped.
pub(crate) fn variant_selector_of(path: &str) -> Option<String> {
    let open = path.find('{')?;
    let close = path[open..].find('}')? + open;
    Some(path[open..=close].to_owned())
}

/// Expand a terse `value_source` token into a hover-friendly sentence.
fn value_source_detail(value_source: &str) -> String {
    let explanation = match value_source {
        "authored/default" => "Authored default value on a contributing layer.",
        "authored/time-sampled" => "Authored, animated (time-sampled) value.",
        "schema fallback" => "No authored opinion — value comes from the schema fallback.",
        "authored targets" => "Authored relationship targets.",
        "schema relationship" => "Relationship declared by the schema, no authored targets.",
        "composed prim metadata" => "Composed prim metadata.",
        _ => "",
    };
    if explanation.is_empty() {
        format!("value source: {value_source}")
    } else {
        format!("value source: {value_source}\n{explanation}")
    }
}

// ── Type-specific helpers ─────────────────────────────────────────────────────

fn bool_label(ui: &mut Ui, s: &str) -> Response {
    let text = match s {
        "1" | "true" | "True" => "✔",
        "0" | "false" | "False" => "✗",
        _ => s,
    };
    ui.add(Label::new(RichText::new(text).monospace().small()))
}

fn monospace_label(ui: &mut Ui, s: &str) -> Response {
    ui.add(Label::new(RichText::new(s).monospace().small()))
}

fn string_label(ui: &mut Ui, s: &str) -> Response {
    ui.add(Label::new(RichText::new(s).small()))
}

/// Monospace label for matrix values. Rendered at full width (capped at
/// [`MAX_VALUE_CHARS`]); the enclosing scroll area scrolls horizontally if the
/// value is wider than the panel.
fn matrix_label(ui: &mut Ui, s: &str) -> Response {
    ui.add(Label::new(RichText::new(s).monospace().small()))
}

/// Compact label for array-typed values: the element count only, `[N]`.
fn array_count_label(ui: &mut Ui, summary: &str) -> Response {
    let label = match array_count(summary) {
        Some(n) => format!("[{n}]"),
        None => "[…]".to_owned(),
    };
    ui.add(Label::new(RichText::new(label).monospace().small().weak()))
        .on_hover_text("array — element count")
}

/// Coloured badge for `token` type values.
pub fn token_chip(ui: &mut Ui, token: &str) -> Response {
    let color = ui.visuals().selection.bg_fill.linear_multiply(0.4);
    let text = RichText::new(token)
        .monospace()
        .small()
        .background_color(color);
    ui.add(Label::new(text))
}

/// Monospace chip for `asset` path values, with a leading `@` sigil.
pub fn asset_chip(ui: &mut Ui, path: &str) -> Response {
    let display = if path.is_empty() { "(none)" } else { path };
    ui.add(Label::new(
        RichText::new(format!("@{display}"))
            .monospace()
            .small()
            .weak(),
    ))
}

/// Small badge showing the animated sample count, appended after the value cell.
pub fn time_sampled_badge(ui: &mut Ui, count: u64) {
    ui.label(
        RichText::new(format!("{count}×"))
            .small()
            .weak()
            .color(ui.visuals().warn_fg_color),
    );
}

/// Small muted type-name label (second column in attribute grids).
pub fn type_tag(ui: &mut Ui, type_name: &str) {
    ui.label(RichText::new(type_name).small().weak().monospace());
}

/// Render a two-column label + value grid row for a simple named scalar/string.
/// Used by panels that don't need the full 3-column `attribute_row` layout.
pub fn label_value_row(ui: &mut Ui, name: &str, value: &str) {
    ui.label(RichText::new(name).strong());
    ui.label(RichText::new(value).monospace().small());
    ui.end_row();
}

/// Try to parse a `primvars:displayColor` `value_summary` string into an RGB
/// triple.
///
/// USD may prefix array value summaries with `"[N values] "` before the
/// `(r, g, b)` tuple, so the full string can look like:
/// - `"(0.9, 0.3, 0.2)"`
/// - `"[1 values] (0.9, 0.3, 0.2)"`
///
/// We find the innermost `(…)` pair and parse whatever is inside it.
/// Returns `None` when the format is unrecognised.
fn parse_color_summary(s: &str) -> Option<[f32; 3]> {
    let start = s.find('(')? + 1;
    let end = start + s[start..].find(')')?;
    let inner = &s[start..end];
    let mut parts = inner.splitn(3, ',');
    let r = parts.next()?.trim().parse::<f32>().ok()?;
    let g = parts.next()?.trim().parse::<f32>().ok()?;
    let b = parts.next()?.trim().parse::<f32>().ok()?;
    Some([r, g, b])
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_usd::PropertyKind;

    /// Minimal `PropertyRecord` for the `has_opinion_stack` gating tests.
    fn prop(authored: bool, provenance: &[&str]) -> PropertyRecord {
        PropertyRecord {
            name: "diffuseColor".to_owned(),
            kind: PropertyKind::Attribute,
            type_name: "color3f".to_owned(),
            value_summary: "(0.8, 0.7, 0.5)".to_owned(),
            authored,
            value_source: "authored/default".to_owned(),
            time_sample_count: 0,
            provenance: provenance.iter().map(|s| (*s).to_owned()).collect(),
            provenance_values: Vec::new(),
            relationship_targets: Vec::new(),
            root_authored: false,
        }
    }

    #[test]
    fn has_opinion_stack_true_only_when_authored_with_provenance() {
        // Authored with at least one contributing layer → a stack to reveal.
        assert!(has_opinion_stack(&prop(
            true,
            &["/proj/strong.usda:/World/Pawn.diffuseColor"]
        )));
        // Not authored (schema fallback) → nothing to reveal, even if provenance
        // somehow lists a layer.
        assert!(!has_opinion_stack(&prop(false, &[])));
        assert!(!has_opinion_stack(&prop(
            false,
            &["/proj/strong.usda:/World/Pawn.diffuseColor"]
        )));
        // Authored but no recorded opinions → no stack.
        assert!(!has_opinion_stack(&prop(true, &[])));
    }

    #[test]
    fn split_provenance_unix_layer() {
        let (layer, path) =
            split_provenance("/proj/assets/chess_set.usda:/World/Pawn.diffuseColor");
        assert_eq!(layer, "/proj/assets/chess_set.usda");
        assert_eq!(path, "/World/Pawn.diffuseColor");
    }

    #[test]
    fn split_provenance_windows_drive_letter() {
        // The ':' in the drive letter must not be mistaken for the separator.
        let (layer, path) = split_provenance(r"C:\assets\chess_set.usda:/World/Pawn");
        assert_eq!(layer, r"C:\assets\chess_set.usda");
        assert_eq!(path, "/World/Pawn");
    }

    #[test]
    fn split_provenance_uri_layer() {
        let (layer, path) = split_provenance("omniverse://host/proj/scene.usda:/World/Pawn.radius");
        assert_eq!(layer, "omniverse://host/proj/scene.usda");
        assert_eq!(path, "/World/Pawn.radius");
    }

    #[test]
    fn split_provenance_anonymous_layer() {
        let (layer, path) = split_provenance("anon:0x55d:session.usda:/World/Pawn");
        assert_eq!(layer, "anon:0x55d:session.usda");
        assert_eq!(path, "/World/Pawn");
    }

    #[test]
    fn split_provenance_no_path() {
        let (layer, path) = split_provenance("some_layer_identifier");
        assert_eq!(layer, "some_layer_identifier");
        assert_eq!(path, "");
    }

    #[test]
    fn variant_selector_parses_from_a_variant_scoped_spec_path() {
        assert_eq!(
            variant_selector_of("/World/Cube{shading=red}.primvars:displayColor"),
            Some("{shading=red}".to_owned()),
        );
        assert_eq!(variant_selector_of("/World/Cube.primvars:displayColor"), None);
        // Nested/multiple selectors: the first (strongest) segment is returned.
        assert_eq!(
            variant_selector_of("/A{lod=hi}/B{mat=steel}.x"),
            Some("{lod=hi}".to_owned()),
        );
    }

    #[test]
    fn layer_basename_strips_unix_and_windows_dirs() {
        assert_eq!(
            layer_basename("/proj/assets/chess_set.usda"),
            "chess_set.usda"
        );
        assert_eq!(
            layer_basename(r"C:\assets\chess_set.usda"),
            "chess_set.usda"
        );
        assert_eq!(layer_basename("scene.usda"), "scene.usda");
    }

    #[test]
    fn layer_basename_labels_anonymous_and_empty() {
        assert_eq!(layer_basename("anon:0x55d:session.usda"), "‹anonymous›");
        assert_eq!(layer_basename(""), "‹unknown›");
    }

    /// The winning opinion is always the first provenance entry (the bridge
    /// walks `GetPropertyStack()` strongest → weakest).
    #[test]
    fn provenance_winner_is_first_entry() {
        let provenance = [
            "/proj/strong.usda:/World/Pawn.diffuseColor".to_owned(),
            "/proj/weak.usda:/World/Pawn.diffuseColor".to_owned(),
        ];
        let (winner_layer, _) = split_provenance(&provenance[0]);
        assert_eq!(layer_basename(winner_layer), "strong.usda");
    }

    #[test]
    fn elide_value_leaves_short_strings_untouched() {
        let s = "(0.9, 0.3, 0.2)";
        assert!(matches!(elide_value(s), std::borrow::Cow::Borrowed(_)));
        assert_eq!(elide_value(s), s);
    }

    #[test]
    fn elide_value_truncates_long_arrays_with_ellipsis() {
        let long = "(0.1, 0.2, 0.3), ".repeat(50); // ~850 chars
        let elided = elide_value(&long);
        assert!(elided.ends_with('…'));
        // MAX_VALUE_CHARS content chars + the ellipsis.
        assert_eq!(elided.chars().count(), MAX_VALUE_CHARS + 1);
    }

    #[test]
    fn elide_value_never_splits_multibyte_chars() {
        // A string of multi-byte chars longer than the cap must not panic and
        // must remain valid UTF-8 when sliced.
        let s = "→".repeat(MAX_VALUE_CHARS + 20);
        let elided = elide_value(&s);
        assert!(elided.ends_with('…'));
        assert_eq!(elided.chars().count(), MAX_VALUE_CHARS + 1);
    }

    // ── Value-column rules ────────────────────────────────────────────────────

    #[test]
    fn array_count_parses_bridge_prefix() {
        assert_eq!(array_count("[300 values] (…)"), Some(300));
        assert_eq!(array_count("[1 values] (0.9, 0.3, 0.2)"), Some(1));
        assert_eq!(array_count("[0 values] "), Some(0));
    }

    #[test]
    fn array_count_rejects_non_array_summaries() {
        assert_eq!(array_count("(0.9, 0.3, 0.2)"), None);
        assert_eq!(array_count("not an array"), None);
        assert_eq!(array_count(""), None);
    }

    #[test]
    fn is_matrix_type_matches_matrix_and_frame_types() {
        assert!(is_matrix_type("matrix4d"));
        assert!(is_matrix_type("matrix3d"));
        assert!(is_matrix_type("matrix2d"));
        assert!(is_matrix_type("frame4d"));
        assert!(!is_matrix_type("float3"));
        // Array-of-matrix is handled by the array rule, not the matrix rule.
        assert!(!is_matrix_type("matrix4d[]"));
    }

    #[test]
    fn display_value_collapses_arrays_to_count() {
        assert_eq!(
            display_value("float3[]", "[300 values] (…)").as_ref(),
            "[300]"
        );
        assert_eq!(display_value("token[]", "[2 values] a, b").as_ref(), "[2]");
        // Unparseable array summary falls back to a placeholder count.
        assert_eq!(display_value("float3[]", "garbage").as_ref(), "[…]");
    }

    #[test]
    fn display_value_passes_simple_types_through() {
        assert_eq!(
            display_value("color3f", "(0.8, 0.7, 0.5)").as_ref(),
            "(0.8, 0.7, 0.5)"
        );
        assert_eq!(display_value("float", "0.35").as_ref(), "0.35");
        assert_eq!(display_value("double3", "(1, 2, 3)").as_ref(), "(1, 2, 3)");
    }

    #[test]
    fn display_value_elides_long_matrix() {
        let long = "( (1,0,0,0), (0,1,0,0), (0,0,1,0), (0,0,0,1) )".repeat(5);
        let shown = display_value("matrix4d", &long);
        // Elided to the cap; the widget clips further at the column boundary.
        assert!(shown.chars().count() <= MAX_VALUE_CHARS + 1);
    }
}
