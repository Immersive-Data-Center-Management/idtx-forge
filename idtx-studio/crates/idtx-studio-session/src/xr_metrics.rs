//! Per-XR-frame performance instrumentation (feature = "metrics").
//!
//! [`XrFrameTimings`] captures a wall-clock breakdown of one `render_xr` call —
//! pose authoring, reprojection tick, snapshot extract (with the pivotal
//! `extracted_this_frame` flag), hand-mesh injection, per-eye render, and
//! submit — plus the `OpenXR` frame-pacing counters read from the graphics
//! session. [`XrMetricsAccumulator`] keeps a rolling window of those samples and
//! produces mean/p95/max aggregates for a periodic `[xr-metrics]` log line and a
//! JSON dump written on `stop_xr_preview`.
//!
//! This module is intentionally free of any `pcvr`/`OpenXR` types (it stores plain
//! integers copied out of the session) so the accumulator and its aggregation
//! are unit-testable without a headset. See `docs/xr-preview-performance.md`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Optional override for the directory the JSON metrics dump
/// ([`XrMetricsAccumulator::write_json`]) is written to. Unset on desktop, where
/// the dump lands in [`std::env::temp_dir`]. On Android the system temp dir is
/// not reachable with `adb pull`, so `idtx_runtime_core::run_android` points this
/// at the app's external-files dir (`/sdcard/Android/data/<pkg>/files/`) via
/// [`set_metrics_output_dir`] so the dump can be retrieved off-device.
static METRICS_OUTPUT_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Direct the XR-metrics JSON dump at `dir` instead of the system temp dir. Call
/// once at startup (e.g. with the Android external-files dir so the dump is
/// `adb pull`-able). First call wins; later calls are ignored.
pub fn set_metrics_output_dir(dir: PathBuf) {
    let _ = METRICS_OUTPUT_DIR.set(dir);
}

/// The directory the JSON dump is written to: the [`set_metrics_output_dir`]
/// override if one was set, else the system temp dir.
#[must_use]
pub fn metrics_output_dir() -> PathBuf {
    METRICS_OUTPUT_DIR
        .get()
        .cloned()
        .unwrap_or_else(std::env::temp_dir)
}

/// Number of recent frames retained for rolling aggregation.
pub const WINDOW: usize = 300;

/// How often (in frames) to emit the periodic `[xr-metrics]` log line.
pub const LOG_EVERY: u64 = 120;

