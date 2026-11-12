//! Staging the `OpenUSD` runtime beside a packaged runtime binary.
//!
//! The [`cook`](crate::cook) step produces a portable *payload* and the
//! [`Packager`](crate::Packager) co-locates the runtime *binary* — but neither
//! ships the `OpenUSD` dynamic libraries the binary links against. On Windows those
//! must sit in the executable's own directory (there is no rpath), and `OpenUSD`'s
//! `PlugRegistry` discovers `usd/plugInfo.json` **relative to `usd_ms.dll`**, so
//! the whole set has to land flat beside the runtime `.exe`. Without it the
//! packaged runtime dies at DLL-load before `main` (a `0xc0000139` on Windows).
//!
//! This module copies that runtime support into a produced bundle. It is the
//! third copy of the same enumeration and the three must stay in sync:
//!
//! * [`crates/idtx-usd-sys/build_support/usd_runtime.rs`] — the *build-time* twin
//!   that stages the same DLLs + `usd/`/`plugin/` trees next to a `cargo run`
//!   binary (so the editor Just Works without `PATH`/`DYLD_LIBRARY_PATH`).
//! * `xtask`'s Windows bundler — the CI/release path.
//! * this module — what the Conductor "Build" command drives.
//!
//! # Standalone targets (Quest / Vision Pro)
//!
//! The desktop lists above are Windows/macOS/Linux DLL/dylib names. The two
//! **standalone** native-library lists (Android `.so`, visionOS `.dylib`) are
//! **not** duplicated here — they have a single source of truth in
//! [`idtx_standalone`] (`android_runtime_libs()` / `visionos_runtime_libs()`),
//! so the cross-build (`xtask build-usd-android` / `-visionos`) and the staging
//! here agree by construction. [`stage_standalone_usd_runtime`] below consumes
//! those lists to copy a cross-built staging prefix into a bundle.
//!
//! # Source resolution
//!
//! 1. `IDTX_USD_DIR` install prefix when set and complete — the authoritative
//!    source, including the `MaterialX` `libraries/` + `resources/` trees an
//!    install carries but a `cargo run` layout does not stage.
//! 2. otherwise the artifacts already staged **next to the editor executable**
//!    (by the build-time twin above) — always present for a running editor, and
//!    exactly the runtime the editor itself is linked against.

use std::path::{Path, PathBuf};

use crate::cook::CookError;
use crate::package::{copy_file, copy_tree};

/// The `OpenUSD` monolithic DLL, shipped from the install's `lib/`. Windows only.
#[cfg(target_os = "windows")]
const WIN_USD_LIB_DLLS: &[&str] = &["usd_ms.dll"];

/// `OpenUSD` + `MaterialX` DLLs shipped from the install's `bin/`. Windows only.
/// Mirrors `xtask`'s `WIN_USD_BIN_DLLS` and the build-time twin — keep in sync.
#[cfg(target_os = "windows")]
const WIN_USD_BIN_DLLS: &[&str] = &[
    // usd_ms.dll may also appear in bin/ in some layouts — belt-and-suspenders.
    "usd_ms.dll",
    "MaterialXCore.dll",
    "MaterialXFormat.dll",
    "MaterialXGenGlsl.dll",
    "MaterialXGenHw.dll",
    "MaterialXGenMdl.dll",
    "MaterialXGenMsl.dll",
    "MaterialXGenOsl.dll",
    "MaterialXGenShader.dll",
    "MaterialXGenSlang.dll",
    "MaterialXRender.dll",
    "MaterialXRenderGlsl.dll",
    "MaterialXRenderHw.dll",
    "MaterialXRenderOsl.dll",
];

/// Intel TBB DLLs `usd_ms` depends on. Present in `bin/` or `lib/` depending on
/// the build; both are searched. Windows only.
#[cfg(target_os = "windows")]
const WIN_TBB_DLLS: &[&str] = &[
    "tbb12.dll",
    "tbb.dll",
    "tbbmalloc.dll",
    "tbbmalloc_proxy.dll",
];

