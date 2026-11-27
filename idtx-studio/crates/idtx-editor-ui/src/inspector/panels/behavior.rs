//! Behavior-attachment inspector panel.
//!
//! A dedicated inspector section for the prim↔behavior attachment, modelled on
//! the sibling **IDTX-POC** behavior inspector. It lets an author:
//! - **attach** a `.wasm` behavior to the selected prim / **detach** it,
//! - **enable/disable** the attachment (drop it from the tick loop without
//!   detaching),
//! - open the behavior's Rust **source in VS Code** for editing, and
//! - **compile-check** the behavior (`cargo check --target wasm32-…`) and see
//!   the rustc diagnostics inline.
//!
//! **Explicit non-goal.** This panel never *plays or runs* a behavior. There is
//! deliberately no play/pause/stop here — the lifecycle hooks
//! (`on_start`/`on_update`/…) are driven by the transport/session and the
//! standalone runtime, and playback ownership stays there. The panel is a pure
//! authoring + tooling surface.
//!
//! Actions are routed through the [`EditSink`] behavior methods;
//! the shell resolves the concrete `.wasm`/source paths, runs the external
//! editor + `cargo check`, and publishes the check result back via
//! [`set_behavior_check`] for [`draw`](BehaviorInspectorPanel::draw) to render.

use egui::{RichText, Ui};

use crate::inspector::{InspectorContext, InspectorContribution, PropertyClaimSet};

/// Session snapshot published by the shell; exact context bytes and native USD
/// resolution, shared with runtime queries rather than parsed display summaries.
#[derive(Debug, Clone)]
pub struct AuthoredBindings {
    pub context: Result<Option<String>, String>,
    pub relationships: Vec<idtx_usd::RelationshipBinding>,
    pub candidates: Vec<String>,
}

pub fn set_authored_bindings(ctx: &egui::Context, path: &str, data: AuthoredBindings) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("behavior-bindings", path)), data));
}

#[derive(Clone, Default)]
struct ContextDraft { source: Option<Result<Option<String>, String>>, text: String }

fn draw_authored_bindings(ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
    let path = ctx.info.path.clone();
    let Some(data) = ui.ctx().data(|d| d.get_temp::<AuthoredBindings>(egui::Id::new(("behavior-bindings", &path)))) else { return; };
    egui::CollapsingHeader::new("Authored context and targets").default_open(true).show(ui, |ui| {
        ui.label(RichText::new("Edits author local overrides for this referenced instance.").small().weak());
        let draft_id = egui::Id::new(("behavior-context-draft", &path));
        let mut draft = ui.ctx().data(|d| d.get_temp::<ContextDraft>(draft_id)).unwrap_or_default();
        if draft.source.as_ref() != Some(&data.context) {
            draft.text = data.context.as_ref().ok().and_then(|v| v.clone()).unwrap_or_else(|| "{}".into());
            draft.source = Some(data.context.clone());
        }
        if let Err(error) = &data.context { ui.colored_label(crate::theme::current().status_warn, error); }
        ui.add(egui::TextEdit::multiline(&mut draft.text).code_editor().desired_rows(4));
        let diagnostic = if draft.text.len() > 8192 { Some("Context exceeds 8192 UTF-8 bytes".to_owned()) }
            else { serde_json::from_str::<serde_json::Value>(&draft.text).err().map(|e| e.to_string()) };
        ui.label(format!("{} / 8192 bytes", draft.text.len()));
        if let Some(error) = &diagnostic { ui.colored_label(crate::theme::current().status_warn, error); }
        if ui.add_enabled(ctx.is_cookable() && diagnostic.is_none(), egui::Button::new("Apply context")).clicked() {
            ctx.set_behavior_context(draft.text.clone());
        }
        ui.label(RichText::new("Applying context reattaches the behavior and resets its runtime state at the next callback boundary.").small().weak());
        ui.ctx().data_mut(|d| d.insert_temp(draft_id, draft));
        ui.separator();
        for binding in &data.relationships {
            ui.label(RichText::new(&binding.name).monospace());
            if !binding.resolved { ui.colored_label(crate::theme::current().status_warn, "Unresolved forwarding chain (native USD)"); }
            if binding.targets.is_empty() { ui.label("(empty relationship)"); }
            for (target, status) in binding.targets.iter().zip(&binding.statuses) {
                ui.horizontal(|ui| {
                    crate::inspector::widgets::prim_target_link(ui, ctx, target);
                    ui.label(match status { 0 => "resolved", 2 => "unloaded", _ => "unresolved / invalid" });
                });
            }
        }
        let name_id = egui::Id::new(("behavior-binding-name", &path));
        let targets_id = egui::Id::new(("behavior-binding-targets", &path));
        let mut name = ui.ctx().data(|d| d.get_temp::<String>(name_id)).unwrap_or_else(|| "coordinator".into());
        let mut targets = ui.ctx().data(|d| d.get_temp::<String>(targets_id)).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("Relationship");
            ui.text_edit_singleline(&mut name);
            egui::ComboBox::from_id_salt("binding-existing").selected_text("Edit existing…").show_ui(ui, |ui| {
                for property in ctx.properties.iter().filter(|p| matches!(p.kind, idtx_usd::PropertyKind::Relationship)) {
                    if ui.selectable_label(false, &property.name).clicked() {
                        name.clone_from(&property.name);
                        targets = property.relationship_targets.join("\n");
                    }
                }
            });
        });
        ui.label("Targets (one composed absolute path per line, in order)");
        ui.add(egui::TextEdit::multiline(&mut targets).desired_rows(2));
        egui::ComboBox::from_id_salt("binding-target-picker").selected_text("Add scene target…").show_ui(ui, |ui| {
            for path in &data.candidates {
                if ui.selectable_label(false, path).clicked() {
                    if !targets.is_empty() { targets.push('\n'); }
                    targets.push_str(path);
                }
            }
        });
        if ui.add_enabled(ctx.is_cookable() && !name.is_empty(), egui::Button::new("Apply targets")).clicked() {
            ctx.set_behavior_relationship(&name, targets.lines().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect());
        }
        ui.ctx().data_mut(|d| { d.insert_temp(name_id, name); d.insert_temp(targets_id, targets); });
    });
}

/// USD attribute names for the behavior attachment. These mirror the canonical
/// constants in `idtx_scene::behavior::attr` (kept as string literals here so
/// the pure-egui `idtx-editor-ui` crate need not depend on `idtx-scene`).
mod attr {
    /// USD type name of the behavior `HasAPI` schema (`IdtxBehaviorAPI`).
    /// Presence of this applied schema is the canonical "this prim has a
    /// behavior" test.
    pub const SCHEMA: &str = "IdtxBehaviorAPI";
    /// Namespace prefix shared by every behavior attribute (`idtx:behavior:`).
    /// Used to test whether *any* behavior opinion is authored on a prim without
    /// enumerating each attribute.
    pub const PREFIX: &str = "idtx:behavior:";
    /// The compiled-module asset attribute (Asset = the `.wasm` module path).
    pub const MODULE: &str = "idtx:behavior:module";
    /// Enabled flag (`idtx:behavior:enabled`).
    pub const ENABLED: &str = "idtx:behavior:enabled";
    /// Execution kind token (`idtx:behavior:kind`).
    pub const KIND: &str = "idtx:behavior:kind";
    /// Binding scope token (`idtx:behavior:scope`).
    pub const SCOPE: &str = "idtx:behavior:scope";
    /// Server-authoritative flag (`idtx:behavior:serverAuthoritative`).
    pub const SERVER_AUTHORITATIVE: &str = "idtx:behavior:serverAuthoritative";
    /// Authoring source (`.rs`) asset path (`idtx:behavior:source`) — the
    /// editor-facing half a Conductor-created behavior carries before its
    /// `.wasm` is built.
    pub const SOURCE: &str = "idtx:behavior:source";
    /// Module uuid (`idtx:behavior:uuid`).
    pub const UUID: &str = "idtx:behavior:uuid";
    /// Behavior display name (`idtx:behavior:name`).
    pub const NAME: &str = "idtx:behavior:name";
    pub const CONTEXT: &str = "idtx:behavior:context";
    /// Semantic version string (`idtx:behavior:version`).
    pub const VERSION: &str = "idtx:behavior:version";
    /// Ship flag (`idtx:behavior:ship`).
    pub const SHIP: &str = "idtx:behavior:ship";
    /// Granted capability buckets (`idtx:behavior:capabilities`, `token[]`).
    pub const CAPABILITIES: &str = "idtx:behavior:capabilities";
    /// Declared input actions (`idtx:behavior:inputs`, `token[]`), each a
    /// `"<action>:<kind>"` token.
    pub const INPUTS: &str = "idtx:behavior:inputs";
    /// Optional per-behavior memory cap in MiB (`idtx:behavior:limits:memoryMb`).
    pub const MEMORY_MB: &str = "idtx:behavior:limits:memoryMb";
}

