//! Bounded experience state. Incoming loads use a fork whose writes become
//! visible only at a successful cutover.
use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, Mutex}};
use idtx_behavior_abi::ErrorCode;

pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_VALUE_BYTES: usize = 16 * 1024;
const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Default)]
struct Data {
    values: BTreeMap<String, Vec<u8>>,
    base: Option<BTreeMap<String, Vec<u8>>>,
    reads: BTreeSet<String>,
    invalid: bool,
}

#[derive(Clone, Default)]
pub struct ExperienceState(Arc<Mutex<Data>>);

impl std::fmt::Debug for ExperienceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExperienceState").field("entries", &self.len()).finish_non_exhaustive()
    }
}

fn fits(values: &BTreeMap<String, Vec<u8>>) -> bool {
    values.len() <= MAX_ENTRIES && values.iter().map(|(key, value)| key.len() + value.len()).sum::<usize>() <= MAX_BYTES
}

impl ExperienceState {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut data = self.0.lock().ok()?;
        if data.base.is_some() && !data.reads.contains(key) {
            if data.reads.len() >= MAX_ENTRIES || key.len() > MAX_KEY_BYTES { data.invalid = true; return None; }
            data.reads.insert(key.into());
        }
        data.values.get(key).cloned()
    }

    pub fn set(&self, mut key: String, mut value: Vec<u8>) -> Result<(), ErrorCode> {
        if key.is_empty() || key.len() > MAX_KEY_BYTES || value.len() > MAX_VALUE_BYTES { return Err(ErrorCode::InvalidArgument); }
        key.shrink_to_fit();
        value.shrink_to_fit();
        let mut data = self.0.lock().map_err(|_| ErrorCode::InvalidArgument)?;
        let existing = data.values.get(&key);
        let count = data.values.len() + usize::from(existing.is_none());
        let bytes: usize = data.values.iter().map(|(key, value)| key.len() + value.len()).sum();
        let previous = existing.map_or(0, |value| key.len() + value.len());
        if count > MAX_ENTRIES || bytes - previous + key.len() + value.len() > MAX_BYTES { return Err(ErrorCode::InvalidArgument); }
        data.values.insert(key, value);
        Ok(())
    }

    #[must_use]
    pub fn remove(&self, key: &str) -> Option<Vec<u8>> { self.0.lock().ok()?.values.remove(key) }
    #[must_use]
    pub fn len(&self) -> usize { self.0.lock().map_or(0, |data| data.values.len()) }
    #[must_use]
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    #[must_use]
    pub fn is_fork(&self) -> bool { self.0.lock().map_or(true, |data| data.base.is_some()) }

    pub fn fork(&self) -> Result<Self, ErrorCode> {
        let data = self.0.lock().map_err(|_| ErrorCode::InvalidArgument)?;
        if data.base.is_some() { return Err(ErrorCode::InvalidArgument); }
        let values = data.values.clone();
        Ok(Self(Arc::new(Mutex::new(Data { base: Some(values.clone()), values, ..Data::default() }))))
    }

    /// Merge only initialization writes, rejecting conflicts with the active
    /// session. Cancelled forks are dropped without publishing their changes.
    pub fn commit_into(&self, parent: &Self) -> Result<(), ErrorCode> {
        if Arc::ptr_eq(&self.0, &parent.0) { return Ok(()); }
        let data = self.0.lock().map_err(|_| ErrorCode::InvalidArgument)?.clone();
        if data.invalid { return Err(ErrorCode::InvalidArgument); }
        let base = data.base.as_ref().ok_or(ErrorCode::InvalidArgument)?;
        let mut parent = parent.0.lock().map_err(|_| ErrorCode::InvalidArgument)?;
        if data.reads.iter().any(|key| parent.values.get(key) != base.get(key)) { return Err(ErrorCode::InvalidArgument); }
        let keys: BTreeSet<_> = base.keys().chain(data.values.keys()).collect();
        let mut merged = parent.values.clone();
        for key in keys {
            let before = base.get(key);
            let after = data.values.get(key);
            if before == after { continue; }
            if parent.values.get(key) != before { return Err(ErrorCode::InvalidArgument); }
            if let Some(value) = after { merged.insert(key.clone(), value.clone()); } else { merged.remove(key); }
        }
        if !fits(&merged) { return Err(ErrorCode::InvalidArgument); }
        parent.values = merged;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_rejection_preserves_previous_values() {
        let state = ExperienceState::default();
        for index in 0..3 { state.set(index.to_string(), vec![1; MAX_VALUE_BYTES]).unwrap(); }
        assert!(state.set("overflow".into(), vec![2; MAX_VALUE_BYTES]).is_err());
        assert_eq!(state.len(), 3);
        assert_eq!(state.get("0").unwrap()[0], 1);
    }
    #[test]
    fn forks_commit_atomically_and_cancel_without_parent_writes() {
        let state = ExperienceState::default();
        state.set("score".into(), vec![1]).unwrap();
        let fork = state.fork().unwrap();
        fork.set("score".into(), vec![2]).unwrap();
        assert_eq!(state.get("score"), Some(vec![1]));
        state.set("mask".into(), vec![3]).unwrap();
        fork.commit_into(&state).unwrap();
        assert_eq!(state.get("score"), Some(vec![2]));
        assert_eq!(state.get("mask"), Some(vec![3]));
        let conflict = state.fork().unwrap();
        conflict.set("score".into(), vec![4]).unwrap();
        state.set("score".into(), vec![5]).unwrap();
        assert!(conflict.commit_into(&state).is_err());
        assert_eq!(state.get("score"), Some(vec![5]));
        let reader = state.fork().unwrap();
        assert_eq!(reader.get("score"), Some(vec![5]));
        reader.set("derived".into(), vec![5]).unwrap();
        state.set("score".into(), vec![6]).unwrap();
        assert!(reader.commit_into(&state).is_err(), "stale initialization reads must conflict too");
        assert_eq!(state.get("derived"), None);
    }
}
