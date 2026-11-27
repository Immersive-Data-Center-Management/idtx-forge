//! Image-based lighting math for `UsdLuxDomeLight` environments.
//!
//! This module is deliberately GPU-free and dependency-light so the core IBL
//! math can be unit-tested on the CPU. It provides:
//!
//! * [`EnvImage`] — an in-memory linear equirectangular (lat-long) environment.
//! * [`ShL2`] — 9 RGB spherical-harmonic coefficients (order-2) projected from an
//!   environment, used for smooth directional **diffuse** irradiance.
//! * [`dir_to_equirect_uv`] / [`equirect_uv_to_dir`] — the lat-long mapping used
//!   for both the CPU projection and the GPU specular sample, kept in one place so
//!   both sides agree.
//! * [`average_radiance`] — the environment's mean radiance, i.e. the ambient
//!   color a dome contributes (the "actual ambient" the inspector reports).
//!
//! # Conventions
//!
//! Directions are unit vectors in the **environment's own frame** (Y-up, right
//! handed): `+Y` is the pole, azimuth measured from `-Z`. The renderer applies a
//! separate rotation (dome world transform + `poleAxis` + stage up-axis) before
//! looking up the environment, so this module never needs to know the stage's
//! coordinate system.
//!
//! The spherical-harmonic diffuse path follows Ramamoorthi & Hanrahan 2001,
//! *"An Efficient Representation for Irradiance Environment Maps"*: the projected
//! radiance coefficients are convolved with the clamped-cosine kernel (`A_0 = π`,
//! `A_1 = 2π/3`, `A_2 = π/4`) at reconstruction time, so [`ShL2::irradiance`]
//! returns physically-scaled irradiance `E(n)` — for a constant radiance `L`
//! environment, `E(n) == π·L` for every normal.

use crate::convert::AsF32;
use glam::{Mat3, Mat4, Vec3};

/// How the environment lat-long poles map to render-space axes.
///
/// Mirrors `idtx_usd::PoleAxis` but kept local so this module stays free of
/// higher-crate coupling. `Scene` and `Y` are equivalent here because the
/// renderer already works in a Y-up render space (the stage up-axis correction
/// is baked into the light's world matrix upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvPoleAxis {
    #[default]
    Y,
    Z,
}

/// Number of order-2 real spherical-harmonic basis functions (`(l+1)^2`, `l≤2`).
pub const SH_COEFF_COUNT: usize = 9;

