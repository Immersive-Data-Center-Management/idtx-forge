//! Purpose extraction is tested without a GPU, including every filter union.
use std::collections::BTreeSet;

use glam::Mat4;
use idtx_render::{RenderShadowSettings, RenderWorldSnapshot};
use idtx_scene::{PopulatorRegistry, SceneHydrator};
use idtx_usd::{PurposeFilter, RenderPurpose, StageService};

fn hydrate(source: &str) -> (tempfile::TempDir, SceneHydrator) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("purpose.usda");
    std::fs::write(&path, source).unwrap();
    let mut stage = StageService::open(&path).unwrap();
    let topology = stage.project_topology_paths(&[]).unwrap();
    let mut scene = SceneHydrator::new(PopulatorRegistry::new());
    scene.apply_topology_delta(&topology, &stage);
    scene
        .apply_viewport_projection(&stage.project_viewport().unwrap())
        .unwrap();
    (dir, scene)
}

fn filter(bits: u8) -> PurposeFilter {
    PurposeFilter {
        guide: bits & 1 != 0,
        proxy: bits & 2 != 0,
        render: bits & 4 != 0,
    }
}

fn expected(bits: u8, purpose: &str) -> bool {
    bits == 0
        || purpose == "default"
        || purpose == "unset"
        || (purpose == "guide" && bits & 1 != 0)
        || (purpose == "proxy" && bits & 2 != 0)
        || (purpose == "render" && bits & 4 != 0)
}

fn extract(scene: &mut SceneHydrator, bits: u8) -> RenderWorldSnapshot {
    RenderWorldSnapshot::extract_with_purpose(
        scene.world_mut(),
        1.0,
        Mat4::IDENTITY,
        RenderShadowSettings::OFF,
        filter(bits),
    )
}

#[test]
fn purpose_all_filter_unions_cover_meshes_and_every_parametric_gprim() {
    let mut source = String::from("#usda 1.0\n");
    let types = [
        "Mesh", "Cube", "Sphere", "Cylinder", "Cone", "Capsule", "Plane",
    ];
    let purposes = ["default", "unset", "guide", "proxy", "render"];
    for purpose in purposes {
        source.push_str(&format!("def Xform \"{purpose}\" {{\n"));
        if purpose != "unset" {
            source.push_str(&format!("uniform token purpose = \"{purpose}\"\n"));
        }
        for kind in types {
            source.push_str(&format!("def {kind} \"{kind}\" {{\n"));
            if kind == "Mesh" {
                source.push_str("point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]\nint[] faceVertexCounts = [3]\nint[] faceVertexIndices = [0,1,2]\n");
            }
            source.push_str("}\n");
        }
        source.push_str("def Cube \"Hidden\" {\n token visibility = \"invisible\"\n }\n}\n");
    }
    let (_dir, mut scene) = hydrate(&source);
    for bits in 0..8 {
        let snapshot = extract(&mut scene, bits);
        let actual: BTreeSet<_> = snapshot
            .meshes
            .iter()
            .chain(&snapshot.guide_meshes)
            .map(|m| m.prim_path.clone())
            .collect();
        let wanted: BTreeSet<_> = purposes
            .iter()
            .filter(|p| expected(bits, p))
            .flat_map(|p| types.iter().map(move |kind| format!("/{p}/{kind}")))
            .collect();
        assert_eq!(actual, wanted, "purpose union {bits}");
        assert!(
            snapshot
                .meshes
                .iter()
                .all(|m| m.purpose != RenderPurpose::Guide)
        );
        assert!(
            snapshot
                .guide_meshes
                .iter()
                .all(|m| m.purpose == RenderPurpose::Guide && !m.casts_shadow)
        );
        assert_eq!(
            snapshot.guide_meshes.len(),
            if expected(bits, "guide") {
                types.len()
            } else {
                0
            }
        );
    }
}

