//! Synthetic Nintendo DS images for the secure-area crypto tests. The
//! secure-area filler is deterministic pseudo-random so a round trip has to
//! restore real payload bytes rather than a run of zeroes.

#![cfg(test)]

use crate::nintendo::nds::{DECRYPTED_MARKER, SECURE_AREA_END, SECURE_AREA_OFFSET};

/// Game code baked into [`synth_nds`].
pub const SYNTH_IDCODE: [u8; 4] = *b"ARCE";

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
