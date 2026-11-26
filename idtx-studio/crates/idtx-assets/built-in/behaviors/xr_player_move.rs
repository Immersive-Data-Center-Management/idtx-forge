// xr_player_move — gaze-relative stick locomotion + snap-turn + FIST-teleport for
// an XR pawn.
//
// Attached to `/World/Player`. Reads the `move` 2D action (left thumbstick) and
// `turn` axis (right thumbstick X) plus the HMD pose, slides the pawn along the
// headset look direction projected onto the ground, applies 45° snap-turns, and
// authors the composed translation + yaw through the `stage_edit` seam.
//
// # Hand-gesture teleport (FIST variant)
//
// Aim by making a FIST — all fingers except the thumb curled (bound to
// `Gesture/fist/<hand>` via the `teleport` action). The arc follows the hand's
// forward direction (wrist → middle-finger knuckle). Commit "the same way" as a
// trigger: raise the thumb UP to ARM, then snap it back DOWN to FIRE; opening the
// fist cancels without teleporting. A sibling variant, `xr_player_move_point.rs`,
// keeps the previous point-to-aim gesture — switch between them by changing the
// rig's `idtx:behavior:source`.
//
// Only the snap yaw is authored on the root; HMD orientation lives on the child
// camera (`hmd_camera_sync`). Capabilities: `input` + `xr_hmd` + `stage_edit` +
// `scene_query` (arc cast) + `overlay` (draw the arc + marker).
//
// This file is loaded as a normal Rust module by the generated build skeleton.

use idtx_behavior_sdk::prelude::*;

// --- Tunables --------------------------------------------------------------

/// Metres per second at full stick deflection.
const MOVE_SPEED: f32 = 3.0;
/// Yaw applied per discrete snap-turn, radians (45°).
const SNAP_STEP: f32 = core::f32::consts::FRAC_PI_4;
/// `turn` deflection past which a flick fires one snap.
const SNAP_ON: f32 = 0.6;
/// `turn` deflection the stick must fall back under before another snap can fire.
const SNAP_OFF: f32 = 0.3;

// --- Hand-gesture teleport (fist to aim, thumb-snap to fire) ------------------

/// Launch speed of the teleport aim arc (stage units / second).
const TELEPORT_SPEED: f32 = 8.0;
/// Downward pull on the aim arc (m/s²).
const TELEPORT_GRAVITY: f32 = 9.81;
/// Maximum flight time the aim arc is sampled over (seconds).
const TELEPORT_MAX_TIME: f32 = 2.0;
/// Maximum teleport distance (stage units); arcs beyond this do not land.
const TELEPORT_REACH: f32 = 15.0;
/// Number of polyline samples used for the aim arc (buffer size).
const TELEPORT_ARC_SAMPLES: usize = 24;
/// Radius of the floor landing marker (metres).
const TELEPORT_DISC_RADIUS: f32 = 0.3;
/// Line width of the drawn arc (metres).
const TELEPORT_ARC_WIDTH: f32 = 0.04;
/// Debug cue: draw a short palm-forward pointer stub + ring for each tracked hand
/// every frame (cyan idle, green while aiming), so hand tracking and the aim
/// direction are visible on-device. Flip to `false` to disable the cue.
const SHOW_POINTER_DEBUG: bool = true;
/// Length of the debug pointer stub (metres).
const POINTER_STUB_LEN: f32 = 0.15;

