//! Tauri backend for the rom-converto desktop GUI. Bridges the Nuxt frontend to
//! `rom-converto-lib`, exposing one command per CLI operation so the GUI and the
//! CLI produce identical results from the same library code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod info_cache;
mod progress;

use commands::*;
use info_cache::InfoCache;
use std::sync::Arc;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .manage(Arc::new(InfoCache::default()))
        .manage(ActiveCancel::default())
        .invoke_handler(tauri::generate_handler![
            cmd_cancel,
            cmd_cdn_to_cia,
            cmd_generate_ticket,
            cmd_decrypt_rom,
            cmd_compress_rom,
            cmd_decompress_rom,
            cmd_chd_compress,
            cmd_cso_compress,
            cmd_cso_to_chd,
            cmd_cso_decompress,
            cmd_cso_verify,
            cmd_chd_extract,
            cmd_chd_to_cso,
            cmd_chd_verify,
            cmd_cue_merge,
            cmd_verify_ctr,
            cmd_convert_ctr,
            cmd_verify_dol,
            cmd_verify_rvl,
            cmd_wup_verify,
            cmd_compress_disc,
            cmd_decompress_disc,
            cmd_wup_compress,
            cmd_wup_decrypt,
            cmd_nx_compress,
            cmd_nx_decompress,
            cmd_nx_verify,
            cmd_read_info,
            cmd_save_icon,
            cmd_hash,
            cmd_playlist,
            cmd_scan_dir,
            cmd_write_report,
            cmd_file_size,
            cmd_dat_verify,
            cmd_dat_scan,
            cmd_dat_rename,
            app_display_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
