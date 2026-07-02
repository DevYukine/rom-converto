//! HFS0 (gamecard partition) container.
//!
//! Same outer shape as PFS0 but entries are 0x40 bytes instead of 0x18
//! and each entry carries a SHA-256 over the first `hashed_region_size`
//! bytes of the file (typically the first 0x200). Atmosphere/yuzu
//! reject HFS0 partitions whose recomputed hashes mismatch, so the
//! writer recomputes them rather than copying through.

use std::io::{Read, Seek, SeekFrom, Write};

use byteorder::{LE, ReadBytesExt, WriteBytesExt};
use sha2::{Digest, Sha256};

use crate::nintendo::nx::constants::{HFS0_ENTRY_SIZE, HFS0_HEADER_SIZE, HFS0_MAGIC};
use crate::nintendo::nx::error::{NxError, NxResult};

pub const DEFAULT_HASHED_REGION: u32 = 0x200;

#[derive(Debug, Clone)]
pub struct Hfs0FileRef {
    pub name: String,
    pub data_offset: u64,
    pub size: u64,
    pub hashed_region_size: u32,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct Hfs0 {
    pub files: Vec<Hfs0FileRef>,
    pub data_section_offset: u64,
}

impl Hfs0 {
    pub fn read<R: Read + Seek>(reader: &mut R) -> NxResult<Self> {
        let header_pos = reader.stream_position()?;

        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if magic != HFS0_MAGIC {
            return Err(NxError::Hfs0BadMagic);
        }
        let file_count = reader.read_u32::<LE>()?;
        let string_table_size = reader.read_u32::<LE>()?;
        let _reserved = reader.read_u32::<LE>()?;

        let mut entries = Vec::with_capacity(file_count as usize);
        for _ in 0..file_count {
            let data_offset = reader.read_u64::<LE>()?;
            let size = reader.read_u64::<LE>()?;
            let name_offset = reader.read_u32::<LE>()?;
            let hashed_region_size = reader.read_u32::<LE>()?;
            let _entry_reserved = reader.read_u64::<LE>()?;
            let mut sha256 = [0u8; 32];
            reader.read_exact(&mut sha256)?;
            entries.push((data_offset, size, name_offset, hashed_region_size, sha256));
        }

        let mut string_table = vec![0u8; string_table_size as usize];
        reader.read_exact(&mut string_table)?;

        let header_total = HFS0_HEADER_SIZE as u64
            + (file_count as u64) * HFS0_ENTRY_SIZE as u64
            + string_table_size as u64;
        let data_section_offset = header_pos + header_total;
        reader.seek(SeekFrom::Start(data_section_offset))?;

        let files = entries
            .into_iter()
            .map(|(off, sz, name_off, hashed, sha)| Hfs0FileRef {
                name: read_c_string(&string_table, name_off as usize),
                data_offset: off,
                size: sz,
                hashed_region_size: hashed,
                sha256: sha,
            })
            .collect();

        Ok(Self {
            files,
            data_section_offset,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Hfs0HeaderBytes {
    pub bytes: Vec<u8>,
    pub entries: Vec<Hfs0EntryRecord>,
}

#[derive(Debug, Clone, Copy)]
pub struct Hfs0EntryRecord {
    pub data_offset: u64,
    pub size: u64,
    pub name_offset: u32,
    pub hashed_region_size: u32,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct Hfs0FileSpec {
    pub name: String,
    pub size: u64,
    pub sha256: [u8; 32],
    pub hashed_region_size: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Hfs0LayoutHints {
    /// Pad the string table so the total HFS0 header size matches
    /// the input. Without this, gamecard root HFS0s emit a different
    /// data-section offset than nsz and per-NCA SHA-256 round-trips
    /// still pass but the file isn't byte-identical.
    pub target_total_header_size: Option<usize>,
    /// First file's data_offset relative to the data section.
    /// Defaults to 0 (files placed contiguously at data section
    /// start). nsz preserves the input's value.
    pub first_file_data_offset: u64,
}

pub fn build_header(specs: &[Hfs0FileSpec], hints: &Hfs0LayoutHints) -> NxResult<Hfs0HeaderBytes> {
    let mut string_table: Vec<u8> = Vec::new();
    let mut name_offsets = Vec::with_capacity(specs.len());
    for spec in specs {
        name_offsets.push(string_table.len() as u32);
        string_table.extend_from_slice(spec.name.as_bytes());
        string_table.push(0);
    }
    while !string_table.len().is_multiple_of(0x10) {
        string_table.push(0);
    }
    let entries_size = specs.len() * HFS0_ENTRY_SIZE;
    let header_overhead = HFS0_HEADER_SIZE + entries_size;
    if let Some(target) = hints.target_total_header_size {
        let want_string_table = target.saturating_sub(header_overhead);
        // Honor the input's value verbatim, even if it is shorter
        // than the 0x10-aligned default used here; this matches nsz's behavior
        // where `getStringTableSize` is forwarded byte-exact.
        string_table.resize(want_string_table, 0);
    }

    let mut data_offset: u64 = hints.first_file_data_offset;
    let mut entries = Vec::with_capacity(specs.len());
    for (i, spec) in specs.iter().enumerate() {
        entries.push(Hfs0EntryRecord {
            data_offset,
            size: spec.size,
            name_offset: name_offsets[i],
            hashed_region_size: spec.hashed_region_size,
            sha256: spec.sha256,
        });
        data_offset = data_offset.saturating_add(spec.size);
    }

    let total_header_size = header_overhead + string_table.len();

    let mut bytes = Vec::with_capacity(total_header_size);
    bytes.write_all(&HFS0_MAGIC)?;
    bytes.write_u32::<LE>(specs.len() as u32)?;
    bytes.write_u32::<LE>(string_table.len() as u32)?;
    bytes.write_u32::<LE>(0)?;
    for entry in &entries {
        bytes.write_u64::<LE>(entry.data_offset)?;
        bytes.write_u64::<LE>(entry.size)?;
        bytes.write_u32::<LE>(entry.name_offset)?;
        bytes.write_u32::<LE>(entry.hashed_region_size)?;
        bytes.write_u64::<LE>(0)?;
        bytes.write_all(&entry.sha256)?;
    }
    bytes.extend_from_slice(&string_table);

    Ok(Hfs0HeaderBytes { bytes, entries })
}

pub fn hash_first_chunk(data: &[u8], hashed_region_size: u32) -> [u8; 32] {
    let take = (hashed_region_size as usize).min(data.len());
    let mut h = Sha256::new();
    h.update(&data[..take]);
    h.finalize().into()
}

fn read_c_string(table: &[u8], offset: usize) -> String {
    let end = table[offset..]
        .iter()
        .position(|&b| b == 0)
        .map(|n| offset + n)
        .unwrap_or(table.len());
    String::from_utf8_lossy(&table[offset..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_two_files() {
        let payload_a = vec![0x10u8; 0x400];
        let payload_b = vec![0x20u8; 0x400];
        let specs = vec![
            Hfs0FileSpec {
                name: "alpha".into(),
                size: payload_a.len() as u64,
                sha256: hash_first_chunk(&payload_a, DEFAULT_HASHED_REGION),
                hashed_region_size: DEFAULT_HASHED_REGION,
            },
            Hfs0FileSpec {
                name: "beta".into(),
                size: payload_b.len() as u64,
                sha256: hash_first_chunk(&payload_b, DEFAULT_HASHED_REGION),
                hashed_region_size: DEFAULT_HASHED_REGION,
            },
        ];
        let hdr = build_header(&specs, &Hfs0LayoutHints::default()).unwrap();
        let mut blob = hdr.bytes;
        blob.extend_from_slice(&payload_a);
        blob.extend_from_slice(&payload_b);

        let mut cur = Cursor::new(&blob);
        let parsed = Hfs0::read(&mut cur).unwrap();
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[0].name, "alpha");
        assert_eq!(parsed.files[1].name, "beta");
        assert_eq!(parsed.files[0].size, payload_a.len() as u64);
        assert_eq!(parsed.files[1].sha256, specs[1].sha256);

        let absolute_a = parsed.data_section_offset + parsed.files[0].data_offset;
        assert_eq!(
            &blob[absolute_a as usize..absolute_a as usize + payload_a.len()],
            payload_a.as_slice()
        );
    }

    #[test]
    fn detects_bad_magic() {
        let mut bad = vec![0u8; 0x10];
        bad[0..4].copy_from_slice(b"XYZW");
        let mut cur = Cursor::new(bad);
        assert!(matches!(
            Hfs0::read(&mut cur).unwrap_err(),
            NxError::Hfs0BadMagic
        ));
    }
}
