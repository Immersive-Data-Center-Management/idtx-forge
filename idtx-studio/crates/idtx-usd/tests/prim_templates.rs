//! U1.4 — typed prim template authoring (Camera / lights / Scope).
//!
//! `StageService::define_prim_from_template` authors a typed prim *and* its
//! schema-sensible default attributes in one call (a `Define` followed by a
//! `SetProperty` per template default). These tests drive each template against
//! a hermetic stage authored on disk and assert both the resulting prim type
//! (via the projected topology record) and the authored default values
//! (`eval_attribute`).
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`); the whole `idtx-usd` crate links USD, so this binary
//! only builds/runs where that is satisfied. Stages are authored into unique
//! temp paths, so no committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{PrimTemplate, StageService};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A unique, not-yet-existing `.usda` path under the system temp dir.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-prim-template-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Author a template at `path`, then drain the notice batch and reproject the
/// whole topology — the sequence a session performs after a mutating edit.
fn define_and_reproject(stage: &mut StageService, path: &str, template: PrimTemplate) {
    stage
        .define_prim_from_template(path, template)
        .expect("define prim from template");
    stage
        .drain_changes()
        .expect("drain change notice")
        .expect("change batch");
    stage.project_topology().expect("reproject topology");
}

/// The authored USD type name of the (already-projected) prim at `path`, read
/// from the projected topology record (the idiomatic short name, e.g.
/// `Camera`/`DistantLight`/`Scope`).
fn type_name_of(stage: &mut StageService, path: &str) -> String {
    let prim = stage.prim_at_path(path).expect("resolve prim");
    stage
        .project_topology()
        .expect("topology")
        .iter()
        .find(|record| record.prim == prim)
        .expect("topology record for prim")
        .type_name
        .clone()
}

/// Evaluate an attribute at default time and parse its scalar value.
fn scalar_at(stage: &StageService, path: &str, attr: &str) -> f64 {
    let prim = stage.prim_at_path(path).expect("resolve prim");
    let value = stage
        .eval_attribute(prim, attr, 0.0, true)
        .expect("eval_attribute call")
        .unwrap_or_else(|| panic!("{attr} should be authored on {path}"));
    value
        .value_repr
        .trim()
        .parse::<f64>()
        .unwrap_or_else(|_| panic!("parse {attr} value {:?}", value.value_repr))
}

#[test]
fn camera_template_authors_type_and_lens_defaults() {
    let path = scratch_path("camera");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    define_and_reproject(&mut stage, "/World/Camera", PrimTemplate::Camera);

    assert_eq!(
        type_name_of(&mut stage, "/World/Camera"),
        "Camera",
        "template authored a Camera prim"
    );
    // focalLength differs from the 50mm schema fallback, so a match proves the
    // default was actually authored (not just the schema default resolving in).
    assert!(
        (scalar_at(&stage, "/World/Camera", "focalLength") - 35.0).abs() < 1e-3,
        "focalLength default authored"
    );
    assert!(
        (scalar_at(&stage, "/World/Camera", "horizontalAperture") - 36.0).abs() < 1e-3,
        "horizontalAperture default authored"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn distant_light_template_authors_type_and_intensity() {
    let path = scratch_path("distant");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    define_and_reproject(&mut stage, "/World/Sun", PrimTemplate::DistantLight);

    assert_eq!(
        type_name_of(&mut stage, "/World/Sun"),
        "DistantLight",
        "template authored a DistantLight prim"
    );
    // intensity 3.0 differs from the 1.0 schema fallback.
    assert!(
        (scalar_at(&stage, "/World/Sun", "inputs:intensity") - 3.0).abs() < 1e-3,
        "inputs:intensity default authored"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rect_light_template_authors_type_and_size() {
    let path = scratch_path("rect");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    define_and_reproject(&mut stage, "/World/Panel", PrimTemplate::RectLight);

    assert_eq!(
        type_name_of(&mut stage, "/World/Panel"),
        "RectLight",
        "template authored a RectLight prim"
    );
    // width 2.0 differs from the 1.0 schema fallback.
    assert!(
        (scalar_at(&stage, "/World/Panel", "inputs:width") - 2.0).abs() < 1e-3,
        "inputs:width default authored"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn dome_light_template_authors_type_and_intensity() {
    let path = scratch_path("dome");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    define_and_reproject(&mut stage, "/World/Env", PrimTemplate::DomeLight);

    assert_eq!(
        type_name_of(&mut stage, "/World/Env"),
        "DomeLight",
        "template authored a DomeLight prim"
    );
    // intensity 1000.0 differs from the 1.0 schema fallback.
    assert!(
        (scalar_at(&stage, "/World/Env", "inputs:intensity") - 0.5).abs() < 1e-1,
        "inputs:intensity default authored"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn scope_template_authors_a_typed_grouping_prim_with_no_defaults() {
    let path = scratch_path("scope");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    define_and_reproject(&mut stage, "/World/Group", PrimTemplate::Scope);

    assert_eq!(
        type_name_of(&mut stage, "/World/Group"),
        "Scope",
        "template authored a Scope prim"
    );
    assert!(
        PrimTemplate::Scope.default_properties().is_empty(),
        "Scope carries no authored defaults"
    );
    let _ = std::fs::remove_file(&path);
}
