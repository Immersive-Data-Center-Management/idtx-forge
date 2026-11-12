//! Off-thread cook + package for the Conductor "Build" menu.
//!
//! The heavy cook (`idtx_build::cook`: flatten + localize the stage, compile +
//! validate behaviors, stamp `run.toml`) runs on a worker thread so the egui UI
//! stays responsive. The shell polls the [`BuildJob`] once per frame and surfaces
//! the resulting payload / bundle path (or error) in the status bar.
//!
//! Packaging into a launchable bundle needs a built `idtx-runtime` binary for the
//! target. We locate one next to the running editor (a sibling `idtx-runtime`);
//! when none is present the job stops after the cook and reports the payload dir,
//! rather than fabricating a broken bundle. Cross-compiling the runtime for a
//! *foreign* target is host-toolchain-bound and remains a `cargo xtask` / CI
//! concern — the packager takes the binary as an input.
//!
//! **Quest shipping model.** The Quest (`aarch64-linux-android`) build consumes a
//! *pre-cross-compiled* `libidtx_runtime.so` staged beside the editor at
//! `android-runtime/arm64-v8a/` by `cargo xtask build-quest-runtime` — the cook
//! **never** cross-compiles. The shipped `idtx-conductor` bundles its
//! cross-compiled runtimes (mac/windows/android) and carries no runtime source,
//! so it packages the Android target without a cross-build; a dev build must run
//! the xtask first. When the `.so` is absent the Quest cook fails closed (see
//! [`quest_runtime_binary`]) rather than packaging the host `.exe`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError};
#[cfg(feature = "cooking")]
use std::sync::Arc;

use idtx_build::{
    AndroidApkPackager, AndroidSdk, ApkAssembler, ApkError, BundleKind, CookPlan, CookTarget,
    DesktopFolderPackager, DeviceAppPlan, MacAppInfo, MacAppPackager, Packager, RuntimeBinary,
    VisionOsAppPackager, bundle_kind_for, cook, discover_development_identity,
    discover_profile_for, finish_device_app, install_app, launch_app, list_paired_devices,
    read_profile, select_visionos_device,
};
use idtx_editor_model::BuildTarget;
use idtx_project::IdtxProject;

/// Outcome of a finished background build.
pub(crate) enum BuildOutcome {
    /// Cooked, and (when a runtime binary was found) packaged.
    Done {
        /// The cooked payload directory (`stage.usdz` + `behaviors/` + `run.toml`).
        payload: PathBuf,
        /// The launchable bundle, when a runtime binary was located to package.
        bundle: Option<PathBuf>,
        /// A human-facing note about a *successful* build that nonetheless did
        /// less than the user may expect — e.g. a Vision Pro build that staged
        /// an unsigned `.app` because no provisioning profile matched, or a
        /// "Build & deploy" whose on-device install was skipped. Surfaced in the
        /// status bar so the GUI user (who sees no `tracing` output) learns why.
        note: Option<String>,
    },
    /// Failed with a human-facing message.
    Failed(String),
}

/// A running background build, owned by the shell and polled each frame.
pub(crate) struct BuildJob {
    rx: Receiver<BuildOutcome>,
    target: BuildTarget,
}

impl BuildJob {
    /// The target this job is building for (for the status message).
    pub(crate) fn target(&self) -> BuildTarget {
        self.target
    }

    /// Non-blocking poll. Returns `Some` exactly once, when the worker finishes
    /// (or the thread died). `None` means still running.
    pub(crate) fn poll(&self) -> Option<BuildOutcome> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(BuildOutcome::Failed(
                "build thread ended unexpectedly".to_owned(),
            )),
        }
    }
}

/// Spawn a background cook + package for `project` targeting `target`.
///
/// Output lands under `<project-root>/build/`: the payload in
/// `build/<label>/payload/` and any bundle in `build/<label>/`.
pub(crate) fn spawn(project: IdtxProject, target: BuildTarget, deploy: bool) -> BuildJob {
    let (tx, rx) = std::sync::mpsc::channel();
    // Move owned data into the worker; IdtxProject shares its retained directory
    // handle and owns the parsed manifest, so it is safe to send.
    std::thread::spawn(move || {
        let outcome = run_build(&project, target, deploy);
        // The receiver may already be gone if the app is shutting down; ignore.
        let _ = tx.send(outcome);
    });
    BuildJob { rx, target }
}

/// The cook target for `target`.
///
/// Desktop/PCVR build for the *host* (the editor packages for the host it runs
/// on), so they take the host triple. Quest/VisionPro are **foreign** standalone
/// triples — fixed, not host-derived — so the editor cooks the portable payload
/// and defers packaging to `xtask`/CI (see [`sibling_runtime_binary`], which
/// never finds a foreign runtime beside the editor). The triple is stamped into
/// the manifest + reproducibility hash and selects the packager.
fn host_cook_target(target: BuildTarget) -> CookTarget {
    match target {
        BuildTarget::Desktop => CookTarget::desktop(host_triple()),
        BuildTarget::Pcvr => CookTarget::pcvr(host_triple()),
        BuildTarget::Quest => CookTarget::quest("aarch64-linux-android"),
        BuildTarget::VisionPro => CookTarget::vision_pro("aarch64-apple-visionos"),
    }
}

/// The first concrete credential origin, independent of network reachability.
fn dev_token_host(project: &IdtxProject) -> Option<String> {
    project
        .manifest()
        .network
        .auth_origins
        .iter()
        .find(|h| !h.contains('*'))
        .cloned()
}

/// ⚠️ TEMPORARY (see docs/plan_remote_refs_external_and_runtime_auth.md): resolve
/// the logged-in bearer token to embed into the cooked `run.toml` for a device
/// build. Keyed by the first concrete `[network] allowed_hosts` entry. Requires
/// the `remote` feature (the auth registry); errors with actionable guidance
/// otherwise or when no token / host is available.
#[cfg(feature = "remote")]
fn resolve_dev_token(project: &IdtxProject) -> Result<String, String> {
    let host = dev_token_host(project).ok_or(
        "list a concrete origin in [network] auth_origins so a token can be resolved for it",
    )?;
    let registry = idtx_remote::AuthRegistry::with_studio_config();
    match registry.token_for(&host).map_err(|e| e.to_string())? {
        Some(token) => Ok(token),
        None => Err(format!(
            "no bearer token for host {host}; log in via Configure Server… first"
        )),
    }
}

/// Fallback when the shell is built without `remote`: embedding a token needs the
/// auth registry, which is not compiled in, so the opt-in cannot be honored.
#[cfg(not(feature = "remote"))]
fn resolve_dev_token(_project: &IdtxProject) -> Result<String, String> {
    Err("[build] embed_dev_token requires the `remote` feature (auth support) \
         to resolve a token; rebuild the editor with it or clear the option"
        .to_owned())
}

/// Best-effort host target triple from compile-time `cfg`. Not authoritative
/// (does not distinguish e.g. gnu vs musl), but correct for packager selection
/// and honest enough for the manifest stamp.
fn host_triple() -> String {
    let arch = std::env::consts::ARCH; // "x86_64", "aarch64", …
    let os = if cfg!(target_os = "windows") {
        "pc-windows-msvc"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        std::env::consts::OS
    };
    format!("{arch}-{os}")
}

/// Locate a built `idtx-runtime` binary next to the running editor, so a bundle
/// can be assembled in place. Returns `None` when absent (cook-only).
///
/// This is the **desktop/PCVR** host binary (`idtx-runtime[.exe]`). The foreign
/// standalone targets do not use it: Quest has its own [`quest_runtime_binary`]
/// and Vision Pro its own [`visionos_runtime_binary`] (the cross-compiled
/// `.so`/`.dylib`).
fn sibling_runtime_binary() -> Option<RuntimeBinary> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = format!("idtx-runtime{}", std::env::consts::EXE_SUFFIX);
    let candidate = dir.join(&name);
    candidate.is_file().then(|| RuntimeBinary::new(candidate))
}

/// The stem of the HTTP-resolver USD plugin dylib the runtime hard-links under
/// the `remote` feature (`@rpath/libidtx_http_resolver.dylib`).
const HTTP_RESOLVER_DYLIB: &str = "libidtx_http_resolver.dylib";
/// Bundle subdirectory the resolver plugin is staged into (mirrors the editor's
/// `cargo xtask bundle-mac --remote` layout + the runtime's own plugin probing).
const HTTP_RESOLVER_SUBDIR: &str = "idtx-http-resolver";