/// Fist test: every non-thumb fingertip must be within this of the palm centre to
/// count as curled (matches the recogniser's `curl_distance`).
const FINGER_CURL: f32 = 0.12;
/// Thumb-snap ARM: the thumb tip must be at least this far (metres) from the
/// index base knuckle to count as "raised" (thumb up/away from the fist), cocking
/// the commit. Hand-RELATIVE (a distance between joints), so it works at any hand
/// tilt — unlike a world-up thumb-height test, which reads "up" forever when the
/// forearm is raised.
const THUMB_ARM_GAP: f32 = 0.07;
/// Thumb-snap FIRE: once armed, the commit fires when the thumb tip snaps back to
/// within this of the index knuckle (thumb resting on the fist). The gap to
/// `THUMB_ARM_GAP` is the hysteresis for one deliberate up-then-down click.
const THUMB_FIRE_GAP: f32 = 0.05;
/// Per-hand aim correction: when `true`, the LEFT hand's aim direction is negated.
/// The geometric forward (wrist → knuckle) is handedness-symmetric — joint
/// POSITIONS are not mirrored between hands (only orientations are) — so this
/// defaults OFF and both hands should aim the same way. Flip it `true` only if a
/// device reports the left hand aiming opposite the right (e.g. straight up while
/// the right aims down); the earlier inversion came from the index-finger aim,
/// which this fist variant no longer uses.
const INVERT_LEFT_AIM: bool = false;

// Joint ordinals not exported by the SDK prelude (prelude has PALM=0, WRIST=1,
// THUMB_TIP=5, INDEX_PROXIMAL=7, INDEX_TIP=10).
/// Middle-finger metacarpal (knuckle) — `this - wrist` is the hand's forward axis.
const HAND_JOINT_MIDDLE_METACARPAL: usize = 11;
/// Non-thumb fingertips, for the fist test.
const HAND_JOINT_MIDDLE_TIP: usize = 15;
const HAND_JOINT_RING_TIP: usize = 20;
const HAND_JOINT_LITTLE_TIP: usize = 25;

/// Translating + snap-rotating its prim in response to the two thumbsticks, with a
/// fist-aim + thumb-snap teleport.
// Most of the bools are `SHOW_POINTER_DEBUG`-gated edge trackers (`dbg_*`), not
// real state, so the "too many bools" heuristic does not apply here.
#[allow(clippy::struct_excessive_bools)]
pub struct XrPlayerMove {
    /// Accumulated world position (locomotion).
    pos: Vec3,
    /// Accumulated snap-turn yaw (radians about world +Y).
    yaw: f32,
    /// Snap re-arm latch: `true` when the `turn` stick has returned near centre.
    turn_armed: bool,
    /// Last valid teleport landing (world space) seen while aiming; consumed on
    /// commit.
    last_landing: Option<Vec3>,
    /// Thumb-snap state: `Some(side)` once that hand's thumb has been raised while
    /// fisting; the commit fires on its snap back down. Per side so a left-hand arm
    /// can't be fired by the right hand's thumb.
    armed_side: Option<XrSide>,
    /// Whether the current aim comes from a tracked hand (thumb-snap commit) vs a
    /// controller/aim-pose (release-to-commit).
    aim_mode_hand: bool,
    /// Debug (gated by `SHOW_POINTER_DEBUG`): one-time binding check + held/hit edge
    /// trackers so the device log gets one line per state change.
    dbg_checked: bool,
    dbg_prev_held: bool,
    dbg_prev_hit: bool,
}

impl Default for XrPlayerMove {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            yaw: 0.0,
            turn_armed: true,
            last_landing: None,
            armed_side: None,
            aim_mode_hand: false,
            dbg_checked: false,
            dbg_prev_held: false,
            dbg_prev_hit: false,
        }
    }
}

#[behavior(kind = "runtime")]
impl XrPlayerMove {
    fn on_update(&mut self, ctx: &Ctx, dt: f32) -> Result<()> {
        let (strafe, forward) = input::axis2("move");
        let turn = input::axis("turn");

        let (yaw, armed) = snap_turn_step(self.yaw, self.turn_armed, turn);
        self.yaw = yaw;
        self.turn_armed = armed;

        if let Some(hmd) = ctx.xr().get_pose(XrSpace::Hmd) {
            self.pos += move_delta(hmd.rotation, self.yaw, strafe, forward, MOVE_SPEED, dt);
        }

        // Fist-aim teleport: aim while fisting (draws the arc + marker), thumb-snap
        // to commit. Runs after smooth-move and before the authoring write so a
        // committed teleport takes effect this frame.
        self.update_teleport(ctx)?;

        ctx.stage_edit().set_self_transform(&Transform {
            translation: self.pos + recenter_translation(self.yaw, ctx.xr().recenter()),
            rotation: Quat::from_rotation_y(self.yaw),
            scale: Vec3::ONE,
        })?;
        Ok(())
    }
}

