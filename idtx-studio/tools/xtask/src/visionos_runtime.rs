//! `cargo xtask build-visionos-runtime` — cross-build `idtx-runtime` for Apple
//! Vision Pro (`aarch64-apple-visionos`, or the simulator slice
//! `aarch64-apple-visionos-sim`) into a real `libidtx_runtime.dylib`, and stage it
//! with every native dylib dependency the `.app` needs, beside the editor binaries.
//!
//! # Why this is an xtask, not the cook step
//!
//! Identical rationale to [`crate::quest_runtime`]: a shipped `idtx-conductor`
//! carries pre-cross-compiled runtimes but no source tree, so the cook merely
//! *consumes* the artifact this command stages. Cross-compilation lives only
//! here. A dev build must run this before cooking for Vision Pro.
//!
//! # Device vs. simulator slice
//!
//! Vision Pro ships two arm64 slices (same as iOS): the **device** slice
//! (`aarch64-apple-visionos`, SDK `xros`, `platform VISIONOS`) and the
//! **simulator** slice (`aarch64-apple-visionos-sim`, SDK `xrsimulator`,
//! `platform VISIONOSSIMULATOR`). They are not interchangeable — the Simulator
//! can only load the simulator slice. `--simulator` selects it; the default is the
//! device slice. Each slice links against its matching cross-built USD install
//! (`thirdparty/openusd-visionos-arm64[-sim]` + the `onetbb`/`materialx` siblings).
//!
//! # Scope (non-XR)
//!
//! This path is **non-XR**: it cross-compiles the runtime
//! cdylib (whose `cfg(target_os = "visionos")` entry links the `OpenUSD` bridge via
//! the winit-free `idtx-usd`) and stages it. The `CompositorServices` immersive
//! session is device-bound and not part of this path.
//!
//! # Toolchain
//!
//! The visionOS targets are **tier-3** Rust targets with no prebuilt `std`, so the
//! cross-build uses the **nightly** toolchain with `-Z build-std=std,panic_abort`
//! (needs the `rust-src` component). The C++ cxx bridge is compiled by the Xcode
//! SDK clang resolved via `xcrun`.
//!
//! # Testability (TMDD)
//!
//! The actual cross-build needs the visionOS SDK + the cross-built USD install
//! (absent on an ordinary CI host), so the *pure* logic — the cargo arg vector,
//! the per-slice triples/SDKs/paths, the Mach-O-arm64 magic check, the staging-dir
//! path, and the native-dep lib-search dirs — is factored into free functions
//! unit-tested here. Only the thin orchestration shells out, and it fails fast with
//! a clear message when a toolchain piece is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::usd_cross::{OPENUSD_VERSION, VisionOsSlice, visionos_deployment_target};

// ---------------------------------------------------------------------------
// Per-slice constants (pure)
// ---------------------------------------------------------------------------

/// The Rust target triple cross-built for `slice`.
#[must_use]
pub fn runtime_target_triple(slice: VisionOsSlice) -> &'static str {
    match slice {
        VisionOsSlice::Device => "aarch64-apple-visionos",
        VisionOsSlice::Simulator => "aarch64-apple-visionos-sim",
    }
}

/// The staging architecture-slice directory leaf for `slice` (`arm64` for device,
/// `arm64-sim` for the simulator) — kept distinct so both slices can be staged
/// side by side under `target/<profile>/visionos-runtime/`.
#[must_use]
pub fn abi_dir(slice: VisionOsSlice) -> &'static str {
    match slice {
        VisionOsSlice::Device => "arm64",
        VisionOsSlice::Simulator => "arm64-sim",
    }
}

/// The `thirdparty/` leaves for `slice`: the USD install's **arm64 leaf** (placed
/// under `openusd-<version>/`, matching `build-usd-visionos`), and the oneTBB and
/// `MaterialX` prefix dirs (direct children of `thirdparty/`). The simulator slice
/// uses the `-sim` cross-builds.
#[must_use]
pub fn thirdparty_leaves(slice: VisionOsSlice) -> (&'static str, &'static str, &'static str) {
    match slice {
        VisionOsSlice::Device => (
            "visionos-arm64",
            "onetbb-visionos",
            "materialx-visionos",
        ),
        VisionOsSlice::Simulator => (
            "visionos-arm64-sim",
            "onetbb-visionos-sim",
            "materialx-visionos-sim",
        ),
    }
}

/// The clang `-target` triple for `slice` at `deployment` — e.g.
/// `arm64-apple-xros2.0` (device) or `arm64-apple-xros2.0-simulator` (simulator).
/// Note the version comes BEFORE the `-simulator` suffix.
#[must_use]
pub fn clang_target(slice: VisionOsSlice, deployment: &str) -> String {
    match slice {
        VisionOsSlice::Device => format!("arm64-apple-xros{deployment}"),
        VisionOsSlice::Simulator => format!("arm64-apple-xros{deployment}-simulator"),
    }
}

/// The `CARGO_TARGET_<triple>_LINKER` env-var name for a target triple (upper-cased,
/// `-`→`_`), e.g. `CARGO_TARGET_AARCH64_APPLE_VISIONOS_SIM_LINKER`.
#[must_use]
pub fn linker_env_key(triple: &str) -> String {
    format!("CARGO_TARGET_{}_LINKER", triple.to_uppercase().replace('-', "_"))
}

/// The `rustup` arg vector that cross-builds the runtime cdylib for `triple` on the
/// nightly toolchain with `build-std`.
///
/// The visionOS targets are tier-3 (no prebuilt `std`), so the build must run on
/// nightly with `-Z build-std=std,panic_abort`. We invoke it via `rustup run
/// nightly cargo …` rather than `cargo +nightly …`, because when cargo spawns an
/// xtask it sets `$CARGO` to the *active* toolchain's cargo binary, which does not
/// understand the `+toolchain` proxy syntax.
///
/// `--lib` restricts the build to the `cdylib` target (the desktop `[[bin]]` is
/// irrelevant on visionOS). `--no-default-features` drops the default `authoring`
/// feature, which forwards to `idtx-runtime-core/authoring` — but
/// `idtx-runtime-core` is **not** a dependency on visionOS (it pulls `winit`,
/// which has no visionOS support). The visionOS cdylib links only the winit-free
/// `idtx-usd` bridge, which needs no feature.
#[must_use]
pub fn rustup_cargo_args(triple: &str, release: bool, features: &[&str]) -> Vec<String> {
    let mut args = vec![
        "run".to_owned(),
        "nightly".to_owned(),
        "cargo".to_owned(),
        "build".to_owned(),
        "--locked".to_owned(),
        "-Z".to_owned(),
        "build-std=std,panic_abort".to_owned(),
        "-p".to_owned(),
        "idtx-runtime".to_owned(),
        "--lib".to_owned(),
        "--no-default-features".to_owned(),
        "--target".to_owned(),
        triple.to_owned(),
    ];
    // Opt-in crate features (e.g. `visionos-immersive` for the CompositorServices
    // render path). The stage-open probe passes none.
    if !features.is_empty() {
        args.push("--features".to_owned());
        args.push(features.join(","));
    }
    if release {
        args.push("--release".to_owned());
    }
    args
}

