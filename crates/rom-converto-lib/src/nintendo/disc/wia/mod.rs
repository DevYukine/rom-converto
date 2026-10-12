//! WIA (Wii ISO Archive, RVZ's predecessor) streaming reconstruction,
//! Wii compression, and integrity verification.

pub mod codec;
pub mod error;
pub mod format;
pub mod reader;
pub mod verify;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use crate::nintendo::disc::rvz::compress::compress_wia as compress_to_wia;
pub use error::{WiaError, WiaResult};
pub use reader::WiaReader;
pub use verify::{verify_total, verify_wia_blocking};

use crate::nintendo::disc::rvz::error::{RvzError, RvzResult};
use std::str::FromStr;

/// Default WIA compression level.
pub const DEFAULT_WIA_LEVEL: u32 = 6;

/// Compression method emitted by the WIA writer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WiaCompression {
    /// Stores groups and tables without compression.
    None,
    /// Compresses groups and tables with bzip2.
    Bzip2,
    /// Compresses groups and tables with raw LZMA1.
    Lzma,
    /// Compresses groups and tables with raw LZMA2.
    #[default]
    Lzma2,
}

impl WiaCompression {
    /// Returns the command-line name of the compression method.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bzip2 => "bzip2",
            Self::Lzma => "lzma",
            Self::Lzma2 => "lzma2",
        }
    }

    /// Levels the codec accepts; `None` for the uncompressed method.
    pub fn level_range(self) -> Option<std::ops::RangeInclusive<u32>> {
        match self {
            Self::None => None,
            Self::Bzip2 => Some(1..=9),
            Self::Lzma | Self::Lzma2 => Some(0..=9),
        }
    }
}

impl std::fmt::Display for WiaCompression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WiaCompression {
    type Err = RvzError;

    fn from_str(value: &str) -> RvzResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "bzip2" => Ok(Self::Bzip2),
            "lzma" => Ok(Self::Lzma),
            "lzma2" => Ok(Self::Lzma2),
            _ => Err(RvzError::Custom(format!(
                "unknown WIA compression {value:?}: expected none, bzip2, lzma, or lzma2"
            ))),
        }
    }
}

/// Compression options for [`compress_to_wia`].
#[derive(Debug, Clone, Copy)]
pub struct WiaCompressOptions {
    /// Codec used for every group and metadata table.
    pub compression: WiaCompression,
    /// Compression level: bzip2 1..=9, LZMA/LZMA2 0..=9; ignored for none.
    pub level: u32,
}

