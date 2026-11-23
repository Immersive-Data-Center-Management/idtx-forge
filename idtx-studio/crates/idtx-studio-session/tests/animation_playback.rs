//! End-to-end authored behavior + native sampling through the standalone session.
#![cfg(feature = "authoring")]
use idtx_studio_session::{StudioSession, RenderWorldUnitScale, TextureDecodeConfig, PlaybackRequest};
use idtx_scene::playback::ClipTransport as T;

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap();
        adapter.request_device(&wgpu::DeviceDescriptor::default()).await.unwrap()
    })
}

#[test]
#[ignore = "requires native OpenUSD and headless GPU"]
fn usd_behavior_switches_sources_completes_resumes_and_preserves_placement() {
    idtx_usd_extension::install().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let actor = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/animation-playback/actor.usda");
    let wasm = wat::parse_str(r#"(module
        (import "idtx" "idtx_playback_command" (func $command (param i64 i32 i32 i32 f32) (result i32)))
        (import "idtx" "idtx_playback_status" (func $status (param i64 i32 i32 i32 i32 i32) (result i32)))
        (import "idtx" "idtx_event_current_name" (func $name (param i32 i32) (result i32)))
        (import "idtx" "idtx_hook_result" (func $result (param i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "reaction")
        (data (i32.const 16) "idle")
        (func (export "_idtx_abi_version") (result i32) i32.const 3)
        (func (export "_idtx_on_start")
            (call $result (call $command (i64.const 0) (i32.const 0) (i32.const 8) (i32.const 0) (f32.const 0))))
        (func (export "_idtx_on_event")
            (if (i32.eq (call $name (i32.const 128) (i32.const 64)) (i32.const 19))
                (then
                    (call $result (call $status (i64.const 0) (i32.const 0) (i32.const 8) (i32.const 1024) (i32.const 8192) (i32.const 512)))
                    (call $result (call $command (i64.const 0) (i32.const 16) (i32.const 4) (i32.const 0) (f32.const 0)))))))"#).unwrap();
    std::fs::write(dir.path().join("reaction.wasm"), wasm).unwrap();
    let path = dir.path().join("scene.usda");
    std::fs::write(&path, format!(r#"#usda 1.0
(timeCodesPerSecond = 24)
def Xform "World" {{
    token visibility.timeSamples = {{ 0: "inherited", 240: "inherited" }}
    def Xform "Actor" (prepend references = @{}@ prepend apiSchemas = ["IdtxBehaviorAPI"]) {{
        double3 xformOp:translate = (11,0,7)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        token visibility.timeSamples = {{ 0: "inherited", 24: "invisible", 48: "inherited" }}
        token idtx:behavior:kind = "runtime"
        string idtx:behavior:name = "reaction"
        string idtx:behavior:version = "1.0.0"
        string idtx:behavior:uuid = "animation-reaction-test"
        asset idtx:behavior:module = @./reaction.wasm@
        uniform token[] idtx:behavior:capabilities = ["playback", "events"]
    }}
    def Xform "Other" (prepend references = @{}@) {{
        custom uniform double idtx:animation:idle:speed = 0.5
    }}
    def Cube "Rigid" {{
        double3 xformOp:translate.timeSamples = {{ 0: (0,0,0), 24: (0,1,0) }}
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}
"#, actor.display(), actor.display()).replace("@ prepend apiSchemas", "@\n prepend apiSchemas")).unwrap();
    let (device, queue) = gpu();
    let mut session = StudioSession::open_with_gpu(&path, device, queue, [64,64], TextureDecodeConfig::FULL_RESOLUTION, RenderWorldUnitScale::METRE).unwrap();
    assert!(session.clip_player_state("/World/Actor").iter().any(|h| h.clip == "reaction"));
    assert_eq!(session.attach_authored_behaviors(|_| None), 1);
    session.set_behavior_playing(true);
    session.apply_playback_request(PlaybackRequest::Play);
    session.tick_animation(0.0).unwrap();
    session.tick_behaviors(0.0);
    session.tick_animation(0.5).unwrap();
    assert_eq!(session.clip_effective_source("/World/Actor", "reaction"), "/World/Actor/Rig/Reaction");
    assert_eq!(session.clip_effective_source("/World/Other", "idle"), "/World/Other/Rig/Idle");
    assert_eq!(session.get_world_transform_f64("/World/Actor").unwrap()[12], 11.0);
    session.tick_animation(0.6).unwrap();
    session.tick_behaviors(0.0);
    session.tick_animation(0.0).unwrap();
    assert!(session.clip_player_state("/World/Actor").iter().any(|h| h.clip == "idle" && h.playing));
    assert_eq!(session.clip_effective_source("/World/Actor", "idle"), "/World/Actor/Rig/Idle");
    session.control_clip("/World/Actor", "idle", T::Pause).unwrap();
    session.tick_animation(0.0).unwrap();
    let pose = session.world().iter_entities().find(|e| e.get::<idtx_scene::PrimNode>().is_some_and(|n| n.path == "/World/Actor/Rig/Skeleton"))
        .unwrap().get::<idtx_scene::SkeletonPoseComp>().unwrap().record.clone();
    session.tick_animation(0.3).unwrap();
    let held = session.world().iter_entities().find(|e| e.get::<idtx_scene::PrimNode>().is_some_and(|n| n.path == "/World/Actor/Rig/Skeleton"))
        .unwrap().get::<idtx_scene::SkeletonPoseComp>().unwrap().record.clone();
    assert_eq!(pose.skinning_transforms, held.skinning_transforms, "global ancestor projection must not clobber a paused actor");
    let rig_visible = |session: &StudioSession| session.world().iter_entities()
        .find(|e| e.get::<idtx_scene::PrimNode>().is_some_and(|n| n.path == "/World/Actor/Rig"))
        .unwrap().get::<idtx_scene::VisibilityComp>().unwrap().visible;
    assert!(!rig_visible(&session));
    session.tick_animation(1.0).unwrap();
    assert!(rig_visible(&session), "palette-only playback must follow parent visibility on the global clock");
    assert!(session.control_clip("/World/Actor", "missing", T::Play).is_err());
    let before = session.root_layer_snapshot().unwrap();
    let mut clip = session.clip_player_state("/World/Actor").into_iter().find(|h| h.clip == "reaction").unwrap().declaration.unwrap();
    clip.speed = 2.0;
    session.author_animation_clip(&clip, false).unwrap();
    let after = session.root_layer_snapshot().unwrap();
    session.apply_root_layer_delta(&after, &before).unwrap();
    session.tick().unwrap(); session.tick_animation(0.0).unwrap();
    assert_eq!(session.clip_player_state("/World/Actor").iter().find(|h| h.clip == "reaction").unwrap().speed, 1.0);
    let run = session.clip_player_state("/World/Actor").iter().find(|h| h.clip == "idle").unwrap().run_id;
    session.author_attribute("/World/Rigid", "size", idtx_usd::PropertyValue::F64(3.0)).unwrap();
    session.tick().unwrap(); session.tick_animation(0.0).unwrap();
    let head = session.clip_player_state("/World/Actor").into_iter().find(|h| h.clip == "idle").unwrap();
    assert_eq!(head.run_id, run);
    assert!(head.independent && !head.playing, "unrelated authoring must preserve paused ownership");
    session.author_attribute("/World/Actor/Rig/Triangle", "points", idtx_usd::PropertyValue::Point3Array(
        vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 1.0, 0.0]])).unwrap();
    session.tick().unwrap(); session.tick_animation(0.0).unwrap();
    let mesh = session.world().iter_entities().find(|e| e.get::<idtx_scene::PrimNode>()
        .is_some_and(|n| n.path == "/World/Actor/Rig/Triangle")).unwrap().get::<idtx_scene::MeshComp>().unwrap();
    assert_eq!(mesh.points[1][0], 2.0, "clip ownership must not suppress authored geometry updates");
    assert_eq!(session.clip_player_state("/World/Actor").iter().find(|h| h.clip == "idle").unwrap().run_id, run);
    session.author_attribute("/World/Actor/Rig/Reaction", "joints", idtx_usd::PropertyValue::TokenArray(vec!["WrongJoint".into()])).unwrap();
    session.tick().unwrap(); session.tick_animation(0.0).unwrap();
    assert!(session.control_clip("/World/Actor", "reaction", T::Play).is_err());
    let idle = session.clip_player_state("/World/Actor").into_iter().find(|h| h.clip == "idle").unwrap().declaration.unwrap();
    session.author_animation_clip(&idle, true).unwrap();
    session.tick_animation(0.0).unwrap();
    assert!(!session.clip_player_state("/World/Actor").iter().any(|h| h.clip == "idle"));
    assert!(session.clip_player_state("/World/Other").iter().any(|h| h.clip == "idle" && h.playing));
}
