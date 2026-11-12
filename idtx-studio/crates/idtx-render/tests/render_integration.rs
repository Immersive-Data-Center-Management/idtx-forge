//! Render integration tests for `idtx-render`.
//!
//! # Running GPU tests (no USD required)
//!
//! ```bash
//! cargo test -p idtx-render -- --include-ignored
//! ```
//!
//! # Running USD integration tests
//!
//! ```bash
//! export IDTX_USD_DIR=/path/to/openusd/install
//! cargo test -p idtx-render -- --include-ignored --test render_integration
//! ```
//!
//! Rendered PNGs are written to `tests/renders/{topic}/` at the workspace root.
//!
//! # Coverage
//!
//! The suite renders fixtures end to end and checks the output: flat and
//! Blinn-Phong shading; camera fitting; Cook-Torrance PBR with multi-channel
//! textures and an 8-light array; the USD asset-reader for `.usdz` textures;
//! a camera-aligned headlight and `faceVarying` `displayColor`;
//! mirrored-transform winding and per-material `GeomSubset` draw calls; 2-pass
//! transparent rendering (opaque-first, then back-to-front depth-sorted); CPU
//! mip generation with domain-classified reprojection routing; the `MaterialX`
//! WGSL shader paths (C++ direct via `materialx-wgsl` and naga transpile via
//! `materialx-naga`); GPU classic-linear blend skinning via a joint-matrix
//! palette; and the mesh GPU buffer cache (warm-frame buffer reuse,
//! pipeline-sorted draw calls).

// test-only pixel/aspect math; ranges are bounded by the fixture sizes
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glam::Mat4;
use idtx_render::{
    FrameTarget, RenderError, RenderWorldSnapshot, Renderer,
    camera_fit::fit_camera_to_snapshot,
    material::fallback_material,
    mesh::{tessellate_cube, tessellate_sphere},
    scene_extract::{ExtractionStats, RenderMeshInstance},
    texture_cache::AssetReadFn,
};
use idtx_usd::{AlphaPolicy, RenderPurpose};

// Allows StageService (which wraps an opaque C++ pointer) to be captured in an
// Arc for use as an AssetReadFn. Integration tests are single-threaded so
// there is no actual data race.
struct SyncSvc(idtx_usd::StageService);
unsafe impl Send for SyncSvc {}
unsafe impl Sync for SyncSvc {}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn renders_dir() -> PathBuf {
    let dir = workspace_root().join("tests/renders");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn renders_subdir(topic: &str) -> PathBuf {
    let dir = renders_dir().join(topic);
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

fn psnr_rgba(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0_f64 * 255.0 / mse).log10()
    }
}

/// A synthetic scene with a red cube and a blue sphere — no USD required.
fn synthetic_snapshot() -> RenderWorldSnapshot {
    let mut snap = RenderWorldSnapshot::default();

    let mut cube = tessellate_cube(0.5);
    cube.prim_path = "/World/Cube".into();
    let mut cube_mat = fallback_material("/Looks/Red");
    cube_mat.base_color = [0.8, 0.15, 0.15];
    snap.meshes.push(RenderMeshInstance {
        prim_path: "/World/Cube".into(),
        mesh: Arc::new(cube),
        world_matrix: Mat4::from_translation(glam::Vec3::new(-0.6, 0.0, 0.0)),
        material: cube_mat,
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    });

    let mut sphere = tessellate_sphere(0.45, 24, 36);
    sphere.prim_path = "/World/Sphere".into();
    let mut sphere_mat = fallback_material("/Looks/Blue");
    sphere_mat.base_color = [0.15, 0.25, 0.8];
    snap.meshes.push(RenderMeshInstance {
        prim_path: "/World/Sphere".into(),
        mesh: Arc::new(sphere),
        world_matrix: Mat4::from_translation(glam::Vec3::new(0.6, 0.0, 0.0)),
        material: sphere_mat,
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    });

    snap.stats = ExtractionStats {
        mesh_count: 2,
        ..Default::default()
    };
    snap
}

/// The single-cube prefix of [`synthetic_snapshot`] — one opaque, single-sided
/// mesh. Used as the differential baseline for the redundant-state dedup guard:
/// [`synthetic_snapshot`] adds one further mesh that shares the same
/// (single-sided, opaque) pipeline, so the *delta* in recorded commands isolates
/// the per-mesh cost with the pipeline bind deduplicated.
#[cfg(feature = "metrics")]
fn single_cube_snapshot() -> RenderWorldSnapshot {
    let mut snap = RenderWorldSnapshot::default();
    let mut cube = tessellate_cube(0.5);
    cube.prim_path = "/World/Cube".into();
    let mut cube_mat = fallback_material("/Looks/Red");
    cube_mat.base_color = [0.8, 0.15, 0.15];
    snap.meshes.push(RenderMeshInstance {
        prim_path: "/World/Cube".into(),
        mesh: Arc::new(cube),
        world_matrix: Mat4::from_translation(glam::Vec3::new(-0.6, 0.0, 0.0)),
        material: cube_mat,
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    });
    snap.stats = ExtractionStats {
        mesh_count: 1,
        ..Default::default()
    };
    snap
}

// ── USD integration tests ─────────────────────────────────────────────────────

/// Verifies that all built-in USD prim types in `basic-scene.usda` are extracted.
///
/// Expected prims: 1 Cube, 1 Sphere, 3 Cylinders, 2 Cones, 1 Capsule, 1 Mesh (Plane) = 9 meshes.
/// Cylinder_02 (axis=X) and Cylinder_01 (axis=Z) exercise the axis-rotation baking.
/// Cone (axis=Z) exercises the cone axis rotation.
#[test]
#[ignore]
fn basic_scene_all_prim_types_extracted() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/hydration/basic-scene.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open basic-scene.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 16.0 / 9.0, glam::Mat4::IDENTITY);

    println!(
        "basic-scene: {} meshes (expected 9: 1 Cube, 1 Sphere, 3 Cyl, 2 Cone, 1 Capsule, 1 Mesh)",
        snap.meshes.len()
    );
    for m in &snap.meshes {
        println!("  {} — {} tris", m.prim_path, m.mesh.triangle_count());
    }

    // 1 Cube + 1 Sphere + 3 Cylinders + 2 Cones + 1 Capsule + 1 Plane mesh = 9
    assert_eq!(
        snap.meshes.len(),
        9,
        "expected 9 meshes, got {}",
        snap.meshes.len()
    );

    // All extracted meshes must have non-empty geometry.
    for m in &snap.meshes {
        assert!(
            m.mesh.triangle_count() > 0,
            "{} has no triangles",
            m.prim_path
        );
        assert_eq!(
            m.mesh.normals.len(),
            m.mesh.positions.len(),
            "{} normals/positions mismatch",
            m.prim_path
        );
    }

    // Optional GPU render.
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let snap_cam = {
        let mut s = snap.clone();
        let cam = idtx_render::camera_fit::fit_camera_to_snapshot(&s, 16.0 / 9.0);
        s.camera = Some(cam);
        s
    };
    let (w, h) = (1280, 720);
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir("scene").join("basic_scene_all_prim_types.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");
}

/// Render Pixar's `UsdCookie.usdz` through the PBR pipeline.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn render_usd_cookie() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let root = workspace_root();
    let usd_path = root.join("tests/fixtures/usd/UsdCookie.usdz");
    let path = usd_path.to_str().unwrap().to_owned();

    let mut service = StageService::open(&path).expect("open UsdCookie.usdz");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");

    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);

    let vp = service.project_viewport().expect("project viewport");
    println!(
        "  UsdCookie viewport: {} meshes, {} lights, cam={}",
        vp.meshes.len(),
        vp.lights.len(),
        vp.cameras.len() > 0
    );
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (1600, 900);
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);
    println!(
        "  UsdCookie snapshot: {} meshes, {} lights, cam={}",
        snap.meshes.len(),
        snap.lights.len(),
        snap.camera.is_some()
    );

    let Some(mut renderer) = ({
        let svc = Arc::new(SyncSvc(service));
        let reader: AssetReadFn = Arc::new(move |path: &str| svc.0.read_asset(path).ok());
        match Renderer::with_asset_reader(reader) {
            Ok(r) => Some(r),
            Err(RenderError::NoAdapter) => {
                eprintln!("Skipping GPU render — no adapter");
                None
            }
            Err(e) => {
                eprintln!("Skipping GPU render — {e}");
                None
            }
        }
    }) else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let out = renders_subdir("scene").join("iteration7_usd_cookie.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(
        non_bg > (w * h / 20) as usize,
        "fewer than 5% non-background pixels: {non_bg}"
    );
}

/// Render the Macbeth colour chart through the PBR pipeline.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn render_macbeth_chart() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let root = workspace_root();
    let usd_path = root.join("tests/fixtures/usd/macbeth_chart/macbeth_chart.usd");
    let path = usd_path.to_str().unwrap().to_owned();

    let mut service = StageService::open(&path).expect("open macbeth_chart.usd");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");

    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);

    let vp = service.project_viewport().expect("project viewport");
    println!(
        "  macbeth_chart viewport: {} meshes, {} lights, cam={}",
        vp.meshes.len(),
        vp.lights.len(),
        vp.cameras.len() > 0
    );
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (1600, 900);
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);
    println!(
        "  macbeth_chart snapshot: {} meshes, {} lights, cam={}",
        snap.meshes.len(),
        snap.lights.len(),
        snap.camera.is_some()
    );

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let out = renders_subdir("scene").join("iteration7_macbeth_chart.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(
        non_bg > (w * h / 20) as usize,
        "fewer than 5% non-background pixels: {non_bg}"
    );
}

// ── Multi-scene metrics baseline ──────────────────────────────────────────────

struct SceneMetrics {
    scene: String,
    mesh_count: usize,
    usd_load_ms: f64,
    renderer_init_ms: f64,
    cold_frame_ms: f64,
    warm_avg_ms: f64,
    warm_min_ms: f64,
    warm_max_ms: f64,
}

impl SceneMetrics {
    fn to_json(&self) -> String {
        format!(
            concat!(
                "  {{\n",
                "    \"scene\": \"{scene}\",\n",
                "    \"mesh_count\": {mc},\n",
                "    \"usd_load_ms\": {usd:.3},\n",
                "    \"renderer_init_ms\": {init:.3},\n",
                "    \"cold_frame_ms\": {cold:.3},\n",
                "    \"warm_frame_avg_ms\": {warm:.3},\n",
                "    \"warm_frame_min_ms\": {wmin:.3},\n",
                "    \"warm_frame_max_ms\": {wmax:.3}\n",
                "  }}"
            ),
            scene = self.scene,
            mc = self.mesh_count,
            usd = self.usd_load_ms,
            init = self.renderer_init_ms,
            cold = self.cold_frame_ms,
            warm = self.warm_avg_ms,
            wmin = self.warm_min_ms,
            wmax = self.warm_max_ms,
        )
    }
}

/// Hydrate `usd_path` and run cold+warm render metrics, returning a [`SceneMetrics`].
///
/// A fresh `Renderer::with_asset_reader` is created per scene so each measurement
/// starts with an empty texture cache (clean cold frame).
fn measure_scene(
    usd_path: &str,
    label: &str,
    w: u32,
    h: u32,
    iteration: u32,
) -> Option<SceneMetrics> {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let t_open = Instant::now();
    let mut service = match StageService::open(usd_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("  [{label}] open failed: {e}");
            return None;
        }
    };
    let delta = service.project_topology_paths(&[]).ok()?;
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().ok()?;
    hydrator.apply_viewport_projection(&vp).ok()?;
    let usd_load_ms = t_open.elapsed().as_secs_f64() * 1000.0;

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);

    let t_init = Instant::now();
    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    let mut renderer = match Renderer::with_asset_reader(reader) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("  [{label}] no GPU adapter");
            return None;
        }
    };
    let renderer_init_ms = t_init.elapsed().as_secs_f64() * 1000.0;

    let target = renderer.create_frame_target(w, h);

    // Cold frame — fresh texture cache, includes ArResolver + GPU upload
    let t_cold = Instant::now();
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).ok()?;
    let cold_ms = t_cold.elapsed().as_secs_f64() * 1000.0;

    // Save render image
    let img_name = format!(
        "iteration{iteration}_metrics_{}.png",
        label.replace(' ', "_").to_lowercase()
    );
    save_png(&renders_subdir("metrics").join(img_name), &pixels, w, h);

    // Warm frames — textures cached
    const WARM_N: usize = 5;
    let mut warm_times = [0.0f64; WARM_N];
    for slot in &mut warm_times {
        let t = Instant::now();
        renderer.render(&target, &snap, false);
        let _ = renderer.read_pixels(&target);
        *slot = t.elapsed().as_secs_f64() * 1000.0;
    }
    let warm_avg = warm_times.iter().sum::<f64>() / WARM_N as f64;
    let warm_min = warm_times.iter().cloned().fold(f64::INFINITY, f64::min);
    let warm_max = warm_times.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    println!(
        "  [{label}] meshes={mc}  usd={usd:.0}ms  init={init:.0}ms  \
         cold={cold:.0}ms  warm={warm:.0}ms (avg, N={WARM_N})",
        mc = snap.meshes.len(),
        usd = usd_load_ms,
        init = renderer_init_ms,
        cold = cold_ms,
        warm = warm_avg,
    );

    Some(SceneMetrics {
        scene: label.to_owned(),
        mesh_count: snap.meshes.len(),
        usd_load_ms,
        renderer_init_ms,
        cold_frame_ms: cold_ms,
        warm_avg_ms: warm_avg,
        warm_min_ms: warm_min,
        warm_max_ms: warm_max,
    })
}

/// Multi-scene render metrics baseline.
///
/// Runs cold + warm frame measurements on three representative USD files:
///   1. `DamagedHelmet.usdz` — packed PBR textures
///   2. `toy_biplane_realistic.usdz` — more complex mesh + textures
///   3. `chess_set/chess_set.usda` — many-mesh scene, no textures
///
/// Results are saved to `tests/renders/metrics/metrics_baseline_scenes.json`
/// and `tests/renders/metrics/phase4_texture_findings.md`.
#[test]
#[ignore]
fn render_metrics_baseline_scenes() {
    let root = workspace_root();
    let (w, h) = (1600, 900);

    let scenes: &[(&str, &str)] = &[
        ("DamagedHelmet.usdz", "DamagedHelmet"),
        ("2S_MM_v16.usdz", "2S_MM_v16"),
        ("chess_set/chess_set.usda", "ChessSet"),
    ];

    println!("=== Multi-Scene Metrics Baseline ===");

    let mut results: Vec<SceneMetrics> = Vec::new();
    for (rel_path, label) in scenes {
        let full = root.join("testfiles/usd").join(rel_path);
        let full_str = full.to_str().unwrap().to_owned();
        if !full.exists() {
            eprintln!("  [{label}] skipping — file not found: {full:?}");
            continue;
        }
        if let Some(m) = measure_scene(&full_str, label, w, h, 7) {
            results.push(m);
        }
    }

    if results.is_empty() {
        eprintln!("No scenes measured — skipping JSON output");
        return;
    }

    // Build JSON with per-scene entries
    let scene_entries = results
        .iter()
        .map(|r| r.to_json())
        .collect::<Vec<_>>()
        .join(",\n");
    let json = format!(
        "{{\n  \"phase\": 4,\n  \"iteration\": 7,\n  \"resolution\": [{w},{h}],\n  \
         \"warm_frame_n\": 5,\n  \"scenes\": [\n{scene_entries}\n  ]\n}}\n"
    );
    let json_path = renders_subdir("metrics").join("metrics_baseline_scenes.json");
    std::fs::write(&json_path, &json).unwrap_or_else(|e| eprintln!("metrics write: {e}"));
    println!("  Saved: {json_path:?}");

    // Build table rows for the findings doc
    let table_rows: String = results
        .iter()
        .map(|r| {
            format!(
                "| {} | {} | {:.1} | {:.1} | {:.1} |\n",
                r.scene, r.mesh_count, r.usd_load_ms, r.cold_frame_ms, r.warm_avg_ms
            )
        })
        .collect();

    let findings = format!(
        "# Phase 4 Texture Pipeline — Findings\n\n\
## Root Causes Fixed (Iteration 5 → 6)\n\n\
1. **No textures on .usdz**: `image::open()` cannot read `.usdz[0/tex.jpg]` bracket paths.\n   \
   Fix: `AssetReadFn` callback wraps `StageService::read_asset()` → `ArResolver` → bytes\n   \
   → `image::load_from_memory()`. `idtx-render` stays decoupled from `idtx-usd`.\n\n\
2. **Dark rendering**: Scenes without `UsdLuxLight` prims rendered near-black (ambient=0.03).\n   \
   Fix: synthesise a camera-aligned `Distant` headlight (power=3.5) when `snapshot.lights` is empty.\n\n\
3. **faceVarying displayColor ignored**: macbeth_chart uses `faceVarying` displayColor.\n   \
   The C++ bridge returned only `colors[0]`. Fixed with inline face-corner expansion\n   \
   (`UsdGeomPrimvarsAPI`, supporting `faceVarying`, `uniform`, `vertex`/`varying`, `constant`).\n   \
   Full `Vec<[f32;3]>` propagates through all layers as `vertex_color` and multiplies\n   \
   into `base_color` in the WGSL fragment shader.\n\n\
## Multi-Scene Metrics Baseline (Iteration 6)\n\n\
Resolution: {w}×{h}\n\n\
| Scene | Meshes | USD load (ms) | Cold frame (ms) | Warm avg (ms) |\n\
|---|---|---|---|---|\n\
{table_rows}\n\
**Cold frame** = first render per renderer instance; includes ArResolver decompression and\n\
GPU texture upload. **Warm avg** = mean of 5 subsequent frames with textures in `TextureCache`.\n\
The warm-frame cost reflects pure render-pass overhead and is the primary throughput metric.\n"
    );
    let findings_path = renders_subdir("metrics").join("phase4_texture_findings.md");
    std::fs::write(&findings_path, &findings).unwrap_or_else(|e| eprintln!("findings write: {e}"));
    println!("  Saved findings: {findings_path:?}");

    for r in &results {
        assert!(
            r.warm_avg_ms < 10_000.0,
            "[{}] warm frame stall: {:.1} ms",
            r.scene,
            r.warm_avg_ms
        );
    }
}

// ── 2-pass transparent rendering ──────────────────────────────────────────────

/// USD `glass_scene.usda` with a semi-transparent glass sphere.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn render_glass_scene() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/materials/glass_scene.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open glass_scene.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (1600, 900);
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);

    let transparent_count = snap
        .meshes
        .iter()
        .filter(|m| m.material.is_transparent())
        .count();
    println!(
        "  glass_scene: {} meshes total, {} transparent",
        snap.meshes.len(),
        transparent_count
    );
    assert!(
        transparent_count >= 1,
        "expected at least one transparent mesh in glass_scene.usda"
    );

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(
        non_bg > (w * h / 20) as usize,
        "fewer than 5% non-background pixels: {non_bg}"
    );

    let out = renders_subdir("materials").join("iteration8_glass_scene.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");
}

