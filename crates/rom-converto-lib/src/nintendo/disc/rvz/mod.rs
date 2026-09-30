//! Wii and GameCube RVZ disc image compression.
//!
//! RVZ stores a disc image as a chunk table of independently-compressed
//! blocks. Both
//! GameCube and Wii discs use the same on-disc format; the console-specific
//! pieces (disc detection, encryption, common keys) live in
//! [`crate::nintendo::dol`] and [`crate::nintendo::rvl`].
//!
//! # Async/sync boundary
//!
//! [`compress_disc`] and [`decompress_disc`] are `async`. They hand the
//! whole blocking pipeline to [`tokio::task::spawn_blocking`] and poll a
//! shared `Arc<AtomicU64>` for progress. Rules:
//!
//! * All `std::fs` calls live inside `spawn_blocking` or worker
//!   threads spawned from it. No sync I/O ever runs on the async
//!   runtime.
//! * `tokio::fs` is avoided in the hot path. On Windows it wraps
//!   `std::fs` + `spawn_blocking` with no speed gain; on Linux it
//!   lacks positional reads, which the worker pools need.
//!   `tokio::fs::metadata` is used once at the entry point for
//!   progress reporting, nothing more.
//! * Worker pools use `std::thread::spawn` with `std::sync::mpsc`,
//!   not Tokio primitives. The pool is owned by the outer
//!   `spawn_blocking` closure and never touches the runtime.

pub mod constants;
pub mod error;
pub mod format;
pub mod packing;
pub mod regions;

pub mod compress;
pub mod decompress;
pub mod verify;

pub use compress::{RvzCompressOptions, compress_disc};
pub use decompress::{decompress_disc, decompress_disc_to_wbfs};
pub use error::{RvzError, RvzResult};
pub use verify::{RvzStructuralVerify, verify_rvz_structure};

use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use binrw::{BinRead, Endian};
use constants::{MAX_CHUNK_SIZE, MAX_PLAUSIBLE_ISO_SIZE, MIN_CHUNK_SIZE};
use format::{
    RVZ_GROUP_SIZE, RvzGroup, WIA_FILE_HEAD_SIZE, WIA_PART_SIZE, WIA_RAW_DATA_SIZE, WiaDisc,
    WiaFileHead, WiaPart, WiaRawData,
};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Read and parse the compressed group descriptor table described by
/// `disc`. Shared by the structural verifier and both decompress paths.
///
/// Every bound runs before any table-sized allocation:
///
/// * `disc.chunk_size` must satisfy the container-level rule (see
///   [`validate_container_chunk_size`]); a partitioned container with a
///   chunk size past the 2 MiB partition-decode limit reads as
///   [`RvzError::PartitionChunkTooLarge`], anything else invalid as
///   [`RvzError::InvalidChunkSize`].
/// * `n_groups` is capped geometrically: every region contributes at
///   most one group per chunk of disc space (plus one for sector
///   alignment), so `iso_file_size.div_ceil(chunk_size)` plus a
///   per-region/per-partition slack term bounds the group count any
///   real container can carry. Inflated counts read as
///   [`RvzError::TableTooLarge`] instead of a multi-gigabyte
///   allocation.
/// * The table's location is checked against the file length
///   ([`RvzError::Truncated`]) and, secondarily, zstd's worst-case
///   expansion ratio ([`RvzError::TableTooLarge`]): the ratio alone
///   would reject sparse containers whose sentinel-packed tables
///   legitimately dwarf the file, but it still catches tables whose
///   claimed expansion no zstd frame could produce. Symmetrically, the
///   compressed table's stored size is capped at zstd's worst case for
///   the declared entry count before the whole table is read into
///   memory: a larger `group_size` cannot decode to the declared
///   entries.
///
/// Decoding streams through `zstd::stream` + `io::take`, so the decode
/// buffer never grows beyond the bounded raw table, and the decoded
/// length must match the declared entry count exactly.
pub(crate) fn read_group_table(
    reader: &mut (impl Read + Seek),
    disc: &WiaDisc,
    iso_file_size: u64,
) -> RvzResult<Vec<RvzGroup>> {
    validate_container_chunk_size(disc.chunk_size, disc.n_part)?;
    if disc.n_groups == 0 {
        return Ok(Vec::new());
    }
    let max_groups = iso_file_size.div_ceil(u64::from(disc.chunk_size))
        + 2 * (u64::from(disc.n_raw_data) + 2 * u64::from(disc.n_part))
        + 64;
    if u64::from(disc.n_groups) > max_groups {
        return Err(RvzError::TableTooLarge {
            table: "group",
            entries: disc.n_groups,
        });
    }
    let file_len = reader.seek(SeekFrom::End(0))?;
    let table_end = disc.group_off.saturating_add(u64::from(disc.group_size));
    if table_end > file_len {
        return Err(RvzError::Truncated {
            expected: table_end,
            actual: file_len,
        });
    }
    // Secondary gate: zstd's worst case expansion is one 128 KiB block
    // per 3 stored bytes (RLE block = 3 bytes → 128 KiB), plus bounded
    // frame overhead. The geometric bound above is what catches inflated
    // counts (sparse containers legitimately need this full ratio
    // headroom), but a table claiming a larger expansion than any frame
    // could produce is still corrupt.
    let raw_len = u64::from(disc.n_groups) * RVZ_GROUP_SIZE as u64;
    let raw_len_cap = (u64::from(disc.group_size) / 3 + 64) * 128 * 1024;
    if raw_len > raw_len_cap {
        return Err(RvzError::TableTooLarge {
            table: "group",
            entries: disc.n_groups,
        });
    }
    // The compressed table is read whole: cap it at zstd's worst case
    // for the declared entry count (plus slack for streaming-encoder
    // overhead) so a hostile `group_size` bounded only by the file
    // length cannot become a file-sized allocation. A larger table
    // cannot decode to `n_groups` entries.
    let table_cap = u64::try_from(zstd::zstd_safe::compress_bound(raw_len as usize))
        .unwrap_or(u64::MAX)
        + 64 * 1024;
    if u64::from(disc.group_size) > table_cap {
        return Err(RvzError::Custom(format!(
            "group table stores {} bytes, more than zstd's worst case for {} entries",
            disc.group_size, disc.n_groups
        )));
    }

    reader.seek(SeekFrom::Start(disc.group_off))?;
    let mut compressed = vec![0u8; disc.group_size as usize];
    reader.read_exact(&mut compressed)?;
    let decompressed = decode_table(&compressed, raw_len)?;
    let mut cursor = Cursor::new(&decompressed);
    let mut groups = Vec::with_capacity(raw_len as usize / RVZ_GROUP_SIZE);
    for _ in 0..disc.n_groups {
        groups.push(RvzGroup::read_options(&mut cursor, Endian::Big, ())?);
    }
    Ok(groups)
}

