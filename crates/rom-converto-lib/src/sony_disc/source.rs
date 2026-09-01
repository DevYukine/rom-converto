//! Logical-sector views of the containers a PlayStation-family disc
//! arrives in: CUE/BIN, CSO/ZSO, and CHD.
//!
//! Each source decodes only the blocks or hunks a lookup touches and
//! caches the last one, so probing a 4 GB compressed image reads a
//! handful of sectors, not the whole thing.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::cd::FRAME_SIZE;
use crate::chd::models::{CHD_METADATA_TAG_CD, CHD_METADATA_TAG_DVD};
use crate::chd::padded_track_frames;
use crate::chd::reader::cue_generator::{ChdTrackInfo, parse_chd_track_metadata};
use crate::chd::reader::worker::{
    ChdDvdExtractWorker, ChdExtractWork, ChdExtractWorker, make_chd_dvd_extract_workers,
    make_chd_extract_workers, resolve_entry,
};
use crate::chd::reader::{SyncChdHandle, open_chd_sync};
use crate::cso::compression::BlockDecompressor;
use crate::cso::reader::{CsoSyncHandle, block_spec, open_cso_sync};
use crate::cue::CueParser;
use crate::cue::models::{FileType, TrackType};
use crate::cue::to_iso::extract_user_data;
use crate::util::iso9660::SectorSource;
use crate::util::pread::file_read_exact_at;
use crate::util::worker_pool::Worker;

const SECTOR: usize = 2048;

fn invalid(msg: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn past_end() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "sector past end of image")
}

/// The data track of a CUE/BIN image, as 2048-byte user sectors.
pub(crate) struct CueSectors {
    bin: File,
    track_type: TrackType,
    start_lba: u64,
    sectors: u64,
}

impl CueSectors {
    /// Opens the first track of the CUE sheet at `path`; it must be a
    /// BINARY MODE1/MODE2 data track.
    pub fn open(path: &Path) -> io::Result<Self> {
        let sheet = CueParser::new(path)
            .parse_bytes(&std::fs::read(path)?)
            .map_err(io::Error::other)?;
        let track = sheet
            .tracks
            .first()
            .ok_or_else(|| invalid("CUE sheet contains no tracks"))?;
        let file = sheet
            .files
            .get(track.file_index)
            .ok_or_else(|| invalid("CUE sheet references no files"))?;
        if !matches!(file.file_type, FileType::Binary) {
            return Err(invalid(format!(
                "CUE sheet references a non-BINARY file: {}",
                file.filename
            )));
        }

        let bin = File::open(path.parent().unwrap_or(Path::new(".")).join(&file.filename))?;
        let bin_bytes = bin.metadata()?.len();
        let start_lba = track.primary_index_lba().unwrap_or(0) as u64;
        let sectors = (bin_bytes / track.track_type.block_size() as u64).saturating_sub(start_lba);

        Ok(Self {
            bin,
            track_type: track.track_type,
            start_lba,
            sectors,
        })
    }
}

impl SectorSource for CueSectors {
    fn read_sector(&mut self, lba: u32, buf: &mut [u8; SECTOR]) -> io::Result<()> {
        if lba as u64 >= self.sectors {
            return Err(past_end());
        }
        let block = self.track_type.block_size() as usize;
        let mut raw = vec![0u8; block];
        file_read_exact_at(
            &self.bin,
            &mut raw,
            (self.start_lba + lba as u64) * block as u64,
        )?;
        buf.copy_from_slice(
            extract_user_data(self.track_type, &raw, lba as u64).map_err(io::Error::other)?,
        );
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.sectors
    }
}

/// The ISO inside a CSO/ZSO/DAX image, decompressed a block at a time.
pub(crate) struct CsoSectors {
    handle: CsoSyncHandle,
    codec: BlockDecompressor,
    cached: Option<u64>,
    block: Vec<u8>,
}

impl CsoSectors {
    /// Opens the CSO/ZSO/DAX image at `path`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let handle = open_cso_sync(path).map_err(io::Error::other)?;
        let codec = BlockDecompressor::new(handle.format);
        Ok(Self {
            handle,
            codec,
            cached: None,
            block: Vec::new(),
        })
    }

    fn load(&mut self, block: u64) -> io::Result<()> {
        if self.cached == Some(block) {
            return Ok(());
        }
        if block >= self.handle.header.block_count() {
            return Err(past_end());
        }
        let spec = block_spec(&self.handle, block).map_err(io::Error::other)?;
        let mut stored = vec![0u8; spec.stored_len];
        file_read_exact_at(&self.handle.file, &mut stored, spec.offset)?;
        self.block = if spec.raw {
            stored
        } else {
            self.codec
                .decompress(&stored, spec.expected_len)
                .map_err(io::Error::other)?
        };
        self.cached = Some(block);
        Ok(())
    }
}

