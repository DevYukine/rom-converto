//! Node name table builder for the ZArchive format.
//!
//! The name table is a flat byte buffer of length-prefixed strings
//! shared across every node in the file tree. Each `FileDirectoryEntry`
//! stores a 31-bit offset into this buffer; identical names are
//! deduplicated to save space.
//!
//! The upstream reader has a known bug in its 2-byte-prefix path
//! (names 128 bytes and up), so this builder refuses to encode
//! anything past [`MAX_NODE_NAME_LEN`] and therefore only ever emits
//! the 1-byte short form.

use std::collections::HashMap;

use crate::nintendo::wup::constants::{FILE_DIR_NAME_OFFSET_MASK, MAX_NODE_NAME_LEN};
use crate::nintendo::wup::error::{WupError, WupResult};

/// Accumulates interned node names into a byte buffer for the
/// `sectionNames` region of a ZArchive.
#[derive(Debug, Default)]
pub struct NameTableBuilder {
    bytes: Vec<u8>,
    offsets: HashMap<String, u32>,
}

impl NameTableBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `name`, returning its byte offset into the table.
    ///
    /// Identical names share one entry: the second call with the same
    /// `name` value returns the first call's offset unchanged. Fails
    /// with [`WupError::NameTooLong`] on names longer than
    /// [`MAX_NODE_NAME_LEN`] bytes or with [`WupError::NameTableTooLarge`]
    /// if the table would exceed the format's 31-bit offset limit.
    pub fn intern(&mut self, name: &str) -> WupResult<u32> {
        if let Some(&offset) = self.offsets.get(name) {
            return Ok(offset);
        }
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NODE_NAME_LEN {
            return Err(WupError::NameTooLong(name_bytes.len()));
        }
        let offset = self.bytes.len();
        // One byte for the length prefix plus the name bytes. Reject
        // anything that would push a future offset past the 31-bit
        // section limit.
        let new_end = offset + 1 + name_bytes.len();
        if new_end > FILE_DIR_NAME_OFFSET_MASK as usize {
            return Err(WupError::NameTableTooLarge);
        }
        // Short-form length prefix: one byte, MSB clear.
        self.bytes.push(name_bytes.len() as u8);
        self.bytes.extend_from_slice(name_bytes);
        let offset_u32 = offset as u32;
        self.offsets.insert(name.to_string(), offset_u32);
        Ok(offset_u32)
    }

    /// Current size of the name table in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// True if no names have been interned yet.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Consume the builder and return the serialised name table bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Borrow the current bytes without consuming the builder. Useful
    /// for incremental inspection in tests.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_has_zero_length() {
        let builder = NameTableBuilder::new();
        assert!(builder.is_empty());
        assert_eq!(builder.len(), 0);
        assert_eq!(builder.into_bytes(), Vec::<u8>::new());
    }

    #[test]
    fn intern_returns_zero_offset_for_first_entry() {
        let mut builder = NameTableBuilder::new();
        assert_eq!(builder.intern("hello").unwrap(), 0);
    }

    #[test]
    fn intern_byte_layout_is_short_prefix() {
        let mut builder = NameTableBuilder::new();
        builder.intern("hello").unwrap();
        // [length=5][h,e,l,l,o]
        assert_eq!(builder.as_bytes(), &[5, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn intern_multiple_names_packs_without_separator() {
        let mut builder = NameTableBuilder::new();
        let a = builder.intern("hello").unwrap();
        let b = builder.intern("world").unwrap();
        assert_eq!(a, 0);
        assert_eq!(b, 6); // after [len=5][h,e,l,l,o] = 6 bytes
        assert_eq!(
            builder.as_bytes(),
            &[
                5, b'h', b'e', b'l', b'l', b'o', 5, b'w', b'o', b'r', b'l', b'd'
            ]
        );
    }

    #[test]
    fn intern_deduplicates_identical_names() {
        let mut builder = NameTableBuilder::new();
        let first = builder.intern("meta.xml").unwrap();
        let second = builder.intern("meta.xml").unwrap();
        assert_eq!(first, second);
        // Only one copy should be stored.
        assert_eq!(builder.len(), 1 + "meta.xml".len());
    }

    #[test]
    fn intern_deduplicates_between_other_names() {
        let mut builder = NameTableBuilder::new();
        let a1 = builder.intern("app.xml").unwrap();
        let _b = builder.intern("cos.xml").unwrap();
        let a2 = builder.intern("app.xml").unwrap();
        assert_eq!(a1, a2, "repeated name must return the first offset");
    }

    #[test]
    fn intern_rejects_name_at_boundary_128() {
        let mut builder = NameTableBuilder::new();
        let name = "a".repeat(128);
        let result = builder.intern(&name);
        assert!(
            matches!(result, Err(WupError::NameTooLong(128))),
            "expected NameTooLong(128), got {result:?}"
        );
    }

    #[test]
    fn intern_accepts_max_length_name() {
        let mut builder = NameTableBuilder::new();
        let name = "b".repeat(MAX_NODE_NAME_LEN);
        let offset = builder.intern(&name).unwrap();
        assert_eq!(offset, 0);
        assert_eq!(builder.len(), 1 + MAX_NODE_NAME_LEN);
    }

    #[test]
    fn intern_accepts_zero_length_name() {
        // Empty names exist only for the root entry, but the builder
        // itself must support them (the writer then skips this path
        // and uses the root sentinel instead).
        let mut builder = NameTableBuilder::new();
        let offset = builder.intern("").unwrap();
        assert_eq!(offset, 0);
        assert_eq!(builder.as_bytes(), &[0u8]);
    }

    #[test]
    fn intern_handles_wii_u_path_components() {
        // Typical Wii U loadiine paths are short and deterministic;
        // exercise the real shapes the writer will see.
        let mut builder = NameTableBuilder::new();
        for name in [
            "meta",
            "meta.xml",
            "code",
            "app.xml",
            "cos.xml",
            "content",
            "0005000e10102000_v32",
        ] {
            builder.intern(name).unwrap();
        }
        // All seven names should be packed end to end with exactly
        // one byte of prefix per name.
        let expected: usize = 7 + "metameta.xmlcodeapp.xmlcos.xmlcontent0005000e10102000_v32".len();
        assert_eq!(builder.len(), expected);
    }
}
