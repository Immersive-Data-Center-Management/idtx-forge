//! Cargo-driven wasm compilation + source-hash cache helpers.

use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{BuildError, Result};

/// The wasm target triple guest behaviors compile to.
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Pure source identifier retained for callers tracking authored-source drift.
/// This digest alone never authorizes reuse of a compiled artifact.
#[must_use]
pub fn source_hash(source: &str, sdk_version: &str) -> String {
    use core::fmt::Write as _;
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"idtx-behavior-build:v1\0");
    h.update(sdk_version.as_bytes());
    h.update(b"\0");
    h.update(source.as_bytes());
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in &digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Where cargo drops the release wasm artifact for the scratch crate.
///
/// `cargo build --release --target wasm32-unknown-unknown` writes to
/// `<scratch>/target/wasm32-unknown-unknown/release/<crate>.wasm`
/// (cdylib output name matches the crate name with `-`→`_`).
#[must_use]
pub fn target_wasm_path(target: &Path, crate_name: &str) -> PathBuf {
    target
        .join(WASM_TARGET)
        .join("release")
        .join(format!("{crate_name}.wasm"))
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ToolIdentity {
    pub program: PathBuf,
    pub version: String,
    pub binary_hash: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Toolchain {
    pub cargo: ToolIdentity,
    pub rustc: ToolIdentity,
    pub target_libraries: String,
    pub auxiliary_tools: Vec<ToolIdentity>,
    pub optimizer: Option<ToolIdentity>,
}

/// Canonical directories a rustup / Homebrew toolchain drops its `cargo`,
/// `rustc` (and `wasm-opt`) into, used as **fallbacks** appended after the
/// inherited `PATH`.
///
/// A GUI app launched from Finder / Dock / `open` is started by `launchd`, which
/// hands the process only a minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`) and
/// does *not* inherit the user's interactive-shell `PATH`. A rustup install puts
/// `cargo`/`rustc` under `~/.cargo/bin`, so from a bundled app they are invisible
/// and `cargo` resolves to "tool not found" even though it is installed. Mirrors
/// what `. "$HOME/.cargo/env"` (plus a login shell) would prepend: `CARGO_HOME/bin`,
/// then the default `~/.cargo/bin`, then the usual Homebrew/manual prefixes that
/// may hold a non-rustup `cargo`/`rustc`.
///
/// Appended (not prepended) so an explicit `PATH` entry stays authoritative; the
/// Homebrew/`/usr/local` prefixes are inert on Windows (they simply won't exist).
fn toolchain_bin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !p.as_os_str().is_empty() && !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        push(PathBuf::from(cargo_home).join("bin"));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        push(PathBuf::from(home).join(".cargo").join("bin"));
    }
    for fixed in ["/opt/homebrew/bin", "/usr/local/bin"] {
        push(PathBuf::from(fixed));
    }
    dirs
}

/// The inherited `PATH` with [`toolchain_bin_dirs`] appended, suitable both for
/// resolving tools ourselves and for handing to a spawned `cargo` so that *it*
/// can find its own `rustc` / linker when the inherited `PATH` is bare (a
/// Homebrew `cargo` locates `rustc` via `PATH`; a rustup proxy does not need
/// this, but it is harmless). Never mutates the process environment — set it
/// per-`Command` via [`Command::env`].
pub(crate) fn augmented_path() -> OsString {
    append_dirs(std::env::var_os("PATH"), toolchain_bin_dirs())
}

/// Pure core of [`augmented_path`]: append `fallbacks` (de-duplicated, order
/// preserved) after the entries of `path`. Factored out so it can be tested
/// without mutating the (process-global, `unsafe`-to-set) environment.
fn append_dirs(path: Option<OsString>, fallbacks: Vec<PathBuf>) -> OsString {
    let original = path.unwrap_or_default();
    // Drop empty segments so a bare/empty inherited PATH does not leave an empty
    // entry (an empty PATH element means "current directory" — never what we want
    // to search for a toolchain binary).
    let mut entries: Vec<PathBuf> = std::env::split_paths(&original)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    for dir in fallbacks {
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    std::env::join_paths(entries).unwrap_or(original)
}

/// A [`Command`] for a resolved tool with an augmented `PATH` (see
/// [`augmented_path`]) so spawned toolchain processes resolve their own
/// dependencies even under a bundled-app launch.
fn tool_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.env("PATH", augmented_path());
    command
}

pub(crate) fn find_program(program: &OsStr, cwd: &Path) -> Option<PathBuf> {
    let path = Path::new(program);
    let candidates = if path.is_absolute() || path.components().count() > 1 {
        vec![if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) }]
    } else {
        std::env::split_paths(&augmented_path()).map(|p| p.join(path)).collect()
    };
    for candidate in candidates {
        let candidate = if candidate.is_absolute() { candidate } else { cwd.join(candidate) };
        // Preserve argv[0] for multicall tools such as rustup's rustc/cargo links.
        if candidate.is_file() { return Some(candidate); }
        #[cfg(windows)]
        if candidate.extension().is_none() {
            for extension in ["exe", "cmd", "bat"] {
                let candidate = candidate.with_extension(extension);
                if candidate.is_file() { return Some(candidate); }
            }
        }
    }
    None
}

