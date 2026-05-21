//! Synthetic NCAs and `KeySet`s for unit tests. Lets crypto round-trip
//! tests run without real prod.keys or real game files.

use std::collections::HashMap;

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};

use crate::nintendo::nx::crypto::derive::{KEY_AREA_KEY_COUNT, KEY_AREA_KEY_SIZE, KEY_AREA_TOTAL};
use crate::nintendo::nx::keys::{KeyAreaKind, KeySet};

pub const TEST_HEADER_KEY: [u8; 32] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
];

pub const TEST_KAK_APPLICATION_00: [u8; 16] = [
    0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
];

pub const TEST_BODY_KEY: [u8; 16] = [
    0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
];

pub fn synthetic_keyset() -> KeySet {
    let mut kak = HashMap::new();
    kak.insert((KeyAreaKind::Application, 0), TEST_KAK_APPLICATION_00);
    KeySet {
        header_key: Some(TEST_HEADER_KEY),
        key_area_keys: kak,
        ..KeySet::default()
    }
}

pub fn encrypt_key_area_block(plain_keys: [[u8; 16]; KEY_AREA_KEY_COUNT]) -> [u8; KEY_AREA_TOTAL] {
    let cipher = Aes128::new_from_slice(&TEST_KAK_APPLICATION_00).unwrap();
    let mut out = [0u8; KEY_AREA_TOTAL];
    for (i, plain) in plain_keys.iter().enumerate() {
        let mut block = GenericArray::clone_from_slice(plain);
        cipher.encrypt_block(&mut block);
        out[i * KEY_AREA_KEY_SIZE..(i + 1) * KEY_AREA_KEY_SIZE].copy_from_slice(block.as_slice());
    }
    out
}
