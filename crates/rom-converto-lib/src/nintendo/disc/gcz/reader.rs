//! `Read + Seek` view over a GCZ container that reconstructs the
//! logical disc on the fly. Block inflation and checksum verification
//! run on the shared worker pool via [`PipelinedGroupReader`]; the
//! reader thread only does sequential disk reads.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use binrw::BinRead;

use flate2::{Decompress, FlushDecompress, Status};

use super::error::{GczError, GczResult};
use super::format::{GCZ_HEADER_SIZE, GCZ_UNCOMPRESSED_FLAG, GczHeader, adler32};
use crate::util::group_reader::{GroupSpan, PipelinedGroupReader, in_flight_cap};
use crate::util::worker_pool::{Worker, parallelism};

pub(crate) struct GczBlockWork {
    block: u64,
    stored: Vec<u8>,
    compressed: bool,
    stored_hash: u32,
    /// Logical bytes this block contributes; smaller than the block
    /// size only for the final block of a non-aligned disc.
    out_size: u32,
    block_size: u32,
}

pub(crate) struct GczBlockWorker {
    inflater: Decompress,
}

impl GczBlockWorker {
    pub(crate) fn new() -> Self {
        Self {
            inflater: Decompress::new(true),
        }
    }
}

impl Worker<GczBlockWork, Vec<u8>, GczError> for GczBlockWorker {
    fn process(&mut self, work: GczBlockWork) -> GczResult<Vec<u8>> {
        let computed = adler32(&work.stored);
        if computed != work.stored_hash {
            return Err(GczError::BlockHashMismatch {
                block: work.block,
                stored: work.stored_hash,
                computed,
            });
        }
        let mut out = if work.compressed {
            let mut out = vec![0u8; work.block_size as usize];
            self.inflater.reset(true);
            let mut in_pos = 0usize;
            let mut out_pos = 0usize;
            loop {
                let before_in = self.inflater.total_in();
                let before_out = self.inflater.total_out();
                let status = self
                    .inflater
                    .decompress(
                        &work.stored[in_pos..],
                        &mut out[out_pos..],
                        FlushDecompress::Finish,
                    )
                    .map_err(|e| GczError::Inflate {
                        block: work.block,
                        reason: e.to_string(),
                    })?;
                in_pos += (self.inflater.total_in() - before_in) as usize;
                out_pos += (self.inflater.total_out() - before_out) as usize;
                match status {
                    Status::StreamEnd => break,
                    Status::Ok | Status::BufError => {
                        if out_pos >= out.len() || in_pos >= work.stored.len() {
                            break;
                        }
                    }
                }
            }
            out.truncate(out_pos);
            out
        } else {
            work.stored
        };
        if out.len() < work.out_size as usize {
            return Err(GczError::Inflate {
                block: work.block,
                reason: format!(
                    "block holds {} bytes, expected at least {}",
                    out.len(),
                    work.out_size
                ),
            });
        }
        // Producers differ on whether the final partial block is
        // stored padded to a full block; serve exactly the logical
        // extent either way.
        out.truncate(work.out_size as usize);
        Ok(out)
    }
}

/// Parsed header plus the block pointer and checksum tables.
pub(crate) struct GczLayout {
    pub header: GczHeader,
    ptrs: Vec<u64>,
    hashes: Vec<u32>,
    data_base: u64,
    file_len: u64,
}

