//! Cross-console ROM metadata extraction (the `info` feature).
//!
//! Per-console extractors live alongside their parsers (e.g. `crate::chd::info`).
//! This module owns the umbrella [`InfoResult`] sum type, the shared
//! [`Image`] / [`MultilingualString`] / [`LanguageCode`] types, and a
//! top-level [`read_info`] dispatcher that the GUI uses to read any
//! supported file without knowing its format in advance.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod image;

pub use crate::chd::info::ChdInfo;
pub use crate::cso::info::CsoInfo;
pub use crate::nintendo::ctr::info::CtrInfo;
pub use crate::nintendo::dol::info::DolInfo;
pub use crate::nintendo::nx::info::NxInfo;
pub use crate::nintendo::rvl::info::RvlInfo;
pub use crate::nintendo::wup::info::WupInfo;
pub use image::Image;

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MultilingualString {
    pub entries: Vec<(LanguageCode, String)>,
}

impl MultilingualString {
    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (LanguageCode, String)>,
    {
        let mut entries: Vec<(LanguageCode, String)> = pairs.into_iter().collect();
        entries.retain(|(_, s)| !s.is_empty());
        Self { entries }
    }

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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedConsole {
    Chd,
    Cso,
    Ctr,
    Dol,
    Rvl,
    Wup,
    Nx,
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
        Some("cia") | Some("3ds") | Some("cci") | Some("cxi") | Some("ncch") => {
            return Ok(DetectedConsole::Ctr);
        }
        Some("nsp") | Some("nsz") | Some("xci") | Some("xcz") => return Ok(DetectedConsole::Nx),
        Some("wud") | Some("wux") | Some("wua") => return Ok(DetectedConsole::Wup),
        Some("gcm") => return Ok(DetectedConsole::Dol),
        Some("wbfs") => return Ok(DetectedConsole::Rvl),
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

    Err(anyhow!(
        "disc file at {} does not match GameCube or Wii magic",
        path.display()
    ))
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
    fn detect_ctr_by_extension() {
        for ext in ["cia", "3ds", "cci", "cxi"] {
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
}