/// Resource/plugin trees that sit at the **install-prefix root** (not under
/// `lib/`). `usd/` is handled separately because it lives under `lib/` in an
/// install prefix but flat next to a runtime binary.
const USD_ROOT_TREES: &[&str] = &["plugin", "libraries", "resources"];

/// What [`stage_usd_runtime`] copied, for the caller to log / surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsdStageReport {
    /// Number of DLL / dynamic-library files copied beside the binary.
    pub libs: usize,
    /// Names of the resource/plugin trees copied (`usd`, `plugin`, …).
    pub trees: Vec<String>,
    /// Whether the `OpenUSD` monolithic library was found in the resolved source.
    /// `false` means the bundle will not run — the caller should warn loudly.
    pub monolithic_found: bool,
    /// Human-readable description of where the runtime was sourced from.
    pub source: String,
}

/// Stage the `OpenUSD` runtime a packaged binary needs at load time into `dest`
/// (the bundle root, flat beside the runtime `.exe`).
///
/// Prefers the `IDTX_USD_DIR` install prefix; falls back to the artifacts staged
/// next to `exe_dir` (the running editor's directory). See the module docs for
/// the source-resolution rules.
///
/// # Errors
///
/// [`CookError::Io`] if a file/tree copy fails. A *missing* `OpenUSD` install is
/// not an error — it yields a report with `monolithic_found == false` so the
/// caller can decide how loudly to complain.
pub fn stage_usd_runtime(dest: &Path, exe_dir: &Path) -> Result<UsdStageReport, CookError> {
    if let Some(usd_dir) = install_dir_from_env()
        && monolithic_lib_present(&usd_dir.join("lib")) {
            return stage_from_install(&usd_dir, dest);
        }
    stage_colocated(exe_dir, dest)
}

/// Stage from an `OpenUSD` **install prefix** (`<usd_dir>/lib`, `<usd_dir>/bin`,
/// the `<usd_dir>/lib/usd` discovery tree, and the root `plugin/`/`libraries/`/
/// `resources/` trees).
///
/// # Errors
///
/// [`CookError::Io`] if a copy fails.
pub fn stage_from_install(usd_dir: &Path, dest: &Path) -> Result<UsdStageReport, CookError> {
    let lib_dir = usd_dir.join("lib");
    let bin_dir = usd_dir.join("bin");
    let mut report = UsdStageReport {
        monolithic_found: monolithic_lib_present(&lib_dir),
        source: format!("OpenUSD install prefix {}", usd_dir.display()),
        ..UsdStageReport::default()
    };

    stage_dlls(&[&lib_dir, &bin_dir], dest, &mut report)?;
    // `usd/` (the plugInfo discovery tree) lives under `lib/` in an install.
    copy_named_tree(&lib_dir.join("usd"), &dest.join("usd"), "usd", &mut report)?;
    for tree in USD_ROOT_TREES {
        copy_named_tree(&usd_dir.join(tree), &dest.join(tree), tree, &mut report)?;
    }
    Ok(report)
}

/// Stage from the artifacts already staged **next to the editor executable** by
/// the build-time twin: DLLs are flat in `exe_dir`, and the `usd/`/`plugin/`
/// (and, if an `xtask` bundle, `libraries/`/`resources/`) trees are flat too.
///
/// # Errors
///
/// [`CookError::Io`] if a copy fails.
pub fn stage_colocated(exe_dir: &Path, dest: &Path) -> Result<UsdStageReport, CookError> {
    let mut report = UsdStageReport {
        monolithic_found: colocated_monolithic_present(exe_dir),
        source: format!("runtime libraries beside editor ({})", exe_dir.display()),
        ..UsdStageReport::default()
    };

    stage_dlls(&[exe_dir], dest, &mut report)?;
    copy_named_tree(&exe_dir.join("usd"), &dest.join("usd"), "usd", &mut report)?;
    for tree in USD_ROOT_TREES {
        copy_named_tree(&exe_dir.join(tree), &dest.join(tree), tree, &mut report)?;
    }
    Ok(report)
}

