//! CHD (Compressed Hunks of Data) compression and extraction for CD and DVD
//! disc images, targeting the same V5 format chdman writes.
//!
//! CD input (`.cue`/`.bin`) keeps its sidecar files, so restoring a CHD back
//! to disc form is called extract rather than decompress; see
//! [`crate::chd::error`] for the failure modes.

use crate::cd::{CD_HUNK_BYTES, IO_BUFFER_SIZE, SECTOR_SIZE};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::models::{CHD_METADATA_TAG_CD, CHD_METADATA_TAG_DVD, ChdHeaderV5, SHA1_BYTES};
use crate::chd::reader::cue_generator::{
    ChdTrackInfo, chd_type_datasize, generate_cue_sheet, parse_chd_track_metadata,
};
use crate::chd::writer::ChdWriter;
use crate::chd::writer::metadata::MetadataHash;
use crate::cue::CueParser;
use crate::cue::models::{CueFile, CueSheet, FileType, Index, Msf, Track, TrackType};
use crate::util::hash::{FileDigests, HashAlgo, MultiHasher};
use crate::util::iso9660::{DiscKind, detect_disc_kind};
use crate::util::{BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel};
use log::{debug, info, warn};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub mod compression;
pub mod error;
pub mod info;
pub(crate) mod map;
mod models;
pub(crate) mod reader;
pub(crate) mod writer;

/// chdman's `createdvd` default: two 2048-byte sectors per hunk.
pub const DVD_HUNK_BYTES_DEFAULT: u32 = 4096;
/// PPSSPP serves the PSP's 2048-byte block API straight from hunks
/// and warns about anything larger, so detected PSP input defaults
/// to single-sector hunks.
pub const DVD_HUNK_BYTES_PSP: u32 = 2048;

/// Options for DVD-mode CHD creation (PS2/PSP ISO input).
#[derive(Debug, Clone, Default)]
pub struct ChdDvdOptions {
    /// Hunk size override; the default is picked per detected
    /// console ([`DVD_HUNK_BYTES_DEFAULT`] / [`DVD_HUNK_BYTES_PSP`]).
    pub hunk_size: Option<u32>,
    /// Add zstd as a third codec. Off by default: the libchdr in
    /// AetherSX2/NetherSX2 rejects CHDs that list zstd.
    pub allow_zstd: bool,
    pub force: bool,
}

/// Which CHD flavor to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscMode {
    Cd,
    Dvd,
}

/// A sibling temp path in the output directory so an interrupted write
/// never lands on the final name and a pre-existing overwrite target
/// survives until the rename.
fn scratch_output_path(output: &std::path::Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    output.with_file_name(name)
}

/// Remove the scratch file and report the cancellation; used as the
/// `on_cancel` fallback for the race where the blocking pipeline
/// finished a hunk just as the token fired.
fn cancel_cleanup(write_path: &std::path::Path) -> impl FnOnce() -> ChdError {
    let write_path = write_path.to_path_buf();
    move || {
        let _ = std::fs::remove_file(&write_path);
        ChdError::Cancelled
    }
}

/// Route a disc image to the right CHD writer: `.cue` input is
/// CD-mode; an `.iso` is probed with [`detect_disc_kind`] and CD-media
/// images (PS1, PS2-CD) become CD-mode CHDs while DVD-media images
/// (PS2-DVD, PSP) become DVD-mode CHDs (the chdman createcd/createdvd
/// split that trips users up). `mode` overrides the auto-routing.
pub async fn convert_disc_to_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    mode: Option<DiscMode>,
    opts: ChdDvdOptions,
) -> ChdResult<()> {
    convert_disc_to_chd_cancellable(
        progress,
        input_path,
        output_path,
        mode,
        opts,
        CancelToken::new(),
    )
    .await
}

/// Like [`convert_disc_to_chd`] but observes `cancel` at every hunk
/// boundary; on cancel the partial CHD is removed and a pre-existing
/// overwrite target is left untouched.
pub async fn convert_disc_to_chd_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    mode: Option<DiscMode>,
    opts: ChdDvdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    let is_cue = input_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    match (mode, is_cue) {
        (None | Some(DiscMode::Cd), true) => {
            convert_to_chd(progress, input_path, output_path, opts.force, cancel).await
        }
        (Some(DiscMode::Dvd), true) => Err(ChdError::DvdModeNeedsIso),
        (Some(DiscMode::Cd), false) => {
            convert_iso_to_cd_chd(progress, input_path, output_path, opts.force, cancel).await
        }
        (Some(DiscMode::Dvd), false) => {
            convert_iso_to_chd(progress, input_path, output_path, opts, cancel).await
        }
        (None, false) => {
            let detect_path = input_path.clone();
            let kind =
                tokio::task::spawn_blocking(move || detect_disc_kind(&detect_path)).await??;
            match kind {
                DiscKind::Ps1 | DiscKind::Ps2Cd => {
                    info!("{} detected, writing CD-mode CHD", kind.label());
                    if kind == DiscKind::Ps2Cd {
                        warn!(
                            "{:?} looks like a CD-media PS2 game; if the original disc had \
                             audio tracks, convert from its bin/cue instead so they survive",
                            input_path
                        );
                    }
                    convert_iso_to_cd_chd(progress, input_path, output_path, opts.force, cancel)
                        .await
                }
                DiscKind::Ps2Dvd | DiscKind::Psp | DiscKind::UnknownIso => {
                    info!("{} detected, writing DVD-mode CHD", kind.label());
                    convert_iso_to_chd_with_kind(
                        progress,
                        input_path,
                        output_path,
                        opts,
                        Some(kind),
                        cancel,
                    )
                    .await
                }
            }
        }
    }
}

/// Compress every `.cue` and `.iso` under `input_dir`, descending into
/// subdirectories up to `max_depth` (`None` for unlimited). Outputs land
/// next to their inputs with the extension replaced by `.chd`, or mirror
/// the source tree under `output_dir` when one is given.
pub async fn convert_disc_to_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdDvdOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    let discs = crate::util::fs::collect_files_with_exts(input_dir, &["cue", "iso"], max_depth)?;
    if discs.is_empty() {
        warn!("No .cue or .iso inputs found in {}", input_dir.display());
        return Ok(());
    }

    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }

    total_progress.start(
        discs.len() as u64,
        &format!("Compressing {} discs", discs.len()),
    );

    for path in discs {
        let output =
            crate::util::place_in_dir_mirrored(&path.with_extension("chd"), input_dir, output_dir);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) =
            convert_disc_to_chd(progress, path.clone(), output, None, opts.clone()).await
        {
            warn!("Failed to compress {}: {err}", path.display());
        }
        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

