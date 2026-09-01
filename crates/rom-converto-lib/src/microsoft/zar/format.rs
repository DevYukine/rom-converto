//! On-disk structures of the ZArchive (`.zar`) container.
//!
//! Every multi-byte integer in the container is big-endian,
//! unconditionally. The only raw-byte fields are the footer integrity
//! hash, the name-table bytes, and the compressed block payloads.
//!
//! Layout, in write order: compressed blocks from offset 0, zero pad to
//! an 8-byte boundary, compression offset records, name table, file
//! tree, two zero-length meta sections, then the 144-byte footer.

use thiserror::Error;

/// Logical size of one compression block.
pub const COMPRESSED_BLOCK_SIZE: usize = 65536;

/// Block size slots per [`CompressionOffsetRecord`].
pub const ENTRIES_PER_OFFSET_RECORD: usize = 16;

/// Footer magic, stored at footer offset 140.
pub const FOOTER_MAGIC: u32 = 0x169F_52D6;

/// The only version ZArchive has ever emitted.
pub const FOOTER_VERSION: u32 = 0x61BF_3A01;

/// Serialized footer size.
pub const FOOTER_SIZE: usize = 144;

/// Serialized [`CompressionOffsetRecord`] size.
pub const OFFSET_RECORD_SIZE: usize = 40;

/// Serialized [`FileDirectoryEntry`] size.
pub const FILE_DIRECTORY_ENTRY_SIZE: usize = 16;

/// Name-table offset stored for the root node, which has no name.
pub const ROOT_NAME_OFFSET_SENTINEL: u32 = 0x7FFF_FFFF;

/// File offsets and sizes are split across a 32-bit low word and a
/// 16-bit high half, so both cap out at 48 bits.
pub const MAX_FILE_EXTENT: u64 = 0xFFFF_FFFF_FFFF;

/// Longest name this writer emits. See [`ZarError::NameTooLong`].
pub const MAX_NAME_LEN: usize = 128;

/// Errors raised while reading or writing a ZArchive.
#[derive(Debug, Error)]
pub enum ZarError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("zstd error: {0}")]
    Zstd(String),

    #[error("invalid ZArchive magic: got {0:#010x}")]
    BadMagic(u32),

    #[error("unsupported ZArchive version: {0:#010x}")]
    BadVersion(u32),

    #[error("footer total size {declared} does not match the {actual}-byte file")]
    SizeMismatch { declared: u64, actual: u64 },

    #[error("{name} section at {offset}+{size} extends past the end of the archive")]
    SectionOutOfBounds {
        name: &'static str,
        offset: u64,
        size: u64,
    },

    #[error("invalid {name} section length {len}")]
    BadSectionLength { name: &'static str, len: u64 },

    #[error("archive is {0} bytes, smaller than the footer")]
    TooSmall(u64),

    #[error("integrity hash mismatch")]
    HashMismatch,

    // The reference reader (`zarchivereader.cpp:370`) mis-decodes
    // two-byte name lengths, so names must stay in the one-byte form.
    #[error("entry name is {0} bytes; ZArchive readers only handle names below 128 bytes")]
    NameTooLong(usize),

    #[error("name table exceeds the 31-bit offset field")]
    NameTableTooLarge,

    #[error("name table offset {0} is out of bounds")]
    BadNameOffset(u32),

    #[error("file tree node {0} is out of bounds")]
    BadNodeIndex(u32),

    #[error("block {0} is out of bounds")]
    BadBlockIndex(u64),

    #[error("block decompressed to {actual} bytes, expected {COMPRESSED_BLOCK_SIZE}")]
    BadBlockSize { actual: usize },

    #[error("no entry for path {0:?}")]
    NotFound(String),

    #[error("invalid archive path {0:?}")]
    InvalidPath(String),

    #[error("path {0:?} traverses a file")]
    PathThroughFile(String),

    #[error("node {0} is a directory, not a file")]
    NotAFile(u32),

    #[error("path {0:?} already exists in the archive")]
    DuplicateEntry(String),

    #[error("no file is open; call start_file first")]
    NoActiveFile,

    #[error("file is {0} bytes; ZArchive entries are limited to 48-bit sizes")]
    FileTooLarge(u64),

    #[error("worker pool channel closed")]
    WorkerPool,

    #[error("corrupt archive structure: {0}")]
    CorruptStructure(String),

    #[error("operation cancelled")]
    Cancelled,
}

impl From<crate::util::worker_pool::PoolChannelClosed> for ZarError {
    fn from(_: crate::util::worker_pool::PoolChannelClosed) -> Self {
        ZarError::WorkerPool
    }
}

/// Convenience alias for a [`Result`] with [`ZarError`].
pub type ZarResult<T> = Result<T, ZarError>;

