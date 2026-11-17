//! Shared versioned IDTX build and runtime manifest contract.
//!
//! The runtime manifest (`run.toml`) describes, per target, where the runtime
//! preview and the packaged player should find optional capability assets —
//! today the XR controller meshes that are mounted over the authored world.
//!
//! Asset *locations* are resolved through an ordered list of search roots, so
//! the same manifest works whether the meshes ship inside a packaged runtime
//! app, inside a self-contained `idtx-conductor` bundle, or are dropped in by a
//! vendor *after* packaging (via [`ENV_ASSETS`]). The manifest itself only names
//! relative asset paths and search roots; it never hard-codes an absolute
//! location, which is what keeps packaging and late-binding vendor overrides
//! flexible.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use idtx_net_guard::DEFAULT_MAX_ASSET_MB;

/// Highest manifest schema version this build understands.
///
/// * v1 — asset search roots + XR controller meshes only.
/// * v2 — adds the packaged [`ExperienceConfig`] entry (stage + default mode +
///   target metadata) and the cook-time [`PinsConfig`] (ABI / toolchain pins).
///   A v1 manifest still loads under a v2 build: the new sections default to
///   empty, so a hand-written v1 `run.toml` keeps working unchanged.
pub const MANIFEST_VERSION: u32 = 2;

/// Environment variable naming an explicit `run.toml` manifest file. Highest
/// priority during [`RuntimeManifest::discover`].
pub const ENV_MANIFEST: &str = "IDTX_RUNTIME_MANIFEST";

/// Environment variable naming a highest-priority asset search root. This is the
/// vendor / late-binding drop-in seam: a shipped app carries default assets in
/// its bundle, but a directory pointed at by this variable is consulted first,
/// so device-specific meshes unknown at packaging time still win.
pub const ENV_ASSETS: &str = "IDTX_RUNTIME_ASSETS";

/// Errors from loading a [`RuntimeManifest`] off disk.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid runtime prototype registration: {0}")]
    InvalidRuntimePrototype(String),
    #[error("invalid credential origins: {0}")]
    Auth(#[from] idtx_auth::AuthError),
    /// The manifest file could not be read.
    #[error("read runtime manifest {path}: {source}")]
    Io {
        /// The manifest path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The manifest file was not valid TOML for this schema.
    // TOML's Display can include the full failing source line, including a dev
    // token. Keep it available as the error source, not in routine diagnostics.
    #[error("invalid TOML in runtime manifest {path}")]
    Parse {
        /// The manifest path that failed to parse.
        path: PathBuf,
        /// The underlying TOML error.
        source: toml::de::Error,
    },
    /// The manifest declares a schema version newer than this build supports.
    #[error("unsupported runtime manifest version {found} (this build supports up to {supported})")]
    UnsupportedVersion {
        /// The version found in the manifest.
        found: u32,
        /// The highest version this build understands.
        supported: u32,
    },
}

/// A parsed runtime manifest. Constructed from a `run.toml` via
/// [`RuntimeManifest::discover`] / [`RuntimeManifest::load`], or synthesized
/// with [`RuntimeManifest::default`] when no file is present.
///
/// Not `Eq`: a mount offset carries `f64` components (see [`MountConfig`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeManifest {
    /// Schema version; see [`MANIFEST_VERSION`].
    #[serde(default = "default_version")]
    pub version: u32,
    /// Where to look for capability assets.
    #[serde(default)]
    pub assets: AssetConfig,
    /// XR-mode capability configuration.
    #[serde(default)]
    pub xr: XrConfig,
    /// Packaged experience entry — schema v2+. Names the entry stage and default
    /// run mode a packaged player should use when no CLI flag overrides them.
    /// Empty for a synthesized preview manifest (and for a v1 file), and then
    /// omitted from the serialized TOML.
    #[serde(default, skip_serializing_if = "ExperienceConfig::is_empty")]
    pub experience: ExperienceConfig,
    /// Toolchain / ABI pins captured at cook time — schema v2+. The packaged
    /// player checks [`PinsConfig::abi_version`] against its own behavior ABI
    /// before loading behaviors. Empty (and omitted) for a preview / v1 manifest.
    #[serde(default, skip_serializing_if = "PinsConfig::is_empty")]
    pub pins: PinsConfig,
    /// Reproducibility stamp baked in by the cook (schema v2+). Empty (and
    /// omitted) for a preview / v1 manifest.
    #[serde(default, skip_serializing_if = "BuildStamp::is_empty")]
    pub stamp: BuildStamp,
    /// Network capability for runtime assets, REST and behavior HTTP — schema
    /// v2+. Deny-by-default (empty + omitted): a REST datasource only polls when
    /// this grants it (see [`NetworkConfig`]). Keeps a preview / v1 `run.toml`
    /// clean when unset.
    #[serde(default, skip_serializing_if = "NetworkConfig::is_empty")]
    pub network: NetworkConfig,
    /// Behavior host per-instance work-budget overrides — schema v2+. Empty (and
    /// omitted) unless an experience tunes the caps (see [`BehaviorConfig`]).
    #[serde(default, skip_serializing_if = "BehaviorConfig::is_empty")]
    pub behavior: BehaviorConfig,
    /// Prepared spawn templates, addressed by stable IDs. Paths are relative
    /// to the manifest directory; preview can use absolute prepared paths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_prototypes: BTreeMap<String, RuntimePrototypeConfig>,
}

/// A reusable USD template. The referenced asset contains its own compiled
/// behavior binding; registration never creates a live scene entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePrototypeConfig {
    pub asset: PathBuf,
    pub prim: String,
}

impl RuntimePrototypeConfig {
    /// Validate a stable ID and a concrete absolute USD prim path.
    pub fn validate(&self, id: &str) -> Result<(), String> {
        fn identifier(value: &str) -> bool {
            let mut bytes = value.bytes();
            bytes.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
                && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        }
        if !identifier(id) { return Err("prototype ID must be an ASCII identifier".into()); }
        if self.asset.as_os_str().is_empty() { return Err("prototype asset is empty".into()); }
        if !self.prim.starts_with('/') || self.prim[1..].split('/').any(|part| !identifier(part)) {
            return Err("prototype prim must be an absolute prim path without properties or variants".into());
        }
        if self.asset.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
            return Err("prototype asset must not contain parent traversal".into());
        }
        Ok(())
    }
}

fn default_version() -> u32 {
    MANIFEST_VERSION
}

