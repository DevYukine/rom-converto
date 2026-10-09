//! CHD to disc image extraction: CD-mode CHDs restore a `.cue` and one
//! `.bin` per track, DVD-mode CHDs a flat `.iso`.

use crate::disc::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::reader::cue_generator::{
    ChdTrackInfo, MAX_CD_TRACKS, generate_cue_sheet, parse_chd_track_metadata,
};
use crate::disc::chd::reader::{ChdFlavor, SyncChdHandle};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, run_scratch_write};
use log::{debug, info};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::*;

/// What the peek pass hands to the extract body: the open handle, the
/// parsed track metadata with its chosen tag family for CD images, and
/// the resolved output path.
struct ChdExtractPeek {
    handle: SyncChdHandle,
    tracks: Option<(Vec<ChdTrackInfo>, [u8; 4])>,
    output_path: PathBuf,
}

/// Extract the CHD at `input_path` back to its disc image at `output_path`.
/// CD accepts `.cue` or no extension (adds `.cue`) and writes one `.bin` per
/// track; DVD accepts anything but `.cue` or `.chd`, with no extension adding
/// `.iso`. A failed or cancelled run leaves existing files untouched.
pub async fn extract_from_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    parent_path: Option<PathBuf>,
    cancel: CancelToken,
) -> ChdResult<()> {
    if parent_path.is_some() {
        return Err(ChdError::ParentChdNotSupported);
    }

    debug!("Opening CHD file: {:?}", input_path);

    // Open once up front so the output type (DVD iso vs CD bin/cue) is
    // known and the progress bar can size itself; the handle and the
    // parsed tracks then carry into the extract below instead of being
    // re-read. `total_bin_bytes` comes from the CHT2 track metadata,
    // not from `header.logical_bytes`: logical_bytes counts the padded
    // physical frames, which the extracted bin drops.
    let input_for_peek = input_path.clone();
    let peek = tokio::task::spawn_blocking(move || -> ChdResult<ChdExtractPeek> {
        let handle = crate::disc::chd::reader::open_chd_sync(&input_for_peek)?;
        let output_path = extract_target(handle.flavor(), output_path)?;
        match handle.flavor() {
            ChdFlavor::Cd => {
                let (track_tag, meta_str) =
                    cd_track_metadata(&handle.metadata).ok_or_else(|| {
                        ChdError::InvalidTrackMetadata("no CD track metadata found".to_string())
                    })?;
                let tracks = parse_chd_track_metadata(&meta_str)?;
                chd_validate_cd_hunks(handle.header.hunk_bytes)?;
                // Reject hostile frame totals before any scratch
                // output exists or progress starts.
                let physical_frames = handle.header.logical_bytes / FRAME_SIZE as u64;
                let padded = chd_layout_is_padded(&tracks, physical_frames);
                chd_validate_frame_total(&tracks, padded, physical_frames)?;
                Ok(ChdExtractPeek {
                    handle,
                    tracks: Some((tracks, track_tag)),
                    output_path,
                })
            }
            _ => Ok(ChdExtractPeek {
                handle,
                tracks: None,
                output_path,
            }),
        }
    })
    .await??;
    let ChdExtractPeek {
        handle,
        tracks,
        output_path,
    } = peek;

    let Some((tracks, track_tag)) = tracks else {
        return extract_dvd_iso(progress, input_path, output_path, handle, cancel).await;
    };

    let total_bin_bytes: u64 = tracks
        .iter()
        .map(crate::disc::chd::layout::chd_track_decoded_size)
        .sum();

    let cue_path = output_path;
    let bin_paths = track_bin_paths(&cue_path, tracks.len());
    let bin_names: Vec<String> = bin_paths
        .iter()
        .map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let cue_content = generate_cue_sheet(&bin_names, &tracks);
    let mut bin_files = Vec::with_capacity(bin_paths.len());
    let mut bin_temps = Vec::with_capacity(bin_paths.len());
    for path in &bin_paths {
        let (file, temp) = crate::util::scratch_output_file(path)?.into_parts();
        bin_files.push(file);
        bin_temps.push(temp);
    }
    let (mut cue_file, cue_temp) = crate::util::scratch_output_file(&cue_path)?.into_parts();

    let total_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    progress.start(
        total_bin_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    let bytes_done = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let handle = tokio::task::spawn_blocking({
        let bytes_done = bytes_done.clone();
        let cancel = cancel.clone();
        move || -> ChdResult<Vec<tempfile::TempPath>> {
            use crate::disc::chd::reader::worker::{
                ChdExtractWork, ChdExtractedOut, HunkExtractArgs, chd_read_admission,
                extract_hunks, make_chd_extract_workers,
            };
            use crate::util::worker_pool::{Pool, parallelism};

            let hunk_bytes = handle.header.hunk_bytes as usize;
            let physical_frames = handle.header.logical_bytes / FRAME_SIZE as u64;
            // The span table comes from the track metadata `FRAMES:`
            // counts; a GD-ROM `PAD:` gap frame and the per-track
            // boundary padding frames carry width 0 so they drop out of
            // the bin. Legacy rom-converto CHDs stored the stream
            // unpadded.
            let padded = chd_layout_is_padded(&tracks, physical_frames);
            let spans = chd_track_spans(&tracks, padded, track_tag, physical_frames)?;

            let mut bin_writer = TrackBinWriter::new(
                bin_files
                    .into_iter()
                    .zip(&tracks)
                    .map(|(file, track)| {
                        (
                            file,
                            crate::disc::chd::layout::chd_track_decoded_size(track),
                        )
                    })
                    .collect(),
            );

            let n_threads = parallelism();
            let admission =
                chd_read_admission(hunk_bytes, n_threads, handle.map.len() as u64, true);
            let workers = make_chd_extract_workers(
                admission.workers,
                &handle.file,
                hunk_bytes,
                handle.header.compressors(),
            )?;
            let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(workers);

            let extract_result = extract_hunks(
                &pool,
                &mut bin_writer,
                HunkExtractArgs {
                    map: &handle.map,
                    hunk_bytes,
                    spans: &spans,
                    bytes_done: &bytes_done,
                    cancel: &cancel,
                    admission,
                },
            );
            pool.shutdown();
            extract_result?;

            bin_writer.finish()?;
            Ok(bin_temps)
        }
    });
    let bin_temps =
        crate::util::await_with_progress_cancel(progress, &bytes_done, handle, &cancel).await?;
    cue_file.write_all(cue_content.as_bytes())?;
    drop(cue_file);
    if cancel.is_cancelled() {
        return Err(crate::util::Cancelled.into());
    }
    let mut members: Vec<(tempfile::TempPath, &Path)> = bin_temps
        .into_iter()
        .zip(bin_paths.iter().map(PathBuf::as_path))
        .collect();
    members.push((cue_temp, &cue_path));
    crate::util::publish_set(members, true)?;

    let bin_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    info!(
        "Extracted: {:.2} MB in {} track BINs + CUE from {:?}",
        bin_mb,
        bin_paths.len(),
        input_path
    );

    debug!("Extraction complete");
    Ok(())
}

fn track_bin_path(cue: &Path, track: Option<usize>, zero_pad: bool) -> PathBuf {
    let Some(track) = track else {
        return cue.with_extension("bin");
    };
    let mut name = cue.file_stem().unwrap_or_default().to_os_string();
    if zero_pad {
        name.push(format!(" (Track {track:02}).bin"));
    } else {
        name.push(format!(" (Track {track}).bin"));
    }
    cue.with_file_name(name)
}

pub(crate) fn track_bin_paths(cue: &Path, track_count: usize) -> Vec<PathBuf> {
    if track_count == 1 {
        vec![track_bin_path(cue, None, false)]
    } else {
        (1..=track_count)
            .map(|track| track_bin_path(cue, Some(track), track_count >= 10))
            .collect()
    }
}

pub(crate) fn possible_track_bins(cue: &Path) -> impl Iterator<Item = PathBuf> {
    std::iter::once(track_bin_path(cue, None, false))
        .chain((1..=MAX_CD_TRACKS).map(move |track| track_bin_path(cue, Some(track), false)))
        .chain((1..=9).map(move |track| track_bin_path(cue, Some(track), true)))
}

struct TrackBinWriter {
    remaining: std::vec::IntoIter<(File, u64)>,
    current: Option<BufWriter<File>>,
    bytes_left: u64,
}

impl TrackBinWriter {
    fn new(tracks: Vec<(File, u64)>) -> Self {
        Self {
            remaining: tracks.into_iter(),
            current: None,
            bytes_left: 0,
        }
    }

    fn finish(mut self) -> io::Result<()> {
        self.flush()?;
        if self.bytes_left != 0 || self.remaining.any(|(_, length)| length != 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "extract stream ends before the last track",
            ));
        }
        Ok(())
    }
}

