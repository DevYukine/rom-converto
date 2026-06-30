pub(crate) mod metadata;
pub(crate) mod worker;

use crate::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::chd::compression::dvd::dvd_compressors;
use crate::chd::compression::tag_to_bytes;
use crate::chd::compute_overall_sha1;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, compress_v5_map};
use crate::chd::models::{
    CHD_V5_HEADER_SIZE, ChdHeaderV5, ChdVersion, DVD_SECTOR_SIZE, SHA1_BYTES,
};
use crate::chd::writer::metadata::{
    MetadataBlock, MetadataHash, generate_cd_metadata, generate_dvd_metadata,
};
use crate::chd::writer::worker::{
    compress_hunks, compress_hunks_dvd, make_chd_compress_workers, make_chd_dvd_compress_workers,
};
use crate::cue::models::CueSheet;
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, parallelism};
use binrw::BinWrite;
use sha1::{Digest, Sha1};
use std::io::{BufReader, BufWriter, Cursor, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Largest accepted DVD hunk: chdman never goes near this; the cap
/// only guards against absurd `--hunk-size` values.
const MAX_DVD_HUNK_BYTES: u32 = 1024 * 1024;

/// Sync CHD writer. One instance is created per output file; it
/// owns the `BufWriter<File>`, the running raw SHA-1, and the map
/// entries accumulated across every hunk. The heavy compress work
/// runs through [`worker::compress_hunks`] which drives a worker
/// pool with a dedicated writer thread.
pub struct ChdWriter {
    writer: BufWriter<std::fs::File>,
    writer_pos: u64,
    header: ChdHeaderV5,
    map_entries: Vec<MapEntry>,
    raw_sha1: Sha1,
    metadata_hashes: Vec<MetadataHash>,
}

impl ChdWriter {
    /// `total_sectors` sizes the logical data (it includes track
    /// padding frames); `data_sectors` is the real frame count the
    /// CHT2 `FRAMES:` metadata records, matching chdman.
    pub fn create(
        output_path: impl AsRef<Path>,
        total_sectors: u32,
        data_sectors: u32,
        hunk_size: u32,
        cue_sheet: &CueSheet,
    ) -> ChdResult<Self> {
        let file = std::fs::File::create(output_path)?;
        let writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

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

        let metadata = generate_cd_metadata(cue_sheet, data_sectors)?;
        Self::init(writer, header, metadata)
    }

    /// DVD-mode writer: flat 2048-byte sectors, `logical_bytes` =
    /// exact input size, `DVD ` marker metadata. `allow_zstd` adds
    /// zstd as a third codec; the default compatibility set is
    /// `[lzma, zlib]` (see [`dvd_compressors`]).
    pub fn create_dvd(
        output_path: impl AsRef<Path>,
        iso_bytes: u64,
        hunk_size: u32,
        allow_zstd: bool,
    ) -> ChdResult<Self> {
        if iso_bytes == 0 || !iso_bytes.is_multiple_of(DVD_SECTOR_SIZE as u64) {
            return Err(ChdError::IsoNotSectorAligned { size: iso_bytes });
        }
        if !(DVD_SECTOR_SIZE..=MAX_DVD_HUNK_BYTES).contains(&hunk_size)
            || !hunk_size.is_multiple_of(DVD_SECTOR_SIZE)
        {
            return Err(ChdError::InvalidHunkSize);
        }

        let file = std::fs::File::create(output_path)?;
        let writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

        let compressors = dvd_compressors(allow_zstd);
        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: compressors[0],
            compressor_1: compressors[1],
            compressor_2: compressors[2],
            compressor_3: compressors[3],
            logical_bytes: iso_bytes,
            map_offset: 0,
            meta_offset: 0,
            hunk_bytes: hunk_size,
            unit_bytes: DVD_SECTOR_SIZE,
            raw_sha1: [0; SHA1_BYTES],
            sha1: [0; SHA1_BYTES],
            parent_sha1: [0; SHA1_BYTES],
        };

        let metadata = generate_dvd_metadata()?;
        Self::init(writer, header, metadata)
    }

    fn init(
        mut writer: BufWriter<std::fs::File>,
        header: ChdHeaderV5,
        metadata: MetadataBlock,
    ) -> ChdResult<Self> {
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

    /// `total_sectors` includes track padding frames; `data_sectors`
    /// of `sector_data_size` bytes each are read from the source.
    pub fn compress_all_hunks(
        &mut self,
        bin_reader: &mut BufReader<std::fs::File>,
        total_sectors: u32,
        data_sectors: u32,
        sector_data_size: usize,
        bytes_done: &Arc<AtomicU64>,
        cancel: &CancelToken,
    ) -> ChdResult<()> {
        let hunk_bytes = self.header.hunk_bytes as usize;
        let n_threads = parallelism();
        let workers = make_chd_compress_workers(n_threads, hunk_bytes)?;
        let pool: Pool<worker::ChdCompressWork, worker::ChdCompressedOut, ChdError> =
            Pool::spawn(workers);

        let result = compress_hunks(
            &pool,
            bin_reader,
            &mut self.writer,
            &mut self.writer_pos,
            &mut self.map_entries,
            &mut self.raw_sha1,
            total_sectors,
            data_sectors,
            sector_data_size,
            hunk_bytes,
            bytes_done,
            cancel,
        );

        pool.shutdown();
        result
    }

    pub fn compress_all_hunks_dvd(
        &mut self,
        iso_reader: &mut BufReader<std::fs::File>,
        bytes_done: &Arc<AtomicU64>,
        cancel: &CancelToken,
    ) -> ChdResult<()> {
        let hunk_bytes = self.header.hunk_bytes as usize;
        let allow_zstd = self.header.compressor_2 == tag_to_bytes("zstd");
        let workers = make_chd_dvd_compress_workers(parallelism(), hunk_bytes, allow_zstd)?;
        let pool: Pool<worker::ChdCompressWork, worker::ChdCompressedOut, ChdError> =
            Pool::spawn(workers);

        let result = compress_hunks_dvd(
            &pool,
            iso_reader,
            &mut self.writer,
            &mut self.writer_pos,
            &mut self.map_entries,
            &mut self.raw_sha1,
            self.header.logical_bytes,
            hunk_bytes,
            bytes_done,
            cancel,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chd::compression::deflate_decompress;
    use crate::chd::compression::lzma::LzmaDecoder;
    use crate::chd::map::{COMPRESSION_NONE, decompress_v5_map};
    use crate::chd::models::{CHD_METADATA_TAG_DVD, DVD_SECTOR_SIZE};
    use crate::util::NoProgress;
    use crate::util::iso9660::test_fixtures::{IsoSpec, make_iso};
    use binrw::BinRead;
    use std::io::Cursor as IoCursor;
    use std::sync::atomic::Ordering;

    use crate::chd::test_fixtures::mixed_iso;

    fn write_dvd_chd(iso: &[u8], hunk_size: u32, allow_zstd: bool) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("in.iso");
        let chd_path = dir.path().join("out.chd");
        std::fs::write(&iso_path, iso).unwrap();

        let iso_file = std::fs::File::open(&iso_path).unwrap();
        let mut reader = BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);
        let mut writer =
            ChdWriter::create_dvd(&chd_path, iso.len() as u64, hunk_size, allow_zstd).unwrap();
        let bytes_done = Arc::new(AtomicU64::new(0));
        writer
            .compress_all_hunks_dvd(&mut reader, &bytes_done, &CancelToken::new())
            .unwrap();
        assert_eq!(bytes_done.load(Ordering::Relaxed), iso.len() as u64);
        writer.finalize().unwrap();

        std::fs::read(&chd_path).unwrap()
    }

    fn decode_hunks(chd: &[u8], header: &ChdHeaderV5) -> Vec<u8> {
        let hunk_bytes = header.hunk_bytes as usize;
        let hunk_count = header.logical_bytes.div_ceil(hunk_bytes as u64) as u32;
        let map_size = ((chd.len() as u64 - header.map_offset).min(u32::MAX as u64)) as usize;
        let map = decompress_v5_map(
            &chd[header.map_offset as usize..header.map_offset as usize + map_size],
            hunk_count,
            header.hunk_bytes,
            header.unit_bytes,
        )
        .unwrap();

        let mut out = Vec::new();
        let mut lzma = LzmaDecoder::new(hunk_bytes).unwrap();
        for entry in &map {
            let stored = &chd[entry.offset as usize..entry.offset as usize + entry.length as usize];
            let hunk = match entry.compression {
                0 => lzma.decompress(stored, hunk_bytes).unwrap(),
                1 => deflate_decompress(stored, hunk_bytes).unwrap(),
                2 => zstd::decode_all(stored).unwrap(),
                COMPRESSION_NONE => stored.to_vec(),
                other => panic!("unexpected compression type {other}"),
            };
            assert_eq!(hunk.len(), hunk_bytes);
            out.extend_from_slice(&hunk);
        }
        out.truncate(header.logical_bytes as usize);
        out
    }

    #[test]
    fn dvd_chd_writes_chdman_shaped_file() {
        // 11 sectors with hunk 4096 = 5 full hunks + 1 partial.
        let iso = mixed_iso(11);
        let chd = write_dvd_chd(&iso, 4096, false);

        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        assert_eq!(header.length, CHD_V5_HEADER_SIZE);
        assert_eq!(&header.compressor_0, b"lzma");
        assert_eq!(&header.compressor_1, b"zlib");
        assert_eq!(header.compressor_2, [0u8; 4]);
        assert_eq!(header.logical_bytes, iso.len() as u64);
        assert_eq!(header.hunk_bytes, 4096);
        assert_eq!(header.unit_bytes, DVD_SECTOR_SIZE);
        assert_eq!(header.meta_offset, CHD_V5_HEADER_SIZE as u64);

        let raw: [u8; SHA1_BYTES] = Sha1::digest(&iso).into();
        assert_eq!(header.raw_sha1, raw);
        let dvd_hash = MetadataHash {
            tag: CHD_METADATA_TAG_DVD,
            sha1: Sha1::digest([0u8]).into(),
        };
        assert_eq!(header.sha1, compute_overall_sha1(raw, &[dvd_hash]));

        assert_eq!(decode_hunks(&chd, &header), iso);
    }

    #[test]
    fn dvd_chd_with_zstd_round_trips() {
        let iso = mixed_iso(8);
        let chd = write_dvd_chd(&iso, 2048, true);
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        assert_eq!(&header.compressor_2, b"zstd");
        assert_eq!(decode_hunks(&chd, &header), iso);
    }

    #[test]
    fn dvd_metadata_block_matches_chdman_layout() {
        let block = generate_dvd_metadata().unwrap();
        // tag, flags, 24-bit length, 8 reserved bytes, single NUL.
        let mut expected = Vec::new();
        expected.extend_from_slice(b"DVD ");
        expected.push(0x01);
        expected.extend_from_slice(&[0, 0, 1]);
        expected.extend_from_slice(&[0u8; 8]);
        expected.push(0);
        assert_eq!(block.bytes, expected);

        assert_eq!(
            hex::encode(block.hashes[0].sha1),
            // SHA-1 of a single NUL byte.
            "5ba93c9db0cff93f52b521d7420e43f6eda2784f"
        );
    }

    #[test]
    fn create_dvd_rejects_bad_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.chd");
        assert!(matches!(
            ChdWriter::create_dvd(&out, 4096 + 1, 4096, false),
            Err(ChdError::IsoNotSectorAligned { .. })
        ));
        assert!(matches!(
            ChdWriter::create_dvd(&out, 4096, 3000, false),
            Err(ChdError::InvalidHunkSize)
        ));
        assert!(matches!(
            ChdWriter::create_dvd(&out, 4096, 0, false),
            Err(ChdError::InvalidHunkSize)
        ));
    }

    #[tokio::test]
    async fn convert_iso_to_chd_picks_hunk_size_by_console() {
        let dir = tempfile::tempdir().unwrap();

        let ps2 = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 2_000_000,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: b"BOOT2 = cdrom0:\\SLUS_123.45;1\r\n",
        });
        let ps2_path = dir.path().join("game.iso");
        std::fs::write(&ps2_path, &ps2).unwrap();
        let ps2_out = dir.path().join("game.chd");
        crate::chd::convert_iso_to_chd(
            &NoProgress,
            ps2_path,
            ps2_out.clone(),
            crate::chd::ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let header =
            ChdHeaderV5::read(&mut IoCursor::new(std::fs::read(&ps2_out).unwrap())).unwrap();
        assert_eq!(header.hunk_bytes, crate::chd::DVD_HUNK_BYTES_DEFAULT);
        assert_eq!(header.unit_bytes, DVD_SECTOR_SIZE);

        let psp = make_iso(&IsoSpec {
            system_id: b"PSP GAME",
            volume_sectors: 100_000,
            root_entries: &[],
            file_content: &[],
        });
        let psp_path = dir.path().join("psp.iso");
        std::fs::write(&psp_path, &psp).unwrap();
        let psp_out = dir.path().join("psp.chd");
        crate::chd::convert_iso_to_chd(
            &NoProgress,
            psp_path,
            psp_out.clone(),
            crate::chd::ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let header =
            ChdHeaderV5::read(&mut IoCursor::new(std::fs::read(&psp_out).unwrap())).unwrap();
        assert_eq!(header.hunk_bytes, crate::chd::DVD_HUNK_BYTES_PSP);
    }
}
