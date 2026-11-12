//! Stage **Layer Stack** panel — a visualization of the composed root layer
//! stack (session layer + root layer + recursively-included sublayers),
//! rendered strongest → weakest per USD's LIVRPS ordering.
//!
//! It shows each layer's arc, nesting, offset/scale, dirty/anonymous/muted
//! state, edit-target marker, and a `Δ` badge counting the prim specs the layer
//! contributes. The selected-layer detail card also offers **reload**
//! (`SdfLayer::Reload`), **set edit target**, **mute / un-mute**, and
//! **reorder** (move a sublayer stronger/weaker within its parent).
//!
//! Data arrives as a UI-neutral [`LayerStackInfo`]; the shell caches it and
//! only refetches it across the FFI bridge when the stage topology changes, so
//! rendering this panel never touches the per-frame viewport path.
//!
//! # Column alignment
//!
//! Rows are laid out with a single [`egui::Grid`] so the four columns — marker,
//! name (indented by nesting depth), metadata, and `Δ` badge — stay aligned
//! across every row regardless of content width.

use std::collections::HashSet;

use egui::{Color32, ImageSource, RichText, Ui};
use idtx_editor_model::{LayerArc, LayerInfo, LayerStackInfo};
use num_traits::ToPrimitive;

use crate::icons;

// ── Palette (mirrors the mockup in docs/assets/layer-stack-panel.svg) ─────────

/// Accent blue used for the edit-target marker/label (`.accent` in the mockup).
fn accent() -> Color32 {
    crate::theme::current().accent
}
/// Muted grey for struck-through muted layers (`.mute` in the mockup).
fn mute() -> Color32 {
    crate::theme::current().text_muted
}
/// Dim grey for secondary metadata text (`.dim` in the mockup).
fn dim() -> Color32 {
    crate::theme::current().text_dim
}

/// Per-depth indentation applied inside the name cell so the metadata column
/// stays aligned regardless of nesting.
const INDENT_PX: f32 = 14.0;
/// Row icon size, matching the tree/inspector row-icon metrics.
const ICON_SIZE: f32 = icons::DEFAULT_ICON_SIZE;

/// Maximum fraction of the panel's available height the layer tree may occupy
/// when the detail card is shown, leaving the remaining 20% for the detail.
const TREE_HEIGHT_RATIO: f32 = 0.75;

/// The maximum number of characters shown for the `identifier` and
/// `resolved path` rows in the detail card. Longer values are middle-truncated
/// (keeping the head and the trailing filename) so a deep absolute path never
/// stretches the panel; the full value is available on hover.
const DETAIL_MAX_CHARS: usize = 64;

// ── Panel state & intents ─────────────────────────────────────────────────────

/// Purely-visual state owned by the shell and threaded into the panel each
/// frame: which layer row (by identifier) is currently selected for the detail
/// card, and which subtrees are collapsed. Kept out of egui temporary memory so
/// the shell can reset it on stage reload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerStackUiState {
    /// Identifier of the layer whose detail card is shown, if any.
    pub selected: Option<String>,
    /// Identifiers of layers whose descendant sublayers/arcs are collapsed
    /// (hidden). Layers default to expanded, so absence means "expanded".
    pub collapsed: HashSet<String>,
}

impl LayerStackUiState {
    /// Whether the subtree introduced by `identifier` is currently collapsed.
    #[must_use]
    pub fn is_collapsed(&self, identifier: &str) -> bool {
        self.collapsed.contains(identifier)
    }

    /// Flips the collapsed/expanded state of the subtree under `identifier`.
    pub fn toggle_collapsed(&mut self, identifier: &str) {
        if !self.collapsed.remove(identifier) {
            self.collapsed.insert(identifier.to_owned());
        }
    }
}

