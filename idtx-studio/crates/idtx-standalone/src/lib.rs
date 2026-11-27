//! Standalone XR capability, cook, toolchain, and runtime-governor contracts.
//!
//! This crate is the shared, device-agnostic home for the two standalone
//! headset targets — **Android/Quest** (`aarch64-linux-android`, `OpenXR` +
//! `Vulkan`) and **Apple Vision Pro** (`aarch64-apple-visionos`,
//! `CompositorServices` + `Metal` + `ARKit`). Everything here is pure data and trait
//! contracts: it compiles and unit-tests on any host (including Windows) and
//! performs no device syscalls. The platform-specific runtimes, the USD
//! cross-build toolchain, and the packagers consume these contracts.

use std::path::PathBuf;

/// A standalone (on-device, foreign-triple) headset platform.
///
/// Carries the compile- and package-time metadata both targets need — the Rust
/// target triple, the native-ABI identifier, and a minimum-SDK level — without
/// pulling in any platform-specific crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandalonePlatform {
    /// Android / Meta Quest 2·3·Pro — `aarch64-linux-android`, `OpenXR` + `Vulkan`,
    /// packaged as a signed `.apk`.
    Quest,
    /// Apple Vision Pro — `aarch64-apple-visionos`, full-immersive via
    /// `CompositorServices` + `Metal` + `ARKit`, packaged as a visionOS `.app`.
    VisionPro,
}

impl StandalonePlatform {
    /// The Rust target triple cross-builds target for this platform.
    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::Quest => "aarch64-linux-android",
            Self::VisionPro => "aarch64-apple-visionos",
        }
    }

    /// The native-ABI identifier: Android's `jniLibs/` ABI subdirectory for
    /// Quest (`arm64-v8a`), and the Apple architecture slice name for Vision Pro
    /// (`arm64`).
    #[must_use]
    pub const fn abi(self) -> &'static str {
        match self {
            Self::Quest => "arm64-v8a",
            Self::VisionPro => "arm64",
        }
    }

    /// The minimum SDK/API level targeted.
    ///
    /// For Quest this is the Android API level (30 — the oldest API the Quest
    /// `OpenXR` runtime + NDK 29 toolchain support cleanly). For Vision Pro this
    /// is the major visionOS deployment-target version (`1` ⟹ visionOS 1.0).
    #[must_use]
    pub const fn min_sdk(self) -> u32 {
        match self {
            Self::Quest => 30,
            Self::VisionPro => 1,
        }
    }

    /// Short human/label token, matching the cooked manifest `label`
    /// (`CookTarget::quest` / `CookTarget::vision_pro`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quest => "quest",
            Self::VisionPro => "visionos",
        }
    }

    /// Whether this platform runs its XR session over `OpenXR`. True for Quest;
    /// false for Vision Pro, which has no `OpenXR` and uses `CompositorServices`.
    #[must_use]
    pub const fn is_openxr(self) -> bool {
        matches!(self, Self::Quest)
    }

    /// The shared-library file-name prefix for this platform (`lib` on both
    /// Android and Apple).
    #[must_use]
    pub const fn lib_prefix(self) -> &'static str {
        "lib"
    }

    /// The shared-library file extension for this platform (`so` on Android,
    /// `dylib` on visionOS), without the leading dot.
    #[must_use]
    pub const fn lib_extension(self) -> &'static str {
        match self {
            Self::Quest => "so",
            Self::VisionPro => "dylib",
        }
    }

    /// The full, ordered set of native shared-library file names a packaged
    /// standalone runtime must co-locate for this platform.
    ///
    /// This is the **single source of truth** for the standalone staging lists
    /// (B3): the monolithic `OpenUSD` library (`libusd_ms`), Intel TBB
    /// (`libtbb`), every [`MATERIALX_COMPONENTS`] library — `MaterialX` is
    /// **enabled** for standalone, a deliberate divergence from idtx-flow — and,
    /// on Android only, the NDK `libc++_shared.so` (`ANDROID_STL=c++_shared`).
    /// The desktop staging lists in `idtx-build`'s `usd_runtime` module and its
    /// build-time twin mirror this for their own (Windows/macOS/Linux) targets.
    #[must_use]
    pub fn runtime_lib_names(self) -> Vec<String> {
        let (prefix, ext) = (self.lib_prefix(), self.lib_extension());
        let mut names: Vec<String> = CORE_RUNTIME_LIB_STEMS
            .iter()
            .chain(MATERIALX_COMPONENTS.iter())
            .map(|stem| format!("{prefix}{stem}.{ext}"))
            .collect();
        if matches!(self, Self::Quest) {
            names.push(ANDROID_CXX_SHARED_LIB.to_string());
        }
        names
    }
}