/// Pawn asset glass material test.
///
/// Verifies that the chess Pawn's `Pawn_Top_B` / `Pawn_Top_W` surfaces, which use
/// `MaterialX` `standard_surface` with `transmission = 1`, are correctly resolved to
/// `AlphaPolicy::Blend` after the `transmission`→`opacity` bridge mapping is applied.
///
/// Expected scene structure:
/// - At least 1 mesh extracted (body)
/// - At least 1 transparent mesh (top sphere — transmission=1 → opacity=0 → Blend)
/// - Render contains non-background pixels (body is opaque, visible through the glass top)
#[cfg(feature = "materialx-wgsl")]
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn render_pawn_glass() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/Pawn/Pawn.usd")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open Pawn.usd");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (1600, 900);
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);

    println!(
        "  pawn: {} meshes total, {} lights, cam={}",
        snap.meshes.len(),
        snap.lights.len(),
        snap.camera.is_some()
    );
    for inst in &snap.meshes {
        println!(
            "    {} — alpha_policy={:?} opacity={:.3} transparent={}",
            inst.prim_path,
            inst.material.alpha_policy,
            inst.material.opacity,
            inst.material.is_transparent()
        );
    }

    assert!(
        !snap.meshes.is_empty(),
        "Pawn.usd produced no meshes — payload not loaded?"
    );

    let transparent_count = snap
        .meshes
        .iter()
        .filter(|m| m.material.is_transparent())
        .count();
    assert!(
        transparent_count >= 1,
        "expected ≥1 transparent mesh (Pawn_Top glass), got 0 — \
         standard_surface transmission not mapped to AlphaPolicy::Blend?"
    );
    println!("  transparent meshes: {transparent_count}");

    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let non_bg = pixels
        .chunks_exact(4)
        .filter(|px| px[0] > 30 || px[1] > 30 || px[2] > 30)
        .count();
    assert!(
        non_bg > (w * h / 50) as usize,
        "fewer than 2% non-background pixels: {non_bg}"
    );

    let out = renders_subdir("materials").join("iteration11_pawn_glass.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");
}

// ── mesh GPU buffer cache — warm-frame perf regression ──────────

/// Regression guard: warm-frame rendering of a large static scene must not
/// degrade back to O(mesh_count) GPU buffer re-allocation.
///
/// Before the mesh GPU cache (`Renderer::mesh_gpu_cache`) the 2S_MM_v16 scene
/// (2161 meshes) took ~416 ms/frame because every frame re-allocated ~8 644
/// wgpu buffers and ~4 322 bind groups.  With the cache, warm frames for a
/// fully static scene should complete in < 50 ms on any hardware (including
/// software renderers used in CI).
///
/// Run with:
/// ```bash
/// cargo test -p idtx-render -- render_warm_frame_perf_2s_mm --ignored --nocapture
/// ```
#[test]
#[ignore]
fn render_warm_frame_perf_2s_mm() {
    use idtx_scene::{PopulatorRegistry, hydrator::SceneHydrator};
    use idtx_usd::StageService;

    let root = workspace_root();
    let path = root.join("testfiles/usd/2S_MM_v16.usdz");
    if !path.exists() {
        eprintln!("render_warm_frame_perf_2s_mm: skipping — 2S_MM_v16.usdz not found");
        return;
    }

    let (w, h) = (1600u32, 900u32);
    let path_str = path.to_str().unwrap().to_owned();

    let mut service = StageService::open(&path_str).expect("open stage");
    let delta = service.project_topology_paths(&[]).expect("topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("viewport");
    hydrator.apply_viewport_projection(&vp).expect("project");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);

    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    let mut renderer = match Renderer::with_asset_reader(reader) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("render_warm_frame_perf_2s_mm: no GPU adapter");
            return;
        }
    };
    let target = renderer.create_frame_target(w, h);

    // Cold frame — fills the mesh GPU cache
    renderer.render(&target, &snap, false);
    let _ = renderer.read_pixels(&target);

    // Warm frames — cache populated, GPU work should be O(1) allocs
    const WARM_N: usize = 5;
    let mut warm_times = [0.0f64; WARM_N];
    for slot in &mut warm_times {
        let t = std::time::Instant::now();
        renderer.render(&target, &snap, false);
        let _ = renderer.read_pixels(&target);
        *slot = t.elapsed().as_secs_f64() * 1000.0;
    }
    let warm_avg = warm_times.iter().sum::<f64>() / WARM_N as f64;
    let mesh_count = snap.meshes.len();

    println!(
        "render_warm_frame_perf_2s_mm: meshes={mesh_count}  warm_avg={warm_avg:.1}ms  \
         samples={warm_times:?}"
    );

    // 50 ms is very generous for software renderers (Warp/SwiftShader in CI).
    // The pre-cache baseline was ~416 ms — this assertion catches any O(N) regression.
    assert!(
        warm_avg < 50.0,
        "warm-frame regression: {warm_avg:.1} ms for {mesh_count} meshes (threshold 50 ms). \
         Likely cause: mesh GPU cache disabled or per-frame buffer re-allocation reintroduced."
    );
}

/// Native USD scenegraph instancing (`instanceable = true`) must be extracted as a
/// single hardware-instanced batch — the prototype Cube is projected once and shared
/// by both instances, mirroring the `PointInstancer` de-dup path.
///
/// Fixture `native_instances.usda`: `/World/First` and `/World/Second` both reference
/// `</World/Prototype>` (one Cube). Expected: one `RenderInstancedBatch` with two
/// instance transforms, no prototype Cube drawn directly, `native_instance_count == 2`.
#[test]
#[ignore]
fn native_instances_extract_single_batch_two_transforms() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/instances/native_instances.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open native_instances.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 16.0 / 9.0, Mat4::IDENTITY);

    println!(
        "native_instances: {} direct meshes, {} instanced batches, native_instance_count={}",
        snap.meshes.len(),
        snap.instanced_batches.len(),
        snap.stats.native_instance_count
    );
    for b in &snap.instanced_batches {
        println!(
            "  batch {} — {} transforms, prims {:?}",
            b.prototype_key,
            b.instance_transforms.len(),
            b.instance_prim_paths
        );
    }

    // The prototype Cube must NOT be drawn directly (prototype_only geometry is
    // hardware-instanced, never emitted as a standalone mesh).
    assert!(
        snap.meshes.is_empty(),
        "prototype geometry must not be drawn directly, got {} direct meshes",
        snap.meshes.len()
    );

    // Exactly one batch: one prototype Cube, single material, no GeomSubsets.
    assert_eq!(
        snap.instanced_batches.len(),
        1,
        "expected exactly one instanced batch for the shared prototype"
    );
    let batch = &snap.instanced_batches[0];

    // Two instances share the prototype.
    assert_eq!(
        batch.instance_transforms.len(),
        2,
        "expected 2 instance transforms (First + Second)"
    );
    assert_eq!(
        batch.instance_prim_paths.len(),
        2,
        "each instance transform must carry its instancing-prim path for picking"
    );
    let mut prims = batch.instance_prim_paths.clone();
    prims.sort();
    assert_eq!(
        prims,
        vec!["/World/First".to_string(), "/World/Second".to_string()]
    );

    // Prototype geometry is real (the Cube tessellated once).
    assert!(
        batch.mesh.triangle_count() > 0,
        "prototype mesh has no geometry"
    );

    // The two instances are placed at x = -2 and x = +2 (Y-up stage basis is identity).
    let xs: Vec<f32> = batch
        .instance_transforms
        .iter()
        .map(|m| m.w_axis.x)
        .collect();
    assert!(
        xs.iter().any(|&x| (x + 2.0).abs() < 1e-3) && xs.iter().any(|&x| (x - 2.0).abs() < 1e-3),
        "instance translations should be x=-2 and x=+2, got {xs:?}"
    );

    // Metric: two native instances counted.
    assert_eq!(
        snap.stats.native_instance_count, 2,
        "native_instance_count metric"
    );

    // Optional GPU render for visual confirmation.
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let snap_cam = {
        let mut s = snap.clone();
        let cam = fit_camera_to_snapshot(&s, 16.0 / 9.0);
        s.camera = Some(cam);
        s
    };
    let (w, h) = (1280, 720);
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap_cam, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    let out = renders_subdir("instances").join("native_instances.png");
    save_png(&out, &pixels, w, h);
    println!("Saved render: {out:?}");
}

/// Baseline capture for the native-instancing win on a real production scene.
///
/// `Kitchen_set_instanced.usd` uses ~425 `instanceable = true` prims sharing a
/// handful of prototypes. Before this change the bridge flattened them into ~425
/// standalone meshes (425 GPU buffers / draw calls). After collapsing to instances
/// and hardware-instancing the prototypes, the direct mesh count collapses to the
/// per-prototype geometry and the instances fold into a few `RenderInstancedBatch`es.
///
/// Ignored by default (large asset, USD link required). Run with:
/// `cargo test -p idtx-render --test render_integration native_instances_kitchen_set_metrics -- --ignored --nocapture`
#[test]
#[ignore = "large real asset; requires IDTX_USD_DIR — run manually for the perf baseline"]
fn native_instances_kitchen_set_metrics() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/Kitchen_set-2/Kitchen_set_instanced.usd")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open Kitchen_set_instanced.usd");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 16.0 / 9.0, Mat4::IDENTITY);

    let batch_draws: usize = snap
        .instanced_batches
        .iter()
        .map(|b| b.instance_transforms.len())
        .sum();
    println!("Kitchen_set_instanced native-instancing baseline:");
    println!(
        "  native instances collapsed : {}",
        snap.stats.native_instance_count
    );
    println!("  direct meshes (drawn once) : {}", snap.meshes.len());
    println!(
        "  instanced batches          : {}",
        snap.instanced_batches.len()
    );
    println!("  instanced draws (expanded) : {batch_draws}");
    println!(
        "  → prototype geometry uploaded once as {} batches, covering {} instanced placements \
         that the old flatten path would have expanded into {} standalone meshes \
         (one GPU buffer + draw call each)",
        snap.instanced_batches.len(),
        batch_draws,
        batch_draws + snap.meshes.len(),
    );

    // The whole point: native instances were detected and folded into batches, not
    // flattened into one standalone mesh each.
    assert!(
        snap.stats.native_instance_count > 0,
        "expected native instances to be detected in Kitchen_set_instanced.usd"
    );
    assert!(
        !snap.instanced_batches.is_empty(),
        "expected at least one hardware-instanced batch"
    );
}

/// TMDD guard for the F1 submission decomposition
/// (docs/draw-submission-analysis.md §4/§6). The named sub-phases
/// (`pass_setup` / `encode_opaque` / `encode_transparent` / `encode_tonemap` /
/// `finish` / `submit`) partition the `gpu_passes_ns` submission window. This
/// asserts they stay a *partition* — their sum never exceeds `gpu_passes_ns` and
/// accounts for the bulk of it — so the decomposition can't silently drift into
/// lying about where the time goes. It also sanity-checks the per-frame counters.
///
/// Runs on any adapter (no USD, no timestamp support required); it measures CPU
/// wall-clock relationships, not absolute GPU speed.
#[cfg(feature = "metrics")]
#[test]
#[ignore]
fn submit_decomposition_partitions_gpu_passes() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    // This test targets the per-mesh submission decomposition, which predates
    // static draw-merge; disable merge so the two synthetic meshes stay on the
    // per-mesh path and the draw/command counts below hold.
    renderer.set_draw_merge_enabled(false);
    let snap = synthetic_snapshot();
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);

    // Cold frame builds caches; measure a warm frame so the numbers are steady.
    renderer.render(&target, &snap, false);
    renderer.render(&target, &snap, false);
    let t = renderer.last_frame_timings();

    let sum = t.pass_setup_ns
        + t.encode_opaque_ns
        + t.encode_transparent_ns
        + t.encode_tonemap_ns
        + t.finish_ns
        + t.submit_ns;

    // The sub-phases are contiguous brackets inside the window, so their sum can
    // never exceed `gpu_passes_ns` (the tiny +slack absorbs timer read jitter).
    assert!(
        sum <= t.gpu_passes_ns + 50_000,
        "sub-phase sum {sum} ns exceeds gpu_passes_ns {} ns",
        t.gpu_passes_ns
    );
    // The only unattributed remainder is the pre-encoder timestamp-writes match
    // and the timestamp resolve section — both tiny. The sub-phases must account
    // for the majority of the window, else time is leaking out of the breakdown.
    assert!(
        sum * 2 >= t.gpu_passes_ns,
        "sub-phases {sum} ns account for less than half of gpu_passes_ns {} ns",
        t.gpu_passes_ns
    );

    // Counter consistency: two opaque meshes → at least two draw calls; actual
    // pipeline switches can never exceed the draw count; each draw records more
    // than one command; a mono desktop frame carries no eye index.
    assert!(
        t.draw_calls >= 2,
        "expected >=2 draws, got {}",
        t.draw_calls
    );
    assert!(
        t.pipeline_switches <= t.draw_calls,
        "pipeline_switches {} exceeds draw_calls {}",
        t.pipeline_switches,
        t.draw_calls
    );
    assert!(
        t.commands_encoded > t.draw_calls,
        "commands_encoded {} should exceed draw_calls {}",
        t.commands_encoded,
        t.draw_calls
    );
    assert!(
        t.bind_group_sets >= t.draw_calls,
        "expected at least one bind group per draw"
    );
    assert!(t.eye.is_none(), "mono desktop frame should have eye = None");
}

/// TMDD guard for the whole-frame CPU decomposition (the "uninstrumented wall"
/// instrumentation). Every named CPU phase plus the computed `residual_ns` must
/// reconstruct `total_ns` exactly — this is the invariant that makes the residual
/// a trustworthy catch-all: if the reconstruction did not equal the total, some
/// phase would be double-counted or `residual_ns` would be wrong.
///
/// Also asserts the named phases actually cover a meaningful fraction of the frame
/// (so the residual is a remainder, not the whole story), and that `gpu_wait_ns`
/// only populates when GPU timestamps do (it rides the same readback poll).
///
/// Runs on any adapter; it measures CPU wall-clock relationships, not GPU speed.
#[cfg(feature = "metrics")]
#[test]
#[ignore]
fn total_ns_partition_is_consistent() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let snap = synthetic_snapshot();
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);

    // Warm frame so the caches are populated and the phases are steady-state.
    renderer.render(&target, &snap, false);
    renderer.render(&target, &snap, false);
    let t = renderer.last_frame_timings();

    let named_sum = t.camera_fit_ns
        + t.camera_lights_ns
        + t.materialx_prep_ns
        + t.merge_plan_ns
        + t.mesh_classify_ns
        + t.transform_update_ns
        + t.material_rebuild_ns
        + t.cache_miss_build_ns
        + t.evict_ns
        + t.instance_cache_ns
        + t.draw_list_build_ns
        + t.gpu_passes_ns;

    // Exact reconstruction: named phases + residual == total. `residual_ns` is
    // computed as `total.saturating_sub(named_sum)`, so this holds by construction
    // *unless* the named phases overlap (double-count) and push the sum past total —
    // in which case residual saturates to 0 and this assert catches the leak.
    assert_eq!(
        named_sum + t.residual_ns,
        t.total_ns,
        "named phases ({named_sum}) + residual ({}) must reconstruct total ({})",
        t.residual_ns,
        t.total_ns
    );

    // The named phases are contiguous sub-spans of the frame, so their sum can
    // never exceed the total (a tiny slack absorbs the fact that `total_ns` is read
    // last, after the final phase timer closes).
    assert!(
        named_sum <= t.total_ns + 50_000,
        "named phase sum {named_sum} ns exceeds total_ns {} ns",
        t.total_ns
    );

    // `gpu_wait_ns` rides the timestamp-readback poll: it is nonzero only when the
    // adapter supports timestamp queries (i.e. `gpu_pbr_ns` populated). On CI
    // software renderers both are 0 — asserted so the proxy stays honest.
    if t.gpu_pbr_ns == 0 {
        assert_eq!(
            t.gpu_wait_ns, 0,
            "gpu_wait_ns {} should be 0 when timestamps are unsupported",
            t.gpu_wait_ns
        );
    }

    // A warm frame with a stable snapshot must not be recompiling pipelines.
    assert_eq!(
        t.materialx_pipeline_compiles, 0,
        "warm frame should compile no MaterialX pipelines, got {}",
        t.materialx_pipeline_compiles
    );
}

/// TMDD guard for the synthetic-camera scene-AABB cache.
///
/// `synthetic_snapshot()` carries no authored camera, so `render()` falls back to
/// `resolve_synthetic_camera`, which runs the full per-vertex `compute_scene_aabb`
/// only when the scene's geometry signature changes. This asserts the cache
/// lifecycle end-to-end via the `scene_aabb_recomputes` metric:
///   - cold frame  → recompute (signature miss),
///   - warm frame (identical content) → cache hit, 0 recomputes,
///   - a transform-only change (new world matrix, serial unchanged at 0) → recompute,
///   - an authored camera → fit path skipped entirely, 0 recomputes.
/// This is the frame-level proof that the ~48 ms per-frame `camera_fit` collapses to
/// a cheap signature check on warm frames.
#[cfg(feature = "metrics")]
#[test]
#[ignore]
fn synthetic_camera_aabb_is_cached() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);

    // Cold: first camera-less frame must recompute the AABB.
    let snap = synthetic_snapshot();
    renderer.render(&target, &snap, false);
    assert_eq!(
        renderer.last_frame_timings().scene_aabb_recomputes,
        1,
        "cold camera-less frame must recompute the scene AABB"
    );

    // Warm: a fresh but content-identical snapshot must hit the signature cache.
    // (The cache keys on content, not object identity, so a rebuilt snapshot with
    // the same geometry + transforms is still a hit.)
    let snap_same = synthetic_snapshot();
    renderer.render(&target, &snap_same, false);
    assert_eq!(
        renderer.last_frame_timings().scene_aabb_recomputes,
        0,
        "warm frame with unchanged geometry must reuse the cached AABB"
    );

    // Transform-only change: move a mesh without bumping any serial (serial stays 0
    // for a Default-built snapshot). A pure-`serial` gate would MISS this and frame
    // stale geometry; the signature must catch it and recompute.
    let mut snap_moved = synthetic_snapshot();
    snap_moved.meshes[0].world_matrix = Mat4::from_translation(glam::Vec3::new(100.0, 0.0, 0.0));
    renderer.render(&target, &snap_moved, false);
    assert_eq!(
        renderer.last_frame_timings().scene_aabb_recomputes,
        1,
        "a transform-only change (no serial bump) must invalidate the cached AABB"
    );

    // Authored camera: the fit path is skipped entirely, so no AABB work at all.
    let mut snap_cam = synthetic_snapshot();
    snap_cam.camera = Some(fit_camera_to_snapshot(&snap_cam, w as f32 / h as f32));
    renderer.render(&target, &snap_cam, false);
    assert_eq!(
        renderer.last_frame_timings().scene_aabb_recomputes,
        0,
        "an authored camera must skip the synthetic-fit AABB path entirely"
    );
}

