//! Minimal ISO9660 probing for PlayStation-family disc images.
//!
//! Reads only what console routing and the `info` readers need: the
//! primary volume descriptor at sector 16, the root directory extent, a
//! few root-level names (`SYSTEM.CNF`, `PSP_GAME`, `UMD_DATA.BIN`), and
//! named files up to one subdirectory deep. `SYSTEM.CNF` distinguishes
//! PS2 (`BOOT2`) from PS1 (`BOOT`); the sector count splits CD-media from
//! DVD-media PS2 discs. Everything is positional reads of a handful of
//! sectors, so probing a 4 GB image costs the same as a 4 MB one.
//!
//! Reads go through [`SectorSource`], so the same probe runs over a plain
//! ISO file, a CUE/BIN data track, or a CSO/CHD container that decodes
//! only the blocks it is asked for.

use std::fs::File;
use std::io;
use std::path::Path;

use super::pread::file_read_exact_at;

const SECTOR: usize = 2048;
const PVD_LBA: u32 = 16;

/// Sector count above which the medium cannot be a CD. Same cutoff
/// PCSX2 uses for its CD/DVD typing (`FindDiskType`).
const CD_MAX_SECTORS: u64 = 452_849;

/// Reading a directory is capped to keep a hostile or corrupt extent
/// length from ballooning the probe.
const MAX_DIR_BYTES: u32 = 256 * 1024;

/// Cap on a file fetched through [`Volume::read_file`]; the largest thing
/// read that way is a PSP `ICON0.PNG`.
const MAX_FILE_BYTES: u32 = 4 * 1024 * 1024;

/// Random access to a disc image's 2048-byte logical sectors.
pub trait SectorSource {
    /// Reads the logical sector at `lba`. Reads past the end of the image
    /// fail with [`io::ErrorKind::UnexpectedEof`].
    fn read_sector(&mut self, lba: u32, buf: &mut [u8; SECTOR]) -> io::Result<()>;

    /// Logical sectors the source can supply, as far as it knows.
    fn total_sectors(&self) -> u64;
}

impl SectorSource for &File {
    fn read_sector(&mut self, lba: u32, buf: &mut [u8; SECTOR]) -> io::Result<()> {
        file_read_exact_at(self, buf, lba as u64 * SECTOR as u64)
    }

    fn total_sectors(&self) -> u64 {
        self.metadata()
            .map(|m| m.len() / SECTOR as u64)
            .unwrap_or(0)
    }
}

/// Console family identified from an ISO9660 disc image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscKind {
    Ps2Dvd,
    Ps2Cd,
    Psp,
    Ps1,
    UnknownIso,
}

impl DiscKind {
    /// Returns the disc kind's human-readable label, e.g. `"PS2 (DVD)"`.
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

/// A probed ISO9660 volume: its console family, volume identifier, sector
/// count, and the root directory extent later lookups walk.
#[derive(Debug, Clone)]
pub struct Volume {
    pub kind: DiscKind,
    pub volume_id: String,
    pub total_sectors: u64,
    root_lba: u32,
    root_size: u32,
}

/// Reads the primary volume descriptor and classifies the disc.
/// `Ok(None)` when the image is not ISO9660.
pub fn read_volume<S: SectorSource>(src: &mut S) -> io::Result<Option<Volume>> {
    let mut pvd = [0u8; SECTOR];
    src.read_sector(PVD_LBA, &mut pvd)?;
    if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return Ok(None);
    }

    // The PVD may under-report on some masters; trust whichever of the
    // declared volume size and the source's own extent is larger.
    let volume_sectors = read_u32(&pvd, 80) as u64;
    let record = &pvd[156..190];

    let mut volume = Volume {
        kind: DiscKind::UnknownIso,
        volume_id: trimmed_ascii(&pvd[40..72]),
        total_sectors: volume_sectors.max(src.total_sectors()),
        root_lba: read_u32(record, 2),
        root_size: read_u32(record, 10).min(MAX_DIR_BYTES),
    };
    volume.kind = classify(src, &volume, &pvd[8..40])?;
    Ok(Some(volume))
}

impl Volume {
    /// Logical size of the disc: its sector count as 2048-byte user
    /// sectors, independent of the container the disc arrived in.
    pub fn size_bytes(&self) -> u64 {
        self.total_sectors * SECTOR as u64
    }

