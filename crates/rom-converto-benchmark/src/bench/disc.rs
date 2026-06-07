use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bench::{BenchCtx, Scratch, file_size, remove_if_exists, require_input};
use crate::report::{Row, Table};
use crate::runner::{run_sided, run_timed};
use crate::tool::find_tool;

const KILL: &[&str] = &["DolphinTool", "rom-converto"];
const CHUNK_BYTES: &str = "131072"; // 128 KiB, Dolphin's RVZ default

/// Benchmark a disc-image RVZ pipeline (Wii or GameCube) against
/// DolphinTool. `subcommand` is the rom-converto verb (`rvl` / `dol`).
pub fn run(
    ctx: &BenchCtx,
    subcommand: &str,
    title: &str,
    iso: Option<PathBuf>,
    levels: &[i32],
) -> Result<()> {
    let Some(iso) = iso else {
        bail!("no {title} input configured. Pass --iso.");
    };
    require_input(&iso, title)?;
    let dolphin = if ctx.rom_converto_only {
        None
    } else {
        Some(find_tool("DolphinTool", &ctx.rom_converto_dir)?)
    };
    let has_ext = dolphin.is_some();

    let mut table = Table::new(format!("{title} (RVZ vs DolphinTool)"), "Dolphin");
    table.rom_converto_only = ctx.rom_converto_only;
    let scratch = Scratch::new(ctx, "bench-disc-")?;
    let dir = scratch.path();

    let ext_rvz = dir.join("dolphin.rvz");
    let rc_rvz = dir.join("romconverto.rvz");

    for &level in levels {
        let mut ext_size = 0u64;
        let mut rc_size = 0u64;
        let (ext_stats, rc_stats) = run_sided(
            &ctx.config,
            KILL,
            has_ext,
            &mut || {
                remove_if_exists(&ext_rvz);
                let mut cmd = Command::new(dolphin.as_deref().expect("DolphinTool"));
                cmd.args(["convert", "-i"])
                    .arg(&iso)
                    .arg("-o")
                    .arg(&ext_rvz)
                    .args(["-f", "rvz", "-c", "zstd", "-l"])
                    .arg(level.to_string())
                    .args(["-b", CHUNK_BYTES]);
                let elapsed = run_timed(&mut cmd)?;
                ext_size = file_size(&ext_rvz)?;
                Ok(elapsed)
            },
            &mut || {
                remove_if_exists(&rc_rvz);
                let mut cmd = ctx.rc();
                cmd.arg(subcommand)
                    .arg("compress")
                    .arg(&iso)
                    .arg(&rc_rvz)
                    .arg("-l")
                    .arg(level.to_string())
                    .args(["--chunk-size", CHUNK_BYTES]);
                let elapsed = run_timed(&mut cmd)?;
                rc_size = file_size(&rc_rvz)?;
                Ok(elapsed)
            },
        )?;
        let name = format!("Compress L{level}");
        table.rows.push(match ext_stats {
            Some(ext) => Row::compared(name, ext, rc_stats).with_size(rc_size, ext_size),
            None => Row::rc_only(name, rc_stats).with_output(rc_size),
        });
    }

    // Decompress: both tools expand the same rom-converto-produced RVZ
    // (byte identical to Dolphin's own encoder, so DolphinTool accepts it).
    let shared_rvz = dir.join("shared.rvz");
    let mut cmd = ctx.rc();
    cmd.arg(subcommand)
        .arg("compress")
        .arg(&iso)
        .arg(&shared_rvz)
        .args(["-l", "5", "--chunk-size", CHUNK_BYTES]);
    run_timed(&mut cmd)?;

    let ext_iso = dir.join("dolphin.iso");
    let rc_iso = dir.join("romconverto.iso");
    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            remove_if_exists(&ext_iso);
            let mut cmd = Command::new(dolphin.as_deref().expect("DolphinTool"));
            cmd.args(["convert", "-i"])
                .arg(&shared_rvz)
                .arg("-o")
                .arg(&ext_iso)
                .args(["-f", "iso"]);
            run_timed(&mut cmd)
        },
        &mut || {
            remove_if_exists(&rc_iso);
            let mut cmd = ctx.rc();
            cmd.arg(subcommand)
                .arg("decompress")
                .arg(&shared_rvz)
                .arg(&rc_iso);
            run_timed(&mut cmd)
        },
    )?;
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared("Decompress", ext, rc_stats),
        None => Row::rc_only("Decompress", rc_stats),
    });

    if let Some(dolphin) = &dolphin {
        sanity_header(dolphin, &shared_rvz);
    }
    table.print();
    Ok(())
}

/// `DolphinTool header` parse check on a rom-converto RVZ, as the .md
/// documents. Non-fatal: a failure is reported but does not drop results.
fn sanity_header(dolphin: &Path, rvz: &Path) {
    let mut cmd = Command::new(dolphin);
    cmd.args(["header", "-i"]).arg(rvz);
    match run_timed(&mut cmd) {
        Ok(_) => println!("sanity: DolphinTool header accepts the rom-converto RVZ"),
        Err(e) => println!("sanity: DolphinTool header rejected the rom-converto RVZ: {e}"),
    }
}
