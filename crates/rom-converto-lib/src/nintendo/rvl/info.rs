use crate::info::{Image, MultilingualString};
use crate::nintendo::rvl::constants::{
    WII_MAGIC, WII_MAGIC_OFFSET, WII_PARTITION_HEADER_TMD_SIZE_OFFSET,
};
use crate::nintendo::rvl::disc::{WiiPartitionEntry, read_partition_table};
use crate::nintendo::rvl::fst::find_file;
use crate::nintendo::rvl::models::banner_bin::{maybe_decompress_lz77_ascii, strip_imd5};
use crate::nintendo::rvl::models::imet::ImetHeader;
use crate::nintendo::rvl::models::tmd::WiiTmd;
use crate::nintendo::rvl::models::u8_archive::U8Archive;
use crate::nintendo::rvl::partition::read_partition_info;
use crate::nintendo::rvl::partition_reader::PartitionPayloadReader;
use crate::util::pixel::{decode_rgb5a3_tiled, decode_rgba32_tiled, encode_png};
use anyhow::{Context, Result, anyhow};
use byteorder::BE as BE_;
use byteorder::{BE, ReadBytesExt};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RvlInfo {
    pub physical_bytes: u64,
    pub game_id: String,
    pub maker_code: String,
    pub maker_name: Option<String>,
    pub disc_number: u8,
    pub disc_version: u8,
    pub game_name: String,
    pub region: String,
    pub partitions: Vec<RvlPartitionSummary>,
    pub tmd: Option<RvlTmdInfo>,
    pub imet_names: Option<MultilingualString>,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RvlPartitionSummary {
    pub offset: u64,
    pub partition_type: u32,
    pub group: u8,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RvlTmdInfo {
    pub title_id: u64,
    pub title_version: u16,
    pub system_version: u64,
    pub ios_slot: Option<u32>,
    pub region_name: String,
    pub content_count: u16,
    pub access_rights: u32,
}

pub fn read_info(path: &Path) -> Result<RvlInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("rvl info: stat {}", path.display()))?
        .len();

    let mut reader = crate::nintendo::disc_input::open_disc_input(path)
        .map_err(|e| anyhow!("rvl info: open input: {}", e))?;

    let disc_header = read_disc_header(&mut reader)?;

    let entries =
        read_partition_table(&mut reader).map_err(|e| anyhow!("partition table: {}", e))?;
    let partitions: Vec<RvlPartitionSummary> = entries
        .iter()
        .map(|e| RvlPartitionSummary {
            offset: e.offset,
            partition_type: e.partition_type,
            group: e.group,
            kind: partition_kind_name(e.partition_type).to_string(),
        })
        .collect();

    let data_partition = entries.iter().find(|e| e.partition_type == 0).copied();
    let (tmd, imet_names, image) = try_read_data_partition_extras(&mut reader, data_partition)
        .unwrap_or_else(|e| {
            log::debug!("rvl info: data partition extras skipped ({})", e);
            (None, None, None)
        });

    let maker_name =
        crate::util::maker_codes::lookup_maker(&disc_header.maker_code).map(|s| s.to_string());

    Ok(RvlInfo {
        physical_bytes,
        game_id: disc_header.game_id,
        maker_name,
        maker_code: disc_header.maker_code,
        disc_number: disc_header.disc_number,
        disc_version: disc_header.disc_version,
        game_name: disc_header.game_name,
        region: disc_header.region.to_string(),
        partitions,
        tmd,
        imet_names,
        image,
    })
}

struct DiscHeader {
    game_id: String,
    maker_code: String,
    disc_number: u8,
    disc_version: u8,
    game_name: String,
    region: WiiRegion,
}

#[derive(Debug, Clone, Copy)]
enum WiiRegion {
    Japan,
    Usa,
    Pal,
    Korea,
    RegionFree,
    Unknown(u32),
}

impl std::fmt::Display for WiiRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Japan => write!(f, "Japan"),
            Self::Usa => write!(f, "USA"),
            Self::Pal => write!(f, "PAL"),
            Self::Korea => write!(f, "Korea"),
            Self::RegionFree => write!(f, "RegionFree"),
            Self::Unknown(n) => write!(f, "Unknown({})", n),
        }
    }
}

