//! `Read + Seek` view over a WIA file that reconstructs the full
//! encrypted disc image on the fly.
//!
//! The logical ISO is tiled into contiguous segments (disc header,
//! raw groups, partition groups, zero gaps), each materialized on the
//! shared worker pool through [`PipelinedGroupReader`]: workers
//! decompress group data with the file's codec and, for Wii partition
//! groups, rebuild the H0/H1/H2 hash tree, apply the stored hash
//! exceptions, and AES-encrypt the sectors. The result is
//! byte-identical to the original disc the WIA was created from.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use binrw::{BinRead, Endian};
use sha1::{Digest, Sha1};
use std::io::Cursor;

use super::codec::WiaCodec;
use super::error::{WiaError, WiaResult};
use super::format::{
    WIA_COMPR_LZMA, WIA_COMPR_LZMA2, WIA_GROUP_SIZE, WIA_MAGIC, WIA_VERSION,
    WIA_VERSION_READ_COMPATIBLE, WiaGroup, exception_lists_per_group, validate_disc,
};
use crate::nintendo::disc::rvz::format::sha1::compute_file_head_hash;
use crate::nintendo::disc::rvz::format::{
    WIA_FILE_HEAD_SIZE, WIA_PART_SIZE, WIA_RAW_DATA_SIZE, WiaDisc, WiaFileHead, WiaPart, WiaRawData,
};
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE, WII_SECTOR_SIZE_U64,
};
use crate::nintendo::rvl::partition::{
    HASH_REGION_BYTES, apply_hash_exceptions, recompute_hash_regions_into, reencrypt_cluster_into,
};
use crate::util::group_reader::{GroupSpan, PipelinedGroupReader, in_flight_cap};
use crate::util::worker_pool::{Worker, parallelism};

/// Parsed and hash-verified WIA metadata.
pub(crate) struct WiaLayout {
    pub head: WiaFileHead,
    pub disc: WiaDisc,
    pub parts: Vec<WiaPart>,
    pub raw_data: Vec<WiaRawData>,
    pub groups: Vec<WiaGroup>,
    pub file_len: u64,
}

/// Largest LZMA/LZMA2 dictionary the decoder will allocate for a
/// file's declared properties; the reference encoder's highest level
/// writes 64 MiB, so anything larger is a hostile reservation.
const MAX_LZMA_DICT_BYTES: u32 = 256 * 1024 * 1024;

/// Rejects declared LZMA/LZMA2 dictionaries larger than
/// [`MAX_LZMA_DICT_BYTES`]; `WiaCodec::new` allocates the declared
/// size up front. Other codecs have no dictionary.
fn validate_lzma_dict(compression: u32, props: &[u8]) -> WiaResult<()> {
    let dict = match compression {
        WIA_COMPR_LZMA if props.len() >= 5 => u32::from_le_bytes(
            props[1..5]
                .try_into()
                .expect("props is at least five bytes"),
        ) as u64,
        WIA_COMPR_LZMA2 if !props.is_empty() => {
            let prop = props[0] as u32;
            if prop >= 40 {
                u32::MAX as u64
            } else {
                ((2 + (prop & 1)) as u64) << (prop / 2 + 11)
            }
        }
        _ => return Ok(()),
    };
    if dict > MAX_LZMA_DICT_BYTES as u64 {
        return Err(WiaError::InvalidHeader(format!(
            "LZMA dictionary of {dict} bytes exceeds the {} byte limit",
            MAX_LZMA_DICT_BYTES
        )));
    }
    Ok(())
}

