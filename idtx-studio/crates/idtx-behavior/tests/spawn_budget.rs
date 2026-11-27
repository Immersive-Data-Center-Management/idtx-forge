//! R20: polling async results must not release credit for undelivered callbacks.
//! Uses real guest imports; no injected completion bypasses request admission.
#![allow(clippy::float_cmp)]

use bevy_ecs::{entity::Entity, world::World};
use idtx_behavior::{BehaviorHost, BehaviorManifest, Capabilities, InputRouter, Limits, PlayState,
    SessionEditOp, WorkLimits, ecs, entity_to_wire};
use idtx_behavior_abi::{BehaviorKind, ErrorCode, SpawnResult, SpawnStatus};
use std::time::Duration;

const GUEST: &str = r#"(module
    (import "idtx" "idtx_entity_self" (func $self (result i64)))
    (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
    (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
    (import "idtx" "idtx_session_spawn_reference_async" (func $spawn (param i64 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
    (import "idtx" "idtx_session_spawn_status" (func $status (param i64 i32) (result i32)))
    (memory (export "memory") 1)
    (data (i32.const 128) "Child")
    (data (i32.const 144) "missing")
    (data (i32.const 160) "{\22context\22:{}}")
    (func (export "_idtx_abi_version") (result i32) i32.const 3)
    (func (export "_idtx_on_interact") (local $code i32)
        (drop (call $status (i64.load (i32.const 256)) (i32.const 272)))
        (drop (call $get (call $self) (i32.const 0)))
        (local.set $code (call $spawn (call $self) (i32.const 128) (i32.const 5) (i32.const 144) (i32.const 7)
            (i32.const 0) (i32.const 160) (i32.const 14) (i32.const 256)))
        ;; Only normal admission or InvalidArgument backpressure is expected.
        (if (i32.and (i32.ne (local.get $code) (i32.const 0)) (i32.ne (local.get $code) (i32.const 6)))
            (then unreachable)))
    (func (export "_idtx_on_spawn_result") (param i64 i32 i64 i32)
        (drop (call $get (call $self) (i32.const 0)))
        (f32.store (i32.const 0) (f32.add (f32.load (i32.const 0)) (f32.const 1)))
        (drop (call $set (call $self) (i32.const 0)))))"#;

fn fixture(work: WorkLimits) -> (BehaviorHost, World, Entity) {
    let limits = Limits { work: WorkLimits { dispatch_budget: Duration::from_secs(2), ..work },
        hook_budget: Duration::from_secs(2), ..Limits::runtime() };
    let mut host = BehaviorHost::new(limits, InputRouter::default());
    host.register_module(&wat::parse_str(GUEST).unwrap(), BehaviorManifest {
        name: "spawn-budget".into(), uuid: "spawn-budget".into(), version: "1".into(),
        kind: BehaviorKind::Runtime, ship: true, capabilities: Capabilities::all(),
        params: Default::default(), inputs: Vec::new(), memory_mb: None,
    }).unwrap();
    let mut world = World::new();
    let actor = add_actor(&mut host, &mut world);
    (host, world, actor)
}

fn add_actor(host: &mut BehaviorHost, world: &mut World) -> Entity {
    let actor = world.spawn((ecs::Transform::default(), ecs::GlobalTransform::default())).id();
    host.attach(world, entity_to_wire(actor), "spawn-budget").unwrap();
    actor
}

fn request(host: &mut BehaviorHost, world: &mut World, actor: Entity) -> Option<u64> {
    host.interact(world, entity_to_wire(actor));
    assert_eq!(host.diagnostics(entity_to_wire(actor), "spawn-budget").unwrap().2, 0, "guest must not trap");
    let edits = host.drain_session_edits();
    assert!(edits.len() <= 1);
    edits.into_iter().map(|edit| match edit.op {
        SessionEditOp::SpawnReference { ticket, .. } => ticket,
        other => panic!("unexpected command {other:?}"),
    }).next()
}

fn finish(host: &mut BehaviorHost, ticket: u64) {
    assert!(host.set_spawn_result(ticket, SpawnResult::new(
        SpawnStatus::Failed, idtx_behavior_abi::Entity::NULL, ErrorCode::NotFound)));
}

fn backlog(host: &mut BehaviorHost, world: &mut World, actor: Entity) -> usize {
    let mut accepted = 0;
    for _ in 0..128 {
        if let Some(ticket) = request(host, world, actor) {
            finish(host, ticket);
            accepted += 1;
        }
    }
    assert!(accepted > 0, "the fixture must exercise accepted requests");
    accepted
}

fn callbacks(world: &World, actor: Entity) -> usize {
    world.get::<ecs::Transform>(actor).unwrap().translation.x as usize
}

#[test]
fn polled_async_callbacks_keep_instance_queue_slots_until_delivery() {
    let (mut host, mut world, actor) = fixture(WorkLimits { queue_items: 4, ..Default::default() });
    let accepted = backlog(&mut host, &mut world, actor);
    assert!(accepted < 4, "each request also needs a deferred-command slot; admitted {accepted}");
    assert_eq!(callbacks(&world, actor), 0);
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, actor), accepted, "polling does not cancel promised callbacks");
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, actor), accepted, "callbacks are one-shot");
    assert!(request(&mut host, &mut world, actor).is_some(), "delivery releases quota");
}

