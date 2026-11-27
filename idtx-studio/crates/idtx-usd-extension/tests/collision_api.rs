//! Integration tests for the codeless `IdtxCollisionAPI` schema.
//!
//! These require a linked `OpenUSD` (`IDTX_USD_DIR`) and the runtime libs on the
//! loader path (`DYLD_LIBRARY_PATH` on macOS), so they are `#[ignore]` and run
//! explicitly:
//!
//! ```bash
//! export IDTX_USD_DIR=.../openusd-26.11.1/install
//! export DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib:$DYLD_LIBRARY_PATH
//! cargo test -p idtx-usd-extension -- --ignored
//! ```

use std::path::{Path, PathBuf};

use idtx_usd::{StageService, StructuralCommand};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/schema/idtx_collision_api.usda")
}

/// Separate layer for the mutating apply/remove test. `OpenUSD` caches stages by
/// root-layer identifier, so a test that authors on the shared read-only fixture
/// would race the detection test under parallel `cargo test`.
fn apply_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/schema/idtx_collision_apply.usda")
}

/// Separate layer for the mutating channel round-trip test (see `apply_fixture`).
fn channel_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/schema/idtx_collision_channel.usda")
}

/// Register the schema plugin, open the fixture, and confirm the registry
/// recognises both the standard `PhysicsCollisionAPI` gate and the reduced
/// `IdtxCollisionAPI` addendum on the prims that apply them.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn collision_api_is_registered_and_detected() {
    use idtx_usd_extension::{COLLISION_SCHEMA, PHYSICS_COLLISION_SCHEMA};

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let f = fixture();
    assert!(f.exists(), "fixture missing: {}", f.display());
    let mut stage = StageService::open(&f).expect("open collision fixture");
    stage.project_topology().expect("topology");

    let ball = stage.prim_at_path("/World/Ball").expect("ball prim");
    let ramp = stage.prim_at_path("/World/Ramp").expect("ramp prim");
    let crate_ = stage.prim_at_path("/World/Crate").expect("crate prim");

    // The collidable gate is the OpenUSD-standard PhysicsCollisionAPI.
    assert!(
        stage
            .prim_has_api(ball, PHYSICS_COLLISION_SCHEMA)
            .expect("prim_has_api ball"),
        "Ball must have PhysicsCollisionAPI (usdPhysics must be registered)"
    );
    assert!(
        stage
            .prim_has_api(ramp, PHYSICS_COLLISION_SCHEMA)
            .expect("prim_has_api ramp"),
        "Ramp must have PhysicsCollisionAPI"
    );
    assert!(
        !stage
            .prim_has_api(crate_, PHYSICS_COLLISION_SCHEMA)
            .expect("prim_has_api crate"),
        "Crate must NOT have PhysicsCollisionAPI"
    );
    // The reduced IDTX addendum is dual-authored on the hull/channel prims.
    assert!(
        stage
            .prim_has_api(ramp, COLLISION_SCHEMA)
            .expect("prim_has_api ramp idtx"),
        "Ramp must also carry the reduced IdtxCollisionAPI addendum"
    );

    // apiSchemas metadata must list both applied schemas.
    let props = stage.inspect_prim(ball).expect("inspect ball");
    let api_summary = props
        .iter()
        .find(|p| p.name == "apiSchemas")
        .map(|p| p.value_summary.clone())
        .unwrap_or_default();
    assert!(
        api_summary.contains(PHYSICS_COLLISION_SCHEMA),
        "apiSchemas must list PhysicsCollisionAPI; got {api_summary:?}"
    );
}

