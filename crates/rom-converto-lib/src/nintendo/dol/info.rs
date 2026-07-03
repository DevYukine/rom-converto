use crate::info::Image;
use crate::nintendo::dol::fst::find_file;
use crate::nintendo::dol::models::banner::{BANNER_IMAGE_HEIGHT, BANNER_IMAGE_WIDTH, GcBanner};
use crate::nintendo::dol::models::boot_bin::GcBootBin;
use crate::util::pixel::{decode_rgb5a3_tiled, encode_png};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcBannerInfo {
    pub format: String,
    pub titles: Vec<GcBannerTitleInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcBannerTitleInfo {
    pub language: String,
    pub short_game_name: String,
    pub short_maker: String,
    pub long_game_name: String,
    pub long_maker: String,
    pub description: String,
}

pub fn read_info(path: &Path) -> Result<DolInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("dol info: stat {}", path.display()))?
        .len();

    let mut reader = crate::nintendo::disc_input::open_disc_input(path)
        .with_context(|| format!("dol info: open {}", path.display()))?;
    let container = reader.container_name().to_string();

    let boot = GcBootBin::read(&mut reader).context("dol info: parse boot.bin")?;

    let banner_info = read_banner(&mut reader, &boot).unwrap_or_else(|e| {
        log::debug!("dol info: banner read skipped ({})", e);
        (None, None)
    });
    let (banner, banner_image) = banner_info;

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
    })
}

fn read_banner<R: Read + Seek>(
    reader: &mut R,
    boot: &GcBootBin,
) -> Result<(Option<GcBannerInfo>, Option<Image>)> {
    if boot.fst_size == 0 || boot.fst_offset == 0 {
        return Ok((None, None));
    }
    reader.seek(SeekFrom::Start(boot.fst_offset as u64))?;
    let mut fst = vec![0u8; boot.fst_size as usize];
    reader.read_exact(&mut fst)?;

    let Some((bnr_offset, bnr_size)) = find_file(&fst, "opening.bnr")? else {
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
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
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
}
