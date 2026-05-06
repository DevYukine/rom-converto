//! Streaming re-encryption sink for NCZ -> NCA.
//!
//! `ReencryptWriter` is a `Write` adapter that takes plaintext NCA
//! payload bytes (whatever the zstd decoder happens to emit per call)
//! and forwards them to the inner writer with the per-section AES-CTR
//! keystream applied. Section boundaries are tracked by the running
//! `position_in_nca` cursor; each section reuses one stream-cipher
//! instance so partial-block writes inside the section land at the
//! correct keystream offset without re-deriving state.
//!
//! Sections that don't cover the current position pass bytes through
//! unchanged, matching how nsz writes the inter-section gap bytes
//! verbatim.

use std::io::{self, Write};

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;

use crate::nintendo::nx::constants::{
    ENC_AES_CTR, ENC_AES_CTR_EX, ENC_AES_CTR_EX_SKIP_LAYER_HASH, ENC_AES_CTR_SKIP_LAYER_HASH,
    ENC_NONE,
};
use crate::nintendo::nx::error::NxError;
use crate::nintendo::nx::ncz::header::NczSectionEntry;

type AesCtr = Ctr128BE<Aes128>;

pub struct ReencryptWriter<'a, W: Write> {
    inner: W,
    sections: &'a [NczSectionEntry],
    position_in_nca: u64,
    current: Option<CurrentSection>,
    scratch: Vec<u8>,
}

struct CurrentSection {
    index: usize,
    cipher: AesCtr,
}

impl<'a, W: Write> ReencryptWriter<'a, W> {
    pub fn new(inner: W, sections: &'a [NczSectionEntry], start_position_in_nca: u64) -> Self {
        Self {
            inner,
            sections,
            position_in_nca: start_position_in_nca,
            current: None,
            scratch: Vec::with_capacity(64 * 1024),
        }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }

    fn find_section_at(&self, pos: u64) -> Option<usize> {
        for (i, s) in self.sections.iter().enumerate() {
            if s.size <= 0 {
                continue;
            }
            let start = s.offset as u64;
            let end = start.saturating_add(s.size as u64);
            if pos >= start && pos < end {
                return Some(i);
            }
        }
        None
    }

    fn next_section_start_after(&self, pos: u64) -> u64 {
        self.sections
            .iter()
            .filter_map(|s| {
                let start = s.offset as u64;
                if s.size > 0 && start > pos {
                    Some(start)
                } else {
                    None
                }
            })
            .min()
            .unwrap_or(u64::MAX)
    }

    fn ensure_cipher_for(&mut self, idx: usize) -> io::Result<()> {
        if matches!(&self.current, Some(c) if c.index == idx) {
            return Ok(());
        }
        let s = &self.sections[idx];
        if !is_ctr_type(s.crypto_type as u8) {
            self.current = Some(CurrentSection {
                index: idx,
                cipher: AesCtr::new(&[0; 16].into(), &[0; 16].into()),
            });
            return Ok(());
        }
        // Match nsz convention: stored `crypto_counter[8..16]` is zero
        // and the runtime fills in `position_in_nca / 16` BE. Replace
        // (not add) so this also tolerates `crypto_counter[8..16]`
        // already containing `section.offset / 16` from older writers.
        let mut counter = s.crypto_counter;
        let block = self.position_in_nca / 16;
        counter[8..16].copy_from_slice(&block.to_be_bytes());
        let cipher = AesCtr::new_from_slices(&s.crypto_key, &counter).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}", NxError::AesError(format!("Ctr128BE init: {e}"))),
            )
        })?;
        self.current = Some(CurrentSection { index: idx, cipher });
        Ok(())
    }
}

fn is_ctr_type(t: u8) -> bool {
    matches!(
        t,
        ENC_AES_CTR | ENC_AES_CTR_EX | ENC_AES_CTR_SKIP_LAYER_HASH | ENC_AES_CTR_EX_SKIP_LAYER_HASH
    )
}

