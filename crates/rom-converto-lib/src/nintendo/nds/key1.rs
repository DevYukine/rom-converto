//! KEY1, the Blowfish variant the DS cartridge protocol and secure area use.
//!
//! Ported from devkitPro `ndstool` `source/encryption.cpp` (`lookup`,
//! `encrypt`, `decrypt`, `update_hashtable`, `init1`/`init2`), cross-read
//! against the gbatek "DS Cartridge Secure Area" and "KEY1 Encryption"
//! notes. It differs from stock Blowfish in the round function (`d + (c ^
//! (b + a))` rather than `((a + b) ^ c) + d`) and in the key schedule,
//! which folds a three-word key code derived from the header id code into
//! the table before expanding it.

use crate::nintendo::nds::embedded_keys::blowfish_table;

/// Words in the KEY1 key buffer: 18 P-array entries plus four 256-word S-boxes.
const KEYBUF_WORDS: usize = 0x412;

/// KEY1 key schedule derived from a cartridge id code.
pub struct Key1 {
    keybuf: [u32; KEYBUF_WORDS],
}

impl Key1 {
    /// Builds the key schedule for `idcode` at KEY1 `level` (1 to 3).
    ///
    /// `modulo` is the byte-wise wrap applied to the key code while folding
    /// it into the P-array; the secure area uses 8. Levels are cumulative:
    /// level 3 additionally doubles/halves the key code words before its
    /// final round, exactly as `ndstool`'s `init1` plus `init2` sequence.
    pub fn new(idcode: u32, level: u8, modulo: usize) -> Self {
        let mut keybuf = [0u32; KEYBUF_WORDS];
        for (word, chunk) in keybuf.iter_mut().zip(blowfish_table().as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*chunk);
        }

        let mut key = Key1 { keybuf };
        let mut keycode = [idcode, idcode >> 1, idcode << 1];
        if level >= 1 {
            key.apply_keycode(&mut keycode, modulo);
        }
        if level >= 2 {
            key.apply_keycode(&mut keycode, modulo);
        }
        if level >= 3 {
            keycode[1] <<= 1;
            keycode[2] >>= 1;
            key.apply_keycode(&mut keycode, modulo);
        }
        key
    }

    /// Encrypts one 64-bit block held as `[low_word, high_word]`.
    pub fn encrypt_block(&self, block: &mut [u32; 2]) {
        let mut a = block[1];
        let mut b = block[0];
        for &k in &self.keybuf[..16] {
            let c = k ^ a;
            a = b ^ self.lookup(c);
            b = c;
        }
        block[0] = a ^ self.keybuf[16];
        block[1] = b ^ self.keybuf[17];
    }

    /// Decrypts one 64-bit block held as `[low_word, high_word]`.
    pub fn decrypt_block(&self, block: &mut [u32; 2]) {
        let mut a = block[1];
        let mut b = block[0];
        for &k in self.keybuf[2..18].iter().rev() {
            let c = k ^ a;
            a = b ^ self.lookup(c);
            b = c;
        }
        block[1] = b ^ self.keybuf[0];
        block[0] = a ^ self.keybuf[1];
    }

    fn lookup(&self, v: u32) -> u32 {
        let a = self.keybuf[18 + ((v >> 24) & 0xFF) as usize];
        let b = self.keybuf[18 + 256 + ((v >> 16) & 0xFF) as usize];
        let c = self.keybuf[18 + 512 + ((v >> 8) & 0xFF) as usize];
        let d = self.keybuf[18 + 768 + (v & 0xFF) as usize];
        d.wrapping_add(c ^ b.wrapping_add(a))
    }

    /// One `init2` round: encrypt the key code with the current table, then
    /// fold it in and re-expand the whole buffer. `keycode` carries forward
    /// between rounds, so the levels are not independent.
    fn apply_keycode(&mut self, keycode: &mut [u32; 3], modulo: usize) {
        let mut block = [keycode[1], keycode[2]];
        self.encrypt_block(&mut block);
        keycode[1] = block[0];
        keycode[2] = block[1];

        let mut block = [keycode[0], keycode[1]];
        self.encrypt_block(&mut block);
        keycode[0] = block[0];
        keycode[1] = block[1];

        let mut bytes = [0u8; 12];
        for (dst, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(keycode.iter()) {
            *dst = word.to_le_bytes();
        }
        for (j, word) in self.keybuf[..18].iter_mut().enumerate() {
            let mut folded = 0u32;
            for i in 0..4 {
                folded = (folded << 8) | u32::from(bytes[(j * 4 + i) % modulo]);
            }
            *word ^= folded;
        }

        // The scratch block chains across both loops; ndstool never resets it.
        let mut scratch = [0u32; 2];
        for i in (0..18).step_by(2) {
            self.encrypt_block(&mut scratch);
            self.keybuf[i] = scratch[1];
            self.keybuf[i + 1] = scratch[0];
        }
        for i in (0..0x400).step_by(2) {
            self.encrypt_block(&mut scratch);
            self.keybuf[18 + i] = scratch[1];
            self.keybuf[18 + i + 1] = scratch[0];
        }
    }
}