/// The standard `physics:*` attributes evaluate with the expected USD types,
/// and the reduced-addendum `idtx:collision:*` arrays/tokens roundtrip through
/// [`idtx_usd_extension`] parsing.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn collision_attributes_evaluate_with_expected_types() {
    use idtx_usd_extension::{
        Approximation, CollisionChannel, ATTR_CHANNEL, ATTR_HULL_POINTS, ATTR_HULL_TRIANGLES,
        ATTR_PHYSICS_APPROXIMATION, ATTR_PHYSICS_COLLISION_ENABLED,
    };

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(fixture()).expect("open collision fixture");
    stage.project_topology().expect("topology");
    let ramp = stage.prim_at_path("/World/Ramp").expect("ramp prim");
    let shield = stage.prim_at_path("/World/Shield").expect("shield prim");

    // Ramp: physics:approximation = token "none" (collide against the mesh).
    let approx = stage
        .eval_attribute(ramp, ATTR_PHYSICS_APPROXIMATION, 0.0, true)
        .expect("eval approximation")
        .expect("approximation authored");
    assert_eq!(approx.type_name, "token");
    assert!(
        approx.value_repr.contains(Approximation::None.as_token()),
        "approximation repr should contain 'none'; got {:?}",
        approx.value_repr
    );

    // Ramp: physics:collisionEnabled = bool true.
    let enabled = stage
        .eval_attribute(ramp, ATTR_PHYSICS_COLLISION_ENABLED, 0.0, true)
        .expect("eval collisionEnabled")
        .expect("collisionEnabled authored");
    assert_eq!(enabled.type_name, "bool");
    assert!(enabled.value_repr.contains('1') || enabled.value_repr.contains("true"));

    // Ramp: channel = token[] containing Physics + Interaction.
    let channel = stage
        .eval_attribute(ramp, ATTR_CHANNEL, 0.0, true)
        .expect("eval channel")
        .expect("channel authored");
    assert_eq!(channel.type_name, "token[]");
    assert!(channel.value_repr.contains(CollisionChannel::Physics.as_token()));
    assert!(channel
        .value_repr
        .contains(CollisionChannel::Interaction.as_token()));

    // Shield: physics:approximation = convexHull, with hull:points (point3f[]) +
    // hull:triangles (int[]) on the reduced addendum.
    let shield_approx = stage
        .eval_attribute(shield, ATTR_PHYSICS_APPROXIMATION, 0.0, true)
        .expect("eval shield approximation")
        .expect("shield approximation authored");
    assert!(shield_approx.value_repr.contains(Approximation::ConvexHull.as_token()));

    let points = stage
        .eval_attribute(shield, ATTR_HULL_POINTS, 0.0, true)
        .expect("eval hull:points")
        .expect("hull:points authored");
    assert_eq!(points.type_name, "point3f[]");
    assert!(!points.value_repr.is_empty());

    let triangles = stage
        .eval_attribute(shield, ATTR_HULL_TRIANGLES, 0.0, true)
        .expect("eval hull:triangles")
        .expect("hull:triangles authored");
    assert_eq!(triangles.type_name, "int[]");
    assert!(!triangles.value_repr.is_empty());
}

/// The typed array bridge readers return numeric collision geometry (not display
/// strings) for the hull points, triangle indices and channel tokens — the data
/// the ECS hydrates into `CollisionComp`.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn collision_arrays_read_as_typed_values() {
    use idtx_usd_extension::{ATTR_CHANNEL, ATTR_HULL_POINTS, ATTR_HULL_TRIANGLES};

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(fixture()).expect("open collision fixture");
    stage.project_topology().expect("topology");
    let shield = stage.prim_at_path("/World/Shield").expect("shield prim");
    let ramp = stage.prim_at_path("/World/Ramp").expect("ramp prim");

    // hull:points — 4 tetrahedron verts (12 floats → 4 × [f32;3]).
    let points = stage
        .eval_point3f_array(shield, ATTR_HULL_POINTS, 0.0, true)
        .expect("read hull:points")
        .expect("hull:points present");
    assert_eq!(points.len(), 4, "tetra hull has 4 points");
    assert_eq!(points[0], [0.0, 0.0, 0.0]);
    assert_eq!(points[1], [1.0, 0.0, 0.0]);

    // hull:triangles — 4 faces × 3 indices.
    let tris = stage
        .eval_int_array(shield, ATTR_HULL_TRIANGLES, 0.0, true)
        .expect("read hull:triangles");
    assert_eq!(tris.len(), 12, "4 triangles × 3 indices");
    assert_eq!(&tris[0..3], &[0, 1, 2]);

    // channel — token[] read as owned strings.
    let shield_channels = stage
        .eval_token_array(shield, ATTR_CHANNEL, 0.0, true)
        .expect("read shield channel");
    assert_eq!(shield_channels, vec!["All".to_owned()]);

    let ramp_channels = stage
        .eval_token_array(ramp, ATTR_CHANNEL, 0.0, true)
        .expect("read ramp channel");
    assert_eq!(
        ramp_channels,
        vec!["Physics".to_owned(), "Interaction".to_owned()]
    );

    // A prim without the hull arrays yields an empty (not errored) read.
    let empty = stage
        .eval_point3f_array(ramp, ATTR_HULL_POINTS, 0.0, true)
        .expect("read missing hull:points");
    assert!(empty.unwrap_or_default().is_empty());
}

