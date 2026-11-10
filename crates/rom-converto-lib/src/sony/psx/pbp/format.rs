//! The fixed structures of a PS1 EBOOT's `DATA.PSAR`: PSISOIMG and
//! PSTITLEIMG headers, the table of contents, the block index, STARTDAT.

use super::disc::{DiscId, DiscImage};
use super::error::{PsxPbpError, PsxPbpResult};
use crate::disc::cd::{FRAMES_PER_SECOND, SECONDS_PER_MINUTE};
use crate::disc::cue::models::Msf;
use crate::util::template::truncate_bytes;

/// Bytes of disc image per block: 16 raw sectors.
pub(crate) const BLOCK_SIZE: usize = 0x9300;
/// Blocks the index between PSISOIMG+0x4000 and +0x100000 holds.
pub(crate) const MAX_BLOCKS: u64 = 32_256;
/// Where a disc's blocks start inside its PSISOIMG.
pub(crate) const DISC_DATA_START: u64 = 0x10_0000;
/// Each PSISOIMG of a multi-disc file starts on this boundary.
pub(crate) const DISC_ALIGN: u64 = 0x8000;
/// `DATA.PSAR` starts on this boundary inside the PBP.
pub(crate) const PSAR_ALIGN: u64 = 0x1_0000;
/// Discs one PSTITLEIMG holds.
pub(crate) const MAX_DISCS: usize = 5;
/// Bytes of the multi-disc table the emulator reads at PSTITLEIMG+0x28C.
pub(crate) const MULTIDISC_TABLE_LEN: usize = 64;
/// Bytes of the PSTITLEIMG header; disc 1's PSISOIMG follows it.
pub(crate) const PSTITLEIMG_LEN: u64 = DISC_ALIGN;

const PSISOIMG_MAGIC: &[u8; 12] = b"PSISOIMG0000";
const PSTITLEIMG_MAGIC: &[u8; 16] = b"PSTITLEIMG000000";
const STARTDAT_MAGIC: &[u8; 8] = b"STARTDAT";
const STARTDAT_HEADER_LEN: usize = 0x50;
const STARTDAT_ALIGN: u64 = 16;

const ISO_END_OF_DATA: usize = 0x00C;
const ISO_DISC_ID: usize = 0x400;
const ISO_TOC: usize = 0x800;
const ISO_TOC_LEN: usize = 0x3FC;
const ISO_DATA_START: usize = 0xBFC;
const ISO_AFTER_STARTDAT: usize = 0x1220;
const ISO_CONSTANT_07FF: usize = 0x1228;
const ISO_TITLE: usize = 0x122C;
const ISO_INDEX: usize = 0x4000;
const INDEX_ENTRY_LEN: usize = 32;

const TITLE_STARTDAT: usize = 0x010;
const TITLE_DISC_OFFSETS: usize = 0x200;
const TITLE_DISC_ID: usize = 0x264;
const TITLE_AFTER_STARTDAT: usize = 0x284;
const TITLE_MULTIDISC_TABLE: usize = 0x28C;
const TITLE_TITLE: usize = 0x30C;

const DISC_ID_SLOT: usize = 0x20;
const TITLE_SLOT: usize = 0x80;

const TOC_ENTRY_LEN: usize = 10;
const TOC_ADR_POSITION: u8 = 1;
const TOC_CONTROL_DATA: u8 = 0x4;
const TOC_CONTROL_AUDIO: u8 = 0x0;
const TOC_FIRST_TRACK: u8 = 0xA0;
const TOC_LAST_TRACK: u8 = 0xA1;
const TOC_LEAD_OUT: u8 = 0xA2;
const TOC_DISC_TYPE_XA: u8 = 0x20;
/// Frames before LBA 0. Track starts and the lead-out are absolute MSF
/// positions, so both carry it: the lead-out is the sector count plus this,
/// as ECMA-130 places it. The PSP's PS1 emulator stops reading at the
/// lead-out, so one placed earlier cuts the end off a disc's last audio
/// track.
const LEAD_IN_FRAMES: u32 = 150;
/// The last position a TOC's BCD minutes:seconds:frames can name, 99:59:74.
const LAST_TOC_FRAME: u32 = (99 * SECONDS_PER_MINUTE + 59) * FRAMES_PER_SECOND + 74;
/// Sectors a disc may hold so its lead-out still falls on [`LAST_TOC_FRAME`].
pub(crate) const MAX_SECTORS: u64 = (LAST_TOC_FRAME - LEAD_IN_FRAMES) as u64;

