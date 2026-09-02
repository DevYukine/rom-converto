//! CHD hunk writer: compresses hunks on a worker pool and assembles the
//! header, compressed map, and metadata blocks into the final file.

pub(crate) mod metadata;
pub(crate) mod worker;

use crate::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::chd::compression::{ChdCodec, codec_header_slots};
use crate::chd::compute_overall_sha1;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, compress_v5_map};
use crate::chd::models::{
    CHD_V5_HEADER_SIZE, ChdHeaderV5, ChdVersion, DVD_SECTOR_SIZE, SHA1_BYTES,
};
use crate::chd::writer::metadata::{
    MetadataBlock, MetadataHash, cd_frame_layout, generate_cd_metadata, generate_dvd_metadata,
    generate_ld_metadata, ld_vbi_bytes,
};
use crate::chd::writer::worker::{
    ChdLdCompressWorker, HunkCompressArgs, HunkWriteState, LdCompressArgs, compress_hunks,
    compress_hunks_dvd, compress_hunks_ld, make_chd_compress_workers,
    make_chd_dvd_compress_workers,
};
use crate::cue::models::CueSheet;
use crate::laserdisc::avi::{AviFile, LdParams};
use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, parallelism};
use binrw::BinWrite;
use sha1::{Digest, Sha1};
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
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
    codecs: Vec<ChdCodec>,
    level: Option<i32>,
    map_entries: Vec<MapEntry>,
    raw_sha1: Sha1,
    metadata_hashes: Vec<MetadataHash>,
    /// Per-frame source flags; empty for DVD-mode writers. `false`
    /// frames are the per-track zero padding chdman appends to reach
    /// a 4-frame boundary; nothing is read from the source for them.
    cd_frame_data: Vec<bool>,
    /// Per-frame audio flags; empty for DVD-mode writers. Audio frames
    /// get their 16-bit sample bytes swapped on ingest to match chdman.
    cd_audio_frames: Vec<bool>,
    /// Packed VBI records, one per field; empty unless this is a
    /// laserdisc writer whose field height calls for an `AVLD` blob.
    ld_vbi: Vec<u8>,
    /// File offset of the reserved `AVLD` payload, backfilled with
    /// [`Self::ld_vbi`] at finalize.
    ld_vbi_offset: u64,
}

impl ChdWriter {
    /// `data_sectors` is the real frame count the CHT2 `FRAMES:`
    /// metadata records; the physical stream pads every track to a
    /// 4-frame boundary like chdman, so the logical size can exceed
    /// `data_sectors * FRAME_SIZE`.
    pub fn create(
        output_path: impl AsRef<Path>,
        data_sectors: u32,
        hunk_size: u32,
        cue_sheet: &CueSheet,
        codecs: Vec<ChdCodec>,
        level: Option<i32>,
    ) -> ChdResult<Self> {
        let file = std::fs::File::create(output_path)?;
        let writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

        let (cd_frame_data, cd_audio_frames) = cd_frame_layout(cue_sheet, data_sectors);
        let logical_bytes = cd_frame_data.len() as u64 * FRAME_SIZE as u64;
        let unit_bytes = FRAME_SIZE as u32;
        if !hunk_size.is_multiple_of(unit_bytes) {
            return Err(ChdError::InvalidHunkSize);
        }

        let slots = codec_header_slots(&codecs);
        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: slots[0],
            compressor_1: slots[1],
            compressor_2: slots[2],
            compressor_3: slots[3],
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
        Self::init(
            writer,
            header,
            codecs,
            level,
            metadata,
            cd_frame_data,
            cd_audio_frames,
        )
    }

