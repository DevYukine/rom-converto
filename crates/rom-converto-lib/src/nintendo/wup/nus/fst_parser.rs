//! Wii U FST parser.
//!
//! The FST is a small filesystem table embedded at the start of
//! content 0 (`00000000.app`) of every Wii U title. Once content 0
//! has been decrypted with the ticket title key it becomes parseable
//! with this module. The layout matches Cemu's `FSTHeader`,
//! `FSTHeader_ClusterEntry`, and `FSTHeader_FileEntry` structs in
//! `src/Cafe/Filesystem/FST/FST.h`:
//!
//! - 0x20-byte [`FstHeader`] (magic `0x46535400`, offset factor,
//!   cluster count, hash disabled flag, padding).
//! - `num_clusters` x 0x20-byte cluster descriptors.
//! - Variable-length file entry array (0x10 bytes each). Entry 0 is
//!   the root directory; its "size" field doubles as the total
//!   entry count.
//! - Name string table (NUL-terminated C strings) filling out the
//!   rest of the FST payload.
//!
//! The parser produces a flat [`VirtualFs`] holding every file's
//! `(path, cluster_index, file_offset, file_size)` tuple. Directory
//! entries are consumed during the depth-first walk but not emitted
//! separately: the writer recreates them implicitly when files are
//! added.

use crate::nintendo::wup::error::{WupError, WupResult};

/// Magic of a valid FST header: ASCII `"FST\0"` big-endian.
pub const FST_MAGIC: u32 = 0x4653_5400;

/// Fixed size of the `FSTHeader`.
pub const FST_HEADER_SIZE: usize = 0x20;

/// Fixed size of one `FSTHeader_ClusterEntry`.
pub const FST_CLUSTER_ENTRY_SIZE: usize = 0x20;

/// Fixed size of one `FSTHeader_FileEntry`.
pub const FST_FILE_ENTRY_SIZE: usize = 0x10;

/// Per-cluster hash mode stored in `FSTHeader_ClusterEntry.hashMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FstClusterHashMode {
    /// Raw AES-CBC, no hashing. Used for content 0 (the FST itself).
    Raw,
    /// Raw AES-CBC, with a hash stored in the TMD content entry.
    RawStream,
    /// 64 KiB blocks of `[hash_prefix_0x400][data_0xFC00]`, each
    /// block independently encrypted.
    HashInterleaved,
    /// Future / unknown hash modes. Stored as the raw byte so a
    /// caller that supports them can still dispatch.
    Unknown(u8),
}

impl FstClusterHashMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => FstClusterHashMode::Raw,
            1 => FstClusterHashMode::RawStream,
            2 => FstClusterHashMode::HashInterleaved,
            other => FstClusterHashMode::Unknown(other),
        }
    }
}

/// One FST cluster descriptor. A cluster maps a virtual offset
/// range to a physical content `.app` file via its `owner_title_id`
/// and per-cluster offset / size (in sectors of `offset_factor`
/// bytes each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FstCluster {
    pub offset: u32,
    pub size: u32,
    pub owner_title_id: u64,
    pub group_id: u32,
    pub hash_mode: FstClusterHashMode,
}

/// One virtual file discovered during the FST walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualFile {
    /// Full path relative to the title root, e.g. `meta/meta.xml`.
    /// Forward slashes are used on every host.
    pub path: String,
    /// Index into [`VirtualFs::clusters`] telling us which cluster
    /// (and therefore which `.app` file) this file lives in.
    pub cluster_index: u16,
    /// Byte offset within the cluster, pre-`offset_factor`
    /// multiplication. Multiply by [`VirtualFs::offset_factor`] to
    /// get the real byte offset relative to the cluster start.
    pub file_offset: u32,
    /// File size in bytes.
    pub file_size: u32,
    /// True when bit 7 of the file entry's type byte is set. The
    /// Wii U FST uses that bit to flag a file whose bytes are
    /// inherited from another title (base for an update, base or
    /// update for a DLC). Extraction must skip these so an update
    /// overlay only emits its own new bytes.
    pub is_shared: bool,
}