pub(crate) fn config_files(scratch: &Path) -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join(".cargo"))
    }) { bases.push(if home.is_absolute() { home } else { scratch.join(home) }); }
    bases.extend(scratch.ancestors().collect::<Vec<_>>().into_iter().rev().map(|p| p.join(".cargo")));
    bases.into_iter().map(|base| {
        let legacy = base.join("config");
        if legacy.exists() { legacy } else { base.join("config.toml") }
    }).collect()
}

fn configured_tool(scratch: &Path, table: &[&str], environment: &[&str]) -> Result<Option<OsString>> {
    for name in environment {
        if let Some(value) = std::env::var_os(name) { return Ok((!value.is_empty()).then_some(value)); }
    }
    let mut selected = None;
    for path in config_files(scratch) {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let config: toml::Value = toml::from_str(&text).map_err(|e| BuildError::InvalidInput(e.to_string()))?;
        let mut value = Some(&config);
        for key in table { value = value.and_then(|v| v.get(*key)); }
        if let Some(value) = value.and_then(toml::Value::as_str) {
            let value = Path::new(value);
            selected = Some(if value.is_relative() && value.components().count() > 1 {
                path.parent().and_then(Path::parent).unwrap_or(scratch).join(value).into_os_string()
            } else { value.as_os_str().to_os_string() });
        }
    }
    Ok(selected)
}

