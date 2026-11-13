//! `cargo xtask` — workspace automation.
//!
//! Subcommands:
//!
//! * `bundle-mac` — build and package `idtx-inspector`, `idtx-poser`,
//!   and/or `idtx-conductor` into self-contained macOS `.app` bundles under
//!   `dist/macos/`. Conductor additionally co-locates `idtx-runtime`
//!   (spawned from `current_exe().parent()`) in `Contents/MacOS/` and stages
//!   the workspace `assets/` and the `built-in/` player tree there.
//! * `bundle-windows` — build and package `idtx-inspector`, `idtx-poser`,
//!   and/or `idtx-conductor` into self-contained Windows bundles (portable
//!   folder + zip) under `dist/windows/` (or `--out <dir>`). The bundle contains
//!   the `.exe`, all `OpenUSD`/`MaterialX`/TBB DLLs, the `usd/` and `plugin/`
//!   plugin-discovery trees, the `MaterialX` `libraries/` and `resources/`
//!   data, and the workspace `assets/`. Conductor additionally bundles
//!   `idtx-runtime.exe` (spawned at runtime from `current_exe().parent()`).
//! * `build-behavior <src.rs> [--out <dir>]` — compile a single-file
//!   behavior to `wasm32-unknown-unknown` through the embedded
//!   `idtx-behavior-build` skeleton.
//!
//! # Why an xtask (not a `build.rs`)
//!
//! Bundling must run **after** the binary is linked so the freshly built
//! executable can be copied into the bundle. A `build.rs` runs *before* its
//! own binary is produced, so it can only ever see the *previous* build's
//! artifact. An xtask runs as an ordinary post-build step and sidesteps that
//! timing problem entirely.
//!
//! # What a bundle needs (macOS, self-contained, ad-hoc signed)
//!
//! ```text
//! idtx-inspector.app/
//! └── Contents/
//!     ├── Info.plist                 CFBundleExecutable / identifier / icon
//!     ├── MacOS/idtx-inspector       the compiled binary
//!     ├── Resources/idtx-inspector.icns
//!     └── Frameworks/
//!         ├── libusd_ms.dylib        OpenUSD monolithic lib
//!         ├── libtbb*.dylib          Intel TBB deps (incl. symlink chain)
//!         ├── libMaterialX*.dylib    MaterialX deps
//!         └── usd/                   plugInfo.json resource + plugin trees
//! ```
//!
//! Conductor's `.app` additionally carries, beside its binary in `MacOS/`:
//! `idtx-runtime` (spawned via `current_exe().parent()`), `assets/` (controller
//! meshes), and `built-in/` (the player stage + rigs + behaviors).
//!
//! ## Dylib resolution (macOS)
//! The build script bakes an absolute `-rpath <install>/lib` into the
//! binary for `cargo run` ergonomics. The bundler replaces it with
//! `@executable_path/../Frameworks` so the loader finds the bundled dylibs
//! regardless of where the `.app` is moved.
//!
//! ## USD plugin discovery (macOS)
//! `OpenUSD` discovers `plugInfo.json` plugin trees relative to the directory
//! that contains `libusd_ms.dylib`. We place `usd/` as a **sibling** of
//! the dylib inside `Frameworks/`, mirroring the upstream install layout
//! `<prefix>/lib/{libusd_ms.dylib,usd/}`. Discovery then works with no
//! environment variable and no explicit `PlugRegistry::RegisterPlugins`.
//!
//! # What a bundle needs (Windows, self-contained, portable folder)
//!
//! ```text
//! dist/windows/idtx-inspector/
//! ├── idtx-inspector.exe             icon already embedded via winresource
//! ├── usd_ms.dll                     OpenUSD monolithic lib  (from <usd>/lib/)
//! ├── MaterialX*.dll  (13)           MaterialX libs          (from <usd>/bin/)
//! ├── tbb12.dll  tbb.dll  tbbmalloc.dll  tbbmalloc_proxy.dll (from <usd>/bin/ + lib/)
//! ├── usd/                           plugInfo.json discovery tree  (<usd>/lib/usd)
//! ├── plugin/                        usdShaders plugin tree        (<usd>/plugin)
//! ├── libraries/                     MaterialX .mtlx stdlib        (<usd>/libraries)
//! ├── resources/                     MaterialX render data         (<usd>/resources)
//! ├── assets/                        controller meshes             (<workspace>/assets)
//! ├── idtx_http_resolver.dll         (--remote) HTTP resolver, beside the exe
//! │                                  so the loader resolves its load-time import
//! └── idtx-http-resolver/            (--remote) plugInfo.json (LibraryPath emptied)
//!
//! dist/windows/idtx-conductor/               … same, PLUS:
//! └── idtx-runtime.exe               spawned via current_exe().parent()
//! ```
//!
//! With `--android`, Conductor additionally carries a cross-compiled Android
//! runtime tree so a shipped Conductor can package a Quest/Android native app:
//!
//! ```text
//! dist/windows/idtx-conductor/
//! └── android-runtime/
//!     ├── arm64-v8a/                 libidtx_runtime.so + native .so deps + OpenXR loader
//!     ├── usd/                       OpenUSD plugInfo discovery trees
//!     ├── usd-exec/                  (--openexec) coded OpenExec plugin
//!     └── idtx-http-resolver/        (--remote) HTTP USD resolver plugin
//! ```
//!
//! The Android cross-builds are driven by `build-usd-android` +
//! `build-quest-runtime`; `bundle-mac --visionos` is the Vision Pro twin (device
//! slice, `visionos-runtime/`). External toolchains (NDK, Android SDK, JDK,
//! Xcode, rust, cargo) are needed to *produce* / later *package* these and are
//! never bundled.
//!
//! ## DLL discovery (Windows)
//! Windows resolves DLLs from the executable's directory; no rpath equivalent
//! is needed. `OpenUSD` discovers `plugInfo.json` relative to `usd_ms.dll`, so
//! placing `usd/` as a sibling of the DLL (mirroring the install layout) means
//! plugin discovery works with no env var.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Headless render performance harness — only compiled with `--features perf-capture`.
#[cfg(feature = "perf-capture")]
mod perf_capture;

mod heavy_stage;
mod review_matrix;

/// Regenerate the `OpenUSD` schema plugin resources for `idtx-usd-extension` by
/// running upstream `usdGenSchema` from a Python-enabled install
/// (`$IDTX_USD_GEN_DIR`) — `gen-schema`.
mod gen_schema;

/// `OpenUSD`/`MaterialX` cross-build toolchain for the standalone targets
/// (`build-usd-android` / `build-usd-visionos`).
mod usd_cross;

/// Cross-build `idtx-runtime` → `libidtx_runtime.so` for Quest and stage it with
/// its native deps (`build-quest-runtime`).
mod quest_runtime;

/// Cross-build `idtx-runtime` → `libidtx_runtime.dylib` for Apple Vision Pro and
/// stage it with its native deps (`build-visionos-runtime`).
mod visionos_runtime;

/// Build the simulator-slice `idtx-runtime` and run it in the visionOS Simulator
/// (`run-visionos-sim`) — Mac-only testing with no device.
mod visionos_sim;

/// Build the device-slice `idtx-runtime`, assemble a signed `.app`, and deploy it
/// to a physical Vision Pro via `devicectl` (`run-visionos-device`) — the free
/// Personal-Team on-device path. Behind the `visionos-device` feature (it links
/// the `OpenUSD` runtime via `idtx-build`); run with `cargo xtask-visionos`.
#[cfg(feature = "visionos-device")]
mod visionos_device;

/// Generate a throwaway visionOS project and run `xcodebuild` with the
/// provisioning flags to mint the free Personal-Team cert + development profile +
/// register the device (`bootstrap-visionos-signing`). Behind `visionos-device`.
#[cfg(feature = "visionos-device")]
mod visionos_bootstrap;

// Instrumented allocator so `perf-capture --alloc` can report allocation volume.
// Only installed in the perf harness build; never in the default xtask binary.
#[cfg(feature = "perf-capture")]
#[global_allocator]
pub(crate) static GLOBAL: &stats_alloc::StatsAlloc<std::alloc::System> =
    &stats_alloc::INSTRUMENTED_SYSTEM;

/// `cargo xtask` — IDTX Studio workspace automation.
#[derive(Parser, Debug)]
#[command(
    name = "xtask",
    bin_name = "cargo xtask",
    about = "IDTX Studio workspace automation",
    long_about = None,
    disable_help_subcommand = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run locked core/USD/cross/Windows/server evidence lanes.
    VerifyReview(review_matrix::VerifyReviewOptions),
    /// Build and package inspector/poser/conductor into self-contained macOS
    /// .app bundles under dist/macos/ (conductor co-locates idtx-runtime
    /// + assets/ + built-in/).
    BundleMac(MacBundleArgs),
    /// Build and package inspector/poser/conductor into self-contained Windows
    /// bundles (portable folder + zip) under dist/windows/.
    BundleWindows(WinBundleArgs),
    /// Compile a single-file behavior to wasm via the idtx-behavior-build
    /// skeleton (CI wasm-target check).
    BuildBehavior(BuildBehaviorArgs),
    /// Cross-build the `OpenUSD`/oneTBB/`MaterialX` runtime for Quest
    /// (aarch64-linux-android) into thirdparty/. Needs the NDK
    /// (ANDROID_NDK_ROOT).
    BuildUsdAndroid(usd_cross::CrossOptions),
    /// Cross-build the `OpenUSD`/oneTBB/`MaterialX` runtime for Vision Pro
    /// (visionOS). macOS only.
    BuildUsdVisionos(usd_cross::CrossOptions),
    /// Cross-build idtx-runtime for Quest into libidtx_runtime.so and stage it
    /// with its native deps + the `OpenXR` loader under
    /// target/<profile>/android-runtime/arm64-v8a/.
    BuildQuestRuntime(quest_runtime::QuestRuntimeOptions),
    /// Cross-build idtx-runtime for Apple Vision Pro into
    /// libidtx_runtime.dylib and stage it with its native deps + the USD
    /// plugInfo trees under target/<profile>/visionos-runtime/arm64/. macOS only.
    BuildVisionosRuntime(visionos_runtime::VisionOsRuntimeOptions),
    /// Build the simulator-slice idtx-runtime and run it in the visionOS
    /// Simulator (no device). macOS only.
    RunVisionosSim(visionos_sim::RunSimOptions),
    /// Build the device-slice idtx-runtime, assemble a signed .app, and
    /// install+launch it on a physical Vision Pro via devicectl. Uses a FREE
    /// Personal-Team identity. Run via `cargo xtask-visionos`. macOS only.
    #[cfg(feature = "visionos-device")]
    RunVisionosDevice(visionos_device::RunDeviceOptions),
    /// Copy the on-device render-metrics dumps out of the immersive app's data
    /// container via devicectl. Run via `cargo xtask-visionos`. macOS only.
    #[cfg(feature = "visionos-device")]
    PullVisionosMetrics(visionos_device::PullMetricsOptions),
    /// Mint the free Personal-Team cert + development provisioning profile +
    /// register the paired Vision Pro. One-time setup for run-visionos-device.
    /// Run via `cargo xtask-visionos`. macOS only.
    #[cfg(feature = "visionos-device")]
    BootstrapVisionosSigning(visionos_bootstrap::BootstrapOptions),
    /// Write a deterministic N-cube synthetic heavy stage (default 2500) to
    /// testfiles/usd/heavy_grid_<N>.usda.
    GenHeavy(GenHeavyArgs),
    /// Regenerate the IDTX schema crates by running usdGenSchema from a
    /// Python-enabled `OpenUSD` install ($IDTX_USD_GEN_DIR).
    GenSchema(gen_schema::GenSchemaOptions),
    /// Headless render perf harness (render/session/both). Requires
    /// --features perf-capture.
    PerfCapture(PerfCaptureArgs),

    // When the `visionos-device` feature is off, the three device commands are
    // still recognized so they produce the helpful "needs the feature" error
    // (run via `cargo xtask-visionos`) rather than clap's generic
    // "unrecognized subcommand".
    /// [needs --features visionos-device] Build + sign + deploy to a physical
    /// Vision Pro. Run via `cargo xtask-visionos`.
    #[cfg(not(feature = "visionos-device"))]
    #[command(name = "run-visionos-device", disable_help_flag = true)]
    RunVisionosDevice {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        rest: Vec<String>,
    },
    /// [needs --features visionos-device] Pull on-device render-metrics dumps.
    /// Run via `cargo xtask-visionos`.
    #[cfg(not(feature = "visionos-device"))]
    #[command(name = "pull-visionos-metrics", disable_help_flag = true)]
    PullVisionosMetrics {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        rest: Vec<String>,
    },
    /// [needs --features visionos-device] Mint the free Personal-Team signing
    /// identity + provisioning profile. Run via `cargo xtask-visionos`.
    #[cfg(not(feature = "visionos-device"))]
    #[command(name = "bootstrap-visionos-signing", disable_help_flag = true)]
    BootstrapVisionosSigning {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        rest: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::VerifyReview(opts) => review_matrix::run(opts),
        Commands::BundleMac(args) => bundle_mac_cmd(args),
        Commands::BundleWindows(args) => bundle_windows_cmd(args),
        Commands::BuildBehavior(args) => build_behavior_cmd(args),
        Commands::BuildUsdAndroid(opts) => usd_cross::build_usd_android_cmd(&opts),
        Commands::BuildUsdVisionos(opts) => usd_cross::build_usd_visionos_cmd(&opts),
        Commands::BuildQuestRuntime(opts) => quest_runtime::build_quest_runtime_cmd(&opts),
        Commands::BuildVisionosRuntime(opts) => {
            visionos_runtime::build_visionos_runtime_cmd(&opts)
        }
        Commands::RunVisionosSim(opts) => visionos_sim::run_visionos_sim_cmd(&opts),
        #[cfg(feature = "visionos-device")]
        Commands::RunVisionosDevice(opts) => visionos_device::run_visionos_device_cmd(&opts),
        #[cfg(feature = "visionos-device")]
        Commands::PullVisionosMetrics(opts) => visionos_device::pull_visionos_metrics_cmd(&opts),
        #[cfg(feature = "visionos-device")]
        Commands::BootstrapVisionosSigning(opts) => {
            visionos_bootstrap::bootstrap_visionos_signing_cmd(&opts)
        }
        #[cfg(not(feature = "visionos-device"))]
        Commands::RunVisionosDevice { .. } => bail_visionos_device_feature("run-visionos-device"),
        #[cfg(not(feature = "visionos-device"))]
        Commands::PullVisionosMetrics { .. } => {
            bail_visionos_device_feature("pull-visionos-metrics")
        }
        #[cfg(not(feature = "visionos-device"))]
        Commands::BootstrapVisionosSigning { .. } => {
            bail_visionos_device_feature("bootstrap-visionos-signing")
        }
        Commands::GenHeavy(args) => gen_heavy_cmd(args),
        Commands::GenSchema(opts) => gen_schema::gen_schema_cmd(&opts),
        Commands::PerfCapture(args) => perf_capture_cmd(args),
    }
}

