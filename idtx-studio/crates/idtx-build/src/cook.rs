//! The target-agnostic cook: turn an authored project into a *portable
//! runtime payload*.
//!
//! The cook takes a [`CookPlan`] (resolved from an [`idtx_project::IdtxProject`])
//! and emits, into an output directory:
//!
//! * `stage.usdz` — the entry stage flattened + localized via
//!   [`idtx_usd::package_stage`] (all references and textures pulled inside a
//!   verified USDZ, so the payload is self-contained and relocatable);
//! * `behaviors/*.wasm` — every behavior module discovered under the project's
//!   asset roots, each compiled once (a real wasmtime compile, so a corrupt or
//!   non-wasm file is rejected at cook time rather than at load) and content
//!   hashed;
//! * `run.toml` — a schema-v2 [`RuntimeManifest`] stamped with the experience
//!   entry (stage + default mode + target label), the toolchain / ABI pins the
//!   payload was cooked against, and a **reproducibility hash** over all of the
//!   staged relative paths, bytes and configuration (excluding the hash itself).
//!
//! This is deliberately split from the per-target *packaging* step (the
//! [`Packager`](crate::Packager) trait): the payload here is identical
//! whether it will be wrapped into a macOS `.app`, a Windows folder, or a Linux
//! bundle. Both `cargo xtask` and the Conductor "Package" command call this one
//! library — there is no second copy of the cook logic.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use idtx_runtime_manifest::{
    BehaviorConfig, BuildStamp, ControllerAsset, ExperienceConfig, NetworkConfig, PinsConfig,
    RuntimeManifest, XrConfig,
};
use sha2::{Digest, Sha256};
use wasmtime::{Engine, Module};

/// Resolved dependency versions captured by the build script from Cargo's
/// lockfile edges. Packaging binds the final pins to the selected runtime.
pub const PIN_WGPU: &str = env!("IDTX_PIN_WGPU");
/// Pinned `wasmtime` version (see [`PIN_WGPU`]).
pub const PIN_WASMTIME: &str = env!("IDTX_PIN_WASMTIME");
/// Pinned `glam` version (see [`PIN_WGPU`]).
pub const PIN_GLAM: &str = env!("IDTX_PIN_GLAM");

/// The file name of the flattened entry stage inside a cooked payload.
pub const PAYLOAD_STAGE: &str = "stage.usdz";
/// The subdirectory holding collected behavior modules inside a payload.
pub const PAYLOAD_BEHAVIORS_DIR: &str = "behaviors";
/// The subdirectory holding copied capability assets inside a payload. Laid out
/// so it lands beside the runtime executable as `<exe>/assets/` — the packaged
/// asset search root `RuntimeManifest::asset_search_roots` consults last, so a
/// manifest path like `controllers/quest3_left.usda` resolves against it.
pub const PAYLOAD_ASSETS_DIR: &str = "assets";
/// The runtime manifest file name inside a payload.
pub const PAYLOAD_MANIFEST: &str = "run.toml";
/// The input-map file name inside a payload (logical action → physical source
/// bindings). Staged at the payload root beside [`PAYLOAD_MANIFEST`] so the
/// packaged runtime's `load_input_map` resolves it via its stage-dir fallback —
/// the project tree (and its `project.toml`) is not part of the payload, so a
/// cooked build has no other place to find the bindings. Without it the XR
/// thumbstick / button actions resolve to nothing (no locomotion, snap-turn, or
/// button behaviors). The editor **preview** copies this separately
/// (`preview_assembly::copy_input_map_into`); the cook is the packaged twin.
pub const PAYLOAD_INPUT_MAP: &str = "input_map.toml";
/// The subdirectory holding the IDTX custom-schema plugin resources
/// (`plugInfo.json` + `generatedSchema.usda`) inside a payload. The runtime
/// registers this directory with `PlugRegistry` on startup (see
/// `idtx_usd_extension::install_at`) so `IdtxCollisionAPI` (and future schemas)
/// resolve on a device / another machine, where the compile-time baked
/// `OUT_DIR` plugin dir does not exist. Without it collision never hydrates and
/// the behaviors' collidable-only raycasts find nothing (no gaze / pointer
/// highlight).
pub const PAYLOAD_SCHEMA_DIR: &str = "usd-schemas";
/// The subdirectory holding the **coded** `OpenExec` plugin (`idtx-usd-exec`:
/// datasource + compute-node schemas compiled into a `PlugRegistry` DLL, plus its
/// own `plugInfo.json` + `generatedSchema.usda`) inside a payload. A *separate*
/// dir from [`PAYLOAD_SCHEMA_DIR`] because it is a distinct USD plugin — two
/// plugins cannot share one `plugInfo.json` dir. The runtime registers it with
/// `idtx_usd_exec::install_at` on startup (feature `openexec`) so authored
/// compute graphs resolve their computations off the build host, where the
/// compile-time baked `OUT_DIR` plugin dir does not exist. Only staged for
/// host/desktop targets by [`cook`] — standalone (Quest/visionOS) carries a
/// cross-built native plugin provided by the runtime cross-build + packager.
pub const PAYLOAD_EXEC_DIR: &str = "usd-exec";
/// The subdirectory holding the toolbox's built-in asset tree inside a payload.
/// Staged beside the runtime binary so `@built-in/...@` references (e.g. a
/// built-in player a non-flattened asset still points at) resolve at runtime:
/// the exe directory is placed on the default-resolver search path, and this
/// dir provides `<exe>/built-in/...`. The cook's flatten already localizes the
/// player + rigs into [`PAYLOAD_STAGE`]; this staging is parity/robustness (and
/// matters for the Android/visionOS extract path).
pub const PAYLOAD_BUILTIN_DIR: &str = "built-in";

/// Prefix of the per-run scratch cook-assembly directory name authored under the
/// project root (`.idtx-cook-assembly-<pid>-<nanos>`). The suffix keeps
/// concurrent cooks from colliding, but must not leak into a loose cook's staged
/// output — see [`PAYLOAD_ASSEMBLY_DIR`] and `relocate_scratch_assembly`.
const COOK_ASSEMBLY_SCRATCH_PREFIX: &str = ".idtx-cook-assembly-";

/// Fixed directory name the staged cook assembly is relocated to in a loose cook,
/// replacing the per-run scratch name ([`COOK_ASSEMBLY_SCRATCH_PREFIX`]) so
/// identical cooks emit an identical layout and `run.toml` stage path. One
/// segment under the payload root — the same depth as the scratch dir — so the
/// assembly's downward same-depth payload arcs to the levels stay resolvable.
const PAYLOAD_ASSEMBLY_DIR: &str = ".idtx-assembly";

/// RAII guard that arms the HTTP resolver's "packaging mode" for the duration of
/// a package/flatten step and disarms it on drop (including on an early `?`
/// return or panic). While armed, the resolver reports every `http(s)://` URI as
/// unresolvable so [`idtx_usd::package_stage_keeping_remote`] leaves remote
/// references authored (external) instead of fetching + embedding them.
///
/// The toggle is process-global. The cook arms it across BOTH the remote-ref
/// detection (`stage_has_remote_refs` → `compute_dependencies`) and the package
/// call — each of which acquires the exclusive stage-lifecycle lock *inside the
/// shim for its own duration*, then releases it. The flag therefore stays armed
/// across the Rust-side gap between those two native calls, during which no
/// lifecycle lock is held: a concurrent in-process read-projection of a stage
/// with remote refs could observe packaging mode in that window and treat those
/// assets as unresolvable.
///
/// KNOWN LIMITATION (deferred): closing this fully requires holding the exclusive
/// lifecycle lock across detection + packaging as one continuous operation — a
/// native shim change (the lock is C++-internal and non-recursive, so it cannot
/// be bracketed around the two separate calls from Rust). The window is narrow
/// and the symptom transient (a remote asset momentarily reported unresolvable to
/// a racing resolver read), so it is left as-is for now; cooking while actively
/// resolving remote refs in the same process is the only trigger.
///
/// Only present when the resolver is linked (`usd-resolver-http`); otherwise the
/// resolver is not installed in the process and remote refs are unresolvable
/// anyway, so no toggle is needed.
#[cfg(feature = "usd-resolver-http")]
struct PackagingModeGuard;

#[cfg(feature = "usd-resolver-http")]
impl PackagingModeGuard {
    fn arm() -> Self {
        idtx_usd_resolver_http::set_packaging_mode(true);
        Self
    }
}

#[cfg(feature = "usd-resolver-http")]
impl Drop for PackagingModeGuard {
    fn drop(&mut self) {
        idtx_usd_resolver_http::set_packaging_mode(false);
    }
}

/// Errors from the cook pipeline.
#[derive(Debug, thiserror::Error)]
pub enum CookError {
    #[error("runtime build metadata: {0}")]
    RuntimeMetadata(String),
    /// The project declares no entry stage: neither a configured `default_stage`
    /// nor any registered `levels` to compose into an assembly.
    #[error("project has no entry stage (set default_stage or register a level)")]
    NoEntryStage,
    /// The resolved entry stage does not exist on disk.
    #[error("entry stage does not exist: {}", idtx_project::normalize_display_path(.0.as_path()))]
    MissingEntryStage(PathBuf),
    /// A filesystem operation failed.
    #[error("cook I/O at {}: {source}", idtx_project::normalize_display_path(.path.as_path()))]
    Io {
        /// The path the operation was on.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// Flattening / localizing the stage into a USDZ failed.
    #[error("stage packaging failed: {0}")]
    Package(#[from] idtx_usd::StageError),
    /// A `.wasm` behavior module failed to compile (corrupt / not-wasm / a wasm
    /// feature the runtime engine does not enable).
    #[error("behavior module {} failed to compile: {source}", idtx_project::normalize_display_path(.path.as_path()))]
    Behavior {
        /// The offending module path.
        path: PathBuf,
        /// The wasmtime compile error.
        source: wasmtime::Error,
    },
    /// The per-level behavior-prepare callback ([`CookPlan::level_behavior_prepare`])
    /// failed: compiling a level's authored behavior sources, or authoring the
    /// `idtx:behavior:module` overlay onto the composed per-level assembly, did not
    /// succeed. Fail-closed — a level is never flattened with stale / missing
    /// behavior bindings.
    #[error("behavior preparation for a level failed: {0}")]
    BehaviorPrepare(String),
    /// The project's `xr_controllers.toml` sidecar could not be loaded/parsed.
    #[error("xr controllers sidecar: {0}")]
    XrControllers(String),
    /// Assembling the final `.apk` from the staging tree failed (Quest).
    #[error("apk assembly failed: {0}")]
    Apk(#[from] crate::apk::ApkError),
    /// Finishing / signing the visionOS `.app` for a device failed (Vision Pro).
    #[error("visionOS .app assembly failed: {0}")]
    VisionOs(#[from] crate::visionos_assemble::VisionOsAssembleError),
    /// The runtime binary handed to a native packager is the wrong format or
    /// architecture — e.g. a Windows PE (`MZ`) `.exe` or a non-aarch64 ELF given
    /// to the Android packager. Fails closed so a host binary can never be staged
    /// into `jniLibs/arm64-v8a/` (the field regression that shipped a Windows
    /// `idtx-runtime.exe` inside the Quest APK).
    #[error("runtime binary {path} is not a valid {expected} ({detail})")]
    InvalidRuntimeBinary {
        /// The offending runtime binary path.
        path: PathBuf,
        /// What was required (e.g. `"aarch64 ELF shared object"`).
        expected: String,
        /// Why it was rejected (magic bytes / machine mismatch).
        detail: String,
    },
    /// The runtime binary carries a forced load command on the coded `OpenExec`
    /// plugin (`libidtx_usd_exec.{dylib,so}` — emitted by `apps/idtx-runtime/build.rs`
    /// only under the `openexec` feature), but the cross-built plugin was not staged
    /// into the bundle. dyld / the Android loader resolves that dependency **at
    /// launch**, so packaging would silently produce an app that crashes before
    /// `main` ("Library not loaded: `@rpath/libidtx_usd_exec.dylib`"). Fails closed
    /// instead — the plugin must be staged beside the runtime (`cargo xtask
    /// build-{quest,visionos}-runtime --with-openexec`).
    #[error(
        "runtime {runtime} requires the coded OpenExec plugin ({plugin}) but it was \
         not staged into the bundle — the runtime was built with the `openexec` \
         feature (forced load command present), so the app would crash at launch. \
         Re-run `cargo xtask build-{{quest,visionos}}-runtime --with-openexec` to \
         stage the cross-built plugin beside the runtime."
    )]
    MissingExecPlugin {
        /// The runtime binary that forces the `OpenExec` load.
        runtime: PathBuf,
        /// The expected plugin library file name.
        plugin: String,
    },
    /// The cooked macOS runtime binary carries a forced load command on the HTTP
    /// resolver plugin (`@rpath/libidtx_http_resolver.dylib`, pulled in by the
    /// `remote` / `usd-resolver-http` feature) but it could not be staged into the
    /// `.app` — the resolver dylib was not found beside the editor, or an
    /// `install_name_tool` rpath fixup failed. Fails closed: dyld resolves that
    /// load **at launch**, so shipping without it produces a bundle that crashes
    /// before `main` ("Library not loaded: `@rpath/libidtx_http_resolver.dylib`").
    #[error("macOS HTTP resolver staging failed: {0}")]
    HttpResolverStaging(String),
    /// The cross-built runtime binary carries a forced load command on the HTTP
    /// resolver plugin (`DT_NEEDED libidtx_http_resolver.so`, pulled in by the
    /// `remote` / `usd-resolver-http` feature) but the resolver library was not
    /// staged into the APK native-lib dir (`jniLibs/arm64-v8a/`). The Android
    /// loader resolves that dependency **at launch**, so packaging would silently
    /// produce an APK that crashes before `main` with a
    /// `java.lang.UnsatisfiedLinkError` (`dlopen failed: library
    /// "libidtx_http_resolver.so" not found: needed by libidtx_runtime.so`). Fails
    /// closed — re-run `cargo xtask build-quest-runtime --with-remote` so the
    /// cross-built resolver is staged beside the runtime.
    #[error(
        "runtime {runtime} requires the HTTP resolver plugin ({plugin}) but it was \
         not staged into the APK native-lib dir — the runtime was built with the \
         `remote` feature (forced DT_NEEDED present), so the app would crash at launch \
         with UnsatisfiedLinkError. Re-run `cargo xtask build-quest-runtime --with-remote` \
         to stage the cross-built resolver beside the runtime."
    )]
    MissingResolverPlugin {
        /// The runtime binary that forces the HTTP resolver load.
        runtime: PathBuf,
        /// The expected resolver library file name.
        plugin: String,
    },
}

/// A cook target profile: the concrete platform + capability set a payload is
/// cooked for. `features` are runtime requirements; the packager checks them
/// against the selected binary's embedded effective features.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookTarget {
    /// Rust target triple (e.g. `"x86_64-pc-windows-msvc"`).
    pub triple: String,
    /// Short profile label stamped into the manifest (`"windows-pcvr"`, …).
    pub label: String,
    /// Default run mode token for the packaged experience (`"xr"` / `"desktop"`).
    pub mode: String,
    /// Whether a packaged desktop player should run borderless-fullscreen and
    /// capture the mouse on launch (ESC quits). Stamped into the manifest's
    /// `[experience].fullscreen`. `true` for the desktop profile, `false` for XR
    /// profiles (the compositor owns presentation there).
    pub fullscreen: bool,
    /// Required runtime Cargo features (e.g. `["pcvr"]`).
    pub features: Vec<String>,
}

impl CookTarget {
    /// A desktop (non-XR) profile for `triple`.
    #[must_use]
    pub fn desktop(triple: impl Into<String>) -> Self {
        Self {
            triple: triple.into(),
            label: "desktop".to_string(),
            mode: "desktop".to_string(),
            fullscreen: true,
            features: Vec::new(),
        }
    }

    /// A PCVR profile for `triple` (enables the `pcvr` runtime feature, default
    /// run mode `xr`).
    #[must_use]
    pub fn pcvr(triple: impl Into<String>) -> Self {
        Self {
            triple: triple.into(),
            label: "pcvr".to_string(),
            mode: "xr".to_string(),
            fullscreen: false,
            features: vec!["pcvr".to_string()],
        }
    }

    /// A Linux PCVR profile (scaffold). Same cook output as [`pcvr`], but
    /// stamped `linux-pcvr` so a packager can select the SteamVR/Monado loader
    /// path and emit the `SteamVR` action manifest (see [`crate::steamvr`]). The
    /// Linux `OpenXR` session itself (Vulkan) is not yet implemented.
    ///
    /// [`pcvr`]: CookTarget::pcvr
    #[must_use]
    pub fn pcvr_linux(triple: impl Into<String>) -> Self {
        Self {
            triple: triple.into(),
            label: "linux-pcvr".to_string(),
            mode: "xr".to_string(),
            fullscreen: false,
            features: vec!["pcvr".to_string()],
        }
    }

