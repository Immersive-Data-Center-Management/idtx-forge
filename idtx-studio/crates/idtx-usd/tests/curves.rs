use glam::{DMat4, DVec3};
use idtx_usd::{PrimId, StageGeneration, StageService, curve::*};

#[path = "../../../tests/support/mod.rs"]
mod support;

fn record(points: &[[f32; 3]], cubic: bool) -> BasisCurvesRecord {
    BasisCurvesRecord {
        prim: PrimId::from_raw_parts(1, StageGeneration::from_raw(1)),
        points: points.to_vec(),
        counts: vec![points.len() as i32],
        curve_type: if cubic {
            "cubic"
        } else {
            "linear"
        }
        .into(),
        basis: "bezier".into(),
        wrap: "nonperiodic".into(),
        world_matrix: DMat4::IDENTITY.to_cols_array(),
        metres_per_unit: 1.0,
        z_up: false,
        tolerance: 1e-5,
        prototype_only: false,
        eligible: true,
        start_group: String::new(),
        end_group: String::new(),
    }
}

#[test]
fn straight_multisegment_batched_and_distance_clamping() {
    let mut r =
        record(&[[0., 0., 0.], [3., 0., 0.], [3., 4., 0.], [10., 0., 0.], [12., 0., 0.]], false);
    r.counts = vec![3, 2];
    let c = CurveEvaluator::new(&r, 0).unwrap();
    assert!((c.length() - 7.0).abs() < 1e-10);
    assert!(
        c.sample_distance(5., CurveSpace::Local)
            .unwrap()
            .point
            .abs_diff_eq(DVec3::new(3., 2., 0.), 1e-10)
    );
    assert_eq!(c.sample_distance(-1., CurveSpace::Local).unwrap().point, DVec3::ZERO);
    assert_eq!(c.sample_distance(100., CurveSpace::Local).unwrap().point, DVec3::new(3., 4., 0.));
    assert!((CurveEvaluator::new(&r, 1).unwrap().length() - 2.).abs() < 1e-10);
    assert!(matches!(CurveEvaluator::new(&r, 2), Err(CurveError::Index)));
}

#[test]
fn nonlinear_parameter_speed_is_not_distance() {
    let c = CurveEvaluator::new(
        &record(&[[0., 0., 0.], [0., 0., 0.], [0., 0., 0.], [10., 0., 0.]], true),
        0,
    )
    .unwrap();
    assert!((c.sample_distance(5., CurveSpace::Local).unwrap().point.x - 5.).abs() < 1e-10);
    assert_eq!(c.sample_distance(0., CurveSpace::Local).unwrap().tangent, DVec3::X);
    // Equal elapsed time must yield equal metre positions across update rates,
    // even when the authored Bezier parameter speed is highly nonuniform.
    for rate in [24, 30, 60, 144] {
        let mut elapsed = 0.0;
        let mut sample = CurveSample::default();
        for _ in 0..rate * 2 {
            elapsed += 1.0 / f64::from(rate);
            sample = c.sample_distance(1.75 * elapsed, CurveSpace::StageWorldMetres).unwrap();
        }
        assert!(sample.point.abs_diff_eq(DVec3::new(3.5, 0.0, 0.0), c.tolerance()));
    }
}

#[test]
fn periodic_bezier_and_adaptive_budget_exhaustion_are_explicit() {
    let mut r = record(
        &[[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.], [-1., 0., 1.], [-1., 0., 0.]],
        true,
    );
    r.wrap = "periodic".into();
    let c = CurveEvaluator::new(&r, 0).unwrap();
    assert_eq!(c.sample_distance(0., CurveSpace::Local).unwrap().point, DVec3::ZERO);
    let end = c.sample_distance(c.length(), CurveSpace::Local).unwrap();
    assert_eq!(end.point, DVec3::ZERO);
    assert_eq!(end.tangent, DVec3::X);
    let mut points = vec![[0., 0., 0.]];
    for _ in 0..200 {
        points.extend([[1., 3., 0.], [-1., 3., 0.], [0., 0., 0.]]);
    }
    assert!(matches!(CurveEvaluator::new(&record(&points, true), 0), Err(CurveError::Limit)));
}

