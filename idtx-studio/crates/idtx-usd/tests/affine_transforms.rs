//! Independent native USD oracles for collaboration affine transforms.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use idtx_usd::{StageService, TransformValues, usd_trs_matrix, usd_world_to_local};

struct Fixture { stage: StageService, dir: PathBuf }
impl Fixture {
    fn open() -> Self {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("idtx-affine-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/usd/collaboration/stage.usda"), dir.join("stage.usda")).unwrap();
        let mut stage = StageService::open_topology_only(dir.join("stage.usda")).unwrap();
        stage.project_topology().unwrap();
        Self { stage, dir }
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

fn affine() -> [f64; 16] {
    [-1.0, 0.0, 0.0, 0.0, 0.25, 1.0, 0.0, 0.0, 0.125, -0.5, 1.0, 0.0, 4.125, 5.25, 6.5, 1.0]
}

#[test]
fn root_signature_observes_shared_layer_edits_masked_by_session_opinions() {
    let mut fixture = Fixture::open();
    let masked = StageService::open_with_session(fixture.dir.join("stage.usda"),
        include_str!("../../../tests/fixtures/usd/collaboration/root-mask.usda"), true).unwrap();
    let initial = masked.root_layer_content_hash().unwrap();
    assert_eq!(initial, masked.root_layer_content_hash().unwrap());
    let prim = fixture.stage.prim_at_path("/World/Local").unwrap();
    fixture.stage.apply_transform_edit(prim, TransformValues {
        translation: [123.0; 3], rotation_xyz_degrees: [0.0; 3], scale: [1.0; 3],
    }).unwrap();
    let edited = masked.root_layer_content_hash().unwrap();
    assert_ne!(initial, edited, "root opinions hidden by the session still invalidate persistence signatures");
    assert_eq!(edited, fixture.stage.root_layer_content_hash().unwrap());
    assert_eq!(edited, masked.root_layer_content_hash().unwrap());
}

fn close(a: [f64; 16], b: [f64; 16], tolerance: f64) {
    for (index, (a, b)) in a.into_iter().zip(b).enumerate() {
        assert!((a - b).abs() <= tolerance, "component {index}: {a} != {b}");
    }
}

#[test]
fn shared_trs_constructor_matches_native_usd_ops() {
    let mut fixture = Fixture::open();
    let prim = fixture.stage.prim_at_path("/World/Local").unwrap();
    for rotation in [[20.0, 30.0, 40.0], [-35.0, 90.0, 70.0], [130.0, -55.0, -15.0]] {
        let values = TransformValues { translation: [1.0, 2.0, 3.0], rotation_xyz_degrees: rotation, scale: [-2.0, 3.0, 4.0] };
        let expected = usd_trs_matrix(values).unwrap();
        fixture.stage.apply_transform_edit(prim, values).unwrap();
        close(fixture.stage.get_local_transform_f64(prim).unwrap(), expected, 1e-12);
    }
}

#[test]
fn exact_matrix_retains_negative_determinant_shear_and_double_precision_after_reopen() {
    let mut fixture = Fixture::open();
    let prim = fixture.stage.prim_at_path("/World/Local").unwrap();
    let mut matrix = affine();
    matrix[12] = 1_000_000_000.123_456_8;
    matrix[13] = 0.000_000_000_012_345;
    fixture.stage.apply_matrix_transform(prim, matrix).unwrap();
    let actual = fixture.stage.get_local_transform_f64(prim).unwrap();
    close(actual, matrix, 0.0);
    assert!((glam::DMat4::from_cols_array(&actual).determinant() + 1.0).abs() < 1e-12);
    let saved = fixture.dir.join("saved.usda");
    fixture.stage.export_root_layer(&saved).unwrap();
    let mut reopened = StageService::open_topology_only(&saved).unwrap();
    reopened.project_topology().unwrap();
    let prim = reopened.prim_at_path("/World/Local").unwrap();
    close(reopened.get_local_transform_f64(prim).unwrap(), matrix, 0.0);
}

#[test]
fn rotated_reflected_nonuniform_parent_reconstructs_transmitted_world() {
    let mut fixture = Fixture::open();
    let parent = fixture.stage.prim_at_path("/World/Parent").unwrap();
    let child = fixture.stage.prim_at_path("/World/Parent/Child").unwrap();
    let parent_matrix = fixture.stage.get_world_transform_f64(parent).unwrap();
    let world = affine();
    let local = usd_world_to_local(&world, &parent_matrix).unwrap();
    fixture.stage.apply_matrix_transform(child, local).unwrap();
    close(fixture.stage.get_world_transform_f64(child).unwrap(), world, 1e-12);
}

#[test]
fn matrix_override_preserves_reset_stack_and_pivot_property_specs() {
    let mut fixture = Fixture::open();
    for path in ["/World/Parent/Reset", "/World/Pivot"] {
        let prim = fixture.stage.prim_at_path(path).unwrap();
        let reset = fixture.stage.authored_xform_stack(prim).unwrap().reset_stack;
        fixture.stage.apply_matrix_transform(prim, affine()).unwrap();
        assert_eq!(fixture.stage.authored_xform_stack(prim).unwrap().reset_stack, reset);
        close(fixture.stage.get_world_transform_f64(prim).unwrap(), affine(), 1e-12);
    }
    assert!(fixture.stage.root_layer_text().unwrap().contains("xformOp:translate:pivot = (1, 2, 3)"));
}

#[test]
fn invalid_values_and_singular_parents_fail_without_writes() {
    let mut fixture = Fixture::open();
    let prim = fixture.stage.prim_at_path("/World/Local").unwrap();
    let before = fixture.stage.root_layer_text().unwrap();
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut invalid = affine();
        invalid[0] = value;
        assert!(fixture.stage.apply_matrix_transform(prim, invalid).is_err());
    }
    let mut projective = affine();
    projective[3] = 0.5;
    assert!(fixture.stage.apply_matrix_transform(prim, projective).is_err());
    let singular = fixture.stage.prim_at_path("/World/Singular").unwrap();
    let parent = fixture.stage.get_world_transform_f64(singular).unwrap();
    assert!(usd_world_to_local(&affine(), &parent).is_err());
    assert_eq!(before, fixture.stage.root_layer_text().unwrap());
    // A small but well-conditioned parent is invertible; do not use an
    // absolute determinant cutoff that rejects ordinary small unit scales.
    let parent = usd_trs_matrix(TransformValues { translation: [0.0; 3], rotation_xyz_degrees: [0.0; 3], scale: [1e-9; 3] }).unwrap();
    assert!(usd_world_to_local(&affine(), &parent).is_ok());
}

#[test]
#[cfg(feature = "test-support")]
fn matrix_edit_failure_rolls_back_new_and_existing_op_and_order() {
    for preexisting in [false, true] {
        for fail_after in [1, 2] {
            let mut fixture = Fixture::open();
            let prim = fixture.stage.prim_at_path("/World/Local").unwrap();
            if preexisting { fixture.stage.apply_matrix_transform(prim, affine()).unwrap(); }
            fixture.stage.drain_changes().unwrap();
            let before = fixture.stage.root_layer_text().unwrap();
            let before_matrix = fixture.stage.get_local_transform_f64(prim).unwrap();
            fixture.stage.inject_transform_edit_failure_after_writes(fail_after).unwrap();
            let mut changed = affine();
            changed[12] = 99.0;
            assert!(fixture.stage.apply_matrix_transform(prim, changed).is_err());
            assert_eq!(before, fixture.stage.root_layer_text().unwrap());
            close(fixture.stage.get_local_transform_f64(prim).unwrap(), before_matrix, 0.0);
            assert!(fixture.stage.drain_changes().unwrap().is_none_or(|batch| batch.changes.is_empty()));
        }
    }
}