impl Default for RuntimeManifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            assets: AssetConfig::default(),
            xr: XrConfig::with_default_controllers(),
            experience: ExperienceConfig::default(),
            pins: PinsConfig::default(),
            stamp: BuildStamp::default(),
            network: NetworkConfig::default(),
            behavior: BehaviorConfig::default(),
            runtime_prototypes: BTreeMap::new(),
        }
    }
}

/// Asset-location configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetConfig {
    /// Ordered, strongest-first asset search roots. Relative controller-mesh
    /// paths are resolved against each root in turn; the first existing file
    /// wins. An `IDTX_RUNTIME_ASSETS` root (when set) is consulted before these,
    /// and an `assets/` dir next to the executable is consulted after them.
    #[serde(default)]
    pub search_paths: Vec<PathBuf>,
}

/// The packaged experience entry — added in schema **v2**.
///
/// Where v1 described only *where to find capability assets*, v2 records *what
/// to run*: the entry stage (relative to the bundle root, so the packaged app
/// stays relocatable) and the default run mode. A packaged player that is
/// launched with no `--stage` / `--mode` flags takes them from here; explicit
/// CLI flags always win. All fields are optional so a v1 manifest — which has no
/// `[experience]` table — parses to an empty (omitted) config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceConfig {
    /// Entry stage relative to the manifest / bundle root (e.g. `"stage.usdz"`).
    /// A packaged cook flattens + localizes the authored stage to this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<PathBuf>,
    /// Default run mode token (`"xr"` / `"desktop"`). Overridden by the runtime's
    /// `--mode` flag; see `idtx-runtime-core`'s `RunMode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Human-readable experience name (window title / logs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Target profile label this bundle was cooked for (e.g. `"windows-pcvr"`,
    /// `"macos-desktop"`). Metadata only — the runtime does not gate on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Whether a packaged **desktop** player should run borderless-fullscreen and
    /// capture the mouse cursor on launch (with ESC quitting). `None` for a
    /// preview / v1 manifest and for XR targets, where the runtime defaults to
    /// windowed. Overridden by the runtime's `--fullscreen` / `--windowed` flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    /// Per-level assembly entries for a **multi-level** experience, in logical
    /// order, each relative to the bundle root (e.g. `"levels/forest/level.usda"`).
    /// Empty for a single-stage experience that uses [`stage`](Self::stage). The
    /// transition target a behavior names is resolved against this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<PathBuf>,
    /// The per-level assembly the experience **starts on** (bundle-relative).
    /// Required when [`levels`](Self::levels) is non-empty; ignored for a
    /// single-stage experience.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_level: Option<PathBuf>,
    /// The transfer/transition assembly shown while the next level loads
    /// (bundle-relative). `None` means the swap is a hard cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_level: Option<PathBuf>,
    /// Texture-decode profile token for the packaged runtime — one of
    /// `"full_resolution"`, `"authoring"`, or `"inspector"` (see
    /// `idtx_render::TextureDecodeConfig::from_profile_token`). Selects how
    /// material/controller textures are resolution-capped and mip-generated on
    /// the target device: `full_resolution` keeps 4K sources (~85 MiB each,
    /// which can exceed the per-prefetch upload budget on a standalone headset
    /// and silently fall back to a white null texture), while `authoring`
    /// (2048) / `inspector` (1024) cap them to fit. `None` → the runtime default
    /// (`full_resolution`). Stamped by the cook from the project's per-target
    /// `[build.android]` / `[build.visionos].texture_quality` setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_quality: Option<String>,
}

impl ExperienceConfig {
    /// `true` when no field is set — used to omit an empty `[experience]` table
    /// from the serialized manifest (keeps a preview / v1 `run.toml` clean).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stage.is_none()
            && self.mode.is_none()
            && self.name.is_none()
            && self.target.is_none()
            && self.fullscreen.is_none()
            && self.levels.is_empty()
            && self.start_level.is_none()
            && self.transfer_level.is_none()
            && self.texture_quality.is_none()
    }
}

/// Toolchain / ABI pins captured at cook time — added in schema **v2**.
///
/// These record the exact contract the bundle was cooked against so a packaged
/// player can detect a mismatch instead of silently misbehaving. The most
/// consequential is [`abi_version`](PinsConfig::abi_version): behaviors are
/// distributed as `.wasm` and a host/guest ABI mismatch makes them silently fall
/// back to no-ops, so the player warns loudly when the pin disagrees with its own
/// `idtx-behavior-abi::ABI_VERSION`. The version-string pins (`wgpu`, `wasmtime`,
/// `glam`) are informational provenance carried into the manifest hash.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinsConfig {
    /// Behavior host/guest ABI version the bundle was cooked against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi_version: Option<u32>,
    /// Pinned `wgpu` version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wgpu: Option<String>,
    /// Pinned `wasmtime` version string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasmtime: Option<String>,
    /// Pinned `glam` version string (behavior-ABI layout pin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glam: Option<String>,
    /// Effective features embedded in the selected runtime. Empty for a payload
    /// that has not yet been bound to a runtime by a packager.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// Target/project feature requirements, checked against the actual runtime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_features: Vec<String>,
    /// Present once pins have been bound to a specific runtime build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rustc: Option<String>,
}

impl PinsConfig {
    /// `true` when no pin is recorded — used to omit an empty `[pins]` table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.abi_version.is_none()
            && self.wgpu.is_none()
            && self.wasmtime.is_none()
            && self.glam.is_none()
            && self.features.is_empty()
            && self.required_features.is_empty()
            && self.runtime_target.is_none()
            && self.rustc.is_none()
    }
}

/// A reproducibility stamp baked into a *cooked* manifest — schema **v2**.
///
/// Records the content hash the cook computed over its inputs (the flattened
/// stage's dependency closure + the collected behavior modules + the pins) so
/// two cooks of byte-identical inputs are recognizably the same build, and a
/// changed input is visible as a changed stamp. Absent for a manifest that was
/// not produced by a cook (a preview / hand-written `run.toml`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildStamp {
    /// Hex SHA-256 over the cook inputs. `None` when not cooked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// The cook target label this stamp was produced for (mirrors
    /// [`ExperienceConfig::target`]; kept here so the stamp is self-describing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl BuildStamp {
    /// `true` when no stamp field is set — used to omit an empty `[stamp]` table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content_hash.is_none() && self.target.is_none()
    }
}

