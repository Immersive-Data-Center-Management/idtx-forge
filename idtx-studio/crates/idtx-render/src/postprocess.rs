//! Post-process chain + silhouette outline.
//!
//! Two cooperating pieces:
//!
//! 1. [`PostProcessChain`] — an **ordered fullscreen-pass abstraction** with
//!    ping-pong targets. The chain records which post-process stages are enabled
//!    and in what order, holds the named scalar parameters driven by the
//!    `SetPostprocessParam` overlay command, and computes the read/write
//!    ping-pong buffer indices for an N-stage run. It is pure data + arithmetic
//!    (no GPU handles) so the ordering / ping-pong scheduling is unit-tested
//!    independently of any device.
//!
//! 2. [`SilhouetteOutlinePass`] — a **jump-flood** silhouette outline over a
//!    prim-selection mask. A seed-init pass turns the mask into a seed-coordinate
//!    field, `log2(dim)` JFA ping-pong steps propagate the nearest seed to every
//!    pixel, and a composite pass reads the distance-to-nearest-seed and draws a
//!    coloured edge band over the scene. The jump-flood step schedule and the
//!    outline band falloff are pure functions (unit-tested against a CPU
//!    reference); the GPU pipelines are compile-checked (creation needs a
//!    device, like [`crate::overlay::DepthOverlayPass`]).
//!
//! The chain consumes the overlay command channel: `Highlight` selects the
//! prims that seed the outline, and `SetPostprocessParam` tunes the named
//! parameters (`outline.thickness`, `outline.softness`, …). Nothing here is ever
//! authored into USD — it is transient per-frame render state.

use std::collections::BTreeMap;

// ── Post-process stage vocabulary ────────────────────────────────────────────

/// The kind of a post-process stage in the chain.
///
/// Additive: new fullscreen effects (bloom, vignette, colour-grade) append here
/// without disturbing the ping-pong scheduling, which is purely a function of
/// how many stages are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PostProcessKind {
    /// Jump-flood silhouette outline around highlighted prims.
    SilhouetteOutline,
    /// Screen-space bloom (reserved; not yet wired to a GPU pass).
    Bloom,
    /// Full-frame colour grade / tone adjust (reserved).
    ColorGrade,
    /// Vignette darkening at the frame edges (reserved).
    Vignette,
}

impl PostProcessKind {
    /// Stable string key used to namespace this stage's parameters
    /// (`"<key>.<param>"`) and to match `SetPostprocessParam` names.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            PostProcessKind::SilhouetteOutline => "outline",
            PostProcessKind::Bloom => "bloom",
            PostProcessKind::ColorGrade => "colorgrade",
            PostProcessKind::Vignette => "vignette",
        }
    }
}

/// One entry in the ordered post-process chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostProcessStage {
    pub kind: PostProcessKind,
    /// When `false`, the stage keeps its slot/params but is skipped when
    /// scheduling ping-pong passes (so toggling it never renumbers the others'
    /// parameters).
    pub enabled: bool,
}

/// A single ping-pong hop: read from one target, write to the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingPong {
    /// Index (0 or 1) of the target this stage samples from.
    pub read: usize,
    /// Index (0 or 1) of the target this stage renders into.
    pub write: usize,
}

/// An ordered chain of fullscreen post-process stages with two ping-pong
/// targets and a flat namespaced parameter map.
///
/// The parameter map is keyed by `"<stage-key>.<name>"` (e.g.
/// `"outline.thickness"`) so a single `SetPostprocessParam { name, value }`
/// overlay command addresses any stage without the guest needing a stage index.
#[derive(Debug, Clone, Default)]
pub struct PostProcessChain {
    stages: Vec<PostProcessStage>,
    params: BTreeMap<String, f32>,
}

impl PostProcessChain {
    /// An empty chain (no stages, no params).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A chain with the silhouette outline enabled and its default parameters
    /// seeded — the editor's default runtime configuration.
    #[must_use]
    pub fn with_default_outline() -> Self {
        let mut c = Self::new();
        c.push(PostProcessKind::SilhouetteOutline, true);
        c.set_param("outline.thickness", DEFAULT_OUTLINE_THICKNESS_PX);
        c.set_param("outline.softness", DEFAULT_OUTLINE_SOFTNESS_PX);
        c
    }

