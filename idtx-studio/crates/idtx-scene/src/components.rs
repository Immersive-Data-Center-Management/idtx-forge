//! ECS component types hydrated from `ViewportProjection` and topology records.
//!
//! Every spawned entity has at minimum a [`PrimNode`] component. Additional
//! domain-specific components are inserted by [`crate::hydrator::SceneHydrator`]
//! when the corresponding record is present in the viewport projection.

use bevy_ecs::prelude::*;
use glam::Mat4;
use idtx_usd::{
    CameraRecord, ExtendedGprimKind, GprimAxis, LightRecord, MaterialRecord, PackedArcKind, PrimId,
    PrimState, RenderPurpose, SkeletonPoseRecord, SkeletonTopologyRecord, SkinBindingRecord,
};
#[cfg(feature = "usd-extension")]
use idtx_usd_extension::{
    Approximation, CollisionChannel, CollisionShape, HudAnchor, WidgetBillboard, WidgetLayout,
    WidgetSpace,
};

#[cfg(feature = "usd-extension")]
use crate::collision::CollisionDims;

// ── Core identity ────────────────────────────────────────────────────────────

/// Core prim identity, topology metadata, and authored-state flags.
/// Present on every entity in the scene world.
#[derive(Debug, Clone, Component)]
pub struct PrimNode {
    /// Stable numeric identity that survives topology resyncs.
    pub prim: PrimId,
    /// Full USD prim path, e.g. `"/World/Objects/Box"`.
    /// Used as the key for [`crate::stable_prim_hash`] egui widget IDs.
    pub path: String,
    /// Parent prim identity, `None` for the pseudo-root.
    pub parent: Option<PrimId>,
    /// Leaf name component of the path.
    pub name: String,
    /// USD schema type name, e.g. `"Mesh"`, `"Camera"`, `"DistantLight"`.
    pub type_name: String,
    pub state: PrimState,
    pub is_pseudo_root: bool,
    /// Non-`None` when this prim has at least one spec in the root layer.
    pub local_layer_identifier: Option<String>,
}

// ── Spatial / imageable ──────────────────────────────────────────────────────

/// World-space 4×4 transform matrix (glam column-major, derived from the
/// `OpenUSD` row-major `GfMatrix4d` by transposition).
#[derive(Debug, Clone, Component)]
pub struct WorldTransform {
    pub matrix: Mat4,
}

/// Imageable visibility and render purpose.
#[derive(Debug, Clone, Component)]
pub struct VisibilityComp {
    pub visible: bool,
    pub purpose: RenderPurpose,
    /// Whether this prim casts shadows (`idtx:castsShadows`, default `true`).
    /// `false` excludes the prim from the shadow depth pre-pass and the shadow
    /// AABB — e.g. visible-but-non-casting debug gizmos / pointer rays.
    pub casts_shadow: bool,
}

/// User-managed visibility override for a prim.
///
/// `Inherited` (default) means the prim follows its parent's effective visibility.
/// `Visible` is an explicit override that makes the prim visible even when an ancestor
/// is `Invisible`.  `Invisible` hides the prim and any `Inherited` descendants.
///
/// This is distinct from USD's `"visibility"` attribute — it is managed entirely
/// in the ECS and is never written back to the stage.  The initial state is
/// derived from the authored USD visibility attribute at load time via
/// [`crate::hydrator::SceneHydrator::initialize_visibility_overrides`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Component)]
pub enum VisibilityOverride {
    /// Follow parent chain (or visible if at root).
    #[default]
    Inherited,
    /// Explicitly visible regardless of parent state.
    Visible,
    /// Explicitly invisible.
    Invisible,
}

/// World-space axis-aligned bounding box, plus local-space extents.
///
/// `minimum/maximum` are the world-space AABB (after the prim's world transform).
/// `local_minimum/local_maximum` are the pre-transform local-space extents from
/// `ComputeExtentFromPlugins`; use these with [`WorldTransform`] to build an
/// oriented bounding box (OBB) that respects prim rotation.
#[derive(Debug, Clone, Component)]
pub struct BoundsComp {
    pub minimum: [f32; 3],
    pub maximum: [f32; 3],
    pub local_minimum: [f32; 3],
    pub local_maximum: [f32; 3],
}

