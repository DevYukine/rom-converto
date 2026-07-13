//! Clap-based CLI over rom-converto-lib. Each subcommand maps one to one
//! onto a library conversion, verification, or info function; this crate
//! adds argument parsing, progress reporting, batch/dry-run orchestration,
//! and config file resolution around those calls.

use crate::commands::chd::ChdCommands;
use crate::commands::completions::ShellCompletionsCommand;
use crate::commands::cso::{CsoCommands, CsoFormatArg};
use crate::commands::ctr::CtrCommands;
use crate::commands::cue::CueCommands;
use crate::commands::dat::DatCommands;
use crate::commands::dol::DolCommands;
use crate::commands::nx::NxCommands;
use crate::commands::playlist::PlaylistModeArg;
use crate::commands::rvl::RvlCommands;
use crate::commands::wup::WupCommands;
use crate::commands::{Cli, Commands, SelfUpdateCommand};
use crate::github::api::GithubApi;
use crate::updater::{check_for_new_version_and_notify, cleanup_old_executable, self_update};
use crate::util::{
    IndicatifProgress, TotalProgress, WriteDecision, ensure_input_exists, policy_of,
    resolve_output, resolve_output_dir, resolve_policy,
};
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use clap_complete::{generate, generate_to};
use indicatif::MultiProgress;
use indicatif_log_bridge::LogWrapper;
use rom_converto_lib::chd::{
    ChdDvdOptions, DiscMode, convert_disc_to_chd_cancellable, extract_from_chd_cancellable,
    verify_chd, verify_chd_batch,
};
use rom_converto_lib::cso::{
    CsoCompressOptions, CsoFormat, compress_to_cso_cancellable, decompress_from_cso_cancellable,
    verify_cso,
};
use rom_converto_lib::cue::merge::merge_bin;
use rom_converto_lib::cue::to_iso::cue_to_iso;
use rom_converto_lib::nintendo::ctr::convert::{
    convert_rom_batch_cancellable, convert_rom_cancellable, derive_converted_path,
};
use rom_converto_lib::nintendo::ctr::verify::{
    CtrVerifyOptions, CtrVerifyResult, verify_ctr, verify_ctr_batch,
};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom_batch, compress_rom_cancellable, decompress_rom_batch, decompress_rom_cancellable,
    derive_compressed_path, derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia_cancellable, decrypt_rom_batch_cancellable,
    decrypt_rom_cancellable, derive_decrypted_path, derive_encrypted_path,
    encrypt_rom_batch_cancellable, encrypt_rom_cancellable, generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};
use rom_converto_lib::nintendo::legacy_input::{
    ALL_MIGRATE_FORMATS, DOL_MIGRATE_FORMATS, LegacyFormat, MigrateOptions, detect_legacy_format,
    ensure_format_allowed, ensure_format_allowed_for, migrate_disc_batch, migrate_disc_cancellable,
};
use rom_converto_lib::nintendo::nx::{
    NczMode, NxCompressOptions, compress_container_async_cancellable,
    decompress_container_async_cancellable, derive_compressed_path as nx_derive_compressed_path,
    derive_decompressed_path as nx_derive_decompressed_path, detect_container, load_keyset,
    verify_container_async,
};
use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc_cancellable, decompress_disc_cancellable,
    decompress_disc_to_wbfs_cancellable, derive_disc_path, derive_rvz_path,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async_cancellable,
    decrypt_nus_title_async_cancellable, verify_wup_async,
};
use rom_converto_lib::pipeline::{chd_to_cso_cancellable, cso_to_chd_cancellable, cue_to_cso};
use rom_converto_lib::playlist::{PlaylistMode, PlaylistOptions, plan_playlists};
use rom_converto_lib::util::fs::{collect_files_with_exts, is_os_junk_dir};
use rom_converto_lib::util::{
    ChecksumBounds, FileDigests, HashAlgo, Tally, TallyDirection, hash_file,
    mixed_playlist_extensions, oversized_rvz_chunk, parse_algos, parse_checksum_bound,
};
use std::io::IsTerminal;
use std::mem::discriminant;
use std::path::Path;
use std::time::Instant;

mod batch;
mod commands;
mod config;
mod dry_run;
mod github;
mod info_print;
mod logging;
mod updater;
mod util;
// Mirrors the inline logic in build.rs; kept here so it is unit-testable.
mod version;

pub mod built_info {
    // The file has been placed there by the build script.
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

// Extension sets mirror the lib-internal batch scanners so the closing
// count summary matches what each lib batch function actually processed.
const CTR_DECRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];
const CTR_ENCRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];
const CTR_COMPRESS_EXTS: &[&str] = &["cia", "cci", "3ds", "cxi", "3dsx"];
const CTR_DECOMPRESS_EXTS: &[&str] = &["zcia", "zcci", "zcxi", "z3dsx"];
const CTR_CONVERT_EXTS: &[&str] = &["cia", "3ds", "cci"];

// Union of image extensions the read side recognizes, used to pick the first
// convertible member when a format-agnostic command (hash) is handed an archive.
const ALL_IMAGE_EXTS: &[&str] = &[
    "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue", "cia",
    "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci", "nca", "nsz",
    "xcz", "ncz", "wud", "wux",
];

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn log_single_summary(input: &Path, output: &Path, direction: TallyDirection, started: Instant) {
    let mut tally = Tally::new();
    tally.record_ok(file_len(input), file_len(output), started.elapsed());
    log::info!("{}", tally.summary_line(direction));
}

fn finish_single(
    input: &Path,
    output: &Path,
    direction: TallyDirection,
    op: &str,
    started: Instant,
    report: Option<&Path>,
) -> Result<()> {
    use rom_converto_lib::util::{
        FileStatus, ReportFormat, ReportRecord, ReportTotals, write_report,
    };

    let elapsed = started.elapsed();
    let in_bytes = file_len(input);
    let out_bytes = file_len(output);
    let mut tally = Tally::new();
    tally.record_ok(in_bytes, out_bytes, elapsed);
    log::info!("{}", tally.summary_line(direction));
    if let Some(path) = report {
        let elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        let record = ReportRecord::new(
            input.display().to_string(),
            output.display().to_string(),
            op,
            FileStatus::Ok,
            in_bytes,
            out_bytes,
            elapsed_ms,
            None,
        );
        let totals = ReportTotals {
            total_files: 1,
            ok: 1,
            total_input_bytes: in_bytes,
            total_output_bytes: out_bytes,
            elapsed_ms,
            ..ReportTotals::default()
        };
        write_report(path, &[record], &totals, ReportFormat::from_path(path))?;
    }
    Ok(())
}

fn hash_single(
    progress: &dyn rom_converto_lib::util::ProgressReporter,
    input: &Path,
    algos: &[HashAlgo],
    report: Option<&Path>,
    cache: &rom_converto_lib::util::HashCache,
) -> Result<()> {
    use rom_converto_lib::util::{
        FileStatus, HashReportRecord, ReportFormat, ReportTotals, write_hash_report,
    };

    let started = Instant::now();
    let mut tally = Tally::new();
    let result = match cache.lookup_raw(input, algos) {
        Some(d) => Ok(d),
        None => {
            let computed = hash_file(input, algos, progress);
            if let Ok(d) = &computed {
                cache.store_raw(input, d);
            }
            computed
        }
    };
    let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let (record, outcome) = match result {
        Ok(d) => {
            print_hash_row(input, &d, algos);
            tally.record_ok(d.size_bytes, 0, started.elapsed());
            let record = HashReportRecord {
                path: input.display().to_string(),
                crc32: d.crc32.clone(),
                sha1: d.sha1.clone(),
                md5: d.md5.clone(),
                sha256: d.sha256.clone(),
                size_bytes: d.size_bytes,
                status: FileStatus::Ok,
                elapsed_ms,
                error: None,
            };
            (record, Ok(d.size_bytes))
        }
        Err(e) => {
            log::warn!("Failed to hash {}: {e}", input.display());
            tally.record_failed();
            let record = HashReportRecord {
                path: input.display().to_string(),
                crc32: None,
                sha1: None,
                md5: None,
                sha256: None,
                size_bytes: 0,
                status: FileStatus::Failed,
                elapsed_ms,
                error: Some(e.to_string()),
            };
            (record, Err(e))
        }
    };

    if let Some(path) = report {
        let totals = ReportTotals {
            total_files: 1,
            ok: outcome.is_ok() as usize,
            failed: outcome.is_err() as usize,
            total_input_bytes: *outcome.as_ref().unwrap_or(&0),
            elapsed_ms,
            ..ReportTotals::default()
        };
        write_hash_report(path, &[record], &totals, ReportFormat::from_path(path))?;
    }

    match outcome {
        Ok(_) => {
            log_count_summary(tally.count(), tally);
            Ok(())
        }
        Err(_) => anyhow::bail!("failed to hash {}", input.display()),
    }
}

fn log_count_summary(count: usize, tally: Tally) {
    log::info!("{}", Tally::count_summary(count, tally.elapsed()));
}

pub(crate) fn print_hash_row(path: &Path, d: &FileDigests, algos: &[HashAlgo]) {
    let cells: Vec<String> = algos
        .iter()
        .map(|a| format!("{}={}", a.label(), d.value(*a).unwrap_or("")))
        .collect();
    log::info!("{}  {}", path.display(), cells.join("  "));
}

fn log_skipped(output: &Path) {
    log::info!("Skipped, output exists: {}", output.display());
}

fn log_kept_valid(output: &Path) {
    log::info!("Kept, output verified valid: {}", output.display());
}

fn log_rewriting_invalid(output: &Path) {
    log::info!(
        "Rewriting, output failed verification: {}",
        output.display()
    );
}

/// Single-file dry-run preview for an `overwrite-invalid` arm. The verify is
/// read-only, so it runs under dry-run to show whether the existing output
/// would be kept or rewritten. The synthesized decision feeds the existing
/// tally/report path so the plan counts match a real run.
#[allow(clippy::too_many_arguments)]
async fn dry_run_single_verify(
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
    policy: rom_converto_lib::util::ConflictPolicy,
    target: crate::util::OutputVerify,
    media: Option<&str>,
    missing_keys: Option<&str>,
    progress: &dyn rom_converto_lib::util::ProgressReporter,
    report: Option<&Path>,
) -> Result<()> {
    use crate::util::{VerifyOutcome, verify_existing_output};
    if policy != rom_converto_lib::util::ConflictPolicy::OverwriteInvalid || !desired.exists() {
        return dry_run_single(
            operation,
            input,
            desired,
            decision,
            media,
            missing_keys,
            report,
        );
    }
    let (synth, outcome) = match verify_existing_output(progress, desired, target).await {
        VerifyOutcome::Valid => (
            WriteDecision::Skip,
            rom_converto_lib::util::PlanDecision::KeepValid,
        ),
        VerifyOutcome::Invalid => (
            WriteDecision::Write(desired.to_path_buf()),
            rom_converto_lib::util::PlanDecision::RewriteInvalid,
        ),
    };
    dry_run::log_plan_decision(
        operation,
        input,
        desired,
        &synth,
        outcome,
        media,
        missing_keys,
    );
    let mut tally = Tally::new();
    dry_run::record(&mut tally, input, &synth);
    let records = [dry_run::report_record(operation, input, desired, &synth)];
    dry_run::finish(&tally, &records, report)
}

