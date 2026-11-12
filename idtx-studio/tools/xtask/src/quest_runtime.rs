//! `cargo xtask build-quest-runtime` — cross-build `idtx-runtime` for Quest
//! (`aarch64-linux-android`) into a real `libidtx_runtime.so`, and stage it with
//! every native dependency the APK needs, beside the editor binaries.
//!
//! # Why this is an xtask, not the cook step
//!
//! The `idtx-conductor` ships as a bundled application that carries
//! pre-cross-compiled `idtx-runtime` binaries/libs for mac/windows/android. The
//! conductor's *source tree* is not shipped, so a shipped conductor can package
//! the Android target **without** cross-compiling the runtime itself. Therefore
//! cross-compilation lives only here; the cook step
//! (`idtx-studio-shell::build_job`) merely consumes the artifact this command
//! stages and fails closed when it is absent. Running this during a cook would
//! both be impossible in a shipped build (no source) and wrong (the cook host is
//! not necessarily the cross-build host).
//!
//! # What it produces
//!
//! `target/<profile>/android-runtime/arm64-v8a/` containing:
//! * `libidtx_runtime.so` — the cross-built runtime cdylib (ELF aarch64).
//! * the 12 [`idtx_standalone::android_runtime_libs`] native `.so`s (`OpenUSD`
//!   monolith, TBB, 9× `MaterialX` generation libs, `libc++_shared.so`), from the
//!   Android USD install.
//! * `libopenxr_loader.so` — the Meta/Khronos loader, fetched from Maven
//!   (for the XR link; **not** currently linked).
//!
//! The cook / bundlers co-locate this directory as
//! `<editor_dir>/android-runtime/arm64-v8a/`.
//!
//! # Testability (TMDD)
//!
//! The link needs the NDK + the cross-built Android USD install (absent on an
//! ordinary CI host), so the *pure* logic — the cargo arg vector, the Maven URL
//! + inner-AAR path, the ELF-aarch64 magic check, the NDK tool-name candidates,
//! and the staging-dir path — is factored into free functions unit-tested here.
//! Only the thin orchestration ([`run`]) shells out, and it fails fast with a
//! clear message when a toolchain piece is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use idtx_standalone::{StandalonePlatform, android_runtime_libs};

use crate::usd_cross::{AndroidToolchain, DEFAULT_ANDROID_API_LEVEL, staging_prefix};

/// The Rust target triple cross-built for Quest.
pub const RUNTIME_TARGET_TRIPLE: &str = "aarch64-linux-android";

/// The Android native-ABI directory (Quest is 64-bit ARM only).
pub const ANDROID_ABI_DIR: &str = "arm64-v8a";

/// Pinned Khronos `OpenXR` loader-for-Android version. Bump to the current release
/// as needed; validated on the first real fetch (the download is user/device-run,
/// never in CI). The loader is **bundled** in the APK `lib/`, never a system lib.
pub const OPENXR_LOADER_AAR_VERSION: &str = "1.1.43";

/// Maven base for the login-free `openxr_loader_for_android` artifact.
pub const OPENXR_LOADER_MAVEN_BASE: &str =
    "https://repo1.maven.org/maven2/org/khronos/openxr/openxr_loader_for_android";

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Parsed `build-quest-runtime` options.
#[derive(Debug, Clone, PartialEq, Eq, Default, clap::Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flag bag; each flag is independent.
pub struct QuestRuntimeOptions {
    /// Build the `debug` profile. **This is the default**; it keeps the runtime
    /// staged under `target/debug/android-runtime/…`, matching where a dev
    /// `cargo run` editor (itself a debug build) looks for the sibling runtime
    /// (`idtx-studio-shell::build_job::quest_runtime_binary` resolves it via
    /// `current_exe().parent()`). Overrides an earlier `--release`.
    #[arg(long, overrides_with = "release")]
    pub debug: bool,
    /// Build the `release` profile instead of the default `debug`. Pass this
    /// only when the editor you cook from is a release build. Overrides an
    /// earlier `--debug` (last profile flag wins).
    #[arg(long, overrides_with = "debug")]
    pub release: bool,
    /// Resolve the toolchain + assemble the plan, print it, but do not shell out.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip fetching/staging `libopenxr_loader.so` (M1 does not link it).
    #[arg(long = "no-openxr-loader")]
    pub skip_openxr_loader: bool,
    /// Enable the `metrics` feature on the runtime cdylib so the performance
    /// measure/metric code paths (gated behind the feature per TMDD) compile in.
    #[arg(long = "with-metrics", alias = "with_metrics")]
    pub with_metrics: bool,
    /// Enable the `openexec` feature so the runtime carries the `OpenExec` dataflow
    /// subsystem + the coded `idtx-usd-exec` plugin. The cross-built plugin is then
    /// staged beside the runtime (`android-runtime/usd-exec/`) for the packager.
    /// Requires an Android `OpenUSD` install built with `PXR_BUILD_EXEC=ON`
    /// (`cargo xtask build-usd-android --with-openexec`).
    #[arg(long = "with-openexec", alias = "with_openexec")]
    pub with_openexec: bool,
    /// Enable the `remote` feature so the device player can open USD stages that
    /// reference remote (`http(s)://`) levels/payloads kept external by the cook.
    /// Compiles the C++ `idtx-usd-resolver-http` plugin against the Android USD
    /// prefix. `--with-remote`.
    #[arg(long = "with-remote", alias = "with_remote")]
    pub with_remote: bool,
    /// Explicit Android `OpenUSD` install prefix (overrides `$IDTX_USD_DIR_ANDROID`
    /// and the `thirdparty/openusd-android-arm64` default).
    #[arg(long = "usd-dir-android", value_name = "PATH")]
    pub usd_dir_android: Option<PathBuf>,
}

impl QuestRuntimeOptions {
    /// Whether the debug profile is selected. Debug is the default; `--release`
    /// switches to release (a later `--debug` switches back).
    #[must_use]
    pub fn is_debug(&self) -> bool {
        self.debug || !self.release
    }

