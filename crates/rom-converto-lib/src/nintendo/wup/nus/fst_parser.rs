//! Wii U FST parser.
//!
//! The FST is a small filesystem table embedded at the start of
//! content 0 (`00000000.app`) of every Wii U title. Once content 0
//! has been decrypted with the ticket title key it becomes parseable
//! with this module. The layout matches Cemu's `FSTHeader`,
//! `FSTHeader_ClusterEntry`, and `FSTHeader_FileEntry` structs in
//! `src/Cafe/Filesystem/FST/FST.h`:
//!
//! - 0x20-byte `FstHeader` (magic `0x46535400`, offset factor,
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

use std::collections::BTreeMap;

use crate::nintendo::wup::error::{WupError, WupResult};
use crate::util::bytes::{u16_be, u32_be, u64_be};

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
    /// Maps a raw `FSTHeader_ClusterEntry.hashMode` byte to its variant.
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
    /// Full path relative to the title root, such as `meta/meta.xml`.
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

/// Parse an FST by requesting only its header, tables, and name strings.
/// `read_range` must return exactly the requested decrypted bytes.
pub fn parse_fst_ranges(
    content_len: u64,
    mut read_range: impl FnMut(u64, usize) -> WupResult<Vec<u8>>,
) -> WupResult<VirtualFs> {
    if content_len < FST_HEADER_SIZE as u64 {
        return Err(WupError::InvalidFst);
    }
    let header = read_range(0, FST_HEADER_SIZE)?;
    if header.len() != FST_HEADER_SIZE {
        return Err(WupError::InvalidFst);
    }
    let (offset_factor, hash_is_disabled, num_clusters, clusters_end) = parse_header(&header)?;
    if clusters_end as u64 > content_len {
        return Err(WupError::InvalidFst);
    }
    let cluster_len = num_clusters
        .checked_mul(FST_CLUSTER_ENTRY_SIZE)
        .ok_or(WupError::InvalidFst)?;
    let cluster_bytes = read_range(FST_HEADER_SIZE as u64, cluster_len)?;
    if cluster_bytes.len() != cluster_len {
        return Err(WupError::InvalidFst);
    }
    let clusters = parse_clusters(&cluster_bytes, num_clusters)?;
    let root_end = clusters_end
        .checked_add(FST_FILE_ENTRY_SIZE)
        .ok_or(WupError::InvalidFst)?;
    if root_end as u64 > content_len {
        return Err(WupError::InvalidFst);
    }
    let root_bytes = read_range(clusters_end as u64, FST_FILE_ENTRY_SIZE)?;
    if root_bytes.len() != FST_FILE_ENTRY_SIZE {
        return Err(WupError::InvalidFst);
    }
    let num_entries = entry_count(FileEntryRaw::parse(&root_bytes))?;
    let entries_len = num_entries
        .checked_mul(FST_FILE_ENTRY_SIZE)
        .ok_or(WupError::InvalidFst)?;
    let entries_end = clusters_end
        .checked_add(entries_len)
        .ok_or(WupError::InvalidFst)?;
    if entries_end as u64 > content_len {
        return Err(WupError::InvalidFst);
    }
    let name_table_start = entries_end;
    let entry_start = u64::try_from(clusters_end).map_err(|_| WupError::InvalidFst)?;
    let entry_table = read_entry_table(&mut read_range, entry_start, entries_len, &root_bytes)?;
    let name_offsets = read_entry_name_offsets(&entry_table)?;
    let names = read_name_table(&mut read_range, content_len, name_table_start, name_offsets)?;
    let entries = EntryTableReader::new(&entry_table, num_entries);
    walk_entries(
        offset_factor,
        hash_is_disabled,
        clusters,
        num_entries,
        |index| entries.read_entry(index),
        |offset| names.get(&offset).cloned().ok_or(WupError::InvalidFst),
    )
}
fn read_entry_table(
    read_range: &mut impl FnMut(u64, usize) -> WupResult<Vec<u8>>,
    entry_start: u64,
    entries_len: usize,
    root_entry: &[u8],
) -> WupResult<Vec<u8>> {
    if root_entry.len() != FST_FILE_ENTRY_SIZE || entries_len < FST_FILE_ENTRY_SIZE {
        return Err(WupError::InvalidFst);
    }
    let rest_len = entries_len - FST_FILE_ENTRY_SIZE;
    let rest_start = entry_start
        .checked_add(FST_FILE_ENTRY_SIZE as u64)
        .ok_or(WupError::InvalidFst)?;
    let rest = if rest_len == 0 {
        Vec::new()
    } else {
        let rest = read_range(rest_start, rest_len)?;
        if rest.len() != rest_len {
            return Err(WupError::InvalidFst);
        }
        rest
    };
    let mut table = Vec::with_capacity(entries_len);
    table.extend_from_slice(root_entry);
    table.extend_from_slice(&rest);
    Ok(table)
}

