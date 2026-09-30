//! Trimmed-ROM detection for cartridge images that carry padding tails.
//!
//! Only known-trimmable signatures are scanned, a file is trimmed only
//! when its size is not a power of two, and the padded form is the next
//! power of two up.

/// A detected padded tail: the size the ROM is padded out to.
pub(super) struct TrimInfo {
    pub padded_size: u64,
}

/// The compressed Nintendo bitmap the BIOS checks, at 0x04 of a GBA
/// header, 0xC0 of an NDS/DSi header (both signatures are keyed on it; the
/// copy in `nintendo::agb` is private to that module).
const NINTENDO_LOGO: [u8; 156] = [
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A, 0x84, 0xE4, 0x09, 0xAD,
    0x11, 0x24, 0x8B, 0x98, 0xC0, 0x81, 0x7F, 0x21, 0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20,
    0x10, 0x46, 0x4A, 0x4A, 0xF8, 0x27, 0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF,
    0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94, 0x56, 0x8A, 0xC0, 0x13, 0x72, 0xA7, 0xFC,
    0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61, 0x58, 0x97, 0xA3, 0x27, 0xFC, 0x03, 0x98, 0x76,
    0x23, 0x1D, 0xC7, 0x61, 0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E, 0xFD,
    0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85, 0x60, 0xD6, 0x80, 0x25,
    0xA9, 0x63, 0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2, 0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44,
    0x78, 0x00, 0x90, 0xCB, 0x88, 0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF,
    0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72, 0x21, 0xD4, 0xF8, 0x07,
];

/// The NDS/DSi logo checksum bytes at 0x15C.
const NDS_LOGO_CHECKSUM: [u8; 2] = [0x56, 0xCF];

/// Detects a padded tail from the first bytes of a ROM file: `Some` only
/// for a trimmable signature whose size is not already a power of two.
/// 3DS dumps are never scanned: their units always convert, and padding is
/// only applied by the zip/copy placements, so a detected tail there could
/// never be used. Detected cartridges pad with 0xFF unless a DAT match
/// verified another fill.
pub(super) fn detect_trim(ext: &str, size: u64, head: &[u8]) -> Option<TrimInfo> {
    let trimmable = match ext {
        // https://problemkaputt.de/gbatek.htm#gbacartridges: logo at 0x04.
        "gba" => head
            .get(0x04..0xA0)
            .is_some_and(|logo| logo == NINTENDO_LOGO),
        // https://dsibrew.org/wiki/DSi_cartridge_header: logo at 0xC0 plus
        // its checksum at 0x15C; DSi adds the unit code at 0x12.
        "nds" | "dsi" => {
            head.get(0xC0..0x15C)
                .is_some_and(|logo| logo == NINTENDO_LOGO)
                && head.get(0x15C..0x15E) == Some(&NDS_LOGO_CHECKSUM[..])
                && (ext == "nds" || head.get(0x12) == Some(&0x03))
        }
        _ => false,
    };
    // A power-of-two (or empty) image fills its chip exactly: not trimmed.
    if !trimmable || size == 0 || size.is_power_of_two() {
        return None;
    }
    Some(TrimInfo {
        padded_size: size.next_power_of_two(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn gba_head() -> Vec<u8> {
        let mut head = vec![0u8; 512];
        head[0x04..0xA0].copy_from_slice(&NINTENDO_LOGO);
        head
    }

    fn nds_head(unit_code: u8) -> Vec<u8> {
        let mut head = vec![0u8; 512];
        head[0x12] = unit_code;
        head[0xC0..0x15C].copy_from_slice(&NINTENDO_LOGO);
        head[0x15C..0x15E].copy_from_slice(&NDS_LOGO_CHECKSUM);
        head
    }

    fn tds_head() -> Vec<u8> {
        let mut head = vec![0u8; 512];
        head[0x100..0x104].copy_from_slice(b"NCSD");
        head
    }

    #[test]
    fn gba_needs_logo_and_non_power_of_two_size() {
        let trimmed = detect_trim("gba", 4 * MIB - 0x10000, &gba_head()).expect("trimmed gba");
        assert_eq!(trimmed.padded_size, 4 * MIB);
        // A full chip size is not trimmed.
        assert!(detect_trim("gba", 4 * MIB, &gba_head()).is_none());
        // A corrupt logo is not a GBA signature.
        let mut corrupt = gba_head();
        corrupt[0x10] ^= 0xFF;
        assert!(detect_trim("gba", 4 * MIB - 0x10000, &corrupt).is_none());
    }

    #[test]
    fn nds_and_dsi_need_logo_checksum() {
        let trimmed = detect_trim("nds", 64 * MIB - 0x10000, &nds_head(0)).expect("trimmed nds");
        assert_eq!(trimmed.padded_size, 64 * MIB);
        // DSi is the same signature plus the unit code at 0x12.
        assert!(detect_trim("dsi", 16 * MIB - 0x1000, &nds_head(0x03)).is_some());
        assert!(detect_trim("dsi", 16 * MIB - 0x1000, &nds_head(0)).is_none());
        // Without the checksum bytes the signature does not hold.
        let mut no_checksum = nds_head(0);
        no_checksum[0x15C] = 0x00;
        assert!(detect_trim("nds", 64 * MIB - 0x10000, &no_checksum).is_none());
    }

    #[test]
    fn tds_is_never_scanned() {
        // 3DS units always convert and padding only applies to zip/copy
        // placements, so the NCSD signature is not trimmable here.
        assert!(detect_trim("3ds", 512 * MIB - 0x100000, &tds_head()).is_none());
    }

    #[test]
    fn unknown_signatures_and_sizes_never_trim() {
        assert!(detect_trim("sfc", 100, &[0u8; 512]).is_none());
        assert!(detect_trim("gba", 0, &gba_head()).is_none());
        assert!(detect_trim("nds", 64 * MIB - 0x10000, &[0u8; 512]).is_none());
        // A short head cannot hold the NDS signature.
        assert!(detect_trim("nds", 64 * MIB - 0x10000, &[0u8; 16]).is_none());
    }
}
