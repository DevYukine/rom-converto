//! XISO volume summary: what the probed base is and what the directory
//! tables account for.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::XboxResult;
use super::xbe::parse_xbe;
use crate::microsoft::xdvdfs::{
    PartitionKind, XBOX_PROBE_BASES, XdvdfsVolume, data_offset, walk_dir_tables, walk_root_table,
};
use crate::microsoft::xex::read_xex_info;

/// Summary of an XISO's probed partition layout and root title metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XisoInfo {
    /// Renamed on the wire: [`crate::info::InfoResult`] already tags its
    /// variants with a `kind` field, which would otherwise collide with
    /// this one when flattened into the same JSON object.
    #[serde(rename = "partition_kind")]
    pub kind: PartitionKind,
    pub base: u64,
    pub root_sector: u32,
    pub root_size: u32,
    pub file_count: u64,
    pub dir_count: u64,
    pub total_file_bytes: u64,
    pub image_size: u64,
    /// Original Xbox title metadata from a root `default.xbe`, when present.
    pub xbe: Option<crate::microsoft::xbox::XbeInfo>,
    /// Xbox 360 title metadata from a root `default.xex` (a 360 XDVDFS disc
    /// image routes here too), when present.
    pub xex: Option<crate::microsoft::xex::XexInfo>,
    /// Entries in the volume's root directory, up to [`MAX_ROOT_ENTRIES`],
    /// sorted by name.
    #[serde(default)]
    pub root_entries: Vec<XisoRootEntry>,
}

/// Cap on the number of root directory entries collected into
/// [`XisoInfo::root_entries`].
const MAX_ROOT_ENTRIES: usize = 64;

/// One entry in an XISO's root directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XisoRootEntry {
    pub name: String,
    pub size: u32,
    pub is_dir: bool,
}

