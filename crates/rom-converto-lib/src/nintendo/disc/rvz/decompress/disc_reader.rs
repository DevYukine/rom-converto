//! `Read + Seek` view over an RVZ-compressed disc that decompresses
//! only the groups touched by each call. Backs the info commands
//! against multi-GB Wii ISOs without materializing the full image
//! anywhere. Reuses the parallel decoder's worker types
//! (`build_raw_region_work_items`, `build_partition_work_items`)
//! single-threaded; small LRU caches keep repeat reads in the same
//! region cheap.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use crate::nintendo::disc::rvz::format::{RvzGroup, WiaDisc, WiaPart, WiaRawData};
use crate::nintendo::disc::rvz::packing::PackedDecoder;
use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::util::positional_reader::PositionalReader;
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::Worker;

use super::parse_rvz_metadata;
use super::partition::{
    PartitionDecompressOut, PartitionDecompressWorker, build_partition_work_items,
    make_one_partition_worker,
};
use super::raw::{
    RawDecompressOut, RawDecompressWork, RawDecompressWorker, build_raw_region_work_items,
    make_one_raw_worker,
};

// Chunks up to this size are decoded whole; larger ones stream through the
// bounded cursor, since a raw-only RVZ may declare a far larger chunk_size.
// With the 8-entry cache this keeps retained raw chunks <= 128 MiB.
const RAW_WHOLE_CHUNK_LIMIT: usize = 16 * 1024 * 1024;
// Retain the last decoded raw chunks for repeat reads.
const RAW_CACHE_CAP: usize = 8;
// Cap each streaming discard/read operation while traversing oversized chunks.
const STREAM_BUFFER_SIZE: usize = 1024 * 1024;
// Partition clusters are small enough that a few recent ones help common seeks.
const PART_CACHE_CAP: usize = 4;

type RawStreamDecoder =
    zstd::stream::read::Decoder<'static, BufReader<PositionalReader<Arc<File>>>>;
type PackedStreamDecoder = PackedDecoder<Box<dyn Read>>;
type PartCacheEntry = ((usize, u64), Arc<[u8]>);

/// Build a windowed decoder over a packed chunk's stored range.
fn make_packed_stream_decoder(
    file: Arc<File>,
    work: &RawDecompressWork,
) -> RvzResult<PackedStreamDecoder> {
    let stored = PositionalReader::new(file, work.data_off, u64::from(work.data_size));
    let source: Box<dyn Read> = if work.is_compressed {
        let buffered = BufReader::with_capacity(STREAM_BUFFER_SIZE, stored);
        Box::new(zstd::stream::read::Decoder::with_buffer(buffered)?)
    } else {
        Box::new(BufReader::with_capacity(STREAM_BUFFER_SIZE, stored))
    };
    Ok(PackedDecoder::new(source, work.chunk_abs_start))
}

/// `Read + Seek` view over an RVZ container that decodes only the groups
/// touched by each call, caching recently decoded raw chunks and
/// partition clusters.
pub struct RvzDiscReader {
    disc: WiaDisc,
    parts: Vec<WiaPart>,
    raw_data: Vec<WiaRawData>,
    groups: Vec<RvzGroup>,
    chunk_size: u64,
    iso_size: u64,
    pos: u64,

    raw_worker: RawDecompressWorker,
    part_worker: PartitionDecompressWorker,
    file: Arc<File>,

    raw_cache: VecDeque<(u32, Arc<[u8]>)>,
    raw_cursor: Option<(u32, RawStreamDecoder, usize)>,
    packed_cursor: Option<(u32, PackedStreamDecoder, usize)>,
    part_cache: VecDeque<PartCacheEntry>,
    discard: Vec<u8>,
}

