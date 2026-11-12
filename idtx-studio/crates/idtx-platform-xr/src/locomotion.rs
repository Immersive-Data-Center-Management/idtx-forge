//! Locomotion primitives.
//!
//! Locomotion moves the whole player rig by mutating the [`Pawn`]'s
//! `stage_to_world` — never by moving the HMD or controllers directly,
//! so head + hands stay rigid. This module is the **pure math** behind the
//! built-in locomotion behaviours (snap-turn, smooth-move, teleport); a WASM
//! guest (or the runtime input router) consumes logical actions and calls into
//! a [`Locomotor`], then pushes the resulting pawn onto the platform via
//! [`crate::XrPlatform::set_pawn`].
//!
//! The [`Locomotor`] tracks the play-space **origin** (world metres) and
//! **yaw** (radians about world +Y) independently, because that is the natural
//! state for the two dominant comfort schemes: discrete snap-turns rotate the
//! yaw; smooth thumbstick locomotion translates the origin in the facing frame;
//! teleport sets the origin outright. [`Locomotor::pawn`] composes them back
//! into a single `stage_to_world` (`translate(origin) · rotateY(yaw)`), matching
//! [`Pawn::from_origin_yaw`].

use glam::{Vec2, Vec3};

use crate::pawn::Pawn;

/// Which way a discrete snap-turn rotates the play space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDirection {
    /// Counter-clockwise about world +Y (a **left** turn: +yaw).
    Left,
    /// Clockwise about world +Y (a **right** turn: −yaw).
    Right,
}

impl TurnDirection {
    /// The signed multiplier applied to a snap-turn step (+1 left, −1 right),
    /// following glam's right-handed +Y-up convention.
    #[must_use]
    pub const fn sign(self) -> f32 {
        match self {
            TurnDirection::Left => 1.0,
            TurnDirection::Right => -1.0,
        }
    }
}

/// Snap-turn tuning: the discrete yaw step applied per turn action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapTurnConfig {
    /// Yaw applied per snap, radians. Default 45°.
    pub step_radians: f32,
}

impl Default for SnapTurnConfig {
    fn default() -> Self {
        Self {
            step_radians: std::f32::consts::FRAC_PI_4,
        }
    }
}

/// Smooth-move tuning: thumbstick locomotion speed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmoothMoveConfig {
    /// Metres per second at full stick deflection. Default 1.5 m/s.
    pub speed_mps: f32,
}

impl Default for SmoothMoveConfig {
    fn default() -> Self {
        Self { speed_mps: 1.5 }
    }
}

/// The player's locomotion state: play-space origin + yaw, applied uniformly to
/// every XR pose through the composed [`Pawn`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Locomotor {
    origin: Vec3,
    yaw: f32,
}

impl Default for Locomotor {
    fn default() -> Self {
        Self::AT_ORIGIN
    }
}

impl Locomotor {
    /// The player at the stage origin, no yaw — equivalent to [`Pawn::IDENTITY`].
    pub const AT_ORIGIN: Self = Self {
        origin: Vec3::ZERO,
        yaw: 0.0,
    };

    /// A locomotor seeded at an explicit world origin + yaw.
    #[must_use]
    pub const fn new(origin: Vec3, yaw: f32) -> Self {
        Self { origin, yaw }
    }

    /// The current play-space origin (world metres).
    #[must_use]
    pub const fn origin(&self) -> Vec3 {
        self.origin
    }

    /// The current yaw (radians about world +Y).
    #[must_use]
    pub const fn yaw(&self) -> f32 {
        self.yaw
    }

    /// Compose the current state into the pawn applied to XR poses.
    #[must_use]
    pub fn pawn(&self) -> Pawn {
        Pawn::from_origin_yaw(self.origin, self.yaw)
    }

    /// Apply one discrete **snap-turn** in `dir` using `cfg.step_radians`.
    /// Rotates the play space about world +Y without translating it — the
    /// dominant comfort turn (no continuous rotation nausea).
    pub fn snap_turn(&mut self, dir: TurnDirection, cfg: SnapTurnConfig) {
        self.yaw += dir.sign() * cfg.step_radians;
    }

