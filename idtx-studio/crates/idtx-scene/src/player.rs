//! Player-ownership role components + resolution.
//!
//! A *player pawn* subtree can carry a network-ownership role authored on its
//! root as `token idtx:playerRole` (+ `string idtx:playerId`). This module
//! defines the ECS-side data and the subtree-resolution helper the behavior
//! host uses to gate execution:
//!
//! * [`NetRole`] — the ownership taxonomy (`local`/`remote`/`server`/`unowned`).
//!   Deliberately mirrored from `idtx_project::NetRole` (identical tokens)
//!   because `idtx-scene` does not depend on `idtx-project`, exactly as the
//!   `StageKind` / `StageKindTag` split is mirrored across the project and USD
//!   layers.
//! * [`PlayerRole`] — a component stamped on a pawn root; its role applies to
//!   the whole subtree (resolved by walking up, not propagated down).
//! * [`resolve_owning_player_role`] — walks `PrimNode.parent` links to the
//!   nearest [`PlayerRole`] ancestor, returning [`NetRole::Local`] when none
//!   exists (the single-player / no-multiplayer default), so existing behavior
//!   attachment is unchanged until a `Remote` role actually appears.
//! * [`PlayerPopulator`] — a [`Populator`] scaffold for API symmetry with
//!   `behavior::BehaviorPopulator` (behaviour feature). As with behaviors, the real
//!   hydration is an FFI-scan on [`crate::hydrator::SceneHydrator`]
//!   (`hydrate_player_roles`), since the `Populator` interface has no
//!   per-attribute read.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use idtx_usd::{PrimId, ViewportProjection};

use crate::components::PrimNode;
use crate::populator::Populator;

/// Re-export of the USD attribute names authored/read for a player pawn, so the
/// hydrator and any authoring path agree on one schema. See
/// [`idtx_usd::PLAYER_ROLE_ATTR`] / [`idtx_usd::PLAYER_ID_ATTR`].
pub use idtx_usd::{PLAYER_ID_ATTR, PLAYER_ROLE_ATTR};

/// The network-ownership role of a player pawn subtree (plan §2.1) — the
/// authority seam that scales to online multiplayer.
///
/// Mirrors `idtx_project::NetRole` token-for-token; the two never share a type
/// because of the crate dependency direction (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NetRole {
    /// Owned by this machine: runs behaviors, consumes input, drives its own
    /// transform.
    Local,
    /// A proxy for another peer: **no behaviors ticked**; transform written by
    /// replication.
    Remote,
    /// Server-authoritative (behaves as [`Local`](Self::Local) in single-player;
    /// reserved so server-only behaviors can be gated in later).
    Server,
    /// No player attribution — the default for every non-player prim.
    #[default]
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

    /// Parse a player-role token (case-insensitively). Unknown/empty tokens map
    /// to [`NetRole::Unowned`] (the safe default: a garbled role is treated as
    /// "no player attribution", never accidentally `Local`).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "local" => Self::Local,
            "remote" => Self::Remote,
            "server" => Self::Server,
            _ => Self::Unowned,
        }
    }
}

/// The ownership role stamped on a player pawn root by the hydrator from the
/// authored `idtx:playerRole` / `idtx:playerId` attributes. The role governs
/// its **whole subtree** — descendants resolve it by walking up (see
/// [`resolve_owning_player_role`]), so it is stored only on the authored prim.
#[derive(Debug, Clone, Component, PartialEq, Eq)]
pub struct PlayerRole {
    /// The ownership role.
    pub role: NetRole,
    /// The per-instance player id (`""` when unauthored). Replaced at join by
    /// the netcode for remote proxies.
    pub player_id: String,
}

impl PlayerRole {
    /// A role with no player id.
    #[must_use]
    pub fn new(role: NetRole) -> Self {
        Self {
            role,
            player_id: String::new(),
        }
    }

    /// Builder: set the player id.
    #[must_use]
    pub fn with_player_id(mut self, id: impl Into<String>) -> Self {
        self.player_id = id.into();
        self
    }
}

/// Resolve the owning-player role for `entity` by walking up `PrimNode.parent`
/// links to the nearest ancestor (or `entity` itself) carrying a [`PlayerRole`].
///
/// Returns [`NetRole::Local`] when no [`PlayerRole`] ancestor exists — the
/// single-player / no-multiplayer default — so the behavior-host gate is a
/// **no-op until a role is actually authored**. A resolved [`NetRole::Unowned`]
/// (an explicitly-authored "unowned" role) is likewise treated as `Local` by
/// callers gating behavior execution; only `Remote` suppresses.
///
/// The ancestor walk mirrors [`SceneHydrator::compute_effective_visibility`] —
/// there is no bevy hierarchy, so parent links are `PrimNode.parent: PrimId`
/// resolved back to entities via a transient index built from the world. This
/// is called at behavior-attach time (not the hot per-frame path), so building
/// the index per call is acceptable. Bounded to 64 hops to defend against a
/// malformed parent cycle.
///
/// [`SceneHydrator::compute_effective_visibility`]: crate::hydrator::SceneHydrator
#[must_use]
pub fn resolve_owning_player_role(world: &World, entity: Entity) -> NetRole {
    nearest_authored_player_role(world, entity).unwrap_or(NetRole::Local)
}

