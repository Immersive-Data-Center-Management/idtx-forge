//! Bone/joint mesh caches for XR hand-skeleton visualisation.
//!
//! Each hand is rendered as:
//!  * one **thin capsule** per bone (wrist→tip along each finger chain), using
//!    [`bone_world_matrix`] to scale and orient a unit capsule between joints.
//!  * one **small sphere** per joint scaled to that joint's tracked radius.
//!
//! [`HandMeshCache`] holds the shared [`GpuMeshDesc`] for the template capsule
//! and sphere. Both hands feed **two hardware-instanced batches** — one bone
//! prototype, one joint prototype — so the entire two-hand skeleton (~100
//! bones+joints) renders in exactly **two** instanced draw calls with two GPU
//! cache entries, instead of ~100 individual meshes/buffers/draws (B4, see
//! `docs/xr-preview-performance.md`). Only the per-instance world matrices are
//! rebuilt each frame from the current [`HandPose`].
//!
//! # Visualisation mode
//!
//! [`XrHandVisualization`] controls whether to show controllers or hand skeletons.
//! The default is [`Auto`][XrHandVisualization::Auto]: use hand skeleton when the
//! device is actively tracking a hand, fall back to the controller capsule when
//! not.

use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use idtx_render::GpuMeshDesc;
use idtx_render::material::fallback_material;
use idtx_render::scene_extract::RenderInstancedBatch;
use idtx_scene::RenderPurpose;

#[cfg(feature = "xr-preview")]
use idtx_platform_xr::{HAND_JOINT_COUNT, HAND_JOINT_PARENTS, HandPose};

/// How XR controllers / hand skeletons are drawn in the preview.
///
/// The shell exposes this as a combo-box in the XR panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XrHandVisualization {
    /// Show hand skeleton when tracked; fall back to controller capsule otherwise.
    #[default]
    Auto,
    /// Always render the controller capsule (suppresses hand skeleton).
    ControllerOnly,
    /// Always render the hand skeleton (suppresses controller capsule).
    HandSkeletonOnly,
}

impl XrHandVisualization {
    /// `true` when controller capsules should be injected for `side` (left=`true`).
    /// `hand_active` indicates whether hand-tracking is currently supplying a pose.
    #[must_use]
    pub fn show_controller(self, hand_active: bool) -> bool {
        match self {
            Self::Auto => !hand_active,
            Self::ControllerOnly => true,
            Self::HandSkeletonOnly => false,
        }
    }

    /// `true` when the hand skeleton should be injected for `side`.
    #[must_use]
    pub fn show_hand(self, hand_active: bool) -> bool {
        match self {
            Self::Auto => hand_active,
            Self::ControllerOnly => false,
            Self::HandSkeletonOnly => hand_active,
        }
    }
}

// ── Capsule template ───────────────────────────────────────────────────────────

/// Bone capsule half-radius in metres (~3 mm).
const BONE_RADIUS: f32 = 0.003;

/// The template capsule has total height 4.0 (hemispheres of radius 1.0 each
/// side, cylinder of height 2.0). All scaling is done via the world matrix.
const CAPSULE_SLICES: u32 = 12;
const CAPSULE_HEMI_STACKS: u32 = 3;
const CAPSULE_TOTAL_HEIGHT: f32 = 4.0; // 2 × unit hemi + cylinder of h=2

/// Bone prim-path prefix for the GPU-resource cache key.
const BONE_PATH_PREFIX: &str = "__xr_hand_bone__";
/// Joint prim-path prefix for the GPU-resource cache key.
const JOINT_PATH_PREFIX: &str = "__xr_hand_joint__";

/// Lazily-built template meshes for hand visualisation.
///
/// The same bone capsule and joint sphere are reused for every bone/joint in
/// both hands: only the world matrix differs per instance.
#[derive(Debug)]
pub struct HandMeshCache {
    bone_mesh: Arc<GpuMeshDesc>,
    joint_mesh: Arc<GpuMeshDesc>,
}

impl HandMeshCache {
    /// Build the template meshes. Pure CPU work.
    #[must_use]
    pub fn new() -> Self {
        let mut bone =
            idtx_render::mesh::tessellate_capsule(1.0, 2.0, CAPSULE_SLICES, CAPSULE_HEMI_STACKS);
        bone.prim_path = BONE_PATH_PREFIX.to_owned();

        let mut joint = idtx_render::mesh::tessellate_sphere(1.0, 8, 16);
        joint.prim_path = JOINT_PATH_PREFIX.to_owned();

        Self {
            bone_mesh: Arc::new(bone),
            joint_mesh: Arc::new(joint),
        }
    }

