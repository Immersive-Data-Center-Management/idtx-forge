//! Headless-testable standalone player core for IDTX.
//!
//! Opens a USD stage in a [`StudioSession`], runs behaviors, renders **without**
//! any editor chrome, and (on the `pcvr` Windows build) drives an `OpenXR`
//! session. This is the reusable engine behind the `idtx-runtime` binary and
//! the editor's out-of-process "Preview in Runtime".
//!
//! - [`run`] drives a winit window + swapchain (the shipped player / preview).
//! - [`run_headless`] steps a fixed number of frames with no surface, for tests
//!   and CI smoke checks — it genuinely ticks USD/animation/behaviors/(mock) XR
//!   each frame (unlike idtx-poc's headless path, which was a no-op).

pub mod bootstrap;
pub mod config;
pub mod present;
#[cfg(idtx_xr)]
pub mod xr_host;

pub use bootstrap::resolve_run_config;
pub use config::{RunConfig, RunMode};
// Re-exported from the extracted (winit-free) `idtx-session-orchestration`
// crate so existing `idtx_runtime_core::{SessionSet, TransitionDriver, …}`
// paths — and the module paths used by integration tests — stay stable.
pub use idtx_session_orchestration::{
    ActiveSession, GlobalState, LevelTable, SessionConfigure, SessionSet, StageLoader,
    TransitionDriver, global_state, session_set, stage_loader, transition,
};

mod player;
#[cfg(all(test, feature = "metrics"))]
mod camera_overlay_tests;
#[cfg(feature = "remote")]
mod remote;

use std::path::PathBuf;

use idtx_behavior::InputMap;
use idtx_studio_session::{RenderWorldUnitScale, StudioSession};

