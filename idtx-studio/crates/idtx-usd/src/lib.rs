//! Safe, revisioned ownership of one live official `OpenUSD` stage.

#![forbid(unsafe_code)]


mod transform;

pub use transform::{usd_trs_matrix, usd_world_to_local, validate_usd_affine};

#[cfg(test)]
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use idtx_usd_sys::bridge::ffi;
use thiserror::Error;

mod stageservice;
pub use stageservice::StageService;
mod identity;
pub use identity::*;
mod packaging;
pub use packaging::*;
mod material;
pub use material::*;
mod projection;
pub use projection::*;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static PLUGIN_ROOT: OnceLock<PathBuf> = OnceLock::new();
static PLUGIN_REGISTRATION: OnceLock<Result<(), String>> = OnceLock::new();
/// Schema version stamped on every [`ViewportProjection::schema_version`]. Bump
/// it whenever the projected-record layout changes so a consumer can reject a
/// projection it does not understand.
pub const PROJECTION_SCHEMA_VERSION: u32 = 3;

/// Every fallible operation in this crate returns this error. Variants wrap an
/// invalid transform/projection, a non-UTF-8 path, a bridge/`OpenUSD` failure
/// (including converted `cxx::Exception`s), unresolved assets, or an exhausted
/// revision counter.
#[derive(Debug, Error)]
pub enum StageError {
    #[error("invalid transform: {0}")]
    InvalidTransform(String),
    #[error("stage path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("OpenUSD operation failed: {0}")]
    OpenUsd(String),
    #[error("projection referenced an unknown path: {0}")]
    UnknownPath(String),
    #[error("invalid projection record for {path}: {detail}")]
    InvalidProjection { path: String, detail: String },
    #[error("OpenUSD returned an unexpected null stage handle")]
    NullHandle,
    #[error("the stage revision counter overflowed")]
    RevisionOverflow,
    #[error("{} referenced asset(s) could not be resolved: {}", .0.len(), .0.join(", "))]
    UnresolvedAssets(Vec<String>),
    #[error("loose staging failed: {0}")]
    LooseStage(String),
}

impl From<cxx::Exception> for StageError {
    fn from(value: cxx::Exception) -> Self {
        Self::OpenUsd(value.what().to_owned())
    }
}


/// Root-layer shadow-rendering mode, resolved from the stage's `customLayerData`
/// and carried on [`StageMetadata`]. Ordered `Off < Hard < Smooth < Soft` by
/// ascending cost/quality; the studio session maps this 1:1 to
/// `idtx_render::ShadowTier` (`Off`/`Hard`/`Smooth`/`Soft`) and clamps it
/// against the target-hardware cap. Shadows are opt-in: [`ShadowMode::Off`] is
/// the default when no key is authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShadowMode {
    /// No shadows (default / opt-out).
    #[default]
    Off,
    /// Hard shadows — a single comparison tap: a crisp ~1-texel anti-aliased
    /// edge with no filter blur or penumbra jitter. The cheapest lit tier.
    Hard,
    /// Smooth shadows — a fixed 3×3 PCF kernel (9 taps): a slightly softened
    /// contact edge. Formerly what `hard` resolved to.
    Smooth,
    /// Soft shadows — maps to the high-quality PCSS tier (variable penumbra).
    Soft,
}

impl ShadowMode {
    /// Decode the FFI carrier (`0`/`1`/`2`/`3`); any other value → [`ShadowMode::Off`].
    #[must_use]
    pub fn from_i32(raw: i32) -> Self {
        match raw {
            1 => ShadowMode::Hard,
            2 => ShadowMode::Smooth,
            3 => ShadowMode::Soft,
            _ => ShadowMode::Off,
        }
    }

    /// Encode as the FFI carrier / authoring token index (`0`/`1`/`2`/`3`).
    #[must_use]
    pub fn as_i32(self) -> i32 {
        match self {
            ShadowMode::Off => 0,
            ShadowMode::Hard => 1,
            ShadowMode::Smooth => 2,
            ShadowMode::Soft => 3,
        }
    }
}

/// Root-layer stage metadata resolved on open: identifier, default prim, up
/// axis, linear unit scale, the time-code range and rate, and the IDTX
/// `customLayerData` opt-ins (shadow mode, stage-behavior name and run scope).
#[derive(Debug, Clone, PartialEq)]
pub struct StageMetadata {
    pub root_identifier: String,
    pub default_prim: Option<String>,
    pub up_axis: String,
    pub meters_per_unit: f64,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub time_codes_per_second: f64,
    /// `true` when `startTimeCode` is explicitly authored in the root layer.
    pub has_time_code_range: bool,
    /// Root-layer shadow-mode opt-in, resolved from
    /// `customLayerData["idtx:shadowMode"]` (token `off`/`hard`/`smooth`/`soft`)
    /// with a
    /// fallback to the legacy `idtx:softShadows` bool (`true`→soft, `false`→hard).
    /// [`ShadowMode::Off`] when neither key is authored — shadows stay opt-in.
    pub shadow_mode: ShadowMode,
    /// The name of the stage-level behavior prim, read from the root layer's
    /// `customLayerData["idtx:stageBehavior"]`, or `None` when no stage behavior
    /// is assigned. By convention this is `"StageBehavior"` (prim `/StageBehavior`).
    pub stage_behavior_name: Option<String>,
    /// The run-condition for the stage behavior, read from
    /// `customLayerData["idtx:stageBehaviorRunScope"]`. `None` when unauthored
    /// (treat as [`StageBehaviorRunScope::Standalone`]).
    pub stage_behavior_run_scope: Option<StageBehaviorRunScope>,
}

