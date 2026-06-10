//! `Read + Seek` view over a native NKit GameCube image that restores
//! the original disc on the fly.
//!
//! Opening runs an index pass: walk the nkit FST in data order,
//! decode every gap record, and build a span plan mapping the
//! restored image to its sources (patched header bytes, verbatim file
//! data, regenerated junk, zero or byte fill). Junk regeneration runs
//! on the shared worker pool through [`PipelinedGroupReader`].
//!
//! Reads are teed through a positional CRC-32; when the last restored
//! byte is produced, the running value must equal the source CRC the
//! NKit header stores, proving the restoration byte-exact without an
//! extra pass.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use super::crc::Crc32;
use super::error::{NkitError, NkitResult};
use super::format::{
    FstFile, GC_FST_OFFSET_FIELD, GC_FST_SIZE_FIELD, NkitHeader, clear_nkit_header, parse_gc_fst,
};
use super::gaps::{GapPiece, parse_gap_record, parse_junk_file_record, peek_record_type};
use crate::nintendo::rvz::packing::LaggedFibonacci;
use crate::util::group_reader::{GroupSpan, PipelinedGroupReader, in_flight_cap};
use crate::util::worker_pool::{Worker, parallelism};

/// Spans are split to this size so no group materializes an unbounded
/// buffer (junk gaps can span hundreds of MiB).
const MAX_SPAN_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
enum SpanKind {
    /// Patched in-memory bytes (Boot.bin with the NKit fields cleared,
    /// the restored FST). The u64 is the offset into the buffer.
    Patched(Arc<Vec<u8>>, u64),
    /// Copied from the nkit stream at this offset.
    Verbatim(u64),
    Zeros,
    Fill(u8),
    /// Junk regenerated at the span's output offset.
    Junk,
}

#[derive(Debug, Clone)]
struct Span {
    out_off: u64,
    len: u64,
    kind: SpanKind,
}

/// Index-pass result: the span plan plus the junk identity.
struct NkitPlan {
    spans: Vec<Span>,
    image_size: u64,
    source_crc: u32,
    junk_id: [u8; 4],
    disc_num: u8,
}

fn read_exact_at<S: Read + Seek>(src: &mut S, off: u64, buf: &mut [u8]) -> NkitResult<()> {
    src.seek(SeekFrom::Start(off))?;
    src.read_exact(buf)?;
    Ok(())
}