/// Timing + counter breakdown for one stereo `render_xr` frame. All `_ns`
/// fields are CPU-side wall-clock nanoseconds.
#[derive(Debug, Clone, Default)]
pub struct XrFrameTimings {
    /// Completed delayed GPU query sequence per eye (zero = unavailable).
    pub eye_gpu_sample_id: [u64; 2],
    pub eye_viewport: [[u32; 2]; 2],
    /// Whole `render_xr` call.
    pub total_ns: u64,
    /// `author_hydrated_controller_poses` (USD pose write + reproject tick).
    pub author_poses_ns: u64,
    /// The `tick()` reprojection inside `author_hydrated_controller_poses`
    /// (subset of `author_poses_ns`).
    pub tick_reproject_ns: u64,
    /// The snapshot-extract branch (full `extract` or cheap patch).
    pub extract_ns: u64,
    /// `true` when a **full** whole-stage `RenderWorldSnapshot::extract` ran this
    /// frame. The key metric for hypothesis B1: expected ≈ 100% today; the fix
    /// target is `false` on pose-only frames.
    pub extracted_this_frame: bool,
    /// `extract_ns` decomposition (all CPU-ns; 0 on the branch not taken). Splits
    /// the per-frame "extract" cost so a non-trivial value is attributed to the
    /// full re-extract vs. a specific value-only patch, rather than lumped.
    /// `full_extract_ns` is non-zero only on an `extracted_this_frame` frame; the
    /// three patch terms only on a warm patch frame.
    pub full_extract_ns: u64,
    /// `patch_world_matrices` (transform value-only patch) wall-ns.
    pub patch_transform_ns: u64,
    /// `patch_joint_matrices` (skeleton value-only patch) wall-ns.
    pub patch_skeleton_ns: u64,
    /// `patch_widget_panels` (widget value-only patch) wall-ns.
    pub patch_widget_ns: u64,
    /// `inject_xr_hand_meshes`.
    pub hand_inject_ns: u64,
    /// Hand bone/joint instances present after injection.
    pub hand_instance_count: u32,
    /// Per-eye `Renderer::render` wall time.
    pub eye_render_ns: [u64; 2],
    /// Per-eye mesh-instance count (from `RenderFrameTimings`).
    pub eye_mesh_count: [u32; 2],
    /// Per-eye instanced-batch instance count.
    pub eye_instance_count: [u32; 2],
    /// Per-eye GPU-cache misses (cold allocations this frame → B3/B4).
    pub eye_cache_misses: [u32; 2],
    /// Per-eye GPU-cache evictions (churn this frame → B4).
    pub eye_evictions: [u32; 2],
    /// Per-eye CPU time building + sorting the draw list (clone cache handles,
    /// pipeline-sort). Subset of `eye_render_ns`; the CPU half of the split that
    /// tells CPU-bound draw submission apart from GPU-bound fill.
    pub eye_draw_list_ns: [u64; 2],
    /// Per-eye CPU time encoding the render passes (`set_pipeline` /
    /// `set_bind_group` / `draw_indexed` per mesh + `queue.submit`). Subset of
    /// `eye_render_ns`. GPU execution is asynchronous, so a high value here with
    /// a many-mesh scene points at CPU-side draw-call submission cost.
    pub eye_gpu_passes_ns: [u64; 2],
    /// Per-eye GPU **execution** time of the PBR beauty pass, from wgpu timestamp
    /// queries (0 when the adapter lacks `TIMESTAMP_QUERY`). Unlike
    /// `eye_gpu_passes_ns` (CPU submission), this is real on-GPU fill time — the
    /// confirmation metric for the Tier-1 bandwidth hypotheses. See
    /// docs/xr-quest-performance-analysis.md §4 Gaps A/B.
    pub eye_gpu_pbr_ns: [u64; 2],
    /// Per-eye GPU execution time of the fullscreen tonemap pass (0 without
    /// timestamp support). Directly measures the separate-tonemap bandwidth cost
    /// (Tier-1 #3).
    pub eye_gpu_tonemap_ns: [u64; 2],
    /// Per-eye GPU execution time of the shadow depth pre-pass (0 without
    /// timestamp support, or when the slot was served from the shared cache).
    pub eye_gpu_shadow_ns: [u64; 2],
    /// Per-eye fragment-shader invocation count (overdraw) from the
    /// pipeline-statistics query (0 without `PIPELINE_STATISTICS_QUERY`). The
    /// single best proxy for the MSAA/tonemap/transparent bandwidth problem
    /// (Tier-1 #3) — not a nanosecond value. See §4 Gap D.
    pub eye_overdraw: [u64; 2],
    /// Per-eye CPU time blocked reading back the GPU timestamp query set
    /// (`gpu_wait_ns`); indicates how much the metrics readback itself stalls.
    pub eye_gpu_wait_ns: [u64; 2],
    /// Per-eye CPU time for the full-resolution `copy_color` blit from the
    /// offscreen target into the `OpenXR` color swapchain image. Untimed before —
    /// directly measures the render-to-offscreen-then-copy cost (Tier-1 #2). See
    /// §4 Gap C.
    pub eye_swapchain_copy_ns: [u64; 2],
    /// Per-eye CPU time for the depth MSAA-resolve + copy into the `OpenXR` depth
    /// swapchain image (0 when no depth layer is active). Tier-1 #2 / Gap C.
    pub eye_depth_copy_ns: [u64; 2],
    /// Per-eye total draw calls issued this frame (all passes), from
    /// `RenderFrameTimings.draw_calls`. The north-star cost signal for a
    /// many-draw-call scene (e.g. unbatched server racks) — a scene that renders
    /// slowly with a high `eye_draw_calls` and low `eye_gpu_pbr_ns` is CPU-bound
    /// on draw submission, not fill.
    pub eye_draw_calls: [u32; 2],
    /// Per-eye `set_pipeline` calls that actually changed the bound pipeline.
    /// `eye_draw_calls − eye_pipeline_switches` is the redundant-bind headroom.
    pub eye_pipeline_switches: [u32; 2],
    /// Per-eye `set_bind_group` calls issued this frame.
    pub eye_bind_group_sets: [u32; 2],
    /// Per-eye static draw-merge batches drawn (folded opaque meshes). Low values
    /// on a repeated-geometry scene mean the merge/instancing path is not firing.
    pub eye_merged_batch_count: [u32; 2],
    /// `submit_xr_frame`.
    pub submit_ns: u64,
    /// `OpenXR`: time blocked in `xrWaitFrame` this frame.
    pub wait_frame_ns: u64,
    /// `OpenXR`: time blocked waiting for swapchain images (both eyes).
    pub acquire_wait_ns: u64,
    /// `OpenXR`: wall interval since the previous `begin_frame` (displayed cadence).
    pub frame_interval_ns: u64,
    /// `OpenXR`: cumulative empty (no-layer) frames submitted so far.
    pub empty_frame_count: u64,
    /// `OpenXR`: whether a depth composition layer was submitted (hypothesis A).
    pub depth_layer_submitted: bool,
}

/// Mean / 95th-percentile / max of a nanosecond series.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stat {
    pub mean_ns: u64,
    pub p95_ns: u64,
    pub max_ns: u64,
}

