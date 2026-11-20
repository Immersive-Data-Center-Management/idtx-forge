//! XR pose accessor (`xr_hmd` capability).
//!
//! `ctx.xr().get_pose(XrSpace::Hmd)` returns the most recently polled
//! tracked pose for a space, or `None` when that space is not currently
//! tracked. The host updates the pose snapshot before every behavior tick
//! (see `idtx-behavior`'s `BehaviorHost::set_xr_snapshot`).
//!
//! On wasm the read routes to the capability-gated `idtx_xr_get_pose`
//! host import; natively it always returns `None` so a behavior using XR
//! stays unit-testable without a host.

#[cfg(target_arch = "wasm32")]
use crate::abi::XrPoseResult;
use crate::abi::{Xform, XrHandJointResult, XrInputSource, XrSide, XrSpace, XR_HAND_JOINT_COUNT};
use crate::entity::Transform;
use glam::{Quat, Vec3};

// ── Hand-joint ordinals (mirror the XR_HAND_JOINT_*_EXT indices) ─────────────

/// Palm centre (hand-skeleton root), joint index 0.
pub const HAND_JOINT_PALM: usize = 0;
/// Wrist, joint index 1.
pub const HAND_JOINT_WRIST: usize = 1;
/// Thumb tip, joint index 5.
pub const HAND_JOINT_THUMB_TIP: usize = 5;
/// Index-finger proximal phalanx (base knuckle), joint index 7. Paired with
/// [`HAND_JOINT_INDEX_TIP`] it gives the index-finger pointing direction
/// (`tip - proximal`) a teleport / pointer behavior aims along.
pub const HAND_JOINT_INDEX_PROXIMAL: usize = 7;
/// Index-finger tip, joint index 10.
pub const HAND_JOINT_INDEX_TIP: usize = 10;

/// Handle to the XR pose surface. Requires the behavior's manifest to
/// grant the `xr_hmd` capability (otherwise the host import is unlinked
/// and the module fails to load).
#[derive(Clone, Copy, Debug, Default)]
pub struct XrHmd {
    _private: (),
}

