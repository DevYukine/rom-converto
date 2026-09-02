//! `info` extractor for CHD files. Surfaces v5 header, hash triplet,
//! per-track CHT2 metadata, optional DVD geometry, and the chdman build
//! string when present.

use crate::chd::models::{
    CHD_METADATA_TAG_AV as CHD_METADATA_TAG_AVAV, CHD_METADATA_TAG_AV_LD as CHD_METADATA_TAG_AVLD,
};
use crate::chd::reader::cue_generator::parse_chd_track_metadata;
use crate::chd::reader::open_chd_sync;
pub use crate::laserdisc::vbi::{LdClvTime, LdDiscType};
use crate::sony_disc::DiscContent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Summary of a CHD file's header, hashes, tracks, and optional DVD
/// geometry, for the `info` command.
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
    /// LaserDisc-only fields derived when an `AVAV` metadata tag is present.
    pub ld: Option<ChdLdInfo>,
    /// Metadata of the PlayStation-family disc the CHD carries, when it
    /// carries one.
    pub content: Option<DiscContent>,
}

/// One CD track parsed from the CHD's CHT2 metadata.
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

/// DVD-only geometry derived from a CHD's `DVD ` metadata tag.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdDvdInfo {
    /// Total 2048-byte sectors derived from header.logical_bytes.
    pub total_sectors: u64,
    pub layer_class: DvdLayerClass,
}

/// Single- vs dual-layer DVD, inferred from total sector count.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DvdLayerClass {
    #[default]
    SingleLayer,
    DualLayer,
}

/// LaserDisc-only fields derived from a CHD's `AVAV` metadata tag, with VBI
/// (`AVLD` tag) statistics when present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdLdInfo {
    /// Field rate rendered verbatim from `AVAV`, e.g. "59.940058" for
    /// interlaced NTSC.
    pub fps: String,
    pub width: u32,
    /// Field height (halved from the source frame height when interlaced).
    pub height: u32,
    pub interlaced: bool,
    pub channels: u32,
    pub sample_rate: u32,
    /// Field/frame count: `AVLD` record count when present, else
    /// `logical_bytes` divided by hunk size (one field per hunk).
    pub frame_count: u32,
    /// VBI statistics decoded from the `AVLD` tag, when present.
    pub vbi: Option<ChdLdVbiInfo>,
}

/// VBI (vertical blanking interval) statistics decoded from a CHD's `AVLD`
/// metadata tag.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdLdVbiInfo {
    pub disc_type: LdDiscType,
    pub white_flag_count: u32,
    pub cav_picture_min: Option<u32>,
    pub cav_picture_max: Option<u32>,
    pub clv_start_time: Option<LdClvTime>,
    pub clv_end_time: Option<LdClvTime>,
    pub chapter_min: Option<u32>,
    pub chapter_max: Option<u32>,
    pub lead_in: bool,
    pub lead_out: bool,
}

/// One CHD metadata tag's fourcc and byte length.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChdMetadataTagSummary {
    pub tag: String,
    pub length: u32,
}

/// Reads a CHD's header, hashes, tracks, and metadata into a [`ChdInfo`] summary.
///
/// # Errors
/// Returns an error if the file cannot be opened or is not a valid V5 CHD.
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
    let ld = extract_ld_info(&handle, header);

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
        ld,
        content: crate::sony_disc::chd_disc_content(path),
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

/// Reads the `AVAV` and, when present, `AVLD` metadata tags into a
/// [`ChdLdInfo`] summary. Returns `None` when the `AVAV` tag is absent or its
/// payload does not match chdman's `FPS:%d.%06d WIDTH:%d HEIGHT:%d
/// INTERLACED:%d CHANNELS:%d SAMPLERATE:%d` format.
fn extract_ld_info(
    handle: &crate::chd::reader::SyncChdHandle,
    header: &crate::chd::models::ChdHeaderV5,
) -> Option<ChdLdInfo> {
    let avav = handle
        .metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_AVAV)?;
    let (fps, width, height, interlaced, channels, sample_rate) = parse_av_metadata(&avav.data)?;

    let avld = handle
        .metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_AVLD)
        .map(|m| m.data.as_slice())
        .filter(|data| {
            !data.is_empty()
                && data
                    .len()
                    .is_multiple_of(crate::laserdisc::vbi::VBI_PACKED_BYTES)
        });

    let (frame_count, vbi) = match avld {
        Some(data) => (
            (data.len() / crate::laserdisc::vbi::VBI_PACKED_BYTES) as u32,
            Some(summarize_vbi(data)),
        ),
        None => (
            header
                .logical_bytes
                .div_ceil(u64::from(header.hunk_bytes.max(1))) as u32,
            None,
        ),
    };

    Some(ChdLdInfo {
        fps,
        width,
        height,
        interlaced,
        channels,
        sample_rate,
        frame_count,
        vbi,
    })
}