/// One block of the PSISOIMG index: where the stored block starts after
/// PSISOIMG+0x100000, and its stored size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub offset: u32,
    pub size: u16,
}

/// Blocks a disc image of `image_size` bytes is cut into.
pub(crate) fn block_count(image_size: u64) -> u64 {
    image_size.div_ceil(BLOCK_SIZE as u64)
}

/// End of a disc's stored data relative to its PSISOIMG: where the
/// STARTDAT of a one-disc file starts.
pub(crate) fn data_end(stored_len: u64) -> u64 {
    (DISC_DATA_START + stored_len).next_multiple_of(STARTDAT_ALIGN)
}

/// Bytes of STARTDAT with a `png_len`-byte image.
pub(crate) fn startdat_len(png_len: usize) -> u64 {
    (STARTDAT_HEADER_LEN + png_len) as u64
}

/// The 1 MiB PSISOIMG header of `disc`: identity, table of contents and
/// the block index for its `entries`.
pub(crate) fn psisoimg_header(
    disc: &DiscImage,
    id: &DiscId,
    title: &str,
    entries: &[IndexEntry],
    stored_len: u64,
    startdat_len: u64,
) -> PsxPbpResult<Vec<u8>> {
    let mut header = vec![0u8; DISC_DATA_START as usize];
    let end = data_end(stored_len);
    header[..PSISOIMG_MAGIC.len()].copy_from_slice(PSISOIMG_MAGIC);
    put_u32(&mut header, ISO_END_OF_DATA, end)?;
    put_text(&mut header, ISO_DISC_ID, DISC_ID_SLOT, &id.image());
    let toc = table_of_contents(disc)?;
    header[ISO_TOC..ISO_TOC + toc.len()].copy_from_slice(&toc);
    put_u32(&mut header, ISO_DATA_START, DISC_DATA_START)?;
    put_u32(&mut header, ISO_AFTER_STARTDAT, end + startdat_len)?;
    header[ISO_CONSTANT_07FF..ISO_CONSTANT_07FF + 2].copy_from_slice(&0x07FFu16.to_le_bytes());
    put_text(&mut header, ISO_TITLE, TITLE_SLOT, title);
    for (i, entry) in entries.iter().enumerate() {
        // Marker (+6), hash (+8) and padding (+24) stay zero: a marker of
        // 1 makes raw CD audio buzz, and the hash is never checked.
        let at = ISO_INDEX + i * INDEX_ENTRY_LEN;
        header[at..at + 4].copy_from_slice(&entry.offset.to_le_bytes());
        header[at + 4..at + 6].copy_from_slice(&entry.size.to_le_bytes());
    }
    Ok(header)
}

