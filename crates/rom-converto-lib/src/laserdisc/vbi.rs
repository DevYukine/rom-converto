//! Port of MAME's `vbiparse`: white flag and Philips code recovery from
//! laserdisc VBI lines.
//!
//! Input rows are the 16-bit YUY16 words of one field; a `source_shift` of 8
//! selects the luma byte. [`vbi_parse_all`] reads the white flag off row 11
//! and the Manchester-coded 24-bit words off rows 16, 17, and 18, which
//! [`vbi_metadata_pack`] then writes into the 16-byte record the CHD `AVLD`
//! metadata blob carries per field.

/// Bytes per packed VBI record in the `AVLD` blob.
pub const VBI_PACKED_BYTES: usize = 16;

/// Bits in a Philips code word.
pub const VBI_CODE_BITS: usize = 24;

use serde::{Deserialize, Serialize};

const MAX_SOURCE_WIDTH: usize = 1024;
const MAX_CLOCK_DIFF: i32 = 3;

pub const VBI_CODE_LEADIN: u32 = 0x88ffff;
pub const VBI_CODE_LEADOUT: u32 = 0x80eeee;
pub const VBI_CODE_STOP: u32 = 0x82cfff;
pub const VBI_CODE_CLV: u32 = 0x87ffff;
pub const VBI_MASK_CAV_PICTURE: u32 = 0xf00000;
pub const VBI_CODE_CAV_PICTURE: u32 = 0xf00000;
pub const VBI_MASK_CHAPTER: u32 = 0xf00fff;
pub const VBI_CODE_CHAPTER: u32 = 0x800ddd;
pub const VBI_MASK_CLV_TIME: u32 = 0xf0ff00;
pub const VBI_CODE_CLV_TIME: u32 = 0xf0dd00;
pub const VBI_MASK_STATUS_CX_ON: u32 = 0xfff000;
pub const VBI_CODE_STATUS_CX_ON: u32 = 0x8dc000;
pub const VBI_MASK_STATUS_CX_OFF: u32 = 0xfff000;
pub const VBI_CODE_STATUS_CX_OFF: u32 = 0x8bc000;
pub const VBI_MASK_USER: u32 = 0xf0f000;
pub const VBI_CODE_USER: u32 = 0x80d000;
pub const VBI_MASK_CLV_PICTURE: u32 = 0xf0f000;
pub const VBI_CODE_CLV_PICTURE: u32 = 0x80e000;

/// The decoded VBI content of one field.
///
/// A line that fails to decode stays zero, which is also how MAME marks
/// "no code present".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VbiMetadata {
    pub white: bool,
    pub line16: u32,
    pub line17: u32,
    pub line18: u32,
    /// Most plausible value out of lines 17 and 18.
    pub line1718: u32,
}