impl Write for TrackBinWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.bytes_left == 0 {
            if let Some(mut writer) = self.current.take() {
                writer.flush()?;
            }
            for (file, length) in self.remaining.by_ref() {
                if length == 0 {
                    continue;
                }
                self.current = Some(BufWriter::with_capacity(IO_BUFFER_SIZE, file));
                self.bytes_left = length;
                break;
            }
        }
        let Some(writer) = self.current.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "extract stream runs past the last track",
            ));
        };
        let take = (bytes.len() as u64).min(self.bytes_left) as usize;
        let written = writer.write(&bytes[..take])?;
        self.bytes_left -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(writer) = self.current.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }
}

pub(crate) fn extract_ext(flavor: ChdFlavor) -> ChdResult<&'static str> {
    match flavor {
        ChdFlavor::Cd => Ok("cue"),
        ChdFlavor::Dvd => Ok("iso"),
        ChdFlavor::Ld => Err(ChdError::LdExtractionUnsupported),
    }
}

/// Settle an extraction target: CD defaults to `.cue` and rejects other
/// extensions; DVD defaults to `.iso` and rejects `.cue` or `.chd`.
/// Extension checks are ASCII case-insensitive. LaserDisc extraction is unsupported.
pub(crate) fn extract_target(flavor: ChdFlavor, output: PathBuf) -> ChdResult<PathBuf> {
    let mode_ext = extract_ext(flavor)?;
    match (flavor, output.extension()) {
        (_, None) => Ok(output.with_extension(mode_ext)),
        (ChdFlavor::Cd, Some(ext)) if ext.eq_ignore_ascii_case(mode_ext) => Ok(output),
        (ChdFlavor::Cd, Some(_)) => Err(ChdError::CdExtractNeedsCue(output)),
        (ChdFlavor::Dvd, Some(ext))
            if ext.eq_ignore_ascii_case("cue") || ext.eq_ignore_ascii_case("chd") =>
        {
            Err(ChdError::DvdExtractNeedsImage(output))
        }
        (ChdFlavor::Dvd, Some(_)) => Ok(output),
        (ChdFlavor::Ld, _) => Err(ChdError::LdExtractionUnsupported),
    }
}

