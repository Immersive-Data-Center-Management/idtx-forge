//! Build script for `xtask`.
//!
//! The `visionos-device` feature links the `OpenUSD` monolith (transitively via
//! `idtx-build` → `idtx-usd` → `idtx-usd-sys`, whose cxx bridge links
//! `libusd_ms.dylib`). Without help, running the resulting `xtask` binary aborts
//! at load with `Library not loaded: @rpath/libusd_ms.dylib` unless the caller
//! exports `DYLD_LIBRARY_PATH`.
//!
//! To make `cargo xtask-visionos run-visionos-device` just work, we bake an
//! absolute **rpath** to the `OpenUSD` install's `lib/` dir into the binary when the
//! feature is enabled, so it self-locates the dylib at runtime. macOS only; a
//! no-op when the feature is off or `IDTX_USD_DIR` is unset (then the established
//! `DYLD_LIBRARY_PATH` convention still applies).

fn main() {
    // Only the `visionos-device` feature pulls OpenUSD into this binary.
    if std::env::var_os("CARGO_FEATURE_VISIONOS_DEVICE").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
    {
        if let Some(usd) = std::env::var_os("IDTX_USD_DIR") {
            let lib = std::path::Path::new(&usd).join("lib");
            if lib.is_dir() {
                println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
            }
        }
    }
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR");
}
