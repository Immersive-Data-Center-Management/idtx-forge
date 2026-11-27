//! Behavior manifest (`idtx.toml`) parsing + capability set (§5.3).
//!
//! The manifest is the host-side source of truth for a behavior's
//! declared kind, parameters, consumed input actions, and granted
//! capabilities. The guest's `_idtx_behavior_desc` export (§3.1) must
//! agree with the parsed manifest — the host validates parity before
//! instantiation (see [`crate::module`]).
//!
//! Unknown capability tokens are a hard load error (no silent
//! downgrade). `filesystem` / `network` are never granted in the MVP.

use std::collections::BTreeMap;

use idtx_behavior_abi::BehaviorKind;
use serde::Deserialize;

/// Parse error surfaced while loading a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The TOML failed to parse.
    #[error("manifest TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    /// An unknown capability token appeared under `[capabilities]`.
    #[error("unknown capability token `{0}` (hard error, no silent downgrade)")]
    UnknownCapability(String),
}

/// A single declared parameter (`[[params]]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ParamDecl {
    /// Parameter name (matches the `#[param]` field on the guest).
    pub name: String,
    /// Wire kind token (`F32`, `Bool`, …). Kept as a string in the MVP.
    #[serde(default)]
    pub kind: String,
    /// Default value as a TOML value (host stores it verbatim).
    #[serde(default)]
    pub default: Option<toml::Value>,
    /// Human-readable inspector label.
    #[serde(default)]
    pub label: Option<String>,
}

/// Kind of a declared input action (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    /// A button-style action (pressed / held / released).
    Button,
    /// A scalar axis `[-1, 1]`.
    Axis,
    /// A 2D axis (`Vec2`).
    Axis2,
}

impl InputKind {
    /// Every variant, in declaration order — handy for UI dropdowns.
    pub const ALL: [InputKind; 3] = [InputKind::Button, InputKind::Axis, InputKind::Axis2];

    /// The lowercase wire token (matches the serde `rename_all = "lowercase"`
    /// spelling and the `IdtxBehaviorAPI` `"<action>:<kind>"` encoding).
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            InputKind::Button => "button",
            InputKind::Axis => "axis",
            InputKind::Axis2 => "axis2",
        }
    }

    /// Parse a wire token back to an [`InputKind`]; `None` for an unknown token.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "button" => Some(InputKind::Button),
            "axis" => Some(InputKind::Axis),
            "axis2" => Some(InputKind::Axis2),
            _ => None,
        }
    }
}

/// A declared input action the behavior consumes (`[[inputs]]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InputDecl {
    /// The named action (`"jump"`, `"move_x"`).
    pub action: String,
    /// The action kind.
    pub kind: InputKind,
}

impl InputDecl {
    /// Encode as the `"<action>:<kind>"` token stored in the
    /// `idtx:behavior:inputs` array. Action names are colon-free identifiers, so
    /// `:` is an unambiguous separator.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}:{}", self.action, self.kind.as_token())
    }

    /// Decode a `"<action>:<kind>"` token back to an [`InputDecl`]. Returns
    /// `None` when there is no `:` separator or the kind token is unknown.
    #[must_use]
    pub fn decode(token: &str) -> Option<Self> {
        let (action, kind) = token.rsplit_once(':')?;
        if action.is_empty() {
            return None;
        }
        Some(Self {
            action: action.to_owned(),
            kind: InputKind::from_token(kind)?,
        })
    }
}

