#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
fn resolver_read_budget_checks_size_before_copy() {
    let scratch = support::scratch_dir("asset-budget");
    let path = scratch.path().join("asset.bin");
    std::fs::write(&path, [42u8; 4096]).unwrap();
    let path = path.to_str().unwrap();
    assert!(idtx_usd::read_asset_bounded(path, 4095).is_err());
    assert_eq!(idtx_usd::read_asset_bounded(path, 4096).unwrap(), vec![42u8; 4096]);
}
