//! Level transitions and bounded carried state (`level_control` capability).
//!
//! A behavior granted `level_control` may ask the runtime to travel to another
//! level. Guests do **not** swap stages on the hot path: each call here
//! *enqueues* a deferred request that the host surfaces after the update hooks
//! for the runtime driver to perform the actual session swap.
//!
//! On wasm the call routes to the capability-gated `idtx_request_level_transition`
//! host import (absent — and therefore link-failing — without the grant);
//! natively it is an inert `Ok(())` so a behavior using level transitions stays
//! unit-testable without a host.

use crate::result::{Result, check};

/// Deferred level-transition surface.
///
/// Requires the behavior's manifest to grant the `level_control` capability
/// (otherwise the host import is unlinked and the module fails to load). Obtain
/// one from [`crate::ctx::Ctx::level`]. Cheap to copy — holds no state; every
/// transition method posts a single deferred request; state access uses the
/// bounded store shared across the experience.
#[derive(Clone, Copy, Debug)]
pub struct Level {
    _private: (),
}

impl Level {
    /// Cancel an in-flight transition and return to the retained level. This is
    /// a bounded deferred flag; it never waits for a native open inside the hook.
    pub fn cancel(&self) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe { crate::__rt::idtx_cancel_level_transition() });
        #[cfg(not(target_arch = "wasm32"))]
        let code = crate::abi::ErrorCode::Ok;
        check(code)
    }
    /// Write experience state carried across levels. Keys are at most 128 UTF-8
    /// bytes, values at most 16 KiB; the shared store is capped at 64 KiB / 128 keys.
    pub fn set_state(&self, key: &str, value: &[u8]) -> Result<()> {
        if key.is_empty() || key.len() > 128 || value.len() > 16 * 1024 { return Err(crate::abi::ErrorCode::InvalidArgument.into()); }
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_state_set(key.as_ptr(), key.len() as u32, value.as_ptr(), value.len() as u32)
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = crate::abi::ErrorCode::Ok;
        check(code)
    }

    /// Read carried state into caller-owned storage; `None` means no such key.
    /// An undersized buffer returns `InvalidArgument`, without a partial value.
    pub fn read_state(&self, key: &str, out: &mut [u8]) -> Result<Option<usize>> {
        if key.is_empty() || key.len() > 128 { return Err(crate::abi::ErrorCode::InvalidArgument.into()); }
        #[cfg(target_arch = "wasm32")]
        {
            let mut length = 0;
            let code = crate::abi::ErrorCode::from_u32(unsafe {
                crate::__rt::idtx_state_get(key.as_ptr(), key.len() as u32, out.as_mut_ptr(), out.len() as u32, &mut length)
            });
            if code == crate::abi::ErrorCode::NotFound { return Ok(None); }
            check(code)?;
            Ok(Some(length as usize))
        }
        #[cfg(not(target_arch = "wasm32"))]
        { let _ = out; Ok(None) }
    }

    /// Construct the handle for the current hook. Cheap; holds no state.
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Request a transition to the level `target` (a level id, e.g. `"cave"`),
    /// spawning the player at that level's **default** spawn point.
    pub fn goto(&self, target: &str) -> Result<()> {
        Self::post(target, None)
    }

    /// Request a transition to the level `target`, spawning the player at the
    /// named spawn point `spawn` in that level.
    pub fn goto_with_spawn(&self, target: &str, spawn: &str) -> Result<()> {
        Self::post(target, Some(spawn))
    }

    fn post(target: &str, spawn: Option<&str>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        let code = {
            // A `None` spawn passes a null pointer + zero length — the host
            // reads that as "the target's default spawn".
            let (spawn_ptr, spawn_len) = match spawn {
                Some(s) => (s.as_ptr(), s.len() as u32),
                None => (core::ptr::null(), 0u32),
            };
            crate::abi::ErrorCode::from_u32(unsafe {
                crate::__rt::idtx_request_level_transition(
                    target.as_ptr(),
                    target.len() as u32,
                    spawn_ptr,
                    spawn_len,
                )
            })
        };
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (target, spawn);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The native stubs are inert `Ok(())` so author logic stays unit-testable
    // without a wasm host (mirrors the stage_edit / net native-stub tests).
    #[test]
    fn goto_native_stub_is_inert() {
        let level = Level::current();
        assert!(level.goto("cave").is_ok());
        assert!(level.cancel().is_ok());
        assert!(level.goto_with_spawn("summit", "north").is_ok());
        assert!(level.set_state("score", &[7]).is_ok());
        assert_eq!(level.read_state("score", &mut [0; 4]), Ok(None));
        assert!(level.set_state("", &[7]).is_err());
        assert!(level.set_state("score", &[0; 16 * 1024 + 1]).is_err());
    }
}
