//! Wii partition encoder.
//!
//! Walks the partition's logical byte range `[0, data_size)` in
//! `chunk_size` strides, emitting one group entry per stride.
//! Partitions are split into Dolphin's two `wia_part_data_t`
//! segments:
//!
//! * `pd[0]` = first 2 MiB cluster (management / FST area in
//!   practice).
//! * `pd[1]` = everything else.
//!
//! For the partial last chunk (when `data_size` is not a multiple
//! of `chunk_size`), the chunk carries fewer than `blocks_per_chunk`
//! payloads and its plaintext payload region is shorter. This
//! matches Dolphin's `CreatePartitionDataEntry` formula:
//! `n_groups = AlignUp(AlignDown(size, BLOCK_TOTAL_SIZE), chunk_size)
//! / chunk_size`.
//!
//! Chunks within a cluster share the same decrypted cluster +
//! exception list. The exception list is built against
//! `recompute_hash_regions_into` run on **zero-padded payloads** for
//! any sector past `data_size`, so decoder and encoder agree on
//! the recompute baseline and the exception diff captures the
//! right bytes.
//!
//! The worker holds persistent scratch buffers
//! (`padded_payloads`, `hash_regions`, `exception_header`, `body`,
//! `chunk_exceptions`) so the per-cluster hot loop allocates zero
//! `Vec`s after the pool is warm. See [`PartitionCompressWorker`].

use super::{
    CompressedKind, PartitionLayout, WriteMsg, push_compressed_chunk_via_channel,
    write_msg_drain_loop,
};
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_TOTAL_SIZE, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE,
    WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::partition::{
    ChunkSectorPos, HASH_REGION_BYTES, HashException, PartitionInfo, build_hash_exceptions,
    read_and_decrypt_cluster, recompute_hash_regions_into, serialize_exception_header_into,
    split_chunk_exceptions_by_range,
};
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::RvzGroup;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A compressed or raw partition chunk emitted by one worker. The
/// writer consumes a `Vec<PartitionChunk>` per cluster and flushes
/// them in cluster order via [`drive`]'s reorder buffer.
///
/// `enc_bytes` is the number of encrypted disc bytes this chunk
/// covers, used by the dispatcher for per-chunk progress
/// reporting so the progress counter updates at 16x finer
/// granularity than per-cluster (one update per 128 KiB chunk
/// instead of per 2 MiB cluster).
pub(super) struct PartitionChunk {
    pub kind: CompressedKind,
    pub rvz_packed_size: u32,
    pub enc_bytes: u64,
}

/// Partition-cluster compression work item: 2 MiB of ciphertext
/// read straight from disc plus all the per-partition metadata
/// a worker needs to emit chunks.
///
/// `title_key`, `partition_data_size`, and `chunk_size_u64` ride
/// along with every cluster instead of living on the worker so
/// one shared `PartitionCompressWorker` pool can service
/// multiple partitions in one compress invocation. Discs with
/// more than one partition (distinct title keys and data sizes)
/// need this per-cluster routing.
pub(super) struct PartitionWork {
    pub raw_cluster: Vec<u8>,
    pub cluster_idx: u64,
    pub title_key: [u8; 16],
    pub partition_data_size: u64,
    pub chunk_size_u64: u64,
}

/// Per-thread partition encoder state. Holds the persistent
/// `zstd::bulk::Compressor` plus scratch buffers reused across
/// every cluster **across every partition this pool services**:
///
/// * `padded_payloads`: fixed 64-entry scratch used only by the
///   partial-cluster path (full clusters skip the copy entirely).
/// * `hash_regions`: 64-entry scratch that
///   `recompute_hash_regions_into` writes the hash hierarchy into.
/// * `exception_header` / `body` / `assembly`: `Vec<u8>` scratch
///   for per-chunk header bytes, payload bytes, and the
///   `[header][payload]` buffer handed to zstd. `assembly` can't
///   share with `body` because `pack_encode`'s `Cow::Borrowed`
///   path keeps `body` borrowed while we assemble.
/// * `chunk_exceptions`: reused `Vec<HashException>` the encoder
///   collects the per-chunk exception iterator into.
///
/// Zero `Vec::new` / `Vec::with_capacity` calls in the hot loop
/// once the pool is warm. Per-partition state (title key, data
/// size, chunk size) rides in each [`PartitionWork`] so one pool
/// can process every partition on a disc without tear-down.
pub(super) struct PartitionCompressWorker {
    compressor: zstd::bulk::Compressor<'static>,
    padded_payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>,
    hash_regions: Vec<[u8; HASH_REGION_BYTES]>,
    exception_header: Vec<u8>,
    body: Vec<u8>,
    assembly: Vec<u8>,
    chunk_exceptions: Vec<HashException>,
}

