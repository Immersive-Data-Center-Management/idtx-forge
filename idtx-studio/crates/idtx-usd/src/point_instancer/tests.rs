use super::*;
use crate::DirtyDomains;
use crate::tests::support::{scratch_dir, usd_fixture};

const PATH: &str = "/World/Referenced/Markers";

fn open() -> (StageService, tempfile::TempDir) {
    let dir = scratch_dir("point-instancer");
    for file in ["bounded_asset.usda", "bounded_point_instancer.usda"] {
        std::fs::copy(
            usd_fixture(&format!("instances/{file}")),
            dir.path().join(file),
        )
        .unwrap();
    }
    (
        StageService::open(dir.path().join("bounded_point_instancer.usda")).unwrap(),
        dir,
    )
}

fn placement() -> PointInstancerPlacement {
    PointInstancerPlacement {
        positions: vec![
            [4.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        ],
        prototype_indices: vec![0, 1, 0, 1],
        ids: vec![2, 500, -7, 81],
        invisible_ids: vec![500],
        ..Default::default()
    }
}

#[test]
#[ignore = "requires OpenUSD"]
fn native_ids_masks_reordering_and_time_varying_visibility() {
    let (mut stage, _dir) = open();
    let initial = stage.project_viewport().unwrap();
    let pi = &initial.point_instancers[0];
    assert_eq!(pi.ids, [500, -7, 81, 2]);
    assert_eq!(pi.mask, [1, 0, 0, 1]);
    assert_eq!(
        pi.transforms.len(),
        4,
        "masking must not remove placement slots"
    );
    assert_eq!(
        stage.inspect_point_instancer(PATH, Some(1.0)).unwrap().mask,
        [1, 1, 0, 0]
    );
    assert_eq!(
        stage.inspect_point_instancer(PATH, Some(2.0)).unwrap().mask,
        [0, 1, 0, 1]
    );

    let root = stage.root_layer_text().unwrap();
    stage.update_point_instancer(PATH, &placement()).unwrap();
    let changes = stage.drain_changes().unwrap().unwrap();
    assert!(changes.changes.iter().all(|c| !c.resync
        && c.domains.contains(DirtyDomains::INSTANCER)
        && !c.domains.contains(DirtyDomains::TOPOLOGY)));
    assert_eq!(stage.root_layer_text().unwrap(), root);
    let (sample, _) = stage
        .sample_point_instancers(&[PATH.into()], false)
        .unwrap();
    assert_eq!(sample[0].ids, [2, 500, -7, 81]);
    assert_eq!(sample[0].mask, [1, 0, 1, 0]);
    stage.update_point_instancer_mask(PATH, &[-7]).unwrap();
    let inspected = stage.inspect_point_instancer(PATH, Some(2.0)).unwrap();
    assert_eq!(
        inspected.mask,
        [1, 1, 0, 0],
        "strong session default masks override weaker time samples"
    );
    assert_eq!(inspected.inactive_ids, [81]);
    let (mask, _) = stage.sample_point_instancers(&[PATH.into()], true).unwrap();
    assert!(
        mask[0].transforms.is_empty(),
        "mask-only sampling must not compute/marshal matrices"
    );
    assert_eq!(mask[0].ids, [2, 500, -7, 81]);
}

#[test]
#[ignore = "requires OpenUSD"]
fn invalid_transactions_preserve_every_previous_opinion() {
    let (mut stage, _dir) = open();
    let valid = placement();
    stage.update_point_instancer(PATH, &valid).unwrap();
    stage.drain_changes().unwrap();
    let before = stage.session_layer_text().unwrap();
    for corrupt in 0..9 {
        let mut bad = valid.clone();
        match corrupt {
            0 => {
                bad.positions.pop();
            }
            1 => bad.prototype_indices[0] = 2,
            2 => bad.positions[1][0] = f32::NAN,
            3 => bad.ids[1] = bad.ids[0],
            4 => bad.invisible_ids = vec![999],
            5 => bad.invisible_ids = vec![500, 500],
            6 => bad.orientations_xyzw = vec![[0.0; 4]; 4],
            7 => bad.scales = vec![[1.0, f32::INFINITY, 1.0]; 4],
            _ => bad.orientations_xyzw = vec![[0.0, 0.0, 0.0, 1.0]],
        }
        assert!(
            stage.update_point_instancer(PATH, &bad).is_err(),
            "case {corrupt}"
        );
        assert_eq!(
            stage.session_layer_text().unwrap(),
            before,
            "case {corrupt}"
        );
        assert!(stage.drain_changes().unwrap().is_none());
    }
    for path in ["/World/Unrelated", "/Missing"] {
        assert!(stage.update_point_instancer(path, &valid).is_err());
        assert_eq!(stage.session_layer_text().unwrap(), before);
    }
    assert!(stage.update_point_instancer_mask(PATH, &[999]).is_err());
    assert!(
        stage
            .update_point_instancer_mask(PATH, &vec![0; MAX_POINT_INSTANCES + 1])
            .is_err()
    );
    assert_eq!(stage.session_layer_text().unwrap(), before);
    stage.update_point_instancer_mask(PATH, &[]).unwrap();
    assert!(
        stage.drain_changes().unwrap().is_some(),
        "invalid transactions must not leak the change block"
    );
}

#[test]
#[ignore = "requires OpenUSD"]
fn repeated_pulses_are_bounded_value_changes_and_preserve_cook_dependencies() {
    let (mut stage, dir) = open();
    stage.update_point_instancer(PATH, &placement()).unwrap();
    stage.drain_changes().unwrap();
    let topology = stage.topology().to_vec();
    let mut sizes = Vec::new();
    for pulse in 0..200 {
        let hidden = if pulse % 2 == 0 {
            &[-7][..]
        } else {
            &[500][..]
        };
        stage.update_point_instancer_mask(PATH, hidden).unwrap();
        let batch = stage.drain_changes().unwrap().unwrap();
        assert!(
            batch
                .changes
                .iter()
                .all(|c| c.property_name.as_deref() == Some("invisibleIds") && !c.resync)
        );
        let text = stage.session_layer_text().unwrap();
        assert!(!text.contains("timeSamples"));
        assert!(!text.contains("inactiveIds"));
        sizes.push(text.len());
        stage.update_point_instancer_mask(PATH, hidden).unwrap();
        assert!(
            stage.drain_changes().unwrap().is_none(),
            "identical mask must be a no-op"
        );
    }
    assert!(sizes.iter().max().unwrap() - sizes.iter().min().unwrap() < 16);
    assert_eq!(stage.topology(), topology);
    let deps =
        crate::compute_dependencies(dir.path().join("bounded_point_instancer.usda")).unwrap();
    assert!(deps.unresolved.is_empty());
    assert!(
        deps.dependencies
            .iter()
            .any(|p| p.ends_with("bounded_asset.usda")),
        "referenced prototypes remain ordinary cook dependencies"
    );
}

#[cfg(feature = "test-support")]
#[test]
#[ignore = "requires OpenUSD"]
fn mid_commit_failure_restores_existing_and_absent_specs() {
    for seeded in [false, true] {
        for fail_after in 1..=6 {
            let (mut stage, _dir) = open();
            if seeded {
                stage.update_point_instancer(PATH, &placement()).unwrap();
            }
            stage.drain_changes().unwrap();
            let before = stage.session_layer_text().unwrap();
            ffi::conformance_fail_instancer_after(stage.handle.pin_mut(), fail_after).unwrap();
            let mut update = placement();
            update.positions[0][0] = 72.0;
            assert!(stage.update_point_instancer(PATH, &update).is_err());
            assert_eq!(
                stage.session_layer_text().unwrap(),
                before,
                "seeded={seeded} fail_after={fail_after}"
            );
            assert!(stage.drain_changes().unwrap().is_none());
            stage.update_point_instancer(PATH, &update).unwrap();
            assert!(stage.drain_changes().unwrap().is_some());
        }
    }
}

#[test]
#[ignore = "requires OpenUSD"]
fn float_orientation_precedence_and_zero_reallocation_stay_native_and_atomic() {
    let (mut stage, _dir) = open();
    stage.replace_root_prim_text(PATH, "#usda 1.0\nover \"World\" {\nover \"Referenced\" {\nover \"Markers\" {\nquatf[] orientationsf = [(0,0,1,0), (0,0,1,0), (0,0,1,0), (0,0,1,0)]\n}\n}\n}\n").unwrap();
    stage.project_topology().unwrap();
    stage.drain_changes().unwrap();
    assert_eq!(
        stage
            .inspect_point_instancer(PATH, None)
            .unwrap()
            .placement
            .orientations_xyzw[0],
        [0.0, 1.0, 0.0, 0.0]
    );
    stage.update_point_instancer(PATH, &placement()).unwrap();
    stage.drain_changes().unwrap();
    assert_eq!(
        stage
            .inspect_point_instancer(PATH, None)
            .unwrap()
            .placement
            .orientations_xyzw,
        [[0.0, 0.0, 0.0, 1.0]; 4]
    );
    assert!(
        stage
            .session_layer_text()
            .unwrap()
            .contains("quatf[] orientationsf")
    );
    #[cfg(feature = "test-support")]
    {
        let before = stage.session_layer_text().unwrap();
        ffi::conformance_fail_instancer_after(stage.handle.pin_mut(), 7).unwrap();
        assert!(
            stage
                .update_point_instancer(PATH, &PointInstancerPlacement::default())
                .is_err()
        );
        assert_eq!(stage.session_layer_text().unwrap(), before);
        assert!(stage.drain_changes().unwrap().is_none());
    }
    stage
        .update_point_instancer(PATH, &PointInstancerPlacement::default())
        .unwrap();
    stage.update_point_instancer_mask(PATH, &[]).unwrap();
    assert_eq!(stage.inspect_point_instancer(PATH, None).unwrap().count, 0);
    stage.update_point_instancer(PATH, &placement()).unwrap();
    stage.update_point_instancer_mask(PATH, &[]).unwrap();
    assert_eq!(
        stage
            .inspect_point_instancer(PATH, None)
            .unwrap()
            .placement
            .orientations_xyzw,
        [[0.0, 0.0, 0.0, 1.0]; 4]
    );
}
