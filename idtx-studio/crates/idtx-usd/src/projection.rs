#[allow(clippy::wildcard_imports)]
use crate::*;
use std::collections::HashSet;

/// One prim in the composed topology projection: its identity, parent, name and
/// type, composition state, local specifier/layer, any packed reference/payload
/// arc, and the xformable/instance/prototype flags the renderer keys on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // structural flags mirror the FFI topology record
pub struct TopologyRecord {
    pub prim: PrimId,
    pub path: PathId,
    pub parent: Option<PrimId>,
    pub name: String,
    pub type_name: String,
    pub state: PrimState,
    pub is_pseudo_root: bool,
    pub local_specifier: Option<PrimSpecifier>,
    pub local_layer_identifier: Option<String>,
    pub packed_arc: Option<PackedArc>,
    pub internal_ref_target: Option<String>,
    pub is_xformable: bool,
    /// Native scenegraph instance (`instanceable = true`): a collapsed leaf whose
    /// geometry lives once in the shared prototype.
    pub is_instance: bool,
    /// Belongs to a shared instancing prototype (`/__Prototype_N`): inspectable once,
    /// never drawn directly.
    pub is_prototype: bool,
}

/// Incremental topology change: `upserts` are prims added or whose record
/// changed, `removed` are prims (by [`PrimId`]) that left the composed stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyDelta {
    pub upserts: Vec<TopologyRecord>,
    pub removed: HashSet<PrimId>,
}

/// The USD prim specifier authored in the edit-target layer: `def`, `over`, or
/// `class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimSpecifier {
    Def,
    Over,
    Class,
}

/// A single composition arc (reference or payload) packed onto a
/// [`TopologyRecord`], carrying the arc kind and the authored asset path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedArc {
    pub kind: PackedArcKind,
    pub asset_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackedArcKind {
    Reference,
    Payload,
}

/// Composition state flags for a prim: whether it is `active`, has a `def`
/// (`defined`), and — for payloads — whether its content is `loaded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimState {
    pub active: bool,
    pub defined: bool,
    pub loaded: bool,
}

/// A prim's resolved world transform as a column-major 4x4 matrix, keyed by
/// [`PrimId`].
#[derive(Debug, Clone, PartialEq)]
pub struct TransformRecord {
    pub prim: PrimId,
    pub world_matrix: [f32; 16],
}

/// A prim's resolved visibility, render purpose, and shadow-casting flag for
/// the viewport projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibilityRecord {
    pub prim: PrimId,
    pub visible: bool,
    pub purpose: RenderPurpose,
    /// `idtx:castsShadows` (default `true` when the attribute is absent).
    pub casts_shadow: bool,
}

/// Resolved `UsdGeomImageable` purpose of a prim (`default`/`proxy`/`render`/
/// `guide`), selecting which purposes a given render pass draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderPurpose {
    Default,
    Proxy,
    Render,
    Guide,
}

