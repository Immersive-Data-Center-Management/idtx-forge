//! `idtx-render` — GPU-agnostic scene extraction and offscreen wgpu renderer.
//!
//! # Crate structure
//!
//! | Module | Role |
//! |---|---|
//! | [`material`] | [`RenderMaterialDesc`] — flat PBR descriptor from a USD material record |
//! | [`mesh`] | [`GpuMeshDesc`] — triangulated, GPU-ready vertex/index data |
//! | [`scene_extract`] | [`RenderWorldSnapshot`] — full per-frame extraction from the ECS world |
//! | [`camera_fit`] | Synthetic preview camera when the stage has no camera prim |
//! | [`overlay`] | Selection corner-tick / axis / bone overlay line builders + pass |
//! | [`light_glyph`] | DCC-style selection glyphs for USD light prims (sphere/spot/rect/…) |
//! | [`math`] | Shared math utilities (projection tests) |
//! | [`vertex`] | [`Vertex`] interleaved GPU vertex layout |
//! | [`surface_gen`] | [`SurfaceGenerator`] trait + [`UsdPbrGenerator`] + PBR WGSL shader |
//! | [`texture_cache`] | GPU texture cache (loads PNG/JPEG, provides null textures) |
//! | [`metrics`] | Frame timing behind the `metrics` Cargo feature flag |
//! | [`pipeline`] | `PbrPipeline` + all uniform structs |
//! | [`renderer`] | [`Renderer`] offscreen wgpu renderer + [`FrameTarget`] |
//!
//! Phase 4 adds Cook-Torrance PBR, a 6-channel texture pipeline, and an
//! 8-light scene array.

pub mod camera_fit;
pub mod batch_limits;
#[cfg(feature = "editor-overlay")]
pub mod collision_glyph;
pub(crate) mod convert;
pub mod env_gpu;
pub mod env_ibl;
pub mod font;
pub mod highlight;
pub mod hud;
pub mod light_glyph;
pub mod material;
pub mod math;
pub mod mesh;
pub mod metrics;
pub mod overlay;
pub mod pipeline;
pub mod postprocess;
pub mod renderer;
pub mod scene_extract;
pub mod surface_gen;
pub mod texture_cache;
pub mod udim;
pub mod vertex;
pub mod wgsl_patch;
pub mod widget_pass;

pub use camera_fit::{compute_scene_aabb, fit_camera_to_snapshot};
pub use batch_limits::{DrawBatchLimits, GeometryError};
pub use light_glyph::{LightGlyphParams, push_light_glyph};
#[cfg(feature = "editor-overlay")]
pub use collision_glyph::{
    push_collision_analytic, push_collision_polysoup, push_collision_trisoup, COLLISION_COLOR,
};
pub use material::RenderMaterialDesc;
pub use math::build_view_proj;
pub use mesh::GpuMeshDesc;
#[cfg(feature = "editor-overlay")]
pub use overlay::push_bone_octahedron;
pub use overlay::{
    DepthOverlayGpuData, DepthOverlayPass, OverlayLine, OverlayPass, push_aabb_corner_ticks,
    push_arc, push_line, push_polyline, push_ribbon, sample_arc,
};
pub use postprocess::{
    JfaStepUniform, OutlineCompositeUniform, OutlineParams, PingPong, PostProcessChain,
    PostProcessKind, PostProcessStage, SEED_FORMAT, SilhouetteOutlinePass, final_target,
    group_outlines_by_color, jfa_step_sequence, outline_alpha, ping_pong_schedule,
};
pub use highlight::HighlightRenderer;
pub use hud::HudRenderer;
pub use widget_pass::{
    SolidWidgetRasterizer, WidgetElementRect, WidgetRasterizer, WidgetRenderer,
    widget_element_rects, widget_hit_test,
};
pub use font::FontContext;
pub use renderer::{COLOR_FORMAT, DEPTH_FORMAT, FrameTarget, RenderError, Renderer};
pub use scene_extract::{
    HudElement, MaterialOverrideMap, RenderCameraDesc, RenderLightDesc, RenderShadowSettings,
    RenderWorldSnapshot, SHADOW_BUDGET, ShadowAtlasAllocator, ShadowSlot, ShadowTier, WidgetPanel,
};
pub use texture_cache::{AssetReadFn, DownscaleAlgo, TextureDecodeConfig, TextureDecodeTimings};

#[cfg(feature = "metrics")]
mod gpu_readback;
pub use vertex::{Vertex, interleave};
