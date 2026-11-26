//! The single source of truth for IDTX Studio's visual appearance.
//!
//! Everything about how the editor *looks* — fonts, sizes, colours and the
//! geometry of frames and tabs — is defined once in [`Theme::dark`]. Change a
//! value there and it propagates everywhere, because the rest of the UI reads
//! its colours and metrics from the process-global instance returned by
//! [`current`] rather than hard-coding literals.
//!
//! There are two halves to a theme:
//!
//! 1. **egui-owned styling.** The parts egui draws for you — panel
//!    backgrounds, button fills, selection highlight, text sizes, widget corner
//!    radii, spacing — are pushed into [`egui::Style`] once at startup by
//!    [`Theme::apply_style`] (called from [`install`]). After that, every stock
//!    widget (`.small()`, `.strong()`, `ui.button(..)`, `selectable_label`, …)
//!    inherits the theme automatically; no per-call-site change is needed.
//!
//! 2. **Semantic tokens.** Colours and metrics we paint *ourselves* — the
//!    folder-style tab bar ([`crate::tab_bar`]), status badges, the material /
//!    compute node canvases — cannot be expressed through [`egui::Visuals`].
//!    Those live as named fields on [`Theme`] and are read via [`current`].
//!
//! # Changing the look
//!
//! * Colours / sizes for a context → edit the relevant field in [`Theme::dark`].
//! * A custom font → point [`Theme::custom_fonts`] at embedded `.ttf` bytes
//!   (see [`CustomFont`]); leave it empty to keep egui's bundled fonts.
//! * A light theme → add a `Theme::light()` constructor and pick it in
//!   [`install`]; the whole pipeline is colour-agnostic.
//!
//! No runtime theme editor is provided (by design) — this module *is* the
//! editor, in code.

use std::sync::OnceLock;

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

/// A custom font to register with egui at startup.
///
/// Point [`bytes`](Self::bytes) at `include_bytes!("…/MyFont.ttf")` and choose
/// which family it should back. It is inserted at the *front* of that family so
/// it becomes the preferred face, with egui's bundled fonts kept as fallback.
#[derive(Clone, Copy, Debug)]
pub struct CustomFont {
    /// Unique key for this face within the font set.
    pub name: &'static str,
    /// The raw `.ttf`/`.otf` bytes, embedded at compile time.
    pub bytes: &'static [u8],
    /// Which family this face backs.
    pub family: FontSlot,
}

/// Which egui font family a [`CustomFont`] should be registered under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSlot {
    /// The variable-width UI font (labels, buttons, headings).
    Proportional,
    /// The fixed-width font (paths, values, code).
    Monospace,
}

/// Domain colour palettes for the node canvases.
///
/// These are *semantic data encodings* (socket data-type, compute-node role),
/// not window chrome, so they get their own grouped sub-palette. The canvas
/// modules ([`crate::graph_canvas`], [`crate::compute_canvas`]) read these
/// instead of declaring their own literals, keeping the single-source promise.
#[derive(Clone, Copy, Debug)]
pub struct CanvasPalette {
    // Graph node chrome — kept in step with the panel/group colours.
    pub node_body_fill: Color32,
    pub node_outline: Color32,
    pub value_box_fill: Color32,
    pub value_box_text: Color32,

    // Socket data-type colour coding.
    pub socket_string: Color32,
    pub socket_float: Color32,
    pub socket_float3: Color32,
    pub socket_path: Color32,
    pub socket_other: Color32,

    // Compute-node role accents (RGB; the paint pass tints headers with these).
    pub compute_datasource: [u8; 3],
    pub compute_compute: [u8; 3],
    pub compute_consumer: [u8; 3],
    pub compute_other: [u8; 3],
}

