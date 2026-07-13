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

use crate::nintendo::rvz::constants::RVZ_MAGIC;
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::sha1::{compute_disc_hash, compute_file_head_hash};
use crate::nintendo::rvz::format::{RvzGroup, WiaDisc, WiaFileHead, WiaPart, WiaRawData};
use crate::nintendo::wbfs::build_disc_usage;
use crate::nintendo::wbfs::format::{
    DEFAULT_HD_SECTOR_SHIFT, DEFAULT_WBFS_SECTOR_SHIFT, WII_SECTOR_SIZE,
};
use crate::util::{CancelToken, ProgressReporter, await_with_progress_cancel, scratch_output_path};
use binrw::{BinRead, Endian};
use log::info;
use sink::{DiscSink, IsoSink, UsageFilter, WbfsSink};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::task;

pub async fn decompress_disc(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> RvzResult<()> {
    decompress_disc_cancellable(input, output, progress, CancelToken::new()).await
}

/// Like [`decompress_disc`] but observes `cancel` at region boundaries;
/// on cancel the partial ISO is removed.
pub async fn decompress_disc_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<()> {
    let iso_size_guess = tokio::fs::metadata(input).await?.len();
    progress.start(iso_size_guess, "Decompressing RVZ");

    let write_path = scratch_output_path(output)?;
    let input_owned: PathBuf = input.to_path_buf();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || -> RvzResult<u64> {
        decompress_blocking(&input_owned, &write_owned, bytes_done_bg, &cancel_bg)
    });

    let cleanup = decompress_cleanup(&write_path);
    let iso_size =
        match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await {
            Ok(size) => size,
            Err(err) => {
                let _ = tokio::fs::remove_file(&write_path).await;
                return Err(err);
            }
        };
    crate::util::publish_temp(write_path, output, true)?;

    info!(
        "Decompressed {} -> {} ({} bytes)",
        input.display(),
        output.display(),
        iso_size
    );
    Ok(())
}

fn decompress_cleanup(write_path: &Path) -> impl FnOnce() -> RvzError {
    let write_path = write_path.to_path_buf();
    move || {
        let _ = std::fs::remove_file(&write_path);
        RvzError::Cancelled
    }
}

/// Decompress an RVZ straight into a WBFS container without an
/// intermediate ISO, using the same parallel worker pool as
/// `.rvz -> .iso`. Builds the FST usage map first so unused (junk)
/// blocks are never decompressed, then reconstructs the used blocks in
/// parallel into a scrubbed `sink::WbfsSink`.
pub async fn decompress_disc_to_wbfs(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> RvzResult<()> {
    decompress_disc_to_wbfs_cancellable(input, output, progress, CancelToken::new()).await
}

/// Like [`decompress_disc_to_wbfs`] but observes `cancel` at region
/// boundaries; on cancel the partial WBFS is removed.
pub async fn decompress_disc_to_wbfs_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<()> {
    let rvz_size = tokio::fs::metadata(input).await?.len();
    progress.start(rvz_size, "Decompressing RVZ to WBFS");

    let write_path = scratch_output_path(output)?;
    let input_owned: PathBuf = input.to_path_buf();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = task::spawn_blocking(move || -> RvzResult<u64> {
        decompress_to_wbfs_blocking(&input_owned, &write_owned, bytes_done_bg, &cancel_bg)
    });

    let cleanup = decompress_cleanup(&write_path);
    let disc_size =
        match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await {
            Ok(size) => size,
            Err(err) => {
                let _ = tokio::fs::remove_file(&write_path).await;
                return Err(err);
            }
        };
    crate::util::publish_temp(write_path, output, true)?;

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

    let mut head_bytes = vec![0u8; crate::nintendo::rvz::format::WIA_FILE_HEAD_SIZE];
    reader.read_exact(&mut head_bytes)?;
    let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
    if head.magic != RVZ_MAGIC {
        return Err(RvzError::InvalidMagic(head.magic));
    }
    if compute_file_head_hash(&head) != head.file_head_hash {
        return Err(RvzError::HeaderHashMismatch);
    }

    let mut disc_bytes = vec![0u8; head.disc_size as usize];
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

    let parts: Vec<WiaPart> = if disc.n_part > 0 {
        reader.seek(SeekFrom::Start(disc.part_off))?;
        let mut buf = vec![0u8; disc.n_part as usize * crate::nintendo::rvz::format::WIA_PART_SIZE];
        reader.read_exact(&mut buf)?;
        let mut cur = Cursor::new(&buf);
        let mut out = Vec::with_capacity(disc.n_part as usize);
        for _ in 0..disc.n_part {
            out.push(WiaPart::read_options(&mut cur, Endian::Big, ())?);
        }
        out
    } else {
        Vec::new()
    };

    reader.seek(SeekFrom::Start(disc.raw_data_off))?;
    let mut raw_compressed = vec![0u8; disc.raw_data_size as usize];
    reader.read_exact(&mut raw_compressed)?;
    let raw_decompressed = zstd::bulk::decompress(
        &raw_compressed,
        disc.n_raw_data as usize * crate::nintendo::rvz::format::WIA_RAW_DATA_SIZE,
    )?;
    let mut raw_cursor = Cursor::new(&raw_decompressed);
    let mut raw_data = Vec::with_capacity(disc.n_raw_data as usize);
    for _ in 0..disc.n_raw_data {
        raw_data.push(WiaRawData::read_options(&mut raw_cursor, Endian::Big, ())?);
    }

    reader.seek(SeekFrom::Start(disc.group_off))?;
    let mut group_compressed = vec![0u8; disc.group_size as usize];
    reader.read_exact(&mut group_compressed)?;
    let group_decompressed = zstd::bulk::decompress(
        &group_compressed,
        disc.n_groups as usize * crate::nintendo::rvz::format::RVZ_GROUP_SIZE,
    )?;
    let mut group_cursor = Cursor::new(&group_decompressed);
    let mut groups: Vec<RvzGroup> = Vec::with_capacity(disc.n_groups as usize);
    for _ in 0..disc.n_groups {
        groups.push(RvzGroup::read_options(&mut group_cursor, Endian::Big, ())?);
    }

    Ok((shared_file, head, disc, parts, raw_data, groups))
}

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
            return Err(RvzError::Cancelled);
        }
        raw::decompress_raw_region(
            region,
            &groups,
            chunk_size,
            head.iso_file_size,
            &shared_file,
            None,
            &mut sink,
            &bytes_done,
        )?;
    }

    for part in &parts {
        if cancel.is_cancelled() {
            return Err(RvzError::Cancelled);
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
            return Err(RvzError::Cancelled);
        }
        raw::decompress_raw_region(
            region,
            &groups,
            chunk_size,
            head.iso_file_size,
            &shared_file,
            Some(&filter),
            &mut sink,
            &bytes_done,
        )?;
    }

    for part in &parts {
        if cancel.is_cancelled() {
            return Err(RvzError::Cancelled);
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
