use super::{HostState, memory};
use idtx_behavior_abi::{CurveSample, Entity, ErrorCode, MAX_CURVE_QUERY_SAMPLES};
use wasmtime::{Caller, Linker};

pub(super) fn link(linker: &mut Linker<HostState>) -> Result<(), wasmtime::Error> {
    linker.func_wrap(
        "idtx",
        "idtx_curve_length",
        |mut caller: Caller<'_, HostState>, entity: u64, index: u32, out: u32| -> u32 {
            let Some(mem) = memory(&mut caller) else {
                return ErrorCode::InvalidArgument.as_u32();
            };
            if (out as usize).checked_add(8).is_none_or(|end| end > mem.data_size(&caller)) {
                return ErrorCode::InvalidArgument.as_u32();
            }
            match caller.data().curve_length(Entity::from_bits(entity), index) {
                Ok(length) => {
                    if mem.write(&mut caller, out as usize, &length.to_le_bytes()).is_ok() {
                        ErrorCode::Ok.as_u32()
                    } else {
                        ErrorCode::InvalidArgument.as_u32()
                    }
                }
                Err(e) => e.as_u32(),
            }
        },
    )?;
    linker.func_wrap(
        "idtx",
        "idtx_curve_samples",
        |mut caller: Caller<'_, HostState>,
         entity: u64,
         index: u32,
         space: u32,
         distances: u32,
         count: u32,
         out: u32|
         -> u32 {
            let n = count as usize;
            if n > MAX_CURVE_QUERY_SAMPLES || space > 1 {
                return ErrorCode::InvalidArgument.as_u32();
            }
            let Some(mem) = memory(&mut caller) else {
                return ErrorCode::InvalidArgument.as_u32();
            };
            let Some(end) = (distances as usize).checked_add(n * 8) else {
                return ErrorCode::InvalidArgument.as_u32();
            };
            let Some(out_end) = (out as usize).checked_add(n * std::mem::size_of::<CurveSample>())
            else {
                return ErrorCode::InvalidArgument.as_u32();
            };
            let data = mem.data(&caller);
            if end > data.len() || out_end > data.len() {
                return ErrorCode::InvalidArgument.as_u32();
            }
            let mut input = [0.0; MAX_CURVE_QUERY_SAMPLES];
            for (slot, bytes) in input.iter_mut().zip(data[distances as usize..end].chunks_exact(8))
            {
                *slot = f64::from_le_bytes(bytes.try_into().expect("8-byte chunk"));
            }
            let mut result = [CurveSample::default(); MAX_CURVE_QUERY_SAMPLES];
            match caller.data().curve_samples(
                Entity::from_bits(entity),
                index,
                space,
                &input[..n],
                &mut result[..n],
            ) {
                Ok(()) => {
                    if mem
                        .write(&mut caller, out as usize, bytemuck::cast_slice(&result[..n]))
                        .is_ok()
                    {
                        ErrorCode::Ok.as_u32()
                    } else {
                        ErrorCode::InvalidArgument.as_u32()
                    }
                }
                Err(e) => e.as_u32(),
            }
        },
    )?;
    Ok(())
}