/// File name of the HTTP-resolver USD plugin library the runtime hard-links
/// under the `remote` feature on the **desktop folder** targets: an import on
/// `idtx_http_resolver.dll` (Windows) / a `DT_NEEDED libidtx_http_resolver.so`
/// (Linux). macOS uses `HTTP_RESOLVER_DYLIB` and the `.app` rpath layout instead
/// (and never reaches the `DesktopFolder` packager — `bundle_kind_for` maps
/// apple/darwin to `MacApp`), so the editor only ever packages this library for
/// the host it runs on.
#[cfg(windows)]
const HTTP_RESOLVER_DESKTOP_LIB: &str = "idtx_http_resolver.dll";
#[cfg(not(windows))]
const HTTP_RESOLVER_DESKTOP_LIB: &str = "libidtx_http_resolver.so";

/// Bundle the HTTP resolver plugin into a cooked macOS `.app` when the runtime
/// binary links it, so the app launches.
///
/// The macOS packager stages the runtime relying on its baked absolute openusd
/// rpath to resolve the USD/`MaterialX` dylibs. A runtime built with the `remote`
/// (`usd-resolver-http`) feature *also* carries a hard `LC_LOAD_DYLIB` on
/// `@rpath/libidtx_http_resolver.dylib` — which is **not** on that openusd rpath
/// (it is built into the resolver crate's `OUT_DIR`). dyld resolves that load at
/// launch, so without the plugin bundled the app dies before `main` with
/// "Library not loaded: @rpath/libidtx_http_resolver.dylib". This stages the
/// dylib (+ its `plugInfo.json`) into `Contents/Frameworks/idtx-http-resolver/`
/// and adds a bundle-relative rpath to the binary, mirroring the editor's own
/// `bundle-mac --remote` path.
///
/// A no-op when the runtime does not link the resolver (built without `remote`):
/// detected by scanning the Mach-O bytes for the dylib's NUL-terminated load
/// name, the same cheap, dependency-free check `idtx_build` uses for the `OpenExec`
/// plugin. Fails **closed** (like the Quest/visionOS runtime lookups) when the
/// binary needs the plugin but it cannot be located — a silent skip would just
/// reproduce the launch crash.
fn stage_macos_http_resolver(
    app: &Path,
    exe_name: &str,
    runtime_exe_dir: &Path,
) -> Result<(), idtx_build::CookError> {
    use idtx_build::CookError;

    let binary = app.join("Contents").join("MacOS").join(exe_name);
    let bytes = std::fs::read(&binary).map_err(|source| CookError::Io {
        path: binary.clone(),
        source,
    })?;
    let needle = HTTP_RESOLVER_DYLIB.as_bytes();
    if !bytes.windows(needle.len()).any(|w| w == needle) {
        // Runtime built without `remote` → no resolver load to satisfy.
        return Ok(());
    }

    let src_dir = locate_resolver_plugin_dir(runtime_exe_dir).ok_or_else(|| {
        CookError::HttpResolverStaging(format!(
            "runtime links {HTTP_RESOLVER_DYLIB} but it was not found beside the editor \
             (looked under {}). The editor must be built with `--features remote` (which \
             builds the resolver), or shipped via `cargo xtask bundle-mac --remote`.",
            runtime_exe_dir.display()
        ))
    })?;

    let plugin_dir = app
        .join("Contents")
        .join("Frameworks")
        .join(HTTP_RESOLVER_SUBDIR);
    std::fs::create_dir_all(&plugin_dir).map_err(|source| CookError::Io {
        path: plugin_dir.clone(),
        source,
    })?;

    // Copy the dylib (required) + its plugInfo.json (for PlugRegistry discovery;
    // present beside the dylib in both the dev OUT_DIR and the shipped layout).
    let src_dylib = src_dir.join(HTTP_RESOLVER_DYLIB);
    std::fs::copy(&src_dylib, plugin_dir.join(HTTP_RESOLVER_DYLIB)).map_err(|source| {
        CookError::Io {
            path: src_dylib.clone(),
            source,
        }
    })?;
    let src_pluginfo = src_dir.join("plugInfo.json");
    if src_pluginfo.is_file() {
        std::fs::copy(&src_pluginfo, plugin_dir.join("plugInfo.json")).map_err(|source| {
            CookError::Io {
                path: src_pluginfo.clone(),
                source,
            }
        })?;
    }

    // Add the bundle-relative rpath so dyld finds the staged dylib. Idempotent:
    // skip if the binary already carries it (a re-cook over an existing app).
    // `install_name_tool` re-applies an ad-hoc "linker-signed" signature to the
    // Mach-O on Apple silicon, which is what keeps the mutated binary launchable
    // — no explicit `codesign` pass is needed for an ad-hoc cooked bundle.
    let rpath = format!("@executable_path/../Frameworks/{HTTP_RESOLVER_SUBDIR}");
    if !bytes
        .windows(rpath.len())
        .any(|w| w == rpath.as_bytes())
    {
        let status = Command::new("install_name_tool")
            .arg("-add_rpath")
            .arg(&rpath)
            .arg(&binary)
            .status()
            .map_err(|e| {
                CookError::HttpResolverStaging(format!("running install_name_tool: {e}"))
            })?;
        if !status.success() {
            return Err(CookError::HttpResolverStaging(format!(
                "install_name_tool -add_rpath {rpath} {} failed ({status})",
                binary.display()
            )));
        }
    }

    tracing::info!(
        plugin = %plugin_dir.display(),
        "staged HTTP resolver plugin into cooked macOS .app (runtime links it via @rpath)"
    );
    Ok(())
}

/// Locate the directory holding `libidtx_http_resolver.dylib` beside the editor.
/// Checks, in order: the shipped-Conductor layout
/// (`<exe_dir>/../Frameworks/idtx-http-resolver/`, staged by `bundle-mac
/// --remote`), flat beside the runtime, then the dev cargo layout
/// (`<exe_dir>/build/idtx-usd-resolver-http-*/out/`, newest match).
fn locate_resolver_plugin_dir(runtime_exe_dir: &Path) -> Option<PathBuf> {
    let has_dylib = |dir: &Path| dir.join(HTTP_RESOLVER_DYLIB).is_file();

    let shipped = runtime_exe_dir
        .join("..")
        .join("Frameworks")
        .join(HTTP_RESOLVER_SUBDIR);
    if has_dylib(&shipped) {
        return Some(shipped);
    }
    if has_dylib(runtime_exe_dir) {
        return Some(runtime_exe_dir.to_path_buf());
    }

    // Dev build: scan `<exe_dir>/build/idtx-usd-resolver-http-*/out/`, newest first.
    let build_dir = runtime_exe_dir.join("build");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&build_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("idtx-usd-resolver-http-")
        })
        .map(|e| e.path().join("out"))
        .filter(|out| has_dylib(out))
        .filter_map(|out| {
            let mtime = std::fs::metadata(out.join(HTTP_RESOLVER_DYLIB))
                .and_then(|m| m.modified())
                .ok()?;
            Some((mtime, out))
        })
        .collect();
    candidates.sort_by_key(|(mtime, _)| *mtime);
    candidates.pop().map(|(_, out)| out)
}