    fn profile(&self) -> &'static str {
        if self.is_debug() { "debug" } else { "release" }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — process-free, unit-tested
// ---------------------------------------------------------------------------

/// `cargo build` arg vector for the runtime cdylib. `--lib` restricts the build
/// to the `cdylib` target (the desktop `[[bin]]` is irrelevant on Android), and
/// the `quest` feature enables the real `OpenXR` Vulkan path. When
/// `remote` is set, the HTTP USD resolver plugin (`idtx-usd-resolver-http`) is
/// compiled against the Android USD prefix and linked in. When `metrics` is set,
/// the runtime's `metrics` feature is appended so the performance measure/metric
/// code paths (feature-gated per TMDD) compile in.
#[must_use]
pub fn cargo_build_args(release: bool, metrics: bool, openexec: bool, remote: bool) -> Vec<String> {
    // The Quest cdylib links the real OpenXR Vulkan path. The
    // `quest` feature cascades `idtx-runtime-core` → `idtx-studio-session`
    // → `idtx-platform-xr`/`idtx-openxr-sys` and enables the Vulkan session,
    // Android loader init, and the `idtx_xr` graphics code (D3D12 stays
    // Windows-gated and compiles out here). `openxr` dlopens the bundled
    // `libopenxr_loader.so` at runtime, so this adds no link-time loader.
    let mut features = String::from("quest");
    // Remote (`http(s)://`) USD asset resolution: the device player opens a local
    // `stage.usdz` that may reference remote levels/payloads kept external by the
    // cook, so when enabled it ships the HTTP ArResolver + bearer-token arming.
    // Compiles the C++ resolver plugin against the Android USD prefix.
    // NOTE: validate the Android cross-build of `idtx-usd-resolver-http` in CI
    // (unverified on a macOS host).
    if remote {
        features.push_str(",remote");
    }
    if metrics {
        features.push_str(",metrics");
        features.push_str(",physics-metrics");
    }
    // `openexec` pulls the OpenExec subsystem + the coded `idtx-usd-exec` plugin
    // (compiled against the Android USD prefix, which must carry PXR_BUILD_EXEC).
    if openexec {
        features.push_str(",openexec");
    }
    let mut args = vec![
        "build".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "idtx-runtime".to_owned(),
        "--lib".to_owned(),
        "--target".to_owned(),
        RUNTIME_TARGET_TRIPLE.to_owned(),
        "--features".to_owned(),
        features,
    ];
    if release {
        args.push("--release".to_owned());
    }
    args
}

/// The Maven URL for the `OpenXR` loader AAR of `version`.
#[must_use]
pub fn openxr_loader_aar_url(base: &str, version: &str) -> String {
    format!("{base}/{version}/openxr_loader_for_android-{version}.aar")
}

/// Path (inside the AAR zip) of the arm64 loader shared object — the prefab
/// layout the Khronos `openxr_loader_for_android` package ships.
#[must_use]
pub fn openxr_loader_inner_so_path() -> &'static str {
    "prefab/modules/openxr_loader/libs/android.arm64-v8a/libopenxr_loader.so"
}

/// Whether `header` is the start of an ELF **aarch64** object (ELF magic +
/// `e_machine == EM_AARCH64`). Mirrors the packager's own guard so the staged
/// artifact can be rejected the moment it is produced if a toolchain misstep
/// yielded a host object or a PE.
#[must_use]
pub fn is_aarch64_elf(header: &[u8]) -> bool {
    header.len() >= 20
        && &header[0..4] == b"\x7fELF"
        && u16::from_le_bytes([header[18], header[19]]) == 0x00B7
}

/// The staging directory the runtime + its deps are assembled into — beside the
/// editor binaries so the cook's sibling lookup finds it:
/// `<workspace>/target/<profile>/android-runtime/arm64-v8a`.
#[must_use]
pub fn android_runtime_staging_dir(workspace_root: &Path, profile: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join(profile)
        .join("android-runtime")
        .join(ANDROID_ABI_DIR)
}

/// The root of the staged Android runtime tree for `profile`
/// (`<workspace>/target/<profile>/android-runtime`) — the parent of the
/// `arm64-v8a` slice dir, also holding the sibling `usd/`, `usd-exec/`, and
/// `idtx-http-resolver/` trees. The bundlers copy this whole directory beside the
/// packaged editor so the conductor's `quest_runtime_binary()` lookup resolves.
#[must_use]
pub fn android_runtime_tree_dir(profile: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join(profile)
        .join("android-runtime")
}

/// The resolved Android `OpenUSD` install prefix with no explicit override:
/// `$IDTX_USD_DIR_ANDROID` → the `build-usd-android` staging prefix. Lets the
/// bundlers decide whether a (re)build of the heavy C++ cross-build is needed.
#[must_use]
pub fn default_android_usd_dir() -> PathBuf {
    resolve_android_usd_dir(&workspace_root(), None)
}

/// Whether a complete Android `OpenUSD` install is present at `usd_dir` (the
/// monolithic `libusd_ms.so` + the `pxr.h` header). Mirrors the guard in
/// [`run`]; the bundlers use it to skip the ~hour-long cross-build when an install
/// is already staged.
#[must_use]
pub fn android_usd_install_present(usd_dir: &Path) -> bool {
    usd_dir.join("lib").join("libusd_ms.so").is_file()
        && usd_dir
            .join("include")
            .join("pxr")
            .join("pxr.h")
            .is_file()
}

/// Whether the Android `OpenUSD` install at `usd_dir` was built with the `OpenExec`
/// framework (`PXR_BUILD_EXEC=ON`). The reuse guard in the bundlers consults this
/// so an `--openexec` request does NOT silently reuse a monolith cross-built
/// WITHOUT exec: the coded `idtx-usd-exec` plugin links the monolith's Vdf/Exec/Ef
/// symbols on device, and a non-exec monolith yields an APK where `OpenExec` never
/// runs (no build error — the plugin binds its symbols lazily).
///
/// Detected by the presence of the installed exec header tree
/// (`include/pxr/exec/vdf/context.h` — the exact header the plugin compiles
/// against). `PXR_BUILD_EXEC=OFF` does not configure, build, or install the
/// `pxr/exec/` subtree, so its absence is a reliable "no exec" signal. Preferred
/// over a written marker: it reads the install's actual contents, so a pre-exec
/// install built before this guard existed is classified correctly without a
/// forced rebuild.
#[must_use]
pub fn android_usd_install_has_exec(usd_dir: &Path) -> bool {
    usd_dir
        .join("include")
        .join("pxr")
        .join("exec")
        .join("vdf")
        .join("context.h")
        .is_file()
}

/// The lib directories to search for the runtime's native deps, in order: the
/// `OpenUSD` install's `lib/` (the monolithic `libusd_ms.so`), then the oneTBB
/// and `MaterialX` sibling prefixes' `lib/` (the cross-build installs those into
/// separate prefixes **beside the `openusd-<version>/` dir**, i.e. directly under
/// `thirdparty/`, not beside the `arm64-v8a` install). The sibling leaves mirror
/// `idtx-usd-sys/build.rs`'s link-search resolution. Explicit env overrides
/// (`IDTX_TBB_DIR_ANDROID` / `IDTX_MATERIALX_DIR_ANDROID`) win over the sibling
/// convention, matching the bridge build.
#[must_use]
pub fn android_lib_search_dirs(usd_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![usd_dir.join("lib")];
    // `build-usd-android` installs oneTBB/MaterialX to `thirdparty/<leaf>`, which
    // is `usd_dir`'s grandparent (`thirdparty/openusd-<v>/android-arm64` → up two).
    let thirdparty = usd_dir.parent().and_then(Path::parent);
    let sibling = |leaf: &str| thirdparty.map(|p| p.join(leaf));
    for (env_key, leaf) in [
        ("IDTX_TBB_DIR_ANDROID", "onetbb-android"),
        ("IDTX_MATERIALX_DIR_ANDROID", "materialx-android"),
    ] {
        if let Some(prefix) = std::env::var_os(env_key)
            .map(PathBuf::from)
            .or_else(|| sibling(leaf))
        {
            dirs.push(prefix.join("lib"));
        }
    }
    dirs
}

