//! GM/UP/UC partition reader.
//!
//! Layout: plaintext 0x20-byte header, then a `headerSize`-byte H3/H4
//! hash-tree region (not needed for extraction), then the content
//! area. Content file offsets inside that area come from the
//! partition's own FST (content 0) for GM partitions, or from the SI
//! partition's FST for ticket/TMD files.
//!
//! See [`super`] for the on-disc byte map.

use crate::nintendo::wup::crypto::aes_cbc_decrypt_in_place;
use crate::nintendo::wup::disc::disc_key::DiscKey;
use crate::nintendo::wup::disc::sector_stream::{DiscSectorSource, SECTOR_SIZE};
use crate::nintendo::wup::error::{WupError, WupResult};
use crate::nintendo::wup::nus::content_stream::ContentBytesSource;

/// Signature at offset 0 of a (plaintext) partition header.
pub const PARTITION_HEADER_SIGNATURE: [u8; 4] = [0xCC, 0x93, 0xA4, 0xF5];

/// Fixed size of the plaintext partition header.
pub const PARTITION_HEADER_SIZE: usize = 0x20;

/// Parsed partition header fields that matter to the content reader.
#[derive(Clone, Copy, Debug)]
pub struct PartitionHeader {
    pub header_size: u32,
    pub fst_size: u32,
}

/// Reads and validates the plaintext header at `partition_byte_offset`.
///
/// # Errors
/// Returns [`WupError::InvalidPartitionHeader`] if the signature does not match.
pub fn read_partition_header(
    disc: &mut dyn DiscSectorSource,
    partition_byte_offset: u64,
) -> WupResult<PartitionHeader> {
    let mut sector = vec![0u8; SECTOR_SIZE];
    let sector_index = partition_byte_offset / SECTOR_SIZE as u64;
    disc.read_sector(sector_index, &mut sector)?;
    let inside = (partition_byte_offset % SECTOR_SIZE as u64) as usize;
    if inside + PARTITION_HEADER_SIZE > SECTOR_SIZE {
        return Err(WupError::InvalidPartitionHeader);
    }
    let hdr = &sector[inside..inside + PARTITION_HEADER_SIZE];
    if hdr[0..4] != PARTITION_HEADER_SIGNATURE {
        return Err(WupError::InvalidPartitionHeader);
    }
    let header_size = u32::from_be_bytes(hdr[0x04..0x08].try_into().expect("4-byte slice"));
    let fst_size = u32::from_be_bytes(hdr[0x14..0x18].try_into().expect("4-byte slice"));
    Ok(PartitionHeader {
        header_size,
        fst_size,
    })
}

/// Sector-aligned byte range describing where one content file lives
/// inside a GM partition.
#[derive(Clone, Copy, Debug)]
pub struct PartitionContentLocation {
    pub disc_byte_offset: u64,
    pub size: u64,
}

/// Computes the absolute disc byte range for a content file given its
/// FST-reported sector offset. `content_offset_sectors == 0` maps to
/// the start of the content area; otherwise the offset is
/// `(sector - 1) * SECTOR_SIZE` into the area.
pub fn compute_content_location(
    partition_byte_offset: u64,
    header_size: u64,
    content_offset_sectors: u64,
    content_size: u64,
) -> PartitionContentLocation {
    // offset within the content area = (offsetSector * 0x8000) - 0x8000
    // for nonzero sectors, 0 for sector 0. Clamp defensively.
    let within_content_area = if content_offset_sectors == 0 {
        0
    } else {
        content_offset_sectors
            .saturating_sub(1)
            .saturating_mul(SECTOR_SIZE as u64)
    };
    PartitionContentLocation {
        disc_byte_offset: partition_byte_offset + header_size + within_content_area,
        size: content_size,
    }
}

/// [`ContentBytesSource`] that reads content files directly out of a
/// disc partition using precomputed content locations.
pub struct PartitionContentSource<'d> {
    disc: &'d mut dyn DiscSectorSource,
    locations: Vec<(u32, PartitionContentLocation)>,
}