/// Parsed FST view: header fields, cluster table, and flat file
/// list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualFs {
    /// Multiplier applied to file offsets to get absolute bytes
    /// within a cluster. Usually 0x20 on disc content and 1 on NUS
    /// content, but the parser doesn't assume either.
    pub offset_factor: u32,
    /// Top-level hash-verification-disabled flag. Informational;
    /// the decryption layer relies on per-cluster `hash_mode`.
    pub hash_is_disabled: bool,
    pub clusters: Vec<FstCluster>,
    pub files: Vec<VirtualFile>,
}

/// Parse a full FST from the given decrypted bytes. `bytes` must be
/// the full decrypted contents of cluster 0 (content 0), including
/// the header, cluster table, file entries, and name string table.
pub fn parse_fst(bytes: &[u8]) -> WupResult<VirtualFs> {
    if bytes.len() < FST_HEADER_SIZE {
        return Err(WupError::InvalidFst);
    }

    let magic = read_u32_be(bytes, 0);
    if magic != FST_MAGIC {
        return Err(WupError::InvalidFst);
    }
    let offset_factor = read_u32_be(bytes, 0x04);
    let num_clusters = read_u32_be(bytes, 0x08) as usize;
    let hash_is_disabled = bytes[0x0C] != 0;

    // Sanity: reject pathological cluster counts that would
    // overflow usize or point past the buffer.
    let clusters_end = FST_HEADER_SIZE
        .checked_add(
            num_clusters
                .checked_mul(FST_CLUSTER_ENTRY_SIZE)
                .ok_or(WupError::InvalidFst)?,
        )
        .ok_or(WupError::InvalidFst)?;
    if bytes.len() < clusters_end {
        return Err(WupError::InvalidFst);
    }

    // Parse cluster table.
    let mut clusters = Vec::with_capacity(num_clusters);
    for i in 0..num_clusters {
        let start = FST_HEADER_SIZE + i * FST_CLUSTER_ENTRY_SIZE;
        let entry = &bytes[start..start + FST_CLUSTER_ENTRY_SIZE];
        clusters.push(FstCluster {
            offset: read_u32_be(entry, 0x00),
            size: read_u32_be(entry, 0x04),
            owner_title_id: read_u64_be(entry, 0x08),
            group_id: read_u32_be(entry, 0x10),
            hash_mode: FstClusterHashMode::from_u8(entry[0x14]),
        });
    }

    // Parse the first file entry (the root directory) to learn the
    // total entry count. Then walk every entry and resolve path
    // names from the string table that follows the entry array.
    let entries_start = clusters_end;
    if bytes.len() < entries_start + FST_FILE_ENTRY_SIZE {
        return Err(WupError::InvalidFst);
    }
    let root_entry =
        FileEntryRaw::parse(&bytes[entries_start..entries_start + FST_FILE_ENTRY_SIZE]);
    if !root_entry.is_directory() || root_entry.parent_or_offset != 0 {
        return Err(WupError::InvalidFst);
    }
    let num_entries = root_entry.size_or_end_index as usize;
    if num_entries == 0 {
        return Err(WupError::InvalidFst);
    }
    let entries_end = entries_start
        .checked_add(
            num_entries
                .checked_mul(FST_FILE_ENTRY_SIZE)
                .ok_or(WupError::InvalidFst)?,
        )
        .ok_or(WupError::InvalidFst)?;
    if bytes.len() < entries_end {
        return Err(WupError::InvalidFst);
    }

    let name_table = &bytes[entries_end..];

    // Depth-first walk, mirroring Cemu's ProcessFST.
    let mut files: Vec<VirtualFile> = Vec::new();
    let mut dir_end_stack: Vec<usize> = vec![num_entries];
    let mut path_stack: Vec<String> = Vec::new();
    for i in 0..num_entries {
        while let Some(&end) = dir_end_stack.last() {
            if i >= end && dir_end_stack.len() > 1 {
                dir_end_stack.pop();
                path_stack.pop();
            } else {
                break;
            }
        }

        let entry_start = entries_start + i * FST_FILE_ENTRY_SIZE;
        let entry = FileEntryRaw::parse(&bytes[entry_start..entry_start + FST_FILE_ENTRY_SIZE]);
        let name = read_nul_terminated(name_table, entry.name_offset())?;

        if entry.is_file() {
            let path = if path_stack.is_empty() {
                name.to_string()
            } else {
                let mut s = path_stack.join("/");
                s.push('/');
                s.push_str(name);
                s
            };
            files.push(VirtualFile {
                path,
                cluster_index: entry.cluster_index,
                file_offset: entry.parent_or_offset,
                file_size: entry.size_or_end_index,
                is_shared: entry.is_shared(),
            });
        } else if entry.is_directory() {
            if i == 0 {
                // Root: already seeded onto the stack; only
                // validation needed.
                if entry.size_or_end_index as usize != num_entries {
                    return Err(WupError::InvalidFst);
                }
            } else {
                let end = entry.size_or_end_index as usize;
                if end <= i || end > num_entries {
                    return Err(WupError::InvalidFst);
                }
                path_stack.push(name.to_string());
                dir_end_stack.push(end);
            }
        } else {
            return Err(WupError::InvalidFst);
        }
    }

    Ok(VirtualFs {
        offset_factor,
        hash_is_disabled,
        clusters,
        files,
    })
}

