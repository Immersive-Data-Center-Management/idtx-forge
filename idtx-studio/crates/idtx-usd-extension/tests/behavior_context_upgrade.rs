//! Registration/application preserves an existing custom authored property.
use idtx_usd::{PropertyValue, StageService, StructuralCommand};

#[test]
#[ignore = "requires OpenUSD"]
fn behavior_context_upgrade_preserves_authored_custom_content_and_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.usda");
    std::fs::write(
        &path,
        r#"#usda 1.0
def Xform "Panel" {
    custom string idtx:behavior:context = '{"label":"legacy","count":3}' (
        documentation = "Keep this custom opinion"
    )
}
"#,
    )
    .unwrap();
    idtx_usd_extension::install().unwrap();
    let mut stage = StageService::open(&path).unwrap();
    stage.project_topology().unwrap();
    let panel = stage.prim_at_path("/Panel").unwrap();
    assert_eq!(
        stage
            .authored_string(panel, "idtx:behavior:context", 8192)
            .unwrap()
            .as_deref(),
        Some(r#"{"label":"legacy","count":3}"#)
    );
    stage
        .apply_structural_command(&StructuralCommand::ApplyApi {
            path: "/Panel".into(),
            schema: "IdtxBehaviorAPI".into(),
        })
        .unwrap();
    let text = stage.root_layer_text().unwrap();
    assert!(
        text.contains("custom string idtx:behavior:context"),
        "registration must not rewrite a custom spec"
    );
    assert!(text.contains("Keep this custom opinion"));
    let saved = dir.path().join("saved.usda");
    stage.export_root_layer(&saved).unwrap();
    let mut reopened = StageService::open(saved).unwrap();
    reopened.project_topology().unwrap();
    let panel = reopened.prim_at_path("/Panel").unwrap();
    assert_eq!(
        reopened
            .authored_string(panel, "idtx:behavior:context", 8192)
            .unwrap()
            .as_deref(),
        Some(r#"{"label":"legacy","count":3}"#)
    );
    for length in [8192, 8193] {
        let context = format!("\"{}\"", "x".repeat(length - 2));
        reopened
            .apply_structural_command(&StructuralCommand::SetProperty {
                prim: panel,
                property: "idtx:behavior:context".into(),
                value: PropertyValue::String(context.clone()),
            })
            .unwrap();
        let result = reopened.authored_string(panel, "idtx:behavior:context", 8192);
        if length == 8192 {
            assert_eq!(result.unwrap(), Some(context));
        } else {
            assert!(
                result.is_err(),
                "oversized context must fail, never truncate"
            );
        }
    }
}
