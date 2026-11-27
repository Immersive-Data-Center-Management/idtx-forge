//! Prim highlight / overlay-material surface a behavior issues.
//!
//! Requires the `overlay` capability (the host import is unlinked without it).
//! Each call posts a transient command the session drains into the render world
//! after the update hooks — a per-prim outline/tint for the current frame only,
//! never authored into USD. Natively the calls are inert `Ok(())`.

use crate::abi::{Entity as WireEntity, ErrorCode, OverlayCommandKind};
use crate::entity::Entity;
use crate::result::{Result, check};
use glam::Vec4;

/// Transient render-overlay surface. Obtain from [`crate::ctx::Ctx::highlight`]
/// or [`crate::scene::Scene::highlight`]. Cheap to copy — holds no state.
///
/// Targeting is geometry-aware: a highlight / tint / outline on a leaf geometry
/// prim styles that prim, while one on a grouping prim (`Xform`, `Scope`, …)
/// expands host-side to every leaf geometry prim beneath it — so highlighting an
/// `Xform` lights up all the geometry it contains, matching the subtree a convex
/// collision hull is built from.
#[derive(Clone, Copy, Debug)]
pub struct Highlight {
    _private: (),
}

impl Highlight {
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Highlight `entity` with a coloured wireframe box in `color` (linear
    /// RGBA) this frame — the low-risk fallback cue. For surface recolouring use
    /// [`Self::tint`]; for a silhouette use [`Self::outline`].
    pub fn highlight(self, entity: Entity, color: Vec4) -> Result<()> {
        overlay(
            OverlayCommandKind::Highlight,
            entity.wire(),
            None,
            Some(color),
            0.0,
        )
    }

    /// Tint `entity`'s surface with `color` (linear RGBA) this frame; the alpha
    /// channel is the tint strength (0 = untouched, 1 = fully `color`).
    pub fn tint(self, entity: Entity, color: Vec4) -> Result<()> {
        overlay(
            OverlayCommandKind::HighlightTint,
            entity.wire(),
            None,
            Some(color),
            0.0,
        )
    }

    /// Draw a silhouette outline around `entity` in `color` (linear RGBA) this
    /// frame, using the renderer's default band width. See [`Self::outline_with`]
    /// to set the thickness.
    pub fn outline(self, entity: Entity, color: Vec4) -> Result<()> {
        self.outline_with(entity, color, 0.0)
    }

    /// Draw a silhouette outline around `entity` in `color` with an explicit
    /// band width `thickness_px` (pixels). `0.0` falls back to the renderer
    /// default; softness is derived from the thickness.
    pub fn outline_with(self, entity: Entity, color: Vec4, thickness_px: f32) -> Result<()> {
        overlay(
            OverlayCommandKind::HighlightOutline,
            entity.wire(),
            None,
            Some(color),
            thickness_px,
        )
    }

    /// Clear any highlight on `entity`.
    pub fn clear(self, entity: Entity) -> Result<()> {
        overlay(
            OverlayCommandKind::ClearHighlight,
            entity.wire(),
            None,
            None,
            0.0,
        )
    }

    /// Override `entity`'s material with the named runtime material this frame.
    pub fn material_override(self, entity: Entity, material: &str) -> Result<()> {
        overlay(
            OverlayCommandKind::SetMaterialOverride,
            entity.wire(),
            Some(material),
            None,
            0.0,
        )
    }

    /// Clear `entity`'s material override.
    pub fn clear_material(self, entity: Entity) -> Result<()> {
        overlay(
            OverlayCommandKind::ClearMaterialOverride,
            entity.wire(),
            None,
            None,
            0.0,
        )
    }

    /// Set a named outline / post-process parameter (e.g. `"outline.thickness"`)
    /// to `value` for this frame. Global (not prim-scoped).
    pub fn outline_param(self, name: &str, value: f32) -> Result<()> {
        overlay(
            OverlayCommandKind::SetPostprocessParam,
            WireEntity::NULL,
            Some(name),
            None,
            value,
        )
    }
}

/// Post one overlay command through the host import (or inert natively).
fn overlay(
    kind: OverlayCommandKind,
    target: WireEntity,
    a: Option<&str>,
    color: Option<Vec4>,
    scalar: f32,
) -> Result<()> {
    let color_val = color.unwrap_or(Vec4::ZERO);
    let color_bytes = bytemuck::bytes_of(&color_val);
    #[cfg(target_arch = "wasm32")]
    let code = {
        let (a_ptr, a_len) = a.map_or((core::ptr::null::<u8>(), 0u32), |s| {
            (s.as_ptr(), u32::try_from(s.len()).unwrap_or(u32::MAX))
        });
        ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_overlay_command(
                kind.as_u32(),
                target.to_bits(),
                a_ptr,
                a_len,
                core::ptr::null::<u8>(),
                0,
                color_bytes.as_ptr(),
                scalar,
            )
        })
    };
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (kind, target, a, color_bytes, scalar);
        ErrorCode::Ok
    };
    check(code)
}
