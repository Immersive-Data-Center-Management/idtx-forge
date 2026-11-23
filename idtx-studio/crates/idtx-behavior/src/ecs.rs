//! The ECS bridge contract — the `bevy_ecs` components the behavior host
//! reads and writes.
//!
//! # Why these live here (and not in `idtx-scene`)
//!
//! The uber `idtx-scene` projects USD prims into a *world-matrix* component
//! model (`WorldTransform { matrix }`, hierarchy via `PrimNode.parent:
//! Option<PrimId>`) and depends on `idtx-usd` — which requires the `OpenUSD`
//! link to build. The behavior host must stay **USD-free** so its unit and
//! integration tests run headlessly (no USD link, no wasm toolchain). It
//! therefore defines its own small, decomposed transform + hierarchy +
//! path contract here, mirroring the reference `idtx-poc`'s `idtx-ecs`
//! component set.
//!
//! A session that wires the host into a live scene world is
//! responsible for keeping these bridge components in sync with the USD
//! projection — e.g. inserting a [`Transform`]/[`UsdPath`] on the synthetic
//! preview-camera entity the HMD-sync behavior drives, and reading the
//! written [`Transform`] back into the stage. Nothing in Phases 0–2 writes
//! back to USD.
//!
//! All types are plain `#[derive(Component)]` POD-ish structs; the crate
//! is `unsafe`-free except for the guarded world pointer in
//! [`crate::stage_access`].

use bevy_ecs::prelude::*;
use glam::{Mat4, Quat, Vec3};

/// A prim's decomposed *local* transform plus its cached local matrix.
///
/// `set_xform` writes the TRS fields and recomputes [`Self::matrix`]; a
/// downstream transform sweep (session-owned) is expected to propagate it
/// into a world matrix and mark descendants dirty.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Local translation.
    pub translation: Vec3,
    /// Local rotation.
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
    /// Cached local matrix (`from_scale_rotation_translation`).
    pub matrix: Mat4,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            matrix: Mat4::IDENTITY,
        }
    }
}

impl Transform {
    /// Recompute [`Self::matrix`] from the current TRS fields.
    pub fn recompute_matrix(&mut self) {
        self.matrix =
            Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation);
    }
}

/// A prim's world-space transform (read-only from a behavior's view).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct GlobalTransform(pub Mat4);

impl Default for GlobalTransform {
    fn default() -> Self {
        Self(Mat4::IDENTITY)
    }
}

/// Parent link in the ECS hierarchy the host's scene-query imports walk.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parent(pub Entity);

/// Ordered child links.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct Children(pub Vec<Entity>);

/// The prim's USD path, keyed by `idtx_entity_find`.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct UsdPath(pub String);

/// Bounded, authored prim data projected by the session outside guest hooks.
/// Keys are exact customData key paths; values are JSON, never printed USD text.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct PrimBehaviorData {
    pub custom_data: std::collections::BTreeMap<String, String>,
    pub context: Option<String>,
}

/// A composed prim that is inactive or not loaded. Synthetic host entities
/// need no marker. Handles to unavailable prims are never usable bindings.
#[derive(Component, Debug, Clone, Copy)]
pub struct Unavailable;

/// Revisioned binding shared by all behaviors querying the same prim/property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipBinding {
    pub revision: u64,
    pub resolved: bool,
    pub targets: Vec<idtx_behavior_abi::RelationshipTarget>,
}

#[derive(Component, Debug, Clone, Default)]
pub struct Relationships(pub std::collections::BTreeMap<String, RelationshipBinding>);

/// Marker inserted by `set_xform` so a session-owned transform sweep knows
/// to refresh the world matrix for this prim this frame.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransformDirty;