/// Errors from opening or running a stage in the player.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("failed to open stage: {0}")]
    Session(#[from] idtx_studio_session::SessionError),
    #[error("GPU init failed: {0}")]
    Gpu(#[from] idtx_platform_desktop::DesktopGpuError),
    #[error("window creation failed: {0}")]
    Window(#[from] winit::error::OsError),
    #[error("event loop error: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    #[error("no suitable GPU adapter for headless rendering")]
    NoHeadlessAdapter,
    #[error("{0}")]
    Message(String),
}

/// Resolve a `BehaviorRef::module_asset` path to `.wasm` bytes, relative to the
/// stage directory. Absolute paths are read as-is.

/// Locate the project root for a stage: the nearest ancestor directory (from
/// the stage's own directory upward) that contains a `project.toml`. Falls back
/// to the stage's parent directory when no project marker is found — a bare
/// single-stage run still gets a sensible place to look for `input_map.toml`.
#[cfg(test)]
fn project_root_for_stage(stage: &std::path::Path) -> Option<PathBuf> {
    let start = stage.parent()?;
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("project.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    Some(start.to_path_buf())
}

/// Load the input map for a stage's project.
///
/// Looks for `input_map.toml` at the stage's [project root](project_root_for_stage)
/// and parses it. A missing or unparsable file is **non-fatal**: it warns and
/// yields an empty [`InputMap`] (no action resolves, so behaviors read zero /
/// inactive — the player still runs, just without input bindings).
#[must_use]
pub fn load_input_map(stage: &std::path::Path) -> InputMap {
    idtx_session_orchestration::BundleContext::for_stage(stage).load_input_map(stage)
}

/// Install the remote asset resolver and arm auth for `cfg`'s stage when built
/// with `feature = "remote"`; a no-op otherwise.
///
/// Must run before any `OpenUSD` type is touched in the process, so both entry
/// points ([`run`], [`run_headless`]) call it as their first step. It consults
/// `cfg`'s injected token fields (`--token` / `--token-file`) before falling
/// back to the shared studio config.
fn prepare_remote(cfg: &RunConfig, resolver_plugin_dir: Option<&std::path::Path>) {
    #[cfg(feature = "remote")]
    remote::prepare(cfg, resolver_plugin_dir);
    #[cfg(not(feature = "remote"))]
    let _ = (cfg, resolver_plugin_dir);
}

/// Install `cfg.asset_search_dirs` as the USD default-resolver search path so a
/// project-relative asset reference (a bare `@assets/foo.usdz@` that does not
/// resolve against its anchoring layer's own directory) anchors at the project's
/// asset roots — mirroring what the editor does on project open.
///
/// Without this, an out-of-process preview opens the composed assembly with an
/// unconfigured resolver: the level's own relative references (e.g.
/// `@assets/Kitchen_set_instanced.usdz@`, authored in `stages/level1.usda` but
/// living in `<project>/assets/`) fail to open and the referenced subtree
/// silently drops. Must run before any stage is opened (after `prepare_remote`,
/// which installs the remote resolver first). Non-fatal: a resolver that can't be
/// configured is logged and the run proceeds (references may then not resolve).
fn install_asset_search_path(cfg: &RunConfig) {
    match idtx_session_orchestration::BundleContext::for_stage(&cfg.stage).install_asset_search_path(&cfg.asset_search_dirs) {
        Ok(()) => tracing::info!(
            dirs = ?cfg.asset_search_dirs,
            "installed USD default-resolver asset search path"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "could not install asset search path; project-relative references may not resolve"
        ),
    }
}

/// The payload subdirectory holding the staged IDTX custom-schema plugin
/// (`plugInfo.json` + `generatedSchema.usda`), beside `run.toml`. A layout
/// contract with the cook (`idtx_build::cook::PAYLOAD_SCHEMA_DIR`); the runtime
/// does not depend on `idtx-build`, so the string is mirrored here (the same way
/// the Android USD-tree dir name `"usd"` is).
const PAYLOAD_SCHEMA_DIR: &str = "usd-schemas";

/// The cooked payload's subdir holding the **coded** `OpenExec` plugin (DLL +
/// `plugInfo.json` + `generatedSchema.usda`), beside `run.toml`. A separate dir
/// from [`PAYLOAD_SCHEMA_DIR`] because it is a distinct USD plugin. A layout
/// contract with the cook (`idtx_build::cook::PAYLOAD_EXEC_DIR`); the runtime
/// does not depend on `idtx-build`, so the string is mirrored here (the same way
/// [`PAYLOAD_SCHEMA_DIR`] is).
#[cfg(feature = "openexec")]
const PAYLOAD_EXEC_DIR: &str = "usd-exec";

/// The cooked payload's subdir holding the HTTP(S) USD resolver plugin's
/// `plugInfo.json`, beside `run.toml`. A layout contract with the packager
/// (`idtx_standalone::HTTP_RESOLVER_PLUGIN_DIR_NAME`); the runtime does not
/// depend on `idtx-standalone`, so the string is mirrored here (the same way
/// [`PAYLOAD_SCHEMA_DIR`] is). On a device bundle the resolver's native library
/// is force-loaded via `DT_NEEDED` from `jniLibs/`, but its `plugInfo.json`
/// extracts here — a *different* tree — so the resolver's own
/// `resolve_plugin_dir` auto-discovery (which only knows the macOS/iOS
/// `Frameworks/` layout) cannot find it. The runtime must register this dir
/// explicitly, else `http(s)` URIs are never bound to the resolver and a
/// packaged payload's remote refs fail to open.
#[cfg(all(target_os = "android", feature = "remote"))]
const PAYLOAD_HTTP_RESOLVER_DIR: &str = "idtx-http-resolver";

/// Register the IDTX custom `OpenUSD` schemas (e.g. `IdtxCollisionAPI`) with the
/// `PlugRegistry` before any stage is opened.
///
/// Mirrors the editor shell's startup `install()` call. Without it,
/// `prim_has_api(.., "IdtxCollisionAPI")` returns `false` in the player (the
/// schema type is unknown to USD), so collision data never hydrates and the
/// behavior host's default collidable-only raycasts find nothing — a gaze /
/// pointer highlight on a collision-authored prim never appears. Must use the
/// runtime `PlugRegistry::RegisterPlugins` path (via `install`/`install_at`), not
/// `PXR_PLUGINPATH_NAME` (read at library-load time, too late here).
///
/// `staged` is a cooked payload's [`PAYLOAD_SCHEMA_DIR`] dir when the player is
/// packaged. It is preferred over `idtx_usd_extension::install`'s compile-time
/// baked plugin dir, which records this build host's `OUT_DIR` — a path that does
/// not exist on a device (Quest) or another machine, so the baked path registers
/// nothing there. On a dev `cargo run` against a raw project stage there is no
/// staged dir, so the baked fallback (valid on the build host) is used.
///
/// Non-fatal: a registration failure is logged and the run proceeds (collision
/// queries then simply return no hits).
fn register_idtx_schemas(
    staged: Option<&std::path::Path>,
    #[cfg_attr(not(feature = "openexec"), allow(unused_variables))] exec_staged: Option<
        &std::path::Path,
    >,
) {
    let result = match staged {
        Some(dir) if dir.join("plugInfo.json").is_file() => {
            tracing::info!(dir = %dir.display(), "registering staged IDTX schema plugin");
            idtx_usd_extension::install_at(dir)
        }
        _ => idtx_usd_extension::install(),
    };
    if let Err(e) = result {
        tracing::warn!("IDTX USD schema registration failed: {e}");
    }

    // The coded OpenExec plugin (compute-node schemas + their computations) is a
    // separate plugin from the codeless schema extension above, so it has its own
    // staged dir (`<payload>/usd-exec`, carrying its DLL + plugInfo.json +
    // generatedSchema.usda) — never the codeless extension's `usd-schemas` dir.
    // Register it the same way (staged dir when packaged, baked OUT_DIR on a dev
    // run) so authored compute graphs resolve their computations. Non-fatal:
    // without it, exec value keys build but Compute finds no computation and
    // authors nothing.
    #[cfg(feature = "openexec")]
    {
        let exec_result = match exec_staged {
            Some(dir) if dir.join("plugInfo.json").is_file() => idtx_usd_exec::install_at(dir),
            _ => idtx_usd_exec::install(),
        };
        if let Err(e) = exec_result {
            tracing::warn!("IDTX OpenExec plugin registration failed: {e}");
        }
    }
}

/// The cooked payload's staged IDTX schema dir for a resolved stage path
/// (`<payload>/usd-schemas`). The packagers stage it beside `run.toml`, which
/// sits beside the stage, so the stage's parent is the payload root. `None` only
/// when the stage has no parent.
fn staged_schema_dir(stage: &std::path::Path) -> Option<PathBuf> {
    stage.parent().map(|d| d.join(PAYLOAD_SCHEMA_DIR))
}

/// The cooked payload's staged `OpenExec` plugin dir for a resolved stage path
/// (`<payload>/usd-exec`), the twin of [`staged_schema_dir`]. `None` only when
/// the stage has no parent.
#[cfg(feature = "openexec")]
fn staged_exec_dir(stage: &std::path::Path) -> Option<PathBuf> {
    stage.parent().map(|d| d.join(PAYLOAD_EXEC_DIR))
}

/// Run the windowed player until the window is closed. Blocks the calling thread.
///
/// # Errors
///
/// Propagates window/GPU/stage/event-loop failures.
pub fn run(cfg: RunConfig) -> Result<(), RuntimeError> {
    prepare_remote(&cfg, None);
    #[cfg(feature = "openexec")]
    let exec_staged = staged_exec_dir(&cfg.stage);
    #[cfg(not(feature = "openexec"))]
    let exec_staged: Option<PathBuf> = None;
    register_idtx_schemas(
        staged_schema_dir(&cfg.stage).as_deref(),
        exec_staged.as_deref(),
    );
    install_asset_search_path(&cfg);
    let event_loop = winit::event_loop::EventLoop::new()?;
    let mut app = player::RuntimePlayer::new(cfg);
    event_loop.run_app(&mut app)?;
    app.into_result()
}

/// Materialize the packaged payload from the APK `assets/` into the app's
/// writable data dir, returning the extracted bundle root.
///
/// The payload (run.toml, stage, behaviors, and the USD plugin-discovery trees)
/// ships read-only inside the APK `assets/`, which `OpenUSD`'s plugin registry and
/// AR resolver cannot read. The cook emits `assets/payload.manifest` — a
/// newline-separated list of every payload file's relative path — because the
/// ndk `AssetManager` cannot enumerate subdirectories; we read it and copy each
/// listed file (opened by exact path) out to `<internal_data_path>/payload/`.
///
/// Device-bound: only verifiable on a real device / emulator; there is no host
/// `AssetManager`.
#[cfg(target_os = "android")]
fn extract_apk_payload(app: &android_activity::AndroidApp) -> Result<PathBuf, RuntimeError> {
    use std::ffi::CString;
    use std::io::Read as _;

    let data_dir = app.internal_data_path().ok_or_else(|| {
        RuntimeError::Message("Android internal_data_path unavailable".to_owned())
    })?;
    let payload = data_dir.join("payload");
    let am = app.asset_manager();

    let manifest_name = CString::new("payload.manifest").expect("static str has no NUL");
    let mut manifest_asset = am.open(&manifest_name).ok_or_else(|| {
        RuntimeError::Message(
            "assets/payload.manifest missing from APK (cook did not emit the payload \
             manifest; cannot extract the bundle)"
                .to_owned(),
        )
    })?;
    let mut manifest_text = String::new();
    manifest_asset
        .read_to_string(&mut manifest_text)
        .map_err(|e| RuntimeError::Message(format!("read payload.manifest: {e}")))?;

    let mut extracted = 0usize;
    let mut missing = 0usize;
    for rel in manifest_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        // aapt2 on Windows writes APK asset entries for subdirectories with
        // backslash separators (`behaviors\foo.wasm`), so a forward-slash open
        // misses them; retry with backslashes. Top-level entries have no
        // separator and match either way.
        let open = |p: &str| CString::new(p).ok().and_then(|c| am.open(&c));
        let Some(mut asset) = open(rel).or_else(|| open(&rel.replace('/', "\\"))) else {
            tracing::warn!("payload file listed in manifest not found in APK: {rel}");
            missing += 1;
            continue;
        };
        let mut bytes = Vec::new();
        asset
            .read_to_end(&mut bytes)
            .map_err(|e| RuntimeError::Message(format!("read asset {rel}: {e}")))?;
        let dest = payload.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RuntimeError::Message(format!("mkdir {}: {e}", parent.display())))?;
        }
        std::fs::write(&dest, &bytes)
            .map_err(|e| RuntimeError::Message(format!("write {}: {e}", dest.display())))?;
        extracted += 1;
    }
    tracing::info!(
        files = extracted,
        missing,
        dest = %payload.display(),
        "extracted APK payload to writable data dir"
    );
    Ok(payload)
}

