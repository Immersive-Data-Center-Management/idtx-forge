//! Self-contained SDK sysroot: staging + path resolution for behavior builds.
//!
//! A guest behavior compiles as a *standalone* scratch crate (empty
//! `[workspace]` table) that depends on `idtx-behavior-sdk` **by filesystem
//! path** (see [`crate::skeleton`]). For that path dependency to resolve, the
//! SDK crate — and its two internal deps `idtx-behavior-abi` and
//! `idtx-behavior-macros` — must be reachable on disk *and* belong to a Cargo
//! workspace that supplies the `[workspace.package]` /
//! `[workspace.dependencies]` tables their `Cargo.toml`s inherit from.
//!
//! In the dev checkout that workspace is the repo root, so [`resolve_sdk_dir`]
//! points at `<workspace>/crates/idtx-behavior-sdk`. In a *shipped* app (e.g.
//! the bundled `idtx-conductor` `.app`) the repo is absent, so [`stage_sdk`]
//! writes a **self-contained SDK sysroot** — the three crate sources plus a
//! generated `[workspace]` root `Cargo.toml` — that [`resolve_sdk_dir`] then
//! discovers next to the running executable.

use std::path::{Path, PathBuf};

use crate::Result;

/// Directory name of the staged SDK sysroot inside a shipped app.
///
/// Placed next to the executable (portable layout) and under
/// `Contents/Resources/` in a macOS `.app`. [`resolve_sdk_dir`] probes both.
pub const SDK_SYSROOT_DIR: &str = "behavior-sdk";

/// Env override pointing directly at an `idtx-behavior-sdk` crate directory.
///
/// Highest-precedence input to [`resolve_sdk_dir`]; lets packagers or tests
/// pin the SDK explicitly regardless of the executable layout.
pub const SDK_ENV: &str = "IDTX_BEHAVIOR_SDK";

/// The three workspace crates a guest behavior links, in dependency order.
/// `idtx-behavior-sdk` is the direct dep; the other two are transitive.
const SDK_CRATES: [&str; 3] = [
    "idtx-behavior-abi",
    "idtx-behavior-macros",
    "idtx-behavior-sdk",
];

/// Generate the SDK workspace from the source workspace's actual inheritance
/// tables. No second copy of the ABI's dependency/version policy lives here.
fn workspace_for_sdk(src_crates_dir: &Path) -> Result<String> {
    use crate::BuildError;
    fn inherited_dependencies(value: &toml::Value, names: &mut std::collections::BTreeSet<String>) {
        if let Some(table) = value.as_table() {
            for (key, value) in table {
                if ["dependencies", "build-dependencies", "dev-dependencies"].contains(&key.as_str()) {
                    if let Some(dependencies) = value.as_table() {
                        for (name, dependency) in dependencies {
                            if dependency.get("workspace").and_then(toml::Value::as_bool) == Some(true) { names.insert(name.clone()); }
                        }
                    }
                } else { inherited_dependencies(value, names); }
            }
        }
    }
    let root = src_crates_dir.parent().ok_or_else(|| BuildError::InvalidInput("SDK crates have no workspace parent".into()))?;
    let source: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)
        .map_err(|e| BuildError::InvalidInput(e.to_string()))?;
    let workspace = source.get("workspace").and_then(toml::Value::as_table)
        .ok_or_else(|| BuildError::InvalidInput("SDK source has no Cargo workspace".into()))?;
    let mut inherited = std::collections::BTreeSet::new();
    for name in SDK_CRATES {
        let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(src_crates_dir.join(name).join("Cargo.toml"))?)
            .map_err(|e| BuildError::InvalidInput(e.to_string()))?;
        inherited_dependencies(&manifest, &mut inherited);
    }
    let mut dependencies = toml::map::Map::new();
    for name in inherited {
        let mut value = workspace.get("dependencies").and_then(|d| d.get(&name)).cloned()
            .ok_or_else(|| BuildError::InvalidInput(format!("SDK inherits missing dependency {name}")))?;
        if let Some(path) = value.get("path").and_then(toml::Value::as_str) {
            let path = std::fs::canonicalize(root.join(path))?;
            let staged_name = SDK_CRATES.iter().find(|name| std::fs::canonicalize(src_crates_dir.join(name)).is_ok_and(|p| p == path))
                .ok_or_else(|| BuildError::InvalidInput("SDK has an unstaged local workspace dependency".into()))?;
            value.as_table_mut().expect("path dependency table").insert("path".into(), (*staged_name).into());
        }
        dependencies.insert(name, value);
    }
    let mut generated = toml::map::Map::new();
    for key in ["resolver", "package", "lints"] {
        if let Some(value) = workspace.get(key) { generated.insert(key.into(), value.clone()); }
    }
    generated.insert("members".into(), SDK_CRATES.iter().map(|s| toml::Value::String((*s).into())).collect::<Vec<_>>().into());
    generated.insert("dependencies".into(), toml::Value::Table(dependencies));
    let document = toml::Value::Table(toml::map::Map::from_iter([("workspace".into(), toml::Value::Table(generated))]));
    toml::to_string_pretty(&document).map_err(|e| BuildError::InvalidInput(e.to_string()))
}

