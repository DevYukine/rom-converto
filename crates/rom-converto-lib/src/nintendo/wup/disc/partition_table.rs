//! Wii U disc partition table parser.
//!
//! The partition table of contents ("TOC") lives at absolute offset
//! `0x18000` on every Wii U disc. It is AES-128-CBC encrypted under
//! the per-disc master key with a zero IV. After decryption:
//!
//! ```text
//! 0x0000  u32 BE  sentinel == CC A6 E6 7B (DECRYPTED_AREA_SIGNATURE)
//! 0x001C  u32 BE  partitionCount
//! 0x0800  u8[0x80] * partitionCount  partition entries
//!   entry:
//!     0x00  u8[0x19]  name (ASCII, null-padded)
//!     0x20  u32 BE    startSector (multiply by 0x8000 for disc byte offset)
//! ```
//!
//! Everything is big-endian once decrypted; the disc-level AES uses
//! the disc key as the 128 bit key and a zero 128 bit IV. Chunks of
//! `0x10000` are decrypted at a time, but we only need the first
//! `0x8000` so we stop there.

use crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place;
use crate::nintendo::wup::disc::disc_key::DiscKey;
use crate::nintendo::wup::disc::sector_stream::{DiscSectorSource, SECTOR_SIZE};
use crate::nintendo::wup::error::{WupError, WupResult};

/// Absolute byte offset of the partition TOC on the disc.
pub const PARTITION_TOC_OFFSET: u64 = 0x18000;

/// Sentinel the first four decrypted bytes must equal for the key to
/// be considered correct. Big-endian.
pub const DECRYPTED_AREA_SIGNATURE: [u8; 4] = [0xCC, 0xA6, 0xE6, 0x7B];

/// Offset to `partitionCount` within the decrypted TOC sector.
const PARTITION_COUNT_OFFSET: usize = 0x1C;

/// Offset of the first entry within the decrypted TOC sector.
const PARTITION_ENTRIES_OFFSET: usize = 0x800;

/// Serialised size of one entry in bytes.
const PARTITION_ENTRY_SIZE: usize = 0x80;

/// Maximum entries we will parse. Real discs have a handful (SI + GM +
/// optional UP/UC), so anything above ~64 almost certainly indicates a
/// corrupt TOC.
const MAX_PARTITIONS: usize = 64;

/// Classification of a Wii U disc partition by name prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartitionKind {
    /// System install. Holds per-title tickets, TMDs, and certs in a
    /// small FST.
    SystemInstall,
    /// Game partition. One per user-visible title on the disc.
    Game,
    /// System update partition.
    Update,
    /// DLC partition.
    Dlc,
    /// Any other kind we should not attempt to process.
    Other(String),
}

impl PartitionKind {
    fn classify(name: &str) -> Self {
        if name.starts_with("SI") {
            Self::SystemInstall
        } else if name.starts_with("GM") {
            Self::Game
        } else if name.starts_with("UP") {
            Self::Update
        } else if name.starts_with("UC") {
            Self::Dlc
        } else {
            Self::Other(name.to_string())
        }
    }
}

/// One partition entry from the disc TOC.
#[derive(Clone, Debug)]
pub struct PartitionEntry {
    /// Full name from the TOC (with trailing NULs stripped).
    pub name: String,
    /// Start sector on disc; multiply by [`SECTOR_SIZE`] for the
    /// absolute byte offset.
    pub start_sector: u64,
    /// Classification for ergonomic dispatch.
    pub kind: PartitionKind,
}

impl PartitionEntry {
    /// Absolute byte offset of this partition on the disc.
    pub fn byte_offset(&self) -> u64 {
        self.start_sector * SECTOR_SIZE as u64
    }
}

/// Parsed partition table: the full list of on-disc partitions in
/// table order.
#[derive(Clone, Debug, Default)]
pub struct PartitionTable {
    pub entries: Vec<PartitionEntry>,
}

