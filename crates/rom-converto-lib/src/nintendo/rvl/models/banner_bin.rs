//! Nintendo IMD5 wrapper + LZ77 (LZSS, type 0x10) decompressor.
//!
//! Wii channel `banner.bin` / `icon.bin` files are stored as an IMD5
//! wrapper (0x20 bytes: magic + 4-byte size + 8 reserved + 0x10 MD5)
//! around payload bytes that are either raw or LZ77-compressed. The
//! LZ77 marker is a 4-byte header at the payload start: byte 0 is
//! `0x10` (LZSS type), followed by a 24-bit little-endian uncompressed
//! size.
//!
//! Reference: wiibrew.org/wiki/U8_archive#IMD5,
//! github.com/rvanasa/lz77.

use anyhow::{Result, anyhow};

pub const IMD5_MAGIC: [u8; 4] = *b"IMD5";
pub const IMD5_HEADER_SIZE: usize = 0x20;
pub const LZ77_MARKER: u8 = 0x10;
pub const LZ77_ASCII_MAGIC: [u8; 4] = *b"LZ77";

/// Strip the IMD5 wrapper if present, returning the payload bytes.
pub fn strip_imd5(buf: &[u8]) -> &[u8] {
    if buf.len() >= IMD5_HEADER_SIZE && buf[..4] == IMD5_MAGIC {
        &buf[IMD5_HEADER_SIZE..]
    } else {
        buf
    }
}

/// Decode the input as LZ77 (LZSS type 0x10). If the payload is not
/// LZ77-marked, returns the input bytes copied unchanged.
pub fn maybe_decompress_lz77(payload: &[u8]) -> Result<Vec<u8>> {
    if payload.is_empty() || payload[0] != LZ77_MARKER {
        return Ok(payload.to_vec());
    }
    decompress_lz77(payload)
}

/// Strip the 4-byte "LZ77" ASCII magic that Wii disc `opening.bnr`
/// inner files use to flag a following LZSS stream. Pass-through when
/// the magic is absent (some titles ship the inner U8 archive raw).
pub fn maybe_decompress_lz77_ascii(payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() >= 4 && payload[..4] == LZ77_ASCII_MAGIC {
        return decompress_lz77(&payload[4..]);
    }
    Ok(payload.to_vec())
}

fn decompress_lz77(input: &[u8]) -> Result<Vec<u8>> {
    if input.len() < 4 || input[0] != LZ77_MARKER {
        return Err(anyhow!("LZ77 input missing 0x10 marker"));
    }
    let uncompressed_len = u32::from_le_bytes([input[1], input[2], input[3], 0]) as usize;
    let mut out = Vec::with_capacity(uncompressed_len);
    let mut cursor = 4usize;
    while out.len() < uncompressed_len {
        if cursor >= input.len() {
            return Err(anyhow!("LZ77 truncated input"));
        }
        let flag = input[cursor];
        cursor += 1;
        for bit in (0..8).rev() {
            if out.len() >= uncompressed_len {
                break;
            }
            if cursor >= input.len() {
                return Err(anyhow!("LZ77 truncated mid-block"));
            }
            if (flag & (1 << bit)) != 0 {
                if cursor + 1 >= input.len() {
                    return Err(anyhow!("LZ77 back-reference truncated"));
                }
                let b0 = input[cursor];
                let b1 = input[cursor + 1];
                cursor += 2;
                let length = ((b0 >> 4) as usize) + 3;
                let distance = (((b0 & 0x0F) as usize) << 8 | b1 as usize) + 1;
                if distance > out.len() {
                    return Err(anyhow!(
                        "LZ77 distance {} exceeds output length {}",
                        distance,
                        out.len()
                    ));
                }
                let copy_start = out.len() - distance;
                for i in 0..length {
                    let byte = out[copy_start + i];
                    out.push(byte);
                    if out.len() >= uncompressed_len {
                        break;
                    }
                }
            } else {
                out.push(input[cursor]);
                cursor += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_imd5_passthrough_when_no_magic() {
        let raw = vec![1u8, 2, 3, 4, 5];
        assert_eq!(strip_imd5(&raw), &raw[..]);
    }

    #[test]
    fn strip_imd5_drops_header_when_present() {
        let mut buf = vec![0u8; IMD5_HEADER_SIZE + 4];
        buf[..4].copy_from_slice(&IMD5_MAGIC);
        buf[IMD5_HEADER_SIZE..].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(strip_imd5(&buf), &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn lz77_uncompressed_passthrough() {
        let raw = vec![0xAAu8, 0xBB, 0xCC];
        let out = maybe_decompress_lz77(&raw).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn lz77_decodes_literal_block() {
        // 8 literals (flag=0x00) of bytes 1..=8.
        let mut input = vec![LZ77_MARKER, 8, 0, 0]; // uncompressed size = 8
        input.push(0x00); // flag: all literals
        input.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let out = decompress_lz77(&input).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn lz77_decodes_back_reference() {
        // Encode 'AAAAAAAA' (8 bytes): first as literal, rest as a
        // back-reference (length=7, distance=1).
        let mut input = vec![LZ77_MARKER, 8, 0, 0]; // size 8
        // Flag: literal then back-reference: 0b01000000 = 0x40
        input.push(0x40);
        input.push(b'A'); // literal
        // Back-reference: length=7 (so byte0 high nibble = 7-3 = 4), distance=1 (so 12-bit value = 1-1 = 0)
        input.push(0x40);
        input.push(0x00);
        // Pad with 6 more bytes to satisfy the 8-flag block (won't be read once size reached)
        input.extend_from_slice(&[0; 6]);
        let out = decompress_lz77(&input).unwrap();
        assert_eq!(out, vec![b'A'; 8]);
    }

    #[test]
    fn lz77_rejects_missing_marker() {
        let input = vec![0x11, 0, 0, 0]; // wrong marker (0x11 = LZSS extended)
        assert!(decompress_lz77(&input).is_err());
    }

    #[test]
    fn lz77_ascii_strips_magic_and_decompresses() {
        // "LZ77" prefix + the same all-literals LZSS stream as the
        // basic test above.
        let mut input = b"LZ77".to_vec();
        input.extend_from_slice(&[LZ77_MARKER, 8, 0, 0]);
        input.push(0x00);
        input.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let out = maybe_decompress_lz77_ascii(&input).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn lz77_ascii_passthrough_without_magic() {
        let raw = vec![0x55, 0xAA, 0x38, 0x2D, 0, 1, 2, 3];
        let out = maybe_decompress_lz77_ascii(&raw).unwrap();
        assert_eq!(out, raw);
    }
}