    /// Append a stage. If `kind` already exists its `enabled` flag is updated in
    /// place (stages are unique by kind, preserving their original order).
    pub fn push(&mut self, kind: PostProcessKind, enabled: bool) {
        if let Some(s) = self.stages.iter_mut().find(|s| s.kind == kind) {
            s.enabled = enabled;
        } else {
            self.stages.push(PostProcessStage { kind, enabled });
        }
    }

    /// Enable/disable a stage without removing it or its parameters. No-op if
    /// the stage is absent.
    pub fn set_enabled(&mut self, kind: PostProcessKind, enabled: bool) {
        if let Some(s) = self.stages.iter_mut().find(|s| s.kind == kind) {
            s.enabled = enabled;
        }
    }

    /// All stages in insertion order (enabled and disabled).
    #[must_use]
    pub fn stages(&self) -> &[PostProcessStage] {
        &self.stages
    }

    /// The enabled stages in order — the passes that actually run this frame.
    pub fn enabled_stages(&self) -> impl Iterator<Item = PostProcessKind> + '_ {
        self.stages.iter().filter(|s| s.enabled).map(|s| s.kind)
    }

    /// Number of enabled stages (the ping-pong hop count).
    #[must_use]
    pub fn active_len(&self) -> usize {
        self.stages.iter().filter(|s| s.enabled).count()
    }

    /// Set a namespaced parameter (`"<stage-key>.<name>"`). This is the sink for
    /// the `SetPostprocessParam { name, value }` overlay command.
    pub fn set_param(&mut self, name: &str, value: f32) {
        self.params.insert(name.to_string(), value);
    }

    /// Read a parameter, or `default` if unset.
    #[must_use]
    pub fn param(&self, name: &str, default: f32) -> f32 {
        self.params.get(name).copied().unwrap_or(default)
    }

    /// The resolved silhouette-outline parameters (falling back to the defaults
    /// when unset), ready to hand to [`SilhouetteOutlinePass`].
    #[must_use]
    pub fn outline_params(&self) -> OutlineParams {
        OutlineParams {
            thickness_px: self
                .param("outline.thickness", DEFAULT_OUTLINE_THICKNESS_PX)
                .max(0.0),
            softness_px: self
                .param("outline.softness", DEFAULT_OUTLINE_SOFTNESS_PX)
                .max(0.0),
        }
    }
}

/// Compute the ping-pong read/write index sequence for `hops` fullscreen passes
/// that alternate between two targets, the first pass reading from `start`
/// (0 or 1).
///
/// Pass *k* reads `(start + k) % 2` and writes `(start + k + 1) % 2`, so a pass
/// never reads and writes the same target. Returns one [`PingPong`] per hop;
/// [`final_target`] reports which target holds the result.
#[must_use]
pub fn ping_pong_schedule(hops: usize, start: usize) -> Vec<PingPong> {
    let start = start & 1;
    (0..hops)
        .map(|k| PingPong {
            read: (start + k) & 1,
            write: (start + k + 1) & 1,
        })
        .collect()
}

/// The target index (0 or 1) holding the result after `hops` ping-pong passes
/// starting from `start`.
#[must_use]
pub fn final_target(hops: usize, start: usize) -> usize {
    (start + hops) & 1
}

// ── Jump-flood silhouette outline math ───────────────────────────────────────

/// Default outline band width, in pixels.
pub const DEFAULT_OUTLINE_THICKNESS_PX: f32 = 4.0;
/// Default outline edge softness (anti-alias falloff), in pixels.
pub const DEFAULT_OUTLINE_SOFTNESS_PX: f32 = 1.5;

/// Resolved silhouette-outline parameters (see [`PostProcessChain::outline_params`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineParams {
    pub thickness_px: f32,
    pub softness_px: f32,
}

/// Group `(prim path, linear-RGBA colour, thickness_px)` outline requests by
/// their `(colour, thickness)` pair, so the renderer encodes one
/// mask→jump-flood→composite pass per *distinct* appearance (the common
/// single-gaze case collapses to one pass). Compared by exact bit pattern;
/// first-appearance order is preserved. Each group yields its shared colour +
/// thickness and the member paths.
#[must_use]
pub fn group_outlines_by_color(
    requests: &[(String, [f32; 4], f32)],
) -> Vec<([f32; 4], f32, Vec<String>)> {
    use std::collections::hash_map::Entry;
    let mut order: Vec<[u32; 5]> = Vec::new();
    let mut groups: std::collections::HashMap<[u32; 5], ([f32; 4], f32, Vec<String>)> =
        std::collections::HashMap::new();
    for (path, color, thickness) in requests {
        let mut key = [0u32; 5];
        key[..4].copy_from_slice(&color.map(f32::to_bits));
        key[4] = thickness.to_bits();
        match groups.entry(key) {
            Entry::Occupied(mut e) => e.get_mut().2.push(path.clone()),
            Entry::Vacant(e) => {
                order.push(key);
                e.insert((*color, *thickness, vec![path.clone()]));
            }
        }
    }
    order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect()
}

