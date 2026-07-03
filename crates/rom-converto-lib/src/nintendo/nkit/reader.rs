//! `Read + Seek` view over a native NKit image that restores the
//! original disc on the fly.
//!
//! Opening runs an index pass: walk the nkit FST in data order,
//! decode every gap record, and build a span plan mapping the
//! restored image to its sources (patched header bytes, verbatim file
//! data, regenerated junk, zero or byte fill, and for Wii whole
//! re-hashed and re-encrypted partition groups). Span materialization
//! runs on the shared worker pool through [`PipelinedGroupReader`].
//!
//! Reads are teed through a positional CRC32; when the last restored
//! byte is produced, the running value must equal the source CRC the
//! NKit header stores, proving the restoration byte-exact without an
//! extra pass.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use log::warn;

use super::crc::Crc32;
use super::error::{NkitError, NkitResult};
use super::format::{
    FstFile, GC_FST_OFFSET_FIELD, GC_FST_SIZE_FIELD, NkitHeader, clear_nkit_header, parse_gc_fst,
};
use super::gaps::{GapPiece, parse_gap_record, parse_junk_file_record, peek_record_type};
use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE,
};
use crate::nintendo::rvl::partition::{
    HASH_REGION_BYTES, recompute_hash_regions_into, reencrypt_cluster_into,
};
use crate::nintendo::rvz::packing::LaggedFibonacci;
use crate::util::group_reader::{GroupSpan, PipelinedGroupReader, in_flight_cap};
use crate::util::worker_pool::{Worker, parallelism};

/// Spans are split to this size so no group materializes an unbounded
/// buffer (junk gaps can span hundreds of MiB).
const MAX_SPAN_BYTES: u64 = 4 * 1024 * 1024;

/// Junk gaps begin with up to this many zero bytes after a file end
/// (`nullsPos = dstPos + 0x1c` in NKit's readers).
pub(crate) const NULLS_LEAD: u64 = 0x1C;
/// Junk runs at least this large skip the leading NULs unless they
/// follow the FST or trail the last file.
pub(crate) const JUNK_NULLS_SKIP: u64 = 0x40000;

/// One piece of a Wii partition group's hashless payload.
#[derive(Debug, Clone)]
pub(crate) enum WiiPiece {
    Patched(Arc<Vec<u8>>, u64),
    /// Copied from the nkit stream at this offset.
    Verbatim(u64),
    Zeros,
    Fill(u8),
    /// Junk at this hashless partition-data position.
    Junk {
        junk_pos: u64,
    },
}

/// Everything a worker needs to rebuild one 2 MiB Wii partition group:
/// payload pieces, optional preserved hash sectors, and the title key.
#[derive(Debug, Clone)]
pub(crate) struct WiiGroupSpec {
    pub pieces: Vec<(u64, WiiPiece)>,
    pub preserved_src: Option<u64>,
    pub title_key: [u8; 16],
    pub blocks: usize,
    pub junk_id: [u8; 4],
    pub disc_num: u8,
}

#[derive(Debug, Clone)]
pub(crate) enum SpanKind {
    /// Patched in-memory bytes (headers, restored FSTs). The u64 is
    /// the offset into the buffer.
    Patched(Arc<Vec<u8>>, u64),
    /// Copied from the nkit stream at this offset.
    Verbatim(u64),
    Zeros,
    Fill(u8),
    /// Junk regenerated with this identity at this junk-stream
    /// position (output offset for GC, hashless data offset inside
    /// Wii partitions).
    Junk {
        id: [u8; 4],
        disc: u8,
        junk_pos: u64,
    },
    /// A Wii partition group rebuilt from hashless pieces.
    WiiGroup(Box<WiiGroupSpec>),
}

#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub out_off: u64,
    pub len: u64,
    pub kind: SpanKind,
}

/// Index-pass result.
pub(crate) struct NkitPlan {
    pub spans: Vec<Span>,
    pub image_size: u64,
    pub source_crc: u32,
    /// False when the restoration is known to be playable-only (a
    /// removed update partition without recovery data).
    pub crc_enforce: bool,
    pub warning: Option<String>,
}