    /// A standalone Android/Quest profile for `triple` (typically
    /// `aarch64-linux-android`). Mirrors [`pcvr_linux`]: the cook payload is the
    /// portable `stage.usdz`; only the stamped `label`/`mode` and the runtime
    /// feature set differ. Requires the `quest` runtime feature
    /// (`OpenXR` + Vulkan on-device). Packaging the `.apk` is deferred to
    /// `xtask`/CI (foreign triple).
    ///
    /// [`pcvr_linux`]: CookTarget::pcvr_linux
    #[must_use]
    pub fn quest(triple: impl Into<String>) -> Self {
        Self {
            triple: triple.into(),
            label: "quest".to_string(),
            mode: "xr".to_string(),
            fullscreen: false,
            features: vec!["quest".to_string()],
        }
    }

    /// A standalone Apple Vision Pro profile for `triple` (typically
    /// `aarch64-apple-visionos`). Mirrors [`quest`]: identical portable cook
    /// payload, distinct `label`/feature set. Requires `visionos-immersive`
    /// (`CompositorServices` + Metal + `ARKit`).
    /// Packaging the `.app` is deferred to `xtask`/CI and runs on macOS only.
    ///
    /// [`quest`]: CookTarget::quest
    #[must_use]
    pub fn vision_pro(triple: impl Into<String>) -> Self {
        Self {
            triple: triple.into(),
            label: "visionos".to_string(),
            mode: "xr".to_string(),
            fullscreen: false,
            features: vec!["visionos-immersive".to_string()],
        }
    }

    /// Whether this target's packaging can consume a LOOSE cook — a directory
    /// tree plus a non-`stage.usdz` entry — rather than a single flattened
    /// `stage.usdz`.
    ///
    /// Standalone bundled targets (Quest `.apk`, Vision Pro `.app`) copy a single
    /// stage resource into the app archive (the device shells hard-code
    /// `stage.usdz`), so they cannot bundle a loose tree yet. A loose request is
    /// force-flattened for them (see [`cook`]). Non-bundled targets (desktop /
    /// PCVR) run from the cooked `out_dir` directly and support loose staging.
    #[must_use]
    pub fn supports_loose_levels(&self) -> bool {
        !matches!(self.label.as_str(), "quest" | "visionos")
    }
}

/// A validated behavior module collected into the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorArtifact {
    /// Absolute source path of the discovered `.wasm` module.
    pub source: PathBuf,
    /// Path inside the payload, relative to its root (`behaviors/<file>.wasm`).
    pub payload_rel: PathBuf,
    /// Hex SHA-256 of the module bytes.
    pub hash: String,
    /// Lifecycle export names the module carries (`_idtx_on_*`), sorted.
    pub hooks: Vec<String>,
    /// Whether the module carries the `_idtx_abi_version` export marker. A module
    /// without it can never pass the runtime's ABI probe, so its absence is a
    /// strong signal the file is not a real IDTX behavior.
    pub has_abi_marker: bool,
}

/// How a project's player stage is named when the assembly composes it as a
/// reference at `/World/Player`.
///
/// Defined here (not in the `cooking`-gated [`crate::assembly`] module) so
/// [`CookPlan`] can carry it even when `cooking` is off. The asset path for the
/// reference is either relativized against the assembly dir (a project-local
/// stage) or a verbatim `built-in/...` search-path arc (the toolbox built-in,
/// which lives outside the project and so must not be relativized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerRef {
    /// A project-local player stage (absolute path), relativized against the
    /// assembly dir when authored.
    Project(PathBuf),
    /// The toolbox built-in player, authored verbatim as a `built-in/...` arc
    /// (resolved via the process-global default-resolver search path).
    Builtin(String),
}

/// The result of the per-level behavior-prepare callback
/// ([`CookPlan::level_behavior_prepare`]): the behavior-overlaid per-level
/// assembly to flatten into `levels/<id>/level.usdz`, plus the compiled `.wasm`
/// modules to co-locate beside it at `levels/<id>/behaviors/`.
///
/// The callback authors `idtx:behavior:module = ./behaviors/<name>.wasm` on each
/// behavior prim of the composed assembly (layer-relative to the assembly dir);
/// [`idtx_usd::package_stage`] preserves that relative value through the flatten,
/// so the runtime resolves the module against the flattened `level.usdz`'s own
/// directory — which is exactly where [`cook`] copies the `.wasm`.
#[derive(Debug, Clone)]
pub struct LevelBehaviorOverlay {
    /// The behavior-overlaid assembly stage [`cook`] flattens into the level's
    /// `level.usdz` (in place of the bare composed assembly).
    pub overlaid_assembly: PathBuf,
    /// The compiled `.wasm` modules to co-locate at `levels/<id>/behaviors/`.
    pub wasm: Vec<PathBuf>,
}

/// The signature of the per-level behavior-prepare hook: given the path to a
/// freshly composed per-level assembly, compile + bind that level's behaviors and
/// return the overlaid stage + compiled modules (`Ok(None)` when the level has no
/// behavior sources — [`cook`] then flattens the bare assembly).
pub type LevelBehaviorPrepareFn =
    dyn Fn(&Path) -> Result<Option<LevelBehaviorOverlay>, String> + Send + Sync;

/// A cloneable, debug-printable wrapper around a [`LevelBehaviorPrepareFn`] so it
/// can ride on [`CookPlan`] (which derives `Debug`/`Clone`) without `idtx-build`
/// depending on the shell that supplies the closure. The shell's `build_job`
/// constructs one; a plain `idtx-build` / `cargo xtask` cook leaves it unset and
/// the per-level flatten binds no behaviors.
#[derive(Clone)]
pub struct LevelBehaviorPrepare(Arc<LevelBehaviorPrepareFn>);

impl LevelBehaviorPrepare {
    /// Wrap a behavior-prepare closure.
    #[must_use]
    pub fn new(f: Arc<LevelBehaviorPrepareFn>) -> Self {
        Self(f)
    }

    /// Invoke the hook on a composed per-level assembly.
    ///
    /// # Errors
    /// Propagates the closure's error string (surfaced as
    /// [`CookError::BehaviorPrepare`] by the caller).
    pub fn call(&self, assembly: &Path) -> Result<Option<LevelBehaviorOverlay>, String> {
        (self.0)(assembly)
    }
}

impl std::fmt::Debug for LevelBehaviorPrepare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LevelBehaviorPrepare(..)")
    }
}

/// The resolved plan for a cook. Build with [`CookPlan::from_project`].
#[derive(Debug, Clone)]
pub struct CookPlan {
    /// Absolute entry stage used by the non-`cooking` flatten path (and as the
    /// single-level fallback when a `cooking` build has no registered levels):
    /// the `default_stage`, else the first registered level.
    pub entry_stage: PathBuf,
    /// Experience display name.
    pub name: String,
    /// Absolute asset search roots to sweep for behavior modules.
    pub asset_roots: Vec<PathBuf>,
    /// The target profile.
    pub target: CookTarget,
    /// Extra cargo features from the project's `[build]` config, merged with the
    /// target features into the cooked pin set.
    pub extra_features: Vec<String>,
    /// The effective XR controller config to bake into the payload: the manifest
    /// default (`generic` device) overlaid with the project's `xr_controllers.toml`
    /// sidecar entries. The cook copies each declared mesh into the payload's
    /// `assets/controllers/` and rewrites these paths to payload-relative.
    pub xr: XrConfig,
    /// Output payload directory, replaced only after a successful staged cook.
    pub out_dir: PathBuf,
    /// The project root, used to anchor the scratch dir that
    /// [`author_cook_assembly`] authors the assembly stage into (so its payload
    /// arcs are depth-stable → the reproducibility hash is machine-independent).
    pub project_root: PathBuf,
    /// Absolute registered level stages, in registry order — the payload holders
    /// the `cooking` assembly composes. Empty falls back to [`entry_stage`].
    pub levels: Vec<PathBuf>,
    /// The configured default level (absolute), the **only** level authored
    /// visible in the cooked *combined* assembly (the single-level / entry path).
    /// `None` → the first level is visible.
    pub default_level: Option<PathBuf>,
    /// The level the experience **starts on** (absolute) in the per-level-assembly
    /// cook mode (multiple registered levels). `None` falls back to
    /// [`default_level`](Self::default_level), then the first level.
    pub start_level: Option<PathBuf>,
    /// The transfer/transition level (absolute) cooked as its own per-level
    /// assembly and shown while the next level loads. `None` = no masking scene.
    pub transfer_level: Option<PathBuf>,
    /// Shared global sublayers (absolute) composed as root `subLayer`s into every
    /// per-level assembly (prototypes/definitions shared without copying). Empty
    /// by default.
    pub shared_sublayers: Vec<PathBuf>,
    /// The player stage composed as a reference at `/World/Player` (a project
    /// stage, or the toolbox built-in). `None` composes no player.
    pub player: Option<PlayerRef>,
    /// When `Some`, `cook()` flattens exactly this stage and skips assembly
    /// generation — the seam the shell `build_job` uses to hand `cook()` the
    /// behavior-overlaid assembly it composed. `None` → `cook()` composes the
    /// assembly itself (under `cooking`) or flattens [`entry_stage`].
    pub entry_override: Option<PathBuf>,
    /// Stage the entry's dependency closure as a LOOSE directory tree
    /// ([`idtx_usd::stage_loose_closure`]) instead of flattening it into one
    /// self-contained `stage.usdz` ([`idtx_usd::package_stage`]). Keeps large
    /// referenced scenes as their original loose files (levels referenced via
    /// payloads, loaded on demand), preserving instancing byte-for-byte and
    /// avoiding the in-package read penalty. Requires the closure to live under a
    /// single common root (a project tree). Default `false` → unchanged `.usdz`
    /// packaging.
    pub loose_levels: bool,
    /// Network capability from the project's `[network]` section — stamped
    /// verbatim into the emitted `run.toml` so the packaged runtime enforces the
    /// same allow-list as the project author configured.
    pub network: NetworkConfig,
    /// Behavior host per-instance work-budget overrides from the project's
    /// `[behavior.limits]` section — stamped into the emitted `run.toml` so the
    /// packaged runtime applies the same caps the author configured.
    pub behavior: BehaviorConfig,
    /// ⚠️ **TEMPORARY / SECURITY RISK.** When `Some`, a bearer token is stamped
    /// into the cooked `run.toml` (`[network].dev_bearer_token`) so a packaged
    /// device runtime can authenticate remote USD fetches without a keychain.
    /// Embedding credentials in a build artifact is unsafe — opt-in only, never
    /// the default, and such a build must not be distributed. The caller (the
    /// Conductor build job, which holds the logged-in auth) supplies the token
    /// only when the user explicitly requests it. Remove when the interactive
    /// logon-level flow lands — see
    /// `docs/plan_remote_refs_external_and_runtime_auth.md`.
    pub embed_dev_token: Option<String>,
    /// Texture-decode profile token stamped into the cooked `run.toml`
    /// (`[experience].texture_quality`), governing how material/controller
    /// textures are resolution-capped and mip-generated on the target device.
    /// Resolved by [`CookPlan::from_project`] from the project's per-target
    /// `[build.android]` / `[build.visionos].texture_quality`, defaulting to
    /// `"authoring"` for the standalone-device targets (Quest / Vision Pro) so a
    /// large 4K texture is capped to fit the runtime's per-prefetch upload budget
    /// instead of falling back to a white null texture. `None` leaves the key
    /// unstamped → the runtime default (`full_resolution`), used for desktop /
    /// PCVR where memory is ample.
    pub texture_quality: Option<String>,
    /// Per-level behavior-prepare hook (build-path integration). When set and
    /// the project is multi-level (so [`cook`] takes the per-level-assembly path),
    /// [`cook`] invokes it on each composed per-level assembly to compile + bind
    /// that level's behaviors before flattening. `None` (the default, and every
    /// `cargo xtask` cook) flattens each per-level assembly with no behaviors
    /// bound. `idtx-build` never depends on the shell that authors behaviors — the
    /// shell's `build_job` supplies this closure (it owns SDK discovery + the
    /// module-overlay authoring). Ignored on the single-/zero-level combined path.
    pub level_behavior_prepare: Option<LevelBehaviorPrepare>,
    /// Registered source templates compiled by the shared build pipeline.
    pub runtime_prototypes: std::collections::BTreeMap<String, idtx_project::RuntimePrototypeConfig>,
    /// Explicit selected guest SDK, otherwise resolved from environment/bundle.
    pub behavior_sdk: Option<PathBuf>,
}

impl CookPlan {
    /// Resolve a cook plan from a loaded project, a target, and an output dir.
    ///
    /// # Errors
    ///
    /// [`CookError::NoEntryStage`] if the project declares neither a default
    /// stage nor any registered level; [`CookError::MissingEntryStage`] if the
    /// resolved entry stage does not exist on disk; [`CookError::XrControllers`]
    /// if the project's `xr_controllers.toml` sidecar is present but cannot be
    /// parsed.
    pub fn from_project(
        project: &idtx_project::IdtxProject,
        target: CookTarget,
        out_dir: impl Into<PathBuf>,
    ) -> Result<Self, CookError> {
        let default_level = project.default_stage_abs();
        let levels = project.levels_abs();
        // The non-cooking flatten target (and single-level fallback for a
        // cooking build with no registered levels): the default stage, else the
        // first registered level. `assembly_stage` is no longer an entry source —
        // the cook composes its own assembly.
        let entry_stage = default_level
            .clone()
            .or_else(|| levels.first().cloned())
            .ok_or(CookError::NoEntryStage)?;
        if !entry_stage.is_file() {
            return Err(CookError::MissingEntryStage(entry_stage));
        }
        // The player stage: the project-configured stage, or the toolbox's
        // built-in player (authored as a verbatim `built-in/...` search arc) when
        // unset. One stage carries both desktop + XR rigs via a `playerKind`
        // variantSet, so the cook no longer selects a per-mode player file.
        let player = Some(match project.player_stage() {
            idtx_project::PlayerStage::Project(abs) => PlayerRef::Project(abs),
            idtx_project::PlayerStage::BuiltinDefault => {
                PlayerRef::Builtin(idtx_assets::BUILTIN_PLAYER_STAGE_REF.to_owned())
            }
        });
        // Base on the manifest default (the conventional `generic` device meshes)
        // so a project that merely drops files at `controllers/generic_*` cooks
        // without authoring a sidecar; the sidecar's per-device entries overlay it.
        let mut xr = RuntimeManifest::default().xr;
        if let Some(sidecar) = project
            .load_xr_controllers()
            .map_err(|e| CookError::XrControllers(e.to_string()))?
        {
            xr.controllers.extend(sidecar.controllers);
        }
        // Texture-decode profile per target: the project's per-target override
        // ([build.android] / [build.visionos].texture_quality) when set, else a
        // sensible per-target default — the standalone-device targets (Quest /
        // Vision Pro) default to `authoring` (2048 cap) so large textures fit the
        // runtime upload budget; desktop / PCVR leave it unstamped (runtime
        // full-resolution default). Non-device targets ignore any stray override.
        let build = &project.manifest().build;
        let texture_quality = match target.label.as_str() {
            "quest" => Some(build.android.texture_quality),
            "visionos" => Some(build.visionos.texture_quality),
            _ => None,
        }
        .map(|override_q| {
            override_q
                .unwrap_or(idtx_project::TextureQuality::Authoring)
                .as_token()
                .to_owned()
        });
        Ok(Self {
            entry_stage,
            name: project.name(),
            asset_roots: project.asset_search_dirs_abs(),
            target,
            extra_features: project.manifest().build.extra_features.clone(),
            xr,
            out_dir: out_dir.into(),
            project_root: project.root().to_path_buf(),
            levels,
            default_level,
            start_level: project.start_level_abs(),
            transfer_level: project.transfer_level_abs(),
            shared_sublayers: project.shared_sublayers_abs(),
            player,
            entry_override: None,
            loose_levels: project.manifest().build.loose_levels,
            network: {
                let n = &project.manifest().network;
                NetworkConfig {
                    allow: n.allow,
                    allowed_hosts: n.allowed_hosts.clone(),
                    private_hosts: n.private_hosts.clone(),
                    auth_origins: n.auth_origins.clone(),
                    max_response_mb: n.max_response_mb,
                    max_asset_mb: n.max_asset_mb,
                    dev_bearer_token: String::new(),
                }
            },
            behavior: {
                let l = &project.manifest().behavior.limits;
                BehaviorConfig {
                    limits: idtx_runtime_manifest::BehaviorLimitsConfig {
                        queue_bytes: l.queue_bytes,
                        http_response_bytes: l.http_response_bytes,
                        http_inflight: l.http_inflight,
                    },
                }
            },
            // Opt-in only; the Conductor build job sets this from the logged-in
            // auth when the user explicitly requests an embedded dev token.
            embed_dev_token: None,
            texture_quality,
            // Supplied by the shell's build_job for a multi-level build; unset for
            // a plain `idtx-build` / `cargo xtask` cook.
            level_behavior_prepare: None,
            runtime_prototypes: project.manifest().runtime_prototypes.clone(),
            behavior_sdk: None,
        })
    }

