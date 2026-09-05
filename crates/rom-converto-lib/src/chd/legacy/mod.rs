//! Reader for the pre-V5 CHD formats (v1 through v4).
//!
//! chdman has written V5 for over a decade, but MAME sets and old disc
//! dumps still carry the earlier layouts. Only the read side lives here:
//! the header, the flat map, and hunk decode, which is enough to stream a
//! legacy image back out as raw bytes.

use crate::cd::FRAME_SIZE;
use crate::chd::compression::avhuff;
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::models::{
    CHD_METADATA_HEADER_BYTES, CHD_METADATA_TAG_HARD_DISK, ChdMetadataHeader, ChdVersion,
};
use byteorder::{BigEndian, ByteOrder};
use crc::{CRC_32_ISO_HDLC, Crc};
use flate2::read::DeflateDecoder;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const CHD_MAGIC: &[u8; 8] = b"MComprHD";
/// On-disk header length per format version, indexed by `version - 1`.
const HEADER_SIZES: [u32; 5] = [76, 80, 120, 108, 124];
const CHDFLAGS_HAS_PARENT: u32 = 0x0000_0001;
const CHDFLAGS_UNDEFINED: u32 = 0xffff_fffc;
const MAX_HUNK_BYTES: u32 = 65536 * 256;
/// V1 predates the per-sector length field and is always 512 bytes.
const V1_SECTOR_BYTES: u32 = 512;

/// `CHDCOMPRESSION_AV`, the v1-v4 A/V (avhuff) compressor.
pub(crate) const LEGACY_COMPRESSION_AV: u32 = 3;

const MAP_ENTRY_FLAG_TYPE_MASK: u8 = 0x0f;
const MAP_ENTRY_FLAG_NO_CRC: u8 = 0x10;
const ENTRY_TYPE_COMPRESSED: u8 = 1;
const ENTRY_TYPE_UNCOMPRESSED: u8 = 2;
const ENTRY_TYPE_MINI: u8 = 3;
const ENTRY_TYPE_SELF_HUNK: u8 = 4;

/// Tags that mark a CD or GD-ROM image, whose unit is one 2448-byte frame.
const CD_METADATA_TAGS: [&[u8; 4]; 5] = [b"CHCD", b"CHTR", b"CHT2", b"CHGT", b"CHGD"];

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

/// Hard disk geometry as a v1/v2 header spells it out.
pub(crate) struct LegacyChs {
    pub cylinders: u32,
    pub heads: u32,
    pub sectors: u32,
    pub sector_bytes: u32,
}

/// A parsed v1-v4 header, normalized to the fields the V5 reader also uses.
pub(crate) struct LegacyChdHeader {
    pub version: ChdVersion,
    pub length: u32,
    /// Raw v1-v4 `compression` field (0 none, 1 zlib, 2 zlib+, 3 A/V).
    pub compression: u32,
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    pub total_hunks: u32,
    pub logical_bytes: u64,
    pub meta_offset: u64,
    pub md5: Option<[u8; 16]>,
    pub parent_md5: Option<[u8; 16]>,
    /// v3: SHA-1 of the raw data. v4: combined raw+metadata SHA-1.
    pub sha1: Option<[u8; 20]>,
    /// v4 only; v3 carries the raw SHA-1 in `sha1`.
    pub raw_sha1: Option<[u8; 20]>,
    pub parent_sha1: Option<[u8; 20]>,
    /// v1/v2 hard disk geometry from the header.
    pub chs: Option<LegacyChs>,
}

#[derive(Clone, Copy)]
struct LegacyMapEntry {
    offset: u64,
    crc: u32,
    length: u32,
    flags: u8,
}

/// An opened v1-v4 CHD: header, metadata chain, and the flat hunk map.
pub(crate) struct LegacyChd {
    file: File,
    header: LegacyChdHeader,
    metadata: Vec<ChdMetadataHeader>,
    map: Vec<LegacyMapEntry>,
}

impl LegacyChd {
    /// Opens a v1-v4 CHD and reads its header, metadata chain, and map.
    pub(crate) fn open(path: &Path) -> ChdResult<Self> {
        let mut file = File::open(path)?;
        let mut header = read_header(&mut file)?;
        let metadata = read_metadata(&mut file, header.meta_offset)?;

        // libchdr synthesizes a GDDD entry for v1/v2, so their unit is the
        // header's sector size rather than a guess from the (empty) chain.
        header.unit_bytes = match &header.chs {
            Some(chs) => chs.sector_bytes,
            None => guess_unit_bytes(header.hunk_bytes, &metadata),
        };
        if header.unit_bytes == 0 {
            return Err(invalid(
                header.version,
                "metadata declares a zero-byte unit size",
            ));
        }

        let map = read_map(&mut file, &header)?;
        Ok(Self {
            file,
            header,
            metadata,
            map,
        })
    }

