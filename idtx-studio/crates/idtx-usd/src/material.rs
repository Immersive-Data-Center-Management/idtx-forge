#[allow(clippy::wildcard_imports)]
use crate::*;
use idtx_usd_sys::bridge::ffi;
use std::collections::HashSet;

/// Schema version of the material [`SurfaceIr`]. `surface_ir` rejects any FFI
/// material record whose `material_ir_version` differs from this value.
pub const MATERIAL_IR_SCHEMA_VERSION: u32 = 1;
/// Hard upper bound on texture inputs in one projected material; `surface_ir`
/// errors out when a record exceeds it, keeping the IR fixed-size.
pub const MAX_MATERIAL_TEXTURE_INPUTS: usize = 8;
/// Hard upper bound on distinct UV requirements a projected material may
/// carry; enforced in `surface_ir` after de-duplication.
pub const MAX_MATERIAL_UV_REQUIREMENTS: usize = 2;
/// Hard upper bound on distinct sampler descriptors a projected material may
/// carry; enforced in `surface_ir` after de-duplication.
pub const MAX_MATERIAL_SAMPLERS: usize = 8;
/// Hard upper bound on material source nodes retained in the projected IR;
/// enforced in `surface_ir`.
pub const MAX_MATERIAL_SOURCE_NODES: usize = 64;
/// Hard upper bound on material diagnostics retained in the projected IR;
/// enforced in `surface_ir`.
pub const MAX_MATERIAL_DIAGNOSTICS: usize = 64;