/// Candidate file names for the per-API NDK clang (or clang++) wrapper, in
/// preference order for `windows`/non-windows hosts. The NDK ships a
/// `aarch64-linux-android<API>-clang[.cmd]` wrapper that bakes in the sysroot and
/// target; `cc`/the linker invoke it directly.
#[must_use]
pub fn ndk_clang_candidates(api: u32, cpp: bool, windows: bool) -> Vec<String> {
    let plus = if cpp { "++" } else { "" };
    let stem = format!("aarch64-linux-android{api}-clang{plus}");
    if windows {
        // NDK r23+ on Windows ships `.cmd` wrappers; some layouts also carry a
        // `.exe`. Try both, then the bare name.
        vec![format!("{stem}.cmd"), format!("{stem}.exe"), stem]
    } else {
        vec![stem]
    }
}

/// The NDK `llvm-ar` file name for the host.
#[must_use]
pub fn ndk_ar_name(windows: bool) -> String {
    if windows {
        "llvm-ar.exe".to_owned()
    } else {
        "llvm-ar".to_owned()
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// `cargo xtask build-quest-runtime [--release] [--dry-run] [--no-openxr-loader]
/// [--with_metrics] [--usd-dir-android <path>]`. Builds the `debug` profile by
/// default; pass `--release` for a release runtime. `--with_metrics` compiles in
/// the runtime's feature-gated performance measure/metric code.
pub fn build_quest_runtime_cmd(opts: &QuestRuntimeOptions) -> Result<()> {
    let plan = QuestRuntimePlan::resolve(opts)?;
    plan.describe();
    if opts.dry_run {
        println!("build-quest-runtime: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }
    run(&plan)
}

/// A fully resolved cross-build plan.
#[derive(Debug, Clone)]
struct QuestRuntimePlan {
    toolchain: AndroidToolchain,
    workspace_root: PathBuf,
    profile: &'static str,
    release: bool,
    usd_dir: PathBuf,
    staging_dir: PathBuf,
    skip_openxr_loader: bool,
    with_metrics: bool,
    with_openexec: bool,
    with_remote: bool,
}

impl QuestRuntimePlan {
    fn resolve(opts: &QuestRuntimeOptions) -> Result<Self> {
        let toolchain = AndroidToolchain::resolve()?;
        let root = workspace_root();
        let usd_dir = resolve_android_usd_dir(&root, opts.usd_dir_android.clone());
        let profile = opts.profile();
        Ok(Self {
            toolchain,
            staging_dir: android_runtime_staging_dir(&root, profile),
            workspace_root: root,
            profile,
            release: !opts.is_debug(),
            usd_dir,
            skip_openxr_loader: opts.skip_openxr_loader,
            with_metrics: opts.with_metrics,
            with_openexec: opts.with_openexec,
            with_remote: opts.with_remote,
        })
    }

    fn describe(&self) {
        println!("build-quest-runtime plan:");
        println!("  Target:       {RUNTIME_TARGET_TRIPLE} ({ANDROID_ABI_DIR})");
        println!("  Profile:      {}", self.profile);
        println!("  NDK:          {}", self.toolchain.ndk_root.display());
        println!(
            "  Toolchain:    {}",
            self.toolchain.toolchain_bin().display()
        );
        println!("  Android USD:  {}", self.usd_dir.display());
        println!(
            "  OpenXR loader: v{OPENXR_LOADER_AAR_VERSION}{}",
            if self.skip_openxr_loader {
                " (SKIPPED)"
            } else {
                ""
            }
        );
        println!("  Staging:      {}", self.staging_dir.display());
        println!(
            "  Features:     quest (real OpenXR Vulkan path — Milestone 2){}{}{}",
            if self.with_remote { " + remote" } else { "" },
            if self.with_metrics {
                " + metrics"
            } else {
                ""
            },
            if self.with_openexec {
                " + openexec"
            } else {
                ""
            }
        );
    }
}

// ---------------------------------------------------------------------------
// Orchestration (shells out — only when not --dry-run)
// ---------------------------------------------------------------------------

/// Resolve the Android `OpenUSD` install prefix: explicit override →
/// `$IDTX_USD_DIR_ANDROID` → the `xtask build-usd-android` staging prefix
/// (`thirdparty/openusd-android-arm64`). Mirrors `idtx-usd-sys/build.rs`.
fn resolve_android_usd_dir(workspace_root: &Path, override_dir: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir;
    }
    if let Some(v) = std::env::var_os("IDTX_USD_DIR_ANDROID") {
        return PathBuf::from(v);
    }
    staging_prefix(workspace_root, StandalonePlatform::Quest)
}

fn run(plan: &QuestRuntimePlan) -> Result<()> {
    // 1) The Android OpenUSD install must be present — the runtime's cxx bridge
    //    links `libusd_ms.so`. Do NOT silently kick off the (very long) C++
    //    cross-build; point the user at it.
    let usd_lib = plan.usd_dir.join("lib").join("libusd_ms.so");
    let usd_header = plan.usd_dir.join("include").join("pxr").join("pxr.h");
    if !usd_lib.is_file() || !usd_header.is_file() {
        bail!(
            "Android OpenUSD install not found at {}\n\
             (expected {} and {}).\n\
             Build it first:  cargo xtask build-usd-android\n\
             or point --usd-dir-android / $IDTX_USD_DIR_ANDROID at an existing install.",
            plan.usd_dir.display(),
            usd_lib.display(),
            usd_header.display(),
        );
    }

    // 2) Fetch the OpenXR loader (prep for XR; not linked in M1).
    let loader_so = if plan.skip_openxr_loader {
        None
    } else {
        Some(fetch_openxr_loader(&plan.workspace_root)?)
    };

    // 3) Cross-build the runtime cdylib with the NDK toolchain + Android USD env.
    cross_build_runtime(plan)?;

    // 4) Assemble the staging dir: the .so + 12 USD deps + the loader.
    let produced = plan
        .workspace_root
        .join("target")
        .join(RUNTIME_TARGET_TRIPLE)
        .join(plan.profile)
        .join("libidtx_runtime.so");
    assemble_staging(plan, &produced, loader_so.as_deref())?;

    println!(
        "\n\u{2705}  Quest runtime staged at {}",
        plan.staging_dir.display()
    );
    Ok(())
}

/// Build the environment the cross `cargo build` needs: the NDK clang (CC/CXX),
/// `llvm-ar` (AR), the linker, and the Android USD dir for the cxx bridge.
fn cross_build_env(plan: &QuestRuntimePlan) -> Result<Vec<(String, String)>> {
    let bin = plan.toolchain.toolchain_bin();
    let windows = cfg!(target_os = "windows");
    let api = DEFAULT_ANDROID_API_LEVEL;

    let cc =
        first_existing(&bin, &ndk_clang_candidates(api, false, windows)).with_context(|| {
            format!(
                "NDK clang not found in {} (looked for {:?})",
                bin.display(),
                ndk_clang_candidates(api, false, windows)
            )
        })?;
    let cxx = first_existing(&bin, &ndk_clang_candidates(api, true, windows))
        .with_context(|| format!("NDK clang++ not found in {}", bin.display()))?;
    let ar = first_existing(&bin, &[ndk_ar_name(windows)])
        .with_context(|| format!("NDK llvm-ar not found in {}", bin.display()))?;

    Ok(vec![
        (
            "CC_aarch64-linux-android".to_owned(),
            cc.display().to_string(),
        ),
        (
            "CXX_aarch64-linux-android".to_owned(),
            cxx.display().to_string(),
        ),
        (
            "AR_aarch64-linux-android".to_owned(),
            ar.display().to_string(),
        ),
        (
            "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER".to_owned(),
            cc.display().to_string(),
        ),
        (
            "IDTX_USD_DIR_ANDROID".to_owned(),
            plan.usd_dir.display().to_string(),
        ),
    ])
}

fn cross_build_runtime(plan: &QuestRuntimePlan) -> Result<()> {
    let env = cross_build_env(plan)?;
    let mut cmd = Command::new(cargo());
    cmd.current_dir(&plan.workspace_root)
        .args(cargo_build_args(plan.release, plan.with_metrics, plan.with_openexec, plan.with_remote));
    for (k, v) in &env {
        cmd.env(k, v);
    }
    println!("  $ {cmd:?}");
    let status = cmd
        .status()
        .context("failed to spawn `cargo build` for the Quest runtime")?;
    if !status.success() {
        bail!("cross-compiling idtx-runtime for {RUNTIME_TARGET_TRIPLE} failed with {status}");
    }
    Ok(())
}

/// Copy the produced `.so` + the 12 Android USD deps + the loader into the
/// staging dir, verifying the runtime is a real aarch64 ELF.
fn assemble_staging(
    plan: &QuestRuntimePlan,
    produced: &Path,
    loader_so: Option<&Path>,
) -> Result<()> {
    if !produced.is_file() {
        bail!(
            "cross-build reported success but {} is missing",
            produced.display()
        );
    }
    verify_aarch64_elf(produced)?;

    // Clear any prior staging tree before re-staging, so artifacts from an
    // earlier, differently-configured build cannot linger and ship. Without this,
    // a previous `--with-openexec` / `--with-remote` run leaves a `usd-exec/` or
    // `idtx-http-resolver/` sibling that a later run (built WITHOUT that feature,
    // so it neither refreshes nor removes the dir) would copy into the bundle
    // verbatim — a stale plugin paired with a runtime that no longer loads it.
    // The whole `android-runtime/` tree (the staging dir's parent, holding
    // `arm64-v8a/`, `usd/`, and the optional `usd-exec/` / `idtx-http-resolver/`
    // siblings) is rebuilt from scratch by this function, so remove it wholesale.
    let tree_root = plan.staging_dir.parent().unwrap_or(plan.staging_dir.as_path());
    if tree_root.exists() {
        std::fs::remove_dir_all(tree_root)
            .with_context(|| format!("clearing stale Android runtime tree {}", tree_root.display()))?;
    }

    std::fs::create_dir_all(&plan.staging_dir)
        .with_context(|| format!("creating staging dir {}", plan.staging_dir.display()))?;

    // 4a. the runtime cdylib.
    copy_into(produced, &plan.staging_dir)?;

    // 4b. the 12 native USD deps (tolerant: warn on an absent optional lib, like
    //     the USD cross-build's own verify step). The monolithic libusd_ms.so is
    //     in the USD prefix; oneTBB and the MaterialX generation libs are in
    //     their SEPARATE sibling prefixes (the cross-build does not consolidate
    //     them), so search all three lib dirs.
    let search_dirs = android_lib_search_dirs(&plan.usd_dir);
    let mut staged = 0usize;
    let mut missing: Vec<String> = Vec::new();
    for name in android_runtime_libs() {
        let src = search_dirs
            .iter()
            .map(|d| d.join(&name))
            .find(|p| p.is_file());
        if let Some(src) = src {
            copy_into(&src, &plan.staging_dir)?;
            staged += 1;
        } else if name == idtx_standalone::ANDROID_CXX_SHARED_LIB {
            // libc++_shared.so is an NDK sysroot lib, not a USD output — fall
            // back to the NDK if the USD staging did not already carry it.
            match ndk_libcxx_shared(&plan.toolchain) {
                Some(src) => {
                    copy_into(&src, &plan.staging_dir)?;
                    staged += 1;
                }
                None => missing.push(name),
            }
        } else {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "WARNING: {} expected native libraries missing from the Android runtime deps: {missing:?}",
            missing.len()
        );
    }

    // 4c. the OpenXR loader (if fetched).
    if let Some(loader) = loader_so {
        copy_into(loader, &plan.staging_dir)?;
    }

    // 4d. OpenUSD plugInfo discovery trees (`lib/usd` + `plugin/usd`) → the
    //     sibling `usd/` dir the cook copies into the APK `assets/usd/`. The
    //     monolithic libusd_ms.so carries the code; these plugInfo.json trees +
    //     schema resources are what the on-device runtime points
    //     PXR_PLUGINPATH_NAME at. Without them USD cannot find ArDefaultResolver
    //     (its build-host default plugin paths do not exist on device) and
    //     aborts at the first stage open.
    let usd_staging = plan
        .staging_dir
        .parent()
        .map_or_else(|| plan.staging_dir.join("usd"), |p| p.join("usd"));
    let mut usd_trees = 0usize;
    for rel in ["lib/usd", "plugin/usd"] {
        let src = plan.usd_dir.join(rel);
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

    println!("  staged libidtx_runtime.so + {staged} native deps");
    println!(
        "  staged {usd_trees} USD plugInfo tree(s) -> {}",
        usd_staging.display()
    );

    // 4e. the cross-built coded OpenExec plugin (feature `openexec`) → the sibling
    //     `usd-exec/` dir the packager injects into the APK `assets/usd-exec/`.
    //     Built into the runtime target's build-script OUT_DIR; we locate it and
    //     copy the lib + plugInfo.json + generatedSchema.usda. Absent is a loud
    //     warning (the runtime was likely built without `openexec`), not fatal.
    if plan.with_openexec {
        let exec_staging = plan.staging_dir.parent().map_or_else(
            || plan.staging_dir.join(idtx_standalone::EXEC_PLUGIN_DIR_NAME),
            |p| p.join(idtx_standalone::EXEC_PLUGIN_DIR_NAME),
        );
        match locate_cross_exec_plugin_dir(
            &plan.workspace_root,
            RUNTIME_TARGET_TRIPLE,
            plan.profile,
            "libidtx_usd_exec.so",
        ) {
            Some(src) => {
                copy_tree_recursive(&src, &exec_staging)
                    .with_context(|| format!("staging OpenExec plugin from {}", src.display()))?;
                println!("  staged OpenExec plugin -> {}", exec_staging.display());
            }
            None => bail!(
                "--with-openexec was set but no cross-built OpenExec plugin \
                 (libidtx_usd_exec.so) was found under \
                 target/{RUNTIME_TARGET_TRIPLE}/{}/build/ (checked both \
                 idtx-usd-exec-*/out/idtx-usd-exec/ and idtx-usd-exec/*/out/idtx-usd-exec/). \
                 Did the runtime build include the `openexec` feature? Staging aborts here on \
                 purpose: the runtime carries a forced `DT_NEEDED libidtx_usd_exec.so`, so an \
                 APK missing the plugin would fail to load at launch on device.",
                plan.profile
            ),
        }
    }

    // 4f. the cross-built HTTP USD resolver plugin (feature `remote`) → the sibling
    //     `idtx-http-resolver/` dir the packager injects into the APK (its `.so`
    //     into `jniLibs/arm64-v8a/`, its plugInfo into `assets/idtx-http-resolver/`).
    //     The resolver's artifacts sit directly in the build-script `out/` (no
    //     nested resource leaf, unlike OpenExec). Absent is FATAL here: the runtime
    //     built with `remote` carries a forced `DT_NEEDED libidtx_http_resolver.so`,
    //     so an APK missing it fails to load at launch on device (exactly the
    //     observed Quest `UnsatisfiedLinkError`).
    if plan.with_remote {
        let resolver_staging = plan.staging_dir.parent().map_or_else(
            || {
                plan.staging_dir
                    .join(idtx_standalone::HTTP_RESOLVER_PLUGIN_DIR_NAME)
            },
            |p| p.join(idtx_standalone::HTTP_RESOLVER_PLUGIN_DIR_NAME),
        );
        match locate_cross_out_artifact_dir(
            &plan.workspace_root,
            RUNTIME_TARGET_TRIPLE,
            plan.profile,
            "idtx-usd-resolver-http",
            None,
            "libidtx_http_resolver.so",
        ) {
            Some(src) => {
                copy_tree_recursive(&src, &resolver_staging).with_context(|| {
                    format!("staging HTTP resolver plugin from {}", src.display())
                })?;
                println!(
                    "  staged HTTP resolver plugin -> {}",
                    resolver_staging.display()
                );
            }
            None => bail!(
                "--with-remote was set but no cross-built HTTP resolver plugin \
                 (libidtx_http_resolver.so) was found under \
                 target/{RUNTIME_TARGET_TRIPLE}/{}/build/ (checked both \
                 idtx-usd-resolver-http-*/out/ and idtx-usd-resolver-http/*/out/). \
                 Did the runtime build include the `remote` feature? Staging aborts here on \
                 purpose: the runtime carries a forced `DT_NEEDED libidtx_http_resolver.so`, so \
                 an APK missing the resolver would fail to load at launch on device.",
                plan.profile
            ),
        }
    }
    Ok(())
}

/// Locate a cross-built artifact dir in a crate's per-build `OUT_DIR`, across the
/// two Cargo build-dir layouts. Returns the newest `out/[resource_subdir]` dir
/// that actually carries `lib_name`. `None` when the crate was not built for this
/// target or the artifact is missing.
///
/// `crate_stem` is the Cargo package name as it appears under `target/<triple>/
/// <profile>/build/`. `resource_subdir` is the leaf the build script writes its
/// artifacts into *inside* `out/`: `Some("idtx-usd-exec")` for the `OpenExec` plugin
/// (nested), `None` for the HTTP resolver (artifacts sit directly in `out/`).
///
/// Two layouts are handled because the `OUT_DIR` path differs by toolchain:
/// * **flat** — `build/<crate_stem>-<hash>/out/…` — the classic layout the stable
///   toolchain emits (Android/Quest build on stable).
/// * **nested** — `build/<crate_stem>/<hash>/out/…` — the newer hashed
///   build-directory layout the **nightly `-Z build-std`** toolchain emits (the
///   visionOS runtime cross-build uses nightly for the tier-3 target). Here the
///   `build/` entry is the bare crate name (no `-<hash>` suffix) and the metadata
///   hash is a sub-directory.
///
/// Missing the nested layout is exactly how a Vision Pro `.app` shipped without
/// `libidtx_usd_exec.dylib` and dyld-crashed at launch on the forced
/// `@rpath/libidtx_usd_exec.dylib` load.
pub fn locate_cross_out_artifact_dir(
    workspace_root: &Path,
    triple: &str,
    profile: &str,
    crate_stem: &str,
    resource_subdir: Option<&str>,
    lib_name: &str,
) -> Option<PathBuf> {
    let build = workspace_root
        .join("target")
        .join(triple)
        .join(profile)
        .join("build");

    // `out/` plus the optional resource leaf the build script writes into.
    let join_out = |base: PathBuf| {
        let out = base.join("out");
        match resource_subdir {
            Some(sub) => out.join(sub),
            None => out,
        }
    };
    let flat_prefix = format!("{crate_stem}-");

    // Collect every candidate artifact dir across both layouts.
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&build).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == crate_stem {
            // Nested (nightly build-std): one metadata-hash sub-dir per build.
            if let Ok(hashes) = std::fs::read_dir(entry.path()) {
                for hash in hashes.flatten() {
                    candidates.push(join_out(hash.path()));
                }
            }
        } else if name.starts_with(&flat_prefix) {
            // Flat (stable): the hash is baked into the `build/` entry name.
            candidates.push(join_out(entry.path()));
        }
    }

    // Keep the newest dir that actually carries the library.
    candidates
        .into_iter()
        .filter_map(|dir| {
            let meta = dir.join(lib_name).metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), dir))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, dir)| dir)
}