    /// Required target/project runtime features, sorted and deduplicated. The
    /// final manifest's effective `features` come from the selected runtime.
    #[must_use]
    pub fn cooked_features(&self) -> Vec<String> {
        let mut features: Vec<String> = self
            .target
            .features
            .iter()
            .chain(self.extra_features.iter())
            .cloned()
            .collect();
        // A project that enables network egress (`[network] allow = true`) can only
        // function on a runtime built with the `remote` feature: that feature gates
        // BOTH the behavior `net` HTTP import (`idtx_http_request[_async]`,
        // `#[cfg(feature = "net")]` in idtx-behavior) AND the only call that lifts the
        // SSRF policy off deny-by-default (`StudioSession::set_exec_network_policy`,
        // `#[cfg(feature = "remote")]`). Derive it as a REQUIRED feature here so it
        // flows into `manifest.pins.required_features` (see `cook_staged`) and the
        // packager (`package::stage_payload`) FAILS CLOSED on any selected runtime
        // binary whose embedded metadata lacks `remote` — instead of silently shipping
        // a player that can never honor the `[network]` allowlist / `auth_origins` the
        // author configured. Without this, a behavior HTTP/login call is permanently
        // capability-denied (or the policy stays `denied()`) at runtime, with no
        // build-time signal. Egress disabled → no `remote` requirement is added.
        if self.network.allow {
            features.push("remote".to_string());
        }
        features.sort();
        features.dedup();
        features
    }
}

/// The result of a successful cook.
#[derive(Debug, Clone)]
pub struct CookReport {
    /// The payload output directory.
    pub out_dir: PathBuf,
    /// Absolute path of the flattened stage (`<out_dir>/stage.usdz`).
    pub stage: PathBuf,
    /// Absolute path of the emitted manifest (`<out_dir>/run.toml`).
    pub manifest: PathBuf,
    /// Collected + validated behavior modules.
    pub behaviors: Vec<BehaviorArtifact>,
    /// Payload-relative paths (`controllers/<file>`) of the controller mesh
    /// assets copied into `assets/`, sorted. Empty when the project declares no
    /// resolvable controller meshes.
    pub controllers: Vec<PathBuf>,
    /// Unresolved references reported by the stage localization (empty is ideal;
    /// a non-empty list is surfaced but does not fail the cook, mirroring how
    /// `idtx-usd` tolerates pre-existing unresolved refs on save).
    pub unresolved: Vec<String>,
    /// Remote (`http(s)://`) references deliberately kept EXTERNAL — left
    /// authored in the payload, never fetched or embedded. The packaged runtime
    /// resolves these at play time via the HTTP resolver.
    pub external: Vec<String>,
    /// The reproducibility hash baked into the manifest.
    pub content_hash: String,
    /// The behavior ABI version the payload was cooked against.
    pub abi_version: u32,
}

/// Cook `plan` into a portable runtime payload.
///
/// # Errors
///
/// Returns [`CookError`] if the output directory cannot be created, the stage
/// cannot be flattened/localized, or a discovered `.wasm` module fails to
/// compile.
pub fn cook(plan: &CookPlan) -> Result<CookReport, CookError> {
    let publication = crate::publication::Publication::new(&plan.out_dir)?;
    let mut staged = plan.clone();
    staged.out_dir = publication.path();
    let destination = publication.destination().to_owned();
    if plan.project_root.starts_with(&destination) || plan.entry_stage.starts_with(&destination) {
        return Err(CookError::Io { path: destination, source: std::io::Error::other("output overlaps project source") });
    }
    let mut report = cook_staged(&staged, &[destination, staged.out_dir.clone()])?;
    let stage_rel = report.stage.strip_prefix(&staged.out_dir).map_err(|source| CookError::Io {
        path: report.stage.clone(), source: std::io::Error::other(source),
    })?.to_owned();
    publication.publish()?;
    report.out_dir.clone_from(&plan.out_dir);
    report.stage = plan.out_dir.join(stage_rel);
    report.manifest = plan.out_dir.join(PAYLOAD_MANIFEST);
    Ok(report)
}

/// Fail-closed guard between a stage packager and the post-package steps that
/// read the produced `.usdz`.
///
/// A packager can return `Ok` while writing NO file: the plain
/// [`idtx_usd::package_stage`] early-returns with an empty `destination` and a
/// populated `unresolved` list when the dependency closure has genuinely
/// unresolved assets, rather than emitting a partial package. Without this guard
/// the very next step ([`crate::usdz_metadata::normalize`]) opens the nonexistent
/// archive and fails with a cryptic [`CookError::Io`] ("No such file or
/// directory") that names the missing *output* instead of the real cause.
///
/// Convert that into a clear [`CookError::Package`] naming the unresolved assets
/// (mirrors `StageService::package_loose_as_usdz`). The destination-existence
/// check is a belt-and-braces backstop for any future packager that writes
/// nothing without reporting it through `unresolved`.
fn guard_packaged(destination: &Path, package: &idtx_usd::PackageResult) -> Result<(), CookError> {
    // Surface any packager diagnostics (e.g. a design-time behavior-source `.rs`
    // dropped from the closure, or a dangling arc in the finished package) so an
    // authoring mistake stays visible in the build log instead of being silently
    // swallowed. Carried out of the packager on `PackageResult::warnings`.
    for warning in &package.warnings {
        tracing::warn!(stage = %destination.display(), "stage packaging: {warning}");
    }
    if !package.unresolved.is_empty() {
        return Err(CookError::Package(idtx_usd::StageError::UnresolvedAssets(
            package.unresolved.clone(),
        )));
    }
    if !destination.is_file() {
        return Err(CookError::Io {
            path: destination.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "stage packager reported success but produced no output file",
            ),
        });
    }
    Ok(())
}

