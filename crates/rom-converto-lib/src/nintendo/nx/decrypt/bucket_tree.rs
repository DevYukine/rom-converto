//! AesCtrEx bucket tree of a BKTR (update) NCA section.
//!
//! Layout follows the upstream `BucketTree` with a 0x4000-byte node
//! size: an L1 node (plus L2 nodes for very large tables) holding
//! entry-set start offsets, then one 0x4000-byte entry node per set,
//! each `{index i32, count i32, offset i64}` followed by `count`
//! 0x10-byte entries. Only the flat entry list matters here: every
//! entry names the section offset where a new AES-CTR generation (or
//! an unencrypted run) begins.

use crate::util::bytes::{u32_le, u64_le};

const NODE_SIZE: usize = 0x4000;
const NODE_HEADER_SIZE: usize = 0x10;
const ENTRY_SIZE: usize = 0x10;
const ENTRIES_PER_NODE: usize = (NODE_SIZE - NODE_HEADER_SIZE) / ENTRY_SIZE;
const OFFSETS_PER_NODE: usize = (NODE_SIZE - NODE_HEADER_SIZE) / 8;

/// One AesCtrEx entry: the run starting at `offset` (relative to the
/// section) is either plaintext or CTR-encrypted under `generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AesCtrExEntry {
    pub offset: u64,
    pub encrypted: bool,
    pub generation: u32,
}

/// Bytes the table for `entry_count` entries occupies: the L1 node,
/// any L2 nodes, and one node per entry set. `None` when the count is
/// zero or implausibly large for an NCA section.
pub fn table_size(entry_count: u32) -> Option<u64> {
    const MAX_ENTRIES: u32 = 1 << 24;
    if entry_count == 0 || entry_count > MAX_ENTRIES {
        return None;
    }
    let entry_set_count = (entry_count as usize).div_ceil(ENTRIES_PER_NODE);
    let node_storage = (1 + l2_node_count(entry_set_count)) * NODE_SIZE;
    Some((node_storage + entry_set_count * NODE_SIZE) as u64)
}

/// Parses the decrypted AesCtrEx table into its entries, in offset
/// order. `entry_count` comes from the bucket-tree header stored in
/// the FS header.
pub fn parse_entries(table: &[u8], entry_count: u32) -> Result<Vec<AesCtrExEntry>, &'static str> {
    let expected = table_size(entry_count).ok_or("AesCtrEx entry count is out of range")?;
    if (table.len() as u64) < expected {
        return Err("AesCtrEx table is smaller than its node count requires");
    }
    let entry_count = entry_count as usize;
    let entry_set_count = entry_count.div_ceil(ENTRIES_PER_NODE);
    let node_storage = (1 + l2_node_count(entry_set_count)) * NODE_SIZE;
    let mut entries = Vec::with_capacity(entry_count);
    for set in 0..entry_set_count {
        let node = &table[node_storage + set * NODE_SIZE..][..NODE_SIZE];
        let count = u32_le(node, 4) as usize;
        if count == 0 || count > ENTRIES_PER_NODE {
            return Err("AesCtrEx entry node has an invalid entry count");
        }
        for i in 0..count {
            let raw = &node[NODE_HEADER_SIZE + i * ENTRY_SIZE..][..ENTRY_SIZE];
            entries.push(AesCtrExEntry {
                offset: u64_le(raw, 0),
                encrypted: raw[8] == 0,
                generation: u32_le(raw, 12),
            });
        }
    }
    if entries.len() != entry_count {
        return Err("AesCtrEx entry nodes do not hold the header's entry count");
    }
    // Bytes before the first entry would otherwise stay ciphertext
    // silently, since the caller decrypts only from the first offset on.
    if entries[0].offset != 0 {
        return Err("AesCtrEx first entry does not start at offset 0");
    }
    let ordered = entries.iter().all(|e| e.offset.is_multiple_of(16))
        && entries.windows(2).all(|w| w[0].offset < w[1].offset);
    if !ordered {
        return Err("AesCtrEx entries are not ascending 16-byte aligned offsets");
    }
    Ok(entries)
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

#[cfg(test)]
pub(crate) fn build_table(entries: &[AesCtrExEntry]) -> Vec<u8> {
    let entry_set_count = entries.len().div_ceil(ENTRIES_PER_NODE);
    let node_storage = (1 + l2_node_count(entry_set_count)) * NODE_SIZE;
    let mut table = vec![0u8; node_storage + entry_set_count * NODE_SIZE];
    // L1 node: count of entry sets plus each set's first offset.
    table[4..8].copy_from_slice(&(entry_set_count as u32).to_le_bytes());
    for (set, chunk) in entries.chunks(ENTRIES_PER_NODE).enumerate() {
        table[NODE_HEADER_SIZE + set * 8..][..8].copy_from_slice(&chunk[0].offset.to_le_bytes());
        let node = &mut table[node_storage + set * NODE_SIZE..][..NODE_SIZE];
        node[0..4].copy_from_slice(&(set as u32).to_le_bytes());
        node[4..8].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
        for (i, e) in chunk.iter().enumerate() {
            let raw = &mut node[NODE_HEADER_SIZE + i * ENTRY_SIZE..][..ENTRY_SIZE];
            raw[0..8].copy_from_slice(&e.offset.to_le_bytes());
            raw[8] = u8::from(!e.encrypted);
            raw[12..16].copy_from_slice(&e.generation.to_le_bytes());
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
        let entries: Vec<AesCtrExEntry> = (0..(ENTRIES_PER_NODE as u64 * 2 + 5))
            .map(|i| entry(i * 0x1000, i % 3 != 0, (i % 7) as u32))
            .collect();
        let table = build_table(&entries);
        let parsed = parse_entries(&table, entries.len() as u32).unwrap();
        assert_eq!(parsed, entries);
    }

    #[test]
    fn rejects_unaligned_or_unordered_offsets() {
        let table = build_table(&[entry(0, true, 1), entry(0x1008, true, 2)]);
        assert!(parse_entries(&table, 2).is_err());
        let table = build_table(&[
            entry(0, true, 1),
            entry(0x2000, true, 2),
            entry(0x1000, true, 3),
        ]);
        assert!(parse_entries(&table, 3).is_err());
    }

    #[test]
    fn rejects_count_mismatch() {
        let table = build_table(&[entry(0, true, 1), entry(0x1000, true, 2)]);
        assert!(parse_entries(&table, 3).is_err());
    }

    #[test]
    fn rejects_table_not_starting_at_zero() {
        let table = build_table(&[entry(0x1000, true, 1), entry(0x2000, true, 2)]);
        assert!(parse_entries(&table, 2).is_err());
    }
}
