//! `cargo xtask build-usd-android` / `build-usd-visionos` — cross-build the
//! `OpenUSD` monolithic runtime (plus oneTBB and `MaterialX`) for the two
//! standalone targets so the native libraries ship in-repo under
//! `thirdparty/openusd-<target>/`.
//!
//! # Why this lives in `xtask`
//!
//! The desktop/PCVR runtime links against an `OpenUSD` install built by
//! upstream `build_usd.py`. The standalone targets have no such prebuilt
//! install, so we cross-compile here. The recipe is ported from the sibling
//! `idtx-flow` project (`scons/tools/openusd.py` + `scons/platforms/android.py`)
//! which proved the Android NDK path; this port stays faithful to its CMake
//! flags, its one-shot oneTBB cross-build, and its five Android source patches.
//!
//! # Deliberate divergence from `idtx-flow`: `MaterialX` is **enabled**
//!
//! `idtx-flow` builds `OpenUSD` with `-DPXR_ENABLE_MATERIALX_SUPPORT=OFF`. We
//! turn it **on** so standalone material fidelity matches desktop/PCVR. That
//! requires first cross-building `MaterialX` itself (which `idtx-flow` never
//! does) and feeding it to the `OpenUSD` configure via `-DMaterialX_DIR`. If a
//! clean `MaterialX` cross-build proves infeasible on a given toolchain, the
//! `--no-materialx` flag falls back to the `idtx-flow` behaviour (built-in PBR,
//! no `MaterialX`) and the fallback is logged loudly — it is never silent.
//!
//! # Testability (TMDD)
//!
//! The actual cross-build needs an Android NDK (Quest) or the visionOS SDK
//! (Vision Pro), neither of which is present on an ordinary CI/dev host. So the
//! *pure* logic — the CMake argument vectors, the patch descriptor set, and the
//! NDK/SDK path resolution — is factored into free functions that invoke **no**
//! external process and are unit-tested here. Only the thin orchestration
//! layer ([`run_android_build`] / `run_visionos_build`) shells out, and it
//! fails fast with a clear message when the toolchain is absent.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;

use idtx_standalone::{StandalonePlatform, android_runtime_libs, visionos_runtime_libs};

// ---------------------------------------------------------------------------
// Pinned third-party versions
// ---------------------------------------------------------------------------

/// `OpenUSD` tag cross-built for the standalone targets. Matches the desktop
/// pin documented in `CLAUDE.md` (`openusd-26.11.1`).
pub const OPENUSD_VERSION: &str = "26.11.1";

/// The `thirdparty/` staging directory, relative to the workspace root
/// (`idtx-studio`). Cross-built `OpenUSD`/oneTBB/`MaterialX` installs land here; the
/// runtime cross-builds (`quest_runtime`, `visionos_runtime`) resolve the same
/// location, so the two must agree on this base.
pub const THIRDPARTY_DIR: &str = "thirdparty";

/// oneTBB tag. `OpenUSD` on Android needs an external TBB (it does not bundle
/// one for the NDK). Ported verbatim from `idtx-flow`.
pub const ONETBB_VERSION: &str = "2021.12.0";

/// `MaterialX` tag cross-built for the standalone targets. Pinned to **1.39.5**
/// to match the desktop `openusd-26.11.1` install exactly (its
/// `MaterialXCore/Generated.h` reports 1.39.5): that version splits
/// `MaterialXGenHw`/`MaterialXGenSlang` into their own shared libraries, which
/// the `idtx-usd-sys` shim link line (`-lMaterialXGenHw`) and the standalone
/// staging set (`idtx-standalone::MATERIALX_COMPONENTS`) both expect — 1.39.3
/// (the earlier pin) folded those into `MaterialXGenShader` and produced only 7
/// generation libs, so the Quest runtime link would fail on `-lMaterialXGenHw`.
/// Validated cross-compiling for Android (NDK 29) with `MATERIALX_BUILD_RENDER=OFF`.
pub const MATERIALX_VERSION: &str = "1.39.5";

/// Default Android API level for Quest cross-builds (`android-30`). The plan
/// (A4) and `idtx-standalone::DEFAULT_ANDROID_API_LEVEL` agree on 30; this is
/// higher than `idtx-flow`'s historical 24 because Quest requires it.
pub const DEFAULT_ANDROID_API_LEVEL: u32 = 30;

/// Android ABI. Quest 2/3/Pro are all `arm64-v8a`.
pub const ANDROID_ABI: &str = "arm64-v8a";

// ---------------------------------------------------------------------------
// Subcommand options
// ---------------------------------------------------------------------------

/// Parsed options shared by both standalone cross-build subcommands.
///
/// `MaterialX` is enabled by default; `--no-materialx` turns it off and
/// `--materialx` turns it back on (last flag wins). Query the resolved state
/// via [`CrossOptions::materialx`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Args)]
#[allow(clippy::struct_excessive_bools)] // CLI flag bag; each flag is independent.
pub struct CrossOptions {
    /// Disable the `MaterialX` cross-build (`PXR_ENABLE_MATERIALX_SUPPORT=OFF`).
    /// `MaterialX` is enabled by default.
    #[arg(long = "no-materialx", overrides_with = "materialx_flag")]
    pub no_materialx: bool,
    /// (Re-)enable the `MaterialX` cross-build; overrides an earlier
    /// `--no-materialx`. Enabled by default, so this is only needed to undo a
    /// preceding `--no-materialx` on the same command line.
    #[arg(long = "materialx", overrides_with = "no_materialx")]
    pub materialx_flag: bool,
    /// Resolve the toolchain and assemble every CMake argument vector, print
    /// the planned invocations, but do **not** shell out. `--dry-run`.
    #[arg(long)]
    pub dry_run: bool,
    /// Build the `OpenUSD` monolith with `PXR_BUILD_EXEC=ON` so it carries the
    /// `OpenExec` (Vdf/Exec/Ef) symbols the coded `idtx-usd-exec` plugin links
    /// against on device. Default `false`; `--with-openexec` sets it `true`.
    /// HAZARD: needs a toolchain that compiles 26.08 `pxr/exec` (NDK r26+/Clang
    /// 16+, or a `cacheView.h` workaround) — see `usd_android_cmake_args`.
    #[arg(long = "with-openexec")]
    pub with_openexec: bool,
}

impl CrossOptions {
    /// Whether the `MaterialX` cross-build is enabled. Enabled by default;
    /// `--no-materialx` disables it (a later `--materialx` re-enables it).
    #[must_use]
    pub fn materialx(&self) -> bool {
        self.materialx_flag || !self.no_materialx
    }
}

// ---------------------------------------------------------------------------
// Subcommand entry points (called from main.rs dispatch)
// ---------------------------------------------------------------------------

/// `cargo xtask build-usd-android [--no-materialx] [--dry-run]`.
pub fn build_usd_android_cmd(opts: &CrossOptions) -> Result<()> {
    let plan = AndroidPlan::resolve(opts)?;
    plan.describe();
    if opts.dry_run {
        println!("build-usd-android: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }
    run_android_build(&plan)
}

/// `cargo xtask build-usd-visionos [--no-materialx] [--dry-run]` (macOS only).
pub fn build_usd_visionos_cmd(opts: &CrossOptions) -> Result<()> {
    if !cfg!(target_os = "macos") && !opts.dry_run {
        bail!(
            "build-usd-visionos must run on macOS (needs xcrun + the visionOS SDK). \
             Use --dry-run to inspect the planned invocation on another host."
        );
    }
    let plan = VisionOsPlan::resolve(opts)?;
    plan.describe();
    if opts.dry_run {
        println!("build-usd-visionos: --dry-run, stopping before any process is spawned.");
        return Ok(());
    }
    run_visionos_build(&plan)
}

// ---------------------------------------------------------------------------
// Android toolchain resolution (ports idtx-flow/scons/platforms/android.py)
// ---------------------------------------------------------------------------

/// A resolved Android NDK toolchain: the NDK root, the API level, and the ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidToolchain {
    pub ndk_root: PathBuf,
    pub api_level: u32,
    pub abi: String,
}

impl AndroidToolchain {
    /// Resolve from `ANDROID_NDK_ROOT`, defaulting the API level/ABI. Returns
    /// an error when the env var is unset or does not point at a directory —
    /// the honest failure on a host without the NDK.
    pub fn resolve() -> Result<Self> {
        let ndk = std::env::var_os("ANDROID_NDK_ROOT")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .context(
                "ANDROID_NDK_ROOT must point to a valid Android NDK directory \
                 (e.g. ANDROID_NDK_ROOT=/path/to/ndk cargo xtask build-usd-android)",
            )?;
        Ok(Self {
            ndk_root: ndk,
            api_level: DEFAULT_ANDROID_API_LEVEL,
            abi: ANDROID_ABI.to_owned(),
        })
    }

    /// The NDK host tag directory component (`windows-x86_64`, `darwin-x86_64`,
    /// `linux-x86_64`).
    #[must_use]
    pub fn host_tag() -> &'static str {
        if cfg!(target_os = "windows") {
            "windows-x86_64"
        } else if cfg!(target_os = "macos") {
            "darwin-x86_64"
        } else {
            "linux-x86_64"
        }
    }

    /// `<ndk>/toolchains/llvm/prebuilt/<host-tag>/bin` — where the NDK's clang
    /// toolchain lives for the current host. Surfaced in the build plan so a
    /// misconfigured NDK is obvious before CMake runs.
    #[must_use]
    pub fn toolchain_bin(&self) -> PathBuf {
        self.ndk_root
            .join("toolchains")
            .join("llvm")
            .join("prebuilt")
            .join(Self::host_tag())
            .join("bin")
    }

    /// `<ndk>/build/cmake/android.toolchain.cmake`.
    #[must_use]
    pub fn toolchain_file(&self) -> PathBuf {
        self.ndk_root
            .join("build")
            .join("cmake")
            .join("android.toolchain.cmake")
    }
}

