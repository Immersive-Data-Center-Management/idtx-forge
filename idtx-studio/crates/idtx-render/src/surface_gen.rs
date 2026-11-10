//! Material surface shader generation.
//!
//! [`SurfaceGenerator`] is the abstract interface for producing WGSL source for
//! a given material IR.  [`UsdPbrGenerator`] is the concrete Phase 4
//! implementation: a single Cook-Torrance PBR shader that handles up to 6
//! common texture channels (base-color, normal, metallic, roughness, occlusion,
//! emissive) and 8 scene lights (distant, sphere, rect, dome).
//!
//! For Phase 4 the shader source is constant (a single variant handles all
//! feature combinations via a `features` bitmask uniform).  Future phases will
//! add a `MaterialX` → GLSL → naga → WGSL transpile path and an optional C++
//! direct WGSL generator path.

use std::fmt::Write;

/// Produces WGSL source and a pipeline cache key for a given material.
pub trait SurfaceGenerator: Send + Sync + 'static {
    /// Generate the full WGSL shader source for the given feature bitmask.
    ///
    /// The caller caches compiled pipelines by `pipeline_key`; only call this
    /// when the cache misses.
    fn generate_wgsl(&self, feature_bits: u64) -> &'static str;

    /// Stable key used to cache the compiled `wgpu::RenderPipeline`.
    ///
    /// Two calls with identical `feature_bits` must return the same key.
    ///
    /// The default implementation hashes the generated WGSL source so that any
    /// generator producing unique WGSL per material automatically gets distinct
    /// pipeline cache entries without needing to override this method.
    ///
    /// Override when the key can be computed more cheaply — `UsdPbrGenerator`
    /// always returns `0` because its WGSL is a single compile-time constant.
    ///
    /// # Descriptor-state note
    ///
    /// This key encodes the *shader source* only. Descriptor-state differences
    /// (blend mode, culling, depth-write) are not encoded here. Pipeline caches
    /// that store per-descriptor variants (e.g. `MaterialxPipelines`, which
    /// bundles six winding/transparency variants per WGSL source) already handle
    /// this; a cache that stores a single pipeline per key must additionally
    /// combine this value with a descriptor-state hash to avoid collisions.
    fn pipeline_key(&self, feature_bits: u64) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.generate_wgsl(feature_bits).hash(&mut h);
        h.finish()
    }
}

// ── UsdPbrGenerator ───────────────────────────────────────────────────────────

/// Phase 4 PBR generator.
///
/// Produces a single universal Cook-Torrance shader that branches on a
/// `features` bitmask uniform at runtime.  Pipeline key is always 0 so the
/// caller compiles exactly one pipeline for all materials.
#[derive(Debug)]
pub struct UsdPbrGenerator;

impl SurfaceGenerator for UsdPbrGenerator {
    fn generate_wgsl(&self, _feature_bits: u64) -> &'static str {
        PBR_WGSL_SHADER
    }

    /// Returns `0` unconditionally.
    ///
    /// `UsdPbrGenerator` produces a single universal shader that handles all
    /// feature combinations via the `features` bitmask uniform at runtime.
    /// There is exactly one compiled pipeline for all materials using this
    /// generator, so a constant key is correct and avoids hashing the shader
    /// source on every cache lookup.
    fn pipeline_key(&self, _feature_bits: u64) -> u64 {
        0
    }
}

// ── PBR WGSL Shader ───────────────────────────────────────────────────────────

/// Cook-Torrance PBR shader (GGX NDF, Smith-Schlick G, Schlick F).
///
/// # Bind group layout
///
/// | Group | Binding | Type              | Description                  |
/// |-------|---------|-------------------|------------------------------|
/// | 0     | 0       | uniform           | `CameraUniforms`             |
/// | 1     | 0       | uniform           | `LightsUniforms`             |
/// | 2     | 0       | uniform           | `MeshUniforms`               |
/// | 3     | 0       | sampler           | bilinear + repeat             |
/// | 3     | 1       | `texture_2d`\<f32\> | base-color / diffuse (sRGB)  |
/// | 3     | 2       | `texture_2d`\<f32\> | normal map (linear)          |
/// | 3     | 3       | `texture_2d`\<f32\> | metallic (linear, channel R) |
/// | 3     | 4       | `texture_2d`\<f32\> | roughness (linear, channel R)|
/// | 3     | 5       | `texture_2d`\<f32\> | occlusion (linear, channel R)|
/// | 3     | 6       | `texture_2d`\<f32\> | emissive (sRGB)              |
/// | 3     | 7       | uniform           | `MaterialUniforms`           |
///
/// # UV convention
///
/// All mesh UVs are pre-flipped (V = 1 − `V_usd`) so that V=0 corresponds to the
/// top-left corner of the image, matching the wgpu texture coordinate origin.
/// Normal-map bitangent sign is negated when V is flipped to preserve the
/// correct handedness of the derivative-based TBN frame.
///
/// # Normal map / TBN
///
/// When `FEAT_NORMAL` is set the shader reconstructs a per-pixel normal via a
/// TBN matrix.  The TBN source is selected per-fragment:
/// - **Vertex TBN** (preferred): when `tangent.w != 0.0`, the authored per-vertex
///   tangent is interpolated from the vertex stage and Gram-Schmidt re-orthogonalised
///   against the interpolated normal.  `tangent.w` encodes the bitangent handedness sign.
/// - **Screen-space fallback**: when `tangent.w == 0.0` (no authored tangent), TBN is
///   reconstructed from `dpdx`/`dpdy` of world position and UV (derivative-based TBN).
///
/// The Z component of the tangent-space normal is reconstructed from X and Y:
/// `z = sqrt(1 − x² − y²)`.
///
/// # Double-sided rendering
///
/// The `@builtin(front_facing)` input flips the geometric normal for back-facing
/// fragments so normal maps and lighting are consistent on both sides of a
/// double-sided mesh.
pub const PBR_WGSL_SHADER: &str = r"
// ── Constants ──────────────────────────────────────────────────────────────────
const PI          : f32 = 3.14159265358979323846;
const MAX_LIGHTS  : u32 = 16u;

// Feature bitmask (mirrors MaterialFeatures in idtx-usd)
const FEAT_BASE_COLOR : u32 = 1u;
const FEAT_NORMAL     : u32 = 2u;
const FEAT_METALLIC   : u32 = 4u;
const FEAT_ROUGHNESS  : u32 = 8u;
const FEAT_OCCLUSION  : u32 = 16u;
const FEAT_EMISSIVE       : u32 = 32u;
const FEAT_DIFFUSE_ALPHA  : u32 = 64u;
const FEAT_BLEND          : u32 = 128u;

// Light kind enum
const LIGHT_DISTANT   : u32 = 0u;
const LIGHT_SPHERE    : u32 = 1u;
const LIGHT_RECT      : u32 = 2u;
const LIGHT_DOME      : u32 = 3u;
const LIGHT_DOME_PROC : u32 = 4u;
const LIGHT_DISK      : u32 = 5u;

// ── Uniform structs ────────────────────────────────────────────────────────────
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

struct LightGpu {
    kind      : u32,
    _pad0     : u32,  // explicit scalars keep WGSL layout == Rust #[repr(C)] layout (64 bytes)
    _pad1     : u32,
    _pad2     : u32,
    dir_or_pos: vec4<f32>,  // distant: world-dir toward light; sphere/rect: world-pos
    color_pwr : vec4<f32>,  // rgb = linear colour, w = power (intensity * 2^exposure)
    size      : vec4<f32>,  // x = radius (sphere/disk) or width; y = height (rect); z = diffuse mult; w = specular mult
}

struct LightsUniforms {
    lights : array<LightGpu, 16>,
    count  : u32,
    _pad0  : u32,
    _pad1  : u32,
    _pad2  : u32,
}

struct ShadowUniforms {
    // Per-slot light-space view-projection (world → shadow NDC).
    light_view_proj : array<mat4x4<f32>, 4>,
    // Per-slot params: x = source light index, y = near, z = far, w = light size.
    params          : array<vec4<f32>, 4>,
    // x = tier (0=Off, 1=Smooth/PCF, 2=Soft/PCSS, 3=Hard), y = atlas resolution, z = active slot count,
    // w = constant receiver depth bias.
    global          : vec4<f32>,
}

// UsdLuxDomeLight environment IBL (group 1). `sh` holds 9 order-2 spherical-
// harmonic radiance coefficients (rgb in xyz); `rot0..2` are the columns of the
// world→environment rotation mat3; `ambient` is the dome's flat radiance; and
// `params` = (max_mip, has_env, has_texture, _).
struct EnvUniforms {
    sh      : array<vec4<f32>, 9>,
    rot0    : vec4<f32>,
    rot1    : vec4<f32>,
    rot2    : vec4<f32>,
    ambient : vec4<f32>,
    params  : vec4<f32>,
}

struct MeshUniforms {
    model    : mat4x4<f32>,
    nrm_col0 : vec4<f32>,   // column 0 of transpose(inverse(model)) upper-left 3x3
    nrm_col1 : vec4<f32>,   // column 1
    nrm_col2 : vec4<f32>,   // column 2
}

struct MaterialUniforms {
    base_color : vec4<f32>,        // rgb = scalar base colour, a = scalar opacity
    mr_sub_thr : vec4<f32>,        // x=metallic, y=roughness, z=subsurface, w=opacity_threshold
    emissive   : vec3<f32>,
    features   : u32,              // FEAT_* bitmask
    channels   : vec4<u32>,        // metallic_ch, roughness_ch, occlusion_ch, (unused)
}

// ── Bindings ──────────────────────────────────────────────────────────────────
@group(0) @binding(0) var<uniform> camera       : CameraUniforms;
@group(1) @binding(0) var<uniform> lights       : LightsUniforms;
@group(1) @binding(1) var          shadow_atlas : texture_depth_2d_array;
@group(1) @binding(2) var          shadow_samp  : sampler_comparison;
@group(1) @binding(3) var<uniform> shadows      : ShadowUniforms;
@group(1) @binding(4) var          env_tex      : texture_2d<f32>;
@group(1) @binding(5) var          env_sampler  : sampler;
@group(1) @binding(6) var<uniform> env          : EnvUniforms;
@group(2) @binding(0) var<uniform> mesh         : MeshUniforms;
@group(3) @binding(0) var          mat_sampler  : sampler;
@group(3) @binding(1) var          tex_diffuse  : texture_2d<f32>;
@group(3) @binding(2) var          tex_normal   : texture_2d<f32>;
@group(3) @binding(3) var          tex_metallic : texture_2d<f32>;
@group(3) @binding(4) var          tex_roughness: texture_2d<f32>;
@group(3) @binding(5) var          tex_occlusion: texture_2d<f32>;
@group(3) @binding(6) var          tex_emissive : texture_2d<f32>;
@group(3) @binding(7) var<uniform> mat          : MaterialUniforms;

// ── Shadow sampling (Phase 3: 3×3 PCF hard shadow; Phase 5: PCSS soft) ───────
const SHADOW_BUDGET_WGSL : u32 = 4u;
// Floor for shadow visibility: a fully-occluded receiver keeps this fraction of
// the light's direct contribution instead of going pitch black. 0.3 ⇒ deepest
// shadow is 70% darker, not 100%. Set to 0.0 for physically hard black shadows.
const SHADOW_MIN_VISIBILITY : f32 = 0.3;

// PCSS — percentage-closer soft shadows. Blocker search → similar-triangles
// penumbra → variable-radius PCF. The atlas is a bounding-sphere ortho fit whose
// depth range (far−near) equals its lateral frustum width (both = 2·half_extent),
// so converting a world-space penumbra to atlas-UV cancels the half-extent and
// reduces to `light_size × (receiver_z − avg_blocker_z)`, with `light_size` the
// caster light's angular size in radians (`params[slot].w`).
fn sample_shadow_pcss(slot: u32, uv: vec2<f32>, recv_z: f32, ref_depth: f32) -> f32 {
    let res        = max(shadows.global.y, 1.0);
    let texel      = 1.0 / res;
    let light_size = shadows.params[slot].w;
    let dim        = i32(res);
    let base       = vec2<i32>(i32(uv.x * res), i32(uv.y * res));

    // 1. Blocker search: mean depth of occluders nearer the light than the
    //    receiver, read raw (non-comparison) via textureLoad over a 7×7 kernel.
    var blk_sum : f32 = 0.0;
    var blk_cnt : f32 = 0.0;
    let search : i32 = 3;
    for (var dy : i32 = -search; dy <= search; dy = dy + 1) {
        for (var dx : i32 = -search; dx <= search; dx = dx + 1) {
            let c = base + vec2<i32>(dx, dy);
            if c.x < 0 || c.y < 0 || c.x >= dim || c.y >= dim { continue; }
            let d = textureLoad(shadow_atlas, c, i32(slot), 0);
            if d < recv_z - 0.0008 {
                blk_sum = blk_sum + d;
                blk_cnt = blk_cnt + 1.0;
            }
        }
    }
    if blk_cnt < 0.5 { return 1.0; }                        // no blocker → fully lit
    let avg_blk = blk_sum / blk_cnt;

    // 2. Penumbra half-width in UV (see header) → filter radius in texels.
    let penumbra_uv = light_size * max(recv_z - avg_blk, 0.0);
    let radius      = clamp(penumbra_uv * res, 1.0, 16.0);

    // 3. Variable-radius PCF: a fixed 5×5 grid stretched to the penumbra radius.
    let step = radius * texel * 0.5;
    var sum : f32 = 0.0;
    for (var dy : i32 = -2; dy <= 2; dy = dy + 1) {
        for (var dx : i32 = -2; dx <= 2; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * step;
            sum = sum + textureSampleCompareLevel(shadow_atlas, shadow_samp, uv + off, slot, ref_depth);
        }
    }
    return sum / 25.0;
}

// Light visibility in [0,1] for light `light_idx` at `world_pos` (1.0 = fully lit).
// The Off tier and any light without an atlas slot short-circuit to 1.0, so the
// call is safe to apply to every non-dome light (only casters carry a slot).
fn shadow_visibility(light_idx: u32, world_pos: vec3<f32>, ndotl: f32) -> f32 {
    if shadows.global.x < 0.5 { return 1.0; }              // Off tier
    let slot_count = u32(shadows.global.z + 0.5);
    var slot : u32 = 0xFFFFFFFFu;
    for (var s : u32 = 0u; s < slot_count && s < SHADOW_BUDGET_WGSL; s = s + 1u) {
        if u32(shadows.params[s].x + 0.5) == light_idx { slot = s; break; }
    }
    if slot == 0xFFFFFFFFu { return 1.0; }                 // this light casts no shadow

    let clip = shadows.light_view_proj[slot] * vec4<f32>(world_pos, 1.0);
    let ndc  = clip.xyz / clip.w;
    if ndc.z < 0.0 || ndc.z > 1.0 ||
       ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 {
        return 1.0;                                        // outside the shadow frustum
    }
    // NDC → atlas UV (Y flipped: NDC +Y is up, texture V grows downward).
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    // Constant + slope-scaled receiver bias, in shadow-NDC depth units. The atlas
    // stores the nearest occluder (smallest depth, near→0/far→1); a receiver is lit
    // when its own depth is <= the stored occluder (comparison sampler = Less), so
    // biasing toward lit means nudging the reference *smaller*.
    let bias = shadows.global.w * (1.0 + 2.0 * clamp(1.0 - ndotl, 0.0, 1.0));
    let ref_depth = ndc.z - bias;

    // Hard tier (global.x ≈ 3): a single comparison tap — one hardware 2×2
    // bilinear PCF sample. A crisp ~1-texel anti-aliased edge with no 3×3 blur
    // and none of PCSS's variable-radius temporal jitter; 1 tap vs the PCF
    // tier's 9. Reuses the uv/ref_depth/bias computed above.
    if shadows.global.x > 2.5 {
        return textureSampleCompareLevel(shadow_atlas, shadow_samp, uv, slot, ref_depth);
    }

    // PCSS tier (global.x ≈ 2): variable-radius soft shadow.
    // Hard tier (global.x ≈ 3): single comparison tap — see PBR_WGSL_SHADER.
    if shadows.global.x > 2.5 {
        return textureSampleCompareLevel(shadow_atlas, shadow_samp, uv, slot, ref_depth);
    }

    if shadows.global.x > 1.5 {
        return sample_shadow_pcss(slot, uv, ndc.z, ref_depth);
    }

    // PCF tier (global.x ≈ 1): fixed 3×3 hard-ish shadow.
    let texel = 1.0 / max(shadows.global.y, 1.0);
    var sum : f32 = 0.0;
    for (var dy : i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx : i32 = -1; dx <= 1; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompareLevel(shadow_atlas, shadow_samp, uv + off, slot, ref_depth);
        }
    }
    return sum / 9.0;
}

// ── Vertex stage ──────────────────────────────────────────────────────────────
struct VertexInput {
    @location(0) position    : vec3<f32>,
    @location(1) normal      : vec3<f32>,
    @location(2) texcoord    : vec2<f32>,
    @location(3) vertex_color: vec3<f32>,
    @location(4) tangent     : vec4<f32>,  // xyz = tangent dir, w = bitangent sign (0 → SS fallback)
}

struct VertexOutput {
    @builtin(position) clip_pos    : vec4<f32>,
    @location(0)       world_nrm   : vec3<f32>,
    @location(1)       world_pos   : vec3<f32>,
    @location(2)       uv          : vec2<f32>,
    @location(3)       vertex_color: vec3<f32>,
    @location(4)       tangent     : vec4<f32>,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out : VertexOutput;
    let wp4      = mesh.model * vec4<f32>(in.position, 1.0);
    out.clip_pos = camera.view_proj * wp4;
    out.world_pos = wp4.xyz;
    let nm = mat3x3<f32>(
        mesh.nrm_col0.xyz,
        mesh.nrm_col1.xyz,
        mesh.nrm_col2.xyz,
    );
    out.world_nrm = normalize(nm * in.normal);
    out.uv = in.texcoord;
    out.vertex_color = in.vertex_color;
    if in.tangent.w != 0.0 {
        out.tangent = vec4<f32>(normalize(nm * in.tangent.xyz), in.tangent.w);
    } else {
        out.tangent = vec4<f32>(0.0);
    }
    return out;
}

