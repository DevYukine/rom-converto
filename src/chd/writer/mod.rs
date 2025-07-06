mod metadata;

use crate::cd::SECTOR_SIZE;
use crate::chd::compression::cdlz::CdlzCompressor;
use crate::chd::compression::cdzl::CdZlCompressor;
use crate::chd::compression::cdzs::CdZsCompressor;
use crate::chd::compression::{ChdCompression, ChdCompressor};
use crate::chd::cue::models::CueSheet;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::models::{ChdHeaderV5, ChdVersion};
use crate::chd::writer::metadata::generate_cd_metadata;
use binrw::BinWrite;
use byteorder::{BigEndian, WriteBytesExt};
use liblzma::read::XzEncoder;
use sha1::{Digest, Sha1};
use std::io::{Cursor, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::task;

pub struct ChdWriter {
    writer: BufWriter<File>,
    header: ChdHeaderV5,
    map_entries: Vec<MapEntry>,
    current_hunk: Vec<u8>,
    hunk_index: u32,
    sha1: Sha1,
    raw_sha1: Sha1,
    compressors: Vec<Arc<dyn ChdCompressor + Send + Sync>>,
    pub map_offset: u64,
}

#[derive(Debug, Clone)]
struct MapEntry {
    compression: u8, // 0-3 for codecs, 4 for uncompressed
    length: u32,     // Compressed length
    offset: u64,     // Offset in file
    crc16: u16,      // CRC16 of compressed data
}

impl ChdWriter {
    pub async fn create(
        output_path: impl AsRef<Path>,
        total_sectors: u32,
        hunk_size: u32,
        cue_sheet: &CueSheet,
    ) -> ChdResult<Self> {
        let file = File::create(output_path).await?;
        let mut buff_writer = BufWriter::with_capacity(8 * 1024 * 1024, file); // 8 MB buffer

        let logical_bytes = total_sectors as u64 * SECTOR_SIZE as u64;
        let unit_bytes = SECTOR_SIZE as u32;

        // Set up compressors in order
        let compressors: Vec<Arc<dyn ChdCompressor + Send + Sync>> = vec![
            Arc::new(CdlzCompressor {}),
            Arc::new(CdZlCompressor {}),
            Arc::new(CdZsCompressor {}),
        ];

        const CHD_V5_HEADER_SIZE: u32 = 124; // Size of CHD v5 header

        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: compressors[0].tag_bytes(),
            compressor_1: compressors[1].tag_bytes(),
            compressor_2: compressors[2].tag_bytes(),
            compressor_3: [0; 4], // No fourth compressor in this case
            logical_bytes,
            map_offset: 0,
            meta_offset: 0,
            hunk_bytes: hunk_size,
            unit_bytes,
            raw_sha1: [0; 20],
            sha1: [0; 20],
            parent_sha1: [0; 20],
        };

        // Generate metadata
        let metadata = generate_cd_metadata(cue_sheet, total_sectors)?;

        // Write placeholder header
        let mut header_data = Cursor::new(Vec::new());
        header.write(&mut header_data)?;
        buff_writer.write_all(&header_data.into_inner()).await?;

        // Write metadata immediately after the header
        buff_writer.write_all(metadata.as_slice()).await?;

        let metadata_end_offset = header.length as u64 + metadata.len() as u64;

        Ok(Self {
            writer: buff_writer,
            header,
            map_entries: Vec::new(),
            current_hunk: Vec::with_capacity(hunk_size as usize),
            hunk_index: 0,
            sha1: Sha1::new(),
            raw_sha1: Sha1::new(),
            map_offset: metadata_end_offset,
            compressors,
        })
    }

    pub async fn write_sector(&mut self, sector_data: &[u8]) -> ChdResult<()> {
        self.raw_sha1.update(sector_data);
        self.current_hunk.extend_from_slice(sector_data);

        if self.current_hunk.len() >= self.header.hunk_bytes as usize {
            self.flush_hunk().await?;
        }

        Ok(())
    }

    async fn flush_hunk(&mut self) -> ChdResult<()> {
        if self.current_hunk.is_empty() {
            return Ok(());
        }

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
            if let Ok((compressed, idx)) = f.await? {
                if compressed.len() < best_size {
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
            }
        }

        let offset = self.writer.stream_position().await?;
        let (data_to_write, compression) = if let Some(compressed) = best_compressed {
            (compressed, best_type)
        } else {
            (self.current_hunk.clone(), ChdCompression::None)
        };

        self.writer.write_all(&data_to_write).await?;
        self.sha1.update(&data_to_write);

        // Calculate CRC16 (not CRC32!)
        let crc16 = calculate_crc16(&data_to_write);

        self.map_entries.push(MapEntry {
            compression: compression as u8,
            length: data_to_write.len() as u32,
            offset,
            crc16,
        });

        self.current_hunk.clear();
        self.hunk_index += 1;

        Ok(())
    }

    pub async fn finalize(mut self) -> ChdResult<()> {
        self.flush_hunk().await?;

        // Encode and compress the map
        let map_data = encode_map(&self.map_entries)?;
        let compressed_map = compress_map(&map_data)?;

        // Write compressed map
        let map_offset = self.map_offset;
        self.writer.write_all(&compressed_map).await?;
        self.sha1.update(&compressed_map);

        // Meta offset points to the metadata location (right after header)
        let meta_offset = self.header.length as u64; // Metadata starts immediately after header

        // Update header
        self.header.map_offset = map_offset;
        self.header.meta_offset = meta_offset;
        self.header.raw_sha1 = self.raw_sha1.finalize().into();
        self.header.sha1 = self.sha1.finalize().into();

        // Rewrite header
        self.writer.seek(SeekFrom::Start(0)).await?;
        let mut header_data = vec![0u8; 124];
        let mut cursor = Cursor::new(&mut header_data);
        self.header.write(&mut cursor)?;
        self.writer.write_all(&header_data).await?;

        self.writer.flush().await?;

        Ok(())
    }
}

