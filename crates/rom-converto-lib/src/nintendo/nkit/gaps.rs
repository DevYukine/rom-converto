//! NKit gap record decoding.
//!
//! Between files (in FST order) NKit replaces the original padding
//! with a compact record (`Gaps.cs`):
//!
//! ```text
//! u32 BE header: bits[1:0] = type, bits[31:2] = gap size (4-aligned)
//!   type 0 AllJunk:     the whole gap is junk-stream output
//!   type 1 AllScrubbed: the whole gap is zeroes
//!   type 2 Mixed:       sub-records follow
//!   type 3 JunkFile:    a file consisting entirely of junk (the
//!                       header carries a leading-NUL count instead
//!                       of a size; a u32 file length follows)
//! ```
//!
//! A header size field of 0xFFFFFFFC is followed by a u32 extension
//! holding the remainder. Mixed sub-records are u32 BE:
//! `bits[31:30]` = kind (0 Junk, 1 NonJunk, 2 ByteFill, 3 Repeat);
//! Junk/NonJunk/Repeat carry a 30-bit count of 256-byte blocks;
//! ByteFill carries a 22-bit block count in bits[29:8] and the fill
//! byte in bits[7:0]. NonJunk is followed by the verbatim bytes
//! (clipped at the gap end). Repeat extends the previous kind.

use std::io::{Read, Seek, SeekFrom};

use super::error::{NkitError, NkitResult};

pub(crate) const GAP_BLOCK_SIZE: u64 = 0x100;
const SIZE_EXTENSION_SENTINEL: u32 = 0xFFFF_FFFC;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GapPiece {
    Junk { len: u64 },
    Zeros { len: u64 },
    ByteFill { len: u64, byte: u8 },
    Verbatim { nkit_off: u64, len: u64 },
}

impl GapPiece {
    pub(crate) fn len(&self) -> u64 {
        match *self {
            GapPiece::Junk { len }
            | GapPiece::Zeros { len }
            | GapPiece::ByteFill { len, .. }
            | GapPiece::Verbatim { len, .. } => len,
        }
    }
}