impl SectorSource for CsoSectors {
    fn read_sector(&mut self, lba: u32, buf: &mut [u8; SECTOR]) -> io::Result<()> {
        let block_size = self.handle.header.block_size as u64;
        let offset = lba as u64 * SECTOR as u64;
        self.load(offset / block_size)?;
        let within = (offset % block_size) as usize;
        buf.copy_from_slice(
            self.block
                .get(within..within + SECTOR)
                .ok_or_else(past_end)?,
        );
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.handle.header.uncompressed_size / SECTOR as u64
    }
}

/// CHD hunk layout: DVD hunks are flat 2048-byte sectors, CD hunks are
/// 2448-byte frames whose user data depends on the track's mode.
#[derive(Clone, Copy)]
enum ChdLayout {
    Dvd,
    Cd {
        track_type: TrackType,
        first_frame: u64,
    },
}

enum ChdDecoder {
    Cd(ChdExtractWorker),
    Dvd(ChdDvdExtractWorker),
}

/// The disc inside a CHD, decoded a hunk at a time. CD images expose the
/// first data track only.
pub(crate) struct ChdSectors {
    handle: SyncChdHandle,
    decoder: ChdDecoder,
    layout: ChdLayout,
    sectors: u64,
    cached: Option<u64>,
    hunk: Vec<u8>,
}

impl ChdSectors {
    /// Opens the CHD at `path`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let handle = open_chd_sync(path).map_err(io::Error::other)?;
        let hunk_bytes = handle.header.hunk_bytes as usize;
        let compressors = handle.header.compressors();
        let is_dvd = handle
            .metadata
            .iter()
            .any(|m| m.tag == CHD_METADATA_TAG_DVD);

        let (layout, sectors, decoder) = if is_dvd {
            if hunk_bytes == 0 || !hunk_bytes.is_multiple_of(SECTOR) {
                return Err(invalid("DVD CHD hunk size is not a multiple of 2048 bytes"));
            }
            let worker = make_chd_dvd_extract_workers(1, &handle.file, hunk_bytes, compressors)
                .map_err(io::Error::other)?
                .pop()
                .expect("one worker requested");
            (
                ChdLayout::Dvd,
                handle.header.logical_bytes / SECTOR as u64,
                ChdDecoder::Dvd(worker),
            )
        } else {
            if hunk_bytes == 0 || !hunk_bytes.is_multiple_of(FRAME_SIZE) {
                return Err(invalid("CD CHD hunk size is not a multiple of 2448 bytes"));
            }
            let (track_type, first_frame, frames) = first_data_track(&handle)?;
            let worker = make_chd_extract_workers(1, &handle.file, hunk_bytes, compressors)
                .map_err(io::Error::other)?
                .pop()
                .expect("one worker requested");
            (
                ChdLayout::Cd {
                    track_type,
                    first_frame,
                },
                frames,
                ChdDecoder::Cd(worker),
            )
        };

        Ok(Self {
            handle,
            decoder,
            layout,
            sectors,
            cached: None,
            hunk: Vec::new(),
        })
    }

    fn load(&mut self, hunk: u64) -> io::Result<()> {
        if self.cached == Some(hunk) {
            return Ok(());
        }
        let index = u32::try_from(hunk).map_err(|_| past_end())?;
        if index as usize >= self.handle.map.len() {
            return Err(past_end());
        }
        let entry = resolve_entry(&self.handle.map, index).map_err(io::Error::other)?;
        let out = match &mut self.decoder {
            ChdDecoder::Cd(worker) => worker.process(ChdExtractWork { entry }),
            ChdDecoder::Dvd(worker) => worker.process(ChdExtractWork { entry }),
        }
        .map_err(io::Error::other)?;
        self.hunk = out.hunk;
        self.cached = Some(hunk);
        Ok(())
    }
}

impl SectorSource for ChdSectors {
    fn read_sector(&mut self, lba: u32, buf: &mut [u8; SECTOR]) -> io::Result<()> {
        if lba as u64 >= self.sectors {
            return Err(past_end());
        }
        let hunk_bytes = self.handle.header.hunk_bytes as usize;
        // `open` rejects a hunk size that is not a whole number of units,
        // so no sector or frame straddles a hunk boundary.
        match self.layout {
            ChdLayout::Dvd => {
                let offset = lba as u64 * SECTOR as u64;
                self.load(offset / hunk_bytes as u64)?;
                let within = (offset % hunk_bytes as u64) as usize;
                buf.copy_from_slice(
                    self.hunk
                        .get(within..within + SECTOR)
                        .ok_or_else(past_end)?,
                );
            }
            ChdLayout::Cd {
                track_type,
                first_frame,
            } => {
                let frames_per_hunk = (hunk_bytes / FRAME_SIZE) as u64;
                let frame = first_frame + lba as u64;
                self.load(frame / frames_per_hunk)?;
                let within = (frame % frames_per_hunk) as usize * FRAME_SIZE;
                let raw = self
                    .hunk
                    .get(within..within + FRAME_SIZE)
                    .ok_or_else(past_end)?;
                buf.copy_from_slice(
                    extract_user_data(track_type, raw, lba as u64).map_err(io::Error::other)?,
                );
            }
        }
        Ok(())
    }