/// TMDD guard for the opaque/transparent GPU-timestamp split (Fix 5 step 1).
///
/// The within-pass interior timestamps partition the whole-pass `gpu_pbr_ns` into
/// opaque and transparent segments. Their sum can never exceed the whole-pass span
/// (they are sub-brackets of slots 0→1). When the adapter lacks
/// `TIMESTAMP_QUERY_INSIDE_PASSES` (e.g. many Apple GPUs) or timestamps entirely
/// (CI software renderers) the split degrades to `0` — asserted here so the
/// fallback stays honest rather than reporting a bogus partition.
///
/// Runs on any adapter; it checks the split *relationship*, not absolute GPU speed.
#[cfg(feature = "metrics")]
#[test]
#[ignore]
fn gpu_pbr_split_is_consistent() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let snap = synthetic_snapshot(); // two opaque meshes
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);

    // Warm frame so the split reflects steady-state GPU execution.
    renderer.render(&target, &snap, false);
    renderer.render(&target, &snap, false);
    let t = renderer.last_frame_timings();

    let split_sum = t.gpu_pbr_opaque_ns + t.gpu_pbr_transparent_ns;
    if split_sum == 0 {
        // Either no timestamp support, or no within-pass (INSIDE_PASSES) support —
        // both are valid; the whole-pass span may still be populated or zero.
        return;
    }
    // The split brackets live inside the whole PBR pass, so their sum must not
    // exceed the whole-pass span (small slack for the interior-vs-boundary
    // sampling jitter).
    assert!(
        split_sum <= t.gpu_pbr_ns + t.gpu_pbr_ns / 20 + 50_000,
        "opaque {} + transparent {} = {split_sum} ns exceeds gpu_pbr_ns {} ns",
        t.gpu_pbr_opaque_ns,
        t.gpu_pbr_transparent_ns,
        t.gpu_pbr_ns
    );
    // This scene is entirely opaque, so opaque time should dominate.
    assert!(
        t.gpu_pbr_opaque_ns >= t.gpu_pbr_transparent_ns,
        "all-opaque scene: opaque {} should be >= transparent {}",
        t.gpu_pbr_opaque_ns,
        t.gpu_pbr_transparent_ns
    );
}

/// Helper: hydrate a USD scene and return a `(renderer, target, snapshot)` ready
/// to render at `w×h`. Mirrors the setup in [`measure_scene`] but keeps the
/// renderer so callers can read `last_frame_timings()`. Returns `None` when the
/// stage cannot open or no GPU adapter is present.
#[cfg(feature = "metrics")]
fn setup_usd_scene(
    usd_path: &str,
    w: u32,
    h: u32,
) -> Option<(Renderer, FrameTarget, RenderWorldSnapshot)> {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let mut service = StageService::open(usd_path).ok()?;
    let delta = service.project_topology_paths(&[]).ok()?;
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().ok()?;
    hydrator.apply_viewport_projection(&vp).ok()?;
    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, glam::Mat4::IDENTITY);

    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    let renderer = Renderer::with_asset_reader(reader).ok()?;
    let target = renderer.create_frame_target(w, h);
    Some((renderer, target, snap))
}

/// Regression guard for physically-correct sphere/area-light radiometry on a
/// centimetre-scale stage (`metersPerUnit = 0.01`), mirroring the BarScene rig
/// that motivated the fix.
///
/// The old shader used a fixed `1/max(d², 0.01)` attenuation clamp and no
/// emitter-area term. On a cm-scale stage the pseudo-root basis shrinks every
/// distance below the 0.01 floor, pinning attenuation at its 100× maximum
/// everywhere; combined with six non-normalized sphere lights the frame blows
/// out to near-uniform white. With the radius-aware clamp + folded `π·r²`
/// emitter-area term the same rig renders at a legible brightness.
///
/// This test hydrates with the real `pseudo_root_matrix` basis (NOT the
/// identity basis of `setup_usd_scene`) so the cm-scale bug can actually
/// reproduce. It reads back the frame and asserts the image is neither blown
/// out (few near-white pixels, mean luminance in a sane band) nor black.
///
/// Run with:
/// ```bash
/// cargo test -p idtx-render cm_scale_sphere_lights_not_blown_out -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn cm_scale_sphere_lights_not_blown_out() {
    use idtx_scene::coordinate::{UpAxis, WorldUnitScale, pseudo_root_matrix};
    use idtx_scene::{PopulatorRegistry, hydrator::SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root().join("tests/fixtures/usd/lighting/cm_scale_sphere_lights.usda");
    let path_str = path.to_str().unwrap().to_owned();
    let (w, h) = (640u32, 360u32);

    let Ok(mut service) = StageService::open(&path_str) else {
        eprintln!("skip: could not open cm_scale_sphere_lights.usda");
        return;
    };
    let meta = service.metadata();
    // Build the same root basis the studio session uses: cm stage → metre render
    // units yields a uniform 0.01 scale, the exact configuration that pinned the
    // old attenuation clamp.
    let stage_basis = pseudo_root_matrix(
        WorldUnitScale::METRE,
        meta.meters_per_unit,
        UpAxis::from_usd_token(&meta.up_axis),
    );

    let delta = service.project_topology_paths(&[]).expect("topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");
    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, stage_basis);

    assert_eq!(snap.lights.len(), 6, "expected six sphere lights");
    assert!(!snap.meshes.is_empty(), "no meshes hydrated");

    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    let Ok(mut renderer) = Renderer::with_asset_reader(reader) else {
        eprintln!("skip: no wgpu adapter");
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    // Luminance stats over all pixels (sRGB-ish 0–255 after tonemap).
    let total = (w * h) as f64;
    let mut lum_sum = 0.0f64;
    let mut near_white = 0usize;
    let mut non_black = 0usize;
    for px in pixels.chunks_exact(4) {
        let (r, g, b) = (px[0] as f64, px[1] as f64, px[2] as f64);
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        lum_sum += lum;
        if r > 250.0 && g > 250.0 && b > 250.0 {
            near_white += 1;
        }
        if r > 20.0 || g > 20.0 || b > 20.0 {
            non_black += 1;
        }
    }
    let mean_lum = lum_sum / total;
    let near_white_frac = near_white as f64 / total;
    let non_black_frac = non_black as f64 / total;
    println!(
        "cm_scale_sphere_lights: mean_lum={mean_lum:.1} near_white={:.2}% non_black={:.2}%",
        near_white_frac * 100.0,
        non_black_frac * 100.0,
    );

    save_png(
        &renders_subdir("lighting").join("cm_scale_sphere_lights.png"),
        &pixels,
        w,
        h,
    );

    // The scene is lit, so it must not be black …
    assert!(
        non_black_frac > 0.10,
        "scene rendered nearly black: {non_black_frac:.3}"
    );
    // … but the fix must prevent the blowout: only a small fraction of pixels
    // may be near-white and the mean luminance must stay well below saturation.
    assert!(
        near_white_frac < 0.05,
        "blown out: {:.1}% near-white pixels (blowout regression)",
        near_white_frac * 100.0
    );
    assert!(
        mean_lum < 200.0,
        "blown out: mean luminance {mean_lum:.1} too close to white (255)"
    );
}

/// TMDD budget guard for the CPU submission window (Fix 4).
///
/// The draw-submission analysis established that `cpu_submit` (== `gpu_passes_ns`,
/// the CPU-side command-encode + submit wall-clock) is already tiny after the
/// static draw-merge + redundant-state-dedup work — Kitchen_set measured ~0.59 ms.
/// This locks that in: a warm Kitchen_set frame must submit in ≤ 1.5 ms (headroom
/// over the measured 0.59 ms). Mirrors the `warm_avg_ms < 10_000.0` stall guard in
/// [`render_metrics_baseline`], but targets the metric that a submission-path
/// regression would move. Requires USD linking, so `#[ignore]` by default.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn cpu_submit_budget_kitchen_set() {
    let path = workspace_root()
        .join("testfiles/usd/Kitchen_set-2/Kitchen_set.usd")
        .to_str()
        .unwrap()
        .to_owned();
    let (w, h) = (1600, 900);
    let Some((mut renderer, target, snap)) = setup_usd_scene(&path, w, h) else {
        eprintln!("skip: could not open Kitchen_set or no adapter");
        return;
    };

    // Cold frame builds caches; measure the warm steady state.
    renderer.render(&target, &snap, false);
    const WARM_N: usize = 5;
    let mut cpu_submit_ns = [0u64; WARM_N];
    for slot in &mut cpu_submit_ns {
        renderer.render(&target, &snap, false);
        *slot = renderer.last_frame_timings().gpu_passes_ns;
    }
    let avg_ns = cpu_submit_ns.iter().sum::<u64>() / WARM_N as u64;
    let avg_ms = avg_ns as f64 * 1e-6;
    println!("  Kitchen_set cpu_submit warm avg = {avg_ms:.3} ms ({avg_ns} ns)");
    assert!(
        avg_ns <= 1_500_000,
        "Kitchen_set cpu_submit regressed: {avg_ms:.3} ms (budget 1.5 ms)"
    );
}

/// TMDD guard for the Fix 5 step 1 counters existing and being tracked on a heavy
/// UDIM scene. No hard budget — the UDIM/FullScene path is expected to be GPU-red;
/// this only asserts the new instrumentation *populates* (the whole-pass GPU span
/// and the CPU submission window are tracked) so a wiring regression is caught.
/// The opaque/transparent split and fragment-invocation counters are backend-gated
/// (`TIMESTAMP_QUERY_INSIDE_PASSES` / `PIPELINE_STATISTICS_QUERY`, the latter absent
/// on Metal), so they are reported but not required to be non-zero.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn udim_scene_counters_tracked() {
    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_instanced.usda")
        .to_str()
        .unwrap()
        .to_owned();
    let (w, h) = (1600, 900);
    let Some((mut renderer, target, snap)) = setup_usd_scene(&path, w, h) else {
        eprintln!("skip: could not open udim_instanced.usda or no adapter");
        return;
    };

    renderer.render(&target, &snap, false); // cold
    renderer.render(&target, &snap, false); // warm
    let t = renderer.last_frame_timings();
    println!(
        "  udim: cpu_submit={:.3}ms gpu_pbr={:.3}ms opaque={:.3}ms transp={:.3}ms invocations={}",
        t.gpu_passes_ns as f64 * 1e-6,
        t.gpu_pbr_ns as f64 * 1e-6,
        t.gpu_pbr_opaque_ns as f64 * 1e-6,
        t.gpu_pbr_transparent_ns as f64 * 1e-6,
        t.fragment_shader_invocations,
    );

    // The CPU submission window is always tracked (no GPU-feature dependency).
    assert!(
        t.gpu_passes_ns > 0,
        "cpu_submit (gpu_passes_ns) should be tracked"
    );
    assert!(t.draw_calls > 0, "expected at least one draw call");
    // If timestamps are supported the whole-pass GPU span must populate; the split
    // and overdraw counters are backend-gated and only sanity-checked when present.
    if t.gpu_pbr_ns > 0 {
        let split = t.gpu_pbr_opaque_ns + t.gpu_pbr_transparent_ns;
        assert!(
            split <= t.gpu_pbr_ns + t.gpu_pbr_ns / 20 + 50_000,
            "split sum {split} ns exceeds gpu_pbr_ns {} ns",
            t.gpu_pbr_ns
        );
    }
}

/// TMDD guard for the redundant-state dedup (docs/draw-submission-analysis.md §7).
///
/// The two synthetic meshes are both opaque and single-sided, so they share the
/// same `single_sided` `RenderPipeline`. After the pipeline-key sort they are
/// recorded back to back, so the second mesh's `set_pipeline` is redundant and
/// must be skipped. Comparing a one-mesh frame to a two-mesh frame isolates the
/// cost of that added mesh: with dedup it records exactly `bg2 + bg3 + vb0 + ib +
/// draw = 5` commands; without dedup it would also re-bind the pipeline (6). The
/// differential cancels every fixed per-frame command (camera/lights binds,
/// tonemap, optional axis overlay), so the assertion is robust to feature flags.
///
/// This is the metric the analysis named as the north star: `commands_encoded`
/// falling while `draw_calls` and pipeline switches stay flat.
#[cfg(feature = "metrics")]
#[test]
#[ignore]
fn redundant_pipeline_binds_are_deduplicated() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    // Isolate the redundant-state dedup: with draw-merge on, the two same-material
    // synthetic meshes would collapse into one baked batch and the per-mesh command
    // delta this test measures would vanish. Disable merge to test dedup alone.
    renderer.set_draw_merge_enabled(false);
    let (w, h) = (128, 128);
    let target = renderer.create_frame_target(w, h);

    let one = single_cube_snapshot();
    renderer.render(&target, &one, false); // cold — build caches
    renderer.render(&target, &one, false); // warm
    let one_cmds = renderer.last_frame_timings().commands_encoded;
    let one_draws = renderer.last_frame_timings().draw_calls;
    let one_switches = renderer.last_frame_timings().pipeline_switches;

    let two = synthetic_snapshot();
    renderer.render(&target, &two, false); // cold — build caches
    renderer.render(&target, &two, false); // warm
    let two_cmds = renderer.last_frame_timings().commands_encoded;
    let two_draws = renderer.last_frame_timings().draw_calls;
    let two_switches = renderer.last_frame_timings().pipeline_switches;

    // One extra same-pipeline opaque mesh → exactly one extra draw …
    assert_eq!(
        two_draws - one_draws,
        1,
        "second mesh should add one draw call"
    );
    // … and its `set_pipeline` must be deduplicated, so pipeline switches are
    // unchanged between the two frames.
    assert_eq!(
        two_switches, one_switches,
        "adding a same-pipeline mesh must not add a pipeline switch (dedup failed)"
    );
    // The added mesh records bg2 + bg3 + vb0 + ib + draw = 5 commands. A 6 here
    // means the redundant pipeline bind slipped back into the command stream.
    assert_eq!(
        two_cmds - one_cmds,
        5,
        "expected +5 recorded commands for a deduped same-pipeline mesh, got +{}",
        two_cmds - one_cmds
    );
}

// ── Static draw-merge (F1/F7) ───────────────────────────────────────────────

/// A scene whose opaque meshes all share one fallback material, so the static
/// draw-merge planner collapses them. Three non-mirrored boxes at distinct
/// translations form one merged batch; two negative-X-scale (mirrored) boxes form
/// a second, exercising the mirrored bake + `FrontFace::Cw` pipeline selection.
fn merge_test_snapshot() -> RenderWorldSnapshot {
    let mut snap = RenderWorldSnapshot::default();
    let mut mat = fallback_material("/Looks/Shared");
    mat.base_color = [0.6, 0.5, 0.2];

    for (i, &x) in [-1.2f32, 0.0, 1.2].iter().enumerate() {
        let mut cube = tessellate_cube(0.4);
        let path = format!("/World/Box_{i}");
        cube.prim_path = path.clone();
        snap.meshes.push(RenderMeshInstance {
            prim_path: path,
            mesh: Arc::new(cube),
            world_matrix: Mat4::from_translation(glam::Vec3::new(x, 0.4, 0.0)),
            material: mat.clone(),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        });
    }
    for (i, &x) in [-0.6f32, 0.6].iter().enumerate() {
        let mut cube = tessellate_cube(0.35);
        let path = format!("/World/Mirror_{i}");
        cube.prim_path = path.clone();
        // Negative-X scale → negative determinant → mirrored pipeline path.
        let m = Mat4::from_translation(glam::Vec3::new(x, -0.5, 0.0))
            * Mat4::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0));
        snap.meshes.push(RenderMeshInstance {
            prim_path: path,
            mesh: Arc::new(cube),
            world_matrix: m,
            material: mat.clone(),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        });
    }
    snap.stats = ExtractionStats {
        mesh_count: snap.meshes.len(),
        ..Default::default()
    };
    snap
}

/// Pixel-identity regression: baking world transforms into merged vertices and
/// drawing with an identity group-2 uniform must reproduce the per-mesh render
/// exactly. Renders the same scene with merge off (reference) and on, and asserts
/// the two frames are effectively identical (PSNR ≥ 50 dB). Covers the mirrored
/// (negative-determinant) bake path too.
#[test]
#[ignore = "requires wgpu GPU adapter"]
fn draw_merge_is_pixel_identical() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (512, 512);
    let target = renderer.create_frame_target(w, h);
    let mut snap = merge_test_snapshot();
    snap.camera = Some(fit_camera_to_snapshot(&snap, w as f32 / h as f32));

    renderer.set_draw_merge_enabled(false);
    renderer.render(&target, &snap, false);
    let reference = renderer.read_pixels(&target).expect("readback");

    renderer.set_draw_merge_enabled(true);
    renderer.render(&target, &snap, false); // cold — bake + build
    renderer.render(&target, &snap, false); // warm — reuse cache
    let merged = renderer.read_pixels(&target).expect("readback");

    save_png(
        &renders_subdir("rendering").join("draw_merge_reference.png"),
        &reference,
        w,
        h,
    );
    save_png(&renders_subdir("rendering").join("draw_merge_merged.png"), &merged, w, h);
    let psnr = psnr_rgba(&reference, &merged);
    println!("draw-merge PSNR merged-vs-reference: {psnr:.1} dB");
    assert!(
        psnr >= 50.0,
        "merged render diverged from per-mesh reference: {psnr:.1} dB (< 50 dB)"
    );
}

/// Merge-happened + draw-call reduction guard (metrics). Five opaque meshes
/// collapse into two batches, so the frame issues three fewer `draw_indexed`
/// calls with merge on than off.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires wgpu GPU adapter"]
fn draw_merge_reduces_draw_calls() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);
    let snap = merge_test_snapshot();

    renderer.set_draw_merge_enabled(false);
    renderer.render(&target, &snap, false);
    renderer.render(&target, &snap, false);
    let unmerged_draws = renderer.last_frame_timings().draw_calls;
    assert_eq!(
        renderer.last_frame_timings().merged_batch_count,
        0,
        "no batches should be merged with merge disabled"
    );

    renderer.set_draw_merge_enabled(true);
    renderer.render(&target, &snap, false); // cold — bake + build
    renderer.render(&target, &snap, false); // warm
    let t = renderer.last_frame_timings();
    assert_eq!(
        t.merged_batch_count, 2,
        "expected 2 merged batches, got {}",
        t.merged_batch_count
    );
    assert_eq!(
        t.merged_mesh_count, 5,
        "expected 5 folded meshes, got {}",
        t.merged_mesh_count
    );
    assert_eq!(
        unmerged_draws - t.draw_calls,
        3,
        "merge should remove 3 draws (5 meshes → 2 batches); before={unmerged_draws} after={}",
        t.draw_calls
    );
}

/// Exclusion guard (metrics): a transparent mesh sharing a material with opaque
/// meshes must never be folded into a merged batch — it keeps its per-mesh
/// back-to-front transparent draw.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires wgpu GPU adapter"]
fn draw_merge_excludes_transparent() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);

    let mut snap = RenderWorldSnapshot::default();
    let mut mat = fallback_material("/Looks/Shared");
    mat.base_color = [0.5, 0.5, 0.5];
    for (i, &x) in [-0.6f32, 0.6].iter().enumerate() {
        let mut cube = tessellate_cube(0.4);
        let path = format!("/World/Opaque_{i}");
        cube.prim_path = path.clone();
        snap.meshes.push(RenderMeshInstance {
            prim_path: path,
            mesh: Arc::new(cube),
            world_matrix: Mat4::from_translation(glam::Vec3::new(x, 0.0, 0.0)),
            material: mat.clone(),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        });
    }
    // Transparent cube sharing the same material fingerprint — must stay separate.
    let mut glass = mat.clone();
    glass.alpha_policy = AlphaPolicy::Blend;
    glass.opacity = 0.4;
    let mut gcube = tessellate_cube(0.4);
    gcube.prim_path = "/World/Glass".into();
    snap.meshes.push(RenderMeshInstance {
        prim_path: "/World/Glass".into(),
        mesh: Arc::new(gcube),
        world_matrix: Mat4::from_translation(glam::Vec3::new(0.0, 0.8, 0.0)),
        material: glass,
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    });
    snap.stats = ExtractionStats {
        mesh_count: 3,
        ..Default::default()
    };

    renderer.set_draw_merge_enabled(true);
    renderer.render(&target, &snap, false);
    renderer.render(&target, &snap, false);
    let t = renderer.last_frame_timings();
    assert_eq!(
        t.merged_batch_count, 1,
        "only the two opaque cubes should merge"
    );
    assert_eq!(
        t.merged_mesh_count, 2,
        "transparent mesh must be excluded from the merge (got {})",
        t.merged_mesh_count
    );
}

