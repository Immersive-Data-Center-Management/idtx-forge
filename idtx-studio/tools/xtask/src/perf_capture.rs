//! `cargo xtask perf-capture` — headless render performance harness.
//!
//! Two selectable measurement modes, both surfaceless (no window, no
//! `wgpu::Surface`):
//!
//! * **render** (default): the isolated `idtx-render` pipeline — open stage →
//!   hydrate ECS → `RenderWorldSnapshot::extract` → `Renderer` → cold + N warm
//!   frames, reading `Renderer::last_frame_timings()` (per-phase CPU + real GPU
//!   timestamp queries).
//! * **session**: the runtime-representative path — mirrors
//!   `idtx-runtime-core::run_headless` (`StudioSession::open_with_gpu` then
//!   `tick` / `tick_animation` / `render_one_frame`), reading
//!   `StudioSession::last_frame_timings()`. This is what the shipped
//!   `idtx-runtime` player actually executes.
//!
//! Output is one diffable JSON document (default
//! `testfiles/renders/perf_capture.json`). GPU timestamps degrade to `0` on
//! adapters without `TIMESTAMP_QUERY` — the harness never panics on such boxes.
//!
//! This whole module compiles only under `--features perf-capture`, which also
//! forwards `metrics` down to `idtx-render` / `idtx-studio-session`. The default
//! shell/app build compiles none of it, so the inspection path is untouched.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use idtx_render::metrics::RenderFrameTimings;
use idtx_render::{AssetReadFn, RenderWorldSnapshot, Renderer};
use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

use crate::heavy_stage;

const SCHEMA_VERSION: u32 = 3;

/// Default heavy real-asset scenes (relative to `testfiles/usd`). Each is guarded
/// by an existence check and skipped-with-note when absent.
const DEFAULT_SCENES: &[&str] = &[
    "chess_set/chess_set.usda",
    "DamagedHelmet.usdz",
    "Kitchen_set-2/Kitchen_set.usd",
    "CarbonFrameBike.usdz",
    "toy_biplane_realistic.usdz",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Render,
    Session,
    Both,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Render => "render",
            Self::Session => "session",
            Self::Both => "both",
        }
    }
    fn wants_render(self) -> bool {
        matches!(self, Self::Render | Self::Both)
    }
    fn wants_session(self) -> bool {
        matches!(self, Self::Session | Self::Both)
    }
}

impl From<crate::PerfMode> for Mode {
    fn from(m: crate::PerfMode) -> Self {
        match m {
            crate::PerfMode::Render => Self::Render,
            crate::PerfMode::Session => Self::Session,
            crate::PerfMode::Both => Self::Both,
        }
    }
}

