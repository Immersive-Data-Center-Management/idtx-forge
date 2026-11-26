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

#[derive(Resource, Default)]
struct BindingIndex {
    owners: BTreeMap<String, Entity>,
    records: HashMap<Entity, (String, HashSet<String>)>,
    dependents: BTreeMap<String, HashSet<Entity>>,
    #[cfg(test)]
    reads: usize,
}

impl BindingIndex {
    fn remove(&mut self, entity: Entity) {
        if let Some((path, dependencies)) = self.records.remove(&entity) {
            self.owners.remove(&path);
            for path in dependencies {
                if let Some(owners) = self.dependents.get_mut(&path) {
                    owners.remove(&entity);
                    if owners.is_empty() { self.dependents.remove(&path); }
                }
            }
        }
    }

    fn insert(&mut self, entity: Entity, path: String, composed: &[idtx_usd::RelationshipBinding]) {
        if composed.is_empty() { return; }
        let dependencies: HashSet<_> = composed.iter().flat_map(|binding|
            binding.dependencies.iter().chain(&binding.targets).cloned()).collect();
        for path in &dependencies { self.dependents.entry(path.clone()).or_default().insert(entity); }
        self.owners.insert(path.clone(), entity);
        self.records.insert(entity, (path, dependencies));
    }
}

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
        let mut index = self.hydrator.world_mut().remove_resource::<BindingIndex>().unwrap_or_default();
        let entities: HashSet<_> = if let Some(changed) = changed.filter(|paths| !paths.contains("/")) {
            let mut entities = HashSet::new();
            for path in changed {
                let owner = path.split('.').next().unwrap_or(path);
                if path.contains('.') {
                    entities.extend(self.hydrator.entity_for_path(owner));
                } else {
                    entities.extend(self.hydrator.subtree_entities(owner));
                }
                // Include removed owners, and forwarding/target dependents outside
                // the edited namespace. No whole-world path or live-handle map.
                if path.contains('.') { entities.extend(index.owners.get(owner).copied()); }
                else {
                    entities.extend(index.owners.range(owner.to_owned()..)
                        .take_while(|(p, _)| p.starts_with(owner))
                        .filter(|(p, _)| idtx_scene::playback::within(p, owner))
                        .map(|(_, &entity)| entity));
                }
                for (dependency, owners) in index.dependents.range(path.clone()..)
                    .take_while(|(p, _)| p.starts_with(path)) {
                    if affected(dependency, changed) { entities.extend(owners); }
                }
            }
            entities
        } else {
            let world = self.hydrator.world_mut();
            let mut entities: HashSet<_> = world.query_filtered::<Entity, With<PrimNode>>().iter(world).collect();
            entities.extend(index.records.keys().copied());
            entities
        };
        let mut clock = self.hydrator.world_mut().remove_resource::<BindingClock>().unwrap_or_default();
        for entity in entities {
            index.remove(entity);
            let Some(node) = self.hydrator.world().get::<PrimNode>(entity).filter(|n| !n.is_pseudo_root).cloned() else { continue; };
            if !node.state.active || !node.state.loaded {
                self.hydrator.world_mut()
                    .entity_mut(entity)
                    .insert(Unavailable)
                    .remove::<(Relationships, ComposedBindings)>();
                continue;
            }
            self.hydrator.world_mut().entity_mut(entity).remove::<Unavailable>();
            #[cfg(test)]
            { index.reads += 1; }
            let composed = match self.service.relationship_bindings(node.prim) {
                Ok(bindings) => bindings,
                Err(error) => {
                    tracing::warn!(path = %node.path, %error, "relationship snapshot failed");
                    self.hydrator.world_mut()
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
                        let handle = self.hydrator.entity_for_path(path)
                            .filter(|_| *status == 0)
                            .filter(|&entity| self.hydrator.world().get::<PrimNode>(entity)
                                .is_some_and(|node| node.state.active && node.state.loaded))
                            .map(entity_to_wire);
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
                let old = self.hydrator.world()
                    .get::<Relationships>(entity)
                    .and_then(|r| r.0.get(&binding.name));
                let same_paths = self.hydrator.world()
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
            index.insert(entity, node.path, &composed);
            if composed.is_empty() {
                self.hydrator.world_mut().entity_mut(entity).remove::<(Relationships, ComposedBindings)>();
            } else {
                self.hydrator.world_mut().entity_mut(entity).insert((Relationships(next), ComposedBindings(composed)));
            }
        }
        self.hydrator.world_mut().insert_resource(clock);
        self.hydrator.world_mut().insert_resource(index);
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