/// The CMake toolchain args shared by every Android cross-build (oneTBB,
/// `MaterialX`, `OpenUSD`). Mirrors `idtx-flow`'s `cmake_toolchain_args`.
///
/// `ninja` is an optional explicit `CMAKE_MAKE_PROGRAM` — on Windows CMake
/// otherwise defaults to MSBuild, which cannot cross-compile. Pass the path
/// from [`locate_sdk_ninja`] (or `None` when Ninja is on `PATH`).
#[must_use]
pub fn android_toolchain_args(tc: &AndroidToolchain, ninja: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "-G".to_owned(),
        "Ninja".to_owned(),
        format!("-DCMAKE_TOOLCHAIN_FILE={}", tc.toolchain_file().display()),
        format!("-DANDROID_ABI={}", tc.abi),
        format!("-DANDROID_PLATFORM=android-{}", tc.api_level),
        "-DANDROID_STL=c++_shared".to_owned(),
    ];
    if let Some(ninja) = ninja {
        args.push(format!("-DCMAKE_MAKE_PROGRAM={}", ninja.display()));
    }
    args
}

/// Locate the Ninja bundled with the Android SDK (`$ANDROID_HOME/cmake/*/bin`).
/// Returns `None` when the SDK is absent or carries no bundled Ninja; CMake
/// then relies on a `PATH` Ninja. Mirrors `idtx-flow`'s `locate_sdk_ninja`.
#[must_use]
pub fn locate_sdk_ninja() -> Option<PathBuf> {
    let sdk = std::env::var_os("ANDROID_HOME")
        .or_else(|| std::env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)?;
    let cmake_dir = sdk.join("cmake");
    let exe = if cfg!(target_os = "windows") {
        "ninja.exe"
    } else {
        "ninja"
    };
    let mut entries: Vec<_> = std::fs::read_dir(&cmake_dir).ok()?.flatten().collect();
    // Highest CMake version directory first.
    entries.sort_by_key(std::fs::DirEntry::file_name);
    entries.reverse();
    for entry in entries {
        let candidate = entry.path().join("bin").join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// CMake argument builders — pure, process-free, unit-tested
// ---------------------------------------------------------------------------

/// CMake args for the oneTBB Android cross-build. Ported verbatim from
/// `idtx-flow`'s `_build_tbb_android`.
#[must_use]
pub fn tbb_android_cmake_args(
    tc: &AndroidToolchain,
    ninja: Option<&Path>,
    install_prefix: &Path,
) -> Vec<String> {
    let mut args = android_toolchain_args(tc, ninja);
    args.extend([
        format!("-DCMAKE_INSTALL_PREFIX={}", install_prefix.display()),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
        "-DTBB_TEST=OFF".to_owned(),
        "-DTBB_STRICT=OFF".to_owned(),
        "-DBUILD_SHARED_LIBS=ON".to_owned(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_owned(),
        // NDK 29+ ld.lld errors on missing version-script symbols; restore the
        // old permissive behaviour (idtx-flow).
        "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,--undefined-version".to_owned(),
    ]);
    args
}

/// CMake args for the `MaterialX` Android cross-build (our divergence). No
/// Python/tests/examples/viewer; shared libs so `OpenUSD` can link them and we
/// can stage them beside the runtime.
#[must_use]
pub fn materialx_android_cmake_args(
    tc: &AndroidToolchain,
    ninja: Option<&Path>,
    install_prefix: &Path,
) -> Vec<String> {
    let mut args = android_toolchain_args(tc, ninja);
    args.extend([
        format!("-DCMAKE_INSTALL_PREFIX={}", install_prefix.display()),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
        "-DCMAKE_CXX_STANDARD=17".to_owned(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_owned(),
        "-DBUILD_SHARED_LIBS=ON".to_owned(),
        "-DMATERIALX_BUILD_SHARED_LIBS=ON".to_owned(),
        "-DMATERIALX_BUILD_PYTHON=OFF".to_owned(),
        "-DMATERIALX_BUILD_VIEWER=OFF".to_owned(),
        "-DMATERIALX_BUILD_GRAPH_EDITOR=OFF".to_owned(),
        "-DMATERIALX_BUILD_TESTS=OFF".to_owned(),
        "-DMATERIALX_TEST_RENDER=OFF".to_owned(),
        // The MaterialXRender* libraries (the sample GPU viewer/render helpers)
        // call `find_package(X11)` — there is no X11 on Android, so configure
        // fails there. OpenUSD here builds with PXR_BUILD_IMAGING=OFF and our
        // material fidelity comes from the shader-GENERATION libs
        // (MaterialXGen*/Core/Format) that the WgslShaderGenerator and usdMtlx
        // link; nothing links MaterialXRender*. Turn the render module off so the
        // cross-build configures headlessly. Keep in sync with the standalone
        // staging set (idtx-standalone `MATERIALX_COMPONENTS`, generation-only).
        "-DMATERIALX_BUILD_RENDER=OFF".to_owned(),
        "-DMATERIALX_BUILD_GEN_GLSL=ON".to_owned(),
        "-DMATERIALX_BUILD_GEN_MSL=ON".to_owned(),
    ]);
    args
}

/// CMake args for the `OpenUSD` Android cross-build. Ports `idtx-flow`'s
/// `_do_build_android`; the **only** functional change is `MaterialX`: when
/// `materialx_dir` is `Some`, support is turned ON and pointed at the staged
/// `MaterialX` install; when `None`, it is OFF (the `idtx-flow` behaviour).
#[must_use]
pub fn usd_android_cmake_args(
    tc: &AndroidToolchain,
    ninja: Option<&Path>,
    install_path: &Path,
    tbb_root: &Path,
    materialx_dir: Option<&Path>,
    with_openexec: bool,
) -> Vec<String> {
    let mut args = android_toolchain_args(tc, ninja);
    args.extend([
        format!("-DCMAKE_INSTALL_PREFIX={}", install_path.display()),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
        "-DCMAKE_CXX_STANDARD=17".to_owned(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_owned(),
        "-DBUILD_SHARED_LIBS=ON".to_owned(),
        "-DPXR_BUILD_MONOLITHIC=ON".to_owned(),
        "-DPXR_BUILD_IMAGING=OFF".to_owned(),
        "-DPXR_BUILD_USD_IMAGING=OFF".to_owned(),
        "-DPXR_BUILD_USDVIEW=OFF".to_owned(),
        "-DPXR_BUILD_TESTS=OFF".to_owned(),
        "-DPXR_BUILD_EXAMPLES=OFF".to_owned(),
        "-DPXR_BUILD_TUTORIALS=OFF".to_owned(),
        "-DPXR_BUILD_DOCUMENTATION=OFF".to_owned(),
        // The 26.08 Exec framework (`pxr/exec`) is OFF by default: it is unused by
        // our bridge in a plain standalone build and does not compile under the NDK
        // r25 Clang 14. `Exec_CacheView` (cacheView.h) has a defaulted default ctor
        // plus `const TfSpan<...>` members; `TfSpan`'s own default ctor is
        // defaulted-not-user-provided but its fields carry default member
        // initializers. Pre-CWG-253 Clang 14 deletes the enclosing ctor anyway;
        // desktop MSVC/AppleClang apply the relaxed C++17 rule and accept it.
        //
        // `--with-openexec` flips it ON so the standalone monolith carries the
        // Vdf/Exec/Ef symbols the coded `idtx-usd-exec` plugin links against on
        // device. HAZARD: this requires a toolchain that applies the relaxed rule
        // (NDK r26+/Clang 16+) or a `cacheView.h` workaround — a stock NDK r25 will
        // fail to compile `pxr/exec`.
        format!("-DPXR_BUILD_EXEC={}", if with_openexec { "ON" } else { "OFF" }),
        "-DPXR_ENABLE_PYTHON_SUPPORT=OFF".to_owned(),
        "-DPXR_ENABLE_OPENVDB_SUPPORT=OFF".to_owned(),
        "-DPXR_ENABLE_VULKAN_SUPPORT=OFF".to_owned(),
        // Size reduction for the Android monolith (.so). Unstripped, the ELF
        // keeps its full static symbol table (.symtab/.strtab) and does no
        // dead-code elimination -> ~500MB vs the ~41MB Windows DLL (which puts
        // symbols in a separate .pdb).
        //   - *_RELEASE variants so the NDK toolchain's base flags are kept;
        //     the NDK adds -g to the base flags, so -g0 here cancels it.
        //   - -ffunction/-fdata-sections + --gc-sections: drop unreferenced
        //     code/data.
        //   - --strip-all: drop .symtab/.strtab at link time (biggest win,
        //     single file, no separate debug artifact).
        //   - --icf=all: fold identical functions (template-heavy USD; lld only).
        //   - --undefined-version: NDK 29+ lld errors on version-script symbols;
        //     matches the oneTBB Android build below.
        // NOTE: do NOT add -fvisibility=hidden or -Wl,--exclude-libs,ALL here.
        // OpenUSD's monolith relies on default ELF visibility: template statics
        // and vtables (TfSingleton<>::_instance, Sdf_Pool<>::_regionStarts,
        // Tf_Remnant) aren't ARCH_EXPORT-annotated and get hidden, breaking the
        // link of anything that consumes the .so.
        "-DCMAKE_C_FLAGS_RELEASE=-O3 -DNDEBUG -g0 -ffunction-sections -fdata-sections".to_owned(),
        "-DCMAKE_CXX_FLAGS_RELEASE=-O3 -DNDEBUG -g0 -ffunction-sections -fdata-sections".to_owned(),
        "-DCMAKE_SHARED_LINKER_FLAGS_RELEASE=-Wl,--gc-sections -Wl,--icf=all -Wl,--strip-all -Wl,--undefined-version".to_owned(),
        format!("-DTBB_ROOT={}", tbb_root.display()),
        format!(
            "-DTBB_DIR={}",
            tbb_root.join("lib").join("cmake").join("TBB").display()
        ),
        format!("-DCMAKE_PREFIX_PATH={}", tbb_root.display()),
        "-DPXR_FIND_TBB_IN_CONFIG=ON".to_owned(),
    ]);
    match materialx_dir {
        Some(mx) => {
            // DIVERGENCE from idtx-flow: MaterialX ON.
            args.push("-DPXR_ENABLE_MATERIALX_SUPPORT=ON".to_owned());
            args.push(format!(
                "-DMaterialX_DIR={}",
                mx.join("lib").join("cmake").join("MaterialX").display()
            ));
        }
        None => {
            args.push("-DPXR_ENABLE_MATERIALX_SUPPORT=OFF".to_owned());
        }
    }
    args
}

// ---------------------------------------------------------------------------
// visionOS CMake argument builders — pure, process-free, unit-tested
// ---------------------------------------------------------------------------

/// A resolved visionOS SDK target (device slice `xros` or simulator `xrsimulator`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionOsSlice {
    /// On-device: `arm64-apple-xros`.
    Device,
    /// Simulator: `arm64-apple-xrsimulator`.
    Simulator,
}

impl VisionOsSlice {
    /// The Apple SDK name passed to `-DCMAKE_OSX_SYSROOT` / `xcrun --sdk`.
    #[must_use]
    pub const fn sdk(self) -> &'static str {
        match self {
            Self::Device => "xros",
            Self::Simulator => "xrsimulator",
        }
    }

    /// The clang target triple for this slice.
    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::Device => "arm64-apple-xros",
            Self::Simulator => "arm64-apple-xros-simulator",
        }
    }
}

/// Base CMake args shared by every visionOS cross-build (TBB, `MaterialX`,
/// `OpenUSD`). Targets Apple's `XROS`/`XRSimulator` SDK with the arm64 slice.
#[must_use]
pub fn visionos_toolchain_args(slice: VisionOsSlice, deployment_target: &str) -> Vec<String> {
    vec![
        "-G".to_owned(),
        "Ninja".to_owned(),
        "-DCMAKE_SYSTEM_NAME=visionOS".to_owned(),
        format!("-DCMAKE_OSX_SYSROOT={}", slice.sdk()),
        "-DCMAKE_OSX_ARCHITECTURES=arm64".to_owned(),
        format!("-DCMAKE_OSX_DEPLOYMENT_TARGET={deployment_target}"),
        "-DCMAKE_BUILD_TYPE=Release".to_owned(),
    ]
}

/// CMake args for `OpenUSD` on visionOS. `MaterialX` uses the Metal shader
/// generator (`MaterialXGenMsl`) rather than GLSL.
#[must_use]
pub fn usd_visionos_cmake_args(
    slice: VisionOsSlice,
    deployment_target: &str,
    install_path: &Path,
    tbb_root: &Path,
    materialx_dir: Option<&Path>,
    with_openexec: bool,
) -> Vec<String> {
    let mut args = visionos_toolchain_args(slice, deployment_target);
    args.extend([
        format!("-DCMAKE_INSTALL_PREFIX={}", install_path.display()),
        "-DCMAKE_CXX_STANDARD=17".to_owned(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_owned(),
        "-DBUILD_SHARED_LIBS=ON".to_owned(),
        "-DPXR_BUILD_MONOLITHIC=ON".to_owned(),
        "-DPXR_BUILD_IMAGING=OFF".to_owned(),
        "-DPXR_BUILD_USD_IMAGING=OFF".to_owned(),
        "-DPXR_BUILD_USDVIEW=OFF".to_owned(),
        "-DPXR_BUILD_TESTS=OFF".to_owned(),
        "-DPXR_BUILD_EXAMPLES=OFF".to_owned(),
        "-DPXR_BUILD_TUTORIALS=OFF".to_owned(),
        "-DPXR_ENABLE_PYTHON_SUPPORT=OFF".to_owned(),
        "-DPXR_ENABLE_OPENVDB_SUPPORT=OFF".to_owned(),
        "-DPXR_ENABLE_VULKAN_SUPPORT=OFF".to_owned(),
        // Unused 26.08 Exec framework — OFF by default for parity with the Android
        // build (see `usd_android_cmake_args`). `--with-openexec` flips it ON so the
        // monolith carries the Vdf/Exec/Ef symbols the coded `idtx-usd-exec` plugin
        // links on device (same NDK/Clang relaxed-rule hazard as Android).
        format!("-DPXR_BUILD_EXEC={}", if with_openexec { "ON" } else { "OFF" }),
        // Size reduction for the visionOS monolith (.dylib). Apple's `ld64`
        // (Mach-O) does NOT accept the GNU/lld ELF flags used by the Android
        // build (`--gc-sections`, `--icf`, `--strip-all`, `--undefined-version`)
        // — passing them fails the link. `-dead_strip` is the Mach-O dead-code
        // elimination equivalent; the `-ffunction/-fdata-sections` compiler flags
        // below are clang-portable and let it drop unreferenced symbols. As with
        // the Android build, do NOT add `-fvisibility=hidden`: OpenUSD's monolith
        // relies on default visibility for un-annotated template statics/vtables.
        "-DCMAKE_C_FLAGS_RELEASE=-O3 -DNDEBUG -g0 -ffunction-sections -fdata-sections".to_owned(),
        "-DCMAKE_CXX_FLAGS_RELEASE=-O3 -DNDEBUG -g0 -ffunction-sections -fdata-sections".to_owned(),
        "-DCMAKE_SHARED_LINKER_FLAGS_RELEASE=-Wl,-dead_strip".to_owned(),
        format!("-DTBB_ROOT={}", tbb_root.display()),
        format!(
            "-DTBB_DIR={}",
            tbb_root.join("lib").join("cmake").join("TBB").display()
        ),
        format!("-DCMAKE_PREFIX_PATH={}", tbb_root.display()),
        "-DPXR_FIND_TBB_IN_CONFIG=ON".to_owned(),
    ]);
    match materialx_dir {
        Some(mx) => {
            args.push("-DPXR_ENABLE_MATERIALX_SUPPORT=ON".to_owned());
            args.push(format!(
                "-DMaterialX_DIR={}",
                mx.join("lib").join("cmake").join("MaterialX").display()
            ));
        }
        None => args.push("-DPXR_ENABLE_MATERIALX_SUPPORT=OFF".to_owned()),
    }
    args
}

// ---------------------------------------------------------------------------
// Android source patches — pure data, applied/reverted around the build
// ---------------------------------------------------------------------------

/// One Android source patch: a source file (relative to the `OpenUSD` root) and
/// the ordered `(old, new)` substring replacements to apply to it. Ported
/// verbatim from `idtx-flow/scons/tools/openusd.py` `_patch_openusd_for_android`.
#[derive(Debug, Clone, Copy)]
pub struct AndroidPatch {
    /// Path relative to the `OpenUSD` source root, using `/` separators.
    pub rel_path: &'static str,
    /// `(old, new)` replacements applied in order, first match each.
    pub edits: &'static [(&'static str, &'static str)],
}

/// The five Android source patches (NDK libc++ quirks). Copied verbatim from
/// the `idtx-flow` recipe; the orchestration applies them to a backup-guarded
/// source tree and reverts them in a RAII guard so the tree is left pristine.
#[must_use]
pub fn android_patches() -> &'static [AndroidPatch] {
    &[
        // 1) align.cpp — treat __ANDROID__ like Darwin / glibcxx-without-aligned_alloc.
        AndroidPatch {
            rel_path: "pxr/base/arch/align.cpp",
            edits: &[
                (
                    "#if defined(ARCH_OS_DARWIN) || (defined(ARCH_OS_LINUX) && defined(__GLIBCXX__) && !defined(_GLIBCXX_HAVE_ALIGNED_ALLOC))\n    // alignment must be >= sizeof(void*)",
                    "#if defined(ARCH_OS_DARWIN) || defined(__ANDROID__) || (defined(ARCH_OS_LINUX) && defined(__GLIBCXX__) && !defined(_GLIBCXX_HAVE_ALIGNED_ALLOC))\n    // alignment must be >= sizeof(void*)",
                ),
                (
                    "#if defined(ARCH_OS_DARWIN) || (defined(ARCH_OS_LINUX) && defined(__GLIBCXX__) && !defined(_GLIBCXX_HAVE_ALIGNED_ALLOC))\n    free(ptr);",
                    "#if defined(ARCH_OS_DARWIN) || defined(__ANDROID__) || (defined(ARCH_OS_LINUX) && defined(__GLIBCXX__) && !defined(_GLIBCXX_HAVE_ALIGNED_ALLOC))\n    free(ptr);",
                ),
            ],
        },
        // 2) daemon.cpp — define NOFILE on Android.
        AndroidPatch {
            rel_path: "pxr/base/arch/daemon.cpp",
            edits: &[(
                "#include <sys/param.h>",
                "#include <sys/param.h>\n#if defined(__ANDROID__) && !defined(NOFILE)\n#define NOFILE 256\n#endif",
            )],
        },
        // 3) stackTrace.cpp — environ vs __environ on Android.
        AndroidPatch {
            rel_path: "pxr/base/arch/stackTrace.cpp",
            edits: &[(
                "#if defined(ARCH_OS_LINUX)\n     return nonLockingLinux__execve (path, argv, __environ);",
                "#if defined(ARCH_OS_LINUX)\n#if defined(__ANDROID__)\n     return nonLockingLinux__execve (path, argv, environ);\n#else\n     return nonLockingLinux__execve (path, argv, __environ);\n#endif",
            )],
        },
        // 4) bboxCache.h — shared_ptr<T[]> → shared_ptr<T> for NDK libc++.
        AndroidPatch {
            rel_path: "pxr/usd/usdGeom/bboxCache.h",
            edits: &[(
                "std::shared_ptr<UsdAttributeQuery[]> queries;",
                "std::shared_ptr<UsdAttributeQuery> queries;",
            )],
        },
        // 5) bboxCache.cpp — adapt usages to the non-array shared_ptr.
        AndroidPatch {
            rel_path: "pxr/usd/usdGeom/bboxCache.cpp",
            edits: &[
                (
                    "std::shared_ptr<UsdAttributeQuery[]> &queries = entry->queries;",
                    "std::shared_ptr<UsdAttributeQuery> &queries = entry->queries;",
                ),
                (
                    "queries.reset(new UsdAttributeQuery[numQueries]);",
                    "queries.reset(new UsdAttributeQuery[numQueries], std::default_delete<UsdAttributeQuery[]>());",
                ),
                ("&queries[ExtentsHint]", "&queries.get()[ExtentsHint]"),
                ("&queries[Extent]", "&queries.get()[Extent]"),
            ],
        },
    ]
}

/// Apply `patch`'s edits to the text of one source file in `src_root`. Returns
/// the patched text, or an error listing any edit whose `old` was not found
/// (a hard error here, unlike `idtx-flow`'s warn-and-continue, because a
/// missing pattern means the pinned `OpenUSD` version drifted and the build
/// would fail confusingly later).
pub fn apply_patch_text(original: &str, patch: &AndroidPatch) -> Result<String> {
    // Normalize CRLF → LF before matching. The git checkout forces LF (see
    // `git_clone_tag`), but a stray CRLF — e.g. from a submodule checkout that
    // did not inherit the clone's config — must never turn a real match into a
    // "pattern not found" failure, since every edit pattern is authored with
    // `\n`. Normalizing here also guarantees the patched file is written as LF.
    let mut content = original.replace("\r\n", "\n");
    for (old, new) in patch.edits {
        if !content.contains(old) {
            bail!(
                "Android patch pattern not found in {} (OpenUSD {} drift?): {:.200}",
                patch.rel_path,
                OPENUSD_VERSION,
                old
            );
        }
        content = content.replacen(old, new, 1);
    }
    Ok(content)
}

/// Gate `MaterialX`'s installed-config `find_dependency(X11 ...)` on the render
/// module (pure text transform; the orchestration reads/writes the file).
///
/// Upstream `cmake/modules/MaterialXConfig.cmake.in` emits an unconditional
/// `find_dependency(X11 REQUIRED COMPONENTS Xt)` for every `UNIX AND NOT APPLE`
/// consumer — which includes an Android cross-build, where there is no X11. X11
/// is only a render/viewer dependency, and we build with
/// `MATERIALX_BUILD_RENDER=OFF`, so without this gate `OpenUSD`'s
/// `find_package(MaterialX)` aborts with "Could NOT find X11". Gating the block
/// on `@MATERIALX_BUILD_RENDER@` makes the generated config skip X11 when render
/// is off and preserves the desktop-Linux behaviour when it is on. Idempotent:
/// returns the text unchanged if it is already gated.
pub fn patch_materialx_config_text(original: &str) -> Result<String> {
    const OLD: &str = "if(UNIX AND NOT APPLE)\n    find_dependency(X11 REQUIRED COMPONENTS Xt)";
    const NEW: &str =
        "if(@MATERIALX_BUILD_RENDER@ AND UNIX AND NOT APPLE)\n    find_dependency(X11 REQUIRED COMPONENTS Xt)";
    let content = original.replace("\r\n", "\n");
    if content.contains(NEW) {
        return Ok(content);
    }
    if !content.contains(OLD) {
        bail!(
            "MaterialXConfig.cmake.in X11 block not found (MaterialX {MATERIALX_VERSION} drift?)"
        );
    }
    Ok(content.replacen(OLD, NEW, 1))
}

/// Apply [`patch_materialx_config_text`] to the `MaterialX` config template in a
/// cloned source tree, in place. A no-op when the tree is already patched.
fn patch_materialx_android_config(mx_src: &Path) -> Result<()> {
    let cfg = mx_src
        .join("cmake")
        .join("modules")
        .join("MaterialXConfig.cmake.in");
    let original = std::fs::read_to_string(&cfg)
        .with_context(|| format!("reading {} to patch", cfg.display()))?;
    let patched = patch_materialx_config_text(&original)?;
    if patched != original {
        std::fs::write(&cfg, patched)
            .with_context(|| format!("writing patched {}", cfg.display()))?;
        println!("  patched MaterialXConfig.cmake.in: X11 dependency now gated on MATERIALX_BUILD_RENDER");
    }
    Ok(())
}

/// Absolute paths of the files patched by [`android_patches`], rooted at
/// `src_root` (the cloned `OpenUSD` tree). Pure — no filesystem access — so it
/// can be unit-tested and reused by both the apply step and the git-restore
/// guard.
#[must_use]
pub fn patched_source_paths(src_root: &Path) -> Vec<PathBuf> {
    android_patches()
        .iter()
        .map(|p| src_root.join(p.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR)))
        .collect()
}