/// Locate the cross-built coded `OpenExec` plugin dir in the runtime build's
/// per-crate `OUT_DIR` (artifacts nested under `out/idtx-usd-exec/`). Thin wrapper
/// over [`locate_cross_out_artifact_dir`]. Shared with the visionOS runtime
/// cross-build.
pub fn locate_cross_exec_plugin_dir(
    workspace_root: &Path,
    triple: &str,
    profile: &str,
    lib_name: &str,
) -> Option<PathBuf> {
    locate_cross_out_artifact_dir(
        workspace_root,
        triple,
        profile,
        "idtx-usd-exec",
        Some("idtx-usd-exec"),
        lib_name,
    )
}

/// Recursively copy `src` into `dst`, creating `dst` and parents; files are
/// overwritten. Used to stage the `OpenUSD` plugInfo trees beside the runtime.
fn copy_tree_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
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

/// Locate `libc++_shared.so` in the NDK sysroot for arm64.
fn ndk_libcxx_shared(tc: &AndroidToolchain) -> Option<PathBuf> {
    let p = tc
        .toolchain_bin()
        .parent()? // .../bin -> toolchain root
        .join("sysroot")
        .join("usr")
        .join("lib")
        .join("aarch64-linux-android")
        .join(idtx_standalone::ANDROID_CXX_SHARED_LIB);
    p.is_file().then_some(p)
}

