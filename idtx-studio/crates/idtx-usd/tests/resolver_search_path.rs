//! U1.7b — project-root asset resolution via the default-resolver search path.
//!
//! `set_default_asset_search_path` installs a process-wide
//! `ArDefaultResolver` search path so a *bare* asset reference (`@prop.usda@`,
//! with no directory component) resolves against a project's asset roots
//! instead of only the referring layer's own directory. This test hand-authors
//! an asset in an `assets/` subdir, references it by bare name from a sibling
//! `main.usda`, and proves the reference composes once the subdir is on the
//! search path.
//!
//! The layers are written as `.usda` text (not via the authoring API) so the
//! test needs no feature flags and does not depend on save-time dependency
//! resolution — resolution is exactly what we are exercising here.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`); the whole `idtx-usd` crate links USD. Stages are
//! authored into a unique temp directory, so no committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageService, set_default_asset_search_path};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A unique, freshly-created temp directory for one test's project layout.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "idtx-resolver-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

const PROP_USDA: &str = "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\"\n{\n    def Sphere \"Inner\"\n    {\n    }\n}\n";

// References `@prop.usda@` by BARE name — no directory component — so it cannot
// resolve relative to main.usda's own directory.
const MAIN_USDA: &str = "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\"\n{\n    def Xform \"Ref\" (\n        prepend references = @prop.usda@\n    )\n    {\n    }\n}\n";

#[test]
fn bare_reference_resolves_via_default_search_path() {
    let dir = scratch_dir("bare-ref");
    let assets_dir = dir.join("assets");
    std::fs::create_dir_all(&assets_dir).expect("create assets dir");
    std::fs::write(assets_dir.join("prop.usda"), PROP_USDA).expect("write prop.usda");
    let main_path = dir.join("main.usda");
    std::fs::write(&main_path, MAIN_USDA).expect("write main.usda");

    // Point the default resolver at the asset subdir, then open main.usda: the
    // bare `@prop.usda@` reference now resolves and its subtree composes under
    // the referencing prim. (Empty slice first proves clearing is a valid,
    // non-erroring call and gives a known starting state.)
    set_default_asset_search_path(&[]).expect("empty search path is accepted");
    set_default_asset_search_path(&[assets_dir.clone()]).expect("set search path");

    let mut stage = StageService::open(&main_path).expect("open main stage");
    stage.project_topology().expect("project topology");
    assert!(
        stage.prim_at_path("/World/Ref/Inner").is_some(),
        "bare reference composes once its directory is on the search path"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