// ---------------------------------------------------------------------------
// Staging prefixes
// ---------------------------------------------------------------------------

/// The in-repo staging prefix for a cross-built standalone `OpenUSD` install,
/// mirroring the Windows install layout the runtime staging expects.
#[must_use]
pub fn staging_prefix(workspace_root: &Path, platform: StandalonePlatform) -> PathBuf {
    let leaf = match platform {
        StandalonePlatform::Quest => "android-arm64",
        StandalonePlatform::VisionPro => "visionos-arm64",
    };
    workspace_root
        .join(THIRDPARTY_DIR)
        .join(format!("openusd-{OPENUSD_VERSION}"))
        .join(leaf)
}

/// The native library file names expected in a finished staging prefix for the
/// platform (delegates to `idtx-standalone`, the single source of truth — B3).
#[must_use]
pub fn expected_staged_libs(platform: StandalonePlatform) -> Vec<String> {
    match platform {
        StandalonePlatform::Quest => android_runtime_libs(),
        StandalonePlatform::VisionPro => visionos_runtime_libs(),
    }
}

// ---------------------------------------------------------------------------
// Build plans (resolved toolchain + paths) and orchestration
// ---------------------------------------------------------------------------

/// The workspace root (the directory holding the `[workspace]` `Cargo.toml` =
/// `idtx-studio`). Delegates to [`crate::workspace_root`]; falls back to `xtask`'s
/// manifest parent only if that walk fails.
fn workspace_root() -> PathBuf {
    crate::workspace_root().unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    })
}

