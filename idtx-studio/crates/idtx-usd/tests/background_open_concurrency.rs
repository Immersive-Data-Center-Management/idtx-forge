//! Regression: a background stage open must NOT block a foreground read
//! projection of a *different* stage.
//!
//! The C++ shim guards stage lifecycle with ONE process-global
//! `stage_lifecycle_mutex` (`crates/idtx-usd-sys/cxx/usd_shim.cpp`): writers take
//! it exclusively, read projections share it. `open_stage_with_session` (the
//! async `StageLoader` path) used to take the EXCLUSIVE side across the entire —
//! possibly multi-second, remote — compose, so every foreground shared-lock read
//! of a resident stage blocked for the whole open. On an OpenXR / visionOS level
//! transition that froze the render thread into a CompositorServices
//! `cp_frame_end_submission` abort / watchdog kill.
//!
//! The fix reclassifies that open as a *reader* (it composes a brand-new stage,
//! performs no Reload and no authoring, and only touches internally-synchronized
//! OpenUSD global registries): it now takes the SHARED side, so a background open
//! runs concurrently with foreground reads while still mutually-excluding true
//! writers. This test pins that: with the `test-support` in-lock delay hook
//! holding stage B's open for ~1s, a concurrent `project_topology` read of stage A
//! must return in a few ms (shared) rather than blocking ~the remaining hold
//! (exclusive).
//!
//! Requires the OpenUSD runtime at link+run time AND the `test-support` feature
//! (which defines `IDTX_TEST_SUPPORT`, compiling the delay hook):
//! `IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib \
//!   cargo test -p idtx-usd --features test-support -- --ignored background_open`

#![cfg(feature = "test-support")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use idtx_usd::StageService;

static NEXT: AtomicU64 = AtomicU64::new(1);
static OPEN_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A unique, not-yet-existing `.usda` path under the system temp dir.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "idtx-openlock-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&p);
    p
}

/// A background `open_stage_with_session` of stage B (held ~1s under the shared
/// lifecycle lock via the `test-support` delay hook) must not block a concurrent
/// `project_topology` read of the resident stage A.
#[test]
#[ignore = "requires OpenUSD link + the test-support in-lock delay hook"]
fn background_open_does_not_block_foreground_read() {
    let _serial = OPEN_TEST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let path_a = scratch_path("a");
    let path_b = scratch_path("b");
    // `create_new` saves to disk before returning, so `path_b` exists on disk for
    // the worker to reopen; the handle is dropped (the file remains).
    StageService::create_new(&path_b, "Y", 1.0).expect("create stage B on disk");

    // Foreground stage A, opened + warmed so the timed read is steady-state.
    let mut a = StageService::create_new(&path_a, "Y", 1.0).expect("create+open stage A");
    a.project_topology().expect("warm stage A projection");

    // Arm the in-lock delay so stage B's open HOLDS the shared lifecycle lock ~1s.
    const HOLD_MS: u64 = 1000;
    // SAFETY: set before the worker spawns and removed after it joins; no other
    // thread opens a stage in this test, so the process-global env is uncontended.
    unsafe { std::env::set_var("IDTX_TEST_OPEN_HOLD_MS", HOLD_MS.to_string()) };

    let (entered_tx, entered_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let b_path = path_b.clone();
    let worker = std::thread::Builder::new()
        .name("bg-open".to_owned())
        .spawn(move || {
            let _ = entered_tx.send(()); // about to enter the (slow) open
            let b = StageService::open_with_session(&b_path, "", true).expect("open stage B");
            let _ = done_tx.send(());
            drop(b);
        })
        .expect("spawn background-open worker");

    // Wait until the worker reached the open, then a grace so it is INSIDE the held
    // critical section (sleeping under the shared lock) when we time the read.
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker reached the open");
    std::thread::sleep(Duration::from_millis(250));

    // Sanity: B's open must still be mid-hold — otherwise the delay hook is inactive
    // (built without `--features test-support`) and the test would prove nothing.
    assert!(
        done_rx.try_recv().is_err(),
        "stage B's open completed before the timed read — the test-support delay \
         hook is inactive (missing --features test-support?) or HOLD_MS is too small"
    );

    // Time a foreground SHARED-lock read of stage A while B's open holds the lock.
    let started = Instant::now();
    a.project_topology().expect("read stage A during stage B's open");
    let elapsed = started.elapsed();

    worker.join().expect("background-open worker joined");
    // SAFETY: the worker has joined; the test is single-threaded again.
    unsafe { std::env::remove_var("IDTX_TEST_OPEN_HOLD_MS") };
    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);

    // With the SHARED-side open the read runs concurrently (a few ms); with the old
    // EXCLUSIVE open it would block ~the remaining hold (~750ms). 300ms cleanly
    // separates the two regimes with wide margin.
    assert!(
        elapsed < Duration::from_millis(300),
        "foreground read blocked {elapsed:?} behind a concurrent background stage \
         open — open_stage_with_session must take the SHARED lifecycle lock, not \
         exclusive (regression of the level-transition read-freeze fix)"
    );
}

#[test]
#[cfg(feature = "authoring")]
#[ignore = "requires OpenUSD link + test-support delay hook"]
fn background_open_does_not_block_private_session_writes() {
    let _serial = OPEN_TEST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd/concurrency");
    std::fs::create_dir_all(&directory).unwrap();
    let suffix = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let path_a = directory.join(format!("writer-{suffix}.usda"));
    let path_b = directory.join(format!("loader-{suffix}.usda"));
    StageService::create_new(&path_b, "Y", 1.0).unwrap();
    let mut stage = StageService::create_new(&path_a, "Y", 1.0).unwrap();
    let mut matrix = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    stage.author_session_transform("/World", matrix).unwrap();
    let root = stage.root_layer_text().unwrap();
    // Test process serialization protects this process-wide delay hook.
    unsafe { std::env::set_var("IDTX_TEST_OPEN_HOLD_MS", "900") };
    let (done, receiver) = mpsc::channel();
    let load_path = path_b.clone();
    let worker = std::thread::spawn(move || {
        let loaded = StageService::open_with_session(load_path, "", true).unwrap();
        done.send(()).unwrap();
        loaded
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(receiver.try_recv().is_err(), "delay hook must still be active");
    let start = Instant::now();
    matrix[12] = 12.0;
    stage.author_session_transform("/World", matrix).unwrap();
    stage.project_topology().unwrap();
    let elapsed = start.elapsed();
    let loaded = worker.join().unwrap();
    unsafe { std::env::remove_var("IDTX_TEST_OPEN_HOLD_MS") };
    assert_eq!(stage.root_layer_text().unwrap(), root);
    // Exercise an in-lock error followed by another valid access.
    assert!(stage.author_session_transform("/Missing", matrix).is_err());
    stage.author_session_transform("/World", matrix).unwrap();
    drop(loaded);
    drop(stage);
    let _ = std::fs::remove_file(path_a);
    let _ = std::fs::remove_file(path_b);
    assert!(elapsed < Duration::from_millis(50), "foreground private write/read blocked for {elapsed:?}");
}

#[test]
#[cfg(feature = "authoring")]
#[ignore = "requires OpenUSD link + test-support"]
fn same_handle_projections_and_shared_root_reload_are_serialized() {
    let _serial = OPEN_TEST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = scratch_path("stress");
    let mut stage = StageService::create_new(&path, "Y", 1.0).unwrap();
    let mut peer = StageService::open(&path).unwrap();
    stage.test_stage_access_stress(&mut peer, 200).unwrap();
    drop(stage);
    drop(peer);
    let _ = std::fs::remove_file(path);
}
