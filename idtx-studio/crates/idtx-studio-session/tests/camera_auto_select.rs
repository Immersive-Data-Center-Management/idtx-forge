//! Verifies the editor's automatic viewport-camera selection on stage open:
//!
//!   1. A stage with an authored `UsdGeomCamera` opens rendering **through**
//!      that camera (so the viewport matches an offline render of the stage).
//!   2. A stage with **no** camera falls back to the synthetic orbit camera.
//!   3. Starting to navigate (orbit) hands off from the scene camera to the
//!      free orbit camera.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith (needs `IDTX_USD_DIR` + the
//! runtime libs on the loader path) and spins up a headless wgpu device:
//!
//! ```bash
//! IDTX_USD_DIR=… cargo test -p idtx-studio-session -- --ignored camera_auto_select
//! ```

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

/// A path under `tests/fixtures/usd/` (forward-slash subpaths accepted).
fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(rel)
}

fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("a headless wgpu adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-camera-auto-select-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("a headless wgpu device")
    })
}

fn open(rel: &str) -> StudioSession {
    let (device, queue) = headless_device();
    StudioSession::open_with_gpu(
        fixture(rel),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .unwrap_or_else(|e| panic!("open {rel}: {e}"))
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn stage_with_camera_opens_through_it() {
    // distant.usda sublayers _calibration.usda, which authors /World/Camera.
    let mut session = open("lighting/types/distant.usda");
    session.initialize_viewport_camera();
    session.render_one_frame(false);

    let cam = session
        .last_render_camera()
        .expect("a camera was injected into the frame");
    assert_eq!(
        cam.prim_path, "/World/Camera",
        "editor should render through the authored stage camera on open, got {:?}",
        cam.prim_path
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn stage_without_camera_falls_back_to_orbit() {
    // basic-scene.usda has geometry but no camera.
    let mut session = open("hydration/basic-scene.usda");
    session.initialize_viewport_camera();
    session.render_one_frame(false);

    let cam = session
        .last_render_camera()
        .expect("a camera was injected into the frame");
    assert_eq!(
        cam.prim_path, "__orbit__",
        "a camera-less stage should fall back to the synthetic orbit camera, got {:?}",
        cam.prim_path
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn navigating_hands_off_from_scene_camera_to_orbit() {
    let mut session = open("lighting/types/distant.usda");
    session.initialize_viewport_camera();
    session.render_one_frame(false);
    assert_eq!(
        session.last_render_camera().map(|c| c.prim_path.as_str()),
        Some("/World/Camera"),
        "precondition: opened through the stage camera"
    );

    // Starting to orbit grabs control: the viewport switches to the free orbit
    // camera (seeded from the scene camera's view).
    session.orbit_camera(0.2, 0.0);
    session.render_one_frame(false);
    assert_eq!(
        session.last_render_camera().map(|c| c.prim_path.as_str()),
        Some("__orbit__"),
        "navigating should hand off to the orbit camera"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn selected_camera_can_be_reentered_after_navigation_without_authoring() {
    let mut session = open("hydration/viewport-navigation.usda");
    session.initialize_viewport_camera();
    session.render_one_frame(false);
    assert_eq!(
        session.last_render_camera().unwrap().prim_path,
        "/World/CameraA"
    );
    let before = session.inspect_prim("/World/CameraB").unwrap();
    let generation = session.change_generation();
    let mut authored_view = None;
    for navigation in 0..4 {
        session.use_scene_camera("/World/CameraB");
        session.render_one_frame(false);
        let camera = session.last_render_camera().unwrap();
        assert_eq!(camera.prim_path, "/World/CameraB");
        if let Some(view) = authored_view {
            assert_eq!(camera.view, view);
        }
        authored_view = Some(camera.view);
        let expected_eye = session
            .projection_basis()
            .transform_point3(glam::vec3(500.0, 600.0, 900.0));
        let (view_proj, eye) = session.camera_view_proj();
        assert!((eye - expected_eye).length() < 1e-4);
        let camera = session.last_render_camera().unwrap();
        assert_eq!(view_proj, camera.projection * camera.view);
        match navigation {
            0 => session.orbit_camera(0.2, 0.1),
            1 => session.pan_camera(20.0, 10.0),
            2 => session.dolly_camera(0.8),
            _ => {
                session.set_selected(Some("/World/Selection/Left".into()));
                session.frame_selected();
            }
        }
        session.render_one_frame(false);
        assert_eq!(session.last_render_camera().unwrap().prim_path, "__orbit__");
    }
    session.tick().unwrap();
    let after = session.inspect_prim("/World/CameraB").unwrap();
    let values = |properties: &[idtx_usd::PropertyRecord]| {
        properties
            .iter()
            .map(|p| (p.name.clone(), p.authored, p.value_summary.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(values(&before), values(&after));
    assert_eq!(
        session.change_generation(),
        generation,
        "navigation must not edit any layer"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn frame_selected_fits_visible_subtree_in_stage_basis_and_keeps_projection() {
    use idtx_studio_session::{ProjectionMode, ViewPreset};
    let mut session = open("hydration/viewport-navigation.usda");
    session.initialize_viewport_camera();
    let initial = session.camera_view_proj();
    session.frame_selected();
    assert_eq!(
        session.camera_view_proj(),
        initial,
        "no selection is a no-op"
    );
    session.set_viewport_purpose(idtx_usd::RenderPurpose::Render);
    session.resize_viewport([80, 160]);
    session.set_selected(Some("/World/Selection".into()));
    let basis = session.projection_basis();
    for orthographic in [true, false] {
        session.set_view_preset(ViewPreset::Front);
        if !orthographic {
            session.toggle_projection();
        }
        session.frame_selected();
        assert_eq!(
            session.projection_mode(),
            if orthographic {
                ProjectionMode::Orthographic
            } else {
                ProjectionMode::Perspective
            }
        );
        let (view_proj, _) = session.camera_view_proj();
        let center =
            view_proj.project_point3(basis.transform_point3(glam::vec3(100.0, 200.0, 300.0)));
        assert!(
            center.x.abs() < 1e-4 && center.y.abs() < 1e-4,
            "selected subtree is centered: {center:?}"
        );
        assert!(
            session.camera_radius() < 20.0,
            "hidden, guide and similarly named siblings must not enlarge the framing"
        );
        for x in [-150.0, 350.0] {
            for y in [150.0, 250.0] {
                for z in [250.0, 350.0] {
                    let point =
                        view_proj.project_point3(basis.transform_point3(glam::vec3(x, y, z)));
                    assert!(
                        point.x.abs() <= 1.0
                            && point.y.abs() <= 1.0
                            && (0.0..=1.0).contains(&point.z),
                        "selection corner outside viewport: {point:?}"
                    );
                }
            }
        }
    }
}
