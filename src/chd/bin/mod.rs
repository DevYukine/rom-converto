pub mod error;

use crate::cd::{IO_BUFFER_SIZE, SECTOR_SIZE};
use crate::chd::bin::error::BinResult;
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncSeekExt;
use tokio::io::{AsyncReadExt, BufReader};

// BIN file reader
#[derive(Debug)]
pub struct BinReader {
    reader: BufReader<File>,
}

impl BinReader {
    pub async fn new(bin_path: impl AsRef<Path>) -> BinResult<Self> {
        let file = File::open(bin_path).await?;

        Ok(Self {
            reader: BufReader::with_capacity(IO_BUFFER_SIZE, file),
        })
    }

    pub async fn read_sectors(&mut self, start_lba: u32, count: u32) -> BinResult<Vec<u8>> {
        let offset = start_lba as u64 * SECTOR_SIZE as u64;
        let byte_count = count as usize * SECTOR_SIZE;
        let mut buffer = vec![0u8; byte_count];
        self.reader.seek(std::io::SeekFrom::Start(offset)).await?;
        self.reader.read_exact(&mut buffer).await?;
        Ok(buffer)
    }
}
