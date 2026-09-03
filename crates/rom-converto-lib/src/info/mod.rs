//! Cross-console ROM metadata extraction (the `info` feature).
//!
//! Per-console extractors live alongside their parsers (such as `crate::chd::info`).
//! This module owns the umbrella [`InfoResult`] sum type, the shared
//! [`Image`] / [`MultilingualString`] / [`LanguageCode`] types, and a
//! top-level [`read_info`] dispatcher that the GUI uses to read any
//! supported file without knowing its format in advance.

use crate::util::iso9660::DiscKind;
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod image;

pub use crate::chd::info::ChdInfo;
pub use crate::cso::info::CsoInfo;
pub use crate::laserdisc::info::LdAviInfo;
pub use crate::microsoft::xbox::XisoInfo;
pub use crate::microsoft::xenon::ZarInfo;
pub use crate::nintendo::ctr::info::CtrInfo;
pub use crate::nintendo::dol::info::DolInfo;
pub use crate::nintendo::nds::info::NdsInfo;
pub use crate::nintendo::nx::info::NxInfo;
pub use crate::nintendo::rvl::info::RvlInfo;
pub use crate::nintendo::wup::info::WupInfo;
pub use crate::ps3::Ps3Info;
pub use crate::sony_disc::{DiscContent, PspInfo, PsxInfo};
pub use image::Image;

/// Per-console metadata read by [`read_info`], tagged with a `kind`
/// field on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InfoResult {
    Chd(ChdInfo),
    Cso(CsoInfo),
    Ctr(CtrInfo),
    Dol(DolInfo),
    Rvl(RvlInfo),
    Wup(WupInfo),
    Nx(NxInfo),
    Xbox(XisoInfo),
    Xenon(ZarInfo),
    Ps3(Ps3Info),
    Psx(PsxInfo),
    Psp(PspInfo),
    LaserDisc(LdAviInfo),
    Nds(NdsInfo),
}

/// A string carried per-language, as found in console-specific metadata
/// blocks (3DS SMDH, Wii IMET, Wii U meta.xml, Switch NACP, ...).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MultilingualString {
    pub entries: Vec<(LanguageCode, String)>,
}

impl MultilingualString {
    /// Builds a [`MultilingualString`] from `(language, text)` pairs,
    /// dropping any pair with empty text.
    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (LanguageCode, String)>,
    {
        let mut entries: Vec<(LanguageCode, String)> = pairs.into_iter().collect();
        entries.retain(|(_, s)| !s.is_empty());
        Self { entries }
    }

    /// True if no language has a non-empty entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Best-effort "primary" entry: English variants first, then any.
    pub fn primary(&self) -> Option<&str> {
        const ORDER: &[LanguageCode] = &[
            LanguageCode::English,
            LanguageCode::AmericanEnglish,
            LanguageCode::BritishEnglish,
        ];
        for pref in ORDER {
            if let Some((_, s)) = self.entries.iter().find(|(l, _)| l == pref) {
                return Some(s);
            }
        }
        self.entries.first().map(|(_, s)| s.as_str())
    }
}

/// Union of every per-language slot the supported console formats carry
/// (3DS SMDH, Wii IMET, Wii U meta.xml, Switch NACP, GameCube BNR2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageCode {
    Japanese,
    English,
    AmericanEnglish,
    BritishEnglish,
    French,
    CanadianFrench,
    German,
    Italian,
    Spanish,
    LatinAmericanSpanish,
    Dutch,
    Portuguese,
    BrazilianPortuguese,
    Russian,
    Korean,
    SimplifiedChinese,
    TraditionalChinese,
    Chinese,
    TaiwaneseChinese,
}

/// Options for [`read_info`]: an optional keys file and parent image
/// path, used only by the formats that need them.
#[derive(Debug, Clone, Default)]
pub struct InfoOptions {
    pub keys_path: Option<PathBuf>,
    pub parent_path: Option<PathBuf>,
}

