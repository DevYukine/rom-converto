//! Wii U TMD (Title Metadata) parser.
//!
//! Field offsets match `TMDFileHeaderWiiU` / `TMDFileContentEntryWiiU`
//! in Cemu's `src/Cemu/ncrypto/ncrypto.cpp`. The TMD layout is:
//!
//! - Fixed `WUP_TMD_HEADER_SIZE` byte header (signature, metadata,
//!   64 x 36-byte content info records).
//! - `num_content` x `WUP_TMD_CONTENT_ENTRY_SIZE` byte content
//!   entries immediately after.
//! - Optional trailing certificate chain (unused here).

use crate::nintendo::wup::error::{WupError, WupResult};

/// Size of the base TMD header including the 64-entry ContentInfo
/// array at `+0x204` (64 x 36 = 2304 bytes).
pub const WUP_TMD_HEADER_SIZE: usize = 0x204 + 64 * 36;

/// Serialised size of a single TMD content entry.
pub const WUP_TMD_CONTENT_ENTRY_SIZE: usize = 0x30;

const OFFSET_SIGNATURE_TYPE: usize = 0x000;
const OFFSET_TMD_VERSION: usize = 0x180;
const OFFSET_TITLE_ID: usize = 0x18C;
const OFFSET_TITLE_TYPE: usize = 0x194;
const OFFSET_GROUP_ID: usize = 0x198;
const OFFSET_ACCESS_RIGHTS: usize = 0x1D8;
const OFFSET_TITLE_VERSION: usize = 0x1DC;
const OFFSET_NUM_CONTENT: usize = 0x1DE;
const OFFSET_BOOT_INDEX: usize = 0x1E0;
const OFFSET_CONTENT_INFO_HASH: usize = 0x1E4;

/// Content entry flag bits from the TMD `type` field. Wii U uses
/// just two of the sixteen bits: encrypted (bit 0) and hashed-mode
/// content (bit 1). Unknown bits are preserved on round trip so we
/// don't silently drop them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmdContentFlags(u16);

impl TmdContentFlags {
    /// Set if the content is AES-CBC encrypted under the ticket
    /// title key. Every retail title sets this.
    pub const ENCRYPTED: TmdContentFlags = TmdContentFlags(0x0001);
    /// Set if the content uses hashed-mode layout (64 KiB blocks
    /// with a 0x400 hash prefix each). Clear means raw AES-CBC
    /// streaming with IV = 0.
    pub const HASHED: TmdContentFlags = TmdContentFlags(0x0002);

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for TmdContentFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        TmdContentFlags(self.0 | rhs.0)
    }
}

/// Minimal parsed view over a Wii U `title.tmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WupTmd {
    pub signature_type: u32,
    pub tmd_version: u8,
    pub title_id: u64,
    pub title_type: u32,
    pub group_id: u16,
    pub access_rights: u32,
    pub title_version: u16,
    pub boot_index: u16,
    pub content_info_hash: [u8; 32],
    pub contents: Vec<TmdContentEntry>,
}

/// One content entry from the TMD content list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmdContentEntry {
    pub content_id: u32,
    pub index: u16,
    pub flags: TmdContentFlags,
    pub size: u64,
    pub hash: [u8; 32],
}

impl TmdContentEntry {
    /// True if the content is hashed-mode (64 KiB blocks with a
    /// 0x400 hash prefix each) rather than raw AES-CBC.
    pub fn is_hashed(&self) -> bool {
        self.flags.contains(TmdContentFlags::HASHED)
    }

    /// True if the content is AES-CBC encrypted under the ticket
    /// title key. Retail Wii U content almost always sets this.
    pub fn is_encrypted(&self) -> bool {
        self.flags.contains(TmdContentFlags::ENCRYPTED)
    }
}

