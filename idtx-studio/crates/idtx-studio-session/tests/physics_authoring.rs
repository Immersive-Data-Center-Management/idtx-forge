//! Stage-inspector physics authoring integration test — the activate → edit →
//! deactivate path that the Stage tab's Physics section drives.
//!
//! Opens a physics-free base stage and exercises the session authoring API that
//! the UI calls through: [`StudioSession::activate_physics`] authors a
//! `PhysicsScene` prim named `physicsScene` as a child of the pseudo-root with
//! the schema gravity defaults (`vector3f physics:gravityDirection = (0, -1, 0)`
//! and `float physics:gravityMagnitude = 9.81`);
//! [`StudioSession::set_physics_gravity_direction`] /
//! [`StudioSession::set_physics_gravity_magnitude`] re-author those values; and
//! [`StudioSession::deactivate_physics`] deletes the prim again.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless
//! wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… cargo test -p idtx-studio-session --features authoring \
//!   -- --ignored physics_authoring
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

/// Component-wise approximate equality for a gravity direction vector.
fn vec3_approx(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/physics")
        .join("physics_authoring_base.usda")
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
                label: Some("idtx-physics-authoring-test"),
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

fn open() -> StudioSession {
    let _ = idtx_usd_extension::install();
    let (device, queue) = headless_device();
    StudioSession::open_with_gpu(
        fixture(),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open physics-authoring base fixture")
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn activate_edit_deactivate_physics() {
    let mut session = open();

    // ── Initially inactive — no UsdPhysicsScene prim in the base stage. ──────
    let state = session.physics_scene_state();
    assert!(!state.active, "base stage must have no PhysicsScene");
    assert_eq!(session.physics_scene_path(), None);

    // ── Activate: authors /physicsScene with the gravity defaults. ───────────
    session.activate_physics().expect("activate physics");
    let state = session.physics_scene_state();
    assert!(state.active, "physics must be active after activate_physics");
    assert_eq!(session.physics_scene_path().as_deref(), Some("/physicsScene"));
    assert!(vec3_approx(state.gravity_direction, [0.0, -1.0, 0.0]));
    assert!((state.gravity_magnitude - 9.81).abs() < 1e-4);

    // The direction must be authored as a `vector3f` (not color3f), per the
    // UsdPhysicsScene schema.
    let props = session
        .inspect_prim("/physicsScene")
        .expect("inspect physicsScene");
    let dir = props
        .iter()
        .find(|p| p.name == "physics:gravityDirection")
        .expect("gravityDirection authored");
    assert_eq!(dir.type_name, "vector3f", "gravity direction must be vector3f");

    // ── Idempotent activate — a second activate must not add a second scene. ─
    session.activate_physics().expect("re-activate is a no-op");
    let root = session
        .root_layer_snapshot()
        .expect("snapshot root layer");
    assert_eq!(
        root.matches("def PhysicsScene").count(),
        1,
        "activate must be idempotent (exactly one PhysicsScene prim)"
    );

    // ── Edit gravity. ────────────────────────────────────────────────────────
    session
        .set_physics_gravity_direction([1.0, 0.0, 0.0])
        .expect("set gravity direction");
    session
        .set_physics_gravity_magnitude(3.5)
        .expect("set gravity magnitude");
    let state = session.physics_scene_state();
    assert!(vec3_approx(state.gravity_direction, [1.0, 0.0, 0.0]));
    assert!((state.gravity_magnitude - 3.5).abs() < 1e-4);

    // ── Deactivate: deletes the prim. ────────────────────────────────────────
    session.deactivate_physics().expect("deactivate physics");
    let state = session.physics_scene_state();
    assert!(!state.active, "physics must be inactive after deactivate");
    assert_eq!(session.physics_scene_path(), None);
}
