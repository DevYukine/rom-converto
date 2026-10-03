//! PFS0 (Nintendo Submission Package container).
//!
//! Layout: 0x10-byte header, `count * 0x18` entries, string table
//! (size in header), then concatenated file data starting at the
//! 0x10-aligned end of the string table. Round-trip preservation
//! requires keeping the string-table padding intact.

use std::io::{Read, Seek, SeekFrom, Write};

use byteorder::{LE, ReadBytesExt, WriteBytesExt};

use crate::nintendo::nx::constants::{PFS0_ENTRY_SIZE, PFS0_HEADER_SIZE, PFS0_MAGIC};
use crate::nintendo::nx::error::{NxError, NxResult};
use crate::nintendo::nx::util::read_table_name;

/// One decoded PFS0 entry: name, offset relative to
/// `Pfs0::data_section_offset`, and size.
#[derive(Debug, Clone)]
pub struct Pfs0FileRef {
    pub name: String,
    pub data_offset: u64,
    pub size: u64,
}

/// Parsed PFS0 container: the decoded file table plus the absolute
/// offset where concatenated file data begins.
#[derive(Debug, Clone)]
pub struct Pfs0 {
    pub files: Vec<Pfs0FileRef>,
    /// Absolute offset where file data begins, equal to the size of
    /// the header + entry table + (padded) string table.
    pub data_section_offset: u64,
}

impl Pfs0 {
    /// Parse the PFS0 container at the current reader position. After
    /// the call the reader is left positioned at `data_section_offset`.
    ///
    /// Returns [`NxError::InvalidStringTable`] for invalid name offsets,
    /// names longer than 1024 bytes, or aggregate name bytes exceeding
    /// the string table's byte length.
    pub fn read<R: Read + Seek>(reader: &mut R) -> NxResult<Self> {
        let header_pos = reader.stream_position()?;

        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if magic != PFS0_MAGIC {
            return Err(NxError::Pfs0BadMagic);
        }
        let file_count = reader.read_u32::<LE>()?;
        let string_table_size = reader.read_u32::<LE>()?;
        let _reserved = reader.read_u32::<LE>()?;

        let table_bytes = u64::from(file_count)
            .checked_mul(PFS0_ENTRY_SIZE as u64)
            .and_then(|n| n.checked_add(u64::from(string_table_size)))
            .ok_or(NxError::IncompleteSection)?;
        let data_section_offset = header_pos
            .checked_add(PFS0_HEADER_SIZE as u64)
            .and_then(|n| n.checked_add(table_bytes))
            .ok_or(NxError::IncompleteSection)?;
        let after_header = reader.stream_position()?;
        let end = reader.seek(SeekFrom::End(0))?;
        if data_section_offset > end {
            return Err(NxError::IncompleteSection);
        }
        reader.seek(SeekFrom::Start(after_header))?;
        let mut entries = Vec::with_capacity(file_count as usize);
        for _ in 0..file_count {
            let data_offset = reader.read_u64::<LE>()?;
            let size = reader.read_u64::<LE>()?;
            let name_offset = reader.read_u32::<LE>()?;
            let _entry_reserved = reader.read_u32::<LE>()?;
            entries.push((data_offset, size, name_offset));
        }

        let mut string_table = vec![0u8; string_table_size as usize];
        reader.read_exact(&mut string_table)?;

        let header_total = PFS0_HEADER_SIZE as u64
            + (file_count as u64) * PFS0_ENTRY_SIZE as u64
            + string_table_size as u64;
        let data_section_offset = header_pos + header_total;
        reader.seek(SeekFrom::Start(data_section_offset))?;

        let mut total = 0usize;
        let files = entries
            .into_iter()
            .map(|(data_offset, size, name_offset)| {
                let name = read_table_name(&string_table, name_offset as usize, &mut total)?;
                Ok(Pfs0FileRef {
                    name,
                    data_offset,
                    size,
                })
            })
            .collect::<NxResult<Vec<_>>>()?;

        Ok(Self {
            files,
            data_section_offset,
        })
    }
}

