pub(crate) mod metadata;

const MAX_COMPRESSION_THREADS: usize = 16;
const DEFAULT_COMPRESSION_THREADS: usize = 4;

use crate::cd::{FRAME_SIZE, IO_BUFFER_SIZE, SECTOR_SIZE, SUBCODE_SIZE};
use crate::chd::bin::BinReader;
use crate::chd::compression::cdfl::CdFlCompressor;
use crate::chd::compression::cdlz::CdlzCompressor;
use crate::chd::compression::cdzl::CdZlCompressor;
use crate::chd::compression::{CdCodecSet, ChdCompression, ChdCompressor};
use crate::chd::compute_overall_sha1;
use crate::chd::cue::models::CueSheet;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, compress_v5_map, crc16_ccitt};
use crate::chd::models::{CHD_V5_HEADER_SIZE, ChdHeaderV5, ChdVersion, SHA1_BYTES};
use crate::chd::writer::metadata::{MetadataHash, generate_cd_metadata};
use binrw::BinWrite;
use indicatif::ProgressBar;
use log::debug;
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::io::{Cursor, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::sync::Semaphore;
use tokio::task;

const ZERO_SUBCODE: [u8; SUBCODE_SIZE] = [0; SUBCODE_SIZE];

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
        let mut buff_writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

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

    pub async fn compress_all_hunks(
        &mut self,
        bin_reader: &mut BinReader,
        total_sectors: u32,
        progress: &ProgressBar,
    ) -> ChdResult<()> {
        let hunk_bytes = self.header.hunk_bytes as usize;
        let frames_per_hunk = hunk_bytes / FRAME_SIZE;
        let total_hunks = total_sectors.div_ceil(frames_per_hunk as u32);

        // Create a pool of persistent codec sets — one per concurrent thread.
        // The semaphore ensures we never have more tasks than codec sets.
        let num_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(MAX_COMPRESSION_THREADS))
            .unwrap_or(DEFAULT_COMPRESSION_THREADS);
        let semaphore = Arc::new(Semaphore::new(num_threads));

        let codec_pool: Arc<Vec<Mutex<CdCodecSet>>> = Arc::new(
            (0..num_threads)
                .map(|_| {
                    Mutex::new(CdCodecSet::new(hunk_bytes).expect("failed to create codec set"))
                })
                .collect::<Vec<_>>(),
        );

        struct CompressedResult {
            compressed: Vec<u8>,
            compression: u8,
            crc16: u16,
        }

        let mut pending: VecDeque<tokio::task::JoinHandle<ChdResult<CompressedResult>>> =
            VecDeque::new();
        let mut sectors_read = 0u32;

        for _hunk_idx in 0..total_hunks {
            // Read sectors for this hunk
            let sectors_in_hunk =
                frames_per_hunk.min((total_sectors - sectors_read) as usize) as u32;
            let sector_data = bin_reader
                .read_sectors(sectors_read, sectors_in_hunk)
                .await?;

            // Build hunk: interleave sectors with zero subcodes
            let mut hunk = Vec::with_capacity(hunk_bytes);
            for s in 0..sectors_in_hunk as usize {
                let start = s * SECTOR_SIZE;
                self.raw_sha1
                    .update(&sector_data[start..start + SECTOR_SIZE]);
                self.raw_sha1.update(ZERO_SUBCODE);
                hunk.extend_from_slice(&sector_data[start..start + SECTOR_SIZE]);
                hunk.extend_from_slice(&ZERO_SUBCODE);
            }
            hunk.resize(hunk_bytes, 0); // pad last hunk
            sectors_read += sectors_in_hunk;

            // Compute CRC before sending to compressor
            let crc16 = crc16_ccitt(&hunk);

            // Acquire semaphore permit — guarantees a codec set is available
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let pool = codec_pool.clone();

            // Spawn compression task using persistent codec state
            let handle = tokio::task::spawn_blocking(move || {
                let _permit = permit;

                // Find an available codec set from the pool. The semaphore ensures
                // there are always enough unlocked mutexes.
                let mut codecs = loop {
                    if let Some(guard) = pool.iter().find_map(|m| m.try_lock().ok()) {
                        break guard;
                    }
                    std::thread::yield_now();
                };

                let (data, compression) = match codecs.compress_hunk(&hunk) {
                    Ok((compressed, codec_type)) => (compressed, codec_type),
                    Err(_) => (hunk, ChdCompression::None as u8),
                };

                Ok(CompressedResult {
                    compressed: data,
                    compression,
                    crc16,
                })
            });

            pending.push_back(handle);

            // Drain completed results from front (maintain write order)
            while pending.front().is_some_and(|h| h.is_finished()) {
                let result = pending.pop_front().unwrap().await??;
                let offset = self.writer.stream_position().await?;
                self.writer.write_all(&result.compressed).await?;
                self.map_entries.push(MapEntry {
                    compression: result.compression,
                    length: result.compressed.len() as u32,
                    offset,
                    crc16: result.crc16,
                });
                progress.inc((frames_per_hunk * SECTOR_SIZE) as u64);
            }
        }

        // Drain remaining pending tasks
        while let Some(handle) = pending.pop_front() {
            let result = handle.await??;
            let offset = self.writer.stream_position().await?;
            self.writer.write_all(&result.compressed).await?;
            self.map_entries.push(MapEntry {
                compression: result.compression,
                length: result.compressed.len() as u32,
                offset,
                crc16: result.crc16,
            });
            progress.inc((frames_per_hunk * SECTOR_SIZE) as u64);
        }

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
