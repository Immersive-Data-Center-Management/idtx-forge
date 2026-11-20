//! `Entity` — the safe handle wrapper an author interacts with.
//!
//! No raw pointers cross into author code: `get_xform` / `set_xform` /
//! `world_xform` / hierarchy accessors marshal the POD wire types through
//! stack buffers and route to the host imports on wasm. Natively they
//! short-circuit to defaults so author logic is unit-testable.

use crate::abi::{Entity as WireEntity, Xform};
use crate::result::{Result, check};
use glam::{Mat4, Quat};

/// A safe entity handle.
///
/// Wraps the opaque wire [`WireEntity`]. Obtain one from
/// [`crate::ctx::Ctx::entity`] (the self prim), [`Entity::find`], or the
/// hierarchy accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entity {
    wire: WireEntity,
}

/// A decomposed local transform an author reads / writes.
///
/// Mirrors the ABI [`Xform`] but exposes glam types directly for
/// ergonomic math (`x.rotation = Quat::from_rotation_y(a)`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub translation: glam::Vec3,
    pub rotation: Quat,
    pub scale: glam::Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: glam::Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: glam::Vec3::ONE,
        }
    }
}

impl From<Xform> for Transform {
    fn from(x: Xform) -> Self {
        Self {
            translation: x.translation.into(),
            rotation: x.rotation.into(),
            scale: x.scale.into(),
        }
    }
}

impl From<Transform> for Xform {
    fn from(t: Transform) -> Self {
        Xform::from_trs(t.translation, t.rotation, t.scale)
    }
}

impl Transform {
    /// Compose this decomposed transform into a 4×4 matrix
    /// (`scale → rotation → translation`).
    #[must_use]
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Decompose a 4×4 matrix back into a [`Transform`] — the inverse of
    /// [`Self::to_matrix`].
    ///
    /// Centralizes the `to_scale_rotation_translation` boilerplate so a
    /// desired local matrix (e.g. from [`crate::math::local_in_parent`]) can be
    /// authored directly via [`Entity::set_xform`] /
    /// [`crate::stage_edit::StageEdit::set_transform`]. A matrix with negative
    /// or sheared scale is decomposed on a best-effort basis, matching glam's
    /// `Mat4::to_scale_rotation_translation`.
    #[must_use]
    pub fn from_matrix(m: Mat4) -> Self {
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        Self {
            translation,
            rotation,
            scale,
        }
    }
}

impl Entity {
    /// Wrap a raw wire handle. Used by the SDK glue and hierarchy
    /// accessors; authors rarely call this directly.
    #[must_use]
    pub fn from_wire(wire: WireEntity) -> Self {
        Self { wire }
    }

    /// The underlying wire handle.
    #[must_use]
    pub fn wire(self) -> WireEntity {
        self.wire
    }

    /// `true` when this is the stage pseudo-root / null handle.
    #[must_use]
    pub fn is_null(self) -> bool {
        self.wire.is_null()
    }

    /// Read this entity's local transform.
    pub fn get_xform(self) -> Result<Transform> {
        #[allow(unused_mut)] // written via raw ptr on wasm; via copy natively
        let mut buf = [0u8; core::mem::size_of::<Xform>()];
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_get_xform(self.wire.to_bits(), buf.as_mut_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            // Natively: hand back identity.
            let x = Xform::default();
            buf.copy_from_slice(bytemuck::bytes_of(&x));
            crate::abi::ErrorCode::Ok
        };
        check(code)?;
        // `buf` is `[u8; N]` (alignment 1); read the POD unaligned.
        let x: Xform = bytemuck::pod_read_unaligned(&buf);
        Ok(x.into())
    }

