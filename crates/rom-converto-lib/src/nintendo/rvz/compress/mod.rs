//! RVZ compression entry point.
//!
//! # Pipeline
//!
//! 1. [`compress_disc`] is the async public entry. It hands the whole
//!    sync pipeline to [`tokio::task::spawn_blocking`] and polls a
//!    shared `AtomicU64` for progress.
//! 2. [`compress_blocking`] is the sync pipeline. It reads the disc
//!    header, runs [`RegionPlan::gamecube`] / [`RegionPlan::wii`] to
//!    slice the disc into an ordered list of raw and partition
//!    regions, then walks the plan calling one of:
//!    * [`raw::parallel_encode_raw_region`] for every raw region,
//!    * [`partition::parallel_encode_partition_region`] for every
//!      Wii partition.
//! 3. After every region lands, the partition table, raw-data table,
//!    and group table are serialized and written back, followed by
//!    a final rewrite of the file head and disc struct now that
//!    every offset is known.
//!
//! # Submodule layout
//!
//! * [`raw`]: all pieces that only the raw-region encoder uses
//!   (work items, worker struct, produce/consume halves).
//! * [`partition`]: the Wii partition encoder, including the
//!   per-cluster decrypt/recompute/exception/re-encrypt pipeline.
//! * [`crate::nintendo::rvz::worker_pool`]: the shared generic
//!   [`Pool<W, O>`] + [`drive`] helper. Both submodules instantiate
//!   it with their own work-item and output types.
//!
//! # Compression methods
//!
//! Only zstd is emitted. The spec defines NONE, PURGE, BZIP2, LZMA,
//! LZMA2, and ZSTD; NONE and PURGE require 4-byte-aligned padding
//! rules documented in the spec, and BZIP2/LZMA/LZMA2 are not used
//! for writing by any modern producer. Supporting those methods for
//! either read or write is explicitly out of scope.

pub mod partition;
pub mod raw;

use crate::nintendo::dol::is_gamecube;
use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::nintendo::rvl::is_wii;
use crate::nintendo::rvz::constants::{
    DEFAULT_CHUNK_SIZE, DEFAULT_COMPRESSION_LEVEL, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE, RVZ_MAGIC,
};
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::sha1::{
    compute_disc_hash, compute_file_head_hash, compute_part_hash,
};
use crate::nintendo::rvz::format::{
    RvzGroup, WIA_DISC_SIZE, WIA_FILE_HEAD_SIZE, WiaDisc, WiaFileHead, WiaPart, WiaPartData,
    WiaRawData,
};
use crate::nintendo::rvz::regions::{DiscRegion, RegionPlan};
use crate::util::ProgressReporter;
use crate::util::worker_pool::{Pool, parallelism};
use binrw::{BinWrite, Endian};
use log::info;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::task;

/// Compression options for [`compress_disc`].
#[derive(Debug, Clone, Copy)]
pub struct RvzCompressOptions {
    /// Zstandard compression level, signed to allow negative levels.
    pub compression_level: i32,
    /// Chunk size in bytes. Must be a power of two between
    /// [`MIN_CHUNK_SIZE`] and [`MAX_CHUNK_SIZE`].
    pub chunk_size: u32,
    /// Reserved for the RVZ packing encoder. Currently a no-op: the
    /// decoder side of packing is fully implemented (so RVZ files
    /// Dolphin produces with packing decompress correctly), but the
    /// encoder side requires Dolphin's
    /// `LaggedFibonacciGenerator::GetSeed` reverse derivation which
    /// has not been ported yet. Setting this flag has no effect on
    /// output until that lands.
    pub use_rvz_packing: bool,
}

impl Default for RvzCompressOptions {
    fn default() -> Self {
        Self {
            compression_level: DEFAULT_COMPRESSION_LEVEL,
            chunk_size: DEFAULT_CHUNK_SIZE,
            use_rvz_packing: false,
        }
    }
}