/// When a stage-level behavior ([`StageMetadata::stage_behavior_name`]) is
/// permitted to tick, keyed on how its authoring layer is composed into the
/// running stage ([`StageComposeContext`]). Authored as the root-layer
/// `customLayerData["idtx:stageBehaviorRunScope"]` token. A layer composed as a
/// **reference** never runs its stage behavior regardless of this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StageBehaviorRunScope {
    /// Run only when the authoring layer is opened directly as the stage root.
    #[default]
    Standalone,
    /// Run only when the authoring layer is pulled in via a payload arc.
    Payload,
    /// Run only when the authoring layer is used as a sublayer.
    Sublayer,
    /// Run in every composition context except reference (standalone ∪ payload ∪
    /// sublayer).
    Always,
}

impl StageBehaviorRunScope {
    /// Parse the authored token; unknown tokens map to
    /// [`StageBehaviorRunScope::Standalone`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "payload" => Self::Payload,
            "sublayer" => Self::Sublayer,
            "always" => Self::Always,
            _ => Self::Standalone,
        }
    }

    /// The token this scope round-trips to.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Payload => "payload",
            Self::Sublayer => "sublayer",
            Self::Always => "always",
        }
    }

    /// Whether a stage behavior with this run-scope may tick in the given
    /// composition `context`. [`StageComposeContext::Reference`] (and
    /// [`StageComposeContext::Unknown`]) never run; [`Self::Always`] runs in the
    /// three non-reference contexts.
    #[must_use]
    pub fn permits(self, context: StageComposeContext) -> bool {
        match context {
            StageComposeContext::Standalone => {
                matches!(self, Self::Standalone | Self::Always)
            }
            StageComposeContext::Payload => matches!(self, Self::Payload | Self::Always),
            StageComposeContext::Sublayer => matches!(self, Self::Sublayer | Self::Always),
            StageComposeContext::Reference | StageComposeContext::Unknown => false,
        }
    }
}

/// How the layer carrying a stage behavior is composed into the open stage,
/// classified at runtime by the bridge for the [`StageBehaviorRunScope`] gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StageComposeContext {
    /// The carrying layer is the stage's own root layer (opened directly).
    Standalone,
    /// The carrying layer entered via a payload arc.
    Payload,
    /// The carrying layer is a sublayer of the root local layer stack.
    Sublayer,
    /// The carrying layer entered via a reference arc — never runs.
    Reference,
    /// No stage behavior found, or the context could not be determined.
    #[default]
    Unknown,
}