/// A fully resolved Android cross-build plan.
#[derive(Debug, Clone)]
pub struct AndroidPlan {
    pub toolchain: AndroidToolchain,
    pub ninja: Option<PathBuf>,
    pub materialx: bool,
    pub with_openexec: bool,
    pub thirdparty: PathBuf,
    pub install_prefix: PathBuf,
}

impl AndroidPlan {
    fn resolve(opts: &CrossOptions) -> Result<Self> {
        let toolchain = AndroidToolchain::resolve()?;
        let root = workspace_root();
        Ok(Self {
            toolchain,
            ninja: locate_sdk_ninja(),
            materialx: opts.materialx(),
            with_openexec: opts.with_openexec,
            thirdparty: root.join(THIRDPARTY_DIR),
            install_prefix: staging_prefix(&root, StandalonePlatform::Quest),
        })
    }

    fn describe(&self) {
        println!("build-usd-android plan:");
        println!("  NDK:          {}", self.toolchain.ndk_root.display());
        println!(
            "  Toolchain:    {}",
            self.toolchain.toolchain_bin().display()
        );
        println!("  API level:    android-{}", self.toolchain.api_level);
        println!("  ABI:          {}", self.toolchain.abi);
        println!(
            "  Ninja:        {}",
            self.ninja
                .as_deref()
                .map_or_else(|| "<PATH>".to_owned(), |p| p.display().to_string())
        );
        println!("  OpenUSD:      v{OPENUSD_VERSION}");
        println!("  oneTBB:       v{ONETBB_VERSION}");
        if self.materialx {
            println!("  MaterialX:    v{MATERIALX_VERSION} (ENABLED — divergence from idtx-flow)");
        } else {
            println!(
                "  MaterialX:    DISABLED (--no-materialx; idtx-flow parity, built-in PBR only)"
            );
        }
        println!("  Install:      {}", self.install_prefix.display());
    }
}

