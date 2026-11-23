//! Offscreen PBR renderer — turns a [`RenderWorldSnapshot`] into RGBA pixels.
//!
//! The pass is a full Cook-Torrance PBR pass
//! with per-material texture binding (diffuse, normal, metallic, roughness,
//! occlusion, emissive) and a scene-light uniform array (up to 8 lights).
//!
//! UV V-flip is applied during mesh extraction (see [`crate::mesh`]).  Normal
//! maps use vertex TBN when authored tangents are available (w ≠ 0), falling
//! back to screen-space derivative TBN when tangents are absent.

use std::sync::mpsc;

use wgpu::util::DeviceExt;
use crate::batch_limits::{checked_bytes, DrawBatchLimits, GeometryCounts, GeometryError};

use crate::convert::{AsF32, u32_sat};
#[cfg(feature = "metrics")]
use crate::convert::{AsF64, nanos_u64};

use crate::material::RenderMaterialDesc;
#[cfg(feature = "editor-overlay")]
use crate::overlay::{AxisGpuData, AxisLine, AxisPass, push_world_axes};
use crate::overlay::{OverlayLine, OverlayPass, push_obb_corner_ticks};
use crate::pipeline::{
    BlitPipeline, CameraUniforms, DepthResolvePipeline, EnvUniforms, HDR_FORMAT, LIGHT_DISK,
    LIGHT_DISTANT, LIGHT_DOME, LIGHT_DOME_PROC, LIGHT_RECT, LIGHT_SPHERE, LightGpu, LightsUniforms,
    MAX_LIGHTS, MaterialUniforms, MeshUniforms, PbrPipeline, SHADOW_ATLAS_DIM,
    ShadowUniforms, SkyboxUniforms, ToneMapPipeline, build_blit_pipeline,
    build_depth_resolve_reverse_z_pipeline,
    build_depth_resolve_pipeline, build_pbr_pipeline, build_skybox_pipeline,
    build_tonemap_pipeline, clamp_sample_count_to_support, resolve_sample_count,
    resolve_z_prepass,
};
#[cfg(feature = "materialx-wgsl")]
use crate::pipeline::{MaterialxPipelines, try_build_materialx_pipelines};
use crate::scene_extract::{
    RenderCameraDesc, RenderLightDesc, RenderLightKind, RenderWorldSnapshot, SHADOW_BUDGET,
    ShadowSlot, ShadowTier,
};
use crate::texture_cache::{AssetReadFn, TextureCache, TextureDecodeConfig, TextureKind};
use crate::vertex::InstanceRaw;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[cfg(feature = "materialx-wgsl")]
fn hash_wgsl(src: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    h.finish()
}

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("no suitable wgpu adapter found")]
    NoAdapter,
    #[error("wgpu device request failed: {0}")]
    Device(String),
    #[error("readback buffer map failed: {0}")]
    MapFailed(String),
    #[error("geometry rejected: {0}")]
    Geometry(#[from] GeometryError),
}

// ── Renderer ──────────────────────────────────────────────────────────────────

// ── GPU buffer cache ──────────────────────────────────────────────────────────

/// Per-mesh GPU resources that persist across frames.
///
/// Keyed by `(prim_path, ordinal)` in `Renderer::mesh_gpu_cache`, where
/// `ordinal` disambiguates multiple instances that share the same prototype
/// prim path (e.g. PointInstancer-expanded meshes).
struct CachedMeshGpu {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    /// `UNIFORM | COPY_DST` — updated in-place via `queue.write_buffer` when
    /// the world transform changes, without recreating the bind group.
    mesh_ubuf: wgpu::Buffer,
    mesh_bg: wgpu::BindGroup,
    last_world_matrix: glam::Mat4,
    mat_bg: wgpu::BindGroup,
    /// `RenderMaterialDesc::fingerprint` at the time this entry was built.
    /// If this changes the `mat_bg` must be rebuilt.
    mat_fingerprint: u64,
    /// `GpuMeshDesc::serial` at the time this entry was built.
    /// If this changes (structural variant switch or geometry authoring) the
    /// entire cache entry must be evicted and rebuilt as a full cache miss.
    last_mesh_serial: u64,
    /// Static skin influence vertex buffer (joint indices + weights).
    skin_vbuf: Option<wgpu::Buffer>,
    /// `UNIFORM | COPY_DST` joint matrix palette — updated in-place each frame
    /// for animated skeletons without recreating the bind group.
    skin_ubuf: Option<wgpu::Buffer>,
    skin_bg: Option<wgpu::BindGroup>,
    /// True when this mesh's material is a `<UDIM>` template and the mesh is
    /// eligible for the tiled-array pipeline (static, non-skinned in v1). When
    /// set, `mat_bg` is built against `pipeline.material_udim_bgl` and drawn with
    /// `pipeline.udim.select(..)`. Always `false` for the non-UDIM fast path.
    udim: bool,
    #[cfg(feature = "materialx-wgsl")]
    wgsl_key: Option<u64>,
}

// ── Instance buffer cache ─────────────────────────────────────────────────────

/// Per-batch GPU resources that persist across frames.
///
/// Keyed by `prototype_key` in `Renderer::instance_batch_cache`.  The prototype
/// geometry buffers (`vbuf`, `ibuf`) are static; only the instance transform buffer
/// is updated when instances move or are added/removed.
struct CachedInstanceBatch {
    /// Interleaved vertex buffer for the prototype mesh geometry.
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    /// Per-instance world matrices — VERTEX | `COPY_DST`, updated in-place when
    /// instance transforms change without count change, reallocated on count change.
    instance_buf: wgpu::Buffer,
    instance_capacity: u32,
    instance_count: u32,
    mat_bg: wgpu::BindGroup,
    mat_fingerprint: u64,
    double_sided: bool,
    /// True when the material references a `<UDIM>` template. `mat_bg` is built
    /// against `pipeline.material_udim_bgl` and the batch is drawn with
    /// `pipeline.udim_instanced.select(..)`. False for the non-UDIM fast path.
    udim: bool,
}

// ── Static draw-merge cache (F1/F7) ──────────────────────────────────────────

/// Groups mergeable opaque, non-skinned items that share a pipeline and material.
///
/// The `wgpu-hal` Metal translation of `encoder.finish()` scales with the number
/// of recorded commands. After `BoundState` collapsed the redundant re-binds, the
/// residual is one draw + two bind groups + two buffer binds *per mesh* across
/// ~1789 unique `Kitchen_set` meshes. Two items with the same `MergeKey` produce
/// pixel-identical output when their world transforms are baked into vertices and
/// they are drawn with the shared identity group-2 `MeshUniforms` — so they can be
/// concatenated into a single `draw_indexed`, cutting the recorded command count.
#[derive(Clone, PartialEq, Eq, Hash)]
struct MergeKey {
    /// Double-sided pipeline (`material.double_sided || mesh.double_sided`).
    double_sided: bool,
    /// Negative-determinant world transform → mirrored (`FrontFace::Cw`) pipeline.
    mirrored: bool,
    /// Material content fingerprint — identical content shares one `mat_bg`.
    fingerprint: u64,
    /// `MaterialX` WGSL pipeline key; `None` for `UsdPreviewSurface` / feature off.
    #[cfg(feature = "materialx-wgsl")]
    wgsl_key: Option<u64>,
}

/// One source item folded into a merged batch: a `snapshot.meshes[i]` or a
/// single-instance `snapshot.instanced_batches[b]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MergeSource {
    Mesh(usize),
    Batch(usize),
}

/// A baked, concatenated draw batch collapsing ≥2 items sharing a [`MergeKey`]
/// into one `draw_indexed`. Vertices are world-space-baked, so the draw binds the
/// shared `identity_mesh_bg` at group 2 and the existing static pipelines.
struct CachedMergedBatch {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    mat_bg: wgpu::BindGroup,
    double_sided: bool,
    mirrored: bool,
    /// Material content fingerprint (copied from the group's [`MergeKey`]). Lets a
    /// warm frame confirm the cached batch still lines up with the freshly-planned
    /// merge group before doing an index-mapped selective re-bake.
    fingerprint: u64,
    /// FNV-1a hash of the folded members' world matrices at bake time. A warm
    /// frame re-hashes the current matrices and re-bakes only when it differs, so
    /// transform-only patches (which never bump `snapshot.serial`) still move
    /// baked world-space geometry — the axis-gizmo / aim-ray debug children that
    /// share a material fold into these batches and must track the controller pose.
    xform_sig: u64,
    members: Vec<MergeSource>,
    #[cfg(feature = "materialx-wgsl")]
    wgsl_key: Option<u64>,
    /// Source items folded into this batch (metrics only).
    #[cfg(feature = "metrics")]
    source_count: u32,
}

/// FNV-1a hash of a merge group's arity + folded members' world matrices. Used on
/// warm frames to detect when a folded item changed so only the affected merged
/// batch is re-baked (static groups keep their cached buffers). The signature
/// mixes each member's world matrix AND its per-mesh geometry `serial`, so it
/// fires both when a folded item *moves* (matrix) and when it is *reshaped*
/// (serial) — the latter is how the value-only geometry fast path (e.g. an
/// OpenExec-driven cube whose `size` changed) re-bakes just its group while the
/// global `snapshot.serial` stays stable.
fn merge_group_xform_sig(members: &[MergeSource], snapshot: &RenderWorldSnapshot) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bits: u64| {
        h ^= bits;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(members.len() as u64);
    for m in members {
        let (world, serial) = match *m {
            MergeSource::Mesh(i) => (
                snapshot.meshes[i].world_matrix,
                snapshot.meshes[i].mesh.serial,
            ),
            MergeSource::Batch(b) => (
                snapshot.instanced_batches[b].instance_transforms[0],
                snapshot.instanced_batches[b].mesh.serial,
            ),
        };
        for f in world.to_cols_array() {
            mix(u64::from(f.to_bits()));
        }
        mix(serial);
    }
    h
}

/// Concatenate every member of a merge group into a single world-space-baked
/// vertex/index pair. Shared by the full structural rebuild and the warm-frame
/// selective re-bake so both bake identical geometry.
fn bake_merge_group_geometry(
    members: &[MergeSource],
    snapshot: &RenderWorldSnapshot,
) -> Result<(Vec<crate::vertex::Vertex>, Vec<u32>), GeometryError> {
    let mut verts: Vec<crate::vertex::Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for m in members {
        let (mesh, world) = match *m {
            MergeSource::Mesh(i) => (&snapshot.meshes[i].mesh, snapshot.meshes[i].world_matrix),
            MergeSource::Batch(b) => (
                &snapshot.instanced_batches[b].mesh,
                snapshot.instanced_batches[b].instance_transforms[0],
            ),
        };
        let base = u32::try_from(verts.len()).map_err(|_| GeometryError::Overflow)?;
        crate::vertex::bake_transformed_into(
            &mut verts,
            &mesh.positions,
            &mesh.normals,
            &mesh.texcoords,
            &mesh.vertex_colors,
            &mesh.tangents,
            world,
        );
        for &ix in &mesh.indices {
            if ix as usize >= mesh.positions.len() { return Err(GeometryError::InvalidGeometry); }
            indices.push(ix.checked_add(base).ok_or(GeometryError::Overflow)?);
        }
    }
    Ok((verts, indices))
}

fn source_counts(source: MergeSource, snapshot: &RenderWorldSnapshot) -> GeometryCounts {
    let mesh = match source {
        MergeSource::Mesh(i) => &snapshot.meshes[i].mesh,
        MergeSource::Batch(b) => &snapshot.instanced_batches[b].mesh,
    };
    GeometryCounts { vertices: mesh.positions.len(), indices: mesh.indices.len() }
}

fn split_merge_group(members: Vec<MergeSource>, snapshot: &RenderWorldSnapshot, limits: DrawBatchLimits) -> Result<Vec<Vec<MergeSource>>, GeometryError> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    let mut counts = GeometryCounts::default();
    for member in members {
        let next = source_counts(member, snapshot);
        limits.check(next)?; // An oversized source must not escape via the unmerged path.
        let combined = counts.checked_add(next);
        if combined.and_then(|total| limits.check(total)).is_err() {
            groups.push(std::mem::take(&mut current));
            counts = GeometryCounts::default();
        }
        counts = counts.checked_add(next)?;
        current.push(member);
    }
    if !current.is_empty() { groups.push(current); }
    Ok(groups)
}

// ── Render-pass state dedup ─────────────────────────────────────────────────

/// Tracks the currently-bound render-pass state so redundant `set_*` commands
/// can be skipped before they reach the command stream.
///
/// The draw-submission analysis (`docs/draw-submission-analysis.md` §7) showed
/// ~97 % of the CPU submission window is `encoder.finish()` — the wgpu-hal Metal
/// translation of the recorded command stream — and that it scales linearly with
/// the *number of recorded commands* (~600 ns/command). Yet the opaque draw loop
/// re-issues `set_pipeline` and both bind groups on every mesh even though, after
/// the pipeline-key sort, consecutive meshes almost always share the same
/// pipeline (`Kitchen_set`: 2 real switches across 1789 draws). Skipping the
/// redundant re-binds is a pure command-count reduction: fewer commands recorded
/// → less `finish()` translation, with no shader or draw-count change.
///
/// Dedup uses wgpu resource identity (`RenderPipeline`/`BindGroup`/`Buffer` all
/// implement `Eq` over their inner Arc handle). Skipping a re-bind is always safe:
/// a resource reported as still bound *is* still bound. A pipeline switch only
/// invalidates bind groups whose layout is incompatible, but any two pipelines
/// that accept the same bind-group object at a slot share a compatible layout
/// there, so keeping an identical binding across a pipeline change never breaks
/// draw-time validation. Buffers are always bound as full `slice(..)` ranges with
/// a fixed index format, so resource identity captures the full binding.
#[derive(Default)]
struct BoundState {
    pipeline: Option<wgpu::RenderPipeline>,
    bg: [Option<wgpu::BindGroup>; 4],
    vbuf: [Option<wgpu::Buffer>; 2],
    ibuf: Option<wgpu::Buffer>,
    /// Recorded (non-redundant) `set_*` commands: pipeline + bind group + vertex
    /// + index buffer sets that actually reached the pass. Draws are counted at
    /// their call sites (they are never deduplicated).
    #[cfg(feature = "metrics")]
    commands_encoded: u32,
    #[cfg(feature = "metrics")]
    pipeline_switches: u32,
    #[cfg(feature = "metrics")]
    bind_group_sets: u32,
}

impl BoundState {
    /// Forget all tracked bindings at a render-pass boundary — a fresh
    /// `RenderPass` starts with nothing bound. Metric counters are preserved so
    /// they accumulate across every pass in the frame.
    fn reset_bindings(&mut self) {
        self.pipeline = None;
        self.bg = Default::default();
        self.vbuf = Default::default();
        self.ibuf = None;
    }

    #[inline]
    fn set_pipeline(&mut self, pass: &mut wgpu::RenderPass<'_>, pipe: &wgpu::RenderPipeline) {
        if self.pipeline.as_ref() == Some(pipe) {
            return;
        }
        pass.set_pipeline(pipe);
        self.pipeline = Some(pipe.clone());
        #[cfg(feature = "metrics")]
        {
            self.commands_encoded += 1;
            self.pipeline_switches += 1;
        }
    }

    #[inline]
    fn set_bind_group(&mut self, pass: &mut wgpu::RenderPass<'_>, slot: u32, bg: &wgpu::BindGroup) {
        let cur = &mut self.bg[slot as usize];
        if cur.as_ref() == Some(bg) {
            return;
        }
        pass.set_bind_group(slot, bg, &[]);
        *cur = Some(bg.clone());
        #[cfg(feature = "metrics")]
        {
            self.commands_encoded += 1;
            self.bind_group_sets += 1;
        }
    }

    #[inline]
    fn set_vertex_buffer(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        slot: u32,
        buf: &wgpu::Buffer,
    ) {
        let cur = &mut self.vbuf[slot as usize];
        if cur.as_ref() == Some(buf) {
            return;
        }
        pass.set_vertex_buffer(slot, buf.slice(..));
        *cur = Some(buf.clone());
        #[cfg(feature = "metrics")]
        {
            self.commands_encoded += 1;
        }
    }

    #[inline]
    fn set_index_buffer(&mut self, pass: &mut wgpu::RenderPass<'_>, buf: &wgpu::Buffer) {
        if self.ibuf.as_ref() == Some(buf) {
            return;
        }
        pass.set_index_buffer(buf.slice(..), wgpu::IndexFormat::Uint32);
        self.ibuf = Some(buf.clone());
        #[cfg(feature = "metrics")]
        {
            self.commands_encoded += 1;
        }
    }
}

// ── GPU timestamp queries ──────────────────────────────────────────────────────

/// GPU timestamp-query resources for measuring real render-pass execution time.
///
/// Present only when the `metrics` feature is on **and** the adapter reports
/// `Features::TIMESTAMP_QUERY`. On adapters without support (common CI software
/// renderers) this is `None` and the GPU timing fields stay `0`.
#[cfg(feature = "metrics")]
struct GpuTimestamps {
    query_set: wgpu::QuerySet,
    /// `QUERY_RESOLVE | COPY_SRC` — `resolve_query_set` writes u64 ticks here.
    resolve_buf: wgpu::Buffer,
    /// Nanoseconds per timestamp tick (`Queue::get_timestamp_period`).
    period_ns: f32,
    /// Whether `TIMESTAMP_QUERY_INSIDE_PASSES` was enabled on the device. When
    /// `false` the interior slot (4) is never written and the opaque vs
    /// transparent split degrades to `0` — only whole-pass PBR/tonemap remain.
    inside_passes: bool,
}

#[cfg(feature = "metrics")]
impl GpuTimestamps {
    /// 9 timestamp slots: PBR pass begin/end (0,1) + tonemap pass begin/end (2,3)
    /// + one interior PBR boundary: after all opaque geometry / before the skybox +
    /// transparent segment (4); + shadow depth pre-pass begin/end (7,8). Slots 5-6
    /// are reserved/unused. Slot 4 is only written when `inside_passes` is true.
    /// Slots 7-8 are only
    /// written when the shadow pre-pass runs this frame (soft shadows enabled and
    /// at least one atlas layer needed a depth pass); when the pre-pass is skipped
    /// they retain the previous frame's ticks and `read_gpu_timestamps` reports a
    /// begin==end span of `0` — see `shadow_writes`.
    const COUNT: u32 = 9;
    /// Bytes per timestamp slot (`u64`).
    const SLOT_BYTES: u64 = 8;

    /// Allocate the query set + resolve/readback buffers, or `None` when the
    /// device was created without `TIMESTAMP_QUERY`.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("Render Timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: Self::COUNT,
        });
        let size = u64::from(Self::COUNT) * Self::SLOT_BYTES;
        let resolve_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Timestamp Resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let inside_passes = device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);
        Some(Self {
            query_set,
            resolve_buf,
            period_ns: queue.get_timestamp_period(),
            inside_passes,
        })
    }

    /// `RenderPassTimestampWrites` for the PBR pass (slots 0 → 1).
    fn pbr_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        }
    }

    /// `RenderPassTimestampWrites` for the tonemap pass (slots 2 → 3).
    fn tonemap_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(2),
            end_of_pass_write_index: Some(3),
        }
    }

    /// `RenderPassTimestampWrites` for the shadow depth pre-pass (slots 7 → 8).
    ///
    /// Wraps the single depth-only render pass that renders every shadow caster
    /// into the atlas. Recorded before the PBR pass in the same encoder, so slots
    /// 7/8 bracket a GPU span that precedes slots 0/1. Wired to the first shadow
    /// depth pass by [`Renderer::render`]; `GpuTimestamps::COUNT` reserves 7/8
    /// so the resolve range is stable.
    fn shadow_writes(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(7),
            end_of_pass_write_index: Some(8),
        }
    }

    /// Record an interior PBR-pass timestamp at `slot` (4), but only when the
    /// device supports `TIMESTAMP_QUERY_INSIDE_PASSES`. No-op otherwise, so the
    /// whole-pass timing keeps working on adapters (e.g. Metal) that only allow
    /// stage-boundary sampling.
    fn write_interior(&self, pass: &mut wgpu::RenderPass<'_>, slot: u32) {
        if self.inside_passes {
            pass.write_timestamp(&self.query_set, slot);
        }
    }
}

/// GPU pipeline-statistics query for `FRAGMENT_SHADER_INVOCATIONS` (overdraw).
///
/// Present only when the `metrics` feature is on **and** the adapter reports
/// `Features::PIPELINE_STATISTICS_QUERY`. This is **unsupported on all Metal
/// backends** (mapped only on Vulkan/D3D12 in wgpu 29), so on Apple hardware
/// this is `None` and `fragment_shader_invocations` stays `0`.
#[cfg(feature = "metrics")]
struct GpuPipelineStats {
    query_set: wgpu::QuerySet,
    /// `QUERY_RESOLVE | COPY_SRC` — kept separate from the timestamp resolve
    /// buffer because `resolve_query_set` requires a 256-aligned destination
    /// offset, so the two query kinds cannot share one resolve buffer cleanly.
    resolve_buf: wgpu::Buffer,
}

#[cfg(feature = "metrics")]
impl GpuPipelineStats {
    /// One statistic (`FRAGMENT_SHADER_INVOCATIONS`) resolves to a single `u64`.
    const SIZE: u64 = 8;

    /// Allocate the query set + resolve/readback buffers, or `None` when the
    /// device was created without `PIPELINE_STATISTICS_QUERY` (all Metal).
    fn new(device: &wgpu::Device) -> Option<Self> {
        if !device
            .features()
            .contains(wgpu::Features::PIPELINE_STATISTICS_QUERY)
        {
            return None;
        }
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("PBR Pipeline Stats"),
            ty: wgpu::QueryType::PipelineStatistics(
                wgpu::PipelineStatisticsTypes::FRAGMENT_SHADER_INVOCATIONS,
            ),
            count: 1,
        });
        let resolve_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pipeline Stats Resolve"),
            size: Self::SIZE,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        Some(Self {
            query_set,
            resolve_buf,
        })
    }
}

/// Result of reading back the per-frame GPU query buffers. Every field is `0`
/// when the corresponding query is unsupported (see the field docs on
/// `RenderFrameTimings`).
#[cfg(feature = "metrics")]
#[derive(Default)]
struct GpuFrameReadback {
    sample_id: u64,
    pbr_ns: u64,
    tonemap_ns: u64,
    shadow_ns: u64,
    pbr_opaque_ns: u64,
    pbr_transparent_ns: u64,
    fragment_shader_invocations: u64,
}

// ── Renderer ──────────────────────────────────────────────────────────────────

/// Owns the wgpu device/queue, PBR pipeline, and GPU texture cache.
/// The lazily-allocated `Depth32Float` `D2Array` shadow atlas.
///
/// One layer per [`SHADOW_BUDGET`] slot. `layer_views` are the per-layer
/// depth render targets the shadow depth pass writes into; `array_view` is the
/// `D2Array` view the PBR fragment stage samples through the comparison sampler.
struct ShadowAtlas {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    /// One `D2` depth render-target view per atlas layer (write target).
    layer_views: Vec<wgpu::TextureView>,
    /// `D2Array` view over all layers, bound at group 1 binding 1 for sampling.
    array_view: wgpu::TextureView,
    /// Per-side texel resolution this atlas was allocated at.
    resolution: u32,
}

impl ShadowAtlas {
    fn new(device: &wgpu::Device, resolution: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shadow Atlas"),
            size: wgpu::Extent3d {
                width: resolution,
                height: resolution,
                depth_or_array_layers: u32_sat(SHADOW_BUDGET),
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let layer_views = (0..u32_sat(SHADOW_BUDGET))
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("Shadow Atlas Layer"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Shadow Atlas Array View"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        Self {
            texture,
            layer_views,
            array_view,
            resolution,
        }
    }
}

/// Renderer-owned GPU resources for the shadow atlas.
///
/// The comparison sampler, the 1×1 dummy `D2Array` depth (bound when the frame
/// is shadow-free) and the persistent `ShadowUniforms` buffer are always
/// present; the full `Depth32Float` `D2Array` atlas is allocated lazily on the
/// first shadowed frame so shadows-off products (idtx-inspector / idtx-poser)
/// never pay the ~`SHADOW_BUDGET * SHADOW_ATLAS_DIM²` depth allocation.
struct ShadowResources {
    /// Comparison sampler (`Less`) driving the PCF depth compares.
    comparison_sampler: wgpu::Sampler,
    /// 1×1 `D2Array` (`SHADOW_BUDGET` layers) depth bound at group 1 binding 1
    /// when the frame runs shadow-free — keeps the bind group valid without
    /// allocating the full atlas.
    dummy_view: wgpu::TextureView,
    #[allow(dead_code)]
    dummy_tex: wgpu::Texture,
    /// Persistent `ShadowUniforms` uniform buffer, re-uploaded per frame.
    uniforms_buf: wgpu::Buffer,
    /// The atlas, allocated on first shadowed frame (re-allocated on a resolution change).
    atlas: Option<ShadowAtlas>,
}

impl ShadowResources {
    fn new(device: &wgpu::Device) -> Self {
        let comparison_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Shadow Comparison Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // Linear + comparison gives the 2×2 bilinear PCF that softens the
            // 3×3 kernel's step edges; the depth compare runs per-tap.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            // Atlas stores nearest-occluder depth (near→0/far→1); a receiver is lit
            // when reference (its own depth) < stored occluder depth.
            compare: Some(wgpu::CompareFunction::Less),
            ..Default::default()
        });
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shadow Dummy Depth"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: u32_sat(SHADOW_BUDGET),
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let dummy_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("Shadow Dummy View"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let uniforms_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Shadow Uniforms"),
            size: std::mem::size_of::<ShadowUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            comparison_sampler,
            dummy_view,
            dummy_tex,
            uniforms_buf,
            atlas: None,
        }
    }

    /// Ensures the atlas is allocated at `resolution`, (re)allocating on first
    /// use or a resolution change. Returns `true` when a (re)allocation happened
    /// — the caller must then invalidate any `ShadowCache` entries, since the new
    /// atlas layers hold no valid depth yet.
    fn ensure_atlas(&mut self, device: &wgpu::Device, resolution: u32) -> bool {
        let needs = match &self.atlas {
            Some(a) => a.resolution != resolution,
            None => true,
        };
        if needs {
            self.atlas = Some(ShadowAtlas::new(device, resolution));
        }
        needs
    }
}

