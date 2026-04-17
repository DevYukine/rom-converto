//! Wii U title key derivation for titles that ship without a ticket.
//!
//! Re-computes the plaintext title key from the title id alone using
//! the PBKDF2-HMAC-SHA1 scheme Nintendo's CDN used to mint tickets.
//! The encrypted key then goes into a synthesized ticket under the
//! Wii U common key (see [`super::ticket_synth`]).
//!
//! Constants are duplicated from the ctr title_key module on purpose.
//! Both platforms happen to share the same values today, but the two
//! code paths can diverge without affecting each other.

use hmac::Hmac;
use pbkdf2::pbkdf2;
use sha1::Sha1;

const SECRET_HEX: &str = "fd040105060b111c2d49";
const PASSWORD: &[u8] = b"mypass";
const PBKDF2_ITERATIONS: u32 = 20;

/// Derive the plaintext title key for the given Wii U title id.
///
/// Format the 64 bit title id as 16 lowercase hex digits, drop the
/// first two, concatenate to [`SECRET_HEX`], hex-decode, MD5 to form
/// the salt, then run PBKDF2-HMAC-SHA1 with [`PASSWORD`] for
/// [`PBKDF2_ITERATIONS`] rounds to produce the 16 byte key.
pub fn derive_title_key(title_id: u64) -> [u8; 16] {
    let full_hex = format!("{title_id:016x}");
    let stripped = &full_hex[2..];
    let mut combined_hex = String::with_capacity(SECRET_HEX.len() + stripped.len());
    combined_hex.push_str(SECRET_HEX);
    combined_hex.push_str(stripped);

    // SECRET_HEX is fixed and stripped is 14 lowercase hex digits, so
    // every byte below is valid hex.
    let mut combined_bytes = Vec::with_capacity(combined_hex.len() / 2);
    let hex_bytes = combined_hex.as_bytes();
    let mut i = 0;
    while i < hex_bytes.len() {
        let hi = hex_nibble(hex_bytes[i]);
        let lo = hex_nibble(hex_bytes[i + 1]);
        combined_bytes.push((hi << 4) | lo);
        i += 2;
    }

    let salt = md5::compute(&combined_bytes);

    let mut out = [0u8; 16];
    // pbkdf2 only fails when the output length is zero.
    pbkdf2::<Hmac<Sha1>>(PASSWORD, salt.0.as_ref(), PBKDF2_ITERATIONS, &mut out)
        .expect("16 byte PBKDF2 output is never an invalid length");
    out
}

#[inline]
fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!("derive_title_key only feeds valid hex bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed known-answer vector: title id 0x0005000e10101e00 derives
    // to the byte sequence below. Any change breaks CDN-minted titles.
    #[test]
    fn matches_known_title_key_vector() {
        let title_id = 0x0005_000E_1010_1E00;
        let key = derive_title_key(title_id);
        let expected = [
            0x52, 0x86, 0x80, 0x7B, 0x77, 0x51, 0xD5, 0x6A, 0x7F, 0x02, 0xEA, 0x6D, 0xA6, 0xD1,
            0xBA, 0x10,
        ];
        assert_eq!(key, expected);
    }

    #[test]
    fn deterministic_across_calls() {
        let tid = 0x0005_000E_1010_1E00;
        assert_eq!(derive_title_key(tid), derive_title_key(tid));
    }

    #[test]
    fn different_titles_produce_different_keys() {
        let a = derive_title_key(0x0005_000E_1010_1E00);
        let b = derive_title_key(0x0005_000E_1010_1E01);
        assert_ne!(a, b);
    }

    #[test]
    fn base_title_key_differs_from_update_title_key() {
        // Title type high-half (0x00050000 vs 0x0005000E) flows into
        // PBKDF2 so swapping only that produces a different key.
        let base = derive_title_key(0x0005_0000_1010_1E00);
        let update = derive_title_key(0x0005_000E_1010_1E00);
        assert_ne!(base, update);
    }
}
