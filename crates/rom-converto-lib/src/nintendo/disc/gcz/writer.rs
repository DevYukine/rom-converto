//! Streaming GCZ compression with bounded parallel block encoding.

use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use binrw::BinWrite;
use flate2::{Compress, Compression, FlushCompress, Status};

use super::error::{GczError, GczResult};
use super::format::{GCZ_HEADER_SIZE, GCZ_MAGIC, GCZ_UNCOMPRESSED_FLAG, GczHeader, adler32};
use crate::nintendo::disc::input::{disc_size_of, open_disc_input};
use crate::nintendo::{dol::is_gamecube, rvl::is_wii};
use crate::util::worker_pool::{Budget, Pool, Worker, drive, parallelism, with_writer_thread};
use crate::util::{CancelToken, Cancelled, ProgressReporter, run_scratch_write};

/// Default GCZ block size in bytes (128 KiB).
pub const DEFAULT_BLOCK_SIZE: u32 = 0x20000;

/// Validates a GCZ writer block size.
///
/// # Errors
/// Returns an error unless the size is a power of two from 32 KiB through 2 MiB.
pub fn validate_block_size(block_size: u32) -> GczResult<()> {
    if !block_size.is_power_of_two() || !(0x8000..=0x20_0000).contains(&block_size) {
        return Err(GczError::Custom(format!(
            "invalid block size {block_size}: expected a power of two in 32 KiB..=2 MiB"
        )));
    }
    Ok(())
}

