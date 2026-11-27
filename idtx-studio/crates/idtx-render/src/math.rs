//! Shared mathematical utilities for the render crate.

use glam::{Mat4, Vec3};

/// Padding factor applied to the shadow-fit bounding-sphere radius so the eight
/// scene-AABB corners (which lie exactly on that sphere) land *strictly* inside
/// the `[-1, 1]` NDC box rather than on its boundary, where float error could
/// push them out and clip a caster edge.
pub const SHADOW_FIT_PAD: f32 = 1.05;

/// Build the light-space view-projection for a **directional (Distant) light**
/// shadow map, fit to the scene's world-space AABB.
///
/// `light_travel_dir` is the direction the light *travels* — from the light
/// toward the scene. For a `UsdLuxDistantLight` this is `-world_matrix.col(2)`:
/// the PBR shader's `dir_or_pos` points from the surface *toward* the light
/// (`renderer.rs` distant assembly + `surface_gen.rs` `LIGHT_DISTANT`), so the
/// travel direction is its negation.
///
/// The frustum is fit to the scene's **bounding sphere** (radius = half the AABB
/// diagonal), not the raw AABB, so the ortho extents and near/far are invariant
/// to the light's rotation — the shadow texel density does not swim as the light
/// turns. [`SHADOW_FIT_PAD`] keeps all eight AABB corners strictly inside NDC.
///
/// Returns `(view_proj, near, far)` with the wgpu/glam right-handed
/// `near → 0 / far → 1` depth convention (the same `orthographic_rh` /
/// `perspective_rh` clip space the beauty pass uses).
#[must_use]
pub fn build_view_proj(light_travel_dir: Vec3, aabb_min: Vec3, aabb_max: Vec3) -> (Mat4, f32, f32) {
    let center = (aabb_min + aabb_max) * 0.5;
    // The AABB corners are exactly `base_radius` from the center (half-diagonal).
    let base_radius = ((aabb_max - aabb_min).length() * 0.5).max(1e-3);
    let half_extent = base_radius * SHADOW_FIT_PAD;

    // Normalize the travel direction, guarding a degenerate (zero) input.
    let dir = light_travel_dir.normalize_or_zero();
    let dir = if dir == Vec3::ZERO { Vec3::NEG_Y } else { dir };
    // Choose an up vector that is not (nearly) parallel to the light direction.
    let up = if dir.abs().dot(Vec3::Y) > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };

    // Place the eye two half-extents back along the incoming ray so the whole
    // bounding sphere sits between `near = half_extent` and `far = 3·half_extent`.
    let eye = center - dir * (half_extent * 2.0);
    let view = glam::camera::rh::view::look_at_mat4(eye, center, up);
    let near = half_extent;
    let far = half_extent * 3.0;
    let proj = glam::camera::rh::proj::directx::orthographic(
        -half_extent,
        half_extent,
        -half_extent,
        half_extent,
        near,
        far,
    );
    (proj * view, near, far)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    #[test]
    fn near_maps_to_ndc_zero() {
        // perspective_rh: near plane is at z_view = -near (RH convention, negative z = in front).
        let proj = glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_2, 1.0, 1.0, 1000.0);
        let p = proj * glam::Vec4::new(0.0, 0.0, -1.0, 1.0);
        let ndc_z = p.z / p.w;
        assert!(
            (ndc_z - 0.0).abs() < 1e-4,
            "near should map to NDC 0, got {ndc_z}"
        );
    }

    #[test]
    fn far_maps_to_ndc_one() {
        let proj = glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_2, 1.0, 1.0, 1000.0);
        let p = proj * glam::Vec4::new(0.0, 0.0, -1000.0, 1.0);
        let ndc_z = p.z / p.w;
        assert!(
            (ndc_z - 1.0).abs() < 1e-3,
            "far should map to NDC 1, got {ndc_z}"
        );
    }

    /// The eight corners of the fitted AABB must all project **strictly inside**
    /// the `[-1, 1]³` NDC box under the built ortho `view_proj` — nothing a
    /// caster occupies is clipped out of the shadow map. Checked from several
    /// light directions to prove the bounding-sphere fit is rotation-invariant.
    #[test]
    fn all_aabb_corners_inside_ndc() {
        let aabb_min = Vec3::new(-3.0, 0.0, -2.0);
        let aabb_max = Vec3::new(5.0, 4.0, 6.0);
        let corners = [
            Vec3::new(aabb_min.x, aabb_min.y, aabb_min.z),
            Vec3::new(aabb_max.x, aabb_min.y, aabb_min.z),
            Vec3::new(aabb_min.x, aabb_max.y, aabb_min.z),
            Vec3::new(aabb_max.x, aabb_max.y, aabb_min.z),
            Vec3::new(aabb_min.x, aabb_min.y, aabb_max.z),
            Vec3::new(aabb_max.x, aabb_min.y, aabb_max.z),
            Vec3::new(aabb_min.x, aabb_max.y, aabb_max.z),
            Vec3::new(aabb_max.x, aabb_max.y, aabb_max.z),
        ];
        for dir in [
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(-0.3, -1.0, -0.2),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(0.0, -0.2, -1.0),
        ] {
            let (vp, near, far) = build_view_proj(dir, aabb_min, aabb_max);
            assert!(near > 0.0 && far > near, "near/far invalid: {near}/{far}");
            for c in corners {
                let clip = vp * Vec4::new(c.x, c.y, c.z, 1.0);
                let ndc = clip.truncate() / clip.w;
                assert!(
                    ndc.x > -1.0 && ndc.x < 1.0 && ndc.y > -1.0 && ndc.y < 1.0,
                    "corner {c:?} XY outside NDC for dir {dir:?}: {ndc:?}"
                );
                assert!(
                    ndc.z > 0.0 && ndc.z < 1.0,
                    "corner {c:?} depth outside (0,1) for dir {dir:?}: {}",
                    ndc.z
                );
            }
        }
    }

    /// The ortho `view_proj` uses the RH `near → 0 / far → 1` depth convention:
    /// the scene center (equidistant plane) lands near NDC 0.5.
    #[test]
    fn ortho_depth_is_zero_to_one() {
        let (vp, _n, _f) = build_view_proj(
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, 1.0, 1.0),
        );
        // Center of the AABB is the center of the fitted sphere → midway in depth.
        let clip = vp * Vec4::new(0.0, 0.0, 0.0, 1.0);
        let ndc_z = clip.z / clip.w;
        assert!(
            (ndc_z - 0.5).abs() < 1e-4,
            "scene center should map to NDC depth 0.5, got {ndc_z}"
        );
    }
}