/// Packs the resolved shadow slots + tier into the GPU [`ShadowUniforms`].
///
/// `params[i]` = `(light_index, near, far, light_size)` for slot `i`;
/// `global` = `(tier, resolution, active_slot_count, receiver_bias)`. The tier
/// float is what the shader branches on per frame (0 = off → visibility 1.0),
/// so no pipeline recompile is needed to toggle shadows.
fn build_shadow_uniforms(
    slots: &[ShadowSlot],
    tier: ShadowTier,
    resolution: u32,
) -> ShadowUniforms {
    let mut u = ShadowUniforms::default();
    let n = slots.len().min(SHADOW_BUDGET);
    for s in slots.iter().take(SHADOW_BUDGET) {
        let i = s.slot as usize;
        if i >= SHADOW_BUDGET {
            continue;
        }
        u.light_view_proj[i] = s.view_proj.to_cols_array_2d();
        u.params[i] = [s.light_index.as_f32(), s.near, s.far, s.light_size];
    }
    let tier_val = match tier {
        ShadowTier::Off => 0.0,
        ShadowTier::Smooth => 1.0,
        ShadowTier::Soft => 2.0,
        // Sentinel need not be monotonic with cost — only the shader's range
        // tests matter. Kept above Soft so the `> 2.5` branch slots in ahead of
        // the existing `> 1.5` PCSS / `< 0.5` Off checks untouched.
        ShadowTier::Hard => 3.0,
    };
    // Constant receiver bias in shadow-NDC depth units. The pipeline's
    // slope-scaled offset carries the bulk of the acne fix, so this stays small;
    // tuned against `shadow_single_distant`.
    const RECEIVER_BIAS: f32 = 0.0015;
    u.global = [tier_val, resolution.as_f32(), n.as_f32(), RECEIVER_BIAS];
    u
}

/// FNV-1a hash of a caster light's light-space matrix — the `light_matrix_hash`
/// half of the `ShadowCache` key. Changes whenever the light moves or
/// the scene AABB (which drives the ortho fit in `math::build_view_proj`) grows
/// or shifts, so a cached atlas layer is reused only while its light frustum is
/// unchanged.
fn light_matrix_hash(view_proj: &glam::Mat4) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for f in view_proj.to_cols_array() {
        h ^= u64::from(f.to_bits());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// FNV-1a signature of every shadow caster's contribution to the atlas — the
/// `casters_epoch` half of the `ShadowCache` key. A matching value
/// means no caster moved, deformed, or changed topology since the last render,
/// so the atlas layers still hold valid depth and the depth pre-pass can be
/// skipped entirely (the single strongest perf lever: static inspector frames
/// add **zero** shadow draws).
///
/// Hashes, per static mesh: geometry `serial` + world matrix; **plus the
/// GPU-skinned joint palette** (`skin.joint_matrices`). The pose is the subtle
/// case: joint matrices change every animation tick *without* bumping the
/// geometry serial (they are the `ANIMATION` domain, not `GEOMETRY`), so a
/// signature that hashed only serials — like [`camera_fit::scene_aabb_signature`],
/// which deliberately omits pose because rest-pose bounds drive the AABB — would
/// wrongly skip re-rendering an animating skinned caster and freeze its shadow.
/// Per instanced batch: geometry serial + every instance transform. AABB growth
/// needs no separate term: it is a pure function of these same serials and
/// transforms, so any growth already perturbs the hash. Transparent surfaces are
/// hashed too (cheap, and conservative — they don't cast, but including them can
/// only over-invalidate, never wrongly skip).
fn casters_signature(snapshot: &RenderWorldSnapshot) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bits: u64| {
        h ^= bits;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(snapshot.meshes.len() as u64);
    for inst in &snapshot.meshes {
        mix(inst.mesh.serial);
        for f in inst.world_matrix.to_cols_array() {
            mix(u64::from(f.to_bits()));
        }
        if let Some(skin) = &inst.mesh.skin {
            for jm in &skin.joint_matrices {
                for f in jm {
                    mix(u64::from(f.to_bits()));
                }
            }
        }
    }
    mix(snapshot.instanced_batches.len() as u64);
    for batch in &snapshot.instanced_batches {
        mix(batch.mesh.serial);
        mix(batch.instance_transforms.len() as u64);
        for m in &batch.instance_transforms {
            for f in m.to_cols_array() {
                mix(u64::from(f.to_bits()));
            }
        }
    }
    h
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: PbrPipeline,
    tonemap_pipeline: ToneMapPipeline,
    /// `DomeLight` environment-background pipeline: draws the equirect env as the
    /// viewport backdrop after opaque / before transparent geometry when a textured dome is present.
    skybox_pipeline: crate::pipeline::SkyboxPipeline,
    /// Fullscreen texture blit used to mirror one XR eye into the desktop
    /// preview target without re-rendering the scene.
    blit_pipeline: BlitPipeline,
    /// MSAA→single-sample depth resolve, used by the XR path to produce the
    /// single-sample depth image copied into the `OpenXR` depth swapchain.
    depth_resolve_pipeline: DepthResolvePipeline,
    /// Reverse-Z MSAA→single-sample depth resolve for the visionOS present path:
    /// `CompositorServices` drawable depth must be reverse-Z (1=near, 0=far), the
    /// opposite of the renderer's forward-Z buffers. See [`Self::resolve_depth_reverse_z`].
    depth_resolve_reverse_pipeline: DepthResolvePipeline,
    overlay_pass: OverlayPass,
    /// Behavior-driven prim highlight renderer (surface tint + jump-flood
    /// silhouette outline). Consumes `snapshot.highlight_tints` /
    /// `highlight_outlines`; inert when both are empty.
    highlight: crate::highlight::HighlightRenderer,
    /// Screen-space HUD overlay renderer (camera-HUD overlay). Consumes
    /// `snapshot.hud_elements`; inert when empty. Composited after the highlight
    /// passes, over the LDR colour target.
    hud: crate::hud::HudRenderer,
    /// World-space widget-panel renderer (2D widgets in 3D). Consumes
    /// `snapshot.widget_panels`; inert when empty. Rasterizes each panel to a
    /// dirty-gated texture and draws it depth-tested inside the beauty pass.
    widget_renderer: crate::widget_pass::WidgetRenderer,
    #[cfg(feature = "editor-overlay")]
    axis_pass: AxisPass,
    sampler: wgpu::Sampler,
    /// Sampler for `UsdLuxDomeLight` equirect environments: repeat in U (azimuth
    /// wraps), clamp in V (poles), linear + mip.
    env_sampler: wgpu::Sampler,
    /// Neutral 1×1 `Rgba16Float` texture bound to group 1 binding 1 when no
    /// textured dome is present, so every PBR pipeline shares one bind-group layout.
    env_fallback_view: wgpu::TextureView,
    /// Loaded dome environments keyed by resolved texture path (float equirect +
    /// mip chain + projected SH). Populated lazily on first sighting of a dome.
    env_cache: std::collections::HashMap<String, crate::env_gpu::GpuEnv>,
    /// Resolved env texture paths whose decode failed. A negative cache that
    /// prevents re-decoding and log-spamming the same broken/unsupported dome
    /// texture every frame; the analytic fallback is used instead. A path is
    /// re-attempted only if the resolved dome texture path CHANGES (a new key),
    /// matching `env_cache`'s per-path semantics.
    env_failed: std::collections::HashSet<String>,
    texture_cache: TextureCache,
    /// Per-material pipelines compiled from `MaterialX` WGSL source.
    /// Keyed by FNV hash of the WGSL source string.
    /// `None` values record failed compilations so we don't retry each frame.
    #[cfg(feature = "materialx-wgsl")]
    wgsl_pipeline_cache: std::collections::HashMap<u64, Option<MaterialxPipelines>>,
    /// Persistent GPU buffers and bind groups keyed by `(prim_path, ordinal)`.
    /// Eliminates per-frame `create_buffer_init` / `create_bind_group` calls for
    /// static and slowly-changing scenes.
    mesh_gpu_cache: std::collections::HashMap<(String, usize), CachedMeshGpu>,
    /// Per-batch GPU resources for hardware-instanced draw calls.
    /// Keyed by `prototype_key` from `RenderInstancedBatch`.
    instance_batch_cache: std::collections::HashMap<String, CachedInstanceBatch>,
    /// Shared identity-matrix `MeshUniforms` bind group bound to group 2 for all
    /// instanced draws.  The instanced vertex shader reads model matrices from the
    /// instance buffer (slot 1) and ignores group 2 — this dummy binding keeps the
    /// pipeline layout consistent without allocating a per-batch mesh uniform.
    identity_mesh_bg: Option<wgpu::BindGroup>,
    /// Timing breakdown of the most recent `render()` call (populated under the
    /// `metrics` Cargo feature; zero-initialised stub otherwise).
    #[cfg(feature = "metrics")]
    last_frame_timings: crate::metrics::RenderFrameTimings,
    /// GPU timestamp-query resources; `None` when the adapter lacks
    /// `TIMESTAMP_QUERY`. Only allocated under the `metrics` feature.
    #[cfg(feature = "metrics")]
    gpu_timestamps: Option<GpuTimestamps>,
    #[cfg(feature = "metrics")]
    gpu_readbacks: crate::gpu_readback::Readbacks,
    #[cfg(feature = "metrics")]
    metrics_view: usize,
    /// GPU pipeline-statistics query (fragment-shader invocations / overdraw);
    /// `None` when the adapter lacks `PIPELINE_STATISTICS_QUERY` (all Metal).
    /// Only allocated under the `metrics` feature.
    #[cfg(feature = "metrics")]
    gpu_pipeline_stats: Option<GpuPipelineStats>,
    /// Serial of the last snapshot whose instance transforms were fully processed.
    /// Compared against [`RenderWorldSnapshot::instance_serial`] (not `serial`): on
    /// a match all instance buffer update work is skipped — transforms cannot have
    /// changed. Decoupled from `serial` so the per-frame XR hand-skeleton injection
    /// can re-upload its ~102 bone/joint transforms without forcing the whole-scene
    /// draw-merge rebuild gated by `last_merge_serial`.
    last_instance_serial: u64,
    /// Serial of the last snapshot whose static draw-merge batches were baked.
    /// Mirrors `last_instance_serial`: the CPU vertex bake + concatenate runs once
    /// per snapshot and is reused across warm frames. `u64::MAX` forces a rebuild.
    last_merge_serial: u64,
    /// Baked static draw-merge batches (F1/F7), rebuilt when `snapshot.serial`
    /// changes. Empty when merging is disabled or nothing is mergeable.
    merged_batch_cache: Vec<CachedMergedBatch>,
    /// Escape hatch: when `false` the merge planner is inert and every path behaves
    /// exactly as before. Defaults to `true` unless `IDTX_DISABLE_DRAW_MERGE` is
    /// set; overridable per-instance via [`Self::set_draw_merge_enabled`].
    draw_merge_enabled: bool,
    draw_batch_limits: DrawBatchLimits,
    last_geometry_error: Option<String>,
    /// Cached synthetic "frame-everything" camera AABB for snapshots that carry no
    /// authored camera. Holds `(signature, aabb_min, aabb_max)` where `signature` is
    /// [`crate::camera_fit::scene_aabb_signature`] of the snapshot the AABB was
    /// computed from. The expensive per-vertex `compute_scene_aabb` pass runs only
    /// when the incoming signature differs; a matching signature reuses the stored
    /// AABB (the ~48 ms → ~0 warm-frame win). `None` until the first camera-less
    /// frame. Never touched when the snapshot has an authored camera.
    cached_fit_aabb: Option<(u64, glam::Vec3, glam::Vec3)>,
    /// Shadow-atlas GPU resources (comparison sampler, 1×1 dummy, persistent
    /// uniforms, lazily-allocated `D2Array` atlas). See [`ShadowResources`].
    shadow: ShadowResources,
    /// `ShadowCache`: per caster light (keyed by `ShadowSlot::light_index`)
    /// the `(light_matrix_hash, casters_epoch)` last rendered into its atlas layer.
    /// A slot whose key is unchanged this frame skips its depth pass, so a fully
    /// static scene adds zero shadow draws after the first frame. Cleared when the
    /// atlas is (re)allocated or shadows turn off (both leave the layers invalid).
    shadow_cache: std::collections::HashMap<u32, (u64, u64)>,
    /// Whether the shadow depth pre-pass recorded at least one draw this frame.
    /// Drives `gpu_shadow_ns` to a deterministic `0` on a fully-cached frame rather
    /// than relying on stale timestamp-query slots. Written every frame; read only
    /// under the `metrics` feature.
    #[cfg_attr(not(feature = "metrics"), allow(dead_code))]
    shadow_pass_ran: bool,
    /// Runtime gate for editor overlays (world-axis lines). Defaults to `true`.
    /// The out-of-process `idtx-runtime` player sets this `false` so preview
    /// frames never draw editor chrome even when the `editor-overlay` feature is
    /// compiled into the shared `idtx-render` rlib via cargo feature unification.
    /// Read only under the `editor-overlay` feature.
    #[cfg_attr(not(feature = "editor-overlay"), allow(dead_code))]
    draw_editor_overlays: bool,
    /// Effective MSAA sample count ({1, 2, 4}) resolved once at construction
    /// (QI-2). Read both when the base pipelines are built and later in
    /// [`Self::create_frame_target_inner`] — the single source of truth so the
    /// pipelines and the frame-target textures always agree. See
    /// [`resolve_sample_count`].
    sample_count: u32,
    /// Whether the camera depth Z-prepass (QI-4) is active, resolved once at
    /// construction ([`resolve_z_prepass`]). When `true`, `render()` records a
    /// depth-only pre-pass over opaque geometry before the beauty PBR pass and the
    /// beauty pass loads (not clears) that depth. Must agree with whether the
    /// pipelines were built with the prepass depth policy, so both come from the
    /// same resolved value. Mirrors `self.pipeline.depth_prepass.is_some()`.
    z_prepass: bool,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer").finish_non_exhaustive()
    }
}

impl Renderer {
    /// Initialises wgpu and compiles the PBR shader.
    ///
    /// The MSAA sample count is resolved from the platform default + optional
    /// `IDTX_XR_MSAA` override (see [`resolve_sample_count`]). To force a specific
    /// count (tests, A/B), use [`Renderer::new_with_sample_count`].
    pub fn new() -> Result<Self, RenderError> {
        Self::new_with_sample_count(resolve_sample_count())
    }

    /// Like [`Renderer::new`] but with an explicit MSAA sample count ({1, 2, 4}).
    ///
    /// The value is sanitised to a supported count; the base pipelines and every
    /// frame target this renderer allocates use it. The Z-prepass (QI-4) is resolved
    /// from the platform default + `IDTX_XR_ZPREPASS` ([`resolve_z_prepass`]); use
    /// [`Renderer::new_with_options`] to force it (A/B, tests). Owns its own wgpu device.
    pub fn new_with_sample_count(sample_count: u32) -> Result<Self, RenderError> {
        Self::new_with_options(sample_count, resolve_z_prepass())
    }