/// Extension picks the console; magic bytes break ties for the
/// disc-image extensions (`.iso`, `.rvz`) shared by GameCube and Wii.
pub fn read_info(path: &Path, opts: &InfoOptions) -> Result<InfoResult> {
    let kind = detect_console(path)?;
    match kind {
        DetectedConsole::Chd => Ok(InfoResult::Chd(crate::chd::info::read_info(path)?)),
        DetectedConsole::Cso => Ok(InfoResult::Cso(crate::cso::info::read_info(path)?)),
        DetectedConsole::Ctr => Ok(InfoResult::Ctr(crate::nintendo::ctr::info::read_info(
            path,
        )?)),
        DetectedConsole::Dol => Ok(InfoResult::Dol(crate::nintendo::dol::info::read_info(
            path,
        )?)),
        DetectedConsole::Rvl => Ok(InfoResult::Rvl(crate::nintendo::rvl::info::read_info(
            path,
        )?)),
        DetectedConsole::Wup => Ok(InfoResult::Wup(crate::nintendo::wup::info::read_info(
            path,
            opts.keys_path.as_deref(),
        )?)),
        DetectedConsole::Nx => Ok(InfoResult::Nx(crate::nintendo::nx::info::read_info(
            path,
            opts.keys_path.as_deref(),
        )?)),
        DetectedConsole::Xbox => Ok(InfoResult::Xbox(crate::microsoft::xbox::read_info(path)?)),
        // sniff_xdvdfs also reports Xenon for a 360 XDVDFS *disc image*
        // (extension .iso/.rvz), not just a .zar container; only the
        // latter is a ZArchive the xenon reader can actually open.
        DetectedConsole::Xenon if !is_zar_extension(path) => {
            Ok(InfoResult::Xbox(crate::microsoft::xbox::read_info(path)?))
        }
        DetectedConsole::Xenon => Ok(InfoResult::Xenon(crate::microsoft::xenon::read_info(path)?)),
        DetectedConsole::Ps3 => Ok(InfoResult::Ps3(
            crate::ps3::read_ps3_info(path).map_err(|e| anyhow!("ps3 info: {e}"))?,
        )),
        DetectedConsole::Psx => Ok(InfoResult::Psx(crate::sony_disc::read_psx_info(path)?)),
        DetectedConsole::Psp => Ok(InfoResult::Psp(crate::sony_disc::read_psp_info(path)?)),
        DetectedConsole::LaserDisc => Ok(InfoResult::LaserDisc(crate::laserdisc::info::read_info(
            path,
        )?)),
        DetectedConsole::Nds => Ok(InfoResult::Nds(crate::nintendo::nds::info::read_info(
            path,
        )?)),
    }
}

/// True for `.zar` (case-insensitive), the only extension `detect_console`
/// ever maps to `Xenon` for an actual ZArchive container.
fn is_zar_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("zar"))
}

/// Console family identified by [`detect_console`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedConsole {
    Chd,
    Cso,
    Ctr,
    Dol,
    Rvl,
    Wup,
    Nx,
    Xbox,
    Xenon,
    Ps3,
    Psx,
    Psp,
    LaserDisc,
    Nds,
}

/// Detect which console family a path belongs to. Extension first, magic
/// bytes as a tiebreaker for the disc-image cases where the same extension
/// (`.iso`) could be GameCube or Wii.
pub fn detect_console(path: &Path) -> Result<DetectedConsole> {
    let lower_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    if path.is_dir() {
        // Treat any directory input as Wii U (NUS or loadiine).
        return Ok(DetectedConsole::Wup);
    }

    match lower_ext.as_deref() {
        Some("chd") => return Ok(DetectedConsole::Chd),
        Some("cso") | Some("zso") => return Ok(DetectedConsole::Cso),
        Some("cia") | Some("3ds") | Some("cci") | Some("cxi") | Some("ncch") | Some("zcia")
        | Some("zcci") | Some("zcxi") | Some("z3dsx") => {
            return Ok(DetectedConsole::Ctr);
        }
        Some("nsp") | Some("nsz") | Some("xci") | Some("xcz") => return Ok(DetectedConsole::Nx),
        Some("wud") | Some("wux") | Some("wua") => return Ok(DetectedConsole::Wup),
        Some("gcm") => return Ok(DetectedConsole::Dol),
        Some("wbfs") => return Ok(DetectedConsole::Rvl),
        Some("xiso") => return Ok(DetectedConsole::Xbox),
        Some("zar") => return Ok(DetectedConsole::Xenon),
        Some("cue") => return Ok(DetectedConsole::Psx),
        Some("avi") => return Ok(DetectedConsole::LaserDisc),
        Some("nds") | Some("dsi") => return Ok(DetectedConsole::Nds),
        Some("iso") | Some("rvz") => return sniff_disc_magic(path),
        _ => {}
    }

    Err(anyhow!(
        "could not detect console for path: {}",
        path.display()
    ))
}

