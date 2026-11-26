use idtx_usd::{PurposeFilter, RenderPurpose, StageService};
use std::collections::BTreeMap;

fn open_stage(text: &str) -> (tempfile::TempDir, StageService) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("purposes.usda");
    std::fs::write(&path, text).unwrap();
    let stage = StageService::open(path).unwrap();
    (dir, stage)
}

fn filters() -> impl Iterator<Item = PurposeFilter> {
    (0..8).map(|bits| PurposeFilter {
        guide: bits & 1 != 0,
        proxy: bits & 2 != 0,
        render: bits & 4 != 0,
    })
}

const PURPOSES: &str = r#"#usda 1.0
def Xform "World" {
    def Cube "Unassigned" {}
    def Cube "ExplicitDefault" {
        uniform token purpose = "default"
    }
    def Xform "Guide" {
        uniform token purpose = "guide"
        def Cube "Inherited" {}
        def Xform "ToProxy" {
            uniform token purpose = "proxy"
            def Cube "Inherited" {}
        }
        def Cube "ToRender" {
            uniform token purpose = "render"
        }
        def Xform "Reset" {
            uniform token purpose = "default"
            def Cube "Inherited" {}
        }
        def "UntypedBridge" {
            uniform token purpose = "render"
            def Cube "Inherited" {}
        }
    }
    def Xform "Proxy" {
        uniform token purpose = "proxy"
        def Cube "Inherited" {}
    }
    def Xform "Render" {
        uniform token purpose = "render"
        def Cube "Inherited" {}
    }
    def "Untyped" {
        uniform token purpose = "guide"
        def Cube "Unassigned" {}
    }
    def Cube "Unknown" {
        uniform token purpose = "other"
    }
}
"#;

fn expected_purposes() -> BTreeMap<&'static str, RenderPurpose> {
    use RenderPurpose::{Default, Guide, Proxy, Render};
    [
        ("/World", Default),
        ("/World/Unassigned", Default),
        ("/World/ExplicitDefault", Default),
        ("/World/Guide", Guide),
        ("/World/Guide/Inherited", Guide),
        ("/World/Guide/ToProxy", Proxy),
        ("/World/Guide/ToProxy/Inherited", Proxy),
        ("/World/Guide/ToRender", Render),
        ("/World/Guide/Reset", Default),
        ("/World/Guide/Reset/Inherited", Default),
        ("/World/Guide/UntypedBridge", Guide),
        ("/World/Guide/UntypedBridge/Inherited", Guide),
        ("/World/Proxy", Proxy),
        ("/World/Proxy/Inherited", Proxy),
        ("/World/Render", Render),
        ("/World/Render/Inherited", Render),
        ("/World/Untyped", Default),
        ("/World/Untyped/Unassigned", Default),
        ("/World/Unknown", Default),
    ]
    .into_iter()
    .collect()
}

#[test]
fn purpose_filter_accepts_all_eight_combinations_and_always_accepts_default() {
    for filter in filters() {
        assert!(filter.matches(RenderPurpose::Default));
        let unrestricted = !filter.guide && !filter.proxy && !filter.render;
        assert_eq!(
            filter.matches(RenderPurpose::Guide),
            unrestricted || filter.guide
        );
        assert_eq!(
            filter.matches(RenderPurpose::Proxy),
            unrestricted || filter.proxy
        );
        assert_eq!(
            filter.matches(RenderPurpose::Render),
            unrestricted || filter.render
        );
    }
}

#[test]
fn traversal_filters_computed_purpose_without_pruning_descendants_or_changing_topology() {
    let (_dir, stage) = open_stage(PURPOSES);
    let topology = stage.topology().to_vec();
    let expected = expected_purposes();
    let actual: BTreeMap<_, _> = topology
        .iter()
        .filter(|record| !record.is_pseudo_root)
        .map(|record| (stage.path_for(record.prim).unwrap(), record.purpose))
        .collect();
    assert_eq!(
        actual, expected,
        "topology must carry computed, inherited purposes"
    );
    for filter in filters() {
        let paths = stage.traverse_prim_paths(filter).unwrap();
        let actual: std::collections::BTreeSet<_> = paths.iter().map(String::as_str).collect();
        let selected: std::collections::BTreeSet<_> = expected
            .iter()
            .filter(|(_, purpose)| filter.matches(**purpose))
            .map(|(path, _)| *path)
            .collect();
        assert_eq!(actual, selected, "{filter:?}");
        assert_eq!(paths.len(), actual.len(), "no duplicate paths");
        assert_eq!(
            stage.topology(),
            topology,
            "filtering must not alter the scene tree"
        );
    }
}

