//! `math` — pure transform helpers shared across behaviors.
//!
//! Every function here is plain `glam` arithmetic over matrices the host has
//! already handed back (`Entity::world_matrix`, `xr().get_pose`, `camera()`,
//! …). There are **no** host imports and no `Entity`/`Ctx` dependency, so the
//! module compiles identically on wasm and native and every helper is a
//! straight unit test with no wasm host — the author's existing `#[cfg(test)]`
//! math tests move here unchanged.
//!
//! # Why the SDK, not a host function
//!
//! These are a matrix inverse and a compose or two — tens of flops. Routing
//! them through the host would mean marshalling 64-byte `Mat4`s each way across
//! the wasm linear-memory boundary per call, which costs more than the
//! arithmetic it replaces, with no offsetting win: the same pinned `glam`
//! (`=0.33.12`) compiles into the guest and Cranelift lowers it to native code
//! at load. The host boundary is reserved for operations that need host *state*
//! — `scene::raycast`, `scene::arc_cast`, `physics`, XR poses — not for
//! arithmetic over values the guest already holds.
//!
//! The helpers are deliberately framed around *frames* and *offsets*, not
//! "grab": the same `frame⁻¹ * target` / `parent⁻¹ * (frame * offset)` pair
//! describes a grabbed prim riding a hand, an attachment pinned to a mount
//! point, a prop following a socket, or a camera holding a fixed offset to its
//! target.

use glam::{Mat4, Vec3};

/// The rigid offset of `target_world` expressed in the local frame of `frame`:
/// `frame⁻¹ * target_world`.
///
/// Capture this once (e.g. at grab time) to record where `target_world` sits
/// *relative to* `frame`; feed it back to [`local_in_parent`] each frame to
/// reproduce that relationship as `frame` moves. When the target coincides with
/// the frame the offset is the identity.
#[must_use]
pub fn relative_transform(frame: Mat4, target_world: Mat4) -> Mat4 {
    frame.inverse() * target_world
}

/// Reproduce a [`relative_transform`] `offset` in world space from a live
/// `frame` (`frame * offset`), then express that world transform in
/// `parent_world`'s local space (`parent_world⁻¹ * …`).
///
/// This is the counterpart to [`relative_transform`] for authoring: the host
/// authors a prim's *local* (session-layer) opinion, so a desired **world**
/// pose must be lifted back through the prim's parent before it is written.
/// Pass [`Mat4::IDENTITY`] as `parent_world` for a root prim (or when the
/// parent world transform is unavailable this frame) — the result is then the
/// desired world transform itself.
#[must_use]
pub fn local_in_parent(frame: Mat4, offset: Mat4, parent_world: Mat4) -> Mat4 {
    parent_world.inverse() * (frame * offset)
}

/// Extract a world-space ray from a transform matrix: the origin is the
/// matrix's translation, the direction its forward (`-Z`) axis, normalized.
///
/// Converts a controller aim / HMD / camera matrix into the `(origin,
/// direction)` pair [`crate::scene::Scene::raycast`] expects. The direction is
/// normalized so a matrix carrying scale still yields a unit ray.
#[must_use]
pub fn ray_from_matrix(m: Mat4) -> (Vec3, Vec3) {
    let origin = m.transform_point3(Vec3::ZERO);
    let dir = m.transform_vector3(Vec3::NEG_Z).normalize();
    (origin, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};

    const EPS: f32 = 1e-5;

    #[test]
    fn offset_is_identity_when_target_sits_at_the_frame() {
        let f = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let off = relative_transform(f, f);
        assert!(off.abs_diff_eq(Mat4::IDENTITY, EPS), "{off:?}");
    }

    #[test]
    fn target_translates_with_the_frame() {
        // Capture with the frame at the origin, target 1 unit along -Z (in front).
        let frame_at_capture = Mat4::IDENTITY;
        let target_world = Mat4::from_translation(Vec3::new(0.0, 0.0, -1.0));
        let offset = relative_transform(frame_at_capture, target_world);

        // Frame slides +2 on X. With a root target (identity parent) the local
        // transform IS the desired world transform: the target rides along.
        let frame_now = Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0));
        let local = local_in_parent(frame_now, offset, Mat4::IDENTITY);
        let p = local.transform_point3(Vec3::ZERO);
        assert!((p - Vec3::new(2.0, 0.0, -1.0)).length() < EPS, "p = {p:?}");
    }

    #[test]
    fn target_orbits_when_the_frame_rotates() {
        let offset =
            relative_transform(Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, 0.0, -1.0)));
        // Frame yaws +90° about +Y: its forward (-Z) swings to -X, so the rigidly
        // held target swings to (-1, 0, 0).
        let frame_now = Mat4::from_quat(Quat::from_rotation_y(core::f32::consts::FRAC_PI_2));
        let local = local_in_parent(frame_now, offset, Mat4::IDENTITY);
        let p = local.transform_point3(Vec3::ZERO);
        assert!((p - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-4, "p = {p:?}");
    }

    #[test]
    fn parent_transform_is_divided_out() {
        // A moved frame with a non-trivial parent on the target: the authored
        // LOCAL must compose back up through the parent to the intended world.
        let offset = Mat4::IDENTITY; // target coincident with the frame at capture
        let frame_now = Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0));
        let parent_world = Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0));
        let local = local_in_parent(frame_now, offset, parent_world);
        let world = parent_world * local;
        let p = world.transform_point3(Vec3::ZERO);
        assert!((p - Vec3::new(1.0, 0.0, 0.0)).length() < EPS, "p = {p:?}");
    }

    #[test]
    fn ray_from_matrix_reads_origin_and_forward() {
        // Translate to (0,1,0) and yaw 90° about +Y: forward (-Z) rotates to -X.
        let m = Mat4::from_rotation_translation(
            Quat::from_rotation_y(core::f32::consts::FRAC_PI_2),
            Vec3::new(0.0, 1.0, 0.0),
        );
        let (origin, dir) = ray_from_matrix(m);
        assert!((origin - Vec3::new(0.0, 1.0, 0.0)).length() < EPS, "origin = {origin:?}");
        assert!((dir - Vec3::new(-1.0, 0.0, 0.0)).length() < EPS, "dir = {dir:?}");
        assert!((dir.length() - 1.0).abs() < EPS, "dir not unit: {dir:?}");
    }
}
