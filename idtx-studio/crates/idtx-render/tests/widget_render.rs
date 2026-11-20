//! Headless render integration tests for the 2D-widgets-in-USD feature
//! (docs/widgets-in-usd-proposal-v2.md, Phases 2–3). These open a widget USD
//! fixture, hydrate it (incl. `hydrate_widgets`), extract a
//! [`RenderWorldSnapshot`], render it to an offscreen target, read the pixels
//! back, and save a PNG under `tests/renders/widgets/` for self-inspection.
//!
//! Requires a linked `OpenUSD` (`IDTX_USD_DIR` + `DYLD_LIBRARY_PATH` on macOS)
//! and a wgpu adapter, so they are `#[ignore]` and run explicitly:
//!
//! ```bash
//! export IDTX_USD_DIR=.../openusd-26.08-mtlx/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib:$DYLD_LIBRARY_PATH
//! cargo test -p idtx-render --test widget_render -- --ignored --nocapture
//! ```

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    deprecated
)]

use std::path::PathBuf;

use idtx_render::{camera_fit::fit_camera_to_snapshot, RenderError, RenderWorldSnapshot, Renderer};
use idtx_scene::{PopulatorRegistry, SceneHydrator, widget::WidgetPopulator};
use idtx_usd::StageService;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn renders_subdir() -> PathBuf {
    let dir = workspace_root().join("tests/renders/widgets");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn save_png(path: &PathBuf, rgba: &[u8], width: u32, height: u32) {
    image::save_buffer(path, rgba, width, height, image::ColorType::Rgba8)
        .unwrap_or_else(|e| eprintln!("PNG save failed {path:?}: {e}"));
}

fn try_renderer() -> Option<Renderer> {
    match Renderer::new() {
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

/// Open a widget fixture, register the schemas, hydrate it (topology → viewport
/// → `hydrate_widgets`), and extract a snapshot.
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
    // Stamp WidgetPanelComp from the IdtxWidgetPanel prims (the Phase-2 drain-time hydrate).
    let n = hydrator.hydrate_widgets(&service);
    println!("hydrate_widgets stamped {n} component(s)");

    let world = hydrator.world_mut();
    RenderWorldSnapshot::extract(world, aspect, glam::Mat4::IDENTITY)
}

/// The headline deliverable: a floating `IdtxWidgetPanel` rendered as a
/// depth-tested quad among ordinary 3D gprims (floor + cube + cylinder +
/// sphere), saved to a PNG for inspection.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_in_scene_renders_to_png() {
    let (w, h) = (1280u32, 720u32);
    let aspect = w as f32 / h as f32;
    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_in_scene.usda", aspect);

    // Extraction: one widget panel + the four 3D gprims (floor, box, pillar, ball).
    assert_eq!(
        snap.widget_panels.len(),
        1,
        "expected exactly one widget panel, got {}",
        snap.widget_panels.len()
    );
    assert_eq!(
        snap.meshes.len(),
        4,
        "expected 4 meshes (floor/cube/cylinder/sphere), got {}",
        snap.meshes.len()
    );
    let panel = &snap.widget_panels[0];
    assert_eq!(panel.prim_path, "/World/InfoPanel");
    assert!(!panel.elements.is_empty(), "panel should carry its element tree");

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let mut snap_cam = snap;
    snap_cam.camera = Some(fit_camera_to_snapshot(&snap_cam, aspect));
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let out = renders_subdir().join("widget_in_scene.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(
        non_bg > (w * h / 50) as usize,
        "fewer than 2% non-background pixels: {non_bg}"
    );
}

/// A world-fixed (`billboard = none`) panel mounted on a cube's +Z face: it
/// keeps its scene orientation (does NOT turn to face the camera), so from a
/// 3/4 orbit it is seen at an angle on the cube face. Renders to a PNG.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_on_cube_face_renders_to_png() {
    use idtx_scene::WidgetBillboard;

    let (w, h) = (1280u32, 720u32);
    let aspect = w as f32 / h as f32;
    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_on_cube_face.usda", aspect);

    assert_eq!(snap.widget_panels.len(), 1, "expected one wall panel");
    let panel = &snap.widget_panels[0];
    assert_eq!(panel.prim_path, "/World/WallPanel");
    assert_eq!(
        panel.billboard,
        WidgetBillboard::None,
        "the wall panel must have a FIXED orientation (billboard = none)"
    );
    // Floor + cube pedestal.
    assert_eq!(snap.meshes.len(), 2, "expected floor + cube, got {}", snap.meshes.len());

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let mut snap_cam = snap;
    snap_cam.camera = Some(fit_camera_to_snapshot(&snap_cam, aspect));
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir().join("widget_on_cube_face.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(
        non_bg > (w * h / 50) as usize,
        "fewer than 2% non-background pixels: {non_bg}"
    );
}

/// The SAME widget stage referenced twice into one scene: `/World/FacePanel`
/// (fixed on a cube face) + `/World/TipPanel` (billboarded at a cone apex).
/// Proves a reusable library widget references + composes, with per-instance
/// billboard/size/transform overrides, and renders both in one frame.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_referenced_twice_renders_to_png() {
    use idtx_scene::WidgetBillboard;

    let (w, h) = (1280u32, 720u32);
    let aspect = w as f32 / h as f32;
    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_referenced_twice.usda", aspect);

    assert_eq!(
        snap.widget_panels.len(),
        2,
        "expected the widget stage referenced twice, got {}",
        snap.widget_panels.len()
    );
    let face = snap
        .widget_panels
        .iter()
        .find(|p| p.prim_path == "/World/FacePanel")
        .expect("FacePanel");
    let tip = snap
        .widget_panels
        .iter()
        .find(|p| p.prim_path == "/World/TipPanel")
        .expect("TipPanel");
    assert_eq!(face.billboard, WidgetBillboard::None, "FacePanel is fixed");
    assert_eq!(
        tip.billboard,
        WidgetBillboard::ScreenAligned,
        "TipPanel follows the camera"
    );
    // Both inherit the referenced element tree (Container + Button + Label).
    assert!(!face.elements.is_empty() && !tip.elements.is_empty());
    // Floor + cube + cone.
    assert_eq!(snap.meshes.len(), 3, "expected floor + cube + cone, got {}", snap.meshes.len());

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let mut snap_cam = snap;
    snap_cam.camera = Some(fit_camera_to_snapshot(&snap_cam, aspect));
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir().join("widget_referenced_twice.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(non_bg > (w * h / 50) as usize, "scene should fill the frame");
}

