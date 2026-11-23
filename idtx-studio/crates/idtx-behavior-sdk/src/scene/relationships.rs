//! Composed, forwarded USD relationships; no path expansion inside JSON.
use crate::{
    abi,
    entity::Entity,
    result::{Result, check},
};

/// One ordered target, including unresolved entries (which retain their slot).
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RelationshipTarget(abi::RelationshipTarget);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetStatus {
    Live,
    Invalid,
    Unloaded,
}

impl RelationshipTarget {
    pub fn status(self) -> TargetStatus {
        match self.0.status {
            0 if !self.0.entity.is_null() => TargetStatus::Live,
            2 => TargetStatus::Unloaded,
            _ => TargetStatus::Invalid,
        }
    }
    pub fn entity(self) -> Option<Entity> {
        (self.status() == TargetStatus::Live).then(|| Entity::from_wire(self.0.entity))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RelationshipTargets {
    pub revision: u64,
    pub total: usize,
    pub written: usize,
    /// False for an unresolved forwarding chain (e.g. a cycle). Native USD
    /// may still return successfully resolved partial targets; inspect this.
    pub resolved: bool,
}
impl RelationshipTargets {
    pub fn truncated(self) -> bool {
        self.written < self.total
    }
}

/// `NotFound` means no relationship; an existing empty relationship succeeds
/// with total zero. Entries preserve native GetForwardedTargets order. An
/// unresolved/property target is Invalid; a target below an unloaded payload is
/// Unloaded. Buffers are caller-owned; only `output[..written]` is updated.
pub fn relationship_targets(
    entity: Entity,
    name: &str,
    output: &mut [RelationshipTarget],
) -> Result<RelationshipTargets> {
    if name.is_empty() || name.len() > 256 || output.len() > abi::MAX_RELATIONSHIP_TARGETS {
        return Err(abi::ErrorCode::InvalidArgument.into());
    }
    #[allow(unused_mut)]
    let mut result = abi::RelationshipResult::default();
    #[cfg(target_arch = "wasm32")]
    let code = abi::ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_relationship_targets(
            entity.wire().to_bits(),
            name.as_ptr(),
            name.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            (&mut result as *mut abi::RelationshipResult).cast(),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (entity, output);
        abi::ErrorCode::NotFound
    };
    check(code)?;
    Ok(RelationshipTargets {
        revision: result.revision,
        total: result.total as usize,
        written: result.written as usize,
        resolved: result.resolved != 0,
    })
}
