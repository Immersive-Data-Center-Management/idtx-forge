//! Behavior attachment components + populator scaffold.
//!
//! A prim can carry a *behavior* — a sandboxed WASM module (hosted by
//! `idtx-behavior`) that runs lifecycle hooks against the ECS projection.
//! This module defines the ECS-side data:
//!
//! * [`BehaviorRef`] — a component describing an attached behavior (its
//!   module uuid, kind, enabled flag, tunable params, and asset origin).
//! * [`StageRoot`] — a marker for the entity a *stage-scoped* behavior
//!   (one attached to the pseudo-root, not a concrete prim) binds to.
//! * [`BehaviorPopulator`] — a [`Populator`] scaffold that will hydrate
//!   [`BehaviorRef`] components from authored USD attributes.
//!
//! # Dependency direction
//!
//! This module depends only on the lightweight, `no_std`
//! `idtx-behavior-abi` (for [`BehaviorKind`]) — **not** on the
//! `idtx-behavior` host. The host sits above `idtx-scene`; the per-frame
//! `tick_behaviors(world, host)` glue therefore lives in
//! `idtx-studio-session` (which depends on both), not here, so no
//! dependency cycle forms. (The plan sketches the tick hook in this file;
//! it is relocated to the session for that reason.)
//!
//! # Populator limitation
//!
//! The [`Populator`] trait currently hands `populate` only a
//! [`ViewportProjection`] + [`PrimId`], with no per-attribute USD read
//! (see [`crate::populator`]). [`BehaviorPopulator`] therefore cannot read
//! the authored `idtx:behavior*` attributes, so its `populate` is a
//! documented no-op. A session/authoring path can still attach behaviors
//! directly by inserting a [`BehaviorRef`] via [`BehaviorRef::new`].

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
pub use idtx_behavior_abi::BehaviorKind;
use idtx_usd::{PrimId, ViewportProjection};

use crate::populator::Populator;

/// Where a behavior binds: to its own prim's entity, or to the stage as a
/// whole (the pseudo-root — see [`StageRoot`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BehaviorScope {
    /// Bound to the entity it is authored on.
    #[default]
    Entity,
    /// Bound to the stage pseudo-root (a stage-wide behavior).
    Stage,
}

impl BehaviorScope {
    /// Parse the authored `idtx:behavior:scope` token. Unknown tokens map
    /// to [`BehaviorScope::Entity`].
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "stage" => Self::Stage,
            _ => Self::Entity,
        }
    }

    /// The token this scope round-trips to.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Entity => "entity",
            Self::Stage => "stage",
        }
    }
}

/// A tunable behavior parameter value (authored `idtx:behavior:param:<name>`).
///
/// Kept deliberately small — the MVP surfaces the scalar/flag/text shapes a
/// behavior's `#[param]` fields use.
#[derive(Debug, Clone, PartialEq)]
pub enum BehaviorParamValue {
    /// A 32-bit float.
    F32(f32),
    /// A boolean flag.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// A UTF-8 string.
    Text(String),
}

/// A behavior attached to a prim's entity.
///
/// Stored by value on the entity; the behavior *host* (`idtx-behavior`) is
/// keyed by [`BehaviorRef::uuid`] and instantiated per (entity, uuid). The
/// module bytes come from [`BehaviorRef::module_asset`] (a resolved
/// `.wasm` path) or, when `None`, from a host-registered *embedded* module
/// (e.g. the built-in HMD-sync behavior).
#[derive(Debug, Clone, PartialEq, Component)]
pub struct BehaviorRef {
    /// Stable module uuid keying the host registry.
    pub uuid: String,
    /// Execution profile (runtime vs editor).
    pub kind: BehaviorKind,
    /// Whether the behavior is currently enabled (ticks fire).
    pub enabled: bool,
    /// Authored tunable parameters, keyed by name.
    pub params: BTreeMap<String, BehaviorParamValue>,
    /// Resolved `.wasm` module asset path, or `None` for a host-embedded
    /// module registered under [`BehaviorRef::uuid`].
    pub module_asset: Option<String>,
    /// The authoring source (`.rs`) the module was built from, if known.
    pub source_asset: Option<String>,
    /// Whether the behavior binds to its prim or the whole stage.
    pub stage_scope: BehaviorScope,
    /// Whether this behavior is allowed to tick on a *server-authoritative*
    /// pawn (a [`NetRole::Server`] subtree), in addition to the always-allowed
    /// [`NetRole::Local`] case. `false` (the default) means the behavior only
    /// runs on locally-owned pawns; a `Remote` proxy never ticks either way.
    /// See the authority gate in `idtx-studio-session`.
    ///
    /// [`NetRole::Server`]: crate::player::NetRole::Server
    /// [`NetRole::Local`]: crate::player::NetRole::Local
    pub server_authoritative: bool,
}