/// Walk `PrimNode.parent` links to the nearest **explicitly authored**
/// [`PlayerRole`] ancestor (inclusive of `entity` itself), returning its role,
/// or `None` when no ancestor carries a `PlayerRole`.
///
/// Unlike [`resolve_owning_player_role`] — which folds "no role authored" into
/// [`NetRole::Local`] for the behavior-gate default — this distinguishes
/// *absent* from *authored `local`*. Callers that must not treat a plain
/// (player-less) scene prim as a player use this: e.g. selecting the render
/// viewport only when a real player camera exists.
///
/// The ancestor walk mirrors [`resolve_owning_player_role`] (a transient
/// `PrimId → Entity` index; bounded to 64 hops against a malformed cycle).
#[must_use]
pub fn nearest_authored_player_role(world: &World, entity: Entity) -> Option<NetRole> {
    nearest_authored_player_root(world, entity).map(|(_, role)| role)
}

/// Like [`nearest_authored_player_role`] but returns the ancestor ENTITY that
/// carries the nearest **explicitly authored** [`PlayerRole`] (inclusive of
/// `entity`), paired with its role. That entity is the pawn ROOT of `entity`'s
/// rig — callers anchor a viewer to (or read a transform from) its
/// [`WorldTransform`][crate::components::WorldTransform]. `None` when no
/// ancestor carries a role.
///
/// Ancestor walk + transient `PrimId → Entity` index as in
/// [`nearest_authored_player_role`]; bounded to 64 hops against a malformed
/// parent cycle.
#[must_use]
pub fn nearest_authored_player_root(
    world: &World,
    entity: Entity,
) -> Option<(Entity, NetRole)> {
    // Build a PrimId → Entity index from the world (no hydrator map is in scope
    // at the session gate). `iter_entities` gives shared access, compatible with
    // an active `Query::iter` borrow at the call site.
    let mut by_prim: HashMap<PrimId, Entity> = HashMap::new();
    for e in world.iter_entities() {
        if let Some(node) = e.get::<PrimNode>() {
            by_prim.insert(node.prim, e.id());
        }
    }

    let mut current = entity;
    for _ in 0..64_u32 {
        if let Some(role) = world.get::<PlayerRole>(current) {
            return Some((current, role.role));
        }
        let node = world.get::<PrimNode>(current)?;
        current = node.parent.and_then(|p| by_prim.get(&p).copied())?;
    }
    None
}

/// Populator scaffold for `idtx:playerRole`-carrying prims, mirroring
/// `behavior::BehaviorPopulator` (behaviour feature).
///
/// As with behaviors, the [`Populator`] interface has no per-attribute read, so
/// [`Populator::populate`] is a documented no-op — the real hydration is
/// [`crate::hydrator::SceneHydrator::hydrate_player_roles`]. The type exists so
/// registration is a localized change if the per-attribute FFI ever lands.
#[derive(Debug, Clone)]
pub struct PlayerPopulator {
    type_names: Vec<&'static str>,
}

impl PlayerPopulator {
    /// Register for the given USD schema `type_names` (typically the pawn-root
    /// `Xform`). Pass an empty slice to register as the fallback (rarely wanted).
    #[must_use]
    pub fn new(type_names: &[&'static str]) -> Self {
        Self {
            type_names: type_names.to_vec(),
        }
    }
}

impl Populator for PlayerPopulator {
    fn type_names(&self) -> &[&'static str] {
        &self.type_names
    }

    fn populate(
        &self,
        _entity: &mut EntityWorldMut<'_>,
        _prim: PrimId,
        _projection: &ViewportProjection,
    ) {
        // Pending the `eval_attribute` FFI escape hatch, `idtx:playerRole` is not
        // reachable from the `ViewportProjection` alone; the hydrator's
        // `hydrate_player_roles` FFI-scan does the work instead.
    }

