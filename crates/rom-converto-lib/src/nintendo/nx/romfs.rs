//! Minimal read-only Switch RomFS walker.
//!
//! The Control NCA's section 0 contains a RomFS image whose root holds
//! `control.nacp` and a handful of `icon_<Language>.dat` files. We do
//! not need full directory traversal for that case, just "find a file
//! by name in the root" and a flat root listing. The IVFC hash layers
//! that wrap the RomFS image inside a real NCA are handled by the
//! caller; this module only sees the RomFS image itself.
//!
//! Layout reference: switchbrew.org/wiki/RomFS.

use crate::nintendo::nx::error::{NxError, NxResult};
use byteorder::{LE, ReadBytesExt};
use std::io::Cursor;

const ROMFS_HEADER_SIZE: u64 = 0x50;
const INVALID_OFFSET: u32 = 0xFFFF_FFFF;

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

#[derive(Debug, Clone)]
pub struct RomfsFile {
    pub name: String,
    /// Offset within the file-data block; add `header.file_data_offset`
    /// to translate into a position inside the RomFS image.
    pub data_offset: u64,
    pub data_size: u64,
    pub parent_dir_offset: u32,
}

pub struct RomfsReader<'a> {
    image: &'a [u8],
    pub header: RomfsHeader,
}

impl<'a> RomfsReader<'a> {
    pub fn new(image: &'a [u8]) -> NxResult<Self> {
        let header = RomfsHeader::parse(image)?;
        Ok(Self { image, header })
    }

    pub fn list_files(&self) -> NxResult<Vec<RomfsFile>> {
        let meta_off = self.header.file_meta_table_offset as usize;
        let meta_size = self.header.file_meta_table_size as usize;
        if meta_off + meta_size > self.image.len() {
            return Err(NxError::InvalidNcaHeader);
        }
        let table = &self.image[meta_off..meta_off + meta_size];

        let mut out = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut cursor: u32 = 0;
        while (cursor as usize) < meta_size && cursor != INVALID_OFFSET {
            if !visited.insert(cursor) {
                // Defensive: should never see the same offset twice
                // in a well-formed RomFS, but skip rather than loop.
                break;
            }
            let entry = parse_file_entry(table, cursor as usize)?;
            let next = entry.next_sibling;
            let name_padded = entry.name_length_padded();
            out.push(RomfsFile {
                name: entry.name,
                data_offset: entry.data_offset,
                data_size: entry.data_size,
                parent_dir_offset: entry.parent_dir_offset,
            });
            // Advance past the variable-length entry: 0x20 fixed bytes
            // plus the name padded to 4. Subsequent files are written
            // immediately after; we use this as the implicit cursor when
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

    pub fn find_root_file(&self, name: &str) -> NxResult<Option<RomfsFile>> {
        self.find_file(0, name)
    }

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
        let hash_off = self.header.file_hash_table_offset as usize;
        let hash_size = self.header.file_hash_table_size as usize;
        if hash_size < 4 || hash_off + hash_size > self.image.len() {
            return Ok(None);
        }
        let buckets = hash_size / 4;
        let bucket = (compute_file_hash(parent_dir_offset, name) as usize) % buckets;
        let bucket_off = hash_off + bucket * 4;
        let mut entry_offset = u32::from_le_bytes([
            self.image[bucket_off],
            self.image[bucket_off + 1],
            self.image[bucket_off + 2],
            self.image[bucket_off + 3],
        ]);

        let meta_off = self.header.file_meta_table_offset as usize;
        let meta_size = self.header.file_meta_table_size as usize;
        if meta_off + meta_size > self.image.len() {
            return Err(NxError::InvalidNcaHeader);
        }
        let meta_table = &self.image[meta_off..meta_off + meta_size];

        while entry_offset != INVALID_OFFSET {
            let entry = parse_file_entry(meta_table, entry_offset as usize)?;
            if entry.parent_dir_offset == parent_dir_offset && entry.name == name {
                return Ok(Some(RomfsFile {
                    name: entry.name,
                    data_offset: entry.data_offset,
                    data_size: entry.data_size,
                    parent_dir_offset: entry.parent_dir_offset,
                }));
            }
            entry_offset = entry.next_hash;
        }
        Ok(None)
    }

    pub fn read_file(&self, file: &RomfsFile) -> NxResult<Vec<u8>> {
        let start = self.header.file_data_offset + file.data_offset;
        let end = start + file.data_size;
        if end > self.image.len() as u64 {
            return Err(NxError::InvalidNcaHeader);
        }
        Ok(self.image[start as usize..end as usize].to_vec())
    }
}

#[derive(Debug)]
struct FileEntryRaw {
    parent_dir_offset: u32,
    next_sibling: u32,
    data_offset: u64,
    data_size: u64,
    next_hash: u32,
    name: String,
    name_length: u32,
}

impl FileEntryRaw {
    fn name_length_padded(&self) -> u32 {
        (self.name_length + 3) & !3
    }
}

fn parse_file_entry(table: &[u8], offset: usize) -> NxResult<FileEntryRaw> {
    if offset + 0x20 > table.len() {
        return Err(NxError::InvalidNcaHeader);
    }
    let mut cur = Cursor::new(&table[offset..]);
    let parent_dir_offset = cur.read_u32::<LE>()?;
    let next_sibling = cur.read_u32::<LE>()?;
    let data_offset = cur.read_u64::<LE>()?;
    let data_size = cur.read_u64::<LE>()?;
    let next_hash = cur.read_u32::<LE>()?;
    let name_length = cur.read_u32::<LE>()?;
    let name_start = offset + 0x20;
    let name_end = name_start + name_length as usize;
    if name_end > table.len() {
        return Err(NxError::InvalidNcaHeader);
    }
    let name = String::from_utf8_lossy(&table[name_start..name_end]).into_owned();
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
}