/// The chunk size an RVZ container may declare: a power of two of at
/// least [`MIN_CHUNK_SIZE`], or a multiple of [`MAX_CHUNK_SIZE`]. A
/// partitioned container (`n_part > 0`) additionally cannot exceed
/// [`MAX_CHUNK_SIZE`]: the partition decoder walks one 2 MiB cluster of
/// sectors per chunk, so a larger chunk is a decode limitation, not a
/// corrupt value, and reads as [`RvzError::PartitionChunkTooLarge`].
/// Anything else is corrupt and reads as [`RvzError::InvalidChunkSize`].
/// Every caller that divides by `chunk_size` runs this first.
pub(crate) fn validate_container_chunk_size(chunk_size: u32, n_part: u32) -> RvzResult<()> {
    let valid = (chunk_size.is_power_of_two() && chunk_size >= MIN_CHUNK_SIZE)
        || (chunk_size >= MAX_CHUNK_SIZE && chunk_size.is_multiple_of(MAX_CHUNK_SIZE));
    if !valid {
        return Err(RvzError::InvalidChunkSize(
            chunk_size,
            MIN_CHUNK_SIZE,
            MAX_CHUNK_SIZE,
        ));
    }
    if n_part > 0 && chunk_size > MAX_CHUNK_SIZE {
        return Err(RvzError::PartitionChunkTooLarge(chunk_size, MAX_CHUNK_SIZE));
    }
    Ok(())
}

/// Read and parse the compressed raw-data descriptor table described by
/// `disc`. [`check_table_bounds`] must have run first: it bounds
/// `n_raw_data` by zstd's worst-case expansion ratio against the bytes
/// the file stores, by the ISO's sector count, and caps the table's
/// file span, so the allocations here are bounded. The stored table is
/// additionally capped at zstd's worst case for the declared entry
/// count before it is read whole. The decode streams through
/// `zstd::stream` + `io::take` and must yield exactly the declared
/// entry bytes. Shared by the structural verifier and both decompress
/// paths.
pub(crate) fn read_raw_data_table(
    reader: &mut (impl Read + Seek),
    disc: &WiaDisc,
) -> RvzResult<Vec<WiaRawData>> {
    if disc.n_raw_data == 0 {
        return Ok(Vec::new());
    }
    let raw_len = u64::from(disc.n_raw_data) * WIA_RAW_DATA_SIZE as u64;
    let table_cap = u64::try_from(zstd::zstd_safe::compress_bound(raw_len as usize))
        .unwrap_or(u64::MAX)
        + 64 * 1024;
    if u64::from(disc.raw_data_size) > table_cap {
        return Err(RvzError::Custom(format!(
            "raw_data table stores {} bytes, more than zstd's worst case for {} entries",
            disc.raw_data_size, disc.n_raw_data
        )));
    }
    reader.seek(SeekFrom::Start(disc.raw_data_off))?;
    let mut compressed = vec![0u8; disc.raw_data_size as usize];
    reader.read_exact(&mut compressed)?;
    let decompressed = decode_table(&compressed, raw_len)?;
    let mut cursor = Cursor::new(&decompressed);
    let mut out = Vec::with_capacity(disc.n_raw_data as usize);
    for _ in 0..disc.n_raw_data {
        out.push(WiaRawData::read_options(&mut cursor, Endian::Big, ())?);
    }
    Ok(out)
}

/// Streaming-decode a compressed metadata table to exactly `raw_len`
/// bytes. The reservation is capped at 64 MiB (real tables max a few MB)
/// and the `take(raw_len)` + `read_to_end` combination grows the buffer
/// only as bytes actually arrive, so a hostile `raw_len` can never turn
/// into a hostile up-front allocation; the streaming decoder's window is
/// zstd's default cap. The frame must end exactly at `raw_len`: the
/// table parsers read exactly the declared entries, so a stream that
/// decodes further is rejected instead of silently ignored.
fn decode_table(compressed: &[u8], raw_len: u64) -> RvzResult<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    let mut out = Vec::with_capacity(raw_len.min(64 * 1024 * 1024) as usize);
    {
        let mut limited = (&mut decoder).take(raw_len);
        limited.read_to_end(&mut out)?;
    }
    if out.len() as u64 != raw_len {
        return Err(RvzError::DecompressedSizeMismatch {
            expected: raw_len,
            actual: out.len() as u64,
        });
    }
    // Probe one byte past the declared length: the frame must end here.
    let mut extra = [0u8; 1];
    let extra_len = decoder.read(&mut extra)?;
    if extra_len != 0 {
        return Err(RvzError::DecompressedSizeMismatch {
            expected: raw_len,
            actual: raw_len + extra_len as u64,
        });
    }
    Ok(out)
}

/// Rejects a `disc_size` the file could not back and an
/// `iso_file_size` no plausible GameCube/Wii disc could have, before
/// the disc-struct allocation: the 64 GiB [`MAX_PLAUSIBLE_ISO_SIZE`]
/// cap reads as [`RvzError::ImplausibleIsoSize`], leaving generous
/// slack over the ~8.5 GB dual-layer ceiling while still bounding the
/// geometric group-count math. Shared by the structural verifier and
/// both decompress paths.
pub(crate) fn check_disc_size(head: &WiaFileHead, file_len: u64) -> RvzResult<()> {
    if head.iso_file_size > MAX_PLAUSIBLE_ISO_SIZE {
        return Err(RvzError::ImplausibleIsoSize(head.iso_file_size));
    }
    if u64::from(head.disc_size) > file_len.saturating_sub(WIA_FILE_HEAD_SIZE as u64) {
        return Err(RvzError::Truncated {
            expected: u64::from(head.disc_size) + WIA_FILE_HEAD_SIZE as u64,
            actual: file_len,
        });
    }
    Ok(())
}

