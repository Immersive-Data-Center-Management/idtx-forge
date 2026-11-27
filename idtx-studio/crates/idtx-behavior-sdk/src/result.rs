//! Guest-facing error type + `Result` alias.

use crate::abi::ErrorCode;

/// Error returned by the safe guest wrappers.
///
/// A thin newtype over the wire [`ErrorCode`] so author code reads
/// `Result<T>` and can `?` naturally.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BehaviorError(pub ErrorCode);

impl BehaviorError {
    /// The underlying wire code.
    #[must_use]
    pub const fn code(self) -> ErrorCode {
        self.0
    }
}

impl From<ErrorCode> for BehaviorError {
    fn from(code: ErrorCode) -> Self {
        Self(code)
    }
}

impl core::fmt::Display for BehaviorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "behavior error: {:?}", self.0)
    }
}

/// Result alias author hooks return.
pub type Result<T> = core::result::Result<T, BehaviorError>;

/// Convert a raw wire `ErrorCode` into `Result<()>` — `Ok` maps to
/// `Ok(())`, everything else to `Err`.
#[must_use = "handle the error code"]
pub fn check(code: ErrorCode) -> Result<()> {
    if code.is_ok() {
        Ok(())
    } else {
        Err(BehaviorError(code))
    }
}