impl WupTmd {
    /// Parse a Wii U TMD from a byte slice.
    pub fn parse(bytes: &[u8]) -> WupResult<Self> {
        if bytes.len() < WUP_TMD_HEADER_SIZE {
            return Err(WupError::InvalidTmd);
        }
        let title_id = read_u64_be(bytes, OFFSET_TITLE_ID);
        let title_version = read_u16_be(bytes, OFFSET_TITLE_VERSION);
        let num_content = read_u16_be(bytes, OFFSET_NUM_CONTENT) as usize;

        let entries_start = WUP_TMD_HEADER_SIZE;
        let entries_end = entries_start
            .checked_add(num_content * WUP_TMD_CONTENT_ENTRY_SIZE)
            .ok_or(WupError::InvalidTmd)?;
        if bytes.len() < entries_end {
            return Err(WupError::InvalidTmd);
        }

        let mut contents = Vec::with_capacity(num_content);
        for i in 0..num_content {
            let entry_start = entries_start + i * WUP_TMD_CONTENT_ENTRY_SIZE;
            let entry = &bytes[entry_start..entry_start + WUP_TMD_CONTENT_ENTRY_SIZE];
            let content_id = u32::from_be_bytes(entry[0x00..0x04].try_into().unwrap());
            let index = u16::from_be_bytes(entry[0x04..0x06].try_into().unwrap());
            let type_bits = u16::from_be_bytes(entry[0x06..0x08].try_into().unwrap());
            let size = u64::from_be_bytes(entry[0x08..0x10].try_into().unwrap());
            let hash: [u8; 32] = entry[0x10..0x30].try_into().unwrap();
            contents.push(TmdContentEntry {
                content_id,
                index,
                flags: TmdContentFlags::from_bits(type_bits),
                size,
                hash,
            });
        }

        Ok(Self {
            signature_type: read_u32_be(bytes, OFFSET_SIGNATURE_TYPE),
            tmd_version: bytes[OFFSET_TMD_VERSION],
            title_id,
            title_type: read_u32_be(bytes, OFFSET_TITLE_TYPE),
            group_id: read_u16_be(bytes, OFFSET_GROUP_ID),
            access_rights: read_u32_be(bytes, OFFSET_ACCESS_RIGHTS),
            title_version,
            boot_index: read_u16_be(bytes, OFFSET_BOOT_INDEX),
            content_info_hash: bytes[OFFSET_CONTENT_INFO_HASH..OFFSET_CONTENT_INFO_HASH + 32]
                .try_into()
                .unwrap(),
            contents,
        })
    }

    /// Lookup a content entry by its FST cluster index. Returns
    /// `None` if no content with that index exists.
    pub fn content_by_index(&self, index: u16) -> Option<&TmdContentEntry> {
        self.contents.iter().find(|c| c.index == index)
    }
}

