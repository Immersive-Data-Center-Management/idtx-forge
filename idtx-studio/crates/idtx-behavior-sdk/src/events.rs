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

/// Post opaque bytes to an explicit live target (`events` capability).
/// Never calls another behavior recursively. Update/late-update posts are
/// collected in attachment order, FIFO per sender, then delivered before the
/// session applies deferred edits. Widget/event callback posts wait for the
/// next collection sweep. Under overload delivery continues across frames in
/// the host's bounded work budget. Success means queued, not acknowledged;
/// targets removed/unloaded before delivery receive nothing.
pub fn post(target: Entity, name: &str, payload: &[u8]) -> crate::result::Result<()> {
    if target.is_null() {
        return Err(crate::abi::ErrorCode::InvalidArgument.into());
    }
    send(target, name, payload)
}

/// Deliberately broadcast to all attached behaviors. Prefer [`post`] for
/// reusable referenced content, resolving its target from a USD relationship.
pub fn broadcast(name: &str, payload: &[u8]) -> crate::result::Result<()> {
    send(Entity::from_wire(WireEntity::NULL), name, payload)
}

fn send(target: Entity, name: &str, payload: &[u8]) -> crate::result::Result<()> {
    use crate::abi::{ErrorCode, MAX_EVENT_NAME_BYTES, MAX_EVENT_PAYLOAD_BYTES};
    if name.is_empty() || name.len() > MAX_EVENT_NAME_BYTES || payload.len() > MAX_EVENT_PAYLOAD_BYTES {
        return Err(ErrorCode::InvalidArgument.into());
    }
    #[cfg(target_arch = "wasm32")]
    let code = ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_event_post(target.wire().to_bits(), name.as_ptr(), name.len() as u32,
            payload.as_ptr(), payload.len() as u32)
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = { let _ = target; ErrorCode::NotFound };
    crate::result::check(code)
}

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