/// An action the panel wants the shell to perform against the live session.
///
/// Beyond the read-only reload, the detail card offers the interactive
/// layer-stack edits: switching the edit target, muting /
/// un-muting a layer, and reordering a sublayer within its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerStackAction {
    /// Reload the layer with this identifier (`SdfLayer::Reload`).
    Reload(String),
    /// Make the layer with this identifier the stage's edit target.
    SetEditTarget(String),
    /// Mute the layer with this identifier (drop it from composition).
    Mute(String),
    /// Un-mute the layer with this identifier (restore its contribution).
    Unmute(String),
    /// Reorder a sublayer within its parent's sublayer list. `stronger = true`
    /// moves it earlier (a stronger opinion), `false` later (weaker).
    MoveSublayer {
        /// Identifier of the sublayer being moved.
        identifier: String,
        /// Identifier of the containing layer (empty = the root layer).
        parent_identifier: String,
        /// Move earlier (stronger) when `true`, later (weaker) when `false`.
        stronger: bool,
    },
}

// ── Panel entry point ─────────────────────────────────────────────────────────

/// Renders the Layer Stack panel and returns an optional action for the shell.
///
/// `state` carries the row selection (mutated in place on click). `editable`
/// gates the interactive layer edits (set edit target, mute/un-mute, reorder)
/// in the detail card — the read-only reload is always offered. The returned
/// [`LayerStackAction`] is `Some` only on the frame the user clicks a button.
pub fn layer_stack_panel(
    ui: &mut Ui,
    info: &LayerStackInfo,
    state: &mut LayerStackUiState,
    editable: bool,
) -> Option<LayerStackAction> {
    let mut action = None;

    // Header: title + LIVRPS strength hint.
    ui.horizontal(|ui| {
        ui.add(icons::icon_image(ui, icons::LAYERS, ICON_SIZE));
        ui.label(RichText::new("Layer Stack").strong());
    });
    ui.label(
        RichText::new("strongest → weakest (LIVRPS)")
            .small()
            .color(dim()),
    );
    ui.label(
        RichText::new("● = edit target   ·   Δ = # prim specs contributed")
            .small()
            .color(dim()),
    );
    ui.add_space(4.0);

    if info.layers.is_empty() {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No layers").small().weak());
        });
        return action;
    }

    // Clear a stale selection if the layer is gone (e.g. after reload).
    if let Some(sel) = &state.selected
        && !info.layers.iter().any(|l| &l.identifier == sel)
    {
        state.selected = None;
    }

    // Layers that introduce at least one child arc get an expander; the rest
    // are aligned with matching blank space.
    let parents = parent_identifiers(&info.layers);
    let visible = visible_indices(&info.layers, &state.collapsed);

    // When a detail card is shown below, cap the tree at 80% of the remaining
    // height so the detail always keeps its 20%; a taller tree scrolls. With no
    // detail card the tree may use all the space (still scrolling if it
    // overflows).
    let detail_shown = state
        .selected
        .as_ref()
        .is_some_and(|sel| info.layers.iter().any(|l| &l.identifier == sel));
    let avail = ui.available_height();

    // `both()` so long layer names / identifiers that extend past the panel
    // width can be reached by scrolling horizontally instead of being clipped.
    let mut scroll = egui::ScrollArea::both().auto_shrink([false, true]);
    if detail_shown {
        scroll = scroll.max_height(avail * TREE_HEIGHT_RATIO);
    }
    scroll.show(ui, |ui| {
        egui::Grid::new("layer_stack_grid")
            .num_columns(4)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for &i in &visible {
                    let layer = &info.layers[i];
                    let has_children = parents.contains(layer.identifier.as_str());
                    let is_collapsed = state.is_collapsed(&layer.identifier);
                    let action = layer_row(ui, layer, has_children, is_collapsed, state);
                    if action.select {
                        state.selected = Some(layer.identifier.clone());
                    }
                    if action.toggle {
                        state.toggle_collapsed(&layer.identifier);
                    }
                    ui.end_row();
                }
            });
    });

    // Selected-layer detail card.
    if let Some(sel) = state.selected.clone()
        && let Some(layer) = info.layers.iter().find(|l| l.identifier == sel)
    {
        ui.add_space(8.0);
        action = detail_card(ui, layer, editable);
    }

    action
}

