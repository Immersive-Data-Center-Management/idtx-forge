//! Regression: a guest `set_widget_value` (→ `SessionEditOp::SetWidgetValue`)
//! authors `idtx:widget:value` on a widget-field child into the session layer,
//! and the resulting `idtx:widget:` change drives the widget re-hydrate so the
//! panel re-rasterizes — the same render path the `openExec` compute binding uses,
//! but driven by a behavior.
//!
//! This is the host half of the `widget_fetch_value` example: it exercises the
//! real `apply_session_edits` → `author_session_property` (string) → `tick()` →
//! `widget_changed` → `hydrate_widgets` chain without a wasm host, via the
//! `apply_guest_widget_value_edit_for_test` seam.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless
//! wgpu device. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-studio-session --features authoring -- --ignored behavior_widget
//! ```

#![cfg(feature = "authoring")]

use std::path::PathBuf;

use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

#[path = "../../../tests/support/mod.rs"]
mod support;

fn fixture(name: &str) -> PathBuf {
    support::usd_fixture(name)
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
                label: Some("idtx-behavior-widget-test"),
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

const PANEL: &str = "/World/Readout";
const FIELD: &str = "/World/Readout/Row/Value";

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn distributable_login_panel_opens_and_projects_only_masked_password_text() {
    idtx_usd_extension::install().unwrap();
    let scratch = support::scratch_dir("login-panel");
    let path = scratch.path().join("login-panel.usda");
    std::fs::copy(fixture("widgets/auth_login.usda"), &path).unwrap();
    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(path, device, queue, [64, 64], TextureDecodeConfig::default(), RenderWorldUnitScale::METRE).unwrap();
    assert_eq!(session.widget_value_for_test("/Login/PasswordValue").as_deref(), Some(""));
    session.apply_guest_widget_value_edit_for_test("/Login", "PasswordValue", "***");
    session.render_one_frame(false);
    assert_eq!(session.widget_value_for_test("/Login/PasswordValue").as_deref(), Some("***"));
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn guest_set_widget_value_authors_into_the_session_layer() {
    // Register the IDTX schemas so IdtxWidgetPanel / IdtxWidgetFieldString and
    // the IdtxBehaviorAPI resolve.
    idtx_usd_extension::install().expect("register IDTX USD schema plugin");

    let (device, queue) = headless_device();
    let mut session = StudioSession::open_with_gpu(
        fixture("widgets/widget_fetch_value.usda"),
        device,
        queue,
        [256, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open widget fetch-value fixture");

    // The authored default is empty.
    assert_eq!(
        session.widget_value_for_test(FIELD).as_deref(),
        Some(""),
        "the IdtxWidgetFieldString starts at its authored default"
    );

    // Exactly what the `widget_fetch_value` behavior posts from `on_fetch` after
    // extracting the scalar with `aas::value_from_aas_json`: the child is
    // addressed panel-relative by name, the value is authored verbatim.
    session.apply_guest_widget_value_edit_for_test(PANEL, "Row/Value", "42.0");

    assert_eq!(
        session.widget_value_for_test(FIELD).as_deref(),
        Some("42.0"),
        "SetWidgetValue must author idtx:widget:value into the session layer on \
         the panel-relative child"
    );

    // A second author overwrites the value (behaviors poll on an interval).
    session.apply_guest_widget_value_edit_for_test(PANEL, "Row/Value", "17.5");
    assert_eq!(
        session.widget_value_for_test(FIELD).as_deref(),
        Some("17.5"),
        "re-authoring replaces the session-layer opinion"
    );
}
