//! Constants shared by the WIA/RVZ disc image format.
//!
//! Console-specific constants (GameCube magic, Wii sector layout, Wii
//! ticket offsets) live in [`crate::nintendo::dol::constants`] and
//! [`crate::nintendo::rvl::constants`].

/// RVZ file magic: `0x015A5652` (`RVZ\x01`). Only RVZ is ever emitted (its group
/// entries carry `rvz_packed_size`, which WIA doesn't define), so
/// the plain WIA magic is never written or accepted.
pub const RVZ_MAGIC: [u8; 4] = *b"RVZ\x01";

/// Smallest chunk size of the power-of-two family an RVZ container may
/// declare. The container-level rule also accepts multiples of
/// [`MAX_CHUNK_SIZE`].
pub const MIN_CHUNK_SIZE: u32 = 32 * 1024;

/// Upper chunk-size bound for decoding chunks of containers that carry
/// Wii partition data (`n_part > 0`): the partition decoder walks one
/// 2 MiB cluster of sectors per chunk, so larger chunks cannot feed it.
/// The writer accepts only power-of-two sizes in
/// [`MIN_CHUNK_SIZE`]..=[`MAX_CHUNK_SIZE`]. Raw-only containers may
/// exceed this: the container-level rule accepts any multiple of this
/// size that the u32 chunk-size field can hold.
pub const MAX_CHUNK_SIZE: u32 = 2 * 1024 * 1024;

/// Plausibility cap on `wia_file_head_t.iso_file_size` when reading:
/// 64 GiB. The largest GameCube/Wii disc ever shipped is a dual-layer
/// Wii image at ~8.5 GB, so the cap is generous slack over that
/// ceiling; anything claiming more is a corrupt header, and an
/// unbounded `iso_file_size` would inflate the geometric group-count
/// bound.
pub const MAX_PLAUSIBLE_ISO_SIZE: u64 = 64 * 1024 * 1024 * 1024;

/// Default chunk size used when compressing. 128 KiB is the format's
/// established default and gives a good ratio/seek-time trade-off.
pub const DEFAULT_CHUNK_SIZE: u32 = 128 * 1024;

/// Default zstd compression level: zstd's maximum level (22, an ultra
/// level), for archive-quality output. The CLI lets users lower this
/// for speed.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 22;

/// Chunk sizes above this read more data per seek than weak playback
/// hardware (handhelds, older Android devices) can comfortably keep up
/// with. 128/256/512 KiB stay silent; only the top end near
/// [`MAX_CHUNK_SIZE`] warns.
pub const WEAK_HW_CHUNK_WARN: u32 = 1024 * 1024;

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