impl WiaLayout {
    /// Parse the header chain, verifying every stored SHA-1 on the
    /// way (file head, disc struct, partition table) and decoding the
    /// codec-compressed metadata tables.
    pub(crate) fn parse(f: &mut File) -> WiaResult<Self> {
        f.seek(SeekFrom::Start(0))?;
        let mut head_bytes = [0u8; WIA_FILE_HEAD_SIZE];
        f.read_exact(&mut head_bytes)?;
        let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
        if head.magic != WIA_MAGIC {
            return Err(WiaError::InvalidMagic(head.magic));
        }
        if compute_file_head_hash(&head) != head.file_head_hash {
            return Err(WiaError::HashChainMismatch("file header"));
        }
        if head.version_compatible > WIA_VERSION || head.version < WIA_VERSION_READ_COMPATIBLE {
            return Err(WiaError::UnsupportedVersion {
                version: head.version,
                compatible: head.version_compatible,
            });
        }
        let file_len = f.metadata()?.len();
        if head.wia_file_size != file_len {
            return Err(WiaError::InvalidHeader(format!(
                "header declares {} bytes but the file is {} bytes",
                head.wia_file_size, file_len
            )));
        }
        crate::util::validate_extent(
            WIA_FILE_HEAD_SIZE as u64,
            head.disc_size as u64,
            file_len,
            "disc struct",
        )?;
        if (head.disc_size as usize) < crate::nintendo::disc::rvz::format::WIA_DISC_SIZE {
            return Err(WiaError::InvalidHeader("disc struct too small".into()));
        }

        let mut disc_bytes = [0u8; crate::nintendo::disc::rvz::format::WIA_DISC_SIZE];
        f.seek(SeekFrom::Start(WIA_FILE_HEAD_SIZE as u64))?;
        f.read_exact(&mut disc_bytes)?;
        let disc = WiaDisc::read_options(&mut Cursor::new(&disc_bytes), Endian::Big, ())?;

        let mut hasher = Sha1::new();
        hasher.update(disc_bytes);
        let mut remaining = head.disc_size as u64 - disc_bytes.len() as u64;
        f.seek(SeekFrom::Start(
            WIA_FILE_HEAD_SIZE as u64 + disc_bytes.len() as u64,
        ))?;
        let mut hash_buf = [0u8; 64 * 1024];
        while remaining > 0 {
            let count = remaining.min(hash_buf.len() as u64) as usize;
            f.read_exact(&mut hash_buf[..count])?;
            hasher.update(&hash_buf[..count]);
            remaining -= count as u64;
        }
        if <[u8; 20]>::from(hasher.finalize()) != head.disc_hash {
            return Err(WiaError::HashChainMismatch("disc struct"));
        }
        validate_disc(&disc)?;

        let parts = if disc.n_part > 0 {
            if (disc.part_t_size as usize) < WIA_PART_SIZE {
                return Err(WiaError::InvalidHeader(format!(
                    "partition entry size {} too small",
                    disc.part_t_size
                )));
            }
            let table_len = (disc.n_part as u64)
                .checked_mul(disc.part_t_size as u64)
                .ok_or_else(|| WiaError::InvalidHeader("partition table size overflows".into()))?;
            crate::util::validate_extent(disc.part_off, table_len, file_len, "partition table")?;
            f.seek(SeekFrom::Start(disc.part_off))?;
            let mut hasher = Sha1::new();
            let row_size = disc.part_t_size as usize;
            let mut out = Vec::with_capacity(disc.n_part as usize);
            for _ in 0..disc.n_part {
                let mut row = [0u8; WIA_PART_SIZE];
                f.read_exact(&mut row)?;
                hasher.update(row);
                let mut remaining = row_size - WIA_PART_SIZE;
                while remaining > 0 {
                    let count = remaining.min(hash_buf.len());
                    f.read_exact(&mut hash_buf[..count])?;
                    hasher.update(&hash_buf[..count]);
                    remaining -= count;
                }
                out.push(WiaPart::read_options(
                    &mut Cursor::new(row),
                    Endian::Big,
                    (),
                )?);
            }
            if <[u8; 20]>::from(hasher.finalize()) != disc.part_hash {
                return Err(WiaError::HashChainMismatch("partition table"));
            }
            out
        } else {
            Vec::new()
        };

        let props = &disc.compr_data[..disc.compr_data_len as usize];
        validate_lzma_dict(disc.compression, props)?;
        let mut codec = WiaCodec::new(disc.compression, props)?;

        crate::util::validate_extent(
            disc.raw_data_off,
            disc.raw_data_size as u64,
            file_len,
            "raw table",
        )?;
        let raw_decoded_size = (disc.n_raw_data as usize)
            .checked_mul(WIA_RAW_DATA_SIZE)
            .ok_or_else(|| WiaError::InvalidHeader("raw table size overflows".into()))?;
        let raw_bytes = codec.decode_table_reader(
            crate::util::positional_reader::PositionalReader::new(
                &*f,
                disc.raw_data_off,
                disc.raw_data_size as u64,
            ),
            disc.raw_data_size as u64,
            raw_decoded_size,
        )?;
        let mut raw_cursor = Cursor::new(&raw_bytes);
        // The declared row count is untrusted; the decoded table (and
        // with it this vector) is bounded by the table extent, so grow
        // as rows actually parse instead of reserving the full count.
        let mut raw_data = Vec::new();
        for _ in 0..disc.n_raw_data {
            raw_data.push(WiaRawData::read_options(&mut raw_cursor, Endian::Big, ())?);
        }
        drop(raw_bytes);

        crate::util::validate_extent(
            disc.group_off,
            disc.group_size as u64,
            file_len,
            "group table",
        )?;
        let group_decoded_size = (disc.n_groups as usize)
            .checked_mul(WIA_GROUP_SIZE)
            .ok_or_else(|| WiaError::InvalidHeader("group table size overflows".into()))?;
        let group_bytes = codec.decode_table_reader(
            crate::util::positional_reader::PositionalReader::new(
                &*f,
                disc.group_off,
                disc.group_size as u64,
            ),
            disc.group_size as u64,
            group_decoded_size,
        )?;
        let mut group_cursor = Cursor::new(&group_bytes);
        let mut groups = Vec::new();
        for _ in 0..disc.n_groups {
            groups.push(WiaGroup::read_options(&mut group_cursor, Endian::Big, ())?);
        }
        drop(group_bytes);
        Ok(Self {
            head,
            disc,
            parts,
            raw_data,
            groups,
            file_len,
        })
    }

