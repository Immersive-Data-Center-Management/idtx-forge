//! Runtime regression with editor-overlay deliberately enabled by dev-dependencies.
//! Run: cargo test -p idtx-runtime-core --features metrics --lib camera_overlay -- --ignored

use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::*;

fn rose_pixels(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|p| p[0] > 245 && (175..=200).contains(&p[1]) && (195..=215).contains(&p[2]))
        .count()
}

fn assert_no_camera_overlay(session: &mut StudioSession) {
    // An unrelated camera remains in view, so hiding the view camera's own
    // glyph cannot make this pass accidentally. Selection must not bypass it.
    session.set_selected(Some("/World/Camera".into()));
    assert!(session.camera_glyph_lines().is_empty());
    session.render_one_frame(false);
    assert_eq!(rose_pixels(&session.read_pixels().unwrap()), 0);
}

#[test]
#[ignore = "requires OpenUSD link and headless GPU"]
fn runtime_camera_overlays_stay_off_at_boot_transfer_and_level_warmup() {
    idtx_usd_extension::install().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let stage_text = r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    metersPerUnit = 1
)
def Xform "World" {
    def Camera "AViewer" {
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Camera "Camera" {
        float focalLength = 35
        float2 clippingRange = (0.1, 5)
    }
}
"#;
    let start = temp.path().join("start.usda");
    let transfer = temp.path().join("transfer.usda");
    let incoming = temp.path().join("levels/incoming/level.usda");
    for path in [&start, &transfer, &incoming] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, stage_text).unwrap();
    }
    let (device, queue) = headless_device().expect("headless GPU");
    let mut cfg = RunConfig::new(start.clone(), RunMode::Desktop);
    cfg.viewport = [320, 240];
    cfg.levels = vec![start.clone(), incoming.clone()];
    cfg.transfer_level = Some(transfer.clone());
    let open = |path: &Path| {
        StudioSession::open_with_gpu(
            path,
            device.clone(),
            queue.clone(),
            cfg.viewport,
            cfg.texture_quality,
            RenderWorldUnitScale::METRE,
        )
        .unwrap()
    };

    // Prove the exact binary contains working studio camera overlays before
    // testing the runtime policy. This catches accidental feature-gate coverage.
    let mut current = open(&start);
    current.use_scene_camera("/World/AViewer");
    current.set_selected(Some("/World/Camera".into()));
    assert!(!current.camera_glyph_lines().is_empty());
    current.render_one_frame(false);
    assert!(rose_pixels(&current.read_pixels().unwrap()) > 30);

    // This is the startup configuration shared by the windowed preview/player
    // and run_headless. It must override the session's editor defaults.
    configure_runtime_session(&mut current, &cfg);
    assert_no_camera_overlay(&mut current);
    let mut mask = open(&transfer);
    configure_runtime_session(&mut mask, &cfg);
    assert_no_camera_overlay(&mut mask);
    let mut sessions = SessionSet::new(current, Some(mask));
    let mut driver = build_transition_driver(&cfg, device, queue, cfg.viewport, "");
    let warmup_checked = Arc::new(AtomicBool::new(false));
    let checked = warmup_checked.clone();
    driver.set_prepare(Arc::new(move |session| {
        // The loader has just rendered its very first frame, before cutover's
        // defensive disable call. Check that frame as well as later frames.
        assert_eq!(rose_pixels(&session.read_pixels().unwrap()), 0);
        assert_no_camera_overlay(session);
        checked.store(true, Ordering::Release);
    }));
    sessions
        .current_mut()
        .post_pending_level_transition(idtx_behavior::LevelTransitionRequest {
            target: "incoming".into(),
            spawn: None,
        });
    driver.poll(&mut sessions);
    assert_eq!(sessions.active(), ActiveSession::Transfer);
    assert_no_camera_overlay(sessions.active_session_mut());
    let deadline = Instant::now() + Duration::from_secs(20);
    while driver.is_transitioning() {
        drop(driver.poll(&mut sessions));
        assert!(Instant::now() < deadline, "level transition timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        warmup_checked.load(Ordering::Acquire),
        "incoming warm-up was checked"
    );
    assert_eq!(sessions.active(), ActiveSession::Level);
    assert_eq!(sessions.current().source_path(), incoming);
    assert_no_camera_overlay(sessions.active_session_mut());
    drop(driver);
    drop(sessions);

    // Exercise the real headless runtime entry point too. Preview and cooked
    // desktop configurations differ in window policy, never in overlay policy.
    cfg.frames = Some(1);
    cfg.screenshot = Some(temp.path().join("runtime.png"));
    for fullscreen in [false, true] {
        cfg.fullscreen = fullscreen;
        run_headless(&cfg).unwrap();
        let pixels = image::open(cfg.screenshot.as_ref().unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(rose_pixels(pixels.as_raw()), 0);
    }
}