/// Stage a **cross-built standalone** `OpenUSD` runtime (produced by
/// `xtask build-usd-android` / `build-usd-visionos`) from its staging prefix
/// into a bundle.
///
/// `prefix` is the `thirdparty/openusd-<target>/` install prefix. Native
/// libraries are copied into `lib_dest` (the APK `jniLibs/arm64-v8a/` for
/// Quest, the `.app` `Contents/Frameworks/` for Vision Pro); the plugin
/// discovery trees (`usd`, `plugin`, `libraries`, `resources`) are copied into
/// `tree_dest` (the APK `assets/`, or beside the frameworks on visionOS).
///
/// The library name set is [`idtx_standalone`]'s per-platform enumeration —
/// the single source of truth, so this never drifts from the cross-build. A
/// library present in the enumeration but absent from the prefix is skipped
/// (an optional `MaterialX` generator may not build on every toolchain), and
/// recorded via `monolithic_found` only for the monolithic library itself.
///
/// # Errors
///
/// [`CookError::Io`] if a copy fails.
pub fn stage_standalone_usd_runtime(
    prefix: &Path,
    lib_dest: &Path,
    tree_dest: &Path,
    platform: idtx_standalone::StandalonePlatform,
) -> Result<UsdStageReport, CookError> {
    let lib_src = prefix.join("lib");
    let monolithic = platform.runtime_lib_names().into_iter().next();
    let mut report = UsdStageReport {
        source: format!("standalone staging prefix {}", prefix.display()),
        ..UsdStageReport::default()
    };

    for name in platform.runtime_lib_names() {
        // The cross-build installs libs under lib/; be lenient and also accept
        // a flat prefix (matching the desktop colocated fallback shape).
        let src = [lib_src.join(&name), prefix.join(&name)]
            .into_iter()
            .find(|p| p.is_file());
        let Some(src) = src else { continue };
        copy_file(&src, &lib_dest.join(&name))?;
        report.libs += 1;
        if monolithic.as_deref() == Some(name.as_str()) {
            report.monolithic_found = true;
        }
    }

    // `usd/` discovery tree lives under lib/ in an install prefix.
    copy_named_tree(
        &lib_src.join("usd"),
        &tree_dest.join("usd"),
        "usd",
        &mut report,
    )?;
    for tree in USD_ROOT_TREES {
        copy_named_tree(&prefix.join(tree), &tree_dest.join(tree), tree, &mut report)?;
    }
    Ok(report)
}

// ── DLL / dynamic-library staging ────────────────────────────────────────────
/// Copy the `OpenUSD`/`MaterialX`/TBB dynamic libraries from the first matching
/// `search_dirs` entry into `dest`, flat. Best-effort per file (a missing
/// optional `MaterialX` generator is skipped, not an error).
#[cfg(target_os = "windows")]
fn stage_dlls(
    search_dirs: &[&Path],
    dest: &Path,
    report: &mut UsdStageReport,
) -> Result<(), CookError> {
    for dir in search_dirs {
        copy_named(dir, dest, WIN_USD_LIB_DLLS, report)?;
        copy_named(dir, dest, WIN_USD_BIN_DLLS, report)?;
        copy_named(dir, dest, WIN_TBB_DLLS, report)?;
    }
    Ok(())
}