// --- Teleport (reads ctx; kept off the #[behavior] impl so the macro only sees
//     the lifecycle hook) -----------------------------------------------------

impl XrPlayerMove {
    // Fist to aim, thumb-up to arm, thumb-snap-down to fire; opening the fist
    // cancels. While the `teleport` (fist) action is held, cast + draw a ballistic
    // arc along the hand's forward direction.
    fn update_teleport(&mut self, ctx: &Ctx) -> Result<()> {
        let teleport = input::action("teleport");
        let pawn = Pawn::new(self.yaw, self.pos, ctx.xr().recenter());

        // Detect the aiming hand from the JOINTS, here in the behaviour — the FIST
        // pose (all four non-thumb fingertips curled), NOT the host `Gesture/*`
        // action. The action also fires on a point/other gesture and must not gate
        // hand aiming, or a point leaks in. The controller fallback uses the
        // `teleport` action ONLY when no hand is tracked at all. Fetch each hand's
        // joints once and reuse.
        let right = ctx.xr().hand_joints(XrSide::Right);
        let left = ctx.xr().hand_joints(XrSide::Left);
        let hand = fisting_hand(right, left);
        let any_hand = right.is_some() || left.is_some();
        let controller_aim = !any_hand && teleport.held();
        let aiming = hand.is_some() || controller_aim;

        if SHOW_POINTER_DEBUG {
            if !self.dbg_checked {
                self.dbg_checked = true;
                if teleport.id() == InputActionId::UNRESOLVED {
                    log::warn(
                        "xr_player_move: 'teleport' action UNBOUND (controller fallback \
                         only) — hand aiming is self-detected from the fist pose and does \
                         not need it.",
                    );
                } else {
                    log::info("xr_player_move: 'teleport' action bound (controller fallback)");
                }
            }
            if aiming != self.dbg_prev_held {
                self.dbg_prev_held = aiming;
                if aiming {
                    log::info("xr_player_move: teleport aiming STARTED (fist self-detected)");
                } else {
                    log::info("xr_player_move: teleport aiming ended");
                }
            }
        }

        draw_hand_pointers(ctx, &pawn, aiming)?;

        if !aiming {
            // Not aiming. Hands only commit via the thumb-snap (never on fist-open);
            // the controller aim commits on release.
            if !self.aim_mode_hand && teleport.released() {
                self.commit_teleport(ctx, &pawn);
            } else if SHOW_POINTER_DEBUG && (self.armed_side.is_some() || self.last_landing.is_some())
            {
                log::info("xr_player_move: teleport cancelled (fist opened before thumb-snap)");
            }
            self.armed_side = None;
            self.last_landing = None;
            self.aim_mode_hand = false;
            return Ok(());
        }

        self.aim_mode_hand = hand.is_some();
        let cur_side = hand.map(|(side, _)| side);
        // A change of aiming hand disarms — arm and fire must be the same hand.
        if self.armed_side.is_some() && self.armed_side != cur_side {
            self.armed_side = None;
        }

        let aim_play = match hand {
            Some((side, j)) => fist_aim_ray(&j).map(|(o, d)| (o, aim_invert(d, side))),
            None => aim_pose_ray(ctx),
        };
        let Some((origin_play, dir_play)) = aim_play else {
            self.last_landing = None;
            return Ok(());
        };
        let origin = pawn.point(origin_play);
        let dir = pawn.dir(dir_play);

        let launch = ArcLaunch {
            origin,
            dir,
            speed: TELEPORT_SPEED,
            gravity: TELEPORT_GRAVITY,
            max_time: TELEPORT_MAX_TIME,
        };
        let opts = RayOpts::new()
            .require_collidable()
            .physics_only()
            .max_distance(TELEPORT_REACH);
        let mut pts = [Vec3::ZERO; TELEPORT_ARC_SAMPLES];
        let hit = ctx.scene().arc_cast(&launch, &opts, &mut pts);
        if SHOW_POINTER_DEBUG && hit.hit != self.dbg_prev_hit {
            self.dbg_prev_hit = hit.hit;
            if hit.hit {
                log::info("xr_player_move: teleport target found (arc blue; green once armed)");
            } else {
                log::warn(
                    "xr_player_move: teleport NO target (arc red) — the aim missed a \
                     physics collider within reach.",
                );
            }
        }

        // Thumb "gap": distance from the thumb tip to the index knuckle. Large when
        // the thumb is raised away from the fist, small when it snaps down to rest
        // on it. Hand-relative — no world-up dependence, so it works at any tilt.
        let thumb_g = hand.and_then(|(_, j)| thumb_index_gap(&j));

        // ARM when the aiming hand's thumb is raised away (gap large), before
        // colouring so the arc turns green the moment it arms. Keyed per side.
        if let (Some((side, _)), Some(g)) = (hand, thumb_g) {
            if g > THUMB_ARM_GAP {
                if SHOW_POINTER_DEBUG && self.armed_side != Some(side) {
                    log::info("xr_player_move: teleport ARMED (thumb up) — snap it down to fire");
                }
                self.armed_side = Some(side);
            }
        }
        let armed = cur_side.is_some() && self.armed_side == cur_side;

        // Arc colour: red = no target, blue = target found but not armed, green =
        // armed (ready to fire on the thumb snap-down).
        let color = if !hit.hit {
            Vec4::new(0.9, 0.3, 0.2, 1.0)
        } else if armed {
            Vec4::new(0.2, 0.9, 0.35, 1.0)
        } else {
            Vec4::new(0.25, 0.5, 0.95, 1.0)
        };
        let draw = ctx.draw();
        draw_arc_to_hit(draw, &pts[..hit.point_count], &hit, color)?;
        if hit.hit {
            draw.disc(hit.position, Vec3::Y, TELEPORT_DISC_RADIUS, color)?;
            self.last_landing = Some(hit.position);
        } else {
            self.last_landing = None;
        }

        // FIRE: the armed hand snaps the thumb back down to rest on the fist (gap
        // small). Hand-relative, so it fires regardless of hand tilt.
        if let (Some((side, _)), Some(g)) = (hand, thumb_g) {
            if self.armed_side == Some(side) && g < THUMB_FIRE_GAP {
                self.commit_teleport(ctx, &pawn);
                self.armed_side = None;
            }
        }
        Ok(())
    }

