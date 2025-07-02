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