/// A prim's axis-aligned bounds in both world space (`minimum`/`maximum`) and
/// local space (`local_minimum`/`local_maximum`).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundsRecord {
    pub prim: PrimId,
    pub minimum: [f32; 3],
    pub maximum: [f32; 3],
    pub local_minimum: [f32; 3],
    pub local_maximum: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct CubeRecord {
    pub prim: PrimId,
    pub size: f32,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SphereRecord {
    pub prim: PrimId,
    pub radius: f32,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedGprimRecord {
    pub prim: PrimId,
    pub kind: ExtendedGprimKind,
    pub radius: f32,
    pub height: f32,
    pub axis: GprimAxis,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

/// A projected `UsdGeomPlane` — a finite, double-sided rectangle centred at the
/// origin. `axis` is the plane normal; `width` and `length` lie on the two axes
/// perpendicular to it (see [`GprimAxis`]). Rendered as two triangles; for physics
/// the same prim is an unbounded static half-space.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaneRecord {
    pub prim: PrimId,
    pub width: f32,
    pub length: f32,
    pub axis: GprimAxis,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendedGprimKind {
    Cone,
    Cylinder,
    Capsule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GprimAxis {
    X,
    #[default]
    Y,
    Z,
}

impl GprimAxis {
    /// Every variant, X/Y/Z order.
    pub const ALL: [GprimAxis; 3] = [GprimAxis::X, GprimAxis::Y, GprimAxis::Z];

    /// The USD token authored for this axis.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            GprimAxis::X => "X",
            GprimAxis::Y => "Y",
            GprimAxis::Z => "Z",
        }
    }

    /// Parse a USD axis token, or `None` if it is not `X`/`Y`/`Z`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<GprimAxis> {
        GprimAxis::ALL
            .into_iter()
            .find(|axis| axis.as_token() == token)
    }
}

/// A projected mesh: points, face-corner normals/tangents/texcoords, face
/// topology, display colours, bound material and per-subset assignments,
/// handedness/sidedness flags, and the subdivision scheme plus crease/corner
/// sharpness spec carried for the render layer.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshRecord {
    pub prim: PrimId,
    pub points: Vec<[f32; 3]>,
    /// Normals expanded to face-corner order by the C++ shim (all USD
    /// interpolation modes handled there). `len = face_vertex_indices.len()`.
    /// Empty when the mesh has no normals primvar; the render layer computes them.
    pub normals: Vec<[f32; 3]>,
    /// Tangents (xyz + bitangent sign w) in face-corner order.
    /// `len = face_vertex_indices.len()`. Empty when not authored.
    pub tangents: Vec<[f32; 4]>,
    pub texcoords: Vec<[f32; 2]>,
    pub face_vertex_counts: Vec<i32>,
    pub face_vertex_indices: Vec<i32>,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub material_subsets: Vec<MaterialSubsetRecord>,
    pub left_handed: bool,
    pub double_sided: bool,
    pub prototype_only: bool,
    /// Subdivision scheme token ("none"/"catmullClark"/"loop"/"bilinear"), empty
    /// when unauthored. Carried for the render layer's hard-edge synthesis.
    pub subdivision_scheme: String,
    /// Sharp-edge crease spec (point-index chains partitioned by `crease_lengths`).
    pub crease_indices: Vec<i32>,
    pub crease_lengths: Vec<i32>,
    pub crease_sharpnesses: Vec<f32>,
    /// Sharp-vertex corner spec (point indices + matching sharpness).
    pub corner_indices: Vec<i32>,
    pub corner_sharpnesses: Vec<f32>,
}

/// A projected `UsdGeomPointInstancer`: the prototype prim ids plus, per
/// instance, a stable native id, mask and instancer-local transform (including
/// the prototype root transform). Bounded runtime pools retain masked slots;
/// larger static pools retain the compact visible-only projection.
#[derive(Debug, Clone, PartialEq)]
pub struct PointInstancerRecord {
    pub prim: PrimId,
    pub prototypes: Vec<PrimId>,
    pub proto_indices: Vec<u32>,
    pub transforms: Vec<[f32; 16]>,
    pub ids: Vec<i64>,
    /// Empty means all visible, otherwise parallel to `ids` (0 = hidden).
    pub mask: Vec<u8>,
    /// Native constant/vertex displayOpacity expanded to slots; empty means 1.
    pub opacities: Vec<f32>,
}

/// A single native scenegraph instance (`instanceable = true`). The shared prototype's
/// geometry is projected once (as `prototype_only`); the instance's world placement is
/// carried by the ordinary transform projection for `prim`. The renderer groups instances
/// by `prototype` and expands them into hardware-instanced batches.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeInstanceRecord {
    pub prim: PrimId,
    pub prototype: PrimId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialSubsetRecord {
    pub material_path: Option<String>,
    pub face_indices: Vec<u32>,
}

