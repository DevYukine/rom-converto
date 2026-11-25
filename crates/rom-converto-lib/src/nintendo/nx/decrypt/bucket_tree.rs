//! Bucket trees of a BKTR (update) NCA section: the AesCtrEx table
//! that names each run's AES-CTR generation and the indirect table that
//! maps the patched RomFS onto the base and patch sections.
//!
//! Layout follows the upstream `BucketTree` with a 0x4000-byte node
//! size: an L1 node (plus L2 nodes for very large tables) holding
//! entry-set start offsets, then one 0x4000-byte entry node per set,
//! each `{index i32, count i32, offset i64}` followed by `count`
//! fixed-size entries. Only the flat entry list and the L1 node's end
//! offset matter here; lookups binary-search the flat list.

use crate::util::bytes::{u32_le, u64_le};

const NODE_SIZE: usize = 0x4000;
const NODE_HEADER_SIZE: usize = 0x10;
const OFFSETS_PER_NODE: usize = (NODE_SIZE - NODE_HEADER_SIZE) / 8;

/// A fixed-size bucket tree entry whose first eight bytes hold the
/// offset where its run starts.
pub trait BucketEntry: Sized {
    /// Bytes one entry occupies in an entry node.
    const SIZE: usize;
    /// Most entries a table of this type may declare; also bounds the
    /// table read and the entry list for a corrupt count.
    const MAX_ENTRIES: u32;
    /// Decodes one raw entry, rejecting values its type does not allow.
    fn decode(raw: &[u8]) -> Result<Self, &'static str>;
    /// The offset where this entry's run starts.
    fn offset(&self) -> u64;
    #[cfg(test)]
    fn encode(&self, raw: &mut [u8]);
}

/// One AesCtrEx entry: the run starting at `offset` (relative to the
/// section) is either plaintext or CTR-encrypted under `generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AesCtrExEntry {
    pub offset: u64,
    pub encrypted: bool,
    pub generation: u32,
}

impl BucketEntry for AesCtrExEntry {
    const SIZE: usize = 0x10;
    // Real tables hold thousands of generation runs.
    const MAX_ENTRIES: u32 = 1 << 20;

    fn decode(raw: &[u8]) -> Result<Self, &'static str> {
        let offset = u64_le(raw, 0);
        if !offset.is_multiple_of(16) {
            return Err("AesCtrEx entry offset is not 16-byte aligned");
        }
        Ok(Self {
            offset,
            encrypted: raw[8] == 0,
            generation: u32_le(raw, 12),
        })
    }

    fn offset(&self) -> u64 {
        self.offset
    }

    #[cfg(test)]
    fn encode(&self, raw: &mut [u8]) {
        raw[0..8].copy_from_slice(&self.offset.to_le_bytes());
        raw[8] = u8::from(!self.encrypted);
        raw[12..16].copy_from_slice(&self.generation.to_le_bytes());
    }
}

/// One indirect entry: the virtual range starting at `virtual_offset`
/// reads storage `storage_index` (0 base, 1 patch) from
/// `physical_offset` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndirectEntry {
    pub virtual_offset: u64,
    pub physical_offset: u64,
    pub storage_index: u32,
}

impl BucketEntry for IndirectEntry {
    const SIZE: usize = 0x14;
    // An entry per 16 KiB run of a 64 GiB image.
    const MAX_ENTRIES: u32 = 1 << 22;

    fn decode(raw: &[u8]) -> Result<Self, &'static str> {
        let storage_index = u32_le(raw, 0x10);
        if storage_index > 1 {
            return Err("indirect entry names a storage other than base or patch");
        }
        Ok(Self {
            virtual_offset: u64_le(raw, 0),
            physical_offset: u64_le(raw, 8),
            storage_index,
        })
    }

    fn offset(&self) -> u64 {
        self.virtual_offset
    }

    #[cfg(test)]
    fn encode(&self, raw: &mut [u8]) {
        raw[0..8].copy_from_slice(&self.virtual_offset.to_le_bytes());
        raw[8..16].copy_from_slice(&self.physical_offset.to_le_bytes());
        raw[16..20].copy_from_slice(&self.storage_index.to_le_bytes());
    }
}

