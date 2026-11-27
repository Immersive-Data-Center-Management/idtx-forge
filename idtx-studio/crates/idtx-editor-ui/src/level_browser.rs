//! Project level-registry list.
//!
//! The Conductor's "Project Settings ▸ Levels" section lists the ordered level
//! stages the active project registers (`idtx_project::IdtxProject::levels`),
//! each tagged with its stage kind (`customLayerData.idtx.stageKind`, read by
//! the shell). This module is a **pure egui section**: it takes a UI-neutral
//! snapshot ([`LevelRow`]) the shell builds each frame and returns the
//! [`LevelBrowserAction`]s the user triggered, which the shell maps onto its
//! own command bus. It never touches `idtx-project` or USD directly, keeping
//! `idtx-editor-ui` at the bottom of the dependency stack.

use egui::{Color32, Layout, RichText, Ui};

use crate::icons;

/// Which stage-kind glyph a level row draws, derived from the raw
/// `customLayerData.idtx.stageKind` token.
///
/// A registered level whose stage is untagged or carries an unrecognised token
/// still lives in the level registry, so it falls back to the level glyph
/// rather than an error marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageGlyph {
    /// A level stage (`"level"`, or any untagged/unknown registry entry).
    Level,
    /// A reusable asset stage (`"asset"`).
    Asset,
    /// An assembly stage (`"assembly"`).
    Assembly,
    /// A player/HUD stage (`"player"`).
    Player,
}

impl StageGlyph {
    /// Classify a stage-kind token (case-insensitively). Unknown/empty tokens
    /// map to [`StageGlyph::Level`] — a registry entry is a level by context.
    #[must_use]
    pub fn for_token(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "asset" => Self::Asset,
            "assembly" => Self::Assembly,
            "player" => Self::Player,
            _ => Self::Level,
        }
    }

    /// The embedded SVG icon for this glyph.
    #[must_use]
    pub fn icon(self) -> egui::ImageSource<'static> {
        match self {
            Self::Level => icons::STAGE_LEVEL,
            Self::Asset => icons::STAGE_ASSET,
            Self::Assembly => icons::STAGE_ASSEMBLY,
            Self::Player => icons::STAGE_PLAYER,
        }
    }
}

/// A single level-browser row — a per-frame snapshot the shell assembles from
/// the project registry and each stage's stage-kind tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelRow {
    /// Display name: the stage's `displayName` tag, else its file stem.
    pub display_name: String,
    /// Project-relative path, shown as a dimmed subtitle.
    pub rel_path: String,
    /// Raw stage-kind token (`"level"`/`"asset"`/…/`""`), driving the glyph.
    pub kind_token: String,
    /// `true` when the registered stage file is missing on disk.
    pub missing: bool,
    /// `true` when this level's stage is the active tab's open stage.
    pub is_open: bool,
}

/// Persistent selection state for the level browser (survives across frames).
#[derive(Debug, Clone, Default)]
pub struct LevelBrowserState {
    /// Registry index of the currently selected row, if any.
    pub selected: Option<usize>,
}

/// An action collected during a level-list pass, applied by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelBrowserAction {
    /// Open the level at this registry index in a tab.
    Open(usize),
    /// Remove the level at this index from the registry (the file is kept).
    Remove(usize),
    /// Move the level at this index one slot earlier (stronger/up).
    MoveUp(usize),
    /// Move the level at this index one slot later (weaker/down).
    MoveDown(usize),
    /// Create a brand-new level stage (save dialog), tag it, register it, open it.
    NewLevel,
    /// Register an existing in-project stage as a level (open dialog).
    AddExisting,
}

/// Whether the row at `idx` in a list of `len` entries can move up / down.
#[must_use]
pub fn move_targets(idx: usize, len: usize) -> (bool, bool) {
    (idx > 0, idx + 1 < len)
}

/// Colour for a row whose registered stage file is missing on disk.
fn missing_color() -> Color32 {
    crate::theme::current().status_err
}

