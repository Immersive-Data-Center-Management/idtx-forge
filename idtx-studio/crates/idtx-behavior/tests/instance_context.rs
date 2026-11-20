//! Real Wasmtime regressions for context isolation, metadata, and reference tickets.
use bevy_ecs::world::World;
use idtx_behavior::{
    BehaviorHost, BehaviorManifest, Capabilities, Limits, PlayState, PrimBehaviorData,
    SessionEditOp, SpawnResult, SpawnStatus, ecs, entity_to_wire,
};
use idtx_behavior_abi::{BehaviorKind, Entity, ErrorCode};
use std::collections::BTreeMap;

fn manifest(uuid: &str, caps: Capabilities) -> BehaviorManifest {
    BehaviorManifest {
        name: "context".into(),
        version: "1".into(),
        uuid: uuid.into(),
        kind: BehaviorKind::Runtime,
        ship: true,
        params: BTreeMap::new(),
        inputs: vec![],
        capabilities: caps,
        memory_mb: None,
    }
}
fn register(host: &mut BehaviorHost, uuid: &str, wat: &str, caps: Capabilities) {
    host.register_module(&wat::parse_str(wat).unwrap(), manifest(uuid, caps))
        .unwrap();
}

#[test]
fn context_is_available_on_attach_and_pinned_per_instance() {
    let wat = r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_behavior_context" (func $context (param i32 i32 i32) (result i32)))
      (import "idtx" "idtx_session_set_widget_value" (func $value (param i64 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_hook_result" (func $result (param i32)))
      (memory (export "memory") 1) (data (i32.const 0) "Value")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func $copy
        (call $result (call $context (i32.const 100) (i32.const 8192) (i32.const 9000)))
        (drop (call $value (call $self) (i32.const 0) (i32.const 5) (i32.const 100) (i32.load (i32.const 9000)))))
      (func (export "_idtx_on_attach") call $copy)
      (func (export "_idtx_on_update") (param f32) call $copy))"#;
    let mut world = World::new();
    let a = world.spawn_empty().id();
    let b = world.spawn_empty().id();
    let mut host = BehaviorHost::new(Limits::runtime(), Default::default());
    register(
        &mut host,
        "same",
        wat,
        Capabilities::from_tokens(["stage_edit"]),
    );
    host.attach_with_context(
        &mut world,
        entity_to_wire(a),
        "same",
        Some(r#"{"url":"a"}"#),
    )
    .unwrap();
    host.attach_with_context(
        &mut world,
        entity_to_wire(b),
        "same",
        Some(r#"{"url":"b"}"#),
    )
    .unwrap();
    let edits = host.drain_session_edits();
    assert_eq!(edits.len(), 2);
    for edit in edits {
        let SessionEditOp::SetWidgetValue { value, .. } = edit.op else {
            panic!("value edit expected");
        };
        assert_eq!(
            value,
            if edit.target == entity_to_wire(a) {
                r#"{"url":"a"}"#
            } else {
                r#"{"url":"b"}"#
            }
        );
    }
    world.entity_mut(a).insert(PrimBehaviorData {
        context: Some(r#"{"url":"changed"}"#.into()),
        ..Default::default()
    });
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    let edits = host.drain_session_edits();
    let a_value = edits
        .iter()
        .find(|edit| edit.target == entity_to_wire(a))
        .unwrap();
    assert!(
        matches!(&a_value.op, SessionEditOp::SetWidgetValue { value, .. } if value == r#"{"url":"a"}"#)
    );
}

#[test]
fn startup_error_and_invalid_context_never_become_ready() {
    let wat = r#"(module (import "idtx" "idtx_hook_result" (func $result (param i32)))
      (memory (export "memory") 1)
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_start") (call $result (i32.const 3))))"#;
    let mut world = World::new();
    let a = world.spawn_empty().id();
    let mut host = BehaviorHost::new(Limits::runtime(), Default::default());
    register(&mut host, "failed", wat, Capabilities::default());
    assert!(
        host.attach_with_context(&mut world, entity_to_wire(a), "failed", Some("broken"))
            .is_err()
    );
    host.attach_with_context(&mut world, entity_to_wire(a), "failed", Some("{}"))
        .unwrap();
    assert_eq!(
        host.instance_readiness(entity_to_wire(a), "failed", true),
        Some(SpawnStatus::Projected)
    );
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert_eq!(
        host.instance_readiness(entity_to_wire(a), "failed", true),
        Some(SpawnStatus::Failed)
    );
    assert_eq!(
        host.instance_startup_error(entity_to_wire(a), "failed"),
        Some(ErrorCode::TypeMismatch)
    );
}

#[test]
fn metadata_import_and_named_child_use_complete_paths() {
    let wat = r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_prim_custom_data" (func $data (param i64 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_entity_named_child" (func $child (param i64 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_session_set_visibility" (func $visible (param i64 i32) (result i32)))
      (import "idtx" "idtx_hook_result" (func $result (param i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "idtx:assetOverview") (data (i32.const 40) "Panel")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_update") (param f32)
        (call $result (call $data (call $self) (i32.const 0) (i32.const 18) (i32.const 100) (i32.const 8192) (i32.const 9000)))
        (call $result (call $child (call $self) (i32.const 40) (i32.const 5) (i32.const 9100)))
        (drop (call $visible (i64.load (i32.const 9100)) (i32.const 1)))))"#;
    let mut world = World::new();
    let path = format!("/World/{}", "LongPath".repeat(60));
    let a = world
        .spawn((
            ecs::UsdPath(path.clone()),
            PrimBehaviorData {
                custom_data: BTreeMap::from([("idtx:assetOverview".into(), "{}".into())]),
                context: None,
            },
        ))
        .id();
    let panel = world.spawn(ecs::UsdPath(format!("{path}/Panel"))).id();
    let mut host = BehaviorHost::new(Limits::runtime(), Default::default());
    let bytes = wat::parse_str(wat).unwrap();
    assert!(
        !idtx_behavior::verify_link(&bytes, Capabilities::from_tokens(["stage_edit"]))
            .unwrap()
            .links
    );
    register(
        &mut host,
        "metadata",
        wat,
        Capabilities::from_tokens(["scene_query", "stage_edit"]),
    );
    host.attach(&mut world, entity_to_wire(a), "metadata")
        .unwrap();
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert!(
        host.drain_session_edits()
            .iter()
            .any(|edit| edit.target == entity_to_wire(panel)
                && edit.op == SessionEditOp::SetVisibility(true))
    );
}

#[test]
fn spawn_tickets_are_bounded_owned_and_consumed_after_terminal_read() {
    let wat = r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_session_spawn_reference" (func $spawn (param i64 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_session_spawn_status" (func $status (param i64 i32) (result i32)))
      (import "idtx" "idtx_hook_result" (func $result (param i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "Panel") (data (i32.const 10) "overview")
      (data (i32.const 100) "{\22context\22:{},\22connections\22:[]}")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_start")
        (f32.store (i32.const 324) (f32.const 1))
        (f32.store (i32.const 328) (f32.const 1)) (f32.store (i32.const 332) (f32.const 1)) (f32.store (i32.const 336) (f32.const 1))
        (call $result (call $spawn (call $self) (i32.const 0) (i32.const 5) (i32.const 10) (i32.const 8)
          (i32.const 300) (i32.const 100) (i32.const 31) (i32.const 400))))
      (func (export "_idtx_on_update") (param f32)
        (drop (call $status (i64.load (i32.const 400)) (i32.const 500)))))"#;
    let mut world = World::new();
    let a = world.spawn_empty().id();
    let mut host = BehaviorHost::new(Limits::runtime(), Default::default());
    host.set_spawn_generation(9);
    register(
        &mut host,
        "spawn",
        wat,
        Capabilities::from_tokens(["stage_edit"]),
    );
    host.attach(&mut world, entity_to_wire(a), "spawn").unwrap();
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    let edits = host.drain_session_edits();
    assert_eq!(edits.len(), 1);
    let SessionEditOp::SpawnReference {
        ticket,
        generation,
        context_json,
        ..
    } = &edits[0].op
    else {
        panic!("reference expected");
    };
    assert_eq!(*generation, 9);
    assert_eq!(context_json, "{}");
    assert!(host.has_spawn_ticket(*ticket));
    assert!(host.set_spawn_result(
        *ticket,
        SpawnResult::new(SpawnStatus::Projected, entity_to_wire(a), ErrorCode::Ok)
    ));
    assert!(host.set_spawn_result(
        *ticket,
        SpawnResult::new(SpawnStatus::Ready, entity_to_wire(a), ErrorCode::Ok)
    ));
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert!(!host.has_spawn_ticket(*ticket));
    assert!(!host.set_spawn_result(
        *ticket,
        SpawnResult::new(SpawnStatus::Failed, Entity::NULL, ErrorCode::NotFound)
    ));
}

#[test]
fn invalid_guest_output_pointers_never_enqueue_or_consume_a_ticket() {
    let wat = r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_session_spawn_reference" (func $spawn (param i64 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_session_spawn_status" (func $status (param i64 i32) (result i32)))
      (import "idtx" "idtx_hook_result" (func $result (param i32)))
      (memory (export "memory") 1) (global $reads (mut i32) (i32.const 0))
      (data (i32.const 0) "Panel") (data (i32.const 10) "overview")
      (data (i32.const 100) "{\22context\22:{},\22connections\22:[]}")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func $request (param $out i32) (result i32)
        (call $spawn (call $self) (i32.const 0) (i32.const 5) (i32.const 10) (i32.const 8)
          (i32.const 300) (i32.const 100) (i32.const 31) (local.get $out)))
      (func (export "_idtx_on_start")
        (f32.store (i32.const 324) (f32.const 1)) (f32.store (i32.const 328) (f32.const 1))
        (f32.store (i32.const 332) (f32.const 1)) (f32.store (i32.const 336) (f32.const 1))
        (if (i32.ne (call $request (i32.const 65532)) (i32.const 6)) (then unreachable))
        (call $result (call $request (i32.const 400))))
      (func (export "_idtx_on_update") (param f32)
        (global.set $reads (i32.add (global.get $reads) (i32.const 1)))
        (if (i32.lt_u (global.get $reads) (i32.const 3))
          (then (if (i32.ne (call $status (i64.load (i32.const 400)) (i32.const 65528)) (i32.const 6)) (then unreachable)))
          (else (drop (call $status (i64.load (i32.const 400)) (i32.const 500)))))))"#;
    let mut world = World::new();
    let a = world.spawn_empty().id();
    let mut host = BehaviorHost::new(Limits::runtime(), Default::default());
    register(
        &mut host,
        "pointers",
        wat,
        Capabilities::from_tokens(["stage_edit"]),
    );
    host.attach(&mut world, entity_to_wire(a), "pointers")
        .unwrap();
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    let edits = host.drain_session_edits();
    assert_eq!(edits.len(), 1, "the rejected request must not be queued");
    let SessionEditOp::SpawnReference { ticket, .. } = edits[0].op else {
        panic!("reference expected")
    };
    assert!(host.set_spawn_result(
        ticket,
        SpawnResult::new(SpawnStatus::Projected, entity_to_wire(a), ErrorCode::Ok)
    ));
    assert!(host.set_spawn_result(
        ticket,
        SpawnResult::new(SpawnStatus::Ready, entity_to_wire(a), ErrorCode::Ok)
    ));
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert!(
        host.has_spawn_ticket(ticket),
        "failed result copy must preserve terminal status"
    );
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert!(!host.has_spawn_ticket(ticket));
}