/// What the user did on a layer row this frame.
#[derive(Default)]
struct RowInteraction {
    /// The row's name was clicked (request to select it).
    select: bool,
    /// The row's expander was clicked (request to toggle its subtree).
    toggle: bool,
}

/// Renders a single layer row across the four grid columns.
///
/// `has_children` controls whether an expander chevron is drawn (childless rows
/// get matching blank space so names stay aligned); `is_collapsed` picks the
/// chevron direction. Returns which affordances were clicked this frame.
fn layer_row(
    ui: &mut Ui,
    layer: &LayerInfo,
    has_children: bool,
    is_collapsed: bool,
    state: &LayerStackUiState,
) -> RowInteraction {
    let selected = state.selected.as_deref() == Some(layer.identifier.as_str());
    let mut interaction = RowInteraction::default();

    // Column 1 — name: indent, expander, arc icon, then the (possibly styled) label.
    ui.horizontal(|ui| {
        ui.add_space(f32::from(layer.depth.to_u16().unwrap_or(u16::MAX)) * INDENT_PX);

        // Expander chevron (or aligning space for leaf layers).
        if has_children {
            let (rect, resp) =
                ui.allocate_exact_size(egui::Vec2::splat(ICON_SIZE), egui::Sense::click());
            if ui.is_rect_visible(rect) {
                let col = if resp.hovered() {
                    ui.style().visuals.text_color()
                } else {
                    ui.style().visuals.weak_text_color()
                };
                egui::Image::new(icons::chevron(!is_collapsed))
                    .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE))
                    .tint(col)
                    .paint_at(ui, rect);
            }
            if resp.clicked() {
                interaction.toggle = true;
            }
        } else {
            ui.add_space(ICON_SIZE);
        }

        let tint = if layer.is_muted {
            mute()
        } else if layer.is_edit_target {
            accent()
        } else {
            ui.style().visuals.text_color()
        };
        ui.add(icons::icon_image_tinted(
            arc_icon(layer.arc),
            ICON_SIZE,
            tint,
        ));

        let mut text = RichText::new(display_label(layer));
        if layer.is_muted {
            text = text.strikethrough().color(mute());
        } else if layer.is_edit_target {
            text = text.color(accent()).strong();
        } else if !matches!(layer.arc, LayerArc::SubLayer) {
            text = text.strong();
        }
        let resp = ui.selectable_label(selected, text);
        let resp = resp.on_hover_text(&layer.identifier);
        if resp.clicked() {
            interaction.select = true;
        }
    });

    // Column 2 — metadata.
    let meta_color = if layer.is_muted { mute() } else { dim() };
    ui.label(
        RichText::new(metadata_line(layer))
            .small()
            .color(meta_color),
    );

    // Column 3 — status marker (edit target / muted).
    ui.horizontal(|ui| {
        if layer.is_edit_target {
            ui.add(icons::icon_image_tinted(
                icons::EDIT_TARGET,
                ICON_SIZE,
                accent(),
            ));
            ui.label(RichText::new("EDIT TARGET").small().color(accent()));
        } else if layer.is_muted {
            ui.add(icons::icon_image_tinted(icons::MUTE, ICON_SIZE, mute()));
        }
    });

    // Column 4 — Δ badge.
    let badge_color = if layer.is_muted { mute() } else { dim() };
    ui.label(
        RichText::new(delta_badge_text(layer.delta_count))
            .small()
            .monospace()
            .color(badge_color),
    );

    interaction
}