/// The complete visual definition of the editor.
///
/// Prefer reading fields through [`current`]; construct new instances only in
/// theme constructors like [`Theme::dark`].
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    // ── Surfaces ────────────────────────────────────────────────────────────
    /// Floating window / modal background.
    pub window_bg: Color32,
    /// Panel & page background (`Visuals::panel_fill`).
    pub content_bg: Color32,
    /// Darker strip behind the tab bar, so the active tab reads as lighter.
    pub tab_bar_bg: Color32,
    /// Faint alternating-row / zebra background.
    pub faint_bg: Color32,
    /// Darkest inset surface (text-edit & code backgrounds).
    pub extreme_bg: Color32,

    // ── Grouping frames (the "visually grouped content" boxes) ───────────────
    /// Fill of a grouped sub-panel card.
    pub group_fill: Color32,
    /// Hairline border around a grouped card / resting widget outline.
    pub group_stroke: Color32,
    /// Stronger border used on hover / emphasis.
    pub group_stroke_strong: Color32,

    // ── Buttons / interactive widget fills ───────────────────────────────────
    pub button_bg: Color32,
    pub button_hover_bg: Color32,
    pub button_active_bg: Color32,

    // ── Accents & selection ──────────────────────────────────────────────────
    /// Primary accent (links, active render mode, selection tint).
    pub accent: Color32,
    /// Warm accent (e.g. an over-ridden / instanced tree root).
    pub accent_warm: Color32,
    /// Background behind selected text / the selected tab or row.
    pub selection_fill: Color32,
    /// Colour of selected text.
    pub selection_text: Color32,

    // ── Text emphasis tiers ──────────────────────────────────────────────────
    pub text_strong: Color32,
    pub text_normal: Color32,
    /// De-emphasised secondary text.
    pub text_dim: Color32,
    /// Muted / disabled text.
    pub text_muted: Color32,
    /// `filtered-out`: scene-tree prims excluded by the viewport purpose filter.
    pub filtered_out: Color32,

    // ── Status semantics ─────────────────────────────────────────────────────
    pub status_ok: Color32,
    pub status_warn: Color32,
    pub status_err: Color32,
    pub status_info: Color32,

    // ── Tab bar (folder-style tabs; see [`crate::tab_bar`]) ──────────────────
    /// Fill of the active tab — equal to [`content_bg`](Self::content_bg) so
    /// the tab visually joins the page beneath it.
    pub tab_active_fill: Color32,
    pub tab_inactive_fill: Color32,
    pub tab_hover_fill: Color32,
    pub tab_text_active: Color32,
    pub tab_text_inactive: Color32,
    /// Unsaved-changes dot.
    pub tab_dirty: Color32,
    /// Close (×) glyph at rest (brightens to [`tab_text_active`](Self::tab_text_active) on hover).
    pub tab_close_idle: Color32,

    // ── Viewport overlay (the floating toolbar pill) ─────────────────────────
    pub overlay_bg: Color32,
    pub overlay_border: Color32,

    // ── Geometry ─────────────────────────────────────────────────────────────
    /// Corner radius for widgets, cards and tab tops (`u8` in egui 0.35).
    pub corner_radius: u8,
    /// Height of a tab in the folder tab bar.
    pub tab_height: f32,
    /// Horizontal gap between tabs.
    pub tab_gap: f32,
    /// Horizontal text padding inside an active vs inactive tab.
    pub tab_pad_active_x: f32,
    pub tab_pad_inactive_x: f32,
    /// `(x, y)` button text padding → `Spacing::button_padding`.
    pub button_pad: (f32, f32),
    /// `(x, y)` spacing between adjacent widgets → `Spacing::item_spacing`.
    pub item_spacing: (f32, f32),
    /// Inner margin of a grouped card (`i8` → `Margin`).
    pub group_margin: i8,
    /// Menu / popup inner margin (`i8` → `Margin`).
    pub menu_margin: i8,
    /// Row-icon square size.
    pub icon_size: f32,
    /// Toolbar icon square size.
    pub toolbar_icon_size: f32,

    // ── Typography (point sizes for the egui `TextStyle` map) ────────────────
    pub font_heading: f32,
    pub font_body: f32,
    pub font_button: f32,
    pub font_mono: f32,
    pub font_small: f32,
    /// Custom faces to register; empty keeps egui's bundled fonts.
    pub custom_fonts: &'static [CustomFont],

    // ── Domain sub-palettes ──────────────────────────────────────────────────
    pub canvas: CanvasPalette,
}