// ── Schema-specific geometry ─────────────────────────────────────────────────

/// Polygon mesh hydrated from `UsdGeomMesh`.
/// Points and texcoords are already extracted into typed arrays.
/// Face vertex counts / indices retain the USD polygon representation;
/// call [`crate::hydrator::triangulate_mesh`] to produce a triangle list.
#[derive(Debug, Clone, Component)]
pub struct MeshComp {
    /// Incremented by the hydrator only when vertex/index data actually changes.
    /// Stable across frames for transform-only animation, allowing the render cache
    /// to use the cheap transform-update path instead of full GPU buffer rebuild.
    pub geometry_version: u64,
    pub points: Vec<[f32; 3]>,
    /// Normals expanded to face-corner order (all USD interpolation modes
    /// handled in the C++ shim). len = `face_vertex_indices.len()`.
    /// Empty when the mesh has no normals primvar; the render layer computes them.
    pub normals: Vec<[f32; 3]>,
    /// Tangents (xyz + bitangent sign w) in face-corner order.
    /// len = `face_vertex_indices.len()`. Empty when not authored.
    pub tangents: Vec<[f32; 4]>,
    pub texcoords: Vec<[f32; 2]>,
    pub face_vertex_counts: Vec<i32>,
    pub face_vertex_indices: Vec<i32>,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub material_subsets: Vec<idtx_usd::MaterialSubsetRecord>,
    pub left_handed: bool,
    pub double_sided: bool,
    pub prototype_only: bool,
    /// Subdivision scheme token ("none"/"catmullClark"/"loop"/"bilinear"), empty
    /// when unauthored. The render layer does not subdivide, but uses the crease/
    /// corner specs below to keep authored sharp features faceted when it
    /// synthesizes smooth normals for a mesh with no `normals` primvar.
    pub subdivision_scheme: String,
    /// Sharp-edge crease spec: point-index chains partitioned by `crease_lengths`,
    /// with per-crease or per-edge `crease_sharpnesses`.
    pub crease_indices: Vec<i32>,
    pub crease_lengths: Vec<i32>,
    pub crease_sharpnesses: Vec<f32>,
    /// Sharp-vertex corner spec: point indices + matching sharpness.
    pub corner_indices: Vec<i32>,
    pub corner_sharpnesses: Vec<f32>,
}

/// Collision description hydrated for a prim carrying the `OpenUSD`-standard
/// `UsdPhysicsCollisionAPI` (`physics:collisionEnabled`), optionally with
/// `UsdPhysicsMeshCollisionAPI` (`physics:approximation`) and the reduced
/// `IdtxCollisionAPI` addendum (hull/channel). Hydrated by
/// [`crate::hydrator::SceneHydrator::hydrate_collision`] for any gprim that has
/// `PhysicsCollisionAPI` applied.
///
/// When present and `enabled`, scene raycasts (via
/// [`crate::picking::pick_prim_filtered`]) narrow-phase against the resolved
/// [`shape`](Self::shape) rather than the render geometry, and the
/// `require_collidable` filter uses its presence (+ `enabled`) to decide whether
/// a prim participates in collision picking at all.
///
/// Shape semantics (the [`shape`](Self::shape) is *resolved* at hydration from
/// `physics:approximation` and the gprim type — see
/// [`crate::collision::resolve_collision_shape`]):
/// - [`CollisionShape::Mesh`] uses the prim's own [`MeshComp`] geometry.
/// - [`CollisionShape::Hull`] uses the pre-baked [`hull_points`](Self::hull_points)
///   + [`hull_triangles`](Self::hull_triangles) (never regenerated at runtime).
/// - the analytic shapes (`Sphere`/`Cube`/`Cylinder`/`Cone`/`Capsule`) are sized
///   from the collider gprim's own geometry (`radius`/`size`/`height`/`axis`),
///   falling back to the prim's local bounds ([`BoundsComp`]) via
///   [`crate::collision::resolve_analytic`].
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, PartialEq, Component)]
pub struct CollisionComp {
    /// `physics:collisionEnabled` — whether this prim is collidable (default
    /// `true`).
    pub enabled: bool,
    /// The *resolved* narrow-phase collision shape.
    pub shape: CollisionShape,
    /// `physics:approximation` on `UsdPhysicsMeshCollisionAPI` (default
    /// [`Approximation::None`]); drives [`shape`](Self::shape) resolution and is
    /// retained for a future physics simulation phase.
    pub approximation: Approximation,
    /// Analytic collider dimensions sourced from the collider gprim's own
    /// geometry; unauthored dimensions fall back to the prim's local bounds in
    /// [`crate::collision::resolve_analytic`].
    pub dims: CollisionDims,
    /// `idtx:collision:hull:points` — pre-baked local-space hull vertices (used
    /// when `shape = hull`; may be empty).
    pub hull_points: Vec<[f32; 3]>,
    /// `idtx:collision:hull:triangles` — flat triangle index list (3 indices per
    /// triangle) into `hull_points`.
    pub hull_triangles: Vec<i32>,
    /// `idtx:collision:channel` — the channels this collider responds to.
    pub channels: Vec<CollisionChannel>,
}

