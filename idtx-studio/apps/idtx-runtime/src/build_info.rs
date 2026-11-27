//! Embedded in the executable/cdylib, so a foreign runtime can be inspected
//! without executing it and without relying on a possibly stale sidecar.

const RECORD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime-build.bin"));

#[used]
#[unsafe(no_mangle)]
pub static IDTX_RUNTIME_BUILD_METADATA: [u8; RECORD.len()] = *include_bytes!(concat!(env!("OUT_DIR"), "/runtime-build.bin"));

pub(crate) fn retain() {
    std::hint::black_box(&IDTX_RUNTIME_BUILD_METADATA);
}
