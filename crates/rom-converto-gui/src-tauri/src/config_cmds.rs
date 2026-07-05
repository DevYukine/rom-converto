//! Tauri commands giving the GUI read/write access to the same
//! `rom-converto.toml` presets the CLI reads, so a GUI-authored profile is
//! reproducible from the CLI and vice versa.

use rom_converto_lib::config::{
    Preset, UserConfig, discover_config_path, load_config_raw, remove_preset, upsert_preset,
    user_config_write_path,
};
use std::path::PathBuf;

fn err_to_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The config file a save/delete would target: the discovered file if one
/// exists, otherwise the per-user config path (which may not exist yet).
fn write_path() -> Result<PathBuf, String> {
    if let Some(path) = discover_config_path(None).map_err(err_to_string)? {
        return Ok(path);
    }
    user_config_write_path().ok_or_else(|| "cannot determine a user config directory".to_string())
}

/// Path to the config file the GUI would read and write, or `None` when no
/// config exists yet and no user config directory could be determined.
#[tauri::command]
pub fn cmd_config_path() -> Result<Option<String>, String> {
    if let Some(path) = discover_config_path(None).map_err(err_to_string)? {
        return Ok(Some(path.display().to_string()));
    }
    Ok(user_config_write_path().map(|p| p.display().to_string()))
}

/// Loads the discovered config file (or built-in defaults when none
/// exists) so the frontend can list presets and their contents. Paths are
/// returned unresolved (not absolutized against the config directory) so a
/// preset's relative `output_dir`/`report` round-trips unchanged.
#[tauri::command]
pub fn cmd_load_config() -> Result<UserConfig, String> {
    load_config_raw(None).map_err(err_to_string)
}

/// Saves `preset` as `[presets.<name>]`, creating the config file (and its
/// parent directory) the first time a preset is saved.
#[tauri::command]
pub fn cmd_save_preset(name: String, preset: Preset) -> Result<(), String> {
    let path = write_path()?;
    upsert_preset(&path, &name, &preset).map_err(err_to_string)
}

/// Removes `[presets.<name>]` from the config file, if present.
#[tauri::command]
pub fn cmd_delete_preset(name: String) -> Result<(), String> {
    let path = write_path()?;
    remove_preset(&path, &name).map_err(err_to_string)
}