/// Managed editor preparation/profile status, published asynchronously by the shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BehaviorEditorState {
    #[default]
    Idle,
    Preparing(String),
    Ready(String),
    SetupRequired(String),
    Failed(String),
}

pub fn set_behavior_editor(ctx: &egui::Context, session_id: u64, prim_path: &str, state: BehaviorEditorState) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(("behavior-editor", session_id, prim_path)), state));
}

#[must_use]
pub fn behavior_editor(ctx: &egui::Context, session_id: u64, prim_path: &str) -> BehaviorEditorState {
    ctx.data(|d| d.get_temp(egui::Id::new(("behavior-editor", session_id, prim_path)))).unwrap_or_default()
}

/// The result of the most recent compile-check for a prim's behavior.
///
/// Stored per-prim in the egui temp data store: the panel reads it during
/// `draw`; the shell writes it (via [`set_behavior_check`]) when it kicks off a
/// check and again when the background `cargo check` completes. `Clone` so it
/// can live in egui's data store.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BehaviorCheckState {
    /// No check has run this session.
    #[default]
    Idle,
    /// A `cargo check` is in flight.
    Running,
    /// The behavior compiled cleanly.
    Passed,
    /// The behavior failed to compile; carries the verbatim rustc diagnostics.
    Failed(String),
}

/// egui temp-store id for a prim's [`BehaviorCheckState`], keyed by prim path so
/// each prim tracks its own result.
fn check_state_id(prim_path: &str) -> egui::Id {
    egui::Id::new(("idtx-editor-ui.behavior-check", prim_path))
}

/// Publish the compile-check state for `prim_path` (called by the shell). The
/// panel reads it next frame with [`behavior_check`].
pub fn set_behavior_check(ctx: &egui::Context, prim_path: &str, state: BehaviorCheckState) {
    let id = check_state_id(prim_path);
    ctx.data_mut(|d| d.insert_temp(id, state));
}

/// Read the compile-check state for `prim_path` (defaults to
/// [`BehaviorCheckState::Idle`]).
#[must_use]
pub fn behavior_check(ctx: &egui::Context, prim_path: &str) -> BehaviorCheckState {
    let id = check_state_id(prim_path);
    ctx.data(|d| d.get_temp::<BehaviorCheckState>(id))
        .unwrap_or_default()
}

/// The required / missing / unused capability buckets from the most recent
/// link-verification, as plain wire tokens. Kept as `Vec<String>` so this
/// pure-egui crate stays free of an `idtx-behavior` type dependency — the shell
/// derives it from `idtx_behavior::LinkReport` and publishes it here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSummary {
    /// Whether the behavior links under its authored capabilities.
    pub links: bool,
    /// The minimal capability set the code needs.
    pub required: Vec<String>,
    /// Required but not granted — these cause the link failure at stage load.
    pub missing: Vec<String>,
    /// Granted but unused — safe to drop.
    pub unused: Vec<String>,
}

/// The result of the most recent capability link-verification for a prim.
///
/// Stored per-prim in the egui temp store, mirroring [`BehaviorCheckState`]:
/// the panel reads it during `draw`; the shell writes it (via
/// [`set_behavior_verify`]) when it kicks off a verify and again when the
/// background build + link probe completes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BehaviorVerifyState {
    /// No verification has run this session.
    #[default]
    Idle,
    /// A build + link probe is in flight.
    Running,
    /// Verification completed; carries the link summary.
    Ok(LinkSummary),
    /// Verification could not complete (build failed, unsatisfiable imports,
    /// …); carries a human-facing message.
    Failed(String),
}

/// egui temp-store id for a prim's [`BehaviorVerifyState`].
fn verify_state_id(prim_path: &str) -> egui::Id {
    egui::Id::new(("idtx-editor-ui.behavior-verify", prim_path))
}

/// Publish the link-verification state for `prim_path` (called by the shell).
pub fn set_behavior_verify(ctx: &egui::Context, prim_path: &str, state: BehaviorVerifyState) {
    let id = verify_state_id(prim_path);
    ctx.data_mut(|d| d.insert_temp(id, state));
}

/// Read the link-verification state for `prim_path` (defaults to
/// [`BehaviorVerifyState::Idle`]).
#[must_use]
pub fn behavior_verify(ctx: &egui::Context, prim_path: &str) -> BehaviorVerifyState {
    let id = verify_state_id(prim_path);
    ctx.data(|d| d.get_temp::<BehaviorVerifyState>(id))
        .unwrap_or_default()
}

/// The build-artifact state of a **source-authored** behavior (one carrying
/// `idtx:behavior:source`), surfaced as a badge in the panel. The `.wasm` is a
/// build artifact produced before preview / at packaging, so a source-only prim
/// is normally uncompiled in the authoring session; the shell may publish
/// [`Stale`](Self::Stale)/[`Built`](Self::Built) once it has run (or cached) a
/// build.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BehaviorBuildState {
    /// No build artifact is known — the module is produced at preview/package.
    #[default]
    NeedsBuild,
    /// A previously-built artifact exists but the source has since changed.
    Stale,
    /// A `.wasm` built from the current source exists.
    Built,
}

/// egui temp-store id for a prim's [`BehaviorBuildState`].
fn build_state_id(prim_path: &str) -> egui::Id {
    egui::Id::new(("idtx-editor-ui.behavior-build", prim_path))
}

/// Publish the build-artifact state for `prim_path` (called by the shell).
pub fn set_behavior_build_state(ctx: &egui::Context, prim_path: &str, state: BehaviorBuildState) {
    let id = build_state_id(prim_path);
    ctx.data_mut(|d| d.insert_temp(id, state));
}

/// Read the build-artifact state for `prim_path` (defaults to
/// [`BehaviorBuildState::NeedsBuild`]).
#[must_use]
pub fn behavior_build_state(ctx: &egui::Context, prim_path: &str) -> BehaviorBuildState {
    let id = build_state_id(prim_path);
    ctx.data(|d| d.get_temp::<BehaviorBuildState>(id))
        .unwrap_or_default()
}

/// Extract the authored asset path from a property `value_summary`.
///
/// USD's `operator<<` prints an `SdfAssetPath` as `@<path>@`; recover the text
/// between the first and last `@` (falling back to the trimmed summary). Returns
/// `None` when the result is empty — the same "empty asset ⇒ not attached"
/// convention the hydrator's `read_asset_attribute` uses.
#[must_use]
fn asset_path_from_summary(summary: &str) -> Option<String> {
    let s = summary.trim();
    let path = match (s.find('@'), s.rfind('@')) {
        (Some(first), Some(last)) if last > first => &s[first + 1..last],
        _ => s,
    }
    .trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_owned())
    }
}

/// The authored `.wasm` module path for the selected prim, or `None` when no
/// behavior is attached (attribute absent or blocked to an empty path).
fn attached_module(ctx: &InspectorContext<'_>) -> Option<String> {
    let prop = ctx.property(attr::MODULE)?;
    if !prop.authored {
        return None;
    }
    asset_path_from_summary(&prop.value_summary)
}

/// The authored `idtx:behavior:source` (`.rs`) path for the selected prim, or
/// `None` when no source is authored — the signal a behavior was *created* from
/// the Conductor but not yet compiled to a `.wasm` module.
fn attached_source(ctx: &InspectorContext<'_>) -> Option<String> {
    let prop = ctx.property(attr::SOURCE)?;
    if !prop.authored {
        return None;
    }
    asset_path_from_summary(&prop.value_summary)
}

