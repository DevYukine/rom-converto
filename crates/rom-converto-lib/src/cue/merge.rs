use crate::cd::IO_BUFFER_SIZE;
use crate::cue::CueParser;
use crate::cue::error::CueError;
use crate::cue::models::{CueSheet, FileType, Msf};
use crate::util::{BYTES_PER_MB, ProgressReporter};
use log::{debug, info};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tokio::fs;

#[derive(Debug, Error)]
pub enum MergeError {
    /// Wraps an underlying I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Wraps a failed worker task join.
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    /// Wraps a CUE sheet parsing or validation failure.
    #[error(transparent)]
    Cue(#[from] CueError),

    /// The output `.cue` or `.bin` file already exists and no overwrite was requested.
    #[error("output already exists: {0}; pass --on-conflict overwrite to replace it")]
    OutputExists(String),

    /// The computed output path is the same as one of the input files.
    #[error("output path collides with an input file: {0}")]
    OutputCollidesWithInput(String),

    /// The CUE sheet does not reference any files.
    #[error("CUE sheet references no files")]
    NoFiles,

    /// The CUE sheet does not define any tracks.
    #[error("CUE sheet contains no tracks")]
    NoTracks,

    /// The CUE sheet already references a single bin file, so there is nothing to merge.
    #[error("CUE sheet already references a single bin file, nothing to merge")]
    AlreadySingleFile,

    /// The tracks in the CUE sheet do not share a single block size.
    #[error("CUE sheet mixes track types with different block sizes")]
    MixedBlockSizes,

    /// A `FILE` entry names a type other than `BINARY`, which cannot be concatenated as raw bytes.
    #[error("CUE sheet references a non-BINARY file, only raw .bin tracks can be merged: {0}")]
    NonBinaryFile(String),

    /// A bin file the CUE sheet references does not exist next to the `.cue`.
    #[error("referenced bin file not found: {0}")]
    BinNotFound(String),

    /// A bin file's size is not a whole multiple of its track's block size.
    #[error("size of {path} ({size} bytes) is not a multiple of the block size {block_size}")]
    SizeNotMultipleOfBlock {
        path: String,
        size: u64,
        block_size: u32,
    },
}

pub type MergeResult<T> = Result<T, MergeError>;

pub(crate) struct MergeFilePlan {
    pub byte_size: u64,
    pub sectors: u32,
}

/// Builds the single-file cue sheet for the merged bin. Index positions in a
/// multi-file cue are relative to their own file, so each one is rebased by
/// the sector count of all preceding files. Unlike binmerge, PREGAP and
/// POSTGAP lines are preserved.
pub(crate) fn build_merged_cue(
    out_bin_filename: &str,
    sheet: &CueSheet,
    plans: &[MergeFilePlan],
) -> String {
    let mut offsets = Vec::with_capacity(plans.len());
    let mut total_sectors = 0u32;
    for plan in plans {
        offsets.push(total_sectors);
        total_sectors += plan.sectors;
    }

    let mut cue = format!("FILE \"{out_bin_filename}\" BINARY\r\n");
    for track in &sheet.tracks {
        cue.push_str(&format!(
            "  TRACK {:02} {}\r\n",
            track.number,
            track.track_type.cue_string()
        ));
        if let Some(pregap) = track.pregap {
            cue.push_str(&format!("    PREGAP {pregap}\r\n"));
        }
        for index in &track.indices {
            let position = Msf::from_lba(offsets[track.file_index] + index.position.to_lba());
            cue.push_str(&format!("    INDEX {:02} {}\r\n", index.number, position));
        }
        if let Some(postgap) = track.postgap {
            cue.push_str(&format!("    POSTGAP {postgap}\r\n"));
        }
    }
    cue
}

fn normalize_for_compare(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    match parent.canonicalize() {
        Ok(parent) => parent.join(path.file_name().unwrap_or_default()),
        Err(_) => path.to_path_buf(),
    }
}

pub async fn merge_bin(
    progress: &dyn ProgressReporter,
    cue_path: PathBuf,
    output_cue_path: PathBuf,
    force: bool,
) -> MergeResult<()> {
    let output_bin_path = output_cue_path.with_extension("bin");

    if !force {
        for path in [&output_cue_path, &output_bin_path] {
            if fs::metadata(path).await.is_ok() {
                return Err(MergeError::OutputExists(path.display().to_string()));
            }
        }
    }

    debug!("Parsing CUE file: {:?}", cue_path);
    let cue_sheet = CueParser::new(&cue_path).parse().await?;

    if cue_sheet.files.is_empty() {
        return Err(MergeError::NoFiles);
    }
    if cue_sheet.tracks.is_empty() {
        return Err(MergeError::NoTracks);
    }
    if cue_sheet.files.len() == 1 {
        return Err(MergeError::AlreadySingleFile);
    }

    // Concatenating raw sectors only makes sense for BINARY tracks. WAVE/MP3/AIFF
    // carry their own container headers, so merging them as bytes would corrupt the
    // image while mislabeling the output FILE as BINARY.
    if let Some(file) = cue_sheet
        .files
        .iter()
        .find(|file| !matches!(file.file_type, FileType::Binary))
    {
        return Err(MergeError::NonBinaryFile(file.filename.clone()));
    }

    let block_size = cue_sheet.tracks[0].track_type.block_size();
    if cue_sheet
        .tracks
        .iter()
        .any(|track| track.track_type.block_size() != block_size)
    {
        return Err(MergeError::MixedBlockSizes);
    }

    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));
    let mut bin_paths = Vec::with_capacity(cue_sheet.files.len());
    let mut plans = Vec::with_capacity(cue_sheet.files.len());
    for file in &cue_sheet.files {
        let bin_path = cue_dir.join(&file.filename);
        let Ok(metadata) = fs::metadata(&bin_path).await else {
            return Err(MergeError::BinNotFound(bin_path.display().to_string()));
        };
        let size = metadata.len();
        if size % block_size as u64 != 0 {
            return Err(MergeError::SizeNotMultipleOfBlock {
                path: bin_path.display().to_string(),
                size,
                block_size,
            });
        }
        plans.push(MergeFilePlan {
            byte_size: size,
            sectors: (size / block_size as u64) as u32,
        });
        bin_paths.push(bin_path);
    }

    // Overwriting an input mid-stream would corrupt the source, so this is
    // refused even with force.
    for output in [&output_cue_path, &output_bin_path] {
        let normalized_output = normalize_for_compare(output);
        if bin_paths
            .iter()
            .chain(std::iter::once(&cue_path))
            .any(|input| normalize_for_compare(input) == normalized_output)
        {
            return Err(MergeError::OutputCollidesWithInput(
                output.display().to_string(),
            ));
        }
    }

    let total_bytes: u64 = plans.iter().map(|plan| plan.byte_size).sum();
    let total_mb = total_bytes as f64 / BYTES_PER_MB;
    progress.start(
        total_bytes,
        &format!(
            "Merging {} bin files (~{:.2} MB)",
            bin_paths.len(),
            total_mb
        ),
    );

    let out_bin_filename = output_bin_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "merged.bin".to_string());
    let cue_text = build_merged_cue(&out_bin_filename, &cue_sheet, &plans);

    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let output_bin_owned = output_bin_path.clone();
    let output_cue_owned = output_cue_path.clone();

    let mut handle = tokio::task::spawn_blocking(move || -> MergeResult<()> {
        let out_file = std::fs::File::create(&output_bin_owned)?;
        let mut writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);
        let mut buffer = vec![0u8; IO_BUFFER_SIZE];
        for bin_path in &bin_paths {
            let mut reader = std::fs::File::open(bin_path)?;
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                writer.write_all(&buffer[..read])?;
                bytes_done_bg.fetch_add(read as u64, Ordering::Relaxed);
            }
        }
        writer.flush()?;
        std::fs::write(&output_cue_owned, cue_text.as_bytes())?;
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

    info!(
        "Merged {} bin files ({:.2} MB) into {:?}",
        cue_sheet.files.len(),
        total_mb,
        output_bin_path
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::models::{CueFile, FileType, Index, Track, TrackType};
    use crate::util::NoProgress;

    fn track(number: u8, track_type: TrackType, file_index: usize, indices: &[(u8, u32)]) -> Track {
        Track {
            number,
            track_type,
            indices: indices
                .iter()
                .map(|&(index_number, lba)| Index {
                    number: index_number,
                    position: Msf::from_lba(lba),
                })
                .collect(),
            pregap: None,
            postgap: None,
            file_index,
        }
    }

    fn sheet(file_count: usize, tracks: Vec<Track>) -> CueSheet {
        CueSheet {
            files: (0..file_count)
                .map(|i| CueFile {
                    filename: format!("Track {}.bin", i + 1),
                    file_type: FileType::Binary,
                })
                .collect(),
            tracks,
        }
    }

    fn plan(sectors: u32, block_size: u32) -> MergeFilePlan {
        MergeFilePlan {
            byte_size: sectors as u64 * block_size as u64,
            sectors,
        }
    }

    #[test]
    fn offsets_rebased_by_preceding_file_sectors() {
        let sheet = sheet(
            2,
            vec![
                track(1, TrackType::Mode1_2352, 0, &[(1, 0)]),
                track(2, TrackType::Audio, 1, &[(1, 0)]),
            ],
        );
        let plans = [plan(4500, 2352), plan(1000, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        assert!(cue.contains("  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n"));
        assert!(cue.contains("  TRACK 02 AUDIO\r\n    INDEX 01 01:00:00\r\n"));
    }

    #[test]
    fn all_indices_of_a_track_shift_by_the_same_offset() {
        let sheet = sheet(
            2,
            vec![
                track(1, TrackType::Mode1_2352, 0, &[(1, 0)]),
                track(2, TrackType::Audio, 1, &[(0, 0), (1, 150)]),
            ],
        );
        let plans = [plan(4500, 2352), plan(1000, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        assert!(cue.contains("    INDEX 00 01:00:00\r\n"));
        assert!(cue.contains("    INDEX 01 01:02:00\r\n"));
    }

    #[test]
    fn pregap_and_postgap_preserved() {
        let mut audio_track = track(2, TrackType::Audio, 1, &[(1, 0)]);
        audio_track.pregap = Some(Msf::from_lba(150));
        audio_track.postgap = Some(Msf::from_lba(75));
        let sheet = sheet(
            2,
            vec![track(1, TrackType::Mode1_2352, 0, &[(1, 0)]), audio_track],
        );
        let plans = [plan(4500, 2352), plan(1000, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        assert!(cue.contains("    PREGAP 00:02:00\r\n"));
        assert!(cue.contains("    POSTGAP 00:01:00\r\n"));
    }

    #[test]
    fn pregap_emitted_before_indices_and_postgap_after() {
        let mut audio_track = track(1, TrackType::Audio, 0, &[(1, 0)]);
        audio_track.pregap = Some(Msf::from_lba(150));
        audio_track.postgap = Some(Msf::from_lba(75));
        let sheet = sheet(
            2,
            vec![audio_track, track(2, TrackType::Audio, 1, &[(1, 0)])],
        );
        let plans = [plan(100, 2352), plan(100, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        let pregap_pos = cue.find("PREGAP").unwrap();
        let index_pos = cue.find("INDEX").unwrap();
        let postgap_pos = cue.find("POSTGAP").unwrap();
        assert!(pregap_pos < index_pos);
        assert!(index_pos < postgap_pos);
    }

    #[test]
    fn header_and_line_endings() {
        let sheet = sheet(
            2,
            vec![
                track(1, TrackType::Mode1_2352, 0, &[(1, 0)]),
                track(2, TrackType::Audio, 1, &[(1, 0)]),
            ],
        );
        let plans = [plan(100, 2352), plan(100, 2352)];

        let cue = build_merged_cue("Game (merged).bin", &sheet, &plans);

        assert!(cue.starts_with("FILE \"Game (merged).bin\" BINARY\r\n"));
        assert!(cue.lines().count() > 1);
        assert!(!cue.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn track_and_index_numbers_zero_padded() {
        let sheet = sheet(
            2,
            vec![
                track(9, TrackType::Audio, 0, &[(1, 0)]),
                track(10, TrackType::Audio, 1, &[(1, 0)]),
            ],
        );
        let plans = [plan(100, 2352), plan(100, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        assert!(cue.contains("  TRACK 09 AUDIO\r\n"));
        assert!(cue.contains("  TRACK 10 AUDIO\r\n"));
        assert!(cue.contains("    INDEX 01 "));
    }

    #[test]
    fn single_file_offsets_unchanged() {
        let sheet = sheet(
            1,
            vec![track(1, TrackType::Mode1_2352, 0, &[(0, 0), (1, 150)])],
        );
        let plans = [plan(4500, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        assert!(cue.contains("    INDEX 00 00:00:00\r\n"));
        assert!(cue.contains("    INDEX 01 00:02:00\r\n"));
    }

    #[test]
    fn track_with_no_indices_does_not_panic() {
        let mut bare = track(2, TrackType::Audio, 1, &[]);
        bare.indices.clear();
        let sheet = sheet(2, vec![track(1, TrackType::Mode1_2352, 0, &[(1, 0)]), bare]);
        let plans = [plan(100, 2352), plan(100, 2352)];

        let cue = build_merged_cue("merged.bin", &sheet, &plans);

        assert!(cue.contains("  TRACK 02 AUDIO\r\n"));
    }

    async fn write_bin(path: &Path, sectors: u32, fill: u8) {
        let bytes = vec![fill; sectors as usize * 2352];
        tokio::fs::write(path, &bytes).await.unwrap();
    }

    #[tokio::test]
    async fn merge_concatenates_bins_and_writes_single_file_cue() {
        let dir = tempfile::tempdir().unwrap();
        let bin1 = dir.path().join("game (Track 1).bin");
        let bin2 = dir.path().join("game (Track 2).bin");
        write_bin(&bin1, 10, 0xAA).await;
        write_bin(&bin2, 5, 0xBB).await;

        let cue_path = dir.path().join("game.cue");
        tokio::fs::write(
            &cue_path,
            "FILE \"game (Track 1).bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n\
             FILE \"game (Track 2).bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    INDEX 00 00:00:00\r\n    INDEX 01 00:02:00\r\n",
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("game (merged).cue");
        let out_bin = dir.path().join("game (merged).bin");
        merge_bin(&NoProgress, cue_path, out_cue.clone(), false)
            .await
            .unwrap();

        let merged = tokio::fs::read(&out_bin).await.unwrap();
        assert_eq!(merged.len(), 15 * 2352);
        assert!(merged[..10 * 2352].iter().all(|&b| b == 0xAA));
        assert!(merged[10 * 2352..].iter().all(|&b| b == 0xBB));

        let cue_text = tokio::fs::read_to_string(&out_cue).await.unwrap();
        assert_eq!(cue_text.matches("FILE ").count(), 1);
        assert!(cue_text.starts_with("FILE \"game (merged).bin\" BINARY\r\n"));
        // Track 2 starts after 10 sectors of track 1, so INDEX 00 rebases to 10 sectors.
        assert!(cue_text.contains("  TRACK 02 AUDIO\r\n    INDEX 00 00:00:10\r\n"));
        assert!(cue_text.contains("    INDEX 01 00:02:10\r\n"));
    }

    #[tokio::test]
    async fn non_binary_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cue_path = dir.path().join("game.cue");
        tokio::fs::write(
            &cue_path,
            "FILE \"track1.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n\
             FILE \"track2.wav\" WAVE\r\n  TRACK 02 AUDIO\r\n    INDEX 01 00:00:00\r\n",
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("merged.cue");
        let err = merge_bin(&NoProgress, cue_path, out_cue, false)
            .await
            .unwrap_err();
        assert!(matches!(err, MergeError::NonBinaryFile(name) if name == "track2.wav"));
    }

    #[tokio::test]
    async fn output_colliding_with_input_is_refused_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let bin1 = dir.path().join("track1.bin");
        let bin2 = dir.path().join("track2.bin");
        write_bin(&bin1, 4, 0x01).await;
        write_bin(&bin2, 4, 0x02).await;

        let cue_path = dir.path().join("game.cue");
        tokio::fs::write(
            &cue_path,
            "FILE \"track1.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n\
             FILE \"track2.bin\" BINARY\r\n  TRACK 02 AUDIO\r\n    INDEX 01 00:00:00\r\n",
        )
        .await
        .unwrap();

        // Output bin would be track1.bin, overwriting an input. Refused regardless of force.
        let out_cue = dir.path().join("track1.cue");
        let err = merge_bin(&NoProgress, cue_path, out_cue, true)
            .await
            .unwrap_err();
        assert!(matches!(err, MergeError::OutputCollidesWithInput(_)));
    }
}
