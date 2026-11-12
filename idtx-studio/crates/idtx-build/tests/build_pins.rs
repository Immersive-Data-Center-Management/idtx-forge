//! Pins must name actual resolved dependencies and real runtime feature names.

use std::path::Path;

#[test]
fn emitted_version_pins_match_cargos_resolved_dependency_edges() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let rustc = std::process::Command::new("rustc").arg("-vV").output().unwrap();
    assert!(rustc.status.success());
    let compiler = String::from_utf8(rustc.stdout).unwrap();
    let host = compiler.lines().find_map(|line| line.strip_prefix("host: ")).unwrap();
    let output = std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root).args(["metadata", "--offline", "--locked", "--format-version=1"])
        .args(["--filter-platform", host])
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = graph["packages"].as_array().unwrap();
    let nodes = graph["resolve"]["nodes"].as_array().unwrap();
    for (owner, dependency, pin) in [
        ("idtx-behavior-abi", "glam", idtx_build::PIN_GLAM),
        ("idtx-behavior", "wasmtime", idtx_build::PIN_WASMTIME),
        ("idtx-render", "wgpu", idtx_build::PIN_WGPU),
    ] {
        let owner = packages.iter().find(|p| p["name"] == owner).unwrap();
        let node = nodes.iter().find(|n| n["id"] == owner["id"]).unwrap();
        let dep = node["deps"].as_array().unwrap().iter().find(|d| d["name"] == dependency).unwrap();
        let package = packages.iter().find(|p| p["id"] == dep["pkg"]).unwrap();
        assert_eq!(pin, package["version"].as_str().unwrap(), "wrong {dependency} pin");
    }
}

#[test]
fn standalone_profiles_only_request_features_declared_by_the_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("apps/idtx-runtime/Cargo.toml")).unwrap()).unwrap();
    let features = manifest["features"].as_table().unwrap();
    for target in [idtx_build::CookTarget::quest("aarch64-linux-android"), idtx_build::CookTarget::vision_pro("aarch64-apple-visionos")] {
        for feature in target.features {
            assert!(features.contains_key(&feature), "runtime has no feature {feature}");
        }
    }
}
