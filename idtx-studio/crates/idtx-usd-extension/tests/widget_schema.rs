//! Integration tests for the codeless `IdtxWidget*` `IsA` schemas.
//!
//! Like the collision / HUD tests these require a linked `OpenUSD`
//! (`IDTX_USD_DIR`) with the runtime libs on the loader path
//! (`DYLD_LIBRARY_PATH` on macOS), so they are `#[ignore]` and run explicitly:
//!
//! ```bash
//! export IDTX_USD_DIR=.../openusd-26.11.1/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib:$DYLD_LIBRARY_PATH
//! cargo test -p idtx-usd-extension --test widget_schema -- --ignored
//! ```

use std::path::{Path, PathBuf};

use idtx_usd::StageService;
use idtx_usd_extension::{
    ATTR_WIDGET_DOUBLE_SIDED, ATTR_WIDGET_PIXELS_PER_UNIT, ATTR_WIDGET_PRECISION, ATTR_WIDGET_SIZE,
    WIDGET_BUTTON_SCHEMA, WIDGET_CONTAINER_SCHEMA, WIDGET_ELEMENT_SCHEMA, WIDGET_FIELD_DOUBLE_SCHEMA,
    WIDGET_FIELD_SCHEMA, WIDGET_LABEL_SCHEMA, WIDGET_PANEL_SCHEMA,
};

fn widget_stage_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/widgets/widget_panel_stage.usda")
}

fn in_scene_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/widgets/widget_in_scene.usda")
}

fn bound_value_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/widgets/widget_bound_value.usda")
}

/// The value-field split (docs/widgets-in-usd-proposal-v2.md §3): a concrete
/// `IdtxWidgetFieldDouble` is detected as its own type AND — through codeless
/// `IsA` inheritance — as the abstract `IdtxWidgetField` base, while a plain
/// `IdtxWidgetLabel` is NOT a field. Its inherited `idtx:widget:precision`
/// fallback also resolves, proving the abstract base's properties flatten onto
/// the concrete subclass in the registry.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn widget_field_double_is_a_abstract_field_base() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let f = bound_value_fixture();
    assert!(f.exists(), "fixture missing: {}", f.display());
    let mut stage = StageService::open(&f).expect("open widget_bound_value fixture");
    stage.project_topology().expect("topology");

    let field = stage
        .prim_at_path("/World/SizeReadout/Row/Value")
        .expect("Value field prim");
    assert!(
        stage
            .prim_is_a(field, WIDGET_FIELD_DOUBLE_SCHEMA)
            .expect("prim_is_a field-double"),
        "the Value prim must be an {WIDGET_FIELD_DOUBLE_SCHEMA}"
    );
    assert!(
        stage
            .prim_is_a(field, WIDGET_FIELD_SCHEMA)
            .expect("prim_is_a field-base"),
        "{WIDGET_FIELD_DOUBLE_SCHEMA} must derive from the abstract {WIDGET_FIELD_SCHEMA}"
    );

    // The inherited precision fallback (3) resolves — proving the abstract base's
    // properties are in the registry under the concrete type.
    let prec = stage
        .eval_attribute(field, ATTR_WIDGET_PRECISION, 0.0, true)
        .expect("eval precision")
        .expect("precision resolves from the inherited schema fallback");
    assert_eq!(prec.type_name, "int");
    assert!(
        prec.value_repr.contains('3'),
        "precision fallback should be 3; got {:?}",
        prec.value_repr
    );

    // A label is text-only — NOT a field (the two roles are distinct).
    let caption = stage
        .prim_at_path("/World/SizeReadout/Row/Caption")
        .expect("Caption label");
    assert!(
        stage
            .prim_is_a(caption, WIDGET_LABEL_SCHEMA)
            .expect("prim_is_a label"),
        "Caption must be an {WIDGET_LABEL_SCHEMA}"
    );
    assert!(
        !stage
            .prim_is_a(caption, WIDGET_FIELD_SCHEMA)
            .expect("prim_is_a label-not-field"),
        "a label must not be detected as an {WIDGET_FIELD_SCHEMA}"
    );
}