impl Theme {
    /// The default dark theme — **the one place to edit the editor's look.**
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            window_bg: Color32::from_rgb(24, 27, 32),
            content_bg: Color32::from_rgb(32, 35, 41),
            tab_bar_bg: Color32::from_rgb(25, 28, 33),
            faint_bg: Color32::from_rgb(38, 42, 49),
            extreme_bg: Color32::from_rgb(18, 20, 24),

            group_fill: Color32::from_rgb(37, 41, 48),
            group_stroke: Color32::from_rgb(52, 57, 65),
            group_stroke_strong: Color32::from_rgb(72, 78, 88),

            button_bg: Color32::from_rgb(45, 50, 58),
            button_hover_bg: Color32::from_rgb(56, 62, 71),
            button_active_bg: Color32::from_rgb(64, 71, 82),

            accent: Color32::from_rgb(90, 167, 255),
            accent_warm: Color32::from_rgb(0xE8, 0x8A, 0x1A),
            selection_fill: Color32::from_rgba_premultiplied(52, 92, 150, 255),
            selection_text: Color32::from_rgb(238, 243, 250),

            text_strong: Color32::from_rgb(230, 234, 240),
            text_normal: Color32::from_rgb(201, 206, 214),
            text_dim: Color32::from_rgb(139, 139, 150),
            text_muted: Color32::from_rgb(138, 138, 150),
            filtered_out: Color32::from_rgb(72, 72, 72),

            status_ok: Color32::from_rgb(70, 190, 110),
            status_warn: Color32::from_rgb(224, 160, 48),
            status_err: Color32::from_rgb(230, 80, 80),
            status_info: Color32::from_rgb(96, 176, 224),

            tab_active_fill: Color32::from_rgb(32, 35, 41),
            tab_inactive_fill: Color32::from_rgb(29, 32, 38),
            tab_hover_fill: Color32::from_rgb(40, 44, 51),
            tab_text_active: Color32::from_rgb(236, 240, 245),
            tab_text_inactive: Color32::from_rgb(150, 156, 166),
            tab_dirty: Color32::from_rgb(230, 170, 80),
            tab_close_idle: Color32::from_rgb(120, 126, 136),

            overlay_bg: Color32::from_rgba_premultiplied(20, 24, 32, 210),
            overlay_border: Color32::from_rgba_premultiplied(80, 90, 110, 180),

            corner_radius: 5,
            tab_height: 26.0,
            tab_gap: 3.0,
            tab_pad_active_x: 14.0,
            tab_pad_inactive_x: 10.0,
            button_pad: (8.0, 4.0),
            item_spacing: (6.0, 4.0),
            group_margin: 8,
            menu_margin: 6,
            icon_size: 14.0,
            toolbar_icon_size: 16.0,

            font_heading: 16.0,
            font_body: 13.0,
            font_button: 13.0,
            font_mono: 12.5,
            font_small: 11.0,
            custom_fonts: &[],