/// Bundle the HTTP resolver plugin into a cooked Windows/Linux desktop folder
/// when the runtime binary links it, so the packaged player launches.
///
/// The desktop packager stages the runtime relying on the OS loader to resolve
/// its dynamic dependencies from the bundle root (there is no rpath on Windows).
/// A runtime built with the `remote` (`usd-resolver-http`) feature carries a
/// load-time import on `idtx_http_resolver.dll` (Windows) / a `DT_NEEDED
/// libidtx_http_resolver.so` (Linux), which the loader resolves **before**
/// `main` — so without the library beside the exe the player dies at launch
/// (`idtx_http_resolver.dll` "was not found", a `0xc0000139` on Windows). This
/// stages the library flat into the bundle root (loader-visible beside the exe)
/// and `plugInfo.json` into `<bundle>/idtx-http-resolver/` with its `LibraryPath`
/// **emptied**, so `PlugRegistry` registers the http/https URI schemes against
/// the already-load-resident resolver WITHOUT a second `LoadLibrary` (a second
/// copy at a different path would split the resolver's process-global fetch
/// state and re-register its `TfType`). Mirrors the shipped-Conductor `cargo
/// xtask bundle-win --remote` layout and the runtime's own `resolve_plugin_dir`
/// probing — the Windows/Linux twin of [`stage_macos_http_resolver`].
///
/// A no-op when the runtime does not link the resolver (built without `remote`):
/// detected by scanning the staged binary's bytes for the library's
/// NUL-terminated load name, the same cheap, dependency-free check the macOS
/// path uses. Fails **closed** (like the Quest/visionOS runtime lookups) when
/// the binary needs the plugin but it cannot be located beside the editor — a
/// silent skip would just reproduce the launch crash.
fn stage_desktop_http_resolver(
    bundle: &Path,
    runtime_bin_name: &str,
    runtime_exe_dir: &Path,
) -> Result<(), idtx_build::CookError> {
    use idtx_build::CookError;

    let binary = bundle.join(runtime_bin_name);
    let bytes = std::fs::read(&binary).map_err(|source| CookError::Io {
        path: binary.clone(),
        source,
    })?;
    let needle = HTTP_RESOLVER_DESKTOP_LIB.as_bytes();
    if !bytes.windows(needle.len()).any(|w| w == needle) {
        // Runtime built without `remote` → no resolver import to satisfy.
        return Ok(());
    }

    let (src_lib, src_pluginfo) = locate_desktop_resolver(runtime_exe_dir).ok_or_else(|| {
        CookError::HttpResolverStaging(format!(
            "runtime links {HTTP_RESOLVER_DESKTOP_LIB} but it was not found beside the editor \
             (looked under {}). The editor must be built with `--features remote` (which \
             builds the resolver), or shipped via `cargo xtask bundle-win --remote`.",
            runtime_exe_dir.display()
        ))
    })?;

    // 1. Library → bundle root, beside the exe, so the loader resolves the
    //    runtime's load-time import.
    std::fs::copy(&src_lib, bundle.join(HTTP_RESOLVER_DESKTOP_LIB)).map_err(|source| {
        CookError::Io {
            path: src_lib.clone(),
            source,
        }
    })?;

    // 2. plugInfo.json → idtx-http-resolver/ with LibraryPath emptied (the lib is
    //    already load-resident from step 1; see the fn doc). No library copy here.
    let plugin_dir = bundle.join(HTTP_RESOLVER_SUBDIR);
    std::fs::create_dir_all(&plugin_dir).map_err(|source| CookError::Io {
        path: plugin_dir.clone(),
        source,
    })?;
    let plug_info = std::fs::read_to_string(&src_pluginfo).map_err(|source| CookError::Io {
        path: src_pluginfo.clone(),
        source,
    })?;
    let dst_pluginfo = plugin_dir.join("plugInfo.json");
    std::fs::write(
        &dst_pluginfo,
        idtx_build::empty_plug_info_library_path(&plug_info),
    )
    .map_err(|source| CookError::Io {
        path: dst_pluginfo,
        source,
    })?;

    tracing::info!(
        plugin = %plugin_dir.display(),
        "staged HTTP resolver plugin into cooked desktop bundle (runtime links it at load time)"
    );
    Ok(())
}

/// Locate the desktop HTTP-resolver library and its `plugInfo.json` beside the
/// editor, returning `(library, plugInfo.json)`. Checks, in order: the
/// shipped-Conductor layout (the library flat in `exe_dir`, its plugInfo under
/// `exe_dir/idtx-http-resolver/`, staged by `bundle-win --remote`), then the dev
/// cargo layout (`<exe_dir>/build/idtx-usd-resolver-http-*/out/`, newest match,
/// where the library and `plugInfo.json` sit together). The desktop twin of
/// [`locate_resolver_plugin_dir`].
fn locate_desktop_resolver(runtime_exe_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    // Shipped-Conductor flat layout: library beside the exe, plugInfo in the
    // subfolder (its LibraryPath already emptied by the xtask bundler).
    let shipped_lib = runtime_exe_dir.join(HTTP_RESOLVER_DESKTOP_LIB);
    let shipped_pi = runtime_exe_dir
        .join(HTTP_RESOLVER_SUBDIR)
        .join("plugInfo.json");
    if shipped_lib.is_file() && shipped_pi.is_file() {
        return Some((shipped_lib, shipped_pi));
    }

    // Dev build: scan `<exe_dir>/build/idtx-usd-resolver-http-*/out/`, newest
    // first; the library and plugInfo.json are produced side by side there.
    let build_dir = runtime_exe_dir.join("build");
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&build_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("idtx-usd-resolver-http-")
        })
        .map(|e| e.path().join("out"))
        .filter(|out| {
            out.join(HTTP_RESOLVER_DESKTOP_LIB).is_file() && out.join("plugInfo.json").is_file()
        })
        .filter_map(|out| {
            let mtime = std::fs::metadata(out.join(HTTP_RESOLVER_DESKTOP_LIB))
                .and_then(|m| m.modified())
                .ok()?;
            Some((mtime, out))
        })
        .collect();
    candidates.sort_by_key(|(mtime, _)| *mtime);
    let (_, out) = candidates.pop()?;
    Some((
        out.join(HTTP_RESOLVER_DESKTOP_LIB),
        out.join("plugInfo.json"),
    ))
}


/// `cargo xtask build-quest-runtime`:
/// `<editor_dir>/android-runtime/arm64-v8a/libidtx_runtime.so`, with that
/// directory (holding the native `.so` deps + the `OpenXR` loader) as the
/// `libs_dir`.
///
/// Fails **closed** — it never falls back to the host `idtx-runtime.exe`, so a
/// Quest cook can only ever bundle a real aarch64 `.so` (the packager's own
/// ELF-arch guard is the second line of defence). The shipped Conductor carries
/// this directory with its bundled cross-compiled runtimes; a dev build must run
/// `cargo xtask build-quest-runtime` first. Cross-compilation is **never** run
/// during a cook: a shipped Conductor has no runtime source to build from, and
/// the cook host is not necessarily the cross-build host.
fn quest_runtime_binary() -> Result<RuntimeBinary, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate the editor executable: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "editor executable has no parent directory".to_owned())?;
    // Quest is 64-bit ARM only (mirrors idtx_build::ANDROID_ABI_DIR).
    let runtime_dir = dir.join("android-runtime").join("arm64-v8a");
    let so = runtime_dir.join("libidtx_runtime.so");
    if !so.is_file() {
        return Err(format!(
            "Quest runtime not found at {}.\n\
             The cross-compiled libidtx_runtime.so (plus its native .so deps and the \
             OpenXR loader) must be staged beside the editor. In a dev build, produce it with:\n    \
             cargo xtask build-quest-runtime\n\
             A shipped Conductor ships this directory with its bundled cross-compiled \
             runtimes; cross-compilation is never run during a cook.",
            so.display()
        ));
    }
    Ok(RuntimeBinary {
        executable: so,
        // The staged file is already correctly named; keep it verbatim so the
        // on-device loader finds `libidtx_runtime.so` (matches the manifest
        // `android.app.lib_name = idtx_runtime`).
        bundled_name: Some("libidtx_runtime.so".to_owned()),
        libs_dir: Some(runtime_dir),
        // OpenUSD plugInfo discovery trees staged beside the runtime by
        // `cargo xtask build-quest-runtime` (sibling of arm64-v8a/). The
        // packager copies them into the APK `assets/usd/`. Absent in older
        // staged trees → packager skips (and the app aborts at stage open until
        // the xtask is re-run).
        usd_plugins_dir: Some(dir.join("android-runtime").join("usd")),
        // Cross-built coded OpenExec plugin staged beside the runtime by
        // `cargo xtask build-quest-runtime --with-openexec` (sibling of
        // arm64-v8a/). `None`-like when absent: the dir simply does not exist, so
        // the packager's `is_dir()` guard skips it (a runtime built without
        // `openexec` carries no exec functionality to stage).
        exec_plugin_dir: Some(dir.join("android-runtime").join("usd-exec")),
        // Cross-built HTTP resolver plugin staged beside the runtime by
        // `cargo xtask build-quest-runtime --with-remote` (sibling of arm64-v8a/).
        // Skipped by the packager when the dir is absent (remote not requested);
        // required (fail-closed guard) when the runtime forces its `DT_NEEDED`.
        resolver_plugin_dir: Some(dir.join("android-runtime").join(HTTP_RESOLVER_SUBDIR)),
    })
}