/// Compress a 2048-byte-sector ISO (PS2 DVD, PSP UMD) to a DVD-mode
/// CHD, the equivalent of `chdman createdvd`.
pub async fn convert_iso_to_chd(
    progress: &dyn ProgressReporter,
    iso_path: PathBuf,
    output_path: PathBuf,
    opts: ChdDvdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    convert_iso_to_chd_with_kind(progress, iso_path, output_path, opts, None, cancel).await
}

/// DVD-mode compress with an already-detected [`DiscKind`], so the
/// auto-routing in [`convert_disc_to_chd`] does not probe twice.
async fn convert_iso_to_chd_with_kind(
    progress: &dyn ProgressReporter,
    iso_path: PathBuf,
    output_path: PathBuf,
    opts: ChdDvdOptions,
    kind: Option<DiscKind>,
    cancel: CancelToken,
) -> ChdResult<()> {
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    let iso_size = fs::metadata(&iso_path).await?.len();

    let kind = match kind {
        Some(kind) => kind,
        None => {
            let detect_path = iso_path.clone();
            tokio::task::spawn_blocking(move || detect_disc_kind(&detect_path)).await??
        }
    };
    debug!("Detected disc kind: {:?}", kind);
    if kind == DiscKind::Ps2Cd {
        warn!(
            "{:?} looks like a CD-media PS2 game; if the original disc had audio \
             tracks, convert from its bin/cue instead so they survive",
            iso_path
        );
    }

    let hunk_size = opts.hunk_size.unwrap_or(match kind {
        DiscKind::Psp => DVD_HUNK_BYTES_PSP,
        _ => DVD_HUNK_BYTES_DEFAULT,
    });

    let total_mb = iso_size as f64 / BYTES_PER_MB;
    progress.start(
        iso_size,
        &format!("Compressing to CHD (~{:.2} MB)", total_mb),
    );

    let write_path = scratch_output_path(&output_path);
    let iso_owned = iso_path.clone();
    let write_owned = write_path.clone();
    let allow_zstd = opts.allow_zstd;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let iso_file = std::fs::File::open(&iso_owned)?;
        let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

        let mut writer = ChdWriter::create_dvd(&write_owned, iso_size, hunk_size, allow_zstd)?;
        writer.compress_all_hunks_dvd(&mut iso_reader, &bytes_done_bg, &cancel_bg)?;
        writer.finalize()?;
        Ok(())
    });

    if let Err(err) = await_with_progress_cancel(
        progress,
        &bytes_done,
        handle,
        &cancel,
        cancel_cleanup(&write_path),
    )
    .await
    {
        let _ = fs::remove_file(&write_path).await;
        return Err(err);
    }
    fs::rename(&write_path, &output_path).await?;

    let chd_size = fs::metadata(&output_path).await?.len();
    let compression_ratio = (chd_size as f64 / iso_size as f64) * 100.0;
    info!(
        "Original: {:.2} MB, CHD: {:.2} MB ({:.1}% compression ratio)",
        total_mb,
        chd_size as f64 / BYTES_PER_MB,
        compression_ratio
    );
    Ok(())
}

/// chdman pads every track, including a lone final one, to a 4-frame
/// boundary; the zero padding frames count into the logical size and
/// the raw SHA-1, while CHT2 `FRAMES:` records the real count.
/// Measured against chdman 0.288: a 10-sector iso produces a CHD with
/// logical size 12 * 2448 and a data SHA-1 over all 12 frames.
const CD_TRACK_PADDING: u32 = 4;

fn padded_track_frames(data_sectors: u32) -> u32 {
    data_sectors.div_ceil(CD_TRACK_PADDING) * CD_TRACK_PADDING
}

/// The track list `chdman createcd` synthesizes for a flat `.iso`
/// input: one MODE1/2048 data track starting at frame 0.
fn synth_mode1_2048_cue_sheet() -> CueSheet {
    CueSheet {
        files: vec![CueFile {
            filename: String::new(),
            file_type: FileType::Binary,
        }],
        tracks: vec![Track {
            number: 1,
            track_type: TrackType::Mode1_2048,
            indices: vec![Index {
                number: 1,
                position: Msf::from_lba(0),
            }],
            pregap: None,
            postgap: None,
            file_index: 0,
        }],
    }
}

/// Compress a CD-media 2048-byte-sector ISO (PS1, PS2-CD) to a
/// CD-mode CHD with a single MODE1/2048 track, the equivalent of
/// `chdman createcd -i game.iso`.
pub async fn convert_iso_to_cd_chd(
    progress: &dyn ProgressReporter,
    iso_path: PathBuf,
    output_path: PathBuf,
    force: bool,
    cancel: CancelToken,
) -> ChdResult<()> {
    if fs::metadata(&output_path).await.is_ok() && !force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    let sector_data_size = TrackType::Mode1_2048.block_size() as u64;
    let iso_size = fs::metadata(&iso_path).await?.len();
    if iso_size == 0 || !iso_size.is_multiple_of(sector_data_size) {
        return Err(ChdError::IsoNotSectorAligned { size: iso_size });
    }
    let data_sectors: u32 = (iso_size / sector_data_size)
        .try_into()
        .map_err(|_| ChdError::InvalidHunkSize)?;
    let total_sectors = padded_track_frames(data_sectors);
    let cue_sheet = synth_mode1_2048_cue_sheet();

    debug!("CD-mode iso: {data_sectors} data sectors, {total_sectors} padded frames");
    let total_mb = iso_size as f64 / BYTES_PER_MB;
    progress.start(
        iso_size,
        &format!("Compressing to CHD (~{:.2} MB)", total_mb),
    );

    let write_path = scratch_output_path(&output_path);
    let iso_owned = iso_path.clone();
    let write_owned = write_path.clone();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let iso_file = std::fs::File::open(&iso_owned)?;
        let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

        let mut writer = ChdWriter::create(
            &write_owned,
            total_sectors,
            data_sectors,
            CD_HUNK_BYTES,
            &cue_sheet,
        )?;
        writer.compress_all_hunks(
            &mut iso_reader,
            total_sectors,
            data_sectors,
            sector_data_size as usize,
            &bytes_done_bg,
            &cancel_bg,
        )?;
        writer.finalize()?;
        Ok(())
    });

    if let Err(err) = await_with_progress_cancel(
        progress,
        &bytes_done,
        handle,
        &cancel,
        cancel_cleanup(&write_path),
    )
    .await
    {
        let _ = fs::remove_file(&write_path).await;
        return Err(err);
    }
    fs::rename(&write_path, &output_path).await?;

    let chd_size = fs::metadata(&output_path).await?.len();
    let compression_ratio = (chd_size as f64 / iso_size as f64) * 100.0;
    info!(
        "Original: {:.2} MB, CHD: {:.2} MB ({:.1}% compression ratio)",
        total_mb,
        chd_size as f64 / BYTES_PER_MB,
        compression_ratio
    );
    Ok(())
}

