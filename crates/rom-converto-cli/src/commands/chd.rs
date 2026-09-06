use crate::commands::cso::CsoFormatArg;
use crate::commands::info_command::InfoCommand;
use crate::commands::{BatchArgs, ConflictArgs, OutputArgs};
use clap::{Parser, Subcommand};
use rom_converto_lib::chd::ChdCodec;
use std::path::PathBuf;

use crate::commands::support::{
    ALL_IMAGE_EXTS, DispatchCtx, chd_media_label, finish_single, finish_single_sized, input_len,
    maybe_log_dvd_codec_tip, require_dir, require_info_input, resolve_chd_codecs,
    resolved_dvd_mode,
};
use crate::util::{
    SingleOutput, WriteDecision, ensure_input_exists, file_len, log_skipped, resolve_output,
    resolve_policy, resolve_single_output,
};
use crate::{batch, config, dry_run, info_print};
use anyhow::Result;
use rom_converto_lib::chd::{
    ChdOptions, DiscMode, convert_disc_to_chd, extract_from_chd, migrate_chd_to_v5,
    migrate_chd_to_v5_batch, verify_chd, verify_chd_batch,
};
use rom_converto_lib::cso::{CsoCompressOptions, CsoFormat};
use rom_converto_lib::pipeline::chd_to_cso;
use rom_converto_lib::util::fs::collect_files_with_exts;
use rom_converto_lib::util::{CancelToken, Tally, TallyDirection};
use std::path::Path;
use std::time::Instant;

/// A parsed `-c/--codecs` value. Aliased (rather than spelled as `Vec<ChdCodec>`
/// on the arg field) so clap-derive treats it as an opaque single value instead
/// of peeling `Vec` and expecting one `ChdCodec` per occurrence.
pub(crate) type ChdCodecList = Vec<ChdCodec>;

/// Parses a `-c/--codecs` value: a comma-separated chdman-style codec
/// list, validated for emptiness/duplicates/slot count. The CD-only-vs-DVD
/// check needs the resolved disc mode, so it happens later against the
/// lib's [`rom_converto_lib::chd::validate_codecs`].
pub(crate) fn parse_chd_codecs(s: &str) -> Result<ChdCodecList, String> {
    let codecs = rom_converto_lib::chd::parse_codec_list(s).map_err(|e| e.to_string())?;
    rom_converto_lib::chd::validate_codecs(&codecs, false).map_err(|e| e.to_string())?;
    Ok(codecs)
}

/// Commands specific to CHD formats
#[derive(Subcommand, Debug, Eq, PartialEq)]
pub enum ChdCommands {
    Compress(CompressCommand),
    Migrate(MigrateCommand),
    Extract(ExtractCommand),
    Verify(VerifyCommand),
    ToCso(ToCsoCommand),
    Info(InfoCommand),
}

/// Compress a disc image to a CHD (Compressed Hunks of Data) file
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Compress a disc image to a CHD (Compressed Hunks of Data) file\n\nA .cue input (with its .bin) becomes a CD-mode CHD. An .iso is probed for its console family: CD-media images (PS1, PS2-CD) become CD-mode CHDs with a single MODE1/2048 track (the chdman createcd equivalent), DVD-media images (PS2-DVD, PSP) become DVD-mode CHDs (the createdvd equivalent). An .avi input auto-detects LaserDisc mode (the chdman createld equivalent): avhuff-compressed audio/video with VBI metadata. The mode is picked automatically so the createcd/createdvd mixup cannot happen. Default codecs match chdman (CD: cdlz,cdzl,cdfl; DVD: lzma,zlib,huff,flac) and every emulator reads them, including AetherSX2/NetherSX2; pick your own with --codecs. LaserDisc CHDs always use avhuff and ignore --codecs, --level, and --hunk-size.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto chd compress game.cue\n  Explicit output: rom-converto chd compress game.iso out.chd\n  Whole folder:    rom-converto chd compress -R ./roms --output-dir ./chd\n"
)]
pub struct CompressCommand {
    /// Input image (.cue, .iso with CD/DVD media auto-detected, or .avi for LaserDisc), or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output chd file path, defaults to the input path with extension replaced by .chd
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output chd file path, defaults to the input path with extension replaced by .chd
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Force DVD mode (.iso input only)
    #[arg(long, conflicts_with_all = ["cd", "ld"])]
    pub dvd: bool,