/// Stage a **self-contained SDK sysroot** at `dest` for shipping.
///
/// Copies the three SDK crates (`idtx-behavior-abi`, `idtx-behavior-macros`,
/// `idtx-behavior-sdk`) out of the workspace `crates/` directory and writes a
/// generated `[workspace]` root `Cargo.toml`, producing a tree a scratch
/// behavior crate can depend on by path with **no repo present**. Reach for
/// this from a packager (e.g. `cargo xtask bundle-conductor`) after the binary
/// is built. `src_crates_dir` is the workspace's `crates/` directory (the
/// source of truth); `dest` is where the sysroot is written (e.g.
/// `<app>.app/Contents/Resources/behavior-sdk`).
///
/// The staged tree matches the layout [`resolve_sdk_dir`] discovers:
/// `<dest>/{Cargo.toml, idtx-behavior-sdk/, idtx-behavior-abi/,
/// idtx-behavior-macros/}`. Existing contents at `dest` are removed first so a
/// re-stage never leaves stale sources behind.
pub fn stage_sdk(src_crates_dir: &Path, dest: &Path) -> Result<()> {
    let manifest = workspace_for_sdk(src_crates_dir)?;
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;

    for crate_name in SDK_CRATES {
        let from = src_crates_dir.join(crate_name);
        if !from.join("Cargo.toml").is_file() {
            return Err(crate::BuildError::SourceNotFound(from.join("Cargo.toml")));
        }
        // Copy `src/` and `Cargo.toml` only — skip any `target/`, `.git`, or
        // build artifacts so the sysroot stays minimal and reproducible.
        let to = dest.join(crate_name);
        std::fs::create_dir_all(&to)?;
        std::fs::copy(from.join("Cargo.toml"), to.join("Cargo.toml"))?;
        copy_dir_recursive(&from.join("src"), &to.join("src"))?;
        if from.join("build.rs").is_file() { std::fs::copy(from.join("build.rs"), to.join("build.rs"))?; }
    }

    std::fs::write(dest.join("Cargo.toml"), manifest)?;
    let lock = src_crates_dir.parent().ok_or_else(|| crate::BuildError::InvalidInput("SDK workspace parent missing".into()))?.join("Cargo.lock");
    if lock.is_file() { std::fs::copy(lock, dest.join("Cargo.lock"))?; }
    Ok(())
}

/// Recursively copy `from` into `to`, skipping symlinks. Used by [`stage_sdk`]
/// to lift each crate's `src/` tree into the sysroot.
fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Resolve the `idtx-behavior-sdk` crate directory the compile skeleton should
/// depend on, given the running executable's path.
///
/// Precedence — first hit wins:
/// 1. `$IDTX_BEHAVIOR_SDK` (an explicit override; the builder validates it and
///    reports an invalid path instead of silently selecting another SDK).
/// 2. A staged sysroot next to the executable (portable layout; the Windows
///    shipping folder and the general case):
///    `<exe_dir>/behavior-sdk/idtx-behavior-sdk`.
/// 3. A staged sysroot in a macOS `.app`:
///    `<exe_dir>/../Resources/behavior-sdk/idtx-behavior-sdk`.
///
/// Candidate 3 is a macOS-only layout; on Windows/Linux it simply never
/// matches, so the same code path is correct on every OS.
///
/// Returns `None` when no shipped SDK is found — callers then fall back to
/// their dev-checkout path (the workspace `crates/idtx-behavior-sdk`). Pass the
/// result of [`std::env::current_exe`] as `exe_path`; kept as a parameter so
/// this stays unit-testable.
#[must_use]
pub fn resolve_sdk_dir(exe_path: Option<&Path>) -> Option<PathBuf> {
    resolve_with_override(exe_path, std::env::var_os(SDK_ENV).map(PathBuf::from))
}