/// Peek a CHD's metadata to tell DVD-mode (flat ISO) apart from
/// CD-mode (bin/cue with CHT2 track metadata) without extracting
/// anything. Used by [`crate::pipeline::chd_to_cso`] to
/// reject CD-mode CHDs up front, since CSO/ZSO have no track layout.
pub async fn is_dvd_mode_chd(path: PathBuf) -> ChdResult<bool> {
    tokio::task::spawn_blocking(move || -> ChdResult<bool> {
        match crate::disc::chd::reader::open_chd_sync(&path)?.flavor() {
            ChdFlavor::Ld => Err(ChdError::LdExtractionUnsupported),
            flavor => Ok(flavor == ChdFlavor::Dvd),
        }
    })
    .await?
}

/// DVD extract path: one flat `.iso`, no cue sheet.
async fn extract_dvd_iso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    iso_path: PathBuf,
    handle: SyncChdHandle,
    cancel: CancelToken,
) -> ChdResult<()> {
    let logical_bytes = handle.header.logical_bytes;
    let total_mb = logical_bytes as f64 / BYTES_PER_MB;
    progress.start(
        logical_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    run_scratch_write(
        &iso_path,
        true,
        progress,
        &cancel,
        move |iso_file, bytes_done, cancel| -> ChdResult<()> {
            use crate::disc::chd::reader::worker::{
                ChdExtractWork, ChdExtractedOut, chd_read_admission, extract_hunks_dvd,
                make_chd_dvd_extract_workers,
            };
            use crate::util::worker_pool::{Pool, parallelism};

            let hunk_bytes = handle.header.hunk_bytes as usize;
            let admission =
                chd_read_admission(hunk_bytes, parallelism(), handle.map.len() as u64, true);

            let mut iso_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, iso_file);

            let workers = make_chd_dvd_extract_workers(
                admission.workers,
                &handle.file,
                hunk_bytes,
                handle.header.compressors(),
            )?;
            let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(workers);

            let extract_result = extract_hunks_dvd(
                &pool,
                &handle.map,
                &mut iso_writer,
                hunk_bytes,
                logical_bytes,
                &bytes_done,
                &cancel,
                admission,
            );
            pool.shutdown();
            extract_result?;

            iso_writer.flush()?;
            Ok(())
        },
    )
    .await?;

    info!(
        "Extracted: {:.2} MB ISO from {}",
        total_mb,
        input_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_bins_use_single_and_numbered_names() {
        let cue = Path::new("output/game.cue");
        assert_eq!(
            track_bin_paths(cue, 1),
            vec![PathBuf::from("output/game.bin")]
        );
        assert_eq!(
            track_bin_paths(cue, 2),
            vec![
                PathBuf::from("output/game (Track 1).bin"),
                PathBuf::from("output/game (Track 2).bin"),
            ]
        );
        assert_eq!(
            track_bin_paths(cue, 10),
            vec![
                PathBuf::from("output/game (Track 01).bin"),
                PathBuf::from("output/game (Track 02).bin"),
                PathBuf::from("output/game (Track 03).bin"),
                PathBuf::from("output/game (Track 04).bin"),
                PathBuf::from("output/game (Track 05).bin"),
                PathBuf::from("output/game (Track 06).bin"),
                PathBuf::from("output/game (Track 07).bin"),
                PathBuf::from("output/game (Track 08).bin"),
                PathBuf::from("output/game (Track 09).bin"),
                PathBuf::from("output/game (Track 10).bin"),
            ]
        );
    }

    #[test]
    fn track_bins_preserve_dots_in_the_stem() {
        let cue = Path::new("output/Game v1.1.cue");
        assert_eq!(
            track_bin_paths(cue, 1),
            vec![PathBuf::from("output/Game v1.1.bin")]
        );
        assert_eq!(
            track_bin_paths(cue, 2),
            vec![
                PathBuf::from("output/Game v1.1 (Track 1).bin"),
                PathBuf::from("output/Game v1.1 (Track 2).bin"),
            ]
        );
    }

    #[test]
    fn possible_bins_cover_every_supported_track_count() {
        let cue = Path::new("output/Game v1.1.cue");
        let possible: std::collections::HashSet<_> = possible_track_bins(cue).collect();
        for count in 1..=MAX_CD_TRACKS {
            for path in track_bin_paths(cue, count) {
                assert!(possible.contains(&path), "missing {}", path.display());
            }
        }
    }

    #[test]
    fn track_writer_routes_bytes_past_empty_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let (files, paths): (Vec<_>, Vec<_>) = (0..5)
            .map(|track| {
                crate::util::scratch_output_file(&dir.path().join(format!("{track}.bin")))
                    .unwrap()
                    .into_parts()
            })
            .unzip();
        let mut writer = TrackBinWriter::new(files.into_iter().zip([0, 3, 0, 2, 0]).collect());
        writer.write_all(b"ab").unwrap();
        writer.write_all(b"cde").unwrap();
        writer.finish().unwrap();
        let expected: [&[u8]; 5] = [b"", b"abc", b"", b"de", b""];
        for (path, bytes) in paths.iter().zip(expected) {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn track_writer_rejects_a_short_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _first_path) = crate::util::scratch_output_file(&dir.path().join("first.bin"))
            .unwrap()
            .into_parts();
        let (second, _second_path) =
            crate::util::scratch_output_file(&dir.path().join("second.bin"))
                .unwrap()
                .into_parts();
        let mut writer = TrackBinWriter::new(vec![(first, 2), (second, 3)]);
        writer.write_all(b"abcd").unwrap();
        assert_eq!(
            writer.finish().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn track_writer_rejects_an_overlong_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _first_path) = crate::util::scratch_output_file(&dir.path().join("first.bin"))
            .unwrap()
            .into_parts();
        let (second, _second_path) =
            crate::util::scratch_output_file(&dir.path().join("second.bin"))
                .unwrap()
                .into_parts();
        let mut writer = TrackBinWriter::new(vec![(first, 2), (second, 1)]);
        assert_eq!(
            writer.write_all(b"abcd").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn track_writer_does_not_follow_replaced_scratch_path() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.bin");
        std::fs::write(&victim, b"private").unwrap();
        let (file, path) = crate::util::scratch_output_file(&dir.path().join("track.bin"))
            .unwrap()
            .into_parts();
        let mut original = file.try_clone().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let mut writer = TrackBinWriter::new(vec![(file, 3)]);
        writer.write_all(b"abc").unwrap();
        writer.finish().unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"private");
        use std::io::{Read, Seek};
        original.rewind().unwrap();
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abc");
    }

    /// A CD-mode CHD whose hunk size is not a whole number of frames
    /// cannot line frames up with the stream; reject it before any
    /// output exists.
    #[tokio::test]
    async fn extract_rejects_cd_hunks_that_are_not_whole_frames() {
        use crate::util::NoProgress;

        let dir = tempfile::tempdir().unwrap();
        let chd_path = dir.path().join("misaligned.chd");
        crate::disc::chd::test_fixtures::write_raw_cd_chd(
            &chd_path,
            CHD_METADATA_TAG_GD_TRACK,
            "TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:1",
            &[0u8; 4096],
            4096,
            2048,
        );

        let out_cue = dir.path().join("out.cue");
        let err = extract_from_chd(
            &NoProgress,
            chd_path,
            out_cue.clone(),
            None,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChdError::InvalidTrackMetadata(_)));
        assert!(!out_cue.exists());
    }

    /// A raw CD CHD extracts its audio per the metadata family: legacy
    /// `CHGT` stores little-endian samples and the bin keeps the stored
    /// bytes, `CHGD` stores big-endian and the bin gets the byte swap,
    /// and metadata claiming frames past the physical stream is
    /// rejected before anything is written.
    #[tokio::test]
    async fn extract_raw_cd_chd_applies_the_metadata_familys_audio_order() {
        use crate::disc::chd::models::{
            CHD_METADATA_FLAG_HASHED, CHD_METADATA_RESERVED_BYTES, ChdMetadataHeader,
        };
        use crate::disc::chd::writer::ChdWriter;
        use crate::util::NoProgress;

        const AUDIO_FRAMES: usize = 10;
        const DATA_BYTES: usize = 2352;
        let physical_frames = padded_track_frames(AUDIO_FRAMES as u32) as usize;
        let audio: Vec<u8> = (0..AUDIO_FRAMES)
            .flat_map(|frame| (0..DATA_BYTES).map(move |i| (frame * 31 + i) as u8))
            .collect();
        let mut raw = Vec::with_capacity(physical_frames * FRAME_SIZE);
        for frame in 0..physical_frames {
            let mut sector = vec![0u8; FRAME_SIZE];
            if frame < AUDIO_FRAMES {
                sector[..DATA_BYTES]
                    .copy_from_slice(&audio[frame * DATA_BYTES..(frame + 1) * DATA_BYTES]);
            }
            raw.extend_from_slice(&sector);
        }

        async fn extract_raw(
            dir: &tempfile::TempDir,
            tag: [u8; 4],
            metadata_text: &str,
            raw: &[u8],
            out_name: &str,
        ) -> ChdResult<std::path::PathBuf> {
            let mut data = metadata_text.as_bytes().to_vec();
            data.push(0);
            let metadata = vec![ChdMetadataHeader {
                tag,
                flags: CHD_METADATA_FLAG_HASHED,
                reserved: [0; CHD_METADATA_RESERVED_BYTES],
                data,
            }];
            let chd_path = dir.path().join("raw.chd");
            let mut writer = ChdWriter::create_raw(
                std::fs::File::create(&chd_path).unwrap(),
                raw.len() as u64,
                19584,
                FRAME_SIZE as u32,
                metadata,
                Vec::new(),
                None,
            )
            .unwrap();
            writer
                .compress_all_hunks_raw(
                    &mut std::io::Cursor::new(raw),
                    &std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    &CancelToken::new(),
                )
                .unwrap();
            writer.finalize().unwrap();

            let out_cue = dir.path().join(out_name);
            extract_from_chd(
                &NoProgress,
                chd_path,
                out_cue.clone(),
                None,
                CancelToken::new(),
            )
            .await
            .map(|()| out_cue)
        }

        let dir = tempfile::tempdir().unwrap();

        let out_cue = extract_raw(
            &dir,
            CHD_METADATA_TAG_GD_TRACK_LEGACY,
            "TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:10",
            &raw,
            "chgt.cue",
        )
        .await
        .unwrap();
        let bin = std::fs::read(out_cue.with_extension("bin")).unwrap();
        assert_eq!(bin.len(), AUDIO_FRAMES * DATA_BYTES);
        assert_eq!(bin, audio, "legacy metadata keeps the stored bytes");

        let out_cue = extract_raw(
            &dir,
            CHD_METADATA_TAG_GD_TRACK,
            "TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:10 PREGAP:0 PGTYPE:AUDIO PGSUB:NONE POSTGAP:0",
            &raw,
            "chgd.cue",
        )
        .await
        .unwrap();
        let bin = std::fs::read(out_cue.with_extension("bin")).unwrap();
        let mut swapped = audio.clone();
        swap_audio_sector(&mut swapped);
        assert_eq!(bin, swapped, "current metadata gets the byte swap");

        // Frames past the physical stream are rejected before anything
        // is written: no cue, no bin.
        let out_cue = dir.path().join("rejected.cue");
        let err = extract_raw(
            &dir,
            CHD_METADATA_TAG_GD_TRACK,
            "TRACK:1 TYPE:AUDIO SUBTYPE:NONE FRAMES:100",
            &raw,
            "rejected.cue",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChdError::InvalidTrackMetadata(_)));
        assert!(!out_cue.exists());
        assert!(!dir.path().join("rejected.bin").exists());
    }
}
