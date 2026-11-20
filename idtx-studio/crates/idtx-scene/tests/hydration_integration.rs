#![cfg(feature = "usd-extension")]
//! Integration tests for `SceneHydrator` that require a live `OpenUSD` link.
//!
//! The whole file exercises the `usd-extension` schema domains (collision / HUD
//! / widgets) and `idtx_usd_extension::install`, so it is gated on that feature;
//! under `--no-default-features` the file compiles to nothing.
//!
//! All tests are `#[ignore]` by default — run them with:
//!
//! ```sh
//! export IDTX_USD_DIR=/path/to/openusd/install
//! # Copy or symlink the USD plugin folder next to the test binary first, e.g.:
//! #   ln -s $IDTX_USD_DIR/lib/usd target/debug/deps/usd
//! cargo test -p idtx-scene -- --include-ignored
//! ```
//!
//! The tests use the staged USD files in `tests/fixtures/usd/` relative to the
//! workspace root.

use std::path::Path;

use idtx_scene::hydrator::triangulate_mesh;
#[cfg(feature = "behavior")]
use idtx_scene::BehaviorRef;
use idtx_scene::{PayloadState, PopulatorRegistry, PrimNode, SceneHydrator};
#[cfg(feature = "behavior")]
use idtx_usd::{PropertyValue, StructuralCommand};
use idtx_usd::{DirtyDomains, StageService};

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("workspace root")
        .to_path_buf()
}

fn usd_path(filename: &str) -> std::path::PathBuf {
    workspace_root().join("tests/fixtures/usd").join(filename)
}

/// Copy a read-only fixture to a uniquely-named temp file so authoring tests
/// stay isolated. `OpenUSD` caches on-disk layers process-globally by identifier,
/// so two tests authoring into the *same* file would pollute each other's
/// (in-memory) root layer — a distinct temp path gives each its own layer.
fn temp_fixture_copy(filename: &str, unique: &str) -> std::path::PathBuf {
    let src = usd_path(filename);
    // `filename` may carry a subdir (e.g. "behavior/foo.usda"); flatten the
    // separators so the temp destination is a single flat file whose parent
    // (the temp dir) already exists — `fs::copy` does not create parent dirs.
    let flat = filename.replace(['/', '\\'], "_");
    let dst = std::env::temp_dir().join(format!("idtx_scene_{unique}_{flat}"));
    std::fs::copy(&src, &dst).expect("copy fixture to temp");
    dst
}

// ── scene_hydration.usda ──────────────────────────────────────────────────────

#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_scene_topology() {
    let path = usd_path("hydration/scene_hydration.usda");
    let mut service = StageService::open(&path).expect("open stage");
    let delta = service.project_topology().unwrap().to_vec();
    assert!(
        !delta.is_empty(),
        "stage should contain at least the pseudo-root"
    );

    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(
        &idtx_usd::TopologyDelta {
            upserts: delta.clone(),
            removed: Default::default(),
        },
        &service,
    );
    // Every topology record should have spawned an entity.
    let world = hydrator.world_mut();
    let prim_count = world.query::<&PrimNode>().iter(world).count();
    assert_eq!(
        prim_count,
        delta.len(),
        "entity count matches topology records"
    );
}

#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_scene_viewport() {
    let path = usd_path("hydration/scene_hydration.usda");
    let mut service = StageService::open(&path).expect("open stage");

    // Full topology first.
    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);

    // Then viewport projection.
    let projection = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&projection)
        .expect("apply viewport");

    let world = hydrator.world_mut();

    // Ground mesh should have a MeshComp.
    let mesh_count = world.query::<&idtx_scene::MeshComp>().iter(world).count();
    assert!(mesh_count >= 1, "at least one mesh should be hydrated");

    // At least one light.
    let light_count = world.query::<&idtx_scene::LightComp>().iter(world).count();
    assert!(light_count >= 1, "at least one light should be hydrated");

    // The camera should be hydrated.
    let cam_count = world.query::<&idtx_scene::CameraComp>().iter(world).count();
    assert!(cam_count >= 1, "camera should be hydrated");
}