fn build_plan<S: Read + Seek>(src: &mut S) -> NkitResult<NkitPlan> {
    let nkit_len = src.seek(SeekFrom::End(0))?;

    let mut dhead = vec![0u8; 0x440];
    read_exact_at(src, 0, &mut dhead)?;
    let header = NkitHeader::parse(&dhead)?;
    if header.is_wii {
        return Err(NkitError::WiiUnsupported);
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
            "FST {fst_off:#X}+{fst_size:#X} outside the image"
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

    let emit = |spans: &mut Vec<Span>, out_off: u64, len: u64, kind: SpanKind| {
        if len > 0 {
            spans.push(Span { out_off, len, kind });
        }
    };

    let expand_gap = |spans: &mut Vec<Span>,
                      src: &mut S,
                      nkit_pos: &mut u64,
                      out_pos: &mut u64|
     -> NkitResult<()> {
        let rec = parse_gap_record(src, *nkit_pos)?;
        *nkit_pos += rec.consumed;
        for piece in &rec.pieces {
            let kind = match *piece {
                GapPiece::Junk { .. } => SpanKind::Junk,
                GapPiece::Zeros { .. } => SpanKind::Zeros,
                GapPiece::ByteFill { byte, .. } => SpanKind::Fill(byte),
                GapPiece::Verbatim { nkit_off, .. } => SpanKind::Verbatim(nkit_off),
            };
            emit(spans, *out_pos, piece.len(), kind);
            *out_pos += piece.len();
        }
        Ok(())
    };

    for (i, file) in files.iter().enumerate() {
        let target = file.data_offset;
        if target < nkit_pos {
            return Err(NkitError::InvalidHeader(format!(
                "FST file {i} at {target:#X} overlaps the previous file"
            )));
        }
        if nkit_pos < target {
            expand_gap(&mut spans, src, &mut nkit_pos, &mut out_pos)?;
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
                SpanKind::Junk,
            );
            patch_fst_entry(&mut fst, file, out_pos, rec.file_len as u32);
            out_pos += restored_len;
        } else {
            let copy_len = ((file.size as u64) + 3) & !3;
            emit(&mut spans, out_pos, copy_len, SpanKind::Verbatim(nkit_pos));
            patch_fst_entry(&mut fst, file, out_pos, file.size);
            nkit_pos += copy_len;
            out_pos += copy_len;
        }
    }
    if nkit_pos < nkit_len {
        expand_gap(&mut spans, src, &mut nkit_pos, &mut out_pos)?;
    }
    if out_pos != image_size {
        return Err(NkitError::InvalidHeader(format!(
            "restored image is {out_pos:#X} bytes but the header declares {:#X}",
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
        spans: split_spans(head_spans),
        image_size,
        source_crc: header.source_crc,
        junk_id,
        disc_num,
    })
}

fn patch_fst_entry(fst: &mut [u8], file: &FstFile, out_off: u64, size: u32) {
    fst[file.entry_offset + 4..file.entry_offset + 8]
        .copy_from_slice(&(out_off as u32).to_be_bytes());
    fst[file.entry_offset + 8..file.entry_offset + 12].copy_from_slice(&size.to_be_bytes());
}

/// Split spans to [`MAX_SPAN_BYTES`] so the pipeline's buffers stay
/// bounded regardless of gap sizes.
fn split_spans(spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let mut done = 0u64;
        while done < span.len {
            let take = (span.len - done).min(MAX_SPAN_BYTES);
            let kind = match &span.kind {
                SpanKind::Patched(buf, off) => SpanKind::Patched(buf.clone(), off + done),
                SpanKind::Verbatim(off) => SpanKind::Verbatim(off + done),
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
    out_off: u64,
    out_len: usize,
}

enum WorkKind {
    Bytes(Vec<u8>),
    Zeros,
    Fill(u8),
    Junk,
}

pub(crate) struct NkitSpanWorker {
    junk_id: [u8; 4],
    disc_num: u8,
}

impl Worker<NkitSpanWork, Vec<u8>, NkitError> for NkitSpanWorker {
    fn process(&mut self, work: NkitSpanWork) -> NkitResult<Vec<u8>> {
        Ok(match work.kind {
            WorkKind::Bytes(b) => b,
            WorkKind::Zeros => vec![0u8; work.out_len],
            WorkKind::Fill(byte) => vec![byte; work.out_len],
            WorkKind::Junk => {
                let mut buf = vec![0u8; work.out_len];
                LaggedFibonacci::fill_junk(&self.junk_id, self.disc_num, work.out_off, &mut buf);
                buf
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

        let spans: Vec<GroupSpan> = plan
            .spans
            .iter()
            .map(|s| GroupSpan {
                logical_offset: s.out_off,
                logical_size: s.len as u32,
            })
            .collect();
        let cap = in_flight_cap(MAX_SPAN_BYTES);
        let workers: Vec<NkitSpanWorker> = (0..parallelism().min(cap.max(2)))
            .map(|_| NkitSpanWorker {
                junk_id: plan.junk_id,
                disc_num: plan.disc_num,
            })
            .collect();

        let plan_spans = plan.spans;
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
                SpanKind::Junk => WorkKind::Junk,
            };
            Ok(NkitSpanWork {
                kind,
                out_off: span.out_off,
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
            pos: 0,
        })
    }

    pub fn image_size(&self) -> u64 {
        self.image_size
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
            if self.crc.value() != self.source_crc {
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
