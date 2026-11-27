//! Interactive layer-stack mutation: set edit target, mute / unmute,
//! and reorder sublayers.
//!
//! Drives `StageService::apply_layer_edit` against hermetic stages built with
//! `StageService::create_new` and asserts the effect through
//! `layer_stack()` (the edit-target / muted flags) and the composed topology
//! (muting drops a contribution; reordering flips which sublayer opinion wins).
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`).

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{LayerArcKind, LayerEdit, StageService, StructuralCommand};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-layer-edit-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Author `/World/<name>` of `type_name` into a fresh stage and save it, so it
/// can be pulled in as a sublayer by another stage.
fn make_sublayer_file(tag: &str, prim_name: &str, type_name: &str) -> std::path::PathBuf {
    let path = scratch_path(tag);
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create sublayer stage");
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: format!("/World/{prim_name}"),
            type_name: type_name.to_owned(),
        })
        .expect("define sublayer prim");
    stage.drain_changes().expect("drain").expect("batch");
    stage.save_stage(&path).expect("save sublayer stage");
    path
}

fn add_sublayer(stage: &mut StageService, asset_path: &std::path::Path) {
    stage
        .apply_structural_command(&StructuralCommand::AddSublayer {
            asset_path: asset_path.to_string_lossy().into_owned(),
        })
        .expect("add sublayer");
    stage.drain_changes().expect("drain").expect("batch");
    stage.project_topology().expect("reproject");
}

#[test]
fn set_edit_target_switches_to_the_session_layer() {
    let path = scratch_path("edit-target");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");

    // The session layer is present but not the edit target by default.
    let stack = stage.layer_stack().expect("layer stack");
    let session = stack
        .iter()
        .find(|l| l.arc == LayerArcKind::Session)
        .expect("session layer present");
    assert!(
        !session.is_edit_target,
        "session is not edit target initially"
    );
    let session_id = session.identifier.clone();

    stage
        .apply_layer_edit(&LayerEdit::SetEditTarget {
            identifier: session_id.clone(),
        })
        .expect("set edit target");

    let stack = stage.layer_stack().expect("layer stack after");
    assert!(
        stack
            .iter()
            .any(|l| l.identifier == session_id && l.is_edit_target),
        "session layer is now the edit target"
    );
    assert!(
        stack.iter().filter(|l| l.is_edit_target).count() == 1,
        "exactly one edit target"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn mute_layer_drops_and_unmute_restores_its_contribution() {
    let sub = make_sublayer_file("mute-sub", "Muteable", "Xform");
    let path = scratch_path("mute-main");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main");
    add_sublayer(&mut stage, &sub);
    assert!(
        stage.prim_at_path("/World/Muteable").is_some(),
        "sublayer prim composes in before muting"
    );

    // Resolve the sublayer's identifier from the stack.
    let sub_id = stage
        .layer_stack()
        .expect("stack")
        .into_iter()
        .find(|l| l.arc == LayerArcKind::SubLayer)
        .expect("sublayer row")
        .identifier;

    stage
        .apply_layer_edit(&LayerEdit::Mute {
            identifier: sub_id.clone(),
        })
        .expect("mute");
    stage.drain_changes().expect("drain").expect("batch");
    stage.project_topology().expect("reproject after mute");
    assert!(
        stage.prim_at_path("/World/Muteable").is_none(),
        "muted sublayer drops its prim"
    );
    assert!(
        stage
            .layer_stack()
            .expect("stack")
            .iter()
            .any(|l| l.identifier == sub_id && l.is_muted),
        "layer row reports muted"
    );

    stage
        .apply_layer_edit(&LayerEdit::Unmute {
            identifier: sub_id.clone(),
        })
        .expect("unmute");
    stage.drain_changes().expect("drain").expect("batch");
    stage.project_topology().expect("reproject after unmute");
    assert!(
        stage.prim_at_path("/World/Muteable").is_some(),
        "unmuted sublayer restores its prim"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&sub);
}

#[test]
fn move_sublayer_changes_which_opinion_wins() {
    // Two sublayers author the SAME prim path with different types; the
    // stronger (earlier) sublayer's type wins. Reordering flips the winner.
    let sphere_sub = make_sublayer_file("order-sphere", "Shared", "Sphere");
    let cube_sub = make_sublayer_file("order-cube", "Shared", "Cube");

    let path = scratch_path("order-main");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main");
    add_sublayer(&mut stage, &sphere_sub); // index 0 — strongest
    add_sublayer(&mut stage, &cube_sub); // index 1 — weaker

    let type_of_shared = |stage: &StageService| {
        stage
            .topology()
            .iter()
            .find(|r| r.name == "Shared")
            .map(|r| r.type_name.clone())
    };
    assert_eq!(
        type_of_shared(&stage).as_deref(),
        Some("Sphere"),
        "strongest sublayer's type wins initially"
    );

    // Identify the cube sublayer row and move it stronger (earlier).
    let cube_id = stage
        .layer_stack()
        .expect("stack")
        .into_iter()
        .find(|l| {
            l.arc == LayerArcKind::SubLayer
                && l.real_path
                    .as_deref()
                    .is_some_and(|p| p.contains("order-cube"))
        })
        .expect("cube sublayer row")
        .identifier;

    stage
        .apply_layer_edit(&LayerEdit::MoveSublayer {
            identifier: cube_id,
            parent_identifier: String::new(), // root layer's sublayers
            stronger: true,
        })
        .expect("move cube sublayer stronger");
    stage.drain_changes().expect("drain").expect("batch");
    stage.project_topology().expect("reproject after reorder");

    assert_eq!(
        type_of_shared(&stage).as_deref(),
        Some("Cube"),
        "after reorder the cube sublayer wins"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&sphere_sub);
    let _ = std::fs::remove_file(&cube_sub);
}