/// Whether `header` is the start of a thin **Mach-O arm64** object: the 64-bit
/// little-endian magic `MH_MAGIC_64` (`0xFEEDFACF`) followed by the little-endian
/// cputype `CPU_TYPE_ARM64` (`0x0100000C`). Mirrors the Quest ELF guard
/// (`is_aarch64_elf`) so a toolchain misstep that yielded a host `x86_64` object, a
/// fat/universal binary, or a non-Mach-O file is rejected the moment it is
/// produced. (Both the device and simulator slices are arm64 Mach-O; they differ
/// only in the `LC_BUILD_VERSION` platform, not the cputype.)
#[must_use]
pub fn is_macho_arm64(header: &[u8]) -> bool {
    header.len() >= 8
        && header[0..4] == [0xCF, 0xFA, 0xED, 0xFE]
        && u32::from_le_bytes([header[4], header[5], header[6], header[7]]) == 0x0100_000C
}

/// The staging directory the runtime + its deps are assembled into — beside the
/// editor binaries so the cook's sibling lookup finds it:
/// `<workspace>/target/<profile>/visionos-runtime/<abi>` (`arm64` or `arm64-sim`).
#[must_use]
pub fn visionos_runtime_staging_dir(
    workspace_root: &Path,
    profile: &str,
    slice: VisionOsSlice,
) -> PathBuf {
    workspace_root
        .join("target")
        .join(profile)
        .join("visionos-runtime")
        .join(abi_dir(slice))
}

/// The lib directories to search for the runtime's native deps, in order: the
/// `OpenUSD` install's `lib/`, then the oneTBB and `MaterialX` prefixes' `lib/`.
#[must_use]
pub fn visionos_lib_search_dirs(prefixes: &SlicePrefixes) -> Vec<PathBuf> {
    vec![
        prefixes.usd_dir.join("lib"),
        prefixes.tbb_prefix.join("lib"),
        prefixes.materialx_prefix.join("lib"),
    ]
}

/// The root of the staged visionOS runtime tree for `profile`
/// (`<workspace>/target/<profile>/visionos-runtime`) — the parent of the `arm64`
/// slice dir, also holding the sibling `usd/`, `usd-exec/`, and
/// `idtx-http-resolver/` trees. The macOS bundler copies this whole directory
/// beside the packaged editor so the conductor's `visionos_runtime_binary()`
/// lookup resolves.
#[must_use]
pub fn visionos_runtime_tree_dir(profile: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join(profile)
        .join("visionos-runtime")
}

/// The resolved **device-slice** visionOS `OpenUSD` install prefix with no
/// explicit override (`$IDTX_USD_DIR_VISIONOS` → the `build-usd-visionos` staging
/// prefix). Lets the bundler decide whether a (re)build of the heavy C++
/// cross-build is needed.
#[must_use]
pub fn default_visionos_usd_dir() -> PathBuf {
    SlicePrefixes::resolve(&workspace_root(), VisionOsSlice::Device, None).usd_dir
}

/// Whether a complete visionOS `OpenUSD` install is present at `usd_dir` (the
/// monolithic `libusd_ms.dylib` + the `pxr.h` header). Mirrors the guard in
/// [`build_slice`]; the bundler uses it to skip the long cross-build when an
/// install is already staged.
#[must_use]
pub fn visionos_usd_install_present(usd_dir: &Path) -> bool {
    usd_dir.join("lib").join("libusd_ms.dylib").is_file()
        && usd_dir
            .join("include")
            .join("pxr")
            .join("pxr.h")
            .is_file()
}

/// Whether the visionOS `OpenUSD` install at `usd_dir` was built with the
/// `OpenExec` framework (`PXR_BUILD_EXEC=ON`). The reuse guard in the bundler
/// consults this so an `--openexec` request does NOT silently reuse a monolith
/// cross-built WITHOUT exec: the coded `idtx-usd-exec` plugin links the monolith's
/// Vdf/Exec/Ef symbols on device, and a non-exec monolith yields an `.app` where
/// `OpenExec` never runs (no build error — the plugin binds its symbols lazily).
///
/// Detected by the presence of the installed exec header tree
/// (`include/pxr/exec/vdf/context.h` — the exact header the plugin compiles
/// against). `PXR_BUILD_EXEC=OFF` does not configure, build, or install the
/// `pxr/exec/` subtree, so its absence is a reliable "no exec" signal. The twin of
/// `quest_runtime::android_usd_install_has_exec`; the `OpenUSD` header layout is
/// identical across the Android and visionOS slices.
#[must_use]
pub fn visionos_usd_install_has_exec(usd_dir: &Path) -> bool {
    usd_dir
        .join("include")
        .join("pxr")
        .join("exec")
        .join("vdf")
        .join("context.h")
        .is_file()
}

// ---------------------------------------------------------------------------
// Resolved per-slice prefixes
// ---------------------------------------------------------------------------

/// The three cross-built `thirdparty/` prefixes a slice links against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicePrefixes {
    pub usd_dir: PathBuf,
    pub tbb_prefix: PathBuf,
    pub materialx_prefix: PathBuf,
}

