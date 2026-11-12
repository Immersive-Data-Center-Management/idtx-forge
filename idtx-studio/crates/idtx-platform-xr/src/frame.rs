//! Frame-timing contracts.
//!
//! This module defines the value types the begin/end-frame handshake against a
//! graphics-bound session produces, so downstream code can be written against
//! them.

/// A predicted display time, in nanoseconds.
///
/// It is modelled as a plain nanosecond newtype (rather than `openxr::Time`) to
/// keep the default (non-XR) build free of the `openxr` dependency. The `pcvr`
/// session path converts `openxr::Time` ↔ [`XrTime`] at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct XrTime(pub i64);

impl XrTime {
    /// Nanoseconds since the runtime epoch.
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }
}

/// The result of `xrWaitFrame`: when to render for, and whether the
/// runtime wants this frame rendered at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XrFrameState {
    /// The time the composited frame should be displayed.
    pub predicted_display_time: XrTime,
    /// `false` when the runtime asks the app to skip rendering this frame
    /// (e.g. the session is not visible).
    pub should_render: bool,
}
