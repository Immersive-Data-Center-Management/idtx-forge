//! End-to-end `OpenExec` dataflow proof (feature-gated via `idtx-usd/openexec`).
//!
//! This exercises the whole new pipeline except ECS hydration (which is the
//! existing, separately-tested notice→hydrate path): plugin discovery + static
//! computation registration → value-key build → `OpenExec` compute → session-layer
//! authoring, all on the calling thread under the exclusive lifecycle lock.
//!
//! Needs the `OpenUSD` runtime DLLs discoverable (set `IDTX_USD_DIR` and put its
//! `lib`/`bin` on `PATH` — the CLAUDE.md test-harness note). Run single-threaded
//! alongside other USD/GPU tests (`-- --test-threads=1`).

use idtx_usd::StageService;
use std::path::PathBuf;
use std::sync::Mutex;

/// `idtx_auth` is a single process-global credential bucket, so tests that store
/// or switch credentials in it would race under the default parallel test run
/// (one test flipping the bucket to Basic while another expects the Bearer it
/// stored). Every bucket-using test holds this lock for its whole body, which
/// serializes them regardless of `--test-threads`. A panicking test poisons the
/// lock; recover the guard rather than cascade-failing the rest.
static AUTH_BUCKET_LOCK: Mutex<()> = Mutex::new(());

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn compute_fixture(name: &str) -> PathBuf {
    workspace_root().join("tests/fixtures/usd/compute").join(name)
}

fn widgets_fixture(name: &str) -> PathBuf {
    workspace_root().join("tests/fixtures/usd/widgets").join(name)
}

fn parse_f64(repr: &str) -> f64 {
    repr.trim()
        .trim_matches(|c| c == '"' || c == '[' || c == ']')
        .parse::<f64>()
        .unwrap_or_else(|_| panic!("result value_repr not a number: {repr:?}"))
}

/// A string `value_repr` comes back quoted (`"Bearer abc"`); strip the quotes so
/// an equality check reads naturally.
fn parse_string(repr: &str) -> String {
    repr.trim().trim_matches('"').to_owned()
}

