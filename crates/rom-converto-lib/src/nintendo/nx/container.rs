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

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use byteorder::{LE, ReadBytesExt};

use crate::nintendo::nx::constants::{HFS0_MAGIC, PFS0_MAGIC, XCI_PARTITIONS};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::models::hfs0::Hfs0;
use crate::nintendo::nx::models::pfs0::{Pfs0, Pfs0FileRef};

const XCI_HEAD_MAGIC_OFFSET: u64 = 0x100;
const XCI_HEAD_MAGIC: [u8; 4] = *b"HEAD";
const XCI_HFS0_OFFSET_FIELD: u64 = 0x130;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ContainerKind {
    Nsp,
    Nsz,
    Xci,
    Xcz,
}

impl ContainerKind {
    pub fn is_compressed(self) -> bool {
        matches!(self, ContainerKind::Nsz | ContainerKind::Xcz)
    }

    pub fn is_xci(self) -> bool {
        matches!(self, ContainerKind::Xci | ContainerKind::Xcz)
    }
}

/// Sniff a file to decide which container it is. Extension is used
/// only to pick between NSP/NSZ when the magic alone can't tell them
/// apart (both are PFS0).
pub fn detect_container(path: &Path) -> NxResult<ContainerKind> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 4];
    file.read_exact(&mut head)?;
    if head == PFS0_MAGIC {
        return Ok(ContainerKind::from_pfs0_extension(path));
    }
    file.seek(SeekFrom::Start(XCI_HEAD_MAGIC_OFFSET))?;
    let mut head_magic = [0u8; 4];
    if file.read_exact(&mut head_magic).is_ok() && head_magic == XCI_HEAD_MAGIC {
        let hfs0_off = read_xci_hfs0_offset(&mut file)?;
        file.seek(SeekFrom::Start(hfs0_off))?;
        let mut hfs0_magic = [0u8; 4];
        if file.read_exact(&mut hfs0_magic).is_ok() && hfs0_magic == HFS0_MAGIC {
            return Ok(ContainerKind::from_xci_extension(path));
        }
    }
    Err(NxError::UnknownContainer)
}

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

#[derive(Debug, Clone)]
pub struct ContainerListing {
    pub kind: ContainerKind,
    pub entries: Vec<ContainerEntry>,
}

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
    Ok(pfs0
        .files
        .into_iter()
        .map(|f: Pfs0FileRef| ContainerEntry {
            partition: None,
            abs_offset: pfs0.data_section_offset + f.data_offset,
            name: f.name,
            size: f.size,
        })
        .collect())
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
    for entry in root.files {
        let partition_name = name_to_static(&entry.name);
        let part_abs_offset = root.data_section_offset + entry.data_offset;
        reader.seek(SeekFrom::Start(part_abs_offset))?;
        let sub = Hfs0::read(&mut reader)?;
        for f in sub.files {
            out.push(ContainerEntry {
                partition: partition_name,
                abs_offset: sub.data_section_offset + f.data_offset,
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
}