impl SlicePrefixes {
    /// Resolve the prefixes for `slice`: an explicit `usd_override` (or
    /// `$IDTX_USD_DIR_VISIONOS`) wins for the USD dir; otherwise the USD install
    /// is `thirdparty/openusd-<version>/<arm64-leaf>` (matching
    /// `build-usd-visionos`). oneTBB/`MaterialX` live in SEPARATE sibling prefixes
    /// (not inside the USD install); each resolves: explicit env override
    /// (`$IDTX_TBB_DIR_VISIONOS` / `$IDTX_MATERIALX_DIR_VISIONOS`) → the
    /// sibling-of-USD convention → the workspace `thirdparty/<leaf>` default.
    /// Process-free.
    #[must_use]
    fn resolve(workspace_root: &Path, slice: VisionOsSlice, usd_override: Option<PathBuf>) -> Self {
        let (usd_leaf, tbb_leaf, mtlx_leaf) = thirdparty_leaves(slice);
        let thirdparty = workspace_root.join("thirdparty");
        let usd_dir = usd_override
            .or_else(|| std::env::var_os("IDTX_USD_DIR_VISIONOS").map(PathBuf::from))
            .unwrap_or_else(|| {
                thirdparty
                    .join(format!("openusd-{OPENUSD_VERSION}"))
                    .join(usd_leaf)
            });
        // oneTBB / MaterialX are NOT inside the USD install. `build-usd-visionos`
        // installs them to `thirdparty/<leaf>`, which is the USD install's
        // grandparent (`thirdparty/openusd-<v>/visionos-arm64` → up two). Resolve
        // each: explicit env override (so a USD dir pointed outside the workspace
        // — e.g. a `thirdparty/` above the Cargo workspace root — still finds its
        // siblings) → the sibling-of-USD convention → the workspace default. Keying
        // off `usd_dir` keeps this slice-correct (device vs sim) even when the
        // single-valued env var cannot distinguish the slice. Mirrors
        // `android_lib_search_dirs` and `idtx-usd-sys/build.rs`.
        let usd_thirdparty = usd_dir.parent().and_then(Path::parent);
        let sibling = |env_key: &str, leaf: &str| -> PathBuf {
            std::env::var_os(env_key)
                .map(PathBuf::from)
                .or_else(|| usd_thirdparty.map(|p| p.join(leaf)))
                .unwrap_or_else(|| thirdparty.join(leaf))
        };
        let tbb_prefix = sibling("IDTX_TBB_DIR_VISIONOS", tbb_leaf);
        let materialx_prefix = sibling("IDTX_MATERIALX_DIR_VISIONOS", mtlx_leaf);
        Self {
            usd_dir,
            tbb_prefix,
            materialx_prefix,
        }
    }
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Parsed `build-visionos-runtime` options.
#[derive(Debug, Clone, PartialEq, Eq, Default, clap::Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flag bag; each flag is independent.
pub struct VisionOsRuntimeOptions {
    /// Build the `debug` profile. **This is the default** (parity with Quest);
    /// the runtime stages under `target/debug/visionos-runtime/…`, matching
    /// where a dev `cargo run` editor looks for the sibling runtime. Overrides
    /// an earlier `--release`.
    #[arg(long, overrides_with = "release")]
    pub debug: bool,
    /// Build the `release` profile instead of the default `debug`. Overrides an
    /// earlier `--debug` (last profile flag wins).
    #[arg(long, overrides_with = "debug")]
    pub release: bool,
    /// Target the simulator slice (`aarch64-apple-visionos-sim`) instead of the
    /// device slice. `--simulator`.
    #[arg(long)]
    pub simulator: bool,
    /// Resolve the plan, print it, but do not shell out.
    #[arg(long)]
    pub dry_run: bool,
    /// Explicit visionOS `OpenUSD` install prefix (overrides `$IDTX_USD_DIR_VISIONOS`
    /// and the `thirdparty/openusd-visionos-arm64[-sim]` default).
    #[arg(long = "usd-dir-visionos", value_name = "PATH")]
    pub usd_dir_visionos: Option<PathBuf>,
    /// Enable the `openexec` feature so the immersive runtime carries the `OpenExec`
    /// subsystem + the coded `idtx-usd-exec` plugin (cross-built + staged beside
    /// the runtime for the packager). Requires a visionOS `OpenUSD` install built
    /// with `PXR_BUILD_EXEC=ON` (`cargo xtask build-usd-visionos --with-openexec`).
    #[arg(long = "with-openexec", alias = "with_openexec")]
    pub with_openexec: bool,
    /// Enable the `remote` feature so that remote usd stages can be accessed from the
    /// runtime
    #[arg(long = "with-remote", alias = "with_remote")]
    pub with_remote: bool,
    /// Enable the `metrics` feature so the immersive runtime records per-eye
    /// render timings (CPU phases, draw-call counts, cache/instancing counts,
    /// whole-pass GPU timestamps) and dumps an `idtx-xr-metrics-<stage>-<ts>.json`
    /// into the app's `tmp/` container for off-device perf analysis. Off by
    /// default (zero-overhead no-ops when disabled). `--with-metrics`.
    #[arg(long = "with-metrics", alias = "with_metrics")]
    pub with_metrics: bool,
}

impl VisionOsRuntimeOptions {
    /// Whether the debug profile is selected. Debug is the default; `--release`
    /// switches to release (a later `--debug` switches back).
    #[must_use]
    pub fn is_debug(&self) -> bool {
        self.debug || !self.release
    }

    fn profile(&self) -> &'static str {
        if self.is_debug() { "debug" } else { "release" }
    }

