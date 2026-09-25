//! `info` extractor for 3DS (CTR) ROM containers.
//!
//! Detects CIA / NCSD / NCCH at the magic level and surfaces the same
//! per-format metadata the verify path exposes plus a parsed SMDH (and
//! its 48x48 icon as PNG) when one is available. CIA inputs without a
//! MetaData block fall back to ExeFS extraction from the boot content.

use crate::info::{ContentKind, Image};
use crate::nintendo::ctr::bundle::{BundleEntry, BundleKind, list_bundle};
use crate::nintendo::ctr::constants::{
    CTR_MEDIA_UNIT_SIZE, NCCH_FLAGS7_SEED_CRYPTO, NCCH_MAGIC, NCCH_MAGIC_OFFSET,
    NCSD_PARTITION_COUNT, NCSD_PARTITION_ENTRY_SIZE, NCSD_PARTITION_TABLE_OFFSET,
    NCSD_TITLE_ID_OFFSET, TICKET_SIG_BODY_OFFSET, TICKET_TITLE_ID_OFFSET, TMD_CONTENT_COUNT_OFFSET,
    TMD_CONTENT_RECORD_SIZE, TMD_CONTENT_RECORDS_OFFSET,
};
use crate::nintendo::ctr::decrypt::util::{decrypt_first_ncch_block, derive_title_key_from_ticket};
use crate::nintendo::ctr::exefs::read_icon_section;
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaHeader, CiaLayout, MetaData};
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::smdh::{AgeRating, SMDH_LARGE_ICON_DIM, SMDH_TOTAL_SIZE, Smdh};
use crate::nintendo::ctr::models::title_metadata::ContentChunkRecord;
use crate::nintendo::ctr::util::is_twl_title_id;
use crate::nintendo::ctr::z3ds::Z3dsReader;
use crate::nintendo::ctr::z3ds::models::{Z3DS_HEADER_SIZE, Z3DS_MAGIC, Z3dsHeader};
use crate::util::bytes::cstr_ascii;
use crate::util::extent_end;
use crate::util::pixel::{decode_rgb565_morton_tiled, encode_png};
use crate::util::slice_reader::SliceReader;
use anyhow::{Context, Result, anyhow, bail};
use binrw::BinRead;
use byteorder::{BE, LE, ReadBytesExt};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

// Type erasure at the Z3DS recursion point so `read_info_from::<Z3dsReader<&mut dyn ReadSeek>>`
// is a fixed point instead of unbounded monomorphization.
pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek + ?Sized> ReadSeek for T {}

/// Metadata extracted from a CIA, NCSD, NCCH, or Z3DS-wrapped 3DS ROM.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrInfo {
    pub format: CtrFormat,
    pub physical_bytes: u64,
    pub title_id: String,
    pub program_id: String,
    pub product_code: String,
    pub maker_code: String,
    pub maker_name: Option<String>,
    pub cartridge_size: Option<u64>,
    pub ncch_encrypted: bool,
    pub seed_crypto: bool,
    pub seed_found: Option<bool>,
    pub seed_keyy: Option<String>,
    /// Normalized Game/Update/DLC/Demo/System classification derived from
    /// `title_id`.
    #[serde(default)]
    pub content_kind: Option<ContentKind>,
    pub smdh: Option<CtrSmdhInfo>,
    pub icon: Option<Image>,
    pub small_icon: Option<Image>,
    #[serde(default)]
    pub compressed: bool,
    /// NCSD partition table entries. Empty for non-NCSD inputs.
    #[serde(default)]
    pub ncsd_partitions: Vec<CtrPartitionEntry>,
    /// TMD content chunk entries. Empty for non-CIA inputs.
    #[serde(default)]
    pub cia_contents: Vec<CtrContentEntry>,
    /// Set when the input is an Azahar bundle archive (`.bcia`/`.bcci`/`.bcxi`);
    /// every other field describes the bootable member, not the container.
    #[serde(default)]
    pub bundle_kind: Option<BundleKind>,
    /// Every member title in archive order. Empty for non-bundle inputs.
    #[serde(default)]
    pub bundled_titles: Vec<CtrBundledTitle>,
}

/// Which container format a ROM was detected as.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub enum CtrFormat {
    #[default]
    Unknown,
    Cia,
    Ncsd,
    Ncch,
    Threedsx,
}

/// Fields of a parsed SMDH relevant to `info` output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrSmdhInfo {
    pub titles: Vec<CtrSmdhTitle>,
    pub region_lock: u32,
    pub region_names: Vec<String>,
    pub flags: u32,
    pub eula_version_major: u8,
    pub eula_version_minor: u8,
    pub age_ratings: Vec<CtrSmdhAgeRating>,
}

/// A single language entry from the SMDH title table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrSmdhTitle {
    pub language: String,
    pub short_description: String,
    pub long_description: String,
    pub publisher: String,
}

/// A single region's age rating from the SMDH age-rating block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrSmdhAgeRating {
    pub region: String,
    pub age: u8,
    pub pending: bool,
    pub banned: bool,
}

/// A single partition entry from an NCSD partition table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrPartitionEntry {
    pub index: u8,
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

/// A single content entry from a CIA's TMD content chunk records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrContentEntry {
    pub index: u16,
    pub content_id: String,
    pub size: u64,
    pub encrypted: bool,
}

/// One member title bundled inside a `.bcia`/`.bcci`/`.bcxi` archive, in
/// archive order. A member the parser cannot read (unknown extension or
/// garbage bytes) still gets a row, as an `Unknown` format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export_to = "info.ts"))]
pub struct CtrBundledTitle {
    pub name: String,
    pub size: u64,
    pub main: bool,
    pub format: CtrFormat,
    pub compressed: bool,
    pub encrypted: bool,
    pub title_id: String,
    pub content_kind: Option<ContentKind>,
    /// English SMDH long description when the member carries one.
    pub title: Option<String>,
}

/// Detects the ROM format at `path` and extracts its [`CtrInfo`] metadata.
/// Azahar bundle extensions are routed to [`read_bundle_info`]; everything
/// else is parsed in place.
pub fn read_info(path: &Path) -> Result<CtrInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("ctr info: stat {}", path.display()))?
        .len();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if let Some(kind) = ext.as_deref().and_then(BundleKind::from_ext) {
        return read_bundle_info(path, kind);
    }
    let mut reader = BufReader::new(File::open(path)?);
    let mut info =
        read_info_from(&mut reader).with_context(|| format!("ctr info: {}", path.display()))?;
    info.physical_bytes = physical_bytes;
    Ok(info)
}

