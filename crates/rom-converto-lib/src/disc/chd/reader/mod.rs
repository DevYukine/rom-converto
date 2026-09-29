pub(crate) mod cue_generator;
pub(crate) mod worker;

use crate::disc::cd::IO_BUFFER_SIZE;
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::map::{MapEntry, decompress_v5_map};
use crate::disc::chd::models::{
    CHD_METADATA_FLAG_HASHED, CHD_METADATA_HEADER_BYTES, CHD_METADATA_TAG_AV,
    CHD_METADATA_TAG_AV_LD, CHD_METADATA_TAG_DVD, CHD_METADATA_TAG_HARD_DISK, CHD_V5_HEADER_SIZE,
    ChdHeaderV5, ChdMetadataHeader, ChdVersion,
};
use crate::disc::chd::writer::metadata::MetadataHash;
use crate::util::extent_end;
use binrw::BinRead;
use byteorder::{BigEndian, ByteOrder};
use sha1::{Digest, Sha1};
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
    pub metadata_tags: Vec<([u8; 4], u32)>,
    pub metadata_hashes: Vec<MetadataHash>,
    pub file: Arc<std::fs::File>,
}

/// What a CHD holds, as told by its metadata tags: CD tracks, a flat
/// DVD sector stream, or laserdisc A/V frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChdFlavor {
    Cd,
    Dvd,
    Ld,
}

impl SyncChdHandle {
    pub(crate) fn flavor(&self) -> ChdFlavor {
        let has = |tag| self.metadata.iter().any(|m| m.tag == tag);
        if has(CHD_METADATA_TAG_AV) {
            ChdFlavor::Ld
        } else if has(CHD_METADATA_TAG_DVD) {
            ChdFlavor::Dvd
        } else {
            ChdFlavor::Cd
        }
    }
}

/// The on-disk CHD format version of `path`, without parsing the rest of
/// the header. Callers use it to pick between the V5 and the v1-v4 reader.
pub(crate) fn chd_version(path: &std::path::Path) -> ChdResult<u32> {
    crate::disc::chd::legacy::peek_chd_version(path)?.ok_or(ChdError::UnsupportedChdVersion)
}

pub(crate) fn open_chd_sync(path: &std::path::Path) -> ChdResult<SyncChdHandle> {
    open_chd_sync_with_metadata_hash(path, false)
}

