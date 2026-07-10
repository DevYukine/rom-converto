//! Extract the data track of a CUE/BIN disc image into a plain ISO of
//! 2048-byte user sectors. Only the first track is converted; it must be a
//! MODE1 or MODE2 data track. Audio and other tracks are skipped.

use crate::cd::IO_BUFFER_SIZE;
use crate::cue::CueParser;
use crate::cue::error::CueError;
use crate::cue::models::{FileType, TrackType};
use crate::util::{BYTES_PER_MB, ProgressReporter};
use log::info;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tokio::fs;

/// User-data payload size of an ISO sector.
const USER_DATA_SIZE: usize = 2048;
/// Submode bit that marks a MODE2 XA sector as Form2 (2324-byte payload).
const FORM2_SUBMODE_BIT: u8 = 0x20;

#[derive(Debug, Error)]
pub enum ToIsoError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// Wraps a CUE sheet parsing or validation failure.
    #[error(transparent)]
    Cue(#[from] CueError),

    /// The output ISO already exists and no overwrite was requested.
    #[error("output already exists: {0}")]
    OutputExists(String),

    /// The CUE sheet does not define any tracks.
    #[error("CUE sheet contains no tracks")]
    NoTracks,

    /// The CUE sheet does not reference any files.
    #[error("CUE sheet references no files")]
    NoFiles,

    /// Track 1 is not a MODE1/MODE2 data track, so there is no ISO to extract.
    #[error("track 1 is {0}, not a MODE1/MODE2 data track; nothing to extract to ISO")]
    UnsupportedDataTrack(String),

    /// Track 1 has no INDEX 01 marking where its data begins.
    #[error("track 1 has no INDEX 01")]
    NoIndex01,

    /// A `FILE` entry names a type other than `BINARY`.
    #[error("CUE sheet references a non-BINARY file: {0}")]
    NonBinaryFile(String),

    /// The bin file the CUE sheet references does not exist next to the `.cue`.
    #[error("referenced bin file not found: {0}")]
    BinNotFound(String),

    /// The bin file's size is not a whole multiple of the track's block size.
    #[error("size of {path} ({size} bytes) is not a multiple of the block size {block_size}")]
    SizeNotMultipleOfBlock {
        path: String,
        size: u64,
        block_size: u32,
    },

    /// The data track's sector range runs past the end of the bin file.
    #[error("track 1 range [{start}, {end}) exceeds the {total}-sector source file")]
    TrackRangeOutOfBounds { start: u64, end: u64, total: u64 },

    /// A MODE2 sector is Form2, which carries a 2324-byte payload with no
    /// plain 2048-byte ISO representation.
    #[error("sector {0} is MODE2 Form2 (2324-byte payload); track has no plain ISO representation")]
    Form2Sector(u64),
}

pub type ToIsoResult<T> = Result<T, ToIsoError>;

/// True for the data-track modes this extractor understands.
fn is_supported_data_track(track_type: TrackType) -> bool {
    matches!(
        track_type,
        TrackType::Mode1_2048
            | TrackType::Mode1_2352
            | TrackType::Mode2_2352
            | TrackType::Mode2_2336
    )
}

/// Slice the 2048-byte user data out of one raw sector. `sector_index` is the
/// absolute sector position, used only for a Form2 error message.
fn extract_user_data(
    track_type: TrackType,
    sector: &[u8],
    sector_index: u64,
) -> ToIsoResult<&[u8]> {
    match track_type {
        // Already bare user data.
        TrackType::Mode1_2048 => Ok(&sector[..USER_DATA_SIZE]),
        // 12-byte sync + 4-byte header precede the user data.
        TrackType::Mode1_2352 => Ok(&sector[16..16 + USER_DATA_SIZE]),
        // 16-byte header + 8-byte XA subheader; submode byte at offset 18.
        TrackType::Mode2_2352 => {
            if sector[18] & FORM2_SUBMODE_BIT != 0 {
                return Err(ToIsoError::Form2Sector(sector_index));
            }
            Ok(&sector[24..24 + USER_DATA_SIZE])
        }
        // 8-byte XA subheader only; submode byte at offset 2.
        TrackType::Mode2_2336 => {
            if sector[2] & FORM2_SUBMODE_BIT != 0 {
                return Err(ToIsoError::Form2Sector(sector_index));
            }
            Ok(&sector[8..8 + USER_DATA_SIZE])
        }
        other => Err(ToIsoError::UnsupportedDataTrack(
            other.cue_string().to_string(),
        )),
    }
}

