//! Directory entries and the dirtab BST walk.

use std::collections::HashSet;
use std::io::{Read, Seek};

use super::error::{XdvdfsError, XdvdfsResult};
use super::{ATTR_DIRECTORY, XdvdfsVolume, data_offset};

/// Largest a directory table may be: past this the u16 word offsets a
/// dirent's `left`/`right` children use can no longer address every entry.
/// Matches `xbox::create`'s `MAX_DIRTAB_BYTES`.
const MAX_DIRTAB_SIZE: u32 = 262_140;

/// Unicode codepoints for Windows-1252 bytes 0x80-0x9F, in order.
/// Undefined positions (0x81, 0x8D, 0x8F, 0x90, 0x9D) map to their C1
/// control codepoint, matching the WHATWG "windows-1252" encoding used by
/// browsers and by Windows itself for round-tripping.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Decodes a Windows-1252 byte string. 0x00-0x7F is ASCII, 0xA0-0xFF is
/// Latin-1 (identical to the Unicode codepoint), and 0x80-0x9F is remapped
/// via [`CP1252_HIGH`].
fn decode_cp1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

/// One XDVDFS directory entry (dirent): a BST node plus its file/directory
/// payload location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub left: u16,
    pub right: u16,
    pub start_sector: u32,
    pub size: u32,
    pub attributes: u8,
    pub name: Vec<u8>,
}

