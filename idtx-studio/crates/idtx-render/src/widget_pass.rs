//! World-space widget-panel rendering. Each [`WidgetPanel`] is rasterized to a
//! texture (dirty-gated by
//! `content_hash`) and drawn as a depth-tested, alpha-blended quad **inside the
//! beauty pass**, mirroring how the skybox records onto the already-open pass
//! (the renderer-seam analysis: scene depth is discarded after the beauty pass
//! on desktop targets, and the MSAA resolve makes a sibling pass incorrect, so
//! the widget draw must share the beauty pass's color + depth attachments).
//!
//! The first [`WidgetRasterizer`] backend ([`SolidWidgetRasterizer`]) fills the
//! panel background and lays out each element, drawing real glyph text through a
//! configurable [`crate::font::FontContext`] (Roboto by default, selectable per
//! element via `idtx:widget:fontFamily`). This establishes the full
//! hydrate → rasterize → PrimId-keyed texture → world-space quad → readback
//! pipeline so a widget can be rendered and visually inspected today.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

use crate::font::{self, FontContext};
use crate::pipeline::HDR_FORMAT;
use crate::renderer::DEPTH_FORMAT;
use crate::scene_extract::WidgetPanel;
use crate::texture_cache::AssetReadFn;

use idtx_scene::{WidgetElement, WidgetElementKind, WidgetLayout};
#[cfg(test)]
use idtx_scene::WidgetBillboard;
pub use idtx_scene::widget_layout::{WidgetElementRect, widget_element_rects, widget_hit_test};
use idtx_scene::widget_layout::{ComputedWidgetElement, WidgetShape, compute_widget_layout, resolve_widget_quad as resolve_quad};

/// Abstracts the UI backend that turns a panel's element tree into an RGBA8
/// image. The first backend is a solid-fill CPU rasterizer with embedded-font
/// glyph text; a richer backend (styled containers, image assets) can follow.
pub trait WidgetRasterizer: Send + Sync {
    /// Rasterize `panel` into a tightly-packed RGBA8 buffer of
    /// `panel.resolution[0] * panel.resolution[1] * 4` bytes (row-major, top
    /// row first). Colours are linear (the GPU texture is `Rgba8Unorm`).
    fn rasterize(&self, panel: &WidgetPanel) -> Vec<u8>;

    /// Required for downcasting — return `self` as `&mut dyn std::any::Any`.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Solid-colour-fill rasterizer: panel background + a border + one colour bar
/// per element (sliders show a proportional fill, toggles an on/off pad), with
/// label/button/field text drawn in a real TrueType font. Deterministic and
/// headless — the self-inspection image.
#[allow(missing_debug_implementations)] // holds a FontContext (opaque font tables).
pub struct SolidWidgetRasterizer {
    fonts: Arc<FontContext>,
    /// USD AssetResolver-backed reader: `Some` only when the renderer was
    /// created with a reader (packed `.usdz` / remote-stage use).  When
    /// `None` — or when the reader returns `None` for a path — the image
    /// element falls back to the placeholder bar.  Direct filesystem access
    /// is never used; assets must come through this callback.
    reader: Option<AssetReadFn>,
}

impl SolidWidgetRasterizer {
    /// A rasterizer with the built-in default font registry (Roboto).
    #[must_use]
    pub fn new() -> Self {
        Self { fonts: Arc::new(FontContext::new()), reader: None }
    }

    /// A rasterizer backed by a shared, caller-supplied font registry — e.g. one
    /// pre-loaded with extra `idtx:widget:fontFamily` faces.
    #[must_use]
    pub fn with_fonts(fonts: Arc<FontContext>) -> Self {
        Self { fonts, reader: None }
    }

    /// Install a USD AssetResolver-backed reader so `IdtxWidgetImage` elements
    /// load their image through the resolver instead of falling back to a
    /// placeholder. Use the same `AssetReadFn` wired into the `TextureCache`.
    pub fn set_reader(&mut self, reader: AssetReadFn) {
        self.reader = Some(reader);
    }
}

impl Default for SolidWidgetRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl WidgetRasterizer for SolidWidgetRasterizer {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn rasterize(&self, panel: &WidgetPanel) -> Vec<u8> {
        let w = panel.resolution[0].max(1) as usize;
        let h = panel.resolution[1].max(1) as usize;
        let mut buf = vec![0u8; w * h * 4];

