use super::*;
use idtx_behavior::{BehaviorManifest, Capabilities, SpawnStatus};
use idtx_usd::point_instancer::PointInstancerPlacement;

#[path = "opacity_tests.rs"]
mod opacity_tests;

#[path = "../../../../tests/support/mod.rs"]
mod support;

const PATH: &str = "/World/Referenced/Markers";

struct Fixture {
    session: StudioSession,
    _dir: tempfile::TempDir,
}
impl std::ops::Deref for Fixture {
    type Target = StudioSession;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}
impl std::ops::DerefMut for Fixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

fn open(payload: bool) -> Fixture {
    let dir = support::scratch_dir("bounded-instancer-session");
    for file in ["bounded_asset.usda", "bounded_point_instancer.usda"] {
        let text =
            std::fs::read_to_string(support::usd_fixture(&format!("instances/{file}"))).unwrap();
        std::fs::write(
            dir.path().join(file),
            if payload {
                text.replace("prepend references", "prepend payload")
            } else {
                text
            },
        )
        .unwrap();
    }
    idtx_usd_extension::install().unwrap();
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .unwrap()
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap()
    });
    let session = StudioSession::open_with_gpu(
        dir.path().join("bounded_point_instancer.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .unwrap();
    Fixture { session, _dir: dir }
}

fn assert_fresh(session: &mut StudioSession) {
    session.prepare_presentation(1.0);
    let delta = session.service.project_topology_paths(&[]).unwrap();
    let projection = session.service.project_viewport().unwrap();
    let mut fresh = SceneHydrator::new(PopulatorRegistry::new());
    fresh.apply_topology_delta(&delta, &session.service);
    fresh.apply_viewport_projection(&projection).unwrap();
    fresh.initialize_visibility_overrides(&session.service);
    let snapshot = RenderWorldSnapshot::extract(fresh.world_mut(), 1.0, session.projection_basis);
    let warm = session.last_snapshot.as_ref().unwrap();
    assert_eq!(
        snapshot.point_instancer_batches.len(),
        warm.point_instancer_batches.len()
    );
    for (&i, expected) in &snapshot.point_instancer_batches {
        let (&j, actual) = warm
            .point_instancer_batches
            .iter()
            .find(|(_, m)| {
                m.owner_path == expected.owner_path && m.prototype_index == expected.prototype_index
            })
            .unwrap();
        assert_eq!(actual.ids, expected.ids);
        for (a, b) in warm.instanced_batches[j]
            .instance_transforms
            .iter()
            .zip(&snapshot.instanced_batches[i].instance_transforms)
        {
            assert!(a.abs_diff_eq(*b, 1e-5), "warm {a:?} != native fresh {b:?}");
        }
    }
}

#[test]
#[ignore = "requires OpenUSD and GPU"]
fn point_instancer_mixed_prototypes_and_referenced_transforms_patch_once() {
    let mut fixture = open(false);
    let session = &mut fixture.session;
    // Prime the existing transform writer's session overs. Its first-write
    // metadata invalidation is independent of F5; subsequent poses are warm.
    for path in [
        "/World/Unrelated",
        "/World",
        "/World/Referenced/Prototypes/Triangle",
    ] {
        let prim = session.service.prim_at_path(path).unwrap();
        let pose = session.service.get_local_transform_f64(prim).unwrap();
        session
            .service
            .author_session_transform(path, pose)
            .unwrap();
    }
    // The initial runtime override also reconciles weaker time-sampled tracks.
    session
        .service
        .update_point_instancer_mask(PATH, &[-7])
        .unwrap();
    session.tick().unwrap();
    session.prepare_presentation(1.0);
    let snapshot = session.last_snapshot.as_ref().unwrap();
    assert_eq!(
        snapshot.point_instancer_batches.len(),
        2,
        "polygon and parametric prototypes both render"
    );
    for (&i, meta) in &snapshot.point_instancer_batches {
        let batch = &snapshot.instanced_batches[i];
        assert!(batch.instance_prim_paths.is_empty());
        assert_eq!(batch.material.emissive, [0.2, 1.0, 0.4]);
        for (&id, matrix) in meta.ids.iter().zip(&batch.instance_transforms) {
            let expected = match id {
                500 => Vec3::new(16.0, 0.0, 2.0),
                2 => Vec3::new(19.0, 2.5, 0.0),
                _ => panic!("masked instance rendered"),
            };
            assert!(
                matrix
                    .transform_point3(Vec3::ZERO)
                    .abs_diff_eq(expected, 1e-5)
            );
        }
    }
    let serial = snapshot.serial;
    let meshes: Vec<_> = snapshot
        .instanced_batches
        .iter()
        .map(|b| Arc::clone(&b.mesh))
        .collect();
    let topology = session.topology_generation;
    session
        .service
        .update_point_instancer_mask(PATH, &[])
        .unwrap();
    // Mix a pulse with an unrelated transform in the same notice drain.
    session
        .service
        .author_session_transform(
            "/World/Unrelated",
            Mat4::from_translation(Vec3::new(3.0, 4.0, 5.0))
                .to_cols_array()
                .map(f64::from),
        )
        .unwrap();
    session.tick().unwrap();
    assert!(
        !session.snapshot_dirty,
        "mixed notice batch must retain the warm snapshot"
    );
    assert!(!session.prepare_presentation(1.0).extracted);
    assert_eq!(session.last_snapshot.as_ref().unwrap().serial, serial);
    assert_eq!(session.topology_generation, topology);
    for (old, new) in meshes
        .iter()
        .zip(&session.last_snapshot.as_ref().unwrap().instanced_batches)
    {
        assert!(Arc::ptr_eq(old, &new.mesh));
    }
    assert_fresh(session);
    // Parent and prototype edits exercise the existing transform-only sampler.
    for (path, pose) in [
        ("/World", Mat4::from_translation(Vec3::new(20.0, 3.0, 0.0))),
        (
            "/World/Referenced/Prototypes/Triangle",
            Mat4::from_scale_rotation_translation(
                Vec3::new(2.0, 1.0, 1.0),
                glam::Quat::from_rotation_y(0.5),
                Vec3::new(0.0, 4.0, 0.0),
            ),
        ),
    ] {
        session
            .service
            .author_session_transform(path, pose.to_cols_array().map(f64::from))
            .unwrap();
        session.tick().unwrap();
        assert!(!session.prepare_presentation(1.0).extracted);
        assert_fresh(session);
    }
    let inspection = session.service.inspect_point_instancer(PATH, None).unwrap();
    let a = inspection.placement;
    let order = [3, 0, 2, 1];
    let replacement = PointInstancerPlacement {
        positions: order.iter().map(|&i| a.positions[i]).collect(),
        prototype_indices: order.iter().map(|&i| a.prototype_indices[i]).collect(),
        ids: order.iter().map(|&i| a.ids[i]).collect(),
        invisible_ids: vec![2],
        ..Default::default()
    };
    session
        .service
        .update_point_instancer(PATH, &replacement)
        .unwrap();
    session.tick().unwrap();
    assert!(!session.prepare_presentation(1.0).extracted);
    assert_fresh(session);
}

/// Post a real wasm ticket but leave its deferred command unapplied for races.
fn pending(
    session: &mut StudioSession,
    expected_error: ErrorCode,
) -> (SessionEditCommand, idtx_behavior_abi::Entity) {
    let target = session.runtime_entity_for_path(PATH).unwrap();
    let caller = session.runtime_entity_for_path("/World/Unrelated").unwrap();
    let wasm = wat::parse_str(format!(r#"(module
      (import "idtx" "idtx_session_update_point_instancer" (func $edit (param i64 i32 i32 i32 i32 i32 i32) (result i32)))
      (import "idtx" "idtx_session_edit_status" (func $status (param i64 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "_idtx_abi_version") (result i32) i32.const 3)
      (func (export "_idtx_on_start")
        (if (call $edit (i64.const {}) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)) (then unreachable)))
      (func (export "_idtx_on_update") (param f32)
        (if (call $status (i64.load (i32.const 0)) (i32.const 16)) (then unreachable))
        (if (i32.ne (i32.load (i32.const 16)) (i32.const 0)) (then
          (if (i32.ne (i32.load (i32.const 20)) (i32.const {})) (then unreachable))))))"#,
        target.to_bits(), expected_error.as_u32())).unwrap();
    session
        .behaviors
        .register_module(
            &wasm,
            BehaviorManifest {
                uuid: "instancer-test".into(),
                name: "instancer-test".into(),
                version: "1".into(),
                kind: idtx_behavior_abi::BehaviorKind::Runtime,
                ship: true,
                params: Default::default(),
                inputs: Vec::new(),
                capabilities: Capabilities::from_tokens(["stage_edit"]),
                memory_mb: None,
            },
        )
        .unwrap();
    session
        .behaviors
        .attach(session.hydrator.world_mut(), caller, "instancer-test")
        .unwrap();
    session
        .behaviors
        .run_frame(session.hydrator.world_mut(), 0.01, PlayState::Playing);
    let mut commands = session.behaviors.drain_session_edits();
    assert_eq!(commands.len(), 1);
    (commands.remove(0), caller)
}

