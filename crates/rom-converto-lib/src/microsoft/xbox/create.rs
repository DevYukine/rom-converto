//! extract-xiso's create path, driven from either a filesystem directory
//! or an existing XDVDFS image. The image source is the trim/rebuild
//! route: the source tree is read back out of the old dirtabs and
//! re-laid-out from scratch, so a full disc image becomes a freshly
//! packed trimmed XISO rather than a byte slice that keeps the mastering
//! tool's padding.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::XisoCreateOptions;
use super::error::{XboxError, XboxResult};
use crate::microsoft::xdvdfs::{
    ATTR_ARCHIVE, ATTR_DIRECTORY, SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR, VOLUME_MAGIC,
    XBOX_PROBE_BASES, XdvdfsVolume, data_offset, walk_dir_tables,
};
use crate::util::CancelToken;

/// First sector the linear allocator hands out. extract-xiso seeds here
/// rather than at the format minimum of 33.
const FIRST_DATA_SECTOR: u64 = 0x108;
/// The image length is padded up to a multiple of this (`XISO_FILE_MODULUS`).
const FILE_MODULUS: u64 = 0x10000;
/// Byte offset of the extract-xiso "already optimized" tag.
const OPTIMIZED_TAG_OFFSET: usize = 31337;
/// Largest a single directory table may grow: past this the u16 word
/// offsets in a dirent can no longer address every entry.
const MAX_DIRTAB_BYTES: u64 = 262_140;
/// Fixed part of a dirent, ahead of the filename.
const DIRENT_FIXED: u64 = 14;
const COPY_BUF: usize = 1024 * 1024;

/// The XDK's media-type check, and the byte that turns its `jge` into an
/// unconditional `jmp`.
const MEDIA_PATTERN: [u8; 8] = [0xE8, 0xCA, 0xFD, 0xFF, 0xFF, 0x85, 0xC0, 0x7D];
const MEDIA_PATCH_BYTE: u8 = 0xEB;
/// Bytes carried between reads so a pattern split across buffers still
/// matches. One short of the pattern, which is why the byte the patch
/// rewrites (the pattern's last) always lands in the current buffer.
const MEDIA_CARRY: usize = MEDIA_PATTERN.len() - 1;

/// Unicode codepoints for Windows-1252 bytes 0x80-0x9F, in order. The
/// inverse of `xdvdfs::dirent`'s decode table.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Encodes to Windows-1252, or `None` for a character the encoding cannot
/// represent.
fn encode_cp1252(name: &str) -> Option<Vec<u8>> {
    name.chars()
        .map(|c| match c {
            '\u{0}'..='\u{7F}' | '\u{A0}'..='\u{FF}' => Some(c as u8),
            _ => CP1252_HIGH
                .iter()
                .position(|&high| high == c)
                .map(|i| 0x80 + i as u8),
        })
        .collect()
}

/// Where a file's bytes come from.
enum Locator {
    Fs(PathBuf),
    /// Absolute byte offset into the source image.
    Image(u64),
}

enum Payload {
    File {
        locator: Locator,
        size: u64,
        sector: u32,
    },
    Dir(DirTable),
}

struct Node {
    name: String,
    /// Windows-1252 name bytes, exactly as they land in the dirtab.
    raw: Vec<u8>,
    /// Byte offset within the parent's dirtab.
    offset: u64,
    /// BST children, as indices into the parent's node vector.
    left: Option<usize>,
    right: Option<usize>,
    payload: Payload,
}

#[derive(Default)]
struct DirTable {
    nodes: Vec<Node>,
    /// Index of the BST root in `nodes`.
    root: Option<usize>,
    /// Dirtab length in bytes, always a whole number of sectors.
    size: u64,
    sector: u32,
}

/// Total bytes the create path will stream, so the caller can size a
/// progress bar before the layout engine runs.
pub fn input_total_bytes(input: &Path) -> XboxResult<u64> {
    if fs::metadata(input)?.is_dir() {
        Ok(dir_bytes(input)?)
    } else {
        Ok(super::info::read_info(input)?.total_file_bytes)
    }
}