#[cfg(feature = "authoring")]
#[test]
fn native_curve_creation_is_undoable() {
    let dir = support::scratch_dir("curve-create");
    let mut stage = StageService::create_new(dir.path().join("stage.usda"), "Y", 1.).unwrap();
    let before = stage.root_layer_text().unwrap();
    stage.define_prim_from_template("/World/Path", idtx_usd::PrimTemplate::BezierCurve).unwrap();
    let after = stage.root_layer_text().unwrap();
    assert_eq!(stage.project_viewport().unwrap().curves.len(), 1);
    stage.apply_root_layer_delta(&after, &before).unwrap();
    stage.project_topology().unwrap();
    assert!(stage.prim_at_path("/World/Path").is_none());
    stage.apply_root_layer_delta(&before, &after).unwrap();
    stage.project_topology().unwrap();
    assert!(stage.prim_at_path("/World/Path").is_some());
}

#[test]
fn degenerate_endpoint_tangents_periodic_and_unsupported_contract() {
    let r = record(&[[1., 2., 3.]; 4], true);
    let c = CurveEvaluator::new(&r, 0).unwrap();
    assert_eq!(c.length(), 0.);
    let s = c.sample_distance(5., CurveSpace::StageWorldMetres).unwrap();
    assert_eq!(s.point, DVec3::new(1., 2., 3.));
    assert_eq!(s.tangent, DVec3::X);
    let mut r = record(&[[0., 0., 0.], [0., 0., 0.], [2., 3., 0.], [2., 3., 0.]], true);
    let c = CurveEvaluator::new(&r, 0).unwrap();
    assert!(
        c.sample_distance(c.length(), CurveSpace::Local)
            .unwrap()
            .tangent
            .abs_diff_eq(DVec3::new(2., 3., 0.).normalize(), 1e-12)
    );
    r.basis = "catmullRom".into();
    assert!(matches!(CurveEvaluator::new(&r, 0), Err(CurveError::Unsupported(_))));
    r.basis = "bezier".into();
    r.wrap = "pinned".into();
    assert!(r.validate().is_err());
    r.wrap = "nonperiodic".into();
    r.counts = vec![3, 1];
    assert!(r.validate().is_err());
    let mut r = record(&[[0., 0., 0.], [1., 0., 0.], [1., 0., 1.]], false);
    r.wrap = "periodic".into();
    let c = CurveEvaluator::new(&r, 0).unwrap();
    assert!(
        c.sample_distance(c.length(), CurveSpace::Local)
            .unwrap()
            .point
            .abs_diff_eq(DVec3::ZERO, 1e-12)
    );
}

#[test]
fn arc_length_and_partial_distances_match_independent_dense_integral() {
    let mut r = record(&[[0., 0., 0.], [1., 3., 0.], [-2., 2., 1.], [3., 0., 0.]], true);
    r.world_matrix = DMat4::from_scale(DVec3::new(2., 0.5, 3.)).to_cols_array();
    let c = CurveEvaluator::new(&r, 0).unwrap();
    let p: Vec<_> = r
        .points
        .iter()
        .map(|p| r.local_to_metres().transform_point3(DVec3::from_array(p.map(f64::from))))
        .collect();
    let eval = |t: f64| {
        p[0] * (1. - t).powi(3)
            + p[1] * 3. * t * (1. - t).powi(2)
            + p[2] * 3. * t * t * (1. - t)
            + p[3] * t.powi(3)
    };
    let mut dense = vec![(0., p[0])];
    let mut length = 0.;
    let mut previous = p[0];
    for i in 1..=200_000 {
        let point = eval(i as f64 / 200_000.);
        length += previous.distance(point);
        dense.push((length, point));
        previous = point;
    }
    assert!((c.length() - length).abs() < r.tolerance);
    for i in 1..100 {
        let distance = length * i as f64 / 100.;
        let n = dense.partition_point(|(d, _)| *d < distance);
        let (a, pa) = dense[n - 1];
        let (b, pb) = dense[n];
        let expected = pa.lerp(pb, (distance - a) / (b - a));
        assert!(
            c.sample_distance(distance, CurveSpace::StageWorldMetres)
                .unwrap()
                .point
                .distance(expected)
                < r.tolerance
        );
    }
}

