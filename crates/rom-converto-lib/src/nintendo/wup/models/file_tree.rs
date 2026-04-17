//! ZArchive `FileDirectoryEntry` (16 bytes, big-endian).
//!
//! A single 16-byte record describes either a file or a directory.
//! The C++ writer exploits a tagged-union layout where both variants
//! are three consecutive u32s after the `name_offset_and_type_flag`
//! header, so this struct mirrors that representation with named
//! fields from the file variant and accessors that decode the
//! directory view on demand.
//!
//! File variant:
//!
//! | field                        | meaning                                   |
//! |------------------------------|-------------------------------------------|
//! | `name_offset_and_type_flag`  | MSB=1 (file) then 31-bit name table offset|
//! | `payload_a` (`file_offset_low`) | lower 32 bits of file offset           |
//! | `payload_b` (`file_size_low`)   | lower 32 bits of file size             |
//! | `payload_c`                  | upper 16 bits of size in `>> 16`, upper 16 bits of offset in `& 0xFFFF` |
//!
//! Directory variant (same three u32s, different meaning):
//!
//! | field                        | meaning                                   |
//! |------------------------------|-------------------------------------------|
//! | `name_offset_and_type_flag`  | MSB=0 (dir) then 31-bit name table offset |
//! | `payload_a` (`node_start_index`) | index of first child in file tree     |
//! | `payload_b` (`child_count`)  | number of consecutive children            |
//! | `payload_c` (`_reserved`)    | always zero                               |
//!
//! Both the file offset and file size are 48-bit quantities split
//! across `payload_b` (low 32) and the low/high halves of `payload_c`
//! (high 16 each).

use binrw::{BinRead, BinWrite};

use crate::nintendo::wup::constants::{
    FILE_DIR_NAME_OFFSET_MASK, FILE_DIR_TYPE_FLAG_FILE, ROOT_NAME_OFFSET_SENTINEL,
};

/// 16-byte file-or-directory record from the ZArchive file tree
/// section. Serialised as four consecutive big-endian u32s; the
/// upstream writer uses a tagged-union optimisation that writes the
/// three payload slots as if they were always the file variant.
#[derive(Debug, Clone, Copy, BinRead, BinWrite, PartialEq, Eq)]
#[brw(big)]
pub struct FileDirectoryEntry {
    /// Bit 31 is the file flag (1 = file, 0 = directory); the lower
    /// 31 bits are the byte offset into the name table.
    pub name_offset_and_type_flag: u32,
    /// File variant: lower 32 bits of file offset.
    /// Directory variant: index of first child entry in the file tree.
    pub payload_a: u32,
    /// File variant: lower 32 bits of file size.
    /// Directory variant: number of consecutive children.
    pub payload_b: u32,
    /// File variant: upper 16 bits of file offset (lower half) and
    /// upper 16 bits of file size (upper half).
    /// Directory variant: reserved, always zero.
    pub payload_c: u32,
}

impl FileDirectoryEntry {
    /// Build a file entry. `offset` and `size` must each fit in 48
    /// bits (the format cap).
    pub fn new_file(name_offset: u32, offset: u64, size: u64) -> Self {
        debug_assert!(offset < (1u64 << 48), "file offset exceeds 48-bit limit");
        debug_assert!(size < (1u64 << 48), "file size exceeds 48-bit limit");
        let name_offset_and_type_flag =
            (name_offset & FILE_DIR_NAME_OFFSET_MASK) | FILE_DIR_TYPE_FLAG_FILE;
        let payload_a = offset as u32;
        let payload_b = size as u32;
        // Low 16 bits of payload_c hold the upper 16 bits of the 48-bit
        // offset; high 16 bits hold the upper 16 bits of the 48-bit size.
        let offset_hi = ((offset >> 32) & 0xFFFF) as u32;
        let size_hi = ((size >> 16) & 0xFFFF_0000) as u32;
        let payload_c = size_hi | offset_hi;
        Self {
            name_offset_and_type_flag,
            payload_a,
            payload_b,
            payload_c,
        }
    }

    /// Build a directory entry.
    pub fn new_directory(name_offset: u32, node_start_index: u32, child_count: u32) -> Self {
        Self {
            name_offset_and_type_flag: name_offset & FILE_DIR_NAME_OFFSET_MASK,
            payload_a: node_start_index,
            payload_b: child_count,
            payload_c: 0,
        }
    }

    /// Build the archive root entry: an empty-name directory with the
    /// sentinel name offset. `first_child_index` is usually 1 (the
    /// root itself is index 0) and `child_count` is the number of
    /// top-level directories.
    pub fn root(first_child_index: u32, child_count: u32) -> Self {
        Self::new_directory(ROOT_NAME_OFFSET_SENTINEL, first_child_index, child_count)
    }

    /// True if this entry describes a file, false for a directory.
    pub fn is_file(&self) -> bool {
        (self.name_offset_and_type_flag & FILE_DIR_TYPE_FLAG_FILE) != 0
    }

    /// Byte offset into the name table, stripped of the type flag.
    pub fn name_offset(&self) -> u32 {
        self.name_offset_and_type_flag & FILE_DIR_NAME_OFFSET_MASK
    }

    /// Decode the 48-bit file offset (file variant only).
    pub fn file_offset(&self) -> u64 {
        debug_assert!(self.is_file());
        (self.payload_a as u64) | (((self.payload_c & 0xFFFF) as u64) << 32)
    }

