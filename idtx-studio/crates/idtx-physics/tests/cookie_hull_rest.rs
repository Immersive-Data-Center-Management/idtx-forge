//! Regression (desktop + Quest): the `UsdCookie`'s ~285-vertex convex-hull collider
//! must settle and **sleep** when dropped on a plane at any orientation. A
//! high-vertex scanned hull generates an unstable contact manifold that keeps the
//! body jittering (angular rocking) above the sleep threshold forever — the
//! "cookie never comes to rest" symptom — and raising damping (tried up to
//! angular 0.6) does not cure it. The engine fixes this by reducing the hull to a
//! direction-sampled ≤48-vtx subset (plus a small border radius) before building
//! the parry collider.
//!
//! USD-free: the real hull points ship as a CSV fixture extracted from
//! the external InteractionDemo project's `stages/level.usda`.

use glam::{Mat4, Quat, Vec3};
use idtx_physics::{
    BodyDesc, BodyKind, ColliderDesc, PhysicsKey, PhysicsShape, PhysicsWorld, SceneParams,
};

fn cookie_hull_points() -> Vec<Vec3> {
    include_str!("data/cookie_hull_points.csv")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut it = l.split(',').map(|s| s.trim().parse::<f32>().expect("f32"));
            Vec3::new(
                it.next().expect("x"),
                it.next().expect("y"),
                it.next().expect("z"),
            )
        })
        .collect()
}

/// Drop a dynamic body with `shape` onto a static Y plane at orientation `rot`,
/// with the shipped damping. Steps up to 30 s of sim; returns
/// `(slept, steps_taken, final linvel, final angvel)`.
fn settle(shape: PhysicsShape, rot: Quat) -> (bool, u32, Vec3, Vec3) {
    let params = SceneParams {
        linear_damping: 0.1,
        angular_damping: 0.6,
        ..SceneParams::default()
    };
    let mut w = PhysicsWorld::new(params);
    let ground = (
        BodyDesc { kind: BodyKind::Static, ..BodyDesc::default() },
        vec![ColliderDesc { shape: PhysicsShape::Plane { normal: Vec3::Y }, ..ColliderDesc::default() }],
    );
    w.insert_body(PhysicsKey(1), &ground.0, &ground.1);

    let bd = BodyDesc {
        kind: BodyKind::Dynamic,
        world: Mat4::from_rotation_translation(rot, Vec3::new(0.0, 0.9, 0.0)),
        ..BodyDesc::default()
    };
    let bc = vec![ColliderDesc { shape, ..ColliderDesc::default() }];
    assert!(w.insert_body(PhysicsKey(2), &bd, &bc), "hull collider builds");

    let mut steps = 0u32;
    for _ in 0..1800 {
        w.step_once();
        steps += 1;
        if w.is_body_sleeping(PhysicsKey(2)) == Some(true) {
            break;
        }
    }
    let slept = w.is_body_sleeping(PhysicsKey(2)).unwrap();
    let (lin, ang) = w.body_velocity(PhysicsKey(2)).unwrap();
    (slept, steps, lin, ang)
}

/// Drop orientations to probe. The scene authors the cookie `RigidBody` tilted
/// ~85° about X; a scanned hull has one clean face and one bumpy face, so before
/// the fix some orientations settle while others rock forever.
fn probe_orientations() -> Vec<(&'static str, Quat)> {
    use std::f32::consts::{FRAC_PI_2, PI};
    vec![
        ("identity", Quat::IDENTITY),
        ("x-90", Quat::from_rotation_x(-FRAC_PI_2)),
        ("x+90", Quat::from_rotation_x(FRAC_PI_2)),
        ("x+85 (scene)", Quat::from_rotation_x(85.0_f32.to_radians())),
        ("x+180", Quat::from_rotation_x(PI)),
        ("x+70", Quat::from_rotation_x(70.0_f32.to_radians())),
    ]
}

#[test]
fn cookie_hull_settles_and_sleeps() {
    // The real 285-vtx cookie hull, built through the engine (which reduces it to
    // a direction-sampled ≤48-vtx hull + small border radius), must settle and
    // SLEEP at every drop orientation — including the scene's ~85° tilt. Before
    // the fix it rocked forever (|angvel|≈0.2–0.46) at identity / x+70 / x+85 /
    // x+90 despite damping.
    let points = cookie_hull_points();
    let mut stuck = Vec::new();
    for (name, rot) in probe_orientations() {
        let (slept, steps, lin, ang) =
            settle(PhysicsShape::Hull { points: points.clone() }, rot);
        eprintln!(
            "orientation {name:>14}: slept={slept} steps={steps} \
             |linvel|={:.4} |angvel|={:.4}",
            lin.length(),
            ang.length()
        );
        if !slept {
            stuck.push(name);
        }
    }
    assert!(stuck.is_empty(), "cookie hull never slept at: {stuck:?}");
}