fn dir_bytes(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        // symlink_metadata, not metadata: following a symlink here risks
        // an infinite recursion on a directory symlink loop.
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            total += dir_bytes(&path)?;
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

pub(super) fn create_blocking(
    input: &Path,
    output: &Path,
    options: XisoCreateOptions,
    bytes_done: Arc<AtomicU64>,
    cancel: &CancelToken,
) -> XboxResult<()> {
    let (image, nodes) = if fs::metadata(input)?.is_dir() {
        (None, scan_dir(input)?)
    } else {
        let (file, nodes) = scan_image(input)?;
        (Some(file), nodes)
    };
    let mut root = DirTable {
        nodes,
        ..DirTable::default()
    };

    lay_out(&mut root, "")?;

    root.sector = FIRST_DATA_SECTOR as u32;
    let mut next = FIRST_DATA_SECTOR + root.size / SECTOR_SIZE;
    allocate(&mut root, &mut next);
    if next > u32::MAX as u64 {
        return Err(XboxError::ImageTooLarge { sectors: next });
    }
    let image_size = (next * SECTOR_SIZE).next_multiple_of(FILE_MODULUS);

    let mut writer = Writer {
        out: io::BufWriter::with_capacity(COPY_BUF, File::create(output)?),
        image,
        media_patch: options.media_patch,
        buf: vec![0u8; COPY_BUF],
        bytes_done: &bytes_done,
        cancel,
    };
    writer.write_image(&root, next, image_size)?;
    writer.out.flush()?;
    Ok(())
}

/// Reads a host directory tree, rejecting names the format cannot carry.
fn scan_dir(dir: &Path) -> XboxResult<Vec<Node>> {
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut nodes = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = entry.path();
        // symlink_metadata, not metadata: following a symlink here risks
        // an infinite recursion on a directory symlink loop.
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_dir() && !meta.is_file() {
            continue;
        }
        let os_name = entry.file_name();
        let Some(raw) = os_name.to_str().and_then(encode_cp1252) else {
            return Err(XboxError::NameNotCp1252 {
                name: os_name.to_string_lossy().into_owned(),
            });
        };
        nodes.push(Node {
            name: os_name.to_string_lossy().into_owned(),
            raw,
            offset: 0,
            left: None,
            right: None,
            payload: if meta.is_dir() {
                Payload::Dir(DirTable {
                    nodes: scan_dir(&path)?,
                    ..DirTable::default()
                })
            } else {
                Payload::File {
                    locator: Locator::Fs(path),
                    size: meta.len(),
                    sector: 0,
                }
            },
        });
    }
    Ok(nodes)
}

/// Reads an existing XDVDFS image's tree back out of its dirtabs.
fn scan_image(path: &Path) -> XboxResult<(File, Vec<Node>)> {
    let mut file = File::open(path)?;
    let volume = XdvdfsVolume::probe(&mut file, &XBOX_PROBE_BASES)?;

    let mut flat: HashMap<Vec<String>, Vec<Node>> = HashMap::new();
    walk_dir_tables(&mut file, &volume, |parent, entry| {
        let node = Node {
            name: entry.name_str(),
            raw: entry.name.clone(),
            offset: 0,
            left: None,
            right: None,
            payload: if entry.is_directory() {
                Payload::Dir(DirTable::default())
            } else {
                Payload::File {
                    locator: Locator::Image(data_offset(&volume, entry.start_sector)),
                    size: entry.size as u64,
                    sector: 0,
                }
            },
        };
        flat.entry(parent.to_vec()).or_default().push(node);
        Ok(())
    })?;

    let nodes = assemble(&mut flat, &mut Vec::new());
    Ok((file, nodes))
}

/// Nests the flat `(parent path, entry)` stream `walk_dir_tables` emits.
/// A directory's own entry always arrives before its contents, so every
/// child bucket exists by the time its parent is assembled.
fn assemble(flat: &mut HashMap<Vec<String>, Vec<Node>>, path: &mut Vec<String>) -> Vec<Node> {
    let mut nodes = flat.remove(path).unwrap_or_default();
    for node in &mut nodes {
        if let Payload::Dir(child) = &mut node.payload {
            path.push(node.name.clone());
            child.nodes = assemble(flat, path);
            path.pop();
        }
    }
    nodes
}

