//! Shared build-script helpers for the `OpenUSD` crates.
//!
//! Used as a `build-dependency` by `idtx-usd-sys`. Centralizes MSVC environment
//! detection and USD install resolution in one place so the logic is not
//! copy-pasted across build scripts.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolves the `OpenUSD` install prefix.
///
/// Resolution order:
/// 1. `$IDTX_USD_DIR` environment variable (must point to the install prefix).
/// 2. `<workspace>/thirdparty/openusd-26.08-mtlx` (conventional workspace layout).
///
/// `manifest_dir` should be `env::var_os("CARGO_MANIFEST_DIR")` of the calling
/// crate (i.e. `crates/idtx-xxx`); two `parent()` calls reach the workspace root.
///
/// # Panics
///
/// Panics if `manifest_dir` does not have at least two ancestor directories
/// (i.e. it is not a `crates/<crate>` directory beneath the workspace root).
/// Only reached when `$IDTX_USD_DIR` is unset.
#[must_use]
pub fn resolve_usd_dir(manifest_dir: &Path) -> PathBuf {
    if let Some(v) = env::var_os("IDTX_USD_DIR") {
        return PathBuf::from(v);
    }
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root is two levels above the crate manifest dir")
        .join("thirdparty/openusd-26.08-mtlx")
}

/// Resolve the `OpenUSD` install prefix for a compilation `target_os`.
///
/// For a native (host) build this is [`resolve_usd_dir`] (`$IDTX_USD_DIR` → the
/// workspace `thirdparty/openusd-26.08-mtlx`). For a **foreign standalone**
/// target the host install is the wrong ABI, so resolution follows the
/// cross-build staging convention:
/// * `android` → `$IDTX_USD_DIR_ANDROID` → `$IDTX_USD_DIR`
///   → `<workspace>/thirdparty/openusd-android-arm64` (`xtask build-usd-android`).
/// * `visionos` → `$IDTX_USD_DIR_VISIONOS` → `$IDTX_USD_DIR`
///   → `<workspace>/thirdparty/openusd-visionos-arm64` (`xtask build-usd-visionos`).
///
/// `target_os` is the build script's `CARGO_CFG_TARGET_OS` (NOT `cfg!(target_os)`,
/// which is the HOST). Used by `idtx-usd-sys` (the cxx bridge) so native and
/// cross builds resolve the prefix identically.
///
/// # Panics
///
/// For a foreign standalone `target_os` (`android` / `visionos`) with neither
/// the target-specific `$IDTX_USD_DIR_*` nor `$IDTX_USD_DIR` set — and, on the
/// native path, via [`resolve_usd_dir`] — panics if `manifest_dir` does not have
/// at least two ancestor directories (the workspace root is two levels up).
#[must_use]
pub fn resolve_usd_dir_for_target(manifest_dir: &Path, target_os: &str) -> PathBuf {
    let standalone = match target_os {
        "android" => Some(("IDTX_USD_DIR_ANDROID", "openusd-android-arm64")),
        "visionos" => Some(("IDTX_USD_DIR_VISIONOS", "openusd-visionos-arm64")),
        _ => None,
    };
    if let Some((env_key, leaf)) = standalone {
        println!("cargo:rerun-if-env-changed={env_key}");
        if let Some(v) = env::var_os(env_key) {
            return PathBuf::from(v);
        }
        if let Some(v) = env::var_os("IDTX_USD_DIR") {
            return PathBuf::from(v);
        }
        return manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("workspace root is two levels above the crate manifest dir")
            .join("thirdparty")
            .join(leaf);
    }
    resolve_usd_dir(manifest_dir)
}

/// Resolve the **standalone** (android / visionOS) sibling prefixes (oneTBB, and
/// `MaterialX` when the C++ `WgslShaderGenerator` fast path is compiled in) whose
/// `include/` and `lib/` an `OpenUSD` C++ consumer needs but which are not inside
/// the `OpenUSD` install prefix.
///
/// A desktop `build_usd.py` install bundles oneTBB (and, in the `-mtlx` build,
/// `MaterialX`) into the USD prefix itself, so `install/{include,lib}` suffices.
/// The `xtask build-usd-android` / `build-usd-visionos` cross-builds install
/// oneTBB and `MaterialX` into separate sibling prefixes under `thirdparty/`
/// (`onetbb-<plat>`, `materialx-<plat>`), so their prefixes must be found and
/// added explicitly.
///
/// `tbb` and `materialx` are each an `(env_override_key, thirdparty_leaf)` pair.
/// Each prefix resolves from the env override, then a sibling of the USD install
/// (the staging convention), then the workspace `thirdparty/<leaf>` default.
/// Returns the resolved prefix directories (TBB first); the caller joins
/// `include`/`lib` as needed.
#[must_use]
pub fn standalone_sibling_prefixes(
    install: &Path,
    manifest_dir: &Path,
    want_materialx: bool,
    tbb: (&str, &str),
    materialx: (&str, &str),
) -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    // oneTBB — always required: `pxr/base/tf/diagnosticMgr.h` pulls `<tbb/...>`
    // and the monolithic lib carries a oneTBB dependency.
    push_prefix(&mut prefixes, tbb.0, tbb.1, install, manifest_dir);
    // MaterialX — only when the C++ WgslShaderGenerator fast path is compiled in.
    if want_materialx {
        push_prefix(&mut prefixes, materialx.0, materialx.1, install, manifest_dir);
    }
    prefixes
}