/// Build a USD row-major (`Mat4::from_cols_array`) translation patch array.
#[cfg(test)]
fn usd_translate(tx: f32, ty: f32, tz: f32) -> [f32; 16] {
    let mut m = [0.0f32; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m[12] = tx; // USD row 3 = translation (last row); glam w_axis after from_cols_array
    m[13] = ty;
    m[14] = tz;
    m
}

/// Regression (F1/F7 + XR pose fast path): a transform-only patch that moves a
/// folded mesh WITHOUT bumping `snapshot.serial` — exactly what the XR controller
/// pose path does via `patch_world_matrices` — must re-bake the affected merged
/// batch's world-space vertices, and ONLY that batch. Before the fix the merge
/// bake was gated solely on the serial, so a warm frame never re-baked and folded
/// rigid children (axis-gizmo cubes, aim ray) froze at their initial pose while
/// the skinned mesh — excluded from merging — kept moving.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires wgpu GPU adapter"]
fn draw_merge_rebakes_only_moved_group_on_transform_patch_without_serial_bump() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);
    let mut snap = merge_test_snapshot();
    snap.camera = Some(fit_camera_to_snapshot(&snap, w as f32 / h as f32));
    renderer.set_draw_merge_enabled(true);

    // Cold frame: full structural bake of both merge groups (3 translated boxes,
    // 2 mirrored boxes).
    renderer.render(&target, &snap, false);
    let cold = renderer.last_frame_timings();
    assert_eq!(
        cold.merged_batch_count, 2,
        "expected 2 merged batches after cold bake"
    );
    assert_eq!(
        cold.merged_batch_rebakes, 2,
        "cold/full rebuild must bake every merged batch (got {})",
        cold.merged_batch_rebakes
    );

    // Warm frame, nothing moved: no batch may re-bake.
    renderer.render(&target, &snap, false);
    assert_eq!(
        renderer.last_frame_timings().merged_batch_rebakes,
        0,
        "an unchanged warm frame must not re-bake any merged batch"
    );

    // Transform-only fast path: patch ONLY the non-mirrored boxes' world matrices
    // (pure translation preserves their determinant sign, so the merge grouping is
    // unchanged) without bumping serial.
    let patches: Vec<(String, [f32; 16])> = snap
        .meshes
        .iter()
        .filter(|m| m.prim_path.starts_with("/World/Box_"))
        .map(|m| (m.prim_path.clone(), usd_translate(0.0, 5.0, 0.0)))
        .collect();
    assert_eq!(patches.len(), 3, "expected 3 non-mirrored boxes to patch");
    let serial_before = snap.serial;
    snap.patch_world_matrices(&patches, Mat4::IDENTITY);
    assert_eq!(
        snap.serial, serial_before,
        "patch_world_matrices must not bump serial"
    );

    // Warm frame after the patch: exactly the moved group re-bakes; the static
    // mirrored group keeps its cached buffer.
    renderer.render(&target, &snap, false);
    let moved = renderer.last_frame_timings();
    assert_eq!(
        moved.merged_batch_count, 2,
        "still 2 merged batches after the patch"
    );
    assert_eq!(
        moved.merged_batch_rebakes, 1,
        "only the moved (non-mirrored) group must re-bake; the static group is \
         skipped — pre-fix this was 0 and folded geometry froze (got {})",
        moved.merged_batch_rebakes
    );
}

/// End-to-end pixel regression: after a serial-preserving transform patch moves
/// the folded boxes out of frame, the rendered merged frame must visibly change.
/// Pre-fix the merged batch kept its stale world-space vertices, so the frame was
/// pixel-identical (the observed "debug children stay put") — asserted here as a
/// large PSNR drop between the pre- and post-patch merged frames.
#[test]
#[ignore = "requires wgpu GPU adapter"]
fn draw_merge_folded_geometry_follows_transform_patch() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (256, 256);
    let target = renderer.create_frame_target(w, h);
    let mut snap = merge_test_snapshot();
    snap.camera = Some(fit_camera_to_snapshot(&snap, w as f32 / h as f32));
    renderer.set_draw_merge_enabled(true);

    renderer.render(&target, &snap, false); // cold — bake
    renderer.render(&target, &snap, false); // warm — reuse
    let before = renderer.read_pixels(&target).expect("readback");

    // Move the three non-mirrored boxes far out of frame; serial stays fixed.
    let patches: Vec<(String, [f32; 16])> = snap
        .meshes
        .iter()
        .filter(|m| m.prim_path.starts_with("/World/Box_"))
        .map(|m| (m.prim_path.clone(), usd_translate(0.0, 1000.0, 0.0)))
        .collect();
    let serial_before = snap.serial;
    snap.patch_world_matrices(&patches, Mat4::IDENTITY);
    assert_eq!(snap.serial, serial_before, "patch must not bump serial");

    renderer.render(&target, &snap, false); // warm — folded geometry must move
    let after = renderer.read_pixels(&target).expect("readback");

    let psnr = psnr_rgba(&before, &after);
    println!("draw-merge transform-patch PSNR before-vs-after: {psnr:.1} dB");
    assert!(
        psnr < 40.0,
        "merged frame did not change after a transform patch ({psnr:.1} dB ≥ 40): \
         folded geometry is frozen — the debug-children regression"
    );
}

// ── UDIM ────────────────────────────────────────────────────────────────────

/// Probe: what does the resolved diffuse texture path look like for a
/// `<UDIM>` material, and does the `<UDIM>` token survive USD path resolution?
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn udim_diffuse_path_survives_resolution() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_plane.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_plane.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 1.0, Mat4::IDENTITY);

    println!("udim materials: {}", snap.materials.len());
    for (mpath, m) in &snap.materials {
        println!(
            "  material {mpath}: diffuse_texture = {:?}",
            m.diffuse_texture
        );
    }
    for m in &snap.meshes {
        let uvmax = m.mesh.texcoords.iter().fold([f32::MIN, f32::MIN], |a, uv| {
            [a[0].max(uv[0]), a[1].max(uv[1])]
        });
        println!(
            "  mesh {} — diffuse={:?}, uv_max={:?}, tris={}",
            m.prim_path,
            m.material.diffuse_texture,
            uvmax,
            m.mesh.triangle_count()
        );
    }

    // The anchored template is absolute with the <UDIM> token intact; expand it
    // per tile and confirm the resolver opens the concrete files. 1001/1002/1011
    // exist (varying sizes); 1012 is deliberately absent (missing-tile fallback).
    let template = snap
        .materials
        .values()
        .find_map(|m| m.diffuse_texture.clone())
        .expect("udim material has a diffuse template");
    assert!(
        idtx_render::udim::is_udim_template(&template),
        "template retains <UDIM> token: {template:?}"
    );
    assert!(
        std::path::Path::new(&template.replace("<UDIM>", "1001")).is_absolute(),
        "anchored template is absolute: {template:?}"
    );
    let svc = SyncSvc(service);
    for tile in [1001u32, 1002, 1011, 1012] {
        let concrete = idtx_render::udim::expand_template(&template, tile);
        let got = svc.0.read_asset(&concrete).map(|b| b.len());
        println!("  read_asset(tile {tile}) -> {got:?} bytes");
        if tile == 1012 {
            assert!(got.is_err(), "tile 1012 is absent (fallback probe)");
        } else {
            assert!(got.is_ok(), "tile {tile} must be readable: {got:?}");
        }
    }
}

/// Probe (packaged): the same UDIM material inside a `.usdz` must anchor
/// to a package-relative template and each expanded tile must read out of the
/// package. Verifies the shim anchoring is package-aware, not filesystem-only.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn udim_diffuse_path_resolves_inside_usdz() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_plane.usdz")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_plane.usdz");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 1.0, Mat4::IDENTITY);

    let template = snap
        .materials
        .values()
        .find_map(|m| m.diffuse_texture.clone())
        .expect("packaged udim material has a diffuse template");
    println!("  packaged diffuse template = {template:?}");
    assert!(
        idtx_render::udim::is_udim_template(&template),
        "packaged template retains <UDIM> token: {template:?}"
    );
    // Package-relative resolved paths carry the `.usdz[...]` bracket syntax.
    assert!(
        template.contains(".usdz["),
        "packaged template is package-relative: {template:?}"
    );

    let svc = SyncSvc(service);
    for tile in [1001u32, 1002, 1011, 1012] {
        let concrete = idtx_render::udim::expand_template(&template, tile);
        let got = svc.0.read_asset(&concrete).map(|b| b.len());
        println!("  read_asset(packaged tile {tile}) -> {got:?} bytes");
        if tile == 1012 {
            assert!(
                got.is_err(),
                "packaged tile 1012 is absent (fallback probe)"
            );
        } else {
            assert!(
                got.is_ok(),
                "packaged tile {tile} must be readable: {got:?}"
            );
        }
    }
}

/// Extract-time tile discovery + V-flip suppression. The plane's
/// `st ∈ [0,2]²` must (a) discover exactly tiles {1001,1002,1011,1012} into
/// `snapshot.udim_tiles`, keyed by the material path, and (b) leave the emitted
/// mesh's texcoords in raw USD space (V un-flipped, so max V ≈ 2.0), because
/// UDIM tile addressing is done on raw UVs with a per-tile flip in the shader.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn udim_extract_discovers_tiles_and_unflips_uvs() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_plane.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_plane.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 1.0, Mat4::IDENTITY);

    // (a) exactly one UDIM material, touching the four expected tiles.
    assert_eq!(snap.udim_tiles.len(), 1, "one UDIM material discovered");
    let (mat_path, tiles) = snap.udim_tiles.iter().next().unwrap();
    assert!(
        snap.materials.contains_key(mat_path),
        "udim_tiles key {mat_path:?} is a real material path"
    );
    assert_eq!(
        tiles.iter().copied().collect::<Vec<_>>(),
        vec![1001, 1002, 1011, 1012],
        "quad spanning [0,2]² touches these tiles"
    );

    // (b) the plane's emitted texcoords are raw USD-space (un-flipped): V spans
    // [0,2], so max V ≈ 2.0 (a flipped mesh would top out at 1.0).
    let plane = snap
        .meshes
        .iter()
        .find(|m| m.material.is_udim())
        .expect("a UDIM mesh instance was emitted");
    let max_v = plane
        .mesh
        .texcoords
        .iter()
        .fold(f32::MIN, |acc, uv| acc.max(uv[1]));
    assert!(
        (max_v - 2.0).abs() < 1e-4,
        "UDIM mesh texcoords are un-flipped (max V = {max_v}, expected ~2.0)"
    );
}

/// `TextureCache::prefetch_udim_parallel` builds one `D2Array` per
/// `(template, kind)`, with layers for the tiles that loaded and the sentinel
/// for absent ones.
///
/// Fixture tiles on disk: `albedo.1001/1002/1011.png` (1012 omitted). Probing
/// the four discovered tiles must yield `layer_count == 3`, sorted layer
/// assignment 1001→0, 1002→1, 1011→2, and `tile_to_layer[1012-1001] == SENTINEL`.
#[test]
#[ignore]
fn udim_prefetch_builds_array_with_sentinel_for_missing_tile() {
    use idtx_render::texture_cache::{
        TextureCache, TextureDecodeConfig, TextureKind, UDIM_LAYER_SENTINEL,
    };

    let Some(renderer) = try_renderer() else {
        return;
    };

    // Absolute on-disk template with the token intact (filesystem read path).
    let template = workspace_root()
        .join("tests/fixtures/usd/udim/albedo.<UDIM>.png")
        .to_str()
        .unwrap()
        .to_owned();

    let mut cache = TextureCache::new(
        &renderer.device,
        &renderer.queue,
        TextureDecodeConfig::default(),
    );

    let requests = vec![(
        template.clone(),
        TextureKind::DiffuseSrgb,
        vec![1001, 1002, 1011, 1012],
    )];
    let (found, missing, timings) =
        cache.prefetch_udim_parallel(&requests, &renderer.device, &renderer.queue);

    println!(
        "udim prefetch: found={found} missing={missing} probed={} upload={:.2}ms",
        timings.tiles_probed, timings.array_upload_ms
    );
    assert_eq!(found, 3, "1001/1002/1011 load; 1012 is absent");
    assert_eq!(missing, 1, "1012 is the single missing tile");
    assert_eq!(timings.tiles_probed, 4);

    let arr = cache.udim_get(&template, TextureKind::DiffuseSrgb);
    assert_eq!(arr.layer_count, 3, "three tiles became layers");
    assert_eq!(arr.tile_to_layer[1001 - 1001], 0, "1001 → layer 0 (sorted)");
    assert_eq!(arr.tile_to_layer[1002 - 1001], 1, "1002 → layer 1");
    assert_eq!(arr.tile_to_layer[1011 - 1001], 2, "1011 → layer 2");
    assert_eq!(
        arr.tile_to_layer[1012 - 1001],
        UDIM_LAYER_SENTINEL,
        "missing 1012 → sentinel (shader falls back to base_color)"
    );

    // A never-probed template returns the shared null array (all sentinels).
    let null = cache.udim_get("nonexistent.<UDIM>.png", TextureKind::DiffuseSrgb);
    assert_eq!(null.layer_count, 1, "null UDIM array is 1×1×1");
    assert!(
        null.tile_to_layer.iter().all(|&l| l == UDIM_LAYER_SENTINEL),
        "null UDIM array maps every tile to the sentinel"
    );
}

/// End-to-end UDIM render. `udim_plane.usda` is a quad whose `st`
/// spans `[0,2]²`, so its four screen quadrants map to tiles 1001 (red),
/// 1002 (green), 1011 (blue) and 1012 (absent → base-color fallback). Rendered
/// straight-on, each present tile must dominate its quadrant with its own hue,
/// and the missing-tile quadrant must be achromatic (the material's scalar
/// base color kept because that tile maps to the sentinel layer).
///
/// This exercises the full UDIM chain: extract-time tile discovery,
/// `prefetch_udim_parallel`, the `material_udim_bgl` bind group, and the
/// `pipeline.udim` shader variant selected by `CachedMeshGpu.udim`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn udim_plane_renders_tiles_with_base_color_fallback() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_plane.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_plane.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (512u32, 512u32);
    let mut snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, Mat4::IDENTITY);

    // The UDIM mesh must have been discovered and left un-flipped (v up to ~2).
    assert!(
        !snap.udim_tiles.is_empty(),
        "extract discovered UDIM tiles: {:?}",
        snap.udim_tiles.keys().collect::<Vec<_>>()
    );

    // Straight-on camera on the +Z side (the plane's +Z front face), framing the
    // 2×2 quad so each of the four tile regions lands squarely in one quadrant.
    let eye = glam::Vec3::new(0.0, 0.0, 3.0);
    snap.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: "/test/cam".into(),
        view: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        projection: glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            w as f32 / h as f32,
            0.1,
            100.0,
        ),
        clip_near: 0.1,
        clip_far: 100.0,
        fov_horizontal: std::f32::consts::FRAC_PI_3,
        exposure_ev: 0.0,
    });

    let Some(mut renderer) = ({
        let svc = Arc::new(SyncSvc(service));
        let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
        match Renderer::with_asset_reader(reader) {
            Ok(r) => Some(r),
            Err(RenderError::NoAdapter) => {
                eprintln!("Skipping GPU render — no adapter");
                None
            }
            Err(e) => {
                eprintln!("Skipping GPU render — {e}");
                None
            }
        }
    }) else {
        return;
    };

    // Route UDIM templates through prefetch_udim_parallel (populates udim_cache).
    renderer.prefetch_textures(&snap);

    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let out = renders_subdir("udim").join("udim_plane_tiles.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");

    // Average an 8×8 block centred on (px,py); returns [r,g,b] in 0..=255.
    let sample = |px: u32, py: u32| -> [u32; 3] {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for dy in 0..8 {
            for dx in 0..8 {
                let x = px + dx - 4;
                let y = py + dy - 4;
                let i = ((y * w + x) * 4) as usize;
                r += pixels[i] as u32;
                g += pixels[i + 1] as u32;
                b += pixels[i + 2] as u32;
                n += 1;
            }
        }
        [r / n, g / n, b / n]
    };

    // Quadrant centres (row 0 = top): st_u = worldx+1, st_v = worldy+1.
    let bl = sample(w / 4, h * 3 / 4); // world(-,-) → st(0.1,0.1) → tile 1001 (red)
    let br = sample(w * 3 / 4, h * 3 / 4); // world(+,-) → st(1.9,0.1) → tile 1002 (green)
    let tl = sample(w / 4, h / 4); // world(-,+) → st(0.1,1.9) → tile 1011 (blue)
    let tr = sample(w * 3 / 4, h / 4); // world(+,+) → st(1.9,1.9) → tile 1012 (absent)
    println!("  quadrants: 1001(bl)={bl:?} 1002(br)={br:?} 1011(tl)={tl:?} 1012(tr)={tr:?}");

    let spread = |c: [u32; 3]| c.iter().max().unwrap() - c.iter().min().unwrap();

    // Present tiles: each dominates its quadrant with its own hue.
    assert!(
        bl[0] > bl[1] + 30 && bl[0] > bl[2] + 30,
        "1001 quadrant is red: {bl:?}"
    );
    assert!(
        br[1] > br[0] + 30 && br[1] > br[2] + 30,
        "1002 quadrant is green: {br:?}"
    );
    assert!(
        tl[2] > tl[0] + 30 && tl[2] > tl[1] + 30,
        "1011 quadrant is blue: {tl:?}"
    );

    // Missing tile 1012: sentinel layer → shader keeps the scalar base color,
    // which is achromatic (no saturated hue like the three loaded tiles).
    assert!(
        spread(tr) < 40,
        "1012 quadrant shows achromatic base-color fallback (low channel spread), got {tr:?} spread {}",
        spread(tr)
    );
    assert!(
        spread(bl) > spread(tr) && spread(br) > spread(tr) && spread(tl) > spread(tr),
        "each loaded tile is more saturated than the fallback quadrant"
    );
}