/// Parses the white flag and lines 16, 17, and 18 of one field.
///
/// `source` holds the field's YUY16 words with a stride of
/// `source_row_pixels`; rows that fall outside `source` are skipped and leave
/// their field zero.
pub fn vbi_parse_all(
    source: &[u16],
    source_row_pixels: usize,
    source_width: usize,
    source_shift: u32,
) -> VbiMetadata {
    let mut vbi = VbiMetadata::default();
    let row = |n: usize| -> Option<&[u16]> {
        let start = n.checked_mul(source_row_pixels)?;
        source.get(start..start.checked_add(source_width)?)
    };

    let mut bits = [[0u32; VBI_CODE_BITS]; 2];

    if let Some(line) = row(11) {
        vbi.white = vbi_parse_white_flag(line, source_width, source_shift);
    }
    if let Some(line) = row(16)
        && decode_line(line, source_width, source_shift, &mut bits[0])
    {
        vbi.line16 = pack_bits(&bits[0]);
    }
    if let Some(line) = row(17)
        && decode_line(line, source_width, source_shift, &mut bits[0])
    {
        vbi.line17 = pack_bits(&bits[0]);
    }
    if let Some(line) = row(18)
        && decode_line(line, source_width, source_shift, &mut bits[1])
    {
        vbi.line18 = pack_bits(&bits[1]);
    }

    // Pick the best out of lines 17/18; bits[0] now holds line 17.
    if vbi.line17 == 0 {
        vbi.line1718 = vbi.line18;
    } else if vbi.line18 == 0 || vbi.line17 == vbi.line18 {
        vbi.line1718 = vbi.line17;
    } else {
        // If both are picture numbers and one is not valid BCD, pick the other.
        if vbi.line17 & VBI_MASK_CAV_PICTURE == VBI_CODE_CAV_PICTURE
            && vbi.line18 & VBI_MASK_CAV_PICTURE == VBI_CODE_CAV_PICTURE
        {
            if !is_bcd(vbi.line17) {
                vbi.line1718 = vbi.line18;
            } else if !is_bcd(vbi.line18) {
                vbi.line1718 = vbi.line17;
            }
        }
        // Still nothing: take each bit from whichever line was more confident.
        if vbi.line1718 == 0 {
            for (from17, from18) in bits[0].iter().zip(bits[1].iter()) {
                let bit = if from17 > from18 { from17 } else { from18 };
                vbi.line1718 = (vbi.line1718 << 1) | (bit & 1);
            }
        }
    }
    vbi
}

fn decode_line(
    line: &[u16],
    source_width: usize,
    source_shift: u32,
    bits: &mut [u32; VBI_CODE_BITS],
) -> bool {
    vbi_parse_manchester_code(line, source_width, source_shift, VBI_CODE_BITS, bits)
        == VBI_CODE_BITS
}

fn pack_bits(bits: &[u32; VBI_CODE_BITS]) -> u32 {
    bits.iter().fold(0u32, |acc, bit| (acc << 1) | (bit & 1))
}

/// Returns whether the low four nibbles of a CAV picture code are valid BCD.
fn is_bcd(code: u32) -> bool {
    code & 0xf000 <= 0x9000 && code & 0xf00 <= 0x900 && code & 0xf0 <= 0x90 && code & 0xf <= 0x9
}

