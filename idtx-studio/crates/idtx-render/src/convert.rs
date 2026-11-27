//! Centralised, auditable numeric conversions for `idtx-render`.
//!
//! Clippy's `cast_precision_loss`, `cast_possible_truncation`, and
//! `cast_sign_loss` fire on the `as` keyword itself, independent of any
//! surrounding `clamp`/`round` guard. Rather than scatter `#[allow(..)]` across
//! the hundreds of render-math cast sites, this module wraps each lossy
//! conversion once — behind a single audited `#[allow]` and a doc comment that
//! states the value-range contract — so the few allows are justified in one
//! place.
//!
//! Semantics deliberately match Rust's `as`:
//! * **int → float** widening is exact below the float mantissa limit (2^24 for
//!   `f32`, 2^53 for `f64`); render dimensions, element counts, indices, and
//!   loop counters are all far below those bounds.
//! * **float → int** ([`clamp_to_u8`], [`f32_to_u32`], [`floor_to_i32`],
//!   [`ceil_to_i32`]) truncates toward zero and saturates on overflow, with
//!   `NaN` mapping to `0` — it never panics.
//! * [`u32_sat`] (and, under the `metrics` feature, [`nanos_u64`]) saturate and
//!   `debug_assert!` instead of wrapping, turning the one genuinely dangerous
//!   narrowing cast into a debuggable path.
//!
//! Conversions that are *lossless* (e.g. `u8`/`u16` → `f32`, `u8`/`u16`/`u32`
//! → `f64`/`u64`, `bool` → `u64`) are intentionally **not** provided here —
//! use the std `From` impls (`f64::from(x)`, `u64::from(x)`, …) directly, which
//! is the existing workspace convention and keeps those sites free of any cast
//! lint.

/// Lossy widening of a wide integer — or an `f64` — to `f32`.
///
/// Exact for magnitudes below 2^24. Used for pixel dimensions, element counts,
/// indices, and loop counters, which never approach that bound. The `f64` impl
/// narrows the mantissa (USD camera parameters stored as `f64`, consumed as
/// `f32` by the GPU).
pub(crate) trait AsF32 {
    fn as_f32(self) -> f32;
}

/// Lossy widening of a 64-bit integer to `f64`.
///
/// Exact for magnitudes below 2^53. Used for byte counts and timing/aggregation
/// values, which never approach that bound. (`u32`/`i32` → `f64` is lossless —
/// use `f64::from` for those.)
pub(crate) trait AsF64 {
    fn as_f64(self) -> f64;
}

macro_rules! impl_as_f32 {
    ($($t:ty),* $(,)?) => {$(
        impl AsF32 for $t {
            #[inline]
            #[allow(clippy::cast_precision_loss)]
            fn as_f32(self) -> f32 {
                self as f32
            }
        }
    )*};
}

macro_rules! impl_as_f64 {
    ($($t:ty),* $(,)?) => {$(
        impl AsF64 for $t {
            #[inline]
            #[allow(clippy::cast_precision_loss)]
            fn as_f64(self) -> f64 {
                self as f64
            }
        }
    )*};
}

impl_as_f32!(u32, usize, u64, i32, i64);
impl_as_f64!(u64, usize, i64);

impl AsF32 for f64 {
    #[inline]
    // f64 -> f32 narrows the mantissa; USD camera params are small and finite.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn as_f32(self) -> f32 {
        self as f32
    }
}

/// Clamp a linear color channel to `0..=255` and truncate toward zero.
///
/// `NaN` maps to `0`. This guards only the range — callers that want rounding
/// pass an already-`.round()`ed value (e.g. `clamp_to_u8((v * 255.0).round())`).
#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn clamp_to_u8(x: f32) -> u8 {
    x.clamp(0.0, 255.0) as u8
}

