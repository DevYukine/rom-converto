//! Wii partition pipeline: parse partition headers, decrypt clusters,
//! recompute the H0/H1/H2 hash hierarchy, and build the per-chunk exception
//! list that RVZ uses to round-trip a partition byte-for-byte.
//!
//! ## Layout reminder
//!
//! - **Sector / block:** 0x8000 bytes encrypted = 0x400 hash region + 0x7C00
//!   plaintext payload.
//! - **Cluster / group:** 64 sectors. Plaintext = 64 × 0x7C00 = 0x1F0000 bytes.
//!   Encrypted = 64 × 0x8000 = 0x200000 bytes (2 MiB).
//! - **Hash region (per sector):** 31 SHA-1s of the 0x400-byte sub-blocks of
//!   that sector's payload (h0), then 8 SHA-1s of the 8 sibling sectors' h0
//!   arrays in the same sub-group (h1), then 8 SHA-1s of the 8 sub-groups' h1
//!   arrays in the same group (h2), with padding fields between each tier.
//!
//! See `Source/Core/DiscIO/VolumeWii.h` in Dolphin for the canonical layout.

use crate::nintendo::rvl::constants::{
    WII_BLOCKS_PER_GROUP, WII_GROUP_PAYLOAD_SIZE, WII_GROUP_TOTAL_SIZE, WII_HASH_SIZE,
    WII_PARTITION_HEADER_DATA_OFFSET_OFFSET, WII_PARTITION_HEADER_DATA_SIZE_OFFSET,
    WII_PARTITION_HEADER_SIZE, WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE, WII_SECTOR_SIZE_U64,
    WII_TICKET_SIZE,
};
use crate::nintendo::rvl::disc::{decrypt_sector, decrypt_title_key, encrypt_sector};
use crate::nintendo::rvz::error::{RvzError, RvzResult};
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom};

/// Number of H0 hashes per sector (each covers a 0x400-byte sub-block of
/// the 0x7C00-byte payload).
pub const H0_PER_SECTOR: usize = 31;

/// Number of H1 hashes per sub-group (one per sibling sector).
pub const H1_PER_SUBGROUP: usize = 8;

/// Number of sub-groups per cluster.
pub const SUBGROUPS_PER_GROUP: usize = 8;

const H0_BLOCK_SIZE: usize = 0x400;

/// Sector-position math for one RVZ partition chunk inside its
/// containing cluster. Centralises the
/// `enc_pos / WII_GROUP_TOTAL_SIZE` and
/// `offset_in_cluster / WII_SECTOR_SIZE` arithmetic so encoder
/// and decoder don't each hand-roll (and mis-roll) it.
///
/// "Enc" prefix = **encrypted-byte coordinates**: the units
/// Dolphin's `data_size` and `chunk_size` live in. One sector
/// = 0x8000 enc bytes, one cluster = 0x200000 enc bytes. The
/// caller supplies `enc_pos` (byte offset from the partition's
/// declared data start) and `this_chunk_enc_bytes` (the chunk's
/// size in enc bytes; < `chunk_size_u64` only for the partial
/// last chunk), and [`ChunkSectorPos::new`] derives the cluster
/// index, the first sector within that cluster, and the sector
/// count the chunk covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkSectorPos {
    /// Which cluster inside the partition this chunk lives in
    /// (0-based). Multiply by [`WII_GROUP_TOTAL_SIZE`] to get
    /// the cluster's first enc-byte offset.
    pub cluster_idx: u64,
    /// First sector inside the cluster this chunk covers (0..64).
    pub first_sector_in_chunk: usize,
    /// How many sectors the chunk covers. `<=
    /// WII_BLOCKS_PER_GROUP - first_sector_in_chunk`.
    pub chunk_n_sectors: usize,
}

impl ChunkSectorPos {
    /// Derive the cluster + sector range for a chunk starting at
    /// `enc_pos` (enc-byte offset from partition data start) and
    /// covering `this_chunk_enc_bytes` encrypted bytes.
    ///
    /// Panics in debug if the chunk would straddle a cluster
    /// boundary; that's a caller bug, all partition chunks are
    /// nested inside one cluster by construction.
    #[inline]
    pub fn new(enc_pos: u64, this_chunk_enc_bytes: u64) -> Self {
        let cluster_idx = enc_pos / WII_GROUP_TOTAL_SIZE;
        let offset_in_cluster_enc = enc_pos % WII_GROUP_TOTAL_SIZE;
        let first_sector_in_chunk = (offset_in_cluster_enc / WII_SECTOR_SIZE_U64) as usize;
        let chunk_n_sectors = (this_chunk_enc_bytes / WII_SECTOR_SIZE_U64) as usize;
        debug_assert!(chunk_n_sectors > 0);
        debug_assert!(first_sector_in_chunk + chunk_n_sectors <= WII_BLOCKS_PER_GROUP);
        Self {
            cluster_idx,
            first_sector_in_chunk,
            chunk_n_sectors,
        }
    }

    /// Plaintext payload position for [`pack_encode`] /
    /// [`pack_decode`]. Plaintext cluster 0 starts at 0,
    /// cluster 1 at 0x1F0000, etc.
    ///
    /// [`pack_encode`]: crate::nintendo::rvz::packing::pack_encode
    /// [`pack_decode`]: crate::nintendo::rvz::packing::pack_decode
    #[inline]
    pub fn chunk_data_offset_pay(&self) -> u64 {
        self.cluster_idx * WII_GROUP_PAYLOAD_SIZE
            + (self.first_sector_in_chunk as u64) * (WII_SECTOR_PAYLOAD_SIZE as u64)
    }

    /// Plaintext payload length this chunk covers in bytes:
    /// `chunk_n_sectors * WII_SECTOR_PAYLOAD_SIZE`. Used to size
    /// scratch buffers before `pack_encode` / `pack_decode`.
    #[inline]
    pub fn payload_len(&self) -> usize {
        self.chunk_n_sectors * WII_SECTOR_PAYLOAD_SIZE
    }
}

/// Size in bytes of one sector's hash region.
pub const HASH_REGION_BYTES: usize = WII_HASH_SIZE;

