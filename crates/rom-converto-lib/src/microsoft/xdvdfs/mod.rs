//! XDVDFS: the disc filesystem shared by the original Xbox and Xbox 360.
//! Byte-identical volume descriptor and dirent layout across every disc
//! variant; only the partition base offset differs.

mod dirent;
mod error;

use std::io::{self, Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};

pub use dirent::{DirEntry, walk_dir_tables, walk_root_table};
pub use error::{XdvdfsError, XdvdfsResult};

/// Bytes per sector, used for every sector-relative address in the format.
pub const SECTOR_SIZE: u64 = 2048;

/// The XDVDFS volume descriptor magic, present at both the head and tail of
/// the descriptor sector.
pub const VOLUME_MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";

/// Sector holding the volume descriptor, relative to the partition base.
pub const VOLUME_DESCRIPTOR_SECTOR: u32 = 32;

/// Byte offset of the tail magic within the volume descriptor sector.
const TAIL_MAGIC_OFFSET: u64 = 0x7EC;

pub const ATTR_READONLY: u8 = 0x01;
pub const ATTR_HIDDEN: u8 = 0x02;
pub const ATTR_SYSTEM: u8 = 0x04;
pub const ATTR_DIRECTORY: u8 = 0x10;
pub const ATTR_ARCHIVE: u8 = 0x20;
pub const ATTR_NORMAL: u8 = 0x80;

/// Xbox 360 XGD3 (2011+) partition base offset.
const XGD3_BASE: u64 = 0x0208_0000;
/// Xbox 360 XGD2 (pre-2011) partition base offset.
const XGD2_BASE: u64 = 0x0FD9_0000;
/// Original Xbox / XGD1 partition base offset.
const XGD1_BASE: u64 = 0x1830_0000;

/// Original Xbox probe order: trimmed XISO first (so an already-trimmed
/// image is recognized before any of the full-image bases), then XGD2, XGD3,
/// XGD1.
pub const XBOX_PROBE_BASES: [u64; 4] = [0x0, XGD2_BASE, XGD3_BASE, XGD1_BASE];

/// Xbox 360 probe bases: the four Xbox bases plus two extra offsets Xenia
/// probes with no established XGD name.
pub const X360_PROBE_BASES: [u64; 6] = [
    0x0,
    0x0000_FB20,
    0x0002_0600,
    XGD3_BASE,
    XGD2_BASE,
    XGD1_BASE,
];

/// Which disc/partition layout a probed base corresponds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionKind {
    /// Already-trimmed XISO, or a base-0 game partition.
    Trimmed,
    Xgd1,
    Xgd2,
    Xgd3,
    /// One of the two extra Xenia-only probe bases, carried by value since
    /// they have no established XGD name.
    X360Extra(u64),
}

impl PartitionKind {
    fn from_base(base: u64) -> Self {
        match base {
            0x0 => PartitionKind::Trimmed,
            XGD2_BASE => PartitionKind::Xgd2,
            XGD3_BASE => PartitionKind::Xgd3,
            XGD1_BASE => PartitionKind::Xgd1,
            other => PartitionKind::X360Extra(other),
        }
    }
}

/// A probed XDVDFS volume: base offset plus the parsed volume descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdvdfsVolume {
    pub base: u64,
    pub root_sector: u32,
    pub root_size: u32,
    pub filetime: u64,
    pub kind: PartitionKind,
}

