//! Structural (fast) verification of an RVZ container's stored SHA-1 hashes.
//!
//! Shared by the GameCube ([`crate::nintendo::dol::verify`]) and Wii
//! ([`crate::nintendo::rvl::verify`]) verify paths. Re-reads the file header,
//! disc struct and partition table and checks all three stored SHA-1 digests
//! without decompressing any group data. Unlike [`super::decompress::RvzDiscReader`]
//! this reports each hash independently instead of erroring on the first
//! mismatch, and it also checks the partition-table hash that the reader skips.

use binrw::{BinRead, Endian};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::nintendo::rvz::constants::RVZ_MAGIC;
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use crate::nintendo::rvz::format::sha1::{
    compute_disc_hash, compute_file_head_hash, compute_part_hash,
};
use crate::nintendo::rvz::format::{
    WIA_FILE_HEAD_SIZE, WIA_PART_SIZE, WiaDisc, WiaFileHead, WiaPart,
};

/// Result of verifying the three SHA-1 hashes an RVZ container stores over its
/// own metadata structs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RvzStructuralVerify {
    pub file_head_hash_ok: bool,
    pub disc_hash_ok: bool,
    /// `None` when the container declares no partition table (`n_part == 0`).
    pub part_hash_ok: Option<bool>,
    /// 1 = GameCube, 2 = Wii.
    pub disc_type: u32,
    pub iso_size: u64,
    pub n_part: u32,
}

impl RvzStructuralVerify {
    pub fn ok(&self) -> bool {
        self.file_head_hash_ok && self.disc_hash_ok && self.part_hash_ok != Some(false)
    }
}

/// Verify the stored SHA-1 digests of an RVZ container. Errors with
/// [`RvzError::InvalidMagic`] when `path` is not an RVZ file, which callers
/// treat as "no structural data to check".
pub fn verify_rvz_structure(path: &Path) -> RvzResult<RvzStructuralVerify> {
    let mut reader = BufReader::with_capacity(64 * 1024, File::open(path)?);

    let mut head_bytes = vec![0u8; WIA_FILE_HEAD_SIZE];
    reader.read_exact(&mut head_bytes)?;
    let head = WiaFileHead::read_options(&mut Cursor::new(&head_bytes), Endian::Big, ())?;
    if head.magic != RVZ_MAGIC {
        return Err(RvzError::InvalidMagic(head.magic));
    }
    let file_head_hash_ok = compute_file_head_hash(&head) == head.file_head_hash;

    let mut disc_bytes = vec![0u8; head.disc_size as usize];
    reader.read_exact(&mut disc_bytes)?;
    let disc = WiaDisc::read_options(&mut Cursor::new(&disc_bytes), Endian::Big, ())?;
    let disc_hash_ok = compute_disc_hash(&disc) == head.disc_hash;

    let part_hash_ok = if disc.n_part > 0 {
        reader.seek(SeekFrom::Start(disc.part_off))?;
        let mut buf = vec![0u8; disc.n_part as usize * WIA_PART_SIZE];
        reader.read_exact(&mut buf)?;
        let mut cur = Cursor::new(&buf);
        let mut parts = Vec::with_capacity(disc.n_part as usize);
        for _ in 0..disc.n_part {
            parts.push(WiaPart::read_options(&mut cur, Endian::Big, ())?);
        }
        Some(compute_part_hash(&parts) == disc.part_hash)
    } else {
        None
    };

    Ok(RvzStructuralVerify {
        file_head_hash_ok,
        disc_hash_ok,
        part_hash_ok,
        disc_type: disc.disc_type,
        iso_size: head.iso_file_size,
        n_part: disc.n_part,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(file: bool, disc: bool, part: Option<bool>) -> RvzStructuralVerify {
        RvzStructuralVerify {
            file_head_hash_ok: file,
            disc_hash_ok: disc,
            part_hash_ok: part,
            disc_type: 2,
            iso_size: 0,
            n_part: part.map(|_| 1).unwrap_or(0),
        }
    }

    #[test]
    fn ok_requires_head_and_disc_and_part_not_false() {
        assert!(mk(true, true, Some(true)).ok());
        assert!(mk(true, true, None).ok());
        assert!(!mk(false, true, Some(true)).ok());
        assert!(!mk(true, false, Some(true)).ok());
        assert!(!mk(true, true, Some(false)).ok());
    }
}