    /// Integrate **smooth thumbstick locomotion** over `dt` seconds.
    ///
    /// `move_axis` is the raw stick (`x` = strafe +right, `y` = forward +away),
    /// with magnitude ≤ 1. `facing_yaw` is the world-space yaw the player is
    /// looking along (typically pawn yaw + the HMD's in-stage yaw), so movement
    /// is head-relative. The horizontal move is rotated into that frame and
    /// scaled by `cfg.speed_mps · dt`; height is never changed here.
    pub fn smooth_move(
        &mut self,
        move_axis: Vec2,
        facing_yaw: f32,
        dt: f32,
        cfg: SmoothMoveConfig,
    ) {
        if move_axis == Vec2::ZERO || dt <= 0.0 {
            return;
        }
        // Clamp the stick magnitude so an out-of-range value can't over-speed.
        let stick = if move_axis.length() > 1.0 {
            move_axis.normalize()
        } else {
            move_axis
        };
        // Head-relative basis on the ground plane: forward = (sin, 0, -cos) for
        // a +Y-up right-handed yaw (yaw 0 looks toward -Z, glam's convention);
        // right = (cos, 0, sin).
        let (sin, cos) = facing_yaw.sin_cos();
        let forward = Vec3::new(sin, 0.0, -cos);
        let right = Vec3::new(cos, 0.0, sin);
        let delta = (right * stick.x + forward * stick.y) * (cfg.speed_mps * dt);
        self.origin += delta;
    }

    /// **Teleport** the play space so the player stands at `target` (world
    /// metres). Sets the origin outright; yaw is preserved.
    pub fn teleport_to(&mut self, target: Vec3) {
        self.origin = target;
    }
}