// ── PBR helper functions ──────────────────────────────────────────────────────
fn distribution_ggx(N: vec3<f32>, H: vec3<f32>, roughness: f32) -> f32 {
    let a   = roughness * roughness;
    let a2  = a * a;
    let nh  = max(dot(N, H), 0.0);
    let d   = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

fn geometry_schlick(ndotx: f32, k: f32) -> f32 {
    return ndotx / (ndotx * (1.0 - k) + k);
}

fn geometry_smith(ndotv: f32, ndotl: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) * 0.125;
    return geometry_schlick(max(ndotv, 0.0001), k)
         * geometry_schlick(max(ndotl, 0.0001), k);
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (1.0 - f0) * pow(saturate(1.0 - cos_theta), 5.0);
}

fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let inv_r = 1.0 - roughness;
    return f0 + (max(vec3<f32>(inv_r), f0) - f0) * pow(saturate(1.0 - cos_theta), 5.0);
}

// Karis 2013 split-sum analytical BRDF (scale, bias) — no prefiltered mip or LUT required.
fn env_brdf_approx(roughness: f32, ndotv: f32) -> vec2<f32> {
    let c0   = vec4<f32>(-1.0, -0.0275, -0.572,  0.022);
    let c1   = vec4<f32>( 1.0,  0.0425,  1.04,  -0.04);
    let r    = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * ndotv)) * r.x + r.y;
    return clamp(vec2<f32>(-1.04, 1.04) * a004 + r.zw, vec2<f32>(0.0), vec2<f32>(1.0));
}

// ── DomeLight environment IBL helpers ────────────────────────────────────────
// Rotate a world-space direction into the environment's sampling frame.
fn env_rotate(d: vec3<f32>) -> vec3<f32> {
    let m = mat3x3<f32>(env.rot0.xyz, env.rot1.xyz, env.rot2.xyz);
    return m * d;
}

// Lat-long (equirect) mapping matching the CPU `env_ibl` module: u wraps azimuth
// from -Z, v=0 is the +Y pole.
fn env_equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let u = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let v = acos(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

// Cosine-convolved order-2 SH irradiance E(n) (already π-scaled). `n_world` is a
// world-space normal; the environment rotation is applied here.
fn env_sh_irradiance(n_world: vec3<f32>) -> vec3<f32> {
    let n = normalize(env_rotate(n_world));
    let x = n.x;
    let y = n.y;
    let z = n.z;
    let A0 = PI;
    let A1 = 2.0 * PI / 3.0;
    let A2 = PI / 4.0;
    var e = vec3<f32>(0.0);
    e += env.sh[0].rgb * (A0 * 0.282095);
    e += env.sh[1].rgb * (A1 * 0.488603 * y);
    e += env.sh[2].rgb * (A1 * 0.488603 * z);
    e += env.sh[3].rgb * (A1 * 0.488603 * x);
    e += env.sh[4].rgb * (A2 * 1.092548 * x * y);
    e += env.sh[5].rgb * (A2 * 1.092548 * y * z);
    e += env.sh[6].rgb * (A2 * 0.315392 * (3.0 * z * z - 1.0));
    e += env.sh[7].rgb * (A2 * 1.092548 * x * z);
    e += env.sh[8].rgb * (A2 * 0.546274 * (x * x - y * y));
    return max(e, vec3<f32>(0.0));
}

// Roughness-selected equirect specular sample. `r_world` is a world-space
// reflection vector; roughness selects the prefiltered mip.
fn env_specular(r_world: vec3<f32>, roughness: f32) -> vec3<f32> {
    let uv  = env_equirect_uv(normalize(env_rotate(r_world)));
    let lod = roughness * env.params.x;
    return textureSampleLevel(env_tex, env_sampler, uv, lod).rgb;
}

/// Extract a scalar channel from a sampled vec4 using the channel index uniform.
fn swizzle_ch(v: vec4<f32>, ch: u32) -> f32 {
    if ch == 0u { return v.r; }
    if ch == 1u { return v.g; }
    if ch == 2u { return v.b; }
    return v.a;
}

/// Build a world-space TBN matrix.
///
/// When vtx_tangent.w != 0.0, uses the authored per-vertex tangent (vertex TBN).
/// Gram-Schmidt re-orthogonalises T against the interpolated N to remove drift.
/// vtx_tangent.w encodes the bitangent handedness sign (+1 or −1).
///
/// When vtx_tangent.w == 0.0, falls back to screen-space derivative reconstruction
/// (dpdx/dpdy), with an arbitrary orthonormal fallback for degenerate UVs.
fn compute_tbn(world_pos: vec3<f32>, N: vec3<f32>, uv: vec2<f32>, vtx_tangent: vec4<f32>) -> mat3x3<f32> {
    if vtx_tangent.w != 0.0 {
        var T = normalize(vtx_tangent.xyz - dot(vtx_tangent.xyz, N) * N);
        let B = cross(N, T) * vtx_tangent.w;
        return mat3x3<f32>(T, B, N);
    }
    let dp1  = dpdx(world_pos);
    let dp2  = dpdy(world_pos);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let det  = duv1.x * duv2.y - duv1.y * duv2.x;
    if abs(det) < 1e-7 {
        var up = vec3<f32>(0.0, 1.0, 0.0);
        if abs(N.y) > 0.99 { up = vec3<f32>(1.0, 0.0, 0.0); }
        let T = normalize(cross(up, N));
        let B = cross(N, T);
        return mat3x3<f32>(T, B, N);
    }
    let inv = 1.0 / det;
    var T = (dp1 * duv2.y - dp2 * duv1.y) * inv;
    T = normalize(T - dot(T, N) * N);
    let B = cross(N, T);
    return mat3x3<f32>(T, B, N);
}

// ── Fragment stage ────────────────────────────────────────────────────────────
@fragment
fn fs_main(
    in            : VertexOutput,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let uv = in.uv;

    // Flip geometric normal for back-facing fragments (double-sided meshes).
    let face_sign = select(-1.0, 1.0, front_facing);
    let N_geom    = normalize(in.world_nrm) * face_sign;

    // ── Base colour ───────────────────────────────────────────────────────
    var base_color : vec3<f32> = mat.base_color.rgb;
    var opacity    : f32       = mat.base_color.a;
    if (mat.features & FEAT_BASE_COLOR) != 0u {
        let s   = textureSample(tex_diffuse, mat_sampler, uv);
        base_color = s.rgb * mat.base_color.rgb;
        if (mat.features & FEAT_DIFFUSE_ALPHA) != 0u {
            opacity = s.a;
        }
    }

    // Vertex colour tint: multiplicative blend (white = no-op).
    base_color = base_color * in.vertex_color;

    // Alpha mask (opacity_threshold > 0 triggers cutout mode)
    if mat.mr_sub_thr.w > 0.0 && opacity < mat.mr_sub_thr.w {
        discard;
    }

    // ── Normal mapping ────────────────────────────────────────────────────
    var N : vec3<f32> = N_geom;
    if (mat.features & FEAT_NORMAL) != 0u {
        let samp = textureSample(tex_normal, mat_sampler, uv);
        let xy   = samp.rg * 2.0 - 1.0;
        // Reconstruct Z assuming unit-length tangent-space normal.
        let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));
        let ts_n = normalize(vec3<f32>(xy, z));
        let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);
        N = normalize(TBN * ts_n);
    }

    // ── PBR scalar parameters ──────────────────────────────────────────────
    var metallic  : f32 = mat.mr_sub_thr.x;
    var roughness : f32 = mat.mr_sub_thr.y;

    if (mat.features & FEAT_METALLIC) != 0u {
        metallic = swizzle_ch(
            textureSample(tex_metallic, mat_sampler, uv), mat.channels.x
        );
    }
    if (mat.features & FEAT_ROUGHNESS) != 0u {
        roughness = swizzle_ch(
            textureSample(tex_roughness, mat_sampler, uv), mat.channels.y
        );
    }
    roughness = clamp(roughness, 0.04, 1.0);

    var ao : f32 = 1.0;
    if (mat.features & FEAT_OCCLUSION) != 0u {
        ao = swizzle_ch(
            textureSample(tex_occlusion, mat_sampler, uv), mat.channels.z
        );
    }

    // ── Fresnel F0 (dielectric 0.04, fully metallic → base_color) ────────
    let F0 = mix(vec3<f32>(0.04), base_color, metallic);

    // ── View vector ────────────────────────────────────────────────────────
    let V     = normalize(camera.eye_pos.xyz - in.world_pos);
    let NdotV = max(dot(N, V), 0.0001);

    // ── Light loop ─────────────────────────────────────────────────────────
    var Lo      : vec3<f32> = vec3<f32>(0.0);  // direct diffuse only
    var spec    : vec3<f32> = vec3<f32>(0.0);  // all specular / reflection
    var ambient : vec3<f32> = vec3<f32>(0.0);

    for (var i : u32 = 0u; i < lights.count && i < MAX_LIGHTS; i++) {
        let light = lights.lights[i];

        if light.kind == LIGHT_DOME || light.kind == LIGHT_DOME_PROC {
            // Textured domes are evaluated once from the global env uniform after
            // the loop (SH diffuse + equirect specular). Here only untextured /
            // procedural domes add their flat analytic hemisphere term, driven by
            // the dome's extracted ambient radiance (`color × power`).
            if env.params.z < 0.5 {
                let dome_col = light.color_pwr.rgb * light.color_pwr.w;
                let F_ibl  = fresnel_schlick_roughness(NdotV, F0, roughness);
                let kD_ibl = (vec3<f32>(1.0) - F_ibl) * (1.0 - metallic);
                let sky_t  = 0.5 + 0.5 * N.y;
                ambient += kD_ibl * dome_col * sky_t * (1.0 / PI);
                let ab = env_brdf_approx(roughness, NdotV);
                spec += dome_col * (F0 * ab.x + vec3<f32>(ab.y));
            }
            continue;
        }

        var L        : vec3<f32>;
        var radiance : vec3<f32>;

        if light.kind == LIGHT_DISTANT {
            // dir_or_pos points FROM surface TOWARD the light.
            L        = normalize(light.dir_or_pos.xyz);
            radiance = light.color_pwr.rgb * light.color_pwr.w;
        } else {
            // Sphere and rect: treat as point source with 1/r² attenuation.
            let to_light = light.dir_or_pos.xyz - in.world_pos;
            let dist     = length(to_light);
            L        = to_light * (1.0 / max(dist, 0.0001));
            // Radius-aware inverse-square falloff: clamp the 1/r² singularity by
            // the light's own render-unit size (size.xy), not a fixed scene-scale
            // constant, so brightness is invariant to metersPerUnit. The bare 0.01
            // floor pinned attenuation at 100x for any centimetre-scale stage.
            let soft_r2 = max(light.size.x * light.size.x, light.size.y * light.size.y);
            let atten = 1.0 / max(dist * dist, max(soft_r2, 1e-6));
            radiance  = light.color_pwr.rgb * light.color_pwr.w * atten;
        }

        let NdotL = max(dot(N, L), 0.0);
        if NdotL < 0.0001 { continue; }

        let H = normalize(V + L);

        // Cook-Torrance BRDF
        let D        = distribution_ggx(N, H, roughness);
        let G        = geometry_smith(NdotV, NdotL, roughness);
        let F        = fresnel_schlick(max(dot(H, V), 0.0), F0);
        let specular = (D * G * F) / max(4.0 * NdotV * NdotL, 0.001);

        // Diffuse: Lambertian, energy-conserving (kD = 0 for metals)
        let kD      = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let diffuse = kD * base_color / PI;

        // Per-light diffuse/specular multipliers (UsdLuxLightAPI, size.zw).
        // Shadow visibility (1.0 = lit) folds in per shadow-casting light; a light
        // with no atlas slot and the Off tier both return 1.0 (Phase 3: distant).
        // Lift the fully-occluded floor off pure black (SHADOW_MIN_VISIBILITY); the
        // lit/no-shadow/off paths return exactly 1.0, so mix leaves them untouched.
        let shadow = mix(SHADOW_MIN_VISIBILITY, 1.0, shadow_visibility(i, in.world_pos, NdotL));
        Lo   += diffuse  * light.size.z * radiance * NdotL * shadow;
        spec += specular * light.size.w * radiance * NdotL * shadow;
    }

    // Ambient environment floor: when the stage has no dome/IBL light (ambient
    // is still zero after the light loop, or there are no lights at all), inject
    // a dim neutral environment through the same split-sum diffuse + analytic
    // specular terms a real dome uses. A fully-metallic surface has kD = 0, so
    // the diffuse floor alone leaves it black; the specular term (tinted by F0)
    // lets it reflect the neutral environment instead. For dielectrics kD ≈ 1,
    // so `ambient ≈ 0.03` — the same magnitude as the previous flat floor.
    // Textured DomeLight IBL, applied once from the dominant environment: SH
    // directional diffuse irradiance (E(N), π-scaled) + roughness-mip equirect
    // specular through the same analytic split-sum BRDF the analytic path uses.
    if env.params.y > 0.5 && env.params.z > 0.5 {
        let F_ibl  = fresnel_schlick_roughness(NdotV, F0, roughness);
        let kD_ibl = (vec3<f32>(1.0) - F_ibl) * (1.0 - metallic);
        let ab     = env_brdf_approx(roughness, NdotV);
        // `env.ambient.rgb` = dome color x intensity x 2^exposure — scales the raw
        // texture radiance for both the SH diffuse and the equirect specular.
        let scale  = env.ambient.rgb;
        let irr    = env_sh_irradiance(N) * scale;
        ambient   += kD_ibl * irr * (1.0 / PI);
        let R      = reflect(-V, N);
        // Specular horizon occlusion: fade reflections whose mirror direction dips below
        // the surface horizon (strongest at grazing angles), preventing the environment
        // from leaking onto silhouettes. Frostbite/Lagarde form.
        var horizon = saturate(1.0 + dot(R, N));
        horizon     = horizon * horizon;
        // Split-sum: the env-BRDF LUT (ab) already integrates the Fresnel grazing term,
        // so scale the flat F0 — multiplying F_ibl here would double-count it and make
        // the IBL specular grow without bound as NdotV -> 0.
        spec      += env_specular(R, roughness) * scale * (F0 * ab.x + vec3<f32>(ab.y)) * horizon;
    }

    if lights.count == 0u {
        let amb    = vec3<f32>(0.03);
        let F_amb  = fresnel_schlick_roughness(NdotV, F0, roughness);
        let kD_amb = (vec3<f32>(1.0) - F_amb) * (1.0 - metallic);
        ambient    = kD_amb * amb;
        let ab_amb = env_brdf_approx(roughness, NdotV);
        spec += amb * (F0 * ab_amb.x + vec3<f32>(ab_amb.y));
    }

    // ── Emissive ──────────────────────────────────────────────────────────
    var emissive : vec3<f32> = mat.emissive;
    if (mat.features & FEAT_EMISSIVE) != 0u {
        emissive = textureSample(tex_emissive, mat_sampler, uv).rgb;
    }

    // Split the shaded response into a transmissive part (diffuse + diffuse IBL,
    // tinted by base colour) and a reflective part (all specular/fresnel plus
    // emission). A transparent dielectric still reflects, so the reflective part
    // must survive transmission.
    let diffuse_total  = (ambient * base_color + Lo) * ao;
    let specular_total = spec * ao;
    let reflective     = specular_total + emissive;  // always visible

    var color : vec3<f32>;
    var out_alpha : f32;
    if (mat.features & FEAT_BLEND) != 0u {
        // Transmissive dielectric drawn with PREMULTIPLIED_ALPHA_BLENDING
        // (out = src.rgb + dst·(1−src.a)): emit premultiplied colour whose diffuse
        // body fades with opacity while reflection/emission stay at full strength,
        // and raise coverage by the reflection's luminance so bright specular glints
        // occlude the background instead of vanishing at opacity 0.
        color        = (reflective + diffuse_total * opacity) * camera.exposure;
        let refl_cov = saturate(luminance(reflective * camera.exposure));
        out_alpha    = saturate(opacity + (1.0 - opacity) * refl_cov);
    } else {
        // Opaque / mask: full colour (diffuse + reflection), coverage untouched.
        color     = (diffuse_total + reflective) * camera.exposure;
        out_alpha = opacity;
    }
    return vec4<f32>(color, out_alpha);
}
";

/// Depth-only shadow caster shader (Phase 3).
///
/// Renders geometry from a light's point of view into one `Depth32Float` shadow
/// atlas layer. Bound with the two-group layout `[camera_bgl, mesh_bgl]`: group 0
/// carries the light-space view-projection in a `CameraUniforms` slot (only
/// `view_proj` is read), group 1 is the standard per-mesh `MeshUniforms`, so the
/// existing per-mesh bind group is reused verbatim. The vertex stage consumes only
/// `@location(0)` of the shared [`crate::vertex::Vertex`] layout; there is no
/// fragment stage — the pipeline writes depth only.
pub const PBR_SHADOW_DEPTH_WGSL: &str = r"
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

struct MeshUniforms {
    model    : mat4x4<f32>,
    nrm_col0 : vec4<f32>,
    nrm_col1 : vec4<f32>,
    nrm_col2 : vec4<f32>,
}

@group(0) @binding(0) var<uniform> light : CameraUniforms;
@group(1) @binding(0) var<uniform> mesh  : MeshUniforms;

@vertex
fn vs_main(@location(0) position : vec3<f32>) -> @builtin(position) vec4<f32> {
    return light.view_proj * mesh.model * vec4<f32>(position, 1.0);
}
";

/// Depth-only shadow caster shader for hardware-instanced batches (Phase 4).
///
/// Same role as [`PBR_SHADOW_DEPTH_WGSL`] but the per-instance world matrix is
/// read from the step-mode=Instance vertex buffer (slot 1, `InstanceRaw`,
/// `@location(5..8)`) instead of a `MeshUniforms` bind group, so the pipeline
/// layout is `[camera_bgl]` (group 0 only). Draws every instance of the batch
/// with a single `draw_indexed(.., 0..instance_count)`.
pub const PBR_SHADOW_DEPTH_INSTANCED_WGSL: &str = r"
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