impl XrHmd {
    /// Construct the handle. Cheap; holds no state.
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Read the current pose for `space` as a raw wire [`Xform`], or
    /// `None` when the space is not tracked this frame.
    #[must_use]
    pub fn get_pose_xform(self, space: XrSpace) -> Option<Xform> {
        #[cfg(target_arch = "wasm32")]
        {
            // The host writes an `XrPoseResult` into the buffer and
            // returns an `ErrorCode`. Any non-Ok code means "no pose".
            let mut buf = [0u8; core::mem::size_of::<XrPoseResult>()];
            let code = crate::abi::ErrorCode::from_u32(unsafe {
                crate::__rt::idtx_xr_get_pose(space.as_u32(), buf.as_mut_ptr())
            });
            if !code.is_ok() {
                return None;
            }
            // `buf` is `[u8; N]` (alignment 1); read the POD unaligned so a
            // response landing off a 4-byte boundary can't panic.
            let res: XrPoseResult = bytemuck::pod_read_unaligned(&buf);
            if res.is_valid() {
                Some(res.xform)
            } else {
                None
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Natively: no XR device, so no pose.
            let _ = space;
            None
        }
    }

    /// Read the current pose for `space` as a decomposed [`Transform`], or
    /// `None` when the space is not tracked this frame.
    #[must_use]
    pub fn get_pose(self, space: XrSpace) -> Option<Transform> {
        self.get_pose_xform(space).map(Transform::from)
    }

    /// The current play-space **recenter offset** — a stage-space ground
    /// translation (y = 0) the host wants the pawn re-based by so the body
    /// tracks the headset (see the session's `recenter_step`). A pawn-root
    /// behavior adds `Quat::from_rotation_y(yaw) * recenter()` to its authored
    /// translation; the compositor subtracts the same offset from every tracked
    /// pose, so the view and the hydrated camera/controllers stay fixed while the
    /// static capsule follows the head. `Vec3::ZERO` when no recenter is active
    /// (and always natively, so XR behaviors stay unit-testable without a host).
    #[must_use]
    pub fn recenter(self) -> Vec3 {
        #[cfg(target_arch = "wasm32")]
        {
            // The host writes an `abi::Vec3` (12 bytes) and returns an
            // `ErrorCode`; any non-Ok code means "no recenter".
            let mut buf = [0u8; core::mem::size_of::<crate::abi::Vec3>()];
            let code = crate::abi::ErrorCode::from_u32(unsafe {
                crate::__rt::idtx_xr_get_recenter(buf.as_mut_ptr())
            });
            if !code.is_ok() {
                return Vec3::ZERO;
            }
            // `buf` is `[u8; N]` (alignment 1); read the POD unaligned.
            let v: crate::abi::Vec3 = bytemuck::pod_read_unaligned(&buf);
            v.into()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Vec3::ZERO
        }
    }

    /// Read the full 26-joint skeleton for `side`, or `None` when that hand is
    /// not tracked this frame. Requires the `xr_hmd` capability.
    ///
    /// The joints are in stage space (`-recenter` compensated like the pose
    /// spaces), so a behavior can derive its own anchor — e.g. the pinch
    /// midpoint between [`HAND_JOINT_THUMB_TIP`] and [`HAND_JOINT_INDEX_TIP`] —
    /// without the host pre-selecting one. Natively always `None`, so an XR
    /// behavior stays unit-testable without a host.
    #[must_use]
    pub fn hand_joints(self, side: XrSide) -> Option<HandJoints> {
        #[cfg(target_arch = "wasm32")]
        {
            let mut buf = [0u8; core::mem::size_of::<[XrHandJointResult; XR_HAND_JOINT_COUNT]>()];
            let code = crate::abi::ErrorCode::from_u32(unsafe {
                crate::__rt::idtx_xr_get_hand_joints(side.as_u32(), buf.as_mut_ptr())
            });
            if !code.is_ok() {
                return None;
            }
            // `buf` is `[u8; N]` (alignment 1); `XrHandJointResult` needs 4-byte
            // alignment, so `cast_slice` could panic on a valid host response.
            // Read the POD array unaligned instead.
            let joints =
                bytemuck::pod_read_unaligned::<[XrHandJointResult; XR_HAND_JOINT_COUNT]>(&buf);
            Some(HandJoints { joints })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = side;
            None
        }
    }

    /// Which input source(s) drive `side` this frame — a physical controller, an
    /// articulated hand (hand tracking), or both. Requires the `xr_hmd`
    /// capability.
    ///
    /// Unlike probing [`get_pose`](Self::get_pose) (whose grip/aim spaces the
    /// host synthesises from the hand when no controller is present, hiding the
    /// source), this reports the source directly. The two bits are independent:
    /// a runtime can report a controller *and* hand tracking for the same side
    /// at once, so test [`has_controller`] / [`has_hand`] rather than assume
    /// they are exclusive. [`XrInputSource::NONE`] when the side is not tracked,
    /// and always natively, so an XR behavior stays unit-testable without a host.
    ///
    /// [`has_controller`]: XrInputSource::has_controller
    /// [`has_hand`]: XrInputSource::has_hand
    #[must_use]
    pub fn input_source(self, side: XrSide) -> XrInputSource {
        #[cfg(target_arch = "wasm32")]
        {
            // The host returns the bitmask directly as the import's `u32`.
            let bits = unsafe { crate::__rt::idtx_xr_get_tracking_mode(side.as_u32()) };
            XrInputSource::from_bits(bits)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = side;
            XrInputSource::NONE
        }
    }
}

/// A tracked hand's 26-joint skeleton, read via [`XrHmd::hand_joints`].
///
/// Joint indices follow the `XR_HAND_JOINT_*_EXT` ordinals; use the
/// `HAND_JOINT_*` constants for the common ones.
#[derive(Clone, Copy, Debug)]
pub struct HandJoints {
    joints: [XrHandJointResult; XR_HAND_JOINT_COUNT],
}

impl HandJoints {
    /// The raw joint array (positions in metres, orientations xyzw, per-joint
    /// `valid` flag).
    #[must_use]
    pub fn raw(&self) -> &[XrHandJointResult; XR_HAND_JOINT_COUNT] {
        &self.joints
    }

    /// World-space position of joint `index`, or `None` when the joint is
    /// out of range or not tracked (`valid == 0`).
    #[must_use]
    pub fn position(&self, index: usize) -> Option<Vec3> {
        let j = self.joints.get(index)?;
        (j.valid != 0).then(|| Vec3::from_array(j.position))
    }

    /// Decomposed [`Transform`] (translation + rotation, unit scale) of joint
    /// `index`, or `None` when out of range or not tracked.
    #[must_use]
    pub fn transform(&self, index: usize) -> Option<Transform> {
        let j = self.joints.get(index)?;
        if j.valid == 0 {
            return None;
        }
        Some(Transform {
            translation: Vec3::from_array(j.position),
            rotation: Quat::from_array(j.orientation),
            scale: Vec3::ONE,
        })
    }

    /// The pinch anchor: the midpoint between the thumb tip and index tip, with
    /// the wrist's orientation. `None` unless all three joints are tracked. A
    /// natural "held between the fingers" frame for pinch-to-grab.
    #[must_use]
    pub fn pinch_anchor(&self) -> Option<Transform> {
        let thumb = self.position(HAND_JOINT_THUMB_TIP)?;
        let index = self.position(HAND_JOINT_INDEX_TIP)?;
        let wrist = self.transform(HAND_JOINT_WRIST)?;
        Some(Transform {
            translation: (thumb + index) * 0.5,
            rotation: wrist.rotation,
            scale: Vec3::ONE,
        })
    }
}