const fn entries_per_node<E: BucketEntry>() -> usize {
    (NODE_SIZE - NODE_HEADER_SIZE) / E::SIZE
}

/// Bytes the table for `entry_count` entries of type `E` occupies: the
/// L1 node, any L2 nodes, and one node per entry set. `None` when the
/// count is zero or implausibly large for an NCA section.
pub fn table_size<E: BucketEntry>(entry_count: u32) -> Option<u64> {
    if entry_count == 0 || entry_count > E::MAX_ENTRIES {
        return None;
    }
    let entry_set_count = (entry_count as usize).div_ceil(entries_per_node::<E>());
    let node_storage = (1 + l2_node_count(entry_set_count)) * NODE_SIZE;
    Some((node_storage + entry_set_count * NODE_SIZE) as u64)
}

/// Parses a decrypted bucket tree table into its entries, in offset
/// order, plus the end of the range they cover (the L1 node's end
/// offset, unchecked). `entry_count` comes from the bucket-tree header
/// stored in the FS header.
pub fn parse_entries<E: BucketEntry>(
    table: &[u8],
    entry_count: u32,
) -> Result<(Vec<E>, u64), &'static str> {
    let expected = table_size::<E>(entry_count).ok_or("bucket tree entry count is out of range")?;
    if (table.len() as u64) < expected {
        return Err("bucket tree table is smaller than its node count requires");
    }
    let per_node = entries_per_node::<E>();
    let entry_count = entry_count as usize;
    let entry_set_count = entry_count.div_ceil(per_node);
    let node_storage = (1 + l2_node_count(entry_set_count)) * NODE_SIZE;
    // Grows only as entry nodes validate, so a corrupt count cannot
    // reserve memory up front.
    let mut entries = Vec::new();
    for set in 0..entry_set_count {
        let node = &table[node_storage + set * NODE_SIZE..][..NODE_SIZE];
        let count = u32_le(node, 4) as usize;
        if count == 0 || count > per_node || entries.len() + count > entry_count {
            return Err("bucket tree entry node has an invalid entry count");
        }
        for i in 0..count {
            entries.push(E::decode(
                &node[NODE_HEADER_SIZE + i * E::SIZE..][..E::SIZE],
            )?);
        }
    }
    if entries.len() != entry_count {
        return Err("bucket tree entry nodes do not hold the header's entry count");
    }
    // Bytes before the first entry would otherwise belong to no run.
    if entries[0].offset() != 0 {
        return Err("bucket tree first entry does not start at offset 0");
    }
    if !entries.windows(2).all(|w| w[0].offset() < w[1].offset()) {
        return Err("bucket tree entries are not in ascending offset order");
    }
    Ok((entries, u64_le(table, 8)))
}

/// Number of L2 offset nodes between the L1 node and the entry nodes,
/// mirroring `BucketTree::GetNodeL2Count`.
fn l2_node_count(entry_set_count: usize) -> usize {
    if entry_set_count <= OFFSETS_PER_NODE {
        return 0;
    }
    let l2 = entry_set_count.div_ceil(OFFSETS_PER_NODE);
    (entry_set_count - (OFFSETS_PER_NODE - (l2 - 1))).div_ceil(OFFSETS_PER_NODE)
}

