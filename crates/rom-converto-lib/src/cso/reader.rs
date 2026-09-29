//! CSO/ZSO reading: header + index parsing and the pool-parallel
//! block decompressor.

use std::io::{BufWriter, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use binrw::BinRead;

use crate::cso::compression::BlockDecompressor;
use crate::cso::dax::DaxTables;
use crate::cso::error::{CsoError, CsoResult};
use crate::cso::models::{
    CISO_HEADER_SIZE, CISO_INDEX_UNCOMPRESSED, CisoHeader, CsoFormat, DAX_MAGIC, valid_block_size,
};
use crate::util::CancelToken;
use crate::util::Cancelled;
use crate::util::extent_end;
use crate::util::hash::{FileDigests, HashAlgo, MultiHasher};
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{
    Admission, Budget, Pool, PoolChannelClosed, Worker, drive, with_writer_thread,
};

pub(crate) struct CsoSyncHandle {
    pub header: CisoHeader,
    pub format: CsoFormat,
    pub index: Vec<u32>,
    /// Present only for DAX inputs; CSO/ZSO drive the `index` above.
    pub dax: Option<DaxTables>,
    pub file: Arc<std::fs::File>,
    pub file_size: u64,
}

pub(crate) fn open_cso_sync(path: &Path) -> CsoResult<CsoSyncHandle> {
    let mut file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();

    let mut header_bytes = [0u8; CISO_HEADER_SIZE as usize];
    file.read_exact(&mut header_bytes)?;
    if header_bytes[..4] == DAX_MAGIC {
        return crate::cso::dax::open_dax_sync(path, file_size);
    }
    let header = CisoHeader::read(&mut std::io::Cursor::new(&header_bytes))?;

    let format = header
        .format()
        .ok_or_else(|| CsoError::InvalidHeader("not a CISO/ZISO file".into()))?;
    if header.version > 1 {
        return Err(CsoError::InvalidHeader(format!(
            "version {} not supported (CSO v2 was never adopted)",
            header.version
        )));
    }
    if !valid_block_size(header.block_size) {
        return Err(CsoError::InvalidBlockSize(header.block_size));
    }
    if header.uncompressed_size == 0 {
        return Err(CsoError::InvalidHeader("empty image".into()));
    }

    let entries = header
        .block_count()
        .checked_add(1)
        .ok_or_else(|| CsoError::CorruptIndex("index entry count overflow".into()))?;
    let index_bytes = entries
        .checked_mul(4)
        .ok_or_else(|| CsoError::CorruptIndex("index byte length overflow".into()))?;
    if extent_end(CISO_HEADER_SIZE as u64, index_bytes, file_size).is_none() {
        return Err(CsoError::CorruptIndex(
            "index table extends beyond the file".into(),
        ));
    }
    let entries = usize::try_from(entries)
        .map_err(|_| CsoError::CorruptIndex("index does not fit memory".into()))?;
    let mut index = Vec::new();
    index
        .try_reserve_exact(entries)
        .map_err(|_| CsoError::CorruptIndex("index does not fit memory".into()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut remaining = index_bytes;
    while remaining != 0 {
        let bytes = remaining.min(buf.len() as u64) as usize;
        file.read_exact(&mut buf[..bytes])?;
        for entry in buf[..bytes].chunks_exact(4) {
            index.push(u32::from_le_bytes(
                entry
                    .try_into()
                    .expect("chunks_exact(4) yields four-byte entries"),
            ));
        }
        remaining -= bytes as u64;
    }

    Ok(CsoSyncHandle {
        header,
        format,
        index,
        dax: None,
        file: Arc::new(std::fs::File::open(path)?),
        file_size,
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockSpec {
    pub offset: u64,
    pub stored_len: usize,
    pub raw: bool,
    pub expected_len: usize,
}

/// Resolve one block's stored span and logical size from the index.
pub(crate) fn block_spec(handle: &CsoSyncHandle, block: u64) -> CsoResult<BlockSpec> {
    if let Some(dax) = &handle.dax {
        return crate::cso::dax::dax_block_spec(handle, dax, block);
    }
    if block >= handle.header.block_count() {
        return Err(CsoError::CorruptIndex(format!(
            "block {block} exceeds the image block count"
        )));
    }
    let shift = handle.header.index_shift as u32;
    let scale = 1u64
        .checked_shl(shift)
        .ok_or_else(|| CsoError::CorruptIndex("index shift is too large".into()))?;
    let entry = *handle
        .index
        .get(block as usize)
        .ok_or_else(|| CsoError::CorruptIndex(format!("missing index entry for block {block}")))?;
    let next = *handle.index.get(block as usize + 1).ok_or_else(|| {
        CsoError::CorruptIndex(format!("missing index sentinel after block {block}"))
    })?;

    let offset = ((entry & !CISO_INDEX_UNCOMPRESSED) as u64)
        .checked_mul(scale)
        .ok_or_else(|| CsoError::CorruptIndex("block offset shift overflow".into()))?;
    let end = ((next & !CISO_INDEX_UNCOMPRESSED) as u64)
        .checked_mul(scale)
        .ok_or_else(|| CsoError::CorruptIndex("block end shift overflow".into()))?;
    // `open_cso_sync` already rejected an index table extending past
    // `file_size`, so the data region start is always in bounds here.
    if end < offset || end > handle.file_size {
        return Err(CsoError::CorruptIndex(format!(
            "block {block} spans {offset:#X}..{end:#X} outside the file"
        )));
    }

    let block_size = handle.header.block_size as u64;
    let logical_start = block
        .checked_mul(block_size)
        .ok_or_else(|| CsoError::CorruptIndex("logical block offset overflow".into()))?;
    let expected_len = (handle.header.uncompressed_size - logical_start).min(block_size) as usize;

    let raw = entry & CISO_INDEX_UNCOMPRESSED != 0;
    // Raw blocks occupy exactly their logical size; the span up to
    // the next entry may carry alignment padding. Compressed spans
    // are consumed incrementally until the codec emits this block.
    let span = end - offset;
    let stored_len = if raw {
        if span < expected_len as u64 {
            return Err(CsoError::CorruptIndex(format!(
                "raw block {block} shorter than its logical size"
            )));
        }
        expected_len
    } else {
        usize::try_from(span)
            .map_err(|_| CsoError::CorruptIndex("compressed span exceeds address space".into()))?
    };
    Ok(BlockSpec {
        offset,
        stored_len,
        raw,
        expected_len,
    })
}

pub(crate) struct CsoExtractWork {
    pub(crate) spec: BlockSpec,
    pub(crate) block: u64,
}

pub(crate) struct CsoExtractedOut {
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct CsoExtractWorker {
    codec: BlockDecompressor,
    file: Arc<std::fs::File>,
}

impl Worker<CsoExtractWork, CsoExtractedOut, CsoError> for CsoExtractWorker {
    fn process(&mut self, work: CsoExtractWork) -> CsoResult<CsoExtractedOut> {
        let bytes = decode_cso_block(&self.file, &mut self.codec, work.spec, work.block)?;
        Ok(CsoExtractedOut { bytes })
    }
}

pub(crate) fn decode_cso_block(
    file: &std::fs::File,
    codec: &mut BlockDecompressor,
    spec: BlockSpec,
    block: u64,
) -> CsoResult<Vec<u8>> {
    let bytes = if spec.raw {
        let mut raw = vec![0u8; spec.expected_len];
        file_read_exact_at(file, &mut raw, spec.offset)?;
        raw
    } else {
        let fast_span_limit = spec
            .expected_len
            .saturating_mul(2)
            .saturating_add(64 * 1024);
        if spec.stored_len <= fast_span_limit {
            let mut stored = vec![0u8; spec.stored_len];
            file_read_exact_at(file, &mut stored, spec.offset)?;
            codec.decompress(&stored, spec.expected_len)?
        } else if matches!(codec, BlockDecompressor::Lz4) {
            decompress_lz4_at(file, codec, spec)?
        } else {
            // Deflate is the only other codec that can exceed the
            // fast span limit; DAX stored spans are u16-bounded (at
            // most 65535 bytes), always under `fast_span_limit`, so
            // the Dax variant never reaches this branch.
            stream_inflate_at(file, spec)?
        }
    };
    if bytes.len() != spec.expected_len {
        return Err(CsoError::BlockSizeMismatch {
            block,
            expected: spec.expected_len,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

fn stream_inflate_at(file: &std::fs::File, spec: BlockSpec) -> CsoResult<Vec<u8>> {
    use flate2::{Decompress, FlushDecompress, Status};

    const INPUT_CHUNK: usize = 64 * 1024;
    let mut decoder = Decompress::new(false);
    // Sized to `expected_len` exactly, same as the fast path's fixed
    // buffer: a stream that decodes to more than this silently
    // truncates at `expected_len` instead of erroring, matching
    // `deflate_decompress_with`'s behavior for identical corrupt
    // input taken through the fast path.
    let mut output = vec![0u8; spec.expected_len];
    let mut input = [0u8; INPUT_CHUNK];
    let mut input_len = 0usize;
    let mut input_used = 0usize;
    let mut read_total = 0usize;
    let mut written = 0usize;
    loop {
        if input_used == input_len && read_total < spec.stored_len {
            input_len = (spec.stored_len - read_total).min(INPUT_CHUNK);
            file_read_exact_at(
                file,
                &mut input[..input_len],
                spec.offset + read_total as u64,
            )?;
            read_total += input_len;
            input_used = 0;
        }
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(
                &input[input_used..input_len],
                &mut output[written..],
                if read_total == spec.stored_len && input_used == input_len {
                    FlushDecompress::Finish
                } else {
                    FlushDecompress::None
                },
            )
            .map_err(|e| CsoError::IoError(std::io::Error::other(e)))?;
        let consumed = (decoder.total_in() - before_in) as usize;
        let produced = (decoder.total_out() - before_out) as usize;
        input_used += consumed;
        written += produced;
        if status == Status::BufError {
            return Err(CsoError::IoError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "deflate decoder needs more input or output space",
            )));
        }
        if consumed == 0 && produced == 0 && input_used < input_len {
            return Err(CsoError::IoError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "deflate decoder made no progress",
            )));
        }
        if status == Status::StreamEnd || written == spec.expected_len {
            output.truncate(written);
            return Ok(output);
        }
        if input_used == input_len && read_total == spec.stored_len {
            return Err(CsoError::IoError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incomplete deflate block",
            )));
        }
    }
}

fn decompress_lz4_at(
    file: &std::fs::File,
    codec: &mut BlockDecompressor,
    spec: BlockSpec,
) -> CsoResult<Vec<u8>> {
    // LZ4's maximum compressed block size, excluding arbitrary
    // index-shift padding that follows the stream.
    let max_stream = spec.expected_len + spec.expected_len / 255 + 16;
    let stored_len = spec.stored_len.min(max_stream);
    let mut stored = vec![0u8; stored_len];
    file_read_exact_at(file, &mut stored, spec.offset)?;
    codec.decompress(&stored, spec.expected_len)
}

pub(crate) fn cso_extract_admission(handle: &CsoSyncHandle, requested_workers: usize) -> Admission {
    let block_bytes = handle.header.block_size as usize;
    Budget {
        codec_per_worker: 0,
        per_job: block_bytes.saturating_mul(2).saturating_add(4096),
        writer_slot: block_bytes.saturating_mul(2),
        fixed: 0,
    }
    .admit(requested_workers, handle.header.block_count())
    .unwrap_or(Admission::DEGRADED)
}

pub(crate) fn make_cso_extract_workers(
    n: usize,
    format: CsoFormat,
    file: &Arc<std::fs::File>,
) -> Vec<CsoExtractWorker> {
    (0..n)
        .map(|_| CsoExtractWorker {
            codec: BlockDecompressor::new(format),
            file: file.clone(),
        })
        .collect()
}

/// Decode every block in order into `writer` (the restored ISO).
pub(crate) fn extract_blocks(
    pool: &Pool<CsoExtractWork, CsoExtractedOut, CsoError>,
    handle: &CsoSyncHandle,
    writer: &mut BufWriter<std::fs::File>,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
    admission: crate::util::worker_pool::Admission,
) -> CsoResult<()> {
    let blocks = handle.header.block_count();
    let max_in_flight = admission.max_in_flight;

    with_writer_thread(
        writer,
        admission.writer_capacity,
        CsoError::WorkerPoolPanic,
        |write_tx| {
            drive(
                pool,
                blocks,
                max_in_flight,
                |block| -> CsoResult<CsoExtractWork> {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    Ok(CsoExtractWork {
                        spec: block_spec(handle, block)?,
                        block,
                    })
                },
                |_seq, out: CsoExtractedOut| -> CsoResult<()> {
                    let len = out.bytes.len() as u64;
                    write_tx
                        .send(out.bytes)
                        .map_err(|_| CsoError::WorkerPoolClosed(PoolChannelClosed))?;
                    bytes_done.fetch_add(len, Ordering::Relaxed);
                    Ok(())
                },
            )
        },
    )
}

/// Digest-side twin of [`extract_blocks`]: the pool decodes every
/// block in parallel and the ordered consume closure folds each block
/// into `hasher` instead of a writer thread. `drive`'s reorder buffer
/// guarantees strict block order, so the digest is identical to one
/// taken over the restored ISO.
pub(crate) fn hash_blocks(
    handle: &CsoSyncHandle,
    pool: &Pool<CsoExtractWork, CsoExtractedOut, CsoError>,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
    admission: crate::util::worker_pool::Admission,
) -> CsoResult<FileDigests> {
    let blocks = handle.header.block_count();
    let max_in_flight = admission.max_in_flight;
    let mut hasher = MultiHasher::new(algos);

    drive(
        pool,
        blocks,
        max_in_flight,
        |block| -> CsoResult<CsoExtractWork> {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            Ok(CsoExtractWork {
                spec: block_spec(handle, block)?,
                block,
            })
        },
        |_seq, out: CsoExtractedOut| -> CsoResult<()> {
            hasher.update(&out.bytes);
            bytes_done.fetch_add(out.bytes.len() as u64, Ordering::Relaxed);
            Ok(())
        },
    )?;

    Ok(hasher.finalize(handle.header.uncompressed_size))
}