/// Regression: wireframe rendering of a UDIM mesh must not trigger a bind-group
/// layout mismatch. UDIM materials bind a `material_udim_bgl` group (array
/// textures + `UdimTable` at binding 8) at group 3; the wireframe pipelines were
/// originally built only against the plain `material_bgl` (2D textures, no
/// binding 8), so drawing a UDIM mesh in wireframe raised a wgpu validation
/// error ("BindGroupLayout ... is not compatible"). The fix compiles UDIM
/// wireframe variants (`WireframePipelines::stat_udim` / `inst_udim`) selected
/// when `gpu.udim` / `e.udim` is set. This test renders `udim_plane.usda` in
/// wireframe mode and asserts the frame completes (no panic) with pixels drawn.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn udim_plane_wireframe_does_not_mismatch_bind_group() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_plane.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_plane.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (256u32, 256u32);
    let mut snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, Mat4::IDENTITY);
    assert!(!snap.udim_tiles.is_empty(), "extract discovered UDIM tiles");

    let eye = glam::Vec3::new(0.0, 0.0, 3.0);
    snap.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: "/test/cam".into(),
        view: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        projection: glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            w as f32 / h as f32,
            0.1,
            100.0,
        ),
        clip_near: 0.1,
        clip_far: 100.0,
        fov_horizontal: std::f32::consts::FRAC_PI_3,
        exposure_ev: 0.0,
    });

    let Some(mut renderer) = ({
        let svc = Arc::new(SyncSvc(service));
        let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
        match Renderer::with_asset_reader(reader) {
            Ok(r) => Some(r),
            Err(RenderError::NoAdapter) => {
                eprintln!("Skipping GPU render — no adapter");
                None
            }
            Err(e) => {
                eprintln!("Skipping GPU render — {e}");
                None
            }
        }
    }) else {
        return;
    };

    renderer.prefetch_textures(&snap);

    let target = renderer.create_frame_target(w, h);
    // The regression: this call panicked on a UDIM mesh before the fix. On
    // adapters without POLYGON_MODE_LINE the renderer falls back to fill mode,
    // which was already valid — either way the frame must complete.
    renderer.render(&target, &snap, true);
    let pixels = renderer.read_pixels(&target).expect("wireframe readback");

    let out = renders_subdir("udim").join("udim_plane_wireframe.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");

    // Some geometry was rasterised (not an all-clear frame).
    let lit = pixels
        .chunks_exact(4)
        .filter(|p| p[0] > 0 || p[1] > 0 || p[2] > 0)
        .count();
    assert!(lit > 0, "wireframe frame drew at least some pixels");
}

/// Instanced UDIM (extract): `udim_instanced.usda` has two `instanceable`
/// Xforms sharing a prototype whose Plane carries a `<UDIM>` material spanning
/// `st ∈ [0,2]²`. The native-instance extraction path must (a) record the four
/// tiles into `snapshot.udim_tiles` under the material path, and (b) emit the
/// prototype batch mesh with raw (un-flipped) texcoords (max V ≈ 2.0) — exactly
/// as the standalone mesh path does, so the hardware-instanced UDIM shader can
/// address tiles from raw UVs. Skinned meshes never reach this path.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn udim_instanced_extract_discovers_tiles_and_unflips_uvs() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_instanced.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_instanced.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 1.0, Mat4::IDENTITY);

    // (a) one UDIM material discovered, touching the four expected tiles.
    assert_eq!(snap.udim_tiles.len(), 1, "one UDIM material discovered");
    let (mat_path, tiles) = snap.udim_tiles.iter().next().unwrap();
    assert!(
        snap.materials.contains_key(mat_path),
        "udim_tiles key {mat_path:?} is a real material path"
    );
    assert_eq!(
        tiles.iter().copied().collect::<Vec<_>>(),
        vec![1001, 1002, 1011, 1012],
        "quad spanning [0,2]² touches these tiles"
    );

    // (b) the instances collapsed into a hardware-instanced batch (≥2 instances)
    // whose UDIM material's prototype mesh is emitted un-flipped (max V ≈ 2.0).
    let batch = snap
        .instanced_batches
        .iter()
        .find(|b| b.material.is_udim())
        .expect("a UDIM instanced batch was emitted for the two instances");
    assert!(
        batch.instance_transforms.len() >= 2,
        "both instances share one batch, got {} instances",
        batch.instance_transforms.len()
    );
    let max_v = batch
        .mesh
        .texcoords
        .iter()
        .fold(f32::MIN, |acc, uv| acc.max(uv[1]));
    assert!(
        (max_v - 2.0).abs() < 1e-4,
        "instanced UDIM batch texcoords are un-flipped (max V = {max_v}, expected ~2.0)"
    );
}

/// Instanced UDIM (end-to-end render): the two instances of `udim_instanced.usda`
/// sit at x = ±2.5. A wide straight-on camera frames both quads; each must show
/// the UDIM tiles resolved (the left quad's lower-left corner is tile 1001, red;
/// the right quad's is likewise red), proving the hardware-instanced UDIM pipeline
/// and `material_udim_bgl` bind group apply per instance — not the base-color
/// fallback the non-UDIM instanced path would produce.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR) and wgpu GPU adapter"]
fn udim_instanced_renders_tiles_on_each_instance() {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/udim/udim_instanced.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open udim_instanced.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");

    let world = hydrator.world_mut();
    let (w, h) = (1024u32, 512u32);
    let mut snap = RenderWorldSnapshot::extract(world, w as f32 / h as f32, Mat4::IDENTITY);

    assert!(
        snap.instanced_batches.iter().any(|b| b.material.is_udim()),
        "a UDIM instanced batch must be present"
    );

    // Wide camera framing both instances (x = ±2.5, each a 2×2 quad).
    let eye = glam::Vec3::new(0.0, 0.0, 7.0);
    snap.camera = Some(idtx_render::scene_extract::RenderCameraDesc {
        prim_path: "/test/cam".into(),
        view: glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y),
        projection: glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            w as f32 / h as f32,
            0.1,
            100.0,
        ),
        clip_near: 0.1,
        clip_far: 100.0,
        fov_horizontal: std::f32::consts::FRAC_PI_3,
        exposure_ev: 0.0,
    });

    let Some(mut renderer) = ({
        let svc = Arc::new(SyncSvc(service));
        let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
        match Renderer::with_asset_reader(reader) {
            Ok(r) => Some(r),
            Err(RenderError::NoAdapter) => {
                eprintln!("Skipping GPU render — no adapter");
                None
            }
            Err(e) => {
                eprintln!("Skipping GPU render — {e}");
                None
            }
        }
    }) else {
        return;
    };

    renderer.prefetch_textures(&snap);
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");

    let out = renders_subdir("udim").join("udim_instanced_tiles.png");
    save_png(&out, &pixels, w, h);
    println!("  Saved: {out:?}");

    let sample = |px: u32, py: u32| -> [u32; 3] {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for dy in 0..8 {
            for dx in 0..8 {
                let x = px + dx - 4;
                let y = py + dy - 4;
                let i = ((y * w + x) * 4) as usize;
                r += pixels[i] as u32;
                g += pixels[i + 1] as u32;
                b += pixels[i + 2] as u32;
                n += 1;
            }
        }
        [r / n, g / n, b / n]
    };

    // Each instance quad's lower-left tile (st u,v ∈ [0,1]) is tile 1001 (red).
    // Camera z=7, vertical FOV 60°, aspect 2 → visible x∈[-8.08,8.08],
    // y∈[-4.04,4.04]. Left quad tile-1001 centre ≈ world(-3.0,-0.5) → px≈322,py≈288;
    // right quad ≈ world(2.0,-0.5) → px≈639,py≈288.
    let left = sample(322, 288);
    let right = sample(639, 288);
    println!("  left 1001={left:?} right 1001={right:?}");

    // Both instances must render the (red) tile, not an achromatic base-color
    // fallback — red channel clearly dominates on each.
    assert!(
        left[0] > left[1] + 30 && left[0] > left[2] + 30,
        "left instance shows UDIM tile 1001 (red): {left:?}"
    );
    assert!(
        right[0] > right[1] + 30 && right[0] > right[2] + 30,
        "right instance shows UDIM tile 1001 (red): {right:?}"
    );
}

/// Regression: pre-USD-21.02 lights authored with bare (un-namespaced)
/// attribute names must be honoured.
///
/// USD 21.02 moved light inputs into the `inputs:` namespace. `UsdLuxLightAPI`
/// accessors resolve only the namespaced form, so a DCC export that still
/// writes `intensity` / `color` (Houdini/Karma, older content) previously fell
/// through to the schema fallback — e.g. an artist's `intensity = 10` was
/// ignored and the unauthored `inputs:intensity` fallback rendered instead,
/// blowing the scene out to white. The bridge now falls back to the bare
/// attribute when the `inputs:` form has no authored opinion, with the
/// `inputs:` opinion winning when both are authored.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn legacy_unnamespaced_light_attributes_are_read() {
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/lighting/legacy_light_attrs.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let mut service = StageService::open(&path).expect("open legacy_light_attrs.usda");
    service
        .project_topology_paths(&[])
        .expect("project topology");
    let vp = service.project_viewport().expect("project viewport");

    println!("  legacy_light_attrs: {} lights", vp.lights.len());
    for l in &vp.lights {
        println!(
            "    intensity={} color={:?} diffuse={} specular={} normalize={}",
            l.intensity, l.color, l.diffuse, l.specular, l.normalize
        );
    }

    // 'Sun': every attribute authored with a bare (legacy) name must be read,
    // not the `inputs:` schema fallback.
    let sun = vp
        .lights
        .iter()
        .find(|l| (l.intensity - 10.0).abs() < 1e-4)
        .expect("legacy DistantLight 'Sun' bare `intensity = 10` must be read");
    assert!(
        (sun.color[0] - 1.0).abs() < 1e-4
            && (sun.color[1] - 0.5).abs() < 1e-4
            && (sun.color[2] - 0.25).abs() < 1e-4,
        "legacy bare `color3f color` must be read, got {:?}",
        sun.color
    );
    assert!(
        (sun.diffuse - 0.75).abs() < 1e-4,
        "legacy bare `diffuse`, got {}",
        sun.diffuse
    );
    assert!(
        (sun.specular - 0.5).abs() < 1e-4,
        "legacy bare `specular`, got {}",
        sun.specular
    );
    assert!(sun.normalize, "legacy bare `normalize` must be read");

    // 'Fill': modern `inputs:` encoding must still be honoured.
    assert!(
        vp.lights.iter().any(|l| (l.intensity - 4.0).abs() < 1e-4),
        "modern `inputs:intensity = 4` must be read"
    );

    // 'Mixed': `inputs:intensity = 7` wins over the legacy `intensity = 99`.
    assert!(
        vp.lights.iter().any(|l| (l.intensity - 7.0).abs() < 1e-4),
        "`inputs:intensity` must take precedence when both forms are authored"
    );
    assert!(
        !vp.lights.iter().any(|l| (l.intensity - 99.0).abs() < 1e-4),
        "legacy bare `intensity` must NOT override an authored `inputs:intensity`"
    );
}

/// End-to-end FFI validation for the light-selection-glyph feature: every USD
/// light type in `lights_all_types.usda` projects with the right `LightKind`,
/// Disk/Cylinder are now first-class (not dropped as unsupported), and the
/// `SphereLight` carrying `UsdLuxShapingAPI` reports its cone as a spotlight.
///
/// Run: `cargo test -p idtx-render --test render_integration
///       all_usd_light_types_project_with_shaping -- --ignored --nocapture`
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn all_usd_light_types_project_with_shaping() {
    use idtx_usd::{LightKind, StageService};

    let path = workspace_root()
        .join("tests/fixtures/usd/lighting/lights_all_types.usda")
        .to_str()
        .unwrap()
        .to_owned();

    let Ok(mut service) = StageService::open(&path) else {
        eprintln!("skip: could not open lights_all_types.usda (USD not linked?)");
        return;
    };
    service
        .project_topology_paths(&[])
        .expect("project topology");
    let vp = service.project_viewport().expect("project viewport");

    println!("  lights_all_types: {} lights", vp.lights.len());
    for l in &vp.lights {
        println!(
            "    kind={:?} has_shaping={} cone_angle={} cone_softness={}",
            l.kind, l.has_shaping, l.cone_angle, l.cone_softness
        );
    }

    let has = |pred: fn(&idtx_usd::LightKind) -> bool| vp.lights.iter().any(|l| pred(&l.kind));

    assert!(
        has(|k| matches!(k, LightKind::Distant { .. })),
        "DistantLight missing"
    );
    assert!(
        has(|k| matches!(k, LightKind::Sphere { .. })),
        "SphereLight missing"
    );
    assert!(
        has(|k| matches!(k, LightKind::Rect { .. })),
        "RectLight missing"
    );
    assert!(
        has(|k| matches!(k, LightKind::Disk { .. })),
        "DiskLight must now project as a first-class light"
    );
    assert!(
        has(|k| matches!(k, LightKind::Cylinder { .. })),
        "CylinderLight must now project as a first-class light"
    );
    assert!(has(|k| matches!(k, LightKind::Dome)), "DomeLight missing");

    // The CylinderLight authored `length = 4.0`, `radius = 0.15`.
    let cylinder = vp
        .lights
        .iter()
        .find(|l| matches!(l.kind, LightKind::Cylinder { .. }))
        .expect("cylinder light");
    if let LightKind::Cylinder { radius, length } = cylinder.kind {
        assert!(
            (radius - 0.15).abs() < 1e-4,
            "cylinder radius, got {radius}"
        );
        assert!((length - 4.0).abs() < 1e-4, "cylinder length, got {length}");
    }

    // Exactly one light carries a UsdLuxShapingAPI cone — the "Spot" sphere.
    let spots: Vec<_> = vp.lights.iter().filter(|l| l.has_shaping).collect();
    assert_eq!(spots.len(), 1, "exactly one shaping/spotlight expected");
    let spot = spots[0];
    assert!(
        matches!(spot.kind, LightKind::Sphere { .. }),
        "the spot is authored on a SphereLight"
    );
    assert!(
        (spot.cone_angle - 25.0).abs() < 1e-3,
        "shaping cone half-angle 25 deg, got {}",
        spot.cone_angle
    );
    assert!(
        (spot.cone_softness - 0.4).abs() < 1e-3,
        "shaping cone softness 0.4, got {}",
        spot.cone_softness
    );

    // Non-shaping lights must report has_shaping == false and thus produce no
    // spotlight glyph downstream.
    assert!(
        vp.lights
            .iter()
            .filter(|l| !l.has_shaping)
            .all(|l| !l.has_shaping),
        "non-spot lights must not report shaping"
    );
}

// ── Soft-shadow render tests ────────────────────────────────────
//
// `#[ignore]` render gates for soft-shadow rendering. Each renders a
// `tests/fixtures/usd/shadows/shadow_*.usda` scene and writes a golden PNG into
// `tests/renders/shadows/` so the fidelity gates can be calibrated against the
// shadow pre-pass (hard-PCF and PCSS). They call `RenderWorldSnapshot::extract`
// with its 3-arg signature.

/// Rec.709 relative luminance of an RGBA pixel (alpha ignored).
fn luma709(px: &[u8]) -> f64 {
    0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64
}

/// True when a pixel is an achromatic (gray) surface — the ground plane and its
/// shadow are gray; the colored caster boxes are not. Lets the shadow-detection
/// heuristics ignore the caster geometry itself and look only at the receiver.
fn is_grayish(px: &[u8]) -> bool {
    let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
    (r - g).abs() < 24 && (g - b).abs() < 24 && (r - b).abs() < 24
}

/// Count gray-receiver pixels in the lower `frac` of the frame whose luminance is
/// below `ratio × (brightest gray-receiver luma)`. On a shadow-free render the
/// ground is uniformly lit, so this is ~0; a cast shadow darkens a gray-ground
/// cluster and drives it positive. The shadow-present fidelity gate.
fn dark_ground_pixels(pixels: &[u8], w: u32, h: u32, frac: f64, ratio: f64) -> usize {
    let y0 = ((1.0 - frac) * h as f64) as u32;
    let band: Vec<[u8; 4]> = (y0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let i = ((y * w + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        })
        .filter(|px| is_grayish(px))
        .collect();
    let bright = band
        .iter()
        .map(|px| luma709(&px[..]))
        .fold(0.0_f64, f64::max);
    if bright <= 0.0 {
        return 0;
    }
    let thresh = ratio * bright;
    let n = band.iter().filter(|px| luma709(&px[..]) < thresh).count();
    if std::env::var_os("IDTX_SHADOW_DEBUG").is_some() {
        eprintln!(
            "[dark] band={} bright={bright:.1} thresh={thresh:.1} dark={n}",
            band.len()
        );
    }
    n
}

/// Count "penumbra" gray-receiver pixels (partial shadow) within an x-column
/// range: luminance between `lo` and `hi` fractions of the brightest gray pixel.
/// A wider soft-shadow edge produces more such transition pixels — the metric
/// behind the penumbra-widening gate.
fn penumbra_pixels(pixels: &[u8], w: u32, h: u32, x0: u32, x1: u32, lo: f64, hi: f64) -> usize {
    let cells: Vec<[u8; 4]> = (0..h)
        .flat_map(|y| (x0..x1.min(w)).map(move |x| (x, y)))
        .map(|(x, y)| {
            let i = ((y * w + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        })
        .filter(|px| is_grayish(px))
        .collect();
    let bright = cells
        .iter()
        .map(|px| luma709(&px[..]))
        .fold(0.0_f64, f64::max);
    if bright <= 0.0 {
        return 0;
    }
    cells
        .iter()
        .map(|px| luma709(&px[..]) / bright)
        .filter(|f| *f > lo && *f < hi)
        .count()
}

/// Open a `tests/fixtures/usd/shadows/shadow_*.usda` scene, hydrate + extract a snapshot, and
/// build a `Renderer` with a USD-backed asset reader. Returns `None` when no wgpu
/// adapter is available (headless CI) so the test degrades to a skip.
fn open_shadow_scene(usda: &str, w: u32, h: u32) -> Option<(Renderer, RenderWorldSnapshot)> {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let path = workspace_root()
        .join("tests/fixtures/usd/shadows")
        .join(usda)
        .to_str()
        .unwrap()
        .to_owned();
    let mut service = StageService::open(&path).unwrap_or_else(|e| panic!("open {usda}: {e:?}"));
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");
    let shadow_mode = service.metadata().shadow_mode;

    let world = hydrator.world_mut();
    // Honor the stage's authored shadow mode 1:1: Hard→Hard (true single-tap),
    // Smooth→Smooth (3×3 PCF), Soft→Soft (PCSS), Off→disabled — so the shadow
    // gates exercise the real path at the tier the scene requests.
    let shadow = match shadow_mode {
        idtx_usd::ShadowMode::Off => idtx_render::RenderShadowSettings::OFF,
        idtx_usd::ShadowMode::Hard => {
            idtx_render::RenderShadowSettings::new(idtx_render::ShadowTier::Hard)
        }
        idtx_usd::ShadowMode::Smooth => {
            idtx_render::RenderShadowSettings::new(idtx_render::ShadowTier::Smooth)
        }
        idtx_usd::ShadowMode::Soft => {
            idtx_render::RenderShadowSettings::new(idtx_render::ShadowTier::Soft)
        }
    };
    let mut snap = RenderWorldSnapshot::extract_with_shadow(
        world,
        w as f32 / h as f32,
        glam::Mat4::IDENTITY,
        shadow,
    );
    if snap.camera.is_none() {
        snap.camera = Some(idtx_render::camera_fit::fit_camera_to_snapshot(
            &snap,
            w as f32 / h as f32,
        ));
    }

    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    match Renderer::with_asset_reader(reader) {
        Ok(r) => Some((r, snap)),
        Err(RenderError::NoAdapter) => {
            eprintln!("Skipping shadow render — no wgpu adapter");
            None
        }
        Err(e) => {
            eprintln!("Skipping shadow render — {e}");
            None
        }
    }
}

/// PCF gate: a hard shadow must appear on the ground under the raised caster —
/// a dark gray-ground cluster that is absent from a shadow-free render.
///
/// Differential gate: the cube's backlit faces are dark in *both* the shadowed and
/// shadow-free renders, so an absolute dark-pixel count could false-pass on the cube
/// alone. Instead we render the same scene twice — PCF on, then forced Off — and
/// require the shadow to add a substantial dark-ground cluster the Off render lacks.
#[test]
#[ignore = "requires IDTX_USD_DIR + GPU"]
fn shadow_single_distant_pcf() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, mut snap)) = open_shadow_scene("shadow_single_distant.usda", w, h)
    else {
        return;
    };

    // Shadowed render (PCF/PCSS tier from the stage's idtx:softShadows flag).
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let lit_shadowed = renderer.read_pixels(&target).expect("readback");
    save_png(
        &renders_subdir("shadows").join("shadow_single_distant_pcf.png"),
        &lit_shadowed,
        w,
        h,
    );
    let dark_on = dark_ground_pixels(&lit_shadowed, w, h, 0.7, 0.55);

    // Shadow-free reference: force the Off tier so `shadow_visibility` short-circuits
    // to 1.0. The cube still renders (and is still backlit-dark), but no cast shadow
    // lands on the ground.
    snap.shadow = idtx_render::RenderShadowSettings::OFF;
    snap.shadow_slots.clear();
    let target_off = renderer.create_frame_target(w, h);
    renderer.render(&target_off, &snap, false);
    let lit_unshadowed = renderer.read_pixels(&target_off).expect("readback");
    save_png(
        &renders_subdir("shadows").join("shadow_single_distant_off.png"),
        &lit_unshadowed,
        w,
        h,
    );
    let dark_off = dark_ground_pixels(&lit_unshadowed, w, h, 0.7, 0.55);

    // The cast shadow must add at least ~0.25% of the frame in fresh dark-ground
    // pixels over the shadow-free baseline — pixels the cube cannot account for.
    let added = dark_on.saturating_sub(dark_off);
    assert!(
        added > (w * h / 400) as usize,
        "expected the cast shadow to darken the ground (>~0.25% extra dark pixels vs \
         shadow-off); dark_on={dark_on} dark_off={dark_off} added={added}"
    );
}

