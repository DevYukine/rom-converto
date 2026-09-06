use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    DispatchCtx, finish_single, require_dir, require_info_input, save_ps3_icon, skipped_single,
};
use crate::util::{
    SingleOutput, ensure_input_exists, file_len, resolve_policy, resolve_single_output,
};
use crate::{batch, info_print};
use anyhow::Result;
use rom_converto_lib::sony::ps3::{decrypt_ps3_iso, read_ps3_info, resolve_ps3_key};
use rom_converto_lib::util::TallyDirection;
use std::path::Path;
use std::time::Instant;

/// Commands for PlayStation 3 disc images
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum Ps3Commands {
    Decrypt(DecryptPs3Command),
    Info(InfoCommand),
}

/// Decrypt a PS3 ISO into a plain ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decrypt a PS3 ISO into a plain ISO\n\nThe disc alternates plain and encrypted sector regions; encrypted regions are AES-128-CBC decrypted with the per-disc data key. Output covers the region-table's sector span; trailing padding past it is not copied.\n\nThe data key is resolved from --key, else the built-in database keyed by the disc's title ID, else a sibling <input>.dkey.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ps3 decrypt game.iso\n  Explicit key:    rom-converto ps3 decrypt --key game.dkey game.iso game.dec.iso\n  Whole folder:    rom-converto ps3 decrypt -R ./roms --output-dir ./decrypted\n"
)]
pub struct DecryptPs3Command {
    /// Input encrypted PS3 ISO path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output ISO path, defaults to the input with `.decrypted` inserted before the extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Alias for the positional OUTPUT argument
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Disc data key file (.dkey). Optional: when omitted the key is looked up in the
    /// built-in database by the disc's title ID, then a sibling `<input>.dkey`. Pass this
    /// to override that for a disc the database doesn't cover. Can't be combined with
    /// --recursive: one key can't be right for every disc in the batch
    #[arg(long = "key", value_name = "FILE", conflicts_with = "recursive")]
    pub key: Option<PathBuf>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Decrypt every .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,

    /// Skip the encryption and key verification probe (use if a correct key is rejected)
    #[arg(long = "skip-probe", default_value_t = false)]
    pub skip_probe: bool,
}

