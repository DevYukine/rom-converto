//! CHD to disc image extraction: CD-mode CHDs restore a `.cue` plus
//! `.bin`, DVD-mode CHDs a flat `.iso`.

use crate::cd::{FRAME_SIZE, IO_BUFFER_SIZE};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::reader::cue_generator::{
    ChdTrackInfo, chd_type_datasize, generate_cue_sheet, parse_chd_track_metadata,
};
use crate::chd::reader::{ChdFlavor, SyncChdHandle};
use crate::util::{
    BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel, run_scratch_write,
};
use log::{debug, info};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::fs;

use super::*;

/// Extract the CHD at `input_path` back to its disc image at
/// `output_path`; on cancel any output file this call created is
/// removed.
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
    let (handle, tracks) = tokio::task::spawn_blocking(
        move || -> ChdResult<(SyncChdHandle, Option<Vec<ChdTrackInfo>>)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            match handle.flavor() {
                ChdFlavor::Ld => Err(ChdError::LdExtractionUnsupported),
                ChdFlavor::Dvd => Ok((handle, None)),
                ChdFlavor::Cd => {
                    let meta_str = cd_track_metadata_text(&handle.metadata).ok_or_else(|| {
                        ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string())
                    })?;
                    let tracks = parse_chd_track_metadata(&meta_str)?;
                    Ok((handle, Some(tracks)))
                }
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

    let cue_path = if output_path.extension().is_some() {
        output_path.clone()
    } else {
        output_path.with_extension("cue")
    };
    let bin_path = cue_path.with_extension("bin");
    let bin_filename = bin_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let total_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    progress.start(
        total_bin_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    let bin_preexisting = fs::metadata(&bin_path).await.is_ok();
    let cue_preexisting = fs::metadata(&cue_path).await.is_ok();

    let bin_owned = bin_path.clone();
    let cue_owned = cue_path.clone();
    let bin_filename_owned = bin_filename;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let task = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, HunkExtractArgs, extract_hunks,
            make_chd_extract_workers,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let hunk_bytes = handle.header.hunk_bytes as usize;
        // Frame maps come from the CHT2 `FRAMES:` counts; padding
        // frames carry width 0 so they drop out of the bin. Legacy
        // rom-converto CHDs stored the stream unpadded.
        let padded = chd_layout_is_padded(&tracks, handle.header.logical_bytes / FRAME_SIZE as u64);
        let (frame_sizes, _) = chd_frame_spans(&tracks, padded);
        let frame_audio = chd_frame_audio(&tracks, padded);

        let bin_file = std::fs::File::create(&bin_owned)?;
        let mut bin_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, bin_file);

        let n_threads = parallelism();
        let workers = make_chd_extract_workers(
            n_threads,
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
                bytes_done: &bytes_done_bg,
                cancel: &cancel_bg,
            },
        );
        pool.shutdown();
        extract_result?;

        use std::io::Write as _;
        bin_writer.flush()?;

        let cue_content = generate_cue_sheet(&bin_filename_owned, &tracks);
        std::fs::write(&cue_owned, cue_content)?;

        Ok(())
    });

    if let Err(err) = await_with_progress_cancel(progress, &bytes_done, task, &cancel).await {
        if !bin_preexisting {
            let _ = fs::remove_file(&bin_path).await;
        }
        if !cue_preexisting {
            let _ = fs::remove_file(&cue_path).await;
        }
        return Err(err);
    }

    let bin_mb = total_bin_bytes as f64 / BYTES_PER_MB;
    info!(
        "Extracted: {:.2} MB BIN + CUE from {:?}",
        bin_mb, input_path
    );

    debug!("Extraction complete");
    Ok(())
}

/// Peek a CHD's metadata to tell DVD-mode (flat ISO) apart from
/// CD-mode (bin/cue with CHT2 track metadata) without extracting
/// anything. Used by [`crate::pipeline::chd_to_cso`] to
/// reject CD-mode CHDs up front, since CSO/ZSO have no track layout.
pub async fn is_dvd_mode_chd(path: PathBuf) -> ChdResult<bool> {
    tokio::task::spawn_blocking(move || -> ChdResult<bool> {
        match crate::chd::reader::open_chd_sync(&path)?.flavor() {
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
    output_path: PathBuf,
    handle: SyncChdHandle,
    cancel: CancelToken,
) -> ChdResult<()> {
    let iso_path = if output_path.extension().is_some() {
        output_path.clone()
    } else {
        output_path.with_extension("iso")
    };

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
            use crate::chd::reader::worker::{
                ChdExtractWork, ChdExtractedOut, extract_hunks_dvd, make_chd_dvd_extract_workers,
            };
            use crate::util::worker_pool::{Pool, parallelism};

            let hunk_bytes = handle.header.hunk_bytes as usize;

            let iso_file = std::fs::File::create(&write_path)?;
            let mut iso_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, iso_file);

            let workers = make_chd_dvd_extract_workers(
                parallelism(),
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
            );
            pool.shutdown();
            extract_result?;

            use std::io::Write as _;
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