/// Copy the native USD, TBB and `MaterialX` families, retaining every versioned
/// soname alias. Rpath/signature finalization remains the embedding packager's
/// job; omitting `MaterialX` here produced non-launchable relocated packages.
#[cfg(not(target_os = "windows"))]
fn stage_dlls(
    search_dirs: &[&Path],
    dest: &Path,
    report: &mut UsdStageReport,
) -> Result<(), CookError> {
    const PREFIXES: &[&str] = &["libusd_ms", "libtbb", "libMaterialX"];
    for dir in search_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let matches = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| PREFIXES.iter().any(|p| name.starts_with(p)));
            if matches {
                copy_file(&path, &dest.join(entry.file_name()))?;
                report.libs += 1;
            }
        }
    }
    Ok(())
}

/// Copy each named file from `src_dir` to `dst_dir`, skipping any that are
/// absent (not all optional `MaterialX` generators ship in every build).
#[cfg(target_os = "windows")]
fn copy_named(
    src_dir: &Path,
    dst_dir: &Path,
    names: &[&str],
    report: &mut UsdStageReport,
) -> Result<(), CookError> {
    for name in names {
        let src = src_dir.join(name);
        if !src.is_file() {
            continue;
        }
        let dst = dst_dir.join(name);
        // Skip a name already staged from an earlier search dir (usd_ms.dll
        // appears in both the lib and bin lists / dirs).
        if dst.is_file() {
            continue;
        }
        copy_file(&src, &dst)?;
        report.libs += 1;
    }
    Ok(())
}

// ── shared helpers ────────────────────────────────────────────────────────────

/// Copy a resource tree (`usd`, `plugin`, …) when the source exists, recording
/// it in `report`. A missing tree is a silent no-op.
fn copy_named_tree(
    src: &Path,
    dst: &Path,
    label: &str,
    report: &mut UsdStageReport,
) -> Result<(), CookError> {
    if src.is_dir() {
        copy_tree(src, dst)?;
        report.trees.push(label.to_string());
    }
    Ok(())
}

/// The `IDTX_USD_DIR` install prefix, when set. Unlike the build-time twin there
/// is no `CARGO_MANIFEST_DIR` workspace fallback (this runs in a shipped editor).
fn install_dir_from_env() -> Option<PathBuf> {
    std::env::var_os("IDTX_USD_DIR").map(PathBuf::from)
}

/// Whether the `OpenUSD` monolithic **runtime** library is present in `lib_dir`.
fn monolithic_lib_present(lib_dir: &Path) -> bool {
    monolithic_names().iter().any(|n| lib_dir.join(n).is_file())
}

/// Whether a monolithic runtime library sits flat next to the editor executable.
fn colocated_monolithic_present(exe_dir: &Path) -> bool {
    monolithic_names().iter().any(|n| exe_dir.join(n).is_file())
}

