//! Wii partition FST walker.
//!
//! Same shape as the GameCube FST in [`crate::nintendo::dol::fst`] but
//! file/dir offsets are stored as `u32 >> 2`, so they are shifted back to
//! byte offsets before returning them. Sizes are stored as raw bytes.

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

pub fn list_files(fst: &[u8]) -> Result<Vec<FstNode>> {
    if fst.len() < FST_ENTRY_SIZE {
        return Err(anyhow!("Wii FST too small"));
    }
    let root_type = fst[0];
    if root_type != 1 {
        return Err(anyhow!("Wii FST root is not a directory"));
    }
    let total_entries = read_u32_be(fst, 0x08)? as usize;
    if total_entries == 0 || total_entries.saturating_mul(FST_ENTRY_SIZE) > fst.len() {
        return Err(anyhow!("Wii FST total_entries overflows buffer"));
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
            // Wii files: data_offset is u32 shifted left by 2 to get
            // bytes; size is raw byte count.
            let file_offset = (read_u32_be(fst, off + 4)? as u64) << 2;
            let size = read_u32_be(fst, off + 8)? as u64;
            out.push(FstNode::File {
                path: full_path,
                offset: file_offset,
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
        return Err(anyhow!("Wii FST read out of bounds at {offset}"));
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
        let total_entries: u32 = 2;
        let string_table = b"opening.bnr\0";
        let mut entries: Vec<u8> = Vec::new();

        // Root
        entries.push(1);
        entries.extend_from_slice(&[0, 0, 0]);
        entries.extend_from_slice(&[0, 0, 0, 0]);
        entries.extend_from_slice(&total_entries.to_be_bytes());

        // opening.bnr: name_offset=0, raw_offset=0x10000>>2=0x4000, size=0x1840
        let raw_offset: u32 = 0x10000;
        let shifted: u32 = raw_offset >> 2;
        let size: u32 = 0x1840;
        entries.push(0);
        entries.extend_from_slice(&[0, 0, 0]);
        entries.extend_from_slice(&shifted.to_be_bytes());
        entries.extend_from_slice(&size.to_be_bytes());

        let mut buf = Vec::new();
        buf.write_all(&entries).unwrap();
        buf.write_all(string_table).unwrap();
        buf
    }

    #[test]
    fn lists_files_with_shifted_offsets() {
        let fst = build_fst();
        let nodes = list_files(&fst).unwrap();
        assert_eq!(nodes.len(), 1);
        match &nodes[0] {
            FstNode::File { path, offset, size } => {
                assert_eq!(path, "opening.bnr");
                assert_eq!(*offset, 0x10000);
                assert_eq!(*size, 0x1840);
            }
            _ => panic!("expected file"),
        }
    }

    #[test]
    fn finds_file_by_path() {
        let fst = build_fst();
        let found = find_file(&fst, "opening.bnr").unwrap();
        assert_eq!(found, Some((0x10000, 0x1840)));
    }
}
