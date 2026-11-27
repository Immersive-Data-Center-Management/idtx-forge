//! Project Asset Browser panel.
//!
//! The Conductor's bottom "Asset Browser" panel lists the files under the
//! active project's directory tree (fed by `idtx_project::IdtxProject::list_assets`,
//! which the shell snapshots each frame). It supports folder navigation and —
//! for USD stages — authoring a reference/payload arc onto the currently
//! selected scene prim, using the asset's **project-relative** path so the
//! authored arc stays portable (resolved via the project-root search path
//! anchored at the project root).
//!
//! Like [`crate::level_browser`], this is a **pure egui panel**: it takes a
//! UI-neutral snapshot ([`AssetRow`]) and returns [`AssetBrowserAction`]s the
//! shell maps onto its own command bus. It never touches `idtx-project` or USD
//! directly, keeping `idtx-editor-ui` at the bottom of the dependency stack —
//! hence its own [`AssetGlyph`] enum mirroring `idtx_project::AssetKind`.

use egui::{Layout, RichText, Ui};

use crate::icons;

/// Which file-type glyph an asset row draws. Mirrors `idtx_project::AssetKind`
/// without depending on it (the shell maps one to the other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetGlyph {
    /// A directory.
    Folder,
    /// A USD stage (`.usd`/`.usda`/`.usdc`/`.usdz`) — referenceable/payloadable.
    Usd,
    /// A raster/HDR texture.
    Image,
    /// A `MaterialX` document.
    Material,
    /// A mesh/model interchange file.
    Mesh,
    /// An audio clip.
    Audio,
    /// A video clip.
    Video,
    /// A structured-data / config sidecar.
    Data,
    /// Anything else.
    File,
}

impl AssetGlyph {
    /// The embedded SVG icon for this glyph.
    #[must_use]
    pub fn icon(self) -> egui::ImageSource<'static> {
        match self {
            Self::Folder => icons::FOLDER,
            Self::Usd => icons::FILE_USD,
            Self::Image => icons::FILE_IMAGE,
            Self::Material => icons::FILE_MATERIAL,
            Self::Mesh => icons::FILE_MESH,
            Self::Audio => icons::FILE_AUDIO,
            Self::Video => icons::FILE_VIDEO,
            Self::Data => icons::FILE_DATA,
            Self::File => icons::FILE,
        }
    }
}

/// The drag-and-drop payload carried while dragging a USD-stage asset row from
/// the Asset Browser onto a scene-tree prim. It holds only the asset's
/// **project-relative** path so the arc authored on drop stays portable
/// (resolved via the project-root search path). The type is
/// shared with [`crate::scene_tree`], the drop target, via egui's typed
/// drag-and-drop channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetDragPayload {
    /// Project-relative path (`/`-separated) of the dragged USD stage asset.
    pub rel_path: String,
}

/// A single asset-browser row — a per-frame snapshot the shell assembles from
/// one project directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRow {
    /// Final path component (file or directory name), for display.
    pub name: String,
    /// Project-relative path with `/` separators (navigation + arc authoring).
    pub rel_path: String,
    /// File-type glyph.
    pub glyph: AssetGlyph,
    /// `true` for directories (double-click navigates in).
    pub is_dir: bool,
    /// `true` for USD stages (enables the reference/payload affordances).
    pub is_stage: bool,
}

/// Persistent selection state for the asset browser (survives across frames).
#[derive(Debug, Clone, Default)]
pub struct AssetBrowserState {
    /// Index of the currently selected row, if any.
    pub selected: Option<usize>,
}

/// An action collected during an asset-browser pass, applied by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetBrowserAction {
    /// Navigate to this project-relative subdir (`""` = root). Emitted by
    /// entering a folder, a breadcrumb click, or the up affordance.
    Navigate(String),
    /// Select the row at this index.
    Select(usize),
    /// Open this project-relative USD stage in a tab. Emitted by double-clicking
    /// a stage row.
    OpenStage(String),
    /// Author a reference to this project-relative asset path onto the current
    /// scene selection.
    AddReference(String),
    /// Author a payload of this project-relative asset path onto the current
    /// scene selection.
    AddPayload(String),
}

/// The parent of a `/`-separated project-relative subdir, or `""` at the root.
#[must_use]
pub fn parent_subdir(subdir: &str) -> String {
    match subdir.trim_end_matches('/').rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    }
}

