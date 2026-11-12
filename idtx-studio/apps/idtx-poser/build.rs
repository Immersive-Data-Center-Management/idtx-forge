//! Build script for the `idtx-poser` binary.
//!
//! `idtx-poser` is one of three thin "flavor" binaries that wrap
//! the shared `idtx-studio-shell` crate behind a runtime `ProductProfile`.
//! All three need the identical post-build side effect: stage `OpenUSD`'s `usd_ms`
//! dynamic library + resource trees next to the produced binary so `cargo run`
//! Just Works™.
//!
//! Because Cargo's `rustc-link-arg-bins` directive and the OUT_DIR-relative
//! dylib copy are both *per-binary-crate* concerns, the logic cannot live in
//! a normal library module — it has to run in each binary crate's own build
//! script. To avoid triplicating it, the single source of truth lives in
//! `crates/idtx-usd-sys/build_support/usd_runtime.rs`; every flavor's
//! `build.rs` `#[path]`-includes it and calls `stage_usd_runtime()`.

#[path = "../../crates/idtx-usd-sys/build_support/usd_runtime.rs"]
mod usd_runtime;

fn main() {
    usd_runtime::stage_usd_runtime();
    embed_windows_icon();
}

/// Embed the crate's application icon into the produced `.exe` on Windows.
///
/// The icon lives next to this build script (`idtx_poser_64.ico`). On
/// non-Windows targets `winresource` is a no-op, so this compiles and links
/// everywhere; we simply skip the work when not targeting Windows.
fn embed_windows_icon() {
    // Rebuild if the icon changes.
    println!("cargo:rerun-if-changed=idtx_poser_64.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("idtx_poser_64.ico");
        res.compile()
            .expect("failed to embed idtx_poser_64.ico into the executable");
    }
}