@group(0) @binding(0) var<uniform> light : CameraUniforms;

struct InstanceInput {
    @location(5) model_col0: vec4<f32>,
    @location(6) model_col1: vec4<f32>,
    @location(7) model_col2: vec4<f32>,
    @location(8) model_col3: vec4<f32>,
}

@vertex
fn vs_main(@location(0) position : vec3<f32>, inst: InstanceInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(inst.model_col0, inst.model_col1, inst.model_col2, inst.model_col3);
    return light.view_proj * model * vec4<f32>(position, 1.0);
}
";

/// Depth-only shadow caster shader for GPU-skinned meshes (Phase 4).
///
/// Applies the same eight-influence linear blend skinning as
/// `PBR_SKINNED_WGSL_SHADER`'s vertex stage (position only — no normal/tangent),
/// then projects by the light view-projection. The joint palette rides group 1
/// (not group 2 as in the beauty pass): the depth pass has no lights bind group,
/// so the layout is `[camera_bgl, skin_bgl]` and the existing skin bind group is
/// bound at slot 1.
pub const PBR_SHADOW_DEPTH_SKINNED_WGSL: &str = r"
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

struct SkinJoints {
    matrices : array<mat4x4<f32>, 256>,
}

@group(0) @binding(0) var<uniform> light : CameraUniforms;
@group(1) @binding(0) var<uniform> skin  : SkinJoints;

@vertex
fn vs_main(
    @location(0) position        : vec3<f32>,
    @location(5) skin_joints      : vec4<u32>,
    @location(6) skin_weights     : vec4<f32>,
    @location(7) skin_joints_hi   : vec4<u32>,
    @location(8) skin_weights_hi  : vec4<f32>,
) -> @builtin(position) vec4<f32> {
    let p = vec4<f32>(position, 1.0);
    var wp4 = vec4<f32>(0.0);
    wp4 += skin.matrices[skin_joints.x] * p * skin_weights.x;
    wp4 += skin.matrices[skin_joints.y] * p * skin_weights.y;
    wp4 += skin.matrices[skin_joints.z] * p * skin_weights.z;
    wp4 += skin.matrices[skin_joints.w] * p * skin_weights.w;
    wp4 += skin.matrices[skin_joints_hi.x] * p * skin_weights_hi.x;
    wp4 += skin.matrices[skin_joints_hi.y] * p * skin_weights_hi.y;
    wp4 += skin.matrices[skin_joints_hi.z] * p * skin_weights_hi.z;
    wp4 += skin.matrices[skin_joints_hi.w] * p * skin_weights_hi.w;
    return light.view_proj * wp4;
}
";

/// Camera **Z-prepass** depth-only shader for static / UDIM / world-baked-merge
/// meshes (QI-4). Layout `[camera_bgl (0), mesh_bgl (1)]` — the same two-group shape
/// as [`PBR_SHADOW_DEPTH_WGSL`], so the existing per-mesh bind group binds at group 1.
///
/// **Must** reproduce [`PBR_WGSL_SHADER`]'s `vs_main` clip-space math *bit-for-bit* —
/// `view_proj * (model * p)`, parenthesised exactly so — because the beauty opaque
/// pass tests this prepass's depth with `LessEqual`; a different multiply association
/// (e.g. `(view_proj * model) * p`, as the shadow shader uses) would round differently
/// and punch holes in frontmost surfaces. No fragment stage (depth only).
pub const PBR_DEPTH_PREPASS_WGSL: &str = r"
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

struct MeshUniforms {
    model    : mat4x4<f32>,
    nrm_col0 : vec4<f32>,
    nrm_col1 : vec4<f32>,
    nrm_col2 : vec4<f32>,
}

@group(0) @binding(0) var<uniform> camera : CameraUniforms;
@group(1) @binding(0) var<uniform> mesh   : MeshUniforms;

@vertex
fn vs_main(@location(0) position : vec3<f32>) -> @builtin(position) vec4<f32> {
    // Association must match PBR_WGSL_SHADER::vs_main: wp4 = model * p; clip = vp * wp4.
    let wp4 = mesh.model * vec4<f32>(position, 1.0);
    return camera.view_proj * wp4;
}
";

/// Camera **Z-prepass** depth-only shader for hardware-instanced batches (QI-4).
/// Layout `[camera_bgl (0)]`; the per-instance model matrix rides vertex slot 1
/// (`InstanceRaw`, `@location(5..8)`), matching [`PBR_INSTANCED_WGSL_SHADER`].
///
/// Reproduces that shader's `view_proj * (model * p)` association exactly (see
/// [`PBR_DEPTH_PREPASS_WGSL`] for why). No fragment stage.
pub const PBR_DEPTH_PREPASS_INSTANCED_WGSL: &str = r"
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

@group(0) @binding(0) var<uniform> camera : CameraUniforms;

struct InstanceInput {
    @location(5) model_col0: vec4<f32>,
    @location(6) model_col1: vec4<f32>,
    @location(7) model_col2: vec4<f32>,
    @location(8) model_col3: vec4<f32>,
}

@vertex
fn vs_main(@location(0) position : vec3<f32>, inst: InstanceInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(inst.model_col0, inst.model_col1, inst.model_col2, inst.model_col3);
    return camera.view_proj * (model * vec4<f32>(position, 1.0));
}
";

// ── UDIM PBR WGSL Shader (derived from PBR_WGSL_SHADER) ───────────────────────

/// Builds the UDIM variant of [`PBR_WGSL_SHADER`] by targeted source rewrites,
/// keeping the vertex stage, lighting loop, and PBR helpers byte-identical to
/// the base shader so the two never drift.
///
/// Differences from the base shader:
/// - The six group-3 texture bindings become `texture_2d_array<f32>`.
/// - A new `@group(3) @binding(8) var<uniform> udim : UdimTable` carries a
///   per-channel `tile → array-layer` table (`channel * 100 + (tile-1001)`,
///   [`crate::texture_cache::UDIM_LAYER_SENTINEL`] = absent) plus a `channel_mask`.
/// - `VertexOutput.uv` carries the **un-flipped** raw USD UV (`v` may exceed 1).
/// - Each channel resolves its tile from the raw UV, samples the matching layer
///   with `textureSampleGrad` (continuous raw-UV derivatives → seam-free LOD)
///   and a per-tile flip `local = (fract(u), 1 - fract(v))`. A sentinel layer
///   (missing tile) falls back to the material's scalar factor, exactly as an
///   absent texture would — required because the shader *replaces* rather than
///   multiplies `base_color` with the diffuse sample.
#[must_use]
pub fn pbr_udim_wgsl() -> String {
    apply_udim_rewrites(PBR_WGSL_SHADER)
}

/// UDIM variant of [`PBR_INSTANCED_WGSL_SHADER`] (hardware-instanced meshes).
///
/// Applies the identical fragment-stage rewrites as [`pbr_udim_wgsl`] via the
/// shared [`apply_udim_rewrites`] helper, so the instanced UDIM path never drifts
/// from the static one. Only the vertex stage differs (per-instance model matrix
/// from the instance vertex buffer); the shared markers (`// ── Bindings ──`,
/// `// ── Vertex stage ──`) and the six texture-sample sites are byte-compatible
/// with the base instanced shader, so the same set of `.replace()` targets match.
#[must_use]
pub fn pbr_udim_instanced_wgsl() -> String {
    apply_udim_rewrites(PBR_INSTANCED_WGSL_SHADER)
}

/// Shared UDIM source rewrites applied to a base PBR shader (static or instanced).
///
/// Rewrites the six group-3 textures to `texture_2d_array`, injects the
/// `UdimTable`/`UdimFetch` decls and the sampling helpers, adds the
/// `@group(3) @binding(8)` `UdimTable` uniform, computes continuous UV
/// derivatives once, and swaps each texture-sample site for a `fetch_udim` call
/// with a per-channel scalar fallback. The sample-site replacements cover both
/// the multi-line form used by [`PBR_WGSL_SHADER`] and the single-line form used
/// by [`PBR_INSTANCED_WGSL_SHADER`]; the non-matching form is a no-op.
fn apply_udim_rewrites(base: &str) -> String {
    const UDIM_DECLS: &str = r"// ── UDIM tile table ─────────────────────────────────────────────────────────
const UDIM_SENTINEL : u32 = 0xFFFFFFFFu;

struct UdimTable {
    // Per-channel tile→layer index; flat = channel * 100 + (tile - 1001).
    // 6 channels × 100 tiles, packed as vec4<u32> for uniform array stride.
    layers       : array<vec4<u32>, 150>,
    channel_mask : u32,   // bit c set → channel c samples a UDIM array
    _up0         : u32,
    _up1         : u32,
    _up2         : u32,
}

struct UdimFetch {
    value : vec4<f32>,
    hit   : bool,
}

";

    const UDIM_FUNCS: &str = r"// ── UDIM sampling helpers ────────────────────────────────────────────────────
fn udim_layer(channel: u32, uv: vec2<f32>) -> u32 {
    let tu = i32(floor(uv.x));
    let tv = i32(floor(uv.y));
    if tu < 0 || tu > 9 || tv < 0 { return UDIM_SENTINEL; }
    let tile_index = u32(tu + 10 * tv);
    if tile_index >= 100u { return UDIM_SENTINEL; }
    let flat = channel * 100u + tile_index;
    return udim.layers[flat / 4u][flat % 4u];
}

fn fetch_udim(
    t   : texture_2d_array<f32>,
    ch  : u32,
    uv  : vec2<f32>,
    ddx : vec2<f32>,
    ddy : vec2<f32>,
) -> UdimFetch {
    var r : UdimFetch;
    r.value = vec4<f32>(0.0);
    r.hit   = false;
    if (udim.channel_mask & (1u << ch)) != 0u {
        let layer = udim_layer(ch, uv);
        if layer == UDIM_SENTINEL {
            return r;   // absent tile → caller keeps the scalar fallback
        }
        // Per-tile flip (wgpu top-left origin) on the fractional coord; the
        // continuous raw-UV derivatives keep LOD seam-free across tile borders.
        let local = vec2<f32>(fract(uv.x), 1.0 - fract(uv.y));
        r.value = textureSampleGrad(t, mat_sampler, local, layer, ddx, ddy);
    } else {
        // Non-UDIM channel of a UDIM material: a plain 1-layer array, flipped V.
        r.value = textureSampleGrad(t, mat_sampler, vec2<f32>(uv.x, 1.0 - uv.y), 0u, ddx, ddy);
    }
    r.hit = true;
    return r;
}

// ── Vertex stage ──────────────────────────────────────────────────────────────";

    let mut s = base.to_string();

    // 1. Six group-3 textures become array textures.
    s = s.replace(": texture_2d<f32>;", ": texture_2d_array<f32>;");

    // 1a. The group-1 DomeLight environment texture is a plain 2D equirect, not a
    //     UDIM array — restore it (step 1 arrayified it indiscriminately).
    s = s.replace(
        "@group(1) @binding(4) var          env_tex      : texture_2d_array<f32>;",
        "@group(1) @binding(4) var          env_tex      : texture_2d<f32>;",
    );

    // 2. Add the UdimTable uniform after the MaterialUniforms binding.
    s = s.replace(
        "@group(3) @binding(7) var<uniform> mat          : MaterialUniforms;",
        "@group(3) @binding(7) var<uniform> mat          : MaterialUniforms;\n@group(3) @binding(8) var<uniform> udim         : UdimTable;",
    );

    // 3. Inject the UdimTable/UdimFetch decls before the bindings section …
    s = s.replace("// ── Bindings ──────────────────────────────────────────────────────────────────", &format!("{UDIM_DECLS}// ── Bindings ──────────────────────────────────────────────────────────────────"));

    // 4. … and the sampling helpers before the vertex stage.
    s = s.replace(
        "// ── Vertex stage ──────────────────────────────────────────────────────────────",
        UDIM_FUNCS,
    );

    // 5. Compute continuous UV derivatives once (uniform control flow).
    s = s.replace(
        "    let uv = in.uv;",
        "    let uv = in.uv;\n    let duv_dx = dpdx(uv);\n    let duv_dy = dpdy(uv);",
    );

    // 6. Rewrite the six texture sample sites to UDIM fetches.
    s = s.replace(
        "    if (mat.features & FEAT_BASE_COLOR) != 0u {\n        let s   = textureSample(tex_diffuse, mat_sampler, uv);\n        base_color = s.rgb * mat.base_color.rgb;\n        if (mat.features & FEAT_DIFFUSE_ALPHA) != 0u {\n            opacity = s.a;\n        }\n    }",
        "    if (mat.features & FEAT_BASE_COLOR) != 0u {\n        let f = fetch_udim(tex_diffuse, 0u, uv, duv_dx, duv_dy);\n        if f.hit {\n            base_color = f.value.rgb * mat.base_color.rgb;\n            if (mat.features & FEAT_DIFFUSE_ALPHA) != 0u {\n                opacity = f.value.a;\n            }\n        }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_NORMAL) != 0u {\n        let samp = textureSample(tex_normal, mat_sampler, uv);\n        let xy   = samp.rg * 2.0 - 1.0;\n        // Reconstruct Z assuming unit-length tangent-space normal.\n        let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));\n        let ts_n = normalize(vec3<f32>(xy, z));\n        let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);\n        N = normalize(TBN * ts_n);\n    }",
        "    if (mat.features & FEAT_NORMAL) != 0u {\n        let f = fetch_udim(tex_normal, 1u, uv, duv_dx, duv_dy);\n        if f.hit {\n            let xy   = f.value.rg * 2.0 - 1.0;\n            // Reconstruct Z assuming unit-length tangent-space normal.\n            let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));\n            let ts_n = normalize(vec3<f32>(xy, z));\n            let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);\n            N = normalize(TBN * ts_n);\n        }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_METALLIC) != 0u {\n        metallic = swizzle_ch(\n            textureSample(tex_metallic, mat_sampler, uv), mat.channels.x\n        );\n    }",
        "    if (mat.features & FEAT_METALLIC) != 0u {\n        let f = fetch_udim(tex_metallic, 2u, uv, duv_dx, duv_dy);\n        if f.hit { metallic = swizzle_ch(f.value, mat.channels.x); }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_ROUGHNESS) != 0u {\n        roughness = swizzle_ch(\n            textureSample(tex_roughness, mat_sampler, uv), mat.channels.y\n        );\n    }",
        "    if (mat.features & FEAT_ROUGHNESS) != 0u {\n        let f = fetch_udim(tex_roughness, 3u, uv, duv_dx, duv_dy);\n        if f.hit { roughness = swizzle_ch(f.value, mat.channels.y); }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_OCCLUSION) != 0u {\n        ao = swizzle_ch(\n            textureSample(tex_occlusion, mat_sampler, uv), mat.channels.z\n        );\n    }",
        "    if (mat.features & FEAT_OCCLUSION) != 0u {\n        let f = fetch_udim(tex_occlusion, 4u, uv, duv_dx, duv_dy);\n        if f.hit { ao = swizzle_ch(f.value, mat.channels.z); }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_EMISSIVE) != 0u {\n        emissive = textureSample(tex_emissive, mat_sampler, uv).rgb;\n    }",
        "    if (mat.features & FEAT_EMISSIVE) != 0u {\n        let f = fetch_udim(tex_emissive, 5u, uv, duv_dx, duv_dy);\n        if f.hit { emissive = f.value.rgb; }\n    }",
    );

    // 7. Instanced-shader sample sites use a single-line form for the normal,
    //    metallic, roughness and occlusion channels (diffuse/emissive are already
    //    byte-identical to the base shader and handled above). These no-op on the
    //    static shader and match only when `base == PBR_INSTANCED_WGSL_SHADER`.
    s = s.replace(
        "    if (mat.features & FEAT_NORMAL) != 0u {\n        let samp = textureSample(tex_normal, mat_sampler, uv);\n        let xy   = samp.rg * 2.0 - 1.0;\n        let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));\n        let ts_n = normalize(vec3<f32>(xy, z));\n        let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);\n        N = normalize(TBN * ts_n);\n    }",
        "    if (mat.features & FEAT_NORMAL) != 0u {\n        let f = fetch_udim(tex_normal, 1u, uv, duv_dx, duv_dy);\n        if f.hit {\n            let xy   = f.value.rg * 2.0 - 1.0;\n            let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));\n            let ts_n = normalize(vec3<f32>(xy, z));\n            let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);\n            N = normalize(TBN * ts_n);\n        }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_METALLIC) != 0u {\n        metallic = swizzle_ch(textureSample(tex_metallic, mat_sampler, uv), mat.channels.x);\n    }",
        "    if (mat.features & FEAT_METALLIC) != 0u {\n        let f = fetch_udim(tex_metallic, 2u, uv, duv_dx, duv_dy);\n        if f.hit { metallic = swizzle_ch(f.value, mat.channels.x); }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_ROUGHNESS) != 0u {\n        roughness = swizzle_ch(textureSample(tex_roughness, mat_sampler, uv), mat.channels.y);\n    }",
        "    if (mat.features & FEAT_ROUGHNESS) != 0u {\n        let f = fetch_udim(tex_roughness, 3u, uv, duv_dx, duv_dy);\n        if f.hit { roughness = swizzle_ch(f.value, mat.channels.y); }\n    }",
    );
    s = s.replace(
        "    if (mat.features & FEAT_OCCLUSION) != 0u {\n        ao = swizzle_ch(textureSample(tex_occlusion, mat_sampler, uv), mat.channels.z);\n    }",
        "    if (mat.features & FEAT_OCCLUSION) != 0u {\n        let f = fetch_udim(tex_occlusion, 4u, uv, duv_dx, duv_dy);\n        if f.hit { ao = swizzle_ch(f.value, mat.channels.z); }\n    }",
    );

    s
}

// ── Instanced PBR WGSL Shader ────────────────────────────────────────────────

