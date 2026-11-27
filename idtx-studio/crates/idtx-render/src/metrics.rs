//! Per-operation render timing metrics behind the `metrics` Cargo feature.
//!
//! When the feature is disabled all types are zero-size and all functions are
//! `#[inline(always)]` no-ops — no runtime overhead.
//!
//! # Usage
//!
//! ```ignore
//! let t = FrameTimer::start("stage_open");
//! // … do work …
//! let sample = t.finish();   // available only with feature = "metrics"
//! session.push(sample);
//! session.save_json(Path::new("metrics_baseline.json")).unwrap();
//! ```

#[cfg(feature = "metrics")]
mod inner {
    use std::path::Path;
    use std::time::Instant;

    use crate::convert::{AsF64, nanos_u64};

    /// One-shot timer for a single labeled measurement.
    #[allow(missing_debug_implementations)]
    pub struct FrameTimer {
        label: String,
        start: Instant,
    }

    impl FrameTimer {
        /// Start the timer with a descriptive label.
        pub fn start(label: impl Into<String>) -> Self {
            Self {
                label: label.into(),
                start: Instant::now(),
            }
        }

        /// Stop and return the elapsed measurement.
        pub fn finish(self) -> MetricSample {
            MetricSample {
                label: self.label,
                elapsed_ns: nanos_u64(self.start.elapsed().as_nanos()),
            }
        }
    }

    /// A single labeled timing sample.
    #[derive(Debug, Clone)]
    pub struct MetricSample {
        pub label: String,
        pub elapsed_ns: u64,
    }

    impl MetricSample {
        pub fn elapsed_ms(&self) -> f64 {
            self.elapsed_ns.as_f64() * 1e-6
        }
    }

    /// Per-frame mesh GPU buffer cache statistics.
    #[derive(Debug, Clone, Default)]
    pub struct CacheFrameStats {
        /// Meshes whose cached entry was reused without modification.
        pub hits: usize,
        /// Meshes whose world transform was updated via `write_buffer`.
        pub transform_updates: usize,
        /// Meshes whose material bind group was rebuilt.
        pub material_rebuilds: usize,
        /// New entries allocated this frame (cold path).
        pub misses: usize,
        /// Stale entries evicted (prims no longer in snapshot).
        pub evictions: usize,
    }

    impl CacheFrameStats {
        pub fn hit_rate(&self) -> f64 {
            let total = self.hits + self.transform_updates + self.material_rebuilds + self.misses;
            if total == 0 {
                1.0
            } else {
                self.hits.as_f64() / total.as_f64()
            }
        }
    }

