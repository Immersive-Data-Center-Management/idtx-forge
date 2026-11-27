//! Named input action accessors (§7).
//!
//! `input::action("jump").pressed()` / `input::axis("move_x")` /
//! `input::mouse_delta()`. Action *names* resolve to an
//! [`InputActionId`](crate::abi::InputActionId) once via the host; a
//! resolved [`Action`] caches the id so repeat reads are integer lookups.
//!
//! Natively every read returns the inactive / zero default so author logic
//! is unit-testable without a host.

use crate::abi::{InputActionId, InputPhase};
pub use crate::abi::PointerSource;

/// A resolved (or unresolved) named input action.
///
/// Obtain one with [`action`]; it caches the resolved id.
#[derive(Clone, Copy, Debug)]
pub struct Action {
    id: InputActionId,
    source: PointerSource,
}

/// Resolve a named action once. Cache the returned [`Action`] in a behavior
/// struct field — the instance persists across hooks — to avoid re-resolving
/// each frame.
#[must_use]
pub fn action(name: &str) -> Action {
    #[cfg(target_arch = "wasm32")]
    let id = InputActionId(unsafe {
        crate::__rt::idtx_input_resolve(
            name.as_ptr(),
            u32::try_from(name.len()).unwrap_or(u32::MAX),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let id = {
        let _ = name;
        InputActionId::UNRESOLVED
    };
    Action { id, source: PointerSource::Unspecified }
}

impl Action {
    /// Read one pointer's independently edge-detected action. An aggregate
    /// action never implies a hand; use this when attaching a menu to a controller.
    pub const fn from_source(mut self, source: PointerSource) -> Self { self.source = source; self }

    pub fn source(self) -> PointerSource {
        if self.source != PointerSource::Unspecified { return self.source; }
        #[cfg(target_arch = "wasm32")]
        return PointerSource::from_u32(unsafe { crate::__rt::idtx_input_source(self.id.0) });
        #[cfg(not(target_arch = "wasm32"))]
        PointerSource::Unspecified
    }
    /// The resolved id (`0` / [`InputActionId::UNRESOLVED`] when the action
    /// is not bound in the stage's `input_map.toml`).
    #[must_use]
    pub fn id(self) -> InputActionId {
        self.id
    }

    /// This action's phase this frame.
    #[must_use]
    pub fn phase(self) -> InputPhase {
        #[cfg(target_arch = "wasm32")]
        {
            InputPhase::from_u32(unsafe { crate::__rt::idtx_input_phase_from(self.id.0, self.source as u32) })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            InputPhase::None
        }
    }

    /// `true` on the frame the action became active.
    #[must_use]
    pub fn pressed(self) -> bool {
        matches!(self.phase(), InputPhase::Pressed)
    }

    /// `true` while the action is active (pressed or held).
    #[must_use]
    pub fn held(self) -> bool {
        matches!(self.phase(), InputPhase::Pressed | InputPhase::Held)
    }

    /// `true` on the frame the action became inactive.
    #[must_use]
    pub fn released(self) -> bool {
        matches!(self.phase(), InputPhase::Released)
    }

    /// The scalar value of an axis action `[-1, 1]`.
    #[must_use]
    pub fn axis(self) -> f32 {
        #[cfg(target_arch = "wasm32")]
        {
            unsafe { crate::__rt::idtx_input_axis_from(self.id.0, self.source as u32) }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            0.0
        }
    }

    /// The 2D value `(x, y)` of an `axis2` action (e.g. a look/stick vector).
    #[must_use]
    pub fn axis2(self) -> (f32, f32) {
        #[cfg(target_arch = "wasm32")]
        {
            let mut buf = [0u8; 8];
            // The host writes two little-endian f32s; a non-zero return code
            // (unresolved/unbound) leaves the zeroed buffer, so `(0, 0)`.
            unsafe { crate::__rt::idtx_input_axis2_from(self.id.0, self.source as u32, buf.as_mut_ptr()) };
            let x = f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            let y = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
            (x, y)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            (0.0, 0.0)
        }
    }
}

/// Persistent modal ownership. Acquire from `on_input_capture(&mut self, ctx)`
/// so requests arbitrate together before consumers run. The opening button must
/// be released before it can confirm. Drop/release on close; the host also
/// releases on detach, target removal and tracking loss.
#[derive(Debug, Default)]
pub struct CaptureToken(u64);

impl CaptureToken {
    pub fn acquire(player: crate::entity::Entity, target: crate::entity::Entity, source: PointerSource,
        priority: i32, actions: &[Action]) -> crate::result::Result<Self> {
        if source == PointerSource::Unspecified || actions.is_empty() || actions.len() > 32 {
            return Err(crate::result::BehaviorError(crate::abi::ErrorCode::InvalidArgument));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut ids = [0u32; 32];
            for (i, action) in actions.iter().enumerate() { ids[i] = action.id.0; }
            let token = unsafe { crate::__rt::idtx_input_capture(player.wire().to_bits(), target.wire().to_bits(), source as u32,
                priority, ids.as_ptr(), actions.len() as u32) };
            if token == 0 { return Err(crate::result::BehaviorError(crate::abi::ErrorCode::InvalidArgument)); }
            Ok(Self(token))
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = (player, target, priority); Ok(Self(0)) }
    }

    pub fn active(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        return self.0 != 0 && unsafe { crate::__rt::idtx_input_capture_active(self.0) != 0 };
        #[cfg(not(target_arch = "wasm32"))]
        false
    }

    pub fn release(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if self.0 != 0 { unsafe { crate::__rt::idtx_input_capture_release(self.0) }; }
        self.0 = 0;
    }
}

impl Drop for CaptureToken { fn drop(&mut self) { self.release(); } }

/// Convenience: resolve + read a scalar axis in one call.
#[must_use]
pub fn axis(name: &str) -> f32 {
    action(name).axis()
}

/// Convenience: resolve + read a 2D `axis2` action in one call.
///
/// Cache the [`Action`] (via [`action`]) instead when reading every frame to
/// avoid re-resolving the name.
#[must_use]
pub fn axis2(name: &str) -> (f32, f32) {
    action(name).axis2()
}

/// Current mouse delta `(dx, dy)` in pixels this frame.
#[must_use]
pub fn mouse_delta() -> (f32, f32) {
    #[cfg(target_arch = "wasm32")]
    {
        let mut buf = [0u8; 8];
        unsafe { crate::__rt::idtx_input_mouse_delta(buf.as_mut_ptr()) };
        let dx = f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let dy = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        (dx, dy)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        (0.0, 0.0)
    }
}

/// Scroll-wheel delta this frame.
#[must_use]
pub fn scroll() -> f32 {
    #[cfg(target_arch = "wasm32")]
    {
        unsafe { crate::__rt::idtx_input_scroll() }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Natively (host-side unit tests) every input read returns the inactive /
    // zero default so author logic is exercisable without a wasm host. These
    // guard that contract for the accessors behaviors depend on.

    #[test]
    fn native_reads_are_inert_defaults() {
        assert_eq!(axis("move_forward"), 0.0);
        assert_eq!(axis2("look"), (0.0, 0.0));
        assert_eq!(mouse_delta(), (0.0, 0.0));
        assert_eq!(scroll(), 0.0);
        assert_eq!(action("jump").id(), InputActionId::UNRESOLVED);
        assert!(!action("jump").held());
    }
}