/// The set of capability buckets a behavior is granted (§5.3).
///
/// The [`crate::linker`] links only the buckets set here; an ungranted
/// import is simply absent, so a module referencing it fails to link. A
/// bool per capability bucket is the intended shape, so the >3-bools lint
/// is waived here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    /// Named-input reads (§7).
    pub input: bool,
    /// Scene-graph queries (`find` / `parent` / `child`).
    pub scene_query: bool,
    /// Local-transform writes.
    pub xform_write: bool,
    /// `define` / `remove` prim + generic attribute set.
    pub stage_edit: bool,
    /// Named event `post` / delivery.
    pub events: bool,
    /// XR HMD / controller pose reads (`xr_hmd` bucket).
    pub xr_hmd: bool,
    /// Animation playback-transport commands (`playback` bucket).
    pub playback: bool,
    /// Transient render-overlay commands (`overlay` bucket): draw
    /// lines/arcs, highlight prims, override materials, post-process params.
    pub overlay: bool,
    /// Runtime physics control (`physics` bucket): suspend/resume simulation of
    /// a specific body (grab support — flip a dynamic body to kinematic while
    /// held so the authored pose is not fought by the solver).
    pub physics: bool,
    /// Write the process-global auth bucket (`auth_write` bucket): store a Bearer
    /// token or user/password the `OpenExec` `auth:` provider resolves into an
    /// `Authorization` header. Write-only; a behavior cannot read it back.
    pub auth_write: bool,
    /// Perform blocking HTTP requests host-side (`net` bucket): method / URL /
    /// content-type / body are caller-supplied and the response body is copied
    /// back into the guest. Every request is validated against the **same**
    /// deny-by-default SSRF policy as `OpenExec` REST datasources (shared
    /// `idtx-net-guard` gate — host allowlist, public-address resolution, DNS-
    /// pinned client), so without a configured `[network]` allowlist a request is
    /// denied even when this capability is granted.
    pub net: bool,
    /// Widget interaction (`widget` bucket, ABI 3): register panel-child click
    /// handlers (panel behavior) and hand a widget raycast hit to host-side
    /// interaction processing (pointer behavior). The widget event *read*
    /// imports are always linked; this gates only `register_handler` / `handle`.
    pub widget: bool,
    /// Request a runtime level transition (`level_control` bucket): a behavior
    /// may ask the runtime to travel to another level via
    /// `idtx_request_level_transition(target, spawn)`. The host only *enqueues*
    /// the request; the session surfaces it and the runtime driver performs the
    /// swap (never from inside the host import). Opt-in — withheld by default.
    pub level_control: bool,
}

impl Capabilities {
    /// Every capability granted — used by tests / trusted hosts.
    #[must_use]
    pub fn all() -> Self {
        Self {
            input: true,
            scene_query: true,
            xform_write: true,
            stage_edit: true,
            events: true,
            xr_hmd: true,
            playback: true,
            overlay: true,
            physics: true,
            auth_write: true,
            net: true,
            widget: true,
            level_control: true,
        }
    }

    /// Fold a set of capability tokens into a [`Capabilities`]. Unknown tokens
    /// are ignored (the schema `allowedTokens` already constrains the authored
    /// set; the host treats anything it does not recognise as simply not
    /// granted). A bucket is granted iff its token is present.
    #[must_use]
    pub fn from_tokens<I, S>(tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut caps = Capabilities::default();
        for token in tokens {
            match token.as_ref() {
                "input" => caps.input = true,
                "scene_query" => caps.scene_query = true,
                "xform_write" => caps.xform_write = true,
                "stage_edit" => caps.stage_edit = true,
                "events" => caps.events = true,
                "xr_hmd" => caps.xr_hmd = true,
                "playback" => caps.playback = true,
                "overlay" => caps.overlay = true,
                "physics" => caps.physics = true,
                "auth_write" => caps.auth_write = true,
                "net" => caps.net = true,
                "widget" => caps.widget = true,
                "level_control" => caps.level_control = true,
                _ => {}
            }
        }
        caps
    }