#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn transforms_are_correct_type() {
    let path = usd_path("hydration/scene_hydration.usda");
    let mut service = StageService::open(&path).expect("open stage");
    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);

    let projection = service.project_viewport().expect("project viewport");
    hydrator.apply_viewport_projection(&projection).unwrap();

    let world = hydrator.world_mut();
    let transform_count = world
        .query::<&idtx_scene::WorldTransform>()
        .iter(world)
        .count();
    assert!(
        transform_count >= 1,
        "at least one transform should be hydrated"
    );
}

// ── payload_outer.usda ───────────────────────────────────────────────────────

#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn payload_opens_with_topology_only() {
    let path = usd_path("hydration/payload_outer.usda");
    let mut service = StageService::open_topology_only(&path).expect("open stage topology-only");

    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);

    let world = hydrator.world_mut();

    // Payload boundary prims should have PayloadState with loaded = false.
    let unloaded: Vec<_> = world
        .query::<(&PrimNode, &PayloadState)>()
        .iter(world)
        .filter(|(_, ps)| !ps.loaded)
        .collect();
    assert!(
        !unloaded.is_empty(),
        "at least one unloaded payload should be present"
    );
}

#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn payload_load_unload_cycle() {
    let path = usd_path("hydration/payload_outer.usda");
    let mut service = StageService::open_topology_only(&path).expect("open stage topology-only");
    let _ = service.project_topology().unwrap();

    // Find the first payload prim path.
    let payload_path = service
        .topology()
        .iter()
        .find(|r| {
            r.packed_arc
                .as_ref()
                .is_some_and(|a| a.kind == idtx_usd::PackedArcKind::Payload)
        })
        .and_then(|r| service.path(r.path))
        .map(str::to_owned)
        .expect("payload prim in outer stage");

    let prim_id = service
        .prim_at_path(&payload_path)
        .expect("resolve payload prim");

    // Load it.
    service
        .set_payload_loaded(prim_id, true)
        .expect("load payload");

    // Drain changes and reproject the affected subtree.
    let batch = service.drain_changes().expect("drain changes");
    assert!(batch.is_some(), "loading a payload should emit changes");
    if let Some(batch) = batch {
        assert!(
            batch
                .changes
                .iter()
                .any(|c| c.domains.contains(DirtyDomains::TOPOLOGY)),
            "topology domain should be dirty after payload load"
        );
    }

    // Unload it again.
    service
        .set_payload_loaded(prim_id, false)
        .expect("unload payload");
    let batch2 = service.drain_changes().expect("drain changes after unload");
    assert!(batch2.is_some(), "unloading a payload should emit changes");
}

// ── packed_override_test.usda ────────────────────────────────────────────────