    // Commit the pending teleport: move the pawn so the player stands over the last
    // valid landing (world space). No-op without a landing or a tracked head pose.
    fn commit_teleport(&mut self, ctx: &Ctx, pawn: &Pawn) {
        let Some(landing) = self.last_landing.take() else {
            return;
        };
        let Some(head) = ctx
            .xr()
            .get_pose(XrSpace::Hmd)
            .map(|p| pawn.point(p.translation))
        else {
            return;
        };
        self.pos = teleport_commit(self.pos, head, landing);
        if SHOW_POINTER_DEBUG {
            log::info("xr_player_move: teleport COMMITTED");
        }
    }
}

// --- Pure math (unit-testable natively) ------------------------------------

/// Project a vector onto the ground plane (drop height) and normalise it.
fn flatten_ground(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z).normalize_or_zero()
}

/// The play-space recenter translation to add to the pawn origin (`R(yaw)·r`).
fn recenter_translation(yaw: f32, r: Vec3) -> Vec3 {
    Quat::from_rotation_y(yaw) * r
}

/// The per-frame world-space translation for the given HMD orientation, snap yaw,
/// and stick inputs — gaze-relative, flattened to the ground, rotated by yaw.
fn move_delta(hmd_rot: Quat, yaw: f32, strafe: f32, forward: f32, speed: f32, dt: f32) -> Vec3 {
    let turn = Quat::from_rotation_y(yaw);
    let fwd = turn * flatten_ground(hmd_rot * Vec3::NEG_Z);
    let right = turn * flatten_ground(hmd_rot * Vec3::X);
    (fwd * forward + right * strafe) * (speed * dt)
}

