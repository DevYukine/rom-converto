//! LaserDisc `.avi` metadata: container header fields, the CHD field
//! geometry [`LdParams`] derives from them, and (for uncompressed video) a
//! summary of the VBI codes recovered from every field.

use std::io::{Read, Seek};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::laserdisc::avi::{AviFile, LdParams};
pub use crate::laserdisc::vbi::{LdClvTime, LdDiscType};
use crate::laserdisc::vbi::{
    VBI_CODE_CLV, VBI_CODE_LEADIN, VBI_CODE_LEADOUT, VbiMetadata, vbi_cav_picture, vbi_chapter,
    vbi_clv_time, vbi_parse_all,
};

/// Metadata read from a laserdisc rip's `.avi`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LdAviInfo {
    // Container header, as the AVI reports it.
    pub video_fourcc: String,
    pub video_width: u32,
    pub video_height: u32,
    pub fps: f64,
    pub frame_count: u32,
    pub duration_seconds: f64,
    pub audio_channels: u32,
    pub audio_rate: u32,
    pub audio_bits: u32,
    pub audio_sample_count: u64,
    pub file_size_bytes: u64,

    // Laserdisc CHD projection: the field geometry that fixes the hunk size.
    pub interlaced: bool,
    pub field_height: u32,
    pub fields: u32,
    pub max_samples_per_field: u32,
    pub bytes_per_frame: u32,
    pub fps_times_1million: u32,
    pub av_metadata: String,

    /// VBI code summary, present only for uncompressed (YUY2/UYVY) video.
    pub vbi: Option<LdVbiSummary>,
}

/// Summary of the Philips codes recovered from every field's VBI lines.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LdVbiSummary {
    pub fields_scanned: u32,
    pub white_flag_count: u32,
    pub lead_in: bool,
    pub lead_out: bool,
    pub disc_type: LdDiscType,
    pub cav_picture_min: Option<u32>,
    pub cav_picture_max: Option<u32>,
    pub clv_start: Option<LdClvTime>,
    pub clv_end: Option<LdClvTime>,
    pub chapter_min: Option<u32>,
    pub chapter_max: Option<u32>,
    pub fields_without_code: u32,
}

/// True for the uncompressed formats [`AviFile`] accepts.
fn is_uncompressed(format: &[u8; 4]) -> bool {
    *format == *b"YUY2" || *format == *b"UYVY"
}

/// Renders a fourcc as ASCII when printable, hex otherwise.
fn fourcc_to_string(fourcc: &[u8; 4]) -> String {
    if fourcc.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(fourcc).into_owned()
    } else {
        format!(
            "{:02x}{:02x}{:02x}{:02x}",
            fourcc[0], fourcc[1], fourcc[2], fourcc[3]
        )
    }
}

/// Reads a laserdisc rip's `.avi`: container header, projected CHD field
/// geometry, and (for uncompressed video) a VBI code summary.
///
/// # Errors
///
/// Fails on I/O errors, malformed RIFF structure, or non-PCM audio -- see
/// [`AviFile::open`]. A compressed video codec is not an error here: the
/// container info is still returned, with `vbi` left `None`.
pub fn read_info(path: &Path) -> Result<LdAviInfo> {
    let file_size_bytes = std::fs::metadata(path)?.len();
    let mut avi = AviFile::open(path)?;
    let params = avi.ld_params()?;
    let info = avi.info().clone();

    let fps = f64::from(info.video_timescale) / f64::from(info.video_sampletime);
    let duration_seconds = f64::from(info.video_numsamples) / fps;

    let vbi = is_uncompressed(&info.video_format)
        .then(|| scan_vbi(&mut avi, info.video_width, info.video_height, &params))
        .transpose()?;

    Ok(LdAviInfo {
        video_fourcc: fourcc_to_string(&info.video_format),
        video_width: info.video_width,
        video_height: info.video_height,
        fps,
        frame_count: info.video_numsamples,
        duration_seconds,
        audio_channels: info.audio_channels,
        audio_rate: info.audio_samplerate,
        audio_bits: info.audio_samplebits,
        audio_sample_count: info.audio_numsamples,
        file_size_bytes,
        interlaced: params.interlaced,
        field_height: params.height,
        fields: params.frame_count,
        max_samples_per_field: params.max_samples_per_frame,
        bytes_per_frame: params.bytes_per_frame,
        fps_times_1million: params.fps_times_1million,
        av_metadata: params.av_metadata(),
        vbi,
    })
}

