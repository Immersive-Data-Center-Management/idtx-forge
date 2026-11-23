//! Native USD composition oracles for runtime object bindings.
#![cfg(feature = "authoring")]
use idtx_usd::{StageService, StructuralCommand};

fn binding(stage: &StageService, path: &str, name: &str) -> idtx_usd::RelationshipBinding {
    stage
        .relationship_bindings(stage.prim_at_path(path).unwrap())
        .unwrap()
        .into_iter()
        .find(|b| b.name == name)
        .unwrap()
}

#[test]
#[ignore = "requires OpenUSD"]
fn references_namespace_forwarding_payloads_and_root_undo_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("asset.usda"),
        r#"#usda 1.0
(defaultPrim = "Experience")
def Xform "Experience" {
    def Scope "Coordinator" {}
    def Scope "Other" {}
    def Scope "Panel" {
        rel coordinator = </Experience/Coordinator>
        rel forwarded = </Experience/Panel.coordinator>
        rel empty = []
        rel invalid = </Experience/Missing>
        rel cycleA = </Experience/Panel.cycleB>
        rel cycleB = </Experience/Panel.cycleA>
        rel ordered = [</Experience/Other>, </Experience/Coordinator>]
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("root.usda"),
        r#"#usda 1.0
(defaultPrim = "Hunt")
def Xform "Hunt" {
    def Xform "A" (prepend references = @asset.usda@) {}
    def Xform "B" (prepend references = @asset.usda@) {}
    def Xform "Payload" (prepend payload = @asset.usda@) {}
    rel loadedTarget = </Hunt/Payload/Coordinator>
}
"#,
    )
    .unwrap();
    let mut stage = StageService::open(dir.path().join("root.usda")).unwrap();
    stage.project_topology().unwrap();
    for copy in ["A", "B"] {
        let path = format!("/Hunt/{copy}/Panel");
        let target = format!("/Hunt/{copy}/Coordinator");
        assert_eq!(
            binding(&stage, &path, "coordinator").targets,
            [target.clone()]
        );
        assert_eq!(
            binding(&stage, &path, "forwarded").targets,
            [target.clone()]
        );
        assert_eq!(
            binding(&stage, &path, "ordered").targets,
            [format!("/Hunt/{copy}/Other"), target]
        );
        assert!(binding(&stage, &path, "empty").targets.is_empty());
        assert_eq!(binding(&stage, &path, "invalid").statuses, [1]);
        assert!(!binding(&stage, &path, "cycleA").resolved);
    }
    let before = stage.root_layer_text().unwrap();
    stage
        .set_relationship_targets("/Hunt/A/Panel", "coordinator", &["/Hunt/A/Other".into()])
        .unwrap();
    let after = stage.root_layer_text().unwrap();
    assert_eq!(
        binding(&stage, "/Hunt/A/Panel", "forwarded").targets,
        ["/Hunt/A/Other"]
    );
    assert_eq!(
        binding(&stage, "/Hunt/B/Panel", "forwarded").targets,
        ["/Hunt/B/Coordinator"]
    );
    stage.apply_root_layer_delta(&after, &before).unwrap();
    assert_eq!(
        binding(&stage, "/Hunt/A/Panel", "forwarded").targets,
        ["/Hunt/A/Coordinator"]
    );
    stage.apply_root_layer_delta(&before, &after).unwrap();

    let payload = stage.prim_at_path("/Hunt/Payload").unwrap();
    stage.set_payload_loaded(payload, false).unwrap();
    assert_eq!(binding(&stage, "/Hunt", "loadedTarget").statuses, [2]);
    stage.set_payload_loaded(payload, true).unwrap();
    assert_eq!(binding(&stage, "/Hunt", "loadedTarget").statuses, [0]);

    let root = stage.prim_at_path("/Hunt").unwrap();
    stage
        .apply_structural_command(&StructuralCommand::Rename {
            prim: root,
            new_name: "Moved".into(),
        })
        .unwrap();
    stage.project_topology().unwrap();
    assert_eq!(
        binding(&stage, "/Moved/A/Panel", "forwarded").targets,
        ["/Moved/A/Other"]
    );
    assert_eq!(
        binding(&stage, "/Moved/B/Panel", "forwarded").targets,
        ["/Moved/B/Coordinator"]
    );
    let saved = dir.path().join("saved.usda");
    stage.export_root_layer(&saved).unwrap();
    let mut reopened = StageService::open(&saved).unwrap();
    reopened.project_topology().unwrap();
    assert_eq!(
        binding(&reopened, "/Moved/A/Panel", "forwarded").targets,
        ["/Moved/A/Other"]
    );
    assert_eq!(
        binding(&reopened, "/Moved/B/Panel", "forwarded").targets,
        ["/Moved/B/Coordinator"]
    );
}
