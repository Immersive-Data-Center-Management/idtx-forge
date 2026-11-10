//! wgpu render pipeline for Phase 4 PBR rendering.
//!
//! Provides two `wgpu::RenderPipeline` variants (single-sided and double-sided)
//! that share bind-group layouts so material bind groups can be reused across
//! both draw modes.
//!
//! # Bind group layout
//!
//! | Group | Binding | Type              | Contents                           |
//! |-------|---------|-------------------|------------------------------------|
//! | 0     | 0       | uniform           | `CameraUniforms`                   |
//! | 1     | 0       | uniform           | `LightsUniforms` (≤16 lights)      |
//! | 2     | 0       | uniform           | `MeshUniforms`                     |
//! | 3     | 0       | sampler           | bilinear + repeat                  |
//! | 3     | 1–6     | texture_2d\<f32\> | diffuse, normal, metal, rough, ao, emissive |
//! | 3     | 7       | uniform           | `MaterialUniforms`                 |

use bytemuck::{Pod, Zeroable};

use crate::scene_extract::SHADOW_BUDGET;
use crate::surface_gen::{
    PBR_DEPTH_PREPASS_INSTANCED_WGSL, PBR_DEPTH_PREPASS_WGSL, PBR_INSTANCED_WGSL_SHADER,
    PBR_SHADOW_DEPTH_INSTANCED_WGSL, PBR_SHADOW_DEPTH_SKINNED_WGSL, PBR_SHADOW_DEPTH_WGSL,
    PBR_SKINNED_WGSL_SHADER, PBR_WGSL_SHADER, WIREFRAME_INSTANCED_WGSL_SHADER,
    WIREFRAME_SKINNED_WGSL_SHADER, WIREFRAME_WGSL_SHADER,
};
use crate::vertex::{InstanceRaw, SkinVertex, Vertex};

// ── Light constants (must match WGSL) ─────────────────────────────────────────
pub const LIGHT_DISTANT: u32 = 0;
pub const LIGHT_SPHERE: u32 = 1;
pub const LIGHT_RECT: u32 = 2;
pub const LIGHT_DOME: u32 = 3;
pub const LIGHT_DOME_PROC: u32 = 4;
pub const LIGHT_DISK: u32 = 5;
pub const MAX_LIGHTS: usize = 16;

/// Intermediate HDR render-target format (PBR pass + MSAA resolve).
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Default MSAA sample count for the PBR pass. The *effective* count is resolved
/// per-`Renderer` at construction via [`resolve_sample_count`] — this is the
/// desktop/PCVR fallback and the value returned when nothing overrides it.
pub const MSAA_SAMPLES: u32 = 4;

/// Coerce an arbitrary requested MSAA count to one of the supported values
/// `{1, 2, 4}`, falling back to `default` when the request is absent or invalid.
///
/// Pure (no env, no device) so the policy is unit-testable without a GPU.
#[must_use]
pub fn sanitize_sample_count(requested: Option<u32>, default: u32) -> u32 {
    match requested {
        Some(1 | 2 | 4) => requested.unwrap(),
        _ => default,
    }
}

/// Clamp a requested MSAA count to one the device actually supports, given a
/// `supported(n)` predicate (typically the HDR + depth
/// `TextureFormatFeatureFlags::sample_count_supported`).
///
/// WebGPU core only guarantees 1× and 4× for most formats; 2× needs
/// `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` and adapter support. When the
/// request is unsupported this steps **up to 4×** (always safe, no visual
/// regression) rather than silently dropping to an aliased 1×. 1× is the final
/// fallback since it is universally supported.
#[must_use]
pub fn clamp_sample_count_to_support(requested: u32, supported: impl Fn(u32) -> bool) -> u32 {
    if supported(requested) {
        requested
    } else if requested < 4 && supported(4) {
        eprintln!(
            "idtx-render: MSAA {requested}× unsupported for the render targets; using 4×"
        );
        4
    } else {
        eprintln!("idtx-render: MSAA {requested}× unsupported; falling back to 1×");
        1
    }
}

/// Resolve the effective MSAA sample count for a new `Renderer` (QI-2).
///
/// Mirrors `scale_eye_resolution` in `idtx-openxr-sys`: a `#[cfg]`-selected
/// platform default plus an `IDTX_XR_MSAA` env override, validated to `{1, 2, 4}`.
///
/// * **Android/Quest**: default **4×**. Run 2 (`docs/xr-quest-native-performance-baseline.md`)
///   measured 4×→2× as only ~1.9 ms on the Adreno tiler — the frame is ALU/shading-bound,
///   not resolve-bandwidth-bound — while the 2× geometric coverage drop was implicated in
///   the QF7 white-edge fringing. 2× is a poor trade (small perf, real quality loss), so the
///   baked default is restored to 4× and the fragment headroom is recovered via the Z-prepass
///   (QI-4, [`resolve_z_prepass`]) instead. Android apps do not inherit a shell env, so this
///   baked default — not the var — is what runs on device; the var is for PCVR/desktop tuning.
/// * **Everything else (desktop / PCVR)**: default **4×**, unchanged.
#[must_use]
pub fn resolve_sample_count() -> u32 {
    // 4× on every platform; the Android fragment budget is recovered via the Z-prepass
    // (QI-4) rather than by dropping MSAA (QF7 — the 2× quality regression).
    const DEFAULT_MSAA: u32 = MSAA_SAMPLES;

    let requested = std::env::var("IDTX_XR_MSAA")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let count = sanitize_sample_count(requested, DEFAULT_MSAA);
    eprintln!("idtx-render: resolved MSAA sample count = {count}× (default {DEFAULT_MSAA}×)");
    count
}

/// Resolve whether the camera depth **Z-prepass** (QI-4) is active for a new
/// `Renderer`.
///
/// A depth-only pre-pass over opaque geometry populates the depth buffer before the
/// beauty PBR pass, which then tests `LessEqual` with depth-write **off** so each
/// pixel/sample is shaded ~once — removing overdraw (Quest Run 2 measured **3.6×**).
/// It is resolution-independent, which is why it funds raising render-scale back up.
///
/// Mirrors [`resolve_sample_count`]: a `#[cfg]`-selected platform default plus an
/// `IDTX_XR_ZPREPASS ∈ {0,1}` env override.
///
/// * **Android/Quest**: default **on** — the tiler has measured 3.6× overdraw to remove.
/// * **visionOS/Vision Pro**: default **on** — same Apple TBDR tile-GPU class; the on-device
///   datacenter capture showed a ~34 ms beauty pass dominated by uncounted overdraw (Metal
///   lacks a pipeline-statistics query, so overdraw reads 0 but is real). Gated on
///   `target_os = "visionos"`, NOT `target_vendor = "apple"`, so the macOS desktop editor
///   (overdraw < 1×) keeps the prepass off.
/// * **Everything else (desktop / PCVR)**: default **off** — desktop overdraw measured
///   < 1×, so a prepass is pure vertex/record overhead there. The env override exists for
///   A/B on PCVR/desktop (Android ignores the shell env, so the baked default is what runs
///   on device).
#[must_use]
pub fn resolve_z_prepass() -> bool {
    #[cfg(any(target_os = "android", target_os = "visionos"))]
    const DEFAULT_ZPREPASS: bool = true;
    #[cfg(not(any(target_os = "android", target_os = "visionos")))]
    const DEFAULT_ZPREPASS: bool = false;

    let enabled = std::env::var("IDTX_XR_ZPREPASS")
        .ok()
        .and_then(|s| match s.trim() {
            "1" | "true" | "on" => Some(true),
            "0" | "false" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(DEFAULT_ZPREPASS);
    eprintln!("idtx-render: Z-prepass (QI-4) = {enabled} (default {DEFAULT_ZPREPASS})");
    enabled
}

/// Shadow-atlas layer resolution (texels per side). Desktop default; the XR
/// targets (Quest 2/3/Pro) will clamp this down through a future hardware knob.
/// The atlas is `Depth32Float` `D2Array` with [`SHADOW_BUDGET`] layers.
pub const SHADOW_ATLAS_DIM: u32 = 2048;

// ── Uniform structs ────────────────────────────────────────────────────────────

/// Camera matrices, eye position, and physical exposure.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct CameraUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub eye_pos: [f32; 4],
    /// Linear scene exposure multiplier derived from physical camera EV100.
    /// 0.0 in this field is never sent to the GPU; the renderer substitutes 1.0.
    pub exposure: f32,
    pub _pad1: [f32; 3],
}

/// One GPU-facing light record (64 bytes, matches WGSL `LightGpu`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LightGpu {
    pub kind: u32,
    pub _pad: [u32; 3],
    /// Distant: world-space direction toward the light (normalised).
    /// Sphere/rect: world-space position.
    pub dir_or_pos: [f32; 4],
    /// rgb = linear colour, w = effective point-source radiant intensity
    /// (`intensity × 2^exposure` folded with the emitter-area / `normalize` term).
    pub color_power: [f32; 4],
    /// x = sphere/disk radius or rect width (render units); y = rect height;
    /// z = per-light diffuse multiplier; w = per-light specular multiplier.
    pub size: [f32; 4],
}

/// All lights for a frame (1040 bytes, matches WGSL `LightsUniforms`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LightsUniforms {
    pub lights: [LightGpu; MAX_LIGHTS],
    pub count: u32,
    pub _pad: [u32; 3],
}

/// `UsdLuxDomeLight` environment IBL parameters (group 1, binding 3).
///
/// Matches WGSL `EnvUniforms`. `sh` holds 9 order-2 spherical-harmonic **radiance**
/// coefficients (rgb in `xyz`) for the diffuse irradiance evaluation; `rot0..2` are
/// the columns of the world→environment rotation `mat3`; `ambient` is the dome's
/// flat radiance (`color × power`) used for the untextured specular term and as the
/// diffuse source for untextured domes. `params` = `(mip_max, has_env, has_texture, _)`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct EnvUniforms {
    pub sh: [[f32; 4]; 9],
    pub rot0: [f32; 4],
    pub rot1: [f32; 4],
    pub rot2: [f32; 4],
    pub ambient: [f32; 4],
    /// x = max mip index of the env texture; y = `has_env` (0/1); z = `has_texture` (0/1); w = pad.
    pub params: [f32; 4],
}