/// Hardware-instanced variant of [`PBR_WGSL_SHADER`].
///
/// Differences from the base shader:
/// - Vertex input adds `InstanceInput` (locations 5–8): four `vec4<f32>` columns
///   that form the column-major per-instance world matrix.  This data comes from
///   a step-mode=Instance vertex buffer in slot 1 (`InstanceRaw`).
/// - The model matrix is built from those four columns instead of `MeshUniforms`.
/// - The normal matrix is computed per-vertex via the cofactor method
///   (`cross(M[i], M[j])`) — mathematically equivalent to `transpose(inverse(M))`
///   but requires no matrix inverse, just three cross products.
/// - `@group(2) @binding(0) var<uniform> mesh: MeshUniforms` is still declared
///   to keep the pipeline layout identical to the standard shader (no new BGL
///   needed), but the uniform is never read; the renderer binds a shared
///   identity-matrix dummy buffer there.
/// - Fragment stage is byte-for-byte identical to the base shader.
pub const PBR_INSTANCED_WGSL_SHADER: &str = r"
// ── Constants ──────────────────────────────────────────────────────────────────
const PI          : f32 = 3.14159265358979323846;
const MAX_LIGHTS  : u32 = 16u;

const FEAT_BASE_COLOR : u32 = 1u;
const FEAT_NORMAL     : u32 = 2u;
const FEAT_METALLIC   : u32 = 4u;
const FEAT_ROUGHNESS  : u32 = 8u;
const FEAT_OCCLUSION  : u32 = 16u;
const FEAT_EMISSIVE   : u32 = 32u;
const FEAT_DIFFUSE_ALPHA  : u32 = 64u;
const FEAT_BLEND          : u32 = 128u;

const LIGHT_DISTANT   : u32 = 0u;
const LIGHT_SPHERE    : u32 = 1u;
const LIGHT_RECT      : u32 = 2u;
const LIGHT_DOME      : u32 = 3u;
const LIGHT_DOME_PROC : u32 = 4u;
const LIGHT_DISK      : u32 = 5u;

// ── Uniform structs ────────────────────────────────────────────────────────────
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

struct LightGpu {
    kind      : u32,
    _pad0     : u32,
    _pad1     : u32,
    _pad2     : u32,
    dir_or_pos: vec4<f32>,
    color_pwr : vec4<f32>,
    size      : vec4<f32>,
}

struct LightsUniforms {
    lights : array<LightGpu, 16>,
    count  : u32,
    _pad0  : u32,
    _pad1  : u32,
    _pad2  : u32,
}

struct ShadowUniforms {
    // Per-slot light-space view-projection (world → shadow NDC).
    light_view_proj : array<mat4x4<f32>, 4>,
    // Per-slot params: x = source light index, y = near, z = far, w = light size.
    params          : array<vec4<f32>, 4>,
    // x = tier (0=Off, 1=Smooth/PCF, 2=Soft/PCSS, 3=Hard), y = atlas resolution, z = active slot count,
    // w = constant receiver depth bias.
    global          : vec4<f32>,
}

// UsdLuxDomeLight environment IBL (group 1). `sh` holds 9 order-2 spherical-
// harmonic radiance coefficients (rgb in xyz); `rot0..2` are the columns of the
// world→environment rotation mat3; `ambient` is the dome's flat radiance; and
// `params` = (max_mip, has_env, has_texture, _).
struct EnvUniforms {
    sh      : array<vec4<f32>, 9>,
    rot0    : vec4<f32>,
    rot1    : vec4<f32>,
    rot2    : vec4<f32>,
    ambient : vec4<f32>,
    params  : vec4<f32>,
}

// Declared to match the standard pipeline layout but never read in this shader.
struct MeshUniforms {
    model    : mat4x4<f32>,
    nrm_col0 : vec4<f32>,
    nrm_col1 : vec4<f32>,
    nrm_col2 : vec4<f32>,
}

struct MaterialUniforms {
    base_color : vec4<f32>,
    mr_sub_thr : vec4<f32>,
    emissive   : vec3<f32>,
    features   : u32,
    channels   : vec4<u32>,
}

// ── Bindings ──────────────────────────────────────────────────────────────────
@group(0) @binding(0) var<uniform> camera       : CameraUniforms;
@group(1) @binding(0) var<uniform> lights       : LightsUniforms;
@group(1) @binding(1) var          shadow_atlas : texture_depth_2d_array;
@group(1) @binding(2) var          shadow_samp  : sampler_comparison;
@group(1) @binding(3) var<uniform> shadows      : ShadowUniforms;
@group(1) @binding(4) var          env_tex      : texture_2d<f32>;
@group(1) @binding(5) var          env_sampler  : sampler;
@group(1) @binding(6) var<uniform> env          : EnvUniforms;
@group(2) @binding(0) var<uniform> mesh         : MeshUniforms;  // layout compat; not read
@group(3) @binding(0) var          mat_sampler  : sampler;
@group(3) @binding(1) var          tex_diffuse  : texture_2d<f32>;
@group(3) @binding(2) var          tex_normal   : texture_2d<f32>;
@group(3) @binding(3) var          tex_metallic : texture_2d<f32>;
@group(3) @binding(4) var          tex_roughness: texture_2d<f32>;
@group(3) @binding(5) var          tex_occlusion: texture_2d<f32>;
@group(3) @binding(6) var          tex_emissive : texture_2d<f32>;
@group(3) @binding(7) var<uniform> mat          : MaterialUniforms;

// ── Shadow sampling (3×3 PCF hard + PCSS soft — identical to PBR_WGSL_SHADER) ─
const SHADOW_BUDGET_WGSL : u32 = 4u;
// Floor for shadow visibility: a fully-occluded receiver keeps this fraction of
// the light's direct contribution instead of going pitch black. 0.2 ⇒ deepest
// shadow is 80% darker, not 100%. Set to 0.0 for physically hard black shadows.
const SHADOW_MIN_VISIBILITY : f32 = 0.2;

// PCSS soft shadow: blocker search → similar-triangles penumbra → variable PCF.
// The atlas is a bounding-sphere ortho fit whose depth range equals its lateral
// width, so the world→UV penumbra conversion reduces to
// `light_size × (receiver_z − avg_blocker_z)`. See PBR_WGSL_SHADER for the
// annotated master copy.
fn sample_shadow_pcss(slot: u32, uv: vec2<f32>, recv_z: f32, ref_depth: f32) -> f32 {
    let res        = max(shadows.global.y, 1.0);
    let texel      = 1.0 / res;
    let light_size = shadows.params[slot].w;
    let dim        = i32(res);
    let base       = vec2<i32>(i32(uv.x * res), i32(uv.y * res));

    var blk_sum : f32 = 0.0;
    var blk_cnt : f32 = 0.0;
    let search : i32 = 3;
    for (var dy : i32 = -search; dy <= search; dy = dy + 1) {
        for (var dx : i32 = -search; dx <= search; dx = dx + 1) {
            let c = base + vec2<i32>(dx, dy);
            if c.x < 0 || c.y < 0 || c.x >= dim || c.y >= dim { continue; }
            let d = textureLoad(shadow_atlas, c, i32(slot), 0);
            if d < recv_z - 0.0008 {
                blk_sum = blk_sum + d;
                blk_cnt = blk_cnt + 1.0;
            }
        }
    }
    if blk_cnt < 0.5 { return 1.0; }
    let avg_blk = blk_sum / blk_cnt;

    let penumbra_uv = light_size * max(recv_z - avg_blk, 0.0);
    let radius      = clamp(penumbra_uv * res, 1.0, 16.0);

    let step = radius * texel * 0.5;
    var sum : f32 = 0.0;
    for (var dy : i32 = -2; dy <= 2; dy = dy + 1) {
        for (var dx : i32 = -2; dx <= 2; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * step;
            sum = sum + textureSampleCompareLevel(shadow_atlas, shadow_samp, uv + off, slot, ref_depth);
        }
    }
    return sum / 25.0;
}

fn shadow_visibility(light_idx: u32, world_pos: vec3<f32>, ndotl: f32) -> f32 {
    if shadows.global.x < 0.5 { return 1.0; }              // Off tier
    let slot_count = u32(shadows.global.z + 0.5);
    var slot : u32 = 0xFFFFFFFFu;
    for (var s : u32 = 0u; s < slot_count && s < SHADOW_BUDGET_WGSL; s = s + 1u) {
        if u32(shadows.params[s].x + 0.5) == light_idx { slot = s; break; }
    }
    if slot == 0xFFFFFFFFu { return 1.0; }                 // this light casts no shadow

    let clip = shadows.light_view_proj[slot] * vec4<f32>(world_pos, 1.0);
    let ndc  = clip.xyz / clip.w;
    if ndc.z < 0.0 || ndc.z > 1.0 ||
       ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 {
        return 1.0;                                        // outside the shadow frustum
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let bias = shadows.global.w * (1.0 + 2.0 * clamp(1.0 - ndotl, 0.0, 1.0));
    let ref_depth = ndc.z - bias;

    // Hard tier (global.x ≈ 3): single comparison tap — see PBR_WGSL_SHADER.
    if shadows.global.x > 2.5 {
        return textureSampleCompareLevel(shadow_atlas, shadow_samp, uv, slot, ref_depth);
    }

    if shadows.global.x > 1.5 {
        return sample_shadow_pcss(slot, uv, ndc.z, ref_depth);
    }

    let texel = 1.0 / max(shadows.global.y, 1.0);
    var sum : f32 = 0.0;
    for (var dy : i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx : i32 = -1; dx <= 1; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompareLevel(shadow_atlas, shadow_samp, uv + off, slot, ref_depth);
        }
    }
    return sum / 9.0;
}

// ── Vertex stage ──────────────────────────────────────────────────────────────
struct VertexInput {
    @location(0) position    : vec3<f32>,
    @location(1) normal      : vec3<f32>,
    @location(2) texcoord    : vec2<f32>,
    @location(3) vertex_color: vec3<f32>,
    @location(4) tangent     : vec4<f32>,
}

// Per-instance world matrix delivered via a step-mode=Instance vertex buffer.
struct InstanceInput {
    @location(5) model_col0: vec4<f32>,
    @location(6) model_col1: vec4<f32>,
    @location(7) model_col2: vec4<f32>,
    @location(8) model_col3: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_pos    : vec4<f32>,
    @location(0)       world_nrm   : vec3<f32>,
    @location(1)       world_pos   : vec3<f32>,
    @location(2)       uv          : vec2<f32>,
    @location(3)       vertex_color: vec3<f32>,
    @location(4)       tangent     : vec4<f32>,
}

@vertex
fn vs_main(in: VertexInput, inst: InstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let model = mat4x4<f32>(inst.model_col0, inst.model_col1, inst.model_col2, inst.model_col3);
    let wp4 = model * vec4<f32>(in.position, 1.0);
    out.clip_pos  = camera.view_proj * wp4;
    out.world_pos = wp4.xyz;
    // Normal matrix via cofactor method: for M = model upper-left 3×3,
    // cofactor(M)^T = det(M) · inverse(M)^T = the correct normal transform.
    // The det(M) factor is absorbed by normalize() in the fragment stage.
    let c0 = cross(model[1].xyz, model[2].xyz);
    let c1 = cross(model[2].xyz, model[0].xyz);
    let c2 = cross(model[0].xyz, model[1].xyz);
    let nm = mat3x3<f32>(c0, c1, c2);
    out.world_nrm = normalize(nm * in.normal);
    out.uv         = in.texcoord;
    out.vertex_color = in.vertex_color;
    if in.tangent.w != 0.0 {
        out.tangent = vec4<f32>(normalize(nm * in.tangent.xyz), in.tangent.w);
    } else {
        out.tangent = vec4<f32>(0.0);
    }
    return out;
}

// ── PBR helper functions (identical to PBR_WGSL_SHADER) ──────────────────────
fn distribution_ggx(N: vec3<f32>, H: vec3<f32>, roughness: f32) -> f32 {
    let a   = roughness * roughness;
    let a2  = a * a;
    let nh  = max(dot(N, H), 0.0);
    let d   = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}
fn geometry_schlick(ndotx: f32, k: f32) -> f32 {
    return ndotx / (ndotx * (1.0 - k) + k);
}
fn geometry_smith(ndotv: f32, ndotl: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) * 0.125;
    return geometry_schlick(max(ndotv, 0.0001), k)
         * geometry_schlick(max(ndotl, 0.0001), k);
}
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (1.0 - f0) * pow(saturate(1.0 - cos_theta), 5.0);
}
fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let inv_r = 1.0 - roughness;
    return f0 + (max(vec3<f32>(inv_r), f0) - f0) * pow(saturate(1.0 - cos_theta), 5.0);
}
fn env_brdf_approx(roughness: f32, ndotv: f32) -> vec2<f32> {
    let c0   = vec4<f32>(-1.0, -0.0275, -0.572,  0.022);
    let c1   = vec4<f32>( 1.0,  0.0425,  1.04,  -0.04);
    let r    = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * ndotv)) * r.x + r.y;
    return clamp(vec2<f32>(-1.04, 1.04) * a004 + r.zw, vec2<f32>(0.0), vec2<f32>(1.0));
}

// ── DomeLight environment IBL helpers ────────────────────────────────────────
// Rotate a world-space direction into the environment's sampling frame.
fn env_rotate(d: vec3<f32>) -> vec3<f32> {
    let m = mat3x3<f32>(env.rot0.xyz, env.rot1.xyz, env.rot2.xyz);
    return m * d;
}