/// The partition data span the writer actually encodes: the declared size
/// clamped to what the ISO holds past `data_start` (the format's "too
/// large partition" clamp), then truncated to whole 0x8000-byte Wii
/// sectors. The RVZ partition layout stores `n_sectors` truncated to
/// sectors, so a sub-sector tail cannot be represented inside a
/// partition. Both the region planner and the partition encoder use
/// this: the group ranges never claim sectors past the ISO, and the
/// dropped tail falls into the following raw region.
pub(crate) fn partition_encoded_size(data_start: u64, data_size: u64, iso_size: u64) -> u64 {
    let clamped = data_size.min(iso_size.saturating_sub(data_start));
    clamped - clamped % WII_SECTOR_SIZE_U64
}

/// Rejects header-declared table sizes the file could not back, before
/// the partition-array and raw-data-table allocations. `n_raw_data` is
/// additionally bounded by the file-backed zstd ratio gate: the decoded
/// table is `n_raw_data * WIA_RAW_DATA_SIZE` bytes, and no zstd frame
/// can expand beyond `(stored_len / 3 + 64) * 128 KiB` (one 128 KiB
/// block per 3 stored bytes), where `stored_len` is `raw_data_size`
/// (capped against `file_len` below). It is also capped at
/// `div_ceil(iso_file_size, 0x8000) + 64`: not a format invariant (the
/// spec allows zero-size or overlapping entries), but every writer
/// emits at most one entry per disc sector plus a few reserved ones,
/// so
/// the `+ 64` slack is writer tolerance and the rest a DoS bound.
/// Shared by the structural verifier
/// and both decompress paths.
pub(crate) fn check_table_bounds(
    disc: &WiaDisc,
    file_len: u64,
    iso_file_size: u64,
) -> RvzResult<()> {
    let raw_table_len = u64::from(disc.n_raw_data) * WIA_RAW_DATA_SIZE as u64;
    let raw_len_cap = (u64::from(disc.raw_data_size) / 3 + 64) * 128 * 1024;
    let n_raw_data_cap = iso_file_size.div_ceil(0x8000) + 64;
    if raw_table_len > raw_len_cap || u64::from(disc.n_raw_data) > n_raw_data_cap {
        return Err(RvzError::TableTooLarge {
            table: "raw_data",
            entries: disc.n_raw_data,
        });
    }
    // The partition table exists only when the container declares one;
    // an unused `part_off` on a raw-only container carries no bytes and
    // is not checked.
    if disc.n_part > 0 {
        let parts_len = u64::from(disc.n_part) * WIA_PART_SIZE as u64;
        let parts_end = disc.part_off.saturating_add(parts_len);
        if parts_end > file_len {
            return Err(RvzError::Truncated {
                expected: parts_end,
                actual: file_len,
            });
        }
    }
    let raw_end = disc
        .raw_data_off
        .saturating_add(u64::from(disc.raw_data_size));
    if raw_end > file_len {
        return Err(RvzError::Truncated {
            expected: raw_end,
            actual: file_len,
        });
    }
    Ok(())
}

/// Bounds every group descriptor's stored bytes against the file
/// length: a group whose data runs past EOF reads as
/// [`RvzError::Truncated`] before any group is decompressed. The scan
/// covers all groups with stored data, not just the last entry:
/// tables can close with zero sentinels that would hide an earlier
/// group running past EOF. Shared by the structural verifier and both
/// decompress paths.
pub(crate) fn check_group_data_bounds(groups: &[RvzGroup], file_len: u64) -> RvzResult<()> {
    let mut data_end = 0u64;
    for group in groups {
        if group.data_size != 0 {
            let end = (u64::from(group.data_off4) << 2) + u64::from(group.compressed_size());
            data_end = data_end.max(end);
        }
    }
    if file_len < data_end {
        return Err(RvzError::Truncated {
            expected: data_end,
            actual: file_len,
        });
    }
    Ok(())
}