fn read_entry_name_offsets(table: &[u8]) -> WupResult<Vec<u32>> {
    if !table.len().is_multiple_of(FST_FILE_ENTRY_SIZE) {
        return Err(WupError::InvalidFst);
    }
    Ok(table
        .as_chunks::<FST_FILE_ENTRY_SIZE>()
        .0
        .iter()
        .map(|entry| FileEntryRaw::parse(entry).name_offset())
        .collect())
}

struct EntryTableReader<'a> {
    table: &'a [u8],
    num_entries: usize,
}

impl<'a> EntryTableReader<'a> {
    fn new(table: &'a [u8], num_entries: usize) -> Self {
        Self { table, num_entries }
    }

    fn read_entry(&self, index: usize) -> WupResult<FileEntryRaw> {
        if index >= self.num_entries {
            return Err(WupError::InvalidFst);
        }
        let start = index
            .checked_mul(FST_FILE_ENTRY_SIZE)
            .ok_or(WupError::InvalidFst)?;
        Ok(FileEntryRaw::parse(
            &self.table[start..start + FST_FILE_ENTRY_SIZE],
        ))
    }
}

fn read_name_table(
    read_range: &mut impl FnMut(u64, usize) -> WupResult<Vec<u8>>,
    content_len: u64,
    table_start: usize,
    mut offsets: Vec<u32>,
) -> WupResult<BTreeMap<u32, String>> {
    const WINDOW_SIZE: usize = 4096;

    offsets.sort_unstable();
    offsets.dedup();
    let table_start_u64 = u64::try_from(table_start).map_err(|_| WupError::InvalidFst)?;
    let name_table_len = content_len
        .checked_sub(table_start_u64)
        .ok_or(WupError::InvalidFst)?;
    if offsets
        .iter()
        .any(|offset| u64::from(*offset) >= name_table_len)
    {
        return Err(WupError::InvalidFst);
    }

    let mut names = BTreeMap::new();
    let mut pending: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut next_offset = 0;
    let mut window_start = 0usize;
    while next_offset < offsets.len() || !pending.is_empty() {
        if pending.is_empty() {
            window_start = offsets[next_offset] as usize;
        }
        let absolute_start = table_start_u64
            .checked_add(window_start as u64)
            .ok_or(WupError::InvalidFst)?;
        let remaining = name_table_len
            .checked_sub(window_start as u64)
            .ok_or(WupError::InvalidFst)?;
        let window_len = WINDOW_SIZE.min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let window = read_range(absolute_start, window_len)?;
        if window.len() != window_len {
            return Err(WupError::InvalidFst);
        }
        let window_end = window_start
            .checked_add(window_len)
            .ok_or(WupError::InvalidFst)?;
        while next_offset < offsets.len() && (offsets[next_offset] as usize) < window_end {
            pending.insert(offsets[next_offset], Vec::new());
            next_offset += 1;
        }

        let mut finished = Vec::new();
        for (&offset, name) in pending.iter_mut() {
            let start = (offset as usize).saturating_sub(window_start);
            let suffix = window.get(start..).ok_or(WupError::InvalidFst)?;
            let bytes_remaining = name_table_len
                .checked_sub(u64::from(offset))
                .and_then(|remaining| remaining.checked_sub(name.len() as u64))
                .ok_or(WupError::InvalidFst)?;
            let suffix_len = suffix
                .len()
                .min(usize::try_from(bytes_remaining).unwrap_or(usize::MAX));
            let suffix = &suffix[..suffix_len];
            if let Some(nul) = suffix.iter().position(|&byte| byte == 0) {
                name.extend_from_slice(&suffix[..nul]);
                finished.push(offset);
            } else {
                name.extend_from_slice(suffix);
                if suffix_len as u64 == bytes_remaining {
                    return Err(WupError::InvalidFst);
                }
            }
        }
        for offset in finished {
            let name = pending.remove(&offset).ok_or(WupError::InvalidFst)?;
            names.insert(
                offset,
                String::from_utf8(name).map_err(|_| WupError::InvalidFst)?,
            );
        }
        window_start = window_end;
    }
    Ok(names)
}