impl Stat {
    /// Compute mean/p95/max over `values`. Empty input yields all-zero.
    #[must_use]
    pub fn from(values: &[u64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let sum: u128 = values.iter().map(|&v| u128::from(v)).sum();
        let mean_ns = (sum / values.len() as u128) as u64;
        let mut sorted: Vec<u64> = values.to_vec();
        sorted.sort_unstable();
        // Nearest-rank p95: index = ceil(0.95 * n) - 1, clamped into range.
        let rank = ((sorted.len() as f64) * 0.95).ceil() as usize;
        let idx = rank.saturating_sub(1).min(sorted.len() - 1);
        Self {
            mean_ns,
            p95_ns: sorted[idx],
            max_ns: *sorted.last().unwrap(),
        }
    }
}

/// Rolling accumulator over the last [`WINDOW`] frames.
#[derive(Debug, Default)]
pub struct XrMetricsAccumulator {
    samples: VecDeque<XrFrameTimings>,
    /// Total frames recorded since the session started (not just the window).
    frame_counter: u64,
}

impl XrMetricsAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(WINDOW),
            frame_counter: 0,
        }
    }

    /// Record one frame, evicting the oldest sample once the window is full.
    pub fn record(&mut self, frame: XrFrameTimings) {
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(frame);
        self.frame_counter = self.frame_counter.wrapping_add(1);
    }

    /// Total frames recorded since the session started.
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frame_counter
    }

    /// Frames currently retained in the rolling window.
    #[must_use]
    pub fn window_len(&self) -> usize {
        self.samples.len()
    }

    /// `true` on frame boundaries where the periodic log line should be emitted.
    #[must_use]
    pub fn should_log(&self) -> bool {
        self.frame_counter > 0 && self.frame_counter % LOG_EVERY == 0
    }

    /// `total_ns` mean/p95/max over the window.
    #[must_use]
    pub fn total(&self) -> Stat {
        self.stat(|f| f.total_ns)
    }

    /// Fraction of windowed frames that ran a full extract (0.0..=1.0).
    #[must_use]
    pub fn extracted_ratio(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let n = self
            .samples
            .iter()
            .filter(|f| f.extracted_this_frame)
            .count();
        n as f64 / self.samples.len() as f64
    }

    /// Mean hand-instance count over the window.
    #[must_use]
    pub fn mean_hand_instances(&self) -> u32 {
        if self.samples.is_empty() {
            return 0;
        }
        let sum: u64 = self
            .samples
            .iter()
            .map(|f| u64::from(f.hand_instance_count))
            .sum();
        (sum / self.samples.len() as u64) as u32
    }

    /// Max per-eye evictions seen across both eyes over the window.
    #[must_use]
    pub fn max_eye_evictions(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_evictions)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye mesh count across both eyes over the window.
    #[must_use]
    pub fn max_eye_mesh_count(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_mesh_count)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye GPU-cache misses across both eyes over the window.
    ///
    /// A miss is a cold allocation or a geometry-serial change that forces a
    /// full cache-entry rebuild (B3/B4). This is the direct confirmation metric
    /// for B3: with content-stable gizmo serials (Fix 5) this should fall to ~0
    /// on steady-state frames, whereas a persistently high value confirms the
    /// per-frame re-tessellation churn.
    #[must_use]
    pub fn max_eye_cache_misses(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_cache_misses)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye overdraw (fragment-shader invocations) across both eyes over
    /// the window. `0` unless the adapter supports `PIPELINE_STATISTICS_QUERY`
    /// (see Gap A/D). The confirmation metric for the MSAA/tonemap/transparent
    /// bandwidth hypothesis (Tier-1 #3).
    #[must_use]
    pub fn max_eye_overdraw(&self) -> u64 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_overdraw)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye draw-call count across both eyes over the window. The primary
    /// confirmation metric for a CPU-bound, many-draw-call scene.
    #[must_use]
    pub fn max_eye_draw_calls(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_draw_calls)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye `set_pipeline` switches across both eyes over the window.
    #[must_use]
    pub fn max_eye_pipeline_switches(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_pipeline_switches)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye `set_bind_group` calls across both eyes over the window.
    #[must_use]
    pub fn max_eye_bind_group_sets(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_bind_group_sets)
            .max()
            .unwrap_or(0)
    }

    /// Max per-eye static draw-merge batch count across both eyes over the window.
    #[must_use]
    pub fn max_eye_merged_batch_count(&self) -> u32 {
        self.samples
            .iter()
            .flat_map(|f| f.eye_merged_batch_count)
            .max()
            .unwrap_or(0)
    }

    /// Frame-interval (displayed cadence) mean/p95/max over the window.
    #[must_use]
    pub fn frame_interval(&self) -> Stat {
        self.stat(|f| f.frame_interval_ns)
    }

    /// `xrWaitFrame` block mean/p95/max over the window.
    #[must_use]
    pub fn wait_frame(&self) -> Stat {
        self.stat(|f| f.wait_frame_ns)
    }

    /// The most recent frame's cumulative empty-frame count (monotonic).
    #[must_use]
    pub fn empty_frame_count(&self) -> u64 {
        self.samples.back().map_or(0, |f| f.empty_frame_count)
    }

    /// Whether the most recent frame submitted a depth layer (hypothesis A).
    #[must_use]
    pub fn depth_layer_submitted(&self) -> bool {
        self.samples.back().is_some_and(|f| f.depth_layer_submitted)
    }

    fn stat(&self, f: impl Fn(&XrFrameTimings) -> u64) -> Stat {
        let values: Vec<u64> = self.samples.iter().map(f).collect();
        Stat::from(&values)
    }

    fn gpu_stat(&self, eye: usize, f: impl Fn(&XrFrameTimings) -> u64) -> Stat {
        let values: Vec<_> = self.samples.iter().filter(|s| s.eye_gpu_sample_id[eye] != 0).map(f).collect();
        Stat::from(&values)
    }

    /// One-line headline for the periodic `[xr-metrics]` `tracing::info!`.
    #[must_use]
    pub fn log_line(&self) -> String {
        let total = self.total();
        let interval = self.frame_interval();
        // Eye-0 CPU split (draw-list build + pass encode). Eye 1 is near-identical
        // (see the JSON dump for both); this inline pair answers CPU-vs-GPU at a
        // glance: high `gpu` with a many-mesh scene = CPU-side draw submission.
        let eye0_draw = self.stat(|f| f.eye_draw_list_ns[0]);
        let eye0_gpu = self.stat(|f| f.eye_gpu_passes_ns[0]);
        // GPU-execution timings (0 unless the adapter supports timestamp queries;
        // see Gap A). `gpu_pbr` is real on-GPU fill time, `copy` is the
        // offscreen→swapchain blit (Gap C), `overdraw` the fragment-invocation
        // proxy (Gap D). Reported for eye 0; the JSON carries both eyes.
        let eye0_gpu_pbr = self.gpu_stat(0, |f| f.eye_gpu_pbr_ns[0]);
        let eye0_gpu_tonemap = self.gpu_stat(0, |f| f.eye_gpu_tonemap_ns[0]);
        let eye0_copy = self.stat(|f| f.eye_swapchain_copy_ns[0]);
        format!(
            "[xr-metrics] frame#{n} win={win} total(p95={total_p95:.2}ms max={total_max:.2}ms) \
             extract_ratio={ratio:.0}% hands~{hands} eye(mesh_max={mesh} miss_max={miss} evict_max={evict}) \
             draw(calls_max={draws} pipe_sw_max={pipe} merged_max={merged}) \
             eye0(draw_p95={draw_p95:.2}ms gpu_p95={gpu_p95:.2}ms pbr_p95={pbr_p95:.2}ms tonemap_p95={tm_p95:.2}ms copy_p95={copy_p95:.2}ms overdraw_max={overdraw}) \
             interval(p95={int_p95:.2}ms) wait(p95={wait_p95:.2}ms) empty={empty} depth_layer={depth}",
            n = self.frame_counter,
            win = self.samples.len(),
            total_p95 = ns_to_ms(total.p95_ns),
            total_max = ns_to_ms(total.max_ns),
            ratio = self.extracted_ratio() * 100.0,
            hands = self.mean_hand_instances(),
            mesh = self.max_eye_mesh_count(),
            miss = self.max_eye_cache_misses(),
            evict = self.max_eye_evictions(),
            draws = self.max_eye_draw_calls(),
            pipe = self.max_eye_pipeline_switches(),
            merged = self.max_eye_merged_batch_count(),
            draw_p95 = ns_to_ms(eye0_draw.p95_ns),
            gpu_p95 = ns_to_ms(eye0_gpu.p95_ns),
            pbr_p95 = ns_to_ms(eye0_gpu_pbr.p95_ns),
            tm_p95 = ns_to_ms(eye0_gpu_tonemap.p95_ns),
            copy_p95 = ns_to_ms(eye0_copy.p95_ns),
            overdraw = self.max_eye_overdraw(),
            int_p95 = ns_to_ms(interval.p95_ns),
            wait_p95 = ns_to_ms(self.wait_frame().p95_ns),
            empty = self.empty_frame_count(),
            depth = self.depth_layer_submitted(),
        )
    }

    /// Write an `idtx-xr-metrics-<stage>-<ts>.json` summary to the configured
    /// metrics output dir ([`metrics_output_dir`]) — the system temp dir by
    /// default, or the Android external-files dir when
    /// [`set_metrics_output_dir`] was called. Best-effort: errors are logged.
    pub fn write_json(&self, stage_name: &str) {
        if self.samples.is_empty() {
            return;
        }
        let dir = metrics_output_dir();
        if let Err(e) = self.write_json_in(&dir, stage_name) {
            tracing::warn!("[xr-metrics] failed to write JSON dump: {e}");
        }
    }

    fn write_json_in(&self, dir: &Path, stage_name: &str) -> std::io::Result<()> {
        use std::io::Write;
        // Ensure the target dir exists — on Android the external-files dir may
        // not have been created yet; on desktop the temp dir always exists.
        std::fs::create_dir_all(dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = dir.join(format!("idtx-xr-metrics-{stage_name}-{ts}.json"));
        let total = self.total();
        let author = self.stat(|f| f.author_poses_ns);
        let tick = self.stat(|f| f.tick_reproject_ns);
        let extract = self.stat(|f| f.extract_ns);
        let full_extract = self.stat(|f| f.full_extract_ns);
        let patch_xform = self.stat(|f| f.patch_transform_ns);
        let patch_skel = self.stat(|f| f.patch_skeleton_ns);
        let patch_widget = self.stat(|f| f.patch_widget_ns);
        let hand = self.stat(|f| f.hand_inject_ns);
        let submit = self.stat(|f| f.submit_ns);
        let eye0 = self.stat(|f| f.eye_render_ns[0]);
        let eye1 = self.stat(|f| f.eye_render_ns[1]);
        let eye0_draw = self.stat(|f| f.eye_draw_list_ns[0]);
        let eye0_gpu = self.stat(|f| f.eye_gpu_passes_ns[0]);
        let eye1_draw = self.stat(|f| f.eye_draw_list_ns[1]);
        let eye1_gpu = self.stat(|f| f.eye_gpu_passes_ns[1]);
        // GPU-execution + copy timings (Gaps A/B/C). Zero unless the adapter
        // supports the timestamp queries enabled in xr_vulkan::bring_up.
        let eye0_pbr = self.gpu_stat(0, |f| f.eye_gpu_pbr_ns[0]);
        let eye1_pbr = self.gpu_stat(1, |f| f.eye_gpu_pbr_ns[1]);
        let eye0_tm = self.gpu_stat(0, |f| f.eye_gpu_tonemap_ns[0]);
        let eye1_tm = self.gpu_stat(1, |f| f.eye_gpu_tonemap_ns[1]);
        let eye0_shadow = self.gpu_stat(0, |f| f.eye_gpu_shadow_ns[0]);
        let eye1_shadow = self.gpu_stat(1, |f| f.eye_gpu_shadow_ns[1]);
        let eye0_copy = self.stat(|f| f.eye_swapchain_copy_ns[0]);
        let eye1_copy = self.stat(|f| f.eye_swapchain_copy_ns[1]);
        let eye0_depth_copy = self.stat(|f| f.eye_depth_copy_ns[0]);
        let eye1_depth_copy = self.stat(|f| f.eye_depth_copy_ns[1]);
        let interval = self.frame_interval();
        let wait = self.wait_frame();
        let acquire = self.stat(|f| f.acquire_wait_ns);
        let mut file = std::fs::File::create(&path)?;
        write!(
            file,
            r#"{{
  "schema_version": 1,
  "workspace": "idtx-studio",
  "stage": "{stage}",
  "frames_recorded": {frames},
  "gpu_samples": [{gpu0}, {gpu1}],
  "eye_viewport": [[{w0}, {h0}], [{w1}, {h1}]],
  "window_len": {win},
  "extracted_this_frame_ratio": {ratio:.4},
  "depth_layer_submitted": {depth},
  "empty_frame_count": {empty},
  "phase_ms": {{
    "total":         {{ "mean": {t_mean:.3}, "p95": {t_p95:.3}, "max": {t_max:.3} }},
    "author_poses":  {{ "mean": {a_mean:.3}, "p95": {a_p95:.3}, "max": {a_max:.3} }},
    "tick_reproject":{{ "mean": {k_mean:.3}, "p95": {k_p95:.3}, "max": {k_max:.3} }},
    "extract":       {{ "mean": {e_mean:.3}, "p95": {e_p95:.3}, "max": {e_max:.3} }},
    "extract_full":  {{ "mean": {ef_mean:.3}, "p95": {ef_p95:.3}, "max": {ef_max:.3} }},
    "patch_transform":{{ "mean": {px_mean:.3}, "p95": {px_p95:.3}, "max": {px_max:.3} }},
    "patch_skeleton":{{ "mean": {pk_mean:.3}, "p95": {pk_p95:.3}, "max": {pk_max:.3} }},
    "patch_widget":  {{ "mean": {pw_mean:.3}, "p95": {pw_p95:.3}, "max": {pw_max:.3} }},
    "hand_inject":   {{ "mean": {h_mean:.3}, "p95": {h_p95:.3}, "max": {h_max:.3} }},
    "eye0_render":   {{ "mean": {e0_mean:.3}, "p95": {e0_p95:.3}, "max": {e0_max:.3} }},
    "eye0_draw_list":  {{ "mean": {e0d_mean:.3}, "p95": {e0d_p95:.3}, "max": {e0d_max:.3} }},
    "eye0_gpu_passes": {{ "mean": {e0g_mean:.3}, "p95": {e0g_p95:.3}, "max": {e0g_max:.3} }},
    "eye1_render":   {{ "mean": {e1_mean:.3}, "p95": {e1_p95:.3}, "max": {e1_max:.3} }},
    "eye1_draw_list":  {{ "mean": {e1d_mean:.3}, "p95": {e1d_p95:.3}, "max": {e1d_max:.3} }},
    "eye1_gpu_passes": {{ "mean": {e1g_mean:.3}, "p95": {e1g_p95:.3}, "max": {e1g_max:.3} }},
    "submit":        {{ "mean": {s_mean:.3}, "p95": {s_p95:.3}, "max": {s_max:.3} }}
  }},
  "gpu_ms": {{
    "eye0_pbr":         {{ "mean": {g0p_mean:.3}, "p95": {g0p_p95:.3}, "max": {g0p_max:.3} }},
    "eye1_pbr":         {{ "mean": {g1p_mean:.3}, "p95": {g1p_p95:.3}, "max": {g1p_max:.3} }},
    "eye0_tonemap":     {{ "mean": {g0t_mean:.3}, "p95": {g0t_p95:.3}, "max": {g0t_max:.3} }},
    "eye1_tonemap":     {{ "mean": {g1t_mean:.3}, "p95": {g1t_p95:.3}, "max": {g1t_max:.3} }},
    "eye0_shadow":      {{ "mean": {g0s_mean:.3}, "p95": {g0s_p95:.3}, "max": {g0s_max:.3} }},
    "eye1_shadow":      {{ "mean": {g1s_mean:.3}, "p95": {g1s_p95:.3}, "max": {g1s_max:.3} }},
    "eye0_swapchain_copy": {{ "mean": {c0_mean:.3}, "p95": {c0_p95:.3}, "max": {c0_max:.3} }},
    "eye1_swapchain_copy": {{ "mean": {c1_mean:.3}, "p95": {c1_p95:.3}, "max": {c1_max:.3} }},
    "eye0_depth_copy":  {{ "mean": {d0_mean:.3}, "p95": {d0_p95:.3}, "max": {d0_max:.3} }},
    "eye1_depth_copy":  {{ "mean": {d1_mean:.3}, "p95": {d1_p95:.3}, "max": {d1_max:.3} }}
  }},
  "pacing_ms": {{
    "frame_interval":{{ "mean": {i_mean:.3}, "p95": {i_p95:.3}, "max": {i_max:.3} }},
    "wait_frame":    {{ "mean": {w_mean:.3}, "p95": {w_p95:.3}, "max": {w_max:.3} }},
    "acquire_wait":  {{ "mean": {q_mean:.3}, "p95": {q_p95:.3}, "max": {q_max:.3} }}
  }},
  "counters": {{
    "mean_hand_instances": {hands},
    "max_eye_mesh_count": {mesh},
    "max_eye_cache_misses": {miss},
    "max_eye_evictions": {evict},
    "max_eye_overdraw": {overdraw},
    "max_eye_draw_calls": {draws},
    "max_eye_pipeline_switches": {pipe},
    "max_eye_bind_group_sets": {binds},
    "max_eye_merged_batch_count": {merged}
  }}
}}"#,
            stage = stage_name,
            frames = self.frame_counter,
            gpu0 = self.samples.iter().filter(|s| s.eye_gpu_sample_id[0] != 0).count(),
            gpu1 = self.samples.iter().filter(|s| s.eye_gpu_sample_id[1] != 0).count(),
            w0 = self.samples.back().map_or(0, |s| s.eye_viewport[0][0]),
            h0 = self.samples.back().map_or(0, |s| s.eye_viewport[0][1]),
            w1 = self.samples.back().map_or(0, |s| s.eye_viewport[1][0]),
            h1 = self.samples.back().map_or(0, |s| s.eye_viewport[1][1]),
            win = self.samples.len(),
            ratio = self.extracted_ratio(),
            depth = self.depth_layer_submitted(),
            empty = self.empty_frame_count(),
            t_mean = ns_to_ms(total.mean_ns),
            t_p95 = ns_to_ms(total.p95_ns),
            t_max = ns_to_ms(total.max_ns),
            a_mean = ns_to_ms(author.mean_ns),
            a_p95 = ns_to_ms(author.p95_ns),
            a_max = ns_to_ms(author.max_ns),
            k_mean = ns_to_ms(tick.mean_ns),
            k_p95 = ns_to_ms(tick.p95_ns),
            k_max = ns_to_ms(tick.max_ns),
            e_mean = ns_to_ms(extract.mean_ns),
            e_p95 = ns_to_ms(extract.p95_ns),
            e_max = ns_to_ms(extract.max_ns),
            ef_mean = ns_to_ms(full_extract.mean_ns),
            ef_p95 = ns_to_ms(full_extract.p95_ns),
            ef_max = ns_to_ms(full_extract.max_ns),
            px_mean = ns_to_ms(patch_xform.mean_ns),
            px_p95 = ns_to_ms(patch_xform.p95_ns),
            px_max = ns_to_ms(patch_xform.max_ns),
            pk_mean = ns_to_ms(patch_skel.mean_ns),
            pk_p95 = ns_to_ms(patch_skel.p95_ns),
            pk_max = ns_to_ms(patch_skel.max_ns),
            pw_mean = ns_to_ms(patch_widget.mean_ns),
            pw_p95 = ns_to_ms(patch_widget.p95_ns),
            pw_max = ns_to_ms(patch_widget.max_ns),
            h_mean = ns_to_ms(hand.mean_ns),
            h_p95 = ns_to_ms(hand.p95_ns),
            h_max = ns_to_ms(hand.max_ns),
            e0_mean = ns_to_ms(eye0.mean_ns),
            e0_p95 = ns_to_ms(eye0.p95_ns),
            e0_max = ns_to_ms(eye0.max_ns),
            e0d_mean = ns_to_ms(eye0_draw.mean_ns),
            e0d_p95 = ns_to_ms(eye0_draw.p95_ns),
            e0d_max = ns_to_ms(eye0_draw.max_ns),
            e0g_mean = ns_to_ms(eye0_gpu.mean_ns),
            e0g_p95 = ns_to_ms(eye0_gpu.p95_ns),
            e0g_max = ns_to_ms(eye0_gpu.max_ns),
            e1_mean = ns_to_ms(eye1.mean_ns),
            e1_p95 = ns_to_ms(eye1.p95_ns),
            e1_max = ns_to_ms(eye1.max_ns),
            e1d_mean = ns_to_ms(eye1_draw.mean_ns),
            e1d_p95 = ns_to_ms(eye1_draw.p95_ns),
            e1d_max = ns_to_ms(eye1_draw.max_ns),
            e1g_mean = ns_to_ms(eye1_gpu.mean_ns),
            e1g_p95 = ns_to_ms(eye1_gpu.p95_ns),
            e1g_max = ns_to_ms(eye1_gpu.max_ns),
            s_mean = ns_to_ms(submit.mean_ns),
            s_p95 = ns_to_ms(submit.p95_ns),
            s_max = ns_to_ms(submit.max_ns),
            g0p_mean = ns_to_ms(eye0_pbr.mean_ns),
            g0p_p95 = ns_to_ms(eye0_pbr.p95_ns),
            g0p_max = ns_to_ms(eye0_pbr.max_ns),
            g1p_mean = ns_to_ms(eye1_pbr.mean_ns),
            g1p_p95 = ns_to_ms(eye1_pbr.p95_ns),
            g1p_max = ns_to_ms(eye1_pbr.max_ns),
            g0t_mean = ns_to_ms(eye0_tm.mean_ns),
            g0t_p95 = ns_to_ms(eye0_tm.p95_ns),
            g0t_max = ns_to_ms(eye0_tm.max_ns),
            g1t_mean = ns_to_ms(eye1_tm.mean_ns),
            g1t_p95 = ns_to_ms(eye1_tm.p95_ns),
            g1t_max = ns_to_ms(eye1_tm.max_ns),
            g0s_mean = ns_to_ms(eye0_shadow.mean_ns),
            g0s_p95 = ns_to_ms(eye0_shadow.p95_ns),
            g0s_max = ns_to_ms(eye0_shadow.max_ns),
            g1s_mean = ns_to_ms(eye1_shadow.mean_ns),
            g1s_p95 = ns_to_ms(eye1_shadow.p95_ns),
            g1s_max = ns_to_ms(eye1_shadow.max_ns),
            c0_mean = ns_to_ms(eye0_copy.mean_ns),
            c0_p95 = ns_to_ms(eye0_copy.p95_ns),
            c0_max = ns_to_ms(eye0_copy.max_ns),
            c1_mean = ns_to_ms(eye1_copy.mean_ns),
            c1_p95 = ns_to_ms(eye1_copy.p95_ns),
            c1_max = ns_to_ms(eye1_copy.max_ns),
            d0_mean = ns_to_ms(eye0_depth_copy.mean_ns),
            d0_p95 = ns_to_ms(eye0_depth_copy.p95_ns),
            d0_max = ns_to_ms(eye0_depth_copy.max_ns),
            d1_mean = ns_to_ms(eye1_depth_copy.mean_ns),
            d1_p95 = ns_to_ms(eye1_depth_copy.p95_ns),
            d1_max = ns_to_ms(eye1_depth_copy.max_ns),
            i_mean = ns_to_ms(interval.mean_ns),
            i_p95 = ns_to_ms(interval.p95_ns),
            i_max = ns_to_ms(interval.max_ns),
            w_mean = ns_to_ms(wait.mean_ns),
            w_p95 = ns_to_ms(wait.p95_ns),
            w_max = ns_to_ms(wait.max_ns),
            q_mean = ns_to_ms(acquire.mean_ns),
            q_p95 = ns_to_ms(acquire.p95_ns),
            q_max = ns_to_ms(acquire.max_ns),
            hands = self.mean_hand_instances(),
            mesh = self.max_eye_mesh_count(),
            miss = self.max_eye_cache_misses(),
            evict = self.max_eye_evictions(),
            overdraw = self.max_eye_overdraw(),
            draws = self.max_eye_draw_calls(),
            pipe = self.max_eye_pipeline_switches(),
            binds = self.max_eye_bind_group_sets(),
            merged = self.max_eye_merged_batch_count(),
        )?;
        eprintln!("[xr-metrics] written to {}", path.display());
        Ok(())
    }
}

