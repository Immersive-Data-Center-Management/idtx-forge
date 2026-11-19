//! Batch-wide invalidation, independent of the identity used to route a notice.
use idtx_usd::{ChangedPrim, DirtyDomains, PrimId};
use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct BatchInvalidation {
    pub structural_paths: Vec<String>,
    pub removed: HashSet<PrimId>,
    pub value_roots: Vec<ChangedPrim>,
}

impl BatchInvalidation {
    pub fn from_changes(changes: &[ChangedPrim]) -> Self {
        let mut summary = Self::default();
        for change in changes {
            if change.resync || change.domains.contains(DirtyDomains::TOPOLOGY) {
                summary.structural_paths.push(change.notice_path.clone());
            } else {
                summary.value_roots.push(change.clone());
            }
        }
        summary.structural_paths.sort();
        summary.structural_paths.dedup();
        summary
    }

    /// Values outside the structural subtrees must not disappear into the
    /// topology branch. For mixed/ambiguous batches use whole-stage viewport
    /// projection AND reconciliation; a scoped render splice is not sufficient.
    /// Material/relationship edits may also affect consumers outside their root.
    pub fn can_scope(&self) -> bool {
        !self.structural_paths.is_empty()
            && self.structural_paths.iter().all(|root| root != "/")
            && self.value_roots.iter().all(|change| {
                !change.domains.contains(DirtyDomains::MATERIAL)
                    && !change.domains.contains(DirtyDomains::RELATIONSHIP)
                    && !change.domains.contains(DirtyDomains::UNKNOWN)
                    && self.structural_paths.iter().any(|root| {
                        change.notice_path == *root
                            || change
                                .notice_path
                                .strip_prefix(root)
                                .is_some_and(|tail| tail.starts_with('/'))
                    })
            })
    }

    pub fn can_splice(&self) -> bool {
        self.can_scope() && self.removed.is_empty()
    }
}

/// Like transform patches, topology drains accumulate until presentation takes
/// them. A later drain must not discard an earlier spawn's scope.
pub(crate) fn merge_topology_splice_into(
    pending: &mut Option<HashSet<PrimId>>,
    scope: &HashSet<PrimId>,
) {
    pending
        .get_or_insert_with(HashSet::new)
        .extend(scope.iter().copied());
}

#[cfg(test)]
mod tests;