/// The jump-flood step-size schedule for a texture whose largest dimension is
/// `max_dim` pixels: `next_pow2(max_dim) / 2`, halved down to 1.
///
/// This is the classic Rong-Tan jump-flooding sequence — `⌈log2(max_dim)⌉`
/// passes, each pass sampling neighbours at `± step` along both axes. A
/// single-pixel (or empty) target needs no propagation and yields an empty
/// schedule.
#[must_use]
pub fn jfa_step_sequence(max_dim: u32) -> Vec<u32> {
    if max_dim <= 1 {
        return Vec::new();
    }
    let mut step = max_dim.next_power_of_two() / 2;
    let mut out = Vec::new();
    while step >= 1 {
        out.push(step);
        step /= 2;
    }
    out
}

/// Outline coverage `[0,1]` for a pixel whose distance to the nearest seed
/// (selected prim) is `dist_px`.
///
/// Seed pixels (inside the silhouette) get `0` — the outline is drawn *around*
/// the selection, not over it. Outside, coverage is `1` at the silhouette edge
/// and falls linearly to `0` across `[thickness - softness, thickness]`,
/// giving an anti-aliased band `thickness` pixels wide.
#[must_use]
pub fn outline_alpha(dist_px: f32, is_seed: bool, params: OutlineParams) -> f32 {
    if is_seed {
        return 0.0;
    }
    let OutlineParams {
        thickness_px,
        softness_px,
    } = params;
    if thickness_px <= 0.0 {
        return 0.0;
    }
    let inner = (thickness_px - softness_px.max(0.0)).max(0.0);
    if dist_px <= inner {
        1.0
    } else if dist_px >= thickness_px {
        0.0
    } else {
        // Linear falloff across the soft band.
        1.0 - (dist_px - inner) / (thickness_px - inner)
    }
}

/// A CPU reference implementation of one jump-flood over an `w × h` seed grid,
/// used to validate the GPU shader's propagation. `seeds[y*w + x] == true`
/// marks a selected pixel.
///
/// Returns, per pixel, the squared pixel distance to the nearest seed
/// (`0` on a seed, `usize::MAX` if the grid has no seeds). Runs the full
/// [`jfa_step_sequence`] for `max(w, h)`.
#[cfg(test)]
pub(crate) fn jfa_reference(w: usize, h: usize, seeds: &[bool]) -> Vec<usize> {
    assert_eq!(seeds.len(), w * h);
    // Per-pixel nearest seed coordinate, if known.
    let mut nearest: Vec<Option<(usize, usize)>> = seeds
        .iter()
        .enumerate()
        .map(|(i, &s)| if s { Some((i % w, i / w)) } else { None })
        .collect();

    // Squared distance in pixels (unsigned throughout via `abs_diff`).
    let dist2 = |ax: usize, ay: usize, bx: usize, by: usize| -> usize {
        let dx = ax.abs_diff(bx);
        let dy = ay.abs_diff(by);
        dx * dx + dy * dy
    };

    let max_dim = u32::try_from(w.max(h)).expect("grid dim fits u32");
    for step_u in jfa_step_sequence(max_dim) {
        let step = usize::try_from(step_u).expect("step fits usize");
        let src = nearest.clone();
        for y in 0..h {
            for x in 0..w {
                let mut best = src[y * w + x];
                let mut best_d = best.map_or(usize::MAX, |(sx, sy)| dist2(x, y, sx, sy));
                // Neighbour coordinates at ±step, staying in-bounds (checked_sub
                // rejects underflow, the < w/h guards reject overflow).
                let ys = [y.checked_sub(step), Some(y), y.checked_add(step)];
                let xs = [x.checked_sub(step), Some(x), x.checked_add(step)];
                for oy in ys {
                    let Some(ny) = oy else { continue };
                    if ny >= h {
                        continue;
                    }
                    for ox in xs {
                        let Some(nx) = ox else { continue };
                        if nx >= w {
                            continue;
                        }
                        if let Some((sx, sy)) = src[ny * w + nx] {
                            let d = dist2(x, y, sx, sy);
                            if d < best_d {
                                best_d = d;
                                best = Some((sx, sy));
                            }
                        }
                    }
                }
                nearest[y * w + x] = best;
            }
        }
    }

    nearest
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let x = i % w;
            let y = i / w;
            n.map_or(usize::MAX, |(sx, sy)| dist2(x, y, sx, sy))
        })
        .collect()
}