fn read_disc_header<R: Read + Seek>(reader: &mut R) -> Result<DiscHeader> {
    let mut id = [0u8; 6];
    reader.seek(SeekFrom::Start(0))?;
    reader.read_exact(&mut id)?;
    let game_id = read_ascii_trim(&id);
    let maker_code = String::from_utf8_lossy(&id[4..6]).into_owned();

    reader.seek(SeekFrom::Start(0x06))?;
    let disc_number = reader.read_u8()?;
    let disc_version = reader.read_u8()?;

    reader.seek(SeekFrom::Start(WII_MAGIC_OFFSET as u64))?;
    let magic = reader.read_u32::<BE>()?;
    if magic != WII_MAGIC {
        return Err(anyhow!("rvl info: Wii magic missing (got 0x{:08x})", magic));
    }

    let mut name = [0u8; 64];
    reader.seek(SeekFrom::Start(0x20))?;
    reader.read_exact(&mut name)?;
    let game_name = read_ascii_trim(&name);

    reader.seek(SeekFrom::Start(0x4E000))?;
    let region_code = reader.read_u32::<BE>().unwrap_or(0xFFFF_FFFF);
    let region = match region_code {
        0 => WiiRegion::Japan,
        1 => WiiRegion::Usa,
        2 => WiiRegion::Pal,
        3 => WiiRegion::RegionFree,
        4 => WiiRegion::Korea,
        other => WiiRegion::Unknown(other),
    };

    Ok(DiscHeader {
        game_id,
        maker_code,
        disc_number,
        disc_version,
        game_name,
        region,
    })
}

fn read_ascii_trim(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    buf[..end].iter().map(|&b| b as char).collect()
}

fn partition_kind_name(t: u32) -> &'static str {
    match t {
        0 => "data",
        1 => "update",
        2 => "channel",
        _ => "unknown",
    }
}

fn try_read_data_partition_extras<R: Read + Seek>(
    reader: &mut R,
    data: Option<WiiPartitionEntry>,
) -> Result<(
    Option<RvlTmdInfo>,
    Option<MultilingualString>,
    Option<Image>,
)> {
    let Some(data) = data else {
        return Ok((None, None, None));
    };
    let info = read_partition_info(reader, data.offset, data.group, data.partition_type)
        .map_err(|e| anyhow!("read partition info: {}", e))?;

    let tmd_info = read_tmd_at(reader, data.offset).ok();

    let mut payload_reader = PartitionPayloadReader::new(&mut *reader, &info);

    let bnr_bytes = read_opening_bnr(&mut payload_reader).ok();

    let imet_names = bnr_bytes
        .as_deref()
        .and_then(|bnr| ImetHeader::parse(bnr).ok())
        .map(|imet| {
            let entries = imet
                .names
                .into_iter()
                .map(|n| (map_imet_language(n.language), n.name))
                .collect::<Vec<_>>();
            MultilingualString::from_pairs(entries)
        });

    let image = bnr_bytes
        .as_deref()
        .and_then(|bnr| match extract_icon_image(bnr) {
            Ok(img) => Some(img),
            Err(e) => {
                log::warn!("rvl info: banner image extraction failed: {}", e);
                None
            }
        });

    Ok((tmd_info, imet_names, image))
}

fn read_tmd_at<R: Read + Seek>(reader: &mut R, partition_offset: u64) -> Result<RvlTmdInfo> {
    reader.seek(SeekFrom::Start(
        partition_offset + WII_PARTITION_HEADER_TMD_SIZE_OFFSET as u64,
    ))?;
    let tmd_size_word = reader.read_u32::<BE>()? as u64;
    let tmd_offset_word = reader.read_u32::<BE>()? as u64;
    let tmd_offset = tmd_offset_word << 2;

    if tmd_size_word == 0 || tmd_size_word > 0x100000 {
        return Err(anyhow!("implausible TMD size: {}", tmd_size_word));
    }

    let mut buf = vec![0u8; tmd_size_word as usize];
    reader.seek(SeekFrom::Start(partition_offset + tmd_offset))?;
    reader.read_exact(&mut buf)?;

    let tmd = WiiTmd::parse(&buf)?;
    Ok(RvlTmdInfo {
        title_id: tmd.title_id,
        title_version: tmd.title_version,
        system_version: tmd.system_version,
        ios_slot: tmd.ios_slot(),
        region_name: tmd.region_name().to_string(),
        content_count: tmd.content_count,
        access_rights: tmd.access_rights,
    })
}

