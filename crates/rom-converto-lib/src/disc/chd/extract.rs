//! CHD to disc image extraction: CD-mode CHDs restore a `.cue` and one
//! `.bin` per track, DVD-mode CHDs a flat `.iso`.

use crate::disc::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::reader::cue_generator::{
    ChdTrackInfo, generate_cue_sheet, parse_chd_track_metadata,
};
use crate::disc::chd::reader::{ChdFlavor, SyncChdHandle};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, run_scratch_write};
use log::{debug, info};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use super::*;

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
    let (handle, tracks, output_path) = tokio::task::spawn_blocking(
        move || -> ChdResult<(SyncChdHandle, Option<Vec<ChdTrackInfo>>, PathBuf)> {
            let handle = crate::disc::chd::reader::open_chd_sync(&input_for_peek)?;
            let output_path = extract_target(handle.flavor(), output_path)?;
            match handle.flavor() {
                ChdFlavor::Cd => {
                    let meta_str = cd_track_metadata_text(&handle.metadata).ok_or_else(|| {
                        ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string())
                    })?;
                    let tracks = parse_chd_track_metadata(&meta_str)?;
                    if tracks.len() > MAX_CD_TRACKS {
                        return Err(ChdError::InvalidTrackMetadata(format!(
                            "CD has {} tracks, maximum is {MAX_CD_TRACKS}",
                            tracks.len()
                        )));
                    }
                    Ok((handle, Some(tracks), output_path))
                }
                _ => Ok((handle, None, output_path)),
            }
        },
    )
    .await??;

    let Some(tracks) = tracks else {
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
    let bin_temps: Vec<tempfile::TempPath> = bin_paths
        .iter()
        .map(|path| crate::util::scratch_output_path(path))
        .collect::<io::Result<_>>()?;
    let cue_temp = crate::util::scratch_output_path(&cue_path)?;

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
            // Frame maps come from the CHT2 `FRAMES:` counts; padding
            // frames carry width 0 so they drop out of the bin. Legacy
            // rom-converto CHDs stored the stream unpadded.
            let padded =
                chd_layout_is_padded(&tracks, handle.header.logical_bytes / FRAME_SIZE as u64);
            let (frame_sizes, _) = chd_frame_spans(&tracks, padded);
            let frame_audio = chd_frame_audio(&tracks, padded);

            let mut bin_writer = TrackBinWriter::new(
                bin_temps
                    .iter()
                    .zip(&tracks)
                    .map(|(temp, track)| {
                        (
                            temp.as_ref(),
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
                    frame_sizes: &frame_sizes,
                    frame_audio: &frame_audio,
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
    {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&cue_temp)?;
        file.write_all(cue_content.as_bytes())?;
    }
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

pub(crate) const MAX_CD_TRACKS: usize = 99;

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

struct TrackBinWriter<'a> {
    remaining: std::vec::IntoIter<(&'a Path, u64)>,
    current: Option<BufWriter<File>>,
    bytes_left: u64,
}

impl<'a> TrackBinWriter<'a> {
    fn new(tracks: Vec<(&'a Path, u64)>) -> Self {
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

impl Write for TrackBinWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.bytes_left == 0 {
            if let Some(mut writer) = self.current.take() {
                writer.flush()?;
            }
            for (path, length) in self.remaining.by_ref() {
                if length == 0 {
                    continue;
                }
                let file = OpenOptions::new().write(true).truncate(true).open(path)?;
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
        move |write_path, bytes_done, cancel| -> ChdResult<()> {
            use crate::disc::chd::reader::worker::{
                ChdExtractWork, ChdExtractedOut, chd_read_admission, extract_hunks_dvd,
                make_chd_dvd_extract_workers,
            };
            use crate::util::worker_pool::{Pool, parallelism};

            let hunk_bytes = handle.header.hunk_bytes as usize;
            let admission =
                chd_read_admission(hunk_bytes, parallelism(), handle.map.len() as u64, true);

            let iso_file = std::fs::File::create(&write_path)?;
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
        let paths: Vec<_> = (0..5)
            .map(|track| {
                crate::util::scratch_output_path(&dir.path().join(format!("{track}.bin"))).unwrap()
            })
            .collect();
        let mut writer = TrackBinWriter::new(
            paths
                .iter()
                .zip([0, 3, 0, 2, 0])
                .map(|(path, length)| (path.as_ref(), length))
                .collect(),
        );
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
        let first = crate::util::scratch_output_path(&dir.path().join("first.bin")).unwrap();
        let second = crate::util::scratch_output_path(&dir.path().join("second.bin")).unwrap();
        let mut writer = TrackBinWriter::new(vec![(&first, 2), (&second, 3)]);
        writer.write_all(b"abcd").unwrap();
        assert_eq!(
            writer.finish().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn track_writer_rejects_an_overlong_stream() {
        let dir = tempfile::tempdir().unwrap();
        let first = crate::util::scratch_output_path(&dir.path().join("first.bin")).unwrap();
        let second = crate::util::scratch_output_path(&dir.path().join("second.bin")).unwrap();
        let mut writer = TrackBinWriter::new(vec![(&first, 2), (&second, 1)]);
        assert_eq!(
            writer.write_all(b"abcd").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
