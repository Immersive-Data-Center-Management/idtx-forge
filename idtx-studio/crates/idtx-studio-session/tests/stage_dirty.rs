//! Unsaved-changes ("dirty stage") tracking integration test.
//!
//! Locks the contract the Poser/Conductor tab dirty-marker and the save-on-close
//! prompt depend on: a change batch that touches the stage's **root layer** while
//! authoring marks the session dirty, while **session-layer** edits (variant
//! selection, visibility overrides, runtime write-back) never do, and the flag is
//! cleared by [`StudioSession::mark_stage_saved`]. See
//! `crates/idtx-studio-session/src/lib.rs` (`StudioSession::tick`).
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless
//! wgpu device. Run with the USD env configured (and single-threaded, like the
//! other GPU/USD integration tests):
//!
//! ```bash
//! IDTX_USD_DIR=… cargo test -p idtx-studio-session --features authoring \
//!   -- --ignored --test-threads=1 stage_dirty
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{PlayState, RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/authoring")
        .join("stage_dirty_base.usda")
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
                label: Some("idtx-stage-dirty-test"),
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
    .expect("open stage-dirty base fixture")
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn root_edits_mark_dirty() {
    let mut session = open();

    // ── A freshly opened stage is clean. ────────────────────────────────────
    let _ = session.tick().expect("tick");
    assert!(
        !session.is_stage_dirty(),
        "a freshly opened stage must not be dirty"
    );

    // ── A ROOT-layer edit (define a PhysicsScene prim) marks it dirty. ───────
    session.activate_physics().expect("activate physics");
    let _ = session.tick().expect("tick");
    assert!(
        session.is_stage_dirty(),
        "authoring a root-layer prim must mark the stage dirty"
    );

    // ── Saving clears the flag. ──────────────────────────────────────────────
    session.mark_stage_saved();
    assert!(
        !session.is_stage_dirty(),
        "mark_stage_saved must clear the dirty flag"
    );

    // ── Deleting a root-layer prim marks it dirty again. ─────────────────────
    session.deactivate_physics().expect("deactivate physics");
    let _ = session.tick().expect("tick");
    assert!(
        session.is_stage_dirty(),
        "deleting a root-layer prim must mark the stage dirty"
    );
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn dirty_marker_stays_clear_after_save() {
    // Regression (PR #22 review): `SdfLayer::IsDirty` stays `true` after an
    // `Export`-based save (both in-place loose Save and Save As route through
    // `export_root_layer`, which never clears the bit). The dirty probe therefore
    // must reconcile a set bit against the saved-state baseline captured by
    // `mark_stage_saved`; without it, the first change batch of ANY kind after a
    // save immediately re-sets the dirty marker.
    let mut session = open();
    let _ = session.tick().expect("tick");

    // Author a root-layer edit, then "save" (captures the content baseline).
    session.activate_physics().expect("activate physics");
    let _ = session.tick().expect("tick");
    assert!(session.is_stage_dirty());
    session.mark_stage_saved();
    assert!(!session.is_stage_dirty());

    // A subsequent SESSION-layer edit produces a change batch while the root
    // layer's `IsDirty` bit is still set from the pre-save physics edit. The
    // marker must stay clear: the root-layer content has not changed since save.
    session
        .set_session_variant("/World/Switchable", "shape", "sphere")
        .expect("set session variant");
    let _ = session.tick().expect("tick");
    assert!(
        !session.is_stage_dirty(),
        "a session-layer edit after save must not re-dirty the saved document"
    );

    // A genuine new root-layer edit after the save must still mark it dirty.
    session.deactivate_physics().expect("deactivate physics");
    let _ = session.tick().expect("tick");
    assert!(
        session.is_stage_dirty(),
        "a root-layer edit after save must re-mark the stage dirty"
    );
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn session_edits_do_not_mark_dirty() {
    // A fresh session with no prior unsaved root edits: the root layer's own
    // `IsDirty` bit is clear, so a session-layer edit cannot be mistaken for a
    // root-layer one.
    let mut session = open();
    let _ = session.tick().expect("tick");
    assert!(!session.is_stage_dirty());

    // A SESSION-layer edit (variant selection) must NOT mark the saved document
    // dirty, even though switching variants is a topology (resync) change.
    session
        .set_session_variant("/World/Switchable", "shape", "sphere")
        .expect("set session variant");
    let _ = session.tick().expect("tick");
    assert!(
        !session.is_stage_dirty(),
        "a session-layer variant selection must not dirty the saved document"
    );
}

#[test]
#[ignore = "links OpenUSD + headless wgpu"]
fn root_edits_while_playing_do_not_mark_dirty() {
    let mut session = open();
    let _ = session.tick().expect("tick");
    assert!(!session.is_stage_dirty());

    // At runtime (Playing) the physics/behavior write-back legitimately authors
    // the stage; that must never mark the saved document dirty. Even a root-layer
    // edit while Playing is ignored by the dirty gate.
    session.set_play_state(PlayState::Playing);
    session.activate_physics().expect("activate physics");
    let _ = session.tick().expect("tick");
    assert!(
        !session.is_stage_dirty(),
        "root edits while Playing must not mark the stage dirty"
    );
}
