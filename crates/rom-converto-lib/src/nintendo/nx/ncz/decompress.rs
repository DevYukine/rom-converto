//! NCZ -> NCA streaming decompression. Pulls from `input`, applies
//! `ReencryptWriter` over the decompressed payload, and forwards
//! re-encrypted bytes to `out`. Memory stays bounded by buffer sizes
//! regardless of input length.

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use byteorder::{LE, ReadBytesExt};

use super::LARGE_BLOCK_STREAM_CHUNK;
use crate::nintendo::nx::constants::{
    MAX_BLOCK_SIZE_EXP, MIN_BLOCK_SIZE_EXP, NCA_PREFIX_SIZE, NCZ_SECTION_ENTRY_SIZE,
    NCZBLOCK_MAGIC, NCZSECTN_MAGIC,
};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::ncz::decompress_worker::{NczDecompressWork, spawn_ncz_decompress_pool};
use crate::nintendo::nx::ncz::header::{NczBlockInfo, NczSectionEntry};
use crate::nintendo::nx::ncz::reencrypt::ReencryptWriter;
use crate::util::worker_pool::{PoolChannelClosed, drive, parallelism};
use crate::util::{CancelToken, Cancelled, ProgressReporter, extent_end};

const STREAM_CHUNK: usize = 256 * 1024;
const READ_BUFFER: usize = 4 * 1024 * 1024;

/// Streams an NCZ container from `input`, decompressing its payload
/// (solid or block mode) and re-encrypting it through a
/// `ReencryptWriter` before writing the resulting NCA bytes to `out`.
/// Checked against `cancel` between steps so a caller can abort early.
///
/// # Errors
///
/// Returns an error if the NCZ headers are malformed, decompression
/// fails, `cancel` is triggered, or I/O on `input` / `out` fails.
pub fn ncz_to_nca<R: Read + Seek + Send, W: Write>(
    input: &mut R,
    out: &mut W,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    check_cancel(cancel)?;
    let mut input = io::BufReader::with_capacity(READ_BUFFER, input);

    let mut prefix = [0u8; NCA_PREFIX_SIZE];
    input.read_exact(&mut prefix)?;
    out.write_all(&prefix)?;
    progress.inc(NCA_PREFIX_SIZE as u64);

    let sections = read_sections(&mut input, cancel)?;
    let (block, payload_prefix) = read_block_or_payload_start(&mut input, cancel)?;

    let mut reenc = ReencryptWriter::new(out, &sections, NCA_PREFIX_SIZE as u64);
    match block {
        Some(info) => decode_blocks_stream(&mut input, &info, &mut reenc, progress, cancel)?,
        None => {
            let stash = payload_prefix.unwrap_or_default();
            let chained = Cursor::new(stash).chain(input);
            decode_solid_stream(chained, &mut reenc, progress, cancel)?;
        }
    }
    Ok(())
}

fn read_sections<R: Read + Seek>(
    input: &mut R,
    cancel: &CancelToken,
) -> NxResult<Vec<NczSectionEntry>> {
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic)?;
    if magic != NCZSECTN_MAGIC {
        return Err(NxError::NczBadMagic(magic));
    }
    let count = input.read_i64::<LE>()?;
    if count < 0 {
        return Err(NxError::IncompleteSection);
    }
    let after_count = input.stream_position()?;
    let end = input.seek(SeekFrom::End(0))?;
    let section_bytes = u64::try_from(count)
        .ok()
        .and_then(|n| n.checked_mul(NCZ_SECTION_ENTRY_SIZE as u64))
        .ok_or(NxError::IncompleteSection)?;
    if extent_end(after_count, section_bytes, end).is_none() {
        return Err(NxError::IncompleteSection);
    }
    input.seek(SeekFrom::Start(after_count))?;
    let count = usize::try_from(count).map_err(|_| NxError::IncompleteSection)?;
    let mut sections = Vec::with_capacity(count);
    let mut entry = vec![0u8; NCZ_SECTION_ENTRY_SIZE];
    for _ in 0..count {
        check_cancel(cancel)?;
        input.read_exact(&mut entry)?;
        let mut cur = Cursor::new(&entry);
        let offset = cur.read_i64::<LE>()?;
        let size = cur.read_i64::<LE>()?;
        let crypto_type = cur.read_i64::<LE>()?;
        let _padding = cur.read_i64::<LE>()?;
        let mut crypto_key = [0u8; 16];
        cur.read_exact(&mut crypto_key)?;
        let mut crypto_counter = [0u8; 16];
        cur.read_exact(&mut crypto_counter)?;
        sections.push(NczSectionEntry {
            offset,
            size,
            crypto_type,
            crypto_key,
            crypto_counter,
        });
    }
    Ok(sections)
}

