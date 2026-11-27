//! Build-cache regressions use the public builder, not a second cache lookup.

use std::path::Path;
use idtx_behavior_build::{BuildOptions, build_behavior, source_hash, stage_sdk};

fn fixture(root: &Path) -> (std::path::PathBuf, BuildOptions) {
    for (name, directory, dependencies, code) in [
        ("idtx-behavior-sdk", "sdk", "[dependencies]\nidtx-behavior-abi = { path = '../abi' }\n", "#![no_std]\npub const VALUE: u32 = idtx_behavior_abi::VALUE + 1;\n"),
        ("idtx-behavior-abi", "abi", "", "#![no_std]\npub const VALUE: u32 = 10;\n"),
    ] {
        let dir = root.join(directory);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), format!("[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2021'\n{dependencies}")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), code).unwrap();
    }
    let sources = root.join("behaviors");
    std::fs::create_dir_all(&sources).unwrap();
    let source = sources.join("probe.rs");
    std::fs::write(&source, "#[panic_handler]\nfn panic(_: &core::panic::PanicInfo<'_>) -> ! { loop {} }\n#[no_mangle]\npub extern \"C\" fn value() -> u32 { idtx_behavior_sdk::VALUE }\n").unwrap();
    (source, BuildOptions::new(root.join("out"), root.join("sdk")))
}

fn value(path: &Path) -> u32 {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_file(&engine, path).unwrap();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
    instance.get_typed_func::<(), u32>(&mut store, "value").unwrap().call(&mut store, ()).unwrap()
}

#[test]
fn relocation_preserves_public_names_and_wasm_bytes_and_distinguishes_level_collections() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    let (source, opts) = fixture(&original);
    std::fs::write(original.join("project.toml"), "").unwrap();
    let first = build_behavior(&source, &opts).unwrap();
    let bytes = std::fs::read(&first.wasm_path).unwrap();
    let relocated = dir.path().join("relocated");
    std::fs::rename(&original, &relocated).unwrap();
    let options = BuildOptions::new(relocated.join("out"), relocated.join("sdk"));
    let second = build_behavior(&relocated.join("behaviors/probe.rs"), &options).unwrap();
    assert_eq!(first.name, second.name);
    assert_eq!(bytes, std::fs::read(&second.wasm_path).unwrap());
    assert_eq!(value(&second.wasm_path), 11);
    let mut names = std::collections::HashSet::new();
    for level in ["level-a", "level-b"] {
        let root = relocated.join(level).join("behaviors");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(relocated.join("behaviors/probe.rs"), root.join("probe.rs")).unwrap();
        let artifact = build_behavior(&root.join("probe.rs"), &options).unwrap();
        assert_ne!(artifact.name, second.name);
        assert!(names.insert(artifact.name));
        assert_eq!(value(&artifact.wasm_path), 11);
    }
}