fn read_opening_bnr<R: Read + Seek>(reader: &mut R) -> Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut boot = [0u8; 0x440];
    reader.read_exact(&mut boot)?;

    let fst_offset_word = u32::from_be_bytes(boot[0x424..0x428].try_into()?) as u64;
    let fst_size_word = u32::from_be_bytes(boot[0x428..0x42C].try_into()?) as u64;
    let fst_offset = fst_offset_word << 2;
    let fst_size = fst_size_word << 2;
    if fst_offset == 0 || fst_size == 0 || fst_size > 0x100000 {
        return Err(anyhow!("rvl info: implausible FST geometry"));
    }

    reader.seek(SeekFrom::Start(fst_offset))?;
    let mut fst_buf = vec![0u8; fst_size as usize];
    reader.read_exact(&mut fst_buf)?;

    let Some((bnr_off, bnr_size)) = find_file(&fst_buf, "opening.bnr")? else {
        return Err(anyhow!("rvl info: opening.bnr not found in FST"));
    };

    reader.seek(SeekFrom::Start(bnr_off))?;
    let mut bnr = vec![0u8; bnr_size as usize];
    reader.read_exact(&mut bnr)?;
    Ok(bnr)
}

/// Pipeline reference: `Tilka/wii-banner-player/Source/Banner.cpp` +
/// `rom-properties/src/librptexture/decoder/ImageDecoder_GCN.cpp`.
///
/// opening.bnr := [0x40 padding] [0x600 IMET] [outer U8 archive]
/// outer U8 := /meta/banner.bin /meta/icon.bin /meta/sound.bin
/// meta/banner.bin := optional "LZ77"-magic LZSS wrapper around an
///                    inner U8 archive that holds arc/timg/<name>.tpl
///                    (the texture name is listed in arc/blyt/Banner.brlyt;
///                    in practice the only TPL is the banner so any
///                    .tpl under arc/timg/ works).
/// banner.tpl := standard GameCube/Wii TPL, format 5 (RGB5A3) at 192x64.
fn extract_icon_image(bnr: &[u8]) -> Result<Image> {
    let u8_offset = locate_outer_u8(bnr)
        .ok_or_else(|| anyhow!("rvl info: U8 archive magic not found in opening.bnr"))?;
    log::debug!("rvl info: outer U8 archive at bnr offset 0x{:X}", u8_offset);
    let outer = U8Archive::parse(&bnr[u8_offset..])
        .map_err(|e| anyhow!("parse outer U8 at offset 0x{:x}: {}", u8_offset, e))?;

    let (label, raw) = if let Some(b) = outer.find("meta/banner.bin") {
        ("meta/banner.bin", b)
    } else if let Some(b) = outer.find("meta/icon.bin") {
        ("meta/icon.bin", b)
    } else if let Some(b) = outer.find_path_ending_with("/banner.bin") {
        ("*/banner.bin", b)
    } else if let Some(b) = outer.find_path_ending_with("/icon.bin") {
        ("*/icon.bin", b)
    } else {
        return Err(anyhow!(
            "rvl info: neither banner.bin nor icon.bin found in opening.bnr U8"
        ));
    };
    log::debug!(
        "rvl info: using {} ({} bytes, first 4: {:02X?})",
        label,
        raw.len(),
        &raw[..raw.len().min(4)]
    );

    let payload = unwrap_disc_banner_payload(raw)
        .with_context(|| format!("rvl info: unwrap {} container", label))?;

    let inner = U8Archive::parse(&payload)
        .with_context(|| format!("rvl info: parse inner {} U8", label))?;

    let tpl_bytes = inner
        .find_path_ending_with("/banner.tpl")
        .or_else(|| inner.find_path_ending_with("/icon.tpl"))
        .or_else(|| find_first_tpl_under_timg(&inner))
        .or_else(|| inner.find_path_ending_with(".tpl"))
        .ok_or_else(|| anyhow!("rvl info: no .tpl file inside {}", label))?;

    let (rgba, w, h) = decode_tpl(tpl_bytes)?;
    let png = encode_png(&rgba, w, h)?;
    Ok(Image::new(png, w, h))
}