impl StageComposeContext {
    /// Decode the FFI carrier (`0`=standalone, `1`=payload, `2`=sublayer,
    /// `3`=reference, anything else → [`StageComposeContext::Unknown`]).
    #[must_use]
    pub fn from_i32(raw: i32) -> Self {
        match raw {
            0 => Self::Standalone,
            1 => Self::Payload,
            2 => Self::Sublayer,
            3 => Self::Reference,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod stage_behavior_run_scope_tests {
    use super::{StageBehaviorRunScope as S, StageComposeContext as C};

    #[test]
    fn token_round_trip() {
        for s in [S::Standalone, S::Payload, S::Sublayer, S::Always] {
            assert_eq!(S::from_token(s.as_token()), s);
        }
        assert_eq!(S::from_token("bogus"), S::Standalone);
        assert_eq!(S::default(), S::Standalone);
    }

    #[test]
    fn compose_context_from_i32() {
        assert_eq!(C::from_i32(0), C::Standalone);
        assert_eq!(C::from_i32(1), C::Payload);
        assert_eq!(C::from_i32(2), C::Sublayer);
        assert_eq!(C::from_i32(3), C::Reference);
        assert_eq!(C::from_i32(99), C::Unknown);
        assert_eq!(C::from_i32(-1), C::Unknown);
    }

    #[test]
    fn reference_context_never_runs() {
        for s in [S::Standalone, S::Payload, S::Sublayer, S::Always] {
            assert!(!s.permits(C::Reference), "{s:?} must not run when referenced");
            assert!(!s.permits(C::Unknown), "{s:?} must not run when unknown");
        }
    }

    #[test]
    fn each_scope_permits_only_its_context_plus_always() {
        assert!(S::Standalone.permits(C::Standalone));
        assert!(!S::Standalone.permits(C::Payload));
        assert!(!S::Standalone.permits(C::Sublayer));

        assert!(S::Payload.permits(C::Payload));
        assert!(!S::Payload.permits(C::Standalone));
        assert!(!S::Payload.permits(C::Sublayer));

        assert!(S::Sublayer.permits(C::Sublayer));
        assert!(!S::Sublayer.permits(C::Standalone));
        assert!(!S::Sublayer.permits(C::Payload));

        assert!(S::Always.permits(C::Standalone));
        assert!(S::Always.permits(C::Payload));
        assert!(S::Always.permits(C::Sublayer));
    }
}

/// The IDTX stage-kind tag read from a stage's root-layer `customLayerData.idtx`
/// (see roadmap Appendix A.1). All fields are empty for an untagged stage.
///
/// The `stage_kind` string is the raw token (`"level"`, `"asset"`, `"assembly"`,
/// `"player"`); higher layers (`idtx-project`) map it to their own taxonomy so
/// this crate stays free of project-domain semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageKindTag {
    /// Raw `stageKind` token, or empty when the stage carries no idtx tag.
    pub stage_kind: String,
    /// Authored `displayName` (level/asset), or empty.
    pub display_name: String,
    /// Authored idtx `schemaVersion`, or empty when untagged.
    pub schema_version: String,
}

impl StageKindTag {
    /// `true` when a `stageKind` tag is present.
    #[must_use]
    pub fn is_tagged(&self) -> bool {
        !self.stage_kind.is_empty()
    }
}

/// Root-layer composition targets read from a stage file without composing it
/// (see [`read_stage_root_prims`]). Drives the scene-tree drop flow's choice of
/// which prim a reference/payload arc should point at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageRootPrims {
    /// The root layer's authored `defaultPrim` name, or `None` when none is
    /// authored. A reference/payload with no explicit target composes this prim.
    pub default_prim: Option<String>,
    /// Names of the root-level `def` prims — the composable arc targets.
    /// `over`/`class` roots are excluded.
    pub root_def_prims: Vec<String>,
}

/// USD attribute name of the player-ownership role token authored on a pawn
/// root (`token idtx:playerRole`, plan §2.1). Value is one of
/// `local`/`remote`/`server`/`unowned`; higher layers map it to their own
/// `NetRole` taxonomy. Authored via `StageService::set_player_role` and read
/// via `StageService::read_player_role`.
pub const PLAYER_ROLE_ATTR: &str = "idtx:playerRole";

/// USD attribute name of the per-instance player id (`string idtx:playerId`),
/// replaced at join by the netcode. Authored via
/// `StageService::set_player_id`, read via `StageService::read_player_id`.
pub const PLAYER_ID_ATTR: &str = "idtx:playerId";

/// Recover a token/string value from an [`AttributeValue::value_repr`], trimming
/// the surrounding quotes USD's `operator<<` prints for a `TfToken`/`string`.
/// Returns `None` for an empty value (mirrors the `idtx-scene` hydrator's
/// `read_token_attribute` so authored roles read back identically on both paths).
fn trim_token_repr(value_repr: &str) -> Option<String> {
    let token = value_repr.trim().trim_matches('"').trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// Read the IDTX stage-kind tag from the **root layer alone** of the file at
/// `path`, without composing a stage or loading payloads. Files with no
/// `customLayerData.idtx` tag (or that cannot be opened as a layer) return a
/// default (untagged) [`StageKindTag`] — the project browser treats those as
/// unclassified rather than erroring.
///
/// # Errors
/// [`StageError`] when `path` is not valid UTF-8 or the bridge call fails.
pub fn read_stage_kind(path: impl AsRef<Path>) -> Result<StageKindTag, StageError> {
    let path = path.as_ref();
    let text = path
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(path.to_path_buf()))?;
    register_plugins_once()?;
    let raw = ffi::read_stage_kind(text)?;
    Ok(StageKindTag {
        stage_kind: raw.stage_kind,
        display_name: raw.display_name,
        schema_version: raw.schema_version,
    })
}

/// Read the root-layer composition targets (`defaultPrim` + root `def` prim
/// names) from the file at `path`, without composing a stage or loading
/// payloads. The scene-tree drop flow uses this to decide what prim a dropped
/// reference/payload arc should point at: an asset with no `defaultPrim`
/// composes nothing unless the arc names an explicit target prim. A file that
/// cannot be opened as a layer returns an empty [`StageRootPrims`].
///
/// # Errors
/// [`StageError`] when `path` is not valid UTF-8 or the bridge call fails.
pub fn read_stage_root_prims(path: impl AsRef<Path>) -> Result<StageRootPrims, StageError> {
    let path = path.as_ref();
    let text = path
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(path.to_path_buf()))?;
    register_plugins_once()?;
    let raw = ffi::read_stage_root_prims(text)?;
    Ok(StageRootPrims {
        default_prim: (!raw.default_prim.is_empty()).then_some(raw.default_prim),
        root_def_prims: raw.root_def_prims,
    })
}

/// Override the process-wide default asset-resolver search path with `dirs`
/// (absolute directories), so bare or project-relative asset references
/// (e.g. `@prop.usda@`) resolve against a project's asset roots. This is the
/// `ArDefaultResolver` fallback; layer-relative references still resolve
/// natively against each layer's own directory.
///
/// The directories are joined with newlines and passed to the shim, which
/// installs them via `ArDefaultResolver::SetDefaultSearchPath` under the
/// exclusive stage-lifecycle lock. It is static and process-global — the last
/// call wins — so anchor it once per active project. Non-UTF-8 directories are
/// silently skipped.
///
/// # Errors
/// [`StageError`] when plugin registration or the bridge call fails.
pub fn set_default_asset_search_path(dirs: &[PathBuf]) -> Result<(), StageError> {
    let joined = dirs
        .iter()
        .filter_map(|d| d.to_str())
        .collect::<Vec<_>>()
        .join("\n");
    register_plugins_once()?;
    ffi::set_default_asset_search_path(&joined)?;
    Ok(())
}

// ── Animation metadata ────────────────────────────────────────────────────────

/// Stage-level animation range and rate, derived from `StageMetadata`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageAnimation {
    pub has_time_code_range: bool,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub time_codes_per_second: f64,
}

impl StageAnimation {
    #[must_use]
    pub fn duration_time_codes(&self) -> Option<f64> {
        if self.has_time_code_range && self.end_time_code > self.start_time_code {
            Some(self.end_time_code - self.start_time_code)
        } else {
            None
        }
    }
    #[must_use]
    pub fn duration_seconds(&self) -> Option<f64> {
        let tps = if self.time_codes_per_second > 0.0 {
            self.time_codes_per_second
        } else {
            24.0
        };
        self.duration_time_codes().map(|d| d / tps)
    }
}

/// Interpolation hint for a time-sampled attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpolation {
    Linear,
    Held,
}

