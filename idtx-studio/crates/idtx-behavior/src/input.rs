//! Named input action router (§7).
//!
//! Decouples named actions (`"jump"`, `"move_x"`) from physical bindings
//! via an `input_map.toml`. The shell/runtime feeds raw device events
//! into an [`InputRouter`], which produces an [`InputSnapshot`] the host
//! copies into guest-visible state at the top of the dispatch loop.
//!
//! Action *names* resolve to an [`InputActionId`] once (at `on_attach`,
//! via `idtx_input_resolve`), so hot-path reads are integer lookups.

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};

use idtx_behavior_abi::{InputActionId, InputPhase, PointerSource};
use serde::Deserialize;

use crate::manifest::InputKind;

/// A parsed `input_map.toml` — the action → bindings mapping.
///
/// Physical source grammar `Device/Control`: `Key/*`,
/// `Mouse/{Left,Right,Middle,Back,Forward,Button<N>}` (buttons),
/// `Mouse/Delta` (axis2) and `Mouse/Scroll` (axis), `Gamepad/*`,
/// `Controller/*` (XR). Unknown sources warn and are skipped when the
/// snapshot is built.
#[derive(Debug, Clone, Default)]
pub struct InputMap {
    /// Action name → stable id. Ids start at 1 (0 is
    /// [`InputActionId::UNRESOLVED`]).
    name_to_id: HashMap<String, InputActionId>,
    /// Action id → its declared kind + physical bindings.
    actions: HashMap<InputActionId, ActionSpec>,
}

/// A single action's kind + physical bindings.
#[derive(Debug, Clone)]
pub struct ActionSpec {
    /// The action name (for diagnostics).
    pub name: String,
    /// Button / axis / axis2.
    pub kind: InputKind,
    /// Physical source bindings.
    pub bindings: Vec<Binding>,
    /// Ordered value modifiers applied to the resolved **scalar axis** value
    /// Empty = raw value. Buttons ignore
    /// modifiers; axis2 uses [`ActionSpec::radial_deadzone`] instead.
    pub modifiers: Vec<Modifier>,
    /// Optional press-pattern trigger for **button** actions. `None` =
    /// raw edge phases. Ignored for axis / axis2 actions.
    pub trigger: Option<Trigger>,
    /// Action *names* this action is chorded with: the action only
    /// fires when every named action is also active this frame. Source of
    /// truth for the chord — [`ActionSpec::chord`] is the resolved id cache,
    /// rebuilt by a second pass whenever ids are (re)assigned.
    pub chord_names: Vec<String>,
    /// Resolved chord member ids. Filled by the second parse pass in
    /// [`InputMap::parse`] / [`InputMap::from_actions`]; empty = no chord.
    pub chord: Vec<InputActionId>,
    /// Radial (2D) deadzone for **axis2** actions. `None` = raw vector.
    /// Ignored for button / axis actions.
    pub radial_deadzone: Option<f32>,
}

/// A value modifier applied, in declaration order, to an axis action's
/// resolved scalar value (Enhanced-Input semantics).
///
/// Parsed from TOML as a tagged table, e.g.
/// `modifiers = [{ type = "deadzone", zone = 0.15 }, { type = "invert" }]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Modifier {
    /// Radial scalar deadzone: values with `|v| <= zone` collapse to `0`, and
    /// the remainder is rescaled so that `|v| == 1` still maps to `1` (no dead
    /// travel at the top). `zone` is clamped to `[0, 1)`.
    Deadzone {
        /// Half-width of the dead region around zero, in `[0, 1)`.
        zone: f32,
    },
    /// Negate the value (equivalent to a `-1` scale, but composable).
    Invert,
    /// Clamp the value into `[min, max]`.
    Clamp {
        /// Lower bound.
        min: f32,
        /// Upper bound.
        max: f32,
    },
}

impl Modifier {
    /// Apply this single modifier to `value`.
    #[must_use]
    pub fn apply(&self, value: f32) -> f32 {
        match *self {
            Modifier::Deadzone { zone } => {
                // Guard the rescale divisor: a zone at/above 1 would divide by
                // zero (or negative), so clamp into `[0, 1)`.
                let zone = zone.clamp(0.0, 0.999_999);
                let mag = value.abs();
                if mag <= zone {
                    0.0
                } else {
                    value.signum() * ((mag - zone) / (1.0 - zone)).min(1.0)
                }
            }
            Modifier::Invert => -value,
            Modifier::Clamp { min, max } => value.clamp(min, max),
        }
    }
}

/// Apply an ordered modifier chain to a scalar value. Left-to-right:
/// each modifier sees the previous modifier's output.
#[must_use]
pub fn apply_modifiers(value: f32, modifiers: &[Modifier]) -> f32 {
    modifiers.iter().fold(value, |v, m| m.apply(v))
}

/// A press-pattern trigger applied to a **button** action
/// (Enhanced-Input semantics). Without a trigger a button reports the raw
/// Pressed/Held/Released edge phases; a trigger reshapes those edges into
/// hold- or tap-recognised phases.
///
/// Parsed from TOML as a tagged table, e.g.
/// `trigger = { type = "hold", duration = 0.5 }` or
/// `trigger = { type = "tap", max_duration = 0.2 }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// Fires only once the button has been continuously held for at least
    /// `duration` seconds. On the frame the threshold is crossed the action
    /// reports [`InputPhase::Pressed`]; while the hold continues it reports
    /// [`InputPhase::Held`]; on release (after triggering) it reports
    /// [`InputPhase::Released`]. Before the threshold the action reports
    /// [`InputPhase::None`].
    ///
    /// With `one_shot = false` (the default) the trigger *re-fires*
    /// `Pressed` every `duration` seconds while held (autofire/repeat); with
    /// `one_shot = true` it fires `Pressed` exactly once.
    Hold {
        /// Seconds the button must be held before the action fires.
        duration: f32,
        /// Fire `Pressed` only once (`true`) versus repeat every `duration`
        /// seconds while held (`false`, the default).
        #[serde(default)]
        one_shot: bool,
    },
    /// Fires only when the button is pressed and released within
    /// `max_duration` seconds. The action reports [`InputPhase::None`] during
    /// the hold and [`InputPhase::Pressed`] for exactly the release frame when
    /// the tap is recognised; a hold longer than `max_duration` reports
    /// [`InputPhase::None`] throughout.
    Tap {
        /// Maximum seconds between press and release for a tap to register.
        max_duration: f32,
    },
}

/// Apply a radial (2D) deadzone to a vector value. Values whose
/// magnitude is `<= zone` collapse to the origin; the remaining magnitude is
/// rescaled so a fully-deflected source (magnitude `1`) still maps to `1` —
/// the 2D generalisation of [`Modifier::Deadzone`]. Intended for normalised
/// 2D sources (thumbsticks); `zone` is clamped to `[0, 1)`.
#[must_use]
pub fn apply_radial_deadzone(value: [f32; 2], zone: f32) -> [f32; 2] {
    let zone = zone.clamp(0.0, 0.999_999);
    let mag = (value[0] * value[0] + value[1] * value[1]).sqrt();
    if mag <= zone || mag <= f32::EPSILON {
        return [0.0, 0.0];
    }
    let rescaled = ((mag - zone) / (1.0 - zone)).min(1.0);
    let s = rescaled / mag;
    [value[0] * s, value[1] * s]
}