/// Recovers `expected_bits` Manchester-coded bits from one VBI line.
///
/// Each entry of `result` holds the bit value in bit 0 and the decode
/// confidence in bits 1 and up. Returns `expected_bits` on success and 0 on
/// any failure (line too wide, no dynamic range, no usable clock, a bit cell
/// without a transition).
pub fn vbi_parse_manchester_code(
    source: &[u16],
    source_width: usize,
    source_shift: u32,
    expected_bits: usize,
    result: &mut [u32],
) -> usize {
    if !(2..=MAX_SOURCE_WIDTH).contains(&source_width) || source.len() < source_width {
        return 0;
    }
    if expected_bits == 0 || result.len() < expected_bits {
        return 0;
    }

    // MAME reads past the ends of its fixed 1024-wide buffers; clamp instead.
    let luma = |i: i64| -> u8 {
        let idx = i.clamp(0, source_width as i64 - 1) as usize;
        (source[idx] >> source_shift) as u8
    };

    let mut min = 0xffu8;
    let mut max = 0x00u8;
    for x in 0..source_width {
        let raw = luma(x as i64);
        min = min.min(raw);
        max = max.max(raw);
    }
    if max < 0x80 || min > 0x80 {
        return 0;
    }

    // Midpoint, then thresholds halfway out to each extreme.
    let mid = ((u16::from(min) + u16::from(max)) / 2) as u8;
    let min = mid - (mid - min) / 2;
    let max = mid + (max - mid) / 2;

    let mut srcabs = [0u8; MAX_SOURCE_WIDTH];
    // Seeded from the unshifted word, as MAME does.
    let mut level = u8::from(source[0] > u16::from(mid));
    for (x, slot) in srcabs.iter_mut().take(source_width).enumerate() {
        let raw = luma(x as i64);
        if raw >= max {
            level = 1;
        } else if raw <= min {
            level = 0;
        }
        *slot = level;
    }

    // The first transition is taken as the middle of the first bit.
    let mut firstedge = source_width - 1;
    for x in 0..source_width - 1 {
        if srcabs[x] != srcabs[x + 1] {
            firstedge = x;
            break;
        }
    }
    if firstedge == source_width - 1 {
        return 0;
    }

    let edge_at = |i: i64| -> u8 { srcabs[i.clamp(0, source_width as i64 - 1) as usize] };

    // Scan for a clock that has a nearby transition on each beat.
    let step = 1.0 / expected_bits as f64;
    let mut clock = source_width as f64 / expected_bits as f64;
    let mut bestclock = 0.0f64;
    let mut besterr = 1000i32;
    while clock >= 2.0 {
        let mut error = 0i32;
        let mut x = 1usize;
        while x < expected_bits {
            let curbit = (firstedge as f64 + x as f64 * clock) as i64;
            let mut offby = 0i32;
            while offby <= MAX_CLOCK_DIFF {
                let off = i64::from(offby);
                if edge_at(curbit + off) != edge_at(curbit + off + 1)
                    || edge_at(curbit - off) != edge_at(curbit - off + 1)
                {
                    break;
                }
                offby += 1;
            }
            if offby > MAX_CLOCK_DIFF {
                break;
            }
            error += offby;
            if error >= besterr {
                break;
            }
            x += 1;
        }
        if x == expected_bits {
            besterr = error;
            bestclock = clock;
        }
        clock -= step;
    }
    if bestclock <= 0.0 {
        return 0;
    }

    for (x, slot) in result.iter_mut().take(expected_bits).enumerate() {
        let xf = x as f64;
        let base = firstedge as f64;
        let leftstart = (base + ((xf - 0.5) * bestclock).ceil()) as i64;
        let leftend = (base + (xf * bestclock).floor()) as i64;
        let rightstart = (base + (xf * bestclock).ceil()) as i64;
        let rightend = (base + ((xf + 0.5) * bestclock).floor()) as i64;

        let mut leftavg = 0i32;
        for tx in leftstart..=leftend {
            leftavg += i32::from(luma(tx)) - i32::from(mid);
        }
        let mut rightavg = 0i32;
        for tx in rightstart..=rightend {
            rightavg += i32::from(luma(tx)) - i32::from(mid);
        }

        let leftabs = leftavg >= 0;
        let rightabs = rightavg >= 0;
        // Every bit is marked by a transition; without one the line is junk.
        if leftabs == rightabs {
            return 0;
        }

        let confidence = leftavg.unsigned_abs() + rightavg.unsigned_abs();
        // The halves differ, so the bit value is just the right-hand level.
        *slot = u32::from(rightabs) | (confidence << 1);
    }
    expected_bits
}

/// Returns whether the white flag is set on a VBI line.
///
/// True when the luma histogram's peak sits above the 90% mark of the line's
/// noise-trimmed range.
pub fn vbi_parse_white_flag(source: &[u16], source_width: usize, source_shift: u32) -> bool {
    if source.len() < source_width || source_width == 0 {
        return false;
    }
    let mut histo = [0i32; 256];
    for &word in &source[..source_width] {
        histo[usize::from((word >> source_shift) as u8)] += 1;
    }

    // Drop the lowest and highest 1% as noise.
    let mut subtract = (source_width / 100) as i32;
    let mut minval = 0usize;
    while minval < 255 {
        subtract -= histo[minval];
        if subtract < 0 {
            break;
        }
        minval += 1;
    }
    let mut subtract = (source_width / 100) as i32;
    let mut maxval = 255usize;
    while maxval > 0 {
        subtract -= histo[maxval];
        if subtract < 0 {
            break;
        }
        maxval -= 1;
    }
    if maxval < minval + 10 {
        return false;
    }

    let mut peakval = 0usize;
    for x in 1..256 {
        if histo[x] > histo[peakval] {
            peakval = x;
        }
    }
    peakval > minval + 9 * (maxval - minval) / 10
}

