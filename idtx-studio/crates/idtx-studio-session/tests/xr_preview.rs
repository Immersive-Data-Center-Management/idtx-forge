//! Headless `OpenXR`-preview integration test (mock-xr).
//!
//! Exercises the preview wiring the shell relies on **without a USD link or
//! wasm toolchain**: a mock XR loader and the bridge-only fallback controller
//! proxies the preview spawns. It proves that
//!
//! * the preview starts against a mock loader and spawns a fallback proxy for
//!   a hand whose real prim did not author a `*_controller_sync` behavior,
//! * an injected controller pose flows platform → [`XrPreviewSession::sweep`]
//!   → the proxy's `GlobalTransform`,
//! * [`XrPreviewSession::controller_world_matrix`] tracks the driven pose and
//!   stays `None` until the first tracked pose lands.
//!
//! The camera is no longer owned by the preview: the HMD-sync behavior is
//! authored on `/World/Player/Camera` and attached by the session's
//! authored-behavior path, and real stereo builds eye views from the located
//! `OpenXR` eye poses — neither is exercisable headlessly, so both are covered
//! by the shell/session tests that hydrate a stage instead.
//!
//! Run with: `cargo test -p idtx-studio-session --features mock-xr`.
#![cfg(feature = "mock-xr")]

use idtx_behavior::ecs as bec;
use idtx_openxr_sys::XrLoader;
use idtx_platform_xr::ControllerPose;
use idtx_scene::bevy_ecs::world::World;
use idtx_studio_session::xr_preview::{XrPreviewSession, probe_xr_runtime};

#[test]
fn probe_finds_mock_runtime() {
    assert!(probe_xr_runtime().is_some(), "mock loader must be detected");
}

#[test]
fn resident_world_reentry_reuses_one_controller_pair() {
    let loader = XrLoader::try_load().unwrap();
    let mut level = World::new();
    let mut mask = World::new();
    let mut preview = XrPreviewSession::start(&loader, &mut level, [false, false]).unwrap();
    for _ in 0..5 {
        preview.reseed_controllers(&mut mask, [false, false]);
        preview.reseed_controllers(&mut level, [false, false]);
    }
    assert_eq!(level.iter_entities().filter(|entity| entity.contains::<bec::UsdPath>()).count(), 2);
    assert_eq!(mask.iter_entities().filter(|entity| entity.contains::<bec::UsdPath>()).count(), 2);
    preview.reseed_controllers(&mut mask, [true, true]);
    assert_eq!(mask.iter_entities().filter(|entity| entity.contains::<bec::UsdPath>()).count(), 0);
}

#[test]
fn controller_pose_drives_fallback_proxy() {
    let loader = XrLoader::try_load().expect("mock loader");
    let mut world = World::new();

    // Neither hand is behavior-owned → the preview spawns both fallback proxies.
    let mut session =
        XrPreviewSession::start(&loader, &mut world, [false, false]).expect("preview starts");

    let left = session
        .left_controller()
        .expect("left fallback proxy spawned")
        .entity;
    assert!(
        session.right_controller().is_some(),
        "right fallback proxy spawned"
    );

    // Before any pose the world matrix is withheld so no phantom capsule
    // renders at the origin.
    assert!(
        session.controller_world_matrix(&world, true).is_none(),
        "no pose yet → no world matrix"
    );

    // Inject a sequence of mock controller poses; the proxy transform must track.
    for expected in [[0.3_f32, 1.2, -0.5], [-0.4, 1.1, -0.6]] {
        session.platform_mut().inject_controller_poses(
            Some(ControllerPose {
                position: glam::Vec3::from_array(expected),
                orientation: glam::Quat::IDENTITY,
                timestamp: 0.0,
            }),
            None,
        );
        // The proxy runs no behavior; `sweep` seeds its `GlobalTransform`
        // directly from the platform grip pose (path 2).
        session.sweep(&mut world);

        let global = world
            .get::<bec::GlobalTransform>(left)
            .expect("left proxy has a global transform");
        let pos = global.0.w_axis.truncate();
        assert!(
            (pos - glam::Vec3::from_array(expected)).length() < 1e-4,
            "left proxy should track {expected:?}, got {pos:?}"
        );
    }

    // The tracked-pose latch now surfaces the world matrix; the right hand,
    // never injected, stays withheld.
    let mat = session
        .controller_world_matrix(&world, true)
        .expect("world matrix after pose");
    let last = glam::Vec3::new(-0.4, 1.1, -0.6);
    assert!(
        (mat.w_axis.truncate() - last).length() < 1e-4,
        "world matrix tracks the last pose"
    );
    assert!(
        session.controller_world_matrix(&world, false).is_none(),
        "uninjected right hand stays withheld"
    );
}

#[test]
fn behavior_owned_hand_suppresses_its_fallback_proxy() {
    // The single-authority invariant: when a hand's real prim already authored a
    // `*_controller_sync` behavior, the preview must NOT spawn a bridge fallback
    // proxy for it — otherwise two writers drive that hand's pose (the classic
    // double-controller bug). The `[bool; 2]` gate encodes that ownership.
    let loader = XrLoader::try_load().expect("mock loader");
    let mut world = World::new();

    // Left hand is behavior-owned; right hand is not.
    let session =
        XrPreviewSession::start(&loader, &mut world, [true, false]).expect("preview starts");

    assert!(
        session.left_controller().is_none(),
        "behavior-owned left hand must not get a fallback proxy"
    );
    assert!(
        session.right_controller().is_some(),
        "unowned right hand still spawns its fallback proxy"
    );
}
