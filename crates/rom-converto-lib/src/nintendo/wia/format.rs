//! WIA-only structures. The container is shared with RVZ, so
//! [`WiaFileHead`], [`WiaDisc`], [`WiaPart`], and friends come from
//! [`crate::nintendo::rvz::format`]; only the 8-byte group descriptor
//! differs (RVZ adds `rvz_packed_size` for a 12-byte entry).
//!
//! [`WiaFileHead`]: crate::nintendo::rvz::format::WiaFileHead
//! [`WiaDisc`]: crate::nintendo::rvz::format::WiaDisc
//! [`WiaPart`]: crate::nintendo::rvz::format::WiaPart

use binrw::{BinRead, BinWrite};

use super::error::{WiaError, WiaResult};
use crate::nintendo::rvz::format::WiaDisc;

pub const WIA_MAGIC: [u8; 4] = [b'W', b'I', b'A', 0x01];
/// Writer version Dolphin and wit emit.
pub const WIA_VERSION: u32 = 0x0100_0000;
/// Oldest file version this reader accepts (matches Dolphin's
/// `WIA_VERSION_READ_COMPATIBLE`).
pub const WIA_VERSION_READ_COMPATIBLE: u32 = 0x0008_0000;
/// Size of a serialized [`WiaGroup`] (8 bytes).
pub const WIA_GROUP_SIZE: usize = 8;
/// WIA chunk sizes must be a multiple of 2 MiB.
pub const WIA_CHUNK_GRANULARITY: u32 = 0x20_0000;

/// Compression methods a WIA file may declare. Zstandard (5) is
/// RVZ-only and rejected here.
pub const WIA_COMPR_NONE: u32 = 0;
pub const WIA_COMPR_PURGE: u32 = 1;
pub const WIA_COMPR_BZIP2: u32 = 2;
pub const WIA_COMPR_LZMA: u32 = 3;
pub const WIA_COMPR_LZMA2: u32 = 4;

/// WIA group descriptor: file offset (divided by 4) and stored size of
/// one chunk. A size of zero means the whole group is zeroes.
#[derive(Debug, Clone, Copy, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaGroup {
    pub data_off4: u32,
    pub data_size: u32,
}

impl WiaGroup {
    pub fn data_offset(&self) -> u64 {
        (self.data_off4 as u64) << 2
    }
}

/// Header-level validation shared by the reader and the verify pass.
pub fn validate_disc(disc: &WiaDisc) -> WiaResult<()> {
    if disc.disc_type != 1 && disc.disc_type != 2 {
        return Err(WiaError::InvalidHeader(format!(
            "unsupported disc type {}",
            disc.disc_type
        )));
    }
    if disc.compression > WIA_COMPR_LZMA2 {
        return Err(WiaError::UnsupportedCompression(disc.compression));
    }
    if disc.chunk_size == 0 || !disc.chunk_size.is_multiple_of(WIA_CHUNK_GRANULARITY) {
        return Err(WiaError::InvalidHeader(format!(
            "chunk size {:#x} is not a multiple of 2 MiB",
            disc.chunk_size
        )));
    }
    Ok(())
}

/// Number of exception lists preceding each Wii partition group.
pub fn exception_lists_per_group(chunk_size: u32) -> usize {
    (chunk_size / WIA_CHUNK_GRANULARITY).max(1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::io::Cursor;

    #[test]
    fn wia_group_is_8_bytes() {
        let g = WiaGroup {
            data_off4: 0x100,
            data_size: 0x4000,
        };
        let mut buf = Cursor::new(Vec::new());
        g.write(&mut buf).unwrap();
        assert_eq!(buf.get_ref().len(), WIA_GROUP_SIZE);
        assert_eq!(g.data_offset(), 0x400);
    }

    #[test]
    fn exception_list_count_follows_chunk_size() {
        assert_eq!(exception_lists_per_group(0x20_0000), 1);
        assert_eq!(exception_lists_per_group(0x280_0000), 20);
    }
}