    /// The granted buckets as their wire tokens, in [`KNOWN_CAPABILITIES`]
    /// order — the inverse of [`Capabilities::from_tokens`], for authoring the
    /// `idtx:behavior:capabilities` array.
    #[must_use]
    pub fn to_tokens(&self) -> Vec<&'static str> {
        let flags = [
            (self.input, "input"),
            (self.scene_query, "scene_query"),
            (self.xform_write, "xform_write"),
            (self.stage_edit, "stage_edit"),
            (self.events, "events"),
            (self.xr_hmd, "xr_hmd"),
            (self.playback, "playback"),
            (self.overlay, "overlay"),
            (self.physics, "physics"),
            (self.auth_write, "auth_write"),
            (self.net, "net"),
            (self.widget, "widget"),
            (self.level_control, "level_control"),
        ];
        flags
            .into_iter()
            .filter_map(|(on, tok)| on.then_some(tok))
            .collect()
    }

    /// Grant or revoke a single bucket by its wire token. Unknown tokens are
    /// ignored. Used by the capability-analysis probe to toggle one bucket at a
    /// time (see [`crate::analyze`]).
    pub fn set_token(&mut self, token: &str, granted: bool) {
        match token {
            "input" => self.input = granted,
            "scene_query" => self.scene_query = granted,
            "xform_write" => self.xform_write = granted,
            "stage_edit" => self.stage_edit = granted,
            "events" => self.events = granted,
            "xr_hmd" => self.xr_hmd = granted,
            "playback" => self.playback = granted,
            "overlay" => self.overlay = granted,
            "physics" => self.physics = granted,
            "auth_write" => self.auth_write = granted,
            "net" => self.net = granted,
            "widget" => self.widget = granted,
            "level_control" => self.level_control = granted,
            _ => {}
        }
    }

    /// The buckets granted by `self` but not by `other` (`self & !other`).
    #[must_use]
    pub fn difference(self, other: Self) -> Self {
        Self {
            input: self.input && !other.input,
            scene_query: self.scene_query && !other.scene_query,
            xform_write: self.xform_write && !other.xform_write,
            stage_edit: self.stage_edit && !other.stage_edit,
            events: self.events && !other.events,
            xr_hmd: self.xr_hmd && !other.xr_hmd,
            playback: self.playback && !other.playback,
            overlay: self.overlay && !other.overlay,
            physics: self.physics && !other.physics,
            auth_write: self.auth_write && !other.auth_write,
            net: self.net && !other.net,
            widget: self.widget && !other.widget,
            level_control: self.level_control && !other.level_control,
        }
    }

    /// `true` when no bucket is granted.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self == Capabilities::default()
    }
}

/// A short human label for a capability token, for UI surfaces. Falls back to
/// the raw token for anything not recognised. Keeping the labels here (next to
/// [`KNOWN_CAPABILITIES`]) makes the capability vocabulary a single source of
/// truth that downstream crates can render without re-typing the token list.
#[must_use]
pub fn capability_label(token: &str) -> &str {
    match token {
        "input" => "input (named-input reads)",
        "scene_query" => "scene_query (raycast / find)",
        "xform_write" => "xform_write (local transform)",
        "stage_edit" => "stage_edit (define / set attr)",
        "events" => "events (post / deliver)",
        "xr_hmd" => "xr_hmd (HMD / controller pose)",
        "playback" => "playback (transport control)",
        "overlay" => "overlay (render overlays)",
        "physics" => "physics (suspend / resume body)",
        "auth_write" => "auth_write (store auth token / creds)",
        "net" => "net (HTTP egress, allowlist-gated)",
        "widget" => "widget (panel interaction handler)",
        "level_control" => "level_control (request level transition)",
        other => other,
    }
}

/// The `[behavior]` table.
#[derive(Debug, Clone, Deserialize)]
struct BehaviorTable {
    name: String,
    #[serde(default)]
    version: String,
    uuid: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    ship: bool,
}