impl Interpolation {
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Held => "held",
        }
    }
}

/// Time-sample metadata for a single animated attribute.
#[derive(Debug, Clone)]
pub struct TimeSamples {
    pub times: Vec<f64>,
    pub count: usize,
    pub interpolation: Option<Interpolation>,
}

impl TimeSamples {
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.count > 0
    }

    #[must_use]
    pub fn time_range(&self) -> Option<(f64, f64)> {
        self.times.first().copied().zip(self.times.last().copied())
    }
}




/// Classifies a [`PropertyRecord`] as a USD attribute, relationship, or
/// metadata field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyKind {
    Attribute,
    Relationship,
    Metadata,
}

/// One inspected property on a prim: name, kind, type, resolved value summary,
/// whether it is authored, its value source, time-sample count, the
/// strongest-first provenance of contributing layers (with parallel
/// `provenance_values`), relationship targets, and whether the root layer
/// authors it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyRecord {
    pub name: String,
    pub kind: PropertyKind,
    pub type_name: String,
    pub value_summary: String,
    pub authored: bool,
    pub value_source: String,
    pub time_sample_count: u64,
    pub provenance: Vec<String>,
    /// Per-opinion authored value summary, parallel to [`provenance`] (same
    /// order, strongest-first). Empty for opinions with no authored default
    /// (relationships, or the empty string when a layer only time-samples).
    ///
    /// [`provenance`]: PropertyRecord::provenance
    pub provenance_values: Vec<String>,
    pub relationship_targets: Vec<String>,
    /// `true` when this property has an authored spec in the stage's **root
    /// layer** (the edit target). The inspector shows an active "remove
    /// root-layer opinion" button only for such properties.
    pub root_authored: bool,
}


/// Presentational classification of a prim in an `OpenExec` compute graph,
/// derived from its USD type name on the C++ side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeNodeRole {
    /// A `*Datasource` prim — a graph root (its `outputs:data` is fed externally).
    Datasource,
    /// A `Compute_*` prim — an intermediate computation node.
    Compute,
    /// The selected terminal consumer prim whose connected attribute roots the walk.
    Consumer,
    /// Anything else reached through a connection.
    Other,
}

/// One socket (attribute) on a [`ComputeNode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputeSocket {
    /// Full attribute name including any `inputs:` / `outputs:` namespace.
    pub name: String,
    /// USD value type name (e.g. `double`, `string`, `token`).
    pub type_name: String,
    /// Formatted authored value summary (empty when unauthored / connected).
    pub value_summary: String,
    /// Authored connection targets (full property paths), empty for outputs.
    pub connections: Vec<String>,
}

/// One prim in a compute graph, with its input / output sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputeNode {
    pub path: String,
    pub type_name: String,
    pub role: ComputeNodeRole,
    pub inputs: Vec<ComputeSocket>,
    pub outputs: Vec<ComputeSocket>,
}

/// The backward-walked `OpenExec` compute graph rooted at a selected prim.
///
/// `root_path` is `None` when the selected prim has no connected attribute
/// (no compute graph to show).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputeGraph {
    pub root_path: Option<String>,
    pub nodes: Vec<ComputeNode>,
    pub diagnostics: Vec<String>,
}

