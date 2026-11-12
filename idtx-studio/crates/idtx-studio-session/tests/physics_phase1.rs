//! Physics integration tests — the full hydrate → step → author →
//! re-project → read-back path against the authored `tests/fixtures/usd/physics/*`
//! fixtures.
//!
//! These exercise [`StudioSession::tick_physics`]: a `UsdPhysicsScene` prim
//! activates the sim, the authored `UsdPhysics*` schemas hydrate into bodies +
//! colliders, the fixed-timestep solver advances, and each dynamic body's pose
//! is authored back into the session layer as a single `xformOp:transform`
//! matrix — read back here through the composed world transform.
//!
//! `#[ignore]` because they link the `OpenUSD` monolith (+ the usdPhysics plugin)
//! and spin up a headless wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… cargo test -p idtx-studio-session --features physics \
//!   -- --ignored physics_phase1
//! ```

#![cfg(feature = "physics")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/physics")
        .join(name)
}

/// Surfaceless wgpu device/queue for headless rendering.
fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("a headless wgpu adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-physics-phase1-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("a headless wgpu device")
    })
}

fn open(fixture_name: &str) -> StudioSession {
    open_path(&fixture(fixture_name))
}

fn open_path(path: &std::path::Path) -> StudioSession {
    // The runtime registers the IDTX codeless schemas at startup; the usdPhysics
    // schemas ship in the linked monolith, so no explicit install is needed for
    // PhysicsCollisionAPI / PhysicsRigidBodyAPI resolution. IdtxCollisionAPI
    // (hull/channel addendum) is unused by these fixtures.
    let _ = idtx_usd_extension::install();
    let (device, queue) = headless_device();
    StudioSession::open_with_gpu(
        path,
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open physics fixture")
}

const DT: f32 = 1.0 / 60.0;

fn guest_edit(session: &mut StudioSession, path: &str, op: idtx_behavior::SessionEditOp) {
    let entity = session.world().iter_entities().find(|entity|
        entity.get::<idtx_scene::PrimNode>().is_some_and(|node| node.path == path)).unwrap().id();
    session.apply_session_edit_commands(&[idtx_behavior::SessionEditCommand {
        target: idtx_behavior::entity_to_wire(entity), op,
    }]);
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn runtime_structural_edits_preserve_root_and_retire_physics() {
    let mut session = open("box_fall_plane.usda");
    let root = session.root_layer_snapshot().unwrap();
    guest_edit(&mut session, "/World", idtx_behavior::SessionEditOp::DefinePrim { type_name: "Xform".into(), name: "Transient".into() });
    assert_eq!(session.root_layer_snapshot().unwrap(), root, "guest define authored the root layer");
    assert!(session.get_world_transform("/World/Transient").is_some());
    guest_edit(&mut session, "/World/Transient", idtx_behavior::SessionEditOp::RemovePrim);
    assert_eq!(session.root_layer_snapshot().unwrap(), root);
    session.tick_physics(DT, false).unwrap();
    let count = session.physics_body_counts_for_test().unwrap().0;
    guest_edit(&mut session, "/World/Box", idtx_behavior::SessionEditOp::RemovePrim);
    session.tick_physics(DT, false).unwrap();
    assert_eq!(session.root_layer_snapshot().unwrap(), root, "weaker prim removal must use a session opinion");
    assert_eq!(session.physics_body_counts_for_test().unwrap().0, count - 1);
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_reconciliation_adds_disables_and_preserves_existing_motion() {
    let mut session = open("box_fall_plane.usda");
    for _ in 0..12 { session.tick_physics(DT, false).unwrap(); }
    let y0 = world_y(&mut session, "/World/Box");
    session.tick_physics(DT, false).unwrap();
    let y1 = world_y(&mut session, "/World/Box");
    guest_edit(&mut session, "/World", idtx_behavior::SessionEditOp::DefinePrim { type_name: "Xform".into(), name: "Unrelated".into() });
    session.tick_physics(DT, false).unwrap();
    let y2 = world_y(&mut session, "/World/Box");
    assert!(y1 - y2 > (y0 - y1) * 0.9, "unrelated topology reset the falling body's velocity");
    let count = session.physics_body_counts_for_test().unwrap().0;
    session.define_prim("/World/NewCollider", "Cube").unwrap();
    session.apply_api_schema("/World/NewCollider", "PhysicsCollisionAPI").unwrap();
    session.tick().unwrap();
    session.tick_physics(0.0, false).unwrap();
    assert_eq!(session.physics_body_counts_for_test().unwrap().0, count + 1);
    session.author_attribute("/World/NewCollider", "physics:collisionEnabled", idtx_usd::PropertyValue::Bool(false)).unwrap();
    session.tick().unwrap();
    session.tick_physics(0.0, false).unwrap();
    assert_eq!(session.physics_body_counts_for_test().unwrap().0, count);
    session.author_attribute("/World/NewCollider", "physics:collisionEnabled", idtx_usd::PropertyValue::Bool(true)).unwrap();
    session.tick().unwrap();
    session.tick_physics(0.0, false).unwrap();
    assert_eq!(session.physics_body_counts_for_test().unwrap().0, count + 1);
    session.remove_api_schema("/World/Ground", "PhysicsCollisionAPI").unwrap();
    session.tick().unwrap();
    session.tick_physics(0.0, false).unwrap();
    assert_eq!(session.physics_body_counts_for_test().unwrap().0, count, "removing a plane's collision API must retire its body");
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn live_session_reconciles_new_disabled_replaced_and_removed_behaviors() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd")
        .join(format!("runtime-lifecycle-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(directory.clone());
    let module = directory.join("actor.wasm");
    std::fs::write(&module, wat::parse_str("(module (memory (export \"memory\") 1) (func (export \"_idtx_abi_version\") (result i32) i32.const 3))").unwrap()).unwrap();
    let mut session = open("box_fall_plane.usda");
    assert_eq!(session.attach_authored_behaviors(|_| None), 0);
    session.attach_behavior_source("/World/Box", "actor.rs", "actor", "runtime", "Actor").unwrap();
    session.author_attribute("/World/Box", "idtx:behavior:module", idtx_usd::PropertyValue::Asset(module.to_string_lossy().into_owned())).unwrap();
    session.tick_behaviors(DT);
    assert_eq!(session.attached_behavior_count(), 1);
    session.tick_behaviors(DT);
    assert_eq!(session.attached_behavior_count(), 1);
    session.set_behavior_enabled("/World/Box", false).unwrap();
    session.tick_behaviors(DT);
    assert_eq!(session.attached_behavior_count(), 0);
    session.set_behavior_enabled("/World/Box", true).unwrap();
    session.author_attribute("/World/Box", "idtx:behavior:uuid", idtx_usd::PropertyValue::String("replacement".into())).unwrap();
    session.tick_behaviors(DT);
    assert_eq!(session.attached_behavior_count(), 1);
    let root = session.root_layer_snapshot().unwrap();
    guest_edit(&mut session, "/World/Box", idtx_behavior::SessionEditOp::RemovePrim);
    session.tick_behaviors(DT);
    assert_eq!(session.attached_behavior_count(), 0);
    assert_eq!(session.root_layer_snapshot().unwrap(), root);
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn stage_behavior_attached_to_a_playing_session_receives_play_start() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd")
        .join(format!("runtime-start-event-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(directory.clone());
    let mut session = open("box_fall_plane.usda");
    let entity = session.world().iter_entities().find(|entity| entity.get::<idtx_scene::PrimNode>().is_some_and(|node| node.path == "/World/Box")).unwrap().id();
    let module = directory.join("start.wasm");
    std::fs::write(&module, wat::parse_str(format!(r#"(module
        (import "idtx" "idtx_session_set_visibility" (func $visible (param i64 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "_idtx_abi_version") (result i32) i32.const 3)
        (func (export "_idtx_on_stage_event") (param $event i32)
            (if (i32.eq (local.get $event) (i32.const 2))
                (then (drop (call $visible (i64.const {}) (i32.const 0)))))))"#, entity.to_bits())).unwrap()).unwrap();
    session.ensure_stage_behavior_prim().unwrap();
    session.attach_behavior_source("/StageBehavior", "start.rs", "stage-start", "runtime", "Start").unwrap();
    session.set_behavior_scope("/StageBehavior", idtx_scene::behavior::BehaviorScope::Stage).unwrap();
    session.author_attribute("/StageBehavior", "idtx:behavior:capabilities", idtx_usd::PropertyValue::TokenArray(vec!["stage_edit".into()])).unwrap();
    session.author_attribute("/StageBehavior", "idtx:behavior:module", idtx_usd::PropertyValue::Asset(module.to_string_lossy().into_owned())).unwrap();
    session.tick().unwrap();
    session.set_behavior_playing(true);
    assert_eq!(session.attach_authored_behaviors(|_| None), 1);
    session.tick_behaviors(DT);
    assert!(!session.prim_visible("/World/Box"), "PlayStart was lost before the stage behavior attached");
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn native_and_external_frame_paths_share_delta_and_stage_clock() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd")
        .join(format!("runtime-clock-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(directory.clone());
    let module = directory.join("clock.wasm");
    std::fs::write(&module, wat::parse_str(r#"(module
        (import "idtx" "idtx_entity_self" (func $self (result i64)))
        (import "idtx" "idtx_session_set_xform" (func $set (param i64 i32) (result i32)))
        (import "idtx" "idtx_delta_seconds" (func $dt (result f32)))
        (import "idtx" "idtx_time_code" (func $time (result f64)))
        (memory (export "memory") 1)
        (func (export "_idtx_abi_version") (result i32) i32.const 3)
        (func (export "_idtx_on_update") (param f32)
            (f32.store (i32.const 0) (call $dt))
            (f32.store (i32.const 4) (f32.demote_f64 (call $time)))
            (f32.store (i32.const 24) (f32.const 1))
            (f32.store (i32.const 28) (f32.const 1))
            (f32.store (i32.const 32) (f32.const 1))
            (f32.store (i32.const 36) (f32.const 1))
            (drop (call $set (call $self) (i32.const 0)))))"#).unwrap()).unwrap();
    for external in [false, true] {
        let stage = directory.join(format!("clock-{external}.usda"));
        std::fs::copy(fixture("box_fall_plane.usda"), &stage).unwrap();
        let mut session = open_path(&stage);
        session.attach_behavior_source("/World/Box", "clock.rs", "clock", "runtime", "Clock").unwrap();
        session.author_attribute("/World/Box", "idtx:behavior:capabilities", idtx_usd::PropertyValue::TokenArray(vec!["stage_edit".into()])).unwrap();
        session.author_attribute("/World/Box", "idtx:behavior:module", idtx_usd::PropertyValue::Asset(module.to_string_lossy().into_owned())).unwrap();
        session.tick().unwrap();
        let root = session.world().iter_entities().find(|entity| entity.get::<idtx_scene::PrimNode>().is_some_and(|node| node.is_pseudo_root)).unwrap().id();
        session.world_mut().entity_mut(root).insert(idtx_scene::StagePlayback { current_time_code: 42.5, ..Default::default() });
        session.set_behavior_playing(true);
        assert_eq!(session.attach_authored_behaviors(|_| None), 1);
        if external {
            session.tick_external_behaviors(0.02, idtx_behavior::XrSnapshot::default(), glam::Vec3::ZERO, idtx_behavior::RawInput::default());
        } else { session.tick_behaviors(0.02); }
        let value = session.get_local_transform_f64("/World/Box").unwrap();
        assert!((value[12] - 0.02).abs() < 1e-6, "delta {value:?}");
        assert!((value[13] - 42.5).abs() < 1e-6, "stage clock {value:?}");
    }
}

/// World-space Y (translation row of the composed USD matrix) of `path`.
fn world_y(session: &mut StudioSession, path: &str) -> f32 {
    session
        .get_world_transform(path)
        .unwrap_or_else(|| panic!("world transform for {path}"))[3][1]
}

/// World-space X of `path`.
fn world_x(session: &mut StudioSession, path: &str) -> f32 {
    session
        .get_world_transform(path)
        .unwrap_or_else(|| panic!("world transform for {path}"))[3][0]
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_writeback_preserves_gprim_type() {
    // Regression (PR #16 review): the dynamic-pose write-back must author only a
    // transform override, never a `typeName` opinion. A prior `DefinePrim(path,
    // "Xform")` in the session layer retyped the simulated `Cube`/`Sphere` to a
    // plain `Xform` after the first tick, so on re-hydration it lost its gprim
    // geometry component — the prim stopped rendering AND stopped producing a
    // highlight mask (while its type-independent CollisionComp kept raycast/grab
    // working, matching the "grab works, highlight gone" field report).
    let mut session = open("box_fall_plane.usda");

    // Authored as a Cube and hydrated as one before any physics write.
    assert_eq!(
        session.composed_prim_type_for_test("/World/Box"),
        "Cube",
        "fixture Box must hydrate as a Cube before simulation"
    );

    // One tick is enough to author the first session-layer pose + re-project.
    session.tick_physics(DT, false).expect("tick_physics");
    assert_eq!(
        session.composed_prim_type_for_test("/World/Box"),
        "Cube",
        "write-back must not retype the simulated Cube to Xform"
    );

    // And it stays a Cube across sustained simulation (settles on the plane).
    for _ in 0..240 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    assert_eq!(
        session.composed_prim_type_for_test("/World/Box"),
        "Cube",
        "write-back must keep the gprim type across many ticks"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_box_falls_and_rests_on_plane() {
    let mut session = open("box_fall_plane.usda");

    // Authored start: the dynamic Box is at y = 5.
    let start = world_y(&mut session, "/World/Box");
    assert!((start - 5.0).abs() < 1e-3, "box starts at y=5, got {start}");

    // Simulate ~4 s; a unit cube (half-extent 0.5) settles on the y=0 plane.
    for _ in 0..240 {
        session.tick_physics(DT, false).expect("tick_physics");
    }

    let end = world_y(&mut session, "/World/Box");
    assert!(end < start, "box should fall: {start} -> {end}");
    assert!(
        (end - 0.5).abs() < 0.1,
        "box should rest near y=0.5, got {end}"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_restitution_ball_rebounds() {
    let mut session = open("restitution_bounce.usda");

    // Drop the restitution-0.9 ball; sample its Y every frame, tracking the
    // first-contact minimum and the highest Y reached AFTER that minimum.
    let mut min_y = f32::INFINITY;
    let mut rebound_peak = f32::NEG_INFINITY;
    let mut touched = false;
    for _ in 0..240 {
        session.tick_physics(DT, false).expect("tick_physics");
        let y = world_y(&mut session, "/World/Ball");
        if y <= min_y {
            min_y = y;
            touched = min_y < 1.0; // reached near the ground (radius 0.5)
        } else if touched {
            rebound_peak = rebound_peak.max(y);
        }
    }
    assert!(touched, "ball should reach the ground (min_y={min_y})");
    // A bouncy ball regains significant height after first contact.
    assert!(
        rebound_peak > min_y + 0.5,
        "restitution-0.9 ball should rebound: min={min_y}, peak={rebound_peak}"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_stack_of_boxes_stays_stable() {
    let mut session = open("stack_5.usda");

    for _ in 0..300 {
        session.tick_physics(DT, false).expect("tick_physics");
    }

    // No box should have exploded sideways or sunk through the floor; the stack
    // settles with each box roughly at its authored resting height.
    for i in 0..5 {
        let path = format!("/World/Box{i}");
        let xf = session
            .get_world_transform(&path)
            .unwrap_or_else(|| panic!("transform for {path}"));
        let (x, y, z) = (xf[3][0], xf[3][1], xf[3][2]);
        assert!(x.is_finite() && y.is_finite() && z.is_finite(), "{path} NaN");
        assert!(x.abs() < 0.5 && z.abs() < 0.5, "{path} drifted: ({x},{z})");
        assert!(y > 0.0, "{path} sank through the floor: y={y}");
    }
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_box_rests_on_scaled_cube_ground() {
    // Regression: a static UsdGeomCube flattened by a non-uniform scale
    // (1, 0.01, 1) is a thin ground slab, not a mean-collapsed cube that
    // swallows the body. The dropped box must REST on top, not fall through.
    let mut session = open("scaled_cube_ground.usda");

    let start = world_y(&mut session, "/World/Box");
    assert!((start - 3.0).abs() < 1e-3, "box starts at y=3, got {start}");

    for _ in 0..240 {
        session.tick_physics(DT, false).expect("tick_physics");
    }

    let end = world_y(&mut session, "/World/Box");
    assert!(end < start, "box should fall: {start} -> {end}");
    // Slab top at y=0.05 (half-extents (5, 0.05, 5)) + box half-extent 0.5.
    assert!(
        (end - 0.55).abs() < 0.1,
        "box should rest on the slab near y=0.55, got {end}"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_grab_suspend_holds_then_resume_falls() {
    // Grab support: suspending a dynamic body (what a `physics`-capable grab
    // behavior posts on grab-press) flips it to kinematic — it stops falling and
    // physics no longer authors its pose, so a held prim can ride the grabber.
    // Resuming (release) returns it to dynamic and it falls again.
    let mut session = open("box_fall_plane.usda");

    // Let it fall partway (well short of the y=0.5 rest).
    for _ in 0..30 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    let suspended_at = world_y(&mut session, "/World/Box");
    assert!(
        suspended_at < 5.0 && suspended_at > 1.0,
        "box should be mid-fall before suspend, got {suspended_at}"
    );

    // Suspend: must flip a tracked dynamic body (true), then it holds its Y —
    // the solver no longer integrates gravity nor authors it.
    assert!(
        session.set_physics_body_simulated_for_test("/World/Box", false),
        "suspend should flip the tracked dynamic body"
    );
    for _ in 0..60 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    let held = world_y(&mut session, "/World/Box");
    assert!(
        (held - suspended_at).abs() < 1e-2,
        "suspended box should hold its pose: {suspended_at} -> {held}"
    );

    // Resume: back to dynamic → it falls again and settles on the plane.
    assert!(
        session.set_physics_body_simulated_for_test("/World/Box", true),
        "resume should flip the suspended body back"
    );
    for _ in 0..240 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    let end = world_y(&mut session, "/World/Box");
    assert!(end < held, "resumed box should fall again: {held} -> {end}");
    assert!(
        (end - 0.5).abs() < 0.1,
        "resumed box should rest near y=0.5, got {end}"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_ball_rests_on_cone() {
    // Issue (PR #16 testing): a UsdGeomCone collider must be hydrated into the
    // sim as an analytic cone — a ball dropped straight down its axis rests on
    // the apex, it does not fall through. Cone base radius 1, height 2, centred
    // at (0,1,0) → apex at y=2; ball radius 0.5 settles near y≈2.5. A skipped or
    // degenerate cone collider would let the ball fall to the ground (y≈0.5).
    let mut session = open("cone_catches_ball.usda");

    let start = world_y(&mut session, "/World/Ball");
    assert!((start - 5.0).abs() < 1e-3, "ball starts at y=5, got {start}");

    for _ in 0..300 {
        session.tick_physics(DT, false).expect("tick_physics");
    }

    let end = world_y(&mut session, "/World/Ball");
    assert!(end < start, "ball should fall: {start} -> {end}");
    // Rests on the cone apex (y=2) + sphere radius 0.5 ≈ 2.5 — well clear of the
    // ground plane; a fall-through would land it near y=0.5.
    assert!(
        end > 1.5,
        "ball should rest on the cone apex (~2.5), not fall through to the ground: {end}"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_compound_body_falls_and_rests_level() {
    // Compound / whole-asset body: one `PhysicsRigidBodyAPI` Xform
    // (/World/Dumbbell) owning two child `Cube` colliders offset ±1 in X. It must
    // be grouped into ONE body with TWO colliders (not skipped, nor
    // split into two bodies), fall, and rest level + centred on the plane
    // (symmetric combined mass ⇒ no net torque).
    let mut session = open("compound_body.usda");

    let start = world_y(&mut session, "/World/Dumbbell");
    assert!((start - 5.0).abs() < 1e-3, "dumbbell starts at y=5, got {start}");

    // One tick builds the subsystem — assert the grouping: ground + dumbbell = 2
    // bodies; plane + two cubes = 3 colliders.
    session.tick_physics(DT, false).expect("tick_physics");
    assert_eq!(
        session.physics_body_counts_for_test(),
        Some((2, 3)),
        "ground + one compound body (2 bodies); plane + two cubes (3 colliders)"
    );

    for _ in 0..300 {
        session.tick_physics(DT, false).expect("tick_physics");
    }

    let xf = session
        .get_world_transform("/World/Dumbbell")
        .expect("dumbbell transform");
    let (x, y) = (xf[3][0], xf[3][1]);
    assert!(y < start, "compound body should fall: {start} -> {y}");
    assert!((y - 0.5).abs() < 0.15, "compound body rests at y≈0.5, got {y}");
    assert!(x.abs() < 0.25, "symmetric compound stays centred in X, got {x}");
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase1_nonuniform_scale_warns_and_still_simulates() {
    // The warning path (plan §5.5): a non-uniform scale is baked to its mean
    // rather than shearing or panicking. The body must still fall and rest.
    let mut session = open("nonuniform_scale_warn.usda");

    let start = world_y(&mut session, "/World/SquashedBox");
    for _ in 0..240 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    let end = world_y(&mut session, "/World/SquashedBox");
    assert!(end < start, "squashed box should still fall: {start} -> {end}");
    assert!(end > 0.0 && end.is_finite(), "squashed box must not sink/NaN: {end}");
    // Avoid an unused-variable warning path masking a hang.
    let _ = world_x(&mut session, "/World/SquashedBox");
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase2_seeded_asleep_box_does_not_fall() {
    // Phase 2A: a dynamic Cube authored `physics:startsAsleep = 1` must begin
    // asleep and hold its authored pose — the USD-dictated initial sleep state.
    // (The otherwise-identical box_fall_plane fixture falls from the same y=5, so
    // staying put proves the seed, not a missing gravity.)
    let mut session = open("box_starts_asleep.usda");

    let start = world_y(&mut session, "/World/Box");
    assert!((start - 5.0).abs() < 1e-3, "box starts at y=5, got {start}");

    // First tick builds the subsystem (seeding the body asleep from USD).
    session.tick_physics(DT, false).expect("tick_physics");
    assert_eq!(
        session.physics_body_sleeping_for_test("/World/Box"),
        Some(true),
        "a startsAsleep body must be seeded asleep"
    );

    // Many ticks later it has not fallen — the sleep gate authored nothing.
    for _ in 0..180 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    let y = world_y(&mut session, "/World/Box");
    assert!((y - 5.0).abs() < 1e-3, "asleep box must not fall: {start} -> {y}");
    assert_eq!(
        session.physics_body_sleeping_for_test("/World/Box"),
        Some(true),
        "box should remain asleep"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn physics_phase2_dropped_box_sleeps_and_syncs_usd() {
    // Phase 2A: a dropped dynamic body settles, rapier puts it to sleep, and the
    // runtime mirrors that into USD by authoring `physics:startsAsleep = true` —
    // keeping the persisted rest state in sync with the live sim.
    let mut session = open("box_fall_plane.usda");

    // Falling: awake, and no sleep flag authored yet.
    session.tick_physics(DT, false).expect("tick_physics");
    assert_eq!(
        session.physics_body_sleeping_for_test("/World/Box"),
        Some(false),
        "a falling box is awake"
    );

    // Let it fall and settle; rapier sleeps the rested island.
    for _ in 0..600 {
        session.tick_physics(DT, false).expect("tick_physics");
    }
    assert_eq!(
        session.physics_body_sleeping_for_test("/World/Box"),
        Some(true),
        "a settled box should sleep"
    );
    // Rested near the plane (half-extent 0.5), proving it actually simulated.
    let y = world_y(&mut session, "/World/Box");
    assert!((y - 0.5).abs() < 0.15, "box rests at y≈0.5, got {y}");
    // USD sleep-sync: the sleep transition authored physics:startsAsleep = true.
    assert_eq!(
        session.physics_starts_asleep_authored_for_test("/World/Box"),
        Some(true),
        "sleeping must be mirrored into the authored physics:startsAsleep"
    );
}