/// A projected `UsdGeomCamera` with its full authored state: projection, lens
/// and aperture, clipping range and planes, depth-of-field, stereo role,
/// shutter, and the exposure parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraRecord {
    pub prim: PrimId,
    pub projection: String,
    pub focal_length: f64,
    pub horizontal_aperture: f64,
    pub vertical_aperture: f64,
    pub horizontal_aperture_offset: f64,
    pub vertical_aperture_offset: f64,
    pub clipping: [f64; 2],
    pub clipping_planes: Vec<[f64; 4]>,
    pub f_stop: f64,
    pub focus_distance: f64,
    pub stereo_role: String,
    pub shutter: [f64; 2],
    pub exposure: f64,
    pub exposure_iso: f64,
    pub exposure_time: f64,
    pub exposure_f_stop: f64,
    pub exposure_responsivity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraSelectionPolicy {
    /// Select the first composed camera in deterministic stage traversal.
    FirstInStageTraversal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraSelectionRecord {
    pub policy: CameraSelectionPolicy,
    pub selected: Option<PrimId>,
    pub authored_camera_count: usize,
    pub diagnostic: Option<String>,
}

/// The discriminated `UsdLux` light type with its shape parameters (distant
/// angle, sphere/disk radius, rect size, cylinder radius+length, dome).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LightKind {
    Distant { angle: f32 },
    Sphere { radius: f32 },
    Rect { width: f32, height: f32 },
    Disk { radius: f32 },
    Cylinder { radius: f32, length: f32 },
    Dome,
    DomeProcedural,
}

/// `UsdLuxDomeLight` `texture:format` — how the environment texture is projected.
///
/// Only [`LatLong`](Self::LatLong) (equirectangular) is fully rendered today;
/// [`Automatic`](Self::Automatic) resolves to lat-long when the image aspect is
/// ~2:1. The other formats are parsed but fall back to lat-long with a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DomeTextureFormat {
    /// Guess from the image aspect ratio (schema default).
    #[default]
    Automatic,
    /// Equirectangular latitude/longitude map.
    LatLong,
    MirroredBall,
    Angular,
    CubeMapVerticalCross,
}

impl DomeTextureFormat {
    /// Parse the USD `texture:format` token; unknown/empty ⇒ [`Automatic`](Self::Automatic).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "latlong" => Self::LatLong,
            "mirroredBall" => Self::MirroredBall,
            "angular" => Self::Angular,
            "cubeMapVerticalCross" => Self::CubeMapVerticalCross,
            _ => Self::Automatic,
        }
    }
}

/// `UsdLuxDomeLight` `poleAxis` — which world axis the lat-long poles map to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PoleAxis {
    /// Follow the stage up-axis (schema default).
    #[default]
    Scene,
    Y,
    Z,
}

impl PoleAxis {
    /// Parse the USD `poleAxis` token; unknown/empty ⇒ [`Scene`](Self::Scene).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "Y" => Self::Y,
            "Z" => Self::Z,
            _ => Self::Scene,
        }
    }
}