/// The error produced when a `visionos-device` subcommand is invoked on a build
/// that did not enable the feature (it links the `OpenUSD` runtime).
#[cfg(not(feature = "visionos-device"))]
fn bail_visionos_device_feature(cmd: &str) -> Result<()> {
    anyhow::bail!(
        "`{cmd}` needs the `visionos-device` feature (it links the OpenUSD runtime). \
         Run it with:\n    \
         cargo xtask-visionos {cmd} [...]\n\
         (and ensure DYLD_LIBRARY_PATH includes the OpenUSD install's lib dir, unless the \
         binary was built with IDTX_USD_DIR set so the rpath is baked in)."
    )
}

// ---------------------------------------------------------------------------
// clap arg structs for subcommands defined in this file
// ---------------------------------------------------------------------------

/// `cargo xtask build-behavior <behavior.rs> [--out <dir>]` — compile a
/// single-file behavior to `wasm32-unknown-unknown`.
#[derive(Args, Debug)]
pub struct BuildBehaviorArgs {
    /// Path to the behavior `.rs` source file.
    #[arg(value_name = "SRC")]
    src: PathBuf,
    /// Output directory for the emitted `<name>.wasm` (default: next to the source).
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
}

/// `cargo xtask gen-heavy [N] [--out <path>]` — write a deterministic N-mesh
/// synthetic `.usda` stage.
#[derive(Args, Debug)]
pub struct GenHeavyArgs {
    /// Number of meshes in the synthetic grid (default 2500).
    #[arg(value_name = "N", default_value_t = 2500)]
    count: u32,
    /// Output path (default: testfiles/usd/heavy_grid_<N>.usda).
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

/// Render mode for `perf-capture`.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfMode {
    Render,
    Session,
    Both,
}

/// `cargo xtask perf-capture [options]` — headless render performance harness.
///
/// The fields are parsed unconditionally (so the subcommand is always
/// recognized), but the harness itself only runs with `--features perf-capture`.
#[derive(Args, Debug)]
pub struct PerfCaptureArgs {
    /// USD file to measure (repeatable). Defaults to a heavy real-asset list
    /// (existing files only).
    #[arg(long = "scene", value_name = "PATH")]
    pub scene: Vec<PathBuf>,
    /// Generate + measure a synthetic N-cube stage first.
    #[arg(long = "gen-heavy", value_name = "N")]
    pub gen_heavy: Option<u32>,
    /// Render resolution (default 1600x900).
    #[arg(long, default_value = "1600x900", value_name = "WxH")]
    pub res: String,
    /// Warm frames per scene, render mode (default 30).
    #[arg(long, default_value_t = 30, value_name = "N")]
    pub warm: usize,
    /// Frames per scene, session mode (default 30).
    #[arg(long, default_value_t = 30, value_name = "N")]
    pub frames: usize,
    /// render | session | both (default render).
    #[arg(long, value_enum, default_value = "render")]
    pub mode: PerfMode,
    /// Output path (default testfiles/renders/perf_capture.json).
    #[arg(long, value_name = "JSON")]
    pub out: Option<PathBuf>,
    /// Also dump the last frame of each scene as PNG.
    #[arg(long)]
    pub png: bool,
    /// Report allocation volume via stats_alloc.
    #[arg(long)]
    pub alloc: bool,
}

// ---------------------------------------------------------------------------
// gen-heavy subcommand — synthetic reproducible heavy stage generator
// ---------------------------------------------------------------------------