/// Field offsets inside a sector's 0x400 hash region. Match Dolphin's
/// `VolumeWii::HashBlock`.
mod hash_region {
    pub const H0_OFFSET: usize = 0;
    pub const H0_LEN: usize = 31 * 20;
    pub const PADDING_0_OFFSET: usize = H0_OFFSET + H0_LEN;
    pub const PADDING_0_LEN: usize = 20;
    pub const H1_OFFSET: usize = PADDING_0_OFFSET + PADDING_0_LEN;
    pub const H1_LEN: usize = 8 * 20;
    pub const PADDING_1_OFFSET: usize = H1_OFFSET + H1_LEN;
    pub const PADDING_1_LEN: usize = 32;
    pub const H2_OFFSET: usize = PADDING_1_OFFSET + PADDING_1_LEN;
    pub const H2_LEN: usize = 8 * 20;
    pub const PADDING_2_OFFSET: usize = H2_OFFSET + H2_LEN;
    pub const PADDING_2_LEN: usize = 32;
    pub const TOTAL: usize = PADDING_2_OFFSET + PADDING_2_LEN;
}

const _: () = assert!(
    hash_region::TOTAL == 0x400,
    "hash region layout must be 0x400"
);

/// Metadata for one Wii partition extracted from its header.
#[derive(Debug, Clone, Copy)]
pub struct PartitionInfo {
    /// Byte offset of the partition inside the raw ISO.
    pub partition_offset: u64,
    /// Partition group index (0..4).
    pub group_index: u8,
    /// Partition type (0 = game, 1 = update, 2 = channel).
    pub partition_type: u32,
    /// Decrypted title key.
    pub title_key: [u8; 16],
    /// Byte offset (relative to `partition_offset`) where encrypted data
    /// starts.
    pub data_offset: u64,
    /// Bytes of encrypted data in the partition.
    pub data_size: u64,
}

impl PartitionInfo {
    /// Number of full clusters in the partition's encrypted data area.
    /// Number of 2 MiB clusters the partition occupies on disc.
    ///
    /// Real Wii discs can declare `data_size` values that aren't a
    /// whole number of clusters. The partition's encrypted storage
    /// still always spans complete clusters on the physical disc,
    /// with the tail of the final cluster carrying junk/padding. We
    /// round up so both encode and decode process every on-disc
    /// cluster; Dolphin does the same via `align_up(data_size,
    /// GROUP_TOTAL_SIZE)` in `WIABlob.cpp`.
    pub fn cluster_count(&self) -> u64 {
        self.data_size.div_ceil(WII_GROUP_TOTAL_SIZE)
    }

    /// Absolute byte offset where encrypted data starts.
    pub fn data_start(&self) -> u64 {
        self.partition_offset + self.data_offset
    }
}