// ── Native runtime-library enumerations (B3 — one source of truth) ───────────

/// `MaterialX` shared-library component stems, without the platform `lib` prefix
/// or file extension.
///
/// These are the shader-**generation** components (plus `Core`/`Format`) that
/// the standalone cross-build produces and the runtime links: the
/// `WgslShaderGenerator` fast path pulls `MaterialXGenGlsl`/`GenShader`, and
/// `usdMtlx` pulls `MaterialXCore`/`Format`.
///
/// The desktop Windows build additionally ships the four `MaterialXRender*`
/// libraries (the sample GPU viewer/render helpers — see `idtx-build`'s
/// `WIN_USD_BIN_DLLS` / `xtask`'s `WIN_USD_BIN_DLLS`). The standalone cross-build
/// builds `MaterialX` with `MATERIALX_BUILD_RENDER=OFF` (those libs call
/// `find_package(X11)`, which has no answer on Android/visionOS) and with
/// `PXR_BUILD_IMAGING=OFF` nothing links them, so they are deliberately **not**
/// part of the standalone set. Material fidelity is unaffected — it comes from
/// the generation libs listed here.
pub const MATERIALX_COMPONENTS: &[&str] = &[
    "MaterialXCore",
    "MaterialXFormat",
    "MaterialXGenGlsl",
    "MaterialXGenHw",
    "MaterialXGenMdl",
    "MaterialXGenMsl",
    "MaterialXGenOsl",
    "MaterialXGenShader",
    "MaterialXGenSlang",
];

/// Non-`MaterialX` `OpenUSD` runtime library stems (the monolithic USD library and
/// Intel TBB), without the platform `lib` prefix or extension.
pub const CORE_RUNTIME_LIB_STEMS: &[&str] = &["usd_ms", "tbb"];

/// The Android C++ runtime shared library the NDK ships beside the cross-built
/// USD libraries when `ANDROID_STL=c++_shared`.
pub const ANDROID_CXX_SHARED_LIB: &str = "libc++_shared.so";

/// Directory-name leaf for the coded `OpenExec` plugin (`idtx-usd-exec`) inside a
/// cooked payload / staged beside a cross-built runtime. One source of truth for
/// the xtask runtime cross-builds (which stage `<staging>/usd-exec/`), the
/// packager (which copies it into the bundle), and the runtime (which registers
/// it from `<payload>/usd-exec`). Matches `idtx_build::cook::PAYLOAD_EXEC_DIR` and
/// `idtx_runtime_core`'s mirror. The exec plugin is a PlugRegistry-dlopened
/// **plugin**, not a link-time runtime dep, so it is deliberately NOT part of
/// [`runtime_lib_names`](StandalonePlatform::runtime_lib_names).
pub const EXEC_PLUGIN_DIR_NAME: &str = "usd-exec";

/// Directory-name leaf for the HTTP USD resolver plugin (`idtx-usd-resolver-http`)
/// inside a cooked payload / staged beside a cross-built runtime. One source of
/// truth for the xtask runtime cross-builds (which stage
/// `<staging>/idtx-http-resolver/`), the packager (which copies it into the
/// bundle), and the macOS editor `.app` bundler
/// (`idtx_studio_shell::build_job`, which stages
/// `Contents/Frameworks/idtx-http-resolver/`). Unlike the `OpenExec` plugin, the
/// resolver is also a **forced link-time `DT_NEEDED`** of the runtime cdylib when
/// built with the `remote` feature, so its native library must additionally live
/// in the loader's search path (`jniLibs/arm64-v8a/` on Android), not only be
/// PlugRegistry-discoverable.
pub const HTTP_RESOLVER_PLUGIN_DIR_NAME: &str = "idtx-http-resolver";