/// Marks an entity whose USD prim carries `IdtxCollisionBodyAPI`
/// ([`idtx_usd_extension::COLLISION_BODY_SCHEMA`]) — the asset's interaction /
/// collision *body* grouping prim.
///
/// Carries no geometry: colliders live on the descendant gprims
/// ([`CollisionComp`]). A raycast still hits the leaf gprim collider; a behavior
/// resolves that up to the owning body with [`collision_body_of`], so
/// gaze / highlight / grab act on the whole asset. Hydrated additively
/// alongside (and independently of) [`CollisionComp`]; a `CollisionBodyComp`
/// entity is never itself a raycast candidate.
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct CollisionBodyComp;

/// `entity` itself when it carries [`CollisionBodyComp`], otherwise its nearest
/// ancestor that does, or `None` when the prim is inside no collision body.
///
/// This is the leaf→root counterpart to the root→leaf `leaf_gprim_entities`
/// expansion the highlight path uses: a raycast returns the hit gprim, this
/// resolves it up to the body, and highlighting the body expands back down to
/// every leaf gprim. The walk is self-then-nearest-ancestor-first, so nested
/// bodies resolve to the innermost. Pure ECS over [`PrimNode::parent`]; no
/// USD/FFI.
#[cfg(feature = "usd-extension")]
#[must_use]
pub fn collision_body_of(world: &World, entity: Entity) -> Option<Entity> {
    // The prim itself is the body when it carries the marker — resolve before
    // climbing so a raycast that hits the body prim directly maps to itself.
    if world.get::<CollisionBodyComp>(entity).is_some() {
        return Some(entity);
    }

    // Index `PrimId → (entity, parent, is_body)` in one immutable pass so the
    // ancestor walk is a chain of map lookups rather than nested scans.
    let mut by_prim: std::collections::HashMap<PrimId, (Entity, Option<PrimId>, bool)> =
        std::collections::HashMap::new();
    for e in world.iter_entities() {
        if let Some(node) = e.get::<PrimNode>() {
            by_prim.insert(node.prim, (e.id(), node.parent, e.contains::<CollisionBodyComp>()));
        }
    }

    let mut cursor = world.get::<PrimNode>(entity)?.parent;
    while let Some(pid) = cursor {
        let (ent, parent, is_body) = *by_prim.get(&pid)?;
        if is_body {
            return Some(ent);
        }
        cursor = parent;
    }
    None
}

/// Marks an entity whose USD prim carries `UsdPhysicsRigidBodyAPI`
/// ([`idtx_usd_extension::PHYSICS_RIGID_BODY_SCHEMA`]) — the root of a simulated
/// (or kinematic / static) rigid body. The physics subsystem groups every
/// collider in the subtree under this prim into one body (compound / whole-asset
/// bodies).
///
/// Carries no geometry of its own: colliders live on this prim or its descendant
/// gprims ([`CollisionComp`]). A raycast hits a leaf collider; a grab behavior
/// resolves that up to the owning body with [`rigid_body_of`] so suspend / resume
/// act on the whole simulated asset — the physics analogue of
/// [`collision_body_of`] / [`CollisionBodyComp`]. Hydrated additively alongside
/// [`CollisionComp`] / [`CollisionBodyComp`].
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct RigidBodyComp;