/// Sample a ballistic **teleport arc** from `start` along `dir` (need not be
/// normalized) at initial `speed` (m/s) under `gravity` (m/s², positive =
/// downward pull on +Y-up), returning up to `max_points` world-space points
/// until the arc first crosses `ground_y` (inclusive of the landing point).
///
/// This is the pointer arc a teleport behaviour visualizes (rendered by the
/// overlay pass) and whose landing point feeds [`Locomotor::teleport_to`].
/// Pure and allocation-bounded so it unit-tests without a render surface.
#[must_use]
pub fn teleport_arc(
    start: Vec3,
    dir: Vec3,
    speed: f32,
    gravity: f32,
    ground_y: f32,
    dt: f32,
    max_points: usize,
) -> Vec<Vec3> {
    let mut points = Vec::new();
    if max_points == 0 {
        return points;
    }
    let v0 = dir.normalize_or_zero() * speed;
    let mut prev = start;
    points.push(prev);
    let mut t = 0.0_f32;
    for _ in 1..max_points {
        t += dt;
        // p = start + v0·t − ½·g·t²·ŷ
        let p = start + v0 * t - Vec3::Y * (0.5 * gravity * t * t);
        // Landing: the segment prev→p crosses the ground plane.
        if p.y <= ground_y && prev.y > ground_y {
            let span = prev.y - p.y;
            let frac = if span > f32::EPSILON {
                (prev.y - ground_y) / span
            } else {
                0.0
            };
            points.push(prev.lerp(p, frac.clamp(0.0, 1.0)));
            break;
        }
        points.push(p);
        prev = p;
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

    #[test]
    fn default_locomotor_is_identity_pawn() {
        assert_eq!(Locomotor::default().pawn(), Pawn::IDENTITY);
    }

    #[test]
    fn snap_turn_accumulates_signed_steps() {
        let cfg = SnapTurnConfig {
            step_radians: FRAC_PI_4,
        };
        let mut loc = Locomotor::AT_ORIGIN;
        loc.snap_turn(TurnDirection::Left, cfg);
        assert!((loc.yaw() - FRAC_PI_4).abs() < 1e-6);
        loc.snap_turn(TurnDirection::Left, cfg);
        assert!((loc.yaw() - FRAC_PI_2).abs() < 1e-6);
        loc.snap_turn(TurnDirection::Right, cfg);
        assert!((loc.yaw() - FRAC_PI_4).abs() < 1e-6);
        // Origin is untouched by turns.
        assert_eq!(loc.origin(), Vec3::ZERO);
    }

    #[test]
    fn smooth_move_forward_is_head_relative() {
        // Facing yaw 0 → forward is -Z (glam convention). Full forward stick.
        let mut loc = Locomotor::AT_ORIGIN;
        let cfg = SmoothMoveConfig { speed_mps: 2.0 };
        loc.smooth_move(Vec2::new(0.0, 1.0), 0.0, 0.5, cfg);
        // 2 m/s · 0.5 s = 1 m along -Z.
        assert!((loc.origin() - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5);
        // Height never changes.
        assert!(loc.origin().y.abs() < 1e-6);
    }

    #[test]
    fn smooth_move_respects_facing_yaw() {
        // +90° yaw sends "forward" (-Z at yaw 0) toward -X? Check: forward =
        // (sin, 0, -cos); at yaw=+90°, sin=1, cos=0 → (1,0,0) = +X.
        let mut loc = Locomotor::AT_ORIGIN;
        let cfg = SmoothMoveConfig { speed_mps: 1.0 };
        loc.smooth_move(Vec2::new(0.0, 1.0), FRAC_PI_2, 1.0, cfg);
        assert!((loc.origin() - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn smooth_move_clamps_overlong_stick() {
        let mut a = Locomotor::AT_ORIGIN;
        let mut b = Locomotor::AT_ORIGIN;
        let cfg = SmoothMoveConfig { speed_mps: 1.0 };
        // An over-range stick must move no farther than a unit stick.
        a.smooth_move(Vec2::new(0.0, 5.0), 0.0, 1.0, cfg);
        b.smooth_move(Vec2::new(0.0, 1.0), 0.0, 1.0, cfg);
        assert!((a.origin() - b.origin()).length() < 1e-5);
    }

    #[test]
    fn smooth_move_ignores_zero_input_and_nonpositive_dt() {
        let mut loc = Locomotor::AT_ORIGIN;
        let cfg = SmoothMoveConfig::default();
        loc.smooth_move(Vec2::ZERO, 0.0, 1.0, cfg);
        loc.smooth_move(Vec2::new(0.0, 1.0), 0.0, 0.0, cfg);
        loc.smooth_move(Vec2::new(0.0, 1.0), 0.0, -1.0, cfg);
        assert_eq!(loc.origin(), Vec3::ZERO);
    }

    #[test]
    fn teleport_sets_origin_preserving_yaw() {
        let mut loc = Locomotor::new(Vec3::ZERO, FRAC_PI_4);
        loc.teleport_to(Vec3::new(3.0, 0.0, -4.0));
        assert_eq!(loc.origin(), Vec3::new(3.0, 0.0, -4.0));
        assert!((loc.yaw() - FRAC_PI_4).abs() < 1e-6);
        // The composed pawn now places a stage-origin pose at the target.
        let p = loc.pawn().stage_to_world().transform_point3(Vec3::ZERO);
        assert!((p - Vec3::new(3.0, 0.0, -4.0)).length() < 1e-5);
    }

    #[test]
    fn teleport_arc_starts_at_origin_and_lands_on_ground() {
        // Shoot forward+up from 1.5 m; it must arc down and land at y≈0.
        let arc = teleport_arc(
            Vec3::new(0.0, 1.5, 0.0),
            Vec3::new(0.0, 0.5, -1.0),
            6.0,
            9.81,
            0.0,
            0.05,
            256,
        );
        assert!(arc.len() >= 2);
        assert_eq!(arc[0], Vec3::new(0.0, 1.5, 0.0));
        let landing = *arc.last().unwrap();
        assert!(landing.y.abs() < 1e-3, "landing near ground: {landing:?}");
        // Travelled forward (−Z).
        assert!(landing.z < 0.0);
    }

    #[test]
    fn teleport_arc_respects_max_points() {
        // A near-flat, fast shot that never reaches the ground within the cap
        // returns exactly max_points samples.
        let arc = teleport_arc(
            Vec3::new(0.0, 1.5, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            50.0,
            0.0,
            0.0,
            0.05,
            8,
        );
        assert_eq!(arc.len(), 8);
    }
}