    /// Force CD mode (a .cue, or a CD-media .iso)
    #[arg(long, conflicts_with_all = ["dvd", "ld"])]
    pub cd: bool,

    /// Force LaserDisc mode (.avi input only)
    #[arg(long, conflicts_with_all = ["cd", "dvd"])]
    pub ld: bool,

    /// DVD hunk size in bytes, a multiple of 2048. Defaults to 4096, or 2048 for detected PSP images (PPSSPP reads 2048-byte blocks)
    #[arg(long, value_name = "BYTES")]
    pub hunk_size: Option<u32>,

    /// Codec list for the CHD header's compressor slots: comma-separated chdman-style names, at most 4 of zlib, zstd, lzma, huff, flac, cdzl, cdzs, cdlz, cdfl. Defaults to cdlz,cdzl,cdfl for CD-mode and lzma,zlib,huff,flac for DVD-mode (chdman parity)
    #[arg(short = 'c', long = "codecs", value_name = "LIST", value_parser = parse_chd_codecs)]
    pub codecs: Option<ChdCodecList>,

    /// Compression level in 1..=22. zstd uses the level directly; zlib and lzma cap at 9. Unset uses per-codec defaults (zstd 19, lzma 8, zlib 9)
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    pub level: Option<i32>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Compress every .cue and .iso found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Rewrite a legacy CHD as a version 5 CHD
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Rewrite a legacy CHD as a version 5 CHD\n\nConverts CHD format versions 1 through 4 into version 5. The raw data and every metadata entry are copied through unchanged, so the image content is untouched and only the container and its compression are rebuilt. A version 5 input is rejected. Hunk size defaults to the source's, and codecs default to chdman's CD or DVD set depending on the source's unit size.\n\nInput and output share the .chd extension, so the derived output name gets a v5 infix (game.chd becomes game.v5.chd) and the source is left alone. Pass --in-place to replace the source instead.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto chd migrate old.chd\n  Explicit output: rom-converto chd migrate old.chd new.chd\n  In place:        rom-converto chd migrate --in-place old.chd\n  Whole folder:    rom-converto chd migrate -R ./chd --output-dir ./v5\n"
)]
pub struct MigrateCommand {
    /// Input CHD file, or a directory of .chd files when --recursive is set
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output chd file path
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output chd file path
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Replace the source file with the migrated v5 CHD instead of writing a .v5.chd sibling
    #[arg(long = "in-place", conflicts_with_all = ["output", "output_flag", "output_dir", "output_template"])]
    pub in_place: bool,

    /// Hunk size in bytes, a multiple of the source's unit size. Defaults to the source's hunk size
    #[arg(long, value_name = "BYTES")]
    pub hunk_size: Option<u32>,

    /// Codec list for the CHD header's compressor slots: comma-separated chdman-style names, at most 4 of zlib, zstd, lzma, huff, flac, cdzl, cdzs, cdlz, cdfl. Defaults to cdlz,cdzl,cdfl for CD-mode sources and lzma,zlib,huff,flac for DVD-mode sources (chdman parity)
    #[arg(short = 'c', long = "codecs", value_name = "LIST", value_parser = parse_chd_codecs)]
    pub codecs: Option<ChdCodecList>,

    /// Compression level in 1..=22. zstd uses the level directly; zlib and lzma cap at 9. Unset uses per-codec defaults (zstd 19, lzma 8, zlib 9)
    #[arg(
        short = 'l',
        long = "level",
        value_name = "LEVEL",
        value_parser = clap::value_parser!(i32).range(1..=22)
    )]
    pub level: Option<i32>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Migrate every .chd found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Extract files from a CHD file to a specified output directory
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto chd extract game.chd game.cue\n  Explicit output: rom-converto chd extract game.chd --output-dir ./extracted\n  Whole folder:    rom-converto chd extract -R ./chds --output-dir ./extracted\n"
)]
pub struct ExtractCommand {
    /// Input CHD file, or a directory of .chd files when --recursive is set
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path for extracted files (ignored with --recursive)
    #[arg(
        value_name = "OUTPUT",
        required_unless_present_any = ["recursive", "output_flag", "output_dir"]
    )]
    pub output: Option<PathBuf>,

    /// Output path for extracted files (ignored with --recursive)
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Optional parent CHD file (for CHDs that reference a parent); not allowed with --recursive
    #[arg(long, short = 'p', value_name = "PARENT", conflicts_with = "recursive")]
    pub parent: Option<PathBuf>,

    /// Extract every .chd in INPUT and its subdirectories; outputs go beside each input
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Write a run report to FILE. Format inferred from the extension: .csv, .json, .html or .htm. Unknown extensions default to JSON. The file is overwritten directly
    #[arg(long = "report", value_name = "FILE")]
    pub report: Option<PathBuf>,
}

