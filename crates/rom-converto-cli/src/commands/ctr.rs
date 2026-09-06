use crate::commands::info_command::InfoCommand;
use crate::commands::{ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, CTR_COMPRESS_EXTS, CTR_CONVERT_EXTS, CTR_CRYPT_EXTS, CTR_DECOMPRESS_EXTS,
    DispatchCtx, dry_run_ctr_scan, log_count_summary, log_single_summary, require_info_input,
    save_ctr_icon,
};
use crate::util::{
    SingleOutput, WriteDecision, ensure_input_exists, file_len, log_skipped, resolve_output,
    resolve_policy, resolve_single_output,
};
use crate::{batch, dry_run, info_print};
use anyhow::Result;
use rom_converto_lib::nintendo::ctr::convert::{
    convert_rom, convert_rom_batch, derive_converted_path,
};
use rom_converto_lib::nintendo::ctr::verify::{
    CtrVerifyOptions, CtrVerifyResult, verify_ctr, verify_ctr_batch,
};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom, compress_rom_batch, decompress_rom, decompress_rom_batch, derive_compressed_path,
    derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia, decrypt_rom, decrypt_rom_batch, derive_decrypted_path,
    derive_encrypted_path, encrypt_rom, encrypt_rom_batch, generate_ticket_from_cdn,
};
use rom_converto_lib::util::fs::{collect_files_with_exts, is_os_junk_dir};
use rom_converto_lib::util::{CancelToken, Tally, TallyDirection};
use std::path::Path;
use std::time::Instant;

/// Commands specific to CTR (3DS) formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum CtrCommands {
    CdnToCia(CdnToCiaCommand),
    GenerateCdnTicket(GenerateCdnTicketCommand),
    Decrypt(DecryptCommand),
    Encrypt(EncryptCommand),
    Compress(CompressRomCommand),
    Decompress(DecompressRomCommand),
    Verify(VerifyCommand),
    Convert(ConvertCommand),
    Info(InfoCommand),
}

/// Convert CDN content to CIA format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert CDN content to CIA format\n\nNote: By default the output CIA file is encrypted, if you want to decrypt it after conversion, use the --decrypt flag\nYou can also use the --compress flag to compress the CIA into Z3DS format (.zcia) after conversion, this requires the CIA to be decrypted first",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr cdn-to-cia ./cdn-content\n  Explicit output: rom-converto ctr cdn-to-cia ./cdn-content game.cia\n  Whole folder:    rom-converto ctr cdn-to-cia -R ./cdn-dumps --output-dir ./cia\n"
)]
pub struct CdnToCiaCommand {
    /// Path to the CDN content directory
    #[arg(value_name = "CDN_DIR")]
    pub cdn_dir: PathBuf,

    /// Output CIA file path, defaults to the folder name with .cia extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output CIA file path, defaults to the folder name with .cia extension
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    /// Write output into this directory using the derived filename. Created if missing. Works with --recursive
    #[arg(long = "output-dir", value_name = "DIR", conflicts_with_all = ["output", "output_flag"])]
    pub output_dir: Option<PathBuf>,

    /// Clean up after conversion by removing the original CDN files
    #[arg(long, short = 'C', default_value = "false")]
    pub cleanup: bool,

    /// Recursively iterate through all directories in the CDN_DIR directory and convert each to a CIA file
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Ensure that a Ticket file exists in the CDN_DIR directory, generating one if it does not
    #[arg(long, short = 'T', default_value = "false")]
    pub ensure_ticket_exists: bool,

    /// Decrypt the CIA file after conversion, useful for emulators like Azahar
    #[arg(long, short = 'D', default_value = "false")]
    pub decrypt: bool,

    /// Compress the CIA file into Z3DS format (.zcia) after conversion, requires the CIA to be decrypted
    #[arg(long, short = 'Z', default_value = "false")]
    pub compress: bool,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Generate a Ticket file from CDN content
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Generate a Ticket file from CDN content\n\nThis Ticket file is not official from Nintendo: it has non-important data like the console ID set to null. A CIA file built with this ticket will not work on a stock 3DS, but works fine on emulators or a 3DS with custom firmware.",
    after_long_help = "EXAMPLES:\n  Default name: rom-converto ctr generate-cdn-ticket ./cdn-content\n  Custom name:  rom-converto ctr generate-cdn-ticket ./cdn-content my-ticket.tik\n"
)]
pub struct GenerateCdnTicketCommand {
    /// Path to the CDN content directory
    #[arg(value_name = "CDN_DIR")]
    pub cdn_dir: PathBuf,

