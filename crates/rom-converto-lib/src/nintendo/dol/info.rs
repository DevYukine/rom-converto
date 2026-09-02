//! GameCube disc metadata extraction: boot.bin fields plus the parsed
//! `opening.bnr` banner and its decoded image, for the `dol info` command.

use crate::info::Image;
use crate::nintendo::dol::fst::{FstNode, find_file, list_files};
use crate::nintendo::dol::models::banner::{BANNER_IMAGE_HEIGHT, BANNER_IMAGE_WIDTH, GcBanner};
use crate::nintendo::dol::models::boot_bin::GcBootBin;
use crate::util::pixel::{decode_rgb5a3_tiled, encode_png};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Cap on the number of entries returned in [`DolInfo::fst_root`], to keep
/// the info payload small for discs with very large root directories.
const FST_ROOT_CAP: usize = 64;

/// Metadata read from a GameCube disc image: boot.bin fields plus the
/// decoded banner, if present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DolInfo {
    pub physical_bytes: u64,
    pub container: String,
    pub game_id: String,
    pub maker_code: String,
    pub maker_name: Option<String>,
    pub disc_number: u8,
    pub disc_version: u8,
    pub audio_streaming: bool,
    pub game_name: String,
    pub region: String,
    pub apploader_date: Option<String>,
    pub banner: Option<GcBannerInfo>,
    pub banner_image: Option<Image>,
    #[serde(default)]
    pub fst_root: Vec<DolFstEntry>,
    #[serde(default)]
    pub fst_file_count: u32,
    #[serde(default)]
    pub fst_dir_count: u32,
}

/// One top-level entry of the disc's file layout (a path with no `/`),
/// as listed from the FST.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DolFstEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Decoded `opening.bnr` banner, with all title blocks it carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcBannerInfo {
    pub format: String,
    pub titles: Vec<GcBannerTitleInfo>,
}

/// One language block of a banner: short/long game and maker names plus
/// the description text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcBannerTitleInfo {
    pub language: String,
    pub short_game_name: String,
    pub short_maker: String,
    pub long_game_name: String,
    pub long_maker: String,
    pub description: String,
}

/// Reads boot.bin and, if present, the `opening.bnr` banner from a
/// GameCube disc image at `path`. Banner read failures are logged and
/// treated as absent rather than propagated, since not all discs carry one.
pub fn read_info(path: &Path) -> Result<DolInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("dol info: stat {}", path.display()))?
        .len();

    let mut reader = crate::nintendo::disc_input::open_disc_input(path)
        .with_context(|| format!("dol info: open {}", path.display()))?;
    let container = reader.container_name().to_string();

    let boot = GcBootBin::read(&mut reader).context("dol info: parse boot.bin")?;

    let fst_bytes = read_fst_bytes(&mut reader, &boot).unwrap_or_else(|e| {
        log::debug!("dol info: fst read skipped ({})", e);
        None
    });

    let (fst_root, fst_file_count, fst_dir_count) = match fst_bytes.as_deref() {
        Some(fst) => fst_summary(fst).unwrap_or_else(|e| {
            log::debug!("dol info: fst listing skipped ({})", e);
            Default::default()
        }),
        None => Default::default(),
    };

    let (banner, banner_image) = match fst_bytes.as_deref() {
        Some(fst) => read_banner(&mut reader, fst).unwrap_or_else(|e| {
            log::debug!("dol info: banner read skipped ({})", e);
            (None, None)
        }),
        None => (None, None),
    };

    let maker_name =
        crate::util::maker_codes::lookup_maker(&boot.maker_code).map(|s| s.to_string());

    Ok(DolInfo {
        physical_bytes,
        container,
        game_id: boot.game_id,
        maker_name,
        maker_code: boot.maker_code,
        disc_number: boot.disc_number,
        disc_version: boot.disc_version,
        audio_streaming: boot.audio_streaming,
        game_name: boot.game_name,
        region: format!("{:?}", boot.region),
        apploader_date: boot.apploader_date,
        banner,
        banner_image,
        fst_root,
        fst_file_count,
        fst_dir_count,
    })
}