impl<'a, W: Write> Write for ReencryptWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut consumed = 0usize;
        while consumed < buf.len() {
            match self.find_section_at(self.position_in_nca) {
                None => {
                    let next = self.next_section_start_after(self.position_in_nca);
                    let max_in_gap = next.saturating_sub(self.position_in_nca) as usize;
                    let take = (buf.len() - consumed).min(max_in_gap);
                    if take == 0 {
                        return Err(io::Error::other(
                            "ReencryptWriter: position past last section, would write past end",
                        ));
                    }
                    self.inner.write_all(&buf[consumed..consumed + take])?;
                    self.position_in_nca += take as u64;
                    consumed += take;
                    self.current = None;
                }
                Some(idx) => {
                    self.ensure_cipher_for(idx)?;
                    let s = &self.sections[idx];
                    let section_end = (s.offset as u64).saturating_add(s.size as u64);
                    let max_in_section = (section_end - self.position_in_nca) as usize;
                    let take = (buf.len() - consumed).min(max_in_section);
                    let plain = &buf[consumed..consumed + take];

                    if s.crypto_type as u8 == ENC_NONE {
                        self.inner.write_all(plain)?;
                    } else if is_ctr_type(s.crypto_type as u8) {
                        self.scratch.clear();
                        self.scratch.extend_from_slice(plain);
                        if let Some(c) = self.current.as_mut() {
                            c.cipher.apply_keystream(&mut self.scratch);
                        }
                        self.inner.write_all(&self.scratch)?;
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("{}", NxError::UnsupportedEncryption(s.crypto_type as u8)),
                        ));
                    }

                    self.position_in_nca += take as u64;
                    consumed += take;
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctr_section(offset: i64, size: i64, key: [u8; 16], counter: [u8; 16]) -> NczSectionEntry {
        NczSectionEntry {
            offset,
            size,
            crypto_type: ENC_AES_CTR as i64,
            crypto_key: key,
            crypto_counter: counter,
        }
    }

    fn one_shot_encrypt(
        plain: &[u8],
        section: &NczSectionEntry,
        position_at_start: u64,
    ) -> Vec<u8> {
        let mut counter = section.crypto_counter;
        let block = position_at_start / 16;
        counter[8..16].copy_from_slice(&block.to_be_bytes());
        let mut cipher = AesCtr::new_from_slices(&section.crypto_key, &counter).unwrap();
        let mut buf = plain.to_vec();
        cipher.apply_keystream(&mut buf);
        buf
    }

    #[test]
    fn single_section_one_shot_matches_byte_by_byte() {
        let key = [0x55u8; 16];
        let mut counter = [0xAAu8; 16];
        // Counter low half starts at 0 (offset 0 in section).
        counter[8..16].copy_from_slice(&0u64.to_be_bytes());
        let s = ctr_section(0x4000, 0x4000, key, counter);
        let plain: Vec<u8> = (0..0x4000).map(|i| (i & 0xFF) as u8).collect();
        let expected = one_shot_encrypt(&plain, &s, 0x4000);

        let mut sink_a = Vec::new();
        {
            let mut w = ReencryptWriter::new(&mut sink_a, std::slice::from_ref(&s), 0x4000);
            w.write_all(&plain).unwrap();
        }
        assert_eq!(sink_a, expected);

        let mut sink_b = Vec::new();
        {
            let mut w = ReencryptWriter::new(&mut sink_b, std::slice::from_ref(&s), 0x4000);
            for byte in &plain {
                w.write_all(std::slice::from_ref(byte)).unwrap();
            }
        }
        assert_eq!(sink_b, expected, "byte-by-byte must match one-shot");
    }

    #[test]
    fn cross_section_boundary_switches_cipher() {
        let key_a = [0x11u8; 16];
        let key_b = [0x22u8; 16];
        let mut ctr_a = [0xAAu8; 16];
        ctr_a[8..16].copy_from_slice(&0u64.to_be_bytes());
        let mut ctr_b = [0xBBu8; 16];
        ctr_b[8..16].copy_from_slice(&0u64.to_be_bytes());

        let sec_a = ctr_section(0x4000, 0x2000, key_a, ctr_a);
        let sec_b = ctr_section(0x6000, 0x2000, key_b, ctr_b);
        let sections = vec![sec_a, sec_b];

        let plain: Vec<u8> = (0..0x4000).map(|i| (i & 0xFF) as u8).collect();
        let mut expected = Vec::with_capacity(0x4000);
        expected.extend_from_slice(&one_shot_encrypt(&plain[..0x2000], &sections[0], 0x4000));
        expected.extend_from_slice(&one_shot_encrypt(&plain[0x2000..], &sections[1], 0x6000));

        let mut sink = Vec::new();
        {
            let mut w = ReencryptWriter::new(&mut sink, &sections, 0x4000);
            w.write_all(&plain).unwrap();
        }
        assert_eq!(sink, expected);
    }

    #[test]
    fn unaligned_writes_inside_section_match_one_shot() {
        let key = [0x77u8; 16];
        let mut ctr = [0xFFu8; 16];
        ctr[8..16].copy_from_slice(&0u64.to_be_bytes());
        let s = ctr_section(0x4000, 0x2000, key, ctr);

        let plain: Vec<u8> = (0..0x2000).map(|i| (i & 0xFF) as u8).collect();
        let expected = one_shot_encrypt(&plain, &s, 0x4000);

        // Pathological chunk pattern: 1, 17, 33, ... non-16-aligned writes.
        let mut sink = Vec::new();
        {
            let mut w = ReencryptWriter::new(&mut sink, std::slice::from_ref(&s), 0x4000);
            let mut at = 0;
            let mut step = 1;
            while at < plain.len() {
                let take = step.min(plain.len() - at);
                w.write_all(&plain[at..at + take]).unwrap();
                at += take;
                step = (step * 2 + 1).min(0x40);
            }
        }
        assert_eq!(sink, expected);
    }

    #[test]
    fn gap_between_sections_passes_through() {
        let key_a = [0x11u8; 16];
        let key_b = [0x22u8; 16];
        let mut ctr = [0u8; 16];
        ctr[8..16].copy_from_slice(&0u64.to_be_bytes());

        let sec_a = ctr_section(0x4000, 0x1000, key_a, ctr);
        let sec_b = ctr_section(0x6000, 0x1000, key_b, ctr);
        let sections = vec![sec_a, sec_b];

        let plain: Vec<u8> = (0..0x3000).map(|i| (i & 0xFF) as u8).collect();
        let mut expected = Vec::with_capacity(0x3000);
        expected.extend_from_slice(&one_shot_encrypt(&plain[..0x1000], &sections[0], 0x4000));
        expected.extend_from_slice(&plain[0x1000..0x2000]);
        expected.extend_from_slice(&one_shot_encrypt(&plain[0x2000..], &sections[1], 0x6000));

        let mut sink = Vec::new();
        {
            let mut w = ReencryptWriter::new(&mut sink, &sections, 0x4000);
            w.write_all(&plain).unwrap();
        }
        assert_eq!(sink, expected);
    }
}