pub(crate) fn read_exact_at<S: Read + Seek>(
    src: &mut S,
    off: u64,
    buf: &mut [u8],
) -> NkitResult<()> {
    src.seek(SeekFrom::Start(off))?;
    src.read_exact(buf)?;
    Ok(())
}

pub(crate) fn emit(spans: &mut Vec<Span>, out_off: u64, len: u64, kind: SpanKind) {
    if len > 0 {
        spans.push(Span { out_off, len, kind });
    }
}

/// Decode one gap record into spans, applying the leading-NUL rule
/// from NKit's `writeGap`: a run of zeroes (up to the tracked
/// `nulls_pos`) precedes junk output, skipped for large mid-image
/// gaps.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_gap_record<S: Read + Seek>(
    spans: &mut Vec<Span>,
    src: &mut S,
    nkit_pos: &mut u64,
    out_pos: &mut u64,
    nulls_pos: &mut u64,
    first_or_last: bool,
    junk_id: [u8; 4],
    disc_num: u8,
) -> NkitResult<()> {
    let rec = parse_gap_record(src, *nkit_pos)?;
    *nkit_pos += rec.consumed;
    let size = rec.out_len;

    let max_nulls = nulls_pos.saturating_sub(*out_pos);
    let lead = if size < max_nulls {
        size
    } else if size >= JUNK_NULLS_SKIP && !first_or_last {
        0
    } else {
        max_nulls
    };
    *nulls_pos = *out_pos + lead;

    let junk = |junk_pos: u64| SpanKind::Junk {
        id: junk_id,
        disc: disc_num,
        junk_pos,
    };

    if !rec.mixed {
        for piece in &rec.pieces {
            match *piece {
                GapPiece::Junk { len } => {
                    emit(spans, *out_pos, lead, SpanKind::Zeros);
                    emit(spans, *out_pos + lead, len - lead, junk(*out_pos + lead));
                }
                GapPiece::Zeros { len } => emit(spans, *out_pos, len, SpanKind::Zeros),
                _ => unreachable!("plain gap records expand to one junk or zero piece"),
            }
            *out_pos += piece.len();
        }
        return Ok(());
    }

    let mut prg = size;
    for piece in &rec.pieces {
        let len = piece.len();
        match *piece {
            GapPiece::Junk { .. } => {
                let max_nulls = nulls_pos.saturating_sub(*out_pos);
                let nulls = if prg < max_nulls {
                    len
                } else if len >= JUNK_NULLS_SKIP && !first_or_last {
                    0
                } else {
                    max_nulls
                };
                emit(spans, *out_pos, nulls, SpanKind::Zeros);
                emit(spans, *out_pos + nulls, len - nulls, junk(*out_pos + nulls));
            }
            GapPiece::Zeros { .. } => emit(spans, *out_pos, len, SpanKind::Zeros),
            GapPiece::ByteFill { byte, .. } => emit(spans, *out_pos, len, SpanKind::Fill(byte)),
            GapPiece::Verbatim { nkit_off, .. } => {
                emit(spans, *out_pos, len, SpanKind::Verbatim(nkit_off))
            }
        }
        *out_pos += len;
        prg -= len;
    }
    Ok(())
}