            canvas: CanvasPalette {
                node_body_fill: Color32::from_rgb(0x2A, 0x2E, 0x36),
                node_outline: Color32::from_rgb(0x14, 0x16, 0x1A),
                value_box_fill: Color32::from_rgb(0x36, 0x3B, 0x46),
                value_box_text: Color32::from_rgb(0xDC, 0xDF, 0xE4),

                socket_string: Color32::from_rgb(0xE5, 0xC1, 0x00),
                socket_float: Color32::from_rgb(0x3F, 0x8F, 0xE5),
                socket_float3: Color32::from_rgb(0x3F, 0xAE, 0x5B),
                socket_path: Color32::from_rgb(0xD3, 0x4F, 0x4F),
                socket_other: Color32::from_rgb(0xB8, 0xB8, 0xB8),

                compute_datasource: [0x2E, 0x9E, 0x8F],
                compute_compute: [0x5B, 0x6E, 0xE0],
                compute_consumer: [0xCF, 0x8A, 0x3A],
                compute_other: [0x7A, 0x7A, 0x7A],
            },
        }
    }

    /// Inner margin of a grouped card, as an egui [`Margin`].
    #[must_use]
    pub fn group_margin(&self) -> Margin {
        Margin::same(self.group_margin)
    }

    /// A themed [`egui::Frame`] for a grouped content card — the "visually
    /// grouped sub-panel" look from the reference UI.
    pub fn group_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.group_fill)
            .stroke(Stroke::new(1.0, self.group_stroke))
            .corner_radius(CornerRadius::same(self.corner_radius))
            .inner_margin(self.group_margin())
    }

    /// Frame for the tab bar strip (darker than the page so active tabs lift).
    pub fn tab_bar_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.tab_bar_bg)
            .inner_margin(Margin {
                left: 4,
                right: 4,
                top: 4,
                bottom: 0,
            })
    }

    /// Frame for a tab's content panel: page fill with a top border in the
    /// active-tab colour, so content reads as the body of the selected tab.
    pub fn content_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.content_bg)
            .inner_margin(Margin::same(6))
    }

    /// Push the egui-owned half of this theme into [`egui::Style`] (both the
    /// dark and light style copies egui keeps) and register custom fonts.
    pub fn apply_style(&self, ctx: &egui::Context) {
        if !self.custom_fonts.is_empty() {
            let mut defs = egui::FontDefinitions::default();
            for cf in self.custom_fonts {
                defs.font_data.insert(
                    cf.name.to_owned(),
                    std::sync::Arc::new(egui::FontData::from_static(cf.bytes)),
                );
                let family = match cf.family {
                    FontSlot::Proportional => FontFamily::Proportional,
                    FontSlot::Monospace => FontFamily::Monospace,
                };
                defs.families
                    .entry(family)
                    .or_default()
                    .insert(0, cf.name.to_owned());
            }
            ctx.set_fonts(defs);
        }

        ctx.all_styles_mut(|s| self.write_style(s));
    }

    /// Write every egui-owned field. Split out so it can be unit-tested
    /// against a default [`egui::Style`] without a live context.
    fn write_style(&self, s: &mut egui::Style) {
        let cr = CornerRadius::same(self.corner_radius);
        let v = &mut s.visuals;

        v.dark_mode = true;
        v.panel_fill = self.content_bg;
        v.window_fill = self.window_bg;
        v.window_stroke = Stroke::new(1.0, self.group_stroke);
        v.window_corner_radius = cr;
        v.menu_corner_radius = cr;
        v.faint_bg_color = self.faint_bg;
        v.extreme_bg_color = self.extreme_bg;
        v.code_bg_color = self.extreme_bg;
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.status_warn;
        v.error_fg_color = self.status_err;
        v.override_text_color = None;

        v.selection.bg_fill = self.selection_fill;
        v.selection.stroke = Stroke::new(1.0, self.selection_text);

        let w = &mut v.widgets;
        // Non-interactive: labels, separators, group outlines.
        w.noninteractive.bg_fill = self.content_bg;
        w.noninteractive.weak_bg_fill = self.content_bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.group_stroke);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text_normal);
        w.noninteractive.corner_radius = cr;
        // Resting interactive widget (buttons use weak_bg_fill, not bg_fill).
        w.inactive.bg_fill = self.button_bg;
        w.inactive.weak_bg_fill = self.button_bg;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, self.text_normal);
        w.inactive.corner_radius = cr;
        // Hovered.
        w.hovered.bg_fill = self.button_hover_bg;
        w.hovered.weak_bg_fill = self.button_hover_bg;
        w.hovered.bg_stroke = Stroke::new(1.0, self.group_stroke_strong);
        w.hovered.fg_stroke = Stroke::new(1.0, self.text_strong);
        w.hovered.corner_radius = cr;
        // Pressed / active.
        w.active.bg_fill = self.button_active_bg;
        w.active.weak_bg_fill = self.button_active_bg;
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.fg_stroke = Stroke::new(1.0, self.text_strong);
        w.active.corner_radius = cr;
        // Open (e.g. an expanded combo box).
        w.open.bg_fill = self.button_bg;
        w.open.weak_bg_fill = self.button_bg;
        w.open.bg_stroke = Stroke::new(1.0, self.group_stroke);
        w.open.fg_stroke = Stroke::new(1.0, self.text_normal);
        w.open.corner_radius = cr;

        s.spacing.button_padding = egui::vec2(self.button_pad.0, self.button_pad.1);
        s.spacing.item_spacing = egui::vec2(self.item_spacing.0, self.item_spacing.1);
        s.spacing.menu_margin = Margin::same(self.menu_margin);

        s.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(self.font_heading, FontFamily::Proportional),
            ),
            (
                TextStyle::Body,
                FontId::new(self.font_body, FontFamily::Proportional),
            ),
            (
                TextStyle::Button,
                FontId::new(self.font_button, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(self.font_mono, FontFamily::Monospace),
            ),
            (
                TextStyle::Small,
                FontId::new(self.font_small, FontFamily::Proportional),
            ),
        ]
        .into_iter()
        .collect();
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Install `theme` as the process-global theme and push its egui-owned styling
/// onto `ctx`. Call once at startup, right after creating the [`egui::Context`].
///
/// A second call still re-applies the style to `ctx` (so a context created
/// later in a test picks it up), but the global instance returned by
/// [`current`] is fixed by the first call.
pub fn install(theme: Theme, ctx: &egui::Context) {
    let _ = THEME.set(theme);
    current().apply_style(ctx);
}

