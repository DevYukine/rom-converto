use crate::cd::{CD_HUNK_BYTES, SECTOR_SIZE};
use crate::chd::bin::BinReader;
use crate::chd::cue::CueParser;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::writer::ChdWriter;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, info};
use std::path::PathBuf;
use tokio::fs;

mod bin;
pub mod compression;
mod cue;
mod error;
mod models;
mod writer;

const BYTES_PER_MB: f64 = 1_000_000.0;
const PROGRESS_TEMPLATE: &str =
    "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})";

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

    for lba in 0..total_sectors {
        let sector_data = bin_reader.read_sector(lba).await?;
        writer.write_sector(&sector_data).await?;
        pg.inc(SECTOR_SIZE as u64);
    }

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
