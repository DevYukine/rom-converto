use crate::cd::{CD_HUNK_BYTES, FRAME_SIZE, SECTOR_SIZE};
use crate::chd::bin::BinReader;
use crate::chd::cue::CueParser;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::models::{CHD_METADATA_TAG_CD, SHA1_BYTES};
use crate::chd::reader::ChdReader;
use crate::chd::reader::cue_generator::{generate_cue_sheet, parse_chd_track_metadata};
use crate::chd::writer::ChdWriter;
use crate::chd::writer::metadata::MetadataHash;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, info};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

mod bin;
pub mod compression;
mod cue;
mod error;
pub(crate) mod map;
mod models;
pub(crate) mod reader;
pub(crate) mod writer;

const BYTES_PER_MB: f64 = 1_000_000.0;
const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})";

pub async fn convert_to_chd(
    pb: MultiProgress,
    cue_path: PathBuf,
    output_path: PathBuf,
    force: bool,
) -> ChdResult<()> {
    // Check if output exists
    if fs::metadata(&output_path).await.is_ok() && !force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    debug!("Parsing CUE file: {:?}", cue_path);
    let parser = CueParser::new(&cue_path);
    let cue_sheet = parser.parse().await?;

    // Find BIN file
    let bin_path = if cue_sheet.files.is_empty() {
        return Err(ChdError::NoFileReferencedInCueSheet);
    } else {
        let cue_dir = cue_path.parent().unwrap_or(std::path::Path::new("."));
        cue_dir.join(&cue_sheet.files[0].filename)
    };

    debug!("Opening BIN file: {:?}", bin_path);
    let mut bin_reader = BinReader::new(&bin_path).await?;

    // Calculate total sectors
    let bin_size = fs::metadata(&bin_path).await?.len();
    let total_sectors: u32 = (bin_size / SECTOR_SIZE as u64)
        .try_into()
        .map_err(|_| ChdError::InvalidHunkSize)?;

    debug!("Total sectors: {}", total_sectors);
    debug!("Creating CHD file: {:?}", output_path);

    let mut writer =
        ChdWriter::create(&output_path, total_sectors, CD_HUNK_BYTES, &cue_sheet).await?;

    let total_mb = (bin_size as f64) / BYTES_PER_MB;
    let pg = pb.add(ProgressBar::new(bin_size));

    pg.set_style(
        ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)?
            .progress_chars("#>-"),
    );
    pg.set_message(format!("Compressing to CHD (~{:.2} MB)", total_mb));

    writer
        .compress_all_hunks(&mut bin_reader, total_sectors, &pg)
        .await?;

    pg.finish_and_clear();

    debug!("Finalizing CHD file...");
    writer.finalize().await?;

    // Calculate compression statistics
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
    pb: MultiProgress,
    input_path: PathBuf,
    output_path: PathBuf,
    parent_path: Option<PathBuf>,
) -> ChdResult<()> {
    debug!("Opening CHD file: {:?}", input_path);
    let mut reader = ChdReader::open_with_parent(&input_path, parent_path.as_ref()).await?;

    // Read metadata and find CHT2 (CD track) entry
    let metadata = reader.read_metadata().await?;
    let cd_meta = metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_CD)
        .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;

    let meta_str = String::from_utf8_lossy(&cd_meta.data);
    let meta_str = meta_str.trim_end_matches('\0');
    let tracks = parse_chd_track_metadata(meta_str)?;

    // Derive filenames: output_path is the CUE file, BIN has same stem
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

    // Generate CUE content
    let cue_content = generate_cue_sheet(&bin_filename, &tracks);

    debug!("Extracting to BIN: {:?}", bin_path);
    let mut bin_file = tokio::fs::File::create(&bin_path).await?;

    let hunk_count = reader.hunk_count();
    let hunk_bytes = reader.header().hunk_bytes as usize;
    let frames_per_hunk = hunk_bytes / FRAME_SIZE;
    let total_frames = (reader.header().logical_bytes / FRAME_SIZE as u64) as u32;

    let total_bin_bytes = total_frames as u64 * SECTOR_SIZE as u64;
    let pg = pb.add(ProgressBar::new(total_bin_bytes));
    pg.set_style(
        ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)?
            .progress_chars("#>-"),
    );
    let total_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    pg.set_message(format!("Extracting from CHD (~{:.2} MB)", total_mb));

    let mut frames_written: u32 = 0;

    for hunk_idx in 0..hunk_count {
        let hunk_data = reader.read_hunk(hunk_idx).await?;

        // Determine how many frames to extract from this hunk
        let remaining = total_frames - frames_written;
        let frames_in_hunk = frames_per_hunk.min(remaining as usize);

        for frame_idx in 0..frames_in_hunk {
            let offset = frame_idx * FRAME_SIZE;
            // Write only the SECTOR_SIZE (2352) bytes, skip SUBCODE_SIZE (96)
            bin_file
                .write_all(&hunk_data[offset..offset + SECTOR_SIZE])
                .await?;
            pg.inc(SECTOR_SIZE as u64);
        }

        frames_written += frames_in_hunk as u32;
    }

    bin_file.flush().await?;
    pg.finish_and_clear();

    // Write CUE file
    debug!("Writing CUE file: {:?}", cue_path);
    tokio::fs::write(&cue_path, cue_content).await?;

    let bin_size = tokio::fs::metadata(&bin_path).await?.len();
    let bin_mb = bin_size as f64 / BYTES_PER_MB;
    info!(
        "Extracted: {:.2} MB BIN + CUE from {:?}",
        bin_mb, input_path
    );

    debug!("Extraction complete!");
    Ok(())
}

pub async fn verify_chd(
    pb: MultiProgress,
    input_path: PathBuf,
    parent_path: Option<PathBuf>,
    fix: bool,
) -> ChdResult<()> {
    debug!("Opening CHD file for verification: {:?}", input_path);
    let mut reader = ChdReader::open_with_parent(&input_path, parent_path.as_ref()).await?;

    // Read metadata hashes
    let metadata_hashes = reader.read_metadata_hashes().await?;

    let hunk_count = reader.hunk_count();
    let hunk_bytes = reader.header().hunk_bytes as u64;
    let logical_bytes = reader.header().logical_bytes;

    let pg = pb.add(ProgressBar::new(logical_bytes));
    pg.set_style(
        ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)?
            .progress_chars("#>-"),
    );
    pg.set_message("Verifying CHD integrity");

    let mut raw_sha1_hasher = Sha1::new();
    let mut bytes_remaining = logical_bytes;

    for hunk_idx in 0..hunk_count {
        let hunk_data = reader.read_hunk(hunk_idx).await?;
        // Only hash up to logical_bytes (last hunk may have zero padding)
        let bytes_to_hash = (bytes_remaining).min(hunk_bytes) as usize;
        raw_sha1_hasher.update(&hunk_data[..bytes_to_hash]);
        bytes_remaining = bytes_remaining.saturating_sub(hunk_bytes);
        pg.inc(bytes_to_hash as u64);
    }

    pg.finish_and_clear();

    // Compare raw SHA1
    let computed_raw: [u8; SHA1_BYTES] = raw_sha1_hasher.finalize().into();
    let expected_raw = reader.header().raw_sha1;
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

    // Compare overall SHA1
    let computed_overall = compute_overall_sha1(computed_raw, &metadata_hashes);
    let expected_overall = reader.header().sha1;
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
    // raw_sha1 is at offset 64, sha1 at 84
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