/// Scans every field's VBI lines out of the raw AVI frames.
///
/// An interlaced source packs two fields (even/odd scanlines) into each raw
/// AVI frame; field `n`'s scanlines are rows `n % 2, n % 2 + 2, ...` of the
/// full-frame buffer, so the field is addressed by slicing off the parity
/// row and doubling `vbi_parse_all`'s row stride rather than copying.
fn scan_vbi<R: Read + Seek>(
    avi: &mut AviFile<R>,
    width: u32,
    raw_height: u32,
    params: &LdParams,
) -> Result<LdVbiSummary> {
    let mut summary = LdVbiSummary::default();
    let mut saw_clv_marker = false;
    let mut frame = vec![0u16; (width * raw_height) as usize];
    let parities: &[u32] = if params.interlaced { &[0, 1] } else { &[0] };

    for raw_frame in 0..avi.info().video_numsamples {
        avi.read_video_frame(raw_frame, &mut frame)?;
        for &parity in parities {
            let stride = if params.interlaced { width * 2 } else { width };
            let field = &frame[(parity * width) as usize..];
            let vbi = vbi_parse_all(field, stride as usize, width as usize, 8);
            accumulate(&mut summary, &vbi, &mut saw_clv_marker);
        }
    }

    summary.disc_type = if summary.cav_picture_min.is_some() {
        LdDiscType::Cav
    } else if summary.clv_start.is_some() || saw_clv_marker {
        LdDiscType::Clv
    } else {
        LdDiscType::Unknown
    };
    Ok(summary)
}