/// Nanoseconds → milliseconds for human-readable reporting.
#[must_use]
fn ns_to_ms(ns: u64) -> f64 {
    ns as f64 * 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(total_ns: u64, extracted: bool) -> XrFrameTimings {
        XrFrameTimings {
            total_ns,
            extracted_this_frame: extracted,
            ..Default::default()
        }
    }

    #[test]
    fn stat_mean_p95_max_over_known_sequence() {
        // 1..=100 ms in ns. mean = 50.5ms, max = 100ms, nearest-rank p95 = 95ms.
        let values: Vec<u64> = (1..=100).map(|i| i * 1_000_000).collect();
        let s = Stat::from(&values);
        assert_eq!(s.max_ns, 100_000_000);
        assert_eq!(s.mean_ns, 50_500_000);
        assert_eq!(s.p95_ns, 95_000_000);
    }

    #[test]
    fn stat_empty_is_zero() {
        assert_eq!(Stat::from(&[]), Stat::default());
    }

    #[test]
    fn stat_single_value() {
        let s = Stat::from(&[42]);
        assert_eq!(s.mean_ns, 42);
        assert_eq!(s.p95_ns, 42);
        assert_eq!(s.max_ns, 42);
    }

    #[test]
    fn accumulator_window_evicts_oldest() {
        let mut acc = XrMetricsAccumulator::new();
        for i in 0..(WINDOW + 50) {
            acc.record(frame(i as u64, true));
        }
        assert_eq!(acc.window_len(), WINDOW);
        assert_eq!(acc.frame_count(), (WINDOW + 50) as u64);
        // Oldest 50 evicted → min retained total is 50 (as ns here).
        let total = acc.total();
        assert_eq!(total.max_ns, (WINDOW + 49) as u64);
    }

    #[test]
    fn accumulator_extracted_ratio() {
        let mut acc = XrMetricsAccumulator::new();
        for i in 0..10 {
            acc.record(frame(1, i % 2 == 0)); // 5 of 10 extracted
        }
        assert!((acc.extracted_ratio() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn accumulator_total_aggregate() {
        let mut acc = XrMetricsAccumulator::new();
        for ms in 1..=100u64 {
            acc.record(frame(ms * 1_000_000, true));
        }
        let s = acc.total();
        assert_eq!(s.mean_ns, 50_500_000);
        assert_eq!(s.p95_ns, 95_000_000);
        assert_eq!(s.max_ns, 100_000_000);
    }

    #[test]
    fn should_log_fires_on_boundary() {
        let mut acc = XrMetricsAccumulator::new();
        for _ in 0..LOG_EVERY {
            acc.record(frame(1, true));
        }
        assert!(acc.should_log(), "should log at frame {LOG_EVERY}");
        acc.record(frame(1, true));
        assert!(!acc.should_log(), "should not log one frame later");
    }

    #[test]
    fn empty_frame_count_reads_latest() {
        let mut acc = XrMetricsAccumulator::new();
        acc.record(XrFrameTimings {
            empty_frame_count: 3,
            ..Default::default()
        });
        acc.record(XrFrameTimings {
            empty_frame_count: 7,
            ..Default::default()
        });
        assert_eq!(acc.empty_frame_count(), 7);
    }

    #[test]
    fn max_eye_metrics_take_both_eyes() {
        let mut acc = XrMetricsAccumulator::new();
        acc.record(XrFrameTimings {
            eye_evictions: [2, 9],
            eye_mesh_count: [11, 4],
            eye_cache_misses: [3, 7],
            ..Default::default()
        });
        assert_eq!(acc.max_eye_evictions(), 9);
        assert_eq!(acc.max_eye_mesh_count(), 11);
        assert_eq!(acc.max_eye_cache_misses(), 7);
    }

    #[test]
    fn eye_cpu_split_aggregates_per_eye() {
        let mut acc = XrMetricsAccumulator::new();
        acc.record(XrFrameTimings {
            eye_draw_list_ns: [4_000_000, 5_000_000],
            eye_gpu_passes_ns: [15_000_000, 14_000_000],
            ..Default::default()
        });
        // draw-list build and gpu-pass encode are tracked independently per eye.
        assert_eq!(acc.stat(|f| f.eye_draw_list_ns[0]).mean_ns, 4_000_000);
        assert_eq!(acc.stat(|f| f.eye_gpu_passes_ns[0]).mean_ns, 15_000_000);
        assert_eq!(acc.stat(|f| f.eye_draw_list_ns[1]).mean_ns, 5_000_000);
        assert_eq!(acc.stat(|f| f.eye_gpu_passes_ns[1]).mean_ns, 14_000_000);
        // The inline log line surfaces the eye-0 CPU split.
        assert!(acc.log_line().contains("eye0(draw_p95="));
    }

    #[test]
    fn gpu_execution_timings_aggregate_per_eye() {
        let mut acc = XrMetricsAccumulator::new();
        acc.record(XrFrameTimings {
            eye_gpu_pbr_ns: [6_000_000, 7_000_000],
            eye_gpu_sample_id: [1, 2],
            eye_gpu_tonemap_ns: [1_000_000, 1_200_000],
            eye_swapchain_copy_ns: [800_000, 900_000],
            eye_depth_copy_ns: [500_000, 600_000],
            eye_overdraw: [12_000, 20_000],
            ..Default::default()
        });
        // Real GPU-execution + copy timings are tracked independently per eye.
        assert_eq!(acc.stat(|f| f.eye_gpu_pbr_ns[0]).mean_ns, 6_000_000);
        assert_eq!(acc.stat(|f| f.eye_gpu_tonemap_ns[1]).mean_ns, 1_200_000);
        assert_eq!(acc.stat(|f| f.eye_swapchain_copy_ns[1]).mean_ns, 900_000);
        assert_eq!(acc.stat(|f| f.eye_depth_copy_ns[0]).mean_ns, 500_000);
        // Overdraw takes the max across both eyes (confirmation metric Tier-1 #3).
        assert_eq!(acc.max_eye_overdraw(), 20_000);
        // The inline log line surfaces the eye-0 GPU + copy headline.
        let line = acc.log_line();
        assert!(line.contains("pbr_p95="));
        assert!(line.contains("copy_p95="));
        assert!(line.contains("overdraw_max=20000"));
    }

    #[test]
    fn write_json_lands_in_given_dir() {
        // The dump must be written into the directory it is handed (on Android
        // this is the external-files dir so `adb pull` can retrieve it), and the
        // dir is created if absent. Exercises `write_json_in` directly to avoid
        // touching the process-global `METRICS_OUTPUT_DIR`.
        let dir = std::env::temp_dir().join(format!(
            "idtx-xr-metrics-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir); // clean any prior run
        let mut acc = XrMetricsAccumulator::new();
        for ms in 1..=5u64 {
            acc.record(frame(ms * 1_000_000, true));
        }
        acc.write_json_in(&dir, "mystage")
            .expect("write_json_in should create the dir and write the dump");

        let written: Vec<_> = std::fs::read_dir(&dir)
            .expect("output dir created")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("idtx-xr-metrics-mystage-") && n.ends_with(".json"))
            .collect();
        assert_eq!(written.len(), 1, "exactly one dump written: {written:?}");
        let body = std::fs::read_to_string(dir.join(&written[0])).unwrap();
        assert!(body.contains("\"stage\": \"mystage\""), "stage recorded in JSON");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metrics_output_dir_defaults_to_temp_when_unset() {
        // With no override installed in this test process, the default resolves
        // to the system temp dir (the desktop behaviour).
        assert_eq!(metrics_output_dir(), std::env::temp_dir());
    }
}
