//! Exercise the real cook, including reused outputs inside the asset search tree.
use std::{fs, path::PathBuf};
use idtx_build::{cook, CookPlan, CookTarget};

fn fixture() -> (PathBuf, CookPlan) {
    let root = std::env::temp_dir().join(format!("idtx-inventory-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(root.join("assets")).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testfiles/usd/cook_inventory/level.usda");
    fs::copy(source, root.join("level.usda")).unwrap();
    fs::write(root.join("assets/image.bin"), b"first").unwrap();
    let mut manifest = idtx_project::ProjectManifest::new("Inventory");
    manifest.default_stage = Some("level.usda".into());
    manifest.asset_roots = vec!["assets".into()];
    let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();
    let mut plan = CookPlan::from_project(&project, CookTarget::desktop("test"), root.join("assets/output")).unwrap();
    plan.player = None;
    plan.xr.controllers.clear();
    (root, plan)
}

#[test]
fn clean_recook_updates_removes_and_preserves_on_failure() {
    let (root, plan) = fixture();
    let first = cook(&plan).unwrap();
    // OpenUSD's ZIP writer uses a two-second DOS clock. Identical inputs must
    // remain reproducible across that boundary, not merely within one tick.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let second = cook(&plan).unwrap();
    assert_eq!(first.content_hash, second.content_hash, "unchanged reused output");
    let mut fresh = plan.clone();
    fresh.out_dir = root.join("target/fresh");
    assert_eq!(first.content_hash, cook(&fresh).unwrap().content_hash);
    fs::write(root.join("assets/image.bin"), b"changed").unwrap();
    let changed = cook(&plan).unwrap();
    assert_ne!(first.content_hash, changed.content_hash);
    assert_eq!(fs::read(plan.out_dir.join("assets/image.bin")).unwrap(), b"changed");
    fs::remove_file(root.join("assets/image.bin")).unwrap();
    let removed = cook(&plan).unwrap();
    assert_ne!(changed.content_hash, removed.content_hash);
    assert!(!plan.out_dir.join("assets/image.bin").exists());
    let good_manifest = fs::read(&removed.manifest).unwrap();
    let good_stage = fs::read(&removed.stage).unwrap();
    fs::write(root.join("bad.wasm"), b"not wasm").unwrap();
    assert!(cook(&plan).is_err());
    assert_eq!(fs::read(&removed.manifest).unwrap(), good_manifest);
    assert_eq!(fs::read(&removed.stage).unwrap(), good_stage);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stamp_includes_paths_network_and_input_map() {
    let (root, mut plan) = fixture();
    let original = cook(&plan).unwrap().content_hash;
    plan.network.allow = true;
    plan.network.allowed_hosts = vec!["example.com".into()];
    let network = cook(&plan).unwrap().content_hash;
    assert_ne!(original, network);
    fs::write(root.join("input_map.toml"), b"[actions]\n").unwrap();
    let input = cook(&plan).unwrap().content_hash;
    assert_ne!(network, input);
    fs::rename(root.join("assets/image.bin"), root.join("assets/renamed.bin")).unwrap();
    assert_ne!(input, cook(&plan).unwrap().content_hash, "same bytes at a new path");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discovery_ignores_generated_outputs_and_links() {
    let (root, plan) = fixture();
    for dir in [root.join("target"), root.join("build"), root.join("assets/dist"), plan.out_dir.clone()] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("invalid.wasm"), b"generated junk").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("target/invalid.wasm"), root.join("linked.wasm")).unwrap();
    let report = cook(&plan).unwrap();
    assert!(report.behaviors.is_empty());
    assert!(!plan.out_dir.join("assets/dist").exists());
    fs::remove_dir_all(root).unwrap();
}