    /// Like [`Renderer::new_with_sample_count`] but also forces the camera Z-prepass
    /// (QI-4) on or off, bypassing [`resolve_z_prepass`]. Lets A/B harnesses and tests
    /// compare prepass-on vs prepass-off deterministically without touching the
    /// `IDTX_XR_ZPREPASS` env var. Owns its own wgpu device.
    pub fn new_with_options(sample_count: u32, z_prepass: bool) -> Result<Self, RenderError> {
        let sample_count =
            crate::pipeline::sanitize_sample_count(Some(sample_count), crate::pipeline::MSAA_SAMPLES);
        let instance = wgpu::Instance::default();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                compatible_surface: None,
                force_fallback_adapter: true,
            }))
        })
        .map_err(|_| RenderError::NoAdapter)?;

        // Request optional features only when the adapter reports them, so a
        // missing feature never fails device creation on constrained hardware.
        let mut required_features = wgpu::Features::empty();
        if adapter
            .features()
            .contains(wgpu::Features::POLYGON_MODE_LINE)
        {
            required_features |= wgpu::Features::POLYGON_MODE_LINE;
        }
        // TIMESTAMP_QUERY drives per-pass GPU timing; only needed for metrics.
        #[cfg(feature = "metrics")]
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            required_features |= wgpu::Features::TIMESTAMP_QUERY;
            // Within-pass timestamps split opaque vs transparent GPU time. Only
            // some backends (not all Metal GPUs) allow draw-boundary sampling.
            if adapter
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES)
            {
                required_features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
            }
        }
        // PIPELINE_STATISTICS_QUERY drives the fragment-invocation (overdraw)
        // counter. Unsupported on Metal — this stays a no-op on Apple hardware.
        #[cfg(feature = "metrics")]
        if adapter
            .features()
            .contains(wgpu::Features::PIPELINE_STATISTICS_QUERY)
        {
            required_features |= wgpu::Features::PIPELINE_STATISTICS_QUERY;
        }
        // TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES unlocks 2× MSAA on the
        // `Rgba16Float` HDR target (QI-2). WebGPU core only guarantees 1× and 4×
        // for that format, so a `sample_count == 2` renderer would fail pipeline
        // creation without this. Requested only when advertised; the resolved
        // count is clamped below so a device lacking it never crashes.
        if adapter
            .features()
            .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
        {
            required_features |= wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        }

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("IDTX Renderer"),
            required_features,
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .map_err(|e| RenderError::Device(e.to_string()))?;

        // Clamp the resolved MSAA count to what this adapter actually supports for
        // the HDR + depth targets (2× needs the feature above; some devices still
        // lack it). An unsupported count steps up to 4× — always supported, no
        // visual regression — rather than crashing at pipeline creation.
        let sample_count = {
            let hdr = adapter.get_texture_format_features(HDR_FORMAT).flags;
            let depth = adapter.get_texture_format_features(DEPTH_FORMAT).flags;
            clamp_sample_count_to_support(sample_count, |n| {
                hdr.sample_count_supported(n) && depth.sample_count_supported(n)
            })
        };

        let polygon_mode_line = device
            .features()
            .contains(wgpu::Features::POLYGON_MODE_LINE);
        // `z_prepass` is the caller-forced value (param), not re-resolved from env.
        let pipeline =
            build_pbr_pipeline(&device, HDR_FORMAT, polygon_mode_line, sample_count, z_prepass);
        let tonemap_pipeline = build_tonemap_pipeline(&device, COLOR_FORMAT);
        let skybox_pipeline = build_skybox_pipeline(&device, HDR_FORMAT, DEPTH_FORMAT, sample_count);
        let blit_pipeline = build_blit_pipeline(&device, COLOR_FORMAT);
        let depth_resolve_pipeline = build_depth_resolve_pipeline(&device, DEPTH_FORMAT, sample_count);
        let depth_resolve_reverse_pipeline = build_depth_resolve_reverse_z_pipeline(&device, DEPTH_FORMAT, sample_count);
        let overlay_pass = OverlayPass::new(&device, COLOR_FORMAT);
        let highlight = crate::highlight::HighlightRenderer::new(&device);
        let hud = crate::hud::HudRenderer::new(&device);
        let widget_renderer = crate::widget_pass::WidgetRenderer::new(&device, sample_count);
        #[cfg(feature = "editor-overlay")]
        let axis_pass = AxisPass::new(&device, HDR_FORMAT, sample_count);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("PBR Sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let texture_cache = TextureCache::new(&device, &queue, TextureDecodeConfig::default());
        let shadow = ShadowResources::new(&device);
        let (env_sampler, env_fallback_view) = build_env_resources(&device, &queue);
        #[cfg(feature = "metrics")]
        let gpu_timestamps = GpuTimestamps::new(&device, &queue);
        #[cfg(feature = "metrics")]
        let gpu_readbacks = crate::gpu_readback::Readbacks::new(&device);
        #[cfg(feature = "metrics")]
        let gpu_pipeline_stats = GpuPipelineStats::new(&device);

        Ok(Self {
            draw_batch_limits: DrawBatchLimits::for_device(&device.limits()),
            last_geometry_error: None,
            device,
            queue,
            pipeline,
            tonemap_pipeline,
            skybox_pipeline,
            blit_pipeline,
            depth_resolve_pipeline,
            depth_resolve_reverse_pipeline,
            overlay_pass,
            highlight,
            hud,
            widget_renderer,
            #[cfg(feature = "editor-overlay")]
            axis_pass,
            sampler,
            env_sampler,
            env_fallback_view,
            env_cache: std::collections::HashMap::new(),
            env_failed: std::collections::HashSet::new(),
            texture_cache,
            #[cfg(feature = "materialx-wgsl")]
            wgsl_pipeline_cache: std::collections::HashMap::new(),
            mesh_gpu_cache: std::collections::HashMap::new(),
            instance_batch_cache: std::collections::HashMap::new(),
            identity_mesh_bg: None,
            #[cfg(feature = "metrics")]
            last_frame_timings: crate::metrics::RenderFrameTimings::default(),
            #[cfg(feature = "metrics")]
            gpu_timestamps,
            #[cfg(feature = "metrics")]
            gpu_readbacks,
            #[cfg(feature = "metrics")]
            metrics_view: 0,
            #[cfg(feature = "metrics")]
            gpu_pipeline_stats,
            last_instance_serial: 0,
            last_merge_serial: u64::MAX,
            merged_batch_cache: Vec::new(),
            draw_merge_enabled: std::env::var_os("IDTX_DISABLE_DRAW_MERGE").is_none(),
            cached_fit_aabb: None,
            shadow,
            shadow_cache: std::collections::HashMap::new(),
            shadow_pass_ran: false,
            draw_editor_overlays: true,
            sample_count,
            z_prepass,
        })
    }

    /// Same as [`Renderer::new`] but wires up a USD asset-reader for packed `.usdz` textures.
    pub fn with_asset_reader(reader: AssetReadFn) -> Result<Self, RenderError> {
        let mut r = Self::new()?;
        r.widget_renderer.set_reader(reader.clone());
        r.texture_cache = TextureCache::new_with_reader(
            &r.device,
            &r.queue,
            reader,
            TextureDecodeConfig::default(),
        );
        Ok(r)
    }

    /// Creates a [`Renderer`] that shares the caller's wgpu device and queue,
    /// with a USD asset-reader wired for packed `.usdz` textures.
    pub fn from_device_with_reader(
        device: wgpu::Device,
        queue: wgpu::Queue,
        reader: AssetReadFn,
        config: TextureDecodeConfig,
    ) -> Self {
        let mut r = Self::from_device(device, queue, config);
        r.widget_renderer.set_reader(reader.clone());
        r.texture_cache = TextureCache::new_with_reader(&r.device, &r.queue, reader, config);
        r
    }

    /// Creates a [`Renderer`] that shares the caller's wgpu device and queue.
    ///
    /// Use this when the caller already holds an active wgpu device (e.g. for
    /// egui surface rendering) so that the offscreen render target lives on the
    /// same device and can be registered as a native egui texture.
    #[must_use]
    pub fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: TextureDecodeConfig,
    ) -> Self {
        let sample_count = resolve_sample_count();
        // Shared device: no adapter to query format features precisely, so use the
        // adapter-feature flag as a proxy — 2× MSAA on `Rgba16Float` requires
        // TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES (QI-2). The device owner
        // (platform-desktop / xr_vulkan) requests it when available; if it is
        // absent, 2× is unsupported, so clamp up to the always-safe 4×.
        let sample_count = clamp_sample_count_to_support(sample_count, |n| {
            n != 2
                || device
                    .features()
                    .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
        });
        let polygon_mode_line = device
            .features()
            .contains(wgpu::Features::POLYGON_MODE_LINE);
        let z_prepass = resolve_z_prepass();
        let pipeline =
            build_pbr_pipeline(&device, HDR_FORMAT, polygon_mode_line, sample_count, z_prepass);
        let tonemap_pipeline = build_tonemap_pipeline(&device, COLOR_FORMAT);
        let skybox_pipeline = build_skybox_pipeline(&device, HDR_FORMAT, DEPTH_FORMAT, sample_count);
        let blit_pipeline = build_blit_pipeline(&device, COLOR_FORMAT);
        let depth_resolve_pipeline = build_depth_resolve_pipeline(&device, DEPTH_FORMAT, sample_count);
        let depth_resolve_reverse_pipeline = build_depth_resolve_reverse_z_pipeline(&device, DEPTH_FORMAT, sample_count);
        let overlay_pass = OverlayPass::new(&device, COLOR_FORMAT);
        let highlight = crate::highlight::HighlightRenderer::new(&device);
        let hud = crate::hud::HudRenderer::new(&device);
        let widget_renderer = crate::widget_pass::WidgetRenderer::new(&device, sample_count);
        #[cfg(feature = "editor-overlay")]
        let axis_pass = AxisPass::new(&device, HDR_FORMAT, sample_count);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("PBR Sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let texture_cache = TextureCache::new(&device, &queue, config);
        let shadow = ShadowResources::new(&device);
        let (env_sampler, env_fallback_view) = build_env_resources(&device, &queue);
        // Timestamps only when the caller's device was created with TIMESTAMP_QUERY.
        #[cfg(feature = "metrics")]
        let gpu_timestamps = GpuTimestamps::new(&device, &queue);
        #[cfg(feature = "metrics")]
        let gpu_readbacks = crate::gpu_readback::Readbacks::new(&device);
        #[cfg(feature = "metrics")]
        let gpu_pipeline_stats = GpuPipelineStats::new(&device);
        Self {
            draw_batch_limits: DrawBatchLimits::for_device(&device.limits()),
            last_geometry_error: None,
            device,
            queue,
            pipeline,
            tonemap_pipeline,
            skybox_pipeline,
            blit_pipeline,
            depth_resolve_pipeline,
            depth_resolve_reverse_pipeline,
            overlay_pass,
            highlight,
            hud,
            widget_renderer,
            #[cfg(feature = "editor-overlay")]
            axis_pass,
            sampler,
            env_sampler,
            env_fallback_view,
            env_cache: std::collections::HashMap::new(),
            env_failed: std::collections::HashSet::new(),
            texture_cache,
            #[cfg(feature = "materialx-wgsl")]
            wgsl_pipeline_cache: std::collections::HashMap::new(),
            mesh_gpu_cache: std::collections::HashMap::new(),
            instance_batch_cache: std::collections::HashMap::new(),
            identity_mesh_bg: None,
            #[cfg(feature = "metrics")]
            last_frame_timings: crate::metrics::RenderFrameTimings::default(),
            #[cfg(feature = "metrics")]
            gpu_timestamps,
            #[cfg(feature = "metrics")]
            gpu_readbacks,
            #[cfg(feature = "metrics")]
            metrics_view: 0,
            #[cfg(feature = "metrics")]
            gpu_pipeline_stats,
            last_instance_serial: 0,
            last_merge_serial: u64::MAX,
            merged_batch_cache: Vec::new(),
            draw_merge_enabled: std::env::var_os("IDTX_DISABLE_DRAW_MERGE").is_none(),
            cached_fit_aabb: None,
            shadow,
            shadow_cache: std::collections::HashMap::new(),
            shadow_pass_ran: false,
            draw_editor_overlays: true,
            sample_count,
            z_prepass,
        }
    }

    /// Returns a reference to the timing breakdown of the most recent [`Self::render`] call.
    ///
    /// Only meaningful when compiled with `--features metrics`; otherwise omitted from the API.
    #[cfg(feature = "metrics")]
    pub fn last_frame_timings(&self) -> &crate::metrics::RenderFrameTimings {
        &self.last_frame_timings
    }

    /// Resolves the synthetic "frame-everything" camera for a snapshot that carries
    /// no authored camera, caching the expensive per-vertex scene AABB.
    ///
    /// The full `compute_scene_aabb` pass (O(total vertices)) runs only when the
    /// snapshot's geometry signature — mesh count + per-mesh `serial` + world matrix,
    /// covering TOPOLOGY/GEOMETRY/TRANSFORM including in-place transform patches that
    /// never bump `snapshot.serial` — differs from the cached one. On a cache hit the
    /// stored AABB is reused and only the cheap camera projection is rebuilt, so a
    /// window-resize aspect change is still honoured for free. Returns the camera and
    /// whether the AABB was recomputed this call (for the `scene_aabb_recomputes`
    /// metric).
    fn resolve_synthetic_camera(
        &mut self,
        snapshot: &RenderWorldSnapshot,
        aspect: f32,
    ) -> (RenderCameraDesc, bool) {
        let sig = crate::camera_fit::scene_aabb_signature(snapshot);
        let (aabb_min, aabb_max, recomputed) = match self.cached_fit_aabb {
            Some((cached_sig, min, max)) if cached_sig == sig => (min, max, false),
            _ => {
                //let (min, max) = crate::camera_fit::compute_scene_aabb(snapshot, false)
                let (min, max) = crate::camera_fit::compute_camera_aabb(snapshot)
                    .unwrap_or((glam::Vec3::splat(-1.0), glam::Vec3::splat(1.0)));
                self.cached_fit_aabb = Some((sig, min, max));
                (min, max, true)
            }
        };
        (
            crate::camera_fit::fit_camera_to_aabb(aabb_min, aabb_max, aspect),
            recomputed,
        )
    }

    /// Enables or disables static draw-merge (F1/F7) at runtime.
    ///
    /// When toggled the merged-batch cache is dropped and its serial guard reset so
    /// the next `render()` rebuilds from scratch. Used by tests and perf-capture to
    /// A/B before/after in the same process without a rebuild; the initial value is
    /// `true` unless the `IDTX_DISABLE_DRAW_MERGE` env var is set.
    pub fn set_draw_merge_enabled(&mut self, enabled: bool) {
        if self.draw_merge_enabled != enabled {
            self.draw_merge_enabled = enabled;
            self.merged_batch_cache.clear();
            self.last_merge_serial = u64::MAX;
        }
    }

    /// Apply per-buffer geometry budgets, clamped to the requested device limits.
    /// A smaller budget also applies to individually uploaded meshes/instances.
    pub fn set_draw_batch_limits(&mut self, limits: DrawBatchLimits) {
        let limits = limits.clamped(&self.device.limits());
        if limits != self.draw_batch_limits {
            self.draw_batch_limits = limits;
            self.merged_batch_cache.clear();
            self.mesh_gpu_cache.clear();
            self.instance_batch_cache.clear();
            self.last_merge_serial = u64::MAX;
            self.last_instance_serial = u64::MAX;
        }
    }

    /// Validate allocation sizes before any GPU mutation. O(meshes + batches),
    /// including on warm frames where instance counts may change independently.
    pub fn validate_geometry(&self, snapshot: &RenderWorldSnapshot) -> Result<(), GeometryError> {
        let limits = self.draw_batch_limits;
        for mesh in snapshot.meshes.iter().map(|inst| &inst.mesh)
            .chain(snapshot.instanced_batches.iter().map(|batch| &batch.mesh)) {
            limits.check(GeometryCounts { vertices: mesh.positions.len(), indices: mesh.indices.len() })?;
            if mesh.positions.len() != mesh.normals.len() || mesh.positions.len() != mesh.texcoords.len() {
                return Err(GeometryError::InvalidGeometry);
            }
            if let Some(skin) = &mesh.skin {
                checked_bytes(skin.influences.len(), std::mem::size_of::<crate::vertex::SkinVertex>(), limits.max_vertex_bytes, "skin vertex")?;
                let device = self.device.limits();
                checked_bytes(256, 64, device.max_buffer_size.min(device.max_uniform_buffer_binding_size), "joint uniform")?;
            }
        }
        for batch in &snapshot.instanced_batches {
            checked_bytes(batch.instance_transforms.len(), std::mem::size_of::<InstanceRaw>(), limits.max_vertex_bytes, "instance")?;
        }
        Ok(())
    }

    fn report_render_error(&mut self, error: &RenderError) {
        let message = error.to_string();
        if self.last_geometry_error.as_ref() != Some(&message) {
            eprintln!("[render] {message}");
            self.last_geometry_error = Some(message);
        }
    }

    /// Allocates offscreen textures (MSAA HDR → resolve HDR → LDR) and CPU readback buffer.
    #[must_use]
    pub fn create_frame_target(&self, width: u32, height: u32) -> FrameTarget {
        self.create_frame_target_inner(width, height, false)
    }

    /// Like [`Self::create_frame_target`] but allocates the MSAA depth buffer so
    /// it can be **sampled** (`TEXTURE_BINDING`) and **stored** by the PBR pass,
    /// which the XR path needs to run [`Self::resolve_depth`] and feed the
    /// `OpenXR` depth composition layer. Desktop targets keep depth discard-only.
    #[must_use]
    pub fn create_xr_eye_target(&self, width: u32, height: u32) -> FrameTarget {
        self.create_frame_target_inner(width, height, true)
    }

    fn create_frame_target_inner(
        &self,
        width: u32,
        height: u32,
        resolve_depth: bool,
    ) -> FrameTarget {
        let sample_count = self.sample_count;

        // MSAA HDR color — PBR renders here at `sample_count` samples, then
        // resolves into `hdr_color_texture`. At 1× there is nothing to resolve, so
        // this target is not allocated: the PBR pass renders directly into the
        // single-sample `hdr_color_texture` (see the PBR pass's attachment branch).
        let (msaa_color_texture, msaa_color_view) = if sample_count > 1 {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("MSAA HDR Color"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            (Some(tex), Some(view))
        } else {
            (None, None)
        };

        // HDR resolve target — MSAA resolves here; input to the tonemapping pass.
        let hdr_color_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("HDR Resolve Color"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let hdr_color_view = hdr_color_texture.create_view(&Default::default());

        // LDR output — tonemapping writes here; read back to CPU.
        let color_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("LDR Color Output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&Default::default());

        // MSAA depth — same sample count as MSAA color. For XR eyes we add
        // TEXTURE_BINDING so `resolve_depth` can sample it; the PBR pass then
        // stores (rather than discards) depth for those targets.
        let depth_usage = if resolve_depth {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        };
        let depth_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("MSAA Depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: depth_usage,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&Default::default());

        // Pre-built tonemapping bind group references the resolved HDR texture.
        let tonemap_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Tonemap BG"),
            layout: &self.tonemap_pipeline.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.tonemap_pipeline.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&hdr_color_view),
                },
            ],
        });

        // Readback buffer — Rgba8UnormSrgb, 4 bytes per pixel (same as before).
        let raw_bpr = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bpr = (raw_bpr + align - 1) & !(align - 1);
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Readback Buffer"),
            size: u64::from(padded_bpr * height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        FrameTarget {
            color_texture,
            color_view,
            depth_texture,
            depth_view,
            msaa_color_texture,
            msaa_color_view,
            hdr_color_texture,
            hdr_color_view,
            tonemap_bind_group,
            readback_buffer,
            width,
            height,
            padded_bpr,
            resolve_depth,
            // XR eye targets clear to black (void at the far plane); the desktop
            // editor keeps its opaque grey backdrop.
            clear_color: if resolve_depth {
                wgpu::Color::BLACK
            } else {
                wgpu::Color {
                    r: 0.18,
                    g: 0.18,
                    b: 0.22,
                    a: 1.0,
                }
            },
            sample_count,
        }
    }

    pub fn texture_config(&self) -> TextureDecodeConfig { self.texture_cache.config() }

    /// Keep delayed stereo measurements associated with their original eye.
    #[cfg(feature = "metrics")]
    pub fn set_metrics_view(&mut self, view: usize) { self.metrics_view = view; }

    #[cfg(feature = "metrics")]
    pub fn texture_budget_stats(&self) -> crate::texture_cache::TextureBudgetStats { self.texture_cache.stats }

    /// Pre-decodes textures referenced by `snapshot` with bounded scratch and
    /// uploads them to the GPU cache. Call this before the first [`Self::render`]
    /// so texture loading does not block the render hot path.
    ///
    /// Returns `(loaded_count, decode_elapsed, upload_elapsed, sub_phase_timings)`.
    pub fn prefetch_textures(
        &mut self,
        snapshot: &RenderWorldSnapshot,
    ) -> (
        usize,
        std::time::Duration,
        std::time::Duration,
        crate::texture_cache::TextureDecodeTimings,
    ) {
        let mut paths: Vec<(String, TextureKind)> = Vec::new();
        // UDIM requests: (template, kind, tiles) built from the snapshot's
        // extract-time tile discovery. Only templates whose material touches at
        // least one tile are routed to the array path; plain (non-`<UDIM>`)
        // slots of a UDIM material still go through the single-tile cache below.
        let mut udim_reqs: Vec<(String, TextureKind, Vec<u32>)> = Vec::new();
        let collect_mat_paths =
            |m: &crate::material::RenderMaterialDesc,
             udim_tiles: &std::collections::HashMap<String, std::collections::BTreeSet<u32>>,
             out: &mut Vec<(String, TextureKind)>,
             udim_out: &mut Vec<(String, TextureKind, Vec<u32>)>| {
                let is_udim = m.is_udim();
                let tiles: Vec<u32> = if is_udim {
                    udim_tiles
                        .get(&m.material_path)
                        .map(|s| s.iter().copied().collect())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let mut push = |p: &Option<String>, kind: TextureKind| {
                    if let Some(p) = p {
                        if p.is_empty() {
                            return;
                        }
                        if is_udim && crate::udim::is_udim_template(p) {
                            udim_out.push((p.clone(), kind, tiles.clone()));
                        } else {
                            out.push((p.clone(), kind));
                        }
                    }
                };
                push(&m.diffuse_texture, TextureKind::DiffuseSrgb);
                push(&m.normal_texture, TextureKind::Normal);
                push(&m.metallic_texture, TextureKind::Data);
                push(&m.roughness_texture, TextureKind::Data);
                push(&m.occlusion_texture, TextureKind::Data);
                push(&m.emissive_texture, TextureKind::EmissiveSrgb);
            };
        for inst in &snapshot.meshes {
            collect_mat_paths(
                &inst.material,
                &snapshot.udim_tiles,
                &mut paths,
                &mut udim_reqs,
            );
        }
        for batch in &snapshot.instanced_batches {
            collect_mat_paths(
                &batch.material,
                &snapshot.udim_tiles,
                &mut paths,
                &mut udim_reqs,
            );
        }
        paths.sort_unstable();
        paths.dedup();
        if self.texture_cache.has_unused_paths(&paths, &udim_reqs) {
            // Bind groups retain wgpu views independently of the cache's Arcs.
            // Drop those owners before evicting texture handles.
            self.mesh_gpu_cache.clear();
            self.instance_batch_cache.clear();
            self.merged_batch_cache.clear();
        }
        self.texture_cache.retain_paths(&paths, &udim_reqs);
        if !udim_reqs.is_empty() {
            udim_reqs.sort_by(|a, b| (a.0.as_str(), a.1).cmp(&(b.0.as_str(), b.1)));
            udim_reqs.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
            let _udim =
                self.texture_cache
                    .prefetch_udim_parallel(&udim_reqs, &self.device, &self.queue);
        }
        self.texture_cache
            .prefetch_parallel(&paths, &self.device, &self.queue)
    }

    /// Resolve the dominant `UsdLuxDomeLight` environment for this frame.
    ///
    /// Picks the dominant dome (a textured dome is preferred; ties broken by
    /// ambient luminance), lazily loads and caches its float equirect + SH, and
    /// returns the `EnvUniforms` to bind plus the cache key of the textured
    /// environment to sample (`None` → bind the neutral fallback texture).
    ///
    /// The returned uniforms drive the shader's dome branch: `params.y` = `has_env`
    /// (any dome present), `params.z` = `has_texture` (a textured dome resolved).
    fn resolve_env(&mut self, lights: &[RenderLightDesc]) -> (EnvUniforms, Option<String>) {
        let lum = |a: [f32; 3]| a[0] * 0.2126 + a[1] * 0.7152 + a[2] * 0.0722;
        let is_dome = |l: &&RenderLightDesc| {
            matches!(
                l.kind,
                RenderLightKind::Dome | RenderLightKind::DomeProcedural
            )
        };
        let best_textured = lights
            .iter()
            .filter(is_dome)
            .filter(|l| l.dome_texture_path.is_some())
            .max_by(|a, b| lum(a.ambient).total_cmp(&lum(b.ambient)));
        let any_dome = lights.iter().any(|l| is_dome(&l));

        // Only the selected dome is referenced by a frame. Retaining every
        // previously selected HDR map bypassed material texture admission.
        let selected = best_textured.and_then(|d| d.dome_texture_path.as_deref());
        let mut released = 0;
        self.env_cache.retain(|path, env| {
            if Some(path.as_str()) == selected { true } else { released += env.bytes; false }
        });
        self.texture_cache.release_external(released);
        self.env_failed.retain(|path| Some(path.as_str()) == selected);

        let mut env = EnvUniforms::default();

        if let Some(dome) = best_textured {
            let path = dome.dome_texture_path.clone().unwrap_or_default();
            let rotation = dome.env_rotation;
            let ambient = dome.ambient;
            // Load + cache on first sighting of this environment path.
            if !self.env_cache.contains_key(&path) && !self.env_failed.contains(&path) {
                let reader = self.texture_cache.reader().cloned();
                match crate::env_gpu::decode_env_bounded(&path, reader.as_deref(), self.texture_cache.config()) {
                    Ok(image) => {
                        let levels = u32::BITS - image.width.max(image.height).leading_zeros();
                        let bytes = crate::texture_cache::rgba_bytes(image.width, image.height, levels, 1)
                            .and_then(|n| n.checked_mul(2));
                        if !bytes.is_some_and(|n| self.texture_cache.admit_external(n)) {
                            self.env_failed.insert(path);
                            return (env, None);
                        }
                        let sh = crate::env_gpu::project_sh(&image);
                        let avg = crate::env_ibl::average_radiance(&image);
                        let (view, mip_count) =
                            crate::env_gpu::upload_env(&self.device, &self.queue, &image);
                        // Logged unconditionally (not just under `metrics`) so a
                        // dome's load success is observable alongside the always-on
                        // failure log below. Fires once per env (first sight only).
                        eprintln!(
                            "[idtx-render] dome env loaded {}x{} mips={}: {path}",
                            image.width, image.height, mip_count,
                        );
                        self.env_cache.insert(
                            path.clone(),
                            crate::env_gpu::GpuEnv {
                                view,
                                mip_count,
                                sh,
                                avg,
                                bytes: bytes.unwrap_or(0),
                            },
                        );
                    }
                    Err(e) => {
                        eprintln!("[idtx-render] dome env load failed ({path}): {e}");
                        // Negative-cache the path so we neither re-decode nor
                        // re-log it every frame; fall back to the analytic dome.
                        self.env_failed.insert(path.clone());
                    }
                }
            }

            if let Some(gpu) = self.env_cache.get(&path) {
                for i in 0..9 {
                    env.sh[i] = [
                        gpu.sh.coeffs[i][0],
                        gpu.sh.coeffs[i][1],
                        gpu.sh.coeffs[i][2],
                        0.0,
                    ];
                }
                // Columns of the world→env rotation mat3.
                env.rot0 = [rotation.x_axis.x, rotation.x_axis.y, rotation.x_axis.z, 0.0];
                env.rot1 = [rotation.y_axis.x, rotation.y_axis.y, rotation.y_axis.z, 0.0];
                env.rot2 = [rotation.z_axis.x, rotation.z_axis.y, rotation.z_axis.z, 0.0];
                env.ambient = [ambient[0], ambient[1], ambient[2], 0.0];
                let max_mip = gpu.mip_count.saturating_sub(1).as_f32();
                env.params = [max_mip, 1.0, 1.0, 0.0];
                return (env, Some(path));
            }
        }

        if any_dome {
            // Untextured / procedural dome(s): flat analytic path driven per-light in
            // the shader; only `has_env` is set here.
            env.params = [0.0, 1.0, 0.0, 0.0];
        }
        (env, None)
    }

    /// Enables or disables editor overlays (world-axis lines) at runtime.
    ///
    /// Overlays default to enabled. The out-of-process `idtx-runtime` preview
    /// player calls this with `false` so preview frames never draw editor chrome,
    /// even when the `editor-overlay` feature ends up compiled into the shared
    /// `idtx-render` rlib via cargo feature unification. A no-op on the draw path
    /// when the feature is not compiled in.
    pub fn set_editor_overlays_enabled(&mut self, enabled: bool) {
        self.draw_editor_overlays = enabled;
    }

    /// Renders `snapshot` into `target`.
    ///
    /// When `wireframe` is `true` all geometry is drawn as unlit wireframe using
    /// `PolygonMode::Line` pipelines (requires that the GPU adapter supports
    /// `wgpu::Features::POLYGON_MODE_LINE`; silently falls back to shaded if not).
    ///
    /// Requires `&mut self` because texture loading mutates the internal cache.
    pub fn render(
        &mut self,
        target: &FrameTarget,
        snapshot: &RenderWorldSnapshot,
        wireframe: bool,
    ) {
        if let Err(error) = self.try_render(target, snapshot, wireframe) {
            self.report_render_error(&error);
        }
    }

    /// Fallible render entry point. Oversized geometry returns a typed error
    /// before GPU allocation/submission, preserving the previous target image.
    /// The compatibility entry points log a changed error once and skip the frame.
    ///
    /// # Errors
    /// Returns [`RenderError::Geometry`] for unrepresentable or over-budget geometry.
    ///
    /// # Panics
    /// GPU validation failures unrelated to geometry budgets retain wgpu's error
    /// handling policy; the target must belong to this renderer's device.
    pub fn try_render(
        &mut self,
        target: &FrameTarget,
        snapshot: &RenderWorldSnapshot,
        wireframe: bool,
    ) -> Result<(), RenderError> {
        #[cfg(feature = "metrics")]
        let t_render_total = std::time::Instant::now();

        self.validate_geometry(snapshot)?;

        let aspect = target.width.as_f32() / target.height.as_f32();
        #[cfg(feature = "metrics")]
        let mut scene_aabb_recomputes: u32 = 0;
        let cam = if let Some(c) = snapshot.camera.clone() { c } else {
            // No authored camera: frame the whole scene with the cached synthetic
            // orbit camera. The per-vertex AABB is recomputed only when the scene
            // signature changed (see `resolve_synthetic_camera`).
            let (c, _recomputed) = self.resolve_synthetic_camera(snapshot, aspect);
            #[cfg(feature = "metrics")]
            {
                scene_aabb_recomputes = u32::from(_recomputed);
            }
            c
        };

        let view_proj = cam.projection * cam.view;
        let eye_pos = cam.view.inverse().w_axis;

        // Prepare world-space widget panels OUTSIDE the beauty pass: rasterize +
        // upload any changed panels (dirty-gated) and build their bind groups,
        // so the actual draw inside the beauty pass only records set/draw calls.
        // Camera basis vectors drive billboard modes. Empty frame → zero cost.
        let widget_frame = {
            let inv_view = snapshot.widget_camera.unwrap_or_else(|| cam.view.inverse());
            self.widget_renderer.prepare(
                &self.device,
                &self.queue,
                &snapshot.widget_panels,
                view_proj,
                inv_view.x_axis.truncate(),
                inv_view.y_axis.truncate(),
            )
        };

        // Pre-build axis GPU data now so the buffers outlive the render pass below.
        // Skipped when overlays are disabled at runtime (e.g. the out-of-process
        // preview player) so no editor chrome is drawn; `axis_gpu == None` makes the
        // draw block below a no-op.
        #[cfg(feature = "editor-overlay")]
        let axis_gpu: Option<AxisGpuData> = if self.draw_editor_overlays {
            let mut lines: Vec<AxisLine> = Vec::with_capacity(6);
            push_world_axes(&mut lines);
            self.axis_pass.prepare(&self.device, view_proj, &lines)
        } else {
            None
        };

        // ── Camera bind group ──────────────────────────────────────────────
        // EV100 = 0 → identity exposure (no physical camera); otherwise 1/(1.2·2^EV100).
        // `camera_fit_ns` closes the previously-untimed span from `render()` entry
        // to here: camera resolve (incl. `resolve_synthetic_camera`'s per-vertex AABB
        // when the scene has no authored camera *and* the cache missed — see
        // `scene_aabb_recomputes`), view/proj, and axis-gizmo prep.
        #[cfg(feature = "metrics")]
        let camera_fit_ns = nanos_u64(t_render_total.elapsed().as_nanos());
        #[cfg(feature = "metrics")]
        let t_camera_lights = std::time::Instant::now();
        let linear_exposure = if cam.exposure_ev.abs() < 1e-6 {
            1.0_f32
        } else {
            1.0 / (1.2 * (2.0_f32).powf(cam.exposure_ev))
        };
        let cam_u = CameraUniforms {
            view_proj: view_proj.to_cols_array_2d(),
            eye_pos: eye_pos.into(),
            exposure: linear_exposure,
            _pad1: [0.0; 3],
        };
        let cam_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Camera Uniforms"),
                contents: bytemuck::bytes_of(&cam_u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let cam_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera BG"),
            layout: &self.pipeline.camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buf.as_entire_binding(),
            }],
        });

        // ── Lights bind group ──────────────────────────────────────────────
        // Synthesise a headlight when the scene has no authored lights so
        // objects are visible in unlit scenes (macbeth_chart, chess_set, etc.)
        let headlight_storage;
        let effective_lights: &[RenderLightDesc] = if snapshot.lights.is_empty() {
            let cam_world = cam.view.inverse();
            let cam_pos = cam_world.w_axis.truncate();
            let fwd = cam_world.z_axis.truncate().normalize_or_zero();
            let world_up = if fwd.y.abs() < 0.99 {
                glam::Vec3::Y
            } else {
                glam::Vec3::X
            };
            let right = world_up.cross(fwd).normalize_or_zero();
            let up = fwd.cross(right);
            let mat = glam::Mat4::from_cols(
                right.extend(0.0),
                up.extend(0.0),
                fwd.extend(0.0),
                cam_pos.extend(1.0),
            );
            headlight_storage = [RenderLightDesc {
                prim_path: String::new(),
                kind: RenderLightKind::Distant {
                    angle_radians: 0.05,
                },
                world_matrix: mat,
                color: [1.0, 1.0, 1.0],
                power: 3.5,
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
                dome_texture_format: idtx_usd::DomeTextureFormat::default(),
                env_rotation: glam::Mat3::IDENTITY,
                ambient: [0.0; 3],
            }];
            &headlight_storage[..]
        } else {
            &snapshot.lights
        };
        let lights_u = build_lights_uniforms(effective_lights);
        let lights_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Lights Uniforms"),
                contents: bytemuck::bytes_of(&lights_u),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // ── Shadow uniforms + atlas selection ─────────────────────────────
        // Shadows run only when the resolved tier is on AND the allocator handed
        // out at least one slot. Off frames upload a zeroed `ShadowUniforms`
        // (tier 0 → the shader short-circuits to visibility 1.0) and bind the 1×1
        // dummy depth so the group-1 layout stays satisfied without allocating
        // the atlas — the win for shadows-off inspector/poser.
        let shadows_on = snapshot.shadow.enabled() && !snapshot.shadow_slots.is_empty();
        if std::env::var_os("IDTX_SHADOW_DEBUG").is_some() {
            eprintln!(
                "[shadow] tier={:?} enabled={} slots={} meshes={}",
                snapshot.shadow.tier,
                snapshot.shadow.enabled(),
                snapshot.shadow_slots.len(),
                snapshot.meshes.len(),
            );
            for s in &snapshot.shadow_slots {
                eprintln!(
                    "[shadow]  slot={} light_idx={} near={} far={} size={}",
                    s.slot, s.light_index, s.near, s.far, s.light_size
                );
            }
        }
        let shadow_resolution = SHADOW_ATLAS_DIM;
        // Reset the per-frame shadow-pass flag; the depth pre-pass sets it true
        // only if at least one slot misses the ShadowCache and actually draws.
        self.shadow_pass_ran = false;
        let shadow_uniforms = if shadows_on {
            // A (re)allocated atlas holds no valid depth, so its layers can't be
            // reused — drop every cache entry to force a fresh depth pass.
            if self.shadow.ensure_atlas(&self.device, shadow_resolution) {
                self.shadow_cache.clear();
            }
            build_shadow_uniforms(
                &snapshot.shadow_slots,
                snapshot.shadow.tier,
                shadow_resolution,
            )
        } else {
            // Shadows off this frame: the atlas layers will go stale, so a later
            // re-enable must re-render. Clear the cache now.
            self.shadow_cache.clear();
            ShadowUniforms::default()
        };
        self.queue.write_buffer(
            &self.shadow.uniforms_buf,
            0,
            bytemuck::bytes_of(&shadow_uniforms),
        );
        // Resolve the dome environment (loads/caches the equirect + SH) and pack the
        // env uniforms. `env_key` selects the textured env view; `None` → fallback.
        // Done before the shadow-atlas borrow below so this `&mut self` env load does
        // not overlap the immutable `self.shadow` borrow held by `shadow_atlas_view`.
        let (env_u, env_key) = self.resolve_env(effective_lights);
        let shadow_atlas_view: &wgpu::TextureView = if shadows_on {
            &self.shadow.atlas.as_ref().unwrap().array_view
        } else {
            &self.shadow.dummy_view
        };
        let env_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Env Uniforms"),
                contents: bytemuck::bytes_of(&env_u),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let env_view = env_key
            .as_ref()
            .and_then(|k| self.env_cache.get(k))
            .map_or(&self.env_fallback_view, |g| &g.view);
        let lights_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Lights BG"),
            layout: &self.pipeline.lights_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: lights_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(shadow_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.shadow.comparison_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.shadow.uniforms_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(env_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.env_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: env_buf.as_entire_binding(),
                },
            ],
        });

        // Skybox background bind group — built only when a textured dome environment
        // is present (`env_key`). Reuses the resolved env view / sampler / uniforms;
        // adds an inverse-view-projection so the fragment stage can rebuild the world
        // view ray. `_sky_buf` is kept alive until submit (the bind group references it).
        let (_sky_buf, skybox_bg) = if env_key.is_some() {
            let sky_u = SkyboxUniforms {
                inv_view_proj: view_proj.inverse().to_cols_array_2d(),
                eye_pos: eye_pos.to_array(),
            };
            let sky_buf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Skybox Uniforms"),
                    contents: bytemuck::bytes_of(&sky_u),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Skybox BG"),
                layout: &self.skybox_pipeline.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: sky_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(env_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.env_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: env_buf.as_entire_binding(),
                    },
                ],
            });
            (Some(sky_buf), Some(bg))
        } else {
            (None, None)
        };

        // ── MaterialX pipeline cache pre-pass ─────────────────────────────
        // Compile per-material pipelines for any new WGSL sources in this
        // snapshot before entering the main mesh loop so borrow splitting works.
        #[cfg(feature = "metrics")]
        let camera_lights_ns = nanos_u64(t_camera_lights.elapsed().as_nanos());
        // MaterialX WGSL pipeline pre-pass timing / compile count. Declared under
        // `metrics` regardless of the `materialx-wgsl` feature so the frame-timing
        // struct always has a value; assigned exactly once from whichever cfg branch
        // compiles.
        #[cfg(feature = "metrics")]
        let (materialx_prep_ns, materialx_pipeline_compiles): (u64, u32);
        #[cfg(feature = "materialx-wgsl")]
        {
            #[cfg(feature = "metrics")]
            let t_mtlx = std::time::Instant::now();
            let mut new_entries: std::collections::HashMap<u64, String> =
                std::collections::HashMap::new();
            for inst in &snapshot.meshes {
                if let Some(ref src) = inst.material.wgsl_source {
                    let key = hash_wgsl(src);
                    if !self.wgsl_pipeline_cache.contains_key(&key) {
                        new_entries.entry(key).or_insert_with(|| src.clone());
                    }
                }
            }
            // Count genuine first-sight compiles this frame (0 on a warm frame; a
            // nonzero value every frame signals pipeline-cache thrash).
            #[cfg(feature = "metrics")]
            let compiles = u32_sat(new_entries.len());
            for (key, src) in new_entries {
                let result =
                    try_build_materialx_pipelines(
                        &self.device,
                        HDR_FORMAT,
                        &self.pipeline,
                        &src,
                        self.sample_count,
                    );
                if result.is_none() {
                    eprintln!(
                        "idtx-render: MaterialX WGSL pipeline compilation failed (key {key:#x}); \
                         falling back to PBR uber-shader"
                    );
                    crate::wgsl_patch::maybe_dump_wgsl(key, "raw", &src);
                }
                self.wgsl_pipeline_cache.insert(key, result);
            }
            #[cfg(feature = "metrics")]
            {
                materialx_prep_ns = nanos_u64(t_mtlx.elapsed().as_nanos());
                materialx_pipeline_compiles = compiles;
            }
        }
        #[cfg(all(feature = "metrics", not(feature = "materialx-wgsl")))]
        {
            materialx_prep_ns = 0;
            materialx_pipeline_compiles = 0;
        }

        let eye_world = eye_pos.truncate();

        // ── Per-mesh GPU resource cache ────────────────────────────────────
        //
        // The cache avoids re-allocating GPU buffers and bind groups on every
        // frame for static scenes.  A `CachedMeshGpu` entry persists across
        // frames; only the mesh-transform uniform is updated in-place via
        // `queue.write_buffer` when the world matrix changes.
        //
        // Cache key: (prim_path, ordinal).  The ordinal disambiguates multiple
        // instances sharing the same prototype path (e.g. PointInstancer).
        //
        // Borrow strategy: we split all mutating phases into separate loops so
        // &self.device/&self.queue remain as shared field borrows while
        // &mut self.mesh_gpu_cache and &mut self.texture_cache are exclusive.

        // Metrics scratch — hoisted outside the mesh-cache block so the final
        // `last_frame_timings` assignment can see them.
        #[cfg(feature = "metrics")]
        let (
            mut mesh_classify_ns,
            mut transform_update_ns,
            mut material_rebuild_ns,
            mut cache_miss_build_ns,
            mut evict_ns,
        ): (u64, u64, u64, u64, u64) = (0, 0, 0, 0, 0);
        #[cfg(feature = "metrics")]
        let (
            mut metrics_mesh_count,
            mut metrics_cache_hits,
            mut metrics_cache_misses,
            mut metrics_cache_xform,
            mut metrics_cache_mat,
            mut metrics_evictions,
        ): (u32, u32, u32, u32, u32, u32) = (0, 0, 0, 0, 0, 0);
        // Merged batches (re)baked this frame — full rebuild counts all, a warm
        // transform-only frame counts only the groups that actually moved.
        #[cfg(feature = "metrics")]
        let mut merged_batch_rebakes: u32 = 0;

        // ── Static draw-merge planner (F1/F7) ──────────────────────────────
        //
        // Runs every frame (cheap — reads the snapshot only). Groups opaque,
        // non-skinned meshes and single-instance batches that share a pipeline +
        // material by `MergeKey`; groups with ≥2 members collapse into one baked
        // `draw_indexed`. `merged_mesh` / `merged_batch_flags` flag which source
        // items are folded so the per-mesh cache, the draw-list build, and the
        // instanced passes can skip them. The actual bake is serial-guarded below.
        #[cfg(feature = "metrics")]
        let t_merge_plan = std::time::Instant::now();
        let mut merged_mesh = vec![false; snapshot.meshes.len()];
        let mut merged_batch_flags = vec![false; snapshot.instanced_batches.len()];
        let merge_groups: Vec<(MergeKey, Vec<MergeSource>)> = if self.draw_merge_enabled {
            let mut groups: std::collections::HashMap<MergeKey, Vec<MergeSource>> =
                std::collections::HashMap::new();
            for (i, inst) in snapshot.meshes.iter().enumerate() {
                if inst.material.is_transparent()
                    || inst.mesh.skin.is_some()
                    || inst.material.is_udim()
                    || !inst.casts_shadow
                    // Spliced spawn meshes stay standalone so they never alter an
                    // existing merge group's membership (which would force a
                    // whole-stage re-bake). See `RenderWorldSnapshot::standalone_paths`.
                    || snapshot.standalone_paths.contains(&inst.prim_path)
                {
                    continue;
                }
                let key = MergeKey {
                    double_sided: inst.material.double_sided || inst.mesh.double_sided,
                    mirrored: inst.world_matrix.determinant() < 0.0,
                    fingerprint: inst.material.fingerprint,
                    #[cfg(feature = "materialx-wgsl")]
                    wgsl_key: inst.material.wgsl_source.as_deref().map(hash_wgsl),
                };
                groups.entry(key).or_default().push(MergeSource::Mesh(i));
            }
            for (b, batch) in snapshot.instanced_batches.iter().enumerate() {
                // Fold only single-instance batches (F7); multi-instance batches
                // keep the hardware-instanced path.
                if batch.instance_transforms.len() != 1 {
                    continue;
                }
                if batch.material.is_transparent()
                    || batch.mesh.skin.is_some()
                    || batch.material.is_udim()
                    || !batch.casts_shadow
                {
                    continue;
                }
                let m = batch.instance_transforms[0];
                let key = MergeKey {
                    double_sided: batch.material.double_sided || batch.mesh.double_sided,
                    mirrored: m.determinant() < 0.0,
                    fingerprint: batch.material.fingerprint,
                    #[cfg(feature = "materialx-wgsl")]
                    wgsl_key: batch.material.wgsl_source.as_deref().map(hash_wgsl),
                };
                groups.entry(key).or_default().push(MergeSource::Batch(b));
            }
            // Keep only groups worth collapsing (≥2 members); flag their sources.
            let mut kept: Vec<(MergeKey, Vec<MergeSource>)> = Vec::new();
            for (k, members) in groups {
                for members in split_merge_group(members, snapshot, self.draw_batch_limits)? {
                    if members.len() < 2 {
                        continue;
                    }
                    for m in &members {
                        match *m {
                            MergeSource::Mesh(i) => merged_mesh[i] = true,
                            MergeSource::Batch(b) => merged_batch_flags[b] = true,
                        }
                    }
                    kept.push((k.clone(), members));
                }
            }
            // Deterministic order: the merge planner runs every frame off a
            // HashMap (nondeterministic iteration), but the warm-frame selective
            // re-bake maps merge_groups[i] ↔ merged_batch_cache[i] by index, so
            // the group order must be stable frame-to-frame. Sort by the full
            // MergeKey (fingerprint + pipeline flags [+ wgsl]); stable sorting
            // preserves source/chunk order within each size-split material group.
            kept.sort_by(|a, b| {
                (a.0.fingerprint, a.0.double_sided, a.0.mirrored)
                    .cmp(&(b.0.fingerprint, b.0.double_sided, b.0.mirrored))
                    .then_with(|| {
                        #[cfg(feature = "materialx-wgsl")]
                        {
                            a.0.wgsl_key.cmp(&b.0.wgsl_key)
                        }
                        #[cfg(not(feature = "materialx-wgsl"))]
                        {
                            std::cmp::Ordering::Equal
                        }
                    })
            });
            kept
        } else {
            Vec::new()
        };
        #[cfg(feature = "metrics")]
        let merge_plan_ns = nanos_u64(t_merge_plan.elapsed().as_nanos());

        // Phase 1 ─ classify each mesh instance (cache hit / miss / dirty).
        {
            #[cfg(feature = "metrics")]
            let t_phase1 = std::time::Instant::now();
            struct MeshWork {
                idx: usize,
                key: (String, usize),
                dirty_transform: bool,
                dirty_material: bool,
                /// Geometry changed (different `GpuMeshDesc::serial`) for the same prim path.
                /// Forces a full cache-entry eviction and rebuild so GPU vertex/index buffers
                /// are refreshed (structural variant switches, future geometry authoring).
                dirty_geometry: bool,
                is_miss: bool,
            }
            let mut work: Vec<MeshWork> = Vec::with_capacity(snapshot.meshes.len());
            #[cfg(feature = "metrics")]
            let mut cache_stats = crate::metrics::CacheFrameStats::default();
            {
                let mut ordinals: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for (idx, inst) in snapshot.meshes.iter().enumerate() {
                    let ord = ordinals.entry(inst.prim_path.as_str()).or_insert(0);
                    let key = (inst.prim_path.clone(), *ord);
                    *ord += 1;
                    // Merged meshes get no per-mesh GPU resources — their vertices
                    // are baked into a merged batch. Ordinal still advances above so
                    // the (prim_path, ordinal) keys of the remaining meshes match
                    // the identical computation in the `mesh_gpus` build below.
                    if merged_mesh[idx] {
                        continue;
                    }
                    let (dirty_transform, dirty_material, dirty_geometry, is_miss) =
                        if let Some(e) = self.mesh_gpu_cache.get(&key) {
                            let geom = e.last_mesh_serial != inst.mesh.serial;
                            (
                                !geom && e.last_world_matrix != inst.world_matrix,
                                !geom && e.mat_fingerprint != inst.material.fingerprint,
                                geom,
                                false,
                            )
                        } else {
                            (false, false, false, true)
                        };
                    #[cfg(feature = "metrics")]
                    {
                        if is_miss || dirty_geometry {
                            cache_stats.misses += 1;
                        } else if dirty_transform {
                            cache_stats.transform_updates += 1;
                        } else if dirty_material {
                            cache_stats.material_rebuilds += 1;
                        } else {
                            cache_stats.hits += 1;
                        }
                    }
                    work.push(MeshWork {
                        idx,
                        key,
                        dirty_transform,
                        dirty_material,
                        dirty_geometry,
                        is_miss,
                    });
                }
            }

            // Phase 2 ─ update world transforms and skin joint matrices in-place.
            #[cfg(feature = "metrics")]
            {
                mesh_classify_ns = nanos_u64(t_phase1.elapsed().as_nanos());
            }
            #[cfg(feature = "metrics")]
            let t_phase2 = std::time::Instant::now();
            for w in work.iter().filter(|w| w.dirty_transform) {
                let inst = &snapshot.meshes[w.idx];
                let entry = self.mesh_gpu_cache.get_mut(&w.key).unwrap();
                let mesh_u = MeshUniforms::from_matrix(inst.world_matrix);
                self.queue
                    .write_buffer(&entry.mesh_ubuf, 0, bytemuck::bytes_of(&mesh_u));
                entry.last_world_matrix = inst.world_matrix;
            }
            // Always refresh skin joint matrices for skinned meshes (animation).
            for w in work.iter().filter(|w| !w.is_miss) {
                let inst = &snapshot.meshes[w.idx];
                if let (Some(skin), Some(entry)) =
                    (inst.mesh.skin.as_ref(), self.mesh_gpu_cache.get(&w.key))
                    && let Some(skin_ubuf) = &entry.skin_ubuf {
                        let matrix_bytes: Vec<u8> = skin
                            .joint_matrices
                            .iter()
                            .flat_map(|m| bytemuck::bytes_of(m).iter().copied())
                            .collect();
                        const JOINT_PALETTE_BYTES: usize = 256 * 16 * 4;
                        let mut padded = matrix_bytes;
                        padded.resize(JOINT_PALETTE_BYTES, 0);
                        self.queue.write_buffer(skin_ubuf, 0, &padded);
                    }
            }

            // Phase 3 ─ rebuild material bind groups for changed materials.
            #[cfg(feature = "metrics")]
            {
                transform_update_ns = nanos_u64(t_phase2.elapsed().as_nanos());
            }
            #[cfg(feature = "metrics")]
            let t_phase3 = std::time::Instant::now();
            for w in work.iter().filter(|w| w.dirty_material) {
                let inst = &snapshot.meshes[w.idx];
                let udim = inst.material.is_udim() && inst.mesh.skin.is_none();
                let new_mat_bg = if udim {
                    build_material_udim_bg(
                        &self.device,
                        &self.pipeline,
                        &self.texture_cache,
                        &inst.material,
                    )
                } else {
                    build_material_bg(
                        &self.device,
                        &self.queue,
                        &self.pipeline.material_bgl,
                        &self.sampler,
                        &mut self.texture_cache,
                        &inst.material,
                    )
                };
                let entry = self.mesh_gpu_cache.get_mut(&w.key).unwrap();
                entry.mat_bg = new_mat_bg;
                entry.mat_fingerprint = inst.material.fingerprint;
                entry.udim = udim;
            }

            // Phase 4 ─ build full cache entries for cache misses and geometry changes.
            #[cfg(feature = "metrics")]
            {
                material_rebuild_ns = nanos_u64(t_phase3.elapsed().as_nanos());
            }
            #[cfg(feature = "metrics")]
            let t_phase4 = std::time::Instant::now();
            for w in work.iter().filter(|w| w.is_miss || w.dirty_geometry) {
                let inst = &snapshot.meshes[w.idx];
                if w.dirty_geometry {
                    self.mesh_gpu_cache.remove(&w.key);
                }
                if inst.mesh.positions.is_empty() || inst.mesh.indices.is_empty() {
                    continue;
                }
                let entry = build_mesh_gpu_cache_entry(
                    &self.device,
                    &self.queue,
                    &self.pipeline,
                    &self.sampler,
                    &mut self.texture_cache,
                    inst,
                );
                self.mesh_gpu_cache.insert(w.key.clone(), entry);
            }

            // Phase 5 ─ evict entries for prims no longer in the snapshot.
            #[cfg(feature = "metrics")]
            {
                cache_miss_build_ns = nanos_u64(t_phase4.elapsed().as_nanos());
            }
            #[cfg(feature = "metrics")]
            let t_phase5 = std::time::Instant::now();
            let live_keys: std::collections::HashSet<&(String, usize)> =
                work.iter().map(|w| &w.key).collect();
            #[cfg(feature = "metrics")]
            {
                let before = self.mesh_gpu_cache.len();
                self.mesh_gpu_cache.retain(|k, _| live_keys.contains(k));
                cache_stats.evictions = before.saturating_sub(self.mesh_gpu_cache.len());
                // Surface stats via eprintln only when IDTX_CACHE_METRICS=1 is set.
                if std::env::var_os("IDTX_CACHE_METRICS").is_some() {
                    eprintln!(
                        "idtx-render cache: hits={} xform_upd={} mat_rebuild={} misses={} evictions={} hit_rate={:.1}%",
                        cache_stats.hits,
                        cache_stats.transform_updates,
                        cache_stats.material_rebuilds,
                        cache_stats.misses,
                        cache_stats.evictions,
                        cache_stats.hit_rate() * 100.0,
                    );
                }
            }
            #[cfg(not(feature = "metrics"))]
            self.mesh_gpu_cache.retain(|k, _| live_keys.contains(k));
            #[cfg(feature = "metrics")]
            {
                evict_ns = nanos_u64(t_phase5.elapsed().as_nanos());
            }
            // Capture mesh-count and cache stats for the timings record.
            #[cfg(feature = "metrics")]
            {
                metrics_mesh_count = u32_sat(snapshot.meshes.len());
                metrics_cache_hits = u32_sat(cache_stats.hits);
                metrics_cache_misses = u32_sat(cache_stats.misses);
                metrics_cache_xform = u32_sat(cache_stats.transform_updates);
                metrics_cache_mat = u32_sat(cache_stats.material_rebuilds);
                metrics_evictions = u32_sat(cache_stats.evictions);
            }
        }

        // ── Lazy-init identity MeshUniforms bind group for instanced draws ─────
        if self.identity_mesh_bg.is_none() {
            let identity_u = MeshUniforms::from_matrix(glam::Mat4::IDENTITY);
            let buf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Identity Mesh Uniforms"),
                    contents: bytemuck::bytes_of(&identity_u),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Identity Mesh BG"),
                layout: &self.pipeline.mesh_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            self.identity_mesh_bg = Some(bg);
        }

        // ── Instance batch cache ───────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        let t_instance_cache = std::time::Instant::now();
        if snapshot.instance_serial != self.last_instance_serial {
            {
                // Classify batches (cache hit / miss / dirty material).
                struct BatchWork {
                    key: String,
                    is_miss: bool,
                    dirty_material: bool,
                }
                let mut batch_work: Vec<BatchWork> =
                    Vec::with_capacity(snapshot.instanced_batches.len());
                for batch in &snapshot.instanced_batches {
                    let (is_miss, dirty_material) =
                        if let Some(e) = self.instance_batch_cache.get(&batch.prototype_key) {
                            (false, e.mat_fingerprint != batch.material.fingerprint)
                        } else {
                            (true, false)
                        };
                    batch_work.push(BatchWork {
                        key: batch.prototype_key.clone(),
                        is_miss,
                        dirty_material,
                    });
                }

                // Pre-build raw instance bytes; owned, no live self borrow after this.
                struct BatchRaw {
                    key: String,
                    raw_bytes: Vec<u8>,
                    new_count: u32,
                }
                let all_raw: Vec<BatchRaw> = snapshot
                    .instanced_batches
                    .iter()
                    .map(|b| {
                        let raw: Vec<InstanceRaw> = b
                            .instance_transforms
                            .iter()
                            .map(|m| InstanceRaw {
                                model: m.to_cols_array_2d(),
                            })
                            .collect();
                        let bytes = bytemuck::cast_slice::<InstanceRaw, u8>(&raw).to_vec();
                        BatchRaw {
                            key: b.prototype_key.clone(),
                            raw_bytes: bytes,
                            new_count: u32_sat(raw.len()),
                        }
                    })
                    .collect();

                // Phase 2 – update or reallocate instance transform buffers.
                for br in &all_raw {
                    let is_miss = batch_work
                        .iter()
                        .find(|w| w.key == br.key)
                        .is_none_or(|w| w.is_miss);
                    if is_miss {
                        continue;
                    }
                    let needs_realloc = {
                        let e = self.instance_batch_cache.get(&br.key).unwrap();
                        e.instance_capacity < br.new_count
                    };
                    if needs_realloc {
                        let new_buf =
                            self.device
                                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                    label: Some("Instance Buffer"),
                                    contents: &br.raw_bytes,
                                    usage: wgpu::BufferUsages::VERTEX
                                        | wgpu::BufferUsages::COPY_DST,
                                });
                        let entry = self.instance_batch_cache.get_mut(&br.key).unwrap();
                        entry.instance_buf = new_buf;
                        entry.instance_capacity = br.new_count;
                        entry.instance_count = br.new_count;
                    } else {
                        // Clone the Arc-backed buffer handle to release the immutable borrow.
                        let buf = self
                            .instance_batch_cache
                            .get(&br.key)
                            .unwrap()
                            .instance_buf
                            .clone();
                        self.queue.write_buffer(&buf, 0, &br.raw_bytes);
                        self.instance_batch_cache
                            .get_mut(&br.key)
                            .unwrap()
                            .instance_count = br.new_count;
                    }
                }

                // Phase 3 – rebuild material bind groups for changed materials.
                for (bw, batch) in batch_work.iter().zip(snapshot.instanced_batches.iter()) {
                    if bw.is_miss || !bw.dirty_material {
                        continue;
                    }
                    let udim = batch.material.is_udim() && batch.mesh.skin.is_none();
                    let new_mat_bg = if udim {
                        build_material_udim_bg(
                            &self.device,
                            &self.pipeline,
                            &self.texture_cache,
                            &batch.material,
                        )
                    } else {
                        build_material_bg(
                            &self.device,
                            &self.queue,
                            &self.pipeline.material_bgl,
                            &self.sampler,
                            &mut self.texture_cache,
                            &batch.material,
                        )
                    };
                    let entry = self.instance_batch_cache.get_mut(&bw.key).unwrap();
                    entry.mat_bg = new_mat_bg;
                    entry.mat_fingerprint = batch.material.fingerprint;
                    entry.udim = udim;
                }

                // Phase 4 – build full entries for new batches.
                for (bw, batch) in batch_work.iter().zip(snapshot.instanced_batches.iter()) {
                    if !bw.is_miss {
                        continue;
                    }
                    use crate::vertex::interleave;
                    use bytemuck::cast_slice;
                    let m = &batch.mesh;
                    if m.positions.is_empty() || m.indices.is_empty() {
                        continue;
                    }
                    let verts = interleave(
                        &m.positions,
                        &m.normals,
                        &m.texcoords,
                        &m.vertex_colors,
                        &m.tangents,
                    );
                    let vbuf = self
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Instance Proto Vbuf"),
                            contents: cast_slice(&verts),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                    let ibuf = self
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("Instance Proto Ibuf"),
                            contents: cast_slice(&m.indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                    let br = all_raw.iter().find(|r| r.key == bw.key).unwrap();
                    let instance_buf =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Instance Buffer"),
                                contents: &br.raw_bytes,
                                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                            });
                    let mat_bg = if batch.material.is_udim() && m.skin.is_none() {
                        build_material_udim_bg(
                            &self.device,
                            &self.pipeline,
                            &self.texture_cache,
                            &batch.material,
                        )
                    } else {
                        build_material_bg(
                            &self.device,
                            &self.queue,
                            &self.pipeline.material_bgl,
                            &self.sampler,
                            &mut self.texture_cache,
                            &batch.material,
                        )
                    };
                    let double_sided = batch.material.double_sided || m.double_sided;
                    let udim = batch.material.is_udim() && m.skin.is_none();
                    self.instance_batch_cache.insert(
                        bw.key.clone(),
                        CachedInstanceBatch {
                            vbuf,
                            ibuf,
                            index_count: u32_sat(m.indices.len()),
                            instance_buf,
                            instance_capacity: br.new_count,
                            instance_count: br.new_count,
                            mat_bg,
                            mat_fingerprint: batch.material.fingerprint,
                            double_sided,
                            udim,
                        },
                    );
                }

                // Phase 5 – evict stale entries.
                let live_keys: std::collections::HashSet<&str> = snapshot
                    .instanced_batches
                    .iter()
                    .map(|b| b.prototype_key.as_str())
                    .collect();
                self.instance_batch_cache
                    .retain(|k, _| live_keys.contains(k.as_str()));
            }
            self.last_instance_serial = snapshot.instance_serial;
        } // end serial guard

        // ── Static draw-merge batch build (F1/F7) ──────────────────────────
        // Bake each source item's vertices into world space and concatenate the
        // members of every merge group into one vbuf/ibuf drawn with the identity
        // group-2 uniform. Reused across warm frames; `set_draw_merge_enabled`
        // resets the guard.
        //
        // Two re-bake modes:
        //  • Full structural rebuild — on a fresh snapshot serial or when the
        //    planned merge-group set no longer lines up with the cache (a group
        //    appeared/vanished, or its pipeline/material key changed). Rebuilds
        //    every batch's vbuf/ibuf/mat_bg from scratch.
        //  • Warm-frame selective re-bake — serial unchanged and the group set
        //    still lines up, so re-bake ONLY the batches whose folded members
        //    moved (detected via `xform_sig`). The transform-only fast path
        //    patches `world_matrix` WITHOUT bumping serial, so without this the
        //    merged world-space vertices would stay frozen at their first-frame
        //    pose — which is exactly why the axis-gizmo cubes and aim ray (folded
        //    by shared material) failed to follow the XR controller while the
        //    skinned mesh, excluded from merging, did. Static groups keep their
        //    cached buffers, so this stays cheap on large scenes.
        {
            use bytemuck::cast_slice;
            let structure_matches = self.merged_batch_cache.len() == merge_groups.len()
                && merge_groups
                    .iter()
                    .zip(self.merged_batch_cache.iter())
                    .all(|((k, members), cb)| {
                        let base = k.fingerprint == cb.fingerprint
                            && k.double_sided == cb.double_sided
                            && k.mirrored == cb.mirrored
                            && *members == cb.members;
                        #[cfg(feature = "materialx-wgsl")]
                        {
                            base && k.wgsl_key == cb.wgsl_key
                        }
                        #[cfg(not(feature = "materialx-wgsl"))]
                        {
                            base
                        }
                    });
            let was_full_rebuild =
                snapshot.serial != self.last_merge_serial || !structure_matches;
            // Diagnostic (always-on): how many merge groups re-baked this frame.
            // On a warm frame, `rebaked == 0` while a folded member (e.g. the XR
            // controller's RGB axis-gizmo cubes) is actually moving means the
            // member's patched world matrix did not reach the snapshot, so the
            // merged batch stays frozen until the next full extract snaps it.
            let mut rebaked_now: u32 = 0;
            if was_full_rebuild {
                self.last_merge_serial = u64::MAX;
                self.merged_batch_cache.clear();
                self.merged_batch_cache.reserve(merge_groups.len());
                for (key, members) in &merge_groups {
                    let (verts, indices) = bake_merge_group_geometry(members, snapshot)?;
                    // All members share identical material content (fingerprint is
                    // part of the key), so the first member builds the shared bg.
                    let rep_material = match members[0] {
                        MergeSource::Mesh(i) => &snapshot.meshes[i].material,
                        MergeSource::Batch(b) => &snapshot.instanced_batches[b].material,
                    };
                    // Empty geometry (placeholder/stub prims) — store a sentinel entry
                    // with index_count=0 to preserve Vec index correspondence with
                    // merge_groups (required by the warm-frame path).
                    let (vbuf, ibuf, index_count) = if verts.is_empty() || indices.is_empty() {
                        let mk_buf = |usage| {
                            self.device.create_buffer(&wgpu::BufferDescriptor {
                                label: None,
                                size: wgpu::COPY_BUFFER_ALIGNMENT,
                                usage,
                                mapped_at_creation: false,
                            })
                        };
                        (
                            mk_buf(wgpu::BufferUsages::VERTEX),
                            mk_buf(wgpu::BufferUsages::INDEX),
                            0u32,
                        )
                    } else {
                        let vbuf = self
                            .device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Merged Batch Vbuf"),
                                contents: cast_slice(&verts),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                        let ibuf = self
                            .device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Merged Batch Ibuf"),
                                contents: cast_slice(&indices),
                                usage: wgpu::BufferUsages::INDEX,
                            });
                        (vbuf, ibuf, u32_sat(indices.len()))
                    };
                    let mat_bg = build_material_bg(
                        &self.device,
                        &self.queue,
                        &self.pipeline.material_bgl,
                        &self.sampler,
                        &mut self.texture_cache,
                        rep_material,
                    );
                    self.merged_batch_cache.push(CachedMergedBatch {
                        vbuf,
                        ibuf,
                        index_count,
                        mat_bg,
                        double_sided: key.double_sided,
                        mirrored: key.mirrored,
                        fingerprint: key.fingerprint,
                        xform_sig: merge_group_xform_sig(members, snapshot),
                        members: members.clone(),
                        #[cfg(feature = "materialx-wgsl")]
                        wgsl_key: key.wgsl_key,
                        #[cfg(feature = "metrics")]
                        source_count: u32_sat(members.len()),
                    });
                    #[cfg(feature = "metrics")]
                    {
                        merged_batch_rebakes += 1;
                    }
                    rebaked_now += 1;
                }
                self.last_merge_serial = snapshot.serial;
            } else {
                // Warm frame: re-bake only the groups whose members moved.
                for (i, (_key, members)) in merge_groups.iter().enumerate() {
                    let sig = merge_group_xform_sig(members, snapshot);
                    if self.merged_batch_cache[i].xform_sig == sig {
                        continue;
                    }
                    let (verts, indices) = bake_merge_group_geometry(members, snapshot)?;
                    let (opt_vbuf, opt_ibuf) = if verts.is_empty() || indices.is_empty() {
                        (None, None)
                    } else {
                        let vbuf = self
                            .device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Merged Batch Vbuf (rebake)"),
                                contents: cast_slice(&verts),
                                usage: wgpu::BufferUsages::VERTEX,
                            });
                        let ibuf = self
                            .device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("Merged Batch Ibuf (rebake)"),
                                contents: cast_slice(&indices),
                                usage: wgpu::BufferUsages::INDEX,
                            });
                        (Some(vbuf), Some(ibuf))
                    };
                    let cb = &mut self.merged_batch_cache[i];
                    if let (Some(vbuf), Some(ibuf)) = (opt_vbuf, opt_ibuf) {
                        cb.vbuf = vbuf;
                        cb.ibuf = ibuf;
                    }
                    cb.index_count = u32_sat(indices.len());
                    cb.xform_sig = sig;
                    #[cfg(feature = "metrics")]
                    {
                        merged_batch_rebakes += 1;
                    }
                    rebaked_now += 1;
                }
            }
            // Rate-limited always-on log (grep `merge-rebake-diag`): confirms
            // whether the warm-frame selective re-bake is following moved folded
            // members (e.g. the controller axis-gizmo cubes) or leaving them frozen.
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static N: AtomicU64 = AtomicU64::new(0);
                if N.fetch_add(1, Ordering::Relaxed) % 240 == 1 {
                    eprintln!(
                        "merge-rebake-diag merge_groups={} rebaked={rebaked_now} full_rebuild={was_full_rebuild}",
                        merge_groups.len()
                    );
                }
            }
        }

        // ── Per-mesh GPU resources for draw recording ──────────────────────
        #[cfg(feature = "metrics")]
        let instance_cache_ns = nanos_u64(t_instance_cache.elapsed().as_nanos());
        #[cfg(feature = "metrics")]
        let t_draw_list = std::time::Instant::now();
        // Build the draw list by cloning lightweight wgpu handle references
        // from the cache.  wgpu handles are Arc-backed; clone is O(1).
        struct MeshGpu {
            vbuf: wgpu::Buffer,
            ibuf: wgpu::Buffer,
            mesh_bg: wgpu::BindGroup,
            mat_bg: wgpu::BindGroup,
            index_count: u32,
            double_sided: bool,
            /// World transform has a negative determinant (mirrored geometry).
            /// Requires `FrontFace::Cw` pipeline to avoid culling the visible face.
            mirrored: bool,
            /// True when `AlphaPolicy::Blend` — drawn in a separate back-to-front pass.
            transparent: bool,
            /// `idtx:castsShadows` (default true). False → skipped in the shadow
            /// depth pre-pass (and excluded from static-merge so a non-caster can
            /// never ride a merged batch's shadow).
            casts_shadow: bool,
            /// True when this is colored glass (Blend + non-white `transmission_color)`:
            /// drawn with the constant-dst-blend pipeline and `glass_blend` as the
            /// per-draw blend constant, so the background is tinted by the glass hue.
            colored_glass: bool,
            /// Per-draw blend constant for colored glass: `transmission_color · (1-opacity)`.
            glass_blend: wgpu::Color,
            /// Squared camera distance used to sort transparent meshes back-to-front.
            depth_sort_key: f32,
            /// Skin vertex buffer (joint indices + weights, slot 1) for GPU LBS.
            skin_vbuf: Option<wgpu::Buffer>,
            /// Joint matrix palette bind group (group 4) for GPU LBS.
            skin_bg: Option<wgpu::BindGroup>,
            /// True when `mat_bg` is a UDIM array bind group → draw with
            /// `pipeline.udim.select(..)`. Always false for skinned/instanced.
            udim: bool,
            /// Hash of the `MaterialX` WGSL source, used to look up per-material pipelines.
            #[cfg(feature = "materialx-wgsl")]
            wgsl_key: Option<u64>,
            /// Pipeline sort key for state-change minimisation (Step 3).
            pipeline_sort: u64,
        }

        let mut mesh_gpus: Vec<MeshGpu> = Vec::with_capacity(snapshot.meshes.len());
        {
            let mut ordinals: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for (idx, inst) in snapshot.meshes.iter().enumerate() {
                let ord = ordinals.entry(inst.prim_path.as_str()).or_insert(0);
                let key = (inst.prim_path.clone(), *ord);
                *ord += 1;
                // Skip merged meshes — no cache entry was built for them in Phase 4.
                // Ordinal advances above to keep keys aligned with Phase 1.
                if merged_mesh[idx] {
                    continue;
                }
                let e = self.mesh_gpu_cache.get(&key).expect("populated in Phase 4");
                let double_sided = inst.material.double_sided || inst.mesh.double_sided;
                let mirrored = inst.world_matrix.determinant() < 0.0;
                let transparent = inst.material.is_transparent();
                let colored_glass = inst.material.has_colored_transmission();
                let glass_blend = inst.material.glass_blend_constant();
                #[cfg(feature = "materialx-wgsl")]
                let wgsl_key = e.wgsl_key;
                #[cfg(feature = "materialx-wgsl")]
                let pipeline_sort = (wgsl_key.unwrap_or(0) & 0xFFFF_FFFF_FFFF_0000)
                    | (u64::from(double_sided) << 2)
                    | (u64::from(mirrored) << 1);
                #[cfg(not(feature = "materialx-wgsl"))]
                let pipeline_sort = (u64::from(double_sided) << 2) | (u64::from(mirrored) << 1);
                let mesh_pos = inst.world_matrix.col(3).truncate();
                mesh_gpus.push(MeshGpu {
                    vbuf: e.vbuf.clone(),
                    ibuf: e.ibuf.clone(),
                    mesh_bg: e.mesh_bg.clone(),
                    mat_bg: e.mat_bg.clone(),
                    index_count: e.index_count,
                    double_sided,
                    mirrored,
                    casts_shadow: inst.casts_shadow,
                    transparent,
                    colored_glass,
                    glass_blend,
                    depth_sort_key: (mesh_pos - eye_world).length_squared(),
                    skin_vbuf: e.skin_vbuf.clone(),
                    skin_bg: e.skin_bg.clone(),
                    udim: e.udim,
                    #[cfg(feature = "materialx-wgsl")]
                    wgsl_key,
                    pipeline_sort,
                });
            }
        }

        // Sort opaque meshes by pipeline key to minimise pipeline/bind-group
        // state changes from O(mesh_count) to O(unique_pipeline_count).
        // Transparent meshes are sorted back-to-front in the draw pass below.
        mesh_gpus.sort_unstable_by_key(|g| {
            if g.transparent {
                u64::MAX
            } else {
                g.pipeline_sort
            }
        });

        // ── Render pass ────────────────────────────────────────────────────
        #[cfg(feature = "metrics")]
        let draw_list_build_ns = nanos_u64(t_draw_list.elapsed().as_nanos());
        #[cfg(feature = "metrics")]
        let t_gpu_passes = std::time::Instant::now();
        // Pass-boundary GPU timestamps (only when the adapter supports them).
        #[cfg(feature = "metrics")]
        let (pbr_ts, tonemap_ts) = match &self.gpu_timestamps {
            Some(ts) => (Some(ts.pbr_writes()), Some(ts.tonemap_writes())),
            None => (None, None),
        };
        #[cfg(not(feature = "metrics"))]
        let (pbr_ts, tonemap_ts): (
            Option<wgpu::RenderPassTimestampWrites<'_>>,
            Option<wgpu::RenderPassTimestampWrites<'_>>,
        ) = (None, None);
        // Sub-phase decomposition of the CPU submission window (F1 instrumentation,
        // docs/draw-submission-analysis.md §4). These buckets partition
        // `gpu_passes_ns` so the dominant term (recording vs backend translation
        // vs submit) is visible. `t_phase` is a rolling timer advanced at each
        // pass boundary; the actual-pipeline-switch count comes from `bound`
        // (`BoundState`), which records only non-redundant `set_*` commands.
        #[cfg(feature = "metrics")]
        let (
            mut pass_setup_ns,
            mut encode_opaque_ns,
            mut encode_transparent_ns,
            mut encode_tonemap_ns,
            mut finish_ns,
            mut submit_ns,
        ): (u64, u64, u64, u64, u64, u64) = (0, 0, 0, 0, 0, 0);
        // `draw_calls` counts every `draw_indexed`/`draw` (never deduplicated).
        // The other three are read out of `bound` after the pass — the dedup
        // guards there record only the commands that actually reached the stream.
        #[cfg(feature = "metrics")]
        let (mut draw_calls, commands_encoded, pipeline_switches, bind_group_sets): (
            u32,
            u32,
            u32,
            u32,
        );
        #[cfg(feature = "metrics")]
        {
            draw_calls = 0;
        }
        // Colored-glass draws (Pass 2 + Pass 4) routed to the constant-dst blend
        // pipeline — measurable so the added path can be tracked in large scenes.
        #[cfg(feature = "metrics")]
        let mut colored_glass_draws: u32 = 0;
        #[cfg(feature = "metrics")]
        let mut t_phase = std::time::Instant::now();
        // Redundant-state dedup tracker — active in every build (this is the
        // optimization, not instrumentation). See `BoundState`.
        let mut bound = BoundState::default();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PBR Encoder"),
            });

        // ── Shadow depth pre-pass ──────────────────────────────────────────
        // Render caster geometry into each allocated atlas layer from its
        // light's point of view, recorded before the beauty pass in the *same*
        // encoder so the GPU orders the depth writes ahead of the PBR sampling.
        // The pre-pass casts static per-mesh, hardware-instanced, and
        // GPU-skinned geometry. The per-slot light-space
        // camera bind groups and their buffers are held in Vecs at this scope so
        // they outlive `encoder.finish()` / `queue.submit()` below.
        let mut _shadow_cam_bufs: Vec<wgpu::Buffer> = Vec::new();
        let mut shadow_cam_bgs: Vec<wgpu::BindGroup> = Vec::new();
        if shadows_on {
            for slot in &snapshot.shadow_slots {
                let cam_u = CameraUniforms {
                    view_proj: slot.view_proj.to_cols_array_2d(),
                    eye_pos: [0.0; 4],
                    exposure: 0.0,
                    _pad1: [0.0; 3],
                };
                let buf = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("Shadow Slot Camera"),
                        contents: bytemuck::bytes_of(&cam_u),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Shadow Slot Camera BG"),
                    layout: &self.pipeline.camera_bgl,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    }],
                });
                shadow_cam_bgs.push(bg);
                _shadow_cam_bufs.push(buf);
            }
            let atlas = self.shadow.atlas.as_ref().unwrap();
            let mut _shadow_draws = 0u32;
            let (mut _n_static, mut _n_merged, mut _n_inst, mut _n_skin) = (0u32, 0u32, 0u32, 0u32);
            // ShadowCache: the caster signature is the same for every
            // slot this frame; the light-matrix hash is per slot. A slot whose
            // `(light_matrix_hash, casters_epoch)` matches the last render keeps
            // its atlas layer's depth and skips the pass. Cache writes are deferred
            // to after the loop because the live `atlas` borrow of `self.shadow`
            // rules out a `&mut self.shadow_cache` here.
            let casters_epoch = casters_signature(snapshot);
            let mut cache_updates: Vec<(u32, (u64, u64))> = Vec::new();
            let mut any_ran = false;
            let mut _n_cached = 0u32;
            for (idx, slot) in snapshot.shadow_slots.iter().enumerate() {
                let layer = slot.slot as usize;
                if layer >= atlas.layer_views.len() {
                    continue;
                }
                let cache_key = (light_matrix_hash(&slot.view_proj), casters_epoch);
                if self.shadow_cache.get(&slot.light_index) == Some(&cache_key) {
                    // Unchanged light frustum + unchanged casters → the layer's
                    // depth is still valid. Skip the whole depth pass for this slot.
                    _n_cached += 1;
                    continue;
                }
                cache_updates.push((slot.light_index, cache_key));
                any_ran = true;
                // Bracket only the first depth pass with the shadow timestamp; the
                // atlas fill is dominated by the first (and often only) caster.
                #[cfg(feature = "metrics")]
                let shadow_ts = if idx == 0 {
                    self.gpu_timestamps.as_ref().map(|ts| ts.shadow_writes())
                } else {
                    None
                };
                #[cfg(not(feature = "metrics"))]
                let shadow_ts: Option<wgpu::RenderPassTimestampWrites<'_>> = None;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Shadow Depth Pass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &atlas.layer_views[layer],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: shadow_ts,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                // 1. Static per-mesh opaque casters (identity or authored model).
                pass.set_pipeline(&self.pipeline.shadow_depth);
                pass.set_bind_group(0, &shadow_cam_bgs[idx], &[]);
                for gpu in &mesh_gpus {
                    // Transparent surfaces do not write shadow depth; skinned
                    // casters are drawn below with the skinned depth pipeline;
                    // `idtx:castsShadows = false` prims opt out entirely.
                    if gpu.transparent || gpu.skin_bg.is_some() || !gpu.casts_shadow {
                        continue;
                    }
                    pass.set_bind_group(1, &gpu.mesh_bg, &[]);
                    pass.set_vertex_buffer(0, gpu.vbuf.slice(..));
                    pass.set_index_buffer(gpu.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..gpu.index_count, 0, 0..1);
                    _shadow_draws += 1;
                    _n_static += 1;
                }

                // 2. Static draw-merge batches (world-baked → identity model).
                if let Some(identity_bg) = self.identity_mesh_bg.as_ref() {
                    pass.set_bind_group(1, identity_bg, &[]);
                    for mb in &self.merged_batch_cache {
                        if mb.index_count == 0 {
                            continue;
                        }
                        pass.set_vertex_buffer(0, mb.vbuf.slice(..));
                        pass.set_index_buffer(mb.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..mb.index_count, 0, 0..1);
                        _shadow_draws += 1;
                        _n_merged += 1;
                    }
                }

                // 3. Hardware-instanced opaque batches (per-instance model on slot 1).
                pass.set_pipeline(&self.pipeline.shadow_depth_instanced);
                pass.set_bind_group(0, &shadow_cam_bgs[idx], &[]);
                for (b, batch) in snapshot.instanced_batches.iter().enumerate() {
                    // Single-instance batches folded into a static merge already cast
                    // above; transparent batches do not write depth; non-casting
                    // batches (`idtx:castsShadows = false`) opt out.
                    if merged_batch_flags[b]
                        || batch.material.is_transparent()
                        || !batch.casts_shadow
                    {
                        continue;
                    }
                    if let Some(e) = self.instance_batch_cache.get(&batch.prototype_key) {
                        if e.instance_count == 0 {
                            continue;
                        }
                        pass.set_vertex_buffer(0, e.vbuf.slice(..));
                        pass.set_vertex_buffer(1, e.instance_buf.slice(..));
                        pass.set_index_buffer(e.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..e.index_count, 0, 0..e.instance_count);
                        _shadow_draws += 1;
                        _n_inst += 1;
                    }
                }

                // 4. GPU-skinned opaque casters (joint palette at group 1, LBS in VS).
                pass.set_pipeline(&self.pipeline.shadow_depth_skinned);
                pass.set_bind_group(0, &shadow_cam_bgs[idx], &[]);
                for gpu in &mesh_gpus {
                    if gpu.transparent || !gpu.casts_shadow {
                        continue;
                    }
                    let (Some(skin_bg), Some(skin_vbuf)) = (&gpu.skin_bg, &gpu.skin_vbuf) else {
                        continue;
                    };
                    pass.set_bind_group(1, skin_bg, &[]);
                    pass.set_vertex_buffer(0, gpu.vbuf.slice(..));
                    pass.set_vertex_buffer(1, skin_vbuf.slice(..));
                    pass.set_index_buffer(gpu.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..gpu.index_count, 0, 0..1);
                    _shadow_draws += 1;
                    _n_skin += 1;
                }
            }
            // The `atlas` immutable borrow of `self.shadow` ends with the loop, so
            // the deferred ShadowCache writes can land now. `shadow_pass_ran` gates
            // the metrics read: a fully-cached frame did zero depth draws, so its
            // shadow timestamp span is stale and must report 0 ns.
            self.shadow_pass_ran = any_ran;
            for (light_index, cache_key) in cache_updates {
                self.shadow_cache.insert(light_index, cache_key);
            }
            if std::env::var_os("IDTX_SHADOW_DEBUG").is_some() {
                eprintln!(
                    "[shadow] depth-pass draws={} (static={} merged={} inst={} skin={}) \
                     slots_cached={} mesh_gpus={} inst_batches={} inst_cached={}",
                    _shadow_draws,
                    _n_static,
                    _n_merged,
                    _n_inst,
                    _n_skin,
                    _n_cached,
                    mesh_gpus.len(),
                    snapshot.instanced_batches.len(),
                    self.instance_batch_cache.len(),
                );
                for (b, batch) in snapshot.instanced_batches.iter().enumerate() {
                    eprintln!(
                        "[shadow]  inst[{b}] key={} merged_flag={} transparent={} cache_hit={} count={}",
                        batch.prototype_key,
                        merged_batch_flags.get(b).copied().unwrap_or(false),
                        batch.material.is_transparent(),
                        self.instance_batch_cache.contains_key(&batch.prototype_key),
                        self.instance_batch_cache
                            .get(&batch.prototype_key)
                            .map_or(0, |e| e.instance_count),
                    );
                }
            }
        }
        // ── Camera Z-prepass (QI-4) ─────────────────────────────────────────────
        // When enabled, lay down the frontmost opaque depth first so the beauty
        // opaque pass can test `LessEqual` with depth-write off — each pixel/sample
        // is shaded ~once, removing the measured 3.6× Quest overdraw. The prepass
        // pipelines reuse the beauty `vs_main` so the depth is bit-identical; it
        // writes into `target.depth_view` (`Clear`→`Store`) and the beauty pass
        // loads that depth instead of clearing. Same `encoder` as the shadow +
        // beauty passes so the GPU runs them in order. `None` (and the beauty pass
        // clears depth) on desktop/PCVR where overdraw is < 1×.
        if let Some(prepass) = self.pipeline.depth_prepass.as_ref() {
            let mut zpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Z-Prepass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The prepass pipelines use the two-group shadow layouts: group 0 =
            // camera (set once here), group 1 = per-draw mesh / skin palette. No
            // lights or material group — the depth-only vertex stage needs neither.
            zpass.set_bind_group(0, &cam_bg, &[]);

            // Static per-mesh + skinned opaque (UDIM meshes share the static
            // prepass — their `vs_main` computes the same clip transform).
            for gpu in &mesh_gpus {
                if gpu.transparent {
                    continue;
                }
                if let (Some(skin_bg), Some(skin_vbuf)) = (&gpu.skin_bg, &gpu.skin_vbuf) {
                    zpass.set_pipeline(prepass.skin.select(gpu.double_sided, gpu.mirrored));
                    zpass.set_bind_group(1, skin_bg, &[]);
                    zpass.set_vertex_buffer(0, gpu.vbuf.slice(..));
                    zpass.set_vertex_buffer(1, skin_vbuf.slice(..));
                } else {
                    zpass.set_pipeline(prepass.stat.select(gpu.double_sided, gpu.mirrored));
                    zpass.set_bind_group(1, &gpu.mesh_bg, &[]);
                    zpass.set_vertex_buffer(0, gpu.vbuf.slice(..));
                }
                zpass.set_index_buffer(gpu.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                zpass.draw_indexed(0..gpu.index_count, 0, 0..1);
            }

            // Merged static batches (world-baked geometry, identity model at group 1).
            if !self.merged_batch_cache.is_empty() {
                let identity_bg = self.identity_mesh_bg.as_ref().unwrap();
                zpass.set_bind_group(1, identity_bg, &[]);
                for mb in &self.merged_batch_cache {
                    if mb.index_count == 0 {
                        continue;
                    }
                    zpass.set_pipeline(prepass.stat.select(mb.double_sided, mb.mirrored));
                    zpass.set_vertex_buffer(0, mb.vbuf.slice(..));
                    zpass.set_index_buffer(mb.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    zpass.draw_indexed(0..mb.index_count, 0, 0..1);
                }
            }

            // Opaque hardware-instanced batches (per-instance model on slot 1). The
            // instanced prepass layout is `[camera]` only — group 0 (already set)
            // satisfies it; the model matrix rides the instance vertex buffer.
            for (b, batch) in snapshot.instanced_batches.iter().enumerate() {
                if merged_batch_flags[b] || batch.material.is_transparent() {
                    continue;
                }
                if let Some(e) = self.instance_batch_cache.get(&batch.prototype_key) {
                    if e.instance_count == 0 {
                        continue;
                    }
                    zpass.set_pipeline(prepass.inst.select(e.double_sided, false));
                    zpass.set_vertex_buffer(0, e.vbuf.slice(..));
                    zpass.set_vertex_buffer(1, e.instance_buf.slice(..));
                    zpass.set_index_buffer(e.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    zpass.draw_indexed(0..e.index_count, 0, 0..e.instance_count);
                }
            }
        }
        // NOTE: the beauty pass reuses the SAME `encoder` that recorded the shadow
        // depth pre-pass above, so the GPU executes the depth writes before the PBR
        // pass samples the atlas. Creating a second encoder here would orphan the
        // shadow depth commands (never submitted → atlas stays uninitialized →
        // every receiver reads as shadowed → the whole scene goes black).
        {
            // Beauty color attachment. With MSAA (2×/4×) the pass renders into the
            // multisampled `msaa_color_view` and resolves into the single-sample
            // `hdr_color_view` (samples discarded). At 1× there is no MSAA target —
            // wgpu rejects a `resolve_target` on a single-sample attachment — so the
            // pass renders directly into `hdr_color_view` and must *store* it.
            let (beauty_view, beauty_resolve, beauty_store) = match target.msaa_color_view.as_ref() {
                Some(msaa) => (msaa, Some(&target.hdr_color_view), wgpu::StoreOp::Discard),
                None => (&target.hdr_color_view, None, wgpu::StoreOp::Store),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("PBR Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: beauty_view,
                    resolve_target: beauty_resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(target.clear_color),                        
                        store: beauty_store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        // With a Z-prepass the opaque depth is already resident, so
                        // load it (clearing would wipe the prepass result and the
                        // LessEqual/write-off opaque test would reject everything).
                        // Without a prepass this pass is the first depth writer and
                        // clears as before.
                        load: if self.z_prepass {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(1.0)
                        },
                        // XR eye targets keep depth so it can be resolved into the
                        // OpenXR depth swapchain; other targets discard after use.
                        store: if target.resolve_depth {
                            wgpu::StoreOp::Store
                        } else {
                            wgpu::StoreOp::Discard
                        },
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: pbr_ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            let use_wireframe = wireframe && self.pipeline.wireframe.is_some();

            bound.set_bind_group(&mut pass, 0, &cam_bg);
            bound.set_bind_group(&mut pass, 1, &lights_bg);
            // End of fixed per-pass setup (encoder create + begin_render_pass +
            // the two frame-constant bind groups). The two binds above are
            // recorded through `bound` and counted there.
            #[cfg(feature = "metrics")]
            {
                pass_setup_ns = nanos_u64(t_phase.elapsed().as_nanos());
                t_phase = std::time::Instant::now();
            }
            // Begin the fragment-invocation (overdraw) pipeline-statistics query
            // around all PBR-pass draws. No-op unless the adapter supports
            // PIPELINE_STATISTICS_QUERY (unsupported on Metal). Query commands
            // only — pixel-neutral.
            #[cfg(feature = "metrics")]
            if let Some(ps) = &self.gpu_pipeline_stats {
                pass.begin_pipeline_statistics_query(&ps.query_set, 0);
            }

            // Pass 1 — opaque geometry (depth-write ON, any order).
            // In wireframe mode, all geometry (including transparent) is drawn here.
            for gpu in &mesh_gpus {
                if !use_wireframe && gpu.transparent {
                    continue;
                }
                if let (Some(skin_bg), Some(skin_vbuf)) = (&gpu.skin_bg, &gpu.skin_vbuf) {
                    let pipe = if use_wireframe {
                        // wireframe mode will never cull backfaces and thus always render "double sided"
                        self.pipeline
                            .wireframe
                            .as_ref()
                            .unwrap()
                            .skin
                            .select(true, gpu.mirrored)
                    } else {
                        self.pipeline
                            .skin
                            .select(gpu.double_sided, gpu.mirrored, false)
                    };
                    bound.set_pipeline(&mut pass, pipe);
                    bound.set_bind_group(&mut pass, 2, skin_bg);
                    bound.set_bind_group(&mut pass, 3, &gpu.mat_bg);
                    bound.set_vertex_buffer(&mut pass, 0, &gpu.vbuf);
                    bound.set_vertex_buffer(&mut pass, 1, skin_vbuf);
                } else {
                    let pipe = if use_wireframe {
                        // wireframe mode will never cull backfaces and thus always render "double sided"
                        let wf = self.pipeline.wireframe.as_ref().unwrap();
                        if gpu.udim {
                            wf.stat_udim.select(true, gpu.mirrored)
                        } else {
                            wf.stat.select(true, gpu.mirrored)
                        }
                    } else if gpu.udim {
                        self.pipeline
                            .udim
                            .select(gpu.double_sided, gpu.mirrored, false)
                    } else {
                        #[cfg(feature = "materialx-wgsl")]
                        {
                            gpu.wgsl_key
                                .and_then(|k| self.wgsl_pipeline_cache.get(&k))
                                .and_then(Option::as_ref).map_or_else(|| {
                                    if gpu.double_sided {
                                        &self.pipeline.double_sided
                                    } else if gpu.mirrored {
                                        &self.pipeline.mirrored_single_sided
                                    } else {
                                        &self.pipeline.single_sided
                                    }
                                }, |mtlx| mtlx.select(gpu.double_sided, gpu.mirrored, false))
                        }
                        #[cfg(not(feature = "materialx-wgsl"))]
                        {
                            if gpu.double_sided {
                                &self.pipeline.double_sided
                            } else if gpu.mirrored {
                                &self.pipeline.mirrored_single_sided
                            } else {
                                &self.pipeline.single_sided
                            }
                        }
                    };
                    bound.set_pipeline(&mut pass, pipe);
                    bound.set_bind_group(&mut pass, 2, &gpu.mesh_bg);
                    bound.set_bind_group(&mut pass, 3, &gpu.mat_bg);
                    bound.set_vertex_buffer(&mut pass, 0, &gpu.vbuf);
                }
                bound.set_index_buffer(&mut pass, &gpu.ibuf);
                pass.draw_indexed(0..gpu.index_count, 0, 0..1);
                #[cfg(feature = "metrics")]
                {
                    draw_calls += 1;
                }
            }

            // Pass 1b — static draw-merge batches (opaque, baked to world space).
            // Each collapses ≥2 meshes / single-instance batches sharing a pipeline
            // + material into one `draw_indexed`, bound through the identity group-2
            // uniform. Rolls into the opaque encode bucket.
            {
                let identity_bg = self.identity_mesh_bg.as_ref().unwrap();
                for mb in &self.merged_batch_cache {
                    if mb.index_count == 0 {
                        continue;
                    }
                    let pipe = if use_wireframe {
                        self.pipeline
                            .wireframe
                            .as_ref()
                            .unwrap()
                            .stat
                            .select(true, mb.mirrored)
                    } else {
                        #[cfg(feature = "materialx-wgsl")]
                        {
                            mb.wgsl_key
                                .and_then(|k| self.wgsl_pipeline_cache.get(&k))
                                .and_then(Option::as_ref)
                                .map_or_else(
                                    || {
                                        if mb.double_sided {
                                            &self.pipeline.double_sided
                                        } else if mb.mirrored {
                                            &self.pipeline.mirrored_single_sided
                                        } else {
                                            &self.pipeline.single_sided
                                        }
                                    },
                                    |mtlx| mtlx.select(mb.double_sided, mb.mirrored, false),
                                )
                        }
                        #[cfg(not(feature = "materialx-wgsl"))]
                        {
                            if mb.double_sided {
                                &self.pipeline.double_sided
                            } else if mb.mirrored {
                                &self.pipeline.mirrored_single_sided
                            } else {
                                &self.pipeline.single_sided
                            }
                        }
                    };
                    bound.set_pipeline(&mut pass, pipe);
                    bound.set_bind_group(&mut pass, 2, identity_bg);
                    bound.set_bind_group(&mut pass, 3, &mb.mat_bg);
                    bound.set_vertex_buffer(&mut pass, 0, &mb.vbuf);
                    bound.set_index_buffer(&mut pass, &mb.ibuf);
                    pass.draw_indexed(0..mb.index_count, 0, 0..1);
                    #[cfg(feature = "metrics")]
                    {
                        draw_calls += 1;
                    }
                }
            }
            // Pass 3 — opaque instanced geometry (all instances in wireframe mode).
            // Drawn alongside Pass 1 / 1b so ALL opaque depth is resident before the
            // skybox, letting the backdrop's depth test reject every covered pixel.
            {
                let identity_bg = self.identity_mesh_bg.as_ref().unwrap();
                for (b, batch) in snapshot.instanced_batches.iter().enumerate() {
                    // Single-instance batches folded into a static merge are drawn
                    // in Pass 1b instead — skip them here.
                    if merged_batch_flags[b] {
                        continue;
                    }
                    if !use_wireframe && batch.material.is_transparent() {
                        continue;
                    }
                    if let Some(e) = self.instance_batch_cache.get(&batch.prototype_key) {
                        if e.instance_count == 0 {
                            continue;
                        }
                        let pipe = if use_wireframe {
                            let wf = self.pipeline.wireframe.as_ref().unwrap();
                            if e.udim {
                                wf.inst_udim.select(e.double_sided, false)
                            } else {
                                wf.inst.select(e.double_sided, false)
                            }
                        } else if e.udim {
                            self.pipeline
                                .udim_instanced
                                .select(e.double_sided, false, false)
                        } else {
                            self.pipeline
                                .instancing
                                .select(e.double_sided, false, false)
                        };
                        bound.set_pipeline(&mut pass, pipe);
                        bound.set_bind_group(&mut pass, 2, identity_bg);
                        bound.set_bind_group(&mut pass, 3, &e.mat_bg);
                        bound.set_vertex_buffer(&mut pass, 0, &e.vbuf);
                        bound.set_vertex_buffer(&mut pass, 1, &e.instance_buf);
                        bound.set_index_buffer(&mut pass, &e.ibuf);
                        pass.draw_indexed(0..e.index_count, 0, 0..e.instance_count);
                        #[cfg(feature = "metrics")]
                        {
                            draw_calls += 1;
                        }
                    }
                }
            }
            #[cfg(feature = "metrics")]
            {
                // Pass 1 opaque per-mesh recording (Pass 3 opaque instanced and the
                // axis overlay are added to this bucket below).
                encode_opaque_ns += nanos_u64(t_phase.elapsed().as_nanos());
                t_phase = std::time::Instant::now();
            }
            // Interior timestamp 4 — end of ALL opaque geometry (Pass 1 + 1b + 3),
            // start of the skybox + transparent segment. No-op unless INSIDE_PASSES.
            #[cfg(feature = "metrics")]
            if let Some(ts) = &self.gpu_timestamps {
                ts.write_interior(&mut pass, 4);
            }

            // Environment backdrop: draw the dome equirect AFTER all opaque geometry
            // and BEFORE transparent geometry. Far plane, depth-compare `LessEqual` +
            // no depth write → it fills only pixels no opaque surface covered (early-Z
            // skips the equirect shader on the rest) while transparent surfaces still
            // blend over it. Raw pass calls bypass `bound`, so afterwards reset `bound`
            // and re-bind groups 0/1 for the transparent draws (which only set 2/3).
            // Skipped in wireframe mode.
            if let (false, Some(sky_bg)) = (use_wireframe, &skybox_bg) {
                pass.set_pipeline(&self.skybox_pipeline.pipeline);
                pass.set_bind_group(0, sky_bg, &[]);
                pass.draw(0..3, 0..1);
                bound.reset_bindings();
                bound.set_bind_group(&mut pass, 0, &cam_bg);
                bound.set_bind_group(&mut pass, 1, &lights_bg);
            }

            // Pass 2 — transparent geometry (depth-write OFF, back-to-front).
            // Skipped in wireframe mode — all geometry was already drawn in Pass 1.
            if !use_wireframe {
                let mut transparent_order: Vec<usize> = mesh_gpus
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.transparent)
                    .map(|(i, _)| i)
                    .collect();
                transparent_order.sort_by(|&a, &b| {
                    mesh_gpus[b]
                        .depth_sort_key
                        .partial_cmp(&mesh_gpus[a].depth_sort_key)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                for idx in transparent_order {
                    let gpu = &mesh_gpus[idx];
                    if let (Some(skin_bg), Some(skin_vbuf)) = (&gpu.skin_bg, &gpu.skin_vbuf) {
                        let pipe = self
                            .pipeline
                            .skin
                            .select(gpu.double_sided, gpu.mirrored, true);
                        bound.set_pipeline(&mut pass, pipe);
                        bound.set_bind_group(&mut pass, 2, skin_bg);
                        bound.set_bind_group(&mut pass, 3, &gpu.mat_bg);
                        bound.set_vertex_buffer(&mut pass, 0, &gpu.vbuf);
                        bound.set_vertex_buffer(&mut pass, 1, skin_vbuf);
                    } else if gpu.colored_glass && !gpu.udim {
                        // Colored glass → per-channel constant-dst blend pipeline,
                        // with the per-draw blend constant tinting the background by
                        // transmission_color · (1-opacity). Skinned/UDIM colored glass
                        // is out of scope and falls through to the plain paths.
                        let pipe = if gpu.double_sided {
                            &self.pipeline.transparent_colored_double_sided
                        } else if gpu.mirrored {
                            &self.pipeline.transparent_colored_mirrored_single_sided
                        } else {
                            &self.pipeline.transparent_colored_single_sided
                        };
                        bound.set_pipeline(&mut pass, pipe);
                        // Set every colored draw: the constant is per-material and
                        // `bound` only dedups pipeline/bind-group state, not this.
                        pass.set_blend_constant(gpu.glass_blend);
                        bound.set_bind_group(&mut pass, 2, &gpu.mesh_bg);
                        bound.set_bind_group(&mut pass, 3, &gpu.mat_bg);
                        bound.set_vertex_buffer(&mut pass, 0, &gpu.vbuf);
                        #[cfg(feature = "metrics")]
                        {
                            colored_glass_draws += 1;
                        }
                    } else {
                        #[cfg(feature = "materialx-wgsl")]
                        let pipe = if gpu.udim {
                            self.pipeline
                                .udim
                                .select(gpu.double_sided, gpu.mirrored, true)
                        } else {
                            gpu.wgsl_key
                                .and_then(|k| self.wgsl_pipeline_cache.get(&k))
                                .and_then(Option::as_ref).map_or_else(|| {
                                    if gpu.double_sided {
                                        &self.pipeline.transparent_double_sided
                                    } else if gpu.mirrored {
                                        &self.pipeline.transparent_mirrored_single_sided
                                    } else {
                                        &self.pipeline.transparent_single_sided
                                    }
                                }, |mtlx| mtlx.select(gpu.double_sided, gpu.mirrored, true))
                        };
                        #[cfg(not(feature = "materialx-wgsl"))]
                        let pipe = if gpu.udim {
                            self.pipeline
                                .udim
                                .select(gpu.double_sided, gpu.mirrored, true)
                        } else if gpu.double_sided {
                            &self.pipeline.transparent_double_sided
                        } else if gpu.mirrored {
                            &self.pipeline.transparent_mirrored_single_sided
                        } else {
                            &self.pipeline.transparent_single_sided
                        };
                        bound.set_pipeline(&mut pass, pipe);
                        bound.set_bind_group(&mut pass, 2, &gpu.mesh_bg);
                        bound.set_bind_group(&mut pass, 3, &gpu.mat_bg);
                        bound.set_vertex_buffer(&mut pass, 0, &gpu.vbuf);
                    }
                    bound.set_index_buffer(&mut pass, &gpu.ibuf);
                    pass.draw_indexed(0..gpu.index_count, 0, 0..1);
                    #[cfg(feature = "metrics")]
                    {
                        draw_calls += 1;
                    }
                }
            } // end if !use_wireframe (Pass 2)
            #[cfg(feature = "metrics")]
            {
                // Pass 2 transparent per-mesh recording, including the depth sort
                // above (Pass 4 transparent instanced is added below).
                encode_transparent_ns += nanos_u64(t_phase.elapsed().as_nanos());
                t_phase = std::time::Instant::now();
            }
            // Interior timestamp 5 — end of transparent segment 2 (Pass 2), start
            // of opaque segment 3. No-op unless INSIDE_PASSES.
            #[cfg(feature = "metrics")]
            if let Some(ts) = &self.gpu_timestamps {
                ts.write_interior(&mut pass, 5);
            }

            // Pass 3 — opaque instanced geometry (all instances in wireframe mode)
            {
                let identity_bg = self.identity_mesh_bg.as_ref().unwrap();
                for (b, batch) in snapshot.instanced_batches.iter().enumerate() {
                    // Single-instance batches folded into a static merge are drawn
                    // in Pass 1b instead — skip them here.
                    if merged_batch_flags[b] {
                        continue;
                    }
                    if !use_wireframe && batch.material.is_transparent() {
                        continue;
                    }
                    if let Some(e) = self.instance_batch_cache.get(&batch.prototype_key) {
                        if e.instance_count == 0 {
                            continue;
                        }
                        let pipe = if use_wireframe {
                            let wf = self.pipeline.wireframe.as_ref().unwrap();
                            if e.udim {
                                wf.inst_udim.select(e.double_sided, false)
                            } else {
                                wf.inst.select(e.double_sided, false)
                            }
                        } else if e.udim {
                            self.pipeline
                                .udim_instanced
                                .select(e.double_sided, false, false)
                        } else {
                            self.pipeline
                                .instancing
                                .select(e.double_sided, false, false)
                        };
                        bound.set_pipeline(&mut pass, pipe);
                        bound.set_bind_group(&mut pass, 2, identity_bg);
                        bound.set_bind_group(&mut pass, 3, &e.mat_bg);
                        bound.set_vertex_buffer(&mut pass, 0, &e.vbuf);
                        bound.set_vertex_buffer(&mut pass, 1, &e.instance_buf);
                        bound.set_index_buffer(&mut pass, &e.ibuf);
                        pass.draw_indexed(0..e.index_count, 0, 0..e.instance_count);
                        #[cfg(feature = "metrics")]
                        {
                            draw_calls += 1;
                        }
                    }
                }
            }
            #[cfg(feature = "metrics")]
            {
                // Pass 3 opaque instanced recording rolls into the opaque bucket.
                encode_opaque_ns += nanos_u64(t_phase.elapsed().as_nanos());
                t_phase = std::time::Instant::now();
            }
            // Interior timestamp 6 — end of opaque segment 3 (Pass 3), start of
            // transparent segment 4. The trailing (t1−t6) window then covers
            // Pass 4 transparent + any editor axis overlay + pass teardown, which
            // rolls into the transparent bucket. No-op unless INSIDE_PASSES.
            #[cfg(feature = "metrics")]
            if let Some(ts) = &self.gpu_timestamps {
                ts.write_interior(&mut pass, 6);
            }

            // Pass 4 — transparent instanced geometry (skipped in wireframe mode)
            if !use_wireframe {
                let identity_bg = self.identity_mesh_bg.as_ref().unwrap();
                for batch in &snapshot.instanced_batches {
                    if !batch.material.is_transparent() {
                        continue;
                    }
                    if let Some(e) = self.instance_batch_cache.get(&batch.prototype_key) {
                        if e.instance_count == 0 {
                            continue;
                        }
                        let colored_glass = batch.material.has_colored_transmission() && !e.udim;
                        let pipe = if e.udim {
                            self.pipeline
                                .udim_instanced
                                .select(e.double_sided, false, true)
                        } else if colored_glass {
                            self.pipeline
                                .instancing
                                .select_colored(e.double_sided, false)
                        } else {
                            self.pipeline.instancing.select(e.double_sided, false, true)
                        };
                        bound.set_pipeline(&mut pass, pipe);
                        if colored_glass {
                            pass.set_blend_constant(batch.material.glass_blend_constant());
                            #[cfg(feature = "metrics")]
                            {
                                colored_glass_draws += 1;
                            }
                        }
                        bound.set_bind_group(&mut pass, 2, identity_bg);
                        bound.set_bind_group(&mut pass, 3, &e.mat_bg);
                        bound.set_vertex_buffer(&mut pass, 0, &e.vbuf);
                        bound.set_vertex_buffer(&mut pass, 1, &e.instance_buf);
                        bound.set_index_buffer(&mut pass, &e.ibuf);
                        pass.draw_indexed(0..e.index_count, 0, 0..e.instance_count);
                        #[cfg(feature = "metrics")]
                        {
                            draw_calls += 1;
                        }
                    }
                }
            }
            #[cfg(feature = "metrics")]
            {
                // Pass 4 transparent instanced recording rolls into the transparent bucket.
                encode_transparent_ns += nanos_u64(t_phase.elapsed().as_nanos());
                t_phase = std::time::Instant::now();
            }

            // Axis lines — depth-tested within the MSAA HDR pass.
            #[cfg(feature = "editor-overlay")]
            if let Some(ref data) = axis_gpu {
                bound.set_pipeline(&mut pass, &self.axis_pass.pipeline);
                bound.set_bind_group(&mut pass, 0, &data.frame_bg);
                bound.set_vertex_buffer(&mut pass, 0, &data.vbuf);
                pass.draw(0..data.vertex_count, 0..1);
                #[cfg(feature = "metrics")]
                {
                    draw_calls += 1;
                }
            }

            // Widget panels — depth-tested textured quads, recorded onto the
            // open beauty pass (like the skybox) so they share its HDR colour +
            // scene depth. No-op when the snapshot carries no widget panels.
            self.widget_renderer.draw(&mut pass, &widget_frame);
            // Close the fragment-invocation pipeline-statistics query before the
            // PBR pass ends (must be ended inside the pass). No-op when absent.
            #[cfg(feature = "metrics")]
            if self.gpu_pipeline_stats.is_some() {
                pass.end_pipeline_statistics_query();
            }
        }
        bound.reset_bindings(); // Tonemap begins a fresh RenderPass — no state carries over.
        #[cfg(feature = "metrics")]
        {
            // Editor axis overlay + PBR pass teardown roll into the opaque bucket.
            encode_opaque_ns += nanos_u64(t_phase.elapsed().as_nanos());
            t_phase = std::time::Instant::now();
        }
        // ── Tonemapping pass ───────────────────────────────────────────────
        // Reads resolved HDR texture, applies ACES, writes LDR Rgba8UnormSrgb.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Tonemap Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: tonemap_ts,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            bound.set_pipeline(&mut pass, &self.tonemap_pipeline.pipeline);
            bound.set_bind_group(&mut pass, 0, &target.tonemap_bind_group);
            // Fullscreen triangle — 3 vertices, no vertex buffer.
            pass.draw(0..3, 0..1);
            #[cfg(feature = "metrics")]
            {
                draw_calls += 1;
            }
        }
        #[cfg(feature = "metrics")]
        {
            // Tonemap (post-process) pass recording. `t_phase` is not reset here —
            // `finish`/`submit` are timed with their own dedicated timers below.
            encode_tonemap_ns = nanos_u64(t_phase.elapsed().as_nanos());
        }
        // Resolve the pass-boundary timestamp queries into a readable buffer while
        // the encoder is still open (only when the adapter supports timestamps).
        #[cfg(feature = "metrics")]
        let readback_slot = if self.gpu_timestamps.is_some() || self.gpu_pipeline_stats.is_some() { self.gpu_readbacks.available() } else { None };
        #[cfg(feature = "metrics")]
        if let (Some(ts), Some(slot)) = (&self.gpu_timestamps, readback_slot) {
            encoder.resolve_query_set(&ts.query_set, 0..GpuTimestamps::COUNT, &ts.resolve_buf, 0);
            encoder.copy_buffer_to_buffer(
                &ts.resolve_buf,
                0,
                self.gpu_readbacks.buffer(slot),
                0,
                u64::from(GpuTimestamps::COUNT) * GpuTimestamps::SLOT_BYTES,
            );
        }
        // Resolve the fragment-invocation pipeline-statistics query (own buffer —
        // resolve_query_set needs a 256-aligned dest offset, so it cannot share
        // the timestamp resolve buffer).
        #[cfg(feature = "metrics")]
        if let (Some(ps), Some(slot)) = (&self.gpu_pipeline_stats, readback_slot) {
            encoder.resolve_query_set(&ps.query_set, 0..1, &ps.resolve_buf, 0);
            encoder.copy_buffer_to_buffer(
                &ps.resolve_buf,
                0,
                self.gpu_readbacks.buffer(slot),
                72,
                GpuPipelineStats::SIZE,
            );
        }
        // Split finish (backend command-stream translation) from submit (driver
        // hand-off) so the two costs can be told apart — see
        // docs/draw-submission-analysis.md §4.1.
        #[cfg(feature = "metrics")]
        let t_finish = std::time::Instant::now();
        let cb = encoder.finish();
        #[cfg(feature = "metrics")]
        {
            finish_ns = nanos_u64(t_finish.elapsed().as_nanos());
        }
        #[cfg(feature = "metrics")]
        let t_submit = std::time::Instant::now();
        self.queue.submit([cb]);
        #[cfg(feature = "metrics")]
        {
            submit_ns = nanos_u64(t_submit.elapsed().as_nanos());
        }
        #[cfg(feature = "metrics")]
        {
            // Fold the dedup tracker's recorded-command tallies into the frame
            // counters. `bound` counted every non-redundant `set_*`; each draw is
            // one further command that is never deduplicated. The gap between
            // `draw_calls` and `pipeline_switches` is now the eliminated
            // redundant-pipeline-bind headroom rather than latent work.
            commands_encoded = bound.commands_encoded + draw_calls;
            pipeline_switches = bound.pipeline_switches;
            bind_group_sets = bound.bind_group_sets;
        }
        #[cfg(feature = "metrics")]
        {
            // Delayed query collection never waits for this submission.
            let total_ns = nanos_u64(t_render_total.elapsed().as_nanos());
            let gpu_passes_ns = nanos_u64(t_gpu_passes.elapsed().as_nanos());
            let gpu = self.read_gpu_timestamps(readback_slot);
            let gpu_wait_ns = 0;
            // `residual_ns` = `total_ns` − Σ(named CPU phases). Saturating so a small
            // measurement skew (timers nested vs. sequential) can never underflow to a
            // huge bogus value; a genuinely large residual flags an unmeasured hot span.
            let named_sum = camera_fit_ns
                .saturating_add(camera_lights_ns)
                .saturating_add(materialx_prep_ns)
                .saturating_add(merge_plan_ns)
                .saturating_add(mesh_classify_ns)
                .saturating_add(transform_update_ns)
                .saturating_add(material_rebuild_ns)
                .saturating_add(cache_miss_build_ns)
                .saturating_add(evict_ns)
                .saturating_add(instance_cache_ns)
                .saturating_add(draw_list_build_ns)
                .saturating_add(gpu_passes_ns);
            let residual_ns = total_ns.saturating_sub(named_sum);
            self.last_frame_timings = crate::metrics::RenderFrameTimings {
                viewport: [target.width, target.height],
                total_ns,
                camera_fit_ns,
                camera_lights_ns,
                materialx_prep_ns,
                merge_plan_ns,
                mesh_classify_ns,
                transform_update_ns,
                material_rebuild_ns,
                cache_miss_build_ns,
                evict_ns,
                instance_cache_ns,
                draw_list_build_ns,
                gpu_passes_ns,
                pass_setup_ns,
                encode_opaque_ns,
                encode_transparent_ns,
                encode_tonemap_ns,
                finish_ns,
                submit_ns,
                draw_calls,
                commands_encoded,
                pipeline_switches,
                bind_group_sets,
                eye: None,
                gpu_pbr_ns: gpu.pbr_ns,
                gpu_sample_id: gpu.sample_id,
                gpu_tonemap_ns: gpu.tonemap_ns,
                gpu_shadow_ns: gpu.shadow_ns,
                gpu_pbr_opaque_ns: gpu.pbr_opaque_ns,
                gpu_pbr_transparent_ns: gpu.pbr_transparent_ns,
                fragment_shader_invocations: gpu.fragment_shader_invocations,
                mesh_count: metrics_mesh_count,
                instance_count: snapshot
                    .instanced_batches
                    .iter()
                    .map(|b| u32_sat(b.instance_transforms.len()))
                    .sum(),
                merged_batch_count: u32_sat(self.merged_batch_cache.len()),
                merged_mesh_count: self
                    .merged_batch_cache
                    .iter()
                    .map(|mb| mb.source_count)
                    .sum(),
                merged_batch_rebakes,
                cache_hits: metrics_cache_hits,
                cache_misses: metrics_cache_misses,
                cache_transform_updates: metrics_cache_xform,
                cache_material_rebuilds: metrics_cache_mat,
                evictions: metrics_evictions,
                residual_ns,
                gpu_wait_ns,
                materialx_pipeline_compiles,
                scene_aabb_recomputes,
                colored_glass_draws,
            };
        }
        self.last_geometry_error = None;
        Ok(())
    }

    /// Like [`Self::render`] but additionally draws a selection overlay on top of
    /// the LDR output if `selection_obb` is `Some(corners)`.
    ///
    /// `corners` must be the 8 OBB corners in render world space, ordered by
    /// bit-index: bit 0 = X axis direction, bit 1 = Y axis direction, bit 2 = Z axis.
    ///
    /// Axis lines are drawn depth-tested inside [`Self::render`]'s PBR pass.
    /// This method only handles the post-tonemap, no-depth selection overlay.
    ///
    /// `extra_lines` are drawn in render world space with the same no-depth
    /// overlay pass — used by the editor for skeleton bone glyphs.
    pub fn render_with_overlay(
        &mut self,
        target: &FrameTarget,
        snapshot: &RenderWorldSnapshot,
        selection_obb: Option<[[f32; 3]; 8]>,
        extra_lines: &[OverlayLine],
        wireframe: bool,
    ) {
        if let Err(error) = self.try_render(target, snapshot, wireframe) {
            self.report_render_error(&error);
            return;
        }

        // Behavior-driven tint + silhouette outline, composited over the LDR
        // colour (after tonemapping, before the overlay line pass). Inert when
        // the snapshot carries no highlights.
        self.highlight.encode(
            &self.device,
            &self.queue,
            snapshot,
            &target.color_view,
            target.width,
            target.height,
        );

        // Screen-space HUD overlay (camera-HUD overlay), composited over the LDR
        // colour after the highlight passes. Loads each element's image through
        // the shared texture cache at its runtime size. Inert when the snapshot
        // carries no HUD elements.
        self.hud.encode(
            &self.device,
            &self.queue,
            &mut self.texture_cache,
            snapshot,
            &target.color_view,
            target.width,
            target.height,
        );

        if selection_obb.is_none() && extra_lines.is_empty() {
            return;
        }

        let aspect = target.width.as_f32() / target.height.as_f32();
        // `render()` above already resolved (and cached) the synthetic camera for this
        // snapshot, so on the None branch this is a cache hit — no second AABB pass.
        let cam = match snapshot.camera.clone() {
            Some(c) => c,
            None => self.resolve_synthetic_camera(snapshot, aspect).0,
        };
        let vp = cam.projection * cam.view;

        let mut lines: Vec<OverlayLine> = Vec::with_capacity(24 + extra_lines.len());
        if let Some(corners) = selection_obb {
            // Orange-yellow Blender-style selection colour.
            push_obb_corner_ticks(&mut lines, &corners, [1.0, 0.55, 0.0, 1.0], 0.25);
        }
        lines.extend_from_slice(extra_lines);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Overlay Encoder"),
            });
        self.overlay_pass
            .render(&self.device, &mut encoder, &target.color_view, vp, &lines);
        self.queue.submit([encoder.finish()]);
    }

    /// Blits `src` (any 2-D color texture view) into `dst`'s LDR color texture,
    /// scaling with a linear filter to fill the destination.
    ///
    /// Used during an `OpenXR` preview to mirror one already-rendered eye image
    /// into the desktop preview target, so the scene is rendered twice (one per
    /// eye) instead of a third time for the mirror. `src` and `dst.color_texture`
    /// are both `COLOR_FORMAT` (sRGB), so the copy is a pixel-exact passthrough.
    pub fn blit_into(&self, src: &wgpu::TextureView, dst: &FrameTarget) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Blit BG"),
            layout: &self.blit_pipeline.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.blit_pipeline.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Blit Encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Blit Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blit_pipeline.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Resolves a 4× MSAA depth view into a single-sample depth view via a
    /// fullscreen pass (see [`build_depth_resolve_pipeline`]).
    ///
    /// Records into the caller's `encoder` (the XR path combines this with the
    /// subsequent depth-swapchain copy in one submit). `src_msaa_depth` must come
    /// from an XR eye target ([`Self::create_xr_eye_target`], which stores depth
    /// and makes it sampleable); `dst_single_sample_depth` must be a
    /// single-sample [`DEPTH_FORMAT`] render target.
    pub fn resolve_depth(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src_msaa_depth: &wgpu::TextureView,
        dst_single_sample_depth: &wgpu::TextureView,
    ) {
        self.resolve_depth_with(
            &self.depth_resolve_pipeline,
            1.0,
            encoder,
            src_msaa_depth,
            dst_single_sample_depth,
        );
    }

    /// Reverse-Z variant of [`Self::resolve_depth`] for the visionOS present
    /// path. `CompositorServices` drawable depth textures must be reverse-Z (1.0 at
    /// the near plane, 0.0 at the far plane) for the device compositor's
    /// depth-aware pose reprojection; the renderer draws forward-Z, so this
    /// resolve flips the value (`1.0 - d`, via
    /// [`build_depth_resolve_reverse_z_pipeline`]) and clears to the reverse-Z far
    /// value (0.0). Mismatching this convention is seen on device as "echo"
    /// ghosting while moving and a scene that swims while standing still.
    pub fn resolve_depth_reverse_z(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src_msaa_depth: &wgpu::TextureView,
        dst_single_sample_depth: &wgpu::TextureView,
    ) {
        self.resolve_depth_with(
            &self.depth_resolve_reverse_pipeline,
            0.0,
            encoder,
            src_msaa_depth,
            dst_single_sample_depth,
        );
    }

    /// Shared body of [`Self::resolve_depth`] / [`Self::resolve_depth_reverse_z`]:
    /// a fullscreen depth-only pass with the given `pipeline` and `clear` value.
    /// (The fullscreen triangle writes every pixel, so `clear` is cosmetic, but it
    /// matches the pass's depth convention.)
    fn resolve_depth_with(
        &self,
        pipeline: &DepthResolvePipeline,
        clear: f32,
        encoder: &mut wgpu::CommandEncoder,
        src_msaa_depth: &wgpu::TextureView,
        dst_single_sample_depth: &wgpu::TextureView,
    ) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Depth Resolve BG"),
            layout: &pipeline.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(src_msaa_depth),
            }],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Depth Resolve Pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: dst_single_sample_depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Copies the rendered colour texture to CPU memory (tightly-packed RGBA bytes).
    pub fn read_pixels(&self, target: &FrameTarget) -> Result<Vec<u8>, RenderError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Readback Encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.color_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_bpr),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: target.width,
                height: target.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let buf_slice = target.readback_buffer.slice(..);
        let (tx, rx) = mpsc::sync_channel(1);
        buf_slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .map_err(|e| RenderError::MapFailed(e.to_string()))?
            .map_err(|e| RenderError::MapFailed(e.to_string()))?;

        let padded = buf_slice.get_mapped_range();
        let row_bytes = target.width as usize * 4;
        let stride = target.padded_bpr as usize;
        let mut out = Vec::with_capacity(row_bytes * target.height as usize);
        for row in 0..target.height as usize {
            out.extend_from_slice(&padded[row * stride..row * stride + row_bytes]);
        }
        drop(padded);
        target.readback_buffer.unmap();
        Ok(out)
    }

    /// Collect completed older GPU queries, then schedule this submission's map.
    ///
    /// Returns an all-zero [`GpuFrameReadback`] when the adapter has no
    /// `TIMESTAMP_QUERY` support or the readback map fails — GPU timing degrades to
    /// zero, never panics. The opaque/transparent split is `0` unless the adapter
    /// also enabled `TIMESTAMP_QUERY_INSIDE_PASSES`; `fragment_shader_invocations`
    /// is `0` unless `PIPELINE_STATISTICS_QUERY` was available (all Metal). This
    /// uses a non-blocking device poll; a full ring drops measurement work.
    #[cfg(feature = "metrics")]
    fn read_gpu_timestamps(&mut self, slot: Option<usize>) -> GpuFrameReadback {
        let mut out = GpuFrameReadback::default();
        let sample = self.gpu_readbacks.collect(&self.device, self.metrics_view);
        if let Some(slot) = slot { self.gpu_readbacks.schedule(slot, self.shadow_pass_ran, self.metrics_view); }
        let Some(sample) = sample else { return out; };
        if self.gpu_pipeline_stats.is_some() { out.fragment_shader_invocations = sample.ticks[9]; }

        let Some(ts) = &self.gpu_timestamps else {
            return out;
        };
        out.sample_id = sample.frame;
        let read_tick = |i: usize| sample.ticks[i];
        let period = f64::from(ts.period_ns);
        let to_ns = |begin: u64, end: u64| -> u64 {
            let ticks = end.saturating_sub(begin).as_f64();
            // f64→u64: tick spans are small, non-negative, and far below u64::MAX.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let ns = (ticks * period) as u64;
            ns
        };
        // Whole-pass spans (pass-boundary slots) — always valid.
        out.pbr_ns = to_ns(read_tick(0), read_tick(1));
        out.tonemap_ns = to_ns(read_tick(2), read_tick(3));
        // Shadow depth pre-pass span (slots 7→8). Only meaningful when the pre-pass
        // ran this frame; when it was skipped the slots keep the previous frame's
        // ticks. The ShadowCache can skip every slot on a static frame, in
        // which case the pass never executed and the stale span must read a hard 0 —
        // `shadow_pass_ran` records whether any slot's depth pass ran this frame.
        out.shadow_ns = if sample.shadow {
            to_ns(read_tick(7), read_tick(8))
        } else {
            0
        };
        // Interior opaque/transparent split — only when INSIDE_PASSES wrote slot 4.
        // GPU order: [opaque: Pass 1 + 1b + 3]→t4→[skybox + transparent: Pass 2 + 4
        // + overlay]→t1. A single boundary now that all opaque precedes all transparent.
        if ts.inside_passes {
            let (t0, t1) = (read_tick(0), read_tick(1));
            let t4 = read_tick(4);
            out.pbr_opaque_ns = to_ns(t0, t4);
            out.pbr_transparent_ns = to_ns(t4, t1);
        }
        out
    }
}