impl Default for EnvUniforms {
    fn default() -> Self {
        Self {
            sh: [[0.0; 4]; 9],
            rot0: [1.0, 0.0, 0.0, 0.0],
            rot1: [0.0, 1.0, 0.0, 0.0],
            rot2: [0.0, 0.0, 1.0, 0.0],
            ambient: [0.0; 4],
            params: [0.0; 4],
        }
    }
}

/// Per-frame camera data for the skybox pass (group 0, binding 0).
///
/// Matches WGSL `SkyUniforms`. `inv_view_proj` unprojects a pixel's NDC into world
/// space to reconstruct the view ray; `eye_pos` is carried for parity (the ray is
/// direction-only for an environment at infinity, so `eye_pos` is currently unused
/// by the shader but kept so the layout can grow without a binding change).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SkyboxUniforms {
    pub inv_view_proj: [[f32; 4]; 4],
    pub eye_pos: [f32; 4],
}

impl Default for SkyboxUniforms {
    fn default() -> Self {
        Self {
            inv_view_proj: [[0.0; 4]; 4],
            eye_pos: [0.0; 4],
        }
    }
}

/// Per-draw-call model transform and normal matrix.
///
/// The normal matrix columns are the columns of `transpose(inverse(model_3x3))`.
/// Passing three vec4 rows (with w=0) keeps the struct 16-byte-aligned.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MeshUniforms {
    pub model: [[f32; 4]; 4],
    /// Column 0 of the normal matrix (xyz used; w = 0).
    pub nrm_col0: [f32; 4],
    /// Column 1 of the normal matrix.
    pub nrm_col1: [f32; 4],
    /// Column 2 of the normal matrix.
    pub nrm_col2: [f32; 4],
}

impl MeshUniforms {
    /// Builds a `MeshUniforms` from a world-space column-major matrix.
    #[must_use]
    pub fn from_matrix(m: glam::Mat4) -> Self {
        // Normal matrix = transpose(inverse(upper-left 3×3 of model)).
        // Using the full 4×4 inverse is equivalent and avoids a separate Mat3.
        let inv_t = m.inverse().transpose();
        Self {
            model: m.to_cols_array_2d(),
            nrm_col0: inv_t.col(0).to_array(),
            nrm_col1: inv_t.col(1).to_array(),
            nrm_col2: inv_t.col(2).to_array(),
        }
    }
}

/// Per-material scalar PBR parameters and texture feature bitmask.
///
/// Layout (64 bytes, matches WGSL `MaterialUniforms`):
/// - `base_color` (vec4): rgb scalar + opacity scalar
/// - `mr_sub_thr` (vec4): metallic, roughness, subsurface, `opacity_threshold`
/// - `emissive` (vec3) + `features` (u32)
/// - `channels` (vec4<u32>): `metallic_ch`, `roughness_ch`, `occlusion_ch`, (pad)
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MaterialUniforms {
    pub base_color: [f32; 4],
    pub mr_sub_thr: [f32; 4],
    pub emissive: [f32; 3],
    pub features: u32,
    pub channels: [u32; 4],
}

impl MaterialUniforms {
    /// Builds `MaterialUniforms` from a [`crate::material::RenderMaterialDesc`].
    #[must_use]
    pub fn from_material(m: &crate::material::RenderMaterialDesc) -> Self {
        let mut features: u32 = 0;
        if m.diffuse_texture.is_some() {
            features |= 1 << 0;
        }
        if m.normal_texture.is_some() {
            features |= 1 << 1;
        }
        if m.metallic_texture.is_some() {
            features |= 1 << 2;
        }
        if m.roughness_texture.is_some() {
            features |= 1 << 3;
        }
        if m.occlusion_texture.is_some() {
            features |= 1 << 4;
        }
        if m.emissive_texture.is_some() {
            features |= 1 << 5;
        }
        if m.uses_diffuse_texture_alpha {
            features |= 1 << 6;
        }
        // Blend materials are drawn through the PREMULTIPLIED_ALPHA_BLENDING
        // transparent pipeline; the shader must premultiply its colour by opacity.
        if m.is_transparent() {
            features |= 1 << 7;
        }

        Self {
            base_color: [m.base_color[0], m.base_color[1], m.base_color[2], m.opacity],
            mr_sub_thr: [m.metallic, m.roughness, m.subsurface, m.opacity_threshold],
            emissive: m.emissive,
            features,
            channels: [
                m.metallic_channel,
                m.roughness_channel,
                m.occlusion_channel,
                3,
            ],
        }
    }
}

// ── Shadow uniforms ───────────────────────────────────────────────────────────

/// GPU shadow state, folded into bind group 1 (binding 3) — the caster→atlas-slot
/// map plus the per-frame filtering config. Kept a fixed [`SHADOW_BUDGET`]-slot
/// uniform so `LightGpu` never has to grow (it stays 64 bytes). Matches WGSL
/// `ShadowUniforms`.
///
/// The PBR fragment shader, for each light `i`, scans `params[..count].x` for a
/// slot whose source-light index equals `i`; the matching slot's `light_view_proj`
/// projects the shaded point into that atlas layer for the depth compare.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
#[allow(missing_debug_implementations)]
pub struct ShadowUniforms {
    /// Per-slot light-space view-projection (world → shadow NDC, `near→0/far→1`).
    pub light_view_proj: [[[f32; 4]; 4]; SHADOW_BUDGET],
    /// Per-slot params: x = source light index (as `f32`), y = near, z = far,
    /// w = light size (angular diameter / radius; consumed by the PCSS tier).
    pub params: [[f32; 4]; SHADOW_BUDGET],
    /// Frame-global config: x = tier (0 = Off, 1 = Smooth/PCF, 2 = Soft/PCSS,
    /// 3 = Hard/single-tap), y = atlas resolution (texels), z = active slot count,
    /// w = constant receiver depth bias (shadow-NDC units).
    pub global: [f32; 4],
}

impl Default for ShadowUniforms {
    fn default() -> Self {
        Self::zeroed()
    }
}

// ── UDIM per-material tile table ──────────────────────────────────────────────
/// Sentinel layer index (mirrors [`crate::texture_cache::UDIM_LAYER_SENTINEL`]).
pub const UDIM_TABLE_SENTINEL: u32 = 0xFFFF_FFFF;

/// Per-material UDIM tile→layer table (WGSL `UdimTable`, group 3 binding 8).
///
/// `layers` holds one array-layer index per `(channel, tile)`: the flat index is
/// `channel * 100 + (tile - 1001)` and the value is that channel's array layer,
/// or [`UDIM_TABLE_SENTINEL`] when the tile is absent (shader keeps the scalar
/// fallback). Packed as 150 `vec4<u32>` (600 u32 = 6 channels × 100 tiles) to
/// satisfy the uniform array stride. `channel_mask` bit `c` is set when channel
/// `c` samples a UDIM array (channel order: diffuse, normal, metallic, roughness,
/// occlusion, emissive).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
#[allow(missing_debug_implementations)]
pub struct UdimTable {
    pub layers: [[u32; 4]; 150],
    pub channel_mask: u32,
    pub _pad: [u32; 3],
}

impl UdimTable {
    /// A table with every tile absent (all sentinels, no UDIM channels).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            layers: [[UDIM_TABLE_SENTINEL; 4]; 150],
            channel_mask: 0,
            _pad: [0; 3],
        }
    }

    /// Sets the layer index for `(channel, tile)` (`channel` 0..6, `tile` a UDIM
    /// number 1001..=1100). Out-of-range inputs are ignored.
    pub fn set(&mut self, channel: u32, tile: u32, layer: u32) {
        if channel >= 6 || !(1001..=1100).contains(&tile) {
            return;
        }
        let flat = (channel * 100 + (tile - 1001)) as usize;
        self.layers[flat / 4][flat % 4] = layer;
    }
}

// ── Tonemapping pipeline ──────────────────────────────────────────────────────

/// Fullscreen ACES tonemapping pass: `HDR_FORMAT` (`Rgba16Float`) → `COLOR_FORMAT` (`Rgba8UnormSrgb`).
#[derive(Debug)]
pub struct ToneMapPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
}

/// Compiles the tonemapping pipeline for the given LDR output format.
#[must_use]
pub fn build_tonemap_pipeline(
    device: &wgpu::Device,
    ldr_format: wgpu::TextureFormat,
) -> ToneMapPipeline {
    use crate::surface_gen::TONEMAP_WGSL_SHADER;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Tonemap Shader"),
        source: wgpu::ShaderSource::Wgsl(TONEMAP_WGSL_SHADER.into()),
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Tonemap BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Tonemap Pipeline Layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Tonemap Pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: ldr_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Tonemap Sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    ToneMapPipeline {
        pipeline,
        bgl,
        sampler,
    }
}

/// A fullscreen texture-to-texture blit pipeline (linear-sampled passthrough).
///
/// Shares the tonemap pipeline's bind-group shape (sampler at 0, texture at 1)
/// but samples with a linear filter so a smaller source (e.g. one XR eye) scales
/// cleanly into a larger destination (the desktop preview).
#[allow(missing_debug_implementations)]
pub struct BlitPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
}

/// Compiles the blit pipeline for the given output color format.
#[must_use]
pub fn build_blit_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
) -> BlitPipeline {
    use crate::surface_gen::BLIT_WGSL_SHADER;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Blit Shader"),
        source: wgpu::ShaderSource::Wgsl(BLIT_WGSL_SHADER.into()),
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Blit BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Blit Pipeline Layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Blit Pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("Blit Sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    BlitPipeline {
        pipeline,
        bgl,
        sampler,
    }
}

// ── Depth resolve ───────────────────────────────────────────────────────────

/// A fullscreen pass that resolves a 2×/4× MSAA depth buffer into a single-sample
/// depth target, or passes a single-sample depth through unchanged at 1× (see
/// [`depth_resolve_wgsl`](crate::surface_gen::depth_resolve_wgsl)).
///
/// Needed by the XR path only: wgpu cannot MSAA-resolve depth in a render pass
/// nor copy MSAA→single-sample, so the resolved single-sample depth is what gets
/// copied into the `OpenXR` depth swapchain.
#[allow(missing_debug_implementations)]
pub struct DepthResolvePipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
}