    /// Decode the 48-bit file size (file variant only).
    pub fn file_size(&self) -> u64 {
        debug_assert!(self.is_file());
        (self.payload_b as u64) | (((self.payload_c & 0xFFFF_0000) as u64) << 16)
    }

    /// Read the first-child index (directory variant only).
    pub fn node_start_index(&self) -> u32 {
        debug_assert!(!self.is_file());
        self.payload_a
    }

    /// Read the child count (directory variant only).
    pub fn child_count(&self) -> u32 {
        debug_assert!(!self.is_file());
        self.payload_b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::constants::FILE_DIRECTORY_ENTRY_SIZE;
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    fn serialise(entry: &FileDirectoryEntry) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        entry.write(&mut buf).unwrap();
        buf.into_inner()
    }

    #[test]
    fn entry_serialises_to_16_bytes() {
        let entry = FileDirectoryEntry::new_file(0x1234, 0, 0);
        assert_eq!(serialise(&entry).len(), FILE_DIRECTORY_ENTRY_SIZE);
    }

    #[test]
    fn file_entry_is_file_flag() {
        let entry = FileDirectoryEntry::new_file(0x1234, 0, 0);
        assert!(entry.is_file());
        assert_eq!(entry.name_offset(), 0x1234);
        assert_eq!(entry.name_offset_and_type_flag, 0x8000_1234);
    }

    #[test]
    fn directory_entry_is_dir_flag() {
        let entry = FileDirectoryEntry::new_directory(0x1234, 1, 5);
        assert!(!entry.is_file());
        assert_eq!(entry.name_offset(), 0x1234);
        assert_eq!(entry.name_offset_and_type_flag, 0x0000_1234);
        assert_eq!(entry.node_start_index(), 1);
        assert_eq!(entry.child_count(), 5);
        assert_eq!(entry.payload_c, 0);
    }

    #[test]
    fn root_uses_sentinel_name_offset() {
        let root = FileDirectoryEntry::root(1, 3);
        assert!(!root.is_file());
        assert_eq!(root.name_offset_and_type_flag, ROOT_NAME_OFFSET_SENTINEL);
        assert_eq!(root.node_start_index(), 1);
        assert_eq!(root.child_count(), 3);
    }

    #[test]
    fn file_entry_round_trips_48_bit_offset_and_size() {
        let offset: u64 = 0x1234_5678_9ABC;
        let size: u64 = 0xEDCB_A987_6543;
        let entry = FileDirectoryEntry::new_file(0x5A5A, offset, size);
        assert_eq!(entry.file_offset(), offset);
        assert_eq!(entry.file_size(), size);
    }

    #[test]
    fn file_entry_round_trips_max_48_bit_values() {
        let max48: u64 = (1u64 << 48) - 1;
        let entry = FileDirectoryEntry::new_file(0, max48, max48);
        assert_eq!(entry.file_offset(), max48);
        assert_eq!(entry.file_size(), max48);
    }

    #[test]
    fn file_entry_byte_layout_is_big_endian() {
        // Pick values where every byte position is distinguishable,
        // so a byte-order bug would jump out in the assertion diff.
        let offset: u64 = 0x1234_5678_9ABC;
        let size: u64 = 0xEDCB_A987_6543;
        let entry = FileDirectoryEntry::new_file(0x1_2345, offset, size);
        let bytes = serialise(&entry);

        // name_offset_and_type_flag = 0x80012345
        assert_eq!(&bytes[0..4], &0x8001_2345u32.to_be_bytes());
        // payload_a = lower 32 bits of offset = 0x56789ABC
        assert_eq!(&bytes[4..8], &0x5678_9ABCu32.to_be_bytes());
        // payload_b = lower 32 bits of size = 0xA9876543
        assert_eq!(&bytes[8..12], &0xA987_6543u32.to_be_bytes());
        // payload_c upper 16 = size high (0xEDCB), lower 16 = offset high (0x1234)
        assert_eq!(&bytes[12..16], &0xEDCB_1234u32.to_be_bytes());
    }

    #[test]
    fn directory_entry_byte_layout() {
        let entry = FileDirectoryEntry::new_directory(0x1234, 0x10, 0x20);
        let bytes = serialise(&entry);
        assert_eq!(&bytes[0..4], &0x0000_1234u32.to_be_bytes());
        assert_eq!(&bytes[4..8], &0x0000_0010u32.to_be_bytes());
        assert_eq!(&bytes[8..12], &0x0000_0020u32.to_be_bytes());
        assert_eq!(&bytes[12..16], &0u32.to_be_bytes());
    }

    #[test]
    fn entry_round_trips_through_binrw() {
        let original = FileDirectoryEntry::new_file(0x7F, 0x1234_5678_9ABC, 0xEDCB_A987_6543);
        let bytes = serialise(&original);
        let parsed = FileDirectoryEntry::read(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(parsed, original);
        assert!(parsed.is_file());
        assert_eq!(parsed.file_offset(), 0x1234_5678_9ABC);
        assert_eq!(parsed.file_size(), 0xEDCB_A987_6543);
    }

    #[test]
    fn new_file_masks_name_offset_to_31_bits() {
        // A caller passing garbage in the top bit must not flip the
        // type flag accidentally.
        let entry = FileDirectoryEntry::new_file(0xFFFF_FFFF, 0, 0);
        assert_eq!(entry.name_offset(), FILE_DIR_NAME_OFFSET_MASK);
        assert!(entry.is_file());
    }
}