/// Host-appropriate monolithic runtime library file names.
fn monolithic_names() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        &["usd_ms.dll"]
    }
    #[cfg(target_os = "macos")]
    {
        &["libusd_ms.dylib"]
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        &["libusd_ms.so"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn native_staging_materializes_materialx_soname_aliases() {
        let root = tempfile::tempdir().unwrap();
        let lib = root.path().join("lib");
        std::fs::create_dir(&lib).unwrap();
        std::fs::write(lib.join("libMaterialXCore.1.39.dylib"), b"materialx").unwrap();
        std::os::unix::fs::symlink("libMaterialXCore.1.39.dylib", lib.join("libMaterialXCore.1.dylib")).unwrap();
        let out = root.path().join("out");
        stage_colocated(&lib, &out).unwrap();
        for name in ["libMaterialXCore.1.39.dylib", "libMaterialXCore.1.dylib"] {
            assert_eq!(std::fs::read(out.join(name)).unwrap(), b"materialx");
            assert!(!std::fs::symlink_metadata(out.join(name)).unwrap().is_symlink());
        }
    }
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-usdrt-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The monolithic runtime library file name to fabricate for the host.
    fn monolithic_file() -> &'static str {
        monolithic_names()[0]
    }

    /// A fake `OpenUSD` install prefix: the monolithic lib, one `MaterialX`/TBB DLL
    /// (Windows), the `usd/` discovery tree, and a root `plugin/` tree.
    fn fake_install(dir: &Path) {
        let lib = dir.join("lib");
        let bin = dir.join("bin");
        fs::create_dir_all(&lib).unwrap();
        fs::create_dir_all(&bin).unwrap();
        fs::write(lib.join(monolithic_file()), b"LIB").unwrap();
        #[cfg(target_os = "windows")]
        {
            fs::write(bin.join("MaterialXCore.dll"), b"MX").unwrap();
            fs::write(bin.join("tbb12.dll"), b"TBB").unwrap();
        }
        fs::create_dir_all(lib.join("usd")).unwrap();
        fs::write(lib.join("usd").join("plugInfo.json"), b"{}").unwrap();
        fs::create_dir_all(dir.join("plugin").join("usdShaders")).unwrap();
        fs::write(
            dir.join("plugin").join("usdShaders").join("plugInfo.json"),
            b"{}",
        )
        .unwrap();
    }

    #[test]
    fn stage_from_install_copies_monolith_usd_tree_and_plugins() {
        let root = scratch("install");
        let usd = root.join("usd-install");
        fake_install(&usd);
        let dest = root.join("bundle");
        fs::create_dir_all(&dest).unwrap();

        let report = stage_from_install(&usd, &dest).unwrap();

        assert!(report.monolithic_found, "monolithic lib detected");
        // The monolithic library landed flat in the bundle root.
        assert!(dest.join(monolithic_file()).is_file(), "monolithic staged");
        assert!(report.libs >= 1, "at least the monolithic lib counted");
        // `usd/` discovery tree is a sibling of the monolithic lib (USD requires
        // plugInfo.json relative to usd_ms.dll).
        assert!(
            dest.join("usd").join("plugInfo.json").is_file(),
            "usd/ tree staged"
        );
        assert!(report.trees.contains(&"usd".to_string()));
        // The root `plugin/` tree came across too.
        assert!(
            dest.join("plugin")
                .join("usdShaders")
                .join("plugInfo.json")
                .is_file(),
            "plugin/ tree staged"
        );
        assert!(report.trees.contains(&"plugin".to_string()));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn stage_from_install_copies_materialx_and_tbb_dlls() {
        let root = scratch("install-dlls");
        let usd = root.join("usd-install");
        fake_install(&usd);
        let dest = root.join("bundle");
        fs::create_dir_all(&dest).unwrap();

        stage_from_install(&usd, &dest).unwrap();

        assert!(dest.join("usd_ms.dll").is_file(), "monolithic DLL staged");
        assert!(
            dest.join("MaterialXCore.dll").is_file(),
            "MaterialX DLL staged"
        );
        assert!(dest.join("tbb12.dll").is_file(), "TBB DLL staged");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stage_colocated_sources_from_editor_dir() {
        let root = scratch("colocated");
        // Simulate the editor's own directory: the build-time twin staged the
        // monolithic lib + usd/ tree flat next to the exe.
        let exe_dir = root.join("target-debug");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(exe_dir.join(monolithic_file()), b"LIB").unwrap();
        fs::create_dir_all(exe_dir.join("usd")).unwrap();
        fs::write(exe_dir.join("usd").join("plugInfo.json"), b"{}").unwrap();

        let dest = root.join("bundle");
        fs::create_dir_all(&dest).unwrap();

        let report = stage_colocated(&exe_dir, &dest).unwrap();

        assert!(report.monolithic_found);
        assert!(
            dest.join(monolithic_file()).is_file(),
            "monolithic staged flat"
        );
        assert!(
            dest.join("usd").join("plugInfo.json").is_file(),
            "usd/ tree staged"
        );
        assert!(report.libs >= 1);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_source_reports_absent_monolith_without_error() {
        let root = scratch("empty");
        let exe_dir = root.join("empty-dir");
        fs::create_dir_all(&exe_dir).unwrap();
        let dest = root.join("bundle");
        fs::create_dir_all(&dest).unwrap();

        // Nothing to source from → not an error, but flagged so the caller warns.
        let report = stage_colocated(&exe_dir, &dest).unwrap();
        assert!(!report.monolithic_found);
        assert_eq!(report.libs, 0);
        assert!(report.trees.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    /// A fake cross-build staging prefix: lib/<monolithic>+<tbb>, the lib/usd/
    /// discovery tree, and a root `plugin/` tree. `MaterialX` libs are intentionally
    /// absent to exercise the skip-missing path.
    fn fake_standalone_prefix(dir: &Path, plat: idtx_standalone::StandalonePlatform) {
        let lib = dir.join("lib");
        fs::create_dir_all(&lib).unwrap();
        // Stage only the first two enumerated libs (monolithic + tbb); the
        // MaterialX generators are left out so the staging must skip them.
        for name in plat.runtime_lib_names().into_iter().take(2) {
            fs::write(lib.join(name), b"SO").unwrap();
        }
        fs::create_dir_all(lib.join("usd")).unwrap();
        fs::write(lib.join("usd").join("plugInfo.json"), b"{}").unwrap();
        fs::create_dir_all(dir.join("plugin").join("usdShaders")).unwrap();
        fs::write(
            dir.join("plugin").join("usdShaders").join("plugInfo.json"),
            b"{}",
        )
        .unwrap();
    }

    #[test]
    fn stage_standalone_copies_present_libs_and_trees_skipping_absent() {
        use idtx_standalone::StandalonePlatform;
        let root = scratch("standalone");
        let prefix = root.join("openusd-android-arm64");
        fake_standalone_prefix(&prefix, StandalonePlatform::Quest);
        let libs = root.join("jniLibs");
        let trees = root.join("assets");
        fs::create_dir_all(&libs).unwrap();
        fs::create_dir_all(&trees).unwrap();

        let report =
            stage_standalone_usd_runtime(&prefix, &libs, &trees, StandalonePlatform::Quest)
                .unwrap();

        // Only the two fabricated libs were copied; the absent MaterialX/cxx
        // entries from the enumeration are skipped, not errors.
        assert_eq!(report.libs, 2, "only present libs staged");
        assert!(report.monolithic_found, "monolithic libusd_ms.so detected");
        let mono = StandalonePlatform::Quest.runtime_lib_names()[0].clone();
        assert!(
            libs.join(&mono).is_file(),
            "monolithic lib staged into lib_dest"
        );
        // Discovery trees land in the (separate) tree destination, not beside libs.
        assert!(
            trees.join("usd").join("plugInfo.json").is_file(),
            "usd/ tree staged"
        );
        assert!(report.trees.contains(&"usd".to_string()));
        assert!(
            trees
                .join("plugin")
                .join("usdShaders")
                .join("plugInfo.json")
                .is_file(),
            "plugin/ tree staged"
        );
        assert!(report.trees.contains(&"plugin".to_string()));
        assert!(
            !libs.join("usd").exists(),
            "trees must not land in lib_dest"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stage_standalone_absent_prefix_reports_no_monolith_without_error() {
        use idtx_standalone::StandalonePlatform;
        let root = scratch("standalone-empty");
        let prefix = root.join("missing-prefix");
        fs::create_dir_all(&prefix).unwrap();
        let libs = root.join("jniLibs");
        let trees = root.join("assets");
        fs::create_dir_all(&libs).unwrap();
        fs::create_dir_all(&trees).unwrap();

        let report =
            stage_standalone_usd_runtime(&prefix, &libs, &trees, StandalonePlatform::VisionPro)
                .unwrap();
        assert_eq!(report.libs, 0);
        assert!(!report.monolithic_found);
        assert!(report.trees.is_empty());

        fs::remove_dir_all(&root).ok();
    }
}
