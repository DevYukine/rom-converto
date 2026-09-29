//! Lossless WUD to WUX and WUX to WUD conversion.
//!
//! The writer mirrors the layout WudCompress produces: a 0x20-byte
//! header, a u32 LE physical-index table (one entry per logical
//! sector, first-occurrence order), and a sector-aligned pool holding
//! each unique 32 KiB sector once. Sectors are deduplicated by their
//! SHA-256 digest, so every byte of input is hashed exactly once.

use std::collections::HashMap;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::info;
use sha2::{Digest, Sha256};

use crate::disc::cd::IO_BUFFER_SIZE;
use crate::nintendo::wup::disc::sector_stream::{
    DiscSectorSource, SECTOR_SIZE, WUD_SINGLE_LAYER_SIZE, discover_split_parts, is_wux_file,
    open_disc, split_part_number,
};
use crate::nintendo::wup::disc::wud_reader::WudReader;
use crate::nintendo::wup::disc::wux_reader::{
    WUX_HEADER_SIZE, WUX_MAGIC_0, WUX_MAGIC_1, WuxReader,
};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::{
    Admission, Budget, Pool, PoolChannelClosed, Worker, drive, parallelism, with_writer_thread,
};
use crate::util::{BYTES_PER_MB, CancelToken, Cancelled, ProgressReporter, run_scratch_write};

/// Sectors per hash/work chunk: 32 x 32 KiB = 1 MiB.
const CHUNK_SECTORS: u64 = 32;
const CHUNK_BYTES: usize = CHUNK_SECTORS as usize * SECTOR_SIZE;

/// `data` holds whole sectors, `CHUNK_SECTORS` at most.
struct WuxHashWork {
    data: Vec<u8>,
}

struct WuxHashOut {
    data: Vec<u8>,
    hashes: Vec<[u8; 32]>,
}

struct WuxHashWorker;

impl Worker<WuxHashWork, WuxHashOut, WupError> for WuxHashWorker {
    fn process(&mut self, work: WuxHashWork) -> WupResult<WuxHashOut> {
        let hashes = work
            .data
            .as_chunks::<SECTOR_SIZE>()
            .0
            .iter()
            .map(|sector| Sha256::digest(sector).into())
            .collect();
        Ok(WuxHashOut {
            data: work.data,
            hashes,
        })
    }
}

/// Convert a raw WUD (single file or `game_part<N>.wud` split set)
/// into a WUX at `output`. Overwrites `output` when present; conflict
/// policy is the runner's job.
pub async fn wud_to_wux(
    progress: &dyn ProgressReporter,
    input: PathBuf,
    output: PathBuf,
    cancel: CancelToken,
) -> WupResult<()> {
    let peek = input.clone();
    let reader = tokio::task::spawn_blocking(move || -> WupResult<WudReader> {
        if is_wux_file(&peek) {
            return Err(WupError::AlreadyWux(peek));
        }
        WudReader::open_parts(discover_split_parts(&peek))
    })
    .await??;

    let sector_count = reader.total_sectors();
    let total = sector_count * SECTOR_SIZE as u64;
    if sector_count > u32::MAX as u64 {
        return Err(WupError::UnsupportedDiscFormat(input));
    }
    // Any game_partN.wud must chain up to the full retail image; a
    // short chain means the zip held only part1, a lone continuation
    // part, or a part is missing.
    if split_part_number(&input).is_some() && total != WUD_SINGLE_LAYER_SIZE {
        return Err(WupError::DiscTruncated {
            expected: WUD_SINGLE_LAYER_SIZE,
            actual: total,
        });
    }

    let total_mb = total as f64 / BYTES_PER_MB;
    progress.start(
        total,
        &format!("Compressing WUD to WUX (~{total_mb:.2} MB)"),
    );

    run_scratch_write(
        &output,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| {
            write_wux_blocking(reader, &write_path, sector_count, &bytes_done, &cancel)
        },
    )
    .await?;

    let out_size = tokio::fs::metadata(&output).await?.len();
    info!(
        "Original: {:.2} MB, WUX: {:.2} MB ({:.1}% compression ratio)",
        total_mb,
        out_size as f64 / BYTES_PER_MB,
        (out_size as f64 / total as f64) * 100.0
    );
    Ok(())
}

/// Restore the raw WUD behind a WUX at `output`, bypassing the
/// reader's LRU with positional pool reads.
pub async fn wux_to_wud(
    progress: &dyn ProgressReporter,
    input: PathBuf,
    output: PathBuf,
    cancel: CancelToken,
) -> WupResult<()> {
    let peek = input.clone();
    let reader =
        tokio::task::spawn_blocking(move || -> WupResult<WuxReader> { WuxReader::open(peek) })
            .await??;

    let uncompressed = reader.total_sectors() * SECTOR_SIZE as u64;
    let total_mb = uncompressed as f64 / BYTES_PER_MB;
    progress.start(
        uncompressed,
        &format!("Decompressing WUX to WUD (~{total_mb:.2} MB)"),
    );

    run_scratch_write(
        &output,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| {
            restore_wud_blocking(reader, &write_path, &bytes_done, &cancel)
        },
    )
    .await?;

    info!(
        "Decompressed: {:.2} MB WUD from {}",
        total_mb,
        input.display()
    );
    Ok(())
}

