//! Minimal ISO9660 probing for PlayStation-family disc images.
//!
//! Reads only what console routing needs: the primary volume
//! descriptor at sector 16, the root directory extent, and a few
//! root-level names (`SYSTEM.CNF`, `PSP_GAME`, `UMD_DATA.BIN`).
//! `SYSTEM.CNF` distinguishes PS2 (`BOOT2`) from PS1 (`BOOT`); the
//! sector count splits CD-media from DVD-media PS2 discs. Everything
//! is positional reads of a handful of sectors, so probing a 4 GB
//! image costs the same as a 4 MB one.

use std::fs::File;
use std::io;
use std::path::Path;

use super::pread::file_read_exact_at;

const SECTOR: u64 = 2048;
const PVD_OFFSET: u64 = 16 * SECTOR;

/// Sector count above which the medium cannot be a CD. Same cutoff
/// PCSX2 uses for its CD/DVD typing (`FindDiskType`).
const CD_MAX_SECTORS: u64 = 452_849;

/// Reading the root directory is capped to keep a hostile or corrupt
/// extent length from ballooning the probe.
const MAX_ROOT_DIR_BYTES: u32 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscKind {
    Ps2Dvd,
    Ps2Cd,
    Psp,
    Ps1,
    UnknownIso,
}

impl DiscKind {
    pub fn label(self) -> &'static str {
        match self {
            DiscKind::Ps2Dvd => "PS2 (DVD)",
            DiscKind::Ps2Cd => "PS2 (CD)",
            DiscKind::Psp => "PSP",
            DiscKind::Ps1 => "PS1",
            DiscKind::UnknownIso => "unknown",
        }
    }
}

/// Identify the console family of a 2048-byte-sector disc image.
/// Malformed or truncated images degrade to [`DiscKind::UnknownIso`];
/// only real I/O failures error.
pub fn detect_disc_kind(path: &Path) -> io::Result<DiscKind> {
    let file = File::open(path)?;
    detect_disc_kind_file(&file)
}

pub fn detect_disc_kind_file(file: &File) -> io::Result<DiscKind> {
    let file_len = file.metadata()?.len();
    match probe(file, file_len) {
        Ok(kind) => Ok(kind),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(DiscKind::UnknownIso),
        Err(e) => Err(e),
    }
}

fn probe(file: &File, file_len: u64) -> io::Result<DiscKind> {
    let mut pvd = [0u8; SECTOR as usize];
    file_read_exact_at(file, &mut pvd, PVD_OFFSET)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return Ok(DiscKind::UnknownIso);
    }

    let system_id = &pvd[8..40];
    if contains(system_id, b"PSP GAME") {
        return Ok(DiscKind::Psp);
    }

    // The PVD may under-report on some masters; trust whichever of the
    // declared volume size and the actual image size is larger.
    let volume_sectors = u32::from_le_bytes(pvd[80..84].try_into().unwrap()) as u64;
    let sectors = volume_sectors.max(file_len / SECTOR);

    let root = scan_root_directory(file, &pvd)?;

    if root.has_psp_markers {
        return Ok(DiscKind::Psp);
    }
    if let Some((lba, size)) = root.system_cnf {
        let mut buf = vec![0u8; size.min(SECTOR as u32) as usize];
        file_read_exact_at(file, &mut buf, lba as u64 * SECTOR)?;
        if contains(&buf, b"BOOT2") {
            return Ok(if sectors > CD_MAX_SECTORS {
                DiscKind::Ps2Dvd
            } else {
                DiscKind::Ps2Cd
            });
        }
        if contains(&buf, b"BOOT") {
            return Ok(DiscKind::Ps1);
        }
    }

    Ok(DiscKind::UnknownIso)
}

#[derive(Default)]
struct RootScan {
    system_cnf: Option<(u32, u32)>,
    has_psp_markers: bool,
}

