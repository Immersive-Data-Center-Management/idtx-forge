//! Authored behavior data and ephemeral reference composition.
use crate::{PrimId, StageError, StageService};
use idtx_usd_sys::bridge::ffi;

/// An attribute connection applied atomically to a spawned subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAttributeConnection {
    /// Relative child prim path; empty addresses the spawned root.
    pub child: String,
    /// Destination attribute name.
    pub property: String,
    /// Absolute source attribute path in the receiving stage.
    pub source: String,
}

impl StageService {
    /// Read the composed value of an exact authored customData dictionary key.
    /// Missing authored opinions return None, unsupported types/oversize data
    /// return an error. Reference and payload opinions count as authored.
    pub fn authored_custom_data_json(
        &self,
        prim: PrimId,
        key: &str,
        max_bytes: u32,
    ) -> Result<Option<String>, StageError> {
        self.validate(prim)?;
        let raw = ffi::authored_custom_data_json(
            self.handle_ref()?,
            self.path_for_prim(prim)?,
            key,
            max_bytes,
        )?;
        Ok(raw.authored.then_some(raw.value))
    }

    /// Read a raw authored string, excluding a schema's unauthored fallback.
    pub fn authored_string(
        &self,
        prim: PrimId,
        property: &str,
        max_bytes: u32,
    ) -> Result<Option<String>, StageError> {
        self.validate(prim)?;
        let raw = ffi::authored_string(
            self.handle_ref()?,
            self.path_for_prim(prim)?,
            property,
            max_bytes,
        )?;
        Ok(raw.authored.then_some(raw.value))
    }

    /// Erase an explicitly runtime-owned session spec and its reference.
    /// The caller must maintain ownership; ordinary weaker authored prims use
    /// mutate_session_prim removal instead. Notices remain queued.
    #[cfg(feature = "authoring")]
    pub fn erase_session_prim(&mut self, path: &str) -> Result<(), StageError> {
        ffi::erase_session_prim(self.handle.pin_mut(), path)?;
        Ok(())
    }

    /// Define, reference and configure a new session-only prim as one
    /// rollback-capable transaction. Retains notices; does not project.
    #[cfg(feature = "authoring")]
    pub fn spawn_session_reference(
        &mut self,
        path: &str,
        asset: &str,
        target: &str,
        matrix: [f64; 16],
        context: &str,
        connections: &[SessionAttributeConnection],
    ) -> Result<(), StageError> {
        let children: Vec<_> = connections.iter().map(|c| c.child.clone()).collect();
        let properties: Vec<_> = connections.iter().map(|c| c.property.clone()).collect();
        let sources: Vec<_> = connections.iter().map(|c| c.source.clone()).collect();
        ffi::spawn_session_reference(
            self.handle.pin_mut(),
            path,
            asset,
            target,
            &matrix,
            context,
            &children,
            &properties,
            &sources,
        )?;
        Ok(())
    }
}
