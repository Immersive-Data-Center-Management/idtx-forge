//! Transient world-space overlay **drawing** a behavior issues.
//!
//! Requires the `overlay` capability (the host import is unlinked without it).
//! Unlike [`crate::scene::Highlight`] — which styles existing prims — this
//! surface draws free-standing world-space geometry (line segments, polylines,
//! ring markers) for the current frame only: the teleport pointer arc, a floor
//! landing marker, a debug ray. Each call posts a transient `DrawLine` overlay
//! command the session resolves into render overlay lines after the update
//! hooks; nothing is ever authored into USD. Natively the calls are inert
//! `Ok(())`, so author logic stays unit-testable without a host.

use crate::abi::ErrorCode;
#[cfg(target_arch = "wasm32")]
use crate::abi::{Entity as WireEntity, OverlayCommandKind};
use crate::result::{Result, check};
use glam::{Vec3, Vec4};

/// Segments used to approximate a [`Draw::disc`] ring.
const DISC_SEGMENTS: u16 = 24;
/// Line width (world metres) for the ring segments of a [`Draw::disc`].
const DISC_WIDTH: f32 = 0.01;

/// Transient world-space overlay-draw surface. Obtain from
/// [`crate::ctx::Ctx::draw`] or [`crate::scene::Scene::draw`]. Cheap to copy —
/// holds no state. All geometry is expressed in **stage space** (the frame
/// [`crate::xr::XrHmd`] poses and scene queries use), drawn over the beauty
/// image for one frame.
#[derive(Clone, Copy, Debug)]
pub struct Draw {
    _private: (),
}

impl Draw {
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Draw a world-space line segment from `a` to `b` in `color` (linear RGBA),
    /// `width` metres wide, this frame.
    pub fn line(self, a: Vec3, b: Vec3, color: Vec4, width: f32) -> Result<()> {
        draw_line(a, b, color, width)
    }

    /// Draw a connected polyline through `points` in `color`, `width` metres
    /// wide. Fewer than two points draw nothing. Used for the sampled teleport
    /// arc (the exact `arc_cast` trajectory).
    pub fn polyline(self, points: &[Vec3], color: Vec4, width: f32) -> Result<()> {
        for seg in points.windows(2) {
            draw_line(seg[0], seg[1], color, width)?;
        }
        Ok(())
    }

    /// Draw a flat ring (disc outline) of `radius` around `center`, lying in the
    /// plane whose normal is `normal`, in `color`. The landing marker for a
    /// teleport aim. A zero/degenerate `normal` falls back to world +Y (a ring on
    /// the ground plane).
    pub fn disc(self, center: Vec3, normal: Vec3, radius: f32, color: Vec4) -> Result<()> {
        let axis_n = normal.normalize_or_zero();
        let axis_n = if axis_n == Vec3::ZERO { Vec3::Y } else { axis_n };
        // A reference axis that is not parallel to `axis_n`, so the cross product
        // is well-conditioned; +Y unless `axis_n` is itself near-vertical.
        let reference = if axis_n.y.abs() < 0.99 { Vec3::Y } else { Vec3::X };
        let axis_u = reference.cross(axis_n).normalize_or_zero();
        let axis_v = axis_n.cross(axis_u);
        let step = core::f32::consts::TAU / f32::from(DISC_SEGMENTS);
        let mut prev = center + axis_u * radius;
        for i in 1..=DISC_SEGMENTS {
            let angle = f32::from(i) * step;
            let point = center + (axis_u * angle.cos() + axis_v * angle.sin()) * radius;
            draw_line(prev, point, color, DISC_WIDTH)?;
            prev = point;
        }
        Ok(())
    }
}

/// Lower a single world-space line segment to the host overlay import (or inert
/// natively). `a`/`b` cross as two `Vec3`s (the host reads 12 bytes at each
/// pointer); `scalar` carries the line width.
fn draw_line(a: Vec3, b: Vec3, color: Vec4, width: f32) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let code = {
        let a_bytes = bytemuck::bytes_of(&a);
        let b_bytes = bytemuck::bytes_of(&b);
        let color_bytes = bytemuck::bytes_of(&color);
        ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_overlay_command(
                OverlayCommandKind::DrawLine.as_u32(),
                WireEntity::NULL.to_bits(),
                a_bytes.as_ptr(),
                0,
                b_bytes.as_ptr(),
                0,
                color_bytes.as_ptr(),
                width,
            )
        })
    };
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (a, b, color, width);
        ErrorCode::Ok
    };
    check(code)
}