/// Reads [`CtrInfo`] from any reader positioned at the start of a CIA, NCSD,
/// NCCH, 3DSX, or Z3DS image. `physical_bytes` is left at 0; callers that
/// know the container's on-disk size set it afterwards.
pub fn read_info_from<R: Read + Seek>(reader: &mut R) -> Result<CtrInfo> {
    // Read may return short (Z3dsReader stops at frame boundaries), so
    // fill the probe until it is full or the source ends.
    let mut probe = [0u8; 0x104];
    let mut n = 0;
    while n < probe.len() {
        let k = reader.read(&mut probe[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    reader.seek(SeekFrom::Start(0))?;

    if n < 4 {
        return Err(anyhow!("ctr info: file is too small"));
    }

    if &probe[0..4] == Z3DS_MAGIC.as_slice() {
        return read_z3ds_info(reader);
    }

    if &probe[0..4] == b"3DSX" {
        return read_3dsx_info(reader);
    }

    // NCSD / NCCH have magic at 0x100; CIA has a 4-byte header_size at 0.
    if n >= 0x104 {
        let magic = &probe[0x100..0x104];
        if magic == NCCH_MAGIC.as_bytes() {
            return read_ncch_info(reader);
        }
        if magic == b"NCSD" {
            return read_ncsd_info(reader);
        }
    }
    let cia_hdr = u32::from_le_bytes(probe[0..4].try_into()?);
    if cia_hdr == CIA_HEADER_SIZE {
        return read_cia_info(reader);
    }

    Err(anyhow!("ctr info: unrecognized format"))
}

/// Reads an Azahar bundle archive (plain tar of decrypted members). The
/// result describes the bootable member; every member is listed in
/// [`CtrInfo::bundled_titles`] in archive order.
fn read_bundle_info(path: &Path, kind: BundleKind) -> Result<CtrInfo> {
    let entries = list_bundle(path)?;

    // The first entry ending .cci/.zcci/.3ds mirrors Azahar's
    // AutoOpenNCCHNCSD, which opens a CCI/3DS main over a CXI or CIA; the
    // CXI-then-CIA fallbacks below are rom-converto's own descriptive
    // choice for a bundle with no CCI/3DS main (a .bcia is handled by
    // Azahar's AM CIA path instead).
    let primary_idx = entries
        .iter()
        .position(|e| {
            [".cci", ".zcci", ".3ds"]
                .iter()
                .any(|ext| e.name.ends_with(ext))
        })
        .or_else(|| {
            entries
                .iter()
                .position(|e| [".cxi", ".zcxi"].iter().any(|ext| e.name.ends_with(ext)))
        })
        .or_else(|| {
            entries
                .iter()
                .position(|e| [".cia", ".zcia"].iter().any(|ext| e.name.ends_with(ext)))
        })
        .ok_or_else(|| anyhow!("ctr info: bundle contains no 3DS ROM at {}", path.display()))?;

    let physical_bytes = std::fs::metadata(path)?.len();
    let mut bundled_titles = Vec::with_capacity(entries.len());
    let mut primary_info = None;
    for (idx, entry) in entries.iter().enumerate() {
        let is_main = idx == primary_idx;
        match read_bundle_member(path, entry) {
            Ok(info) => {
                let row = bundled_title_row(&info, entry, is_main);
                if is_main {
                    primary_info = Some(info);
                }
                bundled_titles.push(row);
            }
            // The bootable member must parse; anything else (unknown
            // extension, garbage bytes) degrades to an Unknown row.
            Err(e) if is_main => {
                return Err(e)
                    .with_context(|| format!("ctr info: {} in {}", entry.name, path.display()));
            }
            Err(_) => bundled_titles.push(unknown_bundled_title(entry)),
        }
    }
    let mut info = primary_info.expect("primary member parse failure returns early");
    info.physical_bytes = physical_bytes;
    info.bundle_kind = Some(kind);
    info.bundled_titles = bundled_titles;
    Ok(info)
}

fn read_bundle_member(path: &Path, entry: &BundleEntry) -> Result<CtrInfo> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(SliceReader::new(file, entry.offset, entry.size));
    read_info_from(&mut reader)
}

fn bundled_title_row(info: &CtrInfo, entry: &BundleEntry, is_main: bool) -> CtrBundledTitle {
    let title = info.smdh.as_ref().and_then(|smdh| {
        smdh.titles
            .iter()
            .find(|t| t.language == "English")
            .map(|t| t.long_description.clone())
            .filter(|t| !t.is_empty())
    });
    CtrBundledTitle {
        name: entry.name.clone(),
        size: entry.size,
        main: is_main,
        format: info.format,
        compressed: info.compressed,
        encrypted: info.ncch_encrypted,
        title_id: info.title_id.clone(),
        content_kind: info.content_kind,
        title,
    }
}

fn unknown_bundled_title(entry: &BundleEntry) -> CtrBundledTitle {
    CtrBundledTitle {
        name: entry.name.clone(),
        size: entry.size,
        main: false,
        format: CtrFormat::Unknown,
        compressed: false,
        encrypted: false,
        title_id: String::new(),
        content_kind: None,
        title: None,
    }
}

fn read_z3ds_info(reader: &mut dyn ReadSeek) -> Result<CtrInfo> {
    let mut header_buf = vec![0u8; Z3DS_HEADER_SIZE as usize];
    reader.read_exact(&mut header_buf)?;
    let header =
        Z3dsHeader::read(&mut Cursor::new(&header_buf)).context("ctr info: parse Z3DS header")?;

    let payload_offset = header.header_size as u64 + header.metadata_size as u64;
    let mut z = Z3dsReader::open(reader, payload_offset, header.compressed_size)
        .context("ctr info: Z3DS seek table")?;
    if z.len() != header.uncompressed_size {
        bail!("ctr info: Z3DS seek table disagrees with header size");
    }
    let mut result = read_info_from(&mut z)?;
    result.compressed = true;
    Ok(result)
}

fn read_cia_info<R: Read + Seek>(reader: &mut R) -> Result<CtrInfo> {
    let mut header_buf = vec![0u8; CIA_HEADER_SIZE as usize];
    reader.read_exact(&mut header_buf)?;
    let cia_header =
        CiaHeader::read_le(&mut Cursor::new(&header_buf)).context("ctr info: parse CIA header")?;

    let CiaLayout {
        ticket_start,
        tmd_start,
        content_start,
        meta_start,
        ..
    } = cia_header.layout();

    let first_chunk = read_first_content_chunk(reader, tmd_start)?;
    let content_encrypted = first_chunk.content_type.is_encrypted();
    let cia_contents = read_cia_contents(reader, tmd_start)?;
    let ticket_title_id = read_ticket_title_id(reader, ticket_start)?;
    let is_twl = is_twl_title_id(ticket_title_id);

    let block = if content_encrypted {
        let title_key = derive_title_key_from_ticket(reader, ticket_start)?;
        decrypt_first_ncch_block(reader, content_start, first_chunk.content_index, &title_key)?
    } else {
        reader.seek(SeekFrom::Start(content_start))?;
        let mut buf = [0u8; 0x200];
        reader.read_exact(&mut buf)?;
        buf
    };

    let (info_from_ncch, seed_crypto, seed_found, seed_keyy) = if is_twl {
        (
            info_from_srl_header(&block, ticket_title_id),
            false,
            None,
            None,
        )
    } else {
        let ncch_hdr =
            NcchHeader::read(&mut Cursor::new(&block)).context("ctr info: parse NCCH header")?;
        let summary = info_from_ncch_header(&ncch_hdr);
        let (sc, sf, sk) = seed_fields(&ncch_hdr);
        (summary, sc, sf, sk)
    };

    let smdh = if cia_header.meta_size > 0 {
        if cia_header.meta_size < MetaData::SERIALIZED_LEN as u32 {
            bail!("ctr info: CIA metadata is shorter than its required fields");
        }
        let file_len = reader.seek(SeekFrom::End(0))?;
        if extent_end(meta_start, u64::from(cia_header.meta_size), file_len).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "CIA metadata overruns file",
            )
            .into());
        }
        reader.seek(SeekFrom::Start(meta_start))?;
        let mut meta_buf = [0u8; MetaData::SERIALIZED_LEN];
        reader.read_exact(&mut meta_buf)?;
        let meta = MetaData::read_le(&mut Cursor::new(&meta_buf))
            .context("ctr info: parse CIA metadata")?;
        Smdh::parse(&meta.icon_data).ok()
    } else {
        None
    };

    let (icon, small_icon) = match &smdh {
        Some(s) => decode_smdh_icons(s),
        None => (None, None),
    };
    let smdh_info = smdh.map(smdh_to_info);
    let content_kind = content_kind_from_title_id(&info_from_ncch.title_id);

    Ok(CtrInfo {
        format: CtrFormat::Cia,
        compressed: false,
        physical_bytes: 0,
        bundle_kind: None,
        bundled_titles: Vec::new(),
        title_id: info_from_ncch.title_id,
        program_id: info_from_ncch.program_id,
        product_code: info_from_ncch.product_code,
        maker_name: crate::util::maker_codes::lookup_maker(&info_from_ncch.maker_code)
            .map(|s| s.to_string()),
        maker_code: info_from_ncch.maker_code,
        cartridge_size: None,
        ncch_encrypted: info_from_ncch.encrypted,
        seed_crypto,
        seed_found,
        seed_keyy,
        content_kind,
        smdh: smdh_info,
        icon,
        small_icon,
        ncsd_partitions: Vec::new(),
        cia_contents,
    })
}