    /// The parsed header.
    pub(crate) fn header(&self) -> &LegacyChdHeader {
        &self.header
    }

    /// The metadata chain, empty for v1/v2 which carry none.
    pub(crate) fn metadata(&self) -> &[ChdMetadataHeader] {
        &self.metadata
    }

    /// Decodes one hunk. `dest.len()` must equal `header.hunk_bytes`.
    pub(crate) fn read_hunk(&mut self, index: u32, dest: &mut [u8]) -> ChdResult<()> {
        if dest.len() != self.header.hunk_bytes as usize {
            return Err(ChdError::DecompressionSizeMismatch {
                expected: self.header.hunk_bytes as usize,
                actual: dest.len(),
            });
        }

        let hunk = self.resolve_self_hunks(index)?;
        let entry = self.map[hunk as usize];
        match entry.flags & MAP_ENTRY_FLAG_TYPE_MASK {
            ENTRY_TYPE_COMPRESSED => {
                let mut packed = vec![0u8; entry.length as usize];
                self.read_at(entry.offset, &mut packed)?;
                match self.header.compression {
                    1 | 2 => {
                        let mut decoder = DeflateDecoder::new(packed.as_slice());
                        let mut filled = fill(&mut decoder, dest)?;
                        // libchdr demands the stream end exactly at the
                        // hunk; v1/v2 have no CRC to catch an overrun.
                        if filled == dest.len() {
                            filled += decoder.read(&mut [0u8; 1])?;
                        }
                        if filled != dest.len() {
                            return Err(ChdError::DecompressionSizeMismatch {
                                expected: dest.len(),
                                actual: filled,
                            });
                        }
                    }
                    LEGACY_COMPRESSION_AV => {
                        dest.copy_from_slice(&avhuff::decode(&packed, dest.len())?)
                    }
                    // CHDCOMPRESSION_NONE cannot legally back a compressed entry.
                    _ => return Err(ChdError::UnsupportedLegacyMapEntry(ENTRY_TYPE_COMPRESSED)),
                }
            }
            ENTRY_TYPE_UNCOMPRESSED => self.read_at(entry.offset, dest)?,
            ENTRY_TYPE_MINI => {
                let pattern = entry.offset.to_be_bytes();
                for (i, byte) in dest.iter_mut().enumerate() {
                    *byte = pattern[i % pattern.len()];
                }
            }
            other => return Err(ChdError::UnsupportedLegacyMapEntry(other)),
        }

        if entry.flags & MAP_ENTRY_FLAG_NO_CRC == 0 {
            let actual = CRC32.checksum(dest);
            if actual != entry.crc {
                return Err(ChdError::LegacyHunkCrcMismatch {
                    hunk,
                    expected: entry.crc,
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Wraps this CHD in a sequential reader over its decoded raw data.
    pub(crate) fn into_raw_reader(self) -> LegacyRawReader {
        let hunk = vec![0u8; self.header.hunk_bytes as usize];
        LegacyRawReader {
            remaining: self.header.logical_bytes,
            // Start drained so the first read decodes hunk 0.
            hunk_pos: hunk.len(),
            hunk_index: 0,
            hunk,
            chd: self,
        }
    }

    /// Follows a chain of SELF_HUNK entries to the hunk that holds the data.
    /// Each hop must point strictly backwards and the chain is length capped,
    /// so a crafted map can neither loop nor make decoding quadratic.
    fn resolve_self_hunks(&self, index: u32) -> ChdResult<u32> {
        let mut hunk = index;
        for _ in 0..=MAX_SELF_HUNK_HOPS {
            let entry = self
                .map
                .get(hunk as usize)
                .ok_or_else(|| io::Error::other(format!("CHD hunk {hunk} is out of range")))?;
            if entry.flags & MAP_ENTRY_FLAG_TYPE_MASK != ENTRY_TYPE_SELF_HUNK {
                return Ok(hunk);
            }
            let next = u32::try_from(entry.offset).unwrap_or(u32::MAX);
            if next >= hunk {
                return Err(ChdError::UnsupportedLegacyMapEntry(ENTRY_TYPE_SELF_HUNK));
            }
            hunk = next;
        }
        Err(ChdError::UnsupportedLegacyMapEntry(ENTRY_TYPE_SELF_HUNK))
    }

    fn read_at(&mut self, offset: u64, dest: &mut [u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(dest)
    }
}

/// Sequential reader over a legacy CHD's decoded raw data, decoding one
/// hunk at a time and stopping at `logical_bytes` (the last hunk is
/// usually only partly in use).
pub(crate) struct LegacyRawReader {
    chd: LegacyChd,
    hunk: Vec<u8>,
    hunk_index: u32,
    hunk_pos: usize,
    remaining: u64,
}

impl Read for LegacyRawReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || out.is_empty() {
            return Ok(0);
        }
        if self.hunk_pos == self.hunk.len() {
            self.chd
                .read_hunk(self.hunk_index, &mut self.hunk)
                .map_err(io::Error::other)?;
            self.hunk_index += 1;
            self.hunk_pos = 0;
        }

        let available = (self.hunk.len() - self.hunk_pos) as u64;
        let take = available.min(out.len() as u64).min(self.remaining) as usize;
        out[..take].copy_from_slice(&self.hunk[self.hunk_pos..self.hunk_pos + take]);
        self.hunk_pos += take;
        self.remaining -= take as u64;
        Ok(take)
    }
}

/// Reads the version word of a CHD without parsing the rest. Returns
/// `None` when the file is not a CHD at all.
pub(crate) fn peek_chd_version(path: &Path) -> io::Result<Option<u32>> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 16];
    if fill(&mut file, &mut head)? != head.len() || &head[0..8] != CHD_MAGIC {
        return Ok(None);
    }
    Ok(Some(BigEndian::read_u32(&head[12..16])))
}

/// Reads until `dest` is full or the source ends, returning the byte count.
/// `read_exact` will not do: a short decode has to surface as a size
/// mismatch rather than an I/O error.
fn fill(source: &mut impl Read, dest: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < dest.len() {
        match source.read(&mut dest[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(filled)
}

/// Longest SELF_HUNK chain accepted. chdman never writes one longer than a
/// single hop, and libchdr resolves them by recursion, so anything deep is
/// hostile rather than real.
const MAX_SELF_HUNK_HOPS: u32 = 64;

fn invalid(version: ChdVersion, reason: &str) -> ChdError {
    ChdError::InvalidLegacyHeader {
        version: version as u8,
        reason: reason.to_string(),
    }
}

fn read_header(file: &mut File) -> ChdResult<LegacyChdHeader> {
    let mut head = [0u8; 16];
    file.read_exact(&mut head)?;
    if &head[0..8] != CHD_MAGIC {
        return Err(ChdError::InvalidLegacyHeader {
            version: 0,
            reason: "missing MComprHD magic".to_string(),
        });
    }

    let version = match BigEndian::read_u32(&head[12..16]) {
        1 => ChdVersion::V1,
        2 => ChdVersion::V2,
        3 => ChdVersion::V3,
        4 => ChdVersion::V4,
        _ => return Err(ChdError::UnsupportedChdVersion),
    };
    let length = BigEndian::read_u32(&head[8..12]);
    let expected = HEADER_SIZES[version as usize - 1];
    if length != expected {
        return Err(invalid(
            version,
            &format!("header length {length}, expected {expected}"),
        ));
    }

    let mut raw = vec![0u8; length as usize];
    raw[..16].copy_from_slice(&head);
    file.read_exact(&mut raw[16..])?;

    let flags = BigEndian::read_u32(&raw[16..20]);
    let compression = BigEndian::read_u32(&raw[20..24]);
    let header = match version {
        ChdVersion::V1 | ChdVersion::V2 => {
            let sector_bytes = if version == ChdVersion::V1 {
                V1_SECTOR_BYTES
            } else {
                BigEndian::read_u32(&raw[76..80])
            };
            let hunk_sectors = BigEndian::read_u32(&raw[24..28]);
            let hunk_bytes = u64::from(sector_bytes) * u64::from(hunk_sectors);
            let hunk_bytes = u32::try_from(hunk_bytes)
                .map_err(|_| invalid(version, "hunk size overflows 32 bits"))?;
            let chs = LegacyChs {
                cylinders: BigEndian::read_u32(&raw[32..36]),
                heads: BigEndian::read_u32(&raw[36..40]),
                sectors: BigEndian::read_u32(&raw[40..44]),
                sector_bytes,
            };
            // Four unvalidated u32 geometry fields, so the product has to be
            // checked: it saturates u64 long before the file could be real.
            let logical_bytes = [chs.heads, chs.sectors, sector_bytes]
                .into_iter()
                .try_fold(u64::from(chs.cylinders), |acc, factor| {
                    acc.checked_mul(u64::from(factor))
                })
                .ok_or_else(|| invalid(version, "geometry overflows the logical size"))?;
            LegacyChdHeader {
                version,
                length,
                compression,
                hunk_bytes,
                unit_bytes: 0,
                total_hunks: BigEndian::read_u32(&raw[28..32]),
                logical_bytes,
                // V1/V2 predate the metadata chain.
                meta_offset: 0,
                md5: Some(fixed(&raw[44..60])),
                parent_md5: Some(fixed(&raw[60..76])),
                sha1: None,
                raw_sha1: None,
                parent_sha1: None,
                chs: Some(chs),
            }
        }
        ChdVersion::V3 => LegacyChdHeader {
            version,
            length,
            compression,
            hunk_bytes: BigEndian::read_u32(&raw[76..80]),
            unit_bytes: 0,
            total_hunks: BigEndian::read_u32(&raw[24..28]),
            logical_bytes: BigEndian::read_u64(&raw[28..36]),
            meta_offset: BigEndian::read_u64(&raw[36..44]),
            md5: Some(fixed(&raw[44..60])),
            parent_md5: Some(fixed(&raw[60..76])),
            sha1: Some(fixed(&raw[80..100])),
            raw_sha1: None,
            parent_sha1: Some(fixed(&raw[100..120])),
            chs: None,
        },
        _ => LegacyChdHeader {
            version,
            length,
            compression,
            hunk_bytes: BigEndian::read_u32(&raw[44..48]),
            unit_bytes: 0,
            total_hunks: BigEndian::read_u32(&raw[24..28]),
            logical_bytes: BigEndian::read_u64(&raw[28..36]),
            meta_offset: BigEndian::read_u64(&raw[36..44]),
            md5: None,
            parent_md5: None,
            sha1: Some(fixed(&raw[48..68])),
            raw_sha1: Some(fixed(&raw[88..108])),
            parent_sha1: Some(fixed(&raw[68..88])),
            chs: None,
        },
    };

    if flags & CHDFLAGS_UNDEFINED != 0 {
        return Err(invalid(version, "undefined header flags are set"));
    }
    if compression > 3 {
        return Err(invalid(
            version,
            &format!("unknown compression type {compression}"),
        ));
    }
    if header.hunk_bytes == 0 || header.hunk_bytes >= MAX_HUNK_BYTES {
        return Err(invalid(
            version,
            &format!("hunk size {} out of range", header.hunk_bytes),
        ));
    }
    if header.total_hunks == 0 {
        return Err(invalid(version, "no hunks"));
    }
    if flags & CHDFLAGS_HAS_PARENT != 0 {
        return Err(ChdError::ParentChdNotSupported);
    }
    Ok(header)
}

fn fixed<const N: usize>(bytes: &[u8]) -> [u8; N] {
    bytes
        .try_into()
        .expect("slice is sized by its literal range")
}

/// Walks the metadata chain the same way the V5 reader does.
fn read_metadata(file: &mut File, meta_offset: u64) -> ChdResult<Vec<ChdMetadataHeader>> {
    let mut metadata = Vec::new();
    let mut offset = meta_offset;
    while offset != 0 {
        file.seek(SeekFrom::Start(offset))?;
        let mut head_buf = [0u8; CHD_METADATA_HEADER_BYTES];
        file.read_exact(&mut head_buf)?;

        let length =
            ((head_buf[5] as u32) << 16) | ((head_buf[6] as u32) << 8) | (head_buf[7] as u32);
        let reserved: [u8; 8] = fixed(&head_buf[8..16]);
        let mut data = vec![0u8; length as usize];
        file.read_exact(&mut data)?;

        metadata.push(ChdMetadataHeader {
            tag: fixed(&head_buf[0..4]),
            flags: head_buf[4],
            reserved,
            data,
        });

        // Follow the chain only forward, and past the entry just read:
        // chdman lays entries out end to end, and a malformed next pointer
        // must not loop the walk or let overlapping entries pile up.
        let next_offset = BigEndian::read_u64(&reserved);
        let entry_end = offset + CHD_METADATA_HEADER_BYTES as u64 + u64::from(length);
        offset = if next_offset >= entry_end {
            next_offset
        } else {
            0
        };
    }
    Ok(metadata)
}

/// chdman's `header_guess_unitbytes`: the unit size is not stored before
/// V5, so it is inferred from the metadata the image carries.
fn guess_unit_bytes(hunk_bytes: u32, metadata: &[ChdMetadataHeader]) -> u32 {
    if let Some(bps) = metadata
        .iter()
        .find(|entry| entry.tag == CHD_METADATA_TAG_HARD_DISK)
        .and_then(|entry| parse_gddd(&entry.data).map(|(_, _, _, bps)| bps))
    {
        return bps;
    }
    if metadata
        .iter()
        .any(|entry| CD_METADATA_TAGS.contains(&&entry.tag))
    {
        return FRAME_SIZE as u32;
    }
    hunk_bytes
}

/// Parses a hard disk `CYLS:%d,HEADS:%d,SECS:%d,BPS:%d` entry into
/// `(cylinders, heads, sectors, sector_bytes)`.
pub(crate) fn parse_gddd(data: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let text = String::from_utf8_lossy(data);
    let mut fields = text.trim_end_matches('\0').trim().split(',');
    let mut field =
        |prefix: &str| -> Option<u32> { fields.next()?.trim().strip_prefix(prefix)?.parse().ok() };
    Some((
        field("CYLS:")?,
        field("HEADS:")?,
        field("SECS:")?,
        field("BPS:")?,
    ))
}

fn read_map(file: &mut File, header: &LegacyChdHeader) -> ChdResult<Vec<LegacyMapEntry>> {
    let entry_size = match header.version {
        ChdVersion::V1 | ChdVersion::V2 => 8,
        _ => 16,
    };
    let map_bytes = u64::from(header.total_hunks) * entry_size as u64;
    if u64::from(header.length) + map_bytes > file.metadata()?.len() {
        return Err(invalid(header.version, "map runs past the end of the file"));
    }

    let mut raw = vec![0u8; map_bytes as usize];
    file.seek(SeekFrom::Start(u64::from(header.length)))?;
    file.read_exact(&mut raw)?;

    Ok(raw
        .chunks_exact(entry_size)
        .map(|entry| {
            if entry_size == 8 {
                let packed = BigEndian::read_u64(entry);
                let length = (packed >> 44) as u32;
                LegacyMapEntry {
                    offset: packed & 0x0000_0FFF_FFFF_FFFF,
                    crc: 0,
                    length,
                    flags: MAP_ENTRY_FLAG_NO_CRC
                        | if length == header.hunk_bytes {
                            ENTRY_TYPE_UNCOMPRESSED
                        } else {
                            ENTRY_TYPE_COMPRESSED
                        },
                }
            } else {
                LegacyMapEntry {
                    offset: BigEndian::read_u64(&entry[0..8]),
                    crc: BigEndian::read_u32(&entry[8..12]),
                    length: u32::from(BigEndian::read_u16(&entry[12..14]))
                        | (u32::from(entry[14]) << 16),
                    flags: entry[15],
                }
            }
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;
    use tempfile::NamedTempFile;

    pub(crate) enum TestHunk {
        Plain(Vec<u8>),
        Deflate(Vec<u8>),
        Mini(u64),
        SelfRef(u32),
        BadCrc(Vec<u8>),
    }

    pub(crate) struct Fixture {
        pub(crate) version: u32,
        pub(crate) flags: u32,
        pub(crate) compression: u32,
        pub(crate) hunk_bytes: u32,
        /// v3/v4 only; v1/v2 derive the logical size from the geometry.
        pub(crate) logical_bytes: u64,
        /// cylinders, heads, sectors, sector bytes.
        pub(crate) chs: (u32, u32, u32, u32),
        pub(crate) metadata: Vec<([u8; 4], Vec<u8>)>,
        pub(crate) hunks: Vec<TestHunk>,
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                version: 3,
                flags: 0,
                compression: 1,
                hunk_bytes: 1024,
                logical_bytes: 2048,
                chs: (1, 1, 4, 512),
                metadata: Vec::new(),
                hunks: Vec::new(),
            }
        }
    }

    impl Fixture {
        pub(crate) fn image(&self) -> Vec<u8> {
            let hunk_bytes = self.hunk_bytes as usize;
            let entry_size = if self.version < 3 { 8 } else { 16 };
            let header_len = HEADER_SIZES[self.version as usize - 1] as usize;
            let mut image = vec![0u8; header_len + (self.hunks.len() + 1) * entry_size];

            let mut meta_offsets = Vec::new();
            for (tag, data) in &self.metadata {
                meta_offsets.push(image.len() as u64);
                image.extend_from_slice(tag);
                image.push(1);
                let len = data.len() as u32;
                image.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
                image.extend_from_slice(&[0u8; 8]);
                image.extend_from_slice(data);
            }
            for (i, &start) in meta_offsets.iter().enumerate() {
                let next = meta_offsets.get(i + 1).copied().unwrap_or(0);
                let at = start as usize + 8;
                image[at..at + 8].copy_from_slice(&next.to_be_bytes());
            }

            let mut entries = Vec::new();
            for hunk in &self.hunks {
                entries.push(match hunk {
                    TestHunk::Plain(data) | TestHunk::BadCrc(data) => {
                        let padded = pad(data, hunk_bytes);
                        let offset = image.len() as u64;
                        image.extend_from_slice(&padded);
                        let crc = CRC32.checksum(&padded);
                        let crc = match hunk {
                            TestHunk::BadCrc(_) => !crc,
                            _ => crc,
                        };
                        (offset, self.hunk_bytes, crc, ENTRY_TYPE_UNCOMPRESSED)
                    }
                    TestHunk::Deflate(data) => {
                        let padded = pad(data, hunk_bytes);
                        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                        encoder.write_all(&padded).expect("in-memory write");
                        let packed = encoder.finish().expect("in-memory flush");
                        let offset = image.len() as u64;
                        image.extend_from_slice(&packed);
                        (
                            offset,
                            packed.len() as u32,
                            CRC32.checksum(&padded),
                            ENTRY_TYPE_COMPRESSED,
                        )
                    }
                    TestHunk::Mini(pattern) => (
                        *pattern,
                        0,
                        CRC32.checksum(&mini_bytes(*pattern, hunk_bytes)),
                        ENTRY_TYPE_MINI,
                    ),
                    TestHunk::SelfRef(target) => (
                        u64::from(*target),
                        0,
                        0,
                        ENTRY_TYPE_SELF_HUNK | MAP_ENTRY_FLAG_NO_CRC,
                    ),
                });
            }

            let mut map = Vec::new();
            for (offset, length, crc, flags) in entries {
                if entry_size == 8 {
                    map.extend_from_slice(&(((length as u64) << 44) | offset).to_be_bytes());
                } else {
                    map.extend_from_slice(&offset.to_be_bytes());
                    map.extend_from_slice(&crc.to_be_bytes());
                    map.extend_from_slice(&(length as u16).to_be_bytes());
                    map.push((length >> 16) as u8);
                    map.push(flags);
                }
            }
            map.extend_from_slice(&b"EndOfListCookie\0"[..entry_size]);
            image[header_len..header_len + map.len()].copy_from_slice(&map);

            let (cylinders, heads, sectors, sector_bytes) = self.chs;
            let meta_offset = meta_offsets.first().copied().unwrap_or(0);
            image[0..8].copy_from_slice(CHD_MAGIC);
            image[8..12].copy_from_slice(&(header_len as u32).to_be_bytes());
            image[12..16].copy_from_slice(&self.version.to_be_bytes());
            image[16..20].copy_from_slice(&self.flags.to_be_bytes());
            image[20..24].copy_from_slice(&self.compression.to_be_bytes());
            match self.version {
                1 | 2 => {
                    image[24..28].copy_from_slice(&(self.hunk_bytes / sector_bytes).to_be_bytes());
                    image[28..32].copy_from_slice(&(self.hunks.len() as u32).to_be_bytes());
                    image[32..36].copy_from_slice(&cylinders.to_be_bytes());
                    image[36..40].copy_from_slice(&heads.to_be_bytes());
                    image[40..44].copy_from_slice(&sectors.to_be_bytes());
                    if self.version == 2 {
                        image[76..80].copy_from_slice(&sector_bytes.to_be_bytes());
                    }
                }
                3 => {
                    image[24..28].copy_from_slice(&(self.hunks.len() as u32).to_be_bytes());
                    image[28..36].copy_from_slice(&self.logical_bytes.to_be_bytes());
                    image[36..44].copy_from_slice(&meta_offset.to_be_bytes());
                    image[76..80].copy_from_slice(&self.hunk_bytes.to_be_bytes());
                }
                _ => {
                    image[24..28].copy_from_slice(&(self.hunks.len() as u32).to_be_bytes());
                    image[28..36].copy_from_slice(&self.logical_bytes.to_be_bytes());
                    image[36..44].copy_from_slice(&meta_offset.to_be_bytes());
                    image[44..48].copy_from_slice(&self.hunk_bytes.to_be_bytes());
                }
            }
            image
        }

        fn open(&self) -> (NamedTempFile, ChdResult<LegacyChd>) {
            let mut file = NamedTempFile::new().expect("temp file");
            file.write_all(&self.image()).expect("write fixture");
            file.flush().expect("flush fixture");
            let chd = LegacyChd::open(file.path());
            (file, chd)
        }

        fn opened(&self) -> (NamedTempFile, LegacyChd) {
            let (file, chd) = self.open();
            (file, chd.expect("fixture opens"))
        }
    }

    #[test]
    fn v1_geometry_that_overflows_the_logical_size_is_rejected() {
        let fixture = Fixture {
            version: 1,
            hunk_bytes: 1024,
            chs: (u32::MAX, u32::MAX, u32::MAX, 512),
            hunks: vec![TestHunk::Plain(pattern(3, 1024))],
            ..Fixture::default()
        };
        let (_file, chd) = fixture.open();
        assert!(matches!(
            chd,
            Err(ChdError::InvalidLegacyHeader { version: 1, .. })
        ));
    }

    fn pad(data: &[u8], len: usize) -> Vec<u8> {
        let mut padded = data.to_vec();
        padded.resize(len, 0);
        padded
    }

    fn pattern(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(seed).wrapping_add(seed))
            .collect()
    }

    fn mini_bytes(value: u64, len: usize) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        (0..len).map(|i| bytes[i % 8]).collect()
    }

    fn read_hunk(chd: &mut LegacyChd, index: u32) -> Vec<u8> {
        let mut dest = vec![0u8; chd.header().hunk_bytes as usize];
        chd.read_hunk(index, &mut dest).expect("hunk decodes");
        dest
    }

    #[test]
    fn v1_decodes_uncompressed_and_deflated_hunks() {
        let plain = pattern(3, 1024);
        let deflated = pattern(7, 1024);
        let fixture = Fixture {
            version: 1,
            hunk_bytes: 1024,
            chs: (1, 1, 4, 512),
            hunks: vec![
                TestHunk::Plain(plain.clone()),
                TestHunk::Deflate(deflated.clone()),
            ],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();

        // 1 * 1 * 4 * 512
        assert_eq!(chd.header().logical_bytes, 2048);
        assert_eq!(chd.header().hunk_bytes, 1024);
        assert_eq!(chd.header().unit_bytes, 512);
        let chs = chd.header().chs.as_ref().expect("v1 carries geometry");
        assert_eq!((chs.cylinders, chs.heads, chs.sectors), (1, 1, 4));
        assert_eq!(chs.sector_bytes, 512);
        assert!(chd.metadata().is_empty());

        assert_eq!(read_hunk(&mut chd, 0), plain);
        assert_eq!(read_hunk(&mut chd, 1), deflated);
    }

    #[test]
    fn v2_honours_a_non_512_sector_length() {
        let data = pattern(5, 4704);
        let fixture = Fixture {
            version: 2,
            compression: 0,
            hunk_bytes: 4704,
            chs: (1, 1, 4, 2352),
            hunks: vec![TestHunk::Plain(data.clone()), TestHunk::Plain(vec![0xAB])],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();

        assert_eq!(chd.header().logical_bytes, 9408);
        assert_eq!(
            chd.header()
                .chs
                .as_ref()
                .expect("v2 carries geometry")
                .sector_bytes,
            2352
        );
        assert_eq!(read_hunk(&mut chd, 0), data);
    }

    #[test]
    fn v3_walks_the_metadata_chain_and_guesses_cd_units() {
        let fixture = Fixture {
            version: 3,
            hunk_bytes: 2448,
            logical_bytes: 4896,
            metadata: vec![
                (
                    *b"CHT2",
                    b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:2 PREGAP:0 PGTYPE:MODE1_RAW \
                       PGSUB:NONE POSTGAP:0\0"
                        .to_vec(),
                ),
                (*b"VERS", b"0.264\0".to_vec()),
            ],
            hunks: vec![
                TestHunk::Deflate(pattern(11, 2448)),
                TestHunk::Plain(pattern(13, 2448)),
            ],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();

        assert_eq!(chd.metadata().len(), 2);
        assert_eq!(chd.metadata()[0].tag, *b"CHT2");
        assert_eq!(chd.metadata()[1].tag, *b"VERS");
        assert_eq!(chd.header().unit_bytes, 2448);
        assert_eq!(read_hunk(&mut chd, 0), pattern(11, 2448));
        assert_eq!(read_hunk(&mut chd, 1), pattern(13, 2448));
    }

    #[test]
    fn v4_takes_unit_bytes_from_gddd_metadata() {
        let fixture = Fixture {
            version: 4,
            hunk_bytes: 2048,
            logical_bytes: 4096,
            metadata: vec![(*b"GDDD", b"CYLS:2,HEADS:1,SECS:4,BPS:512\0".to_vec())],
            hunks: vec![
                TestHunk::Plain(pattern(17, 2048)),
                TestHunk::Plain(pattern(19, 2048)),
            ],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();

        assert_eq!(chd.header().unit_bytes, 512);
        assert_eq!(chd.header().hunk_bytes, 2048);
        assert_eq!(read_hunk(&mut chd, 1), pattern(19, 2048));
    }

    #[test]
    fn mini_entry_repeats_its_eight_byte_pattern() {
        let fixture = Fixture {
            hunk_bytes: 1024,
            hunks: vec![TestHunk::Mini(0x0102_0304_0506_0708)],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();

        assert_eq!(
            read_hunk(&mut chd, 0),
            mini_bytes(0x0102_0304_0506_0708, 1024)
        );
    }

    #[test]
    fn self_hunk_resolves_backwards() {
        let data = pattern(23, 1024);
        let fixture = Fixture {
            hunks: vec![TestHunk::Plain(data.clone()), TestHunk::SelfRef(0)],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();

        assert_eq!(read_hunk(&mut chd, 1), data);
    }

    #[test]
    fn forward_self_hunk_is_rejected() {
        let fixture = Fixture {
            hunks: vec![TestHunk::SelfRef(1), TestHunk::Plain(pattern(29, 1024))],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();
        let mut dest = vec![0u8; 1024];

        assert!(matches!(
            chd.read_hunk(0, &mut dest),
            Err(ChdError::UnsupportedLegacyMapEntry(4))
        ));
    }

    #[test]
    fn raw_reader_stops_at_logical_bytes() {
        let first = pattern(31, 1024);
        let second = pattern(37, 1024);
        let fixture = Fixture {
            hunk_bytes: 1024,
            logical_bytes: 1500,
            hunks: vec![
                TestHunk::Plain(first.clone()),
                TestHunk::Plain(second.clone()),
            ],
            ..Fixture::default()
        };
        let (_file, chd) = fixture.opened();

        let mut raw = Vec::new();
        let mut reader = chd.into_raw_reader();
        reader.read_to_end(&mut raw).expect("raw read");

        assert_eq!(raw.len(), 1500);
        assert_eq!(&raw[..1024], &first[..]);
        assert_eq!(&raw[1024..], &second[..476]);
        assert_eq!(reader.read(&mut [0u8; 16]).expect("eof read"), 0);
    }

    #[test]
    fn crc_mismatch_is_detected() {
        let fixture = Fixture {
            version: 4,
            hunks: vec![TestHunk::BadCrc(pattern(41, 1024))],
            ..Fixture::default()
        };
        let (_file, mut chd) = fixture.opened();
        let mut dest = vec![0u8; 1024];

        assert!(matches!(
            chd.read_hunk(0, &mut dest),
            Err(ChdError::LegacyHunkCrcMismatch { hunk: 0, .. })
        ));
    }

    #[test]
    fn a_parent_reference_is_rejected() {
        let fixture = Fixture {
            flags: CHDFLAGS_HAS_PARENT,
            hunks: vec![TestHunk::Plain(pattern(43, 1024))],
            ..Fixture::default()
        };
        let (_file, chd) = fixture.open();

        assert!(matches!(chd, Err(ChdError::ParentChdNotSupported)));
    }

    #[test]
    fn peek_reports_the_version_and_ignores_other_files() {
        let fixture = Fixture {
            version: 4,
            hunks: vec![TestHunk::Plain(pattern(47, 1024))],
            ..Fixture::default()
        };
        let (file, _chd) = fixture.opened();
        assert_eq!(peek_chd_version(file.path()).expect("peek"), Some(4));

        let mut other = NamedTempFile::new().expect("temp file");
        other.write_all(b"not a chd").expect("write");
        other.flush().expect("flush");
        assert_eq!(peek_chd_version(other.path()).expect("peek"), None);
    }
}
