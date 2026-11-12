//! Swap-spanning experience state.
//!
//! A level transition drops the outgoing [`StudioSession`] wholesale — and with
//! it that session's `BehaviorHost`, so **behavior `Store` (wasmtime) state does
//! NOT survive a transition**. [`GlobalState`] is the carry vehicle for the state
//! that *must* outlive a swap: the player placement override and a small opaque
//! key/value blob (score, inventory, chosen branch, RNG seed — whatever the
//! experience needs to keep across levels).
//!
//! **Ownership.** `GlobalState` lives on the [`crate::TransitionDriver`], the one
//! object that spans a level swap (it persists for the whole run while sessions
//! are dropped and replaced via [`crate::SessionSet::set_current`]). The spec
//! (`docs/level-transition-runtime-plan.md` §2) names `RuntimePlayer` as the
//! owner; the driver is the loop-agnostic equivalent both the headless
//! (`idtx_runtime_core::run_headless`) and windowed (`player.rs`) loops already hold, so
//! the blob persists across a swap for free — it lives on the driver, not the
//! session.
//!
//! **Authoring rule (§2).** Because behavior memory is per-session, a behavior
//! that must persist state across a transition, or decide *when* to transition,
//! belongs on the **shared player sublayer** (re-attached every level) rather
//! than a per-level-only `Store`. `Ctx::level().read_state/set_state` access the
//! bounded shared blob. Incoming initialization runs against a fork; failed or
//! cancelled loads never publish its writes.
//!
//! The [`player_placement`](GlobalState::player_placement) override is a
//! **one-shot**: the driver consumes and clears it at the next cutover (see
//! [`crate::TransitionDriver::poll`]). Absent an override, the target level's
//! `/World/level/Spawn` is used instead.
//!
//! [`StudioSession`]: idtx_studio_session::StudioSession

use idtx_behavior::ExperienceState;

/// Experience state that survives a level swap.
///
/// A bounded shared string→bytes blob plus an optional player
/// placement override (a world transform, USD row-major `m[row][col]`). Owned by
/// the [`crate::TransitionDriver`] so it outlives every session drop. Clones share
/// the blob; incoming initialization uses an isolated, conflict-checked fork.
#[derive(Default, Debug, Clone)]
pub struct GlobalState {
    /// Opaque experience-defined key/value state (score, inventory, seed, …).
    /// Ordered for deterministic iteration/serialization.
    blob: ExperienceState,
    /// A one-shot player placement override consumed at the next cutover. `Some`
    /// → the driver places `/World/Player` at this world transform and clears it;
    /// `None` → the target level's `/World/level/Spawn` is used.
    player_placement: Option<[[f32; 4]; 4]>,
}

impl GlobalState {
    /// An empty carry (no blob entries, no placement override).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a blob value by key, or `None` when unset.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.blob.get(key)
    }

    /// Set (insert or overwrite) a blob value.
    pub fn set(&mut self, key: impl Into<String>, value: Vec<u8>) -> bool {
        self.blob.set(key.into(), value).is_ok()
    }

    #[must_use]
    pub fn shared(&self) -> ExperienceState { self.blob.clone() }

    pub fn bind(&mut self, state: ExperienceState) { self.blob = state; }

    /// Remove a blob value, returning the previous bytes if present.
    pub fn remove(&mut self, key: &str) -> Option<Vec<u8>> {
        self.blob.remove(key)
    }

    /// Number of blob entries.
    #[must_use]
    pub fn blob_len(&self) -> usize {
        self.blob.len()
    }

    /// `true` when there is no carried state at all — no blob entries **and** no
    /// placement override.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blob.is_empty() && self.player_placement.is_none()
    }

    /// The pending player placement override (a world transform, USD row-major),
    /// or `None` when the next cutover should use the target level's spawn.
    #[must_use]
    pub fn player_placement(&self) -> Option<[[f32; 4]; 4]> {
        self.player_placement
    }

    /// Set the player placement override consumed at the next cutover (overrides
    /// the target level's `/World/level/Spawn`, one-shot).
    pub fn set_player_placement(&mut self, matrix: [[f32; 4]; 4]) {
        self.player_placement = Some(matrix);
    }

    /// Clear the player placement override (the driver calls this after consuming
    /// it at cutover, so it never leaks into a later transition).
    pub fn clear_player_placement(&mut self) {
        self.player_placement = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carried_state_has_a_finite_storage_budget() {
        let mut state = GlobalState::new();
        for index in 0..200 { state.set(index.to_string(), vec![1; 4096]); }
        let bytes: usize = (0..200).filter_map(|index| state.get(&index.to_string())).map(|value| value.len()).sum();
        assert!(bytes <= 64 * 1024, "unbounded carried state: {bytes}");
    }

    #[test]
    fn blob_set_get_remove_round_trips() {
        let mut g = GlobalState::new();
        assert!(g.is_empty());
        assert_eq!(g.blob_len(), 0);
        assert_eq!(g.get("score"), None);

        g.set("score", vec![42]);
        g.set("branch".to_owned(), b"north".to_vec());
        assert_eq!(g.blob_len(), 2);
        assert!(!g.is_empty());
        assert_eq!(g.get("score").as_deref(), Some(&[42][..]));
        assert_eq!(g.get("branch").as_deref(), Some(&b"north"[..]));

        // Overwrite in place.
        g.set("score", vec![7, 7]);
        assert_eq!(g.get("score").as_deref(), Some(&[7, 7][..]));
        assert_eq!(g.blob_len(), 2);

        assert_eq!(g.remove("score"), Some(vec![7, 7]));
        assert_eq!(g.get("score"), None);
        assert_eq!(g.remove("score"), None);
        assert_eq!(g.blob_len(), 1);
    }

    #[test]
    fn player_placement_set_and_clear() {
        let mut g = GlobalState::new();
        assert_eq!(g.player_placement(), None);
        // A blob entry alone leaves placement empty; placement alone leaves blob
        // empty — `is_empty` tracks both.
        assert!(g.is_empty());

        let m = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [5.0, 2.0, -3.0, 1.0],
        ];
        g.set_player_placement(m);
        assert_eq!(g.player_placement(), Some(m));
        assert!(!g.is_empty(), "a placement override alone is not empty");

        g.clear_player_placement();
        assert_eq!(g.player_placement(), None);
        assert!(g.is_empty());
    }
}
