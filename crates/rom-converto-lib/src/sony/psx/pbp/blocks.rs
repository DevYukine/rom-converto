//! Block encodings for a PS1 EBOOT's disc data: raw deflate, compressed or
//! stored, since custom firmware inflates every block.

use flate2::{Compress, Compression, FlushCompress, Status};

use super::error::{PsxPbpError, PsxPbpResult};
use super::format::BLOCK_SIZE;
use crate::util::worker_pool::Worker;

/// A final, uncompressed deflate block of `BLOCK_SIZE` bytes: `BFINAL = 1`,
/// `BTYPE = 00`, then `LEN` and `NLEN` little-endian.
const STORED_HEADER: [u8; 5] = [0x01, 0x00, 0x93, 0xFF, 0x6C];

/// Bytes of a block written as stored deflate.
pub(crate) const STORED_BLOCK_LEN: usize = BLOCK_SIZE + STORED_HEADER.len();

/// How each block of disc data is written into the EBOOT.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlockEncoding {
    /// Deflate-compressed: smaller, costs a compression pass.
    #[default]
    Deflate,
    /// Stored deflate: the sectors unchanged behind a 5-byte header.
    Stored,
}

impl std::str::FromStr for BlockEncoding {
    type Err = PsxPbpError;

    fn from_str(name: &str) -> PsxPbpResult<Self> {
        match name {
            "deflate" => Ok(Self::Deflate),
            "stored" => Ok(Self::Stored),
            other => Err(PsxPbpError::UnknownEncoding(other.to_string())),
        }
    }
}

/// Encodes blocks with one reused compressor and output buffer.
pub(crate) struct BlockEncoder {
    compress: Option<Compress>,
    out: Vec<u8>,
}

impl BlockEncoder {
    /// An encoder for `encoding`; deflate keeps one compressor, reset per
    /// block.
    pub fn new(encoding: BlockEncoding) -> Self {
        Self {
            compress: (encoding == BlockEncoding::Deflate)
                .then(|| Compress::new(Compression::default(), false)),
            out: Vec::with_capacity(STORED_BLOCK_LEN),
        }
    }

    /// Encodes one `BLOCK_SIZE`-byte block.
    pub fn encode(&mut self, block: &[u8]) -> &[u8] {
        debug_assert_eq!(block.len(), BLOCK_SIZE);
        self.out.clear();
        if let Some(compress) = self.compress.as_mut() {
            compress.reset();
            // Output stops at the buffer's capacity, so a block that would
            // not shrink never finishes the stream.
            let finished = matches!(
                compress.compress_vec(block, &mut self.out, FlushCompress::Finish),
                Ok(Status::StreamEnd)
            );
            if finished && self.out.len() < BLOCK_SIZE {
                return &self.out;
            }
            self.out.clear();
        }
        self.out.extend_from_slice(&STORED_HEADER);
        self.out.extend_from_slice(block);
        &self.out
    }
}

/// One encoder per pool worker, so each keeps its compressor across blocks.
impl Worker<Vec<u8>, Vec<u8>, PsxPbpError> for BlockEncoder {
    fn process(&mut self, block: Vec<u8>) -> PsxPbpResult<Vec<u8>> {
        Ok(self.encode(&block).to_vec())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn inflate(stored: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::DeflateDecoder::new(stored)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn noise() -> Vec<u8> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..BLOCK_SIZE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn stored_blocks_are_valid_deflate() {
        let block = noise();
        let mut encoder = BlockEncoder::new(BlockEncoding::Stored);
        let stored = encoder.encode(&block).to_vec();
        assert_eq!(stored.len(), STORED_BLOCK_LEN);
        assert_eq!(inflate(&stored), block);
    }

    #[test]
    fn deflate_shrinks_a_compressible_block() {
        let block: Vec<u8> = (0..BLOCK_SIZE).map(|i| (i / 64) as u8).collect();
        let mut encoder = BlockEncoder::new(BlockEncoding::Deflate);
        let stored = encoder.encode(&block).to_vec();
        assert!(stored.len() < BLOCK_SIZE);
        assert_eq!(inflate(&stored), block);
        // The compressor is reset between blocks.
        assert_eq!(encoder.encode(&block), stored.as_slice());
    }

    #[test]
    fn deflate_stores_a_block_that_does_not_shrink() {
        let block = noise();
        let mut encoder = BlockEncoder::new(BlockEncoding::Deflate);
        let stored = encoder.encode(&block).to_vec();
        assert_eq!(stored.len(), STORED_BLOCK_LEN);
        assert_eq!(inflate(&stored), block);
    }
}
