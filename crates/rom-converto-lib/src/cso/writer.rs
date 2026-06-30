//! Streaming CSO/ZSO writer.
//!
//! A placeholder header and zeroed index go out first, blocks are
//! compressed on the worker pool and written in order by a dedicated
//! writer thread, and the real header + index are patched in with one
//! seek at the end. Single output file, no temporaries; memory stays
//! at one block per in-flight pool slot plus the u32 index.

use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use binrw::BinWrite;

use crate::cd::IO_BUFFER_SIZE;
use crate::cso::compression::BlockCompressor;
use crate::cso::error::{CsoError, CsoResult};
use crate::cso::models::{
    CISO_HEADER_SIZE, CISO_INDEX_UNCOMPRESSED, CisoHeader, CsoFormat, block_count,
};
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

pub(crate) struct CsoBlockWork {
    data: Vec<u8>,
}

pub(crate) struct CsoBlockOut {
    bytes: Vec<u8>,
    raw: bool,
}

struct CsoCompressWorker {
    codec: BlockCompressor,
    align: usize,
}

impl Worker<CsoBlockWork, CsoBlockOut, CsoError> for CsoCompressWorker {
    fn process(&mut self, work: CsoBlockWork) -> CsoResult<CsoBlockOut> {
        let compressed = self.codec.compress(&work.data)?;
        // maxcso's store-raw rule: a compressed block only pays off
        // if it is still smaller after alignment padding.
        let aligned = |len: usize| len.div_ceil(self.align) * self.align;
        if aligned(compressed.len()) >= aligned(work.data.len()) {
            Ok(CsoBlockOut {
                bytes: work.data,
                raw: true,
            })
        } else {
            Ok(CsoBlockOut {
                bytes: compressed,
                raw: false,
            })
        }
    }
}

/// Compress `input` into a CSO/ZSO at `output`. `index_shift` comes
/// from [`crate::cso::models::pick_index_shift`] in production;
/// tests pass larger shifts to exercise offset packing and alignment
/// without multi-GiB fixtures.
pub(crate) fn write_cso_blocking(
    input: &Path,
    output: &Path,
    format: CsoFormat,
    block_size: u32,
    index_shift: u8,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> CsoResult<()> {
    let input_file = std::fs::File::open(input)?;
    let input_size = input_file.metadata()?.len();
    let mut reader = BufReader::with_capacity(IO_BUFFER_SIZE, input_file);

    let blocks = block_count(input_size, block_size);
    let mut index = vec![0u32; blocks as usize + 1];
    let align = 1u64 << index_shift;

    let header = CisoHeader::new(format, input_size, block_size, index_shift);
    let mut header_bytes = std::io::Cursor::new(Vec::new());
    header.write(&mut header_bytes)?;
    let header_bytes = header_bytes.into_inner();

    let out_file = std::fs::File::create(output)?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);
    writer.write_all(&header_bytes)?;
    writer.write_all(&vec![0u8; index.len() * 4])?;
    let data_start = CISO_HEADER_SIZE as u64 + index.len() as u64 * 4;

    let workers: Vec<CsoCompressWorker> = (0..parallelism())
        .map(|_| CsoCompressWorker {
            codec: BlockCompressor::new(format),
            align: align as usize,
        })
        .collect();
    let pool: Pool<CsoBlockWork, CsoBlockOut, CsoError> = Pool::spawn(workers);
    let max_in_flight = parallelism() * 2;

    let mut pos = data_start;
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(max_in_flight * 2);

    let scope_result: CsoResult<()> = std::thread::scope(|s| {
        let writer_slot = &mut writer;
        let writer_handle = s.spawn(move || -> CsoResult<()> {
            while let Ok(bytes) = write_rx.recv() {
                writer_slot.write_all(&bytes)?;
            }
            Ok(())
        });

        let drive_result = drive(
            &pool,
            blocks,
            max_in_flight,
            |block_idx| -> CsoResult<CsoBlockWork> {
                if cancel.is_cancelled() {
                    return Err(CsoError::Cancelled);
                }
                let offset = block_idx * block_size as u64;
                let take = ((input_size - offset) as usize).min(block_size as usize);
                let mut data = vec![0u8; take];
                reader.read_exact(&mut data)?;
                bytes_done.fetch_add(take as u64, Ordering::Relaxed);
                Ok(CsoBlockWork { data })
            },
            |seq, out: CsoBlockOut| -> CsoResult<()> {
                let aligned_pos = pos.div_ceil(align) * align;
                if aligned_pos > pos {
                    write_tx
                        .send(vec![0u8; (aligned_pos - pos) as usize])
                        .map_err(|_| CsoError::WorkerPoolClosed)?;
                }
                index[seq as usize] = index_entry(aligned_pos, index_shift, out.raw)?;
                pos = aligned_pos + out.bytes.len() as u64;
                write_tx
                    .send(out.bytes)
                    .map_err(|_| CsoError::WorkerPoolClosed)?;
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
    pool.shutdown();
    scope_result?;

    // EOF sentinel, then pad the file end to the alignment the
    // sentinel claims.
    let aligned_end = pos.div_ceil(align) * align;
    if aligned_end > pos {
        writer.write_all(&vec![0u8; (aligned_end - pos) as usize])?;
    }
    index[blocks as usize] = index_entry(aligned_end, index_shift, false)?;

    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(&header_bytes)?;
    let mut index_bytes = Vec::with_capacity(index.len() * 4);
    for entry in &index {
        index_bytes.extend_from_slice(&entry.to_le_bytes());
    }
    writer.write_all(&index_bytes)?;
    writer.flush()?;
    Ok(())
}

fn index_entry(offset: u64, shift: u8, raw: bool) -> CsoResult<u32> {
    let packed = offset >> shift;
    if packed >= CISO_INDEX_UNCOMPRESSED as u64 {
        return Err(CsoError::CorruptIndex(format!(
            "offset {offset:#X} does not fit the index with shift {shift}"
        )));
    }
    Ok(packed as u32 | if raw { CISO_INDEX_UNCOMPRESSED } else { 0 })
}
