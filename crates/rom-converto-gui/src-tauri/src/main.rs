//! Tauri backend for the rom-converto desktop GUI. Bridges the Nuxt frontend to
//! `rom-converto-lib`, exposing one command per CLI operation so the GUI and the
//! CLI produce identical results from the same library code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config_cmds;
mod info_cache;
mod progress;
#[cfg(all(test, feature = "ts-export"))]
mod ts_export;

use commands::*;
use config_cmds::*;
use info_cache::InfoCache;
use rom_converto_lib::util::HashCache;
use std::sync::Arc;

/// Every command returns errors to the frontend as plain strings.
pub(crate) fn err_to_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Arc::new(InfoCache::default()))
        // Same on-disk store the CLI uses, so hashes computed by either
        // frontend are reused by both.
        .manage(Arc::new(HashCache::load(false, false)))
        .manage(ActiveCancel::default())
        .invoke_handler(tauri::generate_handler![
            cmd_cancel,
            cmd_run,
            cmd_nx_keys_resolve,
            cmd_read_info,
            cmd_save_icon,
            cmd_scan_dir,
            cmd_write_report,
            cmd_file_size,
            cmd_config_path,
            cmd_load_config,
            cmd_save_preset,
            cmd_delete_preset,
            app_display_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