pub async fn convert_to_chd(
    progress: &dyn ProgressReporter,
    cue_path: PathBuf,
    output_path: PathBuf,
    force: bool,
    cancel: CancelToken,
) -> ChdResult<()> {
    if fs::metadata(&output_path).await.is_ok() && !force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    debug!("Parsing CUE file: {:?}", cue_path);
    let parser = CueParser::new(&cue_path);
    let cue_sheet = parser.parse().await?;

    let bin_path = if cue_sheet.files.is_empty() {
        return Err(ChdError::NoFileReferencedInCueSheet);
    } else {
        let cue_dir = cue_path.parent().unwrap_or(std::path::Path::new("."));
        cue_dir.join(&cue_sheet.files[0].filename)
    };

    debug!("Opening BIN file: {:?}", bin_path);
    let bin_size = fs::metadata(&bin_path).await?.len();
    let total_sectors: u32 = (bin_size / SECTOR_SIZE as u64)
        .try_into()
        .map_err(|_| ChdError::InvalidHunkSize)?;

    debug!("Total sectors: {}", total_sectors);
    debug!("Creating CHD file: {:?}", output_path);

    let total_mb = (bin_size as f64) / BYTES_PER_MB;
    progress.start(
        bin_size,
        &format!("Compressing to CHD (~{:.2} MB)", total_mb),
    );

    // Hand the full blocking pipeline (open bin + compress +
    // finalize) to a single `spawn_blocking` and poll a shared
    // `AtomicU64` for progress ticks. Same shape as the RVZ
    // compress entry in `nintendo/rvz/compress/mod.rs`.
    let write_path = scratch_output_path(&output_path);
    let bin_path_owned = bin_path.clone();
    let write_owned = write_path.clone();
    let cue_sheet_owned = cue_sheet.clone();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let bin_file = std::fs::File::open(&bin_path_owned)?;
        let mut bin_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, bin_file);

        let mut writer = ChdWriter::create(
            &write_owned,
            total_sectors,
            total_sectors,
            CD_HUNK_BYTES,
            &cue_sheet_owned,
        )?;

        writer.compress_all_hunks(
            &mut bin_reader,
            total_sectors,
            total_sectors,
            SECTOR_SIZE,
            &bytes_done_bg,
            &cancel_bg,
        )?;
        writer.finalize()?;
        Ok(())
    });

    if let Err(err) = await_with_progress_cancel(
        progress,
        &bytes_done,
        handle,
        &cancel,
        cancel_cleanup(&write_path),
    )
    .await
    {
        let _ = fs::remove_file(&write_path).await;
        return Err(err);
    }
    fs::rename(&write_path, &output_path).await?;

    let chd_size = fs::metadata(&output_path).await?.len();
    let original_size = bin_size;
    let saved_bytes = original_size.saturating_sub(chd_size);
    let compression_ratio = (chd_size as f64 / original_size as f64) * 100.0;
    let saved_mb = saved_bytes as f64 / BYTES_PER_MB;
    let chd_mb = chd_size as f64 / BYTES_PER_MB;

    info!(
        "Original: {:.2} MB, CHD: {:.2} MB, Saved: {:.2} MB ({:.1}% compression ratio)",
        total_mb, chd_mb, saved_mb, compression_ratio
    );

    debug!("Conversion complete");
    Ok(())
}

/// One track's decoded digest set plus its CHT2 identity. `dat`
/// maps this into its own `TrackDigests` at the digest boundary so
/// this module never depends on the `dat` types.
#[derive(Debug, Clone)]
pub struct ChdTrackDigest {
    pub track_number: u8,
    pub track_type: String,
    pub digests: FileDigests,
}

/// Per-frame span map for the decoded CD stream: `frame_sizes[i]` is
/// the payload width of frame `i` and `frame_track[i]` is the index
/// (into `tracks`) of the track that owns frame `i`. Both vecs are
/// laid out exactly as `convert_to_cue_bin`/`extract_hunks` shape the
/// stream, so hashing frame by frame through them reproduces the bin
/// `chdman extractcd` writes. Pure so it is unit-testable against a
/// synthetic CHT2 metadata string.
pub(crate) fn chd_frame_spans(tracks: &[ChdTrackInfo]) -> (Vec<usize>, Vec<usize>) {
    let frame_sizes: Vec<usize> = tracks
        .iter()
        .flat_map(|t| std::iter::repeat_n(chd_type_datasize(&t.track_type), t.frames as usize))
        .collect();
    let frame_track: Vec<usize> = tracks
        .iter()
        .enumerate()
        .flat_map(|(i, t)| std::iter::repeat_n(i, t.frames as usize))
        .collect();
    (frame_sizes, frame_track)
}

/// Decoded payload byte count of one track: `frames * datasize`. This
/// is the value stored as each track's `FileDigests.size_bytes`.
pub(crate) fn chd_track_decoded_size(track: &ChdTrackInfo) -> u64 {
    track.frames as u64 * chd_type_datasize(&track.track_type) as u64
}

pub(crate) fn compute_overall_sha1(
    raw_sha1: [u8; SHA1_BYTES],
    metadata_hashes: &[MetadataHash],
) -> [u8; SHA1_BYTES] {
    let mut overall = Sha1::new();
    overall.update(raw_sha1);

    if !metadata_hashes.is_empty() {
        let mut hashes = metadata_hashes.to_vec();
        hashes.sort_by(|a, b| a.tag.cmp(&b.tag).then(a.sha1.cmp(&b.sha1)));
        for hash in hashes {
            overall.update(hash.tag);
            overall.update(hash.sha1);
        }
    }

    overall.finalize().into()
}

pub async fn extract_from_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    parent_path: Option<PathBuf>,
) -> ChdResult<()> {
    extract_from_chd_cancellable(
        progress,
        input_path,
        output_path,
        parent_path,
        CancelToken::new(),
    )
    .await
}