/// Fetch + unzip the `OpenXR` loader AAR, returning the extracted
/// `libopenxr_loader.so` under `thirdparty/`.
fn fetch_openxr_loader(workspace_root: &Path) -> Result<PathBuf> {
    let thirdparty = workspace_root.join("thirdparty");
    std::fs::create_dir_all(&thirdparty).ok();
    let dest = thirdparty.join("libopenxr_loader.so");
    if dest.is_file() {
        println!("  OpenXR loader already present at {}", dest.display());
        return Ok(dest);
    }
    let url = openxr_loader_aar_url(OPENXR_LOADER_MAVEN_BASE, OPENXR_LOADER_AAR_VERSION);
    let aar = thirdparty.join(format!(
        "openxr_loader_for_android-{OPENXR_LOADER_AAR_VERSION}.aar"
    ));
    println!("  fetching OpenXR loader: {url}");
    download(&url, &aar)?;
    extract_zip_member(&aar, openxr_loader_inner_so_path(), &dest)?;
    if !dest.is_file() {
        bail!(
            "OpenXR loader was fetched but {} was not extracted from the AAR",
            dest.display()
        );
    }
    Ok(dest)
}

// ---------------------------------------------------------------------------
// Shell-out helpers (download / unzip / copy)
// ---------------------------------------------------------------------------

