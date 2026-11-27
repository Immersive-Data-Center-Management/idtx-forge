//! Reused by both behavior entries; this file is not a behavior attachment.

pub const SPEED_SCALE: f32 = 1.0;

/// Advance in radians, bounding long frames and wrapping each full turn.
pub fn advance_angle(angle: f32, radians_per_second: f32, dt: f32) -> f32 {
    let step = dt.max(0.0).min(0.1);
    (angle + radians_per_second * SPEED_SCALE * step) % core::f32::consts::TAU
}
