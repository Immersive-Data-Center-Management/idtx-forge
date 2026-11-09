//! U1.5 — structural undo via root-layer USDA-text snapshot/restore.
//!
//! Structural edits (Define, DeletePrim, AddReference/Payload, rename) author
//! into the **root layer**, so a full-root-layer USDA-text snapshot taken before
//! the edit and another after it form a reversible pair: restoring the `before`
//! text un-does the edit, restoring the `after` text re-does it. This is the
//! uniform undo basis for edits that have no cheap semantic inverse.
//!
//! These tests exercise the primitive directly on `StageService`
//! (`root_layer_text` → structural edit → `replace_root_layer_text` → reproject),
//! asserting topology before/after each restore. This is the round-trip the
//! shell's `EditCommand::Structural` undo entry relies on.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`). Stages are authored into unique temp paths, so no
//! committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageService, StructuralCommand};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A unique, not-yet-existing `.usda` path under the system temp dir.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-structural-undo-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Define a typed prim, then drain the notice batch and reproject topology —
/// the sequence a session performs after a structural edit.
fn define_and_reproject(stage: &mut StageService, path: &str, type_name: &str) {
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: path.to_owned(),
            type_name: type_name.to_owned(),
        })
        .expect("define prim");
    let _ = stage.drain_changes();
    stage.project_topology().expect("reproject topology");
}

/// Restore a captured root-layer snapshot, then drain + reproject.
fn restore_and_reproject(stage: &mut StageService, snapshot: &str) {
    stage
        .replace_root_layer_text(snapshot)
        .expect("restore root layer");
    let _ = stage.drain_changes();
    stage.project_topology().expect("reproject topology");
}

/// `true` when a prim currently resolves at `path` in the projected topology.
fn exists(stage: &mut StageService, path: &str) -> bool {
    stage.prim_at_path(path).is_some()
}

#[test]
fn define_then_restore_before_snapshot_removes_the_new_prim() {
    let path = scratch_path("define-undo");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    // `create_new` already authors the `/World` default-root prim.

    // Snapshot the root layer *before* the structural edit.
    let before = stage.root_layer_text().expect("snapshot before");

    define_and_reproject(&mut stage, "/World/Foo", "Xform");
    assert!(exists(&mut stage, "/World/Foo"), "prim defined");

    // Snapshot *after* — this is the redo target.
    let after = stage.root_layer_text().expect("snapshot after");

    // Undo: restore the before-snapshot; the new prim disappears.
    restore_and_reproject(&mut stage, &before);
    assert!(
        !exists(&mut stage, "/World/Foo"),
        "restoring the before-snapshot undoes the define"
    );
    assert!(exists(&mut stage, "/World"), "parent survives undo");

    // Redo: restore the after-snapshot; the prim comes back.
    restore_and_reproject(&mut stage, &after);
    assert!(
        exists(&mut stage, "/World/Foo"),
        "restoring the after-snapshot redoes the define"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn delete_then_restore_before_snapshot_reinstates_the_prim() {
    let path = scratch_path("delete-undo");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    // `create_new` already authors the `/World` default-root prim.
    define_and_reproject(&mut stage, "/World/Bar", "Sphere");
    assert!(exists(&mut stage, "/World/Bar"), "prim defined");

    // Snapshot before the delete.
    let before = stage.root_layer_text().expect("snapshot before");

    let prim = stage.prim_at_path("/World/Bar").expect("resolve prim");
    stage
        .apply_structural_command(&StructuralCommand::Delete { prim })
        .expect("delete prim");
    let _ = stage.drain_changes();
    stage.project_topology().expect("reproject");
    assert!(!exists(&mut stage, "/World/Bar"), "prim deleted");

    let after = stage.root_layer_text().expect("snapshot after");

    // Undo the delete.
    restore_and_reproject(&mut stage, &before);
    assert!(
        exists(&mut stage, "/World/Bar"),
        "restoring the before-snapshot reinstates the deleted prim"
    );

    // Redo the delete.
    restore_and_reproject(&mut stage, &after);
    assert!(
        !exists(&mut stage, "/World/Bar"),
        "restoring the after-snapshot re-applies the delete"
    );

    let _ = std::fs::remove_file(&path);
}