impl Worker<PartitionWork, Vec<PartitionChunk>, RvzError> for PartitionCompressWorker {
    fn process(&mut self, work: PartitionWork) -> RvzResult<Vec<PartitionChunk>> {
        encode_one_partition_cluster_with(self, work)
    }
}

pub(super) fn make_partition_compress_workers(
    n_threads: usize,
    compression_level: i32,
) -> RvzResult<Vec<PartitionCompressWorker>> {
    (0..n_threads)
        .map(|_| -> RvzResult<PartitionCompressWorker> {
            Ok(PartitionCompressWorker {
                compressor: zstd::bulk::Compressor::new(compression_level)
                    .map_err(|e| RvzError::Custom(format!("zstd init: {e}")))?,
                padded_payloads: vec![[0u8; WII_SECTOR_PAYLOAD_SIZE]; WII_BLOCKS_PER_GROUP],
                hash_regions: vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP],
                exception_header: Vec::new(),
                body: Vec::new(),
                assembly: Vec::new(),
                chunk_exceptions: Vec::new(),
            })
        })
        .collect()
}

/// Process the partition cluster by cluster via the generic worker
/// [`Pool`]. Each worker owns a long-lived `zstd::bulk::Compressor`
/// that stays alive for the full partition, avoiding a
/// `ZSTD_createCCtx` allocation on every cluster.
///
/// [`drive`] handles submit/recv/reorder so this function only
/// supplies the per-cluster read (`produce`) and per-cluster write
/// (`consume`) halves. Results land in monotonic cluster order so
/// the group table stays consistent with the file offsets.
#[allow(clippy::too_many_arguments)]
pub(super) fn parallel_encode_partition_region(
    pool: &Pool<PartitionWork, Vec<PartitionChunk>, RvzError>,
    reader: &mut BufReader<std::fs::File>,
    writer: &mut BufWriter<std::fs::File>,
    writer_pos: &mut u64,
    info: &PartitionInfo,
    chunk_size: u32,
    groups: &mut Vec<RvzGroup>,
    bytes_done: &Arc<AtomicU64>,
) -> RvzResult<PartitionLayout> {
    let title_key = info.title_key;
    let chunk_size_u64 = chunk_size as u64;
    let data_size = info.data_size;

    // Split boundary: one 2 MiB cluster for pd[0] (unless the
    // partition is smaller, which shouldn't happen in practice).
    let pd0_bytes = WII_GROUP_TOTAL_SIZE.min(data_size);
    let pd1_bytes = data_size - pd0_bytes;

    // Dolphin's `CreatePartitionDataEntry` rule:
    //   n_sectors  = size / BLOCK_TOTAL_SIZE   (truncated)
    //   n_groups   = AlignUp(AlignDown(size, BLOCK_TOTAL_SIZE),
    //                        chunk_size) / chunk_size
    let pd0_n_sectors = (pd0_bytes / WII_SECTOR_SIZE_U64) as u32;
    let pd0_rounded = pd0_bytes & !(WII_SECTOR_SIZE_U64 - 1);
    let pd0_n_groups = if pd0_rounded == 0 {
        0
    } else {
        pd0_rounded.div_ceil(chunk_size_u64) as u32
    };
    let pd1_n_sectors = (pd1_bytes / WII_SECTOR_SIZE_U64) as u32;
    let pd1_rounded = pd1_bytes & !(WII_SECTOR_SIZE_U64 - 1);
    let pd1_n_groups = if pd1_rounded == 0 {
        0
    } else {
        pd1_rounded.div_ceil(chunk_size_u64) as u32
    };

    let cluster_size = WII_GROUP_TOTAL_SIZE as usize;
    let cluster_count = info.cluster_count();

    reader.seek(SeekFrom::Start(info.data_start()))?;

    let max_in_flight = parallelism() * 2;

    // Overlap writes with reads + dispatch, mirroring the raw
    // region encoder's writer-thread pattern: the consume
    // closure runs on the dispatcher thread and forwards each
    // chunk's compressed bytes over a bounded channel to a
    // dedicated writer thread that owns `&mut writer` via
    // `std::thread::scope`. Without this split, Wii compress
    // wastes a nontrivial fraction of wall time on
    // `writer.write_all` memcpys that block the next cluster
    // read.
    let mut local_writer_pos = *writer_pos;
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<WriteMsg>(max_in_flight * 2);

    let scope_result: RvzResult<()> = std::thread::scope(|s| {
        let writer_slot: &mut BufWriter<std::fs::File> = writer;
        let writer_handle =
            s.spawn(move || -> RvzResult<()> { write_msg_drain_loop(writer_slot, write_rx) });

        let drive_result = drive(
            pool,
            cluster_count,
            max_in_flight,
            // produce: read the next 2 MiB of ciphertext from
            // the ISO and ship it to a worker along with the
            // per-partition metadata the worker needs. Sharing
            // one pool across partitions means these vary per
            // cluster.
            //
            // The cluster buffer is allocated uninitialised
            // rather than via `vec![0u8; 2 MiB]` because
            // `alloc_zeroed` pays for a 2 MiB `memset` per
            // cluster even though `read_exact` immediately
            // overwrites every byte. On a full Wii disc that
            // zero-fill adds up to gigabytes of wasted work.
            |seq| -> RvzResult<PartitionWork> {
                let mut buf = Vec::with_capacity(cluster_size);
                // SAFETY: `u8` has no drop glue and no
                // validity invariants, so extending `len`
                // over uninit bytes is sound as long as no
                // reader observes them first. `read_exact`
                // only writes its destination slice;
                // `BufReader<File>` is a well-behaved `Read`
                // that never reads uninit bytes. On error
                // the `Vec` is dropped without any byte ever
                // being read. Clippy's `uninit_vec` lint is
                // conservative for element types with real
                // drop glue and is explicitly allowed here.
                #[allow(clippy::uninit_vec)]
                unsafe {
                    buf.set_len(cluster_size);
                }
                reader.read_exact(&mut buf)?;
                Ok(PartitionWork {
                    raw_cluster: buf,
                    cluster_idx: seq,
                    title_key,
                    partition_data_size: data_size,
                    chunk_size_u64,
                })
            },
            // consume: forward every chunk the worker emitted
            // for this cluster to the writer thread, updating
            // the group table + tracked writer position on the
            // dispatcher side. Progress is reported per chunk
            // (not per cluster) so the counter updates at the
            // same 128 KiB granularity as the raw encoder.
            |_seq, cluster_chunks| -> RvzResult<()> {
                for pc in cluster_chunks {
                    let enc_bytes = pc.enc_bytes;
                    push_compressed_chunk_via_channel(
                        &write_tx,
                        &mut local_writer_pos,
                        groups,
                        pc.kind,
                        pc.rvz_packed_size,
                    )?;
                    bytes_done.fetch_add(enc_bytes, Ordering::Relaxed);
                }
                Ok(())
            },
        );

        drop(write_tx);
        let writer_result = writer_handle
            .join()
            .unwrap_or_else(|_| Err(RvzError::Custom("writer thread panicked".into())));
        drive_result?;
        writer_result
    });

    *writer_pos = local_writer_pos;
    scope_result?;

    Ok(PartitionLayout {
        pd0_n_sectors,
        pd0_n_groups,
        pd1_n_sectors,
        pd1_n_groups,
    })
}