/// The full parsed manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorManifest {
    /// Behavior name.
    pub name: String,
    /// Semantic version string.
    pub version: String,
    /// Stable uuid keying the module registry.
    pub uuid: String,
    /// Execution profile.
    pub kind: BehaviorKind,
    /// Editor behaviors: include in the packaged runtime?
    pub ship: bool,
    /// Declared parameters, indexed by name for lookup.
    pub params: BTreeMap<String, ParamDecl>,
    /// Declared input actions.
    pub inputs: Vec<InputDecl>,
    /// Granted capability buckets.
    pub capabilities: Capabilities,
    /// Optional per-behavior linear-memory cap in MiB. `None` = host default.
    /// Clamp-only: it may only LOWER the cap (see [`BehaviorManifest::memory_cap`]).
    pub memory_mb: Option<u32>,
}

/// Raw deserialisation shape of the whole manifest file.
#[derive(Debug, Deserialize)]
struct RawManifest {
    behavior: BehaviorTable,
    #[serde(default)]
    params: Vec<ParamDecl>,
    #[serde(default)]
    inputs: Vec<InputDecl>,
    // We parse capabilities as a free-form table so we can hard-error
    // on unknown tokens rather than silently dropping them.
    #[serde(default)]
    capabilities: BTreeMap<String, bool>,
}

/// The capability tokens the host recognises, in canonical
/// [`Capabilities::to_tokens`] order. Anything else under `[capabilities]` is a
/// hard load error. This is the single source of truth for the capability
/// vocabulary — UI surfaces should render from this list (paired with
/// [`capability_label`]) rather than re-typing their own copy.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "input",
    "scene_query",
    "xform_write",
    "stage_edit",
    "events",
    "xr_hmd",
    "playback",
    "overlay",
    "physics",
    "auth_write",
    "net",
    "widget",
    "level_control",
];

impl BehaviorManifest {
    /// Parse a manifest from TOML text.
    pub fn parse(toml_src: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(toml_src)?;

        // Validate + fold the capability table.
        let mut caps = Capabilities::default();
        for (token, granted) in &raw.capabilities {
            if !KNOWN_CAPABILITIES.contains(&token.as_str()) {
                return Err(ManifestError::UnknownCapability(token.clone()));
            }
            match token.as_str() {
                "input" => caps.input = *granted,
                "scene_query" => caps.scene_query = *granted,
                "xform_write" => caps.xform_write = *granted,
                "stage_edit" => caps.stage_edit = *granted,
                "events" => caps.events = *granted,
                "xr_hmd" => caps.xr_hmd = *granted,
                "playback" => caps.playback = *granted,
                "overlay" => caps.overlay = *granted,
                "physics" => caps.physics = *granted,
                "auth_write" => caps.auth_write = *granted,
                "net" => caps.net = *granted,
                "widget" => caps.widget = *granted,
                "level_control" => caps.level_control = *granted,
                _ => unreachable!("guarded by KNOWN_CAPABILITIES above"),
            }
        }

        let kind = raw
            .behavior
            .kind
            .as_deref()
            .map_or(BehaviorKind::Runtime, BehaviorKind::from_token);

        let params = raw
            .params
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();

        Ok(Self {
            name: raw.behavior.name,
            version: raw.behavior.version,
            uuid: raw.behavior.uuid,
            kind,
            ship: raw.behavior.ship,
            params,
            inputs: raw.inputs,
            capabilities: caps,
            memory_mb: None,
        })
    }

