//! Shared paths for checked-in inputs and owned, automatically cleaned scratch space.
#![allow(dead_code)] // Each integration target uses only the helpers it needs.

use std::io::Read as _;
use std::path::{Path, PathBuf};

pub fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

pub fn required_file(path: PathBuf) -> PathBuf {
    let mut file = std::fs::File::open(&path)
        .unwrap_or_else(|error| panic!("required fixture {}: {error}", path.display()));
    let mut header = [0; 64];
    let count = file.read(&mut header).expect("read fixture header");
    assert!(
        !header[..count].starts_with(b"version https://git-lfs.github.com/spec/v1"),
        "fixture {} is an LFS pointer; run git lfs pull (keep LFS checkout enabled)",
        path.display()
    );
    path
}

pub fn usd_fixture(relative: &str) -> PathBuf {
    required_file(fixture_root().join("usd").join(relative))
}

pub fn scratch_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("idtx-{label}-"))
        .tempdir()
        .expect("create owned test scratch directory")
}