/// A fully resolved visionOS cross-build plan.
#[derive(Debug, Clone)]
pub struct VisionOsPlan {
    pub deployment_target: String,
    pub materialx: bool,
    pub with_openexec: bool,
    pub thirdparty: PathBuf,
    pub install_prefix: PathBuf,
}

impl VisionOsPlan {
    fn resolve(opts: &CrossOptions) -> Result<Self> {
        let root = workspace_root();
        Ok(Self {
            deployment_target: visionos_deployment_target(),
            materialx: opts.materialx(),
            with_openexec: opts.with_openexec,
            thirdparty: root.join(THIRDPARTY_DIR),
            install_prefix: staging_prefix(&root, StandalonePlatform::VisionPro),
        })
    }

    fn describe(&self) {
        println!("build-usd-visionos plan:");
        println!("  Deployment:   visionOS {}", self.deployment_target);
        println!("  Slices:       device (xros) + simulator (xrsimulator)");
        println!("  OpenUSD:      v{OPENUSD_VERSION}");
        println!("  oneTBB:       v{ONETBB_VERSION}");
        if self.materialx {
            println!("  MaterialX:    v{MATERIALX_VERSION} (ENABLED, Metal gen MaterialXGenMsl)");
        } else {
            println!("  MaterialX:    DISABLED (--no-materialx)");
        }
        println!("  Install:      {}", self.install_prefix.display());
    }
}

/// Environment variable overriding the visionOS deployment target, shared by the
/// USD-lib cross-build and the `idtx-runtime` cross-build so a cooked `.app`'s
/// runtime and its embedded USD dylibs declare the same minimum visionOS version.
pub const VISIONOS_DEPLOYMENT_TARGET_ENV: &str = "IDTX_VISIONOS_DEPLOYMENT_TARGET";

/// The visionOS deployment target: `$IDTX_VISIONOS_DEPLOYMENT_TARGET` or the
/// `2.0` default. Kept as a free function (no resolved SDK needed) so it can
/// default on a dry-run host, and reused by `visionos_runtime` so the runtime's
/// min-OS matches the USD slice it links against.
#[must_use]
pub fn visionos_deployment_target() -> String {
    std::env::var(VISIONOS_DEPLOYMENT_TARGET_ENV).unwrap_or_else(|_| "2.0".to_owned())
}

// ---------------------------------------------------------------------------
// Orchestration (shells out — only reached when not --dry-run)
// ---------------------------------------------------------------------------

/// Clone a git tag shallowly into `dest` if it does not already exist.
fn git_clone_tag(url: &str, tag: &str, dest: &Path, recursive: bool) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut cmd = Command::new("git");
    // Force LF line endings on checkout regardless of the host's global git
    // config. On Windows a global `core.autocrlf=true` rewrites the cloned
    // sources with CRLF, which breaks the byte-exact substring matching in
    // `apply_patch_text` (its Android patch patterns are authored with `\n`)
    // and makes the OpenUSD build fail with a confusing "pattern not found".
    // The patched files live in the top-level OpenUSD repo, so command-line
    // `-c` config here covers them even though submodule checkouts may not
    // inherit it (`apply_patch_text` also normalizes CRLF as a safety net).
    cmd.args(["-c", "core.autocrlf=false", "-c", "core.eol=lf"]);
    cmd.args(["clone", "-b", tag]);
    if recursive {
        cmd.arg("--recursive");
    }
    cmd.args(["--depth", "2", url]);
    cmd.arg(dest);
    run(&mut cmd, &format!("git clone {url}@{tag}"))
}

/// Run a CMake configure → build → install cycle in `build_dir` from `source`.
fn cmake_cycle(source: &Path, build_dir: &Path, args: &[String], label: &str) -> Result<()> {
    std::fs::create_dir_all(build_dir).ok();
    let mut configure = Command::new("cmake");
    configure.arg(format!("-S{}", source.display()));
    configure.arg(format!("-B{}", build_dir.display()));
    configure.args(args);
    run(&mut configure, &format!("{label}: cmake configure"))?;

    let mut build = Command::new("cmake");
    build.args([
        "--build",
        &build_dir.display().to_string(),
        "--config",
        "Release",
        "--parallel",
    ]);
    run(&mut build, &format!("{label}: cmake build"))?;

    let mut install = Command::new("cmake");
    install.args([
        "--install",
        &build_dir.display().to_string(),
        "--config",
        "Release",
    ]);
    run(&mut install, &format!("{label}: cmake install"))
}

/// Restore the patched `OpenUSD` sources to the pristine content of the cloned
/// tag by checking them out from `HEAD` with `git checkout HEAD --`. Git is
/// the authoritative "pristine" state (the tree is always a fresh
/// [`git_clone_tag`] checkout and the patches are never committed), so this
/// needs no `.bak` sidecar files and is idempotent on an already-clean tree.
///
/// The explicit `HEAD` tree-ish matters: `git checkout -- <path>` (no tree-ish)
/// restores from the **index**, which fails with "pathspec did not match any
/// file(s) known to git" whenever the index is missing or empty (e.g. a
/// `.git/index` deleted by antivirus/an indexer, or an interrupted clone).
/// `git checkout HEAD -- <path>` resolves the pathspecs against the committed
/// tag tree instead, so it works regardless of index state and rebuilds the
/// index entries as a side effect — exactly the "pristine from the cloned tag"
/// semantics we want.
///
/// The `rel_path`s use `/` separators, which git accepts on every host
/// including Windows.
fn git_restore_sources(src_root: &Path) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(src_root).args(["checkout", "HEAD", "--"]);
    for patch in android_patches() {
        cmd.arg(patch.rel_path);
    }
    run(
        &mut cmd,
        "git checkout -- (restore patched OpenUSD sources)",
    )
}

/// Best-effort removal of any `*.android_bak` sidecar files left by an earlier
/// backup-based patch implementation (or an interrupted run of it). Migration
/// cleanup so stale backups disappear on the next build; failures are ignored.
fn clean_legacy_backups(src_root: &Path) {
    for path in patched_source_paths(src_root) {
        let bak = path.with_extension("android_bak");
        if bak.exists() {
            if let Err(e) = std::fs::remove_file(&bak) {
                println!(
                    "note: could not remove legacy backup {}: {e}",
                    bak.display()
                );
            }
        }
    }
}

/// RAII guard that keeps the cloned `OpenUSD` tree pristine around the build.
///
/// Patches are applied in place and reverted with git — not `.bak` copies — so
/// the operation is **self-healing**: [`PatchGuard::apply`] first restores the
/// sources, meaning a run interrupted before [`Drop`] ran (e.g. Ctrl-C, which
/// skips destructors) can never poison the next run. [`Drop`] restores again on
/// the normal/failed-build path.
struct PatchGuard {
    src_root: PathBuf,
}

impl PatchGuard {
    /// Restore the tree to pristine, then apply every patch in place.
    fn apply(src_root: &Path) -> Result<Self> {
        // 1) Self-heal: start from a guaranteed-pristine tree, discarding any
        //    leftover patches from a previously interrupted run and sweeping any
        //    stale backups from the old backup-based implementation.
        git_restore_sources(src_root)?;
        clean_legacy_backups(src_root);

        // 2) Apply the patches to the now-pristine sources.
        println!(
            "Applying {} temporary Android patches to OpenUSD source...",
            android_patches().len()
        );
        for (patch, path) in android_patches().iter().zip(patched_source_paths(src_root)) {
            let original = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {} to patch", path.display()))?;
            let patched = apply_patch_text(&original, patch)?;
            std::fs::write(&path, patched)
                .with_context(|| format!("writing patched {}", path.display()))?;
        }
        Ok(Self {
            src_root: src_root.to_path_buf(),
        })
    }
}

