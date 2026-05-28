//! Sync, in-memory ExeFS section reader.
//!
//! [`crate::nintendo::ctr::decrypt::cia`] decrypts NCCH ExeFS sections
//! while streaming the file to disk. The `info` extractor needs the
//! same decryption logic but for a single named entry (typically
//! `icon`, holding the SMDH) returned as a `Vec<u8>` it can parse
//! in-process. This module shares the same key-derivation helpers and
//! AES-CTR machinery; it does not duplicate any crypto code.
//!
//! Seed crypto is not supported here: titles that require it must be
//! decrypted to disk first via the `decrypt` command. The info path
//! reports that as a clean error rather than fetching seeds over HTTP.

use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use anyhow::{Result, anyhow};
use binrw::BinRead;
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use std::io::Cursor;

use crate::nintendo::ctr::constants::{
    CTR_KEYS_0, CTR_KEYS_1, EXEFS_ENTRY_SIZE, EXEFS_HEADER_SIZE, EXEFS_MAX_FILE_ENTRIES,
    EXEFS_SECTION_ICON, NCCH_FLAGS7_FIXED_KEY, NCCH_FLAGS7_NOCRYPTO, NCCH_FLAGS7_SEED_CRYPTO,
};
use crate::nintendo::ctr::decrypt::cia::{derive_ctr_key, get_ncch_aes_counter};
use crate::nintendo::ctr::decrypt::model::NcchSection;
use crate::nintendo::ctr::models::exe_fs_header::ExeFSHeader;
use crate::nintendo::ctr::models::ncch_header::NcchHeader;

type Aes128Ctr = ctr::Ctr128BE<Aes128>;

fn fixed_key(fixed_crypto: u8) -> Option<[u8; 16]> {
    (fixed_crypto != 0).then(|| u128::to_be_bytes(CTR_KEYS_1[(fixed_crypto as usize) - 1]))
}

fn decrypt_exefs_with_base(base_key: &[u8; 16], ctr: &[u8; 16], exefs: &mut [u8]) -> Result<()> {
    Aes128Ctr::new_from_slices(base_key, ctr)
        .map_err(|e| anyhow!("aes ctr init: {}", e))?
        .apply_keystream(exefs);
    Ok(())
}

/// Decrypt the named ExeFS section out of `exefs_encrypted` and return
/// just that section's plaintext bytes.
///
/// `header` is the NCCH header for the partition. `exefs_encrypted` is
/// the encrypted ExeFS region (`exefssize * media_unit` bytes) read
/// from the source file at `exefsoffset * media_unit`.
pub fn read_exefs_section(
    header: &NcchHeader,
    exefs_encrypted: &[u8],
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

    let mut decrypted = exefs_encrypted.to_vec();
    if !nocrypto {
        decrypt_exefs_with_base(&working_key, &ctr, &mut decrypted)?;
    }

    // For sections like `icon` / `banner` we never want the extra-crypto
    // variant per the canonical decrypt path; the base key is correct.
    // For other sections we would re-decrypt with the extra key, but the
    // info path only reads the icon today.

    let (offset, size) = find_exefs_entry(&decrypted, section_name)
        .ok_or_else(|| anyhow!("ExeFS section {:?} not found", short_name(section_name)))?;

    let start = EXEFS_HEADER_SIZE + offset;
    let end = start + size;
    if end > decrypted.len() {
        return Err(anyhow!("ExeFS section overruns buffer"));
    }
    Ok(decrypted[start..end].to_vec())
}

pub fn read_icon_section(header: &NcchHeader, exefs_encrypted: &[u8]) -> Result<Vec<u8>> {
    read_exefs_section(header, exefs_encrypted, &EXEFS_SECTION_ICON)
}

fn find_exefs_entry(decrypted_exefs: &[u8], name: &[u8]) -> Option<(usize, usize)> {
    for i in 0..EXEFS_MAX_FILE_ENTRIES {
        let off = i * EXEFS_ENTRY_SIZE;
        if off + EXEFS_ENTRY_SIZE > decrypted_exefs.len() {
            break;
        }
        let entry = ExeFSHeader::read(&mut Cursor::new(
            &decrypted_exefs[off..off + EXEFS_ENTRY_SIZE],
        ))
        .ok()?;
        let entry_name = trim_zero(&entry.file_name);
        if entry_name == name {
            let offset = LittleEndian::read_u32(&entry.file_offset) as usize;
            let size = LittleEndian::read_u32(&entry.file_size) as usize;
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
        let header = NcchHeader::read(&mut Cursor::new(&bytes)).unwrap();

        let mut exefs = vec![0u8; EXEFS_HEADER_SIZE + plaintext_icon.len()];
        exefs[0..4].copy_from_slice(b"icon");
        exefs[8..12].copy_from_slice(&(0u32).to_le_bytes());
        exefs[12..16].copy_from_slice(&(plaintext_icon.len() as u32).to_le_bytes());
        exefs[EXEFS_HEADER_SIZE..EXEFS_HEADER_SIZE + plaintext_icon.len()]
            .copy_from_slice(plaintext_icon);

        (header, exefs)
    }

    #[test]
    fn reads_icon_when_nocrypto() {
        let (header, exefs) = synth_header_and_exefs(b"hello-icon-bytes");
        let bytes = read_icon_section(&header, &exefs).unwrap();
        assert_eq!(bytes, b"hello-icon-bytes");
    }

    #[test]
    fn missing_section_errors() {
        let (header, exefs) = synth_header_and_exefs(b"x");
        assert!(read_exefs_section(&header, &exefs, b"banner").is_err());
    }
}