    /// Assemble the (up to two) hand-skeleton instanced batches from the
    /// accumulated per-instance world matrices. Both hands share these two
    /// batches, so the whole two-hand skeleton is two instanced draws.
    ///
    /// A batch is omitted when its transform accumulator is empty (e.g. only one
    /// hand is tracked, or joints-only). Returns an empty `Vec` when both are
    /// empty.
    #[must_use]
    pub fn build_batches(
        &self,
        joint_transforms: Vec<Mat4>,
        bone_transforms: Vec<Mat4>,
    ) -> Vec<RenderInstancedBatch> {
        let mut out = Vec::with_capacity(2);
        if !joint_transforms.is_empty() {
            out.push(RenderInstancedBatch {
                prototype_key: JOINT_PATH_PREFIX.to_owned(),
                mesh: Arc::clone(&self.joint_mesh),
                instance_transforms: joint_transforms,
                // Post-extract synthetic geometry: no per-instance USD prim
                // identity, matching the `PointInstancer` convention. Leaving
                // this empty means `patch_world_matrices` never touches these
                // transforms (correct — hands are not authored in USD).
                instance_prim_paths: Vec::new(),
                material: hand_material(JOINT_PATH_PREFIX),
                purpose: RenderPurpose::Default,
                casts_shadow: true,
            });
        }
        if !bone_transforms.is_empty() {
            out.push(RenderInstancedBatch {
                prototype_key: BONE_PATH_PREFIX.to_owned(),
                mesh: Arc::clone(&self.bone_mesh),
                instance_transforms: bone_transforms,
                instance_prim_paths: Vec::new(),
                material: hand_material(BONE_PATH_PREFIX),
                purpose: RenderPurpose::Default,
                casts_shadow: true,
            });
        }
        out
    }
}

impl Default for HandMeshCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── Matrix helpers ─────────────────────────────────────────────────────────────

/// World matrix that places the unit capsule (template height 4.0) between
/// `from` and `to` with the given `radius`.
///
/// The capsule's axis is Y; we rotate Y → bone direction and scale to match the
/// true bone length and radius.
#[must_use]
pub fn bone_world_matrix(from: Vec3, to: Vec3, radius: f32) -> Mat4 {
    let diff = to - from;
    let length = diff.length();
    if length < 1e-6 {
        // Degenerate zero-length bone: place a tiny sphere at `from`.
        return Mat4::from_scale_rotation_translation(Vec3::splat(radius), Quat::IDENTITY, from);
    }
    let dir = diff / length;
    let rotation = Quat::from_rotation_arc(Vec3::Y, dir);
    // Capsule template total height is CAPSULE_TOTAL_HEIGHT (4.0).
    // Y-scale → actual length; XZ-scale → actual radius.
    let scale = Vec3::new(radius, length / CAPSULE_TOTAL_HEIGHT, radius);
    let midpoint = from + diff * 0.5;
    Mat4::from_scale_rotation_translation(scale, rotation, midpoint)
}

// ── Material ───────────────────────────────────────────────────────────────────

/// Warm skin-tone fallback material for hand geometry.
fn hand_material(cache_key: &str) -> idtx_render::material::RenderMaterialDesc {
    fallback_material(cache_key)
}

// ── Snapshot injection (xr-preview feature) ───────────────────────────────────

/// Strip previously injected hand bone/joint **instanced batches** from a
/// snapshot so they do not accumulate on reused (non-dirty) snapshots.
///
/// Returns `true` if any hand batch was removed, so the caller can bump the
/// snapshot serial to force the renderer's serial-guarded instance cache to
/// evict the now-orphaned GPU entries.
pub fn strip_hand_instanced_batches(batches: &mut Vec<RenderInstancedBatch>) -> bool {
    let before = batches.len();
    batches.retain(|b| {
        !b.prototype_key.starts_with(BONE_PATH_PREFIX)
            && !b.prototype_key.starts_with(JOINT_PATH_PREFIX)
    });
    batches.len() != before
}

/// Total hand instances across the injected bone/joint batches (metrics
/// diagnostic; replaces the old per-mesh `count_hand_instances`).
///
/// Only called from `render_xr` (pcvr + windows), hence `allow(dead_code)` for
/// metrics builds that omit that path (e.g. `mock-xr`).
#[cfg(feature = "metrics")]
#[allow(dead_code)]
#[must_use]
pub fn count_hand_batch_instances(batches: &[RenderInstancedBatch]) -> usize {
    batches
        .iter()
        .filter(|b| {
            b.prototype_key.starts_with(BONE_PATH_PREFIX)
                || b.prototype_key.starts_with(JOINT_PATH_PREFIX)
        })
        .map(|b| b.instance_transforms.len())
        .sum()
}

