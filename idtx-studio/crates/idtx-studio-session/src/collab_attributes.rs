//! Attribute authority and ordered predictions. The full property snapshots
//! journal the root layer because Studio's undo journal also edits that layer.
//! A broadcast updates authority first; only outstanding requests are replayed
//! above it. Equality never identifies the author of an update.
use crate::{SessionError, StudioSession};
use idtx_collab::proto::{AttributeValue, PrimAttribute};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

type PropertyKey = (String, String);

#[derive(Debug, Default)]
pub(super) struct AttributeSync {
    authority: HashMap<PropertyKey, String>,
    observed: HashMap<PropertyKey, Option<PrimAttribute>>,
    predictions: BTreeMap<u64, HashMap<PropertyKey, String>>,
    applied: HashMap<(String, u64), HashSet<String>>,
    observed_specs: HashMap<PropertyKey, String>,
    local_clears: HashSet<PropertyKey>,
    local_special: HashMap<PropertyKey, String>,
    local_metadata: HashMap<PropertyKey, Vec<(String, String)>>,
    deferred_attributes: Vec<(String, u64, Vec<PrimAttribute>)>,
}

impl AttributeSync {
    fn displayed_snapshot(&self, key: &PropertyKey) -> String {
        self.predictions
            .values()
            .rev()
            .find_map(|request| request.get(key))
            .or_else(|| self.authority.get(key))
            .cloned()
            .unwrap_or_default()
    }

    fn pending_keys(&self) -> HashSet<PropertyKey> {
        self.predictions
            .values()
            .flat_map(|request| request.keys().cloned())
            .collect()
    }
}

fn reserved(name: &str) -> bool {
    name == "xformOpOrder" || name.starts_with("xformOp:")
}

fn deferred_attribute_bytes(path: &str, attributes: &[PrimAttribute]) -> usize {
    512 + path.len()
        + attributes
            .iter()
            .map(|attribute| {
                256 + attribute.name.len()
                    + attribute.type_name.len()
                    + attribute.value.as_ref().map_or(0, |value| {
                        (value.doubles.len() + value.ints.len() + value.uints.len()) * 8
                            + value.bools.len()
                            + value
                                .strings
                                .iter()
                                .map(|text| std::mem::size_of::<String>() + text.len())
                                .sum::<usize>()
                    })
            })
            .sum::<usize>()
}

fn same_attribute(left: &PrimAttribute, right: &PrimAttribute) -> bool {
    if left.name != right.name || left.type_name != right.type_name {
        return false;
    }
    match (&left.value, &right.value) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.is_array == right.is_array
                && left.ints == right.ints
                && left.uints == right.uints
                && left.bools == right.bools
                && left.strings == right.strings
                && left.doubles.len() == right.doubles.len()
                && left
                    .doubles
                    .iter()
                    .zip(&right.doubles)
                    .all(|(left, right)| {
                        left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan())
                    })
        }
        _ => false,
    }
}

fn to_wire(attribute: idtx_usd::SyncAttribute) -> PrimAttribute {
    PrimAttribute {
        name: attribute.name,
        type_name: attribute.type_name,
        value: Some(AttributeValue {
            doubles: attribute.value.doubles,
            ints: attribute.value.ints,
            uints: attribute.value.uints,
            bools: attribute.value.bools,
            strings: attribute.value.strings,
            is_array: attribute.value.is_array,
        }),
    }
}

fn from_wire(attribute: &PrimAttribute) -> Result<idtx_usd::SyncAttribute, SessionError> {
    let value = attribute.value.as_ref().ok_or_else(|| {
        SessionError::Collaboration(format!("attribute {} has no value", attribute.name))
    })?;
    Ok(idtx_usd::SyncAttribute {
        name: attribute.name.clone(),
        type_name: attribute.type_name.clone(),
        value: idtx_usd::SyncAttributeValue {
            doubles: value.doubles.clone(),
            ints: value.ints.clone(),
            uints: value.uints.clone(),
            bools: value.bools.clone(),
            strings: value.strings.clone(),
            is_array: value.is_array,
        },
    })
}

