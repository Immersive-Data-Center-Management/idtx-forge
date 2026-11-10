//! Runtime behavior-driven material overrides keyed by stable [`idtx_scene::PrimId`].

use std::collections::HashMap;

// ── Behavior material overrides (U3.5) ──────────────────────────────────────

/// Runtime **material overrides** keyed by stable [`idtx_scene::PrimId`].
///
/// Populated not by USD extraction but by the session draining the behavior
/// overlay queue (U3.4 `set_material_override` / `clear_material_override`):
/// a behaviour asks for the prim it targets to be drawn with a different
/// material this session (a hover highlight material, a "selected" tint) without
/// authoring anything into USD. Keyed by `PrimId` rather than path string per
/// the stable-identity invariant, so an override survives a topology resync.
///
/// The renderer consults this map while recording mesh draws: for a mesh whose
/// prim resolves to a `PrimId` present here, it swaps in the named override
/// material (looked up in [`super::RenderWorldSnapshot::materials`]) in place of the
/// mesh's authored material. Empty by default — the non-override draw path never
/// consults it.
#[derive(Debug, Clone, Default)]
pub struct MaterialOverrideMap {
    by_prim: HashMap<idtx_scene::PrimId, String>,
}

impl MaterialOverrideMap {
    /// Override `prim`'s material with the material named `material_path`
    /// (a key into [`super::RenderWorldSnapshot::materials`]). Replaces any prior
    /// override for that prim.
    pub fn set(&mut self, prim: idtx_scene::PrimId, material_path: impl Into<String>) {
        self.by_prim.insert(prim, material_path.into());
    }

    /// Clear any override on `prim`. Returns the removed material path, if any.
    pub fn clear(&mut self, prim: idtx_scene::PrimId) -> Option<String> {
        self.by_prim.remove(&prim)
    }

    /// The override material path for `prim`, if one is set.
    #[must_use]
    pub fn get(&self, prim: idtx_scene::PrimId) -> Option<&str> {
        self.by_prim.get(&prim).map(String::as_str)
    }

    /// `true` when no overrides are set (the common case — hot path skip).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_prim.is_empty()
    }

    /// Number of active overrides.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_prim.len()
    }
}