#[test]
fn viewport_projection_uses_inherited_imageable_purposes() {
    let (_dir, mut stage) = open_stage(PURPOSES);
    let expected = expected_purposes();
    let projection = stage.project_viewport().unwrap();
    for visibility in &projection.visibility {
        let path = stage.path_for(visibility.prim).unwrap();
        assert_eq!(visibility.purpose, expected[path], "{path}");
    }
    assert_eq!(
        projection.visibility.len(),
        expected.len() - 2,
        "only the two untyped prims are not imageable"
    );
    assert!(
        projection
            .diagnostics
            .iter()
            .any(|d| d.contains("unknown imageable purpose other"))
    );
}

#[test]
fn native_instance_proxy_purposes_respect_root_inheritance_and_child_overrides() {
    let (_dir, mut stage) = open_stage(
        r#"#usda 1.0
def Xform "World" {
    class "Model" {
        def Cube "Inherited" {}
        def Cube "Default" {
            uniform token purpose = "default"
        }
        def Cube "Proxy" {
            uniform token purpose = "proxy"
        }
    }
    def Xform "Guide" (
        instanceable = true
        references = </World/Model>
    ) {
        uniform token purpose = "guide"
    }
    def Xform "Render" (
        instanceable = true
        references = </World/Model>
    ) {
        uniform token purpose = "render"
    }
}
"#,
    );
    let projection = stage.project_viewport().unwrap();
    assert_eq!(projection.native_instances.len(), 2);
    for instance in &projection.native_instances {
        let instance_path = stage.path_for(instance.prim).unwrap();
        let expected_inherited = if instance_path.ends_with("/Guide") {
            RenderPurpose::Guide
        } else {
            RenderPurpose::Render
        };
        let purposes: BTreeMap<_, _> = instance
            .purposes
            .iter()
            .map(|(prim, purpose)| {
                (
                    stage.path_for(*prim).unwrap().rsplit('/').next().unwrap(),
                    *purpose,
                )
            })
            .collect();
        assert_eq!(
            purposes,
            BTreeMap::from([
                ("Inherited", expected_inherited),
                ("Default", RenderPurpose::Default),
                ("Proxy", RenderPurpose::Proxy),
            ])
        );
        for filter in filters() {
            let paths = stage.traverse_prim_paths(filter).unwrap();
            for (name, purpose) in &purposes {
                assert_eq!(
                    paths.contains(&format!("{instance_path}/{name}")),
                    filter.matches(*purpose),
                    "{instance_path}/{name}: {filter:?}"
                );
            }
        }
    }
}

#[cfg(feature = "authoring")]
#[test]
fn purpose_edits_invalidate_inherited_topology_and_change_traversal_results() {
    let (_dir, mut stage) = open_stage(PURPOSES);
    let guide = PurposeFilter {
        guide: true,
        ..PurposeFilter::default()
    };
    assert!(
        stage
            .traverse_prim_paths(guide)
            .unwrap()
            .contains(&"/World/Guide/Inherited".to_owned())
    );
    stage
        .author_session_property(
            "/World/Guide",
            "purpose",
            &idtx_usd::PropertyValue::Token("render".to_owned()),
        )
        .unwrap();
    let changes = stage.drain_changes().unwrap().unwrap();
    assert!(
        changes
            .changes
            .iter()
            .any(|change| change.domains.contains(idtx_usd::DirtyDomains::TOPOLOGY))
    );
    stage.project_topology().unwrap();
    let inherited = stage
        .topology()
        .iter()
        .find(|record| stage.path_for(record.prim) == Some("/World/Guide/Inherited"))
        .unwrap();
    assert_eq!(inherited.purpose, RenderPurpose::Render);
    assert!(
        !stage
            .traverse_prim_paths(guide)
            .unwrap()
            .contains(&"/World/Guide/Inherited".to_owned())
    );
}
