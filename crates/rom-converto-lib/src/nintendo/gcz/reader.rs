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
}

impl GczLayout {
    pub(crate) fn parse<S: Read + Seek>(inner: &mut S) -> GczResult<Self> {
        inner.seek(SeekFrom::Start(0))?;
        let header = GczHeader::read(inner)?;
        header.validate()?;

        let nb = header.num_blocks as usize;
        let mut table = vec![0u8; nb * 12];
        inner.read_exact(&mut table)?;
        let (ptr_bytes, hash_bytes) = table.split_at(nb * 8);
        let ptrs: Vec<u64> = ptr_bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let hashes: Vec<u32> = hash_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();

        Ok(Self {
            header,
            ptrs,
            hashes,
            data_base: GCZ_HEADER_SIZE + nb as u64 * 12,
        })
    }

    /// Absolute file offset, stored length, and compression flag of
    /// block `i`.
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
                "block {i} pointer table is inconsistent ({start:#X}..{end:#X})"
            )));
        }
        Ok((self.data_base + start, (end - start) as u32, compressed))
    }

    pub(crate) fn stored_hash(&self, i: u64) -> u32 {
        self.hashes[i as usize]
    }

    /// Logical size served by block `i`.
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

pub struct GczReader {
    pipeline: PipelinedGroupReader<GczBlockWork, GczError, ProduceFn>,
    header: GczHeader,
}

impl GczReader {
    pub fn open(path: &Path) -> GczResult<Self> {
        Self::from_source(File::open(path)?)
    }

    /// Build a reader over any seekable source, allowing layered
    /// containers (an NKit stream inside a GCZ wrapper).
    pub fn from_source<S: Read + Seek + Send + 'static>(mut inner: S) -> GczResult<Self> {
        let layout = GczLayout::parse(&mut inner)?;
        let header = layout.header;
        let spans = layout.spans();
        let cap = in_flight_cap(header.block_size as u64);
        let workers: Vec<GczBlockWorker> = (0..parallelism().min(cap.max(2)))
            .map(|_| GczBlockWorker::new())
            .collect();
        let produce: ProduceFn = Box::new(move |i| layout.read_work(&mut inner, i));
        Ok(Self {
            pipeline: PipelinedGroupReader::new(workers, spans, cap, produce),
            header,
        })
    }

    pub fn data_size(&self) -> u64 {
        self.header.data_size
    }

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
