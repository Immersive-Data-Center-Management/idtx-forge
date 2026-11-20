//! Transform math for the read (seed) and write-back paths.
//!
//! Physics works on rigid **isometries** (rotation + translation); USD world
//! matrices routinely carry scale. The read path decomposes world → (pose,
//! scale) and bakes uniform scale into the collider; non-uniform scale on a
//! physics body is unsupported (plan §5.5). The write-back path converts the
//! solver's **world** pose into the prim's **local** `xformOp:transform` matrix
//! (plan §7.2) — `local == world` only when every ancestor is identity, which
//! is not guaranteed.

use glam::{Mat4, Vec3};
use rapier3d::prelude::Pose;

/// A world matrix split into a rigid pose and a scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decomposed {
    /// Rigid pose (rotation + translation) as a rapier/glamx [`Pose`].
    pub pose: Pose,
    /// Per-axis scale factored out of the matrix.
    pub scale: Vec3,
}

/// Decompose a world matrix into (rigid pose, scale) via
/// `Mat4::to_scale_rotation_translation`.
#[must_use]
pub fn decompose(world: Mat4) -> Decomposed {
    let (scale, rotation, translation) = world.to_scale_rotation_translation();
    Decomposed {
        pose: Pose::from_parts(translation, rotation),
        scale,
    }
}

/// Whether `scale` is (near-)uniform within `eps` relative tolerance. Physics
/// bodies require uniform scale (parry rigid shapes carry no non-uniform scale);
/// a non-uniform factor makes a rotating body shear, so the hydrator warns and
/// treats the body as uniform-at-its-mean.
#[must_use]
pub fn is_uniform_scale(scale: Vec3, eps: f32) -> bool {
    let max = scale.abs().max_element();
    if max <= f32::EPSILON {
        return true; // degenerate; treat as uniform (zero)
    }
    let spread = scale.abs().max_element() - scale.abs().min_element();
    spread / max <= eps
}

/// Compose a rigid [`Pose`] plus a uniform `scale` back into a `Mat4`.
#[must_use]
pub fn pose_to_matrix(pose: Pose, scale: Vec3) -> Mat4 {
    Mat4::from_scale_rotation_translation(scale, pose.rotation, pose.translation)
}

/// A dynamic body's solver **world** pose → the prim's **local**
/// `xformOp:transform` matrix: `local = parent_world⁻¹ · body_world`.
///
/// `parent_world` is the prim parent's USD-space world transform (identity for a
/// root-level body). Do **not** fold in `projection_basis` — that is render-only.
#[must_use]
pub fn world_to_local(parent_world: Mat4, body_world: Mat4) -> Mat4 {
    parent_world.inverse() * body_world
}

/// A child collider's **body-local** pose for a compound (whole-asset) body:
/// `body_root_world⁻¹ · collider_world`, decomposed to a rigid pose. This bakes
/// the body-relative transform, **not** the world one (plan §5.1 — correctness
/// critical: using the world transform double-applies the body root).
#[must_use]
pub fn compound_part_pose(body_root_world: Mat4, collider_world: Mat4) -> Pose {
    decompose(body_root_world.inverse() * collider_world).pose
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    fn close_m(a: Mat4, b: Mat4) -> bool {
        a.to_cols_array()
            .iter()
            .zip(b.to_cols_array())
            .all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn decompose_round_trips_rigid_plus_uniform_scale() {
        let m = Mat4::from_scale_rotation_translation(
            Vec3::splat(2.0),
            Quat::from_rotation_y(0.5),
            Vec3::new(1.0, 2.0, 3.0),
        );
        let d = decompose(m);
        assert!(close_m(pose_to_matrix(d.pose, d.scale), m));
    }

    #[test]
    fn uniform_scale_detection() {
        assert!(is_uniform_scale(Vec3::splat(3.0), 1e-3));
        assert!(is_uniform_scale(Vec3::new(2.0, 2.0001, 2.0), 1e-2));
        assert!(!is_uniform_scale(Vec3::new(1.0, 2.0, 1.0), 1e-3));
    }

    #[test]
    fn world_to_local_under_identity_parent_is_world() {
        let body = Mat4::from_translation(Vec3::new(5.0, 6.0, 7.0));
        assert!(close_m(world_to_local(Mat4::IDENTITY, body), body));
    }

    #[test]
    fn world_to_local_under_nonidentity_parent() {
        // The regression target for the "local == world" bug: a body at world
        // (0,10,0) under a parent translated to (0,100,0) must author a LOCAL
        // transform of (0,-90,0), not (0,10,0).
        let parent = Mat4::from_translation(Vec3::new(0.0, 100.0, 0.0));
        let body_world = Mat4::from_translation(Vec3::new(0.0, 10.0, 0.0));
        let local = world_to_local(parent, body_world);
        let (_, _, t) = local.to_scale_rotation_translation();
        assert!((t - Vec3::new(0.0, -90.0, 0.0)).length() < 1e-4, "{t:?}");
    }

    #[test]
    fn compound_part_pose_is_body_relative() {
        // Body root at world (10,0,0); a child collider at world (12,0,0) has a
        // body-local offset of (2,0,0).
        let body_root = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0));
        let collider_world = Mat4::from_translation(Vec3::new(12.0, 0.0, 0.0));
        let pose = compound_part_pose(body_root, collider_world);
        assert!((pose.translation - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4);
    }
}