struct Options {
    scenes: Vec<PathBuf>,
    width: u32,
    height: u32,
    warm: usize,
    frames: usize,
    mode: Mode,
    out: PathBuf,
    png: bool,
    alloc: bool,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Resolve the parsed CLI args into the internal [`Options`]. Handles the
/// `--gen-heavy <N>` side effect (generate then append the stage), the `--res
/// WxH` split, and the default heavy-scene list.
fn resolve_options(args: crate::PerfCaptureArgs) -> Result<Options> {
    let mut scenes: Vec<PathBuf> = args.scene;

    // `--gen-heavy <N>` generates a synthetic stage and measures it too.
    if let Some(n) = args.gen_heavy {
        let path = heavy_stage::default_stage_dir().join(format!("heavy_grid_{n}.usda"));
        heavy_stage::write_heavy_grid(&path, n)?;
        println!("generated synthetic stage → {}", path.display());
        scenes.push(path);
    }

    // `--res WxH`.
    let (width, height) = {
        let (w, h) = args
            .res
            .split_once(['x', 'X'])
            .context("--res must look like 1600x900")?;
        let width: u32 = w.trim().parse().context("bad width in --res")?;
        let height: u32 = h.trim().parse().context("bad height in --res")?;
        (width, height)
    };
    if width == 0 || height == 0 {
        bail!("resolution must be non-zero");
    }

    // Default scene list — real heavy assets that exist on this box.
    if scenes.is_empty() {
        let base = workspace_root().join("testfiles/usd");
        for rel in DEFAULT_SCENES {
            let p = base.join(rel);
            if p.exists() {
                scenes.push(p);
            } else {
                eprintln!("  skip (missing): {}", p.display());
            }
        }
        if scenes.is_empty() {
            bail!("no scenes to measure — pass --scene <path> or --gen-heavy <N>");
        }
    }

    let out = args
        .out
        .unwrap_or_else(|| workspace_root().join("testfiles/renders/perf_capture.json"));

    Ok(Options {
        scenes,
        width,
        height,
        warm: args.warm,
        frames: args.frames,
        mode: args.mode.into(),
        out,
        png: args.png,
        alloc: args.alloc,
    })
}

pub fn run(args: crate::PerfCaptureArgs) -> Result<()> {
    let opts = resolve_options(args)?;

    println!(
        "=== perf-capture: mode={} res={}x{} warm={} frames={} scenes={} ===",
        opts.mode.as_str(),
        opts.width,
        opts.height,
        opts.warm,
        opts.frames,
        opts.scenes.len(),
    );

    let mut scene_reports: Vec<SceneReport> = Vec::new();
    for scene in &opts.scenes {
        let path_str = scene.to_string_lossy().into_owned();
        let label = scene
            .file_stem()
            .map_or_else(|| path_str.clone(), |s| s.to_string_lossy().into_owned());
        println!("\n-- {label}");

        let render = if opts.mode.wants_render() {
            match measure_render(scene, &label, &opts) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("  render mode failed: {e:#}");
                    None
                }
            }
        } else {
            None
        };

        let session = if opts.mode.wants_session() {
            match measure_session(scene, &label, &opts) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("  session mode failed: {e:#}");
                    None
                }
            }
        } else {
            None
        };

        scene_reports.push(SceneReport {
            scene: label,
            path: path_str,
            render,
            session,
        });
    }

    let doc = PerfDocument {
        schema_version: SCHEMA_VERSION,
        mode: opts.mode.as_str().to_owned(),
        resolution: [opts.width, opts.height],
        warm_n: opts.warm,
        session_frames: opts.frames,
        scenes: scene_reports,
    };

    if let Some(dir) = opts.out.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let json = serde_json::to_string_pretty(&doc).context("serializing perf report")?;
    std::fs::write(&opts.out, json).with_context(|| format!("writing {}", opts.out.display()))?;
    println!("\nwrote report → {}", opts.out.display());
    Ok(())
}

// ── Render mode ────────────────────────────────────────────────────────────

// StageService wraps an opaque C++ pointer; the harness is single-threaded so
// there is no actual data race when it is captured in an `AssetReadFn`.
struct SyncSvc(idtx_usd::StageService);
unsafe impl Send for SyncSvc {}
unsafe impl Sync for SyncSvc {}