/// Verifies that `local_layer_identifier` is set correctly for prims inside
/// referenced assets, distinguishing authored overrides from composed-only prims.
/// This is the data layer that drives the gray/white color logic in the scene tree.
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn packed_override_local_authorship() {
    let path = usd_path("hydration/packed_override_test.usda");
    let mut service = StageService::open(&path).expect("open stage");
    let root_id = service.metadata().root_identifier.clone();

    let topo = service.project_topology().unwrap();
    let records: Vec<_> = topo.to_vec();

    // Helper: find local_layer_identifier for a given USD path.
    let layer_id_for = |prim_path: &str| -> Option<String> {
        records
            .iter()
            .find(|r| service.path(r.path) == Some(prim_path))
            .and_then(|r| r.local_layer_identifier.clone())
    };
    let is_root_authored =
        |prim_path: &str| -> bool { layer_id_for(prim_path).as_deref() == Some(root_id.as_str()) };

    // Case 2: PackedNoOverride — no overrides; all inner prims have no root opinion.
    assert!(
        !is_root_authored("/Scene/PackedNoOverride/Geom"),
        "PackedNoOverride/Geom must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedNoOverride/Geom/Top"),
        "PackedNoOverride/Geom/Top must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedNoOverride/Geom/Legs"),
        "PackedNoOverride/Geom/Legs must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedNoOverride/Geom/Legs/Leg1"),
        "PackedNoOverride/Geom/Legs/Leg1 must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedNoOverride/Geom/Edge"),
        "PackedNoOverride/Geom/Edge must not be root-authored"
    );

    // Case 3: PackedDirectOverride — Geom has an over; its children do not.
    assert!(
        is_root_authored("/Scene/PackedDirectOverride/Geom"),
        "PackedDirectOverride/Geom must be root-authored (has over)"
    );
    assert!(
        !is_root_authored("/Scene/PackedDirectOverride/Geom/Top"),
        "PackedDirectOverride/Geom/Top must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedDirectOverride/Geom/Legs"),
        "PackedDirectOverride/Geom/Legs must not be root-authored"
    );

    // Case 4: PackedDeepOverride — Geom/Legs/Leg1 authored; Leg2/Leg3/Leg4 not.
    assert!(
        is_root_authored("/Scene/PackedDeepOverride/Geom"),
        "PackedDeepOverride/Geom must be root-authored (has over)"
    );
    assert!(
        is_root_authored("/Scene/PackedDeepOverride/Geom/Legs"),
        "PackedDeepOverride/Geom/Legs must be root-authored (has over)"
    );
    assert!(
        is_root_authored("/Scene/PackedDeepOverride/Geom/Legs/Leg1"),
        "PackedDeepOverride/Geom/Legs/Leg1 must be root-authored (has over)"
    );
    assert!(
        !is_root_authored("/Scene/PackedDeepOverride/Geom/Legs/Leg2"),
        "PackedDeepOverride/Geom/Legs/Leg2 must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedDeepOverride/Geom/Top"),
        "PackedDeepOverride/Geom/Top must not be root-authored"
    );
    assert!(
        !is_root_authored("/Scene/PackedDeepOverride/Geom/Edge"),
        "PackedDeepOverride/Geom/Edge must not be root-authored"
    );
}

// ── authored_visibility.usda — seeding-order fix ────────────────────────────

/// Locks the visibility seeding-order fix (see
/// `docs/xr-controller-hydration-plan.md` §4).
///
/// `apply_viewport_projection` used to pre-seed a `VisibilityOverride::default()`
/// (`Inherited`) for every imageable, which made `initialize_visibility_overrides`
/// *skip* them all — so an authored-`invisible` prim resolved to visible and
/// rendered anyway (the XR controller proxy double-drew at the origin). The fix
/// stops the pre-seed so the override is derived from the authored USD state.
///
/// This asserts the derived effective visibility (`VisibilityComp::visible`)
/// after the full hydrate → project → initialize sequence:
///   * an authored-visible sibling resolves visible,
///   * an authored-`invisible` prim resolves invisible,
///   * a child with no opinion under an invisible parent inherits invisibility.
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn authored_invisibility_honored_on_hydration() {
    use idtx_scene::VisibilityComp;

    let path = usd_path("hydration/authored_visibility.usda");
    let mut service = StageService::open(&path).expect("open stage");

    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);

    let projection = service.project_viewport().expect("project viewport");
    hydrator
        .apply_viewport_projection(&projection)
        .expect("apply viewport");

    // Derive overrides from authored state — the seam under test.
    hydrator.initialize_visibility_overrides(&service);

    let effective = |p: &str| -> bool {
        let entity = hydrator
            .entity_for_path(p)
            .unwrap_or_else(|| panic!("no entity for {p}"));
        hydrator
            .world()
            .get::<VisibilityComp>(entity)
            .unwrap_or_else(|| panic!("no VisibilityComp for {p}"))
            .visible
    };

    assert!(
        effective("/World/VisibleSphere"),
        "authored-visible sibling must resolve visible"
    );
    assert!(
        !effective("/World/InvisibleSphere"),
        "authored-invisible prim must resolve invisible (seeding-order fix)"
    );
    assert!(
        !effective("/World/InvisibleParent/InheritedChild"),
        "child under an invisible parent must inherit invisibility"
    );
}

