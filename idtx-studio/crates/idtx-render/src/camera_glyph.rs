//! Editor camera bodies, orientation markers, and view frusta.
//!
//! The compact frustum stays readable even when the far clip is kilometres away.
//! Selection adds the actual near/far planes. Both use the rendering projection,
//! including asymmetric and orthographic matrices, rather than a hard-coded FOV.

use glam::{Mat4, Vec3};

use crate::{OverlayLine, scene_extract::RenderCameraDesc};

/// Light rose, in the overlay pass's linear RGBA colour space.
pub const CAMERA_COLOR: [f32; 4] = [1.0, 0.5, 0.6, 1.0];

/// Append a camera body, a +Y up triangle, a -Z direction arrow, and a frustum.
/// The display depth is the compact frustum's length in render units, independent
/// of camera/stage scale. Selection adds the full authored clipping range.
pub fn push_camera_glyph(
    out: &mut Vec<OverlayLine>,
    camera: &RenderCameraDesc,
    display_depth: f32,
    selected: bool,
) {
    if !display_depth.is_finite() || display_depth <= 0.0 {
        return;
    }
    for matrix in [camera.view, camera.projection] {
        let determinant = matrix.determinant();
        if !matrix.is_finite() || !determinant.is_finite() || determinant == 0.0 {
            return;
        }
    }
    let world = camera.view.inverse();
    let inverse_projection = camera.projection.inverse();
    let z_scale = world.z_axis.truncate().length();
    if !world.is_finite() || !inverse_projection.is_finite() || z_scale <= 0.0 {
        return;
    }
    let depth = display_depth / z_scale;
    let Some(front) = frustum_section(inverse_projection, 0.0) else {
        return;
    };
    let Some(gate) = frustum_section(inverse_projection, depth) else {
        return;
    };

    // A small rectangular camera body behind the optical centre. Normalize only
    // the icon basis; the frustum retains the exact camera transform and scale.
    let icon = Mat4::from_cols(
        (world.x_axis.truncate().normalize_or_zero() * display_depth).extend(0.0),
        (world.y_axis.truncate().normalize_or_zero() * display_depth).extend(0.0),
        (world.z_axis.truncate().normalize_or_zero() * display_depth).extend(0.0),
        world.w_axis,
    );
    let body = |z| {
        [
            Vec3::new(-0.18, -0.12, z),
            Vec3::new(0.18, -0.12, z),
            Vec3::new(0.18, 0.12, z),
            Vec3::new(-0.18, 0.12, z),
        ]
    };
    push_cage(out, icon, body(0.0), body(0.28));
    // The asymmetric top marker exposes camera roll even in front/back views.
    let up = [
        Vec3::new(-0.10, 0.12, 0.14),
        Vec3::new(0.10, 0.12, 0.14),
        Vec3::new(0.0, 0.27, 0.14),
    ];
    for i in 0..3 {
        push_segment(out, icon, up[i], up[(i + 1) % 3]);
    }
    for (a, b) in [
        (Vec3::ZERO, Vec3::NEG_Z),
        (Vec3::NEG_Z, Vec3::new(-0.06, 0.0, -0.85)),
        (Vec3::NEG_Z, Vec3::new(0.06, 0.0, -0.85)),
    ] {
        push_segment(out, icon, a, b);
    }
    push_cage(out, world, front, gate);

    if selected
        && camera.clip_near.is_finite()
        && camera.clip_far.is_finite()
        && camera.clip_near > 0.0
        && camera.clip_far > camera.clip_near
    {
        if let (Some(near), Some(far)) = (
            frustum_section(inverse_projection, camera.clip_near),
            frustum_section(inverse_projection, camera.clip_far),
        ) {
            push_cage(out, world, near, far);
        }
    }
}

/// Intersect the four NDC corner rays with a camera-local depth plane. Sampling
/// z=0 and z=0.5 avoids unstable/infinite unprojection at a very distant far clip.
fn frustum_section(inverse_projection: Mat4, depth: f32) -> Option<[Vec3; 4]> {
    let mut corners = [Vec3::ZERO; 4];
    for (corner, (x, y)) in
        corners
            .iter_mut()
            .zip([(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)])
    {
        let a = inverse_projection.project_point3(Vec3::new(x, y, 0.0));
        let b = inverse_projection.project_point3(Vec3::new(x, y, 0.5));
        let ray = b - a;
        if !a.is_finite() || !ray.is_finite() || ray.z == 0.0 {
            return None;
        }
        *corner = a + ray * ((-depth - a.z) / ray.z);
        // Preserve the requested plane exactly despite interpolation rounding.
        corner.z = -depth;
        if !corner.is_finite() {
            return None;
        }
    }
    Some(corners)
}

fn push_cage(out: &mut Vec<OverlayLine>, world: Mat4, a: [Vec3; 4], b: [Vec3; 4]) {
    for i in 0..4 {
        push_segment(out, world, a[i], a[(i + 1) % 4]);
        push_segment(out, world, b[i], b[(i + 1) % 4]);
        push_segment(out, world, a[i], b[i]);
    }
}

