//! Unified detection, verification, and migration entry for legacy
//! GameCube/Wii input formats (GCZ, WIA, NKit).
//!
//! [`migrate_disc`] is the public operation: an integrity pass over
//! the source container first, then streaming conversion to RVZ
//! through the regular compress pipeline. [`compress_disc`] delegates
//! here automatically when it detects a legacy container, so the GUI
//! compress flow handles these formats without changes.
//!
//! [`compress_disc`]: crate::nintendo::rvz::compress::compress_disc

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::{info, warn};
use tokio::task;

use super::gcz;
use super::rvz::compress::{RvzCompressOptions, compress_iso_cancellable};
use super::rvz::error::{RvzError, RvzResult};
use crate::util::{CancelToken, ProgressReporter};

const NKIT_MAGIC_OFFSET: u64 = 0x200;
const NKIT_MAGIC: &[u8; 4] = b"NKIT";
const WIA_MAGIC: [u8; 4] = [b'W', b'I', b'A', 0x01];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyFormat {
    Gcz,
    Wia,
    NkitIso,
    NkitGcz,
}

impl LegacyFormat {
    pub fn name(&self) -> &'static str {
        match self {
            LegacyFormat::Gcz => "GCZ",
            LegacyFormat::Wia => "WIA",
            LegacyFormat::NkitIso => "NKit ISO",
            LegacyFormat::NkitGcz => "NKit GCZ",
        }
    }
}

/// Legacy formats a GameCube (`dol`) migration accepts. WIA is a
/// Wii-only container, so it is excluded here and belongs to `rvl`.
pub const DOL_MIGRATE_FORMATS: &[LegacyFormat] = &[
    LegacyFormat::Gcz,
    LegacyFormat::NkitIso,
    LegacyFormat::NkitGcz,
];

/// Every legacy format the migration pipeline understands. Used by the
/// `rvl` path and by the compress auto-detect route, which stay
/// permissive because a Wii disc image is valid there.
pub const ALL_MIGRATE_FORMATS: &[LegacyFormat] = &[
    LegacyFormat::Gcz,
    LegacyFormat::Wia,
    LegacyFormat::NkitIso,
    LegacyFormat::NkitGcz,
];

/// Reject a detected container that the caller's console gate does not
/// accept, naming the format and pointing at the command that handles
/// it. Single source of the wording for both migrate and compress.
pub fn ensure_format_allowed(fmt: LegacyFormat, allowed: &[LegacyFormat]) -> RvzResult<()> {
    ensure_format_allowed_for(fmt, allowed, "rvl migrate")
}

/// Same console gate as [`ensure_format_allowed`], but naming the command that
/// should handle the rejected container so `verify` points at `rvl verify`.
pub fn ensure_format_allowed_for(
    fmt: LegacyFormat,
    allowed: &[LegacyFormat],
    next_command: &str,
) -> RvzResult<()> {
    if allowed.contains(&fmt) {
        Ok(())
    } else {
        Err(RvzError::Custom(format!(
            "input is a {} image; use {next_command} for Wii disc images",
            fmt.name()
        )))
    }
}

/// Counts from a batch run, returned so callers (and tests) can inspect
/// exactly what happened without parsing the log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchSummary {
    pub ok: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Identify a legacy container by magic bytes; extensions are never