fn read_block_or_payload_start<R: Read + Seek>(
    input: &mut R,
    cancel: &CancelToken,
) -> NxResult<(Option<NczBlockInfo>, Option<[u8; 8]>)> {
    check_cancel(cancel)?;
    let mut peek = [0u8; 8];
    if let Err(e) = input.read_exact(&mut peek) {
        return if e.kind() == io::ErrorKind::UnexpectedEof {
            Ok((None, None))
        } else {
            Err(e.into())
        };
    }
    if peek != NCZBLOCK_MAGIC {
        return Ok((None, Some(peek)));
    }
    let version = input.read_u8()?;
    let kind = input.read_u8()?;
    let _u8 = input.read_u8()?;
    let block_size_exp = input.read_u8()?;
    if !(MIN_BLOCK_SIZE_EXP..=MAX_BLOCK_SIZE_EXP).contains(&block_size_exp) {
        return Err(NxError::BlockSizeOutOfRange(block_size_exp));
    }
    let num_blocks = input.read_u32::<LE>()?;
    let decompressed_size = input.read_i64::<LE>()?;
    let _logical_size = u64::try_from(decompressed_size).map_err(|_| NxError::IncompleteSection)?;
    // The size table must fit between this header and the end of the
    // file (same span check as `read_sections`), so the loop below is
    // bounded by bytes actually present rather than the declared
    // count.
    let after_header = input.stream_position()?;
    let end = input.seek(SeekFrom::End(0))?;
    let table_bytes = u64::from(num_blocks) * size_of::<u32>() as u64;
    if extent_end(after_header, table_bytes, end).is_none() {
        return Err(NxError::IncompleteSection);
    }
    input.seek(SeekFrom::Start(after_header))?;
    let mut compressed_block_sizes = Vec::new();
    for _ in 0..num_blocks {
        check_cancel(cancel)?;
        compressed_block_sizes.push(input.read_u32::<LE>()?);
    }
    Ok((
        Some(NczBlockInfo {
            version,
            kind,
            block_size_exp,
            decompressed_size,
            compressed_block_sizes,
        }),
        None,
    ))
}

fn decode_solid_stream<R: Read + Send, W: Write>(
    input: R,
    reenc: &mut ReencryptWriter<W>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    // Solid-mode bottleneck on a single thread is the chain
    // `zstd_decode -> CTR encrypt -> file write`, all CPU/IO bound
    // and all serial. Splitting decode (Thread A) from encrypt+write
    // (Thread B) lets the OS overlap libzstd's read syscalls and
    // arithmetic with the AES + write pipeline on a second core. On
    // a 14 GB single-NCA NSZ this trims ~30% off serial wall time.
    // A second bounded channel carries drained buffers back to the
    // decoder thread so steady-state operation allocates nothing per
    // chunk instead of a fresh `Vec` every iteration.
    use std::sync::mpsc::sync_channel;
    use std::thread::scope;

    let (tx, rx) = sync_channel::<Vec<u8>>(8);
    let (ret_tx, ret_rx) = sync_channel::<Vec<u8>>(8);

    scope(|s| -> NxResult<()> {
        let decode_handle = s.spawn(move || -> NxResult<()> {
            let mut decoder = zstd::stream::read::Decoder::new(input)
                .map_err(|e| NxError::ZstdError(format!("zstd decoder init: {e}")))?;
            let mut buf = vec![0u8; STREAM_CHUNK];
            loop {
                check_cancel(cancel)?;
                let n = decoder
                    .read(&mut buf)
                    .map_err(|e| NxError::ZstdError(format!("zstd read: {e}")))?;
                if n == 0 {
                    break;
                }
                let mut chunk = std::mem::take(&mut buf);
                chunk.truncate(n);
                if tx.send(chunk).is_err() {
                    break;
                }
                buf = ret_rx
                    .try_recv()
                    .map(|mut recycled| {
                        recycled.clear();
                        recycled.resize(STREAM_CHUNK, 0);
                        recycled
                    })
                    .unwrap_or_else(|_| vec![0u8; STREAM_CHUNK]);
            }
            Ok(())
        });

        let recv_result: NxResult<()> = (|| {
            while let Ok(chunk) = rx.recv() {
                check_cancel(cancel)?;
                reenc.write_all(&chunk)?;
                progress.inc(chunk.len() as u64);
                let _ = ret_tx.send(chunk);
            }
            Ok(())
        })();
        // Drop the receiver (and the now-unused return sender) before
        // joining: if `recv_result` above returned early on
        // cancellation or a write error, the decoder thread may still
        // be blocked in `tx.send()` on a full channel. Since `rx`
        // stays alive for this whole scope by default, that send
        // would otherwise never unblock (a dropped receiver is the
        // only thing that turns a blocked `send` into an `Err`),
        // deadlocking `scope()`'s implicit join below.
        drop(rx);
        drop(ret_tx);
        let join_result = decode_handle
            .join()
            .map_err(|_| NxError::WorkerPoolClosed(PoolChannelClosed));
        recv_result?;
        join_result??;
        Ok(())
    })
}