/// Advance the snap-turn state machine for one frame. One flick = one 45° snap.
fn snap_turn_step(yaw: f32, armed: bool, turn: f32) -> (f32, bool) {
    if armed && turn.abs() >= SNAP_ON {
        let dir = -turn.signum();
        (yaw + dir * SNAP_STEP, false)
    } else if !armed && turn.abs() <= SNAP_OFF {
        (yaw, true)
    } else {
        (yaw, armed)
    }
}

/// The pawn's play→world transform (`R(yaw)·(p + recenter) + pos`, mirroring how
/// the pawn root is authored; the recenter cancels).
#[derive(Clone, Copy)]
struct Pawn {
    rot: Quat,
    pos: Vec3,
    recenter: Vec3,
}

impl Pawn {
    fn new(yaw: f32, pos: Vec3, recenter: Vec3) -> Self {
        Self {
            rot: Quat::from_rotation_y(yaw),
            pos,
            recenter,
        }
    }
    /// A play-space point → world.
    fn point(&self, p: Vec3) -> Vec3 {
        self.rot * (p + self.recenter) + self.pos
    }
    /// A play-space direction → world (rotation only).
    fn dir(&self, d: Vec3) -> Vec3 {
        self.rot * d
    }
}

/// The hand making the TIGHTEST fist (so a deliberate fist beats a loosely-closed
/// one, and EITHER hand works — not just the right), with its 26-joint skeleton.
/// `None` when neither hand is fisting. Right wins an exact tie. Takes the already
/// fetched per-hand joints to avoid re-reading them.
fn fisting_hand(
    right: Option<HandJoints>,
    left: Option<HandJoints>,
) -> Option<(XrSide, HandJoints)> {
    let mut best: Option<(XrSide, HandJoints, f32)> = None;
    for (side, oj) in [(XrSide::Right, right), (XrSide::Left, left)] {
        let Some(j) = oj else {
            continue;
        };
        let Some(score) = fist_score(&j) else {
            continue;
        };
        let better = match best {
            Some((_, _, b)) => score < b,
            None => true,
        };
        if better {
            best = Some((side, j, score));
        }
    }
    best.map(|(side, j, _)| (side, j))
}

/// Fist "score" = total non-thumb fingertip→palm distance, or `None` when ANY of
/// the four fingertips is farther than `FINGER_CURL` from the palm (i.e. not a
/// fist — a point, where the index is extended, scores `None`). Lower = tighter.
fn fist_score(j: &HandJoints) -> Option<f32> {
    let palm = j.position(HAND_JOINT_PALM)?;
    let mut total = 0.0_f32;
    for tip in [
        HAND_JOINT_INDEX_TIP,
        HAND_JOINT_MIDDLE_TIP,
        HAND_JOINT_RING_TIP,
        HAND_JOINT_LITTLE_TIP,
    ] {
        let d = (j.position(tip)? - palm).length();
        if d >= FINGER_CURL {
            return None;
        }
        total += d;
    }
    Some(total)
}

/// The fist aim ray: origin at the palm centre, direction along the hand's forward
/// axis (wrist → middle-finger knuckle). Play space; transform with [`Pawn`].
fn fist_aim_ray(j: &HandJoints) -> Option<(Vec3, Vec3)> {
    let palm = j.position(HAND_JOINT_PALM)?;
    let dir = palm_forward(j)?;
    Some((palm, dir))
}

/// The hand's forward direction: wrist → middle-finger knuckle, normalised. This
/// is where the fist points. `None` when the joints coincide.
fn palm_forward(j: &HandJoints) -> Option<Vec3> {
    let wrist = j.position(HAND_JOINT_WRIST)?;
    let knuckle = j.position(HAND_JOINT_MIDDLE_METACARPAL)?;
    aim_direction(knuckle, wrist)
}

/// Apply the per-hand aim correction (see `INVERT_LEFT_AIM`): negate the LEFT
/// hand's aim direction when enabled, else pass through.
fn aim_invert(dir: Vec3, side: XrSide) -> Vec3 {
    if INVERT_LEFT_AIM && side == XrSide::Left {
        -dir
    } else {
        dir
    }
}

