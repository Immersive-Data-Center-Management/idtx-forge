//! `UsdPhysicsScene` parameter resolution: gravity, units, and the fixed-step
//! configuration.

use glam::Vec3;

/// Stage up-axis (`UsdGeomGetStageUpAxis`), used to default the gravity
/// direction when the scene authors none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UpAxis {
    X,
    #[default]
    Y,
    Z,
}

impl UpAxis {
    /// The up unit vector.
    #[must_use]
    pub fn vec(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }
}

/// Standard earth gravity magnitude in metres per second².
pub const EARTH_GRAVITY: f32 = 9.81;

/// Resolve the world-space gravity acceleration from the authored
/// `UsdPhysicsScene` `physics:gravityDirection` × `physics:gravityMagnitude`,
/// following the spec sentinels (plan §4):
///
/// - a zero `direction` ⇒ along `-up_axis`;
/// - a negative `magnitude` (the "unset" sentinel) ⇒ earth gravity
///   (`9.81 / meters_per_unit`, so the acceleration is correct in stage units).
///
/// `meters_per_unit` scales the earth-gravity default into stage units; an
/// explicit positive magnitude is assumed already in stage units/s² and is used
/// verbatim.
#[must_use]
pub fn resolve_gravity(
    direction: Vec3,
    magnitude: f32,
    up_axis: UpAxis,
    meters_per_unit: f32,
) -> Vec3 {
    let dir = {
        let n = direction.normalize_or_zero();
        if n == Vec3::ZERO { -up_axis.vec() } else { n }
    };
    let mpu = if meters_per_unit > 0.0 { meters_per_unit } else { 1.0 };
    let mag = if magnitude < 0.0 { EARTH_GRAVITY / mpu } else { magnitude };
    dir * mag
}

/// Resolved per-stage simulation parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneParams {
    /// World-space gravity acceleration (stage units/s²).
    pub gravity: Vec3,
    /// Stage `metersPerUnit` — scales solver length tolerances.
    pub meters_per_unit: f32,
    /// Fixed simulation timestep (seconds). Default `1/60`.
    pub dt: f32,
    /// Maximum sub-steps run per rendered frame — the spiral-of-death clamp
    /// (plan §6.2 / risk 9). A frame hitch never queues unbounded catch-up steps.
    pub max_substeps: u32,
    /// Global linear velocity damping applied to every dynamic body (fraction of
    /// linear velocity bled off per second). A small non-zero value lets a body
    /// that has all-but-stopped actually reach rest and cross rapier's sleep
    /// threshold instead of coasting indefinitely on solver noise.
    pub linear_damping: f32,
    /// Global angular velocity damping applied to every dynamic body. This is the
    /// key cure for a convex body (e.g. the cookie's generated hull) *rocking* on
    /// the ground: a slightly-uneven resting face leaves a tiny residual angular
    /// velocity that the solver keeps re-exciting, so the body never sleeps and
    /// visibly jitters. Bleeding angular velocity lets it settle and sleep. Kept
    /// modest so a deliberate throw still spins.
    pub angular_damping: f32,
}

impl Default for SceneParams {
    fn default() -> Self {
        Self {
            gravity: Vec3::new(0.0, -EARTH_GRAVITY, 0.0),
            meters_per_unit: 1.0,
            dt: 1.0 / 60.0,
            max_substeps: 8,
            linear_damping: 0.1,
            angular_damping: 0.6,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn zero_direction_defaults_to_negative_up() {
        // Y-up, unset magnitude ⇒ (0, -9.81, 0).
        let g = resolve_gravity(Vec3::ZERO, -1.0, UpAxis::Y, 1.0);
        assert!(close(g, Vec3::new(0.0, -EARTH_GRAVITY, 0.0)));
        // Z-up ⇒ along -Z.
        let g = resolve_gravity(Vec3::ZERO, -1.0, UpAxis::Z, 1.0);
        assert!(close(g, Vec3::new(0.0, 0.0, -EARTH_GRAVITY)));
    }

    #[test]
    fn negative_magnitude_scales_by_meters_per_unit() {
        // In centimetres (mpu = 0.01), earth gravity is 981 units/s² (relative
        // tolerance — 9.81/0.01 is not bit-exact in f32).
        let g = resolve_gravity(Vec3::ZERO, -1.0, UpAxis::Y, 0.01);
        assert!((g - Vec3::new(0.0, -981.0, 0.0)).length() < 0.01, "{g:?}");
    }

    #[test]
    fn explicit_direction_and_magnitude_used_verbatim() {
        // A non-normalized authored direction is normalized; magnitude is literal.
        let g = resolve_gravity(Vec3::new(0.0, 0.0, -2.0), 20.0, UpAxis::Y, 1.0);
        assert!(close(g, Vec3::new(0.0, 0.0, -20.0)));
    }

    #[test]
    fn default_params_are_earth_y_down_60hz() {
        let p = SceneParams::default();
        assert!(close(p.gravity, Vec3::new(0.0, -EARTH_GRAVITY, 0.0)));
        assert!((p.dt - 1.0 / 60.0).abs() < 1e-9);
        assert!(p.max_substeps >= 1);
    }
}
