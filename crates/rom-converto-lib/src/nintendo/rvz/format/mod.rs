//! WIA/RVZ binrw structures.
//!
//! Every multi-byte integer in the WIA/RVZ container is big-endian. The
//! struct layouts here mirror the Dolphin spec section-for-section, so
//! sibling modules can parse and write files with a single call.

use binrw::{BinRead, BinWrite};

pub mod sha1;

/// Size of [`WiaFileHead`] when serialised (0x48 bytes).
pub const WIA_FILE_HEAD_SIZE: usize = 0x48;

/// Size of [`WiaDisc`] when serialised (0xDC bytes).
pub const WIA_DISC_SIZE: usize = 0xDC;

/// Size of a serialised [`RvzGroup`] (12 bytes).
pub const RVZ_GROUP_SIZE: usize = 12;

/// Size of a serialised [`WiaRawData`] (24 bytes).
pub const WIA_RAW_DATA_SIZE: usize = 24;

/// Size of a serialised [`WiaPartData`] (16 bytes).
pub const WIA_PART_DATA_SIZE: usize = 16;

/// Size of a serialised [`WiaPart`] (48 bytes).
pub const WIA_PART_SIZE: usize = 48;

/// File-level header. Fixed size, starts at offset 0, fully cleartext.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaFileHead {
    /// Magic bytes: `"WIA\x01"` for both WIA and RVZ.
    pub magic: [u8; 4],
    /// Writer version.
    pub version: u32,
    /// Minimum version required to read this file.
    pub version_compatible: u32,
    /// Size of the following [`WiaDisc`] struct.
    pub disc_size: u32,
    /// SHA-1 of the serialised [`WiaDisc`].
    pub disc_hash: [u8; 20],
    /// Original uncompressed disc size.
    pub iso_file_size: u64,
    /// Size of this WIA/RVZ file on disk.
    pub wia_file_size: u64,
    /// SHA-1 of this struct with `file_head_hash` zeroed.
    pub file_head_hash: [u8; 20],
}

/// Disc-level metadata. Variable size declared by [`WiaFileHead::disc_size`].
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaDisc {
    /// 1 = GameCube, 2 = Wii.
    pub disc_type: u32,
    /// Compression method. Only 5 (Zstandard / RVZ) is implemented.
    pub compression: u32,
    /// Zstandard compression level (signed to allow negative levels).
    pub compr_level: i32,
    /// Chunk size in bytes. Minimum 2 MiB per spec.
    pub chunk_size: u32,
    /// First 0x80 bytes of the underlying disc image.
    pub dhead: [u8; 128],
    /// Number of partition structs that follow.
    pub n_part: u32,
    /// Size of one [`WiaPart`] entry in the partition array.
    pub part_t_size: u32,
    /// File offset to the partition array.
    pub part_off: u64,
    /// SHA-1 of the partition array.
    pub part_hash: [u8; 20],
    /// Number of raw-data descriptors.
    pub n_raw_data: u32,
    /// File offset to the compressed raw-data descriptor table.
    pub raw_data_off: u64,
    /// Compressed size of the raw-data descriptor table.
    pub raw_data_size: u32,
    /// Number of group descriptors.
    pub n_groups: u32,
    /// File offset to the compressed group descriptor table.
    pub group_off: u64,
    /// Compressed size of the group descriptor table.
    pub group_size: u32,
    /// Used length of `compr_data`. Zero for Zstandard.
    pub compr_data_len: u8,
    /// Compressor-specific parameters. Empty for Zstandard.
    pub compr_data: [u8; 7],
}

/// Pointer to a single compressed chunk and its metadata.
#[derive(Debug, Clone, Copy, BinRead, BinWrite)]
#[brw(big)]
pub struct RvzGroup {
    /// File offset of the chunk, divided by 4. Multiply by 4 for real offset.
    pub data_off4: u32,
    /// Raw compressed size in the low 31 bits. MSB set means the chunk is
    /// compressed with zstd; MSB clear means the chunk is stored verbatim.
    pub data_size: u32,
    /// Size of the group data after decompression but before RVZ packing is
    /// decoded. Zero means the group data is not RVZ-packed.
    pub rvz_packed_size: u32,
}

impl RvzGroup {
    const COMPRESSED_FLAG: u32 = 1 << 31;

    pub fn is_compressed(&self) -> bool {
        self.data_size & Self::COMPRESSED_FLAG != 0
    }

    pub fn compressed_size(&self) -> u32 {
        self.data_size & !Self::COMPRESSED_FLAG
    }

    pub fn new_compressed(data_off4: u32, compressed_size: u32, rvz_packed_size: u32) -> Self {
        Self {
            data_off4,
            data_size: compressed_size | Self::COMPRESSED_FLAG,
            rvz_packed_size,
        }
    }

    pub fn new_uncompressed(data_off4: u32, size: u32) -> Self {
        Self {
            data_off4,
            data_size: size,
            rvz_packed_size: 0,
        }
    }
}

