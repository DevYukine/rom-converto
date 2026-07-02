//! Wii partition decompressor.
//!
//! Walks each partition's pd\[0\]+pd\[1\] group range one cluster at a
//! time, bucketing chunks by cluster index on the dispatcher
//! thread, and dispatching one cluster per worker through the
//! shared generic [`Pool`]. Workers own persistent
//! `zstd::bulk::Decompressor`s + scratch buffers (payloads,
//! hash-regions, cluster output) so the per-cluster hot loop
//! allocates nothing beyond the final `Box<[u8]>` handoff.
//!
//! See the parent module ([`super`]) for the overall pipeline
//! shape; see the encoder counterpart
//! ([`super::super::compress::partition`]) for the symmetric write
//! path. The per-cluster math mirrors Dolphin's
//! `Source/Core/DiscIO/WIABlob.cpp` partition-data branch:
//!
//! * Each cluster covers `WII_GROUP_TOTAL_SIZE` encrypted bytes.
//! * For a partition whose `data_size` is not a multiple of
//!   `WII_GROUP_TOTAL_SIZE`, the last cluster is partial:
//!   `valid_blocks_in_cluster` sectors of real data followed by
//!   padding sectors left to the pre-filled zero output.
//! * Sectors past `valid_blocks_in_cluster` are zero-filled before
//!   `recompute_hash_regions_into` so the decoder and encoder
//!   agree on the padded recompute baseline; deferred chunk
//!   exceptions then patch any hash-hierarchy bytes that depend on
//!   the real (non-padded) on-disc content.
//!
//! When scrubbing for WBFS, clusters whose blocks are all unused are
//! filtered out before dispatch (see `UsageFilter`) so junk is never
//! read or decompressed.

use super::sink::{DiscSink, UsageFilter};
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_TOTAL_SIZE, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE,
    WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::partition::{
    ChunkSectorPos, HASH_REGION_BYTES, HashException, apply_hash_exceptions,
    parse_exception_header, recompute_hash_regions_into, reencrypt_cluster_into,
};
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::{RvzGroup, WiaPart};
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-chunk spec inside a partition-cluster work item. The
/// dispatcher precomputes the sector math (first_sector,
/// chunk_n_sectors, plaintext data_offset for pack_decode) so the
/// worker never has to reason about partition-level state.
#[derive(Clone)]
pub(crate) struct PartitionChunkSpec {
    data_off: u64,
    data_size: u32,
    is_compressed: bool,
    rvz_packed_size: u32,
    first_sector_in_chunk: usize,
    chunk_n_sectors: usize,
    chunk_data_offset_pay: u64,
    expected_payload_len: usize,
}

/// One partition cluster of work: every chunk that falls inside
/// cluster `cluster_idx`, plus the crypto + layout parameters the
/// worker needs to emit the re-encrypted cluster bytes.
pub(crate) struct PartitionDecompressWork {
    pub(crate) cluster_idx: u64,
    pub(crate) data_start: u64,
    pub(crate) part_key: [u8; 16],
    /// How many sectors this cluster actually stores on the
    /// original disc, that is, how many sectors get written to the
    /// output file. For all but the partial last cluster this
    /// equals `WII_BLOCKS_PER_GROUP`. For the partial last cluster
    /// it's the remainder of `data_size` measured in sectors.
    pub(crate) valid_blocks_in_cluster: usize,
    pub(crate) chunks: Vec<PartitionChunkSpec>,
}

/// Owned cluster buffer ready for sequential write-out on the
/// dispatcher thread. `bytes_to_write` is the prefix of `buf` the
/// consumer actually writes; the tail (for partial last clusters)
/// is left to whatever the pre-filled zero'd output had.
pub(crate) struct PartitionDecompressOut {
    pub(crate) cluster_offset: u64,
    pub(crate) bytes_to_write: usize,
    pub(crate) buf: Box<[u8]>,
}