/// Encode every chunk that belongs to one Wii cluster. Called
/// from the pool worker. Returns chunks in submission order
/// (cluster's first chunk first); each chunk may be full or
/// partial. The caller is responsible for reading the 2 MiB of
/// ciphertext and passing it in as `raw_cluster`.
///
/// This function handles the **cluster-level** work (decrypt,
/// hash hierarchy, exception list) and delegates the **per-chunk**
/// work (exception split, pack_encode, zstd compress,
/// compressed/raw decision) to [`encode_one_chunk_with`].
fn encode_one_partition_cluster_with(
    worker: &mut PartitionCompressWorker,
    work: PartitionWork,
) -> RvzResult<Vec<PartitionChunk>> {
    let PartitionWork {
        raw_cluster,
        cluster_idx,
        title_key,
        partition_data_size,
        chunk_size_u64,
    } = work;

    // Decrypt the full cluster.
    let mut cursor = std::io::Cursor::new(raw_cluster);
    let cluster = read_and_decrypt_cluster(&mut cursor, &title_key)?;

    // Encrypted-coordinate range of this cluster inside the
    // partition's declared data. Dolphin's `data_size` and
    // `chunk_size` are in encrypted bytes (one sector = 0x8000,
    // one cluster = 0x200000). `enc_cluster_end` is clamped to
    // `partition_data_size` for the partial last cluster.
    let enc_cluster_start = cluster_idx * WII_GROUP_TOTAL_SIZE;
    let enc_cluster_end = (enc_cluster_start + WII_GROUP_TOTAL_SIZE).min(partition_data_size);
    debug_assert!(enc_cluster_end > enc_cluster_start);

    // How many blocks the partition's declared data covers in
    // this cluster. Sectors past this are padding and are
    // zero-filled for the hash recompute baseline. The full-
    // cluster fast path skips the copy and hands `cluster.payloads`
    // straight to the recompute routine.
    let valid_bytes_in_cluster = enc_cluster_end - enc_cluster_start;
    let valid_blocks = valid_bytes_in_cluster.div_ceil(WII_SECTOR_SIZE as u64) as usize;
    debug_assert!(valid_blocks <= WII_BLOCKS_PER_GROUP);
    let full_cluster = valid_blocks == WII_BLOCKS_PER_GROUP;

    // `padded_slice` points at whichever buffer we hand to
    // `recompute_hash_regions_into`. Full clusters skip the
    // 2 MiB memcpy; partial clusters copy into the worker's
    // persistent scratch and zero-fill the tail.
    let padded_slice: &[[u8; WII_SECTOR_PAYLOAD_SIZE]] = if full_cluster {
        &cluster.payloads
    } else {
        worker.padded_payloads[..WII_BLOCKS_PER_GROUP]
            .copy_from_slice(&cluster.payloads[..WII_BLOCKS_PER_GROUP]);
        for b in valid_blocks..WII_BLOCKS_PER_GROUP {
            worker.padded_payloads[b] = [0u8; WII_SECTOR_PAYLOAD_SIZE];
        }
        &worker.padded_payloads[..]
    };

    recompute_hash_regions_into(padded_slice, &mut worker.hash_regions[..]);
    let cluster_exceptions = build_hash_exceptions(&cluster, &worker.hash_regions[..]);

    // Self-check: recomputing the hash hierarchy from the padded
    // baseline and then applying our exception list must
    // reproduce the cluster's actual on-disc hash regions
    // exactly. Caught the u16 overflow regression during W4 and
    // keeps the partial-cluster padding math honest.
    #[cfg(debug_assertions)]
    {
        use crate::nintendo::rvl::partition::apply_hash_exceptions;
        let mut rebuilt = worker.hash_regions.clone();
        apply_hash_exceptions(&mut rebuilt, &cluster_exceptions);
        debug_assert_eq!(
            rebuilt, cluster.on_disc_hash_regions,
            "exception list does not reproduce on-disc hash regions for cluster {cluster_idx}"
        );
    }

    // Walk the cluster chunk by chunk. Each chunk covers exactly
    // `chunk_size_u64` encrypted bytes, except the final chunk of
    // the partition's last cluster, which may be partial.
    let mut out_chunks: Vec<PartitionChunk> = Vec::new();
    let mut chunk_enc_pos = enc_cluster_start;
    while chunk_enc_pos < enc_cluster_end {
        let this_chunk_enc_bytes = chunk_size_u64.min(enc_cluster_end - chunk_enc_pos);
        let pos = ChunkSectorPos::new(chunk_enc_pos, this_chunk_enc_bytes);
        debug_assert_eq!(pos.cluster_idx, cluster_idx);

        out_chunks.push(encode_one_chunk_with(
            worker,
            &cluster.payloads,
            &cluster_exceptions,
            pos,
            this_chunk_enc_bytes,
        )?);

        chunk_enc_pos += this_chunk_enc_bytes;
    }

    Ok(out_chunks)
}

