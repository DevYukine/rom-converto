//! Native NKit input support (`.nkit.iso` / `.nkit.gcz`): streaming
//! restoration of the original disc with junk regeneration, plus the
//! whole-file CRC-32 self-check NKit embeds.

pub mod crc;
pub mod error;
pub mod format;
pub mod gaps;
pub mod reader;
pub mod verify;
pub(crate) mod wii;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use error::{NkitError, NkitResult};
pub use reader::NkitReader;
pub use verify::{verify_nkit_blocking, verify_total};

#[cfg(test)]
mod tests {
    use super::test_fixtures::{crc_of, make_fake_gc_fs_iso, make_nkit_gc, make_nkit_gcz};
    use super::*;
    use crate::nintendo::gcz::GczReader;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn write_temp(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::with_suffix(suffix).unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn nkit_iso_restores_byte_identical() {
        let iso = make_fake_gc_fs_iso();
        let nkit = make_nkit_gc(&iso);
        assert!(
            nkit.len() < iso.len(),
            "junk stripping must shrink the image"
        );

        let f = write_temp(&nkit, ".nkit.iso");
        let mut r = NkitReader::open(f.path()).unwrap();
        assert_eq!(r.image_size(), iso.len() as u64);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert!(out == iso, "restoration must be byte-identical");
    }

    #[test]
    fn nkit_gcz_layers_through_gcz_reader() {
        let iso = make_fake_gc_fs_iso();
        let nkit = make_nkit_gc(&iso);
        let f = write_temp(&make_nkit_gcz(&nkit, crc_of(&iso)), ".nkit.gcz");

        let mut r = NkitReader::from_source(GczReader::open(f.path()).unwrap()).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert!(out == iso);
    }

    #[test]
    fn reader_seeks_and_rereads_without_breaking_crc_check() {
        let iso = make_fake_gc_fs_iso();
        let f = write_temp(&make_nkit_gc(&iso), ".nkit.iso");
        let mut r = NkitReader::open(f.path()).unwrap();

        // Header probe followed by a full sequential read, the same
        // access pattern the compress pipeline uses.
        let mut dhead = [0u8; 128];
        r.read_exact(&mut dhead).unwrap();
        assert_eq!(&dhead[..], &iso[..128]);
        r.seek(SeekFrom::Start(0)).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert!(out == iso);
    }

    #[test]
    fn corrupted_file_data_fails_the_in_stream_crc_check() {
        let iso = make_fake_gc_fs_iso();
        let mut nkit = make_nkit_gc(&iso);
        // Flip a byte inside file A's verbatim data. The span planner
        // has no checksum of its own, so only the CRC tee can catch it.
        let pos = nkit.len() / 2;
        nkit[pos] ^= 0x01;
        let f = write_temp(&nkit, ".nkit.iso");
        let mut r = NkitReader::open(f.path()).unwrap();
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert!(err.to_string().contains("CRC32 mismatch"), "{err}");
    }

    #[test]
    fn verify_checks_whole_container_crc() {
        let iso = make_fake_gc_fs_iso();
        let nkit = make_nkit_gc(&iso);

        let f = write_temp(&nkit, ".nkit.iso");
        verify_nkit_blocking(f.path(), false, Arc::new(AtomicU64::new(0))).unwrap();

        let mut bad = nkit.clone();
        let n = bad.len();
        bad[n - 3] ^= 0xFF;
        let f = write_temp(&bad, ".nkit.iso");
        let err = verify_nkit_blocking(f.path(), false, Arc::new(AtomicU64::new(0))).unwrap_err();
        assert!(matches!(err, NkitError::CrcMismatch { .. }), "{err}");
    }

    #[test]
    fn verify_covers_the_gcz_wrapper() {
        let iso = make_fake_gc_fs_iso();
        let nkit = make_nkit_gc(&iso);
        let gcz = make_nkit_gcz(&nkit, crc_of(&iso));

        let f = write_temp(&gcz, ".nkit.gcz");
        verify_nkit_blocking(f.path(), true, Arc::new(AtomicU64::new(0))).unwrap();

        let mut bad = gcz.clone();
        let n = bad.len();
        bad[n - 3] ^= 0xFF;
        let f = write_temp(&bad, ".nkit.gcz");
        let err = verify_nkit_blocking(f.path(), true, Arc::new(AtomicU64::new(0)));
        assert!(err.is_err());
    }
}

#[cfg(test)]
mod wii_tests {
    use super::test_fixtures::{make_fake_wii_fs_iso, make_nkit_wii};
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn nkit_wii_restores_byte_identical() {
        let iso = make_fake_wii_fs_iso();
        let nkit = make_nkit_wii(&iso);
        assert!(nkit.len() < iso.len(), "stripping must shrink the image");

        let mut f = tempfile::NamedTempFile::with_suffix(".nkit.iso").unwrap();
        f.write_all(&nkit).unwrap();
        f.flush().unwrap();
        let mut r = NkitReader::open(f.path()).unwrap();
        assert_eq!(r.image_size(), iso.len() as u64);
        assert!(r.restorable_warning().is_none());
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), iso.len());
        assert!(out == iso, "Wii restoration must be byte-identical");
    }

    #[test]
    fn nkit_wii_corruption_fails_the_crc_check() {
        let iso = make_fake_wii_fs_iso();
        let mut nkit = make_nkit_wii(&iso);
        let pos = nkit.len() / 2;
        nkit[pos] ^= 0x10;
        let mut f = tempfile::NamedTempFile::with_suffix(".nkit.iso").unwrap();
        f.write_all(&nkit).unwrap();
        f.flush().unwrap();
        let mut out = Vec::new();
        let res = NkitReader::open(f.path()).and_then(|mut r| {
            r.read_to_end(&mut out)?;
            Ok(())
        });
        assert!(res.is_err());
    }
}