/// Optional layout parameters for [`build_header`], used to reproduce
/// a byte-identical PFS0 container when round-tripping an existing
/// NSP.
#[derive(Debug, Clone, Default)]
pub struct Pfs0LayoutHints {
    /// Pad the string table so the total header size matches the
    /// input container exactly. The reference implementation uses
    /// `getStringTableSize` from input to output, keeping the entry
    /// table aligned.
    pub target_total_header_size: Option<usize>,
    /// Where the first file should sit in the output's data section
    /// (that is, `pfs0.files[0].data_offset` from input). The reference
    /// implementation preserves this so the original NSP padding round
    /// trips byte-for-byte.
    pub first_file_data_offset: u64,
}

/// Build the header bytes for a PFS0 container that will hold the
/// given named files in order. Returns the header (incl. string
/// table) plus the per-file (data_offset, size) pairs whose offsets
/// start at `hints.first_file_data_offset`. The caller writes the
/// header, then any pre-data padding (= `hints.first_file_data_offset`
/// bytes of zeros if it is non-zero), then file data contiguously.
pub fn build_header(
    file_specs: &[(String, u64)],
    hints: &Pfs0LayoutHints,
) -> NxResult<Pfs0HeaderBytes> {
    let mut string_table: Vec<u8> = Vec::new();
    let mut name_offsets = Vec::with_capacity(file_specs.len());
    for (name, _) in file_specs {
        name_offsets.push(string_table.len() as u32);
        string_table.extend_from_slice(name.as_bytes());
        string_table.push(0);
    }
    while !string_table.len().is_multiple_of(0x10) {
        string_table.push(0);
    }
    let entries_size = file_specs.len() * PFS0_ENTRY_SIZE;
    let header_overhead = PFS0_HEADER_SIZE + entries_size;
    if let Some(target) = hints.target_total_header_size {
        let want_string_table = target.saturating_sub(header_overhead);
        if want_string_table > string_table.len() {
            string_table.resize(want_string_table, 0);
        }
    }

    let mut data_offset: u64 = hints.first_file_data_offset;
    let mut entries = Vec::with_capacity(file_specs.len());
    for (i, (_, size)) in file_specs.iter().enumerate() {
        entries.push(Pfs0EntryRecord {
            data_offset,
            size: *size,
            name_offset: name_offsets[i],
        });
        data_offset = data_offset.saturating_add(*size);
    }

    let total_header_size = header_overhead + string_table.len();

    let mut bytes = Vec::with_capacity(total_header_size);
    bytes.write_all(&PFS0_MAGIC)?;
    bytes.write_u32::<LE>(file_specs.len() as u32)?;
    bytes.write_u32::<LE>(string_table.len() as u32)?;
    bytes.write_u32::<LE>(0)?;
    for entry in &entries {
        bytes.write_u64::<LE>(entry.data_offset)?;
        bytes.write_u64::<LE>(entry.size)?;
        bytes.write_u32::<LE>(entry.name_offset)?;
        bytes.write_u32::<LE>(0)?;
    }
    bytes.extend_from_slice(&string_table);

    Ok(Pfs0HeaderBytes { bytes, entries })
}

/// Output of [`build_header`]: the serialized PFS0 header bytes ready
/// to write, plus the parallel entry records describing where each
/// file lands in the data section.
#[derive(Debug, Clone)]
pub struct Pfs0HeaderBytes {
    pub bytes: Vec<u8>,
    pub entries: Vec<Pfs0EntryRecord>,
}

/// One PFS0 entry as written into the header: offset (relative to the
/// data section) and size of the file, plus its name-table offset.
#[derive(Debug, Clone, Copy)]
pub struct Pfs0EntryRecord {
    pub data_offset: u64,
    pub size: u64,
    pub name_offset: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn write_pfs0_blob(files: &[(&str, &[u8])]) -> Vec<u8> {
        let specs: Vec<(String, u64)> = files
            .iter()
            .map(|(n, b)| (n.to_string(), b.len() as u64))
            .collect();
        let hdr = build_header(&specs, &Pfs0LayoutHints::default()).unwrap();
        let mut out = hdr.bytes;
        for (_, bytes) in files {
            out.extend_from_slice(bytes);
        }
        out
    }

