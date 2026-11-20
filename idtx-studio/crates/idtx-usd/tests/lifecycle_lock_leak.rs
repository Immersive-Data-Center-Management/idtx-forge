//! Regression: an authoring (or read) call that `OpenUSD` *rejects* must not leak
//! the C++ shim's process-global `stage_lifecycle_mutex`.
//!
//! The shim is compiled without `/EHsc`, so a C++ exception that unwinds across
//! the cxx `extern "C"` boundary runs `catch` handlers but does NOT destroy
//! automatic objects. A function that held the lifecycle lock through a bare RAII
//! `std::unique_lock`/`std::shared_lock` and then threw therefore leaked the lock
//! forever — the next lock acquisition (any later edit, projection, or even
//! handle teardown) blocked, hanging and crashing the app. See
//! `crates/idtx-usd-sys/cxx/usd_shim.cpp` (`with_stage_lifecycle_lock`).
//!
//! Each case triggers a C++-rejected operation, asserts it returns `Err`, then
//! performs a *subsequent* lock-requiring operation (a shared-lock projection AND
//! an exclusive-lock edit) plus handle teardown — all of which would deadlock if
//! the lock had leaked. The whole case runs on a worker thread with a watchdog
//! join-timeout so a regression fails as a timeout instead of hanging forever.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and the
//! platform library path); the whole `idtx-usd` crate links USD, so this binary
//! only builds/runs where that is satisfied. Stages are authored into unique temp
//! paths, so no committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use idtx_usd::{PropertyValue, StageService, StructuralCommand};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A unique, not-yet-existing `.usda` path under the system temp dir.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-lock-leak-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Run `body` on a worker thread, failing the test if it does not finish within
/// `timeout`. A leaked lifecycle lock makes the worker block forever on its next
/// lock acquisition; the watchdog converts that hang into a prompt panic. The
/// stuck worker (if any) is abandoned — a leaked *process-global* lock would wedge
/// every later case too, so there is nothing useful to recover.
fn with_watchdog(name: &'static str, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            body();
            let _ = tx.send(());
        })
        .expect("spawn watchdog worker");
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(()) => worker.join().expect("watchdog worker panicked"),
        // The sender dropped without signaling: the worker panicked (e.g. an
        // `expect` in the scenario setup, not a deadlock). Surface that panic.
        Err(mpsc::RecvTimeoutError::Disconnected) => match worker.join() {
            Ok(()) => panic!("{name}: worker exited without completing"),
            Err(payload) => std::panic::resume_unwind(payload),
        },
        // The worker is still blocked after the timeout — a leaked lifecycle lock
        // makes the next lock acquisition hang forever. The stuck worker is
        // abandoned; a leaked process-global lock would wedge every later case too.
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "{name}: a lock-requiring operation did not return within 30s — the \
             lifecycle lock was leaked by a rejected call (deadlock regression)"
        ),
    }
}

/// After a rejected call, prove the lifecycle lock is still free: a shared-lock
/// read projection and an exclusive-lock edit must both complete, and so must
/// handle teardown (`drop`, which reacquires the exclusive lock). Any of these
/// would hang on a leaked lock.
fn assert_stage_still_usable_then_drop(mut stage: StageService, probe_prim: &str) {
    // Shared-lock reader.
    stage.project_topology().expect("reproject after rejection");
    // Exclusive-lock writer.
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: probe_prim.to_owned(),
            type_name: "Xform".to_owned(),
        })
        .expect("author a new prim after rejection");
    stage
        .drain_changes()
        .expect("drain after post-rejection edit");
    assert!(
        stage.prim_at_path(probe_prim).is_none()
            || stage.project_topology().is_ok(),
        "stage remains queryable after rejection"
    );
    // Teardown reacquires the exclusive lock — would deadlock if it leaked.
    drop(stage);
}