/// Encode one partition chunk. Isolated from
/// [`encode_one_partition_cluster_with`] so the cluster-level
/// setup stays separate from the per-chunk pipeline:
///
/// 1. Collect the chunk's slice of `cluster_exceptions` into
///    the worker's persistent `chunk_exceptions` scratch.
/// 2. Copy the chunk's plaintext payload bytes into
///    `worker.body`.
/// 3. Run `pack_encode` on `worker.body`. If LFG junk is found,
///    the packed Vec replaces the payload region going into
///    zstd; otherwise the raw `body` is used directly via
///    [`Cow::Borrowed`].
/// 4. Serialize the exception header into
///    `worker.exception_header`, then assemble
///    `[header | payload_region]` into `worker.assembly`.
/// 5. Run zstd on `worker.assembly`. Mirror Dolphin's "will it
///    compress" check (compressed length < `AlignUp(exception
///    list size, 4) + payload length`) to decide compressed vs
///    raw storage. Raw storage zero-pads the exception list up
///    to a 4-byte boundary as Dolphin's `pad_exception_lists`
///    does for the uncompressed fallback path.
fn encode_one_chunk_with(
    worker: &mut PartitionCompressWorker,
    cluster_payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
    cluster_exceptions: &[HashException],
    pos: ChunkSectorPos,
    enc_bytes: u64,
) -> RvzResult<PartitionChunk> {
    // Chunk-local exception list.
    // `split_chunk_exceptions_by_range` returns an iterator so
    // we can collect into the worker's persistent scratch with
    // zero heap traffic.
    worker.chunk_exceptions.clear();
    worker
        .chunk_exceptions
        .extend(split_chunk_exceptions_by_range(
            cluster_exceptions,
            pos.first_sector_in_chunk,
            pos.chunk_n_sectors,
        ));

    // Build the chunk's plaintext payload into `worker.body`.
    worker.body.clear();
    worker.body.reserve(pos.payload_len());
    for b in 0..pos.chunk_n_sectors {
        worker
            .body
            .extend_from_slice(&cluster_payloads[pos.first_sector_in_chunk + b]);
    }

    // `pack_encode` consumes `&worker.body` and may return either
    // an owned packed Vec (LFG junk found) or `None` (no junk,
    // the raw body itself is the payload region). We bridge the
    // two with `Cow<[u8]>` so downstream code can treat both
    // uniformly without cloning the un-packed path.
    let (payload_region, rvz_packed_size): (std::borrow::Cow<'_, [u8]>, u32) =
        match crate::nintendo::rvz::packing::pack_encode(&worker.body, pos.chunk_data_offset_pay())
        {
            Some(packed) => {
                let pack_len = packed.len() as u32;
                (std::borrow::Cow::Owned(packed), pack_len)
            }
            None => (std::borrow::Cow::Borrowed(&worker.body), 0u32),
        };

    // Serialize the exception header into its reused scratch,
    // then assemble `[header | payload]` into the worker's
    // persistent `assembly` buffer (zero per-chunk allocs once
    // warm).
    worker.exception_header.clear();
    serialize_exception_header_into(&worker.chunk_exceptions, &mut worker.exception_header)?;
    worker.assembly.clear();
    worker
        .assembly
        .reserve(worker.exception_header.len() + payload_region.len());
    worker.assembly.extend_from_slice(&worker.exception_header);
    worker.assembly.extend_from_slice(&payload_region);

    let compressed = worker
        .compressor
        .compress(&worker.assembly)
        .map_err(|e| RvzError::Custom(format!("zstd compress: {e}")))?;

    // Dolphin's RVZ "will it compress" check uses
    // `uncompressed_size = main_data.size() +
    // AlignUp(exception_lists.size(), 4)`. Match that so our
    // compressed/raw decisions agree with Dolphin's.
    let aligned_exc_len = (worker.exception_header.len() + 3) & !3;
    let uncompressed_size = aligned_exc_len + payload_region.len();
    let kind = if compressed.len() < uncompressed_size {
        CompressedKind::Compressed(compressed)
    } else {
        // Dolphin's `pad_exception_lists` zero-pads the
        // exception list up to a 4-byte boundary when a
        // partition chunk falls back to raw storage.
        let mut raw_body = Vec::with_capacity(aligned_exc_len + payload_region.len());
        raw_body.extend_from_slice(&worker.exception_header);
        raw_body.resize(aligned_exc_len, 0);
        raw_body.extend_from_slice(&payload_region);
        CompressedKind::Raw(raw_body)
    };

    Ok(PartitionChunk {
        kind,
        rvz_packed_size,
        enc_bytes,
    })
}
