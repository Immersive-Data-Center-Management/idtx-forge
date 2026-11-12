//! Out-of-process runtime-preview launch: spawns the `idtx-runtime` player for
//! the active stage and manages the child process + any temp stage file.
//!
//! "Preview in Runtime" runs the stage exactly as a packaged experience would —
//! in the separate `idtx-runtime` binary, in `OpenXR` mode — rather than inside
//! the editor. The editor keeps no XR code; it only spawns and tracks the child.
//!
//! Stage handoff: rather than the raw `world.usda`, the editor composes an
//! *assembly-root layer* (see [`crate::preview_assembly`]) that sublayers the
//! world open in the editor plus, in XR mode, a generated controller-defaults
//! layer. When the tab has unsaved edits the world is flattened to a temp beside
//! the source first (so relative references resolve). All generated temps are
//! deleted once the preview process exits (or the editor drops the process).

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use idtx_runtime_manifest::RuntimeManifest;
use idtx_studio_session::StudioSession;

use crate::preview_assembly::{self, PreviewMode, PreviewTemp};

/// A running runtime-preview player process, plus the temp files (if any)
/// to clean up when it exits.
#[derive(Debug)]
pub struct PreviewProcess {
    child: Child,
    _temp: Option<PreviewTemp>,
    // Detached reader threads; they exit when the child's pipes close.
    _io_threads: Vec<thread::JoinHandle<()>>,
    finished: bool,
    /// The child's exit status, captured once it self-exits (see [`Self::poll`]).
    exit_status: Option<ExitStatus>,
    /// Set true by the reader threads the first time the child writes any line to
    /// stdout/stderr. A child that exits having written *nothing* almost always
    /// died at process load (before `main`/`init_tracing` — e.g. a missing DLL),
    /// which is otherwise invisible; we use this to explain the silent exit.
    produced_output: Arc<AtomicBool>,
}

impl PreviewProcess {
    /// Poll the child once; returns `true` while it is still running. Call once
    /// per frame so [`Self::is_running`] reflects a self-exited process.
    pub fn poll(&mut self) -> bool {
        if !self.finished {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.finished = true;
                    self.exit_status = Some(status);
                }
                Err(_) => self.finished = true,
                Ok(None) => {}
            }
        }
        !self.finished
    }

    /// Cached running state (see [`Self::poll`]). Cheap, `&self`.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !self.finished
    }

    /// A human-readable outcome once the child has exited (`None` while running).
    /// Distinguishes a normal exit from a failure, and — critically — flags the
    /// Windows load-time failure codes that produce no console output at all, so
    /// an immediate silent "preview ended" carries an actionable diagnosis.
    #[must_use]
    pub fn exit_report(&self) -> Option<String> {
        let status = self.exit_status?;
        let produced_output = self.produced_output.load(Ordering::Relaxed);
        Some(describe_exit(status, produced_output))
    }

    /// Kill the child and wait for it to exit. The temp file is removed when
    /// this `PreviewProcess` is dropped.
    pub fn stop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.finished = true;
        }
    }
}

impl Drop for PreviewProcess {
    fn drop(&mut self) {
        // Don't orphan a still-running preview when the editor exits.
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // `_temp` drops here, deleting the flattened stage.
    }
}

/// Locate the `idtx-runtime` binary next to the running editor executable.
fn runtime_binary_path() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "editor exe has no parent dir")
    })?;
    let name = if cfg!(windows) {
        "idtx-runtime.exe"
    } else {
        "idtx-runtime"
    };
    Ok(dir.join(name))
}

