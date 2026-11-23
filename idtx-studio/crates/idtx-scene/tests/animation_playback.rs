use idtx_scene::{AnimClipPlayer, PopulatorRegistry, SceneHydrator, PrimNode};
use idtx_scene::playback::{self, ClipTransport as T};
use idtx_usd::{StageService, TopologyDelta};

#[test]
fn authored_hydration_transport_completion_and_ownership() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/animation-playback/two-actors.usda");
    let mut stage = StageService::open(path).unwrap();
    let mut scene = SceneHydrator::new(PopulatorRegistry::new());
    scene.apply_topology_delta(&TopologyDelta { upserts: stage.project_topology().unwrap().to_vec(), removed: Default::default() }, &stage);
    scene.apply_viewport_projection(&stage.project_viewport().unwrap()).unwrap();
    playback::reconcile(scene.world_mut(), &stage).unwrap();
    let actor = scene.entity_for_path("/World/First").unwrap();
    let other = scene.entity_for_path("/World/Second").unwrap();
    let world = scene.world_mut();
    assert_eq!(world.get::<AnimClipPlayer>(actor).unwrap().playheads.len(), 2);
    playback::transport(world, actor, "idle", T::SetSpeed(2.0)).unwrap();
    assert!(!world.get::<AnimClipPlayer>(actor).unwrap().playhead("idle").unwrap().independent);
    playback::transport(world, actor, "idle", T::Play).unwrap();
    playback::transport(world, other, "idle", T::Play).unwrap();
    idtx_scene::tick_clip_players(world, 0.25);
    assert_eq!(world.get::<AnimClipPlayer>(actor).unwrap().playhead("idle").unwrap().current_time_code, 12.0);
    assert_eq!(world.get::<AnimClipPlayer>(other).unwrap().playhead("idle").unwrap().current_time_code, 3.0);
    playback::reconcile(world, &stage).unwrap();
    assert_eq!(world.get::<AnimClipPlayer>(actor).unwrap().playhead("idle").unwrap().speed, 2.0);
    playback::transport(world, actor, "reaction", T::Play).unwrap();
    assert!(!world.get::<AnimClipPlayer>(actor).unwrap().playhead("idle").unwrap().independent);
    let owned = idtx_scene::collect_independent_clip_target_prims(world);
    for e in world.iter_entities() {
        if let Some(node) = e.get::<PrimNode>() {
            assert_eq!(owned.contains(&node.prim), node.path.starts_with("/World/First/Rig") || node.path.starts_with("/World/Second/Rig"));
        }
    }
    idtx_scene::tick_clip_players(world, 2.0);
    idtx_scene::tick_clip_players(world, 2.0);
    let completions = world.resource::<playback::ClipEvents>().0.iter().filter(|e| e.name == "animation.completed").count();
    assert_eq!(completions, 1);
    playback::transport(world, actor, "reaction", T::Seek(70.0)).unwrap();
    playback::transport(world, actor, "reaction", T::Play).unwrap();
    let resumed = world.get::<AnimClipPlayer>(actor).unwrap().playhead("reaction").unwrap();
    assert_eq!((resumed.current_time_code, resumed.run_id), (70.0, 2));
    idtx_scene::tick_clip_players(world, 1.0);
    playback::transport(world, actor, "reaction", T::Play).unwrap();
    let head = world.get::<AnimClipPlayer>(actor).unwrap().playhead("reaction").unwrap();
    assert_eq!(head.current_time_code, 60.0);
    assert_eq!(head.run_id, 3);
    playback::transport(world, actor, "reaction", T::Pause).unwrap();
    playback::transport(world, actor, "reaction", T::Seek(70.0)).unwrap();
    idtx_scene::tick_clip_players(world, 1.0);
    assert_eq!(world.get::<AnimClipPlayer>(actor).unwrap().playhead("reaction").unwrap().current_time_code, 70.0);
    playback::transport(world, actor, "reaction", T::Release).unwrap();
    assert!(!idtx_scene::collect_independent_clip_target_prims(world).contains(&stage.prim_at_path("/World/First/Rig/Skeleton").unwrap()));
    assert!(playback::transport(world, actor, "absent", T::Play).unwrap_err().contains("unknown"));
    assert!(playback::transport(world, actor, "idle", T::SetSpeed(f64::NAN)).is_err());
}

