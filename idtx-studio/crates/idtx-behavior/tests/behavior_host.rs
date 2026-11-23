//! End-to-end host tests driven by hand-written WAT fixtures.
//!
//! These exercise the full host path — module compile + validate,
//! capability-gated linker, per-hook dispatch, and the `stage_access`
//! bridge to a live `bevy_ecs` world — **headlessly**: no wasm toolchain,
//! no USD link. WAT is assembled to wasm bytes in-process via the `wat`
//! crate.
//!
//! Coverage:
//! * `movable_cube_translates` — a behavior reads
//!   its transform, mutates it, writes it back; 3 ticks accumulate.
//! * `xr_capability_denied` — a module importing `idtx_xr_get_pose`
//!   without the `xr_hmd` grant fails to link.
//! * `hmd_pose_drives_transform` — an injected
//!   mock HMD pose flows through `set_xr_snapshot` → guest → transform.

use std::collections::BTreeMap;

use bevy_ecs::world::World;
use idtx_behavior::ecs;
use idtx_behavior::{
    BehaviorHost, BehaviorManifest, Capabilities, Limits, PlayState, XrSnapshot, entity_to_wire,
};
use idtx_behavior_abi::{BehaviorKind, Entity as WireEntity, StageEvent, Xform};

/// A manifest with the given capabilities and no inputs/params.
fn manifest(uuid: &str, caps: Capabilities) -> BehaviorManifest {
    BehaviorManifest {
        name: "test".to_owned(),
        version: "0.1.0".to_owned(),
        uuid: uuid.to_owned(),
        kind: BehaviorKind::Runtime,
        ship: true,
        params: BTreeMap::new(),
        inputs: Vec::new(),
        capabilities: caps,
        memory_mb: None,
    }
}

/// Behavior that adds +1 to its `translation.x` every `on_update`.
const MOVABLE_CUBE_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (local $e i64)
    (local.set $e (call $self))
    ;; read self xform into linear-memory offset 0
    (drop (call $get (local.get $e) (i32.const 0)))
    ;; translation.x (byte offset 0) += 1.0
    (f32.store (i32.const 0)
      (f32.add (f32.load (i32.const 0)) (f32.const 1.0)))
    ;; write it back
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Behavior that reads the HMD pose and copies its translation into the
/// self entity's transform translation.
const HMD_SYNC_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_xr_get_pose" (func $pose (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (local $e i64)
    ;; read HMD pose (space 0 = Hmd) into offset 64 (an XrPoseResult)
    (drop (call $pose (i32.const 0) (i32.const 64)))
    ;; read current xform into offset 0 so rotation/scale stay valid
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    ;; copy pose translation (offset 64, 12 bytes) → xform translation (offset 0)
    (i64.store (i32.const 0) (i64.load (i32.const 64)))
    (i32.store (i32.const 8) (i32.load (i32.const 72)))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Behavior that imports the xr pose function — used to prove a module
/// without the `xr_hmd` grant fails to link.
const XR_IMPORTER_WAT: &str = r#"
(module
  (import "idtx" "idtx_xr_get_pose" (func $pose (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (drop (call $pose (i32.const 0) (i32.const 0))))
)
"#;

/// Behavior that reads the right-hand joint skeleton and copies the PALM
/// (joint 0) position into the self entity's transform translation. Proves the
/// `idtx_xr_get_hand_joints` import links under the `xr_hmd` grant and the host
/// writes the 26-joint array into guest memory.
const HAND_JOINTS_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_xr_get_hand_joints" (func $joints (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (local $e i64)
    ;; right hand (side=1) → 26 x 40 bytes written at offset 64
    (drop (call $joints (i32.const 1) (i32.const 64)))
    ;; read current xform into offset 0 so rotation/scale stay valid
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    ;; copy PALM (joint 0) position (offset 64, 12 bytes) -> translation (offset 0)
    (i64.store (i32.const 0) (i64.load (i32.const 64)))
    (i32.store (i32.const 8) (i32.load (i32.const 72)))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Behavior whose `on_destroy` stamps `translation.x = 9.0`. Proves the
/// terminal teardown hook is dispatched by `BehaviorHost::destroy`.
const ON_DESTROY_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_destroy")
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0) (f32.const 9.0))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Behavior whose `on_start` adds +10 to `translation.x` and whose
/// `on_stop` adds +100 to `translation.y`. Proves `on_stop` fires on the
/// Playing→Authoring transition and that `on_start` re-fires on re-entry.
const START_STOP_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  ;; bump the f32 at byte `$off` by `$amt`, preserving the rest of the xform.
  (func $bump (param $off i32) (param $amt f32)
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (local.get $off)
      (f32.add (f32.load (local.get $off)) (local.get $amt)))
    (drop (call $set (local.get $e) (i32.const 0))))
  (func (export "_idtx_on_start") (call $bump (i32.const 0) (f32.const 10.0)))
  (func (export "_idtx_on_stop") (call $bump (i32.const 4) (f32.const 100.0)))
)
"#;

/// Behavior that posts a `"ping"` event carrying an f32 payload (7.0) to
/// itself in `on_update`, then in `on_event` reads the payload back and
/// writes it into `translation.x`. Proves the payload survives the
/// post → drain → deliver round-trip via the `idtx_event_current_*` imports.
const EVENT_PAYLOAD_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_event_post"
    (func $post (param i64 i32 i32 i32 i32) (result i32)))
  (import "idtx" "idtx_event_current_payload"
    (func $payload (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 100) "ping")        ;; event name at offset 100
  (data (i32.const 110) "\00\00\e0\40") ;; f32 7.0 (little-endian) at offset 110
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    ;; post {target=self, name@100 len4, payload@110 len4}
    (drop (call $post (call $self)
      (i32.const 100) (i32.const 4)
      (i32.const 110) (i32.const 4))))
  (func (export "_idtx_on_event")
    (local $e i64)
    (local.set $e (call $self))
    ;; copy the current payload (up to 64 bytes) into offset 0
    (drop (call $payload (i32.const 0) (i32.const 64)))
    ;; read current xform into offset 16 (preserve rotation/scale)
    (drop (call $get (local.get $e) (i32.const 16)))
    ;; translation.x (offset 16) = payload f32 (offset 0)
    (f32.store (i32.const 16) (f32.load (i32.const 0)))
    (drop (call $set (local.get $e) (i32.const 16))))
)
"#;