// ── behavior attachment hydration ────────────────────────────────────────────

/// Round-trips the authored-behavior write + read path: author an
/// `idtx:behavior:module` attachment via the structural command, then hydrate it into
/// a `BehaviorRef` and confirm the module asset survives the strip of USD's
/// `@…@` asset value-repr delimiters.
#[cfg(feature = "behavior")]
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_authored_behavior_attachment() {
    let path = usd_path("hydration/scene_hydration.usda");
    let mut service = StageService::open(&path).expect("open stage");

    let topo = service.project_topology().unwrap().to_vec();
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(
        &idtx_usd::TopologyDelta {
            upserts: topo,
            removed: Default::default(),
        },
        &service,
    );

    let target = "/World/Objects/Box";
    let asset = "behaviors/spin.wasm";

    // Nothing authored yet → the scan hydrates zero behaviors.
    assert_eq!(hydrator.hydrate_authored_behaviors(&service), 0);

    // Author the attachment (write path: SetProperty of idtx:behavior:module).
    let prim = service.prim_at_path(target).expect("target prim exists");
    service
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: idtx_scene::behavior::attr::MODULE.to_owned(),
            value: PropertyValue::Asset(asset.to_owned()),
        })
        .expect("attach behavior");

    // Read path: exactly one behavior hydrates onto the target entity.
    assert_eq!(hydrator.hydrate_authored_behaviors(&service), 1);

    let entity = hydrator.entity_for_path(target).expect("entity for target");
    let bref = hydrator
        .world()
        .get::<BehaviorRef>(entity)
        .expect("BehaviorRef hydrated onto target");
    assert_eq!(bref.module_asset.as_deref(), Some(asset));
    assert!(bref.enabled, "hydrated behavior defaults to enabled");
    assert_eq!(
        bref.kind,
        BehaviorRef::new("_").kind,
        "hydrated behavior defaults to the Runtime kind"
    );

    // Re-scanning is idempotent: the existing ref is preserved, none added.
    assert_eq!(hydrator.hydrate_authored_behaviors(&service), 0);
}

/// Reconcile semantics — the *authoritative* resync structural undo/redo
/// relies on (`restore_root_layer_snapshot`). Proves reconcile ADDS an authored
/// ref where `idtx:behavior:module` appears, REMOVES it when the attribute is restored
/// away (undo of an attach), and NEVER disturbs a host-embedded ref
/// (`module_asset == None`, e.g. the XR-preview HMD→camera sync behavior).
#[cfg(feature = "behavior")]
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn reconcile_authored_behaviors_add_remove_and_preserve_embedded() {
    let path = temp_fixture_copy("hydration/scene_hydration.usda", "reconcile_behaviors");
    let mut service = StageService::open(&path).expect("open stage");
    let topo = service.project_topology().unwrap().to_vec();
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(
        &idtx_usd::TopologyDelta {
            upserts: topo,
            removed: Default::default(),
        },
        &service,
    );

    let target = "/World/Objects/Box";
    let other = "/World/Objects/Ball";
    let asset = "behaviors/spin.wasm";

    // Pin a host-embedded ref (no authored USD backing) on an unrelated entity.
    let embedded_entity = hydrator.entity_for_path(other).expect("entity for other");
    hydrator
        .world_mut()
        .entity_mut(embedded_entity)
        .insert(BehaviorRef::new("idtx-embedded-0000"));

    // Snapshot the pristine root layer (no attachment) for the "undo" step.
    let before = service.root_layer_text().expect("root layer text");

    // Attach → reconcile ADDS exactly one authored ref onto the target.
    let prim = service.prim_at_path(target).expect("target prim exists");
    service
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: idtx_scene::behavior::attr::MODULE.to_owned(),
            value: PropertyValue::Asset(asset.to_owned()),
        })
        .expect("attach behavior");
    assert_eq!(hydrator.reconcile_authored_behaviors(&service), 1);
    let target_entity = hydrator.entity_for_path(target).expect("entity for target");
    assert_eq!(
        hydrator
            .world()
            .get::<BehaviorRef>(target_entity)
            .and_then(|b| b.module_asset.as_deref()),
        Some(asset),
        "reconcile adds an authored ref carrying the module asset",
    );

    // Undo the attach by restoring the pre-attach root layer → reconcile REMOVES
    // the authored ref, but the embedded ref on the unrelated entity survives.
    service
        .replace_root_layer_text(&before)
        .expect("restore root layer");
    assert_eq!(hydrator.reconcile_authored_behaviors(&service), 1);
    assert!(
        hydrator.world().get::<BehaviorRef>(target_entity).is_none(),
        "authored ref removed when its backing attribute is restored away",
    );
    assert!(
        hydrator
            .world()
            .get::<BehaviorRef>(embedded_entity)
            .is_some(),
        "host-embedded ref (module_asset == None) must never be reconciled away",
    );
}