#[test]
fn bounded_queries_and_placement_footprint_clearance() {
    use placement::*;
    let r = record(&[[0., 0., 0.], [2., 0., 0.]], false);
    let c = CurveEvaluator::new(&r, 0).unwrap();
    let sentinel = CurveSample {
        point: DVec3::ONE,
        tangent: DVec3::ONE,
    };
    let mut out = [sentinel];
    assert!(c.sample_distances(&[0., 1.], CurveSpace::Local, &mut out).is_err());
    assert_eq!(out[0], sentinel);
    assert!(c.sample_distances(&[f64::NAN], CurveSpace::Local, &mut out).is_err());
    assert_eq!(out[0], sentinel);
    let settings = PlacementSettings {
        lateral: [0., 0.],
        half_extents: [0.2, 0.4, 0.2],
        ..Default::default()
    };
    let obstacle = ClearanceObstacle {
        path: "rack".into(),
        minimum: DVec3::new(0.1, 0., 0.1),
        maximum: DVec3::new(0.3, 1., 0.3),
    };
    let p = preview(&r, None, &settings, &[obstacle]).unwrap();
    assert!(!p.collisions.is_empty(), "centre is clear but the full footprint overlaps");
    let a = preview(&r, None, &settings, &[]).unwrap();
    let b = preview(&r, None, &settings, &[]).unwrap();
    assert_eq!(a.transforms, b.transforms);
    assert_eq!(a.diagnostics.len(), 2, "unassigned endpoint groups are reported");
    let mut ineligible = r.clone();
    ineligible.eligible = false;
    assert!(
        preview(&ineligible, None, &settings, &[]).unwrap_err().to_string().contains("ineligible")
    );
    assert!(
        preview(
            &r,
            None,
            &PlacementSettings {
                spacing: 1e-8,
                ..settings
            },
            &[]
        )
        .is_err()
    );
}

#[test]
fn native_projection_reports_unsupported_and_samples_composed_references() {
    let mut stage = StageService::open(support::usd_fixture("curves/paths.usda")).unwrap();
    let p = stage.project_viewport().unwrap();
    assert_eq!(p.curves.len(), 4);
    for curve in p.curves.iter().filter(|c| c.validate().is_ok()) {
        assert!(
            p.bounds.iter().any(|b| b.prim == curve.prim),
            "curve-only scenes must have bounds for camera framing and selection"
        );
    }
    assert!(p.diagnostics.iter().any(|s| s.contains("bspline")));
    assert!(p.diagnostics.iter().any(|s| s.contains("NurbsCurves")));
    assert!(p.diagnostics.iter().any(|s| s.contains("HermiteCurves")));
    let mut stage = StageService::open(support::usd_fixture("curves/references.usda")).unwrap();
    let p = stage.project_viewport().unwrap();
    let a = p.curves.iter().find(|r| stage.path_for(r.prim) == Some("/A/Path")).unwrap();
    let b = p.curves.iter().find(|r| stage.path_for(r.prim) == Some("/B/Path")).unwrap();
    assert_eq!(CurveEvaluator::new(a, 0).unwrap().length(), 2.);
    assert_eq!(CurveEvaluator::new(b, 0).unwrap().length(), 1.);
    let a = CurveEvaluator::new(a, 1).unwrap();
    assert!((a.length() - 3.).abs() < 1e-10);
    assert!(
        a.sample_distance(3., CurveSpace::StageWorldMetres)
            .unwrap()
            .point
            .abs_diff_eq(DVec3::new(0., 3., 0.), 1e-10)
    );
    assert!(
        CurveEvaluator::new(b, 0)
            .unwrap()
            .sample_distance(0., CurveSpace::StageWorldMetres)
            .unwrap()
            .point
            .abs_diff_eq(DVec3::new(1., 1., 0.), 1e-10)
    );
    let sampled = stage.sample_viewport(2.).unwrap();
    let a = sampled.curves.iter().find(|r| stage.path_for(r.prim) == Some("/A/Path")).unwrap();
    assert_eq!(CurveEvaluator::new(a, 0).unwrap().length(), 4.);
}

#[test]
fn unloaded_payload_clearance_is_inconclusive() {
    let mut stage =
        StageService::open_topology_only(support::usd_fixture("curves/unloaded.usda")).unwrap();
    assert!(!stage.curve_scene_complete().unwrap());
    assert!(
        stage
            .preview_curve_placements("/Path", &placement::PlacementSettings::default())
            .unwrap_err()
            .to_string()
            .contains("inconclusive")
    );
}

