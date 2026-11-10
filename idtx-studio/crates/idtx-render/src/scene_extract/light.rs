//! Renderer-facing light descriptors and the radiometric / shadow-fit helpers
//! that turn hydrated `UsdLux` records into GPU-ready light data.

use glam::{Mat3, Mat4, Vec3};
use idtx_usd::DomeTextureFormat;

// ── Light ─────────────────────────────────────────────────────────────────────

/// Renderer-facing light descriptor.
#[derive(Debug, Clone)]
pub struct RenderLightDesc {
    /// Source USD prim path.
    pub prim_path: String,
    pub kind: RenderLightKind,
    /// World-space transform (column-major glam, translation in col 3).
    pub world_matrix: Mat4,
    /// Linear render-space color (after blackbody temperature conversion, if any).
    pub color: [f32; 3],
    /// Effective point-source radiant intensity: `intensity × 2^exposure` with
    /// the emitter-area / `normalize` term folded in (see [`light_point_intensity`]).
    pub power: f32,
    pub diffuse_contribution: f32,
    pub specular_contribution: f32,
    pub shadow_enabled: bool,
    /// `Some(half_angle_degrees)` when a `UsdLuxShapingAPI` cone is authored on
    /// this light (turns a sphere/disk light into a spotlight). `None` otherwise.
    pub cone_angle: Option<f32>,
    /// Shaping cone softness in `[0, 1]`; meaningful only when `cone_angle` is `Some`.
    pub cone_softness: f32,
    /// Light-space view-projection for this light's shadow map, fit to the scene
    /// AABB (see [`crate::math::build_view_proj`]). `Some` only for a Distant
    /// light that is a shadow caster this frame; `None` for non-casters and for
    /// unsupported kinds (v1 shadows are Distant-only, per the `idtx-usd`
    /// distant-only policy). Built in [`super::RenderWorldSnapshot::extract_with_shadow`].
    pub shadow_view_proj: Option<Mat4>,
    /// Near / far planes of `shadow_view_proj` (render units). Unspecified when
    /// `shadow_view_proj` is `None`.
    pub shadow_near: f32,
    pub shadow_far: f32,
    /// Penumbra scale — the PCSS "light size". For v1 this carries the Distant
    /// light's angular diameter in radians; the per-kind source table (plan §4)
    /// and the light-space-radius conversion are finished in Phase 5.
    pub light_size: f32,
    /// Assigned atlas layer in `[0, SHADOW_BUDGET)`, or `None` when this light is
    /// not a caster or lost the importance ranking (overflow). Set by
    /// [`super::ShadowAtlasAllocator::allocate`].
    pub shadow_slot: Option<u32>,
    /// Dome environment texture path (resolved), if this is a textured dome light.
    /// `None` for non-dome lights and untextured domes.
    pub dome_texture_path: Option<String>,
    /// Dome environment projection format (equirect/lat-long is fully supported).
    pub dome_texture_format: DomeTextureFormat,
    /// World-direction → environment-frame rotation for dome sampling (dome
    /// transform + `poleAxis`). Identity for non-dome lights.
    pub env_rotation: Mat3,
    /// Effective ambient radiance a dome contributes (`color × power`). Drives the
    /// analytic path for untextured/procedural domes and is reported by the
    /// inspector as the "actual" ambient color/intensity. `[0;3]` for non-domes.
    pub ambient: [f32; 3],
}

/// Light geometry variant.
#[derive(Debug, Clone, Copy)]
pub enum RenderLightKind {
    Distant {
        angle_radians: f32,
    },
    Sphere {
        radius: f32,
    },
    Rect {
        width: f32,
        height: f32,
    },
    Disk {
        radius: f32,
    },
    /// `UsdLuxCylinderLight`: emitter along the local X axis.
    Cylinder {
        radius: f32,
        length: f32,
    },
    Dome,
    DomeProcedural,
}

/// Fold a `UsdLux` area/sphere light's authored radiometric `base`
/// (`intensity × 2^exposure`) into an effective point-source radiant intensity.
///
/// `UsdLux` area lights emit *radiance* over a surface, so the illuminance reaching
/// a receiver scales with the emitter area unless `normalize` is set. Folding the
/// area term in here lets the shader treat every non-dome light as a point source
/// with a plain inverse-square law. Dimensions must already be in **render units**
/// (unit-scaled by `metersPerUnit`) so the result is invariant to stage units.
///
/// - Sphere (`normalize=false`): `base · π·r²`; (`normalize=true`): `base/4`
///   (emitter area `π·r²` ÷ full surface area `4π·r²`).
/// - Disk   (`normalize=false`): `base · π·r²`; (`normalize=true`): `base`.
/// - Rect   (`normalize=false`): `base · w·h`;  (`normalize=true`): `base`.
/// - Distant / Dome: `base` (area-independent irradiance).
pub(crate) fn light_point_intensity(kind: &RenderLightKind, base: f32, normalize: bool) -> f32 {
    use std::f32::consts::PI;
    match *kind {
        RenderLightKind::Sphere { radius } => {
            if normalize {
                base * 0.25
            } else {
                base * PI * radius * radius
            }
        }
        RenderLightKind::Disk { radius } => {
            if normalize {
                base
            } else {
                base * PI * radius * radius
            }
        }
        RenderLightKind::Rect { width, height } => {
            if normalize {
                base
            } else {
                base * width * height
            }
        }
        // Cylinder emits over its lateral surface `2π·r·l`; approximates the
        // sphere/disk area treatment so the shader can use inverse-square falloff.
        RenderLightKind::Cylinder { radius, length } => {
            if normalize {
                base
            } else {
                base * 2.0 * PI * radius * length
            }
        }
        RenderLightKind::Distant { .. }
        | RenderLightKind::Dome
        | RenderLightKind::DomeProcedural => base,
    }
}