/// Locate the **cross-compiled Vision Pro runtime** staged beside the editor by
/// `cargo xtask build-visionos-runtime`:
/// `<editor_dir>/visionos-runtime/arm64/libidtx_runtime.dylib`, with that
/// directory (holding the native `.dylib` deps) as the `libs_dir` and the sibling
/// `visionos-runtime/usd/` plugInfo trees as `usd_plugins_dir`.
///
/// Fails **closed** — like [`quest_runtime_binary`], it never falls back to the
/// host `idtx-runtime` binary, so a Vision Pro cook can only ever bundle a real
/// Mach-O arm64 `.dylib` (the packager's own Mach-O guard is the second line of
/// defence). The shipped Conductor carries this directory with its bundled
/// cross-compiled runtimes; a dev build must run `cargo xtask build-visionos-runtime`
/// first (on macOS). Cross-compilation is **never** run during a cook.
fn visionos_runtime_binary() -> Result<RuntimeBinary, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate the editor executable: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "editor executable has no parent directory".to_owned())?;
    // Vision Pro is 64-bit ARM only (mirrors xtask VISIONOS_ABI_DIR = "arm64").
    let runtime_dir = dir.join("visionos-runtime").join("arm64");
    let dylib = runtime_dir.join("libidtx_runtime.dylib");
    if !dylib.is_file() {
        return Err(format!(
            "Vision Pro runtime not found at {}.\n\
             The cross-compiled libidtx_runtime.dylib (plus its native .dylib deps) \
             must be staged beside the editor. In a dev build, produce it with (macOS only):\n    \
             cargo xtask build-visionos-runtime\n\
             A shipped Conductor ships this directory with its bundled cross-compiled \
             runtimes; cross-compilation is never run during a cook.",
            dylib.display()
        ));
    }
    Ok(RuntimeBinary {
        executable: dylib,
        bundled_name: Some("libidtx_runtime.dylib".to_owned()),
        libs_dir: Some(runtime_dir),
        // OpenUSD plugInfo discovery trees staged beside the runtime by
        // `cargo xtask build-visionos-runtime` (sibling of arm64/).
        usd_plugins_dir: Some(dir.join("visionos-runtime").join("usd")),
        // Cross-built coded OpenExec plugin staged beside the runtime by
        // `cargo xtask build-visionos-runtime --with-openexec` (sibling of arm64/).
        // Skipped by the packager when the dir is absent (exec not requested).
        exec_plugin_dir: Some(dir.join("visionos-runtime").join("usd-exec")),
        // The visionOS `.app` bundler stages the resolver `.dylib` into
        // `Contents/Frameworks/` via its own path; the Android-only field is unused
        // by the VisionOsAppPackager.
        resolver_plugin_dir: None,
    })
}

fn run_build(project: &IdtxProject, target: BuildTarget, deploy: bool) -> BuildOutcome {
    let cook_target = host_cook_target(target);
    let build_root = project
        .root()
        .join("build")
        .join(target.label().to_lowercase());
    let payload_dir = build_root.join("payload");

    #[cfg_attr(not(feature = "cooking"), allow(unused_mut))]
    let mut plan = match CookPlan::from_project(project, cook_target.clone(), &payload_dir) {
        Ok(p) => p,
        Err(e) => return BuildOutcome::Failed(format!("cook plan: {e}")),
    };

    // TEMPORARY (see docs/plan_remote_refs_external_and_runtime_auth.md): when the
    // project opts in (`[build] embed_dev_token`), resolve the logged-in bearer
    // token and stamp it into the cooked `run.toml` so a device build can
    // authenticate remote USD fetches (no keychain on device). This embeds
    // credentials in the artifact — opt-in only, loud warning, must not be
    // distributed. Remove when the interactive logon-level flow lands.
    if project.manifest().build.embed_dev_token {
        match resolve_dev_token(project) {
            Ok(token) => {
                tracing::warn!(
                    "TEMPORARY: embedding the logged-in bearer token into the cooked run.toml \
                     ([build] embed_dev_token) — this build carries credentials and MUST NOT be \
                     distributed. Remove once the interactive logon-level flow exists."
                );
                plan.embed_dev_token = Some(token);
            }
            Err(e) => return BuildOutcome::Failed(format!("embed_dev_token: {e}")),
        }
    }

    // Cooking: how behaviors are compiled + bound depends on the level count.
    //
    // * **Multi-level** (per-level-assembly cook, Scenario 2): do NOT compose a
    //   single combined assembly and do NOT set `entry_override` — that would gate
    //   off `cook()`'s per-level path (`entry_override.is_none() && levels > 1`) and
    //   emit the old single-stage layout the transition driver cannot swap between.
    //   Instead supply a per-level behavior-prepare hook: `cook()` composes each
    //   per-level assembly, hands it to the hook (compile this level's authored
    //   sources + author the `idtx:behavior:module` overlay, co-located so the path
    //   is `./behaviors/<name>.wasm`), flattens the overlaid assembly to
    //   `levels/<id>/level.usdz`, and co-locates the `.wasm` beside it. This binds
    //   each level's behaviors to its own composed prim paths, then emits
    //   `run.toml` with `levels`/`start_level`/`transfer_level` so the runtime builds
    //   a `TransitionDriver`.
    //
    // * **Single-/zero-level**: compose one default-visible combined assembly,
    //   overlay its behaviors, and hand `cook()` that stage via `entry_override`
    //   (unchanged — byte-identical to the historical path).
    //
    // Fail-closed either way: a compose / compile error aborts the build rather
    // than packaging a player with a missing / stale module. The combined path's
    // assembly scratch + overlay temp back files that `cook()`'s flatten reads, so
    // their RAII guards must outlive the `cook()` call; they drop at the end of
    // this function. (The per-level path keeps no guards here — `cook()` owns the
    // scratch it composes each level in and removes it after flattening.)
    #[cfg(feature = "cooking")]
    let (_assembly, _overlay): (
        Option<idtx_build::AssemblyArtifact>,
        Option<crate::behavior_prepare::AuthoredOverlay>,
    ) = {
        if plan.levels.len() > 1 {
            let project = project.clone();
            let sdk = crate::behavior_tools::discover_sdk_path();
            plan.level_behavior_prepare = Some(idtx_build::LevelBehaviorPrepare::new(Arc::new(
                move |assembly: &Path| -> Result<Option<idtx_build::LevelBehaviorOverlay>, String> {
                    let prepared = crate::behavior_prepare::prepare(
                        assembly,
                        project.root(),
                        sdk.as_deref(),
                    )?;
                    if prepared.is_empty() {
                        return Ok(None);
                    }
                    let (overlaid_assembly, wasm) =
                        crate::behavior_prepare::author_colocated_module_overlay(
                            assembly, &prepared,
                        )?;
                    Ok(Some(idtx_build::LevelBehaviorOverlay {
                        overlaid_assembly,
                        wasm,
                    }))
                },
            )));
            (None, None)
        } else {
            let assembly = match idtx_build::author_cook_assembly(&plan) {
                Ok(a) => a,
                Err(e) => return BuildOutcome::Failed(format!("compose assembly: {e}")),
            };
            let overlay = match prepare_behaviors(project, &assembly.path) {
                Ok(guard) => guard,
                Err(e) => return BuildOutcome::Failed(format!("behavior compile: {e}")),
            };
            plan.entry_override = Some(
                overlay
                    .as_ref()
                    .map_or_else(|| assembly.path.clone(), |o| o.path().to_path_buf()),
            );
            (Some(assembly), overlay)
        }
    };

    let report = match cook(&plan) {
        Ok(r) => r,
        Err(e) => return BuildOutcome::Failed(format!("cook: {e}")),
    };

    // Select the runtime binary to co-locate.
    //
    // Quest and Vision Pro are special: they must bundle the *cross-compiled*
    // standalone runtime staged beside the editor by `cargo xtask
    // build-quest-runtime` / `build-visionos-runtime` — never the host
    // `idtx-runtime` binary (the field regression that shipped a Windows PE into
    // the APK). Fail closed when that artifact is absent. Every other target uses
    // the sibling host/desktop binary (cook-only when absent).
    let runtime = match target {
        BuildTarget::Quest => match quest_runtime_binary() {
            Ok(rt) => Some(rt),
            Err(msg) => return BuildOutcome::Failed(msg),
        },
        BuildTarget::VisionPro => match visionos_runtime_binary() {
            Ok(rt) => Some(rt),
            Err(msg) => return BuildOutcome::Failed(msg),
        },
        _ => sibling_runtime_binary(),
    };

    // Package only when a runtime binary is available to co-locate.
    let (bundle, note) = match runtime {
        Some(runtime) => match package(&report, &cook_target, runtime, project, &build_root, deploy) {
            Ok((path, note)) => (Some(path), note),
            Err(e) => return BuildOutcome::Failed(format!("package: {e}")),
        },
        None => (None, None),
    };

    BuildOutcome::Done {
        payload: report.out_dir.clone(),
        bundle,
        note,
    }
}

