//! Read-only Switch RomFS walker.
//!
//! The Control NCA's section 0 contains a RomFS image whose root holds
//! `control.nacp` and a handful of `icon_<Language>.dat` files, which
//! [`RomfsReader`] finds by name in an in-memory image. Extraction walks
//! the whole directory tree instead, reading the image through a
//! storage ([`walk_romfs`]). The IVFC hash layers that wrap the RomFS
//! image inside a real NCA are handled by the caller; this module only
//! sees the RomFS image itself.
//!
//! Layout reference: switchbrew.org/wiki/RomFS.

use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::walker::NcaInput;
use crate::util::bytes::u32_le;
use crate::util::{extent_end, is_safe_dirent_name};
use byteorder::{LE, ReadBytesExt};
use std::borrow::Cow;
use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub(crate) const ROMFS_HEADER_SIZE: u64 = 0x50;
/// Largest filesystem table (RomFS metadata, PFS0 header) read into
/// memory, and the most memory one filesystem listing may take.
pub(crate) const MAX_TABLE_SIZE: u64 = 256 << 20;
const INVALID_OFFSET: u32 = 0xFFFF_FFFF;
const DIR_ENTRY_SIZE: usize = 0x18;
const ENTRY_OUTSIDE_TABLE: &str = "metadata entry runs past its table";
const LINK_CYCLE: &str = "directory or file links form a cycle";
const NOT_UTF8: &str = "entry name is not UTF-8";
/// Most directories plus files one walk lists.
const MAX_ENTRIES: usize = 1 << 21;
const TOO_MANY_ENTRIES: &str = "more than 2 Mi directories and files";

/// The directories (parents first) and files of one filesystem, with
/// paths relative to its root.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    pub dirs: Vec<PathBuf>,
    pub files: Vec<TreeFile>,
}

/// One file of a [`Tree`] and where its bytes sit in the storage.
#[derive(Debug)]
pub(crate) struct TreeFile {
    pub path: PathBuf,
    pub offset: u64,
    pub size: u64,
}

/// Fixed-size RomFS header: offsets and sizes of the four metadata
/// tables (directory hash/meta, file hash/meta) plus the start of the
/// file-data block that follows them.
#[derive(Debug, Clone)]
pub struct RomfsHeader {
    pub header_size: u64,
    pub dir_hash_table_offset: u64,
    pub dir_hash_table_size: u64,
    pub dir_meta_table_offset: u64,
    pub dir_meta_table_size: u64,
    pub file_hash_table_offset: u64,
    pub file_hash_table_size: u64,
    pub file_meta_table_offset: u64,
    pub file_meta_table_size: u64,
    pub file_data_offset: u64,
}

impl RomfsHeader {
    /// Parses the header from the start of a RomFS image.
    ///
    /// # Errors
    /// Returns an error if `buf` is shorter than the fixed header size
    /// or its declared `header_size` field is not `0x50`.
    pub fn parse(buf: &[u8]) -> NxResult<Self> {
        if buf.len() < ROMFS_HEADER_SIZE as usize {
            return Err(NxError::InvalidNcaHeader);
        }
        let mut cur = Cursor::new(buf);
        let header_size = cur.read_u64::<LE>()?;
        if header_size != ROMFS_HEADER_SIZE {
            return Err(NxError::InvalidNcaHeader);
        }
        let dir_hash_table_offset = cur.read_u64::<LE>()?;
        let dir_hash_table_size = cur.read_u64::<LE>()?;
        let dir_meta_table_offset = cur.read_u64::<LE>()?;
        let dir_meta_table_size = cur.read_u64::<LE>()?;
        let file_hash_table_offset = cur.read_u64::<LE>()?;
        let file_hash_table_size = cur.read_u64::<LE>()?;
        let file_meta_table_offset = cur.read_u64::<LE>()?;
        let file_meta_table_size = cur.read_u64::<LE>()?;
        let file_data_offset = cur.read_u64::<LE>()?;
        Ok(Self {
            header_size,
            dir_hash_table_offset,
            dir_hash_table_size,
            dir_meta_table_offset,
            dir_meta_table_size,
            file_hash_table_offset,
            file_hash_table_size,
            file_meta_table_offset,
            file_meta_table_size,
            file_data_offset,
        })
    }
}

/// One file entry resolved from the RomFS metadata table.
#[derive(Debug, Clone)]
pub struct RomfsFile {
    pub name: String,
    /// Offset within the file-data block; add `header.file_data_offset`
    /// to translate into a position inside the RomFS image.
    pub data_offset: u64,
    pub data_size: u64,
    pub parent_dir_offset: u32,
}