/// Draw the project level-registry list (Project Settings ▸ Levels) and return
/// the collected actions.
///
/// `rows` are in registry order; `state.selected` indexes into them. This is a
/// dialog *section*, so it draws no project heading — the Project Settings modal
/// already provides project context.
pub fn level_list_section(
    ui: &mut Ui,
    rows: &[LevelRow],
    authoring: bool,
    state: &mut LevelBrowserState,
) -> Vec<LevelBrowserAction> {
    let mut actions = Vec::new();
    let tint = ui.style().visuals.text_color();

    if rows.is_empty() {
        ui.add_space(6.0);
        ui.label(
            RichText::new("No levels yet — create or add one below.")
                .small()
                .weak(),
        );
    }

    // Drop any stale selection before iterating so downstream indices are valid.
    if state.selected.is_some_and(|s| s >= rows.len()) {
        state.selected = None;
    }

    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (idx, row) in rows.iter().enumerate() {
                let (can_up, can_down) = move_targets(idx, rows.len());
                ui.horizontal(|ui| {
                    // Row controls, right-aligned (drawn first in RTL layout).
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::image(icons::icon_image_tinted(
                                    icons::CLOSE,
                                    icons::DEFAULT_ICON_SIZE,
                                    tint,
                                ))
                                .frame(false),
                            )
                            .on_hover_text("Remove from project")
                            .clicked()
                        {
                            actions.push(LevelBrowserAction::Remove(idx));
                        }
                        if ui
                            .add_enabled(
                                can_down,
                                egui::Button::image(icons::icon_image_tinted(
                                    icons::MOVE_DOWN,
                                    icons::DEFAULT_ICON_SIZE,
                                    tint,
                                ))
                                .frame(false),
                            )
                            .on_hover_text("Move down")
                            .clicked()
                        {
                            actions.push(LevelBrowserAction::MoveDown(idx));
                        }
                        if ui
                            .add_enabled(
                                can_up,
                                egui::Button::image(icons::icon_image_tinted(
                                    icons::MOVE_UP,
                                    icons::DEFAULT_ICON_SIZE,
                                    tint,
                                ))
                                .frame(false),
                            )
                            .on_hover_text("Move up")
                            .clicked()
                        {
                            actions.push(LevelBrowserAction::MoveUp(idx));
                        }

                        // Icon + selectable name fill the remaining width (LTR).
                        ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                            let glyph = StageGlyph::for_token(&row.kind_token);
                            ui.add(icons::icon_image(
                                ui,
                                glyph.icon(),
                                icons::DEFAULT_ICON_SIZE,
                            ));
                            let mut label = RichText::new(&row.display_name);
                            if row.is_open {
                                label = label.strong();
                            }
                            if row.missing {
                                label = label.color(missing_color());
                            }
                            let resp = ui
                                .selectable_label(state.selected == Some(idx), label)
                                .on_hover_text(if row.rel_path.is_empty() {
                                    row.display_name.clone()
                                } else {
                                    row.rel_path.clone()
                                });
                            if resp.clicked() {
                                state.selected = Some(idx);
                            }
                            if resp.double_clicked() {
                                actions.push(LevelBrowserAction::Open(idx));
                            }
                        });
                    });
                });
            }
        });

    if authoring {
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::image_and_text(
                    icons::icon_image_tinted(icons::ADD, icons::DEFAULT_ICON_SIZE, tint),
                    "New Level…",
                ))
                .clicked()
            {
                actions.push(LevelBrowserAction::NewLevel);
            }
            if ui.button("Add Existing…").clicked() {
                actions.push(LevelBrowserAction::AddExisting);
            }
        });
    }

    // An explicit Open button for the current selection (double-click also works).
    let selected = state.selected.filter(|&s| s < rows.len());
    if let Some(sel) = selected
        && ui.button("Open Selected Level").clicked()
    {
        actions.push(LevelBrowserAction::Open(sel));
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_classifies_known_tokens() {
        assert_eq!(StageGlyph::for_token("level"), StageGlyph::Level);
        assert_eq!(StageGlyph::for_token("asset"), StageGlyph::Asset);
        assert_eq!(StageGlyph::for_token("assembly"), StageGlyph::Assembly);
        assert_eq!(StageGlyph::for_token("player"), StageGlyph::Player);
    }

    #[test]
    fn glyph_is_case_and_whitespace_tolerant() {
        assert_eq!(StageGlyph::for_token("  ASSET "), StageGlyph::Asset);
    }

    #[test]
    fn glyph_falls_back_to_level_for_untagged_or_unknown() {
        // A registry entry with no / unknown kind is still a level by context.
        assert_eq!(StageGlyph::for_token(""), StageGlyph::Level);
        assert_eq!(StageGlyph::for_token("prop"), StageGlyph::Level);
    }

    #[test]
    fn move_targets_bound_first_and_last_rows() {
        // Single-element list: nowhere to move.
        assert_eq!(move_targets(0, 1), (false, false));
        // First of three: down only; last: up only; middle: both.
        assert_eq!(move_targets(0, 3), (false, true));
        assert_eq!(move_targets(1, 3), (true, true));
        assert_eq!(move_targets(2, 3), (true, false));
    }
}