/// Folds one field's decoded VBI codes into the running summary. Control
/// codes (lead-in/out, the CLV flag) and content codes (picture number,
/// chapter, CLV time) can land on either line 16 or the merged 17/18 value,
/// so both are checked the same way.
///
/// `vbi_cav_picture`'s mask only pins down a code's top nibble, which a CLV
/// timecode also carries, so the more specific checks run first and each
/// code counts toward at most one bucket.
fn accumulate(summary: &mut LdVbiSummary, vbi: &VbiMetadata, saw_clv_marker: &mut bool) {
    summary.fields_scanned += 1;
    if vbi.white {
        summary.white_flag_count += 1;
    }

    let mut coded = false;
    for code in [vbi.line16, vbi.line1718] {
        if code == 0 {
            continue;
        }
        coded = true;
        if code == VBI_CODE_LEADIN {
            summary.lead_in = true;
        } else if code == VBI_CODE_LEADOUT {
            summary.lead_out = true;
        } else if code == VBI_CODE_CLV {
            *saw_clv_marker = true;
        } else if let Some(chapter) = vbi_chapter(code) {
            summary.chapter_min = Some(summary.chapter_min.map_or(chapter, |m| m.min(chapter)));
            summary.chapter_max = Some(summary.chapter_max.map_or(chapter, |m| m.max(chapter)));
        } else if let Some((hours, minutes)) = vbi_clv_time(code) {
            let time = LdClvTime { hours, minutes };
            summary.clv_start.get_or_insert(time);
            summary.clv_end = Some(time);
        } else if let Some(picture) = vbi_cav_picture(code) {
            summary.cav_picture_min =
                Some(summary.cav_picture_min.map_or(picture, |m| m.min(picture)));
            summary.cav_picture_max =
                Some(summary.cav_picture_max.map_or(picture, |m| m.max(picture)));
        }
    }
    if !coded {
        summary.fields_without_code += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::laserdisc::avi::test_fixtures::{AviSpec, build_avi};
    use crate::laserdisc::vbi::VBI_CODE_BITS;

    /// Pixels per Manchester bit cell; matches `laserdisc::vbi`'s own tests.
    const VBI_CLOCK: usize = 20;
    const VBI_WIDTH: usize = VBI_CODE_BITS * VBI_CLOCK;

    /// Synthesizes one Manchester-coded VBI line as big-endian YUY2 words
    /// (luma in the high byte), mirroring `laserdisc::vbi`'s test helper.
    fn manchester_words(code: u32) -> Vec<u16> {
        let firstedge = (VBI_CLOCK / 2) as i64;
        (0..VBI_WIDTH)
            .map(|p| {
                let rel = p as i64 - firstedge;
                let bit_index = (rel + (VBI_CLOCK / 2) as i64)
                    .div_euclid(VBI_CLOCK as i64)
                    .clamp(0, VBI_CODE_BITS as i64 - 1);
                let bit = (code >> (VBI_CODE_BITS as i64 - 1 - bit_index)) & 1 == 1;
                let level = if rel - bit_index * VBI_CLOCK as i64 >= 0 {
                    bit
                } else {
                    !bit
                };
                if level { 0xff00u16 } else { 0x0000u16 }
            })
            .collect()
    }

    /// A white-flag line: the given fraction of the line reads full luma.
    fn white_words(white_pixels: usize) -> Vec<u16> {
        (0..VBI_WIDTH)
            .map(|x| {
                if x < white_pixels {
                    0xff00u16
                } else {
                    0x0000u16
                }
            })
            .collect()
    }

    /// Writes big-endian YUY2 words into raw frame bytes at `row`.
    fn write_row(frame: &mut [u8], width: usize, row: usize, words: &[u16]) {
        let base = row * width * 2;
        for (i, word) in words.iter().enumerate() {
            frame[base + i * 2..base + i * 2 + 2].copy_from_slice(&word.to_be_bytes());
        }
    }

    fn base_spec<'a>(
        width: u32,
        height: u32,
        timescale: u32,
        frames: &'a [Vec<u8>],
    ) -> AviSpec<'a> {
        AviSpec {
            width,
            height,
            timescale,
            sampletime: 1,
            video_format: *b"YUY2",
            frames,
            channels: 2,
            sample_rate: 48_000,
            sample_bits: 16,
            samples: &[],
            index: true,
            block_align_override: None,
            video_length_override: None,
        }
    }

    #[test]
    fn serde_tags_laserdisc_as_laser_disc() {
        use crate::info::InfoResult;
        let result = InfoResult::LaserDisc(LdAviInfo::default());
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(json.contains("\"kind\":\"laser_disc\""), "{json}");
        let round_tripped: InfoResult = serde_json::from_str(&json).expect("round-trip");
        match round_tripped {
            InfoResult::LaserDisc(_) => {}
            other => panic!("expected LaserDisc, got {other:?}"),
        }
    }

    #[test]
    fn progressive_avi_reports_container_and_clv_vbi() {
        let width = VBI_WIDTH as u32;
        let height = 24u32; // progressive: well under the 288-line interlace floor
        let frame_bytes = (width * height * 2) as usize;

        let mut frame0 = vec![0u8; frame_bytes];
        write_row(
            &mut frame0,
            width as usize,
            11,
            &white_words(VBI_WIDTH * 95 / 100),
        );
        write_row(
            &mut frame0,
            width as usize,
            16,
            &manchester_words(VBI_CODE_CLV),
        );
        write_row(&mut frame0, width as usize, 17, &manchester_words(0xf1dd32)); // CLV 1:32
        write_row(&mut frame0, width as usize, 18, &manchester_words(0xf1dd32));

        let mut frame1 = vec![0u8; frame_bytes];
        write_row(&mut frame1, width as usize, 17, &manchester_words(0xf1dd45)); // CLV 1:45
        write_row(&mut frame1, width as usize, 18, &manchester_words(0xf1dd45));

        let frames = vec![frame0, frame1];
        let data = build_avi(&base_spec(width, height, 30, &frames));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("progressive.avi");
        std::fs::write(&path, data).expect("write fixture");

        let info = read_info(&path).expect("read_info");
        assert_eq!(info.video_fourcc, "YUY2");
        assert_eq!(info.video_width, width);
        assert_eq!(info.video_height, height);
        assert_eq!(info.frame_count, 2);
        assert!(!info.interlaced);
        assert_eq!(info.field_height, height);
        assert_eq!(info.fields, 2);

        let vbi = info.vbi.expect("uncompressed video carries a VBI summary");
        assert_eq!(vbi.fields_scanned, 2);
        assert_eq!(vbi.white_flag_count, 1);
        assert_eq!(vbi.fields_without_code, 0);
        assert_eq!(vbi.disc_type, LdDiscType::Clv);
        assert_eq!(
            vbi.clv_start,
            Some(LdClvTime {
                hours: 1,
                minutes: 32
            })
        );
        assert_eq!(
            vbi.clv_end,
            Some(LdClvTime {
                hours: 1,
                minutes: 45
            })
        );
        assert_eq!(vbi.cav_picture_min, None);
    }

    #[test]
    fn interlaced_avi_extracts_vbi_per_field() {
        let width = VBI_WIDTH as u32;
        let height = 300u32; // multiple of 2, > 288: triggers field halving
        let frame_bytes = (width * height * 2) as usize;

        let mut frame = vec![0u8; frame_bytes];
        // Top field (parity 0): rows 22, 32, 34, 36 = field rows 11, 16, 17, 18.
        write_row(
            &mut frame,
            width as usize,
            22,
            &white_words(VBI_WIDTH * 95 / 100),
        );
        write_row(
            &mut frame,
            width as usize,
            32,
            &manchester_words(VBI_CODE_LEADIN),
        );
        write_row(&mut frame, width as usize, 34, &manchester_words(0xf00042));
        write_row(&mut frame, width as usize, 36, &manchester_words(0xf00042));
        // Bottom field (parity 1) stays blank: no white flag, no codes.

        let frames = vec![frame];
        let data = build_avi(&base_spec(width, height, 30, &frames));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("interlaced.avi");
        std::fs::write(&path, data).expect("write fixture");

        let info = read_info(&path).expect("read_info");
        assert!(info.interlaced);
        assert_eq!(info.video_height, height);
        assert_eq!(info.field_height, height / 2);
        assert_eq!(info.fields, 2);

        let vbi = info.vbi.expect("uncompressed video carries a VBI summary");
        assert_eq!(vbi.fields_scanned, 2);
        assert_eq!(vbi.white_flag_count, 1);
        assert!(vbi.lead_in);
        assert_eq!(vbi.disc_type, LdDiscType::Cav);
        assert_eq!(vbi.cav_picture_min, Some(42));
        assert_eq!(vbi.cav_picture_max, Some(42));
        assert_eq!(vbi.fields_without_code, 1); // the blank bottom field
    }

    #[test]
    fn compressed_video_reports_container_info_with_no_vbi() {
        let frames = vec![vec![0u8; 8]];
        let mut spec = base_spec(4, 2, 30, &frames);
        spec.video_format = *b"HFYU";
        let data = build_avi(&spec);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("compressed.avi");
        std::fs::write(&path, data).expect("write fixture");

        let info = read_info(&path).expect("header parsing succeeds for any codec");
        assert_eq!(info.video_fourcc, "HFYU");
        assert_eq!(info.video_width, 4);
        assert_eq!(info.video_height, 2);
        assert_eq!(info.frame_count, 1);
        assert!(info.vbi.is_none());
    }
}
