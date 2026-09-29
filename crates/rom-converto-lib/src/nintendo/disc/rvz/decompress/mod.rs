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
    RVZ_GROUP_SIZE, RvzGroup, WIA_DISC_SIZE, WIA_FILE_HEAD_SIZE, WIA_PART_SIZE, WIA_RAW_DATA_SIZE,
    WiaDisc, WiaFileHead, WiaPart, WiaRawData,
};
use crate::nintendo::disc::wbfs::build_disc_usage;
use crate::nintendo::disc::wbfs::format::{
    DEFAULT_HD_SECTOR_SHIFT, DEFAULT_WBFS_SECTOR_SHIFT, WII_SECTOR_SIZE,
};
use crate::util::{CancelToken, Cancelled, ProgressReporter, run_scratch_write, validate_extent};
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

    let file_len = reader.get_ref().metadata()?.len();
    if head.disc_size < WIA_DISC_SIZE as u32 {
        return Err(RvzError::Custom(
            "disc struct is smaller than the fixed RVZ structure".into(),
        ));
    }
    validate_extent(
        WIA_FILE_HEAD_SIZE as u64,
        WIA_DISC_SIZE as u64,
        file_len,
        "disc struct",
    )?;
    let mut disc_bytes = vec![0u8; WIA_DISC_SIZE];
    reader.read_exact(&mut disc_bytes)?;
    let disc = WiaDisc::read_options(&mut Cursor::new(&disc_bytes), Endian::Big, ())?;
    if compute_disc_hash(&disc) != head.disc_hash {
        return Err(RvzError::DiscHashMismatch);
    }
    if disc.compression != 5 {
        return Err(RvzError::UnsupportedCompression(disc.compression));
    }
    if disc.disc_type != 1 && disc.disc_type != 2 {
        return Err(RvzError::UnsupportedDiscType(disc.disc_type));
    }
    let chunk_size = disc.chunk_size;
    // The Wii partition decoder indexes one cluster (2 MiB) of sectors per
    // chunk, so partitioned discs with chunks past MAX_CHUNK_SIZE are not
    // decodable here; raw-only GameCube images have no such limit.
    if disc.n_part > 0 && chunk_size > crate::nintendo::disc::rvz::constants::MAX_CHUNK_SIZE {
        return Err(RvzError::Custom(format!(
            "unsupported RVZ chunk size {chunk_size:#x}"
        )));
    }
    let mut parts = Vec::new();
    if disc.n_part > 0 {
        let part_bytes = (disc.n_part as u64)
            .checked_mul(WIA_PART_SIZE as u64)
            .ok_or_else(|| RvzError::Custom("partition table size overflows".into()))?;
        validate_extent(disc.part_off, part_bytes, file_len, "partition table")?;
        // The validated extent bounds the table by the file size, so a
        // hostile count can no longer size the reservation.
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

    validate_extent(
        disc.raw_data_off,
        disc.raw_data_size as u64,
        file_len,
        "raw table",
    )?;
    let mut raw_reader = BufReader::new(File::open(input)?);
    raw_reader.seek(SeekFrom::Start(disc.raw_data_off))?;
    let mut raw_decoder =
        zstd::stream::read::Decoder::new(raw_reader.take(disc.raw_data_size as u64))?;
    // The declared counts are not bounded by any file extent (only the
    // compressed table size is), so the vectors grow row by row; the read
    // loops fail at EOF.
    let mut raw_data = Vec::new();
    for _ in 0..disc.n_raw_data {
        let mut row = [0u8; WIA_RAW_DATA_SIZE];
        raw_decoder.read_exact(&mut row)?;
        raw_data.push(WiaRawData::read_options(
            &mut Cursor::new(row),
            Endian::Big,
            (),
        )?);
    }
    let mut extra = [0u8; 1];
    if raw_decoder.read(&mut extra)? != 0 {
        return Err(RvzError::Custom(
            "raw table expands past declared count".into(),
        ));
    }

    validate_extent(
        disc.group_off,
        disc.group_size as u64,
        file_len,
        "group table",
    )?;
    let mut group_reader = BufReader::new(File::open(input)?);
    group_reader.seek(SeekFrom::Start(disc.group_off))?;
    let mut group_decoder =
        zstd::stream::read::Decoder::new(group_reader.take(disc.group_size as u64))?;
    let mut groups: Vec<RvzGroup> = Vec::new();
    for _ in 0..disc.n_groups {
        let mut row = [0u8; RVZ_GROUP_SIZE];
        group_decoder.read_exact(&mut row)?;
        groups.push(RvzGroup::read_options(
            &mut Cursor::new(row),
            Endian::Big,
            (),
        )?);
    }
    let mut extra = [0u8; 1];
    if group_decoder.read(&mut extra)? != 0 {
        return Err(RvzError::Custom(
            "group table expands past declared count".into(),
        ));
    }
    // Individual group extents are checked only when the group is accessed;
    // unused entries in a valid table need not point into this file.

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

    // Dolphin stores the first 0x80 bytes of the disc in
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