/// The active theme. Falls back to [`Theme::dark`] if [`install`] was never
/// called (e.g. in a unit test that paints without a shell).
///
/// The fallback lives in a *separate* static and never touches `THEME`, so a
/// `current()` call that happens before [`install`] cannot lock the global to
/// the dark default and silently discard a later installed theme.
#[must_use]
pub fn current() -> &'static Theme {
    static FALLBACK: OnceLock<Theme> = OnceLock::new();
    THEME
        .get()
        .unwrap_or_else(|| FALLBACK.get_or_init(Theme::dark))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_style_sets_panel_and_selection_from_tokens() {
        let th = Theme::dark();
        let mut style = egui::Style::default();
        th.write_style(&mut style);
        assert_eq!(style.visuals.panel_fill, th.content_bg);
        assert_eq!(style.visuals.selection.bg_fill, th.selection_fill);
        // Buttons draw from weak_bg_fill, so it must carry the button colour.
        assert_eq!(style.visuals.widgets.inactive.weak_bg_fill, th.button_bg);
    }

    #[test]
    fn write_style_populates_all_text_styles() {
        let th = Theme::dark();
        let mut style = egui::Style::default();
        th.write_style(&mut style);
        for ts in [
            TextStyle::Heading,
            TextStyle::Body,
            TextStyle::Button,
            TextStyle::Monospace,
            TextStyle::Small,
        ] {
            assert!(style.text_styles.contains_key(&ts), "missing {ts:?}");
        }
        assert!((style.text_styles[&TextStyle::Small].size - th.font_small).abs() < f32::EPSILON);
    }

    #[test]
    fn active_tab_fill_matches_content_so_tab_joins_page() {
        // The folder-tab illusion depends on the active tab sharing the page
        // colour; guard against the two drifting apart.
        let th = Theme::dark();
        assert_eq!(th.tab_active_fill, th.content_bg);
    }

    #[test]
    fn current_falls_back_to_dark_without_install() {
        // Does not call install(); current() must still yield a usable theme.
        let th = current();
        assert_eq!(th.corner_radius, Theme::dark().corner_radius);
    }
}
