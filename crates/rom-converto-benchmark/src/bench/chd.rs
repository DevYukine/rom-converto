//! Disc image CHD compression, extraction, and verification benchmarked
//! against chdman.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bench::{BenchCtx, Scratch, file_size, remove_if_exists, require_input};
use crate::report::{Row, Table};
use crate::runner::{run_sided, run_timed};
use crate::tool::find_tool;

const KILL: &[&str] = &["chdman", "rom-converto"];

/// Benchmark disc image CHD compress, extract, and verify against chdman.
pub fn run(ctx: &BenchCtx, cue: Option<PathBuf>) -> Result<()> {
    let Some(cue) = cue else {
        bail!("no CHD input configured; pass --cue (or set ROMCONVERTO_BENCH_CD_CUE)");
    };
    require_input(&cue, "CHD cue")?;
    let chdman = if ctx.rom_converto_only {
        None
    } else {
        Some(find_tool("chdman", &ctx.rom_converto_dir)?)
    };
    let has_ext = chdman.is_some();

    let mut table = Table::new("CHD (disc image vs chdman)", "chdman");
    table.rom_converto_only = ctx.rom_converto_only;
    let scratch = Scratch::new(ctx, "bench-chd-")?;
    let dir = scratch.path();

    let ext_chd = dir.join("chdman.chd");
    let rc_chd = dir.join("romconverto.chd");

    let mut ext_size = 0u64;
    let mut rc_size = 0u64;
    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            remove_if_exists(&ext_chd);
            let mut cmd = Command::new(chdman.as_deref().expect("chdman"));
            cmd.args(["createcd", "--force", "-i"])
                .arg(&cue)
                .arg("-o")
                .arg(&ext_chd);
            let elapsed = run_timed(&mut cmd)?;
            ext_size = file_size(&ext_chd)?;
            Ok(elapsed)
        },
        &mut || {
            remove_if_exists(&rc_chd);
            let mut cmd = ctx.rc();
            cmd.args(["chd", "compress"])
                .arg(&cue)
                .arg(&rc_chd)
                .arg("-f");
            let elapsed = run_timed(&mut cmd)?;
            rc_size = file_size(&rc_chd)?;
            Ok(elapsed)
        },
    )?;
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared("CD compress", ext, rc_stats).with_size(rc_size, ext_size),
        None => Row::rc_only("CD compress", rc_stats).with_output(rc_size),
    });

    // Extract and verify operate on a shared rom-converto-produced CHD.
    let shared_chd = dir.join("shared.chd");
    let mut cmd = ctx.rc();
    cmd.args(["chd", "compress"])
        .arg(&cue)
        .arg(&shared_chd)
        .arg("-f");
    run_timed(&mut cmd)?;

    let ext_dir = dir.join("chdman_extract");
    let rc_dir = dir.join("romconverto_extract");
    let ext_cue = ext_dir.join("chdman_out.cue");
    let rc_cue = rc_dir.join("romconverto_out.cue");
    let mut ext_size = 0u64;
    let mut rc_size = 0u64;
    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            if ext_dir.exists() {
                std::fs::remove_dir_all(&ext_dir)?;
            }
            std::fs::create_dir_all(&ext_dir)?;
            let mut cmd = Command::new(chdman.as_deref().expect("chdman"));
            cmd.args(["extractcd", "--splitbin", "--force", "-i"])
                .arg(&shared_chd)
                .arg("-o")
                .arg(&ext_cue);
            let elapsed = run_timed(&mut cmd)?;
            ext_size = bin_file_size(&ext_dir)?;
            Ok(elapsed)
        },
        &mut || {
            if rc_dir.exists() {
                std::fs::remove_dir_all(&rc_dir)?;
            }
            std::fs::create_dir_all(&rc_dir)?;
            let mut cmd = ctx.rc();
            cmd.args(["chd", "extract"]).arg(&shared_chd).arg(&rc_cue);
            let elapsed = run_timed(&mut cmd)?;
            rc_size = bin_file_size(&rc_dir)?;
            Ok(elapsed)
        },
    )?;
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared("CD extract", ext, rc_stats).with_size(rc_size, ext_size),
        None => Row::rc_only("CD extract", rc_stats).with_output(rc_size),
    });

    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            let mut cmd = Command::new(chdman.as_deref().expect("chdman"));
            cmd.args(["verify", "-i"]).arg(&shared_chd);
            run_timed(&mut cmd)
        },
        &mut || {
            let mut cmd = ctx.rc();
            cmd.args(["chd", "verify"]).arg(&shared_chd);
            run_timed(&mut cmd)
        },
    )?;
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared("CD verify", ext, rc_stats),
        None => Row::rc_only("CD verify", rc_stats),
    });

    if let Some(chdman) = &chdman {
        sanity_info(chdman, &rc_chd);
    }
    table.print();
    Ok(())
}

fn bin_file_size(dir: &Path) -> Result<u64> {
    let mut size = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "bin") && entry.file_type()?.is_file() {
            size += file_size(&path)?;
        }
    }
    Ok(size)
}

/// Runs a `chdman info` parse check on a rom-converto CHD, as CHD.md documents.
fn sanity_info(chdman: &Path, chd: &Path) {
    let mut cmd = Command::new(chdman);
    cmd.args(["info", "-i"]).arg(chd);
    match run_timed(&mut cmd) {
        Ok(_) => println!("Sanity: chdman info accepts the rom-converto CHD"),
        Err(e) => println!("Sanity: chdman info rejected the rom-converto CHD: {e}"),
    }
}