/// Read a partition's header (ticket + offsets) from the input ISO. The
/// reader is left at an unspecified position.
pub fn read_partition_info<R: Read + Seek>(
    reader: &mut R,
    partition_offset: u64,
    group_index: u8,
    partition_type: u32,
) -> RvzResult<PartitionInfo> {
    reader.seek(SeekFrom::Start(partition_offset))?;
    let mut header = [0u8; WII_PARTITION_HEADER_SIZE];
    reader.read_exact(&mut header)?;

    let mut ticket = [0u8; WII_TICKET_SIZE];
    ticket.copy_from_slice(&header[..WII_TICKET_SIZE]);

    let title_key = decrypt_title_key(&ticket)?;

    let data_offset_word = u32::from_be_bytes(
        header
            [WII_PARTITION_HEADER_DATA_OFFSET_OFFSET..WII_PARTITION_HEADER_DATA_OFFSET_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    let data_size_word = u32::from_be_bytes(
        header[WII_PARTITION_HEADER_DATA_SIZE_OFFSET..WII_PARTITION_HEADER_DATA_SIZE_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    let data_offset = (data_offset_word as u64) << 2;
    let data_size = (data_size_word as u64) << 2;

    // Dolphin's `WIABlob.cpp` rounds `data_size` up to a multiple of
    // `GROUP_TOTAL_SIZE` when computing how many clusters to process.
    // We mirror that by relaxing the strict alignment check: real
    // partitions frequently carry a short tail beyond the declared
    // data_size, and the last cluster's extra bytes are just junk
    // padding that still needs to flow through encrypt/decrypt.

    Ok(PartitionInfo {
        partition_offset,
        group_index,
        partition_type,
        title_key,
        data_offset,
        data_size,
    })
}

/// One decrypted cluster: 64 sectors split into hash regions and payloads.
pub struct DecryptedCluster {
    pub on_disc_hash_regions: Vec<[u8; HASH_REGION_BYTES]>,
    pub payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>,
}

impl Default for DecryptedCluster {
    fn default() -> Self {
        Self {
            on_disc_hash_regions: Vec::with_capacity(WII_BLOCKS_PER_GROUP),
            payloads: Vec::with_capacity(WII_BLOCKS_PER_GROUP),
        }
    }
}

impl DecryptedCluster {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Read one cluster (64 encrypted sectors), decrypt them, and split each
/// sector into its 0x400 hash region + 0x7C00 payload. The reader must be
/// seeked to the cluster's start before calling.
pub fn read_and_decrypt_cluster<R: Read>(
    reader: &mut R,
    title_key: &[u8; 16],
) -> RvzResult<DecryptedCluster> {
    let mut cluster = DecryptedCluster::new();
    let mut sector = [0u8; WII_SECTOR_SIZE];

    for _ in 0..WII_BLOCKS_PER_GROUP {
        reader.read_exact(&mut sector)?;
        decrypt_sector(&mut sector, title_key)?;

        let mut hash = [0u8; HASH_REGION_BYTES];
        hash.copy_from_slice(&sector[..HASH_REGION_BYTES]);
        let mut payload = [0u8; WII_SECTOR_PAYLOAD_SIZE];
        payload.copy_from_slice(&sector[HASH_REGION_BYTES..]);

        cluster.on_disc_hash_regions.push(hash);
        cluster.payloads.push(payload);
    }
    Ok(cluster)
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize().into()
}

/// Compute one sector's H0 array (31 SHA-1s of 0x400-byte sub-blocks).
pub fn compute_h0(payload: &[u8; WII_SECTOR_PAYLOAD_SIZE]) -> [[u8; 20]; H0_PER_SECTOR] {
    let mut h0 = [[0u8; 20]; H0_PER_SECTOR];
    for (i, h) in h0.iter_mut().enumerate() {
        *h = sha1(&payload[i * H0_BLOCK_SIZE..(i + 1) * H0_BLOCK_SIZE]);
    }
    h0
}

/// Compute one sub-group's H1 array. Each entry is the SHA-1 of one sibling
/// sector's full H0 array (31 × 20 = 620 bytes).
pub fn compute_h1(
    subgroup_h0: &[[[u8; 20]; H0_PER_SECTOR]; H1_PER_SUBGROUP],
) -> [[u8; 20]; H1_PER_SUBGROUP] {
    let mut h1 = [[0u8; 20]; H1_PER_SUBGROUP];
    let mut buf = [0u8; H0_PER_SECTOR * 20];
    for (i, h) in h1.iter_mut().enumerate() {
        for (j, hash) in subgroup_h0[i].iter().enumerate() {
            buf[j * 20..(j + 1) * 20].copy_from_slice(hash);
        }
        *h = sha1(&buf);
    }
    h1
}

/// Compute one group's H2 array. Each entry is the SHA-1 of one sub-group's
/// full H1 array (8 × 20 = 160 bytes).
pub fn compute_h2(
    group_h1: &[[[u8; 20]; H1_PER_SUBGROUP]; SUBGROUPS_PER_GROUP],
) -> [[u8; 20]; SUBGROUPS_PER_GROUP] {
    let mut h2 = [[0u8; 20]; SUBGROUPS_PER_GROUP];
    let mut buf = [0u8; H1_PER_SUBGROUP * 20];
    for (i, h) in h2.iter_mut().enumerate() {
        for (j, hash) in group_h1[i].iter().enumerate() {
            buf[j * 20..(j + 1) * 20].copy_from_slice(hash);
        }
        *h = sha1(&buf);
    }
    h2
}

/// Allocate a fresh `Vec<[u8; HASH_REGION_BYTES]>` and fill it via
/// [`recompute_hash_regions_into`]. Convenience for tests and one-
/// shot callers; production RVZ worker pools call the `_into`
/// variant with persistent scratch to avoid per-cluster allocs.
pub fn recompute_hash_regions(
    payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
) -> Vec<[u8; HASH_REGION_BYTES]> {
    let mut regions = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
    recompute_hash_regions_into(payloads, &mut regions);
    regions
}

/// Recompute the Wii H0/H1/H2 hash hierarchy for one 2 MiB cluster,
/// writing every sector's assembled hash region into `out`. Zero
/// heap allocations on the hot path; callers hoist `out` into
/// persistent worker scratch and reuse it across every cluster.
///
/// Expects `payloads.len() == out.len() == WII_BLOCKS_PER_GROUP` (64).
/// The output bytes are overwritten in place; previous contents are
/// not read.
pub fn recompute_hash_regions_into(
    payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
    out: &mut [[u8; HASH_REGION_BYTES]],
) {
    debug_assert_eq!(payloads.len(), WII_BLOCKS_PER_GROUP);
    debug_assert_eq!(out.len(), WII_BLOCKS_PER_GROUP);

    // H0 per sector. `[[u8; 20]; H0_PER_SECTOR]` is 620 bytes so the
    // fixed-size array copies are cheap even on the stack.
    let mut all_h0 = [[[0u8; 20]; H0_PER_SECTOR]; WII_BLOCKS_PER_GROUP];
    for (i, payload) in payloads.iter().enumerate() {
        all_h0[i] = compute_h0(payload);
    }

    // H1 per sub-group.
    let mut all_h1 = [[[0u8; 20]; H1_PER_SUBGROUP]; SUBGROUPS_PER_GROUP];
    for sg in 0..SUBGROUPS_PER_GROUP {
        let mut subgroup_h0 = [[[0u8; 20]; H0_PER_SECTOR]; H1_PER_SUBGROUP];
        for s in 0..H1_PER_SUBGROUP {
            subgroup_h0[s] = all_h0[sg * H1_PER_SUBGROUP + s];
        }
        all_h1[sg] = compute_h1(&subgroup_h0);
    }

    // H2 for the whole group.
    let h2 = compute_h2(&all_h1);

    // Stitch every sector's hash region together in place.
    for sector_idx in 0..WII_BLOCKS_PER_GROUP {
        let subgroup_idx = sector_idx / H1_PER_SUBGROUP;
        let region = &mut out[sector_idx];
        // Wipe. We re-fill every byte below, but the caller may
        // hand us a dirty buffer from the previous cluster.
        *region = [0u8; HASH_REGION_BYTES];

        // h0 for this sector
        for (j, hash) in all_h0[sector_idx].iter().enumerate() {
            region[hash_region::H0_OFFSET + j * 20..hash_region::H0_OFFSET + (j + 1) * 20]
                .copy_from_slice(hash);
        }
        // h1 shared across the sub-group
        for (j, hash) in all_h1[subgroup_idx].iter().enumerate() {
            region[hash_region::H1_OFFSET + j * 20..hash_region::H1_OFFSET + (j + 1) * 20]
                .copy_from_slice(hash);
        }
        // h2 shared across the entire group
        for (j, hash) in h2.iter().enumerate() {
            region[hash_region::H2_OFFSET + j * 20..hash_region::H2_OFFSET + (j + 1) * 20]
                .copy_from_slice(hash);
        }
    }
}

/// One entry in a Dolphin `wia_except_list_t`. Matches Dolphin's
/// `HashExceptionEntry` in `Source/Core/DiscIO/WIABlob.h`:
/// `{ u16 offset; SHA1::Digest hash; }`, 22 bytes on disc, big-endian.
///
/// `offset` is the byte position of the 20-byte SHA-1 inside the chunk's
/// packed hash data, where block N starts at `N * BLOCK_HEADER_SIZE`
/// (0x400). For our single-cluster-per-chunk configuration this maxes at
/// `63 * 0x400 + 0x3E0 = 0xFFE0`, which fits in u16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashException {
    pub offset: u16,
    pub hash: [u8; 20],
}

/// Dolphin's documented cap on exceptions per `wia_except_list_t`.
pub const MAX_HASH_EXCEPTIONS_PER_CHUNK: usize = 3328;

/// Port of Dolphin's `compare_hashes` lambda from `WIABlob.cpp`. Iterates
/// SHA-1-sized slices of a hash-region field, emitting a `HashException`
/// whenever the on-disc bytes diverge from the recomputed ones.
///
/// The `min(l, size - 20)` clamp on the last iteration handles fields
/// whose size isn't a multiple of 20 (`padding_1`, `padding_2` are 32
/// bytes). This produces the 8-byte overlap Dolphin's encoder emits, and
/// the applier on the decoder side accepts it because later overlapping
/// writes are identical to earlier ones.
fn compare_hashes(
    on_disc: &[u8; HASH_REGION_BYTES],
    recomputed: &[u8; HASH_REGION_BYTES],
    region_base: u16,
    field_start: usize,
    field_size: usize,
    out: &mut Vec<HashException>,
) {
    debug_assert!(field_size >= 20);
    let mut l = 0;
    while l < field_size {
        let slice_start = field_start + l.min(field_size - 20);
        let desired = &on_disc[slice_start..slice_start + 20];
        let computed = &recomputed[slice_start..slice_start + 20];
        if desired != computed {
            let mut hash = [0u8; 20];
            hash.copy_from_slice(desired);
            out.push(HashException {
                offset: region_base + slice_start as u16,
                hash,
            });
        }
        l += 20;
    }
}

/// Build the Dolphin-format hash exception list for one Wii cluster.
///
/// This ports the per-block exception-building loop from `WIABlob.cpp`
/// in `dolphin-emu/dolphin`. For each of the 64 blocks in a cluster we
/// compare the on-disc hash region (plaintext, post-decryption) against
/// the recomputed one tier-by-tier (h0, padding_0, h1, padding_1, h2,
/// padding_2), in 20-byte slices.
///
/// `region_base` for block `j` is `j * BLOCK_HEADER_SIZE` (0x400), not
/// `j * BLOCK_TOTAL_SIZE`. The exception offsets are into the chunk's
/// packed hash region, not into raw sector bytes.
pub fn build_hash_exceptions(
    cluster: &DecryptedCluster,
    reconstructed: &[[u8; HASH_REGION_BYTES]],
) -> Vec<HashException> {
    let mut out = Vec::new();
    for (j, (on_disc, recon)) in cluster
        .on_disc_hash_regions
        .iter()
        .zip(reconstructed.iter())
        .enumerate()
        .take(WII_BLOCKS_PER_GROUP)
    {
        let region_base = (j * HASH_REGION_BYTES) as u16;

        compare_hashes(
            on_disc,
            recon,
            region_base,
            hash_region::H0_OFFSET,
            hash_region::H0_LEN,
            &mut out,
        );
        compare_hashes(
            on_disc,
            recon,
            region_base,
            hash_region::PADDING_0_OFFSET,
            hash_region::PADDING_0_LEN,
            &mut out,
        );
        compare_hashes(
            on_disc,
            recon,
            region_base,
            hash_region::H1_OFFSET,
            hash_region::H1_LEN,
            &mut out,
        );
        compare_hashes(
            on_disc,
            recon,
            region_base,
            hash_region::PADDING_1_OFFSET,
            hash_region::PADDING_1_LEN,
            &mut out,
        );
        compare_hashes(
            on_disc,
            recon,
            region_base,
            hash_region::H2_OFFSET,
            hash_region::H2_LEN,
            &mut out,
        );
        compare_hashes(
            on_disc,
            recon,
            region_base,
            hash_region::PADDING_2_OFFSET,
            hash_region::PADDING_2_LEN,
            &mut out,
        );
    }
    out
}

/// Apply a Dolphin-format exception list to a freshly-reconstructed set
/// of hash regions. Each exception overwrites 20 bytes starting at the
/// stored offset. Overlapping writes (see `compare_hashes`) are handled
/// by letting later writes take precedence; since both slices contain
/// the same on-disc bytes the result is deterministic either way.
pub fn apply_hash_exceptions(
    regions: &mut [[u8; HASH_REGION_BYTES]],
    exceptions: &[HashException],
) {
    for ex in exceptions {
        let offset = ex.offset as usize;
        let block_idx = offset / HASH_REGION_BYTES;
        let byte_in_block = offset % HASH_REGION_BYTES;
        if block_idx >= regions.len() || byte_in_block + 20 > HASH_REGION_BYTES {
            continue;
        }
        regions[block_idx][byte_in_block..byte_in_block + 20].copy_from_slice(&ex.hash);
    }
}

/// Project cluster-relative exceptions onto one sub-chunk's block range
/// and re-number them to be chunk-local.
///
/// When `chunk_size < WII_GROUP_TOTAL_SIZE`, one Wii cluster spans
/// `chunks_per_cluster = WII_GROUP_TOTAL_SIZE / chunk_size` chunks, each
/// covering `blocks_per_chunk = 64 / chunks_per_cluster` blocks. Dolphin
/// computes exception offsets using `block_index_in_chunk * 0x400`, so
/// each chunk's exceptions live in `[0, blocks_per_chunk * 0x400)`.
/// This helper takes a full cluster's exceptions (cluster-local offsets
/// in `[0, 64 * 0x400)`) and returns the subset belonging to chunk
/// `chunk_idx_in_cluster`, re-numbered to be chunk-local.
pub fn split_chunk_exceptions(
    cluster_exceptions: &[HashException],
    chunk_idx_in_cluster: usize,
    blocks_per_chunk: usize,
) -> Vec<HashException> {
    split_chunk_exceptions_by_range(
        cluster_exceptions,
        chunk_idx_in_cluster * blocks_per_chunk,
        blocks_per_chunk,
    )
    .collect()
}

/// Project cluster-relative exceptions onto an arbitrary block range
/// inside the cluster. Used by the Wii partition encoder when a chunk
/// doesn't cover `blocks_per_chunk` whole blocks, e.g. the final
/// chunk of a partition whose `data_size` isn't a multiple of
/// `chunk_size`.
///
/// Returns an iterator rather than an owned `Vec` so hot-path callers
/// can either collect into a reused scratch buffer or iterate once
/// through the filtered set with zero heap allocation. The emitted
/// exceptions are chunk-local: `offset` is relative to
/// `first_block * HASH_REGION_BYTES`.
pub fn split_chunk_exceptions_by_range<'a>(
    cluster_exceptions: &'a [HashException],
    first_block: usize,
    blocks_in_chunk: usize,
) -> impl Iterator<Item = HashException> + 'a {
    // Compute offsets in u32 so a full-cluster range (`64 * 1024 =
    // 65536`) doesn't wrap u16 to 0.
    let chunk_start_offset: u32 = (first_block * HASH_REGION_BYTES) as u32;
    let chunk_end_offset: u32 = ((first_block + blocks_in_chunk) * HASH_REGION_BYTES) as u32;
    cluster_exceptions
        .iter()
        .filter(move |ex| {
            let off = ex.offset as u32;
            off >= chunk_start_offset && off < chunk_end_offset
        })
        .map(move |ex| HashException {
            offset: ((ex.offset as u32) - chunk_start_offset) as u16,
            hash: ex.hash,
        })
}

/// Allocate a fresh 2 MiB `Vec<u8>` and fill it via
/// [`reencrypt_cluster_into`]. Convenience for tests and one-shot
/// callers; production RVZ worker pools call the `_into` variant
/// with persistent scratch to avoid per-cluster allocator traffic.
pub fn reencrypt_cluster(
    hash_regions: &[[u8; HASH_REGION_BYTES]],
    payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
    title_key: &[u8; 16],
) -> RvzResult<Vec<u8>> {
    let mut out = vec![0u8; WII_GROUP_TOTAL_SIZE as usize];
    reencrypt_cluster_into(hash_regions, payloads, title_key, &mut out)?;
    Ok(out)
}

/// Re-encrypt one Wii cluster: stitch `hash_region + payload` for each
/// of the 64 sectors, AES-CBC encrypt each sector with `title_key`,
/// write the ciphertext into `out`. Zero heap allocations; callers
/// hoist `out` into persistent worker scratch so a single 2 MiB
/// buffer serves thousands of clusters.
///
/// `out.len()` must equal `WII_GROUP_TOTAL_SIZE` (0x200000). Partial
/// buffers are a programming error.
pub fn reencrypt_cluster_into(
    hash_regions: &[[u8; HASH_REGION_BYTES]],
    payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
    title_key: &[u8; 16],
    out: &mut [u8],
) -> RvzResult<()> {
    debug_assert_eq!(hash_regions.len(), WII_BLOCKS_PER_GROUP);
    debug_assert_eq!(payloads.len(), WII_BLOCKS_PER_GROUP);
    debug_assert_eq!(out.len(), WII_GROUP_TOTAL_SIZE as usize);
    for i in 0..WII_BLOCKS_PER_GROUP {
        // Stitch hash region + payload into a stack-local sector
        // buffer, then AES-CBC encrypt in place. The two memcpys
        // into the local are cheaper than keeping separate tables
        // and encrypting across them.
        let mut sector = [0u8; WII_SECTOR_SIZE];
        sector[..HASH_REGION_BYTES].copy_from_slice(&hash_regions[i]);
        sector[HASH_REGION_BYTES..].copy_from_slice(&payloads[i]);
        encrypt_sector(&mut sector, title_key)?;
        out[i * WII_SECTOR_SIZE..(i + 1) * WII_SECTOR_SIZE].copy_from_slice(&sector);
    }
    Ok(())
}

/// Serialise the exception-list header that prefixes every `wia_part_t`
/// chunk body, matching Dolphin's `wia_except_list_t`:
///
/// ```text
/// [u16 BE n_exceptions][n × (u16 BE offset, 20-byte SHA-1 hash)]
/// ```
///
/// The payload bytes (either verbatim or RVZ-packed) follow immediately.
/// Separated from `pack_partition_chunk` so the RVZ packing encoder can
/// operate on the payload portion independently.
pub fn serialize_exception_header(exceptions: &[HashException]) -> RvzResult<Vec<u8>> {
    let mut out = Vec::with_capacity(2 + exceptions.len() * 22);
    serialize_exception_header_into(exceptions, &mut out)?;
    Ok(out)
}

/// Append the serialised exception header bytes to `out`. Callers hoist
/// `out` into persistent worker scratch (`Vec::clear` + call) so the
/// hot loop has zero per-chunk allocator traffic.
pub fn serialize_exception_header_into(
    exceptions: &[HashException],
    out: &mut Vec<u8>,
) -> RvzResult<()> {
    if exceptions.len() > MAX_HASH_EXCEPTIONS_PER_CHUNK {
        return Err(RvzError::Custom(format!(
            "hash exception count {} exceeds Dolphin's cap of {}",
            exceptions.len(),
            MAX_HASH_EXCEPTIONS_PER_CHUNK
        )));
    }
    debug_assert!(exceptions.len() <= u16::MAX as usize);
    out.reserve(2 + exceptions.len() * 22);
    out.extend_from_slice(&(exceptions.len() as u16).to_be_bytes());
    for ex in exceptions {
        out.extend_from_slice(&ex.offset.to_be_bytes());
        out.extend_from_slice(&ex.hash);
    }
    Ok(())
}

/// Borrowed view of the exception-list entries at the start of a
/// partition chunk body. Keeps a slice of the `n * 22` entry bytes
/// without materialising them into a `Vec<HashException>`. Hot-path
/// callers can iterate directly, or collect into a reused scratch
/// vec, without touching the allocator.
#[derive(Clone, Copy)]
pub struct ExceptionEntriesRef<'a> {
    entries: &'a [u8],
    count: usize,
}

impl<'a> ExceptionEntriesRef<'a> {
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    /// Decode every entry into an owned `HashException`. Cheap:
    /// 22 bytes of memcpy per entry, no allocation beyond the
    /// chosen target.
    pub fn iter(&self) -> impl Iterator<Item = HashException> + 'a {
        let bytes = self.entries;
        (0..self.count).map(move |i| {
            let cursor = i * 22;
            let offset = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]);
            let mut hash = [0u8; 20];
            hash.copy_from_slice(&bytes[cursor + 2..cursor + 22]);
            HashException { offset, hash }
        })
    }
}