fn push_segment(out: &mut Vec<OverlayLine>, world: Mat4, a: Vec3, b: Vec3) {
    let a = world.transform_point3(a);
    let b = world.transform_point3(b);
    if a.is_finite() && b.is_finite() && a != b {
        out.push(OverlayLine {
            a: a.to_array(),
            b: b.to_array(),
            color: CAMERA_COLOR,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> RenderCameraDesc {
        RenderCameraDesc {
            prim_path: "/Camera".into(),
            view: Mat4::IDENTITY,
            projection: glam::camera::rh::proj::directx::perspective(
                std::f32::consts::FRAC_PI_2,
                2.0,
                0.1,
                100_000.0,
            ),
            clip_near: 0.1,
            clip_far: 100_000.0,
            fov_horizontal: 0.0,
            exposure_ev: 0.0,
        }
    }

    #[test]
    fn perspective_gate_matches_fov_and_selected_clip_planes() {
        let camera = camera();
        let inverse = camera.projection.inverse();
        let gate = frustum_section(inverse, 2.0).unwrap();
        assert!(gate[0].abs_diff_eq(Vec3::new(-4.0, -2.0, -2.0), 1e-5));
        assert!(gate[2].abs_diff_eq(Vec3::new(4.0, 2.0, -2.0), 1e-5));
        let mut compact = Vec::new();
        push_camera_glyph(&mut compact, &camera, 2.0, false);
        assert!(
            compact
                .iter()
                .flat_map(|l| [l.a, l.b])
                .all(|p| p[2] >= -2.0)
        );
        let mut selected = Vec::new();
        push_camera_glyph(&mut selected, &camera, 2.0, true);
        let clip_lines = &selected[compact.len()..];
        assert_eq!(clip_lines.len(), 12);
        for point in clip_lines.iter().flat_map(|l| [l.a, l.b]).map(Vec3::from) {
            assert!(point.z == -camera.clip_near || point.z == -camera.clip_far);
            let ndc = camera.projection.project_point3(point);
            assert!((ndc.x.abs() - 1.0).abs() < 1e-5);
            assert!((ndc.y.abs() - 1.0).abs() < 1e-5);
        }
        assert!(selected.iter().all(|l| l.color == CAMERA_COLOR));
    }

    #[test]
    fn orthographic_and_shifted_projections_keep_their_shape() {
        let mut camera = camera();
        camera.projection = glam::camera::rh::proj::directx::orthographic(
            -3.0,
            5.0,
            -1.0,
            2.0,
            camera.clip_near,
            camera.clip_far,
        );
        let inverse = camera.projection.inverse();
        let near = frustum_section(inverse, 1.0).unwrap();
        let far = frustum_section(inverse, 50.0).unwrap();
        assert!(near[0].abs_diff_eq(Vec3::new(-3.0, -1.0, -1.0), 1e-5));
        assert!(far[2].abs_diff_eq(Vec3::new(5.0, 2.0, -50.0), 1e-5));
        for i in 0..4 {
            assert!((far[i] - near[i]).abs_diff_eq(Vec3::new(0.0, 0.0, -49.0), 1e-5));
        }
        camera = self::camera();
        camera.projection.z_axis.x = 0.25;
        camera.projection.z_axis.y = -0.1;
        for point in frustum_section(camera.projection.inverse(), 3.0).unwrap() {
            let ndc = camera.projection.project_point3(point);
            assert!((ndc.x.abs() - 1.0).abs() < 1e-5);
            assert!((ndc.y.abs() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn body_up_and_forward_follow_transformed_camera() {
        let mut camera = camera();
        let world = Mat4::from_scale_rotation_translation(
            Vec3::splat(0.01),
            glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            Vec3::new(3.0, 4.0, 5.0),
        );
        camera.view = world.inverse();
        let mut lines = Vec::new();
        push_camera_glyph(&mut lines, &camera, 1.0, true);
        let points: Vec<_> = lines
            .iter()
            .flat_map(|l| [l.a, l.b])
            .map(Vec3::from)
            .collect();
        // Local -Z points along world +Y; +Y up rotates into world +Z.
        assert!(
            points
                .iter()
                .any(|p| p.abs_diff_eq(Vec3::new(3.0, 5.0, 5.0), 1e-5))
        );
        assert!(
            points
                .iter()
                .any(|p| p.abs_diff_eq(Vec3::new(3.0, 3.86, 5.27), 1e-5))
        );
        assert!(points.iter().any(|p| (p.y - 1004.0).abs() < 1e-3));
    }

    #[test]
    fn invalid_camera_matrices_do_not_emit_nonfinite_lines() {
        for matrix in [Mat4::ZERO, Mat4::from_scale(Vec3::splat(f32::NAN))] {
            let mut camera = camera();
            camera.view = matrix;
            let mut lines = Vec::new();
            push_camera_glyph(&mut lines, &camera, 1.0, true);
            assert!(lines.is_empty());
            camera.view = Mat4::IDENTITY;
            camera.projection = matrix;
            push_camera_glyph(&mut lines, &camera, 1.0, true);
            assert!(lines.is_empty());
        }
    }
}