/// Like [`extract_from_chd`] but observes `cancel` at every hunk
/// boundary; on cancel any output file this call created is removed.
pub async fn extract_from_chd_cancellable(
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

    // Peek at the header + metadata so the output type (DVD iso vs
    // CD bin/cue) is known and the progress bar can size itself
    // before the big spawn_blocking kicks off. `total_frames` comes
    // from the CHT2 track metadata, not from `header.logical_bytes`:
    // chdman rounds logical_bytes up to a full hunk boundary, so it
    // can overstate the real sector count by up to
    // `frames_per_hunk - 1`.
    let input_for_peek = input_path.clone();
    let (header, total_bin_bytes, is_dvd) =
        tokio::task::spawn_blocking(move || -> ChdResult<(ChdHeaderV5, u64, bool)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            if handle
                .metadata
                .iter()
                .any(|m| m.tag == CHD_METADATA_TAG_DVD)
            {
                return Ok((handle.header, 0, true));
            }
            let cd_meta = handle
                .metadata
                .iter()
                .find(|m| m.tag == CHD_METADATA_TAG_CD)
                .ok_or_else(|| {
                    ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string())
                })?;
            let meta_str = String::from_utf8_lossy(&cd_meta.data);
            let meta_str = meta_str.trim_end_matches('\0');
            let tracks = parse_chd_track_metadata(meta_str)?;
            let total_bin_bytes: u64 = tracks
                .iter()
                .map(|t| t.frames as u64 * chd_type_datasize(&t.track_type) as u64)
                .sum();
            Ok((handle.header, total_bin_bytes, false))
        })
        .await??;

    if is_dvd {
        return extract_dvd_iso(
            progress,
            input_path,
            output_path,
            header.logical_bytes,
            cancel,
        )
        .await;
    }

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

    let input_owned = input_path.clone();
    let bin_owned = bin_path.clone();
    let cue_owned = cue_path.clone();
    let bin_filename_owned = bin_filename;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        use crate::chd::reader::open_chd_sync;
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, extract_hunks, make_chd_extract_workers,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;

        let cd_meta = handle
            .metadata
            .iter()
            .find(|m| m.tag == CHD_METADATA_TAG_CD)
            .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;
        let meta_str = String::from_utf8_lossy(&cd_meta.data);
        let meta_str = meta_str.trim_end_matches('\0');
        let tracks = parse_chd_track_metadata(meta_str)?;

        let hunk_bytes = handle.header.hunk_bytes as usize;
        // Use the CHT2 `FRAMES:` sums, not `logical_bytes`; see
        // the outer peek above.
        let frame_sizes: Vec<usize> = tracks
            .iter()
            .flat_map(|t| std::iter::repeat_n(chd_type_datasize(&t.track_type), t.frames as usize))
            .collect();

        let bin_file = std::fs::File::create(&bin_owned)?;
        let mut bin_writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, bin_file);

        let n_threads = parallelism();
        let workers = make_chd_extract_workers(n_threads, &handle.file, hunk_bytes)?;
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(workers);

        let extract_result = extract_hunks(
            &pool,
            &handle.map,
            &mut bin_writer,
            hunk_bytes,
            &frame_sizes,
            &bytes_done_bg,
            &cancel_bg,
        );
        pool.shutdown();
        extract_result?;

        use std::io::Write as _;
        bin_writer.flush()?;

        let cue_content = generate_cue_sheet(&bin_filename_owned, &tracks);
        std::fs::write(&cue_owned, cue_content)?;

        Ok(())
    });

    let on_cancel = ChdError::Cancelled;
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || on_cancel).await
    {
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

/// DVD extract path: one flat `.iso`, no cue sheet.
async fn extract_dvd_iso(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    logical_bytes: u64,
    cancel: CancelToken,
) -> ChdResult<()> {
    let iso_path = if output_path.extension().is_some() {
        output_path.clone()
    } else {
        output_path.with_extension("iso")
    };

    let total_mb = logical_bytes as f64 / BYTES_PER_MB;
    progress.start(
        logical_bytes,
        &format!("Extracting from CHD (~{:.2} MB)", total_mb),
    );

    let write_path = scratch_output_path(&iso_path);
    let input_owned = input_path.clone();
    let write_owned = write_path.clone();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        use crate::chd::reader::open_chd_sync;
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, extract_hunks_dvd, make_chd_dvd_extract_workers,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;
        let hunk_bytes = handle.header.hunk_bytes as usize;
        let logical_bytes = handle.header.logical_bytes;

        let iso_file = std::fs::File::create(&write_owned)?;
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
            &bytes_done_bg,
            &cancel_bg,
        );
        pool.shutdown();
        extract_result?;

        use std::io::Write as _;
        iso_writer.flush()?;
        Ok(())
    });

    if let Err(err) = await_with_progress_cancel(
        progress,
        &bytes_done,
        handle,
        &cancel,
        cancel_cleanup(&write_path),
    )
    .await
    {
        let _ = fs::remove_file(&write_path).await;
        return Err(err);
    }
    fs::rename(&write_path, &iso_path).await?;

    info!(
        "Extracted: {:.2} MB ISO from {}",
        total_mb,
        input_path.display()
    );
    Ok(())
}

pub async fn verify_chd(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    parent_path: Option<PathBuf>,
    fix: bool,
) -> ChdResult<()> {
    verify_chd_cancellable(progress, input_path, parent_path, fix, CancelToken::new()).await
}