fn read_u16_be(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32_be(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64_be(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tmd(title_id: u64, title_version: u16, contents: &[TmdContentEntry]) -> Vec<u8> {
        let mut bytes =
            vec![0u8; WUP_TMD_HEADER_SIZE + contents.len() * WUP_TMD_CONTENT_ENTRY_SIZE];
        bytes[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
        bytes[OFFSET_TMD_VERSION] = 1;
        bytes[OFFSET_TITLE_ID..OFFSET_TITLE_ID + 8].copy_from_slice(&title_id.to_be_bytes());
        bytes[OFFSET_TITLE_TYPE..OFFSET_TITLE_TYPE + 4]
            .copy_from_slice(&0x0000_0100u32.to_be_bytes());
        bytes[OFFSET_GROUP_ID..OFFSET_GROUP_ID + 2].copy_from_slice(&0x1000u16.to_be_bytes());
        bytes[OFFSET_ACCESS_RIGHTS..OFFSET_ACCESS_RIGHTS + 4].copy_from_slice(&0u32.to_be_bytes());
        bytes[OFFSET_TITLE_VERSION..OFFSET_TITLE_VERSION + 2]
            .copy_from_slice(&title_version.to_be_bytes());
        bytes[OFFSET_NUM_CONTENT..OFFSET_NUM_CONTENT + 2]
            .copy_from_slice(&(contents.len() as u16).to_be_bytes());
        bytes[OFFSET_BOOT_INDEX..OFFSET_BOOT_INDEX + 2].copy_from_slice(&0u16.to_be_bytes());

        for (i, entry) in contents.iter().enumerate() {
            let start = WUP_TMD_HEADER_SIZE + i * WUP_TMD_CONTENT_ENTRY_SIZE;
            bytes[start..start + 4].copy_from_slice(&entry.content_id.to_be_bytes());
            bytes[start + 4..start + 6].copy_from_slice(&entry.index.to_be_bytes());
            bytes[start + 6..start + 8].copy_from_slice(&entry.flags.bits().to_be_bytes());
            bytes[start + 8..start + 16].copy_from_slice(&entry.size.to_be_bytes());
            bytes[start + 16..start + 48].copy_from_slice(&entry.hash);
        }
        bytes
    }

    fn entry(content_id: u32, index: u16, flags: TmdContentFlags, size: u64) -> TmdContentEntry {
        let mut hash = [0u8; 32];
        hash[0] = (content_id & 0xFF) as u8;
        TmdContentEntry {
            content_id,
            index,
            flags,
            size,
            hash,
        }
    }

    #[test]
    fn parses_header_and_content_list() {
        let contents = vec![
            entry(0, 0, TmdContentFlags::ENCRYPTED, 0x10000),
            entry(1, 1, TmdContentFlags::ENCRYPTED, 0x8000_0000),
            entry(
                2,
                2,
                TmdContentFlags::ENCRYPTED | TmdContentFlags::HASHED,
                0x4000_0000,
            ),
        ];
        let bytes = make_tmd(0x0005_000E_1010_2000, 32, &contents);
        let tmd = WupTmd::parse(&bytes).unwrap();
        assert_eq!(tmd.title_id, 0x0005_000E_1010_2000);
        assert_eq!(tmd.title_version, 32);
        assert_eq!(tmd.contents.len(), 3);
        assert_eq!(tmd.contents[0].content_id, 0);
        assert_eq!(tmd.contents[1].size, 0x8000_0000);
        assert_eq!(
            tmd.contents[2].flags,
            TmdContentFlags::ENCRYPTED | TmdContentFlags::HASHED
        );
        assert!(tmd.contents[2].is_hashed());
        assert!(tmd.contents[0].is_encrypted());
    }

    #[test]
    fn rejects_truncated_header() {
        let short = vec![0u8; WUP_TMD_HEADER_SIZE - 1];
        let err = WupTmd::parse(&short);
        assert!(matches!(err, Err(WupError::InvalidTmd)));
    }

    #[test]
    fn rejects_truncated_content_entries() {
        let contents = vec![entry(0, 0, TmdContentFlags::ENCRYPTED, 0x10000); 3];
        let mut bytes = make_tmd(0x0005_000E_1010_2000, 32, &contents);
        bytes.truncate(WUP_TMD_HEADER_SIZE + WUP_TMD_CONTENT_ENTRY_SIZE);
        let err = WupTmd::parse(&bytes);
        assert!(matches!(err, Err(WupError::InvalidTmd)));
    }

    #[test]
    fn zero_content_list_is_allowed() {
        // A TMD with zero content entries is legal (though unusual);
        // it just means the title has no payload.
        let bytes = make_tmd(0x0005_000E_1010_2000, 0, &[]);
        let tmd = WupTmd::parse(&bytes).unwrap();
        assert!(tmd.contents.is_empty());
    }

    #[test]
    fn content_by_index_lookup() {
        let contents = vec![
            entry(0x100, 0, TmdContentFlags::ENCRYPTED, 0),
            entry(0x101, 5, TmdContentFlags::ENCRYPTED, 0),
            entry(0x102, 10, TmdContentFlags::ENCRYPTED, 0),
        ];
        let bytes = make_tmd(0x0005_000E_1010_2000, 0, &contents);
        let tmd = WupTmd::parse(&bytes).unwrap();
        assert_eq!(tmd.content_by_index(5).unwrap().content_id, 0x101);
        assert_eq!(tmd.content_by_index(10).unwrap().content_id, 0x102);
        assert!(tmd.content_by_index(99).is_none());
    }

    #[test]
    fn flag_bits_round_trip() {
        let flags = TmdContentFlags::ENCRYPTED | TmdContentFlags::HASHED;
        assert_eq!(flags.bits(), 0x0003);
        assert!(flags.contains(TmdContentFlags::ENCRYPTED));
        assert!(flags.contains(TmdContentFlags::HASHED));
    }

    #[test]
    fn unknown_flag_bits_are_preserved() {
        // Future flag bits pass through from_bits instead of being
        // silently dropped, so an unexpected Wii U title we haven't
        // modelled yet still round-trips.
        let flags = TmdContentFlags::from_bits(0x8003);
        assert_eq!(flags.bits(), 0x8003);
        assert!(flags.contains(TmdContentFlags::ENCRYPTED));
        assert!(flags.contains(TmdContentFlags::HASHED));
    }
}