fn measure_render(scene: &Path, label: &str, opts: &Options) -> Result<RenderModeReport> {
    use idtx_scene::{PopulatorRegistry, SceneHydrator};
    use idtx_usd::StageService;

    let scene_str = scene.to_string_lossy();
    let (w, h) = (opts.width, opts.height);

    let reg = opts.alloc.then(|| stats_alloc::Region::new(crate::GLOBAL));

    let t_open = Instant::now();
    let mut service = StageService::open(&*scene_str).context("StageService::open")?;
    let delta = service
        .project_topology_paths(&[])
        .context("project_topology_paths")?;
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let vp = service.project_viewport().context("project_viewport")?;
    hydrator
        .apply_viewport_projection(&vp)
        .context("apply_viewport_projection")?;
    let usd_load_ms = t_open.elapsed().as_secs_f64() * 1000.0;

    // Resolve the stage's authored shadow mode (`idtx:shadowMode`) so the harness
    // renders shadows exactly as the desktop session does: the authored tier maps
    // 1:1, capped at the desktop `Soft` ceiling. There is no product/hardware
    // clamp in the headless harness (capability = true, cap = Soft), mirroring
    // `resolve_shadow_settings` in idtx-studio-session. Without this the plain
    // `extract` wrapper forces `RenderShadowSettings::OFF`, so shadow scenes
    // rendered flat/shadowless regardless of their authored lights + metadata.
    let shadow = {
        use idtx_render::{RenderShadowSettings, ShadowTier};
        let tier = match service.metadata().shadow_mode {
            idtx_usd::ShadowMode::Off => ShadowTier::Off,
            idtx_usd::ShadowMode::Hard => ShadowTier::Hard,
            idtx_usd::ShadowMode::Smooth => ShadowTier::Smooth,
            idtx_usd::ShadowMode::Soft => ShadowTier::Soft,
        };
        RenderShadowSettings::new(tier)
    };
    println!("  shadow tier: {:?}", shadow.tier);

    let world = hydrator.world_mut();
    let snap = RenderWorldSnapshot::extract_with_shadow(
        world,
        w as f32 / h as f32,
        glam::Mat4::IDENTITY,
        shadow,
    );

    let t_init = Instant::now();
    let svc = Arc::new(SyncSvc(service));
    let reader: AssetReadFn = Arc::new(move |p: &str| svc.0.read_asset(p).ok());
    let mut renderer = Renderer::with_asset_reader(reader).context("no GPU adapter")?;
    let renderer_init_ms = t_init.elapsed().as_secs_f64() * 1000.0;

    let target = renderer.create_frame_target(w, h);

    // Cold frame — fresh texture cache (ArResolver + GPU upload included).
    let t_cold = Instant::now();
    renderer.render(&target, &snap, false);
    let cold_pixels = renderer.read_pixels(&target).ok();
    let cold_frame_ms = t_cold.elapsed().as_secs_f64() * 1000.0;

    // Warm frames — textures cached, steady-state O(1) buffer reuse.
    let mut frames: Vec<RenderFrameTimings> = Vec::with_capacity(opts.warm);
    for _ in 0..opts.warm {
        renderer.render(&target, &snap, false);
        frames.push(renderer.last_frame_timings().clone());
    }

    if opts.png {
        if let Some(px) = cold_pixels {
            save_png(
                &renders_dir().join(format!("perf_{label}_render.png")),
                &px,
                w,
                h,
            );
        }
    }

    let alloc_bytes = reg.map(|r| r.change().bytes_allocated);
    let report = build_render_report(
        usd_load_ms,
        renderer_init_ms,
        cold_frame_ms,
        &frames,
        alloc_bytes,
        w,
        h,
    );
    println!(
        "  render: meshes={} cold={:.1}ms warm_total_avg={:.2}ms gpu_pbr_avg={:.2}ms (gpu_ts={})",
        report.mesh_count,
        report.cold_frame_ms,
        report.total.avg_ms,
        report.gpu_pbr.avg_ms,
        report.gpu_timestamps_supported,
    );
    let d = &report.breakdown.submit_decomposition;
    println!(
        "    submit: cpu_submit_avg={:.2}ms = setup {:.2} + opaque {:.2} + transp {:.2} + tonemap {:.2} + finish {:.2} + submit {:.2}",
        report.gpu_passes_cpu_submit.avg_ms,
        d.pass_setup.avg_ms,
        d.encode_opaque.avg_ms,
        d.encode_transparent.avg_ms,
        d.encode_tonemap.avg_ms,
        d.finish.avg_ms,
        d.submit.avg_ms,
    );
    println!(
        "    draws={} cmds={} pipe_switches={} bind_groups={} ns_per_draw={:.0} pipe_dedup={:.1}%",
        d.draw_calls,
        d.commands_encoded,
        d.pipeline_switches,
        d.bind_group_sets,
        d.ns_per_draw,
        d.redundant_state_ratio * 100.0,
    );
    println!(
        "    draw_merge: batches={} folded_meshes={} draws_eliminated={}",
        d.merged_batch_count,
        d.merged_mesh_count,
        d.merged_mesh_count.saturating_sub(d.merged_batch_count),
    );
    let b = &report.breakdown;
    println!(
        "    gpu_pbr split: opaque={:.2}ms transp={:.2}ms (inside_passes={}) | overdraw={:.2}x invocations_avg={} (pipe_stats={})",
        b.gpu_pbr_opaque.avg_ms,
        b.gpu_pbr_transparent.avg_ms,
        b.timestamp_inside_passes_supported,
        b.overdraw_factor,
        b.fragment_shader_invocations_avg,
        b.pipeline_statistics_supported,
    );
    // CPU-phase decomposition — attributes the historical "uninstrumented wall".
    // `residual` is the remaining catch-all; `gpu_wait` classifies CPU- vs GPU-bound.
    println!(
        "    cpu phases: camera_fit {:.2} + camera_lights {:.2} + materialx_prep {:.2} + merge_plan {:.2} + instance_cache {:.2} + draw_list {:.2} + gpu_submit {:.2} | residual {:.2}ms",
        b.camera_fit.avg_ms,
        b.camera_lights.avg_ms,
        b.materialx_prep.avg_ms,
        b.merge_plan.avg_ms,
        b.instance_cache.avg_ms,
        b.draw_list_build.avg_ms,
        b.gpu_passes_cpu_submit.avg_ms,
        b.residual.avg_ms,
    );
    println!(
        "    total_avg={:.2}ms gpu_wait_avg={:.2}ms ({}) mtlx_compiles_max={} aabb_recomputes_max={}",
        report.total.avg_ms,
        b.gpu_wait.avg_ms,
        if b.gpu_wait.avg_ms > report.total.avg_ms {
            "GPU-bound"
        } else {
            "CPU-bound"
        },
        b.materialx_pipeline_compiles_max,
        b.scene_aabb_recomputes_max,
    );
    Ok(report)
}