/// Raw 16-byte file/directory entry straight out of the FST. The
/// high 8 bits of `type_and_name_offset` are the type+flag nibble
/// (bit 0 = directory, bit 7 = link) and the low 24 bits are the
/// byte offset into the name string table. The `flags_or_permissions`
/// field at `+0x0C` is ignored; every retail Wii U title sets it to
/// zero and we don't need it to walk the tree.
#[derive(Debug, Clone, Copy)]
struct FileEntryRaw {
    type_and_name_offset: u32,
    parent_or_offset: u32,
    size_or_end_index: u32,
    cluster_index: u16,
}

impl FileEntryRaw {
    fn parse(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() >= FST_FILE_ENTRY_SIZE);
        Self {
            type_and_name_offset: read_u32_be(bytes, 0x00),
            parent_or_offset: read_u32_be(bytes, 0x04),
            size_or_end_index: read_u32_be(bytes, 0x08),
            cluster_index: read_u16_be(bytes, 0x0E),
        }
    }

    fn type_flag_field(&self) -> u8 {
        ((self.type_and_name_offset >> 24) & 0xFF) as u8
    }

    fn name_offset(&self) -> u32 {
        self.type_and_name_offset & 0x00FF_FFFF
    }

    fn is_directory(&self) -> bool {
        (self.type_flag_field() & 0x01) != 0
    }

    fn is_file(&self) -> bool {
        (self.type_flag_field() & 0x01) == 0
    }

    fn is_shared(&self) -> bool {
        (self.type_flag_field() & 0x80) != 0
    }
}

