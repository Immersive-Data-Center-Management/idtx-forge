//! U1.1 — create-new / blank stage bridge fn + `StageService::create_new`.
//!
//! Exercises `create_new_stage` end-to-end: a fresh USD stage is authored on
//! disk with a default `/World` `Xform` root, the requested up-axis and
//! metres-per-unit, and `defaultPrim` set. Verifies the returned handle projects
//! the root prim and that the file persists so it can be reopened.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`); the whole `idtx-usd` crate links USD, so this binary
//! only builds/runs where that is satisfied. Stages are authored into unique
//! temp paths, so no committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::StageService;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A unique, not-yet-existing `.usda` path under the system temp dir.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-create-new-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // A previous run may have left the file; start clean.
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn create_new_authors_default_world_root() {
    let path = scratch_path("world");
    let stage = StageService::create_new(&path, "Y", 1.0).expect("create new stage");

    // Stage metadata reflects the authored defaults.
    let meta = stage.metadata();
    assert_eq!(meta.default_prim.as_deref(), Some("/World"));
    assert_eq!(meta.up_axis, "Y");
    assert!((meta.meters_per_unit - 1.0).abs() < f64::EPSILON);

    // The default root prim is projected as a typed Xform directly under the
    // pseudo-root.
    let world = stage
        .topology()
        .iter()
        .find(|r| r.name == "World")
        .expect("/World in topology");
    assert_eq!(world.type_name, "Xform");
    assert!(!world.is_pseudo_root);

    // The file was saved to disk (create_new persists before returning).
    assert!(path.is_file(), "new stage must exist on disk at {path:?}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn create_new_honours_up_axis_and_meters_per_unit() {
    let path = scratch_path("zup");
    let stage = StageService::create_new(&path, "Z", 0.01).expect("create Z-up cm stage");
    let meta = stage.metadata();
    assert_eq!(meta.up_axis, "Z");
    assert!((meta.meters_per_unit - 0.01).abs() < f64::EPSILON);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn create_new_persists_and_reopens() {
    let path = scratch_path("reopen");
    {
        let _stage = StageService::create_new(&path, "Y", 1.0).expect("create");
    }
    // Reopen the saved file with the normal open path and confirm the root prim
    // survived the round-trip to disk.
    let reopened = StageService::open(&path).expect("reopen saved stage");
    assert_eq!(reopened.metadata().default_prim.as_deref(), Some("/World"));
    assert!(reopened.topology().iter().any(|r| r.name == "World"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn create_new_with_empty_root_authors_no_default_prim() {
    let path = scratch_path("empty");
    let stage =
        StageService::create_new_with_root(&path, "Y", 1.0, "", "").expect("create empty-root");
    assert_eq!(stage.metadata().default_prim, None);
    // Only the pseudo-root exists.
    assert!(stage.topology().iter().all(|r| r.is_pseudo_root));
    let _ = std::fs::remove_file(&path);
}
