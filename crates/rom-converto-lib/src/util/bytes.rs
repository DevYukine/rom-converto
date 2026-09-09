//! Fixed-width integer reads at a byte offset into a slice.
//!
//! Callers bounds-check: a slice too short for the requested width panics.

pub fn u16_be(buf: &[u8], off: usize) -> u16 {
    u16::from_be_bytes(buf[off..off + 2].try_into().expect("2-byte slice"))
}

pub fn u32_be(buf: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(buf[off..off + 4].try_into().expect("4-byte slice"))
}

pub fn u64_be(buf: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(buf[off..off + 8].try_into().expect("8-byte slice"))
}

pub fn u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().expect("2-byte slice"))
}

pub fn u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().expect("4-byte slice"))
}

pub fn u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().expect("8-byte slice"))
}

/// Decodes a fixed-width field holding a NUL-terminated ASCII string,
/// dropping the terminator and everything after it.
pub fn cstr_ascii(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Renders a fixed-width header name field as a trimmed string, dropping
/// the zero, 0xFF, and control bytes used as padding.
pub(crate) fn ascii_trim(bytes: &[u8]) -> String {
    bytes
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
}
