//! Headless frame-stepping smoke test for the runtime player core.
//!
//! Unlike idtx-poc's headless path (which was a no-op), [`run_headless`]
//! genuinely steps tick / animation / behaviors / (mock) XR each frame and
//! renders to an offscreen target.
//!
//! Marked `#[ignore]` by default: it opens a real USD stage, which requires the
//! monolithic `OpenUSD` runtime + plugin folder staged next to the test binary
//! (see CLAUDE.md). Run explicitly on a configured machine:
//!
//! ```text
//! cargo test -p idtx-runtime-core --features mock-xr -- --ignored
//! ```

#![cfg(feature = "mock-xr")]

use std::path::PathBuf;

use idtx_runtime_core::{RunConfig, RunMode, run_headless};

fn basic_scene() -> PathBuf {
    // crates/idtx-runtime-core/tests -> repo root -> tests/fixtures/usd/hydration/basic-scene.usda
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/hydration/basic-scene.usda")
}

#[test]
#[ignore = "opens a real USD stage; needs the OpenUSD runtime staged next to the test binary"]
fn headless_xr_steps_frames_and_starts_mock_preview() {
    let mut cfg = RunConfig::new(basic_scene(), RunMode::Xr);
    cfg.frames = Some(3);

    match run_headless(cfg) {
        Ok(report) => {
            assert_eq!(report.frames, 3, "should step exactly the requested frames");
            assert!(report.xr_started, "mock XR preview should start in XR mode");
        }
        Err(idtx_runtime_core::RuntimeError::NoHeadlessAdapter) => {
            eprintln!("skipping: no wgpu adapter available in this environment");
        }
        Err(e) => panic!("headless run failed: {e}"),
    }
}

#[test]
#[ignore = "opens a real USD stage; needs the OpenUSD runtime staged next to the test binary"]
fn headless_desktop_steps_frames_without_xr() {
    let mut cfg = RunConfig::new(basic_scene(), RunMode::Desktop);
    cfg.frames = Some(2);

    match run_headless(cfg) {
        Ok(report) => {
            assert_eq!(report.frames, 2);
            assert!(!report.xr_started, "desktop mode must not start XR");
        }
        Err(idtx_runtime_core::RuntimeError::NoHeadlessAdapter) => {
            eprintln!("skipping: no wgpu adapter available in this environment");
        }
        Err(e) => panic!("headless run failed: {e}"),
    }
}
