//! ZArchive (`.zar`) container support.
//!
//! ZArchive stores an already-extracted directory tree as one logical
//! byte stream cut into 64 KiB zstd blocks, with the offset records,
//! name table, and file tree appended after the data and a 144-byte
//! footer at the very end. Xenia mounts a `.zar` as the game's virtual
//! filesystem, so the Xbox 360 pipeline is ISO -> XDVDFS extract ->
//! ZArchive pack.
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
pub use writer::{ZarSummary, ZarWriter};
