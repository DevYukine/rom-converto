use crate::cd::{CD_HUNK_BYTES, IO_BUFFER_SIZE, SECTOR_SIZE};
use crate::chd::cue::CueParser;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::models::{CHD_METADATA_TAG_CD, ChdHeaderV5, SHA1_BYTES};
use crate::chd::reader::cue_generator::{generate_cue_sheet, parse_chd_track_metadata};
use crate::chd::writer::ChdWriter;
use crate::chd::writer::metadata::MetadataHash;
use crate::util::{BYTES_PER_MB, ProgressReporter};
use log::{debug, info};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub mod compression;
mod cue;
mod error;
pub(crate) mod map;
mod models;
pub(crate) mod reader;
pub(crate) mod writer;

pub async fn convert_to_chd(
    progress: &dyn ProgressReporter,
    cue_path: PathBuf,
    output_path: PathBuf,
    force: bool,
) -> ChdResult<()> {
    if fs::metadata(&output_path).await.is_ok() && !force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    debug!("Parsing CUE file: {:?}", cue_path);
    let parser = CueParser::new(&cue_path);
    let cue_sheet = parser.parse().await?;

    let bin_path = if cue_sheet.files.is_empty() {
        return Err(ChdError::NoFileReferencedInCueSheet);
    } else {
        let cue_dir = cue_path.parent().unwrap_or(std::path::Path::new("."));
        cue_dir.join(&cue_sheet.files[0].filename)
    };

    debug!("Opening BIN file: {:?}", bin_path);
    let bin_size = fs::metadata(&bin_path).await?.len();
    let total_sectors: u32 = (bin_size / SECTOR_SIZE as u64)
        .try_into()
        .map_err(|_| ChdError::InvalidHunkSize)?;

    debug!("Total sectors: {}", total_sectors);
    debug!("Creating CHD file: {:?}", output_path);

    let total_mb = (bin_size as f64) / BYTES_PER_MB;
    progress.start(
        bin_size,
        &format!("Compressing to CHD (~{:.2} MB)", total_mb),
    );

    // Hand the full blocking pipeline (open bin + compress +
    // finalize) to a single `spawn_blocking` and poll a shared
    // `AtomicU64` for progress ticks. Same shape as the RVZ
    // compress entry in `nintendo/rvz/compress/mod.rs`.
    let bin_path_owned = bin_path.clone();
    let output_owned = output_path.clone();
    let cue_sheet_owned = cue_sheet.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let bin_file = std::fs::File::open(&bin_path_owned)?;
        let mut bin_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, bin_file);

        let mut writer = ChdWriter::create(
            &output_owned,
            total_sectors,
            CD_HUNK_BYTES,
            &cue_sheet_owned,
        )?;

        writer.compress_all_hunks(&mut bin_reader, total_sectors, &bytes_done_bg)?;
        writer.finalize()?;
        Ok(())
    });

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                result??;
                break;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    let chd_size = fs::metadata(&output_path).await?.len();
    let original_size = bin_size;
    let saved_bytes = original_size.saturating_sub(chd_size);
    let compression_ratio = (chd_size as f64 / original_size as f64) * 100.0;
    let saved_mb = saved_bytes as f64 / BYTES_PER_MB;
    let chd_mb = chd_size as f64 / BYTES_PER_MB;

    info!(
        "Original: {:.2} MB, CHD: {:.2} MB, Saved: {:.2} MB ({:.1}% compression ratio)",
        total_mb, chd_mb, saved_mb, compression_ratio
    );

    debug!("Conversion complete!");
    Ok(())
}

pub(crate) fn compute_overall_sha1(
    raw_sha1: [u8; SHA1_BYTES],
    metadata_hashes: &[MetadataHash],
) -> [u8; SHA1_BYTES] {
    let mut overall = Sha1::new();
    overall.update(raw_sha1);

    if !metadata_hashes.is_empty() {
        let mut hashes = metadata_hashes.to_vec();
        hashes.sort_by(|a, b| a.tag.cmp(&b.tag).then(a.sha1.cmp(&b.sha1)));
        for hash in hashes {
            overall.update(hash.tag);
            overall.update(hash.sha1);
        }
    }

    overall.finalize().into()
}

