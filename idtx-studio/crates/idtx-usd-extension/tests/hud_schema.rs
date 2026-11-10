//! Integration tests for the codeless `IdtxHud` `IsA` schema + session-layer
//! attribute authoring (Phases 0–1 of the camera-HUD overlay plan).
//!
//! Like the collision tests these require a linked `OpenUSD` (`IDTX_USD_DIR`)
//! and the runtime libs on the loader path (`DYLD_LIBRARY_PATH` on macOS), so
//! they are `#[ignore]` and run explicitly:
//!
//! ```bash
//! export IDTX_USD_DIR=.../openusd-26.11.1/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib:$DYLD_LIBRARY_PATH
//! cargo test -p idtx-usd-extension --test hud_schema -- --ignored
//! ```

use std::path::{Path, PathBuf};

use idtx_usd::{PropertyValue, StageService};
use idtx_usd_extension::{
    ATTR_HUD_ANCHOR, ATTR_HUD_IMAGE, ATTR_HUD_OPACITY, ATTR_HUD_SIZE_PX, HUD_SCHEMA,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/behavior/idtx_hud_camera.usda")
}

/// Defining a prim of the concrete typed `IdtxHud` schema into the session
/// layer resolves the type (the codeless schema is registered) — proven by the
/// schema-supplied fallback for `idtx:hud:opacity` (`1.0`) and
/// `idtx:hud:anchor` (`center`) being readable *before* anything is authored.
/// If the schema were not registered, there would be no fallback to read.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn hud_schema_type_resolves_with_fallbacks() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let f = fixture();
    assert!(f.exists(), "fixture missing: {}", f.display());
    let mut stage = StageService::open(&f).expect("open hud camera fixture");
    stage.project_topology().expect("topology");

    let hud = stage
        .define_session_prim("/World/Camera/Hud", HUD_SCHEMA)
        .expect("define IdtxHud child of camera in session layer");

    // Schema fallbacks resolve only when the type is registered.
    let opacity = stage
        .eval_attribute(hud, ATTR_HUD_OPACITY, 0.0, true)
        .expect("eval opacity")
        .expect("opacity resolves from schema fallback");
    assert_eq!(opacity.type_name, "float");
    assert!(
        opacity.value_repr.contains('1'),
        "opacity fallback should be 1.0; got {:?}",
        opacity.value_repr
    );

    let anchor = stage
        .eval_attribute(hud, ATTR_HUD_ANCHOR, 0.0, true)
        .expect("eval anchor")
        .expect("anchor resolves from schema fallback");
    assert_eq!(anchor.type_name, "token");
    assert!(
        anchor.value_repr.contains("center"),
        "anchor fallback should be 'center'; got {:?}",
        anchor.value_repr
    );
}

/// A behavior-style flow: define the HUD child under the camera in the session
/// layer, author its image/size/anchor/opacity via `author_session_property`,
/// and read each back with the expected USD type.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn hud_session_property_authoring_roundtrips() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(fixture()).expect("open hud camera fixture");
    stage.project_topology().expect("topology");

    let hud = stage
        .define_session_prim("/World/Camera/Hud", HUD_SCHEMA)
        .expect("define IdtxHud in session layer");
    let path = "/World/Camera/Hud";

    stage
        .author_session_property(
            path,
            ATTR_HUD_IMAGE,
            &PropertyValue::Asset("crosshair.png".to_owned()),
        )
        .expect("author hud image asset");
    stage
        .author_session_property(path, ATTR_HUD_SIZE_PX, &PropertyValue::Int2([64, 64]))
        .expect("author hud size int2");
    stage
        .author_session_property(
            path,
            ATTR_HUD_ANCHOR,
            &PropertyValue::Token("center".to_owned()),
        )
        .expect("author hud anchor token");
    stage
        .author_session_property(path, ATTR_HUD_OPACITY, &PropertyValue::F32(0.8))
        .expect("author hud opacity float");

    let image = stage
        .eval_attribute(hud, ATTR_HUD_IMAGE, 0.0, true)
        .expect("eval image")
        .expect("image authored");
    assert_eq!(image.type_name, "asset");
    assert!(
        image.value_repr.contains("crosshair.png"),
        "image repr should contain the asset path; got {:?}",
        image.value_repr
    );

    let size = stage
        .eval_attribute(hud, ATTR_HUD_SIZE_PX, 0.0, true)
        .expect("eval sizePx")
        .expect("sizePx authored");
    assert_eq!(size.type_name, "int2");
    assert!(
        size.value_repr.contains("64"),
        "sizePx repr should contain 64; got {:?}",
        size.value_repr
    );

    let anchor = stage
        .eval_attribute(hud, ATTR_HUD_ANCHOR, 0.0, true)
        .expect("eval anchor")
        .expect("anchor authored");
    assert_eq!(anchor.type_name, "token");
    assert!(anchor.value_repr.contains("center"));

    let opacity = stage
        .eval_attribute(hud, ATTR_HUD_OPACITY, 0.0, true)
        .expect("eval opacity")
        .expect("opacity authored");
    assert_eq!(opacity.type_name, "float");
    assert!(
        opacity.value_repr.contains("0.8"),
        "opacity repr should contain 0.8; got {:?}",
        opacity.value_repr
    );

    // The HUD lives only in the session layer: neither the HUD prim spec
    // (`IdtxHud "Hud"`) nor its `idtx:hud:` attributes may appear in the root
    // layer, so the saved assembly stays clean.
    let root = stage.root_layer_text().expect("read root layer text");
    assert!(
        !root.contains("idtx:hud:") && !root.contains(&format!("{HUD_SCHEMA} \"Hud\"")),
        "HUD prim/attrs must not be authored into the root layer; root was:\n{root}"
    );
}
