//! In-memory directory tree with ZArchive-compatible sorting.
//!
//! The tree holds the virtual filesystem the writer is building. Each
//! node is either a directory (with `children`) or a file (with
//! `file_offset` and `file_size`). The writer hands us `add_file`
//! calls, mutates file sizes through [`PathTree::get_mut`] as data
//! streams in, then calls [`PathTree::sort`] and
//! [`PathTree::bfs_entries`] at finalise time to produce the flat
//! BFS layout the file tree section expects.
//!
//! The sort order matches upstream [`compare_node_name`] exactly. Our
//! BFS walk mirrors the upstream writer (`zarchivewriter.cpp`): two
//! passes over a queue, first to assign `node_start_index` values
//! and then to serialise.

use std::collections::VecDeque;

use crate::nintendo::wup::error::{WupError, WupResult};

/// One entry in the path tree. Directory nodes keep an ordered list
/// of [`PathNode`] children; file nodes ignore `children` and carry
/// the uncompressed `file_offset` / `file_size` pair instead.
#[derive(Debug)]
pub struct PathNode {
    pub name: String,
    pub is_file: bool,
    /// Global uncompressed byte offset of this file's first byte.
    /// Zero for directories and for files that haven't been placed
    /// yet.
    pub file_offset: u64,
    /// Total uncompressed file size in bytes. Writer updates this
    /// incrementally while streaming data into the archive.
    pub file_size: u64,
    /// Child nodes (empty for files).
    pub children: Vec<PathNode>,
}

impl PathNode {
    fn new_directory(name: String) -> Self {
        Self {
            name,
            is_file: false,
            file_offset: 0,
            file_size: 0,
            children: Vec::new(),
        }
    }

    fn new_file(name: String, file_offset: u64) -> Self {
        Self {
            name,
            is_file: true,
            file_offset,
            file_size: 0,
            children: Vec::new(),
        }
    }
}

/// Virtual filesystem being built up for the writer. The root is an
/// empty-named directory and always lives at tree index 0 in the
/// serialised file tree section.
#[derive(Debug)]
pub struct PathTree {
    pub root: PathNode,
}

impl Default for PathTree {
    fn default() -> Self {
        Self::new()
    }
}

impl PathTree {
    pub fn new() -> Self {
        Self {
            root: PathNode::new_directory(String::new()),
        }
    }

    /// Create a directory at `path`, creating any missing
    /// intermediate directories along the way. Idempotent on
    /// directories that already exist; returns [`WupError::PathConflict`]
    /// if any existing node along the path is a file.
    pub fn make_dir(&mut self, path: &str) -> WupResult<()> {
        let components = split_path(path);
        if components.is_empty() {
            // Creating "the root" is a no-op.
            return Ok(());
        }
        make_dir_recursive(&mut self.root, &components, path)
    }

    /// Add a new file at `path` with the given global uncompressed
    /// `file_offset` and return a mutable reference to it. Parent
    /// directories are created as needed.
    ///
    /// Fails with [`WupError::InvalidPath`] on an empty path,
    /// [`WupError::DuplicateFile`] if a file with the same name
    /// already exists in the parent directory, or [`WupError::PathConflict`]
    /// if any intermediate component is itself a file.
    pub fn add_file(&mut self, path: &str, file_offset: u64) -> WupResult<&mut PathNode> {
        let components = split_path(path);
        if components.is_empty() {
            return Err(WupError::InvalidPath(path.to_string()));
        }
        add_file_recursive(&mut self.root, &components, path, file_offset)
    }

    /// Look up a node by path and return a mutable reference, or
    /// `None` if no such node exists.
    pub fn get_mut(&mut self, path: &str) -> Option<&mut PathNode> {
        let components = split_path(path);
        if components.is_empty() {
            return Some(&mut self.root);
        }
        get_mut_recursive(&mut self.root, &components)
    }

    /// Sort every directory's children with the ZArchive compare
    /// function. Must be called before [`Self::bfs_entries`] or the
    /// flat file tree will not match upstream's output byte order.
    pub fn sort(&mut self) {
        sort_recursive(&mut self.root);
    }

