use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Copies the schema plugin resources (`plugInfo.json` + `generatedSchema.usda`)
/// into `OUT_DIR/idtx-usd-extension/` so the registered plugin directory holds
/// the `plugInfo.json` and its sibling `generatedSchema.usda` together — the
/// layout `OpenUSD`'s `PlugRegistry::RegisterPlugins` expects (`Root`/`ResourcePath`
/// in `plugInfo.json` are relative to the `plugInfo.json`'s own directory).
///
/// The directory is baked into the crate via `IDTX_USD_EXTENSION_PLUGIN_DIR` so
/// `cargo run` / tests discover it with no build-machine paths hard-coded in
/// source. A deployed `.app` bundle is handled at runtime by `plugin_dir()`.
///
/// Unlike the HTTP resolver crate this compiles **no** C++ — the schema is
/// codeless, so there is nothing to link.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=resources/plugInfo.json");
    println!("cargo:rerun-if-changed=resources/generatedSchema.usda");

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    let plugin_dir = out_dir.join("idtx-usd-extension");
    fs::create_dir_all(&plugin_dir).expect("create plugin dir in OUT_DIR");

    // `generatedSchema.usda` is copied verbatim, next to `plugInfo.json`.
    copy_resource(
        &manifest.join("resources").join("generatedSchema.usda"),
        &plugin_dir.join("generatedSchema.usda"),
    );

    // `plugInfo.json` ships with `@PLUG_INFO_*@` path placeholders so the source
    // stays layout-agnostic; substitute them for the FLAT baked layout here. Both
    // files are siblings in `plugin_dir`, so OpenUSD must resolve the schema layer
    // *next to* `plugInfo.json` (`Root`/`ResourcePath` are relative to the
    // `plugInfo.json`'s own directory). `Root="."` + `ResourcePath="."` points at
    // that same directory; without this OpenUSD looks under a nonexistent
    // `<dir>/../resources/` and warns ("Failed to open schema layer …") on every
    // stage open. The schema is codeless, so there is no shared library →
    // `LibraryPath` is empty.
    let plug_info_src = manifest.join("resources").join("plugInfo.json");
    let plug_info = fs::read_to_string(&plug_info_src)
        .unwrap_or_else(|e| panic!("read {}: {e}", plug_info_src.display()))
        .replace("@PLUG_INFO_ROOT@", ".")
        .replace("@PLUG_INFO_RESOURCE_PATH@", ".")
        .replace("@PLUG_INFO_LIBRARY_PATH@", "");
    fs::write(plugin_dir.join("plugInfo.json"), plug_info)
        .unwrap_or_else(|e| panic!("write plugInfo.json to {}: {e}", plugin_dir.display()));

    println!(
        "cargo:rustc-env=IDTX_USD_EXTENSION_PLUGIN_DIR={}",
        plugin_dir.display()
    );
}

fn copy_resource(src: &Path, dst: &Path) {
    fs::copy(src, dst)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), dst.display()));
}