        // Panel background (dark, mostly opaque) + a lighter border.
        let background = linear_rgba(panel.surface.color, (panel.surface.opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
        fill_rect(&mut buf, w, h, 0, 0, w, h, background);
        if panel.surface.outline.is_none() && panel.surface.opacity > 0.0 {
            draw_border(&mut buf, w, h, 3, [90, 100, 130, (panel.surface.opacity / (235.0 / 255.0)).min(1.0).mul_add(255.0, 0.0) as u8]);
        }
        for y in 0..h { for x in 0..w {
            if !panel.surface.contains([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32]) {
                buf[(y * w + x) * 4..(y * w + x + 1) * 4].fill(0);
            }
        }}

        // Lay out the visual leaf elements into pixel rects (the single source
        // of layout truth, shared with the host-side hit-test so a click lands
        // on exactly the element drawn here).
        let layout = compute_widget_layout(panel.resolution, &panel.elements);
        let rects: Vec<_> = layout.iter().map(|e| e.label_box).collect();
        let horizontal = panel
            .elements
            .iter()
            .find(|e| e.kind != WidgetElementKind::Container)
            .is_some_and(|e| e.parent_layout == WidgetLayout::Row);

        // In a row of columns, every text column shares ONE font size (the
        // smallest that fits every column) so the readout reads as a balanced
        // row — label and field at the same size — instead of each column sizing
        // its text independently (which made a short value look larger than its
        // longer label). Vertical stacks keep per-slot sizing. An element that
        // authored an explicit `idtx:widget:fontSize` is excluded from the shared
        // fit so its size is honoured verbatim.
        let shared_px = if horizontal {
            rects
                .iter()
                .filter_map(|r| {
                    let e = &panel.elements[r.index];
                    (matches!(e.kind, WidgetElementKind::Label | WidgetElementKind::Field)
                        && !e.text.is_empty()
                        && e.font_size.is_none())
                    .then(|| {
                        let fnt = self.fonts.font_for(e.font_family.as_deref());
                        let base = auto_px(r.y1.saturating_sub(r.y0));
                        let avail = r.x1.saturating_sub(r.x0 + text_margin(base));
                        fit_px(fnt, &e.text, avail, base)
                    })
                })
                .fold(None, |acc: Option<f32>, px| {
                    Some(acc.map_or(px, |a| a.min(px)))
                })
        } else {
            None
        };

        let mut result = buf;
        let mut buf = vec![0u8; w * h * 4];
        for computed in &layout {
            let r = &computed.label_box;
            let el = &panel.elements[r.index];
            let (x0, y0, x1, y1) = (r.x0, r.y0, r.x1, r.y1);
            let col = linear_rgba(el.color, 255);
            let box_h = y1.saturating_sub(y0);
            let box_w = x1.saturating_sub(x0);
            let fnt = self.fonts.font_for(el.font_family.as_deref());
            match el.kind {
                WidgetElementKind::Label | WidgetElementKind::Field => {
                    // Label text / formatted field value, in the element colour on
                    // the panel background. An authored fontSize wins; otherwise a
                    // row uses the shared column size, and a stacked element shrinks
                    // its size so the string fits the slot width (no clipping).
                    let margin = text_margin(auto_px(box_h));
                    let tx = x0 + margin;
                    let avail = x1.saturating_sub(tx);
                    let px = resolve_px(el, h)
                        .map(|p| fit_px(fnt, &el.text, avail, p))
                        .or(shared_px)
                        .unwrap_or_else(|| fit_px(fnt, &el.text, avail, auto_px(box_h)));
                    draw_label(&mut buf, w, h, tx as f32, y0, box_h, &el.text, px, col, fnt);
                }
                WidgetElementKind::Button => {
                    // Accent pill + centred caption in near-white for contrast. A
                    // disabled button draws dimmed (it also never dispatches).
                    let pill = if el.enabled { col } else { dim_rgba(col) };
                    // The label box only positions text; the shape is the button.
                    for y in computed.clip.y0..computed.clip.y1.min(h) {
                        for x in computed.clip.x0..computed.clip.x1.min(w) {
                            if computed.contains([x as f32 + 0.5, y as f32 + 0.5]) {
                                buf[(y * w + x) * 4..(y * w + x + 1) * 4].copy_from_slice(&pill);
                            }
                        }
                    }
                    let avail = box_w.saturating_sub(text_margin(auto_px(box_h)) * 2);
                    let px = resolve_px(el, h)
                        .unwrap_or_else(|| auto_px(box_h));
                    let px = fit_px(fnt, &el.text, avail, px);
                    let tw = font::measure(fnt, &el.text, px);
                    let tx = x0 as f32 + (box_w as f32 - tw).max(0.0) * 0.5;
                    let caption = if el.enabled { [240, 245, 255, 255] } else { [170, 175, 185, 255] };
                    draw_label(&mut buf, w, h, tx, y0, box_h, &el.text, px, caption, fnt);
                }
                WidgetElementKind::Slider => {
                    // Track + proportional fill.
                    fill_rect(&mut buf, w, h, x0, y0, x1, y1, [50, 52, 62, 255]);
                    let frac = el.value.clamp(0.0, 1.0);
                    let fill_x1 = x0 + ((x1 - x0) as f32 * frac) as usize;
                    fill_rect(&mut buf, w, h, x0, y0, fill_x1, y1, col);
                }
                WidgetElementKind::Toggle => {
                    // Small on/off pad at the left of the slot.
                    let pad_w = (y1 - y0).min(x1 - x0);
                    let c = if el.value > 0.5 { col } else { [60, 62, 72, 255] };
                    fill_rect(&mut buf, w, h, x0, y0, x0 + pad_w, y1, c);
                }
                WidgetElementKind::Container => {
                    // Filtered out of the visual layout above; never reached.
                }
                WidgetElementKind::Image => {
                    // Attempt to load the authored image asset through the USD
                    // AssetResolver-backed reader (the same callback used by the
                    // TextureCache). Never uses direct filesystem access; falls
                    // back to a coloured placeholder bar when the reader is absent
                    // or returns nothing for the path.
                    let loaded = el.image_path.as_deref().and_then(|path| {
                        let bytes = (self.reader.as_deref()?)(path)?;
                        image::load_from_memory(&bytes).ok().map(|img| img.into_rgba8())
                    });
                    if let Some(img) = loaded {
                        let img_w = (x1 - x0) as u32;
                        let img_h = (y1 - y0) as u32;
                        if img_w > 0 && img_h > 0 {
                            let scaled = image::imageops::resize(
                                &img, img_w, img_h,
                                image::imageops::FilterType::Lanczos3,
                            );
                            for (iy, row) in scaled.rows().enumerate() {
                                let dst_y = y0 + iy;
                                if dst_y >= h {
                                    break;
                                }
                                for (ix, px) in row.enumerate() {
                                    let dst_x = x0 + ix;
                                    if dst_x >= w {
                                        continue;
                                    }
                                    let i = (dst_y * w + dst_x) * 4;
                                    #[allow(clippy::cast_precision_loss)]
                                    let a = px[3] as f32 / 255.0;
                                    let ia = 1.0 - a;
                                    buf[i] = (px[0] as f32 * a + buf[i] as f32 * ia) as u8;
                                    buf[i + 1] = (px[1] as f32 * a + buf[i + 1] as f32 * ia) as u8;
                                    buf[i + 2] = (px[2] as f32 * a + buf[i + 2] as f32 * ia) as u8;
                                    buf[i + 3] = (a * 255.0 + buf[i + 3] as f32 * ia) as u8;
                                }
                            }
                        }
                    } else {
                        // Placeholder: colour bar in the authored `idtx:widget:color`.
                        fill_rect(&mut buf, w, h, x0, y0, x1, y1, col);
                    }
                }
            }
            // Composite only covered samples; text cannot leak into sector gaps,
            // dead centers, another slot, or outside an authored polygon outline.
            for y in computed.clip.y0..computed.clip.y1.min(h) {
                for x in computed.clip.x0..computed.clip.x1.min(w) {
                    let i = (y * w + x) * 4;
                    if computed.contains([x as f32 + 0.5, y as f32 + 0.5])
                        && panel.surface.contains([(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32]) {
                        let a = buf[i + 3] as f32 / 255.0 * el.opacity.clamp(0.0, 1.0);
                        let old_a = result[i + 3] as f32 / 255.0;
                        let out_a = a + old_a * (1.0 - a);
                        if out_a > 0.0 {
                            for c in 0..3 {
                                result[i + c] = ((buf[i + c] as f32 * a + result[i + c] as f32 * old_a * (1.0 - a)) / out_a).round() as u8;
                            }
                            result[i + 3] = (out_a * 255.0).round() as u8;
                        }
                    }
                }
            }
            // Glyph overhangs may extend outside the clip; clear the scratch
            // image so no pixels from one element can enter a later element.
            buf.fill(0);
        }
        result
    }
}

/// Auto text pixel size for a slot `box_h` pixels tall: ~60% of the slot so
/// the cap height reads clearly while leaving vertical breathing room. Keeps
/// text proportional to the panel regardless of resolution.
#[allow(clippy::cast_precision_loss)]
fn auto_px(box_h: usize) -> f32 {
    (box_h as f32 * 0.6).max(7.0)
}

/// Resolve an authored `idtx:widget:fontSize` (pixels in the panel's
/// rasterization space) to a pixel size, clamped to the panel height. `None`
/// when the element left the size unset — the caller then uses the shared/auto
/// size.
#[allow(clippy::cast_precision_loss)]
fn resolve_px(el: &WidgetElement, panel_h: usize) -> Option<f32> {
    el.font_size
        .filter(|f| *f > 0.0)
        .map(|px| px.clamp(7.0, panel_h as f32))
}

/// Left/right text inset proportional to the font size (so larger text gets a
/// larger margin), with a small floor.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn text_margin(px: f32) -> usize {
    (px * 0.2).round().max(2.0) as usize
}

/// Largest pixel size ≤ `base` (and ≥ 7) at which `text` fits within `avail`
/// pixels in `font` — so a label/field/button caption never clips.
fn fit_px(font: &ab_glyph::FontArc, text: &str, avail: usize, base: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let avail = avail as f32;
    let mut px = base;
    while px > 7.0 && font::measure(font, text, px) > avail {
        px -= 1.0;
    }
    px
}

/// Draw `text` in `font` at pixel size `px`, left-origin `tx`, vertically
/// centred in the `[y0, y0 + box_h)` slot using the font's baseline metrics.
#[allow(clippy::cast_precision_loss)]
fn draw_label(
    buf: &mut [u8],
    w: usize,
    h: usize,
    tx: f32,
    y0: usize,
    box_h: usize,
    text: &str,
    px: f32,
    color: [u8; 4],
    font: &ab_glyph::FontArc,
) {
    let (ascent, descent) = font::v_metrics(font, px);
    // Centre the visual text height (ascent - descent) in the slot; the baseline
    // sits `ascent` below the text top. descent is negative.
    let baseline = y0 as f32 + (box_h as f32 + ascent + descent) * 0.5;
    font::draw_text(buf, w, h, tx, baseline, text, px, color, font);
}
/// is `Rgba8Unorm`, sampled as linear).
fn linear_rgba(c: [f32; 3], a: u8) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        a,
    ]
}

/// Dim an RGBA colour toward grey for a disabled control (keeps alpha).
fn dim_rgba(c: [u8; 4]) -> [u8; 4] {
    [c[0] / 2 + 30, c[1] / 2 + 30, c[2] / 2 + 30, c[3]]
}

fn fill_rect(buf: &mut [u8], w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 4]) {
    let x1 = x1.min(w);
    let y1 = y1.min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * 4;
            buf[i..i + 4].copy_from_slice(&c);
        }
    }
}