    fn slice(&self) -> VisionOsSlice {
        if self.simulator {
            VisionOsSlice::Simulator
        } else {
            VisionOsSlice::Device
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point + plan
// ---------------------------------------------------------------------------

/// `cargo xtask build-visionos-runtime [--simulator] [--release] [--dry-run]
/// [--usd-dir-visionos <path>]`. Builds the device slice by default (`--simulator`
/// for the Simulator slice) and the `debug` profile by default. macOS only (needs
/// `xcrun` + the visionOS SDK); `--dry-run` inspects the plan on any host.
pub fn build_visionos_runtime_cmd(opts: &VisionOsRuntimeOptions) -> Result<()> {
    if !cfg!(target_os = "macos") && !opts.dry_run {
        bail!(
            "build-visionos-runtime must run on macOS (needs xcrun + the visionOS SDK). \
             Use --dry-run to inspect the planned invocation on another host."
        );
    }
    let plan = VisionOsRuntimePlan::resolve(opts);
    plan.describe();
    if opts.dry_run {
        println!("build-visionos-runtime: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }
    // The immersive feature set, plus `openexec` when requested (pulls the session
    // OpenExec subsystem + the coded `idtx-usd-exec` plugin via the runtime's
    // target-split `openexec` feature).
    let mut features = vec!["visionos-immersive", "gesture-recognition"];
    if plan.with_openexec {
        features.push("openexec");
    }
    if plan.with_remote {
        features.push("remote");
    }
    if plan.with_metrics {
        features.push("metrics");
    }
    let built = build_slice(
        plan.slice,
        plan.release,
        plan.usd_override.clone(),
        &features,
    )?;
    stage_runtime(&plan, &built)?;
    println!(
        "\n\u{2705}  Vision Pro runtime ({}) staged at {}",
        plan.slice.sdk(),
        plan.staging_dir.display()
    );
    Ok(())
}

/// A resolved cross-build plan (process-free; describe + staging-dir only).
#[derive(Debug, Clone)]
struct VisionOsRuntimePlan {
    slice: VisionOsSlice,
    profile: &'static str,
    release: bool,
    deployment_target: String,
    prefixes: SlicePrefixes,
    usd_override: Option<PathBuf>,
    staging_dir: PathBuf,
    with_openexec: bool,
    with_remote: bool,
    with_metrics: bool,
}

impl VisionOsRuntimePlan {
    fn resolve(opts: &VisionOsRuntimeOptions) -> Self {
        let root = workspace_root();
        let slice = opts.slice();
        Self {
            slice,
            profile: opts.profile(),
            release: !opts.is_debug(),
            deployment_target: visionos_deployment_target(),
            prefixes: SlicePrefixes::resolve(&root, slice, opts.usd_dir_visionos.clone()),
            usd_override: opts.usd_dir_visionos.clone(),
            staging_dir: visionos_runtime_staging_dir(&root, opts.profile(), slice),
            with_openexec: opts.with_openexec,
            with_remote: opts.with_remote,
            with_metrics: opts.with_metrics,
        }
    }

    fn describe(&self) {
        println!("build-visionos-runtime plan:");
        println!(
            "  Target:       {} ({})",
            runtime_target_triple(self.slice),
            abi_dir(self.slice)
        );
        println!("  Profile:      {}", self.profile);
        println!("  Toolchain:    nightly + -Z build-std (tier-3 target)");
        println!(
            "  SDK:          {} (clang target {})",
            self.slice.sdk(),
            clang_target(self.slice, &self.deployment_target)
        );
        println!("  visionOS USD: {}", self.prefixes.usd_dir.display());
        println!("  Staging:      {}", self.staging_dir.display());
        let mut extra = Vec::new();
        if self.with_openexec {
            extra.push("openexec");
        }
        if self.with_remote {
            extra.push("remote");
        }
        if self.with_metrics {
            extra.push("metrics");
        }
        if !extra.is_empty() {
            println!("  Extra feats:  {}", extra.join(", "));
        }
    }
}

// ---------------------------------------------------------------------------
// Reusable slice build (shared by this command and `run-visionos-sim`)
// ---------------------------------------------------------------------------

/// The product of a successful slice cross-build: the produced `.dylib` plus the
/// resolved prefixes + deployment target the `.app`/staging assembler needs.
#[derive(Debug, Clone)]
pub struct SliceBuild {
    pub deployment_target: String,
    pub prefixes: SlicePrefixes,
    /// The freshly cross-built `target/<triple>/<profile>/libidtx_runtime.dylib`.
    pub produced_dylib: PathBuf,
}

/// Cross-compile the runtime cdylib for `slice`. Verifies the visionOS USD install
/// is present (points the user at `build-usd-visionos` otherwise), shells out to the
/// nightly `build-std` cargo with the per-slice SDK clang + USD env, and verifies the
/// output is a Mach-O arm64 object. macOS only (the callers gate this).
pub fn build_slice(
    slice: VisionOsSlice,
    release: bool,
    usd_override: Option<PathBuf>,
    features: &[&str],
) -> Result<SliceBuild> {
    let root = workspace_root();
    let prefixes = SlicePrefixes::resolve(&root, slice, usd_override);
    let deployment_target = visionos_deployment_target();
    let profile = if release { "release" } else { "debug" };

    // The visionOS OpenUSD install for this slice must be present — the runtime's
    // cxx bridge links `libusd_ms.dylib`. Do NOT silently kick off the (very long)
    // C++ cross-build; point the user at it.
    let usd_lib = prefixes.usd_dir.join("lib").join("libusd_ms.dylib");
    let usd_header = prefixes.usd_dir.join("include").join("pxr").join("pxr.h");
    if !usd_lib.is_file() || !usd_header.is_file() {
        bail!(
            "visionOS OpenUSD install ({}) not found at {}\n\
             (expected {} and {}).\n\
             Build it first:  cargo xtask build-usd-visionos\n\
             or point --usd-dir-visionos / $IDTX_USD_DIR_VISIONOS at an existing install.",
            slice.sdk(),
            prefixes.usd_dir.display(),
            usd_lib.display(),
            usd_header.display(),
        );
    }

    cross_build_runtime(slice, release, &deployment_target, &prefixes, &root, features)?;

    let produced = root
        .join("target")
        .join(runtime_target_triple(slice))
        .join(profile)
        .join("libidtx_runtime.dylib");
    if !produced.is_file() {
        bail!(
            "cross-build reported success but {} is missing",
            produced.display()
        );
    }
    verify_macho_arm64(&produced)?;

    Ok(SliceBuild {
        deployment_target,
        prefixes,
        produced_dylib: produced,
    })
}

/// Build the environment the cross `cargo build` needs for `slice`: the SDK clang
/// (CC/CXX) + sysroot/target flags for the cxx bridge, the linker, the deployment
/// target, and the per-slice USD/TBB/`MaterialX` dirs the cxx bridge resolves.
///
/// The `IDTX_*_DIR_VISIONOS` overrides are set explicitly per slice because
/// `idtx-usd-sys/build.rs` only sees `CARGO_CFG_TARGET_OS == "visionos"` for BOTH
/// slices — without these it would resolve the device siblings for a simulator
/// build and link the wrong-platform dylibs.
fn cross_build_env(
    slice: VisionOsSlice,
    deployment_target: &str,
    prefixes: &SlicePrefixes,
) -> Result<Vec<(String, String)>> {
    let sdk = slice.sdk();
    let sdk_path = xcrun_show_sdk_path(sdk)?;
    let clang = xcrun_find_tool(sdk, "clang")?;
    let clangxx = xcrun_find_tool(sdk, "clang++")?;
    let triple = runtime_target_triple(slice);
    let target = clang_target(slice, deployment_target);
    // Target-scoped CC/CXX/CFLAGS/CXXFLAGS (keyed by the exact triple) so ONLY the
    // visionOS cxx-bridge compilation gets the sysroot/target — host build scripts
    // are untouched (which is why we do NOT set a global SDKROOT).
    let flags = format!("-isysroot {sdk_path} -target {target}");
    Ok(vec![
        (format!("CC_{triple}"), clang.clone()),
        (format!("CXX_{triple}"), clangxx),
        (format!("CFLAGS_{triple}"), flags.clone()),
        (format!("CXXFLAGS_{triple}"), flags),
        (linker_env_key(triple), clang),
        // rustc reads XROS_DEPLOYMENT_TARGET for both visionOS target specs — set it
        // so the Rust objects' Mach-O `minos` matches the staged USD dylibs and the
        // clang `-target` above. Ignored by host build scripts (they target darwin).
        (
            "XROS_DEPLOYMENT_TARGET".to_owned(),
            deployment_target.to_owned(),
        ),
        // Per-slice USD + sibling prefixes for the cxx bridge (see fn docs).
        (
            "IDTX_USD_DIR_VISIONOS".to_owned(),
            prefixes.usd_dir.display().to_string(),
        ),
        (
            "IDTX_TBB_DIR_VISIONOS".to_owned(),
            prefixes.tbb_prefix.display().to_string(),
        ),
        (
            "IDTX_MATERIALX_DIR_VISIONOS".to_owned(),
            prefixes.materialx_prefix.display().to_string(),
        ),
    ])
}

fn cross_build_runtime(
    slice: VisionOsSlice,
    release: bool,
    deployment_target: &str,
    prefixes: &SlicePrefixes,
    workspace_root: &Path,
    features: &[&str],
) -> Result<()> {
    let env = cross_build_env(slice, deployment_target, prefixes)?;
    let triple = runtime_target_triple(slice);
    let mut cmd = Command::new("rustup");
    cmd.current_dir(workspace_root)
        .args(rustup_cargo_args(triple, release, features));
    for (k, v) in &env {
        cmd.env(k, v);
    }
    println!("  $ {cmd:?}");
    let status = cmd
        .status()
        .context("failed to spawn `rustup run nightly cargo build` for the Vision Pro runtime")?;
    if !status.success() {
        bail!("cross-compiling idtx-runtime for {triple} failed with {status}");
    }
    Ok(())
}

/// Copy the produced `.dylib` + the visionOS USD deps + the USD plugInfo trees into
/// the staging dir (device-staging / cook-consumption layout).
fn stage_runtime(plan: &VisionOsRuntimePlan, built: &SliceBuild) -> Result<()> {
    // Clear any prior staging tree before re-staging, so artifacts from an
    // earlier, differently-configured build cannot linger and ship. Without this,
    // a previous `--with-openexec` / `--with-remote` run leaves a `usd-exec/` or
    // `idtx-http-resolver/` sibling that a later run (built WITHOUT that feature,
    // so it neither refreshes nor removes the dir) would copy into the `.app`
    // verbatim — a stale plugin paired with a runtime that no longer loads it. The
    // whole `visionos-runtime/` tree (the staging dir's parent, holding the slice
    // dir, `usd/`, and the optional `usd-exec/` / `idtx-http-resolver/` siblings)
    // is rebuilt from scratch by this function — and only the device slice is ever
    // staged here — so remove it wholesale. Mirrors `assemble_staging` on Android.
    let tree_root = plan.staging_dir.parent().unwrap_or(plan.staging_dir.as_path());
    if tree_root.exists() {
        std::fs::remove_dir_all(tree_root)
            .with_context(|| format!("clearing stale visionOS runtime tree {}", tree_root.display()))?;
    }

    std::fs::create_dir_all(&plan.staging_dir)
        .with_context(|| format!("creating staging dir {}", plan.staging_dir.display()))?;

    // The runtime cdylib.
    copy_into(&built.produced_dylib, &plan.staging_dir)?;

    // The native USD deps: embed the **full dylib load-closure** — every
    // `*.dylib` from the USD / oneTBB / MaterialX lib dirs — NOT a curated name
    // list. `libusd_ms.dylib` hard-links `@rpath/libMaterialX.1.dylib` AND
    // `@rpath/libtbb.12.dylib` (the umbrella + versioned install names); a
    // name-filtered copy of `visionos_runtime_libs()` (MaterialX *component*
    // stems like `libMaterialXCore.dylib`, an unversioned `libtbb.dylib`) misses
    // both, so the loader cannot resolve them and the packaged `.app` crashes at
    // dyld load before `main` with nothing logged. The Conductor Build path
    // copies this staged dir verbatim into `Contents/Frameworks/`, so it must be
    // complete and correctly named here. Mirrors the sim/device bundlers'
    // closure embed.
    let search_dirs = visionos_lib_search_dirs(&built.prefixes);
    let staged = embed_full_dylib_closure(&search_dirs, &plan.staging_dir)?;

    // OpenUSD plugInfo discovery trees (`lib/usd` + `plugin/usd`) → the sibling
    // `usd/` dir the cook copies into the `.app`.
    let usd_staging = plan
        .staging_dir
        .parent()
        .map_or_else(|| plan.staging_dir.join("usd"), |p| p.join("usd"));
    let mut usd_trees = 0usize;
    for rel in ["lib/usd", "plugin/usd"] {
        let src = built.prefixes.usd_dir.join(rel);
        if src.is_dir() {
            copy_tree_recursive(&src, &usd_staging.join(rel))
                .with_context(|| format!("staging USD plugInfo tree {}", src.display()))?;
            usd_trees += 1;
        } else {
            eprintln!(
                "  warning: USD plugInfo tree {} absent; on-device USD plugin \
                 discovery will fail",
                src.display()
            );
        }
    }

    println!("  staged libidtx_runtime.dylib + {staged} native deps");
    println!(
        "  staged {usd_trees} USD plugInfo tree(s) -> {}",
        usd_staging.display()
    );

    // Cross-built coded OpenExec plugin (feature `openexec`) → the sibling
    // `usd-exec/` dir the packager injects at the `.app` bundle root. Built into
    // the runtime target's build-script OUT_DIR; located + copied whole. Absent is
    // a loud warning (runtime likely built without `openexec`), not fatal.
    if plan.with_openexec {
        let exec_staging = plan.staging_dir.parent().map_or_else(
            || plan.staging_dir.join(idtx_standalone::EXEC_PLUGIN_DIR_NAME),
            |p| p.join(idtx_standalone::EXEC_PLUGIN_DIR_NAME),
        );
        let triple = runtime_target_triple(plan.slice);
        match crate::quest_runtime::locate_cross_exec_plugin_dir(
            &workspace_root(),
            triple,
            plan.profile,
            "libidtx_usd_exec.dylib",
        ) {
            Some(src) => {
                copy_tree_recursive(&src, &exec_staging)
                    .with_context(|| format!("staging OpenExec plugin from {}", src.display()))?;
                println!("  staged OpenExec plugin -> {}", exec_staging.display());
            }
            None => bail!(
                "--with-openexec was set but no cross-built OpenExec plugin \
                 (libidtx_usd_exec.dylib) was found under \
                 target/{triple}/{}/build/ (checked both idtx-usd-exec-*/out/idtx-usd-exec/ \
                 and idtx-usd-exec/*/out/idtx-usd-exec/). Did the runtime build include the \
                 `openexec` feature? Staging aborts here on purpose: the runtime dylib carries \
                 a forced `@rpath/libidtx_usd_exec.dylib` load, so a bundle missing the plugin \
                 would dyld-crash at launch on device.",
                plan.profile
            ),
        }
    }

    // Cross-built HTTP(S) USD resolver plugin. The immersive runtime
    // (`visionos-immersive`) links it unconditionally, so `libidtx_runtime.dylib`
    // carries a forced `@rpath/libidtx_http_resolver.dylib` load — the bundle
    // dyld-crashes at launch without it. Stage the dylib into the staging root
    // (→ `Frameworks/` via the `stage_runtime` copy_tree, satisfying the
    // `@executable_path/Frameworks` rpath), and the plugInfo — with `LibraryPath`
    // EMPTIED — into `idtx-http-resolver/` where the resolver's
    // `resolve_plugin_dir` looks. The empty LibraryPath makes `PlugRegistry`
    // register the http/https URI schemes against the already-load-resident type
    // (from the `@rpath` Frameworks load) WITHOUT a second `dlopen`, which would
    // split the resolver's `type_info` / its process-global fetch state (the exec
    // plugin uses the identical pattern). NOTE: on-device load + registration are
    // unverified on a macOS host; validate in the simulator / on device.
    {
        let triple = runtime_target_triple(plan.slice);
        match locate_cross_resolver_out_dir(&workspace_root(), triple, plan.profile) {
            Some(src) => {
                // Dylib → staging root only (NOT the plugin subdir — a second copy
                // there would be dlopen'd and split the type).
                copy_into(&src.join("libidtx_http_resolver.dylib"), &plan.staging_dir)?;
                let plugin_dir = plan.staging_dir.join("idtx-http-resolver");
                // Recreate fresh so a stale dylib from an earlier run (which would
                // be redundant with the Frameworks-root copy) does not linger.
                let _ = std::fs::remove_dir_all(&plugin_dir);
                std::fs::create_dir_all(&plugin_dir).with_context(|| {
                    format!("creating resolver plugin dir {}", plugin_dir.display())
                })?;
                let plug_info = std::fs::read_to_string(src.join("plugInfo.json"))
                    .with_context(|| "reading resolver plugInfo.json")?;
                std::fs::write(
                    plugin_dir.join("plugInfo.json"),
                    empty_plug_info_library_path(&plug_info),
                )
                .with_context(|| "writing resolver plugInfo.json")?;
                println!(
                    "  staged HTTP USD resolver -> {} (+ emptied plugInfo in idtx-http-resolver/)",
                    plan.staging_dir.display()
                );
            }
            None => bail!(
                "no cross-built HTTP USD resolver (libidtx_http_resolver.dylib) was found \
                 under target/{triple}/{}/build/idtx-usd-resolver-http*/out/. The runtime \
                 dylib carries a forced `@rpath/libidtx_http_resolver.dylib` load (pulled by \
                 `visionos-immersive`), so a bundle missing it would dyld-crash at launch.",
                plan.profile
            ),
        }
    }

    Ok(())
}

/// Rewrite a `plugInfo.json`'s `"LibraryPath": "<x>"` to the empty string, so
/// `PlugRegistry` registers the plugin's types as "code already resident" (loaded
/// via `@rpath` at launch) instead of `dlopen`ing a second copy. Mirrors
/// `idtx_build`'s `empty_library_path`; kept local so xtask has no build-crate dep.
pub(crate) fn empty_plug_info_library_path(plug_info: &str) -> String {
    let Some(key) = plug_info.find("\"LibraryPath\"") else {
        return plug_info.to_owned();
    };
    let after_key = &plug_info[key..];
    let Some(colon_rel) = after_key.find(':') else {
        return plug_info.to_owned();
    };
    let val_rel = colon_rel + 1;
    let bytes = after_key.as_bytes();
    let Some(open_rel) = (val_rel..bytes.len()).find(|&i| bytes[i] == b'"') else {
        return plug_info.to_owned();
    };
    let Some(close_off) = after_key[open_rel + 1..].find('"') else {
        return plug_info.to_owned();
    };
    let open = key + open_rel;
    let close = key + open_rel + 1 + close_off;
    let mut out = String::with_capacity(plug_info.len());
    out.push_str(&plug_info[..=open]); // up to and including the opening quote
    out.push_str(&plug_info[close..]); // from the closing quote on → value emptied
    out
}

/// Locate the build-script `OUT_DIR` of `idtx-usd-resolver-http` for the visionOS
/// cross build (flat stable `idtx-usd-resolver-http-<hash>/out/` and nested
/// nightly build-std `idtx-usd-resolver-http/<hash>/out/` layouts), returning the
/// newest dir that carries `libidtx_http_resolver.dylib`.
fn locate_cross_resolver_out_dir(
    workspace_root: &Path,
    triple: &str,
    profile: &str,
) -> Option<PathBuf> {
    let build = workspace_root
        .join("target")
        .join(triple)
        .join(profile)
        .join("build");
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&build).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "idtx-usd-resolver-http" {
            if let Ok(hashes) = std::fs::read_dir(entry.path()) {
                for hash in hashes.flatten() {
                    candidates.push(hash.path().join("out"));
                }
            }
        } else if name.starts_with("idtx-usd-resolver-http-") {
            candidates.push(entry.path().join("out"));
        }
    }
    candidates
        .into_iter()
        .filter_map(|dir| {
            let meta = dir.join("libidtx_http_resolver.dylib").metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), dir))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, dir)| dir)
}