/// trusted, so renamed files still route correctly. Returns `None`
/// for anything the regular compress path already handles (plain
/// ISO/GCM, WBFS, or unknown data).
pub fn detect_legacy_format(input: &Path) -> std::io::Result<Option<LegacyFormat>> {
    let mut f = File::open(input)?;
    let mut head = [0u8; 4];
    match f.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    if head == WIA_MAGIC {
        return Ok(Some(LegacyFormat::Wia));
    }
    if u32::from_le_bytes(head) == gcz::format::GCZ_MAGIC {
        // An NKit stream keeps its own header inside the compressed
        // disc; decode just enough of block 0 to look at it. A
        // damaged block 0 falls back to plain GCZ so the verify pass
        // reports the corruption instead of detection hiding it.
        let is_nkit = gcz::gcz_logical_prefix(input, 0x204)
            .map(|p| p.len() >= 0x204 && p[0x200..0x204] == *NKIT_MAGIC)
            .unwrap_or(false);
        return Ok(Some(if is_nkit {
            LegacyFormat::NkitGcz
        } else {
            LegacyFormat::Gcz
        }));
    }
    // Plain files: NKit puts its header in Boot.bin's reserved area.
    if f.seek(SeekFrom::Start(NKIT_MAGIC_OFFSET)).is_ok() {
        let mut magic = [0u8; 4];
        if f.read_exact(&mut magic).is_ok() && magic == *NKIT_MAGIC {
            return Ok(Some(LegacyFormat::NkitIso));
        }
    }
    Ok(None)
}

/// Run a blocking job that reports progress through an atomic byte
/// counter, polled at 100 ms like the compress pipelines.
async fn run_blocking_with_progress<T, F>(
    total: u64,
    msg: &str,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
    job: F,
) -> RvzResult<T>
where
    T: Send + 'static,
    F: FnOnce(Arc<AtomicU64>, CancelToken) -> RvzResult<T> + Send + 'static,
{
    progress.start(total, msg);
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();
    let cancel_bg = cancel.clone();
    let mut handle = task::spawn_blocking(move || job(bytes_done_bg, cancel_bg));
    let result = loop {
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut handle).await {
            Ok(joined) => break joined?,
            Err(_) => {
                let delta = bytes_done.swap(0, Ordering::Relaxed);
                if delta > 0 {
                    progress.inc(delta);
                }
            }
        }
    };
    let remaining = bytes_done.swap(0, Ordering::Relaxed);
    if remaining > 0 {
        progress.inc(remaining);
    }
    progress.finish();
    if result.is_ok() && cancel.is_cancelled() {
        return Err(RvzError::Cancelled);
    }
    result.map_err(|e| {
        if cancel.is_cancelled() {
            RvzError::Cancelled
        } else {
            e
        }
    })
}

