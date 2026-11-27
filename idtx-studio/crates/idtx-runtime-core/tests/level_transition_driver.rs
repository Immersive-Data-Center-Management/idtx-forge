//! Headless end-to-end level-transition driver test.
//!
//! Proves the full driver chain against real `OpenUSD` sessions + a headless wgpu
//! device: a pending [`LevelTransitionRequest`] → the driver cuts to the resident
//! transfer scene → the target level loads async on a worker → it is swapped in
//! (the old start session dropped, not merged) → the player is placed at the
//! target spawn. Also the negative path: an unresolvable target leaves the set on
//! the current level and clears the request.
//!
//! The request is seeded through the public session seam
//! ([`StudioSession::post_pending_level_transition`]) rather than a compiled
//! `level_control` behavior — the behavior route needs the `wasm32` target and a
//! full SDK build, whereas the seam is the exact field the driver polls
//! (`current_mut().take_pending_level_transition()`), so it exercises the same
//! chain deterministically without that machinery.
//!
//! `#[ignore]` because it links the `OpenUSD` monolith and spins up a headless wgpu
//! device. The crate's dev-deps enable the session `authoring` feature, so the
//! player-placement assertion is active. Run with the USD env configured:
//!
//! ```bash
//! IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-runtime-core -- --ignored level_transition_driver
//! ```

use std::path::Path;
use std::task::Poll;
use std::time::{Duration, Instant};

use idtx_behavior::LevelTransitionRequest;
use idtx_runtime_core::{
    ActiveSession, SessionSet, StageLoader, TransitionDriver, build_transition_driver,
};
use idtx_studio_session::{RenderWorldUnitScale, StudioSession, TextureDecodeConfig};

