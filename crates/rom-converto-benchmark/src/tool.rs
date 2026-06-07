use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

pub fn exe_file_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn candidate_names(base: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{base}.exe"), base.to_string()]
    } else {
        vec![base.to_string()]
    }
}

fn find_in_path(base: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in candidate_names(base) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_in_dir(dir: &Path, base: &str) -> Option<PathBuf> {
    for name in candidate_names(base) {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve the rom-converto binary the benchmark drives. Order: explicit
/// override, `ROMCONVERTO_BIN`, the directory holding this benchmark
/// binary (same `target/<profile>`), PATH, then `./target/{release,debug}`.
pub fn resolve_rom_converto(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(anyhow!("--rom-converto-bin {} does not exist", p.display()));
    }
    if let Some(env) = std::env::var_os("ROMCONVERTO_BIN") {
        let p = PathBuf::from(env);
        if p.is_file() {
            return Ok(p);
        }
        return Err(anyhow!("ROMCONVERTO_BIN {} does not exist", p.display()));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        && let Some(p) = find_in_dir(&dir, "rom-converto")
    {
        return Ok(p);
    }
    if let Some(p) = find_in_path("rom-converto") {
        return Ok(p);
    }
    for profile in ["release", "debug"] {
        let p = PathBuf::from("target")
            .join(profile)
            .join(exe_file_name("rom-converto"));
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "rom-converto binary not found. Build it with `cargo build --release -p rom-converto-cli`, \
         pass --rom-converto-bin <path>, or set ROMCONVERTO_BIN."
    ))
}

/// Find an external reference tool by name, searching PATH first, then the
/// rom-converto folder (the directory of the resolved rom-converto
/// binary). Missing tools error with install and placement guidance.
pub fn find_tool(base: &str, rom_converto_dir: &Path) -> Result<PathBuf> {
    if let Some(p) = find_in_path(base) {
        return Ok(p);
    }
    if let Some(p) = find_in_dir(rom_converto_dir, base) {
        return Ok(p);
    }
    Err(anyhow!(
        "required tool `{base}` was not found.\n  \
         Install it, then either make sure `{base}` is on your PATH, or place the \
         executable in the rom-converto folder ({}) so the benchmark can find it.",
        rom_converto_dir.display()
    ))
}
