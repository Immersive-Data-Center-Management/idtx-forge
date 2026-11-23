//! Native relationship projection, shared across behaviors and the inspector.
use crate::{SessionError, StudioSession};
use bevy_ecs::prelude::*;
use idtx_behavior::{
    ecs::{RelationshipBinding, Relationships, Unavailable},
    entity_to_wire,
};
use idtx_scene::PrimNode;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Component, Default)]
struct ComposedBindings(Vec<idtx_usd::RelationshipBinding>);
#[derive(Resource, Default)]
struct BindingClock(u64);

/// A property notice affects that property; a prim resync affects its subtree.
fn affected(path: &str, changes: &HashSet<String>) -> bool {
    changes.iter().any(|changed| {
        changed == "/"
            || path == changed
            || path
                .strip_prefix(changed)
                .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('.'))
    })
}

/// Invalid authored data is an attachment error, never a substitute JSON value.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextError(pub String);

impl StudioSession {
    /// Rescan after composition changes, or only owners/dependents of changed
    /// relationship properties. Never invoked by a behavior query or frame hook.
    pub(crate) fn refresh_relationship_bindings(&mut self, changed: Option<&HashSet<String>>) {
        let world = self.hydrator.world_mut();
        let nodes: Vec<_> = world
            .query::<(Entity, &PrimNode)>()
            .iter(world)
            .filter(|(_, node)| !node.is_pseudo_root)
            .map(|(entity, node)| (entity, node.clone()))
            .collect();
        let live: HashMap<_, _> = nodes
            .iter()
            .filter(|(_, n)| n.state.loaded && n.state.active)
            .map(|(entity, node)| (node.path.clone(), *entity))
            .collect();
        let mut clock = world.remove_resource::<BindingClock>().unwrap_or_default();
        for (entity, node) in nodes {
            if !node.state.active || !node.state.loaded {
                world
                    .entity_mut(entity)
                    .insert(Unavailable)
                    .remove::<(Relationships, ComposedBindings)>();
                continue;
            }
            world.entity_mut(entity).remove::<Unavailable>();
            if let Some(changed) = changed
                && let Some(cached) = world.get::<ComposedBindings>(entity)
            {
                let owner_changed = affected(&node.path, changed)
                    || changed.iter().any(|path| {
                        path.strip_prefix(&node.path)
                            .is_some_and(|rest| rest.starts_with('.'))
                    });
                let dependent = cached.0.iter().any(|binding| {
                    binding
                        .dependencies
                        .iter()
                        .chain(&binding.targets)
                        .any(|p| affected(p, changed))
                });
                if !owner_changed && !dependent {
                    continue;
                }
            }
            let composed = match self.service.relationship_bindings(node.prim) {
                Ok(bindings) => bindings,
                Err(error) => {
                    tracing::warn!(path = %node.path, %error, "relationship snapshot failed");
                    world
                        .entity_mut(entity)
                        .remove::<(Relationships, ComposedBindings)>();
                    continue;
                }
            };
            let mut next = BTreeMap::new();
            for binding in &composed {
                let targets: Vec<_> = binding
                    .targets
                    .iter()
                    .zip(&binding.statuses)
                    .map(|(path, status)| {
                        let handle = live
                            .get(path)
                            .filter(|_| *status == 0)
                            .map(|entity| entity_to_wire(*entity));
                        idtx_behavior_abi::RelationshipTarget {
                            entity: handle.unwrap_or(idtx_behavior_abi::Entity::NULL),
                            status: if *status == 0 && handle.is_none() {
                                1
                            } else {
                                u32::from(*status)
                            },
                            reserved: 0,
                        }
                    })
                    .collect();
                let old = world
                    .get::<Relationships>(entity)
                    .and_then(|r| r.0.get(&binding.name));
                let same_paths = world
                    .get::<ComposedBindings>(entity)
                    .and_then(|r| r.0.iter().find(|b| b.name == binding.name))
                    .is_some_and(|old| old.targets == binding.targets);
                let revision = if let Some(old) = old.filter(|old| {
                    same_paths && old.targets == targets && old.resolved == binding.resolved
                }) {
                    old.revision
                } else {
                    clock.0 = clock.0.wrapping_add(1);
                    clock.0
                };
                next.insert(
                    binding.name.clone(),
                    RelationshipBinding {
                        revision,
                        resolved: binding.resolved,
                        targets,
                    },
                );
            }
            world
                .entity_mut(entity)
                .insert((Relationships(next), ComposedBindings(composed)));
        }
        world.insert_resource(clock);
    }

    /// The exact bounded authored context for the inspector (never a USD display
    /// summary, which may quote, escape or truncate JSON).
    pub fn behavior_context(&self, path: &str) -> Result<Option<String>, SessionError> {
        let entity = self
            .hydrator
            .entity_for_path(path)
            .ok_or_else(|| idtx_usd::StageError::OpenUsd("context owner missing".into()))?;
        let world = self.hydrator.world();
        if let Some(error) = world.get::<ContextError>(entity) {
            return Err(idtx_usd::StageError::OpenUsd(error.0.clone()).into());
        }
        Ok(world
            .get::<idtx_behavior::PrimBehaviorData>(entity)
            .and_then(|data| data.context.clone()))
    }

    pub fn behavior_relationships(&self, path: &str) -> &[idtx_usd::RelationshipBinding] {
        self.hydrator
            .entity_for_path(path)
            .and_then(|e| self.hydrator.world().get::<ComposedBindings>(e))
            .map_or(&[], |bindings| bindings.0.as_slice())
    }

    #[cfg(feature = "authoring")]
    pub fn set_behavior_relationship(
        &mut self,
        path: &str,
        name: &str,
        targets: &[String],
    ) -> Result<(), SessionError> {
        self.service.set_relationship_targets(path, name, targets)?;
        self.tick()?;
        Ok(())
    }
}

#[cfg(all(test, feature = "authoring"))]
mod tests;
