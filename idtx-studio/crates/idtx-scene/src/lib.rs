//! Revisioned ECS projection facade — `bevy_ecs` hydration layer for IDTX Studio.
//!
//! Provides:
//! - [`Component`] types for all USD prim data domains (transforms, meshes, materials, etc.)
//! - [`PopulatorRegistry`] — per-schema-type handlers that map USD records to ECS components
//! - [`SceneHydrator`] — incremental topology + viewport application to an ECS [`World`]
//!
//! # Architecture
//!
//! The ECS world is a *projection* of the `UsdStage` — not an independent copy.
//! [`SceneHydrator::apply_topology_delta`] spawns/despawns entities to mirror topology
//! changes. [`SceneHydrator::apply_viewport_projection`] upserts domain-specific
//! components from the single `project_viewport()` bulk call.
//!
//! The [`PopulatorRegistry`] extension point lets downstream crates register custom
//! [`Populator`] implementations for non-standard USD schema types without modifying
//! this crate.

#![forbid(unsafe_code)]

pub mod animation;
#[cfg(feature = "behavior")]
pub mod behavior;
#[cfg(feature = "usd-extension")]
pub mod collision;
#[cfg(feature = "collision-bvh")]
pub mod collision_bvh;
pub mod components;
pub mod coordinate;
pub mod curve;
pub use curve::CurveComp;
#[cfg(feature = "usd-extension")]
pub mod hud;
pub mod hydrator;
pub mod metrics;
pub mod picking;
pub mod player;
pub mod populator;
pub mod schema_ext;
pub mod stable_id;
#[cfg(feature = "usd-extension")]
pub mod widget;
#[cfg(feature = "usd-extension")]
pub mod widget_layout;

pub use animation::{
    AnimClipPlayer, ClipPlayhead, ClipSampleRequest, PlaybackRequest, StageAnimClip, StagePlayback,
    apply_playback_request, attach_stage_anim_clip, build_stage_anim_clip,
    collect_animated_prim_ids, collect_clip_sample_requests, mark_clip_players_sampled,
    tick_clip_players,
};
#[cfg(feature = "behavior")]
pub use behavior::{
    BehaviorKind, BehaviorParamValue, BehaviorPopulator, BehaviorRef, BehaviorScope,
    HMD_CAMERA_SYNC_UUID, SYNTHETIC_PREVIEW_CAMERA_PATH, StageRoot,
};
pub use bevy_ecs;
pub use components::{
    AnimTrack, AnimatedAttribute, BoundsComp, CameraComp,
    CubeComp, ExtendedGprimComp, InternalRefArc, LightComp, LocallyAuthoredChild,
    MaterialComp, MeshComp, NativeInstanceComp, PackedStageMetadata, PayloadState,
    PlaneComp, PointInstancerComp, PrimNode, SkeletonPoseComp, SkeletonTopologyComp,
    SkinBindingComp, SphereComp, TrackTarget, ViewportPickable, VisibilityComp,
    VisibilityOverride, WorldTransform, Xformable,
};
#[cfg(feature = "usd-extension")]
pub use components::{
    CollisionBodyComp, CollisionComp, HudComp, RigidBodyComp, WidgetElement, WidgetElementKind,
    WidgetPanelComp, collision_body_of, rigid_body_of,
};
#[cfg(feature = "usd-extension")]
pub use collision::{
    axis_local_y_rotation, collision_shape_geom, resolve_analytic, resolve_collision_shape,
    Approximation, AxialDims, CollisionChannel, CollisionDims, CollisionShape, ResolvedKind,
    ResolvedShape, GprimAxis, ShapeGeom,
};
#[cfg(feature = "collision-bvh")]
pub use collision_bvh::CollisionBvh;
#[cfg(all(feature = "collision-bvh", feature = "collision-metrics"))]
pub use collision_bvh::BvhMetricsSnapshot;
#[cfg(feature = "usd-extension")]
pub use hud::HudPopulator;
#[cfg(feature = "usd-extension")]
pub use idtx_usd_extension::HudAnchor;
//pub use widget::WidgetPopulator;
#[cfg(feature = "usd-extension")]
pub use idtx_usd_extension::{WidgetAlignment, WidgetBillboard, WidgetFit, WidgetLayout, WidgetSpace};
pub use coordinate::{UpAxis, WorldUnitScale, pseudo_root_matrix};
pub use hydrator::{HydrationError, SceneHydrator};
pub use metrics::HydrationMetrics;
pub use player::{
    NetRole, PLAYER_ID_ATTR, PLAYER_ROLE_ATTR, PlayerPopulator, PlayerRole,
    nearest_authored_player_role, nearest_authored_player_root, resolve_owning_player_role,
};
pub use populator::{Populator, PopulatorRegistry};
pub use schema_ext::{SchemaHydrationCtx, SchemaHydrator, SchemaHydratorRegistry};
pub use stable_id::stable_prim_hash;

pub use idtx_usd::{
    DirtyDomains, PackedArcKind, PrimId, PrimSpecifier, PrimState, RenderPurpose, StageChangeBatch,
    TopologyDelta,
};
