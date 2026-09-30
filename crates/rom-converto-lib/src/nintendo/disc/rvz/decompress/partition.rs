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
//! path. The per-cluster math follows the format's partition-data
//! rules:
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
use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use crate::nintendo::disc::rvz::format::{RvzGroup, WiaPart};
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_TOTAL_SIZE, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE,
    WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::partition::{
    ChunkSectorPos, HASH_REGION_BYTES, HashException, apply_hash_exceptions,
    parse_exception_header, recompute_hash_regions_into, reencrypt_cluster_into,
};
use crate::util::positional_reader::PositionalReader;
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Admission, Budget, Pool, Worker, drive, parallelism};
use std::io::{BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Serialized size of one hash exception: u16 offset plus a 20-byte hash.
const EXCEPTION_ENTRY_SIZE: usize = 22;

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

impl PartitionChunkSpec {
    /// Maximum decoded size for a non-packed group: the payload plus
    /// one exception list at its u16 maximum and alignment padding.
    fn decoded_bound(&self) -> usize {
        // Each chunk is bucketed within one cluster, so it has one exception list.
        let exception_bytes = 2 + usize::from(u16::MAX) * EXCEPTION_ENTRY_SIZE + 3;
        self.expected_payload_len + exception_bytes
    }

    fn stored_bound(&self) -> usize {
        let decoded = self.decoded_bound();
        if self.is_compressed {
            zstd::zstd_safe::compress_bound(decoded)
        } else {
            decoded
        }
    }
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
    scratch_packed: Vec<u8>,
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
            // `data_size == 0` is the format's all-zero sentinel: no
            // stored bytes, no exception list. Zero the chunk's
            // sectors directly (mirroring the raw worker) instead of
            // issuing I/O: full clusters skip the pre-loop zeroing,
            // so this path must clear exactly the sectors it covers.
            if spec.data_size == 0 {
                for b in 0..spec.chunk_n_sectors {
                    self.payloads[spec.first_sector_in_chunk + b] = [0u8; WII_SECTOR_PAYLOAD_SIZE];
                }
                deferred.push((
                    spec.first_sector_in_chunk,
                    spec.first_sector_in_chunk + spec.chunk_n_sectors,
                    Vec::new(),
                ));
                continue;
            }
            let target = spec.decoded_bound();
            if self.scratch_decomp.len() < target {
                self.scratch_decomp.resize(target, 0);
            }
            if spec.rvz_packed_size != 0 {
                let max_stored = spec.stored_bound();
                // The packed scratch must hold the whole zstd frame: the
                // exception-list prefix plus the packed records. Two
                // payloads of record headroom keeps the common chunk on
                // the bulk path (a fully plain chunk packs to payload +
                // 4) while anything larger streams without truncating.
                let exception_bytes_max = 2 + usize::from(u16::MAX) * EXCEPTION_ENTRY_SIZE + 3;
                let packed_target = spec
                    .decoded_bound()
                    .max(spec.rvz_packed_size as usize + exception_bytes_max);
                let (chunk_exceptions, decoded_len) = if spec.data_size as usize <= max_stored
                    && spec.rvz_packed_size as usize <= 2 * spec.expected_payload_len
                {
                    self.scratch_in.resize(spec.data_size as usize, 0);
                    file_read_exact_at(&self.file, &mut self.scratch_in, spec.data_off)?;
                    // Stored frames decode straight from scratch_in; only
                    // zstd frames need the scratch_packed staging pass.
                    let packed: &[u8] = if spec.is_compressed {
                        if self.scratch_packed.len() < packed_target {
                            self.scratch_packed.resize(packed_target, 0);
                        }
                        let packed_len = self.decompressor.decompress_to_buffer(
                            &self.scratch_in,
                            &mut self.scratch_packed[..packed_target],
                        )?;
                        &self.scratch_packed[..packed_len]
                    } else {
                        &self.scratch_in
                    };
                    decode_packed_partition_group(
                        &mut std::io::Cursor::new(packed),
                        spec,
                        &mut self.scratch_decomp[..spec.expected_payload_len],
                    )?
                } else {
                    let stored =
                        PositionalReader::new(&*self.file, spec.data_off, spec.data_size as u64);
                    if spec.is_compressed {
                        let mut stream = zstd::stream::read::Decoder::new(stored)?;
                        decode_packed_partition_group(
                            &mut stream,
                            spec,
                            &mut self.scratch_decomp[..spec.expected_payload_len],
                        )?
                    } else {
                        let mut stream = BufReader::with_capacity(64 * 1024, stored);
                        decode_packed_partition_group(
                            &mut stream,
                            spec,
                            &mut self.scratch_decomp[..spec.expected_payload_len],
                        )?
                    }
                };
                // Like the bulk path, the payload only has to be complete.
                if decoded_len < spec.expected_payload_len {
                    return Err(RvzError::DecompressedSizeMismatch {
                        expected: spec.expected_payload_len as u64,
                        actual: decoded_len as u64,
                    });
                }

                for b in 0..spec.chunk_n_sectors {
                    let block_idx = spec.first_sector_in_chunk + b;
                    self.payloads[block_idx].copy_from_slice(
                        &self.scratch_decomp
                            [b * WII_SECTOR_PAYLOAD_SIZE..(b + 1) * WII_SECTOR_PAYLOAD_SIZE],
                    );
                }
                deferred.push((
                    spec.first_sector_in_chunk,
                    spec.first_sector_in_chunk + spec.chunk_n_sectors,
                    chunk_exceptions,
                ));
                continue;
            }
            let decoded_len = if spec.data_size as usize > spec.stored_bound() {
                // Stored size beyond the codec bound: decode straight from the
                // file into the bounded scratch instead of buffering the input.
                let bound = spec.decoded_bound();
                let stored =
                    PositionalReader::new(&*self.file, spec.data_off, u64::from(spec.data_size));
                if !spec.is_compressed {
                    // Bulk path tolerates trailing bytes in plain groups.
                    file_read_exact_at(
                        &self.file,
                        &mut self.scratch_decomp[..bound],
                        spec.data_off,
                    )?;
                    bound
                } else {
                    let mut source = zstd::stream::read::Decoder::new(stored)?;
                    let mut decoded_len = 0;
                    loop {
                        let count = source.read(&mut self.scratch_decomp[decoded_len..bound])?;
                        if count == 0 {
                            break;
                        }
                        decoded_len += count;
                        if decoded_len == bound {
                            let mut extra = [0u8; 1];
                            let count = source.read(&mut extra)?;
                            if count != 0 {
                                return Err(RvzError::DecompressedSizeMismatch {
                                    expected: bound as u64,
                                    actual: decoded_len as u64 + count as u64,
                                });
                            }
                            break;
                        }
                    }
                    decoded_len
                }
            } else {
                self.scratch_in.resize(spec.data_size as usize, 0);
                file_read_exact_at(&self.file, &mut self.scratch_in, spec.data_off)?;
                if spec.is_compressed {
                    self.decompressor
                        .decompress_to_buffer(&self.scratch_in, &mut self.scratch_decomp)?
                } else {
                    if self.scratch_decomp.len() < self.scratch_in.len() {
                        self.scratch_decomp.resize(self.scratch_in.len(), 0);
                    }
                    self.scratch_decomp[..self.scratch_in.len()].copy_from_slice(&self.scratch_in);
                    self.scratch_in.len()
                }
            };

            // Raw chunks have a 4-byte alignment pad after the exception
            // entries.
            let decompressed = &self.scratch_decomp[..decoded_len];
            let (chunk_exceptions_ref, payload_region) =
                parse_exception_header(decompressed, !spec.is_compressed)?;
            let chunk_exceptions: Vec<HashException> = chunk_exceptions_ref.iter().collect();
            let take = spec.expected_payload_len.min(payload_region.len());
            let unpacked: Vec<u8> = payload_region[..take].to_vec();

            if unpacked.len() != spec.expected_payload_len {
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

fn decode_packed_partition_group<R: Read>(
    reader: &mut R,
    spec: &PartitionChunkSpec,
    output: &mut [u8],
) -> RvzResult<(Vec<HashException>, usize)> {
    // Chunks are bucketed per cluster, so each has exactly one exception list.
    let mut count_bytes = [0; 2];
    reader.read_exact(&mut count_bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            RvzError::Custom("truncated partition chunk header".into())
        } else {
            error.into()
        }
    })?;
    let count = u16::from_be_bytes(count_bytes) as usize;

    let mut exceptions = Vec::new();
    for _ in 0..count {
        let mut entry = [0; EXCEPTION_ENTRY_SIZE];
        reader.read_exact(&mut entry).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                RvzError::Custom("truncated exception list".into())
            } else {
                error.into()
            }
        })?;
        exceptions.push(HashException {
            offset: u16::from_be_bytes([entry[0], entry[1]]),
            hash: entry[2..].try_into().expect("exception hash is 20 bytes"),
        });
    }
    let exception_area = 2 + count * EXCEPTION_ENTRY_SIZE;

    if !spec.is_compressed {
        let padding = (4 - exception_area % 4) % 4;
        let mut pad = [0; 3];
        reader.read_exact(&mut pad[..padding]).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                RvzError::Custom("truncated exception list".into())
            } else {
                error.into()
            }
        })?;
    }

    let mut records = reader.take(spec.rvz_packed_size as u64);
    let (decoded_len, _) = crate::nintendo::disc::rvz::packing::pack_decode_reader(
        &mut records,
        spec.chunk_data_offset_pay,
        output,
    )?;
    Ok((exceptions, decoded_len))
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
        scratch_packed: Vec::new(),
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
) -> RvzResult<Vec<PartitionDecompressWork>> {
    let pd0 = part.pd[0];
    let pd1 = part.pd[1];
    let total_n_groups = pd0
        .n_groups
        .checked_add(pd1.n_groups)
        .ok_or_else(|| RvzError::Custom("partition group count overflows".into()))?;
    if total_n_groups == 0 {
        return Ok(Vec::new());
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
        let Some(group) = groups.get(group_cursor as usize) else {
            return Err(RvzError::Custom("group index past table".into()));
        };

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

    Ok(work_items)
}

/// Build the single [`PartitionDecompressWork`] for one cluster of
/// `part`: the read-side counterpart of [`build_partition_work_items`]
/// (which buckets every cluster of the partition, too much to rebuild
/// per random-access read). Chunks never straddle a cluster boundary:
/// chunk starts are multiples of `chunk_size`, which is a power-of-two
/// divisor of `WII_GROUP_TOTAL_SIZE`, so per-cluster reconstruction is
/// exact. The chunk walk is bounded by the partition's declared
/// pd[0]+pd[1] group range capped at the table length, so a corrupt
/// descriptor can never make it index past `groups`. Returns `None`
/// when the cluster lies past the partition's data or no group in the
/// declared range covers it.
pub(crate) fn build_partition_cluster_work(
    part: &WiaPart,
    groups: &[RvzGroup],
    chunk_size_u64: u64,
    cluster_idx: u64,
) -> Option<PartitionDecompressWork> {
    let pd0 = part.pd[0];
    let pd1 = part.pd[1];
    let total_data_size = (pd0.n_sectors as u64 + pd1.n_sectors as u64) * WII_SECTOR_SIZE_U64;
    let cluster_start = cluster_idx * WII_GROUP_TOTAL_SIZE;
    if cluster_start >= total_data_size {
        return None;
    }
    let cluster_end = (cluster_start + WII_GROUP_TOTAL_SIZE).min(total_data_size);

    // Every chunk before the partition's last one is exactly
    // `chunk_size` bytes, so the group storing the chunk at `enc_pos`
    // sits `enc_pos / chunk_size` entries past `pd[0].group_index`.
    // The cursor is bounded by the partition's declared pd[0]+pd[1]
    // group range, capped by the table length, in u64 so neither the
    // cluster-derived start nor the increment can wrap or index past
    // `groups`.
    let group_index_end =
        (u64::from(pd0.group_index) + u64::from(pd0.n_groups) + u64::from(pd1.n_groups))
            .min(groups.len() as u64);
    let mut group_cursor = u64::from(pd0.group_index) + cluster_start / chunk_size_u64;
    let mut chunks: Vec<PartitionChunkSpec> = Vec::new();
    let mut enc_pos = cluster_start;
    while enc_pos < cluster_end && group_cursor < group_index_end {
        let remaining_in_partition = total_data_size - enc_pos;
        let this_chunk_enc_bytes = chunk_size_u64.min(remaining_in_partition);
        let pos = ChunkSectorPos::new(enc_pos, this_chunk_enc_bytes);
        debug_assert_eq!(pos.cluster_idx, cluster_idx);

        let group = &groups[group_cursor as usize];
        chunks.push(PartitionChunkSpec {
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
        group_cursor += 1;
    }

    if chunks.is_empty() {
        return None;
    }

    Some(PartitionDecompressWork {
        cluster_idx,
        data_start: pd0.first_sector as u64 * WII_SECTOR_SIZE_U64,
        part_key: part.part_key,
        valid_blocks_in_cluster: valid_blocks_for_cluster(cluster_idx, total_data_size),
        chunks,
    })
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
    let work_items = build_partition_work_items(part, groups, chunk_size_u64, usage)?;
    if work_items.is_empty() {
        return Ok(());
    }

    let largest_specs = work_items
        .iter()
        .map(|item| item.chunks.len())
        .max()
        .unwrap_or(0);
    let largest_decoded_bound = work_items
        .iter()
        .flat_map(|item| item.chunks.iter())
        .map(PartitionChunkSpec::decoded_bound)
        .max()
        .unwrap_or(0);
    let largest_expected_payload = work_items
        .iter()
        .flat_map(|item| item.chunks.iter())
        .map(|chunk| chunk.expected_payload_len)
        .max()
        .unwrap_or(0);
    // Scratch per worker: the decomposed-chunk scratch plus the packed
    // frame scratch (both bounded by decoded_bound), headroom for the
    // largest plain payload, then the fixed cluster buffers and the
    // decompressor context.
    let codec_bytes = largest_decoded_bound
        .saturating_mul(2)
        .saturating_add(largest_expected_payload)
        .saturating_add(WII_BLOCKS_PER_GROUP * WII_SECTOR_PAYLOAD_SIZE)
        .saturating_add(WII_BLOCKS_PER_GROUP * HASH_REGION_BYTES)
        .saturating_add(WII_GROUP_TOTAL_SIZE as usize)
        .saturating_add(crate::util::worker_pool::zstd_dctx_estimate());
    let queued_bytes = (WII_GROUP_TOTAL_SIZE as usize)
        .saturating_add(largest_specs.saturating_mul(std::mem::size_of::<PartitionChunkSpec>()));
    let admission = Budget {
        codec_per_worker: codec_bytes,
        per_job: queued_bytes,
        writer_slot: 0,
        fixed: 0,
    }
    .admit(parallelism(), work_items.len() as u64)
    .unwrap_or(Admission::DEGRADED);
    let n_threads = admission.workers;
    let workers = make_partition_decompress_workers(n_threads, file)?;
    let pool: Pool<PartitionDecompressWork, PartitionDecompressOut, RvzError> =
        Pool::spawn(workers);
    let max_in_flight = admission.max_in_flight;

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
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn packed_records() -> (Vec<u8>, Vec<u8>) {
        let mut records = Vec::new();
        records.extend_from_slice(&3u32.to_be_bytes());
        records.extend_from_slice(b"abc");
        records.extend_from_slice(&(0x8000_0004u32).to_be_bytes());
        records.extend_from_slice(&[0x11; 68]);

        let mut decoded = [0; 7];
        let (decoded_len, input_len) = crate::nintendo::disc::rvz::packing::pack_decode_reader(
            &mut Cursor::new(&records),
            0,
            &mut decoded,
        )
        .unwrap();
        assert_eq!(decoded_len, decoded.len());
        assert_eq!(input_len, records.len());
        (records, decoded.to_vec())
    }

    fn append_exception(input: &mut Vec<u8>, offset: u16, hash: [u8; 20]) {
        input.extend_from_slice(&offset.to_be_bytes());
        input.extend_from_slice(&hash);
    }

    #[test]
    fn packed_group_streams_exceptions_and_records() {
        let (records, expected_payload) = packed_records();
        let expected_exception = HashException {
            offset: 0x1234,
            hash: [0x56; 20],
        };
        for is_compressed in [false, true] {
            let mut input = Vec::new();
            input.extend_from_slice(&1u16.to_be_bytes());
            append_exception(
                &mut input,
                expected_exception.offset,
                expected_exception.hash,
            );
            input.extend_from_slice(&records);
            let spec = PartitionChunkSpec {
                data_off: 0,
                data_size: input.len() as u32,
                is_compressed,
                rvz_packed_size: records.len() as u32,
                first_sector_in_chunk: 0,
                chunk_n_sectors: 1,
                chunk_data_offset_pay: 0,
                expected_payload_len: expected_payload.len(),
            };
            let mut payload = [0; 7];
            let (exceptions, decoded_len) =
                decode_packed_partition_group(&mut Cursor::new(input), &spec, &mut payload)
                    .unwrap();

            assert_eq!(&payload, expected_payload.as_slice());
            assert_eq!(decoded_len, expected_payload.len());
            assert_eq!(exceptions, vec![expected_exception]);
        }
    }

    #[test]
    fn packed_group_aligns_single_exception_list() {
        let (records, expected_payload) = packed_records();
        let mut input = vec![0, 0, 0, 0];
        input.extend_from_slice(&records);
        let spec = PartitionChunkSpec {
            data_off: 0,
            data_size: input.len() as u32,
            is_compressed: false,
            rvz_packed_size: records.len() as u32,
            first_sector_in_chunk: 0,
            chunk_n_sectors: 1,
            chunk_data_offset_pay: 0,
            expected_payload_len: expected_payload.len(),
        };
        let mut payload = [0; 7];
        let (exceptions, decoded_len) =
            decode_packed_partition_group(&mut Cursor::new(input), &spec, &mut payload).unwrap();

        assert!(exceptions.is_empty());
        assert_eq!(&payload, expected_payload.as_slice());
        assert_eq!(decoded_len, expected_payload.len());
    }
    #[test]
    fn packed_group_rejects_truncated_exception_entry() {
        let spec = PartitionChunkSpec {
            data_off: 0,
            data_size: 2,
            is_compressed: true,
            rvz_packed_size: 0,
            first_sector_in_chunk: 0,
            chunk_n_sectors: 1,
            chunk_data_offset_pay: 0,
            expected_payload_len: 0,
        };
        assert!(matches!(
            decode_packed_partition_group(&mut Cursor::new([0, 1]), &spec, &mut []),
            Err(RvzError::Custom(message)) if message == "truncated exception list"
        ));
    }

    /// Oversized stored groups are read through the bounded streaming
    /// path instead of sizing the input buffer from the declaration.
    #[test]
    fn non_packed_group_streams_oversized_stored_size_with_bounded_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.rvz");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(1 << 30).unwrap();
        let file = Arc::new(std::fs::File::open(&path).unwrap());
        let mut worker = make_one_partition_worker(&file).unwrap();

        let spec = PartitionChunkSpec {
            data_off: 0,
            data_size: 1 << 30,
            is_compressed: false,
            rvz_packed_size: 0,
            first_sector_in_chunk: 0,
            chunk_n_sectors: WII_BLOCKS_PER_GROUP,
            chunk_data_offset_pay: 0,
            expected_payload_len: WII_BLOCKS_PER_GROUP * WII_SECTOR_PAYLOAD_SIZE,
        };
        assert!(spec.stored_bound() < (1 << 30));
        let result = worker.process(PartitionDecompressWork {
            cluster_idx: 0,
            data_start: 0,
            part_key: [0; 16],
            valid_blocks_in_cluster: WII_BLOCKS_PER_GROUP,
            chunks: vec![spec],
        });
        // Plain groups tolerate trailing stored bytes, exactly like the
        // bulk path; the declaration must not size any buffer.
        assert!(result.is_ok());
        assert!(worker.scratch_in.capacity() < (1 << 30));
    }

    /// A packed stream whose size exceeds `decoded_bound` must take the
    /// streaming branch instead of being truncated into the bulk
    /// scratch. Its records out-produce the chunk's payload, so the
    /// bounded `max_output` walk rejects them with a size mismatch
    /// instead of generating the excess.
    #[test]
    fn packed_group_above_decoded_bound_streams_and_rejects_overrun() {
        let mut records = Vec::new();
        let payload = [0x5Cu8; 1000];
        let bound = WII_SECTOR_PAYLOAD_SIZE + 2 + u16::MAX as usize * EXCEPTION_ENTRY_SIZE + 3;
        while records.len() <= bound {
            records.extend_from_slice(&1000u32.to_be_bytes());
            records.extend_from_slice(&payload);
        }
        let mut stored = Vec::new();
        stored.extend_from_slice(&0u16.to_be_bytes());
        stored.extend_from_slice(&records);
        let compressed = zstd::bulk::compress(&stored, 0).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("packed-over-bound.rvz");
        std::fs::write(&path, &compressed).unwrap();
        let file = Arc::new(std::fs::File::open(&path).unwrap());
        let mut worker = make_one_partition_worker(&file).unwrap();

        let spec = PartitionChunkSpec {
            data_off: 0,
            data_size: compressed.len() as u32,
            is_compressed: true,
            rvz_packed_size: records.len() as u32,
            first_sector_in_chunk: 0,
            chunk_n_sectors: 1,
            chunk_data_offset_pay: 0,
            expected_payload_len: WII_SECTOR_PAYLOAD_SIZE,
        };
        assert!(spec.data_size as usize <= spec.stored_bound());
        assert!(spec.rvz_packed_size as usize > spec.decoded_bound());
        // `PartitionDecompressOut` is not `Debug`, so `unwrap_err` is
        // unavailable; this extracts the error the same way.
        let err = match worker.process(PartitionDecompressWork {
            cluster_idx: 0,
            data_start: 0,
            part_key: [0; 16],
            valid_blocks_in_cluster: 1,
            chunks: vec![spec],
        }) {
            Ok(_) => panic!("oversized packed stream unexpectedly decoded"),
            Err(e) => e,
        };
        assert!(
            matches!(err, RvzError::DecompressedSizeMismatch { .. }),
            "{err}"
        );
        // The streaming branch ran: the bulk packed scratch was never
        // grown, so the rejection cost no bulk-sized buffer.
        assert!(worker.scratch_packed.is_empty());
    }

    /// pd[0] claims one group without sectors and pd[1] claims two
    /// groups for 0x200 sectors, but the table only carries one entry.
    /// A random-access read of cluster 1 must not walk the group cursor
    /// past the table: the bounded walk returns `None` and the caller
    /// reports an error instead of panicking on the index.
    #[test]
    fn cluster_work_never_indexes_past_group_table() {
        use crate::nintendo::disc::rvz::format::WiaPartData;

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
        let groups = vec![RvzGroup::new_compressed(0, 8, 0)];
        assert!(build_partition_cluster_work(&part, &groups, 2 * 1024 * 1024, 1).is_none());
        // Cluster 0 still builds work from the one stored group.
        assert!(build_partition_cluster_work(&part, &groups, 2 * 1024 * 1024, 0).is_some());
    }

    /// A `data_size == 0` group is the format's all-zero sentinel: the
    /// worker must synthesise zero payloads without any I/O instead of
    /// trying to decompress an empty stored chunk. The re-encrypted
    /// cluster is decrypted back, so the payloads provably decode to
    /// zeros.
    #[test]
    fn sentinel_chunk_decodes_to_zero_payloads_without_io() {
        use crate::nintendo::rvl::disc::decrypt_sector;
        use crate::nintendo::rvl::partition::HASH_REGION_BYTES;

        let dir = tempfile::tempdir().unwrap();
        let backing = dir.path().join("backing.bin");
        std::fs::write(&backing, b"sentinel workers never read this").unwrap();
        let file = Arc::new(std::fs::File::open(&backing).unwrap());
        let mut worker = make_one_partition_worker(&file).unwrap();

        let out = worker
            .process(PartitionDecompressWork {
                cluster_idx: 0,
                data_start: 0,
                part_key: [0u8; 16],
                valid_blocks_in_cluster: WII_BLOCKS_PER_GROUP,
                chunks: vec![PartitionChunkSpec {
                    data_off: 0,
                    data_size: 0,
                    is_compressed: false,
                    rvz_packed_size: 0,
                    first_sector_in_chunk: 0,
                    chunk_n_sectors: WII_BLOCKS_PER_GROUP,
                    chunk_data_offset_pay: 0,
                    expected_payload_len: WII_BLOCKS_PER_GROUP * WII_SECTOR_PAYLOAD_SIZE,
                }],
            })
            .unwrap();

        assert_eq!(out.bytes_to_write, WII_GROUP_TOTAL_SIZE as usize);
        for sector_bytes in out.buf.as_chunks::<WII_SECTOR_SIZE>().0 {
            let mut sector = [0u8; WII_SECTOR_SIZE];
            sector.copy_from_slice(sector_bytes);
            decrypt_sector(&mut sector, &[0u8; 16]).unwrap();
            assert!(
                sector[HASH_REGION_BYTES..].iter().all(|&b| b == 0),
                "sentinel chunk must decode to zero payloads"
            );
        }
    }
}