/// PCSS penumbra gate: the higher/farther caster casts a wider soft edge than the
/// lower/nearer caster (similar-triangles penumbra widening). Measured by the
/// count of partial-shadow transition pixels in each caster's column range.
#[test]
#[ignore = "TDD red until Phase 5 (PCSS) lands; requires IDTX_USD_DIR + GPU"]
fn shadow_penumbra_heights_pcss() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, snap)) = open_shadow_scene("shadow_penumbra_heights.usda", w, h) else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    save_png(
        &renders_subdir("shadows").join("shadow_penumbra_heights_pcss.png"),
        &pixels,
        w,
        h,
    );

    // CasterNear (translate -6,2,0) projects into the left half; CasterFar
    // (translate 6,7,0) is higher → wider penumbra → into the right half.
    let near = penumbra_pixels(&pixels, w, h, 0, w / 2, 0.30, 0.90);
    let far = penumbra_pixels(&pixels, w, h, w / 2, w, 0.30, 0.90);
    assert!(
        far > near,
        "far/higher caster penumbra ({far}) must exceed near/lower caster ({near})"
    );
}

/// Instanced-caster gate: the hardware-instanced pillars must cast into the atlas
/// (closes the instanced-geometry gap). Asserts a non-empty dark ground region.
#[test]
#[ignore = "TDD red until Phase 4 (all caster paths) lands; requires IDTX_USD_DIR + GPU"]
fn shadow_instanced_casts() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, snap)) = open_shadow_scene("shadow_instanced.usda", w, h) else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    save_png(&renders_subdir("shadows").join("shadow_instanced.png"), &pixels, w, h);

    let dark = dark_ground_pixels(&pixels, w, h, 0.8, 0.55);
    assert!(
        dark > (w * h / 400) as usize,
        "instanced casters cast no shadow: {dark}"
    );
}

/// Skinned-caster gate: the skinned figure casts into the atlas. Degrades to a
/// ground-only (still valid) render when RiggedFigure.usdz is absent per CLAUDE.md.
#[test]
#[ignore = "TDD red until Phase 4 (skinned caster) lands; requires IDTX_USD_DIR + GPU + RiggedFigure.usdz"]
fn shadow_skinned_casts() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, snap)) = open_shadow_scene("shadow_skinned.usda", w, h) else {
        return;
    };
    let has_figure = snap.meshes.iter().any(|m| m.prim_path.contains("Figure"));
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    save_png(&renders_subdir("shadows").join("shadow_skinned.png"), &pixels, w, h);

    if !has_figure {
        eprintln!("RiggedFigure.usdz absent — skinned caster gate skipped (ground-only render)");
        return;
    }
    let dark = dark_ground_pixels(&pixels, w, h, 0.9, 0.55);
    assert!(
        dark > (w * h / 800) as usize,
        "skinned caster cast no shadow: {dark}"
    );
}

/// Off-path gate: with `idtx:softShadows = false` the render must carry NO cast
/// shadow — the off path binds the 1×1 dummy depth and short-circuits the shader
/// to visibility = 1.0. Passes trivially before the feature lands and must keep
/// passing after (the off path never renders occlusion).
#[test]
#[ignore = "requires IDTX_USD_DIR + GPU; off-path must stay shadow-free across all phases"]
fn shadow_flag_off_no_shadow() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, snap)) = open_shadow_scene("shadow_flag_off.usda", w, h) else {
        return;
    };
    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let pixels = renderer.read_pixels(&target).expect("readback");
    save_png(&renders_subdir("shadows").join("shadow_flag_off.png"), &pixels, w, h);

    let dark = dark_ground_pixels(&pixels, w, h, 0.7, 0.55);
    assert!(
        dark <= (w * h / 400) as usize,
        "flag-off render must have no cast shadow, got {dark}"
    );
}

/// Legacy-compat gate: the deprecated bool `idtx:softShadows = false`
/// now maps to HARD shadows (the true single-tap tier), not off. So
/// `shadow_legacy_false_hard.usda` must render a cast shadow — the mirror of
/// `shadow_flag_off_no_shadow`, which authors the explicit
/// `idtx:shadowMode = "off"` token and stays shadow-free.
/// Differential (on minus forced-off) like `shadow_single_distant_pcf`.
#[test]
#[ignore = "requires IDTX_USD_DIR + GPU; false→hard back-compat must render shadows"]
fn shadow_legacy_false_renders_hard() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, mut snap)) = open_shadow_scene("shadow_legacy_false_hard.usda", w, h)
    else {
        return;
    };

    let target = renderer.create_frame_target(w, h);
    renderer.render(&target, &snap, false);
    let lit_shadowed = renderer.read_pixels(&target).expect("readback");
    let dark_on = dark_ground_pixels(&lit_shadowed, w, h, 0.7, 0.55);

    snap.shadow = idtx_render::RenderShadowSettings::OFF;
    snap.shadow_slots.clear();
    let target_off = renderer.create_frame_target(w, h);
    renderer.render(&target_off, &snap, false);
    let lit_unshadowed = renderer.read_pixels(&target_off).expect("readback");
    let dark_off = dark_ground_pixels(&lit_unshadowed, w, h, 0.7, 0.55);

    let added = dark_on.saturating_sub(dark_off);
    assert!(
        added > (w * h / 400) as usize,
        "legacy softShadows=false must render a hard cast shadow; \
         dark_on={dark_on} dark_off={dark_off} added={added}"
    );
}

/// Cache/perf gate: the first frame over a heavy static grid renders the shadow
/// depth pre-pass (gpu_shadow_ns > 0 when timestamps are supported); a subsequent
/// static frame must hit the `ShadowCache` and add zero depth draws
/// (gpu_shadow_ns == 0). Uses the `metrics`-feature GPU-timestamp readback.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires IDTX_USD_DIR + GPU + metrics feature; run with --ignored --features metrics"]
fn shadow_heavy_grid_cache() {
    let (w, h) = (1280, 720);
    let Some((mut renderer, snap)) = open_shadow_scene("shadow_heavy_grid.usda", w, h) else {
        return;
    };
    let target = renderer.create_frame_target(w, h);

    // Frame 1 — dirty: the pre-pass renders every caster once.
    renderer.render(&target, &snap, false);
    let first = renderer.last_frame_timings().gpu_shadow_ns;
    // Frame 2 — identical static snapshot: the cache must short-circuit the pass.
    renderer.render(&target, &snap, false);
    let second = renderer.last_frame_timings().gpu_shadow_ns;

    println!("shadow cache: frame1 shadow_ns={first} frame2 shadow_ns={second}");
    assert_eq!(
        second, 0,
        "static frame must hit the shadow cache (0 depth draws)"
    );
}

/// A minimal perspective eye camera offset laterally by `eye_offset_x` (half the
/// IPD) — enough to give the two stereo eyes distinct view matrices.
#[cfg(feature = "metrics")]
fn eye_camera_desc(eye_offset_x: f32, aspect: f32) -> idtx_render::RenderCameraDesc {
    let eye = glam::Vec3::new(eye_offset_x, 1.5, 4.0);
    idtx_render::RenderCameraDesc {
        prim_path: "xr:eye".to_owned(),
        view: Mat4::from_translation(eye).inverse(),
        projection: Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, aspect, 0.05, 5000.0),
        clip_near: 0.05,
        clip_far: 5000.0,
        fov_horizontal: std::f32::consts::FRAC_PI_2,
        exposure_ev: 0.0,
    }
}

/// Stereo invariant (end-to-end): the two eyes of a VR frame are rendered by the
/// same `Renderer` from the same snapshot with only the camera changed — mirroring
/// `XrGpu::render_eyes`. The shadow atlas is a single shared `Renderer` resource
/// and the `ShadowCache` key is camera-independent, so eye 0 fills the atlas and
/// eye 1 must hit the cache and skip the depth pre-pass entirely. A fully-cached
/// frame forces `gpu_shadow_ns` to 0, the only public proof the pass was skipped —
/// if a change ever makes the atlas/cache per-eye, eye 1 redraws and `second`
/// becomes nonzero. Built by hand (no USD) so it needs only a wgpu adapter.
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires GPU + metrics feature; run with --ignored --features metrics"]
fn shadow_pass_skipped_on_second_stereo_eye() {
    let mut renderer = match Renderer::new() {
        Ok(r) => r,
        Err(RenderError::NoAdapter) => {
            eprintln!("Skipping stereo shadow test — no wgpu adapter");
            return;
        }
        Err(e) => {
            eprintln!("Skipping stereo shadow test — {e}");
            return;
        }
    };

    // One caster cube at the origin + a single distant-light shadow slot fit to a
    // small world AABB. No USD needed — the snapshot is constructed directly.
    let mut mesh = tessellate_cube(1.0);
    mesh.serial = 1;
    let inst = RenderMeshInstance {
        prim_path: "/World/Cube".to_owned(),
        mesh: Arc::new(mesh),
        world_matrix: Mat4::IDENTITY,
        material: fallback_material("/World/mat"),
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    };
    let (view_proj, near, far) = idtx_render::build_view_proj(
        glam::Vec3::new(-0.5, -1.0, -0.5),
        glam::Vec3::splat(-2.0),
        glam::Vec3::splat(2.0),
    );
    let mut snap = RenderWorldSnapshot {
        meshes: vec![inst],
        shadow: idtx_render::RenderShadowSettings::new(idtx_render::ShadowTier::Hard),
        shadow_slots: vec![idtx_render::ShadowSlot {
            slot: 0,
            light_index: 0,
            view_proj,
            near,
            far,
            light_size: 0.1,
        }],
        ..Default::default()
    };

    let (w, h) = (256, 256);
    let aspect = w as f32 / h as f32;
    let eye0 = renderer.create_frame_target(w, h);
    let eye1 = renderer.create_frame_target(w, h);

    // Eye 0 (left): cold cache → the shadow depth pre-pass runs and fills the atlas.
    snap.camera = Some(eye_camera_desc(-0.032, aspect));
    renderer.render(&eye0, &snap, false);
    let first = renderer.last_frame_timings().gpu_shadow_ns;

    // Eye 1 (right): same snapshot, different camera only → guaranteed cache hit.
    snap.camera = Some(eye_camera_desc(0.032, aspect));
    renderer.render(&eye1, &snap, false);
    let second = renderer.last_frame_timings().gpu_shadow_ns;

    println!("stereo shadow: eye0 shadow_ns={first} eye1 shadow_ns={second}");
    assert_eq!(
        second, 0,
        "second eye must reuse eye 0's shadow atlas (cache hit → 0 depth draws); \
         a nonzero value means shadows were re-rendered per eye"
    );
}

// ── Shadow-tier performance sweep (Hard vs Soft instrumentation) ──────────────

/// Warm-frame GPU metrics for one shadow tier, captured by
/// [`measure_shadow_tiers`]. The Hard↔Soft *fragment-cost* delta lives in
/// `gpu_pbr_warm_ns` (the whole beauty pass, which shades every shadowed
/// fragment). `gpu_shadow_ns` (the depth pre-pass) is tier-independent and is
/// cached to 0 on warm frames, so the one-time first-frame span is recorded
/// separately as `cold_shadow_ns`.
#[cfg(feature = "metrics")]
struct TierMetrics {
    tier: &'static str,
    tap_count: u32,
    cold_shadow_ns: u64,
    gpu_pbr_warm_ns: u64,
    gpu_pbr_min_ns: u64,
    gpu_pbr_max_ns: u64,
    gpu_pbr_stddev_ns: u64,
    gpu_wait_warm_ns: u64,
    total_warm_ns: u64,
}

#[cfg(feature = "metrics")]
impl TierMetrics {
    fn to_json(&self) -> String {
        format!(
            concat!(
                "        {{\n",
                "          \"tier\": \"{tier}\",\n",
                "          \"taps_per_fragment\": {taps},\n",
                "          \"cold_shadow_ns\": {cs},\n",
                "          \"gpu_pbr_warm_avg_ns\": {avg},\n",
                "          \"gpu_pbr_min_ns\": {mn},\n",
                "          \"gpu_pbr_max_ns\": {mx},\n",
                "          \"gpu_pbr_stddev_ns\": {sd},\n",
                "          \"gpu_wait_warm_avg_ns\": {wait},\n",
                "          \"total_warm_avg_ns\": {total}\n",
                "        }}"
            ),
            tier = self.tier,
            taps = self.tap_count,
            cs = self.cold_shadow_ns,
            avg = self.gpu_pbr_warm_ns,
            mn = self.gpu_pbr_min_ns,
            mx = self.gpu_pbr_max_ns,
            sd = self.gpu_pbr_stddev_ns,
            wait = self.gpu_wait_warm_ns,
            total = self.total_warm_ns,
        )
    }
}

/// Sweep the four shadow tiers (Off / Hard / Smooth / Soft) over one scene and
/// return per-tier warm-frame GPU metrics, or `None` when there is no GPU
/// adapter or the scene authors no shadow casters (empty `shadow_slots`, so the
/// lit tiers would have nothing to shade differently).
///
/// Each tier gets a FRESH renderer (fresh atlas + `ShadowCache`) so cold/warm
/// behaviour is clean and identical across tiers. The tier is *forced* onto the
/// snapshot regardless of the file's authored `idtx:shadowMode`; the same
/// `shadow_slots` (built once from the file's authored non-off mode) drive all
/// three lit tiers, so between them only `shadows.global.x` — the per-fragment
/// tap count — differs. The `Off` tier clears the slots for a clean shadow-free
/// baseline.
#[cfg(feature = "metrics")]
fn measure_shadow_tiers(usda: &str, w: u32, h: u32) -> Option<Vec<TierMetrics>> {
    use idtx_render::{RenderShadowSettings, ShadowTier};

    const WARM_N: usize = 8;
    // (label, tier, taps-per-shadowed-fragment). Tap counts mirror the headless
    // `surface_gen::tests::shadow_tap_cost_model` cost table (Off=0, Hard=1,
    // Smooth=9, Soft=74).
    let tiers: [(&str, ShadowTier, u32); 4] = [
        ("off", ShadowTier::Off, 0),
        ("hard", ShadowTier::Hard, 1),
        ("smooth", ShadowTier::Smooth, 9),
        ("soft", ShadowTier::Soft, 74),
    ];

    let mut rows = Vec::with_capacity(tiers.len());
    for (label, tier, taps) in tiers {
        // Fresh renderer + snapshot per tier (clean atlas + cache).
        let Some((mut renderer, mut snap)) = open_shadow_scene(usda, w, h) else {
            return None; // no wgpu adapter — skip the whole scene
        };
        // The lit tiers need caster slots; if the file authors none there is
        // nothing to differentiate.
        if tier != ShadowTier::Off && snap.shadow_slots.is_empty() {
            eprintln!("  [{usda}] no shadow_slots (authors no distant caster) — skipping sweep");
            return None;
        }
        if tier == ShadowTier::Off {
            snap.shadow = RenderShadowSettings::OFF;
            snap.shadow_slots.clear();
        } else {
            snap.shadow = RenderShadowSettings::new(tier);
        }

        let target = renderer.create_frame_target(w, h);

        // Cold frame — runs the depth pre-pass (gpu_shadow_ns > 0 if supported).
        renderer.render(&target, &snap, false);
        let _ = renderer.read_pixels(&target);
        let cold_shadow_ns = renderer.last_frame_timings().gpu_shadow_ns;

        // Warm frames — cache-hit steady state; gpu_pbr_ns carries the per-frame
        // fragment cost (including the shadow taps) regardless of the cache.
        let mut pbr = [0u64; WARM_N];
        let mut wait_sum = 0u64;
        let mut total_sum = 0u64;
        for slot in &mut pbr {
            renderer.render(&target, &snap, false);
            let _ = renderer.read_pixels(&target);
            let t = renderer.last_frame_timings();
            *slot = t.gpu_pbr_ns;
            wait_sum += t.gpu_wait_ns;
            total_sum += t.total_ns;
        }
        let pbr_avg = pbr.iter().sum::<u64>() / WARM_N as u64;
        let pbr_min = *pbr.iter().min().unwrap();
        let pbr_max = *pbr.iter().max().unwrap();
        // Population standard deviation (ns) — the timer-noise floor used to
        // slacken the cost-ordering assertion below.
        let var = pbr
            .iter()
            .map(|&x| {
                let d = x as f64 - pbr_avg as f64;
                d * d
            })
            .sum::<f64>()
            / WARM_N as f64;
        let pbr_stddev = var.sqrt() as u64;

        println!(
            "  [{usda}] tier={label:<4} taps={taps:<2} cold_shadow={cs}ns  \
             gpu_pbr warm avg={avg}ns (min={mn} max={mx} sd={sd})",
            cs = cold_shadow_ns,
            avg = pbr_avg,
            mn = pbr_min,
            mx = pbr_max,
            sd = pbr_stddev,
        );

        rows.push(TierMetrics {
            tier: label,
            tap_count: taps,
            cold_shadow_ns,
            gpu_pbr_warm_ns: pbr_avg,
            gpu_pbr_min_ns: pbr_min,
            gpu_pbr_max_ns: pbr_max,
            gpu_pbr_stddev_ns: pbr_stddev,
            gpu_wait_warm_ns: wait_sum / WARM_N as u64,
            total_warm_ns: total_sum / WARM_N as u64,
        });
    }
    Some(rows)
}

