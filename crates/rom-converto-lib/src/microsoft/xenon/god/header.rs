//! Build the `LIVE` container header that fronts a GoD install. Every
//! field is big-endian except the part count, which the console reads
//! little-endian.

use sha1::{Digest, Sha1};

use super::layout::GodScan;
use super::parts::PartsSummary;

/// Full byte length of the `LIVE` container header. Distinct from the
/// metadata size value stored at 0x0340 (see [`METADATA_SIZE`]).
pub const HEADER_SIZE: usize = 0xB000;

/// Offset of the header hash, and the range it covers: everything from
/// the content type through the end of the header.
const HEADER_HASH_OFFSET: usize = 0x032C;
const HASHED_START: usize = 0x0344;
const HASHED_LEN: usize = 0xACBC;

/// Metadata size value carried in real GoD containers at 0x0340. Not
/// the container's own byte length (that's [`HEADER_SIZE`]).
const METADATA_SIZE: u32 = 0x0000_AD0E;

/// Games on Demand content type.
const CONTENT_TYPE: u32 = 0x7000;

/// First license entry: unrestricted licensee id, unlocked for anyone.
const LICENSE_OFFSET: usize = 0x022C;

/// SVOD volume descriptor prefix: descriptor length, block cache
/// element count, worker file count, features. Immediately precedes
/// the MHT root hash at 0x037D.
const SVOD_DESCRIPTOR_OFFSET: usize = 0x0379;
const SVOD_DESCRIPTOR_PREFIX: [u8; 4] = [0x24, 0x05, 0x05, 0x11];

/// Bytes reserved for one UTF-16BE, NUL-terminated title string.
const TITLE_FIELD_SIZE: usize = 0x80;

/// The title string is stored twice: once as the display name, once as
/// the description.
const TITLE_OFFSETS: [usize; 2] = [0x0411, 0x1691];

/// Fixed installer blurb shown for a disc-based Games on Demand title.
const INSTALLED_GAME_DESCRIPTION_OFFSET: usize = 0x0D11;
const INSTALLED_GAME_DESCRIPTION: &str =
    "This is an installed game. To play, insert the original game disc.";

/// Encodes `s` as UTF-16BE, NUL-terminated, keeping whole characters
/// only: truncating at `max_units` code units never splits a
/// surrogate pair and leaves a lone half behind.
fn encode_utf16be(s: &str, max_units: usize) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut units = 0usize;
    for ch in s.chars() {
        let mut ch_buf = [0u16; 2];
        let ch_units = ch.encode_utf16(&mut ch_buf);
        if units + ch_units.len() > max_units {
            break;
        }
        units += ch_units.len();
        for unit in ch_units {
            encoded.extend_from_slice(&unit.to_be_bytes());
        }
    }
    encoded.extend_from_slice(&[0, 0]);
    encoded
}

/// Builds the `LIVE` container header that fronts a GoD install.
///
/// Stamps `scan`'s execution id and part layout, and `parts`'s MHT root
/// and total size. `title`, if given, is stored as both the display name
/// and the description. The header hash is computed last, over the
/// hashed region only.
pub fn build_header(scan: &GodScan, parts: &PartsSummary, title: Option<&str>) -> Vec<u8> {
    let mut buf = vec![0u8; HEADER_SIZE];
    buf[0x0000..0x0004].copy_from_slice(b"LIVE");
    buf[LICENSE_OFFSET..LICENSE_OFFSET + 8].fill(0xFF);
    buf[0x0340..0x0344].copy_from_slice(&METADATA_SIZE.to_be_bytes());
    buf[0x0344..0x0348].copy_from_slice(&CONTENT_TYPE.to_be_bytes());
    buf[0x0348..0x034C].copy_from_slice(&2u32.to_be_bytes());
    buf[0x0354..0x0358].copy_from_slice(&scan.execution.media_id.to_be_bytes());
    buf[0x0360..0x0364].copy_from_slice(&scan.execution.title_id.to_be_bytes());
    buf[0x0364] = scan.execution.platform;
    buf[0x0365] = scan.execution.executable_type;
    buf[0x0366] = scan.execution.disc_number;
    buf[0x0367] = scan.execution.disc_count;
    buf[SVOD_DESCRIPTOR_OFFSET..SVOD_DESCRIPTOR_OFFSET + 4]
        .copy_from_slice(&SVOD_DESCRIPTOR_PREFIX);
    buf[0x037D..0x0391].copy_from_slice(&parts.mht_root);
    buf[0x0392..0x0395].copy_from_slice(&scan.block_count.to_be_bytes()[5..8]);
    buf[0x03A0..0x03A4].copy_from_slice(&(scan.part_count as u32).to_le_bytes());
    buf[0x03A4..0x03A8].copy_from_slice(&((parts.total_size / 0x100) as u32).to_be_bytes());
    buf[0x03AC] = 1;
    // Thumbnail size fields stay zero: the reference template's 0x3841 sits over an all-zero payload.

    if let Some(title) = title {
        let encoded = encode_utf16be(title, TITLE_FIELD_SIZE / 2 - 1);
        for offset in TITLE_OFFSETS {
            buf[offset..offset + encoded.len()].copy_from_slice(&encoded);
        }
    }
    let description = encode_utf16be(INSTALLED_GAME_DESCRIPTION, usize::MAX);
    buf[INSTALLED_GAME_DESCRIPTION_OFFSET..INSTALLED_GAME_DESCRIPTION_OFFSET + description.len()]
        .copy_from_slice(&description);

    let digest = Sha1::digest(&buf[HASHED_START..HASHED_START + HASHED_LEN]);
    buf[HEADER_HASH_OFFSET..HEADER_HASH_OFFSET + 20].copy_from_slice(&digest);
    buf
}

