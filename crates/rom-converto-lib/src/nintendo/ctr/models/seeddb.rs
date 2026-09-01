use binrw::{BinRead, BinWrite};

/// A `seeddb.bin` seed database: a count followed by that many [`SeedEntry`]
/// records.
#[derive(BinRead, BinWrite, Debug, Clone)]
#[brw(little)]
pub struct SeedDatabase {
    pub seed_count: u32,
    #[brw(pad_before = 12)]
    #[br(count = seed_count)]
    pub seeds: Vec<SeedEntry>,
}

/// One seed database entry: a title ID (`key`, big-endian hex) mapped to its
/// 16-byte seed (`value`).
#[derive(BinRead, BinWrite, Debug, Clone)]
#[non_exhaustive]
pub struct SeedEntry {
    #[br(map = |x: [u8; 8]| {
        let mut key = x;
        key.reverse();
        hex::encode(key)
    })]
    #[bw(map = |x: &String| -> [u8; 8] {
        let mut bytes = hex::decode(x).unwrap_or_else(|_| vec![0; 8]);
        bytes.resize(8, 0);
        bytes.reverse();
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes[..8]);
        arr
    })]
    pub key: String,

    #[brw(pad_after = 8)]
    pub value: [u8; 16],
}
