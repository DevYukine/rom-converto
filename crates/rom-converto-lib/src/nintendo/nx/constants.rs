//! Magic bytes, header sizes, and tunable defaults for Switch NSZ.

pub const PFS0_MAGIC: [u8; 4] = *b"PFS0";
pub const HFS0_MAGIC: [u8; 4] = *b"HFS0";
pub const NCA3_MAGIC: [u8; 4] = *b"NCA3";
pub const NCZSECTN_MAGIC: [u8; 8] = *b"NCZSECTN";
pub const NCZBLOCK_MAGIC: [u8; 8] = *b"NCZBLOCK";

pub const NCA_HEADER_SIZE: usize = 0xC00;
pub const NCA_PREFIX_SIZE: usize = 0x4000;
pub const NCA_XTS_SECTOR: usize = 0x200;
pub const NCA_FS_ENTRY_OFFSET: usize = 0x240;
pub const NCA_FS_HEADER_OFFSET: usize = 0x400;
pub const NCA_FS_HEADER_STRIDE: usize = 0x200;
pub const NCA_MAX_SECTIONS: usize = 4;
pub const NCA_SECTOR_SIZE: u64 = 0x200;

pub const PFS0_HEADER_SIZE: usize = 0x10;
pub const PFS0_ENTRY_SIZE: usize = 0x18;
pub const HFS0_HEADER_SIZE: usize = 0x10;
pub const HFS0_ENTRY_SIZE: usize = 0x40;

pub const XCI_HFS0_OFFSET: u64 = 0x10000;
pub const XCI_PARTITIONS: &[&str] = &["update", "logo", "normal", "secure"];

pub const NCZ_SECTION_ENTRY_SIZE: usize = 0x40;

pub const DEFAULT_ZSTD_LEVEL: i32 = 18;
pub const MIN_ZSTD_LEVEL: i32 = 1;
pub const MAX_ZSTD_LEVEL: i32 = 22;

pub const DEFAULT_BLOCK_SIZE_EXP: u8 = 20;
pub const MIN_BLOCK_SIZE_EXP: u8 = 14;
pub const MAX_BLOCK_SIZE_EXP: u8 = 32;

pub const ENC_NONE: u8 = 1;
pub const ENC_AES_XTS: u8 = 2;
pub const ENC_AES_CTR: u8 = 3;
pub const ENC_AES_CTR_EX: u8 = 4;
pub const ENC_AES_CTR_SKIP_LAYER_HASH: u8 = 5;
pub const ENC_AES_CTR_EX_SKIP_LAYER_HASH: u8 = 6;