// Lat-long (equirect) mapping matching the CPU `env_ibl` module: u wraps azimuth
// from -Z, v=0 is the +Y pole.
fn env_equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let u = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let v = acos(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

// Cosine-convolved order-2 SH irradiance E(n) (already π-scaled). `n_world` is a
// world-space normal; the environment rotation is applied here.
fn env_sh_irradiance(n_world: vec3<f32>) -> vec3<f32> {
    let n = normalize(env_rotate(n_world));
    let x = n.x;
    let y = n.y;
    let z = n.z;
    let A0 = PI;
    let A1 = 2.0 * PI / 3.0;
    let A2 = PI / 4.0;
    var e = vec3<f32>(0.0);
    e += env.sh[0].rgb * (A0 * 0.282095);
    e += env.sh[1].rgb * (A1 * 0.488603 * y);
    e += env.sh[2].rgb * (A1 * 0.488603 * z);
    e += env.sh[3].rgb * (A1 * 0.488603 * x);
    e += env.sh[4].rgb * (A2 * 1.092548 * x * y);
    e += env.sh[5].rgb * (A2 * 1.092548 * y * z);
    e += env.sh[6].rgb * (A2 * 0.315392 * (3.0 * z * z - 1.0));
    e += env.sh[7].rgb * (A2 * 1.092548 * x * z);
    e += env.sh[8].rgb * (A2 * 0.546274 * (x * x - y * y));
    return max(e, vec3<f32>(0.0));
}

// Roughness-selected equirect specular sample. `r_world` is a world-space
// reflection vector; roughness selects the prefiltered mip.
fn env_specular(r_world: vec3<f32>, roughness: f32) -> vec3<f32> {
    let uv  = env_equirect_uv(normalize(env_rotate(r_world)));
    let lod = roughness * env.params.x;
    return textureSampleLevel(env_tex, env_sampler, uv, lod).rgb;
}
fn swizzle_ch(v: vec4<f32>, ch: u32) -> f32 {
    if ch == 0u { return v.r; }
    if ch == 1u { return v.g; }
    if ch == 2u { return v.b; }
    return v.a;
}
fn compute_tbn(world_pos: vec3<f32>, N: vec3<f32>, uv: vec2<f32>, vtx_tangent: vec4<f32>) -> mat3x3<f32> {
    if vtx_tangent.w != 0.0 {
        var T = normalize(vtx_tangent.xyz - dot(vtx_tangent.xyz, N) * N);
        let B = cross(N, T) * vtx_tangent.w;
        return mat3x3<f32>(T, B, N);
    }
    let dp1  = dpdx(world_pos);
    let dp2  = dpdy(world_pos);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let det  = duv1.x * duv2.y - duv1.y * duv2.x;
    if abs(det) < 1e-7 {
        var up = vec3<f32>(0.0, 1.0, 0.0);
        if abs(N.y) > 0.99 { up = vec3<f32>(1.0, 0.0, 0.0); }
        let T = normalize(cross(up, N));
        let B = cross(N, T);
        return mat3x3<f32>(T, B, N);
    }
    let inv = 1.0 / det;
    var T = (dp1 * duv2.y - dp2 * duv1.y) * inv;
    T = normalize(T - dot(T, N) * N);
    let B = cross(N, T);
    return mat3x3<f32>(T, B, N);
}

// ── Fragment stage (identical to PBR_WGSL_SHADER) ─────────────────────────────
@fragment
fn fs_main(
    in            : VertexOutput,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let face_sign = select(-1.0, 1.0, front_facing);
    let N_geom    = normalize(in.world_nrm) * face_sign;
    var base_color : vec3<f32> = mat.base_color.rgb;
    var opacity    : f32       = mat.base_color.a;
    if (mat.features & FEAT_BASE_COLOR) != 0u {
        let s   = textureSample(tex_diffuse, mat_sampler, uv);
        base_color = s.rgb * mat.base_color.rgb;
        if (mat.features & FEAT_DIFFUSE_ALPHA) != 0u {
            opacity = s.a;
        }
    }
    base_color = base_color * in.vertex_color;
    if mat.mr_sub_thr.w > 0.0 && opacity < mat.mr_sub_thr.w {
        discard;
    }
    var N : vec3<f32> = N_geom;
    if (mat.features & FEAT_NORMAL) != 0u {
        let samp = textureSample(tex_normal, mat_sampler, uv);
        let xy   = samp.rg * 2.0 - 1.0;
        let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));
        let ts_n = normalize(vec3<f32>(xy, z));
        let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);
        N = normalize(TBN * ts_n);
    }
    var metallic  : f32 = mat.mr_sub_thr.x;
    var roughness : f32 = mat.mr_sub_thr.y;
    if (mat.features & FEAT_METALLIC) != 0u {
        metallic = swizzle_ch(textureSample(tex_metallic, mat_sampler, uv), mat.channels.x);
    }
    if (mat.features & FEAT_ROUGHNESS) != 0u {
        roughness = swizzle_ch(textureSample(tex_roughness, mat_sampler, uv), mat.channels.y);
    }
    roughness = clamp(roughness, 0.04, 1.0);
    var ao : f32 = 1.0;
    if (mat.features & FEAT_OCCLUSION) != 0u {
        ao = swizzle_ch(textureSample(tex_occlusion, mat_sampler, uv), mat.channels.z);
    }
    let F0    = mix(vec3<f32>(0.04), base_color, metallic);
    let V     = normalize(camera.eye_pos.xyz - in.world_pos);
    let NdotV = max(dot(N, V), 0.0001);
    var Lo      : vec3<f32> = vec3<f32>(0.0);  // direct diffuse only
    var spec    : vec3<f32> = vec3<f32>(0.0);  // all specular / reflection
    var ambient : vec3<f32> = vec3<f32>(0.0);
    for (var i : u32 = 0u; i < lights.count && i < MAX_LIGHTS; i++) {
        let light = lights.lights[i];
        if light.kind == LIGHT_DOME || light.kind == LIGHT_DOME_PROC {
            // Textured domes handled once from the global env uniform after the loop;
            // here only untextured/procedural domes add their flat analytic term.
            if env.params.z < 0.5 {
                let dome_col = light.color_pwr.rgb * light.color_pwr.w;
                let F_ibl  = fresnel_schlick_roughness(NdotV, F0, roughness);
                let kD_ibl = (vec3<f32>(1.0) - F_ibl) * (1.0 - metallic);
                let sky_t  = 0.5 + 0.5 * N.y;
                ambient += kD_ibl * dome_col * sky_t * (1.0 / PI);
                let ab = env_brdf_approx(roughness, NdotV);
                spec += dome_col * (F0 * ab.x + vec3<f32>(ab.y));
            }
            continue;
        }
        var L        : vec3<f32>;
        var radiance : vec3<f32>;
        if light.kind == LIGHT_DISTANT {
            L        = normalize(light.dir_or_pos.xyz);
            radiance = light.color_pwr.rgb * light.color_pwr.w;
        } else {
            let to_light = light.dir_or_pos.xyz - in.world_pos;
            let dist     = length(to_light);
            L        = to_light * (1.0 / max(dist, 0.0001));
            // Radius-aware inverse-square falloff: clamp the 1/r² singularity by
            // the light's own render-unit size (size.xy), not a fixed scene-scale
            // constant, so brightness is invariant to metersPerUnit. The bare 0.01
            // floor pinned attenuation at 100x for any centimetre-scale stage.
            let soft_r2 = max(light.size.x * light.size.x, light.size.y * light.size.y);
            let atten = 1.0 / max(dist * dist, max(soft_r2, 1e-6));
            radiance  = light.color_pwr.rgb * light.color_pwr.w * atten;
        }
        let NdotL = max(dot(N, L), 0.0);
        if NdotL < 0.0001 { continue; }
        let H = normalize(V + L);
        let D        = distribution_ggx(N, H, roughness);
        let G        = geometry_smith(NdotV, NdotL, roughness);
        let F        = fresnel_schlick(max(dot(H, V), 0.0), F0);
        let specular = (D * G * F) / max(4.0 * NdotV * NdotL, 0.001);
        let kD      = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let diffuse = kD * base_color / PI;
        // Per-light diffuse/specular multipliers (UsdLuxLightAPI, size.zw).
        // Shadow visibility (1.0 = lit) folds in per shadow-casting light; a light
        // with no atlas slot and the Off tier both return 1.0.
        let shadow = shadow_visibility(i, in.world_pos, NdotL);
        Lo   += diffuse  * light.size.z * radiance * NdotL * shadow;
        spec += specular * light.size.w * radiance * NdotL * shadow;
    }
    // Textured DomeLight IBL, applied once from the dominant environment: SH
    // directional diffuse irradiance (E(N), π-scaled) + roughness-mip equirect
    // specular through the same analytic split-sum BRDF the analytic path uses.
    if env.params.y > 0.5 && env.params.z > 0.5 {
        let F_ibl  = fresnel_schlick_roughness(NdotV, F0, roughness);
        let kD_ibl = (vec3<f32>(1.0) - F_ibl) * (1.0 - metallic);
        let ab     = env_brdf_approx(roughness, NdotV);
        // `env.ambient.rgb` = dome color x intensity x 2^exposure — scales the raw
        // texture radiance for both the SH diffuse and the equirect specular.
        let scale  = env.ambient.rgb;
        let irr    = env_sh_irradiance(N) * scale;
        ambient   += kD_ibl * irr * (1.0 / PI);
        let R      = reflect(-V, N);
        // Specular horizon occlusion: fade reflections whose mirror direction dips below
        // the surface horizon (strongest at grazing angles), preventing the environment
        // from leaking onto silhouettes. Frostbite/Lagarde form.
        var horizon = saturate(1.0 + dot(R, N));
        horizon     = horizon * horizon;
        // Split-sum: the env-BRDF LUT (ab) already integrates the Fresnel grazing term,
        // so scale the flat F0 — multiplying F_ibl here would double-count it and make
        // the IBL specular grow without bound as NdotV -> 0.
        spec      += env_specular(R, roughness) * scale * (F0 * ab.x + vec3<f32>(ab.y)) * horizon;
    }

    if lights.count == 0u {
        let amb    = vec3<f32>(0.03);
        let F_amb  = fresnel_schlick_roughness(NdotV, F0, roughness);
        let kD_amb = (vec3<f32>(1.0) - F_amb) * (1.0 - metallic);
        ambient    = kD_amb * amb;
        let ab_amb = env_brdf_approx(roughness, NdotV);
        spec += amb * (F0 * ab_amb.x + vec3<f32>(ab_amb.y));
    }
    var emissive : vec3<f32> = mat.emissive;
    if (mat.features & FEAT_EMISSIVE) != 0u {
        emissive = textureSample(tex_emissive, mat_sampler, uv).rgb;
    }
    // Split the shaded response into a transmissive part (diffuse + diffuse IBL,
    // tinted by base colour) and a reflective part (all specular/fresnel plus
    // emission). A transparent dielectric still reflects, so the reflective part
    // must survive transmission.
    let diffuse_total  = (ambient * base_color + Lo) * ao;
    let specular_total = spec * ao;
    let reflective     = specular_total + emissive;  // always visible

    var color : vec3<f32>;
    var out_alpha : f32;
    if (mat.features & FEAT_BLEND) != 0u {
        // Transmissive dielectric drawn with PREMULTIPLIED_ALPHA_BLENDING
        // (out = src.rgb + dst·(1−src.a)): emit premultiplied colour whose diffuse
        // body fades with opacity while reflection/emission stay at full strength,
        // and raise coverage by the reflection's luminance so bright specular glints
        // occlude the background instead of vanishing at opacity 0.
        color        = (reflective + diffuse_total * opacity) * camera.exposure;
        let refl_cov = saturate(luminance(reflective * camera.exposure));
        out_alpha    = saturate(opacity + (1.0 - opacity) * refl_cov);
    } else {
        // Opaque / mask: full colour (diffuse + reflection), coverage untouched.
        color     = (diffuse_total + reflective) * camera.exposure;
        out_alpha = opacity;
    }
    return vec4<f32>(color, out_alpha);
}
";

// ── Skinned PBR WGSL Shader ───────────────────────────────────────────────────

/// GPU classic-linear blend skinning variant of [`PBR_WGSL_SHADER`].
///
/// Differences from the base shader:
/// - Vertex input adds two `vec4` joint/weight pairs on the second vertex buffer
///   slot: `@location(5)/@location(6)` carry influences 0..4 and
///   `@location(7)/@location(8)` carry influences 4..8 (up to eight per vertex).
///   Location 4 is shared with the base shader for the `tangent: vec4<f32>` attribute.
/// - Bind group 4 carries the joint matrix palette
///   (`array<mat4x4<f32>, 256>` = 16 KB, within the WebGPU 64 KB uniform limit).
/// - `vs_main` performs classic linear blend skinning (LBS): position, normal, and tangent
///   are blended across up to 8 joints whose pre-multiplied matrices encode
///   `skeleton_world × skinning_transform_j × geom_bind`. Meshes with fewer
///   influences zero-pad the unused slots (weight 0 → no contribution).
///   Group 2 (`MeshUniforms`) is intentionally absent — world space comes
///   entirely from the joint matrices.
/// - Fragment stage is identical to the base shader.
pub const PBR_SKINNED_WGSL_SHADER: &str = r"
// ── Constants ──────────────────────────────────────────────────────────────────
const PI          : f32 = 3.14159265358979323846;
const MAX_LIGHTS  : u32 = 16u;

const FEAT_BASE_COLOR : u32 = 1u;
const FEAT_NORMAL     : u32 = 2u;
const FEAT_METALLIC   : u32 = 4u;
const FEAT_ROUGHNESS  : u32 = 8u;
const FEAT_OCCLUSION  : u32 = 16u;
const FEAT_EMISSIVE   : u32 = 32u;
const FEAT_DIFFUSE_ALPHA  : u32 = 64u;
const FEAT_BLEND          : u32 = 128u;

const LIGHT_DISTANT   : u32 = 0u;
const LIGHT_SPHERE    : u32 = 1u;
const LIGHT_RECT      : u32 = 2u;
const LIGHT_DOME      : u32 = 3u;
const LIGHT_DOME_PROC : u32 = 4u;
const LIGHT_DISK      : u32 = 5u;

// ── Uniform structs ────────────────────────────────────────────────────────────
struct CameraUniforms {
    view_proj : mat4x4<f32>,
    eye_pos   : vec4<f32>,
    exposure  : f32,
    _p1       : f32,
    _p2       : f32,
    _p3       : f32,
}

struct LightGpu {
    kind      : u32,
    _pad0     : u32,
    _pad1     : u32,
    _pad2     : u32,
    dir_or_pos: vec4<f32>,
    color_pwr : vec4<f32>,
    size      : vec4<f32>,
}

struct LightsUniforms {
    lights : array<LightGpu, 16>,
    count  : u32,
    _pad0  : u32,
    _pad1  : u32,
    _pad2  : u32,
}

struct ShadowUniforms {
    // Per-slot light-space view-projection (world → shadow NDC).
    light_view_proj : array<mat4x4<f32>, 4>,
    // Per-slot params: x = source light index, y = near, z = far, w = light size.
    params          : array<vec4<f32>, 4>,
    // x = tier (0=Off, 1=Smooth/PCF, 2=Soft/PCSS, 3=Hard), y = atlas resolution, z = active slot count,
    // w = constant receiver depth bias.
    global          : vec4<f32>,
}

// UsdLuxDomeLight environment IBL (group 1). `sh` holds 9 order-2 spherical-
// harmonic radiance coefficients (rgb in xyz); `rot0..2` are the columns of the
// world→environment rotation mat3; `ambient` is the dome's flat radiance; and
// `params` = (max_mip, has_env, has_texture, _).
struct EnvUniforms {
    sh      : array<vec4<f32>, 9>,
    rot0    : vec4<f32>,
    rot1    : vec4<f32>,
    rot2    : vec4<f32>,
    ambient : vec4<f32>,
    params  : vec4<f32>,
}

struct MaterialUniforms {
    base_color : vec4<f32>,
    mr_sub_thr : vec4<f32>,
    emissive   : vec3<f32>,
    features   : u32,
    channels   : vec4<u32>,
}

/// Joint matrix palette — 256 × mat4x4<f32> = 16 384 bytes (< 64 KB WebGPU limit).
/// Each matrix: effective_j = skeleton_world × skinning_transform_j × geom_bind.
struct SkinJoints {
    matrices : array<mat4x4<f32>, 256>,
}

// ── Bindings ──────────────────────────────────────────────────────────────────
@group(0) @binding(0) var<uniform> camera       : CameraUniforms;
@group(1) @binding(0) var<uniform> lights       : LightsUniforms;
@group(1) @binding(1) var          shadow_atlas : texture_depth_2d_array;
@group(1) @binding(2) var          shadow_samp  : sampler_comparison;
@group(1) @binding(3) var<uniform> shadows      : ShadowUniforms;
@group(1) @binding(4) var          env_tex      : texture_2d<f32>;
@group(1) @binding(5) var          env_sampler  : sampler;
@group(1) @binding(6) var<uniform> env          : EnvUniforms;
// group 2 (MeshUniforms) intentionally absent — joint matrices encode world transform
@group(3) @binding(0) var          mat_sampler  : sampler;
@group(3) @binding(1) var          tex_diffuse  : texture_2d<f32>;
@group(3) @binding(2) var          tex_normal   : texture_2d<f32>;
@group(3) @binding(3) var          tex_metallic : texture_2d<f32>;
@group(3) @binding(4) var          tex_roughness: texture_2d<f32>;
@group(3) @binding(5) var          tex_occlusion: texture_2d<f32>;
@group(3) @binding(6) var          tex_emissive : texture_2d<f32>;
@group(3) @binding(7) var<uniform> mat          : MaterialUniforms;
@group(2) @binding(0) var<uniform> skin         : SkinJoints;

// ── Shadow sampling (3×3 PCF hard + PCSS soft — identical to PBR_WGSL_SHADER) ─
const SHADOW_BUDGET_WGSL : u32 = 4u;
// Floor for shadow visibility: a fully-occluded receiver keeps this fraction of
// the light's direct contribution instead of going pitch black. 0.2 ⇒ deepest
// shadow is 80% darker, not 100%. Set to 0.0 for physically hard black shadows.
const SHADOW_MIN_VISIBILITY : f32 = 0.2;

// PCSS soft shadow: blocker search → similar-triangles penumbra → variable PCF.
// The atlas is a bounding-sphere ortho fit whose depth range equals its lateral
// width, so the world→UV penumbra conversion reduces to
// `light_size × (receiver_z − avg_blocker_z)`. See PBR_WGSL_SHADER for the
// annotated master copy.
fn sample_shadow_pcss(slot: u32, uv: vec2<f32>, recv_z: f32, ref_depth: f32) -> f32 {
    let res        = max(shadows.global.y, 1.0);
    let texel      = 1.0 / res;
    let light_size = shadows.params[slot].w;
    let dim        = i32(res);
    let base       = vec2<i32>(i32(uv.x * res), i32(uv.y * res));

    var blk_sum : f32 = 0.0;
    var blk_cnt : f32 = 0.0;
    let search : i32 = 3;
    for (var dy : i32 = -search; dy <= search; dy = dy + 1) {
        for (var dx : i32 = -search; dx <= search; dx = dx + 1) {
            let c = base + vec2<i32>(dx, dy);
            if c.x < 0 || c.y < 0 || c.x >= dim || c.y >= dim { continue; }
            let d = textureLoad(shadow_atlas, c, i32(slot), 0);
            if d < recv_z - 0.0008 {
                blk_sum = blk_sum + d;
                blk_cnt = blk_cnt + 1.0;
            }
        }
    }
    if blk_cnt < 0.5 { return 1.0; }
    let avg_blk = blk_sum / blk_cnt;

    let penumbra_uv = light_size * max(recv_z - avg_blk, 0.0);
    let radius      = clamp(penumbra_uv * res, 1.0, 16.0);

    let step = radius * texel * 0.5;
    var sum : f32 = 0.0;
    for (var dy : i32 = -2; dy <= 2; dy = dy + 1) {
        for (var dx : i32 = -2; dx <= 2; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * step;
            sum = sum + textureSampleCompareLevel(shadow_atlas, shadow_samp, uv + off, slot, ref_depth);
        }
    }
    return sum / 25.0;
}

fn shadow_visibility(light_idx: u32, world_pos: vec3<f32>, ndotl: f32) -> f32 {
    if shadows.global.x < 0.5 { return 1.0; }              // Off tier
    let slot_count = u32(shadows.global.z + 0.5);
    var slot : u32 = 0xFFFFFFFFu;
    for (var s : u32 = 0u; s < slot_count && s < SHADOW_BUDGET_WGSL; s = s + 1u) {
        if u32(shadows.params[s].x + 0.5) == light_idx { slot = s; break; }
    }
    if slot == 0xFFFFFFFFu { return 1.0; }                 // this light casts no shadow

    let clip = shadows.light_view_proj[slot] * vec4<f32>(world_pos, 1.0);
    let ndc  = clip.xyz / clip.w;
    if ndc.z < 0.0 || ndc.z > 1.0 ||
       ndc.x < -1.0 || ndc.x > 1.0 || ndc.y < -1.0 || ndc.y > 1.0 {
        return 1.0;                                        // outside the shadow frustum
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let bias = shadows.global.w * (1.0 + 2.0 * clamp(1.0 - ndotl, 0.0, 1.0));
    let ref_depth = ndc.z - bias;

    // Hard tier (global.x ≈ 3): single comparison tap — see PBR_WGSL_SHADER.
    if shadows.global.x > 2.5 {
        return textureSampleCompareLevel(shadow_atlas, shadow_samp, uv, slot, ref_depth);
    }

    if shadows.global.x > 1.5 {
        return sample_shadow_pcss(slot, uv, ndc.z, ref_depth);
    }

    let texel = 1.0 / max(shadows.global.y, 1.0);
    var sum : f32 = 0.0;
    for (var dy : i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx : i32 = -1; dx <= 1; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + textureSampleCompareLevel(shadow_atlas, shadow_samp, uv + off, slot, ref_depth);
        }
    }
    return sum / 9.0;
}

