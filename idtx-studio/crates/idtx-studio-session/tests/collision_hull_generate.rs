//! Headless round-trip test for collision convex-hull generation:
//! [`StudioSession::gather_collision_mesh_points`] →
//! [`idtx_hull_gen::generate_convex_hull`] → [`StudioSession::author_collision_hull`].
//!
//! Validates the whole new surface end to end: the gprim-pruning mesh gather,
//! the quickhull compute, and — crucially — the new `point3f[]` / `int[]` FFI
//! authoring path (`set-property-point3-array` / `set-property-int-array` in the
//! C++ shim) by reading the authored arrays back through `inspect_prim`.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith (needs `IDTX_USD_DIR` + the
//! runtime libs on the loader path) and spins up a headless wgpu device:
//!
//! ```bash
//! IDTX_USD_DIR=… \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored collision_hull
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_hull_gen::HullParams;
use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(name)
}

fn headless_device() -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("a headless wgpu adapter");
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-collision-hull-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .expect("a headless wgpu device")
    })
}

fn authored_summary(session: &mut StudioSession, prim: &str, attr: &str) -> String {
    session
        .inspect_prim(prim)
        .expect("inspect")
        .into_iter()
        .find(|p| p.name == attr && p.authored)
        .map(|p| p.value_summary)
        .unwrap_or_default()
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn generate_hull_gathers_subtree_and_authors_arrays() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("schema/idtx_collision_hull.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open hull fixture");

    // Gather on the Xform descends into both child cubes (8 + 8 points),
    // transformed into the Xform's local space.
    let group_points = session.gather_collision_mesh_points("/World/Group");
    assert_eq!(group_points.len(), 16, "both child cubes should be gathered");
    // BoxB is offset +3 in X, so the combined cloud spans X ∈ [-1, 4].
    let max_x = group_points.iter().map(|p| p[0]).fold(f32::MIN, f32::max);
    assert!((max_x - 4.0).abs() < 1e-4, "BoxB offset must land in group space, got max_x={max_x}");

    // Gathering on a Mesh gprim uses only its own 8 points — no child visit (and
    // a Mesh never has mesh children per the gprim-nesting rule).
    let box_points = session.gather_collision_mesh_points("/World/Group/BoxA");
    assert_eq!(box_points.len(), 8, "a Mesh gprim contributes only its own points");

    // Compute + author the hull of the two-cube cloud.
    let hull = idtx_hull_gen::generate_convex_hull(&group_points, &HullParams::default())
        .expect("hull of two cubes");
    assert!(hull.points.len() >= 8, "a two-box union hull has at least 8 corners");
    assert_eq!(hull.triangles.len() % 3, 0, "triangle indices come in triples");

    session
        .apply_collision_api("/World/Group")
        .expect("apply collision schema");
    session
        .author_collision_hull("/World/Group", &hull.points, &hull.triangles)
        .expect("author hull arrays");

    // Read the authored arrays back through the FFI — proves the new
    // point3f[]/int[] authoring + eval round-trips.
    let points_summary = authored_summary(&mut session, "/World/Group", "idtx:collision:hull:points");
    let tris_summary = authored_summary(&mut session, "/World/Group", "idtx:collision:hull:triangles");
    assert!(!points_summary.is_empty(), "hull:points must be authored");
    assert!(!tris_summary.is_empty(), "hull:triangles must be authored");

    // Authoring the hull also stamps the OpenUSD-standard convex-hull mesh
    // collider so a standard DCC reads it: physics:approximation = convexHull on
    // an applied UsdPhysicsMeshCollisionAPI.
    let approx = authored_summary(&mut session, "/World/Group", "physics:approximation");
    assert!(
        approx.contains("convexHull"),
        "physics:approximation must be convexHull; got {approx:?}"
    );
    let api = session
        .inspect_prim("/World/Group")
        .expect("inspect")
        .into_iter()
        .find(|p| p.name == "apiSchemas")
        .map(|p| p.value_summary)
        .unwrap_or_default();
    assert!(
        api.contains("PhysicsMeshCollisionAPI"),
        "the mesh-collision schema must be applied; got {api:?}"
    );
}

/// Round-trip the collision inspector's **Remove Collision**: a convex-hull mesh
/// collider is authored (schemas + the bulky `point3f[]`/`int[]` hull arrays),
/// then [`StudioSession::remove_collision_api`] must strip every applied collider
/// schema AND clear the owned authored opinions — the stale hull arrays included,
/// which is the whole point (dead `idtx:collision:hull:*` data otherwise bloats
/// the layer).
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn remove_collision_api_strips_schemas_and_hull() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("schema/idtx_collision_hull.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open hull fixture");

    // Author a full convex-hull mesh collider on the group (schemas + hull arrays).
    let points = session.gather_collision_mesh_points("/World/Group");
    let hull = idtx_hull_gen::generate_convex_hull(&points, &HullParams::default())
        .expect("hull of two cubes");
    session
        .apply_collision_api("/World/Group")
        .expect("apply collision schema");
    session
        .author_collision_hull("/World/Group", &hull.points, &hull.triangles)
        .expect("author hull arrays");

    // Precondition: the hull arrays are authored before removal.
    assert!(
        !authored_summary(&mut session, "/World/Group", "idtx:collision:hull:points").is_empty(),
        "hull:points must be authored before removal"
    );

    // Remove: reports exactly the applied collider schema set (mesh → all three),
    // ordered standard-first then the reduced addendum.
    let removed = session
        .remove_collision_api("/World/Group")
        .expect("remove collision");
    assert_eq!(
        removed,
        vec![
            "PhysicsCollisionAPI".to_owned(),
            "PhysicsMeshCollisionAPI".to_owned(),
            "IdtxCollisionAPI".to_owned(),
        ],
        "removal reports every applied collider schema"
    );

    // The schemas are gone from the composed apiSchemas.
    let api = session
        .inspect_prim("/World/Group")
        .expect("inspect")
        .into_iter()
        .find(|p| p.name == "apiSchemas")
        .map(|p| p.value_summary)
        .unwrap_or_default();
    for schema in ["PhysicsCollisionAPI", "PhysicsMeshCollisionAPI", "IdtxCollisionAPI"] {
        assert!(
            !api.contains(schema),
            "{schema} must be removed from apiSchemas; got {api:?}"
        );
    }

    // Every owned authored opinion is cleared — the stale hull arrays especially.
    for attr in [
        "physics:collisionEnabled",
        "physics:approximation",
        "idtx:collision:channel",
        "idtx:collision:hull:points",
        "idtx:collision:hull:triangles",
    ] {
        let summary = authored_summary(&mut session, "/World/Group", attr);
        assert!(
            summary.is_empty(),
            "{attr} must be cleared on Remove Collision; got {summary:?}"
        );
    }
}