fn sniff_disc_magic(path: &Path) -> Result<DetectedConsole> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut f = File::open(path)?;
    let mut head = [0u8; 4];
    f.read_exact(&mut head)?;

    // RVZ wraps the original disc; route to Wii or GameCube based on
    // the embedded disc id.
    if head == [b'R', b'V', b'Z', 0x01] {
        // RVZ stores the original disc head[0..0x80] at file offset 0x58.
        let mut disc_head = [0u8; 0x80];
        f.seek(SeekFrom::Start(0x58))?;
        f.read_exact(&mut disc_head)?;
        if disc_head[0x18..0x1C] == [0x5D, 0x1C, 0x9E, 0xA3] {
            return Ok(DetectedConsole::Rvl);
        }
        if disc_head[0x1C..0x20] == [0xC2, 0x33, 0x9F, 0x3D] {
            return Ok(DetectedConsole::Dol);
        }
        return Err(anyhow!(
            "rvz file at {} does not embed a Wii or GameCube disc",
            path.display()
        ));
    }

    let mut buf = [0u8; 4];
    f.seek(SeekFrom::Start(0x18))?;
    f.read_exact(&mut buf)?;
    if buf == [0x5D, 0x1C, 0x9E, 0xA3] {
        return Ok(DetectedConsole::Rvl);
    }

    f.seek(SeekFrom::Start(0x1C))?;
    f.read_exact(&mut buf)?;
    if buf == [0xC2, 0x33, 0x9F, 0x3D] {
        return Ok(DetectedConsole::Dol);
    }

    if let Ok(console) = sniff_xdvdfs(&mut f) {
        return Ok(console);
    }

    // PS3 discs are ISO9660; the reliable marker is /PS3_DISC.SFB in root.
    if let Ok(true) = crate::ps3::fs::is_ps3_disc(&mut f) {
        return Ok(DetectedConsole::Ps3);
    }

    match crate::util::iso9660::detect_disc_kind_file(&f)? {
        DiscKind::Ps1 | DiscKind::Ps2Cd | DiscKind::Ps2Dvd => return Ok(DetectedConsole::Psx),
        DiscKind::Psp => return Ok(DetectedConsole::Psp),
        DiscKind::UnknownIso => {}
    }

    Err(anyhow!(
        "disc file at {} does not match GameCube, Wii, Xbox, Xbox 360, PS3, PS1, PS2, or PSP magic",
        path.display()
    ))
}

