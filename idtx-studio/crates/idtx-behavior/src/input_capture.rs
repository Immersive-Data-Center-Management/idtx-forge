//! Frame-stable modal input ownership. Requests made in `on_input_capture`
//! compete together at the pre-input boundary; ordinary-hook requests take
//! effect at the next boundary. Releasing never exposes a held press edge.

use crate::{
    InputSnapshot,
    work::{QueueBudget, QueueOwner, Reservation},
};
use idtx_behavior_abi::{Entity, ErrorCode, InputActionId, InputPhase, PointerSource};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Capture {
    pub token: u64,
    pub owner: Entity,
    pub owner_key: String,
    lifetime: QueueOwner,
    _reservation: Reservation,
    pub player: Entity,
    pub target: Entity,
    pub source: PointerSource,
    pub priority: i32,
    pub actions: BTreeSet<u32>,
    guarded: BTreeSet<u32>,
    pub active: bool,
    released: bool,
}

#[derive(Default)]
pub struct InputCapture {
    next: u64,
    captures: BTreeMap<u64, Capture>,
    suppressed: BTreeSet<(u64, PointerSource, u32)>,
}

/// Diagnostics are a snapshot; tokens become active only at dispatch boundaries.
#[derive(Debug, Clone)]
pub struct FocusOwner {
    pub token: u64,
    pub owner: Entity,
    pub player: Entity,
    pub target: Entity,
    pub source: PointerSource,
    pub priority: i32,
    pub actions: Vec<u32>,
    pub active: bool,
}

impl InputCapture {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acquire(
        &mut self,
        owner: Entity,
        owner_key: &str,
        player: Entity,
        target: Entity,
        source: PointerSource,
        priority: i32,
        actions: &[u32],
        budget: &QueueBudget,
    ) -> Result<u64, ErrorCode> {
        if source == PointerSource::Unspecified
            || actions.is_empty()
            || actions.len() > 32
            || actions.contains(&0)
            || self.captures.len() >= 128
        {
            return Err(ErrorCode::InvalidArgument);
        }
        let reservation = budget
            .reserve(std::mem::size_of::<Capture>() + owner_key.len() + actions.len() * 16)?;
        self.next = self.next.checked_add(1).ok_or(ErrorCode::InvalidArgument)?;
        let token = self.next;
        self.captures.insert(
            token,
            Capture {
                token,
                owner,
                owner_key: owner_key.into(),
                lifetime: budget.owner(),
                _reservation: reservation,
                player,
                target,
                source,
                priority,
                actions: actions.iter().copied().collect(),
                guarded: BTreeSet::new(),
                active: false,
                released: false,
            },
        );
        Ok(token)
    }

    pub(crate) fn release(&mut self, token: u64, owner: &str, entity: Entity) {
        if let Some(c) = self.captures.get_mut(&token) {
            if c.owner == entity && c.owner_key == owner {
                c.released = true;
            }
        }
    }

    pub(crate) fn active(&self, token: u64, owner: &str, entity: Entity) -> bool {
        self.captures.get(&token).is_some_and(|c| {
            c.active
                && !c.released
                && c.lifetime.is_active()
                && c.owner == entity
                && c.owner_key == owner
        })
    }

