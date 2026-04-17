#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod progress;

use commands::*;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            cmd_cdn_to_cia,
            cmd_generate_ticket,
            cmd_decrypt_rom,
            cmd_compress_rom,
            cmd_decompress_rom,
            cmd_chd_compress,
            cmd_chd_extract,
            cmd_chd_verify,
            cmd_verify_ctr,
            cmd_compress_disc,
            cmd_decompress_disc,
            cmd_wup_compress,
            cmd_wup_decrypt,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
