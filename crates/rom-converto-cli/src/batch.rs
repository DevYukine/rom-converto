use crate::util::ensure_output_writable;
use anyhow::Result;
use log::{info, warn};
use rom_converto_lib::util::fs::collect_files_with_exts;
use rom_converto_lib::util::{ProgressReporter, Tally, TallyDirection};
use std::path::{Path, PathBuf};

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

struct VerifyTally {
    total: usize,
    ok: usize,
    failed: usize,
}

fn collect_or_warn(
    input_dir: &Path,
    exts: &[&str],
    max_depth: Option<usize>,
) -> Result<Vec<PathBuf>> {
    let files = collect_files_with_exts(input_dir, exts, max_depth)?;
    if files.is_empty() {
        warn!(
            "No matching files found in {} (looked for {:?})",
            input_dir.display(),
            exts
        );
    }
    Ok(files)
}

fn finish_verify(tally: VerifyTally) -> Result<()> {
    info!(
        "Verified {} files: {} OK, {} failed",
        tally.total, tally.ok, tally.failed
    );
    if tally.failed > 0 {
        anyhow::bail!("verification failed");
    }
    Ok(())
}

fn finish_tally(tally: &Tally, direction: TallyDirection) -> Result<()> {
    info!("{}", tally.summary_line(direction));
    let failed = tally.failed_count();
    if failed > 0 {
        anyhow::bail!("{failed} of {} files failed", tally.count());
    }
    Ok(())
}

pub async fn cso_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    force: bool,
    output_dir: Option<&Path>,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::cso::decompress_from_cso;
    use rom_converto_lib::util::place_in_dir_mirrored;

    let files = collect_or_warn(input_dir, &["cso", "zso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Decompressing {total} files..."));
    let mut tally = Tally::new();
    for path in files {
        let output = place_in_dir_mirrored(&path.with_extension("iso"), input_dir, output_dir);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = ensure_output_writable(&output, force) {
            warn!("{e}");
            tally.record_skipped();
            total_progress.inc(1);
            continue;
        }
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        if let Err(e) = decompress_from_cso(progress, path.clone(), output, force).await {
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
        } else {
            tally.record_ok(input_bytes, file_len(&out_path), Default::default());
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Decompress)
}