    /// Per-frame timing breakdown for one `Renderer::render()` call.
    ///
    /// All timings are CPU-side wall-clock nanoseconds. GPU command *submission*
    /// is included in `gpu_passes_ns`; actual GPU execution is asynchronous and
    /// not captured here.
    #[derive(Debug, Clone, Default)]
    pub struct RenderFrameTimings {
        /// Sequence of the completed older GPU sample for this view; zero means
        /// unavailable. Never interpret an unavailable sample as zero GPU work.
        pub gpu_sample_id: u64,
        /// Actual render target extent, independent of a mirror window.
        pub viewport: [u32; 2],
        /// Total wall time of the entire `render()` call.
        pub total_ns: u64,
        /// Camera resolution + scene auto-fit: `fit_camera_to_snapshot` when the
        /// snapshot carries no authored camera (a full CPU per-vertex world-space
        /// AABB pass over every mesh, `compute_scene_aabb`), plus view/proj and the
        /// editor axis-gizmo prep. Near-zero when the scene has an authored camera;
        /// the dominant term for auto-framed scenes with large vertex counts.
        pub camera_fit_ns: u64,
        /// Camera + lights uniform buffer creation.
        pub camera_lights_ns: u64,
        /// `MaterialX` WGSL pipeline pre-pass: `hash_wgsl` over every mesh's shader
        /// source to probe the pipeline cache, plus any first-sight pipeline
        /// compilation. `materialx_pipeline_compiles` counts the compiles; on a warm
        /// frame that is 0 and this is pure (wasted) re-hashing cost.
        /// `0` unless the `materialx-wgsl` feature is enabled.
        pub materialx_prep_ns: u64,
        /// Static draw-merge planner (F1/F7): the per-frame `MergeKey` HashMap group
        /// + sort over all meshes and single-instance batches. UDIM / transparent /
        /// skinned items are excluded before hashing, so this is cheap for
        /// UDIM-dominated scenes and grows with mergeable opaque draw count.
        pub merge_plan_ns: u64,
        /// Phase 1: classify each mesh as hit / transform-dirty / material-dirty / miss.
        pub mesh_classify_ns: u64,
        /// Phase 2: `write_buffer` for dirty world transforms and skin joint matrices.
        pub transform_update_ns: u64,
        /// Phase 3: rebuild material bind groups for changed materials.
        pub material_rebuild_ns: u64,
        /// Phase 4: allocate full GPU cache entries for cache misses.
        pub cache_miss_build_ns: u64,
        /// Phase 5: evict stale cache entries for prims no longer in the snapshot.
        pub evict_ns: u64,
        /// All four instance-batch cache phases combined.
        pub instance_cache_ns: u64,
        /// Build the draw list (Arc clones from cache).
        pub draw_list_build_ns: u64,
        /// GPU command encoding + `queue.submit()` for all render passes.
        /// This is CPU-side submission wall-clock — GPU execution is asynchronous
        /// and captured separately in `gpu_pbr_ns` / `gpu_tonemap_ns`.
        pub gpu_passes_ns: u64,
        // ── `gpu_passes_ns` decomposition (F1 draw-submission instrumentation) ──
        // These named buckets partition the CPU submission window so the dominant
        // term is visible instead of hidden in one number. They sum to
        // `gpu_passes_ns` minus a small unattributed remainder (pass teardown +
        // timestamp resolve); the sum-consistency invariant is asserted in the
        // integration test. See `docs/draw-submission-analysis.md` §4.
        //
        /// Create the command encoder, begin the PBR pass, and set the two
        /// frame-constant bind groups (camera + lights). Fixed per pass — matters
        /// ×2 in XR (per eye) and ×(pass count).
        pub pass_setup_ns: u64,
        /// Record the opaque draws (Pass 1 per-mesh + Pass 3 opaque instanced +
        /// editor axis overlay) into the command stream. The suspected F1 hot spot.
        pub encode_opaque_ns: u64,
        /// Record the transparent draws (Pass 2 back-to-front + Pass 4 instanced),
        /// including the depth sort.
        pub encode_transparent_ns: u64,
        /// Record the tonemap (post-process) pass.
        pub encode_tonemap_ns: u64,
        /// `encoder.finish()` alone — backend (e.g. Metal) translation of the
        /// recorded command stream. Split from `submit_ns` so recording cost can
        /// be told apart from driver-translation cost.
        pub finish_ns: u64,
        /// `queue.submit()` alone — driver hand-off of the finished command buffer.
        pub submit_ns: u64,
        /// Total `draw_indexed` / `draw` calls issued this frame (all passes).
        /// Denominator for the `ns_per_draw` north-star metric.
        pub draw_calls: u32,
        /// Total render-pass commands issued (set_pipeline + set_bind_group +
        /// set_vertex/index_buffer + draw). The real per-frame work unit.
        pub commands_encoded: u32,
        /// `set_pipeline` calls that actually changed the bound pipeline.
        /// `draw_calls - pipeline_switches` is the redundant-pipeline-bind headroom
        /// a state-dedup pass could remove.
        pub pipeline_switches: u32,
        /// Total `set_bind_group` calls issued this frame.
        pub bind_group_sets: u32,
        /// XR eye / view index this frame belongs to when rendering stereo; `None`
        /// for mono desktop frames. Stamped by the XR caller after `render()`.
        pub eye: Option<u8>,
        /// Actual GPU execution time of the PBR (geometry) render pass, measured
        /// via a `TIMESTAMP_QUERY` query set. `0` when the adapter does not
        /// support timestamp queries (e.g. CI software renderers).
        pub gpu_pbr_ns: u64,
        /// Actual GPU execution time of the tonemap (post-process) pass, measured
        /// via a `TIMESTAMP_QUERY` query set. `0` when unsupported.
        pub gpu_tonemap_ns: u64,
        /// Actual GPU execution time of the shadow depth pre-pass (all caster
        /// depth-only draws into the shadow atlas), measured via a `TIMESTAMP_QUERY`
        /// query set (slots 7→8). `0` when timestamps are unsupported, when soft
        /// shadows are disabled (no pre-pass recorded), or when the `ShadowCache`
        /// short-circuited every atlas layer on a static frame (cache hit → 0
        /// depth draws). The north-star cost metric for the shadow feature: compare
        /// against `gpu_pbr_ns` to size the shadow budget on a given target GPU.
        pub gpu_shadow_ns: u64,
        /// GPU execution time of the PBR pass **opaque** segment (Pass 1 per-mesh
        /// + Pass 1b merged + Pass 3 instanced, all drawn before the skybox),
        /// measured via a within-pass timestamp written at the single
        /// opaque→(skybox+transparent) boundary.
        ///
        /// **Approximate.** Within-pass timestamps are not pipeline-serialized — a
        /// timestamp records when the GPU *reaches* that command, while earlier
        /// fragment work may still be draining — so the opaque/transparent split is
        /// a proxy, not an exact partition. Only the whole-pass `gpu_pbr_ns`
        /// (slots 0→1) is exact. `0` when the adapter lacks
        /// `TIMESTAMP_QUERY_INSIDE_PASSES` (e.g. many Apple/Metal GPUs).
        pub gpu_pbr_opaque_ns: u64,
        /// GPU execution time of the PBR pass **skybox + transparent** segment (the
        /// dome backdrop + Pass 2 back-to-front + Pass 4 instanced, plus any editor
        /// axis overlay and pass teardown that trail them). Same approximation
        /// caveats as `gpu_pbr_opaque_ns`; `0` when `TIMESTAMP_QUERY_INSIDE_PASSES`
        /// is unavailable.
        pub gpu_pbr_transparent_ns: u64,
        /// Total fragment-shader invocations across the PBR (geometry) pass,
        /// measured via a `PIPELINE_STATISTICS_QUERY`
        /// (`FRAGMENT_SHADER_INVOCATIONS`). Overdraw factor = this ÷ (width×height).
        ///
        /// `0` when the adapter lacks `PIPELINE_STATISTICS_QUERY` — notably **all
        /// Metal backends**, so this is only populated on D3D12/Vulkan runs. A `0`
        /// means "unsupported on this backend", never "no overdraw". Counts cover
        /// the whole PBR pass (opaque + transparent + overlay), and MSAA without
        /// sample-rate shading keeps it ≈ per-pixel (helper invocations on
        /// small-triangle geometry inflate it slightly).
        pub fragment_shader_invocations: u64,
        /// Number of individual mesh instances in this frame's snapshot.
        pub mesh_count: u32,
        /// Number of instanced-batch instances in this frame's snapshot.
        pub instance_count: u32,
        /// Static draw-merge batches drawn this frame (F1/F7). Each collapses ≥2
        /// opaque, non-skinned meshes/1-instance batches that share a pipeline +
        /// material into a single baked `draw_indexed`.
        pub merged_batch_count: u32,
        /// Source meshes/1-instance batches folded into merged batches this frame.
        /// `merged_mesh_count − merged_batch_count` is the eliminated draw count.
        pub merged_mesh_count: u32,
        /// Merged batches whose world-space vertex buffer was (re)baked this
        /// frame. Equals `merged_batch_count` on a cold/structural rebuild; on a
        /// warm transform-only frame it is just the count of groups whose folded
        /// members moved (0 when nothing moved). Guards the fix that lets folded
        /// rigid geometry follow per-frame pose patches without a serial bump.
        pub merged_batch_rebakes: u32,
        pub cache_hits: u32,
        pub cache_misses: u32,
        pub cache_transform_updates: u32,
        pub cache_material_rebuilds: u32,
        pub evictions: u32,
        /// `total_ns` minus the sum of every named CPU phase
        /// (`camera_fit` + `camera_lights` + `materialx_prep` + `merge_plan` +
        /// the five mesh-cache phases + `instance_cache` + `draw_list_build` +
        /// `gpu_passes`). The catch-all bucket: any CPU time inside `render()` not
        /// attributed to a named phase lands here, so the decomposition is always
        /// complete. A large `residual_ns` means an unmeasured hot span still
        /// exists and a new named timer is warranted. Computed, never a raw timer.
        pub residual_ns: u64,
        /// Compatibility field: always zero since S23's delayed query collection
        /// never drains the current GPU submission. Use completed GPU samples
        /// and the adapter's whole-frame/pacing capture for classification.
        pub gpu_wait_ns: u64,
        /// `MaterialX` WGSL pipelines compiled this frame (new cache entries built via
        /// `try_build_materialx_pipelines`). `0` on a healthy warm frame; a nonzero
        /// value every frame means the pipeline cache is thrashing (a new WGSL key
        /// each frame) — a correctness/perf bug, not steady state.
        pub materialx_pipeline_compiles: u32,
        /// Synthetic-camera scene-AABB recomputes this frame: `1` when the snapshot
        /// carried no authored camera AND its geometry signature (mesh count +
        /// per-mesh serial + world matrix) differed from the cached one, forcing the
        /// full per-vertex `compute_scene_aabb` pass; `0` on a cache hit or whenever
        /// the snapshot has an authored camera (the fit path is skipped entirely).
        /// On a warm perf-capture loop over one camera-less snapshot this is `1` on
        /// the first frame and `0` thereafter — the signal that the AABB cache
        /// collapsed the ~48 ms per-frame `camera_fit` to a cheap signature check.
        pub scene_aabb_recomputes: u32,
        /// Colored-glass draws (Pass 2 static + Pass 4 instanced) routed to the
        /// per-channel constant-dst blend pipeline this frame — the draws that carry
        /// a `transmission_color` tint via `set_blend_constant`. `0` for scenes with
        /// no colored transparent materials.
        pub colored_glass_draws: u32,
    }

