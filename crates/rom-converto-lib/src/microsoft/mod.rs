//! Microsoft platforms: Original Xbox ([`xbox`]) and Xbox 360 ([`xenon`]),
//! plus the shared XDVDFS disc filesystem ([`xdvdfs`]).

pub mod xbox;
pub mod xdvdfs;
pub mod xenon;
pub mod xex;

use crate::util::extent_end;

/// Reads `size` bytes at `base + offset` from a seekable source, refusing
/// ranges that would cross the declared `extent`.
pub(crate) fn read_extent_at<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    base: u64,
    extent: u64,
    offset: u64,
    size: usize,
) -> Option<Vec<u8>> {
    use std::io::SeekFrom;
    extent_end(offset, size as u64, extent)?;
    reader
        .seek(SeekFrom::Start(base.checked_add(offset)?))
        .ok()?;
    let mut bytes = vec![0u8; size];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes)
}