// ── Vertex stage ──────────────────────────────────────────────────────────────
struct VertexInput {
    @location(0) position     : vec3<f32>,
    @location(1) normal       : vec3<f32>,
    @location(2) texcoord     : vec2<f32>,
    @location(3) vertex_color : vec3<f32>,
    @location(4) tangent      : vec4<f32>,  // xyz = tangent dir, w = bitangent sign (0 → SS fallback)
    @location(5) skin_joints     : vec4<u32>,  // influences 0..4
    @location(6) skin_weights    : vec4<f32>,
    @location(7) skin_joints_hi  : vec4<u32>,  // influences 4..8
    @location(8) skin_weights_hi : vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_pos    : vec4<f32>,
    @location(0)       world_nrm   : vec3<f32>,
    @location(1)       world_pos   : vec3<f32>,
    @location(2)       uv          : vec2<f32>,
    @location(3)       vertex_color: vec3<f32>,
    @location(4)       tangent     : vec4<f32>,
}

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let j  = in.skin_joints;
    let w  = in.skin_weights;
    let jh = in.skin_joints_hi;
    let wh = in.skin_weights_hi;
    let p = vec4<f32>(in.position, 1.0);

    // Classic linear blend skinning over up to eight influences.
    // Joint matrices encode: skeleton_world × skinning_transform_j × geom_bind.
    // Unused influence slots carry weight 0 and contribute nothing.
    var wp4 = vec4<f32>(0.0);
    wp4 += skin.matrices[j.x] * p * w.x;
    wp4 += skin.matrices[j.y] * p * w.y;
    wp4 += skin.matrices[j.z] * p * w.z;
    wp4 += skin.matrices[j.w] * p * w.w;
    wp4 += skin.matrices[jh.x] * p * wh.x;
    wp4 += skin.matrices[jh.y] * p * wh.y;
    wp4 += skin.matrices[jh.z] * p * wh.z;
    wp4 += skin.matrices[jh.w] * p * wh.w;

    out.clip_pos  = camera.view_proj * wp4;
    out.world_pos = wp4.xyz;

    // Normal LBS — upper-left 3×3 of each joint matrix.
    var wn = vec3<f32>(0.0);
    wn += mat3x3<f32>(skin.matrices[j.x][0].xyz, skin.matrices[j.x][1].xyz, skin.matrices[j.x][2].xyz) * in.normal * w.x;
    wn += mat3x3<f32>(skin.matrices[j.y][0].xyz, skin.matrices[j.y][1].xyz, skin.matrices[j.y][2].xyz) * in.normal * w.y;
    wn += mat3x3<f32>(skin.matrices[j.z][0].xyz, skin.matrices[j.z][1].xyz, skin.matrices[j.z][2].xyz) * in.normal * w.z;
    wn += mat3x3<f32>(skin.matrices[j.w][0].xyz, skin.matrices[j.w][1].xyz, skin.matrices[j.w][2].xyz) * in.normal * w.w;
    wn += mat3x3<f32>(skin.matrices[jh.x][0].xyz, skin.matrices[jh.x][1].xyz, skin.matrices[jh.x][2].xyz) * in.normal * wh.x;
    wn += mat3x3<f32>(skin.matrices[jh.y][0].xyz, skin.matrices[jh.y][1].xyz, skin.matrices[jh.y][2].xyz) * in.normal * wh.y;
    wn += mat3x3<f32>(skin.matrices[jh.z][0].xyz, skin.matrices[jh.z][1].xyz, skin.matrices[jh.z][2].xyz) * in.normal * wh.z;
    wn += mat3x3<f32>(skin.matrices[jh.w][0].xyz, skin.matrices[jh.w][1].xyz, skin.matrices[jh.w][2].xyz) * in.normal * wh.w;
    out.world_nrm   = normalize(wn);

    // Tangent LBS — same upper-left 3×3, bitangent sign (w) is invariant under LBS.
    if in.tangent.w != 0.0 {
        var wt = vec3<f32>(0.0);
        wt += mat3x3<f32>(skin.matrices[j.x][0].xyz, skin.matrices[j.x][1].xyz, skin.matrices[j.x][2].xyz) * in.tangent.xyz * w.x;
        wt += mat3x3<f32>(skin.matrices[j.y][0].xyz, skin.matrices[j.y][1].xyz, skin.matrices[j.y][2].xyz) * in.tangent.xyz * w.y;
        wt += mat3x3<f32>(skin.matrices[j.z][0].xyz, skin.matrices[j.z][1].xyz, skin.matrices[j.z][2].xyz) * in.tangent.xyz * w.z;
        wt += mat3x3<f32>(skin.matrices[j.w][0].xyz, skin.matrices[j.w][1].xyz, skin.matrices[j.w][2].xyz) * in.tangent.xyz * w.w;
        wt += mat3x3<f32>(skin.matrices[jh.x][0].xyz, skin.matrices[jh.x][1].xyz, skin.matrices[jh.x][2].xyz) * in.tangent.xyz * wh.x;
        wt += mat3x3<f32>(skin.matrices[jh.y][0].xyz, skin.matrices[jh.y][1].xyz, skin.matrices[jh.y][2].xyz) * in.tangent.xyz * wh.y;
        wt += mat3x3<f32>(skin.matrices[jh.z][0].xyz, skin.matrices[jh.z][1].xyz, skin.matrices[jh.z][2].xyz) * in.tangent.xyz * wh.z;
        wt += mat3x3<f32>(skin.matrices[jh.w][0].xyz, skin.matrices[jh.w][1].xyz, skin.matrices[jh.w][2].xyz) * in.tangent.xyz * wh.w;
        out.tangent = vec4<f32>(normalize(wt), in.tangent.w);
    } else {
        out.tangent = vec4<f32>(0.0);
    }

    out.uv           = in.texcoord;
    out.vertex_color = in.vertex_color;
    return out;
}

// ── PBR helper functions (identical to PBR_WGSL_SHADER) ──────────────────────
fn distribution_ggx(N: vec3<f32>, H: vec3<f32>, roughness: f32) -> f32 {
    let a   = roughness * roughness;
    let a2  = a * a;
    let nh  = max(dot(N, H), 0.0);
    let d   = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

fn geometry_schlick(ndotx: f32, k: f32) -> f32 {
    return ndotx / (ndotx * (1.0 - k) + k);
}

fn geometry_smith(ndotv: f32, ndotl: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) * 0.125;
    return geometry_schlick(max(ndotv, 0.0001), k)
         * geometry_schlick(max(ndotl, 0.0001), k);
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (1.0 - f0) * pow(saturate(1.0 - cos_theta), 5.0);
}

fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let inv_r = 1.0 - roughness;
    return f0 + (max(vec3<f32>(inv_r), f0) - f0) * pow(saturate(1.0 - cos_theta), 5.0);
}

fn env_brdf_approx(roughness: f32, ndotv: f32) -> vec2<f32> {
    let c0   = vec4<f32>(-1.0, -0.0275, -0.572,  0.022);
    let c1   = vec4<f32>( 1.0,  0.0425,  1.04,  -0.04);
    let r    = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * ndotv)) * r.x + r.y;
    return clamp(vec2<f32>(-1.04, 1.04) * a004 + r.zw, vec2<f32>(0.0), vec2<f32>(1.0));
}

// ── DomeLight environment IBL helpers ────────────────────────────────────────
// Rotate a world-space direction into the environment's sampling frame.
fn env_rotate(d: vec3<f32>) -> vec3<f32> {
    let m = mat3x3<f32>(env.rot0.xyz, env.rot1.xyz, env.rot2.xyz);
    return m * d;
}

// Lat-long (equirect) mapping matching the CPU `env_ibl` module: u wraps azimuth
// from -Z, v=0 is the +Y pole.
fn env_equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let u = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let v = acos(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

// Cosine-convolved order-2 SH irradiance E(n) (already π-scaled). `n_world` is a
// world-space normal; the environment rotation is applied here.
fn env_sh_irradiance(n_world: vec3<f32>) -> vec3<f32> {
    let n = normalize(env_rotate(n_world));
    let x = n.x;
    let y = n.y;
    let z = n.z;
    let A0 = PI;
    let A1 = 2.0 * PI / 3.0;
    let A2 = PI / 4.0;
    var e = vec3<f32>(0.0);
    e += env.sh[0].rgb * (A0 * 0.282095);
    e += env.sh[1].rgb * (A1 * 0.488603 * y);
    e += env.sh[2].rgb * (A1 * 0.488603 * z);
    e += env.sh[3].rgb * (A1 * 0.488603 * x);
    e += env.sh[4].rgb * (A2 * 1.092548 * x * y);
    e += env.sh[5].rgb * (A2 * 1.092548 * y * z);
    e += env.sh[6].rgb * (A2 * 0.315392 * (3.0 * z * z - 1.0));
    e += env.sh[7].rgb * (A2 * 1.092548 * x * z);
    e += env.sh[8].rgb * (A2 * 0.546274 * (x * x - y * y));
    return max(e, vec3<f32>(0.0));
}

// Roughness-selected equirect specular sample. `r_world` is a world-space
// reflection vector; roughness selects the prefiltered mip.
fn env_specular(r_world: vec3<f32>, roughness: f32) -> vec3<f32> {
    let uv  = env_equirect_uv(normalize(env_rotate(r_world)));
    let lod = roughness * env.params.x;
    return textureSampleLevel(env_tex, env_sampler, uv, lod).rgb;
}

fn swizzle_ch(v: vec4<f32>, ch: u32) -> f32 {
    if ch == 0u { return v.r; }
    if ch == 1u { return v.g; }
    if ch == 2u { return v.b; }
    return v.a;
}

fn compute_tbn(world_pos: vec3<f32>, N: vec3<f32>, uv: vec2<f32>, vtx_tangent: vec4<f32>) -> mat3x3<f32> {
    if vtx_tangent.w != 0.0 {
        var T = normalize(vtx_tangent.xyz - dot(vtx_tangent.xyz, N) * N);
        let B = cross(N, T) * vtx_tangent.w;
        return mat3x3<f32>(T, B, N);
    }
    let dp1  = dpdx(world_pos);
    let dp2  = dpdy(world_pos);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let det  = duv1.x * duv2.y - duv1.y * duv2.x;
    if abs(det) < 1e-7 {
        var up = vec3<f32>(0.0, 1.0, 0.0);
        if abs(N.y) > 0.99 { up = vec3<f32>(1.0, 0.0, 0.0); }
        let T = normalize(cross(up, N));
        let B = cross(N, T);
        return mat3x3<f32>(T, B, N);
    }
    let inv = 1.0 / det;
    var T = (dp1 * duv2.y - dp2 * duv1.y) * inv;
    T = normalize(T - dot(T, N) * N);
    let B = cross(N, T);
    return mat3x3<f32>(T, B, N);
}

// ── Fragment stage (identical to PBR_WGSL_SHADER) ────────────────────────────
@fragment
fn fs_main(
    in            : VertexOutput,
    @builtin(front_facing) front_facing: bool,
) -> @location(0) vec4<f32> {
    let uv = in.uv;

    let face_sign = select(-1.0, 1.0, front_facing);
    let N_geom    = normalize(in.world_nrm) * face_sign;

    var base_color : vec3<f32> = mat.base_color.rgb;
    var opacity    : f32       = mat.base_color.a;
    if (mat.features & FEAT_BASE_COLOR) != 0u {
        let s   = textureSample(tex_diffuse, mat_sampler, uv);
        base_color = s.rgb * mat.base_color.rgb;
        if (mat.features & FEAT_DIFFUSE_ALPHA) != 0u {
            opacity = s.a;
        }
    }

    base_color = base_color * in.vertex_color;

    if mat.mr_sub_thr.w > 0.0 && opacity < mat.mr_sub_thr.w {
        discard;
    }

    var N : vec3<f32> = N_geom;
    if (mat.features & FEAT_NORMAL) != 0u {
        let samp = textureSample(tex_normal, mat_sampler, uv);
        let xy   = samp.rg * 2.0 - 1.0;
        let z    = sqrt(max(1.0 - dot(xy, xy), 0.0));
        let ts_n = normalize(vec3<f32>(xy, z));
        let TBN  = compute_tbn(in.world_pos, N_geom, uv, in.tangent);
        N = normalize(TBN * ts_n);
    }

    var metallic  : f32 = mat.mr_sub_thr.x;
    var roughness : f32 = mat.mr_sub_thr.y;

    if (mat.features & FEAT_METALLIC) != 0u {
        metallic = swizzle_ch(
            textureSample(tex_metallic, mat_sampler, uv), mat.channels.x
        );
    }
    if (mat.features & FEAT_ROUGHNESS) != 0u {
        roughness = swizzle_ch(
            textureSample(tex_roughness, mat_sampler, uv), mat.channels.y
        );
    }
    roughness = clamp(roughness, 0.04, 1.0);

    var ao : f32 = 1.0;
    if (mat.features & FEAT_OCCLUSION) != 0u {
        ao = swizzle_ch(
            textureSample(tex_occlusion, mat_sampler, uv), mat.channels.z
        );
    }

    let F0 = mix(vec3<f32>(0.04), base_color, metallic);

    let V     = normalize(camera.eye_pos.xyz - in.world_pos);
    let NdotV = max(dot(N, V), 0.0001);

    var Lo      : vec3<f32> = vec3<f32>(0.0);  // direct diffuse only
    var spec    : vec3<f32> = vec3<f32>(0.0);  // all specular / reflection
    var ambient : vec3<f32> = vec3<f32>(0.0);

    for (var i : u32 = 0u; i < lights.count && i < MAX_LIGHTS; i++) {
        let light = lights.lights[i];

        if light.kind == LIGHT_DOME || light.kind == LIGHT_DOME_PROC {
            // Textured domes handled once from the global env uniform after the loop;
            // here only untextured/procedural domes add their flat analytic term.
            if env.params.z < 0.5 {
                let dome_col = light.color_pwr.rgb * light.color_pwr.w;
                let F_ibl  = fresnel_schlick_roughness(NdotV, F0, roughness);
                let kD_ibl = (vec3<f32>(1.0) - F_ibl) * (1.0 - metallic);
                let sky_t  = 0.5 + 0.5 * N.y;
                ambient += kD_ibl * dome_col * sky_t * (1.0 / PI);
                let ab = env_brdf_approx(roughness, NdotV);
                spec += dome_col * (F0 * ab.x + vec3<f32>(ab.y));
            }
            continue;
        }

        var L        : vec3<f32>;
        var radiance : vec3<f32>;

        if light.kind == LIGHT_DISTANT {
            L        = normalize(light.dir_or_pos.xyz);
            radiance = light.color_pwr.rgb * light.color_pwr.w;
        } else {
            let to_light = light.dir_or_pos.xyz - in.world_pos;
            let dist     = length(to_light);
            L        = to_light * (1.0 / max(dist, 0.0001));
            // Radius-aware inverse-square falloff: clamp the 1/r² singularity by
            // the light's own render-unit size (size.xy), not a fixed scene-scale
            // constant, so brightness is invariant to metersPerUnit. The bare 0.01
            // floor pinned attenuation at 100× for any centimetre-scale stage.
            let soft_r2 = max(light.size.x * light.size.x, light.size.y * light.size.y);
            let atten = 1.0 / max(dist * dist, max(soft_r2, 1e-6));
            radiance  = light.color_pwr.rgb * light.color_pwr.w * atten;
        }

        let NdotL = max(dot(N, L), 0.0);
        if NdotL < 0.0001 { continue; }

        let H = normalize(V + L);

        let D        = distribution_ggx(N, H, roughness);
        let G        = geometry_smith(NdotV, NdotL, roughness);
        let F        = fresnel_schlick(max(dot(H, V), 0.0), F0);
        let specular = (D * G * F) / max(4.0 * NdotV * NdotL, 0.001);

        let kD      = (vec3<f32>(1.0) - F) * (1.0 - metallic);
        let diffuse = kD * base_color / PI;

        // Per-light diffuse/specular multipliers (UsdLuxLightAPI, size.zw).
        // Shadow visibility (1.0 = lit) folds in per shadow-casting light; a light
        // with no atlas slot and the Off tier both return 1.0.
        let shadow = shadow_visibility(i, in.world_pos, NdotL);
        Lo   += diffuse  * light.size.z * radiance * NdotL * shadow;
        spec += specular * light.size.w * radiance * NdotL * shadow;
    }

    // Textured DomeLight IBL, applied once from the dominant environment: SH
    // directional diffuse irradiance (E(N), π-scaled) + roughness-mip equirect
    // specular through the same analytic split-sum BRDF the analytic path uses.
    if env.params.y > 0.5 && env.params.z > 0.5 {
        let F_ibl  = fresnel_schlick_roughness(NdotV, F0, roughness);
        let kD_ibl = (vec3<f32>(1.0) - F_ibl) * (1.0 - metallic);
        let ab     = env_brdf_approx(roughness, NdotV);
        // `env.ambient.rgb` = dome color x intensity x 2^exposure — scales the raw
        // texture radiance for both the SH diffuse and the equirect specular.
        let scale  = env.ambient.rgb;
        let irr    = env_sh_irradiance(N) * scale;
        ambient   += kD_ibl * irr * (1.0 / PI);
        let R      = reflect(-V, N);
        // Specular horizon occlusion: fade reflections whose mirror direction dips below
        // the surface horizon (strongest at grazing angles), preventing the environment
        // from leaking onto silhouettes. Frostbite/Lagarde form.
        var horizon = saturate(1.0 + dot(R, N));
        horizon     = horizon * horizon;
        // Split-sum: the env-BRDF LUT (ab) already integrates the Fresnel grazing term,
        // so scale the flat F0 — multiplying F_ibl here would double-count it and make
        // the IBL specular grow without bound as NdotV -> 0.
        spec      += env_specular(R, roughness) * scale * (F0 * ab.x + vec3<f32>(ab.y)) * horizon;
    }

    if lights.count == 0u {
        let amb    = vec3<f32>(0.03);
        let F_amb  = fresnel_schlick_roughness(NdotV, F0, roughness);
        let kD_amb = (vec3<f32>(1.0) - F_amb) * (1.0 - metallic);
        ambient    = kD_amb * amb;
        let ab_amb = env_brdf_approx(roughness, NdotV);
        spec += amb * (F0 * ab_amb.x + vec3<f32>(ab_amb.y));
    }

    var emissive : vec3<f32> = mat.emissive;
    if (mat.features & FEAT_EMISSIVE) != 0u {
        emissive = textureSample(tex_emissive, mat_sampler, uv).rgb;
    }

    // Split the shaded response into a transmissive part (diffuse + diffuse IBL,
    // tinted by base colour) and a reflective part (all specular/fresnel plus
    // emission). A transparent dielectric still reflects, so the reflective part
    // must survive transmission.
    let diffuse_total  = (ambient * base_color + Lo) * ao;
    let specular_total = spec * ao;
    let reflective     = specular_total + emissive;  // always visible

    var color : vec3<f32>;
    var out_alpha : f32;
    if (mat.features & FEAT_BLEND) != 0u {
        // Transmissive dielectric drawn with PREMULTIPLIED_ALPHA_BLENDING
        // (out = src.rgb + dst·(1−src.a)): emit premultiplied colour whose diffuse
        // body fades with opacity while reflection/emission stay at full strength,
        // and raise coverage by the reflection's luminance so bright specular glints
        // occlude the background instead of vanishing at opacity 0.
        color        = (reflective + diffuse_total * opacity) * camera.exposure;
        let refl_cov = saturate(luminance(reflective * camera.exposure));
        out_alpha    = saturate(opacity + (1.0 - opacity) * refl_cov);
    } else {
        // Opaque / mask: full colour (diffuse + reflection), coverage untouched.
        color     = (diffuse_total + reflective) * camera.exposure;
        out_alpha = opacity;
    }
    return vec4<f32>(color, out_alpha);
}
";

// ── Tonemapping WGSL Shader ───────────────────────────────────────────────────

/// Fullscreen ACES tonemapping pass.
///
/// Reads from the resolved single-sample HDR `Rgba16Float` texture produced by
/// the PBR pass and writes ACES-tonemapped values to the LDR `Rgba8UnormSrgb`
/// output. The hardware applies the linear→sRGB gamma on write automatically.
///
/// A fullscreen triangle is generated from three bare vertices (no vertex
/// buffer) using `vertex_index` to cover the entire viewport.
///
/// # Bind group layout (group 0)
///
/// | Binding | Type              | Description              |
/// |---------|-------------------|--------------------------|
/// | 0       | sampler           | nearest (no interpolation)|
/// | 1       | `texture_2d`\<f32\> | HDR `Rgba16Float` source   |
/// `DomeLight` environment drawn as the viewport backdrop (the "background" half of
/// the IBL feature — the same equirect that lights surfaces is shown behind them).
///
/// A fullscreen triangle whose per-pixel world-space view ray is reconstructed from
/// the inverse view-projection, then rotated into the environment frame (same
/// `env_rotate` / `env_equirect_uv` mapping as the PBR shader) and sampled at mip 0.
/// Emitted into the linear `HDR_FORMAT` target *after* all opaque geometry and
/// *before* transparent geometry, at the far plane with `depth_compare = LessEqual`
/// and no depth write, so it fills only pixels no opaque surface covered (early-Z
/// rejects the rest) and transparent surfaces blend over it.
/// The result is ACES-tonemapped by the shared tonemap pass, matching the surfaces.
///
/// Self-contained group 0 (own inverse-camera uniform + the shared env texture /
/// sampler / [`crate::pipeline::EnvUniforms`]); it does not touch the PBR layouts.
pub const SKYBOX_WGSL_SHADER: &str = r"
const PI : f32 = 3.14159265359;

struct SkyUniforms {
    inv_view_proj : mat4x4<f32>,
    eye_pos       : vec4<f32>,
}

struct EnvUniforms {
    sh      : array<vec4<f32>, 9>,
    rot0    : vec4<f32>,
    rot1    : vec4<f32>,
    rot2    : vec4<f32>,
    ambient : vec4<f32>,
    params  : vec4<f32>,
}

@group(0) @binding(0) var<uniform> sky         : SkyUniforms;
@group(0) @binding(1) var          env_tex     : texture_2d<f32>;
@group(0) @binding(2) var          env_sampler : sampler;
@group(0) @binding(3) var<uniform> env         : EnvUniforms;

struct VsOut {
    @builtin(position) clip_pos : vec4<f32>,
    @location(0)       ndc      : vec2<f32>,
}

// Fullscreen triangle at the far plane (z = 1 in clip space).
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.ndc = vec2<f32>(x, y);
    out.clip_pos = vec4<f32>(x, y, 1.0, 1.0);
    return out;
}

