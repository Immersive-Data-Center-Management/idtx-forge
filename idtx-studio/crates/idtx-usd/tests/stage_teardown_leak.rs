//! Stage teardown / `stage_lifecycle_mutex` leak-regression test.
//!
//! Guards the C++ teardown path in `~StageHandle::Impl`
//! (`crates/idtx-usd-sys/cxx/usd_shim.cpp`): every stage open must be balanced
//! by a *complete* teardown — the change observer revoked from `TfNotice` and
//! the `UsdStage` reset, all under the exclusive stage-lifecycle lock. A leak
//! (a handle whose `~Impl` never ran, or an observer left registered) would
//! show up as the process-wide live-handle count failing to return to its
//! baseline.
//!
//! This lives in its own integration-test binary on purpose: the live-handle
//! counter is a process-global `static`, so a dedicated process isolates it
//! from the concurrent stage opens/drops in `idtx-usd`'s inline unit tests and
//! lets the exact-count assertions below stay deterministic.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`); the whole `idtx-usd` crate links USD, so this binary
//! only builds where that is satisfied. The stage is authored into a temp file,
//! so no committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageService, live_stage_handle_count};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Author a minimal valid stage into a unique temp path.
fn scratch_stage() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-teardown-leak-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(
        &path,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\ndef Xform \"World\"\n{\n}\n",
    )
    .expect("write minimal stage");
    path
}

#[test]
fn stage_teardown_balances_live_handle_count() {
    let source = scratch_stage();
    let baseline = live_stage_handle_count();

    // A single open adds exactly one live handle; a read projection (shared
    // lifecycle lock) must not perturb the count.
    {
        let mut stage = StageService::open(&source).expect("open scratch stage");
        assert_eq!(
            live_stage_handle_count(),
            baseline + 1,
            "one open must add exactly one live handle"
        );
        stage.project_viewport().expect("project once");
        assert_eq!(
            live_stage_handle_count(),
            baseline + 1,
            "a read projection must not change the live-handle count"
        );
    }
    // Scope exit dropped the StageService → ran ~Impl → back to baseline.
    assert_eq!(
        live_stage_handle_count(),
        baseline,
        "dropping the only stage must return the live count to baseline"
    );

    // The regression assertion: many open→drop cycles must never drift. A leak
    // would climb monotonically; a double-teardown would underflow. Each cycle
    // must return to exactly the baseline.
    for i in 0..64 {
        let stage = StageService::open(&source).expect("cycle open");
        assert_eq!(
            live_stage_handle_count(),
            baseline + 1,
            "cycle {i}: open must add exactly one"
        );
        drop(stage);
        assert_eq!(
            live_stage_handle_count(),
            baseline,
            "cycle {i}: drop must return to baseline (teardown ran to completion)"
        );
    }

    // Overlapping lifetimes: N concurrently-live stages report N above
    // baseline, and dropping them all returns to baseline.
    let stages: Vec<StageService> = (0..8)
        .map(|_| StageService::open(&source).expect("overlap open"))
        .collect();
    assert_eq!(
        live_stage_handle_count(),
        baseline + 8,
        "8 concurrently-live stages must add exactly 8"
    );
    drop(stages);
    assert_eq!(
        live_stage_handle_count(),
        baseline,
        "dropping all 8 must return to baseline — no handle leaked"
    );

    let _ = std::fs::remove_file(&source);
}
