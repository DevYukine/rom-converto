//! Nintendo 64 cartridge header parsing, across the three byte orders
//! dumpers produce.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use crc::{CRC_32_ISO_HDLC, Crc};
use serde::{Deserialize, Serialize};

static CRC32_ISO_HDLC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

/// The header plus the IPL3 boot code, the region the parser needs in
/// native byte order.
const HEAD_LEN: usize = 0x1000;

/// Fields of the N64 cartridge header, with the boot code CRC used to
/// identify the CIC lockout chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N64Info {
    pub byte_order: String,
    pub internal_name: String,
    pub game_id: String,
    pub media: String,
    pub region_code: String,
    pub region: Option<String>,
    pub version: u8,
    pub crc1: String,
    pub crc2: String,
    pub bootcode_crc32: String,
    pub cic: Option<String>,
}

/// Parses the N64 header at the start of `data`, normalizing v64 and n64
/// byte orders to big-endian first.
///
/// # Errors
/// Returns an error when `data` is shorter than the header plus boot code,
/// or starts with none of the three known byte-order signatures.
pub fn parse(data: &[u8]) -> Result<N64Info> {
    let head = data
        .get(..HEAD_LEN)
        .ok_or_else(|| anyhow!("n64: file shorter than the 4 KiB header and boot code"))?;

    let (byte_order, head) = match head[..4] {
        [0x80, 0x37, 0x12, 0x40] => ("z64", head.to_vec()),
        [0x37, 0x80, 0x40, 0x12] => ("v64", swap(head, 2)),
        [0x40, 0x12, 0x37, 0x80] => ("n64", swap(head, 4)),
        _ => return Err(anyhow!("n64: unrecognized byte order signature")),
    };

    let bootcode_crc32 = CRC32_ISO_HDLC.checksum(&head[0x40..HEAD_LEN]);

    Ok(N64Info {
        byte_order: byte_order.to_string(),
        internal_name: ascii_trim(&head[0x20..0x34]),
        game_id: ascii_trim(&head[0x3C..0x3E]),
        media: ascii_trim(&head[0x3B..0x3C]),
        region_code: ascii_trim(&head[0x3E..0x3F]),
        region: region(head[0x3E]).map(str::to_string),
        version: head[0x3F],
        crc1: format!(
            "{:08X}",
            u32::from_be_bytes([head[0x10], head[0x11], head[0x12], head[0x13]])
        ),
        crc2: format!(
            "{:08X}",
            u32::from_be_bytes([head[0x14], head[0x15], head[0x16], head[0x17]])
        ),
        bootcode_crc32: format!("{bootcode_crc32:08X}"),
        cic: cic(bootcode_crc32, head[0x3E]).map(str::to_string),
    })
}

/// Reverses each `width`-byte group, turning a v64 (halfword-swapped) or
/// n64 (word-swapped) image into big-endian z64 order.
fn swap(data: &[u8], width: usize) -> Vec<u8> {
    data.chunks(width)
        .flat_map(|c| c.iter().rev().copied())
        .collect()
}

/// NTSC and PAL variants of the same CIC share identical boot code, so the
/// header's region byte picks between the 6xxx and 7xxx names.
fn cic(bootcode_crc32: u32, region: u8) -> Option<&'static str> {
    let pal = matches!(
        region,
        b'D' | b'F' | b'H' | b'I' | b'P' | b'S' | b'U' | b'W' | b'X' | b'Y'
    );
    Some(match (bootcode_crc32, pal) {
        (0x6170A4A1, _) => "CIC-NUS-6101",
        (0x90BB6CB5, false) => "CIC-NUS-6102",
        (0x90BB6CB5, true) => "CIC-NUS-7101",
        (0x0B050EE0, false) => "CIC-NUS-6103",
        (0x0B050EE0, true) => "CIC-NUS-7103",
        (0x98BC2C86, false) => "CIC-NUS-6105",
        (0x98BC2C86, true) => "CIC-NUS-7105",
        (0xACC8580A, false) => "CIC-NUS-6106",
        (0xACC8580A, true) => "CIC-NUS-7106",
        _ => return None,
    })
}

fn region(code: u8) -> Option<&'static str> {
    Some(match code {
        b'7' => "Beta",
        b'A' => "Asia (NTSC)",
        b'B' => "Brazil",
        b'C' => "China",
        b'D' => "Germany",
        b'E' => "North America",
        b'F' => "France",
        b'G' => "Gateway 64 (NTSC)",
        b'H' => "Netherlands",
        b'I' => "Italy",
        b'J' => "Japan",
        b'K' => "South Korea",
        b'L' => "Gateway 64 (PAL)",
        b'N' => "Canada",
        b'P' => "Europe",
        b'S' => "Spain",
        b'U' => "Australia",
        b'W' => "Scandinavia",
        b'X' | b'Y' => "Europe",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn cic_pal_pairs_pick_by_region() {
        assert_eq!(cic(0x90BB6CB5, b'E'), Some("CIC-NUS-6102"));
        assert_eq!(cic(0x90BB6CB5, b'P'), Some("CIC-NUS-7101"));
        assert_eq!(cic(0x98BC2C86, b'D'), Some("CIC-NUS-7105"));
        assert_eq!(cic(0x6170A4A1, b'P'), Some("CIC-NUS-6101"));
        assert_eq!(cic(0xDEAD_BEEF, b'E'), None);
    }

    /// Builds a 4 KiB big-endian N64 image with a filled header.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut rom = vec![0u8; HEAD_LEN];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        rom[0x10..0x14].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        rom[0x14..0x18].copy_from_slice(&0x9ABC_DEF0u32.to_be_bytes());
        rom[0x20..0x28].copy_from_slice(b"TEST ROM");
        rom[0x3B] = b'N';
        rom[0x3C..0x3E].copy_from_slice(b"TR");
        rom[0x3E] = b'E';
        rom[0x3F] = 0x01;
        for (i, b) in rom[0x40..].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        rom
    }

    #[test]
    fn reads_z64_header() {
        let info = parse(&fixture()).unwrap();
        assert_eq!(info.byte_order, "z64");
        assert_eq!(info.internal_name, "TEST ROM");
        assert_eq!(info.game_id, "TR");
        assert_eq!(info.media, "N");
        assert_eq!(info.region_code, "E");
        assert_eq!(info.region.as_deref(), Some("North America"));
        assert_eq!(info.version, 1);
        assert_eq!(info.crc1, "12345678");
        assert_eq!(info.crc2, "9ABCDEF0");
        assert_eq!(info.cic, None);
    }

    #[test]
    fn byte_orders_agree() {
        let z64 = fixture();
        let v64 = swap(&z64, 2);
        let n64 = swap(&z64, 4);

        let a = parse(&z64).unwrap();
        let b = parse(&v64).unwrap();
        let c = parse(&n64).unwrap();

        assert_eq!(b.byte_order, "v64");
        assert_eq!(c.byte_order, "n64");
        for other in [&b, &c] {
            assert_eq!(other.internal_name, a.internal_name);
            assert_eq!(other.game_id, a.game_id);
            assert_eq!(other.crc1, a.crc1);
            assert_eq!(other.bootcode_crc32, a.bootcode_crc32);
        }
    }

    #[test]
    fn rejects_unknown_byte_order() {
        let mut rom = fixture();
        rom[0] = 0x00;
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..0x40]).is_err());
    }
}