/// `cargo xtask gen-heavy [N] [--out <path>]` — write a deterministic N-mesh
/// `.usda` (grid of cubes with distinct materials) into `testfiles/usd/`.
///
/// This reproduces the old ~2161-mesh regression target as a tiny, committed,
/// text-only stage instead of a large binary asset. Available without the
/// `perf-capture` feature (it only writes text).
fn gen_heavy_cmd(args: GenHeavyArgs) -> Result<()> {
    let GenHeavyArgs { count, out } = args;
    let path = out.unwrap_or_else(|| {
        heavy_stage::default_stage_dir().join(format!("heavy_grid_{count}.usda"))
    });
    heavy_stage::write_heavy_grid(&path, count)?;
    println!("wrote {count}-mesh synthetic stage → {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// perf-capture subcommand — dispatch (real impl behind the feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "perf-capture")]
fn perf_capture_cmd(args: PerfCaptureArgs) -> Result<()> {
    perf_capture::run(args)
}

#[cfg(not(feature = "perf-capture"))]
fn perf_capture_cmd(_args: PerfCaptureArgs) -> Result<()> {
    bail!(
        "perf-capture is not compiled in.\n\
         Rebuild the harness with the feature enabled, e.g.:\n\
         \x20   cargo run -p xtask --features perf-capture -- perf-capture --help"
    )
}

// ---------------------------------------------------------------------------
// build-behavior subcommand
// ---------------------------------------------------------------------------

fn build_behavior_cmd(args: BuildBehaviorArgs) -> Result<()> {
    let BuildBehaviorArgs { src, out } = args;
    let src_path = src;
    let out_dir = out;

    let workspace_root = workspace_root()?;
    let out_dir = out_dir.unwrap_or_else(|| {
        src_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    let sdk_path = workspace_root.join("crates").join("idtx-behavior-sdk");

    let opts = idtx_behavior_build::BuildOptions::new(&out_dir, &sdk_path);
    let artifact = idtx_behavior_build::build_behavior(&src_path, &opts)
        .map_err(|e| anyhow::anyhow!("behavior build failed: {e}"))?;

    let cached = if artifact.cache_hit {
        " (cache hit)"
    } else {
        ""
    };
    println!(
        "built {} → {}{}",
        artifact.name,
        artifact.wasm_path.display(),
        cached
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// BundleSpec — data-driven description of each bundleable app
// ---------------------------------------------------------------------------

/// Static description of a bundleable app (macOS and/or Windows).
///
/// All mechanical steps are shared by [`bundle_app`] (macOS) and
/// [`bundle_windows_app`] (Windows). Adding a new target requires only a new
/// constructor.
struct BundleSpec {
    /// Cargo package name (`-p <package>`). Also used as the executable name,
    /// `.app` directory stem, `.icns` file stem, and `CFBundleExecutable`.
    package: &'static str,
    /// Human-readable label for `CFBundleName` / `CFBundleDisplayName`.
    display_name: &'static str,
    /// Reverse-DNS `CFBundleIdentifier`.
    bundle_identifier: &'static str,
    /// Workspace-relative path to the crate's `.ico` for icon conversion.
    /// Missing is non-fatal — the bundle silently falls back to the system
    /// default icon.
    icon_relpath: &'static str,
    /// Whether `idtx-runtime` must be built and co-located next to this app's
    /// executable. True only for Conductor, which spawns the runtime from
    /// `current_exe().parent()` for "Preview in Runtime".
    bundles_runtime: bool,
    /// The Cargo feature that enables the PC-VR / `OpenXR` path for this package,
    /// if it has one. `--pcvr` adds `--features <feature>` only for specs that
    /// declare it; packages without a VR path (inspector, poser) leave this
    /// `None` so the flag is a no-op for them rather than a build error.
    pcvr_feature: Option<&'static str>,
    /// The Cargo feature that enables the `OpenExec` dataflow subsystem for this
    /// package, if it declares one. `--openexec` adds `--features <feature>` only
    /// for specs that declare it. Only the co-located desktop `idtx-runtime`
    /// declares an `openexec` feature; the editors leave this `None` (inspector /
    /// poser have no `OpenExec` path, and `idtx-conductor` gets cook-side `OpenExec`
    /// via its always-on `cooking → idtx-build/openexec`, not a binary feature).
    openexec_feature: Option<&'static str>,
}

impl BundleSpec {
    fn inspector() -> Self {
        Self {
            package: "idtx-inspector",
            display_name: "IDTX Inspector",
            bundle_identifier: "com.sap.apeirora.idtx.inspector",
            icon_relpath: "apps/idtx-inspector/idtx_inspector_256.ico",
            bundles_runtime: false,
            pcvr_feature: None,
            openexec_feature: None,
        }
    }

    fn poser() -> Self {
        Self {
            package: "idtx-poser",
            display_name: "IDTX Poser",
            bundle_identifier: "com.sap.apeirora.idtx.poser",
            icon_relpath: "apps/idtx-poser/idtx_poser_256.ico",
            bundles_runtime: false,
            pcvr_feature: None,
            openexec_feature: None,
        }
    }

    fn conductor() -> Self {
        Self {
            package: "idtx-conductor",
            display_name: "IDTX Conductor",
            bundle_identifier: "com.sap.apeirora.idtx.conductor",
            icon_relpath: "apps/idtx-conductor/idtx_conductor_256.ico",
            bundles_runtime: true,
            pcvr_feature: Some("pcvr"),
            // Conductor carries cook-side OpenExec via its always-on `cooking`
            // feature (`idtx-build/openexec`); it exposes no binary `openexec`
            // feature of its own, so `--openexec` is a no-op for the editor binary
            // (it still flows to the co-located runtime + the standalone builds).
            openexec_feature: None,
        }
    }

    /// Build-only spec for `idtx-runtime` — used when bundling Conductor
    /// (which must co-locate the runtime binary). Not a first-class bundle
    /// target itself.
    fn runtime() -> Self {
        Self {
            package: "idtx-runtime",
            display_name: "IDTX Runtime",
            bundle_identifier: "com.sap.apeirora.idtx.runtime",
            icon_relpath: "",
            bundles_runtime: false,
            pcvr_feature: Some("pcvr"),
            openexec_feature: Some("openexec"),
        }
    }

    fn app_dir_name(&self) -> String {
        format!("{}.app", self.package)
    }

    fn icns_name(&self) -> String {
        format!("{}.icns", self.package)
    }
}

// ---------------------------------------------------------------------------
// BundleOptions — resolved CLI flags common to all bundle-* subcommands
// ---------------------------------------------------------------------------

/// Resolved common bundle flags, built from [`CommonBundleArgs`] (+ `pcvr`,
/// which only `bundle-windows` exposes). The bundling pipeline
/// ([`bundle_app`], [`build_binary`]) reads these.
struct BundleOptions {
    debug: bool,
    remote: bool,
    pcvr: bool,
    usd_dir: Option<PathBuf>,
    /// Enable the `OpenExec` dataflow subsystem in what this bundle builds: the
    /// co-located desktop `idtx-runtime` (via [`BundleSpec::openexec_feature`]) and
    /// — when `--android`/`--visionos` are set — the standalone cross-builds.
    openexec: bool,
    /// Build + stage the cross-compiled Android (Quest) runtime tree into the
    /// conductor bundle. Honored by both bundlers.
    android: bool,
    /// Force the heavyweight Android `OpenUSD` cross-build even when a complete
    /// install is already staged.
    android_rebuild_usd: bool,
    /// Build + stage the cross-compiled visionOS runtime tree into the conductor
    /// bundle. **macOS only** — set solely by `bundle-mac --visionos`; always
    /// `false` on Windows (no such flag there).
    visionos: bool,
    /// Force the heavyweight visionOS `OpenUSD` cross-build even when a complete
    /// install is already staged. macOS only.
    visionos_rebuild_usd: bool,
}

impl BundleOptions {
    fn profile(&self) -> &'static str {
        if self.debug { "debug" } else { "release" }
    }
}

/// Which app a `--app` flag selects. `All` expands to every app.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum AppSel {
    Inspector,
    Poser,
    Conductor,
    All,
}

/// Common `--debug`/`--release`/`--remote`/`--usd-dir` flags shared by
/// `bundle-mac` and `bundle-windows`. Flattened into each subcommand's args.
#[derive(Args, Debug)]
struct CommonBundleArgs {
    /// Bundle the dev (debug) build instead of release. Overrides an earlier
    /// `--release`.
    #[arg(long, overrides_with = "release")]
    debug: bool,
    /// Bundle the release build (the default). Overrides an earlier `--debug`
    /// (last profile flag wins).
    #[arg(long, overrides_with = "debug")]
    release: bool,
    /// Enable the `remote` feature (HTTP resolver plugin); also bundles the
    /// resolver plugin into the bundle.
    #[arg(long)]
    remote: bool,
    /// Enable the `OpenExec` dataflow subsystem (`--features openexec`) in what this
    /// bundle builds: the co-located desktop `idtx-runtime`, and — with
    /// `--android`/`--visionos` — the standalone cross-builds (the `OpenUSD`
    /// cross-build is configured with `PXR_BUILD_EXEC=ON` and the runtime with
    /// `--with-openexec`). A no-op for inspector/poser and for the conductor
    /// binary itself (which carries cook-side `OpenExec` via its `cooking` feature).
    #[arg(long)]
    openexec: bool,
    /// Build the cross-compiled Android (Quest) runtime and stage it into the
    /// conductor bundle under `android-runtime/`, so a shipped conductor can
    /// package an Android native app. Cross-compiles `OpenUSD` for Android (reusing
    /// an already-staged install) and `idtx-runtime` → `libidtx_runtime.so`. Needs
    /// `ANDROID_NDK_ROOT` + the `aarch64-linux-android` Rust target. External
    /// toolchains (JDK/SDK/NDK) are not bundled.
    #[arg(long)]
    android: bool,
    /// Force the Android `OpenUSD` cross-build even when a complete install is
    /// already staged (otherwise it is reused). Only meaningful with `--android`.
    #[arg(long = "android-rebuild-usd")]
    android_rebuild_usd: bool,
    /// `OpenUSD` install prefix. Defaults to $IDTX_USD_DIR, then the workspace
    /// default.
    #[arg(long = "usd-dir", value_name = "PATH")]
    usd_dir: Option<PathBuf>,
}

impl CommonBundleArgs {
    /// Resolve into the shared [`BundleOptions`]. `pcvr` is passed separately
    /// (only `bundle-windows` exposes `--pcvr`; it is always `false` on macOS).
    /// The two `visionos*` fields are left `false` here — only `bundle-mac`
    /// populates them from its own `--visionos` flags.
    fn into_options(self, pcvr: bool) -> BundleOptions {
        BundleOptions {
            // `--debug` / `--release` are mutually exclusive via overrides_with,
            // so at most one is set; default (neither) is release.
            debug: self.debug && !self.release,
            remote: self.remote,
            pcvr,
            usd_dir: self.usd_dir,
            openexec: self.openexec,
            android: self.android,
            android_rebuild_usd: self.android_rebuild_usd,
            visionos: false,
            visionos_rebuild_usd: false,
        }
    }
}

// ---------------------------------------------------------------------------
// MacApp / MacBundleOptions — CLI surface for bundle-mac
// ---------------------------------------------------------------------------

/// Which app to bundle on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacApp {
    Inspector,
    Poser,
    Conductor,
}

impl MacApp {
    fn spec(self) -> BundleSpec {
        match self {
            MacApp::Inspector => BundleSpec::inspector(),
            MacApp::Poser => BundleSpec::poser(),
            MacApp::Conductor => BundleSpec::conductor(),
        }
    }

    fn all() -> &'static [MacApp] {
        &[MacApp::Inspector, MacApp::Poser, MacApp::Conductor]
    }
}

/// Expand `--app` selections into the concrete [`MacApp`] list: `All` fans out
/// to every app, an empty selection defaults to all, and consecutive duplicates
/// are removed (so `--app all --app conductor` bundles each app once).
fn resolve_mac_apps(sel: &[AppSel]) -> Vec<MacApp> {
    let mut apps: Vec<MacApp> = Vec::new();
    for s in sel {
        match s {
            AppSel::Inspector => apps.push(MacApp::Inspector),
            AppSel::Poser => apps.push(MacApp::Poser),
            AppSel::Conductor => apps.push(MacApp::Conductor),
            AppSel::All => apps.extend_from_slice(MacApp::all()),
        }
    }
    if apps.is_empty() {
        apps.extend_from_slice(MacApp::all());
    }
    apps.dedup();
    apps
}

/// CLI arguments for `bundle-mac`.
#[derive(Args, Debug)]
pub struct MacBundleArgs {
    /// Which app(s) to bundle: inspector | poser | conductor | all (default:
    /// all). Repeat the flag to bundle multiple apps.
    #[arg(long = "app", value_enum, value_name = "NAME")]
    app: Vec<AppSel>,
    /// Build + stage the cross-compiled visionOS runtime into the conductor
    /// bundle under `Contents/MacOS/visionos-runtime/`, so a shipped conductor can
    /// package a Vision Pro `.app`. Cross-compiles `OpenUSD` for visionOS (reusing an
    /// already-staged install) and `idtx-runtime` → `libidtx_runtime.dylib` (device
    /// slice). macOS only (needs the visionOS SDK).
    #[arg(long)]
    visionos: bool,
    /// Force the visionOS `OpenUSD` cross-build even when a complete install is
    /// already staged (otherwise it is reused). Only meaningful with `--visionos`.
    #[arg(long = "visionos-rebuild-usd")]
    visionos_rebuild_usd: bool,
    #[command(flatten)]
    common: CommonBundleArgs,
}

impl MacBundleArgs {
    fn into_options(self) -> MacBundleOptions {
        let mut base = self.common.into_options(false);
        base.visionos = self.visionos;
        base.visionos_rebuild_usd = self.visionos_rebuild_usd;
        MacBundleOptions {
            apps: resolve_mac_apps(&self.app),
            base,
        }
    }
}

/// Resolved options specific to `bundle-mac` (extends the shared [`BundleOptions`]).
struct MacBundleOptions {
    /// Apps to bundle (`--app inspector|poser|conductor|all`; default: all).
    apps: Vec<MacApp>,
    /// Common options (debug, remote, usd-dir).
    base: BundleOptions,
}

// ---------------------------------------------------------------------------
// WinBundleOptions — extended CLI flags for bundle-windows
// ---------------------------------------------------------------------------

/// Which app to bundle on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WinApp {
    Inspector,
    Poser,
    Conductor,
}

impl WinApp {
    fn spec(self) -> BundleSpec {
        match self {
            WinApp::Inspector => BundleSpec::inspector(),
            WinApp::Poser => BundleSpec::poser(),
            WinApp::Conductor => BundleSpec::conductor(),
        }
    }

    fn all() -> &'static [WinApp] {
        &[WinApp::Inspector, WinApp::Poser, WinApp::Conductor]
    }
}

/// Expand `--app` selections into the concrete [`WinApp`] list (see
/// [`resolve_mac_apps`] for the `All`/default/dedup semantics).
fn resolve_win_apps(sel: &[AppSel]) -> Vec<WinApp> {
    let mut apps: Vec<WinApp> = Vec::new();
    for s in sel {
        match s {
            AppSel::Inspector => apps.push(WinApp::Inspector),
            AppSel::Poser => apps.push(WinApp::Poser),
            AppSel::Conductor => apps.push(WinApp::Conductor),
            AppSel::All => apps.extend_from_slice(WinApp::all()),
        }
    }
    if apps.is_empty() {
        apps.extend_from_slice(WinApp::all());
    }
    apps.dedup();
    apps
}

/// CLI arguments for `bundle-windows`.
#[derive(Args, Debug)]
pub struct WinBundleArgs {
    /// Which app(s) to bundle: inspector | poser | conductor | all (default:
    /// all). Repeat the flag to bundle multiple apps.
    #[arg(long = "app", value_enum, value_name = "NAME")]
    app: Vec<AppSel>,
    /// Build the `OpenXR` PC-VR path. Adds `--features pcvr` for packages that
    /// declare it (conductor + the co-located runtime); a no-op for
    /// inspector/poser.
    #[arg(long)]
    pcvr: bool,
    /// Output directory for bundles and zips (default: dist/windows/).
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// Produce only the bundle folder, skip the .zip archive.
    #[arg(long = "no-zip")]
    no_zip: bool,
    #[command(flatten)]
    common: CommonBundleArgs,
}

impl WinBundleArgs {
    fn into_options(self) -> WinBundleOptions {
        WinBundleOptions {
            apps: resolve_win_apps(&self.app),
            base: self.common.into_options(self.pcvr),
            out_dir: self.out,
            no_zip: self.no_zip,
        }
    }
}

/// Resolved options specific to `bundle-windows` (extends the shared [`BundleOptions`]).
struct WinBundleOptions {
    /// Apps to bundle (`--app inspector|poser|conductor|all`; default: all).
    apps: Vec<WinApp>,
    /// Common options (debug, remote, pcvr, usd-dir).
    base: BundleOptions,
    /// Output directory; default `<workspace>/dist/windows/`.
    out_dir: Option<PathBuf>,
    /// Produce only the bundle folder, skip the zip archive.
    no_zip: bool,
}

// ---------------------------------------------------------------------------
// bundle-mac subcommand — dispatch across inspector / poser / conductor
// ---------------------------------------------------------------------------

