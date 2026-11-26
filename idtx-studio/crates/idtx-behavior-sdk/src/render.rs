//! Frame-local render overrides (`overlay` capability). Never writes USD.
use crate::{abi::ErrorCode, entity::Entity, result::{Result, check}};
pub use crate::abi::PointInstanceOpacity;

#[derive(Clone, Copy, Debug)]
pub struct Render { _private: () }

impl Render {
    pub(crate) fn current() -> Self { Self { _private: () } }

    /// Override presence coverage for native ids for this frame. Post again on
    /// paused/zero-delta frames to retain the visual state. Missing ids revert to
    /// authored `primvars:displayOpacity` (or 1 when absent); native masks still
    /// apply. Use presence-opacity prototype materials for emissive fading.
    /// At most 4096 unique, existing ids, also bounded by the argument budget.
    pub fn set_point_instance_opacities(self, entity: Entity, values: &[PointInstanceOpacity]) -> Result<()> {
        if values.len() > crate::abi::MAX_POINT_INSTANCES || values.iter().any(|v| !v.is_valid()) {
            return Err(ErrorCode::InvalidArgument.into());
        }
        #[cfg(target_arch = "wasm32")]
        let code = ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_point_instance_opacities(entity.wire().to_bits(), values.as_ptr().cast(), values.len() as u32)
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = { let _ = entity; ErrorCode::Ok };
        check(code)
    }
}