#[test]
fn sampled_visibility_overrides_authored_default_but_preserves_user_hide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("visibility.usda");
    std::fs::write(&path, r#"#usda 1.0
def Xform "Actor" {
    custom uniform int idtx:animation:contractVersion = 1
    custom uniform token[] idtx:animation:clipNames = ["show"]
    custom rel idtx:animation:show:targets = </Actor/Cube>
    custom uniform timecode idtx:animation:show:start = 0
    custom uniform timecode idtx:animation:show:end = 24
    def Cube "Cube" {
        token visibility = "invisible"
        token visibility.timeSamples = { 0: "inherited", 24: "invisible" }
    }
}
"#).unwrap();
    let mut stage = StageService::open(&path).unwrap();
    let mut scene = SceneHydrator::new(PopulatorRegistry::new());
    scene.apply_topology_delta(&TopologyDelta { upserts: stage.project_topology().unwrap().to_vec(), removed: Default::default() }, &stage);
    scene.apply_viewport_projection(&stage.project_viewport().unwrap()).unwrap();
    scene.initialize_visibility_overrides(&stage);
    let cube = scene.entity_for_path("/Actor/Cube").unwrap();
    assert!(!scene.world().get::<idtx_scene::VisibilityComp>(cube).unwrap().visible);
    let clip = stage.animation_clips("/Actor").unwrap().remove(0);
    scene.apply_animation_projection(&stage.sample_animation_clip(&clip, 0.0, false).unwrap()).unwrap();
    assert!(scene.world().get::<idtx_scene::VisibilityComp>(cube).unwrap().visible);
    scene.set_prim_visibility_override(cube, false);
    scene.apply_animation_projection(&stage.sample_animation_clip(&clip, 0.0, false).unwrap()).unwrap();
    assert!(!scene.world().get::<idtx_scene::VisibilityComp>(cube).unwrap().visible);
}

#[test]
fn authored_channel_edit_cannot_leave_two_independent_channel_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("channels.usda");
    std::fs::write(&path, r#"#usda 1.0
def Xform "Actor" {
    custom uniform int idtx:animation:contractVersion = 1
    custom uniform token[] idtx:animation:clipNames = ["a", "b"]
    custom rel idtx:animation:a:targets = </Actor/A>
    custom uniform timecode idtx:animation:a:start = 0
    custom uniform timecode idtx:animation:a:end = 24
    custom uniform token idtx:animation:a:channel = "a"
    custom rel idtx:animation:b:targets = </Actor/B>
    custom uniform timecode idtx:animation:b:start = 0
    custom uniform timecode idtx:animation:b:end = 24
    custom uniform token idtx:animation:b:channel = "b"
    def Xform "A" {}
    def Xform "B" {}
}
"#).unwrap();
    let mut stage = StageService::open(&path).unwrap();
    let mut scene = SceneHydrator::new(PopulatorRegistry::new());
    scene.apply_topology_delta(&TopologyDelta { upserts: stage.project_topology().unwrap().to_vec(), removed: Default::default() }, &stage);
    playback::reconcile(scene.world_mut(), &stage).unwrap();
    let owner = scene.entity_for_path("/Actor").unwrap();
    playback::transport(scene.world_mut(), owner, "a", T::Play).unwrap();
    playback::transport(scene.world_mut(), owner, "b", T::Play).unwrap();
    stage.apply_structural_command(&idtx_usd::StructuralCommand::SetProperty {
        prim: stage.prim_at_path("/Actor").unwrap(), property: "idtx:animation:b:channel".into(),
        value: idtx_usd::PropertyValue::Token("a".into()),
    }).unwrap();
    stage.drain_changes().unwrap();
    scene.world_mut().resource_mut::<playback::PlaybackReconcile>().dirty_paths.push("/Actor".into());
    playback::reconcile(scene.world_mut(), &stage).unwrap();
    let player = scene.world().get::<AnimClipPlayer>(owner).unwrap();
    assert!(player.playhead("a").unwrap().independent);
    assert!(!player.playhead("b").unwrap().independent);
    assert!(player.playhead("b").unwrap().error.contains("conflicts"));
    assert!(scene.world().resource::<playback::ClipRestoreTargets>().0.contains(&stage.prim_at_path("/Actor/B").unwrap()));
}
