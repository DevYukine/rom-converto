use crate::cd::{CD_HUNK_BYTES, IO_BUFFER_SIZE, SECTOR_SIZE};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::models::{CHD_METADATA_TAG_CD, CHD_METADATA_TAG_DVD, ChdHeaderV5, SHA1_BYTES};
use crate::chd::reader::cue_generator::{generate_cue_sheet, parse_chd_track_metadata};
use crate::chd::writer::ChdWriter;
use crate::chd::writer::metadata::MetadataHash;
use crate::cue::CueParser;
use crate::util::iso9660::{DiscKind, detect_disc_kind};
use crate::util::{BYTES_PER_MB, ProgressReporter};
use log::{debug, info, warn};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub mod compression;
pub(crate) mod error;
pub mod info;
pub(crate) mod map;
mod models;
pub(crate) mod reader;
pub(crate) mod writer;

/// chdman's `createdvd` default: two 2048-byte sectors per hunk.
pub const DVD_HUNK_BYTES_DEFAULT: u32 = 4096;
/// PPSSPP serves the PSP's 2048-byte block API straight from hunks
/// and warns about anything larger, so detected PSP input defaults
/// to single-sector hunks.
pub const DVD_HUNK_BYTES_PSP: u32 = 2048;

/// Options for DVD-mode CHD creation (PS2/PSP ISO input).
#[derive(Debug, Clone, Default)]
pub struct ChdDvdOptions {
    /// Hunk size override; the default is picked per detected
    /// console ([`DVD_HUNK_BYTES_DEFAULT`] / [`DVD_HUNK_BYTES_PSP`]).
    pub hunk_size: Option<u32>,
    /// Add zstd as a third codec. Off by default: the libchdr in
    /// AetherSX2/NetherSX2 rejects CHDs that list zstd.
    pub allow_zstd: bool,
    pub force: bool,
}

/// Which CHD flavor to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscMode {
    Cd,
    Dvd,
}

/// Route a disc image to the right CHD writer: `.cue` input is
/// CD-mode, anything else is treated as a flat 2048-byte-sector image
/// and goes through DVD mode (the chdman createcd/createdvd split
/// that trips users up). `mode` overrides the extension routing.
pub async fn convert_disc_to_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    mode: Option<DiscMode>,
    opts: ChdDvdOptions,
) -> ChdResult<()> {
    let is_cue = input_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    let mode = mode.unwrap_or(if is_cue { DiscMode::Cd } else { DiscMode::Dvd });
    match mode {
        DiscMode::Cd if is_cue => {
            convert_to_chd(progress, input_path, output_path, opts.force).await
        }
        DiscMode::Cd => Err(ChdError::CdModeNeedsCue),
        DiscMode::Dvd => convert_iso_to_chd(progress, input_path, output_path, opts).await,
    }
}