impl XdvdfsVolume {
    /// Probes `bases` in order, reading the volume magic at
    /// `base + 0x10000`. The first match wins; its tail magic at
    /// `base + 0x107EC` is then verified.
    pub fn probe<R: Read + Seek>(reader: &mut R, bases: &[u64]) -> XdvdfsResult<Self> {
        for &base in bases {
            let descriptor_offset = base + VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE;
            reader.seek(SeekFrom::Start(descriptor_offset))?;

            let mut magic = [0u8; 20];
            match reader.read_exact(&mut magic) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => continue,
                Err(e) => return Err(e.into()),
            }
            if &magic != VOLUME_MAGIC {
                continue;
            }

            let mut fields = [0u8; 16];
            reader.read_exact(&mut fields)?;
            let root_sector = u32::from_le_bytes(fields[0..4].try_into().unwrap());
            let root_size = u32::from_le_bytes(fields[4..8].try_into().unwrap());
            let filetime = u64::from_le_bytes(fields[8..16].try_into().unwrap());

            reader.seek(SeekFrom::Start(descriptor_offset + TAIL_MAGIC_OFFSET))?;
            let mut tail = [0u8; 20];
            reader.read_exact(&mut tail)?;
            if &tail != VOLUME_MAGIC {
                return Err(XdvdfsError::TailMagicMismatch { base });
            }

            return Ok(Self {
                base,
                root_sector,
                root_size,
                filetime,
                kind: PartitionKind::from_base(base),
            });
        }

        Err(XdvdfsError::NoVolumeDescriptor)
    }
}

