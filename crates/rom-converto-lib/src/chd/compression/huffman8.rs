//! MAME `huffman_8bit_decoder` port for the CHD `huff` hunk codec.
//!
//! chdman's createdvd/createhd default codec set includes `huff`, so
//! reading arbitrary chdman files needs it even though this crate's writer
//! never emits it. The format (MAME `huffman.cpp`): a 24-code/6-bit
//! "small" huffman tree describes the code lengths of the real
//! 256-code/16-bit tree (lengths shifted by one, value 0 = RLE repeat
//! of the previous length), both trees canonical; the payload is one
//! code per output byte.
//!
//! This is the same canonical-code scheme as the compressed-map
//! decoder in `chd/map.rs` but with different parameters and the
//! huffman (not RLE) tree serialization, and it needs MAME's
//! `bitstream_in` semantics: `peek` past the end of input reads
//! zeroes, only `overflow()` at the end reports truncation.

use std::io;

use crate::chd::error::ChdResult;

/// MSB-first bit reader with zero-padded lookahead, mirroring MAME's
/// `bitstream_in` so 16-bit peeks near the end of the stream behave
/// identically.
struct BitStream<'a> {
    data: &'a [u8],
    offset: usize,
    buffer: u32,
    bits: u8,
}

impl<'a> BitStream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            offset: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn peek(&mut self, numbits: u8) -> u32 {
        if numbits > self.bits {
            while self.bits <= 24 {
                if self.offset < self.data.len() {
                    self.buffer |= (self.data[self.offset] as u32) << (24 - self.bits);
                }
                self.offset += 1;
                self.bits += 8;
            }
        }
        self.buffer >> (32 - numbits)
    }

    fn remove(&mut self, numbits: u8) {
        self.buffer <<= numbits;
        self.bits -= numbits;
    }

    fn read(&mut self, numbits: u8) -> u32 {
        let value = self.peek(numbits);
        self.remove(numbits);
        value
    }

    fn overflow(&self) -> bool {
        self.offset.saturating_sub(self.bits as usize / 8) > self.data.len()
    }
}

/// Canonical huffman decoder over `lengths.len()` codes with a flat
/// `max_bits`-wide lookup table, matching MAME's
/// `assign_canonical_codes` + `build_lookup_table`.
struct CanonicalDecoder {
    lookup: Vec<(u16, u8)>,
    max_bits: u8,
}

impl CanonicalDecoder {
    fn from_lengths(lengths: &[u8], max_bits: u8) -> ChdResult<Self> {
        let mut bithisto = [0u32; 33];
        for &len in lengths {
            if len > max_bits {
                return Err(huff_err("code length exceeds maximum"));
            }
            bithisto[len as usize] += 1;
        }

        let mut curstart = 0u32;
        for codelen in (1..=32usize).rev() {
            let nextstart = (curstart + bithisto[codelen]) >> 1;
            if codelen != 1 && nextstart * 2 != curstart + bithisto[codelen] {
                return Err(huff_err("inconsistent code lengths"));
            }
            bithisto[codelen] = curstart;
            curstart = nextstart;
        }

        let mut lookup = vec![(0u16, 0u8); 1usize << max_bits];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let code = bithisto[len as usize];
            bithisto[len as usize] += 1;

            let shift = max_bits - len;
            let base = (code as usize) << shift;
            for slot in lookup.iter_mut().skip(base).take(1usize << shift) {
                *slot = (symbol as u16, len);
            }
        }

        Ok(Self { lookup, max_bits })
    }

    fn decode_one(&self, bits: &mut BitStream) -> ChdResult<u16> {
        let value = bits.peek(self.max_bits);
        let (symbol, numbits) = self.lookup[value as usize];
        if numbits == 0 {
            return Err(huff_err("invalid code in stream"));
        }
        bits.remove(numbits);
        Ok(symbol)
    }
}

