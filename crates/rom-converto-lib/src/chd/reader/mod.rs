pub(crate) mod cue_generator;

use crate::cd::IO_BUFFER_SIZE;
use crate::chd::compression::cdfl::CdFlDecompressor;
use crate::chd::compression::cdlz::CdlzDecompressor;
use crate::chd::compression::cdzl::CdZlDecompressor;
use crate::chd::compression::cdzs::CdZsDecompressor;
use crate::chd::compression::{ChdDecompressor, tag_to_bytes};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{
    COMPRESSION_NONE, COMPRESSION_PARENT, COMPRESSION_SELF, MapEntry, crc16_ccitt,
    decompress_v5_map,
};
use crate::chd::models::{
    CHD_METADATA_FLAG_HASHED, CHD_V5_HEADER_SIZE, ChdHeaderV5, ChdMetadataHeader, ChdVersion,
    SHA1_BYTES,
};
use crate::chd::writer::metadata::MetadataHash;
use binrw::BinRead;
use byteorder::{BigEndian, ByteOrder};
use sha1::{Digest, Sha1};
use std::io::Cursor;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};

pub struct ChdReader {
    reader: BufReader<File>,
    header: ChdHeaderV5,
    map: Vec<MapEntry>,
    decompressors: Vec<Arc<dyn ChdDecompressor>>,
    parent: Option<Box<ChdReader>>,
}

impl ChdReader {
    pub async fn open(path: impl AsRef<std::path::Path>) -> ChdResult<Self> {
        Self::open_with_parent(path, None::<&std::path::Path>).await
    }

    pub async fn open_with_parent(
        path: impl AsRef<std::path::Path>,
        parent_path: Option<impl AsRef<std::path::Path>>,
    ) -> ChdResult<Self> {
        let file = File::open(path).await?;
        let mut reader = BufReader::with_capacity(IO_BUFFER_SIZE, file);

        let mut header_bytes = vec![0u8; CHD_V5_HEADER_SIZE as usize];
        reader.read_exact(&mut header_bytes).await?;
        let mut cursor = Cursor::new(&header_bytes);
        let header = ChdHeaderV5::read(&mut cursor)?;

        if header.version != ChdVersion::V5 {
            return Err(ChdError::UnsupportedChdVersion);
        }

        let tags = [
            header.compressor_0,
            header.compressor_1,
            header.compressor_2,
            header.compressor_3,
        ];

        let mut decompressors: Vec<Arc<dyn ChdDecompressor>> = Vec::new();
        for tag in &tags {
            if *tag == [0, 0, 0, 0] {
                continue;
            }
            let decompressor: Arc<dyn ChdDecompressor> = if *tag == tag_to_bytes("cdlz") {
                Arc::new(CdlzDecompressor)
            } else if *tag == tag_to_bytes("cdzl") {
                Arc::new(CdZlDecompressor)
            } else if *tag == tag_to_bytes("cdfl") {
                Arc::new(CdFlDecompressor)
            } else if *tag == tag_to_bytes("cdzs") {
                Arc::new(CdZsDecompressor)
            } else {
                return Err(ChdError::UnknownCompressionCodec(*tag));
            };
            decompressors.push(decompressor);
        }

        reader.seek(SeekFrom::Start(header.map_offset)).await?;
        let mut map_data = Vec::new();
        reader.read_to_end(&mut map_data).await?;

        let hunk_count = header.logical_bytes.div_ceil(header.hunk_bytes as u64) as u32;

        let map = decompress_v5_map(&map_data, hunk_count, header.hunk_bytes, header.unit_bytes)?;

        let parent = if let Some(pp) = parent_path {
            Some(Box::new(Box::pin(ChdReader::open(pp)).await?))
        } else if header.parent_sha1 != [0u8; SHA1_BYTES] {
            // Header references a parent but none was provided.
            log::warn!(
                "CHD references a parent (SHA1: {}), but no parent file was provided. Parent hunk references will fail.",
                hex::encode(header.parent_sha1)
            );
            None
        } else {
            None
        };

        Ok(Self {
            reader,
            header,
            map,
            decompressors,
            parent,
        })
    }