fn read_ncsd_info<R: Read + Seek>(reader: &mut R) -> Result<CtrInfo> {
    let mut title_id = [0u8; 8];
    reader.seek(SeekFrom::Start(NCSD_TITLE_ID_OFFSET))?;
    reader.read_exact(&mut title_id)?;

    reader.seek(SeekFrom::Start(NCSD_PARTITION_TABLE_OFFSET as u64))?;
    let mut table = [0u8; NCSD_PARTITION_COUNT * NCSD_PARTITION_ENTRY_SIZE];
    reader.read_exact(&mut table)?;

    let first_offset_mu = u32::from_le_bytes(table[0..4].try_into()?);
    if first_offset_mu == 0 {
        return Err(anyhow!("ctr info: NCSD has no boot partition"));
    }
    let first_offset = first_offset_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64;

    let ncsd_partitions = parse_ncsd_partition_table(&table);

    reader.seek(SeekFrom::Start(first_offset))?;
    let ncch_hdr = read_ncch_header_at(reader)?;
    let info_from_ncch = info_from_ncch_header(&ncch_hdr);
    let (seed_crypto, seed_found, seed_keyy) = seed_fields(&ncch_hdr);

    let cartridge_size = read_ncsd_image_size(reader).ok();

    // ExeFS sits at (first_offset + exefsoffset*MU).
    let smdh = if ncch_hdr.exefssize > 0 {
        let exefs_abs = first_offset + ncch_hdr.exefsoffset as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        match read_exefs_icon_as_smdh(reader, &ncch_hdr, exefs_abs) {
            Ok(s) => Some(s),
            Err(e) => {
                log::debug!("ctr info: ExeFS read skipped ({})", e);
                None
            }
        }
    } else {
        None
    };

    let (icon, small_icon) = match &smdh {
        Some(s) => decode_smdh_icons(s),
        None => (None, None),
    };
    let smdh_info = smdh.map(smdh_to_info);
    let content_kind = content_kind_from_title_id(&info_from_ncch.title_id);

    Ok(CtrInfo {
        format: CtrFormat::Ncsd,
        compressed: false,
        physical_bytes: 0,
        bundle_kind: None,
        bundled_titles: Vec::new(),
        title_id: info_from_ncch.title_id,
        program_id: info_from_ncch.program_id,
        product_code: info_from_ncch.product_code,
        maker_name: crate::util::maker_codes::lookup_maker(&info_from_ncch.maker_code)
            .map(|s| s.to_string()),
        maker_code: info_from_ncch.maker_code,
        cartridge_size,
        ncch_encrypted: info_from_ncch.encrypted,
        seed_crypto,
        seed_found,
        seed_keyy,
        content_kind,
        smdh: smdh_info,
        icon,
        small_icon,
        ncsd_partitions,
        cia_contents: Vec::new(),
    })
}

fn read_ncch_info<R: Read + Seek>(reader: &mut R) -> Result<CtrInfo> {
    let ncch_hdr = read_ncch_header_at(reader)?;
    let info_from_ncch = info_from_ncch_header(&ncch_hdr);
    let (seed_crypto, seed_found, seed_keyy) = seed_fields(&ncch_hdr);

    let smdh = if ncch_hdr.exefssize > 0 {
        let exefs_abs = ncch_hdr.exefsoffset as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        match read_exefs_icon_as_smdh(reader, &ncch_hdr, exefs_abs) {
            Ok(s) => Some(s),
            Err(e) => {
                log::debug!("ctr info: ExeFS read skipped ({})", e);
                None
            }
        }
    } else {
        None
    };

    let (icon, small_icon) = match &smdh {
        Some(s) => decode_smdh_icons(s),
        None => (None, None),
    };
    let smdh_info = smdh.map(smdh_to_info);
    let content_kind = content_kind_from_title_id(&info_from_ncch.title_id);

    Ok(CtrInfo {
        format: CtrFormat::Ncch,
        compressed: false,
        physical_bytes: 0,
        bundle_kind: None,
        bundled_titles: Vec::new(),
        title_id: info_from_ncch.title_id,
        program_id: info_from_ncch.program_id,
        product_code: info_from_ncch.product_code,
        maker_name: crate::util::maker_codes::lookup_maker(&info_from_ncch.maker_code)
            .map(|s| s.to_string()),
        maker_code: info_from_ncch.maker_code,
        cartridge_size: None,
        ncch_encrypted: info_from_ncch.encrypted,
        seed_crypto,
        seed_found,
        seed_keyy,
        content_kind,
        smdh: smdh_info,
        icon,
        small_icon,
        ncsd_partitions: Vec::new(),
        cia_contents: Vec::new(),
    })
}

