//! Renderable mesh instances and hardware-instanced batches.

use std::sync::Arc;

use idtx_usd::RenderPurpose;

use crate::material::RenderMaterialDesc;
use crate::mesh::GpuMeshDesc;

// ── Mesh instance ─────────────────────────────────────────────────────────────

/// A single renderable mesh with its world transform and material.
#[derive(Debug, Clone)]
pub struct RenderMeshInstance {
    pub prim_path: String,
    pub mesh: Arc<GpuMeshDesc>,
    pub world_matrix: glam::Mat4,
    pub material: RenderMaterialDesc,
    pub purpose: RenderPurpose,
    /// Whether this mesh casts shadows (`idtx:castsShadows`, default `true`).
    /// `false` skips it in the shadow depth pre-pass and the shadow AABB.
    pub casts_shadow: bool,
}

// ── Instanced batch ───────────────────────────────────────────────────────────

/// A set of identical mesh copies drawn with a single GPU hardware-instanced call.
///
/// Produced by two separate sources:
/// - `UsdGeomPointInstancer` prims: native USD computes instancer-local matrices
///   including prototype roots; extraction composes world placement and batches them.
/// - Pseudo-instance detection: after regular mesh extraction, meshes with identical
///   geometry (same vertex/index count + sampled position data) and the same material
///   are promoted to an `RenderInstancedBatch`, eliminating redundant GPU buffer copies
///   and draw calls for DCC-exported scenes that lack `instanceable = true`.
/// - Topology-aware detection: prims sharing an internal reference arc to the same
///   prototype are grouped deterministically before the fingerprint fallback.
///
/// The `prototype_key` uniquely identifies the shared geometry for GPU buffer caching.
#[derive(Debug, Clone)]
pub struct RenderInstancedBatch {
    /// Unique cache key for the prototype mesh — typically the USD prim path of the
    /// first (canonical) instance or the `PointInstancer` prototype prim path.
    pub prototype_key: String,
    /// Prototype geometry shared by all instances.
    pub mesh: Arc<GpuMeshDesc>,
    /// World-space model matrices for each instance (column-major, glam convention).
    pub instance_transforms: Vec<glam::Mat4>,
    /// USD prim path of the instance root for each entry in `instance_transforms`.
    /// One-to-one with `instance_transforms`. Empty for `PointInstancer`-derived batches
    /// (no per-instance prim identity exists at that level).
    pub instance_prim_paths: Vec<String>,
    /// Material applied to all instances in this batch (subset batches share the prototype key
    /// with a `/subset_<material>` suffix so they cache independently).
    pub material: RenderMaterialDesc,
    pub purpose: RenderPurpose,
    /// Whether the batch casts shadows (`idtx:castsShadows`, default `true`).
    pub casts_shadow: bool,
}