/// Network capability for runtime assets, REST and behavior HTTP — schema
/// **v2**.
///
/// A `RestDatasource` prim's `endpoint` is attacker-controllable stage data, so
/// network access is **deny-by-default** and gated here: a REST datasource only
/// polls when [`allow`](NetworkConfig::allow) is set **and** its endpoint host
/// matches one of [`allowed_hosts`](NetworkConfig::allowed_hosts). Listing a host
/// is mandatory — `allow = true` with an empty list still polls nothing. The
/// runtime additionally rejects non-`http(s)` schemes and URL userinfo. Private
/// addresses require `private_hosts`; metadata/link-local addresses always fail
/// (see `idtx-studio-session`'s `ExecNetworkPolicy`).
///
/// ```toml
/// [network]
/// allow = true
/// allowed_hosts = ["api.example.com", "*.data.example.org"]
/// max_response_mb = 2
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Master switch for runtime asset, REST and behavior HTTP. `false`
    /// (default) disables all egress regardless of host/credential grants.
    #[serde(default)]
    pub allow: bool,
    /// Allowlist of endpoint host patterns. Each is an exact host
    /// (`api.example.com`) or a `*.domain` wildcard (`*.example.com`, matching the
    /// apex and any subdomain). A REST endpoint whose host matches none is blocked.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Explicit loopback/RFC1918/IPv6 unique-local destinations. These host
    /// patterns must also be in `allowed_hosts`. Metadata/link-local and other
    /// special-purpose ranges remain blocked even for hosts named here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_hosts: Vec<String>,
    /// Explicit credential-sharing origins, independent of `allowed_hosts`.
    /// Examples: `https://api.example.com`, `https://*.assets.example.com:8443`.
    /// Empty authorizes no embedded token or behavior credential writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth_origins: Vec<String>,
    /// Maximum accepted REST response body size, in **megabytes** (MiB). Larger
    /// responses are dropped so a hostile/runaway endpoint cannot exhaust memory.
    /// `0` (the default) resolves to the runtime's built-in default (1 MiB) — it
    /// is never unlimited. Omitted from a clean `run.toml` when `0`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub max_response_mb: u32,
    /// Maximum streamed USD asset size in MiB. `0` uses the 256 MiB default.
    /// Separate from REST/behavior bodies, which are buffered in memory.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub max_asset_mb: u32,
    /// ⚠️ **TEMPORARY / SECURITY RISK — remove when the interactive logon-level
    /// flow lands.** A bearer token stamped into the cooked `run.toml` so a
    /// packaged device runtime can authenticate remote (`http(s)://`) USD asset
    /// fetches, since a device has no keychain / `~/.idtx-studio/config.toml`.
    /// Embedding credentials in a build artifact is unsafe — never ship such a
    /// build. The cook stamps this **only** when explicitly opted in
    /// (`embed_dev_token`), and the runtime logs a prominent warning when it is
    /// present. Target replacement: (1) a logon level with an interactive widget
    /// panel → (2) "Log on" transitions to the next level on success → (3) the
    /// next level fetches remote content using the interactively-obtained auth.
    /// See `docs/plan_remote_refs_external_and_runtime_auth.md`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dev_bearer_token: String,
}

impl NetworkConfig {
    /// `true` when no network capability is configured — used to omit an empty
    /// `[network]` table (keeps a preview / v1 `run.toml` clean).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.allow
            && self.allowed_hosts.is_empty()
            && self.private_hosts.is_empty()
            && self.auth_origins.is_empty()
            && self.max_response_mb == 0
            && self.max_asset_mb == 0
            && self.dev_bearer_token.is_empty()
    }

    /// Shared REST/behavior policy. Constructed without requiring an HTTP stack.
    #[must_use]
    pub fn egress_policy(&self) -> idtx_net_guard::NetworkPolicy {
        idtx_net_guard::NetworkPolicy::new(self.allow, self.allowed_hosts.clone())
            .with_private_hosts(self.private_hosts.clone())
            .with_max_response_mb(self.max_response_mb)
    }

    /// Same reachability policy with the separate streamed asset byte cap.
    #[must_use]
    pub fn asset_policy(&self) -> idtx_net_guard::NetworkPolicy {
        self.egress_policy().with_max_response_mb(if self.max_asset_mb == 0 {
            DEFAULT_MAX_ASSET_MB
        } else {
            self.max_asset_mb
        })
    }
}

/// `skip_serializing_if` helper: omit a `u32` field when it is `0`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// `skip_serializing_if` helper: omit a `usize` field when it is `0`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// Behavior host configuration — schema v2+. Currently just per-instance work
/// limits; omitted from a clean `run.toml` when unset.
///
/// ```toml
/// [behavior.limits]
/// queue_bytes = 4194304
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// Per-instance work-budget overrides for the behavior host.
    #[serde(default, skip_serializing_if = "BehaviorLimitsConfig::is_empty")]
    pub limits: BehaviorLimitsConfig,
}

impl BehaviorConfig {
    /// `true` when nothing is configured — used to omit an empty `[behavior]`
    /// table from the serialized manifest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.limits.is_empty()
    }
}

/// Per-instance behavior work-budget caps. Each `0` (the default) means "use the
/// runtime's built-in default"; a non-zero value overrides it. Lets an
/// experience tune the deferred-work budget a `net` / `stage_edit` behavior needs
/// — e.g. to admit more than one full-size async HTTP response in flight at once.
///
/// ```toml
/// [behavior.limits]
/// queue_bytes = 4194304         # 4 MiB per-instance deferred-work budget
/// http_response_bytes = 262144  # 256 KiB cap + reservation per async response
/// http_inflight = 4             # max concurrent async HTTP requests per behavior
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorLimitsConfig {
    /// Per-instance deferred-work byte budget (session edits, overlays, async
    /// HTTP response reservations). `0` uses the runtime default (2 MiB).
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub queue_bytes: usize,
    /// Host ceiling on a single async HTTP response body — also the up-front
    /// per-request reservation drawn from `queue_bytes`. `0` uses the runtime
    /// default (1 MiB). Also clamped by the `[network] max_response_mb` policy, so
    /// raising it *above* 1 MiB requires raising both.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub http_response_bytes: usize,
    /// Max concurrent in-flight async HTTP requests per behavior instance. `0`
    /// uses the runtime default (8).
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub http_inflight: usize,
}

impl BehaviorLimitsConfig {
    /// `true` when no cap is overridden — used to omit an empty
    /// `[behavior.limits]` table from the serialized manifest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue_bytes == 0 && self.http_response_bytes == 0 && self.http_inflight == 0
    }
}

/// XR-mode capability configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct XrConfig {
    /// Per-device controller mesh assets, keyed by device id (`"generic"`,
    /// `"quest3"`, `"valve_index"`, …).
    #[serde(default)]
    pub controllers: BTreeMap<String, ControllerAsset>,
}

