//! `info` extractor for 3DS (CTR) ROM containers.
//!
//! Detects CIA / NCSD / NCCH at the magic level and surfaces the same
//! per-format metadata the verify path exposes plus a parsed SMDH (and
//! its 48x48 icon as PNG) when one is available. CIA inputs without a
//! MetaData block fall back to ExeFS extraction from the boot content.

use crate::info::Image;
use crate::nintendo::ctr::constants::{
    CTR_MEDIA_UNIT_SIZE, NCCH_FLAGS7_SEED_CRYPTO, NCCH_MAGIC, NCCH_MAGIC_OFFSET,
    NCSD_PARTITION_COUNT, NCSD_PARTITION_ENTRY_SIZE, NCSD_PARTITION_TABLE_OFFSET,
    NCSD_TITLE_ID_OFFSET, TMD_CONTENT_RECORD_SIZE, TMD_CONTENT_RECORDS_OFFSET,
};
use crate::nintendo::ctr::decrypt::util::{decrypt_first_ncch_block, derive_title_key_from_ticket};
use crate::nintendo::ctr::exefs::read_icon_section;
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaHeader, MetaData};
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::smdh::{AgeRating, SMDH_LARGE_ICON_DIM, Smdh};
use crate::nintendo::ctr::models::title_metadata::ContentChunkRecord;
use crate::nintendo::ctr::util::align_64;
use crate::nintendo::ctr::z3ds::models::{
    Z3DS_HEADER_SIZE, Z3DS_MAGIC, Z3dsHeader, underlying_magic,
};
use crate::util::pixel::{decode_rgb565_morton_tiled, encode_png};
use anyhow::{Context, Result, anyhow};
use binrw::BinRead;
use byteorder::{LE, ReadBytesExt};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub smdh: Option<CtrSmdhInfo>,
    pub icon: Option<Image>,
    pub small_icon: Option<Image>,
    #[serde(default)]
    pub compressed: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CtrFormat {
    #[default]
    Unknown,
    Cia,
    Ncsd,
    Ncch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CtrSmdhInfo {
    pub titles: Vec<CtrSmdhTitle>,
    pub region_lock: u32,
    pub region_names: Vec<String>,
    pub flags: u32,
    pub eula_version_major: u8,
    pub eula_version_minor: u8,
    pub age_ratings: Vec<CtrSmdhAgeRating>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtrSmdhTitle {
    pub language: String,
    pub short_description: String,
    pub long_description: String,
    pub publisher: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtrSmdhAgeRating {
    pub region: String,
    pub age: u8,
    pub pending: bool,
    pub banned: bool,
}

pub fn read_info(path: &Path) -> Result<CtrInfo> {
    let physical_bytes = std::fs::metadata(path)
        .with_context(|| format!("ctr info: stat {}", path.display()))?
        .len();

    let mut file = File::open(path)?;
    let mut probe = [0u8; 0x104];
    let n = file.read(&mut probe)?;
    file.seek(SeekFrom::Start(0))?;

    if n < 4 {
        return Err(anyhow!("ctr info: file is too small"));
    }

    if &probe[0..4] == Z3DS_MAGIC.as_slice() {
        return read_z3ds_info(path, physical_bytes);
    }

    // NCSD / NCCH have magic at 0x100; CIA has a 4-byte header_size at 0.
    if n >= 0x104 {
        let magic = &probe[0x100..0x104];
        if magic == NCCH_MAGIC.as_bytes() {
            return read_ncch_info(path, physical_bytes);
        }
        if magic == b"NCSD" {
            return read_ncsd_info(path, physical_bytes);
        }
    }
    let cia_hdr = u32::from_le_bytes(probe[0..4].try_into()?);
    if cia_hdr == CIA_HEADER_SIZE {
        return read_cia_info(path, physical_bytes);
    }

    Err(anyhow!(
        "ctr info: unrecognized format at {}",
        path.display()
    ))
}

fn read_z3ds_info(path: &Path, physical_bytes: u64) -> Result<CtrInfo> {
    let mut file = File::open(path)?;
    let mut header_buf = vec![0u8; Z3DS_HEADER_SIZE as usize];
    file.read_exact(&mut header_buf)?;
    let header =
        Z3dsHeader::read(&mut Cursor::new(&header_buf)).context("ctr info: parse Z3DS header")?;

    let payload_offset = header.header_size as u64 + header.metadata_size as u64;
    let compressed_size = header.compressed_size;

    let temp_dir = tempfile::tempdir()?;
    let ext = match header.underlying_magic {
        underlying_magic::CIA => "cia",
        underlying_magic::NCSD => "3ds",
        underlying_magic::NCCH => "cxi",
        _ => "bin",
    };
    let temp_path = temp_dir.path().join(format!("info_temp.{ext}"));

    file.seek(SeekFrom::Start(payload_offset))?;
    let limited = file.take(compressed_size);
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, limited);
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, File::create(&temp_path)?);
    zstd::stream::copy_decode(&mut reader, &mut writer)?;
    writer
        .into_inner()
        .map_err(|e| anyhow!("ctr info: failed to flush decompressed output: {e}"))?
        .sync_all()?;

    let mut result = read_info(&temp_path)?;
    result.physical_bytes = physical_bytes;
    result.compressed = true;
    Ok(result)
}

fn read_cia_info(path: &Path, physical_bytes: u64) -> Result<CtrInfo> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut header_buf = vec![0u8; CIA_HEADER_SIZE as usize];
    reader.read_exact(&mut header_buf)?;
    let cia_header =
        CiaHeader::read_le(&mut Cursor::new(&header_buf)).context("ctr info: parse CIA header")?;

    let header_end = CIA_HEADER_SIZE as u64;
    let cert_start = align_64(header_end);
    let cert_end = cert_start + cia_header.cert_chain_size as u64;
    let ticket_start = align_64(cert_end);
    let ticket_end = ticket_start + cia_header.ticket_size as u64;
    let tmd_start = align_64(ticket_end);
    let tmd_end = tmd_start + cia_header.tmd_size as u64;
    let content_start = align_64(tmd_end);
    let content_end = content_start + cia_header.content_size;
    let meta_start = align_64(content_end);

    let first_chunk = read_first_content_chunk(&mut reader, tmd_start)?;
    let content_encrypted = first_chunk.content_type.is_encrypted();

    let ncch_hdr = if content_encrypted {
        let title_key = derive_title_key_from_ticket(&mut reader, ticket_start)?;
        let block = decrypt_first_ncch_block(
            &mut reader,
            content_start,
            first_chunk.content_index,
            &title_key,
        )?;
        NcchHeader::read(&mut Cursor::new(&block))
            .context("ctr info: parse decrypted NCCH header")?
    } else {
        reader.seek(SeekFrom::Start(content_start))?;
        read_ncch_header_at(&mut reader)?
    };
    let info_from_ncch = info_from_ncch_header(&ncch_hdr);
    let (seed_crypto, seed_found, seed_keyy) = seed_fields(&ncch_hdr);

    let smdh = if cia_header.meta_size > 0 {
        reader.seek(SeekFrom::Start(meta_start))?;
        let mut meta_buf = vec![0u8; cia_header.meta_size as usize];
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

    Ok(CtrInfo {
        format: CtrFormat::Cia,
        compressed: false,
        physical_bytes,
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
        smdh: smdh_info,
        icon,
        small_icon,
    })
}

fn read_ncsd_info(path: &Path, physical_bytes: u64) -> Result<CtrInfo> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

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

    reader.seek(SeekFrom::Start(first_offset))?;
    let ncch_hdr = read_ncch_header_at(&mut reader)?;
    let info_from_ncch = info_from_ncch_header(&ncch_hdr);
    let (seed_crypto, seed_found, seed_keyy) = seed_fields(&ncch_hdr);

    let cartridge_size = read_ncsd_image_size(&mut reader).ok();

    // ExeFS sits at (first_offset + exefsoffset*MU) for exefssize*MU.
    let smdh = if ncch_hdr.exefssize > 0 {
        let exefs_abs = first_offset + ncch_hdr.exefsoffset as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        let exefs_len = (ncch_hdr.exefssize as u64) * CTR_MEDIA_UNIT_SIZE as u64;
        match read_exefs_icon_as_smdh(&mut reader, &ncch_hdr, exefs_abs, exefs_len) {
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

    Ok(CtrInfo {
        format: CtrFormat::Ncsd,
        compressed: false,
        physical_bytes,
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
        smdh: smdh_info,
        icon,
        small_icon,
    })
}

fn read_ncch_info(path: &Path, physical_bytes: u64) -> Result<CtrInfo> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let ncch_hdr = read_ncch_header_at(&mut reader)?;
    let info_from_ncch = info_from_ncch_header(&ncch_hdr);
    let (seed_crypto, seed_found, seed_keyy) = seed_fields(&ncch_hdr);

    let smdh = if ncch_hdr.exefssize > 0 {
        let exefs_abs = ncch_hdr.exefsoffset as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        let exefs_len = ncch_hdr.exefssize as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        match read_exefs_icon_as_smdh(&mut reader, &ncch_hdr, exefs_abs, exefs_len) {
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

    Ok(CtrInfo {
        format: CtrFormat::Ncch,
        compressed: false,
        physical_bytes,
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
        smdh: smdh_info,
        icon,
        small_icon,
    })
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
    let product_code = trim_nul_ascii(&hdr.productcode);
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

fn trim_nul_ascii(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
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
    exefs_len: u64,
) -> Result<Smdh> {
    reader.seek(SeekFrom::Start(exefs_abs))?;
    let mut buf = vec![0u8; exefs_len as usize];
    reader.read_exact(&mut buf)?;
    let icon_bytes = read_icon_section(ncch_hdr, &buf)?;
    Smdh::parse(&icon_bytes)
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
        compress_rom(&cxi_path, &zcxi_path, None, false, &NoProgress)
            .await
            .unwrap();

        let info = read_info(&zcxi_path).unwrap();
        assert_eq!(info.format, CtrFormat::Ncch);
        assert!(info.compressed);
    }
}