impl RvzDiscReader {
    /// Opens the RVZ container at `path` and reads its metadata tables.
    pub fn open(path: &Path) -> RvzResult<Self> {
        let (shared_file, head, disc, parts, raw_data, groups) = parse_rvz_metadata(path)?;
        let file = Arc::clone(&shared_file);
        let raw_worker = make_one_raw_worker(&shared_file)?;
        let part_worker = make_one_partition_worker(&shared_file)?;

        let chunk_size = disc.chunk_size as u64;
        let iso_size = head.iso_file_size;

        Ok(Self {
            disc,
            parts,
            raw_data,
            groups,
            chunk_size,
            iso_size,
            pos: 0,
            raw_worker,
            part_worker,
            raw_cache: VecDeque::with_capacity(RAW_CACHE_CAP),
            part_cache: VecDeque::with_capacity(PART_CACHE_CAP),
            file,
            raw_cursor: None,
            packed_cursor: None,
            discard: vec![0; STREAM_BUFFER_SIZE],
        })
    }

    /// Size of the logical (decompressed) disc image in bytes.
    pub fn iso_size(&self) -> u64 {
        self.iso_size
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.iso_size {
            return Ok(0);
        }
        let remaining_in_iso = self.iso_size - self.pos;
        let want = (buf.len() as u64).min(remaining_in_iso) as usize;
        if want == 0 {
            return Ok(0);
        }

        let pos = self.pos;
        let dhead_len = self.disc.dhead.len() as u64;
        if pos < dhead_len {
            let take = (dhead_len - pos).min(want as u64) as usize;
            buf[..take].copy_from_slice(&self.disc.dhead[pos as usize..pos as usize + take]);
            self.pos += take as u64;
            return Ok(take);
        }

        if let Some(serve) = self.try_read_from_raw(pos, want, buf)? {
            return Ok(serve);
        }
        if let Some(serve) = self.try_read_from_partition(pos, want, buf)? {
            return Ok(serve);
        }

        let bound = self.next_boundary_after(pos);
        let zero_len = (bound - pos).min(want as u64) as usize;
        for slot in &mut buf[..zero_len] {
            *slot = 0;
        }
        self.pos += zero_len as u64;
        Ok(zero_len)
    }