pub(crate) fn open_chd_sync_with_metadata_hash(
    path: &std::path::Path,
    hash_metadata: bool,
) -> ChdResult<SyncChdHandle> {
    use std::io::{BufReader as StdBufReader, Read, Seek, SeekFrom};

    let file = std::fs::File::open(path)?;
    let file_size = file.metadata()?.len();
    let mut reader = StdBufReader::with_capacity(IO_BUFFER_SIZE, file);

    let mut header_bytes = vec![0u8; CHD_V5_HEADER_SIZE as usize];
    reader.read_exact(&mut header_bytes)?;
    let mut cursor = Cursor::new(&header_bytes);
    let header = ChdHeaderV5::read(&mut cursor)?;

    if header.version != ChdVersion::V5 {
        return Err(ChdError::UnsupportedChdVersion);
    }
    if header.hunk_bytes == 0 {
        return Err(ChdError::InvalidHunkSize);
    }
    // chdman caps createcd/createdvd hunks at 1 MiB (chdman.cpp's
    // `constexpr uint32_t HUNK_SIZE_MAX = 1024 * 1024;`), but a v5
    // file migrated from a v1-v4 source inherits that source's hunk
    // size, which the legacy reader accepts up to its own
    // `MAX_HUNK_BYTES = 65536 * 256` (16 MiB, legacy.rs); the v5
    // reader accepts the same ceiling so migrated files still open.
    const CHD_HUNK_BYTES_MAX: u32 = 65536 * 256;
    if header.hunk_bytes > CHD_HUNK_BYTES_MAX {
        return Err(ChdError::InvalidHunkSize);
    }
    let hunk_count = u32::try_from(header.logical_bytes.div_ceil(header.hunk_bytes as u64))
        .map_err(|_| ChdError::MapDecompressionError)?;

    let map_header_end =
        extent_end(header.map_offset, 16, file_size).ok_or(ChdError::MapDecompressionError)?;
    reader.seek(SeekFrom::Start(header.map_offset))?;
    let mut map_header = [0u8; 16];
    reader.read_exact(&mut map_header)?;
    let compressed_len = BigEndian::read_u32(&map_header[..4]) as u64;
    let map_len_u64 = 16u64
        .checked_add(compressed_len)
        .ok_or(ChdError::MapDecompressionError)?;
    if extent_end(map_header_end, compressed_len, file_size).is_none() {
        return Err(ChdError::MapDecompressionError);
    }
    let map_len = usize::try_from(map_len_u64).map_err(|_| ChdError::MapDecompressionError)?;
    let mut map_data = Vec::new();
    map_data
        .try_reserve_exact(map_len)
        .map_err(|_| ChdError::MapDecompressionError)?;
    map_data.extend_from_slice(&map_header);
    map_data.resize(map_len, 0);
    reader.read_exact(&mut map_data[16..])?;
    let map = decompress_v5_map(&map_data, hunk_count, header.hunk_bytes, header.unit_bytes)?;

    let mut metadata = Vec::new();
    let mut metadata_tags = Vec::new();
    let mut metadata_hashes = Vec::new();
    let mut offset = header.meta_offset;
    while offset != 0 {
        let header_end = extent_end(offset, CHD_METADATA_HEADER_BYTES as u64, file_size)
            .ok_or_else(|| {
                ChdError::InvalidTrackMetadata("metadata header extends beyond the file".into())
            })?;
        reader.seek(std::io::SeekFrom::Start(offset))?;
        let mut head_buf = [0u8; CHD_METADATA_HEADER_BYTES];
        reader.read_exact(&mut head_buf)?;

        let tag: [u8; 4] = head_buf[0..4]
            .try_into()
            .expect("head_buf[0..4] is always 4 bytes");
        let flags = head_buf[4];
        let length =
            ((head_buf[5] as u32) << 16) | ((head_buf[6] as u32) << 8) | (head_buf[7] as u32);
        let reserved: [u8; 8] = head_buf[8..16]
            .try_into()
            .expect("head_buf[8..16] is always 8 bytes");
        if extent_end(header_end, u64::from(length), file_size).is_none() {
            return Err(ChdError::InvalidTrackMetadata(
                "metadata data extends beyond the file".into(),
            ));
        }
        metadata_tags.push((tag, length));

        // CHCD (old CD-ROM TOC) is binary and never matched by
        // `cd_track_metadata_text`, so it is hashed but not retained.
        let retained = matches!(&tag, b"CHT2" | b"CHTR" | b"CHGD" | b"CHGT" | b"VERS")
            || tag == CHD_METADATA_TAG_DVD
            || tag == CHD_METADATA_TAG_HARD_DISK
            || tag == CHD_METADATA_TAG_AV
            || tag == CHD_METADATA_TAG_AV_LD;
        let mut metadata_sha1 =
            (hash_metadata && flags & CHD_METADATA_FLAG_HASHED != 0).then(Sha1::new);
        if retained {
            let data_len = usize::try_from(length)
                .map_err(|_| ChdError::InvalidTrackMetadata("metadata is too large".into()))?;
            let mut data = Vec::new();
            data.try_reserve_exact(data_len)
                .map_err(|_| ChdError::InvalidTrackMetadata("metadata is too large".into()))?;
            data.resize(data_len, 0);
            reader.read_exact(&mut data)?;
            if let Some(hasher) = &mut metadata_sha1 {
                hasher.update(&data);
            }
            metadata.push(ChdMetadataHeader {
                tag,
                flags,
                reserved,
                data,
            });
        } else if let Some(hasher) = &mut metadata_sha1 {
            let mut remaining = length;
            let mut buffer = [0u8; 64 * 1024];
            while remaining != 0 {
                let take = remaining.min(buffer.len() as u32) as usize;
                reader.read_exact(&mut buffer[..take])?;
                hasher.update(&buffer[..take]);
                remaining -= take as u32;
            }
        } else {
            reader.seek(std::io::SeekFrom::Current(i64::from(length)))?;
        }
        if let Some(hasher) = metadata_sha1 {
            metadata_hashes.push(MetadataHash {
                tag,
                sha1: hasher.finalize().into(),
            });
        }

        // Follow the chain only forward: chdman writes entries in
        // ascending order, and a malformed next pointer must not
        // loop the walk forever.
        let next_offset = BigEndian::read_u64(&reserved);
        offset = if next_offset > offset { next_offset } else { 0 };
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
        metadata_tags,
        metadata_hashes,
        file: data_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::chd::map::{COMPRESSION_NONE, MapEntry, compress_v5_map};
    use crate::disc::chd::models::ChdVersion;
    use binrw::BinWrite;
    use std::io::{Cursor, Write};

    fn minimal_chd(path: &std::path::Path, logical_bytes: u64) -> Vec<u8> {
        let entries = if logical_bytes == 0 {
            Vec::new()
        } else {
            vec![MapEntry {
                compression: COMPRESSION_NONE,
                length: 2048,
                offset: 0,
                crc16: 0,
            }]
        };
        let map = compress_v5_map(&entries, 2048, 2048).unwrap();
        let header = ChdHeaderV5 {
            length: CHD_V5_HEADER_SIZE,
            version: ChdVersion::V5,
            compressor_0: [0; 4],
            compressor_1: [0; 4],
            compressor_2: [0; 4],
            compressor_3: [0; 4],
            logical_bytes,
            map_offset: CHD_V5_HEADER_SIZE as u64,
            meta_offset: 0,
            hunk_bytes: 2048,
            unit_bytes: 2048,
            raw_sha1: [0; 20],
            sha1: [0; 20],
            parent_sha1: [0; 20],
        };
        let mut bytes = Vec::new();
        header.write(&mut Cursor::new(&mut bytes)).unwrap();
        bytes.extend_from_slice(&map);
        std::fs::File::create(path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        bytes
    }

    #[test]
    fn open_reads_only_the_declared_map_before_sparse_hunk_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.chd");
        let bytes = minimal_chd(&path, 2048);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(bytes.len() as u64 + 256 * 1024 * 1024)
            .unwrap();

        let handle = open_chd_sync(&path).unwrap();
        assert_eq!(handle.map.len(), 1);
        assert_eq!(handle.header.logical_bytes, 2048);
    }

    #[test]
    fn opens_zero_logical_bytes_with_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.chd");
        minimal_chd(&path, 0);

        let handle = open_chd_sync(&path).unwrap();
        assert!(handle.map.is_empty());
        assert_eq!(handle.header.logical_bytes, 0);
    }

    #[test]
    fn rejects_truncated_declared_map_extent() {
        use std::io::{Seek, SeekFrom};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.chd");
        let bytes = minimal_chd(&path, 2048);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(CHD_V5_HEADER_SIZE as u64))
            .unwrap();
        file.write_all(&u32::MAX.to_be_bytes()).unwrap();
        file.set_len(bytes.len() as u64).unwrap();

        assert!(matches!(
            open_chd_sync(&path),
            Err(ChdError::MapDecompressionError)
        ));
    }
}