// ── GPU silhouette outline pass (compile-checked) ────────────────────────────

/// Jump-flood silhouette outline pass over a prim-selection mask.
///
/// Holds the three fullscreen pipelines — seed-init, JFA step, and composite —
/// that together draw an anti-aliased outline around highlighted prims. The
/// caller allocates two ping-pong seed targets (see [`ping_pong_schedule`]),
/// runs the [`jfa_step_sequence`] hops, then composites over the scene colour.
///
/// GPU wiring (target allocation + the actual per-frame encode) lands with the
/// runtime overlay consumer that drains the `Highlight` commands into the
/// selection mask; this type is the pipeline set, compile-checked like
/// [`crate::overlay::DepthOverlayPass`].
pub struct SilhouetteOutlinePass {
    /// Mask (r8unorm) → seed-coordinate field (rg16float).
    pub seed_pipeline: wgpu::RenderPipeline,
    /// One jump-flood propagation step (samples 8 neighbours at `step`).
    pub jfa_pipeline: wgpu::RenderPipeline,
    /// Seed field + scene colour → outlined scene colour.
    pub composite_pipeline: wgpu::RenderPipeline,
    seed_bgl: wgpu::BindGroupLayout,
    jfa_bgl: wgpu::BindGroupLayout,
    composite_bgl: wgpu::BindGroupLayout,
}

impl std::fmt::Debug for SilhouetteOutlinePass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SilhouetteOutlinePass")
            .finish_non_exhaustive()
    }
}

/// The seed / JFA ping-pong target format: two f32 channels holding the nearest
/// seed's pixel coordinates (`-1` = no seed).
pub const SEED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

impl SilhouetteOutlinePass {
    /// Compile the three fullscreen pipelines. `output_format` is the colour
    /// target the composite pass writes into (typically the LDR `COLOR_FORMAT`,
    /// composited after tonemapping like the selection [`crate::overlay::OverlayPass`]).
    pub fn new(device: &wgpu::Device, output_format: wgpu::TextureFormat) -> Self {
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
            count: None,
        };
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let uniform_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };

        // Seed-init: sampler + mask texture.
        let seed_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Outline Seed BGL"),
            entries: &[sampler_entry(0), tex_entry(1)],
        });
        // JFA step: sampler + seed texture + step uniform.
        let jfa_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Outline JFA BGL"),
            entries: &[sampler_entry(0), tex_entry(1), uniform_entry(2)],
        });
        // Composite: sampler + seed texture + scene colour + outline uniform.
        let composite_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Outline Composite BGL"),
            entries: &[
                sampler_entry(0),
                tex_entry(1),
                tex_entry(2),
                uniform_entry(3),
            ],
        });

        let seed_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Outline Seed Shader"),
            source: wgpu::ShaderSource::Wgsl(SEED_INIT_WGSL.as_str().into()),
        });
        let jfa_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Outline JFA Shader"),
            source: wgpu::ShaderSource::Wgsl(JFA_STEP_WGSL.as_str().into()),
        });
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Outline Composite Shader"),
            source: wgpu::ShaderSource::Wgsl(OUTLINE_COMPOSITE_WGSL.as_str().into()),
        });

        let seed_pipeline = build_fullscreen_pipeline(
            device,
            "Outline Seed",
            &seed_bgl,
            &seed_shader,
            SEED_FORMAT,
            None,
        );
        let jfa_pipeline = build_fullscreen_pipeline(
            device,
            "Outline JFA",
            &jfa_bgl,
            &jfa_shader,
            SEED_FORMAT,
            None,
        );
        // Composite blends the outline over the existing colour target.
        let composite_pipeline = build_fullscreen_pipeline(
            device,
            "Outline Composite",
            &composite_bgl,
            &composite_shader,
            output_format,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );

        Self {
            seed_pipeline,
            jfa_pipeline,
            composite_pipeline,
            seed_bgl,
            jfa_bgl,
            composite_bgl,
        }
    }

    /// The seed-init bind-group layout (sampler @0, mask texture @1).
    #[must_use]
    pub fn seed_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.seed_bgl
    }
    /// The JFA-step bind-group layout (sampler @0, seed texture @1, step uniform @2).
    #[must_use]
    pub fn jfa_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.jfa_bgl
    }
    /// The composite bind-group layout (sampler @0, seed @1, scene colour @2, outline uniform @3).
    #[must_use]
    pub fn composite_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.composite_bgl
    }
}

