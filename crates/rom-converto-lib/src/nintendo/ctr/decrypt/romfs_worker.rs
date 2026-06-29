//! Worker-pool RomFS AES-CTR decryptor.
//!
//! RomFS is plain single-key AES-128-CTR, so the keystream at any byte
//! offset is a pure function of the base counter and that offset. Each
//! chunk can therefore be decrypted independently as long as its counter
//! is advanced to the chunk's first 16-byte block. The driver in
//! [`super::cia::write_romfs_section`] reads chunks in order, hands them
//! to the pool, and writes results back in strict sequence so the output
//! stays byte-identical to a single continuous keystream pass.

use aes::cipher::{KeyIvInit, StreamCipher};

use crate::nintendo::ctr::decrypt::cia::Aes128Ctr;
use crate::nintendo::ctr::error::{NintendoCTRError, NintendoCTRResult};
use crate::util::worker_pool::Worker;

pub(super) struct RomfsChunkWork {
    pub key: [u8; 16],
    pub counter: [u8; 16],
    pub data: Vec<u8>,
}

pub(super) struct RomfsChunk {
    pub data: Vec<u8>,
}

pub(super) struct RomfsDecryptWorker;

impl Worker<RomfsChunkWork, RomfsChunk, NintendoCTRError> for RomfsDecryptWorker {
    fn process(&mut self, mut work: RomfsChunkWork) -> NintendoCTRResult<RomfsChunk> {
        let mut cipher = Aes128Ctr::new_from_slices(&work.key, &work.counter)
            .map_err(|e| NintendoCTRError::IoError(std::io::Error::other(e.to_string())))?;
        cipher.apply_keystream(&mut work.data);
        Ok(RomfsChunk { data: work.data })
    }
}

/// Advance a 16-byte big-endian CTR counter by `byte_offset / 16` blocks.
/// The 3DS base counter is a full 128-bit value, so the per-chunk counter
/// is `u128::from_be_bytes(base).wrapping_add(blocks)`.
pub(super) fn advance_counter(base: &[u8; 16], byte_offset: u64) -> [u8; 16] {
    let blocks = (byte_offset / 16) as u128;
    u128::from_be_bytes(*base)
        .wrapping_add(blocks)
        .to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_counter_matches_continuous_stream() {
        let key = [0x11u8; 16];
        let base = [0x00u8; 16];
        let plaintext: Vec<u8> = (0u32..4096).map(|i| (i % 251) as u8).collect();

        let mut whole = plaintext.clone();
        Aes128Ctr::new_from_slices(&key, &base)
            .unwrap()
            .apply_keystream(&mut whole);

        let split = 1024usize;
        let mut first = plaintext[..split].to_vec();
        let mut second = plaintext[split..].to_vec();

        let mut w0 = RomfsDecryptWorker;
        let mut w1 = RomfsDecryptWorker;
        first = w0
            .process(RomfsChunkWork {
                key,
                counter: advance_counter(&base, 0),
                data: first,
            })
            .unwrap()
            .data;
        second = w1
            .process(RomfsChunkWork {
                key,
                counter: advance_counter(&base, split as u64),
                data: second,
            })
            .unwrap()
            .data;

        let mut chunked = first;
        chunked.extend_from_slice(&second);
        assert_eq!(chunked, whole);
    }

    #[test]
    fn advance_counter_wraps_past_block_boundary() {
        let base = [0xFFu8; 16];
        let advanced = advance_counter(&base, 16);
        assert_eq!(advanced, [0u8; 16]);
    }
}