fn env_rotate(d: vec3<f32>) -> vec3<f32> {
    let m = mat3x3<f32>(env.rot0.xyz, env.rot1.xyz, env.rot2.xyz);
    return m * d;
}

fn env_equirect_uv(d: vec3<f32>) -> vec2<f32> {
    let u = 0.5 + atan2(d.x, -d.z) / (2.0 * PI);
    let v = acos(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Unproject the near and far points of this pixel to build the world ray.
    let near = sky.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far  = sky.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir  = normalize(far.xyz / far.w - near.xyz / near.w);
    let d    = normalize(env_rotate(dir));
    let uv   = env_equirect_uv(d);
    let radiance = textureSampleLevel(env_tex, env_sampler, uv, 0.0).rgb * env.ambient.rgb;
    return vec4<f32>(radiance, 1.0);
}
";

pub const TONEMAP_WGSL_SHADER: &str = r"
@group(0) @binding(0) var hdr_sampler : sampler;
@group(0) @binding(1) var hdr_texture : texture_2d<f32>;

struct VsOut {
    @builtin(position) pos : vec4<f32>,
    @location(0)       uv  : vec2<f32>,
}

// Fullscreen triangle: three vertices cover the entire NDC clip space.
// vi=0 → (-1,-1), vi=1 → (3,-1), vi=2 → (-1,3).
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Map NDC [-1,+1] → UV [0,1]; flip Y because NDC +y is up, texture +v is down.
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    return saturate((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let hdr = textureSample(hdr_texture, hdr_sampler, in.uv);
    let ldr = aces_tonemap(hdr.rgb);
    return vec4<f32>(ldr, hdr.a);
}
";

/// Straight texture-to-texture blit (fullscreen triangle + passthrough sample).
///
/// Used to copy one already-rendered eye image into the desktop preview
/// target during an `OpenXR` preview, so the mirror reuses the eye render
/// instead of rendering the scene a third time. Source and destination are
/// both `COLOR_FORMAT` (sRGB): `textureSample` decodes sRGB→linear on read and
/// the sRGB render target re-encodes linear→sRGB on store, so the copy is
/// pixel-exact. A linear sampler handles the eye→viewport size mismatch.
pub const BLIT_WGSL_SHADER: &str = r"
@group(0) @binding(0) var src_sampler : sampler;
@group(0) @binding(1) var src_texture : texture_2d<f32>;

struct VsOut {
    @builtin(position) pos : vec4<f32>,
    @location(0)       uv  : vec2<f32>,
}

// Fullscreen triangle: three vertices cover the entire NDC clip space.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Map NDC [-1,+1] → UV [0,1]; flip Y (NDC +y is up, texture +v is down).
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src_texture, src_sampler, in.uv);
}
";

/// Builds the depth-resolve fullscreen-pass WGSL for the given MSAA sample count.
///
/// wgpu can neither MSAA-resolve depth inside a render pass nor copy an MSAA
/// depth texture into a single-sample one, so the XR path runs this fullscreen
/// pass to produce the single-sample depth image that gets copied into the
/// `OpenXR` depth swapchain (`XR_KHR_composition_layer_depth`). The fragment reads
/// the source depth via `textureLoad` and writes `@builtin(frag_depth)`.
///
/// * **`sample_count > 1`** (2× / 4×): the source binds as
///   `texture_depth_multisampled_2d` and the fragment takes the **nearest**
///   (minimum) of the `sample_count` samples, so silhouette pixels keep the
///   closest surface's depth — the value the compositor wants for reprojection.
///   The number of `textureLoad`s is generated to match the runtime count (QI-2),
///   not hard-coded to 4.
/// * **`sample_count == 1`**: the source is already single-sample, so it binds as
///   a plain `texture_depth_2d` and the pass is a straight passthrough copy
///   (mip-level load). Lets the XR depth-layer path stay identical at 1×.
#[must_use]
pub fn depth_resolve_wgsl(sample_count: u32) -> String {
    let (binding_ty, body) = if sample_count > 1 {
        let mut body = String::from("    var d = textureLoad(depth_src, coord, 0);\n");
        for i in 1..sample_count {
            let _ = writeln!(body,
                "    d = min(d, textureLoad(depth_src, coord, {i}));"
            );
        }
        ("texture_depth_multisampled_2d", body)
    } else {
        // Single-sample: third arg is the mip level (0), not a sample index.
        (
            "texture_depth_2d",
            String::from("    let d = textureLoad(depth_src, coord, 0);\n"),
        )
    };

    format!(
        r"
@group(0) @binding(0) var depth_src : {binding_ty};

struct VsOut {{
    @builtin(position) pos : vec4<f32>,
}}

// Fullscreen triangle: three vertices cover the entire NDC clip space.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {{
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}}

@fragment
fn fs_main(in: VsOut) -> @builtin(frag_depth) f32 {{
    let coord = vec2<i32>(in.pos.xy);
{body}    return d;
}}
"
    )
}

/// Minimum reverse-Z depth the visionOS resolve ([`depth_resolve_reverse_z_wgsl`])
/// is allowed to emit, so the far plane never lands on the exact reverse-Z `0.0`.
///
/// The skybox/dome draws at the far plane with depth-write **disabled**, so sky
/// pixels keep the cleared forward-Z depth `1.0`, which the naive reverse-Z flip
/// (`1.0 - d`) turns into `0.0`. On the Vision Pro **device** compositor, depth
/// `0.0` reads as "no content": it drops the color and shows the immersive black
/// void, blacking out the whole dome (the Simulator is lenient, so it looked
/// fine there). Clamping the far plane to this small epsilon makes the backdrop
/// a valid, present-but-distant depth the compositor keeps and reprojects.
///
/// At `NEAR = 0.05` / `FAR = 5000`, `2.5e-4` reverse-Z ≈ a ~200 m view distance —
/// far enough that head-translation parallax against the dome is imperceptible,
/// nonzero enough to clear the compositor's "empty" sentinel. Lower it to push
/// the dome farther (if it visibly swims); raise it if the top is still black.
pub const VISIONOS_SKY_FAR_EPSILON: f32 = 2.5e-4;

/// Reverse-Z variant of [`DEPTH_RESOLVE_WGSL_SHADER`], used by the visionOS
/// present path. `CompositorServices` **only** supports reverse-Z depth (the
/// drawable depth texture must read 1.0 at the near plane and 0.0 at the far
/// plane — see `cp_drawable_set_depth_range`), but the renderer draws forward-Z
/// (near→0, far→1). This resolve flips the resolved value with `1.0 - d`, which
/// is algebraically the exact reverse-Z encoding of the same near/far planes, so
/// the compositor's depth-aware pose reprojection reads correct per-pixel depth.
///
/// The result is clamped to [`VISIONOS_SKY_FAR_EPSILON`] so the depth-write-less
/// skybox (which stays at the far plane) does not resolve to reverse-Z `0.0` —
/// the device compositor treats that as "no content" and blacks out the dome.
///
/// `min` still selects the nearest of the four MSAA samples in forward-Z, so
/// `1.0 - min(..)` is the nearest sample in reverse-Z (equivalently `max`).
#[must_use]
pub fn depth_resolve_reverse_z_wgsl(sample_count: u32) -> String {
    let (binding_ty, body) = if sample_count > 1 {
        let mut body = String::from("    var d = textureLoad(depth_src, coord, 0);\n");
        for i in 1..sample_count {
            let _ = writeln!(body,
                "    d = min(d, textureLoad(depth_src, coord, {i}));"
            );
        }
        ("texture_depth_multisampled_2d", body)
    } else {
        // Single-sample: third arg is the mip level (0), not a sample index.
        (
            "texture_depth_2d",
            String::from("    let d = textureLoad(depth_src, coord, 0);\n"),
        )
    };

    format!(
        r"
@group(0) @binding(0) var depth_src : {binding_ty};

struct VsOut {{
    @builtin(position) pos : vec4<f32>,
    }}

// Fullscreen triangle: three vertices cover the entire NDC clip space.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {{
    var out: VsOut;
    let x = select(-1.0, 3.0, vi == 1u);
    let y = select(-1.0, 3.0, vi == 2u);
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
    }}

@fragment
fn fs_main(in: VsOut) -> @builtin(frag_depth) f32 {{
    let coord = vec2<i32>(in.pos.xy);
    {body}
    // Forward-Z → reverse-Z: near 0→1, far 1→0, matching CompositorServices.
    // Clamp the far plane off exact 0.0 so the depth-write-less skybox/dome is
    // kept by the device compositor instead of being dropped as empty.
    return max(1.0 - d, {VISIONOS_SKY_FAR_EPSILON:e});
    }}
"
    )
}

// ── Wireframe shaders ─────────────────────────────────────────────────────────

/// Flat wireframe shader for static meshes.
/// Reads `mat.base_color.rgb` from `MaterialUniforms`; no lighting.
/// Compatible with the standard PBR pipeline layout (groups 0–3).
pub const WIREFRAME_WGSL_SHADER: &str = r"
struct CameraUniforms { view_proj: mat4x4<f32>, eye_pos: vec4<f32>, exposure: f32, _p1: f32, _p2: f32, _p3: f32, }
struct MeshUniforms { model: mat4x4<f32>, nrm_col0: vec4<f32>, nrm_col1: vec4<f32>, nrm_col2: vec4<f32>, }
struct MaterialUniforms { base_color: vec4<f32>, mr_sub_thr: vec4<f32>, emissive: vec3<f32>, features: u32, channels: vec4<u32>, }

@group(0) @binding(0) var<uniform> camera : CameraUniforms;
@group(2) @binding(0) var<uniform> mesh   : MeshUniforms;
@group(3) @binding(7) var<uniform> mat    : MaterialUniforms;

struct VertexInput { @location(0) position: vec3<f32>, }
struct VertexOutput { @builtin(position) clip_pos: vec4<f32>, }

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_pos = camera.view_proj * (mesh.model * vec4<f32>(in.position, 1.0));
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(mat.base_color.rgb, 1.0);
}
";

/// Flat wireframe shader for GPU-skinned meshes.
/// Applies LBS for vertex positions; outputs `mat.base_color.rgb`; no lighting.
pub const WIREFRAME_SKINNED_WGSL_SHADER: &str = r"
struct CameraUniforms { view_proj: mat4x4<f32>, eye_pos: vec4<f32>, exposure: f32, _p1: f32, _p2: f32, _p3: f32, }
struct MaterialUniforms { base_color: vec4<f32>, mr_sub_thr: vec4<f32>, emissive: vec3<f32>, features: u32, channels: vec4<u32>, }
struct SkinJoints { matrices: array<mat4x4<f32>, 256>, }

@group(0) @binding(0) var<uniform> camera : CameraUniforms;
@group(2) @binding(0) var<uniform> skin   : SkinJoints;
@group(3) @binding(7) var<uniform> mat    : MaterialUniforms;

struct VertexInput {
    @location(0) position    : vec3<f32>,
    @location(5) skin_joints     : vec4<u32>,
    @location(6) skin_weights    : vec4<f32>,
    @location(7) skin_joints_hi  : vec4<u32>,
    @location(8) skin_weights_hi : vec4<f32>,
}
struct VertexOutput { @builtin(position) clip_pos: vec4<f32>, }

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let j  = in.skin_joints;
    let w  = in.skin_weights;
    let jh = in.skin_joints_hi;
    let wh = in.skin_weights_hi;
    let p = vec4<f32>(in.position, 1.0);
    var wp4 = vec4<f32>(0.0);
    wp4 += skin.matrices[j.x] * p * w.x;
    wp4 += skin.matrices[j.y] * p * w.y;
    wp4 += skin.matrices[j.z] * p * w.z;
    wp4 += skin.matrices[j.w] * p * w.w;
    wp4 += skin.matrices[jh.x] * p * wh.x;
    wp4 += skin.matrices[jh.y] * p * wh.y;
    wp4 += skin.matrices[jh.z] * p * wh.z;
    wp4 += skin.matrices[jh.w] * p * wh.w;
    out.clip_pos = camera.view_proj * wp4;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(mat.base_color.rgb, 1.0);
}
";

/// Flat wireframe shader for hardware-instanced meshes.
/// Reads the per-instance model matrix from vertex buffer slot 1 (`InstanceRaw`);
/// outputs `mat.base_color.rgb`; no lighting.
pub const WIREFRAME_INSTANCED_WGSL_SHADER: &str = r"
struct CameraUniforms { view_proj: mat4x4<f32>, eye_pos: vec4<f32>, exposure: f32, _p1: f32, _p2: f32, _p3: f32, }
struct MaterialUniforms { base_color: vec4<f32>, mr_sub_thr: vec4<f32>, emissive: vec3<f32>, features: u32, channels: vec4<u32>, }

@group(0) @binding(0) var<uniform> camera : CameraUniforms;
@group(3) @binding(7) var<uniform> mat    : MaterialUniforms;

struct VertexInput { @location(0) position: vec3<f32>, }
struct InstanceInput {
    @location(5) model_col0: vec4<f32>,
    @location(6) model_col1: vec4<f32>,
    @location(7) model_col2: vec4<f32>,
    @location(8) model_col3: vec4<f32>,
}
struct VertexOutput { @builtin(position) clip_pos: vec4<f32>, }