/// Stage-scoped behavior: on `on_stage_event`, resolves the marker prim by
/// path and stores `(code + 1)` into its `translation.x`. The `+1` offset
/// keeps `StageEvent::Loaded` (code 0) distinguishable from "never fired".
const STAGE_SCOPED_EVENT_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_find" (func $find (param i32 i32 i32) (result i32)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 200) "/World/StageMarker") ;; 18 bytes at offset 200
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_stage_event") (param $code i32)
    (local $e i64)
    ;; resolve "/World/StageMarker" → WireEntity written to offset 8
    (drop (call $find (i32.const 200) (i32.const 18) (i32.const 8)))
    (local.set $e (i64.load (i32.const 8)))
    ;; read its xform into offset 0 (preserve rotation/scale)
    (drop (call $get (local.get $e) (i32.const 0)))
    ;; translation.x = (code + 1) as f32
    (f32.store (i32.const 0)
      (f32.convert_i32_u (i32.add (local.get $code) (i32.const 1))))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Entity-scoped behavior that ALSO exports `on_stage_event`, writing `42`
/// into its self entity's `translation.x`. It must never fire under
/// `dispatch_stage_event` — that path targets stage-scoped (pseudo-root)
/// instances only, so this proves entity-scoped instances are excluded.
const ENTITY_SCOPED_EVENT_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_stage_event") (param $code i32)
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0) (f32.const 42.0))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Stage-scoped behavior that ACCUMULATES: on each stage event it reads
/// `/World/StageMarker`'s `translation.x` and adds `1.0`. Two instances of this
/// same module (same uuid) therefore drive the marker to `2.0` per event — the
/// regression signal that two stage behaviors sharing a uuid do not collapse
/// onto a single instance.
const STAGE_SCOPED_ACCUMULATE_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_find" (func $find (param i32 i32 i32) (result i32)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 200) "/World/StageMarker") ;; 18 bytes at offset 200
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_stage_event") (param $code i32)
    (local $e i64)
    (drop (call $find (i32.const 200) (i32.const 18) (i32.const 8)))
    (local.set $e (i64.load (i32.const 8)))
    (drop (call $get (local.get $e) (i32.const 0)))
    ;; translation.x += 1.0
    (f32.store (i32.const 0)
      (f32.add (f32.load (i32.const 0)) (f32.const 1.0)))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Behavior that issues a playback-transport command. On `on_update`
/// it posts a `Seek` of clip "walk" to time code 12.0, addressed to the NULL
/// target so the host must resolve it to the posting entity.
const PLAYBACK_COMMAND_WAT: &str = r#"
(module
  (import "idtx" "idtx_playback_command"
    (func $cmd (param i64 i32 i32 i32 f32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 100) "walk") ;; clip name at offset 100, len 4
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    ;; post {target=NULL, clip@100 len4, kind=3 (Seek), value=12.0}
    (drop (call $cmd (i64.const 0)
      (i32.const 100) (i32.const 4)
      (i32.const 3) (f32.const 12))))
)
"#;

// A guest that authors a session-layer visibility edit against its own
// entity (target = NULL) each update. Requires the `stage_edit` capability;
// the `idtx_session_set_visibility` import is absent without it (link-fail).
const SESSION_EDIT_WAT: &str = r#"
(module
  (import "idtx" "idtx_session_set_visibility"
    (func $vis (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    ;; post {target=NULL, visible=0 (invisible)}
    (drop (call $vis (i64.const 0) (i32.const 0))))
)
"#;

/// Behavior whose `on_update` stores a Bearer token in the process-global auth
/// bucket via the `auth_write`-gated `idtx_auth_set_token` import. "tok-xyz" is
/// the 7-byte token at offset 16.
const AUTH_SET_TOKEN_WAT: &str = r#"
(module
  (import "idtx" "idtx_auth_set_token" (func $set (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 16) "tok-xyz")
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (drop (call $set (i32.const 16) (i32.const 7))))
)
"#;

// A guest that requests a level transition each update via the
// `level_control`-gated `idtx_request_level_transition` import. Target "cave"
// (4 bytes at offset 16) + named spawn "north" (5 bytes at offset 32). The
// import is absent without the `level_control` grant (link-fail).
const LEVEL_TRANSITION_WAT: &str = r#"
(module
  (import "idtx" "idtx_request_level_transition"
    (func $goto (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 16) "cave")
  (data (i32.const 32) "north")
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    ;; goto {target="cave"@16 len4, spawn="north"@32 len5}
    (drop (call $goto (i32.const 16) (i32.const 4) (i32.const 32) (i32.const 5))))
)
"#;

#[test]
fn movable_cube_translates() {
    let wasm = wat::parse_str(MOVABLE_CUBE_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Cube".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("cube-0001", Capabilities::all()))
        .expect("module registers");

    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // Three playing frames → translation.x should be 3.0.
    for _ in 0..3 {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
    }

    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.x - 3.0).abs() < 1e-4,
        "expected x==3.0 after 3 ticks, got {}",
        t.translation.x
    );
    assert!(t.translation.y.abs() < 1e-4, "y unchanged");
    // The write path must also refresh the cached matrix + dirty flag.
    assert!(world.get::<ecs::TransformDirty>(entity).is_some());
}

#[test]
fn xr_capability_denied() {
    let wasm = wat::parse_str(XR_IMPORTER_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world.spawn(ecs::Transform::default()).id();

    let mut host = BehaviorHost::with_defaults();
    // Manifest WITHOUT xr_hmd → the linker never provides idtx_xr_get_pose.
    let uuid = host
        .register_module(&wasm, manifest("xr-denied-0001", Capabilities::default()))
        .expect("module compiles");

    let err = host
        .attach(&mut world, entity_to_wire(entity), &uuid)
        .expect_err("attach must fail: ungranted xr_hmd import cannot link");
    // It is a link failure (the import is simply absent).
    assert!(
        matches!(err, idtx_behavior::ModuleError::LinkFailed(_)),
        "expected LinkFailed, got {err:?}"
    );
}

#[test]
fn hmd_pose_drives_transform() {
    let wasm = wat::parse_str(HMD_SYNC_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/IDTXPreviewCamera".to_owned()),
        ))
        .id();

    // Grant xr_hmd + xform_write (what the HMD-sync behavior needs).
    let caps = Capabilities {
        xr_hmd: true,
        xform_write: true,
        ..Capabilities::default()
    };
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("hmd-0001", caps))
        .expect("module registers");
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach");

    // Feed a sequence of mock HMD poses; the transform must track each.
    for (i, expected) in [[5.0, 6.0, 7.0], [-1.0, 2.5, 0.0]].into_iter().enumerate() {
        let pose = Xform::from_trs(
            glam::Vec3::from_array(expected),
            glam::Quat::IDENTITY,
            glam::Vec3::ONE,
        );
        host.set_xr_snapshot(XrSnapshot::with_hmd(pose));
        host.run_frame(&mut world, 0.011, PlayState::Playing);

        let t = world.get::<ecs::Transform>(entity).expect("transform");
        assert!(
            (t.translation - glam::Vec3::from_array(expected)).length() < 1e-4,
            "tick {i}: expected {expected:?}, got {:?}",
            t.translation
        );
    }
}