fn draw_border(buf: &mut [u8], w: usize, h: usize, t: usize, c: [u8; 4]) {
    fill_rect(buf, w, h, 0, 0, w, t, c);
    fill_rect(buf, w, h, 0, h.saturating_sub(t), w, h, c);
    fill_rect(buf, w, h, 0, 0, t, h, c);
    fill_rect(buf, w, h, w.saturating_sub(t), 0, w, h, c);
}

/// Camera uniform (group 0) — just the view-projection for the quad vertex shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WidgetCamUniform {
    view_proj: [[f32; 4]; 4],
}

/// Per-panel uniform (group 1): the quad's world-space centre + half-extent
/// edge vectors (already billboard-resolved on the CPU) + opacity.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WidgetPanelUniform {
    center: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    params: [f32; 4], // opacity, _, _, _
    focus_bounds: [f32; 4],
    focus_sector: [f32; 4],
    focus_angles: [f32; 4],
}

/// A per-panel GPU texture, keyed by prim path and gated by `content_hash`.
#[allow(missing_debug_implementations)]
struct CachedPanel {
    hash: u64,
    view: wgpu::TextureView,
    layout: Vec<ComputedWidgetElement>,
}

/// One prepared draw for this frame: the group-1 bind group (holds the panel
/// uniform + its texture + sampler alive) plus which culling pipeline to use.
#[allow(missing_debug_implementations)]
pub struct WidgetDraw {
    bind_group: wgpu::BindGroup,
    /// `true` → no-cull pipeline (both faces); `false` → back-face-cull pipeline.
    double_sided: bool,
}

/// Everything prepared for one frame's widget draws: the shared camera bind
/// group (group 0) + the per-panel draws. Built by [`WidgetRenderer::prepare`]
/// (outside the beauty pass) and replayed by [`WidgetRenderer::draw`] (inside it).
///
/// `cam_bg` is `None` on the no-widget fast path — [`WidgetRenderer::prepare`]
/// returns before allocating any camera buffer/bind group when the scene has no
/// panels, so a widget-free scene adds zero per-frame GPU allocation.
#[allow(missing_debug_implementations)]
pub struct WidgetFrame {
    cam_bg: Option<wgpu::BindGroup>,
    draws: Vec<WidgetDraw>,
}

impl WidgetFrame {
    /// Whether this frame has any widget draws (lets the caller skip binding).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}

/// Text-readability quality knobs for the widget rasterizer, resolved once from
/// the environment at [`WidgetRenderer::new`]. The **default is SSAA 2× +
/// mipmaps** — the validated readability winner from
/// `docs/widget-text-readability.md` on the VR (2× MSAA, 0.6 render-scale) path.
/// Each knob can be overridden by an env var (e.g. for A/B or to dial back cost).
///
/// * `res_scale` (`IDTX_WIDGET_RES_SCALE`, default 1) — **#1**: multiply the
///   panel's authored rasterization resolution before rasterizing. More source
///   texels for the same quad; helps when the panel is magnified beyond its
///   texel density, but raises minification aliasing at distance unless mips are
///   on. Left at 1 by default (SSAA is the chosen sharpening lever).
/// * `ssaa` (`IDTX_WIDGET_SSAA`, **default 2**) — **#3**: rasterize at
///   `resolution × ssaa`, then box-downsample back to `resolution` before
///   upload. Supersampled (anti-aliased) glyph edges at the target resolution.
///   Set `IDTX_WIDGET_SSAA=1` to disable.
/// * `mips` (`IDTX_WIDGET_MIPS`, **default on**) — **#4**: upload a full mip
///   chain and sample trilinear + anisotropic, so a panel minified at distance or
///   seen at a grazing angle does not alias/undersample (the failure mode that a
///   bare resolution bump makes *worse*). Set `IDTX_WIDGET_MIPS=0` to disable.
#[derive(Clone, Copy, Debug)]
struct WidgetQuality {
    res_scale: u32,
    ssaa: u32,
    mips: bool,
}

impl WidgetQuality {
    /// Largest per-axis texture dimension after `res_scale × ssaa` — bounds one
    /// panel's intermediate buffer at `8192 × 8192 × 4` = 256 MiB.
    const MAX_AXIS: u32 = 8192;