/// Like [`verify_chd`] but observes `cancel` at every hunk boundary.
/// Verify writes no output, so cancellation only stops the read with
/// [`ChdError::Cancelled`].
pub async fn verify_chd_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    parent_path: Option<PathBuf>,
    fix: bool,
    cancel: CancelToken,
) -> ChdResult<()> {
    if parent_path.is_some() {
        return Err(ChdError::ParentChdNotSupported);
    }

    debug!("Opening CHD file for verification: {:?}", input_path);

    // Peek header + metadata hashes up front so the progress bar
    // can size itself and so the fix-path (rewrite header SHA1s)
    // has a metadata snapshot to rebuild the overall hash from.
    let input_for_peek = input_path.clone();
    let (header, metadata_hashes): (ChdHeaderV5, Vec<MetadataHash>) =
        tokio::task::spawn_blocking(move || -> ChdResult<(ChdHeaderV5, Vec<MetadataHash>)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            let hashes: Vec<MetadataHash> = handle
                .metadata
                .iter()
                .filter(|m| m.flags & crate::chd::models::CHD_METADATA_FLAG_HASHED != 0)
                .map(|m| MetadataHash {
                    tag: m.tag,
                    sha1: <[u8; SHA1_BYTES]>::from(Sha1::digest(&m.data)),
                })
                .collect();
            Ok((handle.header, hashes))
        })
        .await??;

    let logical_bytes = header.logical_bytes;
    progress.start(logical_bytes, "Verifying CHD integrity");

    let input_owned = input_path.clone();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<[u8; SHA1_BYTES]> {
        use crate::chd::reader::open_chd_sync;
        use crate::chd::reader::worker::{
            ChdExtractWork, ChdExtractedOut, make_chd_dvd_extract_workers,
            make_chd_extract_workers, verify_hunks,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;
        let hunk_bytes = handle.header.hunk_bytes as usize;
        let logical_bytes = handle.header.logical_bytes;

        let n_threads = parallelism();
        let is_dvd = handle
            .metadata
            .iter()
            .any(|m| m.tag == CHD_METADATA_TAG_DVD);
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = if is_dvd {
            Pool::spawn(make_chd_dvd_extract_workers(
                n_threads,
                &handle.file,
                hunk_bytes,
                handle.header.compressors(),
            )?)
        } else {
            Pool::spawn(make_chd_extract_workers(
                n_threads,
                &handle.file,
                hunk_bytes,
            )?)
        };

        let mut raw_sha1_hasher = Sha1::new();
        let verify_result = verify_hunks(
            &pool,
            &handle.map,
            &mut raw_sha1_hasher,
            hunk_bytes,
            logical_bytes,
            &bytes_done_bg,
            &cancel_bg,
        );
        pool.shutdown();
        verify_result?;

        let computed: [u8; SHA1_BYTES] = raw_sha1_hasher.finalize().into();
        Ok(computed)
    });

    let computed_raw = await_with_progress_cancel(progress, &bytes_done, handle, &cancel, || {
        ChdError::Cancelled
    })
    .await?;

    let expected_raw = header.raw_sha1;
    if computed_raw != expected_raw {
        info!(
            "Raw SHA1 mismatch: expected {}, got {}",
            hex::encode(expected_raw),
            hex::encode(computed_raw)
        );
        if fix {
            fix_sha1(&input_path, computed_raw, &metadata_hashes).await?;
            info!("SHA1 updated to correct value in CHD file");
            return Ok(());
        }
        return Err(ChdError::Sha1Mismatch {
            expected: hex::encode(expected_raw),
            actual: hex::encode(computed_raw),
        });
    }
    info!("Raw SHA-1 verification passed");

    let computed_overall = compute_overall_sha1(computed_raw, &metadata_hashes);
    let expected_overall = header.sha1;
    if computed_overall != expected_overall {
        info!(
            "Overall SHA1 mismatch: expected {}, got {}",
            hex::encode(expected_overall),
            hex::encode(computed_overall)
        );
        if fix {
            fix_sha1(&input_path, computed_raw, &metadata_hashes).await?;
            info!("SHA1 updated to correct value in CHD file");
            return Ok(());
        }
        return Err(ChdError::Sha1Mismatch {
            expected: hex::encode(expected_overall),
            actual: hex::encode(computed_overall),
        });
    }

    info!(
        "Overall SHA-1 verification passed (SHA-1: {})",
        hex::encode(computed_overall)
    );

    Ok(())
}

/// Digest a CHD's decoded content in a single streaming pass, no temp
/// files. CD-mode CHDs return one [`ChdTrackDigest`] per CHT2 track
/// plus the whole concatenated-bin digest. DVD-mode CHDs (no CHT2
/// metadata) return an empty track list and the flat decoded ISO
/// digest as `whole`; the caller treats that as a single stream.
///
/// The per-track shaping matches [`extract_from_chd`] exactly (CHT2
/// `FRAMES:` counts, per-frame datasize slicing), so each track's
/// digest equals the corresponding slice of the extracted bin and
/// `whole` equals the extracted bin's digest.
///
/// Synchronous: intended to run inside the caller's `spawn_blocking`.
/// Progress is relayed through the shared `bytes_done` counter, same
/// convention as [`extract_from_chd`]'s blocking body.
pub fn digest_chd_tracks(
    path: &std::path::Path,
    algos: &[HashAlgo],
    bytes_done: &Arc<AtomicU64>,
    cancel: &CancelToken,
) -> ChdResult<(Vec<ChdTrackDigest>, FileDigests)> {
    use crate::chd::reader::open_chd_sync;
    use crate::chd::reader::worker::{
        ChdExtractWork, ChdExtractedOut, digest_hunks_dvd, digest_hunks_per_track,
        make_chd_dvd_extract_workers, make_chd_extract_workers,
    };
    use crate::util::worker_pool::{Pool, parallelism};

    let handle = open_chd_sync(path)?;
    let hunk_bytes = handle.header.hunk_bytes as usize;
    let n_threads = parallelism();

    let is_dvd = handle
        .metadata
        .iter()
        .any(|m| m.tag == CHD_METADATA_TAG_DVD);

    if is_dvd {
        // Flat decoded stream capped at logical_bytes, same coverage
        // as extract_hunks_dvd.
        let logical_bytes = handle.header.logical_bytes;
        let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> =
            Pool::spawn(make_chd_dvd_extract_workers(
                n_threads,
                &handle.file,
                hunk_bytes,
                handle.header.compressors(),
            )?);
        let mut whole = MultiHasher::new(algos);
        let result = digest_hunks_dvd(
            &pool,
            &handle.map,
            hunk_bytes,
            logical_bytes,
            &mut whole,
            bytes_done,
            cancel,
        );
        pool.shutdown();
        result?;
        return Ok((Vec::new(), whole.finalize(logical_bytes)));
    }

    let cd_meta = handle
        .metadata
        .iter()
        .find(|m| m.tag == CHD_METADATA_TAG_CD)
        .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;
    let meta_str = String::from_utf8_lossy(&cd_meta.data);
    let meta_str = meta_str.trim_end_matches('\0');
    let tracks = parse_chd_track_metadata(meta_str)?;

    let (frame_sizes, frame_track) = chd_frame_spans(&tracks);
    let mut hashers: Vec<MultiHasher> =
        (0..tracks.len()).map(|_| MultiHasher::new(algos)).collect();
    let mut whole = MultiHasher::new(algos);

    let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> = Pool::spawn(
        make_chd_extract_workers(n_threads, &handle.file, hunk_bytes)?,
    );
    let result = digest_hunks_per_track(
        &pool,
        &handle.map,
        hunk_bytes,
        &frame_sizes,
        &frame_track,
        &mut hashers,
        &mut whole,
        bytes_done,
        cancel,
    );
    pool.shutdown();
    result?;

    let whole_size: u64 = frame_sizes.iter().map(|&s| s as u64).sum();
    let track_digests = tracks
        .iter()
        .zip(hashers)
        .map(|(t, h)| ChdTrackDigest {
            track_number: t.track_number,
            track_type: t.track_type.clone(),
            digests: h.finalize(chd_track_decoded_size(t)),
        })
        .collect();
    Ok((track_digests, whole.finalize(whole_size)))
}

