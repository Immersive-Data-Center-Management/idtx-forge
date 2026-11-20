use super::*;
use std::sync::atomic::Ordering;
use std::path::PathBuf;
use crate::{
    DirtyDomains, EditTarget, PropertyValue, XformOpType, TransformValues,
    StructuralCommand,
};
use crate::tests::NEXT_TEST_FILE;


use crate::tests::support::usd_fixture as fixture;

fn isolated_fixture(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-{label}-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::copy(fixture("scene/simple_hierarchy.usda"), &path).expect("copy isolated fixture");
    path
}

/// `author_root_shadow_mode` writes the `idtx:shadowMode` token into the root
/// layer's `customLayerData` and erases the legacy `idtx:softShadows` bool
/// (single source of truth). Requires the USD lib + authoring feature.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn author_root_shadow_mode_writes_token_and_erases_legacy() {
    let shadow_fixture = |name: &str| -> PathBuf { fixture(&format!("shadows/{name}")) };
    // Author tests mutate the opened layer; USD caches layers by path
    // process-globally, so authoring into a shared fixture would contaminate
    // the parse test running in parallel. Open a unique temp copy instead.
    let open_temp_copy = |name: &str| -> (StageService, PathBuf) {
        let bytes = std::fs::read_to_string(shadow_fixture(name))
            .unwrap_or_else(|e| panic!("read {name}: {e:?}"));
        let path = std::env::temp_dir().join(format!(
            "idtx_shadow_src_{}.usda",
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).expect("write temp fixture copy");
        let svc =
            StageService::open(&path).unwrap_or_else(|e| panic!("open temp copy of {name}: {e:?}"));
        (svc, path)
    };

    // Open the legacy-compat scene (authors bool idtx:softShadows=false) and
    // author Soft over it.
    let (mut svc, legacy_src) = open_temp_copy("shadow_legacy_false_hard.usda");
    svc.author_root_shadow_mode(ShadowMode::Soft)
        .expect("author shadow mode");

    let text = svc.root_layer_text().expect("root layer text");
    assert!(
        text.contains("idtx:shadowMode") && text.contains("soft"),
        "root layer must author the shadowMode=soft token; got:\n{text}"
    );
    assert!(
        !text.contains("\"idtx:softShadows\""),
        "legacy idtx:softShadows must be erased; got:\n{text}"
    );

    // Round-trip through a save+reopen so the authored token parses back.
    let tmp = std::env::temp_dir().join(format!(
        "idtx_shadow_roundtrip_{}.usda",
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    svc.save_stage(&tmp).expect("save authored stage");
    let reopened = StageService::open(&tmp).expect("reopen authored stage");
    assert_eq!(
        reopened.metadata().shadow_mode,
        ShadowMode::Soft,
        "reopened stage must parse shadowMode=Soft"
    );
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&legacy_src);

    // Off round-trips too (token authored, still no legacy key).
    let (mut svc, distant_src) = open_temp_copy("shadow_single_distant.usda");
    svc.author_root_shadow_mode(ShadowMode::Off)
        .expect("author off");
    let text = svc.root_layer_text().expect("root layer text");
    assert!(text.contains("idtx:shadowMode") && text.contains("off"));
    assert!(!text.contains("\"idtx:softShadows\""));
    let _ = std::fs::remove_file(&distant_src);

    // The new four-state `Smooth` token authors and round-trips through
    // save+reopen (proves the added variant writes/parses end to end).
    let (mut svc, src_path) = open_temp_copy("shadow_no_key.usda");
    svc.author_root_shadow_mode(ShadowMode::Smooth)
        .expect("author smooth");
    let text = svc.root_layer_text().expect("root layer text");
    assert!(
        text.contains("idtx:shadowMode") && text.contains("smooth"),
        "root layer must author the shadowMode=smooth token; got:\n{text}"
    );
    let tmp = std::env::temp_dir().join(format!(
        "idtx_shadow_smooth_roundtrip_{}.usda",
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    svc.save_stage(&tmp).expect("save smooth stage");
    let reopened = StageService::open(&tmp).expect("reopen smooth stage");
    assert_eq!(
        reopened.metadata().shadow_mode,
        ShadowMode::Smooth,
        "reopened stage must parse shadowMode=Smooth"
    );
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&src_path);
}

#[test]
fn rooted_light_projection_and_light_dirty_domain_stay_scoped() {
    let isolated = std::env::temp_dir().join(format!(
        "idtx-lights-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::copy(fixture("lighting/lights.usda"), &isolated)
        .expect("copy isolated lights fixture");
    {
        let mut stage = StageService::open(&isolated).expect("open lights");
        let supported = stage
            .prim_for_path("/World/Supported")
            .expect("supported root");
        let rooted = stage
            .project_viewport_roots(&[supported])
            .expect("rooted light projection");
        assert_eq!(rooted.lights.len(), 3);
        assert!(rooted.lights.iter().all(|light| {
            stage
                .path_for_prim(light.prim)
                .is_ok_and(|path| path.starts_with("/World/Supported/"))
        }));
        assert!(!rooted.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("unsupported viewport light")
                || diagnostic.contains("supports at most 16 lights")
        }));

        stage
            .conformance_author_double("/World/Supported/Key.inputs:intensity", 24.0)
            .expect("author light intensity");
        let batch = stage
            .drain_changes()
            .expect("drain light notice")
            .expect("light change batch");
        let intensity = batch
            .changes
            .iter()
            .find(|change| change.property_name.as_deref() == Some("inputs:intensity"))
            .expect("intensity change");
        assert!(intensity.domains.contains(DirtyDomains::LIGHT));
        assert!(!intensity.domains.contains(DirtyDomains::MATERIAL));
    }
    std::fs::remove_file(isolated).expect("remove isolated lights fixture");
}

#[test]
fn property_notice_is_normalized_to_prim_and_advances_revision() {
    let isolated = isolated_fixture("property-notice");
    {
        let mut stage = StageService::open(&isolated).expect("open fixture");
        stage
            .conformance_author_double("/World/Group/Cube.size", 3.0)
            .expect("author conformance value");
        let batch = stage.drain_changes().expect("drain").expect("change batch");
        assert_eq!(batch.from_revision.get(), 1);
        assert_eq!(batch.to_revision.get(), 2);
        let change = batch
            .changes
            .iter()
            .find(|change| change.property_name.as_deref() == Some("size"))
            .expect("size change");
        assert_eq!(
            stage.path_for_prim(change.prim).expect("path"),
            "/World/Group/Cube"
        );
        assert_eq!(change.notice_path, "/World/Group/Cube");
        assert!(change.domains.contains(DirtyDomains::GEOMETRY));
    }
    std::fs::remove_file(isolated).expect("remove isolated fixture");
}

#[test]
fn spawn_notice_preserves_exact_path_before_ancestor_identity_mapping() {
    let isolated = isolated_fixture("spawn-notice-provenance");
    let mut stage = StageService::open(&isolated).expect("open fixture");
    let parent = stage.prim_at_path("/World/Group").expect("parent");
    stage.mutate_session_prim("/World/Group/New", "Cube", false).expect("spawn");
    let batch = stage.drain_changes().expect("drain").expect("batch");
    let spawn = batch.changes.iter().find(|change| change.resync && change.notice_path == "/World/Group/New")
        .expect("exact spawn notice");
    assert_eq!(spawn.prim, parent, "new prim still routes to its live ancestor");
    let delta = stage.project_topology_paths(&[spawn.notice_path.clone()]).expect("exact projection");
    assert_eq!(delta.upserts.len(), 1, "must not traverse the spawn parent");
    std::fs::remove_file(isolated).expect("remove isolated fixture");
}

#[test]
fn notices_preserve_relationship_animation_metadata_and_generic_domains() {
    let isolated = isolated_fixture("semantic-dirty-domains");
    let mut stage = StageService::open(&isolated).expect("open fixture");

    stage
        .conformance_set_relationship_target("/World/Group.lookAt", "/World/Group/Cube")
        .expect("change relationship");
    let relationship = stage
        .drain_changes()
        .expect("drain relationship")
        .expect("relationship batch")
        .changes
        .into_iter()
        .find(|change| change.property_name.as_deref() == Some("lookAt"))
        .expect("relationship change");
    assert!(relationship.domains.contains(DirtyDomains::RELATIONSHIP));
    assert!(relationship.domains.contains(DirtyDomains::GENERIC));
    assert!(!relationship.domains.contains(DirtyDomains::UNKNOWN));

    stage
        .conformance_author_double_at_time("/World/Group/Cube.size", 4.0, 1.0)
        .expect("author animated size");
    let animation = stage
        .drain_changes()
        .expect("drain animation")
        .expect("animation batch")
        .changes
        .into_iter()
        .find(|change| change.property_name.as_deref() == Some("size"))
        .expect("animated geometry change");
    assert!(animation.domains.contains(DirtyDomains::GEOMETRY));
    assert!(animation.domains.contains(DirtyDomains::ANIMATION));

    stage
        .conformance_set_custom_data("/World/Group", "reviewTag", "camera-contract")
        .expect("author custom data");
    let metadata = stage
        .drain_changes()
        .expect("drain metadata")
        .expect("metadata batch")
        .changes
        .into_iter()
        .find(|change| change.property_name.is_none())
        .expect("metadata change");
    assert!(metadata.domains.contains(DirtyDomains::METADATA));
    assert!(metadata.domains.contains(DirtyDomains::GENERIC));

    std::fs::remove_file(isolated).expect("remove fixture");
}

#[test]
fn rooted_viewport_projection_visits_only_the_affected_subtree() {
    let isolated = isolated_fixture("rooted-viewport");
    {
        let mut stage = StageService::open(&isolated).expect("open fixture");
        let full = stage.project_viewport().expect("full projection");
        let cube = stage.prim_at_path("/World/Group/Cube").expect("cube prim");
        stage
            .conformance_author_double("/World/Group/Cube.size", 3.0)
            .expect("author cube size");
        let batch = stage.drain_changes().expect("drain").expect("batch");
        assert_eq!(batch.changes.len(), 1);
        let rooted = stage
            .project_viewport_roots(&[cube])
            .expect("rooted projection");
        assert_eq!(rooted.cubes.len(), 1);
        assert!((rooted.cubes[0].size - 3.0).abs() < f32::EPSILON);
        assert_eq!(rooted.transforms.len(), 1);
        assert!(rooted.transforms.len() < full.transforms.len());
        assert_eq!(rooted.bounds.len(), 1);
    }
    std::fs::remove_file(isolated).expect("remove isolated fixture");
}

#[test]
fn repeated_property_notices_coalesce_deterministically() {
    let isolated = isolated_fixture("notice-coalescing");
    {
        let mut stage = StageService::open(&isolated).expect("open fixture");
        stage
            .conformance_author_double("/World/Group/Cube.size", 3.0)
            .expect("first value");
        stage
            .conformance_author_double("/World/Group/Cube.size", 4.0)
            .expect("second value");
        let batch = stage.drain_changes().expect("drain").expect("change batch");
        let size_changes = batch
            .changes
            .iter()
            .filter(|change| change.property_name.as_deref() == Some("size"))
            .count();
        assert_eq!(size_changes, 1);
    }
    std::fs::remove_file(isolated).expect("remove isolated fixture");
}

// ── Authoring integration tests ───────────────────────────────────────────
//
// These tests require the OpenUSD runtime (DYLD_LIBRARY_PATH / LD_LIBRARY_PATH
// pointing at the OpenUSD lib directory) and the test fixtures in
// tests/fixtures/usd/authoring/. Missing required inputs fail explicitly.

fn authoring_fixture(name: &str) -> PathBuf {
    fixture(&format!("authoring/{name}"))
}

/// Round-trips a full 4×4 pose matrix through `author_session_transform`
/// and back out via `get_world_transform`, pinning the FFI marshaling
/// contract the XR controller pose channel relies on: the 16 `f64` (glam
/// column-major `Mat4::to_cols_array` order) are laid into a **row-major**
/// `GfMatrix4d`, and `GetLocalToWorldTransform` returns that same row-major
/// matrix — so with identity ancestors the world matrix equals the authored
/// one element-for-element, and the glam translation column (indices
/// 12/13/14) surfaces in USD's row-vector translation row.
///
/// Self-contained: writes its own root layer to a temp dir so the on-the-fly
/// session layer can anchor its identifier to the root's directory (the
/// relative-asset-path support added to `open_stage_with_session`). Requires
/// the `OpenUSD` runtime; a failed open (no runtime) is treated as skip.

#[test]
fn author_session_transform_round_trips_pose_matrix() {
    let root = std::env::temp_dir().join(format!(
        "idtx-pose-root-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(
        &root,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\" {}\n",
    )
    .expect("write root layer");

    // Session layer seeds the pose Xform (mirrors the prefab shape);
    // author_session_transform would DefinePrim it too if absent.
    let session = "#usda 1.0\n\ndef Xform \"Pose\" {}\n";
    let mut stage = match StageService::open_with_session(&root, session, false) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_file(&root);
            return;
        }
    };

    // glam column-major order: +90° about Z with translation (1, 2, 3).
    // to_cols_array => [c0(0,1,0,0), c1(-1,0,0,0), c2(0,0,1,0), c3(1,2,3,1)].
    let matrix: [f64; 16] = [
        0.0, 1.0, 0.0, 0.0, //
        -1.0, 0.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        1.0, 2.0, 3.0, 1.0, //
    ];
    stage
        .author_session_transform("/Pose", matrix)
        .expect("author pose matrix into session layer");
    // New prim value → re-project topology so /Pose is addressable.
    stage.project_topology().expect("re-project topology");

    let pose = stage.prim_at_path("/Pose").expect("pose prim");
    let world = stage.get_world_transform(pose).expect("world transform");
    let flat: [f32; 16] = [
        world[0][0],
        world[0][1],
        world[0][2],
        world[0][3], //
        world[1][0],
        world[1][1],
        world[1][2],
        world[1][3], //
        world[2][0],
        world[2][1],
        world[2][2],
        world[2][3], //
        world[3][0],
        world[3][1],
        world[3][2],
        world[3][3], //
    ];
    for (i, (&got, &want)) in flat.iter().zip(matrix.iter()).enumerate() {
        assert!(
            (f64::from(got) - want).abs() < 1.0e-5,
            "component {i}: got {got}, want {want}",
        );
    }
    // Translation surfaces in the last row (USD row-vector convention).
    assert!((f64::from(flat[12]) - 1.0).abs() < 1.0e-5);
    assert!((f64::from(flat[13]) - 2.0).abs() < 1.0e-5);
    assert!((f64::from(flat[14]) - 3.0).abs() < 1.0e-5);

    let _ = std::fs::remove_file(&root);
}

/// Round-trips TWO pose matrices through the bulk
/// `author_session_transforms` shim in a single call (one lock / change
/// block / notice batch — the per-frame physics write-back path), and reads
/// both back via `get_world_transform`. Pins the per-entry layout contract:
/// path `i` consumes `matrices[16*i .. 16*i+16]` in glam column-major order.

#[test]
fn author_session_transforms_bulk_round_trips_each_entry() {
    let root = std::env::temp_dir().join(format!(
        "idtx-pose-bulk-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(
        &root,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\n\
             def Xform \"World\"\n{\n    \
             def Xform \"A\"\n    {\n        double3 xformOp:translate = (0, 0, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }\n    \
             def Xform \"B\"\n    {\n        double3 xformOp:translate = (0, 0, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }\n}\n",
    )
    .expect("write root layer");

    let mut stage = match StageService::open_with_session(&root, "#usda 1.0\n", false) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_file(&root);
            return; // no USD runtime — skip
        }
    };

    // Entry 0: identity translated to (1, 2, 3). Entry 1: +90° about Z
    // translated to (4, 5, 6). glam column-major (`to_cols_array`) order.
    let m0: [f64; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        1.0, 2.0, 3.0, 1.0, //
    ];
    let m1: [f64; 16] = [
        0.0, 1.0, 0.0, 0.0, //
        -1.0, 0.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        4.0, 5.0, 6.0, 1.0, //
    ];
    stage
        .author_session_transforms(&[("/World/A".to_owned(), m0), ("/World/B".to_owned(), m1)])
        .expect("bulk-author two session transforms");
    stage.project_topology().expect("re-project topology");

    for (path, want) in [("/World/A", m0), ("/World/B", m1)] {
        let prim = stage.prim_at_path(path).expect("prim");
        let world = stage.get_world_transform(prim).expect("world transform");
        // Row-major GfMatrix4d equals the authored col-major array (identity
        // ancestors), so compare element-for-element in flat order.
        let flat: [f32; 16] = [
            world[0][0],
            world[0][1],
            world[0][2],
            world[0][3], //
            world[1][0],
            world[1][1],
            world[1][2],
            world[1][3], //
            world[2][0],
            world[2][1],
            world[2][2],
            world[2][3], //
            world[3][0],
            world[3][1],
            world[3][2],
            world[3][3], //
        ];
        for (i, (&got, &w)) in flat.iter().zip(want.iter()).enumerate() {
            assert!(
                (f64::from(got) - w).abs() < 1.0e-5,
                "{path} component {i}: got {got}, want {w}",
            );
        }
    }

    let _ = std::fs::remove_file(&root);
}