#[cfg(feature = "authoring")]
#[test]
fn baked_xforms_compensate_nonuniform_z_up_parent_and_remain_native() {
    let dir = support::scratch_dir("curve-bake-parent");
    for name in ["references.usda", "route_asset.usda"] {
        std::fs::copy(support::usd_fixture(&format!("curves/{name}")), dir.path().join(name))
            .unwrap();
    }
    let mut stage = StageService::open(dir.path().join("references.usda")).unwrap();
    let source = "/A/Path";
    let owner = idtx_usd::curve_bake_owner(source, 0);
    stage
        .bake_curve_placements(
            source,
            &owner,
            &placement::PlacementSettings {
                spacing: 0.5,
                window: 2.,
                lateral: [0., 0.],
                ..Default::default()
            },
        )
        .unwrap();
    stage.project_topology().unwrap();
    let sample = stage.prim_at_path(&format!("{owner}/Placements/Sample_2")).unwrap();
    let projection = stage.project_viewport_roots(&[sample]).unwrap();
    let matrix = DMat4::from_cols_array(&projection.transforms[0].world_matrix.map(f64::from));
    let normalized = normalized_stage_matrix(0.01, true) * matrix;
    assert!(normalized.transform_point3(DVec3::ZERO).abs_diff_eq(DVec3::X, 1e-8));
    assert!(
        (normalized.determinant() - 1.).abs() < 1e-7,
        "full parent nonuniform scale is compensated"
    );
    let props = stage.inspect_prim(sample).unwrap();
    assert!(props.iter().any(|p| p.name == "xformOp:transform"));
}

#[cfg(feature = "authoring")]
#[test]
fn referenced_bakes_reuse_composed_owner_and_suppress_obsolete_weak_samples() {
    let dir = support::scratch_dir("curve-bake-reference");
    for name in ["references.usda", "route_asset.usda"] {
        std::fs::copy(support::usd_fixture(&format!("curves/{name}")), dir.path().join(name))
            .unwrap();
    }
    let asset = dir.path().join("route_asset.usda");
    let mut route = StageService::open(&asset).unwrap();
    let owner = route.curve_bake_owner_for("/Route/Path", 0).unwrap();
    route
        .bake_curve_placements(
            "/Route/Path",
            &owner,
            &placement::PlacementSettings {
                spacing: 0.5,
                lateral: [0., 0.],
                ..Default::default()
            },
        )
        .unwrap();
    route.save_stage(&asset).unwrap();
    drop(route);
    let asset_before = std::fs::read(&asset).unwrap();
    let mut stage = StageService::open(dir.path().join("references.usda")).unwrap();
    assert_eq!(stage.stale_curve_bakes().unwrap().len(), 2);
    for source in ["/A/Path", "/B/Path"] {
        let composed_owner = stage.curve_bake_owner_for(source, 0).unwrap();
        assert!(composed_owner.ends_with(owner.rsplit('/').next().unwrap()));
        stage
            .bake_curve_placements(
                source,
                &composed_owner,
                &placement::PlacementSettings {
                    spacing: 1.,
                    lateral: [0., 0.],
                    ..Default::default()
                },
            )
            .unwrap();
    }
    stage.project_topology().unwrap();
    assert!(stage.stale_curve_bakes().unwrap().is_empty());
    let owner_b = stage.curve_bake_owner_for("/B/Path", 0).unwrap();
    let obsolete = format!("{owner_b}/Placements/Sample_2");
    assert!(
        stage
            .topology()
            .iter()
            .find(|r| stage.path(r.path) == Some(obsolete.as_str()))
            .is_none_or(|r| !r.state.active)
    );
    assert_eq!(std::fs::read(&asset).unwrap(), asset_before, "referenced asset is never mutated");
    let saved = dir.path().join("saved.usda");
    stage.save_stage(&saved).unwrap();
    let mut reopened = StageService::open(saved).unwrap();
    assert!(reopened.stale_curve_bakes().unwrap().is_empty());
}

