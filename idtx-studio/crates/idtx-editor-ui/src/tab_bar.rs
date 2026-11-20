//! Godot-style "folder" tabs, drawn to the theme.
//!
//! egui's stock [`egui::Ui::selectable_label`] can't produce the reference
//! look — it only exposes the one global `Visuals::selection` fill and offers
//! no per-corner rounding — so these tabs are painted by hand from
//! [`crate::theme`] tokens:
//!
//! * the active tab gets a lighter fill equal to the page colour
//!   ([`Theme::tab_active_fill`](crate::theme::Theme::tab_active_fill)) with an
//!   accent spine along its top and **open-bottom corners**, so it reads as a
//!   folder tab joined to the content beneath it;
//! * inactive tabs are flat and dimmer, with a hairline outline;
//! * the active tab carries extra horizontal padding;
//! * each tab lays out an optional dirty dot, its label and an optional close
//!   (×) affordance.
//!
//! Two entry points share the same drawing: [`stage_tabs`] for the top-level
//! document tabs (closeable, with an unsaved-changes dot) and [`sub_tabs`] for
//! simple segmented tab rows (Properties / Layer Stack / Stage, etc.).

use egui::{Align2, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, TextStyle, Vec2};

/// What the user did to a [`stage_tabs`] row this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabEvent {
    /// A different tab was clicked.
    Activate(u64),
    /// A tab's close (×) button was clicked.
    Close(u64),
}

/// One document tab in the top-level [`stage_tabs`] bar.
#[derive(Clone, Debug)]
pub struct StageTab {
    /// Stable tab identity (echoed back in [`TabEvent`]).
    pub id: u64,
    pub label: String,
    pub active: bool,
    /// Whether to show the unsaved-changes dot.
    pub dirty: bool,
}

/// Draw the top-level, closeable document tabs. Returns the single event the
/// user triggered this frame, if any.
pub fn stage_tabs(ui: &mut egui::Ui, tabs: &[StageTab]) -> Option<TabEvent> {
    let th = crate::theme::current();
    let mut event = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = th.tab_gap;
        ui.set_height(th.tab_height);
        for t in tabs {
            let (resp, close_clicked) = draw_tab(ui, &t.label, t.active, t.dirty, true);
            if close_clicked {
                event = Some(TabEvent::Close(t.id));
            } else if resp.clicked() && !t.active {
                event = Some(TabEvent::Activate(t.id));
            }
        }
    });
    event
}

/// Draw a simple segmented tab row (no close button, no dirty dot). Returns the
/// index of a newly-selected tab, if one was clicked this frame.
pub fn sub_tabs(ui: &mut egui::Ui, labels: &[&str], active: usize) -> Option<usize> {
    let th = crate::theme::current();
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = th.tab_gap;
        ui.set_height(th.tab_height);
        for (i, label) in labels.iter().enumerate() {
            let (resp, _) = draw_tab(ui, label, i == active, false, false);
            if resp.clicked() && i != active {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Paint one tab cell and hit-test it (plus its optional close button).
///
/// Returns the cell's [`egui::Response`] and whether the close button was
/// clicked this frame.
fn draw_tab(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    dirty: bool,
    closeable: bool,
) -> (egui::Response, bool) {
    let th = crate::theme::current();
    let font = ui
        .style()
        .text_styles
        .get(&TextStyle::Button)
        .cloned()
        .unwrap_or_else(|| FontId::proportional(th.font_button));

    // Measure the label so the cell is exactly wide enough.
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), th.tab_text_active)
        .size()
        .x;
    let pad_x = if active {
        th.tab_pad_active_x
    } else {
        th.tab_pad_inactive_x
    };
    let dot_w = if dirty { 12.0 } else { 0.0 };
    let close_w = if closeable { 20.0 } else { 0.0 };
    let width = pad_x * 2.0 + dot_w + text_w + close_w;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, th.tab_height), Sense::click());

    // ── Body ──────────────────────────────────────────────────────────────
    let painter = ui.painter_at(rect);
    let fill = if active {
        th.tab_active_fill
    } else if resp.hovered() {
        th.tab_hover_fill
    } else {
        th.tab_inactive_fill
    };
    // Open-bottom corners: round the top, square the bottom so the tab merges
    // into the content area below it.
    let top_round = CornerRadius {
        nw: th.corner_radius,
        ne: th.corner_radius,
        sw: 0,
        se: 0,
    };
    painter.rect_filled(rect, top_round, fill);
    if active {
        // Accent spine along the top edge.
        let spine = Rect::from_min_max(rect.left_top(), Pos2::new(rect.right(), rect.top() + 2.0));
        painter.rect_filled(spine, top_round, th.accent);
    } else {
        // Hairline outline so inactive tabs stay legible against the bar.
        painter.rect_stroke(
            rect,
            top_round,
            Stroke::new(1.0, th.group_stroke),
            StrokeKind::Inside,
        );
    }

    // ── Contents (dirty dot · label · close) ───────────────────────────────
    let cy = rect.center().y;
    let mut x = rect.left() + pad_x;
    if dirty {
        painter.circle_filled(Pos2::new(x + 3.0, cy), 3.0, th.tab_dirty);
        x += dot_w;
    }
    let text_color = if active {
        th.tab_text_active
    } else {
        th.tab_text_inactive
    };
    painter.text(
        Pos2::new(x, cy),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        text_color,
    );

    let mut close_clicked = false;
    if closeable {
        let c_center = Pos2::new(rect.right() - pad_x - 6.0, cy);
        let c_rect = Rect::from_center_size(c_center, Vec2::splat(16.0));
        let c_resp = ui.interact(c_rect, resp.id.with("close"), Sense::click());
        let col = if c_resp.hovered() {
            th.tab_text_active
        } else {
            th.tab_close_idle
        };
        ui.painter_at(c_rect)
            .text(c_center, Align2::CENTER_CENTER, "×", font, col);
        close_clicked = c_resp.clicked();
        c_resp.on_hover_text("Close tab");
    }

    (resp, close_clicked)
}
