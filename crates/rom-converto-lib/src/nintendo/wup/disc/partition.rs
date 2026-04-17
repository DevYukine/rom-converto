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
    /// Distance (bytes) from the start of the partition to the
    /// content area.
    pub header_size: u32,
    /// Size of the FST bundled with this partition, if any.
    pub fst_size: u32,
}

/// Read the plaintext 0x20-byte partition header and verify the
/// signature.
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
    let header_size = u32::from_be_bytes(hdr[0x04..0x08].try_into().unwrap());
    let fst_size = u32::from_be_bytes(hdr[0x14..0x18].try_into().unwrap());
    Ok(PartitionHeader {
        header_size,
        fst_size,
    })
}

/// Sector-aligned byte range describing where one content file lives
/// inside a GM partition.
#[derive(Clone, Copy, Debug)]
pub struct PartitionContentLocation {
    /// Absolute disc byte offset of the first byte of this content
    /// file. Sector-aligned.
    pub disc_byte_offset: u64,
    /// Size of this content file in bytes, from the TMD. Not
    /// necessarily sector-aligned on the high end.
    pub size: u64,
}

/// Turn a `ContentFSTInfo`-style `(offset_sectors, size)` pair into
/// the absolute disc byte offset of the content file. Callers build
/// a map from content id to this struct and hand it to the partition
/// source.
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

/// Reads encrypted content bytes from a GM/UP/UC partition on disc.
/// Construct with a populated `locations` map and then use as any
/// `ContentBytesSource`. The disc reader is shared via `&mut dyn`.
pub struct PartitionContentSource<'d> {
    disc: &'d mut dyn DiscSectorSource,
    locations: Vec<(u32, PartitionContentLocation)>,
}

impl<'d> PartitionContentSource<'d> {
    pub fn new(
        disc: &'d mut dyn DiscSectorSource,
        locations: Vec<(u32, PartitionContentLocation)>,
    ) -> Self {
        Self { disc, locations }
    }
}

impl<'d> ContentBytesSource for PartitionContentSource<'d> {
    fn read_encrypted_content(&mut self, content_id: u32) -> WupResult<Vec<u8>> {
        let loc = self
            .locations
            .iter()
            .find(|(id, _)| *id == content_id)
            .map(|(_, l)| *l)
            .ok_or(WupError::ContentNotFound { content_id })?;
        let mut out = vec![0u8; loc.size as usize];
        self.disc.read_bytes(loc.disc_byte_offset, &mut out)?;
        Ok(out)
    }
}

/// Read a raw byte range from disc and AES-CBC decrypt it with the
/// disc key using a zero IV. Used for the partition TOC and for the
/// SI FST. The byte range length must be a multiple of 16.
pub fn read_disc_decrypted_zero_iv(
    disc: &mut dyn DiscSectorSource,
    key: &DiscKey,
    offset: u64,
    len: usize,
) -> WupResult<Vec<u8>> {
    if !len.is_multiple_of(16) {
        return Err(WupError::AesError(format!(
            "disc AES-CBC read length {} is not a multiple of 16",
            len
        )));
    }
    let mut out = vec![0u8; len];
    disc.read_bytes(offset, &mut out)?;
    let iv = [0u8; 16];
    aes_cbc_decrypt_in_place(key.as_bytes(), &iv, &mut out)?;
    Ok(out)
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

/// IV derivation for SI FST file reads: `(offset >> 16)` placed as
/// big-endian u64 in the low 8 bytes of a 16 byte buffer, upper 8
/// bytes zero.
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
        let a = src.read_encrypted_content(0x1111_1111).unwrap();
        assert_eq!(a.len(), 256);
        assert!(a.iter().all(|&b| b == 0xAA));
        let b = src.read_encrypted_content(0x2222_2222).unwrap();
        assert_eq!(b.len(), 128);
        assert!(b.iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn partition_source_content_not_found_for_unknown_id() {
        let mut disc = vec![0u8; 2 * SECTOR_SIZE];
        let mut reader = InMemoryDisc::new(disc.clone());
        let _ = &mut disc;
        let mut src = PartitionContentSource::new(&mut reader, vec![]);
        let result = src.read_encrypted_content(0xDEAD_BEEF);
        assert!(matches!(
            result,
            Err(WupError::ContentNotFound {
                content_id: 0xDEAD_BEEF
            })
        ));
    }
}
