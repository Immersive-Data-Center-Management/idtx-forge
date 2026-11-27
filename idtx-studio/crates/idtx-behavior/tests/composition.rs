//! Real guest imports: event ordering/bounds, composed target snapshots and
//! widget consumption. No USD or wasm toolchain required.
use bevy_ecs::prelude::*;
use idtx_behavior::{
    abi::{ErrorCode, RelationshipTarget},
    *,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn limits() -> Limits {
    let mut limits = Limits::runtime();
    limits.work.dispatch_budget = Duration::from_secs(10);
    limits.hook_budget = Duration::from_secs(10);
    limits
}
fn manifest(uuid: &str, caps: Capabilities) -> BehaviorManifest {
    BehaviorManifest {
        name: uuid.into(),
        version: "1".into(),
        uuid: uuid.into(),
        kind: abi::BehaviorKind::Runtime,
        ship: true,
        params: BTreeMap::new(),
        inputs: vec![],
        capabilities: caps,
        memory_mb: None,
    }
}
fn module(imports: &str, hooks: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_playback_command" (func $record (param i64 i32 i32 i32 f32) (result i32)))
      {imports}
      (memory (export "memory") 2)
      ;; Reserved clip label for the test report side-channel. `idtx_playback_command`
      ;; rejects an empty clip (InvalidArgument) at both the linker and HostState
      ;; guards, so reports must name a non-empty clip; only `value` is ever read
      ;; back by `values()`. Placed at 1024 — clear of every hook's scratch writes.
      (data (i32.const 1024) "report")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func $report (param $v i32) (drop (call $record (call $self) (i32.const 1024) (i32.const 6) (i32.const 3) (f32.convert_i32_u (local.get $v)))))
      {hooks})"#)).unwrap()
}
const POST: &str =
    r#"(import "idtx" "idtx_event_post" (func $post (param i64 i32 i32 i32 i32) (result i32)))"#;
fn values(host: &mut BehaviorHost) -> Vec<u32> {
    host.drain_playback_commands()
        .into_iter()
        .map(|c| c.value as u32)
        .collect()
}

#[test]
fn updates_fifo_and_callback_posts_wait_for_next_sweep() {
    let wasm = module(
        &format!(
            r#"{POST}
      (import "idtx" "idtx_event_current_payload" (func $payload (param i32 i32) (result i32)))"#
        ),
        r#"
      (data (i32.const 0) "event") (data (i32.const 32) "\01\02\03")
      (global $sent (mut i32) (i32.const 0))
      (func (export "_idtx_on_update") (param f32)
        (if (i32.eqz (global.get $sent)) (then
          (drop (call $post (call $self) (i32.const 0) (i32.const 5) (i32.const 32) (i32.const 1))))))
      (func (export "_idtx_on_late_update") (param f32)
        (if (i32.eqz (global.get $sent)) (then
          (global.set $sent (i32.const 1))
          (drop (call $post (call $self) (i32.const 0) (i32.const 5) (i32.const 33) (i32.const 1))))))
      (func (export "_idtx_on_event")
        (drop (call $payload (i32.const 100) (i32.const 1)))
        (call $report (i32.load8_u (i32.const 100)))
        (if (i32.eq (i32.load8_u (i32.const 100)) (i32.const 1)) (then
          (drop (call $post (call $self) (i32.const 0) (i32.const 5) (i32.const 34) (i32.const 1))))))"#,
    );
    let mut host = BehaviorHost::new(limits(), Default::default());
    host.register_module(&wasm, manifest("fifo", Capabilities::all()))
        .unwrap();
    let mut world = World::new();
    let actor = entity_to_wire(world.spawn_empty().id());
    host.attach(&mut world, actor, "fifo").unwrap();
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert_eq!(values(&mut host), [1, 2]);
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert_eq!(values(&mut host), [3]);
}