// Helper functions
fn calculate_crc16(data: &[u8]) -> u16 {
    use crc::{CRC_16_IBM_SDLC, Crc};
    let crc = Crc::<u16>::new(&CRC_16_IBM_SDLC);
    crc.checksum(data)
}

fn encode_map(entries: &[MapEntry]) -> ChdResult<Vec<u8>> {
    let mut encoded = Vec::new();
    let mut cursor = Cursor::new(&mut encoded);

    // Write entry count
    WriteBytesExt::write_u32::<BigEndian>(&mut cursor, entries.len() as u32)?;

    let mut last_offset = 0u64;
    let mut last_crc = 0u16;

    for entry in entries {
        // Pack compression type and length
        let packed = (entry.compression as u32) << 24 | (entry.length & 0x0FFFFFFF);
        WriteBytesExt::write_u32::<BigEndian>(&mut cursor, packed)?;

        // Write variable-length offset delta
        let offset_delta = entry.offset - last_offset;
        write_variable_length(&mut cursor, offset_delta)?;

        // Write CRC delta
        let crc_delta = entry.crc16.wrapping_sub(last_crc);
        WriteBytesExt::write_u16::<BigEndian>(&mut cursor, crc_delta)?;

        last_offset = entry.offset;
        last_crc = entry.crc16;
    }

    Ok(encoded)
}

fn write_variable_length(writer: &mut impl Write, mut value: u64) -> ChdResult<()> {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;

        if value != 0 {
            writer.write_u8(byte | 0x80)?;
        } else {
            writer.write_u8(byte)?;
            break;
        }
    }
    Ok(())
}

fn compress_map(data: &[u8]) -> ChdResult<Vec<u8>> {
    // Compress map with LZMA
    let mut encoder = XzEncoder::new(data, 6);
    let mut compressed = Vec::new();
    std::io::Read::read_to_end(&mut encoder, &mut compressed)?;
    Ok(compressed)
}
