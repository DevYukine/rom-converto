use crate::progress::TauriProgress;
use rom_converto_lib::chd::{convert_to_chd, extract_from_chd, verify_chd};
use rom_converto_lib::nintendo::ctr::verify::{CtrVerifyOptions, verify_ctr};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom, decompress_rom, derive_compressed_path, derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia, decrypt_rom, generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc, decompress_disc, derive_disc_path, derive_rvz_path,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::AppHandle;

fn err_to_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_cdn_to_cia(
    app: AppHandle,
    cdn_dir: PathBuf,
    output: Option<PathBuf>,
    decrypt: bool,
    compress: bool,
    cleanup: bool,
    recursive: bool,
    ensure_ticket_exists: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app.clone(), "cdn-to-cia"));
    let total_progress = Arc::new(TauriProgress::new(app, "cdn-to-cia-total"));
    let opts = CdnToCiaOptions {
        cdn_dir,
        output,
        cleanup,
        recursive,
        ensure_ticket_exists,
        decrypt,
        compress,
    };
    tokio::spawn(async move {
        convert_cdn_to_cia(opts, progress.as_ref(), total_progress.as_ref()).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok("CDN to CIA conversion complete".to_string())
}

#[tauri::command]
pub async fn cmd_generate_ticket(cdn_dir: PathBuf, output: PathBuf) -> Result<String, String> {
    let out_display = output.display().to_string();
    tokio::spawn(async move { generate_ticket_from_cdn(&cdn_dir, &output).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Ticket generated at {out_display}"))
}

#[tauri::command]
pub async fn cmd_decrypt_rom(
    app: AppHandle,
    input: PathBuf,
    output: PathBuf,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decrypt"));
    let out_display = output.display().to_string();
    tokio::spawn(async move { decrypt_rom(&input, &output, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Decrypted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_compress_rom(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "compress"));
    let output = output.unwrap_or_else(|| derive_compressed_path(&input));
    let out_display = output.display().to_string();
    tokio::spawn(async move { compress_rom(&input, &output, level, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_decompress_rom(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decompress"));
    let output = output.unwrap_or_else(|| derive_decompressed_path(&input));
    let out_display = output.display().to_string();
    tokio::spawn(async move { decompress_rom(&input, &output, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Decompressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_chd_compress(
    app: AppHandle,
    cue_path: PathBuf,
    output: PathBuf,
    force: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-compress"));
    let out_display = output.display().to_string();
    tokio::spawn(async move { convert_to_chd(progress.as_ref(), cue_path, output, force).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("CHD created at {out_display}"))
}

// CHD extract and verify use deeply nested async types from ChdReader
// that exceed the compiler's recursion limit for Send inference. We run
// these on a dedicated thread with its own tokio runtime to sidestep the issue.

#[tauri::command]
pub async fn cmd_chd_extract(
    app: AppHandle,
    input: PathBuf,
    output: PathBuf,
    parent: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-extract"));
    let out_display = output.display().to_string();
    // ChdReader's deeply nested async types exceed the compiler's Send recursion
    // limit, so we run on a dedicated thread with its own tokio runtime.
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(extract_from_chd(progress.as_ref(), input, output, parent))
            .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string())??;
    Ok(format!("Extracted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_chd_verify(
    app: AppHandle,
    input: PathBuf,
    parent: Option<PathBuf>,
    fix: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-verify"));
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(verify_chd(progress.as_ref(), input, parent, fix))
            .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string())??;
    Ok("CHD verification passed".to_string())
}

#[tauri::command]
pub async fn cmd_compress_disc(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    chunk_size: Option<u32>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "compress-disc"));
    let output = output.unwrap_or_else(|| derive_rvz_path(&input));
    let out_display = output.display().to_string();
    let opts = RvzCompressOptions {
        compression_level: level.unwrap_or(RvzCompressOptions::default().compression_level),
        chunk_size: chunk_size.unwrap_or(RvzCompressOptions::default().chunk_size),
        ..RvzCompressOptions::default()
    };
    tokio::spawn(async move { compress_disc(&input, &output, opts, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_decompress_disc(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decompress-disc"));
    let output = output.unwrap_or_else(|| derive_disc_path(&input));
    let out_display = output.display().to_string();
    tokio::spawn(async move { decompress_disc(&input, &output, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Decompressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_verify_ctr(
    app: AppHandle,
    input: PathBuf,
    verify_content: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "ctr-verify"));
    let opts = CtrVerifyOptions {
        verify_content_hashes: verify_content,
    };
    let result = tokio::spawn(async move { verify_ctr(&input, &opts, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}
