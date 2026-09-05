//! CHD (Compressed Hunks of Data) compression and extraction for CD and DVD
//! disc images, targeting the same V5 format chdman writes.
//!
//! CD input (`.cue`/`.bin`) keeps its sidecar files, so restoring a CHD back
//! to disc form is called extract rather than decompress; see
//! [`crate::chd::error`] for the failure modes.

use crate::cd::{CD_HUNK_BYTES, FRAME_SIZE, IO_BUFFER_SIZE, SECTOR_SIZE};
use crate::chd::error::{ChdError, ChdResult};
use crate::chd::legacy::LEGACY_COMPRESSION_AV;
use crate::chd::models::{
    CHD_METADATA_FLAG_HASHED, CHD_METADATA_RESERVED_BYTES, CHD_METADATA_TAG_AV,
    CHD_METADATA_TAG_CD, CHD_METADATA_TAG_DVD, CHD_METADATA_TAG_HARD_DISK, ChdHeaderV5,
    ChdMetadataHeader, SHA1_BYTES,
};
use crate::chd::reader::cue_generator::{
    ChdTrackInfo, chd_type_datasize, generate_cue_sheet, parse_chd_track_metadata,
};
use crate::chd::writer::ChdWriter;
use crate::chd::writer::metadata::MetadataHash;
use crate::cue::CueParser;
use crate::cue::models::{CueFile, CueSheet, FileType, Index, Msf, Track, TrackType};
use crate::laserdisc::avi::AviFile;
use crate::util::hash::{FileDigests, HashAlgo, MultiHasher};
use crate::util::iso9660::{DiscKind, detect_disc_kind};
use crate::util::{
    BYTES_PER_MB, CancelToken, DREAMCAST_CHD_WARNING, ProgressReporter, await_with_progress_cancel,
    dreamcast_boot_signature, scratch_output_path,
};
use log::{debug, info, warn};
use sha1::{Digest, Sha1};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// CHD hunk codecs: the compressor set chdman implements and the raw
/// compress/decompress primitives the CD and DVD paths build on.
pub mod compression;
pub use compression::{
    ChdCodec, default_cd_codecs, default_dvd_codecs, deflate_level, lzma_level, parse_codec_list,
    validate_codecs, zstd_level,
};
pub mod error;
pub mod info;
pub(crate) mod legacy;
pub(crate) mod map;
pub(crate) mod models;
pub(crate) mod reader;
pub(crate) mod writer;

/// chdman's `createdvd` default: two 2048-byte sectors per hunk.
pub const DVD_HUNK_BYTES_DEFAULT: u32 = 4096;
/// PPSSPP serves the PSP's 2048-byte block API straight from hunks
/// and warns about anything larger, so detected PSP input defaults
/// to single-sector hunks.
pub const DVD_HUNK_BYTES_PSP: u32 = 2048;

/// Options for CHD creation (CD and DVD modes).
#[derive(Debug, Clone, Default)]
pub struct ChdOptions {
    /// Hunk size override; DVD mode's default is picked per detected
    /// console ([`DVD_HUNK_BYTES_DEFAULT`] / [`DVD_HUNK_BYTES_PSP`]).
    pub hunk_size: Option<u32>,
    /// Codec list for the CHD header's compressor slots. `None` uses
    /// the per-mode chdman default ([`default_cd_codecs`] /
    /// [`default_dvd_codecs`]).
    pub codecs: Option<Vec<ChdCodec>>,
    /// Compression level in `1..=22`. `None` uses each codec's
    /// default level.
    pub level: Option<i32>,
    pub force: bool,
}

/// Validate the codec list and compression level in `opts` against
/// the CHD flavor being written.
fn validate_chd_options(opts: &ChdOptions, dvd: bool) -> ChdResult<()> {
    if let Some(codecs) = &opts.codecs {
        validate_codecs(codecs, dvd)?;
    }
    if let Some(level) = opts.level
        && !(1..=22).contains(&level)
    {
        return Err(ChdError::InvalidCompressionLevel(level));
    }
    Ok(())
}