    pub async fn read_hunk(&mut self, hunk_index: u32) -> ChdResult<Vec<u8>> {
        let entry = &self.map[hunk_index as usize];
        let compression = entry.compression;
        let hunk_bytes = self.header.hunk_bytes as usize;

        match compression {
            0..=3 => {
                // Compressed with codec at index `compression`
                let codec_index = compression as usize;
                if codec_index >= self.decompressors.len() {
                    return Err(ChdError::UnknownCompressionCodec([compression, 0, 0, 0]));
                }

                self.reader.seek(SeekFrom::Start(entry.offset)).await?;
                let mut compressed = vec![0u8; entry.length as usize];
                self.reader.read_exact(&mut compressed).await?;

                let decompressor = self.decompressors[codec_index].clone();
                let decompressed = tokio::task::spawn_blocking(move || {
                    decompressor.decompress(&compressed, hunk_bytes)
                })
                .await??;

                if decompressed.len() != hunk_bytes {
                    return Err(ChdError::DecompressionSizeMismatch {
                        expected: hunk_bytes,
                        actual: decompressed.len(),
                    });
                }

                // Verify CRC16
                let computed_crc = crc16_ccitt(&decompressed);
                if computed_crc != entry.crc16 {
                    return Err(ChdError::HunkCrcMismatch {
                        hunk: hunk_index,
                        expected: entry.crc16,
                        actual: computed_crc,
                    });
                }

                Ok(decompressed)
            }
            COMPRESSION_NONE => {
                self.reader.seek(SeekFrom::Start(entry.offset)).await?;
                let mut data = vec![0u8; hunk_bytes];
                self.reader.read_exact(&mut data).await?;

                // Verify CRC16
                let computed_crc = crc16_ccitt(&data);
                if computed_crc != entry.crc16 {
                    return Err(ChdError::HunkCrcMismatch {
                        hunk: hunk_index,
                        expected: entry.crc16,
                        actual: computed_crc,
                    });
                }

                Ok(data)
            }
            COMPRESSION_SELF => {
                // entry.offset is the referenced hunk number
                let ref_hunk = entry.offset as u32;
                // Use Box::pin to allow recursive async call
                Box::pin(self.read_hunk(ref_hunk)).await
            }
            COMPRESSION_PARENT => {
                let parent = self
                    .parent
                    .as_mut()
                    .ok_or(ChdError::ParentChdNotSupported)?;
                // entry.offset is the unit number in the parent
                let unit_offset = entry.offset;
                let parent_hunk_bytes = parent.header().hunk_bytes as u64;
                let parent_unit_bytes = parent.header().unit_bytes as u64;
                let parent_hunk = (unit_offset * parent_unit_bytes / parent_hunk_bytes) as u32;
                Box::pin(parent.read_hunk(parent_hunk)).await
            }
            _ => Err(ChdError::MapDecompressionError),
        }
    }

    pub async fn read_metadata(&mut self) -> ChdResult<Vec<ChdMetadataHeader>> {
        let mut entries = Vec::new();
        let mut offset = self.header.meta_offset;

        while offset != 0 {
            self.reader.seek(SeekFrom::Start(offset)).await?;

            // Read header: 4 (tag) + 1 (flags) + 3 (length) + 8 (reserved) = 16 bytes
            let mut header_buf = [0u8; 16];
            self.reader.read_exact(&mut header_buf).await?;

            let tag: [u8; 4] = header_buf[0..4].try_into().unwrap();
            let flags = header_buf[4];
            let length = ((header_buf[5] as u32) << 16)
                | ((header_buf[6] as u32) << 8)
                | (header_buf[7] as u32);
            let reserved: [u8; 8] = header_buf[8..16].try_into().unwrap();

            let mut data = vec![0u8; length as usize];
            self.reader.read_exact(&mut data).await?;

            entries.push(ChdMetadataHeader {
                tag,
                flags,
                reserved,
                data,
            });

            // Next metadata offset is stored in reserved as big-endian u64
            let next_offset = BigEndian::read_u64(&reserved);
            offset = if next_offset != 0 { next_offset } else { 0 };
        }

        Ok(entries)
    }

    pub async fn read_metadata_hashes(&mut self) -> ChdResult<Vec<MetadataHash>> {
        let metadata = self.read_metadata().await?;
        let mut hashes = Vec::new();

        for entry in &metadata {
            if entry.flags & CHD_METADATA_FLAG_HASHED != 0 {
                let sha1: [u8; SHA1_BYTES] = Sha1::digest(&entry.data).into();
                hashes.push(MetadataHash {
                    tag: entry.tag,
                    sha1,
                });
            }
        }

        Ok(hashes)
    }

    pub fn header(&self) -> &ChdHeaderV5 {
        &self.header
    }

    pub fn hunk_count(&self) -> u32 {
        self.map.len() as u32
    }
}