/// Pre-conversion integrity pass. GCZ checks every stored block's
/// Adler-32 without inflating; WIA checks the SHA-1 header chain and
/// decodes both metadata tables (`deep` additionally decodes every
/// group through the codec); the NKit pass lands with its reader.
pub async fn verify_legacy_input(
    input: &Path,
    fmt: LegacyFormat,
    deep: bool,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> RvzResult<()> {
    match fmt {
        LegacyFormat::Gcz => {
            let total = {
                let path = input.to_path_buf();
                task::spawn_blocking(move || gcz::verify_total(&path)).await??
            };
            let path = input.to_path_buf();
            run_blocking_with_progress(
                total,
                "Verifying GCZ integrity",
                progress,
                cancel,
                move |done, cancel| Ok(gcz::verify_gcz_blocking(&path, done, cancel)?),
            )
            .await
        }
        LegacyFormat::Wia => {
            let total = {
                let path = input.to_path_buf();
                task::spawn_blocking(move || super::wia::verify_total(&path, deep)).await??
            };
            let path = input.to_path_buf();
            run_blocking_with_progress(
                total,
                "Verifying WIA integrity",
                progress,
                cancel,
                move |done, cancel| Ok(super::wia::verify_wia_blocking(&path, deep, done, cancel)?),
            )
            .await
        }
        LegacyFormat::NkitIso | LegacyFormat::NkitGcz => {
            let wrapped = fmt == LegacyFormat::NkitGcz;
            let total = {
                let path = input.to_path_buf();
                task::spawn_blocking(move || super::nkit::verify_total(&path, wrapped)).await??
            };
            let path = input.to_path_buf();
            run_blocking_with_progress(
                total,
                "Verifying NKit integrity",
                progress,
                cancel,
                move |done, cancel| {
                    Ok(super::nkit::verify_nkit_blocking(
                        &path, wrapped, done, cancel,
                    )?)
                },
            )
            .await
        }
    }
}

/// Knobs for the migrate operation's verify phase.
#[derive(Debug, Clone, Copy, Default)]
pub struct MigrateOptions {
    /// Skip the pre-conversion integrity pass entirely.
    pub skip_verify: bool,
    /// Walk every group through the codec during verification instead
    /// of only the cheap header-chain checks (WIA only; GCZ and NKit
    /// passes are already exhaustive).
    pub deep_verify: bool,
}

/// Verify a legacy container, then stream-convert it to RVZ. No
/// temporary files: the source reconstructs the logical disc on the
/// fly and feeds the regular compress pipeline.
pub async fn migrate_disc(
    input: &Path,
    output: &Path,
    options: RvzCompressOptions,
    migrate: MigrateOptions,
    progress: &dyn ProgressReporter,
) -> RvzResult<()> {
    migrate_disc_cancellable(
        input,
        output,
        options,
        migrate,
        ALL_MIGRATE_FORMATS,
        progress,
        CancelToken::new(),
    )
    .await
}

/// Like [`migrate_disc`] but observes `cancel`. Both phases honor the
/// token: the verify pass checks it at block/group boundaries and the
/// conversion phase streams through [`compress_iso_cancellable`],
/// which writes to a scratch file and renames on success, so an
/// interrupted migration leaves no partial RVZ behind.
pub async fn migrate_disc_cancellable(
    input: &Path,
    output: &Path,
    options: RvzCompressOptions,
    migrate: MigrateOptions,
    allowed: &[LegacyFormat],
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<()> {
    let fmt = {
        let input = input.to_path_buf();
        task::spawn_blocking(move || detect_legacy_format(&input)).await??
    }
    .ok_or_else(|| {
        RvzError::Custom(
            "input is not a GCZ, WIA, or NKit image; use compress for .iso/.gcm/.wbfs".into(),
        )
    })?;
    ensure_format_allowed(fmt, allowed)?;
    info!("Migrating {} input {} to RVZ", fmt.name(), input.display());
    if !migrate.skip_verify {
        verify_legacy_input(input, fmt, migrate.deep_verify, progress, &cancel).await?;
    }
    compress_iso_cancellable(input, output, options, progress, cancel).await
}

/// Migrate every legacy container directly inside `dir` (top level
/// only, matching the ctr batch commands). Outputs land next to their
/// inputs with the extension replaced by .rvz. Failures are reported
/// and counted instead of aborting the run; an existing output
/// without `force` is skipped, not failed.
pub async fn migrate_disc_batch(
    dir: &Path,
    options: RvzCompressOptions,
    migrate: MigrateOptions,
    allowed: &[LegacyFormat],
    force: bool,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> RvzResult<BatchSummary> {
    let detected = {
        let dir = dir.to_path_buf();
        task::spawn_blocking(move || -> std::io::Result<Vec<(PathBuf, LegacyFormat)>> {
            let mut found = Vec::new();
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.is_file()
                    && let Some(fmt) = detect_legacy_format(&path)?
                {
                    found.push((path, fmt));
                }
            }
            found.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(found)
        })
        .await??
    };
    let mut inputs = Vec::new();
    let mut rejected = Vec::new();
    for (path, fmt) in detected {
        if allowed.contains(&fmt) {
            inputs.push(path);
        } else {
            rejected.push((path, fmt));
        }
    }
    if inputs.is_empty() && rejected.is_empty() {
        return Err(RvzError::Custom(format!(
            "no GCZ, WIA, or NKit images found in {}",
            dir.display()
        )));
    }
    for (path, fmt) in &rejected {
        warn!(
            "Skipped {}: {} is a Wii disc image; use rvl migrate",
            path.display(),
            fmt.name()
        );
    }
    info!("Migrating {} legacy images to RVZ", inputs.len());
    let mut summary = BatchSummary {
        skipped: rejected.len(),
        ..BatchSummary::default()
    };
    for input in &inputs {
        if cancel.is_cancelled() {
            break;
        }
        let output = super::rvz::derive_rvz_path(input);
        if !force && output.exists() {
            warn!(
                "Skipped {}: output already exists; pass --force to overwrite",
                input.display()
            );
            summary.skipped += 1;
            continue;
        }
        match migrate_disc_cancellable(
            input,
            &output,
            options,
            migrate,
            allowed,
            progress,
            cancel.clone(),
        )
        .await
        {
            Ok(()) => summary.ok += 1,
            Err(RvzError::Cancelled) => break,
            Err(e) => {
                warn!("Failed to migrate {}: {e}", input.display());
                summary.failed += 1;
            }
        }
    }
    info!(
        "Processed {} files in {}: {} ok, {} failed, {} skipped",
        summary.ok + summary.failed + summary.skipped,
        dir.display(),
        summary.ok,
        summary.failed,
        summary.skipped
    );
    if cancel.is_cancelled() {
        return Ok(summary);
    }
    if summary.failed > 0 {
        return Err(RvzError::Custom(format!(
            "{} of {} migrations failed",
            summary.failed,
            inputs.len()
        )));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::dol::test_fixtures::make_fake_gamecube_iso;
    use crate::nintendo::gcz::test_fixtures::make_gcz;
    use crate::nintendo::rvz::{compress_disc, decompress_disc};
    use crate::util::NoProgress;
    use std::io::Write;

    fn write_gcz_fixture(dir: &Path, iso: &[u8]) -> std::path::PathBuf {
        let path = dir.join("game.gcz");
        let mut f = File::create(&path).unwrap();
        f.write_all(&make_gcz(iso, 0x8000, 0)).unwrap();
        path
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gcz_migrates_to_rvz_byte_identical_to_iso_path() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(5 * 1024 * 1024 + 123);

        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &original).unwrap();
        let gcz = write_gcz_fixture(dir.path(), &original);

        let rvz_from_gcz = dir.path().join("from_gcz.rvz");
        migrate_disc(
            &gcz,
            &rvz_from_gcz,
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();

        // The migrated RVZ must be byte-identical to compressing the
        // original ISO directly.
        let rvz_from_iso = dir.path().join("from_iso.rvz");
        compress_disc(
            &iso,
            &rvz_from_iso,
            RvzCompressOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&rvz_from_gcz).unwrap(),
            std::fs::read(&rvz_from_iso).unwrap()
        );

        let restored = dir.path().join("restored.iso");
        decompress_disc(&rvz_from_gcz, &restored, &NoProgress)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compress_disc_auto_routes_gcz_through_migration() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(2 * 1024 * 1024 + 7);
        let gcz = write_gcz_fixture(dir.path(), &original);

        let rvz = dir.path().join("auto.rvz");
        compress_disc(&gcz, &rvz, RvzCompressOptions::default(), &NoProgress)
            .await
            .unwrap();

        let restored = dir.path().join("restored.iso");
        decompress_disc(&rvz, &restored, &NoProgress).await.unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migrate_rejects_corrupted_gcz_before_converting() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(1024 * 1024);
        let gcz = write_gcz_fixture(dir.path(), &original);

        let mut bytes = std::fs::read(&gcz).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 0xFF;
        std::fs::write(&gcz, &bytes).unwrap();

        let rvz = dir.path().join("out.rvz");
        let err = migrate_disc(
            &gcz,
            &rvz,
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(!rvz.exists(), "no output may be written for corrupt input");
    }

    #[derive(Default)]
    struct PhaseRecorder {
        messages: std::sync::Mutex<Vec<String>>,
    }

    impl ProgressReporter for PhaseRecorder {
        fn start(&self, _total: u64, msg: &str) {
            self.messages.lock().unwrap().push(msg.to_string());
        }
        fn inc(&self, _delta: u64) {}
        fn finish(&self) {}
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compress_verifies_legacy_input_before_converting() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(1024 * 1024);

        let corrupt = write_gcz_fixture(dir.path(), &original);
        let mut bytes = std::fs::read(&corrupt).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 0xFF;
        std::fs::write(&corrupt, &bytes).unwrap();

        let corrupt_out = dir.path().join("corrupt.rvz");
        let err = compress_disc(
            &corrupt,
            &corrupt_out,
            RvzCompressOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        assert!(
            !corrupt_out.exists(),
            "no output may be written for corrupt legacy input"
        );

        let valid_dir = tempfile::tempdir().unwrap();
        let valid = write_gcz_fixture(valid_dir.path(), &original);
        let valid_out = valid_dir.path().join("valid.rvz");
        let recorder = PhaseRecorder::default();
        compress_disc(&valid, &valid_out, RvzCompressOptions::default(), &recorder)
            .await
            .unwrap();
        assert!(valid_out.exists());

        let messages = recorder.messages.lock().unwrap();
        assert!(
            messages.iter().any(|m| m.contains("Verifying")),
            "compress on a legacy input must run the verify phase, saw {messages:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn verify_phase_observes_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(4 * 1024 * 1024);
        let gcz = write_gcz_fixture(dir.path(), &original);

        let cancel = CancelToken::new();
        cancel.cancel();

        let rvz = dir.path().join("out.rvz");
        let err = migrate_disc_cancellable(
            &gcz,
            &rvz,
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            ALL_MIGRATE_FORMATS,
            &NoProgress,
            cancel,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RvzError::Cancelled), "{err}");
        assert!(!rvz.exists(), "no output may be written when cancelled");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batch_cancellation_stops_cleanly_without_failure() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(1024 * 1024);
        for name in ["a.gcz", "b.gcz"] {
            let path = dir.path().join(name);
            let mut f = File::create(&path).unwrap();
            f.write_all(&make_gcz(&original, 0x8000, 0)).unwrap();
        }

        let cancel = CancelToken::new();
        cancel.cancel();

        let summary = migrate_disc_batch(
            dir.path(),
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            ALL_MIGRATE_FORMATS,
            true,
            &NoProgress,
            cancel,
        )
        .await
        .expect("cancelled batch must return Ok, not a failure");

        // Cancellation fires before any file completes, so the summary
        // must not fold the two unprocessed files into the ok count.
        assert_eq!(
            summary,
            BatchSummary {
                ok: 0,
                failed: 0,
                skipped: 0
            }
        );
        assert!(!dir.path().join("a.rvz").exists());
        assert!(!dir.path().join("b.rvz").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batch_existing_output_is_skipped_not_failed() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(1024 * 1024);
        write_gcz_fixture(dir.path(), &original);

        let existing = dir.path().join("game.rvz");
        let junk = b"not a real rvz".to_vec();
        std::fs::write(&existing, &junk).unwrap();

        let summary = migrate_disc_batch(
            dir.path(),
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            ALL_MIGRATE_FORMATS,
            false,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .expect("existing output must be skipped, not counted as a failure");

        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.ok, 0);
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            junk,
            "the pre-existing output must be left untouched"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wia_migrates_to_rvz_byte_identical_to_iso_path() {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use crate::nintendo::wia::test_fixtures::make_wia;

        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_wii_iso_with_partition(2);

        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &original).unwrap();
        let wia = dir.path().join("game.wia");
        std::fs::write(&wia, make_wia(&original, 3, 0x20_0000)).unwrap();

        let rvz_from_wia = dir.path().join("from_wia.rvz");
        migrate_disc(
            &wia,
            &rvz_from_wia,
            RvzCompressOptions::default(),
            MigrateOptions {
                skip_verify: false,
                deep_verify: true,
            },
            &NoProgress,
        )
        .await
        .unwrap();

        let rvz_from_iso = dir.path().join("from_iso.rvz");
        compress_disc(
            &iso,
            &rvz_from_iso,
            RvzCompressOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&rvz_from_wia).unwrap(),
            std::fs::read(&rvz_from_iso).unwrap()
        );

        let restored = dir.path().join("restored.iso");
        decompress_disc(&rvz_from_wia, &restored, &NoProgress)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nkit_migrates_to_rvz_byte_identical_to_iso_path() {
        use crate::nintendo::nkit::test_fixtures::{
            crc_of, make_fake_gc_fs_iso, make_nkit_gc, make_nkit_gcz,
        };

        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gc_fs_iso();
        let nkit_bytes = make_nkit_gc(&original);

        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &original).unwrap();
        let nkit = dir.path().join("game.nkit.iso");
        std::fs::write(&nkit, &nkit_bytes).unwrap();
        let nkit_gcz = dir.path().join("game.nkit.gcz");
        std::fs::write(&nkit_gcz, make_nkit_gcz(&nkit_bytes, crc_of(&original))).unwrap();

        assert_eq!(
            detect_legacy_format(&nkit).unwrap(),
            Some(LegacyFormat::NkitIso)
        );
        assert_eq!(
            detect_legacy_format(&nkit_gcz).unwrap(),
            Some(LegacyFormat::NkitGcz)
        );

        let rvz_from_iso = dir.path().join("from_iso.rvz");
        compress_disc(
            &iso,
            &rvz_from_iso,
            RvzCompressOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();

        for (input, name) in [(&nkit, "from_nkit.rvz"), (&nkit_gcz, "from_nkit_gcz.rvz")] {
            let rvz = dir.path().join(name);
            migrate_disc(
                input,
                &rvz,
                RvzCompressOptions::default(),
                MigrateOptions::default(),
                &NoProgress,
            )
            .await
            .unwrap();
            assert_eq!(
                std::fs::read(&rvz).unwrap(),
                std::fs::read(&rvz_from_iso).unwrap(),
                "{name} must match the direct ISO compression"
            );
        }

        let restored = dir.path().join("restored.iso");
        decompress_disc(&dir.path().join("from_nkit.rvz"), &restored, &NoProgress)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nkit_wii_migrates_to_rvz_byte_identical_to_iso_path() {
        use crate::nintendo::nkit::test_fixtures::{make_fake_wii_fs_iso, make_nkit_wii};

        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_wii_fs_iso();
        let iso = dir.path().join("game.iso");
        std::fs::write(&iso, &original).unwrap();
        let nkit = dir.path().join("game.nkit.iso");
        std::fs::write(&nkit, make_nkit_wii(&original)).unwrap();

        let rvz_from_iso = dir.path().join("from_iso.rvz");
        compress_disc(
            &iso,
            &rvz_from_iso,
            RvzCompressOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();

        let rvz_from_nkit = dir.path().join("from_nkit.rvz");
        migrate_disc(
            &nkit,
            &rvz_from_nkit,
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&rvz_from_nkit).unwrap(),
            std::fs::read(&rvz_from_iso).unwrap()
        );

        let restored = dir.path().join("restored.iso");
        decompress_disc(&rvz_from_nkit, &restored, &NoProgress)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&restored).unwrap(), original);
    }

    #[tokio::test]
    async fn migrate_rejects_plain_iso() {
        let dir = tempfile::tempdir().unwrap();
        let iso = dir.path().join("plain.iso");
        std::fs::write(&iso, make_fake_gamecube_iso(0x10000)).unwrap();
        let err = migrate_disc(
            &iso,
            &dir.path().join("out.rvz"),
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            &NoProgress,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not a GCZ"), "{err}");
    }

    fn write_wia_fixture(dir: &Path) -> std::path::PathBuf {
        use crate::nintendo::rvl::test_fixtures::make_fake_wii_iso_with_partition;
        use crate::nintendo::wia::test_fixtures::make_wia;
        let original = make_fake_wii_iso_with_partition(2);
        let wia = dir.join("wii.wia");
        std::fs::write(&wia, make_wia(&original, 3, 0x20_0000)).unwrap();
        wia
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dol_gate_rejects_wia_but_rvl_gate_accepts_it() {
        let dir = tempfile::tempdir().unwrap();
        let wia = write_wia_fixture(dir.path());

        let rejected = dir.path().join("rejected.rvz");
        let err = migrate_disc_cancellable(
            &wia,
            &rejected,
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            DOL_MIGRATE_FORMATS,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "input is a WIA image; use rvl migrate for Wii disc images"
        );
        assert!(
            !rejected.exists(),
            "a rejected input must not produce output"
        );

        let accepted = dir.path().join("accepted.rvz");
        migrate_disc_cancellable(
            &wia,
            &accepted,
            RvzCompressOptions::default(),
            MigrateOptions {
                skip_verify: false,
                deep_verify: true,
            },
            ALL_MIGRATE_FORMATS,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .expect("the rvl gate must still convert a WIA image");
        assert!(accepted.exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dol_batch_skips_wia_and_migrates_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let gc = make_fake_gamecube_iso(1024 * 1024);
        write_gcz_fixture(dir.path(), &gc);
        let wia = write_wia_fixture(dir.path());

        let summary = migrate_disc_batch(
            dir.path(),
            RvzCompressOptions::default(),
            MigrateOptions::default(),
            DOL_MIGRATE_FORMATS,
            true,
            &NoProgress,
            CancelToken::new(),
        )
        .await
        .expect("a WIA in the directory must not fail a dol batch");

        assert_eq!(
            summary,
            BatchSummary {
                ok: 1,
                failed: 0,
                skipped: 1
            }
        );
        assert!(dir.path().join("game.rvz").exists());
        assert!(!crate::nintendo::rvz::derive_rvz_path(&wia).exists());
    }

    #[test]
    fn detection_distinguishes_formats() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_fake_gamecube_iso(0x20000);

        let gcz = write_gcz_fixture(dir.path(), &original);
        assert_eq!(detect_legacy_format(&gcz).unwrap(), Some(LegacyFormat::Gcz));

        let iso = dir.path().join("plain.iso");
        std::fs::write(&iso, &original).unwrap();
        assert_eq!(detect_legacy_format(&iso).unwrap(), None);

        // NKit ISO: magic in Boot.bin's reserved area.
        let mut nkit_bytes = original.clone();
        nkit_bytes[0x200..0x204].copy_from_slice(b"NKIT");
        let nkit = dir.path().join("game.nkit.iso");
        std::fs::write(&nkit, &nkit_bytes).unwrap();
        assert_eq!(
            detect_legacy_format(&nkit).unwrap(),
            Some(LegacyFormat::NkitIso)
        );

        let nkit_gcz = dir.path().join("game.nkit.gcz");
        let mut f = File::create(&nkit_gcz).unwrap();
        f.write_all(&make_gcz(&nkit_bytes, 0x8000, 0)).unwrap();
        drop(f);
        assert_eq!(
            detect_legacy_format(&nkit_gcz).unwrap(),
            Some(LegacyFormat::NkitGcz)
        );

        let wia = dir.path().join("game.wia");
        std::fs::write(&wia, [b'W', b'I', b'A', 0x01, 0, 0, 0, 0]).unwrap();
        assert_eq!(detect_legacy_format(&wia).unwrap(), Some(LegacyFormat::Wia));

        let tiny = dir.path().join("tiny.bin");
        std::fs::write(&tiny, [0u8; 2]).unwrap();
        assert_eq!(detect_legacy_format(&tiny).unwrap(), None);
    }
}