#[cfg(feature = "authoring")]
#[test]
fn bake_native_xforms_staleness_ownership_undo_save_reopen() {
    use idtx_usd::{PropertyValue, StructuralCommand};
    let dir = support::scratch_dir("curve-bake");
    let file = dir.path().join("stage.usda");
    std::fs::copy(support::usd_fixture("curves/paths.usda"), &file).unwrap();
    let mut stage = StageService::open(&file).unwrap();
    let source = "/World/Straight";
    let owner = idtx_usd::curve_bake_owner(source, 0);
    let settings = placement::PlacementSettings {
        spacing: 2.,
        window: 100.,
        lateral: [0., 0.],
        ..Default::default()
    };
    let before = stage.root_layer_text().unwrap();
    let layer = stage.bake_curve_placements(source, &owner, &settings).unwrap();
    assert!(layer.contains("def Xform \"Sample_5\""));
    stage.project_topology().unwrap();
    let after = stage.root_layer_text().unwrap();
    assert_eq!(stage.curve_bake_settings(&owner).unwrap(), Some(settings.clone()));
    assert!(stage.stale_curve_bakes().unwrap().is_empty());
    let owner_prim = stage.prim_at_path(&owner).unwrap();
    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: owner_prim,
            property: "idtx:placement:spacing".into(),
            value: PropertyValue::F64(3.),
        })
        .unwrap();
    assert_eq!(stage.stale_curve_bakes().unwrap(), vec![owner.clone()]);
    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: owner_prim,
            property: "idtx:placement:spacing".into(),
            value: PropertyValue::F64(2.),
        })
        .unwrap();
    let last = stage.prim_at_path(&format!("{owner}/Placements/Sample_5")).unwrap();
    let p = stage.project_viewport_roots(&[last]).unwrap();
    assert!((p.transforms[0].world_matrix[12] - 10.).abs() < 1e-6);
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: "/World/Peer".into(),
            type_name: "Scope".into(),
        })
        .unwrap();
    stage.apply_root_layer_delta(&after, &before).unwrap();
    stage.project_topology().unwrap();
    assert!(stage.prim_at_path(&owner).is_none());
    assert!(stage.prim_at_path("/World/Peer").is_some());
    stage.apply_root_layer_delta(&before, &after).unwrap();
    stage.project_topology().unwrap();
    stage
        .apply_structural_command(&StructuralCommand::Define {
            path: format!("{owner}/Notes"),
            type_name: "Scope".into(),
        })
        .unwrap();
    let source_id = stage.prim_at_path(source).unwrap();
    stage
        .apply_structural_command(&StructuralCommand::SetProperty {
            prim: source_id,
            property: "points".into(),
            value: PropertyValue::Point3Array(vec![
                [0., 0., 0.],
                [20., 0., 0.],
                [0., 0., 2.],
                [3., 0., 2.],
                [3., 0., 6.],
            ]),
        })
        .unwrap();
    assert_eq!(stage.stale_curve_bakes().unwrap(), vec![owner.clone()]);
    assert!(stage.validate_curve_bakes().is_err());
    stage.bake_curve_placements(source, &owner, &settings).unwrap();
    stage.project_topology().unwrap();
    assert!(stage.prim_at_path(&format!("{owner}/Placements/Sample_10")).is_some());
    assert!(
        stage.prim_at_path(&format!("{owner}/Notes")).is_some(),
        "rebake preserves non-output owner children"
    );
    stage
        .bake_curve_placements(
            source,
            &owner,
            &placement::PlacementSettings {
                window: 2.,
                ..settings.clone()
            },
        )
        .unwrap();
    stage.project_topology().unwrap();
    assert!(
        stage.prim_at_path(&format!("{owner}/Placements/Sample_10")).is_none(),
        "rebake removes only previous output"
    );
    assert!(stage.prim_at_path("/World/Peer").is_some());
    assert!(
        stage.bake_curve_placements(source, "/World/Peer", &settings).is_err(),
        "must not replace user-owned scope"
    );
    stage.save_stage(&file).unwrap();
    drop(stage);
    let mut reopened = StageService::open(&file).unwrap();
    assert!(reopened.stale_curve_bakes().unwrap().is_empty());
    assert!(reopened.prim_at_path(&owner).is_some());
    assert!(reopened.prim_at_path(source).is_some());
}