impl XrConfig {
    /// A default XR config declaring the conventional `generic` device meshes so
    /// that simply dropping files at `controllers/generic_{left,right}.usda`
    /// inside an asset root resolves without authoring a manifest.
    fn with_default_controllers() -> Self {
        let mut controllers = BTreeMap::new();
        controllers.insert(
            "generic".to_string(),
            ControllerAsset {
                left: Some(PathBuf::from("controllers/generic_left.usda")),
                right: Some(PathBuf::from("controllers/generic_right.usda")),
                asset: None,
                mount: None,
            },
        );
        Self { controllers }
    }
}

/// Relative paths (resolved against the asset search roots) to the controller
/// mesh assets for one device. Set [`ControllerAsset::asset`] to use one mesh
/// for both hands, or [`left`](ControllerAsset::left) / [`right`](ControllerAsset::right)
/// for per-hand meshes. An optional [`mount`](ControllerAsset::mount) seats the
/// device's rig frame in the `OpenXR` grip convention.
///
/// Not `Eq`: [`mount`](ControllerAsset::mount) carries `f64` components.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ControllerAsset {
    /// Left-hand controller mesh (relative to an asset root).
    #[serde(default)]
    pub left: Option<PathBuf>,
    /// Right-hand controller mesh (relative to an asset root).
    #[serde(default)]
    pub right: Option<PathBuf>,
    /// A single mesh used for both hands when `left`/`right` are unset.
    #[serde(default)]
    pub asset: Option<PathBuf>,
    /// Per-device mounting offset (TRS) that seats the model in the `OpenXR` grip
    /// frame. Applied to both hands. Absent → no offset (model placed by the
    /// generic up-axis/units normalization only).
    #[serde(default)]
    pub mount: Option<MountConfig>,
}

/// A per-device controller **mounting offset** in `run.toml`, expressed as a TRS
/// (translate / rotate / scale). Composes *outside* the generic upAxis/MPU
/// normalization to align a device-specific rig frame with the `OpenXR` **grip**
/// pose. There is no universal grip→model rotation — vendor meshes are authored
/// inconsistently — so this is calibrated per device (empirically, in-headset).
///
/// All three fields are optional: an absent `translate`/`rotate` is `[0,0,0]`
/// and an absent `scale` is unit `[1,1,1]` (see the accessor methods).
///
/// ```toml
/// [xr.controllers.questpro.mount]
/// rotate    = [180.0, 0.0, 0.0]   # Euler XYZ degrees
/// # translate = [0.0, 0.0, 0.0]   # metres (optional)
/// # scale     = [1.0, 1.0, 1.0]   # optional, unit if absent
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MountConfig {
    /// Translation in world metres. Absent → `[0, 0, 0]`.
    #[serde(default)]
    pub translate: Option<[f64; 3]>,
    /// Euler XYZ rotation in degrees. Absent → `[0, 0, 0]`.
    #[serde(default)]
    pub rotate: Option<[f64; 3]>,
    /// Per-axis scale. Absent → unit `[1, 1, 1]`.
    #[serde(default)]
    pub scale: Option<[f64; 3]>,
}

impl MountConfig {
    /// Resolved translation (metres); `[0, 0, 0]` when unset.
    #[must_use]
    pub fn translate(&self) -> [f64; 3] {
        self.translate.unwrap_or([0.0, 0.0, 0.0])
    }
    /// Resolved Euler XYZ rotation (degrees); `[0, 0, 0]` when unset.
    #[must_use]
    pub fn rotate(&self) -> [f64; 3] {
        self.rotate.unwrap_or([0.0, 0.0, 0.0])
    }
    /// Resolved per-axis scale; unit `[1, 1, 1]` when unset.
    #[must_use]
    pub fn scale(&self) -> [f64; 3] {
        self.scale.unwrap_or([1.0, 1.0, 1.0])
    }
}

impl ControllerAsset {
    fn left_rel(&self) -> Option<&Path> {
        self.left.as_deref().or(self.asset.as_deref())
    }
    fn right_rel(&self) -> Option<&Path> {
        self.right.as_deref().or(self.asset.as_deref())
    }
}

/// Absolute, resolved controller mesh asset paths for one device — the result of
/// [`RuntimeManifest::resolve_controller_asset`].
///
/// Not `Eq`: [`mount`](ResolvedControllers::mount) carries `f64` components.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedControllers {
    /// The device id that resolved (may be `"generic"` after fallback).
    pub device: String,
    /// Absolute path to the left-hand controller mesh.
    pub left: PathBuf,
    /// Absolute path to the right-hand controller mesh.
    pub right: PathBuf,
    /// The mounting offset from the resolved device entry, if any (applied to
    /// both hands). `None` when the entry declares no `mount`.
    pub mount: Option<MountConfig>,
}