#[test]
fn sdk_and_transitive_abi_content_changes_invalidate_real_wasm_even_with_preserved_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let (source, opts) = fixture(dir.path());
    let first = build_behavior(&source, &opts).unwrap();
    assert!(!first.cache_hit);
    assert_eq!(value(&first.wasm_path), 11);
    assert!(build_behavior(&source, &opts).unwrap().cache_hit);
    let abi = dir.path().join("abi/src/lib.rs");
    let modified = std::fs::metadata(&abi).unwrap().modified().unwrap();
    std::fs::write(&abi, "#![no_std]\npub const VALUE: u32 = 40;\n").unwrap();
    std::fs::File::options().write(true).open(&abi).unwrap().set_times(std::fs::FileTimes::new().set_modified(modified)).unwrap();
    let second = build_behavior(&source, &opts).unwrap();
    assert!(!second.cache_hit);
    assert_ne!(first.hash, second.hash);
    assert_eq!(value(&second.wasm_path), 41);
    std::fs::write(dir.path().join("sdk/src/lib.rs"), "#![no_std]\npub const VALUE: u32 = idtx_behavior_abi::VALUE + 2;\n").unwrap();
    let third = build_behavior(&source, &opts).unwrap();
    assert!(!third.cache_hit);
    assert_ne!(third.hash, second.hash);
    assert_eq!(value(&third.wasm_path), 42);
    let abi_manifest = dir.path().join("abi/Cargo.toml");
    let text = std::fs::read_to_string(&abi_manifest).unwrap().replace("0.1.0", "0.2.0");
    std::fs::write(abi_manifest, text).unwrap();
    let dependency_update = build_behavior(&source, &opts).unwrap();
    assert!(!dependency_update.cache_hit);
    assert_ne!(third.hash, dependency_update.hash);
    assert_eq!(value(&dependency_update.wasm_path), 42);
    std::fs::create_dir(dir.path().join(".cargo")).unwrap();
    std::fs::write(dir.path().join(".cargo/config.toml"), "[target.wasm32-unknown-unknown]\nrustflags = ['-C', 'opt-level=1']\n").unwrap();
    let configured = build_behavior(&source, &opts).unwrap();
    assert!(!configured.cache_hit);
    assert_ne!(configured.hash, dependency_update.hash);
    assert_eq!(value(&configured.wasm_path), 42);
    std::fs::remove_dir_all(&opts.sdk_path).unwrap();
    assert!(build_behavior(&source, &opts).is_err(), "cached output must not survive an absent SDK");
}

#[test]
fn literal_sibling_include_changes_invalidate_real_wasm_and_removed_files_cannot_reuse_stale_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let (source, opts) = fixture(dir.path());
    let source_text = std::fs::read_to_string(&source).unwrap().replace("idtx_behavior_sdk::VALUE }", "idtx_behavior_sdk::VALUE + OFFSET }");
    std::fs::write(&source, format!("include!(\"shared.rs\");\n{source_text}")).unwrap();
    let shared = source.parent().unwrap().join("shared.rs");
    std::fs::write(&shared, "const OFFSET: u32 = 1;\n").unwrap();
    let first = build_behavior(&source, &opts).unwrap();
    assert_eq!(value(&first.wasm_path), 12);
    assert!(build_behavior(&source, &opts).unwrap().cache_hit);
    let modified = std::fs::metadata(&shared).unwrap().modified().unwrap();
    std::fs::write(&shared, "const OFFSET: u32 = 9;\n").unwrap();
    std::fs::File::options().write(true).open(&shared).unwrap().set_times(std::fs::FileTimes::new().set_modified(modified)).unwrap();
    let changed = build_behavior(&source, &opts).unwrap();
    assert!(!changed.cache_hit);
    assert_ne!(first.hash, changed.hash);
    assert_eq!(value(&changed.wasm_path), 20);
    std::fs::remove_file(shared).unwrap();
    assert!(build_behavior(&source, &opts).is_err(), "a missing include must not compile a stale staged companion");
}

#[test]
fn a_corrupted_cached_artifact_is_repaired_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let (source, opts) = fixture(dir.path());
    let first = build_behavior(&source, &opts).unwrap();
    let artifacts = opts.out_dir.join(".behavior-cache/artifacts");
    let entry = std::fs::read_dir(&artifacts).unwrap().next().unwrap().unwrap().path();
    std::fs::write(entry.join("artifact.wasm"), "corrupted").unwrap();
    let rebuilt = build_behavior(&source, &opts).unwrap();
    assert!(!rebuilt.cache_hit);
    assert_eq!(first.hash, rebuilt.hash);
    assert_eq!(value(&rebuilt.wasm_path), 11);
    // Cargo refreshes the top-level convenience copy even for a fresh build;
    // corrupt its actual cdylib output in deps/ to exercise receipt validation.
    let cargo_output = entry.join("target/wasm32-unknown-unknown/release/deps").join(format!("{}.wasm", first.name));
    assert!(cargo_output.is_file());
    std::fs::write(cargo_output, "corrupted compiler output").unwrap();
    let rebuilt = build_behavior(&source, &opts).unwrap();
    assert!(!rebuilt.cache_hit);
    assert_eq!(first.hash, rebuilt.hash);
    assert_eq!(value(&rebuilt.wasm_path), 11);
}

