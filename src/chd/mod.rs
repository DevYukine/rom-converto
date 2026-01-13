use crate::cd::{FRAME_SIZE, FRAMES_PER_HUNK, SECTOR_SIZE};
use crate::chd::bin::BinReader;
use crate::chd::cue::CueParser;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::writer::ChdWriter;
use log::debug;
use std::path::PathBuf;
use tokio::fs;

mod bin;
pub mod compression;
mod cue;
mod error;
mod models;
mod writer;

pub async fn convert_to_chd(cue_path: PathBuf, output_path: PathBuf, force: bool) -> ChdResult<()> {
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
    let mut bin_reader = BinReader::new(&bin_path, &cue_sheet).await?;

    // Calculate total sectors
    let bin_size = std::fs::metadata(&bin_path)?.len();
    let total_sectors = (bin_size / SECTOR_SIZE as u64) as u32;

    debug!("Total sectors: {}", total_sectors);
    debug!("Creating CHD file: {:?}", output_path);

    const HUNK_SIZE: u32 = FRAME_SIZE as u32 * FRAMES_PER_HUNK;

    let mut writer = ChdWriter::create(&output_path, total_sectors, HUNK_SIZE, &cue_sheet).await?;

    // Process all sectors
    let progress_interval = std::cmp::max(1, total_sectors / 100);
    for lba in 0..total_sectors {
        let sector_data = bin_reader.read_sector(lba).await?;
        writer.write_sector(&sector_data).await?;

        if lba % progress_interval == 0 {
            let progress = (lba as f32 / total_sectors as f32) * 100.0;
            println!("\rProgress: {:.1}%", progress);
            std::io::Write::flush(&mut std::io::stdout())?;
        }
    }

    debug!("Finalizing CHD file...");
    writer.finalize().await?;

    debug!("Conversion complete!");
    Ok(())
}
