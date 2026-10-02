use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::models::{
    CHD_METADATA_FLAG_HASHED, CHD_METADATA_HEADER_BYTES, CHD_METADATA_RESERVED_BYTES,
    CHD_METADATA_TAG_AV, CHD_METADATA_TAG_AV_LD, CHD_V5_HEADER_SIZE, ChdMetadataHeader, SHA1_BYTES,
};
use crate::disc::cue::models::{CueSheet, TrackType};
use crate::disc::laserdisc::avi::LdParams;
use crate::disc::laserdisc::vbi::VBI_PACKED_BYTES;
use binrw::BinWrite;
use sha1::{Digest, Sha1};
use std::io::Cursor;

// The reference writer leaves PGTYPE at its MODE1 default when the
// pregap is a bare `PREGAP` directive, i.e. the gap frames are not
// stored in the bin.
const PREGAP_TYPE: &str = "MODE1";

#[derive(Debug, Clone)]
pub struct MetadataHash {
    pub tag: [u8; 4],
    pub sha1: [u8; SHA1_BYTES],
}

#[derive(Debug)]
pub struct MetadataBlock {
    pub bytes: Vec<u8>,
    pub hashes: Vec<MetadataHash>,
}

/// Serialized `DVD ` marker block: the reference writer's whole DVD
/// metadata is the hashed empty string.
pub fn generate_dvd_metadata() -> ChdResult<MetadataBlock> {
    let metadata = ChdMetadataHeader::new_dvd_metadata();
    let mut bytes = Vec::new();
    metadata.write(&mut Cursor::new(&mut bytes))?;

    let sha1: [u8; SHA1_BYTES] = Sha1::digest(&metadata.data).into();
    Ok(MetadataBlock {
        bytes,
        hashes: vec![MetadataHash {
            tag: metadata.tag,
            sha1,
        }],
    })
}

/// Per-track frame counts in source order, following the upstream
/// `parse_cue` (`cdrom.cpp`): `file_sectors[i]` is the sector count of
/// `cue_sheet.files[i]`. Inside a shared FILE, a track ends where the
/// next track's INDEX 00 (else INDEX 01) begins. The last track of a
/// FILE runs to its end, and a track alone in its FILE spans the whole
/// file. The first track of each FILE always starts at sector 0 so a
/// nonzero first INDEX cannot silently drop the head of the bin. Each
/// track starts where the previous one in its FILE ended and boundaries
/// clamp to the file, so the counts of a FILE's tracks always sum to its
/// sector count.
///
/// # Errors
/// [`ChdError::CueTrackMissingIndex01`] when a track has no INDEX 01,
/// which the reference parser rejects outright, and
/// [`ChdError::EmptyCueTrack`] when a track resolves to no frames (the
/// reference parser refuses that inside a shared FILE; here it is refused
/// everywhere, since a CHD with an empty track is never what the user
/// wanted).
pub fn track_frames(cue_sheet: &CueSheet, file_sectors: &[u32]) -> ChdResult<Vec<u32>> {
    let tracks = &cue_sheet.tracks;
    let mut start = 0u32;
    (0..tracks.len())
        .map(|idx| {
            let track = tracks[idx].number;
            if tracks[idx].primary_index_lba().is_none() {
                return Err(ChdError::CueTrackMissingIndex01 { track });
            }
            let file = tracks[idx].file_index;
            let sectors = file_sectors[file];
            if idx == 0 || tracks[idx - 1].file_index != file {
                start = 0;
            }
            let end = match tracks.get(idx + 1) {
                Some(next) if next.file_index == file => {
                    next.boundary_lba().unwrap_or(0).clamp(start, sectors)
                }
                _ => sectors,
            };
            let frames = end - start;
            start = end;
            if frames == 0 {
                return Err(ChdError::EmptyCueTrack { track });
            }
            Ok(frames)
        })
        .collect()
}

