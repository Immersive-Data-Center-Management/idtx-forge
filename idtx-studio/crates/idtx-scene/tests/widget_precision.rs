//! Untrusted field precision stays bounded at initial hydration and live edits.
#![cfg(feature = "usd-extension")]
use idtx_scene::{PopulatorRegistry, SceneHydrator, WidgetPanelComp};
use idtx_usd::{PropertyValue, StageService, StructuralCommand};

fn field_text(hydrator: &SceneHydrator, name: &str) -> String {
    hydrator.world().iter_entities().filter_map(|e| e.get::<WidgetPanelComp>())
        .flat_map(|panel| &panel.elements).find(|element| element.path == name).unwrap().text.clone()
}

#[test]
fn authored_widget_precision_is_bounded_during_open_and_live_rehydration() {
    idtx_usd_extension::install().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("precision.usda");
    let cases = [
        ("Absent", None, "1.250"),
        ("Negative", Some(-1), "1.250"),
        ("Zero", Some(0), "1"),
        ("Ordinary", Some(3), "1.250"),
        ("Boundary", Some(16), "1.2500000000000000"),
        ("OverBoundary", Some(17), "1.2500000000000000"),
        ("Huge", Some(100_000), "1.2500000000000000"),
        ("IntMax", Some(i32::MAX), "1.2500000000000000"),
        ("IntMin", Some(i32::MIN), "1.250"),
    ];
    let mut text = String::from("#usda 1.0\ndef IdtxWidgetPanel \"Panel\" {\nint2 idtx:widget:resolution = (16,16)\n");
    for (schema, value_type) in [("Double", "double"), ("Float", "float")] {
        for (name, precision, _) in cases {
            text.push_str(&format!("def IdtxWidgetField{schema} \"{schema}{name}\" {{\n{value_type} idtx:widget:value = 1.25\n"));
            if let Some(precision) = precision { text.push_str(&format!("int idtx:widget:precision = {precision}\n")); }
            text.push_str("}\n");
        }
    }
    text.push_str("}\n");
    std::fs::write(&path, text).unwrap();
    let mut service = StageService::open(&path).unwrap();
    let original = service.root_layer_text().unwrap();
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&service.project_topology_paths(&[]).unwrap(), &service);
    hydrator.hydrate_widgets(&service);
    for schema in ["Double", "Float"] {
        for (name, _, expected) in cases {
            assert_eq!(field_text(&hydrator, &format!("{schema}{name}")), expected);
        }
    }
    assert_eq!(service.root_layer_text().unwrap(), original, "display clamping must not rewrite authored layers");
    for schema in ["Double", "Float"] {
        let name = format!("{schema}Ordinary");
        let prim = service.prim_at_path(&format!("/Panel/{name}")).unwrap();
        for (_, precision, expected) in cases.into_iter().filter(|(_, precision, _)| precision.is_some()) {
            service.apply_structural_command(&StructuralCommand::SetProperty {
                prim, property: "idtx:widget:precision".into(), value: PropertyValue::Int(precision.unwrap()),
            }).unwrap();
            let authored = service.root_layer_text().unwrap();
            service.drain_changes().unwrap();
            hydrator.hydrate_widgets(&service);
            assert_eq!(field_text(&hydrator, &name), expected);
            assert_eq!(service.root_layer_text().unwrap(), authored, "rehydration must not overwrite the authored precision");
        }
    }
}
