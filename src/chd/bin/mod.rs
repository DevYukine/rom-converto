pub mod error;

use crate::cd::SECTOR_SIZE;
use crate::chd::bin::error::BinResult;
use std::path::Path;
use tokio::fs::File;
use tokio::io::AsyncSeekExt;
use tokio::io::{AsyncReadExt, BufReader};

const IO_BUFFER_BYTES: usize = 8 * 1024 * 1024;

// BIN file reader
#[derive(Debug)]
pub struct BinReader {
    reader: BufReader<File>,
}

impl BinReader {
    pub async fn new(bin_path: impl AsRef<Path>) -> BinResult<Self> {
        let file = File::open(bin_path).await?;

        Ok(Self {
            reader: BufReader::with_capacity(IO_BUFFER_BYTES, file),
        })
    }

    pub async fn read_sector(&mut self, lba: u32) -> BinResult<Vec<u8>> {
        let mut buffer = vec![0u8; SECTOR_SIZE];
        let offset = lba as u64 * SECTOR_SIZE as u64;

        self.reader.seek(std::io::SeekFrom::Start(offset)).await?;
        self.reader.read_exact(&mut buffer).await?;

        Ok(buffer)
    }
}