#[test]
#[ignore = "requires OpenUSD and GPU"]
fn point_instancer_deferred_results_cover_commit_failure_and_unload_races() {
    for case in 0..3 {
        let mut fixture = open(true);
        let session = &mut fixture.session;
        let error = match case {
            0 => ErrorCode::Ok,
            1 => ErrorCode::InvalidArgument,
            _ => ErrorCode::StaleHandle,
        };
        let (mut command, caller) = pending(session, error);
        let SessionEditOp::UpdatePointInstancer {
            ticket, instances, ..
        } = &mut command.op
        else {
            panic!();
        };
        let ticket = *ticket;
        if case == 1 {
            *instances = Some(vec![idtx_behavior_abi::PointInstance {
                id: 9,
                prototype_index: 22,
                ..Default::default()
            }]);
        }
        let before = session.service.session_layer_text().unwrap();
        if case == 2 {
            session
                .clear_runtime_spawns_under("/World/Referenced")
                .unwrap();
            let prim = session.service.prim_at_path("/World/Referenced").unwrap();
            session.service.set_payload_loaded(prim, false).unwrap();
            session.tick().unwrap();
        }
        session.apply_session_edits(&[command]);
        assert!(!session.behaviors.has_pending_stage_edit(ticket));
        if case != 0 {
            assert_eq!(session.service.session_layer_text().unwrap(), before);
        }
        session
            .behaviors
            .run_frame(session.hydrator.world_mut(), 0.01, PlayState::Playing);
        assert_eq!(
            session
                .behaviors
                .instance_readiness(caller, "instancer-test", true),
            Some(SpawnStatus::Ready)
        );
    }
}