/// The widget-stage fixture loads and its root/default prim is detected as an
/// `IdtxWidgetPanel`; its element children resolve to their own widget types;
/// and because the panel derives from `UsdGeomGprim`, `prim_is_a("Gprim")` (and
/// transitively `"Boundable"`) is true — the panel is the one widget prim that
/// enters 3D. Its element children derive from `IdtxWidgetElement`
/// (`UsdGeomImageable`), so each `prim_is_a("Imageable")` holds while
/// `prim_is_a("Xformable")` / `"Gprim")` / `"Boundable")` is false — a pure 2D
/// visual, never independently transformed. Detection only works when the codeless
/// schema is registered — `install()` must run first.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn widget_panel_stage_loads_and_panel_is_detected() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let f = widget_stage_fixture();
    assert!(f.exists(), "fixture missing: {}", f.display());
    let mut stage = StageService::open(&f).expect("open widget_panel_stage fixture");
    stage.project_topology().expect("topology");

    let panel = stage.prim_at_path("/Panel").expect("root Panel prim");
    assert!(
        stage
            .prim_is_a(panel, WIDGET_PANEL_SCHEMA)
            .expect("prim_is_a panel"),
        "the stage-root prim must be detected as an {WIDGET_PANEL_SCHEMA}"
    );
    // The panel derives from UsdGeomGprim → IsA<Gprim> holds (a renderable
    // geometric object in 3D), and transitively IsA<Boundable>. This is the
    // decisive Gprim-codeless round-trip.
    assert!(
        stage.prim_is_a(panel, "Gprim").expect("prim_is_a Gprim"),
        "{WIDGET_PANEL_SCHEMA} must derive from UsdGeomGprim"
    );
    assert!(
        stage.prim_is_a(panel, "Boundable").expect("prim_is_a Boundable"),
        "{WIDGET_PANEL_SCHEMA} is transitively a Boundable (Gprim → Boundable)"
    );

    // Element children resolve to their own widget types.
    let root = stage.prim_at_path("/Panel/Root").expect("Root container");
    assert!(
        stage
            .prim_is_a(root, WIDGET_CONTAINER_SCHEMA)
            .expect("prim_is_a container"),
        "Root must be an {WIDGET_CONTAINER_SCHEMA}"
    );
    let title = stage
        .prim_at_path("/Panel/Root/Title")
        .expect("Title label");
    assert!(
        stage
            .prim_is_a(title, WIDGET_LABEL_SCHEMA)
            .expect("prim_is_a label"),
        "Title must be an {WIDGET_LABEL_SCHEMA}"
    );
    // A label is an IdtxWidgetElement → IsA<Imageable> (a pure visual), but NOT
    // Xformable / Gprim / Boundable: it never enters 3D on its own, so no 3D
    // transform or bounds is ever exposed on it.
    assert!(
        stage.prim_is_a(title, WIDGET_ELEMENT_SCHEMA).expect("prim_is_a element"),
        "a label must derive from {WIDGET_ELEMENT_SCHEMA}"
    );
    assert!(
        stage.prim_is_a(title, "Imageable").expect("prim_is_a Imageable"),
        "a widget element must derive from UsdGeomImageable"
    );
    assert!(
        !stage.prim_is_a(title, "Xformable").expect("prim_is_a Xformable"),
        "a widget element must NOT be Xformable (no independent transform)"
    );
    assert!(
        !stage.prim_is_a(title, "Boundable").expect("prim_is_a Boundable"),
        "a widget element must NOT be Boundable (no independent bounds)"
    );
    let ok = stage.prim_at_path("/Panel/Root/Ok").expect("Ok button");
    assert!(
        stage
            .prim_is_a(ok, WIDGET_BUTTON_SCHEMA)
            .expect("prim_is_a button"),
        "Ok must be an {WIDGET_BUTTON_SCHEMA}"
    );
    // A label is NOT a panel — the types are distinct, not just "any widget".
    assert!(
        !stage
            .prim_is_a(title, WIDGET_PANEL_SCHEMA)
            .expect("prim_is_a label-not-panel"),
        "a label must not be detected as a panel"
    );
}

/// The in-scene fixture loads: the floating inline `IdtxWidgetPanel` is detected
/// among ordinary 3D gprims, and those gprims are NOT mistaken for widgets.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn widget_in_scene_panel_detected_among_gprims() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let f = in_scene_fixture();
    assert!(f.exists(), "fixture missing: {}", f.display());
    let mut stage = StageService::open(&f).expect("open widget_in_scene fixture");
    stage.project_topology().expect("topology");

    let panel = stage
        .prim_at_path("/World/InfoPanel")
        .expect("floating InfoPanel");
    assert!(
        stage
            .prim_is_a(panel, WIDGET_PANEL_SCHEMA)
            .expect("prim_is_a panel"),
        "the floating InfoPanel must be detected as an {WIDGET_PANEL_SCHEMA}"
    );

    // The ordinary 3D gprims load and are NOT widget panels.
    for (path, usd_type) in [
        ("/World/Box", "Cube"),
        ("/World/Pillar", "Cylinder"),
        ("/World/Ball", "Sphere"),
    ] {
        let prim = stage.prim_at_path(path).unwrap_or_else(|| panic!("{path} prim"));
        assert!(
            stage.prim_is_a(prim, usd_type).expect("prim_is_a gprim type"),
            "{path} must still be a {usd_type}"
        );
        assert!(
            !stage
                .prim_is_a(prim, WIDGET_PANEL_SCHEMA)
                .expect("prim_is_a gprim-not-panel"),
            "{path} ({usd_type}) must NOT be detected as a widget panel"
        );
    }
}

/// With the schema registered, an `IdtxWidgetPanel`'s schema-supplied fallbacks
/// resolve for attributes left unauthored in the fixture — proving the codeless
/// `generatedSchema.usda` fallbacks are in the registry (if the type were not
/// registered there would be no fallback to read).
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn widget_panel_schema_fallbacks_resolve() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(widget_stage_fixture()).expect("open widget stage");
    stage.project_topology().expect("topology");
    let panel = stage.prim_at_path("/Panel").expect("root Panel prim");

    // `pixelsPerUnit` is NOT authored in the fixture → the schema fallback 2048.
    let ppu = stage
        .eval_attribute(panel, ATTR_WIDGET_PIXELS_PER_UNIT, 0.0, true)
        .expect("eval pixelsPerUnit")
        .expect("pixelsPerUnit resolves from schema fallback");
    assert_eq!(ppu.type_name, "float");
    assert!(
        ppu.value_repr.contains("2048"),
        "pixelsPerUnit fallback should be 2048; got {:?}",
        ppu.value_repr
    );

    // `doubleSided` is NOT authored → schema fallback true.
    let ds = stage
        .eval_attribute(panel, ATTR_WIDGET_DOUBLE_SIDED, 0.0, true)
        .expect("eval doubleSided")
        .expect("doubleSided resolves from schema fallback");
    assert_eq!(ds.type_name, "bool");

    // `size` IS authored (0.4, 0.3) → the authored opinion wins over the fallback.
    let size = stage
        .eval_attribute(panel, ATTR_WIDGET_SIZE, 0.0, true)
        .expect("eval size")
        .expect("size authored");
    assert_eq!(size.type_name, "float2");
    assert!(
        size.value_repr.contains("0.4"),
        "size repr should contain the authored 0.4; got {:?}",
        size.value_repr
    );
}
