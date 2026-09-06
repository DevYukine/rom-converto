//! ZArchive container support.
//!
//! ZArchive stores an already-extracted directory tree as one logical
//! byte stream cut into 64 KiB zstd blocks, with the offset records,
//! name table, and file tree appended after the data and a 144-byte
//! footer at the very end. It is Cemu's container, so both consoles
//! that use it live on top of this module: Xenia mounts a `.zar` as an
//! Xbox 360 game's virtual filesystem (ISO -> XDVDFS extract ->
//! ZArchive pack), and Cemu mounts a `.wua` as one or more Wii U
//! titles ([`crate::nintendo::wup`]).
//!
//! [`format`] holds the on-disk structures, [`reader`] parses and
//! streams an archive, and [`writer`] packs one with parallel block
//! compression.

pub mod format;
pub mod reader;
pub mod writer;

pub use format::{
    COMPRESSED_BLOCK_SIZE, CompressionOffsetRecord, FileDirectoryEntry, Footer, Section, ZarError,
    ZarResult,
};
pub use reader::{ZarEntry, ZarReader, decompress_block};
pub use writer::{
    DEFAULT_COMPRESSION_LEVEL, MAX_COMPRESSION_LEVEL, MIN_COMPRESSION_LEVEL, ZarSummary, ZarWriter,
};