impl Drop for PatchGuard {
    fn drop(&mut self) {
        println!("Reverting Android patches from OpenUSD source...");
        if let Err(e) = git_restore_sources(&self.src_root) {
            let root = self.src_root.display();
            eprintln!(
                "WARNING: failed to git-restore patched OpenUSD sources at {root}: {e}. \
                 Run `git -C {root} checkout HEAD -- pxr/` or just re-run build-usd-android, \
                 which self-heals the tree before patching."
            );
        }
    }
}

#[cfg(feature = "metrics")]
macro_rules! time_step {
    ($label:expr, $body:expr) => {{
        let start = std::time::Instant::now();
        let out = $body;
        eprintln!("[metrics] {} took {:?}", $label, start.elapsed());
        out
    }};
}
#[cfg(not(feature = "metrics"))]
macro_rules! time_step {
    ($label:expr, $body:expr) => {{ $body }};
}

/// Orchestrate the Android cross-build. Only reached when `--dry-run` is off and
/// the NDK resolved, so shelling out is justified.
fn run_android_build(plan: &AndroidPlan) -> Result<()> {
    let tc = &plan.toolchain;
    let ninja = plan.ninja.as_deref();
    std::fs::create_dir_all(&plan.thirdparty).ok();

    // 1) oneTBB.
    let tbb_src = plan.thirdparty.join(format!("onetbb-{ONETBB_VERSION}-src"));
    let tbb_install = plan.thirdparty.join("onetbb-android");
    git_clone_tag(
        "https://github.com/oneapi-src/oneTBB.git",
        &format!("v{ONETBB_VERSION}"),
        &tbb_src,
        false,
    )?;
    time_step!(
        "oneTBB-Android",
        cmake_cycle(
            &tbb_src,
            &plan.thirdparty.join("onetbb-android-build"),
            &tbb_android_cmake_args(tc, ninja, &tbb_install),
            "oneTBB-Android",
        )
    )?;

    // 2) MaterialX (our divergence) — optional, loud fallback on failure.
    let materialx_install = plan.thirdparty.join("materialx-android");
    let materialx_dir = if plan.materialx {
        let mx_src = plan
            .thirdparty
            .join(format!("materialx-{MATERIALX_VERSION}-src"));
        git_clone_tag(
            "https://github.com/AcademySoftwareFoundation/MaterialX.git",
            &format!("v{MATERIALX_VERSION}"),
            &mx_src,
            true,
        )?;
        // MaterialX's installed CMake config unconditionally pulls in X11 on any
        // UNIX-non-Apple platform (Android included). With render off there is no
        // X11 and no need for it; gate that block on the render flag so OpenUSD's
        // `find_package(MaterialX)` does not abort with "Could NOT find X11".
        patch_materialx_android_config(&mx_src)?;
        let built = time_step!(
            "MaterialX-Android",
            cmake_cycle(
                &mx_src,
                &plan.thirdparty.join("materialx-android-build"),
                &materialx_android_cmake_args(tc, ninja, &materialx_install),
                "MaterialX-Android",
            )
        );
        match built {
            Ok(()) => Some(materialx_install.clone()),
            Err(e) => {
                eprintln!(
                    "WARNING: MaterialX Android cross-build FAILED ({e}). \
                     Falling back to PXR_ENABLE_MATERIALX_SUPPORT=OFF (built-in PBR only). \
                     Re-run without this fallback once the MaterialX/NDK issue is fixed."
                );
                None
            }
        }
    } else {
        None
    };

    // 3) OpenUSD (patched source, reverted by the guard).
    let usd_src = plan
        .thirdparty
        .join(format!("openusd-{OPENUSD_VERSION}/src"));
    git_clone_tag(
        "https://github.com/PixarAnimationStudios/OpenUSD.git",
        &format!("v{OPENUSD_VERSION}"),
        &usd_src,
        true,
    )?;
    {
        let _guard = PatchGuard::apply(&usd_src)?;
        time_step!(
            "OpenUSD-Android",
            cmake_cycle(
                &usd_src,
                &plan.thirdparty
                    .join(format!("openusd-{OPENUSD_VERSION}"))
                    .join("android-build"),
                &usd_android_cmake_args(
                    tc,
                    ninja,
                    &plan.install_prefix,
                    &tbb_install,
                    materialx_dir.as_deref(),
                    plan.with_openexec
                ),
                "OpenUSD-Android",
            )
        )?;
    }

    verify_staged(&plan.install_prefix, StandalonePlatform::Quest);
    println!(
        "OpenUSD for Android staged at {}",
        plan.install_prefix.display()
    );
    Ok(())
}

/// Orchestrate the visionOS cross-build (macOS only; the caller gates this).
/// Builds **both** slices — device (`xros`) and simulator (`xrsimulator`) — into
/// separate install prefixes so a `.xcframework` can be assembled from them
/// (the lipo/xcodebuild step is handled by the `.app` packager).
fn run_visionos_build(plan: &VisionOsPlan) -> Result<()> {
    std::fs::create_dir_all(&plan.thirdparty).ok();
    for slice in [VisionOsSlice::Device, VisionOsSlice::Simulator] {
        println!("── visionOS slice: {} ({}) ──", slice.sdk(), slice.triple());
        run_visionos_slice(plan, slice)?;
    }
    println!(
        "OpenUSD for visionOS staged at {} (device) and {}-sim (simulator)",
        plan.install_prefix.display(),
        plan.install_prefix.display()
    );
    Ok(())
}

/// Cross-build oneTBB + (optional) `MaterialX` + `OpenUSD` for one visionOS slice.
fn run_visionos_slice(plan: &VisionOsPlan, slice: VisionOsSlice) -> Result<()> {
    let suffix = match slice {
        VisionOsSlice::Device => String::new(),
        VisionOsSlice::Simulator => "-sim".to_owned(),
    };
    let install_prefix = append_suffix(&plan.install_prefix, &suffix);

    let tbb_src = plan.thirdparty.join(format!("onetbb-{ONETBB_VERSION}-src"));
    let tbb_install = plan.thirdparty.join(format!("onetbb-visionos{suffix}"));
    git_clone_tag(
        "https://github.com/oneapi-src/oneTBB.git",
        &format!("v{ONETBB_VERSION}"),
        &tbb_src,
        false,
    )?;
    let mut tbb_args = visionos_toolchain_args(slice, &plan.deployment_target);
    tbb_args.extend([
        format!("-DCMAKE_INSTALL_PREFIX={}", tbb_install.display()),
        "-DTBB_TEST=OFF".to_owned(),
        "-DTBB_STRICT=OFF".to_owned(),
        "-DBUILD_SHARED_LIBS=ON".to_owned(),
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5".to_owned(),
    ]);
    time_step!(
        "oneTBB-visionOS",
        cmake_cycle(
            &tbb_src,
            &plan
                .thirdparty
                .join(format!("onetbb-visionos{suffix}-build")),
            &tbb_args,
            "oneTBB-visionOS",
        )
    )?;

    let materialx_install = plan.thirdparty.join(format!("materialx-visionos{suffix}"));
    let materialx_dir = if plan.materialx {
        Some(materialx_install.as_path())
    } else {
        None
    };
    if plan.materialx {
        let mx_src = plan
            .thirdparty
            .join(format!("materialx-{MATERIALX_VERSION}-src"));
        git_clone_tag(
            "https://github.com/AcademySoftwareFoundation/MaterialX.git",
            &format!("v{MATERIALX_VERSION}"),
            &mx_src,
            true,
        )?;
        let mut mx_args = visionos_toolchain_args(slice, &plan.deployment_target);
        mx_args.extend([
            format!("-DCMAKE_INSTALL_PREFIX={}", materialx_install.display()),
            "-DCMAKE_CXX_STANDARD=17".to_owned(),
            "-DBUILD_SHARED_LIBS=ON".to_owned(),
            "-DMATERIALX_BUILD_SHARED_LIBS=ON".to_owned(),
            "-DMATERIALX_BUILD_PYTHON=OFF".to_owned(),
            "-DMATERIALX_BUILD_VIEWER=OFF".to_owned(),
            "-DMATERIALX_BUILD_TESTS=OFF".to_owned(),
            // Render module OFF — no X11/windowing on visionOS either, and only
            // the generation libs are linked (see the Android args for the full
            // rationale). Keeps the slice headless and matches the generation-only
            // standalone staging set.
            "-DMATERIALX_BUILD_RENDER=OFF".to_owned(),
            "-DMATERIALX_BUILD_GEN_MSL=ON".to_owned(),
        ]);
        time_step!(
            "MaterialX-visionOS",
            cmake_cycle(
                &mx_src,
                &plan
                    .thirdparty
                    .join(format!("materialx-visionos{suffix}-build")),
                &mx_args,
                "MaterialX-visionOS",
            )
        )?;
    }

    let usd_src = plan
        .thirdparty
        .join(format!("openusd-{OPENUSD_VERSION}-src"));
    git_clone_tag(
        "https://github.com/PixarAnimationStudios/OpenUSD.git",
        &format!("v{OPENUSD_VERSION}"),
        &usd_src,
        true,
    )?;
    time_step!(
        "OpenUSD-visionOS",
        cmake_cycle(
            &usd_src,
            &plan
                .thirdparty
                .join(format!("openusd-visionos{suffix}-build")),
            &usd_visionos_cmake_args(
                slice,
                &plan.deployment_target,
                &install_prefix,
                &tbb_install,
                materialx_dir,
                plan.with_openexec,
            ),
            "OpenUSD-visionOS",
        )
    )?;

    verify_staged(&install_prefix, StandalonePlatform::VisionPro);
    Ok(())
}

