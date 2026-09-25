//! Sync ExeFS section reader over any `Read + Seek` source.
//!
//! `crate::nintendo::ctr::decrypt::cia` decrypts NCCH ExeFS sections
//! while streaming the file to disk. The `info` extractor needs the
//! same decryption logic but for a single named entry (typically
//! `icon`, holding the SMDH). Only the ExeFS header and the requested
//! entry's bytes are read and decrypted, so large neighbour entries
//! (`.code` can run to several megabytes) are never pulled into
//! memory. This module shares the same key-derivation helpers and
//! AES-CTR machinery; it does not duplicate any crypto code.
//!
//! Seed crypto is not supported here: titles that require it must be
//! decrypted to disk first via the `decrypt` command. The info path
//! reports that as a clean error rather than fetching seeds over HTTP.

use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek},
};
use anyhow::{Result, anyhow};
use binrw::BinRead;
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use std::io::{Cursor, Read, Seek, SeekFrom};

use crate::nintendo::ctr::constants::{
    CTR_KEYS_0, CTR_KEYS_1, CTR_MEDIA_UNIT_SIZE, EXEFS_ENTRY_SIZE, EXEFS_HEADER_SIZE,
    EXEFS_MAX_FILE_ENTRIES, EXEFS_SECTION_ICON, NCCH_FLAGS7_FIXED_KEY, NCCH_FLAGS7_NOCRYPTO,
    NCCH_FLAGS7_SEED_CRYPTO,
};
use crate::nintendo::ctr::decrypt::cia::{derive_ctr_key, get_ncch_aes_counter};
use crate::nintendo::ctr::decrypt::model::NcchSection;
use crate::nintendo::ctr::models::exe_fs_header::ExeFSHeader;
use crate::nintendo::ctr::models::ncch_header::NcchHeader;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;

fn fixed_key(fixed_crypto: u8) -> Option<[u8; 16]> {
    (fixed_crypto != 0).then(|| u128::to_be_bytes(CTR_KEYS_1[(fixed_crypto as usize) - 1]))
}

/// Builds an AES-128-CTR stream positioned at `at` bytes into the
/// ExeFS region.
fn exefs_cipher(key: &[u8; 16], ctr: &[u8; 16], at: u64) -> Result<Aes128Ctr> {
    let mut cipher =
        Aes128Ctr::new_from_slices(key, ctr).map_err(|e| anyhow!("aes ctr init: {}", e))?;
    cipher
        .try_seek(at)
        .map_err(|e| anyhow!("exefs: cannot seek keystream to {at}: {e}"))?;
    Ok(cipher)
}