    /// Reads a file named `NAME` or `DIR/NAME` (one subdirectory level) out
    /// of the volume, capped at 4 MiB. `Ok(None)` when it is absent.
    pub fn read_file<S: SectorSource>(
        &self,
        src: &mut S,
        path: &str,
    ) -> io::Result<Option<Vec<u8>>> {
        let (dir_name, file_name) = match path.split_once('/') {
            Some((dir, file)) => (Some(dir), file),
            None => (None, path),
        };

        let mut dir = read_extent(src, self.root_lba, self.root_size, MAX_DIR_BYTES)?;
        if let Some(dir_name) = dir_name {
            match find_in_dir(&dir, dir_name) {
                Some(entry) if entry.is_dir => {
                    dir = read_extent(src, entry.lba, entry.size, MAX_DIR_BYTES)?;
                }
                _ => return Ok(None),
            }
        }

        match find_in_dir(&dir, file_name) {
            Some(entry) if !entry.is_dir => Ok(Some(read_extent(
                src,
                entry.lba,
                entry.size,
                MAX_FILE_BYTES,
            )?)),
            _ => Ok(None),
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

/// Same detection as [`detect_disc_kind`], for an already-open file handle.
/// Malformed or truncated images degrade to [`DiscKind::UnknownIso`]; only
/// I/O errors other than end-of-file propagate.
pub fn detect_disc_kind_file(file: &File) -> io::Result<DiscKind> {
    let mut src = file;
    match read_volume(&mut src) {
        Ok(Some(volume)) => Ok(volume.kind),
        Ok(None) => Ok(DiscKind::UnknownIso),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(DiscKind::UnknownIso),
        Err(e) => Err(e),
    }
}

fn classify<S: SectorSource>(
    src: &mut S,
    volume: &Volume,
    system_id: &[u8],
) -> io::Result<DiscKind> {
    if contains(system_id, b"PSP GAME") {
        return Ok(DiscKind::Psp);
    }

    let root = read_extent(src, volume.root_lba, volume.root_size, MAX_DIR_BYTES)?;
    if find_in_dir(&root, "PSP_GAME").is_some() || find_in_dir(&root, "UMD_DATA.BIN").is_some() {
        return Ok(DiscKind::Psp);
    }

    if let Some(entry) = find_in_dir(&root, "SYSTEM.CNF") {
        let cnf = read_extent(
            src,
            entry.lba,
            entry.size.min(SECTOR as u32),
            MAX_FILE_BYTES,
        )?;
        if contains(&cnf, b"BOOT2") {
            return Ok(if volume.total_sectors > CD_MAX_SECTORS {
                DiscKind::Ps2Dvd
            } else {
                DiscKind::Ps2Cd
            });
        }
        if contains(&cnf, b"BOOT") {
            return Ok(DiscKind::Ps1);
        }
    }

    Ok(DiscKind::UnknownIso)
}

#[derive(Clone, Copy)]
struct DirEntry {
    lba: u32,
    size: u32,
    is_dir: bool,
}

fn read_extent<S: SectorSource>(src: &mut S, lba: u32, size: u32, cap: u32) -> io::Result<Vec<u8>> {
    let size = size.min(cap) as usize;
    let mut out = Vec::with_capacity(size.next_multiple_of(SECTOR));
    let mut sector = [0u8; SECTOR];
    for i in 0..size.div_ceil(SECTOR) {
        src.read_sector(lba.saturating_add(i as u32), &mut sector)?;
        out.extend_from_slice(&sector);
    }
    out.truncate(size);
    Ok(out)
}

fn find_in_dir(dir: &[u8], name: &str) -> Option<DirEntry> {
    let mut off = 0usize;
    while off < dir.len() {
        let rec_len = dir[off] as usize;
        if rec_len == 0 {
            // Records never cross sector boundaries; a zero length byte
            // means the rest of this sector is padding.
            off = (off / SECTOR + 1) * SECTOR;
            continue;
        }
        if off + rec_len > dir.len() || rec_len < 34 {
            return None;
        }
        let entry = &dir[off..off + rec_len];
        let ident_len = entry[32] as usize;
        if 33 + ident_len <= rec_len
            && strip_version(&entry[33..33 + ident_len]).eq_ignore_ascii_case(name.as_bytes())
        {
            return Some(DirEntry {
                lba: read_u32(entry, 2),
                size: read_u32(entry, 10),
                is_dir: entry[25] & 0x02 != 0,
            });
        }
        off += rec_len;
    }
    None
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(
        data[off..off + 4]
            .try_into()
            .expect("slice is exactly 4 bytes"),
    )
}

fn trimmed_ascii(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .trim_matches(|c: char| c == ' ' || c == '\0')
        .to_string()
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
    pub const SUBDIR_LBA: u32 = 20;

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

    /// One subdirectory and the files it holds, for images that need a
    /// nested entry such as `PSP_GAME/PARAM.SFO`.
    pub struct SubDir<'a> {
        pub name: &'a [u8],
        pub files: &'a [(&'a [u8], &'a [u8])],
    }

    /// Build a minimal valid ISO9660 image: PVD at sector 16, root
    /// directory at [`ROOT_DIR_LBA`], every listed file entry backed by
    /// `file_content` at [`FILE_LBA`].
    pub fn make_iso(spec: &IsoSpec) -> Vec<u8> {
        let mut iso = vec![0u8; 20 * SECTOR];

        let pvd_off = 16 * SECTOR;
        let pvd = &mut iso[pvd_off..pvd_off + SECTOR];
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
        let dir_off = ROOT_DIR_LBA as usize * SECTOR;
        iso[dir_off..dir_off + dir.len()].copy_from_slice(&dir);

        let file_off = FILE_LBA as usize * SECTOR;
        iso[file_off..file_off + spec.file_content.len()].copy_from_slice(spec.file_content);

        iso
    }

    /// [`make_iso`] plus one subdirectory at [`SUBDIR_LBA`] holding
    /// `subdir.files`, each at its own sector-aligned extent.
    pub fn make_iso_with_subdir(spec: &IsoSpec, subdir: &SubDir) -> Vec<u8> {
        let mut iso = make_iso(spec);

        let mut records = Vec::new();
        dir_record(&mut records, &[0], SUBDIR_LBA, SECTOR as u32, true);
        dir_record(&mut records, &[1], ROOT_DIR_LBA, SECTOR as u32, true);

        let mut content = Vec::new();
        let mut lba = SUBDIR_LBA + 1;
        for (name, data) in subdir.files {
            dir_record(&mut records, name, lba, data.len() as u32, false);
            let padded = data.len().next_multiple_of(SECTOR).max(SECTOR);
            content.extend_from_slice(data);
            content.resize(content.len() + padded - data.len(), 0);
            lba += (padded / SECTOR) as u32;
        }

        records.resize(SECTOR, 0);
        iso.extend_from_slice(&records);
        iso.extend_from_slice(&content);

        append_root_record(&mut iso, subdir.name, SUBDIR_LBA);
        iso
    }

    /// Overwrite the PVD volume identifier (bytes 40..72).
    pub fn set_volume_id(iso: &mut [u8], id: &[u8]) {
        let off = 16 * SECTOR + 40;
        iso[off..off + 32].fill(b' ');
        iso[off..off + id.len()].copy_from_slice(id);
    }

    fn append_root_record(iso: &mut [u8], name: &[u8], lba: u32) {
        let dir_off = ROOT_DIR_LBA as usize * SECTOR;
        let mut off = dir_off;
        while iso[off] != 0 {
            off += iso[off] as usize;
        }
        let mut rec = Vec::new();
        dir_record(&mut rec, name, lba, SECTOR as u32, true);
        iso[off..off + rec.len()].copy_from_slice(&rec);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::test_fixtures::*;
    use super::*;

    fn detect_bytes(data: &[u8]) -> DiscKind {
        let mut f = tempfile::NamedTempFile::new().expect("temp file");
        f.write_all(data).expect("write image");
        f.flush().expect("flush image");
        detect_disc_kind(f.path()).expect("detect disc kind")
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

    #[test]
    fn reads_volume_id_and_files_one_directory_deep() {
        let mut iso = make_iso_with_subdir(
            &IsoSpec {
                system_id: b"PSP GAME",
                volume_sectors: 800_000,
                root_entries: &[],
                file_content: &[],
            },
            &SubDir {
                name: b"PSP_GAME",
                files: &[
                    (b"PARAM.SFO;1", b"sfo-bytes"),
                    (b"ICON0.PNG;1", b"png-bytes"),
                ],
            },
        );
        set_volume_id(&mut iso, b"UMD_ROOT");

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("game.iso");
        std::fs::write(&path, &iso).expect("write image");
        let file = File::open(&path).expect("open image");
        let mut src = &file;

        let volume = read_volume(&mut src)
            .expect("read volume")
            .expect("iso9660 volume");
        assert_eq!(volume.kind, DiscKind::Psp);
        assert_eq!(volume.volume_id, "UMD_ROOT");
        assert_eq!(volume.total_sectors, 800_000);
        assert_eq!(
            volume
                .read_file(&mut src, "PSP_GAME/PARAM.SFO")
                .expect("read param.sfo")
                .as_deref(),
            Some(&b"sfo-bytes"[..])
        );
        assert_eq!(
            volume
                .read_file(&mut src, "PSP_GAME/ICON0.PNG")
                .expect("read icon0.png")
                .as_deref(),
            Some(&b"png-bytes"[..])
        );
        assert!(
            volume
                .read_file(&mut src, "PSP_GAME/MISSING.BIN")
                .expect("read missing file")
                .is_none()
        );
        assert!(
            volume
                .read_file(&mut src, "NOPE/PARAM.SFO")
                .expect("read missing directory")
                .is_none()
        );
    }
}
