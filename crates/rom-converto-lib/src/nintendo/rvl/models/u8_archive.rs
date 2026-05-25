//! Nintendo U8 archive reader (format reference:
//! `wiimms-szs-tools/project/src/lib-szs.h:93-124`).
//!
//! Non-obvious invariant: a directory node's `size` is the
//! **exclusive end index** of its subtree, NOT a child count.
//! Children of dir `i` live in `[i+1 .. dir.size)`, but a nested
//! directory child owns `[child+1 .. child.size)` and walkers must
//! skip past those nested ranges to find the next direct sibling.
//! Treating `size` as a count produces silent lookup failures on
//! every nested path.

use anyhow::{Result, anyhow};
use byteorder::{BE, ByteOrder};

pub const U8_MAGIC: u32 = 0x55AA_382D;
pub const U8_NODE_SIZE: usize = 12;
pub const U8_HEADER_SIZE: usize = 0x20;

#[derive(Debug, Clone)]
pub struct U8Archive<'a> {
    data: &'a [u8],
    nodes: Vec<U8Node>,
    string_table_offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct U8Node {
    is_dir: bool,
    name_offset: u32,
    data_offset: u32,
    size: u32,
}

impl<'a> U8Archive<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < U8_HEADER_SIZE {
            return Err(anyhow!("U8 archive shorter than header"));
        }
        let magic = BE::read_u32(&data[0..4]);
        if magic != U8_MAGIC {
            return Err(anyhow!("bad U8 magic 0x{:08X}", magic));
        }
        let node_offset = BE::read_u32(&data[4..8]) as usize;
        let _fst_size = BE::read_u32(&data[8..12]) as usize;
        let _data_offset = BE::read_u32(&data[12..16]) as usize;

        if node_offset
            .checked_add(U8_NODE_SIZE)
            .map(|e| e > data.len())
            .unwrap_or(true)
        {
            return Err(anyhow!("U8 root node past end of buffer"));
        }

        // Root node's `size` field is the total node count (root included).
        let n_nodes = BE::read_u32(&data[node_offset + 8..node_offset + 12]) as usize;
        if n_nodes == 0 {
            return Err(anyhow!("U8 root node reports zero entries"));
        }
        let nodes_end = node_offset
            .checked_add(n_nodes * U8_NODE_SIZE)
            .ok_or_else(|| anyhow!("U8 node table overflow"))?;
        if nodes_end > data.len() {
            return Err(anyhow!(
                "U8 node table {}..{} past end of buffer ({})",
                node_offset,
                nodes_end,
                data.len()
            ));
        }

        let mut nodes = Vec::with_capacity(n_nodes);
        for i in 0..n_nodes {
            let off = node_offset + i * U8_NODE_SIZE;
            let header = BE::read_u32(&data[off..off + 4]);
            let is_dir = (header >> 24) & 0xFF == 1;
            let name_offset = header & 0x00FF_FFFF;
            let data_offset = BE::read_u32(&data[off + 4..off + 8]);
            let size = BE::read_u32(&data[off + 8..off + 12]);
            nodes.push(U8Node {
                is_dir,
                name_offset,
                data_offset,
                size,
            });
        }

        let string_table_offset = nodes_end;
        Ok(Self {
            data,
            nodes,
            string_table_offset,
        })
    }

    pub fn find(&self, path: &str) -> Option<&'a [u8]> {
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return None;
        }
        let total_nodes = self.nodes.first()?.size as usize;
        self.find_in_dir(0, total_nodes, &components)
    }

    pub fn list_paths(&self) -> Vec<(String, &'a [u8])> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let total_nodes = self.nodes[0].size as usize;
        let mut stack: Vec<String> = Vec::new();
        let mut end_stack: Vec<usize> = vec![total_nodes];
        let mut idx = 1usize;
        while idx < total_nodes {
            while let Some(end) = end_stack.last().copied() {
                if idx >= end && end_stack.len() > 1 {
                    end_stack.pop();
                    stack.pop();
                } else {
                    break;
                }
            }
            let node = self.nodes[idx];
            let name = match self.read_name(node.name_offset) {
                Some(n) => n.to_string(),
                None => {
                    idx += 1;
                    continue;
                }
            };
            if node.is_dir {
                stack.push(name);
                end_stack.push(node.size as usize);
                idx += 1;
            } else {
                let path = if stack.is_empty() {
                    name
                } else {
                    format!("{}/{}", stack.join("/"), name)
                };
                let start = node.data_offset as usize;
                let end = start.saturating_add(node.size as usize);
                if end <= self.data.len() {
                    out.push((path, &self.data[start..end]));
                }
                idx += 1;
            }
        }
        out
    }

    /// Tolerant fallback for archives that nest files under non-canonical
    /// directories.
    pub fn find_path_ending_with(&self, suffix: &str) -> Option<&'a [u8]> {
        let suffix_lower = suffix.to_ascii_lowercase();
        for (path, bytes) in self.list_paths() {
            if path.to_ascii_lowercase().ends_with(&suffix_lower) {
                return Some(bytes);
            }
        }
        None
    }

    fn find_in_dir(
        &self,
        dir_idx: usize,
        dir_end_excl: usize,
        components: &[&str],
    ) -> Option<&'a [u8]> {
        let (head, rest) = components.split_first()?;
        let mut idx = dir_idx + 1;
        while idx < dir_end_excl {
            let node = self.nodes[idx];
            let name = self.read_name(node.name_offset).unwrap_or("");
            let next_subtree_end = if node.is_dir {
                node.size as usize
            } else {
                idx + 1
            };
            if name == *head {
                if rest.is_empty() {
                    if node.is_dir {
                        return None;
                    }
                    let start = node.data_offset as usize;
                    let end = start.checked_add(node.size as usize)?;
                    if end > self.data.len() {
                        return None;
                    }
                    return Some(&self.data[start..end]);
                } else {
                    if !node.is_dir {
                        return None;
                    }
                    return self.find_in_dir(idx, node.size as usize, rest);
                }
            }
            idx = next_subtree_end.max(idx + 1);
        }
        None
    }

    fn read_name(&self, name_offset: u32) -> Option<&str> {
        let start = self.string_table_offset.checked_add(name_offset as usize)?;
        if start >= self.data.len() {
            return None;
        }
        let end = self.data[start..]
            .iter()
            .position(|b| *b == 0)
            .map(|p| start + p)
            .unwrap_or(self.data.len());
        std::str::from_utf8(&self.data[start..end]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

    fn build_archive(entries: &[Entry]) -> Vec<u8> {
        #[derive(Debug)]
        struct TreeNode {
            is_dir: bool,
            name: String,
            data: Vec<u8>,
            children: Vec<TreeNode>,
        }

        let mut root = TreeNode {
            is_dir: true,
            name: String::new(),
            data: Vec::new(),
            children: Vec::new(),
        };

        fn insert(root: &mut TreeNode, parts: &[&str], data: &[u8]) {
            if parts.len() == 1 {
                root.children.push(TreeNode {
                    is_dir: false,
                    name: parts[0].to_string(),
                    data: data.to_vec(),
                    children: Vec::new(),
                });
                return;
            }
            let head = parts[0];
            let pos = root.children.iter().position(|c| c.is_dir && c.name == head);
            let idx = match pos {
                Some(i) => i,
                None => {
                    root.children.push(TreeNode {
                        is_dir: true,
                        name: head.to_string(),
                        data: Vec::new(),
                        children: Vec::new(),
                    });
                    root.children.len() - 1
                }
            };
            insert(&mut root.children[idx], &parts[1..], data);
        }

        for entry in entries {
            let parts: Vec<&str> = entry.path.split('/').filter(|s| !s.is_empty()).collect();
            insert(&mut root, &parts, entry.data);
        }

        let mut nodes: Vec<NodeBuild> = Vec::new();
        let mut string_table: Vec<u8> = vec![0];
        let mut file_payloads: Vec<Vec<u8>> = Vec::new();

        fn intern_test(table: &mut Vec<u8>, name: &str) -> u32 {
            if name.is_empty() {
                return 0;
            }
            let off = table.len() as u32;
            table.extend_from_slice(name.as_bytes());
            table.push(0);
            off
        }

        fn emit(
            node: &TreeNode,
            nodes: &mut Vec<NodeBuild>,
            string_table: &mut Vec<u8>,
            file_payloads: &mut Vec<Vec<u8>>,
        ) {
            let name_off = intern_test(string_table, &node.name);
            let my_idx = nodes.len();
            nodes.push(NodeBuild {
                is_dir: node.is_dir,
                name_off,
                data_off: 0,
                size: 0,
            });
            if node.is_dir {
                for child in &node.children {
                    emit(child, nodes, string_table, file_payloads);
                }
                let end_excl = nodes.len() as u32;
                nodes[my_idx].size = end_excl;
            } else {
                nodes[my_idx].size = node.data.len() as u32;
                file_payloads.push(node.data.clone());
            }
        }

        emit(&root, &mut nodes, &mut string_table, &mut file_payloads);
        let n = nodes.len();

        let node_table_off = U8_HEADER_SIZE;
        let node_table_size = n * U8_NODE_SIZE;
        let string_table_off = node_table_off + node_table_size;
        let mut data_offset = string_table_off + string_table.len();
        data_offset = (data_offset + 0x1F) & !0x1F;

        let mut total_size = data_offset;
        let mut file_offsets: Vec<u32> = Vec::with_capacity(file_payloads.len());
        for payload in &file_payloads {
            file_offsets.push(total_size as u32);
            total_size += payload.len();
        }
        let mut out = vec![0u8; total_size];

        (&mut out[0..4]).write_u32::<BE>(U8_MAGIC).unwrap();
        (&mut out[4..8]).write_u32::<BE>(node_table_off as u32).unwrap();
        (&mut out[8..12])
            .write_u32::<BE>((node_table_size + string_table.len()) as u32)
            .unwrap();
        (&mut out[12..16])
            .write_u32::<BE>(data_offset as u32)
            .unwrap();

        let mut file_cursor = 0;
        for (i, node) in nodes.iter().enumerate() {
            let off = node_table_off + i * U8_NODE_SIZE;
            let header = ((node.is_dir as u32) << 24) | (node.name_off & 0x00FF_FFFF);
            (&mut out[off..off + 4]).write_u32::<BE>(header).unwrap();
            let data_off = if node.is_dir {
                0
            } else {
                let v = file_offsets[file_cursor];
                file_cursor += 1;
                v
            };
            (&mut out[off + 4..off + 8])
                .write_u32::<BE>(data_off)
                .unwrap();
            (&mut out[off + 8..off + 12])
                .write_u32::<BE>(node.size)
                .unwrap();
        }

        out[string_table_off..string_table_off + string_table.len()]
            .copy_from_slice(&string_table);

        let mut cursor = data_offset;
        for payload in &file_payloads {
            out[cursor..cursor + payload.len()].copy_from_slice(payload);
            cursor += payload.len();
        }
        out
    }

    #[derive(Debug, Clone)]
    struct NodeBuild {
        is_dir: bool,
        name_off: u32,
        data_off: u32,
        size: u32,
    }

    struct Entry<'a> {
        path: &'a str,
        data: &'a [u8],
    }

    fn intern(table: &mut Vec<u8>, name: &str) -> u32 {
        let off = table.len() as u32;
        table.extend_from_slice(name.as_bytes());
        table.push(0);
        off
    }

    fn read_name_in(table: &[u8], off: u32) -> String {
        let start = off as usize;
        let end = table[start..]
            .iter()
            .position(|b| *b == 0)
            .map(|p| start + p)
            .unwrap_or(table.len());
        String::from_utf8_lossy(&table[start..end]).into_owned()
    }

    #[test]
    fn finds_root_level_file() {
        let archive = build_archive(&[Entry {
            path: "icon.tpl",
            data: b"PIXELS",
        }]);
        let parsed = U8Archive::parse(&archive).unwrap();
        assert_eq!(parsed.find("icon.tpl"), Some(&b"PIXELS"[..]));
    }

    #[test]
    fn finds_nested_file() {
        let archive = build_archive(&[Entry {
            path: "meta/banner.bin",
            data: b"BANNER",
        }]);
        let parsed = U8Archive::parse(&archive).unwrap();
        assert_eq!(parsed.find("meta/banner.bin"), Some(&b"BANNER"[..]));
    }

    #[test]
    fn finds_deeply_nested_file() {
        let archive = build_archive(&[Entry {
            path: "arc/timg/banner.tpl",
            data: b"TPL_DATA",
        }]);
        let parsed = U8Archive::parse(&archive).unwrap();
        assert_eq!(
            parsed.find("arc/timg/banner.tpl"),
            Some(&b"TPL_DATA"[..])
        );
    }

    #[test]
    fn returns_none_for_missing_path() {
        let archive = build_archive(&[Entry {
            path: "icon.tpl",
            data: b"PIXELS",
        }]);
        let parsed = U8Archive::parse(&archive).unwrap();
        assert!(parsed.find("missing.bin").is_none());
        assert!(parsed.find("nested/missing.bin").is_none());
    }

    #[test]
    fn list_paths_returns_every_file() {
        let archive = build_archive(&[
            Entry {
                path: "meta/banner.bin",
                data: b"BANNER",
            },
            Entry {
                path: "meta/icon.bin",
                data: b"ICON",
            },
            Entry {
                path: "arc/timg/banner.tpl",
                data: b"TPL",
            },
        ]);
        let parsed = U8Archive::parse(&archive).unwrap();
        let paths: Vec<String> = parsed
            .list_paths()
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(paths.contains(&"meta/banner.bin".to_string()), "got {:?}", paths);
        assert!(paths.contains(&"meta/icon.bin".to_string()), "got {:?}", paths);
        assert!(
            paths.contains(&"arc/timg/banner.tpl".to_string()),
            "got {:?}",
            paths
        );
    }

    #[test]
    fn find_path_ending_with_works_for_suffix() {
        let archive = build_archive(&[Entry {
            path: "arc/timg/banner.tpl",
            data: b"TPL_DATA",
        }]);
        let parsed = U8Archive::parse(&archive).unwrap();
        assert_eq!(
            parsed.find_path_ending_with(".tpl"),
            Some(&b"TPL_DATA"[..])
        );
        assert_eq!(
            parsed.find_path_ending_with("/banner.tpl"),
            Some(&b"TPL_DATA"[..])
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let mut archive = build_archive(&[Entry {
            path: "a",
            data: b"b",
        }]);
        archive[0] = 0;
        assert!(U8Archive::parse(&archive).is_err());
    }
}