/// GPU uniform for a single JFA step (step size in pixels + texel size).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct JfaStepUniform {
    /// Neighbour sample offset, in pixels.
    pub step_px: f32,
    pub _pad: f32,
    /// `1 / width`, `1 / height` of the seed target.
    pub texel_size: [f32; 2],
}

/// GPU uniform for the composite pass (outline band + colour).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OutlineCompositeUniform {
    pub color: [f32; 4],
    pub thickness_px: f32,
    pub softness_px: f32,
    pub _pad: [f32; 2],
}

/// Shared builder for the three fullscreen pipelines (they differ only in bind
/// layout, shader, target format, and blend).
fn build_fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    bgl: &wgpu::BindGroupLayout,
    shader: &wgpu::ShaderModule,
    target_format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

// ── WGSL shaders ─────────────────────────────────────────────────────────────

/// Shared fullscreen-triangle vertex stage (matches the tonemap/blit convention).
const FULLSCREEN_VS: &str = r"
struct VsOut {
    @builtin(position) pos : vec4<f32>,
    @location(0)       uv  : vec2<f32>,
}
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
";

/// Seed-init: a selection-mask texel > 0.5 seeds itself with its own pixel
/// coordinate; everything else stores `(-1, -1)` (the "no seed" sentinel).
static SEED_INIT_WGSL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r"{FULLSCREEN_VS}
@group(0) @binding(0) var samp : sampler;
@group(0) @binding(1) var mask : texture_2d<f32>;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec2<f32> {{
    let dims = vec2<f32>(textureDimensions(mask));
    let coord = in.uv * dims;
    let m = textureSample(mask, samp, in.uv).r;
    if (m > 0.5) {{
        return coord;               // seed with own pixel coordinate
    }}
    return vec2<f32>(-1.0, -1.0);   // no seed
}}
"
    )
});

/// One jump-flood step: sample the current seed field at the 8 neighbours (and
/// self) offset by `step_px`, keep the seed nearest to this pixel.
static JFA_STEP_WGSL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r"{FULLSCREEN_VS}
struct StepU {{
    step_px : f32,
    _pad    : f32,
    texel   : vec2<f32>,
}}
@group(0) @binding(0) var samp : sampler;
@group(0) @binding(1) var seed : texture_2d<f32>;
@group(0) @binding(2) var<uniform> u : StepU;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec2<f32> {{
    let dims = vec2<f32>(textureDimensions(seed));
    let here = in.uv * dims;
    var best = vec2<f32>(-1.0, -1.0);
    var best_d = 3.4e38;
    for (var dy = -1; dy <= 1; dy = dy + 1) {{
        for (var dx = -1; dx <= 1; dx = dx + 1) {{
            let off = vec2<f32>(f32(dx), f32(dy)) * u.step_px * u.texel;
            let s = textureSample(seed, samp, in.uv + off).xy;
            if (s.x >= 0.0) {{
                let d = distance(here, s);
                if (d < best_d) {{
                    best_d = d;
                    best = s;
                }}
            }}
        }}
    }}
    return best;
}}
"
    )
});