/// A linear (scene-referred) equirectangular environment image.
///
/// Pixels are stored row-major, top row first. Row `0` is the top of the
/// environment (`+Y` pole) and the last row is the bottom (`-Y` pole); columns
/// sweep azimuth from `0` to `2π`. Values are linear RGB radiance — **not**
/// gamma-encoded and **not** clamped to `[0,1]`.
#[derive(Debug, Clone)]
pub struct EnvImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` linear RGB texels, row-major from the top pole down.
    pub texels: Vec<[f32; 3]>,
}

impl EnvImage {
    /// Construct from raw texels, validating the length against `width * height`.
    #[must_use]
    pub fn new(width: u32, height: u32, texels: Vec<[f32; 3]>) -> Self {
        assert_eq!(
            texels.len(),
            (width as usize) * (height as usize),
            "EnvImage texel count must equal width * height",
        );
        Self {
            width,
            height,
            texels,
        }
    }

    #[inline]
    fn texel(&self, x: u32, y: u32) -> Vec3 {
        let t = self.texels[(y as usize) * (self.width as usize) + (x as usize)];
        Vec3::new(t[0], t[1], t[2])
    }
}

/// Order-2 spherical-harmonic coefficients: 9 basis functions × RGB.
///
/// These are the projected **radiance** coefficients (the cosine convolution is
/// applied in [`irradiance`](Self::irradiance) / on the GPU), so they can also be
/// used to recover the environment's average radiance cheaply via [`Self::dc`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShL2 {
    pub coeffs: [[f32; 3]; SH_COEFF_COUNT],
}

impl Default for ShL2 {
    fn default() -> Self {
        Self {
            coeffs: [[0.0; 3]; SH_COEFF_COUNT],
        }
    }
}

/// Evaluate the 9 real SH basis functions (order 2) for a unit direction.
///
/// Constant factors are the standard normalized real SH coefficients.
#[inline]
#[must_use]
pub fn sh_basis(dir: Vec3) -> [f32; SH_COEFF_COUNT] {
    let (x, y, z) = (dir.x, dir.y, dir.z);
    [
        0.282_095,                       // Y_0^0
        0.488_603 * y,                   // Y_1^-1
        0.488_603 * z,                   // Y_1^0
        0.488_603 * x,                   // Y_1^1
        1.092_548 * x * y,               // Y_2^-2
        1.092_548 * y * z,               // Y_2^-1
        0.315_392 * (3.0 * z * z - 1.0), // Y_2^0
        1.092_548 * x * z,               // Y_2^1
        0.546_274 * (x * x - y * y),     // Y_2^2
    ]
}

/// Clamped-cosine convolution constants `A_l` per SH band (Ramamoorthi 2001).
const A_L: [f32; SH_COEFF_COUNT] = {
    use std::f32::consts::PI;
    let a0 = PI;
    let a1 = 2.0 * PI / 3.0;
    let a2 = PI / 4.0;
    [a0, a1, a1, a1, a2, a2, a2, a2, a2]
};

impl ShL2 {
    /// Reconstruct irradiance `E(n)` (already cosine-convolved and π-scaled).
    ///
    /// Multiply by `albedo / π` to get outgoing Lambertian diffuse radiance.
    #[must_use]
    pub fn irradiance(&self, normal: Vec3) -> Vec3 {
        let basis = sh_basis(normal.normalize_or_zero());
        let mut e = Vec3::ZERO;
        for i in 0..SH_COEFF_COUNT {
            let c = Vec3::new(self.coeffs[i][0], self.coeffs[i][1], self.coeffs[i][2]);
            e += c * (A_L[i] * basis[i]);
        }
        e.max(Vec3::ZERO)
    }

    /// The environment's average radiance (the DC term), i.e. the ambient color a
    /// dome contributes before its `color`/`intensity`/`exposure` scaling.
    ///
    /// Equals `(1/4π) ∫ L(ω) dω`. For a constant radiance `L`, returns `L`.
    #[must_use]
    pub fn dc(&self) -> Vec3 {
        // average = L_00 * Y_0^0, and Y_0^0 = 0.282095.
        Vec3::new(self.coeffs[0][0], self.coeffs[0][1], self.coeffs[0][2]) * 0.282_095
    }
}

/// Map a unit direction (env frame, Y-up) to equirectangular UV in `[0,1]²`.
///
/// `u` wraps azimuth from `-Z` (u=0.5 faces `-Z`); `v=0` is the `+Y` pole. This
/// is the exact inverse of [`equirect_uv_to_dir`] and matches the GPU sampler.
#[inline]
#[must_use]
pub fn dir_to_equirect_uv(dir: Vec3) -> (f32, f32) {
    use std::f32::consts::PI;
    let d = dir.normalize_or_zero();
    let u = 0.5 + f32::atan2(d.x, -d.z) / (2.0 * PI);
    let v = f32::acos(d.y.clamp(-1.0, 1.0)) / PI;
    (u, v)
}

/// Inverse of [`dir_to_equirect_uv`]: UV in `[0,1]²` → unit direction (env frame).
#[inline]
#[must_use]
pub fn equirect_uv_to_dir(u: f32, v: f32) -> Vec3 {
    use std::f32::consts::PI;
    let phi = (u - 0.5) * 2.0 * PI;
    let theta = v * PI;
    let sin_theta = theta.sin();
    // Invert dir_to_equirect_uv: x = sinθ·sinφ, z = -sinθ·cosφ, y = cosθ.
    Vec3::new(sin_theta * phi.sin(), theta.cos(), -sin_theta * phi.cos())
}

/// Project an equirectangular environment onto order-2 SH radiance coefficients.
///
/// Uses solid-angle (`sinθ`) weighting per texel and normalizes by the summed
/// weight so the DC term is a true average regardless of resolution.
#[must_use]
pub fn project_sh_l2(env: &EnvImage) -> ShL2 {
    use std::f32::consts::PI;
    let mut coeffs = [[0.0f32; 3]; SH_COEFF_COUNT];
    let mut weight_sum = 0.0f32;
    let w = env.width;
    let h = env.height;
    for y in 0..h {
        // Texel center latitude: theta in (0, π), 0 at the +Y pole.
        let theta = PI * (y.as_f32() + 0.5) / h.as_f32();
        let sin_theta = theta.sin();
        for x in 0..w {
            let u = (x.as_f32() + 0.5) / w.as_f32();
            let v = (y.as_f32() + 0.5) / h.as_f32();
            let dir = equirect_uv_to_dir(u, v);
            let basis = sh_basis(dir);
            let radiance = env.texel(x, y);
            // Solid angle of this texel ∝ sinθ (constant factors cancel in the
            // normalization below).
            let dw = sin_theta;
            weight_sum += dw;
            for i in 0..SH_COEFF_COUNT {
                let contrib = basis[i] * dw;
                coeffs[i][0] += radiance.x * contrib;
                coeffs[i][1] += radiance.y * contrib;
                coeffs[i][2] += radiance.z * contrib;
            }
        }
    }
    // The continuous projection integrates against dω over the sphere (total 4π).
    // Our discrete sum used unnormalized weights, so rescale by 4π / Σw.
    let scale = if weight_sum > 0.0 {
        4.0 * PI / weight_sum
    } else {
        0.0
    };
    for c in &mut coeffs {
        c[0] *= scale;
        c[1] *= scale;
        c[2] *= scale;
    }
    ShL2 { coeffs }
}

/// Solid-angle-weighted mean radiance of an environment (its ambient color).
///
/// Equivalent to [`ShL2::dc`] but computed directly; handy when SH coefficients
/// are not otherwise needed.
#[must_use]
pub fn average_radiance(env: &EnvImage) -> Vec3 {
    use std::f32::consts::PI;
    let mut sum = Vec3::ZERO;
    let mut weight = 0.0f32;
    for y in 0..env.height {
        let theta = PI * (y.as_f32() + 0.5) / env.height.as_f32();
        let sin_theta = theta.sin();
        for x in 0..env.width {
            sum += env.texel(x, y) * sin_theta;
            weight += sin_theta;
        }
    }
    if weight > 0.0 {
        sum / weight
    } else {
        Vec3::ZERO
    }
}

/// Build the world-direction → environment-frame rotation for a dome light.
///
/// The dome's transform orients the environment: rotating the light rotates the
/// sky. A world-space direction is brought into the environment's local frame by
/// the inverse of the dome's (orthonormalized) world rotation, after which
/// [`EnvPoleAxis::Z`] domes are tilted so their `+Z` pole aligns with this
/// module's `+Y` sampling pole.
///
/// `world_matrix` is the dome's **render-space** world transform (stage up-axis
/// correction already applied), so `Y` needs no extra remap.
#[must_use]
pub fn env_rotation(world_matrix: Mat4, pole: EnvPoleAxis) -> Mat3 {
    // Orthonormalize the upper-left 3×3 to strip scale/shear, then invert (a pure
    // rotation's inverse is its transpose).
    let m = Mat3::from_mat4(world_matrix);
    let rot = Mat3::from_cols(
        m.x_axis.normalize_or(Vec3::X),
        m.y_axis.normalize_or(Vec3::Y),
        m.z_axis.normalize_or(Vec3::Z),
    );
    let world_to_local = rot.transpose();
    let pole_remap = match pole {
        EnvPoleAxis::Y => Mat3::IDENTITY,
        // Rotate so a +Z pole becomes the +Y sampling pole.
        EnvPoleAxis::Z => Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2),
    };
    pole_remap * world_to_local
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn constant_env(w: u32, h: u32, l: f32) -> EnvImage {
        EnvImage::new(w, h, vec![[l, l, l]; (w * h) as usize])
    }

    #[test]
    fn uv_dir_roundtrip() {
        for &(u, v) in &[
            (0.0, 0.5),
            (0.25, 0.5),
            (0.5, 0.5),
            (0.75, 0.5),
            (0.5, 0.1),
            (0.5, 0.9),
        ] {
            let d = equirect_uv_to_dir(u, v);
            let (u2, v2) = dir_to_equirect_uv(d);
            assert!(
                (u - u2).abs() < 1e-4 || (u - u2).abs() > 1.0 - 1e-4,
                "u {u} vs {u2}"
            );
            assert!((v - v2).abs() < 1e-4, "v {v} vs {v2}");
        }
    }

    #[test]
    fn poles_map_to_up_down() {
        // v=0 → +Y pole, v=1 → -Y pole.
        assert!(equirect_uv_to_dir(0.5, 0.0).y > 0.99);
        assert!(equirect_uv_to_dir(0.5, 1.0).y < -0.99);
    }

    #[test]
    fn constant_env_irradiance_is_pi_l() {
        // Physical invariant: irradiance from a constant radiance L is π·L in every
        // direction. Validates both the projection normalization and A_l scaling.
        let env = constant_env(64, 32, 1.0);
        let sh = project_sh_l2(&env);
        for n in [
            Vec3::Y,
            Vec3::X,
            Vec3::Z,
            -Vec3::Y,
            Vec3::new(1.0, 1.0, 1.0),
        ] {
            let e = sh.irradiance(n);
            assert!((e.x - PI).abs() < 0.02, "E={e:?} expected ~π for n={n:?}");
        }
    }

    #[test]
    fn constant_env_dc_is_l() {
        let env = constant_env(64, 32, 0.5);
        let sh = project_sh_l2(&env);
        let dc = sh.dc();
        assert!((dc.x - 0.5).abs() < 1e-3, "dc={dc:?}");
        let avg = average_radiance(&env);
        assert!((avg.x - 0.5).abs() < 1e-3, "avg={avg:?}");
    }

    #[test]
    fn env_rotation_identity_is_passthrough() {
        let r = env_rotation(Mat4::IDENTITY, EnvPoleAxis::Y);
        for d in [Vec3::X, Vec3::Y, Vec3::Z] {
            assert!(
                (r * d - d).length() < 1e-5,
                "identity should pass {d:?} through"
            );
        }
    }

    #[test]
    fn env_rotation_inverts_dome_yaw() {
        // A dome yawed +90° about Y rotates the environment with it; bringing a
        // world +X direction into the dome's local frame yields +Z (the inverse
        // rotation), confirming the transform is applied as world→local.
        let world = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let r = env_rotation(world, EnvPoleAxis::Y);
        let local = r * Vec3::X;
        assert!(
            (local - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-4,
            "got {local:?}"
        );
    }

    #[test]
    fn env_rotation_z_pole_maps_z_to_y() {
        let r = env_rotation(Mat4::IDENTITY, EnvPoleAxis::Z);
        let up = r * Vec3::Z;
        assert!(
            (up - Vec3::Y).length() < 1e-4,
            "Z pole should sample toward +Y, got {up:?}"
        );
    }

    #[test]
    fn directional_env_irradiance_peaks_toward_bright_region() {
        // Bright cap around +X, dark elsewhere; irradiance should be larger for a
        // normal facing +X than one facing -X.
        let (w, h) = (128u32, 64u32);
        let mut texels = vec![[0.02f32; 3]; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let u = (x.as_f32() + 0.5) / w.as_f32();
                let v = (y.as_f32() + 0.5) / h.as_f32();
                let d = equirect_uv_to_dir(u, v);
                if d.x > 0.7 {
                    texels[(y * w + x) as usize] = [10.0, 10.0, 10.0];
                }
            }
        }
        let sh = project_sh_l2(&EnvImage::new(w, h, texels));
        let toward = sh.irradiance(Vec3::X).x;
        let away = sh.irradiance(-Vec3::X).x;
        assert!(
            toward > away * 1.5,
            "toward={toward} should exceed away={away}"
        );
    }
}