/// Reads a named non-directory file from the volume's root directory,
/// case-insensitively. Best effort: any miss or I/O error yields `None`.
fn read_root_file<R: Read + Seek>(
    reader: &mut R,
    volume: &XdvdfsVolume,
    name: &str,
    image_size: u64,
) -> Option<Vec<u8>> {
    let mut found: Option<(u32, u32)> = None;
    walk_root_table(reader, volume, |entry| {
        if found.is_none() && !entry.is_directory() && entry.name_str().eq_ignore_ascii_case(name) {
            found = Some((entry.start_sector, entry.size));
        }
        Ok(())
    })
    .ok()?;
    let (sector, size) = found?;
    // A corrupt or truncated image can claim a huge size; never allocate
    // past the file itself.
    if size as u64 > image_size {
        return None;
    }
    reader
        .seek(SeekFrom::Start(data_offset(volume, sector)))
        .ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Probes an XISO and summarizes its partition layout, file/dir counts,
/// and root `default.xbe`/`default.xex` metadata.
///
/// # Errors
/// Returns an error if no XDVDFS volume descriptor is found at any probed base.
pub fn read_info(path: &Path) -> XboxResult<XisoInfo> {
    let mut file = File::open(path)?;
    let image_size = file.metadata()?.len();
    let volume = XdvdfsVolume::probe(&mut file, &XBOX_PROBE_BASES)?;

    let mut file_count = 0;
    let mut dir_count = 0;
    let mut total_file_bytes = 0;
    walk_dir_tables(&mut file, &volume, |_, entry| {
        if entry.is_directory() {
            dir_count += 1;
        } else {
            file_count += 1;
            total_file_bytes += entry.size as u64;
        }
        Ok(())
    })?;

    let xbe =
        read_root_file(&mut file, &volume, "default.xbe", image_size).and_then(|b| parse_xbe(&b));
    let xex = read_root_file(&mut file, &volume, "default.xex", image_size)
        .and_then(|b| read_xex_info(&b));

    let mut root_entries = Vec::new();
    walk_root_table(&mut file, &volume, |entry| {
        if root_entries.len() < MAX_ROOT_ENTRIES {
            root_entries.push(XisoRootEntry {
                name: entry.name_str().to_string(),
                size: entry.size,
                is_dir: entry.is_directory(),
            });
        }
        Ok(())
    })?;
    root_entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(XisoInfo {
        kind: volume.kind,
        base: volume.base,
        root_sector: volume.root_sector,
        root_size: volume.root_size,
        file_count,
        dir_count,
        total_file_bytes,
        image_size,
        xbe,
        xex,
        root_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microsoft::xdvdfs::{SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR, VOLUME_MAGIC};

    fn build_descriptor(root_sector: u32, root_size: u32) -> Vec<u8> {
        let mut d = vec![0u8; 0x800];
        d[0..20].copy_from_slice(VOLUME_MAGIC);
        d[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
        d[0x1C..0x24].copy_from_slice(&0u64.to_le_bytes());
        d[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
        d
    }

    fn encode_root_file_dirent(name: &[u8], start_sector: u32, size: u32) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + name.len());
        e.extend_from_slice(&0u16.to_le_bytes()); // left
        e.extend_from_slice(&0u16.to_le_bytes()); // right
        e.extend_from_slice(&start_sector.to_le_bytes());
        e.extend_from_slice(&size.to_le_bytes());
        e.push(0); // attributes: plain file
        e.push(name.len() as u8);
        e.extend_from_slice(name);
        e
    }

    /// Minimal valid XBE carrying `title` in its certificate (mirrors the
    /// layout the xbe module's own tests build).
    fn build_xbe(title: &str) -> Vec<u8> {
        const BASE_ADDRESS: u32 = 0x10000;
        const CERT_OFFSET: usize = 0x180;
        let mut buf = vec![0u8; 0x1000];
        buf[0..4].copy_from_slice(b"XBEH");
        buf[0x104..0x108].copy_from_slice(&BASE_ADDRESS.to_le_bytes());
        let cert_rva = BASE_ADDRESS + CERT_OFFSET as u32;
        buf[0x118..0x11C].copy_from_slice(&cert_rva.to_le_bytes());
        let title_id = 0x4D53_0004u32; // "MS" + game 4
        buf[CERT_OFFSET + 0x08..CERT_OFFSET + 0x0C].copy_from_slice(&title_id.to_le_bytes());
        for (i, unit) in title.encode_utf16().enumerate() {
            let at = CERT_OFFSET + 0x0C + i * 2;
            buf[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        buf
    }

    /// Trimmed (base-0) image whose root holds one `default.xbe` file with
    /// `payload` as its contents.
    fn build_iso_with_default_xbe(payload: &[u8]) -> Vec<u8> {
        let root_sector = 40u32;
        let data_sector = 41u32;
        let data_sectors = (payload.len().div_ceil(SECTOR_SIZE as usize)).max(1) as u64;

        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = encode_root_file_dirent(b"default.xbe", data_sector, payload.len() as u32);
        root[0..entry.len()].copy_from_slice(&entry);

        let total_sectors = data_sector as u64 + data_sectors;
        let mut image = vec![0u8; (total_sectors * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);
        let data_off = (data_sector as u64 * SECTOR_SIZE) as usize;
        image[data_off..data_off + payload.len()].copy_from_slice(payload);
        image
    }

    #[test]
    fn read_info_parses_root_default_xbe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        let xbe = build_xbe("Test Game");
        std::fs::write(&path, build_iso_with_default_xbe(&xbe)).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.file_count, 1);
        assert_eq!(info.dir_count, 0);
        assert_eq!(info.total_file_bytes, xbe.len() as u64);
        let parsed = info.xbe.expect("default.xbe parsed");
        assert_eq!(parsed.title_name, "Test Game");
        assert_eq!(parsed.title_id_code, "MS-004");
        assert!(info.xex.is_none());
        assert_eq!(info.root_entries.len(), 1);
        assert_eq!(info.root_entries[0].name, "default.xbe");
        assert_eq!(info.root_entries[0].size, xbe.len() as u32);
        assert!(!info.root_entries[0].is_dir);
    }

    #[test]
    fn read_info_tolerates_garbage_default_xbe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        let garbage = vec![0u8; 0x1000];
        std::fs::write(&path, build_iso_with_default_xbe(&garbage)).unwrap();

        let info = read_info(&path).unwrap();
        assert!(info.xbe.is_none());
        assert!(info.xex.is_none());
        // Container stats are unaffected by the failed metadata parse.
        assert_eq!(info.file_count, 1);
        assert_eq!(info.dir_count, 0);
        assert_eq!(info.total_file_bytes, garbage.len() as u64);
    }
}