/// Compiles the MSAA depth-resolve pipeline for the given depth format.
#[must_use]
pub fn build_depth_resolve_pipeline(
    device: &wgpu::Device,
    depth_format: wgpu::TextureFormat,
    sample_count: u32,
) -> DepthResolvePipeline {
    use crate::surface_gen::depth_resolve_wgsl;
    build_depth_resolve_pipeline_from(device, depth_format, &depth_resolve_wgsl(sample_count), sample_count)
}

/// Reverse-Z MSAA depth-resolve pipeline for the visionOS present path: resolves
/// the renderer's forward-Z MSAA depth into a single-sample reverse-Z buffer
/// (`1.0 - d`), the convention `CompositorServices` requires for its drawable depth
/// textures. See [`crate::surface_gen::DEPTH_RESOLVE_REVERSE_Z_WGSL_SHADER`].
#[must_use]
pub fn build_depth_resolve_reverse_z_pipeline(
    device: &wgpu::Device,
    depth_format: wgpu::TextureFormat,
    sample_count: u32,
) -> DepthResolvePipeline {
    use crate::surface_gen::depth_resolve_reverse_z_wgsl;
    build_depth_resolve_pipeline_from(device, depth_format, &depth_resolve_reverse_z_wgsl(sample_count), sample_count)
}

/// Shared compiler for the forward-Z and reverse-Z depth-resolve pipelines; they
/// differ only in the fullscreen fragment shader (`shader_src`).
fn build_depth_resolve_pipeline_from(
    device: &wgpu::Device,
    depth_format: wgpu::TextureFormat,
    shader_src: &str,
    sample_count: u32,
) -> DepthResolvePipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Depth Resolve Shader"),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Depth Resolve BGL"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2,
                // Must match the shader's binding: a multisampled source depth
                // (`texture_depth_multisampled_2d`) for 2×/4×, or a plain
                // single-sample `texture_depth_2d` at 1× (passthrough copy).
                multisampled: sample_count > 1,
            },
            count: None,
        }],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Depth Resolve Pipeline Layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Depth Resolve Pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            // Depth-only pass — the fragment writes `@builtin(frag_depth)` and no
            // color attachment.
            targets: &[],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: depth_format,
            depth_write_enabled: Some(true),
            // Always overwrite: this pass owns the destination depth buffer.
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    DepthResolvePipeline { pipeline, bgl }
}

/// `DomeLight` environment-background (skybox) pipeline.
///
/// Draws the equirect environment as the viewport backdrop inside the MSAA PBR
/// pass, *after* all opaque geometry and *before* transparent geometry. Group 0 =
/// [`SkyboxUniforms`] (binding 0) + the shared env texture / sampler /
/// [`EnvUniforms`] (bindings 1–3). It renders at the far plane with
/// `depth_compare = LessEqual` and no depth write, so it fills only the pixels no
/// opaque surface covered (early-Z rejects the rest) while transparent surfaces
/// still blend over it.
#[allow(missing_debug_implementations)]
pub struct SkyboxPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
}

