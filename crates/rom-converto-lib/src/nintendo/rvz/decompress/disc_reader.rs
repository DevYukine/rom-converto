//! `Read + Seek` view over an RVZ-compressed disc that decompresses
//! only the groups touched by each call. Backs the info commands
//! against multi-GB Wii ISOs without materialising the full image
//! anywhere. Reuses the parallel decoder's worker types
//! ([`build_raw_region_work_items`], [`build_partition_work_items`])
//! single-threaded; small LRU caches keep repeat reads in the same
//! region cheap.

use binrw::{BinRead, Endian};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::nintendo::rvl::constants::WII_SECTOR_SIZE_U64;
use crate::nintendo::rvz::constants::RVZ_MAGIC;
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::sha1::{compute_disc_hash, compute_file_head_hash};
use crate::nintendo::rvz::format::{RvzGroup, WiaDisc, WiaFileHead, WiaPart, WiaRawData};
use crate::util::worker_pool::Worker;

use super::partition::{
    PartitionDecompressOut, PartitionDecompressWorker, build_partition_work_items,
    make_one_partition_worker,
};
use super::raw::{
    RawDecompressOut, RawDecompressWork, RawDecompressWorker, build_raw_region_work_items,
    make_one_raw_worker,
};

const RAW_CACHE_CAP: usize = 8;
const PART_CACHE_CAP: usize = 4;

pub struct RvzDiscReader {
    #[allow(dead_code)]
    head: WiaFileHead,
    disc: WiaDisc,
    parts: Vec<WiaPart>,
    raw_data: Vec<WiaRawData>,
    groups: Vec<RvzGroup>,
    chunk_size: u64,
    iso_size: u64,
    pos: u64,

    raw_worker: RawDecompressWorker,
    part_worker: PartitionDecompressWorker,

    raw_cache: VecDeque<(u32, Arc<[u8]>)>,
    part_cache: VecDeque<((usize, u64), Arc<[u8]>)>,
}

impl RvzDiscReader {
    pub fn open(path: &Path) -> RvzResult<Self> {
        let mut reader = BufReader::with_capacity(1024 * 1024, File::open(path)?);

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
            let mut buf =
                vec![0u8; disc.n_part as usize * crate::nintendo::rvz::format::WIA_PART_SIZE];
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

        let shared_file = Arc::new(File::open(path)?);
        let raw_worker = make_one_raw_worker(&shared_file)?;
        let part_worker = make_one_partition_worker(&shared_file)?;

        let chunk_size = disc.chunk_size as u64;
        let iso_size = head.iso_file_size;

        Ok(Self {
            head,
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
        })
    }

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
            .ok_or_else(|| io::Error::other("raw chunk lookup failed"))?;
        let group_idx = self.group_index_for_raw(&region, pos);
        let chunk_abs_start = work.chunk_abs_start;
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
                return Err(io::Error::other(format!(
                    "rvz partition decompress: {}",
                    e
                )));
            }
        };
        let in_cluster = (enc_pos_in_part
            % crate::nintendo::rvl::constants::WII_GROUP_TOTAL_SIZE) as usize;
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
            let total_sectors =
                (part.pd[0].n_sectors + part.pd[1].n_sectors) as u64;
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
    ) -> Option<RawDecompressWork> {
        let items =
            build_raw_region_work_items(region, &self.groups, self.chunk_size, self.iso_size);
        items.into_iter().find(|w| {
            pos >= w.chunk_abs_start
                && pos < w.chunk_abs_start + w.chunk_bytes as u64
        })
    }

    fn get_raw_chunk(
        &mut self,
        group_idx: u32,
        work: &RawDecompressWork,
    ) -> RvzResult<Arc<[u8]>> {
        if let Some(pos) = self.raw_cache.iter().position(|(k, _)| *k == group_idx) {
            let (k, v) = self.raw_cache.remove(pos).unwrap();
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
            let (k, v) = self.part_cache.remove(pos).unwrap();
            self.part_cache.push_back((k, v.clone()));
            return Ok(v);
        }
        let all = build_partition_work_items(part, &self.groups, self.chunk_size);
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
        let new_pos: i128 = match from {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.iso_size as i128 + d as i128,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to negative offset",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