/// Exact logical size of a disc image in bytes: the WUX header's
/// uncompressed size (validated by [`WuxReader::open`]) or the chained
/// total of the WUD split parts.
pub(crate) fn logical_disc_size(path: &Path) -> WupResult<u64> {
    let disc = open_disc(path)?;
    Ok(disc.total_sectors() * SECTOR_SIZE as u64)
}

/// Blocking WUX writer: header first, then hashed deduplicated
/// sectors written past a hole the real index is patched into at the
/// end.
fn write_wux_blocking(
    mut reader: WudReader,
    output: &Path,
    sector_count: u64,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> WupResult<()> {
    let out_file = std::fs::File::create(output)?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

    let uncompressed = sector_count * SECTOR_SIZE as u64;
    let mut header = [0u8; WUX_HEADER_SIZE as usize];
    header[0..4].copy_from_slice(&WUX_MAGIC_0.to_le_bytes());
    header[4..8].copy_from_slice(&WUX_MAGIC_1.to_le_bytes());
    header[8..12].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes());
    header[16..24].copy_from_slice(&uncompressed.to_le_bytes());
    writer.write_all(&header)?;

    let index_len = sector_count as usize * 4;
    let pool_offset = (WUX_HEADER_SIZE + index_len as u64).next_multiple_of(SECTOR_SIZE as u64);
    // The index hole reads as zeros until the real table is patched
    // in below.
    writer.seek(SeekFrom::Start(pool_offset))?;

    let jobs = sector_count.div_ceil(CHUNK_SECTORS);
    let admission = Budget {
        codec_per_worker: 0,
        per_job: CHUNK_BYTES,
        writer_slot: CHUNK_BYTES,
        fixed: 0,
    }
    .admit(parallelism(), jobs)
    .unwrap_or(Admission::DEGRADED);

    let workers: Vec<WuxHashWorker> = (0..admission.workers).map(|_| WuxHashWorker).collect();
    let pool: Pool<WuxHashWork, WuxHashOut, WupError> = Pool::spawn(workers);

    let mut index = Vec::with_capacity(sector_count as usize);
    let mut dedup: HashMap<[u8; 32], u32> = HashMap::new();
    let mut physical_count: u32 = 0;

    let writer_result = with_writer_thread(
        &mut writer,
        admission.writer_capacity,
        WupError::WorkerPoolPanic,
        |write_tx| {
            drive(
                &pool,
                jobs,
                admission.max_in_flight,
                |seq| -> WupResult<WuxHashWork> {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    let first = seq * CHUNK_SECTORS;
                    let count = (sector_count - first).min(CHUNK_SECTORS) as usize;
                    let mut data = vec![0u8; count * SECTOR_SIZE];
                    for (sector, sector_index) in data
                        .as_chunks_mut::<SECTOR_SIZE>()
                        .0
                        .iter_mut()
                        .zip(first..)
                    {
                        reader.read_sector(sector_index, sector)?;
                    }
                    bytes_done.fetch_add(data.len() as u64, Ordering::Relaxed);
                    Ok(WuxHashWork { data })
                },
                |_seq, mut out: WuxHashOut| -> WupResult<()> {
                    // Dedup sector by sector so repeats inside one chunk
                    // collapse too; new sectors compact to the front of
                    // out.data and leave in one write per chunk.
                    let mut kept = 0usize;
                    for (i, hash) in out.hashes.iter().enumerate() {
                        let physical = *dedup.entry(*hash).or_insert_with(|| {
                            let src = i * SECTOR_SIZE;
                            out.data
                                .copy_within(src..src + SECTOR_SIZE, kept * SECTOR_SIZE);
                            kept += 1;
                            physical_count += 1;
                            physical_count - 1
                        });
                        index.push(physical);
                    }
                    out.data.truncate(kept * SECTOR_SIZE);
                    if kept > 0 {
                        write_tx
                            .send(out.data)
                            .map_err(|_| WupError::WorkerPoolClosed(PoolChannelClosed))?;
                    }
                    Ok(())
                },
            )
        },
    );
    pool.shutdown();
    writer_result?;

    writer.seek(SeekFrom::Start(WUX_HEADER_SIZE))?;
    for entry in &index {
        writer.write_all(&entry.to_le_bytes())?;
    }
    writer.flush()?;
    Ok(())
}

