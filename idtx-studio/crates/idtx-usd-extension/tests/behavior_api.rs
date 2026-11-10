//! Integration tests for the codeless `IdtxBehaviorAPI` schema.
//!
//! These require a linked `OpenUSD` (`IDTX_USD_DIR`) and the runtime libs on the
//! loader path (`DYLD_LIBRARY_PATH` on macOS), so they are `#[ignore]` and run
//! explicitly:
//!
//! ```bash
//! export IDTX_USD_DIR=.../openusd-26.11.1/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib:$DYLD_LIBRARY_PATH
//! cargo test -p idtx-usd-extension -- --ignored behavior_api
//! ```

use std::path::{Path, PathBuf};

use idtx_usd::{StageService, StructuralCommand};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/behavior/idtx_behavior_api.usda")
}

/// Separate layer for the mutating apply/remove test. OpenUSD caches stages by
/// root-layer identifier, so a test that authors on the shared read-only fixture
/// would race the detection test under parallel `cargo test`.
fn apply_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/behavior/idtx_behavior_apply.usda")
}

/// Register the schema plugin, open the fixture, and confirm the registry
/// recognises `IdtxBehaviorAPI` on the prim that applies it (and not on a plain
/// prim) — the canonical "this prim has a behavior" test.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn behavior_api_is_registered_and_detected() {
    use idtx_usd_extension::BEHAVIOR_SCHEMA;

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let f = fixture();
    assert!(f.exists(), "fixture missing: {}", f.display());
    let mut stage = StageService::open(&f).expect("open behavior fixture");
    stage.project_topology().expect("topology");

    let spinner = stage.prim_at_path("/World/Spinner").expect("spinner prim");
    let plain = stage.prim_at_path("/World/Plain").expect("plain prim");

    assert!(
        stage
            .prim_has_api(spinner, BEHAVIOR_SCHEMA)
            .expect("prim_has_api spinner"),
        "Spinner must carry IdtxBehaviorAPI"
    );
    assert!(
        !stage
            .prim_has_api(plain, BEHAVIOR_SCHEMA)
            .expect("prim_has_api plain"),
        "Plain must NOT carry IdtxBehaviorAPI"
    );

    // apiSchemas metadata must list the applied schema.
    let props = stage.inspect_prim(spinner).expect("inspect spinner");
    let api_summary = props
        .iter()
        .find(|p| p.name == "apiSchemas")
        .map(|p| p.value_summary.clone())
        .unwrap_or_default();
    assert!(
        api_summary.contains(BEHAVIOR_SCHEMA),
        "apiSchemas must list IdtxBehaviorAPI; got {api_summary:?}"
    );
}

/// Every `idtx:behavior:*` attribute evaluates with the expected USD type and
/// authored value through the schema.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn behavior_attributes_evaluate_with_expected_types() {
    use idtx_usd_extension::{
        ATTR_BEHAVIOR_CAPABILITIES, ATTR_BEHAVIOR_ENABLED, ATTR_BEHAVIOR_INPUTS, ATTR_BEHAVIOR_KIND,
        ATTR_BEHAVIOR_MEMORY_MB, ATTR_BEHAVIOR_MODULE, ATTR_BEHAVIOR_SCOPE,
        ATTR_BEHAVIOR_SERVER_AUTHORITATIVE, ATTR_BEHAVIOR_SOURCE, ATTR_BEHAVIOR_UUID,
    };

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(fixture()).expect("open behavior fixture");
    stage.project_topology().expect("topology");
    let spinner = stage.prim_at_path("/World/Spinner").expect("spinner prim");

    let eval = |stage: &mut StageService, attr: &str| {
        stage
            .eval_attribute(spinner, attr, 0.0, true)
            .unwrap_or_else(|e| panic!("eval {attr}: {e:?}"))
            .unwrap_or_else(|| panic!("{attr} authored"))
    };

    let kind = eval(&mut stage, ATTR_BEHAVIOR_KIND);
    assert_eq!(kind.type_name, "token");
    assert!(kind.value_repr.contains("runtime"));

    let uuid = eval(&mut stage, ATTR_BEHAVIOR_UUID);
    assert_eq!(uuid.type_name, "string");
    assert!(uuid.value_repr.contains("idtx-spin-0000"));

    for attr in [ATTR_BEHAVIOR_SOURCE, ATTR_BEHAVIOR_MODULE] {
        let a = eval(&mut stage, attr);
        assert_eq!(a.type_name, "asset", "{attr} must be asset-typed");
        assert!(!a.value_repr.is_empty());
    }

    let enabled = eval(&mut stage, ATTR_BEHAVIOR_ENABLED);
    assert_eq!(enabled.type_name, "bool");
    assert!(enabled.value_repr.contains('0') || enabled.value_repr.contains("false"));

    let scope = eval(&mut stage, ATTR_BEHAVIOR_SCOPE);
    assert_eq!(scope.type_name, "token");
    assert!(scope.value_repr.contains("stage"));

    let server = eval(&mut stage, ATTR_BEHAVIOR_SERVER_AUTHORITATIVE);
    assert_eq!(server.type_name, "bool");
    assert!(server.value_repr.contains('1') || server.value_repr.contains("true"));

    // Capabilities + inputs are token[] (whole-value); the memory cap is int.
    let caps = stage
        .eval_token_array(spinner, ATTR_BEHAVIOR_CAPABILITIES, 0.0, true)
        .expect("eval capabilities");
    assert!(
        caps.iter().any(|c| c == "input") && caps.iter().any(|c| c == "stage_edit"),
        "capabilities must round-trip the authored grant; got {caps:?}"
    );
    let inputs = stage
        .eval_token_array(spinner, ATTR_BEHAVIOR_INPUTS, 0.0, true)
        .expect("eval inputs");
    assert!(
        inputs.iter().any(|i| i == "jump:button"),
        "inputs must round-trip the \"<action>:<kind>\" token; got {inputs:?}"
    );
    let mem = eval(&mut stage, ATTR_BEHAVIOR_MEMORY_MB);
    assert_eq!(mem.type_name, "int");
    assert!(mem.value_repr.contains('8'));
}

/// `IdtxBehaviorAPI` can be applied to a prim that lacks it, then removed — the
/// backend for the Conductor attach/detach actions (HasAPI = "has a behavior").
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn behavior_api_applies_and_removes() {
    use idtx_usd_extension::BEHAVIOR_SCHEMA;

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(apply_fixture()).expect("open behavior apply fixture");
    stage.project_topology().expect("topology");
    let target = stage.prim_at_path("/World/Target").expect("target prim");
    assert!(
        !stage
            .prim_has_api(target, BEHAVIOR_SCHEMA)
            .expect("prim_has_api before apply"),
        "Target must start without IdtxBehaviorAPI"
    );

    stage
        .apply_structural_command(&StructuralCommand::ApplyApi {
            path: "/World/Target".to_owned(),
            schema: BEHAVIOR_SCHEMA.to_owned(),
        })
        .expect("apply IdtxBehaviorAPI");
    assert!(
        stage
            .prim_has_api(target, BEHAVIOR_SCHEMA)
            .expect("prim_has_api after apply"),
        "IdtxBehaviorAPI must be applied"
    );

    stage
        .apply_structural_command(&StructuralCommand::RemoveApi {
            path: "/World/Target".to_owned(),
            schema: BEHAVIOR_SCHEMA.to_owned(),
        })
        .expect("remove IdtxBehaviorAPI");
    assert!(
        !stage
            .prim_has_api(target, BEHAVIOR_SCHEMA)
            .expect("prim_has_api after remove"),
        "IdtxBehaviorAPI must be removed"
    );
}