// ── Frame target ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct FrameTarget {
    /// LDR `Rgba8UnormSrgb` output — tonemapper writes here; CPU reads back from here.
    pub color_texture: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    /// MSAA depth (same sample count as the beauty pass; `sample_count == 1` means
    /// an ordinary single-sample depth target).
    pub depth_texture: wgpu::Texture,
    pub depth_view: wgpu::TextureView,
    /// MSAA HDR color — PBR geometry renders here and resolves into
    /// `hdr_color_texture`. `None` when `sample_count == 1` (no MSAA): the PBR pass
    /// then renders directly into `hdr_color_texture`.
    pub msaa_color_texture: Option<wgpu::Texture>,
    pub msaa_color_view: Option<wgpu::TextureView>,
    /// Single-sample HDR resolve target — MSAA resolves here; tonemapper reads from here.
    pub hdr_color_texture: wgpu::Texture,
    pub hdr_color_view: wgpu::TextureView,
    /// Pre-built bind group for the tonemapping pass (sampler + HDR texture).
    pub tonemap_bind_group: wgpu::BindGroup,
    readback_buffer: wgpu::Buffer,
    pub width: u32,
    pub height: u32,
    padded_bpr: u32,
    /// When `true` the PBR pass stores MSAA depth (instead of discarding it) and
    /// the depth texture is sampleable, so [`Renderer::resolve_depth`] can run.
    /// Set only for XR eye targets ([`Renderer::create_xr_eye_target`]).
    pub resolve_depth: bool,
    /// Background clear colour for the PBR pass (shows through wherever no
    /// geometry or skybox covers a pixel). The desktop editor uses an opaque grey;
    /// XR eye targets clear to **black** so empty / infinitely-far regions read as
    /// void rather than a solid grey surface at the far plane — a grey fill at the
    /// far plane tears into blocky disocclusion artifacts under the device
    /// compositor's depth reprojection.
    pub clear_color: wgpu::Color,
    /// Effective MSAA sample count ({1, 2, 4}) this target was allocated for —
    /// equals the owning [`Renderer`]'s `sample_count`. Drives the PBR pass's
    /// resolve-vs-direct attachment choice.
    pub sample_count: u32,
}