/// Per-kind soft-shadow penumbra size (`ShadowUniforms.params[slot].w`), the
/// scalar the PCSS shader multiplies by the receiver→blocker depth gap to size
/// the penumbra. The atlas is a bounding-sphere ortho fit whose depth range
/// equals its lateral width, so this term is a *unitless* penumbra-per-unit-gap
/// factor, not a world length — see `sample_shadow_pcss` in `surface_gen.rs`.
///
/// - **Distant**: the angular diameter `inputs:angle` (radians). A 0° sun gives a
///   razor-hard edge; the Sun subtends ≈0.009 rad. This is the physically correct
///   directional-source term.
/// - **Sphere / Disk / Rect / Cylinder** (positional area lights): a first-order
///   proxy derived from the source's dominant radius, so a larger emitter softens
///   the edge. Only `Distant` casts into the atlas in v1 (see the caster fit in
///   `extract_lights`), so these are forward-looking defaults exercised once area
///   lights gain shadow slots; they must never be negative.
/// - **Dome / procedural**: no discrete penumbra → `0.0` (hard fallback).
pub(crate) fn caster_light_size(kind: &RenderLightKind) -> f32 {
    match *kind {
        RenderLightKind::Distant { angle_radians } => angle_radians.max(0.0),
        RenderLightKind::Sphere { radius }
        | RenderLightKind::Disk { radius }
        | RenderLightKind::Cylinder { radius, .. } => radius.max(0.0),
        RenderLightKind::Rect { width, height } => (width.max(height) * 0.5).max(0.0),
        
        RenderLightKind::Dome | RenderLightKind::DomeProcedural => 0.0,
    }
}

/// Fit a shadow-casting light's light-space view-projection to the scene AABB.
///
/// Shared by the full extract ([`super::RenderWorldSnapshot::extract_with_shadow`]) and
/// the transform-fast-path refit in [`super::RenderWorldSnapshot::patch_world_matrices`]
/// so both produce an identical shadow frustum for a given light pose + scene
/// bounds — the two must never drift, or a moved light would refit to a
/// different projection than a full re-extract would.
///
/// Returns `(shadow_view_proj, near, far, light_size)`. Only a shadow-enabled
/// `Distant` light with a non-empty scene AABB is a v1 caster; every other light
/// returns `(None, 0.0, 0.0, 0.0)`. `world_matrix` is the light's render-space
/// world transform; its `+Z` column points from surface toward the light, so the
/// light travels along the negation.
pub(crate) fn caster_shadow_fit(
    kind: &RenderLightKind,
    shadow_enabled: bool,
    world_matrix: Mat4,
    aabb: Option<(Vec3, Vec3)>,
) -> (Option<Mat4>, f32, f32, f32) {
    match (shadow_enabled, kind, aabb) {
        (true, RenderLightKind::Distant { .. }, Some((amin, amax))) => {
            let travel = -world_matrix.col(2).truncate();
            let (vp, near, far) = crate::math::build_view_proj(travel, amin, amax);
            (Some(vp), near, far, caster_light_size(kind))
        }
        _ => (None, 0.0, 0.0, 0.0),
    }
}

// ── Blackbody color temperature ───────────────────────────────────────────────

/// Converts a color temperature in Kelvin to a linear-sRGB normalised color.
///
/// Uses the Kang et al. (2002) Planckian locus fit for CIE xy chromaticity,
/// then converts through XYZ to linear Rec.709.  Normalised so the brightest
/// channel is 1.0, matching `UsdLuxBlackbodyTemperatureAsRgb` semantics.
#[must_use]
pub fn blackbody_to_linear_rgb(temp_k: f32) -> [f32; 3] {
    let t = temp_k.clamp(1_000.0, 15_000.0);
    let t2 = t * t;
    let t3 = t2 * t;
    // CIE x chromaticity (Kang 2002)
    let x = if t <= 4_000.0 {
        -0.2661239e9 / t3 - 2.343_58e5 / t2 + 0.8776956e3 / t + 0.179_910
    } else {
        -3.0258469e9 / t3 + 2.107_038e6 / t2 + 0.2226347e3 / t + 0.240_390
    };
    // CIE y chromaticity (Kang 2002)
    let y = if t <= 2_222.0 {
        -1.106_381_4 * x * x * x - 1.348_110_2 * x * x + 2.185_558_3 * x - 0.202_196_83
    } else if t <= 4_000.0 {
        -0.954_947_6 * x * x * x - 1.374_185_9 * x * x + 2.091_37 * x - 0.167_488_67
    } else {
        3.081_758 * x * x * x - 5.873_387 * x * x + 3.751_129_9 * x - 0.370_014_83
    };
    // XYZ from xyY (Y = 1)
    let big_x = x / y.max(1e-6);
    let big_z = (1.0 - x - y) / y.max(1e-6);
    // XYZ → linear sRGB (D65, Rec.709)
    let r = 3.240_454_2 * big_x - 1.537_138_5 - 0.498_531_4 * big_z;
    let g = -0.969_266 * big_x + 1.876_010_8 + 0.041_556_0 * big_z;
    let b = 0.055_643_4 * big_x - 0.204_025_9 + 1.057_225_2 * big_z;
    let peak = r.max(g).max(b).max(1e-6);
    [
        (r / peak).max(0.0),
        (g / peak).max(0.0),
        (b / peak).max(0.0),
    ]
}