/// Runs one `ps3` subcommand.
pub async fn run(command: Ps3Commands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        dry_run,
        skip_space_check,
        cancel,
        ..
    } = ctx;
    match command {
        Ps3Commands::Decrypt(cmd) => {
            let output_dir = cmd.out.output_dir.clone();
            let report = cmd.batch.report.clone();
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy,
                    output_dir: output_dir.as_deref(),
                    output_template: cmd.out.output_template.as_deref(),
                    max_depth: cmd.batch.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: report.as_deref(),
                    cancel: &cancel,
                };
                batch::ps3_decrypt(&run, cmd.skip_probe).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["iso"])?;
                let input = resolved.path();
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "decrypt",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived: rom_converto_lib::sony::ps3::derive_decrypted_path(
                            resolved.output_basis(),
                        ),
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: "iso",
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: report.as_deref(),
                        dry_run,
                        cancel: cancel.clone(),
                    },
                    &progress,
                )
                .await?
                else {
                    return Ok(());
                };
                if !skip_space_check {
                    let check_dir = output.parent().unwrap_or_else(|| Path::new("."));
                    batch::space_preflight_for_size(file_len(input), check_dir)?;
                }
                let key = resolve_ps3_key(input, resolved.output_basis(), cmd.key.as_deref())?;
                let in_path = input.to_path_buf();
                let out_path = output.clone();
                let started = Instant::now();
                match decrypt_ps3_iso(
                    &progress,
                    in_path,
                    output,
                    key,
                    true,
                    cmd.skip_probe,
                    cancel.clone(),
                )
                .await
                {
                    Ok(()) => {}
                    Err(e @ rom_converto_lib::sony::ps3::Ps3Error::AlreadyDecrypted) => {
                        log::info!("{} is already decrypted", cmd.input.display());
                        skipped_single(&cmd.input, "decrypt", e, report.as_deref())?;
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                }
                finish_single(
                    &cmd.input,
                    &out_path,
                    TallyDirection::Convert,
                    "decrypt",
                    started,
                    report.as_deref(),
                )?;
            }
        }
        Ps3Commands::Info(cmd) => {
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, &["iso"])?;
            let input = resolved.path();
            let size = file_len(input);
            let key = resolve_ps3_key(input, resolved.output_basis(), cmd.keys.as_deref());
            let info = read_ps3_info(input);
            if let Some(dir) = &cmd.save_icon {
                match &info {
                    Ok(i) => save_ps3_icon(i, dir)?,
                    Err(e) => {
                        anyhow::bail!("failed to read PS3 metadata for --save-icon: {e}")
                    }
                }
            }
            let meta = info.as_ref().ok();
            if cmd.json {
                let key_json = match &key {
                    Ok(_) => serde_json::json!({"resolved": true}),
                    Err(e) => serde_json::json!({"resolved": false, "error": e.to_string()}),
                };
                let regions_json = match &info {
                    Ok(i) => serde_json::json!({
                        "regionCount": i.region_count,
                        "totalSectors": i.total_sectors,
                        "encryptedSectors": i.encrypted_sectors,
                    }),
                    Err(e) => serde_json::json!({"error": e.to_string()}),
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "path": input.display().to_string(),
                        "sizeBytes": size,
                        "dataKey": key_json,
                        "regions": regions_json,
                        "title": meta.and_then(|i| i.title.clone()),
                        "titleId": meta.and_then(|i| i.title_id.clone()),
                        "region": meta.and_then(|i| i.region.clone()),
                        "version": meta.and_then(|i| i.version.clone()),
                        "appVer": meta.and_then(|i| i.app_ver.clone()),
                        "resolution": meta.and_then(|i| i.resolution.clone()),
                        "soundFormat": meta.and_then(|i| i.sound_format.clone()),
                        "firmware": meta.and_then(|i| i.firmware.clone()),
                        "parentalLevel": meta.and_then(|i| i.parental_level),
                    }))?
                );
            } else {
                let mut table = info_print::KeyValueTable::new();
                table.push("Path", input.display().to_string());
                if let Some(i) = meta {
                    if let Some(v) = &i.title {
                        table.push("Title", v.clone());
                    }
                    if let Some(v) = &i.title_id {
                        table.push("Title ID", v.clone());
                    }
                    if let Some(v) = &i.region {
                        table.push("Region", v.clone());
                    }
                    if let Some(v) = &i.version {
                        table.push("Version", v.clone());
                    }
                    if let Some(v) = &i.app_ver {
                        table.push("App version", v.clone());
                    }
                }
                table.push("Size", rom_converto_lib::util::format_bytes(size));
                table.push(
                    "Data key",
                    match &key {
                        Ok(_) => "resolved".to_string(),
                        Err(e) => format!("not resolved ({e})"),
                    },
                );
                match &info {
                    Ok(i) => {
                        table.push("Regions", format!("{}", i.region_count));
                        table.push("Total sectors", format!("{}", i.total_sectors));
                        table.push("Encrypted sectors", format!("{}", i.encrypted_sectors));
                    }
                    Err(e) => {
                        table.push("Regions", format!("unavailable ({e})"));
                    }
                }
                if let Some(i) = meta {
                    if let Some(v) = &i.resolution {
                        table.push("Resolution", v.clone());
                    }
                    if let Some(v) = &i.sound_format {
                        table.push("Sound format", v.clone());
                    }
                    if let Some(v) = &i.firmware {
                        table.push("Firmware", v.clone());
                    }
                    if let Some(p) = i.parental_level {
                        table.push("Parental level", format!("{}", p));
                    }
                }
                print!("{}", table.render());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Ps3Commands,
    }

    #[test]
    fn parses_decrypt_with_key() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "--key", "k.dkey"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.input, PathBuf::from("game.iso"));
        assert_eq!(c.key, Some(PathBuf::from("k.dkey")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_decrypt_force() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "-f"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.conflict.force);
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_decrypt_skip_probe() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "--skip-probe"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.skip_probe);
    }

    #[test]
    fn parses_decrypt_recursive() {
        let h = Harness::parse_from(["bin", "decrypt", "roms", "-R"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert!(c.recursive);
    }

    #[test]
    fn decrypt_output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decrypt", "game.iso", "-o", "out.iso"]);
        let Ps3Commands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.iso")));
    }

    #[test]
    fn decrypt_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decrypt", "game.iso", "pos.iso", "-o", "flag.iso"]);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "decrypt",
            "game.iso",
            "pos.iso",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "decrypt", "roms", "--max-depth", "2"]);
        assert!(result.is_err());
    }

    #[test]
    fn key_conflicts_with_recursive() {
        let result = Harness::try_parse_from(["bin", "decrypt", "roms", "-R", "--key", "k.dkey"]);
        assert!(result.is_err());
    }
}
