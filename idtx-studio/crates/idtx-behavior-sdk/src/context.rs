//! Allocation-free, typed behavior contracts carried by authored prim data.
use crate::abi::{ErrorCode, MAX_BEHAVIOR_CONTEXT_BYTES};
use crate::result::{BehaviorError, Result, check};

/// An owned UTF-8 contract string with a compile-time byte bound.
pub type ContextString<const N: usize> = arrayvec::ArrayString<N>;

/// Decode a complete JSON value; malformed, truncated or trailing data is rejected.
pub fn decode<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    if bytes.len() > MAX_BEHAVIOR_CONTEXT_BYTES {
        return Err(ErrorCode::InvalidArgument.into());
    }
    let mut unescaped = [0u8; MAX_BEHAVIOR_CONTEXT_BYTES];
    let (value, used) = serde_json_core::from_slice_escaped(bytes, &mut unescaped)
        .map_err(|_| BehaviorError(ErrorCode::TypeMismatch))?;
    if !bytes[used..].iter().all(u8::is_ascii_whitespace) {
        return Err(ErrorCode::TypeMismatch.into());
    }
    Ok(value)
}

pub(crate) fn own<'a, T: serde::Deserialize<'a>>(scratch: &'a mut [u8]) -> Result<T> {
    if scratch.len() > MAX_BEHAVIOR_CONTEXT_BYTES {
        return Err(ErrorCode::InvalidArgument.into());
    }
    #[allow(unused_mut)]
    let mut written = 0u32;
    #[cfg(target_arch = "wasm32")]
    let code = ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_behavior_context(scratch.as_mut_ptr(), scratch.len() as u32, &mut written)
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = ErrorCode::NotFound;
    check(code)?;
    let bytes = scratch
        .get(..written as usize)
        .ok_or(BehaviorError(ErrorCode::InvalidArgument))?;
    decode(bytes)
}

pub(crate) fn selected<'a, T: serde::Deserialize<'a>>(
    entity: crate::entity::Entity,
    key: &str,
    scratch: &'a mut [u8],
) -> Result<Option<T>> {
    if key.is_empty() || key.len() > 256 || scratch.len() > MAX_BEHAVIOR_CONTEXT_BYTES {
        return Err(ErrorCode::InvalidArgument.into());
    }
    #[allow(unused_mut)]
    let mut written = 0u32;
    #[cfg(target_arch = "wasm32")]
    let code = ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_prim_custom_data(
            entity.wire().to_bits(),
            key.as_ptr(),
            key.len() as u32,
            scratch.as_mut_ptr(),
            scratch.len() as u32,
            &mut written,
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = entity;
        ErrorCode::NotFound
    };
    if code == ErrorCode::NotFound {
        return Ok(None);
    }
    check(code)?;
    let bytes = scratch
        .get(..written as usize)
        .ok_or(BehaviorError(ErrorCode::InvalidArgument))?;
    decode(bytes).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq)]
    struct Contract {
        url: ContextString<128>,
        count: u32,
    }
    #[test]
    fn owned_bounded_contract_roundtrips_escaped_utf8() {
        let json = r#"{"url":"https://example.test/ü?x=\"quoted\"&y=\\path","count":2}"#;
        let decoded: Contract = decode(json.as_bytes()).unwrap();
        assert_eq!(
            decoded.url.as_str(),
            "https://example.test/ü?x=\"quoted\"&y=\\path"
        );
        let mut out = [0u8; 256];
        let n = serde_json_core::to_slice(&decoded, &mut out).unwrap();
        assert_eq!(decode::<Contract>(&out[..n]).unwrap(), decoded);
    }
    #[test]
    fn malformed_overflow_and_trailing_data_fail() {
        #[derive(serde::Deserialize)]
        struct Tiny {
            #[allow(dead_code)]
            url: ContextString<2>,
        }
        assert!(decode::<Tiny>(br#"{"url":"long"}"#).is_err());
        assert!(decode::<Contract>(br#"{"url":"x","count":1} false"#).is_err());
        assert!(decode::<Contract>(br#"{"url":"x","count":"1"}"#).is_err());
        assert!(decode::<Contract>(&[b' '; MAX_BEHAVIOR_CONTEXT_BYTES + 1]).is_err());
    }
}
