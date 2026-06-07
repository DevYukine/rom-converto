use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bench::{BenchCtx, Scratch, file_size, remove_if_exists, require_input};
use crate::report::{Row, Table};
use crate::runner::{run_sided, run_timed};
use crate::tool::find_tool;

const KILL: &[&str] = &["chdman", "rom-converto"];

pub fn run(ctx: &BenchCtx, cue: Option<PathBuf>) -> Result<()> {
    let Some(cue) = cue else {
        bail!("no CHD input configured. Pass --cue (or set ROMCONVERTO_BENCH_CD_CUE).");
    };
    require_input(&cue, "CHD cue")?;
    let chdman = if ctx.rom_converto_only {
        None
    } else {
        Some(find_tool("chdman", &ctx.rom_converto_dir)?)
    };
    let has_ext = chdman.is_some();

    let mut table = Table::new("CHD (CD image vs chdman)", "chdman");
    table.rom_converto_only = ctx.rom_converto_only;
    let scratch = Scratch::new(ctx, "bench-chd-")?;
    let dir = scratch.path();

    let ext_chd = dir.join("chdman.chd");
    let rc_chd = dir.join("romconverto.chd");

    // Compress: chdman createcd  vs  rom-converto chd compress.
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

    let ext_cue = dir.join("chdman_out.cue");
    let ext_bin = dir.join("chdman_out.bin");
    let rc_cue = dir.join("romconverto_out.cue");
    let rc_bin = dir.join("romconverto_out.bin");
    let mut ext_size = 0u64;
    let mut rc_size = 0u64;
    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            remove_if_exists(&ext_cue);
            remove_if_exists(&ext_bin);
            let mut cmd = Command::new(chdman.as_deref().expect("chdman"));
            cmd.args(["extractcd", "--force", "-i"])
                .arg(&shared_chd)
                .arg("-o")
                .arg(&ext_cue);
            let elapsed = run_timed(&mut cmd)?;
            ext_size = file_size(&ext_bin)?;
            Ok(elapsed)
        },
        &mut || {
            remove_if_exists(&rc_cue);
            remove_if_exists(&rc_bin);
            let mut cmd = ctx.rc();
            cmd.args(["chd", "extract"]).arg(&shared_chd).arg(&rc_cue);
            let elapsed = run_timed(&mut cmd)?;
            rc_size = file_size(&rc_bin)?;
            Ok(elapsed)
        },
    )?;
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared("CD extract", ext, rc_stats).with_size(rc_size, ext_size),
        None => Row::rc_only("CD extract", rc_stats).with_output(rc_size),
    });

    // Verify.
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

/// `chdman info` parse check on a rom-converto CHD, as CHD.md documents.
fn sanity_info(chdman: &Path, chd: &Path) {
    let mut cmd = Command::new(chdman);
    cmd.args(["info", "-i"]).arg(chd);
    match run_timed(&mut cmd) {
        Ok(_) => println!("sanity: chdman info accepts the rom-converto CHD"),
        Err(e) => println!("sanity: chdman info rejected the rom-converto CHD: {e}"),
    }
}