/// Regression for the datasource → compute-node chain: a compute node's INPUT
/// connection (`inputs:jsonData.connect = </Src.outputs:data>`) is internal graph
/// wiring, NOT a value key. `build_program` must register only the terminal
/// consumer (`Cube.size`), else `OpenExec` errors "Failed to find computation
/// 'outputs:data'" on the datasource (it has no such computation). Also drives a
/// real JSON payload through `exec_enqueue` (what the REST poller delivers) to
/// prove the full datasource → ValueFromJson → consumer author path.
#[test]
fn datasource_compute_chain_registers_only_terminal_consumer() {
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");

    let path = compute_fixture("exec_chain_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_chain_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");

    // Exactly one value key: `/World/Cube.size`. The compute node's
    // `inputs:jsonData` connection to the datasource is internal wiring and must
    // NOT be registered (that was the "Failed to find computation outputs:data").
    assert_eq!(
        stage.exec_build().expect("exec_build"),
        1,
        "only the terminal Cube.size connection is a value key"
    );

    // With the datasource still at "{}", the JSON pointer misses → compute yields
    // 0.0, authored over the connected default 1.0. The key assertion is that the
    // tick SUCCEEDS (no missing-computation error).
    stage.exec_tick().expect("exec_tick must not error on the datasource chain");

    // Now deliver a real payload the way the REST poller would, and confirm it
    // flows datasource → ValueFromJson(/data/value) → Cube.size.
    assert!(
        stage
            .exec_enqueue("/World/Data/MockDataSource", r#"{"data":{"value":3.5}}"#)
            .expect("exec_enqueue"),
        "enqueue accepted"
    );
    let authored = stage.exec_tick().expect("exec_tick with payload");
    assert_eq!(
        authored, 2,
        "both the datasource outputs:data write and the Cube.size consumer write \
         are authored this tick"
    );

    stage.project_topology().expect("reproject");
    let cube = stage.prim_at_path("/World/Cube").expect("/World/Cube");
    let size = stage
        .eval_attribute(cube, "size", 0.0, true)
        .expect("eval size")
        .expect("size has a value");
    let got = parse_f64(&size.value_repr);
    assert!(
        (got - 3.5).abs() < 1e-9,
        "Cube.size should be the extracted 3.5, got {got} (repr {:?})",
        size.value_repr
    );
}

/// Regression (PR review): a tick that refreshes a datasource payload must report
/// a non-zero authored count even when the downstream computed consumer value is
/// unchanged. `ApplyPendingUpdates` authors `outputs:data` (a stage mutation that
/// emits observer notices the host must drain); if `tick_exec` only counted the
/// consumer writes, a changing payload whose extracted value stayed equal would
/// return 0, the host would skip its drain, and those notices would accumulate
/// indefinitely.
#[test]
fn payload_refresh_counts_even_when_consumer_value_unchanged() {
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");

    let path = compute_fixture("exec_chain_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_chain_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");
    assert_eq!(stage.exec_build().expect("exec_build"), 1);

    // First payload: Cube.size becomes 3.5. Two writes are authored — the
    // datasource `outputs:data` payload and the `Cube.size` consumer value.
    assert!(stage
        .exec_enqueue("/World/Data/MockDataSource", r#"{"data":{"value":3.5}}"#)
        .expect("exec_enqueue"));
    assert_eq!(
        stage.exec_tick().expect("first tick"),
        2,
        "the first payload authors both outputs:data and Cube.size"
    );

    // Second payload: a DIFFERENT JSON body that extracts to the SAME value 3.5,
    // so the computed consumer value is unchanged and the consumer write is
    // skipped. Only the datasource `outputs:data` write happens, so the tick
    // reports exactly 1 (pre-fix this returned 0 and the host skipped its drain).
    assert!(stage
        .exec_enqueue(
            "/World/Data/MockDataSource",
            r#"{"data":{"value":3.5},"note":"changed-payload"}"#
        )
        .expect("exec_enqueue"));
    assert_eq!(
        stage.exec_tick().expect("second tick"),
        1,
        "a refreshed payload must count as authored even when the computed \
         consumer value is unchanged, so the host drains its observer notices"
    );

    // The consumer value is indeed still 3.5 (the point: the consumer write was
    // skipped, yet the tick still reported a mutation).
    stage.project_topology().expect("reproject");
    let cube = stage.prim_at_path("/World/Cube").expect("/World/Cube");
    let size = stage
        .eval_attribute(cube, "size", 0.0, true)
        .expect("eval size")
        .expect("size has a value");
    assert!(
        (parse_f64(&size.value_repr) - 3.5).abs() < 1e-9,
        "Cube.size stays 3.5 (repr {:?})",
        size.value_repr
    );
}

/// Phase-5 compute-binding proof (docs/widgets-in-usd-proposal-v2.md §3): a
/// widget value-field prim (`IdtxWidgetFieldDouble`, which inherits the abstract
/// `IdtxWidgetField : UsdTyped`, NOT xformable) carrying a bound
/// `double idtx:widget:value.connect` must become an `OpenExec` value key —
/// otherwise a widget field can never be the leaf of a compute graph. This
/// exercises the `build_program` traversal widening to `IdtxWidget*`
/// type-name-prefixed prims. The fixture's single produced double feeds BOTH
/// `Cube.size` and the field's `idtx:widget:value`, so exactly two value keys
/// register, and feeding a payload propagates to the field.
#[test]
fn widget_field_value_binds_to_compute_output() {
    // No `idtx_usd_extension::install()` needed: `build_program` classifies prims
    // by authored type-name string (`GetTypeName()` returns "IdtxWidgetFieldDouble"
    // whether or not the codeless schema's TfType is registered), and
    // `eval_attribute` reads the authored `idtx:widget:value` double directly.
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");

    let path = widgets_fixture("widget_bound_value.usda");
    if !path.exists() {
        eprintln!("skip: widget_bound_value.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");

    // Two terminal consumers connect to the compute output: `Cube.size` (an
    // xformable gprim, always registered) and the field's `idtx:widget:value`
    // (a non-xformable widget element — registered only because the traversal now
    // includes `IdtxWidget*` prims). Both point at a `Compute_*` source, so both
    // pass `RegisterAttributeWithConnection`'s source filter.
    assert_eq!(
        stage.exec_build().expect("exec_build"),
        2,
        "Cube.size AND the widget field's idtx:widget:value are both value keys"
    );

    // Deliver a payload the way the mock/REST driver would and confirm the SAME
    // produced double lands on the widget field.
    assert!(
        stage
            .exec_enqueue("/World/Data/Source", r#"{"data":{"value":0.625}}"#)
            .expect("exec_enqueue"),
        "enqueue accepted"
    );
    // outputs:data + Cube.size + field idtx:widget:value = 3 authored.
    assert_eq!(
        stage.exec_tick().expect("exec_tick with payload"),
        3,
        "the datasource write plus both consumer writes are authored this tick"
    );

    stage.project_topology().expect("reproject");
    let field = stage
        .prim_at_path("/World/SizeReadout/Row/Value")
        .expect("/World/SizeReadout/Row/Value");
    let value = stage
        .eval_attribute(field, "idtx:widget:value", 0.0, true)
        .expect("eval idtx:widget:value")
        .expect("idtx:widget:value has a value");
    let got = parse_f64(&value.value_repr);
    assert!(
        (got - 0.625).abs() < 1e-9,
        "the field's idtx:widget:value should track the extracted 0.625, got {got} (repr {:?})",
        value.value_repr
    );
}

/// Phase-5: a reusable readout panel referenced beside two shapes, each wired to
/// its own mock→compute chain. Proves (a) element-level overrides through a
/// reference become value keys (each referenced `IdtxWidgetFieldDouble` carries
/// an overridden `idtx:widget:value.connect`), and (b) multiple independent
/// datasources coexist: `CubeData` drives `Cube.size` + CubePanel's field;
/// `CylData` drives `Column.height` + CylPanel's field — four value keys total,
/// each tracking its own payload.
#[test]
fn widget_referenced_panels_bind_two_mock_chains() {
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");

    let path = widgets_fixture("widget_bound_value_referenced.usda");
    if !path.exists() {
        eprintln!("skip: widget_bound_value_referenced.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");

    // Cube.size, Column.height, and both referenced field values = 4 value keys.
    assert_eq!(
        stage.exec_build().expect("exec_build"),
        4,
        "two shapes + two referenced field overrides are all value keys"
    );

    assert!(stage
        .exec_enqueue("/World/Data/CubeData/Src", r#"{"data":{"value":0.25}}"#)
        .expect("enqueue cube"));
    assert!(stage
        .exec_enqueue("/World/Data/CylData/Src", r#"{"data":{"value":0.75}}"#)
        .expect("enqueue cyl"));
    stage.exec_tick().expect("exec_tick with payloads");
    stage.project_topology().expect("reproject");

    let eval = |stage: &StageService, prim_path: &str, attr: &str| -> f64 {
        let prim = stage.prim_at_path(prim_path).unwrap_or_else(|| panic!("{prim_path}"));
        let v = stage
            .eval_attribute(prim, attr, 0.0, true)
            .unwrap_or_else(|_| panic!("eval {prim_path}.{attr}"))
            .unwrap_or_else(|| panic!("{prim_path}.{attr} has a value"));
        parse_f64(&v.value_repr)
    };

    // The CUBE chain reaches both the cube geometry and its panel field.
    assert!((eval(&stage, "/World/Cube", "size") - 0.25).abs() < 1e-9);
    assert!((eval(&stage, "/World/CubePanel/Row/Value", "idtx:widget:value") - 0.25).abs() < 1e-9);
    // The CYLINDER chain reaches both the cylinder geometry and its panel field.
    assert!((eval(&stage, "/World/Column", "height") - 0.75).abs() < 1e-9);
    assert!((eval(&stage, "/World/CylPanel/Row/Value", "idtx:widget:value") - 0.75).abs() < 1e-9);
}

/// AAS computation proof: `Compute_ValueFromAASJson` resolves a `jsonPath` that
/// navigates by AAS `idShort` (descending `submodelElements` / `value` and the
/// OperationVariable `{"value":{…}}` wrapper) — an addressing mode the RFC-6901
/// `Compute_ValueFromJson` cannot express. Mirrors
/// `datasource_compute_chain_registers_only_terminal_consumer`: one terminal
/// value key (`Cube.size`), then an AAS-structured payload is driven through
/// `exec_enqueue` and must land the nested number on `Cube.size`.
#[test]
fn aas_value_from_json_extracts_by_idshort_path() {
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");

    let path = compute_fixture("exec_aas_chain_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_aas_chain_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");

    // Only the terminal consumer `Cube.size` is a value key; the compute node's
    // `inputs:jsonData` connection to the datasource is internal wiring.
    assert_eq!(
        stage.exec_build().expect("exec_build"),
        1,
        "only the terminal Cube.size connection is a value key"
    );

    // Empty datasource ("{}") → the AAS path misses → 0.0, authored over the
    // connected default 1.0. The key assertion: the tick does NOT raise a
    // "Failed to find computation 'outputs:jsonValue:double'" coding error (which
    // is exactly what an unregistered Compute_ValueFromAASJson produced).
    stage
        .exec_tick()
        .expect("exec_tick must not error on the AAS compute chain");

    // Deliver an AAS DeriveSegments-shaped payload the way the REST poller would:
    // outputArguments → OperationVariable wrapper (value.idShort=DerivedSegment)
    // → submodelElements/value → @Record000 → @CPB_CPU0_Temp → value = 42.5.
    let aas_payload = r#"{
        "outputArguments": [
            {
                "value": {
                    "idShort": "DerivedSegment",
                    "value": [
                        {
                            "idShort": "Record000",
                            "value": [
                                { "idShort": "CPB_CPU0_Temp", "value": 42.5 },
                                { "idShort": "CPB_CPU1_Temp", "value": 7.0 }
                            ]
                        }
                    ]
                }
            }
        ]
    }"#;
    assert!(
        stage
            .exec_enqueue("/World/Data/AasSource", aas_payload)
            .expect("exec_enqueue"),
        "enqueue accepted"
    );
    let authored = stage.exec_tick().expect("exec_tick with payload");
    assert_eq!(
        authored, 2,
        "both the datasource outputs:data write and the Cube.size consumer write \
         are authored this tick"
    );

    stage.project_topology().expect("reproject");
    let cube = stage.prim_at_path("/World/Cube").expect("/World/Cube");
    let size = stage
        .eval_attribute(cube, "size", 0.0, true)
        .expect("eval size")
        .expect("size has a value");
    let got = parse_f64(&size.value_repr);
    assert!(
        (got - 42.5).abs() < 1e-9,
        "Cube.size should be the AAS-extracted 42.5, got {got} (repr {:?})",
        size.value_repr
    );
}

#[test]
fn stage_without_compute_graph_registers_no_value_keys() {
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");

    let path = compute_fixture("exec_empty_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_empty_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");
    assert_eq!(stage.exec_build().expect("exec_build"), 0, "no value keys");
    assert_eq!(stage.exec_tick().expect("exec_tick"), 0, "nothing to author");
}

/// End-to-end proof of the Rust-side default `auth:` environment provider:
/// `install()` registers it into the plugin's `EnvironmentProviderRegistry`
/// (via the C-ABI shim + libloading), and a `Compute_Environment` node resolves
/// `auth:TOKEN` / `auth:USERCRED` into non-secret references. The actual header
/// is resolved against each request origin, including after graph evaluation.
#[test]
fn env_auth_provider_resolves_token_and_usercred() {
    // Serialize all `idtx_auth` bucket mutations (see `AUTH_BUCKET_LOCK`).
    let _bucket = AUTH_BUCKET_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // `install()` registers the plugin AND (at the tail of `register_from`) the
    // default `auth:` provider. Calling the public API too is idempotent and
    // documents the explicit entry point.
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");
    idtx_usd_exec::auth::register_default_auth_provider();

    let path = compute_fixture("exec_auth_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_auth_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");
    // Two terminal consumers: TokenSink.string_value and CredSink.string_value.
    assert_eq!(stage.exec_build().expect("exec_build"), 2, "two value keys");

    let read = |stage: &mut StageService, prim_path: &str| -> String {
        stage.project_topology().expect("reproject");
        let prim = stage.prim_at_path(prim_path).expect("sink prim");
        let v = stage
            .eval_attribute(prim, "string_value", 0.0, true)
            .expect("eval string_value")
            .expect("string_value has a value");
        parse_string(&v.value_repr)
    };

    // Graph outputs are references; the matching origin/key resolves at send time.
    idtx_auth::set_bearer_token_for_origins(&["https://api.example.test".into()], "abc123").unwrap();
    stage.exec_tick().expect("exec_tick (bearer)");
    let token_reference = read(&mut stage, "/World/TokenSink");
    assert_eq!(token_reference, "auth:TOKEN");
    assert_eq!(read(&mut stage, "/World/CredSink"), "auth:USERCRED");
    assert_eq!(idtx_auth::authorization_from_input(&token_reference, "https://api.example.test/data").as_deref(), Some("Bearer abc123"));
    assert_eq!(idtx_auth::authorization_from_input(&token_reference, "https://elsewhere.test/data"), None);

    // Replacing a grant changes future lookups without putting the secret into USD.
    idtx_auth::set_user_credentials_for_origins(&["https://api.example.test".into()], "aladdin", "opensesame").unwrap();
    stage.exec_tick().expect("exec_tick (usercred)");
    assert_eq!(
        read(&mut stage, "/World/CredSink"),
        "auth:USERCRED"
    );
    assert_eq!(idtx_auth::resolve_header("USERCRED", "https://api.example.test").as_deref(), Some("Basic YWxhZGRpbjpvcGVuc2VzYW1l"));
    assert_eq!(idtx_auth::authorization_from_input(&token_reference, "https://api.example.test"), None);

    idtx_auth::clear(); // leave the process-global bucket empty
    assert_eq!(idtx_auth::authorization_from_input("auth:USERCRED", "https://api.example.test"), None);
}

/// REST datasource INPUT resolution: a `RestDatasource` whose `authorization` is
/// driven by a `Compute_Environment(auth:TOKEN)` node must (1) have its `method`
/// read as the authored `token` `POST` (the schema types `method` as `token`, so
/// the old `Get(std::string*)` read silently fell back to `GET` — polling an
/// `/invoke` endpoint as GET → HTTP 405); (2) be registered as a value key in
/// `build_program` even though a datasource prim is neither xformable nor a
/// ComputeValueSink; and (3) surface the compute-graph-resolved Authorization via
/// `exec_data_source_inputs` WITHOUT authoring it onto the stage — the mechanism
/// that lets an env-provider token reach the REST request.
#[test]
fn rest_datasource_input_connection_registers_and_resolves() {
    // Serialize all `idtx_auth` bucket mutations (see `AUTH_BUCKET_LOCK`).
    let _bucket = AUTH_BUCKET_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");
    idtx_usd_exec::auth::register_default_auth_provider();

    let path = compute_fixture("exec_rest_input_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_rest_input_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");

    // (1) `method` is a USD `token`; it must read as the authored "POST", not the
    // "GET" fallback. `authorization` reads empty here: it carries a connection,
    // and the raw `data_sources()` read does not follow connections — which is
    // exactly why the computed value travels the input side channel below.
    let sources = stage.exec_data_sources().expect("exec_data_sources");
    let sensor = sources
        .iter()
        .find(|s| s.prim_path == "/World/Sensor")
        .expect("Sensor datasource present");
    assert_eq!(sensor.method, "POST", "token-typed method reads as POST");
    assert_eq!(sensor.kind, "rest");
    assert_eq!(sensor.authorization, "", "connected authorization not read raw");

    // (2) The datasource's connected `authorization` is the sole value key.
    assert_eq!(
        stage.exec_build().expect("exec_build"),
        1,
        "datasource input authorization registered as a value key"
    );

    // Before any compute (and before a token is stored) there is no resolved
    // input value yet.
    assert!(
        stage
            .exec_data_source_inputs()
            .expect("inputs")
            .is_empty(),
        "no resolved input before first tick"
    );

    // (3) Store a bearer token, compute, and read the resolved Authorization off
    // the input side channel — it must NOT have been authored onto the prim.
    let origin = idtx_auth::Origin::from_url(&sensor.endpoint).unwrap().to_string();
    idtx_auth::set_bearer_token_for_origins(&[origin], "abc123").unwrap();
    stage.exec_tick().expect("exec_tick");

    let inputs = stage.exec_data_source_inputs().expect("inputs");
    let auth = inputs
        .iter()
        .find(|i| i.prim_path == "/World/Sensor" && i.attribute_name == "authorization")
        .expect("authorization input resolved");
    assert_eq!(auth.value, "auth:TOKEN");
    assert_eq!(idtx_auth::authorization_from_input(&auth.value, &sensor.endpoint).as_deref(), Some("Bearer abc123"));
    assert_eq!(idtx_auth::authorization_from_input(&auth.value, "https://unrelated.test"), None);

    // The resolved token must stay OFF the stage (no session-layer authoring of a
    // secret): reading the attribute back still yields the authored empty default.
    stage.project_topology().expect("reproject");
    let prim = stage.prim_at_path("/World/Sensor").expect("sensor prim");
    let authored = stage
        .eval_attribute(prim, "authorization", 0.0, true)
        .expect("eval authorization")
        .expect("authorization has a value");
    assert_eq!(
        parse_string(&authored.value_repr),
        "",
        "resolved token must not be authored onto the stage"
    );

    idtx_auth::clear();
}


/// End-to-end proof of the Rust-side default `idcm:` environment provider:
/// `install()` registers it (via the same C-ABI shim + libloading path as
/// `auth:`), and `Compute_Environment` nodes resolve `idcm:CURRENT_TIME`,
/// `idcm:CURRENT_TIME_LESS5`, and `idcm:INTERVAL` into values authored onto
/// `ComputeValueSink`s. Unlike the static auth bucket these are computed fresh
/// on every compute, so we assert shape/relationship rather than a fixed value.
#[test]
fn env_idcm_provider_resolves_dynamic_vars() {
    idtx_usd_exec::install().expect("register IDTX OpenExec plugin");
    idtx_usd_exec::idcm::register_default_idcm_provider();

    let path = compute_fixture("exec_idcm_scene.usda");
    if !path.exists() {
        eprintln!("skip: exec_idcm_scene.usda not found");
        return;
    }

    let mut stage = StageService::open_with_session(&path, "", true).expect("open stage");
    stage.project_topology().expect("project topology");
    // Three terminal consumers: TimeSink, TimeLess5Sink, IntervalSink.
    assert_eq!(stage.exec_build().expect("exec_build"), 3, "three value keys");

    let read = |stage: &mut StageService, prim_path: &str| -> String {
        stage.project_topology().expect("reproject");
        let prim = stage.prim_at_path(prim_path).expect("sink prim");
        let v = stage
            .eval_attribute(prim, "string_value", 0.0, true)
            .expect("eval string_value")
            .expect("string_value has a value");
        parse_string(&v.value_repr)
    };

    stage.exec_tick().expect("exec_tick (idcm)");

    // INTERVAL is the fixed literal.
    assert_eq!(read(&mut stage, "/World/IntervalSink"), "24h");

    // CURRENT_TIME is an ISO-8601 UTC instant: `YYYY-MM-DDTHH:MM:SSZ` (20 chars).
    let now = read(&mut stage, "/World/TimeSink");
    assert_eq!(now.len(), 20, "ISO-8601 UTC is 20 chars, got {now:?}");
    assert!(now.ends_with('Z'), "UTC designator missing in {now:?}");
    assert_eq!(now.as_bytes()[4], b'-', "date separator in {now:?}");
    assert_eq!(now.as_bytes()[10], b'T', "date/time separator in {now:?}");

    // CURRENT_TIME_LESS5 is the same shape and strictly earlier than CURRENT_TIME
    // (lexicographic compare is chronological for this fixed-width format).
    let less5 = read(&mut stage, "/World/TimeLess5Sink");
    assert_eq!(less5.len(), 20, "less5 ISO-8601 UTC is 20 chars, got {less5:?}");
    assert!(less5 < now, "CURRENT_TIME_LESS5 ({less5}) must precede CURRENT_TIME ({now})");
}
