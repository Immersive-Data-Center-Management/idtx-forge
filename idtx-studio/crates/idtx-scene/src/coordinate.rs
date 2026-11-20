//! Coordinate-system bridge between a USD stage and the renderer.
//!
//! `OpenUSD` stages author a `metersPerUnit` (MPU) and an `upAxis` in their root
//! layer.  The renderer always works in **Y-up** space with a configurable world
//! unit (default: 1 unit = 1 metre via [`WorldUnitScale::METRE`]).
//!
//! [`pseudo_root_matrix`] computes the single root transform that corrects for
//! both mismatches:
//! - A **uniform scale** converts from stage units to render units.
//! - An optional **rotation** maps Z-up authored content to the Y-up renderer.
//!
//! This matrix is meant to be stored once per session and pre-multiplied into
//! every USD world-space matrix before passing it to the GPU.

use glam::{Mat4, Vec3};
use num_traits::ToPrimitive;

// ── UpAxis ────────────────────────────────────────────────────────────────────

/// The up-axis authored in the USD stage root layer.
///
/// Parsed from the raw string stored in [`idtx_usd::StageMetadata`]:
/// `"Y"` or `"y"` → [`UpAxis::Y`]; anything else (including `"Z"`) → [`UpAxis::Z`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpAxis {
    /// Y is up — the default for USD and the renderer's native orientation.
    #[default]
    Y,
    /// Z is up — requires a -90° rotation around the X axis to map to Y-up.
    Z,
}

impl UpAxis {
    /// Parses the raw `upAxis` token string from [`idtx_usd::StageMetadata`].
    ///
    /// `"Y"` and `"y"` map to [`UpAxis::Y`].  Every other value (including
    /// `"Z"` / `"z"`) maps to [`UpAxis::Z`] so that malformed tokens fail
    /// safely toward the most-common non-default.
    #[must_use]
    pub fn from_usd_token(token: &str) -> Self {
        match token {
            "Y" | "y" => Self::Y,
            _ => Self::Z,
        }
    }
}

// ── WorldUnitScale ─────────────────────────────────────────────────────────────

/// How many real-world metres equal one unit in the renderer's coordinate
/// system.  Stages authored in different units are scaled to match.
///
/// The default is [`WorldUnitScale::METRE`] (1.0), meaning one renderer unit
/// equals one metre.
///
/// # Scale factor derivation
///
/// `stage_mpu / renderer_mpu` converts a stage-unit distance into a
/// renderer-unit distance.  Examples:
///
/// | Stage MPU | Renderer scale | Factor |
/// |---|---|---|
/// | 0.01 (cm) | 1.0 m | 0.01 |
/// | 1.0 (m)   | 1.0 m | 1.0  |
/// | 0.01 (cm) | 0.01 m | 1.0 |
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldUnitScale(f64);

impl WorldUnitScale {
    /// One renderer unit = one metre.  This is the recommended default for all
    /// new projects.
    pub const METRE: Self = Self(1.0);

    /// One renderer unit = one centimetre.  Useful for stages where the
    /// scene geometry lives in centimetre-scale coordinates.
    pub const CENTIMETRE: Self = Self(0.01);

    /// Creates a custom scale where one renderer unit equals `metres` real
    /// metres.  Panics in debug builds if `metres <= 0`.
    #[must_use]
    pub fn from_metres(metres: f64) -> Self {
        debug_assert!(metres > 0.0, "WorldUnitScale must be positive");
        Self(metres.max(f64::EPSILON))
    }

    /// Returns the metres-per-unit value that defines this scale.
    #[must_use]
    pub fn metres_per_unit(self) -> f64 {
        self.0
    }

    /// Computes the uniform scale factor needed to convert distances authored
    /// in `stage_meters_per_unit` into renderer units.
    ///
    /// The formula is simply `stage_mpu / renderer_mpu`.  A return value of
    /// `1.0` means no scaling is required (stage and renderer share the same
    /// unit).
    #[must_use]
    pub fn scale_from_stage(self, stage_meters_per_unit: f64) -> f32 {
        // f64 → f32 narrowing; `to_f32` never returns `None` for a finite ratio,
        // and a neutral scale of 1.0 is the safe fallback if it ever did.
        (stage_meters_per_unit / self.0).to_f32().unwrap_or(1.0)
    }
}

impl Default for WorldUnitScale {
    fn default() -> Self {
        Self::METRE
    }
}

// ── pseudo_root_matrix ────────────────────────────────────────────────────────