/// Writes one 16-byte packed VBI record.
///
/// Layout: frame number (u24be), white flag, then lines 16, 17, 18, and the
/// best-of 17/18 as u24be each.
///
/// # Panics
///
/// Panics if `dest` is shorter than [`VBI_PACKED_BYTES`].
pub fn vbi_metadata_pack(dest: &mut [u8], framenum: u32, vbi: &VbiMetadata) {
    put_u24be(&mut dest[0..3], framenum);
    dest[3] = u8::from(vbi.white);
    put_u24be(&mut dest[4..7], vbi.line16);
    put_u24be(&mut dest[7..10], vbi.line17);
    put_u24be(&mut dest[10..13], vbi.line18);
    put_u24be(&mut dest[13..16], vbi.line1718);
}

fn put_u24be(dest: &mut [u8], value: u32) {
    dest.copy_from_slice(&value.to_be_bytes()[1..4]);
}

/// Returns the CAV picture number carried by `code`, if it is one.
pub fn vbi_cav_picture(code: u32) -> Option<u32> {
    if code & VBI_MASK_CAV_PICTURE != VBI_CODE_CAV_PICTURE {
        return None;
    }
    Some(
        ((code >> 16) & 0x07) * 10000
            + ((code >> 12) & 0x0f) * 1000
            + ((code >> 8) & 0x0f) * 100
            + ((code >> 4) & 0x0f) * 10
            + (code & 0x0f),
    )
}

/// Returns the chapter number carried by `code`, if it is a chapter code.
pub fn vbi_chapter(code: u32) -> Option<u32> {
    if code & VBI_MASK_CHAPTER != VBI_CODE_CHAPTER {
        return None;
    }
    Some(((code >> 16) & 0x07) * 10 + ((code >> 12) & 0x0f))
}

/// Returns the `(hours, minutes)` CLV timecode carried by `code`, if it is one.
pub fn vbi_clv_time(code: u32) -> Option<(u32, u32)> {
    if code & VBI_MASK_CLV_TIME != VBI_CODE_CLV_TIME {
        return None;
    }
    Some((
        (code >> 16) & 0x0f,
        ((code >> 4) & 0x0f) * 10 + (code & 0x0f),
    ))
}

/// CAV vs. CLV, inferred from the Philips codes decoded across a laserdisc's
/// VBI lines.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LdDiscType {
    Cav,
    Clv,
    #[default]
    Unknown,
}