/// Parse the exception-list header from the start of a chunk body.
/// Returns a borrowed view over the entries (zero-copy, zero-alloc)
/// and the remaining bytes (payload region).
///
/// When `align_to_4` is true, the parser rounds the exception area
/// up to a 4-byte boundary after reading the count + entries.
/// Dolphin's `Chunk::HandleExceptions` in `WIABlob.cpp` applies this
/// alignment for raw (uncompressed) chunks only, matching the
/// `align=true` flag it passes when `!m_compressed_exception_lists`.
/// Compressed chunks do NOT have the alignment pad.
pub fn parse_exception_header<'a>(
    data: &'a [u8],
    align_to_4: bool,
) -> RvzResult<(ExceptionEntriesRef<'a>, &'a [u8])> {
    if data.len() < 2 {
        return Err(RvzError::Custom("truncated partition chunk header".into()));
    }
    let n = u16::from_be_bytes(data[..2].try_into().unwrap()) as usize;
    let entries_bytes = n * 22;
    let mut exception_area = 2 + entries_bytes;
    if align_to_4 {
        exception_area = (exception_area + 3) & !3;
    }
    if data.len() < exception_area {
        return Err(RvzError::Custom("truncated exception list".into()));
    }
    let view = ExceptionEntriesRef {
        entries: &data[2..2 + entries_bytes],
        count: n,
    };
    Ok((view, &data[exception_area..]))
}

