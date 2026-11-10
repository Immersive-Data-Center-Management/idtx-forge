//! ECS → render-world extraction.
//!
//! [`RenderWorldSnapshot::extract`] queries the ECS [`bevy_ecs::world::World`]
//! owned by a `SceneHydrator` and produces a GPU-agnostic snapshot that Phase 3
//! will convert to wgpu buffers and draw calls.
//!
//! The snapshot is fully owned — callers can keep it alive across frames for
//! diffing or hand it to a background upload thread.
//!
//! # Submodules
//!
//! | Module | Role |
//! |---|---|
//! | [`light`] | [`RenderLightDesc`] / [`RenderLightKind`] + radiometric & shadow-fit helpers |
//! | [`camera`] | [`RenderCameraDesc`] — view / projection / EV100 from a USD camera |
//! | [`mesh_instance`] | [`RenderMeshInstance`] + [`RenderInstancedBatch`] |
//! | [`material_override`] | [`MaterialOverrideMap`] — behavior-driven per-prim overrides |
//! | [`shadow`] | [`ShadowTier`] / [`RenderShadowSettings`] / [`ShadowAtlasAllocator`] |
//! | [`snapshot`] | [`RenderWorldSnapshot`] + [`ExtractionStats`] |
//! | [`instancing`] | topology-aware pseudo-instance promotion + geometry fingerprint |
//! | [`extract`] | the bulk `extract` query and the transform / joint fast-path patches |

mod camera;
mod extract;
mod instancing;
mod light;
mod material_override;
mod mesh_instance;
mod shadow;
mod snapshot;

#[cfg(test)]
mod tests;

pub use camera::RenderCameraDesc;
pub use light::{RenderLightDesc, RenderLightKind, blackbody_to_linear_rgb};
pub use material_override::MaterialOverrideMap;
pub use mesh_instance::{RenderInstancedBatch, RenderMeshInstance};
pub use shadow::{
    RenderShadowSettings, SHADOW_BUDGET, ShadowAtlasAllocator, ShadowSlot, ShadowTier,
};
pub use snapshot::{ExtractionStats, HudElement, RenderWorldSnapshot, WidgetPanel};