    pub(crate) fn commit(&mut self, input: &InputSnapshot, mut live: impl FnMut(&Capture) -> bool) {
        self.suppressed
            .retain(|(_, source, action)| held(input.for_source(*source), *action));
        let expired: Vec<_> = self
            .captures
            .values()
            .filter(|c| c.released || !c.lifetime.is_active() || !live(c))
            .map(|c| c.token)
            .collect();
        for token in expired {
            let c = self.captures.remove(&token).expect("present");
            for a in c.actions {
                if held(input.for_source(c.source), a) {
                    self.suppressed.insert((c.player.to_bits(), c.source, a));
                }
            }
        }
        let mut order: Vec<_> = self.captures.keys().copied().collect();
        order.sort_by(|a, b| {
            let a = &self.captures[a];
            let b = &self.captures[b];
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.owner.to_bits().cmp(&b.owner.to_bits()))
                .then_with(|| a.owner_key.cmp(&b.owner_key))
                .then_with(|| a.target.to_bits().cmp(&b.target.to_bits()))
                .then_with(|| a.token.cmp(&b.token))
        });
        let mut claimed = BTreeSet::new();
        for token in order {
            let c = self.captures.get_mut(&token).expect("present");
            let wins = c.actions.iter().all(|a| {
                !claimed.iter().any(|&(p, s, action)| {
                    s == c.source
                        && action == *a
                        && (p == 0 || c.player.is_null() || p == c.player.to_bits())
                })
            });
            if wins {
                if !c.active {
                    c.guarded = c
                        .actions
                        .iter()
                        .copied()
                        .filter(|a| held(input.for_source(c.source), *a))
                        .collect();
                }
                c.guarded.retain(|a| held(input.for_source(c.source), *a));
                for &a in &c.actions {
                    claimed.insert((c.player.to_bits(), c.source, a));
                }
            }
            c.active = wins;
        }
    }

    pub(crate) fn allows(
        &self,
        owner: Entity,
        key: &str,
        player: Entity,
        source: PointerSource,
        action: u32,
    ) -> bool {
        let scope = |p: u64, s: PointerSource| {
            (p == 0 || player.is_null() || p == player.to_bits())
                && (source == PointerSource::Unspecified || source == s)
        };
        if self
            .suppressed
            .iter()
            .any(|&(p, s, a)| a == action && scope(p, s))
        {
            return false;
        }
        self.captures
            .values()
            .filter(|c| {
                c.active && c.actions.contains(&action) && scope(c.player.to_bits(), c.source)
            })
            .all(|c| {
                c.owner == owner
                    && c.owner_key == key
                    && !c.released
                    && !c.guarded.contains(&action)
            })
    }

    pub fn diagnostics(&self) -> Vec<FocusOwner> {
        self.captures
            .values()
            .filter(|c| !c.released && c.lifetime.is_active())
            .map(|c| FocusOwner {
                token: c.token,
                owner: c.owner,
                player: c.player,
                target: c.target,
                source: c.source,
                priority: c.priority,
                actions: c.actions.iter().copied().collect(),
                active: c.active && !c.released,
            })
            .collect()
    }

    pub(crate) fn clear(&mut self) {
        self.captures.clear();
        self.suppressed.clear();
    }
}

fn held(input: &InputSnapshot, action: u32) -> bool {
    matches!(
        input.phase(InputActionId(action)),
        InputPhase::Pressed | InputPhase::Held
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputMap, InputRouter, RawInput};

    #[test]
    fn priority_is_order_independent_and_release_requires_neutral() {
        let map = InputMap::parse(
            "[actions.confirm]\nkind='button'\nbindings=['Controller/MetaQuest/RightTrigger']",
        )
        .unwrap();
        let id = map.resolve("confirm").0;
        for reverse in [false, true] {
            let mut focus = InputCapture::default();
            let budget = QueueBudget::new(128, 65536);
            let low = Entity::from_bits(2);
            let high = Entity::from_bits(3);
            let mut tokens = Vec::new();
            for p in if reverse { [10, 1] } else { [1, 10] } {
                let e = if p == 10 { high } else { low };
                let t = focus
                    .acquire(
                        e,
                        "menu",
                        Entity::NULL,
                        e,
                        PointerSource::Right,
                        p,
                        &[id],
                        &budget,
                    )
                    .unwrap();
                if p == 10 {
                    tokens.push(t);
                }
            }
            let mut router = InputRouter::new(map.clone());
            let mut raw = RawInput::default();
            raw.active_sources
                .push("Controller/MetaQuest/RightTrigger".into());
            let input = router.snapshot(&raw, 0.01);
            focus.commit(&input, |_| true);
            assert!(focus.active(tokens[0], "menu", high));
            assert!(
                !focus.allows(high, "menu", Entity::NULL, PointerSource::Right, id),
                "opening press guarded"
            );
            assert!(!focus.allows(low, "menu", Entity::NULL, PointerSource::Right, id));
            assert!(focus.allows(low, "menu", Entity::NULL, PointerSource::Left, id));
            focus.release(tokens[0], "menu", high);
            raw.prev_active_sources = raw.active_sources.clone();
            focus.commit(&router.snapshot(&raw, 0.01), |_| true);
            assert!(
                !focus.allows(low, "menu", Entity::NULL, PointerSource::Right, id),
                "held input stays suppressed"
            );
            raw.active_sources.clear();
            focus.commit(&router.snapshot(&raw, 0.01), |_| true);
            assert!(focus.allows(low, "menu", Entity::NULL, PointerSource::Right, id));
            budget.close();
            focus.commit(&router.snapshot(&raw, 0.01), |_| true);
            assert!(focus.diagnostics().is_empty());
        }
    }
}