    /// Total stored bytes across header, tables, and all groups; the
    /// progress denominator for a deep verify.
    pub(crate) fn stored_bytes(&self) -> u64 {
        WIA_FILE_HEAD_SIZE as u64
            + self.head.disc_size as u64
            + self.disc.n_part as u64 * self.disc.part_t_size as u64
            + self.disc.raw_data_size as u64
            + self.disc.group_size as u64
            + self.groups.iter().map(|g| g.data_size as u64).sum::<u64>()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SegmentKind {
    /// Served from the disc struct's embedded header bytes.
    Dhead {
        offset_in_dhead: usize,
    },
    Zero,
    RawGroup {
        group_index: u32,
        chunk_bytes: u32,
        slice_offset: u32,
    },
    PartGroup {
        group_index: u32,
        part_key: [u8; 16],
        n_sectors: u32,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Segment {
    pub logical_offset: u64,
    pub logical_size: u32,
    pub kind: SegmentKind,
}

/// Cap on a single zero-gap segment so no span materializes an
/// unbounded buffer.
const ZERO_SPAN_BYTES: u64 = 4 * 1024 * 1024;

/// Tile the logical ISO `[0, iso_size)` into contiguous segments.
pub(crate) fn build_segments(layout: &WiaLayout) -> WiaResult<Vec<Segment>> {
    let disc = &layout.disc;
    let chunk = disc.chunk_size as u64;
    let iso_size = layout.head.iso_file_size;
    let n_groups_total = layout.groups.len() as u64;
    let mut segs: Vec<Segment> = Vec::new();

    for region in &layout.raw_data {
        if region.raw_data_size == 0 {
            continue;
        }
        let effective_start = region.raw_data_off - region.raw_data_off % WII_SECTOR_SIZE_U64;
        let region_end = region.raw_data_off + region.raw_data_size;
        for i in 0..region.n_groups {
            let gi = region.group_index as u64 + i as u64;
            if gi >= n_groups_total {
                return Err(WiaError::InvalidHeader(
                    "raw group index out of range".into(),
                ));
            }
            let chunk_abs_start = effective_start + i as u64 * chunk;
            let chunk_abs_end = (chunk_abs_start + chunk).min(region_end);
            let write_start = chunk_abs_start.max(region.raw_data_off);
            let write_end = chunk_abs_end.min(iso_size);
            if write_start >= write_end {
                continue;
            }
            segs.push(Segment {
                logical_offset: write_start,
                logical_size: (write_end - write_start) as u32,
                kind: SegmentKind::RawGroup {
                    group_index: gi as u32,
                    chunk_bytes: (chunk_abs_end - chunk_abs_start) as u32,
                    slice_offset: (write_start - chunk_abs_start) as u32,
                },
            });
        }
    }

    for part in &layout.parts {
        let data_start_sector = part.pd[0].first_sector;
        let spc = (chunk / WII_SECTOR_SIZE_U64) as u32;
        for pd in &part.pd {
            if pd.n_sectors == 0 || pd.n_groups == 0 {
                continue;
            }
            if (pd.first_sector - data_start_sector) % WII_BLOCKS_PER_GROUP as u32 != 0 {
                return Err(WiaError::InvalidHeader(
                    "partition data entry is not aligned to the hash group size".into(),
                ));
            }
            for k in 0..pd.n_groups {
                let gi = pd.group_index as u64 + k as u64;
                if gi >= n_groups_total {
                    return Err(WiaError::InvalidHeader(
                        "partition group index out of range".into(),
                    ));
                }
                let sec0 = k * spc;
                if sec0 >= pd.n_sectors {
                    break;
                }
                let n_sectors = (pd.n_sectors - sec0).min(spc);
                segs.push(Segment {
                    logical_offset: (pd.first_sector as u64 + sec0 as u64) * WII_SECTOR_SIZE_U64,
                    logical_size: n_sectors * WII_SECTOR_SIZE as u32,
                    kind: SegmentKind::PartGroup {
                        group_index: gi as u32,
                        part_key: part.part_key,
                        n_sectors,
                    },
                });
            }
        }
    }

    segs.sort_by_key(|s| s.logical_offset);

    let mut tiled = Vec::with_capacity(segs.len() + 8);
    let mut pos = 0u64;
    for seg in segs {
        if seg.logical_offset < pos {
            return Err(WiaError::InvalidHeader(
                "raw and partition regions overlap".into(),
            ));
        }
        if seg.logical_offset > pos {
            fill_gap(&mut tiled, pos, seg.logical_offset, disc);
        }
        pos = seg.logical_offset + seg.logical_size as u64;
        tiled.push(seg);
    }
    if pos < iso_size {
        fill_gap(&mut tiled, pos, iso_size, disc);
    }
    Ok(tiled)
}

fn fill_gap(out: &mut Vec<Segment>, mut from: u64, to: u64, disc: &WiaDisc) {
    let dhead_len = disc.dhead.len() as u64;
    if from < dhead_len {
        let end = to.min(dhead_len);
        out.push(Segment {
            logical_offset: from,
            logical_size: (end - from) as u32,
            kind: SegmentKind::Dhead {
                offset_in_dhead: from as usize,
            },
        });
        from = end;
    }
    while from < to {
        let end = (from + ZERO_SPAN_BYTES).min(to);
        out.push(Segment {
            logical_offset: from,
            logical_size: (end - from) as u32,
            kind: SegmentKind::Zero,
        });
        from = end;
    }
}

pub(crate) enum WiaWorkKind {
    Zero,
    Bytes(Vec<u8>),
    RawGroup {
        stored: Vec<u8>,
        chunk_bytes: usize,
        slice_offset: usize,
    },
    PartGroup {
        stored: Vec<u8>,
        part_key: [u8; 16],
        n_sectors: usize,
        n_lists: usize,
    },
}

pub(crate) struct WiaSegmentWork {
    pub kind: WiaWorkKind,
    pub out_len: usize,
}

pub(crate) struct WiaSegmentWorker {
    codec: WiaCodec,
    payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>,
    hash_regions: Vec<[u8; HASH_REGION_BYTES]>,
    cluster_out: Vec<u8>,
}

impl WiaSegmentWorker {
    pub(crate) fn new(disc: &WiaDisc) -> WiaResult<Self> {
        Ok(Self {
            codec: WiaCodec::new(
                disc.compression,
                &disc.compr_data[..disc.compr_data_len as usize],
            )?,
            payloads: vec![[0u8; WII_SECTOR_PAYLOAD_SIZE]; WII_BLOCKS_PER_GROUP],
            hash_regions: vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP],
            cluster_out: vec![0u8; WII_BLOCKS_PER_GROUP * WII_SECTOR_SIZE],
        })
    }
}

impl Worker<WiaSegmentWork, Vec<u8>, WiaError> for WiaSegmentWorker {
    fn process(&mut self, work: WiaSegmentWork) -> WiaResult<Vec<u8>> {
        match work.kind {
            WiaWorkKind::Zero => Ok(vec![0u8; work.out_len]),
            WiaWorkKind::Bytes(b) => Ok(b),
            WiaWorkKind::RawGroup {
                stored,
                chunk_bytes,
                slice_offset,
            } => {
                if stored.is_empty() {
                    return Ok(vec![0u8; work.out_len]);
                }
                let (_, payload) = self.codec.decode_group(&stored, 0, chunk_bytes)?;
                Ok(payload[slice_offset..slice_offset + work.out_len].to_vec())
            }
            WiaWorkKind::PartGroup {
                stored,
                part_key,
                n_sectors,
                n_lists,
            } => {
                let payload_expected = n_sectors * WII_SECTOR_PAYLOAD_SIZE;
                let (lists, payload) = if stored.is_empty() {
                    (vec![Vec::new(); n_lists], vec![0u8; payload_expected])
                } else {
                    self.codec
                        .decode_group(&stored, n_lists, payload_expected)?
                };

                let mut out = Vec::with_capacity(work.out_len);
                let clusters = n_sectors.div_ceil(WII_BLOCKS_PER_GROUP);
                for c in 0..clusters {
                    let sec0 = c * WII_BLOCKS_PER_GROUP;
                    let sec_count = (n_sectors - sec0).min(WII_BLOCKS_PER_GROUP);
                    for s in 0..WII_BLOCKS_PER_GROUP {
                        if s < sec_count {
                            let p = (sec0 + s) * WII_SECTOR_PAYLOAD_SIZE;
                            self.payloads[s]
                                .copy_from_slice(&payload[p..p + WII_SECTOR_PAYLOAD_SIZE]);
                        } else {
                            self.payloads[s] = [0u8; WII_SECTOR_PAYLOAD_SIZE];
                        }
                    }
                    recompute_hash_regions_into(&self.payloads, &mut self.hash_regions);
                    if let Some(list) = lists.get(c) {
                        apply_hash_exceptions(&mut self.hash_regions, list);
                    }
                    reencrypt_cluster_into(
                        &self.hash_regions,
                        &self.payloads,
                        &part_key,
                        &mut self.cluster_out,
                    )?;
                    out.extend_from_slice(&self.cluster_out[..sec_count * WII_SECTOR_SIZE]);
                }
                Ok(out)
            }
        }
    }
}

/// Build the work item for one segment; runs on the reader thread so
/// file access stays sequential.
pub(crate) fn read_segment_work(
    f: &mut File,
    file_len: u64,
    seg: &Segment,
    layout_groups: &[WiaGroup],
    dhead: &[u8; 128],
    n_lists: usize,
) -> WiaResult<WiaSegmentWork> {
    let out_len = seg.logical_size as usize;
    let kind = match seg.kind {
        SegmentKind::Zero => WiaWorkKind::Zero,
        SegmentKind::Dhead { offset_in_dhead } => {
            WiaWorkKind::Bytes(dhead[offset_in_dhead..offset_in_dhead + out_len].to_vec())
        }
        SegmentKind::RawGroup {
            group_index,
            chunk_bytes,
            slice_offset,
        } => WiaWorkKind::RawGroup {
            stored: read_group_stored(f, &layout_groups[group_index as usize], file_len)?,
            chunk_bytes: chunk_bytes as usize,
            slice_offset: slice_offset as usize,
        },
        SegmentKind::PartGroup {
            group_index,
            part_key,
            n_sectors,
        } => WiaWorkKind::PartGroup {
            stored: read_group_stored(f, &layout_groups[group_index as usize], file_len)?,
            part_key,
            n_sectors: n_sectors as usize,
            n_lists,
        },
    };
    Ok(WiaSegmentWork { kind, out_len })
}

fn read_group_stored(f: &mut File, group: &WiaGroup, file_len: u64) -> WiaResult<Vec<u8>> {
    if group.data_size == 0 {
        return Ok(Vec::new());
    }
    crate::util::validate_extent(
        group.data_offset(),
        group.data_size as u64,
        file_len,
        "group",
    )?;
    let mut stored = vec![0u8; group.data_size as usize];
    f.seek(SeekFrom::Start(group.data_offset()))?;
    f.read_exact(&mut stored)?;
    Ok(stored)
}

type ProduceFn = Box<dyn FnMut(u64) -> WiaResult<WiaSegmentWork> + Send>;

/// `Read + Seek` view over a WIA container's decoded data.
pub struct WiaReader {
    pipeline: PipelinedGroupReader<WiaSegmentWork, WiaError, ProduceFn>,
    iso_size: u64,
}

impl WiaReader {
    /// Opens the WIA file at `path` and parses its metadata tables.
    pub fn open(path: &Path) -> WiaResult<Self> {
        Self::open_with_lookahead(path, usize::MAX)
    }

    /// Same as [`Self::open`], with `lookahead` as the pipeline prefetch
    /// bound in groups; `usize::MAX` means unbounded.
    pub fn open_with_lookahead(path: &Path, lookahead: usize) -> WiaResult<Self> {
        let mut f = File::open(path)?;
        let layout = WiaLayout::parse(&mut f)?;
        let segments = build_segments(&layout)?;
        let iso_size = layout.head.iso_file_size;
        let n_lists = exception_lists_per_group(layout.disc.chunk_size);

        let spans: Vec<GroupSpan> = segments
            .iter()
            .map(|s| GroupSpan {
                logical_offset: s.logical_offset,
                logical_size: s.logical_size,
            })
            .collect();

        let max_segment = segments
            .iter()
            .map(|s| s.logical_size as u64)
            .max()
            .unwrap_or(1);
        let cap = in_flight_cap(max_segment);
        let workers: WiaResult<Vec<WiaSegmentWorker>> =
            (0..parallelism().min(cap.max(2)).min(lookahead.max(2)))
                .map(|_| WiaSegmentWorker::new(&layout.disc))
                .collect();

        let dhead = layout.disc.dhead;
        let groups = layout.groups;
        let file_len = layout.file_len;
        let produce: ProduceFn = Box::new(move |i| {
            read_segment_work(
                &mut f,
                file_len,
                &segments[i as usize],
                &groups,
                &dhead,
                n_lists,
            )
        });

        Ok(Self {
            pipeline: PipelinedGroupReader::with_lookahead(
                workers?, spans, cap, lookahead, produce,
            ),
            iso_size,
        })
    }

    /// Size of the logical (decoded) disc image in bytes.
    pub fn iso_size(&self) -> u64 {
        self.iso_size
    }

    /// Header-only logical size, for progress totals.
    pub fn iso_size_of(path: &Path) -> WiaResult<u64> {
        let mut f = File::open(path)?;
        let mut head_bytes = [0u8; WIA_FILE_HEAD_SIZE];
        f.read_exact(&mut head_bytes)?;
        let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
        if head.magic != WIA_MAGIC {
            return Err(WiaError::InvalidMagic(head.magic));
        }
        Ok(head.iso_file_size)
    }
}

impl Read for WiaReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.pipeline.read(buf)
    }
}

impl Seek for WiaReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pipeline.seek(from)
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::{MAX_LZMA_DICT_BYTES, validate_lzma_dict};
    use crate::nintendo::disc::wia::format::{WIA_COMPR_LZMA, WIA_COMPR_LZMA2, WIA_COMPR_NONE};
    use crate::util::validate_extent;