// ── Session mode ───────────────────────────────────────────────────────────

fn measure_session(scene: &Path, label: &str, opts: &Options) -> Result<SessionModeReport> {
    let (w, h) = (opts.width, opts.height);
    let reg = opts.alloc.then(|| stats_alloc::Region::new(crate::GLOBAL));

    let (device, queue) = headless_device_timestamps().context("no GPU adapter")?;
    let mut session = StudioSession::open_with_gpu(
        scene,
        device,
        queue,
        [w, h],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .context("StudioSession::open_with_gpu")?;

    let dt = 1.0_f32 / 60.0;
    let mut render_one: Vec<u64> = Vec::with_capacity(opts.frames);
    let mut extract: Vec<u64> = Vec::with_capacity(opts.frames);
    let mut gpu_render: Vec<u64> = Vec::with_capacity(opts.frames);
    let mut frames: Vec<RenderFrameTimings> = Vec::with_capacity(opts.frames);

    for _ in 0..opts.frames {
        session.tick().context("session tick")?;
        session
            .tick_animation(f64::from(dt))
            .context("tick_animation")?;
        session.render_one_frame(false);
        let (rof, ext, gpu, timings) = session.last_frame_timings();
        render_one.push(rof);
        extract.push(ext);
        gpu_render.push(gpu);
        frames.push(timings.clone());
    }

    if opts.png {
        if let Ok(px) = session.read_pixels() {
            save_png(
                &renders_dir().join(format!("perf_{label}_session.png")),
                &px,
                w,
                h,
            );
        }
    }

    let alloc_bytes = reg.map(|r| r.change().bytes_allocated);
    let report = SessionModeReport {
        frames: opts.frames,
        render_one_frame: stat(render_one),
        extract: stat(extract),
        gpu_render: stat(gpu_render),
        breakdown: build_breakdown(&frames, w, h),
        peak_alloc_bytes: alloc_bytes,
    };
    println!(
        "  session: frames={} render_frame_avg={:.2}ms extract_avg={:.2}ms gpu_pbr_avg={:.2}ms",
        report.frames,
        report.render_one_frame.avg_ms,
        report.extract.avg_ms,
        report.breakdown.gpu_pbr.avg_ms,
    );
    Ok(report)
}

/// Surfaceless device that requests `TIMESTAMP_QUERY` when the adapter supports
/// it (unlike `run_headless`'s `Features::empty()` device), so session-mode GPU
/// timing is captured. Degrades to no timestamps otherwise.
fn headless_device_timestamps() -> Result<(wgpu::Device, wgpu::Queue)> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| anyhow::anyhow!("no wgpu adapter"))?;
        let mut required_features = wgpu::Features::empty();
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            required_features |= wgpu::Features::TIMESTAMP_QUERY;
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("perf-capture-headless"),
                required_features,
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("device request failed: {e}"))?;
        Ok((device, queue))
    })
}

