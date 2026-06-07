use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bench::{BenchCtx, Scratch, file_size, remove_if_exists, require_input, sha256_file};
use crate::report::{Row, Table};
use crate::runner::{bench_op, run_sided, run_timed};
use crate::tool::find_tool;

const KILL: &[&str] = &["z3ds_compressor", "rom-converto"];

pub fn run(ctx: &BenchCtx, three_ds: Option<PathBuf>, cia: Option<PathBuf>) -> Result<()> {
    let z3ds = if ctx.rom_converto_only {
        None
    } else {
        Some(find_tool("z3ds_compressor", &ctx.rom_converto_dir)?)
    };
    let mut table = Table::new("3DS (Z3DS vs z3ds_compressor)", "external");
    table.rom_converto_only = ctx.rom_converto_only;

    if let Some(input) = three_ds {
        require_input(&input, "3DS .3ds")?;
        bench_input(ctx, z3ds.as_deref(), &input, ".3ds", &mut table)?;
    }
    if let Some(input) = cia {
        require_input(&input, "3DS .cia")?;
        bench_input(ctx, z3ds.as_deref(), &input, ".cia", &mut table)?;
    }

    if table.rows.is_empty() {
        bail!(
            "no 3DS inputs configured. Pass --three-ds and/or --cia \
             (or set ROMCONVERTO_BENCH_CTR_3DS / ROMCONVERTO_BENCH_CTR_CIA). \
             Inputs must already be decrypted with `rom-converto ctr decrypt`."
        );
    }
    table.print();
    Ok(())
}

fn bench_input(
    ctx: &BenchCtx,
    z3ds: Option<&Path>,
    input: &Path,
    op_label: &str,
    table: &mut Table,
) -> Result<()> {
    let scratch = Scratch::new(ctx, "bench-ctr-")?;
    let dir = scratch.path();

    let compressed_ext = compressed_ext(input);
    let orig_ext = input
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_string();

    let ext_out = dir.join(format!("external.{compressed_ext}"));
    let rc_out = dir.join(format!("romconverto.{compressed_ext}"));

    // Compress: z3ds_compressor (compress only)  vs  rom-converto ctr compress.
    let mut ext_size = 0u64;
    let mut rc_size = 0u64;
    let (ext_stats, rc_stats) = run_sided(
        &ctx.config,
        KILL,
        z3ds.is_some(),
        &mut || {
            remove_if_exists(&ext_out);
            let mut cmd = Command::new(z3ds.expect("z3ds_compressor"));
            cmd.arg(input).arg(&ext_out);
            let elapsed = run_timed(&mut cmd)?;
            ext_size = file_size(&ext_out)?;
            Ok(elapsed)
        },
        &mut || {
            remove_if_exists(&rc_out);
            let mut cmd = ctx.rc();
            cmd.args(["ctr", "compress"]).arg(input).arg(&rc_out);
            let elapsed = run_timed(&mut cmd)?;
            rc_size = file_size(&rc_out)?;
            Ok(elapsed)
        },
    )?;
    let name = format!("{op_label} compress");
    table.rows.push(match ext_stats {
        Some(ext) => Row::compared(name, ext, rc_stats).with_size(rc_size, ext_size),
        None => Row::rc_only(name, rc_stats).with_output(rc_size),
    });

    // Decompress is rom-converto only (z3ds_compressor is compress-only).
    let rc_decompressed = dir.join(format!("romconverto-dec.{orig_ext}"));
    let rc_stats = bench_op(&ctx.config, KILL, &mut || {
        remove_if_exists(&rc_decompressed);
        let mut cmd = ctx.rc();
        cmd.args(["ctr", "decompress"])
            .arg(&rc_out)
            .arg(&rc_decompressed);
        run_timed(&mut cmd)
    })?;
    table
        .rows
        .push(Row::rc_only(format!("{op_label} decompress"), rc_stats));

    if let Some(z3ds) = z3ds {
        cross_check(ctx, z3ds, input, &ext_out, &orig_ext, op_label, dir)?;
    }
    Ok(())
}

/// Compress with the external tool, decompress its output with
/// rom-converto, and confirm the bytes match the source, the
/// format-compatibility check 3DS.md documents.
fn cross_check(
    ctx: &BenchCtx,
    z3ds: &Path,
    input: &Path,
    ext_out: &Path,
    orig_ext: &str,
    op_label: &str,
    dir: &Path,
) -> Result<()> {
    remove_if_exists(ext_out);
    let mut cmd = Command::new(z3ds);
    cmd.arg(input).arg(ext_out);
    run_timed(&mut cmd)?;

    let recovered = dir.join(format!("crosscheck.{orig_ext}"));
    remove_if_exists(&recovered);
    let mut cmd = ctx.rc();
    cmd.args(["ctr", "decompress"]).arg(ext_out).arg(&recovered);
    run_timed(&mut cmd)?;

    let ok = sha256_file(&recovered)? == sha256_file(input)?;
    println!(
        "cross-check {op_label}: z3ds_compressor -> rom-converto ctr decompress SHA-256 {}",
        if ok {
            "OK (matches source)"
        } else {
            "MISMATCH"
        }
    );
    Ok(())
}

fn compressed_ext(input: &Path) -> String {
    match input
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("cia") => "zcia",
        Some("cci") | Some("3ds") => "zcci",
        Some("cxi") => "zcxi",
        Some("3dsx") => "z3dsx",
        _ => "z3ds",
    }
    .to_string()
}
