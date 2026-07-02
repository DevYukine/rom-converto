use crate::nintendo::ctr::z3ds::decompress_worker::{
    Z3dsDecompressWork, Z3dsDecompressedFrame, decompress_frames, make_z3ds_decompress_workers,
    plan_decompress_work,
};
use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::models::Z3dsHeader;
use crate::util::worker_pool::{Pool, parallelism};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel};
use binrw::BinRead;
use log::info;
use std::io::{BufWriter, Cursor, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::task;

/// A sibling temp path so an interrupted write never lands on the final
/// name.
pub(super) fn scratch_output_path(output: &Path) -> std::path::PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    output.with_file_name(name)
}

pub async fn decompress_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Z3dsResult<()> {
    decompress_rom_cancellable(input, output, progress, CancelToken::new()).await
}

/// Like [`decompress_rom`] but observes `cancel` at every frame boundary;
/// on cancel the partial output is removed.
pub async fn decompress_rom_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Z3dsResult<()> {
    // Header parse needs only the first 0x20 bytes; the rest is
    // passed through the blocking task that runs the worker pool.
    let (underlying_size_mb, total_work, uncompressed_size) = {
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
            header.uncompressed_size,
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

    // Atomic counter relaying progress out of the blocking thread,
    // same pattern as compress_rom.
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_clone = bytes_done.clone();

    let write_path = scratch_output_path(output);
    let input_owned = input.to_path_buf();
    let write_owned = write_path.clone();
    let cancel_bg = cancel.clone();

    let handle = task::spawn_blocking(move || -> Z3dsResult<u64> {
        // Re-open the file to read the header inside the blocking
        // task and pick up the payload offset + compressed size.
        // Keeping the header read in both places is cheap (32
        // bytes) and avoids shipping the struct across the await.
        let mut header_file = std::fs::File::open(&input_owned)?;
        let mut header_buf = vec![0u8; 0x20];
        header_file.read_exact(&mut header_buf)?;
        let header = Z3dsHeader::read(&mut Cursor::new(&header_buf))?;
        drop(header_file);

        let payload_offset = header.header_size as u64 + header.metadata_size as u64;
        let compressed_size = header.compressed_size;
        let uncompressed_size = header.uncompressed_size;

        // Open the compressed file behind an Arc<File> so every
        // worker can pread from it concurrently without fighting
        // over a shared cursor.
        let in_file = Arc::new(std::fs::File::open(&input_owned)?);

        // Plan: parse the seek table via two small positional reads
        // and produce one work item per frame with an absolute
        // offset into `in_file`.
        let work_items = plan_decompress_work(&in_file, payload_offset, compressed_size)?;

        // Progress accounting: `progress.start` was called with
        // `compressed_size + uncompressed_size`, so the bar reaches
        // 100 % only if both halves get ticked. The parallel driver
        // ticks one `uncompressed_size` per frame from its consume
        // closure; the whole compressed_size (frames +
        // seek table) is pre-ticked here, since the reads that produce them have
        // effectively already happened as workers pread their own
        // frames.
        bytes_done_clone.fetch_add(compressed_size, Ordering::Relaxed);

        let out_file = std::fs::File::create(&write_owned)?;
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

        let n_threads = parallelism();
        let workers = make_z3ds_decompress_workers(n_threads, &in_file)?;
        let pool: Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> = Pool::spawn(workers);

        decompress_frames(
            &pool,
            &mut writer,
            work_items,
            &bytes_done_clone,
            &cancel_bg,
        )?;

        pool.shutdown();
        writer
            .into_inner()
            .map_err(|e| std::io::Error::other(format!("flush decompress output: {e}")))?
            .sync_all()?;

        Ok(uncompressed_size)
    });

    let cleanup = {
        let write_path = write_path.clone();
        move || -> Z3dsError {
            let _ = std::fs::remove_file(&write_path);
            Z3dsError::Cancelled
        }
    };
    let actual_size =
        match await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await {
            Ok(size) => size,
            Err(err) => {
                let _ = tokio::fs::remove_file(&write_path).await;
                return Err(err);
            }
        };

    if actual_size != uncompressed_size {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(Z3dsError::DecompressedSizeMismatch {
            expected: uncompressed_size,
            actual: actual_size,
        });
    }
    tokio::fs::rename(&write_path, output).await?;

    info!(
        "Decompressed {} -> {} ({:.2} MB)",
        input.display(),
        output.display(),
        actual_size as f64 / BYTES_PER_MB,
    );

    Ok(())
}
