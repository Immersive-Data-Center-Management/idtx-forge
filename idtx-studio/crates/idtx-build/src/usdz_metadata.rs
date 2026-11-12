//! Normalize only the ZIP clock fields of freshly cooked `OpenUSD` archives.
//! Entry order, offsets, 64-byte USDZ padding, CRCs and payload bytes are intact.
use std::{fs::OpenOptions, io::{self, Read, Seek, SeekFrom, Write}, path::Path};

fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid classic USDZ archive metadata") }
fn u16_at(bytes: &[u8], at: usize) -> u16 { u16::from_le_bytes([bytes[at], bytes[at + 1]]) }
fn u32_at(bytes: &[u8], at: usize) -> u32 { u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) }

/// SdfZipFileWriter in the pinned `OpenUSD` 26.11.1 produces classic single-disk
/// ZIP, not ZIP64. Reject other formats rather than changing ambiguous offsets.
pub(crate) fn normalize(path: &Path) -> Result<(), crate::CookError> {
    normalize_inner(path).map_err(|source| crate::CookError::Io { path: path.to_owned(), source })
}
fn normalize_inner(path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    if length < 22 { return Err(invalid()); }
    let tail_len = length.min(65557) as usize;
    file.seek(SeekFrom::Start(length - tail_len as u64))?;
    let mut tail = vec![0u8; tail_len]; file.read_exact(&mut tail)?;
    let end = (0..=tail_len - 22).rev().find(|&i| tail[i..i + 4] == *b"PK\x05\x06"
        && i + 22 + usize::from(u16_at(&tail, i + 20)) == tail_len).ok_or_else(invalid)?;
    if u16_at(&tail, end + 4) != 0 || u16_at(&tail, end + 6) != 0 { return Err(invalid()); }
    let entries = u16_at(&tail, end + 10);
    let offset = u32_at(&tail, end + 16);
    let size = u32_at(&tail, end + 12);
    if u16_at(&tail, end + 8) != entries || offset == u32::MAX || size == u32::MAX { return Err(invalid()); }
    let central_start = u64::from(offset);
    let central_end = central_start.checked_add(u64::from(size)).ok_or_else(invalid)?;
    if central_end > length - tail_len as u64 + end as u64 { return Err(invalid()); }
    let mut cursor = central_start;
    const CLOCK: [u8; 4] = [0, 0, 33, 0]; // 00:00:00, 1980-01-01.
    for _ in 0..entries {
        if cursor + 46 > central_end { return Err(invalid()); }
        file.seek(SeekFrom::Start(cursor))?;
        let mut header = [0u8; 46]; file.read_exact(&mut header)?;
        if header[..4] != *b"PK\x01\x02" || u16_at(&header, 34) != 0 { return Err(invalid()); }
        let local = u64::from(u32_at(&header, 42));
        if local + 30 > central_start { return Err(invalid()); }
        file.seek(SeekFrom::Start(local))?;
        let mut local_header = [0u8; 30]; file.read_exact(&mut local_header)?;
        if local_header[..4] != *b"PK\x03\x04" || local_header[14..18] != header[16..20] { return Err(invalid()); }
        file.seek(SeekFrom::Start(local + 10))?; file.write_all(&CLOCK)?;
        file.seek(SeekFrom::Start(cursor + 12))?; file.write_all(&CLOCK)?;
        cursor += 46 + u64::from(u16_at(&header, 28)) + u64::from(u16_at(&header, 30)) + u64::from(u16_at(&header, 32));
    }
    if cursor != central_end { return Err(invalid()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_metadata_fails_without_a_panic() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for bytes in [vec![], vec![0; 100], b"PK\x05\x06".to_vec()] {
            std::fs::write(file.path(), bytes).unwrap();
            assert!(normalize_inner(file.path()).is_err());
        }
    }
}
