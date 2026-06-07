pub mod chd;
pub mod ctr;
pub mod disc;
pub mod switch;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::RunConfig;

/// Shared state every platform benchmark needs: the rom-converto binary
/// under test, the folder counterpart tools may live in, and the run knobs.
pub struct BenchCtx {
    pub rom_converto: PathBuf,
    pub rom_converto_dir: PathBuf,
    pub config: RunConfig,
    pub keep_temp: bool,
    /// Skip the reference tool and time rom-converto alone. Used on hosts
    /// where the counterpart CLI is not available.
    pub rom_converto_only: bool,
}

impl BenchCtx {
    pub fn rc(&self) -> Command {
        Command::new(&self.rom_converto)
    }
}

/// A scratch directory for a platform's outputs. Deleted on drop unless
/// the benchmark was launched with `--keep-temp`, in which case the path
/// is preserved and printed.
pub struct Scratch {
    dir: Option<tempfile::TempDir>,
    path: PathBuf,
    keep: bool,
}

impl Scratch {
    pub fn new(ctx: &BenchCtx, prefix: &str) -> Result<Scratch> {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir()?;
        let path = dir.path().to_path_buf();
        Ok(Scratch {
            dir: Some(dir),
            path,
            keep: ctx.keep_temp,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.keep
            && let Some(dir) = self.dir.take()
        {
            let kept = dir.keep();
            println!("kept temp dir: {}", kept.display());
        }
    }
}

pub fn file_size(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len())
}

pub fn remove_if_exists(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Delete every file directly in `dir` whose extension equals `ext`
/// (case-insensitive). Used to clear a tool's previous output before a run.
pub fn remove_by_ext(dir: &Path, ext: &str) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if has_ext(&path, ext) {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Return the single file in `dir` with extension `ext`, erroring if there
/// is not exactly one (counterpart tools name their output after the input).
pub fn find_one_by_ext(dir: &Path, ext: &str) -> Result<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let path = entry?.path();
        if has_ext(&path, ext) {
            found.push(path);
        }
    }
    match found.len() {
        0 => bail!("no .{ext} file was produced in {}", dir.display()),
        1 => Ok(found.pop().expect("checked len == 1")),
        n => bail!("expected one .{ext} file in {}, found {n}", dir.display()),
    }
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("hash {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn require_input(path: &Path, label: &str) -> Result<()> {
    if !path.exists() {
        bail!("{label} input {} does not exist", path.display());
    }
    Ok(())
}