    #[test]
    fn round_trip_three_files() {
        let blob = write_pfs0_blob(&[
            ("a.nca", b"AAAA"),
            ("b.tik", b"BBBBBB"),
            ("c.cnmt.nca", b"CCCCCCCC"),
        ]);
        let mut cur = Cursor::new(&blob);
        let parsed = Pfs0::read(&mut cur).unwrap();
        assert_eq!(parsed.files.len(), 3);
        assert_eq!(parsed.files[0].name, "a.nca");
        assert_eq!(parsed.files[0].size, 4);
        assert_eq!(parsed.files[1].name, "b.tik");
        assert_eq!(parsed.files[2].name, "c.cnmt.nca");
        let data_off = parsed.data_section_offset;
        assert_eq!(&blob[data_off as usize..data_off as usize + 4], b"AAAA");
        assert_eq!(
            &blob[(data_off + parsed.files[1].data_offset) as usize..]
                [..parsed.files[1].size as usize],
            b"BBBBBB"
        );
    }

    #[test]
    fn rejects_huge_file_count_without_allocating() {
        // A tiny file declaring billions of entries must be rejected
        // by checked arithmetic against the actual (tiny) remaining
        // length, not by attempting `Vec::with_capacity(u32::MAX)`.
        let mut blob = vec![0u8; 0x10];
        blob[0..4].copy_from_slice(&PFS0_MAGIC);
        blob[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        blob[8..12].copy_from_slice(&0u32.to_le_bytes());
        let mut cur = Cursor::new(blob);
        let err = Pfs0::read(&mut cur).unwrap_err();
        assert!(matches!(err, NxError::IncompleteSection));
    }

    #[test]
    fn rejects_huge_string_table_size_without_allocating() {
        let mut blob = vec![0u8; 0x10];
        blob[0..4].copy_from_slice(&PFS0_MAGIC);
        blob[4..8].copy_from_slice(&0u32.to_le_bytes());
        blob[8..12].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
        let mut cur = Cursor::new(blob);
        let err = Pfs0::read(&mut cur).unwrap_err();
        assert!(matches!(err, NxError::IncompleteSection));
    }

    #[test]
    fn rejects_name_offset_beyond_string_table() {
        let mut blob = write_pfs0_blob(&[("a.nca", b"")]);
        let offset_pos = PFS0_HEADER_SIZE + 16;
        blob[offset_pos..offset_pos + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            Pfs0::read(&mut Cursor::new(blob)),
            Err(NxError::InvalidStringTable)
        ));
    }

    #[test]
    fn rejects_shared_names_exceeding_string_table() {
        let mut blob = write_pfs0_blob(&[("shared", b"".as_slice()); 8]);
        let table_pos = PFS0_HEADER_SIZE + 8 * PFS0_ENTRY_SIZE;
        blob[8..12].copy_from_slice(&16u32.to_le_bytes());
        blob.truncate(table_pos + 16);
        for i in 0..8 {
            let offset_pos = PFS0_HEADER_SIZE + i * PFS0_ENTRY_SIZE + 16;
            blob[offset_pos..offset_pos + 4].copy_from_slice(&0u32.to_le_bytes());
        }
        assert!(matches!(
            Pfs0::read(&mut Cursor::new(blob)),
            Err(NxError::InvalidStringTable)
        ));
    }

    #[test]
    fn detects_bad_magic() {
        let mut bad = vec![0u8; 0x10];
        bad[0..4].copy_from_slice(b"XYZW");
        let mut cur = Cursor::new(bad);
        let err = Pfs0::read(&mut cur).unwrap_err();
        assert!(matches!(err, NxError::Pfs0BadMagic));
    }
}
