//! U1.2 — structural authoring commands that had no Rust-level coverage.
//!
//! `Define`/`Delete`/`CreateOverride`/`RemoveOverride`/`SetProperty` already
//! have inline unit tests in `idtx-usd`; the topology-mutating and
//! composition-arc commands — `Rename`, `Reparent`, `AddReference`,
//! `AddPayload` — were only exercised through the C++ shim and the match arm.
//! This binary drives each one through `StageService::apply_structural_command`
//! against a hermetic stage authored on disk and asserts the resulting topology.
//!
//! Requires the `OpenUSD` runtime at link+run time (set `IDTX_USD_DIR` and
//! `DYLD_LIBRARY_PATH`); the whole `idtx-usd` crate links USD, so this binary
//! only builds/runs where that is satisfied. Stages are authored into unique
//! temp paths, so no committed fixture is needed.

use std::sync::atomic::{AtomicU64, Ordering};

use idtx_usd::{StageService, StructuralCommand};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A unique, not-yet-existing `.usda` path under the system temp dir.
fn scratch_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "idtx-structural-{tag}-{}-{}.usda",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

/// Apply a structural command, drain the resulting notice batch, and reproject
/// the whole topology — the sequence a session performs after a mutating edit.
fn apply_and_reproject(stage: &mut StageService, command: &StructuralCommand) {
    stage
        .apply_structural_command(command)
        .expect("apply structural command");
    stage
        .drain_changes()
        .expect("drain change notice")
        .expect("change batch");
    stage.project_topology().expect("reproject topology");
}

/// `true` when a prim with the given absolute path resolves in the current
/// (reprojected) stage.
fn has_prim(stage: &StageService, path: &str) -> bool {
    stage.prim_at_path(path).is_some()
}

#[test]
fn rename_prim_moves_it_to_a_new_sibling_path() {
    let path = scratch_path("rename");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Define {
            path: "/World/Foo".to_owned(),
            type_name: "Xform".to_owned(),
        },
    );
    assert!(has_prim(&stage, "/World/Foo"));

    let foo = stage
        .prim_at_path("/World/Foo")
        .expect("resolve /World/Foo");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Rename {
            prim: foo,
            new_name: "Bar".to_owned(),
        },
    );

    assert!(has_prim(&stage, "/World/Bar"), "renamed prim at new path");
    assert!(!has_prim(&stage, "/World/Foo"), "old path is gone");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reparent_prim_moves_its_subtree_under_a_new_parent() {
    let path = scratch_path("reparent");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create stage");
    for prim_path in ["/World/Src", "/World/Dst", "/World/Src/Child"] {
        apply_and_reproject(
            &mut stage,
            &StructuralCommand::Define {
                path: prim_path.to_owned(),
                type_name: "Xform".to_owned(),
            },
        );
    }
    assert!(has_prim(&stage, "/World/Src/Child"));

    let child = stage
        .prim_at_path("/World/Src/Child")
        .expect("resolve child");
    let dst = stage.prim_at_path("/World/Dst").expect("resolve dst");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Reparent {
            prim: child,
            new_parent: dst,
        },
    );

    assert!(
        has_prim(&stage, "/World/Dst/Child"),
        "child moved under new parent"
    );
    assert!(
        !has_prim(&stage, "/World/Src/Child"),
        "child no longer under old parent"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn add_reference_composes_the_external_asset_subtree() {
    // Author a referenceable asset with a distinctive child under its
    // defaultPrim, then reference it and confirm the child composes in.
    let asset_path = scratch_path("ref-asset");
    {
        let mut asset = StageService::create_new(&asset_path, "Y", 1.0).expect("create asset");
        apply_and_reproject(
            &mut asset,
            &StructuralCommand::Define {
                path: "/World/Inner".to_owned(),
                type_name: "Sphere".to_owned(),
            },
        );
        asset.save_stage(&asset_path).expect("save asset stage");
    }

    let path = scratch_path("ref-main");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main stage");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Define {
            path: "/World/Ref".to_owned(),
            type_name: "Xform".to_owned(),
        },
    );

    let reffer = stage
        .prim_at_path("/World/Ref")
        .expect("resolve /World/Ref");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::AddReference {
            prim: reffer,
            asset_path: asset_path.to_string_lossy().into_owned(),
            target_path: None,
        },
    );

    assert!(
        has_prim(&stage, "/World/Ref/Inner"),
        "referenced defaultPrim's child composes under the referencing prim"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&asset_path);
}