/// One inspected variant set on a prim: its name, available choices, the
/// resolved selection, and the strongest-first provenance of the layers that
/// author the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantSetRecord {
    pub name: String,
    pub choices: Vec<String>,
    pub selection: String,
    pub provenance: Vec<String>,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XformOpType {
    Translate,
    RotateXyz,
    Scale,
    Unsupported,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XformPrecision {
    Double,
    Float,
    Half,
}


#[derive(Debug, Clone, PartialEq)]
pub struct AuthoredXformOp {
    pub name: String,
    pub op_type: XformOpType,
    pub op_type_name: String,
    pub precision: XformPrecision,
    pub inverse: bool,
    pub time_sample_count: u64,
    pub has_default: bool,
    pub value: [f64; 3],
}


#[derive(Debug, Clone, PartialEq)]
pub struct AuthoredXformStack {
    pub prim: PrimId,
    pub reset_stack: bool,
    pub editable: bool,
    pub diagnostic: Option<String>,
    pub ops: Vec<AuthoredXformOp>,
}


#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformValues {
    pub translation: [f64; 3],
    pub rotation_xyz_degrees: [f64; 3],
    pub scale: [f64; 3],
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantLayerState {
    pub root_selection: Option<String>,
    pub session_selection: Option<String>,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantManifestEdit {
    pub prim: PrimId,
    pub set_name: String,
    pub selection: String,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantLayerEdit {
    pub prim: PrimId,
    pub set_name: String,
    pub state: VariantLayerState,
}

/// One semantic structural mutation authored explicitly into the stage root
/// layer. Existing prims are addressed by generation-scoped IDs; new paths and
/// resolver asset paths remain strings until `OpenUSD` validates them.

#[derive(Debug, Clone, PartialEq)]
pub enum StructuralCommand {
    Define {
        path: String,
        type_name: String,
    },
    /// Author an empty `over` for an existing composed prim into the root layer.
    ///
    /// Used to give a display-only (referenced/payload) prim a root-layer
    /// opinion so it becomes editable. Addressed by `path`, like [`Self::Define`].
    CreateOverride {
        path: String,
    },
    /// Remove an empty root-layer `over` previously authored by
    /// [`Self::CreateOverride`]. Refuses to remove a `def` or an over that
    /// carries authored data.
    RemoveOverride {
        path: String,
    },
    Delete {
        prim: PrimId,
    },
    /// Author the `active` metadata of an existing root-layer prim, addressed by
    /// `path` (like [`Self::Define`]). `active = false` is a *soft delete*: USD
    /// prunes the prim and its subtree from the composed stage; `active = true`
    /// restores it. Unlike [`Self::Delete`] this is addressed by path, not
    /// `PrimId`: a currently-inactive prim is absent from the topology projection
    /// (and so has no live `PrimId`), yet must still be re-activatable. The target
    /// must have a `def` spec owned by the root edit target.
    SetActive {
        path: String,
        active: bool,
    },
    Rename {
        prim: PrimId,
        new_name: String,
    },
    Reparent {
        prim: PrimId,
        new_parent: PrimId,
    },
    AddReference {
        prim: PrimId,
        asset_path: String,
        target_path: Option<String>,
    },
    AddPayload {
        prim: PrimId,
        asset_path: String,
        target_path: Option<String>,
    },
    AttachBehavior {
        prim: PrimId,
        asset_path: String,
    },
    SetProperty {
        prim: PrimId,
        property: String,
        value: PropertyValue,
    },
    BindMaterial {
        prim: PrimId,
        material: PrimId,
    },
    AddSublayer {
        asset_path: String,
    },
    RemoveSublayer {
        asset_path: String,
    },
    /// Apply a single-apply API schema (e.g. `ShadowAPI`) to an existing prim,
    /// authored into the root layer. Addressed by `path` (like
    /// [`Self::CreateOverride`]); `schema` is the short USD alias reported by
    /// `apiSchemas` (`"ShadowAPI"`).
    ApplyApi {
        path: String,
        schema: String,
    },
    /// Remove a single-apply API schema previously applied by [`Self::ApplyApi`].
    RemoveApi {
        path: String,
        schema: String,
    },
}


/// A typed value handed to the authoring shim for `SetProperty`. Each variant
/// maps to a specific `set-property-*` bridge path and USD value type; scalar,
/// colour vs. vector3f, token vs. token-array, and the int/array forms are kept
/// distinct so the authored attribute carries the correct USD role.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    F32(f32),
    F64(f64),
    String(String),
    Token(String),
    /// A `token[]` value (e.g. the `IdtxCollisionAPI` channel list). Authored by
    /// the `set-property-token-array` shim path.
    TokenArray(Vec<String>),
    Color3f([f32; 3]),
    /// A `vector3f` value (e.g. `UsdPhysicsScene`'s `physics:gravityDirection`).
    /// Authored by the `set-property-vector3f` shim path, string-encoded as
    /// space-separated `x y z`. Distinct from [`Self::Color3f`] so the authored
    /// attribute carries the `vector3f` role rather than a colour role.
    Vector3f([f32; 3]),
    Asset(String),
    /// A scalar `int` value (e.g. `idtx:behavior:limits:memoryMb`). Authored by
    /// the `set-property-int` shim path, string-encoded as a decimal integer.
    Int(i32),
    /// An `int2` value (e.g. the `IdtxHud` on-screen pixel size). Authored by
    /// the `set-property-int2` shim path (root) or `author_session_property`
    /// (session), string-encoded as `"x y"`.
    Int2([i32; 2]),
    /// A `point3f[]` value (e.g. `idtx:collision:hull:points`). Authored by the
    /// `set-property-point3-array` shim path, string-encoded as space-separated
    /// `x y z` triples.
    Point3Array(Vec<[f32; 3]>),
    /// An `int[]` value (e.g. `idtx:collision:hull:triangles`). Authored by the
    /// `set-property-int-array` shim path, string-encoded as space-separated
    /// integers.
    IntArray(Vec<i32>),
}

/// An interactive layer-stack mutation (U1.3): set the edit target, mute /
/// unmute a layer, or reorder a sublayer. Applied via
/// `StageService::apply_layer_edit`; the caller reprojects afterwards.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerEdit {
    /// Set the stage's edit target to the local layer with this identifier.
    SetEditTarget { identifier: String },
    /// Mute the layer with this identifier (drops it from composition).
    Mute { identifier: String },
    /// Unmute a previously-muted layer.
    Unmute { identifier: String },
    /// Move a sublayer within its parent's sublayer list. `stronger` moves it
    /// earlier (higher LIVRPS strength); otherwise later. An empty
    /// `parent_identifier` targets the root layer's sublayer list.
    MoveSublayer {
        identifier: String,
        parent_identifier: String,
        stronger: bool,
    },
}

/// A typed prim authoring template (U1.4): a well-known prim type plus the
/// schema-sensible default attributes that make a freshly-authored Camera,
/// light, or grouping prim immediately useful. Applied via
/// `StageService::define_prim_from_template`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimTemplate {
    /// A `UsdGeomCamera` with a 35 mm lens on a full-frame (36×24 mm) sensor.
    Camera,
    /// A `UsdLuxDistantLight` (sun-like directional light).
    DistantLight,
    /// A `UsdLuxRectLight` (rectangular area light).
    RectLight,
    /// A `UsdLuxDomeLight` (image-based environment light).
    DomeLight,
    /// A `UsdGeomScope` — a typeless-transform grouping prim.
    Scope,
}


