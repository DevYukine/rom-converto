//! Constants shared by the WIA/RVZ disc image format.
//!
//! Console-specific constants (GameCube magic, Wii sector layout, Wii
//! ticket offsets) live in [`crate::nintendo::dol::constants`] and
//! [`crate::nintendo::rvl::constants`].

/// RVZ file magic. Matches `RVZ_MAGIC = 0x015A5652` in
/// `Source/Core/DiscIO/WIABlob.h`. We only emit RVZ (our group
/// entries carry `rvz_packed_size`, which WIA doesn't define), so
/// the plain WIA magic is never written or accepted.
pub const RVZ_MAGIC: [u8; 4] = *b"RVZ\x01";

/// Minimum chunk size for an RVZ file. The WIA format mandates 2 MiB, but
/// RVZ relaxes this to 32 KiB power-of-two. Dolphin's UI also exposes
/// 32 KiB as the smallest selectable block size.
pub const MIN_CHUNK_SIZE: u32 = 32 * 1024;

/// Maximum chunk size Dolphin's UI exposes for RVZ output. Larger sizes
/// are technically legal but pessimise random-access reads in the emulator.
pub const MAX_CHUNK_SIZE: u32 = 2 * 1024 * 1024;

/// Default chunk size used when compressing. 128 KiB matches Dolphin's
/// documented RVZ default and gives a good ratio/seek-time trade-off.
pub const DEFAULT_CHUNK_SIZE: u32 = 128 * 1024;

/// Default zstd compression level. Dolphin uses level 22 (max non-extreme)
/// for archive-quality output. The CLI lets users lower this for speed.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 22;

const _: () = assert!(
    MIN_CHUNK_SIZE.is_power_of_two(),
    "MIN_CHUNK_SIZE must be a power of two per the RVZ spec",
);
const _: () = assert!(
    MAX_CHUNK_SIZE.is_power_of_two(),
    "MAX_CHUNK_SIZE must be a power of two",
);
const _: () = assert!(
    DEFAULT_CHUNK_SIZE.is_power_of_two(),
    "DEFAULT_CHUNK_SIZE must be a power of two",
);
const _: () = assert!(
    DEFAULT_CHUNK_SIZE >= MIN_CHUNK_SIZE && DEFAULT_CHUNK_SIZE <= MAX_CHUNK_SIZE,
    "DEFAULT_CHUNK_SIZE must fall inside the [MIN, MAX] range",
);
