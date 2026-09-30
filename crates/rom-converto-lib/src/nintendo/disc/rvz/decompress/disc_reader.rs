//! `Read + Seek` view over an RVZ-compressed disc that decompresses
//! only the groups touched by each call. Backs the info commands
//! against multi-GB Wii ISOs without materializing the full image
//! anywhere. Reuses the parallel decoder's worker types
//! (`raw_chunk_work_at`, `build_partition_cluster_work`)
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
    PartitionDecompressOut, PartitionDecompressWorker, build_partition_cluster_work,
    make_one_partition_worker,
};
use super::raw::{
    RawDecompressOut, RawDecompressWork, RawDecompressWorker, check_group_bounds,
    make_one_raw_worker, raw_chunk_work_at,
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
type PackedStreamDecoder = PackedDecoder<io::Take<Box<dyn Read>>>;
type PartCacheEntry = ((usize, u64), Arc<[u8]>);

/// Build a windowed decoder over a packed chunk's stored range. The
/// record walk is cut at the declared `rvz_packed_size`, matching the
/// bulk worker and the region streaming path. The caller has already
/// applied `check_group_bounds` to the descriptor.
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
    let limited = source.take(u64::from(work.rvz_packed_size));
    Ok(PackedDecoder::new(
        limited,
        work.chunk_abs_start,
        work.chunk_bytes,
    ))
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
        let Some((group_idx, work)) =
            raw_chunk_work_at(&region, &self.groups, self.chunk_size, self.iso_size, pos)
        else {
            return Err(io::Error::other("raw chunk lookup failed"));
        };
        let chunk_abs_start = work.chunk_abs_start;
        if work.chunk_bytes > RAW_WHOLE_CHUNK_LIMIT {
            let in_chunk = (pos - chunk_abs_start) as usize;
            let take = (work.chunk_bytes - in_chunk).min(want);
            if work.data_size == 0 {
                buf[..take].fill(0);
                self.pos += take as u64;
                return Ok(Some(take));
            }
            // Every oversized chunk with stored data is held to the
            // descriptor bounds conversion applies.
            check_group_bounds(&work).map_err(io::Error::other)?;
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
                if position >= work.chunk_bytes {
                    // The chunk window is complete: drain the remaining
                    // records, and require the declared record stream to
                    // be consumed exactly and to end there, matching the
                    // bulk worker and the region streaming path.
                    let mut drained: [u8; 0] = [];
                    decoder.read(&mut drained).map_err(io::Error::other)?;
                    if decoder.get_ref().limit() != 0 {
                        return Err(io::Error::other(RvzError::Custom(format!(
                            "packed record stream ends {} bytes short of the declared {}",
                            decoder.get_ref().limit(),
                            work.rvz_packed_size
                        ))));
                    }
                    decoder.get_mut().set_limit(1);
                    let mut probe = [0u8; 1];
                    let extra = decoder
                        .get_mut()
                        .read(&mut probe)
                        .map_err(io::Error::other)?;
                    if extra != 0 {
                        return Err(io::Error::other(RvzError::Custom(format!(
                            "packed record stream continues past the declared {} bytes",
                            work.rvz_packed_size
                        ))));
                    }
                    self.pos += take as u64;
                    return Ok(Some(take));
                }
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
            if position >= work.chunk_bytes {
                // The chunk window is complete: the frame must end here,
                // as the bulk worker (decoding into exactly the chunk)
                // and the region streaming path require.
                let mut probe = [0u8; 1];
                if decoder.read(&mut probe)? != 0 {
                    return Err(io::Error::other(RvzError::DecompressedSizeMismatch {
                        expected: work.chunk_bytes as u64,
                        actual: work.chunk_bytes as u64 + 1,
                    }));
                }
                self.pos += take as u64;
                return Ok(Some(take));
            }
            self.raw_cursor = Some((group_idx, decoder, position));
            self.pos += take as u64;
            return Ok(Some(take));
        }
        let decoded = match self.get_raw_chunk(group_idx, &work) {
            Ok(v) => v,
            Err(e) => return Err(io::Error::other(format!("rvz raw decompress: {}", e))),
        };
        let in_chunk = (pos - chunk_abs_start) as usize;
        if in_chunk >= work.chunk_bytes || in_chunk >= decoded.len() {
            return Ok(Some(0));
        }
        // Serve at most the chunk's declared span: a decoded buffer that
        // overruns it (corrupt stream) must not leak the next chunk's
        // bytes, and a short one is bounded by `decoded.len()`.
        let take = (work.chunk_bytes - in_chunk)
            .min(want)
            .min(decoded.len() - in_chunk);
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
            let total_sectors = part.pd[0].n_sectors as u64 + part.pd[1].n_sectors as u64;
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
        let work = build_partition_cluster_work(part, &self.groups, self.chunk_size, cluster_idx)
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::rvz::format::{RvzGroup, WiaFileHead, WiaRawData};
    use crate::nintendo::disc::rvz::verify::test_support;
    use binrw::{BinRead, BinWrite, Endian};
    use std::io::{Cursor, Read};

    /// Build a raw-only container whose single packed group covers a
    /// chunk larger than the whole-chunk limit, so reads go through the
    /// windowed packed decoder. `declared` is the record stream length
    /// the group declares.
    fn oversized_packed_container(
        records: &[u8],
        declared: u32,
        stored: bool,
        name: &str,
        dir: &tempfile::TempDir,
    ) -> std::path::PathBuf {
        const REGION_SIZE: u64 = 0x110_0000;
        const RAW_DATA_OFF: u64 = 0x8000;
        let group = if stored {
            RvzGroup {
                data_off4: 0,
                data_size: records.len() as u32,
                rvz_packed_size: declared,
            }
        } else {
            RvzGroup::new_compressed(0, records.len() as u32, declared)
        };
        let mut dhead = [0u8; 128];
        dhead[0x1C..0x20].copy_from_slice(&0xC233_9F3Du32.to_be_bytes());
        let build = |group: RvzGroup, trailing: &[u8]| {
            test_support::build_rvz_custom(
                dhead,
                0,
                1,
                &[WiaRawData {
                    raw_data_off: RAW_DATA_OFF,
                    raw_data_size: REGION_SIZE,
                    group_index: 0,
                    n_groups: 1,
                }],
                &[group],
                trailing,
                |disc| disc.chunk_size = 32 * 1024 * 1024,
            )
        };
        // The group table's compressed length does not depend on the
        // group's offset value for a single entry, so a probe build
        // with the real descriptor locates the trailing bytes.
        // Group offsets are 4-byte granular, so the stored bytes start
        // at the next aligned offset after the tables.
        let mut group = group;
        let mut probe = build(group, &[]);
        for _ in 0..4 {
            let off4 = probe.len().div_ceil(4) as u32;
            if group.data_off4 == off4 {
                break;
            }
            group.data_off4 = off4;
            probe = build(group, &[]);
        }
        let data_off = u64::from(group.data_off4) * 4;
        assert!(data_off >= probe.len() as u64);
        // The file backs the whole region span with zero padding so the
        // truncation gates stay quiet.
        let mut trailing = vec![0u8; (data_off - probe.len() as u64) as usize];
        trailing.extend_from_slice(records);
        let total = RAW_DATA_OFF + REGION_SIZE;
        if trailing.len() < (total - probe.len() as u64) as usize {
            trailing.resize((total - probe.len() as u64) as usize, 0);
        }
        let mut file = build(group, &trailing);
        // Raise the declared ISO size over the region span.
        let mut head =
            WiaFileHead::read_options(&mut Cursor::new(&file[..]), Endian::Big, ()).unwrap();
        head.iso_file_size = RAW_DATA_OFF + REGION_SIZE;
        head.file_head_hash =
            crate::nintendo::disc::rvz::format::sha1::compute_file_head_hash(&head);
        head.write_options(&mut Cursor::new(&mut file), Endian::Big, ())
            .unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, &file).unwrap();
        path
    }

    /// The random-access reader rejects a stored packed chunk whose
    /// stored size differs from its declared record stream, exactly
    /// like the bulk worker and the region streaming path.
    #[test]
    fn reader_rejects_stored_packed_size_mismatch() {
        // A plain record header with a short payload. The stored-size
        // check runs before any record is read, so this vector pins that
        // check's message; with it disabled the record walk still
        // rejects the vector as a truncated payload, with different
        // text.
        let mut records = Vec::new();
        records.extend_from_slice(&0x110_0000u32.to_be_bytes());
        records.extend_from_slice(&[0x5Au8; 68]);

        let dir = tempfile::tempdir().unwrap();
        let path = oversized_packed_container(
            &records,
            records.len() as u32 + 4,
            true,
            "reader_size_mismatch.rvz",
            &dir,
        );
        let mut reader = RvzDiscReader::open(&path).unwrap();
        let mut out = Vec::new();
        let err = match reader.read_to_end(&mut out) {
            Ok(_) => panic!("stored packed size mismatch unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, e if e.to_string().contains("stored packed chunk holds")
                && e.to_string().contains("declares")),
            "{err}"
        );
    }

    /// The random-access reader requires a compressed packed chunk's
    /// record stream to fill its declared length exactly, like the
    /// bulk worker and the region streaming path.
    #[test]
    fn reader_rejects_short_compressed_packed_stream() {
        // One junk record filling the whole chunk (72 record bytes),
        // declared as 76.
        let mut records = Vec::new();
        records.extend_from_slice(&(0x8000_0000u32 | 0x110_0000).to_be_bytes());
        records.extend_from_slice(&[0x5Au8; 68]);
        let stored = zstd::bulk::compress(&records, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = oversized_packed_container(
            &stored,
            records.len() as u32 + 4,
            false,
            "reader_short.rvz",
            &dir,
        );
        let mut reader = RvzDiscReader::open(&path).unwrap();
        let mut out = Vec::new();
        let err = match reader.read_to_end(&mut out) {
            Ok(_) => panic!("short packed stream unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, e if e.to_string().contains("ends 4 bytes short of the declared 76")),
            "{err}"
        );
    }

    /// The reader requires a compressed packed chunk's record stream to
    /// end at its declared length: bytes past `rvz_packed_size` fail
    /// with the same 'continues past' rejection as the other paths.
    #[test]
    fn reader_rejects_compressed_packed_stream_past_declared_end() {
        // One junk record filling the whole chunk (72 record bytes),
        // then 4 further bytes the declaration does not cover.
        let mut records = Vec::new();
        records.extend_from_slice(&(0x8000_0000u32 | 0x110_0000).to_be_bytes());
        records.extend_from_slice(&[0x5Au8; 68]);
        let declared = records.len() as u32;
        records.extend_from_slice(&[0u8; 4]);
        let stored = zstd::bulk::compress(&records, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = oversized_packed_container(&stored, declared, false, "reader_long.rvz", &dir);
        let mut reader = RvzDiscReader::open(&path).unwrap();
        let mut out = Vec::new();
        let err = match reader.read_to_end(&mut out) {
            Ok(_) => panic!("packed stream past its declared end unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            err.to_string()
                .contains("continues past the declared 72 bytes"),
            "{err}"
        );
    }

    /// The reader holds a packed descriptor to the same bounds as
    /// conversion: a declared record stream above the worst-case bound
    /// is rejected before any decoding.
    #[test]
    fn reader_applies_the_shared_packed_bounds() {
        let mut records = Vec::new();
        records.extend_from_slice(&(0x8000_0000u32 | 0x110_0000).to_be_bytes());
        records.extend_from_slice(&[0x5Au8; 68]);
        let stored = zstd::bulk::compress(&records, 0).unwrap();
        // The bound for the 17 MiB (0x110_0000-byte) chunk is the chunk
        // plus 76 bytes per sector; declare one byte more than the
        // chunk's bound allows.
        let chunk_bytes: u64 = 0x110_0000;
        let stage1_cap = chunk_bytes + 76 * chunk_bytes.div_ceil(0x8000);
        let dir = tempfile::tempdir().unwrap();
        let path = oversized_packed_container(
            &stored,
            (stage1_cap + 1) as u32,
            false,
            "reader_bounds.rvz",
            &dir,
        );
        let mut reader = RvzDiscReader::open(&path).unwrap();
        let mut out = Vec::new();
        let err = match reader.read_to_end(&mut out) {
            Ok(_) => panic!("over-bound packed descriptor unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("decompressed size mismatch"),
            "{err}"
        );
    }

    /// A compressed non-packed oversized chunk whose zstd frame decodes
    /// past `chunk_bytes` is rejected by the reader like conversion
    /// rejects it.
    #[test]
    fn reader_rejects_oversized_raw_frame_past_chunk() {
        // One byte more than the 17 MiB chunk the region declares.
        let decoded = vec![0x77u8; 0x110_0001];
        let stored = zstd::bulk::compress(&decoded, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = oversized_packed_container(&stored, 0, false, "reader_raw_over.rvz", &dir);
        let mut reader = RvzDiscReader::open(&path).unwrap();
        let mut out = Vec::new();
        let err = match reader.read_to_end(&mut out) {
            Ok(_) => panic!("raw frame past the chunk unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("decompressed size mismatch"),
            "{err}"
        );
    }

    /// A stored non-packed oversized chunk larger than the descriptor
    /// bound is rejected by the reader as conversion rejects it, even
    /// though the direct read would otherwise serve its bytes.
    #[test]
    fn reader_rejects_oversized_stored_chunk_above_the_bound() {
        let chunk_bytes: usize = 0x110_0000;
        let stage1_cap = chunk_bytes + 76 * chunk_bytes.div_ceil(0x8000);
        // One byte above the stored bound for an uncompressed chunk.
        let stored = vec![0x33u8; stage1_cap + 1];
        let dir = tempfile::tempdir().unwrap();
        let path = oversized_packed_container(&stored, 0, true, "reader_stored_over.rvz", &dir);
        let mut reader = RvzDiscReader::open(&path).unwrap();
        let mut out = Vec::new();
        let err = match reader.read_to_end(&mut out) {
            Ok(_) => panic!("over-bound stored chunk unexpectedly accepted"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains(&format!(
                "group stores {} bytes, more than the {}-byte bound",
                stage1_cap + 1,
                stage1_cap
            )),
            "{err}"
        );
    }
}