    /// Output Ticket file path
    #[arg(value_name = "OUTPUT", default_value = "ticket.tik")]
    pub output: PathBuf,
}

/// Decrypt an encrypted 3DS ROM file
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decrypt an encrypted 3DS ROM file\n\nSupported input formats: .cia, .3ds, .cci, .cxi\nThe format is auto-detected from the file contents.\n\nIf OUTPUT is omitted the decrypted file is written next to the input as <name>.decrypted.<ext>.\n\nUse --recursive/-R to point INPUT at a directory and decrypt every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each decrypted file is written next to its source as <name>.decrypted.<ext>.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr decrypt game.cia\n  Explicit output: rom-converto ctr decrypt game.3ds game.decrypted.3ds\n  Whole folder:    rom-converto ctr decrypt -R ./roms --output-dir ./decrypted\n"
)]
pub struct DecryptCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .3ds, .cci, or .cxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output decrypted file path, defaults to <name>.decrypted.<ext> next to the input (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output decrypted file path, defaults to <name>.decrypted.<ext> next to the input (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Encrypt a decrypted 3DS ROM file
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Encrypt a decrypted 3DS ROM file\n\nSupported input formats: .cia, .3ds, .cci, .cxi\nThe format is auto-detected from the file contents.\n\nIf OUTPUT is omitted the encrypted file is written next to the input as <name>.encrypted.<ext>.\n\nUse --recursive/-R to point INPUT at a directory and encrypt every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each encrypted file is written next to its source as <name>.encrypted.<ext>.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr encrypt game.decrypted.cia\n  Explicit output: rom-converto ctr encrypt game.decrypted.3ds game.encrypted.3ds\n  Whole folder:    rom-converto ctr encrypt -R ./roms --output-dir ./encrypted\n"
)]
pub struct EncryptCommand {
    /// Input decrypted ROM file path, or a directory when --recursive is set (.cia, .3ds, .cci, or .cxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output encrypted file path, defaults to <name>.encrypted.<ext> next to the input (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output encrypted file path, defaults to <name>.encrypted.<ext> next to the input (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Compress a decrypted 3DS ROM to the Z3DS format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a decrypted 3DS ROM to the Z3DS format\n\nSupported input formats: .cia, .cci, .3ds, .cxi, .3dsx\nOutput extensions: .zcia, .zcci, .zcxi, .z3dsx\n\nNote: only decrypted ROMs can be compressed, since encrypted ROMs have near-zero compression ratios.\n\nUse --recursive/-R to point INPUT at a directory and compress every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each output is written next to its source.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr compress game.cia\n  Explicit output: rom-converto ctr compress game.3ds game.z3ds\n  Whole folder:    rom-converto ctr compress -R ./roms --output-dir ./z3ds\n"
)]
pub struct CompressRomCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .cci, .3ds, .cxi, or .3dsx)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the extension prefixed by "z" (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output file path, defaults to the input path with the extension prefixed by "z" (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Zstd compression level (0 = library default, 22 = maximum ratio). Higher levels produce smaller output at the cost of compression time. Defaults to the library default when unset
    #[arg(short = 'l', long = "level", value_name = "LEVEL", value_parser = clap::value_parser!(i32).range(0..=22))]
    pub level: Option<i32>,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Compress an encrypted ROM anyway, even though it barely compresses. Decrypt first with: rom-converto ctr decrypt <INPUT>
    #[arg(long = "allow-encrypted", default_value_t = false)]
    pub allow_encrypted: bool,
}

/// Decompress a Z3DS file back to the original ROM format
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Decompress a Z3DS file back to the original ROM format\n\nSupported input formats: .zcia, .zcci, .zcxi, .z3dsx\nOutput extensions: .cia, .cci, .cxi, .3dsx\n\nUse --recursive/-R to point INPUT at a directory and decompress every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each output is written next to its source.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto ctr decompress game.zcia\n  Explicit output: rom-converto ctr decompress game.z3ds game.3ds\n  Whole folder:    rom-converto ctr decompress -R ./z3ds --output-dir ./roms\n"
)]
pub struct DecompressRomCommand {
    /// Input Z3DS file path, or a directory when --recursive is set (.zcia, .zcci, .zcxi, or .z3dsx)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the "z" prefix removed (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output file path, defaults to the input path with the "z" prefix removed (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Convert between CIA and CCI/3DS formats
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Convert between CIA and CCI/3DS formats\n\nDirection is auto-detected from the INPUT extension:\n  .cia       -> .3ds (CCI / NCSD)\n  .3ds, .cci -> .cia\n\nCCI/3DS to CIA produces an unsigned CIA with a zero title key, compatible with CFW (Luma3DS) and emulators (Citra/Lime3DS/Azahar). Not installable on stock 3DS.\n\nUse --recursive/-R to point INPUT at a directory and convert every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). In batch mode OUTPUT is ignored and each output is written next to its source with the opposite extension.",
    after_long_help = "EXAMPLES:\n  CIA to 3DS:      rom-converto ctr convert game.cia\n  Explicit output: rom-converto ctr convert game.3ds game.cia\n  Whole folder:    rom-converto ctr convert -R ./roms --output-dir ./converted\n"
)]
pub struct ConvertCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .3ds, or .cci)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output file path, defaults to the input path with the converted extension (ignored with --recursive)
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output file path, defaults to the input path with the converted extension (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,
}

