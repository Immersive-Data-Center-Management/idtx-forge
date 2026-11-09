#[test]
fn resolver_read_budget_checks_size_before_copy() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd")
        .join(format!("asset-budget-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("asset.bin");
    std::fs::write(&path, [42u8; 4096]).unwrap();
    let path = path.to_str().unwrap();
    assert!(idtx_usd::read_asset_bounded(path, 4095).is_err());
    assert_eq!(idtx_usd::read_asset_bounded(path, 4096).unwrap(), vec![42u8; 4096]);
}
