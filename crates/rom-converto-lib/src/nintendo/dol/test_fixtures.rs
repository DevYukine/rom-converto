//! Synthetic GameCube disc fixtures for tests.

#![cfg(test)]

use crate::nintendo::dol::constants::{GAMECUBE_MAGIC, GAMECUBE_MAGIC_OFFSET};

/// Build a fake GameCube disc image of `size` bytes. The first 0x80 bytes
/// contain the GameCube magic at the correct offset; the rest is a
/// compressible repeating pattern so round-trip tests can assert byte
/// equality without bloating the fixture with random data.
pub fn make_fake_gamecube_iso(size: usize) -> Vec<u8> {
    assert!(size >= 0x80, "synthetic GC ISO must fit the disc header");
    let mut data = vec![0u8; size];
    data[GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4]
        .copy_from_slice(&GAMECUBE_MAGIC.to_be_bytes());
    for (i, b) in data.iter_mut().enumerate().skip(0x80) {
        *b = (i % 251) as u8;
    }
    data
}