#[test]
fn hand_joints_link_and_read() {
    use idtx_behavior_abi::{XrHandJointResult, XrSide, XR_HAND_JOINT_COUNT};

    let wasm = wat::parse_str(HAND_JOINTS_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Hand".to_owned()),
        ))
        .id();

    // Grant xr_hmd (hand joints) + xform_write (the copy-into-transform).
    let caps = Capabilities {
        xr_hmd: true,
        xform_write: true,
        ..Capabilities::default()
    };
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("joints-0001", caps))
        .expect("module registers");
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach: the xr_hmd grant links idtx_xr_get_hand_joints");

    // Feed a right-hand skeleton whose PALM sits at a known position.
    let mut joints = [XrHandJointResult {
        position: [0.0; 3],
        radius: 0.0,
        orientation: [0.0, 0.0, 0.0, 1.0],
        valid: 1,
        _pad: 0,
    }; XR_HAND_JOINT_COUNT];
    joints[0].position = [1.5, -2.0, 0.5];
    let mut snap = XrSnapshot::untracked();
    snap.set_hand_joints(XrSide::Right, Some(Box::new(joints)));
    host.set_xr_snapshot(snap);

    host.run_frame(&mut world, 0.011, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation - glam::Vec3::new(1.5, -2.0, 0.5)).length() < 1e-4,
        "right-hand PALM position must flow into the transform, got {:?}",
        t.translation
    );
}

#[test]
fn on_destroy_is_dispatched() {
    let wasm = wat::parse_str(ON_DESTROY_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Doomed".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("destroy-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // No frame ever runs; destroy alone must fire on_destroy.
    host.destroy(&mut world, wire, &uuid);

    assert_eq!(host.instance_count(), 0, "instance dropped after destroy");
    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.x - 9.0).abs() < 1e-4,
        "on_destroy should have set x==9.0, got {}",
        t.translation.x
    );
}

/// A guest that accumulates `translation.x += 1` each `on_update` (so author
/// state must persist across hooks) and, in the generated `_idtx_drop_instance`
/// terminal export, stamps `translation.y = 42`. The host must invoke that
/// export after the terminal lifecycle hook on both `detach` and `destroy`, and
/// never during ordinary frames.
const DROP_INSTANCE_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0)
      (f32.add (f32.load (i32.const 0)) (f32.const 1.0)))
    (drop (call $set (local.get $e) (i32.const 0))))
  (func (export "_idtx_drop_instance")
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 4) (f32.const 42.0))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Build a host + world with the `DROP_INSTANCE_WAT` module attached and two
/// playing frames run. Returns the pieces a teardown test needs. Asserts the
/// two updates accumulated (x==2) and that the terminal export has NOT fired
/// yet (y==0) — i.e. ordinary frames keep the instance alive.
fn attach_drop_instance_fixture() -> (BehaviorHost, World, bevy_ecs::entity::Entity, WireEntity, String)
{
    let wasm = wat::parse_str(DROP_INSTANCE_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Persistent".to_owned()),
        ))
        .id();
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("drop-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // Two playing frames: author state accumulates across hooks, and the
    // terminal drop export must NOT run while the instance is live.
    for _ in 0..2 {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
    }
    {
        let t = world.get::<ecs::Transform>(entity).expect("transform");
        assert!(
            (t.translation.x - 2.0).abs() < 1e-4,
            "two updates must accumulate on a persistent instance (x={})",
            t.translation.x
        );
        assert!(
            t.translation.y.abs() < 1e-4,
            "_idtx_drop_instance must not fire during frames (y={})",
            t.translation.y
        );
    }
    (host, world, entity, wire, uuid)
}

#[test]
fn drop_instance_runs_on_destroy() {
    let (mut host, mut world, entity, wire, uuid) = attach_drop_instance_fixture();

    host.destroy(&mut world, wire, &uuid);

    assert_eq!(host.instance_count(), 0, "instance dropped after destroy");
    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.y - 42.0).abs() < 1e-4,
        "destroy must invoke _idtx_drop_instance after the terminal hook (y={})",
        t.translation.y
    );
}

#[test]
fn drop_instance_runs_on_detach() {
    let (mut host, mut world, entity, wire, uuid) = attach_drop_instance_fixture();

    host.detach(&mut world, wire, &uuid);

    assert_eq!(host.instance_count(), 0, "instance dropped after detach");
    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.y - 42.0).abs() < 1e-4,
        "detach must invoke _idtx_drop_instance after the terminal hook (y={})",
        t.translation.y
    );
}

#[test]
fn drop_instance_runs_on_detach_all() {
    let (mut host, mut world, entity, _wire, _uuid) = attach_drop_instance_fixture();

    // Host shutdown / replacement path: every live instance must run its
    // terminal lifecycle + guest destructor rather than being dropped silently.
    host.detach_all(&mut world);

    assert_eq!(host.instance_count(), 0, "all instances dropped by detach_all");
    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.y - 42.0).abs() < 1e-4,
        "detach_all must invoke _idtx_drop_instance after the terminal hook (y={})",
        t.translation.y
    );
}

#[test]
fn drop_instance_runs_when_attach_overwrites_existing() {
    let (mut host, mut world, entity, wire, uuid) = attach_drop_instance_fixture();

    // Re-attaching the same (entity, uuid) must tear the prior instance down
    // (running its guest destructor) instead of letting the `insert` drop its
    // store silently.
    host.attach(&mut world, wire, &uuid)
        .expect("re-attach over existing instance");

    assert_eq!(host.instance_count(), 1, "overwrite keeps exactly one instance");
    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.y - 42.0).abs() < 1e-4,
        "attach overwrite must invoke the prior instance's _idtx_drop_instance (y={})",
        t.translation.y
    );
}

#[test]
fn on_stop_fires_on_transition_and_on_start_refires() {
    let wasm = wat::parse_str(START_STOP_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Player".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("startstop-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // Playing frame → on_start fires once (x += 10).
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    // Another playing frame must NOT re-fire on_start (still started).
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    {
        let t = world.get::<ecs::Transform>(entity).expect("transform");
        assert!((t.translation.x - 10.0).abs() < 1e-4, "on_start fired once");
        assert!(t.translation.y.abs() < 1e-4, "on_stop not fired yet");
    }

    // Authoring frame → on_stop fires (y += 100), started reset.
    host.run_frame(&mut world, 0.016, PlayState::Authoring);
    // A second authoring frame must NOT re-fire on_stop.
    host.run_frame(&mut world, 0.016, PlayState::Authoring);
    {
        let t = world.get::<ecs::Transform>(entity).expect("transform");
        assert!((t.translation.y - 100.0).abs() < 1e-4, "on_stop fired once");
    }

    // Re-entering Playing → on_start re-fires (x += 10 → 20).
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.x - 20.0).abs() < 1e-4,
        "on_start should re-fire after stop, x={}",
        t.translation.x
    );
    assert!(
        (t.translation.y - 100.0).abs() < 1e-4,
        "on_stop fired exactly once"
    );
}