/// Download `url` to `dest` via `curl` (ships with Windows 10+ and Unix).
fn download(url: &str, dest: &Path) -> Result<()> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fL", "--retry", "3", "-o"]).arg(dest).arg(url);
    run_cmd(&mut cmd, "curl download")
}

/// Extract the single zip member `inner` from `archive` to `dest` (an AAR is a
/// zip). Uses `.NET ZipFile` via PowerShell on Windows and `unzip` elsewhere;
/// extracts to a scratch dir and copies the one member out.
fn extract_zip_member(archive: &Path, inner: &str, dest: &Path) -> Result<()> {
    let scratch = archive.with_extension("unzip");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).ok();
    if cfg!(target_os = "windows") {
        let ps = format!(
            "Add-Type -AssemblyName System.IO.Compression.FileSystem; \
             [System.IO.Compression.ZipFile]::ExtractToDirectory('{}','{}')",
            archive.display(),
            scratch.display(),
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &ps]);
        run_cmd(&mut cmd, "unzip (PowerShell ZipFile)")?;
    } else {
        let mut cmd = Command::new("unzip");
        cmd.arg("-o").arg(archive).arg("-d").arg(&scratch);
        run_cmd(&mut cmd, "unzip")?;
    }
    let member = scratch.join(inner.replace('/', std::path::MAIN_SEPARATOR_STR));
    if !member.is_file() {
        bail!(
            "expected {} inside {} but it was not found after extraction",
            inner,
            archive.display()
        );
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::copy(&member, dest)
        .with_context(|| format!("copying {} -> {}", member.display(), dest.display()))?;
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

/// Copy `src` into directory `dir`, keeping the file name.
fn copy_into(src: &Path, dir: &Path) -> Result<()> {
    let name = src
        .file_name()
        .with_context(|| format!("source has no file name: {}", src.display()))?;
    let dst = dir.join(name);
    std::fs::copy(src, &dst)
        .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

/// Read a file header and assert it is an aarch64 ELF object.
fn verify_aarch64_elf(path: &Path) -> Result<()> {
    use std::io::Read as _;
    let mut header = [0u8; 20];
    let mut f = std::fs::File::open(path)
        .with_context(|| format!("opening {} to verify", path.display()))?;
    f.read_exact(&mut header)
        .with_context(|| format!("{} is too small to be an ELF object", path.display()))?;
    if !is_aarch64_elf(&header) {
        let kind = if &header[0..2] == b"MZ" {
            "a Windows PE binary".to_owned()
        } else if &header[0..4] == b"\x7fELF" {
            format!(
                "a non-aarch64 ELF (e_machine 0x{:04x})",
                u16::from_le_bytes([header[18], header[19]])
            )
        } else {
            format!("not an ELF object (magic {:02x?})", &header[0..4])
        };
        bail!(
            "{} is {kind}; expected an aarch64 ELF shared object",
            path.display()
        );
    }
    Ok(())
}

/// First of `names` that exists as a file inside `dir`.
fn first_existing(dir: &Path, names: &[String]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

fn run_cmd(cmd: &mut Command, label: &str) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("spawning `{label}`"))?;
    if !status.success() {
        bail!("`{label}` failed with {status}");
    }
    Ok(())
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// The workspace root (the directory holding the `[workspace]` `Cargo.toml` =
/// `idtx-studio`, where cargo's `target/` lives). Delegates to
/// [`crate::workspace_root`]; falls back to `xtask`'s manifest parent only if
/// that walk fails.
fn workspace_root() -> PathBuf {
    crate::workspace_root().unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    })
}