/// Requires every raw-data region to lie inside the disc image the file
/// declares (`raw_data_off + raw_data_size <= iso_file_size`) and to
/// have a group range inside the parsed group table, and every
/// partition's `pd[0] + pd[1]` group range to be contiguous
/// (`pd[1].group_index == pd[0].group_index + pd[0].n_groups`) and to
/// end inside the table (exactly the range
/// [`crate::nintendo::disc::rvz::decompress::partition::build_partition_work_items`]
/// consumes), so later `groups[...]` indexing can neither go out of
/// bounds nor silently decode a non-contiguous span. The partition
/// entries must additionally match the format's partition-data entry
/// geometry, which this crate's writer also emits:
/// `pd.n_groups == div_ceil(pd.n_sectors * 0x8000, chunk_size)` per
/// entry, and (when pd[1] stores data) `pd[1].first_sector` continues
/// pd[0] and pd[0]'s span is a whole number of chunks, so the chunk
/// walk crosses the pd[0]→pd[1] boundary on a chunk boundary. Every
/// raw region must also declare at least the group count its
/// sector-aligned span needs (`div_ceil(span, chunk_size)`; zero-size
/// regions need none), and every
/// partition's sector span must end inside the image
/// (`(pd0.first_sector + pd0.n_sectors + pd1.n_sectors) * 0x8000 <=
/// iso_file_size`). Runs the chunk-size precondition first, since the
/// chunk walks below divide by it. Shared by the structural verifier
/// and both decompress paths.
pub(crate) fn check_group_indices(
    raw_data: &[WiaRawData],
    parts: &[WiaPart],
    groups: &[RvzGroup],
    iso_file_size: u64,
    chunk_size: u32,
) -> RvzResult<()> {
    validate_container_chunk_size(chunk_size, parts.len() as u32)?;
    let len = groups.len() as u64;
    for region in raw_data {
        let end = match region.raw_data_off.checked_add(region.raw_data_size) {
            Some(end) if end <= iso_file_size => end,
            _ => {
                return Err(RvzError::Custom(format!(
                    "raw data region at offset {} ends {}, past the {}-byte disc image",
                    region.raw_data_off,
                    region.raw_data_off.saturating_add(region.raw_data_size),
                    iso_file_size
                )));
            }
        };
        if !group_range_fits(region.group_index, region.n_groups, len) {
            return Err(RvzError::Custom(format!(
                "raw data region at offset {} references groups {}..{}, \
                 but the group table holds {} entries",
                region.raw_data_off,
                region.group_index,
                region.group_index.saturating_add(region.n_groups),
                len
            )));
        }
        // The region decoder walks chunks from the sector-aligned
        // effective start, one group per chunk; an under-declared
        // `n_groups` would leave the region's tail undecodable.
        // Zero-size regions carry no bytes and need no groups.
        if region.raw_data_size > 0 {
            let effective_start = region.raw_data_off - region.raw_data_off % 0x8000;
            let required = (end - effective_start).div_ceil(u64::from(chunk_size));
            if u64::from(region.n_groups) < required {
                return Err(RvzError::Custom(format!(
                    "raw data region at offset {} declares {} groups but {} are needed to \
                     cover {} bytes at chunk size {}",
                    region.raw_data_off,
                    region.n_groups,
                    required,
                    end - effective_start,
                    chunk_size
                )));
            }
        }
    }
    for part in parts {
        let (pd0, pd1) = (&part.pd[0], &part.pd[1]);
        match pd0
            .group_index
            .checked_add(pd0.n_groups)
            .and_then(|end| end.checked_add(pd1.n_groups))
        {
            Some(end) if u64::from(end) <= len => {}
            _ => {
                return Err(RvzError::Custom(format!(
                    "partition data references groups {}..{}, \
                     but the group table holds {} entries",
                    pd0.group_index,
                    pd0.group_index
                        .saturating_add(pd0.n_groups)
                        .saturating_add(pd1.n_groups),
                    len
                )));
            }
        }
        if pd1.group_index != pd0.group_index.wrapping_add(pd0.n_groups) {
            return Err(RvzError::Custom(format!(
                "partition group ranges are not contiguous: pd[1].group_index {} != {}",
                pd1.group_index,
                pd0.group_index.wrapping_add(pd0.n_groups)
            )));
        }
        if pd1.n_groups > 0 {
            if pd1.first_sector != pd0.first_sector.wrapping_add(pd0.n_sectors) {
                return Err(RvzError::Custom(format!(
                    "partition data entries are not contiguous: pd[1].first_sector {} != {}",
                    pd1.first_sector,
                    pd0.first_sector.wrapping_add(pd0.n_sectors)
                )));
            }
            if (u64::from(pd0.n_sectors) * 0x8000) % u64::from(chunk_size) != 0 {
                return Err(RvzError::Custom(format!(
                    "pd[0] data ({} sectors) is not a multiple of the {}-byte chunk size, \
                     so pd[1]'s groups do not start on a chunk boundary",
                    pd0.n_sectors, chunk_size
                )));
            }
        }
        for (idx, pd) in part.pd.iter().enumerate() {
            let expected = (u64::from(pd.n_sectors) * 0x8000).div_ceil(u64::from(chunk_size));
            if u64::from(pd.n_groups) != expected {
                return Err(RvzError::Custom(format!(
                    "pd[{idx}].n_groups {} does not match its sector count ({} sectors \
                     need {} groups at chunk size {})",
                    pd.n_groups, pd.n_sectors, expected, chunk_size
                )));
            }
        }
        // The cluster walk reads the partition's whole sector span out
        // of the disc image, so a span past the declared ISO size is a
        // corrupt descriptor, not a short file.
        let expected_bytes =
            (u64::from(pd0.first_sector) + u64::from(pd0.n_sectors) + u64::from(pd1.n_sectors))
                * 0x8000;
        if expected_bytes > iso_file_size {
            return Err(RvzError::Custom(format!(
                "partition data at sector {} spans {} bytes, past the {}-byte disc image",
                pd0.first_sector, expected_bytes, iso_file_size
            )));
        }
    }
    Ok(())
}

/// True if `group_index + n_groups` stays inside a table of `len`
/// entries.
fn group_range_fits(group_index: u32, n_groups: u32, len: u64) -> bool {
    group_index
        .checked_add(n_groups)
        .is_some_and(|end| u64::from(end) <= len)
}

/// Derives an output `.rvz` path from `input`, collapsing NKit's
/// `.nkit.iso`/`.nkit.gcz` double extension so the result is
/// `name.rvz` rather than `name.nkit.rvz`.
pub fn derive_rvz_path(input: &Path) -> PathBuf {
    // Strip NKit's double extension so game.nkit.iso becomes
    // game.rvz instead of game.nkit.rvz.
    if let Some(name) = input.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".nkit.iso") || lower.ends_with(".nkit.gcz") {
            return input.with_file_name(format!("{}.rvz", &name[..name.len() - 9]));
        }
    }
    input.with_extension("rvz")
}

/// Derives an output `.iso` path from `input` by replacing its extension.
pub fn derive_disc_path(input: &Path) -> PathBuf {
    input.with_extension("iso")
}

/// Derives an output `.wbfs` path from `input` by replacing its extension.
pub fn derive_wbfs_path(input: &Path) -> PathBuf {
    input.with_extension("wbfs")
}

#[cfg(test)]
mod derive_path_tests {
    use super::*;

    #[test]
    fn iso_to_rvz() {
        assert_eq!(
            derive_rvz_path(Path::new("/games/game.iso")),
            PathBuf::from("/games/game.rvz")
        );
    }