/// Emit the plan line, summary, and optional report for a single-file
/// dry-run, then return so the caller can short-circuit before the lib write.
fn dry_run_single(
    operation: &str,
    input: &Path,
    desired: &Path,
    decision: &WriteDecision,
    media: Option<&str>,
    missing_keys: Option<&str>,
    report: Option<&Path>,
) -> Result<()> {
    dry_run::log_plan(operation, input, desired, decision, media, missing_keys);
    let mut tally = Tally::new();
    dry_run::record(&mut tally, input, decision);
    let records = [dry_run::report_record(operation, input, desired, decision)];
    dry_run::finish(&tally, &records, report)
}

/// Plan a legacy-disc migration without writing anything. Recursive mode
/// mirrors [`migrate_disc_batch`]: it enumerates the legacy containers in
/// the top level of the directory (detected by content) and shows each
/// output landing next to its input. Single mode plans one file.
fn migrate_dry_run(
    input: &Path,
    explicit_output: Option<std::path::PathBuf>,
    recursive: bool,
    force: bool,
    allowed: &[LegacyFormat],
) -> Result<()> {
    if recursive {
        require_dir(input)?;
        let mut detected: Vec<(std::path::PathBuf, LegacyFormat)> = std::fs::read_dir(input)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter_map(|p| match detect_legacy_format(&p) {
                Ok(Some(fmt)) => Some((p, fmt)),
                _ => None,
            })
            .collect();
        detected.sort_by(|a, b| a.0.cmp(&b.0));
        if detected.is_empty() {
            anyhow::bail!("no GCZ, WIA, or NKit images found in {}", input.display());
        }
        for (path, fmt) in detected.iter().filter(|(_, f)| !allowed.contains(f)) {
            log::warn!(
                "Skipped {}: {} is a Wii disc image; use rvl migrate",
                path.display(),
                fmt.name()
            );
        }
        let inputs: Vec<std::path::PathBuf> = detected
            .into_iter()
            .filter(|(_, fmt)| allowed.contains(fmt))
            .map(|(p, _)| p)
            .collect();
        let mut tally = Tally::new();
        let mut records = Vec::with_capacity(inputs.len());
        for file in &inputs {
            let desired = derive_rvz_path(file);
            // Mirror migrate_disc_batch: an existing output without
            // --force is skipped, not overwritten, and never aborts the
            // plan.
            let decision = if !force && desired.exists() {
                WriteDecision::Skip
            } else {
                WriteDecision::Write(desired.clone())
            };
            dry_run::log_plan("migrate", file, &desired, &decision, None, None);
            dry_run::record(&mut tally, file, &decision);
            records.push(dry_run::report_record("migrate", file, &desired, &decision));
        }
        dry_run::finish(&tally, &records, None)
    } else {
        ensure_input_exists(input)?;
        match detect_legacy_format(input)? {
            None => anyhow::bail!(
                "input is not a GCZ, WIA, or NKit image; use compress for .iso/.gcm/.wbfs"
            ),
            Some(fmt) => ensure_format_allowed(fmt, allowed)?,
        }
        let policy = policy_of(crate::commands::ConflictPolicyArg::Error, force);
        let desired = explicit_output.unwrap_or_else(|| derive_rvz_path(input));
        let decision = resolve_output(&desired, policy)?;
        dry_run_single("migrate", input, &desired, &decision, None, None, None)
    }
}

/// Reject a legacy container the verify console gate does not accept, pointing
/// at `rvl verify`. A non-legacy input passes through to the normal magic check.
fn verify_gate(input: &Path, allowed: &[LegacyFormat]) -> Result<()> {
    if let Some(fmt) = detect_legacy_format(input)? {
        ensure_format_allowed_for(fmt, allowed, "rvl verify")?;
    }
    Ok(())
}

/// Resolve the compression knobs migrate exposes with the compress precedence:
/// flag over preset/config over built-in default.
fn resolve_migrate_opts(
    level: Option<i32>,
    chunk_size: Option<u32>,
    eff: &rom_converto_lib::config::DiscDefaults,
) -> RvzCompressOptions {
    RvzCompressOptions {
        compression_level: level
            .or(eff.level)
            .unwrap_or(RvzCompressOptions::default().compression_level),
        chunk_size: chunk_size
            .or(eff.chunk_size)
            .unwrap_or(RvzCompressOptions::default().chunk_size),
        ..RvzCompressOptions::default()
    }
}

/// Resolve `--input-checksum-min`/`--input-checksum-max` against the dat
/// config defaults, falling back to crc32/sha256 (compute crc32 first,
/// escalate up to sha256 if the matched entry needs it).
fn resolve_checksum_bounds(
    min: Option<&str>,
    max: Option<&str>,
    eff: &rom_converto_lib::config::DatDefaults,
) -> Result<ChecksumBounds> {
    let min = min
        .map(str::to_string)
        .or_else(|| eff.input_checksum_min.clone())
        .unwrap_or_else(|| "crc32".to_string());
    let max = max
        .map(str::to_string)
        .or_else(|| eff.input_checksum_max.clone())
        .unwrap_or_else(|| "sha256".to_string());
    let min = parse_checksum_bound(&min).map_err(|e| anyhow::anyhow!(e))?;
    let max = parse_checksum_bound(&max).map_err(|e| anyhow::anyhow!(e))?;
    ChecksumBounds::new(min, max).map_err(|e| anyhow::anyhow!(e))
}

/// Best-effort media label for a CHD dry-run plan line. ISO inputs read a
/// header to predict the disc kind; cue inputs imply a CD with no header probe.
fn chd_media_label(input: &Path) -> Option<String> {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("cue") => Some("CD".to_string()),
        Some("iso") => rom_converto_lib::util::iso9660::detect_disc_kind(input)
            .ok()
            .map(|k| k.label().to_string()),
        _ => None,
    }
}

/// Load an NX keyset, but under dry-run a missing keyfile is reported as a
/// plan note instead of aborting, so the preview still shows the resolved
/// paths and exits 0. Outside dry-run the missing-keyfile error is preserved.
fn load_keyset_for_plan(
    explicit: Option<&Path>,
    dry_run: bool,
) -> Result<(rom_converto_lib::nintendo::nx::KeySet, Option<String>)> {
    match load_keyset(explicit) {
        Ok(keys) => Ok((keys, None)),
        Err(e) if dry_run => Ok((
            rom_converto_lib::nintendo::nx::KeySet::default(),
            Some(e.to_string()),
        )),
        Err(e) => Err(e.into()),
    }
}

/// Dry-run preview for the CTR recursive arms. The lib batch functions own
/// their own walk and write with no CLI-level conflict policy, so the preview
/// re-derives each output path here and reports the conflict decision without
/// calling the lib batch. The detected media is omitted; the extension implies
/// the format.
fn dry_run_ctr_scan(
    operation: &str,
    files: &[std::path::PathBuf],
    output_dir: Option<&Path>,
    policy: rom_converto_lib::util::ConflictPolicy,
    derive: fn(&Path) -> std::path::PathBuf,
) -> Result<()> {
    let mut tally = Tally::new();
    for input in files {
        let desired = rom_converto_lib::util::place_in_dir(&derive(input), output_dir);
        let decision = resolve_output(&desired, policy)?;
        dry_run::log_plan(operation, input, &desired, &decision, None, None);
        dry_run::record(&mut tally, input, &decision);
    }
    log::info!("{}", tally.summary_line(TallyDirection::DryRun));
    Ok(())
}

/// Decompress targets a WBFS container when the resolved output path
/// carries a `.wbfs` extension; otherwise it writes a raw disc image.
fn wants_wbfs_output(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("wbfs"))
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let cli = Cli::parse();

    // Must run before logger init; otherwise log lines leak into stdout
    // and corrupt the generated completion script.
    if let Commands::ShellCompletions(cmd) = &cli.command {
        return run_shell_completions(cmd);
    }
    let (project_level, global_level) = logging::resolve_log_levels(cli.quiet, cli.verbose);

    let debug_file = match cli.debug_log.as_deref() {
        Some(path) => {
            let f = std::fs::File::create(path)
                .with_context(|| format!("cannot open debug log: {}", path.display()))?;
            Some(std::io::BufWriter::new(f))
        }
        None => None,
    };

    let mut builder = env_logger::builder();
    builder
        .filter_level(global_level)
        .filter_module("rom_converto", project_level)
        .filter_module("rom_converto_lib", project_level)
        .format_timestamp(None);
    if cli.verbose == 0 && !cli.quiet {
        builder.format_target(false);
        // At default verbosity ordinary info lines are user-facing
        // summaries, so they print as plain text; warnings and errors
        // keep a label so they still stand out.
        builder.format(|buf, record| {
            use std::io::Write;
            match record.level() {
                log::Level::Info => writeln!(buf, "{}", record.args()),
                level => writeln!(buf, "[{level}] {}", record.args()),
            }
        });
    }
    let console_logger = builder.parse_default_env().build();

    let pb = MultiProgress::new();

    let max_level = if debug_file.is_some() {
        log::LevelFilter::Trace
    } else {
        console_logger.filter()
    };

    match debug_file {
        Some(file) => {
            let dual = logging::DualLogger::new(console_logger, file);
            LogWrapper::new(pb.clone(), dual).try_init()?;
        }
        None => {
            LogWrapper::new(pb.clone(), console_logger).try_init()?;
        }
    }
    log::set_max_level(max_level);

    cleanup_old_executable().await?;

    let mut github = GithubApi::new()?;

    if discriminant(&cli.command) != discriminant(&Commands::SelfUpdate(SelfUpdateCommand {}))
        && should_check_for_updates(cli.no_update_check)
    {
        // Non-fatal: network outages or GitHub rate limits shouldn't
        // prevent the user from running conversions offline.
        if let Err(e) = check_for_new_version_and_notify(&mut github).await {
            log::debug!("Update check skipped: {e}");
        }
    }

    let progress = IndicatifProgress::new(pb.clone());
    let total_progress = TotalProgress::new(pb);

    let user_config = rom_converto_lib::config::load_config(cli.config.as_deref())?;
    let preset = rom_converto_lib::config::resolve_preset(&user_config, cli.preset.as_deref())?;
    let effective = config::resolve(&user_config, preset.as_ref());
    let dry_run = cli.dry_run;
    let skip_space_check = cli.skip_space_check;
    let cache = rom_converto_lib::util::HashCache::load(cli.no_cache, cli.rebuild_cache);

    let cancel = rom_converto_lib::util::CancelToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        });
    }

    let dispatch = dispatch_command(
        cli.command,
        progress,
        total_progress,
        &effective,
        dry_run,
        skip_space_check,
        cancel.clone(),
        &mut github,
        &cache,
    )
    .await;

    cache.save();
    log::logger().flush();

    if let Err(err) = dispatch {
        if cancel.is_cancelled() && is_cancelled_error(&err) {
            eprintln!("Cancelled");
            std::process::exit(130);
        }
        return Err(err);
    }
    Ok(())
}