/// Spawn `idtx-runtime --stage <assembly> --mode <mode>` for `session`.
///
/// The `<assembly>` is the composed assembly-root layer (see
/// [`crate::preview_assembly::build_assembly`]), not the raw world stage.
///
/// `controller_device` selects the XR controller mesh and, in XR mode, is also
/// forwarded to the spawned runtime as `--controller-device <id>` so the runtime
/// injects the same device meshes over its bare rig even when its own `OpenXR`
/// probe (a second process over Quest Link) would degrade to `"generic"`. Pass
/// the result of an `XrLoader::detect_controller_kind()` probe (as a manifest
/// device id), or `None` to use the `"generic"` fallback / local probe.
///
/// `player`, when present with at least one level, drives the project-preview
/// composition (level(s) + mode-selected player overlay); otherwise the legacy
/// open-tab world is wrapped. See [`crate::preview_assembly::build_assembly`].
///
/// `asset_search_dirs` are the open project's asset roots (absolute); each is
/// forwarded as `--asset-search-dir` so the spawned runtime installs the same
/// USD default-resolver search path the editor uses, letting a level's own
/// project-relative references (`@assets/foo.usdz@`) resolve out-of-process.
///
/// `network`, when `Some`, forwards `--allow-network` and/or one
/// `--allow-host <pattern>` per entry to the spawned runtime so the preview
/// enforces the same datasource allow-list the project author configured.
/// When `None` (no project open, or project is deny-all), nothing beyond the
/// baseline deny is emitted.  In both cases `--replace-network-policy` is
/// **always** emitted so the CLI values *replace* any `[network]` section in a
/// `run.toml` discovered next to the runtime binary.  This prevents a stale
/// editor-adjacent `run.toml` from silently granting network access in a
/// deny-all preview.  For packaged experiences (no editor spawn), no
/// `--replace-network-policy` flag is passed and `run.toml` remains
/// authoritative.
///
/// stdout and stderr of the child are forwarded to the conductor's own stderr
/// with a `[runtime] ` prefix so both streams appear together in the console.
///
/// # Errors
///
/// Fails when the runtime binary cannot be located, the assembly cannot be
/// composed (e.g. a dirty stage cannot be flattened), or the process cannot be
/// spawned.
#[cfg(feature = "authoring")]
pub fn spawn_preview(
    session: &StudioSession,
    dirty: bool,
    mode: PreviewMode,
    manifest: &RuntimeManifest,
    controller_device: Option<&str>,
    player: Option<&preview_assembly::PlayerComposition>,
    behavior_ctx: Option<&preview_assembly::BehaviorPrepareCtx>,
    asset_search_dirs: &[PathBuf],
    network: Option<&idtx_runtime_manifest::NetworkConfig>,
) -> std::io::Result<PreviewProcess> {
    let binary = runtime_binary_path()?;
    let (stage, temp) = preview_assembly::build_assembly(
        session,
        dirty,
        mode,
        manifest,
        controller_device,
        player,
        behavior_ctx,
    )?;

    let mut cmd = std::process::Command::new(&binary);
    // the preview runtime always spawns with the Debug-LogLevel
    //cmd.env("RUST_LOG", "Debug");
    cmd.arg("--stage")
        .arg(&stage)
        .arg("--mode")
        .arg(mode.as_arg());
    // Editor preview is always windowed (packaged/cooked desktop builds run
    // fullscreen + capture the cursor via run.toml). Pass `--windowed` explicitly
    // so a stray `run.toml` discovered next to the runtime binary can never flip a
    // preview to fullscreen.
    cmd.arg("--windowed");
    // Anchor the runtime's USD resolver at the project's asset roots (verbatim
    // `\\?\` prefix stripped so the paths anchor cleanly) so project-relative
    // references in the composed level(s) resolve exactly as they do in-editor.
    for dir in asset_search_dirs {
        cmd.arg("--asset-search-dir")
            .arg(idtx_project::strip_verbatim_prefix(dir));
    }
    // Forward the project's network capability so the preview runtime enforces
    // the same datasource allow-list the author configured.  `--replace-network-policy`
    // is emitted unconditionally so the CLI is the sole source of truth for
    // network policy in editor-spawned previews — a stale run.toml beside the
    // runtime binary (from a prior packaged build or a different project) can
    // never silently grant network access when the current project is deny-all.
    cmd.arg("--replace-network-policy");
    if let Some(net) = network {
        if net.allow {
            cmd.arg("--allow-network");
        }
        for host in &net.allowed_hosts {
            cmd.arg("--allow-host").arg(host);
        }
        for host in &net.private_hosts {
            cmd.arg("--private-host").arg(host);
        }
        if net.max_asset_mb != 0 {
            cmd.arg("--max-asset-mb").arg(net.max_asset_mb.to_string());
        }
        for origin in &net.auth_origins {
            cmd.arg("--auth-origin").arg(origin);
        }
        // Forward the configured body-size cap so the preview enforces the same
        // limit the author set in Project Settings. `0` is left implicit (the
        // runtime default of 1 MiB applies).
        if net.max_response_mb > 0 {
            cmd.arg("--max-response-mb")
                .arg(net.max_response_mb.to_string());
        }
    }
    // Pass the editor-probed device through so the runtime injects the right
    // controller meshes even when its own probe would degrade to "generic" (the
    // packaged runtime, launched without this flag, still probes for itself).
    if mode == PreviewMode::Xr {
        if let Some(device) = controller_device {
            cmd.arg("--controller-device").arg(device);
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let mut io_threads: Vec<thread::JoinHandle<()>> = Vec::with_capacity(2);
    let produced_output = Arc::new(AtomicBool::new(false));

    if let Some(stdout) = child.stdout.take() {
        let flag = Arc::clone(&produced_output);
        io_threads.push(thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                flag.store(true, Ordering::Relaxed);
                eprintln!("[runtime] {line}");
            }
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let flag = Arc::clone(&produced_output);
        io_threads.push(thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                flag.store(true, Ordering::Relaxed);
                eprintln!("[runtime] {line}");
            }
        }));
    }

    Ok(PreviewProcess {
        child,
        _temp: Some(temp),
        _io_threads: io_threads,
        finished: false,
        exit_status: None,
        produced_output,
    })
}

