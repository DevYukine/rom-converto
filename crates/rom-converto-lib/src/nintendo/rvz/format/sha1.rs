//! SHA-1 integrity helpers for the WIA/RVZ header structs.
//!
//! Each helper serialises the struct directly into a `Sha1Writer`, which
//! feeds bytes to the hasher without any intermediate `Vec` allocation.

use crate::nintendo::rvz::format::{WiaDisc, WiaFileHead, WiaPart};
use binrw::{BinWrite, Endian};
use sha1::{Digest, Sha1};
use std::io::{Result as IoResult, Seek, SeekFrom, Write};

/// `std::io::Write` adapter that streams every byte into a SHA-1 hasher.
/// Implements `Seek` only to satisfy binrw's bounds; the cursor it tracks
/// is purely positional, never used to rewind the hasher.
struct Sha1Writer {
    hasher: Sha1,
    pos: u64,
}

impl Sha1Writer {
    fn new() -> Self {
        Self {
            hasher: Sha1::new(),
            pos: 0,
        }
    }

    fn finalize(self) -> [u8; 20] {
        self.hasher.finalize().into()
    }
}

impl Write for Sha1Writer {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.hasher.update(buf);
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

impl Seek for Sha1Writer {
    fn seek(&mut self, pos: SeekFrom) -> IoResult<u64> {
        // binrw needs Seek for stream_position; we never actually rewind a
        // hash so any non-current position is rejected.
        match pos {
            SeekFrom::Current(0) => Ok(self.pos),
            SeekFrom::Start(p) if p == self.pos => Ok(self.pos),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Sha1Writer only supports forward sequential writes",
            )),
        }
    }
    fn stream_position(&mut self) -> IoResult<u64> {
        Ok(self.pos)
    }
}

fn hash_via<F>(write: F) -> [u8; 20]
where
    F: FnOnce(&mut Sha1Writer) -> binrw::BinResult<()>,
{
    let mut w = Sha1Writer::new();
    write(&mut w).expect("infallible serialisation");
    w.finalize()
}

/// SHA-1 of [`WiaFileHead`] over the first
/// `WIA_FILE_HEAD_SIZE - 20` bytes, i.e. everything before the
/// `file_head_hash` field itself. Matches Dolphin's
/// `CalculateDigest(&header_1, offsetof(WIAHeader1, header_1_hash))`
/// in `Source/Core/DiscIO/WIABlob.cpp`.
///
/// Note: this is NOT "hash the full struct with the hash field zeroed".
/// The trailing 20 bytes are excluded from the hash input, not
/// zero-filled. `SHA1(52 bytes)` is not equal to `SHA1(52 bytes || 20 × 0x00)`.
pub fn compute_file_head_hash(head: &WiaFileHead) -> [u8; 20] {
    use std::io::Cursor;
    let mut buf = Vec::with_capacity(super::WIA_FILE_HEAD_SIZE);
    head.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
        .expect("infallible serialisation");
    debug_assert_eq!(buf.len(), super::WIA_FILE_HEAD_SIZE);
    let hashed_len = super::WIA_FILE_HEAD_SIZE - 20;
    let mut h = Sha1::new();
    h.update(&buf[..hashed_len]);
    h.finalize().into()
}

/// SHA-1 of the full serialised [`WiaDisc`].
pub fn compute_disc_hash(disc: &WiaDisc) -> [u8; 20] {
    hash_via(|w| disc.write_options(w, Endian::Big, ()))
}

/// SHA-1 of a packed array of [`WiaPart`] structs.
pub fn compute_part_hash(parts: &[WiaPart]) -> [u8; 20] {
    let mut w = Sha1Writer::new();
    for part in parts {
        part.write_options(&mut w, Endian::Big, ())
            .expect("infallible serialisation");
    }
    w.finalize()
}

/// Test-only reference path: serialise the struct to a `Vec<u8>`, then
/// hash the buffer. Used to cross-check [`Sha1Writer`]'s streaming
/// output against the straightforward implementation.
#[cfg(test)]
fn disc_hash_via_buffer(disc: &WiaDisc) -> [u8; 20] {
    use std::io::Cursor;
    let mut buf = Vec::with_capacity(super::WIA_DISC_SIZE);
    disc.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
        .unwrap();
    let mut h = Sha1::new();
    h.update(&buf);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::rvz::constants::{MIN_CHUNK_SIZE, RVZ_MAGIC};

    fn sample_disc() -> WiaDisc {
        WiaDisc {
            disc_type: 1,
            compression: 5,
            compr_level: 5,
            chunk_size: MIN_CHUNK_SIZE,
            dhead: [0u8; 128],
            n_part: 0,
            part_t_size: super::super::WIA_PART_SIZE as u32,
            part_off: 0,
            part_hash: [0u8; 20],
            n_raw_data: 0,
            raw_data_off: 0,
            raw_data_size: 0,
            n_groups: 0,
            group_off: 0,
            group_size: 0,
            compr_data_len: 0,
            compr_data: [0u8; 7],
        }
    }

    #[test]
    fn file_head_hash_ignores_existing_hash_field() {
        let head = WiaFileHead {
            magic: RVZ_MAGIC,
            version: 0x01000000,
            version_compatible: 0x00030000,
            disc_size: super::super::WIA_DISC_SIZE as u32,
            disc_hash: [0xAAu8; 20],
            iso_file_size: 1024,
            wia_file_size: 2048,
            file_head_hash: [0u8; 20],
        };
        let h1 = compute_file_head_hash(&head);

        let mut head2 = head.clone();
        head2.file_head_hash = [0xFFu8; 20];
        let h2 = compute_file_head_hash(&head2);

        assert_eq!(h1, h2);
    }

    #[test]
    fn streaming_disc_hash_matches_buffer_reference() {
        let d = sample_disc();
        assert_eq!(compute_disc_hash(&d), disc_hash_via_buffer(&d));
    }

    #[test]
    fn disc_hash_differs_when_field_changes() {
        let a = compute_disc_hash(&sample_disc());
        let mut d2 = sample_disc();
        d2.disc_type = 2;
        let b = compute_disc_hash(&d2);
        assert_ne!(a, b);
    }

    #[test]
    fn part_hash_of_empty_slice_matches_sha1_of_empty() {
        let mut h = Sha1::new();
        h.update(&[] as &[u8]);
        let expected: [u8; 20] = h.finalize().into();
        assert_eq!(compute_part_hash(&[]), expected);
    }
}