    /// Build a manifest directly from already-decoded parts — the path used when
    /// the config is read from the `IdtxBehaviorAPI` USD schema attributes rather
    /// than a sidecar TOML. `params` is left empty: param *declarations* are not
    /// consumed at attach (only `capabilities` + `inputs` are), and per-prim param
    /// *values* ride on the `BehaviorRef` component.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        name: String,
        version: String,
        uuid: String,
        kind: BehaviorKind,
        ship: bool,
        capabilities: Capabilities,
        inputs: Vec<InputDecl>,
        memory_mb: Option<u32>,
    ) -> Self {
        Self {
            name,
            version,
            uuid,
            kind,
            ship,
            params: BTreeMap::new(),
            inputs,
            capabilities,
            memory_mb,
        }
    }

    /// The effective linear-memory cap in bytes given the `host_default`.
    /// Clamp-only: a per-behavior `memory_mb` may only LOWER the cap below the
    /// host default (the manifest grants, the host enforces — mirrors
    /// capabilities). `None` or an over-large request yields `host_default`.
    #[must_use]
    pub fn memory_cap(&self, host_default: usize) -> usize {
        match self.memory_mb {
            Some(mb) => (mb as usize).saturating_mul(1024 * 1024).min(host_default),
            None => host_default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[behavior]
name    = "spinning_cube"
version = "0.1.0"
uuid    = "5f2a-c7"
kind    = "runtime"
ship    = true

[[params]]
name = "speed_deg"
kind = "F32"
default = 90.0
label = "Spin speed (deg/s)"

[[inputs]]
action = "jump"
kind   = "button"
[[inputs]]
action = "move_x"
kind   = "axis"

[capabilities]
input       = true
scene_query = true
xform_write = true
stage_edit  = false
events      = false
"#;

    #[test]
    fn parses_full_manifest() {
        let m = BehaviorManifest::parse(SAMPLE).unwrap();
        assert_eq!(m.name, "spinning_cube");
        assert_eq!(m.uuid, "5f2a-c7");
        assert_eq!(m.kind, BehaviorKind::Runtime);
        assert!(m.ship);
        assert_eq!(m.params.len(), 1);
        assert!(m.params.contains_key("speed_deg"));
        assert_eq!(m.inputs.len(), 2);
        assert_eq!(m.inputs[0].action, "jump");
        assert_eq!(m.inputs[0].kind, InputKind::Button);
        assert!(m.capabilities.input);
        assert!(m.capabilities.xform_write);
        assert!(!m.capabilities.stage_edit);
        assert!(!m.capabilities.xr_hmd);
    }

    #[test]
    fn parses_xr_hmd_capability() {
        let src = r#"
[behavior]
name = "hmd_camera_sync"
uuid = "idtx-hmd-0000-4000-8000-000000000001"
kind = "runtime"

[capabilities]
xr_hmd      = true
xform_write = true
"#;
        let m = BehaviorManifest::parse(src).unwrap();
        assert!(m.capabilities.xr_hmd);
        assert!(m.capabilities.xform_write);
        assert!(!m.capabilities.scene_query);
    }

    #[test]
    fn parses_physics_capability() {
        let src = r#"
[behavior]
name = "camera_grab"
uuid = "idtx-cgrb-0004-4000-8000-000000000013"
kind = "runtime"

[capabilities]
physics    = true
stage_edit = true
"#;
        let m = BehaviorManifest::parse(src).unwrap();
        assert!(m.capabilities.physics);
        assert!(m.capabilities.stage_edit);
        assert!(!m.capabilities.scene_query);
        // `physics` is granted by `all()` and withheld by `default()`.
        assert!(Capabilities::all().physics);
        assert!(!Capabilities::default().physics);
    }

    #[test]
    fn parses_net_capability_and_roundtrips_token() {
        let src = r#"
[behavior]
name = "auth_login"
uuid = "idtx-auth-0000-4000-8000-000000000001"
kind = "runtime"

[capabilities]
net        = true
auth_write = true
"#;
        let m = BehaviorManifest::parse(src).unwrap();
        assert!(m.capabilities.net);
        assert!(m.capabilities.auth_write);
        assert!(!m.capabilities.scene_query);

        // `net` is granted by `all()`, withheld by `default()`, and survives a
        // tokens round-trip.
        assert!(Capabilities::all().net);
        assert!(!Capabilities::default().net);
        let caps = Capabilities::from_tokens(["net", "auth_write"]);
        assert!(caps.net && caps.auth_write);
        assert!(caps.to_tokens().contains(&"net"));
    }

    #[test]
    fn unknown_capability_is_hard_error() {
        let src = r#"
[behavior]
name = "x"
uuid = "u"

[capabilities]
filesystem = true
"#;
        let err = BehaviorManifest::parse(src).unwrap_err();
        assert!(matches!(err, ManifestError::UnknownCapability(t) if t == "filesystem"));
    }

    #[test]
    fn defaults_when_omitted() {
        let src = r#"
[behavior]
name = "minimal"
uuid = "u2"
"#;
        let m = BehaviorManifest::parse(src).unwrap();
        assert_eq!(m.kind, BehaviorKind::Runtime);
        assert!(!m.ship);
        assert!(m.params.is_empty());
        assert!(m.inputs.is_empty());
        assert_eq!(m.capabilities, Capabilities::default());
    }

    #[test]
    fn input_decl_encode_decode_roundtrip() {
        let decl = InputDecl {
            action: "right_grab".to_owned(),
            kind: InputKind::Button,
        };
        assert_eq!(decl.encode(), "right_grab:button");
        assert_eq!(InputDecl::decode("right_grab:button"), Some(decl));
        assert_eq!(
            InputDecl::decode("move:axis2"),
            Some(InputDecl {
                action: "move".to_owned(),
                kind: InputKind::Axis2
            })
        );
        // Malformed / unknown kind / empty action → None.
        assert_eq!(InputDecl::decode("nope"), None);
        assert_eq!(InputDecl::decode("jump:wobble"), None);
        assert_eq!(InputDecl::decode(":button"), None);
    }

    #[test]
    fn capability_tokens_roundtrip() {
        let caps = Capabilities::from_tokens(["input", "stage_edit", "physics", "bogus"]);
        assert!(caps.input && caps.stage_edit && caps.physics);
        assert!(!caps.scene_query && !caps.xr_hmd);
        // `bogus` is ignored, not an error (the schema allowedTokens constrains it).
        let toks = caps.to_tokens();
        assert_eq!(toks, ["input", "stage_edit", "physics"]);
    }

    #[test]
    fn parses_level_control_capability_and_roundtrips_token() {
        let src = r#"
[behavior]
name = "level_control"
uuid = "idtx-lvlc-0000-4000-8000-000000000001"
kind = "runtime"

[capabilities]
level_control = true
"#;
        let m = BehaviorManifest::parse(src).unwrap();
        assert!(m.capabilities.level_control);
        assert!(!m.capabilities.scene_query);

        // `level_control` is granted by `all()`, withheld by `default()` (opt-in),
        // and survives a tokens round-trip.
        assert!(Capabilities::all().level_control);
        assert!(!Capabilities::default().level_control);
        let caps = Capabilities::from_tokens(["level_control"]);
        assert!(caps.level_control);
        assert_eq!(caps.to_tokens(), ["level_control"]);
    }

    #[test]
    fn from_parts_and_memory_cap_clamp() {
        let m = BehaviorManifest::from_parts(
            "b".to_owned(),
            "0.1.0".to_owned(),
            "uuid".to_owned(),
            BehaviorKind::Runtime,
            true,
            Capabilities::from_tokens(["input"]),
            vec![InputDecl {
                action: "jump".to_owned(),
                kind: InputKind::Button,
            }],
            Some(4),
        );
        assert!(m.params.is_empty());
        assert!(m.capabilities.input);
        assert_eq!(m.inputs.len(), 1);
        // Clamp-only: a 4 MiB request below a 16 MiB default applies; a request
        // above the default is clamped down to the default; None = default.
        let host_default = 16 * 1024 * 1024;
        assert_eq!(m.memory_cap(host_default), 4 * 1024 * 1024);
        let big = BehaviorManifest::from_parts(
            String::new(),
            String::new(),
            String::new(),
            BehaviorKind::Runtime,
            false,
            Capabilities::default(),
            Vec::new(),
            Some(64),
        );
        assert_eq!(big.memory_cap(host_default), host_default);
    }
}
