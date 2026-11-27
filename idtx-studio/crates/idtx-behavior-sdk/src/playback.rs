//! Deferred transport commands (`playback` capability).
pub use crate::abi::PlaybackCommandKind;
use crate::{
    abi::ErrorCode,
    entity::Entity,
    result::{Result, check},
};

/// Queue a transport command; applied by the session after behavior dispatch.
/// An empty clip addresses the target's default playhead. NULL is resolved by
/// the session to the sender's own entity, following the existing host contract.
pub fn command(target: Entity, clip: &str, kind: PlaybackCommandKind, value: f32) -> Result<()> {
    if clip.len() > 256 || !value.is_finite() {
        return Err(ErrorCode::InvalidArgument.into());
    }
    #[cfg(target_arch = "wasm32")]
    let code = ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_playback_command(
            target.wire().to_bits(),
            clip.as_ptr(),
            clip.len() as u32,
            kind.as_u32(),
            value,
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (target, kind);
        ErrorCode::NotFound
    };
    check(code)
}