impl PartitionTable {
    pub fn find_si(&self) -> Option<&PartitionEntry> {
        self.entries
            .iter()
            .find(|e| matches!(e.kind, PartitionKind::SystemInstall))
    }

    pub fn content_partitions(&self) -> impl Iterator<Item = &PartitionEntry> {
        self.entries.iter().filter(|e| {
            matches!(
                e.kind,
                PartitionKind::Game | PartitionKind::Update | PartitionKind::Dlc
            )
        })
    }
}

/// Read, decrypt, and parse the partition TOC from a disc image.
pub fn parse_partition_table(
    disc: &mut dyn DiscSectorSource,
    key: &DiscKey,
) -> WupResult<PartitionTable> {
    // The TOC lives at disc byte offset 0x18000 = sector 3.
    let sector_index = PARTITION_TOC_OFFSET / SECTOR_SIZE as u64;
    if sector_index >= disc.total_sectors() {
        return Err(WupError::DiscTruncated {
            expected: PARTITION_TOC_OFFSET + SECTOR_SIZE as u64,
            actual: disc.total_sectors() * SECTOR_SIZE as u64,
        });
    }
    let mut sector = vec![0u8; SECTOR_SIZE];
    disc.read_sector(sector_index, &mut sector)?;

    let iv = [0u8; 16];
    aes_cbc_decrypt_in_place(key.as_bytes(), &iv, &mut sector)?;

    if sector[0..4] != DECRYPTED_AREA_SIGNATURE {
        return Err(WupError::DiscKeyWrong);
    }

    let count = u32::from_be_bytes(
        sector[PARTITION_COUNT_OFFSET..PARTITION_COUNT_OFFSET + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    if count == 0 || count > MAX_PARTITIONS {
        return Err(WupError::InvalidPartitionHeader);
    }

    let entries_end = PARTITION_ENTRIES_OFFSET + count * PARTITION_ENTRY_SIZE;
    if entries_end > SECTOR_SIZE {
        return Err(WupError::InvalidPartitionHeader);
    }

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let base = PARTITION_ENTRIES_OFFSET + i * PARTITION_ENTRY_SIZE;
        let name = parse_partition_name(&sector[base..base + 0x19]);
        let start_sector =
            u32::from_be_bytes(sector[base + 0x20..base + 0x24].try_into().unwrap()) as u64;
        let kind = PartitionKind::classify(&name);
        entries.push(PartitionEntry {
            name,
            start_sector,
            kind,
        });
    }

    Ok(PartitionTable { entries })
}

fn parse_partition_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::disc::sector_stream::InMemoryDisc;
    use aes::Aes128;
    use aes::cipher::BlockEncryptMut;
    use aes::cipher::KeyIvInit;
    use block_padding::NoPadding;
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<Aes128>;

    /// Helper: build a disc image whose sector 3 (offset 0x18000) is
    /// an encrypted partition TOC with the given entries.
    fn build_disc_with_toc(key: &[u8; 16], entries: &[(&str, u32)]) -> Vec<u8> {
        let mut toc = vec![0u8; SECTOR_SIZE];
        toc[0..4].copy_from_slice(&DECRYPTED_AREA_SIGNATURE);
        toc[PARTITION_COUNT_OFFSET..PARTITION_COUNT_OFFSET + 4]
            .copy_from_slice(&(entries.len() as u32).to_be_bytes());
        for (i, (name, start_sector)) in entries.iter().enumerate() {
            let base = PARTITION_ENTRIES_OFFSET + i * PARTITION_ENTRY_SIZE;
            let name_bytes = name.as_bytes();
            let copy = name_bytes.len().min(0x19);
            toc[base..base + copy].copy_from_slice(&name_bytes[..copy]);
            toc[base + 0x20..base + 0x24].copy_from_slice(&start_sector.to_be_bytes());
        }

        // Encrypt the TOC sector.
        let iv = [0u8; 16];
        Aes128CbcEnc::new_from_slices(key, &iv)
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(&mut toc, SECTOR_SIZE)
            .unwrap();

        // Assemble full disc: sectors 0..3 zero, sector 3 is our TOC.
        let mut disc = vec![0u8; 4 * SECTOR_SIZE];
        disc[3 * SECTOR_SIZE..4 * SECTOR_SIZE].copy_from_slice(&toc);
        disc
    }

    #[test]
    fn classifies_known_prefixes() {
        assert_eq!(PartitionKind::classify("SI"), PartitionKind::SystemInstall);
        assert_eq!(PartitionKind::classify("GM12345678"), PartitionKind::Game);
        assert_eq!(PartitionKind::classify("UP"), PartitionKind::Update);
        assert_eq!(PartitionKind::classify("UC12345678"), PartitionKind::Dlc);
        assert!(matches!(
            PartitionKind::classify("XYZ"),
            PartitionKind::Other(_)
        ));
    }

    #[test]
    fn parses_minimal_toc() {
        let key = [0x33u8; 16];
        let disc_bytes = build_disc_with_toc(&key, &[("SI", 4), ("GM12345678", 10), ("UP", 100)]);
        let mut disc = InMemoryDisc::new(disc_bytes);
        let table = parse_partition_table(&mut disc, &DiscKey(key)).unwrap();
        assert_eq!(table.entries.len(), 3);
        assert_eq!(table.entries[0].name, "SI");
        assert_eq!(table.entries[0].start_sector, 4);
        assert!(matches!(
            table.entries[0].kind,
            PartitionKind::SystemInstall
        ));
        assert_eq!(table.entries[1].name, "GM12345678");
        assert_eq!(table.entries[1].start_sector, 10);
        assert_eq!(table.entries[1].byte_offset(), 10 * SECTOR_SIZE as u64);
        assert!(matches!(table.entries[1].kind, PartitionKind::Game));
    }

    #[test]
    fn wrong_key_triggers_disc_key_wrong() {
        let correct = [0x33u8; 16];
        let wrong = [0x44u8; 16];
        let disc_bytes = build_disc_with_toc(&correct, &[("SI", 4), ("GM00000001", 8)]);
        let mut disc = InMemoryDisc::new(disc_bytes);
        let result = parse_partition_table(&mut disc, &DiscKey(wrong));
        assert!(matches!(result, Err(WupError::DiscKeyWrong)));
    }

    #[test]
    fn zero_partitions_is_error() {
        let key = [0x33u8; 16];
        let disc_bytes = build_disc_with_toc(&key, &[]);
        let mut disc = InMemoryDisc::new(disc_bytes);
        let result = parse_partition_table(&mut disc, &DiscKey(key));
        assert!(matches!(result, Err(WupError::InvalidPartitionHeader)));
    }

    #[test]
    fn find_si_returns_system_partition() {
        let key = [0x33u8; 16];
        let disc_bytes = build_disc_with_toc(&key, &[("GM00000001", 8), ("SI", 4)]);
        let mut disc = InMemoryDisc::new(disc_bytes);
        let table = parse_partition_table(&mut disc, &DiscKey(key)).unwrap();
        let si = table.find_si().unwrap();
        assert_eq!(si.name, "SI");
    }

    #[test]
    fn content_partitions_iter_filters() {
        let key = [0x33u8; 16];
        let disc_bytes = build_disc_with_toc(
            &key,
            &[
                ("SI", 4),
                ("GM00000001", 8),
                ("UP", 12),
                ("XX", 16),
                ("UC00000001", 20),
            ],
        );
        let mut disc = InMemoryDisc::new(disc_bytes);
        let table = parse_partition_table(&mut disc, &DiscKey(key)).unwrap();
        let names: Vec<_> = table.content_partitions().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["GM00000001", "UP", "UC00000001"]);
    }
}