/// Compiles the skybox pipeline. `color_format` must be the MSAA HDR target format
/// ([`HDR_FORMAT`]); `depth_format` and MSAA sample count must match the PBR pass.
#[must_use]
pub fn build_skybox_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    sample_count: u32,
) -> SkyboxPipeline {
    use crate::surface_gen::SKYBOX_WGSL_SHADER;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Skybox Shader"),
        source: wgpu::ShaderSource::Wgsl(SKYBOX_WGSL_SHADER.into()),
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Skybox BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Skybox Pipeline Layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Skybox Pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        // Far-plane fill, no depth write. Drawn AFTER all opaque geometry, so the
        // depth buffer already holds opaque depth; `LessEqual` vs the skybox's
        // constant far-plane depth (1.0) passes only where nothing opaque was
        // written (depth still == cleared 1.0), letting hardware early-Z reject
        // the equirect fragment shader on every opaque-covered pixel. Transparent
        // geometry is drawn afterwards and blends over the surviving sky.
        depth_stencil: Some(wgpu::DepthStencilState {
            format: depth_format,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    });

    SkyboxPipeline { pipeline, bgl }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// Three wireframe pipeline variants — no transparent distinction since wireframe
/// ignores opacity. Requires `wgpu::Features::POLYGON_MODE_LINE`.
#[derive(Debug)]
pub struct WireframeVariants {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    pub mirrored_single_sided: wgpu::RenderPipeline,
}

impl WireframeVariants {
    #[must_use]
    pub fn select(&self, double_sided: bool, mirrored: bool) -> &wgpu::RenderPipeline {
        if double_sided {
            &self.double_sided
        } else if mirrored {
            &self.mirrored_single_sided
        } else {
            &self.single_sided
        }
    }
}

/// Wireframe pipeline set for all three mesh types (static, skinned, instanced).
/// Present when the GPU adapter supports `wgpu::Features::POLYGON_MODE_LINE`.
#[derive(Debug)]
pub struct WireframePipelines {
    pub stat: WireframeVariants,
    pub skin: WireframeVariants,
    pub inst: WireframeVariants,
    /// Static wireframe variants whose group 3 is `material_udim_bgl` (array
    /// textures + `UdimTable`). Selected for UDIM meshes so the UDIM material
    /// bind group stays layout-compatible in wireframe mode.
    pub stat_udim: WireframeVariants,
    /// Instanced wireframe variants whose group 3 is `material_udim_bgl`.
    /// Selected for UDIM instanced batches.
    pub inst_udim: WireframeVariants,
}

/// Six hardware-instancing pipeline variants compiled from [`PBR_INSTANCED_WGSL_SHADER`].
///
/// Shares all four bind-group layouts with [`PbrPipeline`] so existing camera,
/// lights, mesh, and material bind groups remain compatible.  The model matrix
/// is supplied per instance via vertex-buffer slot 1 (`InstanceRaw`, step-mode=Instance)
/// instead of the `MeshUniforms` uniform — group 2 is bound but ignored by the
/// instanced vertex shader, receiving a shared identity-matrix dummy buffer.
#[derive(Debug)]
pub struct InstancingPipelines {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    pub mirrored_single_sided: wgpu::RenderPipeline,
    pub transparent_single_sided: wgpu::RenderPipeline,
    pub transparent_double_sided: wgpu::RenderPipeline,
    pub transparent_mirrored_single_sided: wgpu::RenderPipeline,
    /// Colored-glass transparent variants (per-channel constant dst blend). See
    /// [`PbrPipeline::transparent_colored_single_sided`].
    pub transparent_colored_single_sided: wgpu::RenderPipeline,
    pub transparent_colored_double_sided: wgpu::RenderPipeline,
    pub transparent_colored_mirrored_single_sided: wgpu::RenderPipeline,
}

impl InstancingPipelines {
    /// Selects the right instancing pipeline variant.
    #[must_use]
    pub fn select(
        &self,
        double_sided: bool,
        mirrored: bool,
        transparent: bool,
    ) -> &wgpu::RenderPipeline {
        match (double_sided, mirrored, transparent) {
            (true, _, false) => &self.double_sided,
            (true, _, true) => &self.transparent_double_sided,
            (false, true, false) => &self.mirrored_single_sided,
            (false, true, true) => &self.transparent_mirrored_single_sided,
            (false, false, false) => &self.single_sided,
            (false, false, true) => &self.transparent_single_sided,
        }
    }

    /// Selects the colored-glass transparent variant (constant dst blend).
    #[must_use]
    pub fn select_colored(&self, double_sided: bool, mirrored: bool) -> &wgpu::RenderPipeline {
        match (double_sided, mirrored) {
            (true, _) => &self.transparent_colored_double_sided,
            (false, true) => &self.transparent_colored_mirrored_single_sided,
            (false, false) => &self.transparent_colored_single_sided,
        }
    }
}

/// Three depth-only cull variants for one geometry class in the camera Z-prepass
/// (QI-4). Each reuses the matching **beauty** shader module's `vs_main` with
/// `fragment: None`, so the clip-space depth is bit-identical to the beauty pass —
/// the prerequisite for the beauty pass's `LessEqual`/depth-write-off opaque test to
/// not drop frontmost fragments. Cull/front-face mirror the beauty variants so a
/// double-sided mesh's back faces still receive prepass depth.
#[derive(Debug)]
pub struct DepthPrepassVariants {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    pub mirrored_single_sided: wgpu::RenderPipeline,
}

impl DepthPrepassVariants {
    /// Selects the prepass variant matching a mesh's beauty-pass rasterization.
    #[must_use]
    pub fn select(&self, double_sided: bool, mirrored: bool) -> &wgpu::RenderPipeline {
        if double_sided {
            &self.double_sided
        } else if mirrored {
            &self.mirrored_single_sided
        } else {
            &self.single_sided
        }
    }
}

/// Camera depth Z-prepass pipelines (QI-4), present only when the prepass is enabled
/// ([`resolve_z_prepass`]). One [`DepthPrepassVariants`] per geometry class:
/// - `stat`: static per-mesh **and** UDIM meshes **and** world-baked merge batches
///   (all share `PBR_WGSL_SHADER` `vs_main` and bind group 2 = a `MeshUniforms`).
/// - `inst`: hardware-instanced batches (`PBR_INSTANCED_WGSL_SHADER` `vs_main`,
///   per-instance model on vertex slot 1).
/// - `skin`: GPU-skinned meshes (`PBR_SKINNED_WGSL_SHADER` `vs_main`, joint palette at
///   group 2).
#[derive(Debug)]
pub struct DepthPrepassPipelines {
    pub stat: DepthPrepassVariants,
    pub inst: DepthPrepassVariants,
    pub skin: DepthPrepassVariants,
}

/// Compiled PBR render pipelines and their bind-group layouts.
///
/// Six pipeline variants share identical bind-group layouts — three opaque (depth-write ON)
/// and three transparent (depth-write OFF, drawn after opaques in back-to-front order):
/// - `single_sided` / `transparent_single_sided`: back-face culling, CCW front face.
///   Rationale: `look_at_rh` (proper rotation, det = +1) maps a USD right-handed (CCW-in-world)
///   exterior to CCW in framebuffer (Y-down). `FrontFace::Ccw` shows the correct exterior.
/// - `double_sided` / `transparent_double_sided`: culling disabled.
/// - `mirrored_single_sided` / `transparent_mirrored_single_sided`: back-face culling, CW front face.
///   A world transform with det < 0 (odd number of negative-scale axes) reverses winding in
///   world space. The CCW-world exterior becomes CW-world, which after `look_at_rh` arrives CW
///   in framebuffer. `FrontFace::Cw` is therefore required to show the exterior.
#[derive(Debug)]
pub struct PbrPipeline {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    /// For meshes whose world transform has a negative determinant (mirrored geometry).
    pub mirrored_single_sided: wgpu::RenderPipeline,
    /// Transparent variants — depth-write disabled; blended over opaques back-to-front.
    pub transparent_single_sided: wgpu::RenderPipeline,
    pub transparent_double_sided: wgpu::RenderPipeline,
    pub transparent_mirrored_single_sided: wgpu::RenderPipeline,
    /// Colored-glass transparent variants — identical to the transparent variants
    /// except the color target uses a per-channel constant dst factor
    /// (`out = src + dst · blend_constant`) so the background is tinted by the
    /// material's `transmission_color`. The constant is supplied per draw via
    /// [`wgpu::RenderPass::set_blend_constant`]. Selected only for materials where
    /// [`RenderMaterialDesc::has_colored_transmission`] is true.
    pub transparent_colored_single_sided: wgpu::RenderPipeline,
    pub transparent_colored_double_sided: wgpu::RenderPipeline,
    pub transparent_colored_mirrored_single_sided: wgpu::RenderPipeline,
    pub camera_bgl: wgpu::BindGroupLayout,
    pub lights_bgl: wgpu::BindGroupLayout,
    pub mesh_bgl: wgpu::BindGroupLayout,
    pub material_bgl: wgpu::BindGroupLayout,
    /// Sibling group-3 layout for UDIM materials: same slots as `material_bgl`
    /// but textures are `D2Array` and binding 8 adds the `UdimTable` uniform.
    pub material_udim_bgl: wgpu::BindGroupLayout,
    /// Clamp-address sampler for UDIM `local_uv` so per-tile fractional coords do
    /// not bleed across the wrap edge (the shared `material` sampler repeats).
    pub udim_sampler: wgpu::Sampler,
    /// Six UDIM pipeline variants (mirrors the base variants, group 3 = `material_udim_bgl`).
    pub udim: UdimPipelines,
    /// Six hardware-instancing UDIM pipeline variants — the instanced vertex stage
    /// (per-instance model matrix from slot 1) combined with the UDIM fragment stage
    /// (array textures + `UdimTable` at group 3). Selected when an instanced batch's
    /// material references a `<UDIM>` template.
    pub udim_instanced: UdimPipelines,
    /// Six skinned pipeline variants (two-slot vertex buffer + group 2 joint matrices).
    pub skin: SkinPipelines,
    /// Bind group layout for the joint matrix palette (group 2 in skinned pipelines, vertex stage).
    /// Replaces `mesh_bgl` at group 2 — skinned meshes derive world-space entirely from joint matrices.
    pub skin_bgl: wgpu::BindGroupLayout,
    /// Six hardware-instancing pipeline variants (two-slot vertex buffer: geometry + instance matrix).
    /// Group 2 (`mesh_bgl`) is bound but not read — the per-instance model matrix comes from
    /// vertex-buffer slot 1 (`InstanceRaw`).
    pub instancing: InstancingPipelines,
    /// Depth-only shadow caster pipeline. Renders geometry from a light's point
    /// of view into one `Depth32Float` atlas layer. Layout `[camera_bgl, mesh_bgl]`
    /// (group 0 = light view-proj in a `CameraUniforms` slot, group 1 = per-mesh
    /// model) — two groups, no index gap, and the existing per-mesh `mesh_bg` is
    /// reused verbatim so casters need no new GPU resources. Front-face culling +
    /// slope-scaled depth bias tame self-shadow acne.
    pub shadow_depth: wgpu::RenderPipeline,
    /// Depth-only shadow caster pipeline for hardware-instanced batches (Phase 4).
    /// Layout `[camera_bgl]` (group 0 only); the per-instance world matrix rides
    /// vertex-buffer slot 1 (`InstanceRaw`). Same front-cull + slope-bias as
    /// `shadow_depth`.
    pub shadow_depth_instanced: wgpu::RenderPipeline,
    /// Depth-only shadow caster pipeline for GPU-skinned meshes (Phase 4).
    /// Layout `[camera_bgl, skin_bgl]` (joint palette at group 1, not group 2 as in
    /// the beauty pass — the depth pass has no lights group). Vertex slot 1 carries
    /// the `SkinVertex` influences.
    pub shadow_depth_skinned: wgpu::RenderPipeline,
    /// Wireframe pipeline set (requires `PolygonMode::Line` device feature).
    /// `None` when the adapter does not support that feature.
    pub wireframe: Option<WireframePipelines>,
    /// Camera depth Z-prepass pipelines (QI-4). `Some` only when the prepass is
    /// enabled ([`resolve_z_prepass`]); `None` leaves the beauty opaque pipelines in
    /// their standard `Less` + depth-write-on state (no prepass).
    pub depth_prepass: Option<DepthPrepassPipelines>,
}

/// Six GPU skeletal-skinning pipeline variants compiled from [`PBR_SKINNED_WGSL_SHADER`].
///
/// Shares bind-group layouts 0–3 with [`PbrPipeline`] except group 2 is `skin_bgl`
/// (joint matrix palette) instead of `mesh_bgl` (model transform).
/// Total bind groups: 4 (groups 0–3), within the WebGPU limit of 4.
#[derive(Debug)]
pub struct SkinPipelines {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    pub mirrored_single_sided: wgpu::RenderPipeline,
    pub transparent_single_sided: wgpu::RenderPipeline,
    pub transparent_double_sided: wgpu::RenderPipeline,
    pub transparent_mirrored_single_sided: wgpu::RenderPipeline,
}

impl SkinPipelines {
    /// Selects the right skinned pipeline variant for a given mesh state.
    #[must_use]
    pub fn select(
        &self,
        double_sided: bool,
        mirrored: bool,
        transparent: bool,
    ) -> &wgpu::RenderPipeline {
        match (double_sided, mirrored, transparent) {
            (true, _, false) => &self.double_sided,
            (true, _, true) => &self.transparent_double_sided,
            (false, true, false) => &self.mirrored_single_sided,
            (false, true, true) => &self.transparent_mirrored_single_sided,
            (false, false, false) => &self.single_sided,
            (false, false, true) => &self.transparent_single_sided,
        }
    }
}

/// Compile the PBR pipelines for the given colour format.
///
/// When `polygon_mode_line` is `true` (requires the GPU adapter to have
/// `wgpu::Features::POLYGON_MODE_LINE` enabled on the device), wireframe
/// pipeline variants are also compiled and stored in `PbrPipeline::wireframe`.
///
/// When `z_prepass` is `true` (QI-4; see [`resolve_z_prepass`]) the opaque beauty
/// variants are built with depth-write **off** and compare `LessEqual` (a camera
/// Z-prepass has already written the frontmost depth), and the depth-only prepass
/// pipelines are compiled into `PbrPipeline::depth_prepass`. When `false` the opaque
/// variants keep the standard depth-write-on / `Less` state and `depth_prepass` is `None`.
#[must_use]
pub fn build_pbr_pipeline(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    polygon_mode_line: bool,
    sample_count: u32,
    z_prepass: bool,
) -> PbrPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("PBR Shader"),
        source: wgpu::ShaderSource::Wgsl(PBR_WGSL_SHADER.into()),
    });

    let uniform_bgle = |binding: u32, vis: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };

    // Group 0: camera
    let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Camera BGL"),
        entries: &[uniform_bgle(
            0,
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        )],
    });

    // Group 1: lights + shadow atlas. Shadow resources fold into this group
    // rather than a 5th bind group — the PBR layout already uses all four groups
    // (max_bind_groups = 4). Bindings 1–3 are bound to a 1×1 dummy depth + a
    // shadows-off `ShadowUniforms` when the frame runs shadow-free, so every
    // pipeline sharing this layout stays valid regardless of tier. The instanced
    // and skinned shaders declare only binding 0; using a subset of a layout's
    // bindings is legal, so they bind the same group untouched (Phase 4 wires
    // their shadow sampling).
    let lights_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Lights BGL"),
        entries: &[
            uniform_bgle(0, wgpu::ShaderStages::FRAGMENT),
            // binding 1: shadow atlas — Depth32Float D2Array, comparison-sampled.
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            // binding 2: comparison sampler (Less) driving the PCF depth compares.
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
            // binding 3: ShadowUniforms (caster→slot map + filter config).
            uniform_bgle(3, wgpu::ShaderStages::FRAGMENT),
            // binding 4: DomeLight environment equirect (Float D2) — IBL diffuse + specular.
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // binding 5: filtering sampler for the environment texture.
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // binding 6: EnvUniforms (SH coeffs + world→env rotation + ambient + params).
            uniform_bgle(6, wgpu::ShaderStages::FRAGMENT),
        ],
    });

    // Group 2: mesh (model + normal matrix)
    let mesh_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Mesh BGL"),
        entries: &[uniform_bgle(
            0,
            wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        )],
    });

    // Group 3: sampler + 6 textures + material uniforms
    let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let material_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Material BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            tex_entry(1),                                  // diffuse
            tex_entry(2),                                  // normal
            tex_entry(3),                                  // metallic
            tex_entry(4),                                  // roughness
            tex_entry(5),                                  // occlusion
            tex_entry(6),                                  // emissive
            uniform_bgle(7, wgpu::ShaderStages::FRAGMENT), // MaterialUniforms
        ],
    });

    // Group 3 (UDIM sibling): identical slots but array textures + UdimTable at 8.
    let tex_array_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    };
    let material_udim_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Material UDIM BGL"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            tex_array_entry(1),                            // diffuse
            tex_array_entry(2),                            // normal
            tex_array_entry(3),                            // metallic
            tex_array_entry(4),                            // roughness
            tex_array_entry(5),                            // occlusion
            tex_array_entry(6),                            // emissive
            uniform_bgle(7, wgpu::ShaderStages::FRAGMENT), // MaterialUniforms
            uniform_bgle(8, wgpu::ShaderStages::FRAGMENT), // UdimTable
        ],
    });

    // Clamp sampler for UDIM: per-tile local_uv must not wrap across tile edges.
    let udim_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("UDIM Sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("PBR Pipeline Layout"),
        bind_group_layouts: &[
            Some(&camera_bgl),
            Some(&lights_bgl),
            Some(&mesh_bgl),
            Some(&material_bgl),
        ],
        immediate_size: 0,
    });

    let color_target = wgpu::ColorTargetState {
        format: color_format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    };

    // Opaque depth policy. Without a Z-prepass: write ON + `Less` (classic depth
    // buffer). With the QI-4 Z-prepass active, the prepass has already written the
    // frontmost opaque depth, so the beauty opaque pass tests `LessEqual` with
    // write OFF — each pixel/sample is shaded ~once (overdraw removed). `LessEqual`
    // (not `Equal`) tolerates any FP slack; the prepass reuses the beauty `vs_main`
    // so depths match, and the beauty pass loads (not clears) the prepass depth.
    let (opaque_depth_write, opaque_depth_compare) = if z_prepass {
        (false, wgpu::CompareFunction::LessEqual)
    } else {
        (true, wgpu::CompareFunction::Less)
    };

    let opaque_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(opaque_depth_write),
        depth_compare: Some(opaque_depth_compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    // Transparent pass: depth-test ON (correct occlusion by opaques) but depth-write
    // OFF so overlapping transparent fragments don't occlude each other by z-fighting.
    let transparent_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    let make_pipeline = |cull_mode: Option<wgpu::Face>,
                         front_face: wgpu::FrontFace,
                         depth: wgpu::DepthStencilState,
                         label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face,
                cull_mode,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };

    // look_at_rh (det=+1) + viewport Y-flip → exterior arrives CCW in framebuffer.
    // FrontFace::Ccw = exterior visible; mirrored (extra winding flip) → FrontFace::Cw.
    let single_sided = make_pipeline(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Ccw,
        opaque_depth.clone(),
        "PBR Single-Sided",
    );
    let double_sided = make_pipeline(
        None,
        wgpu::FrontFace::Ccw,
        opaque_depth.clone(),
        "PBR Double-Sided",
    );
    let mirrored_single_sided = make_pipeline(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Cw,
        opaque_depth,
        "PBR Mirrored Single-Sided",
    );

    let transparent_single_sided = make_pipeline(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Ccw,
        transparent_depth.clone(),
        "PBR Transparent Single-Sided",
    );
    let transparent_double_sided = make_pipeline(
        None,
        wgpu::FrontFace::Ccw,
        transparent_depth.clone(),
        "PBR Transparent Double-Sided",
    );
    let transparent_mirrored_single_sided = make_pipeline(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Cw,
        transparent_depth,
        "PBR Transparent Mirrored Single-Sided",
    );

    // Colored-glass transparent variants: identical geometry/depth handling to the
    // transparent variants above, but the color target multiplies the destination
    // (background) by a per-draw blend constant (transmission_color · (1-opacity))
    // instead of the scalar premultiplied (1-src.a). The blend constant is set per
    // draw via RenderPass::set_blend_constant. Same shader module and layout — only
    // the color target's blend state differs.
    let colored_color_target = wgpu::ColorTargetState {
        format: color_format,
        blend: Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::Constant,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::Constant,
                operation: wgpu::BlendOperation::Add,
            },
        }),
        write_mask: wgpu::ColorWrites::ALL,
    };
    let colored_transparent_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let make_colored_pipeline = |cull_mode: Option<wgpu::Face>,
                                 front_face: wgpu::FrontFace,
                                 depth: wgpu::DepthStencilState,
                                 label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(colored_color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face,
                cull_mode,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };
    let transparent_colored_single_sided = make_colored_pipeline(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Ccw,
        colored_transparent_depth.clone(),
        "PBR Transparent Colored Single-Sided",
    );
    let transparent_colored_double_sided = make_colored_pipeline(
        None,
        wgpu::FrontFace::Ccw,
        colored_transparent_depth.clone(),
        "PBR Transparent Colored Double-Sided",
    );
    let transparent_colored_mirrored_single_sided = make_colored_pipeline(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Cw,
        colored_transparent_depth,
        "PBR Transparent Colored Mirrored Single-Sided",
    );

    // ── Skinned pipeline variants ──────────────────────────────────────────
    // Group 2 (skinned): joint matrix palette (256 × mat4x4<f32> = 16 384 bytes).
    // Replaces mesh_bgl at group 2 — world-space comes from joint matrices, not MeshUniforms.
    let skin_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("Skin BGL"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });

    let skin_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Skinned PBR Pipeline Layout"),
        bind_group_layouts: &[
            Some(&camera_bgl),
            Some(&lights_bgl),
            Some(&skin_bgl), // group 2 = joint matrices (replaces mesh uniforms for skinned meshes)
            Some(&material_bgl),
        ],
        immediate_size: 0,
    });

    let skin_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("PBR Skinned Shader"),
        source: wgpu::ShaderSource::Wgsl(PBR_SKINNED_WGSL_SHADER.into()),
    });

    let opaque_depth_skin = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(opaque_depth_write),
        depth_compare: Some(opaque_depth_compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let transparent_depth_skin = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    let make_skin_pipeline = |cull_mode: Option<wgpu::Face>,
                              front_face: wgpu::FrontFace,
                              depth: wgpu::DepthStencilState,
                              label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&skin_layout),
            vertex: wgpu::VertexState {
                module: &skin_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout(), SkinVertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &skin_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face,
                cull_mode,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };

    let skin = SkinPipelines {
        single_sided: make_skin_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            opaque_depth_skin.clone(),
            "Skin PBR Single-Sided",
        ),
        double_sided: make_skin_pipeline(
            None,
            wgpu::FrontFace::Ccw,
            opaque_depth_skin.clone(),
            "Skin PBR Double-Sided",
        ),
        mirrored_single_sided: make_skin_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            opaque_depth_skin,
            "Skin PBR Mirrored Single-Sided",
        ),
        transparent_single_sided: make_skin_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            transparent_depth_skin.clone(),
            "Skin PBR Transparent Single-Sided",
        ),
        transparent_double_sided: make_skin_pipeline(
            None,
            wgpu::FrontFace::Ccw,
            transparent_depth_skin.clone(),
            "Skin PBR Transparent Double-Sided",
        ),
        transparent_mirrored_single_sided: make_skin_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            transparent_depth_skin,
            "Skin PBR Transparent Mirrored Single-Sided",
        ),
    };

    // ── Instancing pipeline variants ───────────────────────────────────────
    // Vertex shader reads the per-instance model matrix from slot 1 (InstanceRaw,
    // step-mode=Instance) instead of MeshUniforms.  The same BGL as the standard
    // pipeline is used — group 2 is bound to a shared identity-matrix dummy buffer.
    let instanced_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("PBR Instanced Shader"),
        source: wgpu::ShaderSource::Wgsl(PBR_INSTANCED_WGSL_SHADER.into()),
    });

    let opaque_depth_inst = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(opaque_depth_write),
        depth_compare: Some(opaque_depth_compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let transparent_depth_inst = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    let make_instancing_pipeline = |cull_mode: Option<wgpu::Face>,
                                    front_face: wgpu::FrontFace,
                                    depth: wgpu::DepthStencilState,
                                    label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &instanced_shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout(), InstanceRaw::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &instanced_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face,
                cull_mode,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };

    // Colored-glass instanced variants: instanced vertex stage + constant-dst blend
    // color target (reusing `colored_color_target` from the base build above).
    let colored_transparent_depth_inst = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let make_instancing_colored_pipeline =
        |cull_mode: Option<wgpu::Face>,
         front_face: wgpu::FrontFace,
         depth: wgpu::DepthStencilState,
         label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &instanced_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Vertex::layout(), InstanceRaw::layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &instanced_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(colored_color_target.clone())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face,
                    cull_mode,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(depth),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };

    let instancing = InstancingPipelines {
        single_sided: make_instancing_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            opaque_depth_inst.clone(),
            "PBR Instanced Single-Sided",
        ),
        double_sided: make_instancing_pipeline(
            None,
            wgpu::FrontFace::Ccw,
            opaque_depth_inst.clone(),
            "PBR Instanced Double-Sided",
        ),
        mirrored_single_sided: make_instancing_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            opaque_depth_inst,
            "PBR Instanced Mirrored Single-Sided",
        ),
        transparent_single_sided: make_instancing_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            transparent_depth_inst.clone(),
            "PBR Instanced Transparent Single-Sided",
        ),
        transparent_double_sided: make_instancing_pipeline(
            None,
            wgpu::FrontFace::Ccw,
            transparent_depth_inst.clone(),
            "PBR Instanced Transparent Double-Sided",
        ),
        transparent_mirrored_single_sided: make_instancing_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            transparent_depth_inst,
            "PBR Instanced Transparent Mirrored Single-Sided",
        ),
        transparent_colored_single_sided: make_instancing_colored_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            colored_transparent_depth_inst.clone(),
            "PBR Instanced Transparent Colored Single-Sided",
        ),
        transparent_colored_double_sided: make_instancing_colored_pipeline(
            None,
            wgpu::FrontFace::Ccw,
            colored_transparent_depth_inst.clone(),
            "PBR Instanced Transparent Colored Double-Sided",
        ),
        transparent_colored_mirrored_single_sided: make_instancing_colored_pipeline(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            colored_transparent_depth_inst,
            "PBR Instanced Transparent Colored Mirrored Single-Sided",
        ),
    };

    // ── Wireframe pipeline variants ────────────────────────────────────────
    // Compiled only when the adapter supports PolygonMode::Line.
    let wireframe = if polygon_mode_line {
        let wf_depth = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let wf_color_target = wgpu::ColorTargetState {
            format: color_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        };

        let wf_stat_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Wireframe Static Shader"),
            source: wgpu::ShaderSource::Wgsl(WIREFRAME_WGSL_SHADER.into()),
        });
        // Group 3 layout for the UDIM path: array textures + UdimTable. The
        // wireframe shader only reads `MaterialUniforms` at binding 7 (present in
        // both layouts), but wgpu requires the bound bind group's full layout to
        // match the pipeline's — so UDIM meshes need pipelines built against it.
        let wf_udim_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Wireframe UDIM Pipeline Layout"),
            bind_group_layouts: &[
                Some(&camera_bgl),
                Some(&lights_bgl),
                Some(&mesh_bgl),
                Some(&material_udim_bgl),
            ],
            immediate_size: 0,
        });
        let make_wf_stat = |pl: &wgpu::PipelineLayout,
                            cull: Option<wgpu::Face>,
                            ff: wgpu::FrontFace,
                            label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(pl),
                vertex: wgpu::VertexState {
                    module: &wf_stat_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Vertex::layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &wf_stat_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wf_color_target.clone())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: ff,
                    cull_mode: cull,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Line,
                    conservative: false,
                },
                depth_stencil: Some(wf_depth.clone()),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let stat = WireframeVariants {
            single_sided: make_wf_stat(
                &layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "WF Static Single-Sided",
            ),
            double_sided: make_wf_stat(
                &layout,
                None,
                wgpu::FrontFace::Ccw,
                "WF Static Double-Sided",
            ),
            mirrored_single_sided: make_wf_stat(
                &layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "WF Static Mirrored Single-Sided",
            ),
        };
        let stat_udim = WireframeVariants {
            single_sided: make_wf_stat(
                &wf_udim_layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "WF Static UDIM Single-Sided",
            ),
            double_sided: make_wf_stat(
                &wf_udim_layout,
                None,
                wgpu::FrontFace::Ccw,
                "WF Static UDIM Double-Sided",
            ),
            mirrored_single_sided: make_wf_stat(
                &wf_udim_layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "WF Static UDIM Mirrored Single-Sided",
            ),
        };

        let wf_skin_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Wireframe Skinned Shader"),
            source: wgpu::ShaderSource::Wgsl(WIREFRAME_SKINNED_WGSL_SHADER.into()),
        });
        let make_wf_skin = |cull: Option<wgpu::Face>, ff: wgpu::FrontFace, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&skin_layout),
                vertex: wgpu::VertexState {
                    module: &wf_skin_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Vertex::layout(), SkinVertex::layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &wf_skin_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wf_color_target.clone())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: ff,
                    cull_mode: cull,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Line,
                    conservative: false,
                },
                depth_stencil: Some(wf_depth.clone()),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let skin_wf = WireframeVariants {
            single_sided: make_wf_skin(
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "WF Skinned Single-Sided",
            ),
            double_sided: make_wf_skin(None, wgpu::FrontFace::Ccw, "WF Skinned Double-Sided"),
            mirrored_single_sided: make_wf_skin(
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "WF Skinned Mirrored Single-Sided",
            ),
        };

        let wf_inst_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Wireframe Instanced Shader"),
            source: wgpu::ShaderSource::Wgsl(WIREFRAME_INSTANCED_WGSL_SHADER.into()),
        });
        let make_wf_inst = |pl: &wgpu::PipelineLayout,
                            cull: Option<wgpu::Face>,
                            ff: wgpu::FrontFace,
                            label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(pl),
                vertex: wgpu::VertexState {
                    module: &wf_inst_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[Vertex::layout(), InstanceRaw::layout()],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &wf_inst_shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wf_color_target.clone())],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: ff,
                    cull_mode: cull,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Line,
                    conservative: false,
                },
                depth_stencil: Some(wf_depth.clone()),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let inst_wf = WireframeVariants {
            single_sided: make_wf_inst(
                &layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "WF Instanced Single-Sided",
            ),
            double_sided: make_wf_inst(
                &layout,
                None,
                wgpu::FrontFace::Ccw,
                "WF Instanced Double-Sided",
            ),
            mirrored_single_sided: make_wf_inst(
                &layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "WF Instanced Mirrored Single-Sided",
            ),
        };
        let inst_udim = WireframeVariants {
            single_sided: make_wf_inst(
                &wf_udim_layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "WF Instanced UDIM Single-Sided",
            ),
            double_sided: make_wf_inst(
                &wf_udim_layout,
                None,
                wgpu::FrontFace::Ccw,
                "WF Instanced UDIM Double-Sided",
            ),
            mirrored_single_sided: make_wf_inst(
                &wf_udim_layout,
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "WF Instanced UDIM Mirrored Single-Sided",
            ),
        };

        Some(WireframePipelines {
            stat,
            skin: skin_wf,
            inst: inst_wf,
            stat_udim,
            inst_udim,
        })
    } else {
        None
    };

    let udim = build_udim_pipelines(
        device,
        color_format,
        &camera_bgl,
        &lights_bgl,
        &mesh_bgl,
        &material_udim_bgl,
        sample_count,
        z_prepass,
    );

    let udim_instanced = build_udim_instanced_pipelines(
        device,
        color_format,
        &camera_bgl,
        &lights_bgl,
        &mesh_bgl,
        &material_udim_bgl,
        sample_count,
        z_prepass,
    );

    // ── Shadow depth-only pipeline ─────────────────────────────────────────
    // Two-group layout [camera_bgl (= light view-proj), mesh_bgl (= model)].
    // Single-sampled (the atlas is not MSAA), front-face culled with a
    // slope-scaled polygon offset — rendering back faces into the depth map is
    // the standard acne fix for closed casters. The vertex shader reads only
    // `@location(0)` from the shared `Vertex` layout; the fragment stage is
    // omitted (`fragment: None`) since only depth is written.
    let shadow_depth_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Shadow Depth Pipeline Layout"),
        bind_group_layouts: &[Some(&camera_bgl), Some(&mesh_bgl)],
        immediate_size: 0,
    });
    let shadow_depth_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shadow Depth Shader"),
        source: wgpu::ShaderSource::Wgsl(PBR_SHADOW_DEPTH_WGSL.into()),
    });
    let shadow_depth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Shadow Depth"),
        layout: Some(&shadow_depth_layout),
        vertex: wgpu::VertexState {
            module: &shadow_depth_shader,
            entry_point: Some("vs_main"),
            buffers: &[Vertex::layout()],
            compilation_options: Default::default(),
        },
        fragment: None,
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Front),
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            // Slope-scaled + constant polygon offset: the primary acne guard.
            bias: wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 2.0,
                clamp: 0.0,
            },
        }),
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    });

    // Instanced + skinned depth-only caster pipelines (Phase 4). They share the
    // shadow atlas format, front-cull, and slope-scaled bias with `shadow_depth`;
    // only the vertex sourcing differs (per-instance matrix / joint palette).
    let shadow_depth_primitive = wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleList,
        strip_index_format: None,
        front_face: wgpu::FrontFace::Ccw,
        cull_mode: Some(wgpu::Face::Front),
        unclipped_depth: false,
        polygon_mode: wgpu::PolygonMode::Fill,
        conservative: false,
    };
    let shadow_depth_stencil = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState {
            constant: 2,
            slope_scale: 2.0,
            clamp: 0.0,
        },
    };
    let shadow_depth_multisample = wgpu::MultisampleState {
        count: 1,
        mask: !0,
        alpha_to_coverage_enabled: false,
    };

    // Instanced: group 0 only; the world matrix rides slot-1 InstanceRaw.
    let shadow_depth_instanced_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Shadow Depth Instanced Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bgl)],
            immediate_size: 0,
        });
    let shadow_depth_instanced_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shadow Depth Instanced Shader"),
        source: wgpu::ShaderSource::Wgsl(PBR_SHADOW_DEPTH_INSTANCED_WGSL.into()),
    });
    let shadow_depth_instanced = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Shadow Depth Instanced"),
        layout: Some(&shadow_depth_instanced_layout),
        vertex: wgpu::VertexState {
            module: &shadow_depth_instanced_shader,
            entry_point: Some("vs_main"),
            buffers: &[Vertex::layout(), InstanceRaw::layout()],
            compilation_options: Default::default(),
        },
        fragment: None,
        primitive: shadow_depth_primitive,
        depth_stencil: Some(shadow_depth_stencil.clone()),
        multisample: shadow_depth_multisample,
        multiview_mask: None,
        cache: None,
    });

    // Skinned: group 0 = light view-proj, group 1 = joint palette (skin_bgl); slot-1
    // SkinVertex carries the influences.
    let shadow_depth_skinned_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Shadow Depth Skinned Pipeline Layout"),
            bind_group_layouts: &[Some(&camera_bgl), Some(&skin_bgl)],
            immediate_size: 0,
        });
    let shadow_depth_skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shadow Depth Skinned Shader"),
        source: wgpu::ShaderSource::Wgsl(PBR_SHADOW_DEPTH_SKINNED_WGSL.into()),
    });
    let shadow_depth_skinned = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Shadow Depth Skinned"),
        layout: Some(&shadow_depth_skinned_layout),
        vertex: wgpu::VertexState {
            module: &shadow_depth_skinned_shader,
            entry_point: Some("vs_main"),
            buffers: &[Vertex::layout(), SkinVertex::layout()],
            compilation_options: Default::default(),
        },
        fragment: None,
        primitive: shadow_depth_primitive,
        depth_stencil: Some(shadow_depth_stencil),
        multisample: shadow_depth_multisample,
        multiview_mask: None,
        cache: None,
    });

    // ── Camera Z-prepass pipelines (QI-4) ──────────────────────────────────
    // Depth-only passes that reproduce the beauty `vs_main` clip-space math (so the
    // depth is bit-identical and the beauty opaque `LessEqual`/write-off test above
    // never drops frontmost fragments). They reuse the two-group shadow pipeline
    // *layouts* (`[camera, mesh]` / `[camera]` / `[camera, skin]`) — not the 4-group
    // beauty layout — so no lights/material bind group is needed and UDIM meshes
    // (whose material group differs) share the static prepass. `fragment: None`,
    // write ON + `Less` (sole depth writer), no bias, MSAA at the beauty
    // `sample_count`, back-face cull per variant mirroring the beauty rasterization.
    // Built only when `z_prepass`; `None` otherwise (desktop/PCVR pay nothing).
    let depth_prepass = if z_prepass {
        let prepass_depth = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let prepass_stat_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Z-Prepass Static Shader"),
            source: wgpu::ShaderSource::Wgsl(PBR_DEPTH_PREPASS_WGSL.into()),
        });
        let prepass_inst_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Z-Prepass Instanced Shader"),
            source: wgpu::ShaderSource::Wgsl(PBR_DEPTH_PREPASS_INSTANCED_WGSL.into()),
        });
        let make_prepass = |pl: &wgpu::PipelineLayout,
                            module: &wgpu::ShaderModule,
                            buffers: &[wgpu::VertexBufferLayout<'_>],
                            cull_mode: Option<wgpu::Face>,
                            front_face: wgpu::FrontFace,
                            label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(pl),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: None,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face,
                    cull_mode,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(prepass_depth.clone()),
                multisample: wgpu::MultisampleState {
                    count: sample_count,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };
        // Static per-mesh / UDIM / world-baked merge batches: `[camera, mesh]` layout,
        // single vertex buffer; mesh bind group binds at group 1.
        let stat = DepthPrepassVariants {
            single_sided: make_prepass(
                &shadow_depth_layout,
                &prepass_stat_shader,
                &[Vertex::layout()],
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "ZPrepass Static Single-Sided",
            ),
            double_sided: make_prepass(
                &shadow_depth_layout,
                &prepass_stat_shader,
                &[Vertex::layout()],
                None,
                wgpu::FrontFace::Ccw,
                "ZPrepass Static Double-Sided",
            ),
            mirrored_single_sided: make_prepass(
                &shadow_depth_layout,
                &prepass_stat_shader,
                &[Vertex::layout()],
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "ZPrepass Static Mirrored Single-Sided",
            ),
        };
        // Hardware-instanced batches: `[camera]` layout, per-instance model on slot 1.
        let inst = DepthPrepassVariants {
            single_sided: make_prepass(
                &shadow_depth_instanced_layout,
                &prepass_inst_shader,
                &[Vertex::layout(), InstanceRaw::layout()],
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "ZPrepass Instanced Single-Sided",
            ),
            double_sided: make_prepass(
                &shadow_depth_instanced_layout,
                &prepass_inst_shader,
                &[Vertex::layout(), InstanceRaw::layout()],
                None,
                wgpu::FrontFace::Ccw,
                "ZPrepass Instanced Double-Sided",
            ),
            mirrored_single_sided: make_prepass(
                &shadow_depth_instanced_layout,
                &prepass_inst_shader,
                &[Vertex::layout(), InstanceRaw::layout()],
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "ZPrepass Instanced Mirrored Single-Sided",
            ),
        };
        // GPU-skinned meshes: `[camera, skin]` layout; reuses the shadow skinned
        // depth shader, whose joint-blend math already matches the beauty `vs_main`.
        let skin_pp = DepthPrepassVariants {
            single_sided: make_prepass(
                &shadow_depth_skinned_layout,
                &shadow_depth_skinned_shader,
                &[Vertex::layout(), SkinVertex::layout()],
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Ccw,
                "ZPrepass Skinned Single-Sided",
            ),
            double_sided: make_prepass(
                &shadow_depth_skinned_layout,
                &shadow_depth_skinned_shader,
                &[Vertex::layout(), SkinVertex::layout()],
                None,
                wgpu::FrontFace::Ccw,
                "ZPrepass Skinned Double-Sided",
            ),
            mirrored_single_sided: make_prepass(
                &shadow_depth_skinned_layout,
                &shadow_depth_skinned_shader,
                &[Vertex::layout(), SkinVertex::layout()],
                Some(wgpu::Face::Back),
                wgpu::FrontFace::Cw,
                "ZPrepass Skinned Mirrored Single-Sided",
            ),
        };
        Some(DepthPrepassPipelines {
            stat,
            inst,
            skin: skin_pp,
        })
    } else {
        None
    };

    PbrPipeline {
        single_sided,
        double_sided,
        mirrored_single_sided,
        transparent_single_sided,
        transparent_double_sided,
        transparent_mirrored_single_sided,
        transparent_colored_single_sided,
        transparent_colored_double_sided,
        transparent_colored_mirrored_single_sided,
        camera_bgl,
        lights_bgl,
        mesh_bgl,
        material_bgl,
        material_udim_bgl,
        udim_sampler,
        udim,
        udim_instanced,
        skin,
        skin_bgl,
        instancing,
        shadow_depth,
        shadow_depth_instanced,
        shadow_depth_skinned,
        wireframe,
        depth_prepass,
    }
}