/// Reads a 3DSX homebrew executable's optional embedded SMDH.
///
/// 3DSX files have no title id, so [`CtrInfo::content_kind`] is always
/// `None` and title/program/product/maker fields stay empty.
fn read_3dsx_info<R: Read + Seek>(reader: &mut R) -> Result<CtrInfo> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if magic != *b"3DSX" {
        return Err(anyhow!("ctr info: not a 3DSX file"));
    }
    let header_size = reader.read_u16::<LE>()?;

    let smdh = if header_size >= 0x2C {
        reader.seek(SeekFrom::Start(0x20))?;
        let smdh_offset = reader.read_u32::<LE>()?;
        let smdh_size = reader.read_u32::<LE>()?;
        if smdh_size as usize == SMDH_TOTAL_SIZE {
            let mut buf = vec![0u8; SMDH_TOTAL_SIZE];
            reader
                .seek(SeekFrom::Start(smdh_offset as u64))
                .ok()
                .and_then(|_| reader.read_exact(&mut buf).ok())
                .and_then(|()| Smdh::parse(&buf).ok())
        } else {
            None
        }
    } else {
        None
    };

    let (icon, small_icon) = match &smdh {
        Some(s) => decode_smdh_icons(s),
        None => (None, None),
    };
    let smdh_info = smdh.map(smdh_to_info);

    Ok(CtrInfo {
        format: CtrFormat::Threedsx,
        compressed: false,
        physical_bytes: 0,
        bundle_kind: None,
        bundled_titles: Vec::new(),
        title_id: String::new(),
        program_id: String::new(),
        product_code: String::new(),
        maker_code: String::new(),
        maker_name: None,
        cartridge_size: None,
        ncch_encrypted: false,
        seed_crypto: false,
        seed_found: None,
        seed_keyy: None,
        content_kind: None,
        smdh: smdh_info,
        icon,
        small_icon,
        ncsd_partitions: Vec::new(),
        cia_contents: Vec::new(),
    })
}

/// Derives the shared Game/Update/DLC classification from the first 8 hex
/// chars of a CTR title id (title type + category), matching the ids the
/// title-id-based content routes already use.
fn content_kind_from_title_id(title_id: &str) -> Option<ContentKind> {
    let prefix = title_id.get(0..8)?;
    match prefix.to_ascii_lowercase().as_str() {
        "00040000" => Some(ContentKind::Game),
        "0004000e" => Some(ContentKind::Update),
        "0004008c" => Some(ContentKind::Dlc),
        "00040010" | "00040030" => Some(ContentKind::System),
        _ => None,
    }
}

struct NcchSummary {
    title_id: String,
    program_id: String,
    product_code: String,
    maker_code: String,
    encrypted: bool,
}

/// Detect NCCH seed-crypto and, when present, resolve the seed from a local
/// `seeddb.bin` (offline). Returns `(seed_crypto, seed_found, derived_keyy)`.
fn seed_fields(hdr: &NcchHeader) -> (bool, Option<bool>, Option<String>) {
    if (hdr.flags[7] & NCCH_FLAGS7_SEED_CRYPTO) == 0 {
        return (false, None, None);
    }
    let res = crate::nintendo::ctr::seed::resolve_seed_offline(hdr);
    let keyy = res.derived_key_y.map(|k| format!("{k:032X}"));
    (true, Some(res.found), keyy)
}

fn info_from_ncch_header(hdr: &NcchHeader) -> NcchSummary {
    let mut tid_be = hdr.titleid;
    tid_be.reverse();
    let mut pid_be = hdr.programid;
    pid_be.reverse();
    let product_code = cstr_ascii(&hdr.productcode);
    let maker_code = format!(
        "{}{}",
        ascii_or_dot((hdr.makercode & 0xFF) as u8),
        ascii_or_dot(((hdr.makercode >> 8) & 0xFF) as u8)
    );
    NcchSummary {
        title_id: hex::encode_upper(tid_be),
        program_id: hex::encode_upper(pid_be),
        product_code,
        maker_code,
        encrypted: hdr.is_encrypted(),
    }
}

/// Build title info from a DSiWare (TWL) content's SRL cart header. `title_id`
/// is the authoritative id from the TMD/ticket; the NCCH crypto/seed fields
/// don't apply since the content isn't an NCCH.
fn info_from_srl_header(block: &[u8; 0x200], title_id: u64) -> NcchSummary {
    let title_id_hex = hex::encode_upper(title_id.to_be_bytes());
    NcchSummary {
        title_id: title_id_hex.clone(),
        program_id: title_id_hex,
        product_code: cstr_ascii(&block[0x0C..0x10]),
        maker_code: cstr_ascii(&block[0x10..0x12]),
        encrypted: false,
    }
}

/// Reads the big-endian 8-byte title id from a ticket's signed body.
fn read_ticket_title_id<R: Read + Seek>(reader: &mut R, ticket_offset: u64) -> Result<u64> {
    reader.seek(SeekFrom::Start(
        ticket_offset + TICKET_SIG_BODY_OFFSET + TICKET_TITLE_ID_OFFSET,
    ))?;
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_be_bytes(buf))
}

fn ascii_or_dot(b: u8) -> char {
    if b.is_ascii_graphic() { b as char } else { '.' }
}

fn read_ncch_header_at<R: Read + Seek>(reader: &mut R) -> Result<NcchHeader> {
    let mut buf = [0u8; 0x200];
    reader.read_exact(&mut buf)?;
    let hdr = NcchHeader::read(&mut Cursor::new(&buf)).context("ctr info: parse NCCH header")?;
    Ok(hdr)
}

fn read_first_content_chunk<R: Read + Seek>(
    reader: &mut R,
    tmd_start: u64,
) -> Result<ContentChunkRecord> {
    reader.seek(SeekFrom::Start(tmd_start + TMD_CONTENT_RECORDS_OFFSET))?;
    let mut buf = vec![0u8; TMD_CONTENT_RECORD_SIZE as usize];
    reader.read_exact(&mut buf)?;
    ContentChunkRecord::read_be(&mut Cursor::new(&buf))
        .context("ctr info: parse first TMD content record")
}

/// Reads every TMD content chunk record (capped at 64) for a CIA's `cia_contents`.
///
/// A bogus `content_count` is bounded by what's actually left in the file so
/// it can't read past a truncated TMD; a short or failed record read just
/// stops the scan instead of failing the whole info read.
fn read_cia_contents<R: Read + Seek>(
    reader: &mut R,
    tmd_start: u64,
) -> Result<Vec<CtrContentEntry>> {
    reader.seek(SeekFrom::Start(tmd_start + TMD_CONTENT_COUNT_OFFSET))?;
    let content_count = reader.read_u16::<BE>()? as usize;
    let count = content_count.min(64);

    let records_start = tmd_start + TMD_CONTENT_RECORDS_OFFSET;
    let file_len = reader.seek(SeekFrom::End(0))?;
    let available = file_len.saturating_sub(records_start) / TMD_CONTENT_RECORD_SIZE;
    let count = count.min(available as usize);

    reader.seek(SeekFrom::Start(records_start))?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut buf = vec![0u8; TMD_CONTENT_RECORD_SIZE as usize];
        if reader.read_exact(&mut buf).is_err() {
            break;
        }
        let Ok(rec) = ContentChunkRecord::read_be(&mut Cursor::new(&buf)) else {
            break;
        };
        out.push(CtrContentEntry {
            index: rec.content_index,
            content_id: format!("{:08x}", rec.content_id),
            size: rec.content_size,
            encrypted: rec.content_type.is_encrypted(),
        });
    }
    Ok(out)
}