fn decode_blocks_stream<R: Read, W: Write>(
    input: &mut R,
    info: &NczBlockInfo,
    reenc: &mut ReencryptWriter<W>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    let block_size_u64 = info.block_size_bytes();
    let Ok(block_size) = usize::try_from(block_size_u64) else {
        return decode_blocks_stream_large(input, info, reenc, progress, cancel);
    };
    let decompressed_size =
        u64::try_from(info.decompressed_size).map_err(|_| NxError::IncompleteSection)?;
    let num_blocks = info.compressed_block_sizes.len();
    // `decompressed_size` and the block table must agree: the last
    // block's logical size is `decompressed_size` minus the preceding
    // full blocks, and it is handed to workers as an allocation size,
    // so it can never exceed one block. Without this, a tiny header
    // declaring a huge `decompressed_size` reaches a worker's
    // `vec![0u8; logical_size]` before any byte is decoded.
    let declared_end = block_size_u64
        .checked_mul(num_blocks as u64)
        .ok_or(NxError::IncompleteSection)?;
    let last_block_start = block_size_u64
        .checked_mul(num_blocks.saturating_sub(1) as u64)
        .ok_or(NxError::IncompleteSection)?;
    if decompressed_size > declared_end || last_block_start > decompressed_size {
        return Err(NxError::IncompleteSection);
    }
    // The bulk path needs a compressed block buffer. Stream blocks whose
    // stored size exceeds zstd's usual bound directly from the input
    // instead of allocating from an untrusted stored-size field.
    for (i, &csz) in info.compressed_block_sizes.iter().enumerate() {
        let logical_size = if i + 1 == num_blocks {
            decompressed_size
                .checked_sub((i as u64) * block_size_u64)
                .ok_or(NxError::IncompleteSection)?
        } else {
            block_size_u64
        };
        if u64::from(csz) != logical_size
            && usize::try_from(logical_size)
                .is_ok_and(|size| csz as usize > zstd::zstd_safe::compress_bound(size))
        {
            return decode_blocks_stream_large(input, info, reenc, progress, cancel);
        }
    }

    // The decompression worker holds no persistent codec state (each
    // call is a one-shot `zstd::bulk::decompress_to_buffer`); the
    // per-in-flight-job cost is its compressed input plus decompressed
    // output. Only fall back to the bounded sequential streaming path
    // when even one in-memory block doesn't fit the shared budget.
    const TRANSIENT_DECODE_OVERHEAD: usize = 1024 * 1024;
    let queued_bytes_per_job = block_size.saturating_mul(2);
    let Some(admission) = crate::util::worker_pool::Budget {
        codec_per_worker: TRANSIENT_DECODE_OVERHEAD,
        per_job: queued_bytes_per_job,
        writer_slot: 0,
        fixed: 0,
    }
    .admit(parallelism().min(num_blocks.max(1)), num_blocks as u64) else {
        return decode_blocks_stream_large(input, info, reenc, progress, cancel);
    };
    let n_threads = admission.workers;
    let max_in_flight = admission.max_in_flight;
    let pool = spawn_ncz_decompress_pool(n_threads);

    let drive_result = drive(
        &pool,
        num_blocks as u64,
        max_in_flight,
        |seq| -> NxResult<NczDecompressWork> {
            check_cancel(cancel)?;
            let i = usize::try_from(seq).map_err(|_| NxError::IncompleteSection)?;
            let csz = info.compressed_block_sizes[i] as usize;
            let is_last = i + 1 == num_blocks;
            let logical_size_u64 = if is_last {
                decompressed_size
                    .checked_sub((i as u64) * block_size_u64)
                    .ok_or(NxError::IncompleteSection)?
            } else {
                block_size_u64
            };
            let logical_size =
                usize::try_from(logical_size_u64).map_err(|_| NxError::IncompleteSection)?;
            let raw = csz == logical_size;
            let mut compressed = vec![0u8; csz];
            input.read_exact(&mut compressed)?;
            Ok(NczDecompressWork {
                compressed,
                logical_size,
                raw,
            })
        },
        |_seq, out_block| -> NxResult<()> {
            check_cancel(cancel)?;
            reenc.write_all(&out_block.bytes)?;
            progress.inc(out_block.bytes.len() as u64);
            Ok(())
        },
    );
    pool.shutdown();
    drive_result?;
    Ok(())
}