/// Run the windowed player on Android/Quest, driven by the `NativeActivity`
/// [`AndroidApp`] handed to the generated `android_main`. Blocks until the
/// activity is destroyed.
///
/// Bootstraps the on-device payload first: extracts the APK `assets/` bundle to
/// the writable data dir ([`extract_apk_payload`]), points `PXR_PLUGINPATH_NAME`
/// at the extracted USD plugin trees **before** any stage opens (the registry
/// reads it once at first `Tf`/`Plug` touch), and resolves the packaged
/// experience against the extracted bundle. There is no argv on Android, so the
/// stage/mode come only from the bundled `run.toml`; an unset mode defaults to
/// [`RunMode::Xr`], so a Quest launch starts the real `OpenXR` Vulkan session (the
/// `quest` feature; `DesktopGpu` selects `Backends::VULKAN`). The surface is
/// (re)created across `resumed`/`suspended`.
///
/// # Errors
///
/// Propagates payload-extraction, config-resolution, and window/GPU/stage/
/// event-loop failures.
#[cfg(target_os = "android")]
pub fn run_android(app: android_activity::AndroidApp) -> Result<(), RuntimeError> {
    use winit::platform::android::EventLoopBuilderExtAndroid as _;

    // Hand the OpenXR Quest instance-create path the real Android **Activity**
    // jobject + JavaVM. `android-activity` stores the Application (not the
    // Activity) in `ndk_context`, which `openxr`'s `create_instance` would feed
    // into `XrInstanceCreateInfoAndroidKHR::applicationActivity`; the Meta
    // runtime needs the Activity there to track the lifecycle, else the session
    // never leaves IDLE and the headset shows the loading screen forever. Must
    // run before the event loop (whose `resumed` builds the OpenXR instance).
    #[cfg(idtx_xr)]
    idtx_studio_session::set_android_activity(app.vm_as_ptr(), app.activity_as_ptr());

    let payload = extract_apk_payload(&app)?;

    // Point the XR-metrics JSON dump at an `adb pull`-able location. The dump
    // (feature = "metrics") defaults to the system temp dir, which is not
    // reachable with `adb pull` on device; the app's external-files dir
    // (`/sdcard/Android/data/<pkg>/files/`) is. Prefer external storage, falling
    // back to the internal data dir if the device exposes no external path.
    #[cfg(feature = "metrics")]
    if let Some(dir) = app.external_data_path().or_else(|| app.internal_data_path()) {
        tracing::info!(metrics_dir = %dir.display(), "xr-metrics JSON dump dir");
        idtx_studio_session::set_metrics_output_dir(dir);
    }

    // USD plugin discovery (the on-device crux). The monolithic libusd_ms.so was
    // built with build-host-absolute default plugin paths that do not exist on
    // device, so the Plug registry must be pointed at the extracted plugInfo
    // trees. PXR_PLUGINPATH_NAME is NOT reliable here — USD reads it before our
    // setenv is visible to the C++ `getenv` on-device (the search list never
    // showed it) — so register the trees PROGRAMMATICALLY via
    // `configure_plugin_root`, which `register_plugins_once` consumes at the
    // first stage/USD op: it registers `<root>/plugin/usd` + `<root>/lib/usd`
    // through `PlugRegistry::RegisterPlugins`. The cook stages the Android USD
    // `lib/usd` + `plugin/usd` trees under `assets/usd/`; the per-schema/plugin
    // code is all in the loaded monolith, so only these plugInfo trees need to be
    // on disk. MUST run before `install_asset_search_path` below (the first op
    // that triggers registration).
    let usd_root = payload.join("usd");
    if usd_root.is_dir() {
        match idtx_usd::configure_plugin_root(&usd_root) {
            Ok(()) => tracing::info!(
                usd_root = %usd_root.display(),
                "configured OpenUSD plugin root for on-device discovery"
            ),
            Err(e) => tracing::warn!(
                usd_root = %usd_root.display(),
                "configure_plugin_root failed; USD plugin discovery may fail: {e}"
            ),
        }
    } else {
        tracing::warn!(
            usd_root = %usd_root.display(),
            "extracted USD plugin root absent; USD plugin discovery will fail"
        );
    }

    let load_manifest = || idtx_runtime_manifest::RuntimeManifest::load(payload.join("run.toml"));
    #[cfg(feature = "remote")]
    let manifest = idtx_usd_resolver_http::configure_network(load_manifest, |m| m.network.asset_policy());
    #[cfg(not(feature = "remote"))]
    let manifest = load_manifest();
    let manifest = manifest
        .map_err(|e| RuntimeError::Message(format!("load extracted run.toml: {e}")))?;
    let mut cfg = crate::bootstrap::resolve_with(&manifest, None, None, None, Some(&payload))
        .map_err(RuntimeError::Message)?;
    // The project asset root extracts to `<payload>/assets`; the manifest-derived
    // search path resolves wrong on Android (current_exe is `/system/bin`), so
    // anchor the resolver at the extracted assets dir when present, plus the
    // payload root itself so a staged `built-in/` tree resolves `@built-in/...@`.
    let assets_dir = payload.join("assets");
    if assets_dir.is_dir() {
        cfg.asset_search_dirs = vec![assets_dir, payload.clone()];
    } else {
        cfg.asset_search_dirs = vec![payload.clone()];
    }
    tracing::info!(
        stage = %cfg.stage.display(),
        mode = ?cfg.mode,
        "resolved packaged run config (android)"
    );

    // Install the HTTP(S) USD resolver pointed at the payload's EXTRACTED
    // `idtx-http-resolver/` plugInfo dir. The resolver's own `resolve_plugin_dir`
    // auto-discovery only knows the macOS/iOS `Frameworks/` bundle layout and, on
    // Android, falls back to a compile-time build-host path that does not exist on
    // device — so the plugInfo (hence the `http`/`https` URI-scheme binding) would
    // never register, and a packaged payload's remote refs anchored inside a usdz
    // package get `TfNormPath`-collapsed (`https://` → `https:/`) and fail to open.
    // Mirrors the explicit-dir registration the schema/exec plugins already use.
    #[cfg(feature = "remote")]
    let resolver_plugin_dir = Some(payload.join(PAYLOAD_HTTP_RESOLVER_DIR));
    #[cfg(not(feature = "remote"))]
    let resolver_plugin_dir: Option<PathBuf> = None;
    prepare_remote(&cfg, resolver_plugin_dir.as_deref());
    // Register the IDTX custom schemas from the payload's extracted `usd-schemas/`
    // dir (staged by the cook). On device the compile-time baked plugin dir is a
    // build-host path that does not exist, so without this `IdtxCollisionAPI`
    // never registers → collision never hydrates → the behaviors' collidable-only
    // raycasts find nothing (no gaze / pointer highlight). The coded OpenExec
    // plugin (feature `openexec`) is registered from its own extracted
    // `usd-exec/` dir.
    #[cfg(feature = "openexec")]
    let exec_staged = Some(payload.join(PAYLOAD_EXEC_DIR));
    #[cfg(not(feature = "openexec"))]
    let exec_staged: Option<PathBuf> = None;
    register_idtx_schemas(
        Some(&payload.join(PAYLOAD_SCHEMA_DIR)),
        exec_staged.as_deref(),
    );
    install_asset_search_path(&cfg);
    let event_loop = winit::event_loop::EventLoop::builder()
        .with_android_app(app)
        .build()?;
    let mut player = player::RuntimePlayer::new(cfg);
    event_loop.run_app(&mut player)?;
    player.into_result()
}

