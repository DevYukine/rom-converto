//! Virtual Boy header parsing. The header sits in the last 0x220 bytes of
//! the ROM, where the CPU maps it just below the vector table.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const HEADER_FROM_END: usize = 0x220;

/// Fields of the Virtual Boy ROM header. The format defines no checksum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VbInfo {
    pub title: String,
    pub maker_code: String,
    pub game_code: String,
    pub version: u8,
}

/// Parses the Virtual Boy header from the tail of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the header.
pub fn parse(data: &[u8]) -> Result<VbInfo> {
    let base = data
        .len()
        .checked_sub(HEADER_FROM_END)
        .ok_or_else(|| anyhow!("vb: file shorter than the 0x220-byte header tail"))?;
    let header = &data[base..];

    Ok(VbInfo {
        title: ascii_trim(&header[..20]),
        maker_code: ascii_trim(&header[24..26]),
        game_code: ascii_trim(&header[26..30]),
        version: header[30],
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a 32 KiB image with a filled header in its tail.
    pub(crate) fn fixture() -> Vec<u8> {
        let mut rom = vec![0u8; 32 * 1024];
        let base = rom.len() - HEADER_FROM_END;
        rom[base..base + 20].fill(b' ');
        rom[base..base + 9].copy_from_slice(b"TEST GAME");
        rom[base + 24..base + 26].copy_from_slice(b"01");
        rom[base + 26..base + 30].copy_from_slice(b"VTGE");
        rom[base + 30] = 0x01;
        rom
    }

    #[test]
    fn reads_header() {
        let info = parse(&fixture()).unwrap();
        assert_eq!(info.title, "TEST GAME");
        assert_eq!(info.maker_code, "01");
        assert_eq!(info.game_code, "VTGE");
        assert_eq!(info.version, 1);
    }

    #[test]
    fn rejects_short_file() {
        assert!(parse(&[0u8; 0x100]).is_err());
    }
}