/// The Android (Quest) native shared-library file names a packaged runtime must
/// co-locate under `jniLibs/arm64-v8a/`.
#[must_use]
pub fn android_runtime_libs() -> Vec<String> {
    StandalonePlatform::Quest.runtime_lib_names()
}

/// The visionOS (Vision Pro) native dynamic-library file names a packaged `.app`
/// must embed in `Contents/Frameworks/`.
#[must_use]
pub fn visionos_runtime_libs() -> Vec<String> {
    StandalonePlatform::VisionPro.runtime_lib_names()
}

/// The default Android NDK API level targeted for Quest cross-builds.
pub const DEFAULT_ANDROID_API_LEVEL: u32 = 30;
/// The default Android native-ABI directory for Quest (64-bit ARM only).
pub const DEFAULT_ANDROID_ABI: &str = "arm64-v8a";
/// The default visionOS deployment target.
pub const DEFAULT_VISIONOS_DEPLOYMENT_TARGET: &str = "1.0";
/// Environment variable the NDK root is resolved from (mirrors idtx-flow's
/// `scons/platforms/android.py`).
pub const ANDROID_NDK_ROOT_ENV: &str = "ANDROID_NDK_ROOT";

/// Resolved cross-build toolchain configuration for a [`StandalonePlatform`].
///
/// Mirrors idtx-flow's `scons/platforms/android.py` resolution: the NDK root is
/// read from the `ANDROID_NDK_ROOT` environment variable (left `None` when
/// unset — callers decide whether that is fatal), with the API level and ABI
/// defaulting to [`DEFAULT_ANDROID_API_LEVEL`] / [`DEFAULT_ANDROID_ABI`]. The
/// visionOS fields carry the SDK path (resolved via `xcrun` on macOS; `None`
/// here) and the deployment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainConfig {
    /// The platform this toolchain targets.
    pub platform: StandalonePlatform,
    /// Android NDK install root, from `ANDROID_NDK_ROOT`. `None` when unset.
    pub ndk_root: Option<PathBuf>,
    /// Android API level (`ANDROID_PLATFORM=android-<level>`).
    pub android_api_level: u32,
    /// Android native ABI (`ANDROID_ABI`).
    pub android_abi: String,
    /// visionOS SDK path (from `xcrun --sdk xros --show-sdk-path`). `None` here;
    /// resolved on macOS.
    pub visionos_sdk: Option<PathBuf>,
    /// visionOS deployment target (e.g. `"1.0"`).
    pub visionos_deployment_target: String,
}

impl ToolchainConfig {
    /// A Quest toolchain config with NDK root resolved from `ANDROID_NDK_ROOT`
    /// and the default API level + ABI.
    #[must_use]
    pub fn quest() -> Self {
        Self::quest_with(None)
    }

    /// A Quest toolchain config preferring an explicit `ndk` root (typically
    /// from the project's `[build.android].ndk`) over the `ANDROID_NDK_ROOT`
    /// environment variable. A `None` override falls back to the environment.
    #[must_use]
    pub fn quest_with(ndk: Option<PathBuf>) -> Self {
        Self {
            platform: StandalonePlatform::Quest,
            ndk_root: ndk.or_else(|| std::env::var_os(ANDROID_NDK_ROOT_ENV).map(PathBuf::from)),
            android_api_level: DEFAULT_ANDROID_API_LEVEL,
            android_abi: DEFAULT_ANDROID_ABI.to_string(),
            visionos_sdk: None,
            visionos_deployment_target: DEFAULT_VISIONOS_DEPLOYMENT_TARGET.to_string(),
        }
    }

    /// A Vision Pro toolchain config with the default deployment target. The SDK
    /// path is resolved on macOS.
    #[must_use]
    pub fn vision_pro() -> Self {
        Self {
            platform: StandalonePlatform::VisionPro,
            ndk_root: None,
            android_api_level: DEFAULT_ANDROID_API_LEVEL,
            android_abi: DEFAULT_ANDROID_ABI.to_string(),
            visionos_sdk: None,
            visionos_deployment_target: DEFAULT_VISIONOS_DEPLOYMENT_TARGET.to_string(),
        }
    }

