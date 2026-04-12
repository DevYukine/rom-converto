use binrw::BinResult;
use std::io::{Seek, Write};

pub mod fs;

pub fn align_64(x: u64) -> u64 {
    align(x, 64)
}

pub fn align_64_usize(x: usize) -> usize {
    align_64(x as u64) as usize
}

fn align(x: u64, y: u64) -> u64 {
    let mask: u64 = !(y - 1);
    (x + (y - 1)) & mask
}

pub fn pad_to_align_64(aligned_pos: u64, writer: &mut (impl Write + Seek)) -> BinResult<()> {
    if aligned_pos > writer.stream_position()? {
        // Write padding
        let padding_size = (aligned_pos - writer.stream_position()?) as usize;
        const ZERO_BUF: [u8; 64] = [0u8; 64];
        let mut remaining = padding_size;
        while remaining > 0 {
            let chunk = remaining.min(ZERO_BUF.len());
            writer.write_all(&ZERO_BUF[..chunk])?;
            remaining -= chunk;
        }
    }

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn align_returns_correct_alignment() {
        assert_eq!(align(0, 64), 0);
        assert_eq!(align(1, 64), 64);
        assert_eq!(align(63, 64), 64);
        assert_eq!(align(64, 64), 64);
        assert_eq!(align(65, 64), 128);
        assert_eq!(align(128, 64), 128);
    }

    #[test]
    fn test_cia_alignment() {
        assert_eq!(align_64(0), 0);
        assert_eq!(align_64(1), 64);
        assert_eq!(align_64(63), 64);
        assert_eq!(align_64(64), 64);
        assert_eq!(align_64(65), 128);
        assert_eq!(align_64(128), 128);
    }

    #[test]
    fn pad_to_align_64_writes_correct_padding() {
        use std::io::Cursor;

        let mut buffer = Cursor::new(vec![0u8; 10]);
        let aligned_pos = 16;

        pad_to_align_64(aligned_pos, &mut buffer).unwrap();
        assert_eq!(buffer.get_ref().len(), 16);
        assert_eq!(&buffer.get_ref()[10..], &[0u8; 6]);
    }

    #[test]
    fn pad_to_align_64_does_nothing_if_already_aligned() {
        use std::io::Cursor;

        let mut buffer = Cursor::new(vec![0u8; 16]);
        let aligned_pos = 16;

        pad_to_align_64(aligned_pos, &mut buffer).unwrap();
        assert_eq!(buffer.get_ref().len(), 16);
    }

    /// Proves the identity used by `check_cia_not_encrypted`:
    /// `align_64(a) + align_64(b) + ... = chained_align(a, b, ...)`
    /// (i.e. summing independently-aligned section sizes equals walking the
    /// CIA layout section-by-section). The identity holds because each partial
    /// sum is itself a multiple of 64, so `align_64(P + s) = P + align_64(s)`
    /// when P ≡ 0 (mod 64).
    #[test]
    fn align_64_sum_equals_chained_alignment() {
        let cases: &[&[u64]] = &[
            // Realistic CIA: header, cert chain, ticket, TMD
            &[0x2020, 0x0A00, 0x0304, 0x0B40],
            // From test_simple_cia_file
            &[0x2020, 0x0A00, 0x0350, 0x0B34],
            // Non-aligned cert chain (synthetic edge case)
            &[0x2020, 0x0A01, 0x0304, 0x0B40],
            // Many small unaligned sections
            &[0x21, 0x47, 0x83, 0xC1, 0x0F],
            // All aligned
            &[0x40, 0x80, 0xC0, 0x100],
            // Single section
            &[0x2020],
            // Includes zero-sized sections
            &[0x2020, 0x0, 0x304, 0x0],
        ];

        for case in cases {
            let sum_independent: u64 = case.iter().map(|&s| align_64(s)).sum();
            let mut chained: u64 = 0;
            for &s in *case {
                chained = align_64(chained + s);
            }
            assert_eq!(
                sum_independent, chained,
                "formulas disagree for case {case:?}: sum={sum_independent:#x}, chained={chained:#x}"
            );
        }
    }

    #[test]
    fn pad_to_align_64_handles_large_padding() {
        use std::io::Cursor;

        let mut buffer = Cursor::new(vec![0u8; 5]);
        let aligned_pos = 1024;

        pad_to_align_64(aligned_pos, &mut buffer).unwrap();
        assert_eq!(buffer.get_ref().len(), 1024);
        assert_eq!(&buffer.get_ref()[5..], vec![0u8; 1019].as_slice());
    }
}