/// Computes the root transform that maps USD world space into renderer world
/// space.
///
/// The result is `scale_mat * rotation_mat`:
/// - `scale_mat` is a uniform scale derived from `unit_scale.scale_from_stage`.
/// - `rotation_mat` is identity for Y-up stages, or a -90° rotation about the
///   X axis for Z-up stages.
///
/// Pre-multiply every USD world-space matrix by this result before submitting
/// it to the GPU.  Store it once per session; recompute only when the stage or
/// the render world scale changes.
#[must_use]
pub fn pseudo_root_matrix(
    unit_scale: WorldUnitScale,
    stage_meters_per_unit: f64,
    up_axis: UpAxis,
) -> Mat4 {
    let scale_factor = unit_scale.scale_from_stage(stage_meters_per_unit);
    let scale_mat = Mat4::from_scale(Vec3::splat(scale_factor));
    let rotation_mat = match up_axis {
        UpAxis::Y => Mat4::IDENTITY,
        // Z-up → Y-up: rotate -90° about the X axis.
        // This maps authored +Z (up) to renderer +Y (up), and
        //              authored +Y (forward) to renderer -Z (into screen).
        UpAxis::Z => Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
    };
    scale_mat * rotation_mat
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    #[test]
    fn y_up_metre_stage_is_identity() {
        let m = pseudo_root_matrix(WorldUnitScale::METRE, 1.0, UpAxis::Y);
        assert_eq!(m, Mat4::IDENTITY);
    }

    #[test]
    fn y_up_centimetre_stage_scales_by_point_01() {
        let m = pseudo_root_matrix(WorldUnitScale::METRE, 0.01, UpAxis::Y);
        let p = m * Vec4::new(100.0, 200.0, 300.0, 1.0);
        assert!((p.x - 1.0).abs() < 1e-5, "x={}", p.x);
        assert!((p.y - 2.0).abs() < 1e-5, "y={}", p.y);
        assert!((p.z - 3.0).abs() < 1e-5, "z={}", p.z);
    }

    #[test]
    fn z_up_metre_stage_rotates_axis() {
        let m = pseudo_root_matrix(WorldUnitScale::METRE, 1.0, UpAxis::Z);
        // Authored +Z (up-axis) must map to renderer +Y.
        let z_axis = m * Vec4::new(0.0, 0.0, 1.0, 0.0);
        assert!((z_axis.y - 1.0).abs() < 1e-5, "z→y: {:?}", z_axis);
        // Authored +Y must map to renderer -Z.
        let y_axis = m * Vec4::new(0.0, 1.0, 0.0, 0.0);
        assert!((y_axis.z + 1.0).abs() < 1e-5, "y→-z: {:?}", y_axis);
        // +X is unchanged.
        let x_axis = m * Vec4::new(1.0, 0.0, 0.0, 0.0);
        assert!((x_axis.x - 1.0).abs() < 1e-5, "x→x: {:?}", x_axis);
    }

    #[test]
    fn z_up_centimetre_stage_scale_and_rotate() {
        let m = pseudo_root_matrix(WorldUnitScale::METRE, 0.01, UpAxis::Z);
        // A point at (0, 0, 100) in USD (z-up cm) should be (0, 1, 0) in render.
        let p = m * Vec4::new(0.0, 0.0, 100.0, 1.0);
        assert!((p.x).abs() < 1e-4, "x={}", p.x);
        assert!((p.y - 1.0).abs() < 1e-4, "y={}", p.y);
        assert!((p.z).abs() < 1e-4, "z={}", p.z);
    }

    #[test]
    fn scale_from_stage_ratios() {
        assert!((WorldUnitScale::METRE.scale_from_stage(1.0) - 1.0).abs() < 1e-6);
        assert!((WorldUnitScale::METRE.scale_from_stage(0.01) - 0.01).abs() < 1e-6);
        assert!((WorldUnitScale::CENTIMETRE.scale_from_stage(0.01) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn up_axis_parsing() {
        assert_eq!(UpAxis::from_usd_token("Y"), UpAxis::Y);
        assert_eq!(UpAxis::from_usd_token("y"), UpAxis::Y);
        assert_eq!(UpAxis::from_usd_token("Z"), UpAxis::Z);
        assert_eq!(UpAxis::from_usd_token("z"), UpAxis::Z);
        // Empty / unknown → Z (safe non-default)
        assert_eq!(UpAxis::from_usd_token(""), UpAxis::Z);
    }
}