/// `entity` itself when it carries [`RigidBodyComp`], otherwise its nearest
/// ancestor that does, or `None` when the prim is inside no rigid body — the
/// physics counterpart to [`collision_body_of`], resolving a grabbed child
/// collider up to its rigid-body root. Self-then-nearest-ancestor-first (`UsdPhysics`
/// forbids a rigid body nested under another, so the nearest ancestor is the body
/// root). Pure ECS over [`PrimNode::parent`]; no USD/FFI.
#[cfg(feature = "usd-extension")]
#[must_use]
pub fn rigid_body_of(world: &World, entity: Entity) -> Option<Entity> {
    if world.get::<RigidBodyComp>(entity).is_some() {
        return Some(entity);
    }

    let mut by_prim: std::collections::HashMap<PrimId, (Entity, Option<PrimId>, bool)> =
        std::collections::HashMap::new();
    for e in world.iter_entities() {
        if let Some(node) = e.get::<PrimNode>() {
            by_prim.insert(node.prim, (e.id(), node.parent, e.contains::<RigidBodyComp>()));
        }
    }

    let mut cursor = world.get::<PrimNode>(entity)?.parent;
    while let Some(pid) = cursor {
        let (ent, parent, is_body) = *by_prim.get(&pid)?;
        if is_body {
            return Some(ent);
        }
        cursor = parent;
    }
    None
}

/// A screen-space HUD overlay element hydrated from the concrete typed
/// `IdtxHud` (`idtx:hud:*`) schema.
///
/// Present on entities whose USD prim type is `"IdtxHud"`. Consumed each frame
/// by the session to build [`idtx_render::HudElement`] entries and ultimately
/// rendered by [`idtx_render::HudRenderer`] after the beauty + highlight passes.
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, PartialEq, Component)]
pub struct HudComp {
    /// `idtx:hud:image` — asset path to the HUD image (e.g. a crosshair PNG).
    pub image: String,
    /// `idtx:hud:sizePx` — on-screen size in pixels; `[0, 0]` uses native image size.
    pub size_px: [i32; 2],
    /// `idtx:hud:anchor` — screen anchor position (default `center`).
    pub anchor: HudAnchor,
    /// `idtx:hud:opacity` — opacity multiplier in `[0, 1]` (default `1.0`).
    pub opacity: f32,
}

/// The kind of a widget element inside a panel — mirrors the `IdtxWidget*`
/// element prim types.
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetElementKind {
    Container,
    Label,
    Button,
    Image,
    Slider,
    Toggle,
    /// A value readout (`IdtxWidgetField*`): its `text` is the already-formatted
    /// value (prefix + numeric value at the authored precision + suffix) resolved
    /// at hydration from the bound `idtx:widget:value` compute leaf.
    Field,
}