    fn depopulate(&self, entity: &mut EntityWorldMut<'_>) {
        entity.remove::<PlayerRole>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::PrimNode;
    use idtx_usd::{PrimState, StageGeneration};

    /// Spawn a `PrimNode`-carrying entity; returns its `Entity` and `PrimId`.
    fn spawn_prim(
        world: &mut World,
        slot: u32,
        path: &str,
        parent: Option<PrimId>,
    ) -> (Entity, PrimId) {
        let prim = PrimId::from_raw_parts(slot, StageGeneration::from_raw(1));
        let e = world
            .spawn(PrimNode {
                prim,
                path: path.to_owned(),
                parent,
                name: path.rsplit('/').next().unwrap_or_default().to_owned(),
                type_name: "Xform".to_owned(),
                state: PrimState {
                    active: true,
                    defined: true,
                    loaded: true,
                },
                is_pseudo_root: false,
                local_layer_identifier: None,
            })
            .id();
        (e, prim)
    }

    #[test]
    fn net_role_token_round_trips_and_defaults_unowned() {
        for role in [
            NetRole::Local,
            NetRole::Remote,
            NetRole::Server,
            NetRole::Unowned,
        ] {
            assert_eq!(NetRole::from_token(role.as_token()), role);
        }
        assert_eq!(NetRole::from_token("  REMOTE "), NetRole::Remote);
        // Unknown/empty → Unowned (safe default, never Local).
        assert_eq!(NetRole::from_token("bogus"), NetRole::Unowned);
        assert_eq!(NetRole::from_token(""), NetRole::Unowned);
        assert_eq!(NetRole::default(), NetRole::Unowned);
    }

    #[test]
    fn resolve_defaults_local_without_any_player_role() {
        let mut world = World::new();
        let (child, _) = spawn_prim(&mut world, 0, "/World/Player/Camera", None);
        // No PlayerRole anywhere → single-player default.
        assert_eq!(resolve_owning_player_role(&world, child), NetRole::Local);
    }

    #[test]
    fn resolve_walks_up_to_nearest_player_role() {
        let mut world = World::new();
        let (player, player_prim) = spawn_prim(&mut world, 1, "/World/Player", None);
        let (camera, camera_prim) =
            spawn_prim(&mut world, 2, "/World/Player/Camera", Some(player_prim));
        let (capsule, _) = spawn_prim(&mut world, 3, "/World/Player/Capsule", Some(player_prim));
        let _ = camera_prim;

        // Role authored on the pawn root governs the whole subtree.
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Remote).with_player_id("peer-7"));

        assert_eq!(resolve_owning_player_role(&world, player), NetRole::Remote);
        assert_eq!(resolve_owning_player_role(&world, camera), NetRole::Remote);
        assert_eq!(resolve_owning_player_role(&world, capsule), NetRole::Remote);
    }

    #[test]
    fn resolve_uses_nearest_role_when_nested() {
        let mut world = World::new();
        let (outer, outer_prim) = spawn_prim(&mut world, 1, "/World/Player", None);
        let (inner, inner_prim) = spawn_prim(&mut world, 2, "/World/Player/Sub", Some(outer_prim));
        let (leaf, _) = spawn_prim(&mut world, 3, "/World/Player/Sub/Leaf", Some(inner_prim));

        world
            .entity_mut(outer)
            .insert(PlayerRole::new(NetRole::Local));
        world
            .entity_mut(inner)
            .insert(PlayerRole::new(NetRole::Remote));

        // Nearest ancestor wins.
        assert_eq!(resolve_owning_player_role(&world, leaf), NetRole::Remote);
        assert_eq!(resolve_owning_player_role(&world, outer), NetRole::Local);
    }

    #[test]
    fn populator_depopulate_removes_player_role() {
        let mut world = World::new();
        let e = world.spawn(PlayerRole::new(NetRole::Remote)).id();
        let pop = PlayerPopulator::new(&["Xform"]);
        {
            let mut em = world.entity_mut(e);
            pop.depopulate(&mut em);
        }
        assert!(world.get::<PlayerRole>(e).is_none());
    }

    #[test]
    fn nearest_authored_role_is_none_without_any_player_role() {
        let mut world = World::new();
        let (player, player_prim) = spawn_prim(&mut world, 1, "/World/Player", None);
        let (camera, _) = spawn_prim(&mut world, 2, "/World/Player/Camera", Some(player_prim));
        let _ = player;
        // No PlayerRole authored → None (distinct from the `Local` default that
        // `resolve_owning_player_role` folds absence into).
        assert_eq!(nearest_authored_player_role(&world, camera), None);
        assert_eq!(resolve_owning_player_role(&world, camera), NetRole::Local);
    }

    #[test]
    fn nearest_authored_role_reports_local_and_inherits_down_subtree() {
        let mut world = World::new();
        let (player, player_prim) = spawn_prim(&mut world, 1, "/World/Player", None);
        let (camera, _) = spawn_prim(&mut world, 2, "/World/Player/Camera", Some(player_prim));
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Local));
        assert_eq!(
            nearest_authored_player_role(&world, player),
            Some(NetRole::Local)
        );
        // The role on the pawn root is seen by the child camera.
        assert_eq!(
            nearest_authored_player_role(&world, camera),
            Some(NetRole::Local)
        );
    }

    #[test]
    fn nearest_authored_role_reports_remote() {
        let mut world = World::new();
        let (player, player_prim) = spawn_prim(&mut world, 1, "/World/Player", None);
        let (camera, _) = spawn_prim(&mut world, 2, "/World/Player/Camera", Some(player_prim));
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Remote).with_player_id("peer-7"));
        assert_eq!(
            nearest_authored_player_role(&world, camera),
            Some(NetRole::Remote)
        );
    }
}
