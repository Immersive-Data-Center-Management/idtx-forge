//! Full-width project rows for the Conductor startup dialog.

use std::path::Path;

use egui::{Color32, Rect, RichText, Sense, TextWrapMode, Ui, Vec2, WidgetText};
use idtx_editor_ui::{icons, theme};

const PROJECT_ICON: egui::ImageSource<'static> =
    egui::include_image!("../../../apps/idtx-conductor/idtx_conductor_256.ico");
const ROW_HEIGHT: f32 = 72.0;
const PADDING: f32 = 8.0;
const REMOVE_SIZE: f32 = 32.0;

fn project_path_label(path: &Path) -> String {
    let display = idtx_project::strip_verbatim_prefix(path)
        .display()
        .to_string();
    if let Some(rest) = display.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else {
        display.strip_prefix("//?/").unwrap_or(&display).to_owned()
    }
}

pub(crate) struct ProjectRowResponse {
    pub(crate) selected: bool,
    pub(crate) open: bool,
    pub(crate) remove: bool,
}

/// Selecting a missing entry is allowed; opening it is not. The remove button
/// has its own hit target and only asks the shell to forget the recent entry.
pub(crate) fn project_row(ui: &mut Ui, manifest: &Path, selected: bool) -> ProjectRowResponse {
    let manifest_label = project_path_label(manifest);
    let name = manifest
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| manifest_label.clone());
    let location = project_path_label(manifest.parent().unwrap_or(manifest));
    let exists = manifest.is_file();
    let palette = theme::current();

    ui.push_id(manifest, |ui| {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::hover());
        let remove_rect = Rect::from_center_size(
            egui::pos2(rect.right() - PADDING - REMOVE_SIZE / 2.0, rect.center().y),
            Vec2::splat(REMOVE_SIZE),
        );
        // The later trash widget takes priority over this full-row hit target.
        // Also suppress row actions whenever the remove button is clicked.
        let mut entry = ui.interact(rect, ui.id().with("select"), Sense::click());
        let clicked = entry.clicked();
        let is_selected = selected || clicked;
        let hovered = ui.rect_contains_pointer(rect);
        let fill = if !exists {
            palette.window_bg
        } else if is_selected {
            if hovered {
                palette.selection_fill.gamma_multiply(1.15)
            } else {
                palette.selection_fill
            }
        } else if hovered {
            palette.button_hover_bg
        } else {
            palette.content_bg
        };
        ui.painter().rect_filled(rect, 5.0, fill);
        if !exists {
            let alpha = if hovered {
                76
            } else if is_selected {
                60
            } else {
                36
            };
            let error = palette.status_err;
            ui.painter().rect_filled(
                rect,
                5.0,
                Color32::from_rgba_unmultiplied(error.r(), error.g(), error.b(), alpha),
            );
        }
        if is_selected {
            ui.painter().rect_filled(
                Rect::from_min_size(
                    rect.min + Vec2::splat(PADDING),
                    Vec2::new(3.0, ROW_HEIGHT - 2.0 * PADDING),
                ),
                1.5,
                if exists {
                    palette.accent
                } else {
                    palette.status_err
                },
            );
        }

        let icon_size = ROW_HEIGHT - 2.0 * PADDING;
        let icon_rect = Rect::from_min_size(
            rect.min + Vec2::new(PADDING + 8.0, PADDING),
            Vec2::splat(icon_size),
        );
        egui::Image::new(PROJECT_ICON).paint_at(ui, icon_rect);

        let text_left = icon_rect.right() + 14.0;
        let text_right = remove_rect.left() - 12.0;
        let text_width = (text_right - text_left).max(0.0);
        let title_y = rect.top() + 12.0;
        let name_color = if exists {
            palette.text_strong
        } else {
            palette.status_err
        };
        let mut title_width = text_width;
        if !exists {
            let status = WidgetText::from(
                RichText::new("Project file missing")
                    .small()
                    .color(palette.status_err),
            )
            .into_galley(
                ui,
                Some(TextWrapMode::Truncate),
                text_width,
                egui::TextStyle::Body,
            );
            title_width = (text_width - status.size().x - 12.0).max(0.0);
            ui.painter().galley(
                egui::pos2(text_right - status.size().x, title_y + 2.0),
                status,
                palette.status_err,
            );
        }
        let title = WidgetText::from(RichText::new(&name).strong().size(16.0).color(name_color))
            .into_galley(
                ui,
                Some(TextWrapMode::Truncate),
                title_width,
                egui::TextStyle::Body,
            );
        ui.painter()
            .galley(egui::pos2(text_left, title_y), title, name_color);

        let path_color = if exists {
            palette.text_dim
        } else {
            palette.status_err
        };
        let folder_rect =
            Rect::from_min_size(egui::pos2(text_left, rect.top() + 42.0), Vec2::splat(14.0));
        icons::icon_image_tinted(icons::FOLDER, 14.0, path_color).paint_at(ui, folder_rect);
        let path_left = folder_rect.right() + 8.0;
        let path = WidgetText::from(RichText::new(&location).size(13.0).color(path_color))
            .into_galley(
                ui,
                Some(TextWrapMode::Truncate),
                (text_right - path_left).max(0.0),
                egui::TextStyle::Body,
            );
        ui.painter()
            .galley(egui::pos2(path_left, rect.top() + 40.0), path, path_color);

        entry.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, is_selected, &name)
        });
        if exists {
            entry = entry.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        entry = entry.on_hover_text(if exists {
            format!("{manifest_label}\nDouble-click to open.")
        } else {
            format!("{manifest_label}\nThis project file no longer exists.")
        });
        let remove = ui
            .put(
                remove_rect,
                egui::Button::image(icons::icon_image_tinted(
                    icons::TRASH,
                    18.0,
                    palette.text_dim,
                ))
                .frame(false)
                .min_size(Vec2::splat(REMOVE_SIZE)),
            )
            .on_hover_text("Remove from recent projects. Project files will be kept.")
            .clicked();

        ProjectRowResponse {
            selected: clicked && !remove,
            open: exists && entry.double_clicked() && !remove,
            remove,
        }
    })
    .inner
}