/// Per-attachment override read round-trip: author
/// `idtx:behavior:enabled` / `:kind` / `:scope` alongside the attachment and
/// confirm both `hydrate_authored_behaviors` and `reconcile_authored_behaviors`
/// layer them onto the `BehaviorRef` (defaults otherwise).
#[cfg(feature = "behavior")]
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn behavior_overrides_round_trip_into_behavior_ref() {
    use idtx_scene::behavior::attr;
    use idtx_scene::{BehaviorKind, BehaviorScope};
    use idtx_usd::PropertyValue;

    let path = temp_fixture_copy("hydration/scene_hydration.usda", "behavior_overrides");
    let mut service = StageService::open(&path).expect("open stage");
    let topo = service.project_topology().unwrap().to_vec();
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(
        &idtx_usd::TopologyDelta {
            upserts: topo,
            removed: Default::default(),
        },
        &service,
    );

    let target = "/World/Objects/Box";
    let prim = service.prim_at_path(target).expect("target prim exists");
    service
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim,
            property: attr::MODULE.to_owned(),
            value: PropertyValue::Asset("behaviors/spin.wasm".to_owned()),
        })
        .expect("attach behavior");
    // Author the overrides: disabled, editor kind, stage scope.
    for (property, value) in [
        (attr::ENABLED, PropertyValue::Bool(false)),
        (
            attr::KIND,
            PropertyValue::Token(BehaviorKind::Editor.as_token().to_owned()),
        ),
        (
            attr::SCOPE,
            PropertyValue::Token(BehaviorScope::Stage.as_token().to_owned()),
        ),
    ] {
        service
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim,
                property: property.to_owned(),
                value,
            })
            .expect("author override");
    }

    // A fresh hydrate reads the overrides onto the ref.
    assert_eq!(hydrator.hydrate_authored_behaviors(&service), 1);
    let entity = hydrator.entity_for_path(target).expect("entity for target");
    {
        let bref = hydrator
            .world()
            .get::<BehaviorRef>(entity)
            .expect("ref hydrated");
        assert!(!bref.enabled, "authored enabled=false honored");
        assert_eq!(bref.kind, BehaviorKind::Editor, "authored kind honored");
        assert_eq!(
            bref.stage_scope,
            BehaviorScope::Stage,
            "authored scope honored"
        );
    }

    // Reconcile rebuilds from the same attributes → identical overrides.
    hydrator.reconcile_authored_behaviors(&service);
    let bref = hydrator
        .world()
        .get::<BehaviorRef>(entity)
        .expect("ref present");
    assert!(!bref.enabled);
    assert_eq!(bref.kind, BehaviorKind::Editor);
    assert_eq!(bref.stage_scope, BehaviorScope::Stage);
}

