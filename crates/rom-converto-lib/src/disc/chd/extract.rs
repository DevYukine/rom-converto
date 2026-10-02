//! CHD to disc image extraction: CD-mode CHDs restore a `.cue` plus
//! `.bin`, DVD-mode CHDs a flat `.iso`.

use crate::disc::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::reader::cue_generator::{
    ChdTrackInfo, chd_type_datasize, generate_cue_sheet, parse_chd_track_metadata,
};
use crate::disc::chd::reader::{ChdFlavor, SyncChdHandle};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, run_scratch_write};
use log::{debug, info};
use std::io::Write as _;
use std::path::PathBuf;

use super::*;

/// Extract the CHD at `input_path` back to its disc image at `output_path`.
/// CD accepts `.cue` or no extension (adds `.cue`); DVD accepts anything but
/// `.cue` or `.chd`, with no extension adding `.iso`. A failed or cancelled bin
/// extraction leaves an existing `.bin` untouched.
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
        .map(|t| t.frames as u64 * chd_type_datasize(&t.track_type) as u64)
        .sum();

    let cue_path = output_path;
    let bin_path = cue_path.with_extension("bin");
    let bin_filename = bin_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let cue_content = generate_cue_sheet(&bin_filename, &tracks);

    let total_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    progress.start(
        total_bin_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    run_scratch_write(
        &bin_path,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| -> ChdResult<()> {
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

            let bin_file = std::fs::File::create(&write_path)?;
            let mut bin_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, bin_file);

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

            bin_writer.flush()?;
            Ok(())
        },
    )
    .await?;
    crate::util::atomic_write(&cue_path, true, |file| {
        file.write_all(cue_content.as_bytes())
    })?;

    let bin_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    info!(
        "Extracted: {:.2} MB BIN + CUE from {:?}",
        bin_mb, input_path
    );

    debug!("Extraction complete");
    Ok(())
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