impl StudioSession {
    pub(super) fn defer_missing_snapshot_attributes(
        &mut self,
        path: &str,
        seq: u64,
        attributes: &[PrimAttribute],
    ) -> Result<bool, SessionError> {
        if self
            .collab
            .as_ref()
            .is_some_and(|client| !client.is_synced())
            && self.service.prim_at_path(path).is_none()
        {
            let deferred_bytes: usize = self
                .collab_attributes
                .deferred_attributes
                .iter()
                .map(|(path, _, attributes)| deferred_attribute_bytes(path, attributes))
                .sum();
            if self.collab_attributes.deferred_attributes.len() >= 128
                || deferred_bytes.saturating_add(deferred_attribute_bytes(path, attributes))
                    > 16 * 1024 * 1024
            {
                return Err(SessionError::Collaboration(
                    "unresolved snapshot attributes exceed the recovery budget".into(),
                ));
            }
            self.collab_attributes.deferred_attributes.push((
                path.to_owned(),
                seq,
                attributes.to_vec(),
            ));
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn confirm_attribute_snapshot_targets(&self) -> Result<(), SessionError> {
        if self.collab_attributes.deferred_attributes.is_empty() {
            return Ok(());
        }
        Err(SessionError::Collaboration(format!(
            "snapshot attribute targets were not reconstructed: {:?}",
            self.collab_attributes
                .deferred_attributes
                .iter()
                .map(|(path, _, _)| path)
                .collect::<Vec<_>>()
        )))
    }

    pub(super) fn replay_deferred_attributes(&mut self) -> Result<(), SessionError> {
        let mut index = 0;
        while index < self.collab_attributes.deferred_attributes.len() {
            let (path, seq, attributes) = self.collab_attributes.deferred_attributes[index].clone();
            if self.service.prim_at_path(&path).is_some() {
                self.apply_remote_attributes(&path, seq, &attributes)?;
                self.collab_attributes.deferred_attributes.remove(index);
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    /// Exact authored transform opinions require separate publication evidence
    /// from attribute defaults: publishing a radius cannot cover a raw move.
    pub(super) fn root_transform_opinions(
        &self,
    ) -> Result<BTreeMap<PropertyKey, String>, SessionError> {
        Ok(self
            .service
            .root_transform_attribute_snapshots()?
            .into_iter()
            .filter(|snapshot| reserved(&snapshot.name))
            .map(|snapshot| ((snapshot.prim_path, snapshot.name), snapshot.text))
            .collect())
    }

    pub(super) fn flush_outbound_attributes(
        &mut self,
        batch: &idtx_usd::StageChangeBatch,
    ) -> Result<(), SessionError> {
        if self.collab.is_none() {
            return Ok(());
        }
        let mut changed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut resynced = Vec::new();
        for change in &batch.changes {
            if !change.authored_in_root_layer {
                continue;
            }
            let Some(path) = self.service.path_for(change.prim).map(str::to_owned) else {
                continue;
            };
            if change.resync {
                resynced.push(path);
            } else if let Some(name) = &change.property_name {
                if !reserved(name) {
                    changed.entry(path).or_default().insert(name.clone());
                }
            }
        }
        // Structural undo and bulk authoring can coalesce property notices into
        // a prim resync. Compare root defaults under those roots as well.
        if !resynced.is_empty() {
            for snapshot in self.service.root_attribute_snapshots()? {
                if !reserved(&snapshot.name)
                    && resynced.iter().any(|root| {
                        root == "/"
                            || snapshot.prim_path == *root
                            || snapshot.prim_path.starts_with(&format!("{root}/"))
                    })
                {
                    changed
                        .entry(snapshot.prim_path)
                        .or_default()
                        .insert(snapshot.name);
                }
            }
        }
        for (path, names) in changed {
            self.publish_attributes(&path, &names.into_iter().collect::<Vec<_>>())?;
        }
        Ok(())
    }

    pub(super) fn initialize_attribute_sync(&mut self) -> Result<(), SessionError> {
        self.collab_attributes = AttributeSync::default();
        for snapshot in self.service.root_attribute_snapshots()? {
            if reserved(&snapshot.name) {
                continue;
            }
            let key = (snapshot.prim_path.clone(), snapshot.name.clone());
            let value = self
                .service
                .read_sync_attribute(&snapshot.prim_path, &snapshot.name)
                .ok()
                .flatten()
                .map(to_wire);
            self.collab_attributes
                .authority
                .insert(key.clone(), snapshot.text);
            self.collab_attributes
                .observed_specs
                .insert(key.clone(), self.collab_attributes.authority[&key].clone());
            self.collab_attributes.observed.insert(key, value);
        }
        Ok(())
    }

    /// Save the exact authoritative spec before an immediate editor write.
    pub(super) fn capture_attribute_authority(
        &mut self,
        path: &str,
        name: &str,
    ) -> Result<(), SessionError> {
        if self.collab_applying || self.collab.is_none() || reserved(name) {
            return Ok(());
        }
        let key = (path.to_owned(), name.to_owned());
        if !self.collab_attributes.authority.contains_key(&key) {
            let text = self.service.root_attribute_text(path, name)?;
            self.collab_attributes
                .observed_specs
                .insert(key.clone(), text.clone());
            self.collab_attributes.authority.insert(key, text);
        }
        Ok(())
    }

    /// Publish concrete root defaults only. One notice batch coalesces the
    /// latest value for every changed name and sends one batch per prim.
    pub(super) fn publish_attributes(
        &mut self,
        path: &str,
        names: &[String],
    ) -> Result<(), SessionError> {
        if self.collab_applying || self.collab.is_none() {
            return Ok(());
        }
        let mut attributes = Vec::new();
        let mut snapshots = HashMap::new();
        for name in names {
            if reserved(name) {
                continue;
            }
            let key = (path.to_owned(), name.clone());
            let attribute = match self.service.read_sync_attribute(path, name) {
                Ok(Some(attribute)) => to_wire(attribute),
                Ok(None) => {
                    // Clears, blocks and time-only specs have no wire command.
                    let snapshot = self.service.root_attribute_text(path, name)?;
                    if self.collab_attributes.observed_specs.get(&key) != Some(&snapshot) {
                        self.collab_attributes
                            .local_special
                            .insert(key.clone(), snapshot.clone());
                        if let Some(state) = &mut self.collab_persistence {
                            state.local_only_dirty = true;
                        }
                    }
                    self.collab_attributes
                        .observed_specs
                        .insert(key.clone(), snapshot);
                    self.collab_attributes.observed.insert(key, None);
                    continue;
                }
                Err(error) => {
                    self.push_collab_notice(format!(
                        "Attribute {path}.{name} remains local: {error}"
                    ));
                    if let Some(state) = &mut self.collab_persistence {
                        state.local_only_dirty = true;
                    }
                    continue;
                }
            };
            if self
                .collab_attributes
                .observed
                .get(&key)
                .and_then(Option::as_ref)
                .is_some_and(|observed| same_attribute(observed, &attribute))
            {
                self.preserve_attribute_metadata(&key)?;
                continue;
            }
            let snapshot = self.service.root_attribute_text(path, name)?;
            self.preserve_attribute_metadata(&key)?;
            self.collab_attributes.local_clears.remove(&key);
            self.collab_attributes.local_special.remove(&key);
            snapshots.insert(key.clone(), snapshot);
            self.collab_attributes.authority.entry(key).or_default();
            attributes.push(attribute);
        }
        if attributes.is_empty() {
            return Ok(());
        }
        let request_id = self
            .collab
            .as_ref()
            .and_then(|client| client.send_attributes(path, attributes.clone()));
        if let Some(request_id) = request_id {
            self.collab_attributes.observed_specs.extend(
                snapshots
                    .iter()
                    .map(|(key, snapshot)| (key.clone(), snapshot.clone())),
            );
            self.collab_attributes
                .predictions
                .insert(request_id, snapshots);
            for attribute in attributes {
                self.collab_attributes
                    .observed
                    .insert((path.to_owned(), attribute.name.clone()), Some(attribute));
            }
            let non_sync = self.service.root_non_sync_text()?;
            let transforms = self.root_transform_opinions()?;
            if let Some(state) = &mut self.collab_persistence {
                state.local_only_dirty |= state.non_transform.as_ref() != Some(&non_sync)
                    || state.transform_opinions != transforms;
                state.non_transform = Some(non_sync);
            }
        } else {
            // No optimistic defaults survive an unqueued edit.
            for key in snapshots.keys() {
                self.restore_displayed_attribute(key)?;
            }
            self.push_collab_notice(format!("Attribute edits for {path} could not be queued; wait for collaboration to synchronize"));
        }
        Ok(())
    }

    fn restore_displayed_attribute(&mut self, key: &PropertyKey) -> Result<(), SessionError> {
        let snapshot = self.collab_attributes.displayed_snapshot(key);
        self.service
            .replace_root_attribute_text(&key.0, &key.1, &snapshot)?;
        for (expected, desired) in self
            .collab_attributes
            .local_metadata
            .get(key)
            .cloned()
            .unwrap_or_default()
        {
            if self.service.root_attribute_text(&key.0, &key.1)?.is_empty() {
                self.service
                    .replace_root_attribute_text(&key.0, &key.1, &expected)?;
            }
            self.service.apply_root_layer_delta(&expected, &desired)?;
        }
        if self.collab_attributes.local_clears.contains(key) {
            self.service
                .replace_root_attribute_text(&key.0, &key.1, "")?;
        } else if let Some(special) = self.collab_attributes.local_special.get(key) {
            self.service
                .replace_root_attribute_text(&key.0, &key.1, special)?;
        }
        let value = self
            .service
            .read_sync_attribute(&key.0, &key.1)
            .ok()
            .flatten()
            .map(to_wire);
        self.collab_attributes.observed.insert(key.clone(), value);
        self.collab_attributes.observed_specs.insert(
            key.clone(),
            self.service.root_attribute_text(&key.0, &key.1)?,
        );
        Ok(())
    }

    fn preserve_attribute_metadata(&mut self, key: &PropertyKey) -> Result<(), SessionError> {
        let current = self.service.root_attribute_text(&key.0, &key.1)?;
        if let Some(before) = self.collab_attributes.observed_specs.get(key)
            && before != &current
        {
            let delta = self
                .service
                .root_attribute_metadata_delta(&key.0, &key.1, before, &current)?;
            if delta.0 != delta.1 {
                self.collab_attributes
                    .local_metadata
                    .entry(key.clone())
                    .or_default()
                    .push(delta);
                if let Some(state) = &mut self.collab_persistence {
                    state.local_only_dirty = true;
                }
            }
        }
        self.collab_attributes
            .observed_specs
            .insert(key.clone(), current);
        Ok(())
    }

    pub(super) fn preserve_attribute_clear(&mut self, path: &str, name: &str) {
        if self.collab_applying || self.collab.is_none() || reserved(name) {
            return;
        }
        let key = (path.to_owned(), name.to_owned());
        self.collab_attributes.local_clears.insert(key.clone());
        self.collab_attributes.observed.insert(key.clone(), None);
        self.collab_attributes
            .observed_specs
            .insert(key, String::new());
    }

    pub(super) fn apply_remote_attributes(
        &mut self,
        path: &str,
        seq: u64,
        attributes: &[PrimAttribute],
    ) -> Result<(), SessionError> {
        idtx_collab::attributes::validate_attributes(attributes)
            .map_err(SessionError::Collaboration)?;
        let decoded = attributes
            .iter()
            .map(from_wire)
            .collect::<Result<Vec<_>, _>>()?;
        // Restore the actual authority before applying defaults: a prediction
        // may have created a new spec with a conflicting type.
        let keys: Vec<_> = attributes
            .iter()
            .map(|attribute| (path.to_owned(), attribute.name.clone()))
            .collect();
        let before: Vec<_> = keys
            .iter()
            .map(|key| self.service.root_attribute_text(&key.0, &key.1))
            .collect::<Result<_, _>>()?;
        let result = (|| {
            for key in &keys {
                if self
                    .collab_attributes
                    .predictions
                    .values()
                    .any(|request| request.contains_key(key))
                {
                    let authority = self
                        .collab_attributes
                        .authority
                        .get(key)
                        .cloned()
                        .unwrap_or_default();
                    self.service
                        .replace_root_attribute_text(&key.0, &key.1, &authority)?;
                } else {
                    // With no prediction, native default authoring retains the
                    // live metadata directly; it becomes part of the new spec.
                    self.collab_attributes.local_metadata.remove(key);
                }
            }
            self.service.apply_sync_attributes(path, &decoded)?;
            Ok::<_, SessionError>(())
        })();
        if let Err(error) = result {
            for (key, snapshot) in keys.iter().zip(before) {
                self.service
                    .replace_root_attribute_text(&key.0, &key.1, &snapshot)?;
            }
            return Err(error);
        }
        for key in &keys {
            let snapshot = self.service.root_attribute_text(&key.0, &key.1)?;
            self.collab_attributes
                .authority
                .insert(key.clone(), snapshot);
        }
        if self
            .collab_attributes
            .predictions
            .values()
            .any(|request| request.keys().any(|key| key.0 == path))
        {
            self.collab_attributes
                .applied
                .entry((path.to_owned(), seq))
                .or_default()
                .extend(attributes.iter().map(|attribute| attribute.name.clone()));
        }
        for key in &keys {
            self.restore_displayed_attribute(key)?;
        }
        self.hydrator.reconcile_authored_behaviors(&self.service);
        self.hydrator.hydrate_player_roles(&self.service);
        Ok(())
    }

    pub(super) fn complete_attribute_request(
        &mut self,
        path: &str,
        seq: u64,
        request_id: u64,
        failed: &[idtx_collab::proto::AttributeError],
        attributes: &[PrimAttribute],
    ) -> Result<(), SessionError> {
        if !self.collab_attributes.predictions.contains_key(&request_id) {
            return Ok(());
        }
        let echoed = self.collab_attributes.applied.get(&(path.to_owned(), seq));
        if attributes
            .iter()
            .filter(|attribute| !failed.iter().any(|error| error.name == attribute.name))
            .any(|attribute| {
                seq == 0 || echoed.is_none_or(|names| !names.contains(&attribute.name))
            })
        {
            return Err(SessionError::Collaboration(
                "attribute acknowledgement is missing its authoritative echo".into(),
            ));
        }
        let retired = self
            .collab_attributes
            .predictions
            .remove(&request_id)
            .unwrap_or_default();
        self.collab_attributes
            .applied
            .remove(&(path.to_owned(), seq));
        if !self
            .collab_attributes
            .predictions
            .values()
            .any(|request| request.keys().any(|key| key.0 == path))
        {
            self.collab_attributes
                .applied
                .retain(|(prim, _), _| prim != path);
        }
        for key in retired.keys() {
            self.restore_displayed_attribute(key)?;
        }
        for error in failed {
            self.push_collab_notice(format!(
                "Server rejected attribute {path}.{}: {:?}: {}",
                error.name,
                idtx_collab::proto::AttributeFailureReason::try_from(error.reason).ok(),
                error.detail
            ));
        }
        self.hydrator.reconcile_authored_behaviors(&self.service);
        self.hydrator.hydrate_player_roles(&self.service);
        Ok(())
    }

    /// Disconnection and a new snapshot generation discard every prediction,
    /// revealing exact authority (including absent/blocked/default-less specs).
    pub(super) fn discard_attribute_predictions(&mut self) -> Result<(), SessionError> {
        let keys = self.collab_attributes.pending_keys();
        self.collab_attributes.predictions.clear();
        self.collab_attributes.applied.clear();
        for key in &keys {
            self.restore_displayed_attribute(key)?;
        }
        Ok(())
    }

    pub(super) fn remember_remote_create_attributes(
        &mut self,
        path: &str,
    ) -> Result<(), SessionError> {
        for snapshot in self.service.root_attribute_snapshots()? {
            if snapshot.prim_path != path || reserved(&snapshot.name) {
                continue;
            }
            let key = (snapshot.prim_path, snapshot.name);
            let value = self
                .service
                .read_sync_attribute(&key.0, &key.1)
                .ok()
                .flatten()
                .map(to_wire);
            self.collab_attributes
                .authority
                .insert(key.clone(), snapshot.text);
            self.collab_attributes
                .observed_specs
                .insert(key.clone(), self.collab_attributes.authority[&key].clone());
            self.collab_attributes.observed.insert(key, value);
        }
        self.replay_deferred_attributes()?;
        Ok(())
    }
}