/// Extract a CHD straight to a CSO or ZSO, through a temporary ISO
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    long_about = "Extract a CHD straight to a CSO or ZSO, through a temporary ISO\n\nOnly DVD-mode CHDs (PS2 DVD, PSP UMD) qualify: a CD-mode CHD has no flat ISO for CSO/ZSO to hold. Extracts to a temporary ISO, then compresses it, and always deletes the temporary ISO afterward.",
    after_long_help = "EXAMPLES:\n  Single file:     rom-converto chd to-cso game.chd\n  Explicit output: rom-converto chd to-cso game.chd game.zso --format zso\n  Whole folder:    rom-converto chd to-cso -R ./chd --output-dir ./cso\n"
)]
pub struct ToCsoCommand {
    /// Input CHD path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Output path, defaults to the input format's extension
    #[arg(value_name = "OUTPUT")]
    pub output: Option<PathBuf>,

    /// Output path, defaults to the input format's extension
    #[arg(
        short = 'o',
        long = "output",
        value_name = "OUTPUT",
        conflicts_with = "output"
    )]
    pub output_flag: Option<PathBuf>,

    #[command(flatten)]
    pub out: OutputArgs,

    /// Output container format
    #[arg(long, value_enum, default_value_t = CsoFormatArg::Cso)]
    pub format: CsoFormatArg,

    /// Block size in bytes, a power of two. Defaults to 2048, or 16384 for inputs 2 GiB and beyond (matching maxcso)
    #[arg(long, value_name = "BYTES")]
    pub block_size: Option<u32>,

    #[command(flatten)]
    pub conflict: ConflictArgs,

    /// Convert every .chd found in the INPUT directory and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Verify the integrity of a CHD file
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:  rom-converto chd verify game.chd\n  Whole folder: rom-converto chd verify -R ./chds\n"
)]
pub struct VerifyCommand {
    /// Input CHD file, or a directory of .chd files when --recursive is set
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Optional parent CHD file (for CHDs that reference a parent); not allowed with --recursive
    #[arg(long, short = 'p', value_name = "PARENT", conflicts_with = "recursive")]
    pub parent: Option<PathBuf>,

    /// Fix incorrect SHA1 values in the header
    #[arg(long)]
    pub fix: bool,

    /// Verify every .chd in INPUT and its subdirectories
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    /// Maximum directory depth when --recursive is set. 1 = top level only. Omit for unlimited
    #[arg(long = "max-depth", value_name = "N", requires = "recursive")]
    pub max_depth: Option<usize>,
}