impl GczLayout {
    pub(crate) fn parse<S: Read + Seek>(inner: &mut S) -> GczResult<Self> {
        inner.seek(SeekFrom::Start(0))?;
        let header = GczHeader::read(inner)?;
        header.validate()?;
        let file_len = inner.seek(SeekFrom::End(0))?;
        let nb = header.num_blocks as usize;
        let table_len = (nb as u64)
            .checked_mul(12)
            .ok_or_else(|| GczError::InvalidHeader("block table size overflows".into()))?;
        let data_base = GCZ_HEADER_SIZE
            .checked_add(table_len)
            .ok_or_else(|| GczError::InvalidHeader("block table extent overflows".into()))?;
        if data_base > file_len {
            return Err(GczError::InvalidHeader(format!(
                "block tables end at {data_base:#x}, past file size {file_len:#x}"
            )));
        }
        inner.seek(SeekFrom::Start(GCZ_HEADER_SIZE))?;
        let mut table_buf = [0u8; 64 * 1024];
        let mut ptrs = Vec::with_capacity(nb);
        let mut remaining = nb;
        while remaining > 0 {
            let count = remaining.min(table_buf.len() / 8);
            let bytes = count * 8;
            inner.read_exact(&mut table_buf[..bytes])?;
            ptrs.extend(
                table_buf[..bytes]
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_le_bytes(*c)),
            );
            remaining -= count;
        }
        let mut hashes = Vec::with_capacity(nb);
        remaining = nb;
        while remaining > 0 {
            let count = remaining.min(table_buf.len() / 4);
            let bytes = count * 4;
            inner.read_exact(&mut table_buf[..bytes])?;
            hashes.extend(
                table_buf[..bytes]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c)),
            );
            remaining -= count;
        }
        Ok(Self {
            header,
            ptrs,
            hashes,
            data_base,
            file_len,
        })
    }

    /// Absolute file offset, stored length, and compression flag of block `i`.
    pub(crate) fn stored_extent(&self, i: u64) -> GczResult<(u64, u32, bool)> {
        let ptr = self.ptrs[i as usize];
        let compressed = ptr & GCZ_UNCOMPRESSED_FLAG == 0;
        let start = ptr & !GCZ_UNCOMPRESSED_FLAG;
        let end = match self.ptrs.get(i as usize + 1) {
            Some(next) => next & !GCZ_UNCOMPRESSED_FLAG,
            None => self.header.compressed_data_size,
        };
        if end < start || end - start > self.header.block_size as u64 * 2 + 64 {
            return Err(GczError::InvalidHeader(format!(
                "block {i} pointer table is inconsistent ({start:#x}..{end:#x})"
            )));
        }
        if self.data_base.saturating_add(end) > self.file_len {
            return Err(GczError::InvalidHeader(format!(
                "block {i} extent ends at {end:#x}, past end of file"
            )));
        }
        Ok((self.data_base + start, (end - start) as u32, compressed))
    }

    pub(crate) fn stored_hash(&self, i: u64) -> u32 {
        self.hashes[i as usize]
    }

    pub(crate) fn out_size(&self, i: u64) -> u32 {
        let off = i * self.header.block_size as u64;
        (self.header.data_size - off).min(self.header.block_size as u64) as u32
    }

    pub(crate) fn spans(&self) -> Vec<GroupSpan> {
        (0..self.header.num_blocks as u64)
            .map(|i| GroupSpan {
                logical_offset: i * self.header.block_size as u64,
                logical_size: self.out_size(i),
            })
            .collect()
    }

    pub(crate) fn read_work<S: Read + Seek>(
        &self,
        inner: &mut S,
        i: u64,
    ) -> GczResult<GczBlockWork> {
        let (off, len, compressed) = self.stored_extent(i)?;
        let mut stored = vec![0u8; len as usize];
        inner.seek(SeekFrom::Start(off))?;
        inner.read_exact(&mut stored)?;
        Ok(GczBlockWork {
            block: i,
            stored,
            compressed,
            stored_hash: self.stored_hash(i),
            out_size: self.out_size(i),
            block_size: self.header.block_size,
        })
    }
}

type ProduceFn = Box<dyn FnMut(u64) -> GczResult<GczBlockWork> + Send>;

/// `Read + Seek` view over a GCZ container's decompressed data.
pub struct GczReader {
    pipeline: PipelinedGroupReader<GczBlockWork, GczError, ProduceFn>,
    header: GczHeader,
}

impl GczReader {
    /// Opens the GCZ file at `path`.
    pub fn open(path: &Path) -> GczResult<Self> {
        Self::open_with_lookahead(path, usize::MAX)
    }

    pub fn open_with_lookahead(path: &Path, lookahead: usize) -> GczResult<Self> {
        Self::from_source_with_lookahead(File::open(path)?, lookahead)
    }

    pub fn from_source_with_lookahead<S: Read + Seek + Send + 'static>(
        mut inner: S,
        lookahead: usize,
    ) -> GczResult<Self> {
        let layout = GczLayout::parse(&mut inner)?;
        let header = layout.header;
        let spans = layout.spans();
        let cap = in_flight_cap(header.block_size as u64);
        let workers: Vec<GczBlockWorker> = (0..parallelism().min(cap.max(2)).min(lookahead.max(2)))
            .map(|_| GczBlockWorker::new())
            .collect();
        let produce: ProduceFn = Box::new(move |i| layout.read_work(&mut inner, i));
        Ok(Self {
            pipeline: PipelinedGroupReader::with_lookahead(workers, spans, cap, lookahead, produce),
            header,
        })
    }

    /// Size of the decompressed logical disc image in bytes.
    pub fn data_size(&self) -> u64 {
        self.header.data_size
    }

    /// The header's `sub_type` field (0 = GameCube, 1 = Wii). Dolphin
    /// writes it but never reads it back.
    pub fn sub_type(&self) -> u32 {
        self.header.sub_type
    }

    /// Header-only data size, for progress totals without spinning up
    /// the decode pipeline.
    pub fn data_size_of(path: &Path) -> GczResult<u64> {
        let mut f = File::open(path)?;
        let header = GczHeader::read(&mut f)?;
        header.validate()?;
        Ok(header.data_size)
    }
}

impl Read for GczReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.pipeline.read(buf)
    }
}

impl Seek for GczReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pipeline.seek(from)
    }
}

/// Inflate just enough of block 0 to inspect the logical stream's
/// first bytes without the pipeline, used by format detection to spot
/// an NKit stream inside a GCZ wrapper.
pub fn gcz_logical_prefix(path: &Path, len: usize) -> GczResult<Vec<u8>> {
    let mut f = File::open(path)?;
    let layout = GczLayout::parse(&mut f)?;
    let work = layout.read_work(&mut f, 0)?;
    let mut block = GczBlockWorker::new().process(work)?;
    block.truncate(len);
    Ok(block)
}
