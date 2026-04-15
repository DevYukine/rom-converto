pub(crate) mod metadata;
pub(crate) mod parallel;

use crate::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::chd::compression::tag_to_bytes;
use crate::chd::compute_overall_sha1;
use crate::chd::cue::models::CueSheet;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, compress_v5_map};
use crate::chd::models::{CHD_V5_HEADER_SIZE, ChdHeaderV5, ChdVersion, SHA1_BYTES};
use crate::chd::writer::metadata::{MetadataHash, generate_cd_metadata};
use crate::chd::writer::parallel::{make_chd_compress_workers, parallel_compress_hunks};
use crate::util::worker_pool::{Pool, parallelism};
use binrw::BinWrite;
use sha1::{Digest, Sha1};
use std::io::{BufReader, BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Sync CHD writer. One instance is created per output file; it
/// owns the `BufWriter<File>`, the running raw SHA-1, and the map
/// entries accumulated across every hunk. The heavy compress work
/// runs through [`parallel::parallel_compress_hunks`] which drives
/// a worker pool with a dedicated writer thread.
pub struct ChdWriter {
    writer: BufWriter<std::fs::File>,
    writer_pos: u64,
    header: ChdHeaderV5,
    map_entries: Vec<MapEntry>,
    raw_sha1: Sha1,
    metadata_hashes: Vec<MetadataHash>,
}

impl ChdWriter {
    pub fn create(
        output_path: impl AsRef<Path>,
        total_sectors: u32,
        hunk_size: u32,
        cue_sheet: &CueSheet,
    ) -> ChdResult<Self> {
        let file = std::fs::File::create(output_path)?;
        let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

        let logical_bytes = total_sectors as u64 * FRAME_SIZE as u64;
        let unit_bytes = FRAME_SIZE as u32;
        if !hunk_size.is_multiple_of(unit_bytes) {
            return Err(ChdError::InvalidHunkSize);
        }

        // Fixed CD codec pack: cdlz, cdzl, cdfl. Matches chdman's
        // `createcd` default and the three codecs the writer's
        // `CdCodecSet` knows how to emit.
        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: tag_to_bytes("cdlz"),
            compressor_1: tag_to_bytes("cdzl"),
            compressor_2: tag_to_bytes("cdfl"),
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

        let mut header_buf = Cursor::new(Vec::new());
        header.write(&mut header_buf)?;
        let header_bytes = header_buf.into_inner();
        writer.write_all(&header_bytes)?;
        let mut writer_pos = header_bytes.len() as u64;

        writer.write_all(metadata.bytes.as_slice())?;
        writer_pos += metadata.bytes.len() as u64;

        Ok(Self {
            writer,
            writer_pos,
            header,
            map_entries: Vec::new(),
            raw_sha1: Sha1::new(),
            metadata_hashes: metadata.hashes,
        })
    }

    pub fn compress_all_hunks(
        &mut self,
        bin_reader: &mut BufReader<std::fs::File>,
        total_sectors: u32,
        bytes_done: &Arc<AtomicU64>,
    ) -> ChdResult<()> {
        let hunk_bytes = self.header.hunk_bytes as usize;
        let n_threads = parallelism();
        let workers = make_chd_compress_workers(n_threads, hunk_bytes)?;
        let pool: Pool<parallel::ChdCompressWork, parallel::ChdCompressedOut, ChdError> =
            Pool::spawn(workers);

        let result = parallel_compress_hunks(
            &pool,
            bin_reader,
            &mut self.writer,
            &mut self.writer_pos,
            &mut self.map_entries,
            &mut self.raw_sha1,
            total_sectors,
            hunk_bytes,
            bytes_done,
        );

        pool.shutdown();
        result
    }

    pub fn finalize(mut self) -> ChdResult<u64> {
        // Append the compressed map table right after the last
        // hunk. The map offset goes into the header on the final
        // seek-and-rewrite.
        let map_data = compress_v5_map(
            &self.map_entries,
            self.header.hunk_bytes,
            self.header.unit_bytes,
        )?;

        let map_offset = self.writer_pos;
        self.writer.write_all(&map_data)?;
        self.writer_pos += map_data.len() as u64;

        let meta_offset = self.header.length as u64;
        let raw_sha1: [u8; SHA1_BYTES] = self.raw_sha1.finalize().into();

        self.header.map_offset = map_offset;
        self.header.meta_offset = meta_offset;
        self.header.raw_sha1 = raw_sha1;
        self.header.sha1 = compute_overall_sha1(raw_sha1, &self.metadata_hashes);

        // Seek back and rewrite the header with the finalized
        // offsets and hashes. `BufWriter::seek` flushes the
        // internal buffer first, which is the one place we want
        // that behavior.
        self.writer.seek(SeekFrom::Start(0))?;
        let mut header_buf = vec![0u8; CHD_V5_HEADER_SIZE as usize];
        {
            let mut cursor = Cursor::new(&mut header_buf);
            self.header.write(&mut cursor)?;
        }
        self.writer.write_all(&header_buf)?;
        self.writer.flush()?;

        Ok(self.writer_pos)
    }
}
