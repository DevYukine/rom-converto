//! GCZ (Dolphin CompressedBlob) input support: streaming reconstruction
//! of the logical disc plus a pre-conversion integrity pass.

pub mod error;
pub mod format;
pub mod reader;
pub mod verify;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use error::{GczError, GczResult};
pub use format::GczHeader;
pub use reader::{GczReader, gcz_logical_prefix};
pub use verify::{verify_gcz_blocking, verify_total};

use std::io::Read;
use std::path::Path;

/// Detect a GCZ container by extension or leading magic, mirroring
/// the WBFS detection. The magic fallback covers renamed files.
pub fn is_gcz_input(path: &Path) -> bool {
    let by_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("gcz"))
        .unwrap_or(false);
    by_ext || gcz_magic(path).unwrap_or(false)
}

fn gcz_magic(path: &Path) -> std::io::Result<bool> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = [0u8; 4];
    if f.read(&mut buf)? < 4 {
        return Ok(false);
    }
    Ok(u32::from_le_bytes(buf) == format::GCZ_MAGIC)
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::make_gcz;
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    /// Deterministic bytes mixing compressible runs and xorshift noise
    /// so both the deflate and stored-raw block paths get exercised.
    pub(super) fn mixed_payload(len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for (i, chunk) in out.chunks_mut(0x1000).enumerate() {
            if i % 3 == 0 {
                chunk.fill((i % 251) as u8);
            } else {
                for b in chunk.iter_mut() {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    *b = state as u8;
                }
            }
        }
        out
    }

    fn write_temp_gcz(iso: &[u8], block_size: u32) -> tempfile::NamedTempFile {
        let gcz = make_gcz(iso, block_size, 0);
        let mut f = tempfile::NamedTempFile::with_suffix(".gcz").unwrap();
        f.write_all(&gcz).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn reader_round_trips_aligned_disc() {
        let iso = mixed_payload(0x40000);
        let f = write_temp_gcz(&iso, 0x8000);
        let mut r = GczReader::open(f.path()).unwrap();
        assert_eq!(r.data_size(), iso.len() as u64);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, iso);
    }

    #[test]
    fn reader_round_trips_partial_last_block() {
        let iso = mixed_payload(0x40000 + 0x123);
        let f = write_temp_gcz(&iso, 0x8000);
        let mut r = GczReader::open(f.path()).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, iso);
    }

    #[test]
    fn reader_seeks_into_blocks() {
        let iso = mixed_payload(0x30000);
        let f = write_temp_gcz(&iso, 0x4000);
        let mut r = GczReader::open(f.path()).unwrap();
        r.seek(SeekFrom::Start(0x4000 * 3 + 17)).unwrap();
        let mut buf = [0u8; 64];
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &iso[0x4000 * 3 + 17..0x4000 * 3 + 17 + 64]);
        r.seek(SeekFrom::Start(5)).unwrap();
        r.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..], &iso[5..69]);
    }

    #[test]
    fn corrupted_block_fails_read_and_verify() {
        let iso = mixed_payload(0x20000);
        let gcz = make_gcz(&iso, 0x8000, 0);
        // Flip a byte deep inside the data section.
        let mut corrupted = gcz.clone();
        let n = corrupted.len();
        corrupted[n - 7] ^= 0xFF;
        let mut f = tempfile::NamedTempFile::with_suffix(".gcz").unwrap();
        f.write_all(&corrupted).unwrap();
        f.flush().unwrap();

        let mut r = GczReader::open(f.path()).unwrap();
        let mut out = Vec::new();
        let err = r.read_to_end(&mut out).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");

        let err = verify_gcz_blocking(
            f.path(),
            Arc::new(AtomicU64::new(0)),
            crate::util::CancelToken::new(),
        )
        .unwrap_err();
        assert!(matches!(err, GczError::BlockHashMismatch { .. }));
    }

    #[test]
    fn verify_passes_on_intact_file() {
        let iso = mixed_payload(0x20000);
        let f = write_temp_gcz(&iso, 0x8000);
        let done = Arc::new(AtomicU64::new(0));
        verify_gcz_blocking(f.path(), done.clone(), crate::util::CancelToken::new()).unwrap();
        assert_eq!(
            done.load(std::sync::atomic::Ordering::Relaxed),
            verify_total(f.path()).unwrap()
        );
    }

    #[test]
    fn verify_stops_early_when_cancelled() {
        let iso = mixed_payload(0x40000);
        let f = write_temp_gcz(&iso, 0x4000);
        let cancel = crate::util::CancelToken::new();
        cancel.cancel();
        let done = Arc::new(AtomicU64::new(0));
        let err = verify_gcz_blocking(f.path(), done.clone(), cancel).unwrap_err();
        assert!(matches!(err, GczError::Cancelled), "{err}");
        assert_eq!(done.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn detection_by_extension_and_magic() {
        let iso = mixed_payload(0x10000);
        let f = write_temp_gcz(&iso, 0x8000);
        assert!(is_gcz_input(f.path()));

        // Renamed file: magic only.
        let gcz = make_gcz(&iso, 0x8000, 0);
        let mut renamed = tempfile::NamedTempFile::with_suffix(".iso").unwrap();
        renamed.write_all(&gcz).unwrap();
        renamed.flush().unwrap();
        assert!(is_gcz_input(renamed.path()));

        let mut plain = tempfile::NamedTempFile::with_suffix(".iso").unwrap();
        plain.write_all(&iso).unwrap();
        plain.flush().unwrap();
        assert!(!is_gcz_input(plain.path()));
    }

    #[test]
    fn logical_prefix_reads_block_zero() {
        let iso = mixed_payload(0x10000);
        let f = write_temp_gcz(&iso, 0x8000);
        let prefix = gcz_logical_prefix(f.path(), 0x204).unwrap();
        assert_eq!(&prefix[..], &iso[..0x204]);
    }
}