fn parse_header(header: &[u8]) -> WupResult<(u32, bool, usize, usize)> {
    if header.len() < FST_HEADER_SIZE || u32_be(header, 0) != FST_MAGIC {
        return Err(WupError::InvalidFst);
    }
    let offset_factor = u32_be(header, 0x04);
    let num_clusters = u32_be(header, 0x08) as usize;
    let hash_is_disabled = header[0x0C] != 0;
    let clusters_end = FST_HEADER_SIZE
        .checked_add(
            num_clusters
                .checked_mul(FST_CLUSTER_ENTRY_SIZE)
                .ok_or(WupError::InvalidFst)?,
        )
        .ok_or(WupError::InvalidFst)?;
    Ok((offset_factor, hash_is_disabled, num_clusters, clusters_end))
}

fn parse_clusters(bytes: &[u8], num_clusters: usize) -> WupResult<Vec<FstCluster>> {
    let expected_len = num_clusters
        .checked_mul(FST_CLUSTER_ENTRY_SIZE)
        .ok_or(WupError::InvalidFst)?;
    if bytes.len() != expected_len {
        return Err(WupError::InvalidFst);
    }
    let mut clusters = Vec::with_capacity(num_clusters);
    for entry in bytes.as_chunks::<FST_CLUSTER_ENTRY_SIZE>().0 {
        clusters.push(FstCluster {
            offset: u32_be(entry, 0x00),
            size: u32_be(entry, 0x04),
            owner_title_id: u64_be(entry, 0x08),
            group_id: u32_be(entry, 0x10),
            hash_mode: FstClusterHashMode::from_u8(entry[0x14]),
        });
    }
    Ok(clusters)
}

fn entry_count(root: FileEntryRaw) -> WupResult<usize> {
    if !root.is_directory() || root.parent_or_offset != 0 {
        return Err(WupError::InvalidFst);
    }
    let count = root.size_or_end_index as usize;
    if count == 0 {
        return Err(WupError::InvalidFst);
    }
    Ok(count)
}

