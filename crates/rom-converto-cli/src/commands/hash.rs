use crate::commands::BatchArgs;
use clap::Parser;
use std::path::PathBuf;

use crate::batch;
use crate::commands::support::{DispatchCtx, hash_single, require_dir};
use crate::util::ensure_input_exists;
use anyhow::Result;
use rom_converto_lib::util::{HASH_MEMBER_EXTS, parse_algos, resolve_for_hash};

/// Compute checksums for a file or, with --recursive, every file in a directory
///
/// This is a plain digest tool: it reads the bytes and prints the requested
/// hashes. It does no DAT or database lookup and compares nothing.
#[derive(Parser, Debug, Clone, Eq, PartialEq)]
#[command(
    after_long_help = "EXAMPLES:\n  Single file:   rom-converto hash game.iso\n  Pick digests:  rom-converto hash game.iso --algo sha1,sha256\n  Whole folder:  rom-converto hash -R ./roms --report hashes.csv\n"
)]
pub struct HashCommand {
    /// Input file path, or a directory with --recursive
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Comma-separated digests to compute: crc32, sha1, md5, sha256
    #[arg(long, value_name = "ALGOS", default_value = "crc32,sha1")]
    pub algo: String,

    /// Hash every file in INPUT, descending into subdirectories. Archives holding a known image are opened
    #[arg(long, short = 'R', default_value_t = false)]
    pub recursive: bool,

    #[command(flatten)]
    pub batch: BatchArgs,
}

/// Runs one `hash` subcommand.
pub async fn run(cmd: crate::commands::hash::HashCommand, ctx: DispatchCtx<'_>) -> Result<()> {
    let DispatchCtx {
        progress,
        total_progress,
        cache,
        cancel,
        ..
    } = ctx;
    let algos = parse_algos(&cmd.algo).map_err(|e| anyhow::anyhow!(e))?;
    if cmd.recursive {
        require_dir(&cmd.input)?;
        batch::hash_batch(
            &progress,
            &total_progress,
            &cmd.input,
            &algos,
            cmd.batch.max_depth,
            cmd.batch.report.as_deref(),
            cache,
            &cancel,
        )
        .await?;
    } else {
        ensure_input_exists(&cmd.input)?;
        let input = cmd.input.clone();
        let resolved =
            tokio::task::spawn_blocking(move || resolve_for_hash(&input, &HASH_MEMBER_EXTS))
                .await??;
        // A staged member lives in a temp dir and its digests are the
        // member's, not the archive's: the path-keyed cache must neither
        // answer for nor record it. `None` hashed the raw bytes at the
        // walked path, which the cache may serve.
        let cached = resolved.is_none().then(|| &**cache);
        let source = resolved
            .as_ref()
            .map_or_else(|| cmd.input.clone(), |r| r.path().to_path_buf());
        hash_single(
            &progress,
            &cmd.input,
            &source,
            &algos,
            cmd.batch.report.as_deref(),
            cached,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Parser, Debug)]
    struct Harness {
        #[command(subcommand)]
        cmd: Wrapper,
    }

    #[derive(clap::Subcommand, Debug)]
    enum Wrapper {
        Hash(HashCommand),
    }

    fn parse(args: &[&str]) -> HashCommand {
        let h = Harness::parse_from(args);
        let Wrapper::Hash(c) = h.cmd;
        c
    }

    #[test]
    fn defaults_algo_and_not_recursive() {
        let c = parse(&["bin", "hash", "game.iso"]);
        assert_eq!(c.algo, "crc32,sha1");
        assert!(!c.recursive);
        assert_eq!(c.batch.max_depth, None);
        assert_eq!(c.batch.report, None);
    }

    #[test]
    fn parses_recursive_depth_and_algo() {
        let c = parse(&[
            "bin",
            "hash",
            "roms",
            "-R",
            "--max-depth",
            "2",
            "--algo",
            "sha256",
        ]);
        assert!(c.recursive);
        assert_eq!(c.batch.max_depth, Some(2));
        assert_eq!(c.algo, "sha256");
    }

    #[test]
    fn parses_report_flag() {
        let c = parse(&["bin", "hash", "f", "--report", "out.json"]);
        assert_eq!(c.batch.report, Some(PathBuf::from("out.json")));
    }

    #[test]
    fn max_depth_requires_recursive() {
        let result = Harness::try_parse_from(["bin", "hash", "roms", "--max-depth", "2"]);
        assert!(result.is_err());
    }
}
