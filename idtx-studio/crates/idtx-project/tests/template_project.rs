//! Validates the on-disk baseline project template shipped at
//! `templates/projects/baseline/`.
//!
//! That folder is the canonical starting point for a new Conductor project
//! (copy → edit, see its README). This test guards the template against schema
//! drift: it must always load with the current `idtx-project` model and satisfy
//! the invariants the template promises.
//!
//! Pure-Rust — `idtx-project` does not link the USD C++ SDK — so it runs
//! anywhere with `cargo test -p idtx-project`.

use std::path::{Path, PathBuf};

use idtx_project::{IdtxProject, PROJECT_SCHEMA_VERSION};

/// Absolute path to the shipped baseline project template directory.
fn template_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <workspace>/crates/idtx-project
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates/projects/baseline")
        .canonicalize()
        .expect("baseline project template directory exists")
}

#[test]
fn template_is_recognized_as_a_project_dir() {
    assert!(
        IdtxProject::is_project_dir(&template_dir()),
        "template dir must contain a project.toml"
    );
}

#[test]
fn template_manifest_loads_and_satisfies_invariants() {
    let manifest_file = template_dir().join("project.toml");
    let project = IdtxProject::load(&manifest_file).expect("template project.toml loads");

    let m = project.manifest();

    // Schema version must match what this build understands (never a future one).
    assert_eq!(
        m.version, PROJECT_SCHEMA_VERSION,
        "template must target the current schema version"
    );

    // A non-empty display name (the template sets one explicitly).
    assert!(
        !project.name().trim().is_empty(),
        "template must resolve a non-empty project name"
    );

    // The default stage must be configured AND resolve inside the sandbox to a
    // file that actually exists.
    let stage = project
        .default_stage_abs()
        .expect("template default_stage must resolve inside the project root");
    assert!(
        stage.is_file(),
        "resolved default stage must exist on disk: {}",
        stage.display()
    );
    assert_eq!(
        stage.extension().and_then(|e| e.to_str()),
        Some("usda"),
        "template default stage is a .usda file"
    );

    // The declared asset root must be present and exist on disk.
    assert!(
        m.asset_roots.iter().any(|p| p == Path::new("assets")),
        "template must declare the \"assets\" asset root"
    );
    assert!(
        template_dir().join("assets").is_dir(),
        "template assets/ directory must exist"
    );

    // The baseline leaves the player stage unset — it uses the toolbox's
    // built-in player by default.
    assert!(
        !m.has_player_stage(),
        "baseline must default to the built-in player (player_stage unset)"
    );

    // A positive unit scale (the loader clamps non-positive to 1.0, but the
    // template authors it explicitly).
    assert!(
        m.unit_scale.meters_per_unit() > 0.0,
        "meters_per_unit must be positive"
    );
}

#[test]
fn template_is_discoverable_from_a_nested_file() {
    // Discovery from a file *inside* the project (the stage) must walk up to the
    // template root — the backbone of copy-to-scratch workflows.
    let hint = template_dir().join("stages/level1.usda");
    let found = IdtxProject::discover(&hint)
        .expect("discover does not error")
        .expect("a project.toml is found by walking up from the stage file");
    assert_eq!(
        found.root().canonicalize().expect("root canonicalizes"),
        template_dir(),
        "discovery must resolve to the template root"
    );
}