/// Split a payload-region byte slice into 64 fixed-size Wii sector
/// payloads.
pub fn split_payloads(data: &[u8]) -> RvzResult<Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>> {
    if data.len() < WII_GROUP_PAYLOAD_SIZE as usize {
        return Err(RvzError::Custom(
            "truncated partition chunk payloads".into(),
        ));
    }
    let mut payloads = Vec::with_capacity(WII_BLOCKS_PER_GROUP);
    for i in 0..WII_BLOCKS_PER_GROUP {
        let mut p = [0u8; WII_SECTOR_PAYLOAD_SIZE];
        let start = i * WII_SECTOR_PAYLOAD_SIZE;
        p.copy_from_slice(&data[start..start + WII_SECTOR_PAYLOAD_SIZE]);
        payloads.push(p);
    }
    Ok(payloads)
}

/// Serialise a chunk body with verbatim (non-RVZ-packed) payloads, for
/// the fallback path when RVZ packing finds no junk runs.
pub fn pack_partition_chunk(
    exceptions: &[HashException],
    payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
) -> RvzResult<Vec<u8>> {
    let mut out = serialize_exception_header(exceptions)?;
    out.reserve(WII_GROUP_PAYLOAD_SIZE as usize);
    for p in payloads {
        out.extend_from_slice(p);
    }
    Ok(out)
}