pub async fn extract_from_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    parent_path: Option<PathBuf>,
) -> ChdResult<()> {
    if parent_path.is_some() {
        return Err(ChdError::ParentChdNotSupported);
    }

    debug!("Opening CHD file: {:?}", input_path);

    // Resolve cue + bin paths.
    let cue_path = if output_path.extension().is_some() {
        output_path.clone()
    } else {
        output_path.with_extension("cue")
    };
    let bin_path = cue_path.with_extension("bin");
    let bin_filename = bin_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Peek at the header + CD metadata so the progress bar can
    // size itself before the big spawn_blocking kicks off.
    // `total_frames` comes from the CHT2 track metadata, not
    // from `header.logical_bytes`: chdman rounds logical_bytes
    // up to a full hunk boundary, so it can overstate the real
    // sector count by up to `frames_per_hunk - 1`.
    let input_for_peek = input_path.clone();
    let (_header, total_frames) =
        tokio::task::spawn_blocking(move || -> ChdResult<(ChdHeaderV5, u32)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            let cd_meta = handle
                .metadata
                .iter()
                .find(|m| m.tag == CHD_METADATA_TAG_CD)
                .ok_or_else(|| {
                    ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string())
                })?;
            let meta_str = String::from_utf8_lossy(&cd_meta.data);
            let meta_str = meta_str.trim_end_matches('\0');
            let tracks = parse_chd_track_metadata(meta_str)?;
            let total_frames: u32 = tracks.iter().map(|t| t.frames).sum();
            Ok((handle.header, total_frames))
        })
        .await??;

    let total_bin_bytes = total_frames as u64 * SECTOR_SIZE as u64;
    let total_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    progress.start(
        total_bin_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    let input_owned = input_path.clone();
    let bin_owned = bin_path.clone();
    let cue_owned = cue_path.clone();
    let bin_filename_owned = bin_filename;
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        use crate::chd::reader::open_chd_sync;
        use crate::chd::reader::parallel::{
            ChdExtractWork, ChdExtractedOut, make_chd_extract_workers, parallel_extract_hunks,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;

        let cd_meta = handle
            .metadata
            .iter()
            .find(|m| m.tag == CHD_METADATA_TAG_CD)
            .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;
        let meta_str = String::from_utf8_lossy(&cd_meta.data);
        let meta_str = meta_str.trim_end_matches('\0');
        let tracks = parse_chd_track_metadata(meta_str)?;

        let hunk_bytes = handle.header.hunk_bytes as usize;
        // Use the CHT2 `FRAMES:` sum, not `logical_bytes`; see
        // the outer peek above.
        let total_frames: u32 = tracks.iter().map(|t| t.frames).sum();

        let bin_file = std::fs::File::create(&bin_owned)?;
        let mut bin_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, bin_file);

        let n_threads = parallelism();
        let workers = make_chd_extract_workers(n_threads, &handle.file, hunk_bytes)?;
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(workers);

        let extract_result = parallel_extract_hunks(
            &pool,
            &handle.map,
            &mut bin_writer,
            hunk_bytes,
            total_frames,
            &bytes_done_bg,
        );
        pool.shutdown();
        extract_result?;

        use std::io::Write as _;
        bin_writer.flush()?;

        let cue_content = generate_cue_sheet(&bin_filename_owned, &tracks);
        std::fs::write(&cue_owned, cue_content)?;

        Ok(())
    });

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                result??;
                break;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    let bin_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    info!(
        "Extracted: {:.2} MB BIN + CUE from {:?}",
        bin_mb, input_path
    );

    debug!("Extraction complete!");
    Ok(())
}

