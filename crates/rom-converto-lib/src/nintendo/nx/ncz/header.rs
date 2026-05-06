//! NCZSECTN and NCZBLOCK headers. Layouts come straight from
//! `nicoboss/nsz/IndependentNczDecompressorConcise.py`.

use std::io::{Read, Seek, SeekFrom, Write};

use byteorder::{LE, ReadBytesExt, WriteBytesExt};

use crate::nintendo::nx::constants::{
    MAX_BLOCK_SIZE_EXP, MIN_BLOCK_SIZE_EXP, NCZBLOCK_MAGIC, NCZSECTN_MAGIC,
};
use crate::nintendo::nx::error::{NxError, NxResult};

#[derive(Debug, Clone, Copy)]
pub struct NczSectionEntry {
    pub offset: i64,
    pub size: i64,
    pub crypto_type: i64,
    pub crypto_key: [u8; 16],
    pub crypto_counter: [u8; 16],
}

#[derive(Debug, Clone)]
pub struct NczBlockInfo {
    pub version: u8,
    pub kind: u8,
    pub block_size_exp: u8,
    pub decompressed_size: i64,
    /// Per-block on-disk size. A block whose `compressed_block_sizes[i]
    /// == (1 << block_size_exp)` was stored raw because compression
    /// failed to shrink it; the decompressor passes those through.
    pub compressed_block_sizes: Vec<u32>,
}

impl NczBlockInfo {
    pub fn block_size_bytes(&self) -> u64 {
        1u64 << self.block_size_exp
    }
}

pub fn write_nczsectn<W: Write>(writer: &mut W, sections: &[NczSectionEntry]) -> NxResult<()> {
    writer.write_all(&NCZSECTN_MAGIC)?;
    writer.write_i64::<LE>(sections.len() as i64)?;
    for s in sections {
        writer.write_i64::<LE>(s.offset)?;
        writer.write_i64::<LE>(s.size)?;
        writer.write_i64::<LE>(s.crypto_type)?;
        writer.write_i64::<LE>(0)?;
        writer.write_all(&s.crypto_key)?;
        writer.write_all(&s.crypto_counter)?;
    }
    Ok(())
}

pub fn write_nczblock<W: Write>(writer: &mut W, info: &NczBlockInfo) -> NxResult<()> {
    if !(MIN_BLOCK_SIZE_EXP..=MAX_BLOCK_SIZE_EXP).contains(&info.block_size_exp) {
        return Err(NxError::BlockSizeOutOfRange(info.block_size_exp));
    }
    writer.write_all(&NCZBLOCK_MAGIC)?;
    writer.write_u8(info.version)?;
    writer.write_u8(info.kind)?;
    writer.write_u8(0)?;
    writer.write_u8(info.block_size_exp)?;
    writer.write_u32::<LE>(info.compressed_block_sizes.len() as u32)?;
    writer.write_i64::<LE>(info.decompressed_size)?;
    for s in &info.compressed_block_sizes {
        writer.write_u32::<LE>(*s)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ParsedHeaders {
    pub sections: Vec<NczSectionEntry>,
    pub block: Option<NczBlockInfo>,
    /// Position of the first payload byte (zstd frame start).
    pub payload_offset: u64,
}

/// Read both headers starting at the current reader position. After
/// the call the reader is positioned at the first payload byte.
pub fn read_headers<R: Read + Seek>(reader: &mut R) -> NxResult<ParsedHeaders> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if magic != NCZSECTN_MAGIC {
        return Err(NxError::NczBadMagic(magic));
    }
    let count = reader.read_i64::<LE>()?;
    if count < 0 {
        return Err(NxError::IncompleteSection);
    }
    let mut sections = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = reader.read_i64::<LE>()?;
        let size = reader.read_i64::<LE>()?;
        let crypto_type = reader.read_i64::<LE>()?;
        let _padding = reader.read_i64::<LE>()?;
        let mut crypto_key = [0u8; 16];
        reader.read_exact(&mut crypto_key)?;
        let mut crypto_counter = [0u8; 16];
        reader.read_exact(&mut crypto_counter)?;
        sections.push(NczSectionEntry {
            offset,
            size,
            crypto_type,
            crypto_key,
            crypto_counter,
        });
    }

    let pos_after_sectn = reader.stream_position()?;

    let mut maybe_block = [0u8; 8];
    let block = match reader.read_exact(&mut maybe_block) {
        Ok(()) if maybe_block == NCZBLOCK_MAGIC => {
            let version = reader.read_u8()?;
            let kind = reader.read_u8()?;
            let _u8 = reader.read_u8()?;
            let block_size_exp = reader.read_u8()?;
            if !(MIN_BLOCK_SIZE_EXP..=MAX_BLOCK_SIZE_EXP).contains(&block_size_exp) {
                return Err(NxError::BlockSizeOutOfRange(block_size_exp));
            }
            let num_blocks = reader.read_u32::<LE>()?;
            let decompressed_size = reader.read_i64::<LE>()?;
            let mut compressed_block_sizes = Vec::with_capacity(num_blocks as usize);
            for _ in 0..num_blocks {
                compressed_block_sizes.push(reader.read_u32::<LE>()?);
            }
            Some(NczBlockInfo {
                version,
                kind,
                block_size_exp,
                decompressed_size,
                compressed_block_sizes,
            })
        }
        _ => {
            reader.seek(SeekFrom::Start(pos_after_sectn))?;
            None
        }
    };
    let payload_offset = reader.stream_position()?;
    Ok(ParsedHeaders {
        sections,
        block,
        payload_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_sectn_only() {
        let entries = vec![NczSectionEntry {
            offset: 0xC00,
            size: 0x10000,
            crypto_type: 3,
            crypto_key: [0x55; 16],
            crypto_counter: [0xAA; 16],
        }];
        let mut blob = Vec::new();
        write_nczsectn(&mut blob, &entries).unwrap();
        let mut cur = Cursor::new(&blob);
        let parsed = read_headers(&mut cur).unwrap();
        assert_eq!(parsed.sections.len(), 1);
        assert!(parsed.block.is_none());
        assert_eq!(parsed.sections[0].offset, 0xC00);
        assert_eq!(parsed.sections[0].crypto_key[0], 0x55);
    }

    #[test]
    fn round_trip_sectn_and_block() {
        let entries = vec![NczSectionEntry {
            offset: 0,
            size: 0x100000,
            crypto_type: 1,
            crypto_key: [0; 16],
            crypto_counter: [0; 16],
        }];
        let block = NczBlockInfo {
            version: 1,
            kind: 0,
            block_size_exp: 20,
            decompressed_size: 0x100000,
            compressed_block_sizes: vec![0x40000, 0x30000, 0x20000, 0x10000],
        };
        let mut blob = Vec::new();
        write_nczsectn(&mut blob, &entries).unwrap();
        write_nczblock(&mut blob, &block).unwrap();
        let mut cur = Cursor::new(&blob);
        let parsed = read_headers(&mut cur).unwrap();
        assert_eq!(parsed.sections.len(), 1);
        let b = parsed.block.unwrap();
        assert_eq!(
            b.compressed_block_sizes,
            vec![0x40000, 0x30000, 0x20000, 0x10000]
        );
        assert_eq!(b.block_size_exp, 20);
    }
}