impl DirEntry {
    pub fn is_directory(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    /// Decodes [`Self::name`] as Windows-1252.
    pub fn name_str(&self) -> String {
        decode_cp1252(&self.name)
    }
}

/// A child-offset sentinel: both `0` and `0xFFFF` mean "no child".
fn is_none_child(offset: u16) -> bool {
    offset == 0 || offset == 0xFFFF
}

/// Reads one directory table (`size` bytes starting at `sector`) fully
/// into memory. Tool-built images round `size` up to a sector multiple,
/// but retail masters record the exact byte length (e.g. a 116-byte root
/// table), so any size within the u16 offset range is accepted.
fn read_dirtab<R: Read + Seek>(
    reader: &mut R,
    volume: &XdvdfsVolume,
    sector: u32,
    size: u32,
) -> XdvdfsResult<Vec<u8>> {
    if size > MAX_DIRTAB_SIZE {
        return Err(XdvdfsError::InvalidDirent {
            offset: size as usize,
            reason: "directory table size exceeds the u16 offset range",
        });
    }
    reader.seek(std::io::SeekFrom::Start(data_offset(volume, sector)))?;
    let mut buf = vec![0u8; size as usize];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

/// Parses the dirent at `offset` in a buffered dirtab, returning `None` for
/// padding (a run of all-`0xFF` or all-`0x00`).
fn parse_dirent(table: &[u8], offset: usize) -> XdvdfsResult<Option<DirEntry>> {
    let fixed = table
        .get(offset..offset + 14)
        .ok_or(XdvdfsError::InvalidDirent {
            offset,
            reason: "fixed part past end of dirtab",
        })?;

    if fixed.iter().all(|&b| b == 0xFF) || fixed.iter().all(|&b| b == 0x00) {
        return Ok(None);
    }

    let left = u16::from_le_bytes([fixed[0], fixed[1]]);
    let right = u16::from_le_bytes([fixed[2], fixed[3]]);
    let start_sector = u32::from_le_bytes(fixed[4..8].try_into().unwrap());
    let size = u32::from_le_bytes(fixed[8..12].try_into().unwrap());
    let attributes = fixed[12];
    let name_len = fixed[13] as usize;

    let name_start = offset + 14;
    let name = table
        .get(name_start..name_start + name_len)
        .ok_or(XdvdfsError::InvalidDirent {
            offset,
            reason: "filename runs past end of dirtab",
        })?
        .to_vec();

    Ok(Some(DirEntry {
        left,
        right,
        start_sector,
        size,
        attributes,
        name,
    }))
}

/// Walks one directory table's own entries via its BST (spec section 3's
/// iterative stack walk), calling `on_entry` for each. Does not descend
/// into subdirectories.
fn walk_table<R, F>(
    reader: &mut R,
    volume: &XdvdfsVolume,
    sector: u32,
    size: u32,
    mut on_entry: F,
) -> XdvdfsResult<()>
where
    R: Read + Seek,
    F: FnMut(&DirEntry) -> XdvdfsResult<()>,
{
    let table = read_dirtab(reader, volume, sector, size)?;
    let mut offsets = vec![0usize];
    // Offsets already visited within this table's BST: a left/right
    // pointer cycle would otherwise loop forever.
    let mut visited_offsets: HashSet<usize> = HashSet::new();

    while let Some(off) = offsets.pop() {
        if !visited_offsets.insert(off) {
            return Err(XdvdfsError::InvalidDirent {
                offset: off,
                reason: "dirtab offset already visited (cyclic BST)",
            });
        }

        let Some(entry) = parse_dirent(&table, off)? else {
            continue;
        };

        if !is_none_child(entry.right) {
            offsets.push(entry.right as usize * 4);
        }
        if !is_none_child(entry.left) {
            offsets.push(entry.left as usize * 4);
        }

        on_entry(&entry)?;
    }
    Ok(())
}

/// Walks every directory table reachable from the volume's root, depth
/// first, buffering each dirtab once and then following its BST via
/// [`walk_table`].
///
/// `visit` is called with the parent path components (not including the
/// entry's own name) and the entry itself.
pub fn walk_dir_tables<R, F>(
    reader: &mut R,
    volume: &XdvdfsVolume,
    mut visit: F,
) -> XdvdfsResult<()>
where
    R: Read + Seek,
    F: FnMut(&[String], &DirEntry) -> XdvdfsResult<()>,
{
    // root_sector == 0 && root_size == 0 is a valid empty volume: no dirtab
    // to read at all (distinct from an empty directory, which is one
    // 0xFF-filled sector with size == 2048).
    if volume.root_size == 0 {
        return Ok(());
    }

    let mut dirs: Vec<(Vec<String>, u32, u32)> =
        vec![(Vec::new(), volume.root_sector, volume.root_size)];
    // Sectors already walked as a directory table, across the whole
    // volume: a dirent pointing back at one would otherwise re-descend
    // forever.
    let mut visited_sectors: HashSet<u32> = HashSet::new();

    while let Some((path, sector, size)) = dirs.pop() {
        // A subdirectory can legitimately have size 0 (e.g. a corrupt or
        // minimal image); there is no table to read, just no entries.
        if size == 0 {
            continue;
        }
        if !visited_sectors.insert(sector) {
            return Err(XdvdfsError::InvalidDirent {
                offset: sector as usize,
                reason: "directory table sector already visited (cyclic dirtab)",
            });
        }

        let mut children: Vec<(Vec<String>, u32, u32)> = Vec::new();
        walk_table(reader, volume, sector, size, |entry| {
            if entry.is_directory() {
                let mut child_path = path.clone();
                child_path.push(entry.name_str());
                children.push((child_path, entry.start_sector, entry.size));
            }
            visit(&path, entry)
        })?;
        dirs.extend(children);
    }

    Ok(())
}

/// Walks just the root directory table's own entries, without descending
/// into subdirectories. For callers that only need a root-level check
/// (e.g. `info::sniff_xdvdfs`'s trimmed-image classifier looking for
/// `default.xex`) and would otherwise pay for a full recursive walk.
pub fn walk_root_table<R, F>(reader: &mut R, volume: &XdvdfsVolume, visit: F) -> XdvdfsResult<()>
where
    R: Read + Seek,
    F: FnMut(&DirEntry) -> XdvdfsResult<()>,
{
    if volume.root_size == 0 {
        return Ok(());
    }
    walk_table(reader, volume, volume.root_sector, volume.root_size, visit)
}