/// Convert the first (data) track of a CUE/BIN image into a plain ISO.
///
/// Any additional tracks (audio and so on) are reported as skipped. The data
/// track must live in the first `FILE`, be BINARY, and use a MODE1/MODE2 mode.
pub async fn cue_to_iso(
    progress: &dyn ProgressReporter,
    cue_path: PathBuf,
    output_iso_path: PathBuf,
    force: bool,
) -> ToIsoResult<()> {
    if !force && fs::metadata(&output_iso_path).await.is_ok() {
        return Err(ToIsoError::OutputExists(
            output_iso_path.display().to_string(),
        ));
    }

    let cue_sheet = CueParser::new(&cue_path).parse().await?;
    if cue_sheet.tracks.is_empty() {
        return Err(ToIsoError::NoTracks);
    }
    if cue_sheet.files.is_empty() {
        return Err(ToIsoError::NoFiles);
    }

    let data_track = &cue_sheet.tracks[0];
    let track_type = data_track.track_type;
    if !is_supported_data_track(track_type) {
        return Err(ToIsoError::UnsupportedDataTrack(
            track_type.cue_string().to_string(),
        ));
    }

    let file = &cue_sheet.files[data_track.file_index];
    if !matches!(file.file_type, FileType::Binary) {
        return Err(ToIsoError::NonBinaryFile(file.filename.clone()));
    }

    if cue_sheet.tracks.len() > 1 {
        progress.warn(&format!(
            "Converting only the data track; skipping {} additional track(s)",
            cue_sheet.tracks.len() - 1
        ));
    }

    let block_size = track_type.block_size();
    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));
    let bin_path = cue_dir.join(&file.filename);
    let Ok(metadata) = fs::metadata(&bin_path).await else {
        return Err(ToIsoError::BinNotFound(bin_path.display().to_string()));
    };
    let file_size = metadata.len();
    if file_size % block_size as u64 != 0 {
        return Err(ToIsoError::SizeNotMultipleOfBlock {
            path: bin_path.display().to_string(),
            size: file_size,
            block_size,
        });
    }
    let total_sectors = file_size / block_size as u64;

    let start_lba = data_track
        .primary_index_lba()
        .ok_or(ToIsoError::NoIndex01)? as u64;
    // The data track ends where the next track sharing its FILE begins; if no
    // later track lives in the same file, it runs to the end of the file.
    let end_lba = cue_sheet
        .tracks
        .iter()
        .skip(1)
        .find(|track| track.file_index == data_track.file_index)
        .and_then(|track| {
            track
                .indices
                .iter()
                .map(|index| index.position.to_lba())
                .min()
        })
        .map(|lba| lba as u64)
        .unwrap_or(total_sectors);

    if start_lba > end_lba || end_lba > total_sectors {
        return Err(ToIsoError::TrackRangeOutOfBounds {
            start: start_lba,
            end: end_lba,
            total: total_sectors,
        });
    }

    let consumed = (end_lba - start_lba) * block_size as u64;
    let total_mb = consumed as f64 / BYTES_PER_MB;
    progress.start(
        consumed,
        &format!("Extracting ISO from track 1 (~{total_mb:.2} MB)"),
    );

    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let output_owned = output_iso_path.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> ToIsoResult<()> {
        let in_file = std::fs::File::open(&bin_path)?;
        let mut reader = BufReader::with_capacity(IO_BUFFER_SIZE, in_file);
        reader.seek(SeekFrom::Start(start_lba * block_size as u64))?;

        let out_file = std::fs::File::create(&output_owned)?;
        let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

        let mut sector = vec![0u8; block_size as usize];
        for index in start_lba..end_lba {
            reader.read_exact(&mut sector)?;
            let payload = extract_user_data(track_type, &sector, index)?;
            writer.write_all(payload)?;
            bytes_done_bg.fetch_add(block_size as u64, Ordering::Relaxed);
        }
        writer.flush()?;
        Ok(())
    });

    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(result) => {
                result??;
                break;
            }
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    }
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();

    let out_sectors = end_lba - start_lba;
    info!(
        "Extracted {out_sectors} sectors ({total_mb:.2} MB) to {:?}",
        output_iso_path
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    /// Build one raw sector for `track_type` whose 2048-byte user payload is
    /// `fill`. `form2` sets the MODE2 submode Form2 bit.
    fn make_sector(track_type: TrackType, fill: u8, form2: bool) -> Vec<u8> {
        let block = track_type.block_size() as usize;
        let mut sector = vec![0u8; block];
        let submode = if form2 { FORM2_SUBMODE_BIT } else { 0 };
        let payload_start = match track_type {
            TrackType::Mode1_2048 => 0,
            TrackType::Mode1_2352 => 16,
            TrackType::Mode2_2352 => {
                sector[18] = submode;
                24
            }
            TrackType::Mode2_2336 => {
                sector[2] = submode;
                8
            }
            _ => unreachable!(),
        };
        for byte in &mut sector[payload_start..payload_start + USER_DATA_SIZE] {
            *byte = fill;
        }
        sector
    }

    async fn write_image(dir: &Path, track_type: TrackType, sectors: &[Vec<u8>]) -> PathBuf {
        let bin = dir.join("game.bin");
        let mut data = Vec::new();
        for sector in sectors {
            data.extend_from_slice(sector);
        }
        tokio::fs::write(&bin, &data).await.unwrap();

        let cue = dir.join("game.cue");
        tokio::fs::write(
            &cue,
            format!(
                "FILE \"game.bin\" BINARY\r\n  TRACK 01 {}\r\n    INDEX 01 00:00:00\r\n",
                track_type.cue_string()
            ),
        )
        .await
        .unwrap();
        cue
    }

    async fn assert_extracts(track_type: TrackType) {
        let dir = tempfile::tempdir().unwrap();
        let sectors = vec![
            make_sector(track_type, 0x11, false),
            make_sector(track_type, 0x22, false),
        ];
        let cue = write_image(dir.path(), track_type, &sectors).await;
        let iso = dir.path().join("game.iso");
        cue_to_iso(&NoProgress, cue, iso.clone(), false)
            .await
            .unwrap();

        let out = tokio::fs::read(&iso).await.unwrap();
        assert_eq!(out.len(), 2 * USER_DATA_SIZE);
        assert!(out[..USER_DATA_SIZE].iter().all(|&b| b == 0x11));
        assert!(out[USER_DATA_SIZE..].iter().all(|&b| b == 0x22));
    }

    #[tokio::test]
    async fn extracts_mode1_2048() {
        assert_extracts(TrackType::Mode1_2048).await;
    }

    #[tokio::test]
    async fn extracts_mode1_2352() {
        assert_extracts(TrackType::Mode1_2352).await;
    }

    #[tokio::test]
    async fn extracts_mode2_2352_form1() {
        assert_extracts(TrackType::Mode2_2352).await;
    }

    #[tokio::test]
    async fn extracts_mode2_2336_form1() {
        assert_extracts(TrackType::Mode2_2336).await;
    }

    #[tokio::test]
    async fn form2_sector_errors_with_index() {
        let dir = tempfile::tempdir().unwrap();
        let sectors = vec![
            make_sector(TrackType::Mode2_2352, 0x11, false),
            make_sector(TrackType::Mode2_2352, 0x22, true),
        ];
        let cue = write_image(dir.path(), TrackType::Mode2_2352, &sectors).await;
        let iso = dir.path().join("game.iso");
        let err = cue_to_iso(&NoProgress, cue, iso, false).await.unwrap_err();
        assert!(matches!(err, ToIsoError::Form2Sector(1)));
    }

    #[tokio::test]
    async fn audio_track_still_converts_data_track_one() {
        let dir = tempfile::tempdir().unwrap();
        let data_sectors = vec![
            make_sector(TrackType::Mode1_2352, 0xAA, false),
            make_sector(TrackType::Mode1_2352, 0xBB, false),
        ];
        // Two MODE1 data sectors followed by one 2352-byte audio sector, all in
        // one bin. Track 2 (audio) starts at sector 2 and must be excluded.
        let mut data = Vec::new();
        for sector in &data_sectors {
            data.extend_from_slice(sector);
        }
        data.extend_from_slice(&vec![0xCC; 2352]);
        let bin = dir.path().join("game.bin");
        tokio::fs::write(&bin, &data).await.unwrap();

        let cue = dir.path().join("game.cue");
        tokio::fs::write(
            &cue,
            "FILE \"game.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n\
               TRACK 02 AUDIO\r\n    INDEX 01 00:00:02\r\n",
        )
        .await
        .unwrap();

        let iso = dir.path().join("game.iso");
        cue_to_iso(&NoProgress, cue, iso.clone(), false)
            .await
            .unwrap();

        let out = tokio::fs::read(&iso).await.unwrap();
        assert_eq!(out.len(), 2 * USER_DATA_SIZE);
        assert!(out[..USER_DATA_SIZE].iter().all(|&b| b == 0xAA));
        assert!(out[USER_DATA_SIZE..].iter().all(|&b| b == 0xBB));
    }

    #[tokio::test]
    async fn audio_track_one_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("game.bin");
        tokio::fs::write(&bin, vec![0u8; 2352]).await.unwrap();
        let cue = dir.path().join("game.cue");
        tokio::fs::write(
            &cue,
            "FILE \"game.bin\" BINARY\r\n  TRACK 01 AUDIO\r\n    INDEX 01 00:00:00\r\n",
        )
        .await
        .unwrap();
        let iso = dir.path().join("game.iso");
        let err = cue_to_iso(&NoProgress, cue, iso, false).await.unwrap_err();
        assert!(matches!(err, ToIsoError::UnsupportedDataTrack(_)));
    }

    #[tokio::test]
    async fn size_not_multiple_of_block_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("game.bin");
        tokio::fs::write(&bin, vec![0u8; 2352 + 10]).await.unwrap();
        let cue = dir.path().join("game.cue");
        tokio::fs::write(
            &cue,
            "FILE \"game.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n",
        )
        .await
        .unwrap();
        let iso = dir.path().join("game.iso");
        let err = cue_to_iso(&NoProgress, cue, iso, false).await.unwrap_err();
        assert!(matches!(err, ToIsoError::SizeNotMultipleOfBlock { .. }));
    }
}
