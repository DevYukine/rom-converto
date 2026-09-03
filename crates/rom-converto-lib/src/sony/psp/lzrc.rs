//! The LZRC range decoder an `NPUMDIMG` compressed block uses.

use anyhow::{Result, bail};

// The probability tables are addressed as one flat array with the layout the
// reference decoder's struct has, because its bit-tree walks deliberately run
// past the end of a row into the next table.
const BM_LITERAL: usize = 0;
const BM_DIST_BITS: usize = BM_LITERAL + 8 * 256;
const BM_DIST: usize = BM_DIST_BITS + 8 * 39;
const BM_MATCH: usize = BM_DIST + 18 * 8;
const BM_LEN: usize = BM_MATCH + 8 * 8;
const PROB_COUNT: usize = BM_LEN + 8 * 31;

/// Decompresses the LZRC stream `input` into `out`, returning the number of
/// bytes written.
///
/// # Errors
/// Returns an error if the stream is truncated, addresses a match before the
/// start of the output, or produces more bytes than `out` holds.
pub fn decompress(input: &[u8], out: &mut [u8]) -> Result<usize> {
    if input.len() < 5 {
        bail!(
            "lzrc: stream is {} bytes, shorter than the 5-byte header",
            input.len()
        );
    }

    let mut rc = Decoder {
        input,
        in_ptr: 5,
        range: 0xffff_ffff,
        code: u32::from_be_bytes(input[1..5].try_into().expect("4-byte slice")),
        probs: vec![0x80; PROB_COUNT],
    };
    let lc = input[0];

    if lc & 0x80 != 0 {
        let len = rc.code as usize;
        let Some(src) = input.get(5..).and_then(|rest| rest.get(..len)) else {
            bail!(
                "lzrc: stored block of {len} bytes runs past the {}-byte stream",
                input.len()
            );
        };
        let Some(dst) = out.get_mut(..len) else {
            bail!(
                "lzrc: stored block of {len} bytes exceeds the {}-byte output",
                out.len()
            );
        };
        dst.copy_from_slice(src);
        return Ok(len);
    }

    let mut state = 0usize;
    let mut last_byte = 0u8;
    let mut out_ptr = 0usize;

    loop {
        let mut match_step = 0usize;
        if rc.bit(BM_MATCH + state * 8 + match_step)? == 0 {
            state = state.saturating_sub(1);

            let row = (last_byte.checked_shr(u32::from(lc)).unwrap_or(0) & 0x07) as usize;
            let byte = rc.bittree(BM_LITERAL + row * 256, 0x100)? - 0x100;
            let Some(slot) = out.get_mut(out_ptr) else {
                bail!("lzrc: literal overflows the {}-byte output", out.len());
            };
            *slot = byte as u8;
            last_byte = byte as u8;
            out_ptr += 1;
            continue;
        }

        let mut len_bits = 0u32;
        for _ in 0..7 {
            match_step += 1;
            if rc.bit(BM_MATCH + state * 8 + match_step)? == 0 {
                break;
            }
            len_bits += 1;
        }

        let match_len = if len_bits == 0 {
            1
        } else {
            let len_state =
                (((len_bits - 1) << 2) + (((out_ptr as u32) << (len_bits - 1)) & 0x03)) as usize;
            let n = rc.number(BM_LEN + state * 31 + len_state, len_bits)?;
            if n == 0xFF {
                return Ok(out_ptr);
            }
            n
        };

        let (dist_state, limit) = if match_len > 2 {
            (7usize, 44)
        } else {
            (0usize, 8)
        };
        let dist_bits =
            rc.bittree(BM_DIST_BITS + len_bits as usize * 39 + dist_state, limit)? - limit;
        let match_dist = if dist_bits > 0 {
            rc.number(BM_DIST + dist_bits as usize * 8, dist_bits)?
        } else {
            1
        };

        let dist = match_dist as usize;
        if dist > out_ptr {
            bail!("lzrc: match distance {dist} reaches before the start of the output");
        }
        let end = out_ptr + match_len as usize + 1;
        if end > out.len() {
            bail!("lzrc: match overflows the {}-byte output", out.len());
        }
        let mut src = out_ptr - dist;
        while out_ptr < end {
            out[out_ptr] = out[src];
            out_ptr += 1;
            src += 1;
        }
        last_byte = out[src - 1];
        state = 6 + ((out_ptr + 1) & 1);
    }
}

