//! The sole unsafe/C++ interoperability boundary for IDTX Studio.

pub mod bridge;

/// Build-time resolved runtime layout for the pinned `OpenUSD` install.
pub mod layout {
    include!(concat!(env!("OUT_DIR"), "/layout.rs"));
}

/// # Panics
///
/// This function panics if the target buffer can't hold as many items as we try to copy.
///
/// Layout-asserted bulk copy of a packed `[f32; 3]` slice into `glam::Vec3` values.
///
/// Uses a single `copy_nonoverlapping` — no per-element conversion overhead.
/// Relies on `glam =0.33.0` `Vec3` being `repr(C)` with layout `[f32; 3]`.
/// All safety invariants are internally enforced: overlap is impossible via borrow rules,
/// size is asserted, and layout compatibility is guaranteed by the glam version pin.
///
pub fn vec3f_bulk_copy(src: &[[f32; 3]], dst: &mut [glam::Vec3]) {
    assert!(
        dst.len() >= src.len(),
        "vec3f_bulk_copy: dst too small ({} < {})",
        dst.len(),
        src.len()
    );
    // SAFETY: no overlap (borrow rules), size asserted above, layout identical per glam =0.33.0 pin.
    unsafe {
        std::ptr::copy_nonoverlapping(
            src.as_ptr().cast::<glam::Vec3>(),
            dst.as_mut_ptr(),
            src.len(),
        );
    }
}