/// Lower detail card for the selected layer. Returns `Some(action)` when a
/// button is pressed. `editable` gates the interactive edits; reload is always
/// available.
fn detail_card(ui: &mut Ui, layer: &LayerInfo, editable: bool) -> Option<LayerStackAction> {
    let mut action = None;
    ui.separator();
    ui.label(RichText::new(format!("Selected layer — {}", display_label(layer))).strong());
    ui.add_space(2.0);

    egui::Grid::new("layer_detail_grid")
        .num_columns(2)
        .spacing([12.0, 2.0])
        .show(ui, |ui| {
            detail_row_truncated(ui, "identifier", &layer.identifier);
            detail_row_truncated(
                ui,
                "resolved path",
                layer.real_path.as_deref().unwrap_or("‹none›"),
            );
            detail_row(ui, "anonymous", yes_no(layer.is_anonymous));
            detail_row(ui, "dirty", yes_no(layer.is_dirty));
            detail_row(ui, "muted", yes_no(layer.is_muted));
            if matches!(layer.arc, LayerArc::SubLayer) {
                detail_row(
                    ui,
                    "offset / scale",
                    &format!("{} / {}", fmt_num(layer.offset), fmt_num(layer.scale)),
                );
            }
            if matches!(layer.arc, LayerArc::Reference | LayerArc::Payload) {
                if let Some(map) = arc_mapping(layer) {
                    detail_row(ui, "introduced by", &map);
                }
                if matches!(layer.arc, LayerArc::Payload) {
                    detail_row(ui, "loaded", yes_no(layer.is_loaded));
                }
                detail_row(ui, "has specs", yes_no(layer.has_specs));
            }
        });

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("actions").small().color(dim()));

        // Reload — anonymous layers have nothing on disk to reload.
        let can_reload = !layer.is_anonymous;
        let btn =
            egui::Button::image_and_text(icons::icon_image(ui, icons::RELOAD, ICON_SIZE), "reload");
        let resp = ui.add_enabled(can_reload, btn);
        let resp = if can_reload {
            resp.on_hover_text("Reload this layer from disk (SdfLayer::Reload)")
        } else {
            resp.on_hover_text("Anonymous layers have no backing file to reload")
        };
        if resp.clicked() {
            action = Some(LayerStackAction::Reload(layer.identifier.clone()));
        }

        // The interactive layer edits are authoring-only; a read-only product
        // (Inspector) shows the reload affordance but none of these.
        if !editable {
            return;
        }

        // Set edit target — only a local layer (session/root/sublayer) that is
        // neither already the target nor muted can receive authoring writes.
        let can_target = matches!(
            layer.arc,
            LayerArc::Session | LayerArc::Root | LayerArc::SubLayer
        ) && !layer.is_edit_target
            && !layer.is_muted;
        let btn = egui::Button::image_and_text(
            icons::icon_image(ui, icons::EDIT_TARGET, ICON_SIZE),
            "set edit target",
        );
        let resp = ui.add_enabled(can_target, btn);
        let resp = if layer.is_edit_target {
            resp.on_hover_text("This layer is already the edit target")
        } else if layer.is_muted {
            resp.on_hover_text("A muted layer cannot be the edit target")
        } else if !can_target {
            resp.on_hover_text("Only local layers (session/root/sublayer) can be the edit target")
        } else {
            resp.on_hover_text("Author subsequent edits into this layer (UsdEditTarget)")
        };
        if resp.clicked() {
            action = Some(LayerStackAction::SetEditTarget(layer.identifier.clone()));
        }

        // Mute / un-mute — the session and root layers are never mutable.
        let can_mute = !matches!(layer.arc, LayerArc::Session | LayerArc::Root);
        if can_mute {
            if layer.is_muted {
                let btn = egui::Button::image_and_text(
                    icons::icon_image(ui, icons::UNMUTE, ICON_SIZE),
                    "un-mute",
                );
                if ui
                    .add(btn)
                    .on_hover_text("Restore this layer's contribution to composition")
                    .clicked()
                {
                    action = Some(LayerStackAction::Unmute(layer.identifier.clone()));
                }
            } else {
                let btn = egui::Button::image_and_text(
                    icons::icon_image(ui, icons::MUTE, ICON_SIZE),
                    "mute",
                );
                if ui
                    .add(btn)
                    .on_hover_text("Drop this layer's contribution from composition")
                    .clicked()
                {
                    action = Some(LayerStackAction::Mute(layer.identifier.clone()));
                }
            }
        }

        // Reorder — only sublayers have a meaningful position within a parent.
        if matches!(layer.arc, LayerArc::SubLayer) {
            let up = egui::Button::image_and_text(
                icons::icon_image(ui, icons::MOVE_UP, ICON_SIZE),
                "stronger",
            );
            if ui
                .add(up)
                .on_hover_text("Move earlier in the sublayer list (stronger opinion)")
                .clicked()
            {
                action = Some(LayerStackAction::MoveSublayer {
                    identifier: layer.identifier.clone(),
                    parent_identifier: layer.parent_identifier.clone(),
                    stronger: true,
                });
            }
            let down = egui::Button::image_and_text(
                icons::icon_image(ui, icons::MOVE_DOWN, ICON_SIZE),
                "weaker",
            );
            if ui
                .add(down)
                .on_hover_text("Move later in the sublayer list (weaker opinion)")
                .clicked()
            {
                action = Some(LayerStackAction::MoveSublayer {
                    identifier: layer.identifier.clone(),
                    parent_identifier: layer.parent_identifier.clone(),
                    stronger: false,
                });
            }
        }
    });

    ui.add_space(2.0);
    ui.label(
        RichText::new("Reload discards in-memory edits to this layer and re-reads it from disk.")
            .small()
            .color(dim()),
    );

    action
}