struct Decoder<'a> {
    input: &'a [u8],
    in_ptr: usize,
    range: u32,
    code: u32,
    probs: Vec<u8>,
}

impl Decoder<'_> {
    fn normalize(&mut self) -> Result<()> {
        if self.range < 0x0100_0000 {
            let Some(&byte) = self.input.get(self.in_ptr) else {
                bail!("lzrc: stream ends after {} bytes", self.input.len());
            };
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(byte);
            self.in_ptr += 1;
        }
        Ok(())
    }

    fn bit(&mut self, index: usize) -> Result<u32> {
        self.normalize()?;
        let Some(&stored) = self.probs.get(index) else {
            bail!("lzrc: probability index {index} out of range");
        };

        let mut prob = u32::from(stored);
        let bound = (self.range >> 8) * prob;
        prob -= prob >> 3;

        let bit = if self.code < bound {
            self.range = bound;
            prob += 31;
            1
        } else {
            self.code -= bound;
            self.range -= bound;
            0
        };
        self.probs[index] = prob as u8;
        Ok(bit)
    }

    fn bittree(&mut self, base: usize, limit: u32) -> Result<u32> {
        let mut number = 1u32;
        loop {
            let bit = self.bit(base + number as usize)?;
            number = (number << 1) + bit;
            if number >= limit {
                return Ok(number);
            }
        }
    }

    fn number(&mut self, base: usize, n: u32) -> Result<u32> {
        let mut number = 1u32;

        if n > 3 {
            number = (number << 1) + self.bit(base + 3)?;
            if n > 4 {
                number = (number << 1) + self.bit(base + 3)?;
                if n > 5 {
                    self.normalize()?;
                    for _ in 0..n - 5 {
                        self.range >>= 1;
                        number = number.wrapping_shl(1);
                        if self.code < self.range {
                            number = number.wrapping_add(1);
                        } else {
                            self.code -= self.range;
                        }
                    }
                }
            }
        }

        if n > 0 {
            number = number.wrapping_shl(1).wrapping_add(self.bit(base)?);
            if n > 1 {
                number = number.wrapping_shl(1).wrapping_add(self.bit(base + 1)?);
                if n > 2 {
                    number = number.wrapping_shl(1).wrapping_add(self.bit(base + 2)?);
                }
            }
        }

        Ok(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_stream_shorter_than_the_header() {
        for len in 0..5 {
            let mut out = [0u8; 64];
            assert!(decompress(&vec![0u8; len], &mut out).is_err());
        }
    }

    #[test]
    fn copies_a_stored_block_verbatim() {
        let payload = b"stored, not compressed";
        let mut input = vec![0x80];
        input.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        input.extend_from_slice(payload);

        let mut out = [0u8; 64];
        let n = decompress(&input, &mut out).expect("stored block");
        assert_eq!(&out[..n], payload);
    }

    #[test]
    fn rejects_a_stored_block_that_runs_past_the_stream() {
        let mut input = vec![0x80];
        input.extend_from_slice(&64u32.to_be_bytes());
        input.extend_from_slice(b"short");
        let mut out = [0u8; 64];
        assert!(decompress(&input, &mut out).is_err());
    }

    #[test]
    fn rejects_a_stored_block_larger_than_the_output() {
        let mut input = vec![0x80];
        input.extend_from_slice(&64u32.to_be_bytes());
        input.extend_from_slice(&[0u8; 64]);
        let mut out = [0u8; 16];
        assert!(decompress(&input, &mut out).is_err());
    }

    /// Malformed streams must fail cleanly instead of panicking or looping
    /// forever, whatever the byte pattern.
    #[test]
    fn malformed_streams_error_without_panicking() {
        let mut seed = 0x1234_5678u32;
        for case in 0..400 {
            let len = 5 + (case % 64);
            let input: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            let mut out = vec![0u8; 2048];
            let _ = decompress(&input, &mut out);
        }
    }

    #[test]
    fn truncations_of_a_stored_stream_error_without_panicking() {
        let mut input = vec![0x00];
        input.extend_from_slice(&[0xAA; 128]);
        for len in 5..input.len() {
            let mut out = vec![0u8; 512];
            let _ = decompress(&input[..len], &mut out);
        }
    }
}