/// One `{offset, size}` pair from the footer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Section {
    pub offset: u64,
    pub size: u64,
}

/// The 144-byte trailer at `fileSize - 144`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Footer {
    pub compressed_data: Section,
    pub offset_records: Section,
    pub names: Section,
    pub file_tree: Section,
    pub meta_directory: Section,
    pub meta_data: Section,
    pub integrity_hash: [u8; 32],
    /// Size of the whole file, footer included.
    pub total_size: u64,
}

impl Footer {
    /// The six sections in their serialized order, paired with the
    /// names used in bounds-check errors.
    pub fn sections(&self) -> [(&'static str, Section); 6] {
        [
            ("compressed data", self.compressed_data),
            ("offset records", self.offset_records),
            ("names", self.names),
            ("file tree", self.file_tree),
            ("meta directory", self.meta_directory),
            ("meta data", self.meta_data),
        ]
    }

    /// Serialize to the fixed on-disk form.
    pub fn to_bytes(&self) -> [u8; FOOTER_SIZE] {
        let mut out = [0u8; FOOTER_SIZE];
        for (i, (_, s)) in self.sections().iter().enumerate() {
            out[i * 16..i * 16 + 8].copy_from_slice(&s.offset.to_be_bytes());
            out[i * 16 + 8..i * 16 + 16].copy_from_slice(&s.size.to_be_bytes());
        }
        out[96..128].copy_from_slice(&self.integrity_hash);
        out[128..136].copy_from_slice(&self.total_size.to_be_bytes());
        out[136..140].copy_from_slice(&FOOTER_VERSION.to_be_bytes());
        out[140..144].copy_from_slice(&FOOTER_MAGIC.to_be_bytes());
        out
    }

    /// Parse a footer, validating magic and version. Section bounds are
    /// checked by the reader, which knows the file size.
    pub fn from_bytes(bytes: &[u8; FOOTER_SIZE]) -> ZarResult<Self> {
        let magic = be_u32(&bytes[140..144]);
        if magic != FOOTER_MAGIC {
            return Err(ZarError::BadMagic(magic));
        }
        let version = be_u32(&bytes[136..140]);
        if version != FOOTER_VERSION {
            return Err(ZarError::BadVersion(version));
        }
        let mut s = [Section::default(); 6];
        for (i, sec) in s.iter_mut().enumerate() {
            sec.offset = be_u64(&bytes[i * 16..i * 16 + 8]);
            sec.size = be_u64(&bytes[i * 16 + 8..i * 16 + 16]);
        }
        let mut integrity_hash = [0u8; 32];
        integrity_hash.copy_from_slice(&bytes[96..128]);
        Ok(Self {
            compressed_data: s[0],
            offset_records: s[1],
            names: s[2],
            file_tree: s[3],
            meta_directory: s[4],
            meta_data: s[5],
            integrity_hash,
            total_size: be_u64(&bytes[128..136]),
        })
    }
}

/// Offsets for a run of up to 16 consecutive blocks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompressionOffsetRecord {
    /// Position of the first block of this run, relative to the start
    /// of the compressed-data section.
    pub base_offset: u64,
    /// Each entry stores `compressed_size - 1`, so a full 65536-byte
    /// stored-raw block still fits in a `u16` (`0xFFFF`).
    pub sizes: [u16; ENTRIES_PER_OFFSET_RECORD],
}

impl CompressionOffsetRecord {
    /// Serialize to the fixed 40-byte on-disk form.
    pub fn to_bytes(&self) -> [u8; OFFSET_RECORD_SIZE] {
        let mut out = [0u8; OFFSET_RECORD_SIZE];
        out[0..8].copy_from_slice(&self.base_offset.to_be_bytes());
        for (i, s) in self.sizes.iter().enumerate() {
            out[8 + i * 2..10 + i * 2].copy_from_slice(&s.to_be_bytes());
        }
        out
    }

    /// Parse one record.
    pub fn from_bytes(bytes: &[u8; OFFSET_RECORD_SIZE]) -> Self {
        let mut sizes = [0u16; ENTRIES_PER_OFFSET_RECORD];
        for (i, s) in sizes.iter_mut().enumerate() {
            *s = be_u16(&bytes[8 + i * 2..10 + i * 2]);
        }
        Self {
            base_offset: be_u64(&bytes[0..8]),
            sizes,
        }
    }
}

/// One node of the flat file tree. Node 0 is the root directory.
///
/// Files and directories share the same three-word body, so
/// serialization never branches on the node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDirectoryEntry {
    name_offset_and_type_flag: u32,
    /// File offset low word, or a directory's first child index.
    word1: u32,
    /// File size low word, or a directory's child count.
    word2: u32,
    /// File offset and size high halves, or directory padding.
    word3: u32,
}