fn cook_staged(plan: &CookPlan, excluded: &[PathBuf]) -> Result<CookReport, CookError> {
    let behaviors_dir = plan.out_dir.join(PAYLOAD_BEHAVIORS_DIR);
    create_dir(&plan.out_dir)?;
    create_dir(&behaviors_dir)?;

    // Anchor the USD default-resolver search path at the project's asset roots
    // plus the toolbox's built-in tree, so `@built-in/...@` (the built-in player)
    // and project-relative references resolve while composing the assembly and
    // flattening it. Process-global and idempotent: the editor sets the same
    // path on project activation, but a standalone `cargo xtask` cook has none,
    // so set it here too. Non-fatal — a missing resolver only drops references.
    {
        let mut dirs = plan.asset_roots.clone();
        dirs.push(idtx_assets::builtin_root());
        idtx_usd::set_default_asset_search_path(&dirs)?;
    }

    // 1. Flatten + localize the entry stage into a verified, self-contained USDZ.
    //    Flatten target, in precedence order:
    //      1. `plan.entry_override` — the caller already composed (and possibly
    //         behavior-overlaid) a stage; flatten exactly that (the shell path).
    //      2. (cooking) a freshly composed default-visible assembly, deleted after
    //         flatten — its `Drop` removes the project-anchored scratch dir.
    //      3. the resolved `entry_stage` — a plain `idtx-build` with no cooking.
    let stage_out = plan.out_dir.join(PAYLOAD_STAGE);

    #[cfg(feature = "cook-metrics")]
    let compose_flatten_start = std::time::Instant::now();

    // Per-level-assembly cook mode (Scenario 2 — the committed level-transition
    // design): a multi-level experience cooks each level as its own self-contained
    // `.usdz` body + a thin loose per-level assembly that composes the shared
    // player/globals, instead of one combined assembly flattened to `stage.usdz`.
    // The shell's pre-composed `entry_override` and single-/zero-level projects
    // keep the combined-assembly path. Only meaningful under `cooking` (the
    // per-level composer lives there); `false` otherwise.
    #[cfg(feature = "cooking")]
    let per_level = plan.entry_override.is_none() && plan.levels.len() > 1;
    #[cfg(not(feature = "cooking"))]
    let per_level = false;

    #[cfg(feature = "cooking")]
    let cook_assembly = match &plan.entry_override {
        // Per-level mode authors its own per-level assemblies below; skip the
        // combined-assembly composition entirely.
        _ if per_level => None,
        Some(_) => None,
        None => Some(author_cook_assembly(plan)?),
    };

    let flatten_source: &Path = if let Some(path) = &plan.entry_override {
        path.as_path()
    } else {
        #[cfg(feature = "cooking")]
        {
            cook_assembly
                .as_ref()
                .map_or(plan.entry_stage.as_path(), |a| a.path.as_path())
        }
        #[cfg(not(feature = "cooking"))]
        {
            plan.entry_stage.as_path()
        }
    };

    // The staged entry (relative to the payload root) + its dependency set +
    // unresolved list — the three facts the rest of the cook needs, independent of
    // whether we flattened to `.usdz` or staged a loose tree.
    //
    // Loose staging is only honoured for targets whose packaging consumes the
    // cooked `out_dir` directly. Standalone bundled targets (Quest `.apk`, Vision
    // Pro `.app`) copy a single `stage.usdz` resource into the app archive, so a
    // loose cook would hand signing/assembly a nonexistent resource; force-flatten
    // for them (with a warning) until the bundlers can consume a loose tree.
    let loose_levels = if plan.loose_levels && !plan.target.supports_loose_levels() {
        tracing::warn!(
            target = %plan.target.label,
            "loose-levels packaging is not supported for this target (it bundles a single \
             stage resource into the app); flattening to stage.usdz instead"
        );
        false
    } else {
        plan.loose_levels
    };
    let entry: EntryOutput = if per_level {
        #[cfg(feature = "cooking")]
        {
            cook_per_level_entries(plan)?
        }
        #[cfg(not(feature = "cooking"))]
        {
            unreachable!("per_level is never true without the cooking feature")
        }
    } else if loose_levels {
            // Loose staging: copy the closure verbatim under the payload root,
            // preserving structure (no flatten, no remapper). The reserved
            // `stage.usdz` path (`stage_out`) is unused on this path. Stage
            // relative to the PROJECT ROOT: dependencies outside it (the toolbox
            // built-in player/rig tree) are reported as `external` and left
            // uncopied — they are staged separately by `stage_builtin_tree` below
            // and resolved via the runtime asset search path.
            //
            // Arm the resolver's packaging mode while computing the closure so a
            // remote (`http(s)://`) ref is never *fetched* (the 404s) — with it
            // armed the ref reports unresolvable and `stage_loose_closure*` keeps
            // it external verbatim instead of trying to canonicalize the URL as a
            // filesystem path. The guard disarms on any exit (incl. `?`).
            // Feature-gated: when the resolver is not linked it is not installed
            // in-process either, so remote refs are unresolvable anyway.
            #[cfg(feature = "usd-resolver-http")]
            let _packaging_mode = PackagingModeGuard::arm();
            let loose = idtx_usd::stage_loose_closure_under_root(
                flatten_source,
                &plan.out_dir,
                &plan.project_root,
            )?;
            // The toolbox built-in tree lives outside the project root and is
            // staged separately (`stage_builtin_tree`) + resolved via the runtime
            // asset search path, so its deps are expected-external and must NOT
            // warn. Classify by whether the dependency actually resolves under the
            // canonical built-in tree — a filename heuristic (matching
            // "Player"/"Rig") also silenced genuine project assets carrying those
            // words, which `stage_builtin_tree` cannot provide.
            let builtin_root = std::fs::canonicalize(idtx_assets::builtin_tree()).ok();
            for ext in &loose.external {
                let name = ext.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                // Remote (`http(s)://`) refs are intentionally kept external
                // verbatim (fetched at runtime by the HTTP resolver) — not a
                // missing local dependency, so never warn.
                let is_remote = ext
                    .to_str()
                    .is_some_and(|s| s.starts_with("http:/") || s.starts_with("https:/"));
                let is_builtin = ext
                    .components()
                    .any(|c| c.as_os_str().eq_ignore_ascii_case("built-in"))
                    || is_builtin_dependency(ext, builtin_root.as_deref());
                if is_remote {
                    tracing::info!(url = %ext.display(), "loose cook: remote reference kept external (not packed)");
                } else if is_builtin {
                    tracing::debug!(dep = %ext.display(), "loose cook: built-in dep staged separately, left external");
                } else {
                    tracing::warn!(
                        dep = %ext.display(),
                        name,
                        "loose cook: dependency outside the project root was not staged — it must be \
                         imported into the project (or it will dangle at runtime)"
                    );
                }
            }
            // The scratch assembly dir carries a per-run `<pid>-<nanos>` suffix; its
            // staged copy would leak that into the payload layout and the run.toml
            // stage path, breaking the cook's reproducibility contract. Relocate it
            // to a fixed, same-depth name so identical cooks emit an identical tree
            // (same depth keeps the assembly's downward payload arcs resolvable).
            let staged_entry = relocate_scratch_assembly(&plan.out_dir, loose.staged_entry)?;
            tracing::info!(
                stage = %idtx_project::normalize_display_path(flatten_source),
                entry = %staged_entry.display(),
                deps = loose.dependencies.len(),
                external = loose.external.len(),
                "cooked loose stage payload (levels stay loose, referenced via payloads)"
            );
            // Remote refs already stay external in a loose cook (they resolve
            // outside the project root and are left uncopied/unrewritten); surface
            // the `http(s)` ones so `CookReport.external` is consistent with the
            // flatten path.
            let external_remote: Vec<String> = loose
                .external
                .iter()
                .filter_map(|p| p.to_str())
                .filter(|s| s.starts_with("http:/") || s.starts_with("https:/"))
                .map(ToOwned::to_owned)
                .collect();
            EntryOutput {
                report_stage: plan.out_dir.join(&staged_entry),
                stage_rel: Some(staged_entry),
                levels_rel: Vec::new(),
                start_rel: None,
                transfer_rel: None,
                unresolved: Vec::new(),
                external: external_remote,
            }
        } else {
            // Arm the resolver's packaging mode for the whole pack so that
            // computing the dependency closure never *fetches* a remote ref (the
            // 404s); with it armed, remote URLs report unresolvable. The guard
            // disarms on any exit (incl. a `?` early return). Feature-gated: when
            // the resolver is not linked it is not installed in-process either, so
            // remote refs are unresolvable anyway and no toggle is needed.
            #[cfg(feature = "usd-resolver-http")]
            let _packaging_mode = PackagingModeGuard::arm();
            // If the closure has remote refs, use the packager that LOCALIZES
            // local references to package-internal paths (so the usdz resolves its
            // own assets on device) while PRESERVING the authored `http(s)://`
            // URLs (the plain `package_stage` would rewrite them into dangling
            // internal buckets). A purely-local stage keeps the plain localize.
            let has_remote = idtx_usd::stage_has_remote_refs(flatten_source)?;
            let package = if has_remote {
                idtx_usd::package_stage_localize_keeping_remote(flatten_source, &stage_out)?
            } else {
                idtx_usd::package_stage(flatten_source, &stage_out)?
            };
            guard_packaged(&stage_out, &package)?;
            crate::usdz_metadata::normalize(&stage_out)?;
            tracing::info!(
                stage = %idtx_project::normalize_display_path(flatten_source),
                out = %stage_out.display(),
                deps = package.dependencies.len(),
                unresolved = package.unresolved.len(),
                external = package.external.len(),
                verbatim = has_remote,
                "cooked stage payload"
            );
            for url in &package.external {
                tracing::info!(%url, "cook: remote reference kept external (not packed)");
            }
            #[cfg(feature = "cook-metrics")]
            tracing::info!(
                externalized_remote_refs = package.external.len(),
                "cook: remote references left external (not fetched/packed)"
            );
            EntryOutput {
                stage_rel: Some(PathBuf::from(PAYLOAD_STAGE)),
                levels_rel: Vec::new(),
                start_rel: None,
                transfer_rel: None,
                unresolved: package.unresolved,
                external: package.external,
                report_stage: stage_out.clone(),
            }
        };

    let EntryOutput {
        stage_rel,
        levels_rel,
        start_rel,
        transfer_rel,
        unresolved,
        external,
        report_stage,
    } = entry;

    #[cfg(feature = "cook-metrics")]
    tracing::info!(
        elapsed_ms = u64::try_from(compose_flatten_start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "cook: composed + flattened entry stage"
    );

    // The scratch assembly has been read by `package_stage`; drop it now so the
    // temp dir is gone well before the rest of the cook (behaviors, manifest).
    #[cfg(feature = "cooking")]
    drop(cook_assembly);

    // 2. Collect + validate behavior modules discovered under the asset roots.
    let engine = Engine::default();
    let behaviors = collect_behaviors(&engine, &plan.asset_roots, &behaviors_dir, excluded)?;

    // 3. Copy the declared controller meshes into the payload and rewrite the XR
    //    config paths to payload-relative — the packaged twin of preview asset
    //    resolution.
    //
    //    Resolution roots: the project's own asset roots first, then the toolbox's
    //    built-in tree (`idtx_assets::builtin_tree()`) as a lowest-priority
    //    fallback. The `run.toml` controller arcs are `controllers/<file>`, which
    //    resolve under the built-in tree as `built-in/controllers/<file>` — so a
    //    project that ships no local controller meshes still folds the toolbox's
    //    shipped `generic` / `questpro` / `quest2` meshes into the payload. Without
    //    this fallback the cook silently dropped every device (the meshes live in
    //    the toolbox, not the project), `[xr]` came out empty, and the packaged
    //    runtime resolved no mesh → geometry-less controllers on Quest/PCVR.
    let assets_dir = plan.out_dir.join(PAYLOAD_ASSETS_DIR);
    let mut controller_roots = plan.asset_roots.clone();
    controller_roots.push(idtx_assets::builtin_tree());
    let (xr, controllers) = cook_xr_controllers(&plan.xr, &controller_roots, &assets_dir)?;

    // 3b. Stage runtime-only assets the USD dependency walk cannot see
    //     (behavior-referenced images/audio/etc.) by copying the declared asset
    //     roots verbatim into the payload, copy-if-absent. See
    //     `stage_runtime_asset_roots`.
    let runtime_assets =
        stage_runtime_asset_roots(&plan.asset_roots, &plan.project_root, &plan.out_dir, excluded)?;
    if !runtime_assets.is_empty() {
        tracing::info!(
            count = runtime_assets.len(),
            "cook: staged runtime-only assets from declared asset roots (not in the USD dep closure)"
        );
    }
    // Registered templates are prepared here even for direct/xtask cooking
    // without a shell behavior callback. Package each independent dependency
    // closure before emitting the bundle-relative runtime registry.
    let runtime_prototypes = if plan.runtime_prototypes.is_empty() {
        Default::default()
    } else {
        let prototype_scratch = tempfile::tempdir().map_err(|source| CookError::Io { path: plan.out_dir.clone(), source })?;
        let mut prototype_project = idtx_project::IdtxProject::load(plan.project_root.join("project.toml"))
            .map_err(|error| CookError::BehaviorPrepare(error.to_string()))?;
        prototype_project.manifest_mut().runtime_prototypes = plan.runtime_prototypes.clone();
        let sdk = plan.behavior_sdk.clone()
            .or_else(|| idtx_behavior_build::resolve_sdk_dir(std::env::current_exe().ok().as_deref()))
            .or_else(|| {
                let development = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.join("idtx-behavior-sdk");
                development.join("Cargo.toml").is_file().then_some(development)
            });
        let prepared_prototypes = crate::prototype_prepare::prepare_runtime_prototypes(&prototype_project, sdk.as_deref(), prototype_scratch.path())
            .map_err(CookError::BehaviorPrepare)?;
        crate::prototype_prepare::package_runtime_prototypes(&prepared_prototypes, &plan.out_dir)
            .map_err(CookError::BehaviorPrepare)?
    };

    // 4. Record configuration; the complete inventory is hashed after staging.
    let features = plan.cooked_features();
    let abi_version = idtx_behavior::ABI_VERSION;

    // 5. Emit the stamped schema-v2 manifest.
    let manifest = RuntimeManifest::default()
        .with_experience(ExperienceConfig {
            stage: stage_rel.clone(),
            mode: Some(plan.target.mode.clone()),
            name: Some(plan.name.clone()),
            target: Some(plan.target.label.clone()),
            // Stamp fullscreen only when the target wants it (desktop), so an XR
            // bundle's `[experience]` keeps the key omitted → windowed default.
            fullscreen: plan.target.fullscreen.then_some(true),
            // Per-level cook mode stamps the multi-level entries (paths); the
            // combined-assembly / single-stage path leaves these empty/None.
            levels: levels_rel.clone(),
            start_level: start_rel.clone(),
            transfer_level: transfer_rel.clone(),
            // Per-target texture profile (device targets default to a capped
            // profile); `None` leaves the runtime default in force.
            texture_quality: plan.texture_quality.clone(),
        })
        .with_runtime_prototypes(runtime_prototypes)
        .with_xr(xr)
        .with_pins(PinsConfig {
            abi_version: Some(abi_version),
            wgpu: Some(PIN_WGPU.to_string()),
            wasmtime: Some(PIN_WASMTIME.to_string()),
            glam: Some(PIN_GLAM.to_string()),
            required_features: features,
            ..Default::default()
        })
        .with_network({
            // TEMPORARY (see docs/plan_remote_refs_external_and_runtime_auth.md):
            // fold an opt-in dev bearer token into the stamped `[network]` so a
            // device runtime can authenticate remote fetches without a keychain.
            // Remove when the interactive logon-level flow lands.
            let mut network = plan.network.clone();
            if let Some(token) = plan.embed_dev_token.as_deref().filter(|t| !t.is_empty()) {
                tracing::warn!(
                    "TEMPORARY: embedding a bearer token into run.toml \
                     ([network].dev_bearer_token) — this build carries logon \
                     credentials and MUST NOT be distributed. Remove once the \
                     interactive logon-level flow exists."
                );
                token.clone_into(&mut network.dev_bearer_token);
            }
            network
        })
        .with_behavior(plan.behavior.clone())
        .with_stamp(BuildStamp {
            content_hash: None,
            target: Some(plan.target.label.clone()),
        });
    let manifest_out = plan.out_dir.join(PAYLOAD_MANIFEST);
    write_file(&manifest_out, manifest.to_toml_string().as_bytes())?;

    // 6. Stage the project's `input_map.toml` (if any) at the payload root. The
    //    project tree is not part of the payload, so this is the only copy the
    //    packaged runtime's `load_input_map` can find; without it XR stick /
    //    button actions resolve to nothing. Mirrors the preview path's
    //    `copy_input_map_into`. Absent is non-fatal (a project may bind nothing).
    let input_map_src = plan.project_root.join(PAYLOAD_INPUT_MAP);
    if input_map_src.is_file() {
        let bytes = read_file(&input_map_src)?;
        write_file(&plan.out_dir.join(PAYLOAD_INPUT_MAP), &bytes)?;
        tracing::info!(src = %input_map_src.display(), "staged input_map.toml into payload");
    } else {
        tracing::info!(
            expected = %input_map_src.display(),
            "no input_map.toml at project root; packaged build has no input bindings"
        );
    }

    // 7. Stage the IDTX custom-schema plugin resources (`plugInfo.json` +
    //    `generatedSchema.usda`) into `<payload>/usd-schemas/` so the runtime can
    //    register `IdtxCollisionAPI` on-device (the compile-time baked plugin dir
    //    does not exist there). The source is the schema crate's own baked dir,
    //    which is valid on this (cook) host. Absent is non-fatal — collision just
    //    will not hydrate — but is a real packaging defect, so warn.
    stage_schema_resources(&plan.out_dir.join(PAYLOAD_SCHEMA_DIR))?;

    // 7b. Stage the coded OpenExec plugin (DLL + `plugInfo.json` +
    //     `generatedSchema.usda`) into `<payload>/usd-exec/` so a packaged runtime
    //     built with `openexec` can register it off the build host. Host/desktop
    //     only: the plugin carries a native library, and a standalone bundle gets
    //     a cross-built one from the runtime cross-build + packager instead (see
    //     `stage_exec_resources`). Feature-gated so a non-`openexec` cook pays
    //     neither the `idtx-usd-exec` dep nor its C++ compile.
    #[cfg(feature = "openexec")]
    stage_exec_resources(&plan.out_dir.join(PAYLOAD_EXEC_DIR), &plan.target)?;

    // 8. Stage the toolbox's built-in asset tree beside the runtime binary so
    //    `@built-in/...@` references resolve at runtime (the exe dir is placed on
    //    the default-resolver search path). The flatten already localized the
    //    built-in player + rigs into the entry stage, so this is parity /
    //    robustness; absent is non-fatal.
    stage_builtin_tree(&plan.out_dir.join(PAYLOAD_BUILTIN_DIR))?;

    let content_hash = crate::publication::stamp(&plan.out_dir, &manifest_out)?;

    Ok(CookReport {
        out_dir: plan.out_dir.clone(),
        stage: report_stage,
        manifest: manifest_out,
        behaviors,
        controllers,
        unresolved,
        external,
        content_hash,
        abi_version,
    })
}

/// The entry facts `cook()` needs independent of how the payload was produced
/// (single flattened `stage.usdz`, a loose tree, or per-level assemblies).
struct EntryOutput {
    /// Single entry stage relative to the payload root (combined-assembly /
    /// flatten / loose paths). `None` in per-level mode.
    stage_rel: Option<PathBuf>,
    /// Per-level assembly entries (payload-relative paths), in logical order.
    /// Empty outside per-level mode.
    levels_rel: Vec<PathBuf>,
    /// The start level's assembly (payload-relative). `None` outside per-level mode.
    start_rel: Option<PathBuf>,
    /// The transfer level's assembly (payload-relative). `None` when none / outside
    /// per-level mode.
    transfer_rel: Option<PathBuf>,
    /// Unresolved references surfaced (non-fatal).
    unresolved: Vec<String>,
    /// Remote references kept external.
    external: Vec<String>,
    /// Absolute path recorded in [`CookReport::stage`] (the single entry, or the
    /// start level's assembly in per-level mode).
    report_stage: PathBuf,
}

/// How the shared player rig is referenced by each per-level assembly.
#[cfg(feature = "cooking")]
enum SharedPlayerRef {
    /// A project player stage (absolute), referenced loose and localized by each
    /// level's own flatten into the self-contained `level.usdz`.
    Project(PathBuf),
    /// The toolbox built-in player, referenced by its verbatim `built-in/...` arc.
    Builtin(String),
    /// No player composed.
    None,
}

/// Cook a multi-level experience as **per-level assemblies** (Scenario 2): each
/// level is composed as a per-level assembly (the level body re-rooted under
/// `/World/level`, the shared player rig, and the shared global sublayers — see
/// [`author_per_level_assembly`]) in a scratch dir, has that level's authored
/// behavior sources compiled + bound via `idtx:behavior:module` (the
/// [`CookPlan::level_behavior_prepare`] hook, when supplied), and is then
/// **flattened** into a self-contained `payload/levels/<id>/level.usdz`. Each
/// level's compiled `.wasm` is co-located at `payload/levels/<id>/behaviors/`, so
/// the authored `idtx:behavior:module = ./behaviors/<name>.wasm` — which the
/// flatten preserves verbatim (see [`LevelBehaviorOverlay`]) — resolves
/// layer-relative to the level's own `level.usdz` at runtime. The optional
/// transfer level is cooked the same way under `transfer/`. See
/// `docs/level-transition-runtime-plan.md` §3/§14.
///
/// The per-level assembly references the **loose** project level/player/sublayer
/// files (not a pre-flattened body): this is what lets the behavior-prepare pass
/// resolve each `idtx:behavior:source` against its authoring layer (a project
/// file) exactly as the known-working combined path does — the final per-level
/// flatten localizes every dependency into the usdz. Shared player/global layers
/// are therefore embedded independently into every level (dedup across levels is
/// deliberately sacrificed for correctness — a follow-up could stage them loose
/// once and reference them).
///
/// Unlike the combined-assembly path this never flattens a single `stage.usdz`;
/// the start level's `level.usdz` is the experience entry.
///
/// # Errors
/// [`CookError`] on a failed flatten, assembly authoring, behavior preparation, or
/// scratch I/O.
#[cfg(feature = "cooking")]
fn cook_per_level_entries(plan: &CookPlan) -> Result<EntryOutput, CookError> {
    use crate::assembly::{
        author_per_level_assembly, paths_equal, relative_asset_path, stem_of, unique_holder_name,
        PerLevelAssembly,
    };

    // Arm the resolver's packaging mode so computing each level's dependency
    // closure never *fetches* a remote ref (mirrors the flatten path). Disarms on
    // any exit.
    #[cfg(feature = "usd-resolver-http")]
    let _packaging_mode = PackagingModeGuard::arm();

    let out = &plan.out_dir;

    // A per-run scratch holding each level's composed assembly + overlay + the
    // co-located `.wasm`, flattened into the payload below. The guard removes the
    // whole tree on return (incl. a `?` early exit) — after the per-level flattens
    // and the `.wasm` copies out of it have completed.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch_root = out.join(format!(".idtx-perlevel-{}-{nanos}", std::process::id()));
    create_dir(&scratch_root)?;
    let _scratch_guard = ScratchDir(scratch_root.clone());

    let player = match &plan.player {
        Some(PlayerRef::Project(abs)) => SharedPlayerRef::Project(abs.clone()),
        Some(PlayerRef::Builtin(arc)) => SharedPlayerRef::Builtin(arc.clone()),
        None => SharedPlayerRef::None,
    };
    // Bake the player rig for the target mode (desktop/xr).
    let variant = if plan.target.mode == "xr" { "xr" } else { "desktop" }.to_owned();

    let mut dependencies: Vec<PathBuf> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut external: Vec<String> = Vec::new();

    // Compose one level's assembly in `<scratch>/<id>/`, run the behavior-prepare
    // hook, flatten the (overlaid) assembly into `payload/<dst_dir_rel>/level.usdz`,
    // and co-locate the compiled `.wasm` beside it. Returns the payload-relative
    // `level.usdz` path. `&mut` accumulators capture the closure of each flatten.
    let mut cook_level = |level_abs: &Path, id: &str, dst_dir_rel: &str| -> Result<PathBuf, CookError> {
        let asm_dir = scratch_root.join(id);
        create_dir(&asm_dir)?;

        let per = PerLevelAssembly {
            body_arc: relative_asset_path(&asm_dir, level_abs),
            player_reference: match &player {
                SharedPlayerRef::Project(abs) => Some(relative_asset_path(&asm_dir, abs)),
                SharedPlayerRef::Builtin(arc) => Some(arc.clone()),
                SharedPlayerRef::None => None,
            },
            player_variant: Some(variant.clone()),
            shared_sublayers: plan
                .shared_sublayers
                .iter()
                .map(|a| relative_asset_path(&asm_dir, a))
                .collect(),
        };
        let asm_path = asm_dir.join("level.usda");
        let cfg = read_assembly_cfg(level_abs);
        author_per_level_assembly(&asm_path, &per, level_abs, cfg.as_ref(), &plan.name)?;

        // Compile + bind this level's behaviors onto the composed assembly (the
        // hook authors `idtx:behavior:module = ./behaviors/<name>.wasm` layer-
        // relative to `asm_dir` and co-locates each `.wasm` there). `None` (hook
        // unset, or the level has no behavior sources) flattens the bare assembly.
        let overlay = match &plan.level_behavior_prepare {
            Some(hook) => hook.call(&asm_path).map_err(CookError::BehaviorPrepare)?,
            None => None,
        };
        let flatten_src: &Path = overlay
            .as_ref()
            .map_or(asm_path.as_path(), |o| o.overlaid_assembly.as_path());

        // Flatten the (overlaid) assembly into a self-contained usdz, preserving
        // authored remote URLs when present (mirrors the combined flatten path).
        let level_dir = out.join(dst_dir_rel);
        create_dir(&level_dir)?;
        let usdz = level_dir.join("level.usdz");
        let pkg = if idtx_usd::stage_has_remote_refs(flatten_src)? {
            idtx_usd::package_stage_localize_keeping_remote(flatten_src, &usdz)?
        } else {
            idtx_usd::package_stage(flatten_src, &usdz)?
        };
        guard_packaged(&usdz, &pkg)?;
        crate::usdz_metadata::normalize(&usdz)?;
        dependencies.extend(pkg.dependencies);
        unresolved.extend(pkg.unresolved);
        external.extend(pkg.external);

        // Co-locate the project's `input_map.toml` beside the flattened
        // `level.usdz`. In per-level mode the payload carries no `project.toml`, so
        // the runtime's `project_root_for_stage` falls back to the level's own dir
        // and looks for `input_map.toml` there — the single payload-root copy
        // staged by `cook` is never found. Without this, XR stick / button actions
        // resolve to nothing on a packaged multi-level build. Absent is non-fatal.
        let input_map_src = plan.project_root.join(PAYLOAD_INPUT_MAP);
        if input_map_src.is_file() {
            write_file(&level_dir.join(PAYLOAD_INPUT_MAP), &read_file(&input_map_src)?)?;
        }

        // Co-locate each compiled module beside the flattened `level.usdz` so the
        // runtime's layer-relative `./behaviors/<name>.wasm` read resolves on disk.
        if let Some(ov) = &overlay {
            if !ov.wasm.is_empty() {
                create_dir(&level_dir.join(PAYLOAD_BEHAVIORS_DIR))?;
            }
            for w in &ov.wasm {
                let name = w
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("behavior.wasm");
                write_file(&level_dir.join(PAYLOAD_BEHAVIORS_DIR).join(name), &read_file(w)?)?;
            }
        }

        Ok(PathBuf::from(format!("{dst_dir_rel}/level.usdz")))
    };

    // One per-level assembly per registered level; the start intent is the
    // explicit start_level, else the default_level, else the first level.
    let start_intent = plan.start_level.clone().or_else(|| plan.default_level.clone());
    let mut used_ids = HashSet::new();
    let mut levels_rel: Vec<PathBuf> = Vec::new();
    let mut start_rel: Option<PathBuf> = None;
    for level_abs in &plan.levels {
        let id = unique_holder_name(&stem_of(level_abs), &mut used_ids);
        let rel = cook_level(level_abs, &id, &format!("levels/{id}"))?;
        if start_rel.is_none()
            && start_intent
                .as_deref()
                .is_some_and(|s| paths_equal(s, level_abs))
        {
            start_rel = Some(rel.clone());
        }
        levels_rel.push(rel);
    }
    let start_rel = start_rel.or_else(|| levels_rel.first().cloned());

    // Optional transfer level → `transfer/level.usdz`.
    let transfer_rel = if let Some(t) = &plan.transfer_level {
        Some(cook_level(t, "transfer", "transfer")?)
    } else {
        None
    };

    tracing::info!(
        levels = levels_rel.len(),
        start = ?start_rel,
        transfer = ?transfer_rel,
        deps = dependencies.len(),
        "cooked per-level assembly payload (each level self-contained usdz + co-located behaviors)"
    );

    let report_stage = start_rel
        .as_ref()
        .map_or_else(|| out.clone(), |r| out.join(r));

    Ok(EntryOutput {
        stage_rel: None,
        levels_rel,
        start_rel,
        transfer_rel,
        unresolved,
        external,
        report_stage,
    })
}

/// A composed assembly stage on disk, owning the scratch directory it lives in.
///
/// Returned by [`author_cook_assembly`]. Its [`Drop`] removes the scratch dir —
/// so the generated `assembly-root.usda` is "deleted after the cook" the moment
/// the artifact goes out of scope (whether the cook succeeded or bailed). The
/// shell `build_job` keeps it alive across the behavior overlay + `cook()` call,
/// then drops it; `cook()`'s own `cooking` path drops it right after flatten.
#[cfg(feature = "cooking")]
#[derive(Debug)]
pub struct AssemblyArtifact {
    /// Absolute path of the authored `assembly-root.usda`.
    pub path: PathBuf,
    /// RAII guard that deletes the enclosing scratch dir on drop.
    _guard: ScratchDir,
}

/// A scratch directory removed from disk when dropped (best-effort).
#[cfg(feature = "cooking")]
#[derive(Debug)]
struct ScratchDir(PathBuf);

#[cfg(feature = "cooking")]
impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            // A leftover temp dir is a cleanliness issue, not a cook failure.
            tracing::warn!(dir = %self.0.display(), "failed to remove cook assembly scratch: {e}");
        }
    }
}