// ── Mesh GPU cache entry builder (free function for disjoint borrows) ─────────

/// Allocates all GPU resources for `inst` and returns a [`CachedMeshGpu`].
///
/// Mesh and index buffers are static (VERTEX / INDEX).  The mesh-transform
/// and skin joint-matrix uniform buffers are `UNIFORM | COPY_DST` so they can
/// be updated in-place via `queue.write_buffer` without recreating bind groups.
fn build_mesh_gpu_cache_entry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &crate::pipeline::PbrPipeline,
    sampler: &wgpu::Sampler,
    texture_cache: &mut TextureCache,
    inst: &crate::scene_extract::RenderMeshInstance,
) -> CachedMeshGpu {
    use crate::vertex::interleave;
    use bytemuck::cast_slice;

    let verts = interleave(
        &inst.mesh.positions,
        &inst.mesh.normals,
        &inst.mesh.texcoords,
        &inst.mesh.vertex_colors,
        &inst.mesh.tangents,
    );
    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: cast_slice(&inst.mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    let mesh_u = crate::pipeline::MeshUniforms::from_matrix(inst.world_matrix);
    let mesh_ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&mesh_u),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let mesh_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.mesh_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: mesh_ubuf.as_entire_binding(),
        }],
    });

    // UDIM applies to static (non-skinned) meshes only; skinned UDIM
    // falls back to the plain single-tile path.
    let udim = inst.material.is_udim() && inst.mesh.skin.is_none();
    let mat_bg = if udim {
        build_material_udim_bg(device, pipeline, texture_cache, &inst.material)
    } else {
        build_material_bg(
            device,
            queue,
            &pipeline.material_bgl,
            sampler,
            texture_cache,
            &inst.material,
        )
    };

    let (skin_vbuf, skin_ubuf, skin_bg) = if let Some(skin) = inst.mesh.skin.as_ref() {
        let sv_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Skin Vertex Buffer"),
            contents: cast_slice(&skin.influences),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let matrix_bytes: Vec<u8> = skin
            .joint_matrices
            .iter()
            .flat_map(|m| bytemuck::bytes_of(m).iter().copied())
            .collect();
        const JOINT_PALETTE_BYTES: usize = 256 * 16 * 4;
        let mut padded = matrix_bytes;
        padded.resize(JOINT_PALETTE_BYTES, 0);
        let skin_ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Skin Joint Matrices"),
            contents: &padded,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Skin BG"),
            layout: &pipeline.skin_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: skin_ubuf.as_entire_binding(),
            }],
        });
        (Some(sv_buf), Some(skin_ubuf), Some(bg))
    } else {
        (None, None, None)
    };

    CachedMeshGpu {
        vbuf,
        ibuf,
        index_count: u32_sat(inst.mesh.indices.len()),
        mesh_ubuf,
        mesh_bg,
        last_world_matrix: inst.world_matrix,
        mat_bg,
        mat_fingerprint: inst.material.fingerprint,
        last_mesh_serial: inst.mesh.serial,
        skin_vbuf,
        skin_ubuf,
        skin_bg,
        udim,
        #[cfg(feature = "materialx-wgsl")]
        wgsl_key: inst.material.wgsl_source.as_deref().map(hash_wgsl),
    }
}