/// Composite: distance from this pixel to its nearest seed → an anti-aliased
/// outline band blended over the scene colour. Mirrors [`outline_alpha`].
static OUTLINE_COMPOSITE_WGSL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        r"{FULLSCREEN_VS}
struct OutlineU {{
    color        : vec4<f32>,
    thickness_px : f32,
    softness_px  : f32,
    _pad         : vec2<f32>,
}}
@group(0) @binding(0) var samp  : sampler;
@group(0) @binding(1) var seed  : texture_2d<f32>;
@group(0) @binding(2) var scene : texture_2d<f32>;
@group(0) @binding(3) var<uniform> u : OutlineU;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{
    let dims = vec2<f32>(textureDimensions(seed));
    let here = in.uv * dims;
    let s = textureSample(seed, samp, in.uv).xy;
    let base = textureSample(scene, samp, in.uv);
    if (s.x < 0.0) {{
        return base;                // no seed reached this pixel
    }}
    let dist = distance(here, s);
    let inner = max(u.thickness_px - max(u.softness_px, 0.0), 0.0);
    var a = 0.0;
    if (dist <= 0.5) {{
        a = 0.0;                    // inside the silhouette
    }} else if (dist <= inner) {{
        a = 1.0;
    }} else if (dist < u.thickness_px) {{
        a = 1.0 - (dist - inner) / (u.thickness_px - inner);
    }}
    let edge = u.color.a * a;
    return vec4<f32>(mix(base.rgb, u.color.rgb, edge), 1.0);
}}
"
    )
});

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact-value assertions on known constants
mod tests {
    use super::*;

    // ── Chain / ping-pong ─────────────────────────────────────────────────

    #[test]
    fn push_is_unique_by_kind_and_preserves_order() {
        let mut c = PostProcessChain::new();
        c.push(PostProcessKind::SilhouetteOutline, true);
        c.push(PostProcessKind::Bloom, true);
        c.push(PostProcessKind::SilhouetteOutline, false); // update, not append
        assert_eq!(c.stages().len(), 2);
        assert_eq!(c.stages()[0].kind, PostProcessKind::SilhouetteOutline);
        assert!(!c.stages()[0].enabled);
        assert_eq!(c.active_len(), 1); // only Bloom enabled
    }

