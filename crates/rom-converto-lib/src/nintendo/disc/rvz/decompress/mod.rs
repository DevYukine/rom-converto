//! RVZ decompression entry point.
//!
//! # Pipeline
//!
//! 1. [`decompress_disc`] / [`decompress_disc_to_wbfs`] are the async
//!    public entries. They hand the sync pipeline to
//!    [`tokio::task::spawn_blocking`] and poll a shared `AtomicU64`
//!    for progress, mirroring [`super::compress`].
//! 2. `parse_rvz_metadata` reads the RVZ header, disc struct,
//!    partition table, raw-data table, and group table, and opens a
//!    shared `Arc<std::fs::File>` for the worker pools. Positional
//!    reads via [`crate::util::pread::file_read_exact_at`] let all
//!    workers share that one handle without seek contention.
//! 3. Every raw-data region runs through `raw::decompress_raw_region`
//!    and every Wii partition through `partition::decompress_partition`,
//!    both pumped via [`crate::util::worker_pool::drive`] so output
//!    lands in order despite out-of-order worker completion. The
//!    reconstructed bytes go to a `sink::DiscSink`: `sink::IsoSink`
//!    for `.iso`, or `sink::WbfsSink` (FST-scrubbed) for `.wbfs`.
//!
pub mod disc_reader;
pub mod partition;
pub mod raw;
pub mod sink;

pub use disc_reader::RvzDiscReader;