    fn try_read_from_raw(
        &mut self,
        pos: u64,
        want: usize,
        buf: &mut [u8],
    ) -> io::Result<Option<usize>> {
        let Some(region_idx) = self.find_raw_region(pos) else {
            return Ok(None);
        };
        let region = self.raw_data[region_idx].clone();
        let work = self
            .build_raw_chunk_work_for(&region, pos)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("raw chunk lookup failed"))?;
        let chunk_abs_start = work.chunk_abs_start;
        let group_idx = self.group_index_for_raw(&region, pos);
        if work.chunk_bytes > RAW_WHOLE_CHUNK_LIMIT {
            let in_chunk = (pos - chunk_abs_start) as usize;
            let take = (work.chunk_bytes - in_chunk).min(want);
            if work.data_size == 0 {
                buf[..take].fill(0);
                self.pos += take as u64;
                return Ok(Some(take));
            }
            if work.rvz_packed_size != 0 {
                // Serve oversized packed chunks through the windowed
                // decoder instead of materializing the whole chunk.
                let (mut decoder, mut position) = match self.packed_cursor.take() {
                    Some((idx, decoder, position)) if idx == group_idx && position <= in_chunk => {
                        (decoder, position)
                    }
                    _ => (
                        make_packed_stream_decoder(Arc::clone(&self.file), &work)
                            .map_err(io::Error::other)?,
                        0,
                    ),
                };
                while position < in_chunk {
                    let discard_len = (in_chunk - position).min(self.discard.len());
                    let n = decoder
                        .read(&mut self.discard[..discard_len])
                        .map_err(io::Error::other)?;
                    if n == 0 {
                        return Err(io::Error::other("truncated RVZ raw chunk"));
                    }
                    position += n;
                }
                let mut written = 0;
                while written < take {
                    let n = decoder
                        .read(&mut buf[written..take])
                        .map_err(io::Error::other)?;
                    if n == 0 {
                        return Err(io::Error::other("truncated RVZ raw chunk"));
                    }
                    written += n;
                }
                position += written;
                self.packed_cursor = Some((group_idx, decoder, position));
                self.pos += take as u64;
                return Ok(Some(take));
            }
            if !work.is_compressed {
                let required = work.chunk_slice_offset + work.write_len;
                if (work.data_size as usize) < required {
                    return Err(io::Error::other(RvzError::DecompressedSizeMismatch {
                        expected: required as u64,
                        actual: u64::from(work.data_size),
                    }));
                }
                let requested_end = in_chunk + take;
                if (work.data_size as usize) < requested_end {
                    return Err(io::Error::other(RvzError::DecompressedSizeMismatch {
                        expected: requested_end as u64,
                        actual: u64::from(work.data_size),
                    }));
                }
                file_read_exact_at(
                    &self.file,
                    &mut buf[..take],
                    work.data_off + in_chunk as u64,
                )?;
                self.pos += take as u64;
                return Ok(Some(take));
            }
            let (mut decoder, mut position) = match self.raw_cursor.take() {
                Some((idx, decoder, position)) if idx == group_idx && position <= in_chunk => {
                    (decoder, position)
                }
                _ => {
                    let source = PositionalReader::new(
                        self.file.clone(),
                        work.data_off,
                        u64::from(work.data_size),
                    );
                    let buffered = BufReader::with_capacity(STREAM_BUFFER_SIZE, source);
                    (RawStreamDecoder::with_buffer(buffered)?, 0)
                }
            };
            while position < in_chunk {
                let discard_len = (in_chunk - position).min(self.discard.len());
                let n = decoder.read(&mut self.discard[..discard_len])?;
                if n == 0 {
                    return Err(io::Error::other("truncated RVZ raw chunk"));
                }
                position += n;
            }
            let mut written = 0;
            while written < take {
                let n = decoder.read(&mut buf[written..take])?;
                if n == 0 {
                    return Err(io::Error::other("truncated RVZ raw chunk"));
                }
                written += n;
            }
            position += written;
            self.raw_cursor = Some((group_idx, decoder, position));
            self.pos += take as u64;
            return Ok(Some(take));
        }
        let decoded = match self.get_raw_chunk(group_idx, &work) {
            Ok(v) => v,
            Err(e) => return Err(io::Error::other(format!("rvz raw decompress: {}", e))),
        };
        let in_chunk = (pos - chunk_abs_start) as usize;
        if in_chunk >= decoded.len() {
            return Ok(Some(0));
        }
        let take = (decoded.len() - in_chunk).min(want);
        buf[..take].copy_from_slice(&decoded[in_chunk..in_chunk + take]);
        self.pos += take as u64;
        Ok(Some(take))
    }

    fn try_read_from_partition(
        &mut self,
        pos: u64,
        want: usize,
        buf: &mut [u8],
    ) -> io::Result<Option<usize>> {
        let Some(part_idx) = self.find_partition(pos) else {
            return Ok(None);
        };
        let part = self.parts[part_idx].clone();
        let data_start = part.pd[0].first_sector as u64 * WII_SECTOR_SIZE_U64;
        let enc_pos_in_part = pos - data_start;
        let cluster_idx = enc_pos_in_part / crate::nintendo::rvl::constants::WII_GROUP_TOTAL_SIZE;
        let cluster = match self.get_partition_cluster(part_idx, cluster_idx, &part) {
            Ok(v) => v,
            Err(e) => {
                return Err(io::Error::other(format!("rvz partition decompress: {}", e)));
            }
        };
        let in_cluster =
            (enc_pos_in_part % crate::nintendo::rvl::constants::WII_GROUP_TOTAL_SIZE) as usize;
        if in_cluster >= cluster.len() {
            return Ok(Some(0));
        }
        let take = (cluster.len() - in_cluster).min(want);
        buf[..take].copy_from_slice(&cluster[in_cluster..in_cluster + take]);
        self.pos += take as u64;
        Ok(Some(take))
    }

    fn find_raw_region(&self, pos: u64) -> Option<usize> {
        self.raw_data
            .iter()
            .position(|r| pos >= r.raw_data_off && pos < r.raw_data_off + r.raw_data_size)
    }

    fn find_partition(&self, pos: u64) -> Option<usize> {
        for (idx, part) in self.parts.iter().enumerate() {
            let start = part.pd[0].first_sector as u64 * WII_SECTOR_SIZE_U64;
            let total_sectors = (part.pd[0].n_sectors + part.pd[1].n_sectors) as u64;
            let end = start + total_sectors * WII_SECTOR_SIZE_U64;
            if pos >= start && pos < end {
                return Some(idx);
            }
        }
        None
    }

    fn next_boundary_after(&self, pos: u64) -> u64 {
        let mut next = self.iso_size;
        for r in &self.raw_data {
            if r.raw_data_off > pos && r.raw_data_off < next {
                next = r.raw_data_off;
            }
        }
        for part in &self.parts {
            let start = part.pd[0].first_sector as u64 * WII_SECTOR_SIZE_U64;
            if start > pos && start < next {
                next = start;
            }
        }
        next
    }

    fn group_index_for_raw(&self, region: &WiaRawData, pos: u64) -> u32 {
        let effective_start = region.raw_data_off - (region.raw_data_off % WII_SECTOR_SIZE_U64);
        let local = pos - effective_start;
        let chunk_in_region = (local / self.chunk_size) as u32;
        region.group_index + chunk_in_region
    }

    fn build_raw_chunk_work_for(
        &self,
        region: &WiaRawData,
        pos: u64,
    ) -> RvzResult<Option<RawDecompressWork>> {
        let items = build_raw_region_work_items(
            region,
            &self.groups,
            self.chunk_size,
            self.iso_size,
            None,
        )?;
        Ok(items
            .into_iter()
            .find(|w| pos >= w.chunk_abs_start && pos < w.chunk_abs_start + w.chunk_bytes as u64))
    }

    fn get_raw_chunk(&mut self, group_idx: u32, work: &RawDecompressWork) -> RvzResult<Arc<[u8]>> {
        if let Some(pos) = self.raw_cache.iter().position(|(k, _)| *k == group_idx) {
            let (k, v) = self
                .raw_cache
                .remove(pos)
                .expect("pos came from position() on this deque above");
            self.raw_cache.push_back((k, v.clone()));
            return Ok(v);
        }
        let started = Instant::now();
        let out: RawDecompressOut = self.raw_worker.process(work.clone())?;
        log::trace!(
            "rvz disc reader: raw chunk {} decoded in {:.1?}",
            group_idx,
            started.elapsed()
        );
        let bytes: Arc<[u8]> = out.decoded.into_vec().into();
        if self.raw_cache.len() >= RAW_CACHE_CAP {
            self.raw_cache.pop_front();
        }
        self.raw_cache.push_back((group_idx, bytes.clone()));
        Ok(bytes)
    }
    fn get_partition_cluster(
        &mut self,
        part_idx: usize,
        cluster_idx: u64,
        part: &WiaPart,
    ) -> RvzResult<Arc<[u8]>> {
        let key = (part_idx, cluster_idx);
        if let Some(pos) = self.part_cache.iter().position(|(k, _)| *k == key) {
            let (k, v) = self
                .part_cache
                .remove(pos)
                .expect("pos came from position() on this deque above");
            self.part_cache.push_back((k, v.clone()));
            return Ok(v);
        }
        let all = build_partition_work_items(part, &self.groups, self.chunk_size, None)?;
        let work = all
            .into_iter()
            .find(|w| w.cluster_idx == cluster_idx)
            .ok_or_else(|| {
                RvzError::Custom(format!(
                    "rvz disc reader: no work for part {} cluster {}",
                    part_idx, cluster_idx
                ))
            })?;
        let started = Instant::now();
        let out: PartitionDecompressOut = self.part_worker.process(work)?;
        log::trace!(
            "rvz disc reader: part {} cluster {} decoded in {:.1?}",
            part_idx,
            cluster_idx,
            started.elapsed()
        );
        let bytes: Arc<[u8]> = out.buf.into_vec().into();
        if self.part_cache.len() >= PART_CACHE_CAP {
            self.part_cache.pop_front();
        }
        self.part_cache.push_back((key, bytes.clone()));
        Ok(bytes)
    }
}

impl Read for RvzDiscReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf)
    }
}

impl Seek for RvzDiscReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = crate::util::positional_reader::seek_target(self.pos, self.iso_size, from)?;
        Ok(self.pos)
    }
}