/// True when the error chain bottoms out at one of the codec
/// `Cancelled` variants, so a Ctrl-C abort is reported distinctly
/// rather than as a generic failure.
fn is_cancelled_error(err: &anyhow::Error) -> bool {
    use rom_converto_lib::chd::error::ChdError;
    use rom_converto_lib::cso::CsoError;
    use rom_converto_lib::dat::DatError;
    use rom_converto_lib::nintendo::ctr::error::NintendoCTRError;
    use rom_converto_lib::nintendo::ctr::z3ds::error::Z3dsError;
    use rom_converto_lib::nintendo::nx::NxError;
    use rom_converto_lib::nintendo::rvz::RvzError;
    use rom_converto_lib::nintendo::wup::WupError;

    err.chain().any(|cause| {
        matches!(cause.downcast_ref::<ChdError>(), Some(ChdError::Cancelled))
            || matches!(cause.downcast_ref::<CsoError>(), Some(CsoError::Cancelled))
            || matches!(cause.downcast_ref::<RvzError>(), Some(RvzError::Cancelled))
            || matches!(cause.downcast_ref::<NxError>(), Some(NxError::Cancelled))
            || matches!(
                cause.downcast_ref::<Z3dsError>(),
                Some(Z3dsError::Cancelled)
            )
            || matches!(
                cause.downcast_ref::<NintendoCTRError>(),
                Some(NintendoCTRError::Cancelled)
            )
            || matches!(cause.downcast_ref::<WupError>(), Some(WupError::Cancelled))
            || matches!(cause.downcast_ref::<DatError>(), Some(DatError::Cancelled))
    })
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn dispatch_command(
    command: Commands,
    progress: IndicatifProgress,
    total_progress: TotalProgress,
    effective: &config::Effective,
    dry_run: bool,
    skip_space_check: bool,
    cancel: rom_converto_lib::util::CancelToken,
    github: &mut GithubApi,
    cache: &rom_converto_lib::util::HashCache,
) -> Result<()> {
    match command {
        Commands::Ctr(inner) => match inner {
            CtrCommands::CdnToCia(cmd) => {
                let mut output = cmd.output_flag.or(cmd.output);
                let mut output_dir = cmd.output_dir;
                if cmd.recursive && dry_run {
                    ensure_input_exists(&cmd.cdn_dir)?;
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let mut tally = Tally::new();
                    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&cmd.cdn_dir)?
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.is_dir()
                                && p.file_name()
                                    .and_then(|n| n.to_str())
                                    .is_none_or(|n| !is_os_junk_dir(n))
                        })
                        .collect();
                    dirs.sort();
                    for dir in &dirs {
                        let name = dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| format!("{n}.cia"))
                            .unwrap_or_else(|| "output.cia".to_string());
                        let base = rom_converto_lib::util::place_in_dir(
                            &dir.parent().unwrap_or_else(|| Path::new(".")).join(name),
                            output_dir.as_deref(),
                        );
                        let resolved = if cmd.compress {
                            derive_compressed_path(&base)
                        } else {
                            base
                        };
                        let decision = resolve_output(&resolved, policy)?;
                        dry_run::log_plan("convert", dir, &resolved, &decision, None, None);
                        dry_run::record(&mut tally, dir, &decision);
                    }
                    log::info!("{}", tally.summary_line(TallyDirection::DryRun));
                    return Ok(());
                }
                if !cmd.recursive {
                    ensure_input_exists(&cmd.cdn_dir)?;
                    let base = match output.clone() {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            let name = cmd
                                .cdn_dir
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| format!("{n}.cia"))
                                .unwrap_or_else(|| "output.cia".to_string());
                            let derived = cmd
                                .cdn_dir
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .join(name);
                            rom_converto_lib::util::place_in_dir(&derived, output_dir.as_deref())
                        }
                    };
                    let resolved = if cmd.compress {
                        derive_compressed_path(&base)
                    } else {
                        base.clone()
                    };
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let decision = resolve_output(&resolved, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "convert",
                            &cmd.cdn_dir,
                            &resolved,
                            &decision,
                            None,
                            None,
                            None,
                        );
                    }
                    match decision {
                        WriteDecision::Skip => {
                            log_skipped(&resolved);
                            return Ok(());
                        }
                        WriteDecision::Write(p) if p != resolved => {
                            // rename redirected the write; pin the lib to the
                            // free path and drop output_dir so it is not re-rooted.
                            output = Some(if cmd.compress {
                                derive_decompressed_path(&p)
                            } else {
                                p
                            });
                            output_dir = None;
                        }
                        WriteDecision::Write(_) => {}
                    }
                }
                let opts = CdnToCiaOptions {
                    cdn_dir: cmd.cdn_dir,
                    output,
                    cleanup: cmd.cleanup,
                    recursive: cmd.recursive,
                    ensure_ticket_exists: cmd.ensure_ticket_exists,
                    decrypt: cmd.decrypt,
                    compress: cmd.compress,
                    output_dir,
                    on_conflict: policy_of(cmd.on_conflict, cmd.force),
                };
                convert_cdn_to_cia_cancellable(opts, &progress, &total_progress, cancel.clone())
                    .await?
            }
            CtrCommands::GenerateCdnTicket(cmd) => {
                ensure_input_exists(&cmd.cdn_dir)?;
                if dry_run {
                    let decision = WriteDecision::Write(cmd.output.clone());
                    return dry_run_single(
                        "generate ticket",
                        &cmd.cdn_dir,
                        &cmd.output,
                        &decision,
                        None,
                        None,
                        None,
                    );
                }
                generate_ticket_from_cdn(&cmd.cdn_dir, &cmd.output).await?
            }
            CtrCommands::Decrypt(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let files =
                        collect_files_with_exts(&cmd.input, CTR_DECRYPT_EXTS, cmd.max_depth)?;
                    if dry_run {
                        dry_run_ctr_scan(
                            "decrypt",
                            &files,
                            cmd.output_dir.as_deref(),
                            policy_of(cmd.on_conflict, cmd.force),
                            derive_decrypted_path,
                        )?;
                        return Ok(());
                    }
                    if !skip_space_check {
                        let check_dir = cmd.output_dir.as_deref().unwrap_or(&cmd.input);
                        batch::space_preflight(&files, check_dir)?;
                    }
                    let tally = Tally::new();
                    let count = files.len();
                    decrypt_rom_batch_cancellable(
                        &cmd.input,
                        cmd.output_dir.as_deref(),
                        &progress,
                        &total_progress,
                        cmd.max_depth,
                        cancel.clone(),
                    )
                    .await?;
                    log_count_summary(count, tally);
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, CTR_DECRYPT_EXTS)?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    cmd.output_dir.as_deref(),
                                    derive_decrypted_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_decrypted_path(resolved.output_basis()),
                                    cmd.output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "decrypt", &cmd.input, &output, &decision, None, None, None,
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    decrypt_rom_cancellable(input, &output, &progress, cancel.clone()).await?;
                    log_single_summary(&cmd.input, &output, TallyDirection::Convert, started);
                }
            }
            CtrCommands::Encrypt(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let files =
                        collect_files_with_exts(&cmd.input, CTR_ENCRYPT_EXTS, cmd.max_depth)?;
                    if dry_run {
                        dry_run_ctr_scan(
                            "encrypt",
                            &files,
                            cmd.output_dir.as_deref(),
                            policy_of(cmd.on_conflict, cmd.force),
                            derive_encrypted_path,
                        )?;
                        return Ok(());
                    }
                    if !skip_space_check {
                        let check_dir = cmd.output_dir.as_deref().unwrap_or(&cmd.input);
                        batch::space_preflight(&files, check_dir)?;
                    }
                    let tally = Tally::new();
                    let count = files.len();
                    encrypt_rom_batch_cancellable(
                        &cmd.input,
                        cmd.output_dir.as_deref(),
                        &progress,
                        &total_progress,
                        cmd.max_depth,
                        cancel.clone(),
                    )
                    .await?;
                    log_count_summary(count, tally);
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, CTR_ENCRYPT_EXTS)?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    cmd.output_dir.as_deref(),
                                    derive_encrypted_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_encrypted_path(resolved.output_basis()),
                                    cmd.output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "encrypt", &cmd.input, &output, &decision, None, None, None,
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    encrypt_rom_cancellable(input, &output, &progress, cancel.clone()).await?;
                    log_single_summary(&cmd.input, &output, TallyDirection::Convert, started);
                }
            }
            CtrCommands::Compress(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let files =
                        collect_files_with_exts(&cmd.input, CTR_COMPRESS_EXTS, cmd.max_depth)?;
                    if dry_run {
                        dry_run_ctr_scan(
                            "compress",
                            &files,
                            cmd.output_dir.as_deref(),
                            policy_of(cmd.on_conflict, cmd.force),
                            derive_compressed_path,
                        )?;
                        return Ok(());
                    }
                    if !skip_space_check {
                        let check_dir = cmd.output_dir.as_deref().unwrap_or(&cmd.input);
                        batch::space_preflight(&files, check_dir)?;
                    }
                    let tally = Tally::new();
                    let count = files.len();
                    compress_rom_batch(
                        &cmd.input,
                        cmd.level,
                        cmd.output_dir.as_deref(),
                        &progress,
                        &total_progress,
                        cmd.max_depth,
                        cmd.allow_encrypted,
                    )
                    .await?;
                    log_count_summary(count, tally);
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, CTR_COMPRESS_EXTS)?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    cmd.output_dir.as_deref(),
                                    derive_compressed_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_compressed_path(resolved.output_basis()),
                                    cmd.output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "compress", &cmd.input, &output, &decision, None, None, None,
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    compress_rom_cancellable(
                        input,
                        &output,
                        cmd.level,
                        cmd.allow_encrypted,
                        &progress,
                        cancel.clone(),
                    )
                    .await?;
                    log_single_summary(&cmd.input, &output, TallyDirection::Compress, started);
                }
            }
            CtrCommands::Decompress(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let files =
                        collect_files_with_exts(&cmd.input, CTR_DECOMPRESS_EXTS, cmd.max_depth)?;
                    if dry_run {
                        dry_run_ctr_scan(
                            "decompress",
                            &files,
                            cmd.output_dir.as_deref(),
                            policy_of(cmd.on_conflict, cmd.force),
                            derive_decompressed_path,
                        )?;
                        return Ok(());
                    }
                    if !skip_space_check {
                        let check_dir = cmd.output_dir.as_deref().unwrap_or(&cmd.input);
                        batch::space_preflight(&files, check_dir)?;
                    }
                    let tally = Tally::new();
                    let count = files.len();
                    decompress_rom_batch(
                        &cmd.input,
                        cmd.output_dir.as_deref(),
                        &progress,
                        &total_progress,
                        cmd.max_depth,
                    )
                    .await?;
                    log_count_summary(count, tally);
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, CTR_DECOMPRESS_EXTS)?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    cmd.output_dir.as_deref(),
                                    derive_decompressed_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_decompressed_path(resolved.output_basis()),
                                    cmd.output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "decompress",
                            &cmd.input,
                            &output,
                            &decision,
                            None,
                            None,
                            None,
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    decompress_rom_cancellable(input, &output, &progress, cancel.clone()).await?;
                    log_single_summary(&cmd.input, &output, TallyDirection::Decompress, started);
                }
            }
            CtrCommands::Convert(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let files =
                        collect_files_with_exts(&cmd.input, CTR_CONVERT_EXTS, cmd.max_depth)?;
                    if dry_run {
                        dry_run_ctr_scan(
                            "convert",
                            &files,
                            cmd.output_dir.as_deref(),
                            policy_of(cmd.on_conflict, cmd.force),
                            derive_converted_path,
                        )?;
                        return Ok(());
                    }
                    if !skip_space_check {
                        let check_dir = cmd.output_dir.as_deref().unwrap_or(&cmd.input);
                        batch::space_preflight(&files, check_dir)?;
                    }
                    let tally = Tally::new();
                    let count = files.len();
                    convert_rom_batch_cancellable(
                        &cmd.input,
                        cmd.output_dir.as_deref(),
                        &progress,
                        &total_progress,
                        cmd.max_depth,
                        cancel.clone(),
                    )
                    .await?;
                    log_count_summary(count, tally);
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, CTR_CONVERT_EXTS)?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    cmd.output_dir.as_deref(),
                                    derive_converted_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_converted_path(resolved.output_basis()),
                                    cmd.output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = policy_of(cmd.on_conflict, cmd.force);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "convert", &cmd.input, &output, &decision, None, None, None,
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    convert_rom_cancellable(input, &output, &progress, cancel.clone()).await?;
                    log_single_summary(&cmd.input, &output, TallyDirection::Convert, started);
                }
            }
            CtrCommands::Verify(cmd) => {
                let opts = CtrVerifyOptions {
                    verify_content_hashes: cmd.verify_content,
                };
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    let summary = verify_ctr_batch(
                        &cmd.input,
                        &opts,
                        &progress,
                        &total_progress,
                        cmd.max_depth,
                    )
                    .await?;
                    log::info!(
                        "Verified {} files: {} OK, {} failed",
                        summary.total,
                        summary.ok,
                        summary.failed
                    );
                    if summary.failed > 0 {
                        anyhow::bail!("verification failed");
                    }
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, CTR_DECRYPT_EXTS)?;
                    let result = verify_ctr(resolved.path(), &opts, &progress).await?;
                    match &result {
                        CtrVerifyResult::Cia(cia) => {
                            log::info!("Format: CIA");
                            log::info!("Legitimacy: {}", cia.legitimacy);
                            if cia.compressed {
                                log::info!("Compressed: yes");
                            }
                            for line in &cia.details {
                                log::info!("  {line}");
                            }
                        }
                        CtrVerifyResult::Ncsd(ncsd) => {
                            log::info!("Format: NCSD");
                            log::info!("Title ID: {}", ncsd.title_id);
                            if ncsd.compressed {
                                log::info!("Compressed: yes");
                            }
                            for line in &ncsd.details {
                                log::info!("  {line}");
                            }
                            for part in &ncsd.partitions {
                                log::info!(
                                    "  Partition {} ({}): {}",
                                    part.index,
                                    part.name,
                                    if part.ncch_magic_valid {
                                        "NCCH OK"
                                    } else {
                                        "NCCH INVALID"
                                    }
                                );
                                for line in &part.details {
                                    log::info!("    {line}");
                                }
                            }
                        }
                    }
                    if !result.ok() {
                        anyhow::bail!("verification failed");
                    }
                }
            }
            CtrCommands::Info(cmd) => {
                if cmd.keys.is_some() {
                    anyhow::bail!("--keys is only supported by nx and wup info");
                }
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::nintendo::ctr::info::read_info(resolved.path())?;
                if let Some(dir) = &cmd.save_icon {
                    save_ctr_icon(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Ctr(info), cmd.json)?;
            }
        },
        Commands::Dol(inner) => match inner {
            DolCommands::Compress(cmd) => {
                let eff = &effective.dol;
                let opts = RvzCompressOptions {
                    compression_level: cmd
                        .level
                        .or(eff.level)
                        .unwrap_or(RvzCompressOptions::default().compression_level),
                    chunk_size: cmd
                        .chunk_size
                        .or(eff.chunk_size)
                        .unwrap_or(RvzCompressOptions::default().chunk_size),
                    ..RvzCompressOptions::default()
                };
                if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                    log::warn!("{msg}");
                }
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::rvz_compress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        &["iso", "gcm"],
                        opts,
                        resolve_policy(cmd.on_conflict, cmd.force, fallback),
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["iso", "gcm", "gcz"])?;
                    let input = resolved.path();
                    if let Some(fmt) = detect_legacy_format(input)? {
                        ensure_format_allowed(fmt, DOL_MIGRATE_FORMATS)?;
                    }
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "rvz",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_rvz_path(resolved.output_basis()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Rvz,
                            Some("RVZ"),
                            None,
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Rvz,
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    compress_disc_cancellable(input, &output, opts, &progress, cancel.clone())
                        .await?;
                    finish_single(
                        &cmd.input,
                        &output,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            DolCommands::Migrate(cmd) => {
                let opts = resolve_migrate_opts(cmd.level, cmd.chunk_size, &effective.dol);
                if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                    log::warn!("{msg}");
                }
                let migrate_opts = MigrateOptions {
                    skip_verify: cmd.skip_verify,
                    deep_verify: false,
                };
                if dry_run {
                    return migrate_dry_run(
                        &cmd.input,
                        cmd.output_flag.or(cmd.output),
                        cmd.recursive,
                        cmd.force,
                        DOL_MIGRATE_FORMATS,
                    );
                }
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    migrate_disc_batch(
                        &cmd.input,
                        opts,
                        migrate_opts,
                        DOL_MIGRATE_FORMATS,
                        cmd.force,
                        &progress,
                        cancel.clone(),
                    )
                    .await?;
                } else {
                    let output = cmd
                        .output_flag
                        .or(cmd.output)
                        .unwrap_or_else(|| derive_rvz_path(&cmd.input));
                    let policy = policy_of(crate::commands::ConflictPolicyArg::Error, cmd.force);
                    let output = match resolve_output(&output, policy)? {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    migrate_disc_cancellable(
                        &cmd.input,
                        &output,
                        opts,
                        migrate_opts,
                        DOL_MIGRATE_FORMATS,
                        &progress,
                        cancel.clone(),
                    )
                    .await?
                }
            }
            DolCommands::Decompress(cmd) => {
                let eff = &effective.dol;
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::rvz_decompress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        resolve_policy(cmd.on_conflict, cmd.force, fallback),
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["rvz"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "iso",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_disc_path(resolved.output_basis()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "decompress",
                            &cmd.input,
                            &output,
                            &decision,
                            None,
                            None,
                            report.as_deref(),
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    if wants_wbfs_output(&output) {
                        decompress_disc_to_wbfs_cancellable(
                            input,
                            &output,
                            &progress,
                            cancel.clone(),
                        )
                        .await?
                    } else {
                        decompress_disc_cancellable(input, &output, &progress, cancel.clone())
                            .await?
                    }
                    finish_single(
                        &cmd.input,
                        &output,
                        TallyDirection::Decompress,
                        "decompress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            DolCommands::Verify(cmd) => {
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::dol_verify(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        cmd.full,
                        cmd.max_depth,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(
                        &cmd.input,
                        &["iso", "gcm", "gcz", "rvz"],
                    )?;
                    let input = resolved.path();
                    verify_gate(input, DOL_MIGRATE_FORMATS)?;
                    let opts = DolVerifyOptions { full: cmd.full };
                    let result = verify_dol(input, &opts, &progress)?;
                    log::info!("Game ID: {}", result.game_id);
                    print_rvz_structure(result.rvz_structure.as_ref());
                    if let Some(st) = &result.structural {
                        log::info!("FST within bounds: {}", ok_str(st.fst_within_bounds));
                        for n in &st.notes {
                            log::info!("  {n}");
                        }
                    }
                    if let Some(d) = &result.disc_sha1 {
                        log::info!("Whole-disc SHA-1: {d}");
                    }
                    log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                    if !result.ok {
                        anyhow::bail!("verification failed");
                    }
                }
            }
            DolCommands::Info(cmd) => {
                if cmd.keys.is_some() {
                    anyhow::bail!("--keys is only supported by nx and wup info");
                }
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::nintendo::dol::info::read_info(resolved.path())?;
                if let Some(dir) = &cmd.save_icon {
                    save_dol_banner(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Dol(info), cmd.json)?;
            }
        },
        Commands::Rvl(inner) => match inner {
            RvlCommands::Compress(cmd) => {
                let eff = &effective.rvl;
                let opts = RvzCompressOptions {
                    compression_level: cmd
                        .level
                        .or(eff.level)
                        .unwrap_or(RvzCompressOptions::default().compression_level),
                    chunk_size: cmd
                        .chunk_size
                        .or(eff.chunk_size)
                        .unwrap_or(RvzCompressOptions::default().chunk_size),
                    ..RvzCompressOptions::default()
                };
                if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                    log::warn!("{msg}");
                }
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::rvz_compress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        &["iso", "wbfs"],
                        opts,
                        resolve_policy(cmd.on_conflict, cmd.force, fallback),
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(
                        &cmd.input,
                        &["iso", "wbfs", "gcz", "wia"],
                    )?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "rvz",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_rvz_path(resolved.output_basis()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Rvz,
                            Some("RVZ"),
                            None,
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Rvz,
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    compress_disc_cancellable(input, &output, opts, &progress, cancel.clone())
                        .await?;
                    finish_single(
                        &cmd.input,
                        &output,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            RvlCommands::Migrate(cmd) => {
                let opts = resolve_migrate_opts(cmd.level, cmd.chunk_size, &effective.rvl);
                if let Some(msg) = oversized_rvz_chunk(opts.chunk_size) {
                    log::warn!("{msg}");
                }
                let migrate_opts = MigrateOptions {
                    skip_verify: cmd.skip_verify,
                    deep_verify: cmd.deep,
                };
                if dry_run {
                    return migrate_dry_run(
                        &cmd.input,
                        cmd.output_flag.or(cmd.output),
                        cmd.recursive,
                        cmd.force,
                        ALL_MIGRATE_FORMATS,
                    );
                }
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    migrate_disc_batch(
                        &cmd.input,
                        opts,
                        migrate_opts,
                        ALL_MIGRATE_FORMATS,
                        cmd.force,
                        &progress,
                        cancel.clone(),
                    )
                    .await?;
                } else {
                    let output = cmd
                        .output_flag
                        .or(cmd.output)
                        .unwrap_or_else(|| derive_rvz_path(&cmd.input));
                    let policy = policy_of(crate::commands::ConflictPolicyArg::Error, cmd.force);
                    let output = match resolve_output(&output, policy)? {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    migrate_disc_cancellable(
                        &cmd.input,
                        &output,
                        opts,
                        migrate_opts,
                        ALL_MIGRATE_FORMATS,
                        &progress,
                        cancel.clone(),
                    )
                    .await?
                }
            }
            RvlCommands::Decompress(cmd) => {
                let eff = &effective.rvl;
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::rvz_decompress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        resolve_policy(cmd.on_conflict, cmd.force, fallback),
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["rvz"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "iso",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &derive_disc_path(resolved.output_basis()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "decompress",
                            &cmd.input,
                            &output,
                            &decision,
                            None,
                            None,
                            report.as_deref(),
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let started = Instant::now();
                    if wants_wbfs_output(&output) {
                        decompress_disc_to_wbfs_cancellable(
                            input,
                            &output,
                            &progress,
                            cancel.clone(),
                        )
                        .await?
                    } else {
                        decompress_disc_cancellable(input, &output, &progress, cancel.clone())
                            .await?
                    }
                    finish_single(
                        &cmd.input,
                        &output,
                        TallyDirection::Decompress,
                        "decompress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            RvlCommands::Verify(cmd) => {
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::rvl_verify(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        cmd.full,
                        cmd.max_depth,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(
                        &cmd.input,
                        &["iso", "wbfs", "gcz", "wia", "rvz"],
                    )?;
                    let input = resolved.path();
                    verify_gate(input, ALL_MIGRATE_FORMATS)?;
                    let opts = RvlVerifyOptions { full: cmd.full };
                    let result = verify_rvl(input, &opts, &progress)?;
                    log::info!("Game ID: {}", result.game_id);
                    print_rvz_structure(result.rvz_structure.as_ref());
                    if result.rvz_structure.is_none() && !cmd.full {
                        log::info!(
                            "No RVZ container hashes to check; pass --full to verify the partition hash tree"
                        );
                    }
                    for p in &result.partitions {
                        log::info!(
                            "  Partition @0x{:X} ({}): {} ({} clusters, {} mismatched)",
                            p.offset,
                            p.kind,
                            if p.ok { "OK" } else { "FAIL" },
                            p.clusters_checked,
                            p.mismatched_clusters
                        );
                        if p.scrubbed_clusters > 0 {
                            log::info!(
                                "    {} scrubbed clusters skipped (zero-filled by the dump tool)",
                                p.scrubbed_clusters
                            );
                        }
                        if let Some(note) = &p.note {
                            log::info!("    {note}");
                        }
                        if !p.sample_bad_clusters.is_empty() {
                            log::info!("    bad clusters: {:?}", p.sample_bad_clusters);
                        }
                    }
                    log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                    if !result.ok {
                        anyhow::bail!("verification failed");
                    }
                }
            }
            RvlCommands::Info(cmd) => {
                if cmd.keys.is_some() {
                    anyhow::bail!("--keys is only supported by nx and wup info");
                }
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::nintendo::rvl::info::read_info(resolved.path())?;
                if let Some(dir) = &cmd.save_icon {
                    save_rvl_image(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Rvl(info), cmd.json)?;
            }
        },
        Commands::Wup(inner) => match inner {
            WupCommands::Compress(cmd) => {
                let eff = &effective.wup;
                let policy = resolve_policy(
                    cmd.on_conflict,
                    cmd.force,
                    config::policy_fallback(&eff.on_conflict)?,
                );
                let decision = resolve_output(&cmd.output, policy)?;
                if dry_run {
                    use rom_converto_lib::nintendo::wup::compress::TitleInputFormat;
                    let media = cmd
                        .inputs
                        .first()
                        .and_then(|p| {
                            rom_converto_lib::nintendo::wup::compress::detect_title_format(p).ok()
                        })
                        .map(|f| match f {
                            TitleInputFormat::Loadiine => "Loadiine",
                            TitleInputFormat::Nus => "NUS",
                            TitleInputFormat::Disc => "disc",
                        });
                    let input = cmd
                        .inputs
                        .first()
                        .cloned()
                        .unwrap_or_else(|| cmd.output.clone());
                    return dry_run_single(
                        "compress",
                        &input,
                        &cmd.output,
                        &decision,
                        media,
                        None,
                        None,
                    );
                }
                let output = match decision {
                    WriteDecision::Skip => {
                        log_skipped(&cmd.output);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                if !skip_space_check {
                    let required: u64 = cmd.inputs.iter().map(|p| file_len(p)).sum();
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                let opts = WupCompressOptions {
                    zstd_level: cmd
                        .level
                        .or(eff.level)
                        .unwrap_or(WupCompressOptions::default().zstd_level),
                };
                // Pair --key values with disc inputs in positional
                // order. Non-disc inputs skip past their key slot.
                let mut key_iter = cmd.key.into_iter();
                let titles: Vec<TitleInput> = cmd
                    .inputs
                    .into_iter()
                    .map(|p| {
                        let is_disc = p
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.eq_ignore_ascii_case("wud") || s.eq_ignore_ascii_case("wux"))
                            .unwrap_or(false)
                            && p.is_file();
                        let mut t = TitleInput::auto(p);
                        if is_disc {
                            t.key_path = key_iter.next();
                        }
                        t
                    })
                    .collect();
                compress_titles_async_cancellable(titles, output, opts, &progress, cancel.clone())
                    .await?
            }
            WupCommands::Decrypt(cmd) => {
                ensure_input_exists(&cmd.input)?;
                let policy = policy_of(cmd.on_conflict, cmd.force);
                let decision = resolve_output_dir(&cmd.output, policy)?;
                if dry_run {
                    return dry_run_single(
                        "decrypt",
                        &cmd.input,
                        &cmd.output,
                        &decision,
                        None,
                        None,
                        None,
                    );
                }
                match decision {
                    WriteDecision::Skip => {
                        log_skipped(&cmd.output);
                        return Ok(());
                    }
                    WriteDecision::Write(_) => {}
                }
                if !skip_space_check {
                    batch::space_preflight_for_size(file_len(&cmd.input), &cmd.output)?;
                }
                decrypt_nus_title_async_cancellable(
                    cmd.input,
                    cmd.output,
                    &progress,
                    cancel.clone(),
                )
                .await?
            }
            WupCommands::Verify(cmd) => {
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::wup_verify(&progress, &total_progress, &cmd.input, cmd.max_depth).await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["wud", "wux"])?;
                    let result =
                        verify_wup_async(resolved.path().to_path_buf(), cmd.key, &progress).await?;
                    log::info!("Source kind: {}", result.kind);
                    log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                    for t in &result.titles {
                        log::info!(
                            "  {}: {} (verified: {}, mismatched: {}, skipped: {})",
                            t.title_id_hex,
                            if t.ok { "OK" } else { "FAIL" },
                            t.verified_content,
                            t.mismatched_content,
                            t.skipped_content
                        );
                    }
                    if !result.ok {
                        anyhow::bail!("verification failed");
                    }
                }
            }
            WupCommands::Info(cmd) => {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::nintendo::wup::info::read_info(
                    resolved.path(),
                    cmd.keys.as_deref(),
                )?;
                if let Some(dir) = &cmd.save_icon {
                    save_wup_image(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Wup(info), cmd.json)?;
            }
        },
        Commands::Nx(inner) => match inner {
            NxCommands::Compress(cmd) => {
                let eff = &effective.nx;
                let (keys, keys_note) = load_keyset_for_plan(cmd.keys.as_deref(), dry_run)?;
                let level = cmd.level.or(eff.level);
                let mode = cmd.mode.clone().or_else(|| eff.mode.clone());
                let block_size_exp = cmd.block_size_exp.or(eff.block_size_exp);
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive && dry_run {
                    require_dir(&cmd.input)?;
                    let files = rom_converto_lib::util::fs::collect_files_with_exts(
                        &cmd.input,
                        &["nsp", "xci"],
                        cmd.max_depth,
                    )?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let mut tally = Tally::new();
                    for input in &files {
                        let desired = crate::util::batch_output(
                            input,
                            &nx_derive_compressed_path(input),
                            &cmd.input,
                            output_dir.as_deref(),
                            cmd.output_template.as_deref(),
                            nx_derive_compressed_path(input)
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or(""),
                            cmd.keys.as_deref(),
                            true,
                        )?;
                        let decision = resolve_output(&desired, policy)?;
                        let media = detect_container(input).ok().map(|k| format!("{k:?}"));
                        dry_run::log_plan(
                            "compress",
                            input,
                            &desired,
                            &decision,
                            media.as_deref(),
                            keys_note.as_deref(),
                        );
                        dry_run::record(&mut tally, input, &decision);
                    }
                    log::info!("{}", tally.summary_line(TallyDirection::DryRun));
                    return Ok(());
                }
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let tuning = batch::NxCompressTuning {
                        level,
                        mode,
                        block_size_exp,
                        policy: resolve_policy(cmd.on_conflict, cmd.force, fallback),
                        output_dir,
                        output_template: cmd.output_template,
                        max_depth: cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report,
                    };
                    batch::nx_compress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        keys,
                        tuning,
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["nsp", "xci", "nca"])?;
                    let input = resolved.path();
                    let kind = detect_container(input)?;
                    let mut opts = NxCompressOptions::for_kind(kind);
                    if let Some(level) = level {
                        opts.level = level;
                    }
                    if let Some(mode) = mode.as_deref() {
                        opts.mode = match mode {
                            "solid" => NczMode::Solid,
                            "block" => NczMode::Block {
                                size_exp: block_size_exp.unwrap_or(20),
                            },
                            _ => unreachable!("clap value_parser already validated"),
                        };
                    } else if let Some(exp) = block_size_exp {
                        opts.mode = NczMode::Block { size_exp: exp };
                    }
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    nx_derive_compressed_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    cmd.keys.as_deref(),
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &nx_derive_compressed_path(resolved.output_basis()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Nx(Box::new(keys.clone())),
                            Some(&format!("{kind:?}")),
                            keys_note.as_deref(),
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Nx(Box::new(keys.clone())),
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let in_path = input.to_path_buf();
                    let out_path = output.clone();
                    let started = Instant::now();
                    compress_container_async_cancellable(
                        in_path.clone(),
                        output,
                        opts,
                        keys,
                        &progress,
                        cancel.clone(),
                    )
                    .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            NxCommands::Decompress(cmd) => {
                let eff = &effective.nx;
                let (keys, keys_note) = load_keyset_for_plan(cmd.keys.as_deref(), dry_run)?;
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive && dry_run {
                    require_dir(&cmd.input)?;
                    let files = rom_converto_lib::util::fs::collect_files_with_exts(
                        &cmd.input,
                        &["nsz", "xcz"],
                        cmd.max_depth,
                    )?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let mut tally = Tally::new();
                    for input in &files {
                        let desired = crate::util::batch_output(
                            input,
                            &nx_derive_decompressed_path(input),
                            &cmd.input,
                            output_dir.as_deref(),
                            cmd.output_template.as_deref(),
                            nx_derive_decompressed_path(input)
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or(""),
                            cmd.keys.as_deref(),
                            true,
                        )?;
                        let decision = resolve_output(&desired, policy)?;
                        dry_run::log_plan(
                            "decompress",
                            input,
                            &desired,
                            &decision,
                            None,
                            keys_note.as_deref(),
                        );
                        dry_run::record(&mut tally, input, &decision);
                    }
                    log::info!("{}", tally.summary_line(TallyDirection::DryRun));
                    return Ok(());
                }
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::nx_decompress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        keys,
                        resolve_policy(cmd.on_conflict, cmd.force, fallback),
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["nsz", "xcz", "ncz"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    nx_derive_decompressed_path(resolved.output_basis())
                                        .extension()
                                        .and_then(|e| e.to_str())
                                        .unwrap_or(""),
                                    cmd.keys.as_deref(),
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &nx_derive_decompressed_path(resolved.output_basis()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "decompress",
                            &cmd.input,
                            &output,
                            &decision,
                            None,
                            keys_note.as_deref(),
                            report.as_deref(),
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let in_path = input.to_path_buf();
                    let out_path = output.clone();
                    let started = Instant::now();
                    decompress_container_async_cancellable(
                        in_path.clone(),
                        output,
                        keys,
                        &progress,
                        cancel.clone(),
                    )
                    .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Decompress,
                        "decompress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            NxCommands::Verify(cmd) => {
                let keys = load_keyset(cmd.keys.as_deref())?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::nx_verify(&progress, &total_progress, &cmd.input, keys, cmd.max_depth)
                        .await?;
                    return Ok(());
                }
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(
                    &cmd.input,
                    &["nsp", "xci", "nca", "nsz", "xcz", "ncz"],
                )?;
                let result =
                    verify_container_async(resolved.path().to_path_buf(), keys, &progress).await?;
                log::info!("Container kind: {}", result.kind);
                log::info!("Overall: {}", if result.ok { "OK" } else { "FAIL" });
                for v in &result.ncas {
                    let prefix = match &v.partition {
                        Some(p) => format!("[{p}] "),
                        None => String::new(),
                    };
                    log::info!(
                        "  {prefix}{}: {} (sections mismatched: {})",
                        v.name,
                        if v.ok { "OK" } else { "FAIL" },
                        v.mismatched_sections
                    );
                }
                if !result.ok {
                    anyhow::bail!("verification failed");
                }
            }
            NxCommands::Info(cmd) => {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::nintendo::nx::info::read_info(
                    resolved.path(),
                    cmd.keys.as_deref(),
                )?;
                if let Some(dir) = &cmd.save_icon {
                    save_nx_icon(&info, dir)?;
                }
                info_print::print(&rom_converto_lib::info::InfoResult::Nx(info), cmd.json)?;
            }
        },
        Commands::Chd(inner) => match inner {
            ChdCommands::Compress(cmd) => {
                let eff = &effective.chd;
                let mut opts = ChdDvdOptions {
                    hunk_size: cmd.hunk_size.or(eff.hunk_size),
                    allow_zstd: cmd.zstd,
                    force: cmd.force,
                };
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                let mode = if cmd.dvd {
                    Some(DiscMode::Dvd)
                } else if cmd.cd {
                    Some(DiscMode::Cd)
                } else {
                    None
                };
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    batch::chd_compress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        opts,
                        mode,
                        policy,
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["iso", "cue"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "chd",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &resolved.output_basis().with_extension("chd"),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        let media = chd_media_label(input);
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Chd,
                            media.as_deref(),
                            None,
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Chd,
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    opts.force = true;
                    let out_path = output.clone();
                    let started = Instant::now();
                    convert_disc_to_chd_cancellable(
                        &progress,
                        input.to_path_buf(),
                        output,
                        mode,
                        opts,
                        cancel.clone(),
                    )
                    .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            ChdCommands::Extract(cmd) => {
                let eff = &effective.chd;
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    batch::chd_extract(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        cmd.parent,
                        policy,
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["chd"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            let dir = output_dir.as_deref().expect(
                                "OUTPUT or --output-dir is required without --recursive (enforced by clap)",
                            );
                            if !dry_run {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    Some(dir),
                                    "iso",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &resolved.output_basis().with_extension(""),
                                    Some(dir),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "extract",
                            &cmd.input,
                            &output,
                            &decision,
                            None,
                            None,
                            report.as_deref(),
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let in_path = input.to_path_buf();
                    let out_path = output.clone();
                    let started = Instant::now();
                    extract_from_chd_cancellable(
                        &progress,
                        in_path.clone(),
                        output,
                        cmd.parent,
                        cancel.clone(),
                    )
                    .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::CountOnly,
                        "extract",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            ChdCommands::Verify(cmd) => {
                if cmd.recursive {
                    if !cmd.input.is_dir() {
                        anyhow::bail!(
                            "INPUT must be a directory when --recursive is set: {}",
                            cmd.input.display()
                        );
                    }
                    verify_chd_batch(
                        &progress,
                        &total_progress,
                        cmd.input,
                        cmd.fix,
                        cmd.max_depth,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["chd"])?;
                    verify_chd(
                        &progress,
                        resolved.path().to_path_buf(),
                        cmd.parent,
                        cmd.fix,
                    )
                    .await?
                }
            }
            ChdCommands::ToCso(cmd) => {
                let eff = &effective.cso;
                let format = match cmd.format {
                    CsoFormatArg::Cso => CsoFormat::Cso,
                    CsoFormatArg::Zso => CsoFormat::Zso,
                };
                let mut opts = CsoCompressOptions {
                    format,
                    block_size: cmd.block_size.or(eff.block_size),
                    force: cmd.force,
                };
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    batch::chd_to_cso(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        opts,
                        policy,
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["chd"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    format.extension(),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &resolved.output_basis().with_extension(format.extension()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    let media = format.name();
                    if dry_run {
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Cso,
                            Some(media),
                            None,
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Cso,
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        let required = rom_converto_lib::chd::info::read_info(input)
                            .map(|info| info.logical_bytes)
                            .unwrap_or_else(|_| file_len(input));
                        batch::space_preflight_for_size(required, check_dir)?;
                    }
                    opts.force = true;
                    let in_path = input.to_path_buf();
                    let out_path = output.clone();
                    let started = Instant::now();
                    chd_to_cso_cancellable(&progress, in_path, output, opts, cancel.clone())
                        .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            ChdCommands::Info(cmd) => {
                if cmd.keys.is_some() {
                    anyhow::bail!("--keys is only supported by nx and wup info");
                }
                if cmd.save_icon.is_some() {
                    anyhow::bail!(
                        "--save-icon is not supported for chd: the format has no embedded artwork"
                    );
                }
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::chd::info::read_info(resolved.path())?;
                info_print::print(&rom_converto_lib::info::InfoResult::Chd(info), cmd.json)?;
            }
        },
        Commands::Cso(inner) => match inner {
            CsoCommands::Compress(cmd) => {
                let eff = &effective.cso;
                let format = match cmd.format {
                    CsoFormatArg::Cso => CsoFormat::Cso,
                    CsoFormatArg::Zso => CsoFormat::Zso,
                };
                let mut opts = CsoCompressOptions {
                    format,
                    block_size: cmd.block_size.or(eff.block_size),
                    force: cmd.force,
                };
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    batch::cso_compress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        opts,
                        policy,
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["iso"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    format.extension(),
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &resolved.output_basis().with_extension(format.extension()),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    let media = format.name();
                    if dry_run {
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Cso,
                            Some(media),
                            None,
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Cso,
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    opts.force = true;
                    let out_path = output.clone();
                    let started = Instant::now();
                    compress_to_cso_cancellable(
                        &progress,
                        input.to_path_buf(),
                        output,
                        opts,
                        cancel.clone(),
                    )
                    .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            CsoCommands::Decompress(cmd) => {
                let eff = &effective.cso;
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    batch::cso_decompress(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        policy,
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["cso", "zso", "dax"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "iso",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &resolved.output_basis().with_extension("iso"),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single(
                            "decompress",
                            &cmd.input,
                            &output,
                            &decision,
                            None,
                            None,
                            report.as_deref(),
                        );
                    }
                    let output = match decision {
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        batch::space_preflight_for_size(file_len(input), check_dir)?;
                    }
                    let in_path = input.to_path_buf();
                    let out_path = output.clone();
                    let started = Instant::now();
                    decompress_from_cso_cancellable(
                        &progress,
                        in_path.clone(),
                        output,
                        true,
                        cancel.clone(),
                    )
                    .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Decompress,
                        "decompress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            CsoCommands::Verify(cmd) => {
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::cso_verify(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        cmd.full,
                        cmd.max_depth,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["cso", "zso", "dax"])?;
                    verify_cso(&progress, resolved.path().to_path_buf(), cmd.full).await?
                }
            }
            CsoCommands::ToChd(cmd) => {
                let eff = &effective.chd;
                let mut opts = ChdDvdOptions {
                    hunk_size: cmd.hunk_size.or(eff.hunk_size),
                    allow_zstd: cmd.zstd,
                    force: cmd.force,
                };
                let output_dir = cmd.output_dir.clone().or_else(|| eff.output_dir.clone());
                let report = cmd.report.clone().or_else(|| eff.report.clone());
                let fallback = config::policy_fallback(&eff.on_conflict)?;
                let mode = if cmd.dvd {
                    Some(DiscMode::Dvd)
                } else if cmd.cd {
                    Some(DiscMode::Cd)
                } else {
                    None
                };
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    batch::cso_to_chd(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        mode,
                        opts,
                        policy,
                        output_dir.as_deref(),
                        cmd.output_template.as_deref(),
                        cmd.max_depth,
                        dry_run,
                        skip_space_check,
                        report.as_deref(),
                        cancel.clone(),
                        cache,
                    )
                    .await?
                } else {
                    ensure_input_exists(&cmd.input)?;
                    let resolved =
                        rom_converto_lib::util::resolve_input(&cmd.input, &["cso", "zso", "dax"])?;
                    let input = resolved.path();
                    let output = match cmd.output_flag.or(cmd.output) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "chd",
                                    None,
                                    dry_run,
                                )?,
                                None => rom_converto_lib::util::place_in_dir(
                                    &resolved.output_basis().with_extension("chd"),
                                    output_dir.as_deref(),
                                ),
                            }
                        }
                    };
                    let policy = resolve_policy(cmd.on_conflict, cmd.force, fallback);
                    let decision = resolve_output(&output, policy)?;
                    if dry_run {
                        return dry_run_single_verify(
                            "compress",
                            &cmd.input,
                            &output,
                            &decision,
                            policy,
                            crate::util::OutputVerify::Chd,
                            None,
                            None,
                            &progress,
                            report.as_deref(),
                        )
                        .await;
                    }
                    let output = match decision {
                        WriteDecision::Skip
                            if policy
                                == rom_converto_lib::util::ConflictPolicy::OverwriteInvalid =>
                        {
                            match crate::util::verify_existing_output(
                                &progress,
                                &output,
                                crate::util::OutputVerify::Chd,
                            )
                            .await
                            {
                                crate::util::VerifyOutcome::Valid => {
                                    log_kept_valid(&output);
                                    return Ok(());
                                }
                                crate::util::VerifyOutcome::Invalid => {
                                    log_rewriting_invalid(&output);
                                    output
                                }
                            }
                        }
                        WriteDecision::Skip => {
                            log_skipped(&output);
                            return Ok(());
                        }
                        WriteDecision::Write(p) => p,
                    };
                    if !skip_space_check {
                        let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                        let required = rom_converto_lib::cso::info::read_info(input)
                            .map(|info| info.uncompressed_size)
                            .unwrap_or_else(|_| file_len(input));
                        batch::space_preflight_for_size(required, check_dir)?;
                    }
                    opts.force = true;
                    let in_path = input.to_path_buf();
                    let out_path = output.clone();
                    let started = Instant::now();
                    cso_to_chd_cancellable(&progress, in_path, output, mode, opts, cancel.clone())
                        .await?;
                    finish_single(
                        &cmd.input,
                        &out_path,
                        TallyDirection::Compress,
                        "compress",
                        started,
                        report.as_deref(),
                    )?;
                }
            }
            CsoCommands::Info(cmd) => {
                if cmd.keys.is_some() {
                    anyhow::bail!("--keys is only supported by nx and wup info");
                }
                if cmd.save_icon.is_some() {
                    anyhow::bail!(
                        "--save-icon is not supported for cso: the format has no embedded artwork"
                    );
                }
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let info = rom_converto_lib::cso::info::read_info(resolved.path())?;
                info_print::print(&rom_converto_lib::info::InfoResult::Cso(info), cmd.json)?;
            }
        },
        Commands::Cue(inner) => match inner {
            CueCommands::Merge(cmd) => {
                ensure_input_exists(&cmd.input_cue)?;
                let policy = policy_of(cmd.on_conflict, cmd.force);
                let decision = resolve_output(&cmd.output_cue, policy)?;
                if dry_run {
                    let bin = cmd.output_cue.with_extension("bin");
                    let note = format!("+ {}", bin.display());
                    return dry_run_single(
                        "merge",
                        &cmd.input_cue,
                        &cmd.output_cue,
                        &decision,
                        Some(&note),
                        None,
                        None,
                    );
                }
                let output_cue = match decision {
                    WriteDecision::Skip => {
                        log_skipped(&cmd.output_cue);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                if !skip_space_check {
                    let check_dir = output_cue.parent().unwrap_or_else(|| Path::new("."));
                    let required = rom_converto_lib::cue::referenced_files_size(&cmd.input_cue)
                        .await
                        .unwrap_or_else(|_| file_len(&cmd.input_cue));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                merge_bin(&progress, cmd.input_cue, output_cue, true).await?
            }
            CueCommands::ToIso(cmd) => {
                ensure_input_exists(&cmd.input)?;
                let output = cmd
                    .output
                    .clone()
                    .unwrap_or_else(|| cmd.input.with_extension("iso"));
                let policy = policy_of(cmd.on_conflict, cmd.force);
                let decision = resolve_output(&output, policy)?;
                if dry_run {
                    return dry_run_single(
                        "to-iso", &cmd.input, &output, &decision, None, None, None,
                    );
                }
                let output = match decision {
                    WriteDecision::Skip => {
                        log_skipped(&output);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    let required = rom_converto_lib::cue::referenced_files_size(&cmd.input)
                        .await
                        .unwrap_or_else(|_| file_len(&cmd.input));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                cue_to_iso(&progress, cmd.input, output, true).await?
            }
            CueCommands::ToCso(cmd) => {
                ensure_input_exists(&cmd.input)?;
                let format = match cmd.format {
                    CsoFormatArg::Cso => CsoFormat::Cso,
                    CsoFormatArg::Zso => CsoFormat::Zso,
                };
                let output = cmd
                    .output
                    .clone()
                    .unwrap_or_else(|| cmd.input.with_extension(format.extension()));
                let policy = policy_of(cmd.on_conflict, cmd.force);
                let decision = resolve_output(&output, policy)?;
                if dry_run {
                    return dry_run_single(
                        "to-cso",
                        &cmd.input,
                        &output,
                        &decision,
                        Some(format.name()),
                        None,
                        None,
                    );
                }
                let output = match decision {
                    WriteDecision::Skip => {
                        log_skipped(&output);
                        return Ok(());
                    }
                    WriteDecision::Write(p) => p,
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    let required = rom_converto_lib::cue::referenced_files_size(&cmd.input)
                        .await
                        .unwrap_or_else(|_| file_len(&cmd.input));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                cue_to_cso(&progress, cmd.input, output, format, true).await?
            }
        },
        Commands::Hash(cmd) => {
            let algos = parse_algos(&cmd.algo).map_err(|e| anyhow::anyhow!(e))?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                batch::hash_batch(
                    &progress,
                    &total_progress,
                    &cmd.input,
                    &algos,
                    cmd.max_depth,
                    cmd.report.as_deref(),
                    cache,
                )
                .await?;
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                hash_single(
                    &progress,
                    resolved.path(),
                    &algos,
                    cmd.report.as_deref(),
                    cache,
                )?;
            }
        }
        Commands::Playlist(cmd) => {
            require_dir(&cmd.input)?;

            let exts: Vec<String> = cmd
                .extensions
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            let ext_refs: Vec<&str> = exts.iter().map(String::as_str).collect();

            let mode = match cmd.playlist_mode {
                PlaylistModeArg::Multiple => PlaylistMode::Multiple,
                PlaylistModeArg::Always => PlaylistMode::Always,
            };

            let plans = plan_playlists(&PlaylistOptions {
                scan_dir: &cmd.input,
                output_dir: cmd.output_dir.as_deref(),
                extensions: &ext_refs,
                mode,
                max_depth: cmd.max_depth,
            })?;

            // An .m3u has no integrity check, so overwrite-invalid degrades to skip.
            let policy = policy_of(
                cmd.on_conflict
                    .unwrap_or(crate::commands::ConflictPolicyArg::Error),
                cmd.force,
            );

            if !dry_run && let Some(dir) = cmd.output_dir.as_deref() {
                std::fs::create_dir_all(dir)?;
            }

            let mut tally = Tally::new();
            let started = Instant::now();

            for plan in &plans {
                if plan.has_duplicate_numbers {
                    log::warn!(
                        "Duplicate disc numbers in set {}, including all entries",
                        plan.base_title
                    );
                }
                let entry_exts = plan
                    .contents
                    .lines()
                    .filter_map(|line| Path::new(line).extension())
                    .filter_map(|ext| ext.to_str());
                if let Some(mixed) = mixed_playlist_extensions(entry_exts) {
                    log::warn!(
                        "Mixed track formats ({mixed}) in set {}; emulators expect every disc \
                         in a playlist to use the same format",
                        plan.base_title
                    );
                }
                let decision = resolve_output(&plan.m3u_path, policy)?;
                if dry_run {
                    dry_run::log_plan("write", &cmd.input, &plan.m3u_path, &decision, None, None);
                    for line in plan.contents.lines() {
                        log::info!("    {line}");
                    }
                    dry_run::record(&mut tally, &cmd.input, &decision);
                    continue;
                }
                match decision {
                    WriteDecision::Write(path) => {
                        std::fs::write(&path, &plan.contents)?;
                        log::info!("Wrote {} ({} discs)", path.display(), plan.disc_count);
                        tally.record_ok(0, 0, std::time::Duration::ZERO);
                    }
                    WriteDecision::Skip => {
                        log::info!("Skipped existing {}", plan.m3u_path.display());
                        tally.record_skipped();
                    }
                }
            }

            if dry_run {
                dry_run::finish(&tally, &[], None)?;
            } else {
                log::info!("{}", Tally::count_summary(tally.count(), started.elapsed()));
            }
        }
        Commands::Dat(inner) => match inner {
            DatCommands::Verify(cmd) => {
                let algos = parse_algos(&cmd.algo).map_err(|e| anyhow::anyhow!(e))?;
                let bounds = resolve_checksum_bounds(
                    cmd.input_checksum_min.as_deref(),
                    cmd.input_checksum_max.as_deref(),
                    &effective.dat,
                )?;
                bounds
                    .validate_requested(&algos)
                    .map_err(|e| anyhow::anyhow!(e))?;
                let api_base = cmd.api_base.or_else(|| effective.dat.api_base.clone());
                let report = cmd.report.or_else(|| effective.dat.report.clone());
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                    batch::dat_verify_batch(
                        &progress,
                        &total_progress,
                        &cmd.input,
                        &algos,
                        &bounds,
                        cmd.quick,
                        cmd.max_depth,
                        api_base.as_deref(),
                        report.as_deref(),
                        &cancel,
                        cache,
                    )
                    .await?;
                } else {
                    ensure_input_exists(&cmd.input)?;
                    batch::dat_verify_single(
                        &progress,
                        &cmd.input,
                        &algos,
                        &bounds,
                        cmd.quick,
                        api_base.as_deref(),
                        report.as_deref(),
                        &cancel,
                        cache,
                    )
                    .await?;
                }
            }
            DatCommands::Scan(cmd) => {
                require_dir(&cmd.input)?;
                let algos = parse_algos(&cmd.algo).map_err(|e| anyhow::anyhow!(e))?;
                let api_base = cmd.api_base.or_else(|| effective.dat.api_base.clone());
                let report = cmd.report.or_else(|| effective.dat.report.clone());
                batch::dat_scan(
                    &progress,
                    &total_progress,
                    &cmd.input,
                    cmd.max_depth,
                    &algos,
                    cmd.quick,
                    api_base.as_deref(),
                    report.as_deref(),
                    &cancel,
                    cache,
                )
                .await?;
            }
            DatCommands::Rename(cmd) => {
                if cmd.recursive {
                    require_dir(&cmd.input)?;
                } else {
                    ensure_input_exists(&cmd.input)?;
                }
                let api_base = cmd.api_base.or_else(|| effective.dat.api_base.clone());
                let report = cmd.report.or_else(|| effective.dat.report.clone());
                let policy = policy_of(
                    cmd.on_conflict
                        .unwrap_or(crate::commands::ConflictPolicyArg::Error),
                    cmd.force,
                );
                batch::dat_rename(
                    &progress,
                    &total_progress,
                    &cmd.input,
                    cmd.recursive,
                    cmd.max_depth,
                    api_base.as_deref(),
                    policy,
                    dry_run,
                    report.as_deref(),
                    &cancel,
                    cache,
                )
                .await?;
            }
            DatCommands::Identify(cmd) => {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, ALL_IMAGE_EXTS)?;
                let algos = parse_algos(&cmd.algo).map_err(|e| anyhow::anyhow!(e))?;
                let bounds = resolve_checksum_bounds(
                    cmd.input_checksum_min.as_deref(),
                    cmd.input_checksum_max.as_deref(),
                    &effective.dat,
                )?;
                bounds
                    .validate_requested(&algos)
                    .map_err(|e| anyhow::anyhow!(e))?;
                let api_base = cmd.api_base.or_else(|| effective.dat.api_base.clone());
                batch::dat_identify(
                    &progress,
                    resolved.path(),
                    &algos,
                    &bounds,
                    api_base.as_deref(),
                    &cancel,
                    cache,
                )
                .await?;
            }
            DatCommands::Fixdat(cmd) => {
                require_dir(&cmd.input)?;
                let api_base = cmd
                    .api_base
                    .clone()
                    .or_else(|| effective.dat.api_base.clone());
                let policy = policy_of(
                    cmd.on_conflict
                        .unwrap_or(crate::commands::ConflictPolicyArg::Error),
                    cmd.force,
                );
                let args = batch::DatFixdatArgs {
                    input: cmd.input,
                    output: cmd.output,
                    platform: cmd.platform,
                    dat_id: cmd.dat_id,
                    dat_name: cmd.dat_name,
                    subset: cmd.subset,
                    max_depth: cmd.max_depth,
                    api_base,
                };
                batch::dat_fixdat(&progress, &args, dry_run, policy, &cancel, cache).await?;
            }
        },
        Commands::SelfUpdate(_) => self_update(github).await?,
        Commands::ShellCompletions(_) => unreachable!("handled before logger init"),
    }

    Ok(())
}

fn should_check_for_updates(no_update_check: bool) -> bool {
    if no_update_check {
        return false;
    }
    for var in ["ROM_CONVERTO_NO_UPDATE_CHECK", "NO_UPDATE_NOTIFIER", "CI"] {
        if std::env::var(var).is_ok() {
            return false;
        }
    }
    std::io::stderr().is_terminal()
}

fn require_dir(input: &std::path::Path) -> Result<()> {
    if !input.is_dir() {
        anyhow::bail!("expected a directory: {}", input.display());
    }
    Ok(())
}

fn ok_str(b: bool) -> &'static str {
    if b { "OK" } else { "FAIL" }
}

fn print_rvz_structure(s: Option<&rom_converto_lib::nintendo::rvz::RvzStructuralVerify>) {
    let Some(s) = s else {
        return;
    };
    log::info!("RVZ file header hash: {}", ok_str(s.file_head_hash_ok));
    log::info!("RVZ disc struct hash: {}", ok_str(s.disc_hash_ok));
    match s.part_hash_ok {
        Some(v) => log::info!("RVZ partition table hash: {}", ok_str(v)),
        None => log::info!("RVZ partition table hash: n/a (no partitions)"),
    }
}

fn save_dol_banner(info: &rom_converto_lib::info::DolInfo, dir: &std::path::Path) -> Result<()> {
    let Some(img) = &info.banner_image else {
        log::warn!("No GameCube banner decoded; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let stem = if info.game_id.is_empty() {
        "gamecube-banner".to_string()
    } else {
        info.game_id.clone()
    };
    let path = dir.join(format!("{stem}.png"));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("Wrote {}", path.display());
    Ok(())
}

fn save_ctr_icon(info: &rom_converto_lib::info::CtrInfo, dir: &std::path::Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No SMDH icon decoded; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let stem = if info.title_id.is_empty() {
        "ctr-icon".to_string()
    } else {
        info.title_id.clone()
    };
    let path = dir.join(format!("{stem}.png"));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("Wrote {}", path.display());
    Ok(())
}

fn save_nx_icon(info: &rom_converto_lib::info::NxInfo, dir: &std::path::Path) -> Result<()> {
    let Some(full) = &info.full else {
        log::warn!("No control NCA payload available; nothing to save");
        return Ok(());
    };
    let Some(ctrl) = &full.control else {
        log::warn!("No NACP/icon decoded; nothing to save");
        return Ok(());
    };
    let Some(img) = &ctrl.icon else {
        log::warn!("Control NACP loaded but no icon present; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{:016X}.png", full.application_title_id));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("Wrote {}", path.display());
    Ok(())
}

fn save_rvl_image(info: &rom_converto_lib::info::RvlInfo, dir: &std::path::Path) -> Result<()> {
    let Some(img) = &info.image else {
        log::warn!("No Wii banner decoded; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let stem = if info.game_id.is_empty() {
        "wii-banner".to_string()
    } else {
        info.game_id.clone()
    };
    let path = dir.join(format!("{stem}.png"));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("Wrote {}", path.display());
    Ok(())
}

fn save_wup_image(info: &rom_converto_lib::info::WupInfo, dir: &std::path::Path) -> Result<()> {
    let Some(img) = &info.image else {
        log::warn!("No Wii U icon decoded; nothing to save");
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.png", info.title_id_hex));
    std::fs::write(&path, &img.png_bytes)?;
    log::info!("Wrote {}", path.display());
    Ok(())
}

fn run_shell_completions(cmd: &ShellCompletionsCommand) -> Result<()> {
    // The package is rom-converto-cli but the installed binary is
    // rom-converto; completions must key off the binary name the user
    // actually types. CARGO_BIN_NAME tracks the [[bin]] name even if
    // the crate is renamed.
    let bin = env!("CARGO_BIN_NAME");
    let mut clap_cmd = Cli::command().name(bin).bin_name(bin);

    match &cmd.out_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            let path = generate_to(cmd.shell, &mut clap_cmd, bin, dir)?;
            println!("{}", path.display());
        }
        None => {
            generate(cmd.shell, &mut clap_cmd, bin, &mut std::io::stdout().lock());
        }
    }
    Ok(())
}

#[cfg(test)]
mod migrate_gate_tests {
    use super::*;

    fn write_wia(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("game.wia");
        std::fs::write(&p, [b'W', b'I', b'A', 0x01, 0, 0, 0, 0]).unwrap();
        p
    }

    #[test]
    fn dol_migrate_dry_run_rejects_wia() {
        let dir = tempfile::tempdir().unwrap();
        let wia = write_wia(dir.path());
        let err = migrate_dry_run(&wia, None, false, false, DOL_MIGRATE_FORMATS).unwrap_err();
        assert_eq!(
            err.to_string(),
            "input is a WIA image; use rvl migrate for Wii disc images"
        );
    }

    #[test]
    fn rvl_migrate_dry_run_accepts_wia() {
        let dir = tempfile::tempdir().unwrap();
        let wia = write_wia(dir.path());
        migrate_dry_run(&wia, None, false, false, ALL_MIGRATE_FORMATS)
            .expect("the rvl dry-run must accept a WIA image");
    }

    #[test]
    fn dol_migrate_dry_run_recursive_skips_wia_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        write_wia(dir.path());
        migrate_dry_run(dir.path(), None, true, false, DOL_MIGRATE_FORMATS)
            .expect("a WIA-only directory must not fail a dol dry-run");
    }
}

#[cfg(test)]
mod verify_gate_tests {
    use super::*;

    fn write_wia(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("game.wia");
        std::fs::write(&p, [b'W', b'I', b'A', 0x01, 0, 0, 0, 0]).unwrap();
        p
    }

    #[test]
    fn dol_verify_gate_rejects_wia() {
        let dir = tempfile::tempdir().unwrap();
        let wia = write_wia(dir.path());
        let err = verify_gate(&wia, DOL_MIGRATE_FORMATS).unwrap_err();
        assert_eq!(
            err.to_string(),
            "input is a WIA image; use rvl verify for Wii disc images"
        );
    }

    #[test]
    fn rvl_verify_gate_accepts_wia() {
        let dir = tempfile::tempdir().unwrap();
        let wia = write_wia(dir.path());
        verify_gate(&wia, ALL_MIGRATE_FORMATS).expect("rvl verify must accept a WIA image");
    }

    #[test]
    fn verify_gate_ignores_non_legacy_input() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("game.iso");
        std::fs::write(&plain, [0u8; 16]).unwrap();
        verify_gate(&plain, DOL_MIGRATE_FORMATS).expect("a plain file must pass the gate");
    }
}

#[cfg(test)]
mod migrate_opts_tests {
    use super::*;
    use rom_converto_lib::config::DiscDefaults;

    #[test]
    fn config_level_and_chunk_reach_migrate_opts() {
        let eff = DiscDefaults {
            level: Some(7),
            chunk_size: Some(262_144),
            ..Default::default()
        };
        let opts = resolve_migrate_opts(None, None, &eff);
        assert_eq!(opts.compression_level, 7);
        assert_eq!(opts.chunk_size, 262_144);
    }

    #[test]
    fn migrate_flag_beats_config() {
        let eff = DiscDefaults {
            level: Some(7),
            chunk_size: Some(262_144),
            ..Default::default()
        };
        let opts = resolve_migrate_opts(Some(3), Some(65_536), &eff);
        assert_eq!(opts.compression_level, 3);
        assert_eq!(opts.chunk_size, 65_536);
    }

    #[test]
    fn migrate_falls_back_to_builtin() {
        let opts = resolve_migrate_opts(None, None, &DiscDefaults::default());
        assert_eq!(
            opts.compression_level,
            RvzCompressOptions::default().compression_level
        );
        assert_eq!(opts.chunk_size, RvzCompressOptions::default().chunk_size);
    }
}
