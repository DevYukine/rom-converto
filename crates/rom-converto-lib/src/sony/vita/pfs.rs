//! PFS gamedata decryption: the second encryption layer a PS Vita package
//! keeps under its AES-CTR package layer, keyed by the license klicensee.
//!
//! Ported from psvpfstools. Only the read path for gamedata images
//! (`sce_pfs/unicv.db`, `SCEIFTBL` version 2 and up) is covered, which is
//! what retail packages carry. Integrity check values are not verified.
//!
//! The Vita is end-of-life and these keys are long published, so they are
//! embedded here the way the package keys are.

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, BlockModeDecrypt, KeyInit, KeyIvInit};
use anyhow::{Context, Result, anyhow, bail};
use block_padding::NoPadding;
use hex_literal::hex;
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// Page size `sce_pfs/unicv.db` indexes its file tables by.
const UNICV_PAGE: usize = 0x400;

const TABLE_MAGIC: &[u8; 8] = b"SCEIFTBL";
const TABLE_VERSION_OFFSET: usize = 0x08;
const TABLE_SECTOR_SIZE_OFFSET: usize = 0x18;
const TABLE_SEED_OFFSET: usize = 0x34;
const TABLE_SEED_LEN: usize = 0x14;
const TABLE_HEADER_LEN: usize = TABLE_SEED_OFFSET + TABLE_SEED_LEN;

/// HMAC-SHA1 key the tweak key is derived under.
const HMAC_KEY0: [u8; 20] = hex!("E462258B1F3121560745DB62B1436723D2BF80FE");

/// Key behind the console's F00D service for PFS key slot 0: the sector
/// key is the klicensee ECB-decrypted under it.
const CONTRACT_KEY0: [u8; 16] = hex!("E12213B48016B0E99AB81F8EC02AD4A2");

/// Decrypts `data`, the PFS ciphertext of the package file `path`, in place.
///
/// `pflist` is the package's `sce_pfs/pflist` text and `unicv` its
/// `sce_pfs/unicv.db`; together they carry the page id and per-file seed the
/// key derivation needs. Entries marked `nenc` or `npfs` are already
/// plaintext and are left untouched.
///
/// # Errors
/// Returns an error when `path` is not listed, the databases are malformed,
/// or the file table predates the seed-based key derivation.
pub fn decrypt_file(
    klicensee: &[u8; 16],
    pflist: &str,
    path: &str,
    unicv: &[u8],
    data: &mut [u8],
) -> Result<()> {
    let (flags, page) = pflist_entry(pflist, path)?;
    if flags == "nenc" || flags == "npfs" {
        return Ok(());
    }
    if flags == "dir" || flags == "aciddir" {
        bail!("vita pfs: {path} is a directory");
    }

    let table = (page as usize)
        .checked_mul(UNICV_PAGE)
        .and_then(|start| unicv.get(start..start.checked_add(TABLE_HEADER_LEN)?))
        .ok_or_else(|| anyhow!("vita pfs: unicv.db has no page {page} for {path}"))?;
    if &table[..8] != TABLE_MAGIC {
        bail!("vita pfs: unicv.db page {page} is not a file table");
    }
    let version = le_u32(table, TABLE_VERSION_OFFSET);
    if version < 2 {
        bail!("vita pfs: unicv.db version {version} predates the per-file seed");
    }
    let sector_size = le_u32(table, TABLE_SECTOR_SIZE_OFFSET) as usize;
    if sector_size == 0 || !sector_size.is_multiple_of(16) {
        bail!("vita pfs: unicv.db page {page} has file sector size {sector_size}");
    }

    let seed = &table[TABLE_SEED_OFFSET..TABLE_SEED_OFFSET + TABLE_SEED_LEN];
    let keys = SectorKeys::derive(klicensee, seed);
    for (index, sector) in data.chunks_mut(sector_size).enumerate() {
        let offset = (sector_size as u64) * index as u64;
        keys.decrypt_sector(offset, sector)?;
    }
    Ok(())
}

