//! Built-in player stage — composition + `@built-in/...@` search-path resolution.
//!
//! Guards the structural contract of the toolbox's built-in player
//! (`crates/idtx-assets/built-in/players/Player.usd`): it must compose as a
//! standalone stage rooted at `/Player` (so it opens and renders in the editor)
//! with the selected `playerKind` rig's prims at the paths the renderer / XR /
//! behavior layers expect (`/Player/Head/Camera`, `/Player/Controllers/Left`,
//! …). It also proves the `built-in/players/Player.usd` arc resolves once the
//! containing directory is on the default-resolver search path, and that the
//! stage references cleanly into `/World/Player` (the assembly layout).
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and put
//! its libs on the loader path); the whole `idtx-usd` crate links USD. Run with
//! `--features authoring` (the reference-composition case authors structurally)
//! and `--test-threads=1`.

use std::path::PathBuf;

use idtx_usd::StageService;

/// The directory that *contains* the `built-in/` tree (the search dir), derived
/// from this crate's manifest dir: `<ws>/crates/idtx-usd` → `<ws>/crates/idtx-assets`.
fn builtin_search_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../idtx-assets")
        .canonicalize()
        .expect("idtx-assets crate dir exists")
}

/// Absolute path of the built-in player stage.
fn builtin_player_stage() -> PathBuf {
    builtin_search_dir().join("built-in/players/Player.usd")
}

#[test]
fn builtin_player_composes_standalone_desktop_variant() {
    let stage_path = builtin_player_stage();
    assert!(
        stage_path.is_file(),
        "built-in player stage missing at {}",
        stage_path.display()
    );

    // Open the stage directly — the rig references are layer-relative
    // (`@./Desktop.Player.Rig.usda@`), so no search path is needed. The default
    // `playerKind = "desktop"` variant selects the desktop rig.
    let mut stage = StageService::open(&stage_path).expect("open built-in player stage");
    stage.project_topology().expect("project topology");

    // Standalone the rig composes under the root `/Player` (the editor-editable
    // shape), with the desktop camera under an eye-height `Head`.
    assert!(
        stage.prim_at_path("/Player").is_some(),
        "built-in player must root at /Player"
    );
    assert!(
        stage.prim_at_path("/Player/Head/Camera").is_some(),
        "desktop variant must compose /Player/Head/Camera"
    );
    assert!(
        stage.prim_at_path("/Player/Capsule/Proxy").is_some(),
        "desktop variant must compose the capsule proxy"
    );
}

#[test]
fn builtin_arc_resolves_via_search_path() {
    // With the containing dir on the default-resolver search path, the verbatim
    // `built-in/players/Player.usd` arc resolves (this is how a project's
    // player_stage default and the cook assembly reference it).
    idtx_usd::set_default_asset_search_path(&[builtin_search_dir()])
        .expect("set search path to the built-in root");

    let dir = std::env::temp_dir().join(format!("idtx-builtin-arc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // A stage that references the built-in player by its `built-in/...` arc into
    // `/World/Player`, mirroring `assembly::author_assembly_stage`.
    let main = dir.join("main.usda");
    std::fs::write(
        &main,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\
         def Xform \"World\"\n{\n    def Xform \"Player\" (\n        \
         prepend references = @built-in/players/Player.usd@</Player>\n    )\n    {\n    }\n}\n",
    )
    .expect("write main.usda");

    let mut stage = StageService::open(&main).expect("open referencing stage");
    stage.project_topology().expect("project topology");
    assert!(
        stage.prim_at_path("/World/Player/Head/Camera").is_some(),
        "built-in player must compose at /World/Player via the built-in arc"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