// ── Aggregation + report structs ─────────────────────────────────────────────

#[derive(Default, serde::Serialize)]
struct Stat {
    min_ms: f64,
    avg_ms: f64,
    max_ms: f64,
    p95_ms: f64,
}

fn ns_ms(ns: u64) -> f64 {
    ns as f64 * 1e-6
}

fn stat(mut v: Vec<u64>) -> Stat {
    if v.is_empty() {
        return Stat::default();
    }
    v.sort_unstable();
    let n = v.len();
    let sum: u128 = v.iter().map(|&x| u128::from(x)).sum();
    let avg_ns = (sum / n as u128) as u64;
    let p95_idx = ((n - 1) * 95) / 100;
    Stat {
        min_ms: ns_ms(v[0]),
        avg_ms: ns_ms(avg_ns),
        max_ms: ns_ms(v[n - 1]),
        p95_ms: ns_ms(v[p95_idx]),
    }
}

/// Extract a `u64` field from each frame and reduce to a `Stat`.
fn field_stat(frames: &[RenderFrameTimings], f: impl Fn(&RenderFrameTimings) -> u64) -> Stat {
    stat(frames.iter().map(f).collect())
}

#[derive(serde::Serialize)]
struct CacheCounters {
    mesh_count: u32,
    instance_count: u32,
    hits: u32,
    misses: u32,
    transform_updates: u32,
    material_rebuilds: u32,
    evictions: u32,
}

impl CacheCounters {
    fn from_last(frames: &[RenderFrameTimings]) -> Self {
        let t = frames.last().cloned().unwrap_or_default();
        Self {
            mesh_count: t.mesh_count,
            instance_count: t.instance_count,
            hits: t.cache_hits,
            misses: t.cache_misses,
            transform_updates: t.cache_transform_updates,
            material_rebuilds: t.cache_material_rebuilds,
            evictions: t.evictions,
        }
    }
}

/// The F1 decomposition of the CPU submission window (`gpu_passes_cpu_submit`)
/// into named sub-phases plus per-frame command counters and the derived
/// north-star ratios. See docs/draw-submission-analysis.md §4.
#[derive(serde::Serialize)]
struct SubmitDecomposition {
    /// Encoder create + PBR `begin_render_pass` + frame-constant bind groups.
    pass_setup: Stat,
    /// Opaque draw recording (Pass 1 per-mesh + Pass 3 instanced + axis overlay).
    encode_opaque: Stat,
    /// Transparent draw recording (Pass 2 + Pass 4), incl. the depth sort.
    encode_transparent: Stat,
    /// Tonemap (post-process) pass recording.
    encode_tonemap: Stat,
    /// `encoder.finish()` alone — backend command-stream translation.
    finish: Stat,
    /// `queue.submit()` alone — driver hand-off.
    submit: Stat,
    /// `draw_indexed`/`draw` calls issued (last warm frame; stable across warm frames).
    draw_calls: u32,
    /// Total pass commands issued (set_* + draw).
    commands_encoded: u32,
    /// `set_pipeline` calls that actually changed the bound pipeline.
    pipeline_switches: u32,
    /// `set_bind_group` calls issued.
    bind_group_sets: u32,
    /// Static draw-merge batches drawn (last warm frame). Each collapses ≥2
    /// meshes/1-instance batches sharing a pipeline + material into one draw.
    merged_batch_count: u32,
    /// Source meshes/1-instance batches folded into merged batches.
    /// `merged_mesh_count − merged_batch_count` is the eliminated draw count.
    merged_mesh_count: u32,
    /// North-star: (encode + finish + submit) avg ns / draw call.
    ns_per_draw: f64,
    /// `1 − pipeline_switches / draw_calls` — the fraction of draws that reused
    /// an already-bound pipeline, so their `set_pipeline` was deduplicated out of
    /// the command stream (post-dedup this is realized savings, not headroom).
    redundant_state_ratio: f64,
}