impl RuntimeManifest {
    /// Discover a manifest in priority order:
    /// 1. the file named by [`ENV_MANIFEST`] (explicit override),
    /// 2. `run.toml` next to the running executable (packaged bundle),
    /// 3. a synthesized [`RuntimeManifest::default`] (so preview always works).
    ///
    /// A failed selected manifest uses deny-by-default configuration. An invalid
    /// explicit override never falls through to another file's network grants.
    #[must_use]
    pub fn discover() -> Self {
        let explicit = std::env::var_os(ENV_MANIFEST).map(PathBuf::from);
        let adjacent = std::env::current_exe().ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("run.toml")));
        Self::discover_paths(explicit.as_deref(), adjacent.as_deref())
    }

    fn discover_paths(explicit: Option<&Path>, adjacent: Option<&Path>) -> Self {
        let Some(path) = explicit.or_else(|| adjacent.filter(|p| p.exists())) else { return Self::default(); };
        match Self::load(path) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("[preview] {error}; using default configuration (network denied)");
                Self::default()
            }
        }
    }

    /// Load and validate a manifest from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the file cannot be read, is not valid TOML
    /// for this schema, or declares an unsupported schema version.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest: RuntimeManifest =
            toml::from_str(&text).map_err(|source| ManifestError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        if manifest.version > MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                found: manifest.version,
                supported: MANIFEST_VERSION,
            });
        }
        idtx_auth::AuthScopes::parse(&manifest.network.auth_origins)?;
        for (id, prototype) in &manifest.runtime_prototypes {
            prototype.validate(id).map_err(ManifestError::InvalidRuntimePrototype)?;
        }
        Ok(manifest)
    }

    /// Serialize this manifest back to pretty TOML (for logging / packaging).
    #[must_use]
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// A short content hash (first 8 bytes of SHA-256 over the TOML form), handy
    /// for correlating logs with a specific manifest.
    #[must_use]
    pub fn content_hash(&self) -> String {
        use std::fmt::Write as _;
        let mut hasher = Sha256::new();
        hasher.update(self.to_toml_string().as_bytes());
        hasher
            .finalize()
            .iter()
            .take(8)
            .fold(String::new(), |mut acc, b| {
                let _ = write!(acc, "{b:02x}");
                acc
            })
    }

    /// Set the packaged experience entry (schema v2), consuming and returning
    /// self for chaining. Used by the `idtx-build` cook to stamp the emitted
    /// `run.toml` with the flattened stage path and default run mode.
    #[must_use]
    pub fn with_experience(mut self, experience: ExperienceConfig) -> Self {
        self.experience = experience;
        self
    }

    /// Set the XR controller configuration (`[xr]`), consuming and returning self
    /// for chaining. Used by the `idtx-build` cook to fold the project's
    /// `xr_controllers.toml` sidecar (with paths rewritten to point at the copied
    /// payload meshes) into the emitted `run.toml`, so the packaged runtime
    /// resolves device→asset exactly as the editor preview does.
    #[must_use]
    pub fn with_xr(mut self, xr: XrConfig) -> Self {
        self.xr = xr;
        self
    }

    /// Set the cook-time toolchain / ABI pins (schema v2), consuming and
    /// returning self for chaining.
    #[must_use]
    pub fn with_pins(mut self, pins: PinsConfig) -> Self {
        self.pins = pins;
        self
    }

    /// Set the cook reproducibility stamp (schema v2), consuming and returning
    /// self for chaining.
    #[must_use]
    pub fn with_stamp(mut self, stamp: BuildStamp) -> Self {
        self.stamp = stamp;
        self
    }

    /// Set the prepared runtime template registry.
    #[must_use]
    pub fn with_runtime_prototypes(mut self, prototypes: BTreeMap<String, RuntimePrototypeConfig>) -> Self {
        self.runtime_prototypes = prototypes;
        self
    }

    /// Resolve registered templates against the selected manifest directory.
    /// Explicit manifest overrides therefore keep their own path anchor.
    pub fn resolved_runtime_prototypes(&self, manifest_dir: &Path) -> Vec<(String, PathBuf, String)> {
        self.runtime_prototypes.iter().map(|(id, template)| (
            id.clone(), manifest_dir.join(&template.asset), template.prim.clone(),
        )).collect()
    }

    /// Set the network capability (`[network]`, schema v2), consuming and
    /// returning self for chaining.
    #[must_use]
    pub fn with_network(mut self, network: NetworkConfig) -> Self {
        self.network = network;
        self
    }

    /// Set the behavior host limits (`[behavior]`, schema v2), consuming and
    /// returning self for chaining.
    #[must_use]
    pub fn with_behavior(mut self, behavior: BehaviorConfig) -> Self {
        self.behavior = behavior;
        self
    }

    /// The stage-authored-datasource network policy a runtime should apply:
    /// `(allow, allowed_hosts)`. `(false, [])` for a preview / v1 manifest that
    /// declares no `[network]` table — deny-by-default.
    #[must_use]
    pub fn network_policy(&self) -> (bool, Vec<String>) {
        (self.network.allow, self.network.allowed_hosts.clone())
    }

    /// ⚠️ **TEMPORARY.** The opt-in dev bearer token stamped into `[network]`
    /// (`dev_bearer_token`), or `None` when absent/empty. A packaged device
    /// runtime arms this for remote USD fetches since it has no keychain. Remove
    /// when the interactive logon-level flow lands — see
    /// `docs/plan_remote_refs_external_and_runtime_auth.md`.
    #[must_use]
    pub fn network_dev_bearer_token(&self) -> Option<&str> {
        let t = self.network.dev_bearer_token.trim();
        (!t.is_empty()).then_some(t)
    }

    /// The configured maximum REST response body size in megabytes (`[network]
    /// max_response_mb`). `0` means "use the runtime default" (1 MiB). Applied by
    /// the runtime's `ExecNetworkPolicy`.
    #[must_use]
    pub fn network_max_response_mb(&self) -> u32 {
        self.network.max_response_mb
    }

    /// The configured per-instance behavior work-budget overrides
    /// (`[behavior.limits]`). Each field is `0` when unset, meaning the runtime
    /// uses its built-in default. Applied via `set_behavior_limits` at session
    /// setup.
    #[must_use]
    pub fn behavior_limits(&self) -> &BehaviorLimitsConfig {
        &self.behavior.limits
    }

    /// The packaged run defaults `(stage, mode)` a player should fall back to
    /// when no CLI override is supplied. Both are `None` for a preview / v1
    /// manifest that declares no `[experience]` entry.
    #[must_use]
    pub fn experience_run_defaults(&self) -> (Option<&Path>, Option<&str>) {
        (
            self.experience.stage.as_deref(),
            self.experience.mode.as_deref(),
        )
    }

    /// The packaged fullscreen default a desktop player should fall back to when
    /// no `--fullscreen` / `--windowed` CLI flag is supplied. `None` for a
    /// preview / v1 manifest (and for XR targets) — the runtime then stays
    /// windowed.
    #[must_use]
    pub fn experience_fullscreen_default(&self) -> Option<bool> {
        self.experience.fullscreen
    }

    /// Compare the manifest's recorded behavior ABI pin against `host_abi`.
    ///
    /// * `None` — the manifest records no ABI pin (a v1 / preview manifest); the
    ///   caller cannot decide and should proceed without a warning.
    /// * `Some(true)` — pin matches; behaviors will load.
    /// * `Some(false)` — pin mismatches; behaviors distributed with the bundle
    ///   will silently no-op unless rebuilt. The caller should warn loudly.
    #[must_use]
    pub fn abi_matches(&self, host_abi: u32) -> Option<bool> {
        self.pins.abi_version.map(|pinned| pinned == host_abi)
    }

    /// Ordered asset search roots, strongest-first:
    /// 1. the [`ENV_ASSETS`] root (vendor / late-binding drop-in), when set,
    /// 2. the manifest's configured [`AssetConfig::search_paths`],
    /// 3. an `assets/` dir next to the running executable (packaged default),
    /// 4. the executable's own directory — so a staged `built-in/` tree beside
    ///    the binary makes `@built-in/...@` references resolve (`<exe>/built-in/...`).
    #[must_use]
    pub fn asset_search_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(raw) = std::env::var_os(ENV_ASSETS) {
            roots.push(PathBuf::from(raw));
        }
        roots.extend(self.assets.search_paths.iter().cloned());
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        {
            roots.push(dir.join("assets"));
            roots.push(dir);
        }
        roots
    }

    /// Resolve the controller mesh assets for `device`, falling back to the
    /// `"generic"` device, over the [`asset_search_roots`](Self::asset_search_roots).
    /// Returns `None` when nothing resolves (the caller then omits the reference
    /// and the runtime uses its capsule fallback).
    #[must_use]
    pub fn resolve_controller_asset(&self, device: &str) -> Option<ResolvedControllers> {
        self.resolve_with_roots(&self.asset_search_roots(), device)
    }

    /// [`resolve_controller_asset`](Self::resolve_controller_asset) against an
    /// explicit root list instead of [`asset_search_roots`](Self::asset_search_roots).
    ///
    /// Needed on Android/Quest, where the payload is extracted to a writable data
    /// dir that `asset_search_roots` cannot derive (`current_exe()` is
    /// `/system/bin/...`): the runtime passes the extracted `payload/assets` root
    /// explicitly so the staged `controllers/<file>` meshes resolve.
    #[must_use]
    pub fn resolve_controller_asset_in_roots(
        &self,
        roots: &[PathBuf],
        device: &str,
    ) -> Option<ResolvedControllers> {
        self.resolve_with_roots(roots, device)
    }

    /// [`resolve_controller_asset`](Self::resolve_controller_asset) against an
    /// explicit root list (used by tests to stay independent of process env).
    fn resolve_with_roots(&self, roots: &[PathBuf], device: &str) -> Option<ResolvedControllers> {
        let mut tried = Vec::new();
        if device != "generic" {
            tried.push(device);
        }
        tried.push("generic");
        for dev in tried {
            let Some(entry) = self.xr.controllers.get(dev) else {
                continue;
            };
            let (Some(left_rel), Some(right_rel)) = (entry.left_rel(), entry.right_rel()) else {
                continue;
            };
            if let (Some(left), Some(right)) = (
                resolve_in_roots(roots, left_rel),
                resolve_in_roots(roots, right_rel),
            ) {
                return Some(ResolvedControllers {
                    device: dev.to_string(),
                    left,
                    right,
                    mount: entry.mount,
                });
            }
        }
        None
    }
}