/// One physical binding for an action.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The `Device/Control` source string.
    pub source: String,
    /// Scale applied to the raw value (default `1.0`) — lets `Key/A`
    /// map to `-1` and `Key/D` to `+1` on the same axis.
    pub scale: f32,
}

// ---------------------------------------------------------------------------
// TOML deserialisation shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawInputMap {
    #[serde(default)]
    actions: HashMap<String, RawAction>,
}

#[derive(Debug, Deserialize)]
struct RawAction {
    kind: InputKind,
    #[serde(default)]
    bindings: Vec<RawBinding>,
    #[serde(default)]
    modifiers: Vec<Modifier>,
    /// Optional press-pattern trigger for button actions.
    #[serde(default)]
    trigger: Option<Trigger>,
    /// Action names this action is chorded with.
    #[serde(default)]
    chord: Vec<String>,
    /// Radial deadzone for axis2 actions.
    #[serde(default)]
    radial_deadzone: Option<f32>,
}

/// A binding is either a bare source string or a `{ source, scale }`
/// table.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawBinding {
    Simple(String),
    Detailed {
        source: String,
        #[serde(default = "default_scale")]
        scale: f32,
    },
}

fn default_scale() -> f32 {
    1.0
}

impl InputMap {
    /// Parse an `input_map.toml`.
    pub fn parse(toml_src: &str) -> Result<Self, toml::de::Error> {
        let raw: RawInputMap = toml::from_str(toml_src)?;
        let mut map = Self::default();
        // Deterministic id assignment: sort action names so the same
        // file always yields the same ids (tests + hot-reload rely on
        // this).
        let mut names: Vec<&String> = raw.actions.keys().collect();
        names.sort();
        // Deterministic ids start at 1 (0 is `UNRESOLVED`); the sorted
        // index + 1 gives a stable id per action.
        for (index, name) in names.into_iter().enumerate() {
            let raw_action = &raw.actions[name];
            let id = InputActionId(u32::try_from(index).unwrap_or(u32::MAX - 1) + 1);
            let bindings = raw_action
                .bindings
                .iter()
                .map(|b| match b {
                    RawBinding::Simple(source) => Binding {
                        source: crate::xr_sources::canonicalize_source(source).into_owned(),
                        scale: 1.0,
                    },
                    RawBinding::Detailed { source, scale } => Binding {
                        source: crate::xr_sources::canonicalize_source(source).into_owned(),
                        scale: *scale,
                    },
                })
                .collect();
            map.name_to_id.insert(name.clone(), id);
            map.actions.insert(
                id,
                ActionSpec {
                    name: name.clone(),
                    kind: raw_action.kind,
                    bindings,
                    modifiers: raw_action.modifiers.clone(),
                    trigger: raw_action.trigger.clone(),
                    chord_names: raw_action.chord.clone(),
                    chord: Vec::new(),
                    radial_deadzone: raw_action.radial_deadzone,
                },
            );
        }
        // Second pass: resolve chord member names to ids now that every
        // action has a stable id. Unknown names are dropped (a chord gated on
        // a non-existent action can never fire, which is the safe default).
        map.resolve_chords();
        Ok(map)
    }

    /// Resolve an action *name* to its stable id.
    /// [`InputActionId::UNRESOLVED`] when the action isn't bound.
    #[must_use]
    pub fn resolve(&self, name: &str) -> InputActionId {
        self.name_to_id
            .get(name)
            .copied()
            .unwrap_or(InputActionId::UNRESOLVED)
    }

    /// The declared spec for an action id.
    #[must_use]
    pub fn action(&self, id: InputActionId) -> Option<&ActionSpec> {
        self.actions.get(&id)
    }

    /// Every declared action spec (unordered).
    pub fn actions(&self) -> impl Iterator<Item = &ActionSpec> {
        self.actions.values()
    }

    /// Build a map directly from action specs, assigning deterministic ids
    /// (sorted by name, starting at 1) exactly as [`InputMap::parse`] does.
    ///
    /// When two specs share a name the *last* one wins — callers that need
    /// priority semantics (e.g. [`crate::mapping_context::MappingContextStack`])
    /// feed the winning spec per name. Used to flatten a mapping-context stack
    /// into a single router-consumable map.
    #[must_use]
    pub fn from_actions(specs: impl IntoIterator<Item = ActionSpec>) -> Self {
        let by_name: BTreeMap<String, ActionSpec> =
            specs.into_iter().map(|s| (s.name.clone(), s)).collect();
        let mut map = Self::default();
        for (index, (name, spec)) in by_name.into_iter().enumerate() {
            let id = InputActionId(u32::try_from(index).unwrap_or(u32::MAX - 1) + 1);
            map.name_to_id.insert(name, id);
            map.actions.insert(id, spec);
        }
        // Chord member ids from the incoming specs referred to *their*
        // original ids; re-resolve from the preserved names against the ids
        // we just assigned (flatten).
        map.resolve_chords();
        map
    }

    /// Rebuild every action's resolved [`ActionSpec::chord`] id list from its
    /// [`ActionSpec::chord_names`], against the current name→id table. Unknown
    /// names are dropped. Called after id (re)assignment.
    fn resolve_chords(&mut self) {
        let resolved: Vec<(InputActionId, Vec<InputActionId>)> = self
            .actions
            .iter()
            .map(|(id, spec)| {
                let ids = spec
                    .chord_names
                    .iter()
                    .filter_map(|n| self.name_to_id.get(n).copied())
                    .collect();
                (*id, ids)
            })
            .collect();
        for (id, ids) in resolved {
            if let Some(spec) = self.actions.get_mut(&id) {
                spec.chord = ids;
            }
        }
    }
}

/// A per-frame snapshot of resolved input state, keyed by action id.
///
/// Produced by the [`InputRouter`] from raw device state and copied into
/// host state at step 1 of the dispatch loop. Guest imports read this.
#[derive(Debug, Clone, Default)]
pub struct InputSnapshot {
    sources: HashMap<PointerSource, Box<InputSnapshot>>,
    /// Button phases this frame.
    button_phases: HashMap<InputActionId, InputPhase>,
    /// Axis scalar values this frame.
    axis_values: HashMap<InputActionId, f32>,
    /// 2D axis values this frame.
    axis2_values: HashMap<InputActionId, [f32; 2]>,
    /// Mouse delta `(dx, dy)` this frame.
    pub mouse_delta: [f32; 2],
    /// Scroll-wheel delta this frame.
    pub scroll: f32,
}

