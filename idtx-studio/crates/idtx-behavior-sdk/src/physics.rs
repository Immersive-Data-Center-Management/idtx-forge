//! Runtime physics control (`physics` capability) — grab suspend/resume.
//!
//! A behavior granted `physics` may temporarily **suspend** simulation of a
//! specific body and **resume** it. This is the grab primitive: a dynamic rigid
//! body (`UsdPhysicsRigidBodyAPI`) is fought by the solver, which re-authors its
//! pose from gravity every frame — so a behavior that authors the grabbed prim's
//! transform (see [`crate::stage_edit::StageEdit::set_transform`]) is overwritten
//! and the prim cannot be grabbed. Suspending flips the body to kinematic while
//! held: it stops integrating gravity and is no longer authored by physics (the
//! grab's authored pose wins), yet still collides with and pushes other dynamics.
//! Resuming flips it back to dynamic, carrying the grab motion's velocity as a
//! throw.
//!
//! The call is **deferred** (like [`crate::stage_edit`]): it enqueues a command
//! the host hands to the session, which applies it to the physics subsystem
//! before the next physics tick. On wasm it routes to the capability-gated
//! `idtx_physics_set_simulated` host import (absent — and therefore link-failing
//! — without the grant); natively it is an inert `Ok(())` so a behavior using it
//! stays unit-testable without a host.
//!
//! Suspend/resume target a body by **entity**; a request against an entity that
//! is not a tracked dynamic body (a static prop, an animated body, or anything
//! with no `UsdPhysicsScene` active) is a host-side no-op, so a grab behavior can
//! call it unconditionally on whatever it grabbed.

use crate::abi::Entity as WireEntity;
use crate::entity::Entity;
use crate::result::{Result, check};

/// Runtime physics-control surface.
///
/// Requires the behavior's manifest to grant the `physics` capability (otherwise
/// the host import is unlinked and the module fails to load). Obtain one from
/// [`crate::ctx::Ctx::physics`]. Cheap to copy — holds no state; every method
/// posts a single deferred command.
#[derive(Clone, Copy, Debug)]
pub struct Physics {
    _private: (),
}

impl Physics {
    /// Construct the handle for the current hook. Cheap; holds no state.
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Set whether `entity`'s body is simulated: `true` resumes dynamic
    /// simulation, `false` suspends it (flip to kinematic while grabbed). A
    /// no-op host-side when `entity` is not a tracked dynamic body.
    pub fn set_simulated(self, entity: Entity, simulated: bool) -> Result<()> {
        Self::post(entity.wire(), simulated)
    }

    /// Suspend simulation of `entity`'s body — the grab gesture. Shorthand for
    /// `set_simulated(entity, false)`.
    pub fn suspend(self, entity: Entity) -> Result<()> {
        Self::post(entity.wire(), false)
    }

    /// Resume simulation of `entity`'s body — the release gesture. Shorthand for
    /// `set_simulated(entity, true)`.
    pub fn resume(self, entity: Entity) -> Result<()> {
        Self::post(entity.wire(), true)
    }

    /// Resolve `entity` (typically a raycast hit on a child collider) up to its
    /// rigid-body root (`UsdPhysicsRigidBodyAPI`, self or nearest ancestor), or
    /// `None` when it is inside no rigid body. Use this before [`suspend`]/
    /// [`resume`] so grabbing any part of a compound/whole-asset body grabs the
    /// whole body. Natively (off-wasm) always `None`.
    ///
    /// [`suspend`]: Self::suspend
    /// [`resume`]: Self::resume
    #[must_use]
    pub fn body(self, entity: Entity) -> Option<Entity> {
        #[cfg(target_arch = "wasm32")]
        {
            // Zero-init so a non-Ok host return (capability denied, stale entity,
            // no backend) — which does not write the slot — decodes to NULL.
            let mut out = [0u8; core::mem::size_of::<WireEntity>()];
            unsafe {
                crate::__rt::idtx_scene_physics_body(entity.wire().to_bits(), out.as_mut_ptr());
            }
            let wire: WireEntity = bytemuck::pod_read_unaligned(&out);
            if wire.is_null() {
                None
            } else {
                Some(Entity::from_wire(wire))
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = entity;
            None
        }
    }

    fn post(target: WireEntity, simulated: bool) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_physics_set_simulated(target.to_bits(), u32::from(simulated))
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, simulated);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }
}
