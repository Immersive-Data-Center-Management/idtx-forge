//! Session-owned runtime templates, authored data snapshots and spawn lifecycle.
#[cfg(feature = "authoring")]
use crate::SessionError;
use crate::StudioSession;
use bevy_ecs::prelude::*;
use idtx_behavior::{PlayState, PrimBehaviorData, SessionEditOp};
use idtx_behavior_abi::{
    Entity as WireEntity, ErrorCode, MAX_BEHAVIOR_CONTEXT_BYTES, SpawnResult, SpawnStatus,
};
use idtx_scene::{PrimNode, behavior::BehaviorRef};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "authoring"), allow(dead_code))]
struct RuntimePrototype {
    asset: PathBuf,
    prim: String,
}

#[derive(Debug)]
#[cfg_attr(not(feature = "authoring"), allow(dead_code))]
struct OwnedSpawn {
    prototype: String,
    context: String,
    transform: [f64; 16],
    connections: Vec<idtx_usd::SessionAttributeConnection>,
    generation: u64,
    tickets: Vec<u64>,
    entity: Option<Entity>,
    ready: bool,
}

#[derive(Resource)]
struct RuntimePrototypes {
    entries: BTreeMap<String, RuntimePrototype>,
    owned: BTreeMap<String, OwnedSpawn>,
    generation: u64,
}
impl Default for RuntimePrototypes {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            owned: BTreeMap::new(),
            generation: 1,
        }
    }
}

impl StudioSession {
    /// Install prepared runtime templates. Assets must already be resolved by
    /// the bundle/preview adapter; guests can reference only these stable IDs.
    pub fn set_runtime_prototypes(
        &mut self,
        prototypes: impl IntoIterator<Item = (String, PathBuf, String)>,
    ) {
        self.clear_runtime_spawns();
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        state.entries = prototypes
            .into_iter()
            .map(|(id, asset, prim)| (id, RuntimePrototype { asset, prim }))
            .collect();
        self.hydrator.world_mut().insert_resource(state);
    }

    /// Refresh scoped prim data outside WASM hooks. Exact authored semantics
    /// come from OpenUSD; unsupported or oversized data becomes an explicit
    /// typed-read error (JSON null), never a silently truncated contract.
    pub fn refresh_behavior_data(&mut self) {
        self.refresh_behavior_data_impl(None);
    }

    /// Subtree-scoped counterpart to [`Self::refresh_behavior_data`]: refreshes
    /// authored behavior data (two FFI reads per prim) only for the prims in
    /// `scope` — the newly-spawned / retopologised set of a localized topology
    /// delta — instead of every active, loaded prim in the stage.
    pub(crate) fn refresh_behavior_data_scoped(
        &mut self,
        scope: &std::collections::HashSet<idtx_usd::PrimId>,
    ) {
        self.refresh_behavior_data_impl(Some(scope));
    }

