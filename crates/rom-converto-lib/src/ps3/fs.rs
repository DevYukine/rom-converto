//! Minimal ISO9660 reader for the PS3 disc plain region: fetches
//! `/PS3_DISC.SFB` and `/PS3_GAME/PARAM.SFO` in plaintext (no key). Walks
//! the PVD and at most two directory levels; every read is bounded.

use std::io::{Read, Seek, SeekFrom};

use crate::ps3::error::Ps3Result;
use crate::ps3::region::SECTOR_SIZE;

const PVD_SECTOR: u64 = 16;
const MAX_DIR_BYTES: u32 = 1 << 20;
const MAX_FILE_BYTES: u32 = 4 << 20;

const DISC_SFB: &str = "PS3_DISC.SFB";
const PS3_GAME: &str = "PS3_GAME";
const PARAM_SFO: &str = "PARAM.SFO";
const ICON0_PNG: &str = "ICON0.PNG";
const MAX_ROOT_ENTRIES: usize = 64;

pub struct PlainFiles {
    pub disc_sfb: Option<Vec<u8>>,
    pub param_sfo: Option<Vec<u8>>,
    pub icon0: Option<Vec<u8>>,
    /// `(name, size, is_dir)` for each root directory entry, dot entries
    /// skipped, capped at [`MAX_ROOT_ENTRIES`].
    pub root_entries: Vec<(String, u32, bool)>,
}

#[derive(Clone, Copy)]
struct Entry {
    lba: u32,
    size: u32,
    is_dir: bool,
}

pub fn read_plain_files<R: Read + Seek>(reader: &mut R) -> Ps3Result<PlainFiles> {
    let Some(root) = read_root(reader)? else {
        return Ok(PlainFiles {
            disc_sfb: None,
            param_sfo: None,
            icon0: None,
            root_entries: Vec::new(),
        });
    };
    let root_dir = read_extent(reader, &root, MAX_DIR_BYTES)?;

    let disc_sfb = match find_in_dir(&root_dir, DISC_SFB) {
        Some(e) if !e.is_dir => Some(read_extent(reader, &e, MAX_FILE_BYTES)?),
        _ => None,
    };

    let (param_sfo, icon0) = match find_in_dir(&root_dir, PS3_GAME) {
        Some(e) if e.is_dir => {
            let game_dir = read_extent(reader, &e, MAX_DIR_BYTES)?;
            let sfo = match find_in_dir(&game_dir, PARAM_SFO) {
                Some(f) if !f.is_dir => Some(read_extent(reader, &f, MAX_FILE_BYTES)?),
                _ => None,
            };
            // Decorative asset: a corrupt/out-of-range record must not fail
            // the whole info read.
            let icon = match find_in_dir(&game_dir, ICON0_PNG) {
                Some(f) if !f.is_dir => read_extent(reader, &f, MAX_FILE_BYTES).ok(),
                _ => None,
            };
            (sfo, icon)
        }
        _ => (None, None),
    };

    let root_entries = dir_entries(&root_dir, MAX_ROOT_ENTRIES);

    Ok(PlainFiles {
        disc_sfb,
        param_sfo,
        icon0,
        root_entries,
    })
}

/// Reliable PS3 marker: `/PS3_DISC.SFB` present in the ISO9660 root.
pub(crate) fn is_ps3_disc<R: Read + Seek>(reader: &mut R) -> Ps3Result<bool> {
    let Some(root) = read_root(reader)? else {
        return Ok(false);
    };
    let root_dir = read_extent(reader, &root, MAX_DIR_BYTES)?;
    Ok(matches!(find_in_dir(&root_dir, DISC_SFB), Some(e) if !e.is_dir))
}

/// `Ok(None)` when the image is not ISO9660.
fn read_root<R: Read + Seek>(reader: &mut R) -> Ps3Result<Option<Entry>> {
    let mut pvd = [0u8; SECTOR_SIZE];
    reader.seek(SeekFrom::Start(PVD_SECTOR * SECTOR_SIZE as u64))?;
    reader.read_exact(&mut pvd)?;
    if &pvd[1..6] != b"CD001" {
        return Ok(None);
    }
    Ok(record_fields(&pvd[156..190]))
}

