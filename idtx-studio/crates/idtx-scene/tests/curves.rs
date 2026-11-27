use idtx_scene::{CurveComp, PopulatorRegistry, SceneHydrator};
use idtx_usd::{StageService, TopologyDelta};
use std::sync::Arc;

#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
fn curve_snapshot_reuses_tables_and_separates_reference_instances_and_times() {
    let dir = support::scratch_dir("curve-cache");
    for name in ["references.usda", "route_asset.usda"] {
        std::fs::copy(support::usd_fixture(&format!("curves/{name}")), dir.path().join(name))
            .unwrap();
    }
    let mut service = StageService::open(dir.path().join("references.usda")).unwrap();
    let mut scene = SceneHydrator::new(PopulatorRegistry::new());
    scene.apply_topology_delta(
        &TopologyDelta {
            upserts: service.topology().to_vec(),
            removed: Default::default(),
        },
        &service,
    );
    let projection = service.project_viewport().unwrap();
    scene.apply_viewport_projection(&projection).unwrap();
    let a = scene.entity_for_path("/A/Path").unwrap();
    let b = scene.entity_for_path("/B/Path").unwrap();
    let first = scene.world().get::<CurveComp>(a).unwrap().tables[0].as_ref().unwrap().clone();
    let second = scene.world().get::<CurveComp>(b).unwrap().tables[0].as_ref().unwrap().clone();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(first.length(), 2.);
    assert_eq!(second.length(), 1.);
    scene.apply_viewport_projection(&projection).unwrap();
    let curve = scene.world().get::<CurveComp>(a).unwrap();
    assert!(Arc::ptr_eq(&first, curve.tables[0].as_ref().unwrap()));
    assert_eq!(curve.geometric_revision, 1);
    let mut metadata_only = projection.clone();
    metadata_only
        .curves
        .iter_mut()
        .find(|r| r.prim == service.prim_at_path("/A/Path").unwrap())
        .unwrap()
        .eligible = false;
    scene.apply_viewport_projection(&metadata_only).unwrap();
    assert!(Arc::ptr_eq(
        &first,
        scene.world().get::<CurveComp>(a).unwrap().tables[0].as_ref().unwrap()
    ));
    assert!(!scene.world().get::<CurveComp>(a).unwrap().source.eligible);
    let animated = service.sample_viewport(2.).unwrap();
    scene.apply_viewport_projection(&animated).unwrap();
    let curve = scene.world().get::<CurveComp>(a).unwrap();
    assert_eq!(curve.curve(0).unwrap().length(), 4.);
    assert_eq!(curve.geometric_revision, 2);
    scene.apply_viewport_projection(&animated).unwrap();
    assert_eq!(scene.world().get::<CurveComp>(a).unwrap().geometric_revision, 2);
    let changed = service.prim_at_path("/A/Path").unwrap();
    service
        .apply_structural_command(&idtx_usd::StructuralCommand::SetProperty {
            prim: changed,
            property: "points".into(),
            value: idtx_usd::PropertyValue::Point3Array(vec![
                [0., 0., 0.],
                [400., 0., 0.],
                [0., 0., 0.],
                [0., 0., 100.],
            ]),
        })
        .unwrap();
    service.drain_changes().unwrap();
    let edited = service.project_viewport_roots(&[changed]).unwrap();
    scene.apply_viewport_projection(&edited).unwrap();
    let entity = scene.entity_for(changed).unwrap();
    assert_eq!(scene.world().get::<CurveComp>(entity).unwrap().geometric_revision, 3);
    assert_eq!(scene.world().get::<CurveComp>(entity).unwrap().curve(0).unwrap().length(), 8.);
    assert_eq!(scene.world().get::<CurveComp>(b).unwrap().geometric_revision, 2);
    scene.apply_viewport_projection(&edited).unwrap();
    assert_eq!(scene.world().get::<CurveComp>(entity).unwrap().geometric_revision, 3);
    scene.apply_topology_delta(
        &TopologyDelta {
            upserts: vec![],
            removed: [changed].into_iter().collect(),
        },
        &service,
    );
    assert!(
        scene.world().get::<CurveComp>(entity).is_none(),
        "removed entity cannot retain a live curve handle"
    );
}