impl PrimTemplate {
    /// The USD prim type this template authors.
    #[must_use]
    pub fn type_name(self) -> &'static str {
        match self {
            PrimTemplate::Camera => "Camera",
            PrimTemplate::DistantLight => "DistantLight",
            PrimTemplate::RectLight => "RectLight",
            PrimTemplate::DomeLight => "DomeLight",
            PrimTemplate::Scope => "Scope",
        }
    }

    /// A short human label for menus/toolbars.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PrimTemplate::Camera => "Camera",
            PrimTemplate::DistantLight => "Distant Light",
            PrimTemplate::RectLight => "Rect Light",
            PrimTemplate::DomeLight => "Dome Light",
            PrimTemplate::Scope => "Scope",
        }
    }

    /// The default attributes to author after the prim is defined. Attribute
    /// names are schema attributes (namespaced `inputs:*` for lights); the
    /// values are deliberately distinct from the schema fallbacks so a template
    /// prim is visibly configured rather than relying on invisible defaults.
    #[must_use]
    pub fn default_properties(self) -> Vec<(&'static str, PropertyValue)> {
        match self {
            PrimTemplate::Camera => vec![
                ("focalLength", PropertyValue::F32(35.0)),
                ("horizontalAperture", PropertyValue::F32(36.0)),
                ("verticalAperture", PropertyValue::F32(24.0)),
                ("projection", PropertyValue::Token("perspective".to_owned())),
            ],
            PrimTemplate::DistantLight => vec![
                ("inputs:intensity", PropertyValue::F32(3.0)),
                ("inputs:angle", PropertyValue::F32(0.53)),
                ("inputs:color", PropertyValue::Color3f([1.0, 0.95, 0.9])),
            ],
            PrimTemplate::RectLight => vec![
                ("inputs:intensity", PropertyValue::F32(50.0)),
                ("inputs:width", PropertyValue::F32(2.0)),
                ("inputs:height", PropertyValue::F32(1.0)),
                ("inputs:color", PropertyValue::Color3f([1.0, 1.0, 1.0])),
            ],
            PrimTemplate::DomeLight => vec![
                ("inputs:intensity", PropertyValue::F32(0.5)),
                ("inputs:color", PropertyValue::Color3f([1.0, 1.0, 1.0])),
            ],
            PrimTemplate::Scope => Vec::new(),
        }
    }
}


/// Outcome of a save/export: the written destination, its file format, any
/// package member list, the dependency set, and non-fatal warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistenceResult {
    pub destination: PathBuf,
    pub file_format: String,
    pub package_members: Vec<String>,
    pub dependencies: Vec<String>,
    pub warnings: Vec<String>,
}


/// Which persistence path was verified: an in-place `Save`, a `RootExport`, or
/// a `FlattenedExport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceVerification {
    Save,
    RootExport,
    FlattenedExport,
}

/// Bit set of the stage domains (topology, transform, geometry, material,
/// camera, …) affected by a change. Combine masks with
/// [`DirtyDomains::union`] and test membership with
/// [`DirtyDomains::contains`]; drives which projections a change invalidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyDomains(u32);

impl DirtyDomains {
    pub const TOPOLOGY: Self = Self(1 << 0);
    pub const TRANSFORM: Self = Self(1 << 1);
    pub const GEOMETRY: Self = Self(1 << 2);
    pub const MATERIAL: Self = Self(1 << 3);
    pub const CAMERA: Self = Self(1 << 4);
    pub const INSPECTION: Self = Self(1 << 5);
    pub const VISIBILITY: Self = Self(1 << 6);
    pub const LIGHT: Self = Self(1 << 7);
    pub const ANIMATION: Self = Self(1 << 8);
    pub const SKELETON: Self = Self(1 << 9);
    pub const METADATA: Self = Self(1 << 10);
    pub const RELATIONSHIP: Self = Self(1 << 11);
    pub const GENERIC: Self = Self(1 << 12);
    pub const UNKNOWN: Self = Self(1 << 31);

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Bitwise union of two domain masks. `const` so callers can fold the
    /// per-domain constants into a compile-time allow-mask, and so change
    /// classifications (e.g. an `xformOp` write authoring `TRANSFORM | INSPECTION`)
    /// can be reconstructed in tests without a raw-bits constructor.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A single prim-level entry in a [`StageChangeBatch`]: the prim, the optional
/// property name, the affected [`DirtyDomains`], whether it requires a resync,
/// and whether the opinion was authored in the root layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPrim {
    pub prim: PrimId,
    pub property_name: Option<String>,
    pub domains: DirtyDomains,
    pub resync: bool,
    /// `true` when the authored opinion for this change is in the stage's root
    /// layer.  Session-layer edits (variant selection, visibility overrides)
    /// have this `false` and must not be broadcast by the collaboration sync.
    pub authored_in_root_layer: bool,
}

/// The set of [`ChangedPrim`]s observed between `from_revision` and
/// `to_revision` of one stage generation — the unit the change observer drains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageChangeBatch {
    pub generation: StageGeneration,
    pub from_revision: StageRevision,
    pub to_revision: StageRevision,
    pub changes: Vec<ChangedPrim>,
}

/// Cumulative counters for one bridge endpoint: call count, records and bytes
/// produced, and total elapsed time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndpointMetric {
    pub calls: u64,
    pub records: u64,
    pub bytes: u64,
    pub elapsed: Duration,
}

/// Per-endpoint [`EndpointMetric`] accumulators for a stage (open, topology,
/// viewport, inspection, notice-drain, and — under the `structural-metrics`
/// feature — authoring).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageMetrics {
    pub open: EndpointMetric,
    pub topology: EndpointMetric,
    pub viewport: EndpointMetric,
    pub inspection: EndpointMetric,
    pub notice_drain: EndpointMetric,
    /// Accumulated timing for structural authoring edits (`apply_structural_command`).
    /// Only populated when the `structural-metrics` feature is enabled.
    #[cfg(feature = "structural-metrics")]
    pub authoring: EndpointMetric,
}