/// Read-only view over a parsed RomFS image, borrowing the raw bytes
/// and exposing file lookup and data extraction.
pub struct RomfsReader<'a> {
    image: &'a [u8],
    pub header: RomfsHeader,
}

impl<'a> RomfsReader<'a> {
    /// Parses the RomFS header from `image` and wraps it for lookups.
    pub fn new(image: &'a [u8]) -> NxResult<Self> {
        let header = RomfsHeader::parse(image)?;
        Ok(Self { image, header })
    }

    fn table(&self, offset: u64, size: u64) -> NxResult<&[u8]> {
        let start = usize::try_from(offset).map_err(|_| NxError::InvalidNcaHeader)?;
        let size = usize::try_from(size).map_err(|_| NxError::InvalidNcaHeader)?;
        let end = start.checked_add(size).ok_or(NxError::InvalidNcaHeader)?;
        self.image.get(start..end).ok_or(NxError::InvalidNcaHeader)
    }

    /// Walks the file metadata table and returns every file entry it
    /// finds, following each entry's sibling offset (falling back to
    /// the next fixed-layout record when the sibling link is absent).
    pub fn list_files(&self) -> NxResult<Vec<RomfsFile>> {
        let table = self.table(
            self.header.file_meta_table_offset,
            self.header.file_meta_table_size,
        )?;
        let meta_size = table.len();

        let mut out = Vec::new();
        let mut visited = std::collections::HashSet::new();
        // Real entries are disjoint, so their combined footprint can
        // never exceed the table; an overlapping chain would otherwise
        // re-copy the same bytes for every visited entry.
        let mut budget: usize = 0;
        let mut cursor: u32 = 0;
        while (cursor as usize) < meta_size && cursor != INVALID_OFFSET {
            if !visited.insert(cursor) {
                // Defensive: should never see the same offset twice
                // in a well-formed RomFS, but skip rather than loop.
                break;
            }
            let entry = parse_file_entry(table, cursor as usize)?;
            budget = budget
                .checked_add(0x20 + entry.name_length as usize)
                .ok_or(NxError::InvalidNcaHeader)?;
            if budget > meta_size {
                return Err(NxError::InvalidNcaHeader);
            }
            let next = entry.next_sibling;
            let name_padded = entry.name_length_padded();
            out.push(RomfsFile {
                name: entry.name.into_owned(),
                data_offset: entry.data_offset,
                data_size: entry.data_size,
                parent_dir_offset: entry.parent_dir_offset,
            });
            // Advance past the variable-length entry: 0x20 fixed bytes
            // plus the name padded to 4. Subsequent files are written
            // immediately after; this is used as the implicit cursor when
            // `next` is INVALID_OFFSET (top-level walk).
            if next != INVALID_OFFSET && (next as usize) < meta_size {
                cursor = next;
            } else {
                cursor = cursor
                    .checked_add(0x20 + name_padded)
                    .ok_or(NxError::InvalidNcaHeader)?;
            }
        }
        Ok(out)
    }

    /// Looks up a file named `name` directly under the RomFS root
    /// directory (offset `0`).
    pub fn find_root_file(&self, name: &str) -> NxResult<Option<RomfsFile>> {
        self.find_file(0, name)
    }