/// The standard `PhysicsCollisionAPI` and the reduced `IdtxCollisionAPI` can be
/// applied to a prim that lacks them, then symmetrically removed — the backend
/// for the atomic "add collider" authoring action.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn collision_api_applies_and_removes() {
    use idtx_usd_extension::{COLLISION_SCHEMA, PHYSICS_COLLISION_SCHEMA};

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(apply_fixture()).expect("open collision apply fixture");
    stage.project_topology().expect("topology");
    let crate_ = stage.prim_at_path("/World/Crate").expect("crate prim");
    assert!(
        !stage
            .prim_has_api(crate_, PHYSICS_COLLISION_SCHEMA)
            .expect("prim_has_api before apply"),
        "Crate must start without PhysicsCollisionAPI"
    );

    for schema in [PHYSICS_COLLISION_SCHEMA, COLLISION_SCHEMA] {
        stage
            .apply_structural_command(&StructuralCommand::ApplyApi {
                path: "/World/Crate".to_owned(),
                schema: schema.to_owned(),
            })
            .unwrap_or_else(|e| panic!("apply {schema}: {e:?}"));
        assert!(
            stage
                .prim_has_api(crate_, schema)
                .unwrap_or_else(|e| panic!("prim_has_api after apply {schema}: {e:?}")),
            "{schema} must be applied"
        );
    }

    for schema in [COLLISION_SCHEMA, PHYSICS_COLLISION_SCHEMA] {
        stage
            .apply_structural_command(&StructuralCommand::RemoveApi {
                path: "/World/Crate".to_owned(),
                schema: schema.to_owned(),
            })
            .unwrap_or_else(|e| panic!("remove {schema}: {e:?}"));
        assert!(
            !stage
                .prim_has_api(crate_, schema)
                .unwrap_or_else(|e| panic!("prim_has_api after remove {schema}: {e:?}")),
            "{schema} must be removed"
        );
    }
}

/// Authoring a `token[]` collision channel through the `SetProperty::TokenArray`
/// path round-trips back through the typed `eval_token_array` reader — the write
/// side used by the inspector channel checkboxes.
#[test]
#[ignore = "requires USD link (IDTX_USD_DIR + DYLD_LIBRARY_PATH)"]
fn collision_channel_token_array_authoring_roundtrips() {
    use idtx_usd::PropertyValue;
    use idtx_usd_extension::ATTR_CHANNEL;

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let mut stage = StageService::open(channel_fixture()).expect("open collision channel fixture");
    stage.project_topology().expect("topology");
    let crate_ = stage.prim_at_path("/World/Crate").expect("crate prim");

    stage
        .apply_structural_command(&StructuralCommand::ApplyApi {
            path: "/World/Crate".to_owned(),
            schema: idtx_usd_extension::COLLISION_SCHEMA.to_owned(),
        })
        .expect("apply IdtxCollisionAPI");

    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: crate_,
            property: ATTR_CHANNEL.to_owned(),
            value: PropertyValue::TokenArray(vec![
                "Physics".to_owned(),
                "Interaction".to_owned(),
            ]),
        })
        .expect("author channel token[]");

    let channels = stage
        .eval_token_array(crate_, ATTR_CHANNEL, 0.0, true)
        .expect("read channel back");
    assert_eq!(
        channels,
        vec!["Physics".to_owned(), "Interaction".to_owned()]
    );

    // Authoring an empty list clears the channels.
    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: crate_,
            property: ATTR_CHANNEL.to_owned(),
            value: PropertyValue::TokenArray(Vec::new()),
        })
        .expect("author empty channel token[]");
    let cleared = stage
        .eval_token_array(crate_, ATTR_CHANNEL, 0.0, true)
        .expect("read cleared channel");
    assert!(cleared.is_empty(), "empty token[] authored");
}
