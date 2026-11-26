//! USD-to-viewport purpose regressions. Run with `cargo test -p
//! idtx-studio-session --features authoring,editor-overlay --lib purpose_tests
//! -- --include-ignored` and the desktop OpenUSD runtime on PATH.

use super::*;
use std::collections::BTreeSet;

const STAGE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)
def Xform "World" {
    def Cube "Plain" {
        double size = 1
        double3 xformOp:translate = (4, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Cube "AuthoredDefault" {
        uniform token purpose = "default"
        double size = 1
        double3 xformOp:translate = (0, 0, -3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Cube "Proxy" {
        uniform token purpose = "proxy"
        double size = 1
        double3 xformOp:translate = (0, 0, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Cube "Render" {
        uniform token purpose = "render"
        double size = 1
    }
    def Xform "Guide" {
        uniform token purpose = "guide"
        def Cube "Shape" {
            double size = 1
            double3 xformOp:translate = (0, 0, 6)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
        def "IgnoredAuthored" {
            uniform token purpose = "render"
            def Cube "Inherited" {
                double size = 1
                double3 xformOp:translate = (-4, 0, 0)
                uniform token[] xformOpOrder = ["xformOp:translate"]
            }
        }
        def Cube "Reset" {
            uniform token purpose = "default"
            double size = 1
            double3 xformOp:translate = (8, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def "NonImageable" {
        uniform token purpose = "guide"
        def Cube "Shape" {
            double size = 1
            double3 xformOp:translate = (12, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
}
"#;

fn open() -> (StudioSession, tempfile::TempDir) {
    open_source(STAGE)
}

fn open_source(source: &str) -> (StudioSession, tempfile::TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("idtx-purpose-session-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("purposes.usda");
    std::fs::write(&path, source).unwrap();
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
        path,
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .unwrap();
    (session, dir)
}

fn paths(snapshot: &RenderWorldSnapshot, guides: bool) -> BTreeSet<String> {
    let (meshes, batches) = if guides {
        (&snapshot.guide_meshes, &snapshot.guide_instanced_batches)
    } else {
        (&snapshot.meshes, &snapshot.instanced_batches)
    };
    meshes
        .iter()
        .map(|mesh| mesh.prim_path.clone())
        .chain(
            batches
                .iter()
                .flat_map(|batch| batch.instance_prim_paths.iter().cloned()),
        )
        .collect()
}

fn default_paths() -> BTreeSet<String> {
    [
        "/World/Plain",
        "/World/AuthoredDefault",
        "/World/Guide/Reset",
        "/World/NonImageable/Shape",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn pick(session: &mut StudioSession, x: f32) -> Option<String> {
    let basis = session.projection_basis();
    session.pick_and_resolve(
        basis.transform_point3(Vec3::new(x, 0.0, 10.0)),
        basis.transform_vector3(Vec3::NEG_Z).normalize(),
        &HashSet::new(),
    )
}

#[test]
#[ignore = "requires OpenUSD and GPU"]
fn purpose_filters_refresh_cached_rendering_and_pick_only_visible_prims() {
    let (mut session, _dir) = open();
    let topology = session.topology_generation;
    let entities = session.hydrator.world().entities().len();
    let guides: BTreeSet<_> = [
        "/World/Guide/Shape",
        "/World/Guide/IgnoredAuthored/Inherited",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let mut previous_serial = None;
    for (purpose, expected_hit) in [
        (RenderPurpose::Default, "/World/Guide/Shape"),
        (RenderPurpose::Guide, "/World/Guide/Shape"),
        (RenderPurpose::Proxy, "/World/Proxy"),
        (RenderPurpose::Render, "/World/Render"),
        (RenderPurpose::Default, "/World/Guide/Shape"),
    ] {
        session.set_viewport_purpose(purpose);
        assert!(
            session.snapshot_dirty,
            "changing purpose must invalidate the cached frame"
        );
        let mut expected = default_paths();
        if matches!(purpose, RenderPurpose::Default | RenderPurpose::Proxy) {
            expected.insert("/World/Proxy".into());
        }
        if matches!(purpose, RenderPurpose::Default | RenderPurpose::Render) {
            expected.insert("/World/Render".into());
        }
        let expected_guides = if matches!(purpose, RenderPurpose::Default | RenderPurpose::Guide) {
            guides.clone()
        } else {
            BTreeSet::new()
        };
        session.render_one_frame(false);
        let serial = session.last_snapshot.as_ref().unwrap().serial;
        assert_ne!(
            Some(serial),
            previous_serial,
            "purpose changes require fresh extraction"
        );
        previous_serial = Some(serial);
        for wireframe in [false, true] {
            session.set_viewport_purpose(purpose);
            assert!(
                !session.snapshot_dirty,
                "reselecting the current purpose must reuse the frame"
            );
            session.render_one_frame(wireframe);
            let snapshot = session.last_snapshot.as_ref().unwrap();
            assert_eq!(snapshot.serial, serial);
            assert_eq!(
                snapshot.purpose_filter,
                Some(session.viewport_purpose_filter())
            );
            assert_eq!(
                paths(snapshot, false),
                expected,
                "{purpose:?}, wireframe={wireframe}"
            );
            assert_eq!(paths(snapshot, true), expected_guides);
            assert!(
                snapshot
                    .meshes
                    .iter()
                    .all(|mesh| mesh.purpose != RenderPurpose::Guide)
            );
            assert!(
                snapshot
                    .instanced_batches
                    .iter()
                    .all(|batch| batch.purpose != RenderPurpose::Guide)
            );
            assert!(snapshot.guide_meshes.iter().all(|mesh| !mesh.casts_shadow));
            assert!(
                snapshot
                    .guide_instanced_batches
                    .iter()
                    .all(|batch| !batch.casts_shadow)
            );
            #[cfg(feature = "editor-overlay")]
            assert_eq!(
                snapshot.guide_wireframe_lines().is_empty(),
                expected_guides.is_empty()
            );
            assert_eq!(pick(&mut session, 0.0).as_deref(), Some(expected_hit));
            assert_eq!(pick(&mut session, 4.0).as_deref(), Some("/World/Plain"));
            assert_eq!(
                pick(&mut session, 8.0).as_deref(),
                Some("/World/Guide/Reset")
            );
        }
        assert_eq!(session.topology_generation, topology);
        assert_eq!(session.hydrator.world().entities().len(), entities);
    }
}

#[test]
#[cfg(feature = "authoring")]
#[ignore = "requires OpenUSD and GPU"]
fn purpose_edits_recompute_imageable_inheritance_and_refresh_filtered_snapshot() {
    let (mut session, _dir) = open();
    for (path, expected) in [
        ("/World/Guide", RenderPurpose::Guide),
        ("/World/Guide/Shape", RenderPurpose::Guide),
        ("/World/Guide/IgnoredAuthored", RenderPurpose::Guide),
        (
            "/World/Guide/IgnoredAuthored/Inherited",
            RenderPurpose::Guide,
        ),
        ("/World/Guide/Reset", RenderPurpose::Default),
        ("/World/NonImageable", RenderPurpose::Default),
        ("/World/NonImageable/Shape", RenderPurpose::Default),
        ("/World/Plain", RenderPurpose::Default),
        ("/World/AuthoredDefault", RenderPurpose::Default),
    ] {
        assert_eq!(session.prim_purpose(path), expected, "{path}");
    }
    session.set_viewport_purpose(RenderPurpose::Guide);
    session.render_one_frame(false);
    let before = session.last_snapshot.as_ref().unwrap().serial;
    session
        .author_attribute(
            "/World/Guide",
            "purpose",
            idtx_usd::PropertyValue::Token("render".into()),
        )
        .unwrap();
    session.tick().unwrap();
    session.render_one_frame(false);
    for path in [
        "/World/Guide",
        "/World/Guide/Shape",
        "/World/Guide/IgnoredAuthored",
        "/World/Guide/IgnoredAuthored/Inherited",
    ] {
        assert_eq!(session.prim_purpose(path), RenderPurpose::Render, "{path}");
    }
    assert_eq!(
        session.prim_purpose("/World/Guide/Reset"),
        RenderPurpose::Default
    );
    let snapshot = session.last_snapshot.as_ref().unwrap();
    assert_ne!(snapshot.serial, before);
    assert_eq!(paths(snapshot, false), default_paths());
    assert!(paths(snapshot, true).is_empty());
    // The previous guide and both explicit-purpose foreground prims are now
    // excluded, leaving the default-purpose cube behind them pickable.
    assert_eq!(
        pick(&mut session, 0.0).as_deref(),
        Some("/World/AuthoredDefault")
    );

    session
        .author_attribute(
            "/World/Guide/IgnoredAuthored",
            "purpose",
            idtx_usd::PropertyValue::Token("proxy".into()),
        )
        .unwrap();
    session.tick().unwrap();
    assert_eq!(
        session.prim_purpose("/World/Guide/IgnoredAuthored"),
        RenderPurpose::Render
    );
    assert_eq!(
        session.prim_purpose("/World/Guide/IgnoredAuthored/Inherited"),
        RenderPurpose::Render
    );
}

#[test]
#[cfg(feature = "authoring")]
#[ignore = "requires OpenUSD and GPU"]
fn purpose_edits_to_shared_sources_refresh_native_instance_rendering_and_picking() {
    let (mut session, _dir) = open_source(
        r#"#usda 1.0
(
    metersPerUnit = 1
    upAxis = "Y"
)
def Xform "Asset" {
    double3 xformOp:translate = (8, 0, 0)
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
    double3 xformOp:translate = (0, 0, 0)
}
"#,
    );
    session.set_viewport_purpose(RenderPurpose::Proxy);
    session.render_one_frame(false);
    assert_eq!(pick(&mut session, 0.0).as_deref(), Some("/Native"));

    session
        .author_attribute(
            "/Asset/Child",
            "purpose",
            idtx_usd::PropertyValue::Token("guide".into()),
        )
        .unwrap();
    session.tick().unwrap();
    session.render_one_frame(false);
    assert!(pick(&mut session, 0.0).is_none());
    assert!(!paths(session.last_snapshot.as_ref().unwrap(), false).contains("/Native"));

    session.set_viewport_purpose(RenderPurpose::Guide);
    session.render_one_frame(false);
    assert!(paths(session.last_snapshot.as_ref().unwrap(), true).contains("/Native"));
    assert_eq!(pick(&mut session, 0.0).as_deref(), Some("/Native"));
}