/// Distinguishes Original Xbox from Xbox 360 for an `.iso` that missed the
/// GameCube/Wii magic checks above: probes the shared XDVDFS disc
/// filesystem with the widest known base-offset list, then classifies by
/// partition kind. A `Trimmed` (base 0) image carries no base-offset hint,
/// so its root directory is checked for a `default.xex` entry instead.
fn sniff_xdvdfs<R: std::io::Read + std::io::Seek>(reader: &mut R) -> Result<DetectedConsole> {
    use crate::microsoft::xdvdfs::{
        PartitionKind, X360_PROBE_BASES, XdvdfsVolume, walk_root_table,
    };

    let volume = XdvdfsVolume::probe(reader, &X360_PROBE_BASES)?;
    match volume.kind {
        PartitionKind::Xgd1 => Ok(DetectedConsole::Xbox),
        PartitionKind::Xgd2 | PartitionKind::Xgd3 | PartitionKind::X360Extra(_) => {
            Ok(DetectedConsole::Xenon)
        }
        PartitionKind::Trimmed => {
            let mut has_default_xex = false;
            walk_root_table(reader, &volume, |entry| {
                if entry.name_str().eq_ignore_ascii_case("default.xex") {
                    has_default_xex = true;
                }
                Ok(())
            })?;
            Ok(if has_default_xex {
                DetectedConsole::Xenon
            } else {
                DetectedConsole::Xbox
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multilingual_primary_prefers_english() {
        let m = MultilingualString::from_pairs([
            (LanguageCode::Japanese, "ジャパン".to_string()),
            (LanguageCode::English, "England".to_string()),
            (LanguageCode::French, "Francais".to_string()),
        ]);
        assert_eq!(m.primary(), Some("England"));
    }

    #[test]
    fn multilingual_primary_falls_back_to_first() {
        let m = MultilingualString::from_pairs([
            (LanguageCode::German, "Deutsch".to_string()),
            (LanguageCode::French, "Francais".to_string()),
        ]);
        assert_eq!(m.primary(), Some("Deutsch"));
    }

    #[test]
    fn multilingual_from_pairs_drops_empty() {
        let m = MultilingualString::from_pairs([
            (LanguageCode::English, "Hi".to_string()),
            (LanguageCode::German, String::new()),
        ]);
        assert_eq!(m.entries.len(), 1);
    }

    #[test]
    fn detect_chd_by_extension() {
        let r = detect_console(Path::new("/tmp/disc.chd")).unwrap();
        assert_eq!(r, DetectedConsole::Chd);
    }

    #[test]
    fn detect_laserdisc_by_extension() {
        let r = detect_console(Path::new("/tmp/capture.avi")).unwrap();
        assert_eq!(r, DetectedConsole::LaserDisc);
    }

    #[test]
    fn detect_ctr_by_extension() {
        for ext in ["cia", "3ds", "cci", "cxi"] {
            let p = format!("/tmp/x.{}", ext);
            let r = detect_console(Path::new(&p)).unwrap();
            assert_eq!(r, DetectedConsole::Ctr, "ext {} should route to Ctr", ext);
        }
    }

    #[test]
    fn detect_compressed_ctr_by_extension() {
        for ext in ["zcia", "zcci", "zcxi", "z3dsx"] {
            let p = format!("/tmp/x.{}", ext);
            let r = detect_console(Path::new(&p)).unwrap();
            assert_eq!(r, DetectedConsole::Ctr, "ext {} should route to Ctr", ext);
        }
    }

    #[test]
    fn detect_nx_by_extension() {
        for ext in ["nsp", "nsz", "xci", "xcz"] {
            let p = format!("/tmp/x.{}", ext);
            let r = detect_console(Path::new(&p)).unwrap();
            assert_eq!(r, DetectedConsole::Nx, "ext {} should route to Nx", ext);
        }
    }

    #[test]
    fn detect_xbox_and_xenon_by_extension() {
        let r = detect_console(Path::new("/tmp/game.xiso")).unwrap();
        assert_eq!(r, DetectedConsole::Xbox);
        let r = detect_console(Path::new("/tmp/game.zar")).unwrap();
        assert_eq!(r, DetectedConsole::Xenon);
    }

    /// Builds a full 0x800-byte XDVDFS volume descriptor sector.
    fn build_xdvdfs_descriptor(root_sector: u32, root_size: u32) -> Vec<u8> {
        use crate::microsoft::xdvdfs::VOLUME_MAGIC;

        let mut d = vec![0u8; 0x800];
        d[0..20].copy_from_slice(VOLUME_MAGIC);
        d[0x14..0x18].copy_from_slice(&root_sector.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&root_size.to_le_bytes());
        d[0x1C..0x24].copy_from_slice(&0u64.to_le_bytes());
        d[0x7EC..0x800].copy_from_slice(VOLUME_MAGIC);
        d
    }

    /// A single root-level file dirent with no children.
    fn encode_root_file_dirent(name: &[u8]) -> Vec<u8> {
        let mut e = Vec::with_capacity(14 + name.len());
        e.extend_from_slice(&0u16.to_le_bytes()); // left
        e.extend_from_slice(&0u16.to_le_bytes()); // right
        e.extend_from_slice(&0u32.to_le_bytes()); // start_sector
        e.extend_from_slice(&0u32.to_le_bytes()); // size
        e.push(0); // attributes: plain file
        e.push(name.len() as u8);
        e.extend_from_slice(name);
        e
    }

    /// A trimmed (base-0) XDVDFS image whose root directory holds a single
    /// named file, for the Xbox-vs-Xbox-360 disambiguation tests.
    fn build_trimmed_xdvdfs_iso(root_entry_name: &[u8]) -> Vec<u8> {
        use crate::microsoft::xdvdfs::{SECTOR_SIZE, VOLUME_DESCRIPTOR_SECTOR};

        let root_sector = 40u32;
        let mut root = vec![0xFFu8; SECTOR_SIZE as usize];
        let entry = encode_root_file_dirent(root_entry_name);
        root[0..entry.len()].copy_from_slice(&entry);

        let mut image = vec![0u8; ((root_sector as u64 + 1) * SECTOR_SIZE) as usize];
        let descriptor_off = (VOLUME_DESCRIPTOR_SECTOR as u64 * SECTOR_SIZE) as usize;
        image[descriptor_off..descriptor_off + 0x800]
            .copy_from_slice(&build_xdvdfs_descriptor(root_sector, SECTOR_SIZE as u32));
        let root_off = (root_sector as u64 * SECTOR_SIZE) as usize;
        image[root_off..root_off + root.len()].copy_from_slice(&root);
        image
    }

    #[test]
    fn detect_iso_with_default_xbe_root_is_original_xbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        std::fs::write(&path, build_trimmed_xdvdfs_iso(b"DEFAULT.XBE")).unwrap();

        let r = detect_console(&path).unwrap();
        assert_eq!(r, DetectedConsole::Xbox);
    }

    #[test]
    fn detect_iso_with_default_xex_root_is_xbox_360() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        std::fs::write(&path, build_trimmed_xdvdfs_iso(b"DEFAULT.XEX")).unwrap();

        let r = detect_console(&path).unwrap();
        assert_eq!(r, DetectedConsole::Xenon);
    }

    #[test]
    fn read_info_routes_x360_iso_to_xbox_variant() {
        use crate::microsoft::xdvdfs::PartitionKind;
        use std::io::{Seek, Write};

        const XGD2_BASE: u64 = 0x0FD9_0000;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.iso");
        let trimmed = build_trimmed_xdvdfs_iso(b"DEFAULT.XEX");

        let mut file = std::fs::File::create(&path).unwrap();
        file.seek(std::io::SeekFrom::Start(XGD2_BASE)).unwrap();
        file.write_all(&trimmed).unwrap();
        drop(file);

        let opts = InfoOptions::default();
        match read_info(&path, &opts).unwrap() {
            InfoResult::Xbox(info) => assert_eq!(info.kind, PartitionKind::Xgd2),
            other => panic!("expected Xbox variant, got {other:?}"),
        }
    }

    #[test]
    fn detect_psx_by_cue_extension() {
        let r = detect_console(Path::new("/tmp/game.cue")).expect("detect cue");
        assert_eq!(r, DetectedConsole::Psx);
    }

    #[test]
    fn detect_playstation_isos_by_iso9660_probe() {
        use crate::util::iso9660::test_fixtures::{IsoSpec, make_iso};

        let dir = tempfile::tempdir().expect("temp dir");
        for (name, iso, want) in [
            (
                "ps2.iso",
                make_iso(&IsoSpec {
                    system_id: b"PLAYSTATION",
                    volume_sectors: 2_000_000,
                    root_entries: &[(b"SYSTEM.CNF;1", false)],
                    file_content: b"BOOT2 = cdrom0:\\SLUS_203.12;1\r\n",
                }),
                DetectedConsole::Psx,
            ),
            (
                "psp.iso",
                make_iso(&IsoSpec {
                    system_id: b"PSP GAME",
                    volume_sectors: 800_000,
                    root_entries: &[],
                    file_content: &[],
                }),
                DetectedConsole::Psp,
            ),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, iso).expect("write image");
            assert_eq!(
                detect_console(&path).expect("detect console"),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn detect_nds_and_sony_handheld_by_extension() {
        for (ext, want) in [
            ("nds", DetectedConsole::Nds),
            ("dsi", DetectedConsole::Nds),
        ] {
            let p = format!("/tmp/x.{}", ext);
            assert_eq!(
                detect_console(Path::new(&p)).expect("detect console"),
                want,
                "ext {ext}"
            );
        }
    }

    #[test]
    fn info_result_nds_round_trips_via_json() {
        use crate::nintendo::nds::info::NdsSecureAreaState;

        let r = InfoResult::Nds(NdsInfo {
            game_title: "TEST GAME".to_string(),
            game_code: "ARCE".to_string(),
            secure_area: NdsSecureAreaState::Encrypted,
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"kind\":\"nds\""));

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Nds(n) => {
                assert_eq!(n.game_code, "ARCE");
                assert_eq!(n.secure_area, NdsSecureAreaState::Encrypted);
            }
            _ => panic!("expected Nds variant"),
        }
    }

    #[test]
    fn detect_unknown_extension_errors() {
        let err = detect_console(Path::new("/tmp/unknown.bin"));
        assert!(err.is_err());
    }

    #[test]
    fn read_info_propagates_io_error_for_missing_file() {
        let opts = InfoOptions::default();
        let err = read_info(Path::new("/nonexistent.cia"), &opts).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ctr info") || msg.contains("No such file"));
    }

    #[test]
    fn info_result_round_trips_via_json() {
        let r = InfoResult::Chd(ChdInfo {
            version: 5,
            physical_bytes: 12345,
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();
        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Chd(c) => assert_eq!(c.physical_bytes, 12345),
            _ => panic!("expected Chd variant"),
        }
    }

    #[test]
    fn info_result_xbox_round_trips_via_json() {
        use crate::microsoft::xdvdfs::PartitionKind;

        let r = InfoResult::Xbox(XisoInfo {
            kind: PartitionKind::Trimmed,
            base: 0,
            root_sector: 40,
            root_size: 2048,
            file_count: 3,
            dir_count: 1,
            total_file_bytes: 12345,
            image_size: 999_999,
            xbe: Some(crate::microsoft::xbox::XbeInfo {
                title_name: "Test Game".to_string(),
                icon: Some(Image::new(vec![0x89, b'P', b'N', b'G'], 128, 128)),
                ..Default::default()
            }),
            xex: None,
            root_entries: Vec::new(),
        });
        let s = serde_json::to_string(&r).unwrap();
        // The variant tag and the struct's own (renamed) kind field must
        // not collide on the wire.
        assert!(s.contains("\"kind\":\"xbox\""));
        assert!(s.contains("\"partition_kind\":\"trimmed\""));

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Xbox(x) => {
                assert_eq!(x.kind, PartitionKind::Trimmed);
                assert_eq!(x.file_count, 3);
                let xbe = x.xbe.expect("xbe");
                assert_eq!(xbe.title_name, "Test Game");
                assert_eq!(xbe.icon.expect("icon").width, 128);
                assert!(x.xex.is_none());
            }
            _ => panic!("expected Xbox variant"),
        }
    }

    #[test]
    fn info_result_xenon_round_trips_via_json() {
        let r = InfoResult::Xenon(ZarInfo {
            file_count: 2,
            dir_count: 1,
            logical_size: 100,
            compressed_size: 50,
            block_count: 4,
            has_default_xex: true,
            xex: None,
            root_entries: Vec::new(),
        });
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"kind\":\"xenon\""));

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Xenon(z) => {
                assert_eq!(z.logical_size, 100);
                assert!(z.has_default_xex);
                assert!(z.xex.is_none());
            }
            _ => panic!("expected Xenon variant"),
        }
    }

    #[test]
    fn info_result_nx_round_trips_via_json() {
        use crate::nintendo::nx::info::NxFullInfo;

        const ID: u64 = 0x01AB_CDEF_0123_4801;

        let r = InfoResult::Nx(NxInfo {
            full: Some(NxFullInfo {
                application_title_id: ID,
                application_title_id_hex: format!("{:016X}", ID),
                ..Default::default()
            }),
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"application_title_id_hex\":\"01ABCDEF01234801\""));

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Nx(nx) => {
                let full = nx.full.expect("full info");
                assert_eq!(full.application_title_id, ID);
                assert_eq!(full.application_title_id_hex, "01ABCDEF01234801");
            }
            _ => panic!("expected Nx variant"),
        }
    }

    #[test]
    fn info_result_dol_round_trips_via_json() {
        use crate::nintendo::dol::info::DolFstEntry;

        let r = InfoResult::Dol(DolInfo {
            game_id: "GALE01".to_string(),
            fst_root: vec![DolFstEntry {
                name: "opening.bnr".to_string(),
                size: 1496,
                is_dir: false,
            }],
            fst_file_count: 1,
            fst_dir_count: 0,
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"fst_root\""));

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Dol(d) => {
                assert_eq!(d.fst_root.len(), 1);
                assert_eq!(d.fst_root[0].name, "opening.bnr");
                assert_eq!(d.fst_file_count, 1);
            }
            _ => panic!("expected Dol variant"),
        }
    }

    #[test]
    fn info_result_ctr_round_trips_via_json() {
        use crate::nintendo::ctr::info::{CtrContentEntry, CtrPartitionEntry};

        let r = InfoResult::Ctr(CtrInfo {
            ncsd_partitions: vec![CtrPartitionEntry {
                index: 0,
                name: "Game".to_string(),
                offset: 0x4000,
                size: 0x100000,
            }],
            cia_contents: vec![CtrContentEntry {
                index: 0,
                content_id: "00000000".to_string(),
                size: 12345,
                encrypted: true,
            }],
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Ctr(c) => {
                assert_eq!(c.ncsd_partitions.len(), 1);
                assert_eq!(c.ncsd_partitions[0].name, "Game");
                assert_eq!(c.cia_contents.len(), 1);
                assert_eq!(c.cia_contents[0].content_id, "00000000");
            }
            _ => panic!("expected Ctr variant"),
        }
    }

    #[test]
    fn info_result_wup_round_trips_via_json() {
        use crate::nintendo::wup::info::WupDiscPartition;

        let r = InfoResult::Wup(WupInfo {
            disc_partitions: vec![WupDiscPartition {
                name: "GM12345678".to_string(),
                kind: "Game".to_string(),
                start_sector: 40,
            }],
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Wup(w) => {
                assert_eq!(w.disc_partitions.len(), 1);
                assert_eq!(w.disc_partitions[0].name, "GM12345678");
                assert_eq!(w.disc_partitions[0].kind, "Game");
            }
            _ => panic!("expected Wup variant"),
        }
    }

    #[test]
    fn info_result_ps3_round_trips_via_json() {
        use crate::ps3::info::Ps3RootEntry;

        let r = InfoResult::Ps3(Ps3Info {
            icon: Some(Image::new(vec![0x89, b'P', b'N', b'G'], 128, 128)),
            root_files: vec![Ps3RootEntry {
                name: "PS3_GAME".to_string(),
                size: 2048,
                is_dir: true,
            }],
            encrypted: Some(true),
            ..Default::default()
        });
        let s = serde_json::to_string(&r).unwrap();

        let back: InfoResult = serde_json::from_str(&s).unwrap();
        match back {
            InfoResult::Ps3(p) => {
                let icon = p.icon.expect("icon");
                assert_eq!(icon.width, 128);
                assert_eq!(p.root_files.len(), 1);
                assert_eq!(p.root_files[0].name, "PS3_GAME");
                assert_eq!(p.encrypted, Some(true));
            }
            _ => panic!("expected Ps3 variant"),
        }
    }
}
