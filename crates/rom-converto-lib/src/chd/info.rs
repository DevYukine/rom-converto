//! `info` extractor for CHD files. Surfaces the header, hashes,
//! per-track metadata, optional DVD or hard disk geometry, and the
//! chdman build string when present, for every format version.

use crate::chd::legacy::{LegacyChd, LegacyChdHeader};
use crate::chd::models::{
    CHD_METADATA_TAG_AV as CHD_METADATA_TAG_AVAV, CHD_METADATA_TAG_AV_LD as CHD_METADATA_TAG_AVLD,
    ChdHeaderV5, ChdMetadataHeader, ChdVersion,
};
use crate::chd::reader::cue_generator::parse_chd_track_metadata;
use crate::chd::reader::{chd_version, open_chd_sync};
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
    pub raw_sha1: Option<String>,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub parent_sha1: Option<String>,
    pub parent_md5: Option<String>,
    pub tracks: Vec<ChdTrack>,
    pub metadata_tags: Vec<ChdMetadataTagSummary>,
    /// Chdman build string from the optional `VERS` metadata tag.
    pub version_string: Option<String>,
    /// DVD-only fields derived when a `DVD ` metadata tag is present.
    pub dvd: Option<ChdDvdInfo>,
    /// Hard disk geometry from a v1/v2 header or a `GDDD` metadata tag.
    pub hard_disk: Option<ChdHardDiskInfo>,
    /// LaserDisc-only fields derived when an `AVAV` metadata tag is present.
    pub ld: Option<ChdLdInfo>,
    /// Metadata of the PlayStation-family disc the CHD carries, when it
    /// carries one.
    pub content: Option<DiscContent>,
}

/// Hard disk geometry, from a V1/V2 header or a `GDDD` metadata entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChdHardDiskInfo {
    pub cylinders: u32,
    pub heads: u32,
    pub sectors: u32,
    pub sector_bytes: u32,
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
/// Returns an error if the file cannot be opened or is not a valid CHD of a
/// supported format version.
pub fn read_info(path: &Path) -> Result<ChdInfo> {
    match chd_version(path).map_err(into_anyhow)? {
        5 => {
            let handle = open_chd_sync(path).map_err(into_anyhow)?;
            add_metadata(v5_header_info(&handle.header), &handle.metadata, path)
        }
        _ => {
            let chd = LegacyChd::open(path).map_err(into_anyhow)?;
            add_metadata(legacy_header_info(chd.header()), chd.metadata(), path)
        }
    }
}

/// Fills the fields both format generations derive from the metadata
/// chain and the file itself.
fn add_metadata(mut info: ChdInfo, metadata: &[ChdMetadataHeader], path: &Path) -> Result<ChdInfo> {
    info.physical_bytes = std::fs::metadata(path)?.len();
    info.compression_ratio = if info.logical_bytes > 0 {
        (info.physical_bytes as f64 / info.logical_bytes as f64) * 100.0
    } else {
        0.0
    };
    info.tracks = extract_tracks(metadata);
    info.metadata_tags = metadata
        .iter()
        .map(|m| ChdMetadataTagSummary {
            tag: fourcc_to_string(&m.tag).unwrap_or_else(|| hex::encode(m.tag)),
            length: m.data.len() as u32,
        })
        .collect();
    info.version_string = extract_version_string(metadata);
    info.dvd = extract_dvd_info(metadata, info.logical_bytes);
    info.ld = extract_ld_info(metadata, info.logical_bytes, info.hunk_bytes);
    // chdman trusts the GDDD entry over the v1/v2 header geometry.
    if let Some(geometry) = extract_hard_disk_info(metadata) {
        info.hard_disk = Some(geometry);
    }
    info.content = crate::sony_disc::chd_disc_content(path);
    Ok(info)
}

/// Header-derived fields of a V5 file.
fn v5_header_info(header: &ChdHeaderV5) -> ChdInfo {
    ChdInfo {
        version: 5,
        compressors: header
            .compressors()
            .iter()
            .filter_map(fourcc_to_string)
            .collect(),
        hunk_bytes: header.hunk_bytes,
        unit_bytes: header.unit_bytes,
        hunk_count: header.logical_bytes.div_ceil(header.hunk_bytes as u64),
        logical_bytes: header.logical_bytes,
        raw_sha1: Some(hex::encode(header.raw_sha1)),
        sha1: Some(hex::encode(header.sha1)),
        parent_sha1: non_zero_hash(&header.parent_sha1),
        ..Default::default()
    }
}