impl SubmitDecomposition {
    fn from_frames(frames: &[RenderFrameTimings]) -> Self {
        let pass_setup = field_stat(frames, |t| t.pass_setup_ns);
        let encode_opaque = field_stat(frames, |t| t.encode_opaque_ns);
        let encode_transparent = field_stat(frames, |t| t.encode_transparent_ns);
        let encode_tonemap = field_stat(frames, |t| t.encode_tonemap_ns);
        let finish = field_stat(frames, |t| t.finish_ns);
        let submit = field_stat(frames, |t| t.submit_ns);
        let last = frames.last().cloned().unwrap_or_default();
        let draw_calls = last.draw_calls;
        // Sum of the submission work that scales with draw count.
        let submit_work_ms = encode_opaque.avg_ms
            + encode_transparent.avg_ms
            + encode_tonemap.avg_ms
            + finish.avg_ms
            + submit.avg_ms;
        let ns_per_draw = if draw_calls > 0 {
            submit_work_ms * 1e6 / f64::from(draw_calls)
        } else {
            0.0
        };
        let redundant_state_ratio = if draw_calls > 0 {
            1.0 - f64::from(last.pipeline_switches) / f64::from(draw_calls)
        } else {
            0.0
        };
        Self {
            pass_setup,
            encode_opaque,
            encode_transparent,
            encode_tonemap,
            finish,
            submit,
            draw_calls,
            commands_encoded: last.commands_encoded,
            pipeline_switches: last.pipeline_switches,
            bind_group_sets: last.bind_group_sets,
            merged_batch_count: last.merged_batch_count,
            merged_mesh_count: last.merged_mesh_count,
            ns_per_draw,
            redundant_state_ratio,
        }
    }
}

/// The shared per-phase CPU + GPU timing breakdown reduced across frames.
#[derive(serde::Serialize)]
struct Breakdown {
    total: Stat,
    /// Camera resolve + scene auto-fit (`resolve_synthetic_camera`'s per-vertex AABB
    /// when the snapshot has no authored camera *and* the AABB cache missed) +
    /// axis-gizmo prep. The `render()`-entry span. With the AABB cache this is a
    /// cold-frame cost only; on warm frames it collapses to the cheap signature
    /// check + view/proj (see `scene_aabb_recomputes_max`).
    camera_fit: Stat,
    camera_lights: Stat,
    /// `MaterialX` WGSL pipeline pre-pass (`hash_wgsl` probe + first-sight compiles).
    /// `0` unless the `materialx-wgsl` feature is built.
    materialx_prep: Stat,
    /// Static draw-merge planner (F1/F7): per-frame `MergeKey` group + sort.
    merge_plan: Stat,
    mesh_classify: Stat,
    transform_update: Stat,
    material_rebuild: Stat,
    cache_miss_build: Stat,
    evict: Stat,
    instance_cache: Stat,
    draw_list_build: Stat,
    /// CPU-side command encoding + `queue.submit()` wall-clock.
    gpu_passes_cpu_submit: Stat,
    /// F1 decomposition of `gpu_passes_cpu_submit`.
    submit_decomposition: SubmitDecomposition,
    /// Real GPU execution of the PBR pass (`0` when unsupported).
    gpu_pbr: Stat,
    /// GPU execution of the PBR pass **opaque** segments (approximate; `0` unless
    /// `TIMESTAMP_QUERY_INSIDE_PASSES` is available — not on all Metal GPUs).
    gpu_pbr_opaque: Stat,
    /// GPU execution of the PBR pass **transparent** segments (approximate; `0`
    /// unless `TIMESTAMP_QUERY_INSIDE_PASSES` is available).
    gpu_pbr_transparent: Stat,
    /// Real GPU execution of the tonemap pass (`0` when unsupported).
    gpu_tonemap: Stat,
    gpu_timestamps_supported: bool,
    /// Whether the within-pass opaque/transparent split populated
    /// (`TIMESTAMP_QUERY_INSIDE_PASSES`). `false` on backends that only sample at
    /// stage boundaries — the split fields are then `0`.
    timestamp_inside_passes_supported: bool,
    /// Whether the fragment-invocation counter populated
    /// (`PIPELINE_STATISTICS_QUERY`). Always `false` on Metal.
    pipeline_statistics_supported: bool,
    /// Average fragment-shader invocations per warm frame (`0` when unsupported).
    fragment_shader_invocations_avg: u64,
    /// Overdraw factor = `fragment_shader_invocations_avg ÷ (width×height)`. `0.0`
    /// when the pipeline-statistics query is unsupported (all Metal).
    overdraw_factor: f64,
    /// `total` minus the sum of every named CPU phase — the catch-all bucket for
    /// unmeasured CPU work inside `render()`. A large value means a hot span still
    /// lacks its own named timer. This is the decomposition of the historical
    /// "uninstrumented wall".
    residual: Stat,
    /// CPU-observed GPU-drain latency (blocking timestamp readback). A proxy for
    /// end-to-end GPU frame time; compare against `total` to classify the frame as
    /// CPU-bound (`total ≫ gpu_wait`) or GPU-bound (`gpu_wait ≫ total`). `0` when
    /// timestamps are unsupported (no readback poll runs).
    gpu_wait: Stat,
    /// Peak `MaterialX` WGSL pipeline compiles observed in any single warm frame.
    /// `0` on a healthy warm run; nonzero every-frame means pipeline-cache thrash.
    materialx_pipeline_compiles_max: u32,
    /// Peak synthetic-camera scene-AABB recomputes in any single warm frame. For a
    /// camera-less scene this is `1` on the first warm frame (cache fill) and `0`
    /// thereafter, so the warm-window max is `1` when the AABB cache is working and
    /// would be the warm-frame count if the cache were absent (the old ~48 ms/frame
    /// regression). `0` when the scene has an authored camera (fit path skipped).
    scene_aabb_recomputes_max: u32,
    cache: CacheCounters,
}