/// Per-thread partition decoder state. Owns a persistent
/// `zstd::bulk::Decompressor`, a shared `Arc<File>` for positional
/// reads, and heap scratch for payloads + hash regions + the final
/// cluster output buffer. No `Vec::new` in the per-cluster hot
/// loop.
pub(crate) struct PartitionDecompressWorker {
    decompressor: zstd::bulk::Decompressor<'static>,
    file: Arc<std::fs::File>,
    scratch_in: Vec<u8>,
    scratch_decomp: Vec<u8>,
    // `Vec<[u8; 0x7C00]>` rather than `Box<[[u8; 0x7C00]; 64]>`
    // because the stack-initialize-then-box-move path blows the
    // default worker stack on the 2 MiB array copy. The `Vec` is
    // preallocated to exactly 64 entries in
    // `make_partition_decompress_workers` and never grows, so
    // it's functionally equivalent to a fixed-size array for the
    // hot path. Same argument applies to `hash_regions` and
    // `cluster_out`.
    payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>,
    hash_regions: Vec<[u8; HASH_REGION_BYTES]>,
    cluster_out: Vec<u8>,
}

impl Worker<PartitionDecompressWork, PartitionDecompressOut, RvzError>
    for PartitionDecompressWorker
{
    fn process(&mut self, work: PartitionDecompressWork) -> RvzResult<PartitionDecompressOut> {
        // Only zero the payload scratch on partial last clusters.
        // The common full-cluster case overwrites every sector
        // from chunk data, so wiping would just be waste. Partial
        // clusters only zero the tail past
        // `valid_blocks_in_cluster` so the hash recompute sees
        // the right padded baseline.
        let full_cluster = work.valid_blocks_in_cluster == WII_BLOCKS_PER_GROUP;
        if !full_cluster {
            for p in self.payloads[work.valid_blocks_in_cluster..].iter_mut() {
                *p = [0u8; WII_SECTOR_PAYLOAD_SIZE];
            }
        }

        // Deferred exceptions: (slice_start, slice_end, list).
        // Applied to the recomputed cluster hash regions after
        // all chunks are in, same pattern as the sequential
        // decoder. Allocated once per cluster; the inner
        // `Vec<HashException>` is small (typically 0-8 entries)
        // so collect-into-fresh-Vec is fine.
        let mut deferred: Vec<(usize, usize, Vec<HashException>)> =
            Vec::with_capacity(work.chunks.len());

        for spec in &work.chunks {
            self.scratch_in.resize(spec.data_size as usize, 0);
            file_read_exact_at(&self.file, &mut self.scratch_in, spec.data_off)?;

            let decompressed_len: usize = if spec.is_compressed {
                let target = WII_GROUP_TOTAL_SIZE as usize + 1024 * 1024;
                if self.scratch_decomp.len() < target {
                    self.scratch_decomp.resize(target, 0);
                }
                self.decompressor
                    .decompress_to_buffer(&self.scratch_in, &mut self.scratch_decomp)?
            } else {
                if self.scratch_decomp.len() < self.scratch_in.len() {
                    self.scratch_decomp.resize(self.scratch_in.len(), 0);
                }
                self.scratch_decomp[..self.scratch_in.len()].copy_from_slice(&self.scratch_in);
                self.scratch_in.len()
            };

            // Raw chunks have a 4-byte alignment pad after the exception entries;
            // see `pad_exception_lists` in Dolphin's `WIABlob.cpp`.
            let decompressed = &self.scratch_decomp[..decompressed_len];
            let (chunk_exceptions_ref, payload_region) =
                parse_exception_header(decompressed, !spec.is_compressed)?;
            let chunk_exceptions: Vec<HashException> = chunk_exceptions_ref.iter().collect();

            let unpacked: Vec<u8> = if spec.rvz_packed_size != 0 {
                let records_len = (spec.rvz_packed_size as usize).min(payload_region.len());
                crate::nintendo::rvz::packing::pack_decode(
                    &payload_region[..records_len],
                    spec.chunk_data_offset_pay,
                )?
            } else {
                let take = spec.expected_payload_len.min(payload_region.len());
                payload_region[..take].to_vec()
            };

            if unpacked.len() < spec.expected_payload_len {
                return Err(RvzError::DecompressedSizeMismatch {
                    expected: spec.expected_payload_len as u64,
                    actual: unpacked.len() as u64,
                });
            }

            for b in 0..spec.chunk_n_sectors {
                let block_idx = spec.first_sector_in_chunk + b;
                self.payloads[block_idx].copy_from_slice(
                    &unpacked[b * WII_SECTOR_PAYLOAD_SIZE..(b + 1) * WII_SECTOR_PAYLOAD_SIZE],
                );
            }

            deferred.push((
                spec.first_sector_in_chunk,
                spec.first_sector_in_chunk + spec.chunk_n_sectors,
                chunk_exceptions,
            ));
        }

        // Payloads past `valid_blocks_in_cluster` are zero, matching the
        // encoder's padded recompute baseline.
        recompute_hash_regions_into(&self.payloads[..], &mut self.hash_regions[..]);

        for (slice_start, slice_end, exceptions) in deferred.drain(..) {
            apply_hash_exceptions(&mut self.hash_regions[slice_start..slice_end], &exceptions);
        }

        reencrypt_cluster_into(
            &self.hash_regions[..],
            &self.payloads[..],
            &work.part_key,
            &mut self.cluster_out,
        )?;

        let bytes_to_write = work.valid_blocks_in_cluster * WII_SECTOR_SIZE;
        let cluster_offset = work.data_start + work.cluster_idx * WII_GROUP_TOTAL_SIZE;

        let buf: Box<[u8]> = self.cluster_out[..bytes_to_write]
            .to_vec()
            .into_boxed_slice();

        Ok(PartitionDecompressOut {
            cluster_offset,
            bytes_to_write,
            buf,
        })
    }
}

