//! ZArchive / WUA on-disk format constants.
//!
//! ZArchive is the generic container Cemu uses for `.wua` Wii U
//! archives. Multi-byte fields on disk are big-endian throughout.

/// Fixed magic value in the last 4 bytes of every ZArchive,
/// big-endian.
pub const ZARCHIVE_FOOTER_MAGIC: u32 = 0x169f_52d6;

/// Format version. Serialised immediately before the magic,
/// big-endian. Upstream treats this as a second magic so bumping it
/// would make existing readers reject the file.
pub const ZARCHIVE_FOOTER_VERSION: u32 = 0x61bf_3a01;

/// Serialised size of
/// [`crate::nintendo::wup::models::footer::ZArchiveFooter`] in bytes.
/// A reader locates the footer by seeking to
/// `file_size - ZARCHIVE_FOOTER_SIZE`.
pub const ZARCHIVE_FOOTER_SIZE: usize = 144;

/// Serialised size of
/// [`crate::nintendo::wup::models::offset_record::CompressionOffsetRecord`]
/// in bytes (8 for `base_offset` plus 16 * 2 for the size array).
pub const COMPRESSION_OFFSET_RECORD_SIZE: usize = 40;

/// Serialised size of
/// [`crate::nintendo::wup::models::file_tree::FileDirectoryEntry`] in
/// bytes. One header u32 plus a three-u32 union payload.
pub const FILE_DIRECTORY_ENTRY_SIZE: usize = 16;

/// Uncompressed size of one ZArchive data block. Every file in the
/// archive is chunked into blocks of this size before zstd
/// compression; the final block of the archive is zero-padded up to
/// this size before being compressed.
pub const COMPRESSED_BLOCK_SIZE: usize = 64 * 1024;

/// Number of compressed blocks covered by a single
/// `CompressionOffsetRecord`. Must be a power of two.
pub const ENTRIES_PER_OFFSET_RECORD: usize = 16;

/// Zstd compression level Cemu's own converter uses. Fixed at 6 so
/// that archives produced by rom-converto match Cemu's output
/// byte-for-byte when the input is identical.
pub const ZARCHIVE_DEFAULT_ZSTD_LEVEL: i32 = 6;

/// Minimum zstd level accepted by the WUA compressor. Zero is the
/// sentinel for "use the library default", which in practice is
/// [`ZARCHIVE_DEFAULT_ZSTD_LEVEL`].
pub const MIN_ZSTD_LEVEL: i32 = 0;

/// Maximum zstd level accepted by the WUA compressor.
pub const MAX_ZSTD_LEVEL: i32 = 22;

/// Bit 31 of `FileDirectoryEntry::name_offset_and_type_flag`. Set on
/// files, clear on directories.
pub const FILE_DIR_TYPE_FLAG_FILE: u32 = 0x8000_0000;

/// Mask applied to `name_offset_and_type_flag` to recover the name
/// table offset. The upper bit is the type flag, everything else is
/// the offset.
pub const FILE_DIR_NAME_OFFSET_MASK: u32 = 0x7FFF_FFFF;

/// Sentinel "no name" offset used by the root directory entry. The
/// upstream reader explicitly rejects archives where `file_tree[0]`
/// has any other value in this field.
pub const ROOT_NAME_OFFSET_SENTINEL: u32 = 0x7FFF_FFFF;

/// Maximum node name length accepted by the writer, in bytes. The
/// upstream reader has a confirmed bug in its 2-byte-prefix
/// extended-name path, so we never emit a name that would trigger it.
/// Wii U FST paths are all well below this limit.
pub const MAX_NODE_NAME_LEN: usize = 127;