fn build_plan<S: Read + Seek>(src: &mut S) -> NkitResult<NkitPlan> {
    let nkit_len = src.seek(SeekFrom::End(0))?;

    let mut dhead = vec![0u8; 0x440];
    read_exact_at(src, 0, &mut dhead)?;
    let header = NkitHeader::parse(&dhead)?;
    if header.is_wii {
        return super::wii::build_wii_plan(src, &header);
    }
    let (junk_id, disc_num) = header.junk_identity(&dhead);
    let image_size = header.image_size;

    let fst_off = u32::from_be_bytes(
        dhead[GC_FST_OFFSET_FIELD..GC_FST_OFFSET_FIELD + 4]
            .try_into()
            .unwrap(),
    ) as u64;
    let fst_size = u32::from_be_bytes(
        dhead[GC_FST_SIZE_FIELD..GC_FST_SIZE_FIELD + 4]
            .try_into()
            .unwrap(),
    ) as u64;
    if fst_off < 0x440 || fst_off + fst_size > nkit_len {
        return Err(NkitError::InvalidHeader(format!(
            "FST {fst_off:#x}+{fst_size:#x} outside the image"
        )));
    }
    let mut fst = vec![0u8; fst_size as usize];
    read_exact_at(src, fst_off, &mut fst)?;
    let mut files = parse_gc_fst(&fst)?;
    files.sort_by_key(|f| f.data_offset);

    clear_nkit_header(&mut dhead);

    let mut spans: Vec<Span> = Vec::new();
    let walk_start = (fst_off + fst_size + 3) & !3;
    let mut nkit_pos = walk_start;
    let mut out_pos = walk_start;
    let mut nulls_pos = walk_start + NULLS_LEAD;

    for (i, file) in files.iter().enumerate() {
        let target = file.data_offset;
        if target < nkit_pos {
            return Err(NkitError::InvalidHeader(format!(
                "FST file {i} at {target:#x} overlaps the previous file"
            )));
        }
        if nkit_pos < target {
            expand_gap_record(
                &mut spans,
                src,
                &mut nkit_pos,
                &mut out_pos,
                &mut nulls_pos,
                i == 0,
                junk_id,
                disc_num,
            )?;
            if nkit_pos > target {
                return Err(NkitError::InvalidGap(
                    "gap record extends past the next file".into(),
                ));
            }
            // Bytes up to the file are alignment padding NKit added.
            nkit_pos = target;
        }

        if file.size == 0 && nkit_pos + 8 <= nkit_len && peek_record_type(src, nkit_pos)? == 3 {
            let rec = parse_junk_file_record(src, nkit_pos)?;
            nkit_pos += rec.consumed;
            let restored_len = (rec.file_len + 3) & !3;
            emit(&mut spans, out_pos, rec.leading_nulls, SpanKind::Zeros);
            emit(
                &mut spans,
                out_pos + rec.leading_nulls,
                restored_len - rec.leading_nulls,
                SpanKind::Junk {
                    id: junk_id,
                    disc: disc_num,
                    junk_pos: out_pos + rec.leading_nulls,
                },
            );
            patch_fst_entry(&mut fst, file, out_pos, rec.file_len as u32);
            out_pos += restored_len;
            // The gap after a junk file carries no leading NULs.
            nulls_pos = 0;
        } else {
            let copy_len = ((file.size as u64) + 3) & !3;
            emit(&mut spans, out_pos, copy_len, SpanKind::Verbatim(nkit_pos));
            patch_fst_entry(&mut fst, file, out_pos, file.size);
            nkit_pos += copy_len;
            out_pos += copy_len;
            nulls_pos = out_pos + NULLS_LEAD;
        }
    }
    if nkit_pos < nkit_len {
        expand_gap_record(
            &mut spans,
            src,
            &mut nkit_pos,
            &mut out_pos,
            &mut nulls_pos,
            true,
            junk_id,
            disc_num,
        )?;
    }
    if out_pos != image_size {
        return Err(NkitError::InvalidHeader(format!(
            "restored image is {out_pos:#x} bytes but the header declares {:#x}",
            image_size
        )));
    }

    // The region before the first gap: patched Boot.bin, verbatim
    // apploader and DOL, restored FST.
    let patched_boot = Arc::new(dhead);
    let patched_fst = Arc::new(fst);
    let mut head_spans = vec![
        Span {
            out_off: 0,
            len: 0x440,
            kind: SpanKind::Patched(patched_boot, 0),
        },
        Span {
            out_off: 0x440,
            len: fst_off - 0x440,
            kind: SpanKind::Verbatim(0x440),
        },
        Span {
            out_off: fst_off,
            len: fst_size,
            kind: SpanKind::Patched(patched_fst, 0),
        },
        Span {
            out_off: fst_off + fst_size,
            len: walk_start - (fst_off + fst_size),
            kind: SpanKind::Verbatim(fst_off + fst_size),
        },
    ];
    head_spans.retain(|s| s.len > 0);
    head_spans.extend(spans);

    Ok(NkitPlan {
        spans: head_spans,
        image_size,
        source_crc: header.source_crc,
        crc_enforce: true,
        warning: None,
    })
}