/// Runs one `chd` subcommand.
pub async fn run(command: ChdCommands, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        effective,
        dry_run,
        skip_space_check,
        cancel,
        cache,
        ..
    } = ctx;
    match command {
        ChdCommands::Compress(cmd) => {
            let eff = &effective.chd;
            let mut opts = ChdOptions {
                hunk_size: cmd.hunk_size.or(eff.hunk_size),
                codecs: resolve_chd_codecs(cmd.codecs.clone(), &eff.codecs)?,
                level: cmd.level.or(eff.level),
                force: cmd.conflict.force,
            };
            let codecs_set = opts.codecs.is_some();
            let output_dir = cmd
                .out
                .output_dir
                .clone()
                .or_else(|| eff.output_dir.clone());
            let report = cmd.batch.report.clone().or_else(|| eff.report.clone());
            let fallback = config::policy_fallback(&eff.on_conflict)?;
            let mode = if cmd.dvd {
                Some(DiscMode::Dvd)
            } else if cmd.cd {
                Some(DiscMode::Cd)
            } else if cmd.ld {
                Some(DiscMode::Ld)
            } else {
                None
            };
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
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
                batch::chd_compress(&run, opts, mode, cache).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved =
                    rom_converto_lib::util::resolve_input(&cmd.input, &["iso", "cue", "avi"])?;
                let input = resolved.path();
                maybe_log_dvd_codec_tip(resolved_dvd_mode(mode, input), codecs_set);
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
                let media = if dry_run {
                    chd_media_label(input)
                } else {
                    None
                };
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "compress",
                        cli_input: &cmd.input,
                        input,
                        explicit: cmd.output_flag.or(cmd.output),
                        derived: resolved.output_basis().with_extension("chd"),
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: "chd",
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::Chd,
                        media: media.as_deref(),
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
                opts.force = true;
                let out_path = output.clone();
                let started = Instant::now();
                convert_disc_to_chd(
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
        ChdCommands::Migrate(cmd) => {
            let eff = &effective.chd;
            let mut opts = ChdOptions {
                hunk_size: cmd.hunk_size.or(eff.hunk_size),
                codecs: resolve_chd_codecs(cmd.codecs.clone(), &eff.codecs)?,
                level: cmd.level.or(eff.level),
                force: cmd.conflict.force || cmd.in_place,
            };
            // --in-place names the destination itself, so a configured
            // output directory must not pull the write elsewhere.
            let output_dir = if cmd.in_place {
                None
            } else {
                cmd.out
                    .output_dir
                    .clone()
                    .or_else(|| eff.output_dir.clone())
            };
            let report = cmd.batch.report.clone().or_else(|| eff.report.clone());
            let fallback = config::policy_fallback(&eff.on_conflict)?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                if dry_run {
                    // The lib batch migrator writes as it walks, so the
                    // plan is enumerated here instead.
                    let inputs = collect_files_with_exts(
                        &cmd.input,
                        &["chd"],
                        cmd.batch.max_depth,
                        &CancelToken::new(),
                    )?;
                    let mut tally = Tally::new();
                    let mut records = Vec::with_capacity(inputs.len());
                    for file in &inputs {
                        let desired = match (cmd.in_place, output_dir.as_deref()) {
                            (true, _) => file.clone(),
                            (false, Some(_)) => rom_converto_lib::util::place_in_dir_mirrored(
                                file,
                                &cmd.input,
                                output_dir.as_deref(),
                            ),
                            (false, None) => rom_converto_lib::chd::migrated_chd_path(file),
                        };
                        let decision = WriteDecision::Write(desired.clone());
                        dry_run::log_plan("migrate", file, &desired, &decision, None, None);
                        dry_run::record(&mut tally, file, &decision);
                        records.push(dry_run::report_record("migrate", file, &desired, &decision));
                    }
                    return dry_run::finish(&tally, &records, report.as_deref());
                }
                migrate_chd_to_v5_batch(
                    &progress,
                    &total_progress,
                    &cmd.input,
                    opts,
                    output_dir.as_deref(),
                    cmd.batch.max_depth,
                    cmd.in_place,
                )
                .await?
            } else {
                ensure_input_exists(&cmd.input)?;
                if cmd.in_place && rom_converto_lib::util::is_archive_path(&cmd.input) {
                    anyhow::bail!(
                        "--in-place needs a plain .chd, not an archive: {}",
                        cmd.input.display()
                    );
                }
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["chd"])?;
                let input = resolved.path();
                let output = if cmd.in_place {
                    input.to_path_buf()
                } else {
                    match cmd.output_flag.clone().or_else(|| cmd.output.clone()) {
                        Some(p) => p,
                        None => {
                            if !dry_run && let Some(dir) = output_dir.as_deref() {
                                std::fs::create_dir_all(dir)?;
                            }
                            match cmd.out.output_template.as_deref() {
                                Some(tmpl) => crate::util::templated_output(
                                    tmpl,
                                    input,
                                    output_dir.as_deref(),
                                    "chd",
                                    None,
                                    dry_run,
                                )?,
                                None => match output_dir.as_deref() {
                                    Some(dir) => rom_converto_lib::util::place_in_dir(
                                        &resolved.output_basis().with_extension("chd"),
                                        Some(dir),
                                    ),
                                    None => rom_converto_lib::chd::migrated_chd_path(
                                        resolved.output_basis(),
                                    ),
                                },
                            }
                        }
                    }
                };
                let policy = resolve_policy(
                    cmd.conflict.on_conflict,
                    cmd.conflict.force || cmd.in_place,
                    fallback,
                );
                let decision = resolve_output(&output, policy)?;
                if dry_run {
                    return dry_run::single(
                        "migrate",
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
                opts.force = true;
                let in_path = input.to_path_buf();
                let out_path = output.clone();
                let started = Instant::now();
                // --in-place overwrites the source, so its size has to be
                // taken before the write to keep the summary honest.
                let in_bytes = input_len(&cmd.input);
                migrate_chd_to_v5(&progress, in_path, output, opts, cancel.clone()).await?;
                finish_single_sized(
                    &cmd.input,
                    in_bytes,
                    &out_path,
                    TallyDirection::Compress,
                    "migrate",
                    started,
                    report.as_deref(),
                )?;
            }
        }
        ChdCommands::Extract(cmd) => {
            let eff = &effective.chd;
            let output_dir = cmd
                .out
                .output_dir
                .clone()
                .or_else(|| eff.output_dir.clone());
            let report = cmd.report.clone().or_else(|| eff.report.clone());
            let fallback = config::policy_fallback(&eff.on_conflict)?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
                let run = batch::BatchRun {
                    progress: &progress,
                    total_progress: &total_progress,
                    input_dir: &cmd.input,
                    policy,
                    output_dir: output_dir.as_deref(),
                    output_template: cmd.out.output_template.as_deref(),
                    max_depth: cmd.max_depth,
                    dry_run,
                    skip_space_check,
                    report_path: report.as_deref(),
                    cancel: &cancel,
                };
                batch::chd_extract(&run, cmd.parent).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["chd"])?;
                let input = resolved.path();
                let explicit = cmd.output_flag.or(cmd.output);
                if explicit.is_none() && output_dir.is_none() {
                    anyhow::bail!(
                        "chd extract needs an OUTPUT path or --output-dir without --recursive"
                    );
                }
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
                let Some(output) = resolve_single_output(
                    SingleOutput {
                        operation: "extract",
                        cli_input: &cmd.input,
                        input,
                        explicit,
                        derived: resolved.output_basis().with_extension(""),
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
                let in_path = input.to_path_buf();
                let out_path = output.clone();
                let started = Instant::now();
                extract_from_chd(
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
                    CancelToken::new(),
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
                force: cmd.conflict.force,
            };
            let output_dir = cmd
                .out
                .output_dir
                .clone()
                .or_else(|| eff.output_dir.clone());
            let report = cmd.batch.report.clone().or_else(|| eff.report.clone());
            let fallback = config::policy_fallback(&eff.on_conflict)?;
            if cmd.recursive {
                require_dir(&cmd.input)?;
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
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
                batch::chd_to_cso(&run, opts, cache).await?
            } else {
                ensure_input_exists(&cmd.input)?;
                let resolved = rom_converto_lib::util::resolve_input(&cmd.input, &["chd"])?;
                let input = resolved.path();
                let policy = resolve_policy(cmd.conflict.on_conflict, cmd.conflict.force, fallback);
                let media = format.name();
                let derived = resolved.output_basis().with_extension(format.extension());
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
                        output_dir: output_dir.as_deref(),
                        output_template: cmd.out.output_template.as_deref(),
                        output_ext: &output_ext,
                        keys_path: None,
                        policy,
                        verify: crate::util::OutputVerify::Cso,
                        media: Some(media),
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
                    let required = rom_converto_lib::chd::info::read_info(input)
                        .map(|info| info.logical_bytes)
                        .unwrap_or_else(|_| file_len(input));
                    batch::space_preflight_for_size(required, check_dir)?;
                }
                opts.force = true;
                let in_path = input.to_path_buf();
                let out_path = output.clone();
                let started = Instant::now();
                chd_to_cso(&progress, in_path, output, opts, cancel.clone()).await?;
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
                anyhow::bail!("--keys is only supported by nx, wup, and ps3 info");
            }
            if cmd.save_icon.is_some() {
                anyhow::bail!(
                    "--save-icon is not supported for chd: the format has no embedded artwork"
                );
            }
            let input = require_info_input(&cmd.input)?;
            ensure_input_exists(input)?;
            let resolved = rom_converto_lib::util::resolve_input(input, ALL_IMAGE_EXTS)?;
            let info = rom_converto_lib::chd::info::read_info(resolved.path())?;
            info_print::print(&rom_converto_lib::info::InfoResult::Chd(info), cmd.json)?;
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
        cmd: ChdCommands,
    }

    #[test]
    fn parses_compress_defaults() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.input, PathBuf::from("game.iso"));
        assert_eq!(c.output, None);
        assert!(!c.dvd && !c.cd && !c.conflict.force && !c.recursive);
        assert_eq!(c.hunk_size, None);
        assert_eq!(c.codecs, None);
        assert_eq!(c.level, None);
    }

    #[test]
    fn parses_compress_dvd_flags() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "game.iso",
            "game.chd",
            "--dvd",
            "--hunk-size",
            "2048",
            "-R",
        ]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.output, Some(PathBuf::from("game.chd")));
        assert!(c.dvd && c.recursive);
        assert_eq!(c.hunk_size, Some(2048));
    }

    #[test]
    fn parses_compress_codecs_and_level() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "game.iso",
            "--codecs",
            "zstd,lzma,zlib,flac",
            "--level",
            "12",
        ]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(
            c.codecs,
            Some(vec![
                ChdCodec::Zstd,
                ChdCodec::Lzma,
                ChdCodec::Zlib,
                ChdCodec::Flac
            ])
        );
        assert_eq!(c.level, Some(12));
    }

    #[test]
    fn rejects_unknown_codec_name() {
        let result = Harness::try_parse_from(["bin", "compress", "game.iso", "--codecs", "bogus"]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_too_many_codecs() {
        let result = Harness::try_parse_from([
            "bin",
            "compress",
            "game.iso",
            "--codecs",
            "zlib,zstd,lzma,huff,flac",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_level_out_of_range() {
        let result = Harness::try_parse_from(["bin", "compress", "game.iso", "--level", "23"]);
        assert!(result.is_err());
    }

    #[test]
    fn cd_codec_with_dvd_rejected_by_lib_validation() {
        let h = Harness::parse_from([
            "bin",
            "compress",
            "game.iso",
            "--dvd",
            "--codecs",
            "cdlz,cdzl,cdfl",
        ]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        let codecs = c.codecs.expect("codecs parsed");
        assert!(rom_converto_lib::chd::validate_codecs(&codecs, true).is_err());
    }

    #[test]
    fn rejects_cd_and_dvd_together() {
        let result = Harness::try_parse_from(["bin", "compress", "x.cue", "--cd", "--dvd"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_ld_flag() {
        let h = Harness::parse_from(["bin", "compress", "--ld", "game.avi"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.ld && !c.cd && !c.dvd);
    }

    #[test]
    fn rejects_ld_and_cd_together() {
        let result = Harness::try_parse_from(["bin", "compress", "game.avi", "--ld", "--cd"]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_ld_and_dvd_together() {
        let result = Harness::try_parse_from(["bin", "compress", "game.avi", "--ld", "--dvd"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_cd_flag_on_iso() {
        let h = Harness::parse_from(["bin", "compress", "--cd", "game.iso"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.cd && !c.dvd);
        assert_eq!(c.input, PathBuf::from("game.iso"));
    }

    #[test]
    fn verify_parses_recursive_flag() {
        let h = Harness::parse_from(["bin", "verify", "-R", "dir"]);
        let ChdCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
        assert_eq!(c.input, PathBuf::from("dir"));
    }

    #[test]
    fn extract_output_optional_with_recursive() {
        let h = Harness::parse_from(["bin", "extract", "-R", "dir"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert!(c.recursive);
        assert!(c.output.is_none());
        assert!(Harness::try_parse_from(["bin", "extract", "in.chd"]).is_err());
    }

    #[test]
    fn extract_output_flag_satisfies_requirement() {
        let h = Harness::parse_from(["bin", "extract", "in.chd", "-o", "out.cue"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert!(c.output.is_none());
        assert_eq!(c.output_flag, Some(PathBuf::from("out.cue")));
    }

    #[test]
    fn extract_output_flag_conflicts_with_positional() {
        let result =
            Harness::try_parse_from(["bin", "extract", "in.chd", "pos.cue", "-o", "flag.cue"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_compress_output_dir() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--output-dir", "out"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
        assert_eq!(c.output, None);
    }

    #[test]
    fn max_depth_parses_with_recursive() {
        let h = Harness::parse_from(["bin", "verify", "-R", "--max-depth", "2", "dir"]);
        let ChdCommands::Verify(c) = h.cmd else {
            panic!("expected Verify");
        };
        assert!(c.recursive);
        assert_eq!(c.max_depth, Some(2));
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "verify", "--max-depth", "2", "dir"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_on_conflict_skip() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--on-conflict", "skip"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Skip));
    }

    #[test]
    fn parses_on_conflict_rename() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--on-conflict", "rename"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.conflict.on_conflict, Some(ConflictPolicyArg::Rename));
    }

    #[test]
    fn force_still_accepted() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "-f"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.force);
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn force_and_on_conflict_conflict() {
        let result =
            Harness::try_parse_from(["bin", "compress", "game.iso", "-f", "--on-conflict", "skip"]);
        assert!(result.is_err());
    }

    #[test]
    fn on_conflict_absent_is_none() {
        let h = Harness::parse_from(["bin", "compress", "game.iso"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert!(c.conflict.on_conflict.is_none());
    }

    #[test]
    fn extract_output_dir_satisfies_requirement() {
        let h = Harness::parse_from(["bin", "extract", "in.chd", "--output-dir", "out"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert!(c.output.is_none());
        assert_eq!(c.out.output_dir, Some(PathBuf::from("out")));
    }

    #[test]
    fn parses_compress_report_flag() {
        let h = Harness::parse_from(["bin", "compress", "game.iso", "--report", "out.json"]);
        let ChdCommands::Compress(c) = h.cmd else {
            panic!("expected Compress");
        };
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn parses_extract_report_flag() {
        let h = Harness::parse_from(["bin", "extract", "in.chd", "out", "--report", "out.csv"]);
        let ChdCommands::Extract(c) = h.cmd else {
            panic!("expected Extract");
        };
        assert_eq!(c.report, Some(PathBuf::from("out.csv")));
    }

    #[test]
    fn parses_to_cso_defaults() {
        let h = Harness::parse_from(["bin", "to-cso", "game.chd"]);
        let ChdCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert_eq!(c.input, PathBuf::from("game.chd"));
        assert_eq!(c.output, None);
        assert_eq!(c.format, CsoFormatArg::Cso);
        assert!(!c.conflict.force && !c.recursive);
    }

    #[test]
    fn parses_to_cso_format_and_recursive() {
        let h = Harness::parse_from(["bin", "to-cso", "game.chd", "--format", "zso", "-R"]);
        let ChdCommands::ToCso(c) = h.cmd else {
            panic!("expected ToCso");
        };
        assert_eq!(c.format, CsoFormatArg::Zso);
        assert!(c.recursive);
    }

    #[test]
    fn to_cso_output_dir_conflicts_with_positional() {
        let result = Harness::try_parse_from([
            "bin",
            "to-cso",
            "game.chd",
            "pos.cso",
            "--output-dir",
            "out",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_migrate_with_explicit_output() {
        let h = Harness::parse_from(["bin", "migrate", "in.chd", "out.chd"]);
        let ChdCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert_eq!(c.input, PathBuf::from("in.chd"));
        assert_eq!(c.output, Some(PathBuf::from("out.chd")));
        assert!(!c.in_place && !c.recursive && !c.conflict.force);
        assert_eq!(c.hunk_size, None);
        assert_eq!(c.codecs, None);
        assert_eq!(c.level, None);
    }

    #[test]
    fn parses_migrate_in_place() {
        let h = Harness::parse_from(["bin", "migrate", "--in-place", "in.chd"]);
        let ChdCommands::Migrate(c) = h.cmd else {
            panic!("expected Migrate");
        };
        assert!(c.in_place);
        assert_eq!(c.output, None);
    }

    #[test]
    fn migrate_in_place_conflicts_with_output() {
        let result =
            Harness::try_parse_from(["bin", "migrate", "in.chd", "--in-place", "-o", "out.chd"]);
        assert!(result.is_err());
    }

    #[test]
    fn to_cso_max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "to-cso", "dir", "--max-depth", "2"]);
        assert!(result.is_err());
    }
}