    /// Write this entity's local transform (marks it dirty so the
    /// transform sweep refreshes the world matrix the same frame).
    pub fn set_xform(self, t: &Transform) -> Result<()> {
        let x: Xform = (*t).into();
        let bytes = bytemuck::bytes_of(&x);
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_set_xform(self.wire.to_bits(), bytes.as_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = bytes;
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }

    /// Read this entity's world-space transform (read-only
    /// `GlobalTransform`).
    pub fn world_xform(self) -> Result<Transform> {
        #[allow(unused_mut)] // written via raw ptr on wasm; via copy natively
        let mut buf = [0u8; core::mem::size_of::<Xform>()];
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_world_xform(self.wire.to_bits(), buf.as_mut_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let x = Xform::default();
            buf.copy_from_slice(bytemuck::bytes_of(&x));
            crate::abi::ErrorCode::Ok
        };
        check(code)?;
        // `buf` is `[u8; N]` (alignment 1); read the POD unaligned.
        let x: Xform = bytemuck::pod_read_unaligned(&buf);
        Ok(x.into())
    }

    /// This entity's world-space transform as a 4×4 matrix — the matrix form of
    /// [`Self::world_xform`]. Convenient for composing child-local poses into
    /// world space (see [`Self::local_to_world`]).
    pub fn world_matrix(self) -> Result<Mat4> {
        Ok(self.world_xform()?.to_matrix())
    }

    /// Place a child-local transform into world space using **this prim as the
    /// parent**: `self.world_matrix() * local`.
    ///
    /// The canonical use is lifting an XR controller aim/grip pose — which the
    /// `xr_hmd` accessor reports in play space, i.e. local to the pawn — into
    /// stage/world space so a scene raycast fires from where the controller
    /// actually points after the player has moved or snap-turned:
    ///
    /// ```ignore
    /// let aim = ctx.xr().get_pose(XrSpace::RightAim)?;   // pawn-local pose
    /// ctx.stage_edit().set_self_transform(&aim)?;        // render the prim
    /// let world = ctx.entity().parent()?.local_to_world(&aim)?;
    /// let origin = world.transform_point3(Vec3::ZERO);
    /// let dir = world.transform_vector3(Vec3::NEG_Z).normalize();
    /// ctx.scene().raycast(origin, dir, &RayOpts::new().max_distance(50.0));
    /// ```
    pub fn local_to_world(self, local: &Transform) -> Result<Mat4> {
        Ok(self.world_matrix()? * local.to_matrix())
    }

    /// Read this entity's **effective** visibility — the composed result the
    /// renderer draws from, i.e. this prim's own visibility opinion folded with
    /// its ancestor chain (an `inherited` prim under an `invisible` parent reads
    /// `false`). This is the state [`crate::stage_edit::StageEdit::set_visibility`]
    /// drives, so a behavior can read back what it (or the scene-tree) last set,
    /// or branch on whether a target is currently shown before toggling it:
    ///
    /// ```ignore
    /// if !ctx.entity().visible()? {
    ///     ctx.stage_edit().set_self_visibility(true)?;
    /// }
    /// ```
    ///
    /// Requires the `scene_query` capability; natively (off-wasm) always
    /// reports `true` so author unit tests have a stable default.
    pub fn visible(self) -> Result<bool> {
        #[allow(unused_mut)] // written via raw ptr on wasm; via copy natively
        let mut buf = [0u8; 1];
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_get_visibility(self.wire.to_bits(), buf.as_mut_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            buf[0] = 1; // default visible
            crate::abi::ErrorCode::Ok
        };
        check(code)?;
        Ok(buf[0] != 0)
    }

    /// Resolve a prim path (`/World/Camera`) to an entity handle.
    pub fn find(path: &str) -> Result<Entity> {
        #[allow(unused_mut)] // written via raw ptr on wasm only
        let mut buf = [0u8; core::mem::size_of::<WireEntity>()];
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_entity_find(
                path.as_ptr(),
                u32::try_from(path.len()).unwrap_or(u32::MAX),
                buf.as_mut_ptr(),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = path;
            crate::abi::ErrorCode::NotFound
        };
        check(code)?;
        // `buf` is `[u8; N]` (alignment 1); read the POD unaligned.
        let wire: WireEntity = bytemuck::pod_read_unaligned(&buf);
        Ok(Entity::from_wire(wire))
    }

    /// This entity's parent, if any.
    pub fn parent(self) -> Result<Entity> {
        #[allow(unused_mut)] // written via raw ptr on wasm only
        let mut buf = [0u8; core::mem::size_of::<WireEntity>()];
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_entity_parent(self.wire.to_bits(), buf.as_mut_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = crate::abi::ErrorCode::NotFound;
        check(code)?;
        // `buf` is `[u8; N]` (alignment 1); read the POD unaligned.
        let wire: WireEntity = bytemuck::pod_read_unaligned(&buf);
        Ok(Entity::from_wire(wire))
    }

    /// This entity's `index`-th child.
    pub fn child(self, index: u32) -> Result<Entity> {
        #[allow(unused_mut)] // written via raw ptr on wasm only
        let mut buf = [0u8; core::mem::size_of::<WireEntity>()];
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_entity_child(self.wire.to_bits(), index, buf.as_mut_ptr())
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = index;
            crate::abi::ErrorCode::NotFound
        };
        check(code)?;
        // `buf` is `[u8; N]` (alignment 1); read the POD unaligned.
        let wire: WireEntity = bytemuck::pod_read_unaligned(&buf);
        Ok(Entity::from_wire(wire))
    }

    /// Number of direct children.
    #[must_use]
    pub fn child_count(self) -> u32 {
        #[cfg(target_arch = "wasm32")]
        {
            unsafe { crate::__rt::idtx_entity_child_count(self.wire.to_bits()) }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            0
        }
    }
}