// ── Material bind group (free function for disjoint field borrows) ─────────────

/// Builds a material bind group, loading textures on demand.
///
/// This is a free function (not a method) so the borrow checker can see that
/// `device`, `pipeline.material_bgl`, and `sampler` are shared borrows while
/// `texture_cache` is an exclusive borrow — all from disjoint struct fields.
fn build_material_bg(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    cache: &mut TextureCache,
    mat: &RenderMaterialDesc,
) -> wgpu::BindGroup {
    let mut get = |path: Option<&String>, kind: TextureKind| {
        match path {
            // A `<UDIM>` template names a tile set, not a file on disk. This
            // plain single-tile builder serves the deferred UDIM cases
            // (instanced / merged / skinned geometry — see the tiled-array path
            // in `build_material_udim_bg`); binding the null texture here yields
            // the material's scalar fallback instead of spamming per-frame
            // "texture load failed" on the literal token path.
            Some(p) if !p.is_empty() && !crate::udim::is_udim_template(p) => {
                cache.get_or_load(p, kind, device, queue)
            }
            _ => std::sync::Arc::clone(cache.null(kind)),
        }
    };

    let diffuse = get(mat.diffuse_texture.as_ref(), TextureKind::DiffuseSrgb);
    let normal = get(mat.normal_texture.as_ref(), TextureKind::Normal);
    let metallic = get(mat.metallic_texture.as_ref(), TextureKind::Data);
    let roughness = get(mat.roughness_texture.as_ref(), TextureKind::Data);
    let occlusion = get(mat.occlusion_texture.as_ref(), TextureKind::Data);
    let emissive = get(mat.emissive_texture.as_ref(), TextureKind::EmissiveSrgb);

    let mat_u = MaterialUniforms::from_material(mat);
    let mat_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Material Uniforms"),
        contents: bytemuck::bytes_of(&mat_u),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Material BG"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&diffuse.view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&normal.view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&metallic.view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&roughness.view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&occlusion.view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(&emissive.view),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: mat_buf.as_entire_binding(),
            },
        ],
    })
}