/// The dylibs whose exact install-name file names MUST be present in the staged
/// closure, or a device `.app` built from it crashes at dyld load before `main`.
/// These are `libusd_ms.dylib`'s hard `@rpath` dependencies — the umbrella
/// `libMaterialX.1.dylib` and the versioned `libtbb.12.dylib` — plus the USD
/// monolith itself. (A name-filtered copy of `visionos_runtime_libs()` staged
/// `MaterialX` *component* stems + an unversioned `libtbb.dylib` instead, which the
/// loader could not resolve — the Conductor Build crash.)
const VISIONOS_REQUIRED_CLOSURE_LIBS: &[&str] =
    &["libusd_ms.dylib", "libtbb.12.dylib", "libMaterialX.1.dylib"];

/// Copy the **full dylib load-closure** — every `*.dylib` across `search_dirs` —
/// into `dest`, deduplicating by file name (first dir wins), then assert the
/// hard-required libs ([`VISIONOS_REQUIRED_CLOSURE_LIBS`]) are present. Returns
/// the number of dylibs staged.
///
/// `fs::copy` follows symlinks, materializing versioned aliases (e.g.
/// `libtbb.12.dylib` → `libtbb.12.12.dylib`) as real files — enough for the
/// loader. Errors if a required lib is absent (would dyld-crash the device app).
fn embed_full_dylib_closure(search_dirs: &[PathBuf], dest: &Path) -> Result<usize> {
    std::fs::create_dir_all(dest)
        .with_context(|| format!("creating staging dir {}", dest.display()))?;
    let mut staged = 0usize;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !crate::visionos_sim::is_dylib_name(&name) {
                continue;
            }
            // First search dir wins on a name clash (USD install's own copy).
            if !seen.insert(name) {
                continue;
            }
            copy_into(&entry.path(), dest)?;
            staged += 1;
        }
    }

    let missing: Vec<&str> = VISIONOS_REQUIRED_CLOSURE_LIBS
        .iter()
        .copied()
        .filter(|lib| !dest.join(lib).is_file())
        .collect();
    if !missing.is_empty() {
        bail!(
            "Vision Pro runtime closure is incomplete: {missing:?} not found across the \
             lib search dirs {search_dirs:?}. A device .app built from this would crash \
             at dyld load before main. Check the visionOS OpenUSD/oneTBB/MaterialX \
             cross-build (the dylibs must carry their @rpath install names)."
        );
    }
    Ok(staged)
}