fn patch_fst_entry(fst: &mut [u8], file: &FstFile, out_off: u64, size: u32) {
    fst[file.entry_offset + 4..file.entry_offset + 8]
        .copy_from_slice(&(out_off as u32).to_be_bytes());
    fst[file.entry_offset + 8..file.entry_offset + 12].copy_from_slice(&size.to_be_bytes());
}

/// Split spans to [`MAX_SPAN_BYTES`] so the pipeline's buffers stay
/// bounded regardless of gap sizes. Wii groups are already capped at
/// 2 MiB and pass through whole.
fn split_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        if matches!(span.kind, SpanKind::WiiGroup(_)) || span.len <= MAX_SPAN_BYTES {
            out.push(span);
            continue;
        }
        let mut done = 0u64;
        while done < span.len {
            let take = (span.len - done).min(MAX_SPAN_BYTES);
            let kind = match &span.kind {
                SpanKind::Patched(buf, off) => SpanKind::Patched(buf.clone(), off + done),
                SpanKind::Verbatim(off) => SpanKind::Verbatim(off + done),
                SpanKind::Junk { id, disc, junk_pos } => SpanKind::Junk {
                    id: *id,
                    disc: *disc,
                    junk_pos: junk_pos + done,
                },
                other => other.clone(),
            };
            out.push(Span {
                out_off: span.out_off + done,
                len: take,
                kind,
            });
            done += take;
        }
    }
    out
}

pub(crate) struct NkitSpanWork {
    kind: WorkKind,
    out_len: usize,
}

enum WorkKind {
    Bytes(Vec<u8>),
    Zeros,
    Fill(u8),
    Junk {
        id: [u8; 4],
        disc: u8,
        junk_pos: u64,
    },
    WiiGroup {
        pieces: Vec<(u64, WiiWorkPiece)>,
        preserved: Option<Vec<u8>>,
        title_key: [u8; 16],
        blocks: usize,
        junk_id: [u8; 4],
        disc_num: u8,
    },
}

enum WiiWorkPiece {
    Bytes(Vec<u8>),
    Zeros,
    Fill(u8),
    Junk { junk_pos: u64 },
}

pub(crate) struct NkitSpanWorker {
    payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>,
    hash_regions: Vec<[u8; HASH_REGION_BYTES]>,
    cluster_out: Vec<u8>,
}

impl NkitSpanWorker {
    fn new() -> Self {
        Self {
            payloads: vec![[0u8; WII_SECTOR_PAYLOAD_SIZE]; WII_BLOCKS_PER_GROUP],
            hash_regions: vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP],
            cluster_out: vec![0u8; WII_BLOCKS_PER_GROUP * WII_SECTOR_SIZE],
        }
    }
}

