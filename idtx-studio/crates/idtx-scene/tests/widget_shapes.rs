//! USD composition/hydration for authored radial and polygon panels.
#![cfg(feature = "usd-extension")]
use idtx_scene::widget_layout::compute_widget_layout;
use idtx_scene::{PopulatorRegistry, SceneHydrator, WidgetElementKind, WidgetPanelComp};
use idtx_usd::{PropertyValue, StageService, StructuralCommand};

#[test]
fn composed_menus_and_outline_hydrate_and_reconcile() {
    idtx_usd_extension::install().unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/usd/widgets/radial_and_polygon.usda");
    let mut stage = StageService::open(&path).unwrap();
    let original = stage.root_layer_text().unwrap();
    let topology = stage.project_topology_paths(&[]).unwrap();
    let mut hydrator = SceneHydrator::new(PopulatorRegistry::new());
    hydrator.apply_topology_delta(&topology, &stage);
    hydrator
        .apply_viewport_projection(&stage.project_viewport().unwrap())
        .unwrap();
    hydrator.hydrate_widgets(&stage);
    let panel = |h: &SceneHydrator, path: &str| {
        h.world()
            .iter_entities()
            .find_map(|e| {
                (e.get::<idtx_scene::PrimNode>()?.path == path)
                    .then(|| e.get::<WidgetPanelComp>().cloned())
                    .flatten()
            })
            .unwrap()
    };
    let three = panel(&hydrator, "/World/Three");
    let five = panel(&hydrator, "/World/Five");
    assert_eq!(
        compute_widget_layout(three.resolution, &three.elements).len(),
        3
    );
    assert_eq!(
        compute_widget_layout(five.resolution, &five.elements).len(),
        5
    );
    assert!(
        five.elements
            .iter()
            .any(|e| e.path == "Items/Choose" && !e.enabled)
    );
    assert!(
        three
            .elements
            .iter()
            .any(|e| e.path == "Items/Choose" && e.enabled)
    );
    let message = panel(&hydrator, "/World/Message");
    assert!(
        message
            .surface
            .outline
            .as_ref()
            .unwrap()
            .contains([0.1, 0.2])
    );
    assert!(
        !message
            .surface
            .outline
            .as_ref()
            .unwrap()
            .contains([0.8, 0.9])
    );
    assert_eq!(
        message
            .elements
            .iter()
            .filter(|e| e.kind != WidgetElementKind::Container)
            .count(),
        1,
        "hidden descendants excluded"
    );

    let root = stage.prim_at_path("/World/Three/Items").unwrap();
    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: root,
            property: "idtx:widget:radial:gapAngle".into(),
            value: PropertyValue::F32(20.0),
        })
        .unwrap();
    hydrator.hydrate_widgets(&stage);
    assert_ne!(
        panel(&hydrator, "/World/Three").content_hash,
        three.content_hash
    );
    assert_eq!(
        panel(&hydrator, "/World/Five").content_hash,
        five.content_hash,
        "unrelated raster remains cached"
    );
    // The authoring undo primitive restores the authored property, not a
    // renderer-specific menu definition. Package and reopen both reference arcs.
    stage.replace_root_layer_text(&original).unwrap();
    stage.project_topology().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let package = scratch.path().join("menus.usdz");
    stage.package_loose_as_usdz(&path, &package).unwrap();
    let mut reopened = StageService::open(&package).unwrap();
    let mut restored = SceneHydrator::new(PopulatorRegistry::new());
    restored.apply_topology_delta(&reopened.project_topology_paths(&[]).unwrap(), &reopened);
    restored
        .apply_viewport_projection(&reopened.project_viewport().unwrap())
        .unwrap();
    restored.hydrate_widgets(&reopened);
    assert_eq!(
        panel(&restored, "/World/Three").content_hash,
        three.content_hash
    );
    assert_eq!(
        panel(&restored, "/World/Five").content_hash,
        five.content_hash
    );
    assert_eq!(panel(&restored, "/World/Message").surface, message.surface);
}