#[derive(Debug)]
pub(crate) struct GapRecord {
    /// Bytes the record occupies in the nkit stream.
    pub consumed: u64,
    /// Bytes the record expands to in the restored image.
    #[cfg_attr(not(test), allow(dead_code))]
    pub out_len: u64,
    pub pieces: Vec<GapPiece>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct JunkFileRecord {
    pub consumed: u64,
    pub leading_nulls: u64,
    pub file_len: u64,
}

fn read_u32_be<R: Read>(r: &mut R) -> NkitResult<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

/// Peek the 2-bit record type at `off` without consuming.
pub(crate) fn peek_record_type<R: Read + Seek>(r: &mut R, off: u64) -> NkitResult<u8> {
    r.seek(SeekFrom::Start(off))?;
    Ok((read_u32_be(r)? & 3) as u8)
}

/// Parse one inter-file gap record at `record_off`.
pub(crate) fn parse_gap_record<R: Read + Seek>(
    r: &mut R,
    record_off: u64,
) -> NkitResult<GapRecord> {
    r.seek(SeekFrom::Start(record_off))?;
    let hdr = read_u32_be(r)?;
    let gap_type = hdr & 3;
    let mut size = (hdr & !3) as u64;
    let mut consumed = 4u64;
    if (hdr & !3) == SIZE_EXTENSION_SENTINEL {
        size += read_u32_be(r)? as u64;
        consumed += 4;
    }

    let pieces = match gap_type {
        0 => vec![GapPiece::Junk { len: size }],
        1 => vec![GapPiece::Zeros { len: size }],
        2 => {
            let mut pieces = Vec::new();
            let mut acc = 0u64;
            let mut prev_kind = 0u32;
            let mut prev_byte = 0u8;
            while acc < size {
                let w = read_u32_be(r)?;
                consumed += 4;
                let mut kind = w >> 30;
                let (blocks, byte) = match kind {
                    0 | 1 => ((w & 0x3FFF_FFFF) as u64, 0u8),
                    2 => (((w >> 8) & 0x3F_FFFF) as u64, (w & 0xFF) as u8),
                    3 => {
                        kind = prev_kind;
                        ((w & 0x3FFF_FFFF) as u64, prev_byte)
                    }
                    _ => unreachable!(),
                };
                if blocks == 0 {
                    return Err(NkitError::InvalidGap(
                        "mixed gap sub-record with zero blocks".into(),
                    ));
                }
                let len = (blocks * GAP_BLOCK_SIZE).min(size - acc);
                match kind {
                    0 => pieces.push(GapPiece::Junk { len }),
                    1 => {
                        pieces.push(GapPiece::Verbatim {
                            nkit_off: record_off + consumed,
                            len,
                        });
                        r.seek(SeekFrom::Current(len as i64))?;
                        consumed += len;
                    }
                    2 => {
                        if byte == 0 {
                            pieces.push(GapPiece::Zeros { len });
                        } else {
                            pieces.push(GapPiece::ByteFill { len, byte });
                        }
                    }
                    _ => {
                        return Err(NkitError::InvalidGap(
                            "repeat sub-record without a preceding kind".into(),
                        ));
                    }
                }
                acc += len;
                prev_kind = kind;
                prev_byte = byte;
            }
            pieces
        }
        3 => {
            return Err(NkitError::InvalidGap(
                "junk-file record found where an inter-file gap was expected".into(),
            ));
        }
        _ => unreachable!(),
    };

    Ok(GapRecord {
        consumed,
        out_len: size,
        pieces,
    })
}

/// Parse a junk-file record (type 3) at `record_off`: a file whose
/// content is `leading_nulls` zero bytes followed by junk.
pub(crate) fn parse_junk_file_record<R: Read + Seek>(
    r: &mut R,
    record_off: u64,
) -> NkitResult<JunkFileRecord> {
    r.seek(SeekFrom::Start(record_off))?;
    let hdr = read_u32_be(r)?;
    if hdr & 3 != 3 {
        return Err(NkitError::InvalidGap(format!(
            "expected a junk-file record, found type {}",
            hdr & 3
        )));
    }
    let leading_nulls = (hdr >> 2) as u64;
    let file_len = read_u32_be(r)? as u64;
    if leading_nulls > file_len {
        return Err(NkitError::InvalidGap(
            "junk-file null prefix exceeds the file length".into(),
        ));
    }
    Ok(JunkFileRecord {
        consumed: 8,
        leading_nulls,
        file_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn all_junk_and_scrubbed_records() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(0x1000u32).to_be_bytes());
        let rec = parse_gap_record(&mut Cursor::new(&buf), 0).unwrap();
        assert_eq!(rec.consumed, 4);
        assert_eq!(rec.out_len, 0x1000);
        assert_eq!(rec.pieces, vec![GapPiece::Junk { len: 0x1000 }]);

        let mut buf = Vec::new();
        buf.extend_from_slice(&(0x2000u32 | 1).to_be_bytes());
        let rec = parse_gap_record(&mut Cursor::new(&buf), 0).unwrap();
        assert_eq!(rec.pieces, vec![GapPiece::Zeros { len: 0x2000 }]);
    }

    #[test]
    fn mixed_record_with_all_kinds_and_repeat() {
        let mut buf = Vec::new();
        // size = 0x100 junk + 0x200 verbatim + 0x100 fill(0xAB) + 0x100 fill(0xAB) via repeat
        buf.extend_from_slice(&(0x500u32 | 2).to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes()); // junk, 1 block
        buf.extend_from_slice(&((1u32 << 30) | 2).to_be_bytes()); // nonjunk, 2 blocks
        let verbatim: Vec<u8> = (0..0x200).map(|i| i as u8).collect();
        buf.extend_from_slice(&verbatim);
        buf.extend_from_slice(&((2u32 << 30) | (1 << 8) | 0xAB).to_be_bytes()); // bytefill
        buf.extend_from_slice(&((3u32 << 30) | 1).to_be_bytes()); // repeat
        let rec = parse_gap_record(&mut Cursor::new(&buf), 0).unwrap();
        assert_eq!(rec.out_len, 0x500);
        assert_eq!(rec.consumed, 4 + 4 + 4 + 0x200 + 4 + 4);
        assert_eq!(
            rec.pieces,
            vec![
                GapPiece::Junk { len: 0x100 },
                GapPiece::Verbatim {
                    nkit_off: 12,
                    len: 0x200
                },
                GapPiece::ByteFill {
                    len: 0x100,
                    byte: 0xAB
                },
                GapPiece::ByteFill {
                    len: 0x100,
                    byte: 0xAB
                },
            ]
        );
    }

    #[test]
    fn mixed_record_clips_last_partial_block() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(0x104u32 | 2).to_be_bytes());
        buf.extend_from_slice(&2u32.to_be_bytes()); // junk, 2 blocks = 0x200, clipped to 0x104
        let rec = parse_gap_record(&mut Cursor::new(&buf), 0).unwrap();
        assert_eq!(rec.pieces, vec![GapPiece::Junk { len: 0x104 }]);
    }

    #[test]
    fn size_extension_sentinel() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(0xFFFF_FFFCu32).to_be_bytes());
        buf.extend_from_slice(&(0x10u32).to_be_bytes());
        let rec = parse_gap_record(&mut Cursor::new(&buf), 0).unwrap();
        assert_eq!(rec.out_len, 0xFFFF_FFFC + 0x10);
        assert_eq!(rec.consumed, 8);
    }

    #[test]
    fn junk_file_record_round_trips() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&((0x1Cu32 << 2) | 3).to_be_bytes());
        buf.extend_from_slice(&(0x8000u32).to_be_bytes());
        let rec = parse_junk_file_record(&mut Cursor::new(&buf), 0).unwrap();
        assert_eq!(rec.leading_nulls, 0x1C);
        assert_eq!(rec.file_len, 0x8000);
        assert_eq!(rec.consumed, 8);

        assert_eq!(peek_record_type(&mut Cursor::new(&buf), 0).unwrap(), 3);
        assert!(parse_gap_record(&mut Cursor::new(&buf), 0).is_err());
    }
}