/// Header-derived fields of a v1-v4 file.
fn legacy_header_info(header: &LegacyChdHeader) -> ChdInfo {
    // v3 stores the raw-data SHA-1 in the field v4 reuses for the
    // combined raw+metadata digest.
    let (raw_sha1, sha1) = match header.version {
        ChdVersion::V3 => (header.sha1, None),
        _ => (header.raw_sha1, header.sha1),
    };

    ChdInfo {
        version: header.version as u8,
        compressors: match header.compression {
            1 => vec!["zlib".to_string()],
            2 => vec!["zlib+".to_string()],
            3 => vec!["avhuff".to_string()],
            _ => Vec::new(),
        },
        hunk_bytes: header.hunk_bytes,
        unit_bytes: header.unit_bytes,
        hunk_count: u64::from(header.total_hunks),
        logical_bytes: header.logical_bytes,
        raw_sha1: raw_sha1.and_then(|h| non_zero_hash(&h)),
        sha1: sha1.and_then(|h| non_zero_hash(&h)),
        md5: header.md5.and_then(|h| non_zero_hash(&h)),
        parent_sha1: header.parent_sha1.and_then(|h| non_zero_hash(&h)),
        parent_md5: header.parent_md5.and_then(|h| non_zero_hash(&h)),
        hard_disk: header.chs.as_ref().map(|chs| ChdHardDiskInfo {
            cylinders: chs.cylinders,
            heads: chs.heads,
            sectors: chs.sectors,
            sector_bytes: chs.sector_bytes,
        }),
        ..Default::default()
    }
}

/// Hex-encodes a hash, treating an all-zero value as absent.
fn non_zero_hash(bytes: &[u8]) -> Option<String> {
    bytes.iter().any(|b| *b != 0).then(|| hex::encode(bytes))
}

const CHD_METADATA_TAG_VERS: [u8; 4] = *b"VERS";
const CHD_METADATA_TAG_DVD: [u8; 4] = *b"DVD ";

/// Reads a `GDDD` hard disk entry, formatted
/// `CYLS:%d,HEADS:%d,SECS:%d,BPS:%d`.
fn extract_hard_disk_info(metadata: &[ChdMetadataHeader]) -> Option<ChdHardDiskInfo> {
    let entry = metadata
        .iter()
        .find(|m| m.tag == crate::chd::models::CHD_METADATA_TAG_HARD_DISK)?;
    let (cylinders, heads, sectors, sector_bytes) = crate::chd::legacy::parse_gddd(&entry.data)?;
    Some(ChdHardDiskInfo {
        cylinders,
        heads,
        sectors,
        sector_bytes,
    })
}