/// Resolve one standalone sibling prefix and push it when present, emitting a
/// `cargo:warning` otherwise (the failure would otherwise surface later as a
/// confusing "file not found" from the cross-compiler or a missing `-l` at link).
fn push_prefix(
    prefixes: &mut Vec<PathBuf>,
    env_key: &str,
    leaf: &str,
    install: &Path,
    manifest_dir: &Path,
) {
    println!("cargo:rerun-if-env-changed={env_key}");
    let prefix = env::var_os(env_key)
        .map(PathBuf::from)
        .or_else(|| sibling_prefix(install, leaf))
        .or_else(|| workspace_thirdparty(manifest_dir, leaf));
    match prefix {
        Some(p) if p.is_dir() => prefixes.push(p),
        _ => println!(
            "cargo:warning=standalone OpenUSD consumer: `{leaf}` prefix not found \
             (expected a cross-built prefix with include/ and lib/). Set {env_key} \
             or run the matching `cargo xtask build-usd-*` to stage thirdparty/{leaf}."
        ),
    }
}

/// `<install>/../<leaf>` if it is a directory — the sibling-prefix staging layout.
fn sibling_prefix(install: &Path, leaf: &str) -> Option<PathBuf> {
    let p = install.parent()?.join(leaf);
    p.is_dir().then_some(p)
}

/// `<workspace>/thirdparty/<leaf>` if it is a directory (workspace is two levels
/// above the crate manifest dir).
fn workspace_thirdparty(manifest_dir: &Path, leaf: &str) -> Option<PathBuf> {
    let p = manifest_dir
        .parent()
        .and_then(Path::parent)?
        .join("thirdparty")
        .join(leaf);
    p.is_dir().then_some(p)
}

/// Primes the MSVC build environment in the current process.
///
/// On Windows: locates the latest Visual Studio installation via `vswhere.exe`,
/// runs `vcvars64.bat`, and applies the resulting `PATH`/`INCLUDE`/`LIB`/…
/// variables so `cl.exe`, `link.exe`, and friends are discoverable by child
/// processes spawned from the build script.
///
/// No-op on macOS and Linux.
pub fn prime_msvc_env() {
    if !cfg!(target_os = "windows") {
        return;
    }
    let Some(vs_path) = find_vs() else {
        println!(
            "cargo:warning=idtx-usd-build-support: could not locate Visual Studio via \
             vswhere; relying on caller's PATH for MSVC"
        );
        return;
    };
    let vcvars = vs_path
        .join("VC")
        .join("Auxiliary")
        .join("Build")
        .join("vcvars64.bat");
    if !vcvars.is_file() {
        println!(
            "cargo:warning=idtx-usd-build-support: vcvars64.bat not found at {}",
            vcvars.display()
        );
        return;
    }
    println!(
        "cargo:warning=idtx-usd-build-support: priming MSVC environment via {}",
        vcvars.display()
    );

    let Some(env_map) = dump_msvc_env(&vcvars) else {
        return;
    };

    for (k, v) in env_map {
        let k_upper = k.to_uppercase();
        match k_upper.as_str() {
            "PATH" | "INCLUDE" | "LIB" | "LIBPATH" | "WINDOWSSDKDIR" | "WINDOWSSDKVERSION"
            | "VCTOOLSINSTALLDIR" | "VCINSTALLDIR" => {
                // SAFETY: build scripts run single-threaded before the
                // Rust crate is compiled, so `set_var` cannot race.
                unsafe {
                    env::set_var(&k_upper, &v);
                }
            }
            _ => {}
        }
    }
}

fn find_vs() -> Option<PathBuf> {
    let pf86 = env::var_os("ProgramFiles(x86)")
        .unwrap_or_else(|| std::ffi::OsString::from(r"C:\Program Files (x86)"));
    let vswhere = Path::new(&pf86)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return None;
    }
    let out = Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
            "-format",
            "value",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

fn dump_msvc_env(vcvars: &Path) -> Option<std::collections::HashMap<String, String>> {
    let sep = "=== IDTX_MSVC_ENV_BEGIN ===";
    let bat_path = env::temp_dir().join(format!(
        "idtx_msvc_{}_{}.bat",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    {
        let mut f = fs::File::create(&bat_path).ok()?;
        let _ = writeln!(f, "@echo off");
        let _ = writeln!(f, "call \"{}\" >NUL", vcvars.display());
        let _ = writeln!(f, "if errorlevel 1 exit /b %errorlevel%");
        let _ = writeln!(f, "echo {sep}");
        let _ = writeln!(f, "set");
    }
    let result = Command::new("cmd.exe").arg("/c").arg(&bat_path).output();
    let _ = fs::remove_file(&bat_path);
    let out = result.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut env_map = std::collections::HashMap::new();
    let mut seen_sep = false;
    for line in text.lines() {
        if !seen_sep {
            if line.contains(sep) {
                seen_sep = true;
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            env_map.insert(k.to_string(), v.to_string());
        }
    }
    if env_map.is_empty() {
        None
    } else {
        Some(env_map)
    }
}