/// The pair of keys every sector of one file is crypted with.
struct SectorKeys {
    /// Sector key: the klicensee as the F00D service hands it back.
    sector: [u8; 16],
    /// Mask XORed into the sector offset to make the CBC IV.
    tweak: [u8; 16],
}

impl SectorKeys {
    fn derive(klicensee: &[u8; 16], seed: &[u8]) -> Self {
        let mut mac =
            Hmac::<Sha1>::new_from_slice(&HMAC_KEY0).expect("hmac-sha1 takes any key length");
        mac.update(seed);
        let mut tweak = [0u8; 16];
        tweak.copy_from_slice(&mac.finalize().into_bytes()[..16]);

        let mut sector = *klicensee;
        Aes128::new(&CONTRACT_KEY0.into()).decrypt_block((&mut sector).into());

        Self { sector, tweak }
    }

    /// AES-CBC over whole blocks with the sector tweak as IV, then
    /// ciphertext stealing for a trailing partial block: those bytes are
    /// XORed with the ECB encryption of the IV as it stands after the CBC
    /// pass, which is the last ciphertext block (or the tweak itself when
    /// the sector holds less than one block).
    fn decrypt_sector(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&offset.to_le_bytes());
        for (b, mask) in iv.iter_mut().zip(self.tweak) {
            *b ^= mask;
        }

        let whole = buf.len() & !0xF;
        let mut trailing_iv = iv;
        if whole > 0 {
            trailing_iv.copy_from_slice(&buf[whole - 16..whole]);
            cbc::Decryptor::<Aes128>::new_from_slices(&self.sector, &iv)
                .context("vita pfs: bad cbc key or iv length")?
                .decrypt_padded::<NoPadding>(&mut buf[..whole])
                .map_err(|e| anyhow!("vita pfs: cbc decrypt failed: {e}"))?;
        }
        if whole < buf.len() {
            let mut keystream = trailing_iv;
            Aes128::new(&self.sector.into()).encrypt_block((&mut keystream).into());
            for (b, k) in buf[whole..].iter_mut().zip(keystream) {
                *b ^= k;
            }
        }
        Ok(())
    }
}

/// Finds `path` in the tab-separated `pflist` table, returning its flag
/// word and the `unicv.db` page that holds its file table.
fn pflist_entry<'a>(pflist: &'a str, path: &str) -> Result<(&'a str, u32)> {
    for line in pflist.lines() {
        let mut fields = line.split('\t');
        if fields.next() != Some(path) {
            continue;
        }
        // Fields are: path, access type, flag word, page id, size, digest.
        let flags = fields.nth(1).unwrap_or_default();
        let id = fields.next().unwrap_or_default();
        let page = u32::from_str_radix(id.trim_start_matches("0x"), 16)
            .with_context(|| format!("vita pfs: {path} has page id {id:?}"))?;
        return Ok((flags, page));
    }
    bail!("vita pfs: {path} is not listed in pflist")
}