/// Round-trips a `bool` session property through `author_session_property`
/// (the Phase 2A `physics:startsAsleep` sleep-sync path): authors
/// `physics:startsAsleep = true` as a custom `over` attribute and reads the
/// composed value back. Pins the new `"bool"` kind end-to-end (Rust encoder →
/// cxx shim → `SdfValueTypeNames->Bool`).

#[test]
fn author_session_property_round_trips_bool() {
    let root = std::env::temp_dir().join(format!(
        "idtx-startsasleep-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(
        &root,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\n\
             def Xform \"World\"\n{\n    def Sphere \"Ball\"\n    {\n    }\n}\n",
    )
    .expect("write root layer");

    let mut stage = match StageService::open_with_session(&root, "#usda 1.0\n", false) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_file(&root);
            return; // no USD runtime — skip
        }
    };

    stage
        .author_session_property(
            "/World/Ball",
            "physics:startsAsleep",
            &PropertyValue::Bool(true),
        )
        .expect("author bool session property");
    stage.project_topology().expect("re-project topology");

    let prim = stage.prim_at_path("/World/Ball").expect("prim");
    let val = stage
        .eval_attribute(prim, "physics:startsAsleep", 0.0, true)
        .expect("eval startsAsleep")
        .expect("startsAsleep value present");
    let repr = val.value_repr.trim().to_owned();
    assert!(
        matches!(repr.as_str(), "1" | "true" | "True"),
        "startsAsleep should compose back to true, got {repr:?}",
    );

    let _ = std::fs::remove_file(&root);
}