async fn fix_sha1(
    path: &std::path::Path,
    raw_sha1: [u8; SHA1_BYTES],
    metadata_hashes: &[MetadataHash],
) -> ChdResult<()> {
    use tokio::io::AsyncSeekExt;

    let overall_sha1 = compute_overall_sha1(raw_sha1, metadata_hashes);

    // SHA1 field offsets in the CHD v5 header (byte-counted from magic):
    // 8 + 4 + 4 + 16 + 8 + 8 + 8 + 4 + 4 = 64 for raw_sha1, +20 for sha1.
    const RAW_SHA1_OFFSET: u64 = 64;
    const SHA1_OFFSET: u64 = 84;

    let mut file = tokio::fs::OpenOptions::new().write(true).open(path).await?;

    file.seek(std::io::SeekFrom::Start(RAW_SHA1_OFFSET)).await?;
    file.write_all(&raw_sha1).await?;

    file.seek(std::io::SeekFrom::Start(SHA1_OFFSET)).await?;
    file.write_all(&overall_sha1).await?;

    file.flush().await?;

    Ok(())
}

fn collect_files_with_ext(
    dir: &std::path::Path,
    ext: &str,
    max_depth: Option<usize>,
) -> ChdResult<Vec<PathBuf>> {
    Ok(crate::util::fs::collect_files_with_exts(
        dir,
        &[ext],
        max_depth,
    )?)
}

