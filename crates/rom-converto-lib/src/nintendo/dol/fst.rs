//! GameCube / Wii File String Table (FST) walker.
//!
//! The FST is a flat array of 0x0C-byte records followed by a packed
//! string table. Record 0 is the root directory; its `size` field is
//! the total record count, which gives us the string-table offset.
//!
//! Reference: yagcd.chadderz.co.uk section "FST format".

use anyhow::{Result, anyhow};
use byteorder::{BE, ReadBytesExt};
use std::io::Cursor;

pub const FST_ENTRY_SIZE: usize = 0x0C;

#[derive(Debug, Clone)]
pub enum FstNode {
    File {
        path: String,
        offset: u64,
        size: u64,
    },
    Directory {
        path: String,
    },
}

/// Parse an FST buffer into a flat list of files keyed by full path.
pub fn list_files(fst: &[u8]) -> Result<Vec<FstNode>> {
    if fst.len() < FST_ENTRY_SIZE {
        return Err(anyhow!("FST too small"));
    }

    let root_type = fst[0];
    if root_type != 1 {
        return Err(anyhow!("FST root entry is not a directory"));
    }
    let total_entries = read_u32_be(fst, 0x08)? as usize;
    if total_entries == 0 || total_entries.saturating_mul(FST_ENTRY_SIZE) > fst.len() {
        return Err(anyhow!("FST total_entries overflows buffer"));
    }
    let string_table_start = total_entries * FST_ENTRY_SIZE;
    let string_table = &fst[string_table_start..];

    let mut out = Vec::new();
    let mut dir_stack: Vec<(usize, String)> = vec![(total_entries, String::new())];

    let mut idx = 1usize;
    while idx < total_entries {
        let off = idx * FST_ENTRY_SIZE;
        let kind = fst[off];
        let name_offset =
            (u32::from_be_bytes([0, fst[off + 1], fst[off + 2], fst[off + 3]])) as usize;
        let name = read_c_string(string_table, name_offset);

        while let Some(&(end, _)) = dir_stack.last() {
            if idx >= end && dir_stack.len() > 1 {
                dir_stack.pop();
            } else {
                break;
            }
        }
        let parent_path = &dir_stack.last().unwrap().1;

        let full_path = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };

        if kind == 0 {
            let offset = read_u32_be(fst, off + 4)? as u64;
            let size = read_u32_be(fst, off + 8)? as u64;
            out.push(FstNode::File {
                path: full_path,
                offset,
                size,
            });
            idx += 1;
        } else {
            let next_index = read_u32_be(fst, off + 8)? as usize;
            out.push(FstNode::Directory {
                path: full_path.clone(),
            });
            dir_stack.push((next_index, full_path));
            idx += 1;
        }
    }

    Ok(out)
}

pub fn find_file(fst: &[u8], path: &str) -> Result<Option<(u64, u64)>> {
    for node in list_files(fst)? {
        if let FstNode::File {
            path: p,
            offset,
            size,
        } = node
            && p == path
        {
            return Ok(Some((offset, size)));
        }
    }
    Ok(None)
}

fn read_u32_be(buf: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > buf.len() {
        return Err(anyhow!("FST read out of bounds at {offset}"));
    }
    let mut cur = Cursor::new(&buf[offset..]);
    Ok(cur.read_u32::<BE>()?)
}

fn read_c_string(table: &[u8], offset: usize) -> String {
    if offset >= table.len() {
        return String::new();
    }
    let end = table[offset..]
        .iter()
        .position(|b| *b == 0)
        .map(|n| offset + n)
        .unwrap_or(table.len());
    String::from_utf8_lossy(&table[offset..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_fst() -> Vec<u8> {
        // Layout: root (dir, total=4) -> opening.bnr (file) -> sub/ (dir, end=4) -> nested (file)
        // 4 entries total.
        let total_entries: u32 = 4;
        let string_table = b"opening.bnr\0sub\0nested\0";
        let mut entries: Vec<u8> = Vec::new();

        // Entry 0: root
        entries.push(1); // type=dir
        entries.extend_from_slice(&[0, 0, 0]); // name_offset
        entries.extend_from_slice(&[0, 0, 0, 0]); // parent_offset
        entries.extend_from_slice(&total_entries.to_be_bytes());

        // Entry 1: opening.bnr -> name_offset=0
        let entry1_data_offset: u32 = 0x40000;
        let entry1_size: u32 = 0x1840;
        entries.push(0);
        entries.extend_from_slice(&[0, 0, 0]); // name offset 0
        entries.extend_from_slice(&entry1_data_offset.to_be_bytes());
        entries.extend_from_slice(&entry1_size.to_be_bytes());

        // Entry 2: sub/ (dir, next_index=4, name_offset=12)
        entries.push(1);
        entries.extend_from_slice(&[0, 0, 12]);
        entries.extend_from_slice(&[0, 0, 0, 0]);
        entries.extend_from_slice(&4u32.to_be_bytes());

        // Entry 3: nested file (name_offset=16, offset=0x50000, size=0x100)
        entries.push(0);
        entries.extend_from_slice(&[0, 0, 16]);
        entries.extend_from_slice(&0x50000u32.to_be_bytes());
        entries.extend_from_slice(&0x100u32.to_be_bytes());

        let mut buf = Vec::new();
        buf.write_all(&entries).unwrap();
        buf.write_all(string_table).unwrap();
        buf
    }

    #[test]
    fn lists_files_with_paths() {
        let fst = build_fst();
        let nodes = list_files(&fst).unwrap();
        assert_eq!(nodes.len(), 3);
        match &nodes[0] {
            FstNode::File { path, offset, size } => {
                assert_eq!(path, "opening.bnr");
                assert_eq!(*offset, 0x40000);
                assert_eq!(*size, 0x1840);
            }
            _ => panic!("expected file"),
        }
        match &nodes[1] {
            FstNode::Directory { path } => assert_eq!(path, "sub"),
            _ => panic!("expected dir"),
        }
        match &nodes[2] {
            FstNode::File { path, offset, size } => {
                assert_eq!(path, "sub/nested");
                assert_eq!(*offset, 0x50000);
                assert_eq!(*size, 0x100);
            }
            _ => panic!("expected nested file"),
        }
    }

    #[test]
    fn finds_file_by_path() {
        let fst = build_fst();
        let found = find_file(&fst, "opening.bnr").unwrap();
        assert_eq!(found, Some((0x40000, 0x1840)));
        let nested = find_file(&fst, "sub/nested").unwrap();
        assert_eq!(nested, Some((0x50000, 0x100)));
    }

    #[test]
    fn missing_file_returns_none() {
        let fst = build_fst();
        assert!(find_file(&fst, "nope").unwrap().is_none());
    }
}