#[test]
fn recursive_modules_shared_data_and_duplicate_entry_names_have_content_correct_cache_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (flat, opts) = fixture(dir.path());
    let root = flat.parent().unwrap();
    std::fs::create_dir_all(root.join("spin/motion")).unwrap();
    std::fs::create_dir(root.join("shared")).unwrap();
    let source = root.join("spin/behavior.rs");
    let original = std::fs::read_to_string(&flat).unwrap();
    std::fs::write(&source, format!("//! A module body, including inner docs.\nmod motion;\n#[path = \"../shared/lib.rs\"]\nmod shared;\n{}",
        original.replace("idtx_behavior_sdk::VALUE }", "idtx_behavior_sdk::VALUE + motion::value() + shared::VALUE }"))).unwrap();
    std::fs::write(root.join("spin/motion.rs"), "mod easing; pub fn value() -> u32 { easing::VALUE + include_bytes!(\"../shared/data.bin\")[0] as u32 }").unwrap();
    let nested = root.join("spin/motion/easing.rs");
    std::fs::write(&nested, "pub const VALUE: u32 = 1;").unwrap();
    std::fs::write(root.join("shared/lib.rs"), "pub const VALUE: u32 = 2;").unwrap();
    let data = root.join("shared/data.bin");
    std::fs::write(&data, [3]).unwrap();
    let first = build_behavior(&source, &opts).unwrap();
    assert_eq!(value(&first.wasm_path), 17);
    assert!(build_behavior(&source, &opts).unwrap().cache_hit);
    let modified = std::fs::metadata(&nested).unwrap().modified().unwrap();
    std::fs::write(&nested, "pub const VALUE: u32 = 9;").unwrap();
    std::fs::File::options().write(true).open(&nested).unwrap().set_times(std::fs::FileTimes::new().set_modified(modified)).unwrap();
    let changed = build_behavior(&source, &opts).unwrap();
    assert!(!changed.cache_hit);
    assert_ne!(first.hash, changed.hash);
    assert_eq!(value(&changed.wasm_path), 25);
    std::fs::write(&data, [8]).unwrap();
    let changed_data = build_behavior(&source, &opts).unwrap();
    assert_ne!(changed.hash, changed_data.hash);
    assert_eq!(value(&changed_data.wasm_path), 30);

    std::fs::create_dir(root.join("other")).unwrap();
    let other = root.join("other/behavior.rs");
    std::fs::write(&other, &original).unwrap();
    let second = build_behavior(&other, &opts).unwrap();
    assert_ne!(first.wasm_path, second.wasm_path);
    assert_eq!(value(&first.wasm_path), 30, "publishing another behavior must not replace this one");
    assert_eq!(value(&second.wasm_path), 11);

    std::fs::create_dir(dir.path().join("import")).unwrap();
    let collection = idtx_behavior_build::BehaviorSource::discover(&source, None).unwrap();
    let imported = collection.copy_to(&dir.path().join("import/behaviors")).unwrap();
    assert_eq!(value(&build_behavior(&imported, &opts).unwrap().wasm_path), 30);
    std::fs::remove_file(nested).unwrap();
    let error = build_behavior(&source, &opts).unwrap_err().to_string();
    assert!(error.contains("easing"), "{error}");
    assert!(error.contains(&root.to_string_lossy().to_string()), "snapshot diagnostics must identify the original source: {error}");
}