/// One resolved element in a panel's retained tree, flattened in USD namespace
/// order with a nesting `depth` so the rasterizer can lay it out without
/// re-walking the stage. Carries the per-element layout, text, and font
/// configuration the `WidgetRasterizer` needs.
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetElement {
    pub kind: WidgetElementKind,
    /// Panel-relative namespace path of this element (e.g. `"StartButton"` or
    /// `"Footer/QuitButton"`), the segment of the prim path below the panel. This
    /// is the stable identity an interaction handler addresses (matching the
    /// `SessionEditOp::SetWidgetValue { name }` convention) and what the
    /// host-side hit-test returns for a clicked element.
    pub path: String,
    /// Nesting depth under the panel (panel children = 0).
    pub depth: u32,
    /// Layout of this element's immediate parent container (`Column` when the
    /// parent is the panel itself). Lets the rasterizer place siblings of a `row`
    /// container as columns (label | field) instead of a vertical stack.
    pub parent_layout: WidgetLayout,
    /// This container's own layout (never inferred from a flattened child).
    pub layout: WidgetLayout,
    pub radial: crate::widget_layout::RadialSettings,
    /// Effective element/container opacity, excluding the panel multiplier.
    pub opacity: f32,
    /// Label text / button caption / formatted field value (empty for
    /// non-text elements).
    pub text: String,
    /// Foreground / content colour (linear RGB).
    pub color: [f32; 3],
    /// Scalar state for slider/toggle (`0.0`/`1.0` for a toggle) or the raw
    /// numeric value of a field; else `0.0`.
    pub value: f32,
    /// Panel-local NORMALIZED position from `idtx:widget:position` ((0,0) =
    /// top-left … (1,1) = bottom-right). `None` when the attribute is absent /
    /// left at its `(0,0)` default — the rasterizer then falls back to
    /// namespace-order layout for this element.
    pub position: Option<[f32; 2]>,
    /// Paint/stacking order from `idtx:widget:order` (higher draws on top).
    pub order: i32,
    /// `idtx:widget:fontFamily` — named font family to render text in. `None`
    /// (attribute absent/empty) uses the built-in default family; an unknown
    /// name also falls back to the default in the font registry.
    pub font_family: Option<String>,
    /// `idtx:widget:fontSize` — text size in **pixels** of the panel's
    /// rasterization space (`idtx:widget:resolution`). `None` or `≤ 0` keeps the
    /// auto-proportional sizing the rasterizer derives from the element's slot
    /// height. Shrunk to fit the slot width so text never clips.
    pub font_size: Option<f32>,
    /// `idtx:widget:enabled` for a `Button` (default `true`). A disabled element
    /// is drawn dimmed and never dispatches an interaction event. Unused for
    /// non-button kinds (kept `true`).
    pub enabled: bool,
    /// Resolved asset path for `IdtxWidgetImage` (`idtx:widget:image`). `None`
    /// for every other element kind. The path is already USD-resolver-resolved
    /// at hydration time (absolute or anchored) so the rasterizer can open it
    /// directly with `image::open`. Falls back to the raw authored path when
    /// the USD resolver yields nothing; `None` when the attribute is absent.
    pub image_path: Option<String>,
}

#[cfg(feature = "usd-extension")]
impl Default for WidgetElement {
    fn default() -> Self {
        Self { kind: WidgetElementKind::Label, path: String::new(), depth: 0,
            parent_layout: WidgetLayout::Column, layout: WidgetLayout::Column,
            radial: Default::default(), opacity: 1.0, text: String::new(),
            color: [1.0; 3], value: 0.0, position: None, order: 0,
            font_family: None, font_size: None, enabled: true, image_path: None }
    }
}

/// A 2D widget surface placed in 3D space, hydrated from the `IdtxWidgetPanel`
/// (`idtx:widget:*`) concrete-typed `IsA` schema (which derives from
/// `UsdGeomGprim`, so the entity also carries a [`WorldTransform`]).
///
/// Present on entities whose USD prim type is `"IdtxWidgetPanel"`. The panel
/// aggregates its descendant element prims into [`Self::elements`] (resolved at
/// hydration, not per frame) so the renderer has a self-contained rasterization
/// input. [`Self::content_hash`] gates re-rasterization: an unchanged panel
/// keeps its hash and is never re-rasterized (see the render widget pass).
#[cfg(feature = "usd-extension")]
#[derive(Debug, Clone, PartialEq, Component)]
pub struct WidgetPanelComp {
    /// `idtx:widget:size` — physical panel size `(w, h)` in world units.
    pub size: [f32; 2],
    /// `idtx:widget:resolution` — offscreen rasterization size in pixels.
    pub resolution: [u32; 2],
    /// `idtx:widget:pixelsPerUnit` — rasterization density (px per world unit).
    pub pixels_per_unit: f32,
    /// `idtx:widget:billboard` — camera-facing mode.
    pub billboard: WidgetBillboard,
    /// `idtx:widget:doubleSided` — render both faces.
    pub double_sided: bool,
    /// `idtx:widget:opacity` — opacity multiplier in `[0, 1]`.
    pub opacity: f32,
    /// `idtx:widget:space` — `world` (in-scene quad) vs `hud` (screen-space).
    pub space: WidgetSpace,
    /// `idtx:widget:contentSource` — opaque imported-UI escape hatch; when set,
    /// `elements` is ignored by the rasterizer.
    pub content_source: Option<String>,
    pub surface: crate::widget_layout::WidgetSurface,
    /// Flattened element tree (namespace order), resolved at hydration.
    pub elements: Vec<WidgetElement>,
    /// Hash of every rasterization input; the key for dirty-gated rasterization.
    pub content_hash: u64,
}