#[test]
fn add_payload_composes_the_external_asset_subtree() {
    let asset_path = scratch_path("payload-asset");
    {
        let mut asset = StageService::create_new(&asset_path, "Y", 1.0).expect("create asset");
        apply_and_reproject(
            &mut asset,
            &StructuralCommand::Define {
                path: "/World/Inner".to_owned(),
                type_name: "Sphere".to_owned(),
            },
        );
        asset.save_stage(&asset_path).expect("save asset stage");
    }

    let path = scratch_path("payload-main");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main stage");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Define {
            path: "/World/Load".to_owned(),
            type_name: "Xform".to_owned(),
        },
    );

    let loader = stage
        .prim_at_path("/World/Load")
        .expect("resolve /World/Load");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::AddPayload {
            prim: loader,
            asset_path: asset_path.to_string_lossy().into_owned(),
            target_path: None,
        },
    );

    assert!(
        has_prim(&stage, "/World/Load/Inner"),
        "payloaded defaultPrim's child composes under the loading prim"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&asset_path);
}

#[test]
fn add_reference_with_target_path_composes_a_non_default_root_prim() {
    // An asset with NO defaultPrim and several root `def` prims — the
    // scene-tree-drop case this feature targets. A plain (untargeted) arc would
    // compose nothing; naming an explicit target prim (`@asset@</Beta>`) must
    // compose that prim's subtree under the referencing prim.
    let asset_path = scratch_path("target-asset");
    std::fs::write(
        &asset_path,
        "#usda 1.0\n()\n\n\
         def Xform \"Alpha\"\n{\n    def Sphere \"InnerA\" {}\n}\n\
         def Xform \"Beta\"\n{\n    def Sphere \"InnerB\" {}\n}\n",
    )
    .expect("write no-defaultPrim asset");

    let path = scratch_path("target-main");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main stage");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Define {
            path: "/World/Ref".to_owned(),
            type_name: "Xform".to_owned(),
        },
    );

    let reffer = stage
        .prim_at_path("/World/Ref")
        .expect("resolve /World/Ref");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::AddReference {
            prim: reffer,
            asset_path: asset_path.to_string_lossy().into_owned(),
            target_path: Some("/Beta".to_owned()),
        },
    );

    assert!(
        has_prim(&stage, "/World/Ref/InnerB"),
        "the targeted root prim's subtree composes under the referencing prim"
    );
    assert!(
        !has_prim(&stage, "/World/Ref/InnerA"),
        "the non-targeted root prim must not compose"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&asset_path);
}

/// The scene-tree drag-drop path ([`StudioSession::add_asset_child_arc`]) does
/// not payload onto the drop-target prim — it authors a **new child** under it
/// that carries the arc. This drives that composition primitive directly
/// (`Define` a nested `Xform` child, then `AddPayload` on the *child*) and
/// asserts the asset subtree composes under `<target>/<child>/…`, leaving the
/// drop-target prim itself free of the arc's contents.
#[test]
fn payload_on_a_nested_child_leaves_the_drop_target_arc_free() {
    let asset_path = scratch_path("child-payload-asset");
    {
        let mut asset = StageService::create_new(&asset_path, "Y", 1.0).expect("create asset");
        apply_and_reproject(
            &mut asset,
            &StructuralCommand::Define {
                path: "/World/Inner".to_owned(),
                type_name: "Sphere".to_owned(),
            },
        );
        asset.save_stage(&asset_path).expect("save asset stage");
    }

    let path = scratch_path("child-payload-main");
    let mut stage = StageService::create_new(&path, "Y", 1.0).expect("create main stage");
    // The drop-target prim the user releases onto.
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Define {
            path: "/World/Target".to_owned(),
            type_name: "Xform".to_owned(),
        },
    );
    // The nested child that carries the arc (named from the asset stem).
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::Define {
            path: "/World/Target/child_payload_asset".to_owned(),
            type_name: "Xform".to_owned(),
        },
    );
    let child = stage
        .prim_at_path("/World/Target/child_payload_asset")
        .expect("resolve nested child");
    apply_and_reproject(
        &mut stage,
        &StructuralCommand::AddPayload {
            prim: child,
            asset_path: asset_path.to_string_lossy().into_owned(),
            target_path: None,
        },
    );

    // The asset subtree composes under the *child*, not the drop target itself.
    assert!(
        has_prim(&stage, "/World/Target/child_payload_asset/Inner"),
        "payloaded content composes under the nested child"
    );
    assert!(
        !has_prim(&stage, "/World/Target/Inner"),
        "the drop-target prim itself carries no payloaded content"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&asset_path);
}