/// Compose the project's levels into a **default-visible assembly stage** and
/// return it as a self-deleting [`AssemblyArtifact`].
///
/// The assembly authors each registered level as an unloaded payload holder
/// under `/World/Levels/<name>` with the configured [`CookPlan::default_level`]
/// as the only visible one (all others `invisible`), plus the configured
/// [`CookPlan::player`] composed as a reference at `/World/Player` — the
/// packaged twin of the editor preview's `build_project_assembly`. When no
/// levels are registered
/// it composes the lone [`CookPlan::entry_stage`] as the single visible level.
///
/// Stage metadata (up-axis, meters-per-unit, soft-shadow mode) is read from the
/// default level (else the first level, else the entry stage) so the assembly
/// inherits the authored scene's conventions.
///
/// The scratch dir is anchored directly under [`CookPlan::project_root`]
/// (`.idtx-cook-assembly-<pid>-<nanos>`) so the authored payload arcs are at a
/// stable depth relative to the levels — making them byte-identical across runs
/// and the cook's reproducibility hash machine-independent.
///
/// # Errors
///
/// [`CookError::Io`] if the scratch dir cannot be created, or
/// [`CookError::Package`] if reading the metadata source or authoring the
/// assembly stage fails.
#[cfg(feature = "cooking")]
pub fn author_cook_assembly(plan: &CookPlan) -> Result<AssemblyArtifact, CookError> {
    use crate::assembly::{author_assembly_stage, plan_cook_assembly};

    // Anchor the USD default-resolver search path at the project's asset roots
    // plus the toolbox's built-in tree *before* composing, so the `@built-in/...@`
    // player reference (and any project-relative level references) resolve while
    // this function authors the assembly. `cook()` sets the same path, but it runs
    // *after* this composition (the shell hands the composed assembly to `cook()`
    // via `entry_override`), and the process-global path left by prior editor
    // operations is not guaranteed to still include the built-in root. Without it
    // `@built-in/players/Player.usd@` fails to open, the `playerKind` variantSet
    // composes empty, and the mode variant selection below is then rejected with
    // "variant restore selection does not exist". Process-global, idempotent, and
    // last-call-wins — matches the anchoring in `cook()`.
    {
        let mut dirs = plan.asset_roots.clone();
        dirs.push(idtx_assets::builtin_root());
        idtx_usd::set_default_asset_search_path(&dirs)?;
    }

    // Levels to compose: the registry when non-empty, else the lone entry stage.
    let levels: Vec<PathBuf> = if plan.levels.is_empty() {
        vec![plan.entry_stage.clone()]
    } else {
        plan.levels.clone()
    };
    // The visible level: the configured default, else (for the entry-stage
    // fallback) the lone entry stage itself.
    let default_level = plan
        .default_level
        .clone()
        .or_else(|| (plan.levels.is_empty()).then(|| plan.entry_stage.clone()));

    // Metadata source: the default level, else the first composed level.
    let meta_source = default_level.as_deref().unwrap_or(&levels[0]);
    let cfg = read_assembly_cfg(meta_source);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch = plan.project_root.join(format!(
        "{COOK_ASSEMBLY_SCRATCH_PREFIX}{}-{nanos}",
        std::process::id()
    ));
    create_dir(&scratch)?;
    let guard = ScratchDir(scratch.clone());

    let mut assembly_plan = plan_cook_assembly(
        &scratch,
        &levels,
        default_level.as_deref(),
        plan.player.as_ref(),
    );
    // Bake the player rig for the target mode: an XR target selects the `xr`
    // variant, anything else the `desktop` variant.
    assembly_plan.player_variant = Some(
        if plan.target.mode == "xr" { "xr" } else { "desktop" }.to_owned(),
    );
    let path = scratch.join("assembly-root.usda");
    author_assembly_stage(&path, &assembly_plan, cfg.as_ref(), &plan.name)?;

    Ok(AssemblyArtifact {
        path,
        _guard: guard,
    })
}

/// Read the up-axis / units / soft-shadow mode from `source` for the composed
/// assembly, or `None` when the stage cannot be opened (the composer then falls
/// back to its `Y` / `1.0` / shadow-off defaults).
#[cfg(feature = "cooking")]
fn read_assembly_cfg(source: &Path) -> Option<crate::assembly::AssemblyCfg> {
    let stage = idtx_usd::StageService::open_topology_only(source).ok()?;
    let meta = stage.metadata();
    Some(crate::assembly::AssemblyCfg {
        up_axis: meta.up_axis.clone(),
        meters_per_unit: meta.meters_per_unit,
        shadow_mode: meta.shadow_mode,
    })
}

/// Discover, validate, and copy every `.wasm` behavior module found under
/// `roots` into `behaviors_dir`. Modules are deduplicated by content hash (the
/// same module reachable from two roots is collected once). A name collision
/// between two *different* modules is disambiguated with a hash prefix.
fn collect_behaviors(
    engine: &Engine,
    roots: &[PathBuf],
    behaviors_dir: &Path,
    excluded: &[PathBuf],
) -> Result<Vec<BehaviorArtifact>, CookError> {
    let mut wasm_paths = Vec::new();
    for root in roots {
        collect_wasm_files(root, &mut wasm_paths, excluded)?;
    }
    wasm_paths.sort();
    wasm_paths.dedup();

    let mut artifacts: Vec<BehaviorArtifact> = Vec::new();
    for source in wasm_paths {
        let bytes = read_file(&source)?;
        let hash = hex_sha256(&bytes);
        // Dedup by content: skip a module we have already collected.
        if artifacts.iter().any(|a| a.hash == hash) {
            continue;
        }

        // A real wasmtime compile — rejects corrupt / non-wasm files at cook time.
        let module = Module::new(engine, &bytes).map_err(|source_err| CookError::Behavior {
            path: source.clone(),
            source: source_err,
        })?;
        let (hooks, has_abi_marker) = inspect_exports(&module);
        if !has_abi_marker {
            tracing::warn!(
                module = %idtx_project::normalize_display_path(&source),
                "behavior module lacks the _idtx_abi_version export; it will fail \
                 the runtime ABI probe (collected anyway for diagnosis)"
            );
        }

        let file_name = payload_file_name(&source, &hash, &artifacts);
        let dest = behaviors_dir.join(&file_name);
        write_file(&dest, &bytes)?;
        // The behavior's capability manifest (capabilities, inputs, …) travels
        // on the stage as `IdtxBehaviorAPI` schema attributes — there is no
        // sidecar `idtx.toml` to colocate.
        artifacts.push(BehaviorArtifact {
            source,
            payload_rel: PathBuf::from(PAYLOAD_BEHAVIORS_DIR).join(&file_name),
            hash,
            hooks,
            has_abi_marker,
        });
    }
    Ok(artifacts)
}

/// Choose a collision-free payload file name for a module.
fn payload_file_name(source: &Path, hash: &str, existing: &[BehaviorArtifact]) -> String {
    let stem = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("behavior.wasm")
        .to_string();
    let collides = existing
        .iter()
        .any(|a| a.payload_rel.file_name().and_then(|n| n.to_str()) == Some(stem.as_str()));
    if collides {
        // Prefix with a short hash to disambiguate two different modules that
        // happen to share a file name across roots.
        format!("{}-{stem}", &hash[..8])
    } else {
        stem
    }
}

/// Inspect a compiled module's exports: collect `_idtx_on_*` lifecycle hook
/// names (sorted) and report whether the `_idtx_abi_version` marker is present.
fn inspect_exports(module: &Module) -> (Vec<String>, bool) {
    let mut hooks = Vec::new();
    let mut has_abi = false;
    for export in module.exports() {
        let name = export.name();
        if name == "_idtx_abi_version" {
            has_abi = true;
        } else if name.starts_with("_idtx_on_") {
            hooks.push(name.to_string());
        }
    }
    hooks.sort();
    (hooks, has_abi)
}

/// Stage declared asset roots verbatim into the payload, **without overwriting**
/// files already staged by stage localization or the controller copy.
///
/// USD dependency localization only stages assets reachable by composition.
/// Runtime-only assets referenced from behavior code — e.g. a HUD crosshair a
/// behavior authors via `attach_crosshair("images/crosshair.png")` — are
/// invisible to the dependency walk and would dangle at runtime (the render
/// side resolves them against the runtime asset search path, but the file was
/// never packaged). The project declares `asset_roots` precisely as its runtime
/// asset search root(s), so each root inside the project is copied into the
/// payload at the same path *relative to the project root* (matching how loose
/// staging lays dependencies out), which is exactly where the packaged runtime's
/// asset search path (`<exe>` + `<exe>/assets`) resolves them.
///
/// **Copy-if-absent**: a file already present in the payload (a localized or
/// flattened USD dependency, a copied controller mesh) is the authoritative
/// version and is left untouched; only files the dependency walk never staged
/// are added. This precedence applies only within the current fresh staging
/// tree. The complete tree is hashed after all resources have been staged.
fn stage_runtime_asset_roots(
    asset_roots: &[PathBuf],
    project_root: &Path,
    out_dir: &Path,
    excluded: &[PathBuf],
) -> Result<Vec<PathBuf>, CookError> {
    // Canonicalize the project root once so `strip_prefix` is robust to a
    // symlinked or non-normalized root vs. the canonical asset-root paths.
    let project_canon = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.into());
    // The payload output tree lives INSIDE the project; never copy it back into
    // itself (a broad asset root that is an ancestor of `out_dir` would otherwise
    // recurse forever). Pruned during the walk as a safety net.
    let mut copied: Vec<PathBuf> = Vec::new();
    for root in asset_roots {
        if !root.is_dir() {
            continue;
        }
        // Only roots INSIDE the project have a project-relative payload home; a
        // root outside it (e.g. the toolbox built-in tree, appended for controller
        // resolution) is staged + resolved separately, so skip it here.
        let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let Ok(root_rel) = root_canon.strip_prefix(&project_canon) else {
            continue;
        };
        // Skip the implicit project-root resolution entry. `asset_roots` prepends
        // the bare project root so project-relative *references* resolve; bulk-
        // copying the whole project (source, `.git`, the build output) is never
        // intended and would pull the payload into itself. Only explicitly
        // declared sub-roots (e.g. `assets`) are staged verbatim.
        if root_rel.as_os_str().is_empty() {
            continue;
        }
        fill_dir_if_absent(
            &root_canon,
            &out_dir.join(root_rel),
            root_rel,
            excluded,
            &mut copied,
        )?;
    }
    copied.sort();
    Ok(copied)
}

/// Recursively copy files from `src_dir` to `dst_dir` that do not already exist,
/// recording each copied file's payload-relative path (`payload_rel` + name) in
/// `copied`. Directories are recursed; existing files are skipped. The payload
/// output tree (`exclude`, canonical) is never descended into, so a declared
/// asset root that happens to contain the build output cannot copy it into
/// itself. Symlinks are skipped (never followed), so a link inside a declared
/// asset root cannot be used to stage files from outside the root or to recurse
/// into a cycle.
fn fill_dir_if_absent(
    src_dir: &Path,
    dst_dir: &Path,
    payload_rel: &Path,
    exclude: &[PathBuf],
    copied: &mut Vec<PathBuf>,
) -> Result<(), CookError> {
    if crate::publication::generated_dir(src_dir, exclude) {
        return Ok(());
    }
    let entries = std::fs::read_dir(src_dir).map_err(|source| CookError::Io {
        path: src_dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CookError::Io {
            path: src_dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let src = entry.path();
        let dst = dst_dir.join(&name);
        let rel = payload_rel.join(&name);
        // Classify via the directory entry's own type, NOT `Path::is_dir`/
        // `fs::copy` — both follow symlinks, so a symlink inside a declared asset
        // root would let staging escape the root (a dir link traverses outside the
        // sandbox or into a cycle; a file link copies its target's bytes verbatim).
        // `DirEntry::file_type` reports the link itself without traversing it, so
        // skip symlinks outright.
        let file_type = entry.file_type().map_err(|source| CookError::Io {
            path: src.clone(),
            source,
        })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            fill_dir_if_absent(&src, &dst, &rel, exclude, copied)?;
        } else if file_type.is_file() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|source| CookError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::copy(&src, &dst).map_err(|source| CookError::Io {
                path: dst.clone(),
                source,
            })?;
            copied.push(rel);
        }
    }
    Ok(())
}

/// Recursively collect `*.wasm` files under `dir` into `out`.
fn collect_wasm_files(dir: &Path, out: &mut Vec<PathBuf>, excluded: &[PathBuf]) -> Result<(), CookError> {
    if crate::publication::generated_dir(dir, excluded) || !dir.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|source| CookError::Io { path: dir.to_owned(), source })?;
    for entry in entries {
        let entry = entry.map_err(|source| CookError::Io { path: dir.to_owned(), source })?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|source| CookError::Io { path: path.clone(), source })?;
        if kind.is_dir() {
            collect_wasm_files(&path, out, excluded)?;
        } else if kind.is_file() && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wasm"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Copy each declared device's controller mesh assets into the payload's
