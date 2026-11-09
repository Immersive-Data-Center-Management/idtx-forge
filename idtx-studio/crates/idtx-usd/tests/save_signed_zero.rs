//! Regression: authoring a transform then saving to a **crate** (`.usdc`)
//! target — the Conductor in-place / Save-As path.
//!
//! Symptom: "Save failed: … saved root-layer semantics differ after official
//! reopen" after authoring a transform (or cylinder parameters) then saving a
//! stage in place. Root cause: the save-verification pre-check in
//! `export_layer_atomic` (usd_shim.cpp) compared the in-memory root-layer text
//! against the reopened file's text with a **raw byte** comparison, while every
//! other persistence check in the shim (`verify_persistence_locked`, the USDZ
//! path) tolerates a cosmetic IEEE-754 signed-zero difference via
//! `normalize_signed_zero`. A `.usd`/`.usdc` destination round-trips through the
//! usdc crate writer, which normalizes `-0` → `0`; on USD builds whose in-memory
//! usda writer keeps `-0`, the pre-check false-rejected a semantically identical
//! save. `compose_trs_matrix` / the bootstrap xformOp path can emit `-0.0` for
//! axis-aligned rotations, which is where the authored `-0` came from.
//!
//! The fix (a) aligns the pre-check with `verify_persistence_locked` (normalize
//! signed zero — safe because the authoritative check runs right after and still
//! enforces the full comparison), and (b) `canon_zero`s the transform-authoring
//! paths so no `-0` reaches the layer at all. This test guards the crate-save
//! round-trip end to end.
//!
//! Note: on USD builds whose usda writer already normalizes `-0` (e.g. the
//! 26.08 build used in CI here) the raw difference does not manifest, so this
//! test also passed before the fix on such a build; it still guards the
//! regression on builds that emit `-0`.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`) and the `authoring` feature (`--features authoring`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageService, TransformValues};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/authoring")
        .join(name)
}

fn work_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "idtx-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir work dir");
    dir
}

#[test]
fn save_to_crate_tolerates_signed_zero_from_matrix_transform() {
    let src = fixture("authoring_matrix_op.usda");
    if !src.exists() {
        // Fixture missing (nonstandard checkout) — treat as skip, matching the
        // other runtime-dependent authoring tests.
        return;
    }
    let work = work_dir("save-signed-zero");

    // Open the .usda fixture, then save to a .usdc (crate) destination — the
    // crate writer is the one that normalizes -0 and previously tripped the
    // raw-comparison pre-check.
    let src_copy = work.join("scene.usda");
    std::fs::copy(&src, &src_copy).expect("copy fixture");
    let dest = work.join("scene.usdc");

    let mut stage = StageService::open(&src_copy).expect("open matrix-op fixture");
    stage.project_topology().expect("project topology");
    let box_prim = stage.prim_at_path("/Root/Box").expect("box prim");

    // Author a transform through the single `xformOp:transform` (matrix) path.
    stage
        .apply_transform_edit(
            box_prim,
            TransformValues {
                translation: [1.0, 2.0, 3.0],
                rotation_xyz_degrees: [30.0, 45.0, 15.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .expect("author matrix transform");

    // Regression: previously Err(… "saved root-layer semantics differ after
    // official reopen") on builds whose usda writer keeps -0.
    stage
        .save_stage(&dest)
        .expect("crate save must tolerate cosmetic signed-zero difference");

    drop(stage);

    // Reopen the crate file and confirm the authored translation persisted.
    let mut reopened = StageService::open(&dest).expect("reopen crate save");
    reopened
        .project_topology()
        .expect("project topology reopen");
    let prim = reopened.prim_at_path("/Root/Box").expect("box reopen");
    let mat = reopened
        .get_local_transform(prim)
        .expect("get_local_transform");
    assert!(
        (mat[3][0] - 1.0_f32).abs() < 1e-4,
        "translation X must persist through crate save, got {}",
        mat[3][0],
    );

    let _ = std::fs::remove_dir_all(&work);
}