/// Compress a GameCube or Wii disc image to RVZ.
pub async fn compress_disc(
    input: &Path,
    output: &Path,
    options: RvzCompressOptions,
    progress: &dyn ProgressReporter,
) -> RvzResult<()> {
    validate_chunk_size(options.chunk_size)?;

    let iso_size = tokio::fs::metadata(input).await?.len();
    progress.start(iso_size, "Compressing disc to RVZ...");

    let input_owned: PathBuf = input.to_path_buf();
    let output_owned: PathBuf = output.to_path_buf();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let mut handle = task::spawn_blocking(move || -> RvzResult<u64> {
        compress_blocking(
            &input_owned,
            &output_owned,
            options,
            iso_size,
            bytes_done_bg,
        )
    });

    let compressed_size = loop {
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

    let ratio = (1.0 - compressed_size as f64 / iso_size.max(1) as f64) * 100.0;
    info!(
        "Compressed {} -> {} ({:.1}% reduction)",
        input.display(),
        output.display(),
        ratio
    );

    Ok(())
}

fn validate_chunk_size(chunk_size: u32) -> RvzResult<()> {
    if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&chunk_size) || !chunk_size.is_power_of_two() {
        return Err(RvzError::InvalidChunkSize(
            chunk_size,
            MIN_CHUNK_SIZE,
            MAX_CHUNK_SIZE,
        ));
    }
    Ok(())
}

/// Shared tag describing how one chunk ended up on disk. Emitted by
/// both encoder paths, handled by the shared chunk-write helpers.
pub(super) enum CompressedKind {
    /// The input chunk was all zeros. Emitted as a sentinel (zero
    /// `data_off4` + zero `data_size`) with no bytes on disk.
    AllZero,
    /// zstd shrunk the chunk. The `Vec<u8>` is the compressed bytes.
    Compressed(Vec<u8>),
    /// zstd did not shrink the chunk. The `Vec<u8>` is the raw chunk
    /// body (or the raw fallback with 4-byte-aligned exception list
    /// padding for partition chunks).
    Raw(Vec<u8>),
}

/// Metadata returned by [`partition::parallel_encode_partition_region`]
/// so [`compress_blocking`] can populate `WiaPart::pd[0]` and
/// `WiaPart::pd[1]` with values that match Dolphin's
/// `CreatePartitionDataEntry` formula.
pub(super) struct PartitionLayout {
    pub pd0_n_sectors: u32,
    pub pd0_n_groups: u32,
    pub pd1_n_sectors: u32,
    pub pd1_n_groups: u32,
}