/// A projected material. The flat PBR fields (base colour, metallic, roughness,
/// textures, UV transform, …) are a compatibility adapter for scene/render; the
/// authoritative renderer-facing data is [`MaterialRecord::surface`], with
/// optional generated `wgsl_source` for `MaterialX` standard-surface materials.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialRecord {
    pub path: String,
    pub base_color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    /// Realtime diffuse-wrap approximation of `MaterialX` standard-surface subsurface.
    pub subsurface: f32,
    pub emissive: [f32; 3],
    /// Beer-Lambert transmission tint (glass absorption colour). `[1,1,1]` = no
    /// tint / unauthored. Drives the colored-glass blend in the render fallback.
    pub transmission_color: [f32; 3],
    pub opacity: f32,
    pub opacity_threshold: f32,
    pub diffuse_texture: Option<String>,
    pub normal_texture: Option<String>,
    pub metallic_texture: Option<String>,
    pub roughness_texture: Option<String>,
    pub emissive_texture: Option<String>,
    pub occlusion_texture: Option<String>,
    pub metallic_texture_channel: u32,
    pub roughness_texture_channel: u32,
    pub occlusion_texture_channel: u32,
    pub diffuse_uv_set: Option<String>,
    /// Row-major 2D affine transform `[m00, m01, m10, m11, tx, ty]` in USD UV space.
    pub diffuse_uv_transform: [f32; 6],
    pub uses_diffuse_texture_alpha: bool,
    pub diagnostics: Vec<String>,
    /// Versioned renderer-facing semantics. The flat fields above remain a
    /// compatibility adapter until scene/render consume this IR directly.
    pub surface: SurfaceIr,
    /// MaterialX-generated WGSL source, populated only when
    /// `surface.model == SurfaceModel::MaterialXStandardSurface` and the C++
    /// `WgslShaderGenerator` succeeds (`requires_phase2 == false`).
    /// `None` for `UsdPreviewSurface` materials and on any generation failure.
    pub wgsl_source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceModel {
    UsdPreviewSurface,
    MaterialXStandardSurface,
    DiagnosticFallback,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceFactors {
    pub base_color: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub subsurface: f32,
    pub emissive: [f32; 3],
    pub opacity: f32,
    pub opacity_threshold: f32,
}

/// Bit set of optional material capabilities (per-texture presence plus a
/// two-UV-set flag). `surface_ir` validates that the authored bits exactly
/// match the translated textures. Test membership with
/// [`MaterialFeatures::contains`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialFeatures(u64);

impl MaterialFeatures {
    pub const BASE_COLOR_TEXTURE: Self = Self(1 << 0);
    pub const NORMAL_TEXTURE: Self = Self(1 << 1);
    pub const METALLIC_TEXTURE: Self = Self(1 << 2);
    pub const ROUGHNESS_TEXTURE: Self = Self(1 << 3);
    pub const OCCLUSION_TEXTURE: Self = Self(1 << 4);
    pub const EMISSIVE_TEXTURE: Self = Self(1 << 5);
    pub const OPACITY_TEXTURE: Self = Self(1 << 6);
    pub const TWO_UV_SETS: Self = Self(1 << 7);
    const KNOWN: u64 = (1 << 8) - 1;

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, feature: Self) -> bool {
        self.0 & feature.0 == feature.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaPolicy {
    Opaque,
    Mask,
    Blend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialSidedness {
    Inherited,
    SingleSided,
    DoubleSided,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureSemantic {
    BaseColor,
    Normal,
    Metallic,
    Roughness,
    Occlusion,
    Emissive,
    Opacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureChannel {
    Red,
    Green,
    Blue,
    Alpha,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureSwizzle {
    pub channels: [TextureChannel; 4],
    pub component_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureAddressMode {
    Repeat,
    ClampToEdge,
    MirrorRepeat,
    ClampToBorder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFilterMode {
    Nearest,
    Linear,
    Cubic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureMipFilterMode {
    None,
    Nearest,
    Linear,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SamplerDescriptor {
    pub wrap_s: TextureAddressMode,
    pub wrap_t: TextureAddressMode,
    pub min_filter: TextureFilterMode,
    pub mag_filter: TextureFilterMode,
    pub mip_filter: TextureMipFilterMode,
    pub max_anisotropy: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureInterpretation {
    SrgbColor,
    LinearColor,
    Data,
    Normal,
    SceneLinearHdr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UvRequirement {
    pub name: String,
    /// Raw USD UV params `[tx, rotation_degrees, sx, 0.0, sy, ty]`.
    pub transform: [f32; 6],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialSourceNode {
    pub path: String,
    pub identifier: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialDiagnosticCode {
    UnsupportedSurface,
    UnsupportedNode,
    UnsupportedInput,
    UnsupportedConnection,
    GraphCycle,
    GraphDepthLimit,
    GraphNodeLimit,
    TextureLimit,
    UvLimit,
    InvalidSampler,
    InvalidColorSpace,
    MissingAsset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialDiagnostic {
    pub code: MaterialDiagnosticCode,
    pub source_path: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaterialIrFingerprint {
    pub first: u64,
    pub second: u64,
}

/// One texture binding in the material IR: its semantic, authored and resolved
/// asset paths, channel swizzle, post-swizzle `scale`/`bias` affine and
/// `fallback`, the UV-requirement and sampler indices it uses, colour-space /
/// interpretation, and the source-node provenance indices.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureInput {
    pub semantic: TextureSemantic,
    /// Authored asset identity, retained even when the resolver has no result.
    pub asset_path: String,
    pub resolved_asset_path: Option<String>,
    pub swizzle: TextureSwizzle,
    /// Post-swizzle affine sampling is `sample * scale + bias`.
    pub scale: [f32; 4],
    pub bias: [f32; 4],
    /// Source value used when the image cannot be read, before the affine step.
    pub fallback: [f32; 4],
    pub uv_requirement: u8,
    pub sampler: u8,
    pub source_color_space: Option<String>,
    pub interpretation: TextureInterpretation,
    pub source_node_indices: Vec<u8>,
}

/// Versioned, renderer-facing surface description: the surface model, feature
/// bits, scalar `factors`, de-duplicated texture inputs / UV requirements /
/// samplers, alpha and sidedness policy, source-node provenance, diagnostics,
/// and a content fingerprint independently re-derived at the FFI boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceIr {
    pub schema_version: u32,
    pub model: SurfaceModel,
    pub feature_bits: MaterialFeatures,
    pub factors: SurfaceFactors,
    pub texture_inputs: Vec<TextureInput>,
    pub uv_requirements: Vec<UvRequirement>,
    pub samplers: Vec<SamplerDescriptor>,
    pub alpha_policy: AlphaPolicy,
    pub sidedness: MaterialSidedness,
    pub source_nodes: Vec<MaterialSourceNode>,
    pub diagnostics: Vec<MaterialDiagnostic>,
    pub fingerprint: MaterialIrFingerprint,
}

/// The inspected shading-network graph bound to a prim: the primary and all
/// bound material paths, the shader nodes with their inputs/connections, the
/// material prim's `outputs:*` connections, and any diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialGraph {
    pub material_path: Option<String>,
    pub material_paths: Vec<String>,
    pub nodes: Vec<MaterialNode>,
    pub diagnostics: Vec<String>,
    /// `(outputs:NAME, full_connection_target)` for each `outputs:*` attribute
    /// on the material prim that has a connection, e.g.
    /// `("outputs:surface", "/World/M/surfaceShader.outputs:surface")`.
    pub material_output_connections: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialNode {
    pub path: String,
    pub identifier: String,
    pub inputs: Vec<MaterialInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialInput {
    pub name: String,
    pub value_summary: String,
    pub connections: Vec<String>,
}

/// Result of material-surface shader generation: the material path, the
/// generated shader source (WGSL when `requires_phase2 == false`, otherwise a
/// GLSL stub the caller must transpile), a content fingerprint, and
/// diagnostics.
#[derive(Debug, Clone)]
pub struct WgslMaterial {
    pub material_path: String,
    /// WGSL source when `requires_phase2 == false`; GLSL stub otherwise (naga pass pending).
    pub wgsl_source: String,
    /// `true` when the C++ side emitted a GLSL stub — the caller must run the naga
    /// GLSL→WGSL transpile pass before using `wgsl_source` as a shader.
    pub requires_phase2: bool,
    pub fingerprint: String,
    pub diagnostics: Vec<String>,
}

pub(crate) fn affine_uv_transform(values: &[f32], path: &str) -> Result<[f32; 6], StageError> {
    if values.is_empty() {
        return Ok([0.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
    }
    values
        .try_into()
        .map_err(|_| StageError::InvalidProjection {
            path: path.to_owned(),
            detail: format!(
                "diffuse UV transform returned {} values instead of six",
                values.len()
            ),
        })
}

pub(crate) fn material_projection_error(path: &str, detail: impl Into<String>) -> StageError {
    StageError::InvalidProjection {
        path: path.to_owned(),
        detail: detail.into(),
    }
}

pub(crate) fn float4(values: &[f32], path: &str, field: &str) -> Result<[f32; 4], StageError> {
    let values: [f32; 4] = values.try_into().map_err(|_| {
        material_projection_error(
            path,
            format!(
                "material {field} returned {} values instead of four",
                values.len()
            ),
        )
    })?;
    if values.iter().all(|value| value.is_finite()) {
        Ok(values)
    } else {
        Err(material_projection_error(
            path,
            format!("material {field} contains a non-finite value"),
        ))
    }
}

pub(crate) fn texture_semantic(value: &str, path: &str) -> Result<TextureSemantic, StageError> {
    match value {
        "base_color" => Ok(TextureSemantic::BaseColor),
        "normal" => Ok(TextureSemantic::Normal),
        "metallic" => Ok(TextureSemantic::Metallic),
        "roughness" => Ok(TextureSemantic::Roughness),
        "occlusion" => Ok(TextureSemantic::Occlusion),
        "emissive" => Ok(TextureSemantic::Emissive),
        "opacity" => Ok(TextureSemantic::Opacity),
        _ => Err(material_projection_error(
            path,
            format!("unknown texture semantic {value:?}"),
        )),
    }
}

pub(crate) fn texture_swizzle(value: &str, path: &str) -> Result<TextureSwizzle, StageError> {
    if value.is_empty() || value.len() > 4 {
        return Err(material_projection_error(
            path,
            format!("texture swizzle {value:?} does not contain one to four channels"),
        ));
    }
    let mut channels = [TextureChannel::Red; 4];
    for (index, channel) in value.bytes().enumerate() {
        channels[index] = match channel {
            b'r' => TextureChannel::Red,
            b'g' => TextureChannel::Green,
            b'b' => TextureChannel::Blue,
            b'a' => TextureChannel::Alpha,
            _ => {
                return Err(material_projection_error(
                    path,
                    format!("texture swizzle {value:?} contains an unknown channel"),
                ));
            }
        };
    }
    Ok(TextureSwizzle {
        channels,
        component_count: u8::try_from(value.len())
            .map_err(|_| material_projection_error(path, "texture swizzle length exceeds u8"))?,
    })
}

pub(crate) fn address_mode(value: &str, path: &str) -> Result<TextureAddressMode, StageError> {
    match value {
        "repeat" => Ok(TextureAddressMode::Repeat),
        "clamp_to_edge" => Ok(TextureAddressMode::ClampToEdge),
        "mirror_repeat" => Ok(TextureAddressMode::MirrorRepeat),
        "clamp_to_border" => Ok(TextureAddressMode::ClampToBorder),
        _ => Err(material_projection_error(
            path,
            format!("unknown texture address mode {value:?}"),
        )),
    }
}

pub(crate) fn filter_mode(value: &str, path: &str) -> Result<TextureFilterMode, StageError> {
    match value {
        "nearest" => Ok(TextureFilterMode::Nearest),
        "linear" => Ok(TextureFilterMode::Linear),
        "cubic" => Ok(TextureFilterMode::Cubic),
        _ => Err(material_projection_error(
            path,
            format!("unknown texture filter mode {value:?}"),
        )),
    }
}

pub(crate) fn mip_filter_mode(value: &str, path: &str) -> Result<TextureMipFilterMode, StageError> {
    match value {
        "none" => Ok(TextureMipFilterMode::None),
        "nearest" => Ok(TextureMipFilterMode::Nearest),
        "linear" => Ok(TextureMipFilterMode::Linear),
        _ => Err(material_projection_error(
            path,
            format!("unknown texture mip-filter mode {value:?}"),
        )),
    }
}

pub(crate) fn texture_interpretation(
    value: &str,
    path: &str,
) -> Result<TextureInterpretation, StageError> {
    match value {
        "srgb_color" => Ok(TextureInterpretation::SrgbColor),
        "linear_color" => Ok(TextureInterpretation::LinearColor),
        "data" => Ok(TextureInterpretation::Data),
        "normal" => Ok(TextureInterpretation::Normal),
        "scene_linear_hdr" => Ok(TextureInterpretation::SceneLinearHdr),
        _ => Err(material_projection_error(
            path,
            format!("unknown texture interpretation {value:?}"),
        )),
    }
}

pub(crate) fn diagnostic_code(
    value: &str,
    path: &str,
) -> Result<MaterialDiagnosticCode, StageError> {
    match value {
        "unsupported_surface" => Ok(MaterialDiagnosticCode::UnsupportedSurface),
        "unsupported_node" => Ok(MaterialDiagnosticCode::UnsupportedNode),
        "unsupported_input" => Ok(MaterialDiagnosticCode::UnsupportedInput),
        "unsupported_connection" => Ok(MaterialDiagnosticCode::UnsupportedConnection),
        "graph_cycle" => Ok(MaterialDiagnosticCode::GraphCycle),
        "graph_depth_limit" => Ok(MaterialDiagnosticCode::GraphDepthLimit),
        "graph_node_limit" => Ok(MaterialDiagnosticCode::GraphNodeLimit),
        "texture_limit" => Ok(MaterialDiagnosticCode::TextureLimit),
        "uv_limit" => Ok(MaterialDiagnosticCode::UvLimit),
        "invalid_sampler" => Ok(MaterialDiagnosticCode::InvalidSampler),
        "invalid_color_space" => Ok(MaterialDiagnosticCode::InvalidColorSpace),
        "missing_asset" => Ok(MaterialDiagnosticCode::MissingAsset),
        _ => Err(material_projection_error(
            path,
            format!("unknown material diagnostic code {value:?}"),
        )),
    }
}

pub(crate) fn same_uv(left: &UvRequirement, right: &UvRequirement) -> bool {
    left.name == right.name
        && left
            .transform
            .iter()
            .zip(right.transform)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

pub(crate) struct MaterialIrHasher {
    first: u64,
    second: u64,
}

impl MaterialIrHasher {
    fn new() -> Self {
        Self {
            first: 1_469_598_103_934_665_603,
            second: 7_809_847_782_465_536_322,
        }
    }

    fn byte(&mut self, byte: u8) {
        self.first ^= u64::from(byte);
        self.first = self.first.wrapping_mul(1_099_511_628_211);
        self.second ^= u64::from(byte) + 0x9e;
        self.second = self.second.wrapping_mul(14_029_467_366_897_019_727);
        self.second ^= self.second >> 29;
    }

    fn u32(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.byte(byte);
        }
    }

    fn u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.byte(byte);
        }
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    fn string(&mut self, value: &str) {
        self.u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        for byte in value.bytes() {
            self.byte(byte);
        }
    }

    fn f32s(&mut self, values: &[f32]) {
        self.u64(u64::try_from(values.len()).unwrap_or(u64::MAX));
        for value in values {
            self.f32(*value);
        }
    }

    fn u32s(&mut self, values: &[u32]) {
        self.u64(u64::try_from(values.len()).unwrap_or(u64::MAX));
        for value in values {
            self.u32(*value);
        }
    }

    fn finish(self) -> MaterialIrFingerprint {
        MaterialIrFingerprint {
            first: self.first,
            second: self.second,
        }
    }
}

pub(crate) fn raw_material_ir_fingerprint(raw: &ffi::MaterialRecordRaw) -> MaterialIrFingerprint {
    let mut fingerprint = MaterialIrHasher::new();
    fingerprint.u32(raw.material_ir_version);
    fingerprint.string(&raw.material_model);
    fingerprint.u64(raw.material_feature_bits);
    fingerprint.string(&raw.alpha_policy);
    fingerprint.string(&raw.sidedness);
    fingerprint.f32s(&raw.base_color);
    fingerprint.f32(raw.metallic);
    fingerprint.f32(raw.roughness);
    fingerprint.f32(raw.subsurface);
    fingerprint.f32s(&raw.emissive);
    fingerprint.f32s(&raw.transmission_color);
    fingerprint.f32(raw.opacity);
    fingerprint.f32(raw.opacity_threshold);
    fingerprint.u64(u64::try_from(raw.texture_inputs.len()).unwrap_or(u64::MAX));
    for texture in &raw.texture_inputs {
        fingerprint.string(&texture.semantic);
        fingerprint.string(&texture.asset_path);
        fingerprint.string(&texture.resolved_asset_path);
        fingerprint.string(&texture.swizzle);
        fingerprint.f32s(&texture.scale);
        fingerprint.f32s(&texture.bias);
        fingerprint.f32s(&texture.fallback);
        fingerprint.string(&texture.uv_name);
        fingerprint.f32s(&texture.uv_transform);
        fingerprint.string(&texture.wrap_s);
        fingerprint.string(&texture.wrap_t);
        fingerprint.string(&texture.min_filter);
        fingerprint.string(&texture.mag_filter);
        fingerprint.string(&texture.mip_filter);
        fingerprint.u32(texture.max_anisotropy);
        fingerprint.string(&texture.source_color_space);
        fingerprint.string(&texture.interpretation);
        fingerprint.u32s(&texture.source_node_indices);
    }
    fingerprint.u64(u64::try_from(raw.source_nodes.len()).unwrap_or(u64::MAX));
    for node in &raw.source_nodes {
        fingerprint.string(&node.path);
        fingerprint.string(&node.identifier);
    }
    fingerprint.u64(u64::try_from(raw.material_diagnostics.len()).unwrap_or(u64::MAX));
    for diagnostic in &raw.material_diagnostics {
        fingerprint.string(&diagnostic.code);
        fingerprint.string(&diagnostic.source_path);
        fingerprint.string(&diagnostic.message);
    }
    fingerprint.finish()
}

pub(crate) fn parse_material_ir_fingerprint(
    value: &str,
    path: &str,
) -> Result<MaterialIrFingerprint, StageError> {
    let (first, second) = value.split_once(':').ok_or_else(|| {
        material_projection_error(path, "material IR fingerprint has an invalid encoding")
    })?;
    Ok(MaterialIrFingerprint {
        first: first.parse().map_err(|_| {
            material_projection_error(path, "material IR fingerprint first word is invalid")
        })?,
        second: second.parse().map_err(|_| {
            material_projection_error(path, "material IR fingerprint second word is invalid")
        })?,
    })
}

pub(crate) fn surface_ir(
    raw: &ffi::MaterialRecordRaw,
    factors: SurfaceFactors,
) -> Result<SurfaceIr, StageError> {
    let path = raw.path.as_str();
    if raw.material_ir_version != MATERIAL_IR_SCHEMA_VERSION {
        return Err(material_projection_error(
            path,
            format!(
                "material IR schema version {} is unsupported",
                raw.material_ir_version
            ),
        ));
    }
    if raw.texture_inputs.len() > MAX_MATERIAL_TEXTURE_INPUTS
        || raw.source_nodes.len() > MAX_MATERIAL_SOURCE_NODES
        || raw.material_diagnostics.len() > MAX_MATERIAL_DIAGNOSTICS
    {
        return Err(material_projection_error(
            path,
            "material IR exceeds a fixed projection bound",
        ));
    }
    let model = match raw.material_model.as_str() {
        "usd_preview_surface" => SurfaceModel::UsdPreviewSurface,
        "materialx_standard_surface" => SurfaceModel::MaterialXStandardSurface,
        "diagnostic_fallback" => SurfaceModel::DiagnosticFallback,
        value => {
            return Err(material_projection_error(
                path,
                format!("unknown material model {value:?}"),
            ));
        }
    };
    if raw.material_feature_bits & !MaterialFeatures::KNOWN != 0 {
        return Err(material_projection_error(
            path,
            "material feature bits contain an unknown flag",
        ));
    }
    let alpha_policy = match raw.alpha_policy.as_str() {
        "opaque" => AlphaPolicy::Opaque,
        "mask" => AlphaPolicy::Mask,
        "blend" => AlphaPolicy::Blend,
        value => {
            return Err(material_projection_error(
                path,
                format!("unknown alpha policy {value:?}"),
            ));
        }
    };
    let sidedness = match raw.sidedness.as_str() {
        "inherited" => MaterialSidedness::Inherited,
        "single_sided" => MaterialSidedness::SingleSided,
        "double_sided" => MaterialSidedness::DoubleSided,
        value => {
            return Err(material_projection_error(
                path,
                format!("unknown material sidedness {value:?}"),
            ));
        }
    };
    let source_nodes = raw
        .source_nodes
        .iter()
        .map(|node| {
            if node.path.is_empty() || node.identifier.is_empty() {
                return Err(material_projection_error(
                    path,
                    "material source node has an empty path or identifier",
                ));
            }
            Ok(MaterialSourceNode {
                path: node.path.clone(),
                identifier: node.identifier.clone(),
            })
        })
        .collect::<Result<Vec<_>, StageError>>()?;
    let diagnostics = raw
        .material_diagnostics
        .iter()
        .map(|diagnostic| {
            Ok(MaterialDiagnostic {
                code: diagnostic_code(&diagnostic.code, path)?,
                source_path: diagnostic.source_path.clone(),
                message: diagnostic.message.clone(),
            })
        })
        .collect::<Result<Vec<_>, StageError>>()?;

    let mut uv_requirements = Vec::<UvRequirement>::new();
    let mut samplers = Vec::<SamplerDescriptor>::new();
    let mut texture_inputs = Vec::with_capacity(raw.texture_inputs.len());
    let mut semantics = HashSet::new();
    let mut expected_features = 0_u64;
    for texture in &raw.texture_inputs {
        if texture.asset_path.is_empty() {
            return Err(material_projection_error(
                path,
                "material texture has an empty authored asset path",
            ));
        }
        let semantic = texture_semantic(&texture.semantic, path)?;
        if !semantics.insert(semantic) {
            return Err(material_projection_error(
                path,
                format!("material has duplicate {semantic:?} texture semantics"),
            ));
        }
        expected_features |= match semantic {
            TextureSemantic::BaseColor => MaterialFeatures::BASE_COLOR_TEXTURE.bits(),
            TextureSemantic::Normal => MaterialFeatures::NORMAL_TEXTURE.bits(),
            TextureSemantic::Metallic => MaterialFeatures::METALLIC_TEXTURE.bits(),
            TextureSemantic::Roughness => MaterialFeatures::ROUGHNESS_TEXTURE.bits(),
            TextureSemantic::Occlusion => MaterialFeatures::OCCLUSION_TEXTURE.bits(),
            TextureSemantic::Emissive => MaterialFeatures::EMISSIVE_TEXTURE.bits(),
            TextureSemantic::Opacity => MaterialFeatures::OPACITY_TEXTURE.bits(),
        };
        let uv = UvRequirement {
            name: texture.uv_name.clone(),
            transform: affine_uv_transform(&texture.uv_transform, path)?,
        };
        if uv.name.is_empty() || !uv.transform.iter().all(|value| value.is_finite()) {
            return Err(material_projection_error(
                path,
                "material UV requirement is empty or non-finite",
            ));
        }
        let uv_requirement = uv_requirements
            .iter()
            .position(|existing| same_uv(existing, &uv))
            .unwrap_or_else(|| {
                uv_requirements.push(uv);
                uv_requirements.len() - 1
            });
        let sampler = SamplerDescriptor {
            wrap_s: address_mode(&texture.wrap_s, path)?,
            wrap_t: address_mode(&texture.wrap_t, path)?,
            min_filter: filter_mode(&texture.min_filter, path)?,
            mag_filter: filter_mode(&texture.mag_filter, path)?,
            mip_filter: mip_filter_mode(&texture.mip_filter, path)?,
            max_anisotropy: u8::try_from(texture.max_anisotropy)
                .map_err(|_| material_projection_error(path, "sampler anisotropy exceeds u8"))?,
        };
        if !(1..=16).contains(&sampler.max_anisotropy) {
            return Err(material_projection_error(
                path,
                "sampler anisotropy is outside [1, 16]",
            ));
        }
        let sampler_index = samplers
            .iter()
            .position(|existing| existing == &sampler)
            .unwrap_or_else(|| {
                samplers.push(sampler);
                samplers.len() - 1
            });
        let source_node_indices = texture
            .source_node_indices
            .iter()
            .map(|index| {
                let index = usize::try_from(*index).map_err(|_| {
                    material_projection_error(path, "source-node index cannot fit usize")
                })?;
                if index >= source_nodes.len() {
                    return Err(material_projection_error(
                        path,
                        "texture provenance references an unknown source node",
                    ));
                }
                u8::try_from(index)
                    .map_err(|_| material_projection_error(path, "source-node index cannot fit u8"))
            })
            .collect::<Result<Vec<_>, StageError>>()?;
        texture_inputs.push(TextureInput {
            semantic,
            asset_path: texture.asset_path.clone(),
            resolved_asset_path: (!texture.resolved_asset_path.is_empty())
                .then(|| texture.resolved_asset_path.clone()),
            swizzle: texture_swizzle(&texture.swizzle, path)?,
            scale: float4(&texture.scale, path, "texture scale")?,
            bias: float4(&texture.bias, path, "texture bias")?,
            fallback: float4(&texture.fallback, path, "texture fallback")?,
            uv_requirement: u8::try_from(uv_requirement).map_err(|_| {
                material_projection_error(path, "UV requirement index cannot fit u8")
            })?,
            sampler: u8::try_from(sampler_index)
                .map_err(|_| material_projection_error(path, "sampler index cannot fit u8"))?,
            source_color_space: (!texture.source_color_space.is_empty())
                .then(|| texture.source_color_space.clone()),
            interpretation: texture_interpretation(&texture.interpretation, path)?,
            source_node_indices,
        });
    }
    if uv_requirements.len() > MAX_MATERIAL_UV_REQUIREMENTS
        || samplers.len() > MAX_MATERIAL_SAMPLERS
    {
        return Err(material_projection_error(
            path,
            "material IR exceeds the UV or sampler descriptor bound",
        ));
    }
    if uv_requirements.len() == 2 {
        expected_features |= MaterialFeatures::TWO_UV_SETS.bits();
    }
    if raw.material_feature_bits != expected_features {
        return Err(material_projection_error(
            path,
            format!(
                "material feature bits {:#x} do not match translated textures {expected_features:#x}",
                raw.material_feature_bits
            ),
        ));
    }
    let fingerprint = parse_material_ir_fingerprint(&raw.material_ir_fingerprint, path)?;
    let independently_computed = raw_material_ir_fingerprint(raw);
    if fingerprint != independently_computed {
        return Err(material_projection_error(
            path,
            "material IR fingerprint failed independent boundary validation",
        ));
    }
    Ok(SurfaceIr {
        schema_version: raw.material_ir_version,
        model,
        feature_bits: MaterialFeatures(raw.material_feature_bits),
        factors,
        texture_inputs,
        uv_requirements,
        samplers,
        alpha_policy,
        sidedness,
        source_nodes,
        diagnostics,
        fingerprint,
    })
}

pub(crate) fn logical_material_bytes(material: &MaterialRecord) -> usize {
    let compatibility_strings = material.path.len()
        + [
            material.diffuse_texture.as_deref(),
            material.normal_texture.as_deref(),
            material.metallic_texture.as_deref(),
            material.roughness_texture.as_deref(),
            material.emissive_texture.as_deref(),
            material.occlusion_texture.as_deref(),
            material.diffuse_uv_set.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::len)
        .sum::<usize>()
        + material.diagnostics.iter().map(String::len).sum::<usize>();
    let texture_bytes = material
        .surface
        .texture_inputs
        .iter()
        .map(|texture| {
            std::mem::size_of::<TextureInput>()
                + texture.asset_path.len()
                + texture.resolved_asset_path.as_deref().map_or(0, str::len)
                + texture.source_color_space.as_deref().map_or(0, str::len)
                + texture.source_node_indices.len() * std::mem::size_of::<u8>()
        })
        .sum::<usize>();
    let uv_bytes = material
        .surface
        .uv_requirements
        .iter()
        .map(|uv| std::mem::size_of::<UvRequirement>() + uv.name.len())
        .sum::<usize>();
    let source_node_bytes = material
        .surface
        .source_nodes
        .iter()
        .map(|node| {
            std::mem::size_of::<MaterialSourceNode>() + node.path.len() + node.identifier.len()
        })
        .sum::<usize>();
    let diagnostic_bytes = material
        .surface
        .diagnostics
        .iter()
        .map(|diagnostic| {
            std::mem::size_of::<MaterialDiagnostic>()
                + diagnostic.source_path.len()
                + diagnostic.message.len()
        })
        .sum::<usize>();
    std::mem::size_of::<MaterialRecord>()
        + compatibility_strings
        + texture_bytes
        + uv_bytes
        + material.surface.samplers.len() * std::mem::size_of::<SamplerDescriptor>()
        + source_node_bytes
        + diagnostic_bytes
}

/// Transpiles a complete GLSL fragment-shader source to WGSL using the naga GLSL front-end.
///
/// The naga GLSL front-end requires a fully-formed GLSL source with a `#version` directive and
/// a `main()` entry point. The C++ bridge emits such a stub when `IDTX_HAVE_MATERIALX_WGSL`
/// is not defined at build time. A successful return means the transpiled WGSL is valid and
/// can be compiled by wgpu, but it may still be a grey-placeholder stub rather than the real
/// MaterialX surface evaluation — the caller must check for `IDTX_MATERIALX_GLSL_STUB` to
/// distinguish the two cases.
#[cfg(feature = "materialx-naga")]
pub(crate) fn transpile_glsl_to_wgsl(glsl_source: &str) -> Result<String, String> {
    use naga::back::wgsl as wgsl_out;
    use naga::front::glsl as glsl_in;
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    let options = glsl_in::Options::from(naga::ShaderStage::Fragment);
    let mut frontend = glsl_in::Frontend::default();
    let module = frontend
        .parse(&options, glsl_source)
        .map_err(|e| format!("GLSL parse: {e}"))?;

    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::default());
    let info = validator
        .validate(&module)
        .map_err(|e| format!("WGSL validate: {e}"))?;

    wgsl_out::write_string(&module, &info, wgsl_out::WriterFlags::empty())
        .map_err(|e| format!("WGSL write: {e}"))
}
