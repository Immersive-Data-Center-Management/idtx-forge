//! Floating toolbar rendered on top of the 3D viewport.
//!
//! Call [`viewport_toolbar`] from the viewport `CentralPanel` after painting
//! the render texture. It draws an `egui::Area` pinned to the full viewport
//! width (minus equal left/right margins) and constrained to the viewport rect,
//! and returns the new [`RenderMode`] if the user changed it.
//!
//! # Extending the toolbar
//!
//! - Add variants to `RenderMode` and update its `label`/`icon` impls.
//! - Add new toolbar groups (buttons, dropdowns) inside the `ui.horizontal`
//!   block in `viewport_toolbar`, separated by `ui.separator()`.

use egui::{Area, Color32, Frame, Id, Layout, Rect, RichText, Stroke, Ui, Vec2};

use crate::icons;

// ── Render mode ───────────────────────────────────────────────────────────────

/// Rendering style for the 3D viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    #[default]
    Shaded,
    Wireframe,
}

// ── Gizmo transform space ─────────────────────────────────────────────────────

/// Coordinate space used by the transform gizmo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GizmoSpace {
    /// Axes follow the selected object's local orientation.
    #[default]
    Local,
    /// Axes are aligned to the world coordinate frame.
    World,
}

impl RenderMode {
    fn label(self) -> &'static str {
        match self {
            Self::Shaded => "Shaded",
            Self::Wireframe => "Wire",
        }
    }

    fn icon(self) -> egui::ImageSource<'static> {
        match self {
            Self::Shaded => icons::SHADED,
            Self::Wireframe => icons::WIREFRAME,
        }
    }
}

impl GizmoSpace {
    fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::World => "World",
        }
    }

    fn icon(self) -> egui::ImageSource<'static> {
        match self {
            Self::Local => icons::GIZMO_LOCAL,
            Self::World => icons::GIZMO_WORLD,
        }
    }
}

// ── Camera view label ─────────────────────────────────────────────────────────

/// Indicator of the viewport camera's current view, also used as the selection
/// emitted by the interactive camera-view dropdown.
///
/// Reflects the numpad view presets (or free-orbit perspective) and is drawn
/// right-aligned in the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraViewLabel {
    #[default]
    Perspective,
    Front,
    Back,
    Left,
    Right,
    Top,
    Bottom,
}

impl CameraViewLabel {
    fn label(self) -> &'static str {
        match self {
            Self::Perspective => "Perspective",
            Self::Front => "Front",
            Self::Back => "Back",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Top => "Top",
            Self::Bottom => "Bottom",
        }
    }

    /// The six orthographic axis views offered by the camera-view dropdown, in
    /// menu order. Excludes `Perspective`, which is handled by the projection
    /// toggle row rather than as a selectable axis view.
    const AXIS_VIEWS: [Self; 6] = [
        Self::Front,
        Self::Back,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
    ];
}

// ── Toolbar ───────────────────────────────────────────────────────────────────

/// Values returned by [`viewport_toolbar`] when the user changes toolbar state.
#[derive(Debug, Default)]
pub struct ToolbarChanges {
    /// Set when the user selects a different render mode.
    pub render_mode: Option<RenderMode>,
    /// Set when the user selects a different gizmo transform space.
    pub gizmo_space: Option<GizmoSpace>,
    /// Set when the user picks an orthographic axis view from the camera-view
    /// dropdown. Only the six axis variants are ever emitted — never
    /// `CameraViewLabel::Perspective` (that is reached via `toggle_projection`).
    pub view_preset: Option<CameraViewLabel>,
    /// `true` when the user clicked the projection toggle row (perspective ↔
    /// orthographic).
    pub toggle_projection: bool,
}

