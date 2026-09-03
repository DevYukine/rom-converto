//! `\0PBP` container header: the eight segment offsets an `EBOOT.PBP`
//! carries, sized against each other and the file end.

use std::io::{Read, Seek, SeekFrom};

use anyhow::{Result, bail};

/// The `\0PBP` container magic.
pub const MAGIC: &[u8; 4] = b"\0PBP";

/// Number of segments a PBP header indexes.
pub const SEGMENT_COUNT: usize = 8;

/// Standard file name of each PBP segment, in header order.
pub const SEGMENT_NAMES: [&str; SEGMENT_COUNT] = [
    "PARAM.SFO",
    "ICON0.PNG",
    "ICON1.PMF",
    "PIC0.PNG",
    "PIC1.PNG",
    "SND0.AT3",
    "DATA.PSP",
    "DATA.PSAR",
];

/// Index of the `PARAM.SFO` segment in [`Pbp::segments`].
pub const PARAM_SFO: usize = 0;
/// Index of the `ICON0.PNG` segment in [`Pbp::segments`].
pub const ICON0_PNG: usize = 1;
/// Index of the `DATA.PSAR` segment in [`Pbp::segments`].
pub const DATA_PSAR: usize = 7;

const HEADER_LEN: usize = 40;

/// A PBP segment's byte range inside the container. A zero `size` means
/// the container does not carry that segment.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub offset: u64,
    pub size: u64,
}

/// Parsed `\0PBP` container header.
#[derive(Debug, Clone)]
pub struct Pbp {
    pub version: u32,
    pub segments: [Segment; SEGMENT_COUNT],
    pub file_size: u64,
}

impl Pbp {
    /// Parses the PBP header from `reader`, sizing each segment from the
    /// next segment's offset and the last from the file end.
    ///
    /// # Errors
    /// Returns an error if the file is shorter than the header, the magic
    /// is absent, the first segment starts inside the header, or the
    /// offsets are not monotonically increasing up to the file end.
    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self> {
        let file_size = reader.seek(SeekFrom::End(0))?;
        if file_size < HEADER_LEN as u64 {
            bail!("not a PBP: {file_size} bytes is shorter than the {HEADER_LEN}-byte header");
        }
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_LEN];
        reader.read_exact(&mut header)?;
        if &header[..4] != MAGIC {
            bail!("not a PBP: missing \\0PBP magic");
        }

        let version = u32::from_le_bytes(header[4..8].try_into().expect("4-byte slice"));
        let mut offsets = [0u64; SEGMENT_COUNT];
        for (i, off) in offsets.iter_mut().enumerate() {
            let base = 8 + i * 4;
            *off =
                u32::from_le_bytes(header[base..base + 4].try_into().expect("4-byte slice")) as u64;
        }
        if offsets[0] < HEADER_LEN as u64 {
            bail!("PBP segment 0 starts at {} inside the header", offsets[0]);
        }

        let mut segments = [Segment { offset: 0, size: 0 }; SEGMENT_COUNT];
        for (i, segment) in segments.iter_mut().enumerate() {
            let end = offsets.get(i + 1).copied().unwrap_or(file_size);
            if end < offsets[i] {
                bail!(
                    "PBP segment {i} spans {} to {end}, which is not monotonic",
                    offsets[i]
                );
            }
            *segment = Segment {
                offset: offsets[i],
                size: end - offsets[i],
            };
        }
        Ok(Self {
            version,
            segments,
            file_size,
        })
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;

    /// Builds a PBP carrying `segments` in header order; an empty slice
    /// marks that segment absent.
    pub fn build_pbp(version: u32, segments: &[&[u8]; SEGMENT_COUNT]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&version.to_le_bytes());
        let mut offset = HEADER_LEN as u32;
        for seg in segments {
            out.extend_from_slice(&offset.to_le_bytes());
            offset += seg.len() as u32;
        }
        for seg in segments {
            out.extend_from_slice(seg);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::test_fixtures::build_pbp;
    use super::*;

    #[test]
    fn sizes_segments_from_the_next_offset_and_the_file_end() {
        let bytes = build_pbp(
            0x10000,
            &[b"sfo", b"icon", &[], &[], b"pic1", &[], b"psp", b"psardata"],
        );
        let pbp = Pbp::read(&mut Cursor::new(&bytes)).expect("parse pbp");

        assert_eq!(pbp.version, 0x10000);
        assert_eq!(pbp.file_size, bytes.len() as u64);
        let sizes: Vec<u64> = pbp.segments.iter().map(|s| s.size).collect();
        assert_eq!(sizes, vec![3, 4, 0, 0, 4, 0, 3, 8]);
        assert_eq!(pbp.segments[PARAM_SFO].offset, HEADER_LEN as u64);
        assert_eq!(pbp.segments[DATA_PSAR].offset, bytes.len() as u64 - 8);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = build_pbp(1, &[b"sfo", &[], &[], &[], &[], &[], &[], &[]]);
        bytes[1] = b'X';
        assert!(Pbp::read(&mut Cursor::new(&bytes)).is_err());
    }

    #[test]
    fn rejects_non_monotonic_offsets() {
        let mut bytes = build_pbp(1, &[b"sfo", b"icon", &[], &[], &[], &[], &[], b"psar"]);
        // Pull ICON0's offset back before PARAM.SFO's.
        bytes[12..16].copy_from_slice(&(HEADER_LEN as u32 - 1).to_le_bytes());
        assert!(Pbp::read(&mut Cursor::new(&bytes)).is_err());
    }

    #[test]
    fn rejects_offsets_past_the_file_end() {
        let mut bytes = build_pbp(1, &[b"sfo", &[], &[], &[], &[], &[], &[], b"psar"]);
        bytes[36..40].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert!(Pbp::read(&mut Cursor::new(&bytes)).is_err());
    }

    #[test]
    fn rejects_a_first_segment_inside_the_header() {
        let mut bytes = build_pbp(1, &[b"sfo", &[], &[], &[], &[], &[], &[], &[]]);
        bytes[8..12].copy_from_slice(&8u32.to_le_bytes());
        assert!(Pbp::read(&mut Cursor::new(&bytes)).is_err());
    }

    #[test]
    fn truncated_input_errors_without_panic() {
        let full = build_pbp(1, &[b"sfo", &[], &[], &[], &[], &[], &[], b"psar"]);
        for len in 0..full.len() {
            let _ = Pbp::read(&mut Cursor::new(&full[..len]));
        }
    }
}