/// Resolve `rel` against `roots` (first existing file wins). An absolute `rel`
/// that exists is returned as-is.
fn resolve_in_roots(roots: &[PathBuf], rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute() {
        return rel.is_file().then(|| rel.to_path_buf());
    }
    roots
        .iter()
        .map(|root| root.join(rel))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_prototypes_resolve_relative_to_the_selected_manifest() {
        let dir = scratch("prototypes");
        let path = dir.as_path().join("run.toml");
        std::fs::write(&path, "[runtime_prototypes.panel]\nasset = 'runtime_prototypes/panel.usdz'\nprim = '/Panel'\n").unwrap();
        let manifest = RuntimeManifest::load(&path).unwrap();
        assert_eq!(manifest.resolved_runtime_prototypes(dir.as_path()), vec![(
            "panel".into(), dir.as_path().join("runtime_prototypes/panel.usdz"), "/Panel".into(),
        )]);
        assert_eq!(toml::from_str::<RuntimeManifest>(&manifest.to_toml_string()).unwrap(), manifest);
    }

    #[test]
    fn runtime_prototypes_reject_invalid_ids_paths_and_traversal() {
        let dir = scratch("prototypes");
        let path = dir.as_path().join("run.toml");
        for (id, asset, prim) in [("bad-id", "panel.usdz", "/Panel"),
            ("panel", "../panel.usdz", "/Panel"), ("panel", "panel.usdz", "/Panel.visibility"),
            ("panel", "panel.usdz", "/"), ("panel", "", "/Panel")] {
            std::fs::write(&path, format!("[runtime_prototypes.'{id}']\nasset = '{asset}'\nprim = '{prim}'\n")).unwrap();
            assert!(matches!(RuntimeManifest::load(&path), Err(ManifestError::InvalidRuntimePrototype(_))));
        }
    }


    fn write_file(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"#usda 1.0\n").unwrap();
    }

    /// Unique scratch dir under the system temp dir (no `tempfile` dep).
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-manifest-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_manifest_declares_generic_controllers() {
        let manifest = RuntimeManifest::default();
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert!(manifest.xr.controllers.contains_key("generic"));
    }

    #[test]
    fn default_manifest_is_schema_v2_with_empty_v2_sections() {
        let manifest = RuntimeManifest::default();
        assert_eq!(manifest.version, 2);
        assert!(manifest.experience.is_empty());
        assert!(manifest.pins.is_empty());
        assert!(manifest.stamp.is_empty());
        // Empty v2 sections are omitted, so a preview manifest stays clean.
        let text = manifest.to_toml_string();
        assert!(!text.contains("[experience]"), "got:\n{text}");
        assert!(!text.contains("[pins]"), "got:\n{text}");
        assert!(!text.contains("[stamp]"), "got:\n{text}");
    }

    #[test]
    fn v1_manifest_still_loads_under_v2_build() {
        // A hand-written v1 run.toml (no [experience]/[pins]) must keep working.
        let text = "\
version = 1

[xr.controllers.generic]
left = \"controllers/generic_left.usda\"
right = \"controllers/generic_right.usda\"
";
        let manifest: RuntimeManifest = toml::from_str(text).unwrap();
        assert_eq!(manifest.version, 1);
        assert!(manifest.experience.is_empty());
        assert!(manifest.pins.is_empty());
        assert!(manifest.xr.controllers.contains_key("generic"));
    }

    #[test]
    fn experience_and_pins_round_trip() {
        let manifest = RuntimeManifest::default()
            .with_experience(ExperienceConfig {
                stage: Some(PathBuf::from("stage.usdz")),
                mode: Some("xr".to_string()),
                name: Some("Demo Level".to_string()),
                target: Some("windows-pcvr".to_string()),
                fullscreen: Some(true),
                ..Default::default()
            })
            .with_pins(PinsConfig {
                abi_version: Some(2),
                wgpu: Some("29".to_string()),
                wasmtime: Some("48".to_string()),
                glam: Some("0.33.0".to_string()),
                features: vec!["pcvr".to_string()],
                required_features: vec!["pcvr".to_string()],
                runtime_target: Some("x86_64-pc-windows-msvc".into()),
                rustc: Some("test compiler".into()),
            })
            .with_stamp(BuildStamp {
                content_hash: Some("deadbeef".to_string()),
                target: Some("windows-pcvr".to_string()),
            });
        let text = manifest.to_toml_string();
        assert!(text.contains("[experience]"), "got:\n{text}");
        assert!(text.contains("fullscreen = true"), "got:\n{text}");
        assert!(text.contains("[pins]"), "got:\n{text}");
        assert!(text.contains("[stamp]"), "got:\n{text}");
        let back: RuntimeManifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn multi_level_experience_fields_round_trip_and_stay_clean_by_default() {
        // A multi-level experience carries levels + start/transfer; they round-trip.
        let manifest = RuntimeManifest::default().with_experience(ExperienceConfig {
            mode: Some("vr".to_string()),
            levels: vec![
                PathBuf::from("levels/forest/level.usda"),
                PathBuf::from("levels/cave/level.usda"),
            ],
            start_level: Some(PathBuf::from("levels/forest/level.usda")),
            transfer_level: Some(PathBuf::from("transfer/level.usda")),
            ..Default::default()
        });
        let text = manifest.to_toml_string();
        assert!(text.contains("levels = ["), "got:\n{text}");
        assert!(
            text.contains("start_level = \"levels/forest/level.usda\""),
            "got:\n{text}"
        );
        assert!(
            text.contains("transfer_level = \"transfer/level.usda\""),
            "got:\n{text}"
        );
        let back: RuntimeManifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest, back);

        // A single-stage experience must NOT emit the multi-level keys.
        let single = RuntimeManifest::default().with_experience(ExperienceConfig {
            stage: Some(PathBuf::from("stage.usdz")),
            ..Default::default()
        });
        let text = single.to_toml_string();
        assert!(!text.contains("levels ="), "got:\n{text}");
        assert!(!text.contains("start_level"), "got:\n{text}");
        assert!(!text.contains("transfer_level"), "got:\n{text}");
        // And an experience with only these fields is not "empty".
        assert!(!ExperienceConfig {
            start_level: Some(PathBuf::from("levels/forest/level.usda")),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn fullscreen_omitted_when_unset() {
        // A preview / v1 experience that sets no fullscreen must not emit the key,
        // and must still parse back to `None` (windowed default downstream).
        let manifest = RuntimeManifest::default().with_experience(ExperienceConfig {
            stage: Some(PathBuf::from("stage.usdz")),
            mode: Some("desktop".to_string()),
            ..ExperienceConfig::default()
        });
        let text = manifest.to_toml_string();
        assert!(!text.contains("fullscreen"), "got:\n{text}");
        let back: RuntimeManifest = toml::from_str(&text).unwrap();
        assert_eq!(back.experience_fullscreen_default(), None);
    }

    #[test]
    fn network_section_omitted_when_empty_and_defaults_to_deny() {
        let manifest = RuntimeManifest::default();
        assert!(manifest.network.is_empty());
        assert!(manifest.network.asset_policy().is_denied());
        assert!(manifest.network.egress_policy().is_denied());
        assert_eq!(manifest.network.asset_policy().max_response_bytes(), 256 * 1024 * 1024);
        assert_eq!(manifest.network.egress_policy().max_response_bytes(), 1024 * 1024);
        assert_eq!(manifest.network_policy(), (false, Vec::new()));
        // Deny-by-default: no [network] table in a clean preview manifest.
        assert!(!manifest.to_toml_string().contains("[network]"));
    }

    #[test]
    fn network_section_round_trips() {
        let manifest = RuntimeManifest::default().with_network(NetworkConfig {
            allow: true,
            allowed_hosts: vec!["api.example.com".into(), "*.data.example.org".into()],
            auth_origins: vec!["https://api.example.com:8443".into()],
            private_hosts: vec!["api.example.com".into()],
            max_asset_mb: 64,
            max_response_mb: 3,
            ..Default::default()
        });
        let text = manifest.to_toml_string();
        assert!(text.contains("[network]"), "got:\n{text}");
        assert!(text.contains("allow = true"), "got:\n{text}");
        assert!(text.contains("max_response_mb = 3"), "got:\n{text}");
        let back: RuntimeManifest = toml::from_str(&text).unwrap();
        assert_eq!(manifest, back);
        assert_eq!(
            back.network_policy(),
            (
                true,
                vec!["api.example.com".to_string(), "*.data.example.org".to_string()]
            )
        );
        assert_eq!(back.network_max_response_mb(), 3);
    }

    #[test]
    fn network_max_response_mb_omitted_when_zero() {
        let manifest = RuntimeManifest::default().with_network(NetworkConfig {
            allow: true,
            allowed_hosts: vec!["api.example.com".into()],
            max_response_mb: 0,
            ..Default::default()
        });
        let text = manifest.to_toml_string();
        assert!(!text.contains("max_response_mb"), "got:\n{text}");
        assert_eq!(manifest.network_max_response_mb(), 0);
    }

    #[test]
    fn load_rejects_malformed_credential_origins() {
        let dir = scratch("invalid-auth-origins");
        let path = dir.join("run.toml");
        std::fs::write(&path, "version = 2\n[network]\nauth_origins = [\"https://host/path\"]\n").unwrap();
        assert!(matches!(RuntimeManifest::load(&path), Err(ManifestError::Auth(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_explicit_manifest_never_inherits_an_adjacent_network_grant() {
        let dir = scratch("deny-config-failures");
        let explicit = dir.join("explicit.toml");
        let adjacent = dir.join("run.toml");
        std::fs::write(&adjacent, "[network]\nallow = true\nallowed_hosts = [\"example.com\"]\n").unwrap();
        assert!(!RuntimeManifest::discover_paths(None, Some(&adjacent)).network.asset_policy().is_denied());
        for text in [None, Some("[network\nallow=true"), Some("version=999"),
            Some("[network]\nallow=true\nauth_origins=[\"bad\"]")] {
            if let Some(text) = text { std::fs::write(&explicit, text).unwrap(); }
            let manifest = RuntimeManifest::discover_paths(Some(&explicit), Some(&adjacent));
            assert!(manifest.network.asset_policy().is_denied());
            assert!(manifest.network.egress_policy().is_denied());
        }
        std::fs::write(&explicit, "[network]\ndev_bearer_token = 'do-not-log-me\n").unwrap();
        assert!(!RuntimeManifest::load(&explicit).unwrap_err().to_string().contains("do-not-log-me"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn behavior_limits_round_trip_and_omitted_when_default() {
        // Overrides round-trip through TOML and are exposed via the accessor.
        let manifest = RuntimeManifest::default().with_behavior(BehaviorConfig {
            limits: BehaviorLimitsConfig {
                queue_bytes: 4 * 1024 * 1024,
                http_response_bytes: 65_536,
                http_inflight: 4,
            },
        });
        let text = manifest.to_toml_string();
        assert!(text.contains("[behavior.limits]"), "got:\n{text}");
        assert!(text.contains("queue_bytes = 4194304"), "got:\n{text}");
        let back: RuntimeManifest = toml::from_str(&text).unwrap();
        assert_eq!(back.behavior_limits().queue_bytes, 4 * 1024 * 1024);
        assert_eq!(back.behavior_limits().http_response_bytes, 65_536);
        assert_eq!(back.behavior_limits().http_inflight, 4);

        // A default (empty) behavior config is omitted entirely from the TOML.
        let empty = RuntimeManifest::default().to_toml_string();
        assert!(!empty.contains("[behavior"), "got:\n{empty}");
    }

    #[test]
    fn experience_run_defaults_reports_stage_and_mode() {
        let manifest = RuntimeManifest::default().with_experience(ExperienceConfig {
            stage: Some(PathBuf::from("stage.usdz")),
            mode: Some("desktop".to_string()),
            ..ExperienceConfig::default()
        });
        let (stage, mode) = manifest.experience_run_defaults();
        assert_eq!(stage, Some(Path::new("stage.usdz")));
        assert_eq!(mode, Some("desktop"));
        // A preview manifest reports neither.
        assert_eq!(
            RuntimeManifest::default().experience_run_defaults(),
            (None, None)
        );
    }

    #[test]
    fn abi_matches_reports_pin_agreement() {
        // No pin → cannot decide.
        assert_eq!(RuntimeManifest::default().abi_matches(2), None);
        let pinned = RuntimeManifest::default().with_pins(PinsConfig {
            abi_version: Some(2),
            ..PinsConfig::default()
        });
        assert_eq!(pinned.abi_matches(2), Some(true));
        assert_eq!(pinned.abi_matches(1), Some(false));
    }

    #[test]
    fn toml_round_trips() {
        let manifest = RuntimeManifest::default();
        let text = manifest.to_toml_string();
        let back = toml::from_str::<RuntimeManifest>(&text).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn rejects_future_version() {
        let dir = scratch("ver");
        let path = dir.join("run.toml");
        std::fs::write(&path, format!("version = {}\n", MANIFEST_VERSION + 1)).unwrap();
        let err = RuntimeManifest::load(&path).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedVersion { .. }));
    }

    #[test]
    fn search_path_precedence_first_hit_wins() {
        let dir = scratch("prec");
        let strong = dir.join("strong");
        let weak = dir.join("weak");
        // Present in both roots; the strong (earlier) root must win.
        write_file(&strong.join("controllers/generic_left.usda"));
        write_file(&strong.join("controllers/generic_right.usda"));
        write_file(&weak.join("controllers/generic_left.usda"));
        write_file(&weak.join("controllers/generic_right.usda"));

        let manifest = RuntimeManifest::default();
        let resolved = manifest
            .resolve_with_roots(&[strong.clone(), weak], "generic")
            .expect("resolves");
        assert!(resolved.left.starts_with(&strong));
    }

    #[test]
    fn falls_back_to_generic_device() {
        let dir = scratch("fallback");
        write_file(&dir.join("controllers/generic_left.usda"));
        write_file(&dir.join("controllers/generic_right.usda"));

        let manifest = RuntimeManifest::default();
        // No "quest3" entry exists → falls back to the generic meshes.
        let resolved = manifest
            .resolve_with_roots(&[dir], "quest3")
            .expect("resolves");
        assert_eq!(resolved.device, "generic");
    }

    #[test]
    fn unresolved_asset_is_none() {
        let dir = scratch("missing");
        let manifest = RuntimeManifest::default();
        // Empty root: nothing to resolve.
        assert!(manifest.resolve_with_roots(&[dir], "generic").is_none());
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact default literals, bit-exact by construction
    fn mount_config_defaults_are_zero_translate_rotate_unit_scale() {
        // An empty mount table resolves to identity T/R and unit scale.
        let cfg: MountConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.translate(), [0.0, 0.0, 0.0]);
        assert_eq!(cfg.rotate(), [0.0, 0.0, 0.0]);
        assert_eq!(cfg.scale(), [1.0, 1.0, 1.0]);
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact literals from the parsed TOML, bit-exact
    fn mount_round_trips_through_toml() {
        let text = "\
version = 1

[xr.controllers.questpro]
left = \"controllers/questpro_left.usdz\"
right = \"controllers/questpro_right.usdz\"

[xr.controllers.questpro.mount]
rotate = [180.0, 0.0, 0.0]
";
        let manifest: RuntimeManifest = toml::from_str(text).unwrap();
        let mount = manifest.xr.controllers["questpro"]
            .mount
            .expect("mount parsed");
        assert_eq!(mount.rotate(), [180.0, 0.0, 0.0]);
        // Omitted translate/scale take their defaults.
        assert_eq!(mount.translate(), [0.0, 0.0, 0.0]);
        assert_eq!(mount.scale(), [1.0, 1.0, 1.0]);
        // And the manifest survives a serialize → parse round-trip.
        let back: RuntimeManifest = toml::from_str(&manifest.to_toml_string()).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact literal carried through resolution, bit-exact
    fn resolution_carries_mount_through() {
        let dir = scratch("mount");
        write_file(&dir.join("controllers/qp_left.usda"));
        write_file(&dir.join("controllers/qp_right.usda"));

        let mut controllers = BTreeMap::new();
        controllers.insert(
            "questpro".to_string(),
            ControllerAsset {
                left: Some(PathBuf::from("controllers/qp_left.usda")),
                right: Some(PathBuf::from("controllers/qp_right.usda")),
                asset: None,
                mount: Some(MountConfig {
                    translate: None,
                    rotate: Some([180.0, 0.0, 0.0]),
                    scale: None,
                }),
            },
        );
        let manifest = RuntimeManifest {
            xr: XrConfig { controllers },
            ..RuntimeManifest::default()
        };

        let resolved = manifest
            .resolve_with_roots(&[dir], "questpro")
            .expect("resolves");
        assert_eq!(resolved.device, "questpro");
        assert_eq!(
            resolved.mount.expect("mount carried").rotate(),
            [180.0, 0.0, 0.0]
        );
    }
}
