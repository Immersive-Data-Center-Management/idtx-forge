//! Prioritized, consuming input mapping-context stack (§7).
//!
//! Mirrors Unreal's Input Mapping Context (IMC) stack: several named
//! [`InputMap`]s are layered by integer priority, and the **highest-priority
//! context that binds an action name consumes it** — lower-priority contexts
//! never see that action. A gameplay context can thus be shadowed by a
//! transient "menu" or "vehicle" context pushed on top, without either side
//! knowing about the other.
//!
//! [`MappingContextStack::effective_map`] flattens the active stack into a
//! single [`InputMap`] the existing [`crate::input::InputRouter`] consumes
//! unchanged — so this layer is purely additive. The runtime add/remove API is
//! here; it is not yet wired into the session hot-reload path or the editor UI.

use std::collections::BTreeMap;

use crate::input::{ActionSpec, InputMap};

/// One named mapping context: an [`InputMap`] plus its stack priority.
#[derive(Debug, Clone)]
pub struct MappingContext {
    /// Identifier used by the runtime add/remove API.
    pub name: String,
    /// Higher priority shadows (consumes actions from) lower priority.
    pub priority: i32,
    /// The action → bindings mapping this context contributes.
    pub map: InputMap,
}

/// A priority-ordered set of [`MappingContext`]s with consuming semantics.
///
/// Kept sorted by descending priority; [`MappingContextStack::add`] and
/// [`MappingContextStack::remove`] are the runtime API. Equal priority ties
/// are broken by insertion order (the earlier-added context wins the consume),
/// which keeps flattening deterministic.
#[derive(Debug, Default, Clone)]
pub struct MappingContextStack {
    /// Invariant: sorted by `priority` descending (stable — equal priorities
    /// keep insertion order).
    contexts: Vec<MappingContext>,
}

impl MappingContextStack {
    /// An empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a context, or replace an existing one of the same `name`, keeping
    /// the stack ordered by descending priority. Returns `true` when an
    /// existing context of that name was replaced.
    pub fn add(&mut self, name: impl Into<String>, priority: i32, map: InputMap) -> bool {
        let name = name.into();
        let replaced = self.remove(&name);
        self.contexts.push(MappingContext {
            name,
            priority,
            map,
        });
        // Stable sort: equal priorities preserve insertion order, so the
        // earlier-added of two equal-priority contexts stays first (and wins
        // the consume in `effective_map`).
        self.contexts.sort_by_key(|c| std::cmp::Reverse(c.priority));
        replaced
    }