    /// Walk the tree in breadth-first order starting at the root and
    /// return one entry per node. Directory entries carry the
    /// assigned `node_start_index`; file entries carry [`u32::MAX`]
    /// as a sentinel.
    ///
    /// The root is always at index 0. For each directory, its
    /// children occupy the contiguous range
    /// `node_start_index..node_start_index + children.len()` in the
    /// returned vec. This mirrors the BFS index assignment in
    /// `zarchivewriter.cpp`'s `Finalize()`.
    pub fn bfs_entries(&self) -> Vec<BfsEntry<'_>> {
        let mut entries: Vec<BfsEntry<'_>> = Vec::new();
        let mut queue: VecDeque<&PathNode> = VecDeque::new();
        queue.push_back(&self.root);
        // `current_index` is the next free slot in the flat vec.
        // Matches upstream: root is at index 0, so the first dir's
        // children start at index 1.
        let mut current_index: u32 = 1;
        while let Some(node) = queue.pop_front() {
            if node.is_file {
                entries.push(BfsEntry {
                    node,
                    node_start_index: u32::MAX,
                });
            } else {
                let node_start_index = current_index;
                current_index += node.children.len() as u32;
                entries.push(BfsEntry {
                    node,
                    node_start_index,
                });
                for child in &node.children {
                    queue.push_back(child);
                }
            }
        }
        entries
    }
}

/// One BFS-ordered node plus its assigned `node_start_index`.
/// Directory nodes use `node_start_index` as the base index of their
/// children in the flat file tree; file nodes always carry
/// [`u32::MAX`] here and use the writer's own file offset/size instead.
#[derive(Debug, Clone, Copy)]
pub struct BfsEntry<'tree> {
    pub node: &'tree PathNode,
    pub node_start_index: u32,
}

fn split_path(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|s| !s.is_empty()).collect()
}

fn make_dir_recursive(node: &mut PathNode, components: &[&str], full_path: &str) -> WupResult<()> {
    if components.is_empty() {
        return Ok(());
    }
    if node.is_file {
        return Err(WupError::PathConflict(full_path.to_string()));
    }
    let (first, rest) = components.split_first().unwrap();
    let idx = match node.children.iter().position(|c| c.name == *first) {
        Some(i) => {
            if node.children[i].is_file {
                return Err(WupError::PathConflict(full_path.to_string()));
            }
            i
        }
        None => {
            node.children
                .push(PathNode::new_directory((*first).to_string()));
            node.children.len() - 1
        }
    };
    make_dir_recursive(&mut node.children[idx], rest, full_path)
}

fn add_file_recursive<'a>(
    node: &'a mut PathNode,
    components: &[&str],
    full_path: &str,
    file_offset: u64,
) -> WupResult<&'a mut PathNode> {
    if node.is_file {
        return Err(WupError::PathConflict(full_path.to_string()));
    }
    let (first, rest) = components.split_first().unwrap();
    if rest.is_empty() {
        // `first` is the filename.
        if node.children.iter().any(|c| c.name == *first) {
            return Err(WupError::DuplicateFile(full_path.to_string()));
        }
        node.children
            .push(PathNode::new_file((*first).to_string(), file_offset));
        let last = node.children.len() - 1;
        return Ok(&mut node.children[last]);
    }
    // `first` is an intermediate directory.
    let idx = match node.children.iter().position(|c| c.name == *first) {
        Some(i) => {
            if node.children[i].is_file {
                return Err(WupError::PathConflict(full_path.to_string()));
            }
            i
        }
        None => {
            node.children
                .push(PathNode::new_directory((*first).to_string()));
            node.children.len() - 1
        }
    };
    add_file_recursive(&mut node.children[idx], rest, full_path, file_offset)
}

fn get_mut_recursive<'a>(node: &'a mut PathNode, components: &[&str]) -> Option<&'a mut PathNode> {
    if components.is_empty() {
        return Some(node);
    }
    let (first, rest) = components.split_first().unwrap();
    let idx = node.children.iter().position(|c| c.name == *first)?;
    get_mut_recursive(&mut node.children[idx], rest)
}

fn sort_recursive(node: &mut PathNode) {
    if node.is_file {
        return;
    }
    node.children.sort_by(|a, b| sort_cmp(&a.name, &b.name));
    for child in &mut node.children {
        sort_recursive(child);
    }
}

/// `std::cmp::Ordering` wrapper around [`compare_node_name`] that
/// interprets a positive result as `a < b` so a normal ascending
/// `sort_by` reproduces upstream's child order.
fn sort_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let c = compare_node_name(a.as_bytes(), b.as_bytes());
    // Upstream uses `CompareNodeName(a, b) > 0` as its less-than
    // predicate, so positive means "a should sort before b".
    match c.cmp(&0) {
        std::cmp::Ordering::Greater => std::cmp::Ordering::Less,
        std::cmp::Ordering::Less => std::cmp::Ordering::Greater,
        std::cmp::Ordering::Equal => std::cmp::Ordering::Equal,
    }
}

