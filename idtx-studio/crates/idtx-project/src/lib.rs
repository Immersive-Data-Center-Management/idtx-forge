//! Confined Conductor project, asset, and extension-lock model.
//!
//! A **project** is the outermost container for everything the Conductor edits:
//! a root directory holding a single [`ProjectManifest`] (`project.toml`) plus
//! the assets, levels, and stages referenced relative to that root. This crate
//! owns the *model* — load / save / discover of the manifest and the
//! project-root sandbox — while the shell owns the UI and the live session.
//!
//! Design (per the conductor-vision-roadmap, decisions D1/D2):
//! - Stage-independent config lives in **TOML sidecars**, not USD metadata; the
//!   project manifest is the first such sidecar. It mirrors the versioned
//!   `toml` + schema-version pattern of [`idtx_runtime_manifest`].
//! - The manifest file is `project.toml` at the **project root**; the root is
//!   simply the file's parent directory. [`IdtxProject::discover`] walks the
//!   parent chain looking for a parseable manifest — the backbone of the future
//!   create/select-project dialog.
//! - The crate stays low in the dependency stack: it does **not** depend on the
//!   scene/session/render crates. Unit scale is carried as a plain
//!   `meters_per_unit: f64`; higher layers convert it to their own types.

mod assets;
mod root;
mod xr_controllers;

pub use assets::{AssetEntry, AssetKind, list_dir};
pub use root::{ProjectFileTarget, ProjectRoot, format_stage_display, normalize_display_path, strip_verbatim_prefix};
pub use xr_controllers::{XR_CONTROLLERS_FILENAME, parse_xr_controllers};

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Highest project-manifest schema version this build understands.
pub const PROJECT_SCHEMA_VERSION: u32 = 1;

/// Canonical file name of a project manifest, located at the project root.
pub const PROJECT_MANIFEST_FILENAME: &str = "project.toml";

/// Errors from loading, saving, or discovering a project.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// The manifest file could not be read.
    #[error("read project manifest {path}: {source}")]
    Io {
        /// The manifest path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The manifest file could not be written.
    #[error("write project manifest {path}: {source}")]
    Write {
        /// The manifest path that failed to write.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The manifest file was not valid TOML for this schema.
    #[error("parse project manifest {path}: {source}")]
    Parse {
        /// The manifest path that failed to parse.
        path: PathBuf,
        /// The underlying TOML error.
        source: toml::de::Error,
    },
    /// The manifest could not be serialized back to TOML.
    #[error("serialize project manifest: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The manifest declares a schema version newer than this build supports.
    #[error("unsupported project schema version {found} (this build supports up to {supported})")]
    UnsupportedVersion {
        /// The version found in the manifest.
        found: u32,
        /// The highest version this build understands.
        supported: u32,
    },
    /// A manifest path was given whose parent directory could not be determined
    /// (e.g. a bare relative file name with no parent).
    #[error("project manifest {path} has no parent directory to use as the project root")]
    NoRoot {
        /// The offending manifest path.
        path: PathBuf,
    },
}

/// Per-project unit-scale configuration.
///
/// One editor/renderer unit equals [`meters_per_unit`](Self::meters_per_unit)
/// real metres. Defaults to `1.0` to match the shell's `RenderWorldUnitScale`
/// default (1 unit = 1 metre); a centimetre-authored project would set `0.01`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnitScaleConfig {
    /// Real metres represented by one editor/renderer unit. Must be positive.
    pub meters_per_unit: f64,
}

impl Default for UnitScaleConfig {
    fn default() -> Self {
        Self {
            meters_per_unit: 1.0,
        }
    }
}

impl UnitScaleConfig {
    /// The configured metres-per-unit, clamped to a sane positive value so a
    /// malformed `0`/negative manifest entry can never divide-by-zero downstream.
    #[must_use]
    pub fn meters_per_unit(self) -> f64 {
        if self.meters_per_unit > 0.0 {
            self.meters_per_unit
        } else {
            1.0
        }
    }
}

/// `[network]` — runtime network capability for assets, REST and behavior HTTP.
/// Deny-by-default; copied verbatim into the stamped `run.toml`
/// at cook/preview time.
///
/// ```toml
/// [network]
/// allow = true
/// allowed_hosts = ["api.example.com", "*.data.example.org"]
/// max_response_mb = 2
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkProjectConfig {
    /// Master switch for stage-authored REST datasources. `false` (default)
    /// disables all polling regardless of `allowed_hosts`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow: bool,
    /// Allowlist of endpoint host patterns (exact `api.example.com` or
    /// `*.domain` wildcard). A datasource whose host matches none is blocked
    /// even when `allow = true`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_hosts: Vec<String>,
    /// Explicit loopback/RFC1918/IPv6 unique-local hosts. Runtime access still
    /// requires `allow` plus `allowed_hosts`; metadata is always blocked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_hosts: Vec<String>,
    /// Explicit HTTP(S) credential-sharing origins (scheme/host/effective port),
    /// copied to `run.toml`; network reachability alone does not grant credentials.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth_origins: Vec<String>,
    /// Maximum accepted REST response body size, in **megabytes** (MiB). The
    /// runtime drops any response larger than this so a hostile or runaway
    /// endpoint cannot exhaust process memory. `0` (the default) means "use the
    /// built-in default" of 1 MiB — it is never treated as unlimited.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub max_response_mb: u32,
    /// Streamed USD asset cap in MiB; `0` selects the 256 MiB default.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub max_asset_mb: u32,
}

impl NetworkProjectConfig {
    /// `true` when no capability is configured — omits an empty `[network]`
    /// table from the saved TOML.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.allow && self.allowed_hosts.is_empty() && self.private_hosts.is_empty()
            && self.auth_origins.is_empty() && self.max_response_mb == 0 && self.max_asset_mb == 0
    }
}

/// `skip_serializing_if` helper: omit a `u32` field when it is `0` (the "unset /
/// use default" sentinel for `max_response_mb`).
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Packaged texture-decode profile for a cooked **device** build, exposed in the
/// per-target `[build.android]` / `[build.visionos]` settings so the author can
/// trade texture fidelity against on-device GPU memory without the profile being
/// hardcoded in the runtime.
///
/// Maps 1:1 onto `idtx_render::TextureDecodeConfig`'s three named presets (this
/// crate does not depend on `idtx-render`, so the mapping travels as the token
/// returned by [`TextureQuality::as_token`] through `run.toml`). The default
/// when unset is per-target: device targets (Quest / Vision Pro) default to
/// [`Authoring`](Self::Authoring) so large (e.g. 4K) textures are capped to fit
/// the per-frame upload budget rather than silently falling back to a white
/// null texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureQuality {
    /// Full source resolution, mipmaps on (`TextureDecodeConfig::FULL_RESOLUTION`).
    /// Highest fidelity; a 4K texture stays ~85 MiB with mips — appropriate for
    /// desktop / PCVR, risky on a standalone headset.
    FullResolution,
    /// Cap the longest edge to 2048, mipmaps on (`TextureDecodeConfig::AUTHORING`).
    /// ~21 MiB per 4K source — the recommended device default.
    Authoring,
    /// Cap the longest edge to 1024, mipmaps on (`TextureDecodeConfig::INSPECTOR`).
    /// ~5 MiB per 4K source — lowest memory, softest textures.
    Inspector,
}

impl TextureQuality {
    /// The stable lowercase token stamped into `run.toml` and consumed by
    /// `idtx_render::TextureDecodeConfig::from_profile_token`.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::FullResolution => "full_resolution",
            Self::Authoring => "authoring",
            Self::Inspector => "inspector",
        }
    }

    /// Parse a token (case-insensitive) back into a quality. Unknown → `None`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "full_resolution" | "full" => Some(Self::FullResolution),
            "authoring" | "high" => Some(Self::Authoring),
            "inspector" | "balanced" => Some(Self::Inspector),
            _ => None,
        }
    }
}

/// `[build]` packaging knobs carried by the project (consumed later by
/// `idtx-build`). All fields are optional so the section may be omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildConfig {
    /// Extra cargo features to enable when cooking this project's runtime.
    pub extra_features: Vec<String>,
    /// Requested MSAA sample count for the packaged player.
    pub msaa: Option<u32>,
    /// Target frame rate for the packaged player.
    pub target_fps: Option<u32>,
    /// Player-controller kind (e.g. `"desktop"`, `"pcvr"`); interpreted later.
    pub player_kind: Option<String>,
    /// Locomotion style (e.g. `"teleport"`, `"smooth"`); interpreted later.
    pub player_locomotion: Option<String>,
    /// Keep large referenced scenes as loose files — levels referenced via
    /// payloads, loaded on demand — instead of baking the whole dependency
    /// closure into one self-contained `stage.usdz`. Preserves instancing
    /// byte-for-byte and avoids the in-package read penalty for big scenes, at the
    /// cost of a larger packaged folder. Default `false` (flatten to `.usdz`).
    ///
    /// Skipped when `false` so a default manifest stays clean and both save paths
    /// agree: the comment-preserving patch writer (`patch_manifest_into_doc`)
    /// removes the key when `false`, and this makes the serde writer used by
    /// `IdtxProject::create` omit it too (they diverged otherwise).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loose_levels: bool,
    /// ⚠️ **TEMPORARY / SECURITY RISK.** Embed the current logged-in bearer token
    /// into the cooked `run.toml` (`[network].dev_bearer_token`) so a packaged
    /// **device** build (Quest / Vision Pro — no keychain) can authenticate remote
    /// (`http(s)://`) USD asset fetches. The resulting build carries logon
    /// credentials and **must not be distributed**. Opt-in, default `false`. Remove
    /// when the interactive logon-level flow lands — see
    /// `docs/plan_remote_refs_external_and_runtime_auth.md`. Skipped when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub embed_dev_token: bool,
    /// Android/Quest toolchain overrides (`[build.android]`). Serialized last
    /// because TOML requires sub-tables after a struct's scalar fields, and
    /// omitted entirely when no override is set (keeps a default manifest clean).
    #[serde(default, skip_serializing_if = "AndroidToolchainConfig::is_empty")]
    pub android: AndroidToolchainConfig,
    /// Apple Vision Pro signing/packaging overrides (`[build.visionos]`). Like
    /// `[build.android]` it is a sub-table, omitted when unset. Supplies the free
    /// Personal-Team signing identity + provisioning profile (auto-discovered
    /// when unset), the device bundle id, and hand-tracking opt-in.
    #[serde(default, skip_serializing_if = "VisionOsToolchainConfig::is_empty")]
    pub visionos: VisionOsToolchainConfig,
}

