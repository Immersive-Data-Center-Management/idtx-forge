//! Regression: a guest `attach_crosshair` (→ `SessionEditOp::AttachHud`) must
//! surface a hydrated `HudComp` so the screen-space HUD pass renders it.
//!
//! The HUD child is authored into the session layer via
//! `StageService::define_session_prim`, which drains its own TOPOLOGY notice
//! internally to mint the `PrimId`. That left the trailing `tick()` in
//! `apply_session_edits` with no delta, so the hydrator never spawned an entity
//! for the new `IdtxHud` prim and `hydrate_hud` (which scans existing entities)
//! found nothing — the crosshair was authored but never reached the render
//! snapshot (behavior-driven HUD silently invisible, while a HUD prim authored
//! directly in the root layer rendered fine). `apply_session_edits` now
//! re-projects the camera subtree after an `AttachHud` and hydrates the HUD.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless
//! wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored behavior_hud
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(name)
}

/// Surfaceless wgpu device/queue for headless rendering.
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
                label: Some("idtx-behavior-hud-test"),
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

const CAMERA: &str = "/World/Camera";
const CROSSHAIR: &str = "/World/Camera/Crosshair";

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn guest_attach_crosshair_hydrates_a_hud_component() {
    // Register the IDTX schemas so the `IdtxHud` type resolves on define.
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("behavior/idtx_hud_camera.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open hud camera fixture");

    // No HUD yet.
    assert_eq!(
        session.hud_image_for_test(CROSSHAIR),
        None,
        "no HudComp before the attach"
    );

    // Exactly what a camera behavior's `on_start` posts via `attach_crosshair`.
    // `[0, 0]` = native image size. No asset search path is installed, so the
    // image resolver yields nothing and the raw authored path is kept — which is
    // all this test asserts (resolution is covered elsewhere); the point is that
    // a `HudComp` *exists* and carries the image, i.e. the element reaches the
    // render snapshot.
    session.apply_guest_hud_attach_for_test(CAMERA, "Crosshair", "images/crosshair.png", [0, 0], 1.0);

    assert_eq!(
        session.hud_image_for_test(CROSSHAIR),
        Some("images/crosshair.png".to_owned()),
        "AttachHud must hydrate a HudComp on the new session-layer IdtxHud prim \
         — define_session_prim drains its own topology notice, so apply_session_edits \
         must re-project the camera subtree and hydrate the HUD explicitly"
    );

    // The camera itself must NOT gain a HudComp (only its child does).
    assert_eq!(
        session.hud_image_for_test(CAMERA),
        None,
        "the HUD lives on the child prim, not the camera"
    );
}