/// Procedural unit cube from `UsdGeomCube`.
#[derive(Debug, Clone, Component)]
pub struct CubeComp {
    pub size: f32,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

/// Procedural sphere from `UsdGeomSphere`.
#[derive(Debug, Clone, Component)]
pub struct SphereComp {
    pub radius: f32,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

/// Procedural cone, cylinder, or capsule from the extended gprim schemas.
#[derive(Debug, Clone, Component)]
pub struct ExtendedGprimComp {
    pub kind: ExtendedGprimKind,
    pub radius: f32,
    pub height: f32,
    pub axis: GprimAxis,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

/// Procedural finite, double-sided rectangle from `UsdGeomPlane`.
///
/// `axis` is the plane normal; `width`/`length` lie on the two axes perpendicular
/// to it (see the axis table in the render tessellator). Centred at the origin.
#[derive(Debug, Clone, Component)]
pub struct PlaneComp {
    pub width: f32,
    pub length: f32,
    pub axis: GprimAxis,
    pub display_colors: Vec<[f32; 3]>,
    pub material_path: Option<String>,
    pub double_sided: bool,
    pub prototype_only: bool,
}

// ── Material / light / camera ────────────────────────────────────────────────

/// Resolved PBR material from `UsdShade`, with full `SurfaceIr`.
#[derive(Debug, Clone, Component)]
pub struct MaterialComp {
    pub record: MaterialRecord,
}

/// Light parameters from a `UsdLux` schema.
#[derive(Debug, Clone, Component)]
pub struct LightComp {
    pub record: LightRecord,
}

/// Camera parameters from `UsdGeomCamera`, including physical exposure model.
#[derive(Debug, Clone, Component)]
pub struct CameraComp {
    pub record: CameraRecord,
}

// ── Skeleton / skinning ──────────────────────────────────────────────────────

/// Joint hierarchy and bind/rest poses for a `UsdSkel` skeleton prim.
#[derive(Debug, Clone, Component)]
pub struct SkeletonTopologyComp {
    pub record: SkeletonTopologyRecord,
}

/// Sampled joint poses for the last projected time code.
#[derive(Debug, Clone, Component)]
pub struct SkeletonPoseComp {
    pub record: SkeletonPoseRecord,
}

/// Per-vertex joint influences for a skinned prim.
#[derive(Debug, Clone, Component)]
pub struct SkinBindingComp {
    pub record: SkinBindingRecord,
}

// ── PointInstancer ───────────────────────────────────────────────────────────

/// Instancing data hydrated from `UsdGeomPointInstancer`.
///
/// Each instance is a copy of one of the `prototypes`, placed at the
/// corresponding `transforms[i]` (instancer-local, proto xform included via
/// `ComputeInstanceTransformsAtTime(IncludeProtoXform)`).
#[derive(Debug, Clone, Component)]
pub struct PointInstancerComp {
    /// Prototype prim IDs in the order referenced by `proto_indices`.
    pub prototypes: Vec<PrimId>,
    /// Per-instance prototype selector: `prototypes[proto_indices[i]]`.
    pub proto_indices: Vec<u32>,
    /// Instancer-local transforms, USD row-major, including the prototype root.
    pub transforms: Vec<[f32; 16]>,
    pub ids: Vec<i64>,
    /// Empty means all visible; masking preserves array slots and native ids.
    pub mask: Vec<u8>,
    /// Authored native displayOpacity, parallel to ids. Empty means 1.
    pub opacities: Vec<f32>,
    /// Once updated at runtime, singletons must not churn static merge groups.
    pub runtime_dynamic: bool,
}

/// Marks a native USD scenegraph instance (`instanceable = true`).
///
/// The instance's own world placement lives in its [`WorldTransform`]; the shared
/// prototype geometry is hydrated once (as `prototype_only`) under `prototype`'s
/// subtree. The render extraction pass groups instances by `prototype` and expands
/// them into hardware-instanced batches — mirroring [`PointInstancerComp`].
#[derive(Debug, Clone, Component)]
pub struct NativeInstanceComp {
    /// Prim ID of the shared instancing prototype root (`/__Prototype_N`).
    pub prototype: PrimId,
    /// Computed purpose per instance proxy, keyed by the shared prototype prim.
    pub purposes: Vec<(PrimId, RenderPurpose)>,
}

// ── Packed arcs / payload ────────────────────────────────────────────────────

/// Marks the root prim of a packed arc (reference or payload).
///
/// `nested_*` fields are populated by the session layer when the arc target
/// is separately opened as a stage. They are `None` until then.
#[derive(Debug, Clone, Component)]
pub struct PackedStageMetadata {
    pub arc_kind: PackedArcKind,
    /// Authored asset path of the referenced or payload asset.
    pub asset_path: String,
    /// `metersPerUnit` from the nested stage, if loaded.
    pub nested_meters_per_unit: Option<f64>,
    /// Up-axis token from the nested stage, if loaded.
    pub nested_up_axis: Option<String>,
    /// Total prim count of the nested stage, if loaded.
    pub nested_prim_count: Option<usize>,
}

/// Payload load state for prim roots of a payload arc.
/// Only present on prims where `packed_arc.kind == Payload`.
#[derive(Debug, Clone, Component)]
pub struct PayloadState {
    pub loaded: bool,
}

/// Marks a prim that has root-layer opinions AND sits inside a packed arc
/// boundary. Indicates a locally-authored override inside a referenced or
/// payload asset.
#[derive(Debug, Clone, Component)]
pub struct LocallyAuthoredChild;

/// Present on prims that have an internal (same-layer) reference arc pointing
/// to a prototype prim. This is the USD "pseudo-instance" pattern: multiple
/// prims reference the same `over` prototype and differ only in their transforms.
///
/// `target_path` is the `SdfPath` of the referenced prototype prim (e.g. `"/BoltPrototype"`).
/// Used by the render extraction pass to form topology-aware instanced batches.
#[derive(Debug, Clone, Component)]
pub struct InternalRefArc {
    pub target_path: String,
}

/// Zero-size marker inserted on every non-prototype imageable prim that can
/// be selected in the 3-D viewport. Added by the hydrator for Mesh, Cube,
/// Sphere, and `ExtendedGprim` prims when `prototype_only` is false.
#[derive(Debug, Clone, Component)]
pub struct ViewportPickable;

/// Zero-size marker present on every prim that is-a `UsdGeomXformable`.
/// Populated at topology hydration time directly from `prim.IsA<UsdGeomXformable>()`
/// in the C++ shim — works for built-in and custom schema types alike.
/// Query this component to gate gizmo rendering without per-frame FFI.
#[derive(Debug, Clone, Component)]
pub struct Xformable;

// ── Animation ────────────────────────────────────────────────────────────────

/// What a single animation track drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackTarget {
    /// A generic time-sampled USD attribute (name is the attribute name).
    Attribute { attr_name: String },
    /// A single skeletal joint local transform.
    SkeletalJoint { joint: String },
    /// A blend-shape channel weight.
    BlendShape { channel: String },
}

/// One animated attribute / joint / blend-shape track on a prim.
#[derive(Debug, Clone)]
pub struct AnimTrack {
    /// Display name (attribute name or `"joint:<name>"`).
    pub name: String,
    pub target: TrackTarget,
    /// USD value-type token (e.g. `"float3"`, `"matrix4d"`, `"jointLocalTRS"`).
    pub value_type: String,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub sample_count: usize,
    /// `"held"`, `"linear"`, or `None` (default / linear).
    pub interpolation: Option<String>,
}

/// Set of animated tracks authored on a prim.
/// Attached at hydration time to every entity that has at least one
/// time-sampled attribute.
#[derive(Debug, Clone, Component)]
pub struct AnimatedAttribute {
    pub tracks: Vec<AnimTrack>,
}

// ── Re-exports of upstream record types needed by render / UI layers ─────────

/// Re-export so downstream crates do not need a direct `idtx-usd` dependency
/// for the most common cross-cutting types.
pub use idtx_usd::{
    ExtendedGprimKind as SceneGprimKind, GprimAxis as SceneGprimAxis,
    MaterialSubsetRecord as SceneMaterialSubset, MeshRecord as SceneMeshRecord,
};

#[cfg(all(test, feature = "usd-extension"))]
mod tests {
    use super::*;
    use idtx_usd::StageGeneration;

