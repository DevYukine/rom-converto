//! NKit integrity verification.
//!
//! NKit patches a 4-byte fix-up into its header so the CRC-32 of the
//! whole nkit file equals the CRC-32 of the original source image
//! (`CrcForce.Calculate` in NKit's source). One sequential pass over
//! the container therefore self-checks the file with no external
//! database: for `.nkit.iso` the hash covers the file itself, for
//! `.nkit.gcz` it covers the GCZ container (whose fix-up lives at
//! offset 0x4), checked after the GCZ block checksums.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::crc::Crc32;
use super::error::{NkitError, NkitResult};
use super::format::NkitHeader;
use crate::nintendo::gcz;
use crate::util::CancelToken;

const READ_CHUNK: usize = 4 * 1024 * 1024;

/// Whole-file CRC-32 against the stored source CRC. `wrapped_in_gcz`
/// selects where the NKit header is read from (the decompressed
/// stream) while the hash always covers the file as stored.
pub fn verify_nkit_blocking(
    path: &Path,
    wrapped_in_gcz: bool,
    bytes_done: Arc<AtomicU64>,
    cancel: CancelToken,
) -> NkitResult<()> {
    let source_crc = read_source_crc(path, wrapped_in_gcz)?;
    if wrapped_in_gcz {
        gcz::verify_gcz_blocking(path, bytes_done.clone(), cancel.clone())?;
    }

    let mut f = File::open(path)?;
    let mut crc = Crc32::new();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(NkitError::Cancelled);
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
        bytes_done.fetch_add(n as u64, Ordering::Relaxed);
    }
    if crc.value() != source_crc {
        return Err(NkitError::CrcMismatch {
            what: "the nkit container",
            stored: source_crc,
            computed: crc.value(),
        });
    }
    Ok(())
}

fn read_source_crc(path: &Path, wrapped_in_gcz: bool) -> NkitResult<u32> {
    let dhead = if wrapped_in_gcz {
        gcz::gcz_logical_prefix(path, 0x440)?
    } else {
        let mut f = File::open(path)?;
        let mut d = vec![0u8; 0x440];
        f.read_exact(&mut d)?;
        d
    };
    Ok(NkitHeader::parse(&dhead)?.source_crc)
}

/// Verification work total: the container bytes hashed (plus the GCZ
/// checksum pass when wrapped).
pub fn verify_total(path: &Path, wrapped_in_gcz: bool) -> NkitResult<u64> {
    let file_len = std::fs::metadata(path)?.len();
    Ok(if wrapped_in_gcz {
        file_len + gcz::verify_total(path)?
    } else {
        file_len
    })
}