    /// Looks up a file named `name` under the directory at
    /// `parent_dir_offset`. Tries the file hash table first and falls
    /// back to a linear scan of all files if the table is missing or
    /// too small to use.
    pub fn find_file(&self, parent_dir_offset: u32, name: &str) -> NxResult<Option<RomfsFile>> {
        if let Some(found) = self.find_via_hash_table(parent_dir_offset, name)? {
            return Ok(Some(found));
        }
        for f in self.list_files()? {
            if f.parent_dir_offset == parent_dir_offset && f.name == name {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    fn find_via_hash_table(
        &self,
        parent_dir_offset: u32,
        name: &str,
    ) -> NxResult<Option<RomfsFile>> {
        if self.header.file_hash_table_size < 4 {
            return Ok(None);
        }
        let hash_table = self.table(
            self.header.file_hash_table_offset,
            self.header.file_hash_table_size,
        )?;
        let buckets = hash_table.len() / 4;
        let bucket = (compute_file_hash(parent_dir_offset, name) as usize) % buckets;
        let bucket_off = bucket * 4;
        let mut entry_offset = u32::from_le_bytes([
            hash_table[bucket_off],
            hash_table[bucket_off + 1],
            hash_table[bucket_off + 2],
            hash_table[bucket_off + 3],
        ]);

        let meta_table = self.table(
            self.header.file_meta_table_offset,
            self.header.file_meta_table_size,
        )?;
        let mut visited = std::collections::HashSet::new();
        // Real entries are disjoint, so their combined footprint can
        // never exceed the table; an overlapping chain would otherwise
        // re-copy the same bytes for every visited entry.
        let mut budget: usize = 0;

        while entry_offset != INVALID_OFFSET {
            if !visited.insert(entry_offset) {
                return Err(NxError::InvalidNcaHeader);
            }
            let entry = parse_file_entry(meta_table, entry_offset as usize)?;
            budget = budget
                .checked_add(0x20 + entry.name_length as usize)
                .ok_or(NxError::InvalidNcaHeader)?;
            if budget > meta_table.len() {
                return Err(NxError::InvalidNcaHeader);
            }
            if entry.parent_dir_offset == parent_dir_offset && entry.name == name {
                return Ok(Some(RomfsFile {
                    name: entry.name.into_owned(),
                    data_offset: entry.data_offset,
                    data_size: entry.data_size,
                    parent_dir_offset: entry.parent_dir_offset,
                }));
            }
            entry_offset = entry.next_hash;
        }
        Ok(None)
    }

    /// Reads a file's raw data bytes out of the image, at
    /// `header.file_data_offset + file.data_offset`.
    ///
    /// # Errors
    /// Returns an error if the file's extent overflows or falls outside
    /// the image.
    pub fn read_file(&self, file: &RomfsFile) -> NxResult<Vec<u8>> {
        let start = self
            .header
            .file_data_offset
            .checked_add(file.data_offset)
            .ok_or(NxError::InvalidNcaHeader)?;
        let end = start
            .checked_add(file.data_size)
            .ok_or(NxError::InvalidNcaHeader)?;
        let start = usize::try_from(start).map_err(|_| NxError::InvalidNcaHeader)?;
        let end = usize::try_from(end).map_err(|_| NxError::InvalidNcaHeader)?;
        let data = self
            .image
            .get(start..end)
            .ok_or(NxError::InvalidNcaHeader)?;
        Ok(data.to_vec())
    }
}

/// Lists every directory (parents first) and file of the RomFS image
/// of `size` bytes at `base` in `storage`, from its directory and file
/// metadata tables. Paths are relative to the RomFS root.
///
/// # Errors
/// Fails with [`NxError::InvalidRomfs`] when a metadata table is larger
/// than 256 MiB or lies outside the image, a link leaves its table or
/// leads back to a visited entry, a file's data lies outside the image,
/// the file sizes add up to more than the image, or the listing would
/// hold more than 2 Mi entries or take more than 256 MiB; with
/// [`NxError::UnsafeEntryName`] for a name that cannot be a file name.
pub(crate) fn walk_romfs(storage: &dyn NcaInput, base: u64, size: u64) -> NxResult<Tree> {
    let header = read_romfs_header(storage, base, size)?;
    let dir_table = read_meta_table(
        storage,
        base,
        size,
        header.dir_meta_table_offset,
        header.dir_meta_table_size,
    )?;
    let file_table = read_meta_table(
        storage,
        base,
        size,
        header.file_meta_table_offset,
        header.file_meta_table_size,
    )?;

    let mut tree = Tree::default();
    let mut seen_dirs = HashSet::from([0u32]);
    let mut seen_files = HashSet::new();
    let mut budget = MAX_TABLE_SIZE;
    // Extents may overlap or repeat, but files holding more bytes than
    // the image would let a small input write without bound.
    let mut data_left = size;
    let (root, _) = dir_entry(&dir_table, 0)?;
    let mut pending = vec![(u32_le(root, 8), u32_le(root, 0xC), PathBuf::new())];
    let full = |tree: &Tree| tree.dirs.len() + tree.files.len() >= MAX_ENTRIES;
    while let Some((mut child, mut file, dir)) = pending.pop() {
        while file != INVALID_OFFSET {
            if !seen_files.insert(file) {
                return Err(NxError::InvalidRomfs(LINK_CYCLE));
            }
            if full(&tree) {
                return Err(NxError::InvalidRomfs(TOO_MANY_ENTRIES));
            }
            let entry = parse_file_entry(&file_table, file as usize)
                .map_err(|_| NxError::InvalidRomfs(ENTRY_OUTSIDE_TABLE))?;
            // A lossy decode allocates exactly when the bytes are not UTF-8.
            if let Cow::Owned(_) = entry.name {
                return Err(NxError::InvalidRomfs(NOT_UTF8));
            }
            let path = child_path(&dir, &entry.name, &mut budget)?;
            let start = header
                .file_data_offset
                .checked_add(entry.data_offset)
                .filter(|start| extent_end(*start, entry.data_size, size).is_some())
                .ok_or(NxError::InvalidRomfs("file data lies outside the image"))?;
            data_left = data_left
                .checked_sub(entry.data_size)
                .ok_or(NxError::InvalidRomfs(
                    "file sizes add up to more than the image",
                ))?;
            tree.files.push(TreeFile {
                path,
                offset: base + start,
                size: entry.data_size,
            });
            file = entry.next_sibling;
        }
        while child != INVALID_OFFSET {
            if !seen_dirs.insert(child) {
                return Err(NxError::InvalidRomfs(LINK_CYCLE));
            }
            if full(&tree) {
                return Err(NxError::InvalidRomfs(TOO_MANY_ENTRIES));
            }
            let (entry, name) = dir_entry(&dir_table, child)?;
            let path = child_path(&dir, name, &mut budget)?;
            tree.dirs.push(path.clone());
            pending.push((u32_le(entry, 8), u32_le(entry, 0xC), path));
            child = u32_le(entry, 4);
        }
    }
    Ok(tree)
}

/// Reads the header of the RomFS image of `size` bytes at `base` in
/// `storage`, checking that its four tables lie inside the image.
///
/// # Errors
/// Fails with [`NxError::InvalidNcaHeader`] on a wrong header size and
/// with [`NxError::InvalidRomfs`] for a table outside the image.
pub(crate) fn read_romfs_header(
    storage: &dyn NcaInput,
    base: u64,
    size: u64,
) -> NxResult<RomfsHeader> {
    if size < ROMFS_HEADER_SIZE {
        return Err(NxError::InvalidRomfs("image is smaller than its header"));
    }
    let mut head = [0u8; ROMFS_HEADER_SIZE as usize];
    storage.read_exact_at(&mut head, base)?;
    let header = RomfsHeader::parse(&head)?;
    let tables = [
        (header.dir_hash_table_offset, header.dir_hash_table_size),
        (header.dir_meta_table_offset, header.dir_meta_table_size),
        (header.file_hash_table_offset, header.file_hash_table_size),
        (header.file_meta_table_offset, header.file_meta_table_size),
    ];
    if tables
        .iter()
        .any(|&(offset, len)| extent_end(offset, len, size).is_none())
    {
        return Err(NxError::InvalidRomfs(
            "metadata table lies outside the image",
        ));
    }
    Ok(header)
}

/// Reads one metadata table after checking it fits the image and the
/// size cap, so a corrupt header cannot claim a huge allocation.
fn read_meta_table(
    storage: &dyn NcaInput,
    base: u64,
    image_size: u64,
    offset: u64,
    size: u64,
) -> NxResult<Vec<u8>> {
    if size > MAX_TABLE_SIZE {
        return Err(NxError::InvalidRomfs(
            "metadata table is larger than 256 MiB",
        ));
    }
    if extent_end(offset, size, image_size).is_none() {
        return Err(NxError::InvalidRomfs(
            "metadata table lies outside the image",
        ));
    }
    let mut table = vec![0u8; size as usize];
    storage.read_exact_at(&mut table, base + offset)?;
    Ok(table)
}

/// Splits the directory entry at `offset` into its fixed head, whose
/// last field is the name length, and the name that follows it.
fn dir_entry(table: &[u8], offset: u32) -> NxResult<(&[u8], &str)> {
    let entry = table
        .get(offset as usize..)
        .filter(|rest| rest.len() >= DIR_ENTRY_SIZE)
        .ok_or(NxError::InvalidRomfs(ENTRY_OUTSIDE_TABLE))?;
    let name_len = u32_le(entry, DIR_ENTRY_SIZE - 4) as usize;
    let name = entry[DIR_ENTRY_SIZE..]
        .get(..name_len)
        .ok_or(NxError::InvalidRomfs(ENTRY_OUTSIDE_TABLE))?;
    let name = std::str::from_utf8(name).map_err(|_| NxError::InvalidRomfs(NOT_UTF8))?;
    Ok((&entry[..DIR_ENTRY_SIZE], name))
}

/// `dir/name` once `name` is a safe file name, charged with the list
/// entry it becomes against the budget that bounds the walk's memory.
fn child_path(dir: &Path, name: &str, budget: &mut u64) -> NxResult<PathBuf> {
    if !is_safe_dirent_name(name) {
        return Err(NxError::UnsafeEntryName(name.to_string()));
    }
    let path = dir.join(name);
    let cost = (path.as_os_str().len() + size_of::<TreeFile>()) as u64;
    *budget = budget
        .checked_sub(cost)
        .ok_or(NxError::InvalidRomfs("the listing is larger than 256 MiB"))?;
    Ok(path)
}

#[derive(Debug)]
struct FileEntryRaw<'a> {
    parent_dir_offset: u32,
    next_sibling: u32,
    data_offset: u64,
    data_size: u64,
    next_hash: u32,
    name: Cow<'a, str>,
    name_length: u32,
}

impl FileEntryRaw<'_> {
    fn name_length_padded(&self) -> u32 {
        (self.name_length + 3) & !3
    }
}

fn parse_file_entry(table: &[u8], offset: usize) -> NxResult<FileEntryRaw<'_>> {
    // Checked ends keep a 32-bit build from wrapping past the bounds.
    let name_start = offset.checked_add(0x20).ok_or(NxError::InvalidNcaHeader)?;
    let fixed = table
        .get(offset..name_start)
        .ok_or(NxError::InvalidNcaHeader)?;
    let mut cur = Cursor::new(fixed);
    let parent_dir_offset = cur.read_u32::<LE>()?;
    let next_sibling = cur.read_u32::<LE>()?;
    let data_offset = cur.read_u64::<LE>()?;
    let data_size = cur.read_u64::<LE>()?;
    let next_hash = cur.read_u32::<LE>()?;
    let name_length = cur.read_u32::<LE>()?;
    let name = name_start
        .checked_add(name_length as usize)
        .and_then(|name_end| table.get(name_start..name_end))
        .ok_or(NxError::InvalidNcaHeader)?;
    let name = String::from_utf8_lossy(name);
    Ok(FileEntryRaw {
        parent_dir_offset,
        next_sibling,
        data_offset,
        data_size,
        next_hash,
        name,
        name_length,
    })
}