    #[test]
    fn gcm_to_rvz() {
        assert_eq!(
            derive_rvz_path(Path::new("game.gcm")),
            PathBuf::from("game.rvz")
        );
    }

    #[test]
    fn no_extension_input_appends_rvz() {
        assert_eq!(
            derive_rvz_path(Path::new("noext")),
            PathBuf::from("noext.rvz")
        );
    }

    #[test]
    fn already_rvz_stays_rvz() {
        assert_eq!(
            derive_rvz_path(Path::new("already.rvz")),
            PathBuf::from("already.rvz")
        );
    }

    #[test]
    fn rvz_to_iso() {
        assert_eq!(
            derive_disc_path(Path::new("game.rvz")),
            PathBuf::from("game.iso")
        );
    }

    #[test]
    fn rvz_to_wbfs() {
        assert_eq!(
            derive_wbfs_path(Path::new("game.rvz")),
            PathBuf::from("game.wbfs")
        );
    }

    #[test]
    fn multi_dot_stem_preserved() {
        assert_eq!(
            derive_rvz_path(Path::new("game.backup.iso")),
            PathBuf::from("game.backup.rvz")
        );
    }

    #[test]
    fn nkit_double_extensions_collapse() {
        assert_eq!(
            derive_rvz_path(Path::new("/g/game.nkit.iso")),
            PathBuf::from("/g/game.rvz")
        );
        assert_eq!(
            derive_rvz_path(Path::new("game.NKIT.GCZ")),
            PathBuf::from("game.rvz")
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso;
    use crate::util::CancelToken;
    use crate::util::NoProgress;

    #[tokio::test]
    async fn gamecube_round_trip_small() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        // 5 MiB gets several 2 MiB chunks plus a short tail.
        let original = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "GC round trip must be byte-identical");
    }