impl InputSnapshot {
    /// An explicitly identified source, independently edge-detected per hand.
    /// Missing source data is inactive, never an inferred aggregate action.
    pub fn for_source(&self, source: PointerSource) -> &Self {
        static EMPTY: std::sync::LazyLock<InputSnapshot> = std::sync::LazyLock::new(InputSnapshot::default);
        if source == PointerSource::Unspecified { self }
        else { self.sources.get(&source).map_or(&EMPTY, Box::as_ref) }
    }

    pub fn source(&self, id: InputActionId) -> PointerSource {
        let mut active = self.sources.iter().filter(|(_, s)| s.is_active(id) || s.phase(id) == InputPhase::Released).map(|(&p, _)| p);
        match (active.next(), active.next()) { (Some(p), None) => p, _ => PointerSource::Unspecified }
    }
    /// The phase of a button action this frame.
    #[must_use]
    pub fn phase(&self, id: InputActionId) -> InputPhase {
        self.button_phases.get(&id).copied().unwrap_or_default()
    }

    /// The scalar value of an axis action this frame.
    #[must_use]
    pub fn axis(&self, id: InputActionId) -> f32 {
        self.axis_values.get(&id).copied().unwrap_or(0.0)
    }

    /// The 2D value of an axis2 action this frame.
    #[must_use]
    pub fn axis2(&self, id: InputActionId) -> [f32; 2] {
        self.axis2_values.get(&id).copied().unwrap_or([0.0, 0.0])
    }

    /// Set a button phase (used by the router + tests).
    pub fn set_phase(&mut self, id: InputActionId, phase: InputPhase) {
        self.button_phases.insert(id, phase);
    }

    /// Set an axis value (used by the router + tests).
    pub fn set_axis(&mut self, id: InputActionId, value: f32) {
        self.axis_values.insert(id, value);
    }

    /// Set a 2D axis value (used by the router + tests).
    pub fn set_axis2(&mut self, id: InputActionId, value: [f32; 2]) {
        self.axis2_values.insert(id, value);
    }

    /// Whether an action is "active" this frame for chord-gating purposes
    /// A button held or pressed, or a non-zero axis / axis2 value.
    #[must_use]
    pub fn is_active(&self, id: InputActionId) -> bool {
        if matches!(self.phase(id), InputPhase::Pressed | InputPhase::Held) {
            return true;
        }
        if self.axis(id).abs() > 1e-4 {
            return true;
        }
        let a2 = self.axis2(id);
        a2[0].abs() > 1e-4 || a2[1].abs() > 1e-4
    }

    /// Button actions whose phase changed since `prev` and is not
    /// [`InputPhase::None`] this frame — the edge events delivered to the
    /// `_idtx_on_input` guest hook. Returned sorted by action id for
    /// deterministic delivery order.
    #[must_use]
    pub fn phase_changes(&self, prev: &InputSnapshot) -> Vec<(InputActionId, InputPhase)> {
        let mut out: Vec<(InputActionId, InputPhase)> = self
            .button_phases
            .iter()
            .map(|(&id, &phase)| (id, phase))
            .filter(|&(id, phase)| phase != InputPhase::None && prev.phase(id) != phase)
            .collect();
        out.sort_by_key(|(id, _)| id.0);
        out
    }
}

#[cfg(test)]
mod pointer_tests {
    use super::*;

    #[test]
    fn each_hand_has_independent_edges_and_aggregate_identity_is_ambiguous() {
        let map = InputMap::parse("[actions.click]\nkind='button'\nbindings=['Controller/MetaQuest/LeftTrigger','Controller/MetaQuest/RightTrigger']").unwrap();
        let id = map.resolve("click");
        let mut router = InputRouter::new(map);
        let mut input = FrameInput::new();
        input.press("Controller/MetaQuest/LeftTrigger");
        let first = router.snapshot(&input.raw(), 0.01);
        assert_eq!(first.source(id), PointerSource::Left);
        input.end_frame();
        input.press("Controller/MetaQuest/RightTrigger");
        let second = router.snapshot(&input.raw(), 0.01);
        assert_eq!(second.phase(id), InputPhase::Held);
        assert_eq!(second.source(id), PointerSource::Unspecified);
        assert_eq!(second.for_source(PointerSource::Left).phase(id), InputPhase::Held);
        assert_eq!(second.for_source(PointerSource::Right).phase(id), InputPhase::Pressed);
        input.end_frame();
        input.release("Controller/MetaQuest/LeftTrigger");
        let third = router.snapshot(&input.raw(), 0.01);
        assert_eq!(third.for_source(PointerSource::Left).phase(id), InputPhase::Released);
        assert_eq!(third.for_source(PointerSource::Right).phase(id), InputPhase::Held);
    }
}

/// Raw per-frame device state fed into the router.
///
/// The shell / runtime already collects device events; this is the
/// minimal shape the router consumes. Sources are keyed by their
/// `Device/Control` string (`"Key/Space"`, `"Gamepad/LeftStickX"`).
#[derive(Debug, Clone, Default)]
pub struct RawInput {
    /// Sources currently held down this frame (buttons + digital keys).
    pub active_sources: Vec<String>,
    /// Sources held down last frame — used to derive Pressed/Released.
    pub prev_active_sources: Vec<String>,
    /// Analogue source values (`Gamepad/LeftStickX` → `0.6`).
    pub analog_sources: HashMap<String, f32>,
    /// 2D vector source values (`Controller/MetaQuest/LeftThumbstick` →
    /// `[0.4, -0.9]`). Consumed by `axis2` actions the same way `Mouse/Delta`
    /// is, but unlike the per-frame mouse delta these are *level* values that
    /// persist until overwritten (a thumbstick reports its current deflection
    /// every frame, not a delta).
    pub vector2_sources: HashMap<String, [f32; 2]>,
    /// Mouse delta this frame.
    pub mouse_delta: [f32; 2],
    /// Scroll delta this frame.
    pub scroll: f32,
}

/// Turns raw device state into an [`InputSnapshot`] against an
/// [`InputMap`].
///
/// Stateful: hold/tap triggers accumulate per-action elapsed
/// hold-time across frames, so [`InputRouter::snapshot`] takes `&mut self`
/// and the frame delta-time. Swap the map with [`InputRouter::set_map`] (or
/// build a fresh router) to reset that state on a mapping-context reload.
#[derive(Debug, Default)]
pub struct InputRouter {
    source_hold_elapsed: HashMap<PointerSource, HashMap<InputActionId, f32>>,
    map: InputMap,
    /// Per-action continuous hold time (seconds) for hold/tap triggers.
    /// Present only while a triggered button is held; cleared on release.
    hold_elapsed: HashMap<InputActionId, f32>,
}

impl InputRouter {
    /// Build a router over the given map.
    #[must_use]
    pub fn new(map: InputMap) -> Self {
        Self {
            map,
            hold_elapsed: HashMap::new(),
            source_hold_elapsed: HashMap::new(),
        }
    }