/// An `HH:MM` CLV timecode decoded from a Philips code.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LdClvTime {
    pub hours: u32,
    pub minutes: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOCK: usize = 20;
    const WIDTH: usize = VBI_CODE_BITS * CLOCK;

    /// Synthesizes a Manchester-coded VBI line for `code`.
    ///
    /// Bit `x`'s cell centre sits at `CLOCK / 2 + x * CLOCK`; the left half of
    /// a cell carries the inverse of the bit and the right half the bit, so
    /// every cell centre is a transition.
    fn manchester_line(code: u32) -> Vec<u16> {
        let firstedge = (CLOCK / 2) as i64;
        (0..WIDTH)
            .map(|p| {
                let rel = p as i64 - firstedge;
                let bit_index = (rel + (CLOCK / 2) as i64)
                    .div_euclid(CLOCK as i64)
                    .clamp(0, VBI_CODE_BITS as i64 - 1);
                let bit = (code >> (VBI_CODE_BITS as i64 - 1 - bit_index)) & 1 == 1;
                let level = if rel - bit_index * CLOCK as i64 >= 0 {
                    bit
                } else {
                    !bit
                };
                if level { 0xff00 } else { 0x0000 }
            })
            .collect()
    }

    /// Same Manchester encoding as `manchester_line`, but with configurable
    /// high/low luma levels, so a line's decode confidence can be tuned
    /// independently of its code.
    fn manchester_line_with_levels(code: u32, high: u16, low: u16) -> Vec<u16> {
        let firstedge = (CLOCK / 2) as i64;
        (0..WIDTH)
            .map(|p| {
                let rel = p as i64 - firstedge;
                let bit_index = (rel + (CLOCK / 2) as i64)
                    .div_euclid(CLOCK as i64)
                    .clamp(0, VBI_CODE_BITS as i64 - 1);
                let bit = (code >> (VBI_CODE_BITS as i64 - 1 - bit_index)) & 1 == 1;
                let level = if rel - bit_index * CLOCK as i64 >= 0 {
                    bit
                } else {
                    !bit
                };
                if level { high } else { low }
            })
            .collect()
    }

    fn decode(code: u32) -> u32 {
        let line = manchester_line(code);
        let mut bits = [0u32; VBI_CODE_BITS];
        assert_eq!(
            vbi_parse_manchester_code(&line, WIDTH, 8, VBI_CODE_BITS, &mut bits),
            VBI_CODE_BITS,
            "decode failed for {code:06x}"
        );
        pack_bits(&bits)
    }

    fn white_line(white_pixels: usize) -> Vec<u16> {
        (0..WIDTH)
            .map(|x| if x < white_pixels { 0xff00 } else { 0x0000 })
            .collect()
    }

    #[test]
    fn recovers_known_philips_codes() {
        for code in [
            VBI_CODE_LEADIN,
            VBI_CODE_LEADOUT,
            VBI_CODE_STOP,
            VBI_CODE_CLV,
            0xf00001, // CAV picture 1
            0xf12345, // CAV picture 12345
            0xf1dd32, // CLV 1 hour 32 minutes
            0x812ddd, // chapter 12
            0x555555,
            0xaaaaaa,
        ] {
            assert_eq!(decode(code), code, "code {code:06x}");
        }
    }

    #[test]
    fn decodes_code_semantics() {
        assert_eq!(vbi_cav_picture(0xf12345), Some(12345));
        assert_eq!(vbi_cav_picture(VBI_CODE_LEADIN), None);
        assert_eq!(vbi_chapter(0x812ddd), Some(12));
        assert_eq!(vbi_chapter(0xf12345), None);
        assert_eq!(vbi_clv_time(0xf1dd32), Some((1, 32)));
        assert_eq!(vbi_clv_time(VBI_CODE_CLV), None);
    }

    #[test]
    fn rejects_lines_without_dynamic_range() {
        let flat = vec![0xff00u16; WIDTH];
        let mut bits = [0u32; VBI_CODE_BITS];
        assert_eq!(
            vbi_parse_manchester_code(&flat, WIDTH, 8, VBI_CODE_BITS, &mut bits),
            0
        );
        let too_wide = vec![0u16; MAX_SOURCE_WIDTH + 1];
        assert_eq!(
            vbi_parse_manchester_code(&too_wide, MAX_SOURCE_WIDTH + 1, 8, VBI_CODE_BITS, &mut bits),
            0
        );
    }

    #[test]
    fn detects_the_white_flag() {
        assert!(vbi_parse_white_flag(
            &white_line(WIDTH * 95 / 100),
            WIDTH,
            8
        ));
        assert!(!vbi_parse_white_flag(
            &white_line(WIDTH * 5 / 100),
            WIDTH,
            8
        ));
        // No dynamic range at all is not a white flag.
        assert!(!vbi_parse_white_flag(&vec![0xff00u16; WIDTH], WIDTH, 8));
    }

    #[test]
    fn parses_a_whole_field() {
        let mut field = vec![0u16; 19 * WIDTH];
        field[11 * WIDTH..12 * WIDTH].copy_from_slice(&white_line(WIDTH * 95 / 100));
        field[16 * WIDTH..17 * WIDTH].copy_from_slice(&manchester_line(VBI_CODE_LEADIN));
        field[17 * WIDTH..18 * WIDTH].copy_from_slice(&manchester_line(0xf00042));
        field[18 * WIDTH..19 * WIDTH].copy_from_slice(&manchester_line(0xf00042));

        let vbi = vbi_parse_all(&field, WIDTH, WIDTH, 8);
        assert!(vbi.white);
        assert_eq!(vbi.line16, VBI_CODE_LEADIN);
        assert_eq!(vbi.line17, 0xf00042);
        assert_eq!(vbi.line18, 0xf00042);
        assert_eq!(vbi.line1718, 0xf00042);
        assert_eq!(vbi_cav_picture(vbi.line1718), Some(42));
    }

    #[test]
    fn prefers_the_valid_bcd_picture_number() {
        let mut field = vec![0u16; 19 * WIDTH];
        // 0xf000ab is a CAV picture code whose low nibbles are not BCD.
        field[17 * WIDTH..18 * WIDTH].copy_from_slice(&manchester_line(0xf000ab));
        field[18 * WIDTH..19 * WIDTH].copy_from_slice(&manchester_line(0xf00042));

        let vbi = vbi_parse_all(&field, WIDTH, WIDTH, 8);
        assert_eq!(vbi.line1718, 0xf00042);
    }

    #[test]
    fn picks_the_higher_confidence_line_on_a_bit_tie() {
        let mut field = vec![0u16; 19 * WIDTH];
        // Line 17: full-swing signal, high decode confidence.
        field[17 * WIDTH..18 * WIDTH]
            .copy_from_slice(&manchester_line_with_levels(0x555555, 0xff00, 0x0000));
        // Line 18: same amplitude pattern but low-swing, lower confidence.
        field[18 * WIDTH..19 * WIDTH]
            .copy_from_slice(&manchester_line_with_levels(0xaaaaaa, 0xc800, 0x3800));

        let vbi = vbi_parse_all(&field, WIDTH, WIDTH, 8);
        assert_eq!(vbi.line17, 0x555555);
        assert_eq!(vbi.line18, 0xaaaaaa);
        // Neither code is a CAV picture, so the BCD tiebreak is skipped and
        // the per-bit confidence comparison decides; line 17's stronger
        // signal wins on every bit.
        assert_eq!(vbi.line1718, 0x555555);
    }

    #[test]
    fn falls_back_to_the_surviving_line() {
        let mut field = vec![0u16; 19 * WIDTH];
        field[18 * WIDTH..19 * WIDTH].copy_from_slice(&manchester_line(VBI_CODE_CLV));

        let vbi = vbi_parse_all(&field, WIDTH, WIDTH, 8);
        assert_eq!(vbi.line17, 0);
        assert_eq!(vbi.line18, VBI_CODE_CLV);
        assert_eq!(vbi.line1718, VBI_CODE_CLV);
    }

    #[test]
    fn packs_records_byte_for_byte() {
        let vbi = VbiMetadata {
            white: true,
            line16: 0x88ffff,
            line17: 0xf00042,
            line18: 0x112233,
            line1718: 0xabcdef,
        };
        let mut packed = [0u8; VBI_PACKED_BYTES];
        vbi_metadata_pack(&mut packed, 0x0001_2345, &vbi);
        assert_eq!(
            packed,
            [
                0x01, 0x23, 0x45, 0x01, 0x88, 0xff, 0xff, 0xf0, 0x00, 0x42, 0x11, 0x22, 0x33, 0xab,
                0xcd, 0xef,
            ]
        );

        let mut packed = [0xffu8; VBI_PACKED_BYTES];
        vbi_metadata_pack(&mut packed, 0, &VbiMetadata::default());
        assert_eq!(packed, [0u8; VBI_PACKED_BYTES]);
    }
}