#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires OpenUSD and GPU"]
fn point_instancer_visibility_windows_keep_topology_and_gpu_prototypes_resident() {
    let mut fixture = open(false);
    let session = &mut fixture.session;
    session
        .service
        .author_session_transform(
            "/World/NativeA",
            Mat4::IDENTITY.to_cols_array().map(f64::from),
        )
        .unwrap();
    session.tick().unwrap();
    let n = 1024;
    let placement = markers(n);
    session
        .service
        .update_point_instancer(PATH, &placement)
        .unwrap();
    session.tick().unwrap();
    let owner = session.hydrator.entity_for_path(PATH).unwrap();
    assert!(
        session
            .hydrator
            .world()
            .get::<idtx_scene::AnimatedAttribute>(owner)
            .is_none(),
        "shadowed time-sampled masks must not keep forcing full extraction during play"
    );
    session.render_one_frame(false);
    let topology = session.topology_generation;
    let entities = session.hydrator.world().entities().len();
    let serial = session.last_snapshot.as_ref().unwrap().serial;
    let prototypes = session.renderer.instance_upload_counts().0;
    assert!(
        session
            .last_snapshot
            .as_ref()
            .unwrap()
            .instanced_batches
            .len()
            > session
                .last_snapshot
                .as_ref()
                .unwrap()
                .point_instancer_batches
                .len()
    );
    let viewport_calls = session.service.metrics().viewport.calls;
    let meshes: Vec<_> = session
        .last_snapshot
        .as_ref()
        .unwrap()
        .instanced_batches
        .iter()
        .map(|b| Arc::clone(&b.mesh))
        .collect();
    for pulse in 0..64 {
        let writes_before = session.renderer.instance_upload_counts().1;
        let hidden: Vec<_> = placement
            .ids
            .iter()
            .enumerate()
            .filter(|(i, _)| pulse % 8 == 0 || i % 64 != pulse)
            .map(|(_, id)| *id)
            .collect();
        session
            .service
            .update_point_instancer_mask(PATH, &hidden)
            .unwrap();
        session.tick().unwrap();
        session.render_one_frame(false);
        let snapshot = session.last_snapshot.as_ref().unwrap();
        let point_writes = snapshot
            .point_instancer_batches
            .keys()
            .filter(|&&i| !snapshot.instanced_batches[i].instance_transforms.is_empty())
            .count() as u64;
        assert_eq!(
            session.renderer.instance_upload_counts().1 - writes_before,
            point_writes,
            "a pure mask pulse must not upload unrelated native instance buffers"
        );
        assert_eq!(session.topology_generation, topology);
        assert_eq!(session.hydrator.world().entities().len(), entities);
        assert_eq!(session.last_snapshot.as_ref().unwrap().serial, serial);
        assert_eq!(session.renderer.instance_upload_counts().0, prototypes);
        assert_eq!(session.service.metrics().viewport.calls, viewport_calls);
        assert_eq!(
            session.renderer.last_frame_timings().merged_batch_rebakes,
            0
        );
        for (old, batch) in meshes
            .iter()
            .zip(&session.last_snapshot.as_ref().unwrap().instanced_batches)
        {
            assert!(Arc::ptr_eq(old, &batch.mesh));
        }
    }
    let writes_before = session.renderer.instance_upload_counts().1;
    session
        .service
        .update_point_instancer_mask(PATH, &[])
        .unwrap();
    session
        .service
        .author_session_transform(
            "/World/NativeA",
            Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0))
                .to_cols_array()
                .map(f64::from),
        )
        .unwrap();
    session.tick().unwrap();
    session.render_one_frame(false);
    let snapshot = session.last_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.serial, serial);
    assert_eq!(session.renderer.instance_upload_counts().0, prototypes);
    assert_eq!(
        session.renderer.instance_upload_counts().1 - writes_before,
        3,
        "two point batches plus the independently moved native batch"
    );
    let (batch, i) = snapshot
        .instanced_batches
        .iter()
        .find_map(|b| {
            b.instance_prim_paths
                .iter()
                .position(|p| p == "/World/NativeA")
                .map(|i| (b, i))
        })
        .unwrap();
    assert!((batch.instance_transforms[i].w_axis.x - 13.0).abs() < 1e-5);
}