#[test]
fn purpose_native_instances_use_computed_proxy_purpose_and_keep_default_overrides() {
    let (_dir, mut scene) = hydrate(
        r#"#usda 1.0
def Xform "Asset" {
    def Cube "Inherited" {}
    def Cube "Default" {
        uniform token purpose = "default"
    }
    def Cube "Guide" {
        uniform token purpose = "guide"
    }
}
def Xform "Proxy" (
    instanceable = true
    prepend references = </Asset>
) {
    uniform token purpose = "proxy"
}
def Xform "Render" (
    instanceable = true
    prepend references = </Asset>
) {
    uniform token purpose = "render"
}
"#,
    );
    for bits in 0..8 {
        let snapshot = extract(&mut scene, bits);
        let mut draws = Vec::new();
        for batch in snapshot
            .instanced_batches
            .iter()
            .chain(&snapshot.guide_instanced_batches)
        {
            for path in &batch.instance_prim_paths {
                draws.push((path.clone(), batch.purpose));
            }
        }
        for (path, inherited) in [
            ("/Proxy", RenderPurpose::Proxy),
            ("/Render", RenderPurpose::Render),
        ] {
            assert_eq!(
                draws
                    .iter()
                    .filter(|(p, purpose)| p == path && *purpose == RenderPurpose::Default)
                    .count(),
                1,
                "default override, union {bits}"
            );
            assert_eq!(
                draws
                    .iter()
                    .filter(|(p, purpose)| p == path && *purpose == inherited)
                    .count(),
                usize::from(filter(bits).matches(inherited)),
                "inherited purpose, union {bits}"
            );
            assert_eq!(
                draws
                    .iter()
                    .filter(|(p, purpose)| p == path && *purpose == RenderPurpose::Guide)
                    .count(),
                usize::from(expected(bits, "guide")),
                "guide purpose, union {bits}"
            );
        }
        assert!(
            snapshot
                .instanced_batches
                .iter()
                .all(|b| b.purpose != RenderPurpose::Guide)
        );
    }
}

#[test]
fn purpose_point_instancer_filter_survives_warm_patches_and_preserves_default() {
    let (_dir, mut scene) = hydrate(
        r#"#usda 1.0
def PointInstancer "Instances" {
    uniform token purpose = "proxy"
    rel prototypes = [</Instances/Inherited>, </Instances/Default>, </Instances/Guide>, </Instances/Render>]
    int[] protoIndices = [0,1,2,3]
    point3f[] positions = [(0,0,0),(2,0,0),(4,0,0),(6,0,0)]
    def Cube "Inherited" {}
    def Cube "Default" {
        uniform token purpose = "default"
    }
    def Cube "Guide" {
        uniform token purpose = "guide"
    }
    def Cube "Render" {
        uniform token purpose = "render"
    }
}
"#,
    );
    for bits in 0..8 {
        let mut snapshot = extract(&mut scene, bits);
        let count = |s: &RenderWorldSnapshot, purpose| {
            s.instanced_batches
                .iter()
                .chain(&s.guide_instanced_batches)
                .filter(|b| b.purpose == purpose)
                .map(|b| b.instance_transforms.len())
                .sum::<usize>()
        };
        for purpose in [
            RenderPurpose::Default,
            RenderPurpose::Guide,
            RenderPurpose::Proxy,
            RenderPurpose::Render,
        ] {
            assert_eq!(
                count(&snapshot, purpose),
                usize::from(filter(bits).matches(purpose)),
                "{purpose:?}, union {bits}"
            );
        }
        let warm = snapshot.patch_point_instancers(
            scene.world(),
            &[("/Instances".into(), true)],
            Mat4::IDENTITY,
        );
        if warm {
            for purpose in [
                RenderPurpose::Default,
                RenderPurpose::Guide,
                RenderPurpose::Proxy,
                RenderPurpose::Render,
            ] {
                assert_eq!(
                    count(&snapshot, purpose),
                    usize::from(filter(bits).matches(purpose))
                );
            }
        } else {
            assert!(
                !snapshot.guide_instanced_batches.is_empty(),
                "only guide batches need a full overlay refresh"
            );
        }
    }
}

