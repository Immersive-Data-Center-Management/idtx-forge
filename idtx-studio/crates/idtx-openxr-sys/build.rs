//! Build script: emits the `idtx_xr` cfg alias.
//!
//! `idtx_xr` is true exactly when a real `OpenXR` *graphics* session is compiled
//! in: the Windows PCVR/Direct3D12 path (`pcvr` + `target_os = "windows"`) or
//! the Quest/Android Vulkan path (`quest` + `target_os = "android"`). Within
//! `idtx_xr` code, `cfg(windows)` selects the D3D12 backend and
//! `cfg(target_os = "android")` selects the Vulkan backend, so this single
//! alias replaces the repeated `all(feature = "pcvr", windows)` gates and lets
//! the new Vulkan arms share them: `all(idtx_xr, windows)` is identical to the
//! old `all(feature = "pcvr", windows)` because `quest && android` is false on
//! Windows.
//!
//! Hand-rolled rather than pulling the `cfg_aliases` crate so no new
//! build-dependency enters the `cargo deny` surface; `build.rs` is already an
//! established pattern across the workspace.
//!
//! The predicate below is a pure fn so its truth table is reviewable; it is not
//! unit-tested here because build scripts do not run under `cargo test` (TMDD:
//! unit test intentionally skipped — trivial build-script predicate).

/// Whether a real XR graphics session compiles for this (features, target) pair.
fn xr_enabled(pcvr: bool, quest: bool, target_os: &str) -> bool {
    (pcvr && target_os == "windows") || (quest && target_os == "android")
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(idtx_xr)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let pcvr = std::env::var_os("CARGO_FEATURE_PCVR").is_some();
    let quest = std::env::var_os("CARGO_FEATURE_QUEST").is_some();
    if xr_enabled(pcvr, quest, &target_os) {
        println!("cargo::rustc-cfg=idtx_xr");
    }
}