/// `assets/controllers/` dir and rewrite the manifest paths to be payload-
/// relative (`controllers/<file>`), so the packaged runtime resolves
/// device→asset over its `<exe>/assets` root exactly as the editor preview
/// resolves it over the project's asset roots.
///
/// Each entry's `left`/`right`/`asset` relative path is resolved against
/// `asset_roots` (first existing file wins) and copied once (a source reused
/// across hands/devices is copied a single time). `asset_roots` is the project's
/// own asset roots followed by the toolbox's built-in tree
/// ([`idtx_assets::builtin_tree`]) as a lowest-priority fallback, so the shipped
/// `controllers/<file>` meshes resolve even for a project that carries none. A
/// path that does not resolve is dropped from the emitted entry (logged) — the
/// runtime then applies its capsule fallback for that hand; a device whose meshes
/// all drop out is omitted from the emitted config. Two *different* sources
/// sharing a base filename are disambiguated with a short hash prefix, mirroring
/// behavior collection.
///
/// Returns the rewritten [`XrConfig`] (paths payload-relative) plus the sorted
/// payload-relative paths of every copied mesh.
fn cook_xr_controllers(
    xr: &XrConfig,
    asset_roots: &[PathBuf],
    assets_dir: &Path,
) -> Result<(XrConfig, Vec<PathBuf>), CookError> {
    use std::collections::{BTreeMap, HashMap, HashSet};

    let controllers_dir = assets_dir.join("controllers");
    let mut out_controllers: BTreeMap<String, ControllerAsset> = BTreeMap::new();
    // Resolved source → assigned payload filename, so the same file copies once.
    let mut assigned: HashMap<PathBuf, String> = HashMap::new();
    let mut used_names: HashSet<String> = HashSet::new();
    let mut copied: Vec<PathBuf> = Vec::new();

    for (device, entry) in &xr.controllers {
        // Resolve+copy one relative mesh; returns its payload-relative manifest
        // path (`controllers/<file>`) or `None` when it does not resolve.
        let mut place = |rel: Option<&Path>| -> Result<Option<PathBuf>, CookError> {
            let Some(rel) = rel else {
                return Ok(None);
            };
            let Some(src) = resolve_in_roots(asset_roots, rel) else {
                tracing::warn!(
                    device = %device,
                    rel = %rel.display(),
                    "controller mesh not found in asset roots; dropping from payload"
                );
                return Ok(None);
            };
            if let Some(name) = assigned.get(&src) {
                // Forward-slash, not `Path::join` (which uses `\` on Windows): this
                // string is serialized into `run.toml` and resolved on the Quest
                // (Linux) filesystem, where a backslash is an ordinary filename
                // character, not a separator — a `controllers\x.usdz` path would
                // never match the staged `controllers/x.usdz` file on device.
                return Ok(Some(PathBuf::from(format!("controllers/{name}"))));
            }
            let base = src
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("controller.usda")
                .to_string();
            let name = unique_asset_name(&base, &src, &mut used_names);
            create_dir(&controllers_dir)?;
            let bytes = read_file(&src)?;
            write_file(&controllers_dir.join(&name), &bytes)?;
            // Portable (forward-slash) payload-relative path — see above.
            let rel_out = PathBuf::from(format!("controllers/{name}"));
            assigned.insert(src, name);
            copied.push(rel_out.clone());
            Ok(Some(rel_out))
        };

        let left = place(entry.left.as_deref())?;
        let right = place(entry.right.as_deref())?;
        let asset = place(entry.asset.as_deref())?;
        if left.is_some() || right.is_some() || asset.is_some() {
            out_controllers.insert(
                device.clone(),
                ControllerAsset {
                    left,
                    right,
                    asset,
                    mount: entry.mount,
                },
            );
        }
    }

    copied.sort();
    Ok((
        XrConfig {
            controllers: out_controllers,
        },
        copied,
    ))
}

/// Resolve `rel` against `roots` (first existing file wins); an absolute `rel`
/// that exists is returned as-is. Mirrors the resolver in `idtx-runtime-manifest`
/// (private there) so the cook resolves meshes exactly as the runtime will.
fn resolve_in_roots(roots: &[PathBuf], rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute() {
        return rel.is_file().then(|| rel.to_path_buf());
    }
    roots
        .iter()
        .map(|root| root.join(rel))
        .find(|candidate| candidate.is_file())
}

/// A payload filename for a copied controller mesh: the source's base name when
/// free, else a short source-path hash prefix to disambiguate two *different*
/// sources that share a base name. Records the chosen name in `used`.
fn unique_asset_name(
    base: &str,
    src: &Path,
    used: &mut std::collections::HashSet<String>,
) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let h = hex_sha256(src.to_string_lossy().as_bytes());
    let candidate = format!("{}-{base}", &h[..8]);
    used.insert(candidate.clone());
    candidate
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