/// Parses an NCSD partition table into non-empty [`CtrPartitionEntry`] rows.
fn parse_ncsd_partition_table(table: &[u8]) -> Vec<CtrPartitionEntry> {
    (0..NCSD_PARTITION_COUNT)
        .filter_map(|i| {
            let base = i * NCSD_PARTITION_ENTRY_SIZE;
            let offset_mu =
                u32::from_le_bytes(table[base..base + 4].try_into().expect("4-byte slice"));
            let size_mu =
                u32::from_le_bytes(table[base + 4..base + 8].try_into().expect("4-byte slice"));
            if size_mu == 0 {
                return None;
            }
            Some(CtrPartitionEntry {
                index: i as u8,
                name: ncsd_partition_name(i as u8),
                offset: offset_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64,
                size: size_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64,
            })
        })
        .collect()
}

fn ncsd_partition_name(index: u8) -> String {
    match index {
        0 => "Application (CXI)".to_string(),
        1 => "Manual".to_string(),
        2 => "Download Play".to_string(),
        6 => "N3DS Update".to_string(),
        7 => "Update".to_string(),
        n => format!("Partition {n}"),
    }
}

fn read_ncsd_image_size<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    // NCSD image_size field is at offset 0x104 (just after the magic
    // at 0x100); units of media (0x200 bytes).
    reader.seek(SeekFrom::Start(NCCH_MAGIC_OFFSET as u64 + 4))?;
    let media_units = reader.read_u32::<LE>()? as u64;
    Ok(media_units * CTR_MEDIA_UNIT_SIZE as u64)
}

fn read_exefs_icon_as_smdh<R: Read + Seek>(
    reader: &mut R,
    ncch_hdr: &NcchHeader,
    exefs_abs: u64,
) -> Result<Smdh> {
    Smdh::parse(&read_icon_section(reader, ncch_hdr, exefs_abs)?)
}

fn smdh_to_info(s: Smdh) -> CtrSmdhInfo {
    let titles = s
        .titles
        .iter()
        .map(|t| CtrSmdhTitle {
            language: format!("{:?}", t.language),
            short_description: t.short_description.clone(),
            long_description: t.long_description.clone(),
            publisher: t.publisher.clone(),
        })
        .collect();

    let region_names = region_lock_names(s.region_lock);
    let age_ratings: Vec<CtrSmdhAgeRating> = s
        .enabled_age_ratings()
        .into_iter()
        .map(|r: AgeRating| CtrSmdhAgeRating {
            region: format!("{:?}", r.region),
            age: r.age,
            pending: r.pending,
            banned: r.banned,
        })
        .collect();

    CtrSmdhInfo {
        titles,
        region_lock: s.region_lock,
        region_names,
        flags: s.flags,
        eula_version_major: s.eula_version_major,
        eula_version_minor: s.eula_version_minor,
        age_ratings,
    }
}

fn region_lock_names(mask: u32) -> Vec<String> {
    if mask == 0x7FFFFFFF {
        return vec!["RegionFree".to_string()];
    }
    let mut out = Vec::new();
    if mask & 0x01 != 0 {
        out.push("Japan".to_string());
    }
    if mask & 0x02 != 0 {
        out.push("NorthAmerica".to_string());
    }
    if mask & 0x04 != 0 {
        out.push("Europe".to_string());
    }
    if mask & 0x08 != 0 {
        out.push("Australia".to_string());
    }
    if mask & 0x10 != 0 {
        out.push("China".to_string());
    }
    if mask & 0x20 != 0 {
        out.push("Korea".to_string());
    }
    if mask & 0x40 != 0 {
        out.push("Taiwan".to_string());
    }
    out
}