    #[tokio::test]
    async fn rvz_decompresses_to_wbfs_and_reconstructs() {
        use crate::nintendo::disc::wbfs::WbfsReader;
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let wbfs = dir.path().join("game.wbfs");

        // The synthetic GC disc has a garbage FST, so the usage analyzer
        // falls back to keeping the whole image; the WBFS reconstruction
        // is therefore byte-identical to the source disc.
        let original = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_disc_to_wbfs(&rvz, &wbfs, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let mut reader = WbfsReader::open(&wbfs).unwrap();
        let mut got = vec![0u8; original.len()];
        let mut read = 0;
        while read < got.len() {
            let n = reader.read(&mut got[read..]).unwrap();
            assert!(n > 0, "wbfs reader stalled at {read}");
            read += n;
        }
        assert_eq!(
            got, original,
            "rvz -> wbfs -> read must reconstruct the disc"
        );
    }

    #[tokio::test]
    async fn rvz_to_wbfs_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let a = dir.path().join("a.wbfs");
        let b = dir.path().join("b.wbfs");

        let original = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        tokio::fs::write(&iso, &original).await.unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_disc_to_wbfs(&rvz, &a, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        decompress_disc_to_wbfs(&rvz, &b, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let bytes_a = tokio::fs::read(&a).await.unwrap();
        let bytes_b = tokio::fs::read(&b).await.unwrap();
        assert_eq!(
            bytes_a, bytes_b,
            "parallel rvz -> wbfs must be byte-deterministic across runs"
        );
    }

    #[tokio::test]
    async fn rvz_to_wbfs_preserves_wii_partition() {
        use crate::nintendo::disc::wbfs::WbfsReader;
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let wbfs = dir.path().join("wii.wbfs");

        let original = make_fake_wii_iso_with_partition(2);
        tokio::fs::write(&iso, &original).await.unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_disc_to_wbfs(&rvz, &wbfs, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        // The synthetic partition has no junk gaps, so the usage map keeps
        // the whole image and the parallel partition reconstruction is
        // byte-identical to the source.
        let mut reader = WbfsReader::open(&wbfs).unwrap();
        let mut got = vec![0u8; original.len()];
        let mut read = 0;
        while read < got.len() {
            let n = reader.read(&mut got[read..]).unwrap();
            assert!(n > 0, "wbfs reader stalled at {read}");
            read += n;
        }
        assert_eq!(
            got, original,
            "wii partition disc round-trips through the parallel wbfs writer"
        );
    }

    #[tokio::test]
    async fn gamecube_compress_produces_smaller_file_for_compressible_input() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");

        // All zeros compresses heavily.
        let mut original = make_fake_gamecube_iso(4 * 1024 * 1024);
        for b in original.iter_mut().skip(0x80) {
            *b = 0;
        }
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let rvz_size = tokio::fs::metadata(&rvz).await.unwrap().len();
        assert!(
            rvz_size < original.len() as u64,
            "compressed {} >= original {}",
            rvz_size,
            original.len()
        );
    }

    #[tokio::test]
    async fn wii_partition_round_trips_at_small_chunk_size() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        let original = make_fake_wii_iso_with_partition(2);
        tokio::fs::write(&iso, &original).await.unwrap();

        // 128 KiB chunks → 16 chunks per Wii cluster → exercises the
        // sub-cluster path: per-chunk exception lists with chunk-local
        // offsets, deferred exception application during decompress.
        let opts = RvzCompressOptions {
            chunk_size: 128 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz, opts, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(
            original, result,
            "Wii sub-cluster round-trip must be byte-identical"
        );
    }

    #[tokio::test]
    async fn wii_partial_last_cluster_round_trips() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partial_partition;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz_2m = dir.path().join("wii_2mib.rvz");
        let rvz_128k = dir.path().join("wii_128k.rvz");
        let restored_2m = dir.path().join("wii_2mib.round.iso");
        let restored_128k = dir.path().join("wii_128k.round.iso");

        // 2 full clusters + partial last cluster with 13 sectors of
        // real data (51 padding sectors fall into the raw region that
        // follows). Exercises both the partial-cluster encoder path
        // (zero-padded payload recompute, chunk-local exception
        // filtering) and the partial-chunk decoder path at both the
        // 2 MiB and 128 KiB chunk sizes.
        let original = make_fake_wii_iso_with_partial_partition(2, 13);
        tokio::fs::write(&iso, &original).await.unwrap();

        let opts_2m = RvzCompressOptions {
            chunk_size: 2 * 1024 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz_2m, opts_2m, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        decompress_disc(&rvz_2m, &restored_2m, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(
            original,
            tokio::fs::read(&restored_2m).await.unwrap(),
            "partial-cluster round-trip at 2 MiB chunks must be byte-identical"
        );

        let opts_128k = RvzCompressOptions {
            chunk_size: 128 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz_128k, opts_128k, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        decompress_disc(&rvz_128k, &restored_128k, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(
            original,
            tokio::fs::read(&restored_128k).await.unwrap(),
            "partial-cluster round-trip at 128 KiB chunks must be byte-identical"
        );
    }

    #[tokio::test]
    async fn streaming_disc_reader_matches_full_decompress_on_gamecube() {
        use crate::nintendo::disc::rvz::decompress::RvzDiscReader;
        use std::io::{Read, Seek, SeekFrom};

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        let original = make_fake_gamecube_iso(5 * 1024 * 1024 + 4096);
        tokio::fs::write(&iso, &original).await.unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let expected = tokio::fs::read(&restored).await.unwrap();

        let mut reader = RvzDiscReader::open(&rvz).unwrap();
        assert_eq!(reader.iso_size(), expected.len() as u64);

        let probes: &[(u64, usize)] = &[
            (0, 0x80),
            (0x80, 0x440),
            (0x10000, 4096),
            (expected.len() as u64 - 1024, 1024),
            (0x300000, 65536),
            (0, 4096),
        ];
        for &(off, len) in probes {
            reader.seek(SeekFrom::Start(off)).unwrap();
            let mut buf = vec![0u8; len];
            let mut read_so_far = 0;
            while read_so_far < len {
                let n = reader.read(&mut buf[read_so_far..]).unwrap();
                if n == 0 {
                    break;
                }
                read_so_far += n;
            }
            buf.truncate(read_so_far);
            let end = (off as usize + read_so_far).min(expected.len());
            assert_eq!(
                buf,
                expected[off as usize..end],
                "streaming reader mismatch at {}..{}",
                off,
                end
            );
        }
    }

    #[tokio::test]
    async fn streaming_disc_reader_matches_full_decompress_on_wii_partition() {
        use crate::nintendo::disc::rvz::decompress::RvzDiscReader;
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use std::io::{Read, Seek, SeekFrom};

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        let original = make_fake_wii_iso_with_partition(2);
        tokio::fs::write(&iso, &original).await.unwrap();
        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let expected = tokio::fs::read(&restored).await.unwrap();

        let mut reader = RvzDiscReader::open(&rvz).unwrap();
        assert_eq!(reader.iso_size(), expected.len() as u64);

        let probes: &[(u64, usize)] = &[
            (0, 0x80),
            (0x100, 0x200),
            (0x40000, 0x400),
            (expected.len() as u64 - 4096, 4096),
        ];
        for &(off, len) in probes {
            reader.seek(SeekFrom::Start(off)).unwrap();
            let mut buf = vec![0u8; len];
            let mut read_so_far = 0;
            while read_so_far < len {
                let n = reader.read(&mut buf[read_so_far..]).unwrap();
                if n == 0 {
                    break;
                }
                read_so_far += n;
            }
            buf.truncate(read_so_far);
            let end = (off as usize + read_so_far).min(expected.len());
            assert_eq!(
                buf,
                expected[off as usize..end],
                "wii streaming reader mismatch at {}..{}",
                off,
                end
            );
        }
    }

    #[tokio::test]
    async fn wii_with_real_partition_round_trips() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        // 2 clusters = 4 MiB of partition data, plenty to exercise the
        // partition pipeline (encrypt, hash, decrypt, exception list).
        let original = make_fake_wii_iso_with_partition(2);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(
            original.len(),
            result.len(),
            "Wii partition round-trip should preserve file size"
        );
        assert_eq!(
            original, result,
            "Wii partition round-trip must be byte-identical"
        );
    }

    #[tokio::test]
    async fn wii_round_trip_small() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("wii.round.iso");

        let original = make_fake_wii_iso(3 * 1024 * 1024 + 17);
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "Wii round trip must be byte-identical");
    }

    #[tokio::test]
    async fn gamecube_with_zero_stretch_uses_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        // 4 MiB total: 0x80 header, then ~1 MiB of pattern, then ~3 MiB of
        // zeros, then a tail of pattern. With the default 128 KiB chunk size
        // the zero stretch covers many full chunks.
        let mut original = make_fake_gamecube_iso(4 * 1024 * 1024);
        for byte in original.iter_mut().skip(1024 * 1024).take(3 * 1024 * 1024) {
            *byte = 0;
        }
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "round-trip with sentinels must be exact");

        // Sanity-check that the encoder shrunk the output. With 3 MiB of
        // zeros this should be well under half the original size.
        let rvz_size = tokio::fs::metadata(&rvz).await.unwrap().len();
        assert!(
            rvz_size < (original.len() / 2) as u64,
            "expected sentinel-shrunk output, got {rvz_size} for {} byte input",
            original.len()
        );
    }

    #[tokio::test]
    async fn mostly_zero_disc_verifies_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        // 64 MiB at 32 KiB chunks with only the boot head intact: every
        // chunk is an AllZero sentinel, so the container shrinks far
        // below its own raw group table (2048 groups * 12 bytes = 24 KiB
        // of descriptors in a few-KiB file). The table bound rejects
        // exactly this shape as truncated when it under-declares the
        // table's backing.
        let mut original = make_fake_gamecube_iso(64 * 1024 * 1024);
        for byte in original.iter_mut().skip(0x80) {
            *byte = 0;
        }
        tokio::fs::write(&iso, &original).await.unwrap();

