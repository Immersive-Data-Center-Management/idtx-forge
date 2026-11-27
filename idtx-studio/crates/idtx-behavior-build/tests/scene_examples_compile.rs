//! Integration test: every scene-interaction example behavior bundled under
//! `examples/behaviors/` compiles to wasm through the embedded skeleton against
//! the real SDK.
//!
//! This is the end-to-end validation of the scene-interaction SDK surface
//! (`ctx.scene()`, `raycast`, `arc_cast`, `ctx.camera()`, `ctx.highlight()`,
//! `ctx.widgets()`, …): if any wrapper signature drifts, these builds fail.
//! Rather than hard-coding the roster, the test discovers behavior entry points
//! (sources with `#[behavior]`) under the examples and built-in source trees, so adding a new example
//! extends the coverage automatically. Self-skips when the
//! `wasm32-unknown-unknown` target is absent (CI installs it).

use std::path::{Path, PathBuf};
use std::process::Command;

fn has_wasm_target() -> bool {
    Command::new("rustc")
        .args([
            "--print",
            "target-libdir",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

/// Helpers are modules, not independent behavior crates with their own ABI.
fn collect_behaviors(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => panic!("cannot read behaviors dir {}: {e}", dir.display()),
    };
    for entry in entries {
        let path = entry.expect("read dir entry").path();
        if path.is_dir() {
            collect_behaviors(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && std::fs::read_to_string(&path).unwrap().lines().any(|line| line.trim_start().starts_with("#[behavior")) {
            out.push(path);
        }
    }
}

#[test]
fn scene_interaction_examples_compile() {
    if !has_wasm_target() {
        eprintln!("skipping: wasm32-unknown-unknown target not installed");
        return;
    }

    let root = workspace_root();
    let sdk_path = root.join("crates/idtx-behavior-sdk");
    let out_dir = root.join("target/scene-examples-test");
    let _ = std::fs::remove_dir_all(&out_dir);
    let opts = idtx_behavior_build::BuildOptions::new(&out_dir, &sdk_path);

    let behaviors_dir = root.join("examples/behaviors");
    let mut sources = Vec::new();
    collect_behaviors(&behaviors_dir, &mut sources);
    collect_behaviors(&root.join("crates/idtx-assets/built-in/behaviors"), &mut sources);
    collect_behaviors(&root.join("examples/widgets"), &mut sources);
    collect_behaviors(&root.join("examples/animation-playback"), &mut sources);
    // Deterministic order, and a stable failure message when a build breaks.
    sources.sort();
    assert!(
        !sources.is_empty(),
        "no example behaviors found under {}",
        behaviors_dir.display()
    );

    for src in &sources {
        let name = src
            .file_stem()
            .expect("behavior file has a stem")
            .to_string_lossy()
            .into_owned();
        assert!(src.is_file(), "example missing at {}", src.display());
        let artifact = idtx_behavior_build::build_behavior(src, &opts)
            .unwrap_or_else(|e| panic!("{name} must compile: {e:?}"));
        assert!(artifact.name.starts_with(&format!("{name}_")) || name == "behavior");
        let bytes = std::fs::read(&artifact.wasm_path).expect("read wasm");
        assert_eq!(&bytes[0..4], b"\0asm", "{name}: not a wasm module");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &bytes).expect("valid WASM");
        for export in ["_idtx_abi_version", "_idtx_drop_instance", "_idtx_on_event"] {
            assert!(module.exports().any(|e| e.name() == export), "{name}: missing {export}");
        }
        assert!(idtx_behavior::verify_link(&bytes, idtx_behavior::Capabilities::all())
            .unwrap_or_else(|e| panic!("{name}: link verification failed: {e}")).links);
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}