// ── UDIM per-material pipelines (core; not feature-gated) ─────────────────────

/// Six UDIM pipeline variants compiled from [`crate::surface_gen::pbr_udim_wgsl`].
///
/// Layout is `[camera, lights, mesh, material_udim]` — group 3 uses the sibling
/// `material_udim_bgl` (array textures + `UdimTable`); groups 0–2 are shared with
/// the base pipeline so their bind groups stay valid when a UDIM variant is drawn.
#[derive(Debug)]
pub struct UdimPipelines {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    pub mirrored_single_sided: wgpu::RenderPipeline,
    pub transparent_single_sided: wgpu::RenderPipeline,
    pub transparent_double_sided: wgpu::RenderPipeline,
    pub transparent_mirrored_single_sided: wgpu::RenderPipeline,
}

impl UdimPipelines {
    /// Selects the right UDIM pipeline variant for a given mesh state.
    #[must_use]
    pub fn select(
        &self,
        double_sided: bool,
        mirrored: bool,
        transparent: bool,
    ) -> &wgpu::RenderPipeline {
        match (double_sided, mirrored, transparent) {
            (true, _, false) => &self.double_sided,
            (true, _, true) => &self.transparent_double_sided,
            (false, true, false) => &self.mirrored_single_sided,
            (false, true, true) => &self.transparent_mirrored_single_sided,
            (false, false, false) => &self.single_sided,
            (false, false, true) => &self.transparent_single_sided,
        }
    }
}

