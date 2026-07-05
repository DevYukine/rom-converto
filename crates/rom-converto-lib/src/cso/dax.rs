//! DAX (legacy PSP compressed ISO) reading. DAX is decode-only: it
//! joins CSO/ZSO as an accepted input for decompress, digest, and
//! to-chd. Frames are a fixed 0x2000 bytes; compressed frames are zlib
//! streams decoded by [`crate::cso::compression::BlockDecompressor::Dax`].
//!
//! On-disk layout (all little-endian):
//! ```text
//! 0x00  magic  "DAX\0"
//! 0x04  u32    uncompressed_size
//! 0x08  u32    version         (0 or 1)
//! 0x0C  u32    nc_area_count
//! 0x10  [u32; 4] reserved
//! 0x20  u32 offsets[nframes]   absolute file offsets
//!       u16 lengths[nframes]   stored byte length per frame
//!       nc_area_count * { u32 first_frame, u32 frame_count }  (version >= 1)
//! ```
//! `nframes = ceil(uncompressed_size / 0x2000)`. NC areas mark runs of
//! frames stored raw. The format embeds no checksums.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use crate::cso::error::{CsoError, CsoResult};
use crate::cso::models::{CisoHeader, CsoFormat, DAX_MAGIC};
use crate::cso::reader::{BlockSpec, CsoSyncHandle};

pub(crate) const DAX_FRAME_SIZE: u64 = 0x2000;
const DAX_HEADER_SIZE: usize = 0x20;

pub(crate) struct DaxTables {
    pub offsets: Vec<u32>,
    pub lengths: Vec<u16>,
    pub raw: Vec<bool>,
}

pub(crate) fn open_dax_sync(path: &Path, file_size: u64) -> CsoResult<CsoSyncHandle> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0u8; DAX_HEADER_SIZE];
    file.read_exact(&mut header)?;

    let uncompressed_size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
    let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let nc_area_count = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;

    if uncompressed_size == 0 {
        return Err(CsoError::InvalidHeader("empty DAX image".into()));
    }
    if version > 1 {
        return Err(CsoError::InvalidHeader(format!(
            "DAX version {version} not supported"
        )));
    }

    let nframes = uncompressed_size.div_ceil(DAX_FRAME_SIZE) as usize;

    let mut offset_bytes = vec![0u8; nframes * 4];
    file.read_exact(&mut offset_bytes)?;
    let offsets: Vec<u32> = offset_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let mut length_bytes = vec![0u8; nframes * 2];
    file.read_exact(&mut length_bytes)?;
    let lengths: Vec<u16> = length_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let mut raw = vec![false; nframes];
    // NC (non-compressed) areas are a version 1 addition; each entry
    // marks a run of frames stored uncompressed. Distinct areas cannot
    // outnumber frames, so a larger count is corruption and bounding it
    // here keeps a bogus header from driving a huge allocation.
    if version >= 1 {
        if nc_area_count > nframes {
            return Err(CsoError::CorruptIndex(
                "NC area count exceeds the frame count".into(),
            ));
        }
        let mut nc_bytes = vec![0u8; nc_area_count * 8];
        file.read_exact(&mut nc_bytes)?;
        for area in nc_bytes.chunks_exact(8) {
            let first = u32::from_le_bytes(area[0..4].try_into().unwrap()) as usize;
            let count = u32::from_le_bytes(area[4..8].try_into().unwrap()) as usize;
            let end = first
                .checked_add(count)
                .filter(|e| *e <= nframes)
                .ok_or_else(|| {
                    CsoError::CorruptIndex("NC area runs past the frame count".into())
                })?;
            raw[first..end].fill(true);
        }
    }

    let header = CisoHeader {
        magic: DAX_MAGIC,
        header_size: DAX_HEADER_SIZE as u32,
        uncompressed_size,
        block_size: DAX_FRAME_SIZE as u32,
        version: version as u8,
        index_shift: 0,
        reserved: [0; 2],
    };

    Ok(CsoSyncHandle {
        header,
        format: CsoFormat::Dax,
        index: Vec::new(),
        dax: Some(DaxTables {
            offsets,
            lengths,
            raw,
        }),
        file: Arc::new(std::fs::File::open(path)?),
        file_size,
    })
}

pub(crate) fn dax_block_spec(
    handle: &CsoSyncHandle,
    dax: &DaxTables,
    block: u64,
) -> CsoResult<BlockSpec> {
    let i = block as usize;
    let offset = dax.offsets[i] as u64;
    let raw = dax.raw[i];
    let logical_start = block * DAX_FRAME_SIZE;
    let expected_len =
        (handle.header.uncompressed_size - logical_start).min(DAX_FRAME_SIZE) as usize;
    // Raw frames hold exactly their logical bytes; compressed frames use
    // the stored zlib length. Either way the span must lie in the file.
    let stored_len = if raw {
        expected_len
    } else {
        dax.lengths[i] as usize
    };
    let end = offset + stored_len as u64;
    if end > handle.file_size {
        return Err(CsoError::CorruptIndex(format!(
            "frame {block} spans {offset:#X}..{end:#X} outside the file"
        )));
    }
    Ok(BlockSpec {
        offset,
        stored_len,
        raw,
        expected_len,
    })
}