/// Decrypt the named ExeFS section and return just that section's
/// plaintext bytes.
///
/// `header` is the NCCH header for the partition and `exefs_abs` the
/// absolute offset of the ExeFS region within `reader`
/// (`exefsoffset * media_unit`). The entry size is validated against
/// `header.exefssize` before any section bytes are read.
pub fn read_exefs_section<R: Read + Seek>(
    reader: &mut R,
    header: &NcchHeader,
    exefs_abs: u64,
    section_name: &[u8],
) -> Result<Vec<u8>> {
    let nocrypto = header.flags[7] & NCCH_FLAGS7_NOCRYPTO != 0;
    let fixed = header.flags[7] & NCCH_FLAGS7_FIXED_KEY != 0;
    let needs_seed = header.flags[7] & NCCH_FLAGS7_SEED_CRYPTO != 0;

    if needs_seed {
        return Err(anyhow!(
            "info: NCCH requires seed crypto; run `ctr decrypt` first"
        ));
    }

    let fixed_crypto = if fixed {
        let mut tid_normal: [u8; 8] = header.titleid;
        tid_normal.reverse();
        if (tid_normal[3] & 16) != 0 { 2u8 } else { 1u8 }
    } else {
        0u8
    };

    let key_y = BigEndian::read_u128(header.signature[0..16].try_into()?);
    let base_key = derive_ctr_key(CTR_KEYS_0[0], key_y);
    let working_key = match fixed_key(fixed_crypto) {
        Some(fk) => fk,
        None => base_key,
    };

    let ctr = get_ncch_aes_counter(header, NcchSection::ExeFS);

    reader.seek(SeekFrom::Start(exefs_abs))?;
    let mut hdr = [0u8; EXEFS_HEADER_SIZE];
    reader.read_exact(&mut hdr)?;
    if !nocrypto {
        exefs_cipher(&working_key, &ctr, 0)?.apply_keystream(&mut hdr);
    }

    // Sections like `icon` / `banner` never want the extra-crypto
    // variant per the canonical decrypt path; the base key is correct.
    // Other sections would re-decrypt with the extra key, but the
    // info path only reads the icon today.

    let (offset, size) = find_exefs_entry(&hdr, section_name)
        .ok_or_else(|| anyhow!("ExeFS section {:?} not found", short_name(section_name)))?;

    // Both bounds run in u64: the entry fields are untrusted u32s, and
    // the reader length check keeps a lying header from allocating
    // `size` bytes that the file cannot contain.
    let start = EXEFS_HEADER_SIZE as u64 + u64::from(offset);
    let end = start + u64::from(size);
    if end > u64::from(header.exefssize) * u64::from(CTR_MEDIA_UNIT_SIZE) {
        return Err(anyhow!("ExeFS section overruns ExeFS"));
    }
    if exefs_abs + end > reader.seek(SeekFrom::End(0))? {
        return Err(anyhow!("ExeFS section overruns file"));
    }

    reader.seek(SeekFrom::Start(exefs_abs + start))?;
    let mut section = vec![0u8; size as usize];
    reader.read_exact(&mut section)?;
    if !nocrypto {
        exefs_cipher(&working_key, &ctr, start)?.apply_keystream(&mut section);
    }
    Ok(section)
}

/// Decrypts and returns the `icon` ExeFS section (the SMDH) read from
/// the ExeFS region at `exefs_abs`.
pub fn read_icon_section<R: Read + Seek>(
    reader: &mut R,
    header: &NcchHeader,
    exefs_abs: u64,
) -> Result<Vec<u8>> {
    read_exefs_section(reader, header, exefs_abs, &EXEFS_SECTION_ICON)
}

fn find_exefs_entry(exefs_hdr: &[u8], name: &[u8]) -> Option<(u32, u32)> {
    for i in 0..EXEFS_MAX_FILE_ENTRIES {
        let off = i * EXEFS_ENTRY_SIZE;
        if off + EXEFS_ENTRY_SIZE > exefs_hdr.len() {
            break;
        }
        let entry =
            ExeFSHeader::read(&mut Cursor::new(&exefs_hdr[off..off + EXEFS_ENTRY_SIZE])).ok()?;
        let entry_name = trim_zero(&entry.file_name);
        if entry_name == name {
            let offset = LittleEndian::read_u32(&entry.file_offset);
            let size = LittleEndian::read_u32(&entry.file_size);
            return Some((offset, size));
        }
    }
    None
}

fn trim_zero(name: &[u8; 8]) -> &[u8] {
    let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    &name[..end]
}