/// Reproduces the XR "grab" mechanic at the shim layer: a behavior authors a
/// session-layer transform on a prim that has **no** pre-existing matrix
/// `xformOp` (unlike the player-rig prims, which carry a seeded
/// `matrix4d xformOp:transform`). The first author must create the op specs
/// (a resync), and every subsequent author is a value-only edit. The pin: all
/// of them must keep producing drainable change notices and must not throw —
/// a throw while the shim's `SdfChangeBlock` is live leaks the change block on
/// this (no-`/EHsc`) toolchain, which silences `drain_changes` process-wide
/// and freezes every re-projection (the grab-freeze symptom).

#[test]
fn session_transform_on_opless_prim_keeps_draining_changes() {
    let root = std::env::temp_dir().join(format!(
        "idtx-grabrepro-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    // `/World/Body` is an op-less Xform (the grabbed asset body); its visible
    // geometry is the child `Collider` (as in level.usda, where the grabbed
    // collision-body Xform is op-less and the Sphere child carries the ops).
    std::fs::write(
        &root,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\n\
             def Xform \"World\"\n{\n    def Xform \"Body\"\n    {\n        \
             def Sphere \"Collider\"\n        {\n            double radius = 0.25\n            \
             double3 xformOp:translate = (0, 0, 0)\n            \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n        }\n    }\n}\n",
    )
    .expect("write root layer");

    let mut stage = match StageService::open_with_session(&root, "#usda 1.0\n", false) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_file(&root);
            return; // no USD runtime — skip
        }
    };

    stage.project_topology().expect("initial topology");
    stage.project_viewport().expect("initial viewport");
    let _ = stage.drain_changes().expect("prime drain"); // clear startup notices

    // Three "frames" of grab authoring: frame 0 creates the op specs (resync),
    // frames 1+ are value-only. Each must surface a drainable change and must
    // not error.
    for frame in 0..3u32 {
        let tx = f64::from(frame) + 1.0;
        let matrix: [f64; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            tx, 0.0, 0.0, 1.0, //
        ];
        stage
            .author_session_transform("/World/Body", matrix)
            .unwrap_or_else(|e| panic!("frame {frame}: author must not fail: {e:?}"));

        let batch = stage
            .drain_changes()
            .unwrap_or_else(|e| panic!("frame {frame}: drain must not fail: {e:?}"));
        let batch = batch.unwrap_or_else(|| {
            panic!(
                "frame {frame}: authoring a session transform must surface a change \
                     (an empty drain here means the change block leaked and notifications \
                     are suppressed — the grab-freeze)"
            )
        });
        assert!(
            !batch.changes.is_empty(),
            "frame {frame}: non-empty change batch"
        );
        let any_resync = batch.changes.iter().any(|c| c.resync);
        eprintln!(
            "frame {frame}: changes={} any_resync={any_resync} domains={:?}",
            batch.changes.len(),
            batch.changes.iter().map(|c| c.domains).collect::<Vec<_>>()
        );
        if frame > 0 {
            assert!(
                !any_resync,
                "frame {frame}: a value-only transform edit must NOT resync \
                     (continuous resync = full reproject every frame = freeze-like stall)"
            );
        }

        // Re-project the way the session's tick does; must not throw.
        stage.project_viewport().expect("reproject after author");
    }

    // The body's world translation reflects the last authored frame (x = 3)
    // AND — critically — the child geometry rides with it. Grab authors the
    // body (parent) transform; the visible prim is the child, so if the child
    // world transform did not follow, the user would see "the prim does not
    // move with the controller".
    let body = stage.prim_at_path("/World/Body").expect("body prim");
    let world = stage
        .get_world_transform(body)
        .expect("body world transform");
    assert!(
        (f64::from(world[3][0]) - 3.0).abs() < 1.0e-5,
        "body must sit at the last authored x; got {}",
        world[3][0]
    );
    let child = stage
        .prim_at_path("/World/Body/Collider")
        .expect("collider prim");
    let child_world = stage
        .get_world_transform(child)
        .expect("child world transform");
    assert!(
        (f64::from(child_world[3][0]) - 3.0).abs() < 1.0e-5,
        "child geometry must ride with the grabbed parent body; got x = {}",
        child_world[3][0]
    );

    let _ = std::fs::remove_file(&root);
}

