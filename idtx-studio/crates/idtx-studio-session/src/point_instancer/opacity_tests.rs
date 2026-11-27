use super::*;
use idtx_behavior_abi::PointInstanceOpacity;

fn opacity(id: i64, a: f32) -> PointInstanceOpacity {
    PointInstanceOpacity { id, opacity: a, reserved: 0 }
}

fn command(session: &StudioSession, values: Vec<PointInstanceOpacity>) -> idtx_behavior::OverlayCommand {
    idtx_behavior::OverlayCommand {
        target: session.runtime_entity_for_path(PATH).unwrap(),
        op: idtx_behavior::OverlayOp::PointInstanceOpacities { values },
    }
}

#[test]
#[ignore = "requires OpenUSD and GPU"]
fn point_instancer_opacity_wasm_checks_memory_values_ids_and_capability() {
    let mut fixture = open(false);
    let s = &mut fixture.session;
    let values = [opacity(500, 0.25), opacity(-7, 0.5), opacity(2, 0.0),
        opacity(9999, 0.5), opacity(500, f32::NAN), opacity(500, 0.5), opacity(500, 0.5)];
    let data: String = values.iter().flat_map(|v| v.id.to_le_bytes().into_iter()
        .chain(v.opacity.to_le_bytes()).chain(v.reserved.to_le_bytes()))
        .map(|b| format!("\\{b:02x}")).collect();
    let target = s.runtime_entity_for_path(PATH).unwrap();
    let wasm = wat::parse_str(format!(r#"(module
      (import "idtx" "idtx_point_instance_opacities" (func $set (param i64 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "{data}")
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func $check (param $a i32) (param $b i32)
        (if (i32.ne (local.get $a) (local.get $b)) (then unreachable)))
      (func (export "_idtx_on_start")
        (call $check (call $set (i64.const {id}) (i32.const 0) (i32.const -1)) (i32.const 6))
        (call $check (call $set (i64.const {id}) (i32.const 65530) (i32.const 3)) (i32.const 6))
        (call $check (call $set (i64.const {id}) (i32.const 48) (i32.const 1)) (i32.const 2))
        (call $check (call $set (i64.const {id}) (i32.const 64) (i32.const 1)) (i32.const 6))
        (call $check (call $set (i64.const {id}) (i32.const 80) (i32.const 2)) (i32.const 6))
        (call $check (call $set (i64.const {id}) (i32.const 0) (i32.const 3)) (i32.const 0)))
      (func (export "_idtx_on_update") (param f32)))"#, id=target.to_bits())).unwrap();
    assert!(!idtx_behavior::verify_link(&wasm, Capabilities::default()).unwrap().links);
    let caps = Capabilities::from_tokens(["overlay"]);
    assert!(idtx_behavior::verify_link(&wasm, caps).unwrap().links);
    s.behaviors.register_module(&wasm, BehaviorManifest {
        uuid: "opacity-test".into(), name: "opacity-test".into(), version: "1".into(),
        kind: idtx_behavior_abi::BehaviorKind::Runtime, ship: true,
        params: Default::default(), inputs: Vec::new(), capabilities: caps, memory_mb: None,
    }).unwrap();
    s.behaviors.attach(s.hydrator.world_mut(), target, "opacity-test").unwrap();
    s.set_behavior_playing(true);
    s.tick_behaviors(0.0);
    assert_eq!(s.behaviors.instance_readiness(target, "opacity-test", true), Some(SpawnStatus::Ready));
    assert_eq!(s.behavior_instance_opacities[PATH].len(), 3);
    s.tick_behaviors(0.0);
    assert!(s.behavior_instance_opacities.is_empty(), "one-frame commands expire");
}

#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires OpenUSD and GPU"]
fn point_instancer_opacity_frames_patch_without_usd_or_resource_churn() {
    let mut fixture = open(false);
    let s = &mut fixture.session;
    s.service.update_point_instancer_mask(PATH, &[]).unwrap();
    s.tick().unwrap();
    let first = command(s, vec![opacity(500, 0.25), opacity(-7, 0.5), opacity(2, 0.0)]);
    s.resolve_overlay_highlights_external(&[first], &[]);
    s.render_one_frame(false);
    let topology = s.topology_generation;
    let serial = s.last_snapshot.as_ref().unwrap().serial;
    let text = s.service.session_layer_text().unwrap();
    let projections = s.service.metrics().viewport.calls;
    let uploads = s.renderer.instance_upload_counts().0;
    let materials: Vec<_> = s.last_snapshot.as_ref().unwrap().instanced_batches.iter().map(|b| b.material.fingerprint).collect();
    let meshes: Vec<_> = s.last_snapshot.as_ref().unwrap().instanced_batches.iter().map(|b| Arc::clone(&b.mesh)).collect();
    for frame in 0..128 {
        let a = if frame % 16 == 0 { 0.0 } else { frame as f32 / 128.0 };
        let values = vec![opacity(2, a), opacity(500, a * 0.5), opacity(-7, a * 0.25), opacity(81, 1.0)];
        let update = command(s, values);
        s.resolve_overlay_highlights_external(&[update], &[]);
        s.render_one_frame(false);
        let snapshot = s.last_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.serial, serial);
        assert_eq!(s.topology_generation, topology);
        assert_eq!(s.renderer.instance_upload_counts().0, uploads);
        assert_eq!(s.service.metrics().viewport.calls, projections);
        assert_eq!(s.service.session_layer_text().unwrap(), text);
        for (&index, meta) in &snapshot.point_instancer_batches {
            assert!(!meta.ids.contains(&81), "authored inactiveIds wins over opacity");
            assert_eq!(meta.opacities.len(), meta.ids.len());
            assert!(meta.opacities.iter().all(|a| *a > 0.0));
            assert_eq!(snapshot.instanced_batches[index].material.fingerprint, materials[index]);
            assert!(Arc::ptr_eq(&snapshot.instanced_batches[index].mesh, &meshes[index]));
        }
    }
    s.resolve_overlay_highlights_external(&[], &[]);
    s.prepare_presentation(1.0);
    assert!(s.last_snapshot.as_ref().unwrap().point_instancer_batches.values()
        .all(|m| m.opacities.iter().all(|a| *a == 1.0)));
    s.clear_runtime_spawns_under("/World/Referenced").unwrap();
    assert!(s.behavior_instance_opacities.is_empty());
}