fn resolve_with_override(exe_path: Option<&Path>, explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = explicit { return Some(dir); }

    let exe_dir = exe_path.and_then(Path::parent)?;
    let candidates = [
        exe_dir.join(SDK_SYSROOT_DIR).join("idtx-behavior-sdk"),
        exe_dir
            .parent()
            .map(|p| {
                p.join("Resources")
                    .join(SDK_SYSROOT_DIR)
                    .join("idtx-behavior-sdk")
            })
            .unwrap_or_default(),
    ];
    candidates
        .into_iter()
        .find(|c| c.join("Cargo.toml").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_explicit_sdk_does_not_select_a_valid_adjacent_sdk() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join(SDK_SYSROOT_DIR).join("idtx-behavior-sdk");
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(fallback.join("Cargo.toml"), "[package]\n").unwrap();
        let missing = dir.path().join("missing-sdk");
        assert_eq!(resolve_with_override(Some(&dir.path().join("editor")), Some(missing.clone())), Some(missing));
    }

    /// The repo `crates/` directory (this crate's manifest dir → parent).
    fn crates_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir")
            .to_path_buf()
    }

    #[test]
    fn stage_writes_self_contained_sysroot() {
        let dest = std::env::temp_dir().join(format!("idtx-sdk-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        stage_sdk(&crates_dir(), &dest).expect("stage");

        // Workspace root + the three crate trees exist.
        assert!(dest.join("Cargo.toml").is_file());
        for crate_name in SDK_CRATES {
            assert!(
                dest.join(crate_name).join("Cargo.toml").is_file(),
                "missing {crate_name}/Cargo.toml"
            );
            assert!(
                dest.join(crate_name).join("src").join("lib.rs").is_file(),
                "missing {crate_name}/src/lib.rs"
            );
        }
        // The generated root declares the workspace + inherited pins, including
        // the exact glam ABI pin.
        let root = std::fs::read_to_string(dest.join("Cargo.toml")).unwrap();
        assert!(root.contains("[workspace]"));
        assert!(root.contains("[workspace.package]"));
        assert!(root.contains("edition = \"2024\""));
        let generated: toml::Value = toml::from_str(&root).unwrap();
        let original: toml::Value = toml::from_str(&std::fs::read_to_string(crates_dir().parent().unwrap().join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(generated["workspace"]["dependencies"]["glam"], original["workspace"]["dependencies"]["glam"]);
        assert_eq!(generated["workspace"]["dependencies"]["syn"], original["workspace"]["dependencies"]["syn"]);

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn resolve_finds_portable_sysroot_next_to_exe() {
        let tmp = std::env::temp_dir().join(format!("idtx-sdk-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let sdk = tmp.join(SDK_SYSROOT_DIR).join("idtx-behavior-sdk");
        std::fs::create_dir_all(&sdk).unwrap();
        std::fs::write(sdk.join("Cargo.toml"), "[package]\n").unwrap();
        let exe = tmp.join("idtx-conductor");
        std::fs::write(&exe, b"").unwrap();

        // Guard on the env override: a dev machine / CI may set IDTX_BEHAVIOR_SDK,
        // which takes precedence and would shadow the exe-adjacent layout.
        if std::env::var_os(SDK_ENV).is_none() {
            let resolved = resolve_sdk_dir(Some(&exe));
            assert_eq!(resolved.as_deref(), Some(sdk.as_path()));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_returns_none_without_sysroot() {
        let tmp = std::env::temp_dir().join(format!("idtx-sdk-none-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let exe = tmp.join("idtx-conductor");
        std::fs::write(&exe, b"").unwrap();
        // No env override in this test; the dev machine may set IDTX_BEHAVIOR_SDK
        // in CI, so guard on it to keep the assertion deterministic.
        if std::env::var_os(SDK_ENV).is_none() {
            assert!(resolve_sdk_dir(Some(&exe)).is_none());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
