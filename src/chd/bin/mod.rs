pub mod error;

use crate::cd::SECTOR_SIZE;
use crate::chd::bin::error::BinResult;
use crate::chd::cue::models::CueSheet;
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncSeekExt;
use tokio::io::{AsyncReadExt, BufReader};

// BIN file reader
#[derive(Debug)]
pub struct BinReader {
    reader: BufReader<File>,
    track_offsets: Vec<u64>,
}

impl BinReader {
    pub async fn new(bin_path: impl AsRef<Path>, cue_sheet: &CueSheet) -> BinResult<Self> {
        let file = File::open(bin_path).await?;
        let track_offsets = Self::calculate_track_offsets(cue_sheet).await;

        Ok(Self {
            reader: BufReader::with_capacity(8 * 1024 * 1024, file), // 8 MB buffer
            track_offsets,
        })
    }

    async fn calculate_track_offsets(cue_sheet: &CueSheet) -> Vec<u64> {
        let mut offsets = Vec::new();

        for track in &cue_sheet.tracks {
            if let Some(index) = track.indices.iter().find(|i| i.number == 1) {
                let lba = index.position.to_lba();
                offsets.push(lba as u64 * SECTOR_SIZE as u64);
            }
        }

        offsets
    }

    pub async fn read_sector(&mut self, lba: u32) -> BinResult<Vec<u8>> {
        let mut buffer = vec![0u8; SECTOR_SIZE];
        let offset = lba as u64 * SECTOR_SIZE as u64;

        self.reader.seek(std::io::SeekFrom::Start(offset)).await?;
        self.reader.read_exact(&mut buffer).await?;

        Ok(buffer)
    }
}
