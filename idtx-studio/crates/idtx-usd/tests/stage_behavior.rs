//! Stage-level behavior assignment authoring/reading via root-layer
//! `customLayerData` (`idtx:stageBehavior` + `idtx:stageBehaviorRunScope`).
//!
//! `StageService::author_root_stage_behavior` writes the prim name + run-scope
//! token; `clear_root_stage_behavior` erases them; `StageMetadata` reads them
//! back after a reopen. `stage_behavior_compose_context` classifies how the
//! carrying layer is composed — `Standalone` when the stage is opened directly.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and the
//! platform library path).

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageBehaviorRunScope, StageComposeContext, StageService};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-stage-behavior-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "idtx-stage-behavior-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn author_and_read_stage_behavior_roundtrips() {
    let path = scratch_path("roundtrip");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    stage
        .author_root_stage_behavior("StageBehavior", StageBehaviorRunScope::Sublayer)
        .expect("author stage behavior");
    stage.save_stage(&path).expect("save stage");

    let reopened = StageService::open(&path).expect("reopen stage");
    let meta = reopened.metadata();
    assert_eq!(meta.stage_behavior_name.as_deref(), Some("StageBehavior"));
    assert_eq!(
        meta.stage_behavior_run_scope,
        Some(StageBehaviorRunScope::Sublayer)
    );
    // Opened directly → the root layer carries the assignment → standalone.
    assert_eq!(
        reopened.stage_behavior_compose_context(),
        StageComposeContext::Standalone
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn clear_stage_behavior_removes_both_keys() {
    let path = scratch_path("clear");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    stage
        .author_root_stage_behavior("StageBehavior", StageBehaviorRunScope::Always)
        .expect("author stage behavior");
    stage.clear_root_stage_behavior().expect("clear stage behavior");
    stage.save_stage(&path).expect("save stage");

    let reopened = StageService::open(&path).expect("reopen stage");
    let meta = reopened.metadata();
    assert_eq!(meta.stage_behavior_name, None);
    assert_eq!(meta.stage_behavior_run_scope, None);
    assert_eq!(
        reopened.stage_behavior_compose_context(),
        StageComposeContext::Unknown,
        "no stage behavior → unknown composition context"
    );

    let _ = std::fs::remove_file(&path);
}

/// Per-prim run-condition resolution in an assembly that *both* carries its own
/// root (standalone) stage behavior *and* payloads levels whose own layers carry
/// stage behaviors with different run scopes.
///
/// Regression guard: the resolver used to short-circuit globally — "the open
/// stage's root layer carries an assignment ⇒ standalone" — before ever looking
/// at the queried prim. That classified *every* prim (including the nested
/// payload behaviors) with the root's standalone scope, so a `sublayer`-scoped
/// payload behavior was wrongly admitted and a `payload`-scoped one was gated by
/// the wrong (root) scope. The fix classifies each prim by the owning node of
/// its *own* composed index.
#[test]
fn per_prim_resolution_does_not_let_root_override_payload_behaviors() {
    let dir = scratch_dir("per-prim");

    // A level carrying its own stage behavior with the given run scope. The
    // defaultPrim lets the assembly payload it without an explicit target path.
    let write_level = |file: &str, scope: &str| {
        std::fs::write(
            dir.join(file),
            format!(
                "#usda 1.0\n(\n    defaultPrim = \"Lvl\"\n    customLayerData = {{\n\
                 \x20       string \"idtx:stageBehavior\" = \"StageBehavior\"\n\
                 \x20       token \"idtx:stageBehaviorRunScope\" = \"{scope}\"\n    }}\n)\n\n\
                 def \"Lvl\"\n{{\n    def \"StageBehavior\"\n    {{\n    }}\n}}\n"
            ),
        )
        .expect("write level");
    };
    write_level("level_sublayer.usda", "sublayer");
    write_level("level_payload.usda", "payload");

    // Assembly root carries its OWN standalone stage behavior and payloads both
    // levels. `@./…@` resolves relative to this (assembly) layer's directory.
    let assembly = dir.join("assembly.usda");
    std::fs::write(
        &assembly,
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n    customLayerData = {\n\
         \x20       string \"idtx:stageBehavior\" = \"AssemblyBehavior\"\n\
         \x20       token \"idtx:stageBehaviorRunScope\" = \"standalone\"\n    }\n)\n\n\
         def \"World\"\n{\n\
         \x20   def \"ContentA\" (\n        payload = @./level_sublayer.usda@\n    )\n    {\n    }\n\
         \x20   def \"ContentB\" (\n        payload = @./level_payload.usda@\n    )\n    {\n    }\n\
         }\n\n\
         def \"AssemblyBehavior\"\n{\n}\n",
    )
    .expect("write assembly");

    let stage = StageService::open(&assembly).expect("open assembly with payloads");

    // The assembly's OWN behavior is standalone-scoped and composed from the open
    // stage's root layer → standalone context → permitted.
    assert!(
        stage.stage_behavior_permitted_for_prim("/AssemblyBehavior"),
        "assembly's own standalone behavior must run in the standalone stage"
    );

    // Nested payload behavior with a `sublayer` run scope: owned by the payloaded
    // level layer, composed via a payload arc → payload context. `sublayer` does
    // NOT permit payload, so it must be rejected. Under the old global
    // short-circuit this was misclassified as standalone (root scope) and wrongly
    // admitted.
    assert!(
        !stage.stage_behavior_permitted_for_prim("/World/ContentA/StageBehavior"),
        "sublayer-scoped payload behavior must not run in a payload context"
    );

    // Nested payload behavior with a `payload` run scope: must be admitted in the
    // payload context on its OWN merit — not because the root happens to carry a
    // standalone assignment.
    assert!(
        stage.stage_behavior_permitted_for_prim("/World/ContentB/StageBehavior"),
        "payload-scoped payload behavior must run in a payload context"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