/// Absolute file offset of `sector`, relative to `volume`'s partition base.
pub fn data_offset(volume: &XdvdfsVolume, sector: u32) -> u64 {
    volume.base + sector as u64 * SECTOR_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builds a full 0x800-byte volume descriptor sector.
    fn build_descriptor(root_sector: u32, root_size: u32) -> Vec<u8> {
        let mut d = vec![0u8; 0x800];
        d[0..20].copy_from_slice(VOLUME_MAGIC);
        d[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
        d[0x1C..0x24].copy_from_slice(&0u64.to_le_bytes());
        d[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
        d
    }

    /// Synthetic disk backed by sparse regions, so tests can probe real
    /// XDVDFS base offsets (up to ~400 MB) without allocating a real image.
    struct SparseDisk {
        regions: Vec<(u64, Vec<u8>)>,
        pos: u64,
        len: u64,
    }

    impl SparseDisk {
        fn new() -> Self {
            Self {
                regions: Vec::new(),
                pos: 0,
                len: 0,
            }
        }

        fn put(&mut self, offset: u64, bytes: Vec<u8>) {
            self.len = self.len.max(offset + bytes.len() as u64);
            self.regions.push((offset, bytes));
        }
    }

    impl Read for SparseDisk {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.len {
                return Ok(0);
            }
            let n = buf.len().min((self.len - self.pos) as usize);
            for (i, b) in buf[..n].iter_mut().enumerate() {
                let abs = self.pos + i as u64;
                *b = self
                    .regions
                    .iter()
                    .find_map(|(start, data)| {
                        if abs >= *start && abs < *start + data.len() as u64 {
                            Some(data[(abs - start) as usize])
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
            }
            self.pos += n as u64;
            Ok(n)
        }
    }

    impl Seek for SparseDisk {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let new_pos = match pos {
                SeekFrom::Start(p) => p as i64,
                SeekFrom::End(p) => self.len as i64 + p,
                SeekFrom::Current(p) => self.pos as i64 + p,
            };
            if new_pos < 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
            }
            self.pos = new_pos as u64;
            Ok(self.pos)
        }
    }

    fn expected_kind(base: u64) -> PartitionKind {
        match base {
            0x0 => PartitionKind::Trimmed,
            XGD2_BASE => PartitionKind::Xgd2,
            XGD3_BASE => PartitionKind::Xgd3,
            XGD1_BASE => PartitionKind::Xgd1,
            other => PartitionKind::X360Extra(other),
        }
    }

    #[test]
    fn probe_finds_every_xbox_and_x360_base() {
        for bases in [&XBOX_PROBE_BASES[..], &X360_PROBE_BASES[..]] {
            for &target in bases {
                let mut disk = SparseDisk::new();
                disk.put(
                    target + VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
                    build_descriptor(40, 2048),
                );

                let volume = XdvdfsVolume::probe(&mut disk, bases).unwrap();
                assert_eq!(volume.base, target);
                assert_eq!(volume.root_sector, 40);
                assert_eq!(volume.root_size, 2048);
                assert_eq!(volume.kind, expected_kind(target));
            }
        }
    }

    #[test]
    fn probe_prefers_trimmed_base_over_garbage_at_other_bases() {
        let mut disk = SparseDisk::new();
        disk.put(
            VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
            build_descriptor(40, 2048),
        );
        // Garbage (non-matching) bytes at another probed base's descriptor
        // location must not be mistaken for a match.
        disk.put(
            XGD1_BASE + VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE,
            vec![0x41u8; 0x800],
        );

        let volume = XdvdfsVolume::probe(&mut disk, &XBOX_PROBE_BASES).unwrap();
        assert_eq!(volume.base, 0);
        assert_eq!(volume.kind, PartitionKind::Trimmed);
    }

    #[test]
    fn probe_errors_on_tail_magic_mismatch() {
        let mut descriptor = build_descriptor(40, 2048);
        descriptor[0x7FF] = 0x00; // corrupt one byte of the tail magic
        let mut disk = SparseDisk::new();
        disk.put(VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE, descriptor);

        let err = XdvdfsVolume::probe(&mut disk, &[0x0]).unwrap_err();
        assert!(matches!(err, XdvdfsError::TailMagicMismatch { base: 0 }));
    }

    fn encode_dirent(
        left: u16,
        right: u16,
        start_sector: u32,
        size: u32,
        attrs: u8,
        name: &[u8],
    ) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + name.len());
        e.extend_from_slice(&left.to_le_bytes());
        e.extend_from_slice(&right.to_le_bytes());
        e.extend_from_slice(&start_sector.to_le_bytes());
        e.extend_from_slice(&size.to_le_bytes());
        e.push(attrs);
        e.push(name.len() as u8);
        e.extend_from_slice(name);
        e
    }

    #[test]
    fn walk_emits_all_entries_with_parent_paths_and_a_bumped_entry() {
        let root_sector = 40u32;
        let subdir_sector = 41u32;
        let subdir_size = 4096u32; // 2 sectors

        // Subdir dirtab: "AAA.BIN" at offset 0, "ZZZ.BIN" bumped to the next
        // sector (offset 2048) because it wouldn't otherwise fit before the
        // boundary; the gap in between is 0xFF padding.
        let mut subdir = vec![0xFFu8; subdir_size as usize];
        let aaa = encode_dirent(0, 512, 100, 10, 0, b"AAA.BIN");
        subdir[0..aaa.len()].copy_from_slice(&aaa);
        let zzz = encode_dirent(0, 0, 200, 20, 0, b"ZZZ.BIN");
        subdir[2048..2048 + zzz.len()].copy_from_slice(&zzz);

        // Root dirtab: one directory entry "SUBDIR".
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let subdir_entry =
            encode_dirent(0, 0, subdir_sector, subdir_size, ATTR_DIRECTORY, b"SUBDIR");
        root[0..subdir_entry.len()].copy_from_slice(&subdir_entry);

        let mut image = vec![0u8; ((subdir_sector as u64 + 2) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);
        let subdir_off = (subdir_sector as u64 * SECTOR_SIZE) as usize;
        image[subdir_off..subdir_off + subdir.len()].copy_from_slice(&subdir);

        let mut cursor = Cursor::new(image);
        let volume = XdvdfsVolume::probe(&mut cursor, &[0x0]).unwrap();

        let mut seen: Vec<(Vec<String>, String)> = Vec::new();
        walk_dir_tables(&mut cursor, &volume, |path, entry| {
            seen.push((path.to_vec(), entry.name_str()));
            Ok(())
        })
        .unwrap();

        seen.sort();
        assert_eq!(
            seen,
            vec![
                (vec![], "SUBDIR".to_string()),
                (vec!["SUBDIR".to_string()], "AAA.BIN".to_string()),
                (vec!["SUBDIR".to_string()], "ZZZ.BIN".to_string()),
            ]
        );
    }

    #[test]
    fn walk_accepts_an_exact_unaligned_table_size_like_retail_masters() {
        // Retail discs record the exact byte length of a dirtab rather than
        // rounding it to a sector multiple (a real root table can be as
        // small as 116 bytes).
        let root_sector = 40u32;
        let a = encode_dirent(0, 8, 100, 10, 0, b"A.BIN");
        let b = encode_dirent(0, 0, 200, 20, 0, b"B.BIN");
        let mut root = a.clone();
        root.resize(32, 0xFF);
        root.extend_from_slice(&b);
        let root_size = root.len() as u32;
        assert_ne!(root_size % SECTOR_SIZE as u32, 0);

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, root_size));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);

        let mut cursor = Cursor::new(image);
        let volume = XdvdfsVolume::probe(&mut cursor, &[0x0]).unwrap();

        let mut seen: Vec<String> = Vec::new();
        walk_dir_tables(&mut cursor, &volume, |_, entry| {
            seen.push(entry.name_str());
            Ok(())
        })
        .unwrap();
        seen.sort();
        assert_eq!(seen, vec!["A.BIN".to_string(), "B.BIN".to_string()]);
    }

    #[test]
    fn walk_yields_zero_entries_for_an_empty_directory() {
        let root_sector = 40u32;
        let root = vec![0xFFu8; SECTOR_SIZE as usize];

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);

        let mut cursor = Cursor::new(image);
        let volume = XdvdfsVolume::probe(&mut cursor, &[0x0]).unwrap();

        let mut count = 0;
        walk_dir_tables(&mut cursor, &volume, |_, _| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn walk_rejects_a_self_referencing_dirtab() {
        let root_sector = 40u32;
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        // "LOOP" points back at the root's own sector, so descending into
        // it would re-read the same table forever without a visited-sector
        // guard.
        let loop_entry = encode_dirent(
            0,
            0,
            root_sector,
            SECTOR_SIZE as u32,
            ATTR_DIRECTORY,
            b"LOOP",
        );
        root[0..loop_entry.len()].copy_from_slice(&loop_entry);

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);

        let mut cursor = Cursor::new(image);
        let volume = XdvdfsVolume::probe(&mut cursor, &[0x0]).unwrap();

        let err = walk_dir_tables(&mut cursor, &volume, |_, _| Ok(())).unwrap_err();
        assert!(matches!(err, XdvdfsError::InvalidDirent { .. }), "{err}");
    }

    #[test]
    fn walk_rejects_an_oversized_directory_table() {
        let root_sector = 40u32;
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        // 264,192 = 129 * 2048: sector-aligned, but past the 262,140-byte
        // cap the u16 dirent offsets can address.
        let entry = encode_dirent(0, 0, 41, 264_192, ATTR_DIRECTORY, b"HUGE");
        root[0..entry.len()].copy_from_slice(&entry);

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);

        let mut cursor = Cursor::new(image);
        let volume = XdvdfsVolume::probe(&mut cursor, &[0x0]).unwrap();

        let err = walk_dir_tables(&mut cursor, &volume, |_, _| Ok(())).unwrap_err();
        assert!(matches!(err, XdvdfsError::InvalidDirent { .. }), "{err}");
    }

    #[test]
    fn walk_treats_a_zero_size_subdirectory_as_empty() {
        let root_sector = 40u32;
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = encode_dirent(0, 0, 41, 0, ATTR_DIRECTORY, b"EMPTYDIR");
        root[0..entry.len()].copy_from_slice(&entry);

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);

        let mut cursor = Cursor::new(image);
        let volume = XdvdfsVolume::probe(&mut cursor, &[0x0]).unwrap();

        let mut seen: Vec<String> = Vec::new();
        walk_dir_tables(&mut cursor, &volume, |_, entry| {
            seen.push(entry.name_str());
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec!["EMPTYDIR".to_string()]);
    }

    #[test]
    fn cp1252_high_byte_decodes_via_the_table() {
        let entry = DirEntry {
            left: 0,
            right: 0,
            start_sector: 0,
            size: 0,
            attributes: 0,
            name: vec![0x99],
        };
        assert_eq!(entry.name_str(), "\u{2122}"); // TRADE MARK SIGN
    }
}