/// Which CHD flavor to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscMode {
    Cd,
    Dvd,
    /// Laserdisc A/V CHD (`chdman createld`), written from a `.avi` rip.
    Ld,
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
    opts: ChdOptions,
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
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    let is_avi = input_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("avi"));
    match mode {
        Some(DiscMode::Ld) if !is_avi => return Err(ChdError::LdModeNeedsAvi),
        Some(m @ (DiscMode::Cd | DiscMode::Dvd)) if is_avi => {
            return Err(ChdError::AviNeedsLdMode(m));
        }
        _ => {}
    }
    if is_avi {
        info!("LaserDisc AVI detected, writing LD CHD (createld)");
        return convert_avi_to_chd_cancellable(progress, input_path, output_path, opts, cancel)
            .await;
    }

    let is_cue = input_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    match (mode, is_cue) {
        (None | Some(DiscMode::Cd), true) => {
            convert_to_chd(progress, input_path, output_path, opts, cancel).await
        }
        (Some(DiscMode::Dvd), true) => Err(ChdError::DvdModeNeedsIso),
        (Some(DiscMode::Cd), false) => {
            convert_iso_to_cd_chd(progress, input_path, output_path, opts, cancel).await
        }
        (Some(DiscMode::Dvd), false) => {
            convert_iso_to_chd(progress, input_path, output_path, opts, cancel).await
        }
        (Some(DiscMode::Ld), _) => unreachable!("Ld handled above"),
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
                    convert_iso_to_cd_chd(progress, input_path, output_path, opts, cancel).await
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

/// Compress every `.cue`, `.iso`, and `.avi` under `input_dir`, descending
/// into subdirectories up to `max_depth` (`None` for unlimited). Outputs
/// land next to their inputs with the extension replaced by `.chd`, or
/// mirror the source tree under `output_dir` when one is given.
pub async fn convert_disc_to_chd_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
) -> ChdResult<()> {
    let discs =
        crate::util::fs::collect_files_with_exts(input_dir, &["cue", "iso", "avi"], max_depth)?;
    if discs.is_empty() {
        warn!(
            "No .cue, .iso, or .avi inputs found in {}",
            input_dir.display()
        );
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
    opts: ChdOptions,
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
    opts: ChdOptions,
    kind: Option<DiscKind>,
    cancel: CancelToken,
) -> ChdResult<()> {
    validate_chd_options(&opts, true)?;
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

    let write_path = scratch_output_path(&output_path)?;
    let iso_owned = iso_path.clone();
    let write_owned = write_path.to_path_buf();
    let codecs = opts.codecs.clone().unwrap_or_else(default_dvd_codecs);
    let level = opts.level;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let iso_file = std::fs::File::open(&iso_owned)?;
        let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

        let mut writer = ChdWriter::create_dvd(&write_owned, iso_size, hunk_size, codecs, level)?;
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
    crate::util::publish_temp(write_path, &output_path, true)?;

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

pub(crate) fn padded_track_frames(data_sectors: u32) -> u32 {
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
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    validate_chd_options(&opts, false)?;
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
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

    let write_path = scratch_output_path(&output_path)?;
    let iso_owned = iso_path.clone();
    let write_owned = write_path.to_path_buf();
    let codecs = opts.codecs.clone().unwrap_or_else(default_cd_codecs);
    let level = opts.level;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let iso_file = std::fs::File::open(&iso_owned)?;
        let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

        let mut writer = ChdWriter::create(
            &write_owned,
            data_sectors,
            CD_HUNK_BYTES,
            &cue_sheet,
            codecs,
            level,
        )?;
        writer.compress_all_hunks(
            &mut iso_reader,
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
    crate::util::publish_temp(write_path, &output_path, true)?;

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

/// Compress a laserdisc `.avi` rip to an LD-mode CHD, the equivalent of
/// `chdman createld`. The `avhu` codec, per-field hunk size, and field
/// count are all derived from the AVI's own headers, so `opts.codecs`,
/// `opts.level`, and `opts.hunk_size` must be unset.
///
/// # Errors
/// Returns [`ChdError::LdRejectsOverride`] if `opts` sets a codec list,
/// compression level, or hunk size.
pub async fn convert_avi_to_chd_cancellable(
    progress: &dyn ProgressReporter,
    avi_path: PathBuf,
    output_path: PathBuf,
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    if opts.codecs.is_some() {
        return Err(ChdError::LdRejectsOverride { knob: "codecs" });
    }
    if opts.level.is_some() {
        return Err(ChdError::LdRejectsOverride { knob: "level" });
    }
    if opts.hunk_size.is_some() {
        return Err(ChdError::LdRejectsOverride { knob: "hunk-size" });
    }
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    let avi_size = fs::metadata(&avi_path).await?.len();
    let total_mb = avi_size as f64 / BYTES_PER_MB;
    progress.start(
        avi_size,
        &format!("Compressing to CHD (~{:.2} MB)", total_mb),
    );

    let write_path = scratch_output_path(&output_path)?;
    let avi_owned = avi_path.clone();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let mut avi = AviFile::open(&avi_owned)?;
        let params = avi.ld_params()?;

        let mut writer = ChdWriter::create_ld(&write_owned, &params)?;
        writer.compress_all_hunks_ld(&mut avi, &params, &bytes_done_bg, &cancel_bg)?;
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
    crate::util::publish_temp(write_path, &output_path, true)?;

    let chd_size = fs::metadata(&output_path).await?.len();
    let compression_ratio = (chd_size as f64 / avi_size as f64) * 100.0;
    info!(
        "Original: {:.2} MB, CHD: {:.2} MB ({:.1}% compression ratio)",
        total_mb,
        chd_size as f64 / BYTES_PER_MB,
        compression_ratio
    );
    Ok(())
}

/// Read up to 64 KiB from the head of a data track for the Dreamcast
/// IP.BIN sniff. Advisory only: a missing or short file returns an empty
/// buffer rather than propagating the IO error.
async fn dreamcast_head_bytes(bin_path: &std::path::Path) -> Vec<u8> {
    const HEAD_LEN: usize = 0x10000;
    let Ok(mut file) = fs::File::open(bin_path).await else {
        return Vec::new();
    };
    let mut buf = vec![0u8; HEAD_LEN];
    match file.read(&mut buf).await {
        Ok(n) => {
            buf.truncate(n);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// Compresses a CUE/BIN CD image into a V5 CHD file at `output_path`.
///
/// # Errors
/// Returns [`ChdError::ChdFileAlreadyExists`] if the output exists and
/// `opts.force` is unset.
pub async fn convert_to_chd(
    progress: &dyn ProgressReporter,
    cue_path: PathBuf,
    output_path: PathBuf,
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    validate_chd_options(&opts, false)?;
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
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

    if matches!(cue_sheet.files[0].file_type, FileType::Binary)
        && dreamcast_boot_signature(&dreamcast_head_bytes(&bin_path).await)
    {
        progress.warn(DREAMCAST_CHD_WARNING);
    }

    // The single-bin ingest reads uniform 2352-byte raw sectors; any
    // other track width would silently produce a corrupt CHD.
    if let Some(track) = cue_sheet
        .tracks
        .iter()
        .find(|t| t.track_type.block_size() != SECTOR_SIZE as u32)
    {
        return Err(ChdError::UnsupportedCueTrackWidth {
            cue_type: track.track_type.cue_string(),
        });
    }

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
    let write_path = scratch_output_path(&output_path)?;
    let bin_path_owned = bin_path.clone();
    let write_owned = write_path.to_path_buf();
    let cue_sheet_owned = cue_sheet.clone();
    let codecs = opts.codecs.clone().unwrap_or_else(default_cd_codecs);
    let level = opts.level;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        let bin_file = std::fs::File::open(&bin_path_owned)?;
        let mut bin_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, bin_file);

        let mut writer = ChdWriter::create(
            &write_owned,
            total_sectors,
            CD_HUNK_BYTES,
            &cue_sheet_owned,
            codecs,
            level,
        )?;

        writer.compress_all_hunks(&mut bin_reader, SECTOR_SIZE, &bytes_done_bg, &cancel_bg)?;
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
    crate::util::publish_temp(write_path, &output_path, true)?;

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

/// Older text track-metadata tags, checked as fallbacks after `CHT2`.
/// `CHTR` predates `CHT2`'s pregap/postgap fields; `CHGD`/`CHGT` are
/// GD-ROM equivalents. `CHCD` (old CD-ROM TOC) is binary, not text,
/// and is intentionally never matched here.
const CHD_METADATA_TAG_CD_TRACK: [u8; 4] = *b"CHTR";
const CHD_METADATA_TAG_GD_TRACK: [u8; 4] = *b"CHGD";
const CHD_METADATA_TAG_GD_TRACK_LEGACY: [u8; 4] = *b"CHGT";

/// Concatenated text of every metadata entry for the first present
/// track-tag family, in `CHT2`, `CHTR`, `CHGD`, `CHGT` priority order.
/// chdman writes one entry per track, while older rom-converto builds
/// packed every track into a single entry; joining with a space parses
/// both layouts identically. Families are never mixed within one file.
pub(crate) fn cd_track_metadata_text(metadata: &[ChdMetadataHeader]) -> Option<String> {
    const TAG_PRIORITY: [[u8; 4]; 4] = [
        CHD_METADATA_TAG_CD,
        CHD_METADATA_TAG_CD_TRACK,
        CHD_METADATA_TAG_GD_TRACK,
        CHD_METADATA_TAG_GD_TRACK_LEGACY,
    ];
    let tag = TAG_PRIORITY
        .iter()
        .find(|tag| metadata.iter().any(|m| m.tag == **tag))?;
    let parts: Vec<String> = metadata
        .iter()
        .filter(|m| m.tag == *tag)
        .map(|m| {
            String::from_utf8_lossy(&m.data)
                .trim_end_matches('\0')
                .trim()
                .to_string()
        })
        .collect();
    Some(parts.join(" "))
}

/// Whether a CHD's physical stream uses chdman's per-track 4-frame
/// padding. Pre-padding rom-converto builds wrote the frames
/// back-to-back; those files are recognized by a physical frame count
/// (`logical_bytes / FRAME_SIZE`) that matches the raw `FRAMES:` sum
/// and not the padded one. Anything else, chdman output included, is
/// treated as padded.
pub(crate) fn chd_layout_is_padded(tracks: &[ChdTrackInfo], physical_frames: u64) -> bool {
    let unpadded: u64 = tracks.iter().map(|t| t.frames as u64).sum();
    let padded: u64 = tracks
        .iter()
        .map(|t| padded_track_frames(t.frames) as u64)
        .sum();
    physical_frames != unpadded || unpadded == padded
}

/// Per-frame span map for the physical CHD CD stream: `frame_sizes[i]`
/// is the payload width of frame `i` and `frame_track[i]` is the index
/// (into `tracks`) of the track that owns frame `i`. chdman pads every
/// track to a 4-frame boundary, so each track's `FRAMES:` payload
/// frames are followed by padding frames of width 0 that contribute
/// nothing to the output; `padded` is false only for legacy unpadded
/// layouts (see [`chd_layout_is_padded`]). Both vecs are laid out
/// exactly as `extract_hunks` shapes the stream, so hashing frame by
/// frame through them reproduces the bin `chdman extractcd` writes.
/// Pure so it is unit-testable against a synthetic CHT2 metadata
/// string.
pub(crate) fn chd_frame_spans(tracks: &[ChdTrackInfo], padded: bool) -> (Vec<usize>, Vec<usize>) {
    let mut frame_sizes = Vec::new();
    let mut frame_track = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        let physical = if padded {
            padded_track_frames(t.frames)
        } else {
            t.frames
        } as usize;
        let datasize = chd_type_datasize(&t.track_type);
        frame_sizes.extend(std::iter::repeat_n(datasize, t.frames as usize));
        frame_sizes.extend(std::iter::repeat_n(0, physical - t.frames as usize));
        frame_track.extend(std::iter::repeat_n(i, physical));
    }
    (frame_sizes, frame_track)
}

/// Decoded payload byte count of one track: `frames * datasize`. This
/// is the value stored as each track's `FileDigests.size_bytes`.
pub(crate) fn chd_track_decoded_size(track: &ChdTrackInfo) -> u64 {
    track.frames as u64 * chd_type_datasize(&track.track_type) as u64
}

/// Byte-swap the 16-bit samples of one audio sector in place. chdman
/// stores CD audio big-endian and swaps back on extract; the writer
/// and reader apply this to audio-track frames only.
pub(crate) fn swap_audio_sector(sector: &mut [u8]) {
    for pair in sector.as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
}

/// Per-frame audio flags for the physical CD stream, laid out like
/// [`chd_frame_spans`] (per-track padding frames included when
/// `padded`): `true` where the frame's track is AUDIO.
pub(crate) fn chd_frame_audio(tracks: &[ChdTrackInfo], padded: bool) -> Vec<bool> {
    tracks
        .iter()
        .flat_map(|t| {
            let physical = if padded {
                padded_track_frames(t.frames)
            } else {
                t.frames
            } as usize;
            std::iter::repeat_n(t.track_type == "AUDIO", physical)
        })
        .collect()
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

/// Extracts a CHD back to its CD (cue/bin) or DVD (iso) form at `output_path`.
///
/// # Errors
/// Returns [`ChdError::ParentChdNotSupported`] if `parent_path` is set.
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
    // logical_bytes counts the padded physical frames, which the
    // extracted bin drops.
    let input_for_peek = input_path.clone();
    let (header, total_bin_bytes, is_dvd) =
        tokio::task::spawn_blocking(move || -> ChdResult<(ChdHeaderV5, u64, bool)> {
            let handle = crate::chd::reader::open_chd_sync(&input_for_peek)?;
            if handle.metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_AV) {
                return Err(ChdError::LdExtractionUnsupported);
            }
            if handle
                .metadata
                .iter()
                .any(|m| m.tag == CHD_METADATA_TAG_DVD)
            {
                return Ok((handle.header, 0, true));
            }
            let meta_str = cd_track_metadata_text(&handle.metadata).ok_or_else(|| {
                ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string())
            })?;
            let tracks = parse_chd_track_metadata(&meta_str)?;
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
            ChdExtractWork, ChdExtractedOut, HunkExtractArgs, extract_hunks,
            make_chd_extract_workers,
        };
        use crate::util::worker_pool::{Pool, parallelism};

        let handle = open_chd_sync(&input_owned)?;

        let meta_str = cd_track_metadata_text(&handle.metadata)
            .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;
        let tracks = parse_chd_track_metadata(&meta_str)?;

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

/// Peek a CHD's metadata to tell DVD-mode (flat ISO) apart from
/// CD-mode (bin/cue with CHT2 track metadata) without extracting
/// anything. Used by [`crate::pipeline::chd_to_cso_cancellable`] to
/// reject CD-mode CHDs up front, since CSO/ZSO have no track layout.
pub async fn is_dvd_mode_chd(path: PathBuf) -> ChdResult<bool> {
    tokio::task::spawn_blocking(move || -> ChdResult<bool> {
        let handle = crate::chd::reader::open_chd_sync(&path)?;
        if handle.metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_AV) {
            return Err(ChdError::LdExtractionUnsupported);
        }
        Ok(handle
            .metadata
            .iter()
            .any(|m| m.tag == CHD_METADATA_TAG_DVD))
    })
    .await?
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

    let write_path = scratch_output_path(&iso_path)?;
    let input_owned = input_path.clone();
    let write_owned = write_path.to_path_buf();
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
    crate::util::publish_temp(write_path, &iso_path, true)?;

    info!(
        "Extracted: {:.2} MB ISO from {}",
        total_mb,
        input_path.display()
    );
    Ok(())
}

/// Verifies a CHD's hunks against their stored CRC/SHA-1, optionally
/// rewriting the header's hashes when `fix` is set and mismatches are found.
///
/// # Errors
/// Returns [`ChdError::ParentChdNotSupported`] if `parent_path` is set.
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
                handle.header.compressors(),
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
        ChdExtractWork, ChdExtractedOut, TrackDigestArgs, digest_hunks_dvd, digest_hunks_per_track,
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

    let meta_str = cd_track_metadata_text(&handle.metadata)
        .ok_or_else(|| ChdError::InvalidTrackMetadata("no CHT2 metadata found".to_string()))?;
    let tracks = parse_chd_track_metadata(&meta_str)?;

    let padded = chd_layout_is_padded(&tracks, handle.header.logical_bytes / FRAME_SIZE as u64);
    let (frame_sizes, frame_track) = chd_frame_spans(&tracks, padded);
    let frame_audio = chd_frame_audio(&tracks, padded);
    let mut hashers: Vec<MultiHasher> =
        (0..tracks.len()).map(|_| MultiHasher::new(algos)).collect();
    let mut whole = MultiHasher::new(algos);

    let pool: Pool<ChdExtractWork, ChdExtractedOut, ChdError> =
        Pool::spawn(make_chd_extract_workers(
            n_threads,
            &handle.file,
            hunk_bytes,
            handle.header.compressors(),
        )?);
    let result = digest_hunks_per_track(
        &pool,
        TrackDigestArgs {
            map: &handle.map,
            hunk_bytes,
            frame_sizes: &frame_sizes,
            frame_track: &frame_track,
            frame_audio: &frame_audio,
            hashers: &mut hashers,
            whole: &mut whole,
            bytes_done,
            cancel,
        },
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

/// Default destination for a migrated CHD. Input and output share the
/// `.chd` extension, so the derived name gets a `v5` infix to keep the
/// V5 output off its own source.
pub fn migrated_chd_path(input: &std::path::Path) -> PathBuf {
    input.with_extension("v5.chd")
}

/// Rewrites a V1-V4 CHD as a V5 CHD. The decoded raw stream and every
/// metadata entry are copied through unchanged, so the output's raw SHA-1
/// reproduces the source's.
pub async fn migrate_chd_to_v5(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    opts: ChdOptions,
) -> ChdResult<()> {
    migrate_chd_to_v5_cancellable(progress, input_path, output_path, opts, CancelToken::new()).await
}

/// Like [`migrate_chd_to_v5`] but observes `cancel` at every hunk
/// boundary; on cancel the scratch CHD is removed and the destination is
/// left untouched.
pub async fn migrate_chd_to_v5_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    opts: ChdOptions,
    cancel: CancelToken,
) -> ChdResult<()> {
    match legacy::peek_chd_version(&input_path)? {
        Some(5) => return Err(ChdError::ChdAlreadyV5),
        Some(version) if version > 5 => return Err(ChdError::UnsupportedChdVersion),
        // Not a CHD at all: fall through and let the reader report the bad magic.
        _ => {}
    }
    if fs::metadata(&output_path).await.is_ok() && !opts.force {
        return Err(ChdError::ChdFileAlreadyExists);
    }

    let open_path = input_path.clone();
    let source = tokio::task::spawn_blocking(move || legacy::LegacyChd::open(&open_path)).await??;
    // V1/V2 keep their geometry in header fields V5 dropped, so it rides
    // along as the `GDDD` entry chdman writes.
    let geometry = source
        .header()
        .chs
        .as_ref()
        .map(|chs| (chs.cylinders, chs.heads, chs.sectors, chs.sector_bytes));
    let unit_bytes = source.header().unit_bytes;
    let logical_bytes = source.header().logical_bytes;
    let is_dvd = unit_bytes != FRAME_SIZE as u32;
    // A/V hunks are `avhu` frames; the generic slots store them faithfully but
    // far larger than chdman would.
    if source.header().compression == LEGACY_COMPRESSION_AV {
        warn!(
            "{} is an A/V CHD; the V5 output will not be avhuff-compressed and will be much larger",
            input_path.display()
        );
    }
    validate_chd_options(&opts, is_dvd)?;

    // Re-hunking would rewrite the map for no gain, so the source hunk
    // size carries over unless the caller overrides it.
    let hunk_bytes = match opts.hunk_size {
        Some(size) if size == 0 || !size.is_multiple_of(unit_bytes) => {
            return Err(ChdError::InvalidHunkSize);
        }
        Some(size) => size,
        None => source.header().hunk_bytes,
    };
    let codecs = opts.codecs.clone().unwrap_or_else(|| {
        if is_dvd {
            default_dvd_codecs()
        } else {
            default_cd_codecs()
        }
    });

    let total_mb = logical_bytes as f64 / BYTES_PER_MB;
    progress.start(
        logical_bytes,
        &format!("Migrating to CHD V5 (~{:.2} MB)", total_mb),
    );

    let write_path = scratch_output_path(&output_path)?;
    let write_owned = write_path.to_path_buf();
    let level = opts.level;
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> ChdResult<()> {
        // Copied out field by field before `into_raw_reader` consumes the
        // source; ChdMetadataHeader is not Clone.
        let mut metadata: Vec<ChdMetadataHeader> = source
            .metadata()
            .iter()
            .map(|entry| ChdMetadataHeader {
                tag: entry.tag,
                flags: entry.flags,
                reserved: entry.reserved,
                data: entry.data.clone(),
            })
            .collect();
        if let Some((cylinders, heads, sectors, sector_bytes)) = geometry {
            let mut data =
                format!("CYLS:{cylinders},HEADS:{heads},SECS:{sectors},BPS:{sector_bytes}")
                    .into_bytes();
            data.push(0);
            metadata.push(ChdMetadataHeader {
                tag: CHD_METADATA_TAG_HARD_DISK,
                flags: CHD_METADATA_FLAG_HASHED,
                reserved: [0; CHD_METADATA_RESERVED_BYTES],
                data,
            });
        }
        // chdman's `copy` rewrites the pre-CHT2 `CHTR` entries as hashed
        // CHT2, so the migrated disc hashes like a current createcd and
        // matches MAME's checksums. `CHGD` stays verbatim: its upgrade
        // also byte-swaps the audio frames.
        if metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_CD_TRACK)
            && !metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_CD)
        {
            let text = cd_track_metadata_text(&metadata).unwrap_or_default();
            let tracks = parse_chd_track_metadata(&text)?;
            metadata.retain(|m| m.tag != CHD_METADATA_TAG_CD_TRACK);
            metadata.extend(tracks.iter().map(|t| {
                ChdMetadataHeader::new_cd_metadata(format!(
                    "TRACK:{} TYPE:{} SUBTYPE:{} FRAMES:{} PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0",
                    t.track_number,
                    t.track_type,
                    t.subtype.as_deref().unwrap_or("NONE"),
                    t.frames
                ))
            }));
        }
        let mut writer = ChdWriter::create_raw(
            &write_owned,
            logical_bytes,
            hunk_bytes,
            unit_bytes,
            metadata,
            codecs,
            level,
        )?;
        let mut reader = source.into_raw_reader();
        writer.compress_all_hunks_raw(&mut reader, &bytes_done_bg, &cancel_bg)?;
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
    crate::util::publish_temp(write_path, &output_path, true)?;

    let chd_size = fs::metadata(&output_path).await?.len();
    info!(
        "Migrated to CHD V5: {:.2} MB raw, {:.2} MB written",
        total_mb,
        chd_size as f64 / BYTES_PER_MB
    );
    Ok(())
}

/// Batch twin of [`migrate_chd_to_v5`], mirroring
/// [`convert_disc_to_chd_batch`]: every `.chd` under `input_dir` that is
/// not already V5 is migrated.
pub async fn migrate_chd_to_v5_batch(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &std::path::Path,
    opts: ChdOptions,
    output_dir: Option<&std::path::Path>,
    max_depth: Option<usize>,
    in_place: bool,
) -> ChdResult<()> {
    let chds = crate::util::fs::collect_files_with_exts(input_dir, &["chd"], max_depth)?;
    if chds.is_empty() {
        warn!("No .chd inputs found in {}", input_dir.display());
        return Ok(());
    }

    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }

    total_progress.start(
        chds.len() as u64,
        &format!("Migrating {} chd files", chds.len()),
    );

    for path in chds {
        match legacy::peek_chd_version(&path) {
            Ok(Some(5)) => {
                warn!("Skipping {}: already CHD V5", path.display());
                total_progress.inc(1);
                continue;
            }
            Err(err) => {
                warn!("Skipping {}: {err}", path.display());
                total_progress.inc(1);
                continue;
            }
            _ => {}
        }
        let output = match (in_place, output_dir) {
            (true, _) => path.clone(),
            (false, Some(_)) => crate::util::place_in_dir_mirrored(&path, input_dir, output_dir),
            (false, None) => migrated_chd_path(&path),
        };
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = migrate_chd_to_v5(progress, path.clone(), output, opts.clone()).await {
            warn!("Failed to migrate {}: {err}", path.display());
        }
        total_progress.inc(1);
    }

    total_progress.finish();
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
            ChdOptions {
                hunk_size,
                codecs: allow_zstd.then(|| vec![ChdCodec::Zstd]),
                level: None,
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

    /// `ChdOptions.codecs = None` must resolve to chdman's `createdvd`
    /// default pack, filling the header slots in that exact order.
    #[tokio::test]
    async fn dvd_chd_default_codecs_match_chdman_slots() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(8);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let header = crate::chd::reader::open_chd_sync(&chd_path).unwrap().header;
        assert_eq!(
            header.compressors(),
            [*b"lzma", *b"zlib", *b"huff", *b"flac"]
        );
    }

    /// `ChdOptions.codecs = None` must resolve to chdman's `createcd`
    /// default pack, filling the header slots in that exact order.
    #[tokio::test]
    async fn cd_chd_default_codecs_match_chdman_slots() {
        let dir = tempfile::tempdir().unwrap();
        let iso = mixed_iso(8);
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, &iso).unwrap();
        let chd_path = dir.path().join("game.chd");
        convert_iso_to_cd_chd(
            &NoProgress,
            iso_path,
            chd_path.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let header = crate::chd::reader::open_chd_sync(&chd_path).unwrap().header;
        assert_eq!(
            header.compressors(),
            [*b"cdlz", *b"cdzl", *b"cdfl", [0u8; 4]]
        );
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
            ChdOptions::default(),
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
            ChdOptions::default(),
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
        cd_track_metadata_text(&handle.metadata).expect("CHT2 metadata present")
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
            ChdOptions::default(),
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
            ChdOptions::default(),
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
            ChdOptions::default(),
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
            ChdOptions::default(),
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
            ChdOptions::default(),
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
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ChdError::IsoNotSectorAligned { size: 1000 })
        ));
    }

    #[tokio::test]
    async fn dreamcast_head_sniff_hits_on_ip_bin_magic() {
        let dir = tempfile::tempdir().unwrap();
        let cue_path = dir.path().join("game.cue");
        std::fs::write(
            &cue_path,
            "FILE \"track01.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();

        let mut bin = vec![0u8; 0x1000];
        bin[0x10..0x10 + "SEGA SEGAKATANA".len()].copy_from_slice(b"SEGA SEGAKATANA");
        std::fs::write(dir.path().join("track01.bin"), &bin).unwrap();

        let cue_sheet = CueParser::new(&cue_path).parse().await.unwrap();
        assert!(matches!(cue_sheet.files[0].file_type, FileType::Binary));
        let bin_path = dir.path().join(&cue_sheet.files[0].filename);
        let head = dreamcast_head_bytes(&bin_path).await;
        assert!(dreamcast_boot_signature(&head));
    }

    #[tokio::test]
    async fn dreamcast_head_sniff_misses_on_plain_iso_track() {
        let dir = tempfile::tempdir().unwrap();
        let bin_path = dir.path().join("track01.bin");
        std::fs::write(&bin_path, vec![0u8; 0x1000]).unwrap();
        let head = dreamcast_head_bytes(&bin_path).await;
        assert!(!dreamcast_boot_signature(&head));
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
        let (sizes, track) = chd_frame_spans(&tracks, true);
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
        let (sizes, track) = chd_frame_spans(&tracks, true);

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

    /// Track frame counts that are not 4-frame multiples: each track's
    /// payload frames are followed by width-0 padding frames (10 -> 12,
    /// 5 -> 8, 7 -> 8 physical frames), matching chdman's layout.
    #[test]
    fn frame_spans_mixed_datasizes() {
        let meta = "TRACK:1 TYPE:MODE1 FRAMES:10 TRACK:2 TYPE:MODE2_FORM1 FRAMES:5 TRACK:3 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        let (sizes, track) = chd_frame_spans(&tracks, true);
        assert_eq!(sizes.len(), 28);
        assert_eq!(&sizes[0..10], &[2048; 10]);
        assert_eq!(&sizes[10..12], &[0; 2]);
        assert_eq!(&sizes[12..17], &[2048; 5]);
        assert_eq!(&sizes[17..20], &[0; 3]);
        assert_eq!(&sizes[20..27], &[2352; 7]);
        assert_eq!(sizes[27], 0);
        assert_eq!(&track[9..13], &[0, 0, 0, 1]);
        assert_eq!(&track[19..21], &[1, 2]);

        let audio = chd_frame_audio(&tracks, true);
        assert_eq!(audio.len(), 28);
        assert!(!audio[19]);
        assert!(audio[20..28].iter().all(|&a| a));
    }

    /// Pre-padding rom-converto builds wrote multi-track streams
    /// unpadded; when the physical frame count matches the raw
    /// `FRAMES:` sum the reader must not inject padding.
    #[test]
    fn frame_spans_legacy_unpadded_layout() {
        let meta = "TRACK:1 TYPE:MODE1_RAW FRAMES:10 TRACK:2 TYPE:AUDIO FRAMES:7";
        let tracks = parse_chd_track_metadata(meta).unwrap();
        assert!(!chd_layout_is_padded(&tracks, 17));
        assert!(chd_layout_is_padded(&tracks, 20));

        let (sizes, track) = chd_frame_spans(&tracks, false);
        assert_eq!(sizes.len(), 17);
        assert!(sizes.iter().all(|&s| s == 2352));
        assert_eq!(&track[9..11], &[0, 1]);
        let audio = chd_frame_audio(&tracks, false);
        assert_eq!(audio.len(), 17);
        assert!(!audio[9]);
        assert!(audio[10]);
    }

    /// Non-2352 cue tracks would corrupt the uniform raw-sector
    /// ingest, so they must be refused instead of converted.
    #[tokio::test]
    async fn convert_to_chd_rejects_non_raw_cue_tracks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("game.bin"), vec![0u8; 2048 * 4]).unwrap();
        let cue_path = dir.path().join("game.cue");
        std::fs::write(
            &cue_path,
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        let err = convert_to_chd(
            &NoProgress,
            cue_path,
            dir.path().join("game.chd"),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChdError::UnsupportedCueTrackWidth { .. }));
    }

    /// 10-frame MODE1/2352 data track + 7-frame AUDIO track; neither
    /// count is a 4-frame multiple, so both need interior padding.
    fn write_two_track_cue(dir: &std::path::Path) -> (PathBuf, Vec<u8>) {
        let mut bin = vec![0u8; 17 * 2352];
        let mut state = 0x0123_4567_89AB_CDEFu64;
        for b in bin.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = state as u8;
        }
        let bin_path = dir.join("game.bin");
        std::fs::write(&bin_path, &bin).unwrap();
        let cue_path = dir.join("game.cue");
        std::fs::write(
            &cue_path,
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:00:10\n",
        )
        .unwrap();
        (cue_path, bin)
    }

    /// Multi-track cue/bin with track frame counts that are not 4-frame
    /// multiples: the writer must pad each track to a 4-frame boundary
    /// like `chdman createcd` (10 -> 12, 7 -> 8 frames), and extraction
    /// must drop the interior padding to restore the original bin
    /// byte-for-byte.
    #[tokio::test]
    async fn multi_track_cue_round_trips_with_padding() {
        let dir = tempfile::tempdir().unwrap();
        let (cue_path, bin) = write_two_track_cue(dir.path());

        let chd_path = dir.path().join("game.chd");
        convert_to_chd(
            &NoProgress,
            cue_path,
            chd_path.clone(),
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap();

        let meta = cd_track_metadata(&chd_path);
        assert!(meta.contains("FRAMES:10"), "metadata: {meta}");
        assert!(meta.contains("FRAMES:7"), "metadata: {meta}");
        {
            let handle = crate::chd::reader::open_chd_sync(&chd_path).unwrap();
            assert_eq!(handle.header.logical_bytes, 20 * 2448);
        }

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(&NoProgress, chd_path, out_cue.clone(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(out_cue.with_extension("bin")).unwrap(), bin);
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
            ChdOptions::default(),
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

    /// Cue/bin twin of [`chdman_cd_iso_parity`]: a multi-track cue
    /// whose track frame counts are not 4-frame multiples, so the
    /// interior track padding and the audio byte swap are both
    /// exercised. Both SHA1s reported by `chdman info` must match
    /// between the two files, and extracting chdman's own CHD must
    /// restore the original bin.
    #[tokio::test]
    async fn chdman_cd_cue_parity() {
        let Some(chdman) = std::env::var_os("ROMCONVERTO_CHDMAN") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (cue_path, bin) = write_two_track_cue(dir.path());

        let their_chd = dir.path().join("their.chd");
        let status = std::process::Command::new(&chdman)
            .args(["createcd", "-i"])
            .arg(&cue_path)
            .arg("-o")
            .arg(&their_chd)
            .status()
            .expect("run chdman createcd");
        assert!(status.success(), "chdman createcd failed");

        let restored_cue = dir.path().join("restored.cue");
        extract_from_chd(&NoProgress, their_chd.clone(), restored_cue.clone(), None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(restored_cue.with_extension("bin")).unwrap(),
            bin
        );

        let our_chd = dir.path().join("our.chd");
        convert_to_chd(
            &NoProgress,
            cue_path,
            our_chd.clone(),
            ChdOptions::default(),
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

    use crate::laserdisc::avi::test_fixtures::{
        AviSpec, build_avi, pattern_frame, pattern_samples,
    };

    /// Small non-interlaced synthetic laserdisc AVI: fast to compress
    /// while still exercising real avhuff encode/decode.
    fn ld_avi() -> Vec<u8> {
        let frames: Vec<Vec<u8>> = (0..2).map(|i| pattern_frame(64, 48, i as u8)).collect();
        let samples = pattern_samples(4000, 1);
        build_avi(&AviSpec {
            width: 64,
            height: 48,
            timescale: 30000,
            sampletime: 1001,
            video_format: *b"YUY2",
            frames: &frames,
            channels: 1,
            sample_rate: 48_000,
            sample_bits: 16,
            samples: &samples,
            index: true,
            block_align_override: None,
            video_length_override: None,
        })
    }

    fn has_av_tag(path: &std::path::Path) -> bool {
        let handle = crate::chd::reader::open_chd_sync(path).unwrap();
        handle.metadata.iter().any(|m| m.tag == CHD_METADATA_TAG_AV)
    }

    #[tokio::test]
    async fn avi_auto_routes_to_ld_chd() {
        let dir = tempfile::tempdir().unwrap();
        let avi_path = dir.path().join("game.avi");
        std::fs::write(&avi_path, ld_avi()).unwrap();
        let chd_path = dir.path().join("game.chd");

        convert_disc_to_chd(
            &NoProgress,
            avi_path,
            chd_path.clone(),
            None,
            ChdOptions::default(),
        )
        .await
        .unwrap();

        let header = crate::chd::reader::open_chd_sync(&chd_path).unwrap().header;
        assert_eq!(header.compressor_0, *b"avhu");
        assert!(has_av_tag(&chd_path));
    }

    #[tokio::test]
    async fn avi_with_compressed_video_errors_naming_the_fourcc() {
        let dir = tempfile::tempdir().unwrap();
        let avi_path = dir.path().join("game.avi");
        let frames: Vec<Vec<u8>> = (0..2).map(|i| pattern_frame(64, 48, i as u8)).collect();
        let samples = pattern_samples(4000, 1);
        let data = build_avi(&AviSpec {
            width: 64,
            height: 48,
            timescale: 30000,
            sampletime: 1001,
            video_format: *b"HFYU",
            frames: &frames,
            channels: 1,
            sample_rate: 48_000,
            sample_bits: 16,
            samples: &samples,
            index: true,
            block_align_override: None,
            video_length_override: None,
        });
        std::fs::write(&avi_path, data).unwrap();
        let chd_path = dir.path().join("game.chd");

        let err = convert_avi_to_chd_cancellable(
            &NoProgress,
            avi_path,
            chd_path,
            ChdOptions::default(),
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("HFYU"), "{err}");
    }

    #[tokio::test]
    async fn ld_mode_on_iso_errors() {
        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("game.iso");
        std::fs::write(&iso_path, mixed_iso(4)).unwrap();
        let chd_path = dir.path().join("game.chd");

        let err = convert_disc_to_chd(
            &NoProgress,
            iso_path,
            chd_path,
            Some(DiscMode::Ld),
            ChdOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChdError::LdModeNeedsAvi));
    }

    #[tokio::test]
    async fn dvd_mode_on_avi_errors() {
        let dir = tempfile::tempdir().unwrap();
        let avi_path = dir.path().join("game.avi");
        std::fs::write(&avi_path, ld_avi()).unwrap();
        let chd_path = dir.path().join("game.chd");

        let err = convert_disc_to_chd(
            &NoProgress,
            avi_path,
            chd_path,
            Some(DiscMode::Dvd),
            ChdOptions::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ChdError::AviNeedsLdMode(DiscMode::Dvd)));
    }

    #[tokio::test]
    async fn ld_rejects_option_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let avi_path = dir.path().join("game.avi");
        std::fs::write(&avi_path, ld_avi()).unwrap();

        for opts in [
            ChdOptions {
                codecs: Some(vec![ChdCodec::Zstd]),
                ..Default::default()
            },
            ChdOptions {
                level: Some(5),
                ..Default::default()
            },
            ChdOptions {
                hunk_size: Some(4096),
                ..Default::default()
            },
        ] {
            let chd_path = dir.path().join("game.chd");
            let err = convert_disc_to_chd(&NoProgress, avi_path.clone(), chd_path, None, opts)
                .await
                .unwrap_err();
            assert!(matches!(err, ChdError::LdRejectsOverride { .. }));
        }
    }

    #[tokio::test]
    async fn ld_chd_verify_passes_then_fails_after_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let avi_path = dir.path().join("game.avi");
        std::fs::write(&avi_path, ld_avi()).unwrap();
        let chd_path = dir.path().join("game.chd");

        convert_disc_to_chd(
            &NoProgress,
            avi_path,
            chd_path.clone(),
            None,
            ChdOptions::default(),
        )
        .await
        .unwrap();

        verify_chd(&NoProgress, chd_path.clone(), None, false)
            .await
            .unwrap();

        let mut chd = std::fs::read(&chd_path).unwrap();
        let data_start = 124 + 17;
        let mid = data_start + (chd.len() - data_start) / 2;
        chd[mid] ^= 0xFF;
        std::fs::write(&chd_path, &chd).unwrap();

        assert!(
            verify_chd(&NoProgress, chd_path, None, false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn ld_chd_extraction_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let avi_path = dir.path().join("game.avi");
        std::fs::write(&avi_path, ld_avi()).unwrap();
        let chd_path = dir.path().join("game.chd");

        convert_disc_to_chd(
            &NoProgress,
            avi_path,
            chd_path.clone(),
            None,
            ChdOptions::default(),
        )
        .await
        .unwrap();

        let out = dir.path().join("restored");
        let err = extract_from_chd(&NoProgress, chd_path.clone(), out, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ChdError::LdExtractionUnsupported));

        let err = is_dvd_mode_chd(chd_path).await.unwrap_err();
        assert!(matches!(err, ChdError::LdExtractionUnsupported));
    }

    fn metadata_header(tag: [u8; 4], text: &str) -> ChdMetadataHeader {
        let mut data = text.as_bytes().to_vec();
        data.push(0);
        ChdMetadataHeader {
            tag,
            flags: crate::chd::models::CHD_METADATA_FLAG_HASHED,
            reserved: [0; crate::chd::models::CHD_METADATA_RESERVED_BYTES],
            data,
        }
    }

    #[test]
    fn cd_track_metadata_text_prefers_cht2_over_chtr() {
        let metadata = vec![
            metadata_header(
                CHD_METADATA_TAG_CD_TRACK,
                "TRACK:1 TYPE:MODE1_RAW FRAMES:300",
            ),
            metadata_header(CHD_METADATA_TAG_CD, "TRACK:1 TYPE:AUDIO FRAMES:150"),
        ];
        let text = cd_track_metadata_text(&metadata).expect("CHT2 present");
        assert_eq!(text, "TRACK:1 TYPE:AUDIO FRAMES:150");
    }

    #[test]
    fn cd_track_metadata_text_none_for_chcd_only() {
        let metadata = vec![metadata_header(*b"CHCD", "binary toc, not text")];
        assert!(cd_track_metadata_text(&metadata).is_none());
    }

    /// Hand-built V4 CHD holding `raw` as uncompressed `hunk_bytes` hunks,
    /// with `metadata` chained after the map and the header hashes filled
    /// in the way chdman would. An empty list leaves the chain absent, as
    /// chdman's `createraw` does.
    fn v4_raw_image(raw: &[u8], hunk_bytes: usize, metadata: &[ChdMetadataHeader]) -> Vec<u8> {
        const V4_HEADER_BYTES: usize = 108;

        let hunks = raw.len() / hunk_bytes;
        let raw_sha1: [u8; SHA1_BYTES] = Sha1::digest(raw).into();
        let hashes: Vec<MetadataHash> = metadata
            .iter()
            .filter(|m| m.flags & CHD_METADATA_FLAG_HASHED != 0)
            .map(|m| MetadataHash {
                tag: m.tag,
                sha1: Sha1::digest(&m.data).into(),
            })
            .collect();

        let mut image = vec![0u8; V4_HEADER_BYTES + hunks * 16];
        image[0..8].copy_from_slice(b"MComprHD");
        image[8..12].copy_from_slice(&(V4_HEADER_BYTES as u32).to_be_bytes());
        image[12..16].copy_from_slice(&4u32.to_be_bytes());
        // zlib, though every map entry below stores its hunk uncompressed.
        image[20..24].copy_from_slice(&1u32.to_be_bytes());
        image[24..28].copy_from_slice(&(hunks as u32).to_be_bytes());
        image[28..36].copy_from_slice(&(raw.len() as u64).to_be_bytes());
        image[44..48].copy_from_slice(&(hunk_bytes as u32).to_be_bytes());
        image[48..68].copy_from_slice(&compute_overall_sha1(raw_sha1, &hashes));
        image[88..108].copy_from_slice(&raw_sha1);

        if !metadata.is_empty() {
            let meta_offset = image.len() as u64;
            image[36..44].copy_from_slice(&meta_offset.to_be_bytes());
        }
        for (i, m) in metadata.iter().enumerate() {
            // Tag, flags, 24-bit length, next pointer (zero on the last).
            image.extend_from_slice(&m.tag);
            image.push(m.flags);
            image.extend_from_slice(&(m.data.len() as u32).to_be_bytes()[1..]);
            let next = if i + 1 == metadata.len() {
                0
            } else {
                image.len() as u64 + 8 + m.data.len() as u64
            };
            image.extend_from_slice(&next.to_be_bytes());
            image.extend_from_slice(&m.data);
        }

        let data_offset = image.len();
        for hunk in 0..hunks {
            let entry = V4_HEADER_BYTES + hunk * 16;
            image[entry..entry + 8]
                .copy_from_slice(&((data_offset + hunk * hunk_bytes) as u64).to_be_bytes());
            image[entry + 12..entry + 14].copy_from_slice(&(hunk_bytes as u16).to_be_bytes());
            // UNCOMPRESSED, CRC check suppressed.
            image[entry + 15] = 0x12;
        }
        image.extend_from_slice(raw);
        image
    }

    async fn migrate_image(image: &[u8]) -> crate::chd::reader::SyncChdHandle {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("legacy.chd");
        std::fs::write(&src, image).unwrap();
        let out = dir.path().join("migrated.chd");

        migrate_chd_to_v5(&NoProgress, src, out.clone(), ChdOptions::default())
            .await
            .unwrap();
        verify_chd(&NoProgress, out.clone(), None, false)
            .await
            .unwrap();
        crate::chd::reader::open_chd_sync(&out).unwrap()
    }

    /// A V4 header stores the SHA-1 of the decoded raw data and the
    /// chained overall digest, so a migration that copies the stream and
    /// the metadata through byte for byte must reproduce both.
    #[tokio::test]
    async fn migrate_v4_chd_preserves_header_hashes() {
        let raw = mixed_iso(6);
        let image = v4_raw_image(&raw, 4096, &[ChdMetadataHeader::new_dvd_metadata()]);

        let handle = migrate_image(&image).await;
        assert_eq!(handle.header.raw_sha1, image[88..108]);
        assert_eq!(handle.header.sha1, image[48..68]);
        assert_eq!(handle.header.logical_bytes, raw.len() as u64);
        assert!(
            handle
                .metadata
                .iter()
                .any(|entry| entry.tag == CHD_METADATA_TAG_DVD)
        );
    }

    /// chdman's `createraw` writes no metadata at all. The migrated V5
    /// must then carry a zero metadata offset, or readers walk the first
    /// hunk as a metadata entry and the overall SHA-1 stops verifying.
    #[tokio::test]
    async fn migrate_metadata_less_chd_leaves_meta_offset_zero() {
        let raw = mixed_iso(6);
        let image = v4_raw_image(&raw, 4096, &[]);

        let handle = migrate_image(&image).await;
        assert_eq!(handle.header.meta_offset, 0);
        assert!(handle.metadata.is_empty());
        assert_eq!(handle.header.sha1, image[48..68]);
    }

    /// Pre-2009 CD CHDs carry unhashed `CHTR` track entries. chdman's
    /// `copy` rewrites them as hashed CHT2 with the pregap fields at their
    /// defaults, and the migration has to do the same so the overall
    /// SHA-1 lands on the value current chdman builds produce.
    #[tokio::test]
    async fn migrate_upgrades_chtr_track_metadata_to_cht2() {
        // Two legacy CD hunks of four 2448-byte frames each.
        let raw: Vec<u8> = (0..2 * CD_HUNK_BYTES as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let chtr = ChdMetadataHeader {
            tag: CHD_METADATA_TAG_CD_TRACK,
            flags: 0,
            reserved: [0; CHD_METADATA_RESERVED_BYTES],
            data: b"TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:8 ".to_vec(),
        };
        let image = v4_raw_image(&raw, CD_HUNK_BYTES as usize, &[chtr]);

        let handle = migrate_image(&image).await;
        let cht2 = ChdMetadataHeader::new_cd_metadata(
            "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:8 PREGAP:0 PGTYPE:MODE1 PGSUB:NONE POSTGAP:0"
                .to_string(),
        );
        assert_eq!(handle.metadata.len(), 1);
        assert_eq!(handle.metadata[0].tag, CHD_METADATA_TAG_CD);
        assert_eq!(handle.metadata[0].flags, CHD_METADATA_FLAG_HASHED);
        assert_eq!(handle.metadata[0].data, cht2.data);
        let expected = compute_overall_sha1(
            handle.header.raw_sha1,
            &[MetadataHash {
                tag: cht2.tag,
                sha1: Sha1::digest(&cht2.data).into(),
            }],
        );
        assert_eq!(handle.header.sha1, expected);
    }

    /// V1 keeps its geometry in header fields V5 does not have, so migrating
    /// has to re-express it as the `GDDD` entry or the image stops being
    /// recognizable as a hard disk.
    #[tokio::test]
    async fn migrate_v1_chd_carries_geometry_into_gddd() {
        const V1_HEADER_BYTES: usize = 76;
        const SECTOR_BYTES: u32 = 512;
        const HUNK_SECTORS: u32 = 8;
        const CYLINDERS: u32 = 2;
        const HEADS: u32 = 1;
        const SECTORS: u32 = 8;

        let hunk_bytes = (SECTOR_BYTES * HUNK_SECTORS) as usize;
        let raw = mixed_iso(4)[..hunk_bytes * 2].to_vec();
        let hunks = raw.len() / hunk_bytes;
        let data_offset = V1_HEADER_BYTES + hunks * 8;

        let mut image = vec![0u8; data_offset];
        image[0..8].copy_from_slice(b"MComprHD");
        image[8..12].copy_from_slice(&(V1_HEADER_BYTES as u32).to_be_bytes());
        image[12..16].copy_from_slice(&1u32.to_be_bytes());
        image[20..24].copy_from_slice(&1u32.to_be_bytes());
        image[24..28].copy_from_slice(&HUNK_SECTORS.to_be_bytes());
        image[28..32].copy_from_slice(&(hunks as u32).to_be_bytes());
        image[32..36].copy_from_slice(&CYLINDERS.to_be_bytes());
        image[36..40].copy_from_slice(&HEADS.to_be_bytes());
        image[40..44].copy_from_slice(&SECTORS.to_be_bytes());

        // V1 entries pack the length in the top 20 bits; length == hunk_bytes
        // marks the hunk uncompressed.
        for hunk in 0..hunks {
            let offset = data_offset + hunk * hunk_bytes;
            let packed = ((hunk_bytes as u64) << 44) | offset as u64;
            let base = V1_HEADER_BYTES + hunk * 8;
            image[base..base + 8].copy_from_slice(&packed.to_be_bytes());
        }
        image.extend_from_slice(&raw);

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("legacy.chd");
        std::fs::write(&src, &image).unwrap();
        let out = dir.path().join("migrated.chd");

        migrate_chd_to_v5(&NoProgress, src, out.clone(), ChdOptions::default())
            .await
            .unwrap();

        let handle = crate::chd::reader::open_chd_sync(&out).unwrap();
        assert_eq!(handle.header.unit_bytes, SECTOR_BYTES);
        assert_eq!(handle.header.logical_bytes, raw.len() as u64);
        let gddd = handle
            .metadata
            .iter()
            .find(|entry| entry.tag == CHD_METADATA_TAG_HARD_DISK)
            .expect("geometry carried over");
        assert_eq!(
            String::from_utf8_lossy(&gddd.data).trim_end_matches('\0'),
            "CYLS:2,HEADS:1,SECS:8,BPS:512"
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
