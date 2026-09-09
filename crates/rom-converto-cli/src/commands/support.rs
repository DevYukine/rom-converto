//! Helpers shared by the per-family command runners: output-path derivation,
//! single-file summaries and reports, dry-run planning, and artwork export.

use crate::util::{IndicatifProgress, TotalProgress, ensure_input_exists, ok_str};
use crate::{config, info_print};
use anyhow::{Context, Result};
use rom_converto_lib::disc::chd::{ChdCodec, DiscMode};
use rom_converto_lib::nintendo::disc::legacy::{
    LegacyFormat, detect_legacy_format, ensure_format_allowed_for,
};
use rom_converto_lib::nintendo::disc::rvz::RvzCompressOptions;
use rom_converto_lib::util::{CancelToken, FileDigests, HashAlgo, Tally, hash_file};
use std::path::Path;
use std::time::Instant;

// Extension sets mirror the lib-internal batch scanners so the closing
// count summary matches what each lib batch function actually processed.
pub(crate) const CTR_CRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];

// Union of image extensions the read side recognizes, used to pick the first
// convertible member when a format-agnostic command (hash) is handed an archive.
pub(crate) const ALL_IMAGE_EXTS: &[&str] = &[
    "iso", "gcm", "wbfs", "rvz", "gcz", "wia", "nkit", "chd", "cso", "zso", "dax", "cue", "cia",
    "3ds", "cci", "cxi", "3dsx", "zcia", "zcci", "zcxi", "z3dsx", "nsp", "xci", "nca", "nsz",
    "xcz", "ncz", "wud", "wux", "xiso", "zar",
];

/// Default GoD output directory for `input`: its file stem with `_god`
/// appended, alongside the input.
pub(crate) fn derive_god_dir(input: &Path) -> std::path::PathBuf {
    let mut name = input.file_stem().unwrap_or_default().to_os_string();
    name.push("_god");
    input.with_file_name(name)
}

pub(crate) fn hash_single(
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
            let computed = hash_file(input, algos, progress, &CancelToken::new());
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
        write_hash_report(
            path,
            &[record],
            &totals,
            ReportFormat::from_path(path),
            &CancelToken::new(),
        )?;
    }

    match outcome {
        Ok(_) => {
            log_count_summary(tally.count(), tally);
            Ok(())
        }
        Err(_) => anyhow::bail!("failed to hash {}", input.display()),
    }
}

pub(crate) fn log_count_summary(count: usize, tally: Tally) {
    log::info!("{}", Tally::count_summary(count, tally.elapsed()));
}

pub(crate) fn print_hash_row(path: &Path, d: &FileDigests, algos: &[HashAlgo]) {
    let cells: Vec<String> = algos
        .iter()
        .map(|a| format!("{}={}", a.label(), d.value(*a).unwrap_or("")))
        .collect();
    log::info!("{}  {}", path.display(), cells.join("  "));
}

/// Reject a legacy container the verify console gate does not accept, pointing
/// at `rvl verify`. A non-legacy input passes through to the normal magic check.
pub(crate) fn verify_gate(input: &Path, allowed: &[LegacyFormat]) -> Result<()> {
    if let Some(fmt) = detect_legacy_format(input)? {
        ensure_format_allowed_for(fmt, allowed, "rvl verify")?;
    }
    Ok(())
}

/// Resolve the compression knobs migrate exposes with the compress precedence:
/// flag over preset/config over built-in default.
pub(crate) fn resolve_migrate_opts(
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

/// Best-effort media label for a CHD dry-run plan line. ISO inputs read a
/// header to predict the disc kind; cue inputs imply a CD with no header probe.
pub(crate) fn chd_media_label(input: &Path) -> Option<String> {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("cue") => Some("CD".to_string()),
        Some("avi") => Some("LaserDisc".to_string()),
        Some("iso") => rom_converto_lib::util::iso9660::detect_disc_kind(input)
            .ok()
            .map(|k| k.label().to_string()),
        _ => None,
    }
}

/// Resolves whether `chd compress`'s DVD-codec tip applies to a single input:
/// an explicit --dvd/--cd flag settles it outright, otherwise falls back to
/// the same best-effort probe the dry-run plan line uses.
pub(crate) fn resolved_dvd_mode(mode: Option<DiscMode>, input: &Path) -> bool {
    match mode {
        Some(DiscMode::Dvd) => true,
        Some(DiscMode::Cd) | Some(DiscMode::Ld) => false,
        None => chd_media_label(input).as_deref() == Some("DVD"),
    }
}