/// Inverse of [`pack_partition_chunk`]. Returns the exceptions and the 64
/// payload arrays. Only valid for chunks where the payload portion is
/// verbatim (not RVZ-packed).
pub fn unpack_partition_chunk(
    data: &[u8],
) -> RvzResult<(Vec<HashException>, Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]>)> {
    let (exceptions_ref, remainder) = parse_exception_header(data, false)?;
    let exceptions: Vec<HashException> = exceptions_ref.iter().collect();
    let payloads = split_payloads(remainder)?;
    Ok((exceptions, payloads))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_payload(seed: u8) -> [u8; WII_SECTOR_PAYLOAD_SIZE] {
        let mut p = [0u8; WII_SECTOR_PAYLOAD_SIZE];
        for (i, b) in p.iter_mut().enumerate() {
            *b = ((i as u8).wrapping_mul(7)).wrapping_add(seed);
        }
        p
    }

    #[test]
    fn split_chunk_exceptions_full_cluster_does_not_overflow_u16() {
        // Regression: at chunks_per_cluster=1 (i.e. 2 MiB chunks), the
        // whole-cluster "chunk" must keep every exception. Earlier code
        // computed `chunk_end_offset = 64 * 0x400 = 0x10000` as u16,
        // which wrapped to 0 and silently dropped every exception,
        // corrupting the final Wii partition re-encryption.
        let cluster_exceptions: Vec<HashException> = (0..64)
            .map(|block| HashException {
                offset: (block * HASH_REGION_BYTES) as u16 + 832, // h2 start
                hash: [block as u8; 20],
            })
            .collect();
        let split = split_chunk_exceptions(&cluster_exceptions, 0, 64);
        assert_eq!(
            split.len(),
            64,
            "all 64 exceptions must survive the split for a 2 MiB chunk"
        );
        // Chunk-local offsets must equal the cluster-relative offsets
        // because this is a single-chunk cluster.
        for (i, ex) in split.iter().enumerate() {
            assert_eq!(ex.offset as usize, i * HASH_REGION_BYTES + 832);
        }
    }

    #[test]
    fn full_cluster_pipeline_recovers_mixed_valid_and_padding() {
        // Mix real data (sectors 0..11) with all-zero padding (sectors
        // 12..63) in one cluster, mirroring the tail of a real Wii
        // partition whose data_size doesn't end on a cluster boundary.
        use crate::nintendo::rvl::constants::WII_SECTOR_SIZE;
        let title_key = [0xA5u8; 16];

        // Sectors 0..11: plaintext we craft and then encrypt.
        let mut original_ciphertext = vec![0u8; WII_GROUP_TOTAL_SIZE as usize];
        let valid_payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> =
            (0..12).map(|i| make_payload((i * 7 + 3) as u8)).collect();
        // Compute the hash hierarchy over a FULL 64-sector cluster
        // where sectors 12..63 have the "decrypted junk" that comes
        // from decrypt_sector(all-zero ciphertext, title_key).
        // Step 1: derive those junk payloads + hash regions by
        // actually decrypting 52 all-zero sectors.
        let mut junk_payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = Vec::with_capacity(52);
        for _ in 12..64 {
            let mut s = [0u8; WII_SECTOR_SIZE];
            crate::nintendo::rvl::disc::decrypt_sector(&mut s, &title_key).unwrap();
            let mut p = [0u8; WII_SECTOR_PAYLOAD_SIZE];
            p.copy_from_slice(&s[HASH_REGION_BYTES..]);
            junk_payloads.push(p);
        }
        // Full 64-sector payload vector.
        let mut all_payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = valid_payloads.to_vec();
        all_payloads.extend(junk_payloads.iter().copied());

        // Hash-region authors for the 64 sectors are:
        //   0..11: recompute_hash_regions over `all_payloads` (Nintendo's
        //          authoring tool computes real H0/H1/H2 for these).
        //   12..63: decrypt_sector(all-zero ciphertext), i.e. junk bytes
        //           that don't match the recomputed hash hierarchy.
        let recomputed_full = recompute_hash_regions(&all_payloads);
        let mut on_disc: Vec<[u8; HASH_REGION_BYTES]> = Vec::with_capacity(64);
        // Sectors 0..11: use the computed hash region (so encryption
        // produces the "correct" ciphertext that would be on a real
        // disc for these valid sectors).
        for region in recomputed_full.iter().take(12) {
            on_disc.push(*region);
        }
        // Sectors 12..63: use the junk hash region from decrypting
        // all-zero ciphertext.
        for _ in 12..64 {
            let mut s = [0u8; WII_SECTOR_SIZE];
            crate::nintendo::rvl::disc::decrypt_sector(&mut s, &title_key).unwrap();
            let mut r = [0u8; HASH_REGION_BYTES];
            r.copy_from_slice(&s[..HASH_REGION_BYTES]);
            on_disc.push(r);
        }

        // Now synthesise the actual ciphertext on disc: encrypt the
        // constructed sectors. Sectors 12..63 should encrypt back to
        // all-zero because their on_disc plaintext was derived from
        // decrypt(0).
        for i in 0..64 {
            let mut sector = [0u8; WII_SECTOR_SIZE];
            sector[..HASH_REGION_BYTES].copy_from_slice(&on_disc[i]);
            sector[HASH_REGION_BYTES..].copy_from_slice(&all_payloads[i]);
            crate::nintendo::rvl::disc::encrypt_sector(&mut sector, &title_key).unwrap();
            original_ciphertext[i * WII_SECTOR_SIZE..(i + 1) * WII_SECTOR_SIZE]
                .copy_from_slice(&sector);
        }

        // Now exercise the round-trip: decrypt → recompute → exceptions
        // → apply → reencrypt, and verify the output matches.
        let mut cursor = std::io::Cursor::new(original_ciphertext.clone());
        let cluster = read_and_decrypt_cluster(&mut cursor, &title_key).unwrap();
        let recon = recompute_hash_regions(&cluster.payloads);
        let exceptions = build_hash_exceptions(&cluster, &recon);

        let mut rebuilt = recompute_hash_regions(&cluster.payloads);
        apply_hash_exceptions(&mut rebuilt, &exceptions);
        let reencrypted = reencrypt_cluster(&rebuilt, &cluster.payloads, &title_key).unwrap();

        if reencrypted != original_ciphertext {
            for (i, (a, b)) in reencrypted
                .iter()
                .zip(original_ciphertext.iter())
                .enumerate()
            {
                if a != b {
                    panic!(
                        "byte {:#x} differs: want {:#04x}, got {:#04x} (sector {}, off {:#x})",
                        i,
                        b,
                        a,
                        i / WII_SECTOR_SIZE,
                        i % WII_SECTOR_SIZE
                    );
                }
            }
        }
    }

    #[test]
    fn full_cluster_pipeline_recovers_all_zero_ciphertext() {
        // Simulate the "decrypt junk cluster" that real Wii partitions
        // have in the tail sectors past their declared data_size.
        // Start with 2 MiB of all-zero ciphertext (a whole cluster of
        // zero-padded blocks), run it through the exact path the
        // encoder uses (read_and_decrypt_cluster + recompute_hash_regions
        // + build_hash_exceptions), then through the decoder path
        // (recompute + apply + reencrypt_cluster), and assert the final
        // ciphertext matches the original.
        let title_key = [0xA5u8; 16];
        let original_ciphertext = vec![0u8; WII_GROUP_TOTAL_SIZE as usize];

        // Encode-side: decrypt the cluster.
        let mut cursor = std::io::Cursor::new(original_ciphertext.clone());
        let cluster = read_and_decrypt_cluster(&mut cursor, &title_key).unwrap();
        let recon = recompute_hash_regions(&cluster.payloads);
        let exceptions = build_hash_exceptions(&cluster, &recon);

        // Decode-side: start from just the payloads + exceptions, no
        // access to the original cluster.on_disc_hash_regions.
        let mut rebuilt = recompute_hash_regions(&cluster.payloads);
        apply_hash_exceptions(&mut rebuilt, &exceptions);
        let reencrypted = reencrypt_cluster(&rebuilt, &cluster.payloads, &title_key).unwrap();

        if reencrypted != original_ciphertext {
            // Pinpoint the first differing byte.
            for (i, (a, b)) in reencrypted
                .iter()
                .zip(original_ciphertext.iter())
                .enumerate()
            {
                if a != b {
                    panic!(
                        "byte {:#x} differs: want {:#04x}, got {:#04x} (sector {}, offset {:#x})",
                        i,
                        b,
                        a,
                        i / WII_SECTOR_SIZE,
                        i % WII_SECTOR_SIZE
                    );
                }
            }
        }
    }

    #[test]
    fn exception_roundtrip_recovers_arbitrary_hash_regions() {
        // Build a DecryptedCluster with 64 arbitrary hash regions (not
        // ones that match `recompute_hash_regions` of the payloads).
        // Build exceptions, apply them to a fresh recompute, and assert
        // the result equals the original. If any byte differs, the
        // compare_hashes / apply_hash_exceptions pair is losing the
        // diff, which is the failure mode on partitions whose padding
        // clusters carry decrypt-junk hash regions that don't match
        // the recomputed hierarchy.
        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload((i * 11) as u8))
            .collect();
        let on_disc: Vec<[u8; HASH_REGION_BYTES]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| {
                let mut r = [0u8; HASH_REGION_BYTES];
                for (b, slot) in r.iter_mut().enumerate() {
                    *slot = ((b * 31 + i * 97) as u8).wrapping_add(i as u8);
                }
                r
            })
            .collect();
        // Keep a pristine copy for comparison; apply_hash_exceptions
        // should reproduce this from recompute + exceptions.
        let original_on_disc = on_disc.clone();

        let cluster = DecryptedCluster {
            on_disc_hash_regions: on_disc.clone(),
            payloads: payloads.clone(),
        };
        let recomputed = recompute_hash_regions(&payloads);
        let exceptions = build_hash_exceptions(&cluster, &recomputed);

        let mut rebuilt = recompute_hash_regions(&payloads);
        apply_hash_exceptions(&mut rebuilt, &exceptions);

        for (i, (got, want)) in rebuilt.iter().zip(original_on_disc.iter()).enumerate() {
            if got != want {
                // Pinpoint the first differing byte for diagnostics.
                for (b, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                    if g != w {
                        panic!(
                            "block {} byte {:#x}: want {:#04x}, got {:#04x} (recomputed {:#04x})",
                            i, b, w, g, recomputed[i][b]
                        );
                    }
                }
            }
        }
        let _ = on_disc;
    }

    #[test]
    fn h0_is_31_sha1s() {
        let payload = make_payload(0);
        let h0 = compute_h0(&payload);
        assert_eq!(h0.len(), H0_PER_SECTOR);
        // First hash should match a manual SHA-1 of the first 0x400 bytes.
        let mut h = Sha1::new();
        h.update(&payload[..0x400]);
        let expected: [u8; 20] = h.finalize().into();
        assert_eq!(h0[0], expected);
    }

    #[test]
    fn recompute_hash_regions_returns_64_blocks_of_0x400() {
        let payloads: Vec<_> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload(i as u8))
            .collect();
        let regions = recompute_hash_regions(&payloads);
        assert_eq!(regions.len(), WII_BLOCKS_PER_GROUP);
        assert_eq!(regions[0].len(), HASH_REGION_BYTES);
    }

    #[test]
    fn pack_unpack_partition_chunk_roundtrips_with_no_exceptions() {
        let payloads: Vec<_> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload((i * 3) as u8))
            .collect();
        let chunk = pack_partition_chunk(&[], &payloads).unwrap();
        let (exc, out_payloads) = unpack_partition_chunk(&chunk).unwrap();
        assert!(exc.is_empty());
        assert_eq!(out_payloads.len(), payloads.len());
        for (a, b) in payloads.iter().zip(out_payloads.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn pack_unpack_partition_chunk_roundtrips_with_exceptions() {
        let payloads: Vec<_> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload(i as u8))
            .collect();
        let exceptions = vec![
            HashException {
                offset: 5,
                hash: [0xABu8; 20],
            },
            HashException {
                offset: 0x800,
                hash: [0xCDu8; 20],
            },
        ];
        let chunk = pack_partition_chunk(&exceptions, &payloads).unwrap();
        let (exc, _) = unpack_partition_chunk(&chunk).unwrap();
        assert_eq!(exc, exceptions);
    }

    #[test]
    fn apply_hash_exceptions_overwrites_20_bytes() {
        let mut regions = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
        let exceptions = vec![HashException {
            offset: 0x10,
            hash: [0x77u8; 20],
        }];
        apply_hash_exceptions(&mut regions, &exceptions);
        for i in 0..20 {
            assert_eq!(regions[0][0x10 + i], 0x77);
        }
        assert_eq!(regions[0][0x10 + 20], 0);
    }

    #[test]
    fn apply_hash_exceptions_routes_to_correct_block() {
        let mut regions = vec![[0u8; HASH_REGION_BYTES]; WII_BLOCKS_PER_GROUP];
        // offset 0x801 = block 2 (0x800 = 2 * 0x400), byte 1 inside block
        let exceptions = vec![HashException {
            offset: 0x801,
            hash: [0x99u8; 20],
        }];
        apply_hash_exceptions(&mut regions, &exceptions);
        assert_eq!(regions[2][1], 0x99);
        assert_eq!(regions[2][20], 0x99);
        assert_eq!(regions[2][21], 0);
    }

    #[test]
    fn build_hash_exceptions_produces_none_for_clean_cluster() {
        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload(i as u8))
            .collect();
        let regions = recompute_hash_regions(&payloads);
        let cluster = DecryptedCluster {
            on_disc_hash_regions: regions.clone(),
            payloads: payloads.clone(),
        };
        let recon = recompute_hash_regions(&payloads);
        let exceptions = build_hash_exceptions(&cluster, &recon);
        assert!(
            exceptions.is_empty(),
            "clean cluster should have 0 exceptions"
        );
    }

    #[test]
    fn build_hash_exceptions_detects_flipped_h0_hash() {
        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload(i as u8))
            .collect();
        let mut on_disc = recompute_hash_regions(&payloads);
        // Corrupt the first byte of h0[0] in block 5.
        on_disc[5][hash_region::H0_OFFSET] ^= 0xFF;
        let cluster = DecryptedCluster {
            on_disc_hash_regions: on_disc,
            payloads: payloads.clone(),
        };
        let recon = recompute_hash_regions(&payloads);
        let exceptions = build_hash_exceptions(&cluster, &recon);
        assert_eq!(exceptions.len(), 1);
        assert_eq!(exceptions[0].offset, 5 * 0x400);
    }

    #[test]
    fn build_hash_exceptions_round_trips_through_apply() {
        // Start with a clean cluster, mangle a few bytes across different
        // tiers, build the exception list, then apply it to the recomputed
        // regions and verify the result matches the mangled originals.
        let payloads: Vec<[u8; WII_SECTOR_PAYLOAD_SIZE]> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload(i as u8))
            .collect();
        let mut on_disc = recompute_hash_regions(&payloads);
        on_disc[0][hash_region::H0_OFFSET] = 0xAA;
        on_disc[10][hash_region::H1_OFFSET + 40] = 0xBB;
        on_disc[20][hash_region::PADDING_2_OFFSET + 15] = 0xCC;

        let cluster = DecryptedCluster {
            on_disc_hash_regions: on_disc.clone(),
            payloads: payloads.clone(),
        };
        let recon = recompute_hash_regions(&payloads);
        let exceptions = build_hash_exceptions(&cluster, &recon);

        let mut regenerated = recompute_hash_regions(&payloads);
        apply_hash_exceptions(&mut regenerated, &exceptions);
        assert_eq!(regenerated, on_disc);
    }

    #[test]
    fn cluster_reencrypt_decrypt_roundtrip() {
        // Build 64 plaintext payloads + 64 hash regions; encrypt; decrypt;
        // assert the originals come back.
        let title_key = [0xA5u8; 16];
        let payloads: Vec<_> = (0..WII_BLOCKS_PER_GROUP)
            .map(|i| make_payload(i as u8))
            .collect();
        let regions = recompute_hash_regions(&payloads);
        let ciphertext = reencrypt_cluster(&regions, &payloads, &title_key).unwrap();
        let mut cursor = std::io::Cursor::new(ciphertext);
        let decrypted = read_and_decrypt_cluster(&mut cursor, &title_key).unwrap();
        for i in 0..WII_BLOCKS_PER_GROUP {
            assert_eq!(decrypted.payloads[i], payloads[i]);
            assert_eq!(decrypted.on_disc_hash_regions[i], regions[i]);
        }
    }
}