/// Port of the ZArchive `CompareNodeName` routine.
///
/// Case-folds ASCII A-Z to a-z, compares byte-by-byte, and returns
/// `c2 - c1` on the first mismatch so the sign convention is the
/// inverse of `strcmp`. When one string is a strict prefix of the
/// other, the shorter string sorts first (the function returns `+1`
/// when `n1` is shorter, which the writer interprets as "n1 less-than
/// n2" via its `> 0` predicate).
pub fn compare_node_name(n1: &[u8], n2: &[u8]) -> i32 {
    let min_len = n1.len().min(n2.len());
    for i in 0..min_len {
        let mut c1 = n1[i];
        let mut c2 = n2[i];
        if c1.is_ascii_uppercase() {
            c1 += b'a' - b'A';
        }
        if c2.is_ascii_uppercase() {
            c2 += b'a' - b'A';
        }
        if c1 != c2 {
            return c2 as i32 - c1 as i32;
        }
    }
    if n1.len() < n2.len() {
        return 1;
    }
    if n1.len() > n2.len() {
        return -1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- compare_node_name ----

    #[test]
    fn compare_node_name_equal() {
        assert_eq!(compare_node_name(b"meta", b"meta"), 0);
    }

    #[test]
    fn compare_node_name_is_case_insensitive_ascii() {
        assert_eq!(compare_node_name(b"Meta", b"meta"), 0);
        assert_eq!(compare_node_name(b"META", b"meta"), 0);
        assert_eq!(compare_node_name(b"mEtA", b"MeTa"), 0);
    }

    #[test]
    fn compare_node_name_sign_is_inverse_of_strcmp() {
        // 'a' (97) vs 'b' (98): upstream returns c2 - c1 = 1 > 0.
        // Strict strcmp would return -1 (a < b). Our result is
        // positive, which the writer's `> 0` predicate treats as
        // "a < b" -> same lexicographic order but opposite sign.
        assert!(compare_node_name(b"a", b"b") > 0);
        assert!(compare_node_name(b"b", b"a") < 0);
    }

    #[test]
    fn compare_node_name_shorter_prefix_sorts_first() {
        // compare("ab", "abc") returns +1 (n1 shorter), which the
        // writer treats as "ab less-than abc".
        assert!(compare_node_name(b"ab", b"abc") > 0);
        assert!(compare_node_name(b"abc", b"ab") < 0);
    }

    #[test]
    fn compare_node_name_mixed_case_divergence() {
        // "Abc" vs "abd" after case fold: "abc" vs "abd". c1='c',
        // c2='d', return 'd'-'c' = 1 > 0. Upstream uses this as
        // "Abc < abd".
        assert!(compare_node_name(b"Abc", b"abd") > 0);
        assert!(compare_node_name(b"abd", b"Abc") < 0);
    }

    #[test]
    fn compare_node_name_ascii_wii_u_fixtures() {
        // Reproduce the order Cemu produces for a typical loadiine
        // title folder, where `code` < `content` < `meta` after
        // lowercasing.
        assert!(compare_node_name(b"code", b"content") > 0);
        assert!(compare_node_name(b"content", b"meta") > 0);
        assert!(compare_node_name(b"code", b"meta") > 0);
    }

    // ---- sort_cmp ----

    #[test]
    fn sort_cmp_ascending_order() {
        let mut names = vec!["meta", "code", "content"];
        names.sort_by(|a, b| sort_cmp(a, b));
        assert_eq!(names, vec!["code", "content", "meta"]);
    }

    #[test]
    fn sort_cmp_case_insensitive_with_stable_file_order() {
        let mut names = vec!["Bar.xml", "a.xml", "abc.xml"];
        names.sort_by(|a, b| sort_cmp(a, b));
        // After case fold: "a.xml" < "abc.xml" < "bar.xml"
        assert_eq!(names, vec!["a.xml", "abc.xml", "Bar.xml"]);
    }

    // ---- PathTree make_dir ----

    #[test]
    fn make_dir_creates_single_directory() {
        let mut tree = PathTree::new();
        tree.make_dir("meta").unwrap();
        assert_eq!(tree.root.children.len(), 1);
        assert_eq!(tree.root.children[0].name, "meta");
        assert!(!tree.root.children[0].is_file);
    }

    #[test]
    fn make_dir_is_idempotent() {
        let mut tree = PathTree::new();
        tree.make_dir("meta").unwrap();
        tree.make_dir("meta").unwrap();
        assert_eq!(tree.root.children.len(), 1);
    }

    #[test]
    fn make_dir_creates_intermediates() {
        let mut tree = PathTree::new();
        tree.make_dir("a/b/c").unwrap();
        let a = &tree.root.children[0];
        assert_eq!(a.name, "a");
        let b = &a.children[0];
        assert_eq!(b.name, "b");
        let c = &b.children[0];
        assert_eq!(c.name, "c");
    }

    #[test]
    fn make_dir_empty_path_is_noop() {
        let mut tree = PathTree::new();
        tree.make_dir("").unwrap();
        assert!(tree.root.children.is_empty());
    }

    #[test]
    fn make_dir_accepts_backslash_separator() {
        let mut tree = PathTree::new();
        tree.make_dir("a\\b").unwrap();
        assert_eq!(tree.root.children[0].name, "a");
        assert_eq!(tree.root.children[0].children[0].name, "b");
    }

    #[test]
    fn make_dir_conflict_with_existing_file() {
        let mut tree = PathTree::new();
        tree.add_file("meta", 0).unwrap();
        let err = tree.make_dir("meta").unwrap_err();
        assert!(matches!(err, WupError::PathConflict(_)));
    }

    // ---- PathTree add_file ----

    #[test]
    fn add_file_at_root() {
        let mut tree = PathTree::new();
        let node = tree.add_file("foo.xml", 0x1000).unwrap();
        assert_eq!(node.name, "foo.xml");
        assert!(node.is_file);
        assert_eq!(node.file_offset, 0x1000);
        assert_eq!(node.file_size, 0);
    }

    #[test]
    fn add_file_creates_parent_directories_lazily() {
        let mut tree = PathTree::new();
        tree.add_file("meta/meta.xml", 0x0).unwrap();
        let meta = tree
            .root
            .children
            .iter()
            .find(|c| c.name == "meta")
            .unwrap();
        assert!(!meta.is_file);
        assert_eq!(meta.children[0].name, "meta.xml");
    }

    #[test]
    fn add_file_returns_mutable_reference() {
        let mut tree = PathTree::new();
        let node = tree.add_file("code/app.xml", 0x2000).unwrap();
        node.file_size = 0x3000;
        let fetched = tree.get_mut("code/app.xml").unwrap();
        assert_eq!(fetched.file_offset, 0x2000);
        assert_eq!(fetched.file_size, 0x3000);
    }

    #[test]
    fn add_file_rejects_empty_path() {
        let mut tree = PathTree::new();
        let err = tree.add_file("", 0).unwrap_err();
        assert!(matches!(err, WupError::InvalidPath(_)));
    }

    #[test]
    fn add_file_rejects_duplicate() {
        let mut tree = PathTree::new();
        tree.add_file("meta/meta.xml", 0).unwrap();
        let err = tree.add_file("meta/meta.xml", 0x1000).unwrap_err();
        assert!(matches!(err, WupError::DuplicateFile(_)));
    }

    #[test]
    fn add_file_rejects_nesting_under_file() {
        let mut tree = PathTree::new();
        tree.add_file("meta", 0).unwrap();
        let err = tree.add_file("meta/inside", 0x1000).unwrap_err();
        assert!(matches!(err, WupError::PathConflict(_)));
    }

    // ---- PathTree get_mut ----

    #[test]
    fn get_mut_finds_nested_file() {
        let mut tree = PathTree::new();
        tree.add_file("a/b/c", 0x100).unwrap();
        let found = tree.get_mut("a/b/c").unwrap();
        assert_eq!(found.file_offset, 0x100);
    }

    #[test]
    fn get_mut_missing_returns_none() {
        let mut tree = PathTree::new();
        assert!(tree.get_mut("nope").is_none());
    }

    #[test]
    fn get_mut_empty_returns_root() {
        let mut tree = PathTree::new();
        let root = tree.get_mut("").unwrap();
        assert!(!root.is_file);
    }

    // ---- PathTree sort ----

    #[test]
    fn sort_sorts_root_children() {
        let mut tree = PathTree::new();
        tree.make_dir("meta").unwrap();
        tree.make_dir("code").unwrap();
        tree.make_dir("content").unwrap();
        tree.sort();
        let names: Vec<_> = tree.root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["code", "content", "meta"]);
    }

    #[test]
    fn sort_recurses_into_every_directory() {
        let mut tree = PathTree::new();
        tree.add_file("meta/Bar.xml", 0).unwrap();
        tree.add_file("meta/abc.xml", 0).unwrap();
        tree.add_file("meta/a.xml", 0).unwrap();
        tree.sort();
        let meta = tree
            .root
            .children
            .iter()
            .find(|c| c.name == "meta")
            .unwrap();
        let names: Vec<_> = meta.children.iter().map(|c| c.name.as_str()).collect();
        // compare_node_name says "a.xml" < "abc.xml" < "bar.xml"
        // (shorter prefix first, case insensitive).
        assert_eq!(names, vec!["a.xml", "abc.xml", "Bar.xml"]);
    }

    #[test]
    fn sort_is_deterministic_across_creation_orders() {
        let order_a = {
            let mut tree = PathTree::new();
            tree.make_dir("meta").unwrap();
            tree.make_dir("content").unwrap();
            tree.make_dir("code").unwrap();
            tree.sort();
            tree.root
                .children
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
        };
        let order_b = {
            let mut tree = PathTree::new();
            tree.make_dir("code").unwrap();
            tree.make_dir("meta").unwrap();
            tree.make_dir("content").unwrap();
            tree.sort();
            tree.root
                .children
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(order_a, order_b);
    }

    // ---- PathTree bfs_entries ----

    #[test]
    fn bfs_root_only() {
        let tree = PathTree::new();
        let entries = tree.bfs_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].node.name, "");
        assert_eq!(entries[0].node_start_index, 1);
        assert!(!entries[0].node.is_file);
    }

    #[test]
    fn bfs_assigns_consecutive_indices_to_each_directory_children_block() {
        let mut tree = PathTree::new();
        tree.make_dir("a").unwrap();
        tree.make_dir("b").unwrap();
        tree.add_file("a/f1", 0).unwrap();
        tree.add_file("a/f2", 0).unwrap();
        tree.add_file("b/g1", 0).unwrap();
        tree.sort();
        let entries = tree.bfs_entries();

        // Expected BFS order:
        //   0: root          (children a, b at [1, 3))
        //   1: a             (children f1, f2 at [3, 5))
        //   2: b             (children g1     at [5, 6))
        //   3: f1            (file)
        //   4: f2            (file)
        //   5: g1            (file)
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].node.name, "");
        assert_eq!(entries[0].node_start_index, 1);
        assert_eq!(entries[1].node.name, "a");
        assert_eq!(entries[1].node_start_index, 3);
        assert_eq!(entries[2].node.name, "b");
        assert_eq!(entries[2].node_start_index, 5);
        assert_eq!(entries[3].node.name, "f1");
        assert_eq!(entries[3].node_start_index, u32::MAX);
        assert_eq!(entries[4].node.name, "f2");
        assert_eq!(entries[5].node.name, "g1");
    }

    #[test]
    fn bfs_deeper_tree() {
        // Exercise a three-level nesting and make sure grandchildren
        // end up after all second-level children have been laid out.
        let mut tree = PathTree::new();
        tree.add_file("a/b/c/leaf1", 0).unwrap();
        tree.add_file("a/b/c/leaf2", 0).unwrap();
        tree.add_file("a/sibling", 0).unwrap();
        tree.sort();
        let entries = tree.bfs_entries();

        // Expected BFS order (children sorted ascending at each level):
        //   0: root
        //   1: a
        //   2: b         (children of a, after sibling; but 'b' < 'sibling'? 'b'=0x62, 's'=0x73 -> b first)
        //   3: sibling
        //   4: c         (child of b)
        //   5: leaf1     (child of c)
        //   6: leaf2
        assert_eq!(entries.len(), 7);
        assert_eq!(entries[0].node.name, "");
        assert_eq!(entries[1].node.name, "a");
        assert_eq!(entries[2].node.name, "b");
        assert_eq!(entries[3].node.name, "sibling");
        assert_eq!(entries[4].node.name, "c");
        assert_eq!(entries[5].node.name, "leaf1");
        assert_eq!(entries[6].node.name, "leaf2");

        // a.node_start_index should land on index 2 (b is first child
        // of a, placed at flat-index 2).
        assert_eq!(entries[1].node_start_index, 2);
        // b.node_start_index should land on index 4 (c).
        assert_eq!(entries[2].node_start_index, 4);
        // c.node_start_index should land on index 5 (leaf1).
        assert_eq!(entries[4].node_start_index, 5);
    }

    #[test]
    fn bfs_order_matches_sort_order() {
        // If sort is skipped, BFS still produces a valid layout but
        // with children in insertion order. If sort is called, BFS
        // order must reflect the compare_node_name sort. Verify the
        // sorted case (which is what the writer will always use).
        let mut tree = PathTree::new();
        tree.make_dir("zeta").unwrap();
        tree.make_dir("alpha").unwrap();
        tree.make_dir("beta").unwrap();
        tree.sort();
        let entries = tree.bfs_entries();
        assert_eq!(entries[1].node.name, "alpha");
        assert_eq!(entries[2].node.name, "beta");
        assert_eq!(entries[3].node.name, "zeta");
    }
}