/// Sequential, bounded-memory counterpart of [`decode_blocks_stream`],
/// used when its admission check finds that even one in-memory block
/// doesn't fit the shared worker-pool budget (huge block exponents).
/// Never materializes a whole compressed or decompressed block: raw
/// blocks stream straight through in fixed chunks, compressed blocks
/// stream through a zstd decoder that reads exactly `csz` bytes per
/// block (via `Read::take`) and writes decoded output as it's produced.
fn decode_blocks_stream_large<R: Read, W: Write>(
    input: &mut R,
    info: &NczBlockInfo,
    reenc: &mut ReencryptWriter<W>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> NxResult<()> {
    let block_size = info.block_size_bytes();
    let num_blocks = info.compressed_block_sizes.len();
    if num_blocks == 0 {
        return Ok(());
    }
    let mut scratch = vec![0u8; LARGE_BLOCK_STREAM_CHUNK];

    for (i, &csz) in info.compressed_block_sizes.iter().enumerate() {
        check_cancel(cancel)?;
        let csz = u64::from(csz);
        let is_last = i + 1 == num_blocks;
        let logical_size = if is_last {
            (info.decompressed_size as u64)
                .checked_sub((i as u64) * block_size)
                .ok_or(NxError::IncompleteSection)?
        } else {
            block_size
        };

        if csz == logical_size {
            // Stored raw: copy straight through in fixed chunks.
            let mut remaining = csz;
            while remaining > 0 {
                check_cancel(cancel)?;
                let take = remaining.min(scratch.len() as u64) as usize;
                input.read_exact(&mut scratch[..take])?;
                reenc.write_all(&scratch[..take])?;
                progress.inc(take as u64);
                remaining -= take as u64;
            }
            continue;
        }

        // Compressed: decode through exactly this block's `csz`-byte
        // span. `Take` bounds every read the decoder's internal
        // `BufReader` performs, so it can never pull bytes belonging
        // to the next block regardless of its buffering granularity.
        let limited = (&mut *input).take(csz);
        let mut decoder = zstd::stream::read::Decoder::new(limited)
            .map_err(|e| NxError::ZstdError(format!("decompress block {i}: {e}")))?;
        let mut produced = 0u64;
        while produced < logical_size {
            check_cancel(cancel)?;
            let want = (logical_size - produced).min(scratch.len() as u64) as usize;
            let n = decoder
                .read(&mut scratch[..want])
                .map_err(|e| NxError::ZstdError(format!("decompress block {i}: {e}")))?;
            if n == 0 {
                return Err(NxError::IncompleteSection);
            }
            reenc.write_all(&scratch[..n])?;
            progress.inc(n as u64);
            produced += n as u64;
        }
        // Continue reading *through the decoder* (not just draining
        // the raw underlying stream) until it reports true frame EOF:
        // this is what actually validates the frame's checksum and
        // epilogue. Any further non-empty read past `logical_size` is
        // corruption, since a well-formed block decodes to exactly
        // its declared logical size.
        let trailing = decoder
            .read(&mut scratch)
            .map_err(|e| NxError::ZstdError(format!("decompress block {i}: {e}")))?;
        if trailing != 0 {
            return Err(NxError::IncompleteSection);
        }
        // The frame ending doesn't by itself guarantee every declared
        // `csz` byte was consumed from `input`. A frame that's
        // internally valid but shorter than its declared stored size
        // would leave `input` positioned wrong for the next block.
        // The legacy decoder consumed the full declared block span even
        // when zstd stopped at an earlier frame boundary. Preserve that
        // positioning while allowing trailing bytes in the span.
        let mut remainder = decoder.finish();
        io::copy(&mut remainder, &mut io::sink())?;
    }
    Ok(())
}

fn check_cancel(cancel: &CancelToken) -> NxResult<()> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::nx::compress::{NxCompressOptions, compress_container};
    use crate::nintendo::nx::models::pfs0;
    use crate::nintendo::nx::ncz::compress::{NcaToNczOptions, NczMode, nca_to_ncz};
    use crate::nintendo::nx::test_fixtures::{build_synthetic_nca, synthetic_keyset};
    use crate::nintendo::nx::walker::NcaWalker;
    use crate::util::NoProgress;
    use sha2::{Digest, Sha256};
    use std::fs::{self, File};
    use std::io::{Cursor, Write};
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    #[test]
    fn zero_sized_block_table_decodes_empty_payload() {
        use byteorder::WriteBytesExt;

        let prefix = vec![0xA5; NCA_PREFIX_SIZE];
        let mut ncz = prefix.clone();
        ncz.extend_from_slice(&NCZSECTN_MAGIC);
        ncz.write_i64::<LE>(0).unwrap();
        ncz.extend_from_slice(&NCZBLOCK_MAGIC);
        ncz.write_u8(1).unwrap();
        ncz.write_u8(0).unwrap();
        ncz.write_u8(0).unwrap();
        ncz.write_u8(MIN_BLOCK_SIZE_EXP).unwrap();
        ncz.write_u32::<LE>(0).unwrap();
        ncz.write_i64::<LE>(0).unwrap();

        let mut input = Cursor::new(ncz);
        let mut output = Vec::new();
        ncz_to_nca(&mut input, &mut output, &NoProgress, &CancelToken::new()).unwrap();
        assert_eq!(output, prefix);
    }

    /// A 1-block header declaring a multi-TiB `decompressed_size` must
    /// be rejected from the header alone: the declared size disagrees
    /// with the one-entry block table, and the bulk path would hand
    /// it to a worker as a `vec!` allocation size before decoding a
    /// single byte.
    #[test]
    fn block_header_declaring_size_larger_than_table_is_rejected() {
        use byteorder::WriteBytesExt;

        let prefix = vec![0xA5; NCA_PREFIX_SIZE];
        let mut ncz = prefix.clone();
        ncz.extend_from_slice(&NCZSECTN_MAGIC);
        ncz.write_i64::<LE>(0).unwrap();
        ncz.extend_from_slice(&NCZBLOCK_MAGIC);
        ncz.write_u8(1).unwrap();
        ncz.write_u8(0).unwrap();
        ncz.write_u8(0).unwrap();
        ncz.write_u8(MIN_BLOCK_SIZE_EXP).unwrap();
        ncz.write_u32::<LE>(1).unwrap();
        ncz.write_i64::<LE>(0x1000_0000_0000).unwrap();
        ncz.write_u32::<LE>(1000).unwrap();
        ncz.extend_from_slice(&[0x77; 1000]);

        let mut input = Cursor::new(ncz);
        let mut output = Vec::new();
        let result = ncz_to_nca(&mut input, &mut output, &NoProgress, &CancelToken::new());
        assert!(matches!(result, Err(NxError::IncompleteSection)));
    }

    fn round_trip_with_mode(mode: NczMode, plaintext_size: usize) {
        let plaintext: Vec<u8> = (0..plaintext_size).map(|i| (i & 0xFF) as u8).collect();
        let nca_bytes = build_synthetic_nca(&plaintext);

        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca_bytes).unwrap();
        tmp.flush().unwrap();

        let file = Arc::new(File::open(tmp.path()).unwrap());
        let keys = synthetic_keyset();
        let walker = NcaWalker::open(file, 0, nca_bytes.len() as u64, &keys).unwrap();

        let mut ncz_blob_cursor = Cursor::new(Vec::new());
        nca_to_ncz(
            &walker,
            &mut ncz_blob_cursor,
            NcaToNczOptions { mode, level: 3 },
            &NoProgress,
        )
        .unwrap();
        let ncz_blob = ncz_blob_cursor.into_inner();

        let mut cur = Cursor::new(&ncz_blob);
        let mut recovered = Vec::new();
        ncz_to_nca(&mut cur, &mut recovered, &NoProgress, &CancelToken::new()).unwrap();
        assert_eq!(recovered.len(), nca_bytes.len(), "size mismatch");
        let mismatch = recovered.iter().zip(&nca_bytes).position(|(a, b)| a != b);
        if let Some(p) = mismatch {
            panic!(
                "first mismatch at byte 0x{p:X} (recovered=0x{:02X} expected=0x{:02X})",
                recovered[p], nca_bytes[p]
            );
        }
    }

    #[test]
    fn round_trip_solid_small() {
        round_trip_with_mode(NczMode::Solid, 0x10000);
    }

    #[test]
    fn round_trip_block_aligned() {
        round_trip_with_mode(NczMode::Block { size_exp: 14 }, 0x40000);
    }

    #[test]
    fn round_trip_block_unaligned() {
        round_trip_with_mode(NczMode::Block { size_exp: 14 }, 0x40200);
    }

    /// nsz has written NCZBLOCK version 2 / type 1 since the format's
    /// first commit; strict third-party readers may reject other values.
    #[test]
    fn block_mode_emits_nsz_version_and_type() {
        use crate::nintendo::nx::ncz::header::read_headers;
        use std::io::{Seek, SeekFrom};

        let plaintext: Vec<u8> = (0..0x40000).map(|i| (i & 0xFF) as u8).collect();
        let nca_bytes = build_synthetic_nca(&plaintext);
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&nca_bytes).unwrap();
        tmp.flush().unwrap();

        let file = Arc::new(File::open(tmp.path()).unwrap());
        let keys = synthetic_keyset();
        let walker = NcaWalker::open(file, 0, nca_bytes.len() as u64, &keys).unwrap();

        let mut ncz_cur = Cursor::new(Vec::new());
        nca_to_ncz(
            &walker,
            &mut ncz_cur,
            NcaToNczOptions {
                mode: NczMode::Block { size_exp: 14 },
                level: 3,
            },
            &NoProgress,
        )
        .unwrap();

        let mut cur = Cursor::new(ncz_cur.into_inner());
        cur.seek(SeekFrom::Start(NCA_PREFIX_SIZE as u64)).unwrap();
        let parsed = read_headers(&mut cur).unwrap();
        let block = parsed.block.expect("block-mode NCZ must carry NCZBLOCK");
        assert_eq!(block.version, 2);
        assert_eq!(block.kind, 1);
    }

    fn build_synthetic_nsp(nca_bytes: &[u8]) -> Vec<u8> {
        let specs = vec![
            ("game.nca".to_string(), nca_bytes.len() as u64),
            ("ticket.tik".to_string(), 16),
        ];
        let hdr = pfs0::build_header(&specs, &pfs0::Pfs0LayoutHints::default()).unwrap();
        let mut out = hdr.bytes;
        out.extend_from_slice(nca_bytes);
        out.extend_from_slice(&[0xAB; 16]);
        out
    }

    #[test]
    fn nsp_round_trip_through_files() {
        let nca = build_synthetic_nca(&(0..0x40200).map(|i| (i & 0xFF) as u8).collect::<Vec<_>>());
        let nsp_blob = build_synthetic_nsp(&nca);

        let dir = tempfile::tempdir().unwrap();
        let nsp_path = dir.path().join("game.nsp");
        let nsz_path = dir.path().join("game.nsz");
        let recovered_path = dir.path().join("recovered.nsp");
        fs::write(&nsp_path, &nsp_blob).unwrap();

        let keys = synthetic_keyset();
        compress_container(
            &nsp_path,
            &nsz_path,
            NxCompressOptions {
                level: 3,
                mode: NczMode::Solid,
            },
            &keys,
            &NoProgress,
            None,
        )
        .unwrap();

        crate::nintendo::nx::decompress::decompress_container(
            &nsz_path,
            &recovered_path,
            &keys,
            &NoProgress,
            None,
        )
        .unwrap();

        let recovered = fs::read(&recovered_path).unwrap();
        let original_sha = Sha256::digest(&nsp_blob);
        let recovered_sha = Sha256::digest(&recovered);
        assert_eq!(
            original_sha.as_slice(),
            recovered_sha.as_slice(),
            "round trip lost bytes (orig={}, rec={})",
            nsp_blob.len(),
            recovered.len()
        );
    }
}