/// Parses chdman's `AVAV` format string. Any missing or malformed field
/// yields `None` for the whole tuple.
fn parse_av_metadata(data: &[u8]) -> Option<(String, u32, u32, bool, u32, u32)> {
    let text = String::from_utf8_lossy(data);
    let text = text.trim_end_matches('\0').trim();

    let mut fps = None;
    let mut width = None;
    let mut height = None;
    let mut interlaced = None;
    let mut channels = None;
    let mut sample_rate = None;

    for field in text.split_whitespace() {
        let (key, value) = field.split_once(':')?;
        match key {
            "FPS" => fps = parse_fps(value),
            "WIDTH" => width = value.parse().ok(),
            "HEIGHT" => height = value.parse().ok(),
            "INTERLACED" => {
                interlaced = match value {
                    "0" => Some(false),
                    "1" => Some(true),
                    _ => None,
                }
            }
            "CHANNELS" => channels = value.parse().ok(),
            "SAMPLERATE" => sample_rate = value.parse().ok(),
            _ => {}
        }
    }

    Some((fps?, width?, height?, interlaced?, channels?, sample_rate?))
}

/// Validates an `FPS:%d.%06d` value (integer part plus exactly six fraction
/// digits) and returns it verbatim.
fn parse_fps(value: &str) -> Option<String> {
    let (int_part, frac_part) = value.split_once('.')?;
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int_part) || frac_part.len() != 6 || !all_digits(frac_part) {
        return None;
    }
    Some(value.to_string())
}

/// Decodes VBI statistics from `AVLD` records packed by
/// [`crate::laserdisc::vbi::vbi_metadata_pack`].
fn summarize_vbi(data: &[u8]) -> ChdLdVbiInfo {
    use crate::laserdisc::vbi::{
        VBI_CODE_CLV, VBI_CODE_LEADIN, VBI_CODE_LEADOUT, VBI_PACKED_BYTES, vbi_cav_picture,
        vbi_chapter, vbi_clv_time,
    };

    let mut vbi = ChdLdVbiInfo::default();
    let mut saw_cav = false;
    let mut saw_clv = false;

    for record in data.as_chunks::<VBI_PACKED_BYTES>().0 {
        if record[3] != 0 {
            vbi.white_flag_count += 1;
        }
        let line16 = get_u24be(&record[4..7]);
        let line1718 = get_u24be(&record[13..16]);

        match line16 {
            VBI_CODE_LEADIN => vbi.lead_in = true,
            VBI_CODE_LEADOUT => vbi.lead_out = true,
            VBI_CODE_CLV => saw_clv = true,
            _ => {}
        }

        if let Some(picture) = vbi_cav_picture(line1718) {
            saw_cav = true;
            vbi.cav_picture_min = Some(vbi.cav_picture_min.map_or(picture, |m| m.min(picture)));
            vbi.cav_picture_max = Some(vbi.cav_picture_max.map_or(picture, |m| m.max(picture)));
        }
        if let Some((hours, minutes)) = vbi_clv_time(line1718) {
            saw_clv = true;
            let time = LdClvTime { hours, minutes };
            vbi.clv_start_time.get_or_insert(time);
            vbi.clv_end_time = Some(time);
        }
        if let Some(chapter) = vbi_chapter(line1718) {
            vbi.chapter_min = Some(vbi.chapter_min.map_or(chapter, |m| m.min(chapter)));
            vbi.chapter_max = Some(vbi.chapter_max.map_or(chapter, |m| m.max(chapter)));
        }
    }

    vbi.disc_type = if saw_cav {
        LdDiscType::Cav
    } else if saw_clv {
        LdDiscType::Clv
    } else {
        LdDiscType::Unknown
    };
    vbi
}

/// Reads a big-endian 24-bit value, the inverse of `put_u24be` in
/// [`crate::laserdisc::vbi::vbi_metadata_pack`].
fn get_u24be(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}