fn make_partition_decompress_workers(
    n_threads: usize,
    file: &Arc<std::fs::File>,
) -> RvzResult<Vec<PartitionDecompressWorker>> {
    (0..n_threads)
        .map(|_| make_one_partition_worker(file))
        .collect()
}

pub(crate) fn make_one_partition_worker(
    file: &Arc<std::fs::File>,
) -> RvzResult<PartitionDecompressWorker> {
    Ok(PartitionDecompressWorker {
        decompressor: zstd::bulk::Decompressor::new()
            .map_err(|e| RvzError::Custom(format!("zstd dctx init: {e}")))?,
        file: Arc::clone(file),
        scratch_in: Vec::new(),
        scratch_decomp: Vec::new(),
        payloads: vec![[0u8; WII_SECTOR_PAYLOAD_SIZE]; WII_BLOCKS_PER_GROUP],
        hash_regions: vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP],
        cluster_out: vec![0u8; WII_GROUP_TOTAL_SIZE as usize],
    })
}

/// Walk a partition's pd[0]+pd[1] group entries, bucket chunks by
/// cluster index, and build one [`PartitionDecompressWork`] per
/// cluster. Mirrors the sequential decoder's `enc_pos` walk
/// exactly so the output is byte-identical.
/// Push one finished cluster's work item, unless the WBFS usage filter
/// says every block the cluster occupies is scrubbed (then it is
/// dropped and never reconstructed).
#[allow(clippy::too_many_arguments)]
fn push_cluster(
    work_items: &mut Vec<PartitionDecompressWork>,
    chunks: Vec<PartitionChunkSpec>,
    cluster_idx: u64,
    data_start: u64,
    part_key: [u8; 16],
    total_data_size: u64,
    filter: Option<&UsageFilter>,
) {
    let valid_blocks = valid_blocks_for_cluster(cluster_idx, total_data_size);
    if let Some(filter) = filter {
        let cluster_offset = data_start + cluster_idx * WII_GROUP_TOTAL_SIZE;
        let cluster_bytes = valid_blocks as u64 * WII_SECTOR_SIZE_U64;
        if !filter.keeps(cluster_offset, cluster_bytes) {
            return;
        }
    }
    work_items.push(PartitionDecompressWork {
        cluster_idx,
        data_start,
        part_key,
        valid_blocks_in_cluster: valid_blocks,
        chunks,
    });
}