/// `[build.visionos]` — Apple Vision Pro signing/packaging settings for the
/// `run-visionos-device` xtask and the Conductor "Build Vision Pro Experience"
/// action. Every field is optional: an unset identity/profile is **auto-discovered**
/// (the `Apple Development` keychain identity + the newest matching
/// `.mobileprovision`), so a free Personal-Team dev build needs no config at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionOsToolchainConfig {
    /// `codesign` identity (`Apple Development: NAME (TEAMID)`); auto-discovered
    /// from the login keychain when unset.
    pub identity: Option<String>,
    /// Path to the development provisioning profile embedded in the `.app`;
    /// auto-discovered from `~/Library/MobileDevice/Provisioning Profiles` when
    /// unset.
    pub provisioning_profile: Option<PathBuf>,
    /// Explicit `CFBundleIdentifier` (must match the profile's
    /// `application-identifier` on device); derived/reconciled when unset.
    pub bundle_id: Option<String>,
    /// Signing team id (the `TEAMID`); read from the profile when unset.
    pub team_id: Option<String>,
    /// Output directory for the produced `.app` (overrides the default build dir).
    pub output_dir: Option<PathBuf>,
    /// Add the `ARKit` hand-tracking usage string + enable hand tracking.
    pub enable_hand_tracking: bool,
    /// Packaged texture-decode profile for the Vision Pro build. `None` → the
    /// device default ([`TextureQuality::Authoring`]), applied by the cook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_quality: Option<TextureQuality>,
}

impl VisionOsToolchainConfig {
    /// Whether no override is set — the manifest then omits the
    /// `[build.visionos]` table and everything is auto-discovered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.identity.is_none()
            && self.provisioning_profile.is_none()
            && self.bundle_id.is_none()
            && self.team_id.is_none()
            && self.output_dir.is_none()
            && !self.enable_hand_tracking
            && self.texture_quality.is_none()
    }
}

/// `[build.android]` — explicit toolchain roots for the Quest/Android build,
/// overriding environment discovery (`ANDROID_HOME`/`JAVA_HOME`/
/// `ANDROID_NDK_ROOT`). Each is optional; an unset field falls back to the
/// environment, then the per-OS default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AndroidToolchainConfig {
    /// JDK root (the dir whose `bin/` holds `jar`/`keytool`); overrides
    /// `JAVA_HOME`.
    pub jdk: Option<PathBuf>,
    /// Android SDK root (holds `build-tools/` + `platforms/`); overrides
    /// `ANDROID_HOME`/`ANDROID_SDK_ROOT`.
    pub sdk: Option<PathBuf>,
    /// Android NDK root used to cross-build the runtime; overrides
    /// `ANDROID_NDK_ROOT`.
    pub ndk: Option<PathBuf>,
    /// Packaged texture-decode profile for the Quest build. `None` → the device
    /// default ([`TextureQuality::Authoring`]), applied by the cook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture_quality: Option<TextureQuality>,
}

impl AndroidToolchainConfig {
    /// Whether no toolchain root is set (every field `None`) — the manifest then
    /// omits the `[build.android]` table and discovery falls back to the
    /// environment.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jdk.is_none() && self.sdk.is_none() && self.ndk.is_none() && self.texture_quality.is_none()
    }
}

/// The kind of a USD stage within a Conductor project, mirroring the root-layer
/// `customLayerData.idtx.stageKind` tag (roadmap D2 / Appendix A.1).
///
/// The tag itself is authored and read at the `idtx-usd` layer (which owns the
/// USD dependency); this enum is the project-domain taxonomy those raw token
/// strings map onto. Keeping it here — not in `idtx-usd` — lets the project
/// model reason about level/asset/assembly/player stages without pulling in the
/// USD toolchain, and gives the shell one place to translate a `read_stage_kind`
/// token into a typed kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageKind {
    /// A level stage — a single authorable/playable location. The project
    /// owns an ordered registry of these.
    Level,
    /// A reusable asset stage (prop, character, set piece) referenced by levels.
    Asset,
    /// An assembly stage composing levels plus player/HUD sublayers.
    Assembly,
    /// A player/HUD stage layered persistently across levels.
    Player,
    /// A transfer/transition stage — the lightweight scene shown while the next
    /// level loads during a level transition (see the level-transition plan).
    Transition,
}

impl StageKind {
    /// The canonical lowercase token stored in `customLayerData.idtx.stageKind`.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Level => "level",
            Self::Asset => "asset",
            Self::Assembly => "assembly",
            Self::Player => "player",
            Self::Transition => "transition",
        }
    }

    /// Parse a stage-kind token (case-insensitively), returning `None` for an
    /// unknown or empty token — e.g. an untagged stage read back as `""`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "level" => Some(Self::Level),
            "asset" => Some(Self::Asset),
            "assembly" => Some(Self::Assembly),
            "player" => Some(Self::Player),
            "transition" => Some(Self::Transition),
            _ => None,
        }
    }

    /// A short human-readable label for menus, headers, and the project browser.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Level => "Level",
            Self::Asset => "Asset",
            Self::Assembly => "Assembly",
            Self::Player => "Player",
            Self::Transition => "Transition",
        }
    }
}

/// The network-ownership role of a player pawn subtree, mirroring the
/// `token idtx:playerRole` authored on the pawn root (plan §2.1). This is the
/// **authority seam** that lets the runtime scale to online multiplayer: a
/// *local* pawn runs behaviors and consumes input, while a *remote* proxy is a
/// behavior-less visual whose transform arrives via replication.
///
/// The token itself is authored/read at the `idtx-usd` layer and hydrated into
/// an ECS `PlayerRole` component at the `idtx-scene` layer; this enum is the
/// **project-domain** taxonomy those raw tokens map onto — deliberately mirrored
/// here (rather than shared) because `idtx-project` does not depend on the
/// scene crate, exactly like [`StageKind`] mirrors the `idtx-usd` stage-kind
/// token. Both copies carry identical tokens, so they interoperate by string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetRole {
    /// Owned by this machine: runs behaviors, consumes input, drives its own
    /// transform. The default when no role is authored (single-player).
    Local,
    /// A proxy for another peer: **no behaviors ticked**; transform written by
    /// replication.
    Remote,
    /// Server-authoritative (single-player today behaves as [`Local`]; reserved
    /// so server-only behaviors can be gated in later).
    ///
    /// [`Local`]: Self::Local
    Server,
    /// No player attribution — the default for every non-player prim.
    Unowned,
}

impl NetRole {
    /// The canonical lowercase token stored in `idtx:playerRole`.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Server => "server",
            Self::Unowned => "unowned",
        }
    }

    /// Parse a player-role token (case-insensitively), returning `None` for an
    /// unknown or empty token — e.g. an unauthored role read back as `""`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "remote" => Some(Self::Remote),
            "server" => Some(Self::Server),
            "unowned" => Some(Self::Unowned),
            _ => None,
        }
    }

    /// A short human-readable label for inspectors and role badges.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Server => "Server",
            Self::Unowned => "Unowned",
        }
    }
}

/// Where a project's player stage resolves to.
///
/// The manifest's single [`player_stage`](ProjectManifest::player_stage) field
/// is either a project-relative path (resolved to [`Project`](Self::Project)) or
/// unset — in which case the project uses the toolbox's **built-in** player
/// stage ([`BuiltinDefault`](Self::BuiltinDefault)). `idtx-project` deliberately
/// does not depend on `idtx-assets`/`idtx-usd`, so it cannot name the built-in's
/// absolute path; the caller (cook / editor) maps `BuiltinDefault` to the
/// `built-in/players/Player.usd` search-path arc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerStage {
    /// A project-authored player stage, resolved to an absolute path inside the
    /// project sandbox.
    Project(PathBuf),
    /// No player stage configured — use the toolbox's built-in player.
    BuiltinDefault,
}

/// Normalize a project-relative path for storage and comparison in the levels
/// registry: drop `.` components and reject anything that is not a plain
/// relative path staying inside the root (absolute, `..`, or a root/prefix
/// component). Returns `None` for a rejected or empty path.
fn normalize_rel_level(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    // Collect the Normal components and re-join them with a forward slash so the
    // stored relative path is separator-normalized regardless of host OS. On
    // Windows `PathBuf::push` would use a backslash, which then serializes to the
    // manifest as a single-quoted literal (`'stages\level.usda'`) — divergent from
    // the forward-slash on-disk convention every other path field (default_stage,
    // shared_sublayers) and the editor's `norm()` use. A forward-slash string is a
    // valid relative path on every platform (`components()` parses it back), so
    // consumers (`resolve_relative`, `levels_equal`) are unaffected.
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| PathBuf::from(parts.join("/")))
}