/// Sorts one directory with the on-disk comparator, links it into a
/// balanced BST, and packs its dirtab; then recurses. Balance is not
/// required for correctness (any BST under this comparator works), it
/// only bounds the console driver's lookup depth.
fn lay_out(table: &mut DirTable, path: &str) -> XboxResult<()> {
    validate(&table.nodes, path)?;
    // extract-xiso compares names with a signed `char`, so cp1252 high
    // bytes (0x80-0xFF) sort before ASCII rather than after; map through
    // `i8` to match its on-disk BST order.
    table.nodes.sort_by_cached_key(|node| {
        node.raw
            .to_ascii_uppercase()
            .into_iter()
            .map(|b| b as i8)
            .collect::<Vec<i8>>()
    });

    let len = table.nodes.len();
    table.root = link_bst(&mut table.nodes, 0, len);

    let used = pack(&mut table.nodes, table.root);
    if used >= MAX_DIRTAB_BYTES {
        return Err(XboxError::DirTableTooLarge {
            path: if path.is_empty() { "/" } else { path }.to_string(),
            used,
        });
    }
    // An empty directory is still one 0xFF sector, and its parent dirent
    // records a size of 2048.
    table.size = used.next_multiple_of(SECTOR_SIZE).max(SECTOR_SIZE);

    for node in &mut table.nodes {
        if let Payload::Dir(child) = &mut node.payload {
            lay_out(child, &format!("{path}/{}", node.name))?;
        }
    }
    Ok(())
}

/// Rejects names the format cannot represent, and sibling names that
/// collide case-insensitively (which would corrupt the BST).
fn validate(nodes: &[Node], path: &str) -> XboxResult<()> {
    let mut seen = HashSet::with_capacity(nodes.len());
    for node in nodes {
        if node.raw.len() > 255 {
            return Err(XboxError::NameTooLong {
                name: node.name.clone(),
            });
        }
        if !seen.insert(node.raw.to_ascii_uppercase()) {
            return Err(XboxError::DuplicateName {
                path: format!("{path}/{}", node.name),
            });
        }
    }
    Ok(())
}

/// Links the sorted range `lo..hi` into a balanced BST, returning its root.
fn link_bst(nodes: &mut [Node], lo: usize, hi: usize) -> Option<usize> {
    if lo >= hi {
        return None;
    }
    let mid = lo + (hi - lo) / 2;
    nodes[mid].left = link_bst(nodes, lo, mid);
    nodes[mid].right = link_bst(nodes, mid + 1, hi);
    Some(mid)
}

fn preorder(nodes: &[Node], root: Option<usize>) -> Vec<usize> {
    let mut order = Vec::with_capacity(nodes.len());
    let mut stack: Vec<usize> = root.into_iter().collect();
    while let Some(i) = stack.pop() {
        order.push(i);
        if let Some(right) = nodes[i].right {
            stack.push(right);
        }
        if let Some(left) = nodes[i].left {
            stack.push(left);
        }
    }
    order
}

fn entry_size(name_len: usize) -> u64 {
    (DIRENT_FIXED + name_len as u64).next_multiple_of(4)
}

/// Assigns each entry its byte offset within the dirtab, walking the BST
/// in preorder and bumping any entry that would straddle a sector
/// boundary to the start of the next sector. Returns the bytes used.
fn pack(nodes: &mut [Node], root: Option<usize>) -> u64 {
    let mut offset = 0u64;
    for i in preorder(nodes, root) {
        let size = entry_size(nodes[i].raw.len());
        if offset % SECTOR_SIZE + size > SECTOR_SIZE {
            offset = offset.next_multiple_of(SECTOR_SIZE);
        }
        nodes[i].offset = offset;
        offset += size;
    }
    offset
}

/// The linear allocator: this table's own sectors are already assigned,
/// so hand out its files in BST preorder, then each subdirectory (its
/// dirtab, then everything under it) in the same order.
fn allocate(table: &mut DirTable, next: &mut u64) {
    for i in preorder(&table.nodes, table.root) {
        if let Payload::File { size, sector, .. } = &mut table.nodes[i].payload {
            *sector = *next as u32;
            *next += size.div_ceil(SECTOR_SIZE);
        }
    }
    for i in preorder(&table.nodes, table.root) {
        if let Payload::Dir(child) = &mut table.nodes[i].payload {
            child.sector = *next as u32;
            *next += child.size / SECTOR_SIZE;
            allocate(child, next);
        }
    }
}