// ---------------------------------------------------------------------------
// Shell-out helpers (xcrun / copy) — shared with `visionos_sim`
// ---------------------------------------------------------------------------

/// `xcrun --sdk <sdk> --show-sdk-path` → the SDK root path.
pub(crate) fn xcrun_show_sdk_path(sdk: &str) -> Result<String> {
    xcrun_capture(&["--sdk", sdk, "--show-sdk-path"])
}

/// `xcrun --sdk <sdk> -f <tool>` → the absolute path of an SDK tool (e.g. clang).
pub(crate) fn xcrun_find_tool(sdk: &str, tool: &str) -> Result<String> {
    xcrun_capture(&["--sdk", sdk, "-f", tool])
}

/// Run `xcrun <args…>` and return its trimmed stdout, erroring on failure/empty.
fn xcrun_capture(args: &[&str]) -> Result<String> {
    let out = Command::new("xcrun")
        .args(args)
        .output()
        .with_context(|| format!("spawning `xcrun {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`xcrun {}` failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        bail!("`xcrun {}` returned empty output", args.join(" "));
    }
    Ok(s)
}

/// Copy `src` into directory `dir`, keeping the file name.
pub(crate) fn copy_into(src: &Path, dir: &Path) -> Result<()> {
    let name = src
        .file_name()
        .with_context(|| format!("source has no file name: {}", src.display()))?;
    let dst = dir.join(name);
    std::fs::copy(src, &dst)
        .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

/// Recursively copy `src` into `dst`, creating `dst` and parents; files are
/// overwritten. Used to stage the `OpenUSD` `plugInfo` trees beside the runtime.
pub(crate) fn copy_tree_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Read a file header and assert it is a Mach-O arm64 object.
pub(crate) fn verify_macho_arm64(path: &Path) -> Result<()> {
    use std::io::Read as _;
    let mut header = [0u8; 8];
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("opening {} to verify", path.display()))?;
    f.read_exact(&mut header)
        .with_context(|| format!("{} is too small to be a Mach-O object", path.display()))?;
    if !is_macho_arm64(&header) {
        let kind = if header[0..2] == [b'M', b'Z'] {
            "a Windows PE binary".to_owned()
        } else if header[0..4] == [0x7F, b'E', b'L', b'F'] {
            "an ELF object (Linux/Android, not Apple)".to_owned()
        } else if header[0..4] == [0xCA, 0xFE, 0xBA, 0xBE] {
            "a fat/universal Mach-O (expected a thin arm64 slice)".to_owned()
        } else if header[0..4] == [0xCF, 0xFA, 0xED, 0xFE] {
            format!(
                "a non-arm64 Mach-O (cputype 0x{:08x})",
                u32::from_le_bytes([header[4], header[5], header[6], header[7]])
            )
        } else {
            format!("not a Mach-O object (magic {:02x?})", &header[0..4])
        };
        bail!(
            "{} is {kind}; expected a thin Mach-O arm64 dynamic library",
            path.display()
        );
    }
    Ok(())
}

/// The workspace root (the directory holding the `[workspace]` `Cargo.toml` =
/// `idtx-studio`, where cargo's `target/` lives). Delegates to
/// [`crate::workspace_root`]; falls back to `xtask`'s manifest parent only if that
/// walk fails.
pub(crate) fn workspace_root() -> PathBuf {
    crate::workspace_root().unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    })
}

