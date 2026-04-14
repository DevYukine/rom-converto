//! Wii disc layout constants.

/// Wii disc magic: `0x5D1C9EA3` at offset 0x18 of the disc header.
pub const WII_MAGIC_OFFSET: usize = 0x18;
pub const WII_MAGIC: u32 = 0x5D1C9EA3;

/// Size in bytes of a Wii disc sector before encryption is stripped.
pub const WII_SECTOR_SIZE: usize = 0x8000;

/// Same as [`WII_SECTOR_SIZE`], typed as `u64`. Hot-path callers
/// (sector-position math in the RVZ encoders/decoders) compute in
/// `u64` because cluster indices × cluster size exceed `usize` on
/// 32-bit targets. Re-exporting the typed constant avoids a dozen
/// `as u64` casts peppered through the hot loops.
pub const WII_SECTOR_SIZE_U64: u64 = WII_SECTOR_SIZE as u64;

/// Size in bytes of the hash region at the front of each encrypted Wii sector.
pub const WII_HASH_SIZE: usize = 0x400;

/// Size in bytes of the payload portion of each Wii sector (data after the
/// hash region is stripped).
pub const WII_SECTOR_PAYLOAD_SIZE: usize = WII_SECTOR_SIZE - WII_HASH_SIZE;

/// Offset inside the Wii disc where the partition table group array lives.
pub const WII_PARTITION_INFO_OFFSET: u64 = 0x40000;

/// Number of partition groups in a Wii disc.
pub const WII_PARTITION_GROUPS: usize = 4;

/// Size of a single partition entry in the Wii partition table: offset/4 + type.
pub const WII_PARTITION_ENTRY_SIZE: usize = 8;

/// Size in bytes of a Wii Ticket v0 (the format used on retail discs).
pub const WII_TICKET_SIZE: usize = 0x2A4;

/// Offset inside a Wii Ticket where the encrypted title key lives.
pub const WII_TICKET_TITLE_KEY_OFFSET: usize = 0x1BF;

/// Offset inside a Wii Ticket where the title id lives.
pub const WII_TICKET_TITLE_ID_OFFSET: usize = 0x1DC;

/// Offset inside a Wii Ticket where the common key index lives.
pub const WII_TICKET_COMMON_KEY_INDEX_OFFSET: usize = 0x1F1;

/// Number of Wii sectors per cluster (group).
pub const WII_BLOCKS_PER_GROUP: usize = 0x40;

/// Total bytes of encrypted disc data per cluster (64 × 0x8000).
pub const WII_GROUP_TOTAL_SIZE: u64 = (WII_BLOCKS_PER_GROUP * WII_SECTOR_SIZE) as u64;

/// Total bytes of plaintext payload per cluster (64 × 0x7C00).
pub const WII_GROUP_PAYLOAD_SIZE: u64 = (WII_BLOCKS_PER_GROUP * WII_SECTOR_PAYLOAD_SIZE) as u64;

/// Partition header field offsets (relative to `partition_offset`).
pub const WII_PARTITION_HEADER_TMD_SIZE_OFFSET: usize = 0x2A4;
pub const WII_PARTITION_HEADER_TMD_OFFSET_OFFSET: usize = 0x2A8;
pub const WII_PARTITION_HEADER_CERT_SIZE_OFFSET: usize = 0x2AC;
pub const WII_PARTITION_HEADER_CERT_OFFSET_OFFSET: usize = 0x2B0;
pub const WII_PARTITION_HEADER_H3_OFFSET_OFFSET: usize = 0x2B4;
pub const WII_PARTITION_HEADER_DATA_OFFSET_OFFSET: usize = 0x2B8;
pub const WII_PARTITION_HEADER_DATA_SIZE_OFFSET: usize = 0x2BC;
pub const WII_PARTITION_HEADER_SIZE: usize = 0x2C0;
