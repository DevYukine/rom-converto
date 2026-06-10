//! WIA (Wii ISO Archive, wit's format and RVZ's predecessor) input
//! support: streaming reconstruction of the original encrypted disc
//! plus pre-conversion integrity verification.

pub mod codec;
pub mod error;
pub mod format;
pub mod reader;
pub mod verify;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use error::{WiaError, WiaResult};
pub use reader::WiaReader;
pub use verify::{verify_total, verify_wia_blocking};

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
        let err = verify_wia_blocking(f.path(), false, Arc::new(AtomicU64::new(0))).unwrap_err();
        assert!(matches!(err, WiaError::HashChainMismatch(_)), "{err}");

        // Truncation: declared size no longer matches.
        let mut truncated = wia.clone();
        truncated.pop();
        let f = write_temp(&truncated);
        let err = verify_wia_blocking(f.path(), false, Arc::new(AtomicU64::new(0))).unwrap_err();
        assert!(matches!(err, WiaError::InvalidHeader(_)), "{err}");

        let f = write_temp(&wia);
        verify_wia_blocking(f.path(), true, Arc::new(AtomicU64::new(0))).unwrap();
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
        let err = verify_wia_blocking(f.path(), true, Arc::new(AtomicU64::new(0))).unwrap_err();
        assert!(
            matches!(err, WiaError::HashChainMismatch(_) | WiaError::Decode(_)),
            "{err}"
        );
    }
}