/// The PSTITLEIMG header for discs at `disc_offsets` (relative to
/// `DATA.PSAR`), the last of whose stored data is `last_stored_len` bytes.
pub(crate) fn pstitleimg_header(
    first_id: &DiscId,
    title: &str,
    disc_offsets: &[u64],
    last_stored_len: u64,
    startdat_len: u64,
    table: &[u8; MULTIDISC_TABLE_LEN],
) -> PsxPbpResult<Vec<u8>> {
    let mut header = vec![0u8; PSTITLEIMG_LEN as usize];
    let startdat = disc_offsets.last().copied().unwrap_or_default() + data_end(last_stored_len);
    header[..PSTITLEIMG_MAGIC.len()].copy_from_slice(PSTITLEIMG_MAGIC);
    put_u32(&mut header, TITLE_STARTDAT, startdat)?;
    for (i, offset) in disc_offsets.iter().enumerate() {
        put_u32(&mut header, TITLE_DISC_OFFSETS + i * 4, *offset)?;
    }
    put_text(&mut header, TITLE_DISC_ID, DISC_ID_SLOT, &first_id.image());
    put_u32(&mut header, TITLE_AFTER_STARTDAT, startdat + startdat_len)?;
    // The table's slot is 0x80 bytes; only the first 64 are ever read.
    header[TITLE_MULTIDISC_TABLE..TITLE_MULTIDISC_TABLE + MULTIDISC_TABLE_LEN]
        .copy_from_slice(table);
    put_text(&mut header, TITLE_TITLE, TITLE_SLOT, title);
    Ok(header)
}

/// The STARTDAT header that precedes a `png_len`-byte boot image.
pub(crate) fn startdat_header(png_len: usize) -> [u8; STARTDAT_HEADER_LEN] {
    let mut header = [0u8; STARTDAT_HEADER_LEN];
    header[..8].copy_from_slice(STARTDAT_MAGIC);
    header[0x08..0x0C].copy_from_slice(&1u32.to_le_bytes());
    header[0x0C..0x10].copy_from_slice(&1u32.to_le_bytes());
    header[0x10..0x14].copy_from_slice(&(STARTDAT_HEADER_LEN as u32).to_le_bytes());
    header[0x14..0x18].copy_from_slice(&(png_len as u32).to_le_bytes());
    header
}

/// CD lead-in entries: first track, last track, lead-out, then one per
/// track at its `INDEX 01`.
fn table_of_contents(disc: &DiscImage) -> PsxPbpResult<Vec<u8>> {
    let control = |audio: bool| {
        let control = if audio {
            TOC_CONTROL_AUDIO
        } else {
            TOC_CONTROL_DATA
        };
        (control << 4) | TOC_ADR_POSITION
    };
    let first = disc.tracks[0];
    let last = disc.tracks[disc.tracks.len() - 1];
    let entry = |control: u8, point: u8, p: [u8; 3]| -> [u8; TOC_ENTRY_LEN] {
        [control, 0, point, 0, 0, 0, 0, p[0], p[1], p[2]]
    };
    let sectors = u32::try_from(disc.sectors()).map_err(|_| PsxPbpError::DiscTooLong {
        path: disc.source.clone(),
        sectors: disc.sectors(),
        max: MAX_SECTORS,
    })?;
    let mut toc = Vec::with_capacity(ISO_TOC_LEN);
    toc.extend_from_slice(&entry(
        control(first.audio),
        TOC_FIRST_TRACK,
        [bcd(first.number.into()), TOC_DISC_TYPE_XA, 0],
    ));
    toc.extend_from_slice(&entry(
        control(last.audio),
        TOC_LAST_TRACK,
        [bcd(last.number.into()), 0, 0],
    ));
    toc.extend_from_slice(&entry(
        control(last.audio),
        TOC_LEAD_OUT,
        msf_bcd(sectors + LEAD_IN_FRAMES),
    ));
    for track in &disc.tracks {
        toc.extend_from_slice(&entry(
            control(track.audio),
            bcd(track.number.into()),
            msf_bcd(track.start + LEAD_IN_FRAMES),
        ));
    }
    Ok(toc)
}

fn bcd(value: u32) -> u8 {
    (((value / 10 % 10) << 4) | (value % 10)) as u8
}

fn msf_bcd(frames: u32) -> [u8; 3] {
    let msf = Msf::from_lba(frames);
    [
        bcd(msf.minutes.into()),
        bcd(msf.seconds.into()),
        bcd(msf.frames.into()),
    ]
}