    /// A default config for `platform` (reads the environment for Quest).
    #[must_use]
    pub fn for_platform(platform: StandalonePlatform) -> Self {
        match platform {
            StandalonePlatform::Quest => Self::quest(),
            StandalonePlatform::VisionPro => Self::vision_pro(),
        }
    }

    /// Whether the Android NDK root was resolved. Always `false` for the
    /// visionOS config (which does not use the NDK).
    #[must_use]
    pub fn has_ndk(&self) -> bool {
        self.ndk_root.is_some()
    }
}

/// The environment variable `OpenUSD` consults for extra plugin-discovery roots.
pub const PXR_PLUGINPATH_NAME_ENV: &str = "PXR_PLUGINPATH_NAME";

/// A descriptor for the "extract read-only USD plugInfo trees into a writable
/// directory and point `PXR_PLUGINPATH_NAME` at it" operation.
///
/// On a standalone package the `OpenUSD` plugin-discovery trees ride inside a
/// read-only container (an APK's `assets/`, a signed `.app`'s resources) from
/// which the `OpenUSD` plugin registry cannot load. The generalized fix — ported
/// from idtx-flow's `GodotApp.java` — copies those trees into the app's writable
/// files directory before USD initializes and sets `PXR_PLUGINPATH_NAME` to the
/// copied location so the registry finds them.
///
/// This type is the data/contract only (no filesystem or Android syscalls
/// here); the Android runtime entry performs the actual copy +
/// `setenv` from this descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginExtraction {
    /// The read-only source directory of plugInfo trees inside the package
    /// (e.g. the APK `assets/` subtree, or the `.app` resources).
    pub source_assets_dir: PathBuf,
    /// The writable destination directory the trees are copied into (e.g. the
    /// app's internal files dir).
    pub dest_dir: PathBuf,
    /// The environment variable to point at [`Self::dest_dir`]. Always
    /// [`PXR_PLUGINPATH_NAME_ENV`]; carried on the descriptor so the runtime
    /// `setenv` call has a single source of truth.
    pub env_var: &'static str,
}

impl PluginExtraction {
    /// A descriptor copying plugInfo trees from `source_assets_dir` into
    /// `dest_dir`, targeting `PXR_PLUGINPATH_NAME`.
    #[must_use]
    pub fn new(source_assets_dir: impl Into<PathBuf>, dest_dir: impl Into<PathBuf>) -> Self {
        Self {
            source_assets_dir: source_assets_dir.into(),
            dest_dir: dest_dir.into(),
            env_var: PXR_PLUGINPATH_NAME_ENV,
        }
    }

    /// The value `PXR_PLUGINPATH_NAME` should be set to after extraction — the
    /// writable destination directory. The actual `setenv` is the runtime's job.
    #[must_use]
    pub fn plugin_path_value(&self) -> &std::path::Path {
        &self.dest_dir
    }

    /// Recursively copy the plugInfo trees from a **readable source directory**
    /// into [`Self::dest_dir`], returning the value to assign to
    /// `PXR_PLUGINPATH_NAME` (the destination directory).
    ///
    /// This is the pure, portable half of the extraction: once the plugin trees
    /// are reachable as an ordinary directory, the copy + env-value derivation
    /// is identical on every host and is unit-testable without a device. On
    /// Android the APK `assets/` are **not** a real filesystem directory, so the
    /// runtime entry first materializes them from the `ndk` `AssetManager`
    /// (device-gated) and then calls this driver on the materialized tree — or,
    /// where assets are already extractable, calls it on
    /// [`Self::source_assets_dir`] directly.
    ///
    /// Existing files at the destination are overwritten; the destination tree
    /// is created as needed. The returned path is owned so the caller can
    /// `setenv` it without borrowing `self`.
    ///
    /// # Errors
    ///
    /// Propagates any [`std::io::Error`] from creating directories or copying
    /// files (e.g. an unreadable source entry or an unwritable destination).
    pub fn extract_from_dir(&self, source: &std::path::Path) -> std::io::Result<PathBuf> {
        copy_tree_recursive(source, &self.dest_dir)?;
        Ok(self.dest_dir.clone())
    }
}

