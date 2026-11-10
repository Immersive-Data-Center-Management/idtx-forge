//! Stable widget-ID helper for scene-tree UI widgets.
//!
//! Produces a deterministic `u64` hash of a USD prim path that egui can use
//! as a stable [`egui::Id`]-compatible seed. Keying tree row IDs on the prim
//! path string (rather than the row index) prevents spurious re-layout when
//! siblings are inserted or removed — the fix codex-luna calls `stable_row_id`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Returns a deterministic 64-bit hash of `prim_path` suitable for use as an
/// egui widget seed.
///
/// Callers in `idtx-editor-ui` should convert this to an `egui::Id` via
/// `egui::Id::new(stable_prim_hash(path))`.
#[must_use]
pub fn stable_prim_hash(prim_path: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    prim_path.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_path_same_hash() {
        assert_eq!(
            stable_prim_hash("/World/Objects/Box"),
            stable_prim_hash("/World/Objects/Box"),
        );
    }

    #[test]
    fn different_paths_differ() {
        assert_ne!(
            stable_prim_hash("/World/Objects/Box"),
            stable_prim_hash("/World/Objects/Sphere"),
        );
    }

    #[test]
    fn root_differs_from_child() {
        assert_ne!(stable_prim_hash("/"), stable_prim_hash("/World"));
    }
}
