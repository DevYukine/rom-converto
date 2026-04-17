//! Wii U optical disc (WUD) and compressed disc (WUX) parsing.
//!
//! This module exposes a byte-level reader for Nintendo's Wii U disc
//! image containers. It is one of three Wii U title sources the WUA
//! compressor accepts; see [`super::compress`] for the dispatcher.
//!
//! # Containers
//!
//! Two file formats are supported:
//!
//! - **WUD**: raw 1:1 image of a retail optical disc. Single-layer
//!   retail media is exactly `0x5D3A00000` bytes (roughly 23.3 GiB).
//!   Split images named `game_part<N>.wud` (2 GiB per part,
//!   1-indexed, up to 12 parts) are accepted transparently.
//! - **WUX**: a deduplication-only compressed form of the same data.
//!   32 byte little-endian header followed by a u32 LE physical-index
//!   table, one entry per logical sector. Duplicate sectors (all
//!   zeros are common) share a single physical slot, which typically
//!   shrinks a retail image to 10-15 GiB.
//!
//! Both formats present the same underlying byte stream once opened;
//! callers see them through the unified [`sector_stream::DiscSectorSource`]
//! trait and never need to know which container is on disk.
//!
//! # Cryptography
//!
//! The Wii U disc has two independent layers of encryption:
//!
//! 1. **Per-disc master key** (`game.key`, 16 raw bytes, per-disc,
//!    never released by Nintendo). AES-128-CBC with a zero IV
//!    decrypts the partition table of contents at offset `0x18000`,
//!    and with a file-offset-derived IV decrypts files inside the SI
//!    partition's FST (the per-title ticket, TMD, and cert blobs).
//! 2. **Wii U common key** (`[`super::common_keys::WII_U_COMMON_KEY`]`,
//!    shared across all retail consoles, hardcoded). Decrypts the
//!    encrypted title key inside each ticket. The recovered title key
//!    then decrypts the content files in the matching GM partition
//!    using standard NUS raw or hashed AES-128-CBC modes.
//!
//! The disc key is the only secret the user must supply. Without it
//! the partition TOC will not decrypt and conversion fails fast with
//! a `DiscKeyWrong` error (sentinel `CC A6 E6 7B` at offset 0 of the
//! decrypted TOC).
//!
//! # On-disc layout reference
//!
//! ```text
//! 0x00000 ..                 disc header (not parsed, not used)
//! 0x18000 ..             encrypted partition TOC (0x8000 bytes)
//!   decrypted:
//!     0x0000: 4    CC A6 E6 7B  sentinel (DECRYPTED_AREA_SIGNATURE)
//!     0x001C: 4    partitionCount (u32 BE)
//!     0x0800: 0x80 * partitionCount  partition entries
//!       entry:
//!         0x00: 0x19  name (ASCII, null terminated)
//!         0x20: 4     startSector (u32 BE; multiply by 0x8000)
//!
//! <partition start> ..    partition header (0x20, plaintext)
//!     0x00: 4    CC 93 A4 F5  sentinel (PARTITION_START_SIGNATURE)
//!     0x04: 4    headerSize (u32 BE)
//!     0x14: 4    FSTSize (u32 BE)
//!
//! <partition start + headerSize> ..  encrypted FST
//!     first 4 bytes after disc-key zero-IV decrypt: 46 53 54 00
//!     (FST\0); standard NUS FST format thereafter
//! ```
//!
//! # Partition kinds
//!
//! Partition names are 25-byte ASCII strings with a two-character
//! kind prefix:
//!
//! - **SI** - System Install. Exactly one per disc. Contains an FST
//!   of per-title directories; each holds `title.tik`, `title.tmd`,
//!   and `title.cert` for a game or update partition on the same
//!   disc.
//! - **GM** - Game partition. One per user-visible title. Name
//!   suffix is the lower 8 hex digits of the title ID. Contains the
//!   raw AES-encrypted content files of that title.
//! - **UP** - Update partition. Optional. Same structure as GM; its
//!   ticket/TMD also live in the SI partition.
//! - **UC** - Often DLC; same handling as GM/UP.
//! - Other prefixes are ignored with a log line rather than an error
//!   (future-compatibility).

pub mod compress;
pub mod disc_key;
pub mod partition;
pub mod partition_table;
pub mod sector_stream;
pub mod wud_reader;
pub mod wux_reader;

pub use compress::compress_disc_title;
pub use disc_key::{DiscKey, load_disc_key};
pub use partition_table::{PartitionEntry, PartitionKind, PartitionTable, parse_partition_table};
pub use sector_stream::{DiscSectorSource, SECTOR_SIZE, open_disc};
