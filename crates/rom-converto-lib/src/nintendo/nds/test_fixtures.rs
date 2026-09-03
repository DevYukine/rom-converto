//! Synthetic Nintendo DS images for the secure-area crypto tests. The
//! secure-area filler is deterministic pseudo-random so a round trip has to
//! restore real payload bytes rather than a run of zeroes.

#![cfg(test)]

use crate::nintendo::nds::info::{NdsSecureAreaState, crc16};
use crate::nintendo::nds::{DECRYPTED_MARKER, HEADER_SIZE, SECURE_AREA_END, SECURE_AREA_OFFSET};

/// Game code baked into [`synth_nds`].
pub const SYNTH_IDCODE: [u8; 4] = *b"ARCE";

/// Game title baked into [`make_nds_info_fixture`], both the header field
/// and the banner's English title.
pub const SYNTH_TITLE: &str = "Test Game";

/// Banner block offset baked into [`make_nds_info_fixture`].
const SYNTH_BANNER_OFFSET: u32 = 0x10000;

/// Total size of the images [`make_nds_info_fixture`] and
/// [`make_nds_info_fixture_no_banner`] build.
const SYNTH_FILE_SIZE: usize = 0x20000;

/// Builds a synthetic `.nds` image for the `info` tests: a header with a
/// valid CRC16, an ARM9 secure-area window matching `secure_state`, and a
/// one-language-titled banner with a 32x32 icon.
pub fn make_nds_info_fixture(secure_state: NdsSecureAreaState) -> Vec<u8> {
    let header = build_info_header(secure_state, SYNTH_BANNER_OFFSET);
    let mut rom = vec![0u8; SYNTH_FILE_SIZE];
    rom[..HEADER_SIZE].copy_from_slice(&header);

    if secure_state != NdsSecureAreaState::NotPresent {
        let marker: [u8; 8] = if secure_state == NdsSecureAreaState::Decrypted {
            *DECRYPTED_MARKER
        } else {
            [0xAA; 8]
        };
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + 8].copy_from_slice(&marker);
    }

    let banner = build_info_banner(1, SYNTH_TITLE);
    let banner_off = SYNTH_BANNER_OFFSET as usize;
    rom[banner_off..banner_off + banner.len()].copy_from_slice(&banner);

    rom
}

/// Builds a synthetic `.nds` image with no banner (`0x068` left as `0`),
/// for the "no banner" `info` test.
pub fn make_nds_info_fixture_no_banner() -> Vec<u8> {
    let header = build_info_header(NdsSecureAreaState::Decrypted, 0);
    let mut rom = vec![0u8; SYNTH_FILE_SIZE];
    rom[..HEADER_SIZE].copy_from_slice(&header);
    rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + 8].copy_from_slice(DECRYPTED_MARKER);
    rom
}

fn build_info_header(secure_state: NdsSecureAreaState, banner_offset: u32) -> [u8; HEADER_SIZE] {
    let mut h = [0u8; HEADER_SIZE];
    write_ascii(&mut h[0x000..0x00C], SYNTH_TITLE);
    write_ascii(&mut h[0x00C..0x010], "ARCE");
    write_ascii(&mut h[0x010..0x012], "01");
    h[0x012] = 0x00; // unit code: NDS
    h[0x014] = 7; // device capacity
    h[0x01D] = 0x00; // region
    h[0x01E] = 0x00; // rom version

    let arm9_rom_offset: u32 = match secure_state {
        NdsSecureAreaState::NotPresent => SECURE_AREA_END as u32,
        _ => SECURE_AREA_OFFSET as u32,
    };
    h[0x020..0x024].copy_from_slice(&arm9_rom_offset.to_le_bytes());
    h[0x024..0x028].copy_from_slice(&0x0200_4000u32.to_le_bytes());
    h[0x028..0x02C].copy_from_slice(&0x0200_4000u32.to_le_bytes());
    h[0x02C..0x030].copy_from_slice(&0x0003_0000u32.to_le_bytes());

    h[0x030..0x034].copy_from_slice(&0x0000_8000u32.to_le_bytes());
    h[0x034..0x038].copy_from_slice(&0x0238_0000u32.to_le_bytes());
    h[0x038..0x03C].copy_from_slice(&0x0238_0000u32.to_le_bytes());
    h[0x03C..0x040].copy_from_slice(&0x0001_0000u32.to_le_bytes());

    h[0x068..0x06C].copy_from_slice(&banner_offset.to_le_bytes());
    h[0x080..0x084].copy_from_slice(&(SYNTH_FILE_SIZE as u32).to_le_bytes());

    let crc = crc16(&h[..0x15E]);
    h[0x15E..0x160].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Builds a `version`-language banner block whose English title is
/// `english_title` and whose icon is a solid, non-transparent color.
fn build_info_banner(version: u16, english_title: &str) -> Vec<u8> {
    let mut lang_count = 6;
    if version >= 2 {
        lang_count += 1;
    }
    if version >= 3 {
        lang_count += 1;
    }

    let mut buf = vec![0u8; 0x240 + 0x100 * lang_count];
    buf[0x00..0x02].copy_from_slice(&version.to_le_bytes());

    // Icon: every texel is palette index 1, a solid opaque color.
    buf[0x020..0x220].fill(0x11);
    buf[0x222..0x224].copy_from_slice(&0x7FFFu16.to_le_bytes()); // palette[1]: white

    // English is language index 1 (Japanese, English, French, ...).
    let english_off = 0x240 + 0x100;
    write_utf16(&mut buf[english_off..english_off + 0x100], english_title);

    let crc = crc16(&buf[0x020..0x840]);
    buf[0x02..0x04].copy_from_slice(&crc.to_le_bytes());
    buf
}

fn write_ascii(dst: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    dst[..bytes.len()].copy_from_slice(bytes);
}

fn write_utf16(dst: &mut [u8], s: &str) {
    for (i, unit) in s.encode_utf16().enumerate() {
        dst[i * 2..i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// Builds a 0x10000-byte decrypted NDS image with `idcode` at `0x0C` and
/// `arm9_rom_offset` at `0x20`.
pub fn synth_nds(idcode: [u8; 4], arm9_rom_offset: u32) -> Vec<u8> {
    let mut rom = vec![0u8; 0x10000];
    rom[0x0C..0x10].copy_from_slice(&idcode);
    rom[0x20..0x24].copy_from_slice(&arm9_rom_offset.to_le_bytes());
    rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + 8].copy_from_slice(DECRYPTED_MARKER);
    for (i, byte) in rom[SECURE_AREA_OFFSET + 8..SECURE_AREA_END]
        .iter_mut()
        .enumerate()
    {
        *byte = ((i as u32).wrapping_mul(0x9E37_79B1).rotate_left(11) >> 16) as u8;
    }
    rom
}