/// The exact user scenario: `RemoveOverride` on an override that carries authored
/// data. The target prim is composed from a reference (so it has no root-layer
/// spec); `CreateOverride` authors an inert over, `SetProperty` gives it data, and
/// `RemoveOverride` is then refused by `OpenUSD` while holding the exclusive lock.
#[test]
fn remove_over_carrying_data_rejection_does_not_leak_lock() {
    with_watchdog("remove_over_carrying_data", || {
        let asset_path = scratch_path("rm-over-asset");
        {
            let mut asset =
                StageService::create_new(&asset_path, "Y", 1.0).expect("create asset");
            asset
                .apply_structural_command(&StructuralCommand::Define {
                    path: "/World/Inner".to_owned(),
                    type_name: "Sphere".to_owned(),
                })
                .expect("define asset inner");
            asset.save_stage(&asset_path).expect("save asset stage");
        }

        let path = scratch_path("rm-over-main");
        let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main stage");
        stage
            .apply_structural_command(&StructuralCommand::Define {
                path: "/World/Ref".to_owned(),
                type_name: "Xform".to_owned(),
            })
            .expect("define /World/Ref");
        stage.drain_changes().expect("drain define");
        stage.project_topology().expect("reproject after define");
        let reffer = stage.prim_at_path("/World/Ref").expect("resolve /World/Ref");
        stage
            .apply_structural_command(&StructuralCommand::AddReference {
                prim: reffer,
                asset_path: asset_path.to_string_lossy().into_owned(),
                target_path: None,
            })
            .expect("add reference");
        stage.drain_changes().expect("drain reference");
        stage.project_topology().expect("reproject");

        let inner = "/World/Ref/Inner";
        stage
            .apply_structural_command(&StructuralCommand::CreateOverride {
                path: inner.to_owned(),
            })
            .expect("author inert over on the referenced prim");
        stage.drain_changes().expect("drain create-over");
        let inner_prim = stage.prim_at_path(inner).expect("composed prim identity");
        stage
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim: inner_prim,
                property: "primvars:displayColor".to_owned(),
                value: PropertyValue::Color3f([1.0, 0.0, 0.0]),
            })
            .expect("author data onto the over");
        stage.drain_changes().expect("drain set-property");

        // OpenUSD rejects this inside the exclusive lifecycle lock.
        stage
            .apply_structural_command(&StructuralCommand::RemoveOverride {
                path: inner.to_owned(),
            })
            .expect_err("remove-over must refuse an override carrying authored data");

        assert_stage_still_usable_then_drop(stage, "/World/ProbeAfterRemoveOver");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&asset_path);
    });
}

/// `RemoveOverride` on a plain `def` prim (not a root-layer over) is rejected by
/// the shim while holding the exclusive lock. No fixtures needed.
#[test]
fn remove_over_on_def_rejection_does_not_leak_lock() {
    with_watchdog("remove_over_on_def", || {
        let path = scratch_path("rm-over-def");
        let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
        stage
            .apply_structural_command(&StructuralCommand::Define {
                path: "/World/Foo".to_owned(),
                type_name: "Xform".to_owned(),
            })
            .expect("define /World/Foo");
        stage.drain_changes().expect("drain define");

        stage
            .apply_structural_command(&StructuralCommand::RemoveOverride {
                path: "/World/Foo".to_owned(),
            })
            .expect_err("remove-over must refuse a root-owned def");

        assert_stage_still_usable_then_drop(stage, "/World/ProbeAfterDefReject");
        let _ = std::fs::remove_file(&path);
    });
}

/// `Define` on a path that already exists is rejected by the shim while holding
/// the exclusive lock.
#[test]
fn define_existing_rejection_does_not_leak_lock() {
    with_watchdog("define_existing", || {
        let path = scratch_path("define-existing");
        let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
        stage
            .apply_structural_command(&StructuralCommand::Define {
                path: "/World/Dup".to_owned(),
                type_name: "Xform".to_owned(),
            })
            .expect("define /World/Dup");
        stage.drain_changes().expect("drain define");

        stage
            .apply_structural_command(&StructuralCommand::Define {
                path: "/World/Dup".to_owned(),
                type_name: "Xform".to_owned(),
            })
            .expect_err("defining an existing path must be refused");

        assert_stage_still_usable_then_drop(stage, "/World/ProbeAfterDefineDup");
        let _ = std::fs::remove_file(&path);
    });
}