/// Append a suffix to the final path component (`/a/b/c` + `-sim` → `/a/b/c-sim`).
fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return path.to_path_buf();
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    path.with_file_name(format!("{name}{suffix}"))
}

/// Warn (do not fail) about any expected native library missing from a finished
/// staging prefix — a cross-build can succeed yet drop an optional `MaterialX`
/// generator, which the runtime staging tolerates.
fn verify_staged(prefix: &Path, platform: StandalonePlatform) {
    let lib_dir = prefix.join("lib");
    let missing: Vec<_> = expected_staged_libs(platform)
        .into_iter()
        .filter(|name| !lib_dir.join(name).is_file() && !prefix.join(name).is_file())
        .collect();
    if missing.is_empty() {
        println!(
            "All {} expected native libraries present.",
            expected_staged_libs(platform).len()
        );
    } else {
        eprintln!(
            "WARNING: {} expected native libraries missing from staging: {:?}",
            missing.len(),
            missing
        );
    }
}

/// Run a command, converting a non-zero exit or spawn failure into an error.
fn run(cmd: &mut Command, label: &str) -> Result<()> {
    println!("  $ {cmd:?}");
    let status = cmd
        .status()
        .with_context(|| format!("spawning `{label}`"))?;
    if !status.success() {
        bail!("`{label}` failed with {status}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — pure logic only (no NDK/SDK, no process execution)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_ndk() -> AndroidToolchain {
        AndroidToolchain {
            ndk_root: PathBuf::from("/opt/ndk"),
            api_level: DEFAULT_ANDROID_API_LEVEL,
            abi: ANDROID_ABI.to_owned(),
        }
    }

    #[test]
    fn android_toolchain_args_carry_ndk_abi_and_platform() {
        let tc = fake_ndk();
        let args = android_toolchain_args(&tc, None);
        assert!(args.windows(2).any(|w| w[0] == "-G" && w[1] == "Ninja"));
        assert!(args.iter().any(|a| a.contains("android.toolchain.cmake")));
        assert!(args.iter().any(|a| a == "-DANDROID_ABI=arm64-v8a"));
        assert!(args.iter().any(|a| a == "-DANDROID_PLATFORM=android-30"));
        assert!(args.iter().any(|a| a == "-DANDROID_STL=c++_shared"));
        // No explicit make program unless Ninja is located.
        assert!(!args.iter().any(|a| a.starts_with("-DCMAKE_MAKE_PROGRAM=")));
    }

    #[test]
    fn android_toolchain_args_add_ninja_when_present() {
        let tc = fake_ndk();
        let ninja = PathBuf::from("/sdk/cmake/3.22.1/bin/ninja");
        let args = android_toolchain_args(&tc, Some(&ninja));
        assert!(
            args.iter()
                .any(|a| a.starts_with("-DCMAKE_MAKE_PROGRAM=") && a.contains("ninja"))
        );
    }

    #[test]
    fn materialx_android_args_disable_render_and_viewer() {
        // The MaterialXRender* libs need X11 (absent on Android); the render
        // module must be OFF so configure succeeds headlessly, while the shader
        // generation backends stay ON (they drive material fidelity).
        let args = materialx_android_cmake_args(&fake_ndk(), None, Path::new("/out/mx"));
        assert!(args.iter().any(|a| a == "-DMATERIALX_BUILD_RENDER=OFF"));
        assert!(args.iter().any(|a| a == "-DMATERIALX_BUILD_VIEWER=OFF"));
        assert!(args.iter().any(|a| a == "-DMATERIALX_BUILD_TESTS=OFF"));
        assert!(args.iter().any(|a| a == "-DMATERIALX_BUILD_GEN_GLSL=ON"));
        assert!(args.iter().any(|a| a == "-DMATERIALX_BUILD_GEN_MSL=ON"));
        assert!(args.iter().any(|a| a == "-DMATERIALX_BUILD_SHARED_LIBS=ON"));
    }

    #[test]
    fn materialx_config_patch_gates_x11_on_render_and_is_idempotent() {
        let original = "if(UNIX AND NOT APPLE)\n    find_dependency(X11 REQUIRED COMPONENTS Xt)\n    if(\"RenderGlsl\" IN_LIST MaterialX_FIND_COMPONENTS)\n        find_dependency(OpenGL REQUIRED)\n    endif()\nendif()\n";
        let patched = patch_materialx_config_text(original).unwrap();
        assert!(patched.contains("if(@MATERIALX_BUILD_RENDER@ AND UNIX AND NOT APPLE)"));
        // The bare, ungated opener is gone.
        assert!(!patched.contains("\nif(UNIX AND NOT APPLE)"));
        // Idempotent: re-patching the already-patched text changes nothing.
        assert_eq!(patch_materialx_config_text(&patched).unwrap(), patched);
    }

    #[test]
    fn materialx_config_patch_errors_on_missing_block() {
        assert!(patch_materialx_config_text("no x11 block here").is_err());
    }

    #[test]
    fn tbb_android_args_match_idtx_flow() {
        let args = tbb_android_cmake_args(&fake_ndk(), None, Path::new("/out/tbb"));
        assert!(args.iter().any(|a| a == "-DTBB_TEST=OFF"));
        assert!(args.iter().any(|a| a == "-DTBB_STRICT=OFF"));
        assert!(args.iter().any(|a| a == "-DBUILD_SHARED_LIBS=ON"));
        assert!(
            args.iter()
                .any(|a| a == "-DCMAKE_SHARED_LINKER_FLAGS=-Wl,--undefined-version")
        );
        assert!(
            args.iter()
                .any(|a| a.starts_with("-DCMAKE_INSTALL_PREFIX="))
        );
    }

    #[test]
    fn usd_android_args_enable_materialx_when_dir_given() {
        let args = usd_android_cmake_args(
            &fake_ndk(),
            None,
            Path::new("/out/usd"),
            Path::new("/out/tbb"),
            Some(Path::new("/out/mx")),
            false,
        );
        assert!(
            args.iter()
                .any(|a| a == "-DPXR_ENABLE_MATERIALX_SUPPORT=ON")
        );
        assert!(
            args.iter()
                .any(|a| a.starts_with("-DMaterialX_DIR=") && a.contains("MaterialX"))
        );
        // Core OpenUSD flags preserved from idtx-flow.
        assert!(args.iter().any(|a| a == "-DPXR_BUILD_MONOLITHIC=ON"));
        assert!(args.iter().any(|a| a == "-DPXR_FIND_TBB_IN_CONFIG=ON"));
        assert!(args.iter().any(|a| a == "-DPXR_BUILD_IMAGING=OFF"));
        assert!(args.iter().any(|a| a == "-DPXR_ENABLE_VULKAN_SUPPORT=OFF"));
        // The unused 26.08 Exec framework is disabled by default — it does not
        // compile under the NDK r25 Clang 14 (see usd_android_cmake_args).
        assert!(args.iter().any(|a| a == "-DPXR_BUILD_EXEC=OFF"));
    }

    #[test]
    fn usd_android_args_enable_exec_with_openexec() {
        // `--with-openexec` flips PXR_BUILD_EXEC ON so the monolith carries the
        // OpenExec symbols the coded idtx-usd-exec plugin links against on device.
        let args = usd_android_cmake_args(
            &fake_ndk(),
            None,
            Path::new("/out/usd"),
            Path::new("/out/tbb"),
            None,
            true,
        );
        assert!(args.iter().any(|a| a == "-DPXR_BUILD_EXEC=ON"));
        assert!(!args.iter().any(|a| a == "-DPXR_BUILD_EXEC=OFF"));
    }

    #[test]
    fn usd_android_args_disable_materialx_when_no_dir() {
        let args = usd_android_cmake_args(
            &fake_ndk(),
            None,
            Path::new("/out/usd"),
            Path::new("/out/tbb"),
            None,
            false,
        );
        assert!(
            args.iter()
                .any(|a| a == "-DPXR_ENABLE_MATERIALX_SUPPORT=OFF")
        );
        assert!(
            !args
                .iter()
                .any(|a| a == "-DPXR_ENABLE_MATERIALX_SUPPORT=ON")
        );
        assert!(!args.iter().any(|a| a.starts_with("-DMaterialX_DIR=")));
    }

    #[test]
    fn visionos_args_use_metal_sdk_and_materialx() {
        let args = usd_visionos_cmake_args(
            VisionOsSlice::Device,
            "2.0",
            Path::new("/out/usd"),
            Path::new("/out/tbb"),
            Some(Path::new("/out/mx")),
            false,
        );
        assert!(args.iter().any(|a| a == "-DCMAKE_SYSTEM_NAME=visionOS"));
        assert!(args.iter().any(|a| a == "-DCMAKE_OSX_SYSROOT=xros"));
        assert!(args.iter().any(|a| a == "-DCMAKE_OSX_ARCHITECTURES=arm64"));
        assert!(
            args.iter()
                .any(|a| a == "-DPXR_ENABLE_MATERIALX_SUPPORT=ON")
        );
        assert!(args.iter().any(|a| a == "-DPXR_BUILD_MONOLITHIC=ON"));
        assert!(args.iter().any(|a| a == "-DPXR_BUILD_EXEC=OFF"));
    }

    #[test]
    fn visionos_slice_triples_and_sdks() {
        assert_eq!(VisionOsSlice::Device.sdk(), "xros");
        assert_eq!(VisionOsSlice::Simulator.sdk(), "xrsimulator");
        assert_eq!(VisionOsSlice::Device.triple(), "arm64-apple-xros");
        assert!(VisionOsSlice::Simulator.triple().contains("simulator"));
    }

    #[test]
    fn five_android_patches_cover_the_known_files() {
        let patches = android_patches();
        assert_eq!(patches.len(), 5);
        let paths: Vec<_> = patches.iter().map(|p| p.rel_path).collect();
        assert!(paths.contains(&"pxr/base/arch/align.cpp"));
        assert!(paths.contains(&"pxr/base/arch/daemon.cpp"));
        assert!(paths.contains(&"pxr/base/arch/stackTrace.cpp"));
        assert!(paths.contains(&"pxr/usd/usdGeom/bboxCache.h"));
        assert!(paths.contains(&"pxr/usd/usdGeom/bboxCache.cpp"));
        // Every edit has a non-empty, distinct old/new.
        for p in patches {
            for (old, new) in p.edits {
                assert!(!old.is_empty() && !new.is_empty());
                assert_ne!(old, new, "no-op edit in {}", p.rel_path);
            }
        }
    }

    #[test]
    fn patched_source_paths_cover_the_five_files() {
        let root = Path::new("/clone/openusd-src");
        let paths = patched_source_paths(root);
        assert_eq!(paths.len(), android_patches().len());
        // Every patch's rel_path maps to an absolute path rooted at src_root.
        for (patch, path) in android_patches().iter().zip(&paths) {
            assert!(path.starts_with(root));
            let leaf = patch.rel_path.rsplit('/').next().unwrap();
            assert_eq!(path.file_name().unwrap().to_str().unwrap(), leaf);
        }
    }

    #[test]
    fn clean_legacy_backups_removes_only_bak_files() {
        // Unique temp dir (no tempdir crate in xtask); cleaned up at the end.
        let unique = format!(
            "idtx_xtask_clean_bak_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        // Lay out the patched files plus a stale .android_bak beside each.
        for path in patched_source_paths(&root) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "source").unwrap();
            std::fs::write(path.with_extension("android_bak"), "backup").unwrap();
        }

        clean_legacy_backups(&root);

        for path in patched_source_paths(&root) {
            assert!(
                path.is_file(),
                "real source must survive: {}",
                path.display()
            );
            assert!(
                !path.with_extension("android_bak").exists(),
                "stale backup must be removed: {}",
                path.display()
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn apply_patch_text_rewrites_matched_region() {
        let patch = AndroidPatch {
            rel_path: "pxr/base/arch/daemon.cpp",
            edits: &[(
                "#include <sys/param.h>",
                "#include <sys/param.h>\n#if defined(__ANDROID__) && !defined(NOFILE)\n#define NOFILE 256\n#endif",
            )],
        };
        let original = "// top\n#include <sys/param.h>\n// bottom\n";
        let patched = apply_patch_text(original, &patch).unwrap();
        assert!(patched.contains("#define NOFILE 256"));
        assert!(patched.contains("// bottom"));
    }

    #[test]
    fn apply_patch_text_errors_on_missing_pattern() {
        let patch = AndroidPatch {
            rel_path: "pxr/base/arch/daemon.cpp",
            edits: &[("THIS_PATTERN_DOES_NOT_EXIST", "x")],
        };
        let err = apply_patch_text("unrelated content", &patch).unwrap_err();
        assert!(err.to_string().contains("pattern not found"));
    }

    #[test]
    fn apply_patch_text_matches_crlf_checkout_and_emits_lf() {
        // Simulates a Windows checkout where a global core.autocrlf=true rewrote
        // the source with CRLF. The patterns are authored with LF; normalization
        // must let them still match, and the output must be pure LF.
        let patch = AndroidPatch {
            rel_path: "pxr/base/arch/stackTrace.cpp",
            edits: &[(
                "#if defined(ARCH_OS_LINUX)\n     return nonLockingLinux__execve (path, argv, __environ);",
                "#if defined(ARCH_OS_LINUX)\n#if defined(__ANDROID__)\n     return nonLockingLinux__execve (path, argv, environ);\n#else\n     return nonLockingLinux__execve (path, argv, __environ);\n#endif",
            )],
        };
        let crlf = "x\r\n#if defined(ARCH_OS_LINUX)\r\n     return nonLockingLinux__execve (path, argv, __environ);\r\n// tail\r\n";
        let patched = apply_patch_text(crlf, &patch).unwrap();
        assert!(patched.contains("defined(__ANDROID__)"));
        assert!(!patched.contains('\r'), "patched output must be pure LF");
    }

    #[test]
    fn android_patch_replacements_are_pure_lf() {
        // Every replacement must be authored with `\n`; a `\r` would smuggle a
        // lone CR back into the LF-normalized source (regression guard for the
        // align.cpp patch that once carried a stray `\r\n`).
        for patch in android_patches() {
            for (_, new) in patch.edits {
                assert!(
                    !new.contains('\r'),
                    "patch replacement for {} contains CR: {new:?}",
                    patch.rel_path
                );
            }
        }
    }

    #[test]
    fn real_align_patch_applies_to_representative_source() {
        // The exact upstream guard line the align.cpp patch targets.
        let src = "#if defined(ARCH_OS_DARWIN) || (defined(ARCH_OS_LINUX) && defined(__GLIBCXX__) && !defined(_GLIBCXX_HAVE_ALIGNED_ALLOC))\n    // alignment must be >= sizeof(void*)\n    free(stuff);\n#if defined(ARCH_OS_DARWIN) || (defined(ARCH_OS_LINUX) && defined(__GLIBCXX__) && !defined(_GLIBCXX_HAVE_ALIGNED_ALLOC))\n    free(ptr);\n";
        let align = &android_patches()[0];
        let patched = apply_patch_text(src, align).unwrap();
        assert_eq!(patched.matches("defined(__ANDROID__)").count(), 2);
    }

    #[test]
    fn staging_prefix_mirrors_platform() {
        let root = Path::new("/ws");
        assert!(staging_prefix(root, StandalonePlatform::Quest).ends_with("openusd-26.11.1/android-arm64"));
        assert!(
            staging_prefix(root, StandalonePlatform::VisionPro).ends_with("openusd-26.11.1/visionos-arm64")
        );
    }

    #[test]
    fn expected_staged_libs_delegate_to_standalone() {
        let android = expected_staged_libs(StandalonePlatform::Quest);
        assert_eq!(android, android_runtime_libs());
        assert!(android.iter().all(|n| n.ends_with(".so")));
        let visionos = expected_staged_libs(StandalonePlatform::VisionPro);
        assert_eq!(visionos, visionos_runtime_libs());
        assert!(visionos.iter().all(|n| n.ends_with(".dylib")));
    }

    #[test]
    fn cross_options_parse_flags() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            opts: CrossOptions,
        }
        let parse = |args: &[&str]| -> Result<CrossOptions, clap::Error> {
            TestCli::try_parse_from(std::iter::once("build-usd").chain(args.iter().copied()))
                .map(|c| c.opts)
        };

        // No flags → defaults: MaterialX enabled, dry-run off, openexec off.
        let opts = parse(&[]).unwrap();
        assert_eq!(opts, CrossOptions::default());
        assert!(opts.materialx());
        // --no-materialx disables MaterialX.
        let opts = parse(&["--no-materialx"]).unwrap();
        assert!(!opts.materialx());
        // --dry-run leaves MaterialX enabled.
        let opts = parse(&["--dry-run"]).unwrap();
        assert!(opts.dry_run && opts.materialx());
        // Last flag wins: --no-materialx then --materialx re-enables.
        assert!(parse(&["--no-materialx", "--materialx"]).unwrap().materialx());
        assert!(!parse(&["--materialx", "--no-materialx"]).unwrap().materialx());
        // Unknown flag is rejected.
        assert!(parse(&["--bogus"]).is_err());
    }
}