    /// Remove the context named `name`. Returns `true` when one was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.contexts.len();
        self.contexts.retain(|c| c.name != name);
        self.contexts.len() != before
    }

    /// Drop every context.
    pub fn clear(&mut self) {
        self.contexts.clear();
    }

    /// `true` when no context is active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }

    /// Number of active contexts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.contexts.len()
    }

    /// The active contexts, highest priority first.
    pub fn contexts(&self) -> impl Iterator<Item = &MappingContext> {
        self.contexts.iter()
    }

    /// Whether any active context binds `name`.
    #[must_use]
    pub fn binds(&self, name: &str) -> bool {
        self.contexts
            .iter()
            .any(|c| !c.map.resolve(name).is_unresolved())
    }

    /// Flatten the active stack into a single [`InputMap`] with consuming
    /// shadowing: for each action name, the binding set from the
    /// highest-priority context that declares it wins; lower-priority contexts
    /// never contribute that action. The result carries freshly-assigned
    /// deterministic ids (see [`InputMap::from_actions`]), so callers must
    /// re-resolve action names against the flattened map (the per-context ids
    /// do not survive the flatten).
    #[must_use]
    pub fn effective_map(&self) -> InputMap {
        // `contexts` is highest-priority-first; the first context to bind a
        // name wins (and consumes it for the rest).
        let mut winners: BTreeMap<String, ActionSpec> = BTreeMap::new();
        for ctx in &self.contexts {
            for spec in ctx.map.actions() {
                winners
                    .entry(spec.name.clone())
                    .or_insert_with(|| spec.clone());
            }
        }
        InputMap::from_actions(winners.into_values())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAMEPLAY: &str = r#"
[actions.jump]
kind = "button"
bindings = ["Key/Space"]

[actions.move_x]
kind = "axis"
bindings = [{ source = "Key/D", scale = 1.0 }]
"#;

    // A menu context that rebinds `jump` (to a different source) and adds
    // `pause`. When layered above gameplay it must consume `jump`.
    const MENU: &str = r#"
[actions.jump]
kind = "button"
bindings = ["Gamepad/South"]

[actions.pause]
kind = "button"
bindings = ["Key/Escape"]
"#;

    fn map(src: &str) -> InputMap {
        InputMap::parse(src).unwrap()
    }

    #[test]
    fn empty_stack_binds_nothing() {
        let stack = MappingContextStack::new();
        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);
        assert!(!stack.binds("jump"));
        assert_eq!(stack.effective_map().actions().count(), 0);
    }

    #[test]
    fn disjoint_contexts_merge() {
        let mut stack = MappingContextStack::new();
        stack.add("gameplay", 0, map(GAMEPLAY));
        stack.add("menu", 10, map(MENU));

        let eff = stack.effective_map();
        // Union of action names: jump, move_x, pause.
        assert_eq!(eff.actions().count(), 3);
        assert!(!eff.resolve("jump").is_unresolved());
        assert!(!eff.resolve("move_x").is_unresolved());
        assert!(!eff.resolve("pause").is_unresolved());
    }

    #[test]
    fn higher_priority_consumes_shared_action() {
        let mut stack = MappingContextStack::new();
        stack.add("gameplay", 0, map(GAMEPLAY));
        stack.add("menu", 10, map(MENU)); // higher priority

        let eff = stack.effective_map();
        let jump = eff.resolve("jump");
        let spec = eff.action(jump).expect("jump bound");
        // The menu context (priority 10) consumes `jump`: its single binding
        // is `Gamepad/South`, not gameplay's `Key/Space`.
        assert_eq!(spec.bindings.len(), 1);
        assert_eq!(spec.bindings[0].source, "Gamepad/South");
    }

    #[test]
    fn removing_top_context_restores_lower_binding() {
        let mut stack = MappingContextStack::new();
        stack.add("gameplay", 0, map(GAMEPLAY));
        stack.add("menu", 10, map(MENU));

        assert!(stack.remove("menu"));
        let eff = stack.effective_map();
        // `pause` is gone; `jump` reverts to gameplay's `Key/Space`.
        assert!(eff.resolve("pause").is_unresolved());
        let jump = eff.resolve("jump");
        assert_eq!(eff.action(jump).unwrap().bindings[0].source, "Key/Space");
    }

    #[test]
    fn add_replaces_context_by_name_and_reorders_by_priority() {
        let mut stack = MappingContextStack::new();
        assert!(!stack.add("menu", 5, map(MENU)));
        // Re-adding the same name replaces it (and returns true).
        assert!(stack.add("menu", 20, map(MENU)));
        assert_eq!(stack.len(), 1);

        stack.add("gameplay", 0, map(GAMEPLAY));
        // Highest priority first.
        let names: Vec<&str> = stack.contexts().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["menu", "gameplay"]);
    }

    #[test]
    fn equal_priority_earlier_added_wins_consume() {
        // Two equal-priority contexts both binding `jump`; the earlier-added
        // one wins the consume (deterministic tie-break).
        let mut stack = MappingContextStack::new();
        stack.add("first", 5, map(GAMEPLAY)); // jump = Key/Space
        stack.add("second", 5, map(MENU)); // jump = Gamepad/South

        let eff = stack.effective_map();
        let jump = eff.resolve("jump");
        assert_eq!(eff.action(jump).unwrap().bindings[0].source, "Key/Space");
    }

    #[test]
    fn clear_empties_the_stack() {
        let mut stack = MappingContextStack::new();
        stack.add("gameplay", 0, map(GAMEPLAY));
        stack.add("menu", 10, map(MENU));
        stack.clear();
        assert!(stack.is_empty());
        assert_eq!(stack.effective_map().actions().count(), 0);
    }
}
