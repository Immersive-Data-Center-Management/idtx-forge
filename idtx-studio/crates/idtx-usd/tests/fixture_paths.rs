#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
fn relocated_inputs_are_required() {
    for name in [
        "collaboration/stage.usda",
        "collaboration/asset-a.usda",
        "collaboration/asset-b.usda",
        "physics/box_fall_plane.usda",
        "hydration/authored_visibility.usda",
        "authoring/undo/stage.usda",
        "widgets/auth_login.usda",
        "scene/revolved_gprims.usda",
        "materials/TextureCoordinateTest.usda",
        "materials/TextureCoordinateTemplate.png",
    ] {
        assert!(support::usd_fixture(name).is_file());
    }
}

#[test]
#[should_panic(expected = "required fixture")]
fn missing_inputs_fail_instead_of_skipping() {
    let scratch = support::scratch_dir("missing-fixture");
    support::required_file(scratch.path().join("missing.usda"));
}

#[test]
#[should_panic(expected = "is an LFS pointer")]
fn unhydrated_lfs_inputs_fail_explicitly() {
    let scratch = support::scratch_dir("lfs-fixture");
    let path = scratch.path().join("pointer.usdz");
    std::fs::write(
        &path,
        "version https://git-lfs.github.com/spec/v1\noid sha256:0000\nsize 1\n",
    )
    .unwrap();
    support::required_file(path);
}

#[test]
fn generated_files_are_isolated_and_cleaned() {
    let first = support::scratch_dir("fixture-isolation");
    let second = support::scratch_dir("fixture-isolation");
    let path = first.path().to_path_buf();
    assert_ne!(path, second.path());
    assert!(!path.starts_with(support::fixture_root().canonicalize().unwrap()));
    std::fs::write(path.join("generated.usda"), "#usda 1.0\n").unwrap();
    drop(first);
    assert!(!path.exists());
    assert!(second.path().is_dir());
}