/// Phase-4 iter-1 text validation: render the cube-face panel from CLOSE UP
/// (hand camera framing just the panel) so the embedded fixed-font text
/// ("Panel A" label + "Start" button) is legible for inspection. Same font
/// path as the runtime (idtx-render CPU rasterizer) — no egui.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_text_closeup_renders_to_png() {
    let (w, h) = (1024u32, 768u32);
    let aspect = w as f32 / h as f32;
    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_on_cube_face.usda", aspect);
    assert_eq!(snap.widget_panels.len(), 1);

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    // Close, straight-on camera aimed at the panel centre (0, 0.75, 0.76).
    let eye = glam::Vec3::new(0.0, 0.75, 1.9);
    let target_pt = glam::Vec3::new(0.0, 0.75, 0.76);
    let mut snap_cam = snap;
    snap_cam.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: "/widget/closeup".into(),
        view: glam::Mat4::look_at_rh(eye, target_pt, glam::Vec3::Y),
        projection: glam::Mat4::perspective_rh(std::f32::consts::FRAC_PI_3, aspect, 0.01, 100.0),
        clip_near: 0.01,
        clip_far: 100.0,
        fov_horizontal: std::f32::consts::FRAC_PI_3,
        exposure_ev: 0.0,
    });
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir().join("widget_text_closeup.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(non_bg > (w * h / 20) as usize, "panel should fill the close-up frame");
}

/// Positioned-layout fixture (`IdtxWidgetElement.position`): rows authored as
/// separate caption-LABEL + value-FIELD prims placed by `idtx:widget:position`
/// instead of namespace-order stacking. Asserts the hydrator carried each
/// element's authored normalized position, then renders to a PNG.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_positioned_elements_render_to_png() {
    let (w, h) = (768u32, 576u32);
    let aspect = w as f32 / h as f32;
    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_positioned.usda", aspect);

    assert_eq!(snap.widget_panels.len(), 1, "expected the positioned panel");
    let panel = &snap.widget_panels[0];
    assert_eq!(panel.prim_path, "/Panel");
    // Two caption labels + two value fields, all absolutely positioned.
    assert_eq!(panel.elements.len(), 4, "expected 4 positioned elements, got {:?}", panel.elements);
    assert!(
        panel.elements.iter().all(|e| e.position.is_some()),
        "every element must carry an authored idtx:widget:position"
    );
    // The two rows sit at distinct normalized Y's (0.2 and 0.6).
    let mut ys: Vec<i32> = panel
        .elements
        .iter()
        .filter_map(|e| e.position.map(|p| (p[1] * 100.0) as i32))
        .collect();
    ys.sort_unstable();
    ys.dedup();
    assert_eq!(ys, vec![20, 60], "elements must land on the two authored rows");

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let eye = glam::Vec3::new(0.0, 0.0, 1.0);
    let mut snap_cam = snap;
    snap_cam.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: "/widget/cam".into(),
        view: glam::Mat4::look_at_rh(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        projection: glam::Mat4::perspective_rh(std::f32::consts::FRAC_PI_3, aspect, 0.01, 100.0),
        clip_near: 0.01,
        clip_far: 100.0,
        fov_horizontal: std::f32::consts::FRAC_PI_3,
        exposure_ev: 0.0,
    });
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir().join("widget_positioned.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(non_bg > 0, "positioned widget canvas produced no non-background pixels");
}

/// The widget-stage fixture (panel as stage root) extracts its panel + element
/// tree and renders front-on to a PNG — the 2D authoring-canvas preview.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH) and a wgpu adapter"]
fn widget_panel_stage_extracts_and_renders() {
    let (w, h) = (768u32, 576u32);
    let aspect = w as f32 / h as f32;
    let snap = hydrate_and_extract("tests/fixtures/usd/widgets/widget_panel_stage.usda", aspect);

    assert_eq!(snap.widget_panels.len(), 1, "expected the root panel");
    let panel = &snap.widget_panels[0];
    assert_eq!(panel.prim_path, "/Panel");
    // Container + Label + Button flatten to three elements.
    assert_eq!(
        panel.elements.len(),
        3,
        "expected Container + Label + Button, got {:?}",
        panel.elements
    );

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    // Hand-built front-on camera looking down -Z at the panel (no scene bounds).
    let eye = glam::Vec3::new(0.0, 0.0, 1.0);
    let mut snap_cam = snap;
    snap_cam.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: "/widget/cam".into(),
        view: glam::Mat4::look_at_rh(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        projection: glam::Mat4::perspective_rh(std::f32::consts::FRAC_PI_3, aspect, 0.01, 100.0),
        clip_near: 0.01,
        clip_far: 100.0,
        fov_horizontal: std::f32::consts::FRAC_PI_3,
        exposure_ev: 0.0,
    });
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir().join("widget_panel_stage.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(non_bg > 0, "widget canvas produced no non-background pixels");
}