    fn refresh_behavior_data_impl(
        &mut self,
        scope: Option<&std::collections::HashSet<idtx_usd::PrimId>>,
    ) {
        let max_bytes = u32::try_from(MAX_BEHAVIOR_CONTEXT_BYTES)
            .expect("behavior context byte limit fits u32");
        let targets: Vec<_> = {
            let world = self.hydrator.world_mut();
            let mut query = world.query::<(Entity, &PrimNode)>();
            query
                .iter(world)
                .filter(|(_, node)| !node.is_pseudo_root && node.state.active && node.state.loaded)
                .filter(|(_, node)| scope.is_none_or(|s| s.contains(&node.prim)))
                .map(|(entity, node)| (entity, node.prim, node.path.clone()))
                .collect()
        };
        for (entity, prim, path) in targets {
            let mut data = PrimBehaviorData::default();
            match self
                .service
                .authored_custom_data_json(prim, "idtx", max_bytes)
            {
                Ok(Some(value)) => {
                    data.custom_data.insert("idtx".into(), value);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%path, %error, "invalid authored behavior data");
                    data.custom_data.insert("idtx".into(), "null".into());
                }
            }
            data.context =
                match self
                    .service
                    .authored_string(prim, "idtx:behavior:context", max_bytes)
                {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%path, %error, "invalid authored behavior context");
                        Some("null".into())
                    }
                };
            if data.custom_data.is_empty() && data.context.is_none() {
                self.hydrator
                    .world_mut()
                    .entity_mut(entity)
                    .remove::<PrimBehaviorData>();
            } else {
                self.hydrator.world_mut().entity_mut(entity).insert(data);
            }
        }
    }

    #[cfg(feature = "authoring")]
    pub(crate) fn apply_runtime_spawn(
        &mut self,
        parent_path: &str,
        op: &SessionEditOp,
    ) -> Result<(), SessionError> {
        let SessionEditOp::SpawnReference {
            ticket,
            generation,
            prototype_id,
            name,
            transform,
            context_json,
            connections,
        } = op
        else {
            unreachable!()
        };
        if self.play_state != PlayState::Playing
            || *generation != self.runtime_spawn_generation()
            || !self.behaviors.has_spawn_ticket(*ticket)
        {
            self.behaviors.set_spawn_result(
                *ticket,
                SpawnResult::new(SpawnStatus::Failed, WireEntity::NULL, ErrorCode::ReadOnly),
            );
            return Ok(());
        }
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        let result = (|| {
            let prototype = state.entries.get(prototype_id).ok_or_else(|| {
                SessionError::from(idtx_usd::StageError::UnknownPath(format!(
                    "runtime prototype {prototype_id}"
                )))
            })?;
            if name.is_empty()
                || !name.bytes().enumerate().all(|(i, byte)| {
                    byte == b'_' || byte.is_ascii_alphabetic() || (i > 0 && byte.is_ascii_digit())
                })
            {
                return Err(SessionError::from(
                    idtx_usd::StageError::InvalidProjection {
                        path: name.clone(),
                        detail: "spawn leaf must be a USD identifier".into(),
                    },
                ));
            }
            let path = format!("{}/{}", parent_path.trim_end_matches('/'), name);
            let matrix = transform.to_matrix().to_cols_array().map(f64::from);
            let connections: Vec<_> = connections
                .iter()
                .map(|c| idtx_usd::SessionAttributeConnection {
                    child: c.child.clone(),
                    property: c.property.clone(),
                    source: if c.source.starts_with('/') {
                        c.source.clone()
                    } else {
                        format!("{}/{}", parent_path.trim_end_matches('/'), c.source)
                    },
                })
                .collect();
            if let Some(existing) = state.owned.get_mut(&path) {
                existing
                    .tickets
                    .retain(|ticket| self.behaviors.has_spawn_ticket(*ticket));
                if existing.prototype == *prototype_id
                    && existing.generation == state.generation
                    && existing.context == *context_json
                    && existing.transform == matrix
                    && existing.connections == connections
                    && existing.tickets.len() < 64
                {
                    existing.tickets.push(*ticket);
                    return Ok(());
                }
                return Err(SessionError::from(
                    idtx_usd::StageError::InvalidProjection {
                        path,
                        detail: "runtime spawn name is already owned by another prototype".into(),
                    },
                ));
            }
            if state.owned.len() >= 1024 {
                return Err(SessionError::from(
                    idtx_usd::StageError::InvalidProjection {
                        path,
                        detail: "runtime spawn limit reached".into(),
                    },
                ));
            }
            let asset = prototype.asset.to_str().ok_or_else(|| {
                SessionError::from(idtx_usd::StageError::NonUtf8Path(prototype.asset.clone()))
            })?;
            self.service.spawn_session_reference(
                &path,
                asset,
                &prototype.prim,
                matrix,
                context_json,
                &connections,
            )?;
            // The new prim is not yet in the stage's path→id index, so its resync
            // notice would resolve to the parent and over-scope the next tick's
            // topology work. Record the exact child path so `tick` scopes to it.
            self.pending_spawn_topology_paths.push(path.clone());
            state.owned.insert(
                path,
                OwnedSpawn {
                    prototype: prototype_id.clone(),
                    generation: state.generation,
                    context: context_json.clone(),
                    transform: matrix,
                    connections,
                    tickets: vec![*ticket],
                    entity: None,
                    ready: false,
                },
            );
            Ok(())
        })();
        self.hydrator.world_mut().insert_resource(state);
        if result.is_err() {
            self.behaviors.set_spawn_result(
                *ticket,
                SpawnResult::new(
                    SpawnStatus::Failed,
                    WireEntity::NULL,
                    ErrorCode::InvalidArgument,
                ),
            );
        }
        result
    }

    pub(crate) fn reject_runtime_spawn(&mut self, op: &SessionEditOp, error: ErrorCode) {
        if let SessionEditOp::SpawnReference { ticket, .. } = op {
            self.behaviors.set_spawn_result(
                *ticket,
                SpawnResult::new(SpawnStatus::Failed, WireEntity::NULL, error),
            );
        }
    }

    /// A failed synchronous projection is terminal for newly-created panels.
    /// Existing ready panels retain their ownership for explicit/stop cleanup.
    pub(crate) fn fail_pending_runtime_projection(&mut self) {
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        let pending: Vec<_> = state
            .owned
            .iter()
            .filter(|(_, owned)| !owned.ready)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &pending {
            if let Some(owned) = state.owned.remove(path) {
                for ticket in owned.tickets {
                    self.behaviors.set_spawn_result(
                        ticket,
                        SpawnResult::new(
                            SpawnStatus::Failed,
                            WireEntity::NULL,
                            ErrorCode::InvalidArgument,
                        ),
                    );
                }
            }
        }
        self.hydrator.world_mut().insert_resource(state);
        #[cfg(feature = "authoring")]
        for path in &pending {
            if let Err(error) = self.service.erase_session_prim(path) {
                tracing::warn!(%path, %error, "failed-projection spawn cleanup failed");
            }
        }
    }

    /// Advance accepted tickets to projection, then readiness after attachment
    /// and on_start. Failed/cancelled instances are erased, never left polling.
    pub(crate) fn settle_runtime_spawns(&mut self) {
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        let mut erase = Vec::new();
        let playing = self.play_state == PlayState::Playing;
        for (path, owned) in &mut state.owned {
            let Some(entity) = self.hydrator.entity_for_path(path) else {
                // Projection is synchronous and has already run for this batch.
                // A missing entity is a terminal projection failure.
                for ticket in owned.tickets.drain(..) {
                    self.behaviors.set_spawn_result(
                        ticket,
                        SpawnResult::new(
                            SpawnStatus::Failed,
                            WireEntity::NULL,
                            ErrorCode::NotFound,
                        ),
                    );
                }
                erase.push(path.clone());
                continue;
            };
            owned.entity = Some(entity);
            let wire = idtx_behavior::entity_to_wire(entity);
            // Always pass through Projected before Ready, including a duplicate
            // request for an already attached panel. Host transitions are monotonic.
            for ticket in &owned.tickets {
                self.behaviors.set_spawn_result(
                    *ticket,
                    SpawnResult::new(SpawnStatus::Projected, wire, ErrorCode::Ok),
                );
            }
            let node = self.hydrator.world().get::<PrimNode>(entity);
            if node.is_none_or(|node| !node.state.active || !node.state.loaded) {
                for ticket in owned.tickets.drain(..) {
                    self.behaviors.set_spawn_result(
                        ticket,
                        SpawnResult::new(SpawnStatus::Failed, wire, ErrorCode::NotFound),
                    );
                }
                erase.push(path.clone());
                continue;
            }
            if owned.ready {
                for ticket in owned.tickets.drain(..) {
                    self.behaviors.set_spawn_result(
                        ticket,
                        SpawnResult::new(SpawnStatus::Ready, wire, ErrorCode::Ok),
                    );
                }
                continue;
            }
            // A requester's destruction or stop clears its ticket. Do not
            // leave an orphan pending panel when no requester can observe it.
            owned
                .tickets
                .retain(|ticket| self.behaviors.has_spawn_ticket(*ticket));
            if owned.tickets.is_empty() {
                erase.push(path.clone());
                continue;
            }
            let bref = self.hydrator.world().get::<BehaviorRef>(entity);
            let uuid = crate::behavior::authored_binding_uuid(self.hydrator.world(), entity)
                .or_else(|| {
                    bref.filter(|reference| reference.module_asset.is_none())
                        .map(|reference| reference.uuid.as_str())
                });
            let requires_behavior = node.is_some_and(|node| {
                self.service
                    .prim_has_api(node.prim, "IdtxBehaviorAPI")
                    .unwrap_or(true)
            });
            let status = if bref.is_some() {
                uuid.and_then(|uuid| self.behaviors.instance_readiness(wire, uuid, playing))
                    .unwrap_or(if self.behavior_reconcile_pending {
                        SpawnStatus::Projected
                    } else {
                        SpawnStatus::Failed
                    })
            } else if requires_behavior {
                if self.behavior_reconcile_pending {
                    SpawnStatus::Projected
                } else {
                    SpawnStatus::Failed
                }
            } else {
                SpawnStatus::Ready
            };
            let error = if status == SpawnStatus::Failed {
                uuid.and_then(|uuid| self.behaviors.instance_startup_error(wire, uuid))
                    .unwrap_or(ErrorCode::InvalidArgument)
            } else {
                ErrorCode::Ok
            };
            for ticket in &owned.tickets {
                self.behaviors
                    .set_spawn_result(*ticket, SpawnResult::new(status, wire, error));
            }
            if status == SpawnStatus::Ready {
                owned.ready = true;
                owned.tickets.clear();
            }
            if status == SpawnStatus::Failed {
                erase.push(path.clone());
            }
        }
        for path in &erase {
            state.owned.remove(path);
        }
        self.hydrator.world_mut().insert_resource(state);
        #[cfg(feature = "authoring")]
        if !erase.is_empty() {
            for path in erase {
                if let Err(error) = self.service.erase_session_prim(&path) {
                    tracing::warn!(%path, %error, "runtime spawn cleanup failed");
                }
            }
            if let Err(error) = self.tick() {
                tracing::warn!(%error, "reprojection after runtime spawn cleanup failed");
            }
        }
    }

    #[cfg(feature = "authoring")]
    pub(crate) fn remove_runtime_prim(&mut self, path: &str) -> Result<(), SessionError> {
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        let owned: Vec<_> = state
            .owned
            .keys()
            .filter(|child| *child == path || child.starts_with(&format!("{path}/")))
            .cloned()
            .collect();
        for child in &owned {
            if let Some(spawn) = state.owned.remove(child) {
                for ticket in spawn.tickets {
                    self.behaviors.set_spawn_result(
                        ticket,
                        SpawnResult::new(
                            SpawnStatus::Failed,
                            WireEntity::NULL,
                            ErrorCode::NotFound,
                        ),
                    );
                }
            }
        }
        self.hydrator.world_mut().insert_resource(state);
        for child in owned.iter().rev() {
            self.service.erase_session_prim(child)?;
        }
        if !owned.iter().any(|owned| owned == path) {
            self.service.mutate_session_prim(path, "", true)?;
        }
        Ok(())
    }

    /// Clear strongest-layer runtime references before their weaker payload
    /// is unloaded. Cancel unfinished work from the preceding stage generation.
    #[cfg(feature = "authoring")]
    pub(crate) fn clear_runtime_spawns_under(&mut self, root: &str) -> Result<(), SessionError> {
        for command in self.behaviors.drain_session_edits() {
            self.reject_runtime_spawn(&command.op, ErrorCode::ReadOnly);
        }
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        state.generation = state.generation.wrapping_add(1);
        self.behaviors.set_spawn_generation(state.generation);
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let paths: Vec<_> = state
            .owned
            .iter()
            .filter(|(path, owned)| {
                path.as_str() == root || path.starts_with(&prefix) || !owned.ready
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in &paths {
            if let Some(owned) = state.owned.remove(path) {
                for ticket in owned.tickets {
                    self.behaviors.set_spawn_result(
                        ticket,
                        SpawnResult::new(
                            SpawnStatus::Failed,
                            WireEntity::NULL,
                            ErrorCode::ReadOnly,
                        ),
                    );
                }
            }
        }
        for owned in state.owned.values_mut() {
            owned.generation = state.generation;
        }
        self.hydrator.world_mut().insert_resource(state);
        for path in paths.iter().rev() {
            self.service.erase_session_prim(path)?;
        }
        Ok(())
    }

    pub(crate) fn clear_runtime_spawns(&mut self) {
        self.behaviors.clear_spawn_results();
        // Discard queued requests from the old play generation before another
        // play can start. Unique ticket IDs and has_spawn_ticket also prevent
        // external late completions from recreating an old instance.
        let _ = self.behaviors.drain_session_edits();
        let mut state = self
            .hydrator
            .world_mut()
            .remove_resource::<RuntimePrototypes>()
            .unwrap_or_default();
        state.generation = state.generation.wrapping_add(1);
        self.behaviors.set_spawn_generation(state.generation);
        let paths: Vec<_> = std::mem::take(&mut state.owned).into_keys().collect();
        self.hydrator.world_mut().insert_resource(state);
        #[cfg(feature = "authoring")]
        if !paths.is_empty() {
            for path in paths {
                if let Err(error) = self.service.erase_session_prim(&path) {
                    tracing::warn!(%path, %error, "runtime stop cleanup failed");
                }
            }
            if let Err(error) = self.tick() {
                tracing::warn!(%error, "runtime stop reprojection failed");
            }
        }
        #[cfg(not(feature = "authoring"))]
        let _ = paths;
    }

    /// Projected runtime prim identity for integrations and tests; does not
    /// imply attached behavior readiness.
    pub fn runtime_entity_for_path(&self, path: &str) -> Option<WireEntity> {
        self.hydrator
            .entity_for_path(path)
            .map(idtx_behavior::entity_to_wire)
    }

    pub(crate) fn runtime_spawn_generation(&self) -> u64 {
        self.hydrator
            .world()
            .get_resource::<RuntimePrototypes>()
            .map_or(1, |state| state.generation)
    }

    /// Useful for native embedders inspecting the host's authored-data bridge.
    pub fn runtime_behavior_data(&self, path: &str) -> Option<&PrimBehaviorData> {
        let entity = self.hydrator.entity_for_path(path)?;
        self.hydrator.world().get::<PrimBehaviorData>(entity)
    }
}

#[cfg(all(test, feature = "authoring"))]
mod tests;
