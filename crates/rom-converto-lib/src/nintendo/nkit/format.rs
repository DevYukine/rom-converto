//! NKit native format structures (`.nkit.iso` / `.nkit.gcz`).
//!
//! Spec source: NKit 1.4 C# sources (`NkitWriterGc.cs`,
//! `NkitReaderGc.cs`, `Gaps.cs`, mirrored at
//! `github.com/extremscorner/nkit`). NKit embeds its header in the
//! reserved area of Boot.bin at 0x200; all integers are big-endian:
//!
//! ```text
//! 0x200  "NKIT"      magic
//! 0x204  " v01"      version (the full string check is "NKIT v01")
//! 0x208  u32         CRC32 of the original source ISO
//! 0x20C  u32         CRC fix-up making the whole nkit file's CRC32
//!                    equal the source CRC32
//! 0x210  u32         original image size (GC: bytes, Wii: bytes / 4)
//! 0x214  [u8; 4]     forced junk ID, or NULs when the disc's own ID
//!                    seeds the junk stream
//! 0x218  u32         CRC32 of a removed Wii update partition
//! ```
//!
//! Restoring zeroes the whole 0x200..0x21C region.

use super::error::{NkitError, NkitResult};

pub const NKIT_HEADER_OFFSET: usize = 0x200;
pub const NKIT_HEADER_END: usize = 0x21C;
pub const NKIT_MAGIC_VERSION: &[u8; 8] = b"NKIT v01";
/// GameCube boot header fields used by the FST walk.
pub const GC_FST_OFFSET_FIELD: usize = 0x424;
pub const GC_FST_SIZE_FIELD: usize = 0x428;
/// Standard full GameCube disc size (`NStream.FullSizeGameCube`).
pub const GC_FULL_SIZE: u64 = 0x5705_8000;

#[derive(Debug, Clone, Copy)]
pub struct NkitHeader {
    pub source_crc: u32,
    pub crc_fixup: u32,
    pub image_size: u64,
    pub forced_junk_id: Option<[u8; 4]>,
    pub update_partition_crc: u32,
    pub is_wii: bool,
}

impl NkitHeader {
    /// Parse from the first disc-header bytes (at least 0x440).
    pub fn parse(dhead: &[u8]) -> NkitResult<Self> {
        if dhead.len() < 0x440 {
            return Err(NkitError::InvalidHeader(
                "disc header shorter than 0x440 bytes".into(),
            ));
        }
        if &dhead[0x200..0x208] != NKIT_MAGIC_VERSION {
            return Err(NkitError::InvalidHeader(format!(
                "unsupported NKit magic/version {:02X?}",
                &dhead[0x200..0x208]
            )));
        }
        let be32 = |off: usize| u32::from_be_bytes(dhead[off..off + 4].try_into().unwrap());
        let is_wii = crate::nintendo::rvl::is_wii(dhead[..0x80].try_into().unwrap());
        let raw_size = be32(0x210) as u64;
        let junk = &dhead[0x214..0x218];
        Ok(Self {
            source_crc: be32(0x208),
            crc_fixup: be32(0x20C),
            image_size: if is_wii { raw_size * 4 } else { raw_size },
            forced_junk_id: if junk == [0u8; 4] {
                None
            } else {
                Some(junk.try_into().unwrap())
            },
            update_partition_crc: be32(0x218),
            is_wii,
        })
    }

    /// Junk stream identity for this image: the forced ID when set,
    /// the disc's own ID otherwise, plus the disc number byte.
    pub fn junk_identity(&self, dhead: &[u8]) -> ([u8; 4], u8) {
        let id = self
            .forced_junk_id
            .unwrap_or_else(|| dhead[..4].try_into().unwrap());
        (id, dhead[6])
    }
}

/// Clear the NKit header region, restoring Boot.bin to its original
/// (reserved, zero) state.
pub fn clear_nkit_header(dhead: &mut [u8]) {
    dhead[NKIT_HEADER_OFFSET..NKIT_HEADER_END].fill(0);
}

/// One file entry of a GameCube FST, with enough context to patch the
/// entry in a reconstructed image.
#[derive(Debug, Clone, Copy)]
pub struct FstFile {
    /// Byte offset of this entry inside fst.bin.
    pub entry_offset: usize,
    pub data_offset: u64,
    pub size: u32,
}

/// Parse the FILE entries of a GameCube FST.
pub fn parse_gc_fst(fst: &[u8]) -> NkitResult<Vec<FstFile>> {
    if fst.len() < 12 {
        return Err(NkitError::InvalidHeader(
            "FST shorter than one entry".into(),
        ));
    }
    let n_entries = u32::from_be_bytes(fst[8..12].try_into().unwrap()) as usize;
    if n_entries == 0 || n_entries * 12 > fst.len() {
        return Err(NkitError::InvalidHeader(format!(
            "FST declares {n_entries} entries but is {} bytes",
            fst.len()
        )));
    }
    let mut files = Vec::new();
    for i in 1..n_entries {
        let e = &fst[i * 12..i * 12 + 12];
        if e[0] != 0 {
            continue;
        }
        files.push(FstFile {
            entry_offset: i * 12,
            data_offset: u32::from_be_bytes(e[4..8].try_into().unwrap()) as u64,
            size: u32::from_be_bytes(e[8..12].try_into().unwrap()),
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dhead_with_nkit() -> Vec<u8> {
        let mut d = vec![0u8; 0x440];
        d[..4].copy_from_slice(b"GALE");
        d[6] = 0;
        d[0x1C..0x20].copy_from_slice(&0xC233_9F3Du32.to_be_bytes());
        d[0x200..0x208].copy_from_slice(NKIT_MAGIC_VERSION);
        d[0x208..0x20C].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        d[0x20C..0x210].copy_from_slice(&0x9ABC_DEF0u32.to_be_bytes());
        d[0x210..0x214].copy_from_slice(&0x0040_0000u32.to_be_bytes());
        d
    }

    #[test]
    fn header_parses_gc_fields() {
        let h = NkitHeader::parse(&dhead_with_nkit()).unwrap();
        assert_eq!(h.source_crc, 0x1234_5678);
        assert_eq!(h.crc_fixup, 0x9ABC_DEF0);
        assert_eq!(h.image_size, 0x40_0000);
        assert!(h.forced_junk_id.is_none());
        assert!(!h.is_wii);
        let (id, disc) = h.junk_identity(&dhead_with_nkit());
        assert_eq!(&id, b"GALE");
        assert_eq!(disc, 0);
    }

    #[test]
    fn forced_junk_id_overrides_disc_id() {
        let mut d = dhead_with_nkit();
        d[0x214..0x218].copy_from_slice(b"RMCE");
        let h = NkitHeader::parse(&d).unwrap();
        let (id, _) = h.junk_identity(&d);
        assert_eq!(&id, b"RMCE");
    }

    #[test]
    fn fst_parser_skips_directories() {
        let mut fst = vec![0u8; 12 * 3 + 8];
        fst[8..12].copy_from_slice(&3u32.to_be_bytes());
        // Entry 1: directory.
        fst[12] = 1;
        // Entry 2: file at 0x8000, size 0x100.
        fst[24] = 0;
        fst[28..32].copy_from_slice(&0x8000u32.to_be_bytes());
        fst[32..36].copy_from_slice(&0x100u32.to_be_bytes());
        let files = parse_gc_fst(&fst).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].entry_offset, 24);
        assert_eq!(files[0].data_offset, 0x8000);
        assert_eq!(files[0].size, 0x100);
    }
}