// ---------------------------------------------------------------------------
// Tests — pure logic only (no NDK/SDK, no process execution, no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parse `QuestRuntimeOptions` from a bare argument vector via a minimal clap
    /// wrapper (mirrors how `main.rs` reaches the flattened subcommand args).
    fn parse_opts(args: &[&str]) -> Result<QuestRuntimeOptions, clap::Error> {
        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: QuestRuntimeOptions,
        }
        TestCli::try_parse_from(std::iter::once("build-quest-runtime").chain(args.iter().copied()))
            .map(|c| c.opts)
    }

    #[test]
    fn cargo_build_args_are_lib_only_release_with_quest() {
        let args = cargo_build_args(true, false, false, false);
        assert_eq!(args[0], "build");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-p" && w[1] == "idtx-runtime")
        );
        assert!(
            args.contains(&"--lib".to_owned()),
            "must build only the cdylib"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--target" && w[1] == RUNTIME_TARGET_TRIPLE)
        );
        assert!(args.contains(&"--release".to_owned()));
        // The real Vulkan OpenXR path is behind `quest` (never
        // `pcvr`, which is the Windows/D3D12 path). Remote is opt-in.
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--features" && w[1] == "quest"),
            "must build the cdylib with --features quest (no remote by default)"
        );
        assert!(
            !args.iter().any(|a| a.contains("remote")),
            "remote must not be included without --with-remote"
        );
        assert!(
            !args.iter().any(|a| a.contains("pcvr")),
            "must not enable the Windows pcvr feature on the Quest build"
        );

        // --with-remote adds the remote feature.
        let args_remote = cargo_build_args(true, false, false, true);
        assert!(
            args_remote
                .windows(2)
                .any(|w| w[0] == "--features" && w[1] == "quest,remote"),
            "must build with --features quest,remote when --with-remote"
        );
    }

    #[test]
    fn cargo_build_args_debug_drops_release() {
        assert!(!cargo_build_args(false, false, false, false).contains(&"--release".to_owned()));
    }

    #[test]
    fn cargo_build_args_with_metrics_appends_metrics_feature() {
        // Without metrics the feature list is exactly `quest` (remote is opt-in).
        let plain = cargo_build_args(false, false, false, false);
        let plain_feat = plain
            .windows(2)
            .find(|w| w[0] == "--features")
            .map(|w| w[1].clone())
            .expect("--features present");
        assert_eq!(plain_feat, "quest");

        // With metrics the runtime's `metrics` (+ `physics-metrics`) features are
        // appended to the same single `--features` value, keeping `quest` enabled.
        let with = cargo_build_args(false, true, false, false);
        let feat = with
            .windows(2)
            .find(|w| w[0] == "--features")
            .map(|w| w[1].clone())
            .expect("--features present");
        assert_eq!(feat, "quest,metrics,physics-metrics");
        // Exactly one `--features` flag — not a second one that would shadow it.
        assert_eq!(with.iter().filter(|a| *a == "--features").count(), 1);

        // With both remote and metrics.
        let with_remote_metrics = cargo_build_args(false, true, false, true);
        let feat_rm = with_remote_metrics
            .windows(2)
            .find(|w| w[0] == "--features")
            .map(|w| w[1].clone())
            .expect("--features present");
        assert_eq!(feat_rm, "quest,remote,metrics,physics-metrics");
    }

    #[test]
    fn cargo_build_args_with_openexec_appends_openexec_feature() {
        let with = cargo_build_args(false, false, true, false);
        let feat = with
            .windows(2)
            .find(|w| w[0] == "--features")
            .map(|w| w[1].clone())
            .expect("--features present");
        assert_eq!(feat, "quest,openexec");
        // Off by default — a plain build must not carry it.
        assert!(!cargo_build_args(false, false, false, false).iter().any(|a| a.contains("openexec")));

        // With remote + openexec.
        let with_remote = cargo_build_args(false, false, true, true);
        let feat_r = with_remote
            .windows(2)
            .find(|w| w[0] == "--features")
            .map(|w| w[1].clone())
            .expect("--features present");
        assert_eq!(feat_r, "quest,remote,openexec");
    }

    #[test]
    fn openxr_loader_url_shape() {
        let url = openxr_loader_aar_url(OPENXR_LOADER_MAVEN_BASE, "1.1.43");
        assert!(url.ends_with("/1.1.43/openxr_loader_for_android-1.1.43.aar"));
        assert!(url.starts_with("https://"));
    }

    #[test]
    fn openxr_inner_path_is_arm64_prefab() {
        let p = openxr_loader_inner_so_path();
        assert!(p.ends_with("android.arm64-v8a/libopenxr_loader.so"));
        assert!(p.starts_with("prefab/"));
    }

    #[test]
    fn aarch64_elf_magic_accepts_and_rejects() {
        let mut good = [0u8; 20];
        good[0..4].copy_from_slice(b"\x7fELF");
        good[18] = 0xB7;
        good[19] = 0x00;
        assert!(is_aarch64_elf(&good));

        // x86-64 ELF (e_machine 0x3E) — rejected.
        let mut x64 = good;
        x64[18] = 0x3e;
        x64[19] = 0x00;
        assert!(!is_aarch64_elf(&x64));

        // Windows PE — rejected.
        assert!(!is_aarch64_elf(b"MZ\x90\x00 and more padding here"));
        // Too short — rejected.
        assert!(!is_aarch64_elf(b"\x7fELF"));
    }

    #[test]
    fn staging_dir_is_beside_editor_binaries() {
        let dir = android_runtime_staging_dir(Path::new("/ws"), "release");
        assert!(dir.ends_with("target/release/android-runtime/arm64-v8a"));
    }

    #[test]
    fn android_usd_install_has_exec_tracks_exec_header_tree() {
        // Unique scratch USD prefix under the system temp dir (no tempfile dep).
        let usd = std::env::temp_dir().join(format!(
            "idtx-usd-exec-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        // A complete-but-non-exec install (libusd_ms.so + pxr.h, no exec headers):
        // `present` is true, but `has_exec` is false — exactly the stale-reuse case
        // the bundler must NOT reuse for an `--openexec` request.
        std::fs::create_dir_all(usd.join("lib")).unwrap();
        std::fs::create_dir_all(usd.join("include").join("pxr")).unwrap();
        std::fs::write(usd.join("lib").join("libusd_ms.so"), b"elf").unwrap();
        std::fs::write(usd.join("include").join("pxr").join("pxr.h"), b"// hdr").unwrap();
        assert!(android_usd_install_present(&usd));
        assert!(!android_usd_install_has_exec(&usd));

        // Add the exec header the coded plugin compiles against → now exec-capable.
        let vdf = usd.join("include").join("pxr").join("exec").join("vdf");
        std::fs::create_dir_all(&vdf).unwrap();
        std::fs::write(vdf.join("context.h"), b"// exec").unwrap();
        assert!(android_usd_install_has_exec(&usd));

        std::fs::remove_dir_all(&usd).ok();
    }

    #[test]
    fn android_lib_search_dirs_cover_usd_and_sibling_prefixes() {
        // Assumes the IDTX_*_DIR_ANDROID overrides are unset (true in CI/test),
        // so TBB and MaterialX resolve under `thirdparty/` — the grandparent of the
        // `arm64-v8a` USD install (`thirdparty/openusd-<v>/android-arm64`), which is
        // exactly where `build-usd-android` installs them.
        let usd = Path::new("/tp/thirdparty/openusd-26.11.1/android-arm64");
        let dirs = android_lib_search_dirs(usd);
        assert_eq!(dirs[0], Path::new("/tp/thirdparty/openusd-26.11.1/android-arm64/lib"));
        assert!(dirs.contains(&PathBuf::from("/tp/thirdparty/onetbb-android/lib")));
        assert!(dirs.contains(&PathBuf::from("/tp/thirdparty/materialx-android/lib")));
    }

    #[test]
    fn ndk_clang_candidates_per_host() {
        let win = ndk_clang_candidates(30, false, true);
        assert_eq!(win[0], "aarch64-linux-android30-clang.cmd");
        assert!(win.iter().any(|c| c == "aarch64-linux-android30-clang.exe"));
        let cpp = ndk_clang_candidates(30, true, true);
        assert!(cpp[0].ends_with("clang++.cmd"));
        let unix = ndk_clang_candidates(30, false, false);
        assert_eq!(unix, vec!["aarch64-linux-android30-clang".to_owned()]);
    }

    #[test]
    fn ndk_ar_name_per_host() {
        assert_eq!(ndk_ar_name(true), "llvm-ar.exe");
        assert_eq!(ndk_ar_name(false), "llvm-ar");
    }

    #[test]
    fn options_default_profile_is_debug() {
        // Default (no flags) must build the DEBUG profile so the runtime stages
        // under target/debug/android-runtime/, matching where a dev `cargo run`
        // editor (itself debug) looks for the sibling runtime. Regression guard
        // for the "Quest runtime not found at …/target/debug/…" cook failure
        // that a release-defaulting build-quest-runtime produced.
        let o = QuestRuntimeOptions::default();
        assert!(o.is_debug(), "default must be the debug profile");
        assert_eq!(o.profile(), "debug");
        // Parsing no args is equivalent to the default.
        assert_eq!(parse_opts(&[]).unwrap().profile(), "debug");
    }

    #[test]
    fn options_release_flag_selects_release() {
        // --release opts into the release profile (and overrides a prior --debug).
        let o = parse_opts(&["--release"]).unwrap();
        assert!(!o.is_debug());
        assert_eq!(o.profile(), "release");
        let o = parse_opts(&["--debug", "--release"]).unwrap();
        assert_eq!(o.profile(), "release", "last profile flag wins");
    }

    #[test]
    fn options_parse_flags() {
        assert_eq!(parse_opts(&[]).unwrap(), QuestRuntimeOptions::default());
        let o = parse_opts(&["--debug", "--no-openxr-loader"]).unwrap();
        assert!(o.is_debug() && o.skip_openxr_loader);
        assert_eq!(o.profile(), "debug");
        let o = parse_opts(&["--usd-dir-android", "/opt/usd"]).unwrap();
        assert_eq!(o.usd_dir_android, Some(PathBuf::from("/opt/usd")));
        assert!(parse_opts(&["--bogus"]).is_err());
    }

    #[test]
    fn options_with_metrics_flag() {
        // Off by default.
        assert!(!QuestRuntimeOptions::default().with_metrics);
        // Both the snake `--with_metrics` and the dashed `--with-metrics`
        // spelling opt in.
        for flag in ["--with_metrics", "--with-metrics"] {
            let o = parse_opts(&[flag]).unwrap();
            assert!(o.with_metrics, "{flag} must enable metrics");
        }
    }

    #[test]
    fn options_with_remote_flag() {
        // Off by default.
        assert!(!QuestRuntimeOptions::default().with_remote);
        // Both spellings (snake and kebab) opt in.
        for flag in ["--with_remote", "--with-remote"] {
            let o = parse_opts(&[flag]).unwrap();
            assert!(o.with_remote, "{flag} must enable remote");
        }
        // A build without the flag must not carry the remote feature.
        let args = cargo_build_args(false, false, false, false);
        assert!(
            !args.iter().any(|a| a.contains("remote")),
            "remote must be absent when with_remote is false"
        );
        // A build with the flag must carry it.
        let args_remote = cargo_build_args(false, false, false, true);
        assert!(
            args_remote.iter().any(|a| a.contains("remote")),
            "remote must be present when with_remote is true"
        );
    }

    #[test]
    fn locate_cross_exec_plugin_dir_handles_flat_and_nested_layouts() {
        // Unique scratch workspace under the system temp dir (no tempfile dep).
        let ws = std::env::temp_dir().join(format!(
            "idtx-locate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let build = ws.join("target").join("tri").join("debug").join("build");

        // Flat (stable toolchain): build/idtx-usd-exec-<hash>/out/idtx-usd-exec/.
        let flat = build.join("idtx-usd-exec-aaaa").join("out").join("idtx-usd-exec");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("libidtx_usd_exec.dylib"), b"old").unwrap();

        // Flat layout alone is found.
        assert_eq!(
            locate_cross_exec_plugin_dir(&ws, "tri", "debug", "libidtx_usd_exec.dylib").as_deref(),
            Some(flat.as_path())
        );

        // Nested (nightly -Z build-std): build/idtx-usd-exec/<hash>/out/idtx-usd-exec/.
        let nested = build
            .join("idtx-usd-exec")
            .join("df8a6281f825cb6f")
            .join("out")
            .join("idtx-usd-exec");
        std::fs::create_dir_all(&nested).unwrap();
        // Make the nested artifact newer so it wins the mtime tie-break.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(nested.join("libidtx_usd_exec.dylib"), b"new").unwrap();

        // The newer nested artifact is now located (the bug: it was invisible before).
        assert_eq!(
            locate_cross_exec_plugin_dir(&ws, "tri", "debug", "libidtx_usd_exec.dylib").as_deref(),
            Some(nested.as_path()),
            "nested nightly build-std layout must be discoverable"
        );

        // A directory present but missing the library is not a match.
        assert!(
            locate_cross_exec_plugin_dir(&ws, "tri", "debug", "libidtx_usd_exec.so").is_none(),
            "no .so staged — must not match the .dylib dirs"
        );

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn locate_cross_out_artifact_dir_handles_resolver_out_root_layout() {
        // The HTTP resolver's artifacts sit DIRECTLY in `out/` (resource_subdir
        // None), unlike OpenExec's nested `out/idtx-usd-exec/`. Exercise both the
        // flat (stable) and nested (nightly build-std) build-dir layouts.
        let ws = std::env::temp_dir().join(format!(
            "idtx-locate-resolver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let build = ws.join("target").join("tri").join("debug").join("build");
        let find = |ws: &std::path::Path| {
            locate_cross_out_artifact_dir(
                ws,
                "tri",
                "debug",
                "idtx-usd-resolver-http",
                None,
                "libidtx_http_resolver.so",
            )
        };

        // Flat: build/idtx-usd-resolver-http-<hash>/out/libidtx_http_resolver.so.
        let flat = build.join("idtx-usd-resolver-http-aaaa").join("out");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("libidtx_http_resolver.so"), b"old").unwrap();
        assert_eq!(find(&ws).as_deref(), Some(flat.as_path()));

        // Nested: build/idtx-usd-resolver-http/<hash>/out/libidtx_http_resolver.so.
        let nested = build
            .join("idtx-usd-resolver-http")
            .join("df8a6281f825cb6f")
            .join("out");
        std::fs::create_dir_all(&nested).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(nested.join("libidtx_http_resolver.so"), b"new").unwrap();
        assert_eq!(
            find(&ws).as_deref(),
            Some(nested.as_path()),
            "nested nightly build-std layout must be discoverable"
        );

        std::fs::remove_dir_all(&ws).ok();
    }
}