/// Outcome of a headless fixed-frame run — enough to assert progress in tests.
#[derive(Debug, Clone, Copy)]
pub struct HeadlessReport {
    /// Frames actually stepped.
    pub frames: u64,
    /// Authored runtime behaviors attached.
    pub attached_behaviors: usize,
    /// Whether an (XR) preview was started for this run.
    pub xr_started: bool,
    /// Frames on which a tracked XR pose was available.
    pub xr_pose_frames: u64,
    /// Total attributes authored by the `OpenExec` dataflow across the run (`0`
    /// when the stage carries no compute graph or `openexec` is off). A non-zero
    /// value means the datasource/compute graph produced live data — the signal
    /// the `--screenshot` capture waits on before reading pixels.
    pub exec_authored: u64,
}

/// Apply the per-session runtime setup shared by every session the player holds
/// (the current level and any resident transfer session): editor overlays off,
/// the behavior limits +
/// playing flag + input map, the (remote-gated) exec network policy, the
/// authored WASM behaviors, and the local-player camera selection. Factored out
/// of [`run_headless`] so a multi-session runtime configures each session
/// identically. Returns the number of authored behaviors attached (used for the
/// headless report of the current session).
fn configure_runtime_session(session: &mut StudioSession, cfg: &RunConfig) -> usize {
    // The session owns the single behavior host. The packaged experience
    // is always playing; attach authored runtime behaviors relative to the
    // stage dir (currently a no-op: USD behavior hydration is not wired).
    if let Err(error) = session.set_auth_origins(&cfg.auth_origins) {
        tracing::warn!(%error, "invalid credential origins; behavior credential writes denied");
    }
    // Gate the behavior `net` egress + stage-authored OpenExec REST datasources
    // behind the configured network allowlist (deny-by-default; see
    // `ExecNetworkPolicy`). Without this a hostile stage could issue SSRF requests
    // the moment it opens. Owned by the `remote` feature (the network stack).
    #[cfg(feature = "remote")]
    session.set_exec_network_policy(cfg.egress_policy());
    // Anchor the authored-WASM reader at THIS session's own stage directory, not
    // `cfg.stage` — the cook co-locates each level's modules under that level's
    // own `levels/<id>/behaviors/`, and this function also runs (via the
    // transition driver's `configure` closure) on every swapped-in level and the
    // transfer session. Keying on `session.source_path()` resolves each session's
    // `./behaviors/*.wasm` against its own directory; for the boot session
    // `source_path()` equals `cfg.stage`, so that path is unchanged.
    let bundle = idtx_session_orchestration::BundleContext::for_stage(&cfg.stage).with_state(cfg.experience_state.clone())
        .with_runtime_prototypes(cfg.runtime_prototypes.clone())
        .with_behavior_limits(cfg.behavior_limits());
    let attached_behaviors = bundle.configure_session(session);

    // Render through the local player's camera when the stage carries one
    // (net role `local`, e.g. the desktop/XR player rig's `/World/Player/
    // Camera`). During an XR preview the HMD-sync behavior authored on that
    // camera prim drives its `WorldTransform`, so the desktop mirror follows
    // the headset through this same scene-camera path. Falls back to the orbit
    // camera when the stage has no local-player camera.
    if let Some(path) = session.local_player_camera_path() {
        session.use_scene_camera(path);
    }

    attached_behaviors
}