/// Verify CTR ROM file integrity and legitimacy
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Verify a CTR ROM file's integrity by checking hashes and signatures\n\nSupported formats: .cia, .3ds, .cci, .cxi, .zcia, .zcci, .zcxi\n\nFor .cia files, classifies as:\n  - Legit: Both ticket and TMD signatures verify through Nintendo's cert chain\n  - Piratelegit: TMD signature verifies but ticket is forged\n  - Standard: Neither signature verifies\n\nFor .3ds/.cci files, verifies NCCH partition hashes (ExeFS, RomFS, ExHeader)\nCompressed Z3DS files are decompressed automatically before verification\n\nUse --recursive/-R to point INPUT at a directory and verify every matching file in it and its subdirectories; pass --max-depth N to limit the descent depth (1 = top level only). The command prints one line per file and a final tally.",
    after_long_help = "EXAMPLES:\n  Single file:  rom-converto ctr verify game.cia\n  Full check:   rom-converto ctr verify game.cia --full\n  Whole folder: rom-converto ctr verify -R ./roms\n"
)]
pub struct VerifyCommand {
    /// Input ROM file path, or a directory when --recursive is set (.cia, .3ds, .cci, .cxi, .zcia, .zcci, .zcxi)
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Also verify content hashes against the TMD (CIA only, slower)
    #[arg(
        long = "full",
        visible_alias = "verify-content",
        default_value_t = false
    )]
    pub verify_content: bool,

    /// Process all matching files in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value = "false")]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Runs one `ctr` subcommand.