/// Descriptor for a contiguous raw (unencrypted) region of the disc.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaRawData {
    /// Byte offset inside the original ISO where this region starts.
    pub raw_data_off: u64,
    /// Length of the region in the original ISO.
    pub raw_data_size: u64,
    /// Index of the first [`RvzGroup`] that stores this region's chunks.
    pub group_index: u32,
    /// Number of groups belonging to this region.
    pub n_groups: u32,
}

/// Sub-range of a Wii partition's decrypted data.
#[derive(Debug, Clone, Copy, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaPartData {
    /// First 0x8000-byte sector of this range within the partition.
    pub first_sector: u32,
    /// Number of sectors covered.
    pub n_sectors: u32,
    /// Index of the first group storing this range.
    pub group_index: u32,
    /// Number of groups belonging to this range.
    pub n_groups: u32,
}

/// Descriptor for a decrypted Wii partition.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaPart {
    /// The partition's decrypted title key.
    pub part_key: [u8; 16],
    /// Two sub-ranges of the partition's decrypted data.
    pub pd: [WiaPartData; 2],
}

/// A single hash exception recording a sector position and the corrected hash.
#[derive(Debug, Clone, Copy, BinRead, BinWrite)]
#[brw(big)]
pub struct WiaException {
    /// Offset inside the sector's hash region where this SHA-1 lives.
    pub offset: u16,
    /// SHA-1 that differs from the reader's recomputed value.
    pub hash: [u8; 20],
}

#[cfg(test)]
mod tests {
    use super::*;
    use binrw::{BinRead, BinWrite, Endian};
    use std::io::Cursor;

    fn roundtrip_be_size<T>(value: &T, expected_size: usize)
    where
        T: BinWrite<Args<'static> = ()> + BinRead<Args<'static> = ()> + std::fmt::Debug,
    {
        let mut buf = Vec::new();
        value
            .write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
            .unwrap();
        assert_eq!(buf.len(), expected_size, "serialised size mismatch");
        let mut cursor = Cursor::new(&buf);
        let _ = T::read_options(&mut cursor, Endian::Big, ()).unwrap();
    }

    #[test]
    fn wia_file_head_size_is_0x48() {
        let head = WiaFileHead {
            magic: *b"WIA\x01",
            version: 0x01020304,
            version_compatible: 0x01020304,
            disc_size: WIA_DISC_SIZE as u32,
            disc_hash: [0u8; 20],
            iso_file_size: 0x1234_5678,
            wia_file_size: 0x2345_6789,
            file_head_hash: [0u8; 20],
        };
        roundtrip_be_size(&head, WIA_FILE_HEAD_SIZE);
    }

    #[test]
    fn wia_disc_size_is_0xdc() {
        let disc = WiaDisc {
            disc_type: 1,
            compression: 5,
            compr_level: 5,
            chunk_size: crate::nintendo::rvz::constants::MIN_CHUNK_SIZE,
            dhead: [0u8; 128],
            n_part: 0,
            part_t_size: WIA_PART_SIZE as u32,
            part_off: 0,
            part_hash: [0u8; 20],
            n_raw_data: 0,
            raw_data_off: 0,
            raw_data_size: 0,
            n_groups: 0,
            group_off: 0,
            group_size: 0,
            compr_data_len: 0,
            compr_data: [0u8; 7],
        };
        roundtrip_be_size(&disc, WIA_DISC_SIZE);
    }

    #[test]
    fn rvz_group_size_is_12() {
        let g = RvzGroup::new_compressed(0x12345678, 0x1000, 0x2000);
        roundtrip_be_size(&g, RVZ_GROUP_SIZE);
    }

    #[test]
    fn rvz_group_compressed_flag_roundtrips() {
        let compressed = RvzGroup::new_compressed(0, 1234, 0);
        assert!(compressed.is_compressed());
        assert_eq!(compressed.compressed_size(), 1234);

        let uncompressed = RvzGroup::new_uncompressed(0, 1234);
        assert!(!uncompressed.is_compressed());
        assert_eq!(uncompressed.compressed_size(), 1234);
    }

    #[test]
    fn wia_raw_data_size_is_24() {
        let raw = WiaRawData {
            raw_data_off: 0,
            raw_data_size: 0,
            group_index: 0,
            n_groups: 0,
        };
        roundtrip_be_size(&raw, WIA_RAW_DATA_SIZE);
    }

    #[test]
    fn wia_part_size_is_48() {
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [WiaPartData {
                first_sector: 0,
                n_sectors: 0,
                group_index: 0,
                n_groups: 0,
            }; 2],
        };
        roundtrip_be_size(&part, WIA_PART_SIZE);
    }

    #[test]
    fn wia_exception_size_is_22() {
        let ex = WiaException {
            offset: 0,
            hash: [0u8; 20],
        };
        let mut buf = Vec::new();
        ex.write(&mut Cursor::new(&mut buf)).unwrap();
        assert_eq!(buf.len(), 22);
    }
}