#[cfg(test)]
mod tests {
    use super::super::xex::ExecutionId;
    use super::*;

    fn scan() -> GodScan {
        GodScan {
            base: 0x0208_0000,
            data_size: 0x1234_5000,
            block_count: 0x0012_3456,
            part_count: 3,
            title_name: None,
            execution: ExecutionId {
                media_id: 0x1122_3344,
                title_id: 0x4541_08A7,
                platform: 2,
                executable_type: 1,
                disc_number: 1,
                disc_count: 2,
            },
        }
    }

    fn parts() -> PartsSummary {
        PartsSummary {
            mht_root: [0xAB; 20],
            total_size: 0x0002_0000,
        }
    }

    #[test]
    fn every_field_lands_at_its_documented_offset() {
        let scan = scan();
        let parts = parts();
        let buf = build_header(&scan, &parts, None);

        assert_eq!(buf.len(), HEADER_SIZE);
        assert_eq!(&buf[0x0000..0x0004], b"LIVE");
        assert_eq!(&buf[LICENSE_OFFSET..LICENSE_OFFSET + 8], &[0xFFu8; 8]);
        assert_eq!(&buf[0x0340..0x0344], &0x0000_AD0Eu32.to_be_bytes());
        assert_eq!(&buf[0x0344..0x0348], &0x7000u32.to_be_bytes());
        assert_eq!(&buf[0x0348..0x034C], &2u32.to_be_bytes());
        assert_eq!(&buf[0x0354..0x0358], &0x1122_3344u32.to_be_bytes());
        assert_eq!(&buf[0x0360..0x0364], &0x4541_08A7u32.to_be_bytes());
        assert_eq!(&buf[0x0364..0x0368], &[2, 1, 1, 2]);
        assert_eq!(
            &buf[SVOD_DESCRIPTOR_OFFSET..SVOD_DESCRIPTOR_OFFSET + 4],
            &SVOD_DESCRIPTOR_PREFIX
        );
        assert_eq!(&buf[0x037D..0x0391], &parts.mht_root);
        assert_eq!(&buf[0x0392..0x0395], &[0x12, 0x34, 0x56]);
        assert_eq!(&buf[0x0395..0x0397], &[0, 0]);
        assert_eq!(&buf[0x03A0..0x03A4], &3u32.to_le_bytes());
        assert_eq!(&buf[0x03A4..0x03A8], &0x200u32.to_be_bytes());
        assert_eq!(buf[0x03AC], 1);
        assert_eq!([buf[0x035B], buf[0x035F], buf[0x0391]], [0, 0, 0]);
        // Thumbnail sizes and payloads stay empty.
        assert_eq!(&buf[0x1712..0x171A], &[0u8; 8]);
        assert!(buf[0x171A..0x971A].iter().all(|&b| b == 0));
    }

    #[test]
    fn the_installed_game_description_round_trips_as_utf16be() {
        let buf = build_header(&scan(), &parts(), None);
        let expected = encode_utf16be(INSTALLED_GAME_DESCRIPTION, usize::MAX);
        assert_eq!(
            &buf[INSTALLED_GAME_DESCRIPTION_OFFSET
                ..INSTALLED_GAME_DESCRIPTION_OFFSET + expected.len()],
            expected.as_slice()
        );
        assert_eq!(&expected[expected.len() - 2..], &[0, 0]);
    }

    #[test]
    fn the_title_is_stored_twice_as_nul_terminated_utf16be() {
        let buf = build_header(&scan(), &parts(), Some("Hi"));
        for offset in TITLE_OFFSETS {
            assert_eq!(&buf[offset..offset + 6], &[0, b'H', 0, b'i', 0, 0]);
        }
    }

    #[test]
    fn an_overlong_title_stays_inside_its_field() {
        let buf = build_header(&scan(), &parts(), Some(&"x".repeat(200)));
        for offset in TITLE_OFFSETS {
            assert_eq!(
                &buf[offset + TITLE_FIELD_SIZE - 2..offset + TITLE_FIELD_SIZE],
                &[0, 0]
            );
        }
    }

    #[test]
    fn a_title_ending_in_a_supplementary_plane_char_keeps_no_lone_surrogate() {
        // 62 'x' units plus a 2-unit surrogate pair would total 64,
        // one past the 63-unit cap; the whole character must be
        // dropped rather than split.
        let title = format!("{}{}", "x".repeat(62), '\u{1F600}');
        let buf = build_header(&scan(), &parts(), Some(&title));
        let mut expected_units = Vec::new();
        for _ in 0..62 {
            expected_units.extend_from_slice(&[0, b'x']);
        }
        for offset in TITLE_OFFSETS {
            assert_eq!(&buf[offset..offset + 124], expected_units.as_slice());
            assert_eq!(&buf[offset + 124..offset + 128], &[0, 0, 0, 0]);
        }
    }

    #[test]
    fn the_header_hash_covers_everything_after_it() {
        let buf = build_header(&scan(), &parts(), Some("Hi"));
        let expected = Sha1::digest(&buf[HASHED_START..HASHED_START + HASHED_LEN]);
        assert_eq!(
            &buf[HEADER_HASH_OFFSET..HEADER_HASH_OFFSET + 20],
            expected.as_slice()
        );
        assert_eq!(HASHED_START + HASHED_LEN, HEADER_SIZE);
    }
}