#[test]
fn feedback_cascade_yields_to_the_existing_dispatch_budget() {
    let wasm = module(POST, r#"
      (data (i32.const 0) "feedback")
      (func $send (drop (call $post (call $self) (i32.const 0) (i32.const 8) (i32.const 0) (i32.const 0))))
      (func (export "_idtx_on_attach") call $send)
      (func (export "_idtx_on_event") call $send (call $report (i32.const 1)))"#);
    let mut limits = limits();
    limits.work.dispatch_hooks = 1;
    limits.work.queue_items = 4;
    let mut host = BehaviorHost::new(limits, Default::default());
    host.register_module(&wasm, manifest("feedback", Capabilities::all())).unwrap();
    let mut world = World::new();
    let actor = entity_to_wire(world.spawn_empty().id());
    host.attach(&mut world, actor, "feedback").unwrap();
    for _ in 0..5 {
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert_eq!(values(&mut host), [1], "feedback must not recursively dispatch or replay a callback");
    }
}

#[test]
fn imports_preserve_capability_and_argument_errors() {
    let wasm = module(
        POST,
        r#"
      (data (i32.const 0) "event")
      (func (export "_idtx_on_attach")
        (call $report (call $post (call $self) (i32.const 0) (i32.const 5) (i32.const 64) (i32.const 8192)))
        (call $report (call $post (call $self) (i32.const 0) (i32.const 5) (i32.const 64) (i32.const 65537)))
        (call $report (call $post (call $self) (i32.const -1) (i32.const 5) (i32.const 64) (i32.const 0)))
        (call $report (call $post (call $self) (i32.const 0) (i32.const 257) (i32.const 64) (i32.const 0)))
        (call $report (call $post (i64.const -1) (i32.const 0) (i32.const 5) (i32.const 64) (i32.const 0))))"#,
    );
    let report = verify_link(&wasm, Capabilities::from_tokens(["playback"])).unwrap();
    assert!(!report.links);
    assert!(report.missing.events);
    let mut host = BehaviorHost::new(limits(), Default::default());
    host.register_module(&wasm, manifest("bounds", Capabilities::all()))
        .unwrap();
    let mut world = World::new();
    let actor = entity_to_wire(world.spawn_empty().id());
    host.attach(&mut world, actor, "bounds").unwrap();
    assert_eq!(values(&mut host), [0, 6, 6, 6, 1]);
    let mut state = HostState::new(
        Capabilities::default(),
        actor,
        wasmtime::StoreLimitsBuilder::new().build(),
    );
    assert_eq!(
        state.post_event(actor, "event".into(), vec![]),
        Err(ErrorCode::CapabilityDenied)
    );
}

#[test]
fn unloaded_and_recycled_targets_cannot_receive_pending_events() {
    for unload in [false, true] {
        let mut world = World::new();
        let recipient = world.spawn_empty().id();
        let sender = world.spawn_empty().id();
        let mut host = BehaviorHost::new(limits(), Default::default());
        let sender_wasm = module(
            POST,
            &format!(
                r#"
          (data (i32.const 0) "event")
          (func (export "_idtx_on_attach")
            (call $report (call $post (i64.const {}) (i32.const 0) (i32.const 5) (i32.const 0) (i32.const 0))))
          (func (export "_idtx_on_update") (param f32)
            (call $report (call $post (i64.const {}) (i32.const 0) (i32.const 5) (i32.const 0) (i32.const 0))))"#,
                recipient.to_bits(),
                recipient.to_bits()
            ),
        );
        host.register_module(&sender_wasm, manifest("sender", Capabilities::all()))
            .unwrap();
        host.register_module(
            &module(
                "",
                "(func (export \"_idtx_on_event\") (call $report (i32.const 99)))",
            ),
            manifest("receiver", Capabilities::all()),
        )
        .unwrap();
        host.attach(&mut world, entity_to_wire(recipient), "receiver")
            .unwrap();
        host.attach(&mut world, entity_to_wire(sender), "sender")
            .unwrap();
        assert_eq!(values(&mut host), [0]);
        if unload {
            world.entity_mut(recipient).insert(ecs::Unavailable);
        } else {
            world.despawn(recipient);
            world.spawn_empty();
        }
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert_eq!(values(&mut host), [if unload { 2 } else { 1 }]);
    }
}

#[test]
fn relationship_import_reports_order_count_revision_and_status_without_truncating_silently() {
    let mut world = World::new();
    let target = entity_to_wire(world.spawn_empty().id());
    let actor = entity_to_wire(
        world
            .spawn(ecs::Relationships(BTreeMap::from([
                (
                    "coordinator".into(),
                    ecs::RelationshipBinding {
                        revision: 42,
                        resolved: true,
                        targets: vec![
                            RelationshipTarget {
                                entity: target,
                                status: 0,
                                reserved: 0,
                            },
                            RelationshipTarget {
                                status: 2,
                                ..Default::default()
                            },
                            RelationshipTarget {
                                status: 1,
                                ..Default::default()
                            },
                        ],
                    },
                ),
                (
                    "empty".into(),
                    ecs::RelationshipBinding {
                        revision: 43,
                        resolved: true,
                        targets: vec![],
                    },
                ),
            ])))
            .id(),
    );
    let wasm = module(
        r#"(import "idtx" "idtx_relationship_targets" (func $rel (param i64 i32 i32 i32 i32 i32) (result i32)))"#,
        &format!(
            r#"
      (data (i32.const 0) "coordinatoremptymissing")
      (func (export "_idtx_on_attach")
        (call $report (call $rel (call $self) (i32.const 0) (i32.const 11) (i32.const 100) (i32.const 2) (i32.const 200)))
        (call $report (i32.load (i32.const 200)))
        (call $report (i32.load (i32.const 208)))
        (call $report (i32.load (i32.const 212)))
        (call $report (i64.eq (i64.load (i32.const 100)) (i64.const {})))
        (call $report (i32.load (i32.const 124)))
        (call $report (call $rel (call $self) (i32.const 11) (i32.const 5) (i32.const 100) (i32.const 0) (i32.const 200)))
        (call $report (i32.load (i32.const 208)))
        (call $report (call $rel (call $self) (i32.const 16) (i32.const 7) (i32.const 100) (i32.const 0) (i32.const 200)))
        (call $report (call $rel (call $self) (i32.const 0) (i32.const 11) (i32.const -1) (i32.const 2) (i32.const 200))))"#,
            target.to_bits()
        ),
    );
    assert!(
        verify_link(&wasm, Capabilities::from_tokens(["playback"]))
            .unwrap()
            .missing
            .scene_query
    );
    let mut host = BehaviorHost::new(limits(), Default::default());
    host.register_module(&wasm, manifest("bindings", Capabilities::all()))
        .unwrap();
    host.attach(&mut world, actor, "bindings").unwrap();
    assert_eq!(values(&mut host), [0, 42, 3, 2, 1, 2, 0, 0, 2, 6]);
}

struct PanelHit;
impl SceneBackend for PanelHit {
    fn raycast(&self, _: &mut World, _: &abi::RaycastParams) -> abi::RayHitResult {
        Default::default()
    }
    fn arc_cast(
        &self,
        _: &mut World,
        _: &abi::ArcCastParams,
        _: &mut [glam::Vec3],
    ) -> abi::ArcHitResult {
        Default::default()
    }
    fn find_by_type(&self, _: &mut World, _: u64, _: &mut Vec<Entity>) {}
    fn path_of(&self, _: &World, _: Entity) -> Option<String> {
        None
    }
    fn widget_hit(&self, _: &World, _: Entity, position: glam::Vec3) -> Option<WidgetHitInfo> {
        Some(WidgetHitInfo {
            element: Some("Start".into()),
            uv: [0.5, 0.5],
            position,
            normal: glam::Vec3::Z,
        })
    }
}

#[test]
fn shared_button_consumed_by_widget_yields_one_start_transition() {
    let map = InputMap::parse("[actions.\"widgets.click\"]\nkind=\"button\"\nbindings=[\"Key/Space\"]\n[actions.select]\nkind=\"button\"\nbindings=[\"Key/Space\"]").unwrap();
    let mut host = BehaviorHost::new(limits(), InputRouter::new(map));
    host.set_scene_backend(Arc::new(PanelHit));
    let mut world = World::new();
    let panel = world.spawn_empty().id();
    let pointer = world.spawn_empty().id();
    let panel_wasm = module(
        &format!(
            r#"{POST}
      (import "idtx" "idtx_widget_register_handler" (func $register (param i64 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_widget_event_kind" (func $kind (result i32)))"#
        ),
        r#"
      (data (i32.const 0) "Start") (global $running (mut i32) (i32.const 0))
      (func (export "_idtx_on_attach") (drop (call $register (call $self) (i32.const 0) (i32.const 5) (i32.const 0))))
      (func (export "_idtx_on_event")
        (if (i32.eqz (call $kind)) (then
          (drop (call $post (call $self) (i32.const 0) (i32.const 5) (i32.const 0) (i32.const 0))))
        (else (call $report (i32.const 1)))))"#,
    );
    // World selection would post a second Start on the same physical edge if
    // this pointer ignored the synchronous consumed result.
    let pointer_wasm = module(
        &format!(
            r#"{POST}
      (import "idtx" "idtx_widget_handle" (func $handle (param i64 f32 f32 f32) (result i32)))"#
        ),
        &format!(
            r#"
      (data (i32.const 0) "Start") (global $once (mut i32) (i32.const 0))
      (func (export "_idtx_on_update") (param f32)
        (if (global.get $once) (then return)) (global.set $once (i32.const 1))
        (if (call $handle (i64.const {0}) (f32.const 0) (f32.const 0) (f32.const 0)) (then return))
        (drop (call $post (i64.const {0}) (i32.const 0) (i32.const 5) (i32.const 0) (i32.const 0))))"#,
            panel.to_bits()
        ),
    );
    host.register_module(&panel_wasm, manifest("panel", Capabilities::all()))
        .unwrap();
    let mut pointer_manifest = manifest("pointer", Capabilities::all());
    pointer_manifest.inputs.push(InputDecl {
        action: "widgets.click".into(),
        kind: InputKind::Button,
    });
    pointer_manifest.inputs.push(InputDecl {
        action: "select".into(),
        kind: InputKind::Button,
    });
    host.register_module(&pointer_wasm, pointer_manifest)
        .unwrap();
    host.attach(&mut world, entity_to_wire(panel), "panel")
        .unwrap();
    host.attach(&mut world, entity_to_wire(pointer), "pointer")
        .unwrap();
    let mut input = FrameInput::new();
    input.press("Key/Space");
    host.update_input(&input.raw(), 0.01);
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert!(
        values(&mut host).is_empty(),
        "widget callback posts wait for next sweep"
    );
    host.run_frame(&mut world, 0.01, PlayState::Playing);
    assert_eq!(values(&mut host), [1]);
}

#[test]
fn modal_capture_precedes_consumers_and_guards_open_and_release_edges() {
    let map = InputMap::parse(r#"
[actions.open]
kind="button"
bindings=["Controller/MetaQuest/RightTrigger"]
[actions.confirm]
kind="button"
bindings=["Controller/MetaQuest/RightTrigger"]
[actions.select]
kind="button"
bindings=["Controller/MetaQuest/RightTrigger"]
[actions.turn]
kind="axis"
bindings=["Controller/MetaQuest/RightThumbstickX"]
"#).unwrap();
    let open = map.resolve("open").0;
    let confirm = map.resolve("confirm").0;
    let select = map.resolve("select").0;
    let turn = map.resolve("turn").0;
    for reverse in [false, true] {
        let mut host = BehaviorHost::new(limits(), InputRouter::new(map.clone()));
        let mut world = World::new();
        let menu = entity_to_wire(world.spawn_empty().id());
        let pawn = entity_to_wire(world.spawn_empty().id());
        let imports = r#"
            (import "idtx" "idtx_input_phase_from" (func $phase (param i32 i32) (result i32)))
            (import "idtx" "idtx_input_capture" (func $capture (param i64 i64 i32 i32 i32 i32) (result i64)))
            (import "idtx" "idtx_input_capture_active" (func $active (param i64) (result i32)))
            (import "idtx" "idtx_input_capture_release" (func $release (param i64)))"#;
        let wasm = module(imports, &format!(r#"
            (global $token (mut i64) (i64.const 0))
            (func (export "_idtx_on_input_capture")
                (if (i64.ne (global.get $token) (i64.const 0)) (then return))
                (if (i32.ne (call $phase (i32.const {open}) (i32.const 3)) (i32.const 1)) (then return))
                (i32.store (i32.const 0) (i32.const {open}))
                (i32.store (i32.const 4) (i32.const {confirm}))
                (i32.store (i32.const 8) (i32.const {select}))
                (i32.store (i32.const 12) (i32.const {turn}))
                (global.set $token (call $capture (i64.const 0) (call $self) (i32.const 3) (i32.const 100) (i32.const 0) (i32.const 4))))
            (func (export "_idtx_on_update") (param f32)
                (if (i32.and (call $active (global.get $token))
                    (i32.eq (call $phase (i32.const {confirm}) (i32.const 3)) (i32.const 1))) (then
                    (call $report (i32.const 7)) (call $release (global.get $token)))))
        "#));
        let lower = module(r#"
            (import "idtx" "idtx_input_action_phase" (func $phase (param i32) (result i32)))
            (import "idtx" "idtx_input_axis" (func $axis (param i32) (result f32)))"#,
            &format!(r#"(func (export "_idtx_on_update") (param f32)
                (if (f32.gt (call $axis (i32.const {turn})) (f32.const 0)) (then (call $report (i32.const 9))))
                (if (i32.eq (call $phase (i32.const {select})) (i32.const 1)) (then (call $report (i32.const 99)))))"#));
        for (name, wasm) in [("menu", wasm), ("pawn", lower)] {
            let mut m = manifest(name, Capabilities::all());
            for (action, kind) in [("open", InputKind::Button), ("confirm", InputKind::Button), ("select", InputKind::Button), ("turn", InputKind::Axis)] {
                m.inputs.push(InputDecl { action: action.into(), kind });
            }
            host.register_module(&wasm, m).unwrap();
        }
        for (e, name) in if reverse { [(menu, "menu"), (pawn, "pawn")] } else { [(pawn, "pawn"), (menu, "menu")] } {
            host.attach(&mut world, e, name).unwrap();
        }
        let mut xr = XrSnapshot::default();
        xr.set(abi::XrSpace::RightAim, abi::XrPoseResult::tracked(abi::Xform::default()));
        host.set_xr_snapshot(xr);
        let mut input = FrameInput::new();
        input.press("Controller/MetaQuest/RightTrigger");
        input.set_analog("Controller/MetaQuest/RightThumbstickX", 0.8);
        host.update_input(&input.raw(), 0.01);
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert!(values(&mut host).is_empty(), "opening cannot confirm/select/turn");
        input.end_frame();
        input.release("Controller/MetaQuest/RightTrigger");
        host.update_input(&input.raw(), 0.01);
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert!(values(&mut host).is_empty());
        input.end_frame();
        input.press("Controller/MetaQuest/RightTrigger");
        host.update_input(&input.raw(), 0.01);
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert_eq!(values(&mut host), [7], "confirm only, independent of consumer order");
        input.end_frame();
        input.set_analog("Controller/MetaQuest/RightThumbstickX", 0.0);
        host.update_input(&input.raw(), 0.01);
        host.run_frame(&mut world, 0.01, PlayState::Playing);
        assert!(values(&mut host).is_empty(), "still-held confirmation cannot press after release");
        assert!(host.input_focus_owners().is_empty());
    }
}