fn read_extent<R: Read + Seek>(reader: &mut R, entry: &Entry, cap: u32) -> Ps3Result<Vec<u8>> {
    let mut buf = vec![0u8; entry.size.min(cap) as usize];
    reader.seek(SeekFrom::Start(entry.lba as u64 * SECTOR_SIZE as u64))?;
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn record_fields(rec: &[u8]) -> Option<Entry> {
    if rec.len() < 34 {
        return None;
    }
    Some(Entry {
        lba: u32::from_le_bytes(rec[2..6].try_into().expect("4-byte slice")),
        size: u32::from_le_bytes(rec[10..14].try_into().expect("4-byte slice")),
        is_dir: rec[25] & 0x02 != 0,
    })
}

fn find_in_dir(dir: &[u8], name: &str) -> Option<Entry> {
    let mut off = 0usize;
    while off < dir.len() {
        let rec_len = dir[off] as usize;
        if rec_len == 0 {
            // A zero length byte pads out the rest of the logical sector.
            off = (off / SECTOR_SIZE + 1) * SECTOR_SIZE;
            continue;
        }
        if rec_len < 34 || off + rec_len > dir.len() {
            break;
        }
        let rec = &dir[off..off + rec_len];
        let name_len = rec[32] as usize;
        if 33 + name_len <= rec_len && name_matches(&rec[33..33 + name_len], name) {
            return record_fields(rec);
        }
        off += rec_len;
    }
    None
}

fn name_matches(raw: &[u8], name: &str) -> bool {
    let end = raw.iter().position(|&b| b == b';').unwrap_or(raw.len());
    raw[..end].eq_ignore_ascii_case(name.as_bytes())
}

/// Lists `(name, size, is_dir)` for a directory's records, skipping the two
/// dot entries and stopping at `cap` entries.
fn dir_entries(dir: &[u8], cap: usize) -> Vec<(String, u32, bool)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < dir.len() && out.len() < cap {
        let rec_len = dir[off] as usize;
        if rec_len == 0 {
            // A zero length byte pads out the rest of the logical sector.
            off = (off / SECTOR_SIZE + 1) * SECTOR_SIZE;
            continue;
        }
        if rec_len < 34 || off + rec_len > dir.len() {
            break;
        }
        let rec = &dir[off..off + rec_len];
        let name_len = rec[32] as usize;
        if 33 + name_len <= rec_len {
            let raw = &rec[33..33 + name_len];
            if raw != [0u8]
                && raw != [1u8]
                && let Some(entry) = record_fields(rec)
            {
                let end = raw.iter().position(|&b| b == b';').unwrap_or(raw.len());
                let name = String::from_utf8_lossy(&raw[..end]).into_owned();
                out.push((name, entry.size, entry.is_dir));
            }
        }
        off += rec_len;
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    fn both_endian_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&v.to_be_bytes());
    }

    fn dir_record(out: &mut Vec<u8>, name: &[u8], lba: u32, size: u32, is_dir: bool) {
        let rec_len = (33 + name.len() + 1) & !1;
        let mut rec = vec![0u8; rec_len];
        rec[0] = rec_len as u8;
        both_endian_u32(&mut rec, 2, lba);
        both_endian_u32(&mut rec, 10, size);
        rec[25] = if is_dir { 0x02 } else { 0 };
        rec[32] = name.len() as u8;
        rec[33..33 + name.len()].copy_from_slice(name);
        out.extend_from_slice(&rec);
    }

    /// A real, minimal (1x1) PNG so `Image::from_png` parses it as a valid image.
    pub const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64,
        0xF8, 0xCF, 0x50, 0x0F, 0x00, 0x03, 0x86, 0x01, 0x80, 0x5A, 0x34, 0x7D, 0x6B, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// Root + PS3_DISC.SFB + PS3_GAME/PARAM.SFO/ICON0.PNG across fixed sectors.
    pub fn build_ps3_iso(sfb: &[u8], sfo: &[u8]) -> Vec<u8> {
        build_ps3_iso_with_icon(sfb, sfo, &[])
    }

    /// Same as [`build_ps3_iso`] but also writes `PS3_GAME/ICON0.PNG` when
    /// `icon` is non-empty.
    pub fn build_ps3_iso_with_icon(sfb: &[u8], sfo: &[u8], icon: &[u8]) -> Vec<u8> {
        const ROOT_LBA: u32 = 17;
        const SFB_LBA: u32 = 18;
        const GAME_LBA: u32 = 19;
        const SFO_LBA: u32 = 20;
        const ICON_LBA: u32 = 21;
        let sector_count = if icon.is_empty() { 21 } else { 22 };
        let mut iso = vec![0u8; sector_count * SECTOR_SIZE];

        let pvd = &mut iso[16 * SECTOR_SIZE..17 * SECTOR_SIZE];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        let root = &mut pvd[156..190];
        root[0] = 34;
        both_endian_u32(root, 2, ROOT_LBA);
        both_endian_u32(root, 10, SECTOR_SIZE as u32);
        root[25] = 0x02;
        root[32] = 1;

        let mut rd = Vec::new();
        dir_record(&mut rd, &[0], ROOT_LBA, SECTOR_SIZE as u32, true);
        dir_record(&mut rd, &[1], ROOT_LBA, SECTOR_SIZE as u32, true);
        dir_record(&mut rd, b"PS3_DISC.SFB;1", SFB_LBA, sfb.len() as u32, false);
        dir_record(&mut rd, b"PS3_GAME", GAME_LBA, SECTOR_SIZE as u32, true);
        iso[ROOT_LBA as usize * SECTOR_SIZE..ROOT_LBA as usize * SECTOR_SIZE + rd.len()]
            .copy_from_slice(&rd);

        let mut gd = Vec::new();
        dir_record(&mut gd, &[0], GAME_LBA, SECTOR_SIZE as u32, true);
        dir_record(&mut gd, &[1], ROOT_LBA, SECTOR_SIZE as u32, true);
        dir_record(&mut gd, b"PARAM.SFO;1", SFO_LBA, sfo.len() as u32, false);
        if !icon.is_empty() {
            dir_record(&mut gd, b"ICON0.PNG;1", ICON_LBA, icon.len() as u32, false);
        }
        iso[GAME_LBA as usize * SECTOR_SIZE..GAME_LBA as usize * SECTOR_SIZE + gd.len()]
            .copy_from_slice(&gd);

        iso[SFB_LBA as usize * SECTOR_SIZE..SFB_LBA as usize * SECTOR_SIZE + sfb.len()]
            .copy_from_slice(sfb);
        iso[SFO_LBA as usize * SECTOR_SIZE..SFO_LBA as usize * SECTOR_SIZE + sfo.len()]
            .copy_from_slice(sfo);
        if !icon.is_empty() {
            iso[ICON_LBA as usize * SECTOR_SIZE..ICON_LBA as usize * SECTOR_SIZE + icon.len()]
                .copy_from_slice(icon);
        }
        iso
    }

    #[test]
    fn fetches_both_plain_files() {
        let iso = build_ps3_iso(b"sfb-bytes", b"sfo-bytes");
        let files = read_plain_files(&mut Cursor::new(iso)).unwrap();
        assert_eq!(files.disc_sfb.unwrap(), b"sfb-bytes");
        assert_eq!(files.param_sfo.unwrap(), b"sfo-bytes");
    }

    #[test]
    fn fetches_icon_and_root_entries() {
        let iso = build_ps3_iso_with_icon(b"sfb-bytes", b"sfo-bytes", TINY_PNG);
        let files = read_plain_files(&mut Cursor::new(iso)).unwrap();
        assert_eq!(files.icon0.unwrap(), TINY_PNG);
        let names: Vec<&str> = files
            .root_entries
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert!(names.contains(&"PS3_DISC.SFB"));
        assert!(names.contains(&"PS3_GAME"));
    }

    #[test]
    fn detects_ps3_marker() {
        let iso = build_ps3_iso(b"x", b"y");
        assert!(is_ps3_disc(&mut Cursor::new(iso)).unwrap());
    }

    #[test]
    fn non_iso_is_not_ps3_and_has_no_files() {
        let mut junk = Cursor::new(vec![0u8; 21 * SECTOR_SIZE]);
        assert!(!is_ps3_disc(&mut junk).unwrap());
        let files = read_plain_files(&mut junk).unwrap();
        assert!(files.disc_sfb.is_none() && files.param_sfo.is_none());
    }

    #[test]
    fn malformed_records_do_not_panic() {
        let mut iso = build_ps3_iso(b"x", b"y");
        // Fill the root directory sector with garbage record lengths.
        for b in iso[17 * SECTOR_SIZE..18 * SECTOR_SIZE].iter_mut() {
            *b = 0xFF;
        }
        let files = read_plain_files(&mut Cursor::new(iso)).unwrap();
        assert!(files.disc_sfb.is_none() && files.param_sfo.is_none());
    }
}