/// Compress every `.cue` and `.iso` directly inside `input_dir` (top
/// level only, matching the ctr batch commands). Outputs land next to
/// their inputs with the extension replaced by `.chd`.
pub async fn convert_disc_to_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdDvdOptions,
) -> ChdResult<()> {
    let is_disc_input = |path: &std::path::Path| {
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cue") || e.eq_ignore_ascii_case("iso"))
    };

    let mut count: u64 = 0;
    let mut scan = fs::read_dir(input_dir).await?;
    while let Ok(Some(entry)) = scan.next_entry().await {
        if is_disc_input(&entry.path()) {
            count += 1;
        }
    }
    if count == 0 {
        warn!("No .cue or .iso inputs found in {}", input_dir.display());
        return Ok(());
    }

    total_progress.start(count, &format!("Compressing {count} discs..."));

    let mut entries = fs::read_dir(input_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !is_disc_input(&path) {
            continue;
        }
        let output = path.with_extension("chd");
        if let Err(err) =
            convert_disc_to_chd(progress, path.clone(), output, None, opts.clone()).await
        {
            warn!("Failed to compress {}: {err}", path.display());
        }
        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

/// Compress a 2048-byte-sector ISO (PS2 DVD, PSP UMD) to a DVD-mode
/// CHD, the equivalent of `chdman createdvd`.
pub async fn convert_iso_to_chd(
    progress: &dyn ProgressReporter,
    iso_path: PathBuf,
    output_path: PathBuf,
    opts: ChdDvdOptions,
) -> ChdResult<()> {
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    let iso_size = fs::metadata(&iso_path).await?.len();

    let detect_path = iso_path.clone();
    let kind = tokio::task::spawn_blocking(move || detect_disc_kind(&detect_path)).await??;
    debug!("Detected disc kind: {:?}", kind);
    if kind == DiscKind::Ps2Cd {
        warn!(
            "{:?} looks like a CD-media PS2 game; if the original disc had audio \
             tracks, convert from its bin/cue instead so they survive",
            iso_path
        );
    }

    let hunk_size = opts.hunk_size.unwrap_or(match kind {
        DiscKind::Psp => DVD_HUNK_BYTES_PSP,
        _ => DVD_HUNK_BYTES_DEFAULT,
    });

    let total_mb = iso_size as f64 / BYTES_PER_MB;
    progress.start(
        iso_size,
        &format!("Compressing to CHD (~{:.2} MB)", total_mb),
    );

    let iso_owned = iso_path.clone();
    let output_owned = output_path.clone();
    let allow_zstd = opts.allow_zstd;
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let iso_file = std::fs::File::open(&iso_owned)?;
        let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

        let mut writer = ChdWriter::create_dvd(&output_owned, iso_size, hunk_size, allow_zstd)?;
        writer.compress_all_hunks_dvd(&mut iso_reader, &bytes_done_bg)?;
        writer.finalize()?;
        Ok(())
    });

    crate::util::await_with_progress(progress, &bytes_done, handle).await?;

    let chd_size = fs::metadata(&output_path).await?.len();
    let compression_ratio = (chd_size as f64 / iso_size as f64) * 100.0;
    info!(
        "Original: {:.2} MB, CHD: {:.2} MB ({:.1}% compression ratio)",
        total_mb,
        chd_size as f64 / BYTES_PER_MB,
        compression_ratio
    );
    Ok(())
}

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

    // Peek at the header + metadata so the output type (DVD iso vs
    // CD bin/cue) is known and the progress bar can size itself
    // before the big spawn_blocking kicks off. `total_frames` comes
    // from the CHT2 track metadata, not from `header.logical_bytes`:
    // chdman rounds logical_bytes up to a full hunk boundary, so it
    // can overstate the real sector count by up to
    // `frames_per_hunk - 1`.
    let input_for_peek = input_path.clone();
    let (header, total_frames, is_dvd) =
        tokio::task::spawn_blocking(move || -> ChdResult<(ChdHeaderV5, u32, bool)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            if handle
                .metadata
                .iter()
                .any(|m| m.tag == CHD_METADATA_TAG_DVD)
            {
                return Ok((handle.header, 0, true));
            }
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
            Ok((handle.header, total_frames, false))
        })
        .await??;

    if is_dvd {
        return extract_dvd_iso(progress, input_path, output_path, header.logical_bytes).await;
    }

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
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, extract_hunks, make_chd_extract_workers,
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

        let extract_result = extract_hunks(
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

/// DVD extract path: one flat `.iso`, no cue sheet.
async fn extract_dvd_iso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    logical_bytes: u64,
) -> ChdResult<()> {
    let iso_path = if output_path.extension().is_some() {
        output_path.clone()
    } else {
        output_path.with_extension("iso")
    };

    let total_mb = logical_bytes as f64 / BYTES_PER_MB;
    progress.start(
        logical_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    let input_owned = input_path.clone();
    let iso_owned = iso_path.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        use crate::chd::reader::open_chd_sync;
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, extract_hunks_dvd, make_chd_dvd_extract_workers,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;
        let hunk_bytes = handle.header.hunk_bytes as usize;
        let logical_bytes = handle.header.logical_bytes;

        let iso_file = std::fs::File::create(&iso_owned)?;
        let mut iso_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, iso_file);

        let workers = make_chd_dvd_extract_workers(
            parallelism(),
            &handle.file,
            hunk_bytes,
            handle.header.compressors(),
        )?;
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(workers);

        let extract_result = extract_hunks_dvd(
            &pool,
            &handle.map,
            &mut iso_writer,
            hunk_bytes,
            logical_bytes,
            &bytes_done_bg,
        );
        pool.shutdown();
        extract_result?;

        use std::io::Write as _;
        iso_writer.flush()?;
        Ok(())
    });

    crate::util::await_with_progress(progress, &bytes_done, handle).await?;

    info!("Extracted: {:.2} MB ISO from {:?}", total_mb, input_path);
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
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, make_chd_dvd_extract_workers,
            make_chd_extract_workers, verify_hunks,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;
        let hunk_bytes = handle.header.hunk_bytes as usize;
        let logical_bytes = handle.header.logical_bytes;

        let n_threads = parallelism();
        let is_dvd = handle
            .metadata
            .iter()
            .any(|m| m.tag == CHD_METADATA_TAG_DVD);
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = if is_dvd {
            Pool::spawn(make_chd_dvd_extract_workers(
                n_threads,
                &handle.file,
                hunk_bytes,
                handle.header.compressors(),
            )?)
        } else {
            Pool::spawn(make_chd_extract_workers(
                n_threads,
                &handle.file,
                hunk_bytes,
            )?)
        };

        let mut raw_sha1_hasher = Sha1::new();
        let verify_result = verify_hunks(
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

    // SHA1 field offsets in the CHD v5 header (byte-counted from magic):
    // 8 + 4 + 4 + 16 + 8 + 8 + 8 + 4 + 4 = 64 for raw_sha1, +20 for sha1.
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

#[cfg(test)]
pub(crate) mod test_fixtures {
    /// Alternating compressible runs and xorshift noise so the codec
    /// slots and the store-raw path all appear in the map.
    pub(crate) fn mixed_iso(sectors: usize) -> Vec<u8> {
        let mut iso = vec![0u8; sectors * 2048];
        let mut state = 0xDEAD_BEEF_CAFE_1234u64;
        for (i, b) in iso.iter_mut().enumerate() {
            if (i / 4096).is_multiple_of(2) {
                *b = (i / 97) as u8;
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = state as u8;
            }
        }
        iso
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use test_fixtures::mixed_iso;

    async fn round_trip(allow_zstd: bool, hunk_size: Option<u32>) {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(11);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdDvdOptions {
                hunk_size,
                allow_zstd,
                force: false,
            },
        )
        .await
        .unwrap();

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();

        // No extension on the output: the DVD path must derive .iso.
        let out_base = dir.path().join("restored");
        extract_from_chd(&NoProgress, chd_path, out_base.clone(), None)
            .await
            .unwrap();
        let restored = std::fs::read(out_base.with_extension("iso")).unwrap();
        assert_eq!(restored, iso);
    }

    #[tokio::test]
    async fn dvd_chd_round_trips_with_default_codecs() {
        round_trip(false, None).await;
    }

    #[tokio::test]
    async fn dvd_chd_round_trips_with_zstd_and_psp_hunks() {
        round_trip(true, Some(2048)).await;
    }

    #[tokio::test]
    async fn corrupted_dvd_chd_fails_verify_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(16);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdDvdOptions::default(),
        )
        .await
        .unwrap();

        // Flip a byte inside the hunk data region (after the header
        // and metadata, before the trailing map).
        let mut chd = std::fs::read(&chd_path).unwrap();
        let data_start = 124 + 17;
        let mid = data_start + (chd.len() - data_start) / 2;
        chd[mid] ^= 0xFF;
        std::fs::write(&chd_path, &chd).unwrap();

        assert!(
            verify_chd(&NoProgress, chd_path.clone(), None, false)
                .await
                .is_err()
        );
        let out = dir.path().join("restored.iso");
        assert!(
            extract_from_chd(&NoProgress, chd_path, out, None)
                .await
                .is_err()
        );
    }

    /// Cross-checks against real chdman; set ROMCONVERTO_CHDMAN to
    /// the binary path to enable. Covers both directions: chdman
    /// createdvd output (with its huff/flac codec set) must extract
    /// and verify here, and our DVD CHD must pass chdman verify.
    #[tokio::test]
    async fn chdman_dvd_parity() {
        let Some(chdman) = std::env::var_os("ROMCONVERTO_CHDMAN") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(64);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let their_chd = dir.path().join("their.chd");
        let status = std::process::Command::new(&chdman)
            .args(["createdvd", "-i"])
            .arg(&iso_path)
            .arg("-o")
            .arg(&their_chd)
            .status()
            .expect("run chdman createdvd");
        assert!(status.success(), "chdman createdvd failed");

        verify_chd(&NoProgress, their_chd.clone(), None, false)
            .await
            .unwrap();
        let restored = dir.path().join("restored.iso");
        extract_from_chd(&NoProgress, their_chd, restored.clone(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), iso);

        let our_chd = dir.path().join("our.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            our_chd.clone(),
            ChdDvdOptions::default(),
        )
        .await
        .unwrap();
        let status = std::process::Command::new(&chdman)
            .args(["verify", "-i"])
            .arg(&our_chd)
            .status()
            .expect("run chdman verify");
        assert!(status.success(), "chdman rejected our DVD CHD");
    }
}
