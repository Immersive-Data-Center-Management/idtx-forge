//! Compile-time embedded SVG icon set used across the editor UI.
//!
//! Every icon is shipped as a small monochrome SVG under
//! `crates/idtx-editor-ui/assets/icons/` and pulled in via
//! [`egui::include_image!`], which embeds the file bytes at compile
//! time and produces a `'static` [`egui::ImageSource`]. At runtime
//! `egui_extras`' SVG loader (installed once by the shell via
//! `egui_extras::install_image_loaders`, alongside [`crate::theme::install`])
//! rasterises them to a
//! texture at the target render size, so icons stay crisp at any DPI
//! and never rely on the currently-loaded font atlas the way a
//! Unicode glyph does.
//!
//! ## Adding a new icon
//!
//! 1. Drop the SVG under `crates/idtx-editor-ui/assets/icons/`.
//! 2. Add a `pub const` here through the [`svg!`] helper.
//! 3. Use it via the [`icon_button`] / [`icon_image`] helpers or
//!    build an `egui::Image` / `egui::ImageButton` yourself.
//!
//! ## Tinting
//!
//! The SVGs use `stroke="currentColor"` where they visually should
//! adapt to the theme (eye, chevron, gift, …). Consumers pick the
//! tint through [`egui::Image::tint`] — the built-in helpers below
//! default to the current `Ui`'s `text_color()` so icons visually
//! match adjacent labels, and [`icon_button_tinted`] lets the caller
//! override with a palette accent (e.g. the active render mode).

use egui::{Button, Color32, Image, ImageSource, Response, Ui, Vec2};

/// Default on-screen size for the tree/inspector row icons — a
/// square that matches the tree row's expander glyph metrics
/// (see `scene_tree::draw_expander`) so the new SVG icons drop in
/// without changing row heights.
pub const DEFAULT_ICON_SIZE: f32 = 14.0;

/// Slightly larger square used by the floating viewport toolbar,
/// where icons sit alongside text labels on a taller pill.
pub const TOOLBAR_ICON_SIZE: f32 = 16.0;

macro_rules! svg {
    ($name:ident, $path:literal) => {
        /// Compile-time-embedded SVG icon (see module docs).
        pub const $name: ImageSource<'static> =
            egui::include_image!(concat!("../assets/icons/", $path));
    };
}

svg!(EYE_OPEN, "show.svg");
svg!(EYE_CLOSED, "hide.svg");
svg!(PACKED, "product.svg");
svg!(CODE, "code.svg");
svg!(TREE, "org-chart.svg");
svg!(CHEVRON_RIGHT, "positive.svg");
svg!(CHEVRON_DOWN, "negative.svg");
svg!(CLOSE, "clear-clipboard.svg");
svg!(GLOBE, "world.svg");
svg!(PIN, "pin-off.svg");
svg!(GRID, "grid.svg");
svg!(LIGHT_BULB, "lightbulb.svg");
svg!(SHADED, "shaded.svg");
svg!(WIREFRAME, "inactive.svg");
svg!(OK, "ok.svg");
svg!(GIZMO_LOCAL, "gizmo-local.svg");
svg!(GIZMO_WORLD, "gizmo-world.svg");

// Icons representing the different Prim-Types
svg!(CUBE, "cube.svg");
svg!(CONE, "cone.svg");
svg!(CYLINDER, "cylinder.svg");
svg!(CAPSULE, "capsule.svg");
svg!(SPHERE, "sphere.svg");
svg!(XFORM, "axis-gizmo.svg");
svg!(LIGHT, "lightbulb.svg");
svg!(ENVIRONMENT, "environment.svg");
svg!(MATERIAL, "material.svg");
svg!(SHADER, "shader.svg");
svg!(CAMERA, "camera.svg");
svg!(SCOPE, "scope.svg");
svg!(SKELETON, "skeleton.svg");
svg!(SKEL_ROOT, "skelroot.svg");

// Layer Stack panel icons
svg!(LAYERS, "layers.svg");
svg!(LAYER_SESSION, "layer-session.svg");
svg!(LAYER_ROOT, "layer-root.svg");
svg!(LAYER_SUBLAYER, "layer-sublayer.svg");
svg!(LAYER_REFERENCE, "layer-reference.svg");
svg!(LAYER_PAYLOAD, "layer-payload.svg");
svg!(EDIT_TARGET, "edit-target.svg");
svg!(RELOAD, "reload.svg");
svg!(MUTE, "mute.svg");
svg!(UNMUTE, "unmute.svg");
svg!(MOVE_UP, "move-up.svg");
svg!(MOVE_DOWN, "move-down.svg");