/// Recursively copy `src` into `dst`, creating `dst` and any parents. Files are
/// overwritten; subdirectories are recursed. Shared by [`PluginExtraction`].
fn copy_tree_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
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

/// A coarse thermal/throttle advisory reported by a [`RuntimeGovernor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThermalState {
    /// Within budget — render at full fidelity.
    #[default]
    Nominal,
    /// Rising — the runtime should begin shedding optional work.
    Elevated,
    /// Hot — the runtime must reduce resolution / effects to stay in budget.
    Critical,
}

/// A minimal frame-budget / thermal contract for a standalone runtime.
///
/// Standalone headsets are thermally and power constrained, so the runtime
/// consults a governor each frame to decide whether to shed optional work. This
/// is a thin placeholder seed (the crate's charter names "runtime-governor
/// contracts"); budgeting and metrics are not implemented here.
pub trait RuntimeGovernor {
    /// The target per-frame budget (e.g. 1/72 s on Quest 3).
    fn frame_budget(&self) -> std::time::Duration;

    /// The current thermal advisory. Defaults to [`ThermalState::Nominal`].
    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_metadata_is_correct() {
        assert_eq!(StandalonePlatform::Quest.triple(), "aarch64-linux-android");
        assert_eq!(
            StandalonePlatform::VisionPro.triple(),
            "aarch64-apple-visionos"
        );
        assert_eq!(StandalonePlatform::Quest.abi(), "arm64-v8a");
        assert_eq!(StandalonePlatform::VisionPro.abi(), "arm64");
        assert_eq!(StandalonePlatform::Quest.min_sdk(), 30);
        assert_eq!(StandalonePlatform::VisionPro.min_sdk(), 1);
        assert_eq!(StandalonePlatform::Quest.label(), "quest");
        assert_eq!(StandalonePlatform::VisionPro.label(), "visionos");
        // Only Quest runs over OpenXR.
        assert!(StandalonePlatform::Quest.is_openxr());
        assert!(!StandalonePlatform::VisionPro.is_openxr());
    }

    #[test]
    fn quest_toolchain_defaults() {
        let cfg = ToolchainConfig::quest();
        assert_eq!(cfg.platform, StandalonePlatform::Quest);
        assert_eq!(cfg.android_api_level, DEFAULT_ANDROID_API_LEVEL);
        assert_eq!(cfg.android_api_level, 30);
        assert_eq!(cfg.android_abi, "arm64-v8a");
        // ndk_root reflects the environment; has_ndk tracks it either way.
        assert_eq!(cfg.has_ndk(), cfg.ndk_root.is_some());
    }

    #[test]
    fn quest_with_prefers_explicit_ndk() {
        let explicit = PathBuf::from("/opt/ndk/27");
        let cfg = ToolchainConfig::quest_with(Some(explicit.clone()));
        assert_eq!(cfg.ndk_root.as_deref(), Some(explicit.as_path()));
        assert!(cfg.has_ndk());
        // A None override falls back to the environment (matches `quest()`).
        assert_eq!(ToolchainConfig::quest_with(None).ndk_root, ToolchainConfig::quest().ndk_root);
    }

    #[test]
    fn vision_pro_toolchain_defaults() {
        let cfg = ToolchainConfig::vision_pro();
        assert_eq!(cfg.platform, StandalonePlatform::VisionPro);
        assert_eq!(cfg.visionos_deployment_target, "1.0");
        assert!(cfg.visionos_sdk.is_none());
        // visionOS does not use the NDK.
        assert!(!cfg.has_ndk());
    }

    #[test]
    fn for_platform_dispatches() {
        assert_eq!(
            ToolchainConfig::for_platform(StandalonePlatform::Quest).platform,
            StandalonePlatform::Quest
        );
        assert_eq!(
            ToolchainConfig::for_platform(StandalonePlatform::VisionPro).platform,
            StandalonePlatform::VisionPro
        );
    }