    /// Collects samples during a test or benchmark session and serialises to JSON.
    #[derive(Default)]
    #[allow(missing_debug_implementations)]
    pub struct MetricsSession {
        pub id: String,
        pub samples: Vec<MetricSample>,
    }

    impl MetricsSession {
        pub fn new(id: impl Into<String>) -> Self {
            Self {
                id: id.into(),
                samples: Vec::new(),
            }
        }

        pub fn push(&mut self, s: MetricSample) {
            self.samples.push(s);
        }

        /// Record a pre-computed elapsed value (useful around FFI calls).
        pub fn record(&mut self, label: impl Into<String>, elapsed_ns: u64) {
            self.samples.push(MetricSample {
                label: label.into(),
                elapsed_ns,
            });
        }

        /// Pretty-print all samples to stdout.
        pub fn print(&self) {
            println!("=== Metrics: {} ===", self.id);
            for s in &self.samples {
                println!("  {:40} {:8.2} ms", s.label, s.elapsed_ms());
            }
        }

        /// Serialise to a JSON file for future comparison.
        ///
        /// The output is a simple array of `{label, elapsed_ns, elapsed_ms}` objects.
        /// If the file already exists it is overwritten so the baseline stays fresh.
        pub fn save_json(&self, path: &Path) -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(path)?;
            writeln!(f, "{{")?;
            writeln!(f, "  \"session_id\": \"{}\",", self.id)?;
            writeln!(f, "  \"samples\": [")?;
            for (i, s) in self.samples.iter().enumerate() {
                let comma = if i + 1 < self.samples.len() { "," } else { "" };
                writeln!(
                    f,
                    "    {{\"label\": \"{}\", \"elapsed_ns\": {}, \"elapsed_ms\": {:.3}}}{}",
                    s.label,
                    s.elapsed_ns,
                    s.elapsed_ms(),
                    comma
                )?;
            }
            writeln!(f, "  ]")?;
            writeln!(f, "}}")?;
            Ok(())
        }
    }
}

#[cfg(feature = "metrics")]
pub use inner::{CacheFrameStats, FrameTimer, MetricSample, MetricsSession, RenderFrameTimings};

// ── Zero-overhead no-ops when the feature is disabled ─────────────────────────

#[cfg(not(feature = "metrics"))]
pub struct FrameTimer;

#[cfg(not(feature = "metrics"))]
impl FrameTimer {
    #[inline(always)]
    pub fn start(_label: impl Into<String>) -> Self {
        Self
    }
    #[inline(always)]
    pub fn finish(self) {}
}
