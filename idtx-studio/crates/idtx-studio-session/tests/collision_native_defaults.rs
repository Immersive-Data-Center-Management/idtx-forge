//! Headless round-trip test for [`StudioSession::apply_collision_api`]'s
//! `OpenUSD`-standard collider authoring.
//!
//! **Add Collision** applies `UsdPhysicsCollisionAPI` (+
//! `UsdPhysicsMeshCollisionAPI` on a Mesh) and the reduced `IdtxCollisionAPI`
//! addendum as one failure-atomic batch, authoring `physics:collisionEnabled`.
//! It authors NO collider shape token or dimensions — the collider shape and
//! dimensions are resolved at hydration from the gprim type + the gprim's own
//! geometry.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith (needs `IDTX_USD_DIR` + the
//! runtime libs on the loader path, including the usdPhysics plugin) and spins up
//! a headless wgpu device:
//!
//! ```bash
//! IDTX_USD_DIR=… \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored collision_native
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

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
                label: Some("idtx-collision-native-test"),
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

/// Read an authored attribute's value summary (empty when unauthored).
fn authored_summary(session: &mut StudioSession, prim: &str, attr: &str) -> String {
    session
        .inspect_prim(prim)
        .expect("inspect")
        .into_iter()
        .find(|p| p.name == attr && p.authored)
        .map(|p| p.value_summary)
        .unwrap_or_default()
}

/// `apiSchemas` value summary for a prim (lists the applied API schemas).
fn api_schemas(session: &mut StudioSession, prim: &str) -> String {
    session
        .inspect_prim(prim)
        .expect("inspect")
        .into_iter()
        .find(|p| p.name == "apiSchemas")
        .map(|p| p.value_summary)
        .unwrap_or_default()
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn apply_collision_api_applies_standard_schemas() {
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("schema/idtx_collision_native_defaults.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open native-defaults fixture");

    // An analytic typed gprim gets PhysicsCollisionAPI + the reduced addendum,
    // but NOT PhysicsMeshCollisionAPI (approximation applies only to meshes).
    let applied = session.apply_collision_api("/World/Ball").expect("apply Ball");
    assert_eq!(
        applied,
        vec!["PhysicsCollisionAPI".to_owned(), "IdtxCollisionAPI".to_owned()],
        "analytic collider applies the standard gate + reduced addendum only"
    );
    let api = api_schemas(&mut session, "/World/Ball");
    assert!(api.contains("PhysicsCollisionAPI"), "apiSchemas: {api:?}");
    assert!(api.contains("IdtxCollisionAPI"), "apiSchemas: {api:?}");
    assert!(
        !api.contains("PhysicsMeshCollisionAPI"),
        "analytic collider must not apply the mesh-collision schema; got {api:?}"
    );

    // physics:collisionEnabled is authored true.
    let enabled = authored_summary(&mut session, "/World/Ball", "physics:collisionEnabled");
    assert!(
        enabled.contains('1') || enabled.contains("true"),
        "physics:collisionEnabled must be authored true; got {enabled:?}"
    );

    // No retired shape token / analytic dimensions are authored anymore.
    assert!(
        authored_summary(&mut session, "/World/Ball", "idtx:collision:shape").is_empty(),
        "the retired idtx:collision:shape must not be authored"
    );
    assert!(
        authored_summary(&mut session, "/World/Ball", "idtx:collision:sphere:radius").is_empty(),
        "the retired analytic dimensions must not be authored"
    );
}
