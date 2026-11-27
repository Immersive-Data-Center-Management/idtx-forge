//! Regression: a *search-path-anchored* behavior source resolves to the project
//! root, not to the authoring stage's own directory.
//!
//! The datacenter project authors, on a widget stage at
//! `assets/widgets/<panel>.usda`:
//!
//! ```usda
//! custom asset idtx:behavior:source = @assets/behaviors/<x>.rs@
//! ```
//!
//! That `assets/behaviors/<x>.rs` is **not** relative to the stage (which would
//! be `assets/widgets/assets/behaviors/<x>.rs` and does not exist) — it is
//! anchored against the project's installed resolver search path (the project
//! root is on it, via `asset_search_roots()`'s implicit `""` entry). Preview and
//! cooking resolve it correctly because they go through
//! [`StageService::resolve_prim_asset`]; the Conductor's Check/Verify buttons
//! used to do a naive `stage_dir.join(..)` and produced the wrong path. This
//! pins the resolver guarantee the fix relies on.
//!
//! Isolated in its own test binary because `set_default_asset_search_path`
//! installs a **process-global** search path (racing with any sibling test that
//! also sets it); one test per binary keeps it deterministic.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`); the whole `idtx-usd` crate links USD.

use idtx_usd::{StageService, set_default_asset_search_path};

const SOURCE_ATTR: &str = "idtx:behavior:source";

// Authors the source by a project/search-path-anchored path (`assets/...`), NOT
// by a stage-relative `./...` — so it cannot resolve against the stage's own dir.
const PANEL_USDA: &str = "#usda 1.0\n\
     (\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n)\n\n\
     def Xform \"World\"\n{\n\
     \x20   def Cube \"Panel\"\n    {\n\
     \x20       custom asset idtx:behavior:source = @assets/behaviors/login.rs@\n\
     \x20   }\n}\n";

#[test]
fn search_path_anchored_source_resolves_to_project_root_not_stage_dir() {
    let root = std::env::temp_dir().join(format!(
        "idtx-behavior-src-searchpath-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("assets/behaviors")).unwrap();
    std::fs::create_dir_all(root.join("assets/widgets")).unwrap();

    // The real target lives under the project root's `assets/behaviors/`.
    std::fs::write(
        root.join("assets/behaviors/login.rs"),
        b"// behavior source\n",
    )
    .unwrap();
    // The authoring stage lives one level deeper, under `assets/widgets/`.
    let panel = root.join("assets/widgets/panel.usda");
    std::fs::write(&panel, PANEL_USDA).unwrap();

    // Mirror the Conductor/cook search path: the project ROOT is on it (the
    // implicit `""` entry of `asset_search_roots()`), so `assets/behaviors/x.rs`
    // anchors there. (Empty slice first clears any inherited state.)
    set_default_asset_search_path(&[]).expect("empty search path accepted");
    set_default_asset_search_path(&[root.clone()]).expect("set search path");

    let stage = StageService::open(&panel).expect("open panel stage");
    let prim = stage
        .prim_at_path("/World/Panel")
        .expect("panel prim exists");
    let resolved = stage
        .resolve_prim_asset(prim, SOURCE_ATTR)
        .expect("resolve call")
        .expect("search-path-anchored source resolves to a concrete path")
        .replace('\\', "/");

    assert!(
        resolved.ends_with("assets/behaviors/login.rs"),
        "source must anchor to the project root's assets/ (search path), got {resolved}"
    );
    assert!(
        !resolved.contains("assets/widgets/assets"),
        "source must NOT be joined onto the stage directory (the old bug), got {resolved}"
    );
    assert!(
        std::path::Path::new(&resolved).is_file(),
        "resolved path must point at the on-disk source: {resolved}"
    );

    set_default_asset_search_path(&[]).expect("reset search path");
    let _ = std::fs::remove_dir_all(&root);
}
