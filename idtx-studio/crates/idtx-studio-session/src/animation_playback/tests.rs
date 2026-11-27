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
fn interruption_restores_disjoint_targets_and_preserves_new_overlap_on_a_paused_timeline() {
    idtx_usd_extension::install().unwrap();
    for overlapping in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interrupt.usda");
        let extra = if overlapping { ", </World/Shared>" } else { "" };
        let mut text = format!(r#"#usda 1.0
(
    defaultPrim = "World"
    timeCodesPerSecond = 24
    startTimeCode = 0
    endTimeCode = 24
    metersPerUnit = 1
    upAxis = "Y"
)
def Xform "World" {{
    custom uniform int idtx:animation:contractVersion = 1
    custom uniform token[] idtx:animation:clipNames = ["first", "second"]
    custom uniform timecode idtx:animation:first:start = 0
    custom uniform timecode idtx:animation:first:end = 24
    custom rel idtx:animation:first:targets = [</World/Body>{extra}]
    custom uniform timecode idtx:animation:second:start = 0
    custom uniform timecode idtx:animation:second:end = 24
    custom rel idtx:animation:second:targets = [</World/Other>{extra}]
"#);
        for name in ["Body", "Other", "Shared"] {
            text.push_str(&format!(r#"
    def Cube "{name}" {{
        double3 xformOp:translate.timeSamples = {{0: (0,0,0), 24: (24,0,0)}}
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
"#));
        }
        text.push_str("}\n");
        std::fs::write(&path, text).unwrap();
        let (device, queue) = gpu();
        let mut session = StudioSession::open_with_gpu(&path, device, queue, [64, 64],
            TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap();
        session.tick_animation(0.0).unwrap();
        session.control_clip("/World", "first", ClipTransport::Seek(12.0)).unwrap();
        session.tick_animation(0.0).unwrap();
        session.prepare_presentation(1.0);
        assert_eq!(rendered_matrix(&session, "/World/Body").w_axis.x, 12.0);
        session.control_clip("/World", "second", ClipTransport::Seek(6.0)).unwrap();
        session.tick_animation(0.0).unwrap();
        session.prepare_presentation(1.0);
        for (name, expected) in [("Body", 0.0), ("Other", 6.0), ("Shared", if overlapping { 6.0 } else { 0.0 })] {
            let path = format!("/World/{name}");
            let entity = session.hydrator.entity_for_path(&path).unwrap();
            assert_eq!(session.world().get::<WorldTransform>(entity).unwrap().matrix.w_axis.x, expected, "{path}");
            assert_eq!(rendered_matrix(&session, &path).w_axis.x, expected, "{path}");
        }
        assert!(!session.clip_player_state("/World").iter().find(|h| h.clip == "first").unwrap().independent);
    }
}

#[test]
#[ignore = "requires OpenUSD and headless GPU"]
fn idle_animation_is_gated_and_late_reference_clips_refresh_the_sampling_cache() {
    idtx_usd_extension::install().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut last = None;
    for count in [100, 5_000, 20_000] {
        let path = dir.path().join(format!("static{count}.usda"));
        let mut text = String::from("#usda 1.0\n(metersPerUnit = 1\n upAxis = \"Y\")\ndef Xform \"World\" {\n");
        for i in 0..count { text.push_str(&format!("def Xform \"Static{i}\" {{}}\n")); }
        text.push_str("}\n");
        std::fs::write(&path, text).unwrap();
        let (device, queue) = gpu();
        let mut session = StudioSession::open_with_gpu(&path, device, queue, [32, 32],
            TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap();
        session.tick_animation(0.0).unwrap();
        let mut times = Vec::new();
        for _ in 0..120 {
            let start = std::time::Instant::now();
            session.tick_animation(1. / 90.).unwrap();
            times.push(start.elapsed());
        }
        times.sort_unstable();
        eprintln!("idle animation / {count} static prims: p50={:?}, p95={:?}", times[60], times[114]);
        assert_eq!(session.animation_sampling.rebuilds, 0, "empty animation should not build sampling indices");
        last = Some(session);
    }
    let mut session = last.unwrap();
    let actor = dir.path().join("actor.usda");
    std::fs::write(&actor, r#"#usda 1.0
def Xform "Actor" {
    custom uniform int idtx:animation:contractVersion = 1
    custom uniform token[] idtx:animation:clipNames = ["move"]
    custom uniform timecode idtx:animation:move:start = 0
    custom uniform timecode idtx:animation:move:end = 24
    custom rel idtx:animation:move:targets = </Actor/Body>
    def Cube "Body" {
        double3 xformOp:translate.timeSamples = {0: (0,0,0), 24: (24,0,0)}
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#).unwrap();
    session.service.spawn_session_reference("/World/Actor", actor.to_str().unwrap(), "/Actor",
        Mat4::IDENTITY.to_cols_array().map(f64::from), "{}", &[]).unwrap();
    session.tick().unwrap();
    session.control_clip("/World/Actor", "move", ClipTransport::Seek(12.0)).unwrap();
    session.tick_animation(0.0).unwrap();
    let body = session.hydrator.entity_for_path("/World/Actor/Body").unwrap();
    assert_eq!(session.world().get::<WorldTransform>(body).unwrap().matrix.w_axis.x, 12.0);
    let rebuilds = session.animation_sampling.rebuilds;
    assert!(rebuilds > 0);
    for _ in 0..120 { session.tick_animation(0.0).unwrap(); }
    assert_eq!(session.animation_sampling.rebuilds, rebuilds, "quiet active clips must reuse classifications/membership");
    session.service.mutate_session_prim("/World/Actor", "", true).unwrap();
    session.tick().unwrap();
    session.tick_animation(0.0).unwrap();
    assert!(session.clip_owned_prims().is_empty(), "removed ownership must invalidate cached descendants");
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