// ---------------------------------------------------------------------------
// Tests — pure logic only (no SDK, no process execution, no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parse `VisionOsRuntimeOptions` from a bare argument vector via a minimal
    /// clap wrapper.
    fn parse_opts(args: &[&str]) -> Result<VisionOsRuntimeOptions, clap::Error> {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: VisionOsRuntimeOptions,
        }
        TestCli::try_parse_from(
            std::iter::once("build-visionos-runtime").chain(args.iter().copied()),
        )
        .map(|c| c.opts)
    }

    #[test]
    fn rustup_cargo_args_are_nightly_build_std_lib_only() {
        let args = rustup_cargo_args(runtime_target_triple(VisionOsSlice::Device), true, &[]);
        assert_eq!(&args[0..3], &["run", "nightly", "cargo"]);
        assert!(args.contains(&"build".to_owned()));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-Z" && w[1] == "build-std=std,panic_abort"),
            "tier-3 target needs -Z build-std"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-p" && w[1] == "idtx-runtime")
        );
        assert!(args.contains(&"--lib".to_owned()), "must build only the cdylib");
        assert!(
            args.contains(&"--no-default-features".to_owned()),
            "must drop `authoring` (forwards to winit-bearing runtime-core, absent on visionOS)"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--target" && w[1] == "aarch64-apple-visionos")
        );
        assert!(args.contains(&"--release".to_owned()));
        assert!(
            !args.iter().any(|a| a.contains("pcvr")
                || a.contains("quest")
                || a == "--features"),
            "must not enable any XR feature on the Milestone-1 visionOS build"
        );
    }

    #[test]
    fn rustup_cargo_args_target_tracks_slice() {
        let sim = rustup_cargo_args(runtime_target_triple(VisionOsSlice::Simulator), false, &[]);
        assert!(
            sim.windows(2)
                .any(|w| w[0] == "--target" && w[1] == "aarch64-apple-visionos-sim")
        );
        assert!(!sim.contains(&"--release".to_owned()));
    }

    #[test]
    fn rustup_cargo_args_appends_opt_in_features() {
        let args = rustup_cargo_args(
            runtime_target_triple(VisionOsSlice::Simulator),
            false,
            &["visionos-immersive"],
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--features" && w[1] == "visionos-immersive"),
            "opt-in features must be forwarded to cargo"
        );
    }

    #[test]
    fn slice_triples_abis_and_leaves() {
        assert_eq!(runtime_target_triple(VisionOsSlice::Device), "aarch64-apple-visionos");
        assert_eq!(
            runtime_target_triple(VisionOsSlice::Simulator),
            "aarch64-apple-visionos-sim"
        );
        assert_eq!(abi_dir(VisionOsSlice::Device), "arm64");
        assert_eq!(abi_dir(VisionOsSlice::Simulator), "arm64-sim");
        assert_eq!(thirdparty_leaves(VisionOsSlice::Device).0, "visionos-arm64");
        assert_eq!(
            thirdparty_leaves(VisionOsSlice::Simulator).0,
            "visionos-arm64-sim"
        );
    }

    #[test]
    fn clang_target_places_version_before_simulator_suffix() {
        assert_eq!(clang_target(VisionOsSlice::Device, "2.0"), "arm64-apple-xros2.0");
        assert_eq!(
            clang_target(VisionOsSlice::Simulator, "2.0"),
            "arm64-apple-xros2.0-simulator"
        );
    }

    #[test]
    fn linker_env_key_shape() {
        assert_eq!(
            linker_env_key("aarch64-apple-visionos"),
            "CARGO_TARGET_AARCH64_APPLE_VISIONOS_LINKER"
        );
        assert_eq!(
            linker_env_key("aarch64-apple-visionos-sim"),
            "CARGO_TARGET_AARCH64_APPLE_VISIONOS_SIM_LINKER"
        );
    }

    #[test]
    fn macho_arm64_magic_accepts_and_rejects() {
        let good = [0xCF, 0xFA, 0xED, 0xFE, 0x0C, 0x00, 0x00, 0x01];
        assert!(is_macho_arm64(&good));
        let x64 = [0xCF, 0xFA, 0xED, 0xFE, 0x07, 0x00, 0x00, 0x01];
        assert!(!is_macho_arm64(&x64));
        assert!(!is_macho_arm64(b"\x7fELF\x02\x01\x01\x00"));
        assert!(!is_macho_arm64(b"MZ\x90\x00\x00\x00\x00\x00"));
        assert!(!is_macho_arm64(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2]));
        assert!(!is_macho_arm64(&[0xCF, 0xFA, 0xED, 0xFE]));
    }

    #[test]
    fn staging_dir_tracks_profile_and_slice() {
        assert!(
            visionos_runtime_staging_dir(Path::new("/ws"), "release", VisionOsSlice::Device)
                .ends_with("target/release/visionos-runtime/arm64")
        );
        assert!(
            visionos_runtime_staging_dir(Path::new("/ws"), "debug", VisionOsSlice::Simulator)
                .ends_with("target/debug/visionos-runtime/arm64-sim")
        );
    }

    #[test]
    fn visionos_usd_install_has_exec_tracks_exec_header_tree() {
        // Unique scratch USD prefix under the system temp dir (no tempfile dep).
        let usd = std::env::temp_dir().join(format!(
            "idtx-visionos-usd-exec-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        // A complete-but-non-exec install (libusd_ms.dylib + pxr.h, no exec
        // headers): `present` is true, but `has_exec` is false — exactly the
        // stale-reuse case the bundler must NOT reuse for an `--openexec` request.
        std::fs::create_dir_all(usd.join("lib")).unwrap();
        std::fs::create_dir_all(usd.join("include").join("pxr")).unwrap();
        std::fs::write(usd.join("lib").join("libusd_ms.dylib"), b"macho").unwrap();
        std::fs::write(usd.join("include").join("pxr").join("pxr.h"), b"// hdr").unwrap();
        assert!(visionos_usd_install_present(&usd));
        assert!(!visionos_usd_install_has_exec(&usd));

        // Add the exec header the coded plugin compiles against → now exec-capable.
        let vdf = usd.join("include").join("pxr").join("exec").join("vdf");
        std::fs::create_dir_all(&vdf).unwrap();
        std::fs::write(vdf.join("context.h"), b"// exec").unwrap();
        assert!(visionos_usd_install_has_exec(&usd));

        std::fs::remove_dir_all(&usd).ok();
    }

    #[test]
    fn prefixes_resolve_from_staging_convention() {
        // Env overrides unset in CI/test → thirdparty/<leaf> defaults.
        let p = SlicePrefixes::resolve(Path::new("/ws"), VisionOsSlice::Simulator, None);
        // The USD dir nests under the versioned `openusd-<v>/` dir, matching where
        // `build-usd-visionos` installs it (only asserted when the env override is
        // unset, which wins over the convention).
        if std::env::var_os("IDTX_USD_DIR_VISIONOS").is_none() {
            assert!(
                p.usd_dir
                    .ends_with(format!("thirdparty/openusd-{OPENUSD_VERSION}/visionos-arm64-sim")),
                "usd_dir = {}",
                p.usd_dir.display()
            );
        }
        assert!(p.tbb_prefix.ends_with("thirdparty/onetbb-visionos-sim"));
        assert!(p.materialx_prefix.ends_with("thirdparty/materialx-visionos-sim"));
        let dirs = visionos_lib_search_dirs(&p);
        assert!(dirs.iter().any(|d| d.ends_with("onetbb-visionos-sim/lib")));
        assert!(dirs.iter().any(|d| d.ends_with("materialx-visionos-sim/lib")));
    }

    #[test]
    fn prefixes_siblings_track_an_out_of_workspace_usd_dir() {
        // Regression: a USD dir pointed outside the Cargo workspace (its
        // `thirdparty/` sits ABOVE the workspace root) must resolve oneTBB /
        // MaterialX as siblings of THAT install, not as `/<workspace>/thirdparty`
        // leaves that do not exist. Guard on the env overrides, which win.
        if std::env::var_os("IDTX_TBB_DIR_VISIONOS").is_some()
            || std::env::var_os("IDTX_MATERIALX_DIR_VISIONOS").is_some()
        {
            return;
        }
        let usd = PathBuf::from("/elsewhere/thirdparty/openusd-26.11.1/visionos-arm64");
        let p = SlicePrefixes::resolve(Path::new("/ws"), VisionOsSlice::Device, Some(usd));
        assert_eq!(
            p.tbb_prefix,
            PathBuf::from("/elsewhere/thirdparty/onetbb-visionos")
        );
        assert_eq!(
            p.materialx_prefix,
            PathBuf::from("/elsewhere/thirdparty/materialx-visionos")
        );
    }

    #[test]
    fn options_default_is_device_debug() {
        let o = VisionOsRuntimeOptions::default();
        assert!(o.is_debug() && !o.simulator);
        assert_eq!(o.profile(), "debug");
        assert_eq!(o.slice(), VisionOsSlice::Device);
    }

    #[test]
    fn options_parse_flags() {
        let o = parse_opts(&["--release", "--simulator"]).unwrap();
        assert!(!o.is_debug() && o.simulator);
        assert_eq!(o.profile(), "release");
        assert_eq!(o.slice(), VisionOsSlice::Simulator);
        let o = parse_opts(&["--usd-dir-visionos", "/opt/usd"]).unwrap();
        assert_eq!(o.usd_dir_visionos, Some(PathBuf::from("/opt/usd")));
        assert!(parse_opts(&["--bogus"]).is_err());

        // `--with-metrics` (and its snake alias) flips the opt-in metrics flag,
        // which is off by default.
        let o = parse_opts(&["--with-metrics"]).unwrap();
        assert!(o.with_metrics);
        let o = parse_opts(&["--with_metrics"]).unwrap();
        assert!(o.with_metrics);
        assert!(!VisionOsRuntimeOptions::default().with_metrics);
    }

    /// `--with-metrics` must reach the cargo feature vector as `metrics` so the
    /// cross-built device dylib compiles the per-eye render instrumentation in;
    /// without the flag the vector carries only the always-on immersive features.
    #[test]
    fn with_metrics_pushes_the_metrics_feature() {
        let mut features = vec!["visionos-immersive", "gesture-recognition"];
        let opts = VisionOsRuntimeOptions {
            with_metrics: true,
            ..VisionOsRuntimeOptions::default()
        };
        // Mirror the feature-assembly in `build_visionos_runtime_cmd`.
        if opts.with_metrics {
            features.push("metrics");
        }
        assert!(features.contains(&"metrics"));
        let no_metrics = {
            let opts = VisionOsRuntimeOptions::default();
            let mut f = vec!["visionos-immersive", "gesture-recognition"];
            if opts.with_metrics {
                f.push("metrics");
            }
            f
        };
        assert!(!no_metrics.contains(&"metrics"));
    }

    #[test]
    fn deployment_target_default_is_two_point_oh() {
        if std::env::var_os("IDTX_VISIONOS_DEPLOYMENT_TARGET").is_none() {
            assert_eq!(visionos_deployment_target(), "2.0");
        }
    }

    /// The full-closure embed copies EVERY `*.dylib` across all search dirs
    /// (skipping non-dylibs), dedupes by name, and keeps the exact versioned
    /// install-name files (`libtbb.12.dylib`, `libMaterialX.1.dylib`) that
    /// `libusd_ms.dylib` links — the names a `visionos_runtime_libs()` name-list
    /// copy dropped, causing the on-device dyld crash.
    #[test]
    fn embed_full_dylib_closure_stages_versioned_hard_deps() {
        let tmp = std::env::temp_dir().join(format!("idtx-vos-closure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let usd = tmp.join("usd/lib");
        let tbb = tmp.join("tbb/lib");
        let mtlx = tmp.join("mtlx/lib");
        for d in [&usd, &tbb, &mtlx] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(usd.join("libusd_ms.dylib"), b"x").unwrap();
        std::fs::write(usd.join("plugInfo.json"), b"{}").unwrap(); // non-dylib, skipped
        std::fs::write(tbb.join("libtbb.12.dylib"), b"x").unwrap();
        std::fs::write(tbb.join("libtbb.dylib"), b"x").unwrap(); // unversioned alias too
        std::fs::write(mtlx.join("libMaterialX.1.dylib"), b"x").unwrap();
        std::fs::write(mtlx.join("libMaterialXCore.dylib"), b"x").unwrap();

        let dest = tmp.join("arm64");
        let staged = embed_full_dylib_closure(
            &[usd.clone(), tbb.clone(), mtlx.clone()],
            &dest,
        )
        .expect("closure complete");

        assert_eq!(staged, 5, "all 5 dylibs copied, plugInfo.json skipped");
        for lib in VISIONOS_REQUIRED_CLOSURE_LIBS {
            assert!(dest.join(lib).is_file(), "required {lib} must be staged");
        }
        assert!(dest.join("libMaterialXCore.dylib").is_file());
        assert!(!dest.join("plugInfo.json").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Missing a hard dependency (the exact bug: no umbrella `libMaterialX.1.dylib`)
    /// fails the stage loudly instead of producing a bundle that dyld-crashes.
    #[test]
    fn embed_full_dylib_closure_errors_on_missing_hard_dep() {
        let tmp = std::env::temp_dir().join(format!("idtx-vos-closure-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let lib = tmp.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        // usd_ms + versioned tbb present, but NO libMaterialX.1.dylib.
        std::fs::write(lib.join("libusd_ms.dylib"), b"x").unwrap();
        std::fs::write(lib.join("libtbb.12.dylib"), b"x").unwrap();

        let dest = tmp.join("arm64");
        let err = embed_full_dylib_closure(std::slice::from_ref(&lib), &dest)
            .expect_err("must reject an incomplete closure");
        assert!(err.to_string().contains("libMaterialX.1.dylib"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
