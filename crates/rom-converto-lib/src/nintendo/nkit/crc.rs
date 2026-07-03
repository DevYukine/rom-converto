//! Streaming CRC32 (IEEE reflected, the zlib variant NKit uses for
//! its whole-file self-check). A resumable plain-table implementation
//! so positional tee hashing does not fight borrow lifetimes.

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = build_table();

#[derive(Clone)]
pub(crate) struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub(crate) fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        let mut s = self.state;
        for &b in data {
            s = (s >> 8) ^ TABLE[((s ^ b as u32) & 0xFF) as usize];
        }
        self.state = s;
    }

    pub(crate) fn value(&self) -> u32 {
        !self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_vectors() {
        let mut c = Crc32::new();
        c.update(b"123456789");
        assert_eq!(c.value(), 0xCBF4_3926);

        let mut c = Crc32::new();
        c.update(b"The quick brown fox jumps over the lazy dog");
        assert_eq!(c.value(), 0x414F_A339);

        let mut split = Crc32::new();
        split.update(b"The quick brown fox ");
        split.update(b"jumps over the lazy dog");
        assert_eq!(split.value(), 0x414F_A339);
    }
}
