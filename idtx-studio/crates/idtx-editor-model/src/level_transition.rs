//! Level-transition state machine (§ Appendix A).
//!
//! Drives the assembly-layer payload swap between two level holder prims under
//! `/World/Levels/<Name>` (see the roadmap's assembly-stage convention). The
//! manager is a **pure state machine**: it emits the ordered, abstract
//! [`TransitionStep`]s the session must perform (`set_payload_loaded` /
//! `author_session_visibility`) but performs none of them itself, so it stays
//! free of any USD or session dependency and is unit-tested headlessly. The
//! shell/session drives it exactly as it drives [`crate::EditorCommand`]:
//! call [`LevelTransition::advance`], perform the returned steps against the
//! live stage, repeat until [`LevelTransition::is_done`].
//!
//! The canonical sequence (roadmap Appendix A, "Transition sequence"):
//! 1. **Loading** — load the next level's payload (still invisible), and show
//!    the transfer/loading level to mask the warm-up.
//! 2. **Active** — cut to the new level: show it, hide the transfer level, and
//!    hide the previous level.
//! 3. **Unloading** — unload the previous level's payload to reclaim memory.
//!
//! Visibility is authored into the *session layer*, and payloads are
//! loaded/unloaded via `Prim::Load()/Unload()`; prim *activation* is avoided
//! because it is read-only through the bridge today (see the roadmap's "Why
//! visibility + payload-load, not activation").

/// Stable identity of a level holder prim, e.g. `/World/Levels/Forest`. The
/// prim *name* is API (save/load and the manager key on it), not a display
/// string — display names live in `customLayerData.displayName`.
pub type LevelPath = String;

/// The phase of an in-flight level transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionPhase {
    /// Begun but no steps emitted yet.
    Pending,
    /// The next level's payload is composing (still invisible); the transfer
    /// level is shown.
    Loading,
    /// The new level is visible and active; the previous level is still
    /// resident (payload not yet reclaimed).
    Active,
    /// The previous level's payload is being unloaded to reclaim memory.
    Unloading,
    /// The transition is complete.
    Done,
}

/// One concrete side-effecting step the session must perform to advance a
/// transition. Kept abstract (path strings, no USD types) so the state machine
/// is USD-free and headlessly testable; the session maps each variant onto the
/// mechanic named in its doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionStep {
    /// `set_payload_loaded(path, true)` — compose the level's contents (the
    /// level stays invisible, so there is no frame hitch on visibility).
    LoadPayload(LevelPath),
    /// `author_session_visibility(path, visible)` — session-layer
    /// `inherited`/`invisible` toggle (never dirties the saved assembly).
    SetVisible {
        /// The level holder prim path.
        path: LevelPath,
        /// `true` = `inherited` (shown), `false` = `invisible` (hidden).
        visible: bool,
    },
    /// `set_payload_loaded(path, false)` — unload the payload to free memory.
    UnloadPayload(LevelPath),
}

/// A single level-to-level transition, advanced phase by phase.
///
/// Construct with [`LevelTransition::begin`], then repeatedly call
/// [`LevelTransition::advance`] — each call moves to the next phase and returns
/// the steps to perform — until [`LevelTransition::is_done`].
#[derive(Debug, Clone)]
pub struct LevelTransition {
    /// The currently-active level being transitioned away from. `None` on the
    /// first load (nothing to hide or unload).
    from: Option<LevelPath>,
    /// The level being transitioned to (activated).
    to: LevelPath,
    /// Optional transfer/loading level shown while the target warms up. `None`
    /// = cut directly with no masking level.
    transfer: Option<LevelPath>,
    /// Current phase.
    phase: TransitionPhase,
}

impl LevelTransition {
    /// Begin a transition from `from` (None on first load) to `to`, optionally
    /// masking the swap with a `transfer` level. Starts in
    /// [`TransitionPhase::Pending`]; no steps are emitted until the first
    /// [`LevelTransition::advance`].
    #[must_use]
    pub fn begin(from: Option<LevelPath>, to: LevelPath, transfer: Option<LevelPath>) -> Self {
        Self {
            from,
            to,
            transfer,
            phase: TransitionPhase::Pending,
        }
    }

    /// The current phase.
    #[must_use]
    pub fn phase(&self) -> TransitionPhase {
        self.phase
    }

