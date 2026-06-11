//! CSO/ZSO reading: header + index parsing and the pool-parallel
//! block decompressor.

use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use binrw::BinRead;

use crate::cso::compression::BlockDecompressor;
use crate::cso::error::{CsoError, CsoResult};
use crate::cso::models::{
    CISO_HEADER_SIZE, CISO_INDEX_UNCOMPRESSED, CisoHeader, CsoFormat, valid_block_size,
};
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

pub(crate) struct CsoSyncHandle {
    pub header: CisoHeader,
    pub format: CsoFormat,
    pub index: Vec<u32>,
    pub file: Arc<std::fs::File>,
    pub file_size: u64,
}

pub(crate) fn open_cso_sync(path: &Path) -> CsoResult<CsoSyncHandle> {
    let mut file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();

    let mut header_bytes = [0u8; CISO_HEADER_SIZE as usize];
    file.read_exact(&mut header_bytes)?;
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

    let entries = header.block_count() as usize + 1;
    let mut index_bytes = vec![0u8; entries * 4];
    file.read_exact(&mut index_bytes)?;
    let index: Vec<u32> = index_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    Ok(CsoSyncHandle {
        header,
        format,
        index,
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
    let shift = handle.header.index_shift;
    let entry = handle.index[block as usize];
    let next = handle.index[block as usize + 1];

    let offset = ((entry & !CISO_INDEX_UNCOMPRESSED) as u64) << shift;
    let end = ((next & !CISO_INDEX_UNCOMPRESSED) as u64) << shift;
    if end < offset || end > handle.file_size {
        return Err(CsoError::CorruptIndex(format!(
            "block {block} spans {offset:#X}..{end:#X} outside the file"
        )));
    }

    let block_size = handle.header.block_size as u64;
    let logical_start = block * block_size;
    let expected_len = (handle.header.uncompressed_size - logical_start).min(block_size) as usize;

    let raw = entry & CISO_INDEX_UNCOMPRESSED != 0;
    // Raw blocks occupy exactly their logical size; the span up to
    // the next entry may carry alignment padding. Compressed spans
    // keep the padding: deflate and LZ4 both stop at stream end.
    let stored_len = if raw {
        if ((end - offset) as usize) < expected_len {
            return Err(CsoError::CorruptIndex(format!(
                "raw block {block} shorter than its logical size"
            )));
        }
        expected_len
    } else {
        (end - offset) as usize
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
        let spec = work.spec;
        let mut stored = vec![0u8; spec.stored_len];
        file_read_exact_at(&self.file, &mut stored, spec.offset)?;

        let bytes = if spec.raw {
            stored
        } else {
            self.codec.decompress(&stored, spec.expected_len)?
        };
        if bytes.len() != spec.expected_len {
            return Err(CsoError::BlockSizeMismatch {
                block: work.block,
                expected: spec.expected_len,
                actual: bytes.len(),
            });
        }
        Ok(CsoExtractedOut { bytes })
    }
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
) -> CsoResult<()> {
    let blocks = handle.header.block_count();
    let max_in_flight = parallelism() * 2;
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(max_in_flight * 2);

    let scope_result: CsoResult<()> = std::thread::scope(|s| {
        let writer_slot = &mut *writer;
        let writer_handle = s.spawn(move || -> CsoResult<()> {
            while let Ok(bytes) = write_rx.recv() {
                writer_slot.write_all(&bytes)?;
            }
            Ok(())
        });

        let drive_result = drive(
            pool,
            blocks,
            max_in_flight,
            |block| -> CsoResult<CsoExtractWork> {
                Ok(CsoExtractWork {
                    spec: block_spec(handle, block)?,
                    block,
                })
            },
            |_seq, out: CsoExtractedOut| -> CsoResult<()> {
                let len = out.bytes.len() as u64;
                write_tx
                    .send(out.bytes)
                    .map_err(|_| CsoError::WorkerPoolClosed)?;
                bytes_done.fetch_add(len, Ordering::Relaxed);
                Ok(())
            },
        );

        drop(write_tx);
        let writer_result = writer_handle
            .join()
            .unwrap_or_else(|_| Err(CsoError::WorkerPoolPanic));
        drive_result?;
        writer_result
    });
    scope_result
}
