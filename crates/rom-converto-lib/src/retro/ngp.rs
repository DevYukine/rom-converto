//! Neo Geo Pocket and Neo Geo Pocket Color header parsing.

use super::ascii_trim;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

const HEADER_END: usize = 0x30;

/// The two license strings the BIOS accepts, at offset 0.
const LICENSES: [&[u8; 28]; 2] = [
    b"COPYRIGHT BY SNK CORPORATION",
    b" LICENSED BY SNK CORPORATION",
];

/// Fields of the Neo Geo Pocket cartridge header. The format defines no
/// checksum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NgpInfo {
    pub license: String,
    pub startup_address: u32,
    pub catalog_id: u16,
    pub subcatalog_id: u8,
    pub machine: u8,
    pub machine_name: Option<String>,
    pub title: String,
}

/// Parses the Neo Geo Pocket header at the start of `data`.
///
/// # Errors
/// Returns an error when `data` is shorter than the header or carries
/// neither SNK license string.
pub fn parse(data: &[u8]) -> Result<NgpInfo> {
    let header = data
        .get(..HEADER_END)
        .ok_or_else(|| anyhow!("ngp: file shorter than the 0x30-byte header"))?;
    if !LICENSES.iter().any(|l| &header[..28] == l.as_slice()) {
        return Err(anyhow!("ngp: no SNK license string at offset 0"));
    }

    Ok(NgpInfo {
        license: ascii_trim(&header[..28]),
        startup_address: u32::from_le_bytes([
            header[0x1C],
            header[0x1D],
            header[0x1E],
            header[0x1F],
        ]),
        catalog_id: u16::from_le_bytes([header[0x20], header[0x21]]),
        subcatalog_id: header[0x22],
        machine: header[0x23],
        machine_name: match header[0x23] {
            0x00 => Some("monochrome".to_string()),
            0x10 => Some("color".to_string()),
            _ => None,
        },
        title: ascii_trim(&header[0x24..HEADER_END]),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a header-only Neo Geo Pocket image.
    pub(crate) fn fixture(machine: u8) -> Vec<u8> {
        let mut rom = vec![0u8; HEADER_END];
        rom[..28].copy_from_slice(LICENSES[0]);
        rom[0x1C..0x20].copy_from_slice(&0x0000_2000u32.to_le_bytes());
        rom[0x20..0x22].copy_from_slice(&0x004Du16.to_le_bytes());
        rom[0x22] = 0x01;
        rom[0x23] = machine;
        rom[0x24..HEADER_END].fill(b' ');
        rom[0x24..0x2C].copy_from_slice(b"TESTGAME");
        rom
    }

    #[test]
    fn reads_header() {
        let info = parse(&fixture(0x10)).unwrap();
        assert_eq!(info.license, "COPYRIGHT BY SNK CORPORATION");
        assert_eq!(info.startup_address, 0x2000);
        assert_eq!(info.catalog_id, 0x4D);
        assert_eq!(info.subcatalog_id, 1);
        assert_eq!(info.machine_name.as_deref(), Some("color"));
        assert_eq!(info.title, "TESTGAME");
    }

    #[test]
    fn accepts_licensed_variant() {
        let mut rom = fixture(0x00);
        rom[..28].copy_from_slice(LICENSES[1]);
        let info = parse(&rom).unwrap();
        assert_eq!(info.license, "LICENSED BY SNK CORPORATION");
        assert_eq!(info.machine_name.as_deref(), Some("monochrome"));
    }

    #[test]
    fn rejects_missing_license() {
        let mut rom = fixture(0x00);
        rom[0] = b'X';
        assert!(parse(&rom).is_err());
        assert!(parse(&rom[..8]).is_err());
    }
}