struct Writer<'a> {
    out: io::BufWriter<File>,
    image: Option<File>,
    media_patch: bool,
    buf: Vec<u8>,
    bytes_done: &'a AtomicU64,
    cancel: &'a CancelToken,
}

impl Writer<'_> {
    /// Emits the whole image in ascending sector order, which is exactly
    /// the order [`allocate`] handed sectors out in.
    fn write_image(
        &mut self,
        root: &DirTable,
        total_sectors: u64,
        image_size: u64,
    ) -> XboxResult<()> {
        self.write_lead_in(image_size / SECTOR_SIZE)?;
        self.write_volume_descriptor(root)?;
        let gap = FIRST_DATA_SECTOR - (VOLUME_DESCRIPTOR_SECTOR as u64 + 1);
        write_fill(&mut self.out, 0x00, gap * SECTOR_SIZE)?;

        self.write_table(root)?;

        write_fill(
            &mut self.out,
            0x00,
            image_size - total_sectors * SECTOR_SIZE,
        )?;
        Ok(())
    }

    /// The first 0x10000 bytes: zeros, plus the cosmetic ISO 9660
    /// descriptors and the extract-xiso tag that PC tools look for.
    fn write_lead_in(&mut self, image_sectors: u64) -> io::Result<()> {
        let mut lead = vec![0u8; (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize];
        let tag = format!("in!xiso!{}", env!("CARGO_PKG_VERSION"));
        lead[OPTIMIZED_TAG_OFFSET..OPTIMIZED_TAG_OFFSET + tag.len()]
            .copy_from_slice(tag.as_bytes());
        write_iso9660(&mut lead[0x8000..0x9000], image_sectors as u32);
        self.out.write_all(&lead)
    }

    fn write_volume_descriptor(&mut self, root: &DirTable) -> io::Result<()> {
        let mut descriptor = vec![0u8; SECTOR_SIZE as usize];
        descriptor[0x00..0x14].copy_from_slice(VOLUME_MAGIC);
        descriptor[0x14..0x18].copy_from_slice(&root.sector.to_le_bytes());
        descriptor[0x18..0x1C].copy_from_slice(&(root.size as u32).to_le_bytes());
        // The FILETIME at 0x1C stays zero, as extract-xiso emits.
        descriptor[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
        self.out.write_all(&descriptor)
    }

    fn write_table(&mut self, table: &DirTable) -> XboxResult<()> {
        self.write_dirtab(table)?;
        for i in preorder(&table.nodes, table.root) {
            let node = &table.nodes[i];
            if let Payload::File { locator, size, .. } = &node.payload {
                let patch = self.media_patch && is_xbe(&node.name);
                self.copy_file(locator, *size, patch)?;
            }
        }
        for i in preorder(&table.nodes, table.root) {
            if let Payload::Dir(child) = &table.nodes[i].payload {
                self.write_table(child)?;
            }
        }
        Ok(())
    }

    fn write_dirtab(&mut self, table: &DirTable) -> XboxResult<()> {
        let mut buf = vec![0xFFu8; table.size as usize];
        for node in &table.nodes {
            let (sector, size, attributes) = match &node.payload {
                Payload::File { sector, size, .. } => (*sector, *size as u32, ATTR_ARCHIVE),
                Payload::Dir(child) => (child.sector, child.size as u32, ATTR_DIRECTORY),
            };
            let at = node.offset as usize;
            buf[at..at + 2].copy_from_slice(&child_word(&table.nodes, node.left).to_le_bytes());
            buf[at + 2..at + 4]
                .copy_from_slice(&child_word(&table.nodes, node.right).to_le_bytes());
            buf[at + 4..at + 8].copy_from_slice(&sector.to_le_bytes());
            buf[at + 8..at + 12].copy_from_slice(&size.to_le_bytes());
            buf[at + 12] = attributes;
            buf[at + 13] = node.raw.len() as u8;
            buf[at + 14..at + 14 + node.raw.len()].copy_from_slice(&node.raw);
        }
        self.out.write_all(&buf)?;
        Ok(())
    }

    fn copy_file(&mut self, locator: &Locator, size: u64, patch: bool) -> XboxResult<()> {
        let Self {
            out,
            image,
            buf,
            bytes_done,
            cancel,
            ..
        } = self;

        let mut opened;
        let src: &mut dyn Read = match locator {
            Locator::Fs(path) => {
                opened = File::open(path)?;
                &mut opened
            }
            Locator::Image(offset) => {
                let image = image
                    .as_mut()
                    .expect("image locators only come from an image source");
                image.seek(SeekFrom::Start(*offset))?;
                image
            }
        };

        let mut patcher = MediaPatcher::default();
        let mut left = size;
        while left > 0 {
            if cancel.is_cancelled() {
                return Err(XboxError::Cancelled);
            }
            let take = left.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..take])?;
            if patch {
                patcher.apply(&mut buf[..take]);
            }
            out.write_all(&buf[..take])?;
            bytes_done.fetch_add(take as u64, Ordering::Relaxed);
            left -= take as u64;
        }

        write_fill(out, 0xFF, size.next_multiple_of(SECTOR_SIZE) - size)?;
        Ok(())
    }
}