/// Serializes `entries` as a bucket tree table whose L1 node ends at `end`.
#[cfg(test)]
pub(crate) fn build_table<E: BucketEntry>(entries: &[E], end: u64) -> Vec<u8> {
    let per_node = entries_per_node::<E>();
    let entry_set_count = entries.len().div_ceil(per_node);
    let node_storage = (1 + l2_node_count(entry_set_count)) * NODE_SIZE;
    let mut table = vec![0u8; node_storage + entry_set_count * NODE_SIZE];
    // L1 node: count of entry sets plus each set's first offset.
    table[4..8].copy_from_slice(&(entry_set_count as u32).to_le_bytes());
    table[8..16].copy_from_slice(&end.to_le_bytes());
    for (set, chunk) in entries.chunks(per_node).enumerate() {
        table[NODE_HEADER_SIZE + set * 8..][..8].copy_from_slice(&chunk[0].offset().to_le_bytes());
        let node = &mut table[node_storage + set * NODE_SIZE..][..NODE_SIZE];
        node[0..4].copy_from_slice(&(set as u32).to_le_bytes());
        node[4..8].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
        for (i, e) in chunk.iter().enumerate() {
            e.encode(&mut node[NODE_HEADER_SIZE + i * E::SIZE..][..E::SIZE]);
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(offset: u64, encrypted: bool, generation: u32) -> AesCtrExEntry {
        AesCtrExEntry {
            offset,
            encrypted,
            generation,
        }
    }

    #[test]
    fn round_trips_entries_across_multiple_nodes() {
        let entries: Vec<AesCtrExEntry> = (0..(entries_per_node::<AesCtrExEntry>() as u64 * 2 + 5))
            .map(|i| entry(i * 0x1000, i % 3 != 0, (i % 7) as u32))
            .collect();
        let table = build_table(&entries, 0x9_0000);
        let (parsed, end) = parse_entries::<AesCtrExEntry>(&table, entries.len() as u32).unwrap();
        assert_eq!(parsed, entries);
        assert_eq!(end, 0x9_0000);
    }

    #[test]
    fn rejects_unaligned_or_unordered_offsets() {
        let table = build_table(&[entry(0, true, 1), entry(0x1008, true, 2)], 0);
        assert!(parse_entries::<AesCtrExEntry>(&table, 2).is_err());
        let table = build_table(
            &[
                entry(0, true, 1),
                entry(0x2000, true, 2),
                entry(0x1000, true, 3),
            ],
            0,
        );
        assert!(parse_entries::<AesCtrExEntry>(&table, 3).is_err());
    }

    #[test]
    fn rejects_count_mismatch() {
        let table = build_table(&[entry(0, true, 1), entry(0x1000, true, 2)], 0);
        assert!(parse_entries::<AesCtrExEntry>(&table, 3).is_err());
    }

    #[test]
    fn rejects_table_not_starting_at_zero() {
        let table = build_table(&[entry(0x1000, true, 1), entry(0x2000, true, 2)], 0);
        assert!(parse_entries::<AesCtrExEntry>(&table, 2).is_err());
    }

    #[test]
    fn indirect_entries_round_trip_and_reject_unknown_storage() {
        let entries: Vec<IndirectEntry> = (0..2000u64)
            .map(|i| IndirectEntry {
                virtual_offset: i * 0x20,
                physical_offset: i * 7,
                storage_index: (i % 2) as u32,
            })
            .collect();
        let table = build_table(&entries, 2000 * 0x20);
        assert_eq!(table_size::<IndirectEntry>(2000), Some(table.len() as u64));
        assert!(table_size::<IndirectEntry>((1 << 22) + 1).is_none());
        assert!(table_size::<AesCtrExEntry>(1 << 20).is_some());
        assert!(table_size::<AesCtrExEntry>((1 << 20) + 1).is_none());
        let (parsed, end) = parse_entries::<IndirectEntry>(&table, 2000).unwrap();
        assert_eq!(parsed, entries);
        assert_eq!(end, 2000 * 0x20);

        let mut bad = entries[..2].to_vec();
        bad[1].storage_index = 2;
        let table = build_table(&bad, 0x40);
        assert!(parse_entries::<IndirectEntry>(&table, 2).is_err());
    }
}