use crate::nintendo::disc::rvz::constants::RVZ_MAGIC;
use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use crate::nintendo::disc::rvz::format::sha1::{compute_disc_hash, compute_file_head_hash};
use crate::nintendo::disc::rvz::format::{
    RvzGroup, WIA_DISC_SIZE, WIA_PART_SIZE, WiaDisc, WiaFileHead, WiaPart, WiaRawData,
};
use crate::nintendo::disc::wbfs::build_disc_usage;
use crate::nintendo::disc::wbfs::format::{
    DEFAULT_HD_SECTOR_SHIFT, DEFAULT_WBFS_SECTOR_SHIFT, WII_SECTOR_SIZE,
};
use crate::util::{CancelToken, Cancelled, ProgressReporter, run_scratch_write};
use binrw::{BinRead, Endian};
use log::info;
use sink::{DiscSink, IsoSink, UsageFilter, WbfsSink};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Decompress the RVZ at `input` back to a plain ISO at `output`; on
/// cancel the partial ISO is removed.
pub async fn decompress_disc(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<()> {
    let iso_size_guess = tokio::fs::metadata(input).await?.len();
    progress.start(iso_size_guess, "Decompressing RVZ");

    let input_owned: PathBuf = input.to_path_buf();
    let iso_size = run_scratch_write(
        output,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| {
            decompress_blocking(&input_owned, &write_path, bytes_done, &cancel)
        },
    )
    .await?;

    info!(
        "Decompressed {} -> {} ({} bytes)",
        input.display(),
        output.display(),
        iso_size
    );
    Ok(())
}

/// Decompress the RVZ at `input` into a WBFS image at `output`; on
/// cancel the partial WBFS is removed.
pub async fn decompress_disc_to_wbfs(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<()> {
    let rvz_size = tokio::fs::metadata(input).await?.len();
    progress.start(rvz_size, "Decompressing RVZ to WBFS");

    let input_owned: PathBuf = input.to_path_buf();
    let disc_size = run_scratch_write(
        output,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| {
            decompress_to_wbfs_blocking(&input_owned, &write_path, bytes_done, &cancel)
        },
    )
    .await?;

    info!(
        "Decompressed {} -> {} ({} bytes)",
        input.display(),
        output.display(),
        disc_size
    );
    Ok(())
}

/// Parsed RVZ metadata plus a shared file handle for the worker pools:
/// `(shared_file, head, disc, parts, raw_data, groups)`.
type RvzMetadata = (
    Arc<File>,
    WiaFileHead,
    WiaDisc,
    Vec<WiaPart>,
    Vec<WiaRawData>,
    Vec<RvzGroup>,
);

/// Read the RVZ header and metadata tables and open a shared file
/// handle for the worker pools. Shared by the ISO and WBFS paths.
fn parse_rvz_metadata(input: &Path) -> RvzResult<RvzMetadata> {
    // Shared handle for the worker pools' positional reads. A separate
    // handle backs the sequential header/table reads below so no cursor
    // is shared across threads.
    let shared_file = Arc::new(File::open(input)?);
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, File::open(input)?);

    let mut head_bytes = vec![0u8; crate::nintendo::disc::rvz::format::WIA_FILE_HEAD_SIZE];
    reader.read_exact(&mut head_bytes)?;
    let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
    if head.magic != RVZ_MAGIC {
        return Err(RvzError::InvalidMagic(head.magic));
    }
    if compute_file_head_hash(&head) != head.file_head_hash {
        return Err(RvzError::HeaderHashMismatch);
    }
    // The container declares its own size; falling short of that is
    // truncation no matter what the tables still manage to parse.
    let file_len = reader.get_ref().metadata()?.len();
    if file_len < head.wia_file_size {
        return Err(RvzError::Truncated {
            expected: head.wia_file_size,
            actual: file_len,
        });
    }
    crate::nintendo::disc::rvz::check_disc_size(&head, file_len)?;

    if head.disc_size < WIA_DISC_SIZE as u32 {
        return Err(RvzError::Custom(
            "disc struct is smaller than the fixed RVZ structure".into(),
        ));
    }
    // `check_disc_size` already ensured disc_size <= file_len - head, and
    // the guard above ensures disc_size >= WIA_DISC_SIZE, so the fixed
    // struct fits inside the file.
    let mut disc_bytes = vec![0u8; WIA_DISC_SIZE];
    reader.read_exact(&mut disc_bytes)?;
    let disc = WiaDisc::read_options(&mut Cursor::new(&disc_bytes), Endian::Big, ())?;
    if compute_disc_hash(&disc) != head.disc_hash {
        return Err(RvzError::DiscHashMismatch);
    }
    crate::nintendo::disc::rvz::check_table_bounds(&disc, file_len, head.iso_file_size)?;
    if disc.compression != 5 {
        return Err(RvzError::UnsupportedCompression(disc.compression));
    }
    if disc.disc_type != 1 && disc.disc_type != 2 {
        return Err(RvzError::UnsupportedDiscType(disc.disc_type));
    }
    let mut parts = Vec::new();
    if disc.n_part > 0 {
        // `check_table_bounds` above already bounded the partition
        // table against the file length.
        parts.reserve(disc.n_part as usize);
        reader.seek(SeekFrom::Start(disc.part_off))?;
        for _ in 0..disc.n_part {
            let mut row = [0u8; WIA_PART_SIZE];
            reader.read_exact(&mut row)?;
            parts.push(WiaPart::read_options(
                &mut Cursor::new(row),
                Endian::Big,
                (),
            )?);
        }
    }

    let groups =
        crate::nintendo::disc::rvz::read_group_table(&mut reader, &disc, head.iso_file_size)?;
    crate::nintendo::disc::rvz::check_group_data_bounds(&groups, file_len)?;
    let raw_data = crate::nintendo::disc::rvz::read_raw_data_table(&mut reader, &disc)?;
    crate::nintendo::disc::rvz::check_group_indices(
        &raw_data,
        &parts,
        &groups,
        head.iso_file_size,
        disc.chunk_size,
    )?;

    Ok((shared_file, head, disc, parts, raw_data, groups))
}

/// Synchronous RVZ-to-ISO decompression: parses the container metadata,
/// then decodes every raw region and partition in order into `output`.
pub fn decompress_blocking(
    input: &Path,
    output: &Path,
    bytes_done: Arc<AtomicU64>,
    cancel: &CancelToken,
) -> RvzResult<u64> {
    let (shared_file, head, disc, parts, raw_data, groups) = parse_rvz_metadata(input)?;
    let chunk_size = disc.chunk_size as u64;

    let mut sink = IsoSink::create(output, head.iso_file_size)?;

    // The container stores the first 0x80 bytes of the disc in
    // `wia_disc_t.dhead`; the raw_data table covers the disc from 0x80,
    // so the head bytes are written separately.
    let dhead_bytes = std::cmp::min(head.iso_file_size, disc.dhead.len() as u64) as usize;
    if dhead_bytes > 0 {
        sink.write_at(0, &disc.dhead[..dhead_bytes])?;
    }

    for region in &raw_data {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        raw::decompress_raw_region(
            raw::RawRegionDecode {
                region,
                groups: &groups,
                chunk_size,
                iso_file_size: head.iso_file_size,
                file: &shared_file,
                usage: None,
                bytes_done: &bytes_done,
            },
            &mut sink,
        )?;
    }

    for part in &parts {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        partition::decompress_partition(
            part,
            &groups,
            chunk_size,
            &shared_file,
            None,
            &mut sink,
            &bytes_done,
        )?;
    }

    sink.finish()?;
    Ok(head.iso_file_size)
}

fn decompress_to_wbfs_blocking(
    input: &Path,
    output: &Path,
    bytes_done: Arc<AtomicU64>,
    cancel: &CancelToken,
) -> RvzResult<u64> {
    // FST usage map (serial, cheap): only the partition headers and FST
    // are decrypted here, not the whole disc.
    let mut reader = RvzDiscReader::open(input)?;
    let disc_size = reader.iso_size();
    let usage = build_disc_usage(&mut reader, disc_size)?;
    drop(reader);

    let (shared_file, head, disc, parts, raw_data, groups) = parse_rvz_metadata(input)?;
    let chunk_size = disc.chunk_size as u64;

    let wbfs_sec_sz = 1u64 << DEFAULT_WBFS_SECTOR_SHIFT;
    let filter = UsageFilter {
        usage: &usage,
        wbfs_sec_sz,
        sectors_per_block: wbfs_sec_sz / WII_SECTOR_SIZE,
    };
    let mut sink = WbfsSink::create(
        output,
        &usage,
        disc_size,
        DEFAULT_HD_SECTOR_SHIFT,
        DEFAULT_WBFS_SECTOR_SHIFT,
    )?;

    let dhead_bytes = std::cmp::min(disc_size, disc.dhead.len() as u64) as usize;
    if dhead_bytes > 0 {
        sink.write_at(0, &disc.dhead[..dhead_bytes])?;
    }

    for region in &raw_data {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        raw::decompress_raw_region(
            raw::RawRegionDecode {
                region,
                groups: &groups,
                chunk_size,
                iso_file_size: head.iso_file_size,
                file: &shared_file,
                usage: Some(&filter),
                bytes_done: &bytes_done,
            },
            &mut sink,
        )?;
    }

    for part in &parts {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        partition::decompress_partition(
            part,
            &groups,
            chunk_size,
            &shared_file,
            Some(&filter),
            &mut sink,
            &bytes_done,
        )?;
    }

    sink.finish()?;
    Ok(disc_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::rvz::format::WiaPartData;
    use crate::nintendo::disc::rvz::verify::test_support;
    use binrw::BinWrite;

    /// Writes a hand-built few-hundred-byte container: file head + disc
    /// struct (hashes computed, `tweak` applied first so corrupt fields
    /// stay hash-consistent) + compressed group table + raw table.
    fn write_fixture(
        dir: &tempfile::TempDir,
        name: &str,
        n_groups: u32,
        groups: &[RvzGroup],
        tweak: impl FnOnce(&mut WiaDisc),
    ) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(
            &path,
            test_support::build_rvz_custom([0u8; 128], 0, n_groups, &[], groups, &[], tweak),
        )
        .unwrap();
        path
    }

    /// A group descriptor claiming 0x7FFFFFFF stored bytes must read as
    /// Truncated, before any group I/O or decompression happens.
    #[test]
    fn parse_rejects_group_running_past_eof() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            &dir,
            "huge_group.rvz",
            1,
            &[RvzGroup {
                data_off4: 0,
                data_size: 0x7FFF_FFFF,
                rvz_packed_size: 0,
            }],
            |_| {},
        );
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::Truncated { .. }), "{err}");
    }

    /// A chunk size outside the container-level rule (a power of two of
    /// at least [`MIN_CHUNK_SIZE`], or a multiple of [`MAX_CHUNK_SIZE`])
    /// invalidates the geometric group bound and must read as
    /// InvalidChunkSize even for raw-only containers: the value feeds
    /// the bound in `read_group_table`. 3 MiB is neither a power of two
    /// nor a multiple of 2 MiB. Values past [`MAX_CHUNK_SIZE`] are a
    /// decode limitation instead when the container carries partitions
    /// (see `partitioned_container_rejects_oversized_chunk`) but remain
    /// decodable raw-only (see
    /// `raw_only_oversized_chunk_container_decodes`).
    #[test]
    fn parse_rejects_corrupt_chunk_size() {
        for chunk_size in [0u32, 3 * 1024 * 1024, u32::MAX] {
            let dir = tempfile::tempdir().unwrap();
            let path = write_fixture(
                &dir,
                &format!("chunk_{chunk_size}.rvz"),
                1,
                &[RvzGroup::new_compressed(0, 8, 0)],
                move |disc| disc.chunk_size = chunk_size,
            );
            let err = parse_rvz_metadata(&path).unwrap_err();
            assert!(matches!(err, RvzError::InvalidChunkSize(_, _, _)), "{err}");
        }
    }

    /// A container shorter than the `wia_file_size` its own head
    /// declares reads as Truncated right after the head is parsed (the
    /// same gate the structural verifier applies), before any table is
    /// read.
    #[test]
    fn parse_rejects_file_shorter_than_declared_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.rvz");
        let mut file = test_support::build_rvz(
            [0u8; 128],
            // A group whose stored bytes stay inside the FULL file: only
            // the declared-size gate can flag the cut.
            &[RvzGroup::new_compressed(0, 8, 0)],
            &[],
        );
        file.truncate(file.len() - 1);
        std::fs::write(&path, &file).unwrap();

        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::Truncated { .. }), "{err}");
    }

    /// A raw-only container with a chunk above `MAX_CHUNK_SIZE` (4 MiB,
    /// a legal power of two, and 6 MiB, a multiple of 2 MiB that is not)
    /// parses and decodes through the disc reader's whole-chunk path:
    /// without partitions there are no per-chunk exception lists and
    /// oversized chunks stream when needed, so the partition decoder's
    /// 2 MiB limit must not reject them.
    #[test]
    fn raw_only_oversized_chunk_container_decodes() {
        use std::io::Read;

        for chunk_size in [4 * 1024 * 1024u32, 6 * 1024 * 1024] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("raw_{chunk_size}.rvz"));
            // The single group stores the 128-byte dhead verbatim (group
            // data sits at 0x48, the head's dhead field), and one raw region
            // maps it to disc bytes [0x8000, 0x8080) of the 64 KiB image.
            let dhead: Vec<u8> = (0..128u32).map(|i| i as u8).collect();
            let mut dhead_arr = [0u8; 128];
            dhead_arr.copy_from_slice(&dhead);
            std::fs::write(
                &path,
                test_support::build_rvz_custom(
                    dhead_arr,
                    0,
                    1,
                    &[WiaRawData {
                        raw_data_off: 0x8000,
                        raw_data_size: 0x80,
                        group_index: 0,
                        n_groups: 1,
                    }],
                    // data_off4 0x16 -> byte offset 0x58: the serialized
                    // dhead field (disc struct at 0x48, dhead 16 bytes in).
                    &[RvzGroup::new_uncompressed(0x16, 128)],
                    &[],
                    move |disc| disc.chunk_size = chunk_size,
                ),
            )
            .unwrap();

            let parsed = parse_rvz_metadata(&path);
            assert!(
                parsed.is_ok(),
                "{chunk_size} raw-only container must parse: {:?}",
                parsed.err()
            );

            let mut reader = RvzDiscReader::open(&path).unwrap();
            assert_eq!(reader.iso_size(), 0x1_0000);
            let mut got = Vec::new();
            reader.read_to_end(&mut got).unwrap();
            let mut expected = vec![0u8; 0x1_0000];
            expected[..128].copy_from_slice(&dhead);
            expected[0x8000..0x8080].copy_from_slice(&dhead);
            assert_eq!(got, expected);
        }
    }

    /// A container that carries Wii partition data keeps the 2 MiB
    /// decode limit: the partition decoder walks one 2 MiB cluster of
    /// sectors per chunk, so a 4 MiB chunk reads as a decode-limitation
    /// error, not an invalid-container error.
    #[test]
    fn partitioned_container_rejects_oversized_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [WiaPartData {
                first_sector: 0,
                n_sectors: 0,
                group_index: 0,
                n_groups: 0,
            }; 2],
        };
        let mut part_bytes = Vec::new();
        part.write_options(&mut Cursor::new(&mut part_bytes), Endian::Big, ())
            .unwrap();
        let path = dir.path().join("wii_4mib.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom([0u8; 128], 1, 0, &[], &[], &part_bytes, |disc| {
                disc.chunk_size = 4 * 1024 * 1024;
                disc.part_off = disc.raw_data_off + u64::from(disc.raw_data_size);
            }),
        )
        .unwrap();

        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(err, RvzError::PartitionChunkTooLarge(0x40_0000, 0x20_0000)),
            "{err}"
        );
    }

    /// An inflated group count beyond the disc geometry reads as
    /// TableTooLarge, without allocating the claimed table.
    #[test]
    fn parse_rejects_inflated_group_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "many_groups.rvz", 100_000, &[], |_| {});
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::TableTooLarge { .. }), "{err}");
    }

    /// `n_raw_data` beyond what the partition geometry could produce
    /// reads as TableTooLarge before the raw table is read.
    #[test]
    fn parse_rejects_huge_n_raw_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(&dir, "huge_raw.rvz", 0, &[], |disc| {
            disc.n_raw_data = u32::MAX;
        });
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::TableTooLarge {
                    table: "raw_data",
                    entries: u32::MAX
                }
            ),
            "{err}"
        );
    }

    /// A raw-data region pointing past the group table reads as an
    /// out-of-range descriptor error naming the group table, before
    /// region decoding indexes groups.
    #[test]
    fn parse_rejects_region_group_index_past_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("region_oob.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                1,
                &[WiaRawData {
                    raw_data_off: 0,
                    raw_data_size: 0,
                    group_index: 1,
                    n_groups: 1,
                }],
                &[RvzGroup::new_compressed(0, 8, 0)],
                &[],
                |_| {},
            ),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("references groups 1..2")
                    && m.contains("group table holds 1 entries")),
            "{err}"
        );
    }

    /// Serializes `parts` after the metadata tables and points
    /// `disc.part_off` at them, so fixtures can exercise the partition
    /// branch of `check_group_indices`.
    fn write_part_fixture(
        dir: &tempfile::TempDir,
        name: &str,
        parts: &[WiaPart],
        raw_data: &[WiaRawData],
        n_groups: u32,
        groups: &[RvzGroup],
    ) -> std::path::PathBuf {
        let mut part_bytes = Vec::new();
        for part in parts {
            part.write_options(&mut Cursor::new(&mut part_bytes), Endian::Big, ())
                .unwrap();
        }
        let path = dir.path().join(name);
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                parts.len() as u32,
                n_groups,
                raw_data,
                groups,
                &part_bytes,
                |disc| disc.part_off = disc.raw_data_off + u64::from(disc.raw_data_size),
            ),
        )
        .unwrap();
        path
    }

    /// pd[0] and pd[1] must form one contiguous range inside the group
    /// table: `pd0.group_index + pd0.n_groups + pd1.n_groups` past the
    /// table (here len=10, pd0={9,1}, pd1={0,10}, so 20 > 10) reads as an
    /// out-of-range descriptor error naming the group table instead of
    /// letting the cluster walk index wrong groups or overflow its
    /// arithmetic.
    #[test]
    fn parse_rejects_partition_ranges_past_table() {
        let dir = tempfile::tempdir().unwrap();
        let groups: Vec<RvzGroup> = (0..10).map(|i| RvzGroup::new_compressed(i, 8, 0)).collect();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0,
                    group_index: 9,
                    n_groups: 1,
                },
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0,
                    group_index: 0,
                    n_groups: 10,
                },
            ],
        };
        let path = write_part_fixture(&dir, "overlap.rvz", &[part], &[], 10, &groups);
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("references groups 9..20")
                    && m.contains("group table holds 10 entries")),
            "{err}"
        );
    }

    /// Both per-partition ranges can fit the table individually while
    /// leaving a gap; the gap would make `build_partition_work_items`
    /// silently decode the wrong groups, so the contiguity requirement
    /// (`pd[1].group_index == pd[0].group_index + pd[0].n_groups`)
    /// rejects it. The fixture is geometry-consistent (both entries
    /// cover zero sectors and declare no groups), so only the
    /// contiguity check can fire.
    #[test]
    fn parse_rejects_gap_between_partition_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0,
                    group_index: 0,
                    n_groups: 0,
                },
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0,
                    group_index: 5,
                    n_groups: 0,
                },
            ],
        };
        let path = write_part_fixture(&dir, "gap.rvz", &[part], &[], 0, &[]);
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("group ranges are not contiguous") && m.contains("5 != 0")),
            "{err}"
        );
    }

    /// Raw-region counts above the per-partition writer geometry but
    /// within `check_table_bounds`' caps (the zstd ratio gate and
    /// `div_ceil(iso_file_size, 0x8000) + 64`) parse cleanly.
    #[test]
    fn parse_accepts_raw_regions_beyond_writer_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let regions: Vec<WiaRawData> = (0..6) // 3 * n_part(1) + 3
            .map(|_| WiaRawData {
                raw_data_off: 0,
                raw_data_size: 0,
                group_index: 0,
                n_groups: 0,
            })
            .collect();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [WiaPartData {
                first_sector: 0,
                n_sectors: 0,
                group_index: 0,
                n_groups: 0,
            }; 2],
        };
        let path = write_part_fixture(&dir, "extra_raw_geometry.rvz", &[part], &regions, 0, &[]);
        let (_, _, _, _, raw_data, _) = parse_rvz_metadata(&path).unwrap();
        assert_eq!(raw_data.len(), 6);
    }

    /// A raw region running past the ISO size the header declares reads
    /// as a corrupt-descriptor error, not file truncation: the file
    /// itself is intact.
    #[test]
    fn parse_rejects_raw_region_past_iso_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("region_past_iso.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                0,
                &[WiaRawData {
                    raw_data_off: 0x2_0000,
                    raw_data_size: 1,
                    group_index: 0,
                    n_groups: 0,
                }],
                &[],
                &[],
                |_| {},
            ),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("region at offset 131072 ends 131073")
                    && m.contains("65536-byte disc image")),
            "{err}"
        );
    }

    /// An `iso_file_size` no real GameCube/Wii disc could have reads as
    /// `ImplausibleIsoSize`, not `Truncated`.
    #[test]
    fn parse_rejects_implausible_iso_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge_iso.rvz");
        let mut file = test_support::build_rvz_custom([0u8; 128], 0, 0, &[], &[], &[], |_| {});
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.iso_file_size = 100 * 1024 * 1024 * 1024;
        head.file_head_hash =
            crate::nintendo::disc::rvz::format::sha1::compute_file_head_hash(&head);
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        std::fs::write(&path, &file).unwrap();

        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::ImplausibleIsoSize(..)), "{err}");
    }

    /// pd[0] claims one group without storing any sectors, and pd[1]
    /// claims two groups for 0x200 sectors: the format's
    /// `n_groups == div_ceil(n_sectors * 0x8000, chunk_size)` geometry
    /// is violated, so the cluster walk would decode a group span that
    /// does not cover the partition.
    #[test]
    fn parse_rejects_partition_n_groups_inconsistent_with_n_sectors() {
        let dir = tempfile::tempdir().unwrap();
        let groups: Vec<RvzGroup> = (0..4).map(|i| RvzGroup::new_compressed(i, 8, 0)).collect();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0,
                    group_index: 0,
                    n_groups: 1,
                },
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0x200,
                    group_index: 1,
                    n_groups: 2,
                },
            ],
        };
        let path = write_part_fixture(&dir, "bad_geometry.rvz", &[part], &[], 4, &groups);
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::Custom(_)), "{err}");
    }

    /// When pd[1] stores data, `pd[1].first_sector` must continue
    /// pd[0]: a discontiguous sector span would make the cluster walk
    /// decode the wrong disc range. The fixture is otherwise
    /// geometry-consistent (0x40 sectors = one 2 MiB group per entry),
    /// so only the first-sector contiguity check can fire.
    #[test]
    fn parse_rejects_discontiguous_pd1_first_sector() {
        let dir = tempfile::tempdir().unwrap();
        let groups: Vec<RvzGroup> = (0..2).map(|i| RvzGroup::new_compressed(i, 8, 0)).collect();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [
                // pd[0]: 0x40 sectors = 2 MiB = exactly one 2 MiB group.
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0x40,
                    group_index: 0,
                    n_groups: 1,
                },
                // pd[1] skips 7 sectors instead of continuing pd[0].
                WiaPartData {
                    first_sector: 0x47,
                    n_sectors: 0x40,
                    group_index: 1,
                    n_groups: 1,
                },
            ],
        };
        let path = write_part_fixture(&dir, "discontiguous.rvz", &[part], &[], 2, &groups);
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("data entries are not contiguous")
                    && m.contains("71 != 64")),
            "{err}"
        );
    }

    /// When pd[1] stores data, pd[0]'s byte span must be a whole
    /// number of chunks so the chunk walk crosses the pd[0]→pd[1]
    /// boundary on a chunk boundary; otherwise pd[1]'s groups start
    /// mid-chunk and must read an error.
    #[test]
    fn parse_rejects_pd0_span_not_chunk_multiple() {
        let dir = tempfile::tempdir().unwrap();
        let groups: Vec<RvzGroup> = (0..4).map(|i| RvzGroup::new_compressed(i, 8, 0)).collect();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [
                // pd[0]: 0x60 sectors = 3 MiB, not a multiple of the
                // 2 MiB chunk size (2 groups cover it).
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0x60,
                    group_index: 0,
                    n_groups: 2,
                },
                WiaPartData {
                    first_sector: 0x60,
                    n_sectors: 0x20,
                    group_index: 2,
                    n_groups: 1,
                },
            ],
        };
        let path = write_part_fixture(&dir, "unaligned_pd0.rvz", &[part], &[], 4, &groups);
        // The 0x80-sector span is 4 MiB: raise the declared ISO size to
        // cover it, so the span check cannot mask the chunk-boundary
        // rejection.
        let mut file = std::fs::read(&path).unwrap();
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.iso_file_size = 0x40_0000;
        head.file_head_hash =
            crate::nintendo::disc::rvz::format::sha1::compute_file_head_hash(&head);
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        std::fs::write(&path, &file).unwrap();

        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("is not a multiple of the")),
            "{err}"
        );
    }

    /// A partition whose sector span runs past the ISO size the header
    /// declares (`first_sector = u32::MAX` puts the span ~128 TiB past
    /// anything readable) reads as a corrupt-descriptor error, not file
    /// truncation: the cluster walk cannot consume image bytes the file
    /// never claimed.
    #[test]
    fn parse_rejects_partition_span_past_iso_size() {
        let dir = tempfile::tempdir().unwrap();
        let part = WiaPart {
            part_key: [0u8; 16],
            pd: [
                WiaPartData {
                    first_sector: u32::MAX,
                    n_sectors: 0,
                    group_index: 0,
                    n_groups: 0,
                },
                WiaPartData {
                    first_sector: 0,
                    n_sectors: 0,
                    group_index: 0,
                    n_groups: 0,
                },
            ],
        };
        let path = write_part_fixture(&dir, "span_past_iso.rvz", &[part], &[], 0, &[]);
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m) if m.contains("past the 65536-byte disc image")),
            "{err}"
        );
    }

    /// A raw region whose declared `n_groups` covers less than its
    /// sector-aligned span (`div_ceil(span, chunk_size)`) would leave
    /// the region's tail undecodable and must read an error.
    #[test]
    fn parse_rejects_region_n_groups_below_chunk_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("underdeclared.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                0,
                // 0x1_0000 bytes at chunk size 2 MiB need exactly one
                // group; declaring zero leaves the region uncovered.
                &[WiaRawData {
                    raw_data_off: 0,
                    raw_data_size: 0x1_0000,
                    group_index: 0,
                    n_groups: 0,
                }],
                &[],
                &[],
                |_| {},
            ),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::Custom(_)), "{err}");
    }

    /// A zero-size raw region needs no groups: `check_group_indices`
    /// must not demand the `div_ceil(offset % 0x8000, chunk_size)`
    /// groups its span formula fabricates for an empty region parked
    /// at a non-sector-aligned offset.
    #[test]
    fn parse_accepts_zero_size_raw_region_at_unaligned_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zero_region.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                0,
                &[WiaRawData {
                    raw_data_off: 0x80,
                    raw_data_size: 0,
                    group_index: 0,
                    n_groups: 0,
                }],
                &[],
                &[],
                |_| {},
            ),
        )
        .unwrap();
        let (_, _, _, _, raw_data, _) = parse_rvz_metadata(&path).unwrap();
        assert_eq!(raw_data.len(), 1);
    }

    /// A partition array whose declared location (`part_off`) runs
    /// past the end of the file reads as Truncated from
    /// `check_table_bounds`, before the table read fails with a bare
    /// I/O error.
    #[test]
    fn parse_rejects_partition_table_past_file_end() {
        use crate::nintendo::disc::rvz::format::WIA_PART_SIZE;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("part_off.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                1,
                0,
                &[],
                &[],
                // One real partition entry, but parked at a file offset
                // past EOF: part_off + n_part * WIA_PART_SIZE > file_len.
                &[0u8; WIA_PART_SIZE],
                |disc| disc.part_off = 0x10_0000,
            ),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(matches!(err, RvzError::Truncated { .. }), "{err}");
    }

    /// A group table whose stored size exceeds zstd's worst case for
    /// the declared entry count is rejected before the table bytes are
    /// read, even when the file is long enough to back the claim.
    #[test]
    fn parse_rejects_oversized_group_table_stored_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big_group_table.rvz");
        // One entry needs 12 bytes; the cap is compress_bound(12) + 64 KiB.
        // Claim 128 KiB of stored table and append enough trailing bytes
        // that the Truncated gate cannot reject the container first.
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                1,
                &[],
                &[RvzGroup::new_compressed(0, 8, 0)],
                &vec![0u8; 0x20100],
                |disc| disc.group_size = 0x2_0000,
            ),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("group table stores 131072 bytes")
                    && m.contains("worst case for 1 entries")),
            "{err}"
        );
    }

    /// A raw-data table whose stored size exceeds zstd's worst case for
    /// the declared entry count is rejected before the table bytes are
    /// read.
    #[test]
    fn parse_rejects_oversized_raw_table_stored_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big_raw_table.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                0,
                &[WiaRawData {
                    raw_data_off: 0,
                    raw_data_size: 0,
                    group_index: 0,
                    n_groups: 0,
                }],
                &[],
                &vec![0u8; 0x20100],
                |disc| disc.raw_data_size = 0x2_0000,
            ),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(&err, RvzError::Custom(m)
                if m.contains("raw_data table stores 131072 bytes")
                    && m.contains("worst case for 1 entries")),
            "{err}"
        );
    }

    /// A raw-only container with an unused garbage `part_off` (`n_part
    /// == 0`, offset past EOF) parses: the partition table does not
    /// exist, so its declared location carries no bytes to check.
    #[test]
    fn parse_accepts_unused_garbage_part_off_without_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage_part_off.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom(
                [0u8; 128],
                0,
                1,
                &[],
                &[RvzGroup::new_compressed(0, 8, 0)],
                &[],
                |disc| disc.part_off = 0x10_0000,
            ),
        )
        .unwrap();
        let parsed = parse_rvz_metadata(&path);
        assert!(
            parsed.is_ok(),
            "unused part_off must not reject a raw-only container: {:?}",
            parsed.err()
        );
    }

    /// A partition whose `data_size` is not a whole number of
    /// 0x8000-byte sectors (the partition header field has 4-byte
    /// granularity): the encoder must drop the sub-sector tail before
    /// the cluster walk, so the group count it emits matches the
    /// `n_groups` it declares in `pd[1]` and the next table entry's
    /// `group_index` lines up. At the 32 KiB chunk size the 4-byte
    /// tail would otherwise claim one extra group. The planner must
    /// likewise advance its cursor by the sector-truncated size, so
    /// the tail falls into the following raw region and the
    /// decompressed image matches the input byte-exact, tail
    /// included.
    #[tokio::test]
    async fn compress_truncates_non_sector_multiple_partition_data_size() {
        use crate::nintendo::disc::rvz::compress::{RvzCompressOptions, compress_disc};
        use crate::nintendo::disc::rvz::verify_rvz_structure;
        use crate::nintendo::rvl::constants::WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partial_partition;
        use crate::util::NoProgress;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("restored.iso");

        // 2 MiB + 1 sector of partition data; rewrite the header's
        // `data_size` (stored shifted right by 2) to add a 4-byte tail.
        const PARTITION_OFFSET: usize = 0x050000;
        let mut original = make_fake_wii_iso_with_partial_partition(1, 1);
        let partial = 0x20_0000u64 + 0x8000; // 2 MiB + 1 sector
        let ds_word = ((partial + 4) >> 2) as u32;
        let ds_off = PARTITION_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
        original[ds_off..ds_off + 4].copy_from_slice(&ds_word.to_be_bytes());
        tokio::fs::write(&iso, &original).await.unwrap();

        compress_disc(
            &iso,
            &rvz,
            RvzCompressOptions {
                chunk_size: 32 * 1024,
                ..RvzCompressOptions::default()
            },
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        assert!(
            verify_rvz_structure(&rvz, &CancelToken::new())
                .unwrap()
                .ok()
        );

        // The partition's declared group range must end exactly where
        // the next table entry (the raw region following the
        // partition) starts.
        let (_, _, _, parts, raw_data, groups) = parse_rvz_metadata(&rvz).unwrap();
        let part = &parts[0];
        let declared: u32 = part.pd.iter().map(|pd| pd.n_groups).sum();
        let start = part.pd[0].group_index;
        let next_entry = raw_data
            .iter()
            .map(|r| r.group_index)
            .find(|&g| g > start)
            .unwrap_or(groups.len() as u32);
        assert_eq!(
            declared,
            next_entry - start,
            "emitted group count must match the declared pd group range"
        );

        // The dropped tail is raw-region territory: decompression must
        // reproduce the input byte for byte, 4-byte tail included.
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&restored).await.unwrap(), original);
    }

    /// A sector-aligned partition that ends inside the disc's final
    /// 2 MiB cluster: the encoder's last cluster read must clamp to the
    /// bytes the ISO actually has (zero-filling the rest) instead of
    /// `read_exact`-ing a full cluster past EOF, and the result must
    /// decompress back byte-identical.
    #[tokio::test]
    async fn compresses_partition_ending_inside_final_cluster() {
        use crate::nintendo::disc::rvz::compress::{RvzCompressOptions, compress_disc};
        use crate::nintendo::disc::rvz::verify_rvz_structure;
        use crate::nintendo::rvl::constants::WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use crate::util::NoProgress;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("restored.iso");

        // Two physical clusters on disc, but only 3 MiB of
        // sector-aligned partition data before the ISO ends, so the
        // partition's final cluster read runs 1 MiB past EOF.
        const PARTITION_OFFSET: usize = 0x050000;
        const DATA_OFFSET_IN_PARTITION: u64 = 0x020000;
        let data_size = 3 * 1024 * 1024u64;
        let iso_size = PARTITION_OFFSET as u64 + DATA_OFFSET_IN_PARTITION + data_size;
        let mut original = make_fake_wii_iso_with_partition(2)[..iso_size as usize].to_vec();
        let ds_word = (data_size >> 2) as u32;
        let ds_off = PARTITION_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
        original[ds_off..ds_off + 4].copy_from_slice(&ds_word.to_be_bytes());
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

        assert!(
            verify_rvz_structure(&rvz, &CancelToken::new())
                .unwrap()
                .ok()
        );

        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&restored).await.unwrap(), original);
    }

    /// A partition whose declared data size runs past the ISO's end (a
    /// truncated dump) is clamped to the bytes the ISO holds, so the
    /// writer never emits
    /// partition groups its own geometry
    /// check rejects; the output verifies and round-trips byte-identical.
    #[tokio::test]
    async fn compresses_partition_declared_past_iso_end() {
        use crate::nintendo::disc::rvz::compress::{RvzCompressOptions, compress_disc};
        use crate::nintendo::disc::rvz::verify_rvz_structure;
        use crate::nintendo::rvl::constants::WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use crate::util::NoProgress;

        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("wii.iso");
        let rvz = dir.path().join("wii.rvz");
        let restored = dir.path().join("restored.iso");

        // The header claims 4 MiB of partition data but the ISO ends
        // after 3 MiB of it.
        const PARTITION_OFFSET: usize = 0x050000;
        const DATA_OFFSET_IN_PARTITION: u64 = 0x020000;
        let declared = 4 * 1024 * 1024u64;
        let present = 3 * 1024 * 1024u64;
        let iso_size = PARTITION_OFFSET as u64 + DATA_OFFSET_IN_PARTITION + present;
        let mut original = make_fake_wii_iso_with_partition(2)[..iso_size as usize].to_vec();
        let ds_word = (declared >> 2) as u32;
        let ds_off = PARTITION_OFFSET + WII_PARTITION_HEADER_DATA_SIZE_OFFSET;
        original[ds_off..ds_off + 4].copy_from_slice(&ds_word.to_be_bytes());
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

        assert!(
            verify_rvz_structure(&rvz, &CancelToken::new())
                .unwrap()
                .ok()
        );
        decompress_disc(&rvz, &restored, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&restored).await.unwrap(), original);
    }

    /// A raw-data entry count the ISO could not back geometrically,
    /// far above `div_ceil(iso_file_size, 0x8000) + 64`, reads as
    /// `TableTooLarge` from `check_table_bounds` before the table is
    /// decoded, even when a large declared `raw_data_size` sneaks the
    /// count past the zstd expansion-ratio gate.
    #[test]
    fn parse_rejects_raw_data_count_past_iso_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw_geometry.rvz");
        std::fs::write(
            &path,
            test_support::build_rvz_custom([0u8; 128], 0, 0, &[], &[], &[], |disc| {
                disc.n_raw_data = 100_000;
                // (0x1000 / 3 + 64) * 128 KiB >> 100_000 * 24 bytes of
                // decoded table, so only the geometric cap rejects this.
                disc.raw_data_size = 0x1000;
            }),
        )
        .unwrap();
        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::TableTooLarge {
                    table: "raw_data",
                    entries: 100_000
                }
            ),
            "{err}"
        );
    }

    /// A group table compressed by zstd's streaming encoder never
    /// pledges a content size, so its frame header can declare a window
    /// far larger than the tiny table could ever fill. The decoder runs
    /// with zstd's default window cap, so such a table decodes in one
    /// pass.
    #[test]
    fn group_table_compressed_without_pledged_size_decodes() {
        use crate::nintendo::disc::rvz::format::WIA_FILE_HEAD_SIZE;
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let groups = vec![RvzGroup::new_compressed(0, 8, 0)];
        let file = test_support::build_rvz([0u8; 128], &groups, &[]);

        // Re-encode the group table without a pledged size (level 19
        // carries a large default window in its frame header).
        let mut table_raw = Vec::new();
        {
            let mut cur = Cursor::new(&mut table_raw);
            for group in &groups {
                group.write_options(&mut cur, Endian::Big, ()).unwrap();
            }
        }
        let mut table = Vec::new();
        {
            let mut encoder = zstd::stream::write::Encoder::new(&mut table, 19).unwrap();
            encoder.write_all(&table_raw).unwrap();
            encoder.finish().unwrap();
        }

        // Rebuild the container: the group table sits right after the
        // disc struct, so the raw table moves by the size delta and the
        // disc struct (and its hashes) must describe the new layout.
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        let mut disc = WiaDisc::read_options(
            &mut Cursor::new(&file[WIA_FILE_HEAD_SIZE..]),
            Endian::Big,
            (),
        )
        .unwrap();
        let old_raw_table = file[disc.raw_data_off as usize
            ..(disc.raw_data_off + u64::from(disc.raw_data_size)) as usize]
            .to_vec();
        disc.group_size = table.len() as u32;
        disc.raw_data_off = disc.group_off + table.len() as u64;
        head.disc_hash = compute_disc_hash(&disc);
        head.file_head_hash = compute_file_head_hash(&head);

        let mut out = Vec::new();
        {
            let mut cur = Cursor::new(&mut out);
            head.write_options(&mut cur, Endian::Big, ()).unwrap();
            disc.write_options(&mut cur, Endian::Big, ()).unwrap();
        }
        out.extend_from_slice(&table);
        out.extend_from_slice(&old_raw_table);

        let path = dir.path().join("unpledged.rvz");
        std::fs::write(&path, &out).unwrap();

        let (_, _, _, _, _, parsed) = parse_rvz_metadata(&path).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].data_off4, groups[0].data_off4);
        assert_eq!(parsed[0].data_size, groups[0].data_size);
        assert_eq!(parsed[0].rvz_packed_size, groups[0].rvz_packed_size);
    }

    /// A table frame that decodes more than the declared entry count is
    /// rejected: the parsers read exactly the declared entries, so
    /// trailing descriptors must not be silently ignored.
    #[test]
    fn parse_rejects_table_expanding_past_declared_count() {
        use crate::nintendo::disc::rvz::format::WIA_FILE_HEAD_SIZE;

        let dir = tempfile::tempdir().unwrap();
        let groups = vec![RvzGroup::new_compressed(0, 8, 0)];
        let file = test_support::build_rvz([0u8; 128], &groups, &[]);

        // Encode the declared table bytes plus one trailing descriptor:
        // a longer stream is not the table the entry count declares.
        let mut table_raw = Vec::new();
        {
            let mut cur = Cursor::new(&mut table_raw);
            for group in &groups {
                group.write_options(&mut cur, Endian::Big, ()).unwrap();
            }
        }
        table_raw.extend_from_slice(&[0u8; 12]);
        let table = zstd::bulk::compress(&table_raw, 3).unwrap();

        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        let mut disc = WiaDisc::read_options(
            &mut Cursor::new(&file[WIA_FILE_HEAD_SIZE..]),
            Endian::Big,
            (),
        )
        .unwrap();
        let old_raw_table = file[disc.raw_data_off as usize
            ..(disc.raw_data_off + u64::from(disc.raw_data_size)) as usize]
            .to_vec();
        disc.group_size = table.len() as u32;
        disc.raw_data_off = disc.group_off + table.len() as u64;
        head.disc_hash = compute_disc_hash(&disc);
        head.file_head_hash = compute_file_head_hash(&head);

        let mut out = Vec::new();
        {
            let mut cur = Cursor::new(&mut out);
            head.write_options(&mut cur, Endian::Big, ()).unwrap();
            disc.write_options(&mut cur, Endian::Big, ()).unwrap();
        }
        out.extend_from_slice(&table);
        out.extend_from_slice(&old_raw_table);

        let path = dir.path().join("overlong_table.rvz");
        std::fs::write(&path, &out).unwrap();

        let err = parse_rvz_metadata(&path).unwrap_err();
        assert!(
            matches!(
                err,
                RvzError::DecompressedSizeMismatch {
                    expected: 12,
                    actual: 13
                }
            ),
            "{err}"
        );
    }
}