// ── triangulate_mesh unit test (no USD link required) ────────────────────────

#[test]
fn triangulate_ngon_stress() {
    // 6-sided polygon (hexagon).
    let points: Vec<[f32; 3]> = (0..6_u8)
        .map(|i| {
            let a = std::f32::consts::TAU * f32::from(i) / 6.0;
            [a.cos(), a.sin(), 0.0]
        })
        .collect();
    let indices: Vec<i32> = (0..6_i32).collect();
    let (_pos, _uv, idx) = triangulate_mesh(&points, &[], &[6], &indices);
    assert_eq!(idx.len(), (6 - 2) * 3); // 4 triangles = 12 indices
}

// ── idtx_collision_api.usda (IdtxCollisionAPI hydration) ───────────────────────

#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_collision_components_from_schema() {
    use idtx_scene::{CollisionChannel, CollisionComp, CollisionShape};

    // The schema plugin must be registered before the stage is composed so
    // prim_has_api(IdtxCollisionAPI) resolves.
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let path = usd_path("schema/idtx_collision_api.usda");
    let mut service = StageService::open(&path).expect("open collision stage");
    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let projection = service.project_viewport().expect("project viewport");
    hydrator.apply_viewport_projection(&projection).unwrap();

    let added = hydrator.hydrate_collision(&service);
    assert!(added >= 3, "Ball + Ramp + Shield should hydrate colliders");

    let ball = hydrator.entity_for_path("/World/Ball").expect("ball entity");
    let ramp = hydrator.entity_for_path("/World/Ramp").expect("ramp entity");
    let shield = hydrator.entity_for_path("/World/Shield").expect("shield entity");
    let crate_ = hydrator.entity_for_path("/World/Crate").expect("crate entity");
    let world = hydrator.world_mut();

    // Ball: analytic sphere collider on the Physics channel, enabled (default).
    let ball_col = world.get::<CollisionComp>(ball).expect("ball collider");
    assert!(ball_col.enabled);
    assert_eq!(ball_col.shape, CollisionShape::Sphere);
    assert_eq!(ball_col.channels, vec![CollisionChannel::Physics]);

    // Ramp: shape = mesh (uses the prim's own mesh), two channels.
    let ramp_col = world.get::<CollisionComp>(ramp).expect("ramp collider");
    assert_eq!(ramp_col.shape, CollisionShape::Mesh);
    assert_eq!(
        ramp_col.channels,
        vec![CollisionChannel::Physics, CollisionChannel::Interaction]
    );

    // Shield: hull geometry hydrated into typed arrays, authored disabled.
    let shield_col = world.get::<CollisionComp>(shield).expect("shield collider");
    assert_eq!(shield_col.shape, CollisionShape::Hull);
    assert!(!shield_col.enabled);
    assert_eq!(shield_col.hull_points.len(), 4, "tetra hull");
    assert_eq!(shield_col.hull_triangles.len(), 12);

    // Crate: no schema → no component.
    assert!(world.get::<CollisionComp>(crate_).is_none());
}