    /// Borrow the underlying map (for `resolve` / diagnostics).
    #[must_use]
    pub fn map(&self) -> &InputMap {
        &self.map
    }

    /// Replace the underlying map and clear any per-action trigger state
    /// (hold/tap timers) — used on a mapping-context reload so
    /// stale hold accumulators don't leak across a map swap.
    pub fn set_map(&mut self, map: InputMap) {
        self.map = map;
        self.hold_elapsed.clear();
        self.source_hold_elapsed.clear();
    }

    /// Resolve an action name through the underlying map.
    #[must_use]
    pub fn resolve(&self, name: &str) -> InputActionId {
        self.map.resolve(name)
    }

    /// Produce a snapshot from raw device state, advancing per-action
    /// hold/tap trigger timers by `dt` seconds. Actions without a
    /// trigger and without a chord follow the plain path byte-for-byte.
    pub fn snapshot(&mut self, raw: &RawInput, dt: f32) -> InputSnapshot {
        let mut result = self.snapshot_inner(raw, dt);
        for source in PointerSource::ALL {
            let mut filtered = raw.clone();
            filtered.active_sources.retain(|s| pointer_source(s) == source);
            filtered.prev_active_sources.retain(|s| pointer_source(s) == source);
            filtered.analog_sources.retain(|s, _| pointer_source(s) == source);
            filtered.vector2_sources.retain(|s, _| pointer_source(s) == source);
            if source != PointerSource::Desktop { filtered.mouse_delta = [0.0; 2]; filtered.scroll = 0.0; }
            let mut timers = self.source_hold_elapsed.remove(&source).unwrap_or_default();
            std::mem::swap(&mut self.hold_elapsed, &mut timers);
            let snapshot = self.snapshot_inner(&filtered, dt);
            std::mem::swap(&mut self.hold_elapsed, &mut timers);
            self.source_hold_elapsed.insert(source, timers);
            result.sources.insert(source, Box::new(snapshot));
        }
        result
    }

    fn snapshot_inner(&mut self, raw: &RawInput, dt: f32) -> InputSnapshot {
        let mut snap = InputSnapshot {
            mouse_delta: raw.mouse_delta,
            scroll: raw.scroll,
            ..InputSnapshot::default()
        };

        // First pass: resolve every action's base value against raw device
        // state, applying triggers (buttons), modifiers (axes) and radial
        // deadzones (axis2).
        for spec in self.map.actions() {
            let id = self.map.resolve(&spec.name);
            match spec.kind {
                InputKind::Button => {
                    let now = spec
                        .bindings
                        .iter()
                        .any(|b| raw.active_sources.contains(&b.source));
                    let was = spec
                        .bindings
                        .iter()
                        .any(|b| raw.prev_active_sources.contains(&b.source));
                    let base = match (was, now) {
                        (false, true) => InputPhase::Pressed,
                        (true, true) => InputPhase::Held,
                        (true, false) => InputPhase::Released,
                        (false, false) => InputPhase::None,
                    };
                    let phase = match &spec.trigger {
                        None => base,
                        Some(trigger) => {
                            Self::apply_trigger(&mut self.hold_elapsed, id, trigger, was, now, dt)
                        }
                    };
                    snap.set_phase(id, phase);
                }
                InputKind::Axis => {
                    let mut value = 0.0;
                    for b in &spec.bindings {
                        value += Self::source_scalar(raw, b);
                    }
                    // Apply the modifier chain (deadzone/invert/clamp)
                    // before the safety clamp into the canonical [-1, 1] range.
                    value = apply_modifiers(value, &spec.modifiers);
                    snap.set_axis(id, value.clamp(-1.0, 1.0));
                }
                InputKind::Axis2 => {
                    // For axis2 each binding names a 2D vector source: the raw
                    // mouse delta (`Mouse/Delta`) or a stick vector source
                    // (`Controller/MetaQuest/LeftThumbstick`, …). Contributions
                    // sum; `scale` flips/attenuates per binding.
                    let mut v = [0.0f32, 0.0];
                    for b in &spec.bindings {
                        if b.source == "Mouse/Delta" {
                            v[0] += raw.mouse_delta[0] * b.scale;
                            v[1] += raw.mouse_delta[1] * b.scale;
                        } else if let Some(src) = raw.vector2_sources.get(&b.source) {
                            v[0] += src[0] * b.scale;
                            v[1] += src[1] * b.scale;
                        }
                    }
                    // Radial (2D) deadzone for normalised stick sources.
                    if let Some(zone) = spec.radial_deadzone {
                        v = apply_radial_deadzone(v, zone);
                    }
                    snap.set_axis2(id, v);
                }
            }
        }

        // Second pass: gate chorded actions. An action with a non-empty chord
        // only fires when every chord member is active this frame; otherwise
        // it is forced to its inactive value. Chord member states are read
        // from the first-pass snapshot (chords gate on base activity).
        self.apply_chords(&mut snap);

        snap
    }

    /// Advance a button's hold/tap trigger state and derive its phase.
    ///
    /// `was` / `now` are last-/this-frame held flags; `dt` is the frame delta.
    /// The per-action elapsed hold time lives in `hold_elapsed` (present only
    /// while held).
    fn apply_trigger(
        hold_elapsed: &mut HashMap<InputActionId, f32>,
        id: InputActionId,
        trigger: &Trigger,
        was: bool,
        now: bool,
        dt: f32,
    ) -> InputPhase {
        let prev = hold_elapsed.get(&id).copied().unwrap_or(0.0);
        match *trigger {
            Trigger::Hold { duration, one_shot } => {
                let duration = duration.max(1e-6);
                if now {
                    let elapsed = prev + dt;
                    hold_elapsed.insert(id, elapsed);
                    if elapsed < duration {
                        InputPhase::None
                    } else if one_shot {
                        // Fire once on the frame the threshold is first crossed.
                        if prev < duration {
                            InputPhase::Pressed
                        } else {
                            InputPhase::Held
                        }
                    } else {
                        // Repeat: re-fire each time a new `duration` boundary
                        // is crossed (autofire). Compare the floored quotients
                        // as floats to avoid a forbidden f32->int cast.
                        let crossed_prev = (prev / duration).floor();
                        let crossed_now = (elapsed / duration).floor();
                        if crossed_now > crossed_prev {
                            InputPhase::Pressed
                        } else {
                            InputPhase::Held
                        }
                    }
                } else {
                    hold_elapsed.remove(&id);
                    // Report Released only if the hold had actually triggered.
                    if prev >= duration {
                        InputPhase::Released
                    } else {
                        InputPhase::None
                    }
                }
            }
            Trigger::Tap { max_duration } => {
                if now {
                    hold_elapsed.insert(id, prev + dt);
                    // Suppress phases during the hold; a tap is only known on
                    // release.
                    InputPhase::None
                } else if was {
                    hold_elapsed.remove(&id);
                    // Released this frame: recognise a tap iff the press was
                    // short enough.
                    if prev > 0.0 && prev <= max_duration {
                        InputPhase::Pressed
                    } else {
                        InputPhase::None
                    }
                } else {
                    InputPhase::None
                }
            }
        }
    }

