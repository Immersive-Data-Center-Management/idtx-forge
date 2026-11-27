//! Arc-distance queries over native USD BasisCurves, without live USD calls.
//! Length and input distances are always normalized stage-world metres (Y-up),
//! including full composed transforms and stage units. Local output retains
//! authored units. Distances clamp to endpoints; periodic looping is explicit.
pub use crate::abi::CurveSample;
use crate::{
    abi::ErrorCode,
    entity::Entity,
    result::{Result, check},
};

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveSpace {
    Local = 0,
    StageWorldMetres = 1,
}

/// An entity-generation and batch-index handle, not a frozen copy of geometry.
/// Changes are visible on the next snapshot; removal returns StaleHandle.
#[derive(Debug, Clone, Copy)]
pub struct Curve {
    entity: Entity,
    index: u32,
}

impl Curve {
    pub(crate) fn new(entity: Entity, index: u32) -> Result<Self> {
        let curve = Self {
            entity,
            index,
        };
        curve.length()?;
        Ok(curve)
    }

    /// Total arc length in metres after nonuniform scaling and unit conversion.
    pub fn length(self) -> Result<f64> {
        #[allow(unused_mut)]
        let mut result = 0.0f64;
        #[cfg(target_arch = "wasm32")]
        let code = ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_curve_length(
                self.entity.wire().to_bits(),
                self.index,
                (&mut result as *mut f64).cast(),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (self.entity, self.index);
            ErrorCode::NotFound
        };
        check(code)?;
        Ok(result)
    }

    pub fn sample_distance(self, distance: f64, output_space: CurveSpace) -> Result<CurveSample> {
        let mut out = [CurveSample::default()];
        self.sample_distances(&[distance], output_space, &mut out)?;
        Ok(out[0])
    }

    /// Allocation-free caller-buffer API, at most 256 distances per call. A
    /// short buffer, NaN, infinity, or oversized request fails without writing.
    /// Unsupported native curve bases/types report TypeMismatch.
    pub fn sample_distances(
        self,
        distances: &[f64],
        output_space: CurveSpace,
        out: &mut [CurveSample],
    ) -> Result<()> {
        if distances.len() > crate::abi::MAX_CURVE_QUERY_SAMPLES
            || distances.len() > out.len()
            || distances.iter().any(|d| !d.is_finite())
        {
            return Err(ErrorCode::InvalidArgument.into());
        }
        #[cfg(target_arch = "wasm32")]
        let code = ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_curve_samples(
                self.entity.wire().to_bits(),
                self.index,
                output_space as u32,
                distances.as_ptr().cast(),
                distances.len() as u32,
                out.as_mut_ptr().cast(),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (self, output_space, out);
            ErrorCode::NotFound
        };
        check(code)
    }
}