/// Surfaceless wgpu device/queue (mirrors `runtime-core::headless_device`).
fn headless_gpu() -> (wgpu::Device, wgpu::Queue) {
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
                label: Some("idtx-level-transition-driver-test"),
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

fn open(path: &Path, device: &wgpu::Device, queue: &wgpu::Queue) -> StudioSession {
    let mut session = StudioSession::open_with_gpu(
        path,
        device.clone(),
        queue.clone(),
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
    )
    .expect("open test stage");
    session.render_one_frame(false);
    session
}

/// A start level: a unique `/World/level/Start` marker + a `/World/Player` the
/// driver would place (this level is never a transition *target*, so it has no
/// spawn — it just proves the old session is replaced, not merged).
fn write_start(path: &Path) {
    let text = "#usda 1.0\n\
(\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\n\
def Xform \"World\"\n{\n\
\x20   def Xform \"level\"\n    {\n\
\x20       def Xform \"Start\" {}\n    }\n\
\x20   def Xform \"Player\" {}\n}\n";
    std::fs::write(path, text).expect("write start level");
}

/// A target (cave) level: `/World/level/Spawn` at a known translate + a
/// `/World/Player` to receive the placement.
fn write_cave(path: &Path, spawn_translate: [f64; 3]) {
    let [x, y, z] = spawn_translate;
    let text = format!(
        "#usda 1.0\n\
(\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\n\
def Xform \"World\"\n{{\n\
\x20   def Xform \"level\"\n    {{\n\
\x20       def Xform \"Spawn\"\n        {{\n\
\x20           double3 xformOp:translate = ({x}, {y}, {z})\n\
\x20           uniform token[] xformOpOrder = [\"xformOp:translate\"]\n\
\x20       }}\n    }}\n\
\x20   def Xform \"Player\" {{}}\n}}\n"
    );
    std::fs::write(path, text).expect("write cave level");
}

/// A tiny transfer/mask level with a unique `/World/Mask` prim.
fn write_transfer(path: &Path) {
    let text = "#usda 1.0\n\
(\n    defaultPrim = \"World\"\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\n\
def Xform \"World\"\n{\n\
\x20   def Xform \"Mask\" {}\n}\n";
    std::fs::write(path, text).expect("write transfer level");
}

/// A `RunConfig` whose `levels` lists the per-level cave assembly so the driver's
/// `LevelTable` maps the id `"cave"` (the parent-dir name) to its path.
fn config_with_levels(start: &Path, transfer: &Path, cave: &Path) -> idtx_runtime_core::RunConfig {
    let mut cfg = idtx_runtime_core::RunConfig::new(start.to_path_buf(), idtx_runtime_core::RunMode::Desktop);
    cfg.transfer_level = Some(transfer.to_path_buf());
    cfg.levels = vec![cave.to_path_buf()];
    cfg
}

/// Drive the driver to idle, bounded by a deadline (loads are not sub-ms).
fn drive_to_idle(driver: &mut TransitionDriver, sessions: &mut SessionSet) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while driver.is_transitioning() {
        driver.poll(sessions);
        assert!(Instant::now() < deadline, "transition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Assert the active current session's `/World/Player` sits at `want` (translate).
fn assert_player_translate(sessions: &mut SessionSet, want: [f64; 3]) {
    let world = sessions
        .current_mut()
        .get_world_transform("/World/Player")
        .expect("player world transform");
    let t = world[3];
    assert!(
        (f64::from(t[0]) - want[0]).abs() < 1e-3
            && (f64::from(t[1]) - want[1]).abs() < 1e-3
            && (f64::from(t[2]) - want[2]).abs() < 1e-3,
        "player not at expected spawn: got {:?}, want {want:?}",
        [t[0], t[1], t[2]]
    );
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn driver_masks_loads_swaps_and_places_player() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-driver-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");

    let start = dir.join("start.usda");
    let transfer = dir.join("transfer.usda");
    // The cave assembly lives at <bundle>/levels/cave/level.usda so its level id
    // (the parent dir name) is "cave".
    let cave = dir.join("levels/cave/level.usda");
    let spawn_translate = [5.0, 2.0, -3.0];
    write_start(&start);
    write_transfer(&transfer);
    write_cave(&cave, spawn_translate);

    let cfg = config_with_levels(&start, &transfer, &cave);

    // Boot the session set exactly as `run_headless` does: start as current, the
    // transfer/mask scene resident.
    let current = open(&start, &device, &queue);
    let transfer_session = open(&transfer, &device, &queue);
    let mut sessions = SessionSet::new(current, Some(transfer_session));

    let mut driver = build_transition_driver(&cfg, device.clone(), queue.clone(), [64, 64], "");
    assert!(!driver.is_transitioning());

    // Seed a pending request for level id "cave" (default spawn).
    sessions.current_mut().post_pending_level_transition(LevelTransitionRequest {
        target: "cave".to_owned(),
        spawn: None,
    });

    // Frame 1: the driver drains the request, resolves "cave", cuts to the
    // transfer scene, and starts the async load (it does NOT poll the loader this
    // step), so the set is deterministically masked right after.
    driver.poll(&mut sessions);
    assert!(driver.is_transitioning(), "a load must be in flight");
    assert_eq!(
        sessions.active(),
        ActiveSession::Transfer,
        "the driver must cut to the transfer scene while loading"
    );
    assert!(
        sessions.active_session().stage().prim_at_path("/World/Mask").is_some(),
        "mid-load the active session must be the transfer (mask) scene"
    );
    assert!(
        sessions
            .active_session()
            .stage()
            .prim_at_path("/World/level/Start")
            .is_none(),
        "the transfer scene must not carry the start level's prim"
    );
    // The outgoing start level is still resident as `current` during the load.
    assert!(
        sessions.current().stage().prim_at_path("/World/level/Start").is_some(),
        "the start level stays resident (un-ticked) until cutover"
    );

    // Drive to completion, capturing the session the cutover hands back. `poll`
    // returns `Some` ONLY at cutover (exactly once); every other poll is `None`.
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut outgoing_at_cutover = None;
    while driver.is_transitioning() {
        if let Some(outgoing) = driver.poll(&mut sessions) {
            assert!(
                outgoing_at_cutover.is_none(),
                "the cutover must hand back the outgoing session exactly once"
            );
            outgoing_at_cutover = Some(outgoing);
        }
        assert!(Instant::now() < deadline, "transition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }

    // The session handed back at cutover is the OUTGOING start level — so a caller
    // that drives its own behavior host can tear those behaviors down against this
    // session's OWN world before dropping it (see `SessionSet::set_current`),
    // instead of against the new level's world where stale entity handles collide.
    let outgoing = outgoing_at_cutover.expect("the cutover must hand back the outgoing session");
    assert!(
        outgoing.stage().prim_at_path("/World/level/Start").is_some(),
        "the outgoing session returned at cutover must be the start level"
    );
    assert!(
        outgoing.stage().prim_at_path("/World/level/Spawn").is_none(),
        "the outgoing session must NOT be the swapped-in cave level"
    );
    drop(outgoing);

    // Cutover: the active current session is now the cave level, the old start
    // session was replaced (not merged).
    assert_eq!(sessions.active(), ActiveSession::Level);
    assert!(
        sessions
            .active_session()
            .stage()
            .prim_at_path("/World/level/Spawn")
            .is_some(),
        "after the swap the current session must expose the cave spawn prim"
    );
    assert!(
        sessions
            .active_session()
            .stage()
            .prim_at_path("/World/level/Start")
            .is_none(),
        "the start level's prim must be gone — the session was replaced, not merged"
    );

    // The player was placed at the cave spawn (authoring is on via dev-deps).
    // `get_world_transform` is USD row-major; translation is row 3.
    let player_world = sessions
        .current_mut()
        .get_world_transform("/World/Player")
        .expect("player world transform");
    let t = player_world[3];
    assert!(
        (f64::from(t[0]) - spawn_translate[0]).abs() < 1e-3
            && (f64::from(t[1]) - spawn_translate[1]).abs() < 1e-3
            && (f64::from(t[2]) - spawn_translate[2]).abs() < 1e-3,
        "player was not placed at the cave spawn: got translate {:?}, want {spawn_translate:?}",
        [t[0], t[1], t[2]]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn driver_drops_unresolved_target_and_stays_on_current_level() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-driver-neg-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");

    let start = dir.join("start.usda");
    let transfer = dir.join("transfer.usda");
    let cave = dir.join("levels/cave/level.usda");
    write_start(&start);
    write_transfer(&transfer);
    write_cave(&cave, [5.0, 2.0, -3.0]);

    let cfg = config_with_levels(&start, &transfer, &cave);
    let current = open(&start, &device, &queue);
    let transfer_session = open(&transfer, &device, &queue);
    let mut sessions = SessionSet::new(current, Some(transfer_session));
    let mut driver = build_transition_driver(&cfg, device.clone(), queue.clone(), [64, 64], "");

    // Seed an unresolvable target id.
    sessions.current_mut().post_pending_level_transition(LevelTransitionRequest {
        target: "nowhere".to_owned(),
        spawn: None,
    });

    driver.poll(&mut sessions);

    // The request is consumed and dropped; no transition starts; the set stays on
    // the current level (active never left Level, request cleared).
    assert!(!driver.is_transitioning(), "an unresolved target must not start a load");
    assert_eq!(sessions.active(), ActiveSession::Level);
    assert!(
        sessions.current_mut().take_pending_level_transition().is_none(),
        "the unresolved request must have been cleared, not left pending"
    );
    assert!(
        sessions
            .active_session()
            .stage()
            .prim_at_path("/World/level/Start")
            .is_some(),
        "the current level must still be the start level"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression (controller-mesh loss across a level transition): the session
/// layer the boot path injects — on the real runtime, the XR controller device
/// `Model` `over` onto the rig's bare pose Xforms — must be re-applied to every
/// level the driver swaps in. Before the fix the driver opened swapped-in levels
/// with `""`, so the injected `over` was dropped and the controllers rendered
/// nothing after the first transition.
///
/// Rather than resolve a real device mesh, this authors a recognisable marker
/// prim through the same session-layer seam (`over "World" { def Xform
/// "InjectedMarker" }`) and asserts it survives into the swapped-in cave level —
/// proving the driver forwards its `session_layer` to the `StageLoader`.
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn driver_applies_session_layer_to_swapped_in_level() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-layer-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");

    let start = dir.join("start.usda");
    let transfer = dir.join("transfer.usda");
    let cave = dir.join("levels/cave/level.usda");
    write_start(&start);
    write_transfer(&transfer);
    write_cave(&cave, [5.0, 2.0, -3.0]);

    let cfg = config_with_levels(&start, &transfer, &cave);

    // The injected session layer: a bare `over "World" { … }` block wrapped in a
    // USDA header exactly as the runtime wraps `controller_model_over_layer`
    // (see `idtx_runtime_core::player::controller_injection_session_layer`).
    let session_layer = "#usda 1.0\n(\n)\n\nover \"World\"\n{\n    def Xform \"InjectedMarker\" {}\n}\n";

    let current = open(&start, &device, &queue);
    let transfer_session = open(&transfer, &device, &queue);
    let mut sessions = SessionSet::new(current, Some(transfer_session));

    let mut driver =
        build_transition_driver(&cfg, device.clone(), queue.clone(), [64, 64], session_layer);

    sessions.current_mut().post_pending_level_transition(LevelTransitionRequest {
        target: "cave".to_owned(),
        spawn: None,
    });

    driver.poll(&mut sessions);
    drive_to_idle(&mut driver, &mut sessions);

    // The swap completed onto the cave level …
    assert_eq!(sessions.active(), ActiveSession::Level);
    assert!(
        sessions.active_session().stage().prim_at_path("/World/level/Spawn").is_some(),
        "after the swap the current session must be the cave level"
    );
    // … and it carries the prim authored by the forwarded session layer. Before
    // the fix (driver opened swapped-in levels with `\"\"`) this is absent — which
    // is exactly how the injected controller `Model` was lost.
    assert!(
        sessions.active_session().stage().prim_at_path("/World/InjectedMarker").is_some(),
        "the boot session layer must be re-applied to the swapped-in level"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Sanity: `StageLoader::try_recv` is non-blocking even immediately after a
/// driver starts a load (guards against the driver accidentally blocking the
/// frame loop). Uses the loader directly to keep the check cheap.
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn loader_try_recv_is_non_blocking() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-loader-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");
    let cave = dir.join("levels/cave/level.usda");
    write_cave(&cave, [1.0, 0.0, 0.0]);

    let loader = StageLoader::request(
        &cave,
        device,
        queue,
        [64, 64],
        TextureDecodeConfig::FULL_RESOLUTION,
        RenderWorldUnitScale::METRE,
        "",
    );
    // Must return immediately (loading is not sub-millisecond).
    assert!(matches!(loader.try_recv(), Poll::Pending | Poll::Ready(_)));

    let _ = std::fs::remove_dir_all(&dir);
}

// --- global-state carry across a swap ------------------------------------

/// (a) A blob value set on the driver's `GlobalState` before a transition is
/// still readable from `driver.global_state()` after the cutover — proving the
/// carry lives on the **driver**, not the dropped-and-replaced session.
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn global_state_blob_survives_a_level_swap() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-blob-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");

    let start = dir.join("start.usda");
    let transfer = dir.join("transfer.usda");
    let cave = dir.join("levels/cave/level.usda");
    write_start(&start);
    write_transfer(&transfer);
    write_cave(&cave, [5.0, 2.0, -3.0]);

    let cfg = config_with_levels(&start, &transfer, &cave);
    let current = open(&start, &device, &queue);
    let transfer_session = open(&transfer, &device, &queue);
    let mut sessions = SessionSet::new(current, Some(transfer_session));
    let mut driver = build_transition_driver(&cfg, device.clone(), queue.clone(), [64, 64], "");

    // Carry experience state on the driver before the transition.
    driver.global_state_mut().set("score", vec![7, 7, 7]);
    assert_eq!(driver.global_state().get("score").as_deref(), Some(&[7, 7, 7][..]));

    sessions
        .current_mut()
        .post_pending_level_transition(LevelTransitionRequest {
            target: "cave".to_owned(),
            spawn: None,
        });

    driver.poll(&mut sessions);
    drive_to_idle(&mut driver, &mut sessions);

    // The session was dropped and replaced, but the blob on the driver persists.
    assert_eq!(sessions.active(), ActiveSession::Level);
    assert_eq!(
        driver.global_state().get("score").as_deref(),
        Some(&[7, 7, 7][..]),
        "the blob must survive the swap because it lives on the driver, not the session"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// (b) A `GlobalState` player-placement override set before a transition beats
/// the target level's `/World/level/Spawn` at cutover, and is consumed (cleared)
/// — a one-shot override.
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn placement_override_beats_spawn_and_is_consumed() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-override-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");

    let start = dir.join("start.usda");
    let transfer = dir.join("transfer.usda");
    let cave = dir.join("levels/cave/level.usda");
    let cave_spawn = [5.0, 2.0, -3.0];
    write_start(&start);
    write_transfer(&transfer);
    write_cave(&cave, cave_spawn);

    let cfg = config_with_levels(&start, &transfer, &cave);
    let current = open(&start, &device, &queue);
    let transfer_session = open(&transfer, &device, &queue);
    let mut sessions = SessionSet::new(current, Some(transfer_session));
    let mut driver = build_transition_driver(&cfg, device.clone(), queue.clone(), [64, 64], "");

    // A known world transform (USD row-major; translation is row 3) that differs
    // from the cave spawn, so the assertion distinguishes the two placement paths.
    let known_translate = [9.0_f64, 4.0, -2.0];
    let known = [
        [1.0_f32, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [9.0, 4.0, -2.0, 1.0],
    ];
    driver.global_state_mut().set_player_placement(known);

    sessions
        .current_mut()
        .post_pending_level_transition(LevelTransitionRequest {
            target: "cave".to_owned(),
            spawn: None,
        });

    driver.poll(&mut sessions);
    drive_to_idle(&mut driver, &mut sessions);

    // The override won over the cave spawn...
    assert_player_translate(&mut sessions, known_translate);
    // ...and was consumed (one-shot).
    assert!(
        driver.global_state().player_placement().is_none(),
        "the placement override must be cleared after it is consumed at cutover"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// (c) A round-trip A→B→A with NO override lands the player at **each** level's
/// own `/World/level/Spawn` — placement precedence falls back to the spawn.
#[test]
#[ignore = "requires OpenUSD link + headless GPU"]
fn round_trip_without_override_uses_each_level_spawn() {
    let (device, queue) = headless_gpu();
    let dir = std::env::temp_dir().join(format!("idtx-xition-roundtrip-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("levels/forest")).expect("temp dirs");
    std::fs::create_dir_all(dir.join("levels/cave")).expect("temp dirs");

    let transfer = dir.join("transfer.usda");
    // Both levels are registered targets, each with its own spawn.
    let forest = dir.join("levels/forest/level.usda");
    let cave = dir.join("levels/cave/level.usda");
    let forest_spawn = [1.0, 1.0, 1.0];
    let cave_spawn = [5.0, 2.0, -3.0];
    write_transfer(&transfer);
    write_cave(&forest, forest_spawn);
    write_cave(&cave, cave_spawn);

    // Register both forest and cave; boot with forest as the current level.
    let mut cfg =
        idtx_runtime_core::RunConfig::new(forest.clone(), idtx_runtime_core::RunMode::Desktop);
    cfg.transfer_level = Some(transfer.clone());
    cfg.levels = vec![forest.clone(), cave.clone()];

    let current = open(&forest, &device, &queue);
    let transfer_session = open(&transfer, &device, &queue);
    let mut sessions = SessionSet::new(current, Some(transfer_session));
    let mut driver = build_transition_driver(&cfg, device.clone(), queue.clone(), [64, 64], "");

    // A → B (forest → cave): lands at the cave spawn.
    sessions
        .current_mut()
        .post_pending_level_transition(LevelTransitionRequest {
            target: "cave".to_owned(),
            spawn: None,
        });
    driver.poll(&mut sessions);
    drive_to_idle(&mut driver, &mut sessions);
    assert_player_translate(&mut sessions, cave_spawn);

    // B → A (cave → forest): lands at the forest spawn.
    sessions
        .current_mut()
        .post_pending_level_transition(LevelTransitionRequest {
            target: "forest".to_owned(),
            spawn: None,
        });
    driver.poll(&mut sessions);
    drive_to_idle(&mut driver, &mut sessions);
    assert_player_translate(&mut sessions, forest_spawn);

    let _ = std::fs::remove_dir_all(&dir);
}