pub async fn run(command: CtrCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        dry_run,
        skip_space_check,
        cancel,
        ..
    } = ctx;
    match command {
        CtrCommands::CdnToCia(cmd) => {
            let mut output = cmd.output_flag.or(cmd.output);
            let mut output_dir = cmd.output_dir;
            if cmd.recursive && dry_run {
                ensure_input_exists(&cmd.cdn_dir)?;
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
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
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let decision = resolve_output(&resolved, policy)?;
                if dry_run {
                    return dry_run::single(
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
                on_conflict: resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                ),
            };
            convert_cdn_to_cia(opts, &progress, &total_progress, cancel.clone()).await?
        }
        CtrCommands::GenerateCdnTicket(cmd) => {
            ensure_input_exists(&cmd.cdn_dir)?;
            if dry_run {
                let decision = WriteDecision::Write(cmd.output.clone());
                return dry_run::single(
                    "generate ticket",
                    &cmd.cdn_dir,
                    &cmd.output,
                    &decision,
                    None,
                    None,
                    None,
                );
            }
            generate_ticket_from_cdn(&cmd.cdn_dir, &cmd.output, &CancelToken::new()).await?
        }
        CtrCommands::Decrypt(cmd) => {
            if cmd.recursive {
                if !cmd.input.is_dir() {
                    anyhow::bail!(
                        "INPUT must be a directory when --recursive is set: {}",
                        cmd.input.display()
                    );
                }
                let files = collect_files_with_exts(
                    &cmd.input,
                    CTR_CRYPT_EXTS,
                    cmd.max_depth,
                    &CancelToken::new(),
                )?;
                if dry_run {
                    dry_run_ctr_scan(
                        "decrypt",
                        &files,
                        cmd.out.output_dir.as_deref(),
                        resolve_policy(
                            cmd.conflict.on_conflict,
                            cmd.conflict.force,
                            rom_converto_lib::util::ConflictPolicy::Error,
                        ),
                        derive_decrypted_path,
                    )?;
                    return Ok(());
                }
                if !skip_space_check {
                    let check_dir = cmd.out.output_dir.as_deref().unwrap_or(&cmd.input);
                    batch::space_preflight(&files, check_dir)?;
                }
                let tally = Tally::new();
                let count = files.len();
                decrypt_rom_batch(
                    &cmd.input,
                    cmd.out.output_dir.as_deref(),
                    &progress,
                    &total_progress,
                    cmd.max_depth,
                    cancel.clone(),
                )
                .await?;
                log_count_summary(count, tally);
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, CTR_CRYPT_EXTS)?;
                let input = resolved.path();
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let derived = derive_decrypted_path(resolved.output_basis());
                let output_ext = derived
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "decrypt",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived,
                        output_dir: cmd.out.output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: &output_ext,
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: None,
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
                let started = Instant::now();
                decrypt_rom(input, &output, &progress, cancel.clone()).await?;
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
                let files = collect_files_with_exts(
                    &cmd.input,
                    CTR_CRYPT_EXTS,
                    cmd.max_depth,
                    &CancelToken::new(),
                )?;
                if dry_run {
                    dry_run_ctr_scan(
                        "encrypt",
                        &files,
                        cmd.out.output_dir.as_deref(),
                        resolve_policy(
                            cmd.conflict.on_conflict,
                            cmd.conflict.force,
                            rom_converto_lib::util::ConflictPolicy::Error,
                        ),
                        derive_encrypted_path,
                    )?;
                    return Ok(());
                }
                if !skip_space_check {
                    let check_dir = cmd.out.output_dir.as_deref().unwrap_or(&cmd.input);
                    batch::space_preflight(&files, check_dir)?;
                }
                let tally = Tally::new();
                let count = files.len();
                encrypt_rom_batch(
                    &cmd.input,
                    cmd.out.output_dir.as_deref(),
                    &progress,
                    &total_progress,
                    cmd.max_depth,
                    cancel.clone(),
                )
                .await?;
                log_count_summary(count, tally);
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, CTR_CRYPT_EXTS)?;
                let input = resolved.path();
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let derived = derive_encrypted_path(resolved.output_basis());
                let output_ext = derived
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "encrypt",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived,
                        output_dir: cmd.out.output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: &output_ext,
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: None,
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
                let started = Instant::now();
                encrypt_rom(input, &output, &progress, cancel.clone()).await?;
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
                let files = collect_files_with_exts(
                    &cmd.input,
                    CTR_COMPRESS_EXTS,
                    cmd.max_depth,
                    &CancelToken::new(),
                )?;
                if dry_run {
                    dry_run_ctr_scan(
                        "compress",
                        &files,
                        cmd.out.output_dir.as_deref(),
                        resolve_policy(
                            cmd.conflict.on_conflict,
                            cmd.conflict.force,
                            rom_converto_lib::util::ConflictPolicy::Error,
                        ),
                        derive_compressed_path,
                    )?;
                    return Ok(());
                }
                if !skip_space_check {
                    let check_dir = cmd.out.output_dir.as_deref().unwrap_or(&cmd.input);
                    batch::space_preflight(&files, check_dir)?;
                }
                let tally = Tally::new();
                let count = files.len();
                compress_rom_batch(
                    &cmd.input,
                    cmd.level,
                    cmd.out.output_dir.as_deref(),
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
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let derived = derive_compressed_path(resolved.output_basis());
                let output_ext = derived
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "compress",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived,
                        output_dir: cmd.out.output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: &output_ext,
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: None,
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
                let started = Instant::now();
                compress_rom(
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
                let files = collect_files_with_exts(
                    &cmd.input,
                    CTR_DECOMPRESS_EXTS,
                    cmd.max_depth,
                    &CancelToken::new(),
                )?;
                if dry_run {
                    dry_run_ctr_scan(
                        "decompress",
                        &files,
                        cmd.out.output_dir.as_deref(),
                        resolve_policy(
                            cmd.conflict.on_conflict,
                            cmd.conflict.force,
                            rom_converto_lib::util::ConflictPolicy::Error,
                        ),
                        derive_decompressed_path,
                    )?;
                    return Ok(());
                }
                if !skip_space_check {
                    let check_dir = cmd.out.output_dir.as_deref().unwrap_or(&cmd.input);
                    batch::space_preflight(&files, check_dir)?;
                }
                let tally = Tally::new();
                let count = files.len();
                decompress_rom_batch(
                    &cmd.input,
                    cmd.out.output_dir.as_deref(),
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
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let derived = derive_decompressed_path(resolved.output_basis());
                let output_ext = derived
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "decompress",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived,
                        output_dir: cmd.out.output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: &output_ext,
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: None,
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
                let started = Instant::now();
                decompress_rom(input, &output, &progress, cancel.clone()).await?;
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
                let files = collect_files_with_exts(
                    &cmd.input,
                    CTR_CONVERT_EXTS,
                    cmd.max_depth,
                    &CancelToken::new(),
                )?;
                if dry_run {
                    dry_run_ctr_scan(
                        "convert",
                        &files,
                        cmd.out.output_dir.as_deref(),
                        resolve_policy(
                            cmd.conflict.on_conflict,
                            cmd.conflict.force,
                            rom_converto_lib::util::ConflictPolicy::Error,
                        ),
                        derive_converted_path,
                    )?;
                    return Ok(());
                }
                if !skip_space_check {
                    let check_dir = cmd.out.output_dir.as_deref().unwrap_or(&cmd.input);
                    batch::space_preflight(&files, check_dir)?;
                }
                let tally = Tally::new();
                let count = files.len();
                convert_rom_batch(
                    &cmd.input,
                    cmd.out.output_dir.as_deref(),
                    &progress,
                    &total_progress,
                    cmd.max_depth,
                    cancel.clone(),
                )
                .await?;
                log_count_summary(count, tally);
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, CTR_CONVERT_EXTS)?;
                let input = resolved.path();
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force,
                    rom_converto_lib::util::ConflictPolicy::Error,
                );
                let derived = derive_converted_path(resolved.output_basis());
                let output_ext = derived
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "convert",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived,
                        output_dir: cmd.out.output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: &output_ext,
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::None,
                        media: None,
                        missing_keys: None,
                        report: None,
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
                let started = Instant::now();
                convert_rom(input, &output, &progress, cancel.clone()).await?;
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
                    &CancelToken::new(),
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
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, CTR_CRYPT_EXTS)?;
                let result =
                    verify_ctr(resolved.path(), &opts, &progress, &CancelToken::new()).await?;
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
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::nintendo::ctr::info::read_info(resolved.path())?;
            if let Some(dir) = &cmd.save_icon {
                save_ctr_icon(&info, dir)?;
            }
            info_print::print(&rom_converto_lib::info::InfoResult::Ctr(info), cmd.json)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ConflictPolicyArg;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: CtrCommands,
    }

    #[test]
    fn output_flag_overrides_positional() {
        let h = Harness::parse_from(["bin", "decrypt", "game.cia", "-o", "out.cia"]);
        let CtrCommands::Decrypt(c) = h.cmd else {
            panic!("expected Decrypt");
        };
        assert_eq!(c.input, PathBuf::from("game.cia"));
        assert_eq!(c.output, None);
        assert_eq!(c.output_flag, Some(PathBuf::from("out.cia")));
    }

    #[test]
    fn output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "decrypt", "game.cia", "pos.cia", "-o", "flag.cia"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_force() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "-f"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.force);
    }

    #[test]
    fn parses_encrypt_output_dir() {
        let h = Harness::parse_from(["bin", "encrypt", "game.cia", "--output-dir", "out"]);
        let CtrCommands::Encrypt(c) = h.cmd else {
            panic!("expected Encrypt");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--output-dir", "out"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--on-conflict", "skip"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Skip));
    }

    #[test]
    fn parses_on_conflict_rename() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--on-conflict", "rename"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Rename));
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "-f"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.force);
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn force_and_on_conflict_conflict() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.cia", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.cia"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn parses_allow_encrypted() {
        let h = Harness::parse_from(["bin", "compress", "game.cia", "--allow-encrypted"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.allow_encrypted);
    }

    #[test]
    fn allow_encrypted_defaults_false() {
        let h = Harness::parse_from(["bin", "compress", "game.cia"]);
        let CtrCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(!c.allow_encrypted);
    }

    #[test]
    fn cdn_to_cia_recursive_parses_on_conflict() {
        let h = Harness::parse_from(["bin", "cdn-to-cia", "-R", "./cdn", "--on-conflict", "skip"]);
        let CtrCommands::CdnToCia(c) = h.cmd else {
            panic!("expected CdnToCia");
        };
        assert!(c.recursive);
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Skip));
    }

    #[test]
    fn output_dir_conflicts_with_output() {
        let result = Harness::try_parse_from([
            "bin",
            "compress",
            "game.cia",
            "pos.zcia",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn verify_full_and_alias() {
        let full = Harness::parse_from(["bin", "verify", "game.cia", "--full"]);
        let CtrCommands::Verify(c) = full.cmd else {
            panic!("expected Verify");
        };
        assert!(c.verify_content);

        let alias = Harness::parse_from(["bin", "verify", "game.cia", "--verify-content"]);
        let CtrCommands::Verify(c) = alias.cmd else {
            panic!("expected Verify");
        };
        assert!(c.verify_content);
    }
}