/// `IdtxCollisionBodyAPI` on a grouping `Xform` hydrates a [`CollisionBodyComp`]
/// marker there, and [`idtx_scene::collision_body_of`] resolves a hit on any
/// descendant gprim collider up to that body — the whole-asset interaction
/// grouping. A collider outside any body resolves to `None`.
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_collision_body_and_resolve_ancestor() {
    use idtx_scene::{collision_body_of, CollisionBodyComp, CollisionComp};

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let path = usd_path("schema/idtx_collision_body.usda");
    let mut service = StageService::open(&path).expect("open collision body stage");
    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let projection = service.project_viewport().expect("project viewport");
    hydrator.apply_viewport_projection(&projection).unwrap();

    hydrator.hydrate_collision(&service);
    let bodies = hydrator.hydrate_collision_bodies(&service);
    assert_eq!(bodies, 1, "exactly /World/Asset carries IdtxCollisionBodyAPI");

    let asset = hydrator.entity_for_path("/World/Asset").expect("asset");
    let shell = hydrator
        .entity_for_path("/World/Asset/Body/Shell")
        .expect("shell");
    let lid = hydrator
        .entity_for_path("/World/Asset/Body/Lid")
        .expect("lid");
    let loose = hydrator.entity_for_path("/World/Loose").expect("loose");
    let world = hydrator.world_mut();

    // The body prim carries the marker; the gprim colliders do not.
    assert!(world.get::<CollisionBodyComp>(asset).is_some());
    assert!(world.get::<CollisionBodyComp>(shell).is_none());
    // Both leaf colliders hydrated, and both resolve up to the one body.
    assert!(world.get::<CollisionComp>(shell).is_some());
    assert!(world.get::<CollisionComp>(lid).is_some());
    assert_eq!(collision_body_of(world, shell), Some(asset));
    assert_eq!(collision_body_of(world, lid), Some(asset));
    // Self-first contract: the body prim resolves to itself (a raycast hitting
    // the body prim directly maps to the body, not to an outer ancestor).
    assert_eq!(collision_body_of(world, asset), Some(asset));
    // A collider outside any body resolves to None.
    assert!(world.get::<CollisionComp>(loose).is_some());
    assert_eq!(collision_body_of(world, loose), None);
}

/// A typed-gprim analytic collider (`UsdGeomCylinder` + `PhysicsCollisionAPI`)
/// hydrates its `CollisionComp::dims` from the gprim's OWN geometry
/// (`radius`/`height`/`axis`), not from authored `idtx:collision:<shape>:*`
/// attributes (which no longer exist). The resolved shape comes from the gprim
/// type, so no `physics:approximation` is needed for a typed analytic collider.
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_collision_dimensions_from_gprim_geometry() {
    use idtx_scene::{CollisionComp, CollisionShape, GprimAxis};
    use idtx_usd::StructuralCommand;

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    // Dedicated fixture so this mutating test does not race the read-only
    // detection test through OpenUSD's root-identifier stage cache. `/World/Pillar`
    // is a typed Cylinder (radius=3, height=10, axis=X).
    let path = usd_path("schema/idtx_collision_dims.usda");
    let mut service = StageService::open(&path).expect("open collision dims stage");
    let topo = service.project_topology().unwrap();
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&delta, &service);
    let projection = service.project_viewport().expect("project viewport");
    hydrator.apply_viewport_projection(&projection).unwrap();

    // Mark the typed Cylinder collidable with the standard schema — the gprim's
    // own geometry supplies the collider dimensions.
    service
        .apply_structural_command(&StructuralCommand::ApplyApi {
            path: "/World/Pillar".to_owned(),
            schema: idtx_usd_extension::PHYSICS_COLLISION_SCHEMA.to_owned(),
        })
        .expect("apply PhysicsCollisionAPI");

    hydrator.hydrate_collision(&service);
    let pillar = hydrator.entity_for_path("/World/Pillar").expect("pillar entity");
    let col = hydrator
        .world()
        .get::<CollisionComp>(pillar)
        .expect("pillar collider");
    assert_eq!(col.shape, CollisionShape::Cylinder);
    assert_eq!(col.dims.cylinder.axis, Some(GprimAxis::X));
    assert_eq!(col.dims.cylinder.height, Some(10.0));
    assert_eq!(col.dims.cylinder.radius, Some(3.0));
    // A Cylinder carries only its own axial dims — sphere/cube stay unset.
    assert_eq!(col.dims.sphere_radius, None);
    assert_eq!(col.dims.cube_size, None);
}

// ── idtx_hud_camera.usda (camera-HUD overlay) ──────────────────────────

