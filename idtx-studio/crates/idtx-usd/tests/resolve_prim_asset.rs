//! `StageService::resolve_prim_asset` — resolve an `asset`-typed attribute to an
//! absolute path via USD's own asset resolution, anchored to the *authoring*
//! layer.
//!
//! This backs the Conductor's behavior source/module split: a behavior prim
//! authors `idtx:behavior:source = @./behaviors/<x>.rs@` on a **level** layer.
//! When packaging opens the composed **assembly** (which payloads the level from
//! the project root), the relative source path must still resolve next to the
//! level that authored it — never re-anchored to the assembly's own directory.
//! A plain relative join against the entry stage's dir would diverge between the
//! direct-open (preview) and composed (packaging) flows; this FFI does not.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`).

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::StageService;

static NEXT: AtomicU64 = AtomicU64::new(1);

const SOURCE_ATTR: &str = "idtx:behavior:source";

/// A hermetic project laid out on disk:
/// ```text
/// <root>/assembly.usda            (payloads the level under /World/Levels/lvl)
/// <root>/stages/level.usda        (defaultPrim World; /World/Cylinder authors the source)
/// <root>/stages/behaviors/spin.rs (the resolved target)
/// ```
struct Fixture {
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "idtx-resolve-asset-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("stages/behaviors")).unwrap();

        // The `.rs` must exist so USD's resolver returns a non-empty resolved
        // path (exercises the primary `GetResolvedPath` branch, not the anchor
        // fallback).
        std::fs::write(
            root.join("stages/behaviors/spin.rs"),
            b"// behavior source\n",
        )
        .unwrap();

        // Level layer authors the source relative to itself (`stages/`).
        std::fs::write(
            root.join("stages/level.usda"),
            "#usda 1.0\n\
             (\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n)\n\n\
             def Xform \"World\"\n{\n\
             \x20   def Cylinder \"Cylinder\"\n    {\n\
             \x20       custom asset idtx:behavior:source = @./behaviors/spin.rs@\n\
             \x20   }\n}\n",
        )
        .unwrap();

        // Assembly at the project root payloads the level under a holder scope,
        // mirroring `assembly.rs::author_assembly_stage`.
        std::fs::write(
            root.join("assembly.usda"),
            "#usda 1.0\n\
             (\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n)\n\n\
             def Xform \"World\"\n{\n\
             \x20   def Scope \"Levels\"\n    {\n\
             \x20       def \"lvl\" (\n            prepend payload = @stages/level.usda@\n        )\n\
             \x20       {\n        }\n    }\n}\n",
        )
        .unwrap();

        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn resolves_composed_prim_asset_against_authoring_layer() {
    let fx = Fixture::new();

    // Open the ASSEMBLY (payloads loaded), reach the composed prim, resolve.
    let stage = StageService::open(fx.root.join("assembly.usda")).expect("open assembly");
    let prim = stage
        .prim_at_path("/World/Levels/lvl/Cylinder")
        .expect("composed cylinder prim exists (payload loaded)");
    let resolved = stage
        .resolve_prim_asset(prim, SOURCE_ATTR)
        .expect("resolve call")
        .expect("asset resolves to a concrete path");

    let resolved = resolved.replace('\\', "/");
    assert!(
        resolved.ends_with("stages/behaviors/spin.rs"),
        "composed-prim source must anchor to the LEVEL layer (stages/), not the \
         assembly root; got {resolved}"
    );
    assert!(
        std::path::Path::new(&resolved).is_file(),
        "resolved path must point at the on-disk source: {resolved}"
    );
}

#[test]
fn direct_open_resolves_to_the_same_file_as_composed() {
    // Preview opens the level directly; packaging opens it through the assembly.
    // Both must resolve `@./behaviors/spin.rs@` to the same on-disk file.
    let fx = Fixture::new();

    let level = StageService::open(fx.root.join("stages/level.usda")).expect("open level");
    let prim = level
        .prim_at_path("/World/Cylinder")
        .expect("cylinder prim exists");
    let direct = level
        .resolve_prim_asset(prim, SOURCE_ATTR)
        .expect("resolve call")
        .expect("asset resolves")
        .replace('\\', "/");

    let asm = StageService::open(fx.root.join("assembly.usda")).expect("open assembly");
    let cprim = asm
        .prim_at_path("/World/Levels/lvl/Cylinder")
        .expect("composed cylinder prim exists");
    let composed = asm
        .resolve_prim_asset(cprim, SOURCE_ATTR)
        .expect("resolve call")
        .expect("asset resolves")
        .replace('\\', "/");

    assert_eq!(
        direct, composed,
        "preview (direct) and packaging (composed) must resolve the source \
         identically"
    );
}

#[test]
fn absent_attribute_resolves_to_none() {
    let fx = Fixture::new();
    let stage = StageService::open(fx.root.join("stages/level.usda")).expect("open level");
    let prim = stage.prim_at_path("/World").expect("world prim exists");
    let resolved = stage
        .resolve_prim_asset(prim, SOURCE_ATTR)
        .expect("resolve call");
    assert!(resolved.is_none(), "no authored source ⇒ None");
}