/// Compile every authored behavior source discovered on the composed `assembly`
/// stage to `.wasm` under `<project>/behaviors` and link each module into a temp
/// overlay (via `idtx:behavior:module`) that sublayers the assembly. Returns the overlay
/// guard (kept alive through `cook`, which flattens it), or `None` when the
/// assembly has no behavior sources — in which case the caller flattens the bare
/// assembly. The authored project stages are never mutated.
#[cfg(feature = "cooking")]
fn prepare_behaviors(
    project: &IdtxProject,
    assembly: &Path,
) -> Result<Option<crate::behavior_prepare::AuthoredOverlay>, String> {
    let sdk = crate::behavior_tools::discover_sdk_path();
    let prepared = crate::behavior_prepare::prepare(assembly, project.root(), sdk.as_deref())?;
    if prepared.is_empty() {
        return Ok(None);
    }
    let overlay = crate::behavior_prepare::author_module_overlay(assembly, &prepared)?;
    Ok(Some(overlay))
}

fn package(
    report: &idtx_build::CookReport,
    cook_target: &CookTarget,
    runtime: RuntimeBinary,
    project: &IdtxProject,
    build_root: &Path,
    deploy: bool,
) -> Result<(PathBuf, Option<String>), idtx_build::CookError> {
    let name = project.name();
    match bundle_kind_for(&cook_target.triple) {
        BundleKind::DesktopFolder => {
            // The sibling runtime exe lives beside the editor; that same
            // directory holds the OpenUSD/MaterialX/TBB runtime staged by the
            // build-time twin — our fallback source when `IDTX_USD_DIR` is unset.
            let exe_dir = runtime
                .executable
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            // The runtime's file name inside the bundle — captured before
            // `runtime` moves into the packager, so the finalize closure can
            // locate the staged binary and scan it for the resolver import.
            let runtime_bin_name = runtime
                .bundled_name
                .clone()
                .or_else(|| {
                    runtime
                        .executable
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let bundle = DesktopFolderPackager::new(cook_target.clone(), runtime, build_root)
                .with_bundle_name(name)
                .package_with(report, |bundle| {
                    // Include host-discovered USD resources before stamping and
                    // publishing. I/O failure must preserve the previous bundle.
                    match idtx_build::stage_usd_runtime(bundle, &exe_dir)? {
                        rpt if rpt.monolithic_found => tracing::info!(
                            libs = rpt.libs,
                            trees = ?rpt.trees,
                            source = %rpt.source,
                            "staged OpenUSD runtime beside packaged binary"
                        ),
                        rpt => tracing::warn!(
                            source = %rpt.source,
                            "OpenUSD runtime not found — packaged player will not launch \
                             (set IDTX_USD_DIR or run from a staged editor build)"
                        ),
                    }
                    // When the runtime links the HTTP resolver (`remote`
                    // feature), stage the plugin beside the exe too, or the
                    // packaged player crashes at load before `main`. No-op when
                    // the runtime was built without `remote`.
                    stage_desktop_http_resolver(bundle, &runtime_bin_name, &exe_dir)?;
                    Ok(())
                })?;
            Ok((bundle, None))
        }
        BundleKind::MacApp => {
            let info = MacAppInfo {
                name: name.clone(),
                identifier: format!("com.idtx.{}", sanitize_bundle_id(&name)),
                executable: name.clone(),
            };
            // The sibling runtime exe dir — our source for the HTTP resolver
            // plugin (see `stage_macos_http_resolver`). Captured before `runtime`
            // is moved into the packager.
            let runtime_exe_dir = runtime
                .executable
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let app = MacAppPackager::new(cook_target.clone(), runtime, build_root, info).package_with(report, |app| {
                // Resolver resources and rpath edits participate in the final
                // inventory, and a failed fixup cannot replace a good app.
                stage_macos_http_resolver(app, &name, &runtime_exe_dir)
            })?;
            Ok((app, None))
        }
        BundleKind::AndroidApk => {
            // Stage the gradle-shaped tree (manifest + jniLibs/arm64-v8a +
            // assets), then assemble a signed, installable `.apk` directly via
            // the Android SDK build-tools (aapt2 → jar → zipalign → apksigner).
            // The SDK/JDK roots come from `[build.android]` when set, else the
            // environment. When no SDK root is found at all — the common case
            // for a Windows/macOS editor with no cross-compiled runtime beside
            // it — leave the staging tree in place and defer the APK to an
            // `xtask`/CI host, rather than failing the whole build. Any *other*
            // failure (SDK present but incomplete, or a tool erroring mid-build)
            // is a real error and must abort, not silently fall back.
            let staging = AndroidApkPackager::new(cook_target.clone(), runtime, build_root)
                .with_bundle_name(name.clone())
                .package(report)?;
            let android = &project.manifest().build.android;
            match AndroidSdk::discover_with(android.sdk.as_deref(), android.jdk.as_deref()) {
                Ok(sdk) => {
                    let apk = build_root.join(format!("{name}.apk"));
                    let out = ApkAssembler::new(sdk, &staging, &apk).assemble()?;
                    tracing::info!(apk = %out.display(), "assembled signed Quest .apk");
                    Ok((out, None))
                }
                Err(ApkError::SdkRootNotFound { hint }) => {
                    tracing::warn!(
                        staging = %staging.display(),
                        "Android SDK not found ({hint}); left APK staging tree in place \
                         (set [build.android].sdk or ANDROID_HOME to assemble a signed .apk here)"
                    );
                    Ok((staging, Some(format!(
                        "staged APK tree only: Android SDK not found ({hint}). Set \
                         [build.android].sdk / ANDROID_HOME, or assemble via xtask/CI."
                    ))))
                }
                Err(e) => Err(e.into()),
            }
        }
        BundleKind::VisionOsApp => {
            package_visionos(report, cook_target, runtime, project, build_root, &name, deploy)
        }
    }
}

/// Package + finish + deploy the Vision Pro `.app`.
///
/// Stages the flat bundle (payload + runtime + USD plugins + Info.plist), then —
/// when a signing identity + development provisioning profile are available (from
/// `[build.visionos]` or auto-discovered on macOS) — assembles a launchable,
/// device-signed `.app` (free Personal-Team path: no notarization) and, if a
/// paired Vision Pro is detected, installs + launches it via `devicectl`.
///
/// **Fail-open** like the Android branch: when the signing creds / macOS toolchain
/// are absent, the staged `.app` layout is left in place and the build still
/// succeeds (deferring the real `.app` to `xtask run-visionos-device` or a macOS
/// host). A *real* signing/assembly failure once creds are present aborts.
fn package_visionos(
    report: &idtx_build::CookReport,
    cook_target: &CookTarget,
    runtime: RuntimeBinary,
    project: &IdtxProject,
    build_root: &Path,
    name: &str,
    deploy: bool,
) -> Result<(PathBuf, Option<String>), idtx_build::CookError> {
    let cfg = project.manifest().build.visionos.clone();
    // The configured bundle id, or the derived default (reconciled against the
    // provisioning profile below).
    let configured_bundle_id = cfg
        .bundle_id
        .clone()
        .unwrap_or_else(|| format!("com.sap.apeirora.idtx.{}", sanitize_bundle_id(name)));

    // Try to resolve device signing credentials. Any gap (not macOS, no identity,
    // no matching profile) → None, and we stage-only. Finishing the `.app`
    // (swiftc + codesign) is macOS-only, so never attempt it off macOS even when
    // creds are explicitly configured — stage for parity instead (like Android).
    let mut creds =
        resolve_visionos_creds(&cfg, &configured_bundle_id).filter(|_| cfg!(target_os = "macos"));

    // When the gap is specifically the missing keychain cert / provisioning
    // profile for this bundle id, mint them inline: generate a throwaway Xcode
    // project for *this app's own bundle id*, run `xcodebuild
    // -allowProvisioningUpdates -allowProvisioningDeviceRegistration` to create
    // the Apple Development cert + development profile + register the paired
    // headset, then delete the scratch project — the same "creation → mint →
    // deletion" the `bootstrap-visionos-signing` xtask does. Fail-open: any
    // problem (not macOS, no paired device, free-team GUI mint still required)
    // just leaves us at stage-only below.
    let mut bootstrap_attempted = false;
    if creds.is_none() && cfg!(target_os = "macos") {
        bootstrap_attempted = attempt_signing_bootstrap(&cfg, &configured_bundle_id, build_root);
        if bootstrap_attempted {
            creds = resolve_visionos_creds(&cfg, &configured_bundle_id);
        }
    }

    let bundle_id = creds
        .as_ref()
        .map_or_else(|| configured_bundle_id.clone(), |c| c.bundle_id.clone());

    // Stage the flat `.app` with the resolved bundle id + hand-tracking opt-in.
    let app = VisionOsAppPackager::new(cook_target.clone(), runtime, build_root)
        .with_bundle_name(name)
        .with_bundle_id(bundle_id.clone())
        .with_hand_tracking(cfg.enable_hand_tracking)
        .package(report)?;

    let Some(creds) = creds else {
        let note = if bootstrap_attempted {
            format!(
                "staged only (unsigned): no signing profile for `{configured_bundle_id}` was \
                 created (the one-time Xcode signing was skipped or did not complete). Rebuild to \
                 try the signing bootstrap again, or run \
                 `cargo xtask-visionos bootstrap-visionos-signing --bundle-id {configured_bundle_id}`."
            )
        } else {
            format!(
                "staged only (unsigned): no signing identity or provisioning profile for \
                 `{configured_bundle_id}`. Register a profile for this bundle id (open it once in \
                 Xcode) or set [build.visionos] identity/provisioning_profile; then rebuild. \
                 (`cargo xtask run-visionos-device` works because it defaults to \
                 `com.sap.apeirora.idtx.vision`, which a profile already covers.)"
            )
        };
        tracing::warn!(
            app = %app.display(),
            bootstrap_attempted,
            "visionOS signing identity / provisioning profile not found (or not on macOS); \
             left the staged .app in place. Set [build.visionos] or run \
             `cargo xtask run-visionos-device` on a macOS host with a free Apple ID to produce \
             a signed, installable bundle."
        );
        return Ok((app, Some(note)));
    };

    // Finish into a launchable, device-signed bundle (compile the Swift shell,
    // embed the profile, sign). A failure here IS a real error (creds are present).
    let deployment = std::env::var("IDTX_VISIONOS_DEPLOYMENT_TARGET").unwrap_or_else(|_| "2.0".into());
    let frameworks = app.join("Frameworks");
    let exe = app.join(name);
    finish_device_app(&DeviceAppPlan {
        app: &app,
        frameworks: &frameworks,
        runtime_dylib_name: "libidtx_runtime.dylib",
        // The Conductor-staged bundle already carries the dylib closure in
        // Frameworks/ (via the packager); no extra lib dirs to gather.
        lib_dirs: &[],
        // The cooked entry stage lands at the bundle root as `stage.usdz`.
        stage_resource: "stage",
        stage_ext: "usdz",
        sdk: "xros",
        clang_target: &format!("arm64-apple-xros{deployment}"),
        exe: &exe,
        identity: &creds.identity,
        provisioning_profile: &creds.profile,
        team_id: &creds.team_id,
        bundle_id: &bundle_id,
    })?;
    tracing::info!(app = %app.display(), bundle_id, "assembled signed Vision Pro .app");

    // Install + launch on a paired Vision Pro only when the user chose "Build and
    // deploy". Best-effort: a device/deploy failure never discards the signed
    // bundle. "Build only" stops here with the signed .app on disk.
    let note = if deploy {
        Some(deploy_to_paired_visionos(&app, &bundle_id))
    } else {
        Some(format!("signed `{bundle_id}` (build only — not deployed to device)"))
    };
    Ok((app, note))
}

/// Resolved visionOS device-signing credentials.
struct VisionOsCreds {
    identity: String,
    profile: PathBuf,
    team_id: String,
    /// The bundle id reconciled against the profile's `application-identifier`.
    bundle_id: String,
}

/// Resolve the signing identity + development provisioning profile from
/// `[build.visionos]` or auto-discovery. Returns `None` (→ stage-only) on any
/// gap (not macOS, no identity, no matching profile).
fn resolve_visionos_creds(
    cfg: &idtx_project::VisionOsToolchainConfig,
    configured_bundle_id: &str,
) -> Option<VisionOsCreds> {
    let identity = match cfg.identity.clone() {
        Some(id) => id,
        None => discover_development_identity().ok()?,
    };
    let profile = match cfg.provisioning_profile.clone() {
        Some(p) => p,
        None => discover_profile_for(configured_bundle_id).ok()?,
    };
    // Read the profile to reconcile the bundle id + team id. If it cannot be read
    // (e.g. a configured path on a non-macOS host), fall back to configured values.
    let (bundle_id, team_id) = match read_profile(&profile) {
        Ok(mp) => (
            mp.resolved_bundle_id(configured_bundle_id),
            cfg.team_id.clone().unwrap_or(mp.team_id),
        ),
        Err(_) => (
            configured_bundle_id.to_owned(),
            cfg.team_id.clone().unwrap_or_default(),
        ),
    };
    Some(VisionOsCreds {
        identity,
        profile,
        team_id,
        bundle_id,
    })
}

/// Mint a free-team signing cert + development provisioning profile for
/// `configured_bundle_id`, mirroring the `bootstrap-visionos-signing` xtask:
/// generate a throwaway visionOS Xcode project for *this* bundle id and run
/// `xcodebuild -allowProvisioningUpdates -allowProvisioningDeviceRegistration`
/// against the paired headset.
///
/// Free Personal Teams cannot be provisioned by the `xcodebuild` CLI ("No
/// Accounts") — Apple only mints their cert/profile through the Xcode GUI. So on
/// that failure we fall back (on macOS) to opening the generated project in
/// Xcode, waiting for the user to quit it, and re-checking: if the profile now
/// exists we proceed; otherwise a native dialog lets the user retry in Xcode or
/// stage the app unsigned. The scratch project is deleted when the flow ends.
///
/// Returns `true` when the bootstrap was actually run (so the caller re-tries
/// credential resolution), `false` when it was skipped (no paired Vision Pro to
/// register against — the mint needs one). Entirely best-effort: every failure
/// degrades to the caller's stage-only path. The caller gates on
/// `cfg!(target_os = "macos")`.
fn attempt_signing_bootstrap(
    cfg: &idtx_project::VisionOsToolchainConfig,
    configured_bundle_id: &str,
    build_root: &Path,
) -> bool {
    // The profile must list the device UDID, so a paired Vision Pro is required.
    let devices = match list_paired_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::info!("signing bootstrap skipped: device query failed ({e})");
            return false;
        }
    };
    let Some(device) = select_visionos_device(&devices).map(|d| d.identifier.clone()) else {
        tracing::info!(
            "signing bootstrap skipped: no paired Vision Pro to register a profile against"
        );
        return false;
    };

    // Team: an explicit `[build.visionos].team_id`, else the auto-detected free
    // Personal Team (None lets automatic signing resolve the single account).
    let team = cfg.team_id.clone().or_else(idtx_build::detect_free_team);
    let scratch = build_root.join(".visionos-signing-bootstrap");

    tracing::info!(
        bundle_id = configured_bundle_id,
        device,
        "no signing profile found; minting one via the visionOS signing bootstrap"
    );
    // Headless attempt. `cleanup: false` keeps the generated project on disk so
    // the Xcode GUI fallback can open it when a free team can't mint from the CLI.
    match idtx_build::bootstrap_signing(&idtx_build::SigningBootstrapPlan {
        bundle_id: configured_bundle_id,
        team: team.as_deref(),
        device_udid: &device,
        scratch_dir: &scratch,
        cleanup: false,
    }) {
        Ok(()) => {
            tracing::info!("signing bootstrap minted a cert + provisioning profile (headless)");
        }
        Err(idtx_build::VisionOsBootstrapError::XcodebuildFailed { xcodeproj, .. }) => {
            // Free-team path: drive the one-time mint through the Xcode GUI.
            let proj = xcodeproj.unwrap_or_else(|| {
                scratch.join(format!("{}.xcodeproj", idtx_build::BOOTSTRAP_APP))
            });
            xcode_gui_mint_loop(&proj, configured_bundle_id);
        }
        Err(e) => {
            tracing::warn!("signing bootstrap did not complete ({e})");
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    true
}

/// Drive the free-team one-time mint through the Xcode GUI: open the generated
/// signing project in Xcode and let the user mint the profile (pick a Team +
/// press Run once), using a "Re-check" dialog as the synchronization point —
/// then re-check for the profile. If it is still missing, a dialog offers Retry
/// or Stage-only. Returns when a profile exists or the user declines.
///
/// We deliberately do NOT wait for Xcode to quit (`open -W`): closing a project
/// window leaves the Xcode app running, so that wait blocks indefinitely. The
/// user clicks "Re-check" when the Xcode step is done. No-op on a host without
/// `open`/`osascript` (the dialog returns `None` → caller stage-only).
fn xcode_gui_mint_loop(xcodeproj: &Path, bundle_id: &str) {
    // Open the signing project in Xcode (non-blocking — `open` hands off and
    // returns immediately; it does not wait for Xcode to quit).
    if let Err(e) = std::process::Command::new("open").arg(xcodeproj).status() {
        tracing::warn!("could not open the signing project in Xcode ({e})");
        return;
    }

    loop {
        let prompt = format!(
            "Xcode has opened a one-time signing project for \"{bundle_id}\".\n\n\
             In Xcode: pick your Team under Signing & Capabilities, choose the Vision Pro as the \
             run destination, and press Run (Cmd-R) once to create the development profile.\n\n\
             Click \"Re-check\" here when that has finished."
        );
        // Cancel / "Stage app only" / no dialog available (non-macOS) → give up.
        if macos_dialog(&prompt, &["Stage app only", "Re-check"], "Re-check").as_deref()
            != Some("Re-check")
        {
            return;
        }

        // Did the GUI mint produce a matching profile?
        if discover_profile_for(bundle_id).is_ok() {
            tracing::info!(bundle_id, "signing profile is now present after the Xcode mint");
            return;
        }

        let retry = format!(
            "No development provisioning profile for \"{bundle_id}\" was found yet.\n\n\
             A free-team profile is created only after a SUCCESSFUL Run to the Vision Pro in \
             Xcode. Finish that, then Retry — or stage the app unsigned."
        );
        // Not found yet — let the user retry the Xcode mint or stage unsigned.
        if macos_dialog(&retry, &["Stage app only", "Retry"], "Retry").as_deref() != Some("Retry") {
            return;
        }
        // Bring the signing project back to the front for another attempt.
        let _ = std::process::Command::new("open").arg(xcodeproj).status();
    }
}

/// Show a native macOS dialog with `buttons` (last is the default/safe choice)
/// and return the pressed button's label. Returns `None` when the user cancels,
/// `osascript` is unavailable (non-macOS), or the invocation fails — callers treat
/// `None` as the safe/decline choice.
fn macos_dialog(text: &str, buttons: &[&str], default: &str) -> Option<String> {
    let button_list = buttons
        .iter()
        .map(|b| format!("\"{b}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display dialog \"{escaped}\" buttons {{{button_list}}} default button \"{default}\" \
         with title \"IDTX Vision Pro signing\""
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        // Non-zero means the user hit a Cancel-role button (or osascript is absent).
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split("button returned:")
        .nth(1)
        .map(|s| s.trim().trim_end_matches(['\n', '\r', ',']).to_owned())
}



/// Detect a paired Vision Pro and `devicectl` install + launch the signed `.app`.
/// Entirely best-effort: every failure is logged, none propagates. Returns a
/// short human-facing outcome for the status bar (the GUI user sees no logs).
fn deploy_to_paired_visionos(app: &Path, bundle_id: &str) -> String {
    let devices = match list_paired_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::info!("no paired devices queried ({e}); skipping on-device install");
            return format!("signed, but device query failed ({e}); .app left on disk");
        }
    };
    let Some(device) = select_visionos_device(&devices) else {
        tracing::info!("no paired Vision Pro found; signed .app left on disk for manual install");
        return "signed, but no paired Vision Pro found; .app left on disk".to_owned();
    };
    if let Err(e) = install_app(&device.identifier, app) {
        tracing::warn!("devicectl install failed ({e}); the signed .app is still on disk");
        return format!("signed, but devicectl install failed ({e})");
    }
    // only deploy to the device. Do not auto-start it
    tracing::info!(device = %device.name, "installed on Vision Pro");
    format!("installed on {}", device.name)
    /*match launch_app(&device.identifier, bundle_id) {
        Ok(()) => {
            tracing::info!(device = %device.name, "installed + launched on Vision Pro");
            format!("installed + launched on {}", device.name)
        }
        Err(e) => {
            tracing::warn!("installed, but launch failed ({e})");
            format!("installed on {}, but launch failed ({e})", device.name)
        }
    }*/
}

/// Reduce a project name to a reverse-DNS-safe bundle-id segment.
fn sanitize_bundle_id(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = s.trim_matches('-');
    if trimmed.is_empty() {
        "experience".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end guard for the per-level Conductor build (level-transition build
    /// integration): re-cook the real multi-level `BehaviorRefactor` project for
    /// Desktop and assert the cooked `run.toml` now carries the per-level
    /// `[experience].levels` + `start_level` + `transfer_level` (so the runtime
    /// builds a `TransitionDriver`), that each per-level `level.usdz` exists, and —
    /// the crux — that the lobby login behavior's module is co-located so its
    /// authored `./behaviors/<name>.wasm` resolves on disk (a wrong path here is the
    /// silent "behaviors don't load" failure this change fixes).
    ///
    /// `#[ignore]`: links the `OpenUSD` monolith, compiles the authored behaviors via
    /// the Rust/wasm SDK, and flattens several stages. Run with the USD env set:
    /// `IDTX_USD_DIR=… DYLD_LIBRARY_PATH=$IDTX_USD_DIR/lib cargo test -p idtx-studio-shell \
    ///   --features cooking -- --ignored behaviorrefactor`.
    #[test]
    #[ignore = "re-cooks a real multi-level project; needs IDTX_USD_DIR + the behavior SDK"]
    #[cfg(feature = "cooking")]
    fn behaviorrefactor_cooks_per_level_with_bound_behaviors() {
        // Register the IDTX custom schemas (IdtxBehaviorAPI) as the editor does at
        // startup; without it the behavior-module overlay's ApplyApi fails with
        // "unknown API schema type" in a bare test process.
        let _ = idtx_usd_extension::install();

        let manifest = "/Users/D054981/Documents/Dev/ApeiroRA/IDTXToolbox/testfiles/projects/BehaviorRefactor/project.toml";
        let mut project = IdtxProject::load(manifest).expect("load BehaviorRefactor project");
        // The dev-token embed needs the `remote` auth feature (off in this test
        // build) and is irrelevant to verifying per-level cook + behavior binding.
        project.manifest_mut().build.embed_dev_token = false;

        let payload = match run_build(&project, BuildTarget::Desktop, false) {
            BuildOutcome::Done { payload, .. } => payload,
            BuildOutcome::Failed(msg) => panic!("build failed: {msg}"),
        };

        // run.toml carries the per-level entries, no single combined stage.
        let text = std::fs::read_to_string(payload.join("run.toml")).expect("read run.toml");
        assert!(text.contains("levels = ["), "run.toml missing [experience].levels:\n{text}");
        assert!(text.contains("start_level ="), "run.toml missing start_level:\n{text}");
        assert!(text.contains("transfer_level ="), "run.toml missing transfer_level:\n{text}");
        assert!(
            !text.lines().any(|l| l.trim_start().starts_with("stage =")),
            "a per-level cook must emit no single [experience].stage:\n{text}"
        );

        // Each per-level entry flattened to a self-contained usdz.
        for id in ["lobby", "datacenter"] {
            let entry = payload.join(format!("levels/{id}/level.usdz"));
            assert!(entry.is_file(), "missing per-level entry {}", entry.display());
            // The composed per-level assembly re-opens and exposes the re-rooted body.
            let mut stage = idtx_usd::StageService::open_topology_only(&entry)
                .unwrap_or_else(|e| panic!("open {}: {e}", entry.display()));
            let names: Vec<String> = stage
                .project_topology()
                .expect("project topology")
                .iter()
                .map(|r| r.name.clone())
                .collect();
            assert!(names.contains(&"level".to_string()), "{id}: no /World/level holder: {names:?}");
            assert!(names.contains(&"Player".to_string()), "{id}: no /World/Player: {names:?}");
        }

        // The crux: the lobby login behavior's compiled module is co-located beside
        // its level.usdz, so the authored `./behaviors/login_screen_panel_behavior.wasm`
        // resolves layer-relative at runtime (behaviors will actually load).
        let wasm = payload.join("levels/lobby/behaviors/login_screen_panel_behavior.wasm");
        assert!(
            wasm.is_file(),
            "lobby login behavior module not co-located at {} — behaviors would not load",
            wasm.display()
        );
    }

    #[test]
    fn host_triple_is_nonempty_and_arch_prefixed() {
        let t = host_triple();
        assert!(t.starts_with(std::env::consts::ARCH), "triple = {t}");
        assert!(t.contains('-'));
    }

    #[test]
    fn host_cook_target_mode_tracks_xr() {
        assert_eq!(host_cook_target(BuildTarget::Desktop).mode, "desktop");
        assert_eq!(host_cook_target(BuildTarget::Pcvr).mode, "xr");
        assert_eq!(host_cook_target(BuildTarget::Quest).mode, "xr");
        assert_eq!(host_cook_target(BuildTarget::VisionPro).mode, "xr");
    }

    #[test]
    fn standalone_targets_use_fixed_foreign_triples() {
        // Desktop/PCVR follow the host; standalone targets are foreign-fixed.
        let host = host_triple();
        assert_eq!(host_cook_target(BuildTarget::Desktop).triple, host);
        assert_eq!(host_cook_target(BuildTarget::Pcvr).triple, host);

        let quest = host_cook_target(BuildTarget::Quest);
        assert_eq!(quest.triple, "aarch64-linux-android");
        assert_eq!(quest.label, "quest");
        assert_eq!(
            quest.features,
            vec!["quest".to_string()]
        );

        let avp = host_cook_target(BuildTarget::VisionPro);
        assert_eq!(avp.triple, "aarch64-apple-visionos");
        assert_eq!(avp.label, "visionos");
        assert_eq!(
            avp.features,
            vec!["visionos-immersive".to_string()]
        );
    }

    #[test]
    fn bundle_id_sanitization() {
        assert_eq!(sanitize_bundle_id("My Cool Level!"), "my-cool-level");
        assert_eq!(sanitize_bundle_id("---"), "experience");
        assert_eq!(sanitize_bundle_id(""), "experience");
    }

    #[test]
    fn resolver_plugin_dir_prefers_shipped_then_flat_then_dev() {
        let tmp = std::env::temp_dir().join(format!(
            "idtx-resolver-locate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exe_dir = tmp.join("MacOS");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let touch = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(HTTP_RESOLVER_DYLIB), b"\xcf\xfa\xed\xfe").unwrap();
        };

        // Nothing staged yet → None.
        assert!(locate_resolver_plugin_dir(&exe_dir).is_none());

        // Dev cargo layout only.
        let dev = tmp.join("MacOS/build/idtx-usd-resolver-http-abc123/out");
        touch(&dev);
        assert_eq!(
            locate_resolver_plugin_dir(&exe_dir),
            Some(dev.clone()),
            "dev OUT_DIR found when it is the only candidate"
        );

        // Flat beside the runtime wins over the dev layout.
        touch(&exe_dir);
        assert_eq!(locate_resolver_plugin_dir(&exe_dir), Some(exe_dir.clone()));

        // Shipped Frameworks layout wins over everything.
        let shipped = tmp.join("Frameworks").join(HTTP_RESOLVER_SUBDIR);
        touch(&shipped);
        // Compare by resolved path (the function returns `<exe_dir>/../Frameworks/…`).
        let got = locate_resolver_plugin_dir(&exe_dir).unwrap();
        assert_eq!(
            std::fs::canonicalize(&got).unwrap(),
            std::fs::canonicalize(&shipped).unwrap(),
            "shipped Frameworks layout takes precedence"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The desktop (Windows/Linux) resolver staging is the twin of the macOS
    /// path: it must be a no-op when the runtime was built without `remote`,
    /// fail closed when the runtime links the resolver but no source is found,
    /// and otherwise stage the library flat in the bundle root with an
    /// emptied-`LibraryPath` `plugInfo.json` under `idtx-http-resolver/`.
    #[test]
    fn desktop_http_resolver_noop_fail_closed_then_stages() {
        let tmp = std::env::temp_dir().join(format!(
            "idtx-desktop-resolver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exe_dir = tmp.join("bin");
        let bundle = tmp.join("bundle");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&bundle).unwrap();
        let runtime_name = "idtx-runtime.exe";

        // 1. A staged runtime WITHOUT the resolver import → staging is a no-op.
        std::fs::write(bundle.join(runtime_name), b"MZ plain runtime, no remote").unwrap();
        stage_desktop_http_resolver(&bundle, runtime_name, &exe_dir).unwrap();
        assert!(
            !bundle.join(HTTP_RESOLVER_DESKTOP_LIB).exists(),
            "no library staged when the runtime does not link the resolver"
        );
        assert!(!bundle.join(HTTP_RESOLVER_SUBDIR).exists());

        // 2. A runtime that carries the import but with no source beside the
        //    editor → fail closed rather than ship a crashing bundle.
        let mut linked = b"MZ runtime\x00".to_vec();
        linked.extend_from_slice(HTTP_RESOLVER_DESKTOP_LIB.as_bytes());
        linked.push(0);
        std::fs::write(bundle.join(runtime_name), &linked).unwrap();
        assert!(matches!(
            stage_desktop_http_resolver(&bundle, runtime_name, &exe_dir),
            Err(idtx_build::CookError::HttpResolverStaging(_))
        ));

        // 3. Provide the dev cargo OUT_DIR layout (library + plugInfo together).
        let out = exe_dir.join("build/idtx-usd-resolver-http-abc123/out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(HTTP_RESOLVER_DESKTOP_LIB), b"the-lib").unwrap();
        std::fs::write(
            out.join("plugInfo.json"),
            br#"{ "Plugins": [ { "Type": "library", "LibraryPath": "idtx_http_resolver", "Name": "IdtxHttp" } ] }"#,
        )
        .unwrap();
        stage_desktop_http_resolver(&bundle, runtime_name, &exe_dir).unwrap();
        assert_eq!(
            std::fs::read(bundle.join(HTTP_RESOLVER_DESKTOP_LIB)).unwrap(),
            b"the-lib",
            "library staged flat in the bundle root beside the exe"
        );
        let staged_pi =
            std::fs::read_to_string(bundle.join(HTTP_RESOLVER_SUBDIR).join("plugInfo.json"))
                .unwrap();
        assert!(
            staged_pi.contains("\"LibraryPath\": \"\"")
                || staged_pi.contains("\"LibraryPath\":\"\""),
            "LibraryPath emptied so PlugRegistry does not re-load the library: {staged_pi}"
        );
        assert!(
            !staged_pi.contains("idtx_http_resolver"),
            "the old LibraryPath value is gone: {staged_pi}"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }
}
