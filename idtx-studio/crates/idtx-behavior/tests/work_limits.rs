//! Dispatch and deferred-work regressions, run under native and Pulley hosts.
#![allow(clippy::float_cmp)] // WAT fixtures write exactly representable integer counters.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
mod common;

use bevy_ecs::{entity::Entity, world::World};
use idtx_behavior::{
    BehaviorHost, BehaviorManifest, Capabilities, HostState, InputRouter, Limits, PlayState,
    SessionEditOp, WidgetInteraction, WorkLimits, ecs, entity_to_wire,
};
use idtx_behavior_abi::{BehaviorKind, Entity as WireEntity, WidgetEventKind};

fn state() -> HostState {
    HostState::new(Capabilities::all(), WireEntity::NULL, wasmtime::StoreLimits::default())
}

#[test]
fn widget_input_values_are_bounded_and_private_to_each_instance() {
    let imports = r#"
      (import "idtx" "idtx_widget_set_value" (func $value (param i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_widget_get_value" (func $read (param i32 i32 i32 i32) (result i32)))"#;
    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
    let mut world = World::new();
    for (uuid, write) in [("writer", true), ("reader", false)] {
        let hooks = format!(r#"
          (data (i32.const 256) "password")
          (data (i32.const 512) "secret")
          (func (export "_idtx_on_update") (param f32)
            {}
            (call $bump (f32.convert_i32_u (call $read (i32.const 256) (i32.const 8) (i32.const 1024) (i32.const 1024))))
          )"#, if write { "(drop (call $value (i32.const 256) (i32.const 8) (i32.const 512) (i32.const 6)))" } else { "" });
        host.register_module(&counter_module_with_imports(imports, &hooks), manifest(uuid)).unwrap();
    }
    let writer = add_actor(&mut host, &mut world, "writer");
    let reader = add_actor(&mut host, &mut world, "reader");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(world.get::<ecs::Transform>(writer).unwrap().translation.x, 6.0);
    assert_eq!(world.get::<ecs::Transform>(reader).unwrap().translation.x, 0.0);
    let oversized = r#"(func (export "_idtx_on_update") (param f32)
        (call $bump (f32.convert_i32_u (call $value (i32.const 128) (i32.const 5) (i32.const 512) (i32.const 1025)))))"#;
    host.register_module(&counter_module_with_imports(imports, oversized), manifest("oversized")).unwrap();
    let oversized = add_actor(&mut host, &mut world, "oversized");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert!(world.get::<ecs::Transform>(oversized).unwrap().translation.x > 0.0);
}

fn counter_module(hooks: &str) -> Vec<u8> {
    counter_module_with_imports("", hooks)
}

fn counter_module_with_imports(imports: &str, hooks: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"
(module
  (import "idtx" "idtx_entity_self" (func $self (result i64)))
  (import "idtx" "idtx_get_xform" (func $get (param i64 i32) (result i32)))
  (import "idtx" "idtx_set_xform" (func $set (param i64 i32) (result i32)))
  (import "idtx" "idtx_event_post" (func $post (param i64 i32 i32 i32 i32) (result i32)))
  (import "idtx" "idtx_session_set_visibility" (func $edit (param i64 i32) (result i32)))
  {imports}
  (memory (export "memory") 1)
  (data (i32.const 128) "event")
  (func (export "_idtx_abi_version") (result i32) i32.const 3)
  (func $bump (param $amount f32)
    (drop (call $get (call $self) (i32.const 0)))
    (f32.store (i32.const 0) (f32.add (f32.load (i32.const 0)) (local.get $amount)))
    (drop (call $set (call $self) (i32.const 0))))
  {hooks})
"#)).expect("valid WAT")
}

fn host_with_counter(hooks: &str) -> (BehaviorHost, World, Entity) {
    host_with_limits(hooks, Limits::runtime())
}

fn manifest(uuid: &str) -> BehaviorManifest {
    BehaviorManifest {
        name: "work limits".to_owned(),
        version: "0.1.0".to_owned(),
        uuid: uuid.to_owned(),
        kind: BehaviorKind::Runtime,
        ship: true,
        params: BTreeMap::new(),
        inputs: Vec::new(),
        capabilities: Capabilities::all(),
        memory_mb: None,
    }
}

fn add_actor(host: &mut BehaviorHost, world: &mut World, uuid: &str) -> Entity {
    let entity = world.spawn((ecs::Transform::default(), ecs::GlobalTransform::default())).id();
    host.attach(world, entity_to_wire(entity), uuid).expect("attach");
    entity
}

fn host_with_limits(hooks: &str, limits: Limits) -> (BehaviorHost, World, Entity) {
    let mut host = BehaviorHost::new(limits, InputRouter::default());
    let mut world = World::new();
    host.register_module(&counter_module(hooks), manifest("counter")).expect("register");
    let entity = add_actor(&mut host, &mut world, "counter");
    (host, world, entity)
}

/// Count/fuel assertions are deterministic; give them scheduling headroom.
fn untimed_limits() -> Limits {
    Limits {
        hook_budget: Duration::from_secs(2),
        work: WorkLimits { dispatch_budget: Duration::from_secs(2), ..WorkLimits::default() },
        ..Limits::runtime()
    }
}

fn x(world: &World, entity: Entity) -> f32 {
    world.get::<ecs::Transform>(entity).unwrap().translation.x
}

const TRAPPING_UPDATE: &str = r#"
  (func (export "_idtx_on_update") (param f32)
    (drop (call $post (call $self) (i32.const 128) (i32.const 5) (i32.const 0) (i32.const 0)))
    unreachable)
  (func (export "_idtx_on_late_update") (param f32) (call $bump (f32.const 1)))
  (func (export "_idtx_on_event") (call $bump (f32.const 10)))
  (func (export "_idtx_on_interact") (call $bump (f32.const 100)))
  (func (export "_idtx_on_destroy") (call $bump (f32.const 1000)))
  (func (export "_idtx_drop_instance") (call $bump (f32.const 10000)))
"#;

#[test]
fn fatal_trap_stops_later_hooks_in_the_same_frame() {
    let (mut host, mut world, entity) = host_with_counter(TRAPPING_UPDATE);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 0.0, "late update/event resumed a disabled guest");
}

#[test]
fn removed_entities_cannot_keep_live_guest_instances() {
    let (mut host, mut world, entity) = host_with_counter("(func (export \"_idtx_on_update\") (param f32) (call $bump (f32.const 1)))");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    world.despawn(entity);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(host.instance_count(), 0, "despawned entities must retire their guest stores");
}

#[test]
fn stage_initialization_delivers_loaded_then_play_start_exactly_once() {
    let mut world = World::new();
    let source = world.spawn_empty().id();
    let wire = entity_to_wire(source);
    let bytes = wat::parse_str(format!(r#"(module
        (import "idtx" "idtx_session_set_visibility" (func $edit (param i64 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "_idtx_abi_version") (result i32) i32.const 3)
        (func (export "_idtx_on_stage_event") (param i32)
            (drop (call $edit (i64.const {}) (local.get 0)))))"#, wire.to_bits())).unwrap();
    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
    host.register_module(&bytes, manifest("stage-init")).unwrap();
    host.attach_stage(&mut world, wire, "stage-init").unwrap();
    host.initialize_stage_binding(wire, "stage-init", true);
    host.initialize_stage_binding(wire, "stage-init", true);
    host.run_frame(&mut world, 0.02, PlayState::Playing);
    let edits = host.drain_session_edits();
    assert_eq!(edits.len(), 2);
    assert!(matches!(edits[0].op, SessionEditOp::SetVisibility(false)));
    assert!(matches!(edits[1].op, SessionEditOp::SetVisibility(true)));
    host.initialize_stage_binding(wire, "stage-init", true);
    host.run_frame(&mut world, 0.02, PlayState::Playing);
    assert!(host.drain_session_edits().is_empty());
}

#[test]
fn guest_state_survives_host_replacement_and_requires_level_control() {
    let state = idtx_behavior::ExperienceState::default();
    let writer = counter_module_with_imports(
        r#"(import "idtx" "idtx_state_set" (func $put (param i32 i32 i32 i32) (result i32)))"#,
        r#"(func (export "_idtx_on_update") (param f32)
            (i32.store (i32.const 512) (i32.const 7))
            (if (call $put (i32.const 128) (i32.const 5) (i32.const 512) (i32.const 4)) (then unreachable)))"#);
    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
    host.set_experience_state(state.clone());
    let mut world = World::new();
    host.register_module(&writer, manifest("writer")).unwrap();
    add_actor(&mut host, &mut world, "writer");
    host.run_frame(&mut world, 0.02, PlayState::Playing);
    assert_eq!(state.get("event"), Some(7_u32.to_le_bytes().to_vec()));
    host.detach_all(&mut world);
    drop(host);
    let reader = counter_module_with_imports(
        r#"(import "idtx" "idtx_state_get" (func $get_state (param i32 i32 i32 i32 i32) (result i32)))"#,
        r#"(func (export "_idtx_on_update") (param f32)
            (if (call $get_state (i32.const 128) (i32.const 5) (i32.const 512) (i32.const 4) (i32.const 516)) (then unreachable))
            (if (i32.ne (i32.load (i32.const 512)) (i32.const 7)) (then unreachable))
            (call $bump (f32.const 1)))"#);
    let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
    host.set_experience_state(state);
    host.register_module(&reader, manifest("reader")).unwrap();
    let actor = add_actor(&mut host, &mut world, "reader");
    host.run_frame(&mut world, 0.02, PlayState::Playing);
    assert_eq!(x(&world, actor), 1.0);
    let mut denied = manifest("denied");
    denied.capabilities.level_control = false;
    host.register_module(&reader, denied).unwrap();
    assert!(host.attach(&mut world, entity_to_wire(actor), "denied").is_err());
}

#[test]
fn interact_cannot_resume_a_disabled_guest() {
    let (mut host, mut world, entity) = host_with_counter(TRAPPING_UPDATE);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    world.get_mut::<ecs::Transform>(entity).unwrap().translation.x = 0.0;
    host.interact(&mut world, entity_to_wire(entity));
    assert_eq!(x(&world, entity), 0.0, "interact resumed a disabled guest");
}

#[test]
fn teardown_cannot_resume_a_disabled_guest() {
    let (mut host, mut world, entity) = host_with_counter(TRAPPING_UPDATE);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    world.get_mut::<ecs::Transform>(entity).unwrap().translation.x = 0.0;
    host.detach_all(&mut world);
    assert_eq!(host.instance_count(), 0);
    assert_eq!(x(&world, entity), 0.0, "teardown resumed a disabled guest");
}

#[test]
fn fatal_http_callback_stops_the_rest_of_its_batch() {
    let hooks = r#"
      (func (export "_idtx_on_http_response") (param $id i32) (param i32)
        (if (i32.eq (local.get $id) (i32.const 1)) (then unreachable))
        (call $bump (f32.const 1)))
    "#;
    let (mut host, mut world, entity) = host_with_counter(hooks);
    for request_id in [1, 2] {
        assert!(host.inject_http_completion(entity_to_wire(entity), "counter",
            idtx_behavior::stage_access::HttpCompletion { request_id, status: 200, body: Vec::new() }));
    }
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 0.0, "second completion resumed a disabled guest");
}

#[test]
fn ordinary_update_event_and_interact_still_run() {
    let hooks = r#"
      (func (export "_idtx_on_update") (param f32)
        (call $bump (f32.const 1))
        (drop (call $post (call $self) (i32.const 128) (i32.const 5) (i32.const 0) (i32.const 0))))
      (func (export "_idtx_on_late_update") (param f32) (call $bump (f32.const 2)))
      (func (export "_idtx_on_event") (call $bump (f32.const 4)))
      (func (export "_idtx_on_interact") (call $bump (f32.const 8)))
    "#;
    let (mut host, mut world, entity) = host_with_counter(hooks);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 7.0);
    host.interact(&mut world, entity_to_wire(entity));
    assert_eq!(x(&world, entity), 15.0);
}

#[test]
fn event_posts_are_count_bounded() {
    let mut state = state();
    let rejected = (0..1024).any(|_| state.post_event(WireEntity::NULL, "e".into(), vec![]).is_err());
    assert!(rejected, "event queue accepted every operation in a flood");
}

#[test]
fn event_and_edit_posts_share_capacity() {
    let mut state = state();
    for _ in 0..256 {
        state.post_event(WireEntity::NULL, "e".into(), vec![]).expect("within count cap");
    }
    assert!(state.post_session_edit(WireEntity::NULL, SessionEditOp::RemovePrim).is_err(),
        "switching queue types bypassed the instance's pending-work limit");
}

#[test]
fn oversized_event_and_edit_values_are_rejected() {
    let mut state = state();
    assert!(state.post_event(WireEntity::NULL, "large".into(), vec![0; 128 * 1024]).is_err());
    assert!(state.post_session_edit(WireEntity::NULL, SessionEditOp::SetWidgetValue {
        name: "Value".into(), value: "x".repeat(128 * 1024),
    }).is_err());
    assert!(state.events.is_empty());
    assert!(state.session_edits.is_empty());
}

#[test]
fn widget_handler_registration_is_bounded() {
    let mut state = state();
    state.widget = Some(Arc::new(Mutex::new(WidgetInteraction::default())));
    let rejected = (0..2048).any(|i| state.register_widget_handler(
        WireEntity::from_bits(1), &format!("Button{i}"), WidgetEventKind::Click).is_err());
    assert!(rejected, "widget registry accepted an unbounded set of handlers");
}

#[test]
fn widget_markers_and_pending_clicks_are_bounded() {
    let mut widget = WidgetInteraction::default();
    for _ in 0..8192 {
        let _ = widget.push_marker(idtx_behavior::WidgetMarker {
            position: glam::Vec3::ZERO, normal: glam::Vec3::Z,
        });
    }
    assert!(widget.drain_markers().len() < 8192, "unbounded marker queue");
    for _ in 0..8192 {
        let _ = widget.push_pending(idtx_behavior::PendingWidgetEvent {
            panel: WireEntity::from_bits(1), element: "Button".into(), uv: [0.5, 0.5],
            kind: WidgetEventKind::Click,
        });
    }
    assert!(widget.drain_pending().len() < 8192, "unbounded widget event queue");
}

#[test]
fn host_quota_is_shared_across_instances_and_reusable_after_drain() {
    let hooks = r#"
      (func (export "_idtx_on_attach") (local $n i32)
        (loop $again
          (if (i32.eqz (call $edit (call $self) (i32.const 0)))
            (then (call $bump (f32.const 1))))
          (local.set $n (i32.add (local.get $n) (i32.const 1)))
          (br_if $again (i32.lt_u (local.get $n) (i32.const 4)))))
    "#;
    let mut limits = untimed_limits();
    limits.work.queue_items = 2;
    limits.work.host_queue_items = 3;
    let (mut host, mut world, first) = host_with_limits(hooks, limits);
    let second = add_actor(&mut host, &mut world, "counter");
    assert_eq!(x(&world, first), 2.0);
    assert_eq!(x(&world, second), 1.0, "second instance bypassed the host-wide quota");
    assert_eq!(host.drain_session_edits().len(), 3);
    let third = add_actor(&mut host, &mut world, "counter");
    assert_eq!(x(&world, third), 2.0, "draining must release shared quota");
}

#[test]
fn fatal_guest_releases_queued_work_before_other_instances_run() {
    let mut limits = untimed_limits();
    limits.work.host_queue_items = 1;
    let (mut host, mut world, bad) = host_with_limits(r#"
      (func (export "_idtx_on_update") (param f32)
        (drop (call $edit (call $self) (i32.const 0))) unreachable)
    "#, limits);
    host.register_module(&counter_module(r#"
      (func (export "_idtx_on_update") (param f32)
        (if (i32.eqz (call $edit (call $self) (i32.const 1)))
          (then (call $bump (f32.const 1)))))
    "#), manifest("healthy")).unwrap();
    let healthy = add_actor(&mut host, &mut world, "healthy");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, healthy), 1.0);
    let edits = host.drain_session_edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].target, entity_to_wire(healthy));
    assert!(host.diagnostics(entity_to_wire(bad), "counter").unwrap().2 > 0);
}

#[test]
fn argument_limits_do_not_disable_fixed_size_pod_operations() {
    let mut limits = untimed_limits();
    limits.work.argument_bytes = 8;
    let (mut host, mut world, entity) = host_with_limits(r#"
      (func (export "_idtx_on_update") (param f32)
        (call $bump (f32.convert_i32_u
          (call $post (call $self) (i32.const 128) (i32.const 5) (i32.const 256) (i32.const 100)))))
      (func (export "_idtx_on_event") (call $bump (f32.const 100)))
    "#, limits);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 6.0, "oversized argument must return InvalidArgument; POD writes still work");
}

#[test]
fn invalid_event_payload_is_not_silently_replaced_with_empty_bytes() {
    let (mut host, mut world, entity) = host_with_counter(r#"
      (func (export "_idtx_on_update") (param f32)
        (call $bump (f32.convert_i32_u
          (call $post (call $self) (i32.const 128) (i32.const 5) (i32.const 65535) (i32.const 2)))))
      (func (export "_idtx_on_event") (call $bump (f32.const 100)))
    "#);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 6.0);
}

#[test]
fn widget_registrations_are_idempotent_and_release_when_owner_drops() {
    let widget = Arc::new(Mutex::new(WidgetInteraction::default()));
    let panel = WireEntity::from_bits(7);
    let mut first = state();
    first.widget = Some(widget.clone());
    for i in 0..512 {
        first.register_widget_handler(panel, &format!("B{i}"), WidgetEventKind::Click).unwrap();
    }
    first.register_widget_handler(panel, "B0", WidgetEventKind::Click).unwrap();
    assert!(first.register_widget_handler(panel, "overflow", WidgetEventKind::Click).is_err());
    drop(first);
    assert!(!widget.lock().unwrap().has_handler(panel, "B0", WidgetEventKind::Click));
    let mut second = state();
    second.widget = Some(widget);
    for i in 0..512 {
        second.register_widget_handler(panel, &format!("B{i}"), WidgetEventKind::Click).unwrap();
    }
}

#[test]
fn shared_widget_handler_survives_until_its_last_owner_drops() {
    let widget = Arc::new(Mutex::new(WidgetInteraction::default()));
    let panel = WireEntity::from_bits(7);
    let mut a = state();
    let mut b = state();
    for owner in [&mut a, &mut b] {
        owner.widget = Some(widget.clone());
        owner.register_widget_handler(panel, "B", WidgetEventKind::Click).unwrap();
    }
    drop(a);
    assert!(widget.lock().unwrap().has_handler(panel, "B", WidgetEventKind::Click));
    drop(b);
    assert!(!widget.lock().unwrap().has_handler(panel, "B", WidgetEventKind::Click));
}

fn broadcast_fixture() -> (BehaviorHost, World, [Entity; 3], Entity) {
    let mut limits = untimed_limits();
    limits.work.dispatch_hooks = 2;
    broadcast_fixture_with_limits(limits)
}

fn broadcast_fixture_with_limits(limits: Limits) -> (BehaviorHost, World, [Entity; 3], Entity) {
    let (mut host, mut world, first) = host_with_limits(
        r#"(func (export "_idtx_on_event") (call $bump (f32.const 1)))"#, limits);
    let second = add_actor(&mut host, &mut world, "counter");
    let third = add_actor(&mut host, &mut world, "counter");
    host.register_module(&counter_module(r#"
      (func (export "_idtx_on_attach")
        (drop (call $post (i64.const 0) (i32.const 128) (i32.const 5) (i32.const 0) (i32.const 0))))
    "#), manifest("sender")).unwrap();
    let sender = add_actor(&mut host, &mut world, "sender");
    (host, world, [first, second, third], sender)
}

#[test]
fn broadcast_fanout_resumes_without_replaying_delivered_recipients() {
    let (mut host, mut world, receivers, _) = broadcast_fixture();
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(receivers.map(|e| x(&world, e)), [1.0, 1.0, 0.0]);
    #[cfg(feature = "metrics")]
    assert_eq!(host.dispatch_metrics().hook_calls, 2);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(receivers.map(|e| x(&world, e)), [1.0, 1.0, 1.0]);
}

#[test]
fn a_single_recipient_visit_budget_still_makes_progress() {
    let mut limits = untimed_limits();
    limits.work.dispatch_visits = 1;
    let (mut host, mut world, receivers, _) = broadcast_fixture_with_limits(limits);
    for _ in 0..16 {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        #[cfg(feature = "metrics")]
        assert!(host.dispatch_metrics().visits <= 1);
    }
    assert_eq!(receivers.map(|e| x(&world, e)), [1.0, 1.0, 1.0]);
}

#[test]
fn replacement_does_not_receive_an_inflight_broadcast_for_the_old_instance() {
    let (mut host, mut world, receivers, _) = broadcast_fixture();
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    let replaced = entity_to_wire(receivers[2]);
    host.detach(&mut world, replaced, "counter");
    host.attach(&mut world, replaced, "counter").unwrap();
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(receivers.map(|e| x(&world, e)), [1.0, 1.0, 0.0]);
}

#[test]
fn dropping_sender_cancels_its_remaining_broadcast() {
    let (mut host, mut world, receivers, sender) = broadcast_fixture();
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    host.detach(&mut world, entity_to_wire(sender), "sender");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(receivers.map(|e| x(&world, e)), [1.0, 1.0, 0.0]);
}

#[test]
fn aggregate_fuel_yields_without_disabling_and_rotates_progress() {
    let mut limits = untimed_limits();
    limits.hook_fuel = 50_000;
    limits.work.dispatch_fuel = 10_000;
    let (mut host, mut world, first) = host_with_limits(r#"
      (func (export "_idtx_on_update") (param f32) (local $n i32)
        (local.set $n (i32.const 1000))
        (loop $again
          (if (i32.eqz (local.get $n)) (then (call $bump (f32.const 1)) return))
          (local.set $n (i32.sub (local.get $n) (i32.const 1)))
          br $again))
      (func (export "_idtx_on_interact") (call $bump (f32.const 100)))
    "#, limits);
    let second = add_actor(&mut host, &mut world, "counter");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!([x(&world, first), x(&world, second)], [1.0, 0.0]);
    #[cfg(feature = "metrics")]
    assert_eq!(host.dispatch_metrics().fuel_consumed, 10_000);
    host.interact(&mut world, entity_to_wire(second));
    assert_eq!(x(&world, second), 0.0, "yielded instance resumed before next frame");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!([x(&world, first), x(&world, second)], [1.0, 1.0]);
    for e in [first, second] {
        assert_eq!(host.diagnostics(entity_to_wire(e), "counter").unwrap().2, 0);
    }
}

#[test]
fn overloaded_http_events_and_updates_each_make_progress() {
    let mut limits = untimed_limits();
    limits.work.dispatch_hooks = 1;
    let (mut host, mut world, named) = host_with_limits(r#"
      (func (export "_idtx_on_attach") (local $n i32)
        (loop $again
          (drop (call $post (call $self) (i32.const 128) (i32.const 5) (i32.const 0) (i32.const 0)))
          (local.set $n (i32.add (local.get $n) (i32.const 1)))
          (br_if $again (i32.lt_u (local.get $n) (i32.const 8)))))
      (func (export "_idtx_on_event") (call $bump (f32.const 1)))
    "#, limits);
    host.register_module(&counter_module(r#"
      (func (export "_idtx_on_http_response") (param i32 i32) (call $bump (f32.const 1)))
    "#), manifest("http")).unwrap();
    let http = add_actor(&mut host, &mut world, "http");
    for request_id in 0..8 {
        assert!(host.inject_http_completion(entity_to_wire(http), "http",
            idtx_behavior::stage_access::HttpCompletion { request_id, status: 200, body: Vec::new() }));
    }
    host.register_module(&counter_module(r#"
      (func (export "_idtx_on_update") (param f32) (call $bump (f32.const 1)))
    "#), manifest("update")).unwrap();
    let update = add_actor(&mut host, &mut world, "update");
    for _ in 0..9 { host.run_frame(&mut world, 0.016, PlayState::Playing); }
    for entity in [named, http, update] {
        assert!(x(&world, entity) > 0.0, "a dispatch phase was starved despite repeated frames");
    }
}

#[test]
fn aggregate_epoch_deadline_bounds_guest_execution() {
    common::with_watchdog("aggregate_epoch_deadline_bounds_guest_execution", || {
        let mut limits = untimed_limits();
        limits.hook_fuel = u64::MAX;
        limits.work.dispatch_fuel = u64::MAX;
        limits.work.dispatch_budget = Duration::from_millis(10);
        let (mut host, mut world, entity) = host_with_limits(
            r#"(func (export "_idtx_on_update") (param f32) (loop br 0))"#, limits);
        let start = Instant::now();
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        assert!(start.elapsed() < Duration::from_secs(1));
        let (elapsed_us, _, traps) = host.diagnostics(entity_to_wire(entity), "counter").unwrap();
        assert!(elapsed_us > 0, "guest must actually have run");
        assert_eq!(traps, 0, "aggregate deadline is not an instance fault");
    });
}

#[test]
fn live_instance_cap_allows_replacement_but_rejects_growth() {
    let mut limits = untimed_limits();
    limits.work.instances = 1;
    let (mut host, mut world, first) = host_with_limits("", limits);
    let second = world.spawn_empty().id();
    assert!(host.attach(&mut world, entity_to_wire(second), "counter").is_err());
    host.attach(&mut world, entity_to_wire(first), "counter").unwrap();
    assert_eq!(host.instance_count(), 1);
}

#[cfg(feature = "net")]
#[test]
fn blocking_http_is_explicitly_denied_without_guest_memory_or_io() {
    let wasm = counter_module_with_imports(r#"
      (import "idtx" "idtx_http_request" (func $sync
        (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
    "#, r#"
      (func (export "_idtx_on_update") (param f32)
        (call $bump (f32.convert_i32_u (call $sync
          (i32.const -1) (i32.const -1) (i32.const -1) (i32.const -1)
          (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
          (i32.const 512) (i32.const 128) (i32.const 1024)))))
    "#);
    let mut host = BehaviorHost::new(untimed_limits(), InputRouter::default());
    host.register_module(&wasm, manifest("counter")).unwrap();
    let mut world = World::new();
    let entity = add_actor(&mut host, &mut world, "counter");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 4.0, "legacy HTTP must return CapabilityDenied before reading bad arguments");
}

#[cfg(feature = "net")]
#[test]
fn async_http_backpressure_rejects_before_spawning_and_recovers_after_drain() {
    let wasm = counter_module_with_imports(r#"
      (import "idtx" "idtx_http_request_async" (func $async
        (param i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
    "#, r#"
      (data (i32.const 256) "GET")
      (data (i32.const 264) "https://example.invalid")
      (global $sent (mut i32) (i32.const 0))
      (func (export "_idtx_on_attach") (drop (call $edit (call $self) (i32.const 0))))
      (func (export "_idtx_on_update") (param f32) (local $result i32)
        (if (global.get $sent) (then return))
        (local.set $result (call $async (i32.const 1) (i32.const 256) (i32.const 3)
          (i32.const 264) (i32.const 23) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)))
        (call $bump (f32.convert_i32_u (local.get $result)))
        (if (i32.eqz (local.get $result)) (then (global.set $sent (i32.const 1)))))
      (func (export "_idtx_on_http_response") (param i32 i32) (call $bump (f32.const 100)))
    "#);
    let mut limits = untimed_limits();
    limits.work.queue_items = 1;
    let mut host = BehaviorHost::new(limits, InputRouter::default());
    host.register_module(&wasm, manifest("counter")).unwrap();
    let mut world = World::new();
    let entity = add_actor(&mut host, &mut world, "counter");
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 6.0);
    assert_eq!(host.drain_session_edits().len(), 1);
    // Network policy is denied: the real worker returns status 0 without DNS or
    // network I/O. The accepted request still receives exactly one callback.
    let deadline = Instant::now() + Duration::from_secs(2);
    while x(&world, entity) < 106.0 {
        host.run_frame(&mut world, 0.016, PlayState::Playing);
        assert!(Instant::now() < deadline, "reserved HTTP callback was lost");
        std::thread::sleep(Duration::from_millis(1));
    }
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    assert_eq!(x(&world, entity), 106.0);
}

struct PanelHit;

impl idtx_behavior::SceneBackend for PanelHit {
    fn raycast(&self, _: &mut World, _: &idtx_behavior_abi::RaycastParams) -> idtx_behavior_abi::RayHitResult {
        idtx_behavior_abi::RayHitResult::default()
    }
    fn arc_cast(&self, _: &mut World, _: &idtx_behavior_abi::ArcCastParams, _: &mut [glam::Vec3]) -> idtx_behavior_abi::ArcHitResult {
        idtx_behavior_abi::ArcHitResult::default()
    }
    fn find_by_type(&self, _: &mut World, _: u64, out: &mut Vec<Entity>) { out.clear(); }
    fn path_of(&self, _: &World, _: Entity) -> Option<String> { None }
    fn widget_hit(&self, _: &World, _: Entity, position: glam::Vec3) -> Option<idtx_behavior::WidgetHitInfo> {
        Some(idtx_behavior::WidgetHitInfo {
            element: Some("Button".into()), uv: [0.5, 0.5], position, normal: glam::Vec3::Z,
        })
    }
}

fn widget_frame(queue_items: usize, trap_after_handle: bool) -> (f32, usize) {
    let hooks = format!(r#"
      (data (i32.const 256) "Button")
      (func (export "_idtx_on_attach")
        (drop (call $register (call $self) (i32.const 256) (i32.const 6) (i32.const {}))))
      (func (export "_idtx_on_update") (param f32)
        (call $bump (f32.convert_i32_u (call $handle (call $self) (f32.const 0) (f32.const 0) (f32.const 0))))
        {})
      (func (export "_idtx_on_event") (call $bump (f32.const 10)))
    "#, WidgetEventKind::Click.as_u32(), if trap_after_handle { "unreachable" } else { "" });
    let wasm = counter_module_with_imports(r#"
      (import "idtx" "idtx_widget_register_handler" (func $register (param i64 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_widget_handle" (func $handle (param i64 f32 f32 f32) (result i32)))
    "#, &hooks);
    let mut limits = untimed_limits();
    limits.work.queue_items = queue_items;
    let map = idtx_behavior::InputMap::parse(r#"
      [actions."widgets.click"]
      kind = "button"
      bindings = ["Key/Space"]
    "#).unwrap();
    let mut host = BehaviorHost::new(limits, InputRouter::new(map));
    host.set_scene_backend(Arc::new(PanelHit));
    let mut manifest = manifest("counter");
    manifest.inputs.push(idtx_behavior::InputDecl {
        action: "widgets.click".into(), kind: idtx_behavior::InputKind::Button,
    });
    host.register_module(&wasm, manifest).unwrap();
    let mut world = World::new();
    let entity = add_actor(&mut host, &mut world, "counter");
    let mut input = idtx_behavior::FrameInput::new();
    input.press("Key/Space");
    host.update_input(&input.raw(), 0.016);
    host.run_frame(&mut world, 0.016, PlayState::Playing);
    (x(&world, entity), host.drain_widget_markers().len())
}

#[test]
fn widget_reports_consumed_only_when_the_click_was_queued() {
    assert_eq!(widget_frame(1, false), (0.0, 1), "marker fits, click does not; must report unconsumed");
    assert_eq!(widget_frame(2, false), (11.0, 1), "admitted click reaches its callback");
}

#[test]
fn fatal_widget_publisher_cancels_clicks_and_markers() {
    // The direct counter write before the trap is not transactional; the queued
    // callback (+10) and marker must be cancelled when that publisher disables.
    assert_eq!(widget_frame(2, true), (1.0, 0));
}