    fn total_sectors(&self) -> u64 {
        self.sectors
    }
}

/// First MODE1/MODE2 track of a CD CHD: its cue-side mode, its start
/// frame in the hunk stream, and its frame count.
fn first_data_track(handle: &SyncChdHandle) -> io::Result<(TrackType, u64, u64)> {
    let meta = handle
        .metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_CD)
        .ok_or_else(|| invalid("CHD carries neither DVD nor CHT2 metadata"))?;
    let text = String::from_utf8_lossy(&meta.data);
    let tracks = parse_chd_track_metadata(text.trim_end_matches('\0')).map_err(io::Error::other)?;
    pick_data_track(&tracks)
}

/// [`first_data_track`] over already-parsed CHT2 tracks.
///
/// Two chdman layout rules drive the arithmetic: every track is padded to
/// a 4-frame boundary in the hunk stream, and a cue `INDEX 00` pregap is
/// stored inside the track's own `FRAMES:`, flagged by a `V` prefix on
/// `PGTYPE:`. A pregap that is not stored carries no frames to skip.
fn pick_data_track(tracks: &[ChdTrackInfo]) -> io::Result<(TrackType, u64, u64)> {
    let mut frame = 0u64;
    for track in tracks {
        if let Some(track_type) = cue_track_type(&track.track_type) {
            let stored_pregap = match track.pgtype.as_deref() {
                Some(t) if t.starts_with('V') => track.pregap,
                _ => 0,
            };
            return Ok((
                track_type,
                frame + stored_pregap as u64,
                track.frames.saturating_sub(stored_pregap) as u64,
            ));
        }
        frame += padded_track_frames(track.frames) as u64;
    }
    Err(invalid("CHD has no MODE1/MODE2 data track"))
}

fn cue_track_type(chd_type: &str) -> Option<TrackType> {
    match chd_type {
        "MODE1" => Some(TrackType::Mode1_2048),
        "MODE1_RAW" => Some(TrackType::Mode1_2352),
        "MODE2_RAW" => Some(TrackType::Mode2_2352),
        // chdman's MODE2_FORM1 datasize is 2048 with the user data at
        // frame offset 0, which is the MODE1/2048 payload shape.
        "MODE2_FORM1" => Some(TrackType::Mode1_2048),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(track_type: &str, frames: u32, pregap: u32, pgtype: Option<&str>) -> ChdTrackInfo {
        ChdTrackInfo {
            track_number: 1,
            track_type: track_type.to_string(),
            frames,
            pregap,
            pgtype: pgtype.map(str::to_string),
            ..ChdTrackInfo::default()
        }
    }

    #[test]
    fn data_track_start_skips_padded_audio_tracks() {
        let tracks = [
            track("AUDIO", 10, 0, None),
            track("AUDIO", 4, 0, None),
            track("MODE1_RAW", 300, 0, Some("MODE1")),
        ];
        let (track_type, first_frame, frames) =
            pick_data_track(&tracks).expect("data track present");
        assert_eq!(track_type.cue_string(), "MODE1/2352");
        assert_eq!(first_frame, 16);
        assert_eq!(frames, 300);
    }

    #[test]
    fn stored_pregap_frames_are_skipped() {
        let tracks = [track("MODE2_RAW", 450, 150, Some("VMODE2"))];
        let (_, first_frame, frames) = pick_data_track(&tracks).expect("data track present");
        assert_eq!((first_frame, frames), (150, 300));
    }

    #[test]
    fn synthesized_pregap_frames_are_kept() {
        let tracks = [track("MODE2_RAW", 300, 150, Some("MODE2"))];
        let (_, first_frame, frames) = pick_data_track(&tracks).expect("data track present");
        assert_eq!((first_frame, frames), (0, 300));
    }

    #[test]
    fn mode2_form1_reads_user_data_at_frame_offset_zero() {
        assert_eq!(
            cue_track_type("MODE2_FORM1").map(TrackType::cue_string),
            Some("MODE1/2048")
        );
    }
}