fn detail_row(ui: &mut Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).small().color(dim()));
    ui.label(RichText::new(value).small());
    ui.end_row();
}

/// Like [`detail_row`], but middle-truncates long values (paths, identifiers)
/// to [`DETAIL_MAX_CHARS`] so they never widen the panel; the untruncated value
/// is shown on hover.
fn detail_row_truncated(ui: &mut Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).small().color(dim()));
    let shown = truncate_middle(value, DETAIL_MAX_CHARS);
    let resp = ui.label(RichText::new(&shown).small());
    if shown != value {
        resp.on_hover_text(value);
    }
    ui.end_row();
}

// ── Pure helpers (unit-tested) ─────────────────────────────────────────────────

/// The set of identifiers that appear as some layer's `parent_identifier` —
/// i.e. the layers that introduce at least one child arc and should therefore
/// render an expander. Session/root layers carry an empty parent and are
/// ignored here.
fn parent_identifiers(layers: &[LayerInfo]) -> HashSet<&str> {
    layers
        .iter()
        .filter(|l| !l.parent_identifier.is_empty())
        .map(|l| l.parent_identifier.as_str())
        .collect()
}

/// Given the DFS pre-order `layers` and the set of collapsed identifiers,
/// returns the indices of the rows that should be drawn. A collapsed layer's
/// descendants (every following row with a strictly greater `depth`, until a
/// row returns to the collapsed layer's depth or shallower) are omitted.
fn visible_indices(layers: &[LayerInfo], collapsed: &HashSet<String>) -> Vec<usize> {
    let mut out = Vec::with_capacity(layers.len());
    // Depth of the shallowest collapsed ancestor currently hiding rows, if any.
    let mut hide_below: Option<u32> = None;
    for (i, layer) in layers.iter().enumerate() {
        if let Some(depth) = hide_below {
            if layer.depth > depth {
                continue; // still inside the collapsed subtree
            }
            hide_below = None; // back out to a sibling/ancestor — visible again
        }
        out.push(i);
        if collapsed.contains(&layer.identifier) {
            hide_below = Some(layer.depth);
        }
    }
    out
}

/// Middle-truncates `s` to at most `max` characters, inserting an ellipsis so
/// both the head and the (usually more informative) tail survive, e.g.
/// `/very/long/path/to/materials.usda` → `/very/long/…/materials.usda`.
/// Counts by `char`, so it never splits a multi-byte code point.
fn truncate_middle(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_owned();
    }
    if max <= 1 {
        return "…".to_owned();
    }
    let keep = max - 1; // one char spent on the ellipsis
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let chars: Vec<char> = s.chars().collect();
    let head_s: String = chars[..head].iter().collect();
    let tail_s: String = chars[count - tail..].iter().collect();
    format!("{head_s}…{tail_s}")
}