/// Extract every `.chd` in `input_dir` beside its input: CD-mode CHDs
/// become `.cue` + `.bin`, DVD-mode CHDs become `.iso` (the output
/// extension is derived per file by [`extract_from_chd`]). A failure
/// on one file is logged and skipped rather than aborting the batch.
pub async fn extract_from_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: PathBuf,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    let chds = collect_files_with_ext(&input_dir, "chd", max_depth)?;
    if chds.is_empty() {
        info!("No .chd files found in {}", input_dir.display());
        return Ok(());
    }
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    total_progress.start(
        chds.len() as u64,
        &format!("Extracting {} chd files", chds.len()),
    );
    for chd in chds {
        let output =
            crate::util::place_in_dir_mirrored(&chd.with_extension(""), &input_dir, output_dir);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = extract_from_chd(progress, chd.clone(), output, None).await {
            warn!("Skipping {}: {}", chd.display(), e);
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    Ok(())
}

/// Verify every `.chd` in `input_dir`. Logs a per-file failure and a final
/// `Verified N files: X OK, Y failed` summary.
pub async fn verify_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: PathBuf,
    fix: bool,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    let chds = collect_files_with_ext(&input_dir, "chd", max_depth)?;
    if chds.is_empty() {
        info!("No .chd files found in {}", input_dir.display());
        return Ok(());
    }
    let total = chds.len();
    total_progress.start(total as u64, &format!("Verifying {total} chd files"));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for chd in chds {
        match verify_chd(progress, chd.clone(), None, fix).await {
            Ok(()) => ok += 1,
            Err(e) => {
                failed += 1;
                warn!("{}: {}", chd.display(), e);
            }
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    info!("Verified {total} files: {ok} OK, {failed} failed");
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    /// Alternating compressible runs and xorshift noise so the codec
    /// slots and the store-raw path all appear in the map.
    pub(crate) fn mixed_iso(sectors: usize) -> Vec<u8> {
        let mut iso = vec![0u8; sectors * 2048];
        let mut state = 0xDEAD_BEEF_CAFE_1234u64;
        for (i, b) in iso.iter_mut().enumerate() {
            if (i / 4096).is_multiple_of(2) {
                *b = (i / 97) as u8;
            } else {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = state as u8;
            }
        }
        iso
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use test_fixtures::mixed_iso;

    async fn round_trip(allow_zstd: bool, hunk_size: Option<u32>) {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(11);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdDvdOptions {
                hunk_size,
                allow_zstd,
                force: false,
            },
            CancelToken::new(),
        )
        .await
        .unwrap();

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();

        // No extension on the output: the DVD path must derive .iso.
        let out_base = dir.path().join("restored");
        extract_from_chd(&NoProgress, chd_path, out_base.clone(), None)
            .await
            .unwrap();
        let restored = std::fs::read(out_base.with_extension("iso")).unwrap();
        assert_eq!(restored, iso);
    }

    #[tokio::test]
    async fn dvd_chd_round_trips_with_default_codecs() {
        round_trip(false, None).await;
    }

    #[tokio::test]
    async fn dvd_chd_round_trips_with_zstd_and_psp_hunks() {
        round_trip(true, Some(2048)).await;
    }

    #[tokio::test]
    async fn corrupted_dvd_chd_fails_verify_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(16);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        // Flip a byte inside the hunk data region (after the header
        // and metadata, before the trailing map).
        let mut chd = std::fs::read(&chd_path).unwrap();
        let data_start = 124 + 17;
        let mid = data_start + (chd.len() - data_start) / 2;
        chd[mid] ^= 0xFF;
        std::fs::write(&chd_path, &chd).unwrap();

        assert!(
            verify_chd(&NoProgress, chd_path.clone(), None, false)
                .await
                .is_err()
        );
        let out = dir.path().join("restored.iso");
        assert!(
            extract_from_chd(&NoProgress, chd_path, out, None)
                .await
                .is_err()
        );
    }

    /// Cross-checks against real chdman; set ROMCONVERTO_CHDMAN to
    /// the binary path to enable. Covers both directions: chdman
    /// createdvd output (with its huff/flac codec set) must extract
    /// and verify here, and this crate's DVD CHD must pass chdman verify.
    #[tokio::test]
    async fn chdman_dvd_parity() {
        let Some(chdman) = std::env::var_os("ROMCONVERTO_CHDMAN") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(64);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let their_chd = dir.path().join("their.chd");
        let status = std::process::Command::new(&chdman)
            .args(["createdvd", "-i"])
            .arg(&iso_path)
            .arg("-o")
            .arg(&their_chd)
            .status()
            .expect("run chdman createdvd");
        assert!(status.success(), "chdman createdvd failed");

        verify_chd(&NoProgress, their_chd.clone(), None, false)
            .await
            .unwrap();
        let restored = dir.path().join("restored.iso");
        extract_from_chd(&NoProgress, their_chd, restored.clone(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), iso);

        let our_chd = dir.path().join("our.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            our_chd.clone(),
            ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();
        let status = std::process::Command::new(&chdman)
            .args(["verify", "-i"])
            .arg(&our_chd)
            .status()
            .expect("run chdman verify");
        assert!(status.success(), "chdman rejected our DVD CHD");
    }

    use crate::util::iso9660::test_fixtures::{IsoSpec, make_iso};

    fn ps1_iso() -> Vec<u8> {
        make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors: 250_000,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: b"BOOT = cdrom:\\SLUS_000.01;1\r\nTCB = 4\r\n",
        })
    }

    fn ps2_iso(volume_sectors: u32) -> Vec<u8> {
        make_iso(&IsoSpec {
            system_id: b"PLAYSTATION",
            volume_sectors,
            root_entries: &[(b"SYSTEM.CNF;1", false)],
            file_content: b"BOOT2 = cdrom0:\\SLUS_123.45;1\r\nVER = 1.00\r\n",
        })
    }

    fn cd_track_metadata(path: &std::path::Path) -> String {
        let handle = crate::chd::reader::open_chd_sync(path).unwrap();
        let meta = handle
            .metadata
            .iter()
            .find(|m| m.tag == CHD_METADATA_TAG_CD)
            .expect("CHT2 metadata present");
        String::from_utf8_lossy(&meta.data)
            .trim_end_matches('\0')
            .to_string()
    }

    fn has_dvd_tag(path: &std::path::Path) -> bool {
        let handle = crate::chd::reader::open_chd_sync(path).unwrap();
        handle
            .metadata
            .iter()
            .any(|m| m.tag == CHD_METADATA_TAG_DVD)
    }

    async fn auto_route(iso: &[u8], dir: &std::path::Path) -> PathBuf {
        let iso_path = dir.join("game.iso");
        std::fs::write(&iso_path, iso).unwrap();
        let chd_path = dir.join("game.chd");
        convert_disc_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            None,
            ChdDvdOptions::default(),
        )
        .await
        .unwrap();
        chd_path
    }

    #[tokio::test]
    async fn ps1_iso_routes_to_cd_chd_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let iso = ps1_iso();
        let chd_path = auto_route(&iso, dir.path()).await;

        let meta = cd_track_metadata(&chd_path);
        assert!(meta.contains("TYPE:MODE1 "), "metadata: {meta}");
        assert!(meta.contains("FRAMES:20"), "metadata: {meta}");

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(&NoProgress, chd_path, out_cue.clone(), None)
            .await
            .unwrap();
        let cue = std::fs::read_to_string(&out_cue).unwrap();
        assert!(cue.contains("MODE1/2048"), "cue: {cue}");
        assert!(cue.contains("INDEX 01 00:00:00"), "cue: {cue}");
        assert_eq!(std::fs::read(out_cue.with_extension("bin")).unwrap(), iso);
    }

    #[tokio::test]
    async fn ps2cd_iso_routes_to_cd_chd() {
        let dir = tempfile::tempdir().unwrap();
        let chd_path = auto_route(&ps2_iso(300_000), dir.path()).await;
        assert!(cd_track_metadata(&chd_path).contains("TYPE:MODE1 "));
    }

    #[tokio::test]
    async fn dvd_media_and_unknown_isos_route_to_dvd_chd() {
        let dir = tempfile::tempdir().unwrap();
        for (name, iso) in [
            ("ps2dvd", ps2_iso(2_000_000)),
            (
                "psp",
                make_iso(&IsoSpec {
                    system_id: b"PSP GAME",
                    volume_sectors: 800_000,
                    root_entries: &[],
                    file_content: &[],
                }),
            ),
            ("unknown", mixed_iso(11)),
        ] {
            let sub = dir.path().join(name);
            std::fs::create_dir(&sub).unwrap();
            let chd_path = auto_route(&iso, &sub).await;
            assert!(has_dvd_tag(&chd_path), "{name} should be DVD-mode");
        }
    }

    /// 11 sectors is not a 4-frame multiple, so this also exercises
    /// the track padding: FRAMES records 11 while the extracted bin
    /// must drop the 1 padding frame and match the input exactly.
    #[tokio::test]
    async fn forced_cd_mode_on_iso_round_trips_with_padding() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(11);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_disc_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            Some(DiscMode::Cd),
            ChdDvdOptions::default(),
        )
        .await
        .unwrap();

        let meta = cd_track_metadata(&chd_path);
        assert!(meta.contains("FRAMES:11"), "metadata: {meta}");

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(&NoProgress, chd_path, out_cue.clone(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(out_cue.with_extension("bin")).unwrap(), iso);
    }

    /// digest_chd_tracks over a CD-mode CHD must match the extracted
    /// bin: `whole` equals the bin's hash and the single track's digest
    /// equals the same, with track datasize accounting for padding
    /// (the extracted bin drops padding frames, and the per-track
    /// FRAMES count is used as-is).
    #[tokio::test]
    async fn digest_chd_tracks_cd_matches_extracted_bin() {
        use crate::util::hash::{HashAlgo, hash_file};

        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(13);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_cd_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(&NoProgress, chd_path.clone(), out_cue.clone(), None)
            .await
            .unwrap();
        let bin_path = out_cue.with_extension("bin");

        let algos = [HashAlgo::Crc32, HashAlgo::Sha1, HashAlgo::Sha256];
        let bytes_done = Arc::new(AtomicU64::new(0));
        let (tracks, whole) = tokio::task::spawn_blocking({
            let chd_path = chd_path.clone();
            let bytes_done = bytes_done.clone();
            move || digest_chd_tracks(&chd_path, &algos, &bytes_done, &CancelToken::new())
        })
        .await
        .unwrap()
        .unwrap();

        let bin_hash = hash_file(&bin_path, &algos, &NoProgress).unwrap();
        assert_eq!(whole, bin_hash, "whole digest must equal extracted bin");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_number, 1);
        assert_eq!(tracks[0].digests, bin_hash, "single track equals whole bin");
    }

    /// digest_chd_tracks over a DVD-mode CHD returns an empty track
    /// list and the flat ISO digest, matching a hash of the extracted
    /// iso.
    #[tokio::test]
    async fn digest_chd_tracks_dvd_matches_extracted_iso() {
        use crate::util::hash::{HashAlgo, hash_file};

        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(20);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdDvdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_iso = dir.path().join("restored.iso");
        extract_from_chd(&NoProgress, chd_path.clone(), out_iso.clone(), None)
            .await
            .unwrap();

        let algos = [HashAlgo::Sha1, HashAlgo::Md5];
        let bytes_done = Arc::new(AtomicU64::new(0));
        let (tracks, whole) = tokio::task::spawn_blocking({
            let chd_path = chd_path.clone();
            let bytes_done = bytes_done.clone();
            move || digest_chd_tracks(&chd_path, &algos, &bytes_done, &CancelToken::new())
        })
        .await
        .unwrap()
        .unwrap();

        assert!(tracks.is_empty(), "DVD CHD yields no per-track digests");
        let iso_hash = hash_file(&out_iso, &algos, &NoProgress).unwrap();
        assert_eq!(whole, iso_hash, "whole digest must equal extracted iso");
    }

    #[tokio::test]
    async fn dvd_flag_on_cue_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let cue_path = dir.path().join("game.cue");
        std::fs::write(&cue_path, "FILE \"game.bin\" BINARY\n").unwrap();
        let result = convert_disc_to_chd(
            &NoProgress,
            cue_path,
            dir.path().join("game.chd"),
            Some(DiscMode::Dvd),
            ChdDvdOptions::default(),
        )
        .await;
        assert!(matches!(result, Err(ChdError::DvdModeNeedsIso)));
    }

    #[tokio::test]
    async fn unaligned_iso_is_rejected_in_cd_mode() {
        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, vec![0u8; 1000]).unwrap();
        let result = convert_iso_to_cd_chd(
            &NoProgress,
            iso_path,
            dir.path().join("game.chd"),
            false,
            CancelToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ChdError::IsoNotSectorAligned { size: 1000 })
        ));
    }

    #[test]
    fn padded_track_frames_rounds_to_four() {
        assert_eq!(padded_track_frames(10), 12);
        assert_eq!(padded_track_frames(12), 12);
        assert_eq!(padded_track_frames(1), 4);
    }

    #[test]
    fn frame_spans_single_data_track() {
        let tracks = parse_chd_track_metadata("TRACK:1 TYPE:MODE1 FRAMES:20 PREGAP:0").unwrap();
        let (sizes, track) = chd_frame_spans(&tracks);
        assert_eq!(sizes.len(), 20);
        assert_eq!(track.len(), 20);
        assert!(sizes.iter().all(|&s| s == 2048));
        assert!(track.iter().all(|&t| t == 0));
        assert_eq!(chd_track_decoded_size(&tracks[0]), 20 * 2048);
    }

    /// A two-track disc with a nonzero pregap on the audio track: the
    /// `FRAMES:` counts are used as-is (pregap frames stored in the CHD
    /// are inside `FRAMES`), and per-frame routing keys on the frame
    /// index so the differing datasizes (2352 data, 2352 audio) and the
    /// track boundary line up.
    #[test]
    fn frame_spans_multi_track_with_pregap() {
        let meta =
            "TRACK:1 TYPE:MODE1_RAW FRAMES:300 PREGAP:0 TRACK:2 TYPE:AUDIO FRAMES:500 PREGAP:150";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let (sizes, track) = chd_frame_spans(&tracks);

        assert_eq!(sizes.len(), 800);
        assert_eq!(track.len(), 800);
        assert!(sizes[..300].iter().all(|&s| s == 2352));
        assert!(track[..300].iter().all(|&t| t == 0));
        assert!(sizes[300..].iter().all(|&s| s == 2352));
        assert!(track[300..].iter().all(|&t| t == 1));

        assert_eq!(chd_track_decoded_size(&tracks[0]), 300 * 2352);
        assert_eq!(chd_track_decoded_size(&tracks[1]), 500 * 2352);
        let whole: u64 = sizes.iter().map(|&s| s as u64).sum();
        assert_eq!(whole, 800 * 2352);
    }

    #[test]
    fn frame_spans_mixed_datasizes() {
        let meta = "TRACK:1 TYPE:MODE1 FRAMES:10 TRACK:2 TYPE:MODE2_FORM1 FRAMES:5 TRACK:3 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let (sizes, track) = chd_frame_spans(&tracks);
        assert_eq!(sizes.len(), 22);
        assert_eq!(&sizes[0..10], &[2048; 10]);
        assert_eq!(&sizes[10..15], &[2336; 5]);
        assert_eq!(&sizes[15..22], &[2352; 7]);
        assert_eq!(&track[9..12], &[0, 1, 1]);
        assert_eq!(&track[14..16], &[1, 2]);
    }

    /// Cross-checks the CD-iso path against real chdman; set
    /// ROMCONVERTO_CHDMAN to the binary path to enable. The sector
    /// count is deliberately not a 4-frame multiple so the track
    /// padding rule is exercised, and both SHA1s reported by
    /// `chdman info` must match between the two files, proving the
    /// frame layout, padding, and CHT2 metadata are byte-identical.
    #[tokio::test]
    async fn chdman_cd_iso_parity() {
        let Some(chdman) = std::env::var_os("ROMCONVERTO_CHDMAN") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(13);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let their_chd = dir.path().join("their.chd");
        let status = std::process::Command::new(&chdman)
            .args(["createcd", "-i"])
            .arg(&iso_path)
            .arg("-o")
            .arg(&their_chd)
            .status()
            .expect("run chdman createcd");
        assert!(status.success(), "chdman createcd failed");

        verify_chd(&NoProgress, their_chd.clone(), None, false)
            .await
            .unwrap();
        let restored_cue = dir.path().join("restored.cue");
        extract_from_chd(&NoProgress, their_chd.clone(), restored_cue.clone(), None)
            .await
            .unwrap();
        assert!(
            std::fs::read_to_string(&restored_cue)
                .unwrap()
                .contains("MODE1/2048")
        );
        assert_eq!(
            std::fs::read(restored_cue.with_extension("bin")).unwrap(),
            iso
        );

        let our_chd = dir.path().join("our.chd");
        convert_iso_to_cd_chd(
            &NoProgress,
            iso_path,
            our_chd.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let status = std::process::Command::new(&chdman)
            .args(["verify", "-i"])
            .arg(&our_chd)
            .status()
            .expect("run chdman verify");
        assert!(status.success(), "chdman rejected our CD CHD");

        let info_sha1s = |path: &std::path::Path| -> Vec<String> {
            let out = std::process::Command::new(&chdman)
                .args(["info", "-i"])
                .arg(path)
                .output()
                .expect("run chdman info");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.contains("SHA1"))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            info_sha1s(&their_chd),
            info_sha1s(&our_chd),
            "SHA1s must match chdman's output byte-for-byte"
        );
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn collect_files_with_ext_finds_only_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.chd"), b"x").unwrap();
        std::fs::write(dir.path().join("b.CHD"), b"x").unwrap();
        std::fs::write(dir.path().join("c.cue"), b"x").unwrap();
        let found = collect_files_with_ext(dir.path(), "chd", None).unwrap();
        assert_eq!(found.len(), 2);
    }
}