    /// DVD-mode writer: flat 2048-byte sectors, `logical_bytes` =
    /// exact input size, `DVD ` marker metadata. `codecs` fills the
    /// header compressor slots in order.
    pub fn create_dvd(
        output_path: impl AsRef<Path>,
        iso_bytes: u64,
        hunk_size: u32,
        codecs: Vec<ChdCodec>,
        level: Option<i32>,
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

        let slots = codec_header_slots(&codecs);
        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: slots[0],
            compressor_1: slots[1],
            compressor_2: slots[2],
            compressor_3: slots[3],
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
        Self::init(
            writer,
            header,
            codecs,
            level,
            metadata,
            Vec::new(),
            Vec::new(),
        )
    }

    fn init(
        mut writer: BufWriter<std::fs::File>,
        header: ChdHeaderV5,
        codecs: Vec<ChdCodec>,
        level: Option<i32>,
        metadata: MetadataBlock,
        cd_frame_data: Vec<bool>,
        cd_audio_frames: Vec<bool>,
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
            codecs,
            level,
            map_entries: Vec::new(),
            raw_sha1: Sha1::new(),
            metadata_hashes: metadata.hashes,
            cd_frame_data,
            cd_audio_frames,
            ld_vbi: Vec::new(),
            ld_vbi_offset: 0,
        })
    }

    /// Reads `sector_data_size` bytes from the source for every data
    /// frame of the padded layout; the per-track padding frames stay
    /// zero but are still hashed, matching chdman.
    pub fn compress_all_hunks(
        &mut self,
        bin_reader: &mut BufReader<std::fs::File>,
        sector_data_size: usize,
        bytes_done: &Arc<AtomicU64>,
        cancel: &CancelToken,
    ) -> ChdResult<()> {
        let hunk_bytes = self.header.hunk_bytes as usize;
        let n_threads = parallelism();
        let workers = make_chd_compress_workers(n_threads, hunk_bytes, &self.codecs, self.level)?;
        let pool: Pool<worker::ChdCompressWork, worker::ChdCompressedOut, ChdError> =
            Pool::spawn(workers);

        let result = compress_hunks(
            &pool,
            HunkWriteState {
                writer: &mut self.writer,
                writer_pos: &mut self.writer_pos,
                map_entries: &mut self.map_entries,
            },
            HunkCompressArgs {
                reader: bin_reader,
                raw_sha1: &mut self.raw_sha1,
                hunk_bytes,
                bytes_done,
                cancel,
            },
            &self.cd_frame_data,
            sector_data_size,
            &self.cd_audio_frames,
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
        let workers =
            make_chd_dvd_compress_workers(parallelism(), hunk_bytes, &self.codecs, self.level)?;
        let pool: Pool<worker::ChdCompressWork, worker::ChdCompressedOut, ChdError> =
            Pool::spawn(workers);

        let result = compress_hunks_dvd(
            &pool,
            HunkWriteState {
                writer: &mut self.writer,
                writer_pos: &mut self.writer_pos,
                map_entries: &mut self.map_entries,
            },
            HunkCompressArgs {
                reader: iso_reader,
                raw_sha1: &mut self.raw_sha1,
                hunk_bytes,
                bytes_done,
                cancel,
            },
            self.header.logical_bytes,
        );

        pool.shutdown();
        result
    }

    /// Laserdisc writer: one hunk per output field, `avhu`-compressed,
    /// carrying the `AVAV` geometry string and, at NTSC/PAL field
    /// heights, an `AVLD` blob reserved now and backfilled by
    /// [`Self::finalize`] once every field's VBI has been parsed.
    pub fn create_ld(output_path: impl AsRef<Path>, params: &LdParams) -> ChdResult<Self> {
        let hunk_bytes = params.bytes_per_frame;
        if hunk_bytes == 0 {
            return Err(ChdError::InvalidHunkSize);
        }

        let file = std::fs::File::create(output_path)?;
        let writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);

        let codecs = vec![ChdCodec::AvHuff];
        let slots = codec_header_slots(&codecs);
        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: slots[0],
            compressor_1: slots[1],
            compressor_2: slots[2],
            compressor_3: slots[3],
            logical_bytes: u64::from(params.frame_count) * u64::from(hunk_bytes),
            map_offset: 0,
            meta_offset: 0,
            hunk_bytes,
            unit_bytes: hunk_bytes,
            raw_sha1: [0; SHA1_BYTES],
            sha1: [0; SHA1_BYTES],
            parent_sha1: [0; SHA1_BYTES],
        };

        let frames = params.frame_count as usize;
        let vbi_bytes = ld_vbi_bytes(params, frames);
        let metadata = generate_ld_metadata(params, frames)?;
        // The reserved blob is the tail of the metadata block, which
        // itself starts right after the V5 header.
        let vbi_offset = CHD_V5_HEADER_SIZE as u64 + metadata.bytes.len() as u64 - vbi_bytes as u64;

        let mut this = Self::init(
            writer,
            header,
            codecs,
            None,
            metadata,
            Vec::new(),
            Vec::new(),
        )?;
        this.ld_vbi = vec![0; vbi_bytes];
        this.ld_vbi_offset = vbi_offset;
        Ok(this)
    }

    pub fn compress_all_hunks_ld<R: Read + Seek>(
        &mut self,
        avi: &mut AviFile<R>,
        params: &LdParams,
        bytes_done: &Arc<AtomicU64>,
        cancel: &CancelToken,
    ) -> ChdResult<()> {
        let workers: Vec<ChdLdCompressWorker> =
            (0..parallelism()).map(|_| ChdLdCompressWorker).collect();
        let pool: Pool<worker::ChdCompressWork, worker::ChdCompressedOut, ChdError> =
            Pool::spawn(workers);

        let result = compress_hunks_ld(
            &pool,
            HunkWriteState {
                writer: &mut self.writer,
                writer_pos: &mut self.writer_pos,
                map_entries: &mut self.map_entries,
            },
            LdCompressArgs {
                avi,
                params,
                raw_sha1: &mut self.raw_sha1,
                vbi: &mut self.ld_vbi,
                bytes_done,
                cancel,
            },
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

        // The AVLD blob is unhashed reserved space, so filling it in
        // after both SHA-1s are settled cannot disturb them.
        if !self.ld_vbi.is_empty() {
            self.writer.seek(SeekFrom::Start(self.ld_vbi_offset))?;
            self.writer.write_all(&self.ld_vbi)?;
        }

        // Seek back and rewrite the header with the finalized
        // offsets and hashes. `BufWriter::seek` flushes the
        // internal buffer first, which is the one place that
        // behavior is wanted.
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
    use crate::chd::compression::default_dvd_codecs;
    use crate::chd::compression::dvd::DvdDecoderSet;
    use crate::chd::map::{COMPRESSION_NONE, COMPRESSION_SELF, decompress_v5_map};
    use crate::chd::models::{CHD_METADATA_TAG_DVD, DVD_SECTOR_SIZE};
    use crate::chd::reader::worker::resolve_entry;
    use crate::chd::verify_chd;
    use crate::util::NoProgress;
    use crate::util::iso9660::test_fixtures::{IsoSpec, make_iso};
    use binrw::BinRead;
    use std::io::Cursor as IoCursor;
    use std::sync::atomic::Ordering;

    use crate::chd::compression::avhuff;
    use crate::chd::models::{
        CHD_METADATA_FLAG_HASHED, CHD_METADATA_TAG_AV, CHD_METADATA_TAG_AV_LD, ChdMetadataHeader,
    };
    use crate::chd::test_fixtures::mixed_iso;
    use crate::chd::writer::worker::ld_audio_window;
    use crate::laserdisc::avi::test_fixtures::{
        AviSpec, build_avi, pattern_frame, pattern_samples,
    };
    use crate::laserdisc::vbi::VBI_PACKED_BYTES;

    fn write_dvd_chd(iso: &[u8], hunk_size: u32, codecs: Vec<ChdCodec>) -> Vec<u8> {
        write_dvd_chd_leveled(iso, hunk_size, codecs, None)
    }

    fn write_dvd_chd_leveled(
        iso: &[u8],
        hunk_size: u32,
        codecs: Vec<ChdCodec>,
        level: Option<i32>,
    ) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("in.iso");
        let chd_path = dir.path().join("out.chd");
        std::fs::write(&iso_path, iso).unwrap();

        let iso_file = std::fs::File::open(&iso_path).unwrap();
        let mut reader = BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);
        let mut writer =
            ChdWriter::create_dvd(&chd_path, iso.len() as u64, hunk_size, codecs, level).unwrap();
        let bytes_done = Arc::new(AtomicU64::new(0));
        writer
            .compress_all_hunks_dvd(&mut reader, &bytes_done, &CancelToken::new())
            .unwrap();
        assert_eq!(bytes_done.load(Ordering::Relaxed), iso.len() as u64);
        writer.finalize().unwrap();

        std::fs::read(&chd_path).unwrap()
    }

    fn read_map(chd: &[u8], header: &ChdHeaderV5) -> Vec<MapEntry> {
        let hunk_count = header.logical_bytes.div_ceil(header.hunk_bytes as u64) as u32;
        let map_size = ((chd.len() as u64 - header.map_offset).min(u32::MAX as u64)) as usize;
        decompress_v5_map(
            &chd[header.map_offset as usize..header.map_offset as usize + map_size],
            hunk_count,
            header.hunk_bytes,
            header.unit_bytes,
        )
        .unwrap()
    }

    fn decode_hunks(chd: &[u8], header: &ChdHeaderV5) -> Vec<u8> {
        let hunk_bytes = header.hunk_bytes as usize;
        let map = read_map(chd, header);

        let compressors = [
            header.compressor_0,
            header.compressor_1,
            header.compressor_2,
            header.compressor_3,
        ];
        let mut decoder = DvdDecoderSet::new(compressors, hunk_bytes).unwrap();
        let mut out = Vec::new();
        for hunk_index in 0..map.len() {
            let entry = resolve_entry(&map, hunk_index as u32).unwrap();
            let stored = &chd[entry.offset as usize..entry.offset as usize + entry.length as usize];
            let hunk = match entry.compression {
                slot @ 0..=3 => decoder.decompress(slot, stored, hunk_bytes).unwrap(),
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
        let chd = write_dvd_chd(&iso, 4096, vec![ChdCodec::Lzma, ChdCodec::Zlib]);

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
        let chd = write_dvd_chd(
            &iso,
            2048,
            vec![ChdCodec::Lzma, ChdCodec::Zlib, ChdCodec::Zstd],
        );
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        assert_eq!(&header.compressor_2, b"zstd");
        assert_eq!(decode_hunks(&chd, &header), iso);
    }

    #[test]
    fn dvd_chd_round_trips_each_codec_set() {
        let iso = mixed_iso(11);
        for codecs in [
            vec![ChdCodec::Zstd],
            vec![ChdCodec::Huff],
            vec![ChdCodec::Flac],
            default_dvd_codecs(),
        ] {
            let chd = write_dvd_chd(&iso, 4096, codecs.clone());
            let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
            assert_eq!(
                [
                    header.compressor_0,
                    header.compressor_1,
                    header.compressor_2,
                    header.compressor_3
                ],
                super::codec_header_slots(&codecs),
            );
            assert_eq!(decode_hunks(&chd, &header), iso, "codec set {codecs:?}");
        }
    }

    #[test]
    fn dvd_chd_levels_produce_readable_output() {
        let iso = mixed_iso(11);
        for level in [Some(1), Some(22)] {
            let chd = write_dvd_chd_leveled(&iso, 4096, default_dvd_codecs(), level);
            let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
            assert_eq!(decode_hunks(&chd, &header), iso, "level {level:?}");
        }
    }

    #[test]
    fn header_slots_follow_custom_codec_order() {
        let iso = mixed_iso(8);
        let codecs = vec![ChdCodec::Zstd, ChdCodec::Lzma, ChdCodec::Zlib];
        let chd = write_dvd_chd(&iso, 2048, codecs.clone());
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        assert_eq!(&header.compressor_0, b"zstd");
        assert_eq!(&header.compressor_1, b"lzma");
        assert_eq!(&header.compressor_2, b"zlib");
        assert_eq!(header.compressor_3, [0u8; 4]);
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
            ChdWriter::create_dvd(&out, 4096 + 1, 4096, default_dvd_codecs(), None),
            Err(ChdError::IsoNotSectorAligned { .. })
        ));
        assert!(matches!(
            ChdWriter::create_dvd(&out, 4096, 3000, default_dvd_codecs(), None),
            Err(ChdError::InvalidHunkSize)
        ));
        assert!(matches!(
            ChdWriter::create_dvd(&out, 4096, 0, default_dvd_codecs(), None),
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
            crate::chd::ChdOptions::default(),
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
            crate::chd::ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let header =
            ChdHeaderV5::read(&mut IoCursor::new(std::fs::read(&psp_out).unwrap())).unwrap();
        assert_eq!(header.hunk_bytes, crate::chd::DVD_HUNK_BYTES_PSP);
    }

    /// Synthetic laserdisc AVI: NTSC-rate YUY2 video plus 16-bit PCM.
    fn ld_avi(width: u32, height: u32, frames: usize, channels: u16, samples: usize) -> Vec<u8> {
        let video: Vec<Vec<u8>> = (0..frames)
            .map(|i| pattern_frame(width, height, i as u8))
            .collect();
        let audio = pattern_samples(samples, channels as usize);
        build_avi(&AviSpec {
            width,
            height,
            timescale: 30000,
            sampletime: 1001,
            video_format: *b"YUY2",
            frames: &video,
            channels,
            sample_rate: 48_000,
            sample_bits: 16,
            samples: &audio,
            index: true,
            block_align_override: None,
            video_length_override: None,
        })
    }

    fn ld_params_of(avi: &[u8]) -> LdParams {
        AviFile::new(IoCursor::new(avi.to_vec()))
            .unwrap()
            .ld_params()
            .unwrap()
    }

    fn write_ld_chd(avi_bytes: &[u8]) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        write_ld_chd_at(avi_bytes, &dir.path().join("out.chd"))
    }

    fn write_ld_chd_at(avi_bytes: &[u8], chd_path: &Path) -> Vec<u8> {
        let mut avi = AviFile::new(IoCursor::new(avi_bytes.to_vec())).unwrap();
        let params = avi.ld_params().unwrap();

        let mut writer = ChdWriter::create_ld(chd_path, &params).unwrap();
        let bytes_done = Arc::new(AtomicU64::new(0));
        writer
            .compress_all_hunks_ld(&mut avi, &params, &bytes_done, &CancelToken::new())
            .unwrap();
        assert_eq!(
            bytes_done.load(Ordering::Relaxed),
            u64::from(params.frame_count) * u64::from(params.bytes_per_frame)
        );
        writer.finalize().unwrap();
        std::fs::read(chd_path).unwrap()
    }

    /// Walk the metadata chain from the header's `meta_offset`.
    fn read_metadata(chd: &[u8], header: &ChdHeaderV5) -> Vec<ChdMetadataHeader> {
        let mut entries = Vec::new();
        let mut pos = header.meta_offset;
        loop {
            let mut cursor = IoCursor::new(chd);
            cursor.set_position(pos);
            let entry = ChdMetadataHeader::read(&mut cursor).unwrap();
            let next = u64::from_be_bytes(entry.reserved);
            entries.push(entry);
            if next == 0 {
                break;
            }
            pos = next;
        }
        entries
    }

    /// Re-derive the raw hunk stream straight from the AVI: each output
    /// field is the frame's rows from `n % interlace_factor`, plus that
    /// field's audio window, zero-padded to the hunk size.
    fn expected_ld_stream(avi_bytes: &[u8], params: &LdParams) -> Vec<u8> {
        let mut avi = AviFile::new(IoCursor::new(avi_bytes.to_vec())).unwrap();
        let factor = if params.interlaced { 2 } else { 1 };
        let width = params.width as usize;
        let height = params.height as usize;
        let mut frame = vec![0u16; width * height * factor];
        let mut out = Vec::new();

        for effframe in 0..params.frame_count {
            avi.read_video_frame(effframe / factor as u32, &mut frame)
                .unwrap();
            let first_row = effframe as usize % factor;
            let mut field = Vec::with_capacity(width * height);
            for row in 0..height {
                let start = (first_row + row * factor) * width;
                field.extend_from_slice(&frame[start..start + width]);
            }

            let (first_sample, samples) = ld_audio_window(params, effframe);
            let audio: Vec<Vec<i16>> = (0..params.channels)
                .map(|channel| {
                    let mut buf = vec![0i16; samples as usize];
                    avi.read_sound_samples(channel, first_sample, samples, &mut buf)
                        .unwrap();
                    buf
                })
                .collect();
            let channels: Vec<&[i16]> = audio.iter().map(Vec::as_slice).collect();

            let mut hunk = avhuff::assemble_raw_frame(
                params.width as u16,
                params.height as u16,
                &field,
                &channels,
            )
            .unwrap();
            hunk.resize(params.bytes_per_frame as usize, 0);
            out.extend_from_slice(&hunk);
        }
        out
    }

    #[test]
    fn ld_chd_writes_chdman_shaped_file() {
        let avi = ld_avi(48, 524, 3, 1, 6000);
        let params = ld_params_of(&avi);
        assert!(params.interlaced);
        assert_eq!((params.height, params.frame_count), (262, 6));
        assert_eq!(params.bytes_per_frame, 12 + 801 * 2 + 48 * 262 * 2);

        let chd = write_ld_chd(&avi);
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        assert_eq!(&header.compressor_0, b"avhu");
        assert_eq!(
            [
                header.compressor_1,
                header.compressor_2,
                header.compressor_3
            ],
            [[0u8; 4]; 3]
        );
        assert_eq!(header.hunk_bytes, params.bytes_per_frame);
        assert_eq!(header.unit_bytes, params.bytes_per_frame);
        assert_eq!(
            header.logical_bytes,
            u64::from(params.frame_count) * u64::from(params.bytes_per_frame)
        );
        assert_eq!(header.meta_offset, CHD_V5_HEADER_SIZE as u64);

        let entries = read_metadata(&chd, &header);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tag, CHD_METADATA_TAG_AV);
        assert_eq!(entries[0].flags, CHD_METADATA_FLAG_HASHED);
        let mut expected_av = params.av_metadata().into_bytes();
        expected_av.push(0);
        assert_eq!(entries[0].data, expected_av);

        assert_eq!(entries[1].tag, CHD_METADATA_TAG_AV_LD);
        assert_eq!(entries[1].flags, 0);
        assert_eq!(
            entries[1].data.len(),
            params.frame_count as usize * VBI_PACKED_BYTES
        );
        // Every record was backfilled: the leading u24be is the field index.
        for field in 0..params.frame_count {
            let record = &entries[1].data[field as usize * VBI_PACKED_BYTES..][..3];
            assert_eq!(record, &field.to_be_bytes()[1..4]);
        }

        let raw = expected_ld_stream(&avi, &params);
        let raw_sha1: [u8; SHA1_BYTES] = Sha1::digest(&raw).into();
        assert_eq!(header.raw_sha1, raw_sha1);
        let av_hash = MetadataHash {
            tag: CHD_METADATA_TAG_AV,
            sha1: Sha1::digest(&expected_av).into(),
        };
        assert_eq!(header.sha1, compute_overall_sha1(raw_sha1, &[av_hash]));

        assert_eq!(decode_hunks(&chd, &header), raw);
    }

    #[test]
    fn ld_chd_skips_avld_outside_ntsc_and_pal_heights() {
        let avi = ld_avi(64, 48, 2, 1, 4000);
        let params = ld_params_of(&avi);
        assert!(!params.interlaced);
        assert_eq!((params.height, params.frame_count), (48, 2));

        let chd = write_ld_chd(&avi);
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        let entries = read_metadata(&chd, &header);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tag, CHD_METADATA_TAG_AV);
        assert_eq!(
            decode_hunks(&chd, &header),
            expected_ld_stream(&avi, &params)
        );
    }

    #[test]
    fn ld_chd_emits_avld_for_progressive_ntsc_field_height() {
        let avi = ld_avi(64, 262, 2, 1, 4000);
        let params = ld_params_of(&avi);
        assert!(!params.interlaced);
        assert_eq!((params.height, params.frame_count), (262, 2));

        let chd = write_ld_chd(&avi);
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();
        let entries = read_metadata(&chd, &header);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].tag, CHD_METADATA_TAG_AV_LD);
        assert_eq!(
            entries[1].data.len(),
            params.frame_count as usize * VBI_PACKED_BYTES
        );
    }

    /// Progressive AVI whose fields all carry the same pixels and silence.
    /// 30 fps against a 48000 Hz rate makes every audio window exactly
    /// `48000 / 30` samples, so consecutive hunks come out byte-identical.
    fn ld_avi_repeating(frames: usize) -> Vec<u8> {
        let (width, height) = (48, 262);
        let video: Vec<Vec<u8>> = (0..frames)
            .map(|_| pattern_frame(width, height, 3))
            .collect();
        let samples = vec![0i16; 1600 * frames];
        build_avi(&AviSpec {
            width,
            height,
            timescale: 30,
            sampletime: 1,
            video_format: *b"YUY2",
            frames: &video,
            channels: 1,
            sample_rate: 48_000,
            sample_bits: 16,
            samples: &samples,
            index: true,
            block_align_override: None,
            video_length_override: None,
        })
    }

    #[tokio::test]
    async fn ld_chd_dedups_repeated_fields_into_self_entries() {
        let avi = ld_avi_repeating(4);
        let params = ld_params_of(&avi);
        assert!(!params.interlaced);
        assert_eq!(
            (params.frame_count, params.max_samples_per_frame),
            (4, 1600)
        );

        let dir = tempfile::tempdir().unwrap();
        let chd_path = dir.path().join("out.chd");
        let chd = write_ld_chd_at(&avi, &chd_path);
        let header = ChdHeaderV5::read(&mut IoCursor::new(&chd)).unwrap();

        let map = read_map(&chd, &header);
        assert_eq!(map.len(), 4);
        assert_eq!(map[0].compression, 0);
        for entry in &map[1..] {
            assert_eq!(entry.compression, COMPRESSION_SELF);
            assert_eq!(entry.offset, 0);
            assert_eq!(entry.length, 0);
        }

        assert_eq!(
            decode_hunks(&chd, &header),
            expected_ld_stream(&avi, &params)
        );
        verify_chd(&NoProgress, chd_path, None, false)
            .await
            .unwrap();
    }

    #[test]
    fn ld_audio_windows_tile_the_ceiling_formula() {
        let avi = ld_avi(48, 524, 3, 1, 6000);
        let params = ld_params_of(&avi);
        let at = |field: u64| {
            (u64::from(params.rate) * field * 1_000_000)
                .div_ceil(u64::from(params.fps_times_1million))
        };

        let mut next = 0u64;
        let mut total = 0u64;
        for field in 0..params.frame_count {
            let (first, samples) = ld_audio_window(&params, field);
            assert_eq!(first, at(u64::from(field)));
            assert_eq!(first, next);
            assert!(
                samples == params.max_samples_per_frame
                    || samples + 1 == params.max_samples_per_frame
            );
            next = first + u64::from(samples);
            total += u64::from(samples);
        }
        assert_eq!(total, at(u64::from(params.frame_count)));
    }
}
