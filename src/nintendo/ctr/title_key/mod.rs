mod error;

use crate::nintendo::ctr::constants::{
    CTR_COMMON_KEYS, CTR_DEFAULT_TITLE_KEY_PASSWORD, CTR_TITLE_KEY_SECRET,
};
use crate::nintendo::ctr::title_key::error::{TitleKeyError, TitleKeyResult};
use aes::Aes128;
use block_padding::NoPadding;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use hex::{decode, encode};
use hmac::Hmac;
use log::debug;
use pbkdf2::pbkdf2;
use sha1::Sha1;

pub fn generate_key(mut title_id: &str, password: &str) -> TitleKeyResult<String> {
    // Strip optional "0x"
    if let Some(stripped) = title_id.strip_prefix("0x") {
        title_id = stripped;
    }

    let tid = &title_id[2..];

    // secret || trimmed-title-id → bytes
    let secret_hex = format!("{CTR_TITLE_KEY_SECRET}{tid}");
    let secret_bytes = decode(&secret_hex)?;

    // MD5(secret_bytes)
    let salt = md5::compute(&secret_bytes);

    // PBKDF2-HMAC-SHA1(password, salt, 20 iter) → 16-byte key
    let mut out = [0u8; 16];
    pbkdf2::<Hmac<Sha1>>(password.as_bytes(), salt.0.as_ref(), 20, &mut out)?;

    Ok(encode(out))
}

fn encrypt_title_key(
    title_id: &str,
    title_key_hex: &str,
    ckey_hex: &str,
) -> TitleKeyResult<String> {
    // For the IV only strip "0x" – don't drop any more hex digits
    let tid = title_id.strip_prefix("0x").unwrap_or(title_id);

    // Build IV: full 16-byte title ID (hex) + pad with zeros to 32 hex chars
    let iv_hex = format!("{tid:0<32}");
    let iv = decode(&iv_hex)?;
    let key = decode(ckey_hex)?;
    let data = decode(title_key_hex)?;

    // Initialize AES-CBC encryptor
    let cipher = cbc::Encryptor::<Aes128>::new_from_slices(&key, &iv)?;

    // Prepare buffer: data_len + one block (16 bytes) for padding space
    let data_len = data.len();
    let mut buf = data;
    buf.resize(data_len + 16, 0);

    // Encrypt in-place, using NoPadding
    let ciphertext = cipher
        .encrypt_padded_mut::<NoPadding>(&mut buf, data_len)
        .map_err(|err| TitleKeyError::PadError(err.to_string()))?;

    Ok(encode(ciphertext))
}

pub fn generate_title_key(title_id: &str, password: Option<String>) -> TitleKeyResult<String> {
    let password = password.unwrap_or_else(|| CTR_DEFAULT_TITLE_KEY_PASSWORD.to_string());
    let common_key = CTR_COMMON_KEYS[0];

    debug!("Using title id: {title_id}");
    debug!("Using common key: {common_key}");
    debug!("Using password: {password}");

    let un_encrypted = generate_key(title_id, &password)?;
    debug!("Generated unencrypted key: {un_encrypted}");

    let encrypted = encrypt_title_key(title_id, &un_encrypted, common_key)?;
    debug!("Generated encrypted key: {encrypted}");

    Ok(encrypted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_title_key() {
        let title_id = "0004008c0f70cd00";
        let key = generate_title_key(title_id, None).unwrap();
        assert_eq!(key.len(), 32); // Should be 16 bytes in hex
        assert_eq!(key, "3c7faeff5b1d784d25011149f33f50a7");
    }

    #[test]
    fn test_generate_key() {
        let title_id = "0x00040000001adc00";
        let password = "mypass";
        let key = generate_key(title_id, password).unwrap();
        assert_eq!(key, "3dbe05484b3c5033c2cefd81e27b0d95");
    }
}
