//! U1.6a — IDTX stage-kind tag authoring/reading via root-layer
//! `customLayerData` (roadmap Appendix A.1).
//!
//! `StageService::set_stage_kind` authors `customLayerData.idtx` (`stageKind`,
//! `schemaVersion`, optional `displayName`); the free `read_stage_kind` reads it
//! back from the root layer **without composing a stage**. These tests drive a
//! round-trip against a hermetic on-disk stage and assert the classification the
//! project browser (U1.6) relies on.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`).

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageService, read_stage_kind};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-stage-kind-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn set_and_read_level_stage_kind_roundtrips_with_display_name() {
    let path = scratch_path("level");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    stage
        .set_stage_kind("level", "Forest Clearing")
        .expect("author stage kind");
    stage.save_stage(&path).expect("save stage");

    let tag = read_stage_kind(&path).expect("read stage kind");
    assert!(tag.is_tagged(), "a stageKind was authored");
    assert_eq!(tag.stage_kind, "level");
    assert_eq!(tag.display_name, "Forest Clearing");
    assert_eq!(tag.schema_version, "1", "schemaVersion defaults to \"1\"");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn set_stage_kind_without_display_name_authors_no_display_name() {
    let path = scratch_path("assembly");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    stage
        .set_stage_kind("assembly", "")
        .expect("author stage kind");
    stage.save_stage(&path).expect("save stage");

    let tag = read_stage_kind(&path).expect("read stage kind");
    assert_eq!(tag.stage_kind, "assembly");
    assert!(
        tag.display_name.is_empty(),
        "empty display name authors no displayName key"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn untagged_stage_reads_as_not_tagged() {
    let path = scratch_path("untagged");
    let stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    stage.save_stage(&path).expect("save stage");

    let tag = read_stage_kind(&path).expect("read stage kind");
    assert!(!tag.is_tagged(), "a plain new stage carries no idtx tag");
    assert!(tag.stage_kind.is_empty());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn set_stage_kind_replaces_only_the_idtx_entry() {
    // Re-tagging a stage overwrites the kind without accumulating stale keys.
    let path = scratch_path("retag");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    stage
        .set_stage_kind("asset", "Prop")
        .expect("author asset kind");
    stage
        .set_stage_kind("level", "Arena")
        .expect("re-author level kind");
    stage.save_stage(&path).expect("save stage");

    let tag = read_stage_kind(&path).expect("read stage kind");
    assert_eq!(tag.stage_kind, "level", "latest kind wins");
    assert_eq!(tag.display_name, "Arena");

    let _ = std::fs::remove_file(&path);
}