    #[test]
    fn table_and_group_extents_reject_overflow_and_truncation() {
        assert!(validate_extent(0x100, 0x200, 0x300, "fixture").is_ok());
        assert!(validate_extent(0x100, 0x201, 0x300, "fixture").is_err());
        assert!(validate_extent(u64::MAX - 1, 4, u64::MAX, "fixture").is_err());
    }

    #[test]
    fn lzma_dictionaries_over_256_mib_are_rejected() {
        let mut props = vec![0u8; 5];
        props[1..5].copy_from_slice(&MAX_LZMA_DICT_BYTES.to_le_bytes());
        assert!(validate_lzma_dict(WIA_COMPR_LZMA, &props).is_ok());
        props[1..5].copy_from_slice(&(MAX_LZMA_DICT_BYTES + 1).to_le_bytes());
        assert!(validate_lzma_dict(WIA_COMPR_LZMA, &props).is_err());

        // LZMA2 packs the dictionary into one prop byte; values from
        // 40 on mean u32::MAX, and 39 means 3 GiB.
        assert!(validate_lzma_dict(WIA_COMPR_LZMA2, &[39]).is_err());
        assert!(validate_lzma_dict(WIA_COMPR_LZMA2, &[0xFF]).is_err());
        assert!(validate_lzma_dict(WIA_COMPR_LZMA2, &[0x14]).is_ok());

        // Other codecs carry no dictionary.
        assert!(validate_lzma_dict(WIA_COMPR_NONE, &[]).is_ok());
    }
}
