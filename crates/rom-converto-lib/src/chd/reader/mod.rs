pub(crate) mod cue_generator;
pub(crate) mod parallel;

use crate::cd::IO_BUFFER_SIZE;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::map::{MapEntry, decompress_v5_map};
use crate::chd::models::{CHD_V5_HEADER_SIZE, ChdHeaderV5, ChdMetadataHeader, ChdVersion};
use binrw::BinRead;
use byteorder::{BigEndian, ByteOrder};
use std::io::Cursor;
use std::sync::Arc;

/// Synchronous opener used by the blocking extract / verify paths.
/// Reads the header, decompresses the map, and returns the raw
/// parts so the caller can spin up a worker pool and drive it from
/// a `spawn_blocking` context. This bypasses the tokio-backed
/// [`ChdReader`] which is still used by the remaining async read
/// helpers and the legacy serial extract path.
pub(crate) struct SyncChdHandle {
    pub header: ChdHeaderV5,
    pub map: Vec<MapEntry>,
    pub metadata: Vec<ChdMetadataHeader>,
    pub file: Arc<std::fs::File>,
}

pub(crate) fn open_chd_sync(path: &std::path::Path) -> ChdResult<SyncChdHandle> {
    use std::io::{BufReader as StdBufReader, Read, Seek};

    let file = std::fs::File::open(path)?;
    let mut reader = StdBufReader::with_capacity(IO_BUFFER_SIZE, file);

    let mut header_bytes = vec![0u8; CHD_V5_HEADER_SIZE as usize];
    reader.read_exact(&mut header_bytes)?;
    let mut cursor = Cursor::new(&header_bytes);
    let header = ChdHeaderV5::read(&mut cursor)?;

    if header.version != ChdVersion::V5 {
        return Err(ChdError::UnsupportedChdVersion);
    }

    // Seek to map offset and read to end.
    reader.seek(std::io::SeekFrom::Start(header.map_offset))?;
    let mut map_data = Vec::new();
    reader.read_to_end(&mut map_data)?;

    let hunk_count = header.logical_bytes.div_ceil(header.hunk_bytes as u64) as u32;
    let map = decompress_v5_map(&map_data, hunk_count, header.hunk_bytes, header.unit_bytes)?;

    // Walk metadata starting at header.meta_offset.
    let mut metadata = Vec::new();
    let mut offset = header.meta_offset;
    while offset != 0 {
        reader.seek(std::io::SeekFrom::Start(offset))?;
        let mut head_buf = [0u8; 16];
        reader.read_exact(&mut head_buf)?;

        let tag: [u8; 4] = head_buf[0..4].try_into().unwrap();
        let flags = head_buf[4];
        let length =
            ((head_buf[5] as u32) << 16) | ((head_buf[6] as u32) << 8) | (head_buf[7] as u32);
        let reserved: [u8; 8] = head_buf[8..16].try_into().unwrap();

        let mut data = vec![0u8; length as usize];
        reader.read_exact(&mut data)?;

        metadata.push(ChdMetadataHeader {
            tag,
            flags,
            reserved,
            data,
        });

        let next_offset = BigEndian::read_u64(&reserved);
        offset = if next_offset != 0 { next_offset } else { 0 };
    }

    // A second handle for the worker pool's positional reads. The
    // first handle keeps its sequential position from the
    // metadata walk and can be dropped here.
    drop(reader);
    let data_file = Arc::new(std::fs::File::open(path)?);

    Ok(SyncChdHandle {
        header,
        map,
        metadata,
        file: data_file,
    })
}
