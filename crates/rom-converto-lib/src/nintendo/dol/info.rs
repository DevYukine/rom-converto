//! GameCube disc metadata extraction: boot.bin fields plus the parsed
//! `opening.bnr` banner and its decoded image, for the `dol info` command.

use crate::info::Image;
use crate::nintendo::dol::fst::FST_ENTRY_SIZE;
use crate::nintendo::dol::models::banner::{BANNER_IMAGE_HEIGHT, BANNER_IMAGE_WIDTH, GcBanner};
use crate::nintendo::dol::models::boot_bin::{GcBootBin, GcRegion};
use crate::util::bytes::cstr_shift_jis;
use crate::util::extent_end;
use crate::util::pixel::{decode_rgb5a3_tiled, encode_png};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Cap on the number of entries returned in [`DolInfo::fst_root`], to keep
/// the info payload small for discs with very large root directories.
const FST_ROOT_CAP: usize = 64;
type FstMetadata = (Vec<DolFstEntry>, u32, u32, Option<(u64, u64)>);

/// Metadata read from a GameCube disc image: boot.bin fields plus the
/// decoded banner, if present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
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
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct DolFstEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Decoded `opening.bnr` banner, with all title blocks it carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct GcBannerInfo {
    pub format: String,
    pub titles: Vec<GcBannerTitleInfo>,
}

