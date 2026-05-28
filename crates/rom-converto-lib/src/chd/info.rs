//! `info` extractor for CHD files. Surfaces v5 header, hash triplet,
//! per-track CHT2 metadata, optional DVD geometry, and the chdman build
//! string when present.

use crate::chd::reader::cue_generator::parse_chd_track_metadata;
use crate::chd::reader::open_chd_sync;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdInfo {
    pub version: u8,
    pub compressors: Vec<String>,
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    pub hunk_count: u64,
    pub logical_bytes: u64,
    pub physical_bytes: u64,
    pub compression_ratio: f64,
    pub raw_sha1: String,
    pub sha1: String,
    pub parent_sha1: Option<String>,
    pub tracks: Vec<ChdTrack>,
    pub metadata_tags: Vec<ChdMetadataTagSummary>,
    /// Chdman build string from the optional `VERS` metadata tag.
    pub version_string: Option<String>,
    /// DVD-only fields derived when a `DVD ` metadata tag is present.
    pub dvd: Option<ChdDvdInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdTrack {
    pub number: u8,
    pub track_type: String,
    pub frames: u32,
    pub pregap: u32,
    pub subtype: Option<String>,
    pub pgtype: Option<String>,
    pub pgsub: Option<String>,
    pub postgap: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdDvdInfo {
    /// Total 2048-byte sectors derived from header.logical_bytes.
    pub total_sectors: u64,
    pub layer_class: DvdLayerClass,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DvdLayerClass {
    #[default]
    SingleLayer,
    DualLayer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChdMetadataTagSummary {
    pub tag: String,
    pub length: u32,
}

pub fn read_info(path: &Path) -> Result<ChdInfo> {
    let handle = open_chd_sync(path).map_err(into_anyhow)?;
    let header = &handle.header;

    let physical_bytes = std::fs::metadata(path)?.len();
    let logical_bytes = header.logical_bytes;
    let ratio = if logical_bytes > 0 {
        (physical_bytes as f64 / logical_bytes as f64) * 100.0
    } else {
        0.0
    };

    let compressors: Vec<String> = [
        &header.compressor_0,
        &header.compressor_1,
        &header.compressor_2,
        &header.compressor_3,
    ]
    .iter()
    .filter_map(|c| fourcc_to_string(c))
    .collect();

    let parent_sha1 = if header.parent_sha1 == [0u8; 20] {
        None
    } else {
        Some(hex::encode(header.parent_sha1))
    };

    let tracks = extract_tracks(&handle);
    let version_string = extract_version_string(&handle);
    let dvd = extract_dvd_info(&handle, logical_bytes);

    let metadata_tags = handle
        .metadata
        .iter()
        .map(|m| ChdMetadataTagSummary {
            tag: fourcc_to_string(&m.tag).unwrap_or_else(|| hex::encode(m.tag)),
            length: m.data.len() as u32,
        })
        .collect();

    Ok(ChdInfo {
        version: 5,
        compressors,
        hunk_bytes: header.hunk_bytes,
        unit_bytes: header.unit_bytes,
        hunk_count: header.logical_bytes.div_ceil(header.hunk_bytes as u64),
        logical_bytes,
        physical_bytes,
        compression_ratio: ratio,
        raw_sha1: hex::encode(header.raw_sha1),
        sha1: hex::encode(header.sha1),
        parent_sha1,
        tracks,
        metadata_tags,
        version_string,
        dvd,
    })
}

const CHD_METADATA_TAG_VERS: [u8; 4] = *b"VERS";
const CHD_METADATA_TAG_DVD: [u8; 4] = *b"DVD ";

fn extract_version_string(handle: &crate::chd::reader::SyncChdHandle) -> Option<String> {
    handle
        .metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_VERS)
        .map(|m| {
            String::from_utf8_lossy(&m.data)
                .trim_end_matches('\0')
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

fn extract_dvd_info(
    handle: &crate::chd::reader::SyncChdHandle,
    logical_bytes: u64,
) -> Option<ChdDvdInfo> {
    let has_dvd_tag = handle
        .metadata
        .iter()
        .any(|m| m.tag == CHD_METADATA_TAG_DVD);
    if !has_dvd_tag {
        return None;
    }
    // DVD CHDs store ISO bytes 1:1 (2048-byte sectors); derive count
    // from logical_bytes rather than trusting the tag payload, which
    // chdman has used inconsistently across versions.
    const DVD_SECTOR_SIZE: u64 = 2048;
    const DVD_SL_MAX_SECTORS: u64 = 2_295_104;
    let total_sectors = logical_bytes / DVD_SECTOR_SIZE;
    let layer_class = if total_sectors > DVD_SL_MAX_SECTORS {
        DvdLayerClass::DualLayer
    } else {
        DvdLayerClass::SingleLayer
    };
    Some(ChdDvdInfo {
        total_sectors,
        layer_class,
    })
}

fn extract_tracks(handle: &crate::chd::reader::SyncChdHandle) -> Vec<ChdTrack> {
    use crate::chd::models::CHD_METADATA_TAG_CD;

    let cd_meta = handle
        .metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_CD);
    let Some(cd_meta) = cd_meta else {
        return Vec::new();
    };

    let meta_str = String::from_utf8_lossy(&cd_meta.data);
    let meta_str = meta_str.trim_end_matches('\0');
    parse_chd_track_metadata(meta_str)
        .map(|tracks| {
            tracks
                .into_iter()
                .map(|t| ChdTrack {
                    number: t.track_number,
                    track_type: t.track_type,
                    frames: t.frames,
                    pregap: t.pregap,
                    subtype: t.subtype,
                    pgtype: t.pgtype,
                    pgsub: t.pgsub,
                    postgap: t.postgap,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fourcc_to_string(bytes: &[u8; 4]) -> Option<String> {
    if bytes == &[0u8; 4] {
        return None;
    }
    if !bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).trim_end().to_string())
}

fn into_anyhow(e: crate::chd::error::ChdError) -> anyhow::Error {
    anyhow::anyhow!("chd: {}", e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_skips_zero_slot() {
        assert_eq!(fourcc_to_string(&[0, 0, 0, 0]), None);
    }

    #[test]
    fn fourcc_renders_ascii() {
        assert_eq!(fourcc_to_string(b"cdlz"), Some("cdlz".to_string()));
        assert_eq!(fourcc_to_string(b"DVD "), Some("DVD".to_string()));
    }

    #[test]
    fn fourcc_rejects_non_printable() {
        assert_eq!(fourcc_to_string(&[0x01, 0x02, 0x03, 0x04]), None);
    }
}