/// Sync pipeline driven inside `spawn_blocking`. Returns the final
/// file size so the progress bar can report the final reduction
/// ratio.
fn compress_blocking(
    input: &Path,
    output: &Path,
    options: RvzCompressOptions,
    iso_size: u64,
    bytes_done: Arc<AtomicU64>,
) -> RvzResult<u64> {
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, std::fs::File::open(input)?);

    // Read the 0x80-byte disc header used by both the format struct
    // and the GC/Wii detection helpers.
    let mut dhead = [0u8; 128];
    reader.read_exact(&mut dhead)?;
    reader.seek(SeekFrom::Start(0))?;

    let disc_type = if is_gamecube(&dhead) {
        1u32
    } else if is_wii(&dhead) {
        2u32
    } else {
        return Err(RvzError::UnrecognizedDisc);
    };

    let plan = if disc_type == 1 {
        RegionPlan::gamecube(iso_size)
    } else {
        RegionPlan::wii(&mut reader, iso_size)?
    };

    // Wii partitions: the user's chunk_size flows through unchanged.
    // One Wii cluster (2 MiB) spans `chunks_per_cluster =
    // 0x200000 / chunk_size` output chunks, each carrying its own
    // `wia_except_list_t` with chunk-local block offsets. Dolphin's
    // default is 128 KiB (16 chunks per cluster).
    let effective_chunk_size = options.chunk_size;

    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, std::fs::File::create(output)?);

    // Placeholder header and disc struct. Rewritten at the end once
    // all offsets and sizes are known.
    writer.write_all(&[0u8; WIA_FILE_HEAD_SIZE])?;
    writer.write_all(&[0u8; WIA_DISC_SIZE])?;

    // Manually tracked writer position. Every region encoder and
    // the metadata emitters update this in sync with their
    // writes so we never need to call `writer.stream_position()`,
    // which on a `BufWriter` flushes the internal buffer to
    // disk and negates the whole point of buffering.
    let mut writer_pos: u64 = (WIA_FILE_HEAD_SIZE + WIA_DISC_SIZE) as u64;

    let mut groups: Vec<RvzGroup> = Vec::new();
    let mut raw_data: Vec<WiaRawData> = Vec::new();
    let mut partitions: Vec<WiaPart> = Vec::new();

    // Spawn the worker pools once per compress invocation: one
    // raw pool (always, since the 0x80-aligned disc header lives
    // in a raw region) and, if the plan has partition regions,
    // one partition pool. `zstd::bulk::Compressor::new` runs
    // exactly once per worker this way, instead of once per
    // region. GameCube runs skip the partition pool entirely.
    let n_threads = parallelism();
    let raw_workers = raw::make_raw_compress_workers(n_threads, options.compression_level)?;
    let raw_pool: Pool<raw::RawWork, raw::CompressedChunk, RvzError> = Pool::spawn(raw_workers);

    let has_partitions = plan
        .regions
        .iter()
        .any(|r| matches!(r, DiscRegion::Partition(_)));
    let mut partition_pool: Option<
        Pool<partition::PartitionWork, Vec<partition::PartitionChunk>, RvzError>,
    > = if has_partitions {
        let workers =
            partition::make_partition_compress_workers(n_threads, options.compression_level)?;
        Some(Pool::spawn(workers))
    } else {
        None
    };

    let encode_result: RvzResult<()> = (|| {
        for region in &plan.regions {
            match region {
                DiscRegion::Raw { offset, size } => {
                    let group_index = groups.len() as u32;
                    raw::parallel_encode_raw_region(
                        &raw_pool,
                        &mut reader,
                        &mut writer,
                        &mut writer_pos,
                        *offset,
                        *size,
                        iso_size,
                        effective_chunk_size,
                        &mut groups,
                        &bytes_done,
                    )?;
                    let n_groups = groups.len() as u32 - group_index;
                    raw_data.push(WiaRawData {
                        raw_data_off: *offset,
                        raw_data_size: *size,
                        group_index,
                        n_groups,
                    });
                }
                DiscRegion::Partition(info) => {
                    let group_index = groups.len() as u32;
                    let layout = partition::parallel_encode_partition_region(
                        partition_pool
                            .as_ref()
                            .expect("partition_pool must exist if plan contains partitions"),
                        &mut reader,
                        &mut writer,
                        &mut writer_pos,
                        info,
                        effective_chunk_size,
                        &mut groups,
                        &bytes_done,
                    )?;
                    let first_sector = (info.data_start() / WII_SECTOR_SIZE_U64) as u32;
                    partitions.push(WiaPart {
                        part_key: info.title_key,
                        pd: [
                            WiaPartData {
                                first_sector,
                                n_sectors: layout.pd0_n_sectors,
                                group_index,
                                n_groups: layout.pd0_n_groups,
                            },
                            WiaPartData {
                                first_sector: first_sector + layout.pd0_n_sectors,
                                n_sectors: layout.pd1_n_sectors,
                                group_index: group_index + layout.pd0_n_groups,
                                n_groups: layout.pd1_n_groups,
                            },
                        ],
                    });
                }
            }
        }
        Ok(())
    })();

    // Always tear down the pools before propagating the error.
    // `Pool::shutdown` takes `self`, so `partition_pool.take()`
    // is the idiomatic way to drain the Option.
    raw_pool.shutdown();
    if let Some(pool) = partition_pool.take() {
        pool.shutdown();
    }
    encode_result?;

    // Now that every region is on disk, emit the three metadata
    // tables in the order Dolphin expects: partitions, raw_data
    // (zstd-compressed), groups (zstd-compressed). Each is 4-byte
    // aligned so the file-head offsets are trivially recoverable.
    //
    // These offsets come from our tracked `writer_pos` rather
    // than `writer.stream_position()` so the BufWriter never
    // flushes in the middle of a contiguous write stream.
    let part_off = if !partitions.is_empty() {
        let pos = writer_pos;
        for part in &partitions {
            let mut bytes = Vec::with_capacity(crate::nintendo::rvz::format::WIA_PART_SIZE);
            part.write_options(&mut Cursor::new(&mut bytes), Endian::Big, ())?;
            writer.write_all(&bytes)?;
            writer_pos += bytes.len() as u64;
        }
        pad_to_alignment(&mut writer, &mut writer_pos, 4)?;
        pos
    } else {
        0
    };
    let part_hash = compute_part_hash(&partitions);

    let raw_data_off = writer_pos;
    let raw_data_compressed = serialize_and_compress(&raw_data, options.compression_level)?;
    writer.write_all(&raw_data_compressed)?;
    writer_pos += raw_data_compressed.len() as u64;
    pad_to_alignment(&mut writer, &mut writer_pos, 4)?;

    let group_off = writer_pos;
    let group_compressed = serialize_and_compress(&groups, options.compression_level)?;
    writer.write_all(&group_compressed)?;
    writer_pos += group_compressed.len() as u64;
    pad_to_alignment(&mut writer, &mut writer_pos, 4)?;

    let wia_file_size = writer_pos;

    let disc = WiaDisc {
        disc_type,
        compression: 5,
        compr_level: options.compression_level,
        chunk_size: effective_chunk_size,
        dhead,
        n_part: partitions.len() as u32,
        part_t_size: crate::nintendo::rvz::format::WIA_PART_SIZE as u32,
        part_off,
        part_hash,
        n_raw_data: raw_data.len() as u32,
        raw_data_off,
        raw_data_size: raw_data_compressed.len() as u32,
        n_groups: groups.len() as u32,
        group_off,
        group_size: group_compressed.len() as u32,
        compr_data_len: 0,
        compr_data: [0u8; 7],
    };
    let disc_hash = compute_disc_hash(&disc);

    let head = WiaFileHead {
        magic: RVZ_MAGIC,
        // Matches RVZ_VERSION / RVZ_VERSION_WRITE_COMPATIBLE from
        // Dolphin's `Source/Core/DiscIO/WIABlob.h`.
        version: 0x01000000,
        version_compatible: 0x00030000,
        disc_size: WIA_DISC_SIZE as u32,
        disc_hash,
        iso_file_size: iso_size,
        wia_file_size,
        file_head_hash: [0u8; 20],
    };
    let file_head_hash = compute_file_head_hash(&head);
    let head_final = WiaFileHead {
        file_head_hash,
        ..head
    };

    writer.flush()?;
    writer.seek(SeekFrom::Start(0))?;
    let mut head_bytes = Vec::with_capacity(WIA_FILE_HEAD_SIZE);
    head_final.write_options(&mut Cursor::new(&mut head_bytes), Endian::Big, ())?;
    writer.write_all(&head_bytes)?;

    let mut disc_bytes = Vec::with_capacity(WIA_DISC_SIZE);
    disc.write_options(&mut Cursor::new(&mut disc_bytes), Endian::Big, ())?;
    writer.write_all(&disc_bytes)?;
    writer.flush()?;

    Ok(wia_file_size)
}