/// Truncate a non-negative `f32` to `u32`, toward zero.
///
/// Saturates to `0` for negatives / `NaN` and to `u32::MAX` on overflow — the
/// same saturation Rust's `as` performs. Used for scaled pixel dimensions and
/// coordinates (callers apply any `.round()`/`.floor()` first).
#[inline]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn f32_to_u32(x: f32) -> u32 {
    x as u32
}

/// Floor an `f32` toward negative infinity, as `i32`.
///
/// Saturates on overflow; `NaN` maps to `0`. The caller guarantees finiteness
/// for meaningful results (UDIM tile math guards with `is_finite` upstream).
#[inline]
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn floor_to_i32(x: f32) -> i32 {
    x.floor() as i32
}

/// Ceil an `f32` toward positive infinity, as `i32`.
///
/// Saturates on overflow; `NaN` maps to `0`. UDIM UV-bounding-box math.
#[inline]
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn ceil_to_i32(x: f32) -> i32 {
    x.ceil() as i32
}

/// Narrow a `usize` count/length to `u32`, saturating at `u32::MAX`.
///
/// Vertex / index / instance counts never approach `u32::MAX`; this saturates
/// (and `debug_assert!`s) rather than silently *wrapping* if that invariant is
/// ever violated — the dangerous behavior of a plain `as u32`.
#[inline]
pub(crate) fn u32_sat(x: usize) -> u32 {
    if let Ok(v) = u32::try_from(x) { v } else {
        debug_assert!(false, "count {x} exceeds u32::MAX; saturating to u32::MAX");
        u32::MAX
    }
}

/// Narrow a `u128` nanosecond duration to `u64`, saturating.
///
/// `u64` nanoseconds span ~584 years, so this never saturates in practice; it
/// exists only to keep `Duration::as_nanos()` (which returns `u128`) lint-clean
/// on the metrics path.
#[cfg(feature = "metrics")]
#[inline]
pub(crate) fn nanos_u64(x: u128) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact-value assertions on known constants
mod tests {
    use super::*;

    #[test]
    fn as_f32_is_exact_for_small_values() {
        assert_eq!(1920_u32.as_f32(), 1920.0);
        assert_eq!(1080_usize.as_f32(), 1080.0);
        assert_eq!((-5_i32).as_f32(), -5.0);
        assert_eq!(16_777_216_u64.as_f32(), 16_777_216.0); // exactly 2^24
    }

    #[test]
    fn as_f64_is_exact_for_small_values() {
        assert_eq!(1_000_000_u64.as_f64(), 1_000_000.0);
        assert_eq!(4096_usize.as_f64(), 4096.0);
    }

    #[test]
    fn clamp_to_u8_guards_range_and_nan() {
        assert_eq!(clamp_to_u8(0.0), 0);
        assert_eq!(clamp_to_u8(127.9), 127); // truncates toward zero
        assert_eq!(clamp_to_u8(255.0), 255);
        assert_eq!(clamp_to_u8(-1.0), 0); // negative clamps to 0
        assert_eq!(clamp_to_u8(300.0), 255); // overflow clamps to 255
        assert_eq!(clamp_to_u8(f32::NAN), 0); // NaN maps to 0
    }

    #[test]
    fn f32_to_u32_saturates() {
        assert_eq!(f32_to_u32(3.9), 3);
        assert_eq!(f32_to_u32(-1.0), 0);
        assert_eq!(f32_to_u32(f32::NAN), 0);
        assert_eq!(f32_to_u32(f32::INFINITY), u32::MAX);
    }

    #[test]
    fn floor_and_ceil_to_i32() {
        assert_eq!(floor_to_i32(2.7), 2);
        assert_eq!(floor_to_i32(-0.1), -1);
        assert_eq!(ceil_to_i32(2.1), 3);
        assert_eq!(ceil_to_i32(-0.1), 0);
        assert_eq!(floor_to_i32(f32::NAN), 0);
    }

    #[test]
    fn u32_sat_passes_through_in_range() {
        assert_eq!(u32_sat(0), 0);
        assert_eq!(u32_sat(123_456), 123_456);
    }
}
