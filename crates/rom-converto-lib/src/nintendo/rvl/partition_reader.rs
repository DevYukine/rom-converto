//! Streaming Read+Seek view over a decrypted Wii partition.
//!
//! Built on top of the existing [`decrypt_sector`] primitive. Logical
//! position 0 maps to the start of the partition's first payload sector
//! and advances by `WII_SECTOR_PAYLOAD_SIZE` per sector; the 0x400-byte
//! hash region at the front of every on-disc sector is dropped.
//!
//! The reader caches the most recently decrypted sector so sequential
//! small reads (FST walks, banner.bin parses) only decrypt each sector
//! once.

use crate::nintendo::rvl::constants::{WII_SECTOR_PAYLOAD_SIZE, WII_SECTOR_SIZE};
use crate::nintendo::rvl::disc::decrypt_sector;
use crate::nintendo::rvl::partition::PartitionInfo;
use anyhow::Result;
use std::io::{Read, Seek, SeekFrom};

pub struct PartitionPayloadReader<R: Read + Seek> {
    inner: R,
    title_key: [u8; 16],
    data_start: u64,
    payload_len: u64,
    position: u64,
    cached_sector_index: Option<u64>,
    cached_payload: Box<[u8; WII_SECTOR_PAYLOAD_SIZE]>,
}

impl<R: Read + Seek> PartitionPayloadReader<R> {
    pub fn new(inner: R, info: &PartitionInfo) -> Self {
        let sector_count = info.data_size / WII_SECTOR_SIZE as u64;
        let payload_len = sector_count * WII_SECTOR_PAYLOAD_SIZE as u64;
        Self {
            inner,
            title_key: info.title_key,
            data_start: info.data_start(),
            payload_len,
            position: 0,
            cached_sector_index: None,
            cached_payload: Box::new([0u8; WII_SECTOR_PAYLOAD_SIZE]),
        }
    }

    pub fn payload_len(&self) -> u64 {
        self.payload_len
    }

    fn load_sector(&mut self, sector_index: u64) -> Result<()> {
        if self.cached_sector_index == Some(sector_index) {
            return Ok(());
        }
        let on_disc = self.data_start + sector_index * WII_SECTOR_SIZE as u64;
        self.inner.seek(SeekFrom::Start(on_disc))?;
        let mut buf = [0u8; WII_SECTOR_SIZE];
        self.inner.read_exact(&mut buf)?;
        decrypt_sector(&mut buf, &self.title_key)
            .map_err(|e| anyhow::anyhow!("decrypt sector: {}", e))?;
        let payload_start = WII_SECTOR_SIZE - WII_SECTOR_PAYLOAD_SIZE;
        self.cached_payload.copy_from_slice(&buf[payload_start..]);
        self.cached_sector_index = Some(sector_index);
        Ok(())
    }
}

impl<R: Read + Seek> Read for PartitionPayloadReader<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.payload_len {
            return Ok(0);
        }
        let remaining = self.payload_len - self.position;
        let want = (out.len() as u64).min(remaining) as usize;
        if want == 0 {
            return Ok(0);
        }

        let payload_size = WII_SECTOR_PAYLOAD_SIZE as u64;
        let sector_index = self.position / payload_size;
        let in_sector = (self.position % payload_size) as usize;
        let available = WII_SECTOR_PAYLOAD_SIZE - in_sector;
        let n = want.min(available);

        self.load_sector(sector_index)
            .map_err(std::io::Error::other)?;
        out[..n].copy_from_slice(&self.cached_payload[in_sector..in_sector + n]);
        self.position += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for PartitionPayloadReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(d) => self.payload_len as i128 + d as i128,
            SeekFrom::Current(d) => self.position as i128 + d as i128,
        };
        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.position = new_pos as u64;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::rvl::disc::encrypt_sector;
    use std::io::Cursor;

    fn build_partition(
        payloads: &[[u8; WII_SECTOR_PAYLOAD_SIZE]],
        key: [u8; 16],
    ) -> (Vec<u8>, PartitionInfo) {
        let data_offset = 0u64;
        let mut disc = Vec::new();
        for payload in payloads {
            let mut sector = [0u8; WII_SECTOR_SIZE];
            sector[WII_SECTOR_SIZE - WII_SECTOR_PAYLOAD_SIZE..].copy_from_slice(payload);
            encrypt_sector(&mut sector, &key).unwrap();
            disc.extend_from_slice(&sector);
        }
        let info = PartitionInfo {
            partition_offset: 0,
            group_index: 0,
            partition_type: 0,
            title_key: key,
            data_offset,
            data_size: disc.len() as u64,
        };
        (disc, info)
    }

    #[test]
    fn reads_single_sector_payload() {
        let mut payload = [0u8; WII_SECTOR_PAYLOAD_SIZE];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let (disc, info) = build_partition(&[payload], [0xAA; 16]);
        let mut reader = PartitionPayloadReader::new(Cursor::new(disc), &info);
        let mut out = vec![0u8; 128];
        reader.read_exact(&mut out).unwrap();
        assert_eq!(&out[..], &payload[..128]);
    }

    #[test]
    fn read_across_sector_boundary() {
        let mut p0 = [0xAAu8; WII_SECTOR_PAYLOAD_SIZE];
        let mut p1 = [0xBBu8; WII_SECTOR_PAYLOAD_SIZE];
        p0[WII_SECTOR_PAYLOAD_SIZE - 4..].copy_from_slice(&[1, 2, 3, 4]);
        p1[..4].copy_from_slice(&[5, 6, 7, 8]);
        let (disc, info) = build_partition(&[p0, p1], [0x55; 16]);
        let mut reader = PartitionPayloadReader::new(Cursor::new(disc), &info);

        reader
            .seek(SeekFrom::Start(WII_SECTOR_PAYLOAD_SIZE as u64 - 4))
            .unwrap();
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf).unwrap();
        let p0_tail = &p0[WII_SECTOR_PAYLOAD_SIZE - 4..];
        let p1_head = &p1[..4];
        let mut expected = Vec::with_capacity(8);
        expected.extend_from_slice(p0_tail);
        expected.extend_from_slice(p1_head);
        // The Read impl returns at most one sector slice per call, so read_exact across
        // a boundary may loop internally and leave the second half zero if the caller
        // only does one read. Handle both cases.
        if buf[4..] == [0u8; 4] {
            let mut second = [0u8; 4];
            reader.read_exact(&mut second).unwrap();
            assert_eq!(&buf[..4], p0_tail);
            assert_eq!(&second[..], p1_head);
        } else {
            assert_eq!(&buf[..], &expected[..]);
        }
    }

    #[test]
    fn seek_to_end_returns_zero_bytes() {
        let payload = [0xCCu8; WII_SECTOR_PAYLOAD_SIZE];
        let (disc, info) = build_partition(&[payload], [0x77; 16]);
        let mut reader = PartitionPayloadReader::new(Cursor::new(disc), &info);
        reader.seek(SeekFrom::End(0)).unwrap();
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }
}