/// Hand-relative thumb-snap signal: distance from the thumb tip to the index base
/// knuckle (metres). Large when the thumb is raised/extended away from the fist,
/// small when it snaps down to rest on it. Orientation-independent (a joint-to-
/// joint distance), so it works at any hand tilt — unlike a world-up height test.
fn thumb_index_gap(j: &HandJoints) -> Option<f32> {
    let thumb = j.position(HAND_JOINT_THUMB_TIP)?;
    let knuckle = j.position(HAND_JOINT_INDEX_PROXIMAL)?;
    Some((thumb - knuckle).length())
}

/// The controller / hands-free aim pose as a ray (right aim preferred).
fn aim_pose_ray(ctx: &Ctx) -> Option<(Vec3, Vec3)> {
    for space in [XrSpace::RightAim, XrSpace::LeftAim] {
        if let Some(p) = ctx.xr().get_pose(space) {
            return Some((p.translation, p.rotation * Vec3::NEG_Z));
        }
    }
    None
}

/// Draw the arc polyline in `color`, clipped at the landing. `arc_cast` fills the
/// whole sampled parabola, so stop once the accumulated arc length reaches
/// `hit.distance` and finish exactly on `hit.position` — no overshoot past the
/// ground contact. On a miss the full arc is drawn. Points are world.
fn draw_arc_to_hit(draw: Draw, pts: &[Vec3], hit: &ArcHit, color: Vec4) -> Result<()> {
    if pts.len() < 2 {
        return Ok(());
    }
    let mut prev = pts[0];
    let mut acc = 0.0_f32;
    for &p in &pts[1..] {
        let seg = (p - prev).length();
        if hit.hit && acc + seg >= hit.distance {
            draw.line(prev, hit.position, color, TELEPORT_ARC_WIDTH)?;
            return Ok(());
        }
        draw.line(prev, p, color, TELEPORT_ARC_WIDTH)?;
        acc += seg;
        prev = p;
    }
    Ok(())
}

/// The normalized direction from `from` to `to`, or `None` when they coincide.
fn aim_direction(to: Vec3, from: Vec3) -> Option<Vec3> {
    let d = to - from;
    (d.length_squared() > 1e-8).then(|| d.normalize())
}

/// The committed pawn position after teleporting: move horizontally by the vector
/// from the head's ground position to the `landing` (height preserved — a level
/// teleport). `head` and `landing` share the recenter-compensated space, so the
/// recenter cancels; this mirrors an instantaneous smooth-move, which the pawn
/// already handles correctly.
fn teleport_commit(current: Vec3, head: Vec3, landing: Vec3) -> Vec3 {
    Vec3::new(
        current.x + (landing.x - head.x),
        current.y,
        current.z + (landing.z - head.z),
    )
}