fn build_breakdown(frames: &[RenderFrameTimings], width: u32, height: u32) -> Breakdown {
    let gpu_supported = frames.iter().any(|t| t.gpu_pbr_ns > 0);
    let inside_passes_supported = frames
        .iter()
        .any(|t| t.gpu_pbr_opaque_ns > 0 || t.gpu_pbr_transparent_ns > 0);
    let pipeline_stats_supported = frames.iter().any(|t| t.fragment_shader_invocations > 0);
    let inv_avg = if frames.is_empty() {
        0
    } else {
        let sum: u128 = frames
            .iter()
            .map(|t| u128::from(t.fragment_shader_invocations))
            .sum();
        (sum / frames.len() as u128) as u64
    };
    let px = u64::from(width) * u64::from(height);
    let overdraw_factor = if px > 0 {
        inv_avg as f64 / px as f64
    } else {
        0.0
    };
    let compiles_max = frames
        .iter()
        .map(|t| t.materialx_pipeline_compiles)
        .max()
        .unwrap_or(0);
    let aabb_recomputes_max = frames
        .iter()
        .map(|t| t.scene_aabb_recomputes)
        .max()
        .unwrap_or(0);
    Breakdown {
        total: field_stat(frames, |t| t.total_ns),
        camera_fit: field_stat(frames, |t| t.camera_fit_ns),
        camera_lights: field_stat(frames, |t| t.camera_lights_ns),
        materialx_prep: field_stat(frames, |t| t.materialx_prep_ns),
        merge_plan: field_stat(frames, |t| t.merge_plan_ns),
        mesh_classify: field_stat(frames, |t| t.mesh_classify_ns),
        transform_update: field_stat(frames, |t| t.transform_update_ns),
        material_rebuild: field_stat(frames, |t| t.material_rebuild_ns),
        cache_miss_build: field_stat(frames, |t| t.cache_miss_build_ns),
        evict: field_stat(frames, |t| t.evict_ns),
        instance_cache: field_stat(frames, |t| t.instance_cache_ns),
        draw_list_build: field_stat(frames, |t| t.draw_list_build_ns),
        gpu_passes_cpu_submit: field_stat(frames, |t| t.gpu_passes_ns),
        submit_decomposition: SubmitDecomposition::from_frames(frames),
        gpu_pbr: field_stat(frames, |t| t.gpu_pbr_ns),
        gpu_pbr_opaque: field_stat(frames, |t| t.gpu_pbr_opaque_ns),
        gpu_pbr_transparent: field_stat(frames, |t| t.gpu_pbr_transparent_ns),
        gpu_tonemap: field_stat(frames, |t| t.gpu_tonemap_ns),
        gpu_timestamps_supported: gpu_supported,
        timestamp_inside_passes_supported: inside_passes_supported,
        pipeline_statistics_supported: pipeline_stats_supported,
        fragment_shader_invocations_avg: inv_avg,
        overdraw_factor,
        residual: field_stat(frames, |t| t.residual_ns),
        gpu_wait: field_stat(frames, |t| t.gpu_wait_ns),
        materialx_pipeline_compiles_max: compiles_max,
        scene_aabb_recomputes_max: aabb_recomputes_max,
        cache: CacheCounters::from_last(frames),
    }
}