/// Pad the writer forward to the next `alignment`-byte boundary with
/// zero bytes, updating the tracked `writer_pos` in place. Shared
/// by both the raw-chunk and partition-chunk emitters because
/// every group entry stores a `data_off4` that's multiplied by 4
/// on read, so emitting a non-aligned group would corrupt the
/// next group's offset.
///
/// `writer_pos` replaces the previous `writer.stream_position()`
/// call; see [`push_compressed_chunk`] for why.
pub(super) fn pad_to_alignment(
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    alignment: u64,
) -> RvzResult<()> {
    let rem = *writer_pos % alignment;
    if rem != 0 {
        let pad = alignment - rem;
        let zeros = [0u8; 16];
        let mut left = pad as usize;
        while left > 0 {
            let n = left.min(zeros.len());
            writer.write_all(&zeros[..n])?;
            left -= n;
        }
        *writer_pos += pad;
    }
    Ok(())
}

/// One message sent from an encoder dispatcher to its writer
/// thread. `Bytes` ships a completed chunk's payload; `Pad`
/// requests `n` zero bytes for 4-byte group alignment. Shared
/// between the raw and partition region encoders so both can
/// overlap writes with reads + dispatch via the same channel
/// shape.
pub(super) enum WriteMsg {
    Bytes(Vec<u8>),
    Pad(u32),
}

