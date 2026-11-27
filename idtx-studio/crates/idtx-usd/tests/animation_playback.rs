//! Native composition/source-switch regression tests (no animation schema plugin).
use idtx_usd::StageService;
use std::path::PathBuf;

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/animation-playback/two-actors.usda")
}

#[test]
fn rigid_clip_fast_samples_match_full_projection_with_placement_and_visibility() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rigid.usda");
    std::fs::write(&path, r#"#usda 1.0
(timeCodesPerSecond = 24)
def Xform "Placement" {
    double3 xformOp:translate.timeSamples = {0: (0,0,0), 24: (24,0,0)}
    uniform token[] xformOpOrder = ["xformOp:translate"]
    token visibility.timeSamples = {0: "inherited", 12: "invisible", 18: "inherited"}
    def Xform "Actor" {
        custom uniform int idtx:animation:contractVersion = 1
        custom uniform token[] idtx:animation:clipNames = ["move"]
        custom uniform timecode idtx:animation:move:start = 0
        custom uniform timecode idtx:animation:move:end = 24
        custom rel idtx:animation:move:targets = </Placement/Actor/Body>
        def Cube "Body" {
            double3 xformOp:translate.timeSamples = {0: (0,0,0), 24: (24,0,0)}
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
}
"#).unwrap();
    let mut stage = StageService::open(&path).unwrap();
    let before = stage.root_layer_text().unwrap();
    let clip = stage.animation_clips("/Placement/Actor").unwrap().remove(0);
    for (tc, placement_tc, visible) in [(0.0, 0.0, true), (6.0, 12.0, false), (12.0, 18.0, true)] {
        let full = stage.sample_animation_clip_with_placement(&clip, tc, placement_tc, false).unwrap();
        let fast = stage.sample_animation_clip_transforms(&clip, tc, placement_tc).unwrap();
        assert_eq!(fast.schema_version, idtx_usd::PROJECTION_SCHEMA_VERSION);
        assert_eq!(fast.transforms.len(), full.transforms.len());
        for transform in &full.transforms {
            let actual = fast.transforms.iter().find(|t| t.prim == transform.prim).unwrap();
            assert_eq!(actual.world_matrix, transform.world_matrix);
            assert_eq!(actual.world_matrix[12], (tc + placement_tc) as f32);
        }
        assert_eq!(fast.visibility.len(), full.visibility.len());
        for record in &full.visibility {
            let actual = fast.visibility.iter().find(|r| r.prim == record.prim).unwrap();
            assert_eq!(actual.visible, record.visible);
            assert_eq!(actual.visible, visible);
        }
        assert!(fast.meshes.is_empty() && fast.cubes.is_empty(), "fast sampling must retain its narrow payload");
    }
    assert_eq!(stage.root_layer_text().unwrap(), before);
    assert!(stage.drain_changes().unwrap().is_none());
}

#[test]
fn sources_are_selected_natively_and_isolated_from_other_actors_and_persistence() {
    let mut stage = StageService::open(sample()).unwrap();
    let clips = stage.animation_clips("/World/First").unwrap();
    assert_eq!(clips.len(), 2);
    assert!(clips.iter().all(|c| c.error.is_empty()));
    let idle = &clips[0];
    let reaction = &clips[1];
    assert_eq!(idle.targets, ["/World/First/Rig"]);
    let first = stage.sample_animation_clip(idle, 12.0, false).unwrap();
    let next = stage.sample_animation_clip(reaction, 72.0, false).unwrap();
    assert_eq!(first.skeleton_poses.len(), 1);
    assert_eq!(next.skeleton_poses.len(), 1);
    assert_ne!(first.skeleton_poses[0].skinning_transforms, next.skeleton_poses[0].skinning_transforms);
    assert_eq!(stage.path_for(next.skeleton_poses[0].animation_source.unwrap()), Some("/World/First/Rig/Reaction"));
    assert!(next.transforms.iter().all(|t| stage.path_for(t.prim).unwrap().starts_with("/World/First/Rig")));
    let other = stage.animation_clips("/World/Second").unwrap().remove(0);
    let second = stage.sample_animation_clip(&other, 6.0, false).unwrap();
    assert_eq!(stage.path_for(second.skeleton_poses[0].animation_source.unwrap()), Some("/World/Second/Rig/Idle"));
    let returned = stage.sample_animation_clip(idle, 12.0, true).unwrap();
    assert_eq!(returned.skeleton_poses[0].skinning_transforms, first.skeleton_poses[0].skinning_transforms);
    let global = stage.sample_viewport(12.0).unwrap();
    assert!(global.skeleton_poses.iter().all(|p| stage.path_for(p.animation_source.unwrap()).unwrap().ends_with("/Idle")));
    let changes = stage.drain_changes().unwrap();
    assert!(changes.is_none(), "private bindings must not produce main-stage edits: {changes:?}");
    stage.release_animation_clip(&reaction.key()).unwrap();
}

#[test]
fn endpoints_receive_native_reference_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("offset.usda");
    let actor = sample().parent().unwrap().join("actor.usda");
    std::fs::write(&path, format!(r#"#usda 1.0
(timeCodesPerSecond = 24)
def Xform "Copy" (prepend references = @{}@ (offset = 100; scale = 2)) {{}}
"#, actor.display())).unwrap();
    let stage = StageService::open(path).unwrap();
    let clips = stage.animation_clips("/Copy").unwrap();
    assert_eq!((clips[0].start, clips[0].end), (100.0, 148.0));
    assert_eq!((clips[1].start, clips[1].end), (220.0, 268.0));
    assert_eq!(clips[0].source, "/Copy/Rig/Idle");
    stage.validate_animation_clips().unwrap();
}

#[test]
fn malformed_policy_is_diagnosable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("invalid.usda");
    let actor = sample().parent().unwrap().join("actor.usda");
    std::fs::write(&path, format!(r#"#usda 1.0
def Xform "Copy" (prepend references = @{}@) {{
    custom uniform timecode idtx:animation:reaction:end = 10
}}
"#, actor.display())).unwrap();
    let stage = StageService::open(path).unwrap();
    assert!(stage.animation_clips("/Copy").unwrap()[1].error.contains("range"));
    assert!(stage.validate_animation_clips().is_err());
}

#[test]
fn runtime_packaging_keeps_inactive_sources_and_excludes_live_selection() {
    let dir = tempfile::tempdir().unwrap();
    // Localization temporarily edits its input layer. Give this test its own
    // complete source closure so parallel readers cannot observe those edits.
    let input = dir.path().join("input.usda");
    std::fs::copy(sample(), &input).unwrap();
    std::fs::copy(sample().parent().unwrap().join("actor.usda"), dir.path().join("actor.usda")).unwrap();
    let mut stage = StageService::open(&input).unwrap();
    let reaction = stage.animation_clips("/World/First").unwrap().remove(1);
    stage.sample_animation_clip(&reaction, 72.0, false).unwrap();
    let output = dir.path().join("stage.usdz");
    let report = idtx_usd::package_stage(&input, &output, idtx_usd::PackagingPurpose::Runtime).unwrap();
    assert!(report.unresolved.is_empty(), "{report:?}");
    let mut packaged = StageService::open(&output).unwrap();
    packaged.validate_animation_clips().unwrap();
    let clips = packaged.animation_clips("/World/First").unwrap();
    assert_eq!((clips[1].start, clips[1].end), (60.0, 84.0));
    let default = packaged.sample_viewport(12.0).unwrap();
    assert!(default.skeleton_poses.iter().all(|p| packaged.path_for(p.animation_source.unwrap()).unwrap().ends_with("/Idle")));
    let pose = packaged.sample_animation_clip(&clips[1], 72.0, false).unwrap();
    assert!(packaged.path_for(pose.skeleton_poses[0].animation_source.unwrap()).unwrap().ends_with("/Reaction"));
}

#[test]
fn native_joint_mismatch_is_rejected_and_placement_uses_its_own_clock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("placement.usda");
    let actor = sample().parent().unwrap().join("actor.usda");
    std::fs::write(&path, format!(r#"#usda 1.0
def Xform "Copy" (prepend references = @{}@) {{
    double3 xformOp:translate.timeSamples = {{ 0: (0,0,0), 24: (24,0,0) }}
    uniform token[] xformOpOrder = ["xformOp:translate"]
    over "Rig" {{
        over "Reaction" {{
            uniform token[] joints = ["Other"]
        }}
    }}
}}
"#, actor.display())).unwrap();
    let mut stage = StageService::open(path).unwrap();
    let clips = stage.animation_clips("/Copy").unwrap();
    assert!(stage.prepare_animation_clip(&clips[1]).unwrap_err().to_string().contains("joints"));
    let pose = stage.sample_animation_clip_with_placement(&clips[0], 6.0, 12.0, false).unwrap();
    assert_eq!(pose.skeleton_poses[0].skeleton_world_transform[12], 12.0);
    assert_eq!(pose.skeleton_poses[0].skinning_transforms[0][12], 0.25);
}

#[test]
#[cfg(feature = "authoring")]
fn source_removal_invalidates_an_existing_native_binding() {
    let mut stage = StageService::open(sample()).unwrap();
    let clip = stage.animation_clips("/World/First").unwrap().remove(1);
    stage.sample_animation_clip(&clip, 72.0, false).unwrap();
    stage.mutate_session_prim(&clip.source, "", true).unwrap();
    stage.drain_changes().unwrap();
    assert!(!stage.animation_clips("/World/First").unwrap()[1].error.is_empty());
    assert!(stage.sample_animation_clip(&clip, 72.0, false).is_err());
    let other = stage.animation_clips("/World/Second").unwrap().remove(0);
    assert!(stage.sample_animation_clip(&other, 12.0, false).is_ok());
}
