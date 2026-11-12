//! Compute Graph canvas theme — node header accent tints by role.
//!
//! Distinguishes datasources, compute nodes and the terminal consumer at a
//! glance; passed through to the shared paint pass as `Node::accent_rgb`. The
//! actual colours live in the central [`crate::theme`] (`canvas` sub-palette)
//! so the whole editor has a single definition point.

/// Datasource (graph root) accent.
#[must_use]
pub fn accent_datasource() -> [u8; 3] {
    crate::theme::current().canvas.compute_datasource
}
/// Compute node accent.
#[must_use]
pub fn accent_compute() -> [u8; 3] {
    crate::theme::current().canvas.compute_compute
}
/// Terminal consumer prim accent.
#[must_use]
pub fn accent_consumer() -> [u8; 3] {
    crate::theme::current().canvas.compute_consumer
}
/// Anything else reached through a connection.
#[must_use]
pub fn accent_other() -> [u8; 3] {
    crate::theme::current().canvas.compute_other
}