fn le_u32(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(d[off..off + 4].try_into().expect("4-byte slice"))
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::*;
    use aes::cipher::BlockModeEncrypt;

    /// Encrypts `data` the way a packaging tool would, so tests can build a
    /// package [`decrypt_file`] has to undo.
    pub fn encrypt_file(klicensee: &[u8; 16], seed: &[u8], sector_size: usize, data: &mut [u8]) {
        let keys = SectorKeys::derive(klicensee, seed);
        for (index, sector) in data.chunks_mut(sector_size).enumerate() {
            let mut iv = [0u8; 16];
            iv[..8].copy_from_slice(&((sector_size as u64) * index as u64).to_le_bytes());
            for (b, mask) in iv.iter_mut().zip(keys.tweak) {
                *b ^= mask;
            }

            let whole = sector.len() & !0xF;
            let mut trailing_iv = iv;
            if whole > 0 {
                cbc::Encryptor::<Aes128>::new_from_slices(&keys.sector, &iv)
                    .expect("cbc key and iv length")
                    .encrypt_padded::<NoPadding>(&mut sector[..whole], whole)
                    .expect("no padding needed");
                trailing_iv.copy_from_slice(&sector[whole - 16..whole]);
            }
            if whole < sector.len() {
                let mut keystream = trailing_iv;
                Aes128::new(&keys.sector.into()).encrypt_block((&mut keystream).into());
                for (b, k) in sector[whole..].iter_mut().zip(keystream) {
                    *b ^= k;
                }
            }
        }
    }

    /// Builds a `sce_pfs/unicv.db` holding one `SCEIFTBL` file table per
    /// `(page, seed)` pair.
    pub fn build_unicv(sector_size: u32, tables: &[(u32, [u8; TABLE_SEED_LEN])]) -> Vec<u8> {
        let pages = tables.iter().map(|(p, _)| *p).max().unwrap_or(0) + 1;
        let mut out = vec![0u8; pages as usize * UNICV_PAGE];
        out[..8].copy_from_slice(b"SCEIRODB");
        for (page, seed) in tables {
            let at = *page as usize * UNICV_PAGE;
            out[at..at + 8].copy_from_slice(TABLE_MAGIC);
            out[at + TABLE_VERSION_OFFSET..at + TABLE_VERSION_OFFSET + 4]
                .copy_from_slice(&2u32.to_le_bytes());
            out[at + TABLE_SECTOR_SIZE_OFFSET..at + TABLE_SECTOR_SIZE_OFFSET + 4]
                .copy_from_slice(&sector_size.to_le_bytes());
            out[at + TABLE_SEED_OFFSET..at + TABLE_SEED_OFFSET + TABLE_SEED_LEN]
                .copy_from_slice(seed);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{build_unicv, encrypt_file};
    use super::*;

    const KLICENSEE: [u8; 16] = hex!("000102030405060708090A0B0C0D0E0F");
    const SEED: [u8; 20] = hex!("6B424E9B3F566804D1FB2B7F64335981BE9093BC");
    const SECTOR: usize = 0x40;

    fn pflist() -> String {
        [
            "# List of PFS files/dirs (Don't edit this file).",
            "#\tpfs_mode\t10",
            "sce_sys\tsys\tdir\t0x00000001\t0\t00",
            "sce_sys/param.sfo\tsys\tnenc\t0x00000002\t8\t00",
            "sce_sys/icon0.png\tsys\t\t0x00000003\t100\t00",
        ]
        .join("\n")
    }

    /// The tail of a sector goes through ciphertext stealing, so a length
    /// that is neither block nor sector aligned exercises both paths.
    #[test]
    fn round_trips_a_multi_sector_file() {
        let unicv = build_unicv(SECTOR as u32, &[(3, SEED)]);
        let plain: Vec<u8> = (0..0xA5u32).map(|i| i as u8).collect();

        let mut data = plain.clone();
        encrypt_file(&KLICENSEE, &SEED, SECTOR, &mut data);
        assert_ne!(data, plain);

        decrypt_file(
            &KLICENSEE,
            &pflist(),
            "sce_sys/icon0.png",
            &unicv,
            &mut data,
        )
        .unwrap();
        assert_eq!(data, plain);
    }

    #[test]
    fn leaves_unencrypted_entries_alone() {
        let unicv = build_unicv(SECTOR as u32, &[(3, SEED)]);
        let mut data = vec![0x5Au8; 8];
        decrypt_file(
            &KLICENSEE,
            &pflist(),
            "sce_sys/param.sfo",
            &unicv,
            &mut data,
        )
        .unwrap();
        assert_eq!(data, vec![0x5Au8; 8]);
    }

    #[test]
    fn rejects_a_path_the_pflist_does_not_list() {
        let unicv = build_unicv(SECTOR as u32, &[(3, SEED)]);
        let err = decrypt_file(&KLICENSEE, &pflist(), "sce_sys/pic0.png", &unicv, &mut [])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not listed"), "{err}");
    }

    #[test]
    fn rejects_a_page_without_a_file_table() {
        let unicv = build_unicv(SECTOR as u32, &[(9, SEED)]);
        let err = decrypt_file(
            &KLICENSEE,
            &pflist(),
            "sce_sys/icon0.png",
            &unicv,
            &mut [0u8; 16],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a file table"), "{err}");
    }
}