/// Compiles the six UDIM pipeline variants. Reuses the camera/lights/mesh BGLs so
/// their bind groups remain valid; group 3 binds `material_udim_bgl`.
#[allow(clippy::too_many_arguments)] // internal builder; split only called from build_pbr_pipeline
#[must_use]
pub fn build_udim_pipelines(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    lights_bgl: &wgpu::BindGroupLayout,
    mesh_bgl: &wgpu::BindGroupLayout,
    material_udim_bgl: &wgpu::BindGroupLayout,
    sample_count: u32,
    z_prepass: bool,
) -> UdimPipelines {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("PBR UDIM Shader"),
        source: wgpu::ShaderSource::Wgsl(crate::surface_gen::pbr_udim_wgsl().into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("PBR UDIM Pipeline Layout"),
        bind_group_layouts: &[
            Some(camera_bgl),
            Some(lights_bgl),
            Some(mesh_bgl),
            Some(material_udim_bgl),
        ],
        immediate_size: 0,
    });

    let color_target = wgpu::ColorTargetState {
        format: color_format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    };
    // With the QI-4 Z-prepass active the opaque UDIM variants test `LessEqual` /
    // write-off (the prepass handles UDIM meshes via the shared static prepass
    // pipeline, since UDIM `vs_main` matches the static clip-space transform).
    let (opaque_depth_write, opaque_depth_compare) = if z_prepass {
        (false, wgpu::CompareFunction::LessEqual)
    } else {
        (true, wgpu::CompareFunction::Less)
    };
    let opaque_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(opaque_depth_write),
        depth_compare: Some(opaque_depth_compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let transparent_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    let make = |cull: Option<wgpu::Face>,
                ff: wgpu::FrontFace,
                depth: wgpu::DepthStencilState,
                label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: ff,
                cull_mode: cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };

    UdimPipelines {
        single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            opaque_depth.clone(),
            "UDIM Single-Sided",
        ),
        double_sided: make(
            None,
            wgpu::FrontFace::Ccw,
            opaque_depth.clone(),
            "UDIM Double-Sided",
        ),
        mirrored_single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            opaque_depth,
            "UDIM Mirrored Single-Sided",
        ),
        transparent_single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            transparent_depth.clone(),
            "UDIM Transparent Single-Sided",
        ),
        transparent_double_sided: make(
            None,
            wgpu::FrontFace::Ccw,
            transparent_depth.clone(),
            "UDIM Transparent Double-Sided",
        ),
        transparent_mirrored_single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            transparent_depth,
            "UDIM Transparent Mirrored Single-Sided",
        ),
    }
}