fn is_xbe(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".xbe")
}

fn child_word(nodes: &[Node], child: Option<usize>) -> u16 {
    child.map_or(0, |i| (nodes[i].offset / 4) as u16)
}

fn write_fill<W: Write>(out: &mut W, byte: u8, mut count: u64) -> io::Result<()> {
    let block = [byte; SECTOR_SIZE as usize];
    while count > 0 {
        let take = count.min(block.len() as u64) as usize;
        out.write_all(&block[..take])?;
        count -= take as u64;
    }
    Ok(())
}

/// Minimal ECMA-119 descriptors so PC tools see a mountable volume. The
/// console ignores them; `region` is the two sectors at file offset
/// 0x8000.
fn write_iso9660(region: &mut [u8], image_sectors: u32) {
    let (pvd, terminator) = region.split_at_mut(SECTOR_SIZE as usize);

    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[80..84].copy_from_slice(&image_sectors.to_le_bytes());
    pvd[84..88].copy_from_slice(&image_sectors.to_be_bytes());
    pvd[120..122].copy_from_slice(&1u16.to_le_bytes());
    pvd[122..124].copy_from_slice(&1u16.to_be_bytes());
    pvd[124..126].copy_from_slice(&1u16.to_le_bytes());
    pvd[126..128].copy_from_slice(&1u16.to_be_bytes());
    pvd[128..130].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    pvd[130..132].copy_from_slice(&(SECTOR_SIZE as u16).to_be_bytes());
    pvd[881] = 1;

    terminator[0] = 0xFF;
    terminator[1..6].copy_from_slice(b"CD001");
    terminator[6] = 1;
}

/// Rewrites every media-type check in the stream, carrying the pattern's
/// leading bytes across reads. The byte it rewrites is the pattern's
/// last, so it always lands in the buffer currently in hand.
#[derive(Default)]
struct MediaPatcher {
    carry: [u8; MEDIA_CARRY],
    carry_len: usize,
}

