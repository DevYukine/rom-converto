//! XCI gamecard header (0xF000 prefix) writer for freshly built super
//! cartridge images. The signature at 0x000 is forged, so the output
//! fails gamecard signature checks and is intended for emulators.

use sha2::{Digest, Sha256};

/// Byte length of the gamecard prefix that precedes the root HFS0.
pub const XCI_PREFIX_SIZE: usize = 0xF000;
/// Gamecard media unit; every partition starts on a multiple of it.
pub const MEDIA_UNIT: u64 = 0x200;

const HEAD_MAGIC: &[u8; 4] = b"HEAD";
const PACKAGE_ID: u64 = 0x8750F4C0A9C5A966;

/// Gamecard capacity byte from the total image size, per switchbrew's
/// `RomSize` enum. Emulators ignore it; installers do not.
pub fn card_size_byte(total_size: u64) -> u8 {
    const GIB: u64 = 1 << 30;
    if total_size <= GIB {
        0xFA
    } else if total_size <= 2 * GIB {
        0xF8
    } else if total_size <= 4 * GIB {
        0xF0
    } else if total_size <= 8 * GIB {
        0xE0
    } else if total_size <= 16 * GIB {
        0xE1
    } else {
        0xE2
    }
}

/// Build the 0xF000-byte gamecard prefix that precedes the root HFS0.
/// `secure_offset` is the absolute, 0x200-aligned start of the secure
/// partition; `total_size` is the final image length. All fields are
/// little-endian except the big-endian package id.
pub fn build_xci_prefix(
    secure_offset: u64,
    total_size: u64,
    root_hfs0_header: &[u8],
) -> [u8; 0xF000] {
    let mut p = [0u8; XCI_PREFIX_SIZE];

    // 0x000..0x100: forged gamecard signature.
    p[0x000..0x100].fill(0xFF);

    p[0x100..0x104].copy_from_slice(HEAD_MAGIC);
    let base = 0x100;
    let secure_media = (secure_offset / MEDIA_UNIT) as u32;

    p[base + 0x04..base + 0x08].copy_from_slice(&secure_media.to_le_bytes());
    p[base + 0x08..base + 0x0C].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    p[base + 0x0C] = 0; // KEK index
    p[base + 0x0D] = card_size_byte(total_size);
    p[base + 0x0E] = 0;
    p[base + 0x0F] = 0;
    p[base + 0x10..base + 0x18].copy_from_slice(&PACKAGE_ID.to_be_bytes());
    let valid_data_end = total_size.div_ceil(MEDIA_UNIT).saturating_sub(1);
    p[base + 0x18..base + 0x20].copy_from_slice(&valid_data_end.to_le_bytes());
    p[base + 0x30..base + 0x38].copy_from_slice(&(XCI_PREFIX_SIZE as u64).to_le_bytes());
    p[base + 0x38..base + 0x40].copy_from_slice(&(root_hfs0_header.len() as u64).to_le_bytes());
    let hash = Sha256::digest(root_hfs0_header);
    p[base + 0x40..base + 0x60].copy_from_slice(&hash);
    p[base + 0x8C..base + 0x90].copy_from_slice(&secure_media.to_le_bytes());

    // 0x7000..0x8000: fake gamecard certificate region.
    p[0x7000..0x8000].fill(0xFF);

    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_size_byte_thresholds() {
        const GIB: u64 = 1 << 30;
        assert_eq!(card_size_byte(0), 0xFA);
        assert_eq!(card_size_byte(GIB), 0xFA);
        assert_eq!(card_size_byte(GIB + 1), 0xF8);
        assert_eq!(card_size_byte(2 * GIB), 0xF8);
        assert_eq!(card_size_byte(2 * GIB + 1), 0xF0);
        assert_eq!(card_size_byte(4 * GIB), 0xF0);
        assert_eq!(card_size_byte(4 * GIB + 1), 0xE0);
        assert_eq!(card_size_byte(8 * GIB), 0xE0);
        assert_eq!(card_size_byte(8 * GIB + 1), 0xE1);
        assert_eq!(card_size_byte(16 * GIB), 0xE1);
        assert_eq!(card_size_byte(16 * GIB + 1), 0xE2);
    }

    #[test]
    fn xci_prefix_field_offsets() {
        let root_hdr = vec![0xAAu8; 0x200];
        let secure_offset = 0xF600u64;
        let total_size = 0x2000_0000u64;
        let p = build_xci_prefix(secure_offset, total_size, &root_hdr);

        assert_eq!(&p[0x100..0x104], b"HEAD");
        assert_eq!(
            u32::from_le_bytes(p[0x104..0x108].try_into().unwrap()),
            (secure_offset / MEDIA_UNIT) as u32
        );
        assert_eq!(&p[0x108..0x10C], &[0xFF; 4]);
        assert_eq!(p[0x10C], 0);
        assert_eq!(p[0x10D], card_size_byte(total_size));
        assert_eq!(&p[0x110..0x118], &PACKAGE_ID.to_be_bytes());
        assert_eq!(
            u64::from_le_bytes(p[0x118..0x120].try_into().unwrap()),
            total_size.div_ceil(MEDIA_UNIT) - 1
        );
        assert_eq!(
            u64::from_le_bytes(p[0x130..0x138].try_into().unwrap()),
            0xF000
        );
        assert_eq!(
            u64::from_le_bytes(p[0x138..0x140].try_into().unwrap()),
            root_hdr.len() as u64
        );
        assert_eq!(&p[0x140..0x160], Sha256::digest(&root_hdr).as_slice());
        assert_eq!(
            u32::from_le_bytes(p[0x18C..0x190].try_into().unwrap()),
            (secure_offset / MEDIA_UNIT) as u32
        );
        assert!(p[0x7000..0x8000].iter().all(|&b| b == 0xFF));
        assert!(p[0x000..0x100].iter().all(|&b| b == 0xFF));
    }
}