/// Compiles the six hardware-instancing UDIM pipeline variants from
/// [`crate::surface_gen::pbr_udim_instanced_wgsl`].
///
/// Identical to [`build_udim_pipelines`] except the vertex stage adds the
/// per-instance model-matrix vertex buffer (slot 1, `InstanceRaw`, step-mode
/// Instance). The pipeline layout is `[camera, lights, mesh, material_udim]` —
/// group 2 (`mesh_bgl`) is bound but not read (the shared identity dummy buffer),
/// matching the non-UDIM instancing pipelines so the same bind groups stay valid.
#[allow(clippy::too_many_arguments)] // internal builder; split only called from build_pbr_pipeline
#[must_use]
pub fn build_udim_instanced_pipelines(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    camera_bgl: &wgpu::BindGroupLayout,
    lights_bgl: &wgpu::BindGroupLayout,
    mesh_bgl: &wgpu::BindGroupLayout,
    material_udim_bgl: &wgpu::BindGroupLayout,
    sample_count: u32,
    z_prepass: bool,
) -> UdimPipelines {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("PBR UDIM Instanced Shader"),
        source: wgpu::ShaderSource::Wgsl(crate::surface_gen::pbr_udim_instanced_wgsl().into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("PBR UDIM Instanced Pipeline Layout"),
        bind_group_layouts: &[
            Some(camera_bgl),
            Some(lights_bgl),
            Some(mesh_bgl),
            Some(material_udim_bgl),
        ],
        immediate_size: 0,
    });

    let color_target = wgpu::ColorTargetState {
        format: color_format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    };
    // Opaque UDIM-instanced variants follow the shared Z-prepass policy (QI-4):
    // their depth is written by the instanced prepass pipeline.
    let (opaque_depth_write, opaque_depth_compare) = if z_prepass {
        (false, wgpu::CompareFunction::LessEqual)
    } else {
        (true, wgpu::CompareFunction::Less)
    };
    let opaque_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(opaque_depth_write),
        depth_compare: Some(opaque_depth_compare),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let transparent_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    let make = |cull: Option<wgpu::Face>,
                ff: wgpu::FrontFace,
                depth: wgpu::DepthStencilState,
                label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout(), InstanceRaw::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: ff,
                cull_mode: cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };

    UdimPipelines {
        single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            opaque_depth.clone(),
            "UDIM Instanced Single-Sided",
        ),
        double_sided: make(
            None,
            wgpu::FrontFace::Ccw,
            opaque_depth.clone(),
            "UDIM Instanced Double-Sided",
        ),
        mirrored_single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            opaque_depth,
            "UDIM Instanced Mirrored Single-Sided",
        ),
        transparent_single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Ccw,
            transparent_depth.clone(),
            "UDIM Instanced Transparent Single-Sided",
        ),
        transparent_double_sided: make(
            None,
            wgpu::FrontFace::Ccw,
            transparent_depth.clone(),
            "UDIM Instanced Transparent Double-Sided",
        ),
        transparent_mirrored_single_sided: make(
            Some(wgpu::Face::Back),
            wgpu::FrontFace::Cw,
            transparent_depth,
            "UDIM Instanced Transparent Mirrored Single-Sided",
        ),
    }
}