fn scan_root_directory(file: &File, pvd: &[u8]) -> io::Result<RootScan> {
    let record = &pvd[156..190];
    let root_lba = u32::from_le_bytes(record[2..6].try_into().unwrap());
    let root_size = u32::from_le_bytes(record[10..14].try_into().unwrap()).min(MAX_ROOT_DIR_BYTES);

    let mut dir = vec![0u8; root_size as usize];
    file_read_exact_at(file, &mut dir, root_lba as u64 * SECTOR)?;

    let mut scan = RootScan::default();
    let mut off = 0usize;
    while off < dir.len() {
        let rec_len = dir[off] as usize;
        if rec_len == 0 {
            // Records never cross sector boundaries; a zero length
            // byte means the rest of this sector is padding.
            off = (off / SECTOR as usize + 1) * SECTOR as usize;
            continue;
        }
        if off + rec_len > dir.len() || rec_len < 34 {
            break;
        }
        let entry = &dir[off..off + rec_len];
        let ident_len = entry[32] as usize;
        if 33 + ident_len <= rec_len {
            let name = strip_version(&entry[33..33 + ident_len]);
            if name.eq_ignore_ascii_case(b"SYSTEM.CNF") {
                let lba = u32::from_le_bytes(entry[2..6].try_into().unwrap());
                let size = u32::from_le_bytes(entry[10..14].try_into().unwrap());
                scan.system_cnf = Some((lba, size));
            } else if name.eq_ignore_ascii_case(b"PSP_GAME")
                || name.eq_ignore_ascii_case(b"UMD_DATA.BIN")
            {
                scan.has_psp_markers = true;
            }
        }
        off += rec_len;
    }
    Ok(scan)
}

