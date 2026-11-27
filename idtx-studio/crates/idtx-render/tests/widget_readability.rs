//! Readability probe renders for the datacenter asset-overview widget
//! (`tests/fixtures/usd/widgets/widget_test_panel.usda`, referenced into
//! `widget_test_scene.usda` in front of a background cube).
//!
//! Shoots the SAME panel from three camera distances so that text legibility
//! can be compared across rasterizer iterations:
//!   * `close` — panel ≈ full frame height
//!   * `mid`   — panel ≈ 50% of frame height
//!   * `far`   — panel ≈ 25% of frame height
//!
//! Output PNGs are written to `tests/renders/widgets/readability/` with a
//! stage label so successive iterations do not overwrite each other. The label
//! comes from `IDTX_WIDGET_STAGE` (default `base`):
//!
//! ```bash
//! export IDTX_USD_DIR=.../openusd-26.11.1/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib:$DYLD_LIBRARY_PATH
//! IDTX_WIDGET_STAGE=base cargo test -p idtx-render --no-default-features \
//!     --features materialx-naga --test widget_readability -- --ignored --nocapture
//! ```

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments,
    clippy::unnecessary_debug_formatting,
    deprecated
)]

use std::path::PathBuf;

use idtx_render::{RenderError, RenderWorldSnapshot, Renderer};
use idtx_scene::{PopulatorRegistry, SceneHydrator, widget::WidgetPopulator};
use idtx_usd::StageService;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn renders_subdir() -> PathBuf {
    let dir = workspace_root().join("tests/renders/widgets/readability");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn stage_label() -> String {
    std::env::var("IDTX_WIDGET_STAGE").unwrap_or_else(|_| "base".to_string())
}

fn save_png(path: &PathBuf, rgba: &[u8], width: u32, height: u32) {
    image::save_buffer(path, rgba, width, height, image::ColorType::Rgba8)
        .unwrap_or_else(|e| eprintln!("PNG save failed {path:?}: {e}"));
}

/// MSAA sample count forced for every probe. The VR present path runs at 2×,
/// so the readability renders are shot at 2× (not the desktop default 4×) to
/// mirror what text actually gets on-device.
const PROBE_MSAA: u32 = 2;

fn try_renderer() -> Option<Renderer> {
    match Renderer::new_with_sample_count(PROBE_MSAA) {
        Ok(r) => Some(r),
        Err(RenderError::NoAdapter) => {
            eprintln!("Skipping GPU render test — no wgpu adapter found");
            None
        }
        Err(e) => {
            eprintln!("Skipping GPU render test — {e}");
            None
        }
    }
}

fn hydrate_and_extract(fixture: &str, aspect: f32) -> RenderWorldSnapshot {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let path = workspace_root().join(fixture);
    assert!(path.exists(), "fixture missing: {}", path.display());
    let mut service = StageService::open(&path).expect("open widget fixture");

    let delta = service.project_topology_paths(&[]).expect("project topology");
    let mut registry = PopulatorRegistry::new();
    registry.register(std::sync::Arc::new(WidgetPopulator));
    let mut hydrator = SceneHydrator::new(registry);
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator.apply_viewport_projection(&vp).expect("apply viewport");
    let n = hydrator.hydrate_widgets(&service);
    println!("hydrate_widgets stamped {n} component(s)");

    let world = hydrator.world_mut();
    RenderWorldSnapshot::extract(world, aspect, glam::Mat4::IDENTITY)
}

/// Vertical FOV used for every probe (matches the other widget render tests).
const FOVY: f32 = std::f32::consts::FRAC_PI_3;
/// Authored panel height in world units (`idtx:widget:size.y` in the fixture).
const PANEL_HEIGHT: f32 = 1.14;

/// Camera distance along +Z at which a `PANEL_HEIGHT`-tall panel occupies
/// `fraction` of the frame height for vertical FOV `FOVY`.
fn distance_for_fraction(fraction: f32) -> f32 {
    PANEL_HEIGHT / (2.0 * fraction * (FOVY * 0.5).tan())
}

fn probe(
    renderer: &mut Renderer,
    snap: &RenderWorldSnapshot,
    w: u32,
    h: u32,
    name: &str,
    fraction: f32,
    azimuth_deg: f32,
    elevation_deg: f32,
) {
    let aspect = w as f32 / h as f32;
    let d = distance_for_fraction(fraction);
    // Orbit the panel centre: +azimuth swings the camera to the panel's right,
    // +elevation lifts it up. Both are 0 for the straight-on front probes.
    let az = azimuth_deg.to_radians();
    let el = elevation_deg.to_radians();
    let eye = glam::Vec3::new(
        d * az.sin() * el.cos(),
        d * el.sin(),
        d * az.cos() * el.cos(),
    );
    let center = glam::Vec3::ZERO;

    let mut snap_cam = snap.clone();
    snap_cam.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: format!("/widget/readability/{name}"),
        view: glam::Mat4::look_at_rh(eye, center, glam::Vec3::Y),
        projection: glam::Mat4::perspective_rh(FOVY, aspect, 0.01, 100.0),
        clip_near: 0.01,
        clip_far: 100.0,
        fov_horizontal: 2.0 * ((FOVY * 0.5).tan() * aspect).atan(),
        exposure_ev: 0.0,
    });

    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let out = renders_subdir().join(format!("{}_{name}.png", stage_label()));
    save_png(&out, &pixels, w, h);
    println!(
        "Saved probe {name} (panel ≈ {:.0}% frame height, az={azimuth_deg}° el={elevation_deg}°, eye={eye:?}): {out:?}",
        fraction * 100.0
    );
}

/// Render the datacenter widget at three distances, straight-on and from a
/// 30°-right / 15°-up angle, into
/// `tests/renders/widgets/readability/<stage>_{close,mid,far}[_angled].png`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_readability_probes() {
    // Portrait-friendly frame: the card is tall, so height is the limiting
    // dimension the probe fractions are expressed against.
    let (w, h) = (1280u32, 960u32);
    let aspect = w as f32 / h as f32;

    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_test_scene.usda", aspect);
    assert_eq!(snap.widget_panels.len(), 1, "expected exactly one widget panel");
    let panel = &snap.widget_panels[0];
    assert!(!panel.elements.is_empty(), "panel should carry its element tree");
    println!(
        "panel {} resolution={:?} size={:?} elements={} (MSAA {PROBE_MSAA}×)",
        panel.prim_path,
        panel.resolution,
        panel.size,
        panel.elements.len()
    );

    let Some(mut renderer) = try_renderer() else {
        return;
    };

    // Straight-on.
    probe(&mut renderer, &snap, w, h, "close", 0.92, 0.0, 0.0);
    probe(&mut renderer, &snap, w, h, "mid", 0.50, 0.0, 0.0);
    probe(&mut renderer, &snap, w, h, "far", 0.25, 0.0, 0.0);

    // 30° to the right, 15° up.
    probe(&mut renderer, &snap, w, h, "close_angled", 0.92, 30.0, 15.0);
    probe(&mut renderer, &snap, w, h, "mid_angled", 0.50, 30.0, 15.0);
    probe(&mut renderer, &snap, w, h, "far_angled", 0.25, 30.0, 15.0);
}