/// Selects a packaged `OpenUSD` plugin/resource root before the first stage is
/// opened. Editor builds use the compile-time pinned layout by default.
pub fn configure_plugin_root(root: impl AsRef<Path>) -> Result<(), StageError> {
    let root = root.as_ref().to_path_buf();
    if PLUGIN_REGISTRATION.get().is_some() {
        return Err(StageError::OpenUsd(
            "OpenUSD plugins were already registered".to_owned(),
        ));
    }
    PLUGIN_ROOT
        .set(root)
        .map_err(|_| StageError::OpenUsd("OpenUSD plugin root was already configured".to_owned()))
}

/// Process-wide count of live USD stage handles.
///
/// Incremented when a stage is opened and decremented only at the *end* of the
/// C++ teardown path — after the change observer is revoked from `TfNotice` and
/// the `UsdStage` is reset, all under the exclusive stage-lifecycle lock. A
/// return to a prior baseline after dropping every `StageService` therefore
/// proves teardown ran to completion and no handle (or its observer
/// registration) leaked. Used by the teardown leak-regression test.
#[must_use]
pub fn live_stage_handle_count() -> u64 {
    ffi::live_stage_handle_count()
}



/// Configuration of one IDTX datasource prim, read off the stage by
/// [`StageService::exec_data_sources`] and handed to the host driver. `kind` is
/// `"rest"` (fetch `endpoint`(+`query`) over HTTP every `interval`s) or `"mock"`
/// (generate random JSON locally). The driver feeds the body back via
/// [`StageService::exec_enqueue`]; the work never touches the stage thread.
#[cfg(feature = "openexec")]
#[derive(Debug, Clone, PartialEq)]
pub struct ExecDataSource {
    /// Absolute path of the datasource prim (the enqueue key).
    pub prim_path: String,
    /// Driver kind: `"rest"` or `"mock"`.
    pub kind: String,
    /// Base endpoint URL (REST only).
    pub endpoint: String,
    /// Url-encoded query appended after the endpoint path (REST only).
    pub query: String,
    /// HTTP method: `"GET"` or `"POST"` (REST only).
    pub method: String,
    /// `Authorization` header value (REST only; empty = no header).
    pub authorization: String,
    /// JSON body sent on `POST` (REST only).
    pub json_body: String,
    /// Poll / generate interval in seconds (schema default `1.0`).
    pub interval: f32,
}

#[cfg(feature = "openexec")]
impl ExecDataSource {
    fn from_raw(raw: ffi::ExecDataSourceRaw) -> Self {
        Self {
            prim_path: raw.prim_path,
            kind: raw.kind,
            endpoint: raw.endpoint,
            query: raw.query,
            method: raw.method,
            authorization: raw.authorization,
            json_body: raw.json_body,
            interval: raw.interval,
        }
    }
}

/// One resolved datasource INPUT value, produced by
/// [`StageService::exec_data_source_inputs`]. A datasource prim's
/// `authorization` / `jsonBody` attribute may be driven by a `Compute_*` node
/// (e.g. `authorization.connect = </…/GetAuthToken.outputs:value>` resolving a
/// `${auth:TOKEN}` template). The exec bridge recomputes it every tick and hands
/// the resolved string here — never authored onto the stage — for the REST poller
/// to apply at fetch time.
#[cfg(feature = "openexec")]
#[derive(Debug, Clone, PartialEq)]
pub struct ExecDataSourceInput {
    /// Absolute path of the datasource prim carrying the connected input.
    pub prim_path: String,
    /// Input attribute name, e.g. `"authorization"` or `"jsonBody"`.
    pub attribute_name: String,
    /// Resolved string value from the last tick.
    pub value: String,
}

/// Register a filesystem path containing USD schema plugins for runtime discovery.
///
/// Call before opening any stage that uses the plugin's schema types.
pub fn register_plugin_path(path: impl AsRef<std::path::Path>) -> Result<(), StageError> {
    let path_str = path
        .as_ref()
        .to_str()
        .ok_or_else(|| StageError::NonUtf8Path(path.as_ref().to_path_buf()))?;
    ffi::register_plugin_path(path_str).map_err(|e| StageError::OpenUsd(e.what().to_owned()))
}

/// Reads the bytes of any asset resolvable by the `OpenUSD` `ArResolver` without
/// requiring a live stage handle.  The resolver must already be initialised
/// (i.e. at least one stage must have been opened in this process).
pub fn read_asset(resolved_path: &str) -> Result<Vec<u8>, StageError> {
    ffi::read_asset_global(resolved_path).map_err(|e| StageError::OpenUsd(e.what().to_owned()))
}

/// Reject oversized resolver assets before requesting their buffer or copying
/// into Rust. Network downloads retain the resolver's separate transport cap.
pub fn read_asset_bounded(resolved_path: &str, max_bytes: u64) -> Result<Vec<u8>, StageError> {
    ffi::read_asset_global_bounded(resolved_path, max_bytes).map_err(|e| StageError::OpenUsd(e.what().to_owned()))
}

/// Layout-asserted bulk copy of a packed `[f32; 3]` slice into `glam::Vec3` values.
/// See [`idtx_usd_sys::vec3f_bulk_copy`] for details.
pub use idtx_usd_sys::vec3f_bulk_copy;