#[cfg(feature = "editor-overlay")]
#[test]
fn purpose_guides_only_use_collision_overlay_and_follow_transform_patches() {
    let (_dir, mut scene) = hydrate(
        r#"#usda 1.0
def Cube "Guide" {
    uniform token purpose = "guide"
}
"#,
    );
    let mut snapshot = extract(&mut scene, 0);
    assert!(snapshot.meshes.is_empty() && snapshot.instanced_batches.is_empty());
    assert!(idtx_render::compute_scene_aabb(&snapshot, true).is_none());
    assert!(idtx_render::compute_scene_aabb(&snapshot, false).is_some());
    let before = snapshot.guide_wireframe_lines();
    assert!(!before.is_empty());
    assert!(
        before
            .iter()
            .all(|line| line.color == idtx_render::COLLISION_COLOR)
    );
    let translation = Mat4::from_translation(glam::Vec3::new(3.0, 0.0, 0.0)).to_cols_array();
    snapshot.patch_world_matrices(&[("/Guide".into(), translation)], Mat4::IDENTITY);
    let after = snapshot.guide_wireframe_lines();
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(after) {
        assert!((b.a[0] - a.a[0] - 3.0).abs() < 1e-5);
        assert!((b.b[0] - a.b[0] - 3.0).abs() < 1e-5);
    }
}

#[test]
fn purpose_picking_skips_filtered_foreground_and_hits_native_default_override() {
    let (_dir, mut scene) = hydrate(
        r#"#usda 1.0
def Xform "Asset" {
    double3 xformOp:translate = (8,0,0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Cube "Child" {
        uniform token purpose = "default"
    }
}
def Xform "Native" (
    instanceable = true
    prepend references = </Asset>
) {
    uniform token purpose = "render"
    double3 xformOp:translate = (0,0,0)
}
def Cube "Front" {
    uniform token purpose = "guide"
    double3 xformOp:translate = (0,0,4)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#,
    );
    let origin = glam::Vec3::new(0.0, 0.0, 10.0);
    let direction = -glam::Vec3::Z;
    // Session picking combines regular ECS geometry with this instance-only
    // query, including a default child under an excluded render-purpose root.
    for bits in 0..8 {
        assert_eq!(
            extract(&mut scene, bits)
                .pick_instanced_prim(origin, direction)
                .map(|(path, _)| path),
            Some("/Native".to_owned()),
        );
    }
    assert_eq!(
        extract(&mut scene, 0)
            .pick_prim_path(origin, direction)
            .as_deref(),
        Some("/Front")
    );
    assert_eq!(
        extract(&mut scene, 2)
            .pick_prim_path(origin, direction)
            .as_deref(),
        Some("/Native")
    );
    assert_eq!(
        extract(&mut scene, 4)
            .pick_prim_path(origin, direction)
            .as_deref(),
        Some("/Native")
    );
}

#[test]
fn purpose_picking_resolves_guide_and_shaded_point_instances_to_owner() {
    let (_dir, mut scene) = hydrate(
        r#"#usda 1.0
def PointInstancer "Instances" {
    rel prototypes = [</Instances/Guide>, </Instances/Proxy>]
    int[] protoIndices = [0,1]
    point3f[] positions = [(0,0,3),(0,0,0)]
    def Cube "Guide" {
        uniform token purpose = "guide"
    }
    def Cube "Proxy" {
        uniform token purpose = "proxy"
    }
}
"#,
    );
    let origin = glam::Vec3::new(0.0, 0.0, 10.0);
    for bits in [0, 1, 2, 3] {
        assert_eq!(
            extract(&mut scene, bits)
                .pick_instanced_prim(origin, -glam::Vec3::Z)
                .map(|(path, _)| path),
            Some("/Instances".to_owned()),
        );
        assert_eq!(
            extract(&mut scene, bits)
                .pick_prim_path(origin, -glam::Vec3::Z)
                .as_deref(),
            Some("/Instances")
        );
    }
    assert_eq!(
        extract(&mut scene, 4).pick_prim_path(origin, -glam::Vec3::Z),
        None
    );
}
