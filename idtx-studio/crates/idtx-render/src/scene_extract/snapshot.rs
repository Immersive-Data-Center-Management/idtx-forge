//! The owned, GPU-agnostic [`RenderWorldSnapshot`] and its extraction statistics.
//!
//! The extraction logic that populates these types lives in
//! [`super::extract`]; the topology-instance promotion in
//! [`super::instancing`].

use std::collections::HashMap;

use glam::{Mat4, Vec3};
#[cfg(feature = "usd-extension")]
use idtx_scene::{
    HudAnchor,
    WidgetBillboard, WidgetElement, WidgetSpace,
};

use crate::material::RenderMaterialDesc;

use super::camera::RenderCameraDesc;
use super::light::RenderLightDesc;
use super::material_override::MaterialOverrideMap;
use super::mesh_instance::{RenderInstancedBatch, RenderMeshInstance};
use super::shadow::{RenderShadowSettings, ShadowSlot};

/// A screen-space HUD overlay element to composite this frame, extracted from
/// an [`idtx_scene::HudComp`] (the `IdtxHud` schema) by the session and consumed
/// by [`crate::HudRenderer`] after the beauty + highlight passes.
///
/// The HUD is **camera-independent** in screen space: a center crosshair needs
/// only the target dimensions, not the camera `view_proj`. The anchor + size
/// resolve the on-screen rectangle; `[0, 0]` size means use the loaded image's
/// native pixel dimensions.
#[derive(Debug, Clone, PartialEq)]
pub struct HudElement {
    /// Resolved asset path of the HUD image (e.g. a crosshair PNG).
    pub image_path: String,
    /// On-screen size in pixels; `[0, 0]` means use the image's native size.
    pub size_px: [i32; 2],
    /// Screen anchor position (default `center`).
    pub anchor: HudAnchor,
    /// Opacity multiplier in `[0, 1]`.
    pub opacity: f32,
}

/// A 2D widget panel placed in 3D space, extracted from an
/// [`idtx_scene::WidgetPanelComp`] (the `IdtxWidgetPanel` schema) by
/// [`RenderWorldSnapshot::extract`]. Unlike [`HudElement`] a panel is
/// **world-space geometry**: it carries a [`world_matrix`](Self::world_matrix)
/// and is drawn as a depth-tested textured quad inside the beauty pass by the
/// widget renderer. The resolved element tree + `content_hash` let the renderer
/// rasterize the panel texture once and reuse it until the content changes.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetPanel {
    /// USD prim path — the stable key for the renderer's per-panel texture cache.
    pub prim_path: String,
    /// Render-space world transform (`stage_basis * WorldTransform.matrix`).
    pub world_matrix: Mat4,
    /// Physical panel size `(w, h)` in world units.
    pub size: [f32; 2],
    /// Offscreen rasterization size in pixels.
    pub resolution: [u32; 2],
    /// Camera-facing mode.
    pub billboard: WidgetBillboard,
    /// Render both faces.
    pub double_sided: bool,
    /// Opacity multiplier in `[0, 1]`.
    pub opacity: f32,
    /// `world` (in-scene quad) vs `hud` (screen-space).
    pub space: WidgetSpace,
    /// Flattened element tree (namespace order) for the rasterizer.
    pub elements: Vec<WidgetElement>,
    /// Hash of every rasterization input — the dirty-gating key.
    pub content_hash: u64,
}