/// Debug cue: draw a short palm-forward pointer stub + a ring at the palm for each
/// tracked hand (regardless of gesture), so hand tracking and the aim direction
/// stay visible on-device. `active` tints it green while aiming, cyan otherwise.
fn draw_hand_pointers(ctx: &Ctx, pawn: &Pawn, active: bool) -> Result<()> {
    if !SHOW_POINTER_DEBUG {
        return Ok(());
    }
    let color = if active {
        Vec4::new(0.2, 0.9, 0.35, 1.0)
    } else {
        Vec4::new(0.2, 0.7, 0.95, 0.85)
    };
    let draw = ctx.draw();
    for side in [XrSide::Right, XrSide::Left] {
        let Some(j) = ctx.xr().hand_joints(side) else {
            continue;
        };
        let (Some(palm), Some(dir)) = (j.position(HAND_JOINT_PALM), palm_forward(&j)) else {
            continue;
        };
        // Play → world so the cue follows the player, not the spawn origin, with the
        // same per-hand aim correction as the teleport ray.
        let palm_w = pawn.point(palm);
        let dir_w = pawn.dir(aim_invert(dir, side));
        draw.line(palm_w, palm_w + dir_w * POINTER_STUB_LEN, color, 0.01)?;
        draw.disc(palm_w, dir_w, 0.015, color)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    #[test]
    fn forward_at_identity_hmd_is_negative_z() {
        let d = move_delta(Quat::IDENTITY, 0.0, 0.0, 1.0, 1.0, 1.0);
        assert!(d.x.abs() < EPS && d.y.abs() < EPS && (d.z + 1.0).abs() < EPS);
    }

    #[test]
    fn strafe_at_identity_hmd_is_positive_x() {
        let d = move_delta(Quat::IDENTITY, 0.0, 1.0, 0.0, 1.0, 1.0);
        assert!((d.x - 1.0).abs() < EPS && d.y.abs() < EPS && d.z.abs() < EPS);
    }

    #[test]
    fn pitch_down_keeps_movement_level() {
        let d = move_delta(
            Quat::from_rotation_x(-core::f32::consts::FRAC_PI_4),
            0.0,
            0.0,
            1.0,
            1.0,
            1.0,
        );
        assert!(d.y.abs() < EPS);
    }

    #[test]
    fn one_flick_fires_exactly_one_snap_then_latches() {
        let (yaw1, armed1) = snap_turn_step(0.0, true, 1.0);
        assert!((yaw1 + SNAP_STEP).abs() < EPS && !armed1);
        let (yaw2, armed2) = snap_turn_step(yaw1, armed1, 1.0);
        assert!((yaw2 - yaw1).abs() < EPS && !armed2);
        let (_, armed3) = snap_turn_step(yaw2, armed2, 0.0);
        assert!(armed3);
    }

    #[test]
    fn recenter_translation_rotates_offset_into_yaw_frame() {
        assert!(recenter_translation(1.0, Vec3::ZERO).length() < EPS);
        let r = Vec3::new(0.5, 0.0, -0.3);
        assert!((recenter_translation(0.0, r) - r).length() < EPS);
        let d = recenter_translation(core::f32::consts::FRAC_PI_2, Vec3::new(1.0, 0.0, 0.0));
        assert!((d - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6, "d = {d:?}");
    }

    #[test]
    fn teleport_commit_moves_by_head_to_landing_delta() {
        // The pawn moves horizontally by (landing - head); height is preserved.
        let current = Vec3::new(10.0, 0.0, 5.0);
        let head = Vec3::new(1.0, 1.6, 2.0);
        let landing = Vec3::new(4.0, 0.0, -3.0);
        let out = teleport_commit(current, head, landing);
        assert!((out.x - 13.0).abs() < EPS, "x={}", out.x); // 10 + (4-1)
        assert!((out.z - 0.0).abs() < EPS, "z={}", out.z); // 5 + (-3-2)
        assert!(out.y.abs() < EPS, "y preserved: {}", out.y);
    }

    #[test]
    fn aim_direction_points_from_to() {
        // wrist→knuckle along -Z → forward is -Z, unit length.
        let dir = aim_direction(Vec3::new(0.0, 0.0, -1.0), Vec3::ZERO).unwrap();
        assert!((dir - Vec3::NEG_Z).length() < EPS);
        let long = aim_direction(Vec3::new(3.0, 0.0, 4.0), Vec3::ZERO).unwrap();
        assert!((long.length() - 1.0).abs() < EPS);
        assert!(aim_direction(Vec3::ONE, Vec3::ONE).is_none());
    }

    #[test]
    fn pawn_maps_play_space_into_world() {
        let p = Pawn::new(0.0, Vec3::new(10.0, 0.0, -4.0), Vec3::ZERO);
        let w = p.point(Vec3::new(1.0, 1.5, 0.0));
        assert!((w - Vec3::new(11.0, 1.5, -4.0)).length() < EPS, "{w:?}");
        assert!((p.dir(Vec3::NEG_Z) - Vec3::NEG_Z).length() < EPS);
        // +90° yaw rotates +X play dir toward -Z (glam RH, +Y up).
        let p = Pawn::new(core::f32::consts::FRAC_PI_2, Vec3::ZERO, Vec3::ZERO);
        assert!((p.dir(Vec3::X) - Vec3::NEG_Z).length() < 1e-5, "{:?}", p.dir(Vec3::X));
    }
}