pub(crate) fn build_partition_work_items(
    part: &WiaPart,
    groups: &[RvzGroup],
    chunk_size_u64: u64,
    filter: Option<&UsageFilter>,
) -> Vec<PartitionDecompressWork> {
    let pd0 = part.pd[0];
    let pd1 = part.pd[1];
    let total_n_groups = pd0.n_groups + pd1.n_groups;
    if total_n_groups == 0 {
        return Vec::new();
    }

    let data_start = pd0.first_sector as u64 * WII_SECTOR_SIZE_U64;
    let total_data_size = (pd0.n_sectors as u64 + pd1.n_sectors as u64) * WII_SECTOR_SIZE_U64;
    let group_index_start = pd0.group_index;
    let group_index_end = group_index_start + total_n_groups;

    let mut work_items: Vec<PartitionDecompressWork> = Vec::new();
    let mut current_cluster_idx: Option<u64> = None;
    let mut current_chunks: Vec<PartitionChunkSpec> = Vec::new();
    let mut enc_pos: u64 = 0;

    for group_cursor in group_index_start..group_index_end {
        let group = &groups[group_cursor as usize];

        let remaining_in_partition = total_data_size - enc_pos;
        let this_chunk_enc_bytes = chunk_size_u64.min(remaining_in_partition);

        let pos = ChunkSectorPos::new(enc_pos, this_chunk_enc_bytes);

        // Flush the previous bucket when the cluster changes.
        if let Some(prev_idx) = current_cluster_idx
            && pos.cluster_idx != prev_idx
        {
            push_cluster(
                &mut work_items,
                std::mem::take(&mut current_chunks),
                prev_idx,
                data_start,
                part.part_key,
                total_data_size,
                filter,
            );
        }
        current_cluster_idx = Some(pos.cluster_idx);

        current_chunks.push(PartitionChunkSpec {
            data_off: (group.data_off4 as u64) << 2,
            data_size: group.compressed_size(),
            is_compressed: group.is_compressed(),
            rvz_packed_size: group.rvz_packed_size,
            first_sector_in_chunk: pos.first_sector_in_chunk,
            chunk_n_sectors: pos.chunk_n_sectors,
            chunk_data_offset_pay: pos.chunk_data_offset_pay(),
            expected_payload_len: pos.payload_len(),
        });

        enc_pos += this_chunk_enc_bytes;
    }

    if let Some(idx) = current_cluster_idx {
        push_cluster(
            &mut work_items,
            std::mem::take(&mut current_chunks),
            idx,
            data_start,
            part.part_key,
            total_data_size,
            filter,
        );
    }

    work_items
}

/// Sectors the partition's declared `data_size` occupies in the
/// given cluster. For all but the partial last cluster this is
/// `WII_BLOCKS_PER_GROUP` (64). For the partial last cluster it's
/// the remainder of `data_size` measured in whole sectors.
fn valid_blocks_for_cluster(cluster_idx: u64, total_data_size: u64) -> usize {
    let enc_cluster_start = cluster_idx * WII_GROUP_TOTAL_SIZE;
    let enc_cluster_end = (enc_cluster_start + WII_GROUP_TOTAL_SIZE).min(total_data_size);
    let valid_bytes = enc_cluster_end - enc_cluster_start;
    (valid_bytes / WII_SECTOR_SIZE_U64) as usize
}

/// Parallel Wii partition decoder. Builds one
/// [`PartitionDecompressWork`] per cluster on the dispatcher
/// thread, pumps them through a worker [`Pool`], and writes
/// cluster buffers in submission order. Matches the sequential
/// decoder's write behavior exactly: only the first
/// `bytes_to_write` bytes of each cluster are written; sectors
/// past `valid_blocks_in_cluster` are left to the pre-filled
/// zero'd output.
pub(super) fn decompress_partition(
    part: &WiaPart,
    groups: &[RvzGroup],
    chunk_size_u64: u64,
    file: &Arc<std::fs::File>,
    usage: Option<&UsageFilter>,
    sink: &mut dyn DiscSink,
    bytes_done: &Arc<AtomicU64>,
) -> RvzResult<()> {
    let work_items = build_partition_work_items(part, groups, chunk_size_u64, usage);
    if work_items.is_empty() {
        return Ok(());
    }

    let n_threads = parallelism();
    let workers = make_partition_decompress_workers(n_threads, file)?;
    let pool: Pool<PartitionDecompressWork, PartitionDecompressOut, RvzError> =
        Pool::spawn(workers);
    let max_in_flight = n_threads * 2;

    let total = work_items.len() as u64;
    let mut items_iter = work_items.into_iter();

    let result = drive(
        &pool,
        total,
        max_in_flight,
        |_seq| -> RvzResult<PartitionDecompressWork> {
            items_iter
                .next()
                .ok_or_else(|| RvzError::Custom("partition work iterator exhausted".into()))
        },
        |_seq, out| -> RvzResult<()> {
            let written = out.bytes_to_write as u64;
            if out.bytes_to_write > 0 {
                sink.write_at(out.cluster_offset, &out.buf[..out.bytes_to_write])?;
            }
            bytes_done.fetch_add(written, Ordering::Relaxed);
            Ok(())
        },
    );

    pool.shutdown();
    result
}