#[test]
fn on_event_payload_round_trips() {
    let wasm = wat::parse_str(EVENT_PAYLOAD_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Emitter".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("event-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // One playing frame: on_update posts the event; the host drains it and
    // delivers to on_event, which reads the f32 payload (7.0) into x.
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.x - 7.0).abs() < 1e-4,
        "on_event should have read the 7.0 payload into x, got {}",
        t.translation.x
    );
}

#[test]
fn playback_command_posts_and_drains_with_self_target() {
    use idtx_behavior::PlaybackCommandKind;

    let wasm = wat::parse_str(PLAYBACK_COMMAND_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("playback-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // One playing frame: on_update posts a Seek command targeting NULL.
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let cmds = host.drain_playback_commands();
    assert_eq!(cmds.len(), 1, "exactly one command drained");
    let cmd = &cmds[0];
    assert_eq!(
        cmd.target, wire,
        "NULL target resolved to the posting entity"
    );
    assert_eq!(cmd.clip, "walk");
    assert_eq!(cmd.kind, PlaybackCommandKind::Seek);
    assert!(
        (cmd.value - 12.0).abs() < 1e-6,
        "seek value 12.0, got {}",
        cmd.value
    );

    // Draining again yields nothing — the queue was emptied.
    assert!(host.drain_playback_commands().is_empty());
}

#[test]
fn playback_command_denied_without_capability() {
    // A module importing idtx_playback_command must fail to instantiate when
    // the `playback` capability is not granted (the import is not linked).
    let wasm = wat::parse_str(PLAYBACK_COMMAND_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("playback-0002", Capabilities::default()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    // Attach fails (or disables the instance) because the required import is
    // absent without the capability, mirroring `xr_capability_denied`.
    let attached = host.attach(&mut world, wire, &uuid);
    assert!(
        attached.is_err(),
        "attach must fail when a needed import is not linked"
    );
}

#[test]
fn session_edit_posts_and_drains_with_self_target() {
    use idtx_behavior::SessionEditOp;

    let wasm = wat::parse_str(SESSION_EDIT_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("sessedit-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // One playing frame: on_update posts a SetVisibility(false) to NULL.
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let edits = host.drain_session_edits();
    assert_eq!(edits.len(), 1, "exactly one edit drained");
    assert_eq!(
        edits[0].target, wire,
        "NULL target resolved to the posting entity"
    );
    assert_eq!(edits[0].op, SessionEditOp::SetVisibility(false));

    // Draining again yields nothing — the queue was emptied.
    assert!(host.drain_session_edits().is_empty());
}

#[test]
fn session_edit_denied_without_capability() {
    // A module importing idtx_session_set_visibility must fail to instantiate
    // when the `stage_edit` capability is not granted (the import is absent).
    let wasm = wat::parse_str(SESSION_EDIT_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("sessedit-0002", Capabilities::default()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    let attached = host.attach(&mut world, wire, &uuid);
    assert!(
        attached.is_err(),
        "attach must fail when idtx_session_set_visibility is not linked"
    );
}

#[test]
fn level_transition_posts_and_drains() {
    use idtx_behavior::LevelTransitionRequest;

    let wasm = wat::parse_str(LEVEL_TRANSITION_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Trigger".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("lvlc-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // One playing frame: on_update requests goto("cave", spawn="north").
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let reqs = host.drain_level_transitions();
    assert_eq!(reqs.len(), 1, "exactly one request drained");
    assert_eq!(
        reqs[0],
        LevelTransitionRequest {
            target: "cave".to_owned(),
            spawn: Some("north".to_owned()),
        }
    );

    // Draining again yields nothing — the queue was emptied.
    assert!(host.drain_level_transitions().is_empty());
}

#[test]
fn level_transition_denied_without_capability() {
    // A module importing idtx_request_level_transition must fail to instantiate
    // when the `level_control` capability is not granted (the import is absent),
    // mirroring `session_edit_denied_without_capability`.
    let wasm = wat::parse_str(LEVEL_TRANSITION_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Trigger".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("lvlc-0002", Capabilities::default()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    let attached = host.attach(&mut world, wire, &uuid);
    assert!(
        attached.is_err(),
        "attach must fail when idtx_request_level_transition is not linked"
    );
}

/// The `level_control` trigger is an ADDITIVE import — it must not have bumped
/// the frozen ABI version off 3.
#[test]
fn abi_version_is_still_three() {
    assert_eq!(idtx_behavior::ABI_VERSION, 3);
}

#[test]
fn auth_set_token_round_trips_with_capability() {
    // One explicitly authorized origin feeds every consumer of the shared store.
    let wasm = wat::parse_str(AUTH_SET_TOKEN_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();

    idtx_auth::clear();
    let caps = Capabilities {
        auth_write: true,
        ..Capabilities::default()
    };
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("auth-0001", caps))
        .expect("module registers");
    host.set_auth_scopes(idtx_auth::AuthScopes::parse(&["https://auth.example".into()]).unwrap());
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach: the auth_write grant links idtx_auth_set_token");

    host.run_frame(&mut world, 0.016, PlayState::Playing);

    assert_eq!(
        idtx_auth::resolve_header("TOKEN", "https://auth.example/data").as_deref(),
        Some("Bearer tok-xyz"),
        "the behavior's token reached the shared auth bucket"
    );
    idtx_auth::clear(); // leave the process-global bucket empty for other tests
}

#[test]
fn auth_write_denied_without_capability() {
    // A module importing idtx_auth_set_token must fail to instantiate when the
    // `auth_write` capability is not granted (the import is simply absent).
    let wasm = wat::parse_str(AUTH_SET_TOKEN_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("auth-0002", Capabilities::default()))
        .expect("module registers");
    let attached = host.attach(&mut world, entity_to_wire(entity), &uuid);
    assert!(
        attached.is_err(),
        "attach must fail when idtx_auth_set_token is not linked"
    );
}

#[test]
fn stage_event_reaches_stage_scoped_only() {
    let stage_wasm = wat::parse_str(STAGE_SCOPED_EVENT_WAT).expect("valid wat");
    let actor_wasm = wat::parse_str(ENTITY_SCOPED_EVENT_WAT).expect("valid wat");

    let mut world = World::new();
    let marker = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/StageMarker".to_owned()),
        ))
        .id();
    let actor = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Actor".to_owned()),
        ))
        .id();
    // The stage behavior's own `/StageBehavior` scope prim — its binding
    // identity in the host key (distinct from the entities it drives).
    let stage_prim = world
        .spawn((ecs::UsdPath("/StageBehavior".to_owned()),))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let stage_uuid = host
        .register_module(&stage_wasm, manifest("stage-0001", Capabilities::all()))
        .expect("stage module registers");
    let actor_uuid = host
        .register_module(&actor_wasm, manifest("actor-0001", Capabilities::all()))
        .expect("actor module registers");

    // Stage-scoped: attach via `attach_stage`, keyed by its scope prim but
    // running stage-wide. Entity-scoped: attach to a concrete entity (must be
    // excluded from stage-event delivery).
    host.attach_stage(&mut world, entity_to_wire(stage_prim), &stage_uuid)
        .expect("attach stage-scoped");
    host.attach(&mut world, entity_to_wire(actor), &actor_uuid)
        .expect("attach entity-scoped");

    // Loaded (code 0) → stage marker x == 1.0 (proves the event fired and
    // the code was delivered — not merely a zeroed default).
    host.dispatch_stage_event(&mut world, StageEvent::Loaded);
    {
        let m = world
            .get::<ecs::Transform>(marker)
            .expect("marker transform");
        assert!(
            (m.translation.x - 1.0).abs() < 1e-4,
            "Loaded should set marker x==1.0, got {}",
            m.translation.x
        );
    }

    // PlayStart (code 2) → stage marker x == 3.0 (latest event wins).
    host.dispatch_stage_event(&mut world, StageEvent::PlayStart);
    {
        let m = world
            .get::<ecs::Transform>(marker)
            .expect("marker transform");
        assert!(
            (m.translation.x - 3.0).abs() < 1e-4,
            "PlayStart should set marker x==3.0, got {}",
            m.translation.x
        );
    }

    // The entity-scoped instance must NOT have fired: its self transform is
    // still the spawned default.
    let a = world.get::<ecs::Transform>(actor).expect("actor transform");
    assert!(
        a.translation.x.abs() < 1e-4,
        "entity-scoped instance must be excluded from stage events, got x={}",
        a.translation.x
    );
}

/// Regression (multi stage behavior, same uuid): two stage behaviors that
/// resolve to the **same module uuid** — as two composed levels sharing one
/// behavior source do — must each attach as a distinct instance and both
/// receive stage events. Before the fix both bound to `Entity::NULL`, so the
/// second `attach` tore down and replaced the first (one surviving instance).
#[test]
fn two_stage_behaviors_same_uuid_do_not_overwrite() {
    let accumulate_wasm = wat::parse_str(STAGE_SCOPED_ACCUMULATE_WAT).expect("valid wat");

    let mut world = World::new();
    let marker = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/StageMarker".to_owned()),
        ))
        .id();
    // Two distinct `/StageBehavior` scope prims (as surfaced from two composed
    // levels), both carrying a behavior that resolves to the same module.
    let stage_prim_a = world
        .spawn((ecs::UsdPath("/World/Levels/A/StageBehavior".to_owned()),))
        .id();
    let stage_prim_b = world
        .spawn((ecs::UsdPath("/World/Levels/B/StageBehavior".to_owned()),))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    // One module, one uuid — the shared behavior source.
    let uuid = host
        .register_module(&accumulate_wasm, manifest("shared-0001", Capabilities::all()))
        .expect("module registers");

    host.attach_stage(&mut world, entity_to_wire(stage_prim_a), &uuid)
        .expect("attach stage behavior A");
    host.attach_stage(&mut world, entity_to_wire(stage_prim_b), &uuid)
        .expect("attach stage behavior B");

    // No collapse: two distinct binding entities keep two live instances.
    assert_eq!(
        host.instance_count(),
        2,
        "two same-uuid stage behaviors must remain distinct instances"
    );

    // Both fire on a stage event: each adds 1.0 to the marker → 2.0 total.
    host.dispatch_stage_event(&mut world, StageEvent::Loaded);
    let m = world
        .get::<ecs::Transform>(marker)
        .expect("marker transform");
    assert!(
        (m.translation.x - 2.0).abs() < 1e-4,
        "both stage instances must fire (expected x==2.0, got {})",
        m.translation.x
    );
}

/// Behavior that reads the `move_x` axis every `on_update` and writes it into
/// `translation.x` — proves the runtime input feed reaches guest reads.
const INPUT_AXIS_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_input_resolve" (func $resolve (param i32 i32) (result i32)))
  (import "idtx" "idtx_input_axis" (func $axis (param i32) (result f32)))
  (memory (export "memory") 1)
  ;; "move_x" (6 bytes) at offset 128
  (data (i32.const 128) "move_x")
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (local $e i64)
    (local $id i32)
    (local.set $e (call $self))
    ;; read current xform into offset 0 (keeps rotation/scale valid)
    (drop (call $get (local.get $e) (i32.const 0)))
    ;; resolve "move_x" → id, then write its axis value into translation.x
    (local.set $id (call $resolve (i32.const 128) (i32.const 6)))
    (f32.store (i32.const 0) (call $axis (local.get $id)))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Raw device input fed through the router reaches a guest's resolved
/// axis read and drives it live across frames.
#[test]
fn runtime_input_feed_drives_axis_read() {
    use idtx_behavior::{FrameInput, InputDecl, InputKind, InputMap, InputRouter};

    let wasm = wat::parse_str(INPUT_AXIS_WAT).expect("valid wat");

    // A map binding `move_x` to the gamepad left-stick X axis.
    let map = InputMap::parse(
        r#"
[actions.move_x]
kind = "axis"
bindings = ["Gamepad/LeftStickX"]
"#,
    )
    .expect("map parses");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Pawn".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::new(map));
    // The manifest must declare `move_x` so attach seeds the per-instance
    // action-id table the guest's `idtx_input_resolve` reads from.
    let mut m = manifest("input-0001", Capabilities::all());
    m.inputs = vec![InputDecl {
        action: "move_x".to_owned(),
        kind: InputKind::Axis,
    }];
    let uuid = host.register_module(&wasm, m).expect("module registers");
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach");

    // Frame 1: stick at +0.5 → guest reads axis, writes x == 0.5.
    let mut fi = FrameInput::new();
    fi.set_analog("Gamepad/LeftStickX", 0.5);
    host.update_input(&fi.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    fi.end_frame();
    {
        let t = world.get::<ecs::Transform>(entity).expect("transform");
        assert!(
            (t.translation.x - 0.5).abs() < 1e-4,
            "axis feed should drive x to 0.5, got {}",
            t.translation.x
        );
    }

    // Frame 2: stick swings to -1.0 → x tracks live to -1.0.
    fi.set_analog("Gamepad/LeftStickX", -1.0);
    host.update_input(&fi.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    fi.end_frame();
    {
        let t = world.get::<ecs::Transform>(entity).expect("transform");
        assert!(
            (t.translation.x + 1.0).abs() < 1e-4,
            "axis feed should track to -1.0, got {}",
            t.translation.x
        );
    }
}

/// A behavior that opts into edge-driven input via `_idtx_on_input`.
/// It writes the delivered phase code (as f32) into its self entity's
/// `translation.x`, so the test can observe which phase reached the guest.
const ON_INPUT_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_input") (param $id i32) (param $phase i32)
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0) (f32.convert_i32_u (local.get $phase)))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

/// Button edge events reach a guest's `_idtx_on_input` hook once per
/// phase transition (Pressed → Held → Released), and no event fires on a
/// steady None frame.
#[test]
fn on_input_delivers_button_edges() {
    use idtx_behavior::{FrameInput, InputDecl, InputKind, InputMap, InputRouter};

    let wasm = wat::parse_str(ON_INPUT_WAT).expect("valid wat");

    let map = InputMap::parse(
        r#"
[actions.jump]
kind = "button"
bindings = ["Key/Space"]
"#,
    )
    .expect("map parses");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Pawn".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::new(map));
    let mut m = manifest("on-input-0001", Capabilities::all());
    m.inputs = vec![InputDecl {
        action: "jump".to_owned(),
        kind: InputKind::Button,
    }];
    let uuid = host.register_module(&wasm, m).expect("module registers");
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach");

    let read_x = |world: &World| -> f32 {
        world
            .get::<ecs::Transform>(entity)
            .expect("transform")
            .translation
            .x
    };

    let mut fi = FrameInput::new();

    // Frame 1: press → Pressed (code 1).
    fi.press("Key/Space");
    host.update_input(&fi.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    fi.end_frame();
    assert!((read_x(&world) - 1.0).abs() < 1e-4, "Pressed → 1");

    // Frame 2: still held → Held (code 2).
    host.update_input(&fi.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    fi.end_frame();
    assert!((read_x(&world) - 2.0).abs() < 1e-4, "Held → 2");

    // Frame 3: release → Released (code 3).
    fi.release("Key/Space");
    host.update_input(&fi.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    fi.end_frame();
    assert!((read_x(&world) - 3.0).abs() < 1e-4, "Released → 3");

    // Frame 4: steady None → no on_input event fires, x is unchanged.
    host.update_input(&fi.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    fi.end_frame();
    assert!(
        (read_x(&world) - 3.0).abs() < 1e-4,
        "no edge → hook not called, x stays 3"
    );
}

/// Apple Vision Pro: with the `visionos` cargo feature the host
/// engine runs the **Pulley bytecode interpreter** (no native JIT codegen),
/// satisfying visionOS's W^X restriction. The same `.wasm` behavior must still
/// compile, register, and execute identically through the full dispatch path.
///
/// Run with `cargo test -p idtx-behavior --features visionos`.
#[cfg(feature = "visionos")]
#[test]
fn pulley_interpreter_runs_behaviors() {
    let wasm = wat::parse_str(MOVABLE_CUBE_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Cube".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    // The whole point of the feature: the engine must be the Pulley interpreter,
    // not the native Cranelift JIT.
    assert!(
        host.engine().is_pulley(),
        "the `visionos` feature must select the Pulley interpreter"
    );

    let uuid = host
        .register_module(&wasm, manifest("cube-pulley", Capabilities::all()))
        .expect("module compiles to Pulley bytecode and registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // Three playing frames → translation.x should accumulate to 3.0, exactly as
    // under Cranelift (`movable_cube_translates`).
    for _ in 0..3 {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
    }

    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.x - 3.0).abs() < 1e-4,
        "Pulley-interpreted behavior must translate x to 3.0, got {}",
        t.translation.x
    );
}

// ── scene-interaction imports ────────────────────────────────────

/// Behavior that imports `idtx_raycast` — proves a module without the
/// `scene_query` grant fails to link.
const RAYCAST_IMPORTER_WAT: &str = r#"
(module
  (import "idtx" "idtx_raycast" (func $ray (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (drop (call $ray (i32.const 0) (i32.const 48) (i32.const 128) (i32.const 64))))
)
"#;

#[test]
fn curve_query_imports_are_capability_gated_and_reject_unbounded_or_bad_memory() {
    let wasm=wat::parse_str(r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_curve_samples" (func $samples (param i64 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_curve_length" (func $length (param i64 i32 i32) (result i32)))
      (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
      (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_update") (param f32)
        (if (i32.ne (call $samples (call $self) (i32.const 0) (i32.const 1) (i32.const 0) (i32.const -1) (i32.const 64)) (i32.const 6)) (then unreachable))
        (if (i32.ne (call $samples (call $self) (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 65530)) (i32.const 6)) (then unreachable))
        (if (i32.ne (call $length (call $self) (i32.const 0) (i32.const 65534)) (i32.const 6)) (then unreachable))
        (drop (call $get (call $self) (i32.const 256)))
        (f32.store (i32.const 256) (f32.const 7))
        (drop (call $set (call $self) (i32.const 256))))
    )"#).unwrap();
    let mut world=World::new(); let entity=world.spawn((ecs::Transform::default(),ecs::GlobalTransform::default())).id();
    let mut host=BehaviorHost::with_defaults();
    let uuid=host.register_module(&wasm,manifest("curve-denied",Capabilities {xform_write:true,..Default::default()})).unwrap();
    assert!(host.attach(&mut world,entity_to_wire(entity),&uuid).is_err());
    let uuid=host.register_module(&wasm,manifest("curve-bounded",Capabilities {scene_query:true,xform_write:true,..Default::default()})).unwrap();
    host.attach(&mut world,entity_to_wire(entity),&uuid).unwrap();
    host.run_frame(&mut world,0.016,PlayState::Playing);
    assert_eq!(world.get::<ecs::Transform>(entity).unwrap().translation.x,7.0,"all bounded import checks must complete without a trap");
}

/// Behavior that raycasts and copies the hit distance into its own
/// `translation.x`. Params at offset 0 (zeroed), `RayHitResult` at 48 (so
/// `distance` lands at 48+20 = 68), path buffer at 128, scratch xform at 192.
const RAYCAST_SYNC_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_raycast" (func $ray (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (local $e i64)
    (drop (call $ray (i32.const 0) (i32.const 48) (i32.const 128) (i32.const 64)))
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 192)))
    (f32.store (i32.const 192) (f32.load (i32.const 68)))
    (drop (call $set (local.get $e) (i32.const 192))))
)
"#;

/// Behavior that posts a Highlight overlay command for its own entity
/// (NULL target → resolved to self on drain). `OverlayCommandKind::Highlight`
/// is wire value 2.
const HIGHLIGHT_WAT: &str = r#"
(module
  (import "idtx" "idtx_overlay_command"
    (func $ov (param i32 i64 i32 i32 i32 i32 i32 f32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (drop (call $ov
      (i32.const 2) (i64.const 0)
      (i32.const 0) (i32.const 0)
      (i32.const 0) (i32.const 0)
      (i32.const 0) (f32.const 0))))
)
"#;

/// A `SceneBackend` that always reports a fixed hit at distance 5.
struct FixedHitBackend;

impl idtx_behavior::SceneBackend for FixedHitBackend {
    fn raycast(
        &self,
        _world: &mut World,
        _params: &idtx_behavior_abi::RaycastParams,
    ) -> idtx_behavior_abi::RayHitResult {
        idtx_behavior_abi::RayHitResult {
            entity: WireEntity::NULL,
            position: idtx_behavior_abi::Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            distance: 5.0,
            hit: 1,
            flags: 0,
        }
    }

    fn arc_cast(
        &self,
        _world: &mut World,
        _params: &idtx_behavior_abi::ArcCastParams,
        _out_points: &mut [glam::Vec3],
    ) -> idtx_behavior_abi::ArcHitResult {
        idtx_behavior_abi::ArcHitResult::default()
    }

    fn find_by_type(
        &self,
        _world: &mut World,
        _type_hash: u64,
        out: &mut Vec<bevy_ecs::entity::Entity>,
    ) {
        out.clear();
    }

    fn path_of(&self, _world: &World, _entity: bevy_ecs::entity::Entity) -> Option<String> {
        None
    }
}

#[test]
fn scene_query_capability_denied() {
    let wasm = wat::parse_str(RAYCAST_IMPORTER_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world.spawn(ecs::Transform::default()).id();

    let mut host = BehaviorHost::with_defaults();
    // Manifest WITHOUT scene_query → `idtx_raycast` is never linked.
    let uuid = host
        .register_module(&wasm, manifest("ray-denied-0001", Capabilities::default()))
        .expect("module compiles");
    let err = host
        .attach(&mut world, entity_to_wire(entity), &uuid)
        .expect_err("attach must fail: ungranted scene_query import cannot link");
    assert!(
        matches!(err, idtx_behavior::ModuleError::LinkFailed(_)),
        "expected LinkFailed, got {err:?}"
    );
}

#[test]
fn raycast_routes_through_backend() {
    let wasm = wat::parse_str(RAYCAST_SYNC_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world
        .spawn((ecs::Transform::default(), ecs::GlobalTransform::default()))
        .id();

    let caps = Capabilities {
        scene_query: true,
        xform_write: true,
        ..Capabilities::default()
    };
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    host.set_scene_backend(std::sync::Arc::new(FixedHitBackend));
    let uuid = host
        .register_module(&wasm, manifest("ray-0001", caps))
        .expect("module registers");
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach");

    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        (t.translation.x - 5.0).abs() < 1e-4,
        "raycast hit distance (5.0) must flow into translation.x, got {}",
        t.translation.x
    );
}

#[test]
fn raycast_without_backend_misses_gracefully() {
    let wasm = wat::parse_str(RAYCAST_SYNC_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world
        .spawn((ecs::Transform::default(), ecs::GlobalTransform::default()))
        .id();

    let caps = Capabilities {
        scene_query: true,
        xform_write: true,
        ..Capabilities::default()
    };
    // No backend installed — the import must not trap; it writes a miss.
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("ray-nobackend-0001", caps))
        .expect("module registers");
    host.attach(&mut world, entity_to_wire(entity), &uuid)
        .expect("attach");

    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).expect("transform");
    assert!(
        t.translation.x.abs() < 1e-4,
        "a miss (no backend) must leave translation.x at 0, got {}",
        t.translation.x
    );
}

#[test]
fn overlay_command_routes_to_queue() {
    let wasm = wat::parse_str(HIGHLIGHT_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world.spawn(ecs::Transform::default()).id();

    let caps = Capabilities {
        overlay: true,
        ..Capabilities::default()
    };
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("hl-0001", caps))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let drained = host.drain_overlay_commands();
    assert_eq!(drained.len(), 1, "one overlay command posted");
    assert_eq!(
        drained[0].target, wire,
        "NULL target resolves to the posting entity"
    );
    assert!(
        matches!(drained[0].op, idtx_behavior::OverlayOp::Highlight { .. }),
        "expected a Highlight op, got {:?}",
        drained[0].op
    );

    // Transient: a second drain the next frame is empty.
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    let _ = host.drain_overlay_commands();
}

/// Behavior that posts a tint (kind 7) then an outline (kind 8) overlay
/// command for its own entity (NULL target → resolved to self on drain).
const TINT_OUTLINE_WAT: &str = r#"
(module
  (import "idtx" "idtx_overlay_command"
    (func $ov (param i32 i64 i32 i32 i32 i32 i32 f32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_update") (param $dt f32)
    (drop (call $ov
      (i32.const 7) (i64.const 0)
      (i32.const 0) (i32.const 0)
      (i32.const 0) (i32.const 0)
      (i32.const 0) (f32.const 0)))
    (drop (call $ov
      (i32.const 8) (i64.const 0)
      (i32.const 0) (i32.const 0)
      (i32.const 0) (i32.const 0)
      (i32.const 0) (f32.const 0))))
)
"#;

#[test]
fn tint_and_outline_route_to_queue() {
    let wasm = wat::parse_str(TINT_OUTLINE_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world.spawn(ecs::Transform::default()).id();

    let caps = Capabilities {
        overlay: true,
        ..Capabilities::default()
    };
    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("tint-0001", caps))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let drained = host.drain_overlay_commands();
    assert_eq!(drained.len(), 2, "two overlay commands posted");
    assert!(
        matches!(drained[0].op, idtx_behavior::OverlayOp::HighlightTint { .. }),
        "expected a HighlightTint op, got {:?}",
        drained[0].op
    );
    assert!(
        matches!(
            drained[1].op,
            idtx_behavior::OverlayOp::HighlightOutline { .. }
        ),
        "expected a HighlightOutline op, got {:?}",
        drained[1].op
    );
}

#[test]
fn overlay_capability_denied() {
    let wasm = wat::parse_str(HIGHLIGHT_WAT).expect("valid wat");
    let mut world = World::new();
    let entity = world.spawn(ecs::Transform::default()).id();

    let mut host = BehaviorHost::with_defaults();
    // Manifest WITHOUT overlay → `idtx_overlay_command` is never linked.
    let uuid = host
        .register_module(&wasm, manifest("hl-denied-0001", Capabilities::default()))
        .expect("module compiles");
    let err = host
        .attach(&mut world, entity_to_wire(entity), &uuid)
        .expect_err("attach must fail: ungranted overlay import cannot link");
    assert!(
        matches!(err, idtx_behavior::ModuleError::LinkFailed(_)),
        "expected LinkFailed, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Async HTTP completion delivery (`net`) — Step 0 of `run_frame`.
// ---------------------------------------------------------------------------

/// Stage-scoped-or-entity behavior exporting `_idtx_on_http_response`: writes the
/// delivered `status` → translation.x and `request_id` → translation.y, proving
/// the host drains an injected completion and dispatches the callback with the
/// correct args. Uses no `net` import, so it links under the default feature set.
const HTTP_RESPONSE_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_http_response") (param $rid i32) (param $status i32)
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0) (f32.convert_i32_s (local.get $status)))
    (f32.store (i32.const 4) (f32.convert_i32_s (local.get $rid)))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

#[test]
fn async_http_completion_delivers_id_and_status() {
    let wasm = wat::parse_str(HTTP_RESPONSE_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Agent".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("http-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // No completion queued → the callback must not fire.
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert!(
        world.get::<ecs::Transform>(entity).unwrap().translation.x.abs() < 1e-4,
        "callback fired without a completion"
    );

    // Inject a completion (standing in for a background worker); the next frame
    // delivers it to `_idtx_on_http_response`.
    assert!(host.inject_http_completion(
        wire,
        &uuid,
        idtx_behavior::stage_access::HttpCompletion {
            request_id: 7,
            status: 200,
            body: Vec::new(),
        },
    ));
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).unwrap();
    assert!(
        (t.translation.x - 200.0).abs() < 1e-3,
        "status should reach x==200, got {}",
        t.translation.x
    );
    assert!(
        (t.translation.y - 7.0).abs() < 1e-3,
        "request_id should reach y==7, got {}",
        t.translation.y
    );

    // One-shot: a later frame with no new completion does not re-fire.
    world.get_mut::<ecs::Transform>(entity).unwrap().translation.x = 0.0;
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert!(
        world.get::<ecs::Transform>(entity).unwrap().translation.x.abs() < 1e-4,
        "completion was re-delivered"
    );
}

/// Callback-mode spawn: the host calls `_idtx_on_spawn_result(ticket, state,
/// entity, error)` with the 16-byte `SpawnResult` as scalar args. This fixture
/// writes `state → x`, `error → y`, and the entity's low 32 bits → `z`.
const SPAWN_RESULT_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_spawn_result")
      (param $ticket i64) (param $state i32) (param $entity i64) (param $error i32)
    (local $e i64)
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0) (f32.convert_i32_s (local.get $state)))
    (f32.store (i32.const 4) (f32.convert_i32_s (local.get $error)))
    (f32.store (i32.const 8) (f32.convert_i32_u (i32.wrap_i64 (local.get $entity))))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

#[test]
fn async_spawn_completion_delivers_terminal_result_on_next_frame() {
    let wasm = wat::parse_str(SPAWN_RESULT_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Spawner".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("spawn-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    // No completion queued → the callback must not fire.
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert!(
        world.get::<ecs::Transform>(entity).unwrap().translation.x.abs() < 1e-4,
        "callback fired without a completion"
    );

    // Inject a terminal `Ready` completion (standing in for `settle_runtime_spawns`);
    // the next frame delivers it to `_idtx_on_spawn_result`.
    let spawned = entity_to_wire(world.spawn(ecs::Transform::default()).id());
    assert!(host.inject_spawn_completion(
        wire,
        &uuid,
        42,
        idtx_behavior_abi::SpawnResult::new(
            idtx_behavior_abi::SpawnStatus::Ready,
            spawned,
            idtx_behavior_abi::ErrorCode::Ok,
        ),
    ));
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).unwrap();
    assert!(
        (t.translation.x - 2.0).abs() < 1e-3, // SpawnStatus::Ready == 2
        "status Ready(2) should reach x, got {}",
        t.translation.x
    );
    assert!(
        t.translation.y.abs() < 1e-3, // ErrorCode::Ok == 0
        "error Ok(0) should reach y, got {}",
        t.translation.y
    );
    let expect_z = (spawned.to_bits() & 0xFFFF_FFFF) as f32;
    assert!(
        (t.translation.z - expect_z).abs() < 1.0,
        "spawned entity low bits should reach z, got {} want {}",
        t.translation.z,
        expect_z
    );

    // One-shot: a later frame with no new completion does not re-fire.
    world.get_mut::<ecs::Transform>(entity).unwrap().translation.x = 0.0;
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert!(
        world.get::<ecs::Transform>(entity).unwrap().translation.x.abs() < 1e-4,
        "completion was re-delivered"
    );
}

#[test]
fn cleared_spawn_inbox_delivers_nothing() {
    let wasm = wat::parse_str(SPAWN_RESULT_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Spawner".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("spawn-0002", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    assert!(host.inject_spawn_completion(
        wire,
        &uuid,
        7,
        idtx_behavior_abi::SpawnResult::new(
            idtx_behavior_abi::SpawnStatus::Failed,
            WireEntity::NULL,
            idtx_behavior_abi::ErrorCode::NotFound,
        ),
    ));
    // A teardown clear (session fan-out) drops the pending completion before the
    // next frame — the callback must not fire.
    host.clear_spawn_completions();
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert!(
        world.get::<ecs::Transform>(entity).unwrap().translation.x.abs() < 1e-4,
        "cleared completion was still delivered"
    );
}
/// `idtx_http_response_body` import: writes the body's true length → x and its
/// first byte → y. Only built with `--features net` (the import is otherwise
/// unlinked and a `net` behavior cannot load).
#[cfg(feature = "net")]
const HTTP_RESPONSE_BODY_WAT: &str = r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_http_response_body" (func $body (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func (export "_idtx_on_http_response") (param $rid i32) (param $status i32)
    (local $e i64) (local $len i32)
    (local.set $len (call $body (i32.const 64) (i32.const 256)))
    (local.set $e (call $self))
    (drop (call $get (local.get $e) (i32.const 0)))
    (f32.store (i32.const 0) (f32.convert_i32_s (local.get $len)))
    (f32.store (i32.const 4) (f32.convert_i32_u (i32.load8_u (i32.const 64))))
    (drop (call $set (local.get $e) (i32.const 0))))
)
"#;

#[cfg(feature = "net")]
#[test]
fn async_http_completion_body_is_readable() {
    let wasm = wat::parse_str(HTTP_RESPONSE_BODY_WAT).expect("valid wat");

    let mut world = World::new();
    let entity = world
        .spawn((
            ecs::Transform::default(),
            ecs::GlobalTransform::default(),
            ecs::UsdPath("/World/Agent".to_owned()),
        ))
        .id();

    let mut host = BehaviorHost::new(Limits::runtime(), idtx_behavior::InputRouter::default());
    let uuid = host
        .register_module(&wasm, manifest("httpbody-0001", Capabilities::all()))
        .expect("module registers");
    let wire = entity_to_wire(entity);
    host.attach(&mut world, wire, &uuid).expect("attach");

    assert!(host.inject_http_completion(
        wire,
        &uuid,
        idtx_behavior::stage_access::HttpCompletion {
            request_id: 1,
            status: 200,
            body: b"HELLO".to_vec(), // 5 bytes; first byte 'H' == 72
        },
    ));
    host.run_frame(&mut world, 0.016, PlayState::Playing);

    let t = world.get::<ecs::Transform>(entity).unwrap();
    assert!(
        (t.translation.x - 5.0).abs() < 1e-3,
        "body true length should reach x==5, got {}",
        t.translation.x
    );
    assert!(
        (t.translation.y - 72.0).abs() < 1e-3,
        "first body byte should reach y==72, got {}",
        t.translation.y
    );
}
