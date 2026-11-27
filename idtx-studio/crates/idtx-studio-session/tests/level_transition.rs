//! Headless level-transition integration test.
//!
//! Exercises [`StudioSession::transition_level`] end-to-end against the
//! `level_transition_assembly.usda` fixture: opening the assembly, driving a
//! Hub → Forest transition, and asserting that the session re-projects both the
//! payload swap (Hub's payload unloaded, Forest's composed) and the session-
//! layer visibility toggles (Forest shown, Hub hidden) into the ECS world.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless
//! wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored level_transition
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(name)
}

/// Surfaceless wgpu device/queue for headless rendering (mirrors
/// `idtx-runtime-core::headless_device`).
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
                label: Some("idtx-level-transition-test"),
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

const HUB: &str = "/World/Levels/Hub";
const FOREST: &str = "/World/Levels/Forest";

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn transition_swaps_visibility_and_payload_between_holders() {
    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("layering/level_transition_assembly.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open assembly fixture");

    // Starting state authored into the fixture: Hub shown, Forest hidden.
    assert!(session.prim_visible(HUB), "Hub starts visible");
    assert!(!session.prim_visible(FOREST), "Forest starts invisible");

    let gen_before = session.visibility_generation();

    // Cut Hub → Forest with no transfer (sync/cosmetic path).
    session
        .transition_level(Some(HUB.to_owned()), FOREST.to_owned(), None)
        .expect("transition Hub -> Forest");

    // Visibility swapped and re-asserted after the re-projection tick.
    assert!(
        session.prim_visible(FOREST),
        "Forest visible after transition"
    );
    assert!(!session.prim_visible(HUB), "Hub hidden after transition");
    assert!(
        session.visibility_generation() > gen_before,
        "a visibility-only change bumps the generation"
    );

    // Payload swap: Forest's contents are composed, Hub's are reclaimed.
    assert!(
        session
            .stage()
            .prim_at_path("/World/Levels/Forest/Body")
            .is_some(),
        "Forest payload composed after transition"
    );
    assert!(
        session
            .stage()
            .prim_at_path("/World/Levels/Hub/Body")
            .is_none(),
        "Hub payload unloaded after transition"
    );
}

/// On-demand open (`open_with_gpu_on_demand`): the assembly opens with payloads
/// UNLOADED and only the named default level is composed. The non-default level's
/// payload must stay uncomposed — the whole point of on-demand loading (contrast
/// `open_with_gpu`, which eagerly composes every level payload at open).
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn on_demand_open_composes_only_the_default_level() {
    let (device, queue) = headless_device();
    let session = StudioSession::open_with_gpu_on_demand(
        fixture("layering/level_transition_assembly.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
        "",
        Some(HUB),
    )
    .expect("open assembly on demand");

    // Only the default (Hub) level is composed; Forest's payload stays unloaded.
    assert!(
        session
            .stage()
            .prim_at_path("/World/Levels/Hub/Body")
            .is_some(),
        "default level (Hub) composed on demand"
    );
    assert!(
        session
            .stage()
            .prim_at_path("/World/Levels/Forest/Body")
            .is_none(),
        "non-default level (Forest) payload must stay unloaded on demand"
    );
    assert!(session.prim_visible(HUB), "default level visible after load");
}
