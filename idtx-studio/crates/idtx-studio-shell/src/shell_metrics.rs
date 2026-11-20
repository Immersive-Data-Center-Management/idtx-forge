//! Per-frame timing/metrics collection (feature = "metrics").
//!
//! Accumulates a [`ShellFrameSample`] per rendered frame and flushes them to a
//! JSON report. Compiled only under the `metrics` feature; referenced from the
//! render loop as `crate::shell_metrics::…`.

use std::io::Write as _;
use std::path::PathBuf;

/// All timing breakdowns captured for a single rendered frame.
pub struct ShellFrameSample {
    pub frame: u64,
    pub frame_total_ms: f64,
    pub tick_ms: f64,
    /// Wall time of the USD C++ sampling call + ECS write-back inside
    /// `tick_animation`.  Non-zero only on frames where the animation clock
    /// advanced and a resample was needed.  For `CpuOpenUsd` skeletal
    /// scenes this is the dominant per-frame cost.
    pub tick_animation_sample_ms: f64,
    pub render_one_frame_ms: f64,
    pub extract_ms: f64,
    pub gpu_render_ms: f64,
    pub egui_run_ui_ms: f64,
    pub scene_tree_ms: f64,
    pub egui_tessellate_ms: f64,
    pub egui_submit_ms: f64,
    // extract sub-phases
    pub extract_materials_ms: f64,
    pub extract_meshes_ms: f64,
    pub extract_instancers_ms: f64,
    pub extract_topo_promote_ms: f64,
    // render sub-phases
    pub render_total_ms: f64,
    pub render_camera_lights_ms: f64,
    pub render_mesh_classify_ms: f64,
    pub render_transform_update_ms: f64,
    pub render_material_rebuild_ms: f64,
    pub render_cache_miss_ms: f64,
    pub render_evict_ms: f64,
    pub render_instance_cache_ms: f64,
    pub render_draw_list_ms: f64,
    pub render_gpu_passes_ms: f64,
    pub mesh_count: u32,
    pub instance_count: u32,
    pub cache_hits: u32,
    pub cache_misses: u32,
    pub cache_transform_updates: u32,
    pub cache_material_rebuilds: u32,
    pub evictions: u32,
}

pub struct ShellMetrics {
    pub scene_name: String,
    samples: Vec<ShellFrameSample>,
    frame_count: u64,
    flush_every: usize,
    output_path: PathBuf,
}

impl ShellMetrics {
    pub fn new(scene_name: impl Into<String>, output_path: PathBuf, flush_every: usize) -> Self {
        Self {
            scene_name: scene_name.into(),
            samples: Vec::with_capacity(flush_every),
            frame_count: 0,
            flush_every,
            output_path,
        }
    }

    pub fn push(&mut self, s: ShellFrameSample) {
        self.samples.push(s);
        self.frame_count += 1;
        if self.samples.len() >= self.flush_every {
            let _ = self.flush();
        }
    }

    pub fn next_frame_index(&self) -> u64 {
        self.frame_count
    }

    /// Write all accumulated samples to JSON, overwriting the file each time.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if self.samples.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::File::create(&self.output_path)?;
        writeln!(f, "{{")?;
        writeln!(f, "  \"schema_version\": 4,")?;
        writeln!(f, "  \"scene\": \"{}\",", self.scene_name)?;
        writeln!(f, "  \"total_frames_captured\": {},", self.frame_count)?;
        writeln!(f, "  \"frames\": [")?;
        let n = self.samples.len();
        for (i, s) in self.samples.iter().enumerate() {
            let comma = if i + 1 < n { "," } else { "" };
            writeln!(f, "    {{")?;
            writeln!(f, "      \"frame\": {},", s.frame)?;
            writeln!(f, "      \"frame_total_ms\": {:.3},", s.frame_total_ms)?;
            writeln!(f, "      \"tick_ms\": {:.3},", s.tick_ms)?;
            writeln!(
                f,
                "      \"tick_animation_sample_ms\": {:.3},",
                s.tick_animation_sample_ms
            )?;
            writeln!(
                f,
                "      \"render_one_frame_ms\": {:.3},",
                s.render_one_frame_ms
            )?;
            writeln!(f, "      \"extract_ms\": {:.3},", s.extract_ms)?;
            writeln!(f, "      \"gpu_render_ms\": {:.3},", s.gpu_render_ms)?;
            writeln!(f, "      \"egui_run_ui_ms\": {:.3},", s.egui_run_ui_ms)?;
            writeln!(f, "      \"scene_tree_ms\": {:.3},", s.scene_tree_ms)?;
            writeln!(
                f,
                "      \"egui_tessellate_ms\": {:.3},",
                s.egui_tessellate_ms
            )?;
            writeln!(f, "      \"egui_submit_ms\": {:.3},", s.egui_submit_ms)?;
            writeln!(f, "      \"extract_phases\": {{")?;
            writeln!(
                f,
                "        \"materials_ms\": {:.3},",
                s.extract_materials_ms
            )?;
            writeln!(f, "        \"meshes_ms\": {:.3},", s.extract_meshes_ms)?;
            writeln!(
                f,
                "        \"instancers_ms\": {:.3},",
                s.extract_instancers_ms
            )?;
            writeln!(
                f,
                "        \"topo_promote_ms\": {:.3}",
                s.extract_topo_promote_ms
            )?;
            writeln!(f, "      }},")?;
            writeln!(f, "      \"render_phases\": {{")?;
            writeln!(f, "        \"total_ms\": {:.3},", s.render_total_ms)?;
            writeln!(
                f,
                "        \"camera_lights_ms\": {:.3},",
                s.render_camera_lights_ms
            )?;
            writeln!(
                f,
                "        \"mesh_classify_ms\": {:.3},",
                s.render_mesh_classify_ms
            )?;
            writeln!(
                f,
                "        \"transform_update_ms\": {:.3},",
                s.render_transform_update_ms
            )?;
            writeln!(
                f,
                "        \"material_rebuild_ms\": {:.3},",
                s.render_material_rebuild_ms
            )?;
            writeln!(
                f,
                "        \"cache_miss_build_ms\": {:.3},",
                s.render_cache_miss_ms
            )?;
            writeln!(f, "        \"evict_ms\": {:.3},", s.render_evict_ms)?;
            writeln!(
                f,
                "        \"instance_cache_ms\": {:.3},",
                s.render_instance_cache_ms
            )?;
            writeln!(
                f,
                "        \"draw_list_build_ms\": {:.3},",
                s.render_draw_list_ms
            )?;
            writeln!(
                f,
                "        \"gpu_passes_ms\": {:.3},",
                s.render_gpu_passes_ms
            )?;
            writeln!(f, "        \"mesh_count\": {},", s.mesh_count)?;
            writeln!(f, "        \"instance_count\": {},", s.instance_count)?;
            writeln!(f, "        \"cache_hits\": {},", s.cache_hits)?;
            writeln!(f, "        \"cache_misses\": {},", s.cache_misses)?;
            writeln!(
                f,
                "        \"cache_transform_updates\": {},",
                s.cache_transform_updates
            )?;
            writeln!(
                f,
                "        \"cache_material_rebuilds\": {},",
                s.cache_material_rebuilds
            )?;
            writeln!(f, "        \"evictions\": {}", s.evictions)?;
            writeln!(f, "      }}")?;
            writeln!(f, "    }}{}", comma)?;
        }
        writeln!(f, "  ]")?;
        writeln!(f, "}}")?;
        Ok(())
    }
}

impl Drop for ShellMetrics {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