        let opts = RvzCompressOptions {
            chunk_size: 32 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz, opts, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let rvz_len = tokio::fs::metadata(&rvz).await.unwrap().len();
        let raw_table_len = (64u64 * 1024 * 1024 / (32 * 1024)) * 12;
        assert!(
            rvz_len < raw_table_len,
            "fixture regression: sparse container {rvz_len} must be smaller than its raw table"
        );
        assert!(
            verify_rvz_structure(&rvz, &CancelToken::new())
                .unwrap()
                .ok()
        );

        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let result = tokio::fs::read(&restored).await.unwrap();
        assert_eq!(original, result, "sparse disc round trip must be exact");
    }

    #[test]
    fn sparse_disc_with_all_zero_first_chunk_reads_byte_exact() {
        use crate::nintendo::disc::rvz::decompress::RvzDiscReader;
        use crate::nintendo::disc::rvz::verify::test_support;
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let rvz = dir.path().join("sparse.rvz");

        // Hand-built container: the dhead covers 0..0x80 and the single
        // raw region starts at the unaligned offset 0x80, so its only
        // group, a `data_size == 0` all-zero sentinel covering the
        // chunk from the sector-aligned effective start 0, must be
        // sliced at 0x80. A reader that stalls at the sentinel or
        // mis-slices the padding never reaches the zero fill past the
        // region end.
        std::fs::write(
            &rvz,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                1,
                &[WiaRawData {
                    raw_data_off: 0x80,
                    raw_data_size: 0x1000,
                    group_index: 0,
                    n_groups: 1,
                }],
                &[RvzGroup {
                    data_off4: 0,
                    data_size: 0,
                    rvz_packed_size: 0,
                }],
                &[],
                |_| {},
            ),
        )
        .unwrap();