fn markers(n: usize) -> PointInstancerPlacement {
    PointInstancerPlacement {
        positions: (0..n)
            .map(|i| [(i % 64) as f32 * 0.1, 0.0, (i / 64) as f32 * 0.1])
            .collect(),
        prototype_indices: (0..n).map(|i| (i % 2) as i32).collect(),
        ids: (0..n).map(|i| 10_000 + i as i64 * 7).collect(),
        ..Default::default()
    }
}

#[test]
#[ignore = "requires OpenUSD and GPU"]
fn point_instancer_conductor_imports_f2_and_keeps_play_values_read_only() {
    let mut fixture = open(false);
    let session = &mut fixture.session;
    let mut source = session.service.root_layer_text().unwrap();
    source.push_str(
        r#"
def BasisCurves "Route" {
    uniform token type = "linear"
    uniform token wrap = "nonperiodic"
    point3f[] points = [(100, 0, 0), (102, 0, 0)]
    int[] curveVertexCounts = [2]
    custom bool idtx:curve:eligible = true
}
"#,
    );
    session.service.replace_root_layer_text(&source).unwrap();
    session.tick().unwrap();
    let settings = idtx_usd::curve::placement::PlacementSettings {
        spacing: 1.0,
        window: 2.0,
        lateral: [0.0; 2],
        orient: false,
        ..Default::default()
    };
    session.bake_curve("/Route", &settings).unwrap();
    session.tick().unwrap();
    let owner = session.service.curve_bake_owner_for("/Route", 0).unwrap();
    let output = format!("{owner}/Placements");
    let before = session.service.root_layer_text().unwrap();
    session
        .import_point_instancer_bake(PATH, &output, 1)
        .unwrap();
    let after = session.service.root_layer_text().unwrap();
    assert_ne!(before, after, "import authors ordinary saved defaults");
    let placement = session
        .service
        .inspect_point_instancer(PATH, None)
        .unwrap()
        .placement;
    assert_eq!(placement.ids, [0, 1, 2]);
    assert_eq!(placement.prototype_indices, [1, 1, 1]);
    assert_eq!(
        placement.positions,
        [[85.0, 0.0, 0.0], [86.0, 0.0, 0.0], [87.0, 0.0, 0.0]],
        "F2 world poses must become instancer-local inputs"
    );
    session.prepare_presentation(1.0);
    let state = session.point_instancer_tool_state(PATH).unwrap();
    assert!(state.invalid_entries.is_empty());
    assert_eq!(state.visible_triangles, 36);
    let bounds = session
        .hydrator
        .world()
        .get::<idtx_scene::BoundsComp>(session.hydrator.entity_for_path(PATH).unwrap())
        .unwrap();
    assert!(
        bounds.minimum[0] > 99.0 && bounds.maximum[0] > 102.0,
        "native prim framing bounds follow the imported placements"
    );
    assert!(state.baked_placements.contains(&output));
    assert!(Arc::ptr_eq(
        &state,
        &session.point_instancer_tool_state(PATH).unwrap()
    ));
    // The same root-delta seam used by the shell journal can undo/redo the arrays.
    session
        .service
        .apply_root_layer_delta(&after, &before)
        .unwrap();
    session.tick().unwrap();
    assert_eq!(
        session
            .service
            .inspect_point_instancer(PATH, None)
            .unwrap()
            .placement
            .ids,
        [500, -7, 81, 2]
    );
    session
        .service
        .apply_root_layer_delta(&before, &after)
        .unwrap();
    session.tick().unwrap();
    session.set_play_state(PlayState::Playing);
    assert!(session.point_instancer_tool_state(PATH).unwrap().read_only);
    assert!(
        session
            .edit_point_instancer_defaults(PATH, &placement)
            .is_err()
    );
    assert_eq!(session.service.root_layer_text().unwrap(), after);
}