/// Build a [`TransitionDriver`] from a [`RunConfig`] — the desktop/Quest
/// convenience wrapper over the config-independent
/// [`TransitionDriver::new`](transition::TransitionDriver::new). Resolves the
/// [`LevelTable`] from `cfg.levels` and captures a `configure` closure that
/// runs [`configure_runtime_session`] on each swapped-in level at cutover. The
/// winit-free visionOS runtime constructs its driver directly with its own
/// closure instead (it cannot depend on this crate).
///
/// `session_layer` is the anonymous session-layer USDA the boot path injects
/// into the initial level and the transfer/mask session (the runtime-resolved
/// XR controller device `Model` `over`; `""` when none). It must be forwarded
/// here so each level a transition swaps in receives the same injection —
/// otherwise the swapped-in level's bare controller pose Xforms hydrate with no
/// geometry and the controllers vanish after the first transition.
#[must_use]
pub fn build_transition_driver(
    cfg: &RunConfig,
    device: wgpu::Device,
    queue: wgpu::Queue,
    viewport: [u32; 2],
    session_layer: &str,
) -> TransitionDriver {
    let levels = transition::LevelTable::new(cfg.levels.clone());
    let cfg_owned = cfg.clone();
    TransitionDriver::new(
        levels,
        device,
        queue,
        viewport,
        Box::new(move |session: &mut StudioSession| {
            let _ = configure_runtime_session(session, &cfg_owned);
        }),
        session_layer.to_owned(),
    ).with_state(cfg.experience_state.clone())
}

