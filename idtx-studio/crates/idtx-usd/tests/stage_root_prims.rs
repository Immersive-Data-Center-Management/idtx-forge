//! Root-layer composition-target reads (`read_stage_root_prims`): the
//! scene-tree drop flow reads a dropped stage's `defaultPrim` and root `def`
//! prim names — **without composing a stage** — to decide what prim a
//! reference/payload arc should target. A stage with no `defaultPrim` composes
//! nothing unless the arc names an explicit target prim.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`).

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::read_stage_root_prims;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn scratch_usda(tag: &str, source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-root-prims-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, source).expect("write scratch usda");
    path
}

#[test]
fn reads_authored_default_prim() {
    let path = scratch_usda(
        "default",
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\n\
         def Xform \"World\" {}\ndef Xform \"Other\" {}\n",
    );

    let targets = read_stage_root_prims(&path).expect("read root prims");
    assert_eq!(targets.default_prim.as_deref(), Some("World"));
    // Root `def` prims are still enumerated regardless of defaultPrim.
    assert_eq!(targets.root_def_prims, vec!["World", "Other"]);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn no_default_prim_lists_all_root_def_prims() {
    // A Houdini-style export: several root `def` prims, no defaultPrim.
    let path = scratch_usda(
        "multi",
        "#usda 1.0\n()\n\n\
         def Xform \"Alpha\" {}\ndef Xform \"Beta\" {}\ndef Scope \"Gamma\" {}\n",
    );

    let targets = read_stage_root_prims(&path).expect("read root prims");
    assert_eq!(targets.default_prim, None);
    assert_eq!(targets.root_def_prims, vec!["Alpha", "Beta", "Gamma"]);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn over_and_class_roots_are_excluded() {
    // Only `def` roots are composable arc targets; `over`/`class` are skipped.
    let path = scratch_usda(
        "specifier",
        "#usda 1.0\n()\n\n\
         def Xform \"Real\" {}\nover \"Edit\" {}\nclass \"Base\" {}\n",
    );

    let targets = read_stage_root_prims(&path).expect("read root prims");
    assert_eq!(targets.default_prim, None);
    assert_eq!(
        targets.root_def_prims,
        vec!["Real"],
        "over/class roots compose nothing and must be excluded"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn single_root_def_prim_without_default() {
    let path = scratch_usda(
        "single",
        "#usda 1.0\n()\n\ndef Xform \"Thing\" {}\n",
    );

    let targets = read_stage_root_prims(&path).expect("read root prims");
    assert_eq!(targets.default_prim, None);
    assert_eq!(targets.root_def_prims, vec!["Thing"]);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn unreadable_path_returns_empty_targets() {
    let missing = std::env::temp_dir().join("idtx-root-prims-does-not-exist.usda");
    let _ = std::fs::remove_file(&missing);

    let targets = read_stage_root_prims(&missing).expect("read root prims");
    assert_eq!(targets.default_prim, None);
    assert!(targets.root_def_prims.is_empty());
}