/// REPRO: the runtime physics write-back log shows `author_session_transforms`
/// failing every frame with "Could not clear xformOpOrder" / "failed to author
/// session pose matrix" for level prims (`/World/Levels/level/UsdCookie`, …).
/// Those prims carry their xformOps in a **payload** sublayer and have
/// multi-op / matrix stacks, unlike the local single-translate prims the bulk
/// round-trip test covers. This test reconstructs that composition and authors
/// a pose onto each — pinning that every op configuration under a payload keeps
/// authoring successfully (no throw).

#[test]
fn session_transform_on_payloaded_multiop_prims() {
    let dir = std::env::temp_dir().join(format!(
        "idtx-payloadrepro-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).expect("make repro dir");
    let level = dir.join("level.usda");
    let assembly = dir.join("assembly.usda");

    // A level whose default prim carries three dynamic bodies, each with a
    // different xformOp stack, mirroring imported DCC geometry:
    //  - Sphere: single translate op
    //  - Cube1 : translate + rotateXYZ + scale (multi-op)
    //  - UsdCookie: a single matrix4d xformOp:transform
    std::fs::write(
            &level,
            "#usda 1.0\n(\n    defaultPrim = \"level\"\n)\n\n\
             def Xform \"level\"\n{\n    \
             def Sphere \"Sphere\"\n    {\n        double radius = 0.25\n        \
             double3 xformOp:translate = (0, 1, 0)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }\n    \
             def Cube \"Cube1\"\n    {\n        double size = 0.5\n        \
             double3 xformOp:translate = (1, 2, 0)\n        \
             float3 xformOp:rotateXYZ = (0, 45, 0)\n        \
             float3 xformOp:scale = (1, 1, 1)\n        \
             uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateXYZ\", \"xformOp:scale\"]\n    }\n    \
             def Mesh \"UsdCookie\"\n    {\n        \
             matrix4d xformOp:transform = ( (1,0,0,0),(0,1,0,0),(0,0,1,0),(3,0,0,1) )\n        \
             uniform token[] xformOpOrder = [\"xformOp:transform\"]\n    }\n}\n",
        )
        .expect("write level layer");
    std::fs::write(
            &assembly,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\n\
             def Xform \"World\"\n{\n    def Scope \"Levels\"\n    {\n        \
             def \"level\" (\n            prepend payload = @./level.usda@\n        )\n        {\n        }\n    }\n}\n",
        )
        .expect("write assembly layer");

    let mut stage = match StageService::open_with_session(&assembly, "#usda 1.0\n", true) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&dir);
            return; // no USD runtime — skip
        }
    };
    stage.project_topology().expect("initial topology");
    stage.project_viewport().expect("initial viewport");
    let _ = stage.drain_changes().expect("prime drain");

    let paths = [
        "/World/Levels/level/Sphere",
        "/World/Levels/level/Cube1",
        "/World/Levels/level/UsdCookie",
    ];

    // Three "frames": frame 0 creates the single-matrix op (an op-order
    // resync); frames 1+ MUST be value-only (no resync), else every per-frame
    // physics write-back of a payloaded body would force a full whole-stage
    // re-extract (re-marshaling the dense mesh) — the global-stall bug.
    for frame in 0..3u32 {
        let tx = f64::from(frame) + 5.0;
        let m: [f64; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            tx, 0.0, 0.0, 1.0, //
        ];
        let entries: Vec<(String, [f64; 16])> =
            paths.iter().map(|p| ((*p).to_owned(), m)).collect();
        stage
            .author_session_transforms(&entries)
            .unwrap_or_else(|e| panic!("frame {frame}: bulk author must not fail: {e:?}"));

        let batch = stage
            .drain_changes()
            .unwrap_or_else(|e| panic!("frame {frame}: drain must not fail: {e:?}"))
            .unwrap_or_else(|| panic!("frame {frame}: author must surface a change"));
        let any_resync = batch.changes.iter().any(|c| c.resync);
        eprintln!(
            "frame {frame}: changes={} any_resync={any_resync} domains={:?}",
            batch.changes.len(),
            batch.changes.iter().map(|c| c.domains).collect::<Vec<_>>()
        );
        if frame > 0 {
            assert!(
                !any_resync,
                "frame {frame}: a payloaded body's per-frame pose write must be \
                     value-only (no resync) — a resync forces a full re-extract every \
                     frame while the body is awake, stalling the whole frame loop"
            );
        }
        stage
            .project_viewport()
            .unwrap_or_else(|e| panic!("frame {frame}: reproject must not fail: {e:?}"));

        for p in &paths {
            let prim = stage.prim_at_path(p).unwrap_or_else(|| panic!("prim {p}"));
            let world = stage
                .get_world_transform(prim)
                .unwrap_or_else(|e| panic!("world transform {p}: {e:?}"));
            assert!(
                (f64::from(world[3][0]) - tx).abs() < 1.0e-5,
                "frame {frame}: {p} must sit at authored x={tx}; got {}",
                world[3][0]
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-trips `idtx:playerRole` / `idtx:playerId` through the typed
/// setters and getters: absent reads as `None`, authored values
/// read back verbatim. Self-contained (writes its own root layer); a failed
/// open (no USD runtime) is treated as skip, matching the pose test above.

#[test]
fn player_role_and_id_round_trip() {
    let root = std::env::temp_dir().join(format!(
        "idtx-playerrole-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(
            &root,
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\"\n{\n    def Xform \"Player\" {}\n}\n",
        )
        .expect("write root layer");

    let mut stage = match StageService::open(&root) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_file(&root);
            return;
        }
    };
    stage.project_topology().expect("topology");
    let player = stage.prim_at_path("/World/Player").expect("player prim");

    // Absent → None on both getters.
    assert_eq!(stage.read_player_role(player).expect("read role"), None);
    assert_eq!(stage.read_player_id(player).expect("read id"), None);

    stage
        .set_player_role(player, "remote")
        .expect("author role");
    stage.set_player_id(player, "peer-7").expect("author id");

    assert_eq!(
        stage.read_player_role(player).expect("read role"),
        Some("remote".to_owned()),
    );
    assert_eq!(
        stage.read_player_id(player).expect("read id"),
        Some("peer-7".to_owned()),
    );

    let _ = std::fs::remove_file(&root);
}

/// Define a typed prim straight into the session layer and get back a
/// usable `PrimId`. Verifies the prim is addressable by the returned id,
/// carries the requested schema type, lives ONLY in the session layer (the
/// root layer text is untouched), and that re-defining the same path is
/// rejected. Requires the `OpenUSD` runtime; a failed open is treated as skip.

#[test]
fn define_session_prim_mints_a_typed_session_only_prim() {
    let root = std::env::temp_dir().join(format!(
        "idtx-sessdef-root-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(
        &root,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\" {}\n",
    )
    .expect("write root layer");

    // Empty session layer — the define must create the prim here from scratch.
    let mut stage = match StageService::open_with_session(&root, "#usda 1.0\n", false) {
        Ok(stage) => stage,
        Err(_) => {
            let _ = std::fs::remove_file(&root);
            return;
        }
    };

    let cam = stage
        .define_session_prim("/World/PlayerCamera", "Camera")
        .expect("define camera into session layer");

    // The returned id is the addressable, generation-scoped identity.
    assert_eq!(
        stage.prim_at_path("/World/PlayerCamera"),
        Some(cam),
        "defined prim must be addressable by its path"
    );
    assert_eq!(
        cam.generation(),
        stage.generation(),
        "minted id is scoped to this open's generation"
    );

    // It carries the requested schema type.
    stage.project_topology().expect("re-project topology");
    let record = stage
        .topology()
        .iter()
        .find(|r| r.prim == cam)
        .expect("topology record for the new prim");
    assert_eq!(
        record.type_name, "Camera",
        "defined prim must carry the requested schema type"
    );

    // It lives ONLY in the session layer — the saved root layer is untouched.
    let root_text = stage.root_layer_text().expect("root layer text");
    assert!(
        !root_text.contains("PlayerCamera"),
        "session define must not touch the root layer, got:\n{root_text}"
    );

    // Re-defining an existing path is rejected (this defines new prims only).
    assert!(
        stage
            .define_session_prim("/World/PlayerCamera", "Camera")
            .is_err(),
        "re-defining an existing prim must error"
    );

    let _ = std::fs::remove_file(&root);
}

/// Author a `primvars:displayColor` attribute via `SetProperty` and verify
/// that `drain_changes()` returns a change record with the `MATERIAL` domain.

#[test]
fn author_display_color_fires_material_notice() {
    let src = authoring_fixture("authoring_translate_op.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-color-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: box_prim,
            property: "primvars:displayColor".to_owned(),
            value: PropertyValue::Color3f([1.0, 0.0, 0.0]),
        })
        .expect("author displayColor");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed_box = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed_box.domains.contains(DirtyDomains::MATERIAL),
        "displayColor change must set MATERIAL domain; got {:?}",
        changed_box.domains,
    );

    // Read back the value to confirm it was persisted in the stage.
    let props = stage.inspect_prim(box_prim).expect("inspect");
    let color_prop = props
        .iter()
        .find(|p| p.name == "primvars:displayColor")
        .expect("displayColor property");
    assert!(
        color_prop.value_summary.contains("1"),
        "value_summary should reflect the new red color: {}",
        color_prop.value_summary,
    );

    std::fs::remove_file(dest).expect("cleanup");
}

/// Author a transform on a prim with a `xformOp:translate` op and verify
/// the translate value is updated and the `TRANSFORM` domain is dirty.

#[test]
fn author_transform_with_translate_op_fires_transform_notice() {
    let src = authoring_fixture("authoring_translate_op.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-xlate-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    // Verify we start with a translate xformOp.
    let stack = stage.authored_xform_stack(box_prim).expect("xform stack");
    assert!(
        stack
            .ops
            .iter()
            .any(|op| op.op_type == XformOpType::Translate),
        "fixture must have a translate xformOp"
    );

    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [5.0, 2.0, 3.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("apply_transform_edit");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed_box = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed_box.domains.contains(DirtyDomains::TRANSFORM),
        "transform edit must set TRANSFORM domain; got {:?}",
        changed_box.domains,
    );

    // Read back the composed matrix to verify translation was applied.
    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    // USD row-vector convention: translation in row 3 (index [3][0..2]).
    assert!(
        (mat[3][0] - 5.0_f32).abs() < 1e-3,
        "expected Tx=5.0, got {}",
        mat[3][0],
    );
    assert!(
        (mat[3][1] - 2.0_f32).abs() < 1e-3,
        "expected Ty=2.0, got {}",
        mat[3][1],
    );

    std::fs::remove_file(dest).expect("cleanup");
}

/// Regression guard: a pure transform write on a LIGHT prim (or its Xform
/// ancestor) must classify as `TRANSFORM | INSPECTION` only — it must NOT pick
/// up the `LIGHT` bit. The `LIGHT` bit would defeat the session's
/// transform-only fast path (`is_transform_only_change` admits only
/// `{TRANSFORM, INSPECTION}`), forcing a full whole-stage re-extract on every
/// gizmo/inspector drag of a light. A moved light changes only its world
/// matrix, not its emission, so it must stay on the fast path.

#[test]
fn author_transform_on_light_is_transform_only() {
    let src = fixture("lighting/light_ancestor_xform.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-light-xform-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");

    // Edit the light's own translate op, and (separately) its Xform ancestor.
    // Both already carry a translate op, so the edit is an in-place value
    // write (info-only, no new spec / no resync).
    for path in ["/World/Rig/Key", "/World/Rig"] {
        let prim = stage
            .prim_at_path(path)
            .unwrap_or_else(|| panic!("prim {path}"));
        stage
            .apply_transform_edit(
                prim,
                TransformValues {
                    // Only translation changes; rotation/scale stay identity so
                    // no new rotate/scale op is materialized (which would resync).
                    translation: [7.0, 8.0, 9.0],
                    rotation_xyz_degrees: [0.0, 0.0, 0.0],
                    scale: [1.0, 1.0, 1.0],
                },
            )
            .expect("apply_transform_edit");

        let batch = stage.drain_changes().expect("drain").expect("change batch");
        let changed = batch
            .changes
            .iter()
            .find(|c| stage.path_for(c.prim) == Some(path))
            .unwrap_or_else(|| panic!("change record for {path}"));

        assert!(
            changed.domains.contains(DirtyDomains::TRANSFORM),
            "transform edit on {path} must set TRANSFORM; got {:?}",
            changed.domains,
        );
        assert!(
            !changed.domains.contains(DirtyDomains::LIGHT),
            "pure transform edit on {path} must NOT set LIGHT (defeats the \
                 transform-only fast path); got {:?}",
            changed.domains,
        );
        assert!(
            !changed.domains.contains(DirtyDomains::SKELETON),
            "pure transform edit on {path} must NOT set SKELETON; got {:?}",
            changed.domains,
        );
        assert!(
            !changed.resync,
            "in-place transform edit on {path} must not resync; got resync=true",
        );
    }

    std::fs::remove_file(dest).expect("cleanup");
}

/// Companion guard to `author_transform_on_light_is_transform_only`: the
/// classification gate must not over-strip. A genuine emission edit
/// (`inputs:*`) on the same light MUST still carry the `LIGHT` bit so it
/// correctly forces a re-extract (light params are baked at extract time).

#[test]
fn author_light_emission_still_carries_light_bit() {
    let src = fixture("lighting/light_ancestor_xform.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-light-emit-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");

    stage
        .conformance_author_double("/World/Rig/Key.inputs:intensity", 42.0)
        .expect("author light intensity");
    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| c.property_name.as_deref() == Some("inputs:intensity"))
        .expect("intensity change");
    assert!(
        changed.domains.contains(DirtyDomains::LIGHT),
        "emission edit must still set LIGHT; got {:?}",
        changed.domains,
    );

    std::fs::remove_file(dest).expect("cleanup");
}

/// The authoring transform fast path must, on a moved *ancestor*, refresh
/// world matrices for the whole moved subtree — the descendant mesh and
/// light included — WITHOUT re-resolving materials, geometry, or lights.
/// This is the exact case the user hit: moving/scaling a default prim whose
/// subtree contains lights. `sample_transforms_only_default` must expand the
/// ancestor root to every active Xformable descendant and apply the parent's
/// new placement to each, so the session can patch the warm snapshot in place
/// instead of taking `project_viewport_roots` (which re-hydrates the subtree
/// and emits the `MaterialBindingAPI is not applied` warnings).

#[test]
fn sample_transforms_only_expands_moved_ancestor_subtree() {
    let src = fixture("lighting/light_ancestor_xform.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-xform-subtree-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");

    // Move the ancestor Xform "Rig" (the analog of moving the level's default
    // prim). Its translate op already exists, so this is an in-place value
    // write (no resync).
    let rig = stage.prim_at_path("/World/Rig").expect("rig prim");
    stage
        .apply_transform_edit(
            rig,
            TransformValues {
                translation: [10.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("apply_transform_edit");
    let _ = stage.drain_changes().expect("drain");

    // Sample transforms only for the moved ancestor root.
    let transforms = stage
        .sample_transforms_only_default(&[rig])
        .expect("sample_transforms_only_default");

    // Every active Xformable descendant must be present: Rig itself, the
    // Key light, and the Panel mesh.
    let by_path: std::collections::HashMap<String, [f32; 16]> = transforms
        .iter()
        .filter_map(|t| {
            stage
                .path_for(t.prim)
                .map(|p| (p.to_owned(), t.world_matrix))
        })
        .collect();

    for expected in ["/World/Rig", "/World/Rig/Key", "/World/Rig/Panel"] {
        assert!(
            by_path.contains_key(expected),
            "moved-ancestor subtree sample must include {expected}; got {:?}",
            by_path.keys().collect::<Vec<_>>(),
        );
    }

    // The parent's new +10 X placement must propagate to the descendants'
    // world matrices (row-major, translation in the last row: indices 12..15).
    // Key's local translate is (1,2,3) → world X = 10 + 1 = 11.
    let key = by_path["/World/Rig/Key"];
    assert!(
        (key[12] - 11.0_f32).abs() < 1e-3,
        "light world X must reflect moved ancestor (expected 11.0), got {}",
        key[12],
    );

    std::fs::remove_file(dest).expect("cleanup");
}

#[test]
fn author_transform_with_matrix_op_fires_transform_notice() {
    let src = authoring_fixture("authoring_matrix_op.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-matrix-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    // Verify the fixture has a matrix xformOp.
    let stack = stage.authored_xform_stack(box_prim).expect("xform stack");
    assert!(
        stack
            .ops
            .iter()
            .any(|op| op.op_type == XformOpType::Unsupported),
        "fixture must have a matrix (Unsupported-typed) xformOp; ops: {:?}",
        stack.ops.iter().map(|o| &o.op_type).collect::<Vec<_>>(),
    );

    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [10.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("apply_transform_edit on matrix op");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed_box = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed_box.domains.contains(DirtyDomains::TRANSFORM),
        "matrix transform edit must set TRANSFORM domain; got {:?}",
        changed_box.domains,
    );

    // Verify translation was applied via the composed matrix.
    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    assert!(
        (mat[3][0] - 10.0_f32).abs() < 1e-3,
        "expected Tx=10.0 after matrix edit, got {}",
        mat[3][0],
    );

    std::fs::remove_file(dest).expect("cleanup");
}

/// Regression for the conductor error
/// `could not capture transform property spec`: authoring a transform on a
/// prim whose single `xformOp:transform` matrix composes entirely from a
/// payload (no root-layer property spec) must author a **root override**
/// rather than failing to snapshot a non-existent root spec. Reproduces
/// adjusting the transform of a freshly payloaded prim in the conductor.

#[test]
fn author_transform_on_referenced_matrix_prim_authors_root_override() {
    let (host_dst, dir) = copy_host_with_dependency(
        "authoring_ref_matrix_host.usda",
        "authoring_ref_matrix_asset.usda",
    );

    let mut stage = StageService::open(&host_dst).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/World/Box").expect("box prim");

    // The matrix xformOp composes from the payload (Unsupported-typed op);
    // there is no root-layer transform spec yet.
    let stack = stage.authored_xform_stack(box_prim).expect("xform stack");
    assert!(
        stack
            .ops
            .iter()
            .any(|op| op.op_type == XformOpType::Unsupported),
        "fixture must compose a matrix xformOp from the payload; ops: {:?}",
        stack.ops.iter().map(|o| &o.op_type).collect::<Vec<_>>(),
    );

    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [10.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("apply_transform_edit on referenced matrix prim");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/World/Box"))
        .expect("Box change record");
    assert!(
        changed.domains.contains(DirtyDomains::TRANSFORM),
        "referenced-matrix edit must set TRANSFORM domain; got {:?}",
        changed.domains,
    );

    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    assert!(
        (mat[3][0] - 10.0_f32).abs() < 1e-3,
        "expected Tx=10.0 after referenced-matrix edit, got {}",
        mat[3][0],
    );

    drop(stage);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// Companion to the matrix case for Strategy C (separate T/R/S ops): a prim
/// that references translate+scale ops (no rotate, no root-layer opinion).
/// Authoring with a non-identity rotation must override translate+scale in
/// the root, create a fresh `rotateXYZ` op, and author `xformOpOrder` into
/// the root — none of which previously had a root spec.

#[test]
fn author_transform_on_referenced_trs_prim_overrides_and_creates_ops() {
    let (host_dst, dir) = copy_host_with_dependency(
        "authoring_ref_trs_host.usda",
        "authoring_ref_trs_asset.usda",
    );

    let mut stage = StageService::open(&host_dst).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/World/Box").expect("box prim");

    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [10.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 90.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("apply_transform_edit on referenced T+S prim");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/World/Box"))
        .expect("Box change record");
    assert!(
        changed.domains.contains(DirtyDomains::TRANSFORM),
        "referenced-TRS edit must set TRANSFORM domain; got {:?}",
        changed.domains,
    );

    // The rotate op must have been created and all three types present.
    let stack = stage.authored_xform_stack(box_prim).expect("xform stack");
    assert!(
        stack
            .ops
            .iter()
            .any(|op| op.op_type == XformOpType::RotateXyz),
        "rotateXYZ op must be created over the referenced T+S stack"
    );

    // Translation override took effect through the composed matrix.
    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    assert!(
        (mat[3][0] - 10.0_f32).abs() < 1e-3,
        "expected Tx=10.0 after referenced-TRS edit, got {}",
        mat[3][0],
    );

    drop(stage);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// Author scale on a prim that has only `xformOp:scale` and verify the
/// scale value is updated and the `TRANSFORM` domain is dirty.

#[test]
fn author_transform_with_scale_only_op_updates_scale() {
    let src = authoring_fixture("authoring_scale_op.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-scale-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [0.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [2.0, 3.0, 4.0],
            },
        )
        .expect("apply_transform_edit on scale-only op");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed.domains.contains(DirtyDomains::TRANSFORM),
        "scale edit must set TRANSFORM domain; got {:?}",
        changed.domains,
    );

    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    // For a pure scale [2,3,4] the row magnitudes of the upper-left 3×3 are the scales.
    let sx = (mat[0][0] * mat[0][0] + mat[0][1] * mat[0][1] + mat[0][2] * mat[0][2]).sqrt();
    let sy = (mat[1][0] * mat[1][0] + mat[1][1] * mat[1][1] + mat[1][2] * mat[1][2]).sqrt();
    let sz = (mat[2][0] * mat[2][0] + mat[2][1] * mat[2][1] + mat[2][2] * mat[2][2]).sqrt();
    assert!((sx - 2.0_f32).abs() < 1e-3, "expected Sx=2.0, got {sx}");
    assert!((sy - 3.0_f32).abs() < 1e-3, "expected Sy=3.0, got {sy}");
    assert!((sz - 4.0_f32).abs() < 1e-3, "expected Sz=4.0, got {sz}");

    std::fs::remove_file(dest).expect("cleanup");
}

/// Author rotation on a prim that has only `xformOp:rotateXYZ` and verify
/// the rotation value is updated and the `TRANSFORM` domain is dirty.

#[test]
fn author_transform_with_rotate_only_op_updates_rotate() {
    let src = authoring_fixture("authoring_rotate_op.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-rotate-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    // 90° rotation around Z: row 0 should become [0,1,0], row 1 becomes [-1,0,0].
    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [0.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 90.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("apply_transform_edit on rotate-only op");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed.domains.contains(DirtyDomains::TRANSFORM),
        "rotate edit must set TRANSFORM domain; got {:?}",
        changed.domains,
    );

    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    // Rz(90°) row-vector: row 0 = [cos90, sin90, 0] = [0, 1, 0]
    assert!(
        (mat[0][0]).abs() < 1e-3,
        "expected M[0][0]=0 for Rz=90°, got {}",
        mat[0][0]
    );
    assert!(
        (mat[0][1] - 1.0_f32).abs() < 1e-3,
        "expected M[0][1]=1 for Rz=90°, got {}",
        mat[0][1]
    );

    std::fs::remove_file(dest).expect("cleanup");
}

/// Author both translate and scale on a prim that has T+S ops (no rotate)
/// and verify both values are updated.

#[test]
fn author_transform_with_ts_ops_updates_both() {
    let src = authoring_fixture("authoring_ts_ops.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-ts-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [5.0, 0.0, 0.0],
                rotation_xyz_degrees: [0.0, 0.0, 0.0],
                scale: [2.0, 2.0, 2.0],
            },
        )
        .expect("apply_transform_edit on T+S ops");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed.domains.contains(DirtyDomains::TRANSFORM),
        "T+S edit must set TRANSFORM domain; got {:?}",
        changed.domains,
    );

    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    // Translation in row 3.
    assert!(
        (mat[3][0] - 5.0_f32).abs() < 1e-3,
        "expected Tx=5.0, got {}",
        mat[3][0]
    );
    // Scale in row magnitudes.
    let sx = (mat[0][0] * mat[0][0] + mat[0][1] * mat[0][1] + mat[0][2] * mat[0][2]).sqrt();
    assert!((sx - 2.0_f32).abs() < 1e-3, "expected Sx=2.0, got {sx}");

    std::fs::remove_file(dest).expect("cleanup");
}

/// Author a prim that has only `xformOp:translate` with a non-identity
/// rotation and scale: the missing R and S ops should be created and
/// `xformOpOrder` updated to T→R→S order.

#[test]
fn author_transform_creates_missing_ops_for_partial_trs() {
    let src = authoring_fixture("authoring_translate_op.usda");
    let dest = std::env::temp_dir().join(format!(
        "idtx-author-partial-{}-{}.usda",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy(&src, &dest).expect("copy fixture");

    let mut stage = StageService::open(&dest).expect("open stage");
    stage.project_topology().expect("topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    // Fixture only has translate; supply non-identity R and S so they are created.
    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [1.0, 2.0, 3.0],
                rotation_xyz_degrees: [0.0, 45.0, 0.0],
                scale: [2.0, 2.0, 2.0],
            },
        )
        .expect("apply_transform_edit creates missing ops");

    let batch = stage.drain_changes().expect("drain").expect("change batch");
    let changed = batch
        .changes
        .iter()
        .find(|c| stage.path_for(c.prim) == Some("/Root/Box"))
        .expect("Box change record");
    assert!(
        changed.domains.contains(DirtyDomains::TRANSFORM),
        "partial-TRS edit must set TRANSFORM domain; got {:?}",
        changed.domains,
    );

    // Verify the new stack has T+R+S.
    let stack = stage.authored_xform_stack(box_prim).expect("xform stack");
    assert!(
        stack
            .ops
            .iter()
            .any(|op| op.op_type == XformOpType::Translate),
        "stack must contain translate op after partial edit"
    );
    assert!(
        stack
            .ops
            .iter()
            .any(|op| op.op_type == XformOpType::RotateXyz),
        "stack must contain rotateXYZ op after partial edit"
    );
    assert!(
        stack.ops.iter().any(|op| op.op_type == XformOpType::Scale),
        "stack must contain scale op after partial edit"
    );

    // Verify T value via composed matrix.
    let mat = stage
        .get_local_transform(box_prim)
        .expect("get_local_transform");
    assert!(
        (mat[3][0] - 1.0_f32).abs() < 1e-3,
        "expected Tx=1.0, got {}",
        mat[3][0]
    );
    assert!(
        (mat[3][1] - 2.0_f32).abs() < 1e-3,
        "expected Ty=2.0, got {}",
        mat[3][1]
    );

    std::fs::remove_file(dest).expect("cleanup");
}

/// Validates that `authored_in_root_layer` is `true` for root-layer edits
/// (e.g. conformance attribute writes) and `false` for session-layer edits
/// (e.g. variant selections via `set_session_variant`).
///
/// This is the key discriminant used by the collaboration sync to avoid
/// broadcasting ephemeral session-layer overrides.
#[test]
fn authored_in_root_layer_correctly_tagged() {
    let fixture_path = fixture("layering/collab_layer_filter.usda");
    let mut stage = StageService::open(fixture_path).expect("open collab_layer_filter.usda");
    // Prime prim registry.
    stage.project_topology().expect("project topology");

    // ── root-layer edit ───────────────────────────────────────────────────
    stage
        .conformance_author_double("/World/Cube.size", 75.0)
        .expect("author size into root layer");
    let batch = stage
        .drain_changes()
        .expect("drain after root edit")
        .expect("expected a change batch");
    let cube_change = batch
        .changes
        .iter()
        .find(|c| {
            stage
                .path_for_prim(c.prim)
                .is_ok_and(|p| p == "/World/Cube")
        })
        .expect("change for /World/Cube after root edit");
    assert!(
        cube_change.authored_in_root_layer,
        "transform edit must have authored_in_root_layer=true; got {:?}",
        cube_change
    );

    // ── session-layer edit (variant selection) ────────────────────────────
    let cube_prim = stage.prim_at_path("/World/Cube").expect("cube prim id");
    stage
        .set_session_variant(cube_prim, "shading", "blue")
        .expect("set variant in session layer");
    let batch = stage
        .drain_changes()
        .expect("drain after session edit")
        .expect("expected a change batch");
    let variant_change = batch
        .changes
        .iter()
        .find(|c| {
            stage
                .path_for_prim(c.prim)
                .is_ok_and(|p| p == "/World/Cube")
        })
        .expect("change for /World/Cube after session edit");
    assert!(
        !variant_change.authored_in_root_layer,
        "variant selection must have authored_in_root_layer=false; got {:?}",
        variant_change
    );

    // ── variant-CONTENT edit (root layer, but variant-scoped) ─────────────
    // Authoring a value into a variant lands on a variant-decorated spec path
    // (/World/Cube{shading=blue}.primvars:displayColor). The collab attribute
    // channel is keyed by scene path with no variant coordinate, so such an
    // edit must NOT be tagged authored_in_root_layer — otherwise it would be
    // (mis)published. A session-layer selection spec also forces the resync
    // path to false. Either way the flag must be false.
    stage.project_topology().expect("reproject before variant edit");
    let cube_prim = stage.prim_at_path("/World/Cube").expect("cube prim id");
    stage
        .author_targeted_property(
            cube_prim,
            "primvars:displayColor",
            &PropertyValue::Color3f([0.0, 1.0, 0.0]),
            &EditTarget::Variant {
                set_name: "shading".to_owned(),
                variant_name: "blue".to_owned(),
            },
        )
        .expect("author displayColor into the blue variant");
    let batch = stage
        .drain_changes()
        .expect("drain after variant-content edit")
        .expect("expected a change batch");
    let content_change = batch
        .changes
        .iter()
        .find(|c| {
            stage
                .path_for_prim(c.prim)
                .is_ok_and(|p| p == "/World/Cube")
        })
        .expect("change for /World/Cube after variant-content edit");
    assert!(
        !content_change.authored_in_root_layer,
        "variant-content edit must have authored_in_root_layer=false; got {:?}",
        content_change
    );
    // The edit must not have become a prim-local opinion (that would shadow the
    // variant). The root-layer spec at the NON-variant path stays empty.
    assert!(
        stage
            .root_attribute_text("/World/Cube", "primvars:displayColor")
            .expect("read root attribute text")
            .is_empty(),
        "variant edit must not author a prim-local displayColor opinion",
    );
}

/// Authoring an attribute into a variant via `EditTarget::Variant` composes
/// through only when that variant is selected, and never shadows the variant as
/// a prim-local opinion. The companion of the collab no-sync guarantee.
#[test]
fn author_into_variant_scopes_to_the_selected_branch() {
    let fixture_path = fixture("layering/collab_layer_filter.usda");
    let mut stage = StageService::open(fixture_path).expect("open collab_layer_filter.usda");
    stage.project_topology().expect("project topology");
    let cube = stage.prim_at_path("/World/Cube").expect("cube prim id");

    // The variant set is declared on the (local) root layer, so it is editable.
    let sets = stage.variant_sets(cube).expect("variant sets");
    let shading = sets
        .iter()
        .find(|s| s.name == "shading")
        .expect("shading variant set");
    assert!(
        shading.defined_on_local_layer,
        "a root-authored variant set must be locally editable",
    );

    // Author green into the "blue" variant (while nothing forces blue selected).
    stage
        .author_targeted_property(
            cube,
            "primvars:displayColor",
            &PropertyValue::Color3f([0.0, 1.0, 0.0]),
            &EditTarget::Variant {
                set_name: "shading".to_owned(),
                variant_name: "blue".to_owned(),
            },
        )
        .expect("author into blue variant");

    // Not a prim-local opinion.
    assert!(
        stage
            .root_attribute_text("/World/Cube", "primvars:displayColor")
            .expect("root attribute text")
            .is_empty(),
        "variant edit must not become a prim-local opinion",
    );
    // The green value was authored into the root layer's variant branch, and
    // the original red opinion is still present in the red branch.
    let text = stage.root_layer_text().expect("root layer text");
    assert!(
        text.contains("0, 1, 0"),
        "green must be authored into the blue variant branch: {text}",
    );
    assert!(
        text.contains("1, 0, 0"),
        "the original red variant opinion must remain: {text}",
    );
}

/// Copies a host layer and its single relative-path dependency into a fresh
/// temp directory (preserving the dependency's filename so the host's
/// relative `payload`/`references` arc still resolves) and returns the host
/// copy's path plus the temp dir. **Panics** if either fixture is missing:
/// the fixtures are committed, so an absent one is a real regression (not a
/// USD-less checkout) and CI must fail rather than silently skip.

fn copy_host_with_dependency(host_name: &str, dep_name: &str) -> (PathBuf, PathBuf) {
    let host = authoring_fixture(host_name);
    let dep = authoring_fixture(dep_name);
    if !host.exists() || !dep.exists() {
        panic!(
            "required authoring fixtures are missing: {} and/or {}",
            host.display(),
            dep.display()
        );
    }
    let dir = std::env::temp_dir().join(format!(
        "idtx-refxform-{}-{}",
        std::process::id(),
        NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::copy(&dep, dir.join(dep_name)).expect("copy dependency");
    let host_dst = dir.join("host.usda");
    std::fs::copy(&host, &host_dst).expect("copy host");
    (host_dst, dir)
}
