use binrw::BinResult;
use std::io::{Seek, Write};

pub mod fs;

pub fn align_64(x: u64) -> u64 {
    align(x, 64)
}

fn align(x: u64, y: u64) -> u64 {
    let mask: u64 = !(y - 1);
    (x + (y - 1)) & mask
}

pub fn pad_to_align_64(aligned_pos: u64, writer: &mut (impl Write + Seek)) -> BinResult<()> {
    if aligned_pos > writer.stream_position()? {
        // Write padding
        let padding_size = (aligned_pos - writer.stream_position()?) as usize;
        writer.write_all(&vec![0u8; padding_size])?;
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