    fn node(slot: u32, path: &str, parent: Option<u32>) -> PrimNode {
        PrimNode {
            prim: PrimId::from_raw_parts(slot, StageGeneration::from_raw(0)),
            path: path.to_owned(),
            parent: parent.map(|p| PrimId::from_raw_parts(p, StageGeneration::from_raw(0))),
            name: path.rsplit('/').next().unwrap_or("").to_owned(),
            type_name: "Xform".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        }
    }

    #[test]
    fn collision_body_of_resolves_nearest_ancestor() {
        // /World → /World/Asset [body] → /World/Asset/Geom → …/Mesh [collider]
        let mut world = World::new();
        world.spawn(node(1, "/World", None));
        let asset = world
            .spawn((node(2, "/World/Asset", Some(1)), CollisionBodyComp))
            .id();
        world.spawn(node(3, "/World/Asset/Geom", Some(2)));
        let mesh = world.spawn(node(4, "/World/Asset/Geom/Mesh", Some(3))).id();

        assert_eq!(collision_body_of(&world, mesh), Some(asset));
        // Self-first contract: a body prim resolves to itself (a raycast that hits
        // the body prim directly maps to the body, not to an outer ancestor).
        assert_eq!(collision_body_of(&world, asset), Some(asset));
    }

    #[test]
    fn collision_body_of_none_without_body_ancestor() {
        let mut world = World::new();
        world.spawn(node(1, "/World", None));
        let mesh = world.spawn(node(2, "/World/Mesh", Some(1))).id();
        assert_eq!(collision_body_of(&world, mesh), None);
    }

