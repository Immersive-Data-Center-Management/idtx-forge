//! Fixed-layout, bounded marshalling; no attribute names or JSON mutation seam.
use super::{HostState, memory, range_valid, write_checked};
use idtx_behavior_abi::{Entity, ErrorCode, MAX_POINT_INSTANCES, PointInstance};
use wasmtime::{Caller, Linker};

pub(super) fn link(linker: &mut Linker<HostState>) -> Result<(), wasmtime::Error> {
    linker.func_wrap(
        "idtx",
        "idtx_session_update_point_instancer",
        |mut caller: Caller<'_, HostState>,
         target: u64,
         kind: u32,
         entries_ptr: u32,
         count: u32,
         hidden_ptr: u32,
         hidden_count: u32,
         ticket_ptr: u32|
         -> u32 {
            // Clamp BEFORE arithmetic, reads or allocation (including malicious wasm).
            if kind > 1
                || count as usize > MAX_POINT_INSTANCES
                || hidden_count as usize > MAX_POINT_INSTANCES
                || (kind == 1 && count != 0)
            {
                return ErrorCode::InvalidArgument.as_u32();
            }
            let entries_bytes = count as usize * std::mem::size_of::<PointInstance>();
            let hidden_bytes = hidden_count as usize * 8;
            if entries_bytes + hidden_bytes > caller.data().work_limits.argument_bytes
                || !range_valid(&mut caller, entries_ptr, entries_bytes)
                || !range_valid(&mut caller, hidden_ptr, hidden_bytes)
                || !range_valid(&mut caller, ticket_ptr, 8)
            {
                return ErrorCode::InvalidArgument.as_u32();
            }
            let Some(mem) = memory(&mut caller) else {
                return ErrorCode::InvalidArgument.as_u32();
            };
            let bytes = mem.data(&caller);
            let entries = (kind == 0).then(|| {
                bytes[entries_ptr as usize..entries_ptr as usize + entries_bytes]
                    .chunks_exact(std::mem::size_of::<PointInstance>())
                    .map(bytemuck::pod_read_unaligned)
                    .collect()
            });
            let hidden = bytes[hidden_ptr as usize..hidden_ptr as usize + hidden_bytes]
                .chunks_exact(8)
                .map(bytemuck::pod_read_unaligned)
                .collect();
            match caller.data_mut().post_point_instancer_update(
                Entity::from_bits(target),
                entries,
                hidden,
            ) {
                Ok(ticket) => {
                    // Output was checked before enqueue; guest memory cannot shrink here.
                    write_checked(&mut caller, ticket_ptr, bytemuck::bytes_of(&ticket));
                    ErrorCode::Ok.as_u32()
                }
                Err(error) => error.as_u32(),
            }
        },
    )?;
    linker.func_wrap(
        "idtx",
        "idtx_session_edit_status",
        |mut caller: Caller<'_, HostState>, ticket: u64, out: u32| -> u32 {
            if !range_valid(&mut caller, out, 8) {
                return ErrorCode::InvalidArgument.as_u32();
            }
            match caller.data().edit_result(ticket) {
                Ok(result) => {
                    if !write_checked(&mut caller, out, bytemuck::bytes_of(&result)) {
                        return ErrorCode::InvalidArgument.as_u32();
                    }
                    caller.data_mut().consume_edit_result(ticket);
                    ErrorCode::Ok.as_u32()
                }
                Err(error) => error.as_u32(),
            }
        },
    )?;
    Ok(())
}