#[cfg(unix)]
#[test]
fn compiler_wrapper_changes_invalidate_the_cache() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let wrapper = dir.path().join("compiler-wrapper");
    let report = dir.path().join("report.json");
    let mut previous_hash = None;
    for (revision, flags, expected_hit) in [(1, "", false), (1, "", true), (2, "", false), (2, "-C opt-level=1", false)] {
        std::fs::write(&wrapper, format!("#!/bin/sh\n# compiler wrapper revision {revision}\nexec \"$@\"\n")).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "compiler_fixture_child", "--nocapture"])
            .env("S07_BUILD_ROOT", dir.path()).env("S07_BUILD_REPORT", &report)
            .env("RUSTFLAGS", flags).env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("RUSTC_WRAPPER", &wrapper).output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
        assert_eq!(result["cache_hit"], expected_hit);
        if let Some(previous) = previous_hash { assert_eq!(result["hash"] == previous, expected_hit); }
        previous_hash = Some(result["hash"].clone());
    }
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess entry used by compiler_wrapper_changes_invalidate_the_cache"]
fn compiler_fixture_child() {
    let root = std::path::PathBuf::from(std::env::var_os("S07_BUILD_ROOT").expect("fixture parent"));
    let artifact = build_behavior(&root.join("behaviors/probe.rs"), &BuildOptions::new(root.join("out"), root.join("sdk"))).unwrap();
    assert_eq!(value(&artifact.wasm_path), 11);
    std::fs::write(std::env::var_os("S07_BUILD_REPORT").unwrap(), serde_json::to_vec(&serde_json::json!({"cache_hit": artifact.cache_hit, "hash": artifact.hash})).unwrap()).unwrap();
}

#[test]
fn a_nonexistent_sdk_cannot_authorize_a_preexisting_cache_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let source = "// cache regression\n";
    let file = dir.path().join("probe.rs");
    std::fs::write(&file, source).unwrap();
    let opts = BuildOptions::new(dir.path().join("out"), dir.path().join("missing-sdk"));
    let legacy_key = source_hash(source, env!("CARGO_PKG_VERSION"));
    let cached = opts.out_dir.join(".behavior-cache").join(legacy_key)
        .join("target/wasm32-unknown-unknown/release/probe.wasm");
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::write(cached, b"\0asm\x01\0\0\0").unwrap();
    assert!(build_behavior(&file, &opts).is_err(), "cache hit bypassed SDK validation");
    assert!(!opts.out_dir.join("probe.wasm").exists());
}

#[test]
fn shipped_sdk_inherits_the_actual_workspace_dependency_and_package_contract() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(crates.parent().unwrap().join("Cargo.toml")).unwrap()).unwrap();
    let dest = tempfile::tempdir().unwrap();
    let staged = dest.path().join("sdk");
    stage_sdk(crates, &staged).unwrap();
    let generated: toml::Value = toml::from_str(&std::fs::read_to_string(staged.join("Cargo.toml")).unwrap()).unwrap();
    for dependency in ["glam", "bytemuck", "syn", "quote", "proc-macro2"] {
        assert_eq!(generated["workspace"]["dependencies"][dependency], source["workspace"]["dependencies"][dependency], "stale SDK dependency: {dependency}");
    }
    assert_eq!(generated["workspace"]["package"], source["workspace"]["package"]);
}

#[test]
fn staged_sdk_with_a_quoted_path_compiles_the_real_guest() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let staged = dir.path().join("shipped 'sdk\"");
    #[cfg(windows)]
    let staged = dir.path().join("shipped 'sdk"); // '"' is illegal in Windows paths
    stage_sdk(crates, &staged).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/spinning_cube.rs");
    let options = BuildOptions::new(dir.path().join("out"), staged.join("idtx-behavior-sdk"));
    let first = build_behavior(&source, &options).unwrap();
    assert!(!first.cache_hit);
    assert!(build_behavior(&source, &options).unwrap().cache_hit);
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_file(&engine, first.wasm_path).unwrap();
    assert!(module.exports().any(|e| e.name() == "_idtx_abi_version"));
}