pub(crate) fn compute_file_hash(parent_dir_offset: u32, name: &str) -> u32 {
    let mut hash = parent_dir_offset ^ 123_456_789;
    for &b in name.as_bytes() {
        hash = hash.rotate_right(5) ^ u32::from(b);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;
    use std::io::Write;

    fn build_test_romfs() -> Vec<u8> {
        const FILE_HASH_BUCKETS: u64 = 8;

        let header_size: u64 = 0x50;
        let dir_hash_offset = header_size;
        let dir_hash_size: u64 = 4;
        let dir_meta_offset = dir_hash_offset + dir_hash_size;
        let dir_meta_size: u64 = 0x18;
        let file_hash_offset = dir_meta_offset + dir_meta_size;
        let file_hash_size: u64 = FILE_HASH_BUCKETS * 4;
        let file_meta_offset = file_hash_offset + file_hash_size;

        let file1_name = b"control.nacp";
        let file2_name = b"icon_AmericanEnglish.dat";
        let f1_padded = ((file1_name.len() as u32 + 3) & !3) as u64;
        let f2_padded = ((file2_name.len() as u32 + 3) & !3) as u64;
        let f1_record = 0x20u64 + f1_padded;
        let f2_record = 0x20u64 + f2_padded;
        let file_meta_size = f1_record + f2_record;
        let file_data_offset = file_meta_offset + file_meta_size;

        let f1_meta_offset = 0u32;
        let f2_meta_offset = f1_record as u32;

        let h1 = compute_file_hash(0, std::str::from_utf8(file1_name).unwrap());
        let h2 = compute_file_hash(0, std::str::from_utf8(file2_name).unwrap());
        let b1 = (h1 as usize) % FILE_HASH_BUCKETS as usize;
        let b2 = (h2 as usize) % FILE_HASH_BUCKETS as usize;

        let mut buckets = vec![INVALID_OFFSET; FILE_HASH_BUCKETS as usize];
        let mut f1_next_hash = INVALID_OFFSET;
        let f2_next_hash = INVALID_OFFSET;
        buckets[b1] = f1_meta_offset;
        if b1 == b2 {
            f1_next_hash = f2_meta_offset;
        } else {
            buckets[b2] = f2_meta_offset;
        }

        let mut image = Vec::new();
        image.write_u64::<LE>(header_size).unwrap();
        image.write_u64::<LE>(dir_hash_offset).unwrap();
        image.write_u64::<LE>(dir_hash_size).unwrap();
        image.write_u64::<LE>(dir_meta_offset).unwrap();
        image.write_u64::<LE>(dir_meta_size).unwrap();
        image.write_u64::<LE>(file_hash_offset).unwrap();
        image.write_u64::<LE>(file_hash_size).unwrap();
        image.write_u64::<LE>(file_meta_offset).unwrap();
        image.write_u64::<LE>(file_meta_size).unwrap();
        image.write_u64::<LE>(file_data_offset).unwrap();
        assert_eq!(image.len() as u64, header_size);

        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u32::<LE>(0).unwrap();
        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u32::<LE>(0).unwrap();

        for bucket in &buckets {
            image.write_u32::<LE>(*bucket).unwrap();
        }

        image.write_u32::<LE>(0).unwrap();
        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u64::<LE>(0).unwrap();
        image.write_u64::<LE>(8).unwrap();
        image.write_u32::<LE>(f1_next_hash).unwrap();
        image.write_u32::<LE>(file1_name.len() as u32).unwrap();
        image.write_all(file1_name).unwrap();
        for _ in 0..(f1_padded as usize - file1_name.len()) {
            image.write_u8(0).unwrap();
        }

        image.write_u32::<LE>(0).unwrap();
        image.write_u32::<LE>(INVALID_OFFSET).unwrap();
        image.write_u64::<LE>(8).unwrap();
        image.write_u64::<LE>(4).unwrap();
        image.write_u32::<LE>(f2_next_hash).unwrap();
        image.write_u32::<LE>(file2_name.len() as u32).unwrap();
        image.write_all(file2_name).unwrap();
        for _ in 0..(f2_padded as usize - file2_name.len()) {
            image.write_u8(0).unwrap();
        }

        image.extend_from_slice(b"NACPDATA");
        image.extend_from_slice(b"ICON");
        image
    }

    #[test]
    fn parses_header() {
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();
        assert_eq!(reader.header.header_size, 0x50);
        assert!(reader.header.file_meta_table_size > 0);
    }

    #[test]
    fn lists_root_files() {
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();
        let files = reader.list_files().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "control.nacp");
        assert_eq!(files[1].name, "icon_AmericanEnglish.dat");
    }

    #[test]
    fn finds_root_file_by_name() {
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();
        let f = reader.find_root_file("control.nacp").unwrap().unwrap();
        assert_eq!(f.data_size, 8);
        let data = reader.read_file(&f).unwrap();
        assert_eq!(data, b"NACPDATA");
    }

    #[test]
    fn missing_file_returns_none() {
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();
        assert!(reader.find_root_file("nope.txt").unwrap().is_none());
    }

    #[test]
    fn rejects_short_header() {
        assert!(RomfsHeader::parse(&[0u8; 16]).is_err());
    }

    #[test]
    fn rejects_wrong_header_size() {
        let mut buf = vec![0u8; 0x50];
        buf[0..8].copy_from_slice(&0x40u64.to_le_bytes());
        assert!(RomfsHeader::parse(&buf).is_err());
    }

    #[test]
    fn hash_lookup_matches_fixture_files() {
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();

        let nacp = reader.find_root_file("control.nacp").unwrap().unwrap();
        assert_eq!(nacp.data_size, 8);
        assert_eq!(reader.read_file(&nacp).unwrap(), b"NACPDATA");

        let icon = reader
            .find_root_file("icon_AmericanEnglish.dat")
            .unwrap()
            .unwrap();
        assert_eq!(icon.data_size, 4);
        assert_eq!(reader.read_file(&icon).unwrap(), b"ICON");

        assert!(reader.find_root_file("missing.bin").unwrap().is_none());
    }

    #[test]
    fn hash_chain_walks_bucket_collisions() {
        let buckets: u64 = 8;
        let h1 = compute_file_hash(0, "control.nacp") as u64 % buckets;
        let h2 = compute_file_hash(0, "icon_AmericanEnglish.dat") as u64 % buckets;
        if h1 != h2 {
            return;
        }
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();
        assert!(reader.find_root_file("control.nacp").unwrap().is_some());
        assert!(
            reader
                .find_root_file("icon_AmericanEnglish.dat")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn rejects_overflowing_file_extent() {
        let image = build_test_romfs();
        let reader = RomfsReader::new(&image).unwrap();
        let mut file = reader.find_root_file("control.nacp").unwrap().unwrap();
        file.data_size = u64::MAX;
        assert!(matches!(
            reader.read_file(&file),
            Err(NxError::InvalidNcaHeader)
        ));
        file.data_offset = u64::MAX;
        file.data_size = 0;
        assert!(matches!(
            reader.read_file(&file),
            Err(NxError::InvalidNcaHeader)
        ));
    }

    #[test]
    fn rejects_overflowing_listing_table() {
        let mut image = build_test_romfs();
        image[0x30..0x38].copy_from_slice(&0u64.to_le_bytes());
        image[0x40..0x48].copy_from_slice(&u64::MAX.to_le_bytes());
        let reader = RomfsReader::new(&image).unwrap();
        assert!(matches!(
            reader.list_files(),
            Err(NxError::InvalidNcaHeader)
        ));
        assert!(matches!(
            reader.find_root_file("control.nacp"),
            Err(NxError::InvalidNcaHeader)
        ));
    }

    #[test]
    fn rejects_overflowing_hash_lookup_tables() {
        for size_offset in [0x30, 0x40] {
            let mut image = build_test_romfs();
            image[size_offset..size_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            let reader = RomfsReader::new(&image).unwrap();
            assert!(matches!(
                reader.find_via_hash_table(0, "control.nacp"),
                Err(NxError::InvalidNcaHeader)
            ));
        }
    }

    #[test]
    fn rejects_self_linked_hash_entry() {
        let mut image = build_test_romfs();
        let header = RomfsHeader::parse(&image).unwrap();
        let hash_offset = header.file_hash_table_offset as usize;
        let buckets = header.file_hash_table_size as usize / 4;
        let bucket = compute_file_hash(0, "missing.bin") as usize % buckets;
        let bucket_offset = hash_offset + bucket * 4;
        image[bucket_offset..bucket_offset + 4].copy_from_slice(&0u32.to_le_bytes());
        let next_hash_offset = header.file_meta_table_offset as usize + 0x18;
        image[next_hash_offset..next_hash_offset + 4].copy_from_slice(&0u32.to_le_bytes());
        let reader = RomfsReader::new(&image).unwrap();
        assert!(matches!(
            reader.find_root_file("missing.bin"),
            Err(NxError::InvalidNcaHeader)
        ));
    }

    #[test]
    fn rejects_overlapping_stride_four_entries() {
        let mut image = build_test_romfs();
        let header = RomfsHeader::parse(&image).unwrap();
        let base = header.file_meta_table_offset as usize;
        // Three entries at 0x00/0x04/0x08 overlap, so each u32 word
        // serves up to three fields at once (e.g. word 0x1c is entry
        // 0's name_length and entry 4's next_hash). Every parse stays
        // in bounds, but the claimed extents are mutually exclusive,
        // so the byte budget trips on the third entry instead of
        // re-copying the same overlapping bytes.
        let words: [u32; 10] = [0, 4, 8, 12, 0, 0, 4, 8, 12, 16];
        for (index, value) in words.iter().enumerate() {
            let at = base + index * 4;
            image[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let reader = RomfsReader::new(&image).unwrap();
        assert!(matches!(
            reader.list_files(),
            Err(NxError::InvalidNcaHeader)
        ));
        assert!(matches!(
            reader.find_root_file("control.nacp"),
            Err(NxError::InvalidNcaHeader)
        ));
    }

    mod walk {
        use super::*;
        use crate::nintendo::nx::test_fixtures::{MemInput, noise, romfs_image};

        fn nested() -> Vec<(&'static str, Vec<u8>)> {
            vec![
                ("a.bin", noise(0x3000, 1)),
                ("dir/sub/b.txt", b"base text\n".to_vec()),
                ("dir/c.dat", noise(0x1234, 2)),
                ("dir/empty.bin", Vec::new()),
                ("z/deep/er/f", noise(0x10, 3)),
            ]
        }

        fn walk(image: Vec<u8>) -> NxResult<Tree> {
            let size = image.len() as u64;
            walk_romfs(&MemInput(image), 0, size)
        }

        fn dir_entry_offset(image: &[u8], dir: usize) -> usize {
            let header = RomfsHeader::parse(image).unwrap();
            let mut at = header.dir_meta_table_offset as usize;
            for _ in 0..dir {
                let name_len = u32_le(image, at + 0x14) as usize;
                at += 0x18 + name_len.next_multiple_of(4);
            }
            at
        }

        #[test]
        fn lists_nested_tree_with_parents_first() {
            let files = nested();
            let (image, offsets) = romfs_image(&files);
            let tree = walk(image.clone()).unwrap();

            for (i, dir) in tree.dirs.iter().enumerate() {
                if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                    assert!(tree.dirs[..i].iter().any(|d| d == parent), "{dir:?}");
                }
            }
            let dirs: HashSet<PathBuf> = tree.dirs.iter().cloned().collect();
            let want: HashSet<PathBuf> = ["dir", "dir/sub", "z", "z/deep", "z/deep/er"]
                .iter()
                .map(PathBuf::from)
                .collect();
            assert_eq!(dirs, want);
            assert_eq!(tree.files.len(), files.len());
            for ((path, data), offset) in files.iter().zip(offsets) {
                let file = tree
                    .files
                    .iter()
                    .find(|f| f.path == Path::new(path))
                    .unwrap();
                assert_eq!((file.offset, file.size), (offset, data.len() as u64));
                assert_eq!(&image[offset as usize..][..data.len()], data.as_slice());
            }
        }

        #[test]
        fn unsafe_names_fail() {
            for path in ["../x", "a/../../x", "a\\b"] {
                let (image, _) = romfs_image(&[(path, vec![1])]);
                assert!(
                    matches!(walk(image), Err(NxError::UnsafeEntryName(_))),
                    "{path}"
                );
            }

            let (mut image, _) = romfs_image(&[("ab", vec![1])]);
            let name = RomfsHeader::parse(&image).unwrap().file_meta_table_offset as usize + 0x20;
            image[name..name + 2].copy_from_slice(&[0xFF, 0xFE]);
            assert!(matches!(walk(image), Err(NxError::InvalidRomfs(NOT_UTF8))));
        }

        #[test]
        fn cyclic_links_fail() {
            // Root's child dir `dir` links back to the root as its sibling.
            let (mut image, _) = romfs_image(&nested());
            let at = dir_entry_offset(&image, 1) + 4;
            image[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
            assert!(matches!(walk(image), Err(NxError::InvalidRomfs(_))));

            // The root's first file names itself as its sibling.
            let (mut image, _) = romfs_image(&nested());
            let header = RomfsHeader::parse(&image).unwrap();
            let at = header.file_meta_table_offset as usize + 4;
            image[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
            assert!(matches!(walk(image), Err(NxError::InvalidRomfs(_))));

            // `dir` names itself as its first child.
            let (mut image, _) = romfs_image(&nested());
            let header = RomfsHeader::parse(&image).unwrap();
            let at = dir_entry_offset(&image, 1);
            let own = (at - header.dir_meta_table_offset as usize) as u32;
            image[at + 8..at + 12].copy_from_slice(&own.to_le_bytes());
            assert!(matches!(
                walk(image),
                Err(NxError::InvalidRomfs(LINK_CYCLE))
            ));
        }

        #[test]
        fn file_sizes_past_the_image_fail() {
            // Both files cover all file data; each fits, but together they
            // hold more bytes than the image.
            let (mut image, _) = romfs_image(&[("a", vec![1; 0x400]), ("b", vec![2; 0x400])]);
            let header = RomfsHeader::parse(&image).unwrap();
            let all = image.len() as u64 - header.file_data_offset;
            let table = header.file_meta_table_offset as usize;
            for at in [table, table + 0x24] {
                image[at + 8..at + 0x10].copy_from_slice(&0u64.to_le_bytes());
                image[at + 0x10..at + 0x18].copy_from_slice(&all.to_le_bytes());
            }
            assert!(matches!(walk(image), Err(NxError::InvalidRomfs(_))));
        }

        #[test]
        fn oversized_or_misplaced_tables_fail_before_allocating() {
            // A 257 MiB table inside a 1 GiB image is refused for its size.
            let (mut image, _) = romfs_image(&nested());
            image[0x20..0x28].copy_from_slice(&(257u64 << 20).to_le_bytes());
            assert!(matches!(
                walk_romfs(&MemInput(image), 0, 1 << 30),
                Err(NxError::InvalidRomfs(_))
            ));

            let (mut image, _) = romfs_image(&nested());
            let len = image.len() as u64;
            image[0x38..0x40].copy_from_slice(&len.to_le_bytes());
            assert!(matches!(walk(image), Err(NxError::InvalidRomfs(_))));
        }

        #[test]
        fn file_data_outside_the_image_fails() {
            let (mut image, _) = romfs_image(&nested());
            let header = RomfsHeader::parse(&image).unwrap();
            let at = header.file_meta_table_offset as usize + 0x10;
            image[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            assert!(matches!(walk(image), Err(NxError::InvalidRomfs(_))));
        }
    }
}
