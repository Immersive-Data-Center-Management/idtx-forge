//! Differential native sampling and warm-render ownership regressions (R17/R18).
use super::*;

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
            .request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap()
            .request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    })
}

fn rendered_matrix(session: &StudioSession, path: &str) -> Mat4 {
    let snapshot = session.last_snapshot.as_ref().unwrap();
    if let Some(mesh) = snapshot.meshes.iter().find(|mesh| mesh.prim_path == path) {
        return mesh.world_matrix;
    }
    for batch in &snapshot.instanced_batches {
        if let Some(index) = batch.instance_prim_paths.iter().position(|p| p == path) {
            return batch.instance_transforms[index];
        }
    }
    panic!("missing rendered prim {path}");
}

#[test]
#[ignore = "requires OpenUSD and headless GPU"]
fn animated_ancestors_preserve_independent_clips_and_warm_render_matrices() {
    idtx_usd_extension::install().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("actors.usda");
    let mut text = String::from(r#"#usda 1.0
(
    defaultPrim = "World"
    timeCodesPerSecond = 24
    startTimeCode = 0
    endTimeCode = 24
    metersPerUnit = 1
    upAxis = "Y"
)
def Xform "World" {
    double3 xformOp:translate.timeSamples = {0: (0,0,0), 24: (24,0,0)}
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Cube "Unowned" {
        double size = 9
    }
"#);
    for (actor, size) in [("A", 1), ("B", 2)] {
        text.push_str(&format!(r#"
    def Xform "{actor}" {{
        custom uniform int idtx:animation:contractVersion = 1
        custom uniform token[] idtx:animation:clipNames = ["move"]
        custom uniform timecode idtx:animation:move:start = 0
        custom uniform timecode idtx:animation:move:end = 24
        custom rel idtx:animation:move:targets = </World/{actor}/Body>
        def Xform "Body" {{
            double3 xformOp:translate.timeSamples = {{0: (0,0,0), 24: (24,0,0)}}
            uniform token[] xformOpOrder = ["xformOp:translate"]
            def Cube "Mesh" {{
                double size = {size}
            }}
            def Sphere "Reset" {{
                double radius = {size}
                double3 xformOp:translate = (5,0,0)
                uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"]
            }}
        }}
    }}
"#));
    }
    text.push_str("}\n");
    std::fs::write(&path, text).unwrap();
    let (device, queue) = gpu();
    let mut session = StudioSession::open_with_gpu(&path, device, queue, [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap();
    let before = session.root_layer_snapshot().unwrap();
    session.tick_animation(0.0).unwrap();
    session.control_clip("/World/A", "move", ClipTransport::Seek(12.0)).unwrap();
    session.control_clip("/World/B", "move", ClipTransport::SetSpeed(0.5)).unwrap();
    session.control_clip("/World/B", "move", ClipTransport::Play).unwrap();
    session.apply_playback_request(idtx_scene::PlaybackRequest::Play);
    let mut oracle = StageService::open(&path).unwrap();
    let mut serial = None;
    let mut placement_tc = 0.0;
    for dt in [0.25, 0.25, 0.0, 0.125] {
        placement_tc += dt * 24.0;
        session.tick_animation(dt).unwrap();
        session.prepare_presentation(1.0);
        let current_serial = session.last_snapshot.as_ref().unwrap().serial;
        if let Some(serial) = serial { assert_eq!(current_serial, serial, "warm clip updates must not re-extract"); }
        serial = Some(current_serial);
        for (actor, local_tc) in [("A", 12.0), ("B", placement_tc * 0.5)] {
            let owner = format!("/World/{actor}");
            let clip = oracle.animation_clips(&owner).unwrap().remove(0);
            let fresh = oracle.sample_animation_clip_with_placement(&clip, local_tc, placement_tc, false).unwrap();
            for name in ["Mesh", "Reset"] {
                let prim_path = format!("{owner}/Body/{name}");
                let prim = oracle.prim_at_path(&prim_path).unwrap();
                let record = fresh.transforms.iter().find(|record| record.prim == prim).unwrap();
                let expected = Mat4::from_cols_array(&record.world_matrix);
                let entity = session.hydrator.entity_for_path(&prim_path).unwrap();
                let actual = session.world().get::<WorldTransform>(entity).unwrap().matrix;
                assert!(actual.abs_diff_eq(expected, 1e-5), "{prim_path}: ECS {actual:?} != native clip {expected:?}");
                assert!(rendered_matrix(&session, &prim_path).abs_diff_eq(expected, 1e-5), "rendered pose differs at {prim_path}");
                let x = if name == "Reset" { 5.0 } else { (placement_tc + local_tc) as f32 };
                assert!((actual.w_axis.x - x).abs() < 1e-5);
            }
            assert!(session.clip_player_state(&owner).iter().all(|head| head.error.is_empty()));
        }
        assert!((rendered_matrix(&session, "/World/Unowned").w_axis.x - placement_tc as f32).abs() < 1e-5);
    }
    assert_eq!(session.root_layer_snapshot().unwrap(), before);
}