impl Worker<NkitSpanWork, Vec<u8>, NkitError> for NkitSpanWorker {
    fn process(&mut self, work: NkitSpanWork) -> NkitResult<Vec<u8>> {
        Ok(match work.kind {
            WorkKind::Bytes(b) => b,
            WorkKind::Zeros => vec![0u8; work.out_len],
            WorkKind::Fill(byte) => vec![byte; work.out_len],
            WorkKind::Junk { id, disc, junk_pos } => {
                let mut buf = vec![0u8; work.out_len];
                LaggedFibonacci::fill_junk(&id, disc, junk_pos, &mut buf);
                buf
            }
            WorkKind::WiiGroup {
                pieces,
                preserved,
                title_key,
                blocks,
                junk_id,
                disc_num,
            } => {
                let mut data = Vec::with_capacity(blocks * WII_SECTOR_PAYLOAD_SIZE);
                for (len, piece) in &pieces {
                    let len = *len as usize;
                    match piece {
                        WiiWorkPiece::Bytes(b) => data.extend_from_slice(b),
                        WiiWorkPiece::Zeros => data.resize(data.len() + len, 0),
                        WiiWorkPiece::Fill(b) => data.resize(data.len() + len, *b),
                        WiiWorkPiece::Junk { junk_pos } => {
                            let start = data.len();
                            data.resize(start + len, 0);
                            LaggedFibonacci::fill_junk(
                                &junk_id,
                                disc_num,
                                *junk_pos,
                                &mut data[start..],
                            );
                        }
                    }
                }
                if data.len() != blocks * WII_SECTOR_PAYLOAD_SIZE {
                    return Err(NkitError::Custom(format!(
                        "partition group assembled {} bytes, expected {}",
                        data.len(),
                        blocks * WII_SECTOR_PAYLOAD_SIZE
                    )));
                }
                for s in 0..WII_BLOCKS_PER_GROUP {
                    if s < blocks {
                        self.payloads[s].copy_from_slice(
                            &data[s * WII_SECTOR_PAYLOAD_SIZE..(s + 1) * WII_SECTOR_PAYLOAD_SIZE],
                        );
                    } else {
                        self.payloads[s] = [0u8; WII_SECTOR_PAYLOAD_SIZE];
                    }
                }
                match &preserved {
                    Some(h) => {
                        for (b, region) in self.hash_regions.iter_mut().enumerate() {
                            if b < blocks {
                                region.copy_from_slice(
                                    &h[b * HASH_REGION_BYTES..(b + 1) * HASH_REGION_BYTES],
                                );
                            } else {
                                *region = [0u8; HASH_REGION_BYTES];
                            }
                        }
                    }
                    None => {
                        recompute_hash_regions_into(&self.payloads, &mut self.hash_regions);
                    }
                }
                reencrypt_cluster_into(
                    &self.hash_regions,
                    &self.payloads,
                    &title_key,
                    &mut self.cluster_out,
                )
                .map_err(|e| NkitError::Custom(format!("partition re-encrypt: {e}")))?;
                self.cluster_out[..blocks * WII_SECTOR_SIZE].to_vec()
            }
        })
    }
}

type ProduceFn = Box<dyn FnMut(u64) -> NkitResult<NkitSpanWork> + Send>;

pub struct NkitReader {
    pipeline: PipelinedGroupReader<NkitSpanWork, NkitError, ProduceFn>,
    image_size: u64,
    source_crc: u32,
    crc: Crc32,
    crc_pos: u64,
    crc_complete: bool,
    crc_checked: bool,
    crc_enforce: bool,
    warning: Option<String>,
    pos: u64,
}

impl NkitReader {
    pub fn open(path: &Path) -> NkitResult<Self> {
        Self::from_source(File::open(path)?)
    }

    /// Build a reader over any seekable source, allowing the GCZ
    /// wrapper (`.nkit.gcz`) to layer underneath.
    pub fn from_source<S: Read + Seek + Send + 'static>(mut src: S) -> NkitResult<Self> {
        let plan = build_plan(&mut src)?;
        if let Some(w) = &plan.warning {
            warn!("{w}");
        }

        let plan_spans = split_spans(plan.spans);
        let spans: Vec<GroupSpan> = plan_spans
            .iter()
            .map(|s| GroupSpan {
                logical_offset: s.out_off,
                logical_size: s.len as u32,
            })
            .collect();
        let cap = in_flight_cap(MAX_SPAN_BYTES);
        let workers: Vec<NkitSpanWorker> = (0..parallelism().min(cap.max(2)))
            .map(|_| NkitSpanWorker::new())
            .collect();