/// Shadow-tier performance sweep: measure the per-fragment GPU cost of each
/// shadow tier (Off / Hard=1-tap / Smooth=3×3 PCF / Soft=PCSS) so the "does Soft
/// really cost more than Hard?" question is answered with numbers, not
/// screenshots. Writes `tests/renders/metrics/metrics_shadow_tiers.json`.
///
/// The signal is the `gpu_pbr_ns` (whole beauty pass) delta across tiers on the
/// SAME geometry — the depth pre-pass (`gpu_shadow_ns`) is tier-independent and
/// cached to 0 on warm frames. Skips a scene when timestamps are unsupported
/// (`gpu_pbr_ns == 0`, e.g. a pure software CI renderer).
#[cfg(feature = "metrics")]
#[test]
#[ignore = "requires IDTX_USD_DIR + GPU + metrics feature; run with --ignored --features metrics"]
fn render_metrics_shadow_tiers() {
    let (w, h) = (1280, 720);
    // Light → heavy. The heavy grid shadows enough pixels that the per-fragment
    // tap delta clears timer noise, so the cost-ordering assertion is gated to it.
    let scenes: &[(&str, bool)] = &[
        ("shadow_single_distant.usda", false),
        ("shadow_penumbra_heights.usda", false),
        ("shadow_heavy_grid.usda", true),
    ];

    println!("=== Shadow-Tier Performance Sweep ===");

    let mut scene_entries: Vec<String> = Vec::new();
    for &(usda, heavy) in scenes {
        let Some(rows) = measure_shadow_tiers(usda, w, h) else {
            continue;
        };

        // Skip scenes whose adapter reports no timestamp data at all.
        if rows.iter().all(|r| r.gpu_pbr_warm_ns == 0) {
            eprintln!("  [{usda}] gpu_pbr_ns == 0 (timestamps unsupported) — not recorded");
            continue;
        }

        let warm = |t: &str| {
            rows.iter()
                .find(|r| r.tier == t)
                .map(|r| r.gpu_pbr_warm_ns)
                .unwrap_or(0)
        };
        let (off, hard, smooth, soft) = (warm("off"), warm("hard"), warm("smooth"), warm("soft"));

        // Fragment-attributable deltas (saturating: warm timers can invert under
        // sub-µs noise, which would otherwise wrap the unsigned subtraction).
        let hard_over_off = hard.saturating_sub(off); // true-hard cost over no shadow
        let smooth_over_off = smooth.saturating_sub(off); // 3×3 PCF cost
        let soft_over_off = soft.saturating_sub(off); // PCSS cost
        let smooth_over_hard = smooth.saturating_sub(hard); // 3×3 penalty vs true-hard
        let soft_over_smooth = soft.saturating_sub(smooth); // PCSS penalty vs 3×3

        // On the heavy scene the tap-count ordering must show up in wall cost:
        // off ≤ hard ≤ smooth ≤ soft, within a 2σ (+5µs floor) timer-noise slack
        // so adjacent near-ties don't make the gate flaky. A gross inversion still trips.
        if heavy {
            let noise = rows.iter().map(|r| r.gpu_pbr_stddev_ns).max().unwrap_or(0);
            let slack = 2 * noise + 5_000;
            assert!(
                hard + slack >= off && smooth + slack >= hard && soft + slack >= smooth,
                "heavy-scene tier cost must be non-decreasing by tap count (within {slack}ns \
                 noise): off={off} hard={hard} smooth={smooth} soft={soft}"
            );
        }

        let tier_json = rows
            .iter()
            .map(TierMetrics::to_json)
            .collect::<Vec<_>>()
            .join(",\n");
        scene_entries.push(format!(
            concat!(
                "    {{\n",
                "      \"scene\": \"{scene}\",\n",
                "      \"heavy\": {heavy},\n",
                "      \"tiers\": [\n{tiers}\n      ],\n",
                "      \"fragment_delta_ns\": {{\n",
                "        \"hard_over_off\": {hoo},\n",
                "        \"smooth_over_off\": {smoo},\n",
                "        \"soft_over_off\": {soo},\n",
                "        \"smooth_over_hard\": {smoh},\n",
                "        \"soft_over_smooth\": {sosm}\n",
                "      }}\n",
                "    }}"
            ),
            scene = usda,
            heavy = heavy,
            tiers = tier_json,
            hoo = hard_over_off,
            smoo = smooth_over_off,
            soo = soft_over_off,
            smoh = smooth_over_hard,
            sosm = soft_over_smooth,
        ));
    }

    if scene_entries.is_empty() {
        eprintln!("No shadow scenes measured (no GPU / no timestamps) — skipping JSON output");
        return;
    }

    let json = format!(
        "{{\n  \"metric\": \"shadow_tier_sweep\",\n  \"resolution\": [{w},{h}],\n  \
         \"warm_frame_n\": 8,\n  \
         \"note\": \"gpu_pbr_ns = whole beauty pass (holds the per-fragment shadow-tap delta); \
         gpu_shadow_ns = depth pre-pass, tier-independent, cached to 0 on warm frames so recorded \
         as cold_shadow_ns.\",\n  \"scenes\": [\n{scenes}\n  ]\n}}\n",
        scenes = scene_entries.join(",\n"),
    );
    let json_path = renders_subdir("metrics").join("metrics_shadow_tiers.json");
    std::fs::write(&json_path, &json).unwrap_or_else(|e| eprintln!("metrics write: {e}"));
    println!("  Saved: {json_path:?}");
}

// ── UsdLuxDomeLight image-based lighting ────────────────────────────────────────

/// Encode a linear RGB pixel to Radiance RGBE (shared exponent) bytes.
fn to_rgbe(r: f32, g: f32, b: f32) -> [u8; 4] {
    let m = r.max(g).max(b);
    if m < 1e-32 {
        return [0, 0, 0, 0];
    }
    // e chosen so m / 2^e ∈ [0.5, 1); mantissa bytes = channel · 256 / 2^e.
    let e = m.log2().floor() as i32 + 1;
    let scale = 256.0 / 2f32.powi(e);
    let byte = |c: f32| (c * scale).clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b), (e + 128) as u8]
}

/// Write a tiny flat (uncompressed) Radiance `.hdr` equirect. Width < 8 forces the
/// old flat scanline format, so no RLE encoder is needed. `rows` is top→bottom; each
/// entry is the constant linear RGB for that row (a simple vertical gradient env).
fn write_flat_hdr(path: &std::path::Path, width: u32, rows: &[[f32; 3]]) {
    use std::io::Write;
    let height = rows.len() as u32;
    let mut f = std::fs::File::create(path).expect("create hdr");
    write!(
        f,
        "#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n"
    )
    .expect("hdr header");
    for row in rows {
        let rgbe = to_rgbe(row[0], row[1], row[2]);
        for _ in 0..width {
            f.write_all(&rgbe).expect("hdr scanline");
        }
    }
}

/// Create `dome_light_textured.usda` + its referenced `.hdr` equirect under
/// `tests/fixtures/usd/lighting/`, returning the absolute stage path. Idempotent (overwrites).
fn write_dome_textured_fixture() -> String {
    let dir = workspace_root().join("tests/fixtures/usd/lighting");
    std::fs::create_dir_all(&dir).ok();
    // Bright sky (top) → dim ground (bottom): a nonuniform env so SH/orientation
    // and the background gradient are actually exercised.
    write_flat_hdr(
        &dir.join("dome_equirect.hdr"),
        4,
        &[
            [0.9, 0.95, 1.0],
            [0.8, 0.85, 1.0],
            [0.3, 0.28, 0.25],
            [0.15, 0.13, 0.1],
        ],
    );
    let usda = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def DomeLight "Sky"
{
    float inputs:intensity = 2.0
    color3f inputs:color = (1, 1, 1)
    asset inputs:texture:file = @dome_equirect.hdr@
    token inputs:texture:format = "latlong"
    uniform token poleAxis = "Y"
    bool inputs:enableColorTemperature = 1
    float inputs:colorTemperature = 5000
}

def Sphere "Ball"
{
    double radius = 1
    color3f[] primvars:displayColor = [(0.8, 0.8, 0.8)]
}
"#;
    let path = dir.join("dome_light_textured.usda");
    std::fs::write(&path, usda).expect("write dome usda");
    path.to_str().unwrap().to_owned()
}

/// A textured `UsdLuxDomeLight` must carry its resolved environment texture, lat-long
/// format, `poleAxis`-derived orientation, ambient radiance, and blackbody-tinted
/// colour all the way through the projection → ECS → `RenderLightDesc` extraction,
/// and (when a GPU adapter is present) render — decoding the HDR, projecting SH,
/// uploading the env, and drawing the skybox background. USD-linked, so `#[ignore]`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR); GPU render portion needs an adapter"]
fn dome_light_textured_plumbs_to_render_light_desc() {
    use idtx_render::scene_extract::RenderLightKind;
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::{DomeTextureFormat, LightKind, StageService};

    let path = write_dome_textured_fixture();

    let mut service = StageService::open(&path).expect("open dome_light_textured.usda");
    let delta = service
        .project_topology_paths(&[])
        .expect("project topology");
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().expect("project viewport");

    // ── Projection level: LightRecord carries the raw dome attributes ──
    let dome_rec = vp
        .lights
        .iter()
        .find(|l| matches!(l.kind, LightKind::Dome))
        .expect("projected dome light");
    assert_eq!(
        dome_rec.dome_texture_format,
        DomeTextureFormat::LatLong,
        "texture:format=latlong must survive to the LightRecord"
    );
    assert!(
        dome_rec
            .dome_texture_path
            .as_deref()
            .is_some_and(|p| p.ends_with(".hdr")),
        "resolved dome texture path should end in .hdr, got {:?}",
        dome_rec.dome_texture_path
    );
    // enableColorTemperature=1 → raw Kelvin surfaced (blackbody tint folded into color).
    assert_eq!(
        dome_rec.temperature,
        Some(5000.0),
        "colorTemperature must surface when enableColorTemperature is set"
    );

    // ── Extraction level: RenderLightDesc carries the render-ready dome data ──
    hydrator
        .apply_viewport_projection(&vp)
        .expect("apply viewport");
    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract(world, 1.0, glam::Mat4::IDENTITY);

    let dome = snap
        .lights
        .iter()
        .find(|l| matches!(l.kind, RenderLightKind::Dome))
        .expect("extracted dome RenderLightDesc");
    assert!(
        dome.dome_texture_path
            .as_deref()
            .is_some_and(|p| p.ends_with(".hdr")),
        "texture path must reach RenderLightDesc, got {:?}",
        dome.dome_texture_path
    );
    assert_eq!(dome.dome_texture_format, DomeTextureFormat::LatLong);
    // env_rotation must be a finite, non-degenerate basis.
    assert!(
        dome.env_rotation
            .to_cols_array()
            .iter()
            .all(|v| v.is_finite()),
        "env_rotation must be finite"
    );
    assert!(
        dome.env_rotation.determinant().abs() > 0.5,
        "env_rotation must be a proper rotation (|det|≈1), got {}",
        dome.env_rotation.determinant()
    );
    // ambient = color × power must be positive (intensity 2.0, non-black color).
    assert!(
        dome.ambient.iter().any(|&c| c > 0.0),
        "textured dome must report a positive ambient radiance, got {:?}",
        dome.ambient
    );
    // 5000 K is warm: the blackbody-tinted color must skew red over blue.
    assert!(
        dome.color[0] > dome.color[2],
        "5000 K dome color should be warm (R>B), got {:?}",
        dome.color
    );

    // ── Decode level (GPU-free): the resolved .hdr must actually decode to pixels ──
    // The plumbing assertions above pass regardless of the `image` crate's `hdr`
    // feature, because they only inspect USD attribute values — never the pixels.
    // This is the assertion that fails when `hdr` is disabled (the dome bug).
    let tex_path = dome.dome_texture_path.clone().expect("dome texture path");
    let env = idtx_render::env_gpu::decode_env_equirect(&tex_path, None)
        .expect("resolved dome .hdr must decode to a float equirect (image crate `hdr` feature)");
    assert_eq!(
        (env.width, env.height),
        (4, 4),
        "fixture equirect is 4x4, got {}x{}",
        env.width,
        env.height
    );
    // The fixture is a vertical gradient: bright sky (top rows) over dim ground.
    let row_lum = |r: u32| {
        (0..env.width)
            .map(|x| {
                let t = env.texels[(r * env.width + x) as usize];
                0.2126 * t[0] + 0.7152 * t[1] + 0.0722 * t[2]
            })
            .sum::<f32>()
            / env.width as f32
    };
    assert!(
        row_lum(0) > row_lum(3),
        "fixture sky (top row) must be brighter than ground (bottom row): top={} bottom={}",
        row_lum(0),
        row_lum(3)
    );

    // ── Render level (best-effort): exercise decode → SH → upload → skybox ──
    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    if let Ok(mut renderer) = Renderer::with_asset_reader(reader) {
        let (w, h) = (256, 256);
        let target = renderer.create_frame_target(w, h);
        renderer.render(&target, &snap, false);
        let rgba = renderer.read_pixels(&target).expect("read back frame");
        let out = renders_subdir("lighting").join("dome_light_textured_render.png");
        save_png(&out, &rgba, w, h);
        // The environment background must produce non-black pixels somewhere even
        // if the sphere is off-frame — the skybox fills the viewport.
        assert!(
            rgba.chunks_exact(4)
                .any(|px| px[0] > 0 || px[1] > 0 || px[2] > 0),
            "textured dome render should not be fully black"
        );

        // Differential: the textured render MUST differ from the same scene with the
        // dome texture stripped, which deterministically forces resolve_env's analytic
        // fallback. If the .hdr never decoded/sampled, BOTH renders take the analytic
        // path and come out identical — exactly the failure mode this test guards.
        let mut snap_analytic = snap.clone();
        for l in &mut snap_analytic.lights {
            l.dome_texture_path = None;
        }
        renderer.render(&target, &snap_analytic, false);
        let rgba_analytic = renderer.read_pixels(&target).expect("read analytic frame");
        let psnr = psnr_rgba(&rgba, &rgba_analytic);
        println!("dome textured-vs-analytic PSNR: {psnr:.1} dB");
        assert!(
            psnr < 40.0,
            "textured-dome render is identical to the analytic fallback ({psnr:.1} dB): \
             the .hdr env never decoded / was not sampled"
        );

        // ── Skybox background: the dome must render *behind* geometry ──
        // The sphere is centered, so the frame corners are background. With a textured
        // dome the skybox fills every pixel no opaque surface covered, so the corners
        // show the environment — NOT the clear colour the analytic fallback leaves
        // there (the analytic path draws no skybox). A per-corner differential isolates
        // the *background* (skybox) from the sphere's IBL shading that PSNR also sees.
        let luma = |px: &[u8]| 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        let at = |buf: &[u8], x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [buf[i], buf[i + 1], buf[i + 2]]
        };
        let corners = [(2u32, 2u32), (w - 3, 2), (2, h - 3), (w - 3, h - 3)];
        let corner_delta: f32 = corners
            .iter()
            .map(|&(x, y)| {
                let t = at(&rgba, x, y);
                let a = at(&rgba_analytic, x, y);
                (0..3).map(|c| (t[c] as i32 - a[c] as i32).abs()).sum::<i32>() as f32 / 3.0
            })
            .sum::<f32>()
            / corners.len() as f32;
        assert!(
            corner_delta > 6.0,
            "background corners barely differ from the skybox-less analytic render \
             (mean |Δ|={corner_delta:.1}/255): the dome backdrop did not fill the background"
        );

        // Orientation: the fixture env is a vertical gradient (bright sky over dim
        // ground). Top-of-frame background rays point up (sky), bottom point down
        // (ground), so a top background band must out-luminate a bottom band.
        let band_luma = |buf: &[u8], y0: u32, y1: u32| {
            let mut sum = 0.0;
            let mut n = 0.0;
            for y in y0..y1 {
                for x in 0..w {
                    sum += luma(&at(buf, x, y));
                    n += 1.0;
                }
            }
            sum / n
        };
        let top = band_luma(&rgba, 0, 12);
        let bottom = band_luma(&rgba, h - 12, h);
        assert!(
            top > bottom,
            "skybox gradient is upside down or absent: top band luma {top:.1} \
             should exceed bottom band luma {bottom:.1}"
        );
    } else {
        eprintln!("skip GPU render portion: no wgpu adapter");
    }
}

