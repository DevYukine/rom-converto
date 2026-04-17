//! ZArchive footer (144 bytes, at the tail of every archive).
//!
//! Field order matches the upstream C++ `Footer::Serialize` in
//! `zarchivecommon.h`, which walks the OffsetInfo members in
//! declaration order, then the integrity hash, then total_size,
//! version, and finally magic. The magic u32 is always the last
//! four bytes of the file.

use binrw::{BinRead, BinWrite};

use crate::nintendo::wup::constants::{ZARCHIVE_FOOTER_MAGIC, ZARCHIVE_FOOTER_VERSION};

/// Offset + size pair describing one archive section. Serialised as
/// two big-endian u64s, 16 bytes total. Six of these live inside the
/// [`ZArchiveFooter`].
#[derive(Debug, Clone, Copy, BinRead, BinWrite, Default, PartialEq, Eq)]
#[brw(big)]
pub struct ZArchiveSectionInfo {
    pub offset: u64,
    pub size: u64,
}

impl ZArchiveSectionInfo {
    pub fn new(offset: u64, size: u64) -> Self {
        Self { offset, size }
    }
}

/// 144-byte footer at the tail of every ZArchive. The last 4 bytes of
/// the whole file are [`ZARCHIVE_FOOTER_MAGIC`]; the 4 bytes before
/// them are [`ZARCHIVE_FOOTER_VERSION`]. A reader locates this struct
/// by seeking to `file_size - ZARCHIVE_FOOTER_SIZE` and decoding from
/// there.
#[derive(Debug, Clone, BinRead, BinWrite)]
#[brw(big)]
pub struct ZArchiveFooter {
    /// Zstd-compressed file data. Always starts at offset 0.
    pub section_compressed_data: ZArchiveSectionInfo,
    /// Array of `CompressionOffsetRecord`s mapping block index to
    /// compressed byte offset.
    pub section_offset_records: ZArchiveSectionInfo,
    /// Length-prefixed node name table, shared across files and
    /// directories.
    pub section_names: ZArchiveSectionInfo,
    /// BFS-ordered `FileDirectoryEntry` array describing the virtual
    /// filesystem.
    pub section_file_tree: ZArchiveSectionInfo,
    /// Unused meta directory section. Always size 0 in v1 archives.
    pub section_meta_directory: ZArchiveSectionInfo,
    /// Unused meta data section. Always size 0 in v1 archives.
    pub section_meta_data: ZArchiveSectionInfo,
    /// SHA-256 of the whole archive with this field pre-zeroed during
    /// the hash. Not validated by the upstream reader but computed
    /// anyway for forward compatibility.
    pub integrity_hash: [u8; 32],
    /// Total archive size in bytes. Must equal the actual file length
    /// on disk or the reader rejects the archive.
    pub total_size: u64,
    /// Always [`ZARCHIVE_FOOTER_VERSION`].
    pub version: u32,
    /// Always [`ZARCHIVE_FOOTER_MAGIC`]. Last four bytes of the file.
    pub magic: u32,
}

