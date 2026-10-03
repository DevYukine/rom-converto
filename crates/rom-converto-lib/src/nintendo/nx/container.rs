//! Container detection and XCI walking.
//!
//! Magic-byte sniff distinguishes NSP/NSZ (PFS0) from XCI/XCZ
//! (gamecard image). The HFS0 root sits at the offset stored in the
//! gamecard header at byte 0x130 (`partition_fs_header_address`),
//! not at a fixed location, so it is read dynamically. Once the kind
//! is known, the partition list comes from a single PFS0 read or
//! from walking the four named XCI sub-HFS0s. NCAs in XCIs always
//! live in `secure`; the loader still reads `update`/`logo`/`normal`
//! so a re-pack can pass them through.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use byteorder::{LE, ReadBytesExt};

use crate::nintendo::nx::constants::{HFS0_MAGIC, PFS0_MAGIC, XCI_PARTITIONS};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::hfs0::{Hfs0, Hfs0FileRef};
use crate::nintendo::nx::models::pfs0::{Pfs0, Pfs0FileRef};

pub(crate) const XCI_HEAD_MAGIC_OFFSET: u64 = 0x100;
const XCI_HEAD_MAGIC: [u8; 4] = *b"HEAD";
/// Gamecard magic NxEmu's decrypted DXCI carries in place of `HEAD`.
pub const DXCI_MAGIC: [u8; 4] = *b"DXCI";
const XCI_HFS0_OFFSET_FIELD: u64 = 0x130;

/// The four Switch container formats this crate can read or produce.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ContainerKind {
    Nsp,
    Nsz,
    Xci,
    Xcz,
}

impl ContainerKind {
    /// Returns `true` for the NCZ-bearing formats (NSZ, XCZ).
    pub fn is_compressed(self) -> bool {
        matches!(self, ContainerKind::Nsz | ContainerKind::Xcz)
    }

    /// Returns `true` for the gamecard-image formats (XCI, XCZ).
    pub fn is_xci(self) -> bool {
        matches!(self, ContainerKind::Xci | ContainerKind::Xcz)
    }
}

/// Sniff a file to decide which container it is. Extension is used
/// only to pick between NSP/NSZ when the magic alone can't tell them
/// apart (both are PFS0). NxEmu's decrypted DNSP/DXCI share the
/// container layout and detect as NSP/XCI.
pub fn detect_container(path: &Path) -> NxResult<ContainerKind> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 4];
    file.read_exact(&mut head)?;
    if head == PFS0_MAGIC {
        return Ok(ContainerKind::from_pfs0_extension(path));
    }
    file.seek(SeekFrom::Start(XCI_HEAD_MAGIC_OFFSET))?;
    let mut head_magic = [0u8; 4];
    if file.read_exact(&mut head_magic).is_ok()
        && (head_magic == XCI_HEAD_MAGIC || head_magic == DXCI_MAGIC)
    {
        let hfs0_off = read_xci_hfs0_offset(&mut file)?;
        file.seek(SeekFrom::Start(hfs0_off))?;
        let mut hfs0_magic = [0u8; 4];
        if file.read_exact(&mut hfs0_magic).is_ok() && hfs0_magic == HFS0_MAGIC {
            return Ok(ContainerKind::from_xci_extension(path));
        }
    }
    Err(NxError::UnknownContainer)
}

/// Reads the HFS0 root offset from the gamecard header at byte 0x130
/// (`partition_fs_header_address`).
///
/// # Errors
/// Fails on the underlying seek/read I/O errors.
pub fn read_xci_hfs0_offset(file: &mut File) -> NxResult<u64> {
    file.seek(SeekFrom::Start(XCI_HFS0_OFFSET_FIELD))?;
    Ok(file.read_u64::<LE>()?)
}

impl ContainerKind {
    fn from_pfs0_extension(path: &Path) -> Self {
        match path.extension().and_then(|s| s.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("nsz") => ContainerKind::Nsz,
            _ => ContainerKind::Nsp,
        }
    }

    fn from_xci_extension(path: &Path) -> Self {
        match path.extension().and_then(|s| s.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("xcz") => ContainerKind::Xcz,
            _ => ContainerKind::Xci,
        }
    }
}

/// Flat view of a Switch container: all files (NCA + tickets + cnmt
/// for NSP/NSZ; the union of every sub-partition for XCI/XCZ),
/// keyed by `(partition, name)`.
#[derive(Debug, Clone)]
pub struct ContainerEntry {
    pub partition: Option<&'static str>,
    pub name: String,
    pub abs_offset: u64,
    pub size: u64,
}

/// A container's detected kind plus its flattened file listing.
#[derive(Debug, Clone)]
pub struct ContainerListing {
    pub kind: ContainerKind,
    pub entries: Vec<ContainerEntry>,
}

/// Detects the container kind of `path` and lists every file inside it.
///
/// # Errors
/// Fails if the container's magic bytes are unrecognized, or on the
/// underlying I/O or header-parsing errors.
pub fn list_container(path: &Path) -> NxResult<ContainerListing> {
    let kind = detect_container(path)?;
    let entries = match kind {
        ContainerKind::Nsp | ContainerKind::Nsz => list_pfs0(path)?,
        ContainerKind::Xci | ContainerKind::Xcz => list_xci(path)?,
    };
    Ok(ContainerListing { kind, entries })
}

