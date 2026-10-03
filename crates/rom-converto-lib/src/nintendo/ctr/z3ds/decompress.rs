use crate::nintendo::ctr::z3ds::decompress_worker::{
    Z3dsDecompressWork, Z3dsDecompressedFrame, decompress_frames, decompression_admission,
    digest_frames, make_z3ds_decompress_workers, plan_decompress_work, stream_decompress_frames,
    stream_decompress_to_writer,
};
use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::models::Z3dsHeader;
use crate::util::hash::{FileDigests, HashAlgo};
use crate::util::worker_pool::{Pool, parallelism};
use crate::util::{BYTES_PER_MB, CancelToken, Cancelled, ProgressReporter, run_scratch_write};
use binrw::BinRead;
use log::info;
use std::io::{BufWriter, Cursor, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Restore the `.z3ds` archive at `input` to the original ROM at
/// `output`; on cancel the partial output is removed.
pub async fn decompress_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Z3dsResult<()> {
    // Only the 0x20-byte header is needed here; the payload is handled by the
    // blocking task that runs the worker pool.
    let (underlying_size_mb, total_work) = {
        let mut f = std::fs::File::open(input)?;
        let mut header_buf = vec![0u8; 0x20];
        f.read_exact(&mut header_buf)?;
        let header = Z3dsHeader::read(&mut Cursor::new(&header_buf))?;
        if header.version != 1 {
            return Err(Z3dsError::UnsupportedVersion(header.version));
        }
        (
            header.compressed_size as f64 / BYTES_PER_MB,
            header.compressed_size + header.uncompressed_size,
        )
    };

    progress.start(
        total_work,
        &format!(
            "Decompressing {} ({:.2} MB compressed)",
            input.file_name().unwrap_or_default().to_string_lossy(),
            underlying_size_mb,
        ),
    );

    let input_owned = input.to_path_buf();
    let actual_size = run_scratch_write(
        output,
        true,
        progress,
        &cancel,
        move |out_file, bytes_done, cancel| -> Z3dsResult<u64> {
            // Re-reading the 32-byte header here is cheaper than shipping the parsed
            // struct across the await.
            let mut header_file = std::fs::File::open(&input_owned)?;
            let mut header_buf = vec![0u8; 0x20];
            header_file.read_exact(&mut header_buf)?;
            let header = Z3dsHeader::read(&mut Cursor::new(&header_buf))?;
            drop(header_file);

            let payload_offset = header.header_size as u64 + header.metadata_size as u64;
            let compressed_size = header.compressed_size;
            let uncompressed_size = header.uncompressed_size;

            // Arc<File> so every worker can pread concurrently without fighting over
            // a shared cursor.
            let in_file = Arc::new(std::fs::File::open(&input_owned)?);

            let work_items = plan_decompress_work(&*in_file, payload_offset, compressed_size)?;

            // `progress.start` was called with compressed_size + uncompressed_size,
            // so the bar only reaches 100% if both halves get ticked. The driver ticks
            // uncompressed_size per frame; the compressed half is pre-ticked here
            // because workers pread their own frames.
            bytes_done.fetch_add(compressed_size, Ordering::Relaxed);

            let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

            let Some(admission) = decompression_admission(&work_items, parallelism()) else {
                let written = stream_decompress_to_writer(
                    &in_file,
                    &work_items,
                    &mut writer,
                    &bytes_done,
                    &cancel,
                )?;
                if written != uncompressed_size {
                    return Err(Z3dsError::DecompressedSizeMismatch {
                        expected: uncompressed_size,
                        actual: written,
                    });
                }
                writer
                    .into_inner()
                    .map_err(|e| std::io::Error::other(format!("flush decompress output: {e}")))?
                    .sync_all()?;
                return Ok(uncompressed_size);
            };
            let workers = make_z3ds_decompress_workers(admission.workers, &in_file)?;
            let pool: Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> =
                Pool::spawn(workers);
            let written = decompress_frames(
                admission,
                &pool,
                &mut writer,
                work_items,
                &bytes_done,
                &cancel,
            )?;
            pool.shutdown();

            if written != uncompressed_size {
                return Err(Z3dsError::DecompressedSizeMismatch {
                    expected: uncompressed_size,
                    actual: written,
                });
            }

            writer
                .into_inner()
                .map_err(|e| std::io::Error::other(format!("flush decompress output: {e}")))?
                .sync_all()?;

            Ok(uncompressed_size)
        },
    )
    .await?;

    info!(
        "Decompressed {} -> {} ({:.2} MB)",
        input.display(),
        output.display(),
        actual_size as f64 / BYTES_PER_MB,
    );

    Ok(())
}

/// Digests a Z3DS file's decoded content in one streaming pass, with no temp
/// files: the blocking body of [`decompress_rom`] with each frame
/// folded into the hashers instead of a `BufWriter`. The returned `size_bytes`
/// is the decoded ROM size.
///
/// Synchronous, intended to run inside the caller's `spawn_blocking`. Progress
/// is relayed through the shared `bytes_done` counter.
pub fn digest_z3ds_inner(
    input: &Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> Z3dsResult<FileDigests> {
    let mut header_file = std::fs::File::open(input)?;
    let mut header_buf = vec![0u8; 0x20];
    header_file.read_exact(&mut header_buf)?;
    let header = Z3dsHeader::read(&mut Cursor::new(&header_buf))?;
    drop(header_file);
    if header.version != 1 {
        return Err(Z3dsError::UnsupportedVersion(header.version));
    }

    let payload_offset = header.header_size as u64 + header.metadata_size as u64;
    let compressed_size = header.compressed_size;
    let uncompressed_size = header.uncompressed_size;

    let in_file = Arc::new(std::fs::File::open(input)?);
    let work_items = plan_decompress_work(&*in_file, payload_offset, compressed_size)?;

    let Some(admission) = decompression_admission(&work_items, parallelism()) else {
        let mut hasher = crate::util::hash::MultiHasher::new(algos);
        let streamed_size = stream_decompress_frames(&in_file, &work_items, |chunk| {
            hasher.update(chunk);
            bytes_done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            Ok(())
        })?;
        if streamed_size != uncompressed_size {
            return Err(Z3dsError::DecompressedSizeMismatch {
                expected: uncompressed_size,
                actual: streamed_size,
            });
        }
        return Ok(hasher.finalize(streamed_size));
    };
    let workers = make_z3ds_decompress_workers(admission.workers, &in_file)?;
    let pool: Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> = Pool::spawn(workers);

    let result = digest_frames(admission, &pool, work_items, algos, bytes_done, cancel);
    pool.shutdown();
    let digests = result?;

    if digests.size_bytes != uncompressed_size {
        return Err(Z3dsError::DecompressedSizeMismatch {
            expected: uncompressed_size,
            actual: digests.size_bytes,
        });
    }

    Ok(digests)
}

#[cfg(test)]
mod tests {
    use super::super::compress::compress_rom;
    use super::super::seekable::{FrameEntry, write_seek_table};
    use super::*;
    use crate::nintendo::ctr::z3ds::models::underlying_magic;
    use crate::util::NoProgress;
    use crate::util::hash::{HashAlgo, hash_file};
    use binrw::BinWrite;

    fn fake_3dsx(size: usize) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0..4].copy_from_slice(&underlying_magic::THREEDSX);
        for (i, b) in data.iter_mut().enumerate().skip(4) {
            *b = (i % 251) as u8;
        }
        data
    }

    /// digest_z3ds_inner must equal a plain hash of the decompressed
    /// ROM. Uses a multi-frame input so the pooled per-frame fold and
    /// the seek-table path are exercised.
    #[tokio::test]
    async fn digest_z3ds_inner_matches_decompressed_hash() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("app.3dsx");
        let compressed = dir.path().join("app.z3dsx");
        let decompressed = dir.path().join("app_out.3dsx");

        let original = fake_3dsx(2 * 1024 * 1024 + 123);
        tokio::fs::write(&raw, &original).await.unwrap();

        compress_rom(
            &raw,
            &compressed,
            None,
            false,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        decompress_rom(&compressed, &decompressed, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        let algos = [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256];
        let bytes_done = Arc::new(AtomicU64::new(0));
        let inner = {
            let compressed = compressed.clone();
            tokio::task::spawn_blocking(move || {
                digest_z3ds_inner(&compressed, &algos, &bytes_done, &CancelToken::new())
            })
            .await
            .unwrap()
            .unwrap()
        };
        let direct = hash_file(&decompressed, &algos, &NoProgress, &CancelToken::new()).unwrap();
        assert_eq!(inner, direct);
        assert_eq!(inner.size_bytes, original.len() as u64);
    }

    /// Builds a .z3ds file whose single frame decodes to `actual_len`
    /// bytes while its seek table and header declare `declared_len`,
    /// so every decode comes up short.
    fn short_frame_archive(dir: &Path, actual_len: usize, declared_len: u32) -> std::path::PathBuf {
        let raw = vec![0x5Au8; actual_len];
        let frame = zstd::bulk::compress(&raw, 3).unwrap();
        let mut payload = frame.clone();
        write_seek_table(
            &mut payload,
            &[FrameEntry {
                compressed_size: frame.len() as u32,
                decompressed_size: declared_len,
            }],
        )
        .unwrap();

        let header = Z3dsHeader::new(
            underlying_magic::THREEDSX,
            0,
            payload.len() as u64,
            u64::from(declared_len),
        );
        let mut header_buf = Cursor::new(Vec::with_capacity(0x20));
        header.write(&mut header_buf).unwrap();
        let mut bytes = header_buf.into_inner();
        bytes.extend_from_slice(&payload);

        let path = dir.join("short.z3dsx");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A frame decoding below its declared size must fail the pooled
    /// path with the size-mismatch error instead of silently writing
    /// short output.
    #[tokio::test]
    async fn decompress_rom_rejects_short_frame_pooled() {
        let dir = tempfile::tempdir().unwrap();
        let compressed = short_frame_archive(dir.path(), 1024, 1536);
        let output = dir.path().join("out.3dsx");

        let err = decompress_rom(&compressed, &output, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                Z3dsError::DecompressedSizeMismatch {
                    expected: 1536,
                    actual: 1024
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    /// Same on the streaming fallback, forced by declaring a frame so
    /// large that even one in flight misses the shared memory target.
    #[tokio::test]
    async fn decompress_rom_rejects_short_frame_streamed() {
        let dir = tempfile::tempdir().unwrap();
        let declared_len: u32 = 220 * 1024 * 1024;
        let compressed = short_frame_archive(dir.path(), 64, declared_len);
        let output = dir.path().join("out.3dsx");

        let err = decompress_rom(&compressed, &output, &NoProgress, CancelToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                Z3dsError::DecompressedSizeMismatch {
                    expected: 230686720,
                    actual: 64
                }
            ),
            "unexpected error: {err:?}"
        );
    }
}
