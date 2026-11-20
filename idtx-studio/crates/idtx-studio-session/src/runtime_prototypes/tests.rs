//! Real USD + WASM integration of projection, pinned context and teardown.
use super::*;
use crate::{RenderWorldUnitScale, TextureDecodeConfig};
use idtx_behavior::{BehaviorManifest, Capabilities, Limits, SessionEditCommand};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    session: StudioSession,
    dir: PathBuf,
}
impl Fixture {
    fn open() -> Self {
        idtx_usd_extension::install().unwrap();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "idtx-reference-session-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stage.usda"), r#"#usda 1.0
(defaultPrim = "World"
 upAxis = "Y"
 metersPerUnit = 1)
def Xform "World" {
    def Xform "A" (customData = { dictionary idtx = { dictionary assetOverview = { string label = "A"
        float[] offset = [1, 2, 3] } } }) {
        def Scope "Data" {
            custom string outputs:value = "A"
        }
        def Scope "Status" {
            custom string idtx:widget:value = ""
        }
    }
    def Xform "B" {
        def Scope "Data" {
            custom string outputs:value = "B"
        }
        def Scope "Status" {
            custom string idtx:widget:value = ""
        }
    }
}
"#).unwrap();
        fs::write(
            dir.join("template.usda"),
            r#"#usda 1.0
(defaultPrim = "Template")
def IdtxWidgetPanel "Template" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {
    float2 idtx:widget:size = (1, 0.4)
    int2 idtx:widget:resolution = (128, 64)
    uniform token idtx:widget:billboard = "none"
    token idtx:behavior:kind = "runtime"
    string idtx:behavior:name = "panel"
    string idtx:behavior:uuid = "panel"
    string idtx:behavior:version = "1"
    asset idtx:behavior:module = @panel.wasm@
    uniform token[] idtx:behavior:capabilities = ["stage_edit"]
    def IdtxWidgetFieldString "Value" {
        string idtx:widget:value = ""
    }
    def Scope "Bound" {
        custom string inputs:value = ""
    }
}
"#,
        )
        .unwrap();
        let panel = wat::parse_str(r#"(module
          (import "idtx" "idtx_entity_self" (func $self (result i64)))
          (import "idtx" "idtx_behavior_context" (func $context (param i32 i32 i32) (result i32)))
          (import "idtx" "idtx_session_set_widget_value" (func $value (param i64 i32 i32 i32 i32) (result i32)))
          (import "idtx" "idtx_hook_result" (func $result (param i32)))
          (memory (export "memory") 1) (data (i32.const 0) "Value")
          (func (export "_idtx_abi_version") (result i32) i32.const 3)
          (func (export "_idtx_on_attach")
            (call $result (call $context (i32.const 10000) (i32.const 8192) (i32.const 19000)))
            (if (i32.and (i32.ne (i32.load8_u (i32.const 10010)) (i32.const 65))
                         (i32.ne (i32.load8_u (i32.const 10010)) (i32.const 66)))
              (then (call $result (i32.const 3)))))
          (func (export "_idtx_on_update") (param f32)
            (call $result (call $context (i32.const 10000) (i32.const 8192) (i32.const 19000)))
            (drop (call $value (call $self) (i32.const 0) (i32.const 5) (i32.const 10010) (i32.const 1)))))"#).unwrap();
        fs::write(dir.join("panel.wasm"), panel).unwrap();
        let (device, queue) = pollster::block_on(async {
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .unwrap();
            adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("reference-session-test"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::default(),
                })
                .await
                .unwrap()
        });
        let mut session = StudioSession::open_with_gpu(
            dir.join("stage.usda"),
            device,
            queue,
            [64, 64],
            TextureDecodeConfig::default(),
            RenderWorldUnitScale::METRE,
        )
        .unwrap();
        session.set_behavior_limits(Limits::runtime());
        session.set_runtime_prototypes([(
            "overview".into(),
            dir.join("template.usda"),
            "/Template".into(),
        )]);
        session.attach_authored_behaviors(|_| None);
        session.set_play_state(PlayState::Playing);
        Self { session, dir }
    }
    fn requester(&mut self, parent: &str, label: &str) {
        let payload = format!(
            r#"{{"context":{{"label":"{label}"}},"connections":[{{"child":"Bound","property":"inputs:value","source":"Data.outputs:value"}}]}}"#
        );
        let encoded: String = payload
            .bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect();
        let source = format!(
            r#"(module
          (import "idtx" "idtx_entity_self" (func $self (result i64)))
          (import "idtx" "idtx_session_spawn_reference" (func $spawn (param i64 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
          (import "idtx" "idtx_session_spawn_status" (func $status (param i64 i32) (result i32)))
          (import "idtx" "idtx_session_set_widget_value" (func $value (param i64 i32 i32 i32 i32) (result i32)))
          (import "idtx" "idtx_hook_result" (func $result (param i32)))
          (memory (export "memory") 1)
          (data (i32.const 0) "Panel") (data (i32.const 10) "overview") (data (i32.const 30) "Status")
          (data (i32.const 50) "0123") (data (i32.const 100) "{encoded}")
          (func (export "_idtx_abi_version") (result i32) i32.const 3)
          (func (export "_idtx_on_start")
            (f32.store (i32.const 1024) (f32.const 1))
            (f32.store (i32.const 1028) (f32.const 1))
            (f32.store (i32.const 1032) (f32.const 1))
            (f32.store (i32.const 1036) (f32.const 1))
            (call $result (call $spawn (call $self) (i32.const 0) (i32.const 5) (i32.const 10) (i32.const 8)
              (i32.const 1000) (i32.const 100) (i32.const {length}) (i32.const 1100))))
          (func (export "_idtx_on_update") (param f32)
            (if (i32.eqz (call $status (i64.load (i32.const 1100)) (i32.const 1200)))
              (then (drop (call $value (call $self) (i32.const 30) (i32.const 6)
                (i32.add (i32.const 50) (i32.load (i32.const 1208))) (i32.const 1)))))))"#,
            length = payload.len()
        );
        let uuid = format!("request-{parent}");
        let bytes = wat::parse_str(source).unwrap();
        self.session
            .behaviors
            .register_module(
                &bytes,
                BehaviorManifest {
                    name: uuid.clone(),
                    version: "1".into(),
                    uuid: uuid.clone(),
                    kind: idtx_behavior_abi::BehaviorKind::Runtime,
                    ship: true,
                    params: Default::default(),
                    inputs: vec![],
                    capabilities: Capabilities::from_tokens(["stage_edit"]),
                    memory_mb: None,
                },
            )
            .unwrap();
        let entity = self
            .session
            .hydrator
            .entity_for_path(&format!("/World/{parent}"))
            .unwrap();
        self.session
            .behaviors
            .attach(
                self.session.hydrator.world_mut(),
                idtx_behavior::entity_to_wire(entity),
                &uuid,
            )
            .unwrap();
    }
    fn frame(&mut self) -> Vec<SessionEditCommand> {
        self.session.behaviors.run_frame(
            self.session.hydrator.world_mut(),
            0.01,
            self.session.play_state,
        );
        self.session.behaviors.drain_session_edits()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.dir.starts_with(std::env::temp_dir()) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}
fn statuses(edits: &[SessionEditCommand]) -> Vec<&str> {
    edits
        .iter()
        .filter_map(|edit| match &edit.op {
            SessionEditOp::SetWidgetValue { name, value } if name == "Status" => {
                Some(value.as_str())
            }
            _ => None,
        })
        .collect()
}

fn descendant_template(fixture: &Fixture, options: &str, hook: &str) {
    let kind = if options.contains("idtx:behavior:kind") {
        ""
    } else {
        "token idtx:behavior:kind = \"runtime\""
    };
    let module = if options.contains("idtx:behavior:module") {
        ""
    } else {
        "asset idtx:behavior:module = @child.wasm@"
    };
    fs::write(
        fixture.dir.join("template.usda"),
        format!(
            r#"#usda 1.0
(defaultPrim = "Template")
def Xform "Template" {{
    def Scope "Bound" {{
        custom string inputs:value = ""
    }}
    def Xform "Child" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {{
        {kind}
        string idtx:behavior:name = "child"
        string idtx:behavior:uuid = "child"
        string idtx:behavior:version = "1"
        {module}
        {options}
    }}
}}
"#
        ),
    )
    .unwrap();
    let bytes = wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 1)
        (func (export "_idtx_abi_version") (result i32) i32.const 3)
        {hook})"#
    ))
    .unwrap();
    fs::write(fixture.dir.join("child.wasm"), bytes).unwrap();
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn descendant_startup_failures_never_make_a_behavior_free_root_ready() {
    for hook in ["_idtx_on_attach", "_idtx_on_start"] {
        let mut fixture = Fixture::open();
        descendant_template(
            &fixture,
            "",
            &format!("(func (export \"{hook}\") unreachable)"),
        );
        let before = fixture.session.service.root_layer_text().unwrap();
        fixture.requester("A", "A");
        let edits = fixture.frame();
        fixture.session.apply_session_edits(&edits);
        fixture.session.settle_runtime_spawns();
        assert_eq!(statuses(&fixture.frame()), ["1"]);
        assert_eq!(fixture.session.attach_authored_behaviors(|_| None), 1);
        // Entity on_start is dispatched on the next behavior frame, not attach.
        let _ = fixture.frame();
        fixture.session.settle_runtime_spawns();
        assert_eq!(statuses(&fixture.frame()), ["3"]);
        assert!(
            fixture
                .session
                .runtime_entity_for_path("/World/A/Panel")
                .is_none()
        );
        assert!(
            fixture
                .session
                .runtime_entity_for_path("/World/A/Panel/Child")
                .is_none()
        );
        assert_eq!(fixture.session.service.root_layer_text().unwrap(), before);
    }
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn healthy_descendant_waits_for_start_and_idle_settlement_reuses_requirements() {
    let mut fixture = Fixture::open();
    descendant_template(&fixture, "", "");
    fixture.requester("A", "A");
    let edits = fixture.frame();
    fixture.session.apply_session_edits(&edits);
    fixture.session.settle_runtime_spawns();
    assert_eq!(statuses(&fixture.frame()), ["1"]);
    // Attachment alone does not dispatch the entity's on_start hook.
    let bytes = fs::read(fixture.dir.join("child.wasm")).unwrap();
    let manifest = fixture
        .session
        .behavior_manifest_from_schema("/World/A/Panel/Child")
        .unwrap();
    crate::behavior::attach_authored_behaviors(
        &mut fixture.session.behaviors,
        fixture.session.hydrator.world_mut(),
        |_| true,
        |_, _| Some(bytes.clone()),
        |_, _| Some(manifest.clone()),
    );
    fixture.session.behavior_reconcile_pending = false;
    fixture.session.settle_runtime_spawns();
    let state = fixture
        .session
        .hydrator
        .world()
        .resource::<RuntimePrototypes>();
    assert!(
        !state.owned["/World/A/Panel"].ready,
        "Attached is not Started"
    );
    assert_eq!(
        state.owned["/World/A/Panel"]
            .required_bindings
            .as_ref()
            .unwrap()
            .2
            .len(),
        1
    );
    let _ = fixture.frame();
    fixture.session.settle_runtime_spawns();
    assert_eq!(statuses(&fixture.frame()), ["2"]);
    let pointer = fixture
        .session
        .hydrator
        .world()
        .resource::<RuntimePrototypes>()
        .owned["/World/A/Panel"]
        .required_bindings
        .as_ref()
        .unwrap()
        .2
        .as_ptr();
    for _ in 0..10 {
        fixture.session.settle_runtime_spawns();
    }
    let state = fixture
        .session
        .hydrator
        .world()
        .resource::<RuntimePrototypes>();
    assert!(state.owned["/World/A/Panel"].ready);
    assert_eq!(
        state.owned["/World/A/Panel"]
            .required_bindings
            .as_ref()
            .unwrap()
            .2
            .as_ptr(),
        pointer
    );
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn descendant_eligibility_matches_attachment_and_missing_modules_fail() {
    for (options, ready) in [
        ("bool idtx:behavior:enabled = false", true),
        (
            "bool idtx:behavior:enabled = false\nasset idtx:behavior:module = @@",
            true,
        ),
        ("token idtx:behavior:kind = \"editor\"", true),
        ("token idtx:playerRole = \"remote\"", true),
        ("token idtx:playerRole = \"server\"", true),
        (
            "token idtx:playerRole = \"server\"\nbool idtx:behavior:serverAuthoritative = true",
            false,
        ),
        ("", false),
        ("asset idtx:behavior:module = @@", false),
    ] {
        let mut fixture = Fixture::open();
        descendant_template(
            &fixture,
            options,
            "(func (export \"_idtx_on_start\") unreachable)",
        );
        // Missing bytes cannot silently turn a required descendant into optional content.
        fs::remove_file(fixture.dir.join("child.wasm")).unwrap();
        fixture.requester("A", "A");
        let edits = fixture.frame();
        fixture.session.apply_session_edits(&edits);
        fixture.session.attach_authored_behaviors(|_| None);
        fixture.session.settle_runtime_spawns();
        assert_eq!(
            statuses(&fixture.frame()),
            [if ready { "2" } else { "3" }],
            "{options}"
        );
        assert_eq!(
            fixture
                .session
                .runtime_entity_for_path("/World/A/Panel")
                .is_some(),
            ready,
            "{options}"
        );
    }
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn ready_spawn_still_observes_required_descendant_failure() {
    let mut fixture = Fixture::open();
    descendant_template(
        &fixture,
        "",
        r#"
        (global $updates (mut i32) (i32.const 0))
        (func (export "_idtx_on_update") (param f32)
            (global.set $updates (i32.add (global.get $updates) (i32.const 1)))
            (if (i32.gt_u (global.get $updates) (i32.const 1)) (then unreachable)))
    "#,
    );
    fixture.requester("A", "A");
    let edits = fixture.frame();
    fixture.session.apply_session_edits(&edits);
    fixture.session.attach_authored_behaviors(|_| None);
    let _ = fixture.frame();
    fixture.session.settle_runtime_spawns();
    assert!(
        fixture
            .session
            .hydrator
            .world()
            .resource::<RuntimePrototypes>()
            .owned["/World/A/Panel"]
            .ready
    );
    let _ = fixture.frame();
    fixture.session.settle_runtime_spawns();
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/A/Panel")
            .is_none()
    );
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn real_reference_spawns_project_then_attach_with_independent_pinned_contexts() {
    let mut fixture = Fixture::open();
    let before = fixture.session.service.root_layer_text().unwrap();
    let data = fixture.session.runtime_behavior_data("/World/A").unwrap();
    assert!(data.custom_data["idtx"].contains("offset"));
    fixture.requester("A", "A");
    fixture.requester("B", "B");
    let edits = fixture.frame();
    assert_eq!(statuses(&edits), ["0", "0"]);
    fixture.session.apply_session_edits(&edits);
    fixture.session.settle_runtime_spawns();
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/A/Panel")
            .is_some()
    );
    assert!(!fixture.session.has_live_behavior("/World/A/Panel"));
    let projected = fixture.frame();
    assert_eq!(statuses(&projected), ["1", "1"]);
    assert_eq!(fixture.session.attach_authored_behaviors(|_| None), 2);
    fixture.session.tick_behaviors(0.01);
    for parent in ["A", "B"] {
        assert!(
            fixture
                .session
                .has_live_behavior(&format!("/World/{parent}/Panel"))
        );
        assert_eq!(
            fixture
                .session
                .widget_value_for_test(&format!("/World/{parent}/Panel/Value"))
                .as_deref(),
            Some(parent)
        );
        let bound = fixture
            .session
            .service
            .prim_at_path(&format!("/World/{parent}/Panel/Bound"))
            .unwrap();
        assert!(
            fixture
                .session
                .service
                .compute_graph(bound)
                .unwrap()
                .nodes
                .iter()
                .flat_map(|node| &node.inputs)
                .any(|input| input.connections == [format!("/World/{parent}/Data.outputs:value")])
        );
    }
    let ready = fixture.frame();
    assert_eq!(statuses(&ready), ["2", "2"]);
    fixture
        .session
        .service
        .author_session_property(
            "/World/A/Panel",
            "idtx:behavior:context",
            &idtx_usd::PropertyValue::String(r#"{"label":"B"}"#.into()),
        )
        .unwrap();
    fixture.session.tick_behaviors(0.01);
    assert_eq!(
        fixture
            .session
            .widget_value_for_test("/World/A/Panel/Value")
            .as_deref(),
        Some("A"),
        "existing instance context is pinned"
    );
    assert_eq!(fixture.session.service.root_layer_text().unwrap(), before);
    fixture.session.set_play_state(PlayState::Authoring);
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/A/Panel")
            .is_none()
    );
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/B/Panel")
            .is_none()
    );
    fixture.session.tick_behaviors(0.01);
    assert_eq!(
        fixture.session.attached_behavior_count(),
        2,
        "only the requesters remain"
    );
    fixture.session.set_play_state(PlayState::Playing);
    fixture.session.apply_session_edits(&edits);
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/A/Panel")
            .is_none(),
        "old-generation requests cannot recreate panels"
    );
    fixture.session.tick_behaviors(0.01);
    fixture.session.tick_behaviors(0.01);
    assert!(
        fixture.session.has_live_behavior("/World/A/Panel"),
        "new play generation can recreate the same leaf"
    );
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn malformed_guest_contract_fails_and_removes_the_projected_reference() {
    let mut fixture = Fixture::open();
    fixture.requester("A", "X");
    let edits = fixture.frame();
    fixture.session.apply_session_edits(&edits);
    fixture.session.settle_runtime_spawns();
    assert_eq!(fixture.session.attach_authored_behaviors(|_| None), 1);
    let entity = fixture
        .session
        .runtime_entity_for_path("/World/A/Panel")
        .unwrap();
    assert_eq!(
        fixture
            .session
            .behaviors
            .instance_startup_error(entity, "panel"),
        Some(ErrorCode::TypeMismatch)
    );
    fixture.session.settle_runtime_spawns();
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/A/Panel")
            .is_none()
    );
    assert_eq!(statuses(&fixture.frame()), ["3"]);
    assert!(
        fixture
            .session
            .service
            .root_layer_text()
            .unwrap()
            .contains("def Xform \"A\"")
    );
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn payload_unload_erases_stronger_runtime_references_and_rejects_old_requests() {
    let mut fixture = Fixture::open();
    fs::write(
        fixture.dir.join("payload.usda"),
        r#"#usda 1.0
def Xform "Level" {
    def Xform "Asset" {
        def Scope "Data" {
            custom string outputs:value = "A"
        }
        def Scope "Status" {
            custom string idtx:widget:value = ""
        }
    }
}
"#,
    )
    .unwrap();
    fixture
        .session
        .service
        .replace_root_layer_text(
            r#"#usda 1.0
(defaultPrim = "World"
 upAxis = "Y"
 metersPerUnit = 1)
def Xform "World" {
    def Xform "LevelA" (prepend payload = @payload.usda@</Level>) {}
    def Xform "LevelB" (prepend payload = @payload.usda@</Level>) {}
}
"#,
        )
        .unwrap();
    fixture.session.tick().unwrap();
    let b = fixture
        .session
        .service
        .prim_at_path("/World/LevelB")
        .unwrap();
    fixture
        .session
        .service
        .set_payload_loaded(b, false)
        .unwrap();
    fixture.session.tick().unwrap();
    let before = fixture.session.service.root_layer_text().unwrap();
    fixture.requester("LevelA/Asset", "A");
    let edits = fixture.frame();
    fixture.session.apply_session_edits(&edits);
    fixture.session.settle_runtime_spawns();
    fixture.session.tick_behaviors(0.01);
    assert!(
        fixture
            .session
            .has_live_behavior("/World/LevelA/Asset/Panel")
    );
    let old_generation = fixture.session.runtime_spawn_generation();
    fixture
        .session
        .transition_level(Some("/World/LevelA".into()), "/World/LevelB".into(), None)
        .unwrap();
    assert_ne!(fixture.session.runtime_spawn_generation(), old_generation);
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/LevelA/Asset/Panel")
            .is_none()
    );
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/LevelA/Asset")
            .is_none()
    );
    fixture.session.tick_behaviors(0.01);
    fixture.session.apply_session_edits(&edits);
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/LevelA/Asset/Panel")
            .is_none()
    );
    fixture
        .session
        .transition_level(Some("/World/LevelB".into()), "/World/LevelA".into(), None)
        .unwrap();
    assert!(
        fixture
            .session
            .runtime_entity_for_path("/World/LevelA/Asset/Panel")
            .is_none(),
        "unload removed its session reference rather than masking it"
    );
    fixture.requester("LevelA/Asset", "A");
    fixture.session.tick_behaviors(0.01);
    fixture.session.tick_behaviors(0.01);
    assert!(
        fixture
            .session
            .has_live_behavior("/World/LevelA/Asset/Panel")
    );
    assert_eq!(fixture.session.service.root_layer_text().unwrap(), before);
}