        let mut reader = RvzDiscReader::open(&rvz).unwrap();
        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        assert_eq!(
            got.len() as u64,
            reader.iso_size(),
            "reader must return the full iso_file_size"
        );
        assert!(
            got.iter().all(|&b| b == 0),
            "sparse disc must read as zeros"
        );
    }

    #[tokio::test]
    async fn small_chunk_size_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        let restored = dir.path().join("game.round.iso");

        let original = make_fake_gamecube_iso(512 * 1024 + 11);
        tokio::fs::write(&iso, &original).await.unwrap();

        let opts = RvzCompressOptions {
            chunk_size: 32 * 1024,
            ..RvzCompressOptions::default()
        };
        compress_disc(&iso, &rvz, opts, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        assert_eq!(original, tokio::fs::read(&restored).await.unwrap());
    }

    #[tokio::test]
    async fn invalid_chunk_size_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("game.iso");
        let rvz = dir.path().join("game.rvz");
        tokio::fs::write(&iso, make_fake_gamecube_iso(64 * 1024))
            .await
            .unwrap();

        // Not a power of two.
        let opts = RvzCompressOptions {
            chunk_size: 100 * 1024,
            ..RvzCompressOptions::default()
        };
        let err = compress_disc(&iso, &rvz, opts, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::Custom(_)), "{err}");

        // Below MIN_CHUNK_SIZE.
        let opts = RvzCompressOptions {
            chunk_size: 16 * 1024,
            ..RvzCompressOptions::default()
        };
        let err = compress_disc(&iso, &rvz, opts, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::Custom(_)), "{err}");

        // Above MAX_CHUNK_SIZE: the partition decoder walks one 2 MiB
        // cluster of sectors per chunk, so the writer never emits more.
        let opts = RvzCompressOptions {
            chunk_size: 4 * 1024 * 1024,
            ..RvzCompressOptions::default()
        };
        let err = compress_disc(&iso, &rvz, opts, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::Custom(_)), "{err}");
    }

    #[tokio::test]
    async fn decompress_rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.rvz");
        tokio::fs::write(&bad, vec![0u8; 200]).await.unwrap();
        let out = dir.path().join("out.iso");
        let err = decompress_disc(&bad, &out, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, RvzError::InvalidMagic(_)));
    }

    /// Bidirectional byte-identical round-trip against the reference RVZ
    /// encoder and decoder. Gated behind env vars so CI stays
    /// deterministic:
    ///
    /// * `ROM_CONVERTO_DOLPHIN_PARITY_ISO`: optional GameCube ISO/GCM.
    /// * `ROM_CONVERTO_DOLPHIN_PARITY_WII_ISO`: optional Wii ISO.
    /// * `ROM_CONVERTO_DOLPHIN_TOOL`: path to the reference tool executable.
    ///
    /// For each ISO that's set, the test runs four steps:
    /// 1. Compress the input ISO with rom-converto (L5, 128 KiB).
    /// 2. Decompress that `.rvz` with the reference decoder
    ///    (`convert -f iso`) and assert byte equality (SHA-1) against
    ///    the input.
    /// 3. Compress the input ISO with the reference encoder
    ///    (`convert -f rvz -l 5 -b 131072`).
    /// 4. Decompress the reference encoder's `.rvz` with rom-converto
    ///    and assert byte equality (SHA-1) against the input.
    ///
    /// Skipped with a printed note when the tool is unset or no ISO
    /// env vars are set.
    #[tokio::test]
    async fn dolphin_parity_cross_tool_round_trip() {
        let dolphin_tool = match std::env::var("ROM_CONVERTO_DOLPHIN_TOOL") {
            Ok(p) => p,
            Err(_) => {
                eprintln!(
                    "skipping dolphin_parity_cross_tool_round_trip: ROM_CONVERTO_DOLPHIN_TOOL not set"
                );
                return;
            }
        };
        assert!(
            PathBuf::from(&dolphin_tool).is_file(),
            "ROM_CONVERTO_DOLPHIN_TOOL does not point at a file: {}",
            dolphin_tool
        );

        let gc_iso = std::env::var("ROM_CONVERTO_DOLPHIN_PARITY_ISO").ok();
        let wii_iso = std::env::var("ROM_CONVERTO_DOLPHIN_PARITY_WII_ISO").ok();
        if gc_iso.is_none() && wii_iso.is_none() {
            eprintln!(
                "skipping dolphin_parity_cross_tool_round_trip: no ISO env var set (ROM_CONVERTO_DOLPHIN_PARITY_ISO or ROM_CONVERTO_DOLPHIN_PARITY_WII_ISO)"
            );
            return;
        }

        if let Some(p) = gc_iso {
            run_cross_tool_parity(&PathBuf::from(&p), &dolphin_tool, "GameCube").await;
        }
        if let Some(p) = wii_iso {
            run_cross_tool_parity(&PathBuf::from(&p), &dolphin_tool, "Wii").await;
        }
    }

    /// Run both directions of the cross-tool round-trip on a single
    /// input ISO. Shared helper so the same four steps cover GameCube
    /// and Wii when their respective env vars are set.
    async fn run_cross_tool_parity(iso_path: &Path, dolphin_tool: &str, label: &str) {
        use sha1::{Digest, Sha1};

        assert!(
            iso_path.is_file(),
            "{label} ISO does not point at a file: {}",
            iso_path.display()
        );

        let input_sha1 = sha1_file(iso_path).await;

        let dir = tempfile::tempdir().unwrap();
        let ours_rvz = dir.path().join("ours.rvz");
        let ours_from_dolphin_iso = dir.path().join("ours.from_dolphin.iso");
        let dolphin_rvz = dir.path().join("dolphin.rvz");
        let dolphin_from_ours_iso = dir.path().join("dolphin.from_ours.iso");

        // Step 1: compress with rom-converto.
        let opts = RvzCompressOptions {
            chunk_size: 131072,
            compression_level: 5,
            ..RvzCompressOptions::default()
        };
        compress_disc(iso_path, &ours_rvz, opts, &NoProgress, CancelToken::new())
            .await
            .expect("our compress failed");

        // Step 2: the reference decoder decompresses our RVZ; result must hash-match.
        run_dolphin_with_timeout(
            dolphin_tool,
            &[
                "convert",
                "-i",
                ours_rvz.to_str().unwrap(),
                "-o",
                ours_from_dolphin_iso.to_str().unwrap(),
                "-f",
                "iso",
            ],
            600,
            &format!("{label} Dolphin decode of ours"),
        );
        let dolphin_decoded_sha1 = sha1_file(&ours_from_dolphin_iso).await;
        assert_eq!(
            input_sha1, dolphin_decoded_sha1,
            "[{label}] Dolphin's decode of our RVZ does not match the original ISO"
        );

        // Step 3: compress with the reference encoder.
        run_dolphin_with_timeout(
            dolphin_tool,
            &[
                "convert",
                "-i",
                iso_path.to_str().unwrap(),
                "-o",
                dolphin_rvz.to_str().unwrap(),
                "-f",
                "rvz",
                "-b",
                "131072",
                "-c",
                "zstd",
                "-l",
                "5",
            ],
            900,
            &format!("{label} Dolphin compress"),
        );

        // Step 4: this decoder on the reference encoder's RVZ must hash-match.
        decompress_disc(
            &dolphin_rvz,
            &dolphin_from_ours_iso,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .expect("our decompress failed on Dolphin's RVZ");
        let ours_decoded_sha1 = sha1_file(&dolphin_from_ours_iso).await;
        assert_eq!(
            input_sha1, ours_decoded_sha1,
            "[{label}] our decode of Dolphin's RVZ does not match the original ISO"
        );

        async fn sha1_file(path: &Path) -> [u8; 20] {
            let mut file = tokio::fs::File::open(path).await.expect("open");
            let mut hasher = Sha1::new();
            let mut buf = vec![0u8; 1 << 20];
            use tokio::io::AsyncReadExt;
            loop {
                let n = file.read(&mut buf).await.expect("read");
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            hasher.finalize().into()
        }
    }

    /// Run the external tool with a hard timeout. If the process does not
    /// finish within `timeout_secs`, kill it and panic. This prevents
    /// failed runs from hanging on a modal error dialog on Windows (the
    /// reference tool pops "Unable to open disc image" and similar on
    /// failure and blocks on user acknowledgment without a timeout).
    fn run_dolphin_with_timeout(tool: &str, args: &[&str], timeout_secs: u64, label: &str) {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let mut child = Command::new(tool)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("[{label}] failed to spawn DolphinTool: {e}"));

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "[{label}] DolphinTool exited with failure: {status}"
                    );
                    return;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("[{label}] DolphinTool exceeded {timeout_secs}s timeout; killed");
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => panic!("[{label}] DolphinTool wait failed: {e}"),
            }
        }
    }

    #[test]
    fn wii_exception_format_is_spec_compliant() {
        // Verify that pack_partition_chunk emits the format's
        // wia_except_list_t layout:
        //   [u16 BE count][count × (u16 BE offset, 20-byte hash)][0x1F0000 payloads]
        use crate::nintendo::rvl::constants::{
            WII_BLOCKS_PER_GROUP, WII_GROUP_PAYLOAD_SIZE, WII_SECTOR_PAYLOAD_SIZE,
        };
        use crate::nintendo::rvl::partition::{HashException, pack_partition_chunk};

        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|_| [0u8; WII_SECTOR_PAYLOAD_SIZE])
            .collect();
        let exceptions = vec![
            HashException {
                offset: 0x0042,
                hash: [0x11u8; 20],
            },
            HashException {
                offset: 0xFFE0,
                hash: [0x22u8; 20],
            },
        ];

        let packed = pack_partition_chunk(&exceptions, &payloads).unwrap();

        // u16 count in big-endian
        assert_eq!(&packed[0..2], &[0x00, 0x02]);
        // First entry: offset 0x0042 + 20 bytes of 0x11
        assert_eq!(&packed[2..4], &[0x00, 0x42]);
        assert_eq!(&packed[4..24], &[0x11u8; 20]);
        // Second entry: offset 0xFFE0 + 20 bytes of 0x22
        assert_eq!(&packed[24..26], &[0xFF, 0xE0]);
        assert_eq!(&packed[26..46], &[0x22u8; 20]);
        // Payloads follow immediately, no padding.
        assert_eq!(packed.len(), 2 + 2 * 22 + WII_GROUP_PAYLOAD_SIZE as usize);
        for b in &packed[46..] {
            assert_eq!(*b, 0);
        }
    }
}