    /// Gate chorded actions in-place: force any action whose chord members are
    /// not all active to its inactive value.
    fn apply_chords(&self, snap: &mut InputSnapshot) {
        for spec in self.map.actions() {
            if spec.chord.is_empty() {
                continue;
            }
            let id = self.map.resolve(&spec.name);
            let satisfied = spec.chord.iter().all(|m| snap.is_active(*m));
            if !satisfied {
                match spec.kind {
                    InputKind::Button => snap.set_phase(id, InputPhase::None),
                    InputKind::Axis => snap.set_axis(id, 0.0),
                    InputKind::Axis2 => snap.set_axis2(id, [0.0, 0.0]),
                }
            }
        }
    }

    /// Resolve a single binding's contribution to an axis: an analogue
    /// source uses its analogue value, a digital source contributes its
    /// full scale when held.
    fn source_scalar(raw: &RawInput, b: &Binding) -> f32 {
        // The per-frame scroll-wheel delta is a scalar source bindable as an
        // `axis` (`Mouse/Scroll`). Unlike analogue/digital sources it is a
        // *delta*, not a level, and is cleared each frame by
        // `FrameInput::end_frame`; the Axis branch clamps the summed value into
        // `[-1, 1]`, so a bound action reads one signed "notch" per frame while
        // the raw `scroll()` accessor keeps the unclamped magnitude.
        if b.source == "Mouse/Scroll" {
            return raw.scroll * b.scale;
        }
        if let Some(v) = raw.analog_sources.get(&b.source) {
            v * b.scale
        } else if raw.active_sources.contains(&b.source) {
            b.scale
        } else {
            0.0
        }
    }
}

/// Physical source grammar, not an inference from an aggregated logical action.
fn pointer_source(source: &str) -> PointerSource {
    if source.starts_with("Key/") || source.starts_with("Mouse/") { return PointerSource::Desktop; }
    if source.starts_with("Controller/") {
        let control = source.rsplit('/').next().unwrap_or("");
        if control.starts_with("Left") { return PointerSource::Left; }
        if control.starts_with("Right") { return PointerSource::Right; }
        if matches!(control, "A" | "B") { return PointerSource::Right; }
        if matches!(control, "X" | "Y" | "Menu") { return PointerSource::Left; }
    }
    if source.starts_with("Gesture/") {
        if source.ends_with("/left") { return PointerSource::Left; }
        if source.ends_with("/right") { return PointerSource::Right; }
    }
    PointerSource::Unspecified
}

/// A stateful, per-frame input accumulator that turns discrete device
/// events into the [`RawInput`] the [`InputRouter`] consumes — deriving the
/// Pressed / Held / Released transitions by remembering last frame's held
/// set.
///
/// The frame loop drives it like so:
/// 1. report events for this frame — [`FrameInput::press`] /
///    [`FrameInput::release`] for digital sources, [`FrameInput::set_analog`]
///    for analogue axes, [`FrameInput::add_mouse_delta`] /
///    [`FrameInput::add_scroll`] for pointer deltas;
/// 2. feed [`FrameInput::raw`] into the host (`BehaviorHost::update_input`)
///    and run the frame;
/// 3. call [`FrameInput::end_frame`] to roll the held set into "previous"
///    and clear the per-frame pointer deltas.
///
/// Held digital sources persist across frames until explicitly released;
/// analogue values persist until overwritten; mouse/scroll deltas are
/// per-frame and cleared by `end_frame`.
#[derive(Debug, Clone, Default)]
pub struct FrameInput {
    /// Digital sources currently held down.
    active: BTreeSet<String>,
    /// Digital sources that were held last frame (for edge detection).
    prev: BTreeSet<String>,
    /// Analogue source values.
    analog: HashMap<String, f32>,
    /// 2D vector source values (thumbsticks). Level values that persist across
    /// frames until overwritten — distinct from the per-frame `mouse_delta`.
    vector2: HashMap<String, [f32; 2]>,
    /// Accumulated mouse delta this frame.
    mouse_delta: [f32; 2],
    /// Accumulated scroll this frame.
    scroll: f32,
}

impl FrameInput {
    /// A fresh accumulator with nothing held.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a digital source held (idempotent while held).
    pub fn press(&mut self, source: impl Into<String>) {
        self.active.insert(source.into());
    }

    /// Release a digital source.
    pub fn release(&mut self, source: &str) {
        self.active.remove(source);
    }

    /// Set an analogue source's value (e.g. `"Gamepad/LeftStickX" → 0.6`).
    pub fn set_analog(&mut self, source: impl Into<String>, value: f32) {
        self.analog.insert(source.into(), value);
    }

    /// Clear an analogue source (returns it to `0`).
    pub fn clear_analog(&mut self, source: &str) {
        self.analog.remove(source);
    }

    /// Set a 2D vector source's level this frame (e.g.
    /// `"Controller/MetaQuest/LeftThumbstick" → [0.4, -0.9]`). Persists until
    /// overwritten or cleared — a thumbstick reports its current deflection
    /// every frame, so this is a level value, not a per-frame delta.
    pub fn set_vector2(&mut self, source: impl Into<String>, value: [f32; 2]) {
        self.vector2.insert(source.into(), value);
    }

    /// Clear a 2D vector source (returns it to `[0, 0]`).
    pub fn clear_vector2(&mut self, source: &str) {
        self.vector2.remove(source);
    }

    /// Accumulate mouse delta for this frame.
    pub fn add_mouse_delta(&mut self, delta: [f32; 2]) {
        self.mouse_delta[0] += delta[0];
        self.mouse_delta[1] += delta[1];
    }

    /// Accumulate scroll for this frame.
    pub fn add_scroll(&mut self, scroll: f32) {
        self.scroll += scroll;
    }

    /// Build the [`RawInput`] for the router from the current state. Does
    /// **not** mutate — call [`FrameInput::end_frame`] after the host tick.
    #[must_use]
    pub fn raw(&self) -> RawInput {
        RawInput {
            active_sources: self.active.iter().cloned().collect(),
            prev_active_sources: self.prev.iter().cloned().collect(),
            analog_sources: self.analog.clone(),
            vector2_sources: self.vector2.clone(),
            mouse_delta: self.mouse_delta,
            scroll: self.scroll,
        }
    }