fn extract_tracks(handle: &crate::chd::reader::SyncChdHandle) -> Vec<ChdTrack> {
    let Some(meta_str) = crate::chd::cd_track_metadata_text(&handle.metadata) else {
        return Vec::new();
    };

    parse_chd_track_metadata(&meta_str)
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
    use crate::chd::models::{
        CHD_METADATA_RESERVED_BYTES, ChdHeaderV5, ChdMetadataHeader, ChdVersion, SHA1_BYTES,
    };
    use crate::chd::reader::SyncChdHandle;
    use crate::laserdisc::vbi::{VBI_CODE_LEADIN, VBI_CODE_LEADOUT, VBI_PACKED_BYTES, VbiMetadata};
    use std::sync::Arc;

    fn test_handle(
        metadata: Vec<ChdMetadataHeader>,
        hunk_bytes: u32,
        logical_bytes: u64,
    ) -> SyncChdHandle {
        SyncChdHandle {
            header: ChdHeaderV5 {
                length: 124,
                version: ChdVersion::V5,
                compressor_0: *b"cdlz",
                compressor_1: [0; 4],
                compressor_2: [0; 4],
                compressor_3: [0; 4],
                logical_bytes,
                map_offset: 0,
                meta_offset: 0,
                hunk_bytes,
                unit_bytes: 2352,
                raw_sha1: [0; SHA1_BYTES],
                sha1: [0; SHA1_BYTES],
                parent_sha1: [0; SHA1_BYTES],
            },
            map: Vec::new(),
            metadata,
            file: Arc::new(tempfile::tempfile().expect("tempfile")),
        }
    }

    fn avav_metadata(text: &str) -> ChdMetadataHeader {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        ChdMetadataHeader {
            tag: CHD_METADATA_TAG_AVAV,
            flags: 0,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data,
        }
    }

    fn avld_metadata(data: Vec<u8>) -> ChdMetadataHeader {
        ChdMetadataHeader {
            tag: CHD_METADATA_TAG_AVLD,
            flags: 0,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data,
        }
    }

    #[test]
    fn ld_info_parses_interlaced_ntsc_avav() {
        let handle = test_handle(
            vec![avav_metadata(
                "FPS:59.940058 WIDTH:720 HEIGHT:262 INTERLACED:1 CHANNELS:2 SAMPLERATE:48000",
            )],
            4096,
            4096 * 25,
        );

        let ld = extract_ld_info(&handle, &handle.header).expect("ld info");
        assert_eq!(ld.fps, "59.940058");
        assert_eq!(ld.width, 720);
        assert_eq!(ld.height, 262);
        assert!(ld.interlaced);
        assert_eq!(ld.channels, 2);
        assert_eq!(ld.sample_rate, 48000);
        assert_eq!(ld.frame_count, 25);
        assert!(ld.vbi.is_none());
    }

    #[test]
    fn ld_info_parses_progressive_avav() {
        let handle = test_handle(
            vec![avav_metadata(
                "FPS:29.970029 WIDTH:640 HEIGHT:480 INTERLACED:0 CHANNELS:2 SAMPLERATE:44100",
            )],
            4096,
            4096 * 5,
        );

        let ld = extract_ld_info(&handle, &handle.header).expect("ld info");
        assert_eq!(ld.fps, "29.970029");
        assert_eq!(ld.width, 640);
        assert_eq!(ld.height, 480);
        assert!(!ld.interlaced);
        assert_eq!(ld.channels, 2);
        assert_eq!(ld.sample_rate, 44100);
    }

    #[test]
    fn ld_info_summarizes_avld_vbi_records() {
        let records = [
            VbiMetadata {
                white: true,
                line16: VBI_CODE_LEADIN,
                ..Default::default()
            },
            VbiMetadata {
                white: false,
                line1718: 0xf00001, // CAV picture 1
                ..Default::default()
            },
            VbiMetadata {
                white: true,
                line1718: 0xf00002, // CAV picture 2
                ..Default::default()
            },
            VbiMetadata {
                white: false,
                line1718: 0xf00003, // CAV picture 3
                ..Default::default()
            },
            VbiMetadata {
                white: true,
                line1718: 0xf00005, // CAV picture 5
                ..Default::default()
            },
            VbiMetadata {
                white: false,
                line1718: 0x812ddd, // chapter 12
                ..Default::default()
            },
            VbiMetadata {
                white: false,
                line16: VBI_CODE_LEADOUT,
                ..Default::default()
            },
            VbiMetadata {
                white: true,
                ..Default::default()
            },
        ];

        let mut data = vec![0u8; records.len() * VBI_PACKED_BYTES];
        for (i, record) in records.iter().enumerate() {
            let start = i * VBI_PACKED_BYTES;
            crate::laserdisc::vbi::vbi_metadata_pack(
                &mut data[start..start + VBI_PACKED_BYTES],
                i as u32,
                record,
            );
        }

        let handle = test_handle(
            vec![
                avav_metadata(
                    "FPS:29.970029 WIDTH:640 HEIGHT:480 INTERLACED:0 CHANNELS:2 SAMPLERATE:44100",
                ),
                avld_metadata(data),
            ],
            4096,
            4096 * 100,
        );

        let ld = extract_ld_info(&handle, &handle.header).expect("ld info");
        assert_eq!(ld.frame_count, 8);
        let vbi = ld.vbi.expect("vbi summary");
        assert_eq!(vbi.disc_type, LdDiscType::Cav);
        assert_eq!(vbi.white_flag_count, 4);
        assert_eq!(vbi.cav_picture_min, Some(1));
        assert_eq!(vbi.cav_picture_max, Some(5));
        assert_eq!(vbi.chapter_min, Some(12));
        assert_eq!(vbi.chapter_max, Some(12));
        assert!(vbi.lead_in);
        assert!(vbi.lead_out);
        assert_eq!(vbi.clv_start_time, None);
        assert_eq!(vbi.clv_end_time, None);
    }

    #[test]
    fn ld_info_absent_without_avav_tag() {
        let handle = test_handle(vec![], 4096, 4096 * 10);
        assert!(extract_ld_info(&handle, &handle.header).is_none());
    }

    #[test]
    fn ld_info_none_for_garbled_avav() {
        // FPS fraction must be exactly six digits; "29.97" is garbled.
        let handle = test_handle(
            vec![avav_metadata(
                "FPS:29.97 WIDTH:640 HEIGHT:480 INTERLACED:0 CHANNELS:2 SAMPLERATE:44100",
            )],
            4096,
            4096 * 10,
        );
        assert!(extract_ld_info(&handle, &handle.header).is_none());
    }

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