fn walk_entries(
    offset_factor: u32,
    hash_is_disabled: bool,
    clusters: Vec<FstCluster>,
    num_entries: usize,
    mut read_entry: impl FnMut(usize) -> WupResult<FileEntryRaw>,
    mut read_name: impl FnMut(u32) -> WupResult<String>,
) -> WupResult<VirtualFs> {
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
        let entry = read_entry(i)?;
        let name = read_name(entry.name_offset())?;
        if entry.is_file() {
            let path = if path_stack.is_empty() {
                name
            } else {
                let mut path = path_stack.join("/");
                path.push('/');
                path.push_str(&name);
                path
            };
            files.push(VirtualFile {
                path,
                cluster_index: entry.cluster_index,
                file_offset: entry.parent_or_offset,
                file_size: entry.size_or_end_index,
                is_shared: entry.is_shared(),
            });
        } else if i == 0 {
            if entry.size_or_end_index as usize != num_entries {
                return Err(WupError::InvalidFst);
            }
        } else {
            let end = entry.size_or_end_index as usize;
            if end <= i || end > num_entries {
                return Err(WupError::InvalidFst);
            }
            path_stack.push(name);
            dir_end_stack.push(end);
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
/// zero and it is not needed to walk the tree.
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
            type_and_name_offset: u32_be(bytes, 0x00),
            parent_or_offset: u32_be(bytes, 0x04),
            size_or_end_index: u32_be(bytes, 0x08),
            cluster_index: u16_be(bytes, 0x0E),
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
        // Name string table: names are packed back-to-back with
        // NUL terminators, recording each one's starting offset.
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

        write_entry(&mut buf, 0, true, "", 0, 9, 0);
        write_entry(&mut buf, 1, true, "meta", 0, 4, 0);
        write_entry(&mut buf, 2, false, "meta.xml", 0x0000, 0x0100, 0);
        write_entry(&mut buf, 3, false, "icon.tga", 0x0100, 0x4000, 0);
        write_entry(&mut buf, 4, true, "code", 0, 7, 0);
        write_entry(&mut buf, 5, false, "app.xml", 0x4100, 0x0080, 0);
        write_entry(&mut buf, 6, false, "main.rpx", 0x4180, 0x2_0000, 0);
        write_entry(&mut buf, 7, true, "content", 0, 9, 0);
        write_entry(&mut buf, 8, false, "shader.bin", 0x2_4180, 0x8_0000, 0);

        // Name string table
        let names_start = entries_start + entries_size;
        buf[names_start..names_start + name_table.len()].copy_from_slice(&name_table);

        buf
    }

    fn parse_fixture_fst_ranges(bytes: &[u8]) -> WupResult<VirtualFs> {
        parse_fst_ranges(bytes.len() as u64, |offset, len| {
            let start = usize::try_from(offset).map_err(|_| WupError::InvalidFst)?;
            let end = start.checked_add(len).ok_or(WupError::InvalidFst)?;
            Ok(bytes.get(start..end).ok_or(WupError::InvalidFst)?.to_vec())
        })
    }

    #[test]
    fn parses_fixture_header() {
        let fst = parse_fixture_fst_ranges(&build_fixture_fst()).unwrap();
        assert_eq!(fst.offset_factor, 1);
        assert!(!fst.hash_is_disabled);
        assert_eq!(fst.clusters.len(), 1);
        assert_eq!(fst.clusters[0].owner_title_id, 0x0005_000E_1010_2000);
        assert_eq!(fst.clusters[0].hash_mode, FstClusterHashMode::Raw);
    }

    #[test]
    fn parses_fixture_files() {
        let fst = parse_fixture_fst_ranges(&build_fixture_fst()).unwrap();
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
    fn range_parser_reads_bounded_name_windows() {
        let mut bytes = build_fixture_fst();
        let name_table_start = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 9 * FST_FILE_ENTRY_SIZE;
        bytes.extend_from_slice(&[0xAA; 8192]);
        let mut ranges = Vec::new();
        let actual = parse_fst_ranges(bytes.len() as u64, |offset, len| {
            ranges.push((offset, len));
            let start = offset as usize;
            Ok(bytes[start..start + len].to_vec())
        })
        .unwrap();
        assert_eq!(
            actual
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "meta/meta.xml",
                "meta/icon.tga",
                "code/app.xml",
                "code/main.rpx",
                "content/shader.bin",
            ]
        );
        assert!(!ranges.iter().any(|(offset, len)| {
            *offset >= name_table_start as u64
                && (*len > 4096 || *offset + *len as u64 > (name_table_start + 4096) as u64)
        }));
    }

    #[test]
    fn range_parser_reads_entry_table_once_in_memory() {
        let mut bytes = build_fixture_fst();
        let entry_start = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE;
        let original_count = 9;
        let extra_count = 5000;
        let name_table_start = entry_start + original_count * FST_FILE_ENTRY_SIZE;
        let names = bytes[name_table_start..].to_vec();
        let template_start = entry_start + (original_count - 1) * FST_FILE_ENTRY_SIZE;
        let template = bytes[template_start..template_start + FST_FILE_ENTRY_SIZE].to_vec();
        bytes.truncate(name_table_start);
        let entry_count = original_count + extra_count;
        bytes[entry_start + 8..entry_start + 12]
            .copy_from_slice(&(entry_count as u32).to_be_bytes());
        for _ in 0..extra_count {
            bytes.extend_from_slice(&template);
        }
        bytes.extend_from_slice(&names);

        let entry_end = entry_start + entry_count * FST_FILE_ENTRY_SIZE;
        let mut entry_reads = Vec::new();
        let parsed = parse_fst_ranges(bytes.len() as u64, |offset, len| {
            if offset >= entry_start as u64 && offset < entry_end as u64 {
                entry_reads.push((offset, len));
            }
            let start = offset as usize;
            Ok(bytes[start..start + len].to_vec())
        })
        .unwrap();
        assert_eq!(parsed.files.len(), 5 + extra_count);
        assert_eq!(
            entry_reads,
            vec![
                (entry_start as u64, FST_FILE_ENTRY_SIZE),
                (
                    entry_start as u64 + FST_FILE_ENTRY_SIZE as u64,
                    (entry_count - 1) * FST_FILE_ENTRY_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn range_parser_rejects_cluster_table_past_content_before_reading_it() {
        let mut header = [0u8; FST_HEADER_SIZE];
        header[..4].copy_from_slice(&FST_MAGIC.to_be_bytes());
        header[0x04..0x08].copy_from_slice(&1u32.to_be_bytes());
        header[0x08..0x0C].copy_from_slice(&1u32.to_be_bytes());
        let mut calls = 0;
        let result = parse_fst_ranges(FST_HEADER_SIZE as u64, |offset, len| {
            calls += 1;
            assert_eq!((offset, len), (0, FST_HEADER_SIZE));
            Ok(header.to_vec())
        });
        assert!(matches!(result, Err(WupError::InvalidFst)));
        assert_eq!(calls, 1);
    }

    #[test]
    fn range_parser_rejects_entry_table_past_content_before_reading_it() {
        let bytes = build_fixture_fst();
        let content_len = (FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + FST_FILE_ENTRY_SIZE) as u64;
        let mut calls = 0;
        let result = parse_fst_ranges(content_len, |offset, len| {
            calls += 1;
            let start = offset as usize;
            Ok(bytes[start..start + len].to_vec())
        });
        assert!(matches!(result, Err(WupError::InvalidFst)));
        assert_eq!(calls, 3);
    }

    #[test]
    fn range_parser_rejects_unterminated_name_at_string_table_end() {
        let bytes = build_fixture_fst();
        let result = parse_fst_ranges((bytes.len() - 1) as u64, |offset, len| {
            let start = offset as usize;
            Ok(bytes[start..start + len].to_vec())
        });
        assert!(matches!(result, Err(WupError::InvalidFst)));
    }

    #[test]
    fn range_parser_reads_names_larger_than_4kib() {
        let name_len = 8 * 1024;
        let mut bytes = build_fixture_fst();
        let table_start = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 9 * FST_FILE_ENTRY_SIZE;
        let last_entry = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + 8 * FST_FILE_ENTRY_SIZE;
        let name_offset = u32_be(&bytes, last_entry) & 0x00FF_FFFF;
        bytes.truncate(table_start + name_offset as usize);
        bytes.extend(vec![b'x'; name_len]);
        bytes.push(0);

        let parsed = parse_fixture_fst_ranges(&bytes).unwrap();
        assert_eq!(
            parsed.files.last().unwrap().path.len(),
            "content/".len() + name_len
        );
    }
    #[test]
    fn entry_table_ranges_are_decrypted_once() {
        let bytes = build_fixture_fst();
        let table_start = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE;
        let table_end = table_start + 9 * FST_FILE_ENTRY_SIZE;
        let mut requested_table_bytes = 0;
        parse_fst_ranges(bytes.len() as u64, |offset, len| {
            let start = offset as usize;
            let end = start + len;
            requested_table_bytes += end.min(table_end).saturating_sub(start.max(table_start));
            Ok(bytes[start..end].to_vec())
        })
        .unwrap();
        assert_eq!(requested_table_bytes, table_end - table_start);
    }

    #[test]
    fn preserves_file_offsets_and_sizes() {
        let fst = parse_fixture_fst_ranges(&build_fixture_fst()).unwrap();
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
        let fst = parse_fixture_fst_ranges(&build_fixture_fst()).unwrap();
        for file in &fst.files {
            assert_eq!(file.cluster_index, 0);
        }
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = build_fixture_fst();
        bytes[0] = b'X';
        let err = parse_fixture_fst_ranges(&bytes);
        assert!(matches!(err, Err(WupError::InvalidFst)));
    }

    #[test]
    fn rejects_short_header() {
        let short = vec![0u8; FST_HEADER_SIZE - 1];
        let err = parse_fixture_fst_ranges(&short);
        assert!(matches!(err, Err(WupError::InvalidFst)));
    }

    #[test]
    fn rejects_entries_past_buffer() {
        let mut bytes = build_fixture_fst();
        bytes.truncate(FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE + FST_FILE_ENTRY_SIZE);
        let err = parse_fixture_fst_ranges(&bytes);
        assert!(matches!(err, Err(WupError::InvalidFst)));
    }

    #[test]
    fn file_entries_with_type_bit_7_are_flagged_shared() {
        // Flip entry index 4 (code/main.rpx in the fixture) to type
        // 0x80 and confirm the range parser marks it as shared. Every other
        // file keeps is_shared == false.
        let mut bytes = build_fixture_fst();
        // Layout: header + clusters, then entries. Entry 4 is
        // code/main.rpx per the fixture's build order.
        let entries_start = FST_HEADER_SIZE + FST_CLUSTER_ENTRY_SIZE;
        let rpx_entry = entries_start + 6 * FST_FILE_ENTRY_SIZE;
        // Set bit 7 of the type byte (high byte of type_and_name_offset).
        bytes[rpx_entry] |= 0x80;
        let fst = parse_fixture_fst_ranges(&bytes).unwrap();
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
