//! Real wasm memory/capability boundary and deferred-result consumption.
use bevy_ecs::world::World;
use idtx_behavior::{
    BehaviorHost, BehaviorManifest, Capabilities, Limits, PlayState, SessionEditOp, SpawnStatus,
    entity_to_wire,
};
use idtx_behavior_abi::{BehaviorKind, ErrorCode, PointInstance};

fn wasm() -> Vec<u8> {
    let data: String = bytemuck::bytes_of(&PointInstance {
        id: -7,
        prototype_index: 1,
        ..Default::default()
    })
    .iter()
    .map(|b| format!("\\{b:02x}"))
    .collect();
    wat::parse_str(format!(r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_session_update_point_instancer" (func $update (param i64 i32 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_session_edit_status" (func $status (param i64 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 16) "{data}")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func $expect (param $a i32) (param $b i32)
        (if (i32.ne (local.get $a) (local.get $b)) (then unreachable)))
      (func (export "_idtx_on_start")
        ;; oversized counts are rejected before arithmetic/allocation
        (call $expect (call $update (call $self) (i32.const 0) (i32.const 16) (i32.const -1) (i32.const 0) (i32.const 0) (i32.const 200)) (i32.const 6))
        ;; invalid output pointer must not enqueue even a valid payload
        (call $expect (call $update (call $self) (i32.const 0) (i32.const 16) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 65532)) (i32.const 6))
        ;; malformed kind and input ranges
        (call $expect (call $update (call $self) (i32.const 2) (i32.const 16) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 200)) (i32.const 6))
        (call $expect (call $update (call $self) (i32.const 0) (i32.const 65532) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 200)) (i32.const 6))
        (call $expect (call $update (call $self) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 65532) (i32.const 1) (i32.const 200)) (i32.const 6))
        (call $expect (call $update (call $self) (i32.const 0) (i32.const 16) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 200)) (i32.const 0)))
      (func (export "_idtx_on_update") (param f32)
        ;; failed status-output writes cannot consume a terminal result
        (call $expect (call $status (i64.load (i32.const 200)) (i32.const 65532)) (i32.const 6))
        (call $expect (call $status (i64.load (i32.const 200)) (i32.const 220)) (i32.const 0))
        (if (i32.ne (i32.load (i32.const 220)) (i32.const 0)) (then
          (call $expect (i32.load (i32.const 220)) (i32.const 2))
          (call $expect (i32.load (i32.const 224)) (i32.const 6))
          (call $expect (call $status (i64.load (i32.const 200)) (i32.const 220)) (i32.const 2))))))"#)).unwrap()
}

#[test]
fn point_instancer_imports_are_gated_bounded_and_return_consumable_tickets() {
    let bytes = wasm();
    assert!(
        !idtx_behavior::verify_link(&bytes, Capabilities::default())
            .unwrap()
            .links
    );
    let caps = Capabilities::from_tokens(["stage_edit"]);
    assert!(idtx_behavior::verify_link(&bytes, caps).unwrap().links);
    let mut world = World::new();
    let entity = entity_to_wire(world.spawn_empty().id());
    let mut host = BehaviorHost::new(Limits::runtime(), Default::default());
    host.register_module(
        &bytes,
        BehaviorManifest {
            name: "instancer".into(),
            version: "1".into(),
            uuid: "instancer".into(),
            kind: BehaviorKind::Runtime,
            ship: true,
            params: Default::default(),
            inputs: vec![],
            capabilities: caps,
            memory_mb: None,
        },
    )
    .unwrap();
    host.attach(&mut world, entity, "instancer").unwrap();
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert_eq!(
        host.instance_readiness(entity, "instancer", true),
        Some(SpawnStatus::Ready)
    );
    let commands = host.drain_session_edits();
    assert_eq!(commands.len(), 1, "failed marshals must never queue edits");
    let SessionEditOp::UpdatePointInstancer {
        ticket,
        instances,
        invisible_ids,
        ..
    } = &commands[0].op
    else {
        panic!("native edit expected");
    };
    assert_eq!(commands[0].target, entity);
    assert_eq!(instances.as_ref().unwrap()[0].id, -7);
    assert_eq!(instances.as_ref().unwrap()[0].prototype_index, 1);
    assert!(invisible_ids.is_empty());
    assert!(
        host.has_pending_stage_edit(*ticket),
        "Accepted result reads must retain the ticket"
    );
    assert!(host.finish_stage_edit(*ticket, ErrorCode::InvalidArgument));
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert!(!host.finish_stage_edit(*ticket, ErrorCode::Ok));
    assert_eq!(
        host.instance_readiness(entity, "instancer", true),
        Some(SpawnStatus::Ready)
    );
}

#[test]
fn point_instancer_thousands_require_and_honor_the_explicit_argument_budget() {
    for (budget, expected) in [(64 * 1024, 6), (512 * 1024, 0)] {
        let wasm = wat::parse_str(format!(r#"(module
          (import "idtx" "idtx_entity_self" (func $self (result i64)))
          (import "idtx" "idtx_session_update_point_instancer" (func $edit (param i64 i32 i32 i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 4)
          (func (export "_idtx_abi_version") (result i32) i32.const 3)
          (func (export "_idtx_on_start") (local $i i32) (local $p i32)
            (loop $fill
              (local.set $p (i32.mul (local.get $i) (i32.const 56)))
              (i64.store (local.get $p) (i64.extend_i32_u (local.get $i)))
              (f32.store offset=40 (local.get $p) (f32.const 1))
              (f32.store offset=44 (local.get $p) (f32.const 1))
              (f32.store offset=48 (local.get $p) (f32.const 1))
              (f32.store offset=52 (local.get $p) (f32.const 1))
              (local.set $i (i32.add (local.get $i) (i32.const 1)))
              (br_if $fill (i32.lt_u (local.get $i) (i32.const 4096))))
            (if (i32.ne (call $edit (call $self) (i32.const 0) (i32.const 0) (i32.const 4096)
                (i32.const 0) (i32.const 0) (i32.const 240000)) (i32.const {expected})) (then unreachable))))"#)).unwrap();
        let mut limits = Limits::runtime();
        limits.work.argument_bytes = budget;
        let mut host = BehaviorHost::new(limits, Default::default());
        let mut world = World::new();
        let entity = entity_to_wire(world.spawn_empty().id());
        host.register_module(
            &wasm,
            BehaviorManifest {
                name: "large".into(),
                version: "1".into(),
                uuid: "large".into(),
                kind: BehaviorKind::Runtime,
                ship: true,
                params: Default::default(),
                inputs: vec![],
                capabilities: Capabilities::from_tokens(["stage_edit"]),
                memory_mb: None,
            },
        )
        .unwrap();
        host.attach(&mut world, entity, "large").unwrap();
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert_eq!(
            host.instance_readiness(entity, "large", true),
            Some(SpawnStatus::Ready)
        );
        let commands = host.drain_session_edits();
        if expected == 0 {
            let SessionEditOp::UpdatePointInstancer { instances, .. } = &commands[0].op else {
                panic!();
            };
            assert_eq!(instances.as_ref().unwrap().len(), 4096);
        } else {
            assert!(commands.is_empty());
        }
    }
}