fn find_first_tpl_under_timg<'a>(inner: &U8Archive<'a>) -> Option<&'a [u8]> {
    inner
        .list_paths()
        .into_iter()
        .find(|(path, _)| {
            let lower = path.to_ascii_lowercase();
            lower.starts_with("arc/timg/") && lower.ends_with(".tpl")
        })
        .map(|(_, bytes)| bytes)
}

/// Strip the optional `"LZ77"`-magic LZSS wrapper from a disc
/// opening.bnr inner `.bin` file. Pass-through for the IMD5 form so
/// the same path also handles channel-style banners (the IMD5 prefix
/// gets dropped and processing falls through to the LZSS magic test).
fn unwrap_disc_banner_payload(bytes: &[u8]) -> Result<Vec<u8>> {
    let stripped = strip_imd5(bytes);
    maybe_decompress_lz77_ascii(stripped)
}

fn locate_outer_u8(bnr: &[u8]) -> Option<usize> {
    const U8_MAGIC_BYTES: [u8; 4] = [0x55, 0xAA, 0x38, 0x2D];
    // Tilka/wii-banner-player: disc opening.bnr puts the U8 at 0x600
    // (the IMET block IS the leading 0x600 bytes including padding),
    // NAND 00000000.app puts it at 0x640.
    let probes = [0x600usize, 0x640, 0x680, 0x500, 0x80, 0x40, 0];
    for off in probes {
        if off + 4 <= bnr.len() && bnr[off..off + 4] == U8_MAGIC_BYTES {
            return Some(off);
        }
    }
    // IMET tag sits 0x40 into a 0x600 IMET block, so the U8 starts
    // 0x5C0 bytes after the "IMET" magic on titles whose padding
    // diverges from the canonical 0x40.
    if let Some(imet_at) = bnr.windows(4).position(|w| w == b"IMET") {
        let candidate = imet_at + 0x5C0;
        if candidate + 4 <= bnr.len() && bnr[candidate..candidate + 4] == U8_MAGIC_BYTES {
            return Some(candidate);
        }
    }
    for (chunk_idx, chunk) in bnr.chunks(0x20).enumerate() {
        if chunk.starts_with(&U8_MAGIC_BYTES) {
            return Some(chunk_idx * 0x20);
        }
    }
    None
}

fn decode_tpl(tpl: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    use byteorder::ReadBytesExt;
    use std::io::Cursor;

    if tpl.len() < 0x10 {
        return Err(anyhow!("TPL too small"));
    }
    let mut cur = Cursor::new(tpl);
    let _magic = cur.read_u32::<BE_>()?;
    let _texture_count = cur.read_u32::<BE_>()?;
    let image_table_offset = cur.read_u32::<BE_>()? as usize;
    if image_table_offset + 8 > tpl.len() {
        return Err(anyhow!("TPL image table past end"));
    }
    let image_header_offset = u32::from_be_bytes(
        tpl[image_table_offset..image_table_offset + 4]
            .try_into()
            .map_err(|_| anyhow!("read image_header_offset"))?,
    ) as usize;
    if image_header_offset + 0x10 > tpl.len() {
        return Err(anyhow!("TPL image header past end"));
    }
    let height = u16::from_be_bytes(
        tpl[image_header_offset..image_header_offset + 2]
            .try_into()
            .unwrap(),
    ) as u32;
    let width = u16::from_be_bytes(
        tpl[image_header_offset + 2..image_header_offset + 4]
            .try_into()
            .unwrap(),
    ) as u32;
    let format = u32::from_be_bytes(
        tpl[image_header_offset + 4..image_header_offset + 8]
            .try_into()
            .unwrap(),
    );
    let data_offset = u32::from_be_bytes(
        tpl[image_header_offset + 8..image_header_offset + 12]
            .try_into()
            .unwrap(),
    ) as usize;

    let rgba = match format {
        5 => {
            let size = (width as usize) * (height as usize) * 2;
            if data_offset + size > tpl.len() {
                return Err(anyhow!("TPL RGB5A3 data past end of buffer"));
            }
            decode_rgb5a3_tiled(&tpl[data_offset..data_offset + size], width, height)?
        }
        6 => {
            let size = (width as usize) * (height as usize) * 4;
            if data_offset + size > tpl.len() {
                return Err(anyhow!("TPL RGBA32 data past end of buffer"));
            }
            decode_rgba32_tiled(&tpl[data_offset..data_offset + size], width, height)?
        }
        other => {
            return Err(anyhow!(
                "unsupported TPL pixel format {} (supports 5=RGB5A3, 6=RGBA32)",
                other
            ));
        }
    };
    Ok((rgba, width, height))
}