impl<'d> PartitionContentSource<'d> {
    /// Builds a source over `disc` using `locations` to resolve content ids.
    pub fn new(
        disc: &'d mut dyn DiscSectorSource,
        locations: Vec<(u32, PartitionContentLocation)>,
    ) -> Self {
        Self { disc, locations }
    }
}

impl<'d> ContentBytesSource for PartitionContentSource<'d> {
    fn encrypted_content_len(&mut self, content_id: u32) -> WupResult<u64> {
        self.locations
            .iter()
            .find(|(id, _)| *id == content_id)
            .map(|(_, location)| location.size)
            .ok_or(WupError::ContentNotFound { content_id })
    }

    fn read_encrypted_range(
        &mut self,
        content_id: u32,
        offset: u64,
        output: &mut [u8],
    ) -> WupResult<()> {
        let location = self
            .locations
            .iter()
            .find(|(id, _)| *id == content_id)
            .map(|(_, location)| *location)
            .ok_or(WupError::ContentNotFound { content_id })?;
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(WupError::InvalidFst)?;
        if end > location.size {
            return Err(WupError::InvalidFst);
        }
        self.disc
            .read_bytes(location.disc_byte_offset + offset, output)?;
        Ok(())
    }
}

/// Reads a bounded range from a zero-IV AES-CBC stream on disc.
pub fn read_disc_decrypted_zero_iv_range(
    disc: &mut dyn DiscSectorSource,
    key: &DiscKey,
    stream_offset: u64,
    stream_len: u64,
    byte_offset: u64,
    byte_len: usize,
) -> WupResult<Vec<u8>> {
    if byte_len == 0 {
        return Ok(Vec::new());
    }
    let end = byte_offset
        .checked_add(byte_len as u64)
        .ok_or(WupError::InvalidFst)?;
    let rounded_stream_len = stream_len.checked_add(15).ok_or(WupError::InvalidFst)? & !15;
    let aligned_start = byte_offset & !15;
    let aligned_end = end.checked_add(15).ok_or(WupError::InvalidFst)? & !15;
    if end > stream_len || aligned_end > rounded_stream_len {
        return Err(WupError::InvalidFst);
    }
    let mut iv = [0u8; 16];
    let source_start = stream_offset
        .checked_add(aligned_start)
        .ok_or(WupError::InvalidFst)?;
    if aligned_start > 0 {
        disc.read_bytes(source_start - 16, &mut iv)?;
    }
    let mut encrypted = vec![0u8; (aligned_end - aligned_start) as usize];
    disc.read_bytes(source_start, &mut encrypted)?;
    aes_cbc_decrypt_in_place(key.as_bytes(), &iv, &mut encrypted)?;
    let skip = (byte_offset - aligned_start) as usize;
    Ok(encrypted[skip..skip + byte_len].to_vec())
}

/// Read a file inside the SI partition's FST and decrypt it.
///
/// SI FST files (ticket, TMD, cert) use AES-CBC with the disc key
/// and an IV derived from the file's absolute disc byte offset: the
/// high 48 bits of `offset >> 16` go into the low 8 bytes of a
/// 16-byte IV, big-endian; the upper 8 bytes are zero.
pub fn read_disc_decrypted_file_iv(
    disc: &mut dyn DiscSectorSource,
    key: &DiscKey,
    offset: u64,
    len: usize,
) -> WupResult<Vec<u8>> {
    let aligned_len = len.next_multiple_of(16);
    let mut out = vec![0u8; aligned_len];
    disc.read_bytes(offset, &mut out)?;
    let iv = file_offset_iv(offset);
    aes_cbc_decrypt_in_place(key.as_bytes(), &iv, &mut out)?;
    out.truncate(len);
    Ok(out)
}

