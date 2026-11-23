//! Clip playback transport (`playback` capability).
//!
//! A behavior granted `playback` may drive a named animation clip's playhead —
//! the `AnimClipPlayer`/`ClipPlayhead` the session hydrates from the animation
//! catalog. Guests do **not** mutate playback on the hot path: each call here
//! *enqueues* a deferred [`crate::abi::PlaybackCommandKind`] command that the
//! host drains after the update hooks and the session applies to the addressed
//! playhead (advanced + resampled on the next animation tick — a one-frame
//! settle, exactly like authored transport).
//!
//! A clip is addressed by `(target entity, label)`. Use [`Animations::clip`] to
//! address a clip on the caller's own binding (the host resolves the `NULL`
//! target to the posting attachment at drain), or [`Animations::clip_on`]
//! to address a clip on another entity. The label is the authored clip name
//! (`"open"`) or, for an anonymous discovered group, the owner prim path.
//! Stage-scoped behaviors should resolve their actor relationship and use
//! `clip_on`; playback does not implicitly search their level's descendants.
//!
//! On wasm the calls route to the capability-gated `idtx_playback_command` host
//! import (absent — and therefore link-failing — without the grant); natively
//! they are inert `Ok(())` so a behavior using playback stays unit-testable
//! without a host.

use crate::abi::{Entity as WireEntity, PlaybackCommandKind};
use crate::entity::Entity;
use crate::result::{Result, check};

/// Playback-transport surface. Obtain one from [`crate::ctx::Ctx::animations`].
/// Cheap to copy — holds no state.
#[derive(Clone, Copy, Debug)]
pub struct Animations {
    _private: (),
}

impl Animations {
    /// Construct the handle for the current hook. Cheap; holds no state.
    #[must_use]
    pub(crate) fn current() -> Self {
        Self { _private: () }
    }

    /// Address a clip on the caller's **own** binding. The host resolves the
    /// `NULL` target to the posting attachment at drain time. Use `clip_on` for
    /// an actor controlled by a stage-scoped behavior.
    #[must_use]
    pub fn clip<'a>(&self, label: &'a str) -> Clip<'a> {
        Clip {
            target: WireEntity::NULL,
            label,
        }
    }

    /// Address a clip on a specific `entity`.
    #[must_use]
    pub fn clip_on<'a>(&self, entity: Entity, label: &'a str) -> Clip<'a> {
        Clip {
            target: entity.wire(),
            label,
        }
    }
}

/// A single addressed clip playhead, built by [`Animations::clip`] /
/// [`Animations::clip_on`]. Each transport method posts one deferred command.
#[derive(Clone, Copy, Debug)]
pub struct Clip<'a> {
    target: WireEntity,
    label: &'a str,
}

/// Durable snapshot after deferred commands have settled. String capacities are
/// explicit; callers can instead use `read_status` for their own JSON contract.
#[derive(Debug, serde::Deserialize)]
pub struct PlaybackStatus {
    pub owner: crate::context::ContextString<1024>,
    pub clip: crate::context::ContextString<256>,
    pub channel: crate::context::ContextString<256>,
    pub source: crate::context::ContextString<1024>,
    pub state: crate::context::ContextString<32>,
    pub error: crate::context::ContextString<2048>,
    pub current: f64,
    pub start: f64,
    pub end: f64,
    pub time_codes_per_second: f64,
    pub speed: f64,
    pub looping: bool,
    pub independent: bool,
    pub run_id: u64,
    pub last_command_sequence: u64,
    pub last_command_applied: Option<bool>,
    pub last_command_error: crate::context::ContextString<2048>,
}

impl Clip<'_> {
    /// Release this clip's ownership and restore its containing timeline.
    pub fn release(&self) -> Result<()> { self.post(PlaybackCommandKind::Release, 0.0) }

    /// Read a typed, allocation-free snapshot. Requires `playback` capability.
    pub fn status(&self, scratch: &mut [u8]) -> Result<PlaybackStatus> {
        let n = self.read_status(scratch)?;
        crate::context::decode(&scratch[..n])
    }

    pub fn read_status(&self, scratch: &mut [u8]) -> Result<usize> {
        if scratch.len() > crate::abi::MAX_BEHAVIOR_CONTEXT_BYTES || self.label.is_empty() || self.label.len() > 256 {
            return Err(crate::abi::ErrorCode::InvalidArgument.into());
        }
        #[allow(unused_mut)]
        let mut written = 0u32;
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_playback_status(self.target.to_bits(), self.label.as_ptr(), self.label.len() as u32,
                scratch.as_mut_ptr(), scratch.len() as u32, &mut written)
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = crate::abi::ErrorCode::NotFound;
        check(code)?;
        if written as usize > scratch.len() { return Err(crate::abi::ErrorCode::InvalidArgument.into()); }
        Ok(written as usize)
    }
    /// Start (or resume) the playhead. A finished non-looping clip restarts.
    pub fn play(&self) -> Result<()> {
        self.post(PlaybackCommandKind::Play, 0.0)
    }

    /// Pause the playhead in place.
    pub fn pause(&self) -> Result<()> {
        self.post(PlaybackCommandKind::Pause, 0.0)
    }

    /// Stop and rewind the playhead to its start pose.
    pub fn stop(&self) -> Result<()> {
        self.post(PlaybackCommandKind::Stop, 0.0)
    }

    /// Move the playhead to `time_code`, clamped to the clip range. The wire
    /// value is `f32`; sub-`f32` time-code precision is not preserved.
    pub fn seek(&self, time_code: f64) -> Result<()> {
        self.post(PlaybackCommandKind::Seek, time_code as f32)
    }

    /// Set the playback speed multiplier (negative plays in reverse).
    pub fn set_speed(&self, speed: f32) -> Result<()> {
        self.post(PlaybackCommandKind::SetSpeed, speed)
    }

    /// Enable or disable looping.
    pub fn set_looping(&self, looping: bool) -> Result<()> {
        self.post(PlaybackCommandKind::SetLooping, if looping { 1.0 } else { 0.0 })
    }

    fn post(&self, kind: PlaybackCommandKind, value: f32) -> Result<()> {
        if self.label.is_empty() || self.label.len() > 256 || !value.is_finite() {
            return Err(crate::abi::ErrorCode::InvalidArgument.into());
        }
        #[cfg(target_arch = "wasm32")]
        let code = crate::abi::ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_playback_command(
                self.target.to_bits(),
                self.label.as_ptr(),
                self.label.len() as u32,
                kind.as_u32(),
                value,
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (self.target, kind, value);
            crate::abi::ErrorCode::Ok
        };
        check(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Native stubs are inert `Ok(())` so author logic stays unit-testable
    // without a wasm host (mirrors the level / stage_edit native-stub tests).
    #[test]
    fn transport_native_stubs_are_inert() {
        let a = Animations::current();
        let c = a.clip("open");
        assert!(c.play().is_ok());
        assert!(c.pause().is_ok());
        assert!(c.stop().is_ok());
        assert!(c.seek(12.0).is_ok());
        assert!(c.set_speed(-1.0).is_ok());
        assert!(c.set_looping(true).is_ok());
        assert!(c.release().is_ok());
    }

    #[test]
    fn invalid_transport_values_are_rejected_before_the_import() {
        let a = Animations::current();
        assert!(a.clip("").play().is_err());
        assert!(a.clip("idle").seek(f64::NAN).is_err());
        assert!(a.clip("idle").seek(f64::MAX).is_err());
        assert!(a.clip("idle").set_speed(f32::INFINITY).is_err());
        assert!(a.clip("idle").status(&mut [0; 8192]).is_err());
    }
}
