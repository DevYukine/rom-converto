pub(crate) mod metadata;

use crate::cd::{FRAME_SIZE, SUBCODE_SIZE};
use crate::chd::compression::cdfl::CdFlCompressor;
use crate::chd::compression::cdlz::CdlzCompressor;
use crate::chd::compression::cdzl::CdZlCompressor;
use crate::chd::compression::{ChdCompression, ChdCompressor};
use crate::chd::cue::models::CueSheet;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, compress_v5_map, crc16_ccitt};
use crate::chd::models::{CHD_V5_HEADER_SIZE, SHA1_BYTES, ChdHeaderV5, ChdVersion};
use crate::chd::writer::metadata::{MetadataHash, generate_cd_metadata};
use crate::chd::compute_overall_sha1;
use binrw::BinWrite;
use log::debug;
use sha1::{Digest, Sha1};
use std::io::{Cursor, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::task;

const ZERO_SUBCODE: [u8; SUBCODE_SIZE] = [0; SUBCODE_SIZE];
const IO_BUFFER_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct ChdWriter {
    writer: BufWriter<File>,
    header: ChdHeaderV5,
    map_entries: Vec<MapEntry>,
    current_hunk: Vec<u8>,
    raw_sha1: Sha1,
    compressors: Vec<Arc<dyn ChdCompressor + Send + Sync>>,
    metadata_hashes: Vec<MetadataHash>,
}

impl ChdWriter {
    pub async fn create(
        output_path: impl AsRef<Path>,
        total_sectors: u32,
        hunk_size: u32,
        cue_sheet: &CueSheet,
    ) -> ChdResult<Self> {
        let file = File::create(output_path).await?;
        let mut buff_writer = BufWriter::with_capacity(IO_BUFFER_BYTES, file);

        let logical_bytes = total_sectors as u64 * FRAME_SIZE as u64;
        let unit_bytes = FRAME_SIZE as u32;
        if !hunk_size.is_multiple_of(unit_bytes) {
            return Err(ChdError::InvalidHunkSize);
        }

        let compressors: Vec<Arc<dyn ChdCompressor + Send + Sync>> = vec![
            Arc::new(CdlzCompressor {}),
            Arc::new(CdZlCompressor {}),
            Arc::new(CdFlCompressor {}),
        ];

        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: compressors[0].tag_bytes(),
            compressor_1: compressors[1].tag_bytes(),
            compressor_2: compressors[2].tag_bytes(),
            compressor_3: [0; 4],
            logical_bytes,
            map_offset: 0,
            meta_offset: 0,
            hunk_bytes: hunk_size,
            unit_bytes,
            raw_sha1: [0; SHA1_BYTES],
            sha1: [0; SHA1_BYTES],
            parent_sha1: [0; SHA1_BYTES],
        };

        let metadata = generate_cd_metadata(cue_sheet, total_sectors)?;

        let mut header_data = Cursor::new(Vec::new());
        header.write(&mut header_data)?;
        buff_writer.write_all(&header_data.into_inner()).await?;

        buff_writer.write_all(metadata.bytes.as_slice()).await?;

        Ok(Self {
            writer: buff_writer,
            header,
            map_entries: Vec::new(),
            current_hunk: Vec::with_capacity(hunk_size as usize),
            raw_sha1: Sha1::new(),
            compressors,
            metadata_hashes: metadata.hashes,
        })
    }

    pub async fn write_sector(&mut self, sector_data: &[u8]) -> ChdResult<()> {
        self.raw_sha1.update(sector_data);
        self.raw_sha1.update(ZERO_SUBCODE);

        self.current_hunk.extend_from_slice(sector_data);
        self.current_hunk.extend_from_slice(&ZERO_SUBCODE);

        if self.current_hunk.len() >= self.header.hunk_bytes as usize {
            self.flush_hunk().await?;
        }

        Ok(())
    }

    async fn flush_hunk(&mut self) -> ChdResult<()> {
        if self.current_hunk.is_empty() {
            return Ok(());
        }

        let hunk_bytes = self.header.hunk_bytes as usize;
        if self.current_hunk.len() < hunk_bytes {
            self.current_hunk.resize(hunk_bytes, 0);
        } else if self.current_hunk.len() > hunk_bytes {
            return Err(ChdError::InvalidHunkSize);
        }

        let raw_crc = crc16_ccitt(&self.current_hunk);
        let mut best_compressed = None;
        let mut best_size = self.current_hunk.len();
        let mut best_type = ChdCompression::None;

        let futures: Vec<_> = self
            .compressors
            .iter()
            .enumerate()
            .map(|(idx, compressor)| {
                let compressor = compressor.clone();
                let hunk = self.current_hunk.clone();
                task::spawn_blocking(move || {
                    let compressed = compressor.compress(&hunk)?;
                    Ok::<(Vec<u8>, usize), ChdError>((compressed, idx))
                })
            })
            .collect();

        for f in futures {
            match f.await? {
                Ok((compressed, idx)) if compressed.len() < best_size => {
                    best_size = compressed.len();
                    best_compressed = Some(compressed);
                    best_type = match idx {
                        0 => ChdCompression::Codec0,
                        1 => ChdCompression::Codec1,
                        2 => ChdCompression::Codec2,
                        3 => ChdCompression::Codec3,
                        _ => ChdCompression::None,
                    };
                }
                Err(e) => debug!("Compressor failed (falling back to next): {}", e),
                _ => {}
            }
        }

        let offset = self.writer.stream_position().await?;
        let (data_to_write, compression) = if let Some(compressed) = best_compressed {
            (compressed, best_type)
        } else {
            (std::mem::take(&mut self.current_hunk), ChdCompression::None)
        };

        self.writer.write_all(&data_to_write).await?;

        self.map_entries.push(MapEntry {
            compression: compression as u8,
            length: data_to_write.len() as u32,
            offset,
            crc16: raw_crc,
        });

        self.current_hunk.clear();

        Ok(())
    }

    pub async fn finalize(mut self) -> ChdResult<()> {
        self.flush_hunk().await?;

        let map_data = compress_v5_map(
            &self.map_entries,
            self.header.hunk_bytes,
            self.header.unit_bytes,
        )?;

        let map_offset = self.writer.stream_position().await?;
        self.writer.write_all(&map_data).await?;

        let meta_offset = self.header.length as u64;
        let raw_sha1: [u8; SHA1_BYTES] = self.raw_sha1.finalize().into();

        self.header.map_offset = map_offset;
        self.header.meta_offset = meta_offset;
        self.header.raw_sha1 = raw_sha1;
        self.header.sha1 = compute_overall_sha1(raw_sha1, &self.metadata_hashes);

        self.writer.seek(SeekFrom::Start(0)).await?;
        let mut header_data = vec![0u8; CHD_V5_HEADER_SIZE as usize];
        let mut cursor = Cursor::new(&mut header_data);
        self.header.write(&mut cursor)?;
        self.writer.write_all(&header_data).await?;

        self.writer.flush().await?;

        Ok(())
    }
}