/// Channel-flavored analogue of [`push_compressed_chunk`]. Updates
/// the group table and tracked `writer_pos` on the dispatcher
/// side, but ships the actual bytes to a writer thread over
/// `write_tx` so `write_all` runs in parallel with the next
/// read/dispatch. Handles all three [`CompressedKind`] variants
/// identically to the direct-writer path.
pub(super) fn push_compressed_chunk_via_channel(
    write_tx: &std::sync::mpsc::SyncSender<WriteMsg>,
    writer_pos: &mut u64,
    groups: &mut Vec<RvzGroup>,
    kind: CompressedKind,
    rvz_packed_size: u32,
) -> RvzResult<()> {
    match kind {
        CompressedKind::AllZero => {
            groups.push(RvzGroup {
                data_off4: 0,
                data_size: 0,
                rvz_packed_size: 0,
            });
        }
        CompressedKind::Compressed(bytes) => {
            debug_assert_eq!(*writer_pos % 4, 0);
            let data_off4 = (*writer_pos / 4) as u32;
            let len = bytes.len();
            groups.push(RvzGroup::new_compressed(
                data_off4,
                len as u32,
                rvz_packed_size,
            ));
            write_tx
                .send(WriteMsg::Bytes(bytes))
                .map_err(|_| RvzError::Custom("writer channel closed".into()))?;
            *writer_pos += len as u64;
            pad_to_alignment_via_channel(write_tx, writer_pos, 4)?;
        }
        CompressedKind::Raw(bytes) => {
            debug_assert_eq!(*writer_pos % 4, 0);
            let data_off4 = (*writer_pos / 4) as u32;
            let len = bytes.len() as u32;
            groups.push(RvzGroup {
                data_off4,
                data_size: len,
                rvz_packed_size,
            });
            write_tx
                .send(WriteMsg::Bytes(bytes))
                .map_err(|_| RvzError::Custom("writer channel closed".into()))?;
            *writer_pos += len as u64;
            pad_to_alignment_via_channel(write_tx, writer_pos, 4)?;
        }
    }
    Ok(())
}

/// Channel-flavored analogue of [`pad_to_alignment`]. Queues a
/// `Pad` message on the writer channel if the tracked position is
/// not aligned, and advances `writer_pos` by the pad amount so
/// subsequent chunks see the correct base offset.
pub(super) fn pad_to_alignment_via_channel(
    write_tx: &std::sync::mpsc::SyncSender<WriteMsg>,
    writer_pos: &mut u64,
    alignment: u64,
) -> RvzResult<()> {
    let rem = *writer_pos % alignment;
    if rem != 0 {
        let pad = alignment - rem;
        write_tx
            .send(WriteMsg::Pad(pad as u32))
            .map_err(|_| RvzError::Custom("writer channel closed".into()))?;
        *writer_pos += pad;
    }
    Ok(())
}

/// Body of the dedicated writer thread used by both region
/// encoders. Drains `write_rx` and writes each message to
/// `writer`, exiting cleanly when the dispatcher drops its
/// sender.
pub(super) fn write_msg_drain_loop(
    writer: &mut BufWriter<std::fs::File>,
    write_rx: std::sync::mpsc::Receiver<WriteMsg>,
) -> RvzResult<()> {
    let zeros = [0u8; 16];
    while let Ok(msg) = write_rx.recv() {
        match msg {
            WriteMsg::Bytes(bytes) => writer.write_all(&bytes)?,
            WriteMsg::Pad(n) => {
                let mut left = n as usize;
                while left > 0 {
                    let k = left.min(zeros.len());
                    writer.write_all(&zeros[..k])?;
                    left -= k;
                }
            }
        }
    }
    Ok(())
}

/// Serialize a `BinWrite` slice into a plain byte buffer, then zstd
/// compress it. Used for the raw-data table and the group table at
/// the end of the file; both are small enough that the one-shot
/// allocation is fine.
fn serialize_and_compress<T>(items: &[T], level: i32) -> RvzResult<Vec<u8>>
where
    T: BinWrite<Args<'static> = ()>,
{
    let mut plain = Vec::new();
    let mut cursor = Cursor::new(&mut plain);
    for item in items {
        item.write_options(&mut cursor, Endian::Big, ())?;
    }
    Ok(zstd::bulk::compress(&plain, level)?)
}