/// Draw the project asset browser and return the collected actions.
///
/// `current_subdir` is the `/`-separated project-relative directory being
/// viewed (`""` = root); `rows` are its immediate children; `selected_prim`
/// is the active scene selection's prim path (the reference/payload target),
/// and `authoring` gates the arc-authoring affordances.
pub fn asset_browser_panel(
    ui: &mut Ui,
    current_subdir: &str,
    rows: &[AssetRow],
    selected_prim: Option<&str>,
    authoring: bool,
    state: &mut AssetBrowserState,
) -> Vec<AssetBrowserAction> {
    let mut actions = Vec::new();
    let tint = ui.style().visuals.text_color();

    // ── Breadcrumb / navigation header ────────────────────────────────────
    ui.horizontal(|ui| {
        ui.add(icons::icon_image(
            ui,
            icons::PACKED,
            icons::DEFAULT_ICON_SIZE,
        ));
        if ui
            .add_enabled(
                !current_subdir.is_empty(),
                egui::Button::new("Assets").frame(false),
            )
            .on_hover_text("Project root")
            .clicked()
        {
            actions.push(AssetBrowserAction::Navigate(String::new()));
        }
        // One clickable crumb per path segment, each navigating to its prefix.
        let mut prefix = String::new();
        for segment in current_subdir.split('/').filter(|s| !s.is_empty()) {
            ui.label(RichText::new("›").weak());
            if prefix.is_empty() {
                segment.clone_into(&mut prefix);
            } else {
                prefix = format!("{prefix}/{segment}");
            }
            let is_last = prefix.len() == current_subdir.trim_end_matches('/').len();
            if is_last {
                ui.label(RichText::new(segment).strong());
            } else if ui.add(egui::Button::new(segment).frame(false)).clicked() {
                actions.push(AssetBrowserAction::Navigate(prefix.clone()));
            }
        }
    });
    ui.separator();

    // Drop any stale selection before iterating so downstream indices are valid.
    if state.selected.is_some_and(|s| s >= rows.len()) {
        state.selected = None;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // A leading ".." row when not at the root, to step up one level.
            if !current_subdir.is_empty() {
                ui.horizontal(|ui| {
                    ui.add(icons::icon_image(ui, icons::FOLDER, icons::DEFAULT_ICON_SIZE));
                    if ui
                        .add(egui::Button::new("..").frame(false))
                        .on_hover_text("Up one folder")
                        .clicked()
                    {
                        actions.push(AssetBrowserAction::Navigate(parent_subdir(current_subdir)));
                    }
                });
            }

            if rows.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("Empty folder").small().weak());
            }

            for (idx, row) in rows.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(icons::icon_image(ui, row.glyph.icon(), icons::DEFAULT_ICON_SIZE));
                    // USD stages are both click targets (select / double-click to
                    // open) and drag sources for authoring an arc onto a scene-tree
                    // prim. We sense click *and* drag on a single widget and
                    // arm the drag payload with `dnd_set_drag_payload` rather than
                    // `dnd_drag_source`: the latter overlays a drag-only widget that
                    // steals the pointer (swallowing clicks/double-clicks and forcing
                    // the Grab cursor on hover). `dnd_set_drag_payload` deliberately
                    // omits the Grab cursor for click-sensing widgets, so hovering no
                    // longer looks like a grab and double-click opens reliably.
                    let resp = if authoring && row.is_stage {
                        let resp = ui
                            .selectable_label(state.selected == Some(idx), &row.name)
                            .interact(egui::Sense::click_and_drag());
                        resp.dnd_set_drag_payload(AssetDragPayload {
                            rel_path: row.rel_path.clone(),
                        });
                        resp.on_hover_text(format!(
                            "{}\nDouble-click to open · drag onto a scene prim to nest it as a payload child (hold Shift to reference)",
                            row.rel_path
                        ))
                    } else {
                        let resp = ui.selectable_label(state.selected == Some(idx), &row.name);
                        if row.is_stage {
                            resp.on_hover_text(format!("{}\nDouble-click to open", row.rel_path))
                        } else {
                            resp.on_hover_text(&row.rel_path)
                        }
                    };
                    if resp.clicked() {
                        state.selected = Some(idx);
                        actions.push(AssetBrowserAction::Select(idx));
                    }
                    if resp.double_clicked() && row.is_dir {
                        actions.push(AssetBrowserAction::Navigate(row.rel_path.clone()));
                    }
                    // Double-clicking a USD stage opens it in a tab.
                    if resp.double_clicked() && row.is_stage {
                        actions.push(AssetBrowserAction::OpenStage(row.rel_path.clone()));
                    }
                    // Per-row arc affordances for stages, right-aligned.
                    if authoring && row.is_stage {
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            let hint = selected_prim.map_or_else(
                                || "Select a prim in the scene tree first".to_owned(),
                                |p| format!("Add payload of {} to {p}", row.name),
                            );
                            if ui
                                .add_enabled(
                                    selected_prim.is_some(),
                                    egui::Button::image_and_text(
                                        icons::icon_image_tinted(
                                            icons::LAYER_PAYLOAD,
                                            icons::DEFAULT_ICON_SIZE,
                                            tint,
                                        ),
                                        "Payload",
                                    )
                                    .small(),
                                )
                                .on_hover_text(hint)
                                .clicked()
                            {
                                actions.push(AssetBrowserAction::AddPayload(row.rel_path.clone()));
                            }
                            let hint = selected_prim.map_or_else(
                                || "Select a prim in the scene tree first".to_owned(),
                                |p| format!("Add reference to {} on {p}", row.name),
                            );
                            if ui
                                .add_enabled(
                                    selected_prim.is_some(),
                                    egui::Button::image_and_text(
                                        icons::icon_image_tinted(
                                            icons::LAYER_REFERENCE,
                                            icons::DEFAULT_ICON_SIZE,
                                            tint,
                                        ),
                                        "Reference",
                                    )
                                    .small(),
                                )
                                .on_hover_text(hint)
                                .clicked()
                            {
                                actions.push(AssetBrowserAction::AddReference(row.rel_path.clone()));
                            }
                            // Open (leftmost) — always available; a convenience
                            // equivalent to double-clicking the row.
                            if ui
                                .add(
                                    egui::Button::image_and_text(
                                        icons::icon_image_tinted(
                                            icons::FILE_USD,
                                            icons::DEFAULT_ICON_SIZE,
                                            tint,
                                        ),
                                        "Open",
                                    )
                                    .small(),
                                )
                                .on_hover_text(format!("Open {} in a tab", row.name))
                                .clicked()
                            {
                                actions.push(AssetBrowserAction::OpenStage(row.rel_path.clone()));
                            }
                        });
                    }
                });
            }
        });

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_subdir_steps_up_one_level() {
        assert_eq!(parent_subdir(""), "");
        assert_eq!(parent_subdir("assets"), "");
        assert_eq!(parent_subdir("assets/props"), "assets");
        assert_eq!(parent_subdir("assets/props/chairs"), "assets/props");
        // A trailing slash is tolerated.
        assert_eq!(parent_subdir("assets/props/"), "assets");
    }

    #[test]
    fn stale_selection_is_dropped_when_out_of_range() {
        // Simulate navigating into a smaller folder: a selection past the new
        // row count must be cleared, mirroring the level browser's guard.
        let rows = vec![AssetRow {
            name: "a.usda".to_owned(),
            rel_path: "a.usda".to_owned(),
            glyph: AssetGlyph::Usd,
            is_dir: false,
            is_stage: true,
        }];
        let mut state = AssetBrowserState { selected: Some(5) };
        // Reproduce the guard the panel runs.
        if state.selected.is_some_and(|s| s >= rows.len()) {
            state.selected = None;
        }
        assert_eq!(state.selected, None);
    }

    /// Drives the panel headlessly with a stage row present. egui double-click
    /// input needs pointer plumbing the headless context does not replay
    /// cleanly (see [`crate::stage_panel`] tests), so this covers the pure
    /// render shape: a stage row renders without panic and, absent interaction,
    /// emits no action. The double-click→[`AssetBrowserAction::OpenStage`]
    /// mapping is exercised manually in Conductor and by the shell round-trip.
    #[test]
    fn stage_row_renders_and_emits_no_action_without_interaction() {
        let rows = vec![
            AssetRow {
                name: "props".to_owned(),
                rel_path: "props".to_owned(),
                glyph: AssetGlyph::Folder,
                is_dir: true,
                is_stage: false,
            },
            AssetRow {
                name: "hero.usda".to_owned(),
                rel_path: "props/hero.usda".to_owned(),
                glyph: AssetGlyph::Usd,
                is_dir: false,
                is_stage: true,
            },
        ];
        let mut state = AssetBrowserState::default();
        let mut actions = Vec::new();
        egui::__run_test_ui(|ui| {
            actions = asset_browser_panel(ui, "", &rows, Some("/World"), true, &mut state);
        });
        assert!(actions.is_empty());
    }
}
