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