/// Draw the floating viewport toolbar spanning the full viewport width.
///
/// `current_ortho` reports whether the camera currently uses an orthographic
/// projection, so the projection toggle row can show its state.
///
/// Returns a [`ToolbarChanges`] describing any selections the user made this frame.
#[must_use]
pub fn viewport_toolbar(
    ctx: &egui::Context,
    viewport_rect: Rect,
    current_mode: RenderMode,
    current_gizmo_space: GizmoSpace,
    current_view: CameraViewLabel,
    current_ortho: bool,
) -> ToolbarChanges {
    const MARGIN: f32 = 8.0;
    const INNER_MARGIN_X: i8 = 8;
    const INNER_MARGIN_Y: i8 = 4;
    const ROUNDING: f32 = 6.0;
    let th = crate::theme::current();
    let bg = th.overlay_bg;
    let border = th.overlay_border;
    let active_tint = th.accent;

    let toolbar_width = (viewport_rect.width() - 2.0 * MARGIN).max(0.0);
    let mut changes = ToolbarChanges::default();

    Area::new(Id::new("viewport_toolbar"))
        .fixed_pos(viewport_rect.min + Vec2::new(MARGIN, MARGIN))
        .constrain_to(viewport_rect)
        .interactable(true)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            // `set_width` pins both the minimum *and* maximum width. Using only
            // `set_min_width` here lets the right-aligned camera indicator (a
            // `right_to_left` layout that fills `available_width`) expand the
            // toolbar to the full window width — which never shrinks back when
            // the window is made smaller, so the toolbar spills past the
            // viewport's right edge.
            ui.set_width(toolbar_width);
            Frame::new()
                .fill(bg)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(ROUNDING)
                .inner_margin(egui::Margin::symmetric(INNER_MARGIN_X, INNER_MARGIN_Y))
                .show(ui, |ui| {
                    let content_width =
                        (toolbar_width - (f32::from(INNER_MARGIN_X) * 2.0)).max(0.0);
                    ui.set_width(content_width);
                    ui.horizontal(|ui| {
                        // ── Render-mode dropdown ──────────────────────────
                        if let Some(mode) = render_mode_dropdown(ui, current_mode, active_tint) {
                            changes.render_mode = Some(mode);
                        }

                        ui.separator();

                        // ── Gizmo-space dropdown ──────────────────────────
                        if let Some(space) =
                            gizmo_space_dropdown(ui, current_gizmo_space, active_tint)
                        {
                            changes.gizmo_space = Some(space);
                        }

                        // ── Future toolbar groups go here ─────────────────
                        // ui.separator();
                        // your_group(ui, ...);

                        // ── Camera view dropdown (right-aligned) ──────────
                        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                            let (chosen_view, toggled) = camera_view_dropdown(
                                ui,
                                current_view,
                                current_ortho,
                                active_tint,
                            );
                            changes.view_preset = chosen_view;
                            changes.toggle_projection = toggled;
                        });
                    });
                });
        });

    changes
}

// ── Render-mode dropdown ──────────────────────────────────────────────────────

fn render_mode_dropdown(
    ui: &mut Ui,
    current_mode: RenderMode,
    active_tint: Color32,
) -> Option<RenderMode> {
    let mut chosen = None;

    // Single Button widget: captures clicks on icon AND text uniformly.
    let icon_img = egui::Image::new(current_mode.icon())
        .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
        .tint(active_tint);
    let label = RichText::new(current_mode.label().to_string())
        .small()
        .color(active_tint);
    let btn_response = ui.add(egui::Button::image_and_text(icon_img, label).frame(false));

    // Menu popup anchored below the button.
    egui::Popup::menu(&btn_response).show(|ui| {
        ui.with_layout(Layout::top_down_justified(egui::Align::Min), |ui| {
            for mode in [RenderMode::Shaded, RenderMode::Wireframe] {
                let is_active = mode == current_mode;
                if menu_item(ui, mode, is_active, active_tint).clicked() && mode != current_mode {
                    chosen = Some(mode);
                }
            }
        });
    });

    chosen
}

// ── Menu item ─────────────────────────────────────────────────────────────────

/// A menu row rendered as a single `Button` so clicks anywhere in the row
/// (icon or label) are captured uniformly.
fn menu_item(ui: &mut Ui, mode: RenderMode, active: bool, active_tint: Color32) -> egui::Response {
    let text_color = if active {
        active_tint
    } else {
        ui.style().visuals.text_color()
    };
    // Active items show the checkmark as the leading icon; inactive show the
    // mode icon.  Both tinted appropriately so the active selection is obvious.
    let leading_src = if active { icons::OK } else { mode.icon() };
    let icon_tint = if active {
        active_tint
    } else {
        ui.style().visuals.text_color().gamma_multiply(0.7)
    };

    let icon_img = egui::Image::new(leading_src)
        .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
        .tint(icon_tint);
    let label = RichText::new(mode.label()).small().color(text_color);

    ui.add(egui::Button::image_and_text(icon_img, label))
}

// ── Camera view dropdown ────────────────────────────────────────────────────

