//! Headless mapping-context hot-reload integration test.
//!
//! Exercises the session's runtime input mapping-context stack end-to-end
//! against a real (opened) stage: pushing/removing/reprioritising named
//! [`idtx_studio_session::InputMap`] layers re-flattens the stack with
//! consuming shadowing and *immediately* reinstalls the effective bindings into
//! the behavior host's router — the hot-reload the runtime player relies on when
//! a transient "menu"/"vehicle" context is layered over "gameplay".
//!
//! Behavior execution is runtime-only, so this surface is gated on `xr-preview`
//! and never wired to the editor shell. `#[ignore]` because it links the `OpenUSD` monolith and spins up a
//! headless wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-studio-session --features xr-preview -- --ignored mapping_context
//! ```

#![cfg(feature = "xr-preview")]

use std::path::PathBuf;

use idtx_studio_session::{InputMap, RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

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
                label: Some("idtx-mapping-context-test"),
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

// Gameplay context: `jump` (Key/Space) + `move_x` (axis).
const GAMEPLAY: &str = r#"
[actions.jump]
kind = "button"
bindings = ["Key/Space"]

[actions.move_x]
kind = "axis"
bindings = [{ source = "Key/D", scale = 1.0 }]
"#;

// Menu context: rebinds `jump` (Gamepad/South) and adds `pause`.
const MENU: &str = r#"
[actions.jump]
kind = "button"
bindings = ["Gamepad/South"]

[actions.pause]
kind = "button"
bindings = ["Key/Escape"]
"#;

fn map(src: &str) -> InputMap {
    InputMap::parse(src).expect("valid input-map TOML")
}

/// The binding source the host router currently resolves `action` to, or `None`
/// when the action is unbound in the effective map.
fn bound_source(session: &StudioSession, action: &str) -> Option<String> {
    let router = session.behavior_router();
    let id = router.resolve(action);
    if id.is_unresolved() {
        return None;
    }
    let spec = router.map().action(id)?;
    Some(spec.bindings.first()?.source.clone())
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn mapping_context_stack_hot_reloads_effective_bindings() {
    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("hydration/basic-scene.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open basic-scene fixture");

    // Nothing pushed yet.
    assert_eq!(session.mapping_context_count(), 0);
    assert_eq!(bound_source(&session, "jump"), None);

    // Push gameplay: its bindings hot-reload straight into the router.
    let replaced = session.push_mapping_context("gameplay", 0, map(GAMEPLAY));
    assert!(!replaced, "first push of a name replaces nothing");
    assert_eq!(session.mapping_context_count(), 1);
    assert_eq!(bound_source(&session, "jump").as_deref(), Some("Key/Space"));
    assert!(bound_source(&session, "move_x").is_some());
    assert_eq!(bound_source(&session, "pause"), None);

    // Layer menu on top (higher priority): it consumes `jump` (now Gamepad/South)
    // and adds `pause`; gameplay's disjoint `move_x` still merges through.
    session.push_mapping_context("menu", 10, map(MENU));
    assert_eq!(session.mapping_context_count(), 2);
    assert_eq!(
        bound_source(&session, "jump").as_deref(),
        Some("Gamepad/South"),
        "the higher-priority menu context consumes `jump`"
    );
    assert_eq!(
        bound_source(&session, "pause").as_deref(),
        Some("Key/Escape")
    );
    assert!(
        bound_source(&session, "move_x").is_some(),
        "disjoint action still merges"
    );

    // Highest-first name order reflects priority.
    assert_eq!(
        session.mapping_context_names().collect::<Vec<_>>(),
        vec!["menu", "gameplay"]
    );

    // Drop menu below gameplay: gameplay reclaims `jump`, `pause` disappears.
    assert!(session.reprioritise_mapping_context("menu", -10));
    assert_eq!(
        bound_source(&session, "jump").as_deref(),
        Some("Key/Space"),
        "after reprioritise gameplay wins `jump` again"
    );
    assert_eq!(
        bound_source(&session, "pause").as_deref(),
        Some("Key/Escape"),
        "menu still present, keeps pause"
    );
    assert_eq!(
        session.mapping_context_names().collect::<Vec<_>>(),
        vec!["gameplay", "menu"]
    );

    // Reprioritise miss is a no-op.
    assert!(!session.reprioritise_mapping_context("nonexistent", 5));

    // Remove menu: pause unbinds, only gameplay remains.
    assert!(session.remove_mapping_context("menu"));
    assert!(
        !session.remove_mapping_context("menu"),
        "removing twice is a no-op"
    );
    assert_eq!(session.mapping_context_count(), 1);
    assert_eq!(bound_source(&session, "pause"), None);
    assert_eq!(bound_source(&session, "jump").as_deref(), Some("Key/Space"));

    // Clear empties the stack and hot-reloads to an empty map.
    session.clear_mapping_contexts();
    assert_eq!(session.mapping_context_count(), 0);
    assert_eq!(bound_source(&session, "jump"), None);
    assert_eq!(bound_source(&session, "move_x"), None);
}