/// Builds a material bind group against `pipeline.material_udim_bgl` for a
/// `<UDIM>` material (the tiled-array pipeline variant).
///
/// Each of the six PBR channels binds a `D2Array` texture view: the prefetched
/// tile array for channels whose slot is a `<UDIM>` template, or the shared
/// 1×1×1 null array for plain / absent channels. Binding 8 is the [`UdimTable`]
/// uniform carrying the per-channel tile→layer maps (read back from each
/// array's `tile_to_layer`) plus the channel mask, so the shader knows which
/// channels sample the array and where each tile lives.
///
/// Requires the material's arrays to have been prefetched via
/// [`TextureCache::prefetch_udim_parallel`]; any unprefetched channel resolves
/// to the null array (all-sentinel layers → scalar fallback in the shader).
fn build_material_udim_bg(
    device: &wgpu::Device,
    pipeline: &crate::pipeline::PbrPipeline,
    texture_cache: &TextureCache,
    mat: &RenderMaterialDesc,
) -> wgpu::BindGroup {
    use crate::pipeline::UdimTable;
    use crate::texture_cache::{GpuUdimTexture, UDIM_LAYER_SENTINEL};

    // (channel bit, texture path, kind) in canonical channel order
    // (diffuse=0 … emissive=5), matching `RenderMaterialDesc::udim_channels`.
    let channels: [(u32, &Option<String>, TextureKind); 6] = [
        (0, &mat.diffuse_texture, TextureKind::DiffuseSrgb),
        (1, &mat.normal_texture, TextureKind::Normal),
        (2, &mat.metallic_texture, TextureKind::Data),
        (3, &mat.roughness_texture, TextureKind::Data),
        (4, &mat.occlusion_texture, TextureKind::Data),
        (5, &mat.emissive_texture, TextureKind::EmissiveSrgb),
    ];

    let mut table = UdimTable::empty();
    table.channel_mask = u32::from(mat.udim_channels());

    // Resolve one array per channel and fold its tile→layer map into the table.
    // The Arcs are held in `arrays` so the borrowed `.view`s outlive the
    // bind-group creation below.
    let mut arrays: Vec<std::sync::Arc<GpuUdimTexture>> = Vec::with_capacity(6);
    for (bit, path, kind) in channels {
        let arr = match path {
            Some(p) if crate::udim::is_udim_template(p) => {
                let a = std::sync::Arc::clone(texture_cache.udim_get(p, kind));
                for tile in 1001u32..=1100 {
                    let layer = a.tile_to_layer[(tile - 1001) as usize];
                    if layer != UDIM_LAYER_SENTINEL {
                        table.set(bit, tile, layer);
                    }
                }
                a
            }
            _ => std::sync::Arc::clone(texture_cache.udim_null()),
        };
        arrays.push(arr);
    }

    let mat_u = MaterialUniforms::from_material(mat);
    let mat_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Material Uniforms (UDIM)"),
        contents: bytemuck::bytes_of(&mat_u),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let udim_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("UDIM Table"),
        contents: bytemuck::bytes_of(&table),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Material BG (UDIM)"),
        layout: &pipeline.material_udim_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Sampler(&pipeline.udim_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&arrays[0].view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&arrays[1].view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&arrays[2].view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&arrays[3].view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&arrays[4].view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(&arrays[5].view),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: mat_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: udim_buf.as_entire_binding(),
            },
        ],
    })
}

