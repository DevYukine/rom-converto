use crate::info_cache::InfoCache;
use crate::progress::TauriProgress;
use rom_converto_lib::chd::{convert_to_chd, extract_from_chd, verify_chd};
use rom_converto_lib::info::{InfoOptions, InfoResult, read_info};
use rom_converto_lib::nintendo::ctr::convert::{convert_rom, derive_converted_path};
use rom_converto_lib::nintendo::ctr::verify::{CtrVerifyOptions, verify_ctr};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom, decompress_rom, derive_compressed_path, derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia, decrypt_rom, generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::nx::{
    NczMode, NxCompressOptions, compress_container_async, decompress_container_async,
    derive_compressed_path as nx_derive_compressed_path,
    derive_decompressed_path as nx_derive_decompressed_path, detect_container, load_keyset,
    verify_container_async,
};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc, decompress_disc, derive_disc_path, derive_rvz_path,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async, decrypt_nus_title_async,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, State};

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
    std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(extract_from_chd(progress.as_ref(), input, output, parent))
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
    std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(verify_chd(progress.as_ref(), input, parent, fix))
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
pub async fn cmd_wup_compress(
    app: AppHandle,
    inputs: Vec<PathBuf>,
    output: PathBuf,
    level: Option<i32>,
    keys: Option<Vec<PathBuf>>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-compress"));
    let out_display = output.display().to_string();
    let opts = WupCompressOptions {
        zstd_level: level.unwrap_or(WupCompressOptions::default().zstd_level),
    };
    // Pair each supplied key with the next disc input in positional
    // order. Non-disc inputs do not consume a key slot.
    let mut key_iter = keys.unwrap_or_default().into_iter();
    let titles: Vec<TitleInput> = inputs
        .into_iter()
        .map(|p| {
            let is_disc = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wud") || s.eq_ignore_ascii_case("wux"))
                .unwrap_or(false)
                && p.is_file();
            let mut t = TitleInput::auto(p);
            if is_disc {
                t.key_path = key_iter.next();
            }
            t
        })
        .collect();
    tokio::spawn(
        async move { compress_titles_async(titles, output, opts, progress.as_ref()).await },
    )
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_wup_decrypt(
    app: AppHandle,
    input: PathBuf,
    output: PathBuf,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-decrypt"));
    let out_display = output.display().to_string();
    tokio::spawn(async move { decrypt_nus_title_async(input, output, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Decrypted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_nx_compress(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
    level: Option<i32>,
    mode: Option<String>,
    block_size_exp: Option<u8>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-compress"));
    let kind = detect_container(&input).map_err(err_to_string)?;
    let mut opts = NxCompressOptions::for_kind(kind);
    if let Some(level) = level {
        opts.level = level;
    }
    if let Some(mode) = mode.as_deref() {
        opts.mode = match mode {
            "solid" => NczMode::Solid,
            "block" => NczMode::Block {
                size_exp: block_size_exp.unwrap_or(20),
            },
            other => return Err(format!("unknown mode {other:?}")),
        };
    } else if let Some(exp) = block_size_exp {
        opts.mode = NczMode::Block { size_exp: exp };
    }
    let output = output.unwrap_or_else(|| nx_derive_compressed_path(&input));
    let out_display = output.display().to_string();
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    tokio::spawn(async move {
        compress_container_async(input, output, opts, keys, progress.as_ref()).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_nx_decompress(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-decompress"));
    let output = output.unwrap_or_else(|| nx_derive_decompressed_path(&input));
    let out_display = output.display().to_string();
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    tokio::spawn(async move {
        decompress_container_async(input, output, keys, progress.as_ref()).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    Ok(format!("Decompressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_nx_verify(
    app: AppHandle,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-verify"));
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    let result =
        tokio::spawn(async move { verify_container_async(input, keys, progress.as_ref()).await })
            .await
            .map_err(err_to_string)?
            .map_err(err_to_string)?;
    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_convert_ctr(
    app: AppHandle,
    input: PathBuf,
    output: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "ctr-convert"));
    let output = output.unwrap_or_else(|| derive_converted_path(&input));
    let out_display = output.display().to_string();
    tokio::spawn(async move { convert_rom(&input, &output, progress.as_ref()).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Converted to {out_display}"))
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

#[tauri::command]
pub async fn cmd_read_info(
    cache: State<'_, Arc<InfoCache>>,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let cache_inner = cache.inner().clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Arc<InfoResult>, anyhow::Error> {
        if let Some(key) = InfoCache::key_for(&input)
            && let Some(hit) = cache_inner.get(&key)
        {
            return Ok(hit);
        }
        let opts = InfoOptions {
            keys_path: keys.clone(),
            parent_path: None,
        };
        let info = read_info(&input, &opts)?;
        let arc = Arc::new(info);
        if let Some(key) = InfoCache::key_for(&input) {
            cache_inner.insert(key, arc.clone());
        }
        Ok(arc)
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;
    serde_json::to_string(result.as_ref()).map_err(err_to_string)
}

/// The frontend posts back the InfoResult JSON it already holds, so the
/// Rust side does not need to redo the extraction.
#[tauri::command]
pub async fn cmd_save_icon(info_json: String, dest: PathBuf) -> Result<String, String> {
    let info: InfoResult = serde_json::from_str(&info_json).map_err(err_to_string)?;
    let bytes =
        extract_icon_png(&info).ok_or_else(|| "no icon present in info payload".to_string())?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(err_to_string)?;
    }
    std::fs::write(&dest, &bytes).map_err(err_to_string)?;
    Ok(dest.display().to_string())
}

fn extract_icon_png(info: &InfoResult) -> Option<Vec<u8>> {
    match info {
        InfoResult::Ctr(c) => c.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Dol(d) => d.banner_image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Rvl(r) => r.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Wup(_) => None,
        InfoResult::Nx(n) => n
            .full
            .as_ref()
            .and_then(|f| f.control.as_ref())
            .and_then(|c| c.icon.as_ref())
            .map(|i| i.png_bytes.clone()),
        InfoResult::Chd(_) => None,
    }
}