// ── MaterialX per-material pipelines (feature = "materialx-wgsl") ─────────────

#[cfg(feature = "materialx-wgsl")]
/// Six pipeline variants compiled from MaterialX-generated WGSL.
///
/// Shares the same bind-group layout as the parent `PbrPipeline` so existing
/// bind groups remain valid when this pipeline is selected.
#[allow(missing_debug_implementations)]
pub struct MaterialxPipelines {
    pub single_sided: wgpu::RenderPipeline,
    pub double_sided: wgpu::RenderPipeline,
    pub mirrored_single_sided: wgpu::RenderPipeline,
    pub transparent_single_sided: wgpu::RenderPipeline,
    pub transparent_double_sided: wgpu::RenderPipeline,
    pub transparent_mirrored_single_sided: wgpu::RenderPipeline,
}

#[cfg(feature = "materialx-wgsl")]
impl MaterialxPipelines {
    /// Selects the right pipeline variant for a given mesh state.
    #[must_use]
    pub fn select(
        &self,
        double_sided: bool,
        mirrored: bool,
        transparent: bool,
    ) -> &wgpu::RenderPipeline {
        match (double_sided, mirrored, transparent) {
            (true, _, false) => &self.double_sided,
            (true, _, true) => &self.transparent_double_sided,
            (false, true, false) => &self.mirrored_single_sided,
            (false, true, true) => &self.transparent_mirrored_single_sided,
            (false, false, false) => &self.single_sided,
            (false, false, true) => &self.transparent_single_sided,
        }
    }
}

#[cfg(feature = "materialx-wgsl")]
/// Tries to compile 6 pipeline variants from MaterialX-generated WGSL source.
///
/// Reuses the bind-group layouts from `pbr` so existing bind groups are
/// compatible with the returned pipelines. Returns `None` when the WGSL fails
/// validation or pipeline creation fails — the caller must fall back to `pbr`.
#[must_use]
pub fn try_build_materialx_pipelines(
    device: &wgpu::Device,
    color_format: wgpu::TextureFormat,
    pbr: &PbrPipeline,
    wgsl_source: &str,
    sample_count: u32,
) -> Option<MaterialxPipelines> {
    // Stable key for dump filenames — hash the raw (pre-patch) source.
    let key: u64 = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        wgsl_source.hash(&mut h);
        h.finish()
    };

    // MaterialX WGSL (via naga transpile) is fragment-only: no vs_main entry point
    // and no vertex TBN inputs. Full pipeline integration requires a dedicated vertex
    // shader and a new bind group layout matching the MaterialX uniform blocks.
    // Until that work is done, skip wgpu compilation and fall back to PBR.
    if !wgsl_source.contains("fn vs_main") {
        eprintln!(
            "idtx-render: MaterialX WGSL for key {key:#x} is fragment-only \
             (no vs_main) — pipeline integration pending (vertex TBN shader + \
             bind group redesign required); using PBR uber-shader"
        );
        return None;
    }

    // Apply post-processing to fix known MaterialX generator bugs (e.g. bool in
    // uniform structs) before handing the source to wgpu.
    let (patched_src, patch_diags) = crate::wgsl_patch::patch_materialx_wgsl(wgsl_source);
    for diag in &patch_diags {
        eprintln!("idtx-render: {diag}");
    }

    // Capture any WGSL validation errors without propagating a panic.
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("MaterialX Shader"),
        source: wgpu::ShaderSource::Wgsl(patched_src.as_str().into()),
    });
    let compile_error = pollster::block_on(scope.pop());
    if let Some(err) = compile_error {
        eprintln!("idtx-render: WGSL shader module validation error (key {key:#x}): {err}");
        crate::wgsl_patch::maybe_dump_wgsl(key, "fail", &patched_src);
        return None;
    }

    // Reuse the layouts from the main PBR pipeline so bind groups created
    // from pbr.{camera,lights,mesh,material}_bgl remain compatible here.
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("MaterialX Pipeline Layout"),
        bind_group_layouts: &[
            Some(&pbr.camera_bgl),
            Some(&pbr.lights_bgl),
            Some(&pbr.mesh_bgl),
            Some(&pbr.material_bgl),
        ],
        immediate_size: 0,
    });

    let color_target = wgpu::ColorTargetState {
        format: color_format,
        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        write_mask: wgpu::ColorWrites::ALL,
    };
    let opaque_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let transparent_depth = wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth32Float,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };

    // Push a second error scope to catch pipeline creation failures.
    let pipeline_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let make = |cull: Option<wgpu::Face>,
                ff: wgpu::FrontFace,
                depth: wgpu::DepthStencilState,
                label: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(color_target.clone())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: ff,
                cull_mode: cull,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(depth),
            multisample: wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        })
    };

    let single_sided = make(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Ccw,
        opaque_depth.clone(),
        "MtlX Single-Sided",
    );
    let double_sided = make(
        None,
        wgpu::FrontFace::Ccw,
        opaque_depth.clone(),
        "MtlX Double-Sided",
    );
    let mirrored_single_sided = make(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Cw,
        opaque_depth,
        "MtlX Mirrored Single-Sided",
    );
    let transparent_single_sided = make(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Ccw,
        transparent_depth.clone(),
        "MtlX Transparent Single-Sided",
    );
    let transparent_double_sided = make(
        None,
        wgpu::FrontFace::Ccw,
        transparent_depth.clone(),
        "MtlX Transparent Double-Sided",
    );
    let transparent_mirrored_single_sided = make(
        Some(wgpu::Face::Back),
        wgpu::FrontFace::Cw,
        transparent_depth,
        "MtlX Transparent Mirrored Single-Sided",
    );

    let pipeline_error = pollster::block_on(pipeline_scope.pop());
    if let Some(err) = pipeline_error {
        eprintln!("idtx-render: WGSL pipeline creation error (key {key:#x}): {err}");
        crate::wgsl_patch::maybe_dump_wgsl(key, "pipeline_fail", &patched_src);
        return None;
    }

    Some(MaterialxPipelines {
        single_sided,
        double_sided,
        mirrored_single_sided,
        transparent_single_sided,
        transparent_double_sided,
        transparent_mirrored_single_sided,
    })
}

#[cfg(test)]
mod tests {
    use super::{clamp_sample_count_to_support, sanitize_sample_count};

    #[test]
    fn sanitize_accepts_supported_counts() {
        // The three MSAA counts the renderer supports pass through unchanged,
        // regardless of the fallback default.
        for n in [1u32, 2, 4] {
            assert_eq!(sanitize_sample_count(Some(n), 4), n);
            assert_eq!(sanitize_sample_count(Some(n), 2), n);
        }
    }

    #[test]
    fn sanitize_rejects_unsupported_and_falls_back() {
        // Anything outside {1,2,4} — including the common-but-unsupported 8× and
        // nonsense like 0/3 — falls back to the supplied platform default.
        for bad in [0u32, 3, 5, 6, 8, 16, 1000] {
            assert_eq!(sanitize_sample_count(Some(bad), 4), 4);
            assert_eq!(sanitize_sample_count(Some(bad), 2), 2);
        }
    }

    #[test]
    fn sanitize_none_is_default() {
        // No request (env unset) → the platform default is used verbatim.
        assert_eq!(sanitize_sample_count(None, 4), 4);
        assert_eq!(sanitize_sample_count(None, 2), 2);
        assert_eq!(sanitize_sample_count(None, 1), 1);
    }

    #[test]
    fn clamp_keeps_supported_count() {
        // Device supports everything → request passes through.
        let all = |_n: u32| true;
        assert_eq!(clamp_sample_count_to_support(1, all), 1);
        assert_eq!(clamp_sample_count_to_support(2, all), 2);
        assert_eq!(clamp_sample_count_to_support(4, all), 4);
    }

    #[test]
    fn clamp_steps_2x_up_to_4x_when_unsupported() {
        // The core-only guarantee: {1,4} supported, 2× not (no adapter feature).
        let core = |n: u32| n == 1 || n == 4;
        assert_eq!(clamp_sample_count_to_support(2, core), 4);
        // 1× and 4× still pass through untouched.
        assert_eq!(clamp_sample_count_to_support(1, core), 1);
        assert_eq!(clamp_sample_count_to_support(4, core), 4);
    }

    #[test]
    fn clamp_falls_back_to_1x_when_only_1x() {
        // Degenerate device advertising only single-sample.
        let only1 = |n: u32| n == 1;
        assert_eq!(clamp_sample_count_to_support(2, only1), 1);
        assert_eq!(clamp_sample_count_to_support(4, only1), 1);
        assert_eq!(clamp_sample_count_to_support(1, only1), 1);
    }
}