/// The row icon for a layer's composition arc.
fn arc_icon(arc: LayerArc) -> ImageSource<'static> {
    match arc {
        LayerArc::Session => icons::LAYER_SESSION,
        LayerArc::Root => icons::LAYER_ROOT,
        LayerArc::SubLayer => icons::LAYER_SUBLAYER,
        LayerArc::Reference => icons::LAYER_REFERENCE,
        LayerArc::Payload => icons::LAYER_PAYLOAD,
    }
}

/// Human-readable primary label: the layer's display name, or a friendly
/// placeholder for anonymous layers with no meaningful name.
fn display_label(layer: &LayerInfo) -> String {
    if !layer.display_name.is_empty() {
        return layer.display_name.clone();
    }
    if layer.is_anonymous || matches!(layer.arc, LayerArc::Session) {
        return "‹anonymous›".to_owned();
    }
    layer
        .identifier
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&layer.identifier)
        .to_owned()
}

/// The dim secondary metadata string shown in column 2.
fn metadata_line(layer: &LayerInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    match layer.arc {
        LayerArc::Session => parts.push("anonymous session layer".to_owned()),
        LayerArc::Root => parts.push("root".to_owned()),
        LayerArc::SubLayer => {
            parts.push("subLayer".to_owned());
            parts.push(format!(
                "offset {} scale {}",
                fmt_num(layer.offset),
                fmt_num(layer.scale)
            ));
        }
        LayerArc::Reference => {
            parts.push("reference".to_owned());
            if let Some(map) = arc_mapping(layer) {
                parts.push(map);
            }
        }
        LayerArc::Payload => {
            parts.push("payload".to_owned());
            if let Some(map) = arc_mapping(layer) {
                parts.push(map);
            }
            if !layer.is_loaded {
                parts.push("not loaded".to_owned());
            }
        }
    }
    if matches!(layer.arc, LayerArc::Reference | LayerArc::Payload) && !layer.has_specs {
        parts.push("no specs".to_owned());
    }
    if layer.is_muted {
        parts.push("MUTED".to_owned());
    }
    if layer.is_dirty {
        parts.push("dirty*".to_owned());
    }
    parts.join("  ·  ")
}

/// Formats the `</source> → </target>` namespace mapping for a reference/payload
/// row, or `None` when either endpoint is missing.
fn arc_mapping(layer: &LayerInfo) -> Option<String> {
    match (&layer.arc_source_path, &layer.arc_target_path) {
        (Some(src), Some(tgt)) => Some(format!("{src} → {tgt}")),
        _ => None,
    }
}

/// The `Δ N` prim-spec-count badge text.
fn delta_badge_text(count: u32) -> String {
    format!("Δ {count}")
}