/// One language block of a banner: short/long game and maker names plus
/// the description text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
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

    let mut reader = crate::nintendo::disc::input::open_disc_input_with_lookahead(path, 1)
        .with_context(|| format!("dol info: open {}", path.display()))?;
    let container = reader.container_name().to_string();

    let boot = GcBootBin::read(&mut reader).context("dol info: parse boot.bin")?;

    let fst = read_fst_metadata(&mut reader, &boot).unwrap_or_else(|e| {
        log::debug!("dol info: fst read skipped ({})", e);
        None
    });
    let (fst_root, fst_file_count, fst_dir_count, banner_extent) = fst.unwrap_or_default();
    let (banner, banner_image) = match banner_extent {
        Some((offset, size)) => {
            read_banner(&mut reader, offset, size, boot.region).unwrap_or_else(|e| {
                log::debug!("dol info: banner read skipped ({})", e);
                (None, None)
            })
        }
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

/// Read only FST records and names that are surfaced in info. Nested entries
/// still contribute to summary counts but do not materialize full paths.
fn read_fst_metadata<R: Read + Seek>(
    reader: &mut R,
    boot: &GcBootBin,
) -> Result<Option<FstMetadata>> {
    if boot.fst_size == 0 || boot.fst_offset == 0 {
        return Ok(None);
    }
    let fst_start = boot.fst_offset as u64;
    let fst_size = boot.fst_size as u64;
    let file_len = reader.seek(SeekFrom::End(0))?;
    if extent_end(fst_start, fst_size, file_len).is_none() || fst_size < FST_ENTRY_SIZE as u64 {
        anyhow::bail!("FST extent is outside the logical disc");
    }
    let mut root_record = [0u8; FST_ENTRY_SIZE];
    reader.seek(SeekFrom::Start(fst_start))?;
    reader.read_exact(&mut root_record)?;
    if root_record[0] != 1 {
        anyhow::bail!("FST root entry is not a directory");
    }
    let total_entries = u32::from_be_bytes(root_record[8..12].try_into()?) as usize;
    let records_size = total_entries
        .checked_mul(FST_ENTRY_SIZE)
        .context("FST record count overflows")? as u64;
    if total_entries == 0 || records_size > fst_size {
        anyhow::bail!("FST entry table exceeds its declared extent");
    }
    let string_start = fst_start + records_size;
    let string_size = fst_size - records_size;
    let string_table = if string_size <= 64 * 1024 {
        let mut table = vec![0; string_size as usize];
        reader.seek(SeekFrom::Start(string_start))?;
        reader.read_exact(&mut table)?;
        reader.seek(SeekFrom::Start(fst_start + FST_ENTRY_SIZE as u64))?;
        Some(table)
    } else {
        None
    };

    let mut root = Vec::new();
    let mut file_count = 0u32;
    let mut dir_count = 0u32;
    let mut banner = None;
    let mut dir_ends = vec![total_entries];
    let mut name_scratch = [0u8; 4 * 1024];
    const ENTRIES_PER_CHUNK: usize = (64 * 1024) / FST_ENTRY_SIZE;
    let mut entries = [0u8; ENTRIES_PER_CHUNK * FST_ENTRY_SIZE];
    let mut idx = 1usize;
    while idx < total_entries {
        let chunk_entries = (total_entries - idx).min(ENTRIES_PER_CHUNK);
        let chunk_size = chunk_entries * FST_ENTRY_SIZE;
        reader.read_exact(&mut entries[..chunk_size])?;
        let mut read_name = false;
        for entry_idx in 0..chunk_entries {
            let current_idx = idx + entry_idx;
            while dir_ends.len() > 1 && current_idx >= *dir_ends.last().expect("root remains") {
                dir_ends.pop();
            }
            let entry = &entries[entry_idx * FST_ENTRY_SIZE..(entry_idx + 1) * FST_ENTRY_SIZE];
            let is_dir = entry[0] != 0;
            let top_level = dir_ends.len() == 1;
            if is_dir {
                dir_count += 1;
            } else {
                file_count += 1;
            }

            if is_dir {
                let end = u32::from_be_bytes(entry[8..12].try_into()?) as usize;
                if top_level && root.len() < FST_ROOT_CAP {
                    let name_offset = u32::from_be_bytes([0, entry[1], entry[2], entry[3]]) as u64;
                    let name = read_fst_name(
                        reader,
                        string_table.as_deref(),
                        string_start,
                        string_size,
                        name_offset,
                        &mut name_scratch,
                    )?;
                    read_name |= string_table.is_none();
                    root.push(DolFstEntry {
                        name,
                        size: 0,
                        is_dir: true,
                    });
                }
                dir_ends.push(end);
            } else if top_level {
                let offset = u32::from_be_bytes(entry[4..8].try_into()?) as u64;
                let size = u32::from_be_bytes(entry[8..12].try_into()?) as u64;
                let retained = root.len() < FST_ROOT_CAP;
                if retained || banner.is_none() {
                    let name_offset = u32::from_be_bytes([0, entry[1], entry[2], entry[3]]) as u64;
                    let name = read_fst_name(
                        reader,
                        string_table.as_deref(),
                        string_start,
                        string_size,
                        name_offset,
                        &mut name_scratch,
                    )?;
                    read_name |= string_table.is_none();
                    if name == "opening.bnr" {
                        banner = Some((offset, size));
                    }
                    if retained {
                        root.push(DolFstEntry {
                            name,
                            size,
                            is_dir: false,
                        });
                    }
                }
            }
        }
        if read_name {
            let next_entry = idx + chunk_entries;
            reader.seek(SeekFrom::Start(
                fst_start + (next_entry * FST_ENTRY_SIZE) as u64,
            ))?;
        }
        idx += chunk_entries;
    }
    Ok(Some((root, file_count, dir_count, banner)))
}

fn read_fst_name<R: Read + Seek>(
    reader: &mut R,
    string_table: Option<&[u8]>,
    strings_start: u64,
    strings_len: u64,
    offset: u64,
    scratch: &mut [u8; 4 * 1024],
) -> Result<String> {
    if offset >= strings_len {
        return Ok(String::new());
    }
    if let Some(table) = string_table {
        return Ok(cstr_shift_jis(&table[offset as usize..]));
    }

    let mut name = Vec::new();
    let mut consumed = 0u64;
    while consumed < strings_len - offset {
        let read_len = (strings_len - offset - consumed).min(scratch.len() as u64) as usize;
        reader.seek(SeekFrom::Start(strings_start + offset + consumed))?;
        reader.read_exact(&mut scratch[..read_len])?;
        let chunk = &scratch[..read_len];
        if let Some(nul) = chunk.iter().position(|&byte| byte == 0) {
            name.extend_from_slice(&chunk[..nul]);
            break;
        }
        name.extend_from_slice(chunk);
        consumed += read_len as u64;
    }
    Ok(cstr_shift_jis(&name))
}

fn read_banner<R: Read + Seek>(
    reader: &mut R,
    bnr_offset: u64,
    bnr_size: u64,
    region: GcRegion,
) -> Result<(Option<GcBannerInfo>, Option<Image>)> {
    use crate::nintendo::dol::models::banner::{
        BNR1_FILE_SIZE, BNR1_MAGIC, BNR2_FILE_SIZE, BNR2_MAGIC,
    };
    if bnr_size < 4 {
        anyhow::bail!("opening.bnr is too small");
    }
    reader.seek(SeekFrom::End(0))?;
    let file_len = reader.stream_position()?;
    if extent_end(bnr_offset, bnr_size, file_len).is_none() {
        anyhow::bail!("opening.bnr extent exceeds the logical disc");
    }
    reader.seek(SeekFrom::Start(bnr_offset))?;
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    let expected = match magic {
        BNR1_MAGIC => BNR1_FILE_SIZE,
        BNR2_MAGIC => BNR2_FILE_SIZE,
        _ => anyhow::bail!("opening.bnr has unknown magic"),
    };
    if bnr_size < expected as u64 {
        anyhow::bail!("opening.bnr is truncated");
    }
    reader.seek(SeekFrom::Start(bnr_offset))?;
    let mut bnr = vec![0u8; expected];
    reader.read_exact(&mut bnr)?;
    let banner = GcBanner::parse(&bnr, region)?;

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
    use crate::nintendo::disc::gcz::test_fixtures::make_gcz;
    use crate::nintendo::dol::test_fixtures::{
        make_fake_gamecube_iso, make_fake_gamecube_iso_with_fst,
    };
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

    #[test]
    fn banner_reads_only_the_format_defined_extent() {
        use crate::nintendo::dol::models::banner::{BNR1_FILE_SIZE, BNR1_MAGIC};
        let mut disc = vec![0u8; BNR1_FILE_SIZE + 4096];
        disc[0..4].copy_from_slice(&BNR1_MAGIC);
        let mut reader = std::io::Cursor::new(disc);

        let (info, _) = read_banner(
            &mut reader,
            0,
            (BNR1_FILE_SIZE + 4096) as u64,
            GcRegion::Usa,
        )
        .unwrap();
        assert_eq!(info.unwrap().titles.len(), 1);
        assert_eq!(reader.position(), BNR1_FILE_SIZE as u64);
    }
    #[test]
    fn fst_name_longer_than_four_kibibytes_is_preserved() {
        let mut strings = vec![b'a'; 100 * 1024];
        strings[10_000] = 0;
        let mut reader = std::io::Cursor::new(strings);
        let name = read_fst_name(&mut reader, None, 0, 100 * 1024, 0, &mut [0; 4 * 1024]).unwrap();
        assert_eq!(name.len(), 10_000);
        assert!(name.bytes().all(|byte| byte == b'a'));
        assert!(reader.position() <= 3 * 4 * 1024);
    }

    #[test]
    fn fst_name_decodes_shift_jis_across_chunk_boundary() {
        let mut strings = vec![b'a'; 4095];
        strings.extend_from_slice(&[0x82, 0xad, 0x00]);
        let expected = format!("{}く", "a".repeat(4095));

        let name = read_fst_name(
            &mut std::io::Cursor::new(strings.clone()),
            Some(strings.as_slice()),
            0,
            strings.len() as u64,
            0,
            &mut [0; 4 * 1024],
        )
        .unwrap();
        assert_eq!(name, expected);

        let name = read_fst_name(
            &mut std::io::Cursor::new(strings.clone()),
            None,
            0,
            strings.len() as u64,
            0,
            &mut [0; 4 * 1024],
        )
        .unwrap();
        assert_eq!(name, expected);
    }

    #[test]
    fn fst_names_use_entry_offsets_and_banner_is_root_only() {
        fn entry(kind: u8, name_offset: u32, first: u32, second: u32) -> [u8; FST_ENTRY_SIZE] {
            let mut entry = [0; FST_ENTRY_SIZE];
            entry[0] = kind;
            entry[1..4].copy_from_slice(&name_offset.to_be_bytes()[1..]);
            entry[4..8].copy_from_slice(&first.to_be_bytes());
            entry[8..12].copy_from_slice(&second.to_be_bytes());
            entry
        }

        let strings = b"xopening.bnr\0sub\0opening.bnr\0";
        let records = [
            entry(1, 0, 0, 5),
            entry(0, 0, 0x1111, 0x10),
            entry(0, 1, 0x1234, 0x56),
            entry(1, 13, 0, 5),
            entry(0, 17, 0x9999, 0x20),
        ];
        let fst_start = 32u32;
        let mut disc = vec![0; fst_start as usize];
        for record in records {
            disc.extend_from_slice(&record);
        }
        disc.extend_from_slice(strings);
        let boot = GcBootBin {
            game_id: String::new(),
            maker_code: String::new(),
            disc_number: 0,
            disc_version: 0,
            audio_streaming: false,
            stream_buffer_size: 0,
            game_name: String::new(),
            region: crate::nintendo::dol::models::boot_bin::GcRegion::Unknown(0),
            fst_offset: fst_start,
            fst_size: (records.len() * FST_ENTRY_SIZE + strings.len()) as u32,
            apploader_date: None,
        };

        let mut reader = std::io::Cursor::new(disc);
        let (root, file_count, dir_count, banner) =
            read_fst_metadata(&mut reader, &boot).unwrap().unwrap();
        assert_eq!(
            root.iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["xopening.bnr", "opening.bnr", "sub"]
        );
        assert_eq!((file_count, dir_count), (3, 1));
        assert_eq!(banner, Some((0x1234, 0x56)));
    }
}