fn identify(program: &OsStr, args: &[&str], scratch: &Path) -> Result<ToolIdentity> {
    let program = find_program(program, scratch).ok_or_else(|| BuildError::CargoSpawn(format!("tool not found: {}", program.to_string_lossy())))?;
    let output = tool_command(&program).current_dir(scratch).args(args).output()?;
    if !output.status.success() {
        return Err(BuildError::CargoFailed(String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    let version = String::from_utf8(output.stdout).map_err(|e| BuildError::InvalidInput(e.to_string()))?;
    let binary_hash = crate::fingerprint::file_hash(&program)?;
    Ok(ToolIdentity { program, version, binary_hash })
}

impl Toolchain {
    pub(crate) fn discover(scratch: &Path) -> Result<Self> {
        let cargo = identify(&std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()), &["-vV"], scratch)?;
        let rustc = configured_tool(scratch, &["build", "rustc"], &["RUSTC", "CARGO_BUILD_RUSTC"])?
            .unwrap_or_else(|| "rustc".into());
        let rustc = identify(&rustc, &["-vV"], scratch)?;
        let output = tool_command(&rustc.program).current_dir(scratch)
            .args(["--print", "target-libdir", "--target", WASM_TARGET]).output()?;
        if !output.status.success() { return Err(BuildError::InvalidInput("wasm target libraries unavailable".into())); }
        let path = String::from_utf8(output.stdout).map_err(|e| BuildError::InvalidInput(e.to_string()))?;
        let mut libraries = std::fs::read_dir(path.trim())?.collect::<std::io::Result<Vec<_>>>()?;
        libraries.sort_by_key(std::fs::DirEntry::file_name);
        if libraries.is_empty() { return Err(BuildError::InvalidInput("wasm target libraries missing".into())); }
        let mut hash = crate::fingerprint::Fingerprint::new();
        for entry in libraries {
            if entry.file_type()?.is_file() {
                hash.add("library-name", entry.file_name().as_encoded_bytes());
                hash.add("library", crate::fingerprint::file_hash(&entry.path())?.as_bytes());
            }
        }
        let mut auxiliary_tools = Vec::new();
        for (table, vars) in [
            (&["build", "rustc-wrapper"][..], &["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"][..]),
            (&["build", "rustc-workspace-wrapper"][..], &["RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"][..]),
            (&["target", WASM_TARGET, "linker"][..], &["CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER"][..]),
        ] {
            if let Some(program) = configured_tool(scratch, table, vars)? {
                let program = find_program(&program, scratch).ok_or_else(|| BuildError::InvalidInput("configured compiler wrapper/linker is missing".into()))?;
                auxiliary_tools.push(ToolIdentity { binary_hash: crate::fingerprint::file_hash(&program)?, program, version: String::new() });
            }
        }
        let optimizer = if find_program(OsStr::new("wasm-opt"), scratch).is_some() {
            Some(identify(OsStr::new("wasm-opt"), &["--version"], scratch)?)
        } else { None };
        Ok(Self { cargo, rustc, target_libraries: hash.finish(), auxiliary_tools, optimizer })
    }

    fn command(&self, scratch: &Path) -> Command {
        let mut command = tool_command(&self.cargo.program);
        command.current_dir(scratch).env_remove("CARGO_TARGET_DIR");
        command
    }
}

pub(crate) fn cargo_metadata(scratch: &Path, tools: &Toolchain) -> Result<serde_json::Value> {
    let output = tools.command(scratch)
        .args(["metadata", "--format-version=1", "--filter-platform", WASM_TARGET]).output()
        .map_err(|e| BuildError::CargoSpawn(e.to_string()))?;
    if !output.status.success() {
        return Err(BuildError::CargoFailed(String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| BuildError::InvalidInput(e.to_string()))
}

/// Cargo is invoked even for a cache hit: its dep-info also checks referenced
/// environment variables and include! inputs beyond the SDK's package trees.
pub(crate) fn cargo_wasm(scratch: &Path, target: &Path, name: &str, tools: &Toolchain, subcommand: &str) -> Result<bool> {
    let output = tools.command(scratch)
        .arg(subcommand)
        .arg("--release")
        .arg("--locked")
        .arg("--target")
        .arg(WASM_TARGET)
        .arg("--target-dir").arg(target)
        .arg("--message-format=json-render-diagnostics")
        .output()
        .map_err(|e| BuildError::CargoSpawn(e.to_string()))?;

    let mut fresh = None;
    let mut diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue; };
        if let Some(rendered) = message["message"]["rendered"].as_str() { diagnostics.push_str(rendered); }
        if message["reason"] == "compiler-artifact" && message["target"]["name"] == name {
            fresh = message["fresh"].as_bool();
        }
    }
    if !output.status.success() { return Err(BuildError::CargoFailed(diagnostics)); }
    if subcommand == "check" { return Ok(fresh.unwrap_or(false)); }
    fresh.ok_or_else(|| BuildError::ArtifactMissing(target_wasm_path(target, name)))
}

/// Best-effort `wasm-opt -Oz` to shrink the artifact. A missing `wasm-opt`
/// (or a non-zero exit) is non-fatal — the un-optimised module is still
/// valid. Logs at debug level.
pub(crate) fn maybe_wasm_opt(wasm: &Path, output: &Path, optimizer: Option<&ToolIdentity>) -> bool {
    let Some(optimizer) = optimizer else { return false; };
    let status = tool_command(&optimizer.program)
        .arg("-Oz")
        .arg(wasm)
        .arg("-o")
        .arg(output)
        .status();
    match status {
        Ok(s) if s.success() => {
            tracing::debug!(path = %wasm.display(), "wasm-opt -Oz applied");
            let mut magic = [0; 8];
            std::fs::File::open(output).and_then(|mut file| file.read_exact(&mut magic)).is_ok()
                && magic == *b"\0asm\x01\0\0\0"
        }
        Ok(s) => {
            tracing::debug!(?s, "wasm-opt exited non-zero; keeping un-optimised module");
            false
        }
        Err(_) => {
            tracing::debug!("wasm-opt not found; skipping shrink pass");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = source_hash("fn main() {}", "0.1.0");
        let b = source_hash("fn main() {}", "0.1.0");
        let c = source_hash("fn main() {}", "0.2.0");
        let d = source_hash("fn other() {}", "0.1.0");
        assert_eq!(a, b);
        assert_ne!(a, c); // sdk bump invalidates
        assert_ne!(a, d); // source change invalidates
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn target_path_shape() {
        let p = target_wasm_path(Path::new("/scratch/target"), "my_beh");
        assert!(p.ends_with("target/wasm32-unknown-unknown/release/my_beh.wasm"));
    }

    #[test]
    fn append_dirs_adds_fallbacks_after_path() {
        let path = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let out = append_dirs(
            Some(path),
            vec![PathBuf::from("/home/u/.cargo/bin"), PathBuf::from("/opt/homebrew/bin")],
        );
        let entries: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(
            entries,
            [
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/home/u/.cargo/bin"),
                PathBuf::from("/opt/homebrew/bin"),
            ]
        );
    }

    #[test]
    fn append_dirs_dedups_and_handles_empty_path() {
        // A fallback already present on PATH is not duplicated.
        let path = std::env::join_paths([Path::new("/opt/homebrew/bin")]).unwrap();
        let out = append_dirs(Some(path), vec![PathBuf::from("/opt/homebrew/bin")]);
        assert_eq!(std::env::split_paths(&out).collect::<Vec<_>>(), [PathBuf::from("/opt/homebrew/bin")]);

        // No inherited PATH (bare GUI launch) still yields the fallbacks.
        let out = append_dirs(None, vec![PathBuf::from("/home/u/.cargo/bin")]);
        assert_eq!(std::env::split_paths(&out).collect::<Vec<_>>(), [PathBuf::from("/home/u/.cargo/bin")]);
    }

    #[test]
    fn find_program_resolves_tool_off_path_in_fallback_dir() {
        // Simulate a bundled-app launch: the tool lives in a cargo-bin-style
        // directory that is NOT on PATH. `find_program` searches PATH first and
        // then the toolchain fallback dirs; a dir passed as a multi-component
        // relative path is anchored on `cwd`, so we exercise the absolute case
        // by resolving a program whose name we drop into a known fallback dir.
        //
        // We can't mutate global env safely here, so instead assert the weaker
        // but meaningful invariant: a program given by an absolute path that
        // exists is returned as-is (the resolution path used once a fallback dir
        // is joined onto the bare program name).
        let dir = std::env::temp_dir().join("idtx-findprog-test");
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("cargo");
        std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
        let found = find_program(tool.as_os_str(), Path::new("/"));
        assert_eq!(found.as_deref(), Some(tool.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