    fn from_env() -> Self {
        let read_u = |key: &str, default: u32| -> u32 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(default)
                .clamp(1, 4)
        };
        // Boolean env override: absent → `default`; present → off only for an
        // explicit falsey value, otherwise on.
        let read_b = |key: &str, default: bool| -> bool {
            match std::env::var(key) {
                Ok(v) => !matches!(v.as_str(), "0" | "false" | "off" | "no" | ""),
                Err(_) => default,
            }
        };
        let q = Self {
            res_scale: read_u("IDTX_WIDGET_RES_SCALE", 1),
            ssaa: read_u("IDTX_WIDGET_SSAA", 2),
            mips: read_b("IDTX_WIDGET_MIPS", true),
        };
        println!(
            "idtx-render: widget quality res_scale={}× ssaa={}× mips={}",
            q.res_scale, q.ssaa, q.mips
        );
        q
    }
}

/// Scale every element's authored `idtx:widget:fontSize` by the ratio of the
/// (scaled) rasterization height `hi.resolution[1]` to the `authored_h` the font
/// sizes were chosen against, so apparent text size is preserved when a
/// readability lever raises the raster resolution. No-op at factor 1.
#[allow(clippy::cast_precision_loss)]
fn scale_authored_fonts(hi: &mut WidgetPanel, authored_h: u32) {
    let factor = hi.resolution[1] as f32 / authored_h.max(1) as f32;
    if (factor - 1.0).abs() <= f32::EPSILON {
        return;
    }
    for el in &mut hi.elements {
        if let Some(fs) = el.font_size.as_mut() {
            *fs *= factor;
        }
    }
}

/// Box-downsample a tightly-packed RGBA8 image from `(w, h)` by integer `factor`
/// on each axis, averaging each `factor × factor` block (straight-alpha average).
/// Returns the `(w/factor, h/factor)` image. `factor == 1` is the identity.
#[allow(clippy::cast_possible_truncation)]
fn box_downsample(src: &[u8], w: usize, h: usize, factor: usize) -> Vec<u8> {
    if factor <= 1 {
        return src.to_vec();
    }
    let (dw, dh) = (w / factor, h / factor);
    let mut out = vec![0u8; dw * dh * 4];
    let n = (factor * factor) as u32;
    for dy in 0..dh {
        for dx in 0..dw {
            let mut acc = [0u32; 4];
            for sy in 0..factor {
                let row = (dy * factor + sy) * w;
                for sx in 0..factor {
                    let i = (row + dx * factor + sx) * 4;
                    acc[0] += u32::from(src[i]);
                    acc[1] += u32::from(src[i + 1]);
                    acc[2] += u32::from(src[i + 2]);
                    acc[3] += u32::from(src[i + 3]);
                }
            }
            let o = (dy * dw + dx) * 4;
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
            out[o + 3] = (acc[3] / n) as u8;
        }
    }
    out
}

/// Halve a tightly-packed RGBA8 image to `((w / 2).max(1), (h / 2).max(1))`,
/// averaging each 2×2 block with edge clamping so arbitrary (odd / non-power-of-
/// two) dimensions are supported. Used to build a texture's mip chain on the CPU.
/// Floor halving (not `div_ceil`) matches wgpu's per-level mip dimensions
/// (`max(1, base >> level)`); a ceil-sized level would desync the concatenated
/// `MipMajor` upload's offsets for non-power-of-two bases (e.g. 680×1140).
#[allow(clippy::cast_possible_truncation)]
fn downsample_half(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (dw, dh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; dw * dh * 4];
    let sample = |x: usize, y: usize| -> [u32; 4] {
        let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        [
            u32::from(src[i]),
            u32::from(src[i + 1]),
            u32::from(src[i + 2]),
            u32::from(src[i + 3]),
        ]
    };
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = (dx * 2, dy * 2);
            let mut acc = [0u32; 4];
            for p in [sample(sx, sy), sample(sx + 1, sy), sample(sx, sy + 1), sample(sx + 1, sy + 1)] {
                for c in 0..4 {
                    acc[c] += p[c];
                }
            }
            let o = (dy * dw + dx) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / 4) as u8;
            }
        }
    }
    (out, dw, dh)
}

/// Build a complete mip chain for a `(w, h)` RGBA8 image: returns the levels
/// concatenated (level 0 first, `TextureDataOrder::MipMajor` layout) and the
/// level count, halving down to 1×1. Successive-halving box filter — adequate
/// for text panels and cheap (dirty-gated, built only on content change).
fn build_mip_chain(level0: Vec<u8>, w: usize, h: usize) -> (Vec<u8>, u32) {
    // wgpu caps the level count at floor(log2(max_dim)) + 1; floor halving of a
    // non-power-of-two dimension reaches 1×1 in exactly that many levels.
    let max_levels = (usize::BITS - w.max(h).max(1).leading_zeros()).max(1);
    let mut data = level0.clone();
    let (mut cur, mut cw, mut ch) = (level0, w, h);
    let mut levels = 1u32;
    while (cw > 1 || ch > 1) && levels < max_levels {
        let (next, nw, nh) = downsample_half(&cur, cw, ch);
        data.extend_from_slice(&next);
        cur = next;
        cw = nw;
        ch = nh;
        levels += 1;
    }
    (data, levels)
}

/// Renders world-space widget panels as depth-tested textured quads inside the
/// beauty pass. Owns the pipeline, a sampler, and a per-panel texture cache so
/// an unchanged panel is never re-rasterized.
#[allow(missing_debug_implementations)]
pub struct WidgetRenderer {
    sampler: wgpu::Sampler,
    cam_bgl: wgpu::BindGroupLayout,
    panel_bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    /// Back-face-culling pipeline for single-sided (`double_sided = false`) panels.
    pipeline_single: wgpu::RenderPipeline,
    rasterizer: Box<dyn WidgetRasterizer>,
    cache: HashMap<String, CachedPanel>,
    quality: WidgetQuality,
}