/// Blocking WUD restore: positional reads straight from the physical
/// pool, one reused chunk buffer, cancel checked per chunk.
fn restore_wud_blocking(
    reader: WuxReader,
    output: &Path,
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> WupResult<()> {
    let out_file = std::fs::File::create(output)?;
    let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

    let logical_count = reader.total_sectors();
    let index_table = reader.index_table();
    let pool_offset = reader.pool_offset();
    let file = reader.file();

    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut done: u64 = 0;
    while done < logical_count {
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }
        let count = (logical_count - done).min(CHUNK_SECTORS) as usize;
        for (slot, &physical) in buf
            .as_chunks_mut::<SECTOR_SIZE>()
            .0
            .iter_mut()
            .zip(&index_table[done as usize..done as usize + count])
        {
            file_read_exact_at(
                file,
                slot,
                pool_offset + physical as u64 * SECTOR_SIZE as u64,
            )?;
        }
        writer.write_all(&buf[..count * SECTOR_SIZE])?;
        done += count as u64;
        bytes_done.fetch_add((count * SECTOR_SIZE) as u64, Ordering::Relaxed);
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::disc::wux_reader::write_wux_for_test;
    use crate::util::NoProgress;

    /// 40 sectors: crosses the 32-sector chunk boundary, duplicates
    /// inside one chunk (2, 5 and the zeros 21, 22) and across chunks
    /// (9, 37 and the zero 35 matching 21, 22).
    fn fixture_sectors() -> Vec<[u8; SECTOR_SIZE]> {
        let mut sectors: Vec<[u8; SECTOR_SIZE]> = (1u8..=40).map(|v| [v; SECTOR_SIZE]).collect();
        sectors[5] = sectors[2];
        sectors[21] = [0u8; SECTOR_SIZE];
        sectors[22] = [0u8; SECTOR_SIZE];
        sectors[35] = [0u8; SECTOR_SIZE];
        sectors[37] = sectors[9];
        sectors
    }

    fn write_wud(path: &Path, sectors: &[[u8; SECTOR_SIZE]]) {
        let mut f = std::fs::File::create(path).unwrap();
        for sector in sectors {
            f.write_all(sector).unwrap();
        }
    }

    #[tokio::test]
    async fn round_trip_matches_reference_layout() {
        let dir = tempfile::tempdir().unwrap();
        let sectors = fixture_sectors();
        let wud_path = dir.path().join("game.wud");
        write_wud(&wud_path, &sectors);
        let raw = std::fs::read(&wud_path).unwrap();

        let wux_path = dir.path().join("game.wux");
        wud_to_wux(
            &NoProgress,
            wud_path.clone(),
            wux_path.clone(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let oracle_path = dir.path().join("oracle.wux");
        write_wux_for_test(&oracle_path, &sectors).unwrap();
        assert_eq!(
            std::fs::read(&wux_path).unwrap(),
            std::fs::read(&oracle_path).unwrap()
        );

        let restored_path = dir.path().join("restored.wud");
        wux_to_wud(
            &NoProgress,
            wux_path.clone(),
            restored_path.clone(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&restored_path).unwrap(), raw);
    }

    #[tokio::test]
    async fn wud_to_wux_rejects_wux_input() {
        let dir = tempfile::tempdir().unwrap();
        let wux_path = dir.path().join("game.wux");
        write_wux_for_test(&wux_path, &[[0xAA; SECTOR_SIZE]]).unwrap();
        let out_path = dir.path().join("out.wux");

        let err = wud_to_wux(&NoProgress, wux_path, out_path.clone(), CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, WupError::AlreadyWux(_)));
        assert!(!out_path.exists());
    }

    #[tokio::test]
    async fn wud_to_wux_rejects_incomplete_part1_set() {
        let dir = tempfile::tempdir().unwrap();
        let part1_path = dir.path().join("game_part1.wud");
        write_wud(&part1_path, &[[0x11; SECTOR_SIZE], [0x22; SECTOR_SIZE]]);
        let out_path = dir.path().join("out.wux");

        let err = wud_to_wux(
            &NoProgress,
            part1_path,
            out_path.clone(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            WupError::DiscTruncated {
                expected: WUD_SINGLE_LAYER_SIZE,
                ..
            }
        ));
        assert!(!out_path.exists());
    }

    #[test]
    fn logical_disc_size_reads_wux_header_and_wud_parts() {
        let dir = tempfile::tempdir().unwrap();
        let wux_path = dir.path().join("game.wux");
        write_wux_for_test(&wux_path, &[[0xAA; SECTOR_SIZE], [0xBB; SECTOR_SIZE]]).unwrap();
        assert_eq!(
            logical_disc_size(&wux_path).unwrap(),
            2 * SECTOR_SIZE as u64
        );

        let wud_path = dir.path().join("game.wud");
        write_wud(&wud_path, &fixture_sectors());
        assert_eq!(
            logical_disc_size(&wud_path).unwrap(),
            40 * SECTOR_SIZE as u64
        );
    }
}