    #[test]
    fn collision_body_of_resolves_innermost_nested_body() {
        let mut world = World::new();
        world.spawn((node(1, "/World", None), CollisionBodyComp));
        let inner = world
            .spawn((node(2, "/World/Inner", Some(1)), CollisionBodyComp))
            .id();
        let mesh = world.spawn(node(3, "/World/Inner/Mesh", Some(2))).id();
        assert_eq!(collision_body_of(&world, mesh), Some(inner));
    }

    #[test]
    fn rigid_body_of_resolves_compound_child_to_body_root() {
        // /World → /World/Cookie [RigidBody] → /World/Cookie/Geom → …/group_0
        // [collider]. A grab on the child collider resolves to the body root.
        let mut world = World::new();
        world.spawn(node(1, "/World", None));
        let body = world
            .spawn((node(2, "/World/Cookie", Some(1)), RigidBodyComp))
            .id();
        world.spawn(node(3, "/World/Cookie/Geom", Some(2)));
        let collider = world.spawn(node(4, "/World/Cookie/Geom/group_0", Some(3))).id();

        assert_eq!(rigid_body_of(&world, collider), Some(body));
        // Self-first: the body prim resolves to itself.
        assert_eq!(rigid_body_of(&world, body), Some(body));
    }

    #[test]
    fn rigid_body_of_none_without_body_ancestor() {
        let mut world = World::new();
        world.spawn(node(1, "/World", None));
        let mesh = world.spawn(node(2, "/World/Mesh", Some(1))).id();
        assert_eq!(rigid_body_of(&world, mesh), None);
    }
}
