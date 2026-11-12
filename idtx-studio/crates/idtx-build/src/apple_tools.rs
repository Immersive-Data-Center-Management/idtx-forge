//! Shared macOS/Apple command-line primitives: `install_name_tool` rpath +
//! install-name edits, `codesign` (ad-hoc and real-identity), and the thin
//! `xcrun` SDK-path / tool-locator wrappers.
//!
//! These primitives live here (rather than in `xtask`) because the launchable
//! visionOS `.app` assembly must run from **both** `xtask` (the
//! `run-visionos-device` command) and the Conductor build thread
//! (`idtx-studio-shell`), and both depend on `idtx-build`. Keeping the primitives
//! here lets the assembler ([`crate::visionos_assemble`]) be shared rather
//! than forked, and keeps `xtask` as a thin re-export.
//!
//! The pure predicates/arg logic elsewhere compile everywhere; the shell-outs in
//! this module only run on macOS (an Apple SDK is required), so each is gated
//! `#[cfg(target_os = "macos")]`. On other hosts the fns return
//! [`AppleToolError::NotMacOs`], so callers can degrade gracefully.

// `install_name_tool`, `codesign`, `xcrun`, `MTLDevice`, rpath etc. read fine
// unbackticked in the docs.
#![allow(clippy::doc_markdown)]

use std::path::Path;