/// Emits the zstd-for-DVD codec tip once per run, when the resolved CHD
/// flavor is DVD and the user did not pass an explicit --codecs list.
pub(crate) fn maybe_log_dvd_codec_tip(dvd: bool, codecs_set: bool) {
    if dvd && !codecs_set {
        log::info!(
            "tip: zstd usually compresses DVD images better than the default codecs; \
             enable it with --codecs zstd,lzma,zlib,flac (note: some emulators' older \
             libchdr builds (e.g. AetherSX2/NetherSX2) reject zstd CHDs)"
        );
    }
}

/// Resolves the codec list to hand `ChdOptions`: an explicit CLI value wins,
/// otherwise the preset/config codec names are parsed, otherwise `None`
/// (letting the lib apply its per-mode chdman default).
pub(crate) fn resolve_chd_codecs(
    cli: Option<Vec<ChdCodec>>,
    preset: &Option<Vec<String>>,
) -> Result<Option<Vec<ChdCodec>>> {
    if let Some(codecs) = cli {
        return Ok(Some(codecs));
    }
    preset
        .as_ref()
        .map(|names| {
            names
                .iter()
                .map(|name| name.parse::<ChdCodec>().map_err(anyhow::Error::from))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
}

pub(crate) struct DispatchCtx<'a> {
    pub(crate) progress: IndicatifProgress,
    pub(crate) total_progress: TotalProgress,
    pub(crate) effective: &'a config::Effective,
    /// `--config` and `--preset`, forwarded verbatim so the runner resolves
    /// the same config file the CLI did instead of re-searching the default
    /// locations.
    pub(crate) config: Option<std::path::PathBuf>,
    pub(crate) preset: Option<String>,
    pub(crate) dry_run: bool,
    pub(crate) skip_space_check: bool,
    pub(crate) cancel: rom_converto_lib::util::CancelToken,
    pub(crate) cache: &'a std::sync::Arc<rom_converto_lib::util::HashCache>,
}

/// A recursive run needs a directory; a single run just needs the input to
/// exist.
pub(crate) fn require_input(input: &Path, recursive: bool) -> Result<()> {
    if recursive {
        require_dir(input)
    } else {
        ensure_input_exists(input)
    }
}

/// The runner's `mode` option value for a resolved disc mode.
pub(crate) fn disc_mode_name(mode: DiscMode) -> &'static str {
    match mode {
        DiscMode::Cd => "cd",
        DiscMode::Dvd => "dvd",
        DiscMode::Ld => "ld",
    }
}

pub(crate) fn require_dir(input: &std::path::Path) -> Result<()> {
    if !input.is_dir() {
        anyhow::bail!("expected a directory: {}", input.display());
    }
    Ok(())
}

/// Unwraps a per-console `info` subcommand's INPUT; these subcommands don't
/// support `--paths-file`, so INPUT is always required.
pub(crate) fn require_info_input(input: &Option<std::path::PathBuf>) -> Result<&Path> {
    input
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("INPUT is required"))
}

/// Reads one input path per line from `path`; blank lines and `#` comments
/// are skipped.
pub(crate) fn read_info_paths_file(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read paths file: {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(std::path::PathBuf::from)
        .collect())
}

