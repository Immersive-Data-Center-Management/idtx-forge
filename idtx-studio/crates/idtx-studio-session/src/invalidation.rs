//! Batch-wide invalidation, independent of the identity used to route a notice.
use idtx_usd::{ChangedPrim, DirtyDomains, PrimId};
use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct BatchInvalidation {
    pub structural_paths: Vec<String>,
    pub removed: HashSet<PrimId>,
    pub value_roots: Vec<ChangedPrim>,
    // USD merges covered property notices into their ancestor resync record.
    // Their non-local domains must not disappear with the separate value list.
    pub nonlocal_domains: bool,
    pub material_graph_resync: bool,
}

impl BatchInvalidation {
    pub fn from_changes(changes: &[ChangedPrim]) -> Self {
        let mut summary = Self::default();
        for change in changes {
            summary.nonlocal_domains |= change.domains.contains(DirtyDomains::MATERIAL)
                || change.domains.contains(DirtyDomains::RELATIONSHIP)
                || change.domains.contains(DirtyDomains::UNKNOWN);
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

    /// Capture old shading identities before hydration/removal can erase them.
    /// Existing shading graphs can feed bindings outside the notice namespace.
    /// Standalone additions to such graphs are also ambiguous; a wholly new
    /// referenced/spawned subtree may still hydrate its own local materials.
    /// This examines only delta identities (no full-stage FFI scan).
    pub fn include_topology(
        &mut self,
        delta: &idtx_usd::TopologyDelta,
        hydrator: &idtx_scene::SceneHydrator,
        service: &idtx_usd::StageService,
    ) {
        self.removed.clone_from(&delta.removed);
        let shading = |name: &str| matches!(name, "Material" | "Shader" | "NodeGraph");
        let old_shading = |prim| {
            hydrator.entity_for(prim).is_some_and(|entity| {
                hydrator
                    .world()
                    .get::<idtx_scene::PrimNode>(entity)
                    .is_some_and(|node| shading(&node.type_name))
                    || hydrator
                        .world()
                        .get::<idtx_scene::MaterialComp>(entity)
                        .is_some()
            })
        };
        self.material_graph_resync = delta.removed.iter().copied().any(old_shading)
            || delta.upserts.iter().any(|record| {
                old_shading(record.prim)
                    || (shading(&record.type_name)
                        && service.path(record.path).is_some_and(|path| {
                            self.structural_paths.iter().any(|root| root == path)
                        }))
            });
    }

    /// Values outside the structural subtrees must not disappear into the
    /// topology branch. For mixed/ambiguous batches use whole-stage viewport
    /// projection AND reconciliation; a scoped render splice is not sufficient.
    /// Material/relationship edits may also affect consumers outside their root.
    pub fn can_scope(&self) -> bool {
        !self.structural_paths.is_empty()
            && !self.nonlocal_domains
            && !self.material_graph_resync
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
