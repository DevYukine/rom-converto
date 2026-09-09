//! Disc image to CHD compression: the DVD (`createdvd`), CD
//! (`createcd`), and laserdisc (`createld`) writer entry points.

use crate::disc::cd::{CD_HUNK_BYTES, IO_BUFFER_SIZE, SECTOR_SIZE};
use crate::disc::chd::error::{ChdError, ChdResult};
use crate::disc::chd::writer::ChdWriter;
use crate::disc::cue::CueParser;
use crate::disc::cue::models::{CueFile, CueSheet, FileType, Index, Msf, Track, TrackType};
use crate::disc::laserdisc::avi::AviFile;
use crate::util::iso9660::{DiscKind, detect_disc_kind};
use crate::util::{
    BYTES_PER_MB, CancelToken, DREAMCAST_CHD_WARNING, ProgressReporter, dreamcast_boot_signature,
    run_scratch_write,
};
use log::{debug, info, warn};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncReadExt;

use super::*;

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
pub(crate) async fn convert_iso_to_chd_with_kind(
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

    let iso_owned = iso_path.clone();
    let codecs = opts.codecs.clone().unwrap_or_else(default_dvd_codecs);
    let level = opts.level;
    run_scratch_write(
        &output_path,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| -> ChdResult<()> {
            let iso_file = std::fs::File::open(&iso_owned)?;
            let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

            let mut writer =
                ChdWriter::create_dvd(&write_path, iso_size, hunk_size, codecs, level)?;
            writer.compress_all_hunks_dvd(&mut iso_reader, &bytes_done, &cancel)?;
            writer.finalize()?;
            Ok(())
        },
    )
    .await?;

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

    let iso_owned = iso_path.clone();
    let codecs = opts.codecs.clone().unwrap_or_else(default_cd_codecs);
    let level = opts.level;
    run_scratch_write(
        &output_path,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| -> ChdResult<()> {
            let iso_file = std::fs::File::open(&iso_owned)?;
            let mut iso_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, iso_file);

            let mut writer = ChdWriter::create(
                &write_path,
                data_sectors,
                CD_HUNK_BYTES,
                &cue_sheet,
                codecs,
                level,
            )?;
            writer.compress_all_hunks(
                &mut iso_reader,
                sector_data_size as usize,
                &bytes_done,
                &cancel,
            )?;
            writer.finalize()?;
            Ok(())
        },
    )
    .await?;

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
pub async fn convert_avi_to_chd(
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

    let avi_owned = avi_path.clone();
    run_scratch_write(
        &output_path,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| -> ChdResult<()> {
            let mut avi = AviFile::open(&avi_owned)?;
            let params = avi.ld_params()?;

            let mut writer = ChdWriter::create_ld(&write_path, &params)?;
            writer.compress_all_hunks_ld(&mut avi, &params, &bytes_done, &cancel)?;
            writer.finalize()?;
            Ok(())
        },
    )
    .await?;

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

    let bin_path_owned = bin_path.clone();
    let cue_sheet_owned = cue_sheet.clone();
    let codecs = opts.codecs.clone().unwrap_or_else(default_cd_codecs);
    let level = opts.level;
    run_scratch_write(
        &output_path,
        true,
        progress,
        &cancel,
        move |write_path, bytes_done, cancel| -> ChdResult<()> {
            let bin_file = std::fs::File::open(&bin_path_owned)?;
            let mut bin_reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, bin_file);

            let mut writer = ChdWriter::create(
                &write_path,
                total_sectors,
                CD_HUNK_BYTES,
                &cue_sheet_owned,
                codecs,
                level,
            )?;

            writer.compress_all_hunks(&mut bin_reader, SECTOR_SIZE, &bytes_done, &cancel)?;
            writer.finalize()?;
            Ok(())
        },
    )
    .await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::chd::reader::ChdFlavor;

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

        verify_chd(
            &NoProgress,
            chd_path.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        // No extension on the output: the DVD path must derive .iso.
        let out_base = dir.path().join("restored");
        extract_from_chd(
            &NoProgress,
            chd_path,
            out_base.clone(),
            None,
            CancelToken::new(),
        )
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

        let header = crate::disc::chd::reader::open_chd_sync(&chd_path)
            .unwrap()
            .header;
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

        let header = crate::disc::chd::reader::open_chd_sync(&chd_path)
            .unwrap()
            .header;
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
            verify_chd(
                &NoProgress,
                chd_path.clone(),
                None,
                false,
                CancelToken::new()
            )
            .await
            .is_err()
        );
        let out = dir.path().join("restored.iso");
        assert!(
            extract_from_chd(&NoProgress, chd_path, out, None, CancelToken::new())
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

        verify_chd(
            &NoProgress,
            their_chd.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let restored = dir.path().join("restored.iso");
        extract_from_chd(
            &NoProgress,
            their_chd,
            restored.clone(),
            None,
            CancelToken::new(),
        )
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
        let handle = crate::disc::chd::reader::open_chd_sync(path).unwrap();
        cd_track_metadata_text(&handle.metadata).expect("CHT2 metadata present")
    }

    fn has_dvd_tag(path: &std::path::Path) -> bool {
        crate::disc::chd::reader::open_chd_sync(path)
            .unwrap()
            .flavor()
            == ChdFlavor::Dvd
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
            CancelToken::new(),
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

        verify_chd(
            &NoProgress,
            chd_path.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(
            &NoProgress,
            chd_path,
            out_cue.clone(),
            None,
            CancelToken::new(),
        )
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
            CancelToken::new(),
        )
        .await
        .unwrap();

        let meta = cd_track_metadata(&chd_path);
        assert!(meta.contains("FRAMES:11"), "metadata: {meta}");

        verify_chd(
            &NoProgress,
            chd_path.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(
            &NoProgress,
            chd_path,
            out_cue.clone(),
            None,
            CancelToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(out_cue.with_extension("bin")).unwrap(), iso);
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
            CancelToken::new(),
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
            let handle = crate::disc::chd::reader::open_chd_sync(&chd_path).unwrap();
            assert_eq!(handle.header.logical_bytes, 20 * 2448);
        }

        verify_chd(
            &NoProgress,
            chd_path.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out_cue = dir.path().join("restored.cue");
        extract_from_chd(
            &NoProgress,
            chd_path,
            out_cue.clone(),
            None,
            CancelToken::new(),
        )
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

        verify_chd(
            &NoProgress,
            their_chd.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();
        let restored_cue = dir.path().join("restored.cue");
        extract_from_chd(
            &NoProgress,
            their_chd.clone(),
            restored_cue.clone(),
            None,
            CancelToken::new(),
        )
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
        extract_from_chd(
            &NoProgress,
            their_chd.clone(),
            restored_cue.clone(),
            None,
            CancelToken::new(),
        )
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

    use crate::disc::laserdisc::avi::test_fixtures::{
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
        crate::disc::chd::reader::open_chd_sync(path)
            .unwrap()
            .flavor()
            == ChdFlavor::Ld
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
            CancelToken::new(),
        )
        .await
        .unwrap();

        let header = crate::disc::chd::reader::open_chd_sync(&chd_path)
            .unwrap()
            .header;
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

        let err = convert_avi_to_chd(
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
            CancelToken::new(),
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
            CancelToken::new(),
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
            let err = convert_disc_to_chd(
                &NoProgress,
                avi_path.clone(),
                chd_path,
                None,
                opts,
                CancelToken::new(),
            )
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
            CancelToken::new(),
        )
        .await
        .unwrap();

        verify_chd(
            &NoProgress,
            chd_path.clone(),
            None,
            false,
            CancelToken::new(),
        )
        .await
        .unwrap();

        let mut chd = std::fs::read(&chd_path).unwrap();
        let data_start = 124 + 17;
        let mid = data_start + (chd.len() - data_start) / 2;
        chd[mid] ^= 0xFF;
        std::fs::write(&chd_path, &chd).unwrap();

        assert!(
            verify_chd(&NoProgress, chd_path, None, false, CancelToken::new())
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
            CancelToken::new(),
        )
        .await
        .unwrap();

        let out = dir.path().join("restored");
        let err = extract_from_chd(&NoProgress, chd_path.clone(), out, None, CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ChdError::LdExtractionUnsupported));

        let err = is_dvd_mode_chd(chd_path).await.unwrap_err();
        assert!(matches!(err, ChdError::LdExtractionUnsupported));
    }
}