fn short_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::test_fixtures::make_ncch_header_bytes;

    fn synth_header_and_exefs(plaintext_icon: &[u8]) -> (NcchHeader, Vec<u8>) {
        let bytes = make_ncch_header_bytes(0x000400000C123456);
        // The fixture writes the NCCH magic at 0x100; binrw expects to
        // read from the start of the 0x200-byte header structure.
        let mut header = NcchHeader::read(&mut Cursor::new(&bytes)).unwrap();
        header.exefssize = (EXEFS_HEADER_SIZE + plaintext_icon.len())
            .div_ceil(CTR_MEDIA_UNIT_SIZE as usize) as u32;

        let mut exefs = vec![0u8; EXEFS_HEADER_SIZE + plaintext_icon.len()];
        exefs[0..4].copy_from_slice(b"icon");
        exefs[8..12].copy_from_slice(&(0u32).to_le_bytes());
        exefs[12..16].copy_from_slice(&(plaintext_icon.len() as u32).to_le_bytes());
        exefs[EXEFS_HEADER_SIZE..EXEFS_HEADER_SIZE + plaintext_icon.len()]
            .copy_from_slice(plaintext_icon);

        (header, exefs)
    }

    /// ExeFS with `.code` first (so the icon entry has a nonzero
    /// offset) and `icon` second, as on real titles.
    fn synth_code_then_icon_exefs(code: &[u8], icon: &[u8]) -> Vec<u8> {
        let icon_offset = EXEFS_HEADER_SIZE + code.len();
        let mut exefs = vec![0u8; icon_offset + icon.len()];
        exefs[0..5].copy_from_slice(b".code");
        exefs[12..16].copy_from_slice(&(code.len() as u32).to_le_bytes());
        exefs[EXEFS_ENTRY_SIZE..EXEFS_ENTRY_SIZE + 4].copy_from_slice(b"icon");
        exefs[EXEFS_ENTRY_SIZE + 8..EXEFS_ENTRY_SIZE + 12]
            .copy_from_slice(&(code.len() as u32).to_le_bytes());
        exefs[EXEFS_ENTRY_SIZE + 12..EXEFS_ENTRY_SIZE + 16]
            .copy_from_slice(&(icon.len() as u32).to_le_bytes());
        exefs[EXEFS_HEADER_SIZE..icon_offset].copy_from_slice(code);
        exefs[icon_offset..].copy_from_slice(icon);
        exefs
    }

    #[test]
    fn reads_icon_when_nocrypto() {
        let (header, exefs) = synth_header_and_exefs(b"hello-icon-bytes");
        let bytes = read_icon_section(&mut Cursor::new(&exefs), &header, 0).unwrap();
        assert_eq!(bytes, b"hello-icon-bytes");
    }

    #[test]
    fn missing_section_errors() {
        let (header, exefs) = synth_header_and_exefs(b"x");
        assert!(read_exefs_section(&mut Cursor::new(&exefs), &header, 0, b"banner").is_err());
    }

    #[test]
    fn reads_icon_when_encrypted() {
        let code = vec![0x41u8; 700];
        let icon = b"encrypted-icon-plaintext";
        let mut exefs = synth_code_then_icon_exefs(&code, icon);

        let bytes = make_ncch_header_bytes(0x000400000C123456);
        let mut header = NcchHeader::read(&mut Cursor::new(&bytes)).unwrap();
        header.flags[7] &= !NCCH_FLAGS7_NOCRYPTO;
        // Entry offsets are relative to the end of the ExeFS header;
        // the bound needs header + offset + size bytes of coverage.
        header.exefssize = (EXEFS_HEADER_SIZE + code.len() + icon.len())
            .div_ceil(CTR_MEDIA_UNIT_SIZE as usize) as u32;

        // Pre-encrypt with the same derived key and counter; CTR is
        // symmetric, so applying the keystream from offset 0 flips it
        // back to plaintext on read.
        let key_y = BigEndian::read_u128(header.signature[0..16].try_into().unwrap());
        let working_key = derive_ctr_key(CTR_KEYS_0[0], key_y);
        let ctr = get_ncch_aes_counter(&header, NcchSection::ExeFS);
        Aes128Ctr::new_from_slices(&working_key, &ctr)
            .unwrap()
            .apply_keystream(&mut exefs);

        let out = read_icon_section(&mut Cursor::new(&exefs), &header, 0).unwrap();
        assert_eq!(out, icon);
    }

    #[test]
    fn entry_overrunning_exefssize_errors() {
        let (mut header, exefs) = synth_header_and_exefs(b"icon-bytes");
        // One media unit covers only the header; the icon entry claims
        // 10 bytes past it. The buffer is long enough, so only the
        // exefssize bound can reject this.
        header.exefssize = 1;
        assert!(read_icon_section(&mut Cursor::new(&exefs), &header, 0).is_err());
    }
}