/// Compresses a logical GameCube or Wii disc from any supported container to GCZ.
///
/// # Errors
/// Returns an error for an invalid block size or disc header, an input or
/// output failure, a compression failure, or cancellation. Failed writes
/// leave no partial output.
pub async fn compress_to_gcz(
    input: &Path,
    output: &Path,
    block_size: u32,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> GczResult<()> {
    validate_block_size(block_size)?;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let input = input.to_path_buf();
    let logical_size = {
        let input = input.clone();
        tokio::task::spawn_blocking(move || disc_size_of(&input)).await??
    };
    progress.start(logical_size, "Compressing disc to GCZ");
    run_scratch_write(
        output,
        true,
        progress,
        &cancel,
        move |file, bytes_done, cancel| {
            compress_blocking(&input, file, block_size, bytes_done, &cancel)
        },
    )
    .await
}

struct BlockWork {
    data: Vec<u8>,
    this_size: u64,
}

struct EncodedBlock {
    stored: Vec<u8>,
    flag: u64,
    hash: u32,
    this_size: u64,
}

struct BlockWorker {
    compressor: Compress,
}

impl Worker<BlockWork, EncodedBlock, GczError> for BlockWorker {
    fn process(&mut self, work: BlockWork) -> GczResult<EncodedBlock> {
        self.compressor.reset();
        let mut compressed = vec![0u8; work.data.len() - 10];
        let status = self
            .compressor
            .compress(&work.data, &mut compressed, FlushCompress::Finish)
            .map_err(|e| GczError::Custom(format!("zlib compress: {e}")))?;
        let (stored, flag) = if status == Status::StreamEnd {
            compressed.truncate(self.compressor.total_out() as usize);
            (compressed, 0)
        } else {
            // The format stores a full padded block when the zlib stream
            // cannot finish within block_size - 10 bytes.
            (work.data, GCZ_UNCOMPRESSED_FLAG)
        };
        Ok(EncodedBlock {
            hash: adler32(&stored),
            stored,
            flag,
            this_size: work.this_size,
        })
    }
}

fn compress_blocking(
    input: &Path,
    output: std::fs::File,
    block_size: u32,
    bytes_done: Arc<AtomicU64>,
    cancel: &CancelToken,
) -> GczResult<()> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    let disc = open_disc_input(input)?;
    let logical_size = disc.logical_size();
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, disc);
    if logical_size < 0x80 {
        return Err(GczError::Custom("not a GameCube or Wii disc image".into()));
    }
    let mut dhead = [0u8; 0x80];
    reader.read_exact(&mut dhead)?;
    let sub_type = if is_gamecube(&dhead) {
        0
    } else if is_wii(&dhead) {
        1
    } else {
        return Err(GczError::Custom("not a GameCube or Wii disc image".into()));
    };
    reader.seek(SeekFrom::Start(0))?;
    let num_blocks = u32::try_from(logical_size.div_ceil(block_size as u64))
        .map_err(|_| GczError::Custom("disc has too many GCZ blocks".into()))?;
    let data_base = GCZ_HEADER_SIZE + num_blocks as u64 * 12;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, output);
    writer.seek(SeekFrom::Start(data_base))?;
    let mut ptrs = Vec::new();
    let mut hashes = Vec::new();
    let mut compressed_data_size = 0u64;

    let admission = Budget {
        codec_per_worker: 512 * 1024,
        per_job: block_size as usize * 2,
        writer_slot: block_size as usize,
        fixed: 8 * 1024 * 1024,
    }
    .admit(parallelism(), num_blocks as u64)
    .expect("validated GCZ blocks fit the worker memory target");
    let pool = Pool::spawn(
        (0..admission.workers)
            .map(|_| BlockWorker {
                compressor: Compress::new(Compression::new(9), true),
            })
            .collect(),
    );
    let result = with_writer_thread(
        &mut writer,
        admission.writer_capacity,
        GczError::Custom("writer thread panicked".into()),
        |tx| {
            drive(
                &pool,
                num_blocks as u64,
                admission.max_in_flight,
                |i| {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    let this_size = (logical_size - i * block_size as u64).min(block_size as u64);
                    let mut data = vec![0u8; block_size as usize];
                    reader.read_exact(&mut data[..this_size as usize])?;
                    Ok(BlockWork { data, this_size })
                },
                |_, block| {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    ptrs.push(compressed_data_size | block.flag);
                    hashes.push(block.hash);
                    compressed_data_size += block.stored.len() as u64;
                    tx.send(block.stored)
                        .map_err(|_| GczError::Custom("writer channel closed".into()))?;
                    bytes_done.fetch_add(block.this_size, Ordering::Relaxed);
                    Ok(())
                },
            )
        },
    );
    pool.shutdown();
    result?;
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    writer.seek(SeekFrom::Start(GCZ_HEADER_SIZE))?;
    for ptr in ptrs {
        writer.write_all(&ptr.to_le_bytes())?;
    }
    for hash in hashes {
        writer.write_all(&hash.to_le_bytes())?;
    }
    writer.seek(SeekFrom::Start(0))?;
    GczHeader {
        magic: GCZ_MAGIC,
        sub_type,
        compressed_data_size,
        data_size: logical_size,
        block_size,
        num_blocks,
    }
    .write(&mut writer)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::disc::gcz::reader::GczLayout;
    use crate::nintendo::disc::gcz::{GczReader, verify_gcz_blocking, verify_total};
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso;
    use crate::util::NoProgress;

    const BLOCK_SIZE: u32 = 0x8000;

    #[test]
    fn validate_block_size_accepts_bounds_and_rejects_invalid_sizes() {
        for size in [0x8000, DEFAULT_BLOCK_SIZE, 0x20_0000] {
            assert!(validate_block_size(size).is_ok());
        }
        for size in [0, 0x4000, 0xc000, 0x40_0000] {
            assert!(validate_block_size(size).is_err());
        }
    }

    async fn round_trip(iso: &[u8], sub_type: u32) {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("disc.iso");
        let output = dir.path().join("disc.gcz");
        std::fs::write(&input, iso).unwrap();
        compress_to_gcz(&input, &output, BLOCK_SIZE, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let layout = GczLayout::parse(&mut std::fs::File::open(&output).unwrap()).unwrap();
        assert_eq!(layout.header.sub_type, sub_type);
        assert_eq!(layout.header.data_size, iso.len() as u64);
        assert_eq!(layout.header.block_size, BLOCK_SIZE);
        assert_eq!(
            layout.header.num_blocks as usize,
            iso.len().div_ceil(BLOCK_SIZE as usize)
        );
        assert_eq!(
            std::fs::metadata(&output).unwrap().len(),
            GCZ_HEADER_SIZE
                + layout.header.num_blocks as u64 * 12
                + layout.header.compressed_data_size
        );
        let mut restored = Vec::new();
        GczReader::open(&output)
            .unwrap()
            .read_to_end(&mut restored)
            .unwrap();
        assert_eq!(restored, iso);
        let verified = Arc::new(AtomicU64::new(0));
        verify_gcz_blocking(&output, verified.clone(), CancelToken::new()).unwrap();
        assert_eq!(
            verified.load(Ordering::Relaxed),
            verify_total(&output).unwrap()
        );
    }

    #[tokio::test]
    async fn gamecube_aligned_and_partial_blocks_round_trip() {
        for size in [BLOCK_SIZE as usize * 3, BLOCK_SIZE as usize * 3 + 0x123] {
            round_trip(&make_fake_gamecube_iso(size), 0).await;
        }
    }

    #[tokio::test]
    async fn wii_header_and_disc_round_trip() {
        round_trip(&make_fake_wii_iso(BLOCK_SIZE as usize * 2 + 17), 1).await;
    }

    #[tokio::test]
    async fn zero_and_random_blocks_preserve_padding_and_raw_flag() {
        let block_size = BLOCK_SIZE as usize;
        let mut iso = make_fake_gamecube_iso(block_size * 4 - 3);
        iso[block_size..block_size * 2].fill(0);
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for b in &mut iso[block_size * 2..] {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        round_trip(&iso, 0).await;
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("disc.iso");
        let output = dir.path().join("disc.gcz");
        std::fs::write(&input, &iso).unwrap();
        compress_to_gcz(&input, &output, BLOCK_SIZE, &NoProgress, CancelToken::new())
            .await
            .unwrap();
        let mut file = std::fs::File::open(&output).unwrap();
        let layout = GczLayout::parse(&mut file).unwrap();
        let (_, zero_len, _) = layout.stored_extent(1).unwrap();
        assert!(zero_len > 0);
        for i in [2, 3] {
            let (off, len, compressed) = layout.stored_extent(i).unwrap();
            assert!(
                !compressed,
                "random block {i} must set the uncompressed flag"
            );
            assert_eq!(len, BLOCK_SIZE);
            if i == 3 {
                file.seek(SeekFrom::Start(off + len as u64 - 3)).unwrap();
                let mut padding = [1u8; 3];
                file.read_exact(&mut padding).unwrap();
                assert_eq!(padding, [0; 3]);
            }
        }
        let mut reader = GczReader::open(&output).unwrap();
        reader.seek(SeekFrom::Start(BLOCK_SIZE as u64)).unwrap();
        let mut zero = vec![1; block_size];
        reader.read_exact(&mut zero).unwrap();
        assert_eq!(zero, vec![0; block_size]);
        let mut random = Vec::new();
        reader.read_to_end(&mut random).unwrap();
        assert_eq!(random, iso[block_size * 2..]);
    }

    #[tokio::test]
    async fn cancelled_write_leaves_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("disc.iso");
        let output = dir.path().join("disc.gcz");
        std::fs::write(&input, make_fake_gamecube_iso(BLOCK_SIZE as usize)).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = compress_to_gcz(&input, &output, BLOCK_SIZE, &NoProgress, cancel).await;
        assert!(matches!(result, Err(GczError::Cancelled(_))));
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn cancellation_after_start_removes_scratch_output() {
        struct CancelOnStart(CancelToken);
        impl ProgressReporter for CancelOnStart {
            fn start(&self, _: u64, _: &str) {
                self.0.cancel();
            }
            fn inc(&self, _: u64) {}
            fn finish(&self) {}
        }

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("disc.iso");
        let output = dir.path().join("disc.gcz");
        std::fs::write(&input, make_fake_gamecube_iso(BLOCK_SIZE as usize)).unwrap();
        let cancel = CancelToken::new();
        let result = compress_to_gcz(
            &input,
            &output,
            BLOCK_SIZE,
            &CancelOnStart(cancel.clone()),
            cancel,
        )
        .await;
        assert!(matches!(result, Err(GczError::Cancelled(_))));
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn rejects_invalid_block_sizes_and_unrecognized_disc() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("disc.iso");
        let output = dir.path().join("disc.gcz");
        std::fs::write(&input, vec![0u8; BLOCK_SIZE as usize]).unwrap();
        for block_size in [0, 0x4000, 0x8001, 0x40_0000] {
            assert!(matches!(
                compress_to_gcz(&input, &output, block_size, &NoProgress, CancelToken::new()).await,
                Err(GczError::Custom(_))
            ));
        }
        assert!(matches!(
            compress_to_gcz(&input, &output, BLOCK_SIZE, &NoProgress, CancelToken::new()).await,
            Err(GczError::Custom(_))
        ));
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
