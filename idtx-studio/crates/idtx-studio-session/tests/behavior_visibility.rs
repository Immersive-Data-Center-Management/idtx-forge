//! Regression: a guest `set_visibility` session edit must actually surface in
//! the render world (gaze-highlight).
//!
//! A behavior that posts `SessionEditOp::SetVisibility(true)` on an authored-
//! `invisible` prim used to have no visible effect: `author_session_visibility`
//! flips only the composed USD opinion, while the renderer reads
//! `VisibilityComp.visible`, derived from the ECS `VisibilityOverride` chain —
//! which the viewport projection leaves untouched and
//! `initialize_visibility_overrides` preserves once seeded. The prim stayed
//! `Invisible` in the ECS and never rendered (unlike the editor scene-tree
//! toggle, which drives the override chain directly). `apply_session_edits` now
//! re-asserts the override after the re-projection tick, mirroring
//! `apply_level_transition`.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless
//! wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored behavior_visibility
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd")
        .join(name)
}

/// Surfaceless wgpu device/queue for headless rendering.
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
                label: Some("idtx-behavior-visibility-test"),
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

const MARKER: &str = "/World/Marker";

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn guest_set_visibility_surfaces_authored_invisible_prim() {
    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("behavior/behavior_set_visibility.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open visibility fixture");

    // Authored starting state: the marker is `invisible`.
    assert!(
        !session.prim_visible(MARKER),
        "Marker starts invisible (authored)"
    );

    let gen_before = session.visibility_generation();

    // A guest `set_visibility(true)` — exactly what the gaze-highlight behavior
    // posts when the raycast hits — must make the prim render.
    session.apply_guest_visibility_edit_for_test(MARKER, true);

    assert!(
        session.prim_visible(MARKER),
        "Marker visible after guest set_visibility(true) — the ECS override \
         chain must be driven, not only the USD opinion"
    );
    assert!(
        session.visibility_generation() > gen_before,
        "a visibility-only change bumps the generation"
    );

    // And hiding it again (raycast miss) must take effect too.
    let gen_mid = session.visibility_generation();
    session.apply_guest_visibility_edit_for_test(MARKER, false);
    assert!(
        !session.prim_visible(MARKER),
        "Marker hidden again after guest set_visibility(false)"
    );
    assert!(
        session.visibility_generation() > gen_mid,
        "hiding bumps the generation again"
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn repeated_same_visibility_is_deduped() {
    // Perf regression (Quest Cost B): a behavior like `left_aim_sync` re-asserts
    // `set_visibility(gaze_point, true)` EVERY frame while the aim ray keeps
    // hitting a target. Re-authoring an unchanged visibility used to force a full
    // snapshot re-extract every frame (VISIBILITY defeats the transform-only fast
    // path and sets `snapshot_dirty`). The host now dedups: an unchanged value is
    // a no-op — no author, no reprojection, no `visibility_generation` bump — so
    // only the show↔hide EDGE costs anything.
    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("behavior/behavior_set_visibility.usda"),
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open visibility fixture");

    // Edge false→true, then a second true: the FIRST author creates the session
    // `visibility` attribute spec (a resync that clears the dedup cache), so the
    // steady-state dedup only holds once the attribute exists. Prime past that.
    session.apply_guest_visibility_edit_for_test(MARKER, true);
    session.apply_guest_visibility_edit_for_test(MARKER, true);
    assert!(session.prim_visible(MARKER), "visible after priming the attribute");
    let gen_primed = session.visibility_generation();

    // Re-assert true for several frames (the steady-aim case): each is deduped,
    // so the generation does NOT move and the prim stays visible.
    for _ in 0..5 {
        session.apply_guest_visibility_edit_for_test(MARKER, true);
    }
    assert_eq!(
        session.visibility_generation(),
        gen_primed,
        "re-asserting the same visibility must be a no-op (deduped)"
    );
    assert!(session.prim_visible(MARKER), "still visible after redundant trues");

    // The hide EDGE still takes effect and bumps.
    session.apply_guest_visibility_edit_for_test(MARKER, false);
    assert!(!session.prim_visible(MARKER), "hidden on the true→false edge");
    assert!(
        session.visibility_generation() > gen_primed,
        "the show→hide edge still bumps the generation"
    );
    let gen_after_hide = session.visibility_generation();

    // Redundant hides are likewise deduped (the attribute already exists, so no
    // resync clears the cache this time).
    for _ in 0..5 {
        session.apply_guest_visibility_edit_for_test(MARKER, false);
    }
    assert_eq!(
        session.visibility_generation(),
        gen_after_hide,
        "re-asserting the same hidden state must be a no-op (deduped)"
    );
}