/// Whether two level paths denote the same registry entry, comparing their
/// normalized forms so `./stages/a.usda` and `stages/a.usda` match.
fn levels_equal(a: &Path, b: &Path) -> bool {
    match (normalize_rel_level(a), normalize_rel_level(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// A parsed project manifest — the serde model of `project.toml`.
///
/// Constructed via [`IdtxProject::load`] / [`IdtxProject::discover`], or with
/// [`ProjectManifest::default`] for a brand-new project. Every field is
/// `#[serde(default)]` so partial or forward-compatible manifests still load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectManifest {
    /// Schema version; see [`PROJECT_SCHEMA_VERSION`].
    pub version: u32,
    /// Human-readable project name (shown in tabs / the project browser).
    pub name: String,
    /// The stage opened by default when the project is loaded, **relative to the
    /// project root**. `None` means "open no stage / show the empty state".
    pub default_stage: Option<PathBuf>,
    /// The level the packaged experience **starts on** (project-relative), tagged
    /// [`StageKind::Level`]. In a multi-level experience this is the only level
    /// loaded at boot; the others load on transition. `None` falls back to
    /// [`default_stage`](Self::default_stage), then the first registered level —
    /// see [`IdtxProject::resolved_start_level_abs`]. A single-level project may
    /// leave it unset.
    pub start_level: Option<PathBuf>,
    /// The **transfer/transition stage** (project-relative), tagged
    /// [`StageKind::Transition`] — the lightweight scene shown while the next
    /// level loads during a transition. `None` means "no masking scene" (the swap
    /// is a hard cut). See the level-transition plan.
    pub transfer_level: Option<PathBuf>,
    /// Additional asset search roots, relative to the project root. The project
    /// root itself is always an implicit asset root.
    pub asset_roots: Vec<PathBuf>,
    /// Ordered registry of level stages (project-relative paths). Manage via
    /// [`IdtxProject::add_level`] / [`remove_level`](IdtxProject::remove_level) /
    /// [`move_level`](IdtxProject::move_level); each entry names a stage tagged
    /// [`StageKind::Level`]. Empty for a project with no levels yet.
    pub levels: Vec<PathBuf>,
    /// The project's authored **assembly stage** (project-relative), tagged
    /// [`StageKind::Assembly`] — the outer stage that composes the registered
    /// levels as unloaded payload references plus the persistent player/HUD
    /// sublayer. `None` until one has been authored. Set via
    /// [`IdtxProject::set_assembly_stage`].
    pub assembly_stage: Option<PathBuf>,
    /// Metres-per-unit for this project.
    pub unit_scale: UnitScaleConfig,
    /// Packaging knobs (`[build]`).
    pub build: BuildConfig,
    /// The project's **player stage** (project-relative), tagged
    /// [`StageKind::Player`] — a standalone stage (root `def Xform "Player"`,
    /// `defaultPrim = "Player"`) that the assembly composer references into
    /// `/World/Player` at cook/preview time. `None` (the default) means "use the
    /// toolbox's built-in player stage" — see [`IdtxProject::player_stage`].
    pub player_stage: Option<PathBuf>,
    /// Shared global sublayers (project-relative) composed into **every** level's
    /// per-level assembly at cook time — the place for prototypes/definitions a
    /// whole experience shares without copying. Composed as root `subLayer`s of
    /// each per-level assembly; identical layer identifiers are deduplicated by
    /// USD across the per-level sessions. Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_sublayers: Vec<PathBuf>,
    /// Runtime HTTP policy for assets, REST datasources and behaviors
    /// (`[network]`). Deny-by-default; stamped into the `run.toml` emitted for
    /// both cooked packages and the runtime preview so the data-source policy is
    /// the same in all delivery modes.
    #[serde(default, skip_serializing_if = "NetworkProjectConfig::is_empty")]
    pub network: NetworkProjectConfig,
}

fn default_version() -> u32 {
    PROJECT_SCHEMA_VERSION
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            version: default_version(),
            name: String::new(),
            default_stage: None,
            start_level: None,
            transfer_level: None,
            asset_roots: Vec::new(),
            levels: Vec::new(),
            assembly_stage: None,
            unit_scale: UnitScaleConfig::default(),
            build: BuildConfig::default(),
            player_stage: None,
            shared_sublayers: Vec::new(),
            network: NetworkProjectConfig::default(),
        }
    }
}

impl ProjectManifest {
    /// A fresh manifest for a newly-created project with the given display name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Serialize this manifest to pretty TOML.
    ///
    /// # Errors
    /// Returns [`ProjectError::Serialize`] if serialization fails (should not
    /// happen for a well-formed manifest).
    pub fn to_toml_string(&self) -> Result<String, ProjectError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// A short content hash (first 8 bytes of SHA-256 over the TOML form), handy
    /// for correlating logs / reproducibility stamps with a specific manifest.
    /// Returns an empty string if the manifest cannot be serialized.
    #[must_use]
    pub fn content_hash(&self) -> String {
        let Ok(text) = self.to_toml_string() else {
            return String::new();
        };
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        hasher
            .finalize()
            .iter()
            .take(8)
            .fold(String::new(), |mut acc, b| {
                let _ = write!(acc, "{b:02x}");
                acc
            })
    }

    /// Whether this manifest configures a project-local player stage. `false`
    /// means the project falls back to the toolbox's built-in player — see
    /// [`IdtxProject::player_stage`].
    #[must_use]
    pub fn has_player_stage(&self) -> bool {
        self.player_stage
            .as_ref()
            .is_some_and(|p| !p.as_os_str().is_empty())
    }

}

/// Convert a `Path` to a forward-slash TOML string (invariant across platforms).
fn path_to_toml_str(p: &Path) -> String {
    p.to_string_lossy().into_owned().replace('\\', "/")
}