/// Writes a header offset, refusing one past what 32 bits address.
fn put_u32(buf: &mut [u8], at: usize, value: u64) -> PsxPbpResult<()> {
    let word = u32::try_from(value).map_err(|_| PsxPbpError::HeaderFieldOverflow {
        field: "PSAR header offset",
        value,
    })?;
    buf[at..at + 4].copy_from_slice(&word.to_le_bytes());
    Ok(())
}

/// Writes `text` into a `slot`-byte field, cut at a character boundary so
/// the field keeps its NUL terminator.
fn put_text(buf: &mut [u8], at: usize, slot: usize, text: &str) {
    let text = truncate_bytes(text, slot - 1);
    buf[at..at + text.len()].copy_from_slice(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::super::disc::test_fixtures::{ps1_data_track, write_disc};
    use super::*;

    fn disc(sectors: u32) -> (tempfile::TempDir, DiscImage) {
        let dir = tempfile::tempdir().unwrap();
        let cue = write_disc(dir.path(), "game", &ps1_data_track("SLUS_999.99", sectors));
        let disc = DiscImage::open(&cue, &cue).unwrap();
        (dir, disc)
    }

    /// The one-data-track example in the format reference: 1,054 sectors,
    /// so the lead-out is at 1,204 frames, 00:16:04.
    #[test]
    fn toc_of_one_data_track() {
        let (_dir, disc) = disc(1054);
        let toc = table_of_contents(&disc).unwrap();
        assert_eq!(
            toc,
            [
                [0x41, 0, 0xA0, 0, 0, 0, 0, 0x01, 0x20, 0x00],
                [0x41, 0, 0xA1, 0, 0, 0, 0, 0x01, 0x00, 0x00],
                [0x41, 0, 0xA2, 0, 0, 0, 0, 0x00, 0x16, 0x04],
                [0x41, 0, 0x01, 0, 0, 0, 0, 0x00, 0x02, 0x00],
            ]
            .concat()
        );
    }

    /// A data track and an audio track in separate files, the audio one
    /// with a stored two-second INDEX 00: the audio control nibble, the last
    /// track and the lead-out all follow the audio track.
    #[test]
    fn toc_of_a_data_and_an_audio_track() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t1.bin"), vec![0u8; 2352 * 100]).unwrap();
        std::fs::write(dir.path().join("t2.bin"), vec![0u8; 2352 * 200]).unwrap();
        let cue = dir.path().join("game.cue");
        std::fs::write(
            &cue,
            "FILE \"t1.bin\" BINARY\n TRACK 01 MODE2/2352\n  INDEX 01 00:00:00\n\
             FILE \"t2.bin\" BINARY\n TRACK 02 AUDIO\n  INDEX 00 00:00:00\n  INDEX 01 00:02:00\n",
        )
        .unwrap();
        let disc = DiscImage::open(&cue, &cue).unwrap();
        assert_eq!(
            table_of_contents(&disc).unwrap(),
            [
                [0x41, 0, 0xA0, 0, 0, 0, 0, 0x01, 0x20, 0x00],
                // Last track 2, audio.
                [0x01, 0, 0xA1, 0, 0, 0, 0, 0x02, 0x00, 0x00],
                // 300 sectors + 150 = 450 frames, 00:06:00.
                [0x01, 0, 0xA2, 0, 0, 0, 0, 0x00, 0x06, 0x00],
                [0x41, 0, 0x01, 0, 0, 0, 0, 0x00, 0x02, 0x00],
                // INDEX 01 at 100 + 150 sectors, + 150 = 400 frames, 00:05:25.
                [0x01, 0, 0x02, 0, 0, 0, 0, 0x00, 0x05, 0x25],
            ]
            .concat()
        );
    }

    #[test]
    fn the_longest_disc_ends_its_lead_out_on_99_59_74() {
        assert_eq!(MAX_SECTORS, 449_849);
        let lead_out = u32::try_from(MAX_SECTORS).unwrap() + LEAD_IN_FRAMES;
        assert_eq!(msf_bcd(lead_out), [0x99, 0x59, 0x74]);
    }

    #[test]
    fn psisoimg_header_places_every_field() {
        let (_dir, disc) = disc(1054);
        let id = DiscId::parse("SLUS-99999").unwrap();
        let entries: Vec<IndexEntry> = (0..66)
            .map(|k| IndexEntry {
                offset: k * 0x9305,
                size: 0x9305,
            })
            .collect();
        let stored = 66 * 0x9305;
        let header =
            psisoimg_header(&disc, &id, "Test", &entries, stored, startdat_len(486)).unwrap();

        assert_eq!(&header[..12], b"PSISOIMG0000");
        assert_eq!(
            u32::from_le_bytes(header[0xC..0x10].try_into().unwrap()),
            0x35E750
        );
        assert_eq!(&header[0x400..0x40C], b"_SLUS_99999\0");
        assert_eq!(&header[0xBFC..0xC00], &0x0010_0000u32.to_le_bytes());
        assert_eq!(
            u32::from_le_bytes(header[0x1220..0x1224].try_into().unwrap()),
            0x35E750 + 0x50 + 486
        );
        assert_eq!(&header[0x1228..0x122A], &[0xFF, 0x07]);
        assert_eq!(&header[0x122C..0x1231], b"Test\0");
        let last = 0x4000 + 65 * 32;
        // Entry 65: offset 65 * 0x9305 = 0x255445, size 0x9305, marker 0.
        assert_eq!(
            &header[last..last + 8],
            &[0x45, 0x54, 0x25, 0x00, 0x05, 0x93, 0, 0]
        );
        assert!(header[last + 32..].iter().all(|&b| b == 0));
    }

    #[test]
    fn pstitleimg_header_points_at_discs_and_the_one_startdat() {
        let id = DiscId::parse("SLPS-99998").unwrap();
        let table = [0xA5u8; MULTIDISC_TABLE_LEN];
        let header =
            pstitleimg_header(&id, "Two", &[0x8000, 0x30_0000], 0x1000, 0x60, &table).unwrap();
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let startdat = 0x30_0000 + data_end(0x1000) as u32;
        assert_eq!(&header[..16], b"PSTITLEIMG000000");
        assert_eq!(word(0x10), startdat);
        assert_eq!(
            (word(0x200), word(0x204), word(0x208)),
            (0x8000, 0x30_0000, 0)
        );
        assert_eq!(&header[0x264..0x270], b"_SLPS_99998\0");
        assert_eq!(word(0x284), startdat + 0x60);
        assert_eq!(&header[0x28C..0x2CC], &table);
        assert!(header[0x2CC..0x30C].iter().all(|&b| b == 0));
        assert_eq!(&header[0x30C..0x310], b"Two\0");
    }

    #[test]
    fn titles_are_cut_at_a_character_boundary() {
        let mut slot = vec![0xFFu8; 8];
        put_text(&mut slot, 0, 4, "ab\u{e9}");
        assert_eq!(&slot[..4], b"ab\xFF\xFF", "the 2-byte \u{e9} does not fit 3 bytes");
        let mut slot = vec![0xFFu8; 8];
        put_text(&mut slot, 0, 4, "abcdef");
        assert_eq!(&slot[..4], b"abc\xFF");
    }

    #[test]
    fn startdat_header_names_the_image_size() {
        let header = startdat_header(486);
        assert_eq!(&header[..8], b"STARTDAT");
        assert_eq!(&header[0x10..0x18], &[0x50, 0, 0, 0, 0xE6, 0x01, 0, 0]);
        assert!(header[0x18..].iter().all(|&b| b == 0));
    }
}