/// Per-frame maps for the physical CHD stream, each track padded to
/// the reference implementation's 4-frame boundary. `.0` is `true` where
/// the frame carries source data (`false` for the zero padding frames
/// appended per track); `.1` is `true` where the frame belongs to an
/// AUDIO track. The reference implementation byte-swaps audio sector
/// samples on ingest and swaps them back on extract; the writer consults
/// `.1` to swap the right frames before hashing and compressing. `frames`
/// is the per-track source frame count from [`track_frames`], shared with
/// the CHT2 metadata.
pub fn cd_frame_layout(cue_sheet: &CueSheet, frames: &[u32]) -> (Vec<bool>, Vec<bool>) {
    let mut is_data = Vec::new();
    let mut is_audio = Vec::new();
    for (track, &frames) in cue_sheet.tracks.iter().zip(frames) {
        let padded = crate::disc::chd::padded_track_frames(frames);
        let audio = matches!(track.track_type, TrackType::Audio);
        is_data.extend(std::iter::repeat_n(true, frames as usize));
        is_data.extend(std::iter::repeat_n(false, (padded - frames) as usize));
        is_audio.extend(std::iter::repeat_n(audio, padded as usize));
    }
    (is_data, is_audio)
}

/// One CHT2 metadata entry per track, chained through the reserved
/// bytes (the on-disk `next` offset), exactly as the upstream
/// `write_metadata` lays them out right after the V5 header. A pregap
/// stored in the bin (cue `INDEX 00`) is counted in `FRAMES:` and
/// flagged by a `V` prefix on `PGTYPE:`; a bare `PREGAP` directive is
/// recorded without stored frames. `frames` comes from
/// [`track_frames`].
pub fn generate_cd_metadata(cue_sheet: &CueSheet, frames: &[u32]) -> ChdResult<MetadataBlock> {
    let mut entries = Vec::new();
    for (track, &frames) in cue_sheet.tracks.iter().zip(frames) {
        let (pregap, pgtype) = match track.stored_pregap() {
            Some(stored) => (stored, format!("V{}", track.track_type.chd_metadata_type())),
            None => (
                track.pregap.map(|p| p.to_lba()).unwrap_or(0),
                PREGAP_TYPE.to_string(),
            ),
        };

        // Format: TRACK:n TYPE:type SUBTYPE:NONE FRAMES:nnn PREGAP:n PGTYPE:type PGSUB:NONE POSTGAP:n
        entries.push(ChdMetadataHeader::new_cd_metadata(format!(
            "TRACK:{} TYPE:{} SUBTYPE:NONE FRAMES:{} PREGAP:{} PGTYPE:{} PGSUB:NONE POSTGAP:{}",
            track.number,
            track.track_type.chd_metadata_type(),
            frames,
            pregap,
            pgtype,
            track.postgap.map(|p| p.to_lba()).unwrap_or(0)
        )));
    }

    chain_and_serialize(entries)
}

/// NTSC and PAL field heights. The reference writer emits the `AVLD`
/// blob only for these two, so anything else is a plain A/V CHD.
const LD_VBI_FIELD_HEIGHTS: [u32; 2] = [524 / 2, 624 / 2];

/// Size of the `AVLD` VBI blob these parameters call for: one packed
/// record per field at NTSC and PAL field heights, nothing otherwise.
pub fn ld_vbi_bytes(params: &LdParams, vbi_frames: usize) -> usize {
    if LD_VBI_FIELD_HEIGHTS.contains(&params.height) {
        vbi_frames * VBI_PACKED_BYTES
    } else {
        0
    }
}

/// `AVAV` A/V metadata, plus a reserved `AVLD` VBI blob of
/// `vbi_frames` packed records when the field height is NTSC or PAL.
///
/// The blob is emitted zero-filled; the writer backfills it once every
/// field has been parsed. That is safe because the `AVLD` entry is not
/// hashed, so it never feeds the overall SHA-1.
pub fn generate_ld_metadata(params: &LdParams, vbi_frames: usize) -> ChdResult<MetadataBlock> {
    let mut av_data = params.av_metadata().into_bytes();
    av_data.push(0);

    let mut entries = vec![ChdMetadataHeader {
        tag: CHD_METADATA_TAG_AV,
        flags: CHD_METADATA_FLAG_HASHED,
        reserved: [0; CHD_METADATA_RESERVED_BYTES],
        data: av_data,
    }];
    let vbi_bytes = ld_vbi_bytes(params, vbi_frames);
    if vbi_bytes > 0 {
        entries.push(ChdMetadataHeader {
            tag: CHD_METADATA_TAG_AV_LD,
            flags: 0,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data: vec![0; vbi_bytes],
        });
    }

    chain_and_serialize(entries)
}

