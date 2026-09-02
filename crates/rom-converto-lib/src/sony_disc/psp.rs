//! PSP UMD metadata from `PSP_GAME/PARAM.SFO`, plus the game's
//! `ICON0.PNG` and `PIC1.PNG` when the disc carries them.

use std::io;

use serde::{Deserialize, Serialize};

use crate::info::Image;
use crate::util::iso9660::{SectorSource, Volume};
use crate::util::sfo::Sfo;

/// Metadata read from a PSP UMD image.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PspInfo {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub version: Option<String>,
    pub firmware: Option<String>,
    pub category: Option<String>,
    pub total_sectors: u64,
    /// Logical size of the disc, not of the file or container it came in.
    pub size_bytes: u64,
    pub icon: Option<Image>,
    #[serde(default)]
    pub background: Option<Image>,
}

pub(crate) fn read<S: SectorSource>(src: &mut S, volume: &Volume) -> io::Result<PspInfo> {
    let mut info = PspInfo {
        total_sectors: volume.total_sectors,
        size_bytes: volume.size_bytes(),
        ..Default::default()
    };

    if let Some(sfo) = volume
        .read_file(src, "PSP_GAME/PARAM.SFO")?
        .as_deref()
        .and_then(|bytes| Sfo::parse(bytes).ok())
    {
        info.title = sfo.get_str("TITLE").map(str::to_string);
        info.title_id = sfo.get_str("DISC_ID").map(str::to_string);
        info.version = sfo.get_str("DISC_VERSION").map(str::to_string);
        info.firmware = sfo.get_str("PSP_SYSTEM_VER").map(str::to_string);
        info.category = sfo.get_str("CATEGORY").map(str::to_string);
    }
    info.icon = volume
        .read_file(src, "PSP_GAME/ICON0.PNG")?
        .and_then(Image::from_png);
    info.background = volume
        .read_file(src, "PSP_GAME/PIC1.PNG")?
        .and_then(Image::from_png);

    Ok(info)
}