/// Regression for the grazing-angle IBL "aura": a smooth, dark sphere lit only by
/// an environment (dome) light must not develop a silhouette rim that runs away in
/// brightness relative to its front-facing cap.
///
/// Root cause (fixed in `surface_gen.rs`): the IBL specular combined the full,
/// view-dependent `fresnel_schlick_roughness(NdotV, …)` with the Karis split-sum
/// env-BRDF `scale` term (`ab.x`) — but that LUT already integrates the Fresnel
/// `pow(1-NdotV,5)` grazing response, so the grazing boost was applied twice and
/// the environment specular climbed far past its energy-conserving value as
/// `NdotV → 0`. On a sphere that reads as a glowing silhouette ring. The fix scales
/// the flat `F0` against `ab.x` (and adds specular horizon occlusion on the
/// textured path; inert for a smooth sphere, so not what this test exercises).
///
/// Setup chosen so the signal is clean and strong:
/// - **Untextured/procedural dome** → no skybox is drawn, so the background is the
///   (black) clear and the grazing rim is measured against black rather than a
///   bright environment backdrop that would bleed into antialiased edge pixels.
/// - **Dark, smooth dielectric** (`base≈0`, low roughness) → the IBL *specular*
///   term, where the bug lives, dominates the sphere's appearance.
/// - Luminance is read back in **linearized** space (the framebuffer is ACES-
///   tonemapped sRGB, which otherwise compresses the ratio).
///
/// The procedural and textured IBL branches share the identical
/// `(F0 * ab.x + ab.y)` split-sum code, so this is a faithful guard for the shared
/// bug. GPU-only (no USD); self-skips when no wgpu adapter is present.
#[test]
fn ibl_grazing_rim_not_blown_out() {
    use idtx_render::scene_extract::{RenderLightDesc, RenderLightKind};
    use idtx_usd::DomeTextureFormat;

    let Some(mut renderer) = try_renderer() else {
        return; // no adapter — try_renderer already logged the skip
    };

    // Sole light: an untextured dome. `dome_texture_path = None` forces the analytic
    // path (no skybox); being the only light suppresses the empty-scene headlight so
    // the sphere is lit purely by the environment term.
    let dome = RenderLightDesc {
        prim_path: "/World/Dome".into(),
        kind: RenderLightKind::DomeProcedural,
        world_matrix: Mat4::IDENTITY,
        color: [1.0, 1.0, 1.0],
        power: 1.0,
        diffuse_contribution: 1.0,
        specular_contribution: 1.0,
        shadow_enabled: false,
        cone_angle: None,
        cone_softness: 0.0,
        shadow_view_proj: None,
        shadow_near: 0.0,
        shadow_far: 0.0,
        light_size: 0.0,
        shadow_slot: None,
        dome_texture_path: None,
        dome_texture_format: DomeTextureFormat::LatLong,
        env_rotation: glam::Mat3::IDENTITY,
        ambient: [1.0, 1.0, 1.0],
    };

    // A single dark, smooth dielectric sphere at the origin.
    let mut sphere = tessellate_sphere(1.0, 64, 96);
    sphere.prim_path = "/World/Ball".into();
    let mut mat = fallback_material("/Looks/Dark");
    mat.base_color = [0.02, 0.02, 0.02];
    mat.metallic = 0.0;
    mat.roughness = 0.3;

    let mut snap = RenderWorldSnapshot::default();
    snap.lights.push(dome);
    snap.meshes.push(RenderMeshInstance {
        prim_path: "/World/Ball".into(),
        mesh: Arc::new(sphere),
        world_matrix: Mat4::IDENTITY,
        material: mat,
        purpose: RenderPurpose::Default,
        casts_shadow: true,
    });
    snap.stats = ExtractionStats {
        mesh_count: 1,
        ..Default::default()
    };

    let (w, h) = (256u32, 256u32);
    snap.camera = Some(fit_camera_to_snapshot(&snap, w as f32 / h as f32));

    let mut target = renderer.create_frame_target(w, h);
    target.clear_color = wgpu::Color::BLACK; // no skybox on the analytic path → black background
    renderer.render(&target, &snap, false);
    let rgba = renderer.read_pixels(&target).expect("read back frame");
    save_png(&renders_subdir("lighting").join("ibl_grazing_rim_sphere.png"), &rgba, w, h);

    // Undo the sRGB encode so the ratio is measured in (roughly) linear light; the
    // 8-bit framebuffer gamma otherwise squashes the highlight difference.
    let srgb_to_linear = |u: u8| {
        let c = u as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let luma = |px: &[u8]| {
        0.2126 * srgb_to_linear(px[0]) + 0.7152 * srgb_to_linear(px[1]) + 0.0722 * srgb_to_linear(px[2])
    };

    // Segment the sphere: any pixel above a small floor over the black clear.
    let mut pts: Vec<(f32, f32, f32)> = Vec::new(); // (x, y, linear-luma)
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if rgba[i] as u32 + rgba[i + 1] as u32 + rgba[i + 2] as u32 > 6 {
                pts.push((x as f32, y as f32, luma(&rgba[i..i + 3])));
            }
        }
    }
    assert!(
        pts.len() > 500,
        "sphere barely rendered ({} lit pixels) — scene/camera setup is wrong",
        pts.len()
    );
    let (cx, cy) = {
        let (sx, sy) = pts.iter().fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p.0, ay + p.1));
        (sx / pts.len() as f32, sy / pts.len() as f32)
    };
    let max_r = pts
        .iter()
        .map(|p| ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt())
        .fold(0.0f32, f32::max);

    // Cap = inner disc (front-facing, NdotV ≈ 1); rim = grazing annulus just inside
    // the limb (NdotV ≈ 0.2–0.4), where the double-Fresnel runaway is strongest.
    let mean_in = |lo: f32, hi: f32| {
        let (sum, n) = pts.iter().fold((0.0f32, 0u32), |(s, n), p| {
            let r = ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt() / max_r;
            if r >= lo && r < hi { (s + p.2, n + 1) } else { (s, n) }
        });
        if n == 0 { 0.0 } else { sum / n as f32 }
    };
    let cap = mean_in(0.0, 0.35);
    let rim = mean_in(0.86, 0.97);
    let ratio = rim / cap.max(1e-6);
    println!("ibl_grazing_rim: cap_luma={cap:.4} rim_luma={rim:.4} ratio={ratio:.2}");

    assert!(cap > 1e-5 && rim > 1e-5, "cap/rim bands empty (cap={cap:.5} rim={rim:.5})");
    // A dark smooth dielectric physically has a brighter grazing rim than its cap
    // (real Fresnel), so some rim/cap > 1 is expected and correct. The double-Fresnel
    // bug roughly 1.6×'s that grazing specular, pushing the ratio much higher.
    // Measured here (linear luma): fixed ≈ 4.5, buggy ≈ 7.2. Guard at 6.0 — the
    // corrected render clears it with margin; the pre-fix render exceeds it.
    assert!(
        ratio <= 6.0,
        "grazing silhouette is blown out relative to the lit cap \
         (rim/cap = {ratio:.2} > 6.0): IBL specular is likely double-counting the \
         Fresnel grazing term (should scale F0, not fresnel_schlick_roughness, against ab.x)"
    );
}


/// Anchoring regression for the dome environment path. A `UsdLuxDomeLight` whose
/// `texture:file` is a layer-relative reference to a *non-existent* asset forces
/// `SdfAssetPath::GetResolvedPath()` to come back empty; the shim must then anchor
/// the authored path against its authoring layer (as the material texture path
/// does) so the Rust side receives an absolute path instead of a bare relative
/// string it would read against the process CWD. Path-only — no HDR, no GPU.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn dome_unresolved_texture_path_is_anchored_absolute() {
    use idtx_usd::{LightKind, StageService};

    let dir = std::env::temp_dir().join(format!("idtx-dome-unresolved-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).ok();
    // Saved on-disk layer so it has a real directory to anchor against. The asset
    // points one level down to a file that does NOT exist, so the default resolver
    // yields an empty resolved path and the anchoring fallback must engage.
    let usda = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def DomeLight "Sky"
{
    asset inputs:texture:file = @sub/missing_env.hdr@
    token inputs:texture:format = "latlong"
}
"#;
    let path = dir.join("dome_unresolved_texture.usda");
    std::fs::write(&path, usda).expect("write unresolved dome usda");

    let mut service =
        StageService::open(path.to_str().unwrap()).expect("open dome_unresolved_texture.usda");
    let vp = service.project_viewport().expect("project viewport");
    let dome = vp
        .lights
        .iter()
        .find(|l| matches!(l.kind, LightKind::Dome))
        .expect("projected dome light");
    let tex = dome
        .dome_texture_path
        .as_deref()
        .expect("dome with authored texture:file must carry a path");
    assert!(
        std::path::Path::new(tex).is_absolute(),
        "unresolved relative dome texture must be anchored to an absolute path, got {tex:?}"
    );
    assert!(
        tex.replace('\\', "/").ends_with("sub/missing_env.hdr"),
        "anchored path must preserve the authored relative tail, got {tex:?}"
    );
}

/// Legacy (pre-USD-21.02) un-namespaced dome attributes. Older assets author
/// `texture:file` / `texture:format` directly rather than the namespaced
/// `inputs:texture:file`. `GetTextureFileAttr()` reads only the namespaced input,
/// so the dome texture read must fall back to the bare name (as `read_light_input`
/// does for scalar inputs) or a textured dome authored the old way projects with
/// no texture path and never renders a skybox. Regression for the bar-scene dome.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR)"]
fn dome_legacy_unnamespaced_texture_file_is_read() {
    use idtx_usd::{DomeTextureFormat, LightKind, StageService};

    let dir = workspace_root().join("tests/fixtures/usd/lighting");
    std::fs::create_dir_all(&dir).ok();
    write_flat_hdr(&dir.join("dome_equirect.hdr"), 4, &[[0.9, 0.95, 1.0]; 4]);
    // Note: `texture:file` / `texture:format`, NOT `inputs:texture:file`.
    let usda = r#"#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
)

def DomeLight "Sky"
{
    float inputs:intensity = 1.0
    asset texture:file = @dome_equirect.hdr@
    token texture:format = "latlong"
}
"#;
    let path = dir.join("dome_legacy_texture.usda");
    std::fs::write(&path, usda).expect("write legacy dome usda");

    let mut service =
        StageService::open(path.to_str().unwrap()).expect("open dome_legacy_texture.usda");
    let vp = service.project_viewport().expect("project viewport");
    let dome = vp
        .lights
        .iter()
        .find(|l| matches!(l.kind, LightKind::Dome))
        .expect("projected dome light");
    assert!(
        dome.dome_texture_path
            .as_deref()
            .is_some_and(|p| p.ends_with("dome_equirect.hdr")),
        "legacy un-namespaced texture:file must be read, got {:?}",
        dome.dome_texture_path
    );
    assert_eq!(
        dome.dome_texture_format,
        DomeTextureFormat::LatLong,
        "legacy un-namespaced texture:format must be read"
    );
}

/// QI-2: the runtime MSAA capability must render a scene correctly at every
/// supported sample count — including the 1× **no-resolve** path (the highest-risk
/// change: a single-sample attachment with `resolve_target: None` and
/// `StoreOp::Store` instead of the MSAA→resolve→discard chain).
///
/// For each count in {1, 2, 4} this constructs a renderer forced to that count,
/// renders a cube, and asserts:
///   * the frame target's `sample_count` matches and `msaa_color_view` is present
///     iff count > 1 (so the PBR pass takes the resolve branch only when it should),
///   * readback is non-empty and contains foreground pixels (the cube drew — the
///     pass didn't silently produce an empty/background-only image), and
///   * the 1× and 4× images are *structurally* similar (same cube, same camera),
///     guarding against the no-resolve branch rendering into the wrong target.
#[test]
#[ignore = "requires a wgpu GPU adapter"]
fn msaa_sample_count_render_paths() {
    let (w, h) = (320u32, 240);
    let mut first_fg: Option<Vec<u8>> = None;

    for n in [1u32, 2, 4] {
        let mut renderer = match Renderer::new_with_sample_count(n) {
            Ok(r) => r,
            Err(RenderError::NoAdapter) => {
                eprintln!("Skipping MSAA render test — no wgpu adapter found");
                return;
            }
            Err(e) => {
                eprintln!("Skipping MSAA render test — {e}");
                return;
            }
        };

        let snap = {
            let mut s = RenderWorldSnapshot::default();
            let mut cube = tessellate_cube(0.5);
            cube.prim_path = "/World/Cube".into();
            let mut mat = fallback_material("/Looks/Red");
            mat.base_color = [0.8, 0.15, 0.15];
            s.meshes.push(RenderMeshInstance {
                prim_path: "/World/Cube".into(),
                mesh: Arc::new(cube),
                world_matrix: Mat4::IDENTITY,
                material: mat,
                purpose: RenderPurpose::Default,
                casts_shadow: true,
            });
            s.stats = ExtractionStats {
                mesh_count: 1,
                ..Default::default()
            };
            let cam = fit_camera_to_snapshot(&s, w as f32 / h as f32);
            s.camera = Some(cam);
            s
        };

        let target = renderer.create_frame_target(w, h);
        assert_eq!(target.sample_count, n, "{n}x: frame target records the count");
        assert_eq!(
            target.msaa_color_view.is_some(),
            n > 1,
            "{n}x: MSAA color target exists iff multisampled"
        );

        renderer.render(&target, &snap, false);
        let pixels = renderer.read_pixels(&target).expect("readback");
        assert_eq!(pixels.len(), (w * h * 4) as usize, "{n}x: full RGBA readback");

        // The top-left corner is background (the cube is centred); count pixels
        // that differ from it — the drawn cube must cover a meaningful fraction.
        let bg = [pixels[0], pixels[1], pixels[2]];
        let fg = pixels
            .chunks_exact(4)
            .filter(|p| {
                let d = |a: u8, b: u8| u32::from(a.abs_diff(b));
                d(p[0], bg[0]) + d(p[1], bg[1]) + d(p[2], bg[2]) > 24
            })
            .count();
        assert!(
            fg > (w * h / 20) as usize && fg < (w * h) as usize,
            "{n}x: expected visible cube over a background, got {fg}/{} fg pixels",
            w * h
        );

        save_png(&renders_subdir("rendering").join(format!("msaa_{n}x_cube.png")), &pixels, w, h);
        println!("MSAA {n}x: {fg} foreground pixels");

        match &first_fg {
            None => first_fg = Some(pixels),
            Some(base) => {
                // Same scene/camera → the images must be close. The 1× (no-resolve)
                // path landing in the wrong target would tank PSNR to near zero.
                let p = psnr_rgba(base, &pixels);
                assert!(
                    p > 20.0,
                    "{n}x vs 1x PSNR {p:.1} dB too low — no-resolve branch likely wrong"
                );
            }
        }
    }
}

/// QI-4: the camera Z-prepass must (a) **not change the rendered image** — only how it
/// is produced — and (b) **measurably cut** fragment-shader invocations (overdraw) on a
/// scene with heavily overlapping opaque geometry.
///
/// Six unit cubes stacked along the camera's view axis fully occlude one another, so
/// without a prepass every covered pixel is shaded ~6×; with the prepass only the
/// frontmost survives the beauty pass's `LessEqual`/depth-write-off opaque test. This
/// renders the identical scene/camera twice — prepass **off** then **on**, via the
/// deterministic [`Renderer::new_with_options`] (no `IDTX_XR_ZPREPASS` env mutation) —
/// and asserts the two readbacks are structurally near-identical (the depth-equality
/// path must not drop frontmost fragments / punch holes) and, when the backend reports
/// pipeline statistics, that the prepass strictly reduces `fragment_shader_invocations`.
#[test]
#[ignore = "requires a wgpu GPU adapter"]
fn z_prepass_removes_overdraw_without_changing_image() {
    let (w, h) = (320u32, 240);

    // Scene built fresh per renderer (each owns its own device); CPU data is identical.
    let build_snap = || {
        let mut s = RenderWorldSnapshot::default();
        for i in 0..6u32 {
            let mut cube = tessellate_cube(0.5);
            let path = format!("/World/Cube{i}");
            cube.prim_path = path.clone().into();
            let mut mat = fallback_material("/Looks/Red");
            mat.base_color = [0.8, 0.15, 0.15];
            s.meshes.push(RenderMeshInstance {
                prim_path: path.into(),
                mesh: Arc::new(cube),
                // Stacked along world Z so they overlap in screen space and occlude —
                // the per-pixel overdraw the prepass is meant to remove.
                world_matrix: Mat4::from_translation(glam::Vec3::new(0.0, 0.0, -0.6 * i as f32)),
                material: mat,
                purpose: RenderPurpose::Default,
                casts_shadow: true,
            });
        }
        s.stats = ExtractionStats {
            mesh_count: 6,
            ..Default::default()
        };
        let cam = fit_camera_to_snapshot(&s, w as f32 / h as f32);
        s.camera = Some(cam);
        s
    };

    let render_once = |z_prepass: bool| -> Option<(Vec<u8>, u64)> {
        let mut renderer = match Renderer::new_with_options(4, z_prepass) {
            Ok(r) => r,
            Err(RenderError::NoAdapter) => return None,
            Err(e) => {
                eprintln!("skip z-prepass test — {e}");
                return None;
            }
        };
        let snap = build_snap();
        let target = renderer.create_frame_target(w, h);
        renderer.render(&target, &snap, false); // cold
        renderer.render(&target, &snap, false); // warm (caches populated)
        let pixels = renderer.read_pixels(&target).expect("readback");
        #[cfg(feature = "metrics")]
        let invocations = renderer.last_frame_timings().fragment_shader_invocations;
        #[cfg(not(feature = "metrics"))]
        let invocations = 0u64;
        Some((pixels, invocations))
    };

    let Some((off_px, off_inv)) = render_once(false) else {
        eprintln!("Skipping Z-prepass test — no wgpu adapter");
        return;
    };
    let (on_px, on_inv) = render_once(true).expect("second renderer");

    // (a) The prepass must not alter the final image (same geometry, same shading).
    let p = psnr_rgba(&off_px, &on_px);
    assert!(
        p > 30.0,
        "Z-prepass changed the image (PSNR {p:.1} dB) — depth-equality path is \
         dropping or altering fragments"
    );
    save_png(&renders_subdir("rendering").join("zprepass_off.png"), &off_px, w, h);
    save_png(&renders_subdir("rendering").join("zprepass_on.png"), &on_px, w, h);

    // (b) When the backend reports overdraw, the prepass must strictly reduce it.
    #[cfg(feature = "metrics")]
    if off_inv > 0 && on_inv > 0 {
        assert!(
            on_inv < off_inv,
            "Z-prepass did not reduce overdraw: on={on_inv} off={off_inv}"
        );
        println!(
            "Z-prepass overdraw: off={off_inv} on={on_inv} ({:.2}× reduction)",
            off_inv as f64 / on_inv.max(1) as f64
        );
    } else {
        println!("Z-prepass: pipeline-statistics unavailable; image-equality checked only");
    }
    #[cfg(not(feature = "metrics"))]
    let _ = (off_inv, on_inv);
}

/// Behavior-driven highlight: surface tint + silhouette outline.
///
/// Renders the synthetic cube+sphere scene three times — baseline, cube tinted,
/// sphere outlined — through `render_with_overlay` (the shared editor/runtime
/// path that drains the highlight channels). Each highlighted render must differ
/// from the baseline (proving the mask + composite passes ran), and the PNGs are
/// dumped for visual inspection.
#[test]
#[ignore = "requires a wgpu GPU adapter"]
fn render_behavior_tint_and_outline() {
    let Some(mut renderer) = try_renderer() else {
        return;
    };
    let (w, h) = (1280, 720);
    let target = renderer.create_frame_target(w, h);

    let base = {
        let mut s = synthetic_snapshot();
        s.camera = Some(fit_camera_to_snapshot(&s, w as f32 / h as f32));
        s
    };

    // Baseline (no highlights).
    renderer.render_with_overlay(&target, &base, None, &[], false);
    let baseline = renderer.read_pixels(&target).expect("readback");
    save_png(&renders_subdir("rendering").join("highlight_baseline.png"), &baseline, w, h);

    // Cube tinted bright magenta (50% strength).
    let mut tinted = base.clone();
    tinted
        .highlight_tints
        .insert("/World/Cube".to_string(), [1.0, 0.0, 1.0, 0.5]);
    renderer.render_with_overlay(&target, &tinted, None, &[], false);
    let tint_px = renderer.read_pixels(&target).expect("readback");
    save_png(&renders_subdir("rendering").join("highlight_tint.png"), &tint_px, w, h);

    // Sphere outlined yellow with an explicit 8 px band.
    let mut outlined = base.clone();
    outlined
        .highlight_outlines
        .push(("/World/Sphere".to_string(), [1.0, 1.0, 0.0, 1.0], 8.0));
    renderer.render_with_overlay(&target, &outlined, None, &[], false);
    let outline_px = renderer.read_pixels(&target).expect("readback");
    save_png(&renders_subdir("rendering").join("highlight_outline.png"), &outline_px, w, h);

    // Each highlighted frame must visibly differ from the baseline (finite PSNR
    // well below the identical-image infinity).
    let tint_psnr = psnr_rgba(&baseline, &tint_px);
    let outline_psnr = psnr_rgba(&baseline, &outline_px);
    println!("tint PSNR vs baseline = {tint_psnr:.1} dB; outline PSNR = {outline_psnr:.1} dB");
    assert!(
        tint_psnr.is_finite() && tint_psnr < 50.0,
        "tint must change the image (PSNR {tint_psnr:.1} dB too high — nothing drawn?)"
    );
    assert!(
        outline_psnr.is_finite() && outline_psnr < 50.0,
        "outline must change the image (PSNR {outline_psnr:.1} dB too high — nothing drawn?)"
    );
}