pub async fn verify_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    parent_path: Option<PathBuf>,
    fix: bool,
) -> ChdResult<()> {
    if parent_path.is_some() {
        return Err(ChdError::ParentChdNotSupported);
    }

    debug!("Opening CHD file for verification: {:?}", input_path);

    // Peek header + metadata hashes up front so the progress bar
    // can size itself and so the fix-path (rewrite header SHA1s)
    // has a metadata snapshot to rebuild the overall hash from.
    let input_for_peek = input_path.clone();
    let (header, metadata_hashes): (ChdHeaderV5, Vec<MetadataHash>) =
        tokio::task::spawn_blocking(move || -> ChdResult<(ChdHeaderV5, Vec<MetadataHash>)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            let hashes: Vec<MetadataHash> = handle
                .metadata
                .iter()
                .filter(|m| m.flags & crate::chd::models::CHD_METADATA_FLAG_HASHED != 0)
                .map(|m| MetadataHash {
                    tag: m.tag,
                    sha1: <[u8; SHA1_BYTES]>::from(Sha1::digest(&m.data)),
                })
                .collect();
            Ok((handle.header, hashes))
        })
        .await??;

    let logical_bytes = header.logical_bytes;
    progress.start(logical_bytes, "Verifying CHD integrity");

    let input_owned = input_path.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> ChdResult<[u8; SHA1_BYTES]> {
        use crate::chd::reader::open_chd_sync;
        use crate::chd::reader::parallel::{
            ChdExtractWork, ChdExtractedOut, make_chd_extract_workers, parallel_verify_hunks,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;
        let hunk_bytes = handle.header.hunk_bytes as usize;
        let logical_bytes = handle.header.logical_bytes;

        let n_threads = parallelism();
        let workers = make_chd_extract_workers(n_threads, &handle.file, hunk_bytes)?;
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(workers);

        let mut raw_sha1_hasher = Sha1::new();
        let verify_result = parallel_verify_hunks(
            &pool,
            &handle.map,
            &mut raw_sha1_hasher,
            hunk_bytes,
            logical_bytes,
            &bytes_done_bg,
        );
        pool.shutdown();
        verify_result?;

        let computed: [u8; SHA1_BYTES] = raw_sha1_hasher.finalize().into();
        Ok(computed)
    });

    let computed_raw = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => break result??,
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

    let expected_raw = header.raw_sha1;
    if computed_raw != expected_raw {
        info!(
            "Raw SHA1 mismatch: expected {}, got {}",
            hex::encode(expected_raw),
            hex::encode(computed_raw)
        );
        if fix {
            fix_sha1(&input_path, computed_raw, &metadata_hashes).await?;
            info!("SHA1 updated to correct value in CHD file");
            return Ok(());
        }
        return Err(ChdError::Sha1Mismatch {
            expected: hex::encode(expected_raw),
            actual: hex::encode(computed_raw),
        });
    }
    info!("Raw SHA1 verification successful!");

    let computed_overall = compute_overall_sha1(computed_raw, &metadata_hashes);
    let expected_overall = header.sha1;
    if computed_overall != expected_overall {
        info!(
            "Overall SHA1 mismatch: expected {}, got {}",
            hex::encode(expected_overall),
            hex::encode(computed_overall)
        );
        if fix {
            fix_sha1(&input_path, computed_raw, &metadata_hashes).await?;
            info!("SHA1 updated to correct value in CHD file");
            return Ok(());
        }
        return Err(ChdError::Sha1Mismatch {
            expected: hex::encode(expected_overall),
            actual: hex::encode(computed_overall),
        });
    }

    info!(
        "Overall SHA1 verification successful! (SHA1: {})",
        hex::encode(computed_overall)
    );

    Ok(())
}

async fn fix_sha1(
    path: &std::path::Path,
    raw_sha1: [u8; SHA1_BYTES],
    metadata_hashes: &[MetadataHash],
) -> ChdResult<()> {
    use tokio::io::AsyncSeekExt;

    let overall_sha1 = compute_overall_sha1(raw_sha1, metadata_hashes);

    // SHA1 field offsets in the CHD v5 header:
    // 8 (magic) + 4 (length) + 4 (version) + 16 (compressors) + 8 (logical) + 8 (map_offset) + 8 (meta_offset) + 4 (hunk_bytes) + 4 (unit_bytes) = 64
    const RAW_SHA1_OFFSET: u64 = 64;
    const SHA1_OFFSET: u64 = 84;

    let mut file = tokio::fs::OpenOptions::new().write(true).open(path).await?;

    file.seek(std::io::SeekFrom::Start(RAW_SHA1_OFFSET)).await?;
    file.write_all(&raw_sha1).await?;

    file.seek(std::io::SeekFrom::Start(SHA1_OFFSET)).await?;
    file.write_all(&overall_sha1).await?;

    file.flush().await?;

    Ok(())
}
