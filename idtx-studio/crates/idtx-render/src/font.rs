//! Configurable-font text rasterization for widget panels.
//!
//! A [`FontContext`] holds the built-in default font (Roboto, embedded in
//! `idtx-assets`) plus a registry of additional families keyed by name, so a
//! widget element can select a face via `idtx:widget:fontFamily`. Rendering is a
//! pure-Rust CPU path ([`ab_glyph`]): glyphs are outlined to anti-aliased
//! coverage and alpha-composited onto the panel's RGBA8 buffer. The same path
//! runs in the Conductor preview and the packaged runtime (both go through
//! `idtx-render`), so text is pixel-identical across targets — no egui, no GPU
//! glyph atlas.

use std::collections::HashMap;

use ab_glyph::{point, Font, FontArc, GlyphId, PxScale, ScaleFont};

/// A registry of named TrueType/OpenType font families used to rasterize widget
/// text, with a guaranteed built-in default.
///
/// [`FontContext::new`] registers the compiled-in default font under
/// [`idtx_assets::DEFAULT_FONT_FAMILY`]; [`FontContext::register`] adds more
/// families (e.g. a stage-supplied `idtx:widget:font` asset).
/// [`FontContext::font_for`] resolves a family name, falling back to
/// the default when the name is absent or unregistered.
#[allow(missing_debug_implementations)] // FontArc holds opaque font tables.
pub struct FontContext {
    default: FontArc,
    families: HashMap<String, FontArc>,
}

impl FontContext {
    /// Build a context pre-loaded with the built-in default font (Roboto,
    /// Apache-2.0) under [`idtx_assets::DEFAULT_FONT_FAMILY`].
    ///
    /// # Panics
    /// Panics only if the compiled-in default font fails to parse, which would
    /// mean the embedded `idtx-assets` font blob is corrupt — a build-time bug,
    /// not a runtime condition.
    #[must_use]
    pub fn new() -> Self {
        let default = FontArc::try_from_vec(idtx_assets::default_font_bytes().to_vec())
            .expect("built-in default font (idtx_assets::default_font_bytes) must parse");
        let mut families = HashMap::new();
        families.insert(idtx_assets::DEFAULT_FONT_FAMILY.to_owned(), default.clone());
        Self { default, families }
    }

    /// Register a font family from raw `.ttf`/`.otf` bytes. Returns `false`
    /// (leaving the registry unchanged) when the bytes do not parse as a font,
    /// so a bad stage-supplied asset degrades to the default rather than failing.
    pub fn register(&mut self, family: impl Into<String>, bytes: &[u8]) -> bool {
        match FontArc::try_from_vec(bytes.to_vec()) {
            Ok(font) => {
                self.families.insert(family.into(), font);
                true
            }
            Err(_) => false,
        }
    }

    /// Resolve a family name to a font, falling back to the default when
    /// `family` is `None` or names a family that has not been registered.
    #[must_use]
    pub fn font_for(&self, family: Option<&str>) -> &FontArc {
        family
            .and_then(|f| self.families.get(f))
            .unwrap_or(&self.default)
    }