/// Recursively collects files under `dir` whose lowercase extension is in
/// [`rom_converto_lib::info::SUPPORTED_INFO_EXTENSIONS`], sorted for a
/// deterministic batch order.
pub(crate) fn collect_info_batch_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("failed to read directory: {}", dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read directory: {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .is_some_and(|e| {
                    rom_converto_lib::info::SUPPORTED_INFO_EXTENSIONS.contains(&e.as_str())
                })
            {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// One result row of a batch `info` run, serialized as either
/// `{"path","ok":true,"info"}` or `{"path","ok":false,"error"}`.
#[derive(serde::Serialize)]
pub(crate) struct InfoBatchEntry {
    path: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<rom_converto_lib::info::InfoResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(crate) fn info_for_batch_entry(
    path: &Path,
    opts: &rom_converto_lib::info::InfoOptions,
    save_icon: Option<&Path>,
) -> Result<rom_converto_lib::info::InfoResult> {
    let info = rom_converto_lib::info::read_info(path, opts)?;
    if let Some(dir) = save_icon {
        save_info_icon(&info, dir)?;
    }
    Ok(info)
}

/// Runs `info` over every path, printing a single JSON array with `--json`
/// or a separated pretty section per file otherwise. Per-file failures are
/// reported inline and never affect the process exit code.
pub(crate) fn run_info_batch(
    paths: &[std::path::PathBuf],
    opts: &rom_converto_lib::info::InfoOptions,
    json: bool,
    save_icon: Option<&Path>,
) {
    if json {
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let path_str = path.display().to_string();
            match info_for_batch_entry(path, opts, save_icon) {
                Ok(info) => results.push(InfoBatchEntry {
                    path: path_str,
                    ok: true,
                    info: Some(info),
                    error: None,
                }),
                Err(e) => results.push(InfoBatchEntry {
                    path: path_str,
                    ok: false,
                    info: None,
                    error: Some(e.to_string()),
                }),
            }
        }
        match serde_json::to_string_pretty(&results) {
            Ok(s) => println!("{s}"),
            Err(e) => log::error!("failed to serialize batch results: {e}"),
        }
    } else {
        for path in paths {
            println!("==> {}", path.display());
            match info_for_batch_entry(path, opts, save_icon) {
                Ok(info) => {
                    if let Err(e) = info_print::print(&info, false) {
                        log::warn!("{}: {}", path.display(), e);
                    }
                }
                Err(e) => log::warn!("{}: {}", path.display(), e),
            }
        }
    }
}

pub(crate) fn print_rvz_structure(
    s: Option<&rom_converto_lib::nintendo::disc::rvz::RvzStructuralVerify>,
) {
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

pub(crate) fn save_dol_banner(info: &rom_converto_lib::info::DolInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.banner_image else {
        log::warn!("No GameCube banner decoded; nothing to save");
        return Ok(());
    };
    save_png(&img.png_bytes, &info.game_id, "gamecube-banner", dir)
}

/// Dispatches an [`InfoResult`](rom_converto_lib::info::InfoResult) to its
/// format-specific icon/banner saver.
pub(crate) fn save_info_icon(info: &rom_converto_lib::info::InfoResult, dir: &Path) -> Result<()> {
    use rom_converto_lib::info::InfoResult;
    match info {
        InfoResult::Ctr(i) => save_ctr_icon(i, dir),
        InfoResult::Dol(i) => save_dol_banner(i, dir),
        InfoResult::Rvl(i) => save_rvl_image(i, dir),
        InfoResult::Wup(i) => save_wup_image(i, dir),
        InfoResult::Nx(i) => save_nx_icon(i, dir),
        InfoResult::Xbox(i) => save_xbox_icon(i, dir),
        InfoResult::Xenon(i) => save_xex_icon(i.xex.as_ref(), dir),
        InfoResult::Psp(i) => save_psp_icon(i, dir),
        InfoResult::Ps3(i) => save_ps3_icon(i, dir),
        InfoResult::Ntr(i) => save_ntr_icon(i, dir),
        InfoResult::Pbp(i) => save_pbp_icon(i, dir),
        InfoResult::Vpk(i) => save_vpk_icon(i, dir),
        InfoResult::Pkg(i) => save_pkg_icon(i, dir),
        InfoResult::Chd(_)
        | InfoResult::Cso(_)
        | InfoResult::Psx(_)
        | InfoResult::Retro(_)
        | InfoResult::LaserDisc(_) => {
            anyhow::bail!("--save-icon is not supported for this format: no embedded artwork")
        }
    }
}

/// Writes one decoded artwork PNG into `dir`, named after the title id with
/// `fallback_stem` standing in when the id is missing or sanitizes to nothing.
pub(crate) fn save_png(png_bytes: &[u8], id: &str, fallback_stem: &str, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let stem = rom_converto_lib::util::template::sanitize_file_stem(id);
    let stem = if stem.is_empty() {
        fallback_stem
    } else {
        &stem
    };
    let path = dir.join(format!("{stem}.png"));
    std::fs::write(&path, png_bytes)?;
    log::info!("Wrote {}", path.display());
    Ok(())
}

pub(crate) fn save_ctr_icon(info: &rom_converto_lib::info::CtrInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No SMDH icon decoded; nothing to save");
        return Ok(());
    };
    save_png(&img.png_bytes, &info.title_id, "ctr-icon", dir)
}

pub(crate) fn save_xex_icon(
    xex: Option<&rom_converto_lib::microsoft::xex::XexInfo>,
    dir: &Path,
) -> Result<()> {
    let Some(xex) = xex else {
        log::warn!("No default.xex found; nothing to save");
        return Ok(());
    };
    let Some(img) = &xex.icon else {
        log::warn!("No XEX icon decoded; nothing to save");
        return Ok(());
    };
    save_png(&img.png_bytes, &xex.title_id_hex, "xex-icon", dir)
}

/// Prefers the OG Xbox XBE title image, falling back to the Xbox 360 XEX
/// icon when the disc only carries a `default.xex` — mirrors the GUI's
/// `extract_icon_png`.
pub(crate) fn save_xbox_icon(info: &rom_converto_lib::info::XisoInfo, dir: &Path) -> Result<()> {
    if let Some(xbe) = &info.xbe
        && let Some(img) = &xbe.icon
    {
        return save_png(&img.png_bytes, &xbe.title_id_hex, "xbox-icon", dir);
    }
    save_xex_icon(info.xex.as_ref(), dir)
}

pub(crate) fn save_ps3_icon(info: &rom_converto_lib::info::Ps3Info, dir: &Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No PS3_GAME/ICON0.PNG decoded; nothing to save");
        return Ok(());
    };
    save_png(
        &img.png_bytes,
        info.title_id.as_deref().unwrap_or_default(),
        "ps3-icon",
        dir,
    )
}

pub(crate) fn save_nx_icon(info: &rom_converto_lib::info::NxInfo, dir: &Path) -> Result<()> {
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
    let id = format!("{:016X}", full.application_title_id);
    save_png(&img.png_bytes, &id, "nx-icon", dir)
}

pub(crate) fn save_rvl_image(info: &rom_converto_lib::info::RvlInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.image else {
        log::warn!("No Wii banner decoded; nothing to save");
        return Ok(());
    };
    save_png(&img.png_bytes, &info.game_id, "wii-banner", dir)
}

pub(crate) fn save_wup_image(info: &rom_converto_lib::info::WupInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.image else {
        log::warn!("No Wii U icon decoded; nothing to save");
        return Ok(());
    };
    save_png(&img.png_bytes, &info.title_id_hex, "wup-icon", dir)
}

pub(crate) fn save_psp_icon(info: &rom_converto_lib::info::PspInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No ICON0.PNG decoded; nothing to save");
        return Ok(());
    };
    save_png(
        &img.png_bytes,
        info.title_id.as_deref().unwrap_or_default(),
        "psp-icon",
        dir,
    )
}

pub(crate) fn save_ntr_icon(info: &rom_converto_lib::info::NtrInfo, dir: &Path) -> Result<()> {
    let Some(banner) = &info.banner else {
        log::warn!("No DS banner decoded; nothing to save");
        return Ok(());
    };
    save_png(&banner.icon.png_bytes, &info.game_code, "ntr-icon", dir)
}

pub(crate) fn save_pbp_icon(info: &rom_converto_lib::info::PbpInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No ICON0.PNG decoded; nothing to save");
        return Ok(());
    };
    save_png(
        &img.png_bytes,
        info.disc_id.as_deref().unwrap_or_default(),
        "pbp-icon",
        dir,
    )
}