/// A projected `UsdLux` light: its kind and world matrix, linear colour (with
/// blackbody temperature already folded in), intensity/exposure/diffuse/
/// specular, normalize and shadow flags, optional shaping-cone parameters, raw
/// colour temperature, and dome environment texture path/format/pole-axis.
#[derive(Debug, Clone, PartialEq)]
pub struct LightRecord {
    pub prim: PrimId,
    pub kind: LightKind,
    pub world_matrix: [f32; 16],
    /// Linear rendering-space color after applying the official `OpenUSD`
    /// blackbody-temperature multiplier when requested by `LightAPI`.
    pub color: [f32; 3],
    pub intensity: f32,
    pub exposure: f32,
    pub diffuse: f32,
    pub specular: f32,
    pub normalize: bool,
    pub shadow_enabled: bool,
    /// True when a `UsdLuxShapingAPI` cone is authored (spotlight).
    pub has_shaping: bool,
    /// Shaping cone half-angle in degrees (180 = no cone). Valid when `has_shaping`.
    pub cone_angle: f32,
    /// Shaping cone softness in `[0, 1]`. Valid when `has_shaping`.
    pub cone_softness: f32,
    /// Color temperature in Kelvin for `UsdLuxBlackbodyTemperatureAsRgb` conversion.
    /// `None` when `enableColorTemperature` is not authored; `Some(K)` when present.
    /// The blackbody multiplier is already folded into `color` by the C++ bridge;
    /// this raw value is surfaced for the inspector and ambient reporting.
    pub temperature: Option<f32>,
    /// `UsdLuxDomeLight` resolved environment texture path, if authored. `None`
    /// for non-dome lights and untextured domes.
    pub dome_texture_path: Option<String>,
    /// Dome environment texture projection format (meaningful for dome kinds).
    pub dome_texture_format: DomeTextureFormat,
    /// Dome lat-long pole axis (meaningful for dome kinds).
    pub dome_pole_axis: PoleAxis,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonJointRecord {
    /// Composed joint path relative to the skeleton, in palette order.
    pub path: String,
    pub parent: Option<u32>,
    pub bind_transform: [f32; 16],
    pub rest_transform: [f32; 16],
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonTopologyRecord {
    pub skeleton: PrimId,
    pub joints: Vec<SkeletonJointRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonPoseRecord {
    pub skeleton: PrimId,
    pub animation_source: Option<PrimId>,
    pub skeleton_world_transform: [f32; 16],
    pub joint_local_transforms: Vec<[f32; 16]>,
    pub joint_world_transforms: Vec<[f32; 16]>,
    /// Inverse-bind-to-current transforms in skeleton space, in joint order.
    pub skinning_transforms: Vec<[f32; 16]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinningMethod {
    ClassicLinear,
    DualQuaternion,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinningExecution {
    GpuClassicLinear,
    CpuOpenUsd,
    RestPose,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VertexInfluences {
    pub joints: [u16; 8],
    pub weights: [f32; 8],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkinnedBoundsRecord {
    /// World-space bounds computed from official `OpenUSD` deformation.
    pub minimum: [f32; 3],
    pub maximum: [f32; 3],
}

/// A projected skinned prim: its bound skeleton, chosen skinning method and
/// execution path, geom-bind transform, up-to-eight-slot per-point influences,
/// optional CPU-skinned points, and optional deformed bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct SkinBindingRecord {
    pub prim: PrimId,
    pub skeleton: PrimId,
    pub method: SkinningMethod,
    pub execution: SkinningExecution,
    pub geom_bind_transform: [f32; 16],
    /// Up to eight normalized slots per point for portable GPU execution
    /// (unused slots carry joint 0 and weight 0.0). Empty for bounded CPU and
    /// rest-pose fallbacks.
    pub influences: Vec<VertexInfluences>,
    /// Skeleton-space points produced by official `OpenUSD`, populated only
    /// for the bounded CPU fallback.
    pub cpu_skinned_points: Vec<[f32; 3]>,
    pub skinned_bounds: Option<SkinnedBoundsRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkeletonDependencyRecord {
    pub skeleton: PrimId,
    pub animation_source: Option<PrimId>,
    pub skinned_prims: Vec<PrimId>,
}

/// One batched, self-consistent snapshot of everything the viewport renders for
/// a stage at a given `generation`/`revision` (and optional `time_code`):
/// transforms, visibility, bounds, every typed gprim/mesh/instancer, materials,
/// cameras, lights, skeleton topology/pose/skin data, and the selected camera.
/// `diagnostics` carries non-fatal projection notes.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportProjection {
    pub schema_version: u32,
    pub generation: StageGeneration,
    pub revision: StageRevision,
    /// `None` denotes `OpenUSD` default time; otherwise this is the composed
    /// host-stage time used for the single batched sample.
    pub time_code: Option<f64>,
    pub diagnostics: Vec<String>,
    pub transforms: Vec<TransformRecord>,
    pub visibility: Vec<VisibilityRecord>,
    pub bounds: Vec<BoundsRecord>,
    pub cubes: Vec<CubeRecord>,
    pub spheres: Vec<SphereRecord>,
    pub extended_gprims: Vec<ExtendedGprimRecord>,
    pub planes: Vec<PlaneRecord>,
    pub meshes: Vec<MeshRecord>,
    pub curves: Vec<crate::curve::BasisCurvesRecord>,
    pub point_instancers: Vec<PointInstancerRecord>,
    pub native_instances: Vec<NativeInstanceRecord>,
    pub materials: Vec<MaterialRecord>,
    pub cameras: Vec<CameraRecord>,
    pub lights: Vec<LightRecord>,
    pub skeleton_topologies: Vec<SkeletonTopologyRecord>,
    pub skeleton_poses: Vec<SkeletonPoseRecord>,
    pub skin_bindings: Vec<SkinBindingRecord>,
    pub skeleton_dependencies: Vec<SkeletonDependencyRecord>,
    pub camera_selection: CameraSelectionRecord,
}

impl ViewportProjection {
    /// Enforce sampling ownership after native dependency expansion. Metadata
    /// and shared materials remain available; only conflicting actor writes go.
    pub fn exclude_prims(&mut self, excluded: &HashSet<PrimId>) {
        macro_rules! keep { ($field:ident, $id:ident) => {
            self.$field.retain(|r| !excluded.contains(&r.$id));
        }; }
        keep!(transforms, prim); keep!(visibility, prim); keep!(bounds, prim);
        keep!(cubes, prim); keep!(spheres, prim); keep!(extended_gprims, prim);
        keep!(planes, prim); keep!(meshes, prim); keep!(curves, prim);
        keep!(point_instancers, prim); keep!(native_instances, prim);
        keep!(cameras, prim); keep!(lights, prim);
        keep!(skeleton_topologies, skeleton); keep!(skeleton_poses, skeleton);
        keep!(skin_bindings, prim); keep!(skeleton_dependencies, skeleton);
    }
}

pub(crate) fn flat_to_colors(values: &[f32]) -> Vec<[f32; 3]> {
    values.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}

pub(crate) fn color(values: &[f32]) -> Option<[f32; 3]> {
    (values.len() >= 3).then(|| [values[0], values[1], values[2]])
}

pub(crate) fn projection_matrix(
    values: Vec<f32>,
    path: &str,
    label: &str,
) -> Result<[f32; 16], StageError> {
    let matrix: [f32; 16] =
        values
            .try_into()
            .map_err(|values: Vec<f32>| StageError::InvalidProjection {
                path: path.to_owned(),
                detail: format!("{label} has {} values instead of 16", values.len()),
            })?;
    if matrix.iter().all(|value| value.is_finite()) {
        Ok(matrix)
    } else {
        Err(StageError::InvalidProjection {
            path: path.to_owned(),
            detail: format!("{label} contains a non-finite value"),
        })
    }
}

pub(crate) fn projection_matrices(
    values: &[f32],
    path: &str,
    label: &str,
) -> Result<Vec<[f32; 16]>, StageError> {
    if !values.len().is_multiple_of(16) {
        return Err(StageError::InvalidProjection {
            path: path.to_owned(),
            detail: format!("{label} has a non-matrix-aligned value count"),
        });
    }
    values
        .chunks_exact(16)
        .map(|matrix| {
            let matrix: [f32; 16] =
                matrix
                    .try_into()
                    .map_err(|_| StageError::InvalidProjection {
                        path: path.to_owned(),
                        detail: format!("{label} matrix conversion failed"),
                    })?;
            if !matrix.iter().all(|value| value.is_finite()) {
                return Err(StageError::InvalidProjection {
                    path: path.to_owned(),
                    detail: format!("{label} contains a non-finite value"),
                });
            }
            Ok(matrix)
        })
        .collect()
}

pub(crate) fn projection_points(
    values: &[f32],
    path: &str,
    label: &str,
) -> Result<Vec<[f32; 3]>, StageError> {
    if !values.len().is_multiple_of(3) {
        return Err(StageError::InvalidProjection {
            path: path.to_owned(),
            detail: format!("{label} has a non-vec3-aligned value count"),
        });
    }
    values
        .chunks_exact(3)
        .map(|point| {
            let point = [point[0], point[1], point[2]];
            if point.iter().all(|value| value.is_finite()) {
                Ok(point)
            } else {
                Err(StageError::InvalidProjection {
                    path: path.to_owned(),
                    detail: format!("{label} contains a non-finite value"),
                })
            }
        })
        .collect()
}