/// Errors from the Apple command-line primitives.
#[derive(Debug, thiserror::Error)]
pub enum AppleToolError {
    /// The operation is macOS-only and this host is not macOS.
    #[error("`{0}` is only available on macOS")]
    NotMacOs(&'static str),
    /// A tool could not be spawned (not on `PATH`).
    #[error("failed to spawn `{tool}`: {source}")]
    Spawn {
        /// The tool that failed to launch.
        tool: String,
        /// The underlying spawn error.
        source: std::io::Error,
    },
    /// A tool ran but exited non-zero.
    #[error("`{tool}` failed ({status}): {stderr}")]
    CommandFailed {
        /// The tool that failed.
        tool: String,
        /// Its exit status.
        status: String,
        /// Captured stderr (trimmed).
        stderr: String,
    },
    /// `xcrun` returned empty stdout where a path was expected.
    #[error("`xcrun {args}` returned empty output")]
    EmptyOutput {
        /// The args that produced no output.
        args: String,
    },
}

/// Run a tool with `args`, mapping spawn/non-zero into [`AppleToolError`].
/// `tolerate` is given the trimmed stderr on a non-zero exit and may return
/// `true` to treat the failure as success (e.g. "rpath already present").
#[cfg(target_os = "macos")]
fn run(
    tool: &str,
    args: &[&std::ffi::OsStr],
    tolerate: impl Fn(&str) -> bool,
) -> Result<(), AppleToolError> {
    let out = std::process::Command::new(tool)
        .args(args)
        .output()
        .map_err(|source| AppleToolError::Spawn {
            tool: tool.to_owned(),
            source,
        })?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if tolerate(&stderr) {
        return Ok(());
    }
    Err(AppleToolError::CommandFailed {
        tool: tool.to_owned(),
        status: out.status.to_string(),
        stderr,
    })
}

/// Change a dylib's `LC_ID_DYLIB` install name (`install_name_tool -id`).
///
/// # Errors
/// [`AppleToolError`] if `install_name_tool` is missing or fails (or
/// [`AppleToolError::NotMacOs`] off macOS).
#[cfg(target_os = "macos")]
pub fn change_install_name(dylib: &Path, new_id: &str) -> Result<(), AppleToolError> {
    run(
        "install_name_tool",
        &["-id".as_ref(), new_id.as_ref(), dylib.as_os_str()],
        |_| false,
    )
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn change_install_name(_dylib: &Path, _new_id: &str) -> Result<(), AppleToolError> {
    Err(AppleToolError::NotMacOs("install_name_tool -id"))
}

/// Add an `LC_RPATH` to a binary (`install_name_tool -add_rpath`), tolerating an
/// already-present path.
///
/// # Errors
/// [`AppleToolError`] on a non-"duplicate" failure (or off macOS).
#[cfg(target_os = "macos")]
pub fn add_rpath(binary: &Path, rpath: &str) -> Result<(), AppleToolError> {
    run(
        "install_name_tool",
        &["-add_rpath".as_ref(), rpath.as_ref(), binary.as_os_str()],
        |stderr| stderr.contains("would duplicate path") || stderr.contains("already"),
    )
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn add_rpath(_binary: &Path, _rpath: &str) -> Result<(), AppleToolError> {
    Err(AppleToolError::NotMacOs("install_name_tool -add_rpath"))
}

/// Delete an `LC_RPATH` from a binary, tolerating its absence.
///
/// # Errors
/// [`AppleToolError`] on a non-"missing" failure (or off macOS).
#[cfg(target_os = "macos")]
pub fn delete_rpath(binary: &Path, rpath: &str) -> Result<(), AppleToolError> {
    run(
        "install_name_tool",
        &["-delete_rpath".as_ref(), rpath.as_ref(), binary.as_os_str()],
        |stderr| stderr.contains("no LC_RPATH"),
    )
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn delete_rpath(_binary: &Path, _rpath: &str) -> Result<(), AppleToolError> {
    Err(AppleToolError::NotMacOs("install_name_tool -delete_rpath"))
}

/// Ad-hoc sign `target` (`codesign --force [--deep] --sign -`). Required after
/// `install_name_tool` edits (which invalidate the Mach-O signature on Apple
/// silicon). Accepted by the Simulator; a physical device needs a real identity
/// via [`codesign_with_identity`].
///
/// # Errors
/// [`AppleToolError`] if `codesign` is missing or fails (or off macOS).
#[cfg(target_os = "macos")]
pub fn codesign_adhoc(target: &Path, deep: bool) -> Result<(), AppleToolError> {
    let mut args: Vec<&std::ffi::OsStr> = vec!["--force".as_ref()];
    if deep {
        args.push("--deep".as_ref());
    }
    args.push("--sign".as_ref());
    args.push("-".as_ref());
    args.push(target.as_os_str());
    run("codesign", &args, |_| false)
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn codesign_adhoc(_target: &Path, _deep: bool) -> Result<(), AppleToolError> {
    Err(AppleToolError::NotMacOs("codesign --sign -"))
}

/// Sign `target` with a real `identity` (`codesign --force --sign <identity>`),
/// optionally with an `--entitlements` plist. **No** `--deep` and **no**
/// `--options runtime` — a visionOS/iOS device bundle must not carry the macOS
/// hardened-runtime flag, and inner Mach-O objects are signed individually first
/// by the caller. `identity` of `"-"` is an ad-hoc signature.
///
/// # Errors
/// [`AppleToolError`] if `codesign` is missing or fails (or off macOS).
#[cfg(target_os = "macos")]
pub fn codesign_with_identity(
    target: &Path,
    identity: &str,
    entitlements: Option<&Path>,
) -> Result<(), AppleToolError> {
    let mut args: Vec<&std::ffi::OsStr> = vec!["--force".as_ref()];
    if let Some(ents) = entitlements {
        args.push("--entitlements".as_ref());
        args.push(ents.as_os_str());
    }
    args.push("--sign".as_ref());
    args.push(identity.as_ref());
    args.push(target.as_os_str());
    run("codesign", &args, |_| false)
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn codesign_with_identity(
    _target: &Path,
    _identity: &str,
    _entitlements: Option<&Path>,
) -> Result<(), AppleToolError> {
    Err(AppleToolError::NotMacOs("codesign --sign <identity>"))
}

/// `xcrun --sdk <sdk> --show-sdk-path` → the SDK root path.
///
/// # Errors
/// [`AppleToolError`] if `xcrun` is missing/fails or returns no path.
#[cfg(target_os = "macos")]
pub fn xcrun_show_sdk_path(sdk: &str) -> Result<String, AppleToolError> {
    xcrun_capture(&["--sdk", sdk, "--show-sdk-path"])
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn xcrun_show_sdk_path(_sdk: &str) -> Result<String, AppleToolError> {
    Err(AppleToolError::NotMacOs("xcrun --show-sdk-path"))
}

/// `xcrun --sdk <sdk> -f <tool>` → the absolute path of an SDK tool.
///
/// # Errors
/// [`AppleToolError`] if `xcrun` is missing/fails or returns no path.
#[cfg(target_os = "macos")]
pub fn xcrun_find_tool(sdk: &str, tool: &str) -> Result<String, AppleToolError> {
    xcrun_capture(&["--sdk", sdk, "-f", tool])
}

/// Off-macOS stub.
#[cfg(not(target_os = "macos"))]
pub fn xcrun_find_tool(_sdk: &str, _tool: &str) -> Result<String, AppleToolError> {
    Err(AppleToolError::NotMacOs("xcrun -f <tool>"))
}

/// Run `xcrun <args…>`, returning trimmed stdout; error on failure/empty.
#[cfg(target_os = "macos")]
fn xcrun_capture(args: &[&str]) -> Result<String, AppleToolError> {
    let out = std::process::Command::new("xcrun")
        .args(args)
        .output()
        .map_err(|source| AppleToolError::Spawn {
            tool: "xcrun".to_owned(),
            source,
        })?;
    if !out.status.success() {
        return Err(AppleToolError::CommandFailed {
            tool: "xcrun".to_owned(),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        return Err(AppleToolError::EmptyOutput {
            args: args.join(" "),
        });
    }
    Ok(s)
}
