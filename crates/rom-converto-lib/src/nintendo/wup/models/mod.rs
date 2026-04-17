//! On-disk structs for the WUP module.
//!
//! Split into two groups:
//!
//! - **ZArchive format** (`file_tree`, `footer`, `offset_record`):
//!   binrw structs serialised big-endian. Every serialised size is
//!   fixed by the spec and asserted in its own module.
//! - **Nintendo NUS format** (`ticket`, `tmd`): fixed-offset parsers
//!   over the published Wii U ticket and TMD layouts. These don't
//!   use binrw because the format has awkward padding and variable
//!   trailing arrays; a hand-rolled parser at the offsets Cemu's
//!   `ncrypto.cpp` uses is simpler.

pub mod file_tree;
pub mod footer;
pub mod offset_record;
pub mod ticket;
pub mod tmd;

pub use file_tree::FileDirectoryEntry;
pub use footer::{ZArchiveFooter, ZArchiveSectionInfo};
pub use offset_record::CompressionOffsetRecord;
pub use ticket::WupTicket;
pub use tmd::{TmdContentEntry, TmdContentFlags, WupTmd};