fn extract_version_string(metadata: &[ChdMetadataHeader]) -> Option<String> {
    metadata
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

fn extract_dvd_info(metadata: &[ChdMetadataHeader], logical_bytes: u64) -> Option<ChdDvdInfo> {
    let has_dvd_tag = metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_DVD);
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
    metadata: &[ChdMetadataHeader],
    logical_bytes: u64,
    hunk_bytes: u32,
) -> Option<ChdLdInfo> {
    let avav = metadata.iter().find(|m| m.tag == CHD_METADATA_TAG_AVAV)?;
    let (fps, width, height, interlaced, channels, sample_rate) = parse_av_metadata(&avav.data)?;

    let avld = metadata
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
            logical_bytes.div_ceil(u64::from(hunk_bytes.max(1))) as u32,
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

fn extract_tracks(metadata: &[ChdMetadataHeader]) -> Vec<ChdTrack> {
    let Some(meta_str) = crate::chd::cd_track_metadata_text(metadata) else {
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
    use crate::chd::legacy::tests::{Fixture, TestHunk};
    use crate::chd::models::CHD_METADATA_RESERVED_BYTES;
    use crate::laserdisc::vbi::{VBI_CODE_LEADIN, VBI_CODE_LEADOUT, VBI_PACKED_BYTES, VbiMetadata};
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Writes a legacy fixture image, optionally patched with header
    /// hashes the builder does not model, and reads it back through
    /// [`read_info`].
    fn legacy_info(fixture: &Fixture, patches: &[(usize, &[u8])]) -> ChdInfo {
        let mut image = fixture.image();
        for (at, bytes) in patches {
            image[*at..*at + bytes.len()].copy_from_slice(bytes);
        }
        let mut file = NamedTempFile::new().expect("temp file");
        file.write_all(&image).expect("write fixture");
        file.flush().expect("flush fixture");
        read_info(file.path()).expect("info reads")
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
        let metadata = [avav_metadata(
            "FPS:59.940058 WIDTH:720 HEIGHT:262 INTERLACED:1 CHANNELS:2 SAMPLERATE:48000",
        )];

        let ld = extract_ld_info(&metadata, 4096 * 25, 4096).expect("ld info");
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
        let metadata = [avav_metadata(
            "FPS:29.970029 WIDTH:640 HEIGHT:480 INTERLACED:0 CHANNELS:2 SAMPLERATE:44100",
        )];

        let ld = extract_ld_info(&metadata, 4096 * 5, 4096).expect("ld info");
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

        let metadata = [
            avav_metadata(
                "FPS:29.970029 WIDTH:640 HEIGHT:480 INTERLACED:0 CHANNELS:2 SAMPLERATE:44100",
            ),
            avld_metadata(data),
        ];

        let ld = extract_ld_info(&metadata, 4096 * 100, 4096).expect("ld info");
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
        assert!(extract_ld_info(&[], 4096 * 10, 4096).is_none());
    }

    #[test]
    fn ld_info_none_for_garbled_avav() {
        // FPS fraction must be exactly six digits; "29.97" is garbled.
        let metadata = [avav_metadata(
            "FPS:29.97 WIDTH:640 HEIGHT:480 INTERLACED:0 CHANNELS:2 SAMPLERATE:44100",
        )];
        assert!(extract_ld_info(&metadata, 4096 * 10, 4096).is_none());
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

    #[test]
    fn v1_reports_md5_and_header_geometry() {
        let info = legacy_info(
            &Fixture {
                version: 1,
                hunk_bytes: 1024,
                chs: (2, 1, 4, 512),
                hunks: vec![TestHunk::Plain(vec![0xA5; 1024])],
                ..Fixture::default()
            },
            // v1 header md5 at offset 44.
            &[(44, &[0x11u8; 16])],
        );

        assert_eq!(info.version, 1);
        assert_eq!(info.compressors, ["zlib"]);
        assert_eq!(info.md5.as_deref(), Some("11".repeat(16).as_str()));
        assert_eq!(info.sha1, None);
        assert_eq!(info.raw_sha1, None);
        assert_eq!(info.parent_md5, None);
        let hard_disk = info.hard_disk.expect("v1 header geometry");
        assert_eq!(
            (
                hard_disk.cylinders,
                hard_disk.heads,
                hard_disk.sectors,
                hard_disk.sector_bytes
            ),
            (2, 1, 4, 512)
        );
    }

    #[test]
    fn v3_reports_the_header_sha1_as_the_raw_hash() {
        let info = legacy_info(
            &Fixture {
                version: 3,
                hunk_bytes: 1024,
                logical_bytes: 1024,
                hunks: vec![TestHunk::Plain(vec![0x5A; 1024])],
                ..Fixture::default()
            },
            // v3 header sha1 at offset 80.
            &[(80, &[0x22u8; 20])],
        );

        assert_eq!(info.version, 3);
        assert_eq!(info.raw_sha1.as_deref(), Some("22".repeat(20).as_str()));
        assert_eq!(info.sha1, None);
        assert!(info.hard_disk.is_none());
    }

    #[test]
    fn v4_reports_both_the_raw_and_combined_sha1() {
        let info = legacy_info(
            &Fixture {
                version: 4,
                hunk_bytes: 1024,
                logical_bytes: 1024,
                hunks: vec![TestHunk::Plain(vec![0x3C; 1024])],
                ..Fixture::default()
            },
            // v4 header sha1 at offset 48, raw sha1 at offset 88.
            &[(48, &[0x33u8; 20]), (88, &[0x44u8; 20])],
        );

        assert_eq!(info.version, 4);
        assert_eq!(info.sha1.as_deref(), Some("33".repeat(20).as_str()));
        assert_eq!(info.raw_sha1.as_deref(), Some("44".repeat(20).as_str()));
        assert_eq!(info.md5, None);
    }

    #[test]
    fn legacy_chtr_metadata_yields_tracks() {
        let info = legacy_info(
            &Fixture {
                version: 3,
                hunk_bytes: 2448,
                logical_bytes: 2448,
                metadata: vec![(
                    *b"CHTR",
                    b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1\0".to_vec(),
                )],
                hunks: vec![TestHunk::Plain(vec![0u8; 2448])],
                ..Fixture::default()
            },
            &[],
        );

        assert_eq!(info.unit_bytes, 2448);
        assert_eq!(info.tracks.len(), 1);
        assert_eq!(info.tracks[0].track_type, "MODE1_RAW");
        assert_eq!(info.tracks[0].frames, 1);
        assert_eq!(info.metadata_tags[0].tag, "CHTR");
    }
}