/// Decode one `huff` hunk: import the huffman-encoded 256-code tree,
/// then decode `dest_len` bytes. Port of
/// `huffman_8bit_decoder::decode`.
pub(crate) fn huffman8_decode(src: &[u8], dest_len: usize) -> ChdResult<Vec<u8>> {
    const NUM_CODES: usize = 256;
    // ceil(log2(NUM_CODES - 9)) per MAME's rlefullbits derivation.
    const RLE_FULL_BITS: u8 = 8;

    let mut bits = BitStream::new(src);

    let mut small_lengths = [0u8; 24];
    small_lengths[0] = bits.read(3) as u8;
    let start = bits.read(3) as usize + 1;
    let mut count = 0u32;
    for (index, length) in small_lengths.iter_mut().enumerate().skip(1) {
        if index < start || count == 7 {
            *length = 0;
        } else {
            count = bits.read(3);
            *length = if count == 7 { 0 } else { count as u8 };
        }
    }
    let small = CanonicalDecoder::from_lengths(&small_lengths, 6)?;

    let mut lengths = [0u8; NUM_CODES];
    let mut last = 0u8;
    let mut curcode = 0usize;
    while curcode < NUM_CODES {
        let value = small.decode_one(&mut bits)?;
        if value != 0 {
            last = (value - 1) as u8;
            lengths[curcode] = last;
            curcode += 1;
        } else {
            let mut repeat = bits.read(3) + 2;
            if repeat == 7 + 2 {
                repeat += bits.read(RLE_FULL_BITS);
            }
            while repeat != 0 && curcode < NUM_CODES {
                lengths[curcode] = last;
                curcode += 1;
                repeat -= 1;
            }
        }
    }

    let decoder = CanonicalDecoder::from_lengths(&lengths, 16)?;
    let mut out = vec![0u8; dest_len];
    for byte in out.iter_mut() {
        *byte = decoder.decode_one(&mut bits)? as u8;
    }
    if bits.overflow() {
        return Err(huff_err("input buffer too small"));
    }
    Ok(out)
}

fn huff_err(msg: &str) -> crate::chd::error::ChdError {
    io::Error::other(format!("huffman hunk decode: {msg}")).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BitWriter {
        out: Vec<u8>,
        accum: u32,
        bits: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                accum: 0,
                bits: 0,
            }
        }

        fn write(&mut self, value: u32, numbits: u8) {
            for i in (0..numbits).rev() {
                self.accum = (self.accum << 1) | ((value >> i) & 1);
                self.bits += 1;
                if self.bits == 8 {
                    self.out.push(self.accum as u8);
                    self.accum = 0;
                    self.bits = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bits > 0 {
                self.out.push((self.accum << (8 - self.bits)) as u8);
            }
            self.out
        }
    }

    /// Encode `payload` with a uniform 8-bit tree (every symbol gets
    /// length 8, so the canonical code of byte `b` is `b` itself).
    /// The tree header writes the length value 9 (= 8 + 1) once via
    /// the small tree, then RLE-repeats it across all 256 codes.
    fn encode_uniform(payload: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        // Small tree: symbol 0 -> code 0, symbol 9 -> code 1, both
        // 1 bit. Layout: lengths[0]=1, start=8, then per-index
        // 3-bit lengths: index 8 = 0, index 9 = 1, index 10 = 7
        // (sentinel: all remaining are zero).
        w.write(1, 3);
        w.write(7, 3);
        w.write(0, 3);
        w.write(1, 3);
        w.write(7, 3);

        // Tree body: symbol 9 (code 1) sets length 8 for code 0;
        // symbol 0 (code 0) starts RLE: count = 7 + 2 + 246 = 255.
        w.write(1, 1);
        w.write(0, 1);
        w.write(7, 3);
        w.write(246, 8);

        for &b in payload {
            w.write(b as u32, 8);
        }
        w.finish()
    }

    #[test]
    fn decodes_uniform_tree_stream() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let encoded = encode_uniform(&payload);
        let decoded = huffman8_decode(&encoded, payload.len()).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn truncated_stream_reports_overflow() {
        let payload = vec![0xABu8; 64];
        let mut encoded = encode_uniform(&payload);
        encoded.truncate(encoded.len() - 8);
        assert!(huffman8_decode(&encoded, payload.len()).is_err());
    }
}