pub(crate) fn save_vpk_icon(info: &rom_converto_lib::info::VpkInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No sce_sys/icon0.png decoded; nothing to save");
        return Ok(());
    };
    save_png(
        &img.png_bytes,
        info.title_id.as_deref().unwrap_or_default(),
        "vpk-icon",
        dir,
    )
}

pub(crate) fn save_pkg_icon(info: &rom_converto_lib::info::PkgInfo, dir: &Path) -> Result<()> {
    let Some(img) = &info.icon else {
        log::warn!("No icon0.png decoded; nothing to save");
        return Ok(());
    };
    save_png(
        &img.png_bytes,
        info.title_id.as_deref().unwrap_or_default(),
        "pkg-icon",
        dir,
    )
}

#[cfg(test)]
mod verify_gate_tests {
    use super::*;
    use rom_converto_lib::nintendo::disc::legacy::{ALL_MIGRATE_FORMATS, DOL_MIGRATE_FORMATS};

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

#[cfg(test)]
mod chd_migrate_target_tests {
    use super::*;
    use crate::commands::chd::MigrateCommand;
    use clap::Parser;

    #[test]
    fn derived_output_is_a_v5_sibling_not_the_input() {
        let derived = rom_converto_lib::disc::chd::migrated_chd_path(Path::new("roms/game.chd"));
        assert_eq!(derived, Path::new("roms/game.v5.chd"));
    }

    #[test]
    fn in_place_conflicts_with_an_explicit_output() {
        assert!(
            MigrateCommand::try_parse_from(["migrate", "--in-place", "-o", "out.chd", "in.chd"])
                .is_err()
        );
    }
}

#[cfg(test)]
mod chd_codecs_tip_tests {
    use super::*;

    #[test]
    fn preset_codecs_count_as_effectively_set() {
        // A preset/config codec list must resolve to Some so the DVD zstd
        // tip is suppressed for it, not just for an explicit --codecs flag.
        let resolved = resolve_chd_codecs(None, &Some(vec!["zstd".to_string()])).unwrap();
        assert!(resolved.is_some());
    }

    #[test]
    fn no_cli_and_no_preset_leaves_codecs_unset() {
        let resolved = resolve_chd_codecs(None, &None).unwrap();
        assert!(resolved.is_none());
    }
}