/// `true` when the behavior's `idtx:behavior:enabled` override reads false; a
/// missing override defaults to enabled.
fn enabled_flag(ctx: &InspectorContext<'_>) -> bool {
    ctx.property(attr::ENABLED)
        .filter(|p| p.authored)
        .is_none_or(|p| !matches!(p.value_summary.trim(), "0" | "false" | "False"))
}

/// Strip USD's quoting/`@`-delimiters from a token or string `value_summary`,
/// returning the bare token or `None` when empty.
fn bare_token(summary: &str) -> Option<String> {
    let t = summary.trim().trim_matches('"').trim_matches('@').trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// The authored `idtx:behavior:kind` token, defaulting to `"runtime"` when the
/// override is absent — the same default the hydrator applies.
fn kind_token(ctx: &InspectorContext<'_>) -> String {
    ctx.property(attr::KIND)
        .filter(|p| p.authored)
        .and_then(|p| bare_token(&p.value_summary))
        .unwrap_or_else(|| "runtime".to_owned())
}

/// The authored `idtx:behavior:scope` token, defaulting to `"entity"`.
fn scope_token(ctx: &InspectorContext<'_>) -> String {
    ctx.property(attr::SCOPE)
        .filter(|p| p.authored)
        .and_then(|p| bare_token(&p.value_summary))
        .unwrap_or_else(|| "entity".to_owned())
}

/// `true` when the behavior's `idtx:behavior:serverAuthoritative` override reads
/// true; a missing override defaults to false.
fn server_authoritative_flag(ctx: &InspectorContext<'_>) -> bool {
    ctx.property(attr::SERVER_AUTHORITATIVE)
        .filter(|p| p.authored)
        .is_some_and(|p| matches!(p.value_summary.trim(), "1" | "true" | "True"))
}

/// Input-action kind tokens, matching `idtx_behavior::manifest::InputKind`.
const INPUT_KIND_TOKENS: &[&str] = &["button", "axis", "axis2"];

/// The `value_summary` of a `token[]` attribute, or an empty string when absent
/// / not authored.
fn token_array_summary(ctx: &InspectorContext<'_>, attr: &str) -> String {
    ctx.property(attr)
        .filter(|p| p.authored)
        .map(|p| p.value_summary.clone())
        .unwrap_or_default()
}

/// Strip the `"[N values] "` array-size prefix the USD bridge prepends to every
/// array `value_summary` (see `summarize_value` in `usd_shim.cpp`), returning the
/// bare `TfStringify` body (e.g. `[input, scene_query]`).
///
/// This prefix is **not** part of the array data; parsing it as a token is what
/// corrupted the authored `token[]` — each edit read the previous summary, kept
/// the `[N values]` prefix as a bogus leading token, and re-authored it,
/// compounding `[7 values] [6 values] …` on every toggle.
fn strip_array_count_prefix(summary: &str) -> &str {
    let s = summary.trim_start();
    if let Some(rest) = s.strip_prefix('[')
        && let Some(close) = rest.find(']')
        && let Some(count) = rest[..close].strip_suffix(" values")
        && !count.is_empty()
        && count.bytes().all(|b| b.is_ascii_digit())
    {
        return rest[close + 1..].trim_start();
    }
    s
}

/// Parse a USD `token[]` `value_summary` (e.g. `[input, scene_query]` or
/// `["right_grab:button", "left_grab:button"]`) into its member tokens.
/// Strips the bridge's `"[N values] "` prefix first (see
/// [`strip_array_count_prefix`]), then splits on `,`/brackets only, preserving
/// `_` and `:` inside a token.
fn parse_token_array_summary(summary: &str) -> Vec<String> {
    strip_array_count_prefix(summary)
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|t| t.trim().trim_matches('"').trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Decode an `"<action>:<kind>"` input token into `(action, kind)`; `None` when
/// malformed or the kind is not a known input kind.
fn decode_input(token: &str) -> Option<(String, String)> {
    let (action, kind) = token.rsplit_once(':')?;
    if action.is_empty() || !INPUT_KIND_TOKENS.contains(&kind) {
        return None;
    }
    Some((action.to_owned(), kind.to_owned()))
}

/// The authored value of a `string` attribute (unquoted), or `""` when absent.
fn text_attr(ctx: &InspectorContext<'_>, attr: &str) -> String {
    ctx.property(attr)
        .filter(|p| p.authored)
        .and_then(|p| bare_token(&p.value_summary))
        .unwrap_or_default()
}

/// The authored value of a `bool` attribute, or `default` when absent.
fn bool_attr(ctx: &InspectorContext<'_>, attr: &str, default: bool) -> bool {
    ctx.property(attr)
        .filter(|p| p.authored)
        .map_or(default, |p| matches!(p.value_summary.trim(), "1" | "true" | "True"))
}

/// The authored value of an `int` attribute, or `default` when absent/unparseable.
fn int_attr(ctx: &InspectorContext<'_>, attr: &str, default: i32) -> i32 {
    ctx.property(attr)
        .filter(|p| p.authored)
        .and_then(|p| p.value_summary.trim().parse::<i32>().ok())
        .unwrap_or(default)
}

/// egui temp-data key for the project's available input actions, published by
/// the shell each frame and read by the inputs table.
fn available_actions_id() -> egui::Id {
    egui::Id::new("idtx_behavior_available_input_actions")
}

/// Publish the project's available input actions (`(name, kind)` pairs, from
/// `input_map.toml`) so the behavior inputs table can offer them in its add
/// combo. Called by the shell each frame it builds the inspector.
pub fn set_available_input_actions(ctx: &egui::Context, actions: Vec<(String, String)>) {
    ctx.data_mut(|d| d.insert_temp(available_actions_id(), actions));
}

/// The project's available input actions published via
/// [`set_available_input_actions`], or an empty list when none were published.
fn available_input_actions(ctx: &egui::Context) -> Vec<(String, String)> {
    ctx.data(|d| d.get_temp::<Vec<(String, String)>>(available_actions_id()))
        .unwrap_or_default()
}

/// egui temp-data key for the capability vocabulary, published by the shell each
/// frame and read by the capabilities table.
fn available_capabilities_id() -> egui::Id {
    egui::Id::new("idtx_behavior_available_capabilities")
}

/// Publish the capability vocabulary — `(token, human label)` pairs, in
/// canonical order — so the capabilities table renders and offers the full set
/// without the panel hardcoding it. The shell derives this from
/// `idtx_behavior::KNOWN_CAPABILITIES` + `idtx_behavior::capability_label`, the
/// single source of truth, so a new capability needs no panel edit. Called each
/// frame the inspector is built (mirrors [`set_available_input_actions`]).
pub fn set_available_capabilities(ctx: &egui::Context, vocab: Vec<(String, String)>) {
    ctx.data_mut(|d| d.insert_temp(available_capabilities_id(), vocab));
}

/// The capability vocabulary published via [`set_available_capabilities`], or an
/// empty list when none was published (e.g. a non-authoring shell).
fn available_capabilities(ctx: &egui::Context) -> Vec<(String, String)> {
    ctx.data(|d| d.get_temp::<Vec<(String, String)>>(available_capabilities_id()))
        .unwrap_or_default()
}

/// Human label for a capability `token` from the published vocabulary, falling
/// back to the bare token when the vocabulary is unavailable or the token is
/// unknown.
fn capability_label_from(vocab: &[(String, String)], token: &str) -> String {
    vocab
        .iter()
        .find(|(t, _)| t == token)
        .map_or_else(|| token.to_owned(), |(_, label)| label.clone())
}

/// Human label for a behavior-kind token.
fn kind_label(token: &str) -> &'static str {
    match token {
        "editor" => "Editor (authoring only)",
        _ => "Runtime (player)",
    }
}

/// Human label for a behavior-scope token.
fn scope_label(token: &str) -> &'static str {
    match token {
        "stage" => "Stage (one per stage)",
        _ => "Entity (per prim)",
    }
}

