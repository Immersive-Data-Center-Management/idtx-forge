//! Build script for the `idtx-usd` crate.
//!
//! Resolves the `OpenUSD` install directory and, when its `lib` directory is
//! present, embeds that directory as a linker rpath so test and example
//! binaries locate the USD shared libraries (e.g. `libusd_ms`) at runtime
//! without the caller setting `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.
//!
//! The install directory is taken from `IDTX_USD_DIR` when set, otherwise it
//! defaults to `thirdparty/openusd-26.08-mtlx/install` relative to the
//! workspace root. If the resolved `lib` directory does not exist the script is
//! a no-op. The rpath is only emitted on macOS and Linux.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR");

    let usd_dir = if let Some(path) = env::var_os("IDTX_USD_DIR") {
        PathBuf::from(path)
    } else {
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root above crates/idtx-usd")
            .join("thirdparty/openusd-26.08-mtlx/install")
    };

    let lib = usd_dir.join("lib");
    if !lib.is_dir() {
        return;
    }

    // Embed the USD lib dir as an rpath so test binaries find libusd_ms at runtime
    // without requiring DYLD_LIBRARY_PATH / LD_LIBRARY_PATH to be set manually.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-rpath");
        println!("cargo:rustc-link-arg={}", lib.display());
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
    }
}