/// Stable uuid of the built-in HMD → camera sync behavior.
///
/// Matches the `hmd_camera_sync` behavior template. The XR preview session
/// registers the compiled module under this uuid and attaches a
/// [`BehaviorRef::hmd_camera_sync`] to the preview camera entity.
pub const HMD_CAMERA_SYNC_UUID: &str = "idtx-hmd-0000-4000-8000-000000000001";

/// USD path of the synthesised preview camera created when the open stage
/// has no camera prim of its own.
pub const SYNTHETIC_PREVIEW_CAMERA_PATH: &str = "/World/IDTXPreviewCamera";

impl BehaviorRef {
    /// A minimal enabled runtime behavior referencing a host-embedded
    /// module by uuid (no on-disk `.wasm`). This is exactly the shape the
    /// synthetic-camera HMD-sync attachment uses.
    #[must_use]
    pub fn new(uuid: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            kind: BehaviorKind::Runtime,
            enabled: true,
            params: BTreeMap::new(),
            module_asset: None,
            source_asset: None,
            stage_scope: BehaviorScope::Entity,
            server_authoritative: false,
        }
    }

    /// Builder: set the execution kind.
    #[must_use]
    pub fn with_kind(mut self, kind: BehaviorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Builder: set the binding scope.
    #[must_use]
    pub fn with_scope(mut self, scope: BehaviorScope) -> Self {
        self.stage_scope = scope;
        self
    }

    /// Builder: allow this behavior to tick on a server-authoritative pawn.
    #[must_use]
    pub fn with_server_authoritative(mut self, server_authoritative: bool) -> Self {
        self.server_authoritative = server_authoritative;
        self
    }

    /// Builder: reference an on-disk `.wasm` module asset.
    #[must_use]
    pub fn with_module_asset(mut self, path: impl Into<String>) -> Self {
        self.module_asset = Some(path.into());
        self
    }

    /// Builder: record the authoring source (`.rs`) the module was built from.
    #[must_use]
    pub fn with_source_asset(mut self, path: impl Into<String>) -> Self {
        self.source_asset = Some(path.into());
        self
    }

    /// Builder: set a tunable parameter.
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, value: BehaviorParamValue) -> Self {
        self.params.insert(name.into(), value);
        self
    }

    /// The built-in HMD → camera sync behavior reference.
    ///
    /// A host-embedded runtime behavior keyed by [`HMD_CAMERA_SYNC_UUID`];
    /// the session registers its module bytes under that uuid before
    /// attaching this to the preview camera entity.
    #[must_use]
    pub fn hmd_camera_sync() -> Self {
        Self::new(HMD_CAMERA_SYNC_UUID)
    }
}

/// Marker for the entity a *stage-scoped* behavior binds to (the stage
/// pseudo-root). Inserted by the hydrator/session on the pseudo-root
/// entity so [`BehaviorScope::Stage`] behaviors have a stable target.
#[derive(Debug, Clone, Copy, Default, Component)]
pub struct StageRoot;

/// USD attribute names the authoring side and hydrator agree on, all members of
/// the `IdtxBehaviorAPI` codeless schema (namespace `idtx:behavior`). Presence of
/// that API schema (`HasAPI`) is the canonical "this prim has a behavior" test;
/// these constants name the schema's individual properties.
pub mod attr {
    /// USD type name of the behavior `HasAPI` schema (`idtx_usd_extension::
    /// BEHAVIOR_SCHEMA`). Duplicated as a literal so `idtx-scene` need not depend
    /// on `idtx-usd-extension` for the one name.
    pub const SCHEMA: &str = "IdtxBehaviorAPI";
    /// The `Asset`-typed compiled-module attribute: its value is the `.wasm`
    /// asset path. A resolvable module is what gates runtime ticking; absent for
    /// a source-only (Conductor-created, not-yet-compiled) behavior.
    pub const MODULE: &str = "idtx:behavior:module";
    /// Module uuid.
    pub const UUID: &str = "idtx:behavior:uuid";
    /// Behavior display name (the old `[behavior].name`).
    pub const NAME: &str = "idtx:behavior:name";
    /// Semantic version string (the old `[behavior].version`).
    pub const VERSION: &str = "idtx:behavior:version";
    /// Editor behaviors: include in the packaged runtime? (the old
    /// `[behavior].ship` flag). `bool`.
    pub const SHIP: &str = "idtx:behavior:ship";
    /// Enabled flag.
    pub const ENABLED: &str = "idtx:behavior:enabled";
    /// Execution kind token.
    pub const KIND: &str = "idtx:behavior:kind";
    /// Authoring source (`.rs`) asset path. Editor/authoring side: the Rust
    /// source a behavior is created from and edited as. Cross-compiled to a
    /// `.wasm` [`MODULE`] before preview/packaging; the two are the source vs.
    /// compiled-artifact halves of the same behavior.
    pub const SOURCE: &str = "idtx:behavior:source";
    /// Binding scope token.
    pub const SCOPE: &str = "idtx:behavior:scope";
    /// Server-authoritative flag (`bool`): when `true`, the behavior is also
    /// permitted to tick on a `server`-role pawn, not just a `local` one.
    pub const SERVER_AUTHORITATIVE: &str = "idtx:behavior:serverAuthoritative";
    /// Granted capability buckets (`uniform token[]`). A bucket is granted iff
    /// its token is present; the host links only the granted buckets. This is
    /// the full manifest capability grant — previously in the sidecar `idtx.toml`.
    pub const CAPABILITIES: &str = "idtx:behavior:capabilities";
    /// Declared input actions (`uniform token[]`), one `"<action>:<kind>"` token
    /// per entry. The host resolves each action to a stable id at attach.
    pub const INPUTS: &str = "idtx:behavior:inputs";
    /// Optional per-behavior linear-memory cap in MiB (`int`); clamp-only.
    pub const MEMORY_MB: &str = "idtx:behavior:limits:memoryMb";
    /// Prefix for per-parameter attributes (`idtx:behavior:param:<name>`).
    pub const PARAM_PREFIX: &str = "idtx:behavior:param:";
}