fn decode_smdh_icons(s: &Smdh) -> (Option<Image>, Option<Image>) {
    let large = decode_rgb565_morton_tiled(&s.large_icon, SMDH_LARGE_ICON_DIM, SMDH_LARGE_ICON_DIM)
        .ok()
        .and_then(|rgba| encode_png(&rgba, SMDH_LARGE_ICON_DIM, SMDH_LARGE_ICON_DIM).ok())
        .map(|png| Image::new(png, SMDH_LARGE_ICON_DIM, SMDH_LARGE_ICON_DIM));

    let small_dim = 24;
    let small = decode_rgb565_morton_tiled(&s.small_icon, small_dim, small_dim)
        .ok()
        .and_then(|rgba| encode_png(&rgba, small_dim, small_dim).ok())
        .map(|png| Image::new(png, small_dim, small_dim));

    (large, small)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::constants::{
        NCCH_FLAGS_OFFSET, NCCH_FLAGS7_NOCRYPTO, NCCH_MAGIC_OFFSET,
    };
    use crate::nintendo::ctr::z3ds::compress_rom;
    use crate::nintendo::ctr::z3ds::models::underlying_magic;
    use crate::util::CancelToken;
    use crate::util::NoProgress;

    fn make_fake_decrypted_cxi(size: usize) -> Vec<u8> {
        let size = size.max(0x200);
        let mut data = vec![0u8; size];
        data[NCCH_MAGIC_OFFSET..NCCH_MAGIC_OFFSET + 4].copy_from_slice(&underlying_magic::NCCH);
        data[NCCH_FLAGS_OFFSET + 7] = NCCH_FLAGS7_NOCRYPTO;
        for (i, b) in data.iter_mut().enumerate().skip(0x200) {
            *b = (i % 251) as u8;
        }
        data
    }

    #[tokio::test]
    async fn read_info_on_compressed_cxi_returns_ncch_compressed() {
        let dir = tempfile::tempdir().unwrap();
        let cxi_path = dir.path().join("game.cxi");
        let zcxi_path = dir.path().join("game.zcxi");

        std::fs::write(&cxi_path, make_fake_decrypted_cxi(64 * 1024)).unwrap();
        compress_rom(
            &cxi_path,
            &zcxi_path,
            None,
            false,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let info = read_info(&zcxi_path).unwrap();
        assert_eq!(info.format, CtrFormat::Ncch);
        assert!(info.compressed);
    }

    #[test]
    fn read_info_on_dsiware_cia_uses_tmd_title_id_and_srl_header() {
        use crate::nintendo::ctr::test_fixtures::synth_cia_with_content;
        use sha2::{Digest, Sha256};

        let title_id = 0x0004800400000000u64;

        // SRL cart header: gamecode at 0x0C, makercode at 0x10.
        let mut content = vec![0u8; 0x200];
        content[0x0C..0x10].copy_from_slice(b"ABCD");
        content[0x10..0x12].copy_from_slice(b"01");

        let content_hash = {
            let mut h = Sha256::new();
            h.update(&content);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            arr
        };

        let (_tmp, cia_path) = synth_cia_with_content(
            title_id,
            vec![(0, 0, content.clone(), content_hash)],
            content,
            false,
        );

        let info = read_info(&cia_path).unwrap();
        assert_eq!(info.format, CtrFormat::Cia);
        assert_eq!(info.title_id, "0004800400000000");
        assert_eq!(info.product_code, "ABCD");
        assert_eq!(info.maker_code, "01");
        assert!(!info.ncch_encrypted);
        assert!(!info.seed_crypto);
        assert_eq!(info.cia_contents.len(), 1);
        assert_eq!(info.cia_contents[0].index, 0);
        assert_eq!(info.cia_contents[0].content_id, "00000000");
        assert_eq!(info.cia_contents[0].size, 0x200);
        assert!(!info.cia_contents[0].encrypted);
    }

    #[test]
    fn cia_info_rejects_metadata_extent_past_eof() {
        use crate::nintendo::ctr::test_fixtures::{make_meta, synth_cia_with_meta};

        let (_tmp, cia_path, _, _) = synth_cia_with_meta(make_meta(0));
        let mut cia = std::fs::read(&cia_path).unwrap();
        cia[0x14..0x18].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&cia_path, cia).unwrap();

        assert!(read_info(&cia_path).is_err());
    }

    #[test]
    fn cia_info_reads_real_sized_metadata_title_and_icon() {
        use crate::nintendo::ctr::test_fixtures::{make_meta, synth_cia_with_meta};

        let mut meta = make_meta(0);
        meta.icon_data = build_minimal_smdh("Homebrew", "Author");
        let (_tmp, cia_path, _, _) = synth_cia_with_meta(meta);

        let info = read_info(&cia_path).unwrap();
        let smdh = info.smdh.expect("CIA metadata SMDH");
        let title = smdh
            .titles
            .iter()
            .find(|title| title.language == "English")
            .expect("English title");
        assert_eq!(title.short_description, "Homebrew");
        assert_eq!(title.publisher, "Author");
        assert!(info.icon.is_some());
    }

    #[test]
    fn read_info_on_ncsd_lists_boot_partition() {
        use crate::nintendo::ctr::test_fixtures::{SYNTH_CIA_TITLE_ID, make_fake_ncsd};

        let dir = tempfile::tempdir().unwrap();
        let ncsd_path = dir.path().join("game.3ds");
        std::fs::write(&ncsd_path, make_fake_ncsd(SYNTH_CIA_TITLE_ID)).unwrap();

        let info = read_info(&ncsd_path).unwrap();
        assert_eq!(info.format, CtrFormat::Ncsd);
        assert!(!info.ncsd_partitions.is_empty());
        let boot = &info.ncsd_partitions[0];
        assert_eq!(boot.index, 0);
        assert_eq!(boot.name, "Application (CXI)");
        assert!(boot.size > 0);
    }

    /// Builds a minimal 0x36C0-byte SMDH with an English title and a
    /// zero-filled (but correctly sized) icon pair, matching the fixture
    /// style in `models::smdh::tests::build_minimal_smdh`.
    fn build_minimal_smdh(short: &str, publisher: &str) -> Vec<u8> {
        use crate::nintendo::ctr::models::smdh::{
            SMDH_MAGIC, SMDH_REGION_LOCK_OFFSET, SMDH_TITLE_ENTRY_SIZE, SMDH_TITLES_OFFSET,
            SMDH_TOTAL_SIZE,
        };

        let mut buf = vec![0u8; SMDH_TOTAL_SIZE];
        buf[0..4].copy_from_slice(&SMDH_MAGIC);
        let entry_off = SMDH_TITLES_OFFSET + SMDH_TITLE_ENTRY_SIZE; // English slot
        for (i, u) in short.encode_utf16().enumerate() {
            let off = entry_off + i * 2;
            buf[off..off + 2].copy_from_slice(&u.to_le_bytes());
        }
        for (i, u) in publisher.encode_utf16().enumerate() {
            let off = entry_off + 0x180 + i * 2;
            buf[off..off + 2].copy_from_slice(&u.to_le_bytes());
        }
        buf[SMDH_REGION_LOCK_OFFSET..SMDH_REGION_LOCK_OFFSET + 4]
            .copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        buf
    }

    /// Builds a minimal 3DSX file with an extended header and an embedded
    /// SMDH placed right after the header.
    fn build_minimal_3dsx_with_smdh(smdh: &[u8]) -> Vec<u8> {
        const HEADER_SIZE: u16 = 0x2C;
        let mut data = vec![0u8; HEADER_SIZE as usize];
        data[0..4].copy_from_slice(b"3DSX");
        data[0x04..0x06].copy_from_slice(&HEADER_SIZE.to_le_bytes());
        let smdh_offset = HEADER_SIZE as u32;
        let smdh_size = smdh.len() as u32;
        data[0x20..0x24].copy_from_slice(&smdh_offset.to_le_bytes());
        data[0x24..0x28].copy_from_slice(&smdh_size.to_le_bytes());
        data.extend_from_slice(smdh);
        data
    }

    #[test]
    fn read_info_on_3dsx_decodes_embedded_smdh() {
        let smdh = build_minimal_smdh("Homebrew", "Author");
        let data = build_minimal_3dsx_with_smdh(&smdh);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.3dsx");
        std::fs::write(&path, &data).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.format, CtrFormat::Threedsx);
        assert!(!info.compressed);
        assert_eq!(info.content_kind, None);
        let smdh_info = info.smdh.expect("smdh should be decoded");
        let title = smdh_info
            .titles
            .iter()
            .find(|t| t.language == "English")
            .expect("english title");
        assert_eq!(title.short_description, "Homebrew");
        assert_eq!(title.publisher, "Author");
        assert!(info.icon.is_some());
    }

    #[tokio::test]
    async fn read_info_on_z3dsx_decompresses_and_reports_compressed() {
        let smdh = build_minimal_smdh("Homebrew", "Author");
        let data = build_minimal_3dsx_with_smdh(&smdh);

        let dir = tempfile::tempdir().unwrap();
        let dsx_path = dir.path().join("game.3dsx");
        let z3dsx_path = dir.path().join("game.z3dsx");
        std::fs::write(&dsx_path, &data).unwrap();

        compress_rom(
            &dsx_path,
            &z3dsx_path,
            None,
            false,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let info = read_info(&z3dsx_path).unwrap();
        assert_eq!(info.format, CtrFormat::Threedsx);
        assert!(info.compressed);
        assert!(info.smdh.is_some());
    }

    /// Compresses a synthetic CXI into `game.zcxi` inside a fresh tempdir.
    async fn zcxi_with_frames(size: usize) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cxi_path = dir.path().join("game.cxi");
        let zcxi_path = dir.path().join("game.zcxi");
        std::fs::write(&cxi_path, make_fake_decrypted_cxi(size)).unwrap();
        compress_rom(
            &cxi_path,
            &zcxi_path,
            None,
            false,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap();
        (dir, zcxi_path)
    }

    /// Number of zstd frames in the `game.zcxi` payload per its seek table.
    fn zcxi_frame_count(zcxi_path: &std::path::Path) -> usize {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(zcxi_path)
            .unwrap();
        let mut header_buf = [0u8; 0x20];
        file.read_exact(&mut header_buf).unwrap();
        let header = Z3dsHeader::read(&mut Cursor::new(&header_buf)).unwrap();
        let payload_offset = header.header_size as u64 + header.metadata_size as u64;
        crate::nintendo::ctr::z3ds::plan_decompress_work(
            &file,
            payload_offset,
            header.compressed_size,
        )
        .unwrap()
        .len()
    }

    /// Zeroes the first 4 bytes (zstd frame magic) of the seek-table frame
    /// at `index`, so decoding that frame fails deterministically.
    fn corrupt_zstd_frame_magic(zcxi_path: &std::path::Path, index: usize) {
        use std::io::Write;

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(zcxi_path)
            .unwrap();
        let mut header_buf = [0u8; 0x20];
        file.read_exact(&mut header_buf).unwrap();
        let header = Z3dsHeader::read(&mut Cursor::new(&header_buf)).unwrap();
        let payload_offset = header.header_size as u64 + header.metadata_size as u64;
        let frames = crate::nintendo::ctr::z3ds::plan_decompress_work(
            &file,
            payload_offset,
            header.compressed_size,
        )
        .unwrap();
        let frame = &frames[index];
        file.seek(SeekFrom::Start(frame.file_offset)).unwrap();
        file.write_all(&[0u8; 4]).unwrap();
    }

    #[tokio::test]
    async fn read_info_on_z3ds_reads_only_the_frames_it_needs() {
        let (_dir, zcxi_path) = zcxi_with_frames(3 * 256 * 1024).await;
        let count = zcxi_frame_count(&zcxi_path);
        assert!(count >= 2);
        corrupt_zstd_frame_magic(&zcxi_path, count - 1);

        let info = read_info(&zcxi_path).unwrap();
        assert_eq!(info.format, CtrFormat::Ncch);
        assert!(info.compressed);
    }

    #[tokio::test]
    async fn read_info_on_z3ds_with_corrupt_first_frame_errors() {
        let (_dir, zcxi_path) = zcxi_with_frames(3 * 256 * 1024).await;
        corrupt_zstd_frame_magic(&zcxi_path, 0);

        assert!(read_info(&zcxi_path).is_err());
    }

    #[test]
    fn read_info_on_3dsx_without_smdh_still_succeeds() {
        // header_size below 0x2C means no extended header / no SMDH fields.
        let data = vec![b'3', b'D', b'S', b'X', 0x20, 0x00, 0, 0];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.3dsx");
        std::fs::write(&path, &data).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.format, CtrFormat::Threedsx);
        assert!(info.smdh.is_none());
        assert!(info.icon.is_none());
    }

    #[test]
    fn read_info_on_3dsx_with_bogus_smdh_size_still_succeeds() {
        // A header claiming a huge smdh_size (not matching SMDH_TOTAL_SIZE)
        // must not be trusted for an allocation; the read still succeeds,
        // just without a decoded SMDH.
        let smdh = build_minimal_smdh("Homebrew", "Author");
        let mut data = build_minimal_3dsx_with_smdh(&smdh);
        data[0x24..0x28].copy_from_slice(&u32::MAX.to_le_bytes());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.3dsx");
        std::fs::write(&path, &data).unwrap();

        let info = read_info(&path).unwrap();
        assert_eq!(info.format, CtrFormat::Threedsx);
        assert!(info.smdh.is_none());
        assert!(info.icon.is_none());
    }

    fn cia_with_title_id(title_id: u64) -> (tempfile::TempDir, std::path::PathBuf) {
        use crate::nintendo::ctr::test_fixtures::{make_ncch_header_bytes, synth_cia_with_content};
        use sha2::{Digest, Sha256};

        let content = make_ncch_header_bytes(title_id);
        let content_hash = {
            let mut h = Sha256::new();
            h.update(&content);
            let d = h.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&d);
            arr
        };
        synth_cia_with_content(
            title_id,
            vec![(0, 0, content.clone(), content_hash)],
            content,
            false,
        )
    }

    #[test]
    fn content_kind_maps_update_title_id() {
        let (_tmp, cia_path) = cia_with_title_id(0x0004000E_12345678u64);
        let info = read_info(&cia_path).unwrap();
        assert_eq!(info.title_id, "0004000E12345678");
        assert_eq!(info.content_kind, Some(ContentKind::Update));
    }

    #[test]
    fn content_kind_maps_system_title_id() {
        for title_id in [0x0004001000030000u64, 0x0004003000030000u64] {
            let (_tmp, cia_path) = cia_with_title_id(title_id);
            let info = read_info(&cia_path).unwrap();
            assert_eq!(info.content_kind, Some(ContentKind::System));
        }
    }

    /// Persists `bytes` as `name` inside `dir` and appends it to `builder`
    /// under that exact archive name.
    fn tar_append(builder: &mut tar::Builder<std::fs::File>, dir: &Path, name: &str, bytes: &[u8]) {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        builder.append_path_with_name(&path, name).unwrap();
    }

    #[test]
    fn read_info_on_bcci_reports_primary_ncsd_and_member_rows() {
        use crate::nintendo::ctr::bundle::BundleKind;
        use crate::nintendo::ctr::test_fixtures::{SYNTH_CIA_TITLE_ID, make_fake_ncsd, synth_cia};

        let dir = tempfile::tempdir().unwrap();
        let (_cia_tmp, cia_path, _) = synth_cia(SYNTH_CIA_TITLE_ID, 0x400);

        let bundle_path = dir.path().join("game.bcci");
        let mut builder = tar::Builder::new(std::fs::File::create(&bundle_path).unwrap());
        tar_append(
            &mut builder,
            dir.path(),
            "game.cci",
            &make_fake_ncsd(SYNTH_CIA_TITLE_ID),
        );
        std::fs::copy(&cia_path, dir.path().join("update.cia")).unwrap();
        builder
            .append_path_with_name(dir.path().join("update.cia"), "update.cia")
            .unwrap();
        builder.finish().unwrap();
        let bundle_size = std::fs::metadata(&bundle_path).unwrap().len();

        let info = read_info(&bundle_path).unwrap();
        assert_eq!(info.format, CtrFormat::Ncsd);
        assert_eq!(info.bundle_kind, Some(BundleKind::Cci));
        assert_eq!(info.physical_bytes, bundle_size);
        assert_eq!(info.bundled_titles.len(), 2);
        assert!(info.bundled_titles[0].main);
        assert_eq!(info.bundled_titles[0].name, "game.cci");
        assert!(!info.bundled_titles[0].compressed);
        assert!(!info.bundled_titles[1].main);
        assert_eq!(info.bundled_titles[1].format, CtrFormat::Cia);
        assert!(!info.bundled_titles[1].title_id.is_empty());
    }

    #[test]
    fn read_info_on_bcia_picks_first_cia_as_primary() {
        use crate::nintendo::ctr::bundle::BundleKind;
        use crate::nintendo::ctr::test_fixtures::{make_ncch_header_bytes, synth_cia_with_content};
        use sha2::Digest;

        let dir = tempfile::tempdir().unwrap();
        let synth = |title_id: u64, name: &str| {
            let content = make_ncch_header_bytes(title_id);
            let content_hash = {
                let mut h = sha2::Sha256::new();
                h.update(&content);
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&h.finalize());
                arr
            };
            let (_tmp, path) = synth_cia_with_content(
                title_id,
                vec![(0, 0, content.clone(), content_hash)],
                content,
                false,
            );
            let dest = dir.path().join(name);
            std::fs::copy(&path, &dest).unwrap();
            dest
        };
        let first = synth(0x00040000_00000001, "game.cia");
        let second = synth(0x0004000E_00000002, "update.cia");

        let bundle_path = dir.path().join("titles.bcia");
        let mut builder = tar::Builder::new(std::fs::File::create(&bundle_path).unwrap());
        builder.append_path_with_name(&first, "game.cia").unwrap();
        builder
            .append_path_with_name(&second, "update.cia")
            .unwrap();
        builder.finish().unwrap();

        let info = read_info(&bundle_path).unwrap();
        assert_eq!(info.bundle_kind, Some(BundleKind::Cia));
        assert_eq!(info.format, CtrFormat::Cia);
        assert_eq!(info.title_id, "0004000000000001");
        assert_eq!(info.bundled_titles.len(), 2);
        assert!(info.bundled_titles[0].main);
        assert_eq!(info.bundled_titles[0].title_id, "0004000000000001");
        assert!(!info.bundled_titles[1].main);
        assert_eq!(info.bundled_titles[1].title_id, "0004000E00000002");
    }

    #[test]
    fn read_info_on_bcia_with_only_a_main_member_describes_it() {
        use crate::nintendo::ctr::bundle::BundleKind;
        use crate::nintendo::ctr::test_fixtures::{SYNTH_CIA_TITLE_ID, make_fake_ncsd};

        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("odd.bcia");
        let mut builder = tar::Builder::new(std::fs::File::create(&bundle_path).unwrap());
        tar_append(
            &mut builder,
            dir.path(),
            "game.cci",
            &make_fake_ncsd(SYNTH_CIA_TITLE_ID),
        );
        builder.finish().unwrap();

        let info = read_info(&bundle_path).unwrap();
        assert_eq!(info.bundle_kind, Some(BundleKind::Cia));
        assert_eq!(info.format, CtrFormat::Ncsd);
        assert_eq!(info.bundled_titles.len(), 1);
        assert!(info.bundled_titles[0].main);
        assert_eq!(info.bundled_titles[0].name, "game.cci");
    }

    #[test]
    fn read_info_on_bundle_lists_unknown_member_as_unknown_row() {
        use crate::nintendo::ctr::bundle::BundleKind;
        use crate::nintendo::ctr::test_fixtures::{SYNTH_CIA_TITLE_ID, synth_cia};

        let dir = tempfile::tempdir().unwrap();
        let (_cia_tmp, cia_path, _) = synth_cia(SYNTH_CIA_TITLE_ID, 0x400);

        let bundle_path = dir.path().join("mixed.bcia");
        let mut builder = tar::Builder::new(std::fs::File::create(&bundle_path).unwrap());
        builder
            .append_path_with_name(&cia_path, "game.cia")
            .unwrap();
        tar_append(&mut builder, dir.path(), "notes.txt", b"not a rom at all");
        builder.finish().unwrap();

        let info = read_info(&bundle_path).unwrap();
        assert_eq!(info.bundle_kind, Some(BundleKind::Cia));
        assert_eq!(info.format, CtrFormat::Cia);
        assert_eq!(info.bundled_titles.len(), 2);
        assert!(info.bundled_titles[0].main);
        assert!(!info.bundled_titles[1].main);
        assert_eq!(info.bundled_titles[1].format, CtrFormat::Unknown);
        assert!(info.bundled_titles[1].title_id.is_empty());
        assert_eq!(info.bundled_titles[1].name, "notes.txt");
    }

    #[test]
    fn read_bundle_info_prefers_cci_over_cxi_and_cia_regardless_of_position() {
        use crate::nintendo::ctr::test_fixtures::{SYNTH_CIA_TITLE_ID, make_fake_ncsd, synth_cia};

        let dir = tempfile::tempdir().unwrap();
        let (_cia_tmp, cia_path, _) = synth_cia(SYNTH_CIA_TITLE_ID, 0x400);

        let bundle_path = dir.path().join("game.bcci");
        let mut builder = tar::Builder::new(std::fs::File::create(&bundle_path).unwrap());
        builder
            .append_path_with_name(&cia_path, "update.cia")
            .unwrap();
        tar_append(
            &mut builder,
            dir.path(),
            "game.cxi",
            &make_fake_decrypted_cxi(0x400),
        );
        tar_append(
            &mut builder,
            dir.path(),
            "game.cci",
            &make_fake_ncsd(SYNTH_CIA_TITLE_ID),
        );
        builder.finish().unwrap();

        let info = read_info(&bundle_path).unwrap();
        assert_eq!(info.bundled_titles.len(), 3);
        assert!(!info.bundled_titles[0].main);
        assert!(!info.bundled_titles[1].main);
        assert!(info.bundled_titles[2].main);
        assert_eq!(info.bundled_titles[2].name, "game.cci");
        assert_eq!(info.format, CtrFormat::Ncsd);
    }

    #[test]
    fn read_bundle_info_falls_back_to_cxi_when_no_cci_member() {
        use crate::nintendo::ctr::test_fixtures::{SYNTH_CIA_TITLE_ID, synth_cia};

        let dir = tempfile::tempdir().unwrap();
        let (_cia_tmp, cia_path, _) = synth_cia(SYNTH_CIA_TITLE_ID, 0x400);

        let bundle_path = dir.path().join("game.bcxi");
        let mut builder = tar::Builder::new(std::fs::File::create(&bundle_path).unwrap());
        builder
            .append_path_with_name(&cia_path, "update.cia")
            .unwrap();
        tar_append(
            &mut builder,
            dir.path(),
            "game.cxi",
            &make_fake_decrypted_cxi(0x400),
        );
        builder.finish().unwrap();

        let info = read_info(&bundle_path).unwrap();
        assert_eq!(info.bundled_titles.len(), 2);
        assert!(!info.bundled_titles[0].main);
        assert!(info.bundled_titles[1].main);
        assert_eq!(info.bundled_titles[1].name, "game.cxi");
        assert_eq!(info.format, CtrFormat::Ncch);
    }
}
