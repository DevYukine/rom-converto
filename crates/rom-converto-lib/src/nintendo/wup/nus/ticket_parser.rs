//! High-level ticket loading helpers.
//!
//! These wrap [`crate::nintendo::wup::models::ticket::WupTicket`]
//! and [`crate::nintendo::wup::crypto::decrypt_title_key`] with the
//! operations the NUS pipeline actually wants: read a `title.tik`
//! file and immediately produce a decrypted title key.

use std::path::Path;

use crate::nintendo::wup::crypto::decrypt_title_key;
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::models::WupTicket;

/// Decrypted title key used to encrypt all content files for a
/// single Wii U title. The raw bytes are carried by value because
/// the key is short-lived: it's derived from the ticket, used for
/// content decryption, and dropped when the title finishes
/// processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleKey(pub [u8; 16]);

/// Parse an in-memory ticket blob and return the decrypted title
/// key alongside the parsed ticket metadata. Personalised tickets
/// are rejected in v1 because depersonalisation requires the
/// console's private key, which we do not have.
pub fn parse_ticket_bytes(bytes: &[u8]) -> WupResult<(WupTicket, TitleKey)> {
    let ticket = WupTicket::parse(bytes)?;
    if ticket.is_personalized() {
        return Err(WupError::TitleKeyDecryptFailed);
    }
    let key = decrypt_title_key(&ticket.encrypted_title_key, ticket.title_id)?;
    Ok((ticket, TitleKey(key)))
}

/// Convenience: read a ticket file from disk and run
/// [`parse_ticket_bytes`] on its contents.
pub fn read_ticket_file(path: &Path) -> WupResult<(WupTicket, TitleKey)> {
    let bytes = std::fs::read(path)?;
    parse_ticket_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::common_keys::WII_U_COMMON_KEY;
    use crate::nintendo::wup::models::ticket::{WUP_TICKET_BASE_SIZE, WUP_TICKET_FORMAT_V1};
    use aes::{
        Aes128,
        cipher::{BlockEncryptMut, KeyIvInit},
    };
    use block_padding::NoPadding;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    /// Build a fake ticket binary whose encrypted title key matches
    /// the plaintext `plain_key` when decrypted with the Wii U
    /// common key under the `title_id`-derived IV.
    fn make_ticket_blob(title_id: u64, plain_key: &[u8; 16], device_id: u32) -> Vec<u8> {
        let mut iv = [0u8; 16];
        iv[0..8].copy_from_slice(&title_id.to_be_bytes());
        let mut encrypted = *plain_key;
        Aes128CbcEnc::new_from_slices(&WII_U_COMMON_KEY, &iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(&mut encrypted, 16)
            .unwrap();

        let mut bytes = vec![0u8; WUP_TICKET_BASE_SIZE];
        bytes[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
        bytes[0x1BC] = WUP_TICKET_FORMAT_V1;
        bytes[0x1BF..0x1CF].copy_from_slice(&encrypted);
        bytes[0x1D8..0x1DC].copy_from_slice(&device_id.to_be_bytes());
        bytes[0x1DC..0x1E4].copy_from_slice(&title_id.to_be_bytes());
        bytes
    }

    #[test]
    fn round_trip_decrypts_plain_title_key() {
        let title_id = 0x0005_000E_1234_5678u64;
        let plain_key = [0xAAu8; 16];
        let bytes = make_ticket_blob(title_id, &plain_key, 0);
        let (ticket, key) = parse_ticket_bytes(&bytes).unwrap();
        assert_eq!(ticket.title_id, title_id);
        assert_eq!(key.0, plain_key);
    }

    #[test]
    fn rejects_personalised_ticket() {
        let bytes = make_ticket_blob(0x0005_000E_1010_2000, &[0u8; 16], 0xDEAD_BEEF);
        let err = parse_ticket_bytes(&bytes);
        assert!(matches!(err, Err(WupError::TitleKeyDecryptFailed)));
    }

    #[test]
    fn reads_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("title.tik");
        let plain_key = [0x42u8; 16];
        let bytes = make_ticket_blob(0x0005_000E_0000_0001, &plain_key, 0);
        std::fs::write(&path, &bytes).unwrap();
        let (_, key) = read_ticket_file(&path).unwrap();
        assert_eq!(key.0, plain_key);
    }
}