// ── Light assembly ────────────────────────────────────────────────────────────

/// Build the `DomeLight` environment sampler (repeat-U / clamp-V, linear + mip) and
/// a neutral 1×1 `Rgba16Float` fallback texture view. Shared by both constructors.
fn build_env_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Sampler, wgpu::TextureView) {
    let env_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Env Sampler"),
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Env Fallback 1x1"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // A single mid-grey texel; never sampled when `has_texture = 0`, but valid data
    // keeps validation layers quiet. Packed as raw f16 bit patterns (u16 is Pod).
    let grey = half::f16::from_f32(0.5).to_bits();
    let one = half::f16::from_f32(1.0).to_bits();
    let texel: [u16; 4] = [grey, grey, grey, one];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&texel),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (env_sampler, view)
}

fn build_lights_uniforms(lights: &[crate::scene_extract::RenderLightDesc]) -> LightsUniforms {
    let count = u32_sat(lights.len().min(MAX_LIGHTS));
    let zero_light = LightGpu {
        kind: LIGHT_DISTANT,
        _pad: [0; 3],
        dir_or_pos: [0.0; 4],
        color_power: [0.0; 4],
        size: [0.0; 4],
    };
    let mut gpu_lights = [zero_light; MAX_LIGHTS];

    for (i, light) in lights.iter().take(MAX_LIGHTS).enumerate() {
        let (kind, dir_or_pos, mut size) = match light.kind {
            RenderLightKind::Distant { angle_radians } => {
                let dir = light.world_matrix.col(2).truncate().normalize_or_zero();
                (
                    LIGHT_DISTANT,
                    [dir.x, dir.y, dir.z, 0.0],
                    [angle_radians, 0.0, 0.0, 0.0],
                )
            }
            RenderLightKind::Sphere { radius } => {
                let pos = light.world_matrix.col(3).truncate();
                (
                    LIGHT_SPHERE,
                    [pos.x, pos.y, pos.z, 0.0],
                    [radius, 0.0, 0.0, 0.0],
                )
            }
            RenderLightKind::Rect { width, height } => {
                let pos = light.world_matrix.col(3).truncate();
                (
                    LIGHT_RECT,
                    [pos.x, pos.y, pos.z, 0.0],
                    [width, height, 0.0, 0.0],
                )
            }
            RenderLightKind::Disk { radius } => {
                let pos = light.world_matrix.col(3).truncate();
                (
                    LIGHT_DISK,
                    [pos.x, pos.y, pos.z, 0.0],
                    [radius, 0.0, 0.0, 0.0],
                )
            }
            // Cylinder lights reuse the sphere point-lobe for shading (no dedicated
            // WGSL cylinder lobe); the emitter length is reflected only through
            // `power` (see `light_point_intensity`), not the lobe shape.
            RenderLightKind::Cylinder { radius, .. } => {
                let pos = light.world_matrix.col(3).truncate();
                (
                    LIGHT_SPHERE,
                    [pos.x, pos.y, pos.z, 0.0],
                    [radius, 0.0, 0.0, 0.0],
                )
            }
            RenderLightKind::Dome => (LIGHT_DOME, [0.0; 4], [0.0; 4]),
            RenderLightKind::DomeProcedural => (LIGHT_DOME_PROC, [0.0; 4], [0.0; 4]),
        };
        // size.zw carry the UsdLuxLightAPI diffuse/specular multipliers for every
        // light kind; the shared shader accumulation applies them per lobe.
        size[2] = light.diffuse_contribution;
        size[3] = light.specular_contribution;
        gpu_lights[i] = LightGpu {
            kind,
            _pad: [0; 3],
            dir_or_pos,
            color_power: [light.color[0], light.color[1], light.color[2], light.power],
            size,
        };
    }

    LightsUniforms {
        lights: gpu_lights,
        count,
        _pad: [0; 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "metrics")]
    #[test]
    fn gpu_metrics_are_delayed_and_never_wait_for_this_render() {
        let mut renderer = Renderer::new().unwrap();
        let target = renderer.create_frame_target(16, 16);
        let snapshot = mesh_snapshot(glam::Vec3::ZERO, 1, None);
        renderer.render(&target, &snapshot, false);
        assert_eq!(renderer.last_frame_timings().gpu_sample_id, 0);
        assert_eq!(renderer.last_frame_timings().gpu_wait_ns, 0);
        if renderer.gpu_timestamps.is_some() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while renderer.last_frame_timings().gpu_sample_id == 0 {
                renderer.render(&target, &snapshot, false);
                assert_eq!(renderer.last_frame_timings().gpu_wait_ns, 0);
                assert!(std::time::Instant::now() < deadline);
            }
        }
    }

    fn constrained_renderer() -> Renderer {
        let (device, queue) = pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap();
            adapter.request_device(&wgpu::DeviceDescriptor {
                required_limits: wgpu::Limits { max_buffer_size: 1024 * 1024, ..Default::default() },
                ..Default::default()
            }).await.unwrap()
        });
        Renderer::from_device(device, queue, TextureDecodeConfig::default())
    }

    fn large_mesh_snapshot(vertices: usize, count: usize) -> RenderWorldSnapshot {
        let mut snapshot = mesh_snapshot(glam::Vec3::ZERO, 1, None);
        snapshot.serial = 1;
        snapshot.instance_serial = 1;
        snapshot.camera = Some(eye_camera_desc(0.0));
        let mesh = std::sync::Arc::make_mut(&mut snapshot.meshes[0].mesh);
        mesh.positions.resize(vertices, [0.0; 3]);
        mesh.normals.resize(vertices, [0.0, 1.0, 0.0]);
        mesh.texcoords.resize(vertices, [0.0; 2]);
        let template = snapshot.meshes[0].clone();
        snapshot.meshes = (0..count).map(|i| {
            let mut inst = template.clone();
            inst.prim_path = format!("/World/Mesh{i}");
            inst.world_matrix = glam::Mat4::from_translation(glam::Vec3::new(i as f32, 0.0, 0.0));
            inst
        }).collect();
        snapshot
    }

    #[test]
    fn merge_limits_two_meshes_under_one_mib_render_without_validation_error() {
        let mut renderer = constrained_renderer();
        let snapshot = large_mesh_snapshot(12_288, 2); // 786,432 bytes each, 1,572,864 merged.
        let target = renderer.create_frame_target(32, 32);
        renderer.set_draw_merge_enabled(false);
        renderer.try_render(&target, &snapshot, false).unwrap();
        let unmerged = renderer.read_pixels(&target).unwrap();
        renderer.set_draw_merge_enabled(true);
        renderer.try_render(&target, &snapshot, false).unwrap();
        assert_eq!(renderer.read_pixels(&target).unwrap(), unmerged);
        for batch in &renderer.merged_batch_cache {
            assert!(batch.vbuf.size() <= 1024 * 1024);
            assert!(batch.ibuf.size() <= 1024 * 1024);
        }
    }

    #[test]
    fn merge_limits_split_batches_rebake_only_moved_chunk_and_honor_configuration() {
        let mut renderer = constrained_renderer();
        let mut snapshot = large_mesh_snapshot(8192, 5); // Two fit exactly; fifth stays unmerged.
        let target = renderer.create_frame_target(32, 32);
        renderer.set_draw_merge_enabled(false);
        renderer.try_render(&target, &snapshot, false).unwrap();
        let reference = renderer.read_pixels(&target).unwrap();
        renderer.set_draw_merge_enabled(true);
        renderer.try_render(&target, &snapshot, false).unwrap();
        assert_eq!(renderer.read_pixels(&target).unwrap(), reference);
        assert_eq!(renderer.merged_batch_cache.len(), 2);
        let buffers: Vec<_> = renderer.merged_batch_cache.iter().map(|b| b.vbuf.clone()).collect();
        assert!(buffers.iter().all(|buffer| buffer.size() == 1024 * 1024));
        renderer.try_render(&target, &snapshot, false).unwrap();
        assert_eq!(renderer.merged_batch_cache[0].vbuf, buffers[0]);
        snapshot.meshes[0].world_matrix.w_axis.x += 0.5;
        renderer.try_render(&target, &snapshot, false).unwrap();
        assert_ne!(renderer.merged_batch_cache[0].vbuf, buffers[0]);
        assert_eq!(renderer.merged_batch_cache[1].vbuf, buffers[1]);
        renderer.set_draw_batch_limits(DrawBatchLimits { max_vertex_bytes: 512 * 1024, max_index_bytes: u64::MAX });
        renderer.try_render(&target, &snapshot, false).unwrap();
        assert!(renderer.merged_batch_cache.is_empty());
        assert_eq!(renderer.mesh_gpu_cache.len(), 5);
        assert_eq!(renderer.draw_batch_limits.max_index_bytes, 1024 * 1024);
    }

    #[test]
    fn merge_limits_index_bound_and_single_instance_sources() {
        let mut snapshot = large_mesh_snapshot(24, 3);
        let inst = snapshot.meshes.pop().unwrap();
        snapshot.instanced_batches.push(crate::scene_extract::RenderInstancedBatch {
            prototype_key: "single".into(), mesh: inst.mesh, instance_transforms: vec![inst.world_matrix],
            instance_prim_paths: vec![inst.prim_path], material: inst.material, purpose: inst.purpose, casts_shadow: true,
        });
        let sources = vec![MergeSource::Mesh(0), MergeSource::Mesh(1), MergeSource::Batch(0)];
        let index_bytes = snapshot.meshes[0].mesh.indices.len() as u64 * 4;
        let limits = DrawBatchLimits { max_vertex_bytes: 1024 * 1024, max_index_bytes: index_bytes * 2 };
        let groups = split_merge_group(sources.clone(), &snapshot, limits).unwrap();
        assert_eq!(groups, vec![sources[..2].to_vec(), sources[2..].to_vec()]);
        let mixed = split_merge_group(vec![sources[0], sources[2]], &snapshot, limits).unwrap();
        let (vertices, indices) = bake_merge_group_geometry(&mixed[0], &snapshot).unwrap();
        assert_eq!(vertices.len(), 48);
        assert_eq!(indices.len() as u64 * 4, index_bytes * 2);
        assert!(indices[snapshot.meshes[0].mesh.indices.len()..].iter().all(|&index| index >= 24 && index < 48));
        let mesh = std::sync::Arc::make_mut(&mut snapshot.meshes[0].mesh);
        mesh.indices[0] = u32::MAX;
        assert!(matches!(bake_merge_group_geometry(&mixed[0], &snapshot), Err(GeometryError::InvalidGeometry)));
    }

    #[test]
    fn merge_limits_oversized_input_is_rejected_before_gpu_work_and_can_recover() {
        let mut renderer = constrained_renderer();
        let target = renderer.create_frame_target(32, 32);
        let valid = large_mesh_snapshot(24, 1);
        renderer.try_render(&target, &valid, false).unwrap();
        let reference = renderer.read_pixels(&target).unwrap();
        let oversized = large_mesh_snapshot(16_385, 1);
        for merge in [false, true] {
            renderer.set_draw_merge_enabled(merge);
            assert!(matches!(renderer.try_render(&target, &oversized, false), Err(RenderError::Geometry(GeometryError::BufferLimit { kind: "vertex", .. }))));
            renderer.render_with_overlay(&target, &oversized, None, &[], false);
            assert_eq!(renderer.read_pixels(&target).unwrap(), reference);
        }
        let mut index_heavy = valid.clone();
        std::sync::Arc::make_mut(&mut index_heavy.meshes[0].mesh).indices.resize(262_145, 0);
        assert!(matches!(renderer.try_render(&target, &index_heavy, false), Err(RenderError::Geometry(GeometryError::BufferLimit { kind: "index", .. }))));
        let inst = valid.meshes[0].clone();
        let mut instances = valid.clone();
        instances.instanced_batches.push(crate::scene_extract::RenderInstancedBatch {
            prototype_key: "many".into(), mesh: inst.mesh, instance_transforms: vec![glam::Mat4::IDENTITY; 16_385],
            instance_prim_paths: Vec::new(), material: inst.material, purpose: inst.purpose, casts_shadow: true,
        });
        assert!(matches!(renderer.try_render(&target, &instances, false), Err(RenderError::Geometry(GeometryError::BufferLimit { kind: "instance", .. }))));
        renderer.try_render(&target, &valid, false).unwrap();
        assert!(renderer.last_geometry_error.is_none());
        assert_eq!(renderer.read_pixels(&target).unwrap(), reference);
    }

    // ── ShadowCache signature helpers — pure logic, no GPU ──────────
    //
    // These validate the two halves of the `ShadowCache` key. They must run on
    // headless CI (no adapter), so they build snapshots directly rather than
    // through `try_renderer`.

    /// A minimal static-mesh snapshot: one cube at `translate`, with an optional
    /// skinned joint palette and an explicit geometry `serial`.
    fn mesh_snapshot(
        translate: glam::Vec3,
        serial: u64,
        joints: Option<Vec<[f32; 16]>>,
    ) -> RenderWorldSnapshot {
        use crate::scene_extract::RenderMeshInstance;
        use idtx_usd::RenderPurpose;
        let mut mesh = crate::mesh::tessellate_cube(1.0);
        mesh.serial = serial;
        mesh.skin = joints.map(|joint_matrices| crate::mesh::SkinData {
            influences: Vec::new(),
            joint_matrices,
        });
        let inst = RenderMeshInstance {
            prim_path: "/World/Cube".to_owned(),
            mesh: std::sync::Arc::new(mesh),
            world_matrix: glam::Mat4::from_translation(translate),
            material: crate::material::fallback_material("/World/mat"),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        };
        RenderWorldSnapshot {
            meshes: vec![inst],
            ..Default::default()
        }
    }

    /// The caster signature is stable for an unchanged scene and shifts whenever a
    /// caster moves or its geometry serial bumps — the two triggers a static
    /// per-mesh caster can have.
    #[test]
    fn casters_signature_stable_and_moves_with_transform_and_serial() {
        let a = mesh_snapshot(glam::Vec3::ZERO, 7, None);
        let b = mesh_snapshot(glam::Vec3::ZERO, 7, None);
        assert_eq!(
            casters_signature(&a),
            casters_signature(&b),
            "identical scenes must match"
        );

        let moved = mesh_snapshot(glam::Vec3::new(0.0, 1.0, 0.0), 7, None);
        assert_ne!(
            casters_signature(&a),
            casters_signature(&moved),
            "a moved caster must invalidate"
        );

        let resynced = mesh_snapshot(glam::Vec3::ZERO, 8, None);
        assert_ne!(
            casters_signature(&a),
            casters_signature(&resynced),
            "a serial bump must invalidate"
        );
    }

    /// The critical correctness point: a skinned caster's pose lives in the joint
    /// palette, which changes every animation tick *without* bumping the geometry
    /// serial. The signature must hash the palette or an animating shadow freezes.
    #[test]
    fn casters_signature_tracks_skinned_pose_without_serial_bump() {
        let rest = mesh_snapshot(glam::Vec3::ZERO, 3, Some(vec![[0.0; 16], [0.0; 16]]));
        let mut posed_palette = vec![[0.0; 16], [0.0; 16]];
        posed_palette[1][12] = 0.5; // translate one joint — same serial, new pose
        let posed = mesh_snapshot(glam::Vec3::ZERO, 3, Some(posed_palette));
        assert_ne!(
            casters_signature(&rest),
            casters_signature(&posed),
            "a new joint palette at the same serial must invalidate the shadow cache"
        );
    }

    /// The light-matrix hash is deterministic and reacts to any matrix change (a
    /// moved light or a grown/shifted scene AABB both re-fit the ortho frustum).
    #[test]
    fn light_matrix_hash_is_deterministic_and_change_sensitive() {
        let m = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(
            light_matrix_hash(&m),
            light_matrix_hash(&m),
            "same matrix → same hash"
        );

        let mut m2 = m;
        m2.w_axis.x += 0.001;
        assert_ne!(
            light_matrix_hash(&m),
            light_matrix_hash(&m2),
            "a perturbed matrix must re-hash"
        );
    }

    /// A minimal perspective eye camera offset laterally by `eye_offset_x` (half
    /// the IPD) — enough to give the two stereo eyes distinct view matrices.
    fn eye_camera_desc(eye_offset_x: f32) -> crate::RenderCameraDesc {
        let eye = glam::Vec3::new(eye_offset_x, 1.6, 3.0);
        crate::RenderCameraDesc {
            prim_path: "xr:eye".to_owned(),
            view: glam::Mat4::from_translation(eye).inverse(),
            projection: glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_2, 1.0, 0.05, 5000.0),
            clip_near: 0.05,
            clip_far: 5000.0,
            fov_horizontal: std::f32::consts::FRAC_PI_2,
            exposure_ev: 0.0,
        }
    }

    /// Stereo invariant: the two eyes of a VR frame share one snapshot and differ
    /// only in `camera` (see `XrGpu::render_eyes`). The `ShadowCache` key the depth
    /// pre-pass computes per slot — `(light_matrix_hash(&slot.view_proj),
    /// casters_signature(snapshot))` — must therefore be identical for both eyes,
    /// so eye 1 is a guaranteed cache hit that never re-renders the shared shadow
    /// atlas. If a future change folds eye/view state into either half of the key,
    /// this fails and flags the regression. Pure logic — runs on headless CI.
    #[test]
    fn shadow_cache_key_is_camera_independent() {
        let mut left = mesh_snapshot(glam::Vec3::ZERO, 7, None);
        left.camera = Some(eye_camera_desc(-0.032)); // left eye (−IPD/2)
        let mut right = mesh_snapshot(glam::Vec3::ZERO, 7, None);
        right.camera = Some(eye_camera_desc(0.032)); // right eye (+IPD/2)

        // Sanity: the two eyes really do carry different cameras.
        assert_ne!(
            left.camera.as_ref().unwrap().view,
            right.camera.as_ref().unwrap().view,
            "test setup must use two distinct eye cameras"
        );

        // Half 1 — the caster epoch must ignore the camera.
        assert_eq!(
            casters_signature(&left),
            casters_signature(&right),
            "caster signature must not depend on the eye camera"
        );

        // Half 2 — the full per-slot key (both halves, exactly as the depth
        // pre-pass builds it) must match across eyes for a representative
        // light-space slot.
        let (view_proj, _near, _far) =
            crate::math::build_view_proj(glam::Vec3::NEG_Y, glam::Vec3::splat(-1.0), glam::Vec3::splat(1.0));
        let cache_key =
            |s: &RenderWorldSnapshot| (light_matrix_hash(&view_proj), casters_signature(s));
        assert_eq!(
            cache_key(&left),
            cache_key(&right),
            "shadow cache key must be identical for both stereo eyes"
        );
    }

    /// Skips (returns `None`) when no wgpu adapter is available (headless CI),
    /// matching the integration-test harness.
    fn try_renderer() -> Option<Renderer> {
        match Renderer::new() {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("Skipping GPU blit test — {e}");
                None
            }
        }
    }

    /// A uniform-colour source blitted into a differently-sized target must
    /// reproduce that colour pixel-for-pixel (sRGB decode-on-read cancels
    /// encode-on-store), validating the fullscreen blit pipeline end to end.
    #[test]
    fn blit_reproduces_source_color() {
        let Some(renderer) = try_renderer() else {
            return;
        };

        // 2×2 solid-red source with COPY_DST so we can seed known bytes.
        const RED: [u8; 4] = [220, 30, 40, 255];
        let (sw, sh) = (2u32, 2u32);
        let src = renderer.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blit-test-src"),
            size: wgpu::Extent3d {
                width: sw,
                height: sh,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let seed: Vec<u8> = RED
            .iter()
            .copied()
            .cycle()
            .take((sw * sh * 4) as usize)
            .collect();
        renderer.queue.write_texture(
            src.as_image_copy(),
            &seed,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(sw * 4),
                rows_per_image: Some(sh),
            },
            wgpu::Extent3d {
                width: sw,
                height: sh,
                depth_or_array_layers: 1,
            },
        );
        let src_view = src.create_view(&Default::default());

        // Blit into a larger target and read it back.
        let (dw, dh) = (8u32, 8u32);
        let dst = renderer.create_frame_target(dw, dh);
        renderer.blit_into(&src_view, &dst);
        let pixels = renderer.read_pixels(&dst).expect("readback");

        // Center pixel of a uniform image must match the source colour.
        let cx = dw / 2;
        let cy = dh / 2;
        let i = ((cy * dw + cx) * 4) as usize;
        let px = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
        for c in 0..4 {
            let d = (i32::from(px[c]) - i32::from(RED[c])).abs();
            assert!(
                d <= 1,
                "channel {c}: blitted {} vs source {} (Δ {d})",
                px[c],
                RED[c]
            );
        }
    }

    /// The plain single-tile material builder serves the deferred UDIM cases
    /// (instanced / merged / skinned geometry). A `<UDIM>` template names a tile
    /// set, not a file, so it must be treated as absent and bound to the null
    /// texture — never handed to `get_or_load`, which would fail on the literal
    /// token path and spam "texture load failed" every frame.
    #[test]
    fn plain_material_bg_skips_udim_template_slots() {
        let mut renderer = match try_renderer() {
            Some(r) => r,
            None => return,
        };

        const UDIM_DIFFUSE: &str = "/nonexistent/albedo_<UDIM>_BaseColor.png";
        let mut mat = crate::material::fallback_material("/World/mat");
        mat.diffuse_texture = Some(UDIM_DIFFUSE.to_owned());

        let bgl = renderer.pipeline.material_bgl.clone();
        let sampler = renderer.sampler.clone();
        let _bg = build_material_bg(
            &renderer.device,
            &renderer.queue,
            &bgl,
            &sampler,
            &mut renderer.texture_cache,
            &mat,
        );

        assert!(
            !renderer
                .texture_cache
                .is_cached(UDIM_DIFFUSE, TextureKind::DiffuseSrgb),
            "the <UDIM> template must not be routed through the single-tile loader",
        );
    }
}