fn map_imet_language(
    lang: crate::nintendo::rvl::models::imet::ImetLanguage,
) -> crate::info::LanguageCode {
    use crate::info::LanguageCode;
    use crate::nintendo::rvl::models::imet::ImetLanguage;
    match lang {
        ImetLanguage::Japanese => LanguageCode::Japanese,
        ImetLanguage::English => LanguageCode::English,
        ImetLanguage::German => LanguageCode::German,
        ImetLanguage::French => LanguageCode::French,
        ImetLanguage::Spanish => LanguageCode::Spanish,
        ImetLanguage::Italian => LanguageCode::Italian,
        ImetLanguage::Dutch => LanguageCode::Dutch,
    }
}

#[cfg(test)]
mod banner_tests {
    use super::*;
    use byteorder::WriteBytesExt;

    fn write_be_u32(buf: &mut [u8], v: u32) {
        buf[..4].copy_from_slice(&v.to_be_bytes());
    }

    fn build_test_tpl(width: u32, height: u32) -> (Vec<u8>, [u8; 4]) {
        const TPL_MAGIC: [u8; 4] = [0x00, 0x20, 0xAF, 0x30];
        let pixel_bytes = (width * height * 2) as usize;

        let tpl_header_off = 0usize;
        let imgtab_off = 0x0C;
        let img_header_off = 0x14;
        let data_off = 0x40;

        let mut tpl = vec![0u8; data_off + pixel_bytes];
        tpl[tpl_header_off..tpl_header_off + 4].copy_from_slice(&TPL_MAGIC);
        write_be_u32(&mut tpl[tpl_header_off + 4..], 1);
        write_be_u32(&mut tpl[tpl_header_off + 8..], imgtab_off as u32);

        write_be_u32(&mut tpl[imgtab_off..], img_header_off as u32);

        tpl[img_header_off..img_header_off + 2].copy_from_slice(&(height as u16).to_be_bytes());
        tpl[img_header_off + 2..img_header_off + 4].copy_from_slice(&(width as u16).to_be_bytes());
        write_be_u32(&mut tpl[img_header_off + 4..], 5);
        write_be_u32(&mut tpl[img_header_off + 8..], data_off as u32);

        // 0xFC00 = RGB5A3 opaque red (high bit + 0b11111 in the R5 field).
        let red_word = 0xFC00u16.to_be_bytes();
        for i in 0..(width * height) as usize {
            tpl[data_off + i * 2..data_off + i * 2 + 2].copy_from_slice(&red_word);
        }
        (tpl, [0xFF, 0, 0, 0xFF])
    }