/// Formats an offset/scale value without trailing zeros (`0.0` → `0`,
/// `1.0` → `1`, `2.5` → `2.5`).
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        // Whole number: `f64` Display omits the fractional part (`24.0` → "24").
        format!("{v}")
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(arc: LayerArc) -> LayerInfo {
        LayerInfo {
            identifier: "/proj/chess/materials.usda".to_owned(),
            display_name: "materials.usda".to_owned(),
            real_path: Some("/proj/chess/materials.usda".to_owned()),
            arc,
            arc_source_path: None,
            arc_target_path: None,
            has_specs: true,
            is_loaded: true,
            depth: 1,
            offset: 0.0,
            scale: 1.0,
            delta_count: 18,
            is_anonymous: false,
            is_dirty: false,
            is_muted: false,
            is_edit_target: false,
            parent_identifier: String::new(),
        }
    }

    #[test]
    fn metadata_session_is_anonymous() {
        let mut l = layer(LayerArc::Session);
        l.is_anonymous = true;
        assert_eq!(metadata_line(&l), "anonymous session layer");
    }

    #[test]
    fn metadata_root_dirty() {
        let mut l = layer(LayerArc::Root);
        l.is_dirty = true;
        assert_eq!(metadata_line(&l), "root  ·  dirty*");
    }

    #[test]
    fn metadata_sublayer_offset_scale() {
        let l = layer(LayerArc::SubLayer);
        assert_eq!(metadata_line(&l), "subLayer  ·  offset 0 scale 1");
    }

    #[test]
    fn metadata_sublayer_muted() {
        let mut l = layer(LayerArc::SubLayer);
        l.is_muted = true;
        assert_eq!(metadata_line(&l), "subLayer  ·  offset 0 scale 1  ·  MUTED");
    }

    #[test]
    fn metadata_sublayer_non_identity_offset() {
        let mut l = layer(LayerArc::SubLayer);
        l.offset = 24.0;
        l.scale = 2.5;
        assert_eq!(metadata_line(&l), "subLayer  ·  offset 24 scale 2.5");
    }

    #[test]
    fn delta_badge_formats() {
        assert_eq!(delta_badge_text(0), "Δ 0");
        assert_eq!(delta_badge_text(41), "Δ 41");
    }

    #[test]
    fn fmt_num_trims_zeros() {
        assert_eq!(fmt_num(0.0), "0");
        assert_eq!(fmt_num(1.0), "1");
        assert_eq!(fmt_num(2.5), "2.5");
        assert_eq!(fmt_num(-3.0), "-3");
        assert_eq!(fmt_num(0.125), "0.125");
    }

    #[test]
    fn display_label_falls_back_for_anonymous() {
        let mut l = layer(LayerArc::Session);
        l.display_name.clear();
        l.is_anonymous = true;
        assert_eq!(display_label(&l), "‹anonymous›");
    }

    #[test]
    fn display_label_uses_basename_when_name_empty() {
        let mut l = layer(LayerArc::SubLayer);
        l.display_name.clear();
        assert_eq!(display_label(&l), "materials.usda");
    }

    #[test]
    fn selection_helpers_map_arcs() {
        // arc_icon is total; just ensure distinct sources per arc compile/select.
        let _ = arc_icon(LayerArc::Session);
        let _ = arc_icon(LayerArc::Root);
        let _ = arc_icon(LayerArc::SubLayer);
        let _ = arc_icon(LayerArc::Reference);
        let _ = arc_icon(LayerArc::Payload);
    }

    #[test]
    fn metadata_reference_shows_mapping() {
        let mut l = layer(LayerArc::Reference);
        l.display_name = "Pawn.usd".to_owned();
        l.arc_source_path = Some("/World/Pawn".to_owned());
        l.arc_target_path = Some("/Pawn".to_owned());
        assert_eq!(metadata_line(&l), "reference  ·  /World/Pawn → /Pawn");
    }

    #[test]
    fn metadata_reference_without_mapping() {
        let l = layer(LayerArc::Reference);
        assert_eq!(metadata_line(&l), "reference");
    }

    #[test]
    fn metadata_payload_loaded() {
        let mut l = layer(LayerArc::Payload);
        l.arc_source_path = Some("/Scene/Asset".to_owned());
        l.arc_target_path = Some("/Asset".to_owned());
        assert_eq!(metadata_line(&l), "payload  ·  /Scene/Asset → /Asset");
    }

    #[test]
    fn metadata_payload_not_loaded() {
        let mut l = layer(LayerArc::Payload);
        l.arc_source_path = Some("/Scene/Asset".to_owned());
        l.arc_target_path = Some("/Asset".to_owned());
        l.is_loaded = false;
        assert_eq!(
            metadata_line(&l),
            "payload  ·  /Scene/Asset → /Asset  ·  not loaded"
        );
    }

    #[test]
    fn metadata_reference_no_specs() {
        let mut l = layer(LayerArc::Reference);
        l.arc_source_path = Some("/World/Pawn".to_owned());
        l.arc_target_path = Some("/Pawn".to_owned());
        l.has_specs = false;
        assert_eq!(
            metadata_line(&l),
            "reference  ·  /World/Pawn → /Pawn  ·  no specs"
        );
    }

    #[test]
    fn display_label_reference_uses_basename_when_name_empty() {
        let mut l = layer(LayerArc::Reference);
        l.display_name.clear();
        l.identifier = "/assets/Pawn/Pawn.usd".to_owned();
        assert_eq!(display_label(&l), "Pawn.usd");
    }

    // ── Expand / collapse + truncation ──────────────────────────────────────

    /// Builds a layer with a given identifier, parent, and depth for
    /// hierarchy-shape tests. Other fields are irrelevant here.
    fn node(id: &str, parent: &str, depth: u32) -> LayerInfo {
        let mut l = layer(LayerArc::SubLayer);
        l.identifier = id.to_owned();
        l.parent_identifier = parent.to_owned();
        l.depth = depth;
        l
    }

    /// A small hierarchy: root → {sub, outer → inner, ref}. Mirrors the panel's
    /// DFS pre-order.
    fn sample() -> Vec<LayerInfo> {
        vec![
            node("root", "", 0),
            node("sub", "root", 1),
            node("outer", "root", 1),
            node("inner", "outer", 2),
            node("ref", "root", 1),
        ]
    }

    #[test]
    fn parents_are_layers_that_introduce_children() {
        let layers = sample();
        let parents = parent_identifiers(&layers);
        assert!(parents.contains("root"));
        assert!(parents.contains("outer"));
        // Leaves never appear as a parent.
        assert!(!parents.contains("sub"));
        assert!(!parents.contains("inner"));
        assert!(!parents.contains("ref"));
    }

    #[test]
    fn visible_indices_all_when_nothing_collapsed() {
        let layers = sample();
        let collapsed = HashSet::new();
        assert_eq!(visible_indices(&layers, &collapsed), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn visible_indices_hides_collapsed_subtree() {
        let layers = sample();
        // Collapsing "outer" hides only "inner" (index 3); siblings stay.
        let collapsed = HashSet::from(["outer".to_owned()]);
        assert_eq!(visible_indices(&layers, &collapsed), vec![0, 1, 2, 4]);
    }

    #[test]
    fn visible_indices_collapsing_root_hides_everything_below() {
        let layers = sample();
        let collapsed = HashSet::from(["root".to_owned()]);
        assert_eq!(visible_indices(&layers, &collapsed), vec![0]);
    }

    #[test]
    fn visible_indices_nested_collapse_is_idempotent() {
        let layers = sample();
        // Collapsing both an ancestor and a descendant still just hides the
        // ancestor's whole subtree once.
        let collapsed = HashSet::from(["root".to_owned(), "outer".to_owned()]);
        assert_eq!(visible_indices(&layers, &collapsed), vec![0]);
    }

    #[test]
    fn toggle_collapsed_flips_membership() {
        let mut state = LayerStackUiState::default();
        assert!(!state.is_collapsed("root"));
        state.toggle_collapsed("root");
        assert!(state.is_collapsed("root"));
        state.toggle_collapsed("root");
        assert!(!state.is_collapsed("root"));
    }

    #[test]
    fn truncate_middle_keeps_short_strings() {
        assert_eq!(truncate_middle("short.usda", 64), "short.usda");
    }

    #[test]
    fn truncate_middle_shortens_long_paths_with_ellipsis() {
        let s = "/Users/dev/Documents/very/deep/project/stages/materials.usda";
        let out = truncate_middle(s, 32);
        assert_eq!(out.chars().count(), 32);
        assert!(out.contains('…'));
        assert!(out.starts_with("/Users/dev"));
        assert!(out.ends_with("materials.usda"));
    }

    #[test]
    fn truncate_middle_respects_char_boundaries() {
        // Multi-byte characters must never be split mid-code-point.
        let s = "αβγδεζηθικλμνξοπρστυφχψω_αβγδεζηθικλμνξοπρστυφχψω";
        let out = truncate_middle(s, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.contains('…'));
    }
}
