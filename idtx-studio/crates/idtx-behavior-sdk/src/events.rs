//! Named-event delivery (`events` capability).
//!
//! Inside `on_event`, `ctx.event()` returns a handle to the event
//! currently being delivered. Its name and payload are read into
//! caller-supplied buffers (this crate is `#![no_std]` with no allocator,
//! so the guest owns the storage); the true length is returned so a guest
//! can detect truncation.
//!
//! On wasm the reads route to the capability-gated `idtx_event_current_*`
//! host imports; natively they return an empty/null event so a behavior
//! using events stays unit-testable without a host.

use crate::abi::Entity as WireEntity;
use crate::entity::Entity;

/// Handle to the event currently being delivered to `on_event`.
///
/// Requires the behavior's manifest to grant the `events` capability
/// (otherwise the host imports are unlinked and the module fails to load).
/// Valid only for the duration of the `on_event` hook — the host clears
/// the current event once the hook returns.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    _private: (),
}

impl Event {
    /// Construct the handle for the current hook. Cheap; holds no state.
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// The event's target entity — the entity `on_event` is firing for
    /// ([`Entity::is_null`] for a broadcast, or outside `on_event`).
    #[must_use]
    pub fn target(self) -> Entity {
        #[cfg(target_arch = "wasm32")]
        let bits = unsafe { crate::__rt::idtx_event_current_target() };
        #[cfg(not(target_arch = "wasm32"))]
        let bits = 0u64;
        Entity::from_wire(WireEntity::from_bits(bits))
    }

    /// Copy the event name into `buf`, returning the name's *true* byte
    /// length. At most `buf.len()` bytes are written; a return value
    /// greater than `buf.len()` means the name was truncated.
    #[must_use]
    pub fn read_name(self, buf: &mut [u8]) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            unsafe { crate::__rt::idtx_event_current_name(buf.as_mut_ptr(), cap) as usize }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = buf;
            0
        }
    }

    /// Copy the event payload into `buf`, returning the payload's *true*
    /// byte length. At most `buf.len()` bytes are written; a return value
    /// greater than `buf.len()` means the payload was truncated.
    #[must_use]
    pub fn read_payload(self, buf: &mut [u8]) -> usize {
        #[cfg(target_arch = "wasm32")]
        {
            let cap = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            unsafe { crate::__rt::idtx_event_current_payload(buf.as_mut_ptr(), cap) as usize }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = buf;
            0
        }
    }
}