fn read_nul_terminated(name_table: &[u8], offset: u32) -> WupResult<&str> {
    let start = offset as usize;
    if start >= name_table.len() {
        return Err(WupError::InvalidFst);
    }
    let rel = name_table[start..]
        .iter()
        .position(|&b| b == 0)
        .ok_or(WupError::InvalidFst)?;
    std::str::from_utf8(&name_table[start..start + rel]).map_err(|_| WupError::InvalidFst)
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

    /// A tiny, hand-built FST fixture. Describes this tree with 1
    /// cluster:
    ///
    /// ```text
    /// /
    /// |-- meta/
    /// |   |-- meta.xml          (size = 0x100)
    /// |   \-- icon.tga          (size = 0x4000)
    /// |-- code/
    /// |   |-- app.xml           (size = 0x80)
    /// |   \-- main.rpx          (size = 0x2_0000)
    /// \-- content/
    ///     \-- shader.bin        (size = 0x8_0000)
    /// ```
    ///
    /// Total: 3 top-level dirs + 5 files + 1 root dir = 9 entries.
    /// All files live in cluster 0 at sequential offsets.
    fn build_fixture_fst() -> Vec<u8> {
        // Name string table: we pack the names back-to-back with
        // NUL terminators and record each one's starting offset.
        let mut name_table: Vec<u8> = Vec::new();
        let mut name_offsets = std::collections::HashMap::new();
        for name in [
            "",
            "meta",
            "meta.xml",
            "icon.tga",
            "code",
            "app.xml",
            "main.rpx",
            "content",
            "shader.bin",
        ] {
            name_offsets.insert(name.to_string(), name_table.len() as u32);
            name_table.extend_from_slice(name.as_bytes());
            name_table.push(0);
        }

        // Layout (indices in parentheses):
        //   0: root dir     (end = 9,  parent = 0)
        //   1: meta dir     (end = 4,  parent = 0)
        //   2: meta.xml file
        //   3: icon.tga file
        //   4: code dir     (end = 7,  parent = 0)
        //   5: app.xml file
        //   6: main.rpx file
        //   7: content dir  (end = 9,  parent = 0)
        //   8: shader.bin file
        let num_entries: u32 = 9;
        let num_clusters: u32 = 1;

        let header_size = FST_HEADER_SIZE;
        let cluster_table_size = (num_clusters as usize) * FST_CLUSTER_ENTRY_SIZE;
        let entries_size = (num_entries as usize) * FST_FILE_ENTRY_SIZE;
        let total = header_size + cluster_table_size + entries_size + name_table.len();
        let mut buf = vec![0u8; total];

        // Header
        buf[0..4].copy_from_slice(&FST_MAGIC.to_be_bytes());
        buf[4..8].copy_from_slice(&1u32.to_be_bytes()); // offset_factor
        buf[8..12].copy_from_slice(&num_clusters.to_be_bytes());
        buf[12] = 0; // hash_is_disabled = false

        // Cluster table: one cluster, raw mode (hashMode=0)
        let c0 = header_size;
        buf[c0..c0 + 4].copy_from_slice(&0u32.to_be_bytes()); // cluster.offset
        buf[c0 + 4..c0 + 8].copy_from_slice(&0x10_0000u32.to_be_bytes()); // cluster.size
        buf[c0 + 8..c0 + 16].copy_from_slice(&0x0005_000E_1010_2000u64.to_be_bytes()); // owner_title_id
        buf[c0 + 16..c0 + 20].copy_from_slice(&0x1000u32.to_be_bytes()); // group_id
        buf[c0 + 20] = 0; // hash_mode = Raw

        // Helper to write one file/dir entry.
        let entries_start = header_size + cluster_table_size;
        let write_entry = |buf: &mut Vec<u8>,
                           idx: usize,
                           is_dir: bool,
                           name: &str,
                           a: u32,
                           b: u32,
                           cluster: u16| {
            let start = entries_start + idx * FST_FILE_ENTRY_SIZE;
            let type_flag: u8 = if is_dir { 0x01 } else { 0x00 };
            let name_offset = name_offsets[name];
            let type_and_name = ((type_flag as u32) << 24) | (name_offset & 0x00FF_FFFF);
            buf[start..start + 4].copy_from_slice(&type_and_name.to_be_bytes());
            buf[start + 4..start + 8].copy_from_slice(&a.to_be_bytes());
            buf[start + 8..start + 12].copy_from_slice(&b.to_be_bytes());
            buf[start + 12..start + 14].copy_from_slice(&0u16.to_be_bytes());
            buf[start + 14..start + 16].copy_from_slice(&cluster.to_be_bytes());
        };

        //   0: root            (parent=0, end=9)
        write_entry(&mut buf, 0, true, "", 0, 9, 0);
        //   1: meta dir        (parent=0, end=4)
        write_entry(&mut buf, 1, true, "meta", 0, 4, 0);
        //   2: meta.xml
        write_entry(&mut buf, 2, false, "meta.xml", 0x0000, 0x0100, 0);
        //   3: icon.tga
        write_entry(&mut buf, 3, false, "icon.tga", 0x0100, 0x4000, 0);
        //   4: code dir        (parent=0, end=7)
        write_entry(&mut buf, 4, true, "code", 0, 7, 0);
        //   5: app.xml
        write_entry(&mut buf, 5, false, "app.xml", 0x4100, 0x0080, 0);
        //   6: main.rpx
        write_entry(&mut buf, 6, false, "main.rpx", 0x4180, 0x2_0000, 0);
        //   7: content dir     (parent=0, end=9)
        write_entry(&mut buf, 7, true, "content", 0, 9, 0);
        //   8: shader.bin
        write_entry(&mut buf, 8, false, "shader.bin", 0x2_4180, 0x8_0000, 0);

        // Name string table
        let names_start = entries_start + entries_size;
        buf[names_start..names_start + name_table.len()].copy_from_slice(&name_table);

        buf
    }

    #[test]
    fn parses_fixture_header() {
        let fst = parse_fst(&build_fixture_fst()).unwrap();
        assert_eq!(fst.offset_factor, 1);
        assert!(!fst.hash_is_disabled);
        assert_eq!(fst.clusters.len(), 1);
        assert_eq!(fst.clusters[0].owner_title_id, 0x0005_000E_1010_2000);
        assert_eq!(fst.clusters[0].hash_mode, FstClusterHashMode::Raw);
    }

    #[test]
    fn parses_fixture_files() {
        let fst = parse_fst(&build_fixture_fst()).unwrap();
        let paths: Vec<_> = fst.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                "meta/meta.xml".to_string(),
                "meta/icon.tga".to_string(),
                "code/app.xml".to_string(),
                "code/main.rpx".to_string(),
                "content/shader.bin".to_string(),
            ]
        );
    }

    #[test]
    fn preserves_file_offsets_and_sizes() {
        let fst = parse_fst(&build_fixture_fst()).unwrap();
        let by_path: std::collections::HashMap<_, _> = fst
            .files
            .iter()
            .map(|f| (f.path.clone(), (f.file_offset, f.file_size)))
            .collect();
        assert_eq!(by_path["meta/meta.xml"], (0x0000, 0x0100));
        assert_eq!(by_path["meta/icon.tga"], (0x0100, 0x4000));
        assert_eq!(by_path["code/main.rpx"], (0x4180, 0x2_0000));
        assert_eq!(by_path["content/shader.bin"], (0x2_4180, 0x8_0000));
    }

    #[test]
    fn every_file_points_at_cluster_zero() {
        let fst = parse_fst(&build_fixture_fst()).unwrap();
        for file in &fst.files {
            assert_eq!(file.cluster_index, 0);
        }
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = build_fixture_fst();
        bytes[0] = b'X';
        let err = parse_fst(&bytes);
        assert!(matches!(err, Err(WupError::InvalidFst)));
    }

    #[test]
    fn rejects_short_header() {
        let short = vec![0u8; FST_HEADER_SIZE - 1];
        let err = parse_fst(&short);
        assert!(matches!(err, Err(WupError::InvalidFst)));
    }

    #[test]
    fn rejects_entries_past_buffer() {
        let mut bytes = build_fixture_fst();
        bytes.truncate(FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + FST_FILE_ENTRY_SIZE);
        let err = parse_fst(&bytes);
        assert!(matches!(err, Err(WupError::InvalidFst)));
    }

    #[test]
    fn file_entries_with_type_bit_7_are_flagged_shared() {
        // Flip entry index 4 (code/main.rpx in the fixture) to type
        // 0x80 and confirm parse_fst marks it as shared. Every other
        // file keeps is_shared == false.
        let mut bytes = build_fixture_fst();
        // Layout: header + clusters, then entries. Entry 4 is
        // code/main.rpx per the fixture's build order.
        let entries_start = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE;
        let rpx_entry = entries_start + 6 * FST_FILE_ENTRY_SIZE;
        // Set bit 7 of the type byte (high byte of type_and_name_offset).
        bytes[rpx_entry] |= 0x80;
        let fst = parse_fst(&bytes).unwrap();
        let by_path: std::collections::HashMap<_, _> = fst
            .files
            .iter()
            .map(|f| (f.path.clone(), f.is_shared))
            .collect();
        assert!(by_path["code/main.rpx"]);
        assert!(!by_path["meta/meta.xml"]);
        assert!(!by_path["content/shader.bin"]);
    }

    #[test]
    fn hash_mode_from_u8_maps_every_variant() {
        assert_eq!(FstClusterHashMode::from_u8(0), FstClusterHashMode::Raw);
        assert_eq!(
            FstClusterHashMode::from_u8(1),
            FstClusterHashMode::RawStream
        );
        assert_eq!(
            FstClusterHashMode::from_u8(2),
            FstClusterHashMode::HashInterleaved
        );
        assert_eq!(
            FstClusterHashMode::from_u8(7),
            FstClusterHashMode::Unknown(7)
        );
    }
}