/// A behavior-style flow for the HUD overlay: define an `IdtxHud` child under
/// the camera in the SESSION layer, author its `idtx:hud:*` params, re-project
/// topology into the ECS, and assert `hydrate_hud` stamps a correct `HudComp`.
/// The root layer must stay clean (the HUD is ephemeral session state).
#[test]
#[ignore = "requires USD link and IDTX_USD_DIR env var"]
fn hydrate_hud_component_from_session_layer_schema() {
    use idtx_scene::{HudAnchor, HudComp, HudPopulator};
    use idtx_usd::PropertyValue;

    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let path = temp_fixture_copy("behavior/idtx_hud_camera.usda", "hud_hydrate");
    let mut service = StageService::open(&path).expect("open hud camera stage");
    service.project_topology().expect("topology");

    // A behavior defines the HUD child into the session layer and authors its
    // params (mirrors `attach_session_hud` in the studio session).
    let hud_path = "/World/Camera/Crosshair";
    service
        .define_session_prim(hud_path, idtx_usd_extension::HUD_SCHEMA)
        .expect("define IdtxHud child in session layer");
    service
        .author_session_property(
            hud_path,
            idtx_usd_extension::ATTR_HUD_IMAGE,
            &PropertyValue::Asset("crosshair.png".to_owned()),
        )
        .expect("author hud image");
    service
        .author_session_property(
            hud_path,
            idtx_usd_extension::ATTR_HUD_SIZE_PX,
            &PropertyValue::Int2([64, 64]),
        )
        .expect("author hud size");
    service
        .author_session_property(
            hud_path,
            idtx_usd_extension::ATTR_HUD_OPACITY,
            &PropertyValue::F32(0.8),
        )
        .expect("author hud opacity");

    // Re-project the (now larger) topology into the ECS so the new HUD prim
    // gets an entity, then hydrate. `define_session_prim` drained its own notice
    // to mint the PrimId, so project a fresh full topology here.
    let topo = service.project_topology().expect("re-topology");
    let delta = idtx_usd::TopologyDelta {
        upserts: topo.to_vec(),
        removed: Default::default(),
    };
    let mut registry = PopulatorRegistry::new();
    registry.register(std::sync::Arc::new(HudPopulator));
    let mut hydrator = SceneHydrator::new(registry);
    hydrator.apply_topology_delta(&delta, &service);
    let projection = service.project_viewport().expect("project viewport");
    hydrator.apply_viewport_projection(&projection).unwrap();

    let changed = hydrator.hydrate_hud(&service);
    assert!(changed >= 1, "the IdtxHud prim should hydrate a HudComp");

    let hud_entity = hydrator
        .entity_for_path(hud_path)
        .expect("hud prim entity exists after re-topology");
    let camera = hydrator
        .entity_for_path("/World/Camera")
        .expect("camera entity");
    let world = hydrator.world_mut();

    let hud = world.get::<HudComp>(hud_entity).expect("HudComp present");
    // The resolver falls back to the authored path here (no search path / file
    // in the temp dir); in a real project it resolves to an absolute path, so
    // match on the suffix rather than the exact string.
    assert!(
        hud.image.ends_with("crosshair.png"),
        "hud image should resolve to crosshair.png; got {:?}",
        hud.image
    );
    assert_eq!(hud.size_px, [64, 64]);
    assert!((hud.opacity - 0.8).abs() < 1e-6);
    // Unauthored anchor resolves to the schema default (`center`).
    assert_eq!(hud.anchor, HudAnchor::Center);

    // The camera (type Camera, no IdtxHud) must not gain a HudComp.
    assert!(world.get::<HudComp>(camera).is_none());

    // The HUD lives only in the session layer — the root stays clean.
    let root = service.root_layer_text().expect("root layer text");
    assert!(
        !root.contains("idtx:hud:") && !root.contains("IdtxHud \"Crosshair\""),
        "HUD must not be authored into the root layer; root was:\n{root}"
    );
}