/// Reproducible CPU update+hydrate+snapshot-preparation comparison. No hard wall
/// clock thresholds in CI; cache/topology invariants are asserted above.
#[test]
#[ignore = "performance experiment: OpenUSD and GPU; run with --nocapture"]
fn point_instancer_marker_benchmark() {
    let mut fixture = open(false);
    let session = &mut fixture.session;
    const PULSES: usize = 120;
    for n in [256, 1024, 4096] {
        let placement = markers(n);
        session
            .service
            .update_point_instancer(PATH, &placement)
            .unwrap();
        session.tick().unwrap();
        session.render_one_frame(false);
        let mut measurements = Vec::new();
        for pulse in 0..PULSES {
            let hidden: Vec<_> = placement
                .ids
                .iter()
                .enumerate()
                .filter(|(i, _)| (*i + n - pulse) % n >= 32)
                .map(|(_, id)| *id)
                .collect();
            let start = std::time::Instant::now();
            session
                .service
                .update_point_instancer_mask(PATH, &hidden)
                .unwrap();
            session.tick().unwrap();
            assert!(!session.prepare_presentation(1.0).extracted);
            measurements.push(start.elapsed().as_secs_f64() * 1e6);
        }
        measurements.sort_by(f64::total_cmp);
        eprintln!(
            "F5 native mask, capacity={n}, visible=32, CPU us p50={:.1} p95={:.1}",
            measurements[PULSES / 2],
            measurements[PULSES * 95 / 100]
        );
    }
    // Small reusable mesh pool. Distinct tiny cube-size differences prevent
    // automatic instance promotion, preserving the standalone-mesh baseline.
    let mut source = String::from(
        "#usda 1.0\n(defaultPrim = \"World\"\n metersPerUnit = 1\n upAxis = \"Y\")\ndef Xform \"World\" {\n",
    );
    for i in 0..32 {
        source.push_str(&format!(
            "def Cube \"M{i}\" {{ double size = {}\n }}\n",
            0.2 + i as f64 * 0.001
        ));
    }
    source.push_str("}\n");
    session.service.replace_root_layer_text(&source).unwrap();
    session.tick().unwrap();
    session.prepare_presentation(1.0);
    assert!(
        session
            .last_snapshot
            .as_ref()
            .unwrap()
            .instanced_batches
            .is_empty(),
        "pool must remain the standalone-mesh baseline"
    );
    let mut measurements = Vec::new();
    for pulse in 0..PULSES {
        let entries: Vec<_> = (0..32)
            .map(|i| {
                (
                    format!("/World/M{i}"),
                    Mat4::from_translation(Vec3::new((pulse + i) as f32 * 0.1, 0.0, 0.0))
                        .to_cols_array()
                        .map(f64::from),
                )
            })
            .collect();
        let start = std::time::Instant::now();
        session.service.author_session_transforms(&entries).unwrap();
        session.tick().unwrap();
        session.prepare_presentation(1.0);
        measurements.push(start.elapsed().as_secs_f64() * 1e6);
    }
    measurements.sort_by(f64::total_cmp);
    eprintln!(
        "Small native mesh pool, capacity=32, visible=32, CPU us p50={:.1} p95={:.1}",
        measurements[PULSES / 2],
        measurements[PULSES * 95 / 100]
    );
}