impl MediaPatcher {
    fn apply(&mut self, buf: &mut [u8]) {
        let total = self.carry_len + buf.len();
        for start in 0..total.saturating_sub(MEDIA_CARRY) {
            let hit = MEDIA_PATTERN.iter().enumerate().all(|(k, &want)| {
                let i = start + k;
                let got = if i < self.carry_len {
                    self.carry[i]
                } else {
                    buf[i - self.carry_len]
                };
                got == want
            });
            if hit {
                buf[start + MEDIA_CARRY - self.carry_len] = MEDIA_PATCH_BYTE;
            }
        }

        let keep = MEDIA_CARRY.min(total);
        let from_buf = keep.min(buf.len());
        let from_carry = keep - from_buf;
        let mut next = [0u8; MEDIA_CARRY];
        next[..from_carry]
            .copy_from_slice(&self.carry[self.carry_len - from_carry..self.carry_len]);
        next[from_carry..keep].copy_from_slice(&buf[buf.len() - from_buf..]);
        self.carry = next;
        self.carry_len = keep;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp1252_encodes_latin1_and_the_high_table_but_rejects_the_rest() {
        assert_eq!(encode_cp1252("AB").unwrap(), b"AB");
        assert_eq!(encode_cp1252("\u{FF}").unwrap(), vec![0xFF]);
        assert_eq!(encode_cp1252("\u{2122}").unwrap(), vec![0x99]);
        assert!(encode_cp1252("\u{4E2D}").is_none());
    }

    #[test]
    fn entries_bump_past_a_sector_boundary_rather_than_straddle_it() {
        // 14 + 242 = 256 bytes per entry, so eight fill exactly one
        // sector and the ninth would straddle.
        let long = "N".repeat(242);
        let mut nodes: Vec<Node> = (0..9)
            .map(|i| test_node(&format!("{long}{i}"), 0))
            .collect();
        let len = nodes.len();
        let root = link_bst(&mut nodes, 0, len);
        let used = pack(&mut nodes, root);

        let mut offsets: Vec<u64> = nodes.iter().map(|n| n.offset).collect();
        offsets.sort_unstable();
        for offset in &offsets {
            assert!(
                offset % SECTOR_SIZE + entry_size(243) <= SECTOR_SIZE,
                "entry at {offset} straddles a sector boundary"
            );
        }
        assert_eq!(offsets[7], 2048, "the eighth entry is bumped to sector 1");
        assert_eq!(used, 2048 + 2 * entry_size(243));
    }

    #[test]
    fn media_patch_fires_across_a_buffer_boundary() {
        let mut first = MEDIA_PATTERN[..3].to_vec();
        let mut second = MEDIA_PATTERN[3..].to_vec();

        let mut patcher = MediaPatcher::default();
        patcher.apply(&mut first);
        patcher.apply(&mut second);

        assert_eq!(first, MEDIA_PATTERN[..3]);
        assert_eq!(second.last(), Some(&MEDIA_PATCH_BYTE));
    }

    #[test]
    fn media_patch_rewrites_every_match_in_one_buffer() {
        let mut buf = [MEDIA_PATTERN, MEDIA_PATTERN].concat();
        MediaPatcher::default().apply(&mut buf);
        assert_eq!(buf[7], MEDIA_PATCH_BYTE);
        assert_eq!(buf[15], MEDIA_PATCH_BYTE);
    }

    #[test]
    fn xbe_extension_match_is_case_insensitive() {
        assert!(is_xbe("default.XBE"));
        assert!(is_xbe("Default.xbe"));
        assert!(!is_xbe("default.xbx"));
        assert!(!is_xbe("xbe"));
    }

    fn test_node(name: &str, size: u64) -> Node {
        Node {
            name: name.to_string(),
            raw: name.as_bytes().to_vec(),
            offset: 0,
            left: None,
            right: None,
            payload: Payload::File {
                locator: Locator::Image(0),
                size,
                sector: 0,
            },
        }
    }

    #[test]
    fn duplicate_names_that_differ_only_in_case_are_rejected() {
        let nodes = vec![test_node("Default.xbe", 0), test_node("DEFAULT.XBE", 0)];
        assert!(matches!(
            validate(&nodes, "").unwrap_err(),
            XboxError::DuplicateName { .. }
        ));
    }

    #[test]
    fn names_past_255_bytes_are_rejected() {
        let nodes = vec![test_node(&"a".repeat(256), 0)];
        assert!(matches!(
            validate(&nodes, "").unwrap_err(),
            XboxError::NameTooLong { .. }
        ));
    }

    #[test]
    fn sorted_order_puts_underscore_after_letters_and_shorter_first() {
        let mut nodes = [test_node("a_b", 0), test_node("abb", 0), test_node("ab", 0)];
        nodes.sort_by_cached_key(|node| node.raw.to_ascii_uppercase());
        let order: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(order, ["ab", "abb", "a_b"]);
    }

    #[test]
    fn lay_out_sorts_cp1252_high_bytes_as_signed_bytes_like_extract_xiso() {
        // A raw name byte >= 0x80 is negative as `i8`, so it must sort
        // before plain ASCII under extract-xiso's signed-char comparator
        // (the opposite of a plain unsigned-byte comparison).
        let high = Node {
            name: "high".to_string(),
            raw: vec![0x80],
            offset: 0,
            left: None,
            right: None,
            payload: Payload::File {
                locator: Locator::Image(0),
                size: 0,
                sector: 0,
            },
        };
        let mut table = DirTable {
            nodes: vec![high, test_node("A", 0)],
            ..DirTable::default()
        };
        lay_out(&mut table, "").unwrap();
        let order: Vec<&str> = table.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(order, ["high", "A"]);
    }
}
