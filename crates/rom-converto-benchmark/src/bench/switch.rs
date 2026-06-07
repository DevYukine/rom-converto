use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bench::{
    BenchCtx, Scratch, file_size, find_one_by_ext, remove_by_ext, remove_if_exists, require_input,
    sha256_file,
};
use crate::report::{Row, Table};
use crate::runner::{bench_op, run_sided, run_timed};
use crate::tool::find_tool;

const KILL: &[&str] = &["nsz", "rom-converto"];

#[derive(Clone, Copy)]
enum Container {
    Nsp,
    Xci,
}

impl Container {
    fn label(self) -> &'static str {
        match self {
            Container::Nsp => "NSP",
            Container::Xci => "XCI",
        }
    }
    fn rc_mode(self) -> &'static str {
        match self {
            Container::Nsp => "solid",
            Container::Xci => "block",
        }
    }
    fn block(self) -> bool {
        matches!(self, Container::Xci)
    }
    fn compressed_ext(self) -> &'static str {
        match self {
            Container::Nsp => "nsz",
            Container::Xci => "xcz",
        }
    }
    fn decompressed_ext(self) -> &'static str {
        match self {
            Container::Nsp => "nsp",
            Container::Xci => "xci",
        }
    }
}

pub fn run(
    ctx: &BenchCtx,
    nsp: Option<PathBuf>,
    xci: Option<PathBuf>,
    keys: Option<PathBuf>,
) -> Result<()> {
    let nsz = if ctx.rom_converto_only {
        None
    } else {
        Some(find_tool("nsz", &ctx.rom_converto_dir)?)
    };
    let mut table = Table::new("Switch (NSP/XCI vs nsz)", "nsz");
    table.rom_converto_only = ctx.rom_converto_only;

    if let Some(nsp) = nsp {
        require_input(&nsp, "Switch NSP")?;
        bench_container(
            ctx,
            nsz.as_deref(),
            &nsp,
            keys.as_deref(),
            Container::Nsp,
            &mut table,
        )?;
    }
    if let Some(xci) = xci {
        require_input(&xci, "Switch XCI")?;
        bench_container(
            ctx,
            nsz.as_deref(),
            &xci,
            keys.as_deref(),
            Container::Xci,
            &mut table,
        )?;
    }

    if table.rows.is_empty() {
        bail!(
            "no Switch inputs configured. Pass --nsp and/or --xci \
             (or set ROMCONVERTO_BENCH_NX_NSP / ROMCONVERTO_BENCH_NX_XCI)."
        );
    }
    table.print();
    Ok(())
}