    fn build_u8_archive(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        // Inlined rather than imported from `models/u8_archive::tests`
        // so this banner test stays self-contained.

        #[derive(Debug)]
        struct N {
            is_dir: bool,
            name: String,
            data: Vec<u8>,
            children: Vec<N>,
        }
        fn insert(root: &mut N, parts: &[&str], data: Vec<u8>) {
            if parts.len() == 1 {
                root.children.push(N {
                    is_dir: false,
                    name: parts[0].to_string(),
                    data,
                    children: Vec::new(),
                });
                return;
            }
            let head = parts[0];
            let pos = root
                .children
                .iter()
                .position(|c| c.is_dir && c.name == head);
            let idx = match pos {
                Some(i) => i,
                None => {
                    root.children.push(N {
                        is_dir: true,
                        name: head.to_string(),
                        data: Vec::new(),
                        children: Vec::new(),
                    });
                    root.children.len() - 1
                }
            };
            insert(&mut root.children[idx], &parts[1..], data);
        }

        let mut root = N {
            is_dir: true,
            name: String::new(),
            data: Vec::new(),
            children: Vec::new(),
        };
        for (path, data) in entries {
            let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            insert(&mut root, &parts, data.clone());
        }

        let mut nodes: Vec<(bool, u32, u32, u32)> = Vec::new(); // (is_dir, name_off, data_off_or_0, size)
        let mut string_table: Vec<u8> = vec![0];
        let mut payloads: Vec<Vec<u8>> = Vec::new();

        fn intern(t: &mut Vec<u8>, name: &str) -> u32 {
            if name.is_empty() {
                return 0;
            }
            let off = t.len() as u32;
            t.extend_from_slice(name.as_bytes());
            t.push(0);
            off
        }

        fn emit(
            node: &N,
            nodes: &mut Vec<(bool, u32, u32, u32)>,
            st: &mut Vec<u8>,
            payloads: &mut Vec<Vec<u8>>,
        ) {
            let name_off = intern(st, &node.name);
            let my_idx = nodes.len();
            nodes.push((node.is_dir, name_off, 0, 0));
            if node.is_dir {
                for child in &node.children {
                    emit(child, nodes, st, payloads);
                }
                let end_excl = nodes.len() as u32;
                nodes[my_idx].3 = end_excl;
            } else {
                nodes[my_idx].3 = node.data.len() as u32;
                payloads.push(node.data.clone());
            }
        }

        emit(&root, &mut nodes, &mut string_table, &mut payloads);
        let n = nodes.len();

        let header_size = 0x20usize;
        let node_table_off = header_size;
        let node_table_size = n * 12;
        let string_table_off = node_table_off + node_table_size;
        let mut data_off = string_table_off + string_table.len();
        data_off = (data_off + 0x1F) & !0x1F;

        let mut total = data_off;
        let mut file_offs: Vec<u32> = Vec::new();
        for p in &payloads {
            file_offs.push(total as u32);
            total += p.len();
        }
        let mut out = vec![0u8; total];

        write_be_u32(&mut out[0..], 0x55AA382D);
        write_be_u32(&mut out[4..], node_table_off as u32);
        write_be_u32(&mut out[8..], (node_table_size + string_table.len()) as u32);
        write_be_u32(&mut out[12..], data_off as u32);

        let mut fc = 0usize;
        for (i, (is_dir, name_off, _data_off, size)) in nodes.iter().enumerate() {
            let off = node_table_off + i * 12;
            let header = ((*is_dir as u32) << 24) | (name_off & 0x00FF_FFFF);
            write_be_u32(&mut out[off..], header);
            let real_data_off = if *is_dir {
                0
            } else {
                let v = file_offs[fc];
                fc += 1;
                v
            };
            write_be_u32(&mut out[off + 4..], real_data_off);
            write_be_u32(&mut out[off + 8..], *size);
        }

        out[string_table_off..string_table_off + string_table.len()].copy_from_slice(&string_table);

        let mut cur = data_off;
        for p in &payloads {
            out[cur..cur + p.len()].copy_from_slice(p);
            cur += p.len();
        }
        out
    }

    fn build_synthetic_opening_bnr() -> Vec<u8> {
        let (tpl, _) = build_test_tpl(192, 64);
        let inner_u8 = build_u8_archive(&[("arc/timg/banner.tpl", tpl)]);
        let outer_u8 = build_u8_archive(&[("meta/banner.bin", inner_u8)]);

        let mut bnr = vec![0u8; 0x640];
        // Plant a fake "IMET" tag so locate_outer_u8's IMET fallback
        // doesn't accidentally help us reach the right answer.
        bnr[0x40..0x44].copy_from_slice(b"IMET");
        bnr.extend_from_slice(&outer_u8);
        bnr
    }

    #[test]
    fn extracts_192x64_banner_from_synthetic_opening_bnr() {
        let bnr = build_synthetic_opening_bnr();
        let image = extract_icon_image(&bnr).expect("banner extraction must succeed");
        assert_eq!(image.width, 192, "expected 192-wide banner");
        assert_eq!(image.height, 64, "expected 64-tall banner");
        assert!(!image.png_bytes.is_empty(), "PNG bytes should be non-empty");
        assert_eq!(&image.png_bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn locates_u8_archive_at_0x640() {
        let bnr = build_synthetic_opening_bnr();
        assert_eq!(locate_outer_u8(&bnr), Some(0x640));
    }

    #[allow(dead_code)]
    fn _silence_unused(_: &mut [u8]) {
        let mut b = [0u8; 4];
        b.as_mut_slice().write_u32::<BE_>(0).unwrap();
    }
}