/// Severity of a [`role_hint`] — drives the inspector label colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoleHintLevel {
    /// Informational: the behavior will tick in this session.
    Info,
    /// Warning: the behavior will **not** tick in this session under the authority gate.
    Warn,
}

/// A player-role execution hint for the Behaviors panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleHint {
    pub level: RoleHintLevel,
    pub message: String,
}

/// Compute the execution hint for a behavior, mirroring the authority gate
/// (`attach_authored_behaviors`): `Local`/`Unowned` always tick locally; `Remote`
/// never ticks locally; `Server` ticks locally only when the behavior is
/// server-authoritative.
///
/// `role` is the resolved player-role token (`"local"`/`"remote"`/`"server"`/
/// `"unowned"`) from `PrimInfo::resolved_player_role`; `None` when unresolved.
/// Returns `None` when there is nothing worth surfacing — an unresolved role, or
/// a role that ticks with no caveat (the single-player `local` default).
pub(crate) fn role_hint(role: Option<&str>, server_authoritative: bool) -> Option<RoleHint> {
    match role? {
        "remote" => Some(RoleHint {
            level: RoleHintLevel::Warn,
            message: "Owned by a remote player — this behavior will not run in this session."
                .to_owned(),
        }),
        "server" if server_authoritative => Some(RoleHint {
            level: RoleHintLevel::Info,
            message: "Server-authoritative — runs on the authoritative server role only."
                .to_owned(),
        }),
        "server" => Some(RoleHint {
            level: RoleHintLevel::Warn,
            message: "On a server-owned player but not marked server-authoritative — \
                      it will not run here."
                .to_owned(),
        }),
        _ => None,
    }
}

/// Which of the five behavior-panel states the selected prim is in, driving
/// whether details render, whether the config is editable, and whether the
/// panel offers Create/Attach or Overwrite. Derived by [`behavior_panel_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BehaviorPanelState {
    /// Behavior present (composed `HasAPI`), root-authored, on an editable prim —
    /// full editing (the long-standing path).
    Editable,
    /// Behavior present but the owning prim is not editable (no local opinion) —
    /// details read-only, no active actions.
    ReadOnly,
    /// Behavior present but it composes entirely from a *weaker* layer; the
    /// editable prim's `over` authors nothing for it — details read-only plus an
    /// **Overwrite** button to pull it into the current layer.
    OverwriteWeaker,
    /// No composed behavior API, but weaker-layer `idtx:behavior:*` opinions
    /// linger (the `over` *detached* the API and the attributes can't be removed
    /// here) — hide the stale details and offer **Overwrite** instead of Create.
    Detached,
    /// No behavior at all — Create/Attach when the prim is editable.
    Unattached,
}

/// Classify the behavior panel state from the four composed signals. Pure so it
/// is unit-tested exhaustively; see the module tests.
///
/// - `prim_editable`: the shared editability gate (mode + local root-layer
///   opinion); here it means "the prim has a local `over`".
/// - `has_api`: composed `HasAPI(IdtxBehaviorAPI)` — false once an `over`
///   detaches the API even if weaker-layer attributes remain.
/// - `behavior_authored`: any `idtx:behavior:*` attribute authored (any layer).
/// - `behavior_root_authored`: any such attribute authored in the edit layer.
#[must_use]
pub(crate) fn behavior_panel_state(
    prim_editable: bool,
    has_api: bool,
    behavior_authored: bool,
    behavior_root_authored: bool,
) -> BehaviorPanelState {
    if has_api {
        if !prim_editable {
            BehaviorPanelState::ReadOnly
        } else if behavior_root_authored {
            BehaviorPanelState::Editable
        } else {
            BehaviorPanelState::OverwriteWeaker
        }
    } else if behavior_authored {
        BehaviorPanelState::Detached
    } else {
        BehaviorPanelState::Unattached
    }
}

/// Behavior-attachment inspector section.
#[derive(Debug)]
pub struct BehaviorInspectorPanel;

impl InspectorContribution for BehaviorInspectorPanel {
    fn label(&self) -> &'static str {
        "Behavior"
    }

    fn default_open(&self) -> bool {
        true
    }

    // After the shape/material/light sections, before relationships/fallback.
    fn priority(&self) -> i32 {
        300
    }

    fn claim(&self, claims: &mut PropertyClaimSet) {
        // Claim every behavior attribute so the fallback panel does not also
        // list them as raw properties.
        for name in [
            attr::MODULE,
            attr::ENABLED,
            attr::KIND,
            attr::SCOPE,
            attr::SERVER_AUTHORITATIVE,
            attr::SOURCE,
            attr::UUID,
            attr::NAME,
            attr::VERSION,
            attr::SHIP,
            attr::CAPABILITIES,
            attr::INPUTS,
            attr::MEMORY_MB,
            attr::CONTEXT,
        ] {
            claims.claim(name);
        }
    }

    fn present(&self, ctx: &InspectorContext<'_>, _claims: &PropertyClaimSet) -> bool {
        // never show if the context does not allow cooking
        if !ctx.is_cookable() {
            return false;
        }

        // Never on the pseudo-root. Show whenever cooking is available (so an
        // author can inspect / attach / create / overwrite a behavior) or a
        // behavior is already composed (`HasAPI(IdtxBehaviorAPI)`) — the latter so
        // a read-only viewer still sees it.
        if ctx.info.path == "/" {
            return false;
        }
        ctx.has_applied_schema(attr::SCHEMA) || ctx.is_cookable()
    }

    fn draw(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, _claims: &PropertyClaimSet) {
        render_behavior_body(ui, ctx);
    }
}

/// Render the behavior-attachment section body for the prim in `ctx` — the
/// shared surface used both by [`BehaviorInspectorPanel`] (the prim inspector)
/// and the Stage tab's stage-level behavior section (driven against the
/// `/StageBehavior` prim). Resolves the five-state machine
/// ([`behavior_panel_state`]) and renders the matching UI, then the compile-check
/// result. The caller is responsible for the `present()` gate (cookable, not
/// pseudo-root).
pub(crate) fn render_behavior_body(ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
    let panel = BehaviorInspectorPanel;
    // Signals driving the panel state. `has_api` is the *composed*
    // `HasAPI(IdtxBehaviorAPI)` (projected from `UsdPrim::GetAppliedSchemas`),
    // so it flips to false once an `over` *detaches* the API even though the
    // weaker-layer `idtx:behavior:*` attributes linger. `prim_editable` is the
    // shared editability gate (mode + a local root-layer opinion) every other
    // inspector panel honours — in this Cooking-only panel it means "the prim
    // has a local `over`".
    let prim_editable = ctx.is_prim_editable();
    let has_api = ctx.has_applied_schema(attr::SCHEMA);
    let behavior_authored = ctx
        .properties
        .iter()
        .any(|p| p.authored && p.name.starts_with(attr::PREFIX));
    let behavior_root_authored = ctx
        .properties
        .iter()
        .any(|p| p.root_authored && p.name.starts_with(attr::PREFIX));

    match behavior_panel_state(prim_editable, has_api, behavior_authored, behavior_root_authored) {
        BehaviorPanelState::Editable => panel.draw_behavior(ui, ctx, true, false),
        BehaviorPanelState::ReadOnly => panel.draw_behavior(ui, ctx, false, false),
        BehaviorPanelState::OverwriteWeaker => panel.draw_behavior(ui, ctx, false, true),
        BehaviorPanelState::Detached => panel.draw_detached(ui, ctx, prim_editable),
        BehaviorPanelState::Unattached => panel.draw_unattached(ui, ctx, prim_editable),
    }

    // Compile-check result (if any) — rendered for all states so a stale
    // result stays visible right after a detach.
    panel.draw_check_result(ui, &ctx.info.path.clone());
    // Capability link-verification result (if any).
    panel.draw_verify_result(ui, &ctx.info.path.clone());
}