/// Populator that will hydrate [`BehaviorRef`] components from authored
/// USD `idtx:behavior*` attributes.
///
/// See the module docs: the [`Populator`] interface does not yet provide
/// per-attribute reads, so [`Populator::populate`] is currently a
/// documented no-op. The type is complete and registrable so that wiring
/// the `eval_attribute` FFI later is a localized change here.
#[derive(Debug, Clone)]
pub struct BehaviorPopulator {
    type_names: Vec<&'static str>,
}

impl BehaviorPopulator {
    /// Register for the given USD schema `type_names`. Pass an empty slice
    /// to register as the fallback populator (rarely wanted).
    #[must_use]
    pub fn new(type_names: &[&'static str]) -> Self {
        Self {
            type_names: type_names.to_vec(),
        }
    }
}

impl Populator for BehaviorPopulator {
    fn type_names(&self) -> &[&'static str] {
        &self.type_names
    }

    fn populate(
        &self,
        _entity: &mut EntityWorldMut<'_>,
        _prim: PrimId,
        _projection: &ViewportProjection,
    ) {
        // Pending the `eval_attribute` FFI (see `crate::populator`), the
        // authored `idtx:behavior*` attributes are not reachable from the
        // `ViewportProjection` alone, so there is nothing to hydrate yet.
        // A session/authoring path attaches behaviors by inserting a
        // `BehaviorRef` directly.
    }

    fn depopulate(&self, entity: &mut EntityWorldMut<'_>) {
        entity.remove::<BehaviorRef>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::populator::PopulatorRegistry;
    use std::sync::Arc;

    #[test]
    fn behavior_ref_builder() {
        let r = BehaviorRef::new("idtx-hmd-0001")
            .with_kind(BehaviorKind::Runtime)
            .with_scope(BehaviorScope::Entity)
            .with_param("speed", BehaviorParamValue::F32(2.5));
        assert_eq!(r.uuid, "idtx-hmd-0001");
        assert!(r.enabled);
        assert!(r.module_asset.is_none());
        assert_eq!(r.params.get("speed"), Some(&BehaviorParamValue::F32(2.5)));
    }

    #[test]
    fn behavior_ref_source_asset_builder() {
        let r = BehaviorRef::new("uuid-src")
            .with_module_asset("behaviors/spin.wasm")
            .with_source_asset("./behaviors/spin.rs");
        assert_eq!(r.module_asset.as_deref(), Some("behaviors/spin.wasm"));
        assert_eq!(r.source_asset.as_deref(), Some("./behaviors/spin.rs"));
        // Source alone (no module) is also representable.
        let src_only = BehaviorRef::new("uuid-src2").with_source_asset("./behaviors/a.rs");
        assert!(src_only.module_asset.is_none());
        assert_eq!(src_only.source_asset.as_deref(), Some("./behaviors/a.rs"));
    }

    #[test]
    fn behavior_ref_is_a_component() {
        let mut world = World::new();
        let e = world.spawn(BehaviorRef::new("uuid-1")).id();
        assert_eq!(world.get::<BehaviorRef>(e).unwrap().uuid, "uuid-1");
    }

    #[test]
    fn scope_token_round_trip() {
        for s in [BehaviorScope::Entity, BehaviorScope::Stage] {
            assert_eq!(BehaviorScope::from_token(s.as_token()), s);
        }
        assert_eq!(BehaviorScope::from_token("bogus"), BehaviorScope::Entity);
    }

    #[test]
    fn populator_registers_for_its_types() {
        let mut registry = PopulatorRegistry::new();
        registry.register(Arc::new(BehaviorPopulator::new(&["IdtxBehavior"])));
        assert!(registry.get("IdtxBehavior").is_some());
        assert!(registry.get("Mesh").is_none());
    }
}