// Opinion / provenance inspector icons
svg!(PROVENANCE_WINNER, "provenance-up.svg");
svg!(SCHEMA_FALLBACK, "schema-fallback.svg");
svg!(REFERENCE_ARC, "reference-arc.svg");
svg!(PENCIL, "pencil.svg");

// Project level-registry — one glyph per stage kind, plus the add-level
// affordance in the level browser's footer.
svg!(STAGE_LEVEL, "stage-level.svg");
svg!(STAGE_ASSET, "stage-asset.svg");
svg!(STAGE_ASSEMBLY, "stage-assembly.svg");
svg!(STAGE_PLAYER, "stage-player.svg");
svg!(ADD, "add.svg");
svg!(TRASH, "trash.svg");

// Asset browser — one file-type glyph per `AssetKind`, plus the folder
// glyph for directories. `Material`/`Model`/etc. get document-style icons so a
// browser row reads as "a file of kind X" rather than the scene-tree prim glyph.
svg!(FOLDER, "folder.svg");
svg!(FILE, "file.svg");
svg!(FILE_USD, "file-usd.svg");
svg!(FILE_IMAGE, "file-image.svg");
svg!(FILE_MATERIAL, "file-material.svg");
svg!(FILE_MESH, "file-mesh.svg");
svg!(FILE_AUDIO, "file-audio.svg");
svg!(FILE_VIDEO, "file-video.svg");
svg!(FILE_DATA, "file-data.svg");

/// The eye icon appropriate for the current `visible` state.
#[must_use]
pub fn eye(visible: bool) -> ImageSource<'static> {
    if visible { EYE_OPEN } else { EYE_CLOSED }
}

/// The collapse-arrow appropriate for the current `open` state.
#[must_use]
pub fn chevron(open: bool) -> ImageSource<'static> {
    if open { CHEVRON_DOWN } else { CHEVRON_RIGHT }
}

/// Build an [`Image`] widget for the given SVG source, sized to a
/// square of `size` logical pixels and tinted to the current
/// UI-style's text color so icons blend in with adjacent labels.
///
/// Use this when the icon should render as **non-interactive art**
/// (e.g. inline in a label row). For clickable icons see
/// [`icon_button`] / [`icon_button_tinted`].
pub fn icon_image<'a>(ui: &Ui, src: ImageSource<'a>, size: f32) -> Image<'a> {
    Image::new(src)
        .fit_to_exact_size(Vec2::splat(size))
        .tint(ui.style().visuals.text_color())
}

/// Build an [`Image`] widget tinted with an explicit accent — used
/// for icons that should draw attention (active render-mode, warning
/// banner, …).
pub fn icon_image_tinted(src: ImageSource<'_>, size: f32, tint: Color32) -> Image<'_> {
    Image::new(src)
        .fit_to_exact_size(Vec2::splat(size))
        .tint(tint)
}

/// Draw a frameless icon button using the standard row-icon size.
///
/// The button uses the same "small + frameless" metrics the previous
/// Unicode glyph buttons used (eye toggle, packed-toggle) so the
/// tree row heights don't shift when the icon set is swapped in.
pub fn icon_button(ui: &mut Ui, src: ImageSource<'_>) -> Response {
    icon_button_sized(ui, src, DEFAULT_ICON_SIZE)
}

/// Draw a frameless icon button at an explicit square size.
///
/// egui 0.35 no longer exposes a standalone `ImageButton` at the crate
/// root — `Button::image(..)` is the canonical way to build one, and
/// carries the same `.frame(false)` toggle we relied on for the row
/// icons.
pub fn icon_button_sized(ui: &mut Ui, src: ImageSource<'_>, size: f32) -> Response {
    let tint = ui.style().visuals.text_color();
    let img = Image::new(src)
        .fit_to_exact_size(Vec2::splat(size))
        .tint(tint);
    ui.add(Button::image(img).frame(false))
}

/// Draw a frameless icon button tinted with an explicit accent.
pub fn icon_button_tinted(ui: &mut Ui, src: ImageSource<'_>, size: f32, tint: Color32) -> Response {
    let img = Image::new(src)
        .fit_to_exact_size(Vec2::splat(size))
        .tint(tint);
    ui.add(Button::image(img).frame(false))
}