/// Step the stage for `cfg.frames` frames with no surface, rendering each frame
/// to the session's offscreen target. Used by tests and CI smoke checks.
///
/// On the `mock-xr` build with [`RunMode::Xr`] this starts a mock XR preview so
/// the HMD-sync behavior + camera path are exercised headlessly.
///
/// # Errors
///
/// Propagates stage-open failures; returns [`RuntimeError::NoHeadlessAdapter`]
/// when no wgpu adapter is available.
pub fn run_headless(cfg: &RunConfig) -> Result<HeadlessReport, RuntimeError> {
    prepare_remote(cfg, None);
    #[cfg(feature = "openexec")]
    let exec_staged = staged_exec_dir(&cfg.stage);
    #[cfg(not(feature = "openexec"))]
    let exec_staged: Option<PathBuf> = None;
    register_idtx_schemas(
        staged_schema_dir(&cfg.stage).as_deref(),
        exec_staged.as_deref(),
    );
    install_asset_search_path(cfg);
    let frames = cfg.frames.unwrap_or(1);
    let (device, queue) = headless_device()?;

    // Resolver HTTP performs the same bounded refresh/retry for every adapter,
    // including nested assets and transition loads.
    let open = |device: wgpu::Device, queue: wgpu::Queue| {
        StudioSession::open_with_gpu(
            &cfg.stage,
            device,
            queue,
            cfg.viewport,
            cfg.texture_quality,
            RenderWorldUnitScale::METRE,
        )
    };
    let mut session = open(device.clone(), queue.clone())?;

    let attached_behaviors = configure_runtime_session(&mut session, cfg);

    // Multi-level experience: when the manifest declares a
    // transfer/mask assembly, open and configure it as a resident session so a
    // later transition can cut to it instantly while the next level loads async.
    // A single-stage run (`transfer_level` None) opens no transfer session, so its
    // behavior is byte-identical to before — the set stays on `ActiveSession::Level`.
    let transfer = if let Some(transfer_path) = &cfg.transfer_level {
        let mut transfer_session = StudioSession::open_with_gpu(
            transfer_path,
            device.clone(),
            queue.clone(),
            cfg.viewport,
            cfg.texture_quality,
            RenderWorldUnitScale::METRE,
        )?;
        configure_runtime_session(&mut transfer_session, cfg);
        Some(transfer_session)
    } else {
        None
    };

    let mut sessions = SessionSet::new(session, transfer);

    // Level-transition driver: a multi-level experience (`cfg.levels`
    // non-empty) builds the driver that services behavior-posted transition
    // requests; a single-stage run builds none, so the frame loop below never
    // polls one and stays byte-identical to before. The headless loop opens its
    // sessions with `open_with_gpu` (no controller injection layer), so it
    // forwards `""` — controllers are an XR/windowed concern.
    let mut driver = (!cfg.levels.is_empty()).then(|| {
        build_transition_driver(cfg, device.clone(), queue.clone(), cfg.viewport, "")
    });

    // `xr_started` is set true only on the mock-xr build in XR mode.
    #[cfg_attr(not(feature = "mock-xr"), allow(unused_mut))]
    let mut xr_started = false;
    #[cfg(feature = "mock-xr")]
    if cfg.mode == RunMode::Xr {
        if let Some(loader) = idtx_studio_session::probe_xr_runtime() {
            sessions
                .active_session_mut()
                .start_xr_preview(&loader)
                .map_err(|e| RuntimeError::Message(format!("mock XR start: {e}")))?;
            #[cfg(feature = "gesture-recognition")]
            {
                sessions
                    .active_session_mut()
                    .enable_gesture_recognition_default();
            }
            xr_started = true;
        }
    }

    let dt = 1.0_f32 / 60.0;
    let mut xr_pose_frames = 0_u64;
    // Total attributes the OpenExec dataflow authored across the run; a non-zero
    // value means the compute graph produced live data (the `--screenshot`
    // capture waits on this). Stays 0 (and never mutated) without `openexec`.
    #[cfg_attr(not(feature = "openexec"), allow(unused_mut))]
    let mut exec_authored = 0_u64;
    // Behind `feature = "metrics"`: accumulate wall-clock spent in the per-frame
    // behavior tick — the hot path that runs every attached WASM behavior
    // (input read → math → `stage_edit` authoring → re-projection). Zero cost
    // when the feature is off (the block and its timer compile out entirely).
    #[cfg(feature = "metrics")]
    let mut behavior_tick_total = std::time::Duration::ZERO;
    #[cfg(feature = "metrics")]
    let frame_metrics = idtx_session_orchestration::frame_metrics::FrameMetrics::new("headless", 16_666_667);
    for _ in 0..frames {
        #[cfg(feature = "metrics")]
        let _whole_frame = frame_metrics.start();
        {
            let session = sessions.active_session_mut();
            session.tick()?;
            session.tick_animation(f64::from(dt))?;
            // Single unified behavior tick (authored + XR sync). Returns whether a
            // tracked HMD pose was available this frame.
            #[cfg(feature = "metrics")]
            let tick_start = std::time::Instant::now();
            let tracked = session.tick_behaviors(dt);
            #[cfg(feature = "metrics")]
            {
                behavior_tick_total += tick_start.elapsed();
            }
            if xr_started && tracked {
                xr_pose_frames += 1;
            }
        }
        // Level-transition driver: service a behavior-posted transition
        // request right after `tick_behaviors` and before the render, so a
        // request posted this frame is picked up without a one-frame lag. One
        // non-blocking step per frame; no driver on a single-stage run. The
        // active session may change here (cut to the transfer scene, or swap in
        // the newly loaded level), so the physics/exec/render below re-borrow it.
        if let Some(driver) = driver.as_mut() {
            driver.poll(&mut sessions);
        }
        let session = sessions.active_session_mut();
        // Runtime physics (plan §6.1): the headless loop must step the same
        // `UsdPhysicsScene`-gated sim as the windowed player, after behaviors and
        // before the render reads the result, so a physics-enabled headless run
        // matches the shipped runtime. A no-op unless the stage carries a
        // `UsdPhysicsScene` prim; compiled only under the `physics` feature.
        #[cfg(feature = "physics")]
        if let Err(e) = session.tick_physics(dt, false) {
            tracing::warn!("physics tick: {e}");
        }
        // Runtime OpenExec dataflow (plan §4): drive REST/compute-node values into
        // the session layer after behaviors/physics and before render. A no-op
        // unless the stage carries an IDTX compute graph; `false` owns the drain
        // (headless has no XR single-drain fold).
        #[cfg(feature = "openexec")]
        match session.tick_exec(false) {
            Ok(n) => exec_authored += n,
            Err(e) => tracing::warn!("exec tick: {e}"),
        }
        session.render_one_frame(false);
    }

    #[cfg(feature = "metrics")]
    if frames > 0 {
        #[allow(clippy::cast_precision_loss)]
        let avg_us = behavior_tick_total.as_micros() as f64 / frames as f64;
        tracing::info!(
            target: "player-metrics",
            "behavior tick over {frames} frames: total {:.3} ms, avg {avg_us:.1} µs/frame",
            behavior_tick_total.as_secs_f64() * 1e3,
        );
    }

    // `--screenshot <path>`: read the final frame back from the GPU and write a
    // PNG, then exit. Under `openexec`, first pump extra frames (with a short
    // `--screenshot <path>`: read the final frame back from the GPU and write a
    // PNG, then exit. Under `openexec`, first pump extra frames (with a short
    // real sleep so the datasource poller threads can produce) until a datasource
    // payload has actually been applied — so the captured image reflects live data,
    // not just the authored compute defaults. The GPU readback (`read_pixels`) is
    // behind the `metrics` feature; a non-metrics build logs an error rather than
    // silently writing nothing.
    if let Some(path) = cfg.screenshot.clone() {
        let session = sessions.active_session_mut();
        #[cfg(feature = "openexec")]
        {
            // The mock/REST drivers produce on wall-clock `interval`s; the headless
            // loop runs far faster, so no external payload has arrived yet — the
            // values authored so far are just the compute DEFAULTS propagating. A
            // settled graph (no new datasource data) authors nothing, so any FURTHER
            // authoring past this baseline can only come from a real datasource
            // payload being applied. Wait (bounded) for that advance, so the capture
            // proves the datasource actually produced a value rather than snapshotting
            // the defaults.
            let baseline = exec_authored;
            let mut waited = 0u32;
            const MAX_WAIT_FRAMES: u32 = 600; // ~10 s at 16 ms/frame.
            while exec_authored <= baseline && waited < MAX_WAIT_FRAMES {
                std::thread::sleep(std::time::Duration::from_millis(16));
                session.tick()?;
                match session.tick_exec(false) {
                    Ok(n) => exec_authored += n,
                    Err(e) => tracing::warn!("exec tick (capture wait): {e}"),
                }
                session.render_one_frame(false);
                waited += 1;
            }
            if exec_authored > baseline {
                tracing::info!(
                    "datasource produced a value after {waited} frame(s) \
                     (exec authored {exec_authored} total); capturing live data"
                );
            } else if baseline > 0 {
                tracing::warn!(
                    "no datasource payload within the capture window; capturing compute defaults \
                     (exec authored {baseline})"
                );
            } else {
                tracing::warn!("no openexec activity; stage carries no compute graph");
            }
        }
        // One last clean frame so the capture reflects the latest authored values.
        session.render_one_frame(false);

        #[cfg(feature = "metrics")]
        {
            // Propagate readback/save failures so `--screenshot` is reliable for
            // scripts and CI: a logged-and-ignored error would let the command exit
            // successfully without ever creating the requested file.
            let rgba = session.read_pixels().map_err(|e| {
                RuntimeError::Message(format!("screenshot read_pixels failed: {e}"))
            })?;
            let [w, h] = cfg.viewport;
            image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8)
                .map_err(|e| RuntimeError::Message(format!("screenshot save failed: {e}")))?;
            tracing::info!("wrote screenshot {}", path.display());
        }
        #[cfg(not(feature = "metrics"))]
        return Err(RuntimeError::Message(format!(
            "--screenshot {} requires the `metrics` feature (the GPU readback is \
             metrics-gated); rebuild with e.g. --features openexec,metrics",
            path.display()
        )));
    }

    Ok(HeadlessReport {
        frames,
        attached_behaviors,
        xr_started,
        xr_pose_frames,
        exec_authored,
    })
}