fn list_pfs0(path: &Path) -> NxResult<Vec<ContainerEntry>> {
    let mut reader = BufReader::new(File::open(path)?);
    let pfs0 = Pfs0::read(&mut reader)?;
    pfs0.files
        .into_iter()
        .map(|f: Pfs0FileRef| {
            Ok(ContainerEntry {
                partition: None,
                abs_offset: pfs0
                    .data_section_offset
                    .checked_add(f.data_offset)
                    .ok_or(NxError::OverlappingEntries)?,
                name: f.name,
                size: f.size,
            })
        })
        .collect()
}

/// Walks the root HFS0 of a gamecard image and reads each
/// sub-partition header, returning the root entry, the partition's
/// absolute offset, and the parsed sub-HFS0.
///
/// A real gamecard references each known partition at most once;
/// repeated extents would reparse the same table and re-emit its
/// whole file list per reference, so the entry count is capped at the
/// known partition set and each absolute offset is visited once.
pub(crate) fn xci_root_partitions<'a, R: Read + Seek>(
    reader: &mut R,
    root: &'a Hfs0,
) -> NxResult<Vec<(&'a Hfs0FileRef, u64, Hfs0)>> {
    if root.files.len() > XCI_PARTITIONS.len() {
        return Err(NxError::InvalidXci);
    }
    let mut visited = HashSet::new();
    let mut partitions = Vec::with_capacity(root.files.len());
    for entry in &root.files {
        let part_abs_offset = root
            .data_section_offset
            .checked_add(entry.data_offset)
            .ok_or(NxError::OverlappingEntries)?;
        if !visited.insert(part_abs_offset) {
            return Err(NxError::OverlappingEntries);
        }
        reader.seek(SeekFrom::Start(part_abs_offset))?;
        let sub = Hfs0::read(reader)?;
        partitions.push((entry, part_abs_offset, sub));
    }
    Ok(partitions)
}

fn list_xci(path: &Path) -> NxResult<Vec<ContainerEntry>> {
    let hfs0_off = {
        let mut probe = File::open(path)?;
        read_xci_hfs0_offset(&mut probe)?
    };
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(hfs0_off))?;
    let root = Hfs0::read(&mut reader)?;

    let mut out = Vec::new();
    for (root_entry, _, sub) in xci_root_partitions(&mut reader, &root)? {
        let partition_name = name_to_static(&root_entry.name);
        for f in sub.files {
            out.push(ContainerEntry {
                partition: partition_name,
                abs_offset: sub
                    .data_section_offset
                    .checked_add(f.data_offset)
                    .ok_or(NxError::OverlappingEntries)?,
                name: f.name,
                size: f.size,
            });
        }
    }
    Ok(out)
}

fn name_to_static(name: &str) -> Option<&'static str> {
    XCI_PARTITIONS
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f
    }

    #[test]
    fn rejects_unknown_container() {
        let f = write(&[0u8; 0x100]);
        let err = detect_container(f.path()).unwrap_err();
        assert!(matches!(err, NxError::UnknownContainer));
    }

    #[test]
    fn rejects_repeated_xci_partition_offset() {
        let f = write_xci(b"update\0secure\0\0\0", &[(0, 0, 0x10), (0, 7, 0x10)]);
        let err = list_xci(f.path()).unwrap_err();
        assert!(matches!(err, NxError::OverlappingEntries));
    }

    #[test]
    fn rejects_root_partition_count_over_known_set() {
        // Five root entries exceed the four known partitions even with
        // distinct names and offsets.
        let entries: Vec<(u64, u32, u64)> = (0u32..5)
            .map(|i| (0x10 * u64::from(i), 3 * i, 0x10u64))
            .collect();
        let f = write_xci(b"n0\0n1\0n2\0n3\0n4\0", &entries);
        let err = list_xci(f.path()).unwrap_err();
        assert!(matches!(err, NxError::InvalidXci));
    }

    /// Builds a synthetic gamecard image whose root HFS0 at 0x200
    /// holds one entry per `(data_offset, name_offset, size)` triple
    /// over `table`, followed by an empty sub-HFS0.
    fn write_xci(table: &[u8], entries: &[(u64, u32, u64)]) -> NamedTempFile {
        let mut buf = vec![0u8; 0x400];
        buf[0x100..0x104].copy_from_slice(b"HEAD");
        buf[0x130..0x138].copy_from_slice(&0x200u64.to_le_bytes());

        let mut root = Vec::new();
        root.extend_from_slice(b"HFS0");
        root.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        root.extend_from_slice(&(table.len() as u32).to_le_bytes());
        root.extend_from_slice(&0u32.to_le_bytes());
        for (data_offset, name_offset, size) in entries {
            root.extend_from_slice(&data_offset.to_le_bytes());
            root.extend_from_slice(&size.to_le_bytes());
            root.extend_from_slice(&name_offset.to_le_bytes());
            root.extend_from_slice(&0u32.to_le_bytes());
            root.extend_from_slice(&0u64.to_le_bytes());
            root.extend_from_slice(&[0u8; 32]);
        }
        root.extend_from_slice(table);
        buf[0x200..0x200 + root.len()].copy_from_slice(&root);

        let sub = 0x200 + root.len();
        buf[sub..sub + 4].copy_from_slice(b"HFS0");

        write(&buf)
    }
}
