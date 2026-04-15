//! RVZ decompression entry point.
//!
//! # Pipeline
//!
//! 1. [`decompress_disc`] is the async public entry. It hands the
//!    whole sync pipeline to [`tokio::task::spawn_blocking`] and
//!    polls a shared `AtomicU64` for progress, mirroring
//!    [`super::compress`].
//! 2. [`decompress_blocking`] reads the RVZ header, disc struct,
//!    partition table, raw-data table, and group table via a
//!    throwaway `BufReader` on the main thread, then opens a
//!    second file handle as `Arc<std::fs::File>` for the worker
//!    pools. Positional reads via
//!    [`crate::util::pread::file_read_exact_at`] let all workers
//!    share that one handle without seek contention.
//! 3. Every raw-data region is dispatched through
//!    [`raw::parallel_decompress_raw_region`] and every Wii
//!    partition through [`partition::parallel_decompress_partition`].
//!    Both pool-pump closures run through
//!    [`crate::util::worker_pool::drive`] so chunks are submitted
//!    in source order and output is written back in the same order
//!    despite out-of-order worker completion.
//!
//! # Submodule layout
//!
//! * [`raw`]: raw-region decompress worker pool.
//! * [`partition`]: Wii partition decompress worker pool.
//! * [`crate::util::worker_pool`]: shared generic pool.

pub mod partition;
pub mod raw;

use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::nintendo::rvz::constants::RVZ_MAGIC;
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::sha1::{compute_disc_hash, compute_file_head_hash};
use crate::nintendo::rvz::format::{RvzGroup, WiaDisc, WiaFileHead, WiaPart, WiaRawData};
use crate::util::ProgressReporter;
use binrw::{BinRead, Endian};
use log::info;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::task;

pub async fn decompress_disc(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> RvzResult<()> {
    let iso_size_guess = tokio::fs::metadata(input).await?.len();
    progress.start(iso_size_guess, "Decompressing RVZ...");

    let input_owned: PathBuf = input.to_path_buf();
    let output_owned: PathBuf = output.to_path_buf();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let mut handle = task::spawn_blocking(move || -> RvzResult<u64> {
        decompress_blocking(&input_owned, &output_owned, bytes_done_bg)
    });

    let iso_size = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => break result??,
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    info!(
        "Decompressed {} -> {} ({} bytes)",
        input.display(),
        output.display(),
        iso_size
    );
    Ok(())
}

fn decompress_blocking(input: &Path, output: &Path, bytes_done: Arc<AtomicU64>) -> RvzResult<u64> {
    // Shared file handle for the worker pools' positional reads.
    // A second handle is opened below for the main thread's
    // sequential header/table reads so no cursor is shared across
    // threads.
    let shared_file = Arc::new(std::fs::File::open(input)?);
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, std::fs::File::open(input)?);

    // Header.
    let mut head_bytes = vec![0u8; crate::nintendo::rvz::format::WIA_FILE_HEAD_SIZE];
    reader.read_exact(&mut head_bytes)?;
    let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
    if head.magic != RVZ_MAGIC {
        return Err(RvzError::InvalidMagic(head.magic));
    }
    let expected_head_hash = compute_file_head_hash(&head);
    if expected_head_hash != head.file_head_hash {
        return Err(RvzError::HeaderHashMismatch);
    }

    // Disc struct.
    let mut disc_bytes = vec![0u8; head.disc_size as usize];
    reader.read_exact(&mut disc_bytes)?;
    let disc = WiaDisc::read_options(&mut Cursor::new(&disc_bytes), Endian::Big, ())?;
    let disc_hash = compute_disc_hash(&disc);
    if disc_hash != head.disc_hash {
        return Err(RvzError::DiscHashMismatch);
    }

    if disc.compression != 5 {
        return Err(RvzError::UnsupportedCompression(disc.compression));
    }
    if disc.disc_type != 1 && disc.disc_type != 2 {
        return Err(RvzError::UnsupportedDiscType(disc.disc_type));
    }

    // Partition table (uncompressed in the file).
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

    // Raw-data table (zstd compressed).
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

    // Group table (zstd compressed).
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

    // Pre-size the output so seek-to-end isn't needed for trailing
    // zero chunks. We still write sequentially below; the position
    // tracker elides redundant seeks so the BufWriter doesn't flush
    // on every iteration.
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, std::fs::File::create(output)?);
    if head.iso_file_size > 0 {
        writer.seek(SeekFrom::Start(head.iso_file_size - 1))?;
        writer.write_all(&[0u8])?;
        writer.seek(SeekFrom::Start(0))?;
    }

    // Dolphin stores the first 0x80 bytes of the original disc
    // inside `wia_disc_t.dhead`. The raw_data table covers the
    // disc starting at 0x80, so the reader has to write the dhead
    // bytes back separately.
    let dhead_bytes = std::cmp::min(head.iso_file_size, disc.dhead.len() as u64) as usize;
    if dhead_bytes > 0 {
        writer.write_all(&disc.dhead[..dhead_bytes])?;
    }
    let mut writer_pos: u64 = dhead_bytes as u64;

    let chunk_size = disc.chunk_size as u64;

    // Raw regions: one `Pool` per region keeps the worker set
    // thin (no long-lived cross-region state) and lets dispatch
    // amortize across the region's ~10 chunks.
    for region in &raw_data {
        raw::parallel_decompress_raw_region(
            region,
            &groups,
            chunk_size,
            head.iso_file_size,
            &shared_file,
            &mut writer,
            &mut writer_pos,
            &bytes_done,
        )?;
    }

    // Wii partitions. Sectors past `total_data_size` (padding
    // inside the last cluster on disc) are NOT stored in any
    // chunk; we zero-pad their plaintext payloads before
    // recomputing the hash hierarchy so the encoder and decoder
    // agree on the recompute baseline, then re-encrypt only the
    // sectors that fall inside `total_data_size` and leave the
    // tail untouched.
    for part in &parts {
        partition::parallel_decompress_partition(
            part,
            &groups,
            chunk_size,
            &shared_file,
            &mut writer,
            &mut writer_pos,
            &bytes_done,
        )?;
    }

    writer.flush()?;
    Ok(head.iso_file_size)
}

// Silence unused-const warning on platforms where
// `WII_SECTOR_SIZE_U64` is only read via child modules.
#[allow(dead_code)]
const _: u64 = WII_SECTOR_SIZE_U64;