    /// Roll the held set into "previous" and clear per-frame pointer deltas.
    /// Held digital + analogue sources persist; mouse/scroll reset to zero.
    pub fn end_frame(&mut self) {
        self.prev = self.active.clone();
        self.mouse_delta = [0.0, 0.0];
        self.scroll = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"
[actions.jump]
kind = "button"
bindings = ["Key/Space", "Gamepad/South"]

[actions.move_x]
kind = "axis"
bindings = [
  { source = "Key/A", scale = -1.0 },
  { source = "Key/D", scale =  1.0 },
  { source = "Gamepad/LeftStickX" },
]

[actions.look]
kind = "axis2"
bindings = ["Mouse/Delta"]
"#;

    #[test]
    fn parses_and_resolves() {
        let map = InputMap::parse(MAP).unwrap();
        assert!(!map.resolve("jump").is_unresolved());
        assert!(!map.resolve("move_x").is_unresolved());
        assert!(map.resolve("nonexistent").is_unresolved());
        // Deterministic id assignment (sorted names: jump, look, move_x).
        assert_eq!(map.resolve("jump"), InputActionId(1));
        assert_eq!(map.resolve("look"), InputActionId(2));
        assert_eq!(map.resolve("move_x"), InputActionId(3));
    }

    #[test]
    fn quoted_widget_action_names_resolve() {
        // A dotted action name in the `widgets.*` group must be a FLAT key
        // (quoted in TOML), not a nested `[actions.widgets]` table, so the host
        // can resolve `widgets.click` by name for ABI-3 widget interaction.
        let src = r#"
            [actions."widgets.click"]
            kind = "button"
            bindings = ["Mouse/Left", "Gesture/pinch/right"]

            [actions."widgets.scroll"]
            kind = "axis"
            bindings = [{ source = "Mouse/Scroll", scale = 1.0 }]
        "#;
        let map = InputMap::parse(src).unwrap();
        assert!(!map.resolve("widgets.click").is_unresolved());
        assert!(!map.resolve("widgets.scroll").is_unresolved());
        // Not mis-parsed as a single `widgets` action.
        assert!(map.resolve("widgets").is_unresolved());
    }

    #[test]
    fn button_phase_transitions() {
        let map = InputMap::parse(MAP).unwrap();
        let mut router = InputRouter::new(map);
        let jump = router.resolve("jump");

        // Pressed this frame.
        let raw = RawInput {
            active_sources: vec!["Key/Space".to_owned()],
            prev_active_sources: vec![],
            ..RawInput::default()
        };
        assert_eq!(router.snapshot(&raw, 0.0).phase(jump), InputPhase::Pressed);

        // Held.
        let raw = RawInput {
            active_sources: vec!["Key/Space".to_owned()],
            prev_active_sources: vec!["Key/Space".to_owned()],
            ..RawInput::default()
        };
        assert_eq!(router.snapshot(&raw, 0.0).phase(jump), InputPhase::Held);

        // Released.
        let raw = RawInput {
            active_sources: vec![],
            prev_active_sources: vec!["Key/Space".to_owned()],
            ..RawInput::default()
        };
        assert_eq!(router.snapshot(&raw, 0.0).phase(jump), InputPhase::Released);
    }

    #[test]
    fn axis_sums_scaled_bindings() {
        let map = InputMap::parse(MAP).unwrap();
        let mut router = InputRouter::new(map);
        let move_x = router.resolve("move_x");

        // D held → +1.
        let raw = RawInput {
            active_sources: vec!["Key/D".to_owned()],
            ..RawInput::default()
        };
        assert!((router.snapshot(&raw, 0.0).axis(move_x) - 1.0).abs() < 1e-6);

        // A held → -1.
        let raw = RawInput {
            active_sources: vec!["Key/A".to_owned()],
            ..RawInput::default()
        };
        assert!((router.snapshot(&raw, 0.0).axis(move_x) + 1.0).abs() < 1e-6);

        // Analogue stick 0.5.
        let mut analog = HashMap::new();
        analog.insert("Gamepad/LeftStickX".to_owned(), 0.5);
        let raw = RawInput {
            analog_sources: analog,
            ..RawInput::default()
        };
        assert!((router.snapshot(&raw, 0.0).axis(move_x) - 0.5).abs() < 1e-6);
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact values are set then read back verbatim
    fn axis2_maps_mouse_delta() {
        let map = InputMap::parse(MAP).unwrap();
        let mut router = InputRouter::new(map);
        let look = router.resolve("look");
        let raw = RawInput {
            mouse_delta: [3.0, -2.0],
            ..RawInput::default()
        };
        let snap = router.snapshot(&raw, 0.0);
        assert_eq!(snap.axis2(look), [3.0, -2.0]);
        assert_eq!(snap.mouse_delta, [3.0, -2.0]);
    }

    #[test]
    fn mouse_button_binds_as_button_action() {
        // A `Mouse/*` button source routes through the generic held-source
        // path exactly like a keyboard key or controller button.
        let src = r#"
[actions.fire]
kind = "button"
bindings = ["Mouse/Left"]
"#;
        let mut router = InputRouter::new(InputMap::parse(src).unwrap());
        let fire = router.resolve("fire");

        let mut fi = FrameInput::new();
        fi.press("Mouse/Left");
        assert_eq!(router.snapshot(&fi.raw(), 0.0).phase(fire), InputPhase::Pressed);
        fi.end_frame();
        assert_eq!(router.snapshot(&fi.raw(), 0.0).phase(fire), InputPhase::Held);
        fi.end_frame();
        fi.release("Mouse/Left");
        assert_eq!(router.snapshot(&fi.raw(), 0.0).phase(fire), InputPhase::Released);
    }

    #[test]
    fn scroll_binds_as_axis_action() {
        // `Mouse/Scroll` is a per-frame scalar delta bindable as an `axis`;
        // `scale` applies and the summed value is clamped into `[-1, 1]`.
        let src = r#"
[actions.zoom]
kind = "axis"
bindings = [{ source = "Mouse/Scroll", scale = -1.0 }]
"#;
        let mut router = InputRouter::new(InputMap::parse(src).unwrap());
        let zoom = router.resolve("zoom");

        // One notch up, inverted by scale → -1.
        let raw = RawInput {
            scroll: 1.0,
            ..RawInput::default()
        };
        let snap = router.snapshot(&raw, 0.0);
        assert!((snap.axis(zoom) + 1.0).abs() < 1e-6);
        // The raw accessor keeps the unclamped magnitude in parallel.
        assert!((snap.scroll - 1.0).abs() < 1e-6);

        // A fast multi-notch scroll clamps to the axis range (raw unaffected).
        let raw = RawInput {
            scroll: 5.0,
            ..RawInput::default()
        };
        assert!((router.snapshot(&raw, 0.0).axis(zoom) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn frame_input_clears_scroll_each_frame() {
        let src = r#"
[actions.zoom]
kind = "axis"
bindings = ["Mouse/Scroll"]
"#;
        let mut router = InputRouter::new(InputMap::parse(src).unwrap());
        let zoom = router.resolve("zoom");

        let mut fi = FrameInput::new();
        fi.add_scroll(1.0);
        assert!((router.snapshot(&fi.raw(), 0.0).axis(zoom) - 1.0).abs() < 1e-6);

        // Scroll is a per-frame delta: `end_frame` clears it.
        fi.end_frame();
        assert!(router.snapshot(&fi.raw(), 0.0).axis(zoom).abs() < 1e-6);
    }

    #[test]
    fn frame_input_derives_button_edges() {
        let map = InputMap::parse(MAP).unwrap();
        let mut router = InputRouter::new(map);
        let jump = router.resolve("jump");

        let mut fi = FrameInput::new();

        // Frame 1: press → Pressed.
        fi.press("Key/Space");
        assert_eq!(
            router.snapshot(&fi.raw(), 0.0).phase(jump),
            InputPhase::Pressed
        );
        fi.end_frame();

        // Frame 2: still held (no new event) → Held.
        assert_eq!(
            router.snapshot(&fi.raw(), 0.0).phase(jump),
            InputPhase::Held
        );
        fi.end_frame();

        // Frame 3: release → Released.
        fi.release("Key/Space");
        assert_eq!(
            router.snapshot(&fi.raw(), 0.0).phase(jump),
            InputPhase::Released
        );
        fi.end_frame();

        // Frame 4: nothing held → None.
        assert_eq!(
            router.snapshot(&fi.raw(), 0.0).phase(jump),
            InputPhase::None
        );
    }

    #[test]
    #[allow(clippy::float_cmp)] // axis2 values are set then read back verbatim
    fn frame_input_analog_persists_mouse_clears() {
        let map = InputMap::parse(MAP).unwrap();
        let mut router = InputRouter::new(map);
        let move_x = router.resolve("move_x");
        let look = router.resolve("look");

        let mut fi = FrameInput::new();
        fi.set_analog("Gamepad/LeftStickX", 0.5);
        fi.add_mouse_delta([2.0, -1.0]);

        let snap = router.snapshot(&fi.raw(), 0.0);
        assert!((snap.axis(move_x) - 0.5).abs() < 1e-6);
        assert_eq!(snap.axis2(look), [2.0, -1.0]);

        // After end_frame: analogue persists, mouse delta cleared.
        fi.end_frame();
        let snap = router.snapshot(&fi.raw(), 0.0);
        assert!((snap.axis(move_x) - 0.5).abs() < 1e-6, "analogue persists");
        assert_eq!(snap.axis2(look), [0.0, 0.0], "mouse delta cleared");

        // Clearing the analogue returns it to zero.
        fi.clear_analog("Gamepad/LeftStickX");
        assert!(router.snapshot(&fi.raw(), 0.0).axis(move_x).abs() < 1e-6);
    }

    // ── modifiers ──────────────────────────────────────────────────────

    #[test]
    fn deadzone_collapses_small_and_rescales_remainder() {
        let dz = Modifier::Deadzone { zone: 0.2 };
        // Inside the zone → 0.
        assert!(dz.apply(0.1).abs() < 1e-6);
        assert!(dz.apply(-0.2).abs() < 1e-6);
        // Full deflection still reaches ±1 (no dead travel at the top).
        assert!((dz.apply(1.0) - 1.0).abs() < 1e-6);
        assert!((dz.apply(-1.0) + 1.0).abs() < 1e-6);
        // Midpoint rescales: (0.6 - 0.2) / (1 - 0.2) = 0.5.
        assert!((dz.apply(0.6) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn deadzone_zone_at_one_does_not_divide_by_zero() {
        // A zone >= 1 is clamped below 1; any finite input stays finite.
        let dz = Modifier::Deadzone { zone: 1.0 };
        assert!(dz.apply(0.5).is_finite());
        assert!(dz.apply(1.0).is_finite());
    }

    #[test]
    fn invert_and_clamp_apply() {
        assert!((Modifier::Invert.apply(0.3) + 0.3).abs() < 1e-6);
        let clamp = Modifier::Clamp {
            min: -0.5,
            max: 0.5,
        };
        assert!((clamp.apply(0.9) - 0.5).abs() < 1e-6);
        assert!((clamp.apply(-0.9) + 0.5).abs() < 1e-6);
        assert!((clamp.apply(0.1) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn modifier_chain_applies_left_to_right() {
        // Deadzone(0.2) then Invert: 0.6 → 0.5 → -0.5.
        let chain = [Modifier::Deadzone { zone: 0.2 }, Modifier::Invert];
        assert!((apply_modifiers(0.6, &chain) + 0.5).abs() < 1e-6);
        // Empty chain is the identity.
        assert!((apply_modifiers(0.42, &[]) - 0.42).abs() < 1e-6);
    }

    #[test]
    fn parsed_axis_modifiers_flow_through_the_router() {
        let src = r#"
[actions.throttle]
kind = "axis"
bindings = [{ source = "Gamepad/RightTrigger" }]
modifiers = [{ type = "deadzone", zone = 0.25 }]
"#;
        let map = InputMap::parse(src).unwrap();
        let mut router = InputRouter::new(map);
        let throttle = router.resolve("throttle");

        // 0.1 is inside the deadzone → 0.
        let mut analog = HashMap::new();
        analog.insert("Gamepad/RightTrigger".to_owned(), 0.1);
        let raw = RawInput {
            analog_sources: analog,
            ..RawInput::default()
        };
        assert!(router.snapshot(&raw, 0.0).axis(throttle).abs() < 1e-6);

        // 1.0 still reaches full range through the deadzone rescale.
        let mut analog = HashMap::new();
        analog.insert("Gamepad/RightTrigger".to_owned(), 1.0);
        let raw = RawInput {
            analog_sources: analog,
            ..RawInput::default()
        };
        assert!((router.snapshot(&raw, 0.0).axis(throttle) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn actions_without_modifiers_are_unchanged() {
        // The stock MAP declares no modifiers; the field defaults to empty and
        // axis behaviour matches the plain path.
        let map = InputMap::parse(MAP).unwrap();
        let spec = map.action(map.resolve("move_x")).unwrap();
        assert!(spec.modifiers.is_empty());
    }

    // ── triggers (hold / tap) ───────────────────────────────────────────

    const HOLD_MAP: &str = r#"
[actions.charge]
kind = "button"
bindings = ["Key/Space"]
trigger = { type = "hold", duration = 0.5, one_shot = true }
"#;

    fn held(source: &str) -> RawInput {
        RawInput {
            active_sources: vec![source.to_owned()],
            prev_active_sources: vec![source.to_owned()],
            ..RawInput::default()
        }
    }

    fn pressed(source: &str) -> RawInput {
        RawInput {
            active_sources: vec![source.to_owned()],
            prev_active_sources: vec![],
            ..RawInput::default()
        }
    }

    fn released(source: &str) -> RawInput {
        RawInput {
            active_sources: vec![],
            prev_active_sources: vec![source.to_owned()],
            ..RawInput::default()
        }
    }

    #[test]
    fn hold_one_shot_fires_once_after_threshold() {
        let map = InputMap::parse(HOLD_MAP).unwrap();
        let mut router = InputRouter::new(map);
        let charge = router.resolve("charge");

        // Press then hold below the 0.5s threshold → suppressed (None).
        assert_eq!(
            router.snapshot(&pressed("Key/Space"), 0.2).phase(charge),
            InputPhase::None
        );
        assert_eq!(
            router.snapshot(&held("Key/Space"), 0.2).phase(charge),
            InputPhase::None
        );
        // Crossing 0.5s (0.4 + 0.2 = 0.6) → Pressed exactly once.
        assert_eq!(
            router.snapshot(&held("Key/Space"), 0.2).phase(charge),
            InputPhase::Pressed
        );
        // Still held → Held (one_shot does not re-fire).
        assert_eq!(
            router.snapshot(&held("Key/Space"), 0.2).phase(charge),
            InputPhase::Held
        );
        // Release after triggering → Released.
        assert_eq!(
            router.snapshot(&released("Key/Space"), 0.2).phase(charge),
            InputPhase::Released
        );
    }

    #[test]
    fn hold_repeat_refires_each_interval() {
        let src = r#"
[actions.autofire]
kind = "button"
bindings = ["Key/F"]
trigger = { type = "hold", duration = 0.5 }
"#;
        let map = InputMap::parse(src).unwrap();
        let mut router = InputRouter::new(map);
        let af = router.resolve("autofire");

        assert_eq!(
            router.snapshot(&pressed("Key/F"), 0.5).phase(af),
            InputPhase::Pressed,
            "first threshold"
        );
        assert_eq!(
            router.snapshot(&held("Key/F"), 0.2).phase(af),
            InputPhase::Held,
            "between intervals"
        );
        assert_eq!(
            router.snapshot(&held("Key/F"), 0.4).phase(af),
            InputPhase::Pressed,
            "second threshold (0.5 + 0.6 = 1.1 crosses 1.0)"
        );
    }

    #[test]
    fn hold_released_before_threshold_never_fires() {
        let map = InputMap::parse(HOLD_MAP).unwrap();
        let mut router = InputRouter::new(map);
        let charge = router.resolve("charge");
        assert_eq!(
            router.snapshot(&pressed("Key/Space"), 0.2).phase(charge),
            InputPhase::None
        );
        // Released before 0.5s → no Released event (it never triggered).
        assert_eq!(
            router.snapshot(&released("Key/Space"), 0.2).phase(charge),
            InputPhase::None
        );
    }

    #[test]
    fn tap_fires_on_quick_release_only() {
        let src = r#"
[actions.tap]
kind = "button"
bindings = ["Key/E"]
trigger = { type = "tap", max_duration = 0.25 }
"#;
        let map = InputMap::parse(src).unwrap();
        let mut router = InputRouter::new(map);
        let tap = router.resolve("tap");

        // Quick tap: press (0.1s) then release → Pressed on release.
        assert_eq!(
            router.snapshot(&pressed("Key/E"), 0.1).phase(tap),
            InputPhase::None,
            "suppressed during hold"
        );
        assert_eq!(
            router.snapshot(&released("Key/E"), 0.1).phase(tap),
            InputPhase::Pressed,
            "tap recognised on quick release"
        );

        // Long press then release → no tap.
        assert_eq!(
            router.snapshot(&pressed("Key/E"), 0.3).phase(tap),
            InputPhase::None
        );
        assert_eq!(
            router.snapshot(&held("Key/E"), 0.3).phase(tap),
            InputPhase::None
        );
        assert_eq!(
            router.snapshot(&released("Key/E"), 0.1).phase(tap),
            InputPhase::None,
            "held too long → not a tap"
        );
    }

    #[test]
    fn set_map_clears_hold_timers() {
        let map = InputMap::parse(HOLD_MAP).unwrap();
        let mut router = InputRouter::new(map);
        let charge = router.resolve("charge");
        // Accumulate almost to the threshold.
        router.snapshot(&pressed("Key/Space"), 0.4);
        // Swap in a fresh copy of the same map — timers reset.
        router.set_map(InputMap::parse(HOLD_MAP).unwrap());
        // The next held frame starts from zero, so 0.2s is still below 0.5s.
        assert_eq!(
            router.snapshot(&held("Key/Space"), 0.2).phase(charge),
            InputPhase::None
        );
    }

    // ── chords ──────────────────────────────────────────────────────────

    #[test]
    fn chord_gates_action_until_all_members_active() {
        let src = r#"
[actions.sprint]
kind = "button"
bindings = ["Key/Shift"]

[actions.super_jump]
kind = "button"
bindings = ["Key/Space"]
chord = ["sprint"]
"#;
        let map = InputMap::parse(src).unwrap();
        let mut router = InputRouter::new(map);
        let super_jump = router.resolve("super_jump");

        // Space alone (sprint not held) → gated to None.
        let raw = RawInput {
            active_sources: vec!["Key/Space".to_owned()],
            ..RawInput::default()
        };
        assert_eq!(
            router.snapshot(&raw, 0.0).phase(super_jump),
            InputPhase::None
        );

        // Space + Shift → chord satisfied → Pressed.
        let raw = RawInput {
            active_sources: vec!["Key/Space".to_owned(), "Key/Shift".to_owned()],
            ..RawInput::default()
        };
        assert_eq!(
            router.snapshot(&raw, 0.0).phase(super_jump),
            InputPhase::Pressed
        );
    }

    #[test]
    fn chord_member_names_resolve_to_ids() {
        let src = r#"
[actions.a]
kind = "button"
bindings = ["Key/A"]

[actions.combo]
kind = "button"
bindings = ["Key/B"]
chord = ["a"]
"#;
        let map = InputMap::parse(src).unwrap();
        let combo = map.action(map.resolve("combo")).unwrap();
        assert_eq!(combo.chord, vec![map.resolve("a")]);
        // Unknown chord names are dropped.
        assert!(!combo.chord.contains(&InputActionId::UNRESOLVED));
    }

    // ── radial deadzone (axis2) ─────────────────────────────────────────

    #[test]
    fn radial_deadzone_collapses_small_and_rescales() {
        // Inside the zone → origin.
        let inside = apply_radial_deadzone([0.1, 0.0], 0.2);
        assert!(inside[0].abs() < 1e-6 && inside[1].abs() < 1e-6);
        // Full deflection along one axis still reaches magnitude 1.
        let out = apply_radial_deadzone([1.0, 0.0], 0.2);
        assert!((out[0] - 1.0).abs() < 1e-6 && out[1].abs() < 1e-6);
        // Direction is preserved; magnitude is rescaled.
        let out = apply_radial_deadzone([0.6, 0.0], 0.2);
        assert!((out[0] - 0.5).abs() < 1e-6, "0.6 → (0.6-0.2)/(1-0.2)=0.5");
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn axis2_without_radial_deadzone_is_unchanged() {
        // The stock MAP's `look` declares no radial deadzone → raw passthrough.
        let map = InputMap::parse(MAP).unwrap();
        let mut router = InputRouter::new(map);
        let look = router.resolve("look");
        let raw = RawInput {
            mouse_delta: [3.0, -2.0],
            ..RawInput::default()
        };
        assert_eq!(router.snapshot(&raw, 0.016).axis2(look), [3.0, -2.0]);
    }
}