/// Entry point for `cargo xtask bundle-mac`.
///
/// Bundles each requested app into a self-contained macOS `.app` under
/// `dist/macos/` via the [`bundle_app`] pipeline. Conductor additionally
/// co-locates `idtx-runtime` and stages the `assets/` + `built-in/` trees.
fn bundle_mac_cmd(args: MacBundleArgs) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("bundle-mac produces macOS .app bundles and must run on macOS");
    }

    let opts = args.into_options();
    for app in &opts.apps {
        let spec = app.spec();
        bundle_app(&spec, &opts.base)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// bundle_app — generic macOS .app bundling pipeline
// ---------------------------------------------------------------------------

/// Full pipeline: build → skeleton → copy binary → copy USD runtime →
/// rpath fixups → icon → Info.plist → ad-hoc sign. When `spec.bundles_runtime`
/// (Conductor), also builds + co-locates `idtx-runtime` and stages the
/// `assets/` + `built-in/` trees into `Contents/MacOS/`.
fn bundle_app(spec: &BundleSpec, opts: &BundleOptions) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "bundle-{} produces a macOS .app and must run on macOS",
            spec.package
        );
    }

    let workspace_root = workspace_root()?;
    let usd_dir = resolve_usd_dir(opts.usd_dir.clone(), &workspace_root);
    let lib_dir = usd_dir.join("lib");
    if !lib_dir.join("libusd_ms.dylib").is_file() {
        bail!(
            "OpenUSD monolithic lib not found at {}.\n\
             Set --usd-dir or $IDTX_USD_DIR to a prebuilt install prefix.",
            lib_dir.join("libusd_ms.dylib").display()
        );
    }

    // 1. Build the binary.
    build_binary(&workspace_root, &usd_dir, spec, opts)?;

    // 1b. Build idtx-runtime alongside Conductor — it is spawned via
    //     `current_exe().parent()` and must sit in the same directory inside the
    //     bundle (Contents/MacOS/).
    if spec.bundles_runtime {
        let rt = BundleSpec::runtime();
        build_binary(&workspace_root, &usd_dir, &rt, opts)?;
    }

    let profile_dir = workspace_root.join("target").join(opts.profile());
    let binary = profile_dir.join(spec.package);
    if !binary.is_file() {
        bail!(
            "expected freshly built binary at {} but it is missing",
            binary.display()
        );
    }

    // 2. (Re)create the .app skeleton from a clean slate.
    //    The binary is built by cargo under `target/<profile>/`, but the shipped
    //    `.app` lands under `dist/macos/` (the Windows twin is `dist/windows/`),
    //    so the final artifacts for both platforms sit side-by-side under `dist/`.
    let dist_dir = workspace_root.join("dist").join("macos");
    fs::create_dir_all(&dist_dir)
        .with_context(|| format!("creating output directory {}", dist_dir.display()))?;
    let app_dir = dist_dir.join(spec.app_dir_name());
    let contents = app_dir.join("Contents");
    let macos_dir = contents.join("MacOS");
    let frameworks_dir = contents.join("Frameworks");
    let resources_dir = contents.join("Resources");
    if app_dir.exists() {
        fs::remove_dir_all(&app_dir)
            .with_context(|| format!("clearing old bundle {}", app_dir.display()))?;
    }
    for dir in [&macos_dir, &frameworks_dir, &resources_dir] {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    // 3. Copy the binary into Contents/MacOS/.
    let bundled_binary = macos_dir.join(spec.package);
    fs::copy(&binary, &bundled_binary)
        .with_context(|| format!("copying binary into {}", bundled_binary.display()))?;

    // 3b. Co-locate idtx-runtime next to the Conductor binary (Contents/MacOS/),
    //     mirroring the Windows bundle. Conductor spawns it via
    //     `current_exe().parent()` for "Preview in Runtime".
    let bundled_runtime = if spec.bundles_runtime {
        let rt_src = profile_dir.join("idtx-runtime");
        if !rt_src.is_file() {
            bail!(
                "expected idtx-runtime at {} but it is missing — \
                 Conductor needs it co-located for 'Preview in Runtime'",
                rt_src.display()
            );
        }
        let rt_dst = macos_dir.join("idtx-runtime");
        fs::copy(&rt_src, &rt_dst).context("copying idtx-runtime into bundle")?;
        Some(rt_dst)
    } else {
        None
    };

    // 4. Copy USD runtime dylibs (monolithic + TBB + MaterialX) into Frameworks/.
    let copied = copy_usd_runtime_libs(&lib_dir, &frameworks_dir)?;
    if copied == 0 {
        bail!(
            "no USD runtime dylibs matched under {} — bundle would not run",
            lib_dir.display()
        );
    }

    // 5. Copy the usd/ resource + plugin tree next to the dylibs so USD's
    //    PlugRegistry can discover plugins without any env vars.
    let usd_resource = lib_dir.join("usd");
    if !usd_resource.is_dir() {
        bail!(
            "USD resource tree not found at {} (plugin discovery would fail)",
            usd_resource.display()
        );
    }
    copy_dir_recursive(&usd_resource, &frameworks_dir.join("usd"))
        .context("copying usd/ resource tree into Frameworks")?;

    // 6. Rpath fixups.
    //
    //    The build script bakes `cargo:rustc-link-arg-bins=-Wl,-rpath,<lib>`
    //    into the binary for `cargo run` ergonomics. That absolute path must
    //    be replaced with a bundle-relative one before shipping.
    add_rpath(&bundled_binary, "@executable_path/../Frameworks")?;
    delete_rpath(&bundled_binary, &lib_dir.to_string_lossy())?;

    // The co-located runtime binary carries the same baked absolute-lib rpath;
    // rewire it to the bundle's Frameworks/ too (it is a sibling of the Conductor
    // binary in Contents/MacOS/, so @executable_path/../Frameworks is the same).
    if let Some(ref rt) = bundled_runtime {
        add_rpath(rt, "@executable_path/../Frameworks")?;
        delete_rpath(rt, &lib_dir.to_string_lossy())?;
    }

    // libusd_ms.dylib records @rpath/libtbb.12.dylib — its own directory
    // (@loader_path = Frameworks/) is where TBB siblings live.
    let bundled_usd = frameworks_dir.join("libusd_ms.dylib");
    add_rpath(&bundled_usd, "@loader_path")?;
    delete_rpath(&bundled_usd, &lib_dir.to_string_lossy())?;

    // MaterialX dylibs cross-reference each other through @rpath too.
    let materialx_dylibs = materialx_dylibs_in(&frameworks_dir)?;
    for dylib in &materialx_dylibs {
        add_rpath(dylib, "@loader_path")?;
        delete_rpath(dylib, &lib_dir.to_string_lossy())?;
    }

    // 6b. (--remote only) Bundle the HTTP resolver plugin.
    //
    //     The resolver is a C++ dylib + plugInfo.json built into a
    //     package-specific OUT_DIR. We copy both files into
    //     Contents/Frameworks/idtx-http-resolver/ and rewire rpaths so
    //     the binary finds the dylib via @executable_path/../Frameworks/
    //     idtx-http-resolver and the resolver finds libusd_ms via
    //     @loader_path/../ (the parent Frameworks/ dir where the USD
    //     dylibs live).
    let resolver_dylib = if opts.remote {
        // Both Conductor and the co-located idtx-runtime link the resolver and
        // must be rewired; otherwise the spawned preview runtime aborts in dyld.
        let mut deps: Vec<&Path> = vec![&bundled_binary];
        if let Some(ref rt) = bundled_runtime {
            deps.push(rt);
        }
        let dylib = bundle_resolver_plugin(&profile_dir, &lib_dir, &frameworks_dir, &deps)?;
        Some(dylib)
    } else {
        None
    };

    // 7. Icon: .ico → .icns into Resources/.
    let ico = workspace_root.join(spec.icon_relpath);
    let icns = resources_dir.join(spec.icns_name());
    let have_icon = convert_ico_to_icns(&ico, &icns, spec.package).unwrap_or_else(|e| {
        eprintln!("xtask: warning: icon conversion failed ({e}); bundle will use the default icon");
        false
    });

    // 8. Write Info.plist.
    write_info_plist(&contents.join("Info.plist"), spec, have_icon)?;

    // 8b. Stage exe-relative asset dirs into Contents/MacOS/ for Conductor, so
    //     `builtin_root()` (its packaged branch returns the exe dir) and the
    //     runtime manifest's `<exe>/assets` + `<exe>/built-in` search roots
    //     resolve — the macOS twin of the Windows bundle's assets/ + built-in/
    //     staging.
    if spec.bundles_runtime {
        let assets_src = workspace_root.join("assets");
        if assets_src.is_dir() {
            copy_dir_recursive(&assets_src, &macos_dir.join("assets"))
                .context("copying assets/ into bundle")?;
        }
        let builtin_src = workspace_root.join("crates/idtx-assets/built-in");
        if builtin_src.is_dir() {
            copy_dir_recursive(&builtin_src, &macos_dir.join("built-in"))
                .context("copying built-in/ player tree into bundle")?;
        }
    }

    // 8c. (--android / --visionos) Cross-build + stage the standalone runtime
    //     trees beside the Conductor binary (Contents/MacOS/android-runtime,
    //     …/visionos-runtime), so a shipped Conductor can package Quest / Vision
    //     Pro apps on-site. The cook's sibling lookup
    //     (`idtx_studio_shell::build_job::{quest,visionos}_runtime_binary`) reads
    //     `<exe_dir>/<platform>-runtime/{<abi>,usd,usd-exec,idtx-http-resolver}` —
    //     for a bundle the exe dir is Contents/MacOS/.
    //
    //     Opt-in (these cross-builds are heavyweight): `--android` builds the Quest
    //     tree, `--visionos` the Vision Pro tree. Both are conductor-only. The
    //     staged Mach-O visionOS slices are re-signed ad-hoc by the `--deep` pass
    //     in step 9 (like the usd/ plugin tree); Android `.so`s are ELF and ignored
    //     by codesign. External toolchains (NDK/JDK/SDK/Xcode) are never bundled.
    if spec.bundles_runtime {
        if opts.android {
            stage_android_runtime(&macos_dir, opts)?;
        }
        if opts.visionos {
            stage_visionos_runtime(&macos_dir, opts)?;
        }
    } else if opts.android || opts.visionos {
        eprintln!(
            "xtask: note: --android/--visionos are no-ops for {} (only idtx-conductor \
             packages standalone apps)",
            spec.package
        );
    }

    // 9. Ad-hoc code-sign everything we rewrote with install_name_tool.
    //
    //    install_name_tool edits invalidate a Mach-O's signature; on Apple
    //    silicon an invalid signature causes the kernel to SIGKILL the
    //    process at load time. Sign inside-out: individual dylibs first,
    //    then the whole .app with --deep (required because the usd/ plugin
    //    tree under Frameworks/ contains unsigned plugInfo.json files that
    //    a shallow bundle sign would reject).
    codesign_adhoc(&bundled_usd, false)?;
    for dylib in &materialx_dylibs {
        codesign_adhoc(dylib, false)?;
    }
    if let Some(ref dylib) = resolver_dylib {
        codesign_adhoc(dylib, false)?;
    }
    // The co-located runtime binary was rewritten by install_name_tool too; sign
    // it before the enclosing --deep bundle sign.
    if let Some(ref rt) = bundled_runtime {
        codesign_adhoc(rt, false)?;
    }
    codesign_adhoc(&app_dir, true)?;

    println!("\n\u{2705}  Bundled: {}", app_dir.display());
    println!("    Launch with:  open \"{}\"", app_dir.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS helpers — resolver plugin
// ---------------------------------------------------------------------------

/// Find the `OUT_DIR` of `idtx-usd-resolver-http` for the given profile by
/// scanning `target/<profile>/build/idtx-usd-resolver-http-*/out/`. Returns
/// the path to the directory that contains the built dylib + `plugInfo.json`.
fn find_resolver_out_dir(profile_dir: &Path) -> Result<PathBuf> {
    let build_dir = profile_dir.join("build");
    if !build_dir.is_dir() {
        bail!(
            "build dir {} not found — has `cargo build` run yet?",
            build_dir.display()
        );
    }
    for entry in
        fs::read_dir(&build_dir).with_context(|| format!("reading {}", build_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("idtx-usd-resolver-http-") {
            continue;
        }
        let out = entry.path().join("out");
        if out.join("libidtx_http_resolver.dylib").is_file() {
            return Ok(out);
        }
    }
    bail!(
        "could not find libidtx_http_resolver.dylib under {}/build/idtx-usd-resolver-http-*/out/\n\
         Did you build with --features remote (and IDTX_USD_DIR set)?",
        profile_dir.display()
    );
}

/// Bundle the HTTP resolver plugin into `Contents/Frameworks/idtx-http-resolver/`.
///
/// Steps:
/// 1. Locate the resolver's `OUT_DIR` (built by `idtx-usd-resolver-http`).
/// 2. Copy `libidtx_http_resolver.dylib` and `plugInfo.json` into
///    `Contents/Frameworks/idtx-http-resolver/`.
/// 3. Set the dylib's install name to `@rpath/libidtx_http_resolver.dylib`
///    (idempotent if already set by the updated build.rs).
/// 4. Add `@loader_path/../` rpath to the resolver dylib so it finds
///    `libusd_ms.dylib` in the sibling `Frameworks/` directory at runtime.
/// 5. Add `@executable_path/../Frameworks/idtx-http-resolver` rpath to every
///    binary that carries the forced `@rpath/libidtx_http_resolver.dylib` load
///    and remove the old absolute `OUT_DIR` rpath.
///
/// `dependent_binaries` must list *all* Mach-O binaries in the bundle that link
/// the resolver — both Conductor and the co-located `idtx-runtime`. The runtime
/// is the process Conductor spawns for "Preview in Runtime", so omitting it
/// leaves dyld unable to resolve `@rpath/libidtx_http_resolver.dylib` and the
/// preview aborts at load time.
///
/// Returns the path to the bundled dylib for later signing.
fn bundle_resolver_plugin(
    profile_dir: &Path,
    usd_lib_dir: &Path,
    frameworks_dir: &Path,
    dependent_binaries: &[&Path],
) -> Result<PathBuf> {
    let out_dir = find_resolver_out_dir(profile_dir)?;

    let src_dylib = out_dir.join("libidtx_http_resolver.dylib");
    let src_pluginfo = out_dir.join("plugInfo.json");
    if !src_pluginfo.is_file() {
        bail!(
            "plugInfo.json not found at {} — resolver build may be incomplete",
            src_pluginfo.display()
        );
    }

    // 1. Create the plugin subdirectory inside Frameworks/.
    let plugin_dir = frameworks_dir.join("idtx-http-resolver");
    fs::create_dir_all(&plugin_dir)
        .with_context(|| format!("creating {}", plugin_dir.display()))?;

    // 2. Copy the dylib and plugInfo.json.
    let dst_dylib = plugin_dir.join("libidtx_http_resolver.dylib");
    let dst_pluginfo = plugin_dir.join("plugInfo.json");
    fs::copy(&src_dylib, &dst_dylib)
        .with_context(|| format!("copying resolver dylib into {}", plugin_dir.display()))?;
    fs::copy(&src_pluginfo, &dst_pluginfo).with_context(|| {
        format!(
            "copying resolver plugInfo.json into {}",
            plugin_dir.display()
        )
    })?;

    // 3. Stamp a proper @rpath-relative install name on the bundled dylib.
    //    If build.rs already set it this is a no-op (install_name_tool
    //    silently accepts overwriting with the same name).
    change_install_name(&dst_dylib, "@rpath/libidtx_http_resolver.dylib")?;

    // 4. The resolver dylib links against libusd_ms via @rpath. Its own
    //    directory is `Frameworks/idtx-http-resolver/`; libusd_ms lives one
    //    level up in `Frameworks/`. @loader_path/../ resolves to that sibling.
    add_rpath(&dst_dylib, "@loader_path/../")?;
    delete_rpath(&dst_dylib, &usd_lib_dir.to_string_lossy())?;
    // Remove the OUT_DIR rpath that build.rs also added to the dylib (if any).
    delete_rpath(&dst_dylib, &out_dir.to_string_lossy())?;

    // 5. Wire every binary that carries the forced resolver load to find the
    //    dylib in the bundle. Conductor *and* the co-located idtx-runtime both
    //    link @rpath/libidtx_http_resolver.dylib; the runtime is spawned for
    //    "Preview in Runtime", so it needs the same rpath or dyld aborts it.
    //    Also remove the absolute OUT_DIR rpath that cargo baked in (the usd
    //    lib_dir rpath is handled by the outer bundle_app rpath fixup).
    for binary in dependent_binaries {
        add_rpath(binary, "@executable_path/../Frameworks/idtx-http-resolver")?;
        delete_rpath(binary, &out_dir.to_string_lossy())?;
    }

    println!(
        "   \u{2022} bundled resolver plugin → {}",
        plugin_dir.display()
    );
    Ok(dst_dylib)
}

// ---------------------------------------------------------------------------
// bundle-windows subcommand
// ---------------------------------------------------------------------------

/// DLLs to copy from `<usd>/lib/` into the bundle root.
///
/// Mirrors the `copy_glob(lib_dir, ...)` call in
/// `crates/idtx-usd-sys/build_support/usd_runtime.rs` `stage_usd_runtime()`.
const WIN_USD_LIB_DLLS: &[&str] = &["usd_ms.dll"];

/// DLLs to copy from `<usd>/bin/` into the bundle root.
///
/// Mirrors the `copy_glob(bin_dir, ...)` call in `stage_usd_runtime()`.
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

/// TBB DLLs — searched in both `bin/` and `lib/` (belt-and-suspenders, as
/// some `OpenUSD` builds ship TBB in `lib/` rather than `bin/`).
const WIN_TBB_DLLS: &[&str] = &[
    "tbb12.dll",
    "tbb.dll",
    "tbbmalloc.dll",
    "tbbmalloc_proxy.dll",
];

/// Entry point for `cargo xtask bundle-windows`.
fn bundle_windows_cmd(args: WinBundleArgs) -> Result<()> {
    if !cfg!(target_os = "windows") {
        bail!("bundle-windows produces Windows bundles and must run on Windows");
    }

    let opts = args.into_options();
    let workspace_root = workspace_root()?;
    // Windows bundles live under `dist/windows/` (the macOS twin is `dist/macos/`,
    // written by `bundle-mac`). An explicit `--out <dir>` is honoured verbatim.
    let out_dir = opts
        .out_dir
        .clone()
        .unwrap_or_else(|| workspace_root.join("dist").join("windows"));
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output directory {}", out_dir.display()))?;

    let apps = opts.apps.clone();
    for app in apps {
        bundle_windows_app(app.spec(), &opts, &workspace_root, &out_dir)?;
    }
    Ok(())
}

/// Full Windows bundling pipeline for a single app:
///
/// build → (optional runtime build) → create bundle dir → copy exe(s) →
/// copy DLLs → copy USD/`MaterialX`/asset trees → (optional resolver) → zip
fn bundle_windows_app(
    spec: BundleSpec,
    opts: &WinBundleOptions,
    workspace_root: &Path,
    out_dir: &Path,
) -> Result<()> {
    let base = &opts.base;
    let usd_dir = resolve_usd_dir(base.usd_dir.clone(), workspace_root);
    let lib_dir = usd_dir.join("lib");
    let bin_dir = usd_dir.join("bin");

    // Verify the OpenUSD install is present before kicking off a long build.
    let usd_dll = lib_dir.join("usd_ms.dll");
    if !usd_dll.is_file() {
        bail!(
            "OpenUSD monolithic DLL not found at {}.\n\
             Set --usd-dir or $IDTX_USD_DIR to a prebuilt Windows install prefix.",
            usd_dll.display()
        );
    }

    // 1. Build the main app binary.
    println!("\u{25b6}  Building {} ({})", spec.package, base.profile());
    build_binary(workspace_root, &usd_dir, &spec, base)?;

    // 2. Build idtx-runtime alongside Conductor — it is spawned via
    //    `current_exe().parent()` and must sit in the same directory.
    if spec.bundles_runtime {
        let rt = BundleSpec::runtime();
        println!("\u{25b6}  Building {} ({})", rt.package, base.profile());
        build_binary(workspace_root, &usd_dir, &rt, base)?;
    }

    let profile_dir = workspace_root.join("target").join(base.profile());

    // 3. Create (or recreate) the bundle directory from a clean slate.
    let bundle_dir = out_dir.join(spec.package);
    if bundle_dir.exists() {
        fs::remove_dir_all(&bundle_dir)
            .with_context(|| format!("clearing old bundle {}", bundle_dir.display()))?;
    }
    fs::create_dir_all(&bundle_dir)
        .with_context(|| format!("creating bundle dir {}", bundle_dir.display()))?;

    // 4. Copy the main executable.
    let exe_src = profile_dir.join(format!("{}.exe", spec.package));
    if !exe_src.is_file() {
        bail!(
            "expected freshly built binary at {} but it is missing",
            exe_src.display()
        );
    }
    fs::copy(&exe_src, bundle_dir.join(format!("{}.exe", spec.package)))
        .with_context(|| format!("copying {}.exe into bundle", spec.package))?;
    println!("   \u{2022} {}.exe", spec.package);

    // 4b. Co-locate idtx-runtime.exe for Conductor.
    if spec.bundles_runtime {
        let rt_src = profile_dir.join("idtx-runtime.exe");
        if !rt_src.is_file() {
            bail!(
                "expected idtx-runtime.exe at {} but it is missing — \
                 Conductor needs it co-located for 'Preview in Runtime'",
                rt_src.display()
            );
        }
        fs::copy(&rt_src, bundle_dir.join("idtx-runtime.exe"))
            .context("copying idtx-runtime.exe into bundle")?;
        println!("   \u{2022} idtx-runtime.exe");
    }

    // 5. Copy OpenUSD/MaterialX/TBB DLLs.
    //    Windows resolves DLLs from the exe directory — no rpath equivalent.
    let dll_count = copy_win_dlls(&lib_dir, &bin_dir, &bundle_dir)?;
    println!("   \u{2022} {} DLLs", dll_count);

    // 6. Copy OpenUSD resource trees.
    //
    //    6a. usd/ — plugInfo.json discovery tree.
    //        Placed as a sibling of usd_ms.dll, mirroring the upstream install
    //        layout <prefix>/lib/{usd_ms.dll,usd/}, so USD's PlugRegistry
    //        discovers plugins with no environment variable.
    let usd_tree = lib_dir.join("usd");
    if usd_tree.is_dir() {
        copy_dir_recursive(&usd_tree, &bundle_dir.join("usd"))
            .context("copying usd/ plugin-discovery tree")?;
        println!("   \u{2022} usd/  (plugin discovery)");
    } else {
        eprintln!(
            "xtask: warning: USD resource tree not found at {} — \
             plugin discovery will fail at runtime",
            usd_tree.display()
        );
    }

    //    6b. plugin/ — usdShaders and other top-level plugins.
    let plugin_tree = usd_dir.join("plugin");
    if plugin_tree.is_dir() {
        copy_dir_recursive(&plugin_tree, &bundle_dir.join("plugin"))
            .context("copying plugin/ tree")?;
        println!("   \u{2022} plugin/");
    }

    //    6c. libraries/ — MaterialX .mtlx standard data libraries.
    //        Required for runtime MaterialX shader generation.
    let libraries_tree = usd_dir.join("libraries");
    if libraries_tree.is_dir() {
        copy_dir_recursive(&libraries_tree, &bundle_dir.join("libraries"))
            .context("copying libraries/ (MaterialX stdlib)")?;
        println!("   \u{2022} libraries/  (MaterialX stdlib)");
    } else {
        eprintln!(
            "xtask: warning: MaterialX libraries/ not found at {} — \
             runtime MaterialX shader generation may fail",
            libraries_tree.display()
        );
    }

    //    6d. resources/ — MaterialX render resources (fonts, geometry, images, lights).
    let resources_tree = usd_dir.join("resources");
    if resources_tree.is_dir() {
        copy_dir_recursive(&resources_tree, &bundle_dir.join("resources"))
            .context("copying resources/ (MaterialX render data)")?;
        println!("   \u{2022} resources/  (MaterialX render data)");
    }

    //    6e. assets/ — workspace default controller meshes and other runtime assets.
    //        Resolved at runtime via idtx-runtime-manifest's asset_search_roots()
    //        (<exe>/assets is the packaged fallback root).
    let assets_src = workspace_root.join("assets");
    if assets_src.is_dir() {
        copy_dir_recursive(&assets_src, &bundle_dir.join("assets")).context("copying assets/")?;
        println!("   \u{2022} assets/");
    }

    //    6f. built-in/ — the toolbox's built-in asset tree (player stage + rigs +
    //        default gameplay behaviors). Staged beside the exe so `@built-in/...@`
    //        layer references resolve: idtx-assets' builtin_root() returns the exe
    //        dir in a packaged layout, and the runtime manifest places it on the
    //        default-resolver search path.
    let builtin_src = workspace_root.join("crates/idtx-assets/built-in");
    if builtin_src.is_dir() {
        copy_dir_recursive(&builtin_src, &bundle_dir.join("built-in"))
            .context("copying built-in/ player tree")?;
        println!("   \u{2022} built-in/  (player stage + rigs + behaviors)");
    }

    // 7. (--remote) Bundle the HTTP/USD resolver plugin.
    if base.remote {
        bundle_win_resolver_plugin(&profile_dir, &bundle_dir)?;
    }

    // 7b. (--android) Cross-build + stage the Android (Quest) runtime tree so the
    //     shipped conductor can package an Android native app. Conductor only —
    //     the editors do not spawn a runtime, so there is nothing to co-locate.
    if base.android {
        if spec.bundles_runtime {
            stage_android_runtime(&bundle_dir, base)?;
        } else {
            eprintln!(
                "xtask: note: --android is a no-op for {} (only idtx-conductor packages \
                 standalone apps)",
                spec.package
            );
        }
    }

    // 8. Produce a zip archive with PowerShell Compress-Archive.
    //    Version comes from the xtask crate version (tracks the workspace).
    if !opts.no_zip {
        let version = env!("CARGO_PKG_VERSION");
        let zip_name = format!("{}-v{}-win64.zip", spec.package, version);
        let zip_path = out_dir.join(&zip_name);
        zip_bundle(&bundle_dir, &zip_path)?;
        println!("   \u{2022} {} (zip)", zip_name);
    }

    println!(
        "\n\u{2705}  Bundled: {}\n    Run:  {}\\{}.exe",
        bundle_dir.display(),
        bundle_dir.display(),
        spec.package,
    );
    Ok(())
}

/// Copy the Windows USD/`MaterialX`/TBB DLLs into `bundle_dir`.
///
/// Mirrors the enumeration in `crates/idtx-usd-sys/build_support/usd_runtime.rs`
/// `stage_usd_runtime()` Windows path. Files absent in the source are silently
/// skipped (not all `OpenUSD` builds include every optional `MaterialX` generator).
/// Returns the total number of DLLs actually written.
fn copy_win_dlls(lib_dir: &Path, bin_dir: &Path, bundle_dir: &Path) -> Result<usize> {
    let mut count = 0;
    count += copy_named(lib_dir, bundle_dir, WIN_USD_LIB_DLLS);
    count += copy_named(bin_dir, bundle_dir, WIN_USD_BIN_DLLS);
    count += copy_named(bin_dir, bundle_dir, WIN_TBB_DLLS);
    // Belt-and-suspenders: some OpenUSD builds ship TBB in lib/ not bin/.
    count += copy_named(lib_dir, bundle_dir, WIN_TBB_DLLS);
    Ok(count)
}

/// Copy a list of named files from `src_dir` to `dst_dir`.
///
/// Silently skips files that don't exist in `src_dir` — this is intentional:
/// not all optional `MaterialX` generators are present in every `OpenUSD` build.
/// Returns the number of files successfully copied.
fn copy_named(src_dir: &Path, dst_dir: &Path, names: &[&str]) -> usize {
    let mut count = 0;
    for name in names {
        let src = src_dir.join(name);
        if !src.is_file() {
            continue;
        }
        let dst = dst_dir.join(name);
        match fs::copy(&src, &dst) {
            Ok(_) => count += 1,
            Err(e) => eprintln!(
                "xtask: warning: failed to copy {} \u{2192} {}: {e}",
                src.display(),
                dst.display()
            ),
        }
    }
    count
}

/// Bundle the Windows HTTP/USD resolver plugin.
///
/// The conductor (and the editors) link the resolver at load time — the
/// `idtx-usd-resolver-http` build script emits
/// `cargo:rustc-link-lib=dylib=idtx_http_resolver`, so the `.exe` carries a
/// load-time import on `idtx_http_resolver.dll`. Windows resolves that import
/// from the executable's own directory (there is no rpath equivalent), so the
/// DLL MUST sit in the bundle root beside the `.exe`; a copy tucked away in a
/// subfolder leaves the loader unable to find it and the process dies at launch
/// with "idtx_http_resolver.dll was not found" before `main()` runs.
///
/// So, mirroring the visionOS runtime packager
/// ([`visionos_runtime`]'s resolver staging):
/// * the DLL goes to the **bundle root** (loader-visible, one physical copy);
/// * `plugInfo.json` goes to an `idtx-http-resolver/` subfolder with its
///   `LibraryPath` **emptied**, so `install()` →
///   `PlugRegistry::RegisterPlugins(<subfolder>)` registers the http/https URI
///   schemes against the already-load-resident resolver type WITHOUT a second
///   `LoadLibrary` (a second copy at a different path would split the resolver's
///   process-global fetch state and re-register its `TfType`).
///
/// `resolve_plugin_dir` (in `idtx-usd-resolver-http`) probes this
/// `<exe_dir>/idtx-http-resolver/` layout, so no `PXR_PLUGINPATH_NAME` is needed.
fn bundle_win_resolver_plugin(profile_dir: &Path, bundle_dir: &Path) -> Result<()> {
    let out_dir = find_resolver_out_dir_win(profile_dir)?;

    let src_dll = out_dir.join("idtx_http_resolver.dll");
    let src_pluginfo = out_dir.join("plugInfo.json");
    if !src_dll.is_file() {
        bail!(
            "resolver DLL not found at {} — resolver build may be incomplete",
            src_dll.display()
        );
    }
    if !src_pluginfo.is_file() {
        bail!(
            "plugInfo.json not found at {} — resolver build may be incomplete",
            src_pluginfo.display()
        );
    }

    // 1. DLL → bundle root, beside the .exe, so the Windows loader resolves the
    //    .exe's load-time import of idtx_http_resolver.dll.
    fs::copy(&src_dll, bundle_dir.join("idtx_http_resolver.dll"))
        .with_context(|| format!("copying resolver DLL into {}", bundle_dir.display()))?;

    // 2. plugInfo.json → idtx-http-resolver/ with LibraryPath emptied (the DLL
    //    is already load-resident from step 1; see the fn doc). No DLL copy here.
    let plugin_dir = bundle_dir.join("idtx-http-resolver");
    fs::create_dir_all(&plugin_dir)
        .with_context(|| format!("creating {}", plugin_dir.display()))?;
    let plug_info =
        fs::read_to_string(&src_pluginfo).with_context(|| "reading resolver plugInfo.json")?;
    fs::write(
        plugin_dir.join("plugInfo.json"),
        visionos_runtime::empty_plug_info_library_path(&plug_info),
    )
    .with_context(|| {
        format!(
            "writing resolver plugInfo.json into {}",
            plugin_dir.display()
        )
    })?;

    println!(
        "   \u{2022} idtx_http_resolver.dll  (+ idtx-http-resolver/plugInfo.json)"
    );
    Ok(())
}

/// Locate the Windows resolver DLL by scanning
/// `target/<profile>/build/idtx-usd-resolver-http-*/out/` for
/// `idtx_http_resolver.dll`. Mirrors the macOS [`find_resolver_out_dir`].
fn find_resolver_out_dir_win(profile_dir: &Path) -> Result<PathBuf> {
    let build_dir = profile_dir.join("build");
    if !build_dir.is_dir() {
        bail!(
            "build dir {} not found — has `cargo build` run yet?",
            build_dir.display()
        );
    }
    for entry in
        fs::read_dir(&build_dir).with_context(|| format!("reading {}", build_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("idtx-usd-resolver-http-") {
            continue;
        }
        let out = entry.path().join("out");
        if out.join("idtx_http_resolver.dll").is_file() {
            return Ok(out);
        }
    }
    bail!(
        "could not find idtx_http_resolver.dll under \
         {}/build/idtx-usd-resolver-http-*/out/\n\
         Did you build with --features remote (and IDTX_USD_DIR set)?",
        profile_dir.display()
    );
}

/// Zip `bundle_dir` into `zip_path` via PowerShell `Compress-Archive`.
///
/// Requires PowerShell 5.0+ (ships with Windows 10 and later). The `-Force`
/// flag overwrites any existing zip at the same destination path, making the
/// command idempotent. PowerShell single-quoted strings treat backslashes
/// literally, so Windows paths from `Path::display()` are safe inside them.
fn zip_bundle(bundle_dir: &Path, zip_path: &Path) -> Result<()> {
    let cmd = format!(
        "Compress-Archive -Force -Path '{src}\\*' -DestinationPath '{dst}'",
        src = bundle_dir.display(),
        dst = zip_path.display(),
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &cmd])
        .status()
        .context("failed to spawn powershell for Compress-Archive")?;
    if !status.success() {
        bail!(
            "PowerShell Compress-Archive failed (exit {})\n\
             Command: {cmd}",
            status.code().unwrap_or(-1),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared build / path helpers
// ---------------------------------------------------------------------------

/// Change the `LC_ID_DYLIB` install name of a dylib via `install_name_tool -id`.
pub(crate) fn change_install_name(dylib: &Path, new_id: &str) -> Result<()> {
    let output = Command::new("install_name_tool")
        .arg("-id")
        .arg(new_id)
        .arg(dylib)
        .output()
        .context("failed to spawn install_name_tool -id")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "install_name_tool -id {new_id} {} failed: {}",
            dylib.display(),
            stderr.trim()
        );
    }
    Ok(())
}

fn build_binary(
    workspace_root: &Path,
    usd_dir: &Path,
    spec: &BundleSpec,
    opts: &BundleOptions,
) -> Result<()> {
    let mut cmd = Command::new(cargo());
    cmd.current_dir(workspace_root)
        .arg("build")
        .arg("-p")
        .arg(spec.package)
        .env("IDTX_USD_DIR", usd_dir);
    if !opts.debug {
        cmd.arg("--release");
    }
    if opts.remote {
        cmd.arg("--features")
            .arg(format!("{}/remote", spec.package));
    }
    if opts.pcvr {
        if let Some(feature) = spec.pcvr_feature {
            cmd.arg("--features").arg(feature);
        }
    }
    if opts.openexec {
        if let Some(feature) = spec.openexec_feature {
            cmd.arg("--features").arg(feature);
        }
    }
    let status = cmd
        .status()
        .with_context(|| format!("failed to spawn `cargo build -p {}`", spec.package))?;
    if !status.success() {
        bail!("`cargo build -p {}` failed with {status}", spec.package);
    }
    Ok(())
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

// ---------------------------------------------------------------------------
// Standalone (Android / visionOS) runtime build + stage
// ---------------------------------------------------------------------------

/// Options for the Android `OpenUSD` cross-build (`build-usd-android`) driven by a
/// bundle. `MaterialX` stays enabled (standalone material fidelity matches
/// desktop); `openexec` turns on `PXR_BUILD_EXEC=ON` so the monolith carries the
/// `OpenExec` symbols the coded plugin links.
fn android_cross_options(openexec: bool) -> usd_cross::CrossOptions {
    usd_cross::CrossOptions {
        no_materialx: false,
        materialx_flag: false,
        dry_run: false,
        with_openexec: openexec,
    }
}

/// Options for the Quest runtime cross-build (`build-quest-runtime`), matching the
/// bundle profile and propagating `--remote` → `--with-remote`, `--openexec` →
/// `--with-openexec`.
fn quest_runtime_options(
    debug: bool,
    remote: bool,
    openexec: bool,
) -> quest_runtime::QuestRuntimeOptions {
    quest_runtime::QuestRuntimeOptions {
        debug,
        release: !debug,
        dry_run: false,
        skip_openxr_loader: false,
        with_metrics: false,
        with_openexec: openexec,
        with_remote: remote,
        usd_dir_android: None,
    }
}

/// Options for the visionOS runtime cross-build (`build-visionos-runtime`, device
/// slice), matching the bundle profile and propagating `--remote`/`--openexec`.
fn visionos_runtime_options(
    debug: bool,
    remote: bool,
    openexec: bool,
) -> visionos_runtime::VisionOsRuntimeOptions {
    visionos_runtime::VisionOsRuntimeOptions {
        debug,
        release: !debug,
        simulator: false,
        dry_run: false,
        usd_dir_visionos: None,
        with_openexec: openexec,
        with_remote: remote,
        with_metrics: false,
    }
}

/// Cross-build the Android (Quest) runtime and copy its staged tree into
/// `dest_dir/android-runtime/` — the directory the shipped conductor's
/// `quest_runtime_binary()` lookup reads beside its exe.
///
/// Cross-compiles `OpenUSD` for Android first (reused when a complete install is
/// already staged, unless `--android-rebuild-usd`), then `idtx-runtime` →
/// `libidtx_runtime.so`, then stages `target/<profile>/android-runtime/` whole
/// (the `arm64-v8a/` slice + the sibling `usd/`, and — when enabled — `usd-exec/`,
/// `idtx-http-resolver/`). External toolchains (NDK/JDK/SDK) are never copied.
fn stage_android_runtime(dest_dir: &Path, base: &BundleOptions) -> Result<()> {
    let usd = quest_runtime::default_android_usd_dir();
    // Reuse the (slow) OpenUSD cross-build only when a complete install is present
    // AND — when `--openexec` is requested — that install actually carries the
    // OpenExec framework. A staged non-exec monolith reused for an `--openexec`
    // bundle would produce an APK where OpenExec never runs, with no build error
    // (the plugin binds the monolith's exec symbols lazily on device).
    let openexec_mismatch = base.openexec && !quest_runtime::android_usd_install_has_exec(&usd);
    if base.android_rebuild_usd
        || !quest_runtime::android_usd_install_present(&usd)
        || openexec_mismatch
    {
        if openexec_mismatch && !base.android_rebuild_usd && quest_runtime::android_usd_install_present(&usd) {
            println!(
                "   \u{2022} staged Android OpenUSD install at {} lacks the OpenExec framework \
                 but --openexec was requested; rebuilding",
                usd.display()
            );
        }
        println!(
            "\u{25b6}  Cross-building OpenUSD for Android (slow; reused on later runs) \u{2192} {}",
            usd.display()
        );
        usd_cross::build_usd_android_cmd(&android_cross_options(base.openexec))?;
    } else {
        println!(
            "   \u{2022} reusing staged Android OpenUSD install at {}",
            usd.display()
        );
    }

    println!(
        "\u{25b6}  Cross-building idtx-runtime for Android ({})",
        base.profile()
    );
    quest_runtime::build_quest_runtime_cmd(&quest_runtime_options(
        base.debug,
        base.remote,
        base.openexec,
    ))?;

    let tree = quest_runtime::android_runtime_tree_dir(base.profile());
    let so = tree
        .join(quest_runtime::ANDROID_ABI_DIR)
        .join("libidtx_runtime.so");
    if !so.is_file() {
        bail!(
            "Android runtime staging incomplete: expected {} after build-quest-runtime",
            so.display()
        );
    }
    let dst = dest_dir.join("android-runtime");
    if dst.exists() {
        fs::remove_dir_all(&dst).with_context(|| format!("clearing old {}", dst.display()))?;
    }
    copy_dir_recursive(&tree, &dst).context("staging android-runtime/ into bundle")?;
    println!("   \u{2022} android-runtime/  (Quest libidtx_runtime.so + USD deps + loader)");
    Ok(())
}

/// Cross-build the visionOS runtime (device slice) and copy its staged tree into
/// `macos_dir/visionos-runtime/`. **macOS only** — the caller ([`bundle_app`]) has
/// already bailed on non-macOS. Reuses an already-staged visionOS `OpenUSD` install
/// unless `--visionos-rebuild-usd`. `build-usd-visionos` builds both the device
/// and simulator USD slices (its existing behavior); only the device runtime is
/// staged into the bundle.
fn stage_visionos_runtime(macos_dir: &Path, base: &BundleOptions) -> Result<()> {
    let usd = visionos_runtime::default_visionos_usd_dir();
    // Reuse the (slow) OpenUSD cross-build only when a complete install is present
    // AND — when `--openexec` is requested — that install actually carries the
    // OpenExec framework. A staged non-exec monolith reused for an `--openexec`
    // bundle would produce an `.app` where OpenExec never runs, with no build error
    // (the plugin binds the monolith's exec symbols lazily on device). Mirrors the
    // Android guard in `stage_android_runtime`.
    let openexec_mismatch = base.openexec && !visionos_runtime::visionos_usd_install_has_exec(&usd);
    if base.visionos_rebuild_usd
        || !visionos_runtime::visionos_usd_install_present(&usd)
        || openexec_mismatch
    {
        if openexec_mismatch
            && !base.visionos_rebuild_usd
            && visionos_runtime::visionos_usd_install_present(&usd)
        {
            println!(
                "   \u{2022} staged visionOS OpenUSD install at {} lacks the OpenExec framework \
                 but --openexec was requested; rebuilding",
                usd.display()
            );
        }
        println!(
            "\u{25b6}  Cross-building OpenUSD for visionOS (device + simulator; slow; reused later) \u{2192} {}",
            usd.display()
        );
        usd_cross::build_usd_visionos_cmd(&android_cross_options(base.openexec))?;
    } else {
        println!(
            "   \u{2022} reusing staged visionOS OpenUSD install at {}",
            usd.display()
        );
    }

    println!(
        "\u{25b6}  Cross-building idtx-runtime for visionOS ({}, device slice)",
        base.profile()
    );
    visionos_runtime::build_visionos_runtime_cmd(&visionos_runtime_options(
        base.debug,
        base.remote,
        base.openexec,
    ))?;

    let tree = visionos_runtime::visionos_runtime_tree_dir(base.profile());
    let dylib = tree.join("arm64").join("libidtx_runtime.dylib");
    if !dylib.is_file() {
        bail!(
            "visionOS runtime staging incomplete: expected {} after build-visionos-runtime",
            dylib.display()
        );
    }
    let dst = macos_dir.join("visionos-runtime");
    if dst.exists() {
        fs::remove_dir_all(&dst).with_context(|| format!("clearing old {}", dst.display()))?;
    }
    copy_dir_recursive(&tree, &dst).context("staging visionos-runtime/ into bundle")?;
    println!("   \u{2022} visionos-runtime/  (Vision Pro libidtx_runtime.dylib + USD deps)");
    Ok(())
}

/// Locate the workspace root from `CARGO_MANIFEST_DIR` (set by cargo when
/// running as an alias) or fall back to the current directory.
///
/// Shared with the cross-build modules (`quest_runtime`, `usd_cross`,
/// `visionos_runtime`), whose own path math (the cargo `target/` dir and the
/// `thirdparty/` staging prefixes) must agree with the bundlers here — the real
/// `[workspace]` root is `idtx-studio`, not `xtask`'s immediate parent
/// (`idtx-studio/tools`).
pub(crate) fn workspace_root() -> Result<PathBuf> {
    // Start from this crate's manifest dir (`<root>/tools/xtask`) and walk up
    // ancestors until we find the `Cargo.toml` that declares `[workspace]`.
    // This is robust to how deeply xtask is nested under the workspace root.
    let start = match env::var_os("CARGO_MANIFEST_DIR") {
        Some(manifest) => PathBuf::from(manifest),
        None => env::current_dir().context("could not determine current directory")?,
    };

    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        if let Ok(contents) = fs::read_to_string(&manifest) {
            if contents.contains("[workspace]") {
                return Ok(dir.to_path_buf());
            }
        }
    }

    bail!(
        "could not locate the workspace root (a Cargo.toml with [workspace]) \
         above {}",
        start.display()
    )
}

/// Precedence: explicit `--usd-dir` → `$IDTX_USD_DIR` →
/// `<workspace>/third_party/openusd/install`.
fn resolve_usd_dir(explicit: Option<PathBuf>, workspace_root: &Path) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(dir) = env::var_os("IDTX_USD_DIR") {
        return PathBuf::from(dir);
    }
    workspace_root
        .join("third_party")
        .join("openusd")
        .join("install")
}

// ---------------------------------------------------------------------------
// macOS-specific helpers — dylib copying and rpath manipulation
// ---------------------------------------------------------------------------

/// Copy the `OpenUSD` runtime dylibs the bundle needs: `libusd_ms*.dylib`,
/// `libtbb*.dylib` (including the symlink chain), and `libMaterialX*.dylib`.
/// Returns the count of real (non-symlink) files copied.
fn copy_usd_runtime_libs(lib_dir: &Path, frameworks_dir: &Path) -> Result<usize> {
    let mut copied = 0usize;
    // Two passes so symlink targets exist before the links are recreated.
    // Pass 1: real files.
    for entry in fs::read_dir(lib_dir)
        .with_context(|| format!("reading USD lib dir {}", lib_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_usd_runtime_lib(&name) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())
            .with_context(|| format!("stat {}", entry.path().display()))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        let dst = frameworks_dir.join(&name);
        fs::copy(entry.path(), &dst).with_context(|| format!("copying {name} into Frameworks"))?;
        copied += 1;
    }
    // Pass 2: symlinks (e.g. libtbb.12.dylib → libtbb.12.12.dylib).
    for entry in fs::read_dir(lib_dir)
        .with_context(|| format!("reading USD lib dir {}", lib_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_usd_runtime_lib(&name) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())
            .with_context(|| format!("stat {}", entry.path().display()))?;
        if !meta.file_type().is_symlink() {
            continue;
        }
        let target = fs::read_link(entry.path())
            .with_context(|| format!("readlink {}", entry.path().display()))?;
        let dst = frameworks_dir.join(&name);
        let _ = fs::remove_file(&dst);
        // cfg-gated: the code must compile on Windows even though it only
        // executes on macOS (bundle_app bails early on non-macOS).
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &dst).with_context(|| {
            format!(
                "recreating symlink {} -> {} in Frameworks",
                name,
                target.display()
            )
        })?;
        #[cfg(not(unix))]
        {
            let _ = &target;
            bail!("macOS bundling (symlink recreation) is not supported on this host");
        }
    }
    Ok(copied)
}

/// Collect real (non-symlink) `libMaterialX*.dylib` files in `frameworks_dir`.
fn materialx_dylibs_in(frameworks_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(frameworks_dir)
        .with_context(|| format!("reading Frameworks dir {}", frameworks_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().contains("materialx") || !name.contains(".dylib") {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path())
            .with_context(|| format!("stat {}", entry.path().display()))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        out.push(entry.path());
    }
    Ok(out)
}

fn is_usd_runtime_lib(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !lower.contains(".dylib") {
        return false;
    }
    lower.contains("usd_ms") || lower.contains("tbb") || lower.contains("materialx")
}

// ---------------------------------------------------------------------------
// General helpers
// ---------------------------------------------------------------------------

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} \u{2192} {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

pub(crate) fn add_rpath(binary: &Path, rpath: &str) -> Result<()> {
    let output = Command::new("install_name_tool")
        .arg("-add_rpath")
        .arg(rpath)
        .arg(binary)
        .output()
        .context("failed to spawn install_name_tool")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("would duplicate path") || stderr.contains("already") {
        return Ok(());
    }
    bail!(
        "install_name_tool -add_rpath {rpath} {} failed: {}",
        binary.display(),
        stderr.trim()
    );
}

pub(crate) fn delete_rpath(binary: &Path, rpath: &str) -> Result<()> {
    let output = Command::new("install_name_tool")
        .arg("-delete_rpath")
        .arg(rpath)
        .arg(binary)
        .output()
        .context("failed to spawn install_name_tool")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no LC_RPATH") {
        return Ok(());
    }
    bail!(
        "install_name_tool -delete_rpath {rpath} {} failed: {}",
        binary.display(),
        stderr.trim()
    );
}

/// Ad-hoc sign `target` (`codesign --force [--deep] --sign -`).
/// Required after `install_name_tool` edits, which invalidate the Mach-O
/// signature (Apple silicon kills processes with invalid signatures).
/// `deep = true` is needed when signing the whole `.app` bundle so the
/// usd/ plugin tree under Frameworks/ is covered.
pub(crate) fn codesign_adhoc(target: &Path, deep: bool) -> Result<()> {
    let mut cmd = Command::new("codesign");
    cmd.arg("--force");
    if deep {
        cmd.arg("--deep");
    }
    cmd.arg("--sign").arg("-").arg(target);
    let output = cmd.output().context("failed to spawn codesign")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "codesign --force{} --sign - {} failed: {}",
            if deep { " --deep" } else { "" },
            target.display(),
            stderr.trim()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS icon + plist helpers
// ---------------------------------------------------------------------------

/// Convert the crate's `.ico` to a macOS `.icns` via `sips` + `iconutil`.
///
/// Pipeline:
/// 1. `sips` decodes `.ico` → `base.png`
/// 2. Resize into all iconset slots (≤256 px, matching source resolution)
/// 3. `iconutil -c icns` packs them into `.icns`
///
/// Returns `Ok(true)` when the `.icns` was written, `Ok(false)` when the
/// source `.ico` is absent (non-fatal — bundle uses the system default icon).
fn convert_ico_to_icns(ico: &Path, icns: &Path, iconset_stem: &str) -> Result<bool> {
    if !ico.is_file() {
        return Ok(false);
    }
    let tmp = icns
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".idtx-iconbuild");
    let iconset = tmp.join(format!("{iconset_stem}.iconset"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&iconset).with_context(|| format!("creating {}", iconset.display()))?;

    // Step 1: decode .ico → base PNG.
    let base_png = tmp.join("base.png");
    run_sips(&[
        "-s".as_ref(),
        "format".as_ref(),
        "png".as_ref(),
        ico.as_os_str(),
        "--out".as_ref(),
        base_png.as_os_str(),
    ])
    .context("decoding .ico to .png via sips")?;

    // Step 2: emit iconset slots the 256 px source can fill without upscaling.
    let slots: &[(u32, &str)] = &[
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (64, "icon_64x64.png"),
        (128, "icon_64x64@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
    ];
    for (px, fname) in slots {
        let dst = iconset.join(fname);
        run_sips(&[
            "-z".as_ref(),
            px.to_string().as_ref(),
            px.to_string().as_ref(),
            base_png.as_os_str(),
            "--out".as_ref(),
            dst.as_os_str(),
        ])
        .with_context(|| format!("resizing icon to {px}px slot {fname}"))?;
    }

    // Step 3: pack iconset → .icns.
    let output = Command::new("iconutil")
        .arg("-c")
        .arg("icns")
        .arg(&iconset)
        .arg("-o")
        .arg(icns)
        .output()
        .context("failed to spawn iconutil")?;
    let _ = fs::remove_dir_all(&tmp);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("iconutil failed to pack .icns: {}", stderr.trim());
    }
    Ok(icns.is_file())
}

fn run_sips(args: &[&std::ffi::OsStr]) -> Result<()> {
    let output = Command::new("sips")
        .args(args)
        .output()
        .context("failed to spawn sips")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("sips failed: {}", stderr.trim());
    }
    Ok(())
}

fn write_info_plist(path: &Path, spec: &BundleSpec, have_icon: bool) -> Result<()> {
    let icon_entry = if have_icon {
        format!(
            "\n    <key>CFBundleIconFile</key>\n    <string>{}</string>",
            spec.package
        )
    } else {
        String::new()
    };
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>{display_name}</string>
    <key>CFBundleDisplayName</key>
    <string>{display_name}</string>
    <key>CFBundleExecutable</key>
    <string>{package}</string>
    <key>CFBundleIdentifier</key>
    <string>{bundle_identifier}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>{icon_entry}
</dict>
</plist>
"#,
        display_name = spec.display_name,
        package = spec.package,
        bundle_identifier = spec.bundle_identifier,
        version = env!("CARGO_PKG_VERSION"),
        icon_entry = icon_entry,
    );
    fs::write(path, plist).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `bundle-mac` args from a bare argument vector through the real clap
    /// tree, then resolve to the domain [`MacBundleOptions`].
    fn parse_mac(args: &[&str]) -> Result<MacBundleOptions, clap::Error> {
        let argv = ["cargo xtask", "bundle-mac"]
            .into_iter()
            .chain(args.iter().copied());
        match Cli::try_parse_from(argv)?.command {
            Commands::BundleMac(a) => Ok(a.into_options()),
            _ => unreachable!("parsed a bundle-mac command"),
        }
    }

    /// Parse `bundle-windows` args from a bare argument vector through the real
    /// clap tree, then resolve to the domain [`WinBundleOptions`].
    fn parse_win(args: &[&str]) -> Result<WinBundleOptions, clap::Error> {
        let argv = ["cargo xtask", "bundle-windows"]
            .into_iter()
            .chain(args.iter().copied());
        match Cli::try_parse_from(argv)?.command {
            Commands::BundleWindows(a) => Ok(a.into_options()),
            _ => unreachable!("parsed a bundle-windows command"),
        }
    }

    #[test]
    fn win_dll_manifest_completeness() {
        // usd_ms.dll must be present in the lib DLL list
        assert!(
            WIN_USD_LIB_DLLS.contains(&"usd_ms.dll"),
            "WIN_USD_LIB_DLLS must include usd_ms.dll"
        );

        // The bin DLL list must cover all 13 MaterialX components that the
        // current OpenUSD 26.08-mtlx build ships.
        let materialx_count = WIN_USD_BIN_DLLS
            .iter()
            .filter(|n| n.to_ascii_lowercase().starts_with("materialx"))
            .count();
        assert_eq!(
            materialx_count, 13,
            "WIN_USD_BIN_DLLS must list exactly 13 MaterialX DLLs (found {materialx_count})"
        );

        // All four TBB DLLs must be present.
        assert_eq!(
            WIN_TBB_DLLS.len(),
            4,
            "WIN_TBB_DLLS must have exactly 4 entries"
        );
        for name in [
            "tbb12.dll",
            "tbb.dll",
            "tbbmalloc.dll",
            "tbbmalloc_proxy.dll",
        ] {
            assert!(
                WIN_TBB_DLLS.contains(&name),
                "WIN_TBB_DLLS must contain {name}"
            );
        }
    }

    #[test]
    fn bundle_spec_runtime_flag() {
        // Only Conductor bundles idtx-runtime; inspector and poser do not.
        assert!(!BundleSpec::inspector().bundles_runtime);
        assert!(!BundleSpec::poser().bundles_runtime);
        assert!(BundleSpec::conductor().bundles_runtime);
        // The runtime build-only spec must not try to recurse.
        assert!(!BundleSpec::runtime().bundles_runtime);
    }

    #[test]
    fn bundle_spec_pcvr_feature() {
        // Only packages with an OpenXR path declare a pcvr feature; inspector
        // and poser have none, so `--pcvr` must be a no-op for them (otherwise
        // `cargo build -p idtx-inspector --features pcvr` fails: no such feature).
        assert_eq!(BundleSpec::inspector().pcvr_feature, None);
        assert_eq!(BundleSpec::poser().pcvr_feature, None);
        assert_eq!(BundleSpec::conductor().pcvr_feature, Some("pcvr"));
        assert_eq!(BundleSpec::runtime().pcvr_feature, Some("pcvr"));
    }

    #[test]
    fn bundle_spec_packages() {
        assert_eq!(BundleSpec::inspector().package, "idtx-inspector");
        assert_eq!(BundleSpec::poser().package, "idtx-poser");
        assert_eq!(BundleSpec::conductor().package, "idtx-conductor");
        assert_eq!(BundleSpec::runtime().package, "idtx-runtime");
    }

    #[test]
    fn zip_name_format() {
        // The zip name must follow the pattern <package>-v<version>-win64.zip
        let version = env!("CARGO_PKG_VERSION");
        let spec = BundleSpec::inspector();
        let name = format!("{}-v{}-win64.zip", spec.package, version);
        assert!(
            name.starts_with("idtx-inspector-v"),
            "zip name must start with idtx-inspector-v"
        );
        assert!(
            name.ends_with("-win64.zip"),
            "zip name must end with -win64.zip"
        );
    }

    #[test]
    fn win_app_all_coverage() {
        // WinApp::all() must cover every app variant exactly once.
        let all = WinApp::all();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&WinApp::Inspector));
        assert!(all.contains(&WinApp::Poser));
        assert!(all.contains(&WinApp::Conductor));
    }

    #[test]
    fn win_bundle_options_defaults() {
        // No args → all three apps, release profile, remote off, zip enabled.
        let opts = parse_win(&[]).unwrap();
        assert_eq!(opts.apps.len(), 3, "default should select all three apps");
        assert!(!opts.base.debug, "default profile should be release");
        assert!(!opts.base.remote, "remote should be off by default");
        assert!(!opts.no_zip, "zip should be enabled by default");
        assert!(
            opts.out_dir.is_none(),
            "out_dir should default to None (resolved later)"
        );
    }

    #[test]
    fn win_bundle_options_single_app() {
        let opts = parse_win(&["--app", "inspector"]).unwrap();
        assert_eq!(opts.apps, vec![WinApp::Inspector]);
    }

    #[test]
    fn win_bundle_options_multiple_apps() {
        let opts = parse_win(&["--app", "inspector", "--app", "conductor"]).unwrap();
        assert_eq!(opts.apps, vec![WinApp::Inspector, WinApp::Conductor]);
    }

    #[test]
    fn win_bundle_options_all_app() {
        let opts = parse_win(&["--app", "all"]).unwrap();
        assert_eq!(opts.apps.len(), 3);
    }

    #[test]
    fn win_bundle_options_flags() {
        let opts = parse_win(&["--debug", "--remote", "--no-zip"]).unwrap();
        assert!(opts.base.debug);
        assert!(opts.base.remote);
        assert!(opts.no_zip);
    }

    #[test]
    fn win_bundle_options_out_dir() {
        let opts = parse_win(&["--out", "my-dist"]).unwrap();
        assert_eq!(opts.out_dir, Some(PathBuf::from("my-dist")));
    }

    #[test]
    fn win_bundle_options_unknown_app_errors() {
        assert!(parse_win(&["--app", "bogus"]).is_err());
    }

    #[test]
    fn win_bundle_options_missing_app_value_errors() {
        assert!(parse_win(&["--app"]).is_err());
    }

    // -- new flags: --android / --openexec (windows) -------------------------

    #[test]
    fn win_bundle_options_android_flags() {
        // Off by default.
        let opts = parse_win(&[]).unwrap();
        assert!(!opts.base.android);
        assert!(!opts.base.android_rebuild_usd);
        // Set when passed.
        let opts = parse_win(&["--android", "--android-rebuild-usd"]).unwrap();
        assert!(opts.base.android);
        assert!(opts.base.android_rebuild_usd);
    }

    #[test]
    fn win_bundle_options_openexec_flag() {
        assert!(!parse_win(&[]).unwrap().base.openexec);
        assert!(parse_win(&["--openexec"]).unwrap().base.openexec);
    }

    #[test]
    fn win_has_no_visionos_flag() {
        // --visionos is a macOS-only flag; bundle-windows must reject it.
        assert!(parse_win(&["--visionos"]).is_err());
    }

    // -- new flags: --openexec / --visionos mapping (pure) -------------------

    #[test]
    fn bundle_spec_openexec_feature() {
        // Only the co-located desktop runtime declares an `openexec` feature; the
        // editors do not (conductor gets cook-side OpenExec via `cooking`).
        assert_eq!(BundleSpec::inspector().openexec_feature, None);
        assert_eq!(BundleSpec::poser().openexec_feature, None);
        assert_eq!(BundleSpec::conductor().openexec_feature, None);
        assert_eq!(BundleSpec::runtime().openexec_feature, Some("openexec"));
    }

    #[test]
    fn quest_runtime_options_map_profile_remote_openexec() {
        // Release bundle (debug=false) → release runtime; remote/openexec propagate.
        let o = quest_runtime_options(false, true, true);
        assert!(!o.is_debug(), "release bundle → release quest runtime");
        assert!(o.with_remote && o.with_openexec);
        assert!(!o.with_metrics);
        // Debug bundle → debug runtime; flags off.
        let o = quest_runtime_options(true, false, false);
        assert!(o.is_debug());
        assert!(!o.with_remote && !o.with_openexec);
    }

    #[test]
    fn visionos_runtime_options_map_profile_remote_openexec_device() {
        let o = visionos_runtime_options(false, true, true);
        assert!(!o.is_debug());
        assert!(!o.simulator, "bundling stages the device slice");
        assert!(o.with_remote && o.with_openexec && !o.with_metrics);
    }

    #[test]
    fn android_cross_options_keep_materialx_and_map_openexec() {
        let o = android_cross_options(true);
        assert!(o.materialx(), "MaterialX stays enabled for standalone fidelity");
        assert!(o.with_openexec);
        assert!(!android_cross_options(false).with_openexec);
    }

    // -- bundle-mac ---------------------------------------------------------

    #[test]
    fn mac_app_all_coverage() {
        // MacApp::all() must cover every app variant exactly once.
        let all = MacApp::all();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&MacApp::Inspector));
        assert!(all.contains(&MacApp::Poser));
        assert!(all.contains(&MacApp::Conductor));
    }

    #[test]
    fn mac_app_conductor_bundles_runtime() {
        // Conductor co-locates idtx-runtime (and stages assets/ + built-in/);
        // inspector/poser do not.
        assert!(!MacApp::Inspector.spec().bundles_runtime);
        assert!(!MacApp::Poser.spec().bundles_runtime);
        assert!(MacApp::Conductor.spec().bundles_runtime);
    }

    #[test]
    fn mac_app_spec_packages() {
        assert_eq!(MacApp::Inspector.spec().package, "idtx-inspector");
        assert_eq!(MacApp::Poser.spec().package, "idtx-poser");
        assert_eq!(MacApp::Conductor.spec().package, "idtx-conductor");
    }

    #[test]
    fn mac_bundle_options_defaults() {
        // No args → all three apps, release profile, remote off.
        let opts = parse_mac(&[]).unwrap();
        assert_eq!(opts.apps.len(), 3, "default should select all three apps");
        assert!(!opts.base.debug, "default profile should be release");
        assert!(!opts.base.remote, "remote should be off by default");
    }

    #[test]
    fn mac_bundle_options_single_app() {
        let opts = parse_mac(&["--app", "poser"]).unwrap();
        assert_eq!(opts.apps, vec![MacApp::Poser]);
    }

    #[test]
    fn mac_bundle_options_multiple_apps() {
        let opts = parse_mac(&["--app", "inspector", "--app", "conductor"]).unwrap();
        assert_eq!(opts.apps, vec![MacApp::Inspector, MacApp::Conductor]);
    }

    #[test]
    fn mac_bundle_options_all_dedup() {
        // `--app all --app conductor` must not bundle conductor twice.
        let opts = parse_mac(&["--app", "all", "--app", "conductor"]).unwrap();
        assert_eq!(opts.apps.len(), 3);
    }

    #[test]
    fn mac_bundle_options_flags() {
        let opts = parse_mac(&["--debug", "--remote"]).unwrap();
        assert!(opts.base.debug);
        assert!(opts.base.remote);
    }

    #[test]
    fn mac_bundle_options_openexec_and_visionos() {
        // Shared --openexec + mac-only --visionos / --visionos-rebuild-usd.
        let opts = parse_mac(&[]).unwrap();
        assert!(!opts.base.openexec);
        assert!(!opts.base.visionos);
        assert!(!opts.base.visionos_rebuild_usd);
        let opts =
            parse_mac(&["--openexec", "--visionos", "--visionos-rebuild-usd"]).unwrap();
        assert!(opts.base.openexec);
        assert!(opts.base.visionos);
        assert!(opts.base.visionos_rebuild_usd);
    }

    #[test]
    fn mac_bundle_options_android_flags() {
        // --android is shared (both bundlers); exercise it through bundle-mac too.
        let opts = parse_mac(&["--android", "--android-rebuild-usd"]).unwrap();
        assert!(opts.base.android);
        assert!(opts.base.android_rebuild_usd);
    }

    #[test]
    fn mac_bundle_options_profile_last_flag_wins() {
        // `--debug --release` → release (default), `--release --debug` → debug.
        assert!(!parse_mac(&["--debug", "--release"]).unwrap().base.debug);
        assert!(parse_mac(&["--release", "--debug"]).unwrap().base.debug);
    }

    #[test]
    fn mac_bundle_options_unknown_app_errors() {
        assert!(parse_mac(&["--app", "bogus"]).is_err());
    }

    #[test]
    fn mac_bundle_options_missing_app_value_errors() {
        assert!(parse_mac(&["--app"]).is_err());
    }
}