impl WidgetRenderer {
    /// Build the widget quad pipeline at the renderer's `sample_count` (the
    /// beauty pass is MSAA), targeting the HDR colour + scene depth so panels
    /// occlude and are occluded by scene geometry.
    #[must_use]
    pub fn new(device: &wgpu::Device, sample_count: u32) -> Self {
        let quality = WidgetQuality::from_env();
        // #4: trilinear + anisotropic sampling when a mip chain is uploaded;
        // otherwise the original bilinear single-level sampler (unchanged).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Widget Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: if quality.mips {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            anisotropy_clamp: if quality.mips { 16 } else { 1 },
            ..Default::default()
        });

        let cam_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Widget Cam BGL"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let panel_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Widget Panel BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Widget Shader"),
            source: wgpu::ShaderSource::Wgsl(WIDGET_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Widget Layout"),
            bind_group_layouts: &[Some(&cam_bgl), Some(&panel_bgl)],
            immediate_size: 0,
        });
        // Two pipelines identical but for face culling: panels authored
        // `idtx:widget:doubleSided = true` render both faces (cull disabled);
        // single-sided panels cull the back face so they vanish when viewed from
        // behind. Selected per draw from `panel.double_sided`.
        let make_pipeline = |cull_mode: Option<wgpu::Face>, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false), // alpha-blended; test only, like the skybox
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline(None, "Widget Pipeline (double-sided)");
        let pipeline_single = make_pipeline(Some(wgpu::Face::Back), "Widget Pipeline (single-sided)");

        Self {
            sampler,
            cam_bgl,
            panel_bgl,
            pipeline,
            pipeline_single,
            rasterizer: Box::new(SolidWidgetRasterizer::new()),
            cache: HashMap::new(),
            quality,
        }
    }

    /// Wire a USD AssetResolver-backed reader into the [`SolidWidgetRasterizer`]
    /// so `IdtxWidgetImage` elements load their image through the resolver.
    /// Call this once after [`Self::new`] whenever the renderer is created with
    /// an asset reader (e.g. for packed `.usdz` / remote-stage use). Clears the
    /// panel texture cache so any already-rasterised placeholder is re-rendered
    /// with the real image on the next frame.
    pub fn set_reader(&mut self, reader: AssetReadFn) {
        // Downcast to the concrete type to install the reader. The rasterizer is
        // always a `SolidWidgetRasterizer` when created through `Self::new`; if a
        // custom rasterizer was installed via `with_rasterizer` this is a no-op.
        if let Some(solid) = self
            .rasterizer
            .as_any_mut()
            .downcast_mut::<SolidWidgetRasterizer>()
        {
            solid.set_reader(reader);
            // Force re-rasterisation so the real image replaces any cached placeholder.
            self.cache.clear();
        }
    }

    /// Prepare this frame's widget draws OUTSIDE the beauty pass: rasterize +
    /// upload any changed panels (dirty-gated by `content_hash`), then build the
    /// camera + per-panel bind groups. `cam_right`/`cam_up` are the camera's
    /// world-space basis vectors (for billboard modes). Returns an empty frame
    /// when there are no panels.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panels: &[WidgetPanel],
        view_proj: Mat4,
        cam_right: Vec3,
        cam_up: Vec3,
    ) -> WidgetFrame {
        // No-widget fast path: return before allocating ANY camera resources. The
        // common scene has no panels, and the documented contract is zero per-frame
        // GPU cost there — creating a camera buffer + bind group only to discover
        // `panels` is empty would churn an allocation every frame on every scene.
        if panels.is_empty() {
            // Drop any cached panels left over from a stage that previously had
            // widgets (bounded memory), then return the empty frame.
            self.cache.clear();
            return WidgetFrame { cam_bg: None, draws: Vec::new() };
        }

        let cam_u = WidgetCamUniform {
            view_proj: view_proj.to_cols_array_2d(),
        };
        let cam_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Widget Cam Uniform"),
            contents: bytemuck::bytes_of(&cam_u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let cam_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Widget Cam BG"),
            layout: &self.cam_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buf.as_entire_binding(),
            }],
        });

        // Drop cache entries for panels no longer present (bounded memory).
        let live: std::collections::HashSet<&str> =
            panels.iter().map(|p| p.prim_path.as_str()).collect();
        self.cache.retain(|k, _| live.contains(k.as_str()));

        let mut draws = Vec::with_capacity(panels.len());
        for panel in panels {
            // HUD-space panels are screen-space overlays; the compositing path for
            // them is not implemented. Rendering them here would depth-test and
            // transform them as world geometry (wrong semantics), so skip anything
            // that is not a world-space quad.
            if panel.space != idtx_scene::WidgetSpace::World {
                continue;
            }
            if panel.opacity <= 0.0 || panel.size[0] <= 0.0 || panel.size[1] <= 0.0 {
                continue;
            }

            // Rasterize + upload only when the content changed (dirty gate).
            // (`quality` is fixed per renderer instance, so it need not enter the
            // dirty key — a given cache only ever holds one quality setting.)
            let needs = self
                .cache
                .get(&panel.prim_path)
                .is_none_or(|c| c.hash != panel.content_hash);
            if needs {
                // #1 res_scale: upload resolution = authored × res_scale.
                // #3 ssaa: rasterize at upload resolution × ssaa, then box-
                // downsample to the upload resolution. Both clamp per-axis.
                let q = self.quality;
                let up_w = (panel.resolution[0].max(1) * q.res_scale).min(WidgetQuality::MAX_AXIS);
                let up_h = (panel.resolution[1].max(1) * q.res_scale).min(WidgetQuality::MAX_AXIS);
                // Pick one supersample factor that fits BOTH axes under `MAX_AXIS`.
                // Clamping each axis independently could yield `fx != fy` (e.g.
                // 4096×100 with res_scale 2), which desyncs the box-downsample stride
                // and produces data whose height no longer matches the texture.
                let factor = q
                    .ssaa
                    .min(WidgetQuality::MAX_AXIS / up_w)
                    .min(WidgetQuality::MAX_AXIS / up_h)
                    .max(1);
                let hi_w = up_w * factor;
                let hi_h = up_h * factor;
                let (fx, fy) = (factor, factor);

                let rgba = if up_w == panel.resolution[0].max(1)
                    && up_h == panel.resolution[1].max(1)
                    && fx == 1
                    && fy == 1
                {
                    // Fast path: no quality scaling, rasterize at authored size.
                    self.rasterizer.rasterize(panel)
                } else {
                    let mut hi = panel.clone();
                    hi.resolution = [hi_w, hi_h];
                    // `idtx:widget:fontSize` is absolute pixels in rasterization
                    // space, but positions/`auto_px` are fraction/slot-relative and
                    // already scale with resolution. So raising the raster height
                    // without scaling authored font sizes would shrink only the
                    // explicitly-sized text. Scale each authored fontSize by the
                    // same height factor so apparent text size is preserved at any
                    // res_scale/ssaa (see docs/widget-text-readability.md).
                    scale_authored_fonts(&mut hi, panel.resolution[1]);
                    let raster = self.rasterizer.rasterize(&hi);
                    // Only square (fx == fy) integer downsample is wired; the env
                    // knob guarantees that, so assert rather than branch per axis.
                    debug_assert_eq!(fx, fy, "non-uniform widget SSAA factor");
                    box_downsample(&raster, hi_w as usize, hi_h as usize, fx as usize)
                };
                // #4: build a CPU mip chain (level 0 = the `up_w × up_h` image)
                // when enabled; otherwise a single level, as before.
                let (tex_data, mip_levels) = if q.mips {
                    build_mip_chain(rgba, up_w as usize, up_h as usize)
                } else {
                    (rgba, 1)
                };
                let texture = device.create_texture_with_data(
                    queue,
                    &wgpu::TextureDescriptor {
                        label: Some("Widget Panel Texture"),
                        size: wgpu::Extent3d {
                            width: up_w,
                            height: up_h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: mip_levels,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::MipMajor,
                    &tex_data,
                );
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.cache
                    .insert(panel.prim_path.clone(), CachedPanel { hash: panel.content_hash, view,
                        layout: compute_widget_layout(panel.resolution, &panel.elements) });
            }
            let view = &self.cache.get(&panel.prim_path).expect("just inserted").view;

            // Billboard-resolve the quad's world-space edge vectors on the CPU.
            let (center, right, up) =
                resolve_quad(panel.world_matrix, panel.size, panel.billboard, cam_right, cam_up);
            let focused = panel.highlight.as_deref().and_then(|path|
                self.cache[&panel.prim_path].layout.iter().find(|r| panel.elements[r.index].path == path && panel.elements[r.index].enabled));
            let (focus_bounds, focus_sector, focus_angles) = focused.map_or(([0.0; 4], [0.0; 4], [0.0; 4]), |r| {
                let bounds = [r.clip.x0 as f32, r.clip.y0 as f32, r.clip.x1 as f32, r.clip.y1 as f32];
                match r.shape {
                    WidgetShape::Rect(_) => (bounds, [0.0; 4], [0.0, 0.0, 1.0, 0.0]),
                    WidgetShape::AnnularSector { center, inner, outer, start, sweep, gap_half_width } =>
                        (bounds, [center[0], center[1], inner, outer], [start.to_radians(), sweep.to_radians(), 2.0, gap_half_width]),
                }
            });
            let u = WidgetPanelUniform {
                center: [center.x, center.y, center.z, 0.0],
                right: [right.x, right.y, right.z, 0.0],
                up: [up.x, up.y, up.z, 0.0],
                params: [panel.opacity.clamp(0.0, 1.0), panel.resolution[0] as f32, panel.resolution[1] as f32, 0.0],
                focus_bounds, focus_sector, focus_angles,
            };
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Widget Panel Uniform"),
                contents: bytemuck::bytes_of(&u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Widget Panel BG"),
                layout: &self.panel_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubuf.as_entire_binding(),
                    },
                ],
            });
            draws.push(WidgetDraw { bind_group, double_sided: panel.double_sided });
        }

        WidgetFrame { cam_bg: Some(cam_bg), draws }
    }

    /// Record the prepared widget draws onto the ALREADY-OPEN beauty render pass
    /// (mirrors the skybox block). Must be called inside the beauty pass so the
    /// quads share its HDR colour + scene depth attachments.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, frame: &'a WidgetFrame) {
        // Nothing to record, or the no-widget fast path (no camera bind group was
        // allocated). `draws` non-empty always pairs with `cam_bg = Some`.
        let Some(cam_bg) = &frame.cam_bg else {
            return;
        };
        if frame.draws.is_empty() {
            return;
        }
        pass.set_bind_group(0, cam_bg, &[]);
        for d in &frame.draws {
            // Single-sided panels cull their back face; double-sided panels draw both.
            pass.set_pipeline(if d.double_sided {
                &self.pipeline
            } else {
                &self.pipeline_single
            });
            pass.set_bind_group(1, &d.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

/// Compute the quad's world-space `(center, right_half, up_half)` from the
/// panel transform, physical size, and billboard mode. `right`/`up` are already
/// scaled to the half-extent so the vertex shader just does
/// `center ± right ± up`.
/// Positions the quad in world space from the per-panel edge vectors, projects
/// it with the camera `view_proj`, and samples the panel texture (alpha scaled
/// by opacity). Output is linear; it composites into the HDR beauty target and
/// is tonemapped with the rest of the frame.
const WIDGET_WGSL: &str = r"
struct Cam { view_proj: mat4x4<f32> };
struct Panel {
    center: vec4<f32>,
    right: vec4<f32>,
    up: vec4<f32>,
    params: vec4<f32>, // opacity, _, _, _
    focus_bounds: vec4<f32>,
    focus_sector: vec4<f32>,
    focus_angles: vec4<f32>,
};
@group(0) @binding(0) var<uniform> cam: Cam;
@group(1) @binding(0) var samp: sampler;
@group(1) @binding(1) var tex: texture_2d<f32>;
@group(1) @binding(2) var<uniform> p: Panel;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
    );
    let c = corners[vi];
    let world = p.center.xyz + c.x * p.right.xyz + c.y * p.up.xyz;
    var out: VsOut;
    out.pos = cam.view_proj * vec4<f32>(world, 1.0);
    // corner [-1,1] → uv [0,1]; flip V so the image top maps to panel top.
    out.uv = vec2<f32>((c.x + 1.0) * 0.5, (1.0 - c.y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(tex, samp, in.uv);
    let pixel = floor(in.uv * p.params.yz) + vec2<f32>(0.5);
    var focus = p.focus_angles.z > 0.0 && all(pixel >= p.focus_bounds.xy) && all(pixel < p.focus_bounds.zw);
    if focus && p.focus_angles.z == 2.0 {
        let d = pixel - p.focus_sector.xy;
        let r = length(d);
        let inset = asin(clamp(p.focus_angles.w / max(r, 0.000001), 0.0, 1.0));
        let angle = atan2(d.x, -d.y) - (p.focus_angles.x + inset);
        let turn = 6.28318530718;
        let wrapped = angle - floor(angle / turn) * turn;
        focus = r > 0.0 && r >= p.focus_sector.z && r < p.focus_sector.w
            && r >= p.focus_angles.w && wrapped < p.focus_angles.y - 2.0 * inset;
    }
    let color = select(s.rgb, mix(s.rgb, vec3<f32>(1.0), 0.28), focus);
    return vec4<f32>(color, s.a * p.params.x);
}
";

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_scene::WidgetElement;

    fn panel(elements: Vec<WidgetElement>) -> WidgetPanel {
        WidgetPanel {
            highlight: None,
            surface: Default::default(),
            prim_path: "/Panel".to_owned(),
            world_matrix: Mat4::IDENTITY,
            size: [0.4, 0.3],
            resolution: [64, 48],
            billboard: WidgetBillboard::None,
            double_sided: true,
            opacity: 1.0,
            space: idtx_scene::WidgetSpace::World,
            elements,
            content_hash: 7,
        }
    }

    fn leaf(path: &str, kind: WidgetElementKind, position: Option<[f32; 2]>) -> WidgetElement {
        WidgetElement {
            kind,
            path: path.to_owned(),
            depth: 0,
            parent_layout: WidgetLayout::Column,
            text: path.to_owned(),
            color: [1.0, 1.0, 1.0],
            value: 0.0,
            position,
            order: 0,
            font_family: None,
            font_size: None,
            enabled: true,
            image_path: None,
            ..Default::default()
        }
    }

    #[test]
    fn hit_test_maps_uv_to_stacked_element() {
        // Two vertically stacked buttons; the top slot UV hits index 0, the
        // bottom slot hits index 1, and the shared layout returns paths that the
        // host addresses handlers by.
        let els = vec![
            leaf("Top", WidgetElementKind::Button, None),
            leaf("Bottom", WidgetElementKind::Button, None),
        ];
        let res = [64, 48];
        // Centre of the upper half / lower half (inside the padded inner rect).
        assert_eq!(widget_hit_test(res, &els, 0.5, 0.30), Some(0));
        assert_eq!(widget_hit_test(res, &els, 0.5, 0.70), Some(1));
        // Outside the normalized panel is always a miss.
        assert_eq!(widget_hit_test(res, &els, 1.5, 0.5), None);
    }

    #[test]
    fn hit_test_row_columns_resolve_left_vs_right() {
        // A `row` container lays children as columns: left half → col 0,
        // right half → col 1.
        let mut a = leaf("Left", WidgetElementKind::Button, None);
        let mut b = leaf("Right", WidgetElementKind::Button, None);
        a.parent_layout = WidgetLayout::Row;
        b.parent_layout = WidgetLayout::Row;
        let els = vec![a, b];
        let res = [64, 48];
        assert_eq!(widget_hit_test(res, &els, 0.18, 0.5), Some(0));
        assert_eq!(widget_hit_test(res, &els, 0.78, 0.5), Some(1));
    }

    #[test]
    fn hit_test_ignores_containers_and_picks_topmost() {
        // A container is never a hit target; of two overlapping positioned
        // elements the later one (painted on top) wins.
        let els = vec![
            leaf("Group", WidgetElementKind::Container, None),
            leaf("Under", WidgetElementKind::Button, Some([0.2, 0.2])),
            leaf("Over", WidgetElementKind::Button, Some([0.2, 0.2])),
        ];
        let res = [128, 128];
        // Index 2 ("Over") is the top-most at the shared coordinate.
        assert_eq!(widget_hit_test(res, &els, 0.25, 0.25), Some(2));
    }

    #[test]
    fn solid_rasterizer_fills_buffer() {
        let r = SolidWidgetRasterizer::new();
        let el = WidgetElement {
            kind: WidgetElementKind::Label,
            path: "Label".to_owned(),
            depth: 0,
            parent_layout: WidgetLayout::Column,
            text: "Hi".to_owned(),
            color: [1.0, 1.0, 0.9],
            value: 0.0,
            position: None,
            order: 0,
            font_family: None,
            font_size: None,
            enabled: true,
            image_path: None,
            ..Default::default()
        };
        let buf = r.rasterize(&panel(vec![el]));
        assert_eq!(buf.len(), 64 * 48 * 4);
        // Background is non-zero (opaque dark fill), so the buffer is not blank.
        assert!(buf.iter().any(|&b| b > 0), "rasterized buffer must not be blank");
        // The element bar wrote the label colour somewhere (R≈255).
        assert!(
            buf.chunks_exact(4).any(|px| px[0] > 200 && px[3] > 0),
            "label colour bar must appear"
        );
    }

    #[test]
    fn positioned_element_lands_near_its_normalized_coordinate() {
        // A single label at normalized (0.5, 0.5) must draw its colour in the
        // lower-right quadrant (text flows right from the origin), NOT in the
        // upper-left — i.e. the absolute-position path, not the index fallback.
        let r = SolidWidgetRasterizer::new();
        let el = WidgetElement {
            kind: WidgetElementKind::Label,
            path: "Label".to_owned(),
            depth: 0,
            parent_layout: WidgetLayout::Column,
            text: "X".to_owned(),
            color: [1.0, 0.0, 0.0],
            value: 0.0,
            position: Some([0.5, 0.5]),
            order: 0,
            font_family: None,
            font_size: None,
            enabled: true,
            image_path: None,
            ..Default::default()
        };
        let p = panel(vec![el]); // 64x48
        let buf = r.rasterize(&p);
        let (w, h) = (64usize, 48usize);
        let red_at = |x: usize, y: usize| {
            let px = &buf[(y * w + x) * 4..][..4];
            px[0] > 200 && px[1] < 80 && px[2] < 80
        };
        // Some red pixel exists at/after the horizontal midpoint and vertical
        // midpoint; none in the top-left corner slot.
        let mut found_mid = false;
        let mut found_topleft = false;
        for y in 0..h {
            for x in 0..w {
                if red_at(x, y) {
                    if x >= w / 2 && y >= h / 2 - 8 {
                        found_mid = true;
                    }
                    if x < w / 4 && y < h / 4 {
                        found_topleft = true;
                    }
                }
            }
        }
        assert!(found_mid, "positioned label must render near its (0.5, 0.5) coordinate");
        assert!(!found_topleft, "positioned label must NOT fall back to the top-left index slot");
    }

    #[test]
    fn font_size_changes_pixel_coverage() {
        // A larger authored `idtx:widget:fontSize` must light up more label
        // pixels than a small one — proof the per-element font size flows into
        // the rasterizer (configurable-font text).
        let r = SolidWidgetRasterizer::new();
        let make = |frac: f32| WidgetElement {
            kind: WidgetElementKind::Label,
            path: "Label".to_owned(),
            depth: 0,
            parent_layout: WidgetLayout::Column,
            text: "Size".to_owned(),
            color: [1.0, 1.0, 1.0],
            value: 0.0,
            position: None,
            order: 0,
            font_family: None,
            font_size: Some(frac),
            enabled: true,
            image_path: None,
            ..Default::default()
        };
        let bright = |buf: &[u8]| buf.chunks_exact(4).filter(|px| px[0] > 180).count();
        // 256x256 panel so both sizes fit; fontSize is pixels in raster space.
        let big_panel = WidgetPanel { resolution: [256, 256], ..panel(vec![make(96.0)]) };
        let small_panel = WidgetPanel { resolution: [256, 256], ..panel(vec![make(18.0)]) };
        let big = bright(&r.rasterize(&big_panel));
        let small = bright(&r.rasterize(&small_panel));
        assert!(big > small, "larger fontSize must cover more pixels (big={big}, small={small})");
    }

    #[test]
    fn unknown_family_falls_back_and_still_draws() {
        // An unregistered fontFamily must not blank the text — it falls back to
        // the default font so the label still rasterizes.
        let r = SolidWidgetRasterizer::new();
        let el = WidgetElement {
            kind: WidgetElementKind::Label,
            path: "Label".to_owned(),
            depth: 0,
            parent_layout: WidgetLayout::Column,
            text: "Hi".to_owned(),
            color: [1.0, 1.0, 1.0],
            value: 0.0,
            position: None,
            order: 0,
            font_family: Some("NoSuchFamily".to_owned()),
            font_size: None,
            enabled: true,
            image_path: None,
            ..Default::default()
        };
        let buf = r.rasterize(&panel(vec![el]));
        assert!(
            buf.chunks_exact(4).any(|px| px[0] > 180 && px[1] > 180),
            "unknown family must fall back to the default font and still draw"
        );
    }

    #[test]
    fn screen_aligned_quad_faces_camera() {
        let cam_right = Vec3::X;
        let cam_up = Vec3::Y;
        let (center, right, up) =
            resolve_quad(Mat4::IDENTITY, [2.0, 1.0], WidgetBillboard::ScreenAligned, cam_right, cam_up);
        assert_eq!(center, Vec3::ZERO);
        assert!((right - Vec3::X).length() < 1e-6, "half-width 1.0 along camera right");
        assert!((up - Vec3::Y * 0.5).length() < 1e-6, "half-height 0.5 along camera up");
    }

    #[test]
    fn radial_and_polygon_pixels_match_hit_samples() {
        use idtx_scene::widget_layout::{WidgetPolygon, WidgetSurface, widget_surface_hit};
        let mut menu = leaf("Menu", WidgetElementKind::Container, None);
        menu.layout = WidgetLayout::Radial;
        menu.radial.gap_angle = 12.0;
        let elements = vec![menu, leaf("Menu/A", WidgetElementKind::Button, None),
            leaf("Menu/B", WidgetElementKind::Button, None), leaf("Menu/C", WidgetElementKind::Button, None)];
        let mut p = panel(elements);
        p.resolution = [120, 96];
        p.surface.opacity = 0.0;
        let rasterizer = SolidWidgetRasterizer::new();
        for polygon in [false, true] {
            if polygon {
                p.surface.outline = Some(WidgetPolygon::new(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], vec![0, 1, 2]).unwrap());
            }
            let image = rasterizer.rasterize(&p);
            for y in 0..96 { for x in 0..120 {
                let uv = [(x as f32 + 0.5) / 120.0, (y as f32 + 0.5) / 96.0];
                let hit = widget_surface_hit(p.resolution, &p.elements, &p.surface, uv);
                assert_eq!(image[(y * 120 + x) * 4 + 3] > 0, hit, "pixel/hit mismatch at {x},{y}, polygon={polygon}");
            }}
        }
        p.surface = WidgetSurface::default();
        p.elements = vec![leaf("Button", WidgetElementKind::Button, None)];
        p.surface.opacity = 0.0;
        let image = rasterizer.rasterize(&p);
        for y in 0..96 { for x in 0..120 {
            let hit = widget_hit_test(p.resolution, &p.elements, (x as f32 + 0.5) / 120.0, (y as f32 + 0.5) / 96.0);
            assert_eq!(image[(y * 120 + x) * 4 + 3] > 0, hit.is_some());
        }}
    }

    #[test]
    fn radial_gap_raster_width_does_not_grow_toward_outer_ring() {
        let mut menu = leaf("Menu", WidgetElementKind::Container, None);
        menu.layout = WidgetLayout::Radial;
        menu.radial.gap_angle = 12.0;
        let mut p = panel(vec![menu, leaf("Menu/A", WidgetElementKind::Button, None),
            leaf("Menu/B", WidgetElementKind::Button, None), leaf("Menu/C", WidgetElementKind::Button, None)]);
        p.resolution = [256; 2];
        p.surface.opacity = 0.0;
        let image = SolidWidgetRasterizer::new().rasterize(&p);
        // The upward separator is vertical. Measure perpendicular to it near
        // the inner ring, halfway out, and near the outer ring.
        for y in [88, 64, 16] {
            let transparent: Vec<_> = (112..144).filter(|&x| image[(y * 256 + x) * 4 + 3] == 0).collect();
            assert_eq!(transparent, (124..132).collect::<Vec<_>>(), "gap at row {y}");
            for x in 112..144 {
                assert_eq!(widget_hit_test(p.resolution, &p.elements, (x as f32 + 0.5) / 256.0,
                    (y as f32 + 0.5) / 256.0).is_none(), transparent.contains(&x));
            }
        }
    }

    #[test]
    fn highlight_shader_validates() {
        let module = naga::front::wgsl::parse_str(WIDGET_WGSL).expect("widget WGSL");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("valid highlight shader");
    }
}