#[test]
fn polled_async_callbacks_keep_shared_host_queue_slots() {
    let (mut host, mut world, actor) = fixture(WorkLimits { host_queue_items: 4, ..Default::default() });
    let second = add_actor(&mut host, &mut world);
    let accepted = backlog(&mut host, &mut world, actor);
    assert!(accepted < 4);
    assert!(request(&mut host, &mut world, second).is_none(), "a second store cannot bypass the host's occupied credits");
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, actor), accepted);
    assert!(request(&mut host, &mut world, second).is_some());
}

#[test]
fn polled_async_callbacks_keep_instance_and_host_byte_reservations() {
    for work in [
        WorkLimits { queue_bytes: 512, ..Default::default() },
        WorkLimits { host_queue_bytes: 512, ..Default::default() },
    ] {
        let (mut host, mut world, actor) = fixture(work);
        let accepted = backlog(&mut host, &mut world, actor);
        assert!(accepted * std::mem::size_of::<SpawnResult>() <= 512,
            "retained callback payload alone exceeds the byte budget: {accepted}");
        host.run_frame(&mut world, 0.01, PlayState::Authoring);
        assert_eq!(callbacks(&world, actor), accepted);
        assert!(request(&mut host, &mut world, actor).is_some());
    }
}

#[test]
fn polled_async_callbacks_still_count_toward_the_64_ticket_limit() {
    let (mut host, mut world, actor) = fixture(WorkLimits::default());
    let accepted = backlog(&mut host, &mut world, actor);
    assert_eq!(accepted, 64);
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, actor), accepted);
    assert!(request(&mut host, &mut world, actor).is_some());
}

#[test]
fn deferred_callbacks_keep_credit_and_teardown_releases_it() {
    let (mut host, mut world, actor) = fixture(WorkLimits {
        queue_items: 4, host_queue_items: 4, dispatch_hooks: 1, ..Default::default()
    });
    let accepted = backlog(&mut host, &mut world, actor);
    assert!(accepted > 1 && accepted < 4);
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, actor), 1, "only one callback fits this dispatch");
    let replacement = request(&mut host, &mut world, actor).expect("one delivered callback released one slot");
    finish(&mut host, replacement);
    assert!(request(&mut host, &mut world, actor).is_none(), "deferred callbacks remain charged");
    host.set_spawn_generation(2);
    host.clear_spawn_results();
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, actor), 1, "old-generation callbacks are cancelled");
    let ticket = request(&mut host, &mut world, actor).unwrap();
    finish(&mut host, ticket);
    host.detach(&mut world, entity_to_wire(actor), "spawn-budget");
    let second = add_actor(&mut host, &mut world);
    host.run_frame(&mut world, 0.01, PlayState::Authoring);
    assert_eq!(callbacks(&world, second), 0);
    assert!(request(&mut host, &mut world, second).is_some(), "detach releases shared host credits");
}