impl Default for WiaCompressOptions {
    fn default() -> Self {
        Self {
            compression: WiaCompression::default(),
            level: DEFAULT_WIA_LEVEL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format::{
        WIA_COMPR_BZIP2, WIA_COMPR_LZMA, WIA_COMPR_LZMA2, WIA_COMPR_NONE, WIA_COMPR_PURGE,
    };
    use super::test_fixtures::make_wia;
    use super::*;
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::nintendo::rvl::test_fixtures::{
        make_fake_wii_iso_with_partial_partition, make_fake_wii_iso_with_partition,
    };
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn compression_names_round_trip() {
        for compression in [
            WiaCompression::None,
            WiaCompression::Bzip2,
            WiaCompression::Lzma,
            WiaCompression::Lzma2,
        ] {
            assert_eq!(
                compression.as_str().parse::<WiaCompression>().unwrap(),
                compression
            );
            assert_eq!(compression.to_string(), compression.as_str());
        }
        assert_eq!(WiaCompressOptions::default().level, DEFAULT_WIA_LEVEL);
    }

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::with_suffix(".wia").unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    fn assert_round_trip(iso: &[u8], compression: u32, chunk: u32) {
        let f = write_temp(&make_wia(iso, compression, chunk));
        let mut r = WiaReader::open(f.path()).unwrap();
        assert_eq!(r.iso_size(), iso.len() as u64);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert!(
            out == iso,
            "reconstruction must be byte-identical (compression {compression}, chunk {chunk:#X})"
        );
    }

    #[test]
    fn gamecube_round_trips_every_codec() {
        let iso = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);
        for compression in [
            WIA_COMPR_NONE,
            WIA_COMPR_PURGE,
            WIA_COMPR_BZIP2,
            WIA_COMPR_LZMA,
            WIA_COMPR_LZMA2,
        ] {
            assert_round_trip(&iso, compression, 0x20_0000);
        }
    }

    #[test]
    fn wii_partition_round_trips() {
        let iso = make_fake_wii_iso_with_partition(3);
        assert_round_trip(&iso, WIA_COMPR_NONE, 0x20_0000);
        assert_round_trip(&iso, WIA_COMPR_LZMA, 0x20_0000);
    }

    #[test]
    fn wii_round_trips_with_multi_cluster_groups() {
        // 4 MiB chunks: one partition group spans two hash clusters,
        // exercising the per-group exception list indexing.
        let iso = make_fake_wii_iso_with_partition(3);
        assert_round_trip(&iso, WIA_COMPR_BZIP2, 0x40_0000);
        assert_round_trip(&iso, WIA_COMPR_LZMA2, 0x40_0000);
    }

    #[test]
    fn wii_round_trips_partial_cluster() {
        let iso = make_fake_wii_iso_with_partial_partition(2, 5);
        assert_round_trip(&iso, WIA_COMPR_NONE, 0x20_0000);
        assert_round_trip(&iso, WIA_COMPR_LZMA, 0x40_0000);
    }

    #[test]
    fn wii_reproduces_on_disc_hash_deviations_via_exceptions() {
        // Flip bytes inside the encrypted hash area of two sectors:
        // the stored exceptions must reproduce them exactly.
        let mut iso = make_fake_wii_iso_with_partition(2);
        let mut cur = std::io::Cursor::new(&iso[..]);
        let entries = crate::nintendo::rvl::disc::read_partition_table(&mut cur).unwrap();
        let info = crate::nintendo::rvl::partition::read_partition_info(
            &mut cur,
            entries[0].offset,
            entries[0].group,
            entries[0].partition_type,
        )
        .unwrap();
        let data_start = info.data_start() as usize;
        iso[data_start + 0x37] ^= 0xA5;
        iso[data_start + 0x8000 + 0x3F0] ^= 0x5A;
        assert_round_trip(&iso, WIA_COMPR_NONE, 0x20_0000);
        assert_round_trip(&iso, WIA_COMPR_PURGE, 0x20_0000);
    }

    #[test]
    fn reader_seeks_across_segments() {
        let iso = make_fake_gamecube_iso(3 * 1024 * 1024);
        let f = write_temp(&make_wia(&iso, WIA_COMPR_NONE, 0x20_0000));
        let mut r = WiaReader::open(f.path()).unwrap();
        let mut buf = [0u8; 64];
        r.seek(SeekFrom::Start(0x20_0000 + 5)).unwrap();
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &iso[0x20_0000 + 5..0x20_0000 + 5 + 64]);
        r.seek(SeekFrom::Start(0x10)).unwrap();
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &iso[0x10..0x50]);
    }

    #[test]
    fn verify_detects_header_chain_corruption() {
        let iso = make_fake_gamecube_iso(1024 * 1024);
        let wia = make_wia(&iso, WIA_COMPR_LZMA, 0x20_0000);

        // Corrupt the disc struct.
        let mut bad = wia.clone();
        bad[0x60] ^= 0xFF;
        let f = write_temp(&bad);
        let err = verify_wia_blocking(
            f.path(),
            false,
            Arc::new(AtomicU64::new(0)),
            crate::util::CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, WiaError::HashChainMismatch(_)), "{err}");

        // Truncation: declared size no longer matches.
        let mut truncated = wia.clone();
        truncated.pop();
        let f = write_temp(&truncated);
        let err = verify_wia_blocking(
            f.path(),
            false,
            Arc::new(AtomicU64::new(0)),
            crate::util::CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, WiaError::InvalidHeader(_)), "{err}");

        let f = write_temp(&wia);
        verify_wia_blocking(
            f.path(),
            true,
            Arc::new(AtomicU64::new(0)),
            crate::util::CancelToken::new(),
        )
        .unwrap();
    }

    #[test]
    fn deep_verify_detects_group_corruption() {
        let iso = make_fake_gamecube_iso(1024 * 1024);
        let mut wia = make_wia(&iso, WIA_COMPR_PURGE, 0x20_0000);
        // Flip a byte in the middle of the group data blob (between
        // the partition table and the trailing metadata tables).
        let mid = wia.len() / 2;
        wia[mid] ^= 0xFF;
        let f = write_temp(&wia);
        let err = verify_wia_blocking(
            f.path(),
            true,
            Arc::new(AtomicU64::new(0)),
            crate::util::CancelToken::new(),
        )
        .unwrap_err();
        assert!(
            matches!(err, WiaError::HashChainMismatch(_) | WiaError::Decode(_)),
            "{err}"
        );
    }

    async fn assert_writer_round_trip(iso: &[u8], compression: WiaCompression, level: u32) {
        use crate::nintendo::disc::rvz::format::{WiaDisc, WiaFileHead};
        use crate::util::NoProgress;
        use binrw::{BinRead, Endian};

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.iso");
        let output = dir.path().join("output.wia");
        tokio::fs::write(&input, iso).await.unwrap();
        compress_to_wia(
            &input,
            &output,
            WiaCompressOptions { compression, level },
            &NoProgress,
            crate::util::CancelToken::new(),
        )
        .await
        .unwrap();

        let mut file = std::fs::File::open(&output).unwrap();
        let head = WiaFileHead::read_options(&mut file, Endian::Big, ()).unwrap();
        let disc = WiaDisc::read_options(&mut file, Endian::Big, ()).unwrap();
        assert_eq!(head.magic, format::WIA_MAGIC);
        assert_eq!(head.version, format::WIA_VERSION);
        assert_eq!(head.version_compatible, 0x00090000);
        assert_eq!(head.wia_file_size, file.metadata().unwrap().len());
        assert_eq!(head.iso_file_size, iso.len() as u64);
        assert_eq!(disc.disc_type, 2);
        assert_eq!(disc.chunk_size, format::WIA_CHUNK_GRANULARITY);
        assert_eq!(disc.compr_level, level as i32);
        let id = match compression {
            WiaCompression::None => WIA_COMPR_NONE,
            WiaCompression::Bzip2 => WIA_COMPR_BZIP2,
            WiaCompression::Lzma => WIA_COMPR_LZMA,
            WiaCompression::Lzma2 => WIA_COMPR_LZMA2,
        };
        assert_eq!(disc.compression, id);
        let options = lzma_rust2::LzmaOptions::with_preset(level);
        let dict = options.dict_size.min(4 * 1024 * 1024);
        match compression {
            WiaCompression::Lzma => {
                assert_eq!(disc.compr_data_len, 5);
                assert_eq!(disc.compr_data[0], options.get_props());
                assert_eq!(&disc.compr_data[1..5], &dict.to_le_bytes());
            }
            WiaCompression::Lzma2 => {
                assert_eq!(disc.compr_data_len, 1);
                let p = disc.compr_data[0];
                assert!(p <= 20);
                assert_eq!((2 | (p as u32 & 1)) << (p / 2 + 11), dict);
            }
            _ => assert_eq!(disc.compr_data_len, 0),
        }

        let mut reader = crate::nintendo::disc::input::open_disc_input(&output).unwrap();
        let mut restored = Vec::new();
        reader.read_to_end(&mut restored).unwrap();
        assert!(
            restored == iso,
            "writer round trip failed for {compression:?}"
        );
        verify_total(&output, true).unwrap();
        verify_wia_blocking(
            &output,
            true,
            Arc::new(AtomicU64::new(0)),
            crate::util::CancelToken::new(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn writer_round_trips_raw_and_partitioned_wii_with_every_codec() {
        let raw = crate::nintendo::rvl::test_fixtures::make_fake_wii_iso(5 * 1024 * 1024 + 123);
        let partitioned = make_fake_wii_iso_with_partition(3);
        for compression in [
            WiaCompression::None,
            WiaCompression::Bzip2,
            WiaCompression::Lzma,
            WiaCompression::Lzma2,
        ] {
            assert_writer_round_trip(&raw, compression, 6).await;
            assert_writer_round_trip(&partitioned, compression, 6).await;
        }
    }

    #[tokio::test]
    async fn writer_round_trips_partial_partition_and_raw_zero_sentinel() {
        let partial = make_fake_wii_iso_with_partial_partition(1, 5);
        let mut raw = crate::nintendo::rvl::test_fixtures::make_fake_wii_iso(5 * 1024 * 1024 + 123);
        raw[2 * 1024 * 1024..4 * 1024 * 1024].fill(0);
        for compression in [
            WiaCompression::None,
            WiaCompression::Bzip2,
            WiaCompression::Lzma,
            WiaCompression::Lzma2,
        ] {
            assert_writer_round_trip(&partial, compression, 6).await;
            assert_writer_round_trip(&raw, compression, 6).await;
        }
    }

    #[tokio::test]
    async fn writer_rejects_gamecube_and_out_of_range_levels() {
        use crate::util::{CancelToken, NoProgress};
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.iso");
        let output = dir.path().join("output.wia");
        tokio::fs::write(&input, make_fake_gamecube_iso(1024 * 1024))
            .await
            .unwrap();
        let error = compress_to_wia(
            &input,
            &output,
            WiaCompressOptions::default(),
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RvzError::Custom(_)));
        assert!(error.to_string().contains("Wii-only"));
        assert!(!output.exists());

        for (compression, level, range) in [
            (WiaCompression::Bzip2, 0, "1..=9"),
            (WiaCompression::Bzip2, 10, "1..=9"),
            (WiaCompression::Lzma, 10, "0..=9"),
            (WiaCompression::Lzma2, 10, "0..=9"),
            (WiaCompression::Lzma2, u32::MAX, "0..=9"),
        ] {
            let error = compress_to_wia(
                &input,
                &output,
                WiaCompressOptions { compression, level },
                &NoProgress,
                CancelToken::new(),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, RvzError::Custom(_)));
            assert!(error.to_string().contains(range));
            assert!(!output.exists());
        }
    }

    #[test]
    fn writer_compression_parsing_and_defaults() {
        assert_eq!(WiaCompression::default(), WiaCompression::Lzma2);
        assert_eq!(WiaCompressOptions::default().level, 6);
        for (name, compression) in [
            ("none", WiaCompression::None),
            ("bzip2", WiaCompression::Bzip2),
            ("lzma", WiaCompression::Lzma),
            ("lzma2", WiaCompression::Lzma2),
        ] {
            assert_eq!(name.parse::<WiaCompression>().unwrap(), compression);
        }
        assert!("zstd".parse::<WiaCompression>().is_err());
    }
}