///
/// Produced by [`RenderWorldSnapshot::extract`] from the ECS world.
/// The renderer consumes this to allocate wgpu buffers and record draw calls.
#[derive(Debug, Clone, Default)]
pub struct RenderWorldSnapshot {
    pub meshes: Vec<RenderMeshInstance>,
    /// Hardware-instanced batches from `UsdGeomPointInstancer` prims and pseudo-instance dedup.
    pub instanced_batches: Vec<RenderInstancedBatch>,
    pub lights: Vec<RenderLightDesc>,
    pub camera: Option<RenderCameraDesc>,
    /// Material descriptors keyed by USD material path.
    pub materials: HashMap<String, RenderMaterialDesc>,
    /// UDIM tiles touched per material, keyed by USD material path.
    ///
    /// Populated only for materials that (a) reference at least one `<UDIM>`
    /// texture template and (b) are drawn by a static (non-instanced,
    /// non-skinned) mesh in v1. The set is the union of tiles touched by every
    /// such mesh/subset sharing the material, so one material maps to one GPU
    /// texture array. Empty when no UDIM materials are present — the non-UDIM
    /// path never allocates or consults this map.
    pub udim_tiles: HashMap<String, std::collections::BTreeSet<u32>>,
    /// Behavior-driven material overrides keyed by [`idtx_scene::PrimId`].
    /// Empty except when a behaviour has posted a `set_material_override`; the
    /// renderer swaps the named override material in for the matching mesh.
    pub material_overrides: MaterialOverrideMap,
    /// Behavior-requested surface **tints** keyed by USD prim path. Each
    /// value is a linear-RGBA colour whose alpha is the tint strength
    /// (0 = untouched, 1 = fully the colour). Transient — injected per frame by
    /// the session (like [`camera`](Self::camera)) and consumed when the draw
    /// loop builds a prim's material bind group. Empty on the common path.
    pub highlight_tints: HashMap<String, [f32; 4]>,
    /// Behavior-requested silhouette **outlines**: `(USD prim path, linear-RGBA
    /// colour, thickness_px)`. `thickness_px == 0.0` means the renderer
    /// default. Transient — injected per frame by the session and consumed by
    /// the JFA outline pass, which groups entries by `(colour, thickness)` and
    /// runs one mask→seed→jump-flood→composite pass per distinct group. Empty on
    /// the common path (no outline pass encoded).
    pub highlight_outlines: Vec<(String, [f32; 4], f32)>,
    /// Screen-space HUD overlay elements to composite this frame (camera-HUD
    /// overlay). Transient — injected per frame by the session from ECS entities
    /// carrying [`idtx_scene::HudComp`]. Empty on the common path (no HUD pass
    /// encoded). Consumed by [`crate::HudRenderer`] after the highlight passes.
    pub hud_elements: Vec<HudElement>,
    /// World-space widget panels to draw this frame (2D-widgets-in-USD). Unlike
    /// [`hud_elements`](Self::hud_elements) these ARE populated by
    /// [`extract`](Self::extract) from [`idtx_scene::WidgetPanelComp`] entities
    /// (they are world-space geometry with a transform). Empty on the common
    /// path (no widgets). Consumed by the widget renderer inside the beauty pass.
    pub widget_panels: Vec<WidgetPanel>,
    /// Render statistics.
    pub stats: ExtractionStats,
    /// Monotonically increasing counter set by [`RenderWorldSnapshot::extract`].
    /// Gates the renderer's **structural** caches (static draw-merge bake): a
    /// change forces a full rebuild. Deliberately *not* bumped by in-place warm
    /// patches (`patch_world_matrices` / `patch_joint_matrices`) so those caches
    /// survive transform-only ticks.
    pub serial: u64,
    /// Serial gating only the **instanced-batch** transform re-upload. Set equal
    /// to [`serial`](Self::serial) on every fresh extract, but bumped
    /// *independently* when a reused snapshot's instance transforms change without
    /// any structural change — e.g. the per-frame XR hand-skeleton injection. This
    /// lets hand transforms re-upload every frame without tripping the whole-scene
    /// draw-merge rebuild that keying off [`serial`](Self::serial) would cause.
    pub instance_serial: u64,
    /// Resolved shadow configuration for this frame (tier + master on/off).
    /// Defaults to [`RenderShadowSettings::OFF`]; set by
    /// [`RenderWorldSnapshot::extract_with_shadow`].
    pub shadow: RenderShadowSettings,
    /// Per-frame shadow-atlas allocation: one entry per active caster layer, in
    /// slot order (`[0, SHADOW_BUDGET)`). Empty when shadows are off or no
    /// Distant caster exists. Produced by [`super::ShadowAtlasAllocator::allocate`].
    pub shadow_slots: Vec<ShadowSlot>,
    /// Render-space scene AABB the Distant casters' shadow frusta are fit to,
    /// captured at the last full [`extract_with_shadow`](Self::extract_with_shadow).
    /// Reused by [`patch_world_matrices`](Self::patch_world_matrices) to refit a
    /// moved light's shadow without an `O(vertices)` bounds recompute. `None` when
    /// shadows are off or the scene has no geometry.
    pub shadow_aabb: Option<(Vec3, Vec3)>,
    /// [`scene_aabb_signature`](crate::camera_fit::scene_aabb_signature) of the
    /// inputs that produced [`shadow_aabb`](Self::shadow_aabb). Lets
    /// [`patch_world_matrices`](Self::patch_world_matrices) skip the (now
    /// O(meshes)) AABB recompute when the signature is unchanged — the
    /// signature-gate half of the per-frame shadow-refit fix. `0` until the first
    /// shadow-enabled extract.
    pub shadow_aabb_sig: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractionStats {
    pub mesh_count: usize,
    pub light_count: usize,
    pub skipped_invisible: usize,
    pub skipped_prototype_only: usize,
    /// Number of `RenderInstancedBatch` entries produced (`PointInstancer` + pseudo-instance).
    pub instanced_batch_count: usize,
    /// Total instance count across all instanced batches.
    pub instanced_draw_count: usize,
    /// Singleton meshes promoted to instanced batches by pseudo-instance dedup.
    pub pseudo_instance_promoted: usize,
    /// Meshes promoted to instanced batches by topology-aware (internal-ref-arc) detection.
    pub topology_instance_promoted: usize,
    /// Native scenegraph instances (`instanceable = true`) expanded into instanced batches.
    pub native_instance_count: usize,
    /// Wall time for the materials query phase (nanoseconds; `metrics` feature only).
    #[cfg(feature = "metrics")]
    pub extract_materials_ns: u64,
    /// Wall time for all primitive mesh extraction phases combined (nanoseconds; `metrics` feature only).
    #[cfg(feature = "metrics")]
    pub extract_meshes_ns: u64,
    /// Wall time for the PointInstancer / instanced-batch phase (nanoseconds; `metrics` feature only).
    #[cfg(feature = "metrics")]
    pub extract_instancers_ns: u64,
    /// Wall time for the topology-aware instance promotion phase (nanoseconds; `metrics` feature only).
    #[cfg(feature = "metrics")]
    pub extract_topo_promote_ns: u64,
    /// Wall time spent discovering UDIM tiles (per-mesh `touched_tiles` union;
    /// nanoseconds; `metrics` feature only). Zero when no UDIM materials exist.
    #[cfg(feature = "metrics")]
    pub extract_udim_discovery_ns: u64,
    /// Wall time of the shadow refit inside [`RenderWorldSnapshot::patch_world_matrices`]
    /// on the transform-fast-path (nanoseconds; `metrics` feature only). Covers the
    /// caster view-projection refit and atlas re-allocation, plus the scene-AABB
    /// recompute when a caster (not just a light) moved. Zero on frames with no
    /// transform patch or with shadows off.
    #[cfg(feature = "metrics")]
    pub patch_shadow_refit_ns: u64,
}