impl ZArchiveFooter {
    /// Build a new footer with the magic and version fields set,
    /// integrity hash zeroed, and caller-supplied section bounds plus
    /// total archive size.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        section_compressed_data: ZArchiveSectionInfo,
        section_offset_records: ZArchiveSectionInfo,
        section_names: ZArchiveSectionInfo,
        section_file_tree: ZArchiveSectionInfo,
        section_meta_directory: ZArchiveSectionInfo,
        section_meta_data: ZArchiveSectionInfo,
        total_size: u64,
    ) -> Self {
        Self {
            section_compressed_data,
            section_offset_records,
            section_names,
            section_file_tree,
            section_meta_directory,
            section_meta_data,
            integrity_hash: [0u8; 32],
            total_size,
            version: ZARCHIVE_FOOTER_VERSION,
            magic: ZARCHIVE_FOOTER_MAGIC,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::constants::ZARCHIVE_FOOTER_SIZE;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    fn empty_footer() -> ZArchiveFooter {
        ZArchiveFooter::new(
            ZArchiveSectionInfo::default(),
            ZArchiveSectionInfo::default(),
            ZArchiveSectionInfo::default(),
            ZArchiveSectionInfo::default(),
            ZArchiveSectionInfo::default(),
            ZArchiveSectionInfo::default(),
            ZARCHIVE_FOOTER_SIZE as u64,
        )
    }

    #[test]
    fn section_info_serialises_to_16_bytes() {
        let info = ZArchiveSectionInfo::new(0x1234_5678, 0xAABB_CCDD);
        let mut buf = Cursor::new(Vec::new());
        info.write(&mut buf).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[0..8], &0x1234_5678u64.to_be_bytes());
        assert_eq!(&bytes[8..16], &0xAABB_CCDDu64.to_be_bytes());
    }

    #[test]
    fn footer_serialises_to_144_bytes() {
        let footer = empty_footer();
        let mut buf = Cursor::new(Vec::new());
        footer.write(&mut buf).unwrap();
        assert_eq!(buf.into_inner().len(), ZARCHIVE_FOOTER_SIZE);
    }

    #[test]
    fn footer_magic_is_last_four_bytes() {
        let footer = empty_footer();
        let mut buf = Cursor::new(Vec::new());
        footer.write(&mut buf).unwrap();
        let bytes = buf.into_inner();
        let tail = &bytes[bytes.len() - 4..];
        assert_eq!(
            tail,
            &ZARCHIVE_FOOTER_MAGIC.to_be_bytes(),
            "archive magic must be the trailing 4 bytes"
        );
    }

    #[test]
    fn footer_version_precedes_magic() {
        let footer = empty_footer();
        let mut buf = Cursor::new(Vec::new());
        footer.write(&mut buf).unwrap();
        let bytes = buf.into_inner();
        let version_slice = &bytes[bytes.len() - 8..bytes.len() - 4];
        assert_eq!(version_slice, &ZARCHIVE_FOOTER_VERSION.to_be_bytes());
    }

    #[test]
    fn footer_round_trip_preserves_every_field() {
        let mut hash = [0u8; 32];
        for (i, b) in hash.iter_mut().enumerate() {
            *b = (i * 7 + 3) as u8;
        }
        let original = ZArchiveFooter {
            section_compressed_data: ZArchiveSectionInfo::new(0, 0x1_0000),
            section_offset_records: ZArchiveSectionInfo::new(0x1_0000, 40),
            section_names: ZArchiveSectionInfo::new(0x1_0028, 16),
            section_file_tree: ZArchiveSectionInfo::new(0x1_0038, 32),
            section_meta_directory: ZArchiveSectionInfo::new(0x1_0058, 0),
            section_meta_data: ZArchiveSectionInfo::new(0x1_0058, 0),
            integrity_hash: hash,
            total_size: 0x1_0058 + ZARCHIVE_FOOTER_SIZE as u64,
            version: ZARCHIVE_FOOTER_VERSION,
            magic: ZARCHIVE_FOOTER_MAGIC,
        };
        let mut buf = Cursor::new(Vec::new());
        original.write(&mut buf).unwrap();
        let bytes = buf.into_inner();

        let mut reader = Cursor::new(&bytes);
        let parsed = ZArchiveFooter::read(&mut reader).unwrap();

        assert_eq!(
            parsed.section_compressed_data,
            original.section_compressed_data
        );
        assert_eq!(
            parsed.section_offset_records,
            original.section_offset_records
        );
        assert_eq!(parsed.section_names, original.section_names);
        assert_eq!(parsed.section_file_tree, original.section_file_tree);
        assert_eq!(
            parsed.section_meta_directory,
            original.section_meta_directory
        );
        assert_eq!(parsed.section_meta_data, original.section_meta_data);
        assert_eq!(parsed.integrity_hash, hash);
        assert_eq!(parsed.total_size, original.total_size);
        assert_eq!(parsed.version, ZARCHIVE_FOOTER_VERSION);
        assert_eq!(parsed.magic, ZARCHIVE_FOOTER_MAGIC);
    }

    #[test]
    fn footer_sections_serialise_in_declaration_order() {
        // The upstream writer serialises sectionCompressedData first,
        // then sectionOffsetRecords, etc. A caller that mixes them up
        // would still produce a 144-byte blob but the byte order
        // would be wrong and every reader would reject it.
        let footer = ZArchiveFooter {
            section_compressed_data: ZArchiveSectionInfo::new(0x1111, 0x1112),
            section_offset_records: ZArchiveSectionInfo::new(0x2221, 0x2222),
            section_names: ZArchiveSectionInfo::new(0x3331, 0x3332),
            section_file_tree: ZArchiveSectionInfo::new(0x4441, 0x4442),
            section_meta_directory: ZArchiveSectionInfo::new(0x5551, 0x5552),
            section_meta_data: ZArchiveSectionInfo::new(0x6661, 0x6662),
            integrity_hash: [0u8; 32],
            total_size: 0,
            version: ZARCHIVE_FOOTER_VERSION,
            magic: ZARCHIVE_FOOTER_MAGIC,
        };
        let mut buf = Cursor::new(Vec::new());
        footer.write(&mut buf).unwrap();
        let bytes = buf.into_inner();
        assert_eq!(&bytes[0..8], &0x1111u64.to_be_bytes());
        assert_eq!(&bytes[8..16], &0x1112u64.to_be_bytes());
        assert_eq!(&bytes[16..24], &0x2221u64.to_be_bytes());
        assert_eq!(&bytes[24..32], &0x2222u64.to_be_bytes());
        assert_eq!(&bytes[80..88], &0x6661u64.to_be_bytes());
        assert_eq!(&bytes[88..96], &0x6662u64.to_be_bytes());
    }
}
