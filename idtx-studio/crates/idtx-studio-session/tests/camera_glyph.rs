//! Editor camera overlays: multi-camera visibility, navigation, and GPU output.
//! Run with features editor-overlay,metrics to enable viewport pixel readback.
#![cfg(all(feature = "editor-overlay", feature = "metrics"))]

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn camera_glyphs_follow_editor_visibility_and_render_in_rose() {
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("cameras.usda");
    std::fs::write(
        &stage,
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Z"
    metersPerUnit = 0.01
)
def Xform "World" {
    def Camera "A" {
        float focalLength = 35
        float2 clippingRange = (10, 500)
    }
    def Xform "Rig" {
        double3 xformOp:translate = (300, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Camera "B" {
            float focalLength = 75
        }
    }
    def Xform "Hidden" {
        token visibility = "invisible"
        def Camera "Camera" {
            double3 xformOp:translate = (90000, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def Camera "Guide" {
        uniform token purpose = "guide"
        double3 xformOp:translate = (1000, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#,
    )
    .unwrap();
    let (device, queue) = pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .expect("headless adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("headless device")
    });
    let mut session = StudioSession::open_with_gpu(
        stage,
        device,
        queue,
        [640, 480],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .unwrap();
    session.initialize_viewport_camera();
    session.use_orbit_camera();
    session.set_viewport_purpose(idtx_usd::RenderPurpose::Render);
    let compact = session.camera_glyph_lines();
    assert!(!compact.is_empty(), "unselected cameras must be visible");
    assert!(compact.iter().all(|l| l.color == idtx_render::CAMERA_COLOR));
    let points: Vec<_> = compact.iter().flat_map(|l| [l.a, l.b]).collect();
    assert!(
        points.iter().all(|p| p[0] > -2.0 && p[0] < 5.0),
        "hidden/guide cameras excluded; x range {:?}",
        points
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), p| (
                min.min(p[0]),
                max.max(p[0])
            ))
    );
    let b_origin = session
        .projection_basis()
        .transform_point3(glam::vec3(300.0, 0.0, 0.0));
    assert!(
        points
            .iter()
            .any(|p| glam::Vec3::from(*p).abs_diff_eq(b_origin, 1e-5)),
        "parent transform and stage units applied"
    );

    // Exercise the actual common desktop render path, not just the line builder.
    session.render_one_frame(false);
    let pixels = session.read_pixels().unwrap();
    let rose_pixels = |pixels: &[u8]| {
        pixels
            .chunks_exact(4)
            .filter(|p| p[0] > 245 && (175..=200).contains(&p[1]) && (195..=215).contains(&p[2]))
            .count()
    };
    assert!(
        rose_pixels(&pixels) > 30,
        "camera wireframe must reach the frame buffer"
    );
    if let Some(path) = std::env::var_os("IDTX_CAMERA_GLYPH_CAPTURE") {
        image::save_buffer(path, &pixels, 640, 480, image::ColorType::Rgba8).unwrap();
    }
    session.set_selected(Some("/World/A".into()));
    let selected = session.camera_glyph_lines();
    assert_eq!(
        selected.len(),
        compact.len() + 12,
        "selection adds clipping planes and edges"
    );

    // Hierarchical visibility and looking through a camera suppress its glyph.
    session.toggle_visibility("/World/Rig");
    session.use_scene_camera("/World/A");
    assert!(session.camera_glyph_lines().is_empty());
    session.set_viewport_purpose(idtx_usd::RenderPurpose::Guide);
    assert!(
        !session.camera_glyph_lines().is_empty(),
        "guide filter admits the guide camera"
    );
    session.set_editor_overlays_enabled(false);
    assert!(
        session.camera_glyph_lines().is_empty(),
        "preview has no camera overlays"
    );

    session.use_orbit_camera();
    session.set_selected(None);
    session.render_one_frame(false);
    assert_eq!(rose_pixels(&session.read_pixels().unwrap()), 0);
}