pub(crate) fn file_offset_iv(offset: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    let val = offset >> 16;
    iv[8..16].copy_from_slice(&val.to_be_bytes());
    iv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::wup::disc::sector_stream::InMemoryDisc;

    #[test]
    fn header_signature_matches_upstream_constant() {
        assert_eq!(PARTITION_HEADER_SIGNATURE, [0xCC, 0x93, 0xA4, 0xF5]);
    }

    #[test]
    fn read_partition_header_parses_fields() {
        let mut disc = vec![0u8; 4 * SECTOR_SIZE];
        // Plant a partition at sector 3 (byte offset 0x18000).
        let off = 3 * SECTOR_SIZE;
        disc[off..off + 4].copy_from_slice(&PARTITION_HEADER_SIGNATURE);
        disc[off + 0x04..off + 0x08].copy_from_slice(&0x0000_2000u32.to_be_bytes());
        disc[off + 0x14..off + 0x18].copy_from_slice(&0x0000_1234u32.to_be_bytes());
        let mut reader = InMemoryDisc::new(disc);
        let hdr = read_partition_header(&mut reader, 3 * SECTOR_SIZE as u64).unwrap();
        assert_eq!(hdr.header_size, 0x2000);
        assert_eq!(hdr.fst_size, 0x1234);
    }

    #[test]
    fn read_partition_header_rejects_bad_signature() {
        let mut disc = vec![0u8; 2 * SECTOR_SIZE];
        disc[SECTOR_SIZE..SECTOR_SIZE + 4].copy_from_slice(&[0xAA; 4]);
        let mut reader = InMemoryDisc::new(disc);
        let result = read_partition_header(&mut reader, SECTOR_SIZE as u64);
        assert!(matches!(result, Err(WupError::InvalidPartitionHeader)));
    }

    #[test]
    fn compute_location_handles_zero_sector() {
        let loc = compute_content_location(0x1000_0000, 0x2000, 0, 100);
        assert_eq!(loc.disc_byte_offset, 0x1000_0000 + 0x2000);
        assert_eq!(loc.size, 100);
    }

    #[test]
    fn compute_location_applies_minus_one_sector() {
        let loc = compute_content_location(0, 0, 3, 0);
        assert_eq!(loc.disc_byte_offset, 2 * SECTOR_SIZE as u64);
    }

    #[test]
    fn partition_source_reads_correct_bytes() {
        // Build a disc where one partition has two content files
        // planted at known offsets, then ask the source to retrieve
        // them by content id.
        let mut disc = vec![0u8; 8 * SECTOR_SIZE];
        let partition_off: u64 = 2 * SECTOR_SIZE as u64;
        let header_size: u64 = SECTOR_SIZE as u64;
        // Write "AAAA..." 256 bytes at partition_off + header_size.
        let content0_off = (partition_off + header_size) as usize;
        disc[content0_off..content0_off + 256].fill(0xAA);
        // Write "BBBB..." 128 bytes one sector further in.
        let content1_off = content0_off + SECTOR_SIZE;
        disc[content1_off..content1_off + 128].fill(0xBB);
        let mut reader = InMemoryDisc::new(disc);
        let locations = vec![
            (
                0x1111_1111,
                PartitionContentLocation {
                    disc_byte_offset: (partition_off + header_size),
                    size: 256,
                },
            ),
            (
                0x2222_2222,
                PartitionContentLocation {
                    disc_byte_offset: (partition_off + header_size + SECTOR_SIZE as u64),
                    size: 128,
                },
            ),
        ];
        let mut src = PartitionContentSource::new(&mut reader, locations);
        let mut a = vec![0; src.encrypted_content_len(0x1111_1111).unwrap() as usize];
        src.read_encrypted_range(0x1111_1111, 0, &mut a).unwrap();
        assert_eq!(a.len(), 256);
        assert!(a.iter().all(|&b| b == 0xAA));
        let mut b = vec![0; src.encrypted_content_len(0x2222_2222).unwrap() as usize];
        src.read_encrypted_range(0x2222_2222, 0, &mut b).unwrap();
        assert_eq!(b.len(), 128);
        assert!(b.iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn partition_source_content_not_found_for_unknown_id() {
        let mut reader = InMemoryDisc::new(vec![0u8; 2 * SECTOR_SIZE]);
        let mut src = PartitionContentSource::new(&mut reader, vec![]);
        let result = src.encrypted_content_len(0xDEAD_BEEF);
        assert!(matches!(
            result,
            Err(WupError::ContentNotFound {
                content_id: 0xDEAD_BEEF
            })
        ));
    }
}
