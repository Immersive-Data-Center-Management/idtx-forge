//! Integration test: the single-file `spinning_cube` fixture compiles to
//! wasm through the embedded skeleton, and a second build hits cache.
//!
//! Requires the `wasm32-unknown-unknown` target + a working `cargo`. When
//! the target is absent the test self-skips so it doesn't fail on a
//! machine without the wasm target — CI installs the target so the check
//! runs there.

use std::path::PathBuf;
use std::process::Command;

/// `true` when the wasm guest target is installed.
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

/// The workspace root (this crate's manifest dir → ../..).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn spinning_cube_compiles_and_caches() {
    if !has_wasm_target() {
        eprintln!("skipping: wasm32-unknown-unknown target not installed");
        return;
    }

    let root = workspace_root();
    let fixture = root.join("crates/idtx-behavior-build/fixtures/spinning_cube.rs");
    assert!(
        fixture.is_file(),
        "fixture missing at {}",
        fixture.display()
    );

    let sdk_path = root.join("crates/idtx-behavior-sdk");
    let out_dir = root.join("target/behaviors-test");
    // Fresh cache so the first build is a real compile.
    let _ = std::fs::remove_dir_all(&out_dir);

    let opts = idtx_behavior_build::BuildOptions::new(&out_dir, &sdk_path);

    // First build: real compile.
    let a = idtx_behavior_build::build_behavior(&fixture, &opts).expect("first build");
    assert_eq!(a.name, "spinning_cube");
    assert!(a.wasm_path.is_file(), "wasm not emitted");
    assert!(!a.cache_hit, "first build should not be a cache hit");

    // Second build: cache hit, same hash.
    let b = idtx_behavior_build::build_behavior(&fixture, &opts).expect("second build");
    assert!(b.cache_hit, "second build should hit cache");
    assert_eq!(a.hash, b.hash);

    // The emitted module is a non-trivial wasm binary.
    let bytes = std::fs::read(&a.wasm_path).expect("read wasm");
    assert!(bytes.len() > 8, "wasm too small");
    assert_eq!(&bytes[0..4], b"\0asm", "not a wasm module (bad magic)");

    let _ = std::fs::remove_dir_all(&out_dir);
}