#[allow(clippy::unused_self)]
impl BehaviorInspectorPanel {
    /// The behavior authoring body — the single config surface for a prim that
    /// carries `IdtxBehaviorAPI` (created, attached, or just schema-assigned).
    ///
    /// In the Conductor the stage is never cooked, so a behavior normally carries
    /// only its authoring source (`.rs`) and is cross-compiled to a `.wasm`
    /// module at the cook step — never here. The panel therefore always exposes
    /// the full config (identity, role hint, enable/kind/scope/authority, the
    /// manifest config, and Edit/Check/Detach), regardless of whether a compiled
    /// module exists yet.
    ///
    /// `config_editable` greys the config + action controls read-only (prim not
    /// editable, or the behavior composes from a weaker layer); `show_overwrite`
    /// adds an active **Overwrite Behavior…** button that pulls a weaker-layer
    /// behavior into the current layer.
    fn draw_behavior(
        &self,
        ui: &mut Ui,
        ctx: &mut InspectorContext<'_>,
        config_editable: bool,
        show_overwrite: bool,
    ) {
        // Identity — the authoring source (`.rs`) and/or the compiled module
        // (`.wasm`). A Conductor-created behavior carries only the source until
        // it is cooked; an attach-by-`.wasm` behavior carries only the module.
        let source = attached_source(ctx);
        let module = attached_module(ctx);
        if let Some(src) = &source {
            ui.horizontal(|ui| {
                ui.label(RichText::new("source:").small().weak());
                ui.label(RichText::new(src).small().monospace())
                    .on_hover_text(src);
            });
        }
        if let Some(m) = &module {
            ui.horizontal(|ui| {
                ui.label(RichText::new("module:").small().weak());
                ui.label(RichText::new(m).small().monospace())
                    .on_hover_text(m);
            });
        }
        if let Some(uuid) = ctx.property(attr::UUID).filter(|p| p.authored) {
            let short: String = uuid
                .value_summary
                .trim()
                .trim_matches('"')
                .chars()
                .take(8)
                .collect();
            if !short.is_empty() {
                ui.label(
                    RichText::new(format!("uuid: {short}…"))
                        .small()
                        .weak()
                        .monospace(),
                );
            }
        }

        // Build-artifact badge — a source-authored behavior is compiled to
        // `.wasm` at the cook step, so it is normally uncompiled while authoring.
        // Only meaningful when a source is authored (an attach-by-`.wasm`
        // behavior has no source and is compiled by definition).
        if source.is_some() {
            let badge = match behavior_build_state(ui.ctx(), &ctx.info.path) {
                BehaviorBuildState::NeedsBuild => (
                    "⚠ not yet compiled — built at the cook step",
                    crate::theme::current().status_warn,
                ),
                BehaviorBuildState::Stale => (
                    "↻ source changed — rebuild pending",
                    crate::theme::current().status_warn,
                ),
                BehaviorBuildState::Built => (
                    "✔ compiled module up to date",
                    crate::theme::current().status_ok,
                ),
            };
            ui.label(RichText::new(badge.0).small().color(badge.1))
                .on_hover_text(badge.0);
        }

        // Player-role execution hint — shown in every mode so a read-only viewer
        // also learns a remote/server pawn's behavior will not tick here. Mirrors
        // the authority gate.
        if let Some(hint) = role_hint(
            ctx.info.resolved_player_role.as_deref(),
            server_authoritative_flag(ctx),
        ) {
            let color = match hint.level {
                RoleHintLevel::Warn => crate::theme::current().status_warn,
                RoleHintLevel::Info => crate::theme::current().status_info,
            };
            ui.label(RichText::new(&hint.message).small().color(color))
                .on_hover_text(&hint.message);
        }

        // Config + actions — greyed/read-only when the config is not editable
        // (the prim has no local opinion, or the behavior composes from a weaker
        // layer). `add_enabled_ui` blocks interaction, so the `ctx` setters never
        // fire while disabled.
        ui.add_enabled_ui(config_editable, |ui| {
            self.draw_behavior_controls(ui, ctx);
        });

        draw_authored_bindings(ui, ctx);

        // Overwrite — offered when the behavior composes from a weaker layer and
        // the editable prim's `over` authors nothing for it. Active even though
        // the config above is read-only; it pulls an editable copy into this
        // layer (fresh source + uuid).
        if show_overwrite {
            ui.horizontal(|ui| {
                if ui.button("Make Local Copy…")
                    .on_hover_text("Copy the inherited source collection into this level and preserve its behavior config")
                    .clicked() {
                    ctx.duplicate_behavior_source();
                }
                if ui
                    .button("✨ Overwrite Behavior…")
                    .on_hover_text(
                        "Create a fresh local behavior in this layer, carrying over the \
                         inherited config (new source + uuid)",
                    )
                    .clicked()
                {
                    ctx.overwrite_behavior();
                }
            });
            ui.label(
                RichText::new(
                    "Code and manifest are inherited. Context and targets can be overridden above.",
                )
                .small()
                .weak(),
            );
        } else if !config_editable {
            ui.label(
                RichText::new(
                    "Create a scene-tree override to edit code and manifest. \
                     Context and target edits create their own local opinions.",
                )
                .small()
                .weak(),
            );
        }
    }

