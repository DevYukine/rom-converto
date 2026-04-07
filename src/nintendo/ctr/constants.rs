use hex_literal::hex;

pub const CTR_COMMON_KEYS: [&str; 6] = [
    "64c5fd55dd3ad988325baaec5243db98",
    "4aaa3d0e27d4d728d0b1b433f0f9cbc8",
    "fbb0ef8cdbb0d8e453cd99344371697f",
    "25959b7ad0409f72684198ba2ecd7dc6",
    "7ada22caffc476cc8297a0c7ceeeeebe",
    "a5051ca1b37dcf3afbcf8cc1edd9ce02",
];

pub const CTR_COMMON_KEYS_HEX: [[u8; 16]; 6] = [
    hex!("64c5fd55dd3ad988325baaec5243db98"),
    hex!("4aaa3d0e27d4d728d0b1b433f0f9cbc8"),
    hex!("fbb0ef8cdbb0d8e453cd99344371697f"),
    hex!("25959b7ad0409f72684198ba2ecd7dc6"),
    hex!("7ada22caffc476cc8297a0c7ceeeeebe"),
    hex!("a5051ca1b37dcf3afbcf8cc1edd9ce02"),
];

pub const CTR_TITLE_KEY_SECRET: &str = "fd040105060b111c2d49";

pub const CTR_DEFAULT_TITLE_KEY_PASSWORD: &str = "mypass";

pub const CTR_KEY_0X2C: u128 = 246647523836745093481291640204864831571;
pub const CTR_KEY_0X25: u128 = 275024782269591852539264289417494026995;
pub const CTR_KEY_0X18: u128 = 174013536497093865167571429864564540276;
pub const CTR_KEY_0X1B: u128 = 92615092018138441822550407327763030402;
pub const CTR_FIXED_SYS: u128 = 109645209274529458878270608689136408907;

pub const CTR_KEYS_0: [u128; 4] = [CTR_KEY_0X2C, CTR_KEY_0X25, CTR_KEY_0X18, CTR_KEY_0X1B];
pub const CTR_KEYS_1: [u128; 2] = [0, CTR_FIXED_SYS];

pub const CTR_NCSD_PARTITIONS: [&str; 8] = [
    "Main",
    "Manual",
    "Download Play",
    "Partition4",
    "Partition5",
    "Partition6",
    "N3DSUpdateData",
    "UpdateData",
];

pub const CTR_MEDIA_UNIT_SIZE: u32 = 512;

// NCCH header offsets (relative to the start of an NCCH block)
pub const NCCH_MAGIC_OFFSET: usize = 0x100;
pub const NCCH_FLAGS_OFFSET: usize = 0x188;
pub const NCCH_FLAGS7_FIXED_KEY: u8 = 0x01;
pub const NCCH_FLAGS7_CRYPTO_METHOD: u8 = 0x02;
pub const NCCH_FLAGS7_NOCRYPTO: u8 = 0x04;
pub const NCCH_FLAGS7_SEED_CRYPTO: u8 = 0x20;
pub const NCCH_FLAGS_EXTRA_CRYPTO_INDEX: usize = 3;

// NCSD partition table
pub const NCSD_PARTITION0_OFFSET_FIELD: usize = 0x120;

// ExeFS format
pub const EXEFS_HEADER_SIZE: usize = 0x200;
pub const EXEFS_MAX_FILE_ENTRIES: usize = 10;
pub const EXEFS_ENTRY_SIZE: usize = 16;
pub const EXEFS_SECTION_ICON: [u8; 4] = *b"icon";
pub const EXEFS_SECTION_BANNER: [u8; 6] = *b"banner";

// NCCH magic identifier
pub const NCCH_MAGIC: &str = "NCCH";

// Ticket structure offsets (relative to start of ticket)
pub const TICKET_SIG_BODY_OFFSET: u64 = 0x140;
pub const TICKET_TITLE_KEY_OFFSET: u64 = 0x7F;
pub const TICKET_TITLE_ID_OFFSET: u64 = 0x9C;
pub const TICKET_COMMON_KEY_IDX_OFFSET: u64 = 0xB1;

// TMD structure offsets (relative to start of TMD)
pub const TMD_CONTENT_COUNT_OFFSET: u64 = 0x206;
pub const TMD_CONTENT_RECORDS_OFFSET: u64 = 0xB04;
pub const TMD_CONTENT_RECORD_SIZE: u64 = 48;

// Key scrambler constant used in 3DS key derivation
pub const CTR_KEY_SCRAMBLE_C: u128 = 42503689118608475533858958821215598218;

// CIA format
pub const CIA_CONTENT_INDEX_SIZE: usize = 0x2000;
pub const CIA_CERT_CHAIN_SIZE: u32 = 0xA00;
pub const CERT_SIG_TYPE_MIN: u32 = 0x010000;
pub const CERT_SIG_TYPE_MAX: u32 = 0x010005;

// Title key derivation
pub const CTR_TITLE_KEY_PBKDF2_ITERATIONS: u32 = 20;

// Seed fetch countries for NCCH seed crypto
pub const CTR_SEED_COUNTRIES: [&str; 7] = ["JP", "US", "GB", "KR", "TW", "AU", "NZ"];