/// Create a surfaceless wgpu device/queue for headless rendering.
pub(crate) fn headless_device() -> Result<(wgpu::Device, wgpu::Queue), RuntimeError> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| RuntimeError::NoHeadlessAdapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("idtx-runtime-headless"),
                // Enable 2× MSAA on `Rgba16Float` (QI-2) when the adapter supports
                // it; WebGPU core only guarantees 1×/4× for that format, and
                // `Renderer::from_device` clamps 2× → 4× if this is absent.
                required_features: if adapter
                    .features()
                    .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
                {
                    wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES
                } else {
                    wgpu::Features::empty()
                },
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .map_err(|e| RuntimeError::Message(format!("device request failed: {e}")))?;
        Ok((device, queue))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_behavior::manifest::InputKind;

    /// Absolute path to the Demo1 fixture project's desktop-player stage.
    fn demo1_stage() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../idtx-assets/built-in/projects/stages/level1.usda")
    }

    #[test]
    fn project_root_walks_up_to_project_toml() {
        let root = project_root_for_stage(&demo1_stage()).expect("root");
        assert!(
            root.join("project.toml").is_file(),
            "resolved root {} has no project.toml",
            root.display()
        );
        assert!(
            root.ends_with("projects"),
            "unexpected root {}",
            root.display()
        );
    }

    #[test]
    fn demo1_input_map_binds_the_player_actions() {
        let map = load_input_map(&demo1_stage());

        let forward = map.resolve("move_forward");
        let right = map.resolve("move_right");
        let look = map.resolve("look");
        assert!(map.action(forward).is_some(), "move_forward unbound");
        assert!(map.action(right).is_some(), "move_right unbound");
        assert!(map.action(look).is_some(), "look unbound");

        assert_eq!(map.action(forward).unwrap().kind, InputKind::Axis);
        assert_eq!(map.action(right).unwrap().kind, InputKind::Axis);
        assert_eq!(map.action(look).unwrap().kind, InputKind::Axis2);

        // The move axes bind opposite keys with opposite scales.
        let fwd_sources: Vec<_> = map
            .action(forward)
            .unwrap()
            .bindings
            .iter()
            .map(|b| b.source.as_str())
            .collect();
        assert!(fwd_sources.contains(&"Key/W"));
        assert!(fwd_sources.contains(&"Key/S"));
    }

    #[test]
    fn missing_input_map_is_non_fatal_empty() {
        // A stage path whose project has no input_map.toml → empty map, no panic.
        let nowhere = PathBuf::from("/tmp/idtx-nonexistent-project/stages/s.usda");
        let map = load_input_map(&nowhere);
        assert_eq!(
            map.resolve("move_forward"),
            idtx_behavior::abi::InputActionId::UNRESOLVED
        );
    }

    /// StageLoader opens Demo1 asynchronously; try_recv is always non-blocking.
    ///
    /// Requires a GPU adapter + `OpenUSD` installation; skipped in CI unless the
    /// test runner sets IDTX_USD_DIR and passes `--include-ignored`.
    #[test]
    #[ignore]
    fn stage_loader_opens_demo1_headless() {
        use std::task::Poll;

        let (device, queue) = headless_device().expect("headless GPU");
        let stage = demo1_stage();

        let loader = StageLoader::request(
            &stage,
            device,
            queue,
            [640, 360],
            idtx_studio_session::TextureDecodeConfig::FULL_RESOLUTION,
            RenderWorldUnitScale::METRE,
            "",
        );

        // First call must return immediately (non-blocking); stage won't be ready
        // in sub-millisecond time.
        assert!(
            matches!(loader.try_recv(), Poll::Pending),
            "try_recv must not block"
        );

        // Poll until ready with a hard timeout so a failure doesn't stall CI.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let session = loop {
            match loader.try_recv() {
                Poll::Ready(Ok(s)) => break s,
                Poll::Ready(Err(e)) => panic!("stage load failed: {e}"),
                Poll::Pending => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "StageLoader timed out after 120 s"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };

        // The opened session must expose the prims in the stage.
        let prim_count = session.stage().topology().len();
        assert!(
            prim_count > 0,
            "Demo1 stage opened with zero prims — USD bridge may not be initialised"
        );
    }
}