/// Reads the raw FST blob referenced by `boot`, if it carries valid
/// geometry. Returns `None` rather than erroring when a disc has no FST.
fn read_fst_bytes<R: Read + Seek>(reader: &mut R, boot: &GcBootBin) -> Result<Option<Vec<u8>>> {
    if boot.fst_size == 0 || boot.fst_offset == 0 {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(boot.fst_offset as u64))?;
    let mut fst = vec![0u8; boot.fst_size as usize];
    reader.read_exact(&mut fst)?;
    Ok(Some(fst))
}

/// Summarizes an FST into its top-level entries (capped at
/// [`FST_ROOT_CAP`]) plus total file and directory counts.
fn fst_summary(fst: &[u8]) -> Result<(Vec<DolFstEntry>, u32, u32)> {
    let mut root = Vec::new();
    let mut file_count = 0u32;
    let mut dir_count = 0u32;
    for node in list_files(fst)? {
        match node {
            FstNode::File { path, size, .. } => {
                file_count += 1;
                if !path.contains('/') && root.len() < FST_ROOT_CAP {
                    root.push(DolFstEntry {
                        name: path,
                        size,
                        is_dir: false,
                    });
                }
            }
            FstNode::Directory { path } => {
                dir_count += 1;
                if !path.contains('/') && root.len() < FST_ROOT_CAP {
                    root.push(DolFstEntry {
                        name: path,
                        size: 0,
                        is_dir: true,
                    });
                }
            }
        }
    }
    Ok((root, file_count, dir_count))
}

fn read_banner<R: Read + Seek>(
    reader: &mut R,
    fst: &[u8],
) -> Result<(Option<GcBannerInfo>, Option<Image>)> {
    let Some((bnr_offset, bnr_size)) = find_file(fst, "opening.bnr")? else {
        return Ok((None, None));
    };

    reader.seek(SeekFrom::Start(bnr_offset))?;
    let mut bnr = vec![0u8; bnr_size as usize];
    reader.read_exact(&mut bnr)?;
    let banner = GcBanner::parse(&bnr)?;

    let image = decode_rgb5a3_tiled(&banner.image_raw, BANNER_IMAGE_WIDTH, BANNER_IMAGE_HEIGHT)
        .ok()
        .and_then(|rgba| encode_png(&rgba, BANNER_IMAGE_WIDTH, BANNER_IMAGE_HEIGHT).ok())
        .map(|png| Image::new(png, BANNER_IMAGE_WIDTH, BANNER_IMAGE_HEIGHT));

    let info = GcBannerInfo {
        format: format!("{:?}", banner.format),
        titles: banner
            .titles
            .into_iter()
            .map(|t| GcBannerTitleInfo {
                language: format!("{:?}", t.language),
                short_game_name: t.short_game_name,
                short_maker: t.short_maker,
                long_game_name: t.long_game_name,
                long_maker: t.long_maker,
                description: t.description,
            })
            .collect(),
    };

    Ok((Some(info), image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::dol::test_fixtures::{
        make_fake_gamecube_iso, make_fake_gamecube_iso_with_fst,
    };
    use crate::nintendo::gcz::test_fixtures::make_gcz;
    use std::io::Write;

    #[test]
    fn info_reports_container() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(0x40000);

        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &original).unwrap();
        assert_eq!(read_info(&iso).unwrap().container, "ISO");

        let gcz = dir.path().join("game.gcz");
        let mut f = std::fs::File::create(&gcz).unwrap();
        f.write_all(&make_gcz(&original, 0x8000, 0)).unwrap();
        drop(f);
        assert_eq!(read_info(&gcz).unwrap().container, "GCZ");
    }

    #[test]
    fn info_reports_fst_root_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso_with_fst(0x40000);
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &original).unwrap();

        let info = read_info(&iso).unwrap();
        assert_eq!(info.fst_file_count, 2);
        assert_eq!(info.fst_dir_count, 1);
        assert_eq!(info.fst_root.len(), 2);
        assert!(
            info.fst_root
                .iter()
                .any(|e| e.name == "opening.bnr" && !e.is_dir)
        );
        assert!(info.fst_root.iter().any(|e| e.name == "sub" && e.is_dir));
    }
}