fn bench_container(
    ctx: &BenchCtx,
    nsz: Option<&Path>,
    input: &Path,
    keys: Option<&Path>,
    container: Container,
    table: &mut Table,
) -> Result<()> {
    let scratch = Scratch::new(ctx, "bench-nx-")?;
    let dir = scratch.path();
    let has_ext = nsz.is_some();

    let cext = container.compressed_ext();
    let dext = container.decompressed_ext();
    let rc_compressed = dir.join(format!("romconverto.{cext}"));
    let rc_decompressed = dir.join(format!("romconverto.{dext}"));

    // Compress: nsz -C [-B] -l 18 -o <dir> <input>  vs  rom-converto nx compress.
    let mut ext_size = 0u64;
    let mut rc_size = 0u64;
    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            remove_by_ext(dir, cext)?;
            let mut cmd = nsz_compress(nsz.expect("nsz"), container, dir, input);
            let elapsed = run_timed(&mut cmd)?;
            ext_size = file_size(&find_one_by_ext(dir, cext)?)?;
            Ok(elapsed)
        },
        &mut || {
            remove_if_exists(&rc_compressed);
            let mut cmd = rc_compress(ctx, container, keys, &rc_compressed, input);
            let elapsed = run_timed(&mut cmd)?;
            rc_size = file_size(&rc_compressed)?;
            Ok(elapsed)
        },
    )?;
    let name = format!("{} compress", container.label());
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared(name, ext, rc_stats).with_size(rc_size, ext_size),
        None => Row::rc_only(name, rc_stats).with_output(rc_size),
    });

    // Decompress against a shared compressed input. Produce it with nsz when
    // available, otherwise with rom-converto so the decompress can still run.
    let shared = dir.join(format!("for-decomp.{cext}"));
    remove_by_ext(dir, cext)?;
    if let Some(nsz) = nsz {
        run_timed(&mut nsz_compress(nsz, container, dir, input))?;
        std::fs::rename(find_one_by_ext(dir, cext)?, &shared)?;
    } else {
        run_timed(&mut rc_compress(ctx, container, keys, &shared, input))?;
    }

    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        has_ext,
        &mut || {
            remove_by_ext(dir, dext)?;
            let mut cmd = Command::new(nsz.expect("nsz"));
            cmd.args(["-D", "-o"]).arg(dir).arg(&shared);
            let elapsed = run_timed(&mut cmd)?;
            find_one_by_ext(dir, dext)?;
            Ok(elapsed)
        },
        &mut || {
            remove_if_exists(&rc_decompressed);
            let mut cmd = ctx.rc();
            cmd.args(["nx", "decompress"]);
            if let Some(k) = keys {
                cmd.arg("--keys").arg(k);
            }
            cmd.arg("-o").arg(&rc_decompressed).arg(&shared);
            run_timed(&mut cmd)
        },
    )?;
    let name = format!("{} decompress", container.label());
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared(name, ext, rc_stats),
        None => Row::rc_only(name, rc_stats),
    });

    // Verify is rom-converto only.
    let rc_stats = bench_op(&ctx.config, KILL, &mut || {
        let mut cmd = ctx.rc();
        cmd.args(["nx", "verify"]);
        if let Some(k) = keys {
            cmd.arg("--keys").arg(k);
        }
        cmd.arg(input);
        run_timed(&mut cmd)
    })?;
    table.rows.push(Row::rc_only(
        format!("{} verify", container.label()),
        rc_stats,
    ));

    if let Some(nsz) = nsz {
        cross_check(ctx, nsz, input, keys, container, &rc_compressed, dir)?;
    }
    Ok(())
}

/// Compress with rom-converto, decompress that output with nsz, and confirm
/// the recovered bytes match the source (the round-trip check Switch.md documents).
fn cross_check(
    ctx: &BenchCtx,
    nsz: &Path,
    input: &Path,
    keys: Option<&Path>,
    container: Container,
    rc_compressed: &Path,
    dir: &Path,
) -> Result<()> {
    remove_if_exists(rc_compressed);
    run_timed(&mut rc_compress(ctx, container, keys, rc_compressed, input))?;
    remove_by_ext(dir, container.decompressed_ext())?;
    let mut cmd = Command::new(nsz);
    cmd.args(["-D", "-o"]).arg(dir).arg(rc_compressed);
    run_timed(&mut cmd)?;
    let recovered = find_one_by_ext(dir, container.decompressed_ext())?;
    let ok = sha256_file(&recovered)? == sha256_file(input)?;
    println!(
        "cross-check {}: rom-converto compress -> nsz -D SHA-256 {}",
        container.label(),
        if ok {
            "OK (matches source)"
        } else {
            "MISMATCH"
        }
    );
    Ok(())
}

fn nsz_compress(nsz: &Path, container: Container, out_dir: &Path, input: &Path) -> Command {
    let mut cmd = Command::new(nsz);
    cmd.arg("-C");
    if container.block() {
        cmd.arg("-B");
    }
    cmd.args(["-l", "18", "-o"]).arg(out_dir).arg(input);
    cmd
}

fn rc_compress(
    ctx: &BenchCtx,
    container: Container,
    keys: Option<&Path>,
    output: &Path,
    input: &Path,
) -> Command {
    let mut cmd = ctx.rc();
    cmd.args(["nx", "compress"]);
    if let Some(k) = keys {
        cmd.arg("--keys").arg(k);
    }
    cmd.args(["--mode", container.rc_mode(), "-l", "18", "-o"])
        .arg(output)
        .arg(input);
    cmd
}