    #[test]
    fn group_outlines_collapses_by_distinct_color() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let green = [0.0, 1.0, 0.0, 1.0];
        let reqs = vec![
            ("/A".to_string(), red, 4.0),
            ("/B".to_string(), green, 4.0),
            ("/C".to_string(), red, 4.0),
        ];
        let groups = group_outlines_by_color(&reqs);
        // Two distinct colours → two passes; first-seen order (red, green).
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, red);
        assert_eq!(groups[0].2, vec!["/A".to_string(), "/C".to_string()]);
        assert_eq!(groups[1].0, green);
        assert_eq!(groups[1].2, vec!["/B".to_string()]);
    }

    #[test]
    fn group_outlines_splits_by_thickness() {
        let c = [0.2, 0.4, 0.6, 1.0];
        // Same colour, different thickness → two separate passes.
        let reqs = vec![("/A".to_string(), c, 4.0), ("/B".to_string(), c, 8.0)];
        let groups = group_outlines_by_color(&reqs);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].1, 4.0);
        assert_eq!(groups[1].1, 8.0);
    }

    #[test]
    fn group_outlines_single_color_is_one_pass() {
        let c = [0.2, 0.4, 0.6, 1.0];
        let reqs = vec![("/A".to_string(), c, 4.0), ("/B".to_string(), c, 4.0)];
        let groups = group_outlines_by_color(&reqs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].2.len(), 2);
    }

    #[test]
    fn enabled_stages_skips_disabled_without_renumbering() {
        let mut c = PostProcessChain::new();
        c.push(PostProcessKind::SilhouetteOutline, true);
        c.push(PostProcessKind::Bloom, false);
        c.push(PostProcessKind::Vignette, true);
        let active: Vec<_> = c.enabled_stages().collect();
        assert_eq!(
            active,
            vec![
                PostProcessKind::SilhouetteOutline,
                PostProcessKind::Vignette
            ]
        );
    }

    #[test]
    fn ping_pong_alternates_and_never_aliases() {
        let sched = ping_pong_schedule(3, 0);
        assert_eq!(
            sched,
            vec![
                PingPong { read: 0, write: 1 },
                PingPong { read: 1, write: 0 },
                PingPong { read: 0, write: 1 },
            ]
        );
        // No pass reads and writes the same target.
        for pp in &sched {
            assert_ne!(pp.read, pp.write);
        }
        assert_eq!(final_target(3, 0), 1);
        assert_eq!(final_target(0, 0), 0);
        // Odd start flips the parity.
        assert_eq!(ping_pong_schedule(1, 1)[0], PingPong { read: 1, write: 0 });
    }

    #[test]
    fn params_are_namespaced_and_default_when_unset() {
        let mut c = PostProcessChain::new();
        c.set_param("outline.thickness", 7.0);
        assert_eq!(c.param("outline.thickness", 4.0), 7.0);
        assert_eq!(c.param("outline.softness", 1.5), 1.5); // unset → default
    }

    #[test]
    fn default_outline_chain_seeds_params() {
        let c = PostProcessChain::with_default_outline();
        assert_eq!(c.active_len(), 1);
        let p = c.outline_params();
        assert_eq!(p.thickness_px, DEFAULT_OUTLINE_THICKNESS_PX);
        assert_eq!(p.softness_px, DEFAULT_OUTLINE_SOFTNESS_PX);
    }

    // ── Jump-flood step schedule ───────────────────────────────────────────

    #[test]
    fn jfa_schedule_is_halving_powers_of_two() {
        assert_eq!(jfa_step_sequence(8), vec![4, 2, 1]);
        assert_eq!(jfa_step_sequence(16), vec![8, 4, 2, 1]);
        // Non-power-of-two rounds up first.
        assert_eq!(jfa_step_sequence(10), vec![8, 4, 2, 1]);
        // Degenerate sizes need no propagation.
        assert!(jfa_step_sequence(1).is_empty());
        assert!(jfa_step_sequence(0).is_empty());
    }

    // ── Outline band falloff ───────────────────────────────────────────────

    #[test]
    fn outline_alpha_band_shape() {
        let p = OutlineParams {
            thickness_px: 4.0,
            softness_px: 2.0,
        };
        // Seeds never get an outline.
        assert_eq!(outline_alpha(0.0, true, p), 0.0);
        // Inside the solid band → full coverage.
        assert_eq!(outline_alpha(1.0, false, p), 1.0);
        assert_eq!(outline_alpha(2.0, false, p), 1.0);
        // Beyond the band → nothing.
        assert_eq!(outline_alpha(4.0, false, p), 0.0);
        assert_eq!(outline_alpha(10.0, false, p), 0.0);
        // Halfway across the soft band → ~0.5.
        let mid = outline_alpha(3.0, false, p);
        assert!((mid - 0.5).abs() < 1e-6, "mid={mid}");
    }

    #[test]
    fn outline_alpha_zero_thickness_is_off() {
        let p = OutlineParams {
            thickness_px: 0.0,
            softness_px: 0.0,
        };
        assert_eq!(outline_alpha(1.0, false, p), 0.0);
    }

    // ── Jump-flood CPU reference (validates the shader's propagation) ───────

    #[test]
    fn jfa_reference_single_seed_reaches_every_pixel() {
        // 8×8 grid, one seed at (2,3). Every pixel must resolve to that seed's
        // exact squared distance (JFA is exact for a single seed).
        let (w, h) = (8usize, 8usize);
        let mut seeds = vec![false; w * h];
        seeds[3 * w + 2] = true;
        let dist2 = jfa_reference(w, h, &seeds);
        for y in 0..h {
            for x in 0..w {
                let dx = x.abs_diff(2);
                let dy = y.abs_diff(3);
                let expected = dx * dx + dy * dy;
                assert_eq!(dist2[y * w + x], expected, "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn jfa_reference_no_seeds_is_unreached() {
        let d = jfa_reference(4, 4, &vec![false; 16]);
        assert!(d.iter().all(|&v| v == usize::MAX));
    }

    #[test]
    fn jfa_reference_two_seeds_takes_nearest() {
        // Seeds at both ends of a 1×9 row; the middle resolves to the closer one.
        let (w, h) = (9usize, 1usize);
        let mut seeds = vec![false; w];
        seeds[0] = true;
        seeds[8] = true;
        let d = jfa_reference(w, h, &seeds);
        assert_eq!(d[0], 0);
        assert_eq!(d[8], 0);
        assert_eq!(d[1], 1); // nearest seed is at x=0
        assert_eq!(d[7], 1); // nearest seed is at x=8
        assert_eq!(d[4], 16); // equidistant (4² from either); ties resolve to 16
    }

    #[test]
    fn shaders_are_nonempty() {
        // Cheap guard that the LazyLock shader assembly ran.
        assert!(SEED_INIT_WGSL.contains("fs_main"));
        assert!(JFA_STEP_WGSL.contains("textureSample"));
        assert!(OUTLINE_COMPOSITE_WGSL.contains("thickness_px"));
    }
}