/// Interactive camera-view control: a right-aligned button showing the current
/// view that opens a menu of the six orthographic axis views plus a
/// perspective/orthographic projection toggle.
///
/// Returns `(chosen_axis_view, projection_toggled)`. `chosen_axis_view` is only
/// ever one of the six axis variants (never `Perspective`); the projection row
/// reports via the boolean instead.
///
/// This gives keyboards without a numpad (most laptops and compact keyboards) a
/// mouse path to the Blender-numpad axis views, which are otherwise only bound
/// to physical numpad keys.
fn camera_view_dropdown(
    ui: &mut Ui,
    current_view: CameraViewLabel,
    current_ortho: bool,
    active_tint: Color32,
) -> (Option<CameraViewLabel>, bool) {
    let mut chosen = None;
    let mut toggled = false;

    // Button layout: `[camera icon] Label`.
    let icon_img = egui::Image::new(icons::CAMERA)
        .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
        .tint(active_tint);
    let label = RichText::new(current_view.label()).small().color(active_tint);
    let btn_response = ui.add(egui::Button::image_and_text(icon_img, label).frame(false));

    egui::Popup::menu(&btn_response).show(|ui| {
        ui.with_layout(Layout::top_down_justified(egui::Align::Min), |ui| {
            for view in CameraViewLabel::AXIS_VIEWS {
                let is_active = view == current_view;
                if camera_view_menu_item(ui, view, is_active, active_tint).clicked()
                    && !is_active
                {
                    chosen = Some(view);
                }
            }

            ui.separator();

            // Projection toggle row — the checkmark reflects the current mode.
            let proj_label = if current_ortho {
                "Orthographic"
            } else {
                "Perspective"
            };
            let leading_src = if current_ortho {
                icons::OK
            } else {
                icons::CAMERA
            };
            let icon_tint = if current_ortho {
                active_tint
            } else {
                ui.style().visuals.text_color().gamma_multiply(0.7)
            };
            let proj_icon = egui::Image::new(leading_src)
                .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
                .tint(icon_tint);
            let proj_text = RichText::new(proj_label)
                .small()
                .color(ui.style().visuals.text_color());
            if ui
                .add(egui::Button::image_and_text(proj_icon, proj_text))
                .clicked()
            {
                toggled = true;
            }
        });
    });

    (chosen, toggled)
}

/// A camera-view menu row rendered as a single `Button` (icon + label), with a
/// leading checkmark when the view is the active one.
fn camera_view_menu_item(
    ui: &mut Ui,
    view: CameraViewLabel,
    active: bool,
    active_tint: Color32,
) -> egui::Response {
    let text_color = if active {
        active_tint
    } else {
        ui.style().visuals.text_color()
    };
    let leading_src = if active { icons::OK } else { icons::CAMERA };
    let icon_tint = if active {
        active_tint
    } else {
        ui.style().visuals.text_color().gamma_multiply(0.7)
    };

    let icon_img = egui::Image::new(leading_src)
        .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
        .tint(icon_tint);
    let label = RichText::new(view.label()).small().color(text_color);

    ui.add(egui::Button::image_and_text(icon_img, label))
}

// ── Gizmo-space dropdown ──────────────────────────────────────────────────────

fn gizmo_space_dropdown(
    ui: &mut Ui,
    current_space: GizmoSpace,
    active_tint: Color32,
) -> Option<GizmoSpace> {
    let mut chosen = None;

    let icon_img = egui::Image::new(current_space.icon())
        .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
        .tint(active_tint);
    let label = RichText::new(current_space.label())
        .small()
        .color(active_tint);
    let btn_response = ui.add(egui::Button::image_and_text(icon_img, label).frame(false));

    egui::Popup::menu(&btn_response).show(|ui| {
        ui.with_layout(Layout::top_down_justified(egui::Align::Min), |ui| {
            for space in [GizmoSpace::Local, GizmoSpace::World] {
                if gizmo_space_menu_item(ui, space, space == current_space, active_tint).clicked()
                    && space != current_space
                {
                    chosen = Some(space);
                }
            }
        });
    });

    chosen
}

fn gizmo_space_menu_item(
    ui: &mut Ui,
    space: GizmoSpace,
    active: bool,
    active_tint: Color32,
) -> egui::Response {
    let text_color = if active {
        active_tint
    } else {
        ui.style().visuals.text_color()
    };
    let leading_src = if active { icons::OK } else { space.icon() };
    let icon_tint = if active {
        active_tint
    } else {
        ui.style().visuals.text_color().gamma_multiply(0.7)
    };

    let icon_img = egui::Image::new(leading_src)
        .fit_to_exact_size(Vec2::splat(icons::TOOLBAR_ICON_SIZE))
        .tint(icon_tint);
    let label = RichText::new(space.label()).small().color(text_color);

    ui.add(egui::Button::image_and_text(icon_img, label))
}