#[derive(serde::Serialize)]
struct RenderModeReport {
    mesh_count: u32,
    usd_load_ms: f64,
    renderer_init_ms: f64,
    cold_frame_ms: f64,
    warm_frames: usize,
    // Flatten the shared breakdown fields for convenience.
    total: Stat,
    gpu_pbr: Stat,
    gpu_tonemap: Stat,
    gpu_passes_cpu_submit: Stat,
    gpu_timestamps_supported: bool,
    breakdown: Breakdown,
    #[serde(skip_serializing_if = "Option::is_none")]
    peak_alloc_bytes: Option<usize>,
}

fn build_render_report(
    usd_load_ms: f64,
    renderer_init_ms: f64,
    cold_frame_ms: f64,
    frames: &[RenderFrameTimings],
    alloc_bytes: Option<usize>,
    width: u32,
    height: u32,
) -> RenderModeReport {
    let breakdown = build_breakdown(frames, width, height);
    RenderModeReport {
        mesh_count: frames.last().map_or(0, |t| t.mesh_count),
        usd_load_ms,
        renderer_init_ms,
        cold_frame_ms,
        warm_frames: frames.len(),
        total: field_stat(frames, |t| t.total_ns),
        gpu_pbr: field_stat(frames, |t| t.gpu_pbr_ns),
        gpu_tonemap: field_stat(frames, |t| t.gpu_tonemap_ns),
        gpu_passes_cpu_submit: field_stat(frames, |t| t.gpu_passes_ns),
        gpu_timestamps_supported: breakdown.gpu_timestamps_supported,
        breakdown,
        peak_alloc_bytes: alloc_bytes,
    }
}

#[derive(serde::Serialize)]
struct SessionModeReport {
    frames: usize,
    /// End-to-end `render_one_frame` wall-clock.
    render_one_frame: Stat,
    /// Snapshot extraction (ECS → render input).
    extract: Stat,
    /// Session-tracked GPU render wall-clock.
    gpu_render: Stat,
    breakdown: Breakdown,
    #[serde(skip_serializing_if = "Option::is_none")]
    peak_alloc_bytes: Option<usize>,
}

#[derive(serde::Serialize)]
struct SceneReport {
    scene: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    render: Option<RenderModeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<SessionModeReport>,
}

#[derive(serde::Serialize)]
struct PerfDocument {
    schema_version: u32,
    mode: String,
    resolution: [u32; 2],
    warm_n: usize,
    session_frames: usize,
    scenes: Vec<SceneReport>,
}

// ── PNG helpers ──────────────────────────────────────────────────────────────

fn renders_dir() -> PathBuf {
    workspace_root().join("testfiles/renders")
}

fn save_png(path: &Path, rgba: &[u8], w: u32, h: u32) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Err(e) = image::save_buffer(path, rgba, w, h, image::ColorType::Rgba8) {
        eprintln!("  png write failed ({}): {e}", path.display());
    }
}