impl FileDirectoryEntry {
    /// Build a file entry. Fails if the 48-bit offset or size fields
    /// cannot hold `offset` / `size`.
    pub fn file(name_offset: u32, offset: u64, size: u64) -> ZarResult<Self> {
        if offset > MAX_FILE_EXTENT || size > MAX_FILE_EXTENT {
            return Err(ZarError::FileTooLarge(size.max(offset)));
        }
        Ok(Self {
            name_offset_and_type_flag: name_offset | 0x8000_0000,
            word1: offset as u32,
            word2: size as u32,
            word3: ((offset >> 32) as u32 & 0xFFFF) | (((size >> 32) as u32 & 0xFFFF) << 16),
        })
    }

    /// Build a directory entry covering children
    /// `[node_start_index, node_start_index + count)`.
    pub fn directory(name_offset: u32, node_start_index: u32, count: u32) -> Self {
        Self {
            name_offset_and_type_flag: name_offset,
            word1: node_start_index,
            word2: count,
            word3: 0,
        }
    }

    /// True if this node is a file; false if it is a directory.
    pub fn is_file(&self) -> bool {
        self.name_offset_and_type_flag & 0x8000_0000 != 0
    }

    /// Byte offset of this entry's name in the name table.
    pub fn name_offset(&self) -> u32 {
        self.name_offset_and_type_flag & 0x7FFF_FFFF
    }

    /// Offset of the file in the logical concatenated data stream.
    pub fn file_offset(&self) -> u64 {
        self.word1 as u64 | ((self.word3 as u64 & 0xFFFF) << 32)
    }

    /// The file's logical size in bytes.
    pub fn file_size(&self) -> u64 {
        self.word2 as u64 | ((self.word3 as u64 & 0xFFFF_0000) << 16)
    }

    /// Index of this directory's first child.
    pub fn node_start_index(&self) -> u32 {
        self.word1
    }

    /// Number of children in this directory.
    pub fn count(&self) -> u32 {
        self.word2
    }

    /// Serialize to the fixed 16-byte on-disk form.
    pub fn to_bytes(&self) -> [u8; FILE_DIRECTORY_ENTRY_SIZE] {
        let mut out = [0u8; FILE_DIRECTORY_ENTRY_SIZE];
        out[0..4].copy_from_slice(&self.name_offset_and_type_flag.to_be_bytes());
        out[4..8].copy_from_slice(&self.word1.to_be_bytes());
        out[8..12].copy_from_slice(&self.word2.to_be_bytes());
        out[12..16].copy_from_slice(&self.word3.to_be_bytes());
        out
    }

    /// Parse one entry.
    pub fn from_bytes(bytes: &[u8; FILE_DIRECTORY_ENTRY_SIZE]) -> Self {
        Self {
            name_offset_and_type_flag: be_u32(&bytes[0..4]),
            word1: be_u32(&bytes[4..8]),
            word2: be_u32(&bytes[8..12]),
            word3: be_u32(&bytes[12..16]),
        }
    }
}

/// Append a name-table length prefix. Lengths below `0x80` take one
/// byte; longer names use the 15-bit two-byte form.
pub fn encode_name_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        out.push(((len & 0x7F) | 0x80) as u8);
        out.push((len >> 7) as u8);
    }
}

/// Decode the length prefix at `offset`, returning the name length and
/// the number of prefix bytes consumed. Handles the two-byte form so
/// archives from other producers still parse.
pub fn decode_name_len(table: &[u8], offset: usize) -> ZarResult<(usize, usize)> {
    let bad = || ZarError::BadNameOffset(offset as u32);
    let b0 = *table.get(offset).ok_or_else(bad)?;
    if b0 & 0x80 == 0 {
        return Ok((b0 as usize, 1));
    }
    let b1 = *table.get(offset + 1).ok_or_else(bad)?;
    Ok((((b0 & 0x7F) as usize) | ((b1 as usize) << 7), 2))
}

/// Split an archive path on either separator, dropping empty
/// components so leading and doubled separators are ignored.
pub fn split_path(path: &str) -> impl Iterator<Item = &str> {
    path.split(['/', '\\']).filter(|c| !c.is_empty())
}

fn be_u16(b: &[u8]) -> u16 {
    u16::from_be_bytes(b.try_into().expect("2-byte slice"))
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().expect("4-byte slice"))
}

fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b.try_into().expect("8-byte slice"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_footer() -> Footer {
        Footer {
            compressed_data: Section {
                offset: 0,
                size: 0x1234,
            },
            offset_records: Section {
                offset: 0x1238,
                size: 40,
            },
            names: Section {
                offset: 0x1260,
                size: 12,
            },
            file_tree: Section {
                offset: 0x126C,
                size: 32,
            },
            meta_directory: Section {
                offset: 0x128C,
                size: 0,
            },
            meta_data: Section {
                offset: 0x128C,
                size: 0,
            },
            integrity_hash: [0xAB; 32],
            total_size: 0x128C + FOOTER_SIZE as u64,
        }
    }

    #[test]
    fn footer_round_trips_through_bytes() {
        let footer = sample_footer();
        let bytes = footer.to_bytes();
        let parsed = Footer::from_bytes(&bytes).expect("footer parses");
        assert_eq!(parsed, footer);
        assert_eq!(parsed.to_bytes(), bytes);
    }

    #[test]
    fn footer_fields_land_at_spec_offsets() {
        let bytes = sample_footer().to_bytes();
        assert_eq!(be_u64(&bytes[16..24]), 0x1238);
        assert_eq!(be_u64(&bytes[24..32]), 40);
        assert_eq!(&bytes[96..128], &[0xABu8; 32]);
        assert_eq!(be_u64(&bytes[128..136]), 0x128C + FOOTER_SIZE as u64);
        assert_eq!(be_u32(&bytes[136..140]), FOOTER_VERSION);
        assert_eq!(be_u32(&bytes[140..144]), FOOTER_MAGIC);
    }

    #[test]
    fn footer_rejects_bad_magic_and_version() {
        let mut bytes = sample_footer().to_bytes();
        bytes[140] ^= 0xFF;
        assert!(matches!(
            Footer::from_bytes(&bytes),
            Err(ZarError::BadMagic(_))
        ));

        let mut bytes = sample_footer().to_bytes();
        bytes[136] ^= 0xFF;
        assert!(matches!(
            Footer::from_bytes(&bytes),
            Err(ZarError::BadVersion(_))
        ));
    }

    #[test]
    fn offset_record_round_trips_through_bytes() {
        let mut sizes = [0u16; ENTRIES_PER_OFFSET_RECORD];
        for (i, s) in sizes.iter_mut().enumerate() {
            *s = (i as u16) * 1000;
        }
        sizes[15] = 0xFFFF;
        let rec = CompressionOffsetRecord {
            base_offset: 0xDEAD_BEEF,
            sizes,
        };
        let bytes = rec.to_bytes();
        assert_eq!(be_u64(&bytes[0..8]), 0xDEAD_BEEF);
        assert_eq!(be_u16(&bytes[38..40]), 0xFFFF);
        assert_eq!(CompressionOffsetRecord::from_bytes(&bytes), rec);
    }

    #[test]
    fn file_entry_carries_48_bit_offset_and_size() {
        let entry = FileDirectoryEntry::file(0x1234, 0xABCD_1234_5678, 0x9876_5432_10FE)
            .expect("48-bit extents fit");
        let parsed = FileDirectoryEntry::from_bytes(&entry.to_bytes());
        assert!(parsed.is_file());
        assert_eq!(parsed.name_offset(), 0x1234);
        assert_eq!(parsed.file_offset(), 0xABCD_1234_5678);
        assert_eq!(parsed.file_size(), 0x9876_5432_10FE);
    }

    #[test]
    fn file_entry_rejects_oversized_extents() {
        assert!(matches!(
            FileDirectoryEntry::file(0, 0, MAX_FILE_EXTENT + 1),
            Err(ZarError::FileTooLarge(_))
        ));
    }

    #[test]
    fn directory_entry_carries_child_range() {
        let entry = FileDirectoryEntry::directory(ROOT_NAME_OFFSET_SENTINEL, 1, 7);
        let parsed = FileDirectoryEntry::from_bytes(&entry.to_bytes());
        assert!(!parsed.is_file());
        assert_eq!(parsed.name_offset(), ROOT_NAME_OFFSET_SENTINEL);
        assert_eq!(parsed.node_start_index(), 1);
        assert_eq!(parsed.count(), 7);
    }

    #[test]
    fn name_len_uses_one_byte_below_128() {
        let mut out = Vec::new();
        encode_name_len(127, &mut out);
        assert_eq!(out, vec![127]);
        assert_eq!(decode_name_len(&out, 0).expect("decodes"), (127, 1));
    }

    #[test]
    fn name_len_decodes_hand_built_two_byte_form() {
        // 200 = 0xC8 | (1 << 7)
        let table = [0xC8u8, 0x01];
        assert_eq!(decode_name_len(&table, 0).expect("decodes"), (200, 2));

        let mut out = Vec::new();
        encode_name_len(200, &mut out);
        assert_eq!(out, table.to_vec());
    }

    #[test]
    fn split_path_ignores_separators_and_empties() {
        let parts: Vec<&str> = split_path("/a\\b//c/").collect();
        assert_eq!(parts, vec!["a", "b", "c"]);
    }
}