/// Inspection summary of one skeleton: its path, joint count, whether an
/// animation query resolved, and the bound animation prim path if any.
#[derive(Debug, Clone)]
pub struct SkeletonSummary {
    pub path: String,
    pub joint_count: u32,
    pub has_anim_query: bool,
    pub animation_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkinBindingSummary {
    pub path: String,
    pub skeleton_path: Option<String>,
    pub point_count: u64,
    pub skinning_method: String,
    pub influences_per_point: u32,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct AnimationSummary {
    pub path: String,
    pub skeleton_path: Option<String>,
    pub start_time_code: f64,
    pub end_time_code: f64,
    pub has_translations: bool,
    pub has_rotations: bool,
    pub has_scales: bool,
    pub has_blend_shapes: bool,
}

/// Aggregated skeleton, skin-binding, and animation summaries for a stage,
/// returned by the skeleton-inspection bridge call.
#[derive(Debug, Clone, Default)]
pub struct SkelSummaries {
    pub skeletons: Vec<SkeletonSummary>,
    pub skin_bindings: Vec<SkinBindingSummary>,
    pub animations: Vec<AnimationSummary>,
}

/// Which composition arc introduced a layer into the stage's layer stack.
///
/// The three local arcs (`Session`, `Root`, `SubLayer`) build the root layer
/// stack. `Reference` and `Payload` are per-prim arcs that pull external layers
/// into the composition; they are discovered by a stage traversal and surfaced
/// here so the Layer Stack panel reports the full set of contributing layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerArcKind {
    /// The anonymous session layer (strongest).
    Session,
    /// The stage root layer.
    Root,
    /// A sublayer pulled in by a `subLayers` arc (recursively).
    SubLayer,
    /// A layer pulled in by a `references` arc on some prim.
    Reference,
    /// A layer pulled in by a `payload` arc on some prim (may be unloaded).
    Payload,
}

/// One layer in the stage's composed layer stack, returned strongest-first by
/// `StageService::layer_stack`.
///
/// Read-only and requested on demand — never on the per-frame render path.
/// Callers should cache the result and refresh only on topology state changes
/// that advance the stage generation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRecord {
    /// Full layer identifier (file path, URI, or `anon:0x…`).
    pub identifier: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Resolved real filesystem path; `None` for anonymous/session layers.
    pub real_path: Option<String>,
    /// The arc that introduced this layer.
    pub arc: LayerArcKind,
    /// For `Reference`/`Payload` arcs: the introducing prim path in the
    /// referencing layer stack (e.g. `/World/Pawn`). `None` for local arcs.
    pub arc_source_path: Option<String>,
    /// For `Reference`/`Payload` arcs: the target prim path inside the
    /// referenced layer (e.g. `/Pawn`). `None` for local arcs.
    pub arc_target_path: Option<String>,
    /// Whether this arc's node contributes prim specs. Always `true` for local
    /// session/root/sublayer arcs.
    pub has_specs: bool,
    /// Whether the arc's content is loaded. `false` only for a deferred
    /// (unloaded) payload; `true` for references and local layers.
    pub is_loaded: bool,
    /// Nesting depth for indentation (session and root are `0`).
    pub depth: u32,
    /// `SdfLayerOffset` offset applied by the parent sublayer arc.
    pub offset: f64,
    /// `SdfLayerOffset` time scale applied by the parent sublayer arc.
    pub scale: f64,
    /// Number of prim specs this layer contributes (the `Δ` badge).
    pub delta_count: u32,
    pub is_anonymous: bool,
    pub is_dirty: bool,
    pub is_muted: bool,
    /// True when this layer is the stage's current edit target.
    pub is_edit_target: bool,
    /// Identifier of the layer that directly introduced this layer.
    /// Empty for the session and root layers.  For sublayers: the containing
    /// layer's identifier.  For reference/payload arcs: the anchor layer
    /// (the layer that authors the arc spec).
    pub parent_identifier: String,
}

/// Typed representation of a USD attribute value returned by `eval_attribute`.
///
/// `value_repr` is the `VtValue` ostringstream serialization from C++ — suitable
/// for display and JSON round-tripping but not for numeric extraction. Parse
/// `type_name` and deserialize `value_repr` for structured use.
#[derive(Debug, Clone)]
pub struct AttributeValue {
    pub type_name: String,
    pub value_repr: String,
    pub is_time_varying: bool,
    pub time_sample_count: u64,
}


#[cfg(feature = "authoring")]
fn path_text(path: &Path) -> Result<&str, StageError> {
    path.to_str()
        .ok_or_else(|| StageError::NonUtf8Path(path.to_path_buf()))
}

#[cfg(feature = "authoring")]
fn persistence_result(raw: ffi::PersistenceResultRaw) -> PersistenceResult {
    PersistenceResult {
        destination: PathBuf::from(raw.destination),
        file_format: raw.file_format,
        package_members: raw.package_members,
        dependencies: raw.dependencies,
        warnings: raw.warnings,
    }
}

fn logical_metadata_bytes(metadata: &StageMetadata) -> u64 {
    let strings = metadata.root_identifier.len()
        + metadata.default_prim.as_deref().map_or(0, str::len)
        + metadata.up_axis.len();
    u64::try_from(strings + 4 * std::mem::size_of::<f64>()).unwrap_or(u64::MAX)
}

fn path_is_below(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn register_plugins_once() -> Result<(), StageError> {
    match PLUGIN_REGISTRATION.get_or_init(|| {
        let root = PLUGIN_ROOT
            .get()
            .map_or(idtx_usd_sys::layout::USD_DIR, |path| {
                path.to_str().unwrap_or(idtx_usd_sys::layout::USD_DIR)
            });
        ffi::register_plugins(root).map_err(|error| error.what().to_owned())
    }) {
        Ok(()) => Ok(()),
        Err(error) => Err(StageError::OpenUsd(error.clone())),
    }
}



#[cfg(test)]
mod tests;
