use super::*;
use idtx_usd::{PropertyValue, StructuralCommand};

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap();
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap()
    })
}

#[test]
#[ignore = "requires OpenUSD and headless GPU"]
fn localized_spawn_and_context_edit_keep_behavior_and_relationship_reads_scoped() {
    idtx_usd_extension::install().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wasm = wat::parse_str("(module (memory (export \"memory\") 1) (func (export \"_idtx_abi_version\") (result i32) i32.const 3))").unwrap();
    std::fs::write(dir.path().join("actor.wasm"), wasm).unwrap();
    let actor = dir.path().join("actor.usda");
    std::fs::write(&actor, r#"#usda 1.0
def Xform "Actor" {
    def Scope "Behavior" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {
        asset idtx:behavior:module = @actor.wasm@
        string idtx:behavior:uuid = "scoped"
        custom string idtx:behavior:context = "{}"
    }
}
"#).unwrap();
    let root = dir.path().join("root.usda");
    let mut text = String::from(r#"#usda 1.0
def Xform "World" {
    def Xform "Existing" (prepend references = @actor.usda@</Actor>) {}
    def Scope "Receiver" {
        rel target = </World/New/Behavior>
    }
"#);
    for i in 0..5000 { text.push_str(&format!("def Xform \"Static{i}\" {{}}\n")); }
    text.push_str("}\n");
    std::fs::write(&root, text).unwrap();
    let (device, queue) = gpu();
    let mut session = StudioSession::open_with_gpu(&root, device, queue, [32, 32],
        crate::TextureDecodeConfig::default(), crate::RenderWorldUnitScale::METRE).unwrap();
    assert_eq!(session.attach_authored_behaviors(|_| None), 1);
    // Establish parent session specs before measuring a genuinely scoped spawn.
    session.service.author_session_visibility("/World", true).unwrap();
    session.tick().unwrap();
    session.attach_authored_behaviors(|_| None);
    let reads = session.behavior_data_reads;
    #[cfg(feature = "metrics")]
    let schema_reads = session.hydrator.metrics().behavior_prim_reads;
    let rel_reads = session.world().resource::<BindingIndex>().reads;
    session.service.spawn_session_reference("/World/New", actor.to_str().unwrap(), "/Actor",
        glam::Mat4::IDENTITY.to_cols_array().map(f64::from), "{}", &[]).unwrap();
    session.tick().unwrap();
    assert_eq!(session.attach_authored_behaviors(|_| None), 1);
    assert_eq!(session.attached_behavior_count(), 2);
    assert!(session.behavior_data_reads - reads <= 2, "spawn repeated whole-stage data reads");
    #[cfg(feature = "metrics")]
    assert!(session.hydrator.metrics().behavior_prim_reads - schema_reads <= 2, "spawn repeated whole-stage behavior probes");
    assert!(session.world().resource::<BindingIndex>().reads - rel_reads <= 3, "relationship refresh scanned unrelated prims");
    assert_eq!(snapshot(&session, "/World/Receiver", "target").targets[0].status, 0);
    let reads = session.behavior_data_reads;
    #[cfg(feature = "metrics")]
    let schema_reads = session.hydrator.metrics().behavior_prim_reads;
    session.set_behavior_attribute("/World/Existing/Behavior", "idtx:behavior:context", PropertyValue::String("{\"revision\":2}".into())).unwrap();
    session.tick().unwrap();
    assert_eq!(session.attach_authored_behaviors(|_| None), 1);
    assert_eq!(session.behavior_data_reads - reads, 1);
    #[cfg(feature = "metrics")]
    assert!(session.hydrator.metrics().behavior_prim_reads - schema_reads <= 2, "single-owner edit repeated whole-stage behavior probes");
    session.service.mutate_session_prim("/World/New", "", true).unwrap();
    session.tick().unwrap();
    session.attach_authored_behaviors(|_| None);
    assert_eq!(session.attached_behavior_count(), 1);
    assert_ne!(snapshot(&session, "/World/Receiver", "target").targets[0].status, 0);
}

fn snapshot(session: &StudioSession, path: &str, name: &str) -> RelationshipBinding {
    let entity = session.hydrator.entity_for_path(path).unwrap();
    session
        .hydrator
        .world()
        .get::<Relationships>(entity)
        .unwrap()
        .0[name]
        .clone()
}

fn frame(session: &mut StudioSession) -> HashMap<String, f32> {
    session.behaviors.run_frame(
        session.hydrator.world_mut(),
        0.01,
        idtx_behavior::PlayState::Playing,
    );
    session
        .behaviors
        .drain_playback_commands()
        .into_iter()
        .map(|command| {
            let entity =
                idtx_behavior::resolve_live(session.hydrator.world(), command.target).unwrap();
            (
                session
                    .hydrator
                    .world()
                    .get::<PrimNode>(entity)
                    .unwrap()
                    .path
                    .clone(),
                command.value,
            )
        })
        .collect()
}

#[test]
#[ignore = "requires OpenUSD and headless GPU"]
fn referenced_copies_deliver_locally_and_refresh_bindings_and_context_at_boundaries() {
    idtx_usd_extension::install().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let panel = wat::parse_str(r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_relationship_targets" (func $rel (param i64 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_event_post" (func $post (param i64 i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1) (data (i32.const 0) "forwarded")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_update") (param f32)
        (if (i32.eqz (call $rel (call $self) (i32.const 0) (i32.const 9) (i32.const 100) (i32.const 1) (i32.const 200)))
          (then (if (i32.eqz (i32.load (i32.const 108))) (then
            (drop (call $post (i64.load (i32.const 100)) (i32.const 0) (i32.const 9) (i32.const 0) (i32.const 0)))))))))"#).unwrap();
    let coordinator = wat::parse_str(r#"(module
      (import "idtx" "idtx_entity_self" (func $self (result i64)))
      (import "idtx" "idtx_playback_command" (func $record (param i64 i32 i32 i32 f32) (result i32)))
      (memory (export "memory") 1) (global $n (mut f32) (f32.const 0))
      (data (i32.const 1024) "report")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_event")
        (global.set $n (f32.add (global.get $n) (f32.const 1)))
        (if (call $record (call $self) (i32.const 1024) (i32.const 6) (i32.const 3) (global.get $n))
          (then unreachable))))"#).unwrap();
    std::fs::write(dir.path().join("panel.wasm"), panel).unwrap();
    std::fs::write(dir.path().join("coordinator.wasm"), coordinator).unwrap();
    std::fs::write(
        dir.path().join("asset.usda"),
        r#"#usda 1.0
(defaultPrim = "Experience")
def Xform "Experience" (
    prepend variantSets = "binding"
    variants = { string binding = "main" }
) {
    def Scope "Panel" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {
        asset idtx:behavior:module = @panel.wasm@
        string idtx:behavior:uuid = "panel"
        uniform token[] idtx:behavior:capabilities = ["scene_query", "events"]
        rel forwarded = </Experience/Panel.coordinator>
    }
    def Scope "Coordinator" (prepend apiSchemas = ["IdtxBehaviorAPI"]) {
        asset idtx:behavior:module = @coordinator.wasm@
        string idtx:behavior:uuid = "coordinator"
        uniform token[] idtx:behavior:capabilities = ["playback"]
        custom string idtx:behavior:context = "{}"
    }
    def Scope "Other" {}
    variantSet "binding" = {
        "main" {
            over "Panel" {
                rel coordinator = </Experience/Coordinator>
            }
        }
        "other" {
            over "Panel" {
                rel coordinator = </Experience/Other>
            }
        }
    }
}
"#,
    )
    .unwrap();
    let root = dir.path().join("root.usda");
    std::fs::write(
        &root,
        r#"#usda 1.0
(defaultPrim = "Hunt")
def Xform "Hunt" {
    def Xform "A" (prepend references = @asset.usda@) {}
    def Xform "B" (prepend references = @asset.usda@) {}
    def Xform "Payload" (prepend payload = @asset.usda@) {}
    rel payloadTarget = </Hunt/Payload/Coordinator>
}
"#,
    )
    .unwrap();
    let (device, queue) = gpu();
    let mut session = StudioSession::open_with_gpu(
        root,
        device,
        queue,
        [32, 32],
        crate::TextureDecodeConfig::default(),
        crate::RenderWorldUnitScale::METRE,
    )
    .unwrap();
    assert_eq!(session.attach_authored_behaviors(|_| None), 6);
    let delivered = frame(&mut session);
    assert_eq!(delivered["/Hunt/A/Coordinator"], 1.0);
    assert_eq!(delivered["/Hunt/B/Coordinator"], 1.0);
    let a = snapshot(&session, "/Hunt/A/Panel", "forwarded");
    let b = snapshot(&session, "/Hunt/B/Panel", "forwarded");
    session.tick().unwrap();
    assert_eq!(
        snapshot(&session, "/Hunt/A/Panel", "forwarded"),
        a,
        "unchanged snapshots stay revision-stable"
    );

    // A context edit resets only that attachment, and is not visible in a
    // running callback: author, drain notices, reconcile, then dispatch.
    session
        .set_behavior_attribute(
            "/Hunt/A/Coordinator",
            "idtx:behavior:context",
            PropertyValue::String("{\"round\":2}".into()),
        )
        .unwrap();
    session.tick().unwrap();
    assert_eq!(session.attach_authored_behaviors(|_| None), 1);
    let delivered = frame(&mut session);
    assert_eq!(delivered["/Hunt/A/Coordinator"], 1.0);
    assert_eq!(delivered["/Hunt/B/Coordinator"], 2.0);

    let before = session.service.root_layer_text().unwrap();
    session
        .set_behavior_relationship("/Hunt/A/Panel", "coordinator", &["/Hunt/A/Other".into()])
        .unwrap();
    let after = session.service.root_layer_text().unwrap();
    assert_ne!(
        snapshot(&session, "/Hunt/A/Panel", "forwarded").revision,
        a.revision
    );
    assert_eq!(
        snapshot(&session, "/Hunt/B/Panel", "forwarded"),
        b,
        "unrelated binding cache survives edits"
    );
    assert!(!frame(&mut session).contains_key("/Hunt/A/Coordinator"));
    session
        .service
        .apply_root_layer_delta(&after, &before)
        .unwrap();
    session.tick().unwrap();
    assert!(frame(&mut session).contains_key("/Hunt/A/Coordinator"));

    // Variants invalidate forwarding in the copy whose composition changed.
    let prim = session.service.prim_at_path("/Hunt/B").unwrap();
    session
        .service
        .set_session_variant(prim, "binding", "other")
        .unwrap();
    session.tick().unwrap();
    assert_ne!(snapshot(&session, "/Hunt/B/Panel", "forwarded"), b);
    session
        .service
        .set_session_variant(prim, "binding", "main")
        .unwrap();
    session.tick().unwrap();

    let payload = session.service.prim_at_path("/Hunt/Payload").unwrap();
    session.service.set_payload_loaded(payload, false).unwrap();
    session.tick().unwrap();
    assert_eq!(
        snapshot(&session, "/Hunt", "payloadTarget").targets[0].status,
        2
    );
    session.service.set_payload_loaded(payload, true).unwrap();
    session.tick().unwrap();
    assert_eq!(
        snapshot(&session, "/Hunt", "payloadTarget").targets[0].status,
        0
    );

    let prim = session.service.prim_at_path("/Hunt").unwrap();
    session
        .service
        .apply_structural_command(&StructuralCommand::Rename {
            prim,
            new_name: "Moved".into(),
        })
        .unwrap();
    session.tick().unwrap();
    session.attach_authored_behaviors(|_| None);
    let delivered = frame(&mut session);
    assert!(delivered.contains_key("/Moved/A/Coordinator"));
    assert!(delivered.contains_key("/Moved/B/Coordinator"));

    // The validated authoring route rejects oversize; externally authored bad
    // assets are also diagnosed and cannot attach with substituted parameters.
    let oversized = format!("\"{}\"", "x".repeat(8191));
    assert!(
        session
            .set_behavior_attribute(
                "/Moved/A/Coordinator",
                "idtx:behavior:context",
                PropertyValue::String(oversized.clone())
            )
            .is_err()
    );
    session
        .author_attribute(
            "/Moved/A/Coordinator",
            "idtx:behavior:context",
            PropertyValue::String(oversized),
        )
        .unwrap();
    session.tick().unwrap();
    assert!(session.behavior_context("/Moved/A/Coordinator").is_err());
    session.attach_authored_behaviors(|_| None);
    let entity = session
        .hydrator
        .entity_for_path("/Moved/A/Coordinator")
        .unwrap();
    assert!(
        !session
            .behaviors
            .has_instance(entity_to_wire(entity), "coordinator")
    );

    // Reference replacement recomposes an existing forwarding chain.
    let alternate = std::fs::read_to_string(dir.path().join("asset.usda"))
        .unwrap()
        .replace("</Experience/Coordinator>", "</Experience/Other>");
    std::fs::write(dir.path().join("alternate.usda"), alternate).unwrap();
    let before = snapshot(&session, "/Moved/B/Panel", "forwarded");
    let root_text = session.service.root_layer_text().unwrap().replace("@asset.usda@", "@alternate.usda@");
    session.service.replace_root_layer_text(&root_text).unwrap();
    session.tick().unwrap();
    let after = snapshot(&session, "/Moved/B/Panel", "forwarded");
    assert_ne!(before.revision, after.revision);
    let other = session.hydrator.entity_for_path("/Moved/B/Other").unwrap();
    assert_eq!(after.targets[0].entity, entity_to_wire(other));

    // Removing a target invalidates bindings on owners outside its subtree.
    session.service.apply_structural_command(&StructuralCommand::Define {
        path: "/Moved/LocalTarget".into(), type_name: "Scope".into(),
    }).unwrap();
    session.tick().unwrap();
    session.set_behavior_relationship("/Moved/B/Panel", "coordinator", &["/Moved/LocalTarget".into()]).unwrap();
    let before = snapshot(&session, "/Moved/B/Panel", "forwarded");
    let prim = session.service.prim_at_path("/Moved/LocalTarget").unwrap();
    session.service.apply_structural_command(&StructuralCommand::Delete { prim }).unwrap();
    session.tick().unwrap();
    let after = snapshot(&session, "/Moved/B/Panel", "forwarded");
    assert_ne!(before.revision, after.revision);
    assert_eq!(after.targets[0].status, 1);
    assert!(after.targets[0].entity.is_null());
}
