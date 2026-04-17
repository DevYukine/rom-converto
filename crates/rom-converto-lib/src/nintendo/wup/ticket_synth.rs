//! Build in-memory Wii U tickets for titles that ship without one.
//!
//! The blob is only used internally. The NUS loader parses it through
//! the same [`WupTicket::parse`] path as real tickets and the derived
//! title key flows into content decryption. Nothing is written to
//! disk or bundled into the produced `.wua`, so RSA signing is
//! skipped (the parser does not verify) and only the parser-read
//! fields are populated.
//!
//! Pair with [`super::title_key_derive::derive_title_key`] for titles
//! that ship without a `cetk.*` file.

use crate::nintendo::wup::common_keys::WII_U_COMMON_KEY;
use crate::nintendo::wup::models::ticket::{WUP_TICKET_BASE_SIZE, WUP_TICKET_FORMAT_V1};

use aes::cipher::{BlockEncryptMut, KeyIvInit};
use block_padding::NoPadding;

// Offsets are duplicated from models::ticket instead of imported to
// keep that module's surface minimal.
const OFFSET_SIGNATURE_TYPE: usize = 0x000;
const OFFSET_TICKET_FORMAT_VERSION: usize = 0x1BC;
const OFFSET_ENCRYPTED_TITLE_KEY: usize = 0x1BF;
const OFFSET_TICKET_ID: usize = 0x1D0;
const OFFSET_DEVICE_ID: usize = 0x1D8;
const OFFSET_TITLE_ID: usize = 0x1DC;
const OFFSET_TITLE_VERSION: usize = 0x1E6;

const SIG_TYPE_RSA2048_SHA256: u32 = 0x0001_0004;

/// Build a byte-valid Wii U ticket for the given title id, version,
/// and plaintext title key. The key is AES-CBC encrypted under
/// [`WII_U_COMMON_KEY`] with IV = title_id big-endian followed by
/// 8 zero bytes, matching the shape of real tickets.
pub fn synthesize_wup_ticket(
    title_id: u64,
    title_version: u16,
    plain_title_key: &[u8; 16],
) -> Vec<u8> {
    let mut ticket = vec![0u8; WUP_TICKET_BASE_SIZE];

    ticket[OFFSET_SIGNATURE_TYPE..OFFSET_SIGNATURE_TYPE + 4]
        .copy_from_slice(&SIG_TYPE_RSA2048_SHA256.to_be_bytes());

    // Signature body at [0x04..0x140] stays zero. The parser skips
    // verification; a third-party verifier would correctly reject this.

    ticket[OFFSET_TICKET_FORMAT_VERSION] = WUP_TICKET_FORMAT_V1;

    // IV = title_id BE in the low 8 bytes, zeros in the high 8.
    let mut iv = [0u8; 16];
    iv[0..8].copy_from_slice(&title_id.to_be_bytes());
    let mut ciphertext = *plain_title_key;
    cbc::Encryptor::<aes::Aes128>::new_from_slices(&WII_U_COMMON_KEY, &iv)
        .expect("common key and IV are exactly 16 bytes")
        .encrypt_padded_mut::<NoPadding>(&mut ciphertext, plain_title_key.len())
        .expect("16 byte plaintext always encrypts cleanly with NoPadding");

    ticket[OFFSET_ENCRYPTED_TITLE_KEY..OFFSET_ENCRYPTED_TITLE_KEY + 16]
        .copy_from_slice(&ciphertext);

    ticket[OFFSET_TICKET_ID..OFFSET_TICKET_ID + 8].copy_from_slice(&0u64.to_be_bytes());

    // device_id = 0 flags the ticket non-personalised. The parser
    // rejects personalised tickets since we cannot depersonalise
    // without the console private key.
    ticket[OFFSET_DEVICE_ID..OFFSET_DEVICE_ID + 4].copy_from_slice(&0u32.to_be_bytes());

    ticket[OFFSET_TITLE_ID..OFFSET_TITLE_ID + 8].copy_from_slice(&title_id.to_be_bytes());

    ticket[OFFSET_TITLE_VERSION..OFFSET_TITLE_VERSION + 2]
        .copy_from_slice(&title_version.to_be_bytes());

    ticket
}

// Test helper: round-trip a synthesized ticket through the real
// parser and recover the plaintext title key.
#[cfg(test)]
pub(crate) fn decrypt_synth_title_key(
    synth_ticket: &[u8],
) -> crate::nintendo::wup::error::WupResult<[u8; 16]> {
    let ticket = crate::nintendo::wup::models::ticket::WupTicket::parse(synth_ticket)?;
    let mut iv = [0u8; 16];
    iv[0..8].copy_from_slice(&ticket.title_id.to_be_bytes());
    let mut key = ticket.encrypted_title_key;
    crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place(&WII_U_COMMON_KEY, &iv, &mut key)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::nus::ticket_parser::parse_ticket_bytes;
    use crate::nintendo::wup::title_key_derive::derive_title_key;

    #[test]
    fn synthesized_ticket_is_base_size() {
        let bytes = synthesize_wup_ticket(0, 0, &[0u8; 16]);
        assert_eq!(bytes.len(), WUP_TICKET_BASE_SIZE);
    }

    #[test]
    fn round_trips_through_real_parser() {
        let title_id = 0x0005_000E_1010_2000;
        let title_version: u16 = 32;
        let plain_key = [0xABu8; 16];
        let bytes = synthesize_wup_ticket(title_id, title_version, &plain_key);

        let (ticket, title_key) = parse_ticket_bytes(&bytes).unwrap();
        assert_eq!(ticket.title_id, title_id);
        assert_eq!(ticket.title_version, title_version);
        assert!(!ticket.is_personalized());
        assert_eq!(title_key.0, plain_key);
    }

    #[test]
    fn pairs_with_derive_for_known_vector() {
        // Derive + synth + parse must recover the same fixed key that
        // title_key_derive::tests pins down. Covers derive and synth
        // in one end-to-end shot.
        let title_id = 0x0005_000E_1010_1E00;
        let title_version: u16 = 80;
        let derived = derive_title_key(title_id);
        let bytes = synthesize_wup_ticket(title_id, title_version, &derived);

        let (ticket, title_key) = parse_ticket_bytes(&bytes).unwrap();
        assert_eq!(ticket.title_id, title_id);
        assert_eq!(ticket.title_version, title_version);
        assert_eq!(
            title_key.0,
            [
                0x52, 0x86, 0x80, 0x7B, 0x77, 0x51, 0xD5, 0x6A, 0x7F, 0x02, 0xEA, 0x6D, 0xA6, 0xD1,
                0xBA, 0x10,
            ]
        );
    }

    #[test]
    fn decrypt_synth_helper_matches_parser() {
        let title_id = 0x0005_000E_1010_2000;
        let plain_key = [0x33u8; 16];
        let bytes = synthesize_wup_ticket(title_id, 1, &plain_key);
        let recovered = decrypt_synth_title_key(&bytes).unwrap();
        assert_eq!(recovered, plain_key);
    }
}