/// Apply all manifest values onto a parsed `toml_edit` document, preserving any
/// existing comments and key ordering. Falls back to `to_toml_string` if the raw
/// text cannot be re-parsed (should be impossible after a successful load).
fn patch_manifest_into_doc(raw: &str, m: &ProjectManifest) -> Result<String, ProjectError> {
    let Ok(mut doc) = raw.parse::<toml_edit::DocumentMut>() else {
        return m.to_toml_string();
    };
    let root = doc.as_table_mut();

    root["version"] = toml_edit::value(i64::from(m.version));
    root["name"] = toml_edit::value(m.name.clone());

    if let Some(p) = &m.default_stage {
        root["default_stage"] = toml_edit::value(path_to_toml_str(p));
    } else {
        root.remove("default_stage");
    }

    if let Some(p) = &m.start_level {
        root["start_level"] = toml_edit::value(path_to_toml_str(p));
    } else {
        root.remove("start_level");
    }

    if let Some(p) = &m.transfer_level {
        root["transfer_level"] = toml_edit::value(path_to_toml_str(p));
    } else {
        root.remove("transfer_level");
    }

    root["asset_roots"] = {
        let mut arr = toml_edit::Array::new();
        for p in &m.asset_roots {
            arr.push(path_to_toml_str(p));
        }
        toml_edit::value(arr)
    };

    root["levels"] = {
        let mut arr = toml_edit::Array::new();
        for p in &m.levels {
            arr.push(path_to_toml_str(p));
        }
        toml_edit::value(arr)
    };

    if let Some(p) = &m.assembly_stage {
        root["assembly_stage"] = toml_edit::value(path_to_toml_str(p));
    } else {
        root.remove("assembly_stage");
    }

    if !root.contains_key("unit_scale") {
        root["unit_scale"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if let Some(t) = root["unit_scale"].as_table_mut() {
        t["meters_per_unit"] = toml_edit::value(m.unit_scale.meters_per_unit);
    }

    if !root.contains_key("build") {
        root["build"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if let Some(t) = root["build"].as_table_mut() {
        t["extra_features"] = {
            let mut arr = toml_edit::Array::new();
            for f in &m.build.extra_features {
                arr.push(f.clone());
            }
            toml_edit::value(arr)
        };
        match m.build.msaa {
            Some(v) => t["msaa"] = toml_edit::value(i64::from(v)),
            None => {
                t.remove("msaa");
            }
        }
        match m.build.target_fps {
            Some(v) => t["target_fps"] = toml_edit::value(i64::from(v)),
            None => {
                t.remove("target_fps");
            }
        }
        match &m.build.player_kind {
            Some(v) => t["player_kind"] = toml_edit::value(v.clone()),
            None => {
                t.remove("player_kind");
            }
        }
        match &m.build.player_locomotion {
            Some(v) => t["player_locomotion"] = toml_edit::value(v.clone()),
            None => {
                t.remove("player_locomotion");
            }
        }
        // Written only when enabled (serde default is `false`), so a default
        // manifest stays clean.
        if m.build.loose_levels {
            t["loose_levels"] = toml_edit::value(true);
        } else {
            t.remove("loose_levels");
        }
        // TEMPORARY dev-token opt-in — written only when enabled (serde default
        // `false`), pruned otherwise so a default manifest stays clean.
        if m.build.embed_dev_token {
            t["embed_dev_token"] = toml_edit::value(true);
        } else {
            t.remove("embed_dev_token");
        }
        // `[build.android]` toolchain overrides — a nested table. Written only
        // when at least one root is set, pruned to a clean state otherwise.
        let a = &m.build.android;
        if a.is_empty() {
            t.remove("android");
        } else {
            if !t.contains_key("android") {
                t["android"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            if let Some(at) = t["android"].as_table_mut() {
                for (key, val) in [("jdk", &a.jdk), ("sdk", &a.sdk), ("ndk", &a.ndk)] {
                    match val {
                        Some(p) => at[key] = toml_edit::value(path_to_toml_str(p)),
                        None => {
                            at.remove(key);
                        }
                    }
                }
                match a.texture_quality {
                    Some(q) => at["texture_quality"] = toml_edit::value(q.as_token()),
                    None => {
                        at.remove("texture_quality");
                    }
                }
            }
        }
        // `[build.visionos]` signing/packaging overrides — a nested table,
        // written only when at least one field is set, pruned otherwise.
        let v = &m.build.visionos;
        if v.is_empty() {
            t.remove("visionos");
        } else {
            if !t.contains_key("visionos") {
                t["visionos"] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            if let Some(vt) = t["visionos"].as_table_mut() {
                for (key, val) in [
                    ("identity", v.identity.clone()),
                    ("bundle_id", v.bundle_id.clone()),
                    ("team_id", v.team_id.clone()),
                ] {
                    match val {
                        Some(s) => vt[key] = toml_edit::value(s),
                        None => {
                            vt.remove(key);
                        }
                    }
                }
                for (key, val) in [
                    ("provisioning_profile", &v.provisioning_profile),
                    ("output_dir", &v.output_dir),
                ] {
                    match val {
                        Some(p) => vt[key] = toml_edit::value(path_to_toml_str(p)),
                        None => {
                            vt.remove(key);
                        }
                    }
                }
                if v.enable_hand_tracking {
                    vt["enable_hand_tracking"] = toml_edit::value(true);
                } else {
                    vt.remove("enable_hand_tracking");
                }
                match v.texture_quality {
                    Some(q) => vt["texture_quality"] = toml_edit::value(q.as_token()),
                    None => {
                        vt.remove("texture_quality");
                    }
                }
            }
        }
    }

    // `player_stage` — an optional project-relative path. Written when set,
    // removed when unset (the project then uses the built-in player).
    if let Some(p) = &m.player_stage {
        root["player_stage"] = toml_edit::value(path_to_toml_str(p));
    } else {
        root.remove("player_stage");
    }

    // `shared_sublayers` — written only when non-empty (serde default is empty),
    // so a default manifest stays clean.
    if m.shared_sublayers.is_empty() {
        root.remove("shared_sublayers");
    } else {
        let mut arr = toml_edit::Array::new();
        for p in &m.shared_sublayers {
            arr.push(path_to_toml_str(p));
        }
        root["shared_sublayers"] = toml_edit::value(arr);
    }

    // `[network]` — omitted entirely when no capability is configured.
    let n = &m.network;
    if n.is_empty() {
        root.remove("network");
    } else {
        if !root.contains_key("network") {
            root["network"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        if let Some(nt) = root["network"].as_table_mut() {
            if n.allow {
                nt["allow"] = toml_edit::value(true);
            } else {
                nt.remove("allow");
            }
            if n.allowed_hosts.is_empty() {
                nt.remove("allowed_hosts");
            } else {
                let mut arr = toml_edit::Array::new();
                for h in &n.allowed_hosts {
                    arr.push(h.clone());
                }
                nt["allowed_hosts"] = toml_edit::value(arr);
            }
            if n.private_hosts.is_empty() {
                nt.remove("private_hosts");
            } else {
                let mut hosts = toml_edit::Array::new();
                for host in &n.private_hosts { hosts.push(host.clone()); }
                nt["private_hosts"] = toml_edit::value(hosts);
            }
            if n.auth_origins.is_empty() {
                nt.remove("auth_origins");
            } else {
                let mut origins = toml_edit::Array::new();
                for origin in &n.auth_origins { origins.push(origin.clone()); }
                nt["auth_origins"] = toml_edit::value(origins);
            }
            if n.max_response_mb == 0 {
                nt.remove("max_response_mb");
            } else {
                nt["max_response_mb"] = toml_edit::value(i64::from(n.max_response_mb));
            }
            if n.max_asset_mb == 0 {
                nt.remove("max_asset_mb");
            } else {
                nt["max_asset_mb"] = toml_edit::value(i64::from(n.max_asset_mb));
            }
        }
    }

    Ok(doc.to_string())
}

/// An opened project: its canonical root directory plus the parsed manifest and
/// the on-disk path the manifest was loaded from / will be saved to.
#[derive(Debug, Clone)]
pub struct IdtxProject {
    root: ProjectRoot,
    manifest_path: PathBuf,
    manifest: ProjectManifest,
    /// Original on-disk text captured at load time; used as the base for
    /// comment-preserving saves via `toml_edit`. `None` for newly created
    /// projects (no prior file to preserve comments from).
    raw_toml: Option<String>,
}

impl IdtxProject {
    /// The canonical `project.toml` path for a project rooted at `root`.
    #[must_use]
    pub fn manifest_path_for(root: &Path) -> PathBuf {
        root.join(PROJECT_MANIFEST_FILENAME)
    }

    /// Whether `root` is a project directory (contains a `project.toml`).
    #[must_use]
    pub fn is_project_dir(root: &Path) -> bool {
        Self::manifest_path_for(root).is_file()
    }

    /// Load a project from an explicit manifest **file** path (the CLI seam).
    ///
    /// The project root is the manifest file's parent directory, canonicalized.
    ///
    /// # Errors
    /// [`ProjectError`] when the file cannot be read, is not valid TOML for this
    /// schema, declares an unsupported schema version, or has no parent
    /// directory to use as the project root.
    pub fn load(manifest_file: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let manifest_path = manifest_file.as_ref();
        let parent = manifest_path.parent().filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| ProjectError::NoRoot { path: manifest_path.to_path_buf() })?;
        let root = std::fs::canonicalize(parent).and_then(ProjectRoot::from_canonical)
            .map_err(|source| ProjectError::Io { path: parent.to_path_buf(), source })?;
        let name = manifest_path.file_name().ok_or_else(|| ProjectError::NoRoot { path: manifest_path.to_path_buf() })?;
        let text = root.prepare_file(Path::new(name)).and_then(|file| file.read_to_string())
            .map_err(|source| ProjectError::Io {
            path: manifest_path.to_path_buf(),
            source,
        })?;
        let manifest: ProjectManifest =
            toml::from_str(&text).map_err(|source| ProjectError::Parse {
                path: manifest_path.to_path_buf(),
                source,
            })?;
        if manifest.version > PROJECT_SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedVersion {
                found: manifest.version,
                supported: PROJECT_SCHEMA_VERSION,
            });
        }
        let manifest_path = root.as_path().join(PROJECT_MANIFEST_FILENAME);
        Ok(Self {
            root,
            manifest_path,
            manifest,
            raw_toml: Some(text),
        })
    }

    /// Open the project rooted at directory `root`, if it is one.
    ///
    /// Returns `Ok(None)` when `root` has no `project.toml` (not an error — the
    /// directory simply is not a project).
    ///
    /// # Errors
    /// [`ProjectError`] when the manifest exists but fails to read/parse/validate.
    pub fn open_dir(root: impl AsRef<Path>) -> Result<Option<Self>, ProjectError> {
        let manifest_path = Self::manifest_path_for(root.as_ref());
        if !manifest_path.is_file() {
            return Ok(None);
        }
        Self::load(&manifest_path).map(Some)
    }

    /// Walk the parent chain from `hint` upward, returning the first directory
    /// that is a valid project. This is the backbone of the future
    /// create/select-project dialog and of opening a stage that happens to live
    /// inside a project tree.
    ///
    /// A manifest that exists but fails to parse stops the walk and is reported
    /// as an error (a broken project is more useful surfaced than skipped).
    ///
    /// # Errors
    /// [`ProjectError`] when a `project.toml` is found but cannot be parsed.
    pub fn discover(hint: impl AsRef<Path>) -> Result<Option<Self>, ProjectError> {
        let mut cursor = Some(hint.as_ref());
        while let Some(dir) = cursor {
            if Self::is_project_dir(dir) {
                return Self::load(Self::manifest_path_for(dir)).map(Some);
            }
            cursor = dir.parent();
        }
        Ok(None)
    }

    /// Create a new project rooted at `root` with the given manifest, writing
    /// `project.toml` to disk. The root directory is created if missing.
    ///
    /// # Errors
    /// [`ProjectError`] when the directory or manifest cannot be written, or the
    /// manifest cannot be serialized.
    pub fn create(root: impl AsRef<Path>, manifest: ProjectManifest) -> Result<Self, ProjectError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|source| ProjectError::Write {
            path: root.to_path_buf(),
            source,
        })?;
        let root = std::fs::canonicalize(root).and_then(ProjectRoot::from_canonical)
            .map_err(|source| ProjectError::Write { path: root.to_path_buf(), source })?;
        let project = Self {
            manifest_path: Self::manifest_path_for(root.as_path()),
            root,
            manifest,
            raw_toml: None,
        };
        project.save()?;
        Ok(project)
    }

    /// Create a new project in a single new child of an existing selected parent.
    /// The child is created exclusively; existing entries are never reused.
    pub fn create_child(parent: &Path, name: &Path, manifest: ProjectManifest) -> Result<Self, ProjectError> {
        let root = std::fs::canonicalize(parent).and_then(ProjectRoot::from_canonical)
            .and_then(|parent| parent.create_child(name))
            .map_err(|source| ProjectError::Write { path: parent.join(name), source })?;
        let project = Self {
            manifest_path: Self::manifest_path_for(root.as_path()),
            root, manifest, raw_toml: None,
        };
        let text = project.manifest.to_toml_string()?;
        project.root.prepare_file(Path::new(PROJECT_MANIFEST_FILENAME))
            .and_then(|file| file.write_new(text))
            .map_err(|source| ProjectError::Write { path: project.manifest_path.clone(), source })?;
        Ok(project)
    }

    /// Persist the current manifest to its on-disk `project.toml`.
    ///
    /// When the project was loaded from an existing file the original text is used
    /// as a base (via `toml_edit`) so that comments and key ordering are preserved.
    /// Newly created projects (no prior file) fall back to pretty-printed TOML.
    ///
    /// # Errors
    /// [`ProjectError`] when the manifest cannot be serialized or written.
    pub fn save(&self) -> Result<(), ProjectError> {
        let text = match &self.raw_toml {
            Some(raw) => patch_manifest_into_doc(raw, &self.manifest)?,
            None => self.manifest.to_toml_string()?,
        };
        self.root.prepare_file(Path::new(PROJECT_MANIFEST_FILENAME))
            .and_then(|file| file.write(text)).map_err(|source| ProjectError::Write {
            path: self.manifest_path.clone(),
            source,
        })
    }

    /// The canonical project root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// The on-disk manifest path (`<root>/project.toml`).
    #[must_use]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// The parsed manifest.
    #[must_use]
    pub fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }

    /// Mutable access to the manifest (call [`save`](Self::save) to persist).
    pub fn manifest_mut(&mut self) -> &mut ProjectManifest {
        &mut self.manifest
    }

    /// The project display name, falling back to the root directory's file name
    /// when the manifest carries no explicit name.
    #[must_use]
    pub fn name(&self) -> String {
        if !self.manifest.name.trim().is_empty() {
            return self.manifest.name.clone();
        }
        self.root.as_path().file_name().map_or_else(
            || "Untitled Project".to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// The absolute path of the project's default stage, if configured and if it
    /// resolves inside the project root sandbox.
    #[must_use]
    pub fn default_stage_abs(&self) -> Option<PathBuf> {
        let rel = self.manifest.default_stage.as_ref()?;
        self.project_root().resolve_relative(rel)
    }

    /// The project's authored assembly stage (project-relative), if any.
    #[must_use]
    pub fn assembly_stage(&self) -> Option<&Path> {
        self.manifest.assembly_stage.as_deref()
    }

    /// The absolute path of the project's assembly stage, if configured and if it
    /// resolves inside the project root sandbox.
    #[must_use]
    pub fn assembly_stage_abs(&self) -> Option<PathBuf> {
        let rel = self.manifest.assembly_stage.as_ref()?;
        self.project_root().resolve_relative(rel)
    }

    /// Record `path` as the project's assembly stage.
    ///
    /// Accepts a project-relative path or an absolute path inside the root
    /// (which is relativized). Returns `true` if it was recorded, `false` if the
    /// path escapes the sandbox. Call [`save`](Self::save) to persist.
    pub fn set_assembly_stage(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(rel) = self.to_level_rel(path.as_ref()) else {
            return false;
        };
        self.manifest.assembly_stage = Some(rel);
        true
    }

    /// The project-root sandbox guard, for resolving project-relative asset
    /// paths safely (see [`ProjectRoot`]).
    #[must_use]
    pub fn project_root(&self) -> ProjectRoot {
        self.root.clone()
    }

    /// The ordered registry of level stages (project-relative paths).
    #[must_use]
    pub fn levels(&self) -> &[PathBuf] {
        &self.manifest.levels
    }

    /// Absolute paths of the registered levels that resolve inside the sandbox,
    /// in registry order. Entries that escape the root are skipped.
    #[must_use]
    pub fn levels_abs(&self) -> Vec<PathBuf> {
        let root = self.project_root();
        self.manifest
            .levels
            .iter()
            .filter_map(|rel| root.resolve_relative(rel))
            .collect()
    }

    /// The configured start level (project-relative), if any. See
    /// [`resolved_start_level_abs`](Self::resolved_start_level_abs) for the
    /// fallback-applied absolute form the cook/runtime consume.
    #[must_use]
    pub fn start_level(&self) -> Option<&Path> {
        self.manifest.start_level.as_deref()
    }

    /// The absolute path of the configured start level, if set and resolvable
    /// inside the sandbox. Does **not** apply the fallback — use
    /// [`resolved_start_level_abs`](Self::resolved_start_level_abs) for that.
    #[must_use]
    pub fn start_level_abs(&self) -> Option<PathBuf> {
        let rel = self.manifest.start_level.as_ref()?;
        self.project_root().resolve_relative(rel)
    }

    /// The absolute start-level path with the fallback chain applied:
    /// `start_level` → [`default_stage`](Self::default_stage_abs) → the first
    /// registered level. `None` only when none of the three resolves inside the
    /// sandbox (e.g. a project with no levels and no default stage).
    #[must_use]
    pub fn resolved_start_level_abs(&self) -> Option<PathBuf> {
        self.start_level_abs()
            .or_else(|| self.default_stage_abs())
            .or_else(|| self.levels_abs().into_iter().next())
    }

    /// Record `path` as the project's start level. Accepts a project-relative
    /// path or an absolute path inside the root. Returns `false` if the path
    /// escapes the sandbox. Call [`save`](Self::save) to persist.
    pub fn set_start_level(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(rel) = self.to_level_rel(path.as_ref()) else {
            return false;
        };
        self.manifest.start_level = Some(rel);
        true
    }

    /// Clear the configured start level (the project then uses the fallback).
    pub fn clear_start_level(&mut self) {
        self.manifest.start_level = None;
    }

    /// The configured transfer/transition stage (project-relative), if any.
    #[must_use]
    pub fn transfer_level(&self) -> Option<&Path> {
        self.manifest.transfer_level.as_deref()
    }

    /// The absolute path of the transfer/transition stage, if set and resolvable
    /// inside the sandbox.
    #[must_use]
    pub fn transfer_level_abs(&self) -> Option<PathBuf> {
        let rel = self.manifest.transfer_level.as_ref()?;
        self.project_root().resolve_relative(rel)
    }

    /// Record `path` as the project's transfer/transition stage. Returns `false`
    /// if the path escapes the sandbox. Call [`save`](Self::save) to persist.
    pub fn set_transfer_level(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(rel) = self.to_level_rel(path.as_ref()) else {
            return false;
        };
        self.manifest.transfer_level = Some(rel);
        true
    }

    /// Clear the configured transfer/transition stage (transitions then hard-cut).
    pub fn clear_transfer_level(&mut self) {
        self.manifest.transfer_level = None;
    }

    /// The shared global sublayers (project-relative), in authored order.
    #[must_use]
    pub fn shared_sublayers(&self) -> &[PathBuf] {
        &self.manifest.shared_sublayers
    }

    /// Absolute paths of the shared global sublayers that resolve inside the
    /// sandbox, in authored order. Entries that escape the root are skipped.
    #[must_use]
    pub fn shared_sublayers_abs(&self) -> Vec<PathBuf> {
        let root = self.project_root();
        self.manifest
            .shared_sublayers
            .iter()
            .filter_map(|rel| root.resolve_relative(rel))
            .collect()
    }

    /// The project's **player stage**: either a project-authored stage
    /// ([`PlayerStage::Project`], resolved to an absolute path inside the
    /// sandbox) or the toolbox's built-in player
    /// ([`PlayerStage::BuiltinDefault`]) when `player_stage` is unset, empty, or
    /// escapes the project root.
    ///
    /// This is the single source of truth both the editor preview and the cook
    /// assembly composer consult. The caller maps `BuiltinDefault` to the
    /// `built-in/players/Player.usd` search-path arc (`idtx-project` cannot name
    /// the built-in's absolute path without depending on `idtx-assets`).
    #[must_use]
    pub fn player_stage(&self) -> PlayerStage {
        let Some(rel) = self.manifest.player_stage.as_ref() else {
            return PlayerStage::BuiltinDefault;
        };
        if rel.as_os_str().is_empty() {
            return PlayerStage::BuiltinDefault;
        }
        match self.project_root().resolve_relative(rel) {
            Some(abs) => PlayerStage::Project(abs),
            None => PlayerStage::BuiltinDefault,
        }
    }

    /// The absolute path of the project-authored player stage, or `None` when
    /// the project uses the built-in default (see [`Self::player_stage`]).
    /// Convenience for callers that cannot resolve the built-in themselves.
    #[must_use]
    pub fn player_stage_abs(&self) -> Option<PathBuf> {
        match self.player_stage() {
            PlayerStage::Project(abs) => Some(abs),
            PlayerStage::BuiltinDefault => None,
        }
    }

    /// Record `path` (project-relative, or absolute inside the root) as the
    /// project's player stage. Returns `false` if the path escapes the sandbox.
    /// Call [`save`](Self::save) to persist.
    pub fn set_player_stage(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(rel) = self.to_level_rel(path.as_ref()) else {
            return false;
        };
        self.manifest.player_stage = Some(rel);
        true
    }

    /// List the immediate children of a project-relative directory (empty =
    /// the project root), sandbox-confined. See [`assets::list_dir`].
    #[must_use]
    pub fn list_assets(&self, subdir: impl AsRef<Path>) -> Vec<AssetEntry> {
        assets::list_dir(&self.project_root(), subdir.as_ref())
    }

    /// The project's asset search roots (project-relative): the implicit root
    /// (`""`) first, then each configured `asset_roots` entry that stays inside
    /// the sandbox, de-duplicated. Feeds the resolver context and the
    /// asset-browser root list.
    #[must_use]
    pub fn asset_search_roots(&self) -> Vec<PathBuf> {
        let root = self.project_root();
        let mut roots = vec![PathBuf::new()];
        for extra in &self.manifest.asset_roots {
            // Keep only roots that resolve inside the sandbox, and skip the
            // implicit root duplicate.
            if !extra.as_os_str().is_empty()
                && root.resolve_relative(extra).is_some()
                && !roots.contains(extra)
            {
                roots.push(extra.clone());
            }
        }
        roots
    }

    /// Absolute paths of the asset search roots (see [`Self::asset_search_roots`])
    /// that resolve inside the sandbox, in search order. This feeds the USD
    /// default-resolver search path so bare/project-relative references anchor
    /// at the project's asset roots.
    #[must_use]
    pub fn asset_search_dirs_abs(&self) -> Vec<PathBuf> {
        let root = self.project_root();
        self.asset_search_roots()
            .iter()
            .filter_map(|rel| root.resolve_relative(rel))
            .collect()
    }

    /// Whether `path` (project-relative, or absolute inside the root) is already
    /// registered as a level.
    #[must_use]
    pub fn is_level_registered(&self, path: impl AsRef<Path>) -> bool {
        self.to_level_rel(path.as_ref())
            .is_some_and(|rel| self.manifest.levels.iter().any(|l| levels_equal(l, &rel)))
    }

    /// Append `path` to the level registry.
    ///
    /// Accepts a project-relative path or an absolute path inside the root
    /// (which is relativized). Returns `true` if it was added, `false` if the
    /// path escapes the sandbox or is already registered. Call
    /// [`save`](Self::save) to persist.
    pub fn add_level(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(rel) = self.to_level_rel(path.as_ref()) else {
            return false;
        };
        if self.manifest.levels.iter().any(|l| levels_equal(l, &rel)) {
            return false;
        }
        self.manifest.levels.push(rel);
        true
    }

    /// Remove a level from the registry. Returns `true` if a matching entry was
    /// removed. Call [`save`](Self::save) to persist.
    pub fn remove_level(&mut self, path: impl AsRef<Path>) -> bool {
        // Invalid/missing registry entries must remain removable from the UI.
        let Some(rel) = (if path.as_ref().is_absolute() {
            self.to_level_rel(path.as_ref())
        } else {
            normalize_rel_level(path.as_ref())
        }) else {
            return false;
        };
        let before = self.manifest.levels.len();
        self.manifest.levels.retain(|l| !levels_equal(l, &rel));
        self.manifest.levels.len() != before
    }

    /// Move the level at index `from` to index `to`, shifting the rest to keep
    /// the registry contiguous. Returns `false` if either index is out of range.
    /// Call [`save`](Self::save) to persist.
    pub fn move_level(&mut self, from: usize, to: usize) -> bool {
        let len = self.manifest.levels.len();
        if from >= len || to >= len {
            return false;
        }
        if from != to {
            let item = self.manifest.levels.remove(from);
            self.manifest.levels.insert(to, item);
        }
        true
    }

    /// Convert an input path (project-relative, or absolute inside the root) to
    /// the normalized project-relative form used in the levels registry, or
    /// `None` if it lies outside the sandbox.
    fn to_level_rel(&self, path: &Path) -> Option<PathBuf> {
        normalize_rel_level(&self.root.relative_path(path).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir under the system temp dir (no `tempfile` dep) — mirrors
    /// the pattern used in `idtx-runtime-manifest`.
    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "idtx-project-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_manifest_is_current_version() {
        let m = ProjectManifest::default();
        assert_eq!(m.version, PROJECT_SCHEMA_VERSION);
        assert!((m.unit_scale.meters_per_unit() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn toml_round_trips() {
        let mut m = ProjectManifest::new("Demo");
        m.default_stage = Some(PathBuf::from("stages/main.usda"));
        m.asset_roots.push(PathBuf::from("assets"));
        m.build.msaa = Some(4);
        let text = m.to_toml_string().unwrap();
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn build_android_toolchain_round_trips() {
        let mut m = ProjectManifest::new("Demo");
        m.build.android.jdk = Some(PathBuf::from("C:/jdk-17"));
        m.build.android.sdk = Some(PathBuf::from("C:/Android/Sdk"));
        m.build.android.ndk = Some(PathBuf::from("C:/Android/Sdk/ndk/27.0.0"));
        let text = m.to_toml_string().unwrap();
        assert!(text.contains("[build.android]"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.build.android.jdk.as_deref(), Some(Path::new("C:/jdk-17")));
    }

    #[test]
    fn build_android_absent_by_default() {
        // An unset toolchain writes no `[build.android]` table and round-trips
        // back to all-None.
        let m = ProjectManifest::new("Demo");
        let text = m.to_toml_string().unwrap();
        assert!(!text.contains("[build.android]"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(back.build.android, AndroidToolchainConfig::default());
    }

    #[test]
    fn build_visionos_round_trips() {
        let mut m = ProjectManifest::new("Demo");
        m.build.visionos.identity = Some("Apple Development: Jane Doe (TEAMID1234)".to_string());
        m.build.visionos.provisioning_profile = Some(PathBuf::from("signing/dev.mobileprovision"));
        m.build.visionos.bundle_id = Some("com.example.demo".to_string());
        m.build.visionos.team_id = Some("TEAMID1234".to_string());
        m.build.visionos.enable_hand_tracking = true;
        let text = m.to_toml_string().unwrap();
        assert!(text.contains("[build.visionos]"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.build.visionos.bundle_id.as_deref(), Some("com.example.demo"));
        assert!(back.build.visionos.enable_hand_tracking);
    }

    #[test]
    fn build_visionos_hand_tracking_only_still_writes_table() {
        // Even when only the bool is set (every path/string None), `is_empty` is
        // false so the table must be written and round-trip.
        let mut m = ProjectManifest::new("Demo");
        m.build.visionos.enable_hand_tracking = true;
        let text = m.to_toml_string().unwrap();
        assert!(text.contains("[build.visionos]"), "toml = {text}");
        assert!(text.contains("enable_hand_tracking"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert!(back.build.visionos.enable_hand_tracking);
    }

    #[test]
    fn build_visionos_absent_by_default() {
        let m = ProjectManifest::new("Demo");
        let text = m.to_toml_string().unwrap();
        assert!(!text.contains("[build.visionos]"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(back.build.visionos, VisionOsToolchainConfig::default());
    }

    #[test]
    fn create_load_save_round_trip() {
        let dir = scratch("create");
        let project = IdtxProject::create(&dir, ProjectManifest::new("Demo")).unwrap();
        assert!(IdtxProject::is_project_dir(project.root()));
        assert_eq!(project.name(), "Demo");

        // Load it back from the manifest file (the CLI seam).
        let loaded = IdtxProject::load(project.manifest_path()).unwrap();
        assert_eq!(loaded.manifest(), project.manifest());
        assert_eq!(loaded.root(), project.root());
    }

    #[test]
    fn name_falls_back_to_dir() {
        let dir = scratch("noname");
        let project = IdtxProject::create(&dir, ProjectManifest::default()).unwrap();
        // The manifest carries no name → the root dir's file name is used.
        let expected = project.root().file_name().unwrap().to_string_lossy();
        assert_eq!(project.name(), expected);
    }

    #[test]
    fn discover_walks_up_parent_chain() {
        let dir = scratch("discover");
        IdtxProject::create(&dir, ProjectManifest::new("Root")).unwrap();
        let nested = dir.join("levels").join("hub");
        std::fs::create_dir_all(&nested).unwrap();
        let found = IdtxProject::discover(&nested).unwrap().expect("discovered");
        assert_eq!(found.name(), "Root");
    }

    #[test]
    fn open_dir_none_when_not_a_project() {
        let dir = scratch("empty");
        assert!(IdtxProject::open_dir(&dir).unwrap().is_none());
    }

    #[test]
    fn rejects_future_version() {
        let dir = scratch("ver");
        let path = IdtxProject::manifest_path_for(&dir);
        std::fs::write(&path, format!("version = {}\n", PROJECT_SCHEMA_VERSION + 1)).unwrap();
        let err = IdtxProject::load(&path).unwrap_err();
        assert!(matches!(err, ProjectError::UnsupportedVersion { .. }));
    }

    #[test]
    fn default_stage_resolves_inside_root_only() {
        let dir = scratch("stage");
        let mut m = ProjectManifest::new("Stages");
        m.default_stage = Some(PathBuf::from("stages/main.usda"));
        let project = IdtxProject::create(&dir, m).unwrap();
        let abs = project.default_stage_abs().expect("resolves");
        assert!(abs.starts_with(project.root()));
        assert!(abs.ends_with("main.usda"));

        // An escaping default_stage must not resolve.
        let mut m2 = ProjectManifest::new("Escape");
        m2.default_stage = Some(PathBuf::from("../../../etc/passwd"));
        let project2 = IdtxProject::create(scratch("stage2"), m2).unwrap();
        assert!(project2.default_stage_abs().is_none());
    }

    #[test]
    fn assembly_stage_records_relativizes_and_resolves() {
        let dir = scratch("assembly");
        let project_res = IdtxProject::create(&dir, ProjectManifest::new("Assembly")).unwrap();
        // Freshly created: no assembly stage yet.
        assert!(project_res.assembly_stage().is_none());
        assert!(project_res.assembly_stage_abs().is_none());

        let mut project = project_res;
        // A project-relative path is recorded verbatim and resolves inside root.
        assert!(project.set_assembly_stage("assembly.usda"));
        assert_eq!(project.assembly_stage(), Some(Path::new("assembly.usda")));
        let abs = project.assembly_stage_abs().expect("resolves");
        assert!(abs.starts_with(project.root()));
        assert!(abs.ends_with("assembly.usda"));

        // An escaping path is rejected and leaves the prior value untouched.
        assert!(!project.set_assembly_stage("../../elsewhere.usda"));
        assert_eq!(project.assembly_stage(), Some(Path::new("assembly.usda")));
    }

    #[test]
    fn assembly_stage_survives_toml_round_trip() {
        let dir = scratch("assembly-rt");
        let mut m = ProjectManifest::new("RoundTrip");
        m.assembly_stage = Some(PathBuf::from("stages/assembly.usda"));
        let project = IdtxProject::create(&dir, m).unwrap();
        project.save().unwrap();
        let reloaded = IdtxProject::open_dir(&dir)
            .unwrap()
            .expect("project exists");
        assert_eq!(
            reloaded.assembly_stage(),
            Some(Path::new("stages/assembly.usda"))
        );
    }

    #[test]
    fn start_and_transfer_level_record_relativize_and_resolve() {
        let dir = scratch("startxfer");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Nav")).unwrap();
        // Freshly created: neither is set.
        assert!(project.start_level().is_none());
        assert!(project.transfer_level().is_none());

        assert!(project.set_start_level("stages/forest.usda"));
        assert!(project.set_transfer_level("stages/_transfer.usda"));
        assert_eq!(project.start_level(), Some(Path::new("stages/forest.usda")));
        assert_eq!(
            project.transfer_level(),
            Some(Path::new("stages/_transfer.usda"))
        );
        assert!(
            project
                .start_level_abs()
                .expect("start resolves")
                .starts_with(project.root())
        );
        assert!(
            project
                .transfer_level_abs()
                .expect("transfer resolves")
                .ends_with("_transfer.usda")
        );

        // Escaping paths are rejected and leave the prior values untouched.
        assert!(!project.set_start_level("../../../etc/passwd"));
        assert_eq!(project.start_level(), Some(Path::new("stages/forest.usda")));

        // Clearing restores the fallback / hard-cut behaviour.
        project.clear_start_level();
        project.clear_transfer_level();
        assert!(project.start_level().is_none());
        assert!(project.transfer_level().is_none());
    }

    #[test]
    fn resolved_start_level_applies_fallback_chain() {
        let dir = scratch("startfallback");
        // No start_level, no default_stage → first registered level wins.
        let mut m = ProjectManifest::new("Fallback");
        m.levels = vec![
            PathBuf::from("stages/a.usda"),
            PathBuf::from("stages/b.usda"),
        ];
        let project = IdtxProject::create(&dir, m).unwrap();
        assert!(
            project
                .resolved_start_level_abs()
                .expect("falls back to first level")
                .ends_with("a.usda")
        );

        // default_stage beats the first level when start_level is unset.
        let mut m = ProjectManifest::new("Fallback2");
        m.default_stage = Some(PathBuf::from("stages/b.usda"));
        m.levels = vec![PathBuf::from("stages/a.usda")];
        let project = IdtxProject::create(scratch("startfallback2"), m).unwrap();
        assert!(
            project
                .resolved_start_level_abs()
                .expect("falls back to default_stage")
                .ends_with("b.usda")
        );

        // An explicit start_level beats both.
        let mut m = ProjectManifest::new("Fallback3");
        m.default_stage = Some(PathBuf::from("stages/b.usda"));
        m.start_level = Some(PathBuf::from("stages/c.usda"));
        m.levels = vec![PathBuf::from("stages/a.usda")];
        let project = IdtxProject::create(scratch("startfallback3"), m).unwrap();
        assert!(
            project
                .resolved_start_level_abs()
                .expect("explicit start_level wins")
                .ends_with("c.usda")
        );
    }

    #[test]
    fn texture_quality_token_round_trips() {
        for q in [
            TextureQuality::FullResolution,
            TextureQuality::Authoring,
            TextureQuality::Inspector,
        ] {
            assert_eq!(TextureQuality::from_token(q.as_token()), Some(q));
        }
        // Case-insensitive + friendly aliases; unknown → None.
        assert_eq!(
            TextureQuality::from_token("  FULL  "),
            Some(TextureQuality::FullResolution)
        );
        assert_eq!(
            TextureQuality::from_token("balanced"),
            Some(TextureQuality::Inspector)
        );
        assert_eq!(TextureQuality::from_token(""), None);
        assert_eq!(TextureQuality::from_token("nope"), None);
    }

    #[test]
    fn shared_sublayers_record_and_resolve() {
        let dir = scratch("sharedsub");
        let mut m = ProjectManifest::new("Shared");
        m.shared_sublayers = vec![
            PathBuf::from("stages/_globals/prototypes.usda"),
            PathBuf::from("../escapes.usda"),
        ];
        let project = IdtxProject::create(&dir, m).unwrap();
        assert_eq!(project.shared_sublayers().len(), 2);
        // Only the in-sandbox entry resolves; the escaping one is skipped.
        let abs = project.shared_sublayers_abs();
        assert_eq!(abs.len(), 1);
        assert!(abs[0].ends_with("prototypes.usda"));
    }

    #[test]
    fn nav_fields_survive_comment_preserving_save() {
        // Exercises patch_manifest_into_doc (the toml_edit writer), not just serde.
        let dir = scratch("nav-rt");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Nav")).unwrap();
        project.save().unwrap(); // seed an on-disk file so saves go through toml_edit
        assert!(project.set_start_level("stages/forest.usda"));
        assert!(project.set_transfer_level("stages/_transfer.usda"));
        project.manifest_mut().shared_sublayers = vec![PathBuf::from("stages/globals.usda")];
        project.save().unwrap();

        let text = std::fs::read_to_string(dir.join(PROJECT_MANIFEST_FILENAME)).unwrap();
        assert!(text.contains("start_level = \"stages/forest.usda\""), "{text}");
        assert!(text.contains("transfer_level = \"stages/_transfer.usda\""), "{text}");
        assert!(text.contains("stages/globals.usda"), "{text}");

        let reloaded = IdtxProject::open_dir(&dir).unwrap().expect("exists");
        assert_eq!(
            reloaded.start_level(),
            Some(Path::new("stages/forest.usda"))
        );
        assert_eq!(
            reloaded.transfer_level(),
            Some(Path::new("stages/_transfer.usda"))
        );
        assert_eq!(reloaded.shared_sublayers().len(), 1);
    }

    #[test]
    fn default_manifest_omits_new_nav_fields() {
        // A clean manifest must not emit the new keys (serde skips None/empty).
        let text = ProjectManifest::new("Clean").to_toml_string().unwrap();
        assert!(!text.contains("start_level"), "{text}");
        assert!(!text.contains("transfer_level"), "{text}");
        assert!(!text.contains("shared_sublayers"), "{text}");
    }

    #[test]
    fn content_hash_is_stable_hex() {
        let m = ProjectManifest::new("Hash");
        let a = m.content_hash();
        let b = m.content_hash();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn unit_scale_clamps_non_positive() {
        let cfg = UnitScaleConfig {
            meters_per_unit: 0.0,
        };
        assert!((cfg.meters_per_unit() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn stage_kind_token_round_trips() {
        for kind in [
            StageKind::Level,
            StageKind::Asset,
            StageKind::Assembly,
            StageKind::Player,
            StageKind::Transition,
        ] {
            assert_eq!(StageKind::from_token(kind.as_token()), Some(kind));
        }
        // Case-insensitive and whitespace-tolerant; unknown/empty → None.
        assert_eq!(StageKind::from_token("  LEVEL "), Some(StageKind::Level));
        assert_eq!(StageKind::from_token(""), None);
        assert_eq!(StageKind::from_token("prop"), None);
    }

    #[test]
    fn net_role_token_round_trips() {
        for role in [
            NetRole::Local,
            NetRole::Remote,
            NetRole::Server,
            NetRole::Unowned,
        ] {
            assert_eq!(NetRole::from_token(role.as_token()), Some(role));
        }
        // Case-insensitive and whitespace-tolerant; unknown/empty → None.
        assert_eq!(NetRole::from_token("  REMOTE "), Some(NetRole::Remote));
        assert_eq!(NetRole::from_token(""), None);
        assert_eq!(NetRole::from_token("spectator"), None);
    }

    #[test]
    fn player_stage_survives_toml_round_trip_via_serde() {
        let mut m = ProjectManifest::new("Player");
        m.player_stage = Some(PathBuf::from("stages/player.usda"));
        let text = m.to_toml_string().unwrap();
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.player_stage, Some(PathBuf::from("stages/player.usda")));
    }

    #[test]
    fn player_stage_survives_comment_preserving_save() {
        let dir = scratch("player-stage-save");
        let m = ProjectManifest::new("PlayerSave");
        // create() writes via to_toml_string; reload, mutate, then save() goes
        // through the toml_edit patch path (raw_toml is Some after load).
        IdtxProject::create(&dir, m).unwrap();
        let mut project = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert!(project.manifest().player_stage.is_none());
        assert!(project.set_player_stage("stages/player.usda"));
        project.save().unwrap();
        let reloaded = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert_eq!(
            reloaded.manifest().player_stage,
            Some(PathBuf::from("stages/player.usda"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_project_omits_default_loose_levels() {
        // create() writes via the serde writer (raw_toml = None). A default
        // (false) loose_levels must NOT be emitted — matching the
        // comment-preserving patch writer, which removes the key — so default
        // manifests stay clean and the two save paths produce the same text.
        let dir = scratch("loose-levels-create-off");
        IdtxProject::create(&dir, ProjectManifest::new("LooseCreateOff")).unwrap();
        let toml = std::fs::read_to_string(dir.join(PROJECT_MANIFEST_FILENAME)).unwrap();
        assert!(
            !toml.contains("loose_levels"),
            "default loose_levels must be omitted by the serde writer, got:\n{toml}"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // When enabled, a freshly-created manifest DOES serialize it.
        let dir2 = scratch("loose-levels-create-on");
        let mut manifest = ProjectManifest::new("LooseCreateOn");
        manifest.build.loose_levels = true;
        IdtxProject::create(&dir2, manifest).unwrap();
        let toml2 = std::fs::read_to_string(dir2.join(PROJECT_MANIFEST_FILENAME)).unwrap();
        assert!(
            toml2.contains("loose_levels = true"),
            "enabled loose_levels must be serialized, got:\n{toml2}"
        );
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn loose_levels_survives_comment_preserving_save() {
        let dir = scratch("loose-levels-save");
        IdtxProject::create(&dir, ProjectManifest::new("LooseSave")).unwrap();
        // Reload so save() goes through the toml_edit comment-preserving patch
        // path (raw_toml is Some after load) — the fragile hand-written writer.
        let mut project = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert!(!project.manifest().build.loose_levels, "default is false");
        project.manifest_mut().build.loose_levels = true;
        project.save().unwrap();
        let reloaded = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert!(
            reloaded.manifest().build.loose_levels,
            "loose_levels must persist through the comment-preserving save"
        );
        // And toggling back off removes it (default stays clean).
        let mut project = reloaded;
        project.manifest_mut().build.loose_levels = false;
        project.save().unwrap();
        let reloaded = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert!(!reloaded.manifest().build.loose_levels, "off round-trips");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_stage_resolves_project_or_builtin_default() {
        let dir = scratch("player-stage-abs");
        // Unset → built-in default.
        let project = IdtxProject::create(&dir, ProjectManifest::new("Default")).unwrap();
        assert_eq!(project.player_stage(), PlayerStage::BuiltinDefault);
        assert!(project.player_stage_abs().is_none());

        // A set, in-sandbox path → Project(abs).
        let mut m = ProjectManifest::new("Custom");
        m.player_stage = Some(PathBuf::from("stages/player.usda"));
        let dir2 = scratch("player-stage-custom");
        let project2 = IdtxProject::create(&dir2, m).unwrap();
        let expected = project2
            .project_root()
            .resolve_relative(Path::new("stages/player.usda"));
        assert_eq!(project2.player_stage(), PlayerStage::Project(expected.clone().unwrap()));
        assert_eq!(project2.player_stage_abs(), expected);

        // An escaping path → built-in default (sandbox guard).
        let mut m3 = ProjectManifest::new("Escape");
        m3.player_stage = Some(PathBuf::from("../../../etc/player.usda"));
        let dir3 = scratch("player-stage-escape");
        let project3 = IdtxProject::create(&dir3, m3).unwrap();
        assert_eq!(project3.player_stage(), PlayerStage::BuiltinDefault);

        // An empty path → built-in default.
        let mut m4 = ProjectManifest::new("Empty");
        m4.player_stage = Some(PathBuf::new());
        let dir4 = scratch("player-stage-emptypath");
        let project4 = IdtxProject::create(&dir4, m4).unwrap();
        assert_eq!(project4.player_stage(), PlayerStage::BuiltinDefault);

        for d in [&dir, &dir2, &dir3, &dir4] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn set_player_stage_rejects_sandbox_escape() {
        let dir = scratch("player-stage-set");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Set")).unwrap();
        assert!(project.set_player_stage("stages/player.usda"));
        assert_eq!(
            project.manifest().player_stage,
            Some(PathBuf::from("stages/player.usda"))
        );
        assert!(!project.set_player_stage("../../elsewhere.usda"));
        // Unchanged after a rejected set.
        assert_eq!(
            project.manifest().player_stage,
            Some(PathBuf::from("stages/player.usda"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn network_config_round_trips_via_serde() {
        let mut m = ProjectManifest::new("Net");
        m.network.allow = true;
        m.network.allowed_hosts = vec!["api.example.com".to_owned(), "*.data.org".to_owned()];
        m.network.max_response_mb = 4;
        let text = m.to_toml_string().unwrap();
        assert!(text.contains("[network]"), "toml = {text}");
        assert!(text.contains("allow = true"), "toml = {text}");
        assert!(text.contains("api.example.com"), "toml = {text}");
        assert!(text.contains("max_response_mb = 4"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn network_max_response_mb_written_and_pruned() {
        // A lone body cap (network otherwise off) is still persisted...
        let mut m = ProjectManifest::new("CapOnly");
        m.network.max_response_mb = 2;
        assert!(!m.network.is_empty());
        let text = m.to_toml_string().unwrap();
        assert!(text.contains("max_response_mb = 2"), "toml = {text}");
        // ...and a zero value is omitted entirely.
        let mut m2 = ProjectManifest::new("NoCap");
        m2.network.allow = true;
        m2.network.allowed_hosts = vec!["api.example.com".to_owned()];
        let text2 = m2.to_toml_string().unwrap();
        assert!(!text2.contains("max_response_mb"), "toml = {text2}");
    }

    #[test]
    fn network_config_absent_by_default() {
        let m = ProjectManifest::new("NoNet");
        let text = m.to_toml_string().unwrap();
        assert!(!text.contains("[network]"), "toml = {text}");
        let back: ProjectManifest = toml::from_str(&text).unwrap();
        assert_eq!(back.network, NetworkProjectConfig::default());
    }

    #[test]
    fn network_config_survives_comment_preserving_save() {
        let dir = scratch("network-save");
        IdtxProject::create(&dir, ProjectManifest::new("NetSave")).unwrap();
        let mut project = IdtxProject::open_dir(&dir).unwrap().expect("project");
        project.manifest_mut().network.allow = true;
        project
            .manifest_mut()
            .network
            .allowed_hosts
            .push("api.example.com".to_owned());
        project.manifest_mut().network.auth_origins = vec!["https://api.example.com:8443".into()];
        project.manifest_mut().network.private_hosts = vec!["api.example.com".into()];
        project.manifest_mut().network.max_asset_mb = 64;
        project.save().unwrap();
        let reloaded = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert!(reloaded.manifest().network.allow);
        assert_eq!(reloaded.manifest().network.auth_origins, ["https://api.example.com:8443"]);
        assert_eq!(reloaded.manifest().network.private_hosts, ["api.example.com"]);
        assert_eq!(reloaded.manifest().network.max_asset_mb, 64);
        assert_eq!(
            reloaded.manifest().network.allowed_hosts,
            vec!["api.example.com"]
        );
        // Toggle back off — should prune [network] entirely.
        let mut project2 = reloaded;
        project2.manifest_mut().network = NetworkProjectConfig::default();
        project2.save().unwrap();
        let reloaded2 = IdtxProject::open_dir(&dir).unwrap().expect("project");
        assert!(reloaded2.manifest().network.is_empty());
        let text = std::fs::read_to_string(reloaded2.manifest_path()).unwrap();
        assert!(!text.contains("[network]"), "should be pruned: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unset_player_stage_is_omitted_from_saved_toml() {
        let dir = scratch("player-stage-omit");
        let project = IdtxProject::create(&dir, ProjectManifest::new("NoPlayer")).unwrap();
        project.save().unwrap();
        let text = std::fs::read_to_string(project.manifest_path()).unwrap();
        assert!(
            !text.contains("player_stage"),
            "an unset player_stage must not be written"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_level_appends_and_dedups() {
        let dir = scratch("addlevel");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Levels")).unwrap();
        assert!(project.levels().is_empty());

        assert!(project.add_level("levels/hub.usda"));
        assert!(project.add_level("levels/arena.usda"));
        // Same level via a `./`-prefixed form is a duplicate, not a new entry.
        assert!(!project.add_level("./levels/hub.usda"));
        assert!(!project.add_level("levels/arena.usda"));

        assert_eq!(
            project.levels(),
            &[
                PathBuf::from("levels/hub.usda"),
                PathBuf::from("levels/arena.usda"),
            ]
        );
        assert!(project.is_level_registered("levels/hub.usda"));
        assert!(project.is_level_registered("./levels/hub.usda"));
        assert!(!project.is_level_registered("levels/missing.usda"));
    }

    #[test]
    fn add_level_rejects_sandbox_escapes() {
        let dir = scratch("escapelevel");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Levels")).unwrap();
        assert!(!project.add_level("../elsewhere/secret.usda"));
        assert!(!project.add_level("/etc/passwd"));
        assert!(project.levels().is_empty());
    }

    #[test]
    fn remove_level_removes_matching_entry() {
        let dir = scratch("rmlevel");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Levels")).unwrap();
        project.add_level("levels/a.usda");
        project.add_level("levels/b.usda");

        assert!(project.remove_level("./levels/a.usda"));
        assert!(!project.remove_level("levels/a.usda")); // already gone
        assert_eq!(project.levels(), &[PathBuf::from("levels/b.usda")]);
    }

    #[test]
    fn move_level_reorders() {
        let dir = scratch("movelevel");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Levels")).unwrap();
        for name in ["a", "b", "c"] {
            project.add_level(format!("levels/{name}.usda"));
        }

        // Move the last entry to the front.
        assert!(project.move_level(2, 0));
        assert_eq!(
            project.levels(),
            &[
                PathBuf::from("levels/c.usda"),
                PathBuf::from("levels/a.usda"),
                PathBuf::from("levels/b.usda"),
            ]
        );
        // Out-of-range indices are a no-op false; a same-index move is a no-op true.
        assert!(!project.move_level(0, 9));
        assert!(project.move_level(1, 1));
    }

    #[test]
    fn levels_registry_survives_toml_round_trip() {
        let dir = scratch("levelpersist");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Levels")).unwrap();
        project.add_level("levels/hub.usda");
        project.add_level("levels/arena.usda");
        project.save().unwrap();

        let reloaded = IdtxProject::load(project.manifest_path()).unwrap();
        assert_eq!(reloaded.levels(), project.levels());
    }

    #[test]
    fn add_level_relativizes_absolute_inside_root() {
        let dir = scratch("abslevel");
        let mut project = IdtxProject::create(&dir, ProjectManifest::new("Levels")).unwrap();
        // Create a real level file inside the root so canonicalize succeeds.
        let levels_dir = project.root().join("levels");
        std::fs::create_dir_all(&levels_dir).unwrap();
        let abs = levels_dir.join("hub.usda");
        std::fs::write(&abs, "#usda 1.0\n").unwrap();

        assert!(project.add_level(&abs));
        assert_eq!(project.levels(), &[PathBuf::from("levels/hub.usda")]);
        // The absolute form is recognized as already registered.
        assert!(project.is_level_registered(&abs));

        let abs_levels = project.levels_abs();
        assert_eq!(abs_levels.len(), 1);
        assert!(abs_levels[0].ends_with("hub.usda"));
    }

    #[test]
    fn asset_search_roots_prepends_implicit_root_and_filters_escapes() {
        let dir = scratch("searchroots");
        let mut manifest = ProjectManifest::new("Assets");
        manifest.asset_roots = vec![
            PathBuf::from("assets"),
            PathBuf::from("../outside"), // escapes the sandbox → dropped
            PathBuf::from("assets"),     // duplicate → dropped
        ];
        let project = IdtxProject::create(&dir, manifest).unwrap();
        assert_eq!(
            project.asset_search_roots(),
            vec![PathBuf::new(), PathBuf::from("assets")]
        );
    }

    #[test]
    fn asset_search_dirs_abs_resolves_roots_under_the_project_root() {
        let dir = scratch("searchdirs");
        let mut manifest = ProjectManifest::new("Assets");
        manifest.asset_roots = vec![PathBuf::from("assets"), PathBuf::from("../outside")];
        let project = IdtxProject::create(&dir, manifest).unwrap();
        let dirs = project.asset_search_dirs_abs();
        // Implicit root + the in-sandbox "assets" root; the escape is dropped.
        assert_eq!(dirs.len(), 2);
        assert!(dirs.iter().all(|d| d.is_absolute()));
        assert!(dirs[1].ends_with("assets"));
    }

    #[test]
    fn list_assets_lists_project_root_children() {
        let dir = scratch("listassets");
        let project = IdtxProject::create(&dir, ProjectManifest::new("Assets")).unwrap();
        std::fs::create_dir_all(project.root().join("stages")).unwrap();
        std::fs::write(project.root().join("readme.txt"), "hi").unwrap();
        let entries = project.list_assets("");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // Directory first, then the file; the manifest sidecar is skipped.
        assert_eq!(names, ["stages", "readme.txt"]);
        assert_eq!(entries[1].kind, AssetKind::Data);
    }
}