fn strip_version(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b';') {
        Some(p) => &name[..p],
        None => name,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    use super::SECTOR;

    pub const ROOT_DIR_LBA: u32 = 18;
    pub const FILE_LBA: u32 = 19;

    fn both_endian_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&v.to_be_bytes());
    }

    fn dir_record(out: &mut Vec<u8>, name: &[u8], lba: u32, size: u32, is_dir: bool) {
        let rec_len = (33 + name.len() + 1) & !1;
        let mut rec = vec![0u8; rec_len];
        rec[0] = rec_len as u8;
        rec[2..6].copy_from_slice(&lba.to_le_bytes());
        rec[6..10].copy_from_slice(&lba.to_be_bytes());
        rec[10..14].copy_from_slice(&size.to_le_bytes());
        rec[14..18].copy_from_slice(&size.to_be_bytes());
        rec[25] = if is_dir { 2 } else { 0 };
        rec[32] = name.len() as u8;
        rec[33..33 + name.len()].copy_from_slice(name);
        out.extend_from_slice(&rec);
    }

    pub struct IsoSpec<'a> {
        pub system_id: &'a [u8],
        pub volume_sectors: u32,
        pub root_entries: &'a [(&'a [u8], bool)],
        pub file_content: &'a [u8],
    }

    /// Build a minimal valid ISO9660 image: PVD at sector 16, root
    /// directory at [`ROOT_DIR_LBA`], every listed file entry backed by
    /// `file_content` at [`FILE_LBA`].
    pub fn make_iso(spec: &IsoSpec) -> Vec<u8> {
        let mut iso = vec![0u8; 20 * SECTOR as usize];

        let pvd_off = 16 * SECTOR as usize;
        let pvd = &mut iso[pvd_off..pvd_off + SECTOR as usize];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[8..8 + spec.system_id.len().min(32)]
            .copy_from_slice(&spec.system_id[..spec.system_id.len().min(32)]);
        both_endian_u32(pvd, 80, spec.volume_sectors);
        let root = &mut pvd[156..190];
        root[0] = 34;
        root[2..6].copy_from_slice(&ROOT_DIR_LBA.to_le_bytes());
        root[6..10].copy_from_slice(&ROOT_DIR_LBA.to_be_bytes());
        root[10..14].copy_from_slice(&(SECTOR as u32).to_le_bytes());
        root[14..18].copy_from_slice(&(SECTOR as u32).to_be_bytes());
        root[25] = 2;
        root[32] = 1;

        let mut dir = Vec::new();
        dir_record(&mut dir, &[0], ROOT_DIR_LBA, SECTOR as u32, true);
        dir_record(&mut dir, &[1], ROOT_DIR_LBA, SECTOR as u32, true);
        for (name, is_dir) in spec.root_entries {
            dir_record(
                &mut dir,
                name,
                FILE_LBA,
                spec.file_content.len() as u32,
                *is_dir,
            );
        }
        let dir_off = ROOT_DIR_LBA as usize * SECTOR as usize;
        iso[dir_off..dir_off + dir.len()].copy_from_slice(&dir);

        let file_off = FILE_LBA as usize * SECTOR as usize;
        iso[file_off..file_off + spec.file_content.len()].copy_from_slice(spec.file_content);

        iso
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::test_fixtures::*;
    use super::*;

    fn detect_bytes(data: &[u8]) -> DiscKind {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        detect_disc_kind(f.path()).unwrap()
    }

    #[test]
    fn detects_ps2_cd_and_dvd_by_sector_count() {
        let cnf: &[u8] = b"BOOT2 = cdrom0:\\SLUS_123.45;1\r\nVER = 1.00\r\n";
        let entries: &[(&[u8], bool)] = &[(b"SYSTEM.CNF;1", false)];

        let cd = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 300_000,
            root_entries: entries,
            file_content: cnf,
        });
        assert_eq!(detect_bytes(&cd), DiscKind::Ps2Cd);

        let dvd = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 2_000_000,
            root_entries: entries,
            file_content: cnf,
        });
        assert_eq!(detect_bytes(&dvd), DiscKind::Ps2Dvd);
    }

    #[test]
    fn detects_ps1_via_boot_line() {
        let cnf: &[u8] = b"BOOT = cdrom:\\SLUS_000.01;1\r\nTCB = 4\r\n";
        let iso = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 250_000,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: cnf,
        });
        assert_eq!(detect_bytes(&iso), DiscKind::Ps1);
    }

    #[test]
    fn detects_psp_by_system_id_and_by_root_markers() {
        let by_id = make_iso(&IsoSpec {
            system_id: b"PSP GAME",
            volume_sectors: 800_000,
            root_entries: &[],
            file_content: &[],
        });
        assert_eq!(detect_bytes(&by_id), DiscKind::Psp);

        let by_dir = make_iso(&IsoSpec {
            system_id: b"",
            volume_sectors: 800_000,
            root_entries: &[(b"PSP_GAME", true), (b"UMD_DATA.BIN;1", false)],
            file_content: &[],
        });
        assert_eq!(detect_bytes(&by_dir), DiscKind::Psp);
    }

    #[test]
    fn unknown_for_non_iso_and_truncated_input() {
        assert_eq!(detect_bytes(&[0u8; 256]), DiscKind::UnknownIso);
        assert_eq!(detect_bytes(&vec![0u8; 20 * 2048]), DiscKind::UnknownIso);

        let mut garbage = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 300_000,
            root_entries: &[],
            file_content: &[],
        });
        garbage[16 * 2048 + 1] = b'X';
        assert_eq!(detect_bytes(&garbage), DiscKind::UnknownIso);
    }

    #[test]
    fn file_size_overrides_underreported_volume_size() {
        let cnf: &[u8] = b"BOOT2 = cdrom0:\\SLES_999.99;1\r\n";
        let mut iso = make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 1,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: cnf,
        });
        iso.resize((CD_MAX_SECTORS as usize + 2) * 2048, 0);
        assert_eq!(detect_bytes(&iso), DiscKind::Ps2Dvd);
    }
}
