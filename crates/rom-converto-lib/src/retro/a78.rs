//! Atari 7800 A78 header parsing.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const HEADER_LEN: usize = 128;
const MAGIC: [u8; 9] = *b"ATARI7800";

/// Fields of the A78 header. The format defines no checksum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A78Info {
    pub version: u8,
    pub title: String,
    pub cart_size: u32,
    pub cart_type: u16,
    pub cart_features: Vec<String>,
    pub controller1: u8,
    pub controller1_name: Option<String>,
    pub controller2: u8,
    pub controller2_name: Option<String>,
    pub tv_type: String,
    pub save_device: u8,
}

/// Parses the 128-byte A78 header at the start of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the header or lacks the
/// `ATARI7800` signature at offset 1.
pub fn parse(data: &[u8]) -> Result<A78Info> {
    let header = data
        .get(..HEADER_LEN)
        .ok_or_else(|| anyhow!("a78: file shorter than the 128-byte A78 header"))?;
    if header[1..10] != MAGIC {
        return Err(anyhow!(
            "a78: bad magic, expected \"ATARI7800\" at offset 1"
        ));
    }

    Ok(A78Info {
        version: header[0],
        title: ascii_trim(&header[0x11..0x31]),
        cart_size: u32::from_be_bytes([header[0x31], header[0x32], header[0x33], header[0x34]]),
        cart_type: u16::from_be_bytes([header[0x35], header[0x36]]),
        cart_features: cart_features(header[0x36]),
        controller1: header[0x37],
        controller1_name: controller(header[0x37]).map(str::to_string),
        controller2: header[0x38],
        controller2_name: controller(header[0x38]).map(str::to_string),
        tv_type: if header[0x39] == 0 {
            "NTSC".to_string()
        } else {
            "PAL".to_string()
        },
        save_device: header[0x3A],
    })
}

/// Decodes the low byte of the cart type word. The high byte carries later
/// extensions and is reported raw only.
fn cart_features(low: u8) -> Vec<String> {
    [
        (0x01, "POKEY at $4000"),
        (0x02, "SuperGame bank switched"),
        (0x04, "SuperGame RAM at $4000"),
        (0x08, "ROM at $4000"),
        (0x10, "bank 6 at $4000"),
        (0x20, "SuperGame banked RAM"),
        (0x40, "POKEY at $0450"),
        (0x80, "mirror RAM at $4000"),
    ]
    .into_iter()
    .filter(|(mask, _)| low & mask != 0)
    .map(|(_, name)| name.to_string())
    .collect()
}

fn controller(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => "none",
        1 => "joystick",
        2 => "light gun",
        3 => "paddle",
        4 => "trackball",
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a header-only A78 image.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut rom = vec![0u8; HEADER_LEN];
        rom[0] = 4;
        rom[1..10].copy_from_slice(&MAGIC);
        rom[0x11..0x31].fill(b' ');
        rom[0x11..0x19].copy_from_slice(b"TESTGAME");
        rom[0x31..0x35].copy_from_slice(&0x0002_0000u32.to_be_bytes());
        rom[0x35..0x37].copy_from_slice(&0x0003u16.to_be_bytes());
        rom[0x37] = 1;
        rom[0x38] = 3;
        rom[0x39] = 1;
        rom[0x3A] = 2;
        rom
    }

    #[test]
    fn reads_header() {
        let info = parse(&fixture()).unwrap();
        assert_eq!(info.version, 4);
        assert_eq!(info.title, "TESTGAME");
        assert_eq!(info.cart_size, 0x20000);
        assert_eq!(info.cart_type, 3);
        assert_eq!(
            info.cart_features,
            ["POKEY at $4000", "SuperGame bank switched"]
        );
        assert_eq!(info.controller1_name.as_deref(), Some("joystick"));
        assert_eq!(info.controller2_name.as_deref(), Some("paddle"));
        assert_eq!(info.tv_type, "PAL");
        assert_eq!(info.save_device, 2);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut rom = fixture();
        rom[1] = b'X';
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..32]).is_err());
    }
}