/// Serializes metadata entries copied verbatim from another CHD, relinking
/// their `next` offsets for the new file's layout.
pub fn copy_metadata(mut entries: Vec<ChdMetadataHeader>) -> ChdResult<MetadataBlock> {
    for entry in &mut entries {
        entry.reserved = [0; CHD_METADATA_RESERVED_BYTES];
    }
    chain_and_serialize(entries)
}

/// Link the entries through their reserved `next` offsets (the metadata
/// list starts right after the V5 header) and serialize them, hashing
/// every entry that carries the checksum flag.
fn chain_and_serialize(mut entries: Vec<ChdMetadataHeader>) -> ChdResult<MetadataBlock> {
    let mut offset = CHD_V5_HEADER_SIZE as u64;
    let count = entries.len();
    for (i, entry) in entries.iter_mut().enumerate() {
        let size = CHD_METADATA_HEADER_BYTES as u64 + entry.data.len() as u64;
        if i + 1 < count {
            entry.reserved = (offset + size).to_be_bytes();
        }
        offset += size;
    }

    let mut metadata_buffer = Vec::new();
    let mut hashes = Vec::new();
    let mut cursor = Cursor::new(&mut metadata_buffer);
    for entry in &entries {
        entry.write(&mut cursor)?;
        if entry.flags & CHD_METADATA_FLAG_HASHED != 0 {
            let sha1: [u8; SHA1_BYTES] = Sha1::digest(&entry.data).into();
            hashes.push(MetadataHash {
                tag: entry.tag,
                sha1,
            });
        }
    }

    Ok(MetadataBlock {
        bytes: metadata_buffer,
        hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::chd::reader::cue_generator::{generate_cue_sheet, parse_chd_track_metadata};
    use crate::disc::cue::models::{CueFile, FileType, Index, Msf, Track};

    /// A single-bin sheet with one MODE1/2352 track and the given
    /// pregap and postgap directives in frames.
    fn sheet(pregap: Option<u32>, postgap: Option<u32>) -> CueSheet {
        CueSheet {
            files: vec![CueFile {
                filename: "game.bin".to_string(),
                file_type: FileType::Binary,
            }],
            tracks: vec![Track {
                number: 1,
                track_type: TrackType::Mode1_2352,
                indices: vec![Index {
                    number: 1,
                    position: Msf::from_lba(0),
                }],
                pregap: pregap.map(Msf::from_lba),
                postgap: postgap.map(Msf::from_lba),
                file_index: 0,
            }],
        }
    }

    /// The CHT2 text of a single-entry block: one 16-byte header, then
    /// the NUL-terminated string.
    fn cht2_text(block: &MetadataBlock) -> String {
        String::from_utf8_lossy(&block.bytes[CHD_METADATA_HEADER_BYTES..])
            .trim_end_matches('\0')
            .to_string()
    }

    /// The postgap value travels cue -> CHT2 -> generated cue, the way
    /// the upstream `write_metadata` and `output_track_metadata` carry it;
    /// a track without one keeps `POSTGAP:0` and no cue line.
    #[test]
    fn postgap_round_trips_through_cht2_metadata() {
        let block = generate_cd_metadata(&sheet(None, Some(150)), &[300]).unwrap();
        let text = cht2_text(&block);
        assert!(text.contains("POSTGAP:150"), "metadata: {text}");

        let tracks = parse_chd_track_metadata(&text).unwrap();
        let cue = generate_cue_sheet(&["game.bin".to_string()], &tracks);
        let index_pos = cue.find("INDEX 01").expect("INDEX 01 present");
        let postgap_pos = cue.find("    POSTGAP 00:02:00\r\n").expect("POSTGAP line");
        assert!(index_pos < postgap_pos, "cue: {cue}");

        let block = generate_cd_metadata(&sheet(None, None), &[300]).unwrap();
        let text = cht2_text(&block);
        assert!(text.contains("POSTGAP:0"), "metadata: {text}");
        let cue = generate_cue_sheet(
            &["game.bin".to_string()],
            &parse_chd_track_metadata(&text).unwrap(),
        );
        assert!(!cue.contains("POSTGAP"), "cue: {cue}");
    }
}