pub async fn cso_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::cso::verify_cso;

    let files = collect_or_warn(input_dir, &["cso", "zso"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        match verify_cso(progress, path.clone(), full).await {
            Ok(()) => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

#[allow(clippy::too_many_arguments)]
pub async fn rvz_compress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    exts: &[&str],
    opts: rom_converto_lib::nintendo::rvz::RvzCompressOptions,
    force: bool,
    output_dir: Option<&Path>,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvz::{compress_disc, derive_rvz_path};
    use rom_converto_lib::util::place_in_dir_mirrored;

    let files = collect_or_warn(input_dir, exts, max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Compressing {total} files..."));
    let mut tally = Tally::new();
    for path in files {
        let output = place_in_dir_mirrored(&derive_rvz_path(&path), input_dir, output_dir);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = ensure_output_writable(&output, force) {
            warn!("{e}");
            tally.record_skipped();
            total_progress.inc(1);
            continue;
        }
        let input_bytes = file_len(&path);
        if let Err(e) = compress_disc(&path, &output, opts, progress).await {
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
        } else {
            tally.record_ok(input_bytes, file_len(&output), Default::default());
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Compress)
}

pub async fn rvz_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    force: bool,
    output_dir: Option<&Path>,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvz::{decompress_disc, derive_disc_path};
    use rom_converto_lib::util::place_in_dir_mirrored;

    let files = collect_or_warn(input_dir, &["rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Decompressing {total} files..."));
    let mut tally = Tally::new();
    for path in files {
        let output = place_in_dir_mirrored(&derive_disc_path(&path), input_dir, output_dir);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = ensure_output_writable(&output, force) {
            warn!("{e}");
            tally.record_skipped();
            total_progress.inc(1);
            continue;
        }
        let input_bytes = file_len(&path);
        if let Err(e) = decompress_disc(&path, &output, progress).await {
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
        } else {
            tally.record_ok(input_bytes, file_len(&output), Default::default());
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Decompress)
}

pub async fn dol_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};

    let files = collect_or_warn(input_dir, &["iso", "gcm", "rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    let opts = DolVerifyOptions { full };
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        match verify_dol(&path, &opts, progress) {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

pub async fn rvl_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    full: bool,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};

    let files = collect_or_warn(input_dir, &["iso", "wbfs", "rvz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    let opts = RvlVerifyOptions { full };
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        match verify_rvl(&path, &opts, progress) {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

pub struct NxCompressTuning {
    pub level: Option<i32>,
    pub mode: Option<String>,
    pub block_size_exp: Option<u8>,
    pub force: bool,
    pub output_dir: Option<PathBuf>,
    pub max_depth: Option<usize>,
}

pub async fn nx_compress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    tuning: NxCompressTuning,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::{
        NczMode, NxCompressOptions, compress_container_async, derive_compressed_path,
        detect_container,
    };
    use rom_converto_lib::util::place_in_dir_mirrored;

    let files = collect_or_warn(input_dir, &["nsp", "xci"], tuning.max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if let Some(dir) = tuning.output_dir.as_deref() {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Compressing {total} files..."));
    let mut tally = Tally::new();
    for path in files {
        let kind = match detect_container(&path) {
            Ok(kind) => kind,
            Err(e) => {
                warn!("Failed to compress {}: {e}", path.display());
                tally.record_failed();
                total_progress.inc(1);
                continue;
            }
        };
        let mut opts = NxCompressOptions::for_kind(kind);
        if let Some(level) = tuning.level {
            opts.level = level;
        }
        if let Some(mode) = tuning.mode.as_deref() {
            opts.mode = match mode {
                "solid" => NczMode::Solid,
                "block" => NczMode::Block {
                    size_exp: tuning.block_size_exp.unwrap_or(20),
                },
                _ => unreachable!("clap value_parser already validated"),
            };
        } else if let Some(exp) = tuning.block_size_exp {
            opts.mode = NczMode::Block { size_exp: exp };
        }
        let output = place_in_dir_mirrored(
            &derive_compressed_path(&path),
            input_dir,
            tuning.output_dir.as_deref(),
        );
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = ensure_output_writable(&output, tuning.force) {
            warn!("{e}");
            tally.record_skipped();
            total_progress.inc(1);
            continue;
        }
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        if let Err(e) =
            compress_container_async(path.clone(), output, opts, keys.clone(), progress).await
        {
            warn!("Failed to compress {}: {e}", path.display());
            tally.record_failed();
        } else {
            tally.record_ok(input_bytes, file_len(&out_path), Default::default());
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Compress)
}

pub async fn nx_decompress(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    force: bool,
    output_dir: Option<&Path>,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::{decompress_container_async, derive_decompressed_path};
    use rom_converto_lib::util::place_in_dir_mirrored;

    let files = collect_or_warn(input_dir, &["nsz", "xcz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir)?;
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Decompressing {total} files..."));
    let mut tally = Tally::new();
    for path in files {
        let output = place_in_dir_mirrored(&derive_decompressed_path(&path), input_dir, output_dir);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = ensure_output_writable(&output, force) {
            warn!("{e}");
            tally.record_skipped();
            total_progress.inc(1);
            continue;
        }
        let input_bytes = file_len(&path);
        let out_path = output.clone();
        if let Err(e) =
            decompress_container_async(path.clone(), output, keys.clone(), progress).await
        {
            warn!("Failed to decompress {}: {e}", path.display());
            tally.record_failed();
        } else {
            tally.record_ok(input_bytes, file_len(&out_path), Default::default());
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_tally(&tally, TallyDirection::Decompress)
}

pub async fn nx_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    keys: rom_converto_lib::nintendo::nx::KeySet,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::nx::verify_container_async;

    let files = collect_or_warn(input_dir, &["nsp", "xci", "nsz", "xcz"], max_depth)?;
    if files.is_empty() {
        return Ok(());
    }
    let total = files.len();
    total_progress.start(total as u64, &format!("Verifying {total} files..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in files {
        match verify_container_async(path.clone(), keys.clone(), progress).await {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}

/// A direct subdirectory of `input_dir` is a NUS title dir when it
/// holds a `title.tmd` or any community `tmd.<N>` file, mirroring the
/// NUS layout discovery in the wup loader.
fn is_nus_title_dir(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name == "title.tmd" {
            return true;
        }
        if let Some(rest) = name.strip_prefix("tmd.")
            && rest.parse::<u32>().is_ok()
        {
            return true;
        }
    }
    false
}

pub async fn wup_verify(
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    input_dir: &Path,
    max_depth: Option<usize>,
) -> Result<()> {
    use rom_converto_lib::nintendo::wup::verify_wup_async;

    let mut inputs = collect_files_with_exts(input_dir, &["wud", "wux"], max_depth)?;
    if let Ok(entries) = std::fs::read_dir(input_dir) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && is_nus_title_dir(p))
            .collect();
        dirs.sort();
        inputs.extend(dirs);
    }
    inputs.sort();

    if inputs.is_empty() {
        warn!(
            "No .wud / .wux discs or NUS title directories found in {}",
            input_dir.display()
        );
        return Ok(());
    }

    let total = inputs.len();
    total_progress.start(total as u64, &format!("Verifying {total} titles..."));
    let mut ok = 0usize;
    let mut failed = 0usize;
    for path in inputs {
        match verify_wup_async(path.clone(), None, progress).await {
            Ok(result) if result.ok => {
                ok += 1;
                info!("[OK] {}", path.display());
            }
            Ok(_) => {
                failed += 1;
                warn!("[FAIL] {}", path.display());
            }
            Err(e) => {
                failed += 1;
                warn!("[FAIL] {}: {e}", path.display());
            }
        }
        total_progress.inc(1);
    }
    total_progress.finish();
    finish_verify(VerifyTally { total, ok, failed })
}