    /// `true` once the transition has fully completed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.phase == TransitionPhase::Done
    }

    /// The level being transitioned to.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.to
    }

    /// Advance to the next phase, returning the steps the session must perform.
    /// Returns an empty vec once [`TransitionPhase::Done`] is reached (and on
    /// every call thereafter — advancing a finished transition is a no-op).
    pub fn advance(&mut self) -> Vec<TransitionStep> {
        match self.phase {
            TransitionPhase::Pending => {
                self.phase = TransitionPhase::Loading;
                let mut steps = vec![TransitionStep::LoadPayload(self.to.clone())];
                if let Some(transfer) = &self.transfer {
                    // Show the transfer/loading level to mask the warm-up.
                    steps.push(TransitionStep::SetVisible {
                        path: transfer.clone(),
                        visible: true,
                    });
                }
                steps
            }
            TransitionPhase::Loading => {
                self.phase = TransitionPhase::Active;
                // Cut to the new level: show it, hide the transfer mask, hide
                // the previous level.
                let mut steps = vec![TransitionStep::SetVisible {
                    path: self.to.clone(),
                    visible: true,
                }];
                if let Some(transfer) = &self.transfer {
                    steps.push(TransitionStep::SetVisible {
                        path: transfer.clone(),
                        visible: false,
                    });
                }
                if let Some(from) = &self.from {
                    steps.push(TransitionStep::SetVisible {
                        path: from.clone(),
                        visible: false,
                    });
                }
                steps
            }
            TransitionPhase::Active => {
                self.phase = TransitionPhase::Unloading;
                // Reclaim the previous level's payload (nothing to unload on a
                // first load).
                match &self.from {
                    Some(from) => vec![TransitionStep::UnloadPayload(from.clone())],
                    None => Vec::new(),
                }
            }
            TransitionPhase::Unloading => {
                self.phase = TransitionPhase::Done;
                Vec::new()
            }
            TransitionPhase::Done => Vec::new(),
        }
    }

    /// Run the whole transition to completion, collecting every step in order.
    /// A convenience for a synchronous (non-async-load) swap where the session
    /// performs all steps in one frame.
    pub fn run_to_completion(&mut self) -> Vec<TransitionStep> {
        let mut all = Vec::new();
        while !self.is_done() {
            all.extend(self.advance());
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> LevelPath {
        s.to_owned()
    }

    #[test]
    fn full_transition_with_transfer_emits_canonical_sequence() {
        let mut t = LevelTransition::begin(
            Some(p("/World/Levels/Hub")),
            p("/World/Levels/Forest"),
            Some(p("/World/Levels/Transfer")),
        );
        assert_eq!(t.phase(), TransitionPhase::Pending);

        // 1. Loading: load next payload + show transfer.
        assert_eq!(
            t.advance(),
            vec![
                TransitionStep::LoadPayload(p("/World/Levels/Forest")),
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Transfer"),
                    visible: true
                },
            ]
        );
        assert_eq!(t.phase(), TransitionPhase::Loading);

        // 2. Active: cut — show next, hide transfer, hide previous.
        assert_eq!(
            t.advance(),
            vec![
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Forest"),
                    visible: true
                },
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Transfer"),
                    visible: false
                },
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Hub"),
                    visible: false
                },
            ]
        );
        assert_eq!(t.phase(), TransitionPhase::Active);

        // 3. Unloading: reclaim previous payload.
        assert_eq!(
            t.advance(),
            vec![TransitionStep::UnloadPayload(p("/World/Levels/Hub"))]
        );
        assert_eq!(t.phase(), TransitionPhase::Unloading);

        // 4. Done.
        assert!(t.advance().is_empty());
        assert!(t.is_done());
    }

    #[test]
    fn first_load_has_no_previous_to_hide_or_unload() {
        let mut t = LevelTransition::begin(None, p("/World/Levels/Hub"), None);

        // Loading: just load (no transfer configured).
        assert_eq!(
            t.advance(),
            vec![TransitionStep::LoadPayload(p("/World/Levels/Hub"))]
        );
        // Active: show the new level only — nothing to hide.
        assert_eq!(
            t.advance(),
            vec![TransitionStep::SetVisible {
                path: p("/World/Levels/Hub"),
                visible: true
            }]
        );
        // Unloading: nothing to unload on a first load.
        assert!(t.advance().is_empty());
        assert_eq!(t.phase(), TransitionPhase::Unloading);
        // Done.
        assert!(t.advance().is_empty());
        assert!(t.is_done());
    }

    #[test]
    fn transition_without_transfer_cuts_directly() {
        let mut t = LevelTransition::begin(
            Some(p("/World/Levels/Hub")),
            p("/World/Levels/Forest"),
            None,
        );
        assert_eq!(
            t.advance(),
            vec![TransitionStep::LoadPayload(p("/World/Levels/Forest"))]
        );
        // No transfer visibility toggles.
        assert_eq!(
            t.advance(),
            vec![
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Forest"),
                    visible: true
                },
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Hub"),
                    visible: false
                },
            ]
        );
        assert_eq!(
            t.advance(),
            vec![TransitionStep::UnloadPayload(p("/World/Levels/Hub"))]
        );
    }

    #[test]
    fn advancing_past_done_is_a_noop() {
        let mut t = LevelTransition::begin(None, p("/World/Levels/Hub"), None);
        t.run_to_completion();
        assert!(t.is_done());
        assert!(t.advance().is_empty());
        assert!(t.advance().is_empty());
        assert!(t.is_done());
    }

    #[test]
    fn run_to_completion_collects_every_step_in_order() {
        let mut t = LevelTransition::begin(
            Some(p("/World/Levels/Hub")),
            p("/World/Levels/Forest"),
            Some(p("/World/Levels/Transfer")),
        );
        let all = t.run_to_completion();
        assert_eq!(
            all,
            vec![
                TransitionStep::LoadPayload(p("/World/Levels/Forest")),
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Transfer"),
                    visible: true
                },
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Forest"),
                    visible: true
                },
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Transfer"),
                    visible: false
                },
                TransitionStep::SetVisible {
                    path: p("/World/Levels/Hub"),
                    visible: false
                },
                TransitionStep::UnloadPayload(p("/World/Levels/Hub")),
            ]
        );
        assert!(t.is_done());
    }
}