    /// Whether a family name resolves to a registered font (not the fallback).
    #[must_use]
    pub fn has_family(&self, family: &str) -> bool {
        self.families.contains_key(family)
    }
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Vertical metrics of a font at a pixel size: distance above (`ascent`, > 0)
/// and below (`descent`, < 0) the baseline. The visual text height is
/// `ascent - descent`.
#[must_use]
pub fn v_metrics(font: &FontArc, px: f32) -> (f32, f32) {
    let scaled = font.as_scaled(PxScale::from(px));
    (scaled.ascent(), scaled.descent())
}

/// Advance width in pixels of `text` rendered in `font` at pixel size `px`
/// (sum of per-glyph horizontal advances plus kerning).
#[must_use]
pub fn measure(font: &FontArc, text: &str, px: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut width = 0.0;
    let mut prev: Option<GlyphId> = None;
    for ch in text.chars() {
        let gid = font.glyph_id(ch);
        if let Some(p) = prev {
            width += scaled.kern(p, gid);
        }
        width += scaled.h_advance(gid);
        prev = Some(gid);
    }
    width
}

/// Rasterize `text` in `font` at pixel size `px`, left origin `origin_x` and
/// baseline `baseline_y`, alpha-compositing anti-aliased glyph coverage in
/// `color` onto the row-major RGBA8 `buf` of `w`×`h`. Pixels outside the buffer
/// are clipped. Linear colour (the panel texture is `Rgba8Unorm`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
pub fn draw_text(
    buf: &mut [u8],
    w: usize,
    h: usize,
    origin_x: f32,
    baseline_y: f32,
    text: &str,
    px: f32,
    color: [u8; 4],
    font: &FontArc,
) {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut pen_x = origin_x;
    let mut prev: Option<GlyphId> = None;
    for ch in text.chars() {
        let gid = font.glyph_id(ch);
        if let Some(p) = prev {
            pen_x += scaled.kern(p, gid);
        }
        let glyph = gid.with_scale_and_position(px, point(pen_x, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            let min_x = bounds.min.x as i32;
            let min_y = bounds.min.y as i32;
            outlined.draw(|gx, gy, coverage| {
                let x = min_x + gx as i32;
                let y = min_y + gy as i32;
                if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                    return;
                }
                blend_px(buf, (y as usize * w + x as usize) * 4, color, coverage);
            });
        }
        pen_x += scaled.h_advance(gid);
        prev = Some(gid);
    }
}

/// Alpha-composite `src` (scaled by `coverage`) over the RGBA8 pixel at byte
/// offset `i` ("over" operator on straight-alpha pixels).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn blend_px(buf: &mut [u8], i: usize, src: [u8; 4], coverage: f32) {
    let a = coverage.clamp(0.0, 1.0) * (f32::from(src[3]) / 255.0);
    if a <= 0.0 {
        return;
    }
    for c in 0..3 {
        let s = f32::from(src[c]);
        let d = f32::from(buf[i + c]);
        buf[i + c] = (s * a + d * (1.0 - a)).round() as u8;
    }
    let da = f32::from(buf[i + 3]) / 255.0;
    let out_a = a + da * (1.0 - a);
    buf[i + 3] = (out_a * 255.0).round() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_context_has_default_family() {
        let ctx = FontContext::new();
        assert!(ctx.has_family(idtx_assets::DEFAULT_FONT_FAMILY));
        // An unknown family falls back to the default font (same Arc contents).
        let fallback = ctx.font_for(Some("DoesNotExist"));
        let default = ctx.font_for(None);
        assert!(std::ptr::eq(fallback, default));
    }

    #[test]
    fn register_rejects_garbage_keeps_default() {
        let mut ctx = FontContext::new();
        assert!(!ctx.register("Bogus", b"not a font"), "garbage bytes must not register");
        assert!(!ctx.has_family("Bogus"));
    }

    #[test]
    fn measure_is_positive_and_grows_with_size() {
        let ctx = FontContext::new();
        let font = ctx.font_for(None);
        let small = measure(font, "Hello", 16.0);
        let big = measure(font, "Hello", 32.0);
        assert!(small > 0.0, "non-empty text must have positive width");
        assert!(big > small, "larger pixel size must advance further");
        assert_eq!(measure(font, "", 16.0), 0.0, "empty text has zero width");
    }

    #[test]
    fn draw_text_writes_coverage() {
        let ctx = FontContext::new();
        let font = ctx.font_for(None);
        let (w, h) = (64usize, 32usize);
        let mut buf = vec![0u8; w * h * 4];
        let (ascent, descent) = v_metrics(font, 20.0);
        let baseline = (h as f32 + ascent + descent) * 0.5;
        draw_text(&mut buf, w, h, 2.0, baseline, "Ag", 20.0, [255, 255, 255, 255], font);
        let lit = buf.chunks_exact(4).filter(|px| px[3] > 0 && px[0] > 0).count();
        assert!(lit > 0, "rasterized glyphs must light up some pixels");
        // Anti-aliasing: at least some partial-coverage edge pixels (not pure 0/255).
        let partial = buf.chunks_exact(4).filter(|px| px[0] > 0 && px[0] < 255).count();
        assert!(partial > 0, "expected anti-aliased edge pixels");
    }
}