/// Append one hand's joint-sphere and bone-capsule world matrices to the shared
/// per-instance transform accumulators (see [`HandMeshCache::build_batches`]).
///
/// Both hands push into the same two accumulators, so the resulting batches
/// cover the whole skeleton in two draws.
///
/// `placement` maps the raw play-space joint positions into the virtual world so
/// the skeleton follows the player pawn exactly like the eye cameras and the
/// hydrated controller child-prims. It is
/// `projection_basis * worldMatrix(/World/Player) * T(−recenter)` (locomotion +
/// snap-turn yaw, with the play-space recenter offset removed), or [`Mat4::IDENTITY`]
/// when no local-player pawn resolves. Hand joints are located by
/// `xrLocateHandJointsEXT` in play (STAGE) space — they are *not* authored USD
/// prims under the pawn — so without this pre-multiply they render frozen at
/// their raw tracked position while the player moves away.
#[cfg(feature = "xr-preview")]
pub fn push_hand_transforms(
    pose: &HandPose,
    placement: Mat4,
    joint_transforms: &mut Vec<Mat4>,
    bone_transforms: &mut Vec<Mat4>,
) {
    let joints = &pose.joints;

    // Joint spheres — scale to the tracked radius (minimum visible size).
    for j in joints.iter() {
        let scale = j.radius.max(0.002);
        joint_transforms.push(
            placement
                * Mat4::from_scale_rotation_translation(
                    Vec3::splat(scale),
                    Quat::IDENTITY,
                    j.position,
                ),
        );
    }

    // Bone capsules: one per joint that has a parent.
    for (child_idx, parent_opt) in HAND_JOINT_PARENTS.iter().enumerate().take(HAND_JOINT_COUNT) {
        if let Some(parent_idx) = parent_opt {
            let from = joints[*parent_idx].position;
            let to = joints[child_idx].position;
            bone_transforms.push(placement * bone_world_matrix(from, to, BONE_RADIUS));
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_builds_without_panic() {
        let c = HandMeshCache::new();
        assert!(!c.bone_mesh.positions.is_empty(), "bone mesh has vertices");
        assert!(
            !c.joint_mesh.positions.is_empty(),
            "joint mesh has vertices"
        );
    }

    #[test]
    fn bone_world_matrix_degenerate_zero_length() {
        let m = bone_world_matrix(Vec3::ZERO, Vec3::ZERO, BONE_RADIUS);
        // Should not panic; scale should be BONE_RADIUS
        let (scale, _, _) = m.to_scale_rotation_translation();
        assert!((scale.x - BONE_RADIUS).abs() < 1e-6);
    }

    #[test]
    fn bone_world_matrix_y_axis_bone() {
        let from = Vec3::new(0.0, 0.0, 0.0);
        let to = Vec3::new(0.0, 0.1, 0.0); // 10 cm along Y
        let m = bone_world_matrix(from, to, BONE_RADIUS);
        let (scale, rot, translation) = m.to_scale_rotation_translation();
        // translation should be the midpoint (0, 0.05, 0)
        assert!(
            (translation.y - 0.05).abs() < 1e-5,
            "midpoint y = {}",
            translation.y
        );
        // Y scale = 0.1 / 4.0 = 0.025
        assert!((scale.y - 0.025).abs() < 1e-5, "scale.y = {}", scale.y);
        // XZ scale = BONE_RADIUS
        assert!((scale.x - BONE_RADIUS).abs() < 1e-6);
        // rotation: Y→Y, so rotation arc should be identity-ish
        let _ = rot; // just verifies no panic
    }

    #[test]
    fn visualization_mode_auto_with_hand_active() {
        let v = XrHandVisualization::Auto;
        assert!(!v.show_controller(true));
        assert!(v.show_hand(true));
    }

    #[test]
    fn visualization_mode_auto_without_hand() {
        let v = XrHandVisualization::Auto;
        assert!(v.show_controller(false));
        assert!(!v.show_hand(false));
    }

    #[test]
    fn visualization_mode_controller_only() {
        let v = XrHandVisualization::ControllerOnly;
        assert!(v.show_controller(true));
        assert!(!v.show_hand(true));
        assert!(v.show_controller(false));
        assert!(!v.show_hand(false));
    }

    #[test]
    fn visualization_mode_hand_skeleton_only() {
        let v = XrHandVisualization::HandSkeletonOnly;
        assert!(!v.show_controller(true));
        assert!(v.show_hand(true));
        // When hand inactive, HandSkeletonOnly still suppresses controller
        assert!(!v.show_controller(false));
        assert!(!v.show_hand(false));
    }

    #[test]
    fn strip_hand_instanced_batches_removes_correct_keys() {
        let make = |key: &str| RenderInstancedBatch {
            prototype_key: key.to_owned(),
            mesh: Arc::new(idtx_render::mesh::tessellate_capsule(0.01, 0.02, 8, 2)),
            instance_transforms: vec![Mat4::IDENTITY, Mat4::IDENTITY],
            instance_prim_paths: Vec::new(),
            material: fallback_material(key),
            purpose: RenderPurpose::Default,
            casts_shadow: true,
        };
        let mut batches = vec![
            make("__xr_hand_bone__"),
            make("__xr_hand_joint__"),
            make("/World/PointInstancer"),
        ];
        let removed = strip_hand_instanced_batches(&mut batches);
        assert!(removed, "should report that hand batches were removed");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].prototype_key, "/World/PointInstancer");
        // A second strip on the cleaned list removes nothing.
        assert!(!strip_hand_instanced_batches(&mut batches));
    }

    #[test]
    fn build_batches_collapses_two_hands_into_two_draws() {
        let cache = HandMeshCache::new();
        // Simulate two hands: 26 joints each + some bones each.
        let joint_xf = vec![Mat4::IDENTITY; 52];
        let bone_xf = vec![Mat4::IDENTITY; 50];
        let batches = cache.build_batches(joint_xf, bone_xf);
        assert_eq!(
            batches.len(),
            2,
            "exactly two instanced draws for both hands"
        );
        let joint = batches
            .iter()
            .find(|b| b.prototype_key == "__xr_hand_joint__")
            .unwrap();
        let bone = batches
            .iter()
            .find(|b| b.prototype_key == "__xr_hand_bone__")
            .unwrap();
        assert_eq!(joint.instance_transforms.len(), 52);
        assert_eq!(bone.instance_transforms.len(), 50);
    }

    #[test]
    fn build_batches_omits_empty_accumulators() {
        let cache = HandMeshCache::new();
        assert!(cache.build_batches(Vec::new(), Vec::new()).is_empty());
        let only_joints = cache.build_batches(vec![Mat4::IDENTITY], Vec::new());
        assert_eq!(only_joints.len(), 1);
        assert_eq!(only_joints[0].prototype_key, "__xr_hand_joint__");
    }

    /// The placement matrix (`projection_basis · worldMatrix(/World/Player) ·
    /// T(−recenter)`) must be pre-multiplied onto every joint and bone transform
    /// so the skeleton follows the pawn, exactly like the eye cameras. A pure
    /// translation placement shifts each tracked joint position by that offset.
    #[cfg(feature = "xr-preview")]
    #[test]
    fn push_hand_transforms_applies_placement_to_joints() {
        use idtx_platform_xr::HandPose;

        let mut pose = HandPose::zeroed();
        // WRIST at a known play-space position; give it a visible radius.
        pose.joints[1].position = Vec3::new(0.2, 1.0, -0.5);
        pose.joints[1].radius = 0.01;

        let offset = Vec3::new(10.0, 0.0, -5.0);
        let placement = Mat4::from_translation(offset);

        let mut joints_placed = Vec::new();
        let mut bones_placed = Vec::new();
        push_hand_transforms(&pose, placement, &mut joints_placed, &mut bones_placed);

        let mut joints_raw = Vec::new();
        let mut bones_raw = Vec::new();
        push_hand_transforms(&pose, Mat4::IDENTITY, &mut joints_raw, &mut bones_raw);

        // The placed wrist sphere sits exactly `offset` from the raw one.
        let placed = joints_placed[1].transform_point3(Vec3::ZERO);
        let raw = joints_raw[1].transform_point3(Vec3::ZERO);
        assert!(
            (placed - (raw + offset)).length() < 1e-5,
            "placed joint {placed:?} should be raw {raw:?} + offset {offset:?}"
        );
        // Bone capsules receive the same shift.
        assert_eq!(bones_placed.len(), bones_raw.len());
        if let (Some(bp), Some(br)) = (bones_placed.first(), bones_raw.first()) {
            let bp_mid = bp.transform_point3(Vec3::ZERO);
            let br_mid = br.transform_point3(Vec3::ZERO);
            assert!((bp_mid - (br_mid + offset)).length() < 1e-5);
        }
    }
}
