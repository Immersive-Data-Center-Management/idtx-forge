//! Native USD authored metadata and session reference transaction oracles.
#![cfg(feature = "authoring")]
use idtx_usd::{PropertyValue, SessionAttributeConnection, StageService};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    stage: StageService,
    dir: PathBuf,
}
impl Fixture {
    fn open() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "idtx-runtime-data-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("source.usda"),
            r#"#usda 1.0
def Xform "Source" (customData = {
    dictionary idtx = {
        dictionary assetOverview = {
            string label = "referenced"
            float[] offset = [1, 2, 3]
            bool enabled = true
        }
    }
}) {}
"#,
        )
        .unwrap();
        fs::write(
            dir.join("template.usda"),
            r#"#usda 1.0
(defaultPrim = "Template")
def Xform "Template" {
    custom string idtx:behavior:context = "{}"
    def Scope "Field" {
        custom string inputs:value = ""
    }
}
"#,
        )
        .unwrap();
        fs::write(dir.join("stage.usda"), r#"#usda 1.0
(defaultPrim = "World")
def Xform "World" {
    def Xform "A" (prepend references = @source.usda@</Source>) {
        def Scope "Data" {
            custom string outputs:value = "A"
        }
    }
    def Xform "B" (prepend references = @source.usda@</Source>
        customData = { dictionary idtx = { dictionary assetOverview = { string label = "override" } } }) {
        def Scope "Data" {
            custom string outputs:value = "B"
        }
    }
    def Xform "Missing" {}
    def Xform "Wrong" (customData = { dictionary idtx = { string assetOverview = "wrong-type"
        asset unsupported = @other.usda@ } }) {
        custom token idtx:behavior:context = "wrong"
    }
}
"#).unwrap();
        let mut stage = StageService::open(dir.join("stage.usda")).unwrap();
        stage.project_topology().unwrap();
        Self { stage, dir }
    }
    fn spawn(&mut self, parent: &str, context: &str, source: &str) {
        let template = self.dir.join("template.usda");
        self.stage
            .spawn_session_reference(
                &format!("/World/{parent}/Panel"),
                template.to_str().unwrap(),
                "/Template",
                glam::DMat4::from_translation(glam::DVec3::new(1.0, 2.0, 3.0)).to_cols_array(),
                context,
                &[SessionAttributeConnection {
                    child: "Field".into(),
                    property: "inputs:value".into(),
                    source: source.into(),
                }],
            )
            .unwrap();
    }
    fn project(&mut self) {
        let _ = self.stage.drain_changes().unwrap();
        self.stage.project_topology().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.dir.starts_with(std::env::temp_dir()) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

#[test]
fn authored_metadata_counts_references_merges_overrides_and_preserves_arrays() {
    let fixture = Fixture::open();
    let a = fixture.stage.prim_at_path("/World/A").unwrap();
    let b = fixture.stage.prim_at_path("/World/B").unwrap();
    let missing = fixture.stage.prim_at_path("/World/Missing").unwrap();
    let read = |prim| {
        serde_json::from_str::<serde_json::Value>(
            &fixture
                .stage
                .authored_custom_data_json(prim, "idtx:assetOverview", 8192)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(read(a)["label"], "referenced");
    assert_eq!(read(b)["label"], "override");
    assert_eq!(read(b)["offset"], serde_json::json!([1.0, 2.0, 3.0]));
    assert_eq!(read(a)["enabled"], true);
    assert_eq!(
        fixture
            .stage
            .authored_custom_data_json(missing, "idtx:assetOverview", 8192)
            .unwrap(),
        None
    );
    assert!(
        fixture
            .stage
            .authored_custom_data_json(a, "idtx:assetOverview", 16)
            .is_err()
    );
    let wrong = fixture.stage.prim_at_path("/World/Wrong").unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fixture
                .stage
                .authored_custom_data_json(wrong, "idtx:assetOverview", 8192)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        "wrong-type"
    );
    assert!(
        fixture
            .stage
            .authored_custom_data_json(wrong, "idtx:unsupported", 8192)
            .is_err()
    );
    assert_eq!(
        fixture
            .stage
            .authored_string(missing, "idtx:behavior:context", 8192)
            .unwrap(),
        None
    );
    assert!(
        fixture
            .stage
            .authored_string(wrong, "idtx:behavior:context", 8192)
            .is_err()
    );
}

#[test]
fn references_keep_independent_context_connections_and_root_layer_clean() {
    let mut fixture = Fixture::open();
    let before = fixture.stage.root_layer_text().unwrap();
    fixture.spawn("A", r#"{"label":"A"}"#, "/World/A/Data.outputs:value");
    fixture.spawn("B", r#"{"label":"B"}"#, "/World/B/Data.outputs:value");
    let batch = fixture
        .stage
        .drain_changes()
        .unwrap()
        .expect("retained topology notices");
    assert!(
        batch
            .changes
            .iter()
            .any(|change| change.domains.contains(idtx_usd::DirtyDomains::TOPOLOGY))
    );
    fixture.stage.project_topology().unwrap();
    for label in ["A", "B"] {
        let panel = fixture
            .stage
            .prim_at_path(&format!("/World/{label}/Panel"))
            .unwrap();
        assert_eq!(
            fixture
                .stage
                .authored_string(panel, "idtx:behavior:context", 8192)
                .unwrap()
                .unwrap(),
            format!(r#"{{"label":"{label}"}}"#)
        );
        assert_eq!(
            fixture.stage.get_local_transform_f64(panel).unwrap()[12..15],
            [1.0, 2.0, 3.0]
        );
        assert!(fixture.stage.prim_authored_visible(panel).unwrap());
        let field = fixture
            .stage
            .prim_at_path(&format!("/World/{label}/Panel/Field"))
            .unwrap();
        let graph = fixture.stage.compute_graph(field).unwrap();
        assert!(
            graph
                .nodes
                .iter()
                .flat_map(|node| &node.inputs)
                .any(|input| input.connections == [format!("/World/{label}/Data.outputs:value")])
        );
        fixture
            .stage
            .author_session_property(
                &format!("/World/{label}/Panel/Field"),
                "inputs:value",
                &PropertyValue::String(label.into()),
            )
            .unwrap();
    }
    assert_eq!(fixture.stage.root_layer_text().unwrap(), before);
    fixture.stage.erase_session_prim("/World/A/Panel").unwrap();
    fixture.project();
    assert!(fixture.stage.prim_at_path("/World/A/Panel").is_none());
    assert!(fixture.stage.prim_at_path("/World/B/Panel").is_some());
    fixture.spawn(
        "A",
        r#"{"label":"reopened"}"#,
        "/World/A/Data.outputs:value",
    );
    fixture.project();
    let reopened = fixture.stage.prim_at_path("/World/A/Panel").unwrap();
    assert_eq!(
        fixture
            .stage
            .authored_string(reopened, "idtx:behavior:context", 8192)
            .unwrap()
            .as_deref(),
        Some(r#"{"label":"reopened"}"#)
    );
    assert_eq!(fixture.stage.root_layer_text().unwrap(), before);
}

#[test]
fn invalid_spawn_rolls_back_without_dropping_earlier_notices() {
    let mut fixture = Fixture::open();
    let before = fixture.stage.root_layer_text().unwrap();
    fixture
        .stage
        .author_session_visibility("/World/A", false)
        .unwrap();
    let template = fixture.dir.join("template.usda");
    let bad_connection = SessionAttributeConnection {
        child: "Field".into(),
        property: "inputs:value".into(),
        source: "/World/B/missing.outputs:value".into(),
    };
    assert!(
        fixture
            .stage
            .spawn_session_reference(
                "/World/B/Panel",
                template.to_str().unwrap(),
                "/Template",
                glam::DMat4::IDENTITY.to_cols_array(),
                "{}",
                &[bad_connection]
            )
            .is_err()
    );
    assert!(
        fixture
            .stage
            .spawn_session_reference(
                "/World/B/Panel",
                template.to_str().unwrap(),
                "/Template",
                glam::DMat4::IDENTITY.to_cols_array(),
                "[]",
                &[]
            )
            .is_err()
    );
    // A conflicting matrix attribute fails after DefinePrim + reference,
    // exercising transaction rollback rather than only argument validation.
    let invalid_template = fixture.dir.join("bad-transform.usda");
    fs::write(
        &invalid_template,
        r#"#usda 1.0
def Xform "Template" {
    custom string xformOp:transform = "bad"
}
"#,
    )
    .unwrap();
    assert!(
        fixture
            .stage
            .spawn_session_reference(
                "/World/B/Panel",
                invalid_template.to_str().unwrap(),
                "/Template",
                glam::DMat4::IDENTITY.to_cols_array(),
                "{}",
                &[]
            )
            .is_err()
    );
    let batch = fixture.stage.drain_changes().unwrap().unwrap();
    assert!(
        batch
            .changes
            .iter()
            .any(|change| change.property_name.as_deref() == Some("visibility"))
    );
    fixture.stage.project_topology().unwrap();
    assert!(fixture.stage.prim_at_path("/World/B/Panel").is_none());
    assert!(
        !fixture
            .stage
            .prim_authored_visible(fixture.stage.prim_at_path("/World/A").unwrap())
            .unwrap()
    );
    assert_eq!(fixture.stage.root_layer_text().unwrap(), before);
}