        let produce: ProduceFn = Box::new(move |i| {
            let span = &plan_spans[i as usize];
            let kind = match &span.kind {
                SpanKind::Patched(buf, off) => {
                    WorkKind::Bytes(buf[*off as usize..(*off + span.len) as usize].to_vec())
                }
                SpanKind::Verbatim(nkit_off) => {
                    let mut bytes = vec![0u8; span.len as usize];
                    read_exact_at(&mut src, *nkit_off, &mut bytes)?;
                    WorkKind::Bytes(bytes)
                }
                SpanKind::Zeros => WorkKind::Zeros,
                SpanKind::Fill(byte) => WorkKind::Fill(*byte),
                SpanKind::Junk { id, disc, junk_pos } => WorkKind::Junk {
                    id: *id,
                    disc: *disc,
                    junk_pos: *junk_pos,
                },
                SpanKind::WiiGroup(spec) => {
                    let mut pieces = Vec::with_capacity(spec.pieces.len());
                    for (len, piece) in &spec.pieces {
                        let work_piece = match piece {
                            WiiPiece::Patched(buf, off) => WiiWorkPiece::Bytes(
                                buf[*off as usize..(*off + len) as usize].to_vec(),
                            ),
                            WiiPiece::Verbatim(src_off) => {
                                let mut bytes = vec![0u8; *len as usize];
                                read_exact_at(&mut src, *src_off, &mut bytes)?;
                                WiiWorkPiece::Bytes(bytes)
                            }
                            WiiPiece::Zeros => WiiWorkPiece::Zeros,
                            WiiPiece::Fill(b) => WiiWorkPiece::Fill(*b),
                            WiiPiece::Junk { junk_pos } => WiiWorkPiece::Junk {
                                junk_pos: *junk_pos,
                            },
                        };
                        pieces.push((*len, work_piece));
                    }
                    let preserved = match spec.preserved_src {
                        Some(off) => {
                            let mut h = vec![0u8; spec.blocks * HASH_REGION_BYTES];
                            read_exact_at(&mut src, off, &mut h)?;
                            Some(h)
                        }
                        None => None,
                    };
                    WorkKind::WiiGroup {
                        pieces,
                        preserved,
                        title_key: spec.title_key,
                        blocks: spec.blocks,
                        junk_id: spec.junk_id,
                        disc_num: spec.disc_num,
                    }
                }
            };
            Ok(NkitSpanWork {
                kind,
                out_len: span.len as usize,
            })
        });

        Ok(Self {
            pipeline: PipelinedGroupReader::new(workers, spans, cap, produce),
            image_size: plan.image_size,
            source_crc: plan.source_crc,
            crc: Crc32::new(),
            crc_pos: 0,
            crc_complete: true,
            crc_checked: false,
            crc_enforce: plan.crc_enforce,
            warning: plan.warning,
            pos: 0,
        })
    }

    pub fn image_size(&self) -> u64 {
        self.image_size
    }

    /// A restoration caveat (removed update partition), if any.
    pub fn restorable_warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// Header-only logical size, for progress totals.
    pub fn image_size_of(path: &Path) -> NkitResult<u64> {
        let mut f = File::open(path)?;
        let mut dhead = vec![0u8; 0x440];
        read_exact_at(&mut f, 0, &mut dhead)?;
        Ok(NkitHeader::parse(&dhead)?.image_size)
    }
}

impl Read for NkitReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.pipeline.read(buf)?;
        let read_start = self.pos;
        self.pos += n as u64;

        // Positional CRC tee: hash only contiguous forward progress
        // so header probing and re-reads never double-count.
        if read_start <= self.crc_pos && self.pos > self.crc_pos {
            let skip = (self.crc_pos - read_start) as usize;
            self.crc.update(&buf[skip..n]);
            self.crc_pos = self.pos;
        } else if read_start > self.crc_pos {
            self.crc_complete = false;
        }
        if self.crc_complete && !self.crc_checked && self.crc_pos == self.image_size {
            self.crc_checked = true;
            if self.crc_enforce && self.crc.value() != self.source_crc {
                return Err(io::Error::other(NkitError::CrcMismatch {
                    what: "the restored image",
                    stored: self.source_crc,
                    computed: self.crc.value(),
                }));
            }
        }
        Ok(n)
    }
}

impl Seek for NkitReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = self.pipeline.seek(from)?;
        Ok(self.pos)
    }
}