/// Format a child exit status for the console / status bar. Normal exits are
/// terse; failures include the raw code, and the Windows *loader* failure codes
/// (which kill the process before `main` runs and so leave no `[runtime]` output
/// whatsoever) get a targeted, actionable hint — this is the "further notice"
/// that an otherwise-silent immediate preview exit was missing.
fn describe_exit(status: ExitStatus, produced_output: bool) -> String {
    let Some(code) = status.code() else {
        return "runtime terminated without an exit code (signal/abort)".to_owned();
    };
    if code == 0 {
        return "runtime exited normally".to_owned();
    }

    // On Windows a process that fails to *load* exits with an NTSTATUS code;
    // `ExitStatus::code()` surfaces it as the raw 32-bit value (seen as i32).
    let raw = code as u32;
    let loader_hint = match raw {
        0xC000_0135 => Some(
            "a required DLL was not found next to idtx-runtime.exe. A `pcvr` \
             runtime build needs its OpenXR/D3D12 DLLs (and the OpenUSD runtime \
             DLLs + usd plugin dir) co-located with the binary the editor spawns",
        ),
        0xC000_0139 => Some(
            "a DLL next to idtx-runtime.exe is the wrong version (entry point \
             not found); rebuild the runtime against the same OpenUSD as the editor",
        ),
        0xC000_007B => {
            Some("a DLL next to idtx-runtime.exe has the wrong architecture (32/64-bit mismatch)")
        }
        _ => None,
    };

    match loader_hint {
        Some(hint) => format!(
            "runtime failed to start (exit 0x{raw:08X}): {hint}. It produced no \
             output because it died before startup."
        ),
        None if code != 0 && !produced_output => format!(
            "runtime exited with code {code} (0x{raw:08X}) before producing any \
             output — it likely failed to load before startup (check the DLLs \
             next to idtx-runtime.exe)."
        ),
        None => format!("runtime exited with code {code} (0x{raw:08X})"),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::describe_exit;
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn status(code: u32) -> ExitStatus {
        ExitStatus::from_raw(code)
    }

    #[test]
    fn normal_exit_is_plain() {
        let report = describe_exit(status(0), true);
        assert_eq!(report, "runtime exited normally");
    }

    #[test]
    fn missing_dll_is_flagged_as_load_failure() {
        // STATUS_DLL_NOT_FOUND — the process dies before `main`, no output.
        let report = describe_exit(status(0xC000_0135), false);
        assert!(report.contains("failed to start"), "got: {report}");
        assert!(report.contains("0xC0000135"), "got: {report}");
        assert!(report.contains("DLL"), "got: {report}");
    }

    #[test]
    fn entrypoint_not_found_is_flagged() {
        // STATUS_ENTRYPOINT_NOT_FOUND — DLL version mismatch.
        let report = describe_exit(status(0xC000_0139), false);
        assert!(report.contains("wrong version"), "got: {report}");
        assert!(report.contains("0xC0000139"), "got: {report}");
    }

    #[test]
    fn silent_nonzero_exit_suggests_load_failure() {
        // An unclassified non-zero code with no output still points at loading.
        let report = describe_exit(status(1), false);
        assert!(report.contains("before producing any"), "got: {report}");
    }

    #[test]
    fn nonzero_exit_with_output_is_terse() {
        // The runtime ran and reported its own error; don't second-guess it.
        let report = describe_exit(status(1), true);
        assert!(report.contains("exited with code 1"), "got: {report}");
        assert!(!report.contains("failed to load"), "got: {report}");
    }
}
