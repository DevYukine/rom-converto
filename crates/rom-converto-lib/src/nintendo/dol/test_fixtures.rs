//! Synthetic GameCube disc fixtures for tests.

#![cfg(test)]

use crate::nintendo::dol::constants::{GAMECUBE_MAGIC, GAMECUBE_MAGIC_OFFSET};
use crate::nintendo::dol::models::boot_bin::{BOOT_BIN_FST_OFFSET_FIELD, BOOT_BIN_FST_SIZE_FIELD};

/// Offset within the fixture where the small FST built by
/// [`make_fake_gamecube_iso_with_fst`] lives.
const FST_FIXTURE_OFFSET: u32 = 0x3000;

/// Build a fake GameCube disc image of `size` bytes. The first 0x80 bytes
/// contain the GameCube magic at the correct offset; the rest is a
/// compressible repeating pattern so round-trip tests can assert byte
/// equality without bloating the fixture with random data.
///
/// Carries no FST: shared by round-trip and scrubbing tests (rvz, wia,
/// wbfs, gcz) whose byte-equality and usage-table expectations depend on
/// the boot.bin FST fields staying zero. Use
/// [`make_fake_gamecube_iso_with_fst`] when the test needs a file layout.
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

/// Like [`make_fake_gamecube_iso`], but embeds a small FST at
/// [`FST_FIXTURE_OFFSET`] with boot.bin's fst_offset/fst_size fields
/// pointing at it: one `opening.bnr` file and one `sub` subdirectory,
/// laid out like the fixture built by `fst.rs`'s tests.
pub fn make_fake_gamecube_iso_with_fst(size: usize) -> Vec<u8> {
    let mut data = make_fake_gamecube_iso(size);

    let fst = build_fake_fst();
    if size >= FST_FIXTURE_OFFSET as usize + fst.len() {
        let start = FST_FIXTURE_OFFSET as usize;
        data[start..start + fst.len()].copy_from_slice(&fst);
        data[BOOT_BIN_FST_OFFSET_FIELD as usize..BOOT_BIN_FST_OFFSET_FIELD as usize + 4]
            .copy_from_slice(&FST_FIXTURE_OFFSET.to_be_bytes());
        data[BOOT_BIN_FST_SIZE_FIELD as usize..BOOT_BIN_FST_SIZE_FIELD as usize + 4]
            .copy_from_slice(&(fst.len() as u32).to_be_bytes());
    }

    data
}

/// Builds a tiny FST: root, `opening.bnr` (top-level file), `sub`
/// (top-level directory), `sub/nested` (file inside `sub`). Mirrors the
/// fixture built by `fst.rs`'s own tests.
fn build_fake_fst() -> Vec<u8> {
    let total_entries: u32 = 4;
    let string_table = b"opening.bnr\0sub\0nested\0";
    let mut entries: Vec<u8> = Vec::new();

    // Entry 0: root directory, size = total_entries.
    entries.push(1);
    entries.extend_from_slice(&[0, 0, 0]);
    entries.extend_from_slice(&[0, 0, 0, 0]);
    entries.extend_from_slice(&total_entries.to_be_bytes());

    // Entry 1: opening.bnr file.
    let bnr_data_offset: u32 = 0x40000;
    let bnr_size: u32 = 0x1840;
    entries.push(0);
    entries.extend_from_slice(&[0, 0, 0]);
    entries.extend_from_slice(&bnr_data_offset.to_be_bytes());
    entries.extend_from_slice(&bnr_size.to_be_bytes());

    // Entry 2: sub directory, next_index = 4 (past the last entry).
    entries.push(1);
    entries.extend_from_slice(&[0, 0, 12]);
    entries.extend_from_slice(&[0, 0, 0, 0]);
    entries.extend_from_slice(&4u32.to_be_bytes());

    // Entry 3: sub/nested file.
    entries.push(0);
    entries.extend_from_slice(&[0, 0, 16]);
    entries.extend_from_slice(&0x50000u32.to_be_bytes());
    entries.extend_from_slice(&0x100u32.to_be_bytes());

    let mut buf = entries;
    buf.extend_from_slice(string_table);
    buf
}
