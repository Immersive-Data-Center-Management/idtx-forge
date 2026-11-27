use super::{MAX_POINT_INSTANCES, PointInstance, StageEdit, StageEditResult, StageEditTicket};
use crate::{
    abi::ErrorCode,
    entity::Entity,
    result::{Result, check},
};

impl StageEdit {
    /// Atomically replace native PointInstancer placement and its binary mask.
    /// Inputs are **local USD coordinates**, quaternions are **XYZW**; parent and
    /// prototype transforms are evaluated by USD, not baked into these inputs.
    /// Every id must be unique. Reordered / noncontiguous / negative ids work.
    /// `invisible_ids` names ids, never slots. Poll `edit_status` for commit failure.
    ///
    /// At most 4096 entries (also bounded by the host's argument/queue budget;
    /// 64 KiB by default). Raise that authored budget for larger allocations.
    /// Subsequent pulses should call `set_point_instancer_visibility` instead.
    pub fn set_point_instances(
        self,
        entity: Entity,
        instances: &[PointInstance],
        invisible_ids: &[i64],
    ) -> Result<StageEditTicket> {
        if instances.len() > MAX_POINT_INSTANCES
            || invisible_ids.len() > instances.len()
            || instances.iter().any(|i| !i.is_valid())
        {
            return Err(ErrorCode::InvalidArgument.into());
        }
        Self::post_point_instancer(entity, 0, instances, invisible_ids)
    }

    /// Replace only native `invisibleIds`, preserving topology, placements,
    /// stable ids and cached prototypes. Empty reveals all ids except those in
    /// authored non-time-varying `inactiveIds`. This is native binary visibility.
    /// For a glow use an emissive prototype; instances are not prims/entities and
    /// cannot be passed to `highlight`. Per-instance opacity is not supported.
    pub fn set_point_instancer_visibility(
        self,
        entity: Entity,
        invisible_ids: &[i64],
    ) -> Result<StageEditTicket> {
        Self::post_point_instancer(entity, 1, &[], invisible_ids)
    }

    /// Poll a deferred transaction. Accepted (status 0) is retained; Committed
    /// (1) / Failed (2, with an ErrorCode) is consumed. Unloaded/expired tickets
    /// return NotFound, or a terminal StaleHandle if the requester survives.
    pub fn edit_status(self, ticket: StageEditTicket) -> Result<StageEditResult> {
        #[allow(unused_mut)]
        let mut result = StageEditResult::default();
        #[cfg(target_arch = "wasm32")]
        let code = ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_edit_status(
                ticket.0,
                (&mut result as *mut StageEditResult).cast(),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = ticket;
            ErrorCode::NotFound
        };
        check(code)?;
        Ok(result)
    }

    fn post_point_instancer(
        entity: Entity,
        kind: u32,
        instances: &[PointInstance],
        invisible_ids: &[i64],
    ) -> Result<StageEditTicket> {
        if invisible_ids.len() > MAX_POINT_INSTANCES {
            return Err(ErrorCode::InvalidArgument.into());
        }
        #[allow(unused_mut)]
        let mut ticket = StageEditTicket::default();
        #[cfg(target_arch = "wasm32")]
        let code = ErrorCode::from_u32(unsafe {
            crate::__rt::idtx_session_update_point_instancer(
                entity.wire().to_bits(),
                kind,
                instances.as_ptr().cast(),
                instances.len() as u32,
                invisible_ids.as_ptr().cast(),
                invisible_ids.len() as u32,
                (&mut ticket as *mut StageEditTicket).cast(),
            )
        });
        #[cfg(not(target_arch = "wasm32"))]
        let code = {
            let _ = (entity, kind, instances, invisible_ids);
            ErrorCode::NotFound
        };
        check(code)?;
        Ok(ticket)
    }
}
