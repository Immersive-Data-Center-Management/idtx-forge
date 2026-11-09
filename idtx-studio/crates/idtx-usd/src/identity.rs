#[allow(clippy::wildcard_imports)]
use crate::*;
use std::collections::HashMap;

/// Opaque per-open identity epoch for a stage. A fresh value is minted each
/// time a stage is opened so that [`PrimId`]s from different opens never alias;
/// read the raw counter with [`StageGeneration::get`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StageGeneration(pub(crate) u64);

impl StageGeneration {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Construct a generation from a raw counter. Intended for tests and
    /// synthetic fixtures only; production generations originate in the bridge.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_raw(v: u64) -> Self {
        Self(v)
    }
}

/// Monotonic edit counter for one open stage. Ordered so callers can diff two
/// revisions (e.g. [`StageChangeBatch`]); read the raw value with
/// [`StageRevision::get`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StageRevision(pub(crate) u64);

impl StageRevision {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Bounded lifetime policy for identities whose prims have left the composed
/// topology. Live identities are never reclaimed. A tombstone is retained
/// until either the revision window expires or the capacity is exceeded, in
/// which case the oldest tombstone is reclaimed. Reintroducing a retained path
/// restores the same [`PathId`] and [`PrimId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityRetentionPolicy {
    pub(crate) max_tombstones: usize,
    pub(crate) revision_window: u64,
}

impl IdentityRetentionPolicy {
    #[must_use]
    pub const fn new(max_tombstones: usize, revision_window: u64) -> Self {
        Self {
            max_tombstones,
            revision_window,
        }
    }

    #[must_use]
    pub const fn max_tombstones(self) -> usize {
        self.max_tombstones
    }

    #[must_use]
    pub const fn revision_window(self) -> u64 {
        self.revision_window
    }
}

impl Default for IdentityRetentionPolicy {
    fn default() -> Self {
        Self::new(4_096, 256)
    }
}

/// Observable accounting for the stage-scoped identity index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityRetentionStats {
    pub generation: StageGeneration,
    pub live_prims: usize,
    pub retained_tombstones: usize,
    pub interned_paths: usize,
    pub path_string_bytes: usize,
    pub next_path_slot: u64,
    pub next_prim_slot: u64,
    pub policy: IdentityRetentionPolicy,
    pub oldest_tombstone_revision: Option<StageRevision>,
}

/// Stable interned handle for a prim path string within one stage, allocated by
/// the internal `PathTable`. Survives a path leaving and re-entering the
/// composed topology within the identity-retention window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathId(u32);

impl PathId {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Generation-scoped handle for a live prim: a `slot` qualified by the
/// [`StageGeneration`] of the open it was minted in, so an id from a prior open
/// can never be mistaken for a current prim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrimId {
    pub(crate) slot: u32,
    pub(crate) generation: StageGeneration,
}

impl PrimId {
    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot
    }

    #[must_use]
    pub const fn generation(self) -> StageGeneration {
        self.generation
    }

    /// Construct a synthetic id from raw parts. Intended for tests and
    /// synthetic fixtures only; production ids originate in the bridge, which
    /// keeps `slot`/`generation` private everywhere else.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_raw_parts(slot: u32, generation: StageGeneration) -> Self {
        Self { slot, generation }
    }
}

#[derive(Debug, Default)]
pub(crate) struct PathTable {
    pub(crate) strings: HashMap<PathId, String>,
    by_string: HashMap<String, PathId>,
    pub(crate) next_slot: u32,
}

impl PathTable {
    pub(crate) fn intern(&mut self, path: String) -> Result<PathId, StageError> {
        if let Some(id) = self.by_string.get(&path) {
            return Ok(*id);
        }
        let next_slot =
            self.next_slot
                .checked_add(1)
                .ok_or_else(|| StageError::InvalidProjection {
                    path: path.clone(),
                    detail: "path identity counter exhausted".to_owned(),
                })?;
        let id = PathId(self.next_slot);
        self.next_slot = next_slot;
        self.strings.insert(id, path.clone());
        self.by_string.insert(path, id);
        Ok(id)
    }

    pub(crate) fn get_id(&self, path: &str) -> Option<PathId> {
        self.by_string.get(path).copied()
    }

    pub(crate) fn get(&self, id: PathId) -> Option<&str> {
        self.strings.get(&id).map(String::as_str)
    }

    pub(crate) fn remove(&mut self, id: PathId) {
        if let Some(path) = self.strings.remove(&id) {
            self.by_string.remove(&path);
        }
    }

    pub(crate) fn string_bytes(&self) -> usize {
        self.strings
            .values()
            .fold(0_usize, |total, path| total.saturating_add(path.len()))
    }
}