fn create_dir(path: &Path) -> Result<(), CookError> {
    std::fs::create_dir_all(path).map_err(|source| CookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Whether `dep` (an absolute source path reported as outside the project root)
/// belongs to the toolbox built-in asset tree, which the cook stages separately
/// (`stage_builtin_tree`) and resolves through the runtime asset search path — so
/// it is expected-external, not a dangling dependency.
///
/// `builtin_root` must already be canonicalized (the caller computes it once via
/// `std::fs::canonicalize`) so the prefix comparison is consistent with the
/// canonicalized `dep` — on Windows both carry the `\\?\` verbatim prefix. A dep
/// that cannot be canonicalized, or a missing built-in root, is treated as *not*
/// built-in so a genuinely missing asset still warns (correctly erring toward
/// surfacing loss).
fn is_builtin_dependency(dep: &Path, builtin_root: Option<&Path>) -> bool {
    let Some(root) = builtin_root else {
        return false;
    };
    std::fs::canonicalize(dep).is_ok_and(|canon| canon.starts_with(root))
}

/// Relocate the staged cook-assembly directory to the fixed, reproducible
/// [`PAYLOAD_ASSEMBLY_DIR`] when `staged_entry`'s leading segment is a per-run
/// scratch dir ([`COOK_ASSEMBLY_SCRATCH_PREFIX`]), returning the rewritten entry
/// path. Renames the directory under `payload_root`; the rename is at the same
/// depth, so the assembly's relative payload arcs to the loose levels still
/// resolve. Any other `staged_entry` (e.g. an `entry_override` staged at a real,
/// already-deterministic project path) is returned unchanged.
fn relocate_scratch_assembly(
    payload_root: &Path,
    staged_entry: PathBuf,
) -> Result<PathBuf, CookError> {
    let mut components = staged_entry.components();
    let Some(first) = components.next() else {
        return Ok(staged_entry);
    };
    let is_scratch = first
        .as_os_str()
        .to_str()
        .is_some_and(|segment| segment.starts_with(COOK_ASSEMBLY_SCRATCH_PREFIX));
    if !is_scratch {
        return Ok(staged_entry);
    }

    let from = payload_root.join(first.as_os_str());
    let to = payload_root.join(PAYLOAD_ASSEMBLY_DIR);
    // A prior cook into the same out_dir may have left the fixed dir behind.
    if to.exists() {
        std::fs::remove_dir_all(&to).map_err(|source| CookError::Io {
            path: to.clone(),
            source,
        })?;
    }
    std::fs::rename(&from, &to).map_err(|source| CookError::Io {
        path: to.clone(),
        source,
    })?;
    Ok(Path::new(PAYLOAD_ASSEMBLY_DIR).join(components.as_path()))
}

fn read_file(path: &Path) -> Result<Vec<u8>, CookError> {
    std::fs::read(path).map_err(|source| CookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), CookError> {
    std::fs::write(path, bytes).map_err(|source| CookError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Copy the IDTX custom-schema plugin resources into `dest` (`<payload>/
/// usd-schemas/`), so the packaged runtime can register `IdtxCollisionAPI` from
/// disk instead of the compile-time baked `OUT_DIR` dir (which only exists on
/// the build host). The source is [`idtx_usd_extension::plugin_dir`] — valid on
/// this cook host. A missing source file warns rather than fails: a payload
/// without the schema still runs (collision simply does not hydrate), and
/// failing the whole cook over it would be disproportionate.
fn stage_schema_resources(dest: &Path) -> Result<(), CookError> {
    let src_dir = idtx_usd_extension::plugin_dir();
    create_dir(dest)?;
    let mut staged = 0usize;
    for file in ["plugInfo.json", "generatedSchema.usda"] {
        let src = src_dir.join(file);
        if src.is_file() {
            write_file(&dest.join(file), &read_file(&src)?)?;
            staged += 1;
        } else {
            tracing::warn!(
                src = %src.display(),
                "IDTX schema resource missing; on-device schema registration may be incomplete"
            );
        }
    }
    tracing::info!(
        files = staged,
        dest = %dest.display(),
        "staged IDTX schema plugin resources into payload"
    );
    Ok(())
}

/// Copy the coded `OpenExec` plugin resources into `dest` (`<payload>/usd-exec/`),
/// so a packaged runtime built with `openexec` can register the plugin from disk
/// (the compile-time baked `OUT_DIR` dir only exists on the build host). The
/// source is [`idtx_usd_exec::plugin_dir`] — valid on this cook host. Unlike the
/// codeless schema extension this plugin carries a **native library**, so it is
/// staged for host/desktop targets only; a standalone bundle (Quest/visionOS)
/// gets a *cross-built* plugin from the runtime cross-build + packager instead
/// (staging the host DLL there would be wrong-ABI). A missing source file warns
/// rather than fails — parity with [`stage_schema_resources`].
#[cfg(feature = "openexec")]
fn stage_exec_resources(dest: &Path, target: &CookTarget) -> Result<(), CookError> {
    // Standalone targets provide their own cross-built native plugin — skip the
    // host DLL (same predicate `CookTarget::supports_loose_levels` keys off).
    if !target.supports_loose_levels() {
        tracing::info!(
            target = %target.label,
            "skipping host OpenExec plugin staging for standalone target \
             (cross-built plugin provided by the runtime cross-build)"
        );
        return Ok(());
    }

    let src_dir = idtx_usd_exec::plugin_dir();
    create_dir(dest)?;
    let mut staged = 0usize;
    // The coded plugin needs all three siblings: the two resource files and the
    // host-appropriate plugin library (only one of the three names is present).
    let files = [
        "plugInfo.json",
        "generatedSchema.usda",
        "idtx_usd_exec.dll",
        "libidtx_usd_exec.dylib",
        "libidtx_usd_exec.so",
    ];
    for file in files {
        let src = src_dir.join(file);
        if src.is_file() {
            write_file(&dest.join(file), &read_file(&src)?)?;
            staged += 1;
        }
    }
    if staged == 0 {
        tracing::warn!(
            src = %src_dir.display(),
            "OpenExec plugin resources missing; packaged runtime exec registration \
             will be incomplete"
        );
    } else {
        tracing::info!(
            files = staged,
            dest = %dest.display(),
            "staged OpenExec plugin resources into payload"
        );
    }
    Ok(())
}

/// Copy the toolbox's built-in asset tree (`idtx_assets::builtin_tree()`) into
/// `dest` (`<payload>/built-in/`). A missing source tree is non-fatal — the
/// flatten already localized the built-in player + rigs into the entry stage;
/// this staging is parity for `@built-in/...@` references that survive flatten.
fn stage_builtin_tree(dest: &Path) -> Result<(), CookError> {
    let src = idtx_assets::builtin_tree();
    if !src.is_dir() {
        tracing::warn!(
            src = %src.display(),
            "built-in asset tree missing; `@built-in/...@` references will only \
             resolve via the flattened stage"
        );
        return Ok(());
    }
    fill_dir_if_absent(&src, dest, Path::new(""), &[], &mut Vec::new())?;
    tracing::info!(dest = %dest.display(), "staged built-in asset tree into payload");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_cook_targets_stamp_label_mode_features() {
        let q = CookTarget::quest("aarch64-linux-android");
        assert_eq!(q.triple, "aarch64-linux-android");
        assert_eq!(q.label, "quest");
        assert_eq!(q.mode, "xr");
        assert_eq!(
            q.features,
            vec!["quest".to_string()]
        );

        let v = CookTarget::vision_pro("aarch64-apple-visionos");
        assert_eq!(v.triple, "aarch64-apple-visionos");
        assert_eq!(v.label, "visionos");
        assert_eq!(v.mode, "xr");
        assert_eq!(
            v.features,
            vec!["visionos-immersive".to_string()]
        );
    }

    #[test]
    fn standalone_bundled_targets_do_not_support_loose_levels() {
        // Quest `.apk` / Vision Pro `.app` bundle a single stage resource, so a
        // loose cook would hand signing/assembly a nonexistent file — the cook
        // force-flattens for them.
        assert!(!CookTarget::quest("aarch64-linux-android").supports_loose_levels());
        assert!(!CookTarget::vision_pro("aarch64-apple-visionos").supports_loose_levels());
        // Non-bundled targets run from the cooked out_dir directly and support it.
        assert!(CookTarget::pcvr("x86_64-pc-windows-msvc").supports_loose_levels());
        assert!(CookTarget::pcvr_linux("x86_64-unknown-linux-gnu").supports_loose_levels());
    }

    /// Unique scratch dir under the system temp dir (no `tempfile` dep).
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-cook-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Regression (cooked network/login failure): a project that enables network
    /// egress MUST pull the `remote` runtime feature into the cook's required
    /// features, so the packager (`stage_payload`) rejects a selected runtime
    /// binary that cannot honor `[network]` — one with no behavior `net` HTTP
    /// import and the SSRF policy stuck at deny-by-default. A project with egress
    /// disabled must NOT spuriously require `remote`.
    ///
    /// Guards the derivation in [`CookPlan::cooked_features`]; revert that `if
    /// self.network.allow` push and this test fails. Pure: exercises
    /// `from_project` + `cooked_features` only, so it links no USD.
    #[test]
    fn network_enabled_project_requires_remote_runtime_feature() {
        fn plan_for(allow: bool, tag: &str) -> CookPlan {
            let root = scratch(tag);
            std::fs::write(root.join("level.usda"), b"#usda 1.0\n").unwrap();
            let mut manifest = idtx_project::ProjectManifest::new("Net Remote");
            manifest.default_stage = Some(PathBuf::from("level.usda"));
            manifest.network.allow = allow;
            if allow {
                manifest.network.allowed_hosts = vec!["api.example.com".into()];
            }
            let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();
            let out = scratch(&format!("{tag}-out"));
            // Desktop target carries no features of its own, so this isolates the
            // network-derived requirement.
            CookPlan::from_project(&project, CookTarget::desktop("t"), &out).unwrap()
        }

        assert!(
            plan_for(true, "net-remote-on")
                .cooked_features()
                .contains(&"remote".to_string()),
            "a `[network] allow = true` project must require the `remote` runtime feature \
             so the packager cannot ship a runtime that can never honor it"
        );
        assert!(
            !plan_for(false, "net-remote-off")
                .cooked_features()
                .contains(&"remote".to_string()),
            "a project with egress disabled must not require `remote`"
        );
    }

    /// Security regression: runtime asset staging must NOT follow symlinks. A
    /// symlink placed inside a declared asset root (file or directory) could
    /// otherwise stage files from outside the root (`fs::copy` copies the link
    /// target's bytes; `Path::is_dir` recurses through a dir link into arbitrary
    /// locations or cycles). `fill_dir_if_absent` classifies via
    /// `DirEntry::file_type` and skips symlinks outright.
    #[cfg(unix)]
    #[test]
    fn fill_dir_if_absent_skips_symlinks() {
        use std::os::unix::fs::symlink;
        let root = scratch("symlink-skip");

        // Targets OUTSIDE the declared asset root that symlinks could escape to.
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        let outside_dir = root.join("outside_dir");
        std::fs::create_dir_all(outside_dir.join("nested")).unwrap();
        std::fs::write(outside_dir.join("nested").join("leak.txt"), b"leak").unwrap();

        // The declared asset root being staged: two real entries plus a file
        // symlink and a directory symlink, both pointing outside the root.
        let src = root.join("assets");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("real.txt"), b"ok").unwrap();
        std::fs::write(src.join("sub").join("child.txt"), b"ok").unwrap();
        symlink(outside.join("secret.txt"), src.join("link_file.txt")).unwrap();
        symlink(&outside_dir, src.join("link_dir")).unwrap();

        let dst = root.join("out");
        let exclude = root.join("does-not-exist");
        let mut copied = Vec::new();
        fill_dir_if_absent(&src, &dst, Path::new(""), &[exclude], &mut copied).unwrap();

        // Only the real files are staged; neither symlink is followed or copied.
        let mut got: Vec<String> =
            copied.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        got.sort();
        assert_eq!(got, vec!["real.txt".to_owned(), "sub/child.txt".to_owned()]);
        assert!(dst.join("real.txt").is_file());
        assert!(dst.join("sub").join("child.txt").is_file());
        assert!(
            !dst.join("link_file.txt").exists(),
            "a file symlink must not be followed or copied into the payload"
        );
        assert!(
            !dst.join("link_dir").exists(),
            "a directory symlink must not be recursed into or copied"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Full per-level cook (Scenario 2): a 2-level project with a start level, a
    /// transfer level, a shared global sublayer, and a project player cooks each
    /// level to a self-contained flattened `levels/<id>/level.usdz` (the per-level
    /// assembly composed over the loose body/player/globals, then flattened), and
    /// emits a multi-level `run.toml` (no single `stage.usdz`). No behaviors, so
    /// the `level_behavior_prepare` hook is unset and each level flattens bare.
    /// Links `OpenUSD`, so it needs `IDTX_USD_DIR`.
    #[cfg(feature = "cooking")]
    #[test]
    fn cook_per_level_assemblies_for_multi_level_project() {
        let root = scratch("perlevel-proj");
        let stages = root.join("stages");
        std::fs::create_dir_all(&stages).unwrap();
        // forest: a body with a root StageBehavior + physicsScene to exercise the
        // re-surface paths, plus /World content.
        std::fs::write(
            stages.join("forest.usda"),
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n    \
             customLayerData = {\n        string \"idtx:stageBehavior\" = \"StageBehavior\"\n    }\n)\n\
             def Scope \"StageBehavior\" (\n    prepend apiSchemas = [\"IdtxBehaviorAPI\"]\n)\n{\n}\n\
             def PhysicsScene \"physicsScene\" {}\n\
             def Xform \"World\"\n{\n    def Cube \"Box\" {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            stages.join("cave.usda"),
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\ndef Xform \"World\"\n{\n    def Sphere \"Orb\" {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            stages.join("_transfer.usda"),
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\ndef Xform \"World\"\n{\n    def Cube \"Loader\" {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            stages.join("globals.usda"),
            "#usda 1.0\ndef Xform \"Prototypes\"\n{\n    def Xform \"Tree\" {}\n}\n",
        )
        .unwrap();
        // A player with a playerKind variantSet so the desktop variant resolves.
        std::fs::write(
            stages.join("player.usda"),
            "#usda 1.0\n(\n    defaultPrim = \"Player\"\n)\n\
             def Xform \"Player\" (\n    prepend variantSets = \"playerKind\"\n    \
             variants = {\n        string playerKind = \"desktop\"\n    }\n)\n{\n    \
             variantSet \"playerKind\" = {\n        \
             \"desktop\" {\n            \
             def Xform \"Head\"\n            {\n                def Camera \"Camera\" {}\n            }\n        }\n        \
             \"xr\" {\n            \
             def Xform \"Head\"\n            {\n                def Camera \"Camera\" {}\n            }\n        }\n    }\n}\n",
        )
        .unwrap();

        let mut manifest = idtx_project::ProjectManifest::new("Per Level");
        manifest.default_stage = Some(PathBuf::from("stages/forest.usda"));
        manifest.levels = vec![
            PathBuf::from("stages/forest.usda"),
            PathBuf::from("stages/cave.usda"),
        ];
        manifest.start_level = Some(PathBuf::from("stages/cave.usda")); // start != first
        manifest.transfer_level = Some(PathBuf::from("stages/_transfer.usda"));
        manifest.shared_sublayers = vec![PathBuf::from("stages/globals.usda")];
        manifest.player_stage = Some(PathBuf::from("stages/player.usda"));
        let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();

        // An input map at the project root must be co-located into every per-level
        // dir (the payload carries no project.toml, so the runtime resolves each
        // level's project root to its own dir and looks for input_map.toml there).
        std::fs::write(
            root.join(PAYLOAD_INPUT_MAP),
            "[actions.jump]\nkind = \"button\"\nsources = [\"Keyboard/Space\"]\n",
        )
        .unwrap();

        let out_dir = scratch("perlevel-payload");
        let plan =
            CookPlan::from_project(&project, CookTarget::desktop("t"), &out_dir).unwrap();
        let report = cook(&plan).unwrap();

        // The cooked tree: one self-contained flattened `level.usdz` per level +
        // the transfer level, each under its own `<id>` directory.
        for rel in [
            "levels/forest/level.usdz",
            "levels/cave/level.usdz",
            "transfer/level.usdz",
        ] {
            assert!(out_dir.join(rel).is_file(), "missing cooked file: {rel}");
        }
        // The project's input_map.toml is co-located in each level dir (and the
        // transfer dir) so the runtime's per-level project-root fallback finds it.
        for dir in ["levels/forest", "levels/cave", "transfer"] {
            assert!(
                out_dir.join(dir).join(PAYLOAD_INPUT_MAP).is_file(),
                "input_map.toml missing from {dir}"
            );
        }
        // Per-level mode does NOT flatten a single stage.usdz, and the scratch
        // compose tree is cleaned up (no leaked `.idtx-perlevel-*`).
        assert!(
            !out_dir.join(PAYLOAD_STAGE).exists(),
            "per-level cook must not emit a combined stage.usdz"
        );
        assert!(
            std::fs::read_dir(&out_dir)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(".idtx-perlevel-")),
            "per-level scratch dir must be cleaned up"
        );
        // CookReport.stage points at the start level's flattened assembly.
        assert_eq!(report.stage, out_dir.join("levels/cave/level.usdz"));
        assert!(!report.content_hash.is_empty());

        // run.toml carries the multi-level entry set (as `.usdz`), no single stage.
        let emitted = RuntimeManifest::load(&report.manifest).unwrap();
        assert_eq!(emitted.experience.stage, None, "no single-stage entry");
        assert_eq!(
            emitted.experience.levels,
            vec![
                PathBuf::from("levels/forest/level.usdz"),
                PathBuf::from("levels/cave/level.usdz"),
            ]
        );
        assert_eq!(
            emitted.experience.start_level.as_deref(),
            Some(Path::new("levels/cave/level.usdz")),
            "start_level resolves to the configured cave level"
        );
        assert_eq!(
            emitted.experience.transfer_level.as_deref(),
            Some(Path::new("transfer/level.usdz"))
        );

        // The start level's flattened assembly re-composes: body re-rooted under
        // /World/level, player authority seam resolved, and the shared prototype
        // present (localized into the self-contained usdz).
        let mut stage =
            idtx_usd::StageService::open_topology_only(&out_dir.join("levels/cave/level.usdz"))
                .expect("re-open cooked start assembly");
        let names: Vec<String> = stage
            .project_topology()
            .expect("project topology")
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert!(names.contains(&"Orb".to_string()), "body content: {names:?}");
        assert!(names.contains(&"Tree".to_string()), "shared prototype: {names:?}");
        assert!(stage.prim_at_path("/World/level/Orb").is_some());
        assert!(stage.prim_at_path("/World/Player").is_some());

        // Reproducibility: a second cook of the same project yields the same hash.
        let out_dir2 = scratch("perlevel-payload2");
        let plan2 =
            CookPlan::from_project(&project, CookTarget::desktop("t"), &out_dir2).unwrap();
        let report2 = cook(&plan2).unwrap();
        assert_eq!(
            report.content_hash, report2.content_hash,
            "per-level cook must be reproducible"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&out_dir);
        let _ = std::fs::remove_dir_all(&out_dir2);
    }

    fn wasm_behavior(abi: i32, extra_hook: bool) -> Vec<u8> {
        let hook = if extra_hook {
            "(func (export \"_idtx_on_update\"))"
        } else {
            ""
        };
        let wat = format!(
            "(module (func (export \"_idtx_abi_version\") (result i32) i32.const {abi}) {hook})"
        );
        wat::parse_str(&wat).unwrap()
    }

    // Exercises the full cook: real USD stage localization + wasm compile. Links
    // OpenUSD, so it needs `IDTX_USD_DIR` set and the USD plugin folder staged
    // next to the test exe (see CLAUDE.md).
    #[cfg(windows)]
    #[test]
    fn cook_emits_stamped_payload() {
        let root = scratch("proj");
        // Minimal authored entry stage inside the project sandbox.
        std::fs::write(
            root.join("level.usda"),
            b"#usda 1.0\n(\n    defaultPrim = \"root\"\n)\ndef Xform \"root\"\n{\n    def Sphere \"ball\" {}\n}\n",
        )
        .unwrap();
        // A behavior module dropped in the implicit asset root (the project root).
        std::fs::write(root.join("spin.wasm"), wasm_behavior(2, true)).unwrap();
        // The conventional `generic` controller meshes, so the default XR config
        // resolves and folds into the payload without authoring a sidecar.
        std::fs::create_dir_all(root.join("controllers")).unwrap();
        std::fs::write(root.join("controllers/generic_left.usda"), b"#usda 1.0\n").unwrap();
        std::fs::write(root.join("controllers/generic_right.usda"), b"#usda 1.0\n").unwrap();

        let mut manifest = idtx_project::ProjectManifest::new("Cook Demo");
        manifest.default_stage = Some(PathBuf::from("level.usda"));
        manifest.network.allow = true;
        manifest.network.allowed_hosts = vec!["assets.internal.example".into()];
        manifest.network.private_hosts = vec!["assets.internal.example".into()];
        manifest.network.max_asset_mb = 64;
        let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();

        let out_dir = scratch("payload");
        let plan = CookPlan::from_project(
            &project,
            CookTarget::pcvr("x86_64-pc-windows-msvc"),
            &out_dir,
        )
        .unwrap();
        let report = cook(&plan).unwrap();

        // Stage was flattened to a self-contained USDZ.
        assert!(report.stage.is_file(), "stage.usdz emitted");
        assert_eq!(report.stage.file_name().unwrap(), PAYLOAD_STAGE);
        assert!(report.unresolved.is_empty(), "no unresolved refs");

        // The behavior was collected + validated.
        assert_eq!(report.behaviors.len(), 1);
        let b = &report.behaviors[0];
        assert!(b.has_abi_marker);
        assert_eq!(b.hooks, vec!["_idtx_on_update".to_string()]);
        assert!(out_dir.join(&b.payload_rel).is_file());

        // The emitted manifest is schema v2 and carries the stamp.
        let emitted = RuntimeManifest::load(&report.manifest).unwrap();
        assert_eq!(emitted.version, 2);
        assert_eq!(emitted.network.private_hosts, ["assets.internal.example"]);
        assert!(!emitted.network.asset_policy().is_denied());
        assert_eq!(emitted.network.asset_policy().max_response_bytes(), 64 * 1024 * 1024);
        assert_eq!(
            emitted.experience.stage.as_deref(),
            Some(Path::new(PAYLOAD_STAGE))
        );
        assert_eq!(emitted.experience.mode.as_deref(), Some("xr"));
        // An XR cook leaves fullscreen unset (omitted) → windowed default downstream.
        assert_eq!(emitted.experience.fullscreen, None);
        assert_eq!(emitted.pins.abi_version, Some(idtx_behavior::ABI_VERSION));
        assert_eq!(emitted.pins.wgpu.as_deref(), Some(PIN_WGPU));
        assert_eq!(
            emitted.stamp.content_hash.as_deref(),
            Some(report.content_hash.as_str())
        );
        assert_eq!(emitted.stamp.target.as_deref(), Some("pcvr"));

        // The default `generic` controller meshes folded into `[xr]` with
        // payload-relative paths, and the meshes were copied under `assets/`.
        let generic = emitted
            .xr
            .controllers
            .get("generic")
            .expect("generic controllers folded into run.toml");
        assert_eq!(
            generic.left.as_deref(),
            Some(Path::new("controllers/generic_left.usda"))
        );
        assert_eq!(
            generic.right.as_deref(),
            Some(Path::new("controllers/generic_right.usda"))
        );
        assert!(
            out_dir
                .join(PAYLOAD_ASSETS_DIR)
                .join("controllers/generic_left.usda")
                .is_file(),
            "left controller mesh copied into payload assets"
        );
        assert!(
            out_dir
                .join(PAYLOAD_ASSETS_DIR)
                .join("controllers/generic_right.usda")
                .is_file(),
            "right controller mesh copied into payload assets"
        );
        assert_eq!(report.controllers.len(), 2, "two controller meshes copied");

        // The project's input_map.toml was staged at the payload root (so the
        // packaged runtime's `load_input_map` resolves it — the project tree is
        // not in the payload). Written below before the second cook so both runs
        // see it; assert the first run already staged it.
        //
        // The IDTX schema plugin resources were staged under `usd-schemas/` so
        // the on-device runtime can register `IdtxCollisionAPI` (its compile-time
        // baked plugin dir is a build-host path absent on a device).
        assert!(
            out_dir.join(PAYLOAD_SCHEMA_DIR).join("plugInfo.json").is_file(),
            "schema plugInfo.json staged into payload"
        );
        assert!(
            out_dir
                .join(PAYLOAD_SCHEMA_DIR)
                .join("generatedSchema.usda")
                .is_file(),
            "schema generatedSchema.usda staged into payload"
        );

        // Author an input_map.toml at the project root and re-cook: it must land
        // at the payload root verbatim.
        std::fs::write(
            root.join(PAYLOAD_INPUT_MAP),
            b"[actions.move]\nkind = \"axis2\"\nbindings = [\"Controller/MetaQuest/LeftThumbstick\"]\n",
        )
        .unwrap();

        // Adding an input map changes the payload and therefore its stamp.
        let out_dir2 = scratch("payload2");
        let plan2 = CookPlan::from_project(
            &project,
            CookTarget::pcvr("x86_64-pc-windows-msvc"),
            &out_dir2,
        )
        .unwrap();
        let report2 = cook(&plan2).unwrap();
        assert_ne!(report.content_hash, report2.content_hash);
        // The staged input map is the project's, byte-for-byte.
        let staged_map = out_dir2.join(PAYLOAD_INPUT_MAP);
        assert!(staged_map.is_file(), "input_map.toml staged into payload");
        assert_eq!(
            std::fs::read(&staged_map).unwrap(),
            std::fs::read(root.join(PAYLOAD_INPUT_MAP)).unwrap()
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&out_dir);
        let _ = std::fs::remove_dir_all(&out_dir2);
    }

    /// Loose-levels cook: the entry's dependency closure is staged as a loose
    /// directory tree under the payload root (not flattened into one
    /// `stage.usdz`), so a large referenced scene keeps its original files and
    /// composition/instancing. Links `OpenUSD` (needs `IDTX_USD_DIR` + staged
    /// plugins). `player` is cleared so the closure stays under the project root
    /// (the built-in player lives under a different tree).
    #[test]
    fn cook_loose_levels_stages_tree_instead_of_usdz() {
        let root = scratch("loose-proj");
        std::fs::create_dir_all(root.join("stages")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        // A sub-asset with a distinctly-named mesh, referenced by the level.
        std::fs::write(
            root.join("assets/widget.usda"),
            b"#usda 1.0\n(\n    defaultPrim = \"Widget\"\n)\ndef Xform \"Widget\"\n{\n    def Mesh \"LooseMesh\" {}\n}\n",
        )
        .unwrap();
        // The default stage references the sub-asset via a relative path.
        std::fs::write(
            root.join("stages/level.usda"),
            b"#usda 1.0\n(\n    defaultPrim = \"root\"\n)\ndef Xform \"root\"\n{\n    def \"w\" (\n        prepend references = @../assets/widget.usda@\n    )\n    {\n    }\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("controllers")).unwrap();
        std::fs::write(root.join("controllers/generic_left.usda"), b"#usda 1.0\n").unwrap();
        std::fs::write(root.join("controllers/generic_right.usda"), b"#usda 1.0\n").unwrap();

        let mut manifest = idtx_project::ProjectManifest::new("Loose Demo");
        manifest.default_stage = Some(PathBuf::from("stages/level.usda"));
        // Drive the loose path through the project config (not the CookPlan field)
        // so this also covers the manifest -> CookPlan::from_project threading.
        manifest.build.loose_levels = true;
        let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();

        let out_dir = scratch("loose-payload");
        let mut plan = CookPlan::from_project(
            &project,
            CookTarget::pcvr("x86_64-pc-windows-msvc"),
            &out_dir,
        )
        .unwrap();
        assert!(plan.loose_levels, "from_project read build.loose_levels");
        plan.player = None;

        let report = cook(&plan).unwrap();

        // Staged LOOSE, not flattened to stage.usdz.
        assert!(report.stage.is_file(), "loose entry staged");
        assert_ne!(
            report.stage.extension().and_then(|e| e.to_str()),
            Some("usdz"),
            "loose mode must not emit a .usdz: {}",
            report.stage.display()
        );
        assert!(report.unresolved.is_empty(), "nothing unresolved");

        // The referenced sub-asset is staged verbatim under the payload root.
        assert!(
            out_dir.join("assets/widget.usda").is_file(),
            "referenced asset staged verbatim"
        );

        // The manifest points the runtime at the staged loose entry (relative).
        let emitted = RuntimeManifest::load(&report.manifest).unwrap();
        let stage_rel = emitted.experience.stage.expect("experience.stage");
        assert!(
            !stage_rel.ends_with("stage.usdz"),
            "experience.stage is the loose entry, not stage.usdz: {}",
            stage_rel.display()
        );
        assert_eq!(out_dir.join(&stage_rel), report.stage);

        // Reopening the staged entry resolves the referenced mesh — composition
        // and references preserved by the verbatim copy.
        let mut staged = idtx_usd::StageService::open(&report.stage).unwrap();
        let recs = staged.project_topology().unwrap();
        assert!(
            recs.iter().any(|r| r.type_name == "Mesh" && r.name == "LooseMesh"),
            "referenced mesh resolves in the loose-staged payload; got {} prims",
            recs.len()
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    /// Flatten cook (visionOS target → force-flatten path) keeps REMOTE
    /// (`http(s)://`) references EXTERNAL: the cook must NOT try to fetch + embed
    /// them (which 404s and aborts the pack); the authored URL stays referenced
    /// in the produced `stage.usdz` and is reported in `report.external`, while a
    /// local reference is packed normally. Links `OpenUSD` (needs `IDTX_USD_DIR` +
    /// staged plugins). No HTTP resolver is installed in the test process, so the
    /// remote URL is unresolvable — the same condition packaging mode creates —
    /// which is exactly what keeps it external. Asserts at the dependency level
    /// (never resolves the remote).
    #[test]
    fn cook_flatten_keeps_remote_refs_external() {
        const REMOTE_URL: &str =
            "https://idtx-core.example.invalid/api/v1/download/Datacenter/remote.usda";

        let root = scratch("remote-proj");
        std::fs::create_dir_all(root.join("stages")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        // A LOCAL sub-asset that must still be packed into stage.usdz.
        std::fs::write(
            root.join("assets/widget.usda"),
            b"#usda 1.0\n(\n    defaultPrim = \"Widget\"\n)\ndef Xform \"Widget\"\n{\n    def Mesh \"LocalMesh\" {}\n}\n",
        )
        .unwrap();
        // The entry stage references BOTH a local asset (relative) and a remote
        // asset (absolute http URL on an unreachable host).
        let entry = format!(
            "#usda 1.0\n(\n    defaultPrim = \"root\"\n)\ndef Xform \"root\"\n{{\n    def \"local\" (\n        prepend references = @../assets/widget.usda@\n    )\n    {{\n    }}\n    def \"remote\" (\n        prepend references = @{REMOTE_URL}@\n    )\n    {{\n    }}\n}}\n"
        );
        std::fs::write(root.join("stages/level.usda"), entry.as_bytes()).unwrap();
        std::fs::create_dir_all(root.join("controllers")).unwrap();
        std::fs::write(root.join("controllers/generic_left.usda"), b"#usda 1.0\n").unwrap();
        std::fs::write(root.join("controllers/generic_right.usda"), b"#usda 1.0\n").unwrap();

        let mut manifest = idtx_project::ProjectManifest::new("Remote Demo");
        manifest.default_stage = Some(PathBuf::from("stages/level.usda"));
        let project = idtx_project::IdtxProject::create(&root, manifest).unwrap();

        let out_dir = scratch("remote-payload");
        // vision_pro ⟹ supports_loose_levels() == false ⟹ the force-flatten path
        // (the exact path that 404'd on remote refs before this fix).
        let mut plan = CookPlan::from_project(
            &project,
            CookTarget::vision_pro("aarch64-apple-visionos"),
            &out_dir,
        )
        .unwrap();
        assert!(!plan.loose_levels, "flatten path (loose off)");
        plan.player = None;

        // Must NOT error with a fetch/404 abort.
        let report = cook(&plan).expect("cook succeeds without fetching the remote ref");

        // Flattened to stage.usdz.
        assert_eq!(
            report.stage.file_name().and_then(|n| n.to_str()),
            Some(PAYLOAD_STAGE),
            "flatten path emits stage.usdz: {}",
            report.stage.display()
        );
        assert!(report.stage.is_file(), "stage.usdz emitted");

        // The remote URL is reported as kept-external, not packed.
        assert!(
            report.external.iter().any(|u| u.contains("remote.usda")),
            "remote URL reported external; got {:?}",
            report.external
        );

        // The usdz stores files uncompressed → the authored URL survives verbatim
        // (byte-for-byte), the local asset is embedded, and the remote ref was NOT
        // rewritten into an internal bucket.
        let bytes = std::fs::read(&report.stage).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains(REMOTE_URL),
            "authored remote URL must survive verbatim in stage.usdz"
        );
        assert!(
            !text.contains("0/remote.usda") && !text.contains("1/remote.usda"),
            "remote ref must NOT be remapped into an internal package bucket"
        );
        assert!(
            text.contains("LocalMesh"),
            "local asset content must be embedded in stage.usdz"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    /// `stage_schema_resources` copies the IDTX schema plugin resources from the
    /// crate's baked plugin dir into the payload's `usd-schemas/`. Pure (no USD
    /// link): the baked `plugInfo.json` + `generatedSchema.usda` exist on any
    /// build host, which is exactly the invariant the cook relies on.
    #[test]
    fn stage_schema_resources_copies_pluginfo_and_generated_schema() {
        let dest = scratch("schema-stage").join(PAYLOAD_SCHEMA_DIR);
        stage_schema_resources(&dest).unwrap();
        assert!(
            dest.join("plugInfo.json").is_file(),
            "plugInfo.json staged"
        );
        assert!(
            dest.join("generatedSchema.usda").is_file(),
            "generatedSchema.usda staged"
        );
        // The staged plugInfo matches the crate's baked source byte-for-byte.
        let src = idtx_usd_extension::plugin_dir();
        assert_eq!(
            std::fs::read(dest.join("plugInfo.json")).unwrap(),
            std::fs::read(src.join("plugInfo.json")).unwrap()
        );
        let _ = std::fs::remove_dir_all(dest.parent().unwrap());
    }

    /// `stage_exec_resources` copies the coded `OpenExec` plugin's resources (and the
    /// host-native library) from the crate's baked plugin dir into the payload's
    /// `usd-exec/` for a desktop target. The baked artifacts exist on any build
    /// host (guarded by the exec crate's own `baked_plugin_dir_has_library_and_resources`).
    #[cfg(feature = "openexec")]
    #[test]
    fn stage_exec_resources_copies_plugin_for_desktop() {
        let dest = scratch("exec-stage").join(PAYLOAD_EXEC_DIR);
        let target = CookTarget::desktop("x86_64-pc-windows-msvc");
        stage_exec_resources(&dest, &target).unwrap();
        assert!(dest.join("plugInfo.json").is_file(), "plugInfo.json staged");
        assert!(
            dest.join("generatedSchema.usda").is_file(),
            "generatedSchema.usda staged"
        );
        // Exactly one of the three host-library names landed beside the resources.
        let libs = ["idtx_usd_exec.dll", "libidtx_usd_exec.dylib", "libidtx_usd_exec.so"]
            .iter()
            .filter(|n| dest.join(n).is_file())
            .count();
        assert_eq!(libs, 1, "exactly the host plugin library staged");
        let _ = std::fs::remove_dir_all(dest.parent().unwrap());
    }

    /// A standalone target gets NO host plugin staged — its native plugin is
    /// cross-built and provided by the runtime cross-build + packager, so the
    /// (wrong-ABI) host DLL must not land in the payload.
    #[cfg(feature = "openexec")]
    #[test]
    fn stage_exec_resources_skips_standalone() {
        let dest = scratch("exec-stage-quest").join(PAYLOAD_EXEC_DIR);
        let target = CookTarget::quest("aarch64-linux-android");
        stage_exec_resources(&dest, &target).unwrap();
        assert!(
            !dest.join("plugInfo.json").is_file(),
            "standalone target must not stage the host OpenExec plugin"
        );
        let _ = std::fs::remove_dir_all(dest.parent().unwrap());
    }

    #[test]
    fn cook_rejects_corrupt_behavior() {
        let engine = Engine::default();
        let roots_dir = scratch("badwasm");
        std::fs::write(roots_dir.join("broken.wasm"), b"not a real wasm module").unwrap();
        let behaviors_dir = roots_dir.join("out");
        let err = collect_behaviors(&engine, std::slice::from_ref(&roots_dir), &behaviors_dir, &[])
            .unwrap_err();
        assert!(matches!(err, CookError::Behavior { .. }));
        let _ = std::fs::remove_dir_all(&roots_dir);
    }

    #[test]
    fn collect_behaviors_emits_artifact_and_skips_cache_dir() {
        let engine = Engine::default();
        let roots_dir = scratch("behmanifest");

        // The emitted artifact (capability config now lives on the stage's
        // IdtxBehaviorAPI schema attrs — no sidecar `idtx.toml`).
        let wasm = wasm_behavior(2, false);
        std::fs::write(roots_dir.join("foo.wasm"), &wasm).unwrap();

        // A decoy copy under a hidden build-scratch dir with IDENTICAL bytes.
        // It sorts before `foo.wasm` and would win content-dedup — becoming the
        // emitted `source` — unless the dot-dir is skipped.
        let cache = roots_dir.join(".behavior-cache/abc/target/wasm32-unknown-unknown/release");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("foo.wasm"), &wasm).unwrap();

        let behaviors_dir = roots_dir.join("out");
        std::fs::create_dir_all(&behaviors_dir).unwrap();
        let artifacts =
            collect_behaviors(&engine, std::slice::from_ref(&roots_dir), &behaviors_dir, &[]).unwrap();

        // Exactly one module, emitted from the real artifact (not the cache copy).
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].source, roots_dir.join("foo.wasm"));
        assert!(behaviors_dir.join("foo.wasm").is_file(), "wasm in payload");

        let _ = std::fs::remove_dir_all(&roots_dir);
    }

    // ── controller-mesh copy + XR-config rewrite (no USD linkage) ──────

    fn controllers_config() -> XrConfig {
        use idtx_runtime_manifest::MountConfig;
        let mut controllers = std::collections::BTreeMap::new();
        // Per-hand device with a mount offset.
        controllers.insert(
            "quest3".to_string(),
            ControllerAsset {
                left: Some(PathBuf::from("controllers/quest3_left.usda")),
                right: Some(PathBuf::from("controllers/quest3_right.usda")),
                asset: None,
                mount: Some(MountConfig {
                    rotate: Some([180.0, 0.0, 0.0]),
                    translate: None,
                    scale: None,
                }),
            },
        );
        // Shared single-mesh device (both hands from one asset).
        controllers.insert(
            "generic".to_string(),
            ControllerAsset {
                left: None,
                right: None,
                asset: Some(PathBuf::from("controllers/generic.usda")),
                mount: None,
            },
        );
        // A device whose mesh does not exist in the roots → dropped entirely.
        controllers.insert(
            "phantom".to_string(),
            ControllerAsset {
                left: Some(PathBuf::from("controllers/missing.usda")),
                right: Some(PathBuf::from("controllers/missing.usda")),
                asset: None,
                mount: None,
            },
        );
        XrConfig { controllers }
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact mount literals carried through, bit-exact
    fn cook_xr_controllers_copies_meshes_and_rewrites_paths() {
        let root = scratch("xrctl");
        let ctl = root.join("controllers");
        std::fs::create_dir_all(&ctl).unwrap();
        for f in ["quest3_left.usda", "quest3_right.usda", "generic.usda"] {
            std::fs::write(ctl.join(f), b"#usda 1.0\n").unwrap();
        }

        let assets_dir = scratch("xrctl-out").join("assets");
        let (xr, copied) = cook_xr_controllers(
            &controllers_config(),
            std::slice::from_ref(&root),
            &assets_dir,
        )
        .unwrap();

        // The unresolvable `phantom` device is omitted; the two resolvable ones remain.
        assert!(
            !xr.controllers.contains_key("phantom"),
            "unresolved device dropped"
        );
        assert!(xr.controllers.contains_key("quest3"));
        assert!(xr.controllers.contains_key("generic"));

        // quest3 paths rewritten to payload-relative and the mount preserved.
        let q = &xr.controllers["quest3"];
        assert_eq!(
            q.left.as_deref(),
            Some(Path::new("controllers/quest3_left.usda"))
        );
        assert_eq!(
            q.right.as_deref(),
            Some(Path::new("controllers/quest3_right.usda"))
        );
        assert_eq!(q.mount.unwrap().rotate(), [180.0, 0.0, 0.0]);

        // Shared-mesh device: the single `asset` path rewritten, copied once.
        let g = &xr.controllers["generic"];
        assert_eq!(
            g.asset.as_deref(),
            Some(Path::new("controllers/generic.usda"))
        );

        // Every rewritten mesh exists in the payload assets dir.
        for f in ["quest3_left.usda", "quest3_right.usda", "generic.usda"] {
            assert!(
                assets_dir.join("controllers").join(f).is_file(),
                "copied mesh missing: {f}"
            );
        }
        // `copied` lists exactly the three payload-relative meshes, sorted.
        assert_eq!(
            copied,
            vec![
                PathBuf::from("controllers/generic.usda"),
                PathBuf::from("controllers/quest3_left.usda"),
                PathBuf::from("controllers/quest3_right.usda"),
            ]
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(assets_dir.parent().unwrap());
    }

    #[test]
    fn cook_xr_controllers_empty_when_nothing_resolves() {
        let root = scratch("xrctl-empty");
        let assets_dir = root.join("payload/assets");
        let (xr, copied) = cook_xr_controllers(
            &controllers_config(),
            std::slice::from_ref(&root),
            &assets_dir,
        )
        .unwrap();
        assert!(
            xr.controllers.is_empty(),
            "no device resolves → empty config"
        );
        assert!(copied.is_empty());
        // Nothing to copy → the controllers dir is never created.
        assert!(!assets_dir.join("controllers").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cook_xr_controllers_folds_builtin_meshes_without_project_copies() {
        // The core of the Quest/PCVR controller fix: a project that ships NO local
        // controller meshes must still fold the toolbox's built-in meshes into the
        // payload. Resolve against `idtx_assets::builtin_tree()` (the dir that
        // contains `controllers/`) exactly as `cook()` does, with an EMPTY project
        // root first, and assert the shipped meshes are copied + paths rewritten.
        use idtx_runtime_manifest::MountConfig;

        let empty_project = scratch("xrctl-noproject");
        let builtin = idtx_assets::builtin_tree();
        assert!(
            builtin.join("controllers").is_dir(),
            "test precondition: built-in controllers dir must ship at {builtin:?}"
        );

        let mut controllers = std::collections::BTreeMap::new();
        controllers.insert(
            "generic".to_string(),
            ControllerAsset {
                left: Some(PathBuf::from("controllers/generic_left.usda")),
                right: Some(PathBuf::from("controllers/generic_right.usda")),
                asset: None,
                mount: None,
            },
        );
        controllers.insert(
            "questpro".to_string(),
            ControllerAsset {
                left: Some(PathBuf::from("controllers/questpro_left.usdz")),
                right: Some(PathBuf::from("controllers/questpro_right.usdz")),
                asset: None,
                mount: Some(MountConfig {
                    rotate: Some([-60.0, 0.0, 0.0]),
                    translate: None,
                    scale: None,
                }),
            },
        );
        let xr = XrConfig { controllers };

        let assets_dir = scratch("xrctl-noproject-out").join("assets");
        // Project roots first (empty here), then the built-in tree fallback — the
        // exact root order `cook()` builds.
        let roots = vec![empty_project.clone(), builtin];
        let (out, copied) = cook_xr_controllers(&xr, &roots, &assets_dir).unwrap();

        // Both devices resolve from the built-in tree even though the project
        // carries nothing.
        assert!(
            out.controllers.contains_key("generic"),
            "generic resolved from the built-in tree"
        );
        assert!(
            out.controllers.contains_key("questpro"),
            "questpro resolved from the built-in tree"
        );
        for f in [
            "generic_left.usda",
            "generic_right.usda",
            "questpro_left.usdz",
            "questpro_right.usdz",
        ] {
            assert!(
                assets_dir.join("controllers").join(f).is_file(),
                "built-in mesh not copied into payload: {f}"
            );
        }
        assert!(copied.contains(&PathBuf::from("controllers/generic_left.usda")));
        assert!(copied.contains(&PathBuf::from("controllers/questpro_left.usdz")));

        let _ = std::fs::remove_dir_all(&empty_project);
        let _ = std::fs::remove_dir_all(assets_dir.parent().unwrap());
    }

    #[test]
    fn builtin_dependency_classified_by_tree_not_filename() {
        let root = scratch("builtin-class");
        let builtin = root.join("built-in");
        let project = root.join("project");
        std::fs::create_dir_all(&builtin).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        // The real caller passes an already-canonicalized built-in root (computed
        // once); canonicalize here too so the comparison is prefix-consistent on
        // Windows (`\\?\` verbatim paths).
        let builtin_root = std::fs::canonicalize(&builtin).unwrap();

        // A real dependency under the built-in tree → built-in (no warning).
        let under_builtin = builtin.join("Player.usd");
        std::fs::write(&under_builtin, b"#usda 1.0\n").unwrap();
        assert!(is_builtin_dependency(&under_builtin, Some(&builtin_root)));

        // A project asset whose NAME contains "Player"/"Rig" but lives OUTSIDE the
        // built-in tree → NOT built-in, so its dangling warning is preserved (the
        // regression the old filename heuristic caused).
        let decoy = project.join("PlayerStatue.usd");
        std::fs::write(&decoy, b"#usda 1.0\n").unwrap();
        assert!(!is_builtin_dependency(&decoy, Some(&builtin_root)));
        let rig = project.join("OilRig.usd");
        std::fs::write(&rig, b"#usda 1.0\n").unwrap();
        assert!(!is_builtin_dependency(&rig, Some(&builtin_root)));

        // No built-in root available → nothing is classified as built-in.
        assert!(!is_builtin_dependency(&under_builtin, None));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scratch_assembly_relocated_to_fixed_reproducible_path() {
        let root = scratch("relocate");
        let payload = root.join("payload");
        // Simulate a staged cook with a per-run scratch assembly dir name.
        let scratch_dir = format!("{COOK_ASSEMBLY_SCRATCH_PREFIX}12345-67890");
        std::fs::create_dir_all(payload.join(&scratch_dir)).unwrap();
        std::fs::write(payload.join(&scratch_dir).join("assembly.usda"), b"#usda 1.0\n")
            .unwrap();

        let staged_entry = PathBuf::from(&scratch_dir).join("assembly.usda");
        let rewritten = relocate_scratch_assembly(&payload, staged_entry).unwrap();

        // The per-run name is gone from both the directory layout and the entry
        // path — identical cooks now produce an identical run.toml stage path.
        assert_eq!(rewritten, Path::new(PAYLOAD_ASSEMBLY_DIR).join("assembly.usda"));
        assert!(payload.join(PAYLOAD_ASSEMBLY_DIR).join("assembly.usda").exists());
        assert!(!payload.join(&scratch_dir).exists());

        // A non-scratch entry (e.g. an entry_override staged at a real path) is
        // returned unchanged and nothing is moved.
        let plain = PathBuf::from("levels").join("level.usda");
        assert_eq!(
            relocate_scratch_assembly(&payload, plain.clone()).unwrap(),
            plain
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