    /// The editable config + action controls (enable/kind/scope/authority, the
    /// manifest config, and Edit/Check/Detach). Rendered inside an
    /// [`egui::Ui::add_enabled_ui`] gate by [`Self::draw_behavior`] so the whole
    /// block greys out read-only when the config is not editable; the `ctx`
    /// setters are themselves no-ops unless the prim is editable.
    fn draw_behavior_controls(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
        // Enable/disable.
        let mut enabled = enabled_flag(ctx);
        if ui
            .checkbox(&mut enabled, "Enabled")
            .on_hover_text("Disabling drops the behavior from the tick loop without detaching it")
            .changed()
        {
            ctx.set_behavior_enabled(enabled);
        }

        // Execution kind — Runtime (ticks in the packaged player) vs Editor
        // (ticks only in the authoring session).
        let mut kind = kind_token(ctx);
        egui::ComboBox::from_label("Kind")
            .selected_text(kind_label(&kind))
            .show_ui(ui, |ui| {
                let mut changed = false;
                changed |= ui
                    .selectable_value(&mut kind, "runtime".to_owned(), kind_label("runtime"))
                    .changed();
                changed |= ui
                    .selectable_value(&mut kind, "editor".to_owned(), kind_label("editor"))
                    .changed();
                if changed {
                    ctx.set_behavior_kind(&kind);
                }
            });

        // Binding scope — Entity (one instance per prim) vs Stage (a single
        // stage-wide instance).
        let mut scope = scope_token(ctx);
        egui::ComboBox::from_label("Scope")
            .selected_text(scope_label(&scope))
            .show_ui(ui, |ui| {
                let mut changed = false;
                changed |= ui
                    .selectable_value(&mut scope, "entity".to_owned(), scope_label("entity"))
                    .changed();
                changed |= ui
                    .selectable_value(&mut scope, "stage".to_owned(), scope_label("stage"))
                    .changed();
                if changed {
                    ctx.set_behavior_scope(&scope);
                }
            });

        // Server-authoritative — required for a behavior on a server-owned
        // player to tick (see the authority gate / role hint above).
        let mut server_auth = server_authoritative_flag(ctx);
        if ui
            .checkbox(&mut server_auth, "Server-authoritative")
            .on_hover_text(
                "Runs on the authoritative server role only. On a server-owned player \
                 this must be set for the behavior to tick.",
            )
            .changed()
        {
            ctx.set_behavior_server_authoritative(server_auth);
        }

        // Manifest config — capabilities / inputs / limits, authored straight
        // onto the IdtxBehaviorAPI schema (there is no sidecar idtx.toml).
        self.draw_behavior_config(ui, ctx);

        let editor = behavior_editor(ui.ctx(), ctx.behavior_editor_session_id(), &ctx.info.path);
        let preparing = matches!(editor, BehaviorEditorState::Preparing(_));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!preparing, egui::Button::new("✏ Edit Code"))
                .on_hover_text("Open the managed VS Code workspace with SDK completion, navigation and Rust diagnostics")
                .clicked()
            {
                ctx.edit_behavior_source();
            }
            if ui
                .button("🛠 Cargo Check")
                .on_hover_text("cargo check --target wasm32-… — compile-check only, does not run")
                .clicked()
            {
                // Optimistically flip to Running so the UI updates the same
                // frame; the shell overwrites with Passed/Failed on completion.
                set_behavior_check(ui.ctx(), &ctx.info.path.clone(), BehaviorCheckState::Running);
                ctx.check_behavior();
            }
            if ui
                .button("🔗 Verify capabilities")
                .on_hover_text(
                    "Build the behavior and check it links against the granted capabilities — \
                     reports required / missing / unused. Does not run it.",
                )
                .clicked()
            {
                set_behavior_verify(
                    ui.ctx(),
                    &ctx.info.path.clone(),
                    BehaviorVerifyState::Running,
                );
                ctx.verify_behavior();
            }
            if ui
                .button("🔌 Detach")
                .on_hover_text("Remove the behavior attachment from this prim")
                .clicked()
            {
                ctx.detach_behavior();
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!preparing, egui::Button::new("Regenerate Editor Workspace"))
                .on_hover_text("Repair managed Cargo/editor files and refresh source/SDK paths")
                .clicked() { ctx.regenerate_behavior_workspace(); }
            if ui.button("Make Local Copy…")
                .on_hover_text("Duplicate this source collection and give this attachment its own editable implementation")
                .clicked() { ctx.duplicate_behavior_source(); }
        });
        match editor {
            BehaviorEditorState::Idle => {}
            BehaviorEditorState::Preparing(message) => { ui.horizontal(|ui| { ui.spinner(); ui.label(message); }); }
            BehaviorEditorState::Ready(detail) => {
                ui.label(RichText::new("Editor workspace ready — rust-analyzer provides completion and checks on save.").small())
                    .on_hover_text(detail);
            }
            BehaviorEditorState::SetupRequired(message) | BehaviorEditorState::Failed(message) => {
                ui.colored_label(crate::theme::current().status_warn, message);
            }
        }
        if ui.add_enabled(!preparing, egui::Button::new("Set up rust-analyzer"))
            .on_hover_text("Install rust-analyzer in the selected VS Code profile, then open the workspace")
            .clicked() { ctx.setup_behavior_editor(); }
        ui.label(RichText::new("Save all files in VS Code before Check, Verify or Preview. Trust the workspace in VS Code to enable Cargo and procedural macros.").small().weak());
    }

    /// Render the "behavior detached in a stronger layer" state: the `over` has
    /// deleted `IdtxBehaviorAPI` so `HasAPI` is false, but weaker-layer
    /// `idtx:behavior:*` attributes linger and can't be removed here. The stale
    /// details are deliberately not rendered; when the prim is editable an
    /// **Overwrite** button creates a fresh local behavior (carrying over the
    /// lingering config).
    fn draw_detached(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, prim_editable: bool) {
        ui.label(
            RichText::new(
                "Behavior detached in a stronger layer — weaker-layer opinions remain but \
                 can't be removed here.",
            )
            .small()
            .weak(),
        );
        if prim_editable {
            ui.horizontal(|ui| {
                if ui
                    .button("✨ Overwrite Behavior…")
                    .on_hover_text(
                        "Create a fresh local behavior in this layer, carrying over the \
                         inherited config (new source + uuid)",
                    )
                    .clicked()
                {
                    ctx.overwrite_behavior();
                }
            });
        }
    }

    /// The manifest-config area: capabilities, declared input actions, memory
    /// cap, version, and ship flag — all authored straight onto the
    /// `IdtxBehaviorAPI` schema (there is no sidecar `idtx.toml`). Shown only in
    /// an editable (Conductor) context.
    fn draw_behavior_config(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
        use idtx_editor_model::AttributeValue;

        egui::CollapsingHeader::new("Manifest config")
            .default_open(false)
            .show(ui, |ui| {
                // ── identity: name + version ───────────────────────────────
                let mut version = text_attr(ctx, attr::VERSION);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("version").small().weak());
                    if ui.text_edit_singleline(&mut version).lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        ctx.set_behavior_property(attr::VERSION, AttributeValue::String(version.clone()));
                    }
                });

                // ── ship flag ──────────────────────────────────────────────
                let mut ship = bool_attr(ctx, attr::SHIP, false);
                if ui
                    .checkbox(&mut ship, "Ship in packaged runtime")
                    .on_hover_text("Editor behaviors: include this behavior when packaging the runtime player")
                    .changed()
                {
                    ctx.set_behavior_property(attr::SHIP, AttributeValue::Bool(ship));
                }

                // ── memory cap ─────────────────────────────────────────────
                let mut mem = int_attr(ctx, attr::MEMORY_MB, 0).max(0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("memory cap (MiB)").small().weak());
                    let resp = ui.add(
                        egui::DragValue::new(&mut mem)
                            .range(0..=4096)
                            .speed(1.0),
                    );
                    if resp.changed() {
                        ctx.set_behavior_property(attr::MEMORY_MB, AttributeValue::Int(mem));
                    }
                    ui.label(RichText::new("(0 = host default; clamp-only)").small().weak());
                });

                // ── capabilities: add/remove table + verify hints ──────────
                ui.separator();
                ui.label(RichText::new("capabilities").small().weak());
                self.draw_capabilities_table(ui, ctx);

                // ── input actions: add/remove table ────────────────────────
                ui.separator();
                ui.label(RichText::new("input actions").small().weak());
                self.draw_input_actions_table(ui, ctx);
            });
    }

    /// The capabilities add/remove table. One row per *granted* bucket (with a
    /// remove button), rows for any *missing* bucket the most recent verify
    /// found required-but-ungranted (with a grant button), a footer combo to add
    /// any ungranted bucket from the published vocabulary, and — when a verify
    /// summary exists with a delta — an "Apply required set" button that writes
    /// exactly the capabilities the code needs. Every edit authors the whole
    /// `idtx:behavior:capabilities` token[] — complete assignment, no list-op
    /// (mirrors [`Self::draw_input_actions_table`]). The vocabulary is published
    /// by the shell from `idtx_behavior::KNOWN_CAPABILITIES`, so this panel never
    /// needs editing when a capability is added.
    fn draw_capabilities_table(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
        use idtx_editor_model::AttributeValue;

        let prim_path = ctx.info.path.clone();
        let vocab = available_capabilities(ui.ctx());
        let granted = parse_token_array_summary(&token_array_summary(ctx, attr::CAPABILITIES));
        let verify = behavior_verify(ui.ctx(), &prim_path);
        let (missing, unused) = match &verify {
            BehaviorVerifyState::Ok(s) => (s.missing.clone(), s.unused.clone()),
            _ => (Vec::new(), Vec::new()),
        };
        let theme = crate::theme::current();

        let mut next = granted.clone();
        let mut changed = false;

        // Rows: one per granted capability. `unused` grants are flagged.
        for tok in &granted {
            ui.horizontal(|ui| {
                let label = capability_label_from(&vocab, tok);
                let is_unused = unused.iter().any(|u| u == tok);
                let mut text = RichText::new(&label).small();
                if is_unused {
                    text = text.color(theme.status_info);
                }
                ui.label(text);
                if is_unused {
                    ui.label(
                        RichText::new("• unused")
                            .small()
                            .color(theme.status_info),
                    )
                    .on_hover_text("Granted but the code never uses it — safe to remove");
                }
                if ui
                    .small_button("🗑")
                    .on_hover_text("Remove this capability")
                    .clicked()
                {
                    next.retain(|c| c != tok);
                    changed = true;
                }
            });
        }

        // Rows: capabilities the code requires but the prim does not grant —
        // these are the ones that cause the LinkFailed at stage load.
        for tok in &missing {
            ui.horizontal(|ui| {
                let label = capability_label_from(&vocab, tok);
                ui.label(
                    RichText::new(format!("⚠ {label}"))
                        .small()
                        .color(theme.status_warn),
                )
                .on_hover_text("Required by the code but not granted — the behavior will not link");
                if ui
                    .small_button("➕")
                    .on_hover_text("Grant this required capability")
                    .clicked()
                    && !next.iter().any(|c| c == tok)
                {
                    next.push(tok.clone());
                    changed = true;
                }
            });
        }

        // Footer: add a capability not already granted, from the vocabulary.
        let addable: Vec<(String, String)> = vocab
            .iter()
            .filter(|(tok, _)| !next.iter().any(|c| c == tok))
            .cloned()
            .collect();
        if addable.is_empty() {
            if vocab.is_empty() {
                ui.label(
                    RichText::new("Capability vocabulary unavailable in this shell.")
                        .small()
                        .weak(),
                );
            } else {
                ui.label(
                    RichText::new("All capabilities are already granted.")
                        .small()
                        .weak(),
                );
            }
        } else {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("behavior_capability_add")
                    .selected_text("➕ Add capability…")
                    .show_ui(ui, |ui| {
                        for (tok, label) in &addable {
                            if ui.selectable_label(false, label).clicked() {
                                next.push(tok.clone());
                                changed = true;
                            }
                        }
                    });
            });
        }

        // One-click reconcile: set the grant to exactly what the code needs.
        if let BehaviorVerifyState::Ok(s) = &verify {
            if !s.missing.is_empty() || !s.unused.is_empty() {
                if ui
                    .button("Apply required set")
                    .on_hover_text(
                        "Set capabilities to exactly what the code needs \
                         (grants missing, drops unused)",
                    )
                    .clicked()
                {
                    ctx.set_behavior_property(
                        attr::CAPABILITIES,
                        AttributeValue::TokenArray(s.required.clone()),
                    );
                    return;
                }
            }
        }

        if changed {
            ctx.set_behavior_property(attr::CAPABILITIES, AttributeValue::TokenArray(next));
        }
    }

    /// The input-actions add/remove table. Each row is one declared action with
    /// a kind dropdown + a remove button; a footer combo adds an action drawn
    /// from the project `input_map.toml` (each action addable once). Every edit
    /// authors the whole `idtx:behavior:inputs` token[] (`"<action>:<kind>"`
    /// entries) — complete assignment, no list-op.
    fn draw_input_actions_table(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>) {
        use idtx_editor_model::AttributeValue;

        // Current declared inputs, decoded from the token[].
        let current: Vec<(String, String)> =
            parse_token_array_summary(&token_array_summary(ctx, attr::INPUTS))
                .iter()
                .filter_map(|t| decode_input(t))
                .collect();

        // Rows: action label + kind dropdown + remove.
        let mut next = current.clone();
        let mut changed = false;
        for (idx, (action, kind)) in current.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(action).monospace().small());
                let mut k = kind.clone();
                egui::ComboBox::from_id_salt(("behavior_input_kind", idx))
                    .selected_text(&k)
                    .show_ui(ui, |ui| {
                        for &kt in INPUT_KIND_TOKENS {
                            if ui.selectable_value(&mut k, kt.to_owned(), kt).changed() {
                                next[idx].1 = kt.to_string();
                                changed = true;
                            }
                        }
                    });
                if ui.small_button("🗑").on_hover_text("Remove this input").clicked() {
                    next.retain(|(a, _)| a != action);
                    changed = true;
                }
            });
        }

        // Footer: add an action not already present, from input_map.toml.
        let available = available_input_actions(ui.ctx());
        let addable: Vec<(String, String)> = available
            .into_iter()
            .filter(|(name, _)| !next.iter().any(|(a, _)| a == name))
            .collect();
        if addable.is_empty() {
            ui.label(
                RichText::new("All project input actions are already added (or input_map.toml is empty).")
                    .small()
                    .weak(),
            );
        } else {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("behavior_input_add")
                    .selected_text("➕ Add input…")
                    .show_ui(ui, |ui| {
                        for (name, kind) in &addable {
                            if ui.selectable_label(false, format!("{name} ({kind})")).clicked() {
                                next.push((name.clone(), kind.clone()));
                                changed = true;
                            }
                        }
                    });
            });
        }

        if changed {
            let tokens: Vec<String> =
                next.iter().map(|(a, k)| format!("{a}:{k}")).collect();
            ctx.set_behavior_property(attr::INPUTS, AttributeValue::TokenArray(tokens));
        }
    }


    fn draw_unattached(&self, ui: &mut Ui, ctx: &mut InspectorContext<'_>, editable: bool) {
        if editable {
            ui.label(RichText::new("No behavior attached.").small().weak());
            ui.horizontal(|ui| {
                if ui
                    .button("✨ Create Behavior…")
                    .on_hover_text(
                        "Create a new Rust behavior source for this prim (compiled to \
                         .wasm before preview / at packaging)",
                    )
                    .clicked()
                {
                    ctx.create_behavior();
                }
                if ui
                    .button("➕ Attach Behavior…")
                    .on_hover_text("Attach an already-compiled .wasm behavior to this prim")
                    .clicked()
                {
                    ctx.attach_behavior();
                }
            });
        } else {
            ui.label(RichText::new("No behavior attached.").small().weak());
        }
    }

    /// Render the compile-check result for `prim_path`, if any.
    fn draw_check_result(&self, ui: &mut Ui, prim_path: &str) {
        match behavior_check(ui.ctx(), prim_path) {
            BehaviorCheckState::Idle => {}
            BehaviorCheckState::Running => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("cargo check…").small().weak());
                });
            }
            BehaviorCheckState::Passed => {
                ui.label(
                    RichText::new("✔ compiles")
                        .small()
                        .color(crate::theme::current().status_ok),
                );
            }
            BehaviorCheckState::Failed(diagnostics) => {
                ui.label(
                    RichText::new("✘ compile errors")
                        .small()
                        .color(crate::theme::current().status_err),
                );
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(&diagnostics).small().monospace());
                    });
            }
        }
    }

    /// Render the capability link-verification result for `prim_path`, if any.
    fn draw_verify_result(&self, ui: &mut Ui, prim_path: &str) {
        let theme = crate::theme::current();
        match behavior_verify(ui.ctx(), prim_path) {
            BehaviorVerifyState::Idle => {}
            BehaviorVerifyState::Running => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("verifying capabilities…")
                            .small()
                            .weak(),
                    );
                });
            }
            BehaviorVerifyState::Ok(s) => {
                if s.links {
                    ui.label(
                        RichText::new("✔ links — granted capabilities satisfy the code")
                            .small()
                            .color(theme.status_ok),
                    );
                } else {
                    ui.label(
                        RichText::new("✘ won't link — missing capabilities")
                            .small()
                            .color(theme.status_err),
                    );
                }
                if !s.required.is_empty() {
                    ui.label(
                        RichText::new(format!("required: {}", s.required.join(", ")))
                            .small()
                            .weak(),
                    );
                }
                if !s.missing.is_empty() {
                    ui.label(
                        RichText::new(format!("⚠ missing: {}", s.missing.join(", ")))
                            .small()
                            .color(theme.status_warn),
                    );
                }
                if !s.unused.is_empty() {
                    ui.label(
                        RichText::new(format!("• unused: {}", s.unused.join(", ")))
                            .small()
                            .color(theme.status_info),
                    );
                }
            }
            BehaviorVerifyState::Failed(msg) => {
                ui.label(
                    RichText::new("✘ verification failed")
                        .small()
                        .color(theme.status_err),
                );
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(&msg).small().monospace());
                    });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_path_strips_at_delimiters() {
        assert_eq!(
            asset_path_from_summary("@behaviors/spin.wasm@").as_deref(),
            Some("behaviors/spin.wasm")
        );
        assert_eq!(
            asset_path_from_summary("  behaviors/spin.wasm ").as_deref(),
            Some("behaviors/spin.wasm")
        );
    }

    #[test]
    fn empty_asset_reads_as_detached() {
        assert_eq!(asset_path_from_summary("@@"), None);
        assert_eq!(asset_path_from_summary("  "), None);
        assert_eq!(asset_path_from_summary(""), None);
    }

    #[test]
    fn check_state_id_is_per_prim() {
        assert_ne!(check_state_id("/World/A"), check_state_id("/World/B"));
        assert_eq!(check_state_id("/World/A"), check_state_id("/World/A"));
    }

    #[test]
    fn capability_vocabulary_round_trips_through_temp_store() {
        // The panel is agnostic to the capability set: it renders whatever the
        // shell publishes. Verify the publish/read round-trip and the label
        // lookup the capabilities table relies on.
        let ctx = egui::Context::default();
        // Nothing published yet → empty, and label lookup falls back to the token.
        assert!(available_capabilities(&ctx).is_empty());
        assert_eq!(capability_label_from(&[], "net"), "net");

        let vocab = vec![
            ("scene_query".to_owned(), "scene_query (raycast / find)".to_owned()),
            ("level_control".to_owned(), "level_control (request level transition)".to_owned()),
        ];
        set_available_capabilities(&ctx, vocab.clone());
        assert_eq!(available_capabilities(&ctx), vocab);
        assert_eq!(
            capability_label_from(&vocab, "level_control"),
            "level_control (request level transition)"
        );
        // Unknown token still falls back to the bare token.
        assert_eq!(capability_label_from(&vocab, "mystery"), "mystery");
    }

    #[test]
    fn verify_state_id_is_per_prim_and_defaults_idle() {
        assert_ne!(verify_state_id("/World/A"), verify_state_id("/World/B"));
        assert_eq!(verify_state_id("/World/A"), verify_state_id("/World/A"));
        assert_eq!(BehaviorVerifyState::default(), BehaviorVerifyState::Idle);
    }

    #[test]
    fn check_state_defaults_idle() {
        assert_eq!(BehaviorCheckState::default(), BehaviorCheckState::Idle);
    }

    #[test]
    fn build_state_defaults_needs_build_and_id_is_per_prim() {
        assert_eq!(
            BehaviorBuildState::default(),
            BehaviorBuildState::NeedsBuild
        );
        assert_ne!(build_state_id("/World/A"), build_state_id("/World/B"));
        assert_eq!(build_state_id("/World/A"), build_state_id("/World/A"));
        // Distinct id namespaces so a build state never aliases a check state.
        assert_ne!(build_state_id("/World/A"), check_state_id("/World/A"));
    }

    #[test]
    fn bare_token_strips_quotes_and_at_delimiters() {
        assert_eq!(bare_token("\"runtime\"").as_deref(), Some("runtime"));
        assert_eq!(bare_token("  editor ").as_deref(), Some("editor"));
        assert_eq!(bare_token("@mod.wasm@").as_deref(), Some("mod.wasm"));
        assert_eq!(bare_token(""), None);
        assert_eq!(bare_token("  "), None);
    }

    #[test]
    fn kind_and_scope_labels_cover_tokens_and_default() {
        assert_eq!(kind_label("runtime"), "Runtime (player)");
        assert_eq!(kind_label("editor"), "Editor (authoring only)");
        assert_eq!(kind_label("bogus"), "Runtime (player)"); // unknown → default
        assert_eq!(scope_label("entity"), "Entity (per prim)");
        assert_eq!(scope_label("stage"), "Stage (one per stage)");
        assert_eq!(scope_label("bogus"), "Entity (per prim)");
    }

    #[test]
    fn role_hint_local_and_unowned_are_silent() {
        // Single-player defaults tick locally — no caveat worth surfacing.
        assert_eq!(role_hint(Some("local"), false), None);
        assert_eq!(role_hint(Some("local"), true), None);
        assert_eq!(role_hint(Some("unowned"), false), None);
        assert_eq!(role_hint(None, false), None); // unresolved → no hint
    }

    #[test]
    fn role_hint_remote_always_warns() {
        // Remote-owned pawns never tick locally, regardless of server-authority.
        for sa in [false, true] {
            let hint = role_hint(Some("remote"), sa).expect("remote warns");
            assert_eq!(hint.level, RoleHintLevel::Warn);
            assert!(hint.message.contains("remote"));
        }
    }

    #[test]
    fn role_hint_server_depends_on_authority_flag() {
        // Server-owned: ticks locally only when marked server-authoritative.
        let warn = role_hint(Some("server"), false).expect("server without flag warns");
        assert_eq!(warn.level, RoleHintLevel::Warn);

        let info = role_hint(Some("server"), true).expect("server with flag informs");
        assert_eq!(info.level, RoleHintLevel::Info);
    }

    #[test]
    fn role_hint_ignores_unknown_role_tokens() {
        assert_eq!(role_hint(Some("bogus"), true), None);
    }

    #[test]
    fn strip_array_count_prefix_removes_bridge_prefix() {
        // The USD bridge prepends "[N values] " to every array value_summary.
        assert_eq!(
            strip_array_count_prefix("[7 values] [input, scene_query]"),
            "[input, scene_query]"
        );
        assert_eq!(
            strip_array_count_prefix("[1 values] [input]"),
            "[input]"
        );
        // No prefix (e.g. an empty array or a non-array summary) → unchanged.
        assert_eq!(strip_array_count_prefix("[input]"), "[input]");
        assert_eq!(strip_array_count_prefix("[]"), "[]");
        // A genuine first token that merely looks bracket-y must not be eaten:
        // only the exact "[<digits> values]" shape is a count prefix.
        assert_eq!(strip_array_count_prefix("[look:axis2]"), "[look:axis2]");
    }

    #[test]
    fn parse_token_array_strips_count_prefix() {
        // The regression: the "[N values]" prefix must never become a token.
        assert_eq!(
            parse_token_array_summary("[2 values] [input, scene_query]"),
            vec!["input".to_owned(), "scene_query".to_owned()]
        );
        // Quoted token-string elements (as USD serializes them) round-trip too.
        assert_eq!(
            parse_token_array_summary("[2 values] [\"look:axis2\", \"right_grab:button\"]"),
            vec!["look:axis2".to_owned(), "right_grab:button".to_owned()]
        );
        // An absent/empty summary yields no tokens (not a spurious one).
        assert!(parse_token_array_summary("").is_empty());
    }

    // ── behavior_panel_state ──────────────────────────────────────────────────

    #[test]
    fn panel_state_editable_requires_api_root_opinion_and_editable_prim() {
        // Composed HasAPI + a root-layer behavior opinion + an editable prim →
        // the full editing path.
        assert_eq!(
            behavior_panel_state(true, true, true, true),
            BehaviorPanelState::Editable
        );
    }

    #[test]
    fn panel_state_readonly_when_prim_not_editable() {
        // A composed behavior on a prim with no local opinion is read-only,
        // regardless of where the opinion lives.
        assert_eq!(
            behavior_panel_state(false, true, true, true),
            BehaviorPanelState::ReadOnly
        );
        assert_eq!(
            behavior_panel_state(false, true, true, false),
            BehaviorPanelState::ReadOnly
        );
    }

    #[test]
    fn panel_state_overwrite_weaker_when_api_present_but_not_root_authored() {
        // Editable prim, behavior composes from a weaker layer (the `over`
        // authors nothing for it) → read-only details + Overwrite.
        assert_eq!(
            behavior_panel_state(true, true, true, false),
            BehaviorPanelState::OverwriteWeaker
        );
        // The weaker-layer attributes need not even be visible as authored for
        // the state to hold — `has_api` alone (composed) drives it.
        assert_eq!(
            behavior_panel_state(true, true, false, false),
            BehaviorPanelState::OverwriteWeaker
        );
    }

    #[test]
    fn panel_state_detached_when_api_absent_but_attributes_linger() {
        // The `over` detached the API (composed HasAPI false) but weaker-layer
        // `idtx:behavior:*` attributes remain → detached state, independent of
        // editability (the Overwrite button is gated separately).
        assert_eq!(
            behavior_panel_state(true, false, true, false),
            BehaviorPanelState::Detached
        );
        assert_eq!(
            behavior_panel_state(false, false, true, false),
            BehaviorPanelState::Detached
        );
    }

    #[test]
    fn panel_state_unattached_when_no_behavior_at_all() {
        assert_eq!(
            behavior_panel_state(true, false, false, false),
            BehaviorPanelState::Unattached
        );
        assert_eq!(
            behavior_panel_state(false, false, false, false),
            BehaviorPanelState::Unattached
        );
    }
}