@vertex
fn vs_main(in: VertexInput, inst: InstanceInput) -> VertexOutput {
    var out: VertexOutput;
    let model = mat4x4<f32>(inst.model_col0, inst.model_col1, inst.model_col2, inst.model_col3);
    out.clip_pos = camera.view_proj * (model * vec4<f32>(in.position, 1.0));
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(mat.base_color.rgb, 1.0);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_returns_same_shader_for_any_features() {
        let r#gen = UsdPbrGenerator;
        let s0 = r#gen.generate_wgsl(0);
        let s1 = r#gen.generate_wgsl(0xFF);
        assert!(std::ptr::eq(s0, s1), "same static string expected");
        assert!(
            s0.contains("Cook-Torrance"),
            "shader missing Cook-Torrance comment"
        );
    }

    #[test]
    fn pipeline_key_is_always_zero() {
        let r#gen = UsdPbrGenerator;
        assert_eq!(r#gen.pipeline_key(0), 0);
        assert_eq!(r#gen.pipeline_key(u64::MAX), 0);
    }

    #[test]
    fn default_pipeline_key_is_content_hash() {
        // A generator that does not override pipeline_key must derive distinct
        // keys from distinct WGSL sources and identical keys for identical sources.
        struct StaticGen(pub &'static str);
        impl SurfaceGenerator for StaticGen {
            fn generate_wgsl(&self, _: u64) -> &'static str {
                self.0
            }
            // pipeline_key intentionally uses the default implementation.
        }
        let a = StaticGen("// shader_a\n@vertex fn vs_main() {}");
        let b = StaticGen("// shader_b\n@vertex fn vs_main() {}");
        let same_as_a = StaticGen("// shader_a\n@vertex fn vs_main() {}");
        assert_ne!(
            a.pipeline_key(0),
            b.pipeline_key(0),
            "distinct WGSL must produce distinct keys"
        );
        assert_eq!(
            a.pipeline_key(0),
            same_as_a.pipeline_key(0),
            "identical WGSL must produce the same key"
        );
        // feature_bits must be irrelevant when WGSL is constant.
        assert_eq!(
            a.pipeline_key(0),
            a.pipeline_key(0xFF),
            "constant-WGSL generator must be feature_bits-stable"
        );
    }

    /// The derived UDIM shader must parse and pass naga validation (same
    /// front-end wgpu uses), so pipeline creation cannot fail on malformed WGSL.
    /// Also guards that every source rewrite actually matched (no leftover
    /// `texture_2d<f32>` sample, and the array bindings / UdimTable are present).
    #[test]
    fn udim_shader_is_valid_wgsl() {
        let src = pbr_udim_wgsl();

        // Rewrites landed: array textures, UdimTable binding, fetch helper.
        assert!(
            src.contains("texture_2d_array<f32>"),
            "textures must be arrays"
        );
        // The DomeLight env map is a plain equirect 2d texture and must survive the
        // blanket arrayify rewrite unchanged; it is the only plain 2d texture left.
        assert!(
            src.contains("env_tex      : texture_2d<f32>"),
            "env texture must stay a plain 2d texture"
        );
        assert_eq!(
            src.matches("texture_2d<f32>").count(),
            1,
            "only env_tex may remain plain 2d; every material texture must be arrayified"
        );
        assert!(
            src.contains("@group(3) @binding(8) var<uniform> udim"),
            "UdimTable binding"
        );
        assert!(src.contains("fn fetch_udim("), "fetch_udim helper present");
        assert!(
            src.contains("textureSampleGrad("),
            "seam-free grad sampling"
        );
        assert!(
            !src.contains("textureSample(tex_"),
            "all sample sites rewritten"
        );

        let module = naga::front::wgsl::parse_str(&src)
            .unwrap_or_else(|e| panic!("UDIM WGSL failed to parse:\n{}", e.emit_to_string(&src)));
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("UDIM WGSL failed validation: {e:?}"));
    }

    /// The instanced UDIM shader must parse and validate too, and it must retain
    /// the per-instance vertex input while gaining the same UDIM fragment rewrites
    /// as the static variant (shared [`apply_udim_rewrites`]).
    #[test]
    fn udim_instanced_shader_is_valid_wgsl() {
        let src = pbr_udim_instanced_wgsl();

        // Instanced vertex path preserved.
        assert!(
            src.contains("struct InstanceInput"),
            "per-instance input kept"
        );
        assert!(
            src.contains("fn vs_main(in: VertexInput, inst: InstanceInput)"),
            "instanced vs_main kept"
        );
        // UDIM fragment rewrites landed on the instanced source too.
        assert!(
            src.contains("texture_2d_array<f32>"),
            "textures must be arrays"
        );
        assert!(
            src.contains("env_tex      : texture_2d<f32>"),
            "env texture must stay a plain 2d texture"
        );
        assert_eq!(
            src.matches("texture_2d<f32>").count(),
            1,
            "only env_tex may remain plain 2d; every material texture must be arrayified"
        );
        assert!(
            src.contains("@group(3) @binding(8) var<uniform> udim"),
            "UdimTable binding"
        );
        assert!(src.contains("fn fetch_udim("), "fetch_udim helper present");
        assert!(
            src.contains("textureSampleGrad("),
            "seam-free grad sampling"
        );
        assert!(
            !src.contains("textureSample(tex_"),
            "all sample sites rewritten"
        );

        let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| {
            panic!(
                "instanced UDIM WGSL failed to parse:\n{}",
                e.emit_to_string(&src)
            )
        });
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("instanced UDIM WGSL failed validation: {e:?}"));
    }

    /// Parse + naga-validate a WGSL source, panicking with the emitted diagnostic.
    fn assert_valid_wgsl(label: &str, src: &str) {
        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("{label} failed to parse:\n{}", e.emit_to_string(src)));
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("{label} failed validation: {e:?}"));
    }

    /// The runtime-generated depth-resolve shader (QI-2) must parse + validate for
    /// every supported MSAA count. 2×/4× bind a multisampled source and `min` over
    /// exactly `n` samples; 1× binds a plain `texture_depth_2d` and passes through.
    /// A GPU-free guard that the per-count codegen never emits malformed WGSL.
    #[test]
    fn depth_resolve_shader_is_valid_for_each_sample_count() {
        for n in [1u32, 2, 4] {
            let src = super::depth_resolve_wgsl(n);
            assert_valid_wgsl(&format!("depth_resolve({n}x)"), &src);

            if n > 1 {
                assert!(
                    src.contains("texture_depth_multisampled_2d"),
                    "{n}x: multisampled source binding"
                );
                // One initial load + (n-1) min-folds = n textureLoads total.
                assert_eq!(
                    src.matches("textureLoad(").count(),
                    n as usize,
                    "{n}x: exactly n sample loads"
                );
            } else {
                assert!(
                    src.contains("texture_depth_2d") && !src.contains("multisampled"),
                    "1x: single-sample passthrough binding"
                );
                assert_eq!(
                    src.matches("textureLoad(").count(),
                    1,
                    "1x: single passthrough load"
                );
            }
        }
    }

    /// The reverse-Z depth-resolve shader (visionOS present path) must parse +
    /// validate for every supported MSAA count, and must **clamp** the far plane
    /// off exact reverse-Z `0.0`. The skybox/dome draws at the far plane without
    /// writing depth, so a bare `1.0 - d` resolves sky to `0.0`, which the Vision
    /// Pro device compositor drops as "no content" — blacking out the dome. The
    /// clamp to [`VISIONOS_SKY_FAR_EPSILON`] keeps it. GPU-free codegen guard.
    #[test]
    fn depth_resolve_reverse_z_clamps_far_plane_off_zero() {
        // The epsilon must be a usable reverse-Z depth: strictly inside (0, 1),
        // and small enough to read as a distant backdrop rather than near geometry.
        assert!(
            super::VISIONOS_SKY_FAR_EPSILON > 0.0 && super::VISIONOS_SKY_FAR_EPSILON < 1e-2,
            "sky far epsilon {} must be a small positive reverse-Z depth",
            super::VISIONOS_SKY_FAR_EPSILON
        );

        for n in [1u32, 2, 4] {
            let src = super::depth_resolve_reverse_z_wgsl(n);
            assert_valid_wgsl(&format!("depth_resolve_reverse_z({n}x)"), &src);

            // The flip is clamped, never a bare `return 1.0 - d;` that would let
            // the far plane resolve to 0.0 and drop the dome on device.
            assert!(
                src.contains("max(1.0 - d,"),
                "{n}x: far plane must be clamped off reverse-Z 0.0, got:\n{src}"
            );
            assert!(
                !src.contains("return 1.0 - d;"),
                "{n}x: unclamped reverse-Z flip would black out the dome on device"
            );
        }
    }

    /// Every base PBR shader (static, instanced, skinned) must parse and pass naga
    /// validation, and each must gate its output on `FEAT_BLEND` so transparent
    /// dielectrics fade their diffuse body with opacity while keeping specular
    /// reflection (a fully transparent glass still reflects lights). Otherwise a
    /// low-opacity surface either glows (non-premultiplied) or loses its glints
    /// (naive premultiply of the whole colour).
    #[test]
    fn base_pbr_shaders_premultiply_on_blend() {
        for (name, src) in [
            ("static", PBR_WGSL_SHADER),
            ("instanced", PBR_INSTANCED_WGSL_SHADER),
            ("skinned", PBR_SKINNED_WGSL_SHADER),
        ] {
            assert!(
                src.contains("const FEAT_BLEND"),
                "{name}: FEAT_BLEND constant present"
            );
            assert!(
                src.contains("mat.features & FEAT_BLEND"),
                "{name}: output gated on FEAT_BLEND"
            );
            // Diffuse body fades with opacity; the reflective part is not scaled by it.
            assert!(
                src.contains("diffuse_total * opacity"),
                "{name}: diffuse body must be premultiplied by opacity"
            );
            // Coverage is lifted by the reflection so specular glints occlude the bg.
            assert!(
                src.contains("saturate(luminance(reflective")
                    && src.contains("opacity + (1.0 - opacity)"),
                "{name}: reflection must raise coverage"
            );

            let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| {
                panic!(
                    "{name} PBR WGSL failed to parse:\n{}",
                    e.emit_to_string(src)
                )
            });
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            validator
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name} PBR WGSL failed validation: {e:?}"));
        }
    }

    /// The DomeLight skybox/background shader must parse and pass naga validation
    /// (same front-end wgpu uses), and its env binding layout must match the
    /// `SkyboxPipeline` bind-group layout it is compiled against.
    #[test]
    fn skybox_shader_is_valid_wgsl() {
        let src = SKYBOX_WGSL_SHADER;
        // Group-0 bindings the SkyboxPipeline layout provides (uniform, tex, sampler, uniform).
        assert!(
            src.contains("@group(0) @binding(0) var<uniform> sky"),
            "sky uniform binding"
        );
        assert!(
            src.contains("@group(0) @binding(1) var          env_tex"),
            "env texture binding"
        );
        assert!(
            src.contains("@group(0) @binding(2) var          env_sampler"),
            "env sampler binding"
        );
        assert!(
            src.contains("@group(0) @binding(3) var<uniform> env"),
            "env uniform binding"
        );
        assert!(
            src.contains("textureSampleLevel(env_tex"),
            "samples the env at an explicit mip"
        );

        let module = naga::front::wgsl::parse_str(src)
            .unwrap_or_else(|e| panic!("skybox WGSL failed to parse:\n{}", e.emit_to_string(src)));
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("skybox WGSL failed validation: {e:?}"));
    }

    /// All three beauty-pass shaders must carry the shared shadow sampling path
    /// (Phase 4: instanced + skinned mirror the base) and still be valid WGSL.
    #[test]
    fn all_pbr_shaders_have_shadow_sampling_and_validate() {
        for (label, src) in [
            ("PBR_WGSL_SHADER", PBR_WGSL_SHADER),
            ("PBR_INSTANCED_WGSL_SHADER", PBR_INSTANCED_WGSL_SHADER),
            ("PBR_SKINNED_WGSL_SHADER", PBR_SKINNED_WGSL_SHADER),
        ] {
            assert!(
                src.contains("fn shadow_visibility("),
                "{label} missing shadow_visibility"
            );
            assert!(
                src.contains("fn sample_shadow_pcss("),
                "{label} missing the PCSS soft-shadow path (Phase 5)"
            );
            assert!(
                src.contains("if shadows.global.x > 1.5 {"),
                "{label} missing the PCSS tier branch (global.x == 2.0)"
            );
            assert!(
                src.contains("if shadows.global.x > 2.5 {"),
                "{label} missing the true-hard single-tap tier branch (global.x == 3.0)"
            );
            assert!(
                src.contains("textureLoad(shadow_atlas,"),
                "{label} missing the PCSS blocker-search textureLoad"
            );
            assert!(
                src.contains("* radiance * NdotL * shadow;"),
                "{label} missing the per-light shadow multiply"
            );
            assert!(
                src.contains("var          shadow_atlas : texture_depth_2d_array;"),
                "{label} missing the group-1 shadow atlas binding"
            );
            assert_valid_wgsl(label, src);
        }
    }

    /// Documents and guards the per-shadowed-fragment shadow-sampling cost model
    /// that the GPU `metrics_shadow_tiers.json` sweep measures against. Each entry
    /// is `(tier, compare_taps, load_taps)` — `textureSampleCompareLevel` and
    /// `textureLoad` fetches issued by `shadow_visibility` for one shadowed
    /// fragment per caster. If a kernel size changes, this test fails and forces
    /// both the cost table and the JSON baseline interpretation to be revisited.
    ///
    /// - `Off`    : short-circuits to 1.0, zero fetches.
    /// - `Hard`   : 1 comparison tap (single-tap true hard).
    /// - `Smooth` : 3×3 = 9 comparison taps, averaged (`sum / 9.0`) — PCF.
    /// - `Soft`   : 7×7 = 49 `textureLoad` blocker search + 5×5 = 25 comparison
    ///              taps = 74 fetches total — PCSS.
    #[test]
    fn shadow_tap_cost_model() {
        // (tier label, compare taps, load taps, total)
        const COST: [(&str, u32, u32); 4] = [
            ("Off", 0, 0),
            ("Hard", 1, 0),
            ("Smooth", 9, 0),
            ("Soft", 25, 49),
        ];
        assert_eq!(
            COST[0].1 + COST[0].2,
            0,
            "Off must issue zero shadow fetches"
        );
        assert_eq!(
            COST[1].1 + COST[1].2,
            1,
            "Hard must be a single comparison tap"
        );
        assert_eq!(
            COST[2].1 + COST[2].2,
            9,
            "Smooth must be a 3×3 (9-tap) PCF kernel"
        );
        assert_eq!(
            COST[3].1 + COST[3].2,
            74,
            "Soft must be 49 blocker + 25 filter taps (PCSS)"
        );

        // Guard the kernel-loop signatures so a change to any kernel size trips
        // this test rather than silently invalidating the cost table above.
        for (label, src) in [
            ("PBR_WGSL_SHADER", PBR_WGSL_SHADER),
            ("PBR_INSTANCED_WGSL_SHADER", PBR_INSTANCED_WGSL_SHADER),
            ("PBR_SKINNED_WGSL_SHADER", PBR_SKINNED_WGSL_SHADER),
        ] {
            // Hard: exactly one comparison tap gated on the > 2.5 tier branch.
            assert!(
                src.contains(
                    "if shadows.global.x > 2.5 {\n        return textureSampleCompareLevel(shadow_atlas, shadow_samp, uv, slot, ref_depth);"
                ),
                "{label} hard tier is not a single unfiltered comparison tap"
            );
            // Smooth (PCF): 3×3 grid (dy/dx in -1..=1) averaged by /9.0.
            assert!(
                src.contains("dy : i32 = -1; dy <= 1;") && src.contains("return sum / 9.0;"),
                "{label} PCF kernel drifted from 3×3 /9.0"
            );
            // Soft (PCSS): 7×7 blocker search (search=3) + 5×5 filter (/25.0).
            assert!(
                src.contains("let search : i32 = 3;") && src.contains("return sum / 25.0;"),
                "{label} PCSS kernel drifted from 7×7 blocker + 5×5 /25.0"
            );
        }
    }

    /// The three depth-only caster shaders (static / instanced / skinned) must
    /// all parse and validate — pipeline creation cannot fail on malformed WGSL.
    #[test]
    fn shadow_depth_shaders_validate() {
        assert_valid_wgsl("PBR_SHADOW_DEPTH_WGSL", PBR_SHADOW_DEPTH_WGSL);
        assert_valid_wgsl(
            "PBR_SHADOW_DEPTH_INSTANCED_WGSL",
            PBR_SHADOW_DEPTH_INSTANCED_WGSL,
        );
        assert_valid_wgsl(
            "PBR_SHADOW_DEPTH_SKINNED_WGSL",
            PBR_SHADOW_DEPTH_SKINNED_WGSL,
        );
        // Instanced reads the per-instance matrix; skinned blends the joint palette.
        assert!(PBR_SHADOW_DEPTH_INSTANCED_WGSL.contains("struct InstanceInput"));
        assert!(PBR_SHADOW_DEPTH_SKINNED_WGSL.contains("skin.matrices[skin_joints.x]"));
    }

    /// The QI-4 Z-prepass depth shaders must be valid WGSL and must reproduce the
    /// beauty `view_proj * (model * p)` association (parenthesised model-first), so the
    /// `LessEqual` opaque test downstream never drops frontmost fragments.
    #[test]
    fn z_prepass_depth_shaders_validate() {
        assert_valid_wgsl("PBR_DEPTH_PREPASS_WGSL", PBR_DEPTH_PREPASS_WGSL);
        assert_valid_wgsl(
            "PBR_DEPTH_PREPASS_INSTANCED_WGSL",
            PBR_DEPTH_PREPASS_INSTANCED_WGSL,
        );
        // Static prepass transforms model-first then by view_proj (matches beauty).
        assert!(PBR_DEPTH_PREPASS_WGSL.contains("let wp4 = mesh.model * vec4<f32>(position, 1.0);"));
        assert!(PBR_DEPTH_PREPASS_WGSL.contains("return camera.view_proj * wp4;"));
        // Instanced prepass parenthesises model * p before view_proj.
        assert!(
            PBR_DEPTH_PREPASS_INSTANCED_WGSL
                .contains("return camera.view_proj * (model * vec4<f32>(position, 1.0));")
        );
    }

    /// The UDIM-derived instanced shader must also inherit the shadow path (it is
    /// rewritten from `PBR_INSTANCED_WGSL_SHADER`, which now samples shadows).
    #[test]
    fn udim_instanced_shader_keeps_shadow_sampling() {
        let src = pbr_udim_instanced_wgsl();
        assert!(
            src.contains("fn shadow_visibility("),
            "UDIM instanced lost shadow_visibility"
        );
        assert!(
            src.contains("* radiance * NdotL * shadow;"),
            "UDIM instanced lost shadow multiply"
        );
        assert_valid_wgsl("pbr_udim_instanced_wgsl", &src);
    }
}
