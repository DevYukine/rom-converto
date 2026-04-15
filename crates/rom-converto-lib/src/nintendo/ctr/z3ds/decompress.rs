use crate::nintendo::ctr::z3ds::decompress_parallel::{
    Z3dsDecompressWork, Z3dsDecompressedFrame, make_z3ds_decompress_workers,
    parallel_decompress_frames, plan_decompress_work,
};
use crate::nintendo::ctr::z3ds::error::{Z3dsError, Z3dsResult};
use crate::nintendo::ctr::z3ds::models::Z3dsHeader;
use crate::util::worker_pool::{Pool, parallelism};
use crate::util::{BYTES_PER_MB, ProgressReporter};
use binrw::BinRead;
use log::info;
use std::io::{BufWriter, Cursor, Read};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::task;

pub async fn decompress_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
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

    let input_owned = input.to_path_buf();
    let output_owned = output.to_path_buf();

    let mut handle = task::spawn_blocking(move || -> Z3dsResult<u64> {
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
        // 100 % only if we tick both halves. The parallel driver
        // ticks one `uncompressed_size` per frame from its consume
        // closure; we pre-tick the whole compressed_size (frames +
        // seek table) here, since the reads that produce them have
        // effectively already happened as workers pread their own
        // frames.
        bytes_done_clone.fetch_add(compressed_size, Ordering::Relaxed);

        let out_file = std::fs::File::create(&output_owned)?;
        let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, out_file);

        let n_threads = parallelism();
        let workers = make_z3ds_decompress_workers(n_threads, &in_file)?;
        let pool: Pool<Z3dsDecompressWork, Z3dsDecompressedFrame, Z3dsError> = Pool::spawn(workers);

        parallel_decompress_frames(&pool, &mut writer, work_items, &bytes_done_clone)?;

        pool.shutdown();
        writer
            .into_inner()
            .map_err(|e| std::io::Error::other(format!("flush decompress output: {e}")))?
            .sync_all()?;

        Ok(uncompressed_size)
    });

    // Poll the background task, reporting progress every 100 ms.
    // Matches compress_rom's polling loop so the GUI sees the bar
    // advance during long runs.
    let actual_size = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                break result??;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    if actual_size != uncompressed_size {
        return Err(Z3dsError::DecompressedSizeMismatch {
            expected: uncompressed_size,
            actual: actual_size,
        });
    }

    info!(
        "Decompressed {} -> {} ({:.2} MB)",
        input.display(),
        output.display(),
        actual_size as f64 / BYTES_PER_MB,
    );

    Ok(())
}