    #[test]
    fn plugin_extraction_targets_pxr_env() {
        let px = PluginExtraction::new("/apk/assets/usd", "/data/files/usd");
        assert_eq!(px.env_var, "PXR_PLUGINPATH_NAME");
        assert_eq!(px.env_var, PXR_PLUGINPATH_NAME_ENV);
        assert_eq!(
            px.plugin_path_value(),
            std::path::Path::new("/data/files/usd")
        );
        assert_eq!(px.source_assets_dir, PathBuf::from("/apk/assets/usd"));
    }

    #[test]
    fn plugin_extraction_copies_tree_and_returns_env_value() {
        // Unique scratch dir (no tempfile dep), matching the cook test convention.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("idtx-plugextract-{}-{nanos}", std::process::id()));
        let src = root.join("src_assets");
        let dst = root.join("writable/usd");
        // A nested plugInfo tree: usd/plugin/usdGeom/resources/plugInfo.json.
        let nested = src.join("plugin").join("usdGeom").join("resources");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("plugInfo.json"), b"{\"Plugins\":[]}").unwrap();
        std::fs::write(src.join("root.marker"), b"x").unwrap();

        let px = PluginExtraction::new(&src, &dst);
        let env_value = px.extract_from_dir(&src).unwrap();

        // Returns the destination dir (what PXR_PLUGINPATH_NAME is set to).
        assert_eq!(env_value, dst);
        assert_eq!(env_value, px.plugin_path_value());
        // Tree copied faithfully, nested files included.
        assert!(dst.join("root.marker").is_file());
        assert!(
            dst.join("plugin")
                .join("usdGeom")
                .join("resources")
                .join("plugInfo.json")
                .is_file()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn thermal_state_defaults_nominal() {
        assert_eq!(ThermalState::default(), ThermalState::Nominal);
    }

    #[test]
    fn materialx_component_count_matches_desktop() {
        // The 9 shader-generation components the standalone cross-build ships.
        // Render* libs are desktop-only (no X11 on Android/visionOS; unused with
        // imaging off) — see MATERIALX_COMPONENTS.
        assert_eq!(MATERIALX_COMPONENTS.len(), 9);
        assert!(MATERIALX_COMPONENTS.contains(&"MaterialXCore"));
        assert!(MATERIALX_COMPONENTS.contains(&"MaterialXGenMsl"));
        assert!(MATERIALX_COMPONENTS.contains(&"MaterialXGenGlsl"));
        // No render libraries in the standalone set.
        assert!(!MATERIALX_COMPONENTS.iter().any(|c| c.starts_with("MaterialXRender")));
    }

    #[test]
    fn android_runtime_libs_are_so_and_include_cxx_shared() {
        let libs = android_runtime_libs();
        // usd_ms + tbb (2) + 9 MaterialX + libc++_shared = 12.
        assert_eq!(libs.len(), 12);
        assert!(libs.contains(&"libusd_ms.so".to_string()));
        assert!(libs.contains(&"libtbb.so".to_string()));
        assert!(libs.contains(&"libMaterialXCore.so".to_string()));
        assert!(libs.contains(&"libc++_shared.so".to_string()));
        // Every entry is a .so.
        assert!(libs.iter().all(|n| n.ends_with(".so")));
    }

    #[test]
    fn visionos_runtime_libs_are_dylib_without_cxx_shared() {
        let libs = visionos_runtime_libs();
        // usd_ms + tbb (2) + 9 MaterialX = 11 (no libc++_shared on Apple).
        assert_eq!(libs.len(), 11);
        assert!(libs.contains(&"libusd_ms.dylib".to_string()));
        assert!(libs.contains(&"libtbb.dylib".to_string()));
        assert!(libs.contains(&"libMaterialXGenMsl.dylib".to_string()));
        assert!(!libs.iter().any(|n| n.contains("c++_shared")));
        assert!(libs.iter().all(|n| n.ends_with(".dylib")));
    }

    #[test]
    fn lib_decoration_per_platform() {
        assert_eq!(StandalonePlatform::Quest.lib_extension(), "so");
        assert_eq!(StandalonePlatform::VisionPro.lib_extension(), "dylib");
        assert_eq!(StandalonePlatform::Quest.lib_prefix(), "lib");
    }
}
