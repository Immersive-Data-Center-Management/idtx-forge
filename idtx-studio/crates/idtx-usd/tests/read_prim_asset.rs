//! `StageService::read_asset` fed by `resolve_prim_asset` — the resolve→read
//! path the behavior runtime uses to load a module `.wasm` and its
//! `.rs` source **through USD's own asset resolution**, anchored to the
//! *authoring* layer.
//!
//! This is the mechanism behind `StudioSession`'s USD-primary behavior readers
//! (`read_asset_via_resolver`): a behavior prim carries `idtx:behavior:module`
//! (and a sibling `idtx:behavior:source`) authored as *relative* asset paths
//! on an `over`. Whether the runtime opens that layer directly (preview) or
//! reaches the prim through a composed assembly (packaged player), the relative
//! path must resolve — and its bytes read — against the layer that authored the
//! opinion, never the entry stage's directory. Reading via the resolver is also
//! what lets the same call serve package-internal `foo.usdz[inner.wasm]` paths.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`).

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::StageService;

static NEXT: AtomicU64 = AtomicU64::new(1);

const MODULE_ATTR: &str = "idtx:behavior:module";
const SOURCE_ATTR: &str = "idtx:behavior:source";

/// Bytes we write as the "module" — an arbitrary payload (a valid host module is
/// not needed; we only prove byte-exact resolve→read, not wasm validation).
const MODULE_BYTES: &[u8] = b"\0asm\x01\x00\x00\x00idtx-fake-module";
const SOURCE_TEXT: &str = "// cube_behavior.rs\nfn tick() {}\n";

/// A hermetic project laid out on disk:
/// ```text
/// <root>/assembly.usda                        (payloads the level under /World/Levels/lvl)
/// <root>/stages/level.usda                    (/World/Cylinder overs the module + source)
/// <root>/stages/behaviors/cube_behavior.wasm      (the resolved module target)
/// <root>/stages/behaviors/cube_behavior.rs (the resolved source target)
/// ```
struct Fixture {
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "idtx-read-asset-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("stages/behaviors")).unwrap();

        std::fs::write(
            root.join("stages/behaviors/cube_behavior.wasm"),
            MODULE_BYTES,
        )
        .unwrap();
        std::fs::write(
            root.join("stages/behaviors/cube_behavior.rs"),
            SOURCE_TEXT,
        )
        .unwrap();

        // Level layer overs the module + source as relative asset paths, i.e.
        // exactly what `behavior_prepare::author_module_overlay` authors next to
        // each other on the assembly overlay.
        std::fs::write(
            root.join("stages/level.usda"),
            "#usda 1.0\n\
             (\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n)\n\n\
             def Xform \"World\"\n{\n\
             \x20   def Cylinder \"Cylinder\"\n    {\n\
             \x20       custom asset idtx:behavior:module = @./behaviors/cube_behavior.wasm@\n\
             \x20       custom asset idtx:behavior:source = @./behaviors/cube_behavior.rs@\n\
             \x20   }\n}\n",
        )
        .unwrap();

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

/// Read the bytes of the `attr` asset authored on `prim_path` via the same
/// resolve→read composition `StudioSession::read_asset_via_resolver` performs.
fn read_via_resolver(stage: &StageService, prim_path: &str, attr: &str) -> Option<Vec<u8>> {
    let prim = stage.prim_at_path(prim_path)?;
    let resolved = stage
        .resolve_prim_asset(prim, attr)
        .expect("resolve call")?;
    Some(stage.read_asset(&resolved).expect("read asset"))
}

#[test]
fn reads_module_and_source_bytes_via_resolver_direct_open() {
    // Preview shape: open the authoring level directly.
    let fx = Fixture::new();
    let stage = StageService::open(fx.root.join("stages/level.usda")).expect("open level");

    let module = read_via_resolver(&stage, "/World/Cylinder", MODULE_ATTR)
        .expect("module resolves and reads");
    assert_eq!(module, MODULE_BYTES, "module bytes must round-trip exactly");

    let source = read_via_resolver(&stage, "/World/Cylinder", SOURCE_ATTR)
        .expect("source resolves and reads");
    assert_eq!(
        std::str::from_utf8(&source).unwrap(),
        SOURCE_TEXT,
        "source text must round-trip exactly"
    );
}

#[test]
fn reads_module_bytes_via_resolver_through_composed_assembly() {
    // Packaged/preview shape: reach the prim through the composed assembly whose
    // entry layer sits in a *different* directory than the authoring level. The
    // relative `@./behaviors/...@` must still resolve+read against the level.
    let fx = Fixture::new();
    let stage = StageService::open(fx.root.join("assembly.usda")).expect("open assembly");

    let module = read_via_resolver(&stage, "/World/Levels/lvl/Cylinder", MODULE_ATTR)
        .expect("composed module resolves and reads");
    assert_eq!(
        module, MODULE_BYTES,
        "composed-prim module must anchor to the level layer, not the assembly dir"
    );

    let source = read_via_resolver(&stage, "/World/Levels/lvl/Cylinder", SOURCE_ATTR)
        .expect("composed source resolves and reads");
    assert_eq!(std::str::from_utf8(&source).unwrap(), SOURCE_TEXT);
}

#[test]
fn absent_module_attribute_resolves_to_none() {
    let fx = Fixture::new();
    let stage = StageService::open(fx.root.join("stages/level.usda")).expect("open level");
    assert!(
        read_via_resolver(&stage, "/World", MODULE_ATTR).is_none(),
        "a prim with no authored module ⇒ None (caller falls back to filesystem)"
    );
}
