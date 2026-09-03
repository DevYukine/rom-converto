//! Atari Lynx LNX header parsing.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const HEADER_LEN: usize = 64;
const MAGIC: [u8; 4] = *b"LYNX";

/// Fields of the LNX header. The format defines no checksum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LynxInfo {
    pub bank0_page_size: u16,
    pub bank1_page_size: u16,
    pub version: u16,
    pub cart_name: String,
    pub manufacturer: String,
    pub rotation: u8,
    pub rotation_name: Option<String>,
}

/// Parses the 64-byte LNX header at the start of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the header or does not
/// start with the `LYNX` magic.
pub fn parse(data: &[u8]) -> Result<LynxInfo> {
    let header = data
        .get(..HEADER_LEN)
        .ok_or_else(|| anyhow!("lynx: file shorter than the 64-byte LNX header"))?;
    if header[..4] != MAGIC {
        return Err(anyhow!("lynx: bad magic, expected \"LYNX\""));
    }

    Ok(LynxInfo {
        bank0_page_size: u16::from_le_bytes([header[4], header[5]]),
        bank1_page_size: u16::from_le_bytes([header[6], header[7]]),
        version: u16::from_le_bytes([header[8], header[9]]),
        cart_name: ascii_trim(&header[0x0A..0x2A]),
        manufacturer: ascii_trim(&header[0x2A..0x3A]),
        rotation: header[0x3A],
        rotation_name: match header[0x3A] {
            0 => Some("none".to_string()),
            1 => Some("left".to_string()),
            2 => Some("right".to_string()),
            _ => None,
        },
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a header-only LNX image.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut rom = vec![0u8; HEADER_LEN];
        rom[..4].copy_from_slice(&MAGIC);
        rom[4..6].copy_from_slice(&256u16.to_le_bytes());
        rom[6..8].copy_from_slice(&512u16.to_le_bytes());
        rom[8..10].copy_from_slice(&1u16.to_le_bytes());
        rom[0x0A..0x12].copy_from_slice(b"TESTCART");
        rom[0x2A..0x30].copy_from_slice(b"ATARI ");
        rom[0x3A] = 1;
        rom
    }

    #[test]
    fn reads_header() {
        let info = parse(&fixture()).unwrap();
        assert_eq!(info.bank0_page_size, 256);
        assert_eq!(info.bank1_page_size, 512);
        assert_eq!(info.version, 1);
        assert_eq!(info.cart_name, "TESTCART");
        assert_eq!(info.manufacturer, "ATARI");
        assert_eq!(info.rotation_name.as_deref(), Some("left"));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut rom = fixture();
        rom[0] = b'X';
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..16]).is_err());
    }
}
